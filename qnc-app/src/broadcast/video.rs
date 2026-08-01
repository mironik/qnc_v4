//! Video frame queue for broadcast playback.
//!
//! The decoder/worker is deliberately not a clock. It only fills this queue
//! with decoded frames. The UI/render side selects frames from the queue using
//! the broadcast program clock position.

use std::collections::VecDeque;

use super::timebase::FrameNumber;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueuedVideoFrame<T> {
    pub frame: FrameNumber,
    pub payload: T,
}

#[derive(Debug, Clone)]
pub struct VideoFrameQueue<T> {
    frames: VecDeque<QueuedVideoFrame<T>>,
    capacity: usize,
}

impl<T> VideoFrameQueue<T> {
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
        self.frames.push_back(QueuedVideoFrame { frame, payload });
        while self.frames.len() > self.capacity {
            self.frames.pop_front();
        }
    }
}

impl<T: Clone> VideoFrameQueue<T> {
    pub fn frame_for_program_clock(
        &mut self,
        master_frame: FrameNumber,
    ) -> Option<QueuedVideoFrame<T>> {
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
    fn decoder_queue_returns_latest_ready_frame_for_program_clock() {
        let mut queue = VideoFrameQueue::new(8);
        queue.push_decoded(FrameNumber(100), "f100");
        queue.push_decoded(FrameNumber(101), "f101");
        queue.push_decoded(FrameNumber(102), "f102");

        assert_eq!(
            queue.frame_for_program_clock(FrameNumber(101)),
            Some(QueuedVideoFrame {
                frame: FrameNumber(101),
                payload: "f101"
            })
        );
        assert_eq!(
            queue.frame_for_program_clock(FrameNumber(103)),
            Some(QueuedVideoFrame {
                frame: FrameNumber(102),
                payload: "f102"
            })
        );
    }

    #[test]
    fn decoder_queue_does_not_present_future_video() {
        let mut queue = VideoFrameQueue::new(8);
        queue.push_decoded(FrameNumber(100), "f100");

        assert_eq!(queue.frame_for_program_clock(FrameNumber(99)), None);
    }

    #[test]
    fn decoder_queue_exposes_buffer_span_without_owning_clock() {
        let mut queue = VideoFrameQueue::new(8);
        assert_eq!(queue.oldest_frame(), None);
        assert_eq!(queue.newest_frame(), None);

        queue.push_decoded(FrameNumber(100), "f100");
        queue.push_decoded(FrameNumber(101), "f101");

        assert_eq!(queue.oldest_frame(), Some(FrameNumber(100)));
        assert_eq!(queue.newest_frame(), Some(FrameNumber(101)));
    }

    #[test]
    fn decoder_queue_discards_skipped_frames_on_master_jump() {
        // Same failure mode as audio: one present after a jump cannot recover
        // intermediate frames — they are popped and gone (visual hitch/skip).
        let mut queue = VideoFrameQueue::new(8);
        for f in 100..=104 {
            queue.push_decoded(FrameNumber(f), f);
        }
        let got = queue
            .frame_for_program_clock(FrameNumber(104))
            .expect("latest");
        assert_eq!(got.payload, 104);
        assert_eq!(queue.len(), 1);
        assert_eq!(queue.oldest_frame(), Some(FrameNumber(104)));
    }
}
