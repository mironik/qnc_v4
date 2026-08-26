//! Neutral playback media resolver.
//!
//! Forms select clips; this component resolves the concrete playback input
//! through qnc-host media gateway.

use crate::api::HostRequestTimeout;
use crate::component_runtime::{ComponentBackendCommand, ComponentBackendEvent};
use crate::player_contract::BroadcastSourceTimebase;
use qnc_service_contracts::{MediaAccessKind, MediaLocator, MediaResolveResponse};

const COMPONENT_ID: &str = "playback.media.resolver";
const OP_RESOLVE_PLAYBACK_PROXY: &str = "resolve.playback_proxy";
const PORT_MEDIA_RESOLVE: &str = "media.resolve";
const REQUEST_SEP: char = '\u{1f}';

#[derive(Debug, Clone)]
pub(crate) struct PlaybackMediaResolution {
    pub media_input: String,
    pub locator_kind: &'static str,
    pub source_timebase: Option<BroadcastSourceTimebase>,
    pub duration_sec: Option<f64>,
    pub duration_frames: Option<i64>,
    pub has_audio: Option<bool>,
    pub audio_channels: Option<u8>,
}

pub(crate) struct PlaybackMediaResolverComponent;

impl PlaybackMediaResolverComponent {
    pub(crate) fn resolve_playback_proxy(
        instance_id: &str,
        project_id: &str,
        clip_id: &str,
    ) -> ComponentBackendCommand {
        ComponentBackendCommand::post(
            COMPONENT_ID,
            PORT_MEDIA_RESOLVE,
            OP_RESOLVE_PLAYBACK_PROXY,
            request_key(instance_id, project_id, clip_id),
            "/api/media/resolve",
            serde_json::json!({
                "project_id": project_id,
                "clip_id": clip_id,
                "access": MediaAccessKind::PlaybackProxy,
            }),
        )
        .with_timeout(HostRequestTimeout::Long)
    }

    pub(crate) fn accepts_event(event: &ComponentBackendEvent) -> bool {
        event.component_id == COMPONENT_ID
            && event.port_id == PORT_MEDIA_RESOLVE
            && event.operation_id == OP_RESOLVE_PLAYBACK_PROXY
    }

    pub(crate) fn into_media_resolution(
        event: ComponentBackendEvent,
    ) -> Option<(
        String,
        String,
        String,
        Result<PlaybackMediaResolution, String>,
    )> {
        if !Self::accepts_event(&event) {
            return None;
        }
        let (instance_id, project_id, clip_id) = split_request_key(&event.request_key)
            .unwrap_or_else(|| (String::new(), String::new(), event.request_key.clone()));
        let result = event.result.and_then(parse_media_resolution);
        Some((instance_id, project_id, clip_id, result))
    }
}

fn parse_media_resolution(value: serde_json::Value) -> Result<PlaybackMediaResolution, String> {
    let response: MediaResolveResponse =
        serde_json::from_value(value).map_err(|e| format!("media resolve: {e}"))?;
    let metadata = parse_playback_metadata(&response.metadata);
    match response.media.locator {
        MediaLocator::LocalPath { path } => {
            let media_input = path.to_string_lossy().trim().to_string();
            if media_input.is_empty() {
                return Err("media resolver returned empty local path".into());
            }
            Ok(PlaybackMediaResolution {
                media_input,
                locator_kind: "local",
                source_timebase: metadata.source_timebase,
                duration_sec: metadata.duration_sec,
                duration_frames: metadata.duration_frames,
                has_audio: metadata.has_audio,
                audio_channels: metadata.audio_channels,
            })
        }
        MediaLocator::IntranetPath { uri } => {
            let media_input = uri.trim().to_string();
            if media_input.is_empty() {
                return Err("media resolver returned empty intranet uri".into());
            }
            Ok(PlaybackMediaResolution {
                media_input,
                locator_kind: "intranet",
                source_timebase: metadata.source_timebase,
                duration_sec: metadata.duration_sec,
                duration_frames: metadata.duration_frames,
                has_audio: metadata.has_audio,
                audio_channels: metadata.audio_channels,
            })
        }
        MediaLocator::ManagedAsset { asset_id } => Err(format!(
            "media resolver returned managed asset '{asset_id}', but playback input route is not configured"
        )),
    }
}

#[derive(Default)]
struct PlaybackMetadata {
    source_timebase: Option<BroadcastSourceTimebase>,
    duration_sec: Option<f64>,
    duration_frames: Option<i64>,
    has_audio: Option<bool>,
    audio_channels: Option<u8>,
}

fn parse_playback_metadata(metadata: &serde_json::Value) -> PlaybackMetadata {
    let source_timebase = parse_source_timebase(metadata);
    PlaybackMetadata {
        source_timebase,
        duration_sec: metadata
            .get("duration_sec")
            .and_then(serde_json::Value::as_f64)
            .filter(|value| value.is_finite() && *value > 0.0),
        duration_frames: metadata
            .get("duration_frames")
            .and_then(serde_json::Value::as_i64)
            .filter(|frames| *frames > 0),
        has_audio: metadata
            .get("has_audio")
            .and_then(serde_json::Value::as_bool),
        audio_channels: metadata
            .get("audio_channels")
            .and_then(serde_json::Value::as_u64)
            .and_then(|channels| u8::try_from(channels).ok())
            .filter(|channels| *channels > 0),
    }
}

fn parse_source_timebase(metadata: &serde_json::Value) -> Option<BroadcastSourceTimebase> {
    let timebase = metadata
        .get("source_timebase")
        .or_else(|| metadata.get("timebase"))
        .or_else(|| {
            metadata
                .get("probe")
                .and_then(|probe| probe.get("timebase"))
        })?;
    let fps_num = timebase
        .get("fps_num")
        .or_else(|| timebase.get("frame_rate_num"))
        .and_then(serde_json::Value::as_u64)
        .and_then(|value| u32::try_from(value).ok())?;
    let fps_den = timebase
        .get("fps_den")
        .or_else(|| timebase.get("frame_rate_den"))
        .and_then(serde_json::Value::as_u64)
        .and_then(|value| u32::try_from(value).ok())?;
    let timebase = BroadcastSourceTimebase { fps_num, fps_den };
    timebase.is_valid().then_some(timebase)
}

fn request_key(instance_id: &str, project_id: &str, clip_id: &str) -> String {
    format!("{instance_id}{REQUEST_SEP}{project_id}{REQUEST_SEP}{clip_id}")
}

fn split_request_key(value: &str) -> Option<(String, String, String)> {
    let mut parts = value.split(REQUEST_SEP);
    let instance_id = parts.next()?.to_string();
    let project_id = parts.next()?.to_string();
    let clip_id = parts.next()?.to_string();
    Some((instance_id, project_id, clip_id))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::HostRequestMethod;
    use serde_json::json;

    #[test]
    fn resolve_playback_proxy_uses_media_gateway_contract() {
        let command =
            PlaybackMediaResolverComponent::resolve_playback_proxy("ingest", "p1", "clip_a");

        assert_eq!(command.component_id, COMPONENT_ID);
        assert_eq!(command.port_id, PORT_MEDIA_RESOLVE);
        assert_eq!(command.operation_id, OP_RESOLVE_PLAYBACK_PROXY);
        assert_eq!(command.path, "/api/media/resolve");
        assert_eq!(command.method, HostRequestMethod::Post);
        assert_eq!(command.timeout, HostRequestTimeout::Long);
        let payload = command.payload.expect("payload");
        assert_eq!(
            payload.get("project_id").and_then(|v| v.as_str()),
            Some("p1")
        );
        assert_eq!(
            payload.get("clip_id").and_then(|v| v.as_str()),
            Some("clip_a")
        );
        assert_eq!(
            payload.get("access").and_then(|v| v.as_str()),
            Some("playback_proxy")
        );
    }

    #[test]
    fn media_resolution_accepts_local_and_intranet_inputs() {
        let local = parse_media_resolution(json!({
            "media": {
                "clip_id": "clip_a",
                "locator": { "kind": "local_path", "path": "C:/qnc/proxy/a.mp4" }
            },
            "access": "playback_proxy",
            "gateway_kind": "local_fs",
            "read_only": false,
            "metadata": {}
        }))
        .expect("local");
        assert_eq!(local.media_input, "C:/qnc/proxy/a.mp4");
        assert_eq!(local.locator_kind, "local");

        let intranet = parse_media_resolution(json!({
            "media": {
                "clip_id": "clip_a",
                "locator": { "kind": "intranet_path", "uri": "http://mam.local/play/clip_a" }
            },
            "access": "playback_proxy",
            "gateway_kind": "enterprise_proxy",
            "read_only": true,
            "metadata": {}
        }))
        .expect("intranet");
        assert_eq!(intranet.media_input, "http://mam.local/play/clip_a");
        assert_eq!(intranet.locator_kind, "intranet");
    }

    #[test]
    fn media_resolution_parses_playback_probe_metadata() {
        let resolution = parse_media_resolution(json!({
            "media": {
                "clip_id": "clip_a",
                "locator": { "kind": "local_path", "path": "C:/qnc/proxy/a.mp4" }
            },
            "access": "playback_proxy",
            "gateway_kind": "local_fs",
            "read_only": true,
            "metadata": {
                "source_timebase": { "fps_num": 50, "fps_den": 1 },
                "duration_sec": 2.0,
                "duration_frames": 100,
                "has_audio": true,
                "audio_channels": 2
            }
        }))
        .expect("resolution");

        assert_eq!(
            resolution.source_timebase,
            Some(BroadcastSourceTimebase {
                fps_num: 50,
                fps_den: 1
            })
        );
        assert_eq!(resolution.duration_frames, Some(100));
        assert_eq!(resolution.duration_sec, Some(2.0));
        assert_eq!(resolution.has_audio, Some(true));
        assert_eq!(resolution.audio_channels, Some(2));
    }
}
