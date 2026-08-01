use serde_json::{Value, json};

pub const BROADCAST_PLAYER_COMPONENT_ID: &str = "broadcast_player";

pub fn broadcast_player_contract() -> Value {
    json!({
        "component_id": "broadcast_player",
        "component_kind": "runtime_anchor",
        "display_name": "Broadcast Player",
        "neutral": true,
        "core_runtime": true,
        "runtime_scope": "av_playback_executor",
        "durable_truth_owner": false,
        "command_in": "broadcast_player.protocol.command",
        "event_out": "broadcast_player.protocol.event",
        "runtime_truth": [
            "carrier_frame",
            "timebase",
            "transport_status",
            "frame_clock_runtime",
            "active_source_runtime",
            "preloaded_source_runtime",
            "execution_range",
            "video_runtime",
            "audio_runtime",
            "av_sync"
        ],
        "player_commands": {
            "source_runtime": [
                "LoadSource",
                "PreloadSource",
                "SetActiveSource",
                "UnloadSource",
                "SetPlaybackRequest",
                "CueFrame"
            ],
            "transport": [
                "Play",
                "Pause",
                "Stop"
            ]
        },
        "playback_request_contract": [
            "single_playback_request",
            "request_id",
            "source_runtime_snapshot",
            "start_frame",
            "end_frame",
            "initial_frame",
            "execution_range",
            "rate",
            "audio_monitor_state"
        ],
        "engine_contract": [
            "SourceOpenAdapter",
            "VideoDecodeAdapter",
            "AudioOutputAdapter",
            "FramePresenter",
            "MonotonicScheduler"
        ],
        "events": [
            "CarrierPositionChanged",
            "TransportStatusChanged",
            "SourceReady",
            "SourceFailed",
            "SourcePreloaded",
            "ActiveSourceChanged",
            "SourceSnapshotReloaded",
            "VideoRuntimeChanged",
            "FramePresented",
            "PlaybackBoundaryReached",
            "AudioLevelChanged",
            "AudioRuntimeChanged",
            "AVSyncWarning",
            "DroppedFrame",
            "BufferStateChanged",
            "DecodeWarning",
            "CommandAccepted",
            "CommandRejected",
            "PlaybackError"
        ],
        "protocol_events": [
            "CommandAccepted",
            "CommandRejected",
            "SourceReady",
            "SourceFailed",
            "SourcePreloaded",
            "ActiveSourceChanged",
            "SourceSnapshotReloaded",
            "CarrierPositionChanged",
            "TransportStatusChanged",
            "ExecutionRangeChanged",
            "VideoRuntimeChanged",
            "FramePresented",
            "PlaybackBoundaryReached",
            "AudioLevelChanged",
            "AudioRuntimeChanged",
            "AVSyncWarning",
            "DroppedFrame",
            "BufferStateChanged",
            "DecodeWarning",
            "PlaybackError"
        ],
        "rules": [
            "all_position_math_is_frame_based",
            "timecode_display_is_derived_from_frame",
            "player_executes_request_without_owning_edit_semantics",
            "bounded_playback_uses_execution_boundary",
            "followup_after_execution_boundary_is_external_resolution",
            "protocol_accepts_only_av_executor_commands",
            "cue_frame_is_transport_position_only",
            "goto_and_edit_navigation_are_external_control_commands",
            "protocol_maps_boundary_without_edit_semantics",
            "execution_range_is_request_boundary",
            "engine_contract_is_adapter_only",
            "engine_contract_uses_source_runtime_snapshot",
            "engine_contract_returns_frame_based_events",
            "frame_clock_uses_timebase",
            "frame_clock_outputs_due_frames",
            "frame_clock_supports_request_rate",
            "frame_clock_supports_25_30_50_60_timebases",
            "transport_engine_uses_frame_clock",
            "transport_engine_uses_engine_contract_adapters",
            "transport_engine_preloads_sources",
            "transport_engine_commands_interrupt_prior_motion",
            "transport_engine_outputs_frame_based_events"
        ],
        "internal_modules": [
            "engine_contract",
            "event",
            "frame_clock",
            "interface::protocol",
            "interface::protocol::command",
            "interface::protocol::event",
            "interface::protocol::request",
            "model::frame",
            "model::source",
            "model::transport",
            "model::av",
            "transport_engine"
        ]
    })
}

pub fn assert_broadcast_player_contract(contract: &Value) -> Result<(), String> {
    expect_string(contract, "component_id", BROADCAST_PLAYER_COMPONENT_ID)?;
    expect_string(contract, "display_name", "Broadcast Player")?;
    expect_string(contract, "runtime_scope", "av_playback_executor")?;
    expect_string(contract, "command_in", "broadcast_player.protocol.command")?;
    expect_string(contract, "event_out", "broadcast_player.protocol.event")?;
    expect_bool(contract, "neutral", true)?;
    expect_bool(contract, "core_runtime", true)?;
    expect_bool(contract, "durable_truth_owner", false)?;

    for command in [
        "LoadSource",
        "PreloadSource",
        "SetActiveSource",
        "UnloadSource",
        "SetPlaybackRequest",
        "CueFrame",
        "Play",
        "Pause",
        "Stop",
    ] {
        require_json_text(contract, command)?;
    }

    for event in [
        "CarrierPositionChanged",
        "TransportStatusChanged",
        "SourceReady",
        "FramePresented",
        "AudioLevelChanged",
        "AudioRuntimeChanged",
        "AVSyncWarning",
        "VideoRuntimeChanged",
        "DroppedFrame",
        "PlaybackBoundaryReached",
        "PlaybackError",
    ] {
        require_json_text(contract, event)?;
    }

    for rule in [
        "all_position_math_is_frame_based",
        "timecode_display_is_derived_from_frame",
        "player_executes_request_without_owning_edit_semantics",
        "bounded_playback_uses_execution_boundary",
        "followup_after_execution_boundary_is_external_resolution",
        "protocol_accepts_only_av_executor_commands",
        "goto_and_edit_navigation_are_external_control_commands",
        "protocol_maps_boundary_without_edit_semantics",
        "execution_range_is_request_boundary",
        "engine_contract_is_adapter_only",
        "engine_contract_uses_source_runtime_snapshot",
        "engine_contract_returns_frame_based_events",
        "frame_clock_uses_timebase",
        "frame_clock_outputs_due_frames",
        "frame_clock_supports_request_rate",
        "frame_clock_supports_25_30_50_60_timebases",
        "transport_engine_uses_frame_clock",
        "transport_engine_uses_engine_contract_adapters",
        "transport_engine_preloads_sources",
        "transport_engine_commands_interrupt_prior_motion",
        "transport_engine_outputs_frame_based_events",
    ] {
        require_json_text(contract, rule)?;
    }

    for adapter in [
        "SourceOpenAdapter",
        "VideoDecodeAdapter",
        "AudioOutputAdapter",
        "FramePresenter",
        "MonotonicScheduler",
    ] {
        require_json_text(contract, adapter)?;
    }

    for forbidden in [
        concat!("in", "gest"),
        concat!("sto", "ry"),
        concat!("media", "_assist"),
        concat!("active", "_form"),
        concat!("form", "_owner"),
        concat!("ui", "_owner"),
        concat!("sec", "ond"),
        concat!("sec", "onds"),
        concat!("milli", "sec", "ond"),
        concat!("time", "stamp"),
        concat!("Set", "In"),
        concat!("Set", "Out"),
        concat!("Set", "Play", "out", "Range"),
        concat!("Move", "Mar", "ker"),
        concat!("Add", "Mar", "ker"),
        concat!("Add", "Lay", "er"),
        concat!("Add", "Edit", "Item"),
        concat!("Un", "do", "De", "lete"),
        concat!("play", "out", "_range"),
        concat!("command", "_bus"),
        concat!("command", "_template"),
        concat!("carrier", "_event", "_bus"),
        "quarantined",
        concat!("Go", "To", "Frame"),
        concat!("Step", "Frame"),
        concat!("Jog", "Frame"),
        concat!("Shut", "tle"),
        concat!("Set", "Rate"),
        concat!("Set", "Timebase"),
        concat!("Set", "Video", "Format"),
        concat!("Set", "Color", "Space"),
        concat!("Set", "Pixel", "Aspect"),
        concat!("Set", "Source", "Start", "Tc"),
        concat!("Map", "Source", "Timecode"),
        concat!("Set", "Field", "Mode"),
        concat!("Set", "Drop", "Frame", "Mode"),
        concat!("Set", "Audio", "Runtime"),
        concat!("Set", "Audio", "Track", "Enabled"),
        concat!("Set", "Audio", "Channel", "Map"),
        concat!("Set", "Monitor", "Volume"),
        concat!("Mu", "te"),
        concat!("So", "lo"),
        concat!("Reload", "Source", "Snapshot"),
    ] {
        reject_json_text(contract, forbidden)?;
    }
    Ok(())
}

fn expect_string(contract: &Value, key: &str, expected: &str) -> Result<(), String> {
    let actual = contract
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("broadcast player contract missing string key: {key}"))?;
    if actual != expected {
        return Err(format!(
            "broadcast player contract key {key} expected {expected}, got {actual}"
        ));
    }
    Ok(())
}

fn expect_bool(contract: &Value, key: &str, expected: bool) -> Result<(), String> {
    let actual = contract
        .get(key)
        .and_then(Value::as_bool)
        .ok_or_else(|| format!("broadcast player contract missing bool key: {key}"))?;
    if actual != expected {
        return Err(format!(
            "broadcast player contract key {key} expected {expected}, got {actual}"
        ));
    }
    Ok(())
}

fn require_json_text(contract: &Value, needle: &str) -> Result<(), String> {
    let haystack = serde_json::to_string(contract).map_err(|err| err.to_string())?;
    if !haystack.contains(needle) {
        return Err(format!(
            "broadcast player contract missing required text: {needle}"
        ));
    }
    Ok(())
}

fn reject_json_text(contract: &Value, needle: &str) -> Result<(), String> {
    let haystack = serde_json::to_string(contract)
        .map_err(|err| err.to_string())?
        .to_ascii_lowercase();
    let needle = needle.to_ascii_lowercase();
    if haystack.contains(&needle) {
        return Err(format!(
            "broadcast player contract contains forbidden text: {needle}"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    #[test]
    fn contract_is_neutral_and_complete() {
        let contract = broadcast_player_contract();
        assert_broadcast_player_contract(&contract).expect("contract");
    }

    #[test]
    fn contract_runtime_truth_is_player_executor_only() {
        let contract = broadcast_player_contract();

        assert_eq!(
            array_set(&contract, "runtime_truth"),
            string_set(&[
                "carrier_frame",
                "timebase",
                "transport_status",
                "frame_clock_runtime",
                "active_source_runtime",
                "preloaded_source_runtime",
                "execution_range",
                "video_runtime",
                "audio_runtime",
                "av_sync",
            ])
        );
    }

    #[test]
    fn contract_player_commands_are_executor_subset() {
        let contract = broadcast_player_contract();
        let player_commands = player_command_set(&contract);

        assert_eq!(
            player_commands,
            string_set(&[
                "LoadSource",
                "PreloadSource",
                "SetActiveSource",
                "UnloadSource",
                "SetPlaybackRequest",
                "CueFrame",
                "Play",
                "Pause",
                "Stop",
            ])
        );

        for external in [
            concat!("Go", "To", "Frame"),
            concat!("Step", "Frame"),
            concat!("Jog", "Frame"),
            concat!("Shut", "tle"),
            concat!("Set", "Rate"),
            concat!("Set", "Timebase"),
            concat!("Set", "Video", "Format"),
            concat!("Set", "Color", "Space"),
            concat!("Set", "Pixel", "Aspect"),
            concat!("Set", "Source", "Start", "Tc"),
            concat!("Map", "Source", "Timecode"),
            concat!("Set", "Field", "Mode"),
            concat!("Set", "Drop", "Frame", "Mode"),
            concat!("Set", "Audio", "Runtime"),
            concat!("Set", "Audio", "Track", "Enabled"),
            concat!("Set", "Audio", "Channel", "Map"),
            concat!("Set", "Monitor", "Volume"),
            concat!("Mu", "te"),
            concat!("So", "lo"),
            concat!("Reload", "Source", "Snapshot"),
            concat!("Set", "In"),
            concat!("Set", "Out"),
            concat!("Set", "Play", "out", "Range"),
            concat!("Move", "In"),
            concat!("Move", "Out"),
            concat!("Add", "Mar", "ker"),
            concat!("Move", "Mar", "ker"),
            concat!("Add", "Lay", "er"),
            concat!("Add", "Edit", "Item"),
            concat!("Move", "Overlay", "Item"),
            concat!("Un", "do"),
            concat!("Re", "do"),
            concat!("Un", "do", "De", "lete"),
        ] {
            assert!(!player_commands.contains(external));
        }
    }

    #[test]
    fn contract_modules_are_only_player_modules() {
        let contract = broadcast_player_contract();
        assert_eq!(
            array_set(&contract, "internal_modules"),
            string_set(&[
                "engine_contract",
                "event",
                "frame_clock",
                "interface::protocol",
                "interface::protocol::command",
                "interface::protocol::event",
                "interface::protocol::request",
                "model::frame",
                "model::source",
                "model::transport",
                "model::av",
                "transport_engine",
            ])
        );
    }

    #[test]
    fn contract_position_language_stays_frame_based() {
        let contract = broadcast_player_contract();
        let contract_text = serde_json::to_string(&contract)
            .unwrap()
            .to_ascii_lowercase();

        for required in [
            "carrier_frame",
            "all_position_math_is_frame_based",
            "timecode_display_is_derived_from_frame",
            "execution_boundary",
        ] {
            assert!(contract_text.contains(required));
        }

        for forbidden in [
            concat!("sec", "ond"),
            concat!("milli", "sec", "ond"),
            concat!("time", "stamp"),
            concat!("play", "out", "_range"),
        ] {
            assert!(!contract_text.contains(forbidden));
        }
    }

    fn array_set(contract: &Value, key: &str) -> BTreeSet<String> {
        contract
            .get(key)
            .and_then(Value::as_array)
            .unwrap()
            .iter()
            .map(|value| value.as_str().unwrap().to_string())
            .collect()
    }

    fn player_command_set(contract: &Value) -> BTreeSet<String> {
        contract
            .get("player_commands")
            .and_then(Value::as_object)
            .unwrap()
            .values()
            .flat_map(|value| value.as_array().unwrap())
            .map(|value| value.as_str().unwrap().to_string())
            .collect()
    }

    fn string_set(values: &[&str]) -> BTreeSet<String> {
        values.iter().map(|value| value.to_string()).collect()
    }
}
