//! Media Assist — same editorial form as Story (`story::StoryScreen`).
//!
//! Only composition attributes differ (`EditorialRole::MediaAssist`).
//! There is no parallel UI tree: components are shared; the form configures them.

pub use crate::story::StoryScreen as MediaAssistScreen;
