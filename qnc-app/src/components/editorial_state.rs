use serde_json::Value;

use crate::api::{self, EditorialPlaylist, TimelineModel};
use crate::component_runtime::{ComponentBackendCommand, ComponentBackendEvent};

const COMPONENT_ID: &str = "editorial.state";
const OP_LOAD: &str = "load";
const PORT_STORY_STATE: &str = "story.state";
const PORT_TIMELINE_MODEL: &str = "timeline.model";
const PORT_PLAYLIST: &str = "playlist";
const REQUEST_SEP: char = '\u{1f}';

#[derive(Debug, Clone)]
pub(crate) enum EditorialStateData {
    StoryState {
        instance_id: String,
        project_id: String,
        state: Value,
    },
    TimelineModel {
        instance_id: String,
        project_id: String,
        timeline: TimelineModel,
    },
    Playlist {
        instance_id: String,
        project_id: String,
        playlist: EditorialPlaylist,
    },
}

pub(crate) struct EditorialStateComponent;

impl EditorialStateComponent {
    pub fn load_story_state(instance_id: &str, project_id: &str) -> ComponentBackendCommand {
        ComponentBackendCommand::get(
            COMPONENT_ID,
            PORT_STORY_STATE,
            OP_LOAD,
            request_key(instance_id, project_id),
            format!(
                "/api/story/state?project_id={}",
                api::encode_query_value(project_id)
            ),
        )
    }

    pub fn load_timeline_model(instance_id: &str, project_id: &str) -> ComponentBackendCommand {
        ComponentBackendCommand::get(
            COMPONENT_ID,
            PORT_TIMELINE_MODEL,
            OP_LOAD,
            request_key(instance_id, project_id),
            format!(
                "/api/story/timeline-model?project_id={}",
                api::encode_query_value(project_id)
            ),
        )
    }

    pub fn load_playlist(instance_id: &str, project_id: &str) -> ComponentBackendCommand {
        ComponentBackendCommand::get(
            COMPONENT_ID,
            PORT_PLAYLIST,
            OP_LOAD,
            request_key(instance_id, project_id),
            format!(
                "/api/story/playlist?project_id={}",
                api::encode_query_value(project_id)
            ),
        )
    }

    pub fn accepts_event(event: &ComponentBackendEvent) -> bool {
        event.component_id == COMPONENT_ID
            && event.operation_id == OP_LOAD
            && matches!(
                event.port_id.as_str(),
                PORT_STORY_STATE | PORT_TIMELINE_MODEL | PORT_PLAYLIST
            )
    }

    pub fn into_data(
        event: ComponentBackendEvent,
    ) -> Option<(String, String, Result<EditorialStateData, String>)> {
        if !Self::accepts_event(&event) {
            return None;
        }
        let (instance_id, project_id) = split_request_key(&event.request_key)
            .unwrap_or_else(|| (String::new(), event.request_key.clone()));
        let port_id = event.port_id.clone();
        let result = event
            .result
            .and_then(|value| parse_data(&port_id, &instance_id, &project_id, value));
        Some((instance_id, project_id, result))
    }
}

fn parse_data(
    port_id: &str,
    instance_id: &str,
    project_id: &str,
    value: Value,
) -> Result<EditorialStateData, String> {
    match port_id {
        PORT_STORY_STATE => Ok(EditorialStateData::StoryState {
            instance_id: instance_id.to_string(),
            project_id: project_id.to_string(),
            state: value,
        }),
        PORT_TIMELINE_MODEL => {
            let timeline: TimelineModel =
                serde_json::from_value(value).map_err(|e| format!("timeline model: {e}"))?;
            Ok(EditorialStateData::TimelineModel {
                instance_id: instance_id.to_string(),
                project_id: project_id.to_string(),
                timeline,
            })
        }
        PORT_PLAYLIST => {
            let playlist: EditorialPlaylist =
                serde_json::from_value(value).map_err(|e| format!("playlist: {e}"))?;
            Ok(EditorialStateData::Playlist {
                instance_id: instance_id.to_string(),
                project_id: project_id.to_string(),
                playlist,
            })
        }
        _ => Err(format!("unknown editorial state port: {port_id}")),
    }
}

fn request_key(instance_id: &str, project_id: &str) -> String {
    format!("{instance_id}{REQUEST_SEP}{project_id}")
}

fn split_request_key(value: &str) -> Option<(String, String)> {
    let (instance_id, project_id) = value.split_once(REQUEST_SEP)?;
    Some((instance_id.to_string(), project_id.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::HostRequestMethod;

    #[test]
    fn state_projection_commands_use_independent_ports_for_latest_wins() {
        let commands = [
            EditorialStateComponent::load_story_state("story", "p1"),
            EditorialStateComponent::load_timeline_model("story", "p1"),
            EditorialStateComponent::load_playlist("story", "p1"),
        ];
        let ports: std::collections::BTreeSet<_> = commands
            .iter()
            .map(|command| command.port_id.as_str())
            .collect();
        assert_eq!(commands.len(), 3);
        assert_eq!(ports.len(), 3);
        assert!(commands.iter().all(|command| {
            command.component_id == COMPONENT_ID
                && command.operation_id == OP_LOAD
                && command.request_key == request_key("story", "p1")
                && command.method == HostRequestMethod::Get
        }));
    }

    #[test]
    fn request_key_keeps_instance_and_project_separate() {
        let key = request_key("media_assist", "project-1");
        assert_eq!(
            split_request_key(&key),
            Some(("media_assist".to_string(), "project-1".to_string()))
        );
    }
}
