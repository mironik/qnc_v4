//! Project form — Lego composition.
//!
//! **Board** ([`empty_story_layout`]): column shell + body slots.
//! **Components:** [`project_list`] | `setting_panel` ([`settings`]).
//! **Orchestrator:** [`screen`] — calls components, maps actions, host I/O.

mod ai;
mod create;
mod empty_story_layout;
mod layout;
mod project_list;
mod screen;
mod settings;
mod template_picker;

pub use screen::{ProjectAction, ProjectScreen};
