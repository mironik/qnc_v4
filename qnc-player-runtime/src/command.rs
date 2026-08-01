use qnc_broadcast_player::BroadcastPlayerProtocolCommand;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlayerRuntimeCommand {
    pub command_id: String,
    pub command: BroadcastPlayerProtocolCommand,
}

impl PlayerRuntimeCommand {
    pub fn new(command_id: impl Into<String>, command: BroadcastPlayerProtocolCommand) -> Self {
        Self {
            command_id: command_id.into(),
            command,
        }
    }

    pub fn command_name(&self) -> &'static str {
        self.command.command_name()
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.command_id.trim().is_empty() {
            return Err("command_id must not be blank".to_string());
        }
        self.command.validate()
    }
}
