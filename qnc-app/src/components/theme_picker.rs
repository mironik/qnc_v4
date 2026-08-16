use eframe::egui::{self, RichText};

use crate::component_runtime::{ComponentBackendCommand, ComponentBackendEvent};
use crate::qnc_theme::ThemeId;

const COMPONENT_ID: &str = "theme.picker";
const PORT_SETTINGS: &str = "settings";
const OP_APPEARANCE_SAVE: &str = "appearance.save";
const REQUEST_ACTIVE_THEME: &str = "active-theme";

pub(crate) struct ThemePickerComponent {
    active: ThemeId,
    pending_save: bool,
}

impl Default for ThemePickerComponent {
    fn default() -> Self {
        Self {
            active: ThemeId::Dark,
            pending_save: false,
        }
    }
}

impl ThemePickerComponent {
    pub fn active(&self) -> ThemeId {
        self.active
    }

    pub fn set_active(&mut self, theme: ThemeId) {
        self.active = theme;
    }

    pub fn ui(&mut self, ui: &mut egui::Ui) -> Option<ComponentBackendCommand> {
        ui.label(RichText::new("Tema").weak());
        let mut selected = self.active;
        egui::ComboBox::from_id_salt(COMPONENT_ID)
            .selected_text(selected.label())
            .width(110.0)
            .show_ui(ui, |ui| {
                for id in ThemeId::ALL {
                    ui.selectable_value(&mut selected, id, id.label());
                }
            });
        if selected == self.active {
            return None;
        }

        self.active = selected;
        self.pending_save = true;
        Some(ComponentBackendCommand::post(
            COMPONENT_ID,
            PORT_SETTINGS,
            OP_APPEARANCE_SAVE,
            REQUEST_ACTIVE_THEME,
            "/api/settings/appearance",
            serde_json::json!({ "theme_id": selected.as_str() }),
        ))
    }

    pub fn accepts_event(&self, event: &ComponentBackendEvent) -> bool {
        event.component_id == COMPONENT_ID
            && event.port_id == PORT_SETTINGS
            && event.operation_id == OP_APPEARANCE_SAVE
            && event.request_key == REQUEST_ACTIVE_THEME
    }

    pub fn handle_event(&mut self, event: ComponentBackendEvent) -> Result<(), String> {
        if !self.accepts_event(&event) {
            return Ok(());
        }
        self.pending_save = false;
        event
            .result
            .map(|_| ())
            .map_err(|e| format!("{COMPONENT_ID}:{OP_APPEARANCE_SAVE} {e}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::component_runtime::ComponentResultPolicy;

    #[test]
    fn theme_picker_command_is_neutral_component_envelope() {
        let mut picker = ThemePickerComponent::default();
        picker.set_active(ThemeId::Soft);
        let mut selected = ThemeId::Soft;
        assert_eq!(picker.active(), selected);
        selected = ThemeId::HighContrast;
        picker.set_active(selected);
        let command = ComponentBackendCommand::post(
            COMPONENT_ID,
            PORT_SETTINGS,
            OP_APPEARANCE_SAVE,
            REQUEST_ACTIVE_THEME,
            "/api/settings/appearance",
            serde_json::json!({ "theme_id": selected.as_str() }),
        );
        assert_eq!(command.component_id, COMPONENT_ID);
        assert_eq!(command.port_id, PORT_SETTINGS);
        assert_eq!(command.operation_id, OP_APPEARANCE_SAVE);
        assert_eq!(command.request_key, REQUEST_ACTIVE_THEME);
        assert_eq!(command.result_policy, ComponentResultPolicy::LatestWins);
    }
}
