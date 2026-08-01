//! Audio frame queue for broadcast playback.
//!
//! Audio payloads are consumers of the carrier/master clock, not the clock
//! source. The queue mirrors the video queue behavior: it exposes the latest
//! ready audio payload for a requested program frame and never advances time by
//! itself.

use std::collections::VecDeque;

use super::timebase::FrameNumber;

#[derive(Debug, Clone, PartialEq)]
pub struct QueuedAudioFrame<T> {
    pub frame: FrameNumber,
    pub payload: T,
}

#[derive(Debug, Clone)]
pub struct AudioFrameQueue<T> {
    frames: VecDeque<QueuedAudioFrame<T>>,
    capacity: usize,
}

impl<T> AudioFrameQueue<T> {
    pub fn new(capacity: usize) -> Self {
        Self {
            frames: VecDeque::with_capacity(capacity.max(1)),
            capacity: capacity.max(1),
        }
    }

    pub fn len(&self) -> usize {
        self.frames.len()
    }

    pub fn is_empty(&self) -> bool {
        self.frames.is_empty()
    }

    pub fn oldest_frame(&self) -> Option<FrameNumber> {
        self.frames.front().map(|frame| frame.frame)
    }

    pub fn newest_frame(&self) -> Option<FrameNumber> {
        self.frames.back().map(|frame| frame.frame)
    }

    pub fn clear(&mut self) {
        self.frames.clear();
    }

    pub fn push_decoded(&mut self, frame: FrameNumber, payload: T) {
        self.frames.push_back(QueuedAudioFrame { frame, payload });
        while self.frames.len() > self.capacity {
            self.frames.pop_front();
        }
    }

    /// Take exactly `frame` for contiguous sink append.
    ///
    /// Drops older frames (already emitted / intentionally skipped). Does **not**
    /// discard newer neighbors — unlike [`Self::frame_for_program_clock`].
    pub fn take_exact_frame(&mut self, frame: FrameNumber) -> Option<QueuedAudioFrame<T>> {
        while let Some(front) = self.frames.front() {
            if front.frame.0 < frame.0 {
                self.frames.pop_front();
            } else {
                break;
            }
        }
        match self.frames.front() {
            Some(front) if front.frame == frame => self.frames.pop_front(),
            _ => None,
        }
    }
}

impl<T: Clone> AudioFrameQueue<T> {
    /// Hold policy: latest ready ≤ master. Discards intermediates — do **not**
    /// use this for live PCM sink append (see [`Self::take_exact_frame`]).
    pub fn frame_for_program_clock(
        &mut self,
        master_frame: FrameNumber,
    ) -> Option<QueuedAudioFrame<T>> {
        while self.frames.len() > 1 {
            let Some(next) = self.frames.get(1) else {
                break;
            };
            if next.frame <= master_frame {
                self.frames.pop_front();
            } else {
                break;
            }
        }

        self.frames
            .front()
            .filter(|frame| frame.frame <= master_frame)
            .cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn audio_queue_returns_latest_ready_frame_for_program_clock() {
        let mut queue = AudioFrameQueue::new(8);
        queue.push_decoded(FrameNumber(100), "a100");
        queue.push_decoded(FrameNumber(101), "a101");
        queue.push_decoded(FrameNumber(102), "a102");

        assert_eq!(
            queue.frame_for_program_clock(FrameNumber(101)),
            Some(QueuedAudioFrame {
                frame: FrameNumber(101),
                payload: "a101"
            })
        );
        assert_eq!(
            queue.frame_for_program_clock(FrameNumber(103)),
            Some(QueuedAudioFrame {
                frame: FrameNumber(102),
                payload: "a102"
            })
        );
    }

    #[test]
    fn audio_queue_does_not_present_future_audio() {
        let mut queue = AudioFrameQueue::new(8);
        queue.push_decoded(FrameNumber(100), "a100");

        assert_eq!(queue.frame_for_program_clock(FrameNumber(99)), None);
    }

    #[test]
    fn audio_queue_exposes_buffer_span_without_owning_clock() {
        let mut queue = AudioFrameQueue::new(8);
        assert_eq!(queue.oldest_frame(), None);
        assert_eq!(queue.newest_frame(), None);

        queue.push_decoded(FrameNumber(100), "a100");
        queue.push_decoded(FrameNumber(101), "a101");

        assert_eq!(queue.oldest_frame(), Some(FrameNumber(100)));
        assert_eq!(queue.newest_frame(), Some(FrameNumber(101)));
    }

    #[test]
    fn audio_queue_discards_skipped_frames_on_master_jump() {
        // Documented failure mode: one presentable() call after a jump cannot
        // recover intermediate frames — they are popped and gone.
        let mut queue = AudioFrameQueue::new(8);
        for f in 100..=104 {
            queue.push_decoded(FrameNumber(f), f);
        }
        let got = queue
            .frame_for_program_clock(FrameNumber(104))
            .expect("latest");
        assert_eq!(got.payload, 104);
        assert_eq!(queue.oldest_frame(), Some(FrameNumber(104)));
        assert_eq!(queue.len(), 1, "101..103 were discarded, not deferred");
    }

    #[test]
    fn audio_queue_evicts_oldest_when_over_capacity() {
        let mut queue = AudioFrameQueue::new(3);
        for f in 10..=14 {
            queue.push_decoded(FrameNumber(f), f);
        }
        assert_eq!(queue.len(), 3);
        assert_eq!(queue.oldest_frame(), Some(FrameNumber(12)));
        assert_eq!(queue.newest_frame(), Some(FrameNumber(14)));
    }

    #[test]
    fn audio_queue_sequential_master_emits_every_frame() {
        // Healthy play: master advances one frame per tick → every PCM chunk.
        let mut queue = AudioFrameQueue::new(16);
        for f in 100..=110 {
            queue.push_decoded(FrameNumber(f), f);
        }
        let mut emitted = Vec::new();
        let mut last = None;
        for master in 100..=110 {
            let got = queue
                .frame_for_program_clock(FrameNumber(master))
                .expect("ready");
            if last != Some(got.payload) {
                emitted.push(got.payload);
                last = Some(got.payload);
            }
        }
        assert_eq!(emitted, (100..=110).collect::<Vec<_>>());
    }

    #[test]
    fn audio_queue_same_master_represents_same_frame_without_advance() {
        let mut queue = AudioFrameQueue::new(4);
        queue.push_decoded(FrameNumber(50), "a50");
        queue.push_decoded(FrameNumber(51), "a51");
        let a = queue.frame_for_program_clock(FrameNumber(50)).unwrap();
        let b = queue.frame_for_program_clock(FrameNumber(50)).unwrap();
        assert_eq!(a.payload, "a50");
        assert_eq!(b.payload, "a50");
        assert_eq!(
            queue.len(),
            2,
            "re-query must not consume the only ready frame"
        );
    }

    #[test]
    fn audio_queue_clear_drops_all_ready_pcm() {
        let mut queue = AudioFrameQueue::new(4);
        queue.push_decoded(FrameNumber(1), 1);
        queue.push_decoded(FrameNumber(2), 2);
        queue.clear();
        assert!(queue.is_empty());
        assert_eq!(queue.frame_for_program_clock(FrameNumber(2)), None);
    }

    #[test]
    fn audio_queue_capacity_one_keeps_only_newest() {
        let mut queue = AudioFrameQueue::new(0); // max(1)
        queue.push_decoded(FrameNumber(1), 1);
        queue.push_decoded(FrameNumber(2), 2);
        assert_eq!(queue.len(), 1);
        assert_eq!(queue.newest_frame(), Some(FrameNumber(2)));
    }

    #[test]
    fn take_exact_preserves_neighbors_on_master_jump() {
        let mut queue = AudioFrameQueue::new(16);
        for f in 100..=104 {
            queue.push_decoded(FrameNumber(f), f);
        }
        // Contiguous catch-up after jump 100 → 104: every frame still available.
        let mut emitted = Vec::new();
        for f in 100..=104 {
            let got = queue.take_exact_frame(FrameNumber(f)).expect("exact");
            emitted.push(got.payload);
        }
        assert_eq!(emitted, vec![100, 101, 102, 103, 104]);
        assert!(queue.is_empty());
    }

    #[test]
    fn take_exact_returns_none_on_gap_without_discarding_newer() {
        let mut queue = AudioFrameQueue::new(8);
        queue.push_decoded(FrameNumber(100), 100);
        queue.push_decoded(FrameNumber(102), 102);
        // Requesting 101 drops stale 100, then sees gap before 102 — keep 102.
        assert!(queue.take_exact_frame(FrameNumber(101)).is_none());
        assert_eq!(queue.oldest_frame(), Some(FrameNumber(102)));
        assert_eq!(
            queue.take_exact_frame(FrameNumber(102)).unwrap().payload,
            102
        );
    }
}
