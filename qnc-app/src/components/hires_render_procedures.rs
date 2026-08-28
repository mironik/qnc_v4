use std::time::{Duration, Instant};

use qnc_service_contracts::{ExportHiResSubmitResponse, PreviewHiResInputResponse};

use crate::component_runtime::ComponentBackendCommand;

use super::{
    ExportHiResStatus, HiResExportProcedureComponent, HiResExportProcedureState,
    HiResPreviewProcedureComponent, HiResPreviewProcedureState, HIRES_EXPORT_POLL_INTERVAL,
};

#[derive(Debug, Default)]
pub(crate) struct HiResRenderProceduresState {
    export: HiResExportProcedureState,
    preview: HiResPreviewProcedureState,
}

pub(crate) struct HiResRenderProceduresComponent;

impl HiResRenderProceduresState {
    #[cfg(test)]
    pub(crate) fn export_pending(&self) -> bool {
        self.export.pending()
    }

    #[cfg(test)]
    pub(crate) fn preview_pending(&self) -> bool {
        self.preview.pending()
    }

    pub(crate) fn export_has_watch(&self) -> bool {
        self.export.has_watch()
    }
}

impl HiResRenderProceduresComponent {
    pub(crate) fn export_poll_interval() -> Duration {
        HIRES_EXPORT_POLL_INTERVAL
    }

    pub(crate) fn export_button_active(
        state: &mut HiResRenderProceduresState,
        now: Instant,
    ) -> bool {
        HiResExportProcedureComponent::button_active(&mut state.export, now)
    }

    pub(crate) fn preview_button_active(
        state: &mut HiResRenderProceduresState,
        now: Instant,
    ) -> bool {
        HiResPreviewProcedureComponent::button_active(&mut state.preview, now)
    }

    pub(crate) fn start_export(
        state: &mut HiResRenderProceduresState,
        instance_id: &str,
        request_id: u64,
        project_id: &str,
        now: Instant,
    ) -> Result<(ComponentBackendCommand, String), String> {
        HiResExportProcedureComponent::start(
            &mut state.export,
            instance_id,
            request_id,
            project_id,
            now,
        )
        .map(|start| (start.command, start.status))
    }

    pub(crate) fn apply_export_submit(
        state: &mut HiResRenderProceduresState,
        active_project_id: &str,
        project_id: &str,
        response: ExportHiResSubmitResponse,
    ) -> Option<String> {
        HiResExportProcedureComponent::apply_submit(
            &mut state.export,
            active_project_id,
            project_id,
            response,
        )
    }

    pub(crate) fn set_export_error(
        state: &mut HiResRenderProceduresState,
        active_project_id: &str,
        project_id: &str,
        error: impl Into<String>,
        now: Instant,
    ) -> Option<String> {
        HiResExportProcedureComponent::set_submit_error(
            &mut state.export,
            active_project_id,
            project_id,
            error,
            now,
        )
    }

    pub(crate) fn apply_export_status(
        state: &mut HiResRenderProceduresState,
        active_project_id: &str,
        project_id: &str,
        status: ExportHiResStatus,
        now: Instant,
    ) -> Option<String> {
        HiResExportProcedureComponent::apply_status(
            &mut state.export,
            active_project_id,
            project_id,
            status,
            now,
        )
    }

    pub(crate) fn set_export_status_error(
        state: &mut HiResRenderProceduresState,
        active_project_id: &str,
        project_id: &str,
        error: impl Into<String>,
    ) -> Option<String> {
        HiResExportProcedureComponent::set_status_error(
            &mut state.export,
            active_project_id,
            project_id,
            error,
        )
    }

    pub(crate) fn claim_export_status_poll(
        state: &mut HiResRenderProceduresState,
        now: Instant,
    ) -> Option<String> {
        HiResExportProcedureComponent::claim_status_poll(&mut state.export, now)
    }

    pub(crate) fn export_status_command(
        instance_id: &str,
        request_id: u64,
        project_id: &str,
        job_id: &str,
    ) -> ComponentBackendCommand {
        HiResExportProcedureComponent::status_command(instance_id, request_id, project_id, job_id)
    }

    pub(crate) fn start_preview(
        state: &mut HiResRenderProceduresState,
        instance_id: &str,
        request_id: u64,
        project_id: &str,
        now: Instant,
    ) -> Result<(ComponentBackendCommand, String), String> {
        HiResPreviewProcedureComponent::start(
            &mut state.preview,
            instance_id,
            request_id,
            project_id,
            now,
        )
        .map(|start| (start.command, start.status))
    }

    pub(crate) fn apply_preview_submit(
        state: &mut HiResRenderProceduresState,
        active_project_id: &str,
        project_id: &str,
        response: PreviewHiResInputResponse,
    ) -> Option<String> {
        HiResPreviewProcedureComponent::apply_submit(
            &mut state.preview,
            active_project_id,
            project_id,
            response,
        )
    }

    pub(crate) fn set_preview_error(
        state: &mut HiResRenderProceduresState,
        active_project_id: &str,
        project_id: &str,
        error: impl Into<String>,
        now: Instant,
    ) -> Option<String> {
        HiResPreviewProcedureComponent::set_error(
            &mut state.preview,
            active_project_id,
            project_id,
            error,
            now,
        )
    }
}
