//! Screen **composition** — orchestrator chooses which shared blocks are on.
//!
//! Screens paint via `qnc_ui` / `qnc_form` / `editorial::*`; they do not invent
//! which tabs, docks, or right panels exist. Resolve once per frame in `app.rs`
//! (or look up the same table from a screen for head flags).

use crate::editorial::media_pool::MediaPoolHeadInput;
use crate::editorial::types::LibraryTab;
use crate::qnc_media_card::MediaCardFeatures;

/// Editorial form role — one screen impl, different composition attributes.
/// Story and Media Assist share UI components; only flags/layers differ.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum EditorialRole {
    #[default]
    Story,
    MediaAssist,
}

impl EditorialRole {
    pub fn workflow(self) -> WorkflowScreen {
        match self {
            Self::Story => WorkflowScreen::Story,
            Self::MediaAssist => WorkflowScreen::MediaAssist,
        }
    }

    pub fn composition(self) -> ScreenComposition {
        ScreenComposition::resolve(self.workflow())
    }

    pub fn head(self) -> HeadFeatures {
        self.composition().head
    }

    pub fn card_features(self) -> MediaCardFeatures {
        match self {
            Self::Story => MediaCardFeatures::STORY,
            Self::MediaAssist => MediaCardFeatures::MEDIA_ASSIST,
        }
    }

    pub fn idle_status(self) -> &'static str {
        match self {
            Self::Story => "Story idle",
            Self::MediaAssist => "Media Assist idle",
        }
    }
}

/// Which top-level shell layout to use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShellKind {
    /// Host connect gate (no workspace shell).
    HostGate,
    /// Project list | template settings (`project_workspace`).
    ProjectWorkspace,
    /// Preview | chrome | body + right panel (`editorial_shell`).
    Editorial,
}

/// Right column of the editorial / project shell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RightPanelKind {
    /// Project template settings (form kit).
    TemplateSettings,
    /// Ingest clip selection grid.
    ClipGrid,
    /// Story segment / wrap / markers.
    SegmentPanel,
    /// No right domain panel (reserved).
    None,
}

/// Media-pool head chrome (All / Virtual / Segment + transport).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HeadFeatures {
    pub show_segment_tab: bool,
    pub show_cover_tab: bool,
    pub show_export_xml: bool,
    pub show_quick_cover: bool,
}

impl HeadFeatures {
    pub const STORY: Self = Self {
        show_segment_tab: true,
        show_cover_tab: true,
        show_export_xml: true,
        show_quick_cover: true,
    };

    pub const MEDIA_ASSIST: Self = Self {
        show_segment_tab: false,
        show_cover_tab: true,
        show_export_xml: false,
        show_quick_cover: false,
    };

    pub const INGEST: Self = Self {
        show_segment_tab: false,
        show_cover_tab: false,
        show_export_xml: false,
        show_quick_cover: false,
    };

    /// Project has no media-pool head.
    pub const NONE: Self = Self {
        show_segment_tab: false,
        show_cover_tab: false,
        show_export_xml: false,
        show_quick_cover: false,
    };

    pub fn to_pool_head(self, library_tab: LibraryTab, playing: bool) -> MediaPoolHeadInput {
        MediaPoolHeadInput {
            library_tab,
            playing,
            show_segment_tab: self.show_segment_tab,
            show_cover_tab: self.show_cover_tab,
            show_export_xml: self.show_export_xml,
            show_quick_cover: self.show_quick_cover,
        }
    }
}

/// Bottom source dock (timeline).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DockFeatures {
    pub show: bool,
    pub show_header: bool,
    pub show_story_actions: bool,
    pub show_ingest_actions: bool,
}

impl DockFeatures {
    pub const HIDDEN: Self = Self {
        show: false,
        show_header: false,
        show_story_actions: false,
        show_ingest_actions: false,
    };

    pub const STORY: Self = Self {
        show: true,
        show_header: true,
        show_story_actions: true,
        show_ingest_actions: false,
    };

    pub const MEDIA_ASSIST: Self = Self {
        show: true,
        show_header: true,
        show_story_actions: true,
        show_ingest_actions: false,
    };

    pub const INGEST: Self = Self {
        show: true,
        show_header: true,
        show_story_actions: false,
        show_ingest_actions: true,
    };
}

/// Full block composition for one screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScreenComposition {
    pub shell: ShellKind,
    pub head: HeadFeatures,
    pub right: RightPanelKind,
    pub dock: DockFeatures,
    /// Use shared `qnc_form` element kit (Project settings).
    pub use_form_kit: bool,
}

/// Workflow screens the orchestrator can show.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkflowScreen {
    HostGate,
    Project,
    Ingest,
    MediaAssist,
    Story,
    Unsupported,
}

impl ScreenComposition {
    pub fn resolve(screen: WorkflowScreen) -> Self {
        match screen {
            WorkflowScreen::HostGate => Self {
                shell: ShellKind::HostGate,
                head: HeadFeatures::NONE,
                right: RightPanelKind::None,
                dock: DockFeatures::HIDDEN,
                use_form_kit: false,
            },
            WorkflowScreen::Project => Self {
                shell: ShellKind::ProjectWorkspace,
                head: HeadFeatures::NONE,
                right: RightPanelKind::TemplateSettings,
                dock: DockFeatures::HIDDEN,
                use_form_kit: true,
            },
            WorkflowScreen::Ingest => Self {
                shell: ShellKind::Editorial,
                head: HeadFeatures::INGEST,
                right: RightPanelKind::ClipGrid,
                dock: DockFeatures::INGEST,
                use_form_kit: false,
            },
            WorkflowScreen::MediaAssist => Self {
                shell: ShellKind::Editorial,
                head: HeadFeatures::MEDIA_ASSIST,
                // No Segment tab → no segment right panel (composition matches chrome).
                right: RightPanelKind::None,
                dock: DockFeatures::MEDIA_ASSIST,
                use_form_kit: false,
            },
            WorkflowScreen::Story => Self {
                shell: ShellKind::Editorial,
                head: HeadFeatures::STORY,
                right: RightPanelKind::SegmentPanel,
                dock: DockFeatures::STORY,
                use_form_kit: false,
            },
            WorkflowScreen::Unsupported => Self {
                shell: ShellKind::Editorial,
                head: HeadFeatures::NONE,
                right: RightPanelKind::None,
                dock: DockFeatures::HIDDEN,
                use_form_kit: false,
            },
        }
    }
}
