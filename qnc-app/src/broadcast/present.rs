//! Present: composite decoded video layers → egui image; mix audio buses → PCM.

use eframe::egui::{Color32, ColorImage};

use super::backend::{DecodedAudioBus, DecodedVideoLayer};
use super::layers::AudioChannel;
use super::payload::{BroadcastAudioPayload, BroadcastPixelFormat, BroadcastVideoPayload};
use super::sync::BROADCAST_AUDIO_SAMPLE_RATE_HZ;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PresentError {
    pub message: String,
}

impl PresentError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

/// Composite video layers by decode order (already Z-sorted in the plan): base
/// first, then overlays with simple source-over alpha blend. Empty → black.
pub fn composite_video_layers(
    layers: &[DecodedVideoLayer<BroadcastVideoPayload>],
    fallback_width: u32,
    fallback_height: u32,
) -> Result<ColorImage, PresentError> {
    let mut canvas: Option<(u32, u32, Vec<u8>)> = None;

    for layer in layers {
        let Some(payload) = layer.payload.as_ref() else {
            continue;
        };
        let rgba = payload_to_rgba(payload)?;
        match &mut canvas {
            None => {
                canvas = Some((payload.width, payload.height, rgba));
            }
            Some((w, h, base)) => {
                if payload.width != *w || payload.height != *h {
                    return Err(PresentError::new(format!(
                        "overlay {} size {}x{} does not match base {}x{}",
                        layer.layer_id, payload.width, payload.height, w, h
                    )));
                }
                blend_source_over(base, &rgba);
            }
        }
    }

    let (width, height, rgba) = canvas.unwrap_or_else(|| {
        let w = fallback_width.max(1);
        let h = fallback_height.max(1);
        (w, h, vec![0; (w as usize) * (h as usize) * 4])
    });

    Ok(ColorImage::from_rgba_unmultiplied(
        [width as usize, height as usize],
        &rgba,
    ))
}

/// Route independent mono program buses to a dual-mono monitor pair.
///
/// - A1 (and A3) → **left only**
/// - A2 (and A4) → **right only**
///
/// No L=R fold and no summing of A1 into R (or A2 into L). Each bus is mono
/// after source-channel extract; if a multi-channel payload still arrives it is
/// averaged to mono for that bus only, then hard-panned.
pub fn mix_audio_buses(
    buses: &[DecodedAudioBus<BroadcastAudioPayload>],
) -> Result<(u32, u8, Vec<f32>), PresentError> {
    let mut out_frames = 0_usize;
    for bus in buses {
        if !bus.mix.is_audible() {
            continue;
        }
        let Some(payload) = bus.payload.as_ref() else {
            continue;
        };
        out_frames = out_frames.max(payload.sample_frames());
    }
    if out_frames == 0 {
        return Ok((BROADCAST_AUDIO_SAMPLE_RATE_HZ, 2, Vec::new()));
    }

    let out_channels = 2_u8;
    let mut mixed = vec![0.0_f32; out_frames * out_channels as usize];

    for bus in buses {
        if !bus.mix.is_audible() {
            continue;
        }
        let Some(payload) = bus.payload.as_ref() else {
            continue;
        };
        let gain = bus.mix.effective_linear_gain();
        if gain == 0.0 {
            continue;
        }
        let Some(side) = dual_mono_side(bus.channel) else {
            continue;
        };
        let ch = payload.channels.max(1) as usize;
        let frames = payload.sample_frames().min(out_frames);
        for i in 0..frames {
            let src_base = i * ch;
            let mut mono = 0.0_f32;
            for c in 0..ch {
                mono += payload.samples[src_base + c];
            }
            mono = (mono / ch as f32) * gain;
            mixed[i * 2 + side] += mono;
        }
    }

    // Soft clip
    for sample in &mut mixed {
        *sample = sample.clamp(-1.0, 1.0);
    }

    Ok((BROADCAST_AUDIO_SAMPLE_RATE_HZ, out_channels, mixed))
}

/// Dual-mono monitor map: odd buses → L, even buses → R.
fn dual_mono_side(channel: AudioChannel) -> Option<usize> {
    match channel.get() {
        1 | 3 => Some(0),
        2 | 4 => Some(1),
        _ => None,
    }
}

fn payload_to_rgba(payload: &BroadcastVideoPayload) -> Result<Vec<u8>, PresentError> {
    let w = payload.width as usize;
    let h = payload.height as usize;
    let bpp = payload.pixel_format.bytes_per_pixel();
    let mut out = vec![0_u8; w * h * 4];
    for y in 0..h {
        let src_row = y * payload.stride_bytes;
        let dst_row = y * w * 4;
        for x in 0..w {
            let src = src_row + x * bpp;
            let dst = dst_row + x * 4;
            match payload.pixel_format {
                BroadcastPixelFormat::Rgba8 => {
                    out[dst..dst + 4].copy_from_slice(&payload.data[src..src + 4]);
                }
                BroadcastPixelFormat::Bgra8 => {
                    out[dst] = payload.data[src + 2];
                    out[dst + 1] = payload.data[src + 1];
                    out[dst + 2] = payload.data[src];
                    out[dst + 3] = payload.data[src + 3];
                }
            }
        }
    }
    Ok(out)
}

fn blend_source_over(dst: &mut [u8], src: &[u8]) {
    for (d, s) in dst.chunks_exact_mut(4).zip(src.chunks_exact(4)) {
        let a = s[3] as f32 / 255.0;
        if a <= 0.0 {
            continue;
        }
        if a >= 1.0 {
            d.copy_from_slice(s);
            continue;
        }
        let inv = 1.0 - a;
        d[0] = (s[0] as f32 * a + d[0] as f32 * inv).round() as u8;
        d[1] = (s[1] as f32 * a + d[1] as f32 * inv).round() as u8;
        d[2] = (s[2] as f32 * a + d[2] as f32 * inv).round() as u8;
        d[3] = 255;
    }
}

/// Status overlay string from playout diagnostics (Phase D).
pub fn diagnostics_status_hint(
    problem: Option<super::diagnostics::PlayoutProblem>,
) -> Option<String> {
    use super::diagnostics::PlayoutProblem;
    match problem {
        None => None,
        Some(PlayoutProblem::NoPresentation) => Some("Waiting presentation".into()),
        Some(PlayoutProblem::PresentationBehind { held_frame }) => {
            Some(format!("Hold presentation @{}", held_frame.0))
        }
        Some(PlayoutProblem::VideoMissing) => Some("Video missing".into()),
        Some(PlayoutProblem::VideoBehind { held_frame }) => {
            Some(format!("Hold video @{}", held_frame.0))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::broadcast::backend::DecodedVideoLayer;
    use crate::broadcast::layers::AudioMix;
    use crate::broadcast::render::VideoRenderRole;
    use crate::broadcast::sync::AudioSampleSpan;
    use crate::broadcast::timebase::FrameNumber;
    use crate::broadcast::AudioChannel;

    #[test]
    fn composite_stacks_overlay_over_base() {
        let base =
            BroadcastVideoPayload::new_rgba8(2, 1, vec![255, 0, 0, 255, 255, 0, 0, 255]).unwrap();
        let overlay = BroadcastVideoPayload::new_rgba8(
            2,
            1,
            vec![0, 255, 0, 255, 0, 0, 0, 0], // left green opaque, right transparent
        )
        .unwrap();
        let layers = vec![
            DecodedVideoLayer {
                layer_id: "base".into(),
                role: VideoRenderRole::Base,
                source_frame: FrameNumber(0),
                pts_sec: 0.0,
                media_seek_sec: 0.0,
                payload: Some(base),
            },
            DecodedVideoLayer {
                layer_id: "ov".into(),
                role: VideoRenderRole::Overlay { index: 1 },
                source_frame: FrameNumber(0),
                pts_sec: 0.0,
                media_seek_sec: 0.0,
                payload: Some(overlay),
            },
        ];
        let image = composite_video_layers(&layers, 2, 1).unwrap();
        assert_eq!(image.size, [2, 1]);
        assert_eq!(image.pixels[0], Color32::from_rgb(0, 255, 0));
        assert_eq!(image.pixels[1], Color32::from_rgb(255, 0, 0));
    }

    #[test]
    fn mix_applies_gain_across_buses() {
        let span = AudioSampleSpan {
            start_sample: 0,
            end_exclusive: 2,
        };
        let a1 = BroadcastAudioPayload::new_f32_interleaved(
            BROADCAST_AUDIO_SAMPLE_RATE_HZ,
            1,
            span,
            vec![1.0, 1.0],
        )
        .unwrap();
        let buses = vec![DecodedAudioBus {
            layer_id: "a1".into(),
            channel: AudioChannel::A1,
            mix: AudioMix::with_gain_db_tenths(-60), // -6dB ≈ 0.5
            source_frame: FrameNumber(0),
            pts_sec: 0.0,
            media_seek_sec: 0.0,
            sample_rate_hz: BROADCAST_AUDIO_SAMPLE_RATE_HZ,
            sample_span: span,
            payload: Some(a1),
        }];
        let (_rate, ch, samples) = mix_audio_buses(&buses).unwrap();
        assert_eq!(ch, 2);
        assert_eq!(samples.len(), 4);
        assert!((samples[0] - 0.5).abs() < 0.02, "A1 → left");
        assert!((samples[1]).abs() < 1e-6, "A1 must not appear on right");
    }

    #[test]
    fn mix_outputs_monitor_stereo_48k_from_mono_bus() {
        let span = AudioSampleSpan {
            start_sample: 0,
            end_exclusive: 1_920,
        };
        let samples = vec![0.25_f32; 1_920];
        let a1 = BroadcastAudioPayload::new_f32_interleaved(
            BROADCAST_AUDIO_SAMPLE_RATE_HZ,
            1,
            span,
            samples,
        )
        .unwrap();
        let buses = vec![DecodedAudioBus {
            layer_id: "a1".into(),
            channel: AudioChannel::A1,
            mix: AudioMix::UNITY,
            source_frame: FrameNumber(0),
            pts_sec: 0.0,
            media_seek_sec: 0.0,
            sample_rate_hz: BROADCAST_AUDIO_SAMPLE_RATE_HZ,
            sample_span: span,
            payload: Some(a1),
        }];
        let (rate, ch, mixed) = mix_audio_buses(&buses).unwrap();
        assert_eq!(rate, 48_000);
        assert_eq!(ch, 2);
        assert_eq!(mixed.len(), 1_920 * 2);
        assert!((mixed[0] - 0.25).abs() < 1e-6, "A1 → left");
        assert!((mixed[1]).abs() < 1e-6, "A1 silent on right");
    }

    fn bus(
        id: &str,
        channel: AudioChannel,
        mix: AudioMix,
        channels: u8,
        span: AudioSampleSpan,
        samples: Vec<f32>,
    ) -> DecodedAudioBus<BroadcastAudioPayload> {
        let payload = BroadcastAudioPayload::new_f32_interleaved(
            BROADCAST_AUDIO_SAMPLE_RATE_HZ,
            channels,
            span,
            samples,
        )
        .unwrap();
        DecodedAudioBus {
            layer_id: id.into(),
            channel,
            mix,
            source_frame: FrameNumber(0),
            pts_sec: 0.0,
            media_seek_sec: 0.0,
            sample_rate_hz: BROADCAST_AUDIO_SAMPLE_RATE_HZ,
            sample_span: span,
            payload: Some(payload),
        }
    }

    #[test]
    fn mix_dual_mono_a1_left_a2_right() {
        // Dual mono: A1 → L only, A2 → R only — no cross-mix.
        let span = AudioSampleSpan::new(0, 2);
        let buses = vec![
            bus(
                "a1",
                AudioChannel::A1,
                AudioMix::UNITY,
                1,
                span,
                vec![0.2, 0.2],
            ),
            bus(
                "a2",
                AudioChannel::A2,
                AudioMix::UNITY,
                1,
                span,
                vec![0.3, 0.3],
            ),
        ];
        let (_rate, ch, mixed) = mix_audio_buses(&buses).unwrap();
        assert_eq!(ch, 2);
        assert!((mixed[0] - 0.2).abs() < 1e-6, "A1 on left");
        assert!((mixed[1] - 0.3).abs() < 1e-6, "A2 on right");
        assert!((mixed[2] - 0.2).abs() < 1e-6);
        assert!((mixed[3] - 0.3).abs() < 1e-6);
    }

    #[test]
    fn mix_ignores_muted_and_missing_payload_buses() {
        let span = AudioSampleSpan::new(0, 1);
        let mut silent = bus("a2", AudioChannel::A2, AudioMix::UNITY, 1, span, vec![0.9]);
        silent.payload = None;
        let buses = vec![
            bus("a1", AudioChannel::A1, AudioMix::MUTED, 1, span, vec![1.0]),
            silent,
            bus("a3", AudioChannel::A3, AudioMix::UNITY, 1, span, vec![0.4]),
        ];
        let (_rate, _ch, mixed) = mix_audio_buses(&buses).unwrap();
        // A3 → left only (A1 muted, A2 missing).
        assert!((mixed[0] - 0.4).abs() < 1e-6);
        assert!((mixed[1]).abs() < 1e-6);
    }

    #[test]
    fn mix_mono_a1_hard_pans_left() {
        let span = AudioSampleSpan::new(0, 3);
        let buses = vec![bus(
            "a1",
            AudioChannel::A1,
            AudioMix::UNITY,
            1,
            span,
            vec![0.5, -0.5, 0.25],
        )];
        let (_rate, ch, mixed) = mix_audio_buses(&buses).unwrap();
        assert_eq!(ch, 2);
        assert_eq!(mixed.len(), 6);
        assert!((mixed[0] - 0.5).abs() < 1e-6 && mixed[1].abs() < 1e-6);
        assert!((mixed[2] + 0.5).abs() < 1e-6 && mixed[3].abs() < 1e-6);
        assert!((mixed[4] - 0.25).abs() < 1e-6 && mixed[5].abs() < 1e-6);
    }

    #[test]
    fn mix_soft_clips_per_side_when_same_ear_buses_sum_past_unity() {
        let span = AudioSampleSpan::new(0, 1);
        let buses = vec![
            bus("a1", AudioChannel::A1, AudioMix::UNITY, 1, span, vec![0.8]),
            bus("a3", AudioChannel::A3, AudioMix::UNITY, 1, span, vec![0.8]),
        ];
        let (_rate, _ch, mixed) = mix_audio_buses(&buses).unwrap();
        // A1+A3 both map to left → soft-clip; right stays silent.
        assert!((mixed[0] - 1.0).abs() < 1e-6, "0.8+0.8 on L must soft-clip");
        assert!(mixed[1].abs() < 1e-6);
    }

    #[test]
    fn mix_empty_audible_buses_returns_empty_pcm_at_48k_stereo() {
        let (rate, ch, samples) = mix_audio_buses(&[]).unwrap();
        assert_eq!(rate, 48_000);
        assert_eq!(ch, 2);
        assert!(samples.is_empty());
    }

    #[test]
    fn mix_uses_longest_bus_span_as_output_length() {
        let short = AudioSampleSpan::new(0, 1);
        let long = AudioSampleSpan::new(0, 3);
        let buses = vec![
            bus("a1", AudioChannel::A1, AudioMix::UNITY, 1, short, vec![1.0]),
            bus(
                "a2",
                AudioChannel::A2,
                AudioMix::UNITY,
                1,
                long,
                vec![0.5, 0.5, 0.5],
            ),
        ];
        let (_rate, _ch, mixed) = mix_audio_buses(&buses).unwrap();
        assert_eq!(mixed.len(), 6, "output follows longest audible bus");
        // Frame 0: A1→L 1.0, A2→R 0.5; frames 1–2: A2 only on R
        assert!((mixed[0] - 1.0).abs() < 1e-6);
        assert!((mixed[1] - 0.5).abs() < 1e-6);
        assert!(mixed[2].abs() < 1e-6);
        assert!((mixed[3] - 0.5).abs() < 1e-6);
        assert!(mixed[4].abs() < 1e-6);
        assert!((mixed[5] - 0.5).abs() < 1e-6);
    }

    #[test]
    fn one_frame_at_25fps_is_40ms_of_48k_pcm() {
        let span = AudioSampleSpan::from_carrier_frame(
            &crate::broadcast::CelluloidTrack::new(
                "p",
                "s",
                "c",
                crate::broadcast::Timebase::from_source_fps(25.0),
                crate::broadcast::FrameRange::new(FrameNumber(0), FrameNumber(10)),
            ),
            FrameNumber(0),
            BROADCAST_AUDIO_SAMPLE_RATE_HZ,
        );
        assert_eq!(span.len(), 1_920);
        let secs = span.len() as f64 / BROADCAST_AUDIO_SAMPLE_RATE_HZ as f64;
        assert!((secs - 0.04).abs() < 1e-9);
    }
}
