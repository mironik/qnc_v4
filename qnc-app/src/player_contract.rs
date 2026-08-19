//! Neutral app-side player contract.
//!
//! This is the only source identity/types surface the native UI needs in order
//! to send commands to the modular broadcast player. It intentionally does not
//! expose timeline, marker, filmstrip, or legacy player program models.

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub struct FrameNumber(pub i64);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BroadcastSourceKind {
    VideoAndAudio,
    VideoOnly,
    AudioOnly,
}

#[derive(Debug, Clone, PartialEq)]
pub struct BroadcastHostSourceRef {
    pub project_id: String,
    pub virtual_shot_id: String,
    pub clip_id: String,
    pub in_frame: Option<FrameNumber>,
    pub out_frame: Option<FrameNumber>,
    pub duration_frames: FrameNumber,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BroadcastHostSourceError {
    pub message: String,
}

impl BroadcastHostSourceError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl std::fmt::Display for BroadcastHostSourceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for BroadcastHostSourceError {}

impl BroadcastHostSourceRef {
    pub fn from_frame_fields(
        project_id: impl Into<String>,
        shot_id: impl Into<String>,
        root_shot_id: impl Into<String>,
        clip_id: impl Into<String>,
        in_frame: Option<FrameNumber>,
        out_frame: Option<FrameNumber>,
        duration_frames: FrameNumber,
    ) -> Result<Self, BroadcastHostSourceError> {
        let project_id = project_id.into();
        let shot_id = shot_id.into();
        let root_shot_id = root_shot_id.into();
        let clip_id = clip_id.into();
        let virtual_shot_id =
            first_non_empty([shot_id.as_str(), root_shot_id.as_str(), clip_id.as_str()])
                .ok_or_else(|| BroadcastHostSourceError::new("source is missing id"))?;

        if project_id.trim().is_empty() {
            return Err(BroadcastHostSourceError::new(
                "source is missing project id",
            ));
        }
        if clip_id.trim().is_empty() {
            return Err(BroadcastHostSourceError::new("source is missing clip id"));
        }
        if duration_frames.0 <= 0 {
            return Err(BroadcastHostSourceError::new(
                "source duration_frames must be greater than zero",
            ));
        }
        let clamp = |frame: FrameNumber| FrameNumber(frame.0.clamp(0, duration_frames.0));

        Ok(Self {
            project_id,
            virtual_shot_id: virtual_shot_id.to_string(),
            clip_id,
            in_frame: in_frame.map(clamp),
            out_frame: out_frame.map(clamp),
            duration_frames,
        })
    }
}

fn first_non_empty<'a>(values: impl IntoIterator<Item = &'a str>) -> Option<&'a str> {
    values
        .into_iter()
        .map(str::trim)
        .find(|value| !value.is_empty())
}
