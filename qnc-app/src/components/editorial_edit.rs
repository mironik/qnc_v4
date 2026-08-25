use serde_json::{json, Value};

use crate::component_runtime::{ComponentBackendCommand, ComponentBackendEvent};

const COMPONENT_ID: &str = "editorial.edit";
const PORT_MUTATION: &str = "mutation";
const REQUEST_SEP: char = '\u{1f}';

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EditorialEditKind {
    MarkPartIn,
    MarkPartOut,
    SaveVirtualShot,
    CreatePartFromMarks,
    Commit,
    DeletePart,
    ReorderPart,
    CreateMarker,
    DeleteMarker,
    MoveMarker,
    SelectMarkerSlot,
    SelectCover,
    CreateCover,
    DeleteCover,
    UndoObject,
    RedoObject,
}

impl EditorialEditKind {
    fn operation_id(self) -> &'static str {
        match self {
            Self::MarkPartIn => "part.mark_in",
            Self::MarkPartOut => "part.mark_out",
            Self::SaveVirtualShot => "virtual_shot.create",
            Self::CreatePartFromMarks => "part.create_from_marks",
            Self::Commit => "story.commit",
            Self::DeletePart => "part.delete",
            Self::ReorderPart => "part.reorder",
            Self::CreateMarker => "marker.create",
            Self::DeleteMarker => "marker.delete",
            Self::MoveMarker => "marker.move",
            Self::SelectMarkerSlot => "marker_slot.select",
            Self::SelectCover => "cover.select",
            Self::CreateCover => "cover.create",
            Self::DeleteCover => "cover.delete",
            Self::UndoObject => "object.undo",
            Self::RedoObject => "object.redo",
        }
    }

    fn from_operation_id(operation_id: &str) -> Option<Self> {
        Some(match operation_id {
            "part.mark_in" => Self::MarkPartIn,
            "part.mark_out" => Self::MarkPartOut,
            "virtual_shot.create" => Self::SaveVirtualShot,
            "part.create_from_marks" => Self::CreatePartFromMarks,
            "story.commit" => Self::Commit,
            "part.delete" => Self::DeletePart,
            "part.reorder" => Self::ReorderPart,
            "marker.create" => Self::CreateMarker,
            "marker.delete" => Self::DeleteMarker,
            "marker.move" => Self::MoveMarker,
            "marker_slot.select" => Self::SelectMarkerSlot,
            "cover.select" => Self::SelectCover,
            "cover.create" => Self::CreateCover,
            "cover.delete" => Self::DeleteCover,
            "object.undo" => Self::UndoObject,
            "object.redo" => Self::RedoObject,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone)]
pub(crate) struct EditorialEditData {
    pub instance_id: String,
    pub project_id: String,
    pub kind: EditorialEditKind,
    pub detail: String,
    pub state: Value,
}

pub(crate) struct EditorialEditComponent;

impl EditorialEditComponent {
    pub fn mark_part_in(
        instance_id: &str,
        request_id: u64,
        project_id: &str,
        part_id: &str,
        local_frame: i64,
    ) -> ComponentBackendCommand {
        Self::post(
            instance_id,
            request_id,
            project_id,
            EditorialEditKind::MarkPartIn,
            part_id,
            "/api/story/part/mark_in",
            json!({
                "project_id": project_id,
                "part_id": part_id,
                "local_frame": local_frame.max(0),
            }),
        )
    }

    pub fn mark_part_out(
        instance_id: &str,
        request_id: u64,
        project_id: &str,
        part_id: &str,
        local_frame: i64,
    ) -> ComponentBackendCommand {
        Self::post(
            instance_id,
            request_id,
            project_id,
            EditorialEditKind::MarkPartOut,
            part_id,
            "/api/story/part/mark_out",
            json!({
                "project_id": project_id,
                "part_id": part_id,
                "local_frame": local_frame.max(0),
            }),
        )
    }

    pub fn save_virtual_shot(
        instance_id: &str,
        request_id: u64,
        project_id: &str,
        clip_id: &str,
        in_frame: i64,
        out_frame: i64,
    ) -> ComponentBackendCommand {
        Self::post(
            instance_id,
            request_id,
            project_id,
            EditorialEditKind::SaveVirtualShot,
            clip_id,
            "/api/story/virtual-shot",
            json!({
                "project_id": project_id,
                "clip_id": clip_id,
                "in_frame": in_frame.max(0),
                "out_frame": out_frame.max(in_frame + 1),
            }),
        )
    }

    pub fn create_part_from_marks(
        instance_id: &str,
        request_id: u64,
        project_id: &str,
        segment_kind: &str,
        clip_id: &str,
        in_frame: i64,
        out_frame: i64,
    ) -> ComponentBackendCommand {
        Self::post(
            instance_id,
            request_id,
            project_id,
            EditorialEditKind::CreatePartFromMarks,
            segment_kind,
            "/api/story/part/create",
            json!({
                "project_id": project_id,
                "kind": segment_kind,
                "clip_id": clip_id,
                "in_frame": in_frame.max(0),
                "out_frame": out_frame.max(in_frame + 1),
            }),
        )
    }

    pub fn commit(instance_id: &str, request_id: u64, project_id: &str) -> ComponentBackendCommand {
        Self::post(
            instance_id,
            request_id,
            project_id,
            EditorialEditKind::Commit,
            "",
            "/api/story/commit",
            json!({ "project_id": project_id }),
        )
    }

    pub fn delete_part(
        instance_id: &str,
        request_id: u64,
        project_id: &str,
        part_id: &str,
    ) -> ComponentBackendCommand {
        Self::post(
            instance_id,
            request_id,
            project_id,
            EditorialEditKind::DeletePart,
            part_id,
            "/api/story/part/delete",
            json!({ "project_id": project_id, "part_id": part_id }),
        )
    }

    pub fn reorder_part(
        instance_id: &str,
        request_id: u64,
        project_id: &str,
        part_id: &str,
        direction: &str,
    ) -> ComponentBackendCommand {
        Self::post(
            instance_id,
            request_id,
            project_id,
            EditorialEditKind::ReorderPart,
            part_id,
            "/api/story/part/reorder",
            json!({
                "project_id": project_id,
                "part_id": part_id,
                "direction": direction,
            }),
        )
    }

    pub fn create_marker(
        instance_id: &str,
        request_id: u64,
        project_id: &str,
        timeline_frame: i64,
        part_id: &str,
    ) -> ComponentBackendCommand {
        Self::post(
            instance_id,
            request_id,
            project_id,
            EditorialEditKind::CreateMarker,
            part_id,
            "/api/story/marker/create",
            json!({
                "project_id": project_id,
                "timeline_frame": timeline_frame.max(0),
                "part_id": part_id,
            }),
        )
    }

    pub fn delete_marker(
        instance_id: &str,
        request_id: u64,
        project_id: &str,
        marker_id: &str,
    ) -> ComponentBackendCommand {
        Self::post(
            instance_id,
            request_id,
            project_id,
            EditorialEditKind::DeleteMarker,
            marker_id,
            "/api/story/marker/delete",
            json!({ "project_id": project_id, "marker_id": marker_id }),
        )
    }

    pub fn select_marker_slot(
        instance_id: &str,
        request_id: u64,
        project_id: &str,
        slot_id: &str,
    ) -> ComponentBackendCommand {
        Self::post(
            instance_id,
            request_id,
            project_id,
            EditorialEditKind::SelectMarkerSlot,
            slot_id,
            "/api/story/marker_slot/select",
            json!({ "project_id": project_id, "slot_id": slot_id }),
        )
    }

    pub fn select_cover(
        instance_id: &str,
        request_id: u64,
        project_id: &str,
        cover_id: &str,
    ) -> ComponentBackendCommand {
        Self::post(
            instance_id,
            request_id,
            project_id,
            EditorialEditKind::SelectCover,
            cover_id,
            "/api/story/cover/select",
            json!({ "project_id": project_id, "cover_id": cover_id }),
        )
    }

    #[allow(dead_code)]
    pub fn create_cover(
        instance_id: &str,
        request_id: u64,
        project_id: &str,
        slot_id: &str,
        clip_id: Option<&str>,
        virtual_shot_id: Option<&str>,
    ) -> ComponentBackendCommand {
        Self::post(
            instance_id,
            request_id,
            project_id,
            EditorialEditKind::CreateCover,
            slot_id,
            "/api/story/cover/create",
            json!({
                "project_id": project_id,
                "slot_id": slot_id,
                "clip_id": clip_id,
                "virtual_shot_id": virtual_shot_id,
            }),
        )
    }

    pub fn create_cover_from_source(
        instance_id: &str,
        request_id: u64,
        project_id: &str,
        slot_id: &str,
        clip_id: &str,
        in_frame: i64,
        out_frame: i64,
    ) -> ComponentBackendCommand {
        Self::post(
            instance_id,
            request_id,
            project_id,
            EditorialEditKind::CreateCover,
            slot_id,
            "/api/story/cover/create",
            json!({
                "project_id": project_id,
                "slot_id": slot_id,
                "clip_id": clip_id,
                "in_frame": in_frame.max(0),
                "out_frame": out_frame.max(in_frame + 1),
            }),
        )
    }

    pub fn delete_cover(
        instance_id: &str,
        request_id: u64,
        project_id: &str,
        cover_id: &str,
    ) -> ComponentBackendCommand {
        Self::post(
            instance_id,
            request_id,
            project_id,
            EditorialEditKind::DeleteCover,
            cover_id,
            "/api/story/cover/delete",
            json!({ "project_id": project_id, "cover_id": cover_id }),
        )
    }

    pub fn undo_object(
        instance_id: &str,
        request_id: u64,
        project_id: &str,
        object_type: &str,
        object_id: &str,
    ) -> ComponentBackendCommand {
        Self::post(
            instance_id,
            request_id,
            project_id,
            EditorialEditKind::UndoObject,
            &object_key(object_type, object_id),
            "/api/story/object/undo",
            json!({
                "project_id": project_id,
                "object_type": object_type,
                "object_id": object_id,
            }),
        )
    }

    pub fn redo_object(
        instance_id: &str,
        request_id: u64,
        project_id: &str,
        object_type: &str,
        object_id: &str,
    ) -> ComponentBackendCommand {
        Self::post(
            instance_id,
            request_id,
            project_id,
            EditorialEditKind::RedoObject,
            &object_key(object_type, object_id),
            "/api/story/object/redo",
            json!({
                "project_id": project_id,
                "object_type": object_type,
                "object_id": object_id,
            }),
        )
    }

    pub fn accepts_event(event: &ComponentBackendEvent) -> bool {
        event.component_id == COMPONENT_ID
            && event.port_id == PORT_MUTATION
            && EditorialEditKind::from_operation_id(&event.operation_id).is_some()
    }

    pub fn into_data(
        event: ComponentBackendEvent,
    ) -> Option<(
        String,
        String,
        EditorialEditKind,
        String,
        Result<EditorialEditData, String>,
    )> {
        if !Self::accepts_event(&event) {
            return None;
        }
        let kind = EditorialEditKind::from_operation_id(&event.operation_id)?;
        let (instance_id, project_id, _request_id, detail) = split_request_key(&event.request_key)
            .unwrap_or_else(|| (String::new(), event.request_key.clone(), 0, String::new()));
        let result = event.result.map(|state| EditorialEditData {
            instance_id: instance_id.clone(),
            project_id: project_id.clone(),
            kind,
            detail: detail.clone(),
            state,
        });
        Some((instance_id, project_id, kind, detail, result))
    }

    fn post(
        instance_id: &str,
        request_id: u64,
        project_id: &str,
        kind: EditorialEditKind,
        detail: &str,
        path: &str,
        payload: Value,
    ) -> ComponentBackendCommand {
        ComponentBackendCommand::post(
            COMPONENT_ID,
            PORT_MUTATION,
            kind.operation_id(),
            request_key(instance_id, project_id, request_id, detail),
            path,
            payload,
        )
    }
}

fn request_key(instance_id: &str, project_id: &str, request_id: u64, detail: &str) -> String {
    format!("{instance_id}{REQUEST_SEP}{project_id}{REQUEST_SEP}{request_id}{REQUEST_SEP}{detail}")
}

fn object_key(object_type: &str, object_id: &str) -> String {
    format!("{}:{}", object_type.trim(), object_id.trim())
}

fn split_request_key(value: &str) -> Option<(String, String, u64, String)> {
    let mut parts = value.splitn(4, REQUEST_SEP);
    let instance_id = parts.next()?.to_string();
    let project_id = parts.next()?.to_string();
    let request_id = parts.next()?.parse().ok()?;
    let detail = parts.next().unwrap_or("").to_string();
    Some((instance_id, project_id, request_id, detail))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::HostRequestMethod;

    #[test]
    fn edit_command_request_key_is_unique_per_request() {
        let a = EditorialEditComponent::delete_part("story", 1, "p1", "part_a");
        let b = EditorialEditComponent::delete_part("story", 2, "p1", "part_a");
        assert_ne!(a.request_key, b.request_key);
        assert_eq!(a.component_id, COMPONENT_ID);
        assert_eq!(a.port_id, PORT_MUTATION);
        assert_eq!(a.method, HostRequestMethod::Post);
    }

    #[test]
    fn create_part_uses_frame_payload_only() {
        let command = EditorialEditComponent::create_part_from_marks(
            "story", 1, "p1", "tonovi", "clip_a", 10, 20,
        );
        let payload = command.payload.expect("payload");
        assert_eq!(payload["in_frame"], 10);
        assert_eq!(payload["out_frame"], 20);
        assert!(payload.get("in_seconds").is_none());
        assert!(payload.get("out_seconds").is_none());
    }
}
