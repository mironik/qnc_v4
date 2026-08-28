use std::time::{Duration, Instant};

use qnc_service_contracts::PreviewHiResInputResponse;

use crate::component_runtime::ComponentBackendCommand;

use super::HiResRenderTransportComponent;

const FEEDBACK_SHORT: Duration = Duration::from_secs(5);

#[derive(Debug, Default)]
pub(crate) struct HiResPreviewProcedureState {
    pending: bool,
    feedback_until: Option<Instant>,
}

#[derive(Debug)]
pub(crate) struct HiResPreviewStart {
    pub command: ComponentBackendCommand,
    pub status: String,
}

pub(crate) struct HiResPreviewProcedureComponent;

impl HiResPreviewProcedureState {
    #[cfg(test)]
    pub(crate) fn pending(&self) -> bool {
        self.pending
    }
}

impl HiResPreviewProcedureComponent {
    pub(crate) fn button_active(state: &mut HiResPreviewProcedureState, now: Instant) -> bool {
        if state.pending {
            return true;
        }
        let Some(until) = state.feedback_until else {
            return false;
        };
        if now < until {
            true
        } else {
            state.feedback_until = None;
            false
        }
    }

    pub(crate) fn start(
        state: &mut HiResPreviewProcedureState,
        instance_id: &str,
        request_id: u64,
        project_id: &str,
        now: Instant,
    ) -> Result<HiResPreviewStart, String> {
        if project_id.trim().is_empty() {
            return Err("Nema otvorenog projekta".into());
        }
        if state.pending {
            return Err("Preview HI-res · flat playlist request je već poslan...".into());
        }
        state.pending = true;
        state.feedback_until = Some(now + FEEDBACK_SHORT);
        Ok(HiResPreviewStart {
            command: HiResRenderTransportComponent::submit_preview(
                instance_id,
                request_id,
                project_id,
            ),
            status: "Preview HI-res · gradim export flat playlistu...".into(),
        })
    }

    pub(crate) fn apply_submit(
        state: &mut HiResPreviewProcedureState,
        active_project_id: &str,
        project_id: &str,
        response: PreviewHiResInputResponse,
    ) -> Option<String> {
        if active_project_id != project_id {
            return None;
        }
        state.pending = false;
        state.feedback_until = Some(Instant::now() + FEEDBACK_SHORT);
        Some(format!(
            "Preview HI-res flat playlist · {} itema",
            response.items.len()
        ))
    }

    pub(crate) fn set_error(
        state: &mut HiResPreviewProcedureState,
        active_project_id: &str,
        project_id: &str,
        error: impl Into<String>,
        now: Instant,
    ) -> Option<String> {
        if active_project_id != project_id {
            return None;
        }
        state.pending = false;
        state.feedback_until = Some(now + FEEDBACK_SHORT);
        Some(format!("Preview HI-res: {}", error.into()))
    }
}

#[cfg(test)]
mod tests {
    use qnc_service_contracts::FrameTimebase;

    use super::*;

    fn input_response() -> PreviewHiResInputResponse {
        PreviewHiResInputResponse {
            project_id: "p1".into(),
            preview_id: "preview_1".into(),
            timeline_timebase: FrameTimebase {
                fps_num: 50,
                fps_den: 1,
            },
            duration_frames: 100,
            items: Vec::new(),
            message: None,
        }
    }

    #[test]
    fn start_builds_preview_flat_playlist_command() {
        let mut state = HiResPreviewProcedureState::default();

        let start =
            HiResPreviewProcedureComponent::start(&mut state, "story", 7, "p1", Instant::now())
                .unwrap();

        assert!(state.pending());
        assert_eq!(start.command.component_id, "hires.render_transport");
        assert_eq!(start.command.operation_id, "preview_hires_input.build");
        assert_eq!(start.command.path, "/api/preview/hires/input/build");
        assert!(start.status.contains("flat playlist"));
    }

    #[test]
    fn submit_response_completes_without_worker_polling() {
        let mut state = HiResPreviewProcedureState::default();
        state.pending = true;

        let message =
            HiResPreviewProcedureComponent::apply_submit(&mut state, "p1", "p1", input_response())
                .unwrap();

        assert!(!state.pending());
        assert!(message.contains("flat playlist"));
    }
}
