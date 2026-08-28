use std::time::{Duration, Instant};

use qnc_service_contracts::ExportHiResSubmitResponse;

use crate::component_runtime::ComponentBackendCommand;

use super::{ExportHiResStatus, HiResRenderTransportComponent};

pub(crate) const HIRES_EXPORT_POLL_INTERVAL: Duration = Duration::from_millis(900);

#[derive(Debug, Clone)]
struct ExportHiResJobWatch {
    job_id: String,
    output_path: String,
    last_poll: Option<Instant>,
    poll_in_flight: bool,
}

#[derive(Debug, Default)]
pub(crate) struct HiResExportProcedureState {
    pending: bool,
    watch: Option<ExportHiResJobWatch>,
}

#[derive(Debug)]
pub(crate) struct HiResExportStart {
    pub command: ComponentBackendCommand,
    pub status: String,
}

pub(crate) struct HiResExportProcedureComponent;

impl HiResExportProcedureState {
    #[cfg(test)]
    pub(crate) fn pending(&self) -> bool {
        self.pending
    }

    pub(crate) fn has_watch(&self) -> bool {
        self.watch.is_some()
    }
}

impl HiResExportProcedureComponent {
    pub(crate) fn button_active(state: &mut HiResExportProcedureState, _now: Instant) -> bool {
        state.pending || state.watch.is_some()
    }

    pub(crate) fn start(
        state: &mut HiResExportProcedureState,
        instance_id: &str,
        request_id: u64,
        project_id: &str,
        _now: Instant,
    ) -> Result<HiResExportStart, String> {
        if project_id.trim().is_empty() {
            return Err("Nema otvorenog projekta".into());
        }
        if state.pending {
            let status = state
                .watch
                .as_ref()
                .map(|watch| format!("Export HI-res već radi · {}", watch.output_path))
                .unwrap_or_else(|| "Export HI-res · worker job se već šalje...".into());
            return Err(status);
        }
        state.pending = true;
        Ok(HiResExportStart {
            command: HiResRenderTransportComponent::submit(instance_id, request_id, project_id),
            status: "Export HI-res · šaljem worker job...".into(),
        })
    }

    pub(crate) fn apply_submit(
        state: &mut HiResExportProcedureState,
        active_project_id: &str,
        project_id: &str,
        response: ExportHiResSubmitResponse,
    ) -> Option<String> {
        if active_project_id != project_id {
            return None;
        }
        let output_path = response.output_path.display().to_string();
        state.pending = true;
        state.watch = Some(ExportHiResJobWatch {
            job_id: response.job_id.clone(),
            output_path: output_path.clone(),
            last_poll: None,
            poll_in_flight: false,
        });
        Some(format!(
            "Export HI-res queued · {} · {}",
            truncate(&response.job_id, 28),
            output_path
        ))
    }

    pub(crate) fn set_submit_error(
        state: &mut HiResExportProcedureState,
        active_project_id: &str,
        project_id: &str,
        error: impl Into<String>,
        _now: Instant,
    ) -> Option<String> {
        if active_project_id != project_id {
            return None;
        }
        state.pending = false;
        state.watch = None;
        Some(format!("Export HI-res: {}", error.into()))
    }

    pub(crate) fn apply_status(
        state: &mut HiResExportProcedureState,
        active_project_id: &str,
        project_id: &str,
        job_status: ExportHiResStatus,
        now: Instant,
    ) -> Option<String> {
        if active_project_id != project_id {
            return None;
        }
        let Some(watch) = state.watch.as_mut() else {
            return None;
        };
        if watch.job_id != job_status.job_id {
            return None;
        }
        watch.poll_in_flight = false;
        watch.last_poll = Some(now);
        match job_status.status.as_str() {
            "done" => {
                let output_path = watch.output_path.clone();
                state.pending = false;
                state.watch = None;
                Some(format!("Export HI-res gotov · {output_path}"))
            }
            "error" => {
                let error = if job_status.error.trim().is_empty() {
                    "worker je završio s greškom".to_string()
                } else {
                    job_status.error
                };
                state.pending = false;
                state.watch = None;
                Some(format!("Export HI-res greška · {error}"))
            }
            "processing" => {
                state.pending = true;
                Some(format!("Export HI-res u tijeku · {}", watch.output_path))
            }
            "queued" => {
                state.pending = true;
                Some(format!("Export HI-res čeka worker · {}", watch.output_path))
            }
            other => {
                state.pending = true;
                Some(format!("Export HI-res {other} · {}", watch.output_path))
            }
        }
    }

    pub(crate) fn set_status_error(
        state: &mut HiResExportProcedureState,
        active_project_id: &str,
        project_id: &str,
        error: impl Into<String>,
    ) -> Option<String> {
        if active_project_id != project_id {
            return None;
        }
        if let Some(watch) = state.watch.as_mut() {
            watch.poll_in_flight = false;
        }
        Some(format!("Export HI-res status: {}", error.into()))
    }

    pub(crate) fn claim_status_poll(
        state: &mut HiResExportProcedureState,
        now: Instant,
    ) -> Option<String> {
        let watch = state.watch.as_mut()?;
        if watch.poll_in_flight {
            return None;
        }
        if watch
            .last_poll
            .map(|last| now.duration_since(last) < HIRES_EXPORT_POLL_INTERVAL)
            .unwrap_or(false)
        {
            return None;
        }
        watch.poll_in_flight = true;
        watch.last_poll = Some(now);
        Some(watch.job_id.clone())
    }

    pub(crate) fn status_command(
        instance_id: &str,
        request_id: u64,
        project_id: &str,
        job_id: &str,
    ) -> ComponentBackendCommand {
        HiResRenderTransportComponent::status(instance_id, request_id, project_id, job_id)
    }
}

fn truncate(value: &str, max: usize) -> String {
    let mut out = String::new();
    for ch in value.chars().take(max) {
        out.push(ch);
    }
    if value.chars().count() > max {
        out.push('…');
    }
    out
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use qnc_service_contracts::{ExportHiResSubmitResponse, ExportJobState, FrameTimebase};

    use super::*;

    #[test]
    fn start_builds_export_submit_command() {
        let mut state = HiResExportProcedureState::default();

        let start =
            HiResExportProcedureComponent::start(&mut state, "story", 7, "p1", Instant::now())
                .unwrap();

        assert!(state.pending());
        assert_eq!(start.command.component_id, "hires.render_transport");
        assert_eq!(start.command.operation_id, "export_hires.submit");
        assert_eq!(start.command.path, "/api/render/hires/submit");
    }

    #[test]
    fn submit_then_status_poll_uses_existing_transport_component() {
        let now = Instant::now();
        let mut state = HiResExportProcedureState::default();
        let response = ExportHiResSubmitResponse {
            project_id: "p1".into(),
            job_id: "job_1".into(),
            export_id: "export_1".into(),
            state: ExportJobState::Queued,
            output_path: PathBuf::from("C:/qnc/out/export.mxf"),
            timeline_timebase: FrameTimebase {
                fps_num: 50,
                fps_den: 1,
            },
            duration_frames: 100,
            message: None,
        };

        let status =
            HiResExportProcedureComponent::apply_submit(&mut state, "p1", "p1", response).unwrap();
        let job_id = HiResExportProcedureComponent::claim_status_poll(&mut state, now).unwrap();
        let command = HiResExportProcedureComponent::status_command("story", 8, "p1", &job_id);

        assert!(status.contains("queued"));
        assert_eq!(job_id, "job_1");
        assert_eq!(command.operation_id, "export_hires.status");
        assert!(command.path.contains("/api/render/hires/status"));
    }

    #[test]
    fn done_status_clears_pending_watch() {
        let now = Instant::now();
        let mut state = HiResExportProcedureState::default();
        let response = ExportHiResSubmitResponse {
            project_id: "p1".into(),
            job_id: "job_1".into(),
            export_id: "export_1".into(),
            state: ExportJobState::Queued,
            output_path: PathBuf::from("C:/qnc/out/export.mxf"),
            timeline_timebase: FrameTimebase {
                fps_num: 50,
                fps_den: 1,
            },
            duration_frames: 100,
            message: None,
        };
        HiResExportProcedureComponent::apply_submit(&mut state, "p1", "p1", response).unwrap();

        let status = HiResExportProcedureComponent::apply_status(
            &mut state,
            "p1",
            "p1",
            ExportHiResStatus {
                job_id: "job_1".into(),
                status: "done".into(),
                error: String::new(),
            },
            now,
        )
        .unwrap();

        assert!(status.contains("gotov"));
        assert!(!state.pending());
        assert!(!state.has_watch());
        assert!(!HiResExportProcedureComponent::button_active(
            &mut state, now
        ));
    }
}
