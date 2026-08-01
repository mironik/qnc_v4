//! A/V sync contracts for broadcast preview play.
//!
//! These helpers exist so regressions are proven by tests — not by guessing at
//! rodio/ffmpeg behaviour. The engine/player must honour the same rules.
//!
//! Observed failure ("sve zapinje" + nerazuman ton):
//! 1. Wall clock free-runs past decode → video HoldPrevious / jump, audio skips.
//! 2. Stall then resume after **one** new frame → immediate re-stall every frame
//!    (freeze/unfreeze) — hitching on both picture and sound.
//! 3. Playing decode budget of 1 cannot refill the buffer while stalled.

use super::audio::AudioFrameQueue;
use super::timebase::FrameNumber;

/// Minimum decoded frames ahead of the stall point before the clock may resume.
/// Resume-on-+1 causes stall thrashing (video+audio hitch every frame).
pub const STALL_RESUME_BUFFER_FRAMES: i64 = 4;

/// Play must not start the wall clock until this many carrier frames (inclusive
/// from start) are buffered in A/V lockstep. Matching lookahead (~8–12) avoids
/// an immediate underrun stall while continuous ffmpeg pipes warm up.
pub const PLAY_START_MIN_BUFFER_FRAMES: i64 = 12;

/// Sustained ticks at carrier end (or decode-exhausted stall) before soft EOS.
pub const SOFT_EOS_TICKS: u32 = 3;

/// Consecutive decode failures while Playing before we stop or soft-EOS.
pub const MAX_DECODE_RECOVER_STREAK: u32 = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecodeErrorAction {
    /// Clear pipes, stall, keep Playing — try again next tick.
    Recover,
    /// Near carrier end — finish via soft EOS instead of Error.
    SoftEos,
    /// Too many failures mid-clip — surface Error and stop.
    Fatal,
}

/// Policy for live decode Err (ffmpeg desync/EOF mid-play).
pub fn decode_error_action(
    fail_streak_after: u32,
    near_carrier_end: bool,
    max_streak: u32,
) -> DecodeErrorAction {
    let max_streak = max_streak.max(1);
    if fail_streak_after < max_streak {
        return DecodeErrorAction::Recover;
    }
    if near_carrier_end {
        DecodeErrorAction::SoftEos
    } else {
        DecodeErrorAction::Fatal
    }
}

/// Error when play would emit non-contiguous or future audio.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AvSyncError {
    pub message: String,
}

impl AvSyncError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

/// Contiguous carrier frames that must reach the audio sink before `master`
/// may advance further.
pub fn audio_emit_range(
    last_emitted: Option<FrameNumber>,
    master: FrameNumber,
    newest_ready: Option<FrameNumber>,
) -> Result<Vec<FrameNumber>, AvSyncError> {
    let Some(newest) = newest_ready else {
        return Ok(Vec::new());
    };
    let end = master.0.min(newest.0);
    let start = match last_emitted {
        Some(prev) => prev.0 + 1,
        None => end,
    };
    if start > end {
        return Ok(Vec::new());
    }
    if end - start > 64 {
        return Err(AvSyncError::new(format!(
            "audio emit backlog too large: {start}..={end} (clock jumped past decode)"
        )));
    }
    Ok((start..=end).map(FrameNumber).collect())
}

/// Decode frontier used for stall — source-aware min of ready queues.
///
/// Video+audio sources stall on the slower of the two so the master never
/// free-runs past missing PCM (exact-match silence / crackle).
pub fn decode_frontier_for_stall(
    expects_video: bool,
    expects_audio: bool,
    newest_video: Option<FrameNumber>,
    newest_audio: Option<FrameNumber>,
) -> Option<FrameNumber> {
    match (expects_video, expects_audio) {
        (true, true) => match (newest_video, newest_audio) {
            (Some(v), Some(a)) => Some(FrameNumber(v.0.min(a.0))),
            (Some(v), None) => Some(v),
            (None, Some(a)) => Some(a),
            (None, None) => None,
        },
        (true, false) => newest_video,
        (false, true) => newest_audio,
        (false, false) => newest_video.or(newest_audio),
    }
}

/// Newest frame already buffered for sequential refill (`newest + 1` next).
///
/// Must match the stall frontier — if decode advances only the video queue while
/// audio lags, the clock stalls forever at start / underrun ("zapne").
pub fn decode_newest_for_refill(
    expects_video: bool,
    expects_audio: bool,
    newest_video: Option<FrameNumber>,
    newest_audio: Option<FrameNumber>,
) -> Option<FrameNumber> {
    decode_frontier_for_stall(expects_video, expects_audio, newest_video, newest_audio)
}

/// True when video/audio queues that the source expects stay on the same newest.
pub fn av_queues_in_lockstep(
    expects_video: bool,
    expects_audio: bool,
    newest_video: Option<FrameNumber>,
    newest_audio: Option<FrameNumber>,
) -> bool {
    match (expects_video, expects_audio) {
        (true, true) => newest_video == newest_audio,
        _ => true,
    }
}

/// After preroll, play may start once the lagging queue has this many frames
/// at/after `start` (inclusive). Matches stall resume cushion so the first
/// ticks do not immediately re-stall.
pub fn play_start_ready(
    start: FrameNumber,
    expects_video: bool,
    expects_audio: bool,
    newest_video: Option<FrameNumber>,
    newest_audio: Option<FrameNumber>,
    min_buffer: i64,
) -> bool {
    let min_buffer = min_buffer.max(1);
    let Some(frontier) =
        decode_frontier_for_stall(expects_video, expects_audio, newest_video, newest_audio)
    else {
        return false;
    };
    if !av_queues_in_lockstep(expects_video, expects_audio, newest_video, newest_audio) {
        return false;
    }
    frontier.0 >= start.0 + min_buffer - 1
}

/// True when decode cannot advance past `newest` toward `last_frame`.
///
/// Without this, EOF near the carrier end stalls forever waiting for a resume
/// cushion that will never arrive ("zapne na kraju").
pub fn carrier_decode_exhausted(
    newest: Option<FrameNumber>,
    last_frame: FrameNumber,
    decode_budget_this_tick: usize,
    decoded_this_tick: usize,
) -> bool {
    match newest {
        Some(n) if n.0 >= last_frame.0 => true,
        Some(_) | None => decode_budget_this_tick > 0 && decoded_this_tick == 0,
    }
}

/// Enter underrun stall when wall > decode frontier — unless media is exhausted
/// at/near EOS (then freeze presentation via soft-EOS, do not wait for cushion).
pub fn should_enter_underrun_stall(
    master: FrameNumber,
    newest: FrameNumber,
    decode_exhausted: bool,
) -> bool {
    master.0 > newest.0 && !decode_exhausted
}

/// Soft EOS: clamped on last carrier frame, or stalled because decode hit EOF.
pub fn soft_eos_tick_progress(
    master: FrameNumber,
    last_frame: FrameNumber,
    stalled: bool,
    decode_exhausted: bool,
) -> bool {
    master.0 >= last_frame.0 || (stalled && decode_exhausted)
}

/// Engine lifecycle simulator — slow start decode + EOF at end.
///
/// This is what unit tests must exercise: thin preroll + slow decode ⇒ start
/// hitch; EOF before `last` ⇒ permanent stall without soft-EOS policy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LifecycleTrace {
    pub stalls: u32,
    pub resumes: u32,
    pub freeze_ticks: usize,
    pub soft_eos_at_tick: Option<usize>,
    pub final_stalled: bool,
    pub completed_clean: bool,
    pub audio_emitted: Vec<i64>,
    pub video_presented: Vec<i64>,
}

pub fn simulate_engine_lifecycle(
    start: i64,
    last: i64,
    // Frames already buffered before play_from (preroll depth).
    preroll_frames: i64,
    ticks: usize,
    wall_advance_per_tick: i64,
    // Decode capacity while pipes are cold (first `cold_ticks`).
    cold_decode_per_tick: usize,
    cold_ticks: usize,
    // Decode capacity after warm-up.
    warm_decode_per_tick: usize,
    // Once newest reaches this, further decode produces 0 (EOF).
    eof_after_newest: i64,
    healthy: usize,
    resume_min_buffer: i64,
    // When true, apply soft-EOS / no infinite stall-on-EOF policy.
    use_soft_eos_policy: bool,
) -> LifecycleTrace {
    let mut newest = start + preroll_frames.max(0) - 1;
    if preroll_frames <= 0 {
        newest = start - 1;
    }
    let mut master = start;
    let mut stalled = false;
    let mut last_audio: Option<i64> = None;
    let mut stalls = 0_u32;
    let mut resumes = 0_u32;
    let mut video = Vec::new();
    let mut audio = Vec::new();
    let mut eos_ticks = 0_u32;
    let mut soft_eos_at = None;
    let mut completed_clean = false;

    for tick in 0..ticks {
        let decode_cap = if tick < cold_ticks {
            cold_decode_per_tick
        } else {
            warm_decode_per_tick
        };

        let ahead = if newest >= master {
            (newest - master) as usize
        } else {
            0
        };
        let budget = decode_budget(true, stalled, ahead, healthy, 4);
        let want = budget.min(decode_cap);
        let decoded = if newest >= eof_after_newest.min(last) {
            0
        } else {
            want.min((eof_after_newest.min(last) - newest).max(0) as usize)
        };
        let exhausted = carrier_decode_exhausted(
            if newest >= start {
                Some(FrameNumber(newest))
            } else {
                None
            },
            FrameNumber(last),
            budget,
            decoded,
        ) || (budget > 0 && decoded == 0 && newest < last);

        // Stall decision (before present).
        if !stalled && newest >= start && master > newest {
            stalled = true;
            stalls += 1;
            master = newest;
        }

        video.push(master);
        let emit = audio_emit_range(
            last_audio.map(FrameNumber),
            FrameNumber(master),
            if newest >= start {
                Some(FrameNumber(newest))
            } else {
                None
            },
        )
        .unwrap_or_default();
        if let Some(f) = emit.last() {
            last_audio = Some(f.0);
        }
        for f in emit {
            audio.push(f.0);
        }

        newest += decoded as i64;
        if newest > last {
            newest = last;
        }

        if stalled {
            let can_resume = should_resume_after_stall(
                FrameNumber(master),
                FrameNumber(newest),
                resume_min_buffer,
            );
            let at_eof = newest >= eof_after_newest.min(last);
            if use_soft_eos_policy {
                if can_resume && !at_eof {
                    stalled = false;
                    resumes += 1;
                }
            } else if can_resume {
                stalled = false;
                resumes += 1;
            }
            let _ = exhausted;
        }

        if !stalled {
            master = (master + wall_advance_per_tick.max(0)).min(last);
        }

        let progress = if use_soft_eos_policy {
            soft_eos_tick_progress(
                FrameNumber(master),
                FrameNumber(last),
                stalled,
                newest >= eof_after_newest.min(last) || newest >= last,
            )
        } else {
            master >= last
        };
        if progress {
            eos_ticks += 1;
        } else {
            eos_ticks = 0;
        }
        if eos_ticks >= SOFT_EOS_TICKS {
            soft_eos_at = Some(tick);
            completed_clean = true;
            break;
        }
    }

    let freeze = video.windows(2).filter(|w| w[0] == w[1]).count();
    LifecycleTrace {
        stalls,
        resumes,
        freeze_ticks: freeze,
        soft_eos_at_tick: soft_eos_at,
        final_stalled: stalled && !completed_clean,
        completed_clean,
        audio_emitted: audio,
        video_presented: video,
    }
}

/// Simulate hold-policy audio presents (queue returns latest ≤ master).
pub fn simulate_hold_policy_emits(decoded_in_order: &[i64], master_per_tick: &[i64]) -> Vec<i64> {
    let mut queue = AudioFrameQueue::new(64);
    for &f in decoded_in_order {
        queue.push_decoded(FrameNumber(f), f);
    }
    let mut last: Option<i64> = None;
    let mut emitted = Vec::new();
    for &master in master_per_tick {
        if let Some(got) = queue.frame_for_program_clock(FrameNumber(master)) {
            if last != Some(got.payload) {
                emitted.push(got.payload);
                last = Some(got.payload);
            }
        }
    }
    emitted
}

/// Contiguous audio emit simulation (no skips).
pub fn simulate_contiguous_emits(
    decoded_newest_per_tick: &[i64],
    master_per_tick: &[i64],
) -> Result<Vec<i64>, AvSyncError> {
    assert_eq!(decoded_newest_per_tick.len(), master_per_tick.len());
    let mut last: Option<FrameNumber> = None;
    let mut emitted = Vec::new();
    for (&newest, &master) in decoded_newest_per_tick.iter().zip(master_per_tick) {
        let batch = audio_emit_range(last, FrameNumber(master), Some(FrameNumber(newest)))?;
        for frame in batch {
            emitted.push(frame.0);
            last = Some(frame);
        }
    }
    Ok(emitted)
}

/// Wall-clock free-run vs decode frontier.
pub fn clock_ahead_of_decode(master: FrameNumber, newest_ready: Option<FrameNumber>) -> bool {
    match newest_ready {
        Some(newest) => master.0 > newest.0,
        None => false,
    }
}

/// After a stall, resume only once the decode queue has a healthy cushion.
pub fn should_resume_after_stall(
    held_frame: FrameNumber,
    newest_ready: FrameNumber,
    min_buffer: i64,
) -> bool {
    let min_buffer = min_buffer.max(1);
    newest_ready.0 >= held_frame.0 + min_buffer
}

/// How many sequential frames to decode this tick.
///
/// While Playing with a healthy buffer → 0 (present only).
/// While low / stalled → refill up to `healthy`, capped at `max_burst`.
pub fn decode_budget(
    playing: bool,
    stalled: bool,
    ahead_of_master: usize,
    healthy: usize,
    max_burst: usize,
) -> usize {
    let healthy = healthy.max(1);
    let max_burst = max_burst.max(1);
    if !playing {
        return max_burst.min(4);
    }
    if ahead_of_master >= healthy && !stalled {
        return 0;
    }
    let need = healthy.saturating_sub(ahead_of_master).max(1);
    need.min(max_burst)
}

/// Simulate stall/resume decisions across ticks.
///
/// `decode_per_tick[i]` = how many new frames become ready that tick.
/// Returns how many times we transitioned stalled→running (resumes).
pub fn simulate_stall_resume_cycles(
    start_newest: i64,
    decode_per_tick: &[usize],
    resume_min_buffer: i64,
) -> StallResumeTrace {
    let mut newest = start_newest;
    let mut held = start_newest;
    let mut stalled = true;
    let mut resumes = 0_u32;
    let mut stalls = 0_u32;
    let mut presented = Vec::new();

    for &decoded in decode_per_tick {
        newest += decoded as i64;
        if stalled {
            presented.push(held);
            if should_resume_after_stall(FrameNumber(held), FrameNumber(newest), resume_min_buffer)
            {
                stalled = false;
                resumes += 1;
                // Resume from `held` — do not snap held to newest (that hides thrash).
            }
        } else {
            // Wall advances one frame per tick; decode may lag.
            let next = held + 1;
            if next > newest {
                stalled = true;
                stalls += 1;
                held = newest;
                presented.push(held);
            } else {
                held = next;
                presented.push(held);
            }
        }
    }

    StallResumeTrace {
        resumes,
        stalls,
        presented,
        final_stalled: stalled,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StallResumeTrace {
    pub resumes: u32,
    pub stalls: u32,
    pub presented: Vec<i64>,
    pub final_stalled: bool,
}

/// Full preview play-loop model used by regression tests.
///
/// Mirrors engine policy: stall when wall > newest, refill with
/// [`decode_budget`], resume only with cushion, present video+audio in lockstep.
pub fn simulate_coupled_play_loop(
    start_frame: i64,
    ticks: usize,
    // Wall frames advanced per tick while running (1 = realtime).
    wall_advance_per_tick: i64,
    // Max frames produced this tick if asked (simulates slow decode).
    decode_capacity_per_tick: usize,
    healthy: usize,
    resume_min_buffer: i64,
) -> CoupledPlayTrace {
    let mut newest = start_frame;
    let mut master = start_frame;
    let mut stalled = false;
    let mut last_audio: Option<i64> = None;
    let mut video = Vec::with_capacity(ticks);
    let mut audio = Vec::with_capacity(ticks);
    let mut resumes = 0_u32;
    let mut stalls = 0_u32;
    let mut decode_total = 0_usize;

    for _ in 0..ticks {
        // 1) Stall if wall ran ahead (before present).
        if !stalled && master > newest {
            stalled = true;
            stalls += 1;
            master = newest;
        }

        // 2) Present under current master (frozen while stalled).
        video.push(master);
        let emit = audio_emit_range(
            last_audio.map(FrameNumber),
            FrameNumber(master),
            Some(FrameNumber(newest)),
        )
        .unwrap_or_default();
        if let Some(last) = emit.last() {
            last_audio = Some(last.0);
        }
        for f in emit {
            audio.push(f.0);
        }

        // 3) Decode refill.
        let ahead = (newest - master).max(0) as usize;
        let budget = decode_budget(true, stalled, ahead, healthy, 4);
        let decoded = budget.min(decode_capacity_per_tick);
        newest += decoded as i64;
        decode_total += decoded;

        // 4) Resume when cushion ready.
        if stalled
            && should_resume_after_stall(
                FrameNumber(master),
                FrameNumber(newest),
                resume_min_buffer,
            )
        {
            stalled = false;
            resumes += 1;
        }

        // 5) Wall advance only while running.
        if !stalled {
            master += wall_advance_per_tick.max(0);
        }
    }

    CoupledPlayTrace {
        video_presented: video,
        audio_emitted: audio,
        resumes,
        stalls,
        decode_total,
        final_stalled: stalled,
        final_master: master,
        final_newest: newest,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoupledPlayTrace {
    pub video_presented: Vec<i64>,
    pub audio_emitted: Vec<i64>,
    pub resumes: u32,
    pub stalls: u32,
    pub decode_total: usize,
    pub final_stalled: bool,
    pub final_master: i64,
    pub final_newest: i64,
}

impl CoupledPlayTrace {
    /// Max gap between consecutive presented video frames (0 = freeze, 1 = ok, >1 = jump).
    pub fn max_video_step(&self) -> i64 {
        self.video_presented
            .windows(2)
            .map(|w| (w[1] - w[0]).abs())
            .max()
            .unwrap_or(0)
    }

    pub fn max_audio_step(&self) -> i64 {
        self.audio_emitted
            .windows(2)
            .map(|w| (w[1] - w[0]).abs())
            .max()
            .unwrap_or(0)
    }

    pub fn video_freeze_ticks(&self) -> usize {
        self.video_presented
            .windows(2)
            .filter(|w| w[0] == w[1])
            .count()
    }

    /// Audio must be a dense range; never ahead of the max video frame seen.
    pub fn assert_av_invariants(&self) {
        if let (Some(&a0), Some(&a1)) = (self.audio_emitted.first(), self.audio_emitted.last()) {
            assert_eq!(
                self.audio_emitted,
                (a0..=a1).collect::<Vec<_>>(),
                "audio must be dense: {self:?}"
            );
        }
        let max_video = self.video_presented.iter().copied().max().unwrap_or(0);
        for &a in &self.audio_emitted {
            assert!(
                a <= max_video || self.video_presented.contains(&a),
                "audio frame {a} never presented on video timeline: {self:?}"
            );
            assert!(
                a <= self.final_newest,
                "audio past decode frontier: {self:?}"
            );
        }
        for &v in &self.video_presented {
            assert!(v <= self.final_newest || v <= max_video);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::broadcast::clock::{BroadcastMasterClock, ClockReference};
    use crate::broadcast::sync::{AudioSampleSpan, BROADCAST_AUDIO_SAMPLE_RATE_HZ};
    use crate::broadcast::timebase::{FrameRange, Timebase};
    use crate::broadcast::CelluloidTrack;
    use std::time::{Duration, Instant};

    #[test]
    fn hold_policy_skips_intermediate_audio_when_master_jumps() {
        let emitted = simulate_hold_policy_emits(&[100, 101, 102, 103, 104], &[100, 104]);
        assert_eq!(emitted, vec![100, 104]);
    }

    #[test]
    fn contiguous_contract_never_skips_on_same_jump() {
        let emitted = simulate_contiguous_emits(&[100, 104], &[100, 104]).expect("emit");
        assert_eq!(emitted, vec![100, 101, 102, 103, 104]);
    }

    #[test]
    fn contiguous_contract_emits_nothing_past_decode_frontier() {
        let emitted = simulate_contiguous_emits(&[100], &[105]).expect("emit");
        assert_eq!(emitted, vec![100]);
    }

    #[test]
    fn contiguous_contract_rejects_huge_backlog() {
        let err = audio_emit_range(
            Some(FrameNumber(0)),
            FrameNumber(200),
            Some(FrameNumber(200)),
        )
        .expect_err("backlog");
        assert!(err.message.contains("backlog"));
    }

    #[test]
    fn clock_ahead_detects_underrun() {
        assert!(clock_ahead_of_decode(
            FrameNumber(110),
            Some(FrameNumber(105))
        ));
        assert!(!clock_ahead_of_decode(
            FrameNumber(105),
            Some(FrameNumber(105))
        ));
    }

    #[test]
    fn stall_prevents_master_from_skipping_decode_frontier() {
        let tb = Timebase::from_source_fps(25.0);
        let range = FrameRange::new(FrameNumber(100), FrameNumber(200));
        let mut clock = BroadcastMasterClock::new(tb, range, ClockReference::InternalMonotonic);
        let t0 = Instant::now();
        clock.play_from(FrameNumber(100), t0);

        let wall = clock.current_frame(t0 + Duration::from_millis(400));
        assert_eq!(wall, FrameNumber(110));
        clock.stall_at(FrameNumber(102));
        assert_eq!(
            clock.current_frame(t0 + Duration::from_secs(5)),
            FrameNumber(102)
        );
        clock.resume(t0 + Duration::from_secs(5));
        assert_eq!(
            clock.current_frame(t0 + Duration::from_secs(5) + Duration::from_millis(40)),
            FrameNumber(103)
        );
    }

    #[test]
    fn carrier_audio_spans_are_abutting_at_25fps() {
        let carrier = CelluloidTrack::new(
            "project",
            "shot",
            "clip",
            Timebase::from_source_fps(25.0),
            FrameRange::new(FrameNumber(100), FrameNumber(110)),
        );
        let mut prev_end = 0_i64;
        for offset in 0..10 {
            let frame = FrameNumber(100 + offset);
            let span = AudioSampleSpan::from_carrier_frame(
                &carrier,
                frame,
                BROADCAST_AUDIO_SAMPLE_RATE_HZ,
            );
            assert_eq!(span.start_sample, prev_end);
            assert_eq!(span.len(), 1_920);
            prev_end = span.end_exclusive;
        }
    }

    #[test]
    fn hold_policy_multi_tick_catchup_also_skips() {
        let emitted =
            simulate_hold_policy_emits(&[100, 101, 102, 103, 104, 105, 106], &[100, 101, 105, 106]);
        assert_eq!(emitted, vec![100, 101, 105, 106]);
    }

    #[test]
    fn contiguous_contract_same_catchup_is_dense() {
        let emitted =
            simulate_contiguous_emits(&[100, 101, 106, 106], &[100, 101, 105, 106]).expect("emit");
        assert_eq!(emitted, (100..=106).collect::<Vec<_>>());
    }

    #[test]
    fn resume_on_one_frame_thrashes_video_and_audio() {
        // PROOF of "sve zapinje": resume as soon as +1 is ready, then wall
        // outruns sparse decode → stall again → freeze/unfreeze.
        let thrash = simulate_stall_resume_cycles(
            100,
            &[1, 0, 0, 1, 0, 0, 1, 0, 0, 1, 0, 0],
            1, // bad policy
        );
        assert!(
            thrash.resumes >= 3 && thrash.stalls >= 2,
            "resume-on-+1 must thrash under sparse decode: {thrash:?}"
        );
        let duplicates = thrash.presented.windows(2).filter(|w| w[0] == w[1]).count();
        assert!(
            duplicates >= 3,
            "video holds same frame across ticks while thrashing: {thrash:?}"
        );
    }

    #[test]
    fn resume_after_buffer_avoids_stall_thrash() {
        // Same sparse decode, but wait for a cushion before resume.
        let ok = simulate_stall_resume_cycles(
            100,
            &[1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1],
            STALL_RESUME_BUFFER_FRAMES,
        );
        assert_eq!(ok.resumes, 1, "one resume after buffer fill: {ok:?}");
        assert_eq!(ok.stalls, 0, "must not re-stall immediately: {ok:?}");
        let advancing = ok.presented.windows(2).filter(|w| w[1] == w[0] + 1).count();
        assert!(
            advancing >= 4,
            "after healthy resume, video advances frame-by-frame: {ok:?}"
        );
    }

    #[test]
    fn decode_budget_refills_while_stalled_or_low() {
        assert_eq!(decode_budget(true, false, 8, 4, 4), 0);
        assert_eq!(decode_budget(true, false, 1, 4, 4), 3);
        assert_eq!(decode_budget(true, true, 0, 4, 4), 4);
        assert_eq!(decode_budget(false, false, 0, 4, 4), 4);
    }

    #[test]
    fn should_resume_requires_cushion_not_one_frame() {
        assert!(!should_resume_after_stall(
            FrameNumber(100),
            FrameNumber(101),
            STALL_RESUME_BUFFER_FRAMES
        ));
        assert!(should_resume_after_stall(
            FrameNumber(100),
            FrameNumber(104),
            STALL_RESUME_BUFFER_FRAMES
        ));
    }

    #[test]
    fn coupled_loop_with_bad_resume_jumps_or_thrashes() {
        // Wall faster than decode; resume-on-+1 thrashs more than cushion.
        let bad = simulate_coupled_play_loop(100, 40, 2, 1, 4, 1);
        let good = simulate_coupled_play_loop(100, 40, 2, 1, 4, STALL_RESUME_BUFFER_FRAMES);
        assert!(
            bad.stalls > good.stalls && bad.resumes > good.resumes,
            "resume-on-+1 must thrash more than cushion:\n bad={bad:?}\n good={good:?}"
        );
        assert!(
            good.max_audio_step() <= 1,
            "cushion must keep audio dense: {good:?}"
        );
        assert!(
            good.max_video_step() <= 2,
            "video steps must not exceed wall advance: {good:?}"
        );
        // Cushion spends ticks frozen while filling; resume-on-+1 runs/jumps more.
        assert!(
            good.video_freeze_ticks() > bad.video_freeze_ticks(),
            "cushion should freeze to refill; bad runs hot: bad={} good={}",
            bad.video_freeze_ticks(),
            good.video_freeze_ticks()
        );
    }

    #[test]
    fn coupled_loop_with_cushion_keeps_av_lockstep_no_jumps() {
        let ok = simulate_coupled_play_loop(100, 60, 1, 4, 4, STALL_RESUME_BUFFER_FRAMES);
        assert_eq!(ok.max_video_step(), 1, "video must not jump: {ok:?}");
        assert!(ok.max_audio_step() <= 1, "audio must be contiguous: {ok:?}");
        assert!(ok.stalls <= 1, "healthy decode should rarely stall: {ok:?}");
        assert!(!ok.audio_emitted.is_empty());
        assert_eq!(
            ok.audio_emitted,
            (ok.audio_emitted[0]..=*ok.audio_emitted.last().unwrap()).collect::<Vec<_>>()
        );
    }

    #[test]
    fn coupled_loop_slow_decode_freezes_then_continues_without_skip() {
        // wall=2, decode=1 → must stall; cushion ⇒ freeze not skip.
        let slow = simulate_coupled_play_loop(0, 80, 2, 1, 4, STALL_RESUME_BUFFER_FRAMES);
        assert!(slow.max_audio_step() <= 1, "audio must not skip: {slow:?}");
        assert!(
            slow.max_video_step() <= 2,
            "video must not skip more than wall step: {slow:?}"
        );
        assert!(
            slow.stalls >= 1 && slow.video_freeze_ticks() > 0,
            "slow decode should stall/freeze: {slow:?}"
        );
        if let (Some(&a0), Some(&a1)) = (slow.audio_emitted.first(), slow.audio_emitted.last()) {
            assert_eq!(slow.audio_emitted, (a0..=a1).collect::<Vec<_>>());
        }
    }

    #[test]
    fn coupled_loop_never_presents_past_decode_frontier() {
        let trace = simulate_coupled_play_loop(50, 30, 3, 1, 4, STALL_RESUME_BUFFER_FRAMES);
        for &v in &trace.video_presented {
            assert!(
                v <= 50 + trace.decode_total as i64,
                "video frame {v} past decode total: {trace:?}"
            );
            assert!(
                v <= trace.final_newest,
                "video frame {v} past final newest: {trace:?}"
            );
        }
        for &a in &trace.audio_emitted {
            assert!(a <= trace.final_newest);
        }
    }

    #[test]
    fn audio_spans_abut_for_broadcast_rates() {
        for fps in [24.0, 25.0, 29.97, 30.0, 50.0] {
            let tb = Timebase::from_source_fps(fps);
            let carrier = CelluloidTrack::new(
                "p",
                "s",
                "c",
                tb,
                FrameRange::new(FrameNumber(0), FrameNumber(30)),
            );
            let mut prev_end = 0_i64;
            for f in 0..20 {
                let span = AudioSampleSpan::from_carrier_frame(
                    &carrier,
                    FrameNumber(f),
                    BROADCAST_AUDIO_SAMPLE_RATE_HZ,
                );
                assert_eq!(
                    span.start_sample, prev_end,
                    "gap/overlap at fps={fps} frame={f}"
                );
                assert!(span.len() > 0);
                prev_end = span.end_exclusive;
            }
        }
    }

    #[test]
    fn decode_budget_monotonic_in_ahead() {
        // More headroom ⇒ smaller or equal budget while playing/not stalled.
        let mut prev = decode_budget(true, false, 0, 8, 4);
        for ahead in 1..=10 {
            let b = decode_budget(true, false, ahead, 8, 4);
            assert!(
                b <= prev,
                "budget must not grow as ahead grows: ahead={ahead} b={b} prev={prev}"
            );
            prev = b;
        }
        assert_eq!(decode_budget(true, false, 8, 8, 4), 0);
    }

    #[test]
    fn stall_at_idempotent_and_resume_is_realtime() {
        let tb = Timebase::from_source_fps(25.0);
        let range = FrameRange::new(FrameNumber(0), FrameNumber(1000));
        let mut clock = BroadcastMasterClock::new(tb, range, ClockReference::InternalMonotonic);
        let t0 = Instant::now();
        clock.play_from(FrameNumber(0), t0);
        clock.stall_at(FrameNumber(10));
        clock.stall_at(FrameNumber(10));
        assert!(clock.is_stalled());
        assert_eq!(
            clock.current_frame(t0 + Duration::from_secs(1)),
            FrameNumber(10)
        );

        let t1 = t0 + Duration::from_millis(500);
        clock.resume(t1);
        assert_eq!(
            clock.current_frame(t1 + Duration::from_millis(120)),
            FrameNumber(13)
        );
    }

    #[test]
    fn coupled_loop_matrix_cushion_never_skips_audio() {
        // Property-style: many wall/decode mixes under cushion policy.
        for wall in [1_i64, 2, 3] {
            for cap in [1_usize, 2, 4] {
                let trace =
                    simulate_coupled_play_loop(0, 50, wall, cap, 4, STALL_RESUME_BUFFER_FRAMES);
                trace.assert_av_invariants();
                assert!(
                    trace.max_audio_step() <= 1,
                    "wall={wall} cap={cap} audio skip: {trace:?}"
                );
                assert!(
                    trace.max_video_step() <= wall.max(1),
                    "wall={wall} cap={cap} video jump: {trace:?}"
                );
            }
        }
    }

    #[test]
    fn coupled_loop_resume_on_one_always_thrashes_vs_cushion() {
        for wall in [1_i64, 2] {
            for cap in [1_usize, 2] {
                let bad = simulate_coupled_play_loop(10, 36, wall, cap, 4, 1);
                let good =
                    simulate_coupled_play_loop(10, 36, wall, cap, 4, STALL_RESUME_BUFFER_FRAMES);
                good.assert_av_invariants();
                if wall > cap as i64 {
                    assert!(
                        bad.stalls + bad.resumes > good.stalls + good.resumes,
                        "wall={wall} cap={cap}: bad should cycle more\n bad={bad:?}\n good={good:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn audio_emit_range_is_prefix_closed_under_master_growth() {
        // Growing master with fixed newest only extends the emit suffix.
        let mut last = None;
        let mut all = Vec::new();
        for master in 100..=110 {
            let batch =
                audio_emit_range(last, FrameNumber(master), Some(FrameNumber(110))).unwrap();
            for f in &batch {
                all.push(f.0);
                last = Some(*f);
            }
        }
        assert_eq!(all, (100..=110).collect::<Vec<_>>());
    }

    #[test]
    fn audio_emit_range_first_cue_does_not_dump_backlog() {
        // Opening on frame 150 with buffer already at 160 must not dump 150..160.
        let batch = audio_emit_range(None, FrameNumber(150), Some(FrameNumber(160))).unwrap();
        assert_eq!(batch, vec![FrameNumber(150)]);
    }

    #[test]
    fn decode_budget_zero_only_when_healthy_and_running() {
        assert_eq!(decode_budget(true, false, 4, 4, 4), 0);
        assert_eq!(decode_budget(true, true, 4, 4, 4), 1); // stalled still refills at least 1
        assert_ne!(decode_budget(true, false, 3, 4, 4), 0);
    }

    #[test]
    fn hold_policy_must_not_drive_live_sink_on_master_jump() {
        // Hold returns latest ≤ master and discards intermediates — that is the
        // crackle failure mode. Live engine must use take_exact + audio_emit_range.
        let mut queue = crate::broadcast::AudioFrameQueue::new(8);
        for f in 100..=104 {
            queue.push_decoded(FrameNumber(f), f);
        }
        let master = FrameNumber(104);
        let held = queue.frame_for_program_clock(master).unwrap();
        assert_eq!(held.frame, FrameNumber(104));
        assert_eq!(queue.len(), 1, "hold discarded 100..103");
    }

    #[test]
    fn contiguous_emit_keeps_dense_pcm_on_master_jump() {
        let mut queue = crate::broadcast::AudioFrameQueue::new(16);
        for f in 100..=104 {
            queue.push_decoded(FrameNumber(f), f);
        }
        let mut last = Some(FrameNumber(99));
        let mut emitted = Vec::new();
        for &master in &[100_i64, 104] {
            let batch = audio_emit_range(last, FrameNumber(master), queue.newest_frame()).unwrap();
            for frame in batch {
                let got = queue.take_exact_frame(frame).expect("exact");
                emitted.push(got.payload);
                last = Some(frame);
            }
        }
        assert_eq!(emitted, vec![100, 101, 102, 103, 104]);
    }

    #[test]
    fn contiguous_emit_appends_when_audio_and_master_align() {
        let mut queue = crate::broadcast::AudioFrameQueue::new(8);
        queue.push_decoded(FrameNumber(200), 200);
        queue.push_decoded(FrameNumber(201), 201);
        let master = FrameNumber(201);
        let last = Some(FrameNumber(200));
        let batch = audio_emit_range(last, master, queue.newest_frame()).unwrap();
        assert_eq!(batch, vec![FrameNumber(201)]);
        assert_eq!(
            queue.take_exact_frame(FrameNumber(201)).unwrap().payload,
            201
        );
    }

    #[test]
    fn decode_frontier_uses_slower_of_video_and_audio() {
        assert_eq!(
            decode_frontier_for_stall(true, true, Some(FrameNumber(110)), Some(FrameNumber(105)),),
            Some(FrameNumber(105))
        );
        assert_eq!(
            decode_frontier_for_stall(true, false, Some(FrameNumber(110)), None),
            Some(FrameNumber(110))
        );
        assert_eq!(
            decode_frontier_for_stall(false, true, None, Some(FrameNumber(50))),
            Some(FrameNumber(50))
        );
    }

    #[test]
    fn sequential_contiguous_ticks_emit_every_frame() {
        let mut queue = crate::broadcast::AudioFrameQueue::new(32);
        for f in 100..=110 {
            queue.push_decoded(FrameNumber(f), f);
        }
        let mut last = None;
        let mut emitted = Vec::new();
        for master in 100..=110 {
            let batch = audio_emit_range(last, FrameNumber(master), queue.newest_frame()).unwrap();
            for frame in batch {
                emitted.push(queue.take_exact_frame(frame).unwrap().payload);
                last = Some(frame);
            }
        }
        assert_eq!(emitted, (100..=110).collect::<Vec<_>>());
    }

    #[test]
    fn thin_preroll_with_cold_decode_causes_start_hitch() {
        // Old policy depth (4) + cold pipes: many freeze ticks at play start.
        let thin = simulate_engine_lifecycle(
            100,
            199,
            4,
            80,
            1,
            0,
            20,
            2,
            199,
            8,
            STALL_RESUME_BUFFER_FRAMES,
            true,
        );
        let deep = simulate_engine_lifecycle(
            100,
            199,
            PLAY_START_MIN_BUFFER_FRAMES,
            80,
            1,
            0,
            20,
            2,
            199,
            8,
            STALL_RESUME_BUFFER_FRAMES,
            true,
        );
        assert!(
            thin.freeze_ticks > deep.freeze_ticks,
            "12-frame preroll must reduce start freeze vs 4: thin={} deep={}",
            thin.freeze_ticks,
            deep.freeze_ticks
        );
        assert!(
            deep.freeze_ticks < 25,
            "deep preroll start must not zapne long: {deep:?}"
        );
    }

    #[test]
    fn eof_before_last_without_soft_eos_stalls_forever() {
        let bad = simulate_engine_lifecycle(
            100,
            150,
            12,
            200,
            1,
            2,
            0,
            2,
            140,
            8,
            STALL_RESUME_BUFFER_FRAMES,
            false,
        );
        assert!(
            bad.final_stalled && !bad.completed_clean,
            "EOF without soft-EOS must hang: {bad:?}"
        );
    }

    #[test]
    fn eof_before_last_with_soft_eos_completes_clean() {
        let ok = simulate_engine_lifecycle(
            100,
            150,
            12,
            200,
            1,
            2,
            0,
            2,
            140,
            8,
            STALL_RESUME_BUFFER_FRAMES,
            true,
        );
        assert!(
            ok.completed_clean,
            "soft-EOS must finish play on EOF: {ok:?}"
        );
        assert!(!ok.final_stalled, "must not remain stalled: {ok:?}");
        assert!(ok.soft_eos_at_tick.is_some(), "{ok:?}");
    }

    #[test]
    fn soft_eos_progress_on_last_or_exhausted_stall() {
        assert!(soft_eos_tick_progress(
            FrameNumber(99),
            FrameNumber(99),
            false,
            false
        ));
        assert!(soft_eos_tick_progress(
            FrameNumber(90),
            FrameNumber(99),
            true,
            true
        ));
        assert!(!soft_eos_tick_progress(
            FrameNumber(90),
            FrameNumber(99),
            true,
            false
        ));
    }

    #[test]
    fn decode_error_recovers_then_fatals_or_soft_eos() {
        assert_eq!(
            decode_error_action(1, false, MAX_DECODE_RECOVER_STREAK),
            DecodeErrorAction::Recover
        );
        assert_eq!(
            decode_error_action(2, false, MAX_DECODE_RECOVER_STREAK),
            DecodeErrorAction::Recover
        );
        assert_eq!(
            decode_error_action(3, false, MAX_DECODE_RECOVER_STREAK),
            DecodeErrorAction::Fatal
        );
        assert_eq!(
            decode_error_action(3, true, MAX_DECODE_RECOVER_STREAK),
            DecodeErrorAction::SoftEos
        );
    }
}
