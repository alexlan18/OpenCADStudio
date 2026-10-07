use crate::modules::{IconKind, ModuleEvent, ToolDef};
pub const ICON: IconKind = IconKind::Svg(include_bytes!("../../../assets/icons/ai_assistant.svg"));
/// View › Palettes › AI Assistant: opens or closes the chat panel.
pub fn tool() -> ToolDef {
    ToolDef {
        id: "AIASSIST",
        label: "AI\nAssistant",
        icon: ICON,
        event: ModuleEvent::Command("_AIASSISTTOGGLE".to_string()),
    }
}

inventory::submit!(crate::command::CommandRegistration {
    names: &["AIASSIST", "AIASSISTCLOSE"]
});
