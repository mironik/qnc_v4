//! Frame/timebase primitives for broadcast playback.
//!
//! This module intentionally uses integer frame numbers and rational frame
//! rates. UI seconds are projections only; they are not the playback truth.

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct FrameNumber(pub i64);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Timebase {
    pub num: u32,
    pub den: u32,
    pub drop_frame: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TimebaseParseError {
    pub message: String,
}

impl TimebaseParseError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl Timebase {
    pub const DEFAULT_SOURCE_TIMEBASE: Self = Self {
        num: 50,
        den: 1,
        drop_frame: false,
    };

    pub fn from_source_fps(fps: f64) -> Self {
        Self::try_from_source_fps(fps).unwrap_or(Self::DEFAULT_SOURCE_TIMEBASE)
    }

    pub fn try_from_source_fps(fps: f64) -> Result<Self, TimebaseParseError> {
        if approx(fps, 23.976) {
            Self::from_source_rate(24_000, 1_001)
        } else if approx(fps, 29.97) {
            Self::from_source_rate(30_000, 1_001)
        } else if approx(fps, 59.94) {
            Self::from_source_rate(60_000, 1_001)
        } else if fps.is_finite() && fps > 0.0 {
            Self::from_source_rate(fps.round().max(1.0) as u32, 1)
        } else {
            Err(TimebaseParseError::new("source FPS must be finite and > 0"))
        }
    }

    pub fn from_source_rate(num: u32, den: u32) -> Result<Self, TimebaseParseError> {
        if num == 0 || den == 0 {
            return Err(TimebaseParseError::new(
                "source frame rate must be non-zero",
            ));
        }
        let divisor = gcd(num, den);
        let num = num / divisor;
        let den = den / divisor;
        Ok(Self {
            num,
            den,
            drop_frame: matches!((num, den), (30_000, 1_001) | (60_000, 1_001)),
        })
    }

    pub fn parse_ffprobe_rate(value: &str) -> Result<Self, TimebaseParseError> {
        let value = value.trim();
        let Some((num, den)) = value.split_once('/') else {
            return Err(TimebaseParseError::new(format!(
                "invalid ffprobe frame rate '{value}'"
            )));
        };
        let num = num
            .trim()
            .parse::<u32>()
            .map_err(|_| TimebaseParseError::new(format!("invalid rate numerator '{value}'")))?;
        let den = den
            .trim()
            .parse::<u32>()
            .map_err(|_| TimebaseParseError::new(format!("invalid rate denominator '{value}'")))?;
        Self::from_source_rate(num, den)
    }

    pub fn fps(self) -> f64 {
        self.num as f64 / self.den as f64
    }

    pub fn frame_duration_sec(self) -> f64 {
        self.den as f64 / self.num as f64
    }

    pub fn frame_at_seconds(self, seconds: f64) -> FrameNumber {
        FrameNumber(((seconds.max(0.0) * self.num as f64) / self.den as f64).round() as i64)
    }

    pub fn frame_floor_at_seconds(self, seconds: f64) -> FrameNumber {
        let frames = (seconds.max(0.0) * self.num as f64) / self.den as f64;
        FrameNumber((frames + 0.000_000_001).floor() as i64)
    }

    pub fn seconds_at_frame(self, frame: FrameNumber) -> f64 {
        frame.0.max(0) as f64 * self.den as f64 / self.num as f64
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameRange {
    pub start: FrameNumber,
    pub end_exclusive: FrameNumber,
}

impl FrameRange {
    pub fn new(start: FrameNumber, end_exclusive: FrameNumber) -> Self {
        Self {
            start,
            end_exclusive: FrameNumber(end_exclusive.0.max(start.0 + 1)),
        }
    }

    pub fn clamp(self, frame: FrameNumber) -> FrameNumber {
        FrameNumber(frame.0.clamp(self.start.0, self.end_exclusive.0 - 1))
    }

    pub fn contains(self, frame: FrameNumber) -> bool {
        frame.0 >= self.start.0 && frame.0 < self.end_exclusive.0
    }
}

fn approx(a: f64, b: f64) -> bool {
    (a - b).abs() < 0.01
}

fn gcd(mut left: u32, mut right: u32) -> u32 {
    while right != 0 {
        let rem = left % right;
        left = right;
        right = rem;
    }
    left.max(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_common_broadcast_rates() {
        assert_eq!(Timebase::from_source_fps(25.0).num, 25);
        assert_eq!(Timebase::from_source_fps(29.97).num, 30_000);
        assert_eq!(Timebase::from_source_fps(29.97).den, 1_001);
        assert!(Timebase::from_source_fps(59.94).drop_frame);
    }

    #[test]
    fn parses_source_rational_rate_without_float_guessing() {
        assert_eq!(
            Timebase::parse_ffprobe_rate("30000/1001").unwrap(),
            Timebase {
                num: 30_000,
                den: 1_001,
                drop_frame: true
            }
        );
        assert_eq!(
            Timebase::parse_ffprobe_rate("50000/1000").unwrap(),
            Timebase {
                num: 50,
                den: 1,
                drop_frame: false
            }
        );
    }

    #[test]
    fn rejects_invalid_source_rational_rate() {
        assert!(Timebase::parse_ffprobe_rate("0/0").is_err());
        assert!(Timebase::parse_ffprobe_rate("N/A").is_err());
    }

    #[test]
    fn converts_seconds_to_integer_frames() {
        let tb = Timebase::from_source_fps(50.0);
        assert_eq!(tb.frame_at_seconds(0.04), FrameNumber(2));
        assert!((tb.seconds_at_frame(FrameNumber(2)) - 0.04).abs() < 0.000001);
    }
}
