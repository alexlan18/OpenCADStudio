//! Built-in AI assistant: a docked chat panel whose model drives the editor
//! through the same tool surface the MCP server advertises.
//!
//! The panel state and its messages live here; the model conversation runs
//! on the worker thread in [`agent`] (native only) and the wire formats are
//! in [`provider`]. Every tool call the model makes arrives back in the GUI
//! as a `Message::ControlRequest`, so the assistant can do exactly what an
//! external MCP client can — no more, no less.

#[cfg(not(target_arch = "wasm32"))]
pub mod agent;
pub mod provider;

pub use provider::{AssistantSettings, Effort, Provider, Usage};

use super::{Message, OpenCADStudio};
use crate::app::control::Envelope;
use iced::widget::{image, markdown, text_editor};
use iced::Task;
use serde_json::Value;
use std::sync::Arc;

/// Progress the worker reports back to the GUI.
#[derive(Debug, Clone)]
pub enum AssistantEvent {
    /// The worker thread is up and accepting commands.
    Ready,
    TurnStarted,
    AssistantText(String),
    ToolCall {
        id: String,
        name: String,
        input: Value,
    },
    ToolResult {
        id: String,
        ok: bool,
        text: String,
        image_png: Option<Arc<Vec<u8>>>,
    },
    /// Cumulative usage for the conversation.
    Usage(Usage),
    /// The turn is over: `end_turn`, `cancelled`, `max_rounds`, `error`…
    TurnFinished(String),
    Error(String),
    /// A tool call for the GUI's automation dispatcher.
    Control(Envelope),
}

/// Panel interactions and worker events.
#[derive(Debug, Clone)]
pub enum AssistantMsg {
    Input(text_editor::Action),
    Send,
    Stop,
    NewChat,
    ToggleSettings,
    Provider(Provider),
    BaseUrl(String),
    Model(String),
    ApiKey(String),
    Effort(Effort),
    ToggleEntry(usize),
    Event(AssistantEvent),
}

/// What a finished tool call showed.
#[derive(Debug)]
pub struct ToolResultView {
    pub ok: bool,
    pub text: String,
    pub image: Option<image::Handle>,
}

/// One item in the transcript.
#[derive(Debug)]
pub enum Entry {
    User(String),
    Assistant {
        text: String,
        markdown: markdown::Content,
    },
    Tool {
        id: String,
        name: String,
        /// One line for the collapsed card: the op and its key argument.
        summary: String,
        /// Pretty-printed arguments for the expanded card.
        input: String,
        result: Option<ToolResultView>,
        expanded: bool,
    },
    Error(String),
}

/// The docked chat panel.
pub struct AssistantPanel {
    pub show: bool,
    pub settings: AssistantSettings,
    pub settings_open: bool,
    pub input: text_editor::Content,
    pub entries: Vec<Entry>,
    /// A turn is in flight (model call or tool rounds).
    pub running: bool,
    /// The worker subscription has started.
    pub ready: bool,
    pub usage: Usage,
    /// Transcript scrollable, for snapping to the newest message.
    pub scroll_id: iced::widget::Id,
}

impl Default for AssistantPanel {
    fn default() -> Self {
        Self {
            show: false,
            settings: AssistantSettings::default(),
            settings_open: false,
            input: text_editor::Content::new(),
            entries: Vec::new(),
            running: false,
            ready: false,
            usage: Usage::default(),
            scroll_id: iced::widget::Id::new("assistant-transcript"),
        }
    }
}

impl std::fmt::Debug for AssistantPanel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AssistantPanel")
            .field("show", &self.show)
            .field("entries", &self.entries.len())
            .field("running", &self.running)
            .finish()
    }
}

/// Collapsed-card label for a tool call.
pub fn summarize_call(name: &str, input: &Value) -> String {
    fn short(value: &Value, max: usize) -> String {
        let text = match value {
            Value::String(s) => s.clone(),
            Value::Null => String::new(),
            other => other.to_string(),
        };
        if text.chars().count() > max {
            let cut: String = text.chars().take(max).collect();
            format!("{cut}…")
        } else {
            text
        }
    }
    match name {
        "ocs_read" => {
            let op = input["op"].as_str().unwrap_or("state");
            let params = &input["parameters"];
            let detail = ["name", "search", "collection", "type", "find", "handle"]
                .iter()
                .find_map(|key| params.get(*key).filter(|v| !v.is_null()).map(|v| short(v, 40)))
                .unwrap_or_default();
            if detail.is_empty() {
                format!("read {op}")
            } else {
                format!("read {op} · {detail}")
            }
        }
        "ocs_execute" => {
            let request = &input["request"];
            let op = request["op"].as_str().unwrap_or("?");
            match op {
                "run" | "start" => format!("{op} · {}", short(&request["cmd"], 60)),
                "input" => format!("input · {}", short(&request["text"], 40)),
                "batch" => format!(
                    "batch · {} steps",
                    request["steps"].as_array().map(Vec::len).unwrap_or(0)
                ),
                _ => {
                    let mut rest = request.as_object().cloned().unwrap_or_default();
                    for key in ["op", "request_id", "document_id", "revision", "client_id", "selection"] {
                        rest.remove(key);
                    }
                    if rest.is_empty() {
                        op.to_string()
                    } else {
                        format!("{op} · {}", short(&Value::Object(rest), 60))
                    }
                }
            }
        }
        "ocs_capture" => format!(
            "capture {}{}",
            input["scope"].as_str().unwrap_or("viewport"),
            if input["annotate"].as_bool().unwrap_or(false) { " · annotated" } else { "" }
        ),
        other => other.to_string(),
    }
}

fn pretty(value: &Value) -> String {
    let text = serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string());
    const MAX: usize = 4000;
    if text.len() > MAX {
        format!("{}…", &text[..text.floor_char_boundary(MAX)])
    } else {
        text
    }
}

fn decode_png(bytes: &[u8]) -> Option<image::Handle> {
    let decoded = ::image::load_from_memory_with_format(bytes, ::image::ImageFormat::Png).ok()?;
    let rgba = decoded.to_rgba8();
    Some(image::Handle::from_rgba(rgba.width(), rgba.height(), rgba.into_raw()))
}

impl OpenCADStudio {
    /// Show or hide the panel (docked on the right like the other managers).
    pub(in crate::app) fn set_assistant_panel(&mut self, open: bool) {
        let id = crate::ui::dock::PanelId::Assistant;
        self.assistant.show = open;
        self.assistant.settings.panel_open = open;
        self.ribbon.set_assistant(open);
        if open {
            if self.dock.location(id).is_none() {
                self.dock
                    .dock(id, crate::app::config::DockSide::Right, usize::MAX);
            }
            self.dock_expanded = Some(id);
        } else if self.dock_expanded == Some(id) {
            self.dock_expanded = None;
        }
        self.persist_settings_if_changed();
    }

    /// `AIASSIST` / `AIASSISTCLOSE` and the ribbon toggle.
    pub(in crate::app) fn dispatch_assistant(&mut self, cmd: &str, _i: usize) -> Option<Task<Message>> {
        match cmd {
            "AIASSIST" => {
                self.set_assistant_panel(true);
                Some(Task::none())
            }
            "AIASSISTCLOSE" => {
                self.set_assistant_panel(false);
                Some(Task::none())
            }
            "_AIASSISTTOGGLE" => {
                let open = !self.assistant.show;
                self.set_assistant_panel(open);
                Some(Task::none())
            }
            _ => None,
        }
    }

    fn assistant_scroll_to_end(&self) -> Task<Message> {
        iced::widget::operation::snap_to_end(self.assistant.scroll_id.clone())
    }

    pub(in crate::app) fn on_assistant(&mut self, msg: AssistantMsg) -> Task<Message> {
        match msg {
            AssistantMsg::Input(action) => {
                self.assistant.input.perform(action);
                Task::none()
            }
            AssistantMsg::Send => self.assistant_send(),
            AssistantMsg::Stop => {
                #[cfg(not(target_arch = "wasm32"))]
                agent::request_cancel();
                Task::none()
            }
            AssistantMsg::NewChat => {
                if self.assistant.running {
                    #[cfg(not(target_arch = "wasm32"))]
                    agent::request_cancel();
                }
                self.assistant.entries.clear();
                self.assistant.usage = Usage::default();
                #[cfg(not(target_arch = "wasm32"))]
                let _ = agent::submit(agent::AgentCommand::Reset);
                Task::none()
            }
            AssistantMsg::ToggleSettings => {
                self.assistant.settings_open = !self.assistant.settings_open;
                Task::none()
            }
            AssistantMsg::Provider(provider) => {
                if self.assistant.settings.provider != provider {
                    self.assistant.settings.provider = provider;
                    // The two wire formats do not share a transcript.
                    self.assistant.entries.clear();
                    self.assistant.usage = Usage::default();
                    #[cfg(not(target_arch = "wasm32"))]
                    let _ = agent::submit(agent::AgentCommand::Reset);
                    self.persist_settings_if_changed();
                }
                Task::none()
            }
            AssistantMsg::BaseUrl(value) => {
                self.assistant.settings.base_url = value;
                self.persist_settings_if_changed();
                Task::none()
            }
            AssistantMsg::Model(value) => {
                self.assistant.settings.model = value;
                self.persist_settings_if_changed();
                Task::none()
            }
            AssistantMsg::ApiKey(value) => {
                self.assistant.settings.api_key = value;
                self.persist_settings_if_changed();
                Task::none()
            }
            AssistantMsg::Effort(effort) => {
                self.assistant.settings.effort = effort;
                self.persist_settings_if_changed();
                Task::none()
            }
            AssistantMsg::ToggleEntry(index) => {
                if let Some(Entry::Tool { expanded, .. }) = self.assistant.entries.get_mut(index) {
                    *expanded = !*expanded;
                }
                Task::none()
            }
            AssistantMsg::Event(event) => self.on_assistant_event(event),
        }
    }

    fn assistant_send(&mut self) -> Task<Message> {
        let text = self.assistant.input.text();
        let text = text.trim();
        if text.is_empty() || self.assistant.running {
            return Task::none();
        }
        let text = text.to_string();
        #[cfg(target_arch = "wasm32")]
        {
            self.assistant.entries.push(Entry::User(text));
            self.assistant.input = text_editor::Content::new();
            self.assistant.entries.push(Entry::Error(
                crate::t!("The AI assistant needs the desktop application.").into_owned(),
            ));
            return self.assistant_scroll_to_end();
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            let command = agent::AgentCommand::Send {
                settings: self.assistant.settings.clone(),
                text: text.clone(),
            };
            match agent::submit(command) {
                Ok(()) => {
                    self.assistant.entries.push(Entry::User(text));
                    self.assistant.input = text_editor::Content::new();
                    self.assistant.running = true;
                    if !self.assistant.show {
                        self.set_assistant_panel(true);
                    }
                }
                Err(error) => self.assistant.entries.push(Entry::Error(error)),
            }
            self.assistant_scroll_to_end()
        }
    }

    fn on_assistant_event(&mut self, event: AssistantEvent) -> Task<Message> {
        match event {
            AssistantEvent::Ready => {
                self.assistant.ready = true;
                Task::none()
            }
            AssistantEvent::TurnStarted => {
                self.assistant.running = true;
                Task::none()
            }
            AssistantEvent::AssistantText(text) => {
                let markdown = markdown::Content::parse(&text);
                self.assistant.entries.push(Entry::Assistant { text, markdown });
                self.assistant_scroll_to_end()
            }
            AssistantEvent::ToolCall { id, name, input } => {
                let summary = summarize_call(&name, &input);
                self.assistant.entries.push(Entry::Tool {
                    id,
                    name,
                    summary,
                    input: pretty(&input),
                    result: None,
                    expanded: false,
                });
                self.assistant_scroll_to_end()
            }
            AssistantEvent::ToolResult { id, ok, text, image_png } => {
                let image = image_png.as_deref().and_then(|bytes| decode_png(bytes));
                let shown = if text.len() > 4000 {
                    format!("{}…", &text[..text.floor_char_boundary(4000)])
                } else {
                    text
                };
                if let Some(Entry::Tool { result, .. }) = self
                    .assistant
                    .entries
                    .iter_mut()
                    .rev()
                    .find(|entry| matches!(entry, Entry::Tool { id: entry_id, .. } if *entry_id == id))
                {
                    *result = Some(ToolResultView { ok, text: shown, image });
                }
                self.assistant_scroll_to_end()
            }
            AssistantEvent::Usage(usage) => {
                self.assistant.usage = usage;
                Task::none()
            }
            AssistantEvent::TurnFinished(_) => {
                self.assistant.running = false;
                Task::none()
            }
            AssistantEvent::Error(error) => {
                self.assistant.entries.push(Entry::Error(error));
                self.assistant_scroll_to_end()
            }
            AssistantEvent::Control(envelope) => self.update(Message::ControlRequest(envelope)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn tool_call_summaries_name_the_op_and_key_argument() {
        assert_eq!(summarize_call("ocs_read", &json!({"op": "state"})), "read state");
        assert_eq!(
            summarize_call("ocs_read", &json!({"op": "commands", "parameters": {"name": "LINE"}})),
            "read commands · LINE"
        );
        assert_eq!(
            summarize_call("ocs_execute", &json!({"request": {"op": "run", "cmd": "LINE 0,0 10,0"}})),
            "run · LINE 0,0 10,0"
        );
        assert_eq!(
            summarize_call("ocs_execute", &json!({"request": {"op": "batch", "steps": [{}, {}]}})),
            "batch · 2 steps"
        );
        assert_eq!(
            summarize_call("ocs_execute", &json!({"request": {"op": "undo", "request_id": "x", "document_id": 1}})),
            "undo"
        );
        assert_eq!(
            summarize_call("ocs_capture", &json!({"scope": "window", "annotate": true})),
            "capture window · annotated"
        );
    }

    #[test]
    fn panel_toggles_through_commands_and_docks_on_the_right() {
        let mut app = OpenCADStudio::new();
        // A fresh profile opens the panel on the right edge, auto-collapsed.
        assert!(app.assistant.show);
        assert_eq!(
            app.dock.location(crate::ui::dock::PanelId::Assistant).map(|(side, _)| side),
            Some(crate::app::config::DockSide::Right)
        );
        assert!(app.dock.auto_collapse(crate::ui::dock::PanelId::Assistant));
        app.set_assistant_panel(false);
        assert!(!app.assistant.show);
        assert!(!app.current_settings().assistant.panel_open);
        assert!(app.dispatch_assistant("AIASSIST", 0).is_some());
        assert!(app.assistant.show);
        assert_eq!(
            app.dock.location(crate::ui::dock::PanelId::Assistant).map(|(side, _)| side),
            Some(crate::app::config::DockSide::Right)
        );
        assert_eq!(app.dock_expanded, Some(crate::ui::dock::PanelId::Assistant));
        app.dispatch_assistant("_AIASSISTTOGGLE", 0);
        assert!(!app.assistant.show);
        assert_eq!(app.dock_expanded, None);
        assert!(app.dispatch_assistant("LINE", 0).is_none());
    }

    #[test]
    fn events_build_the_transcript_and_attach_results_to_their_call() {
        let mut app = OpenCADStudio::new();
        let _ = app.on_assistant(AssistantMsg::Event(AssistantEvent::TurnStarted));
        assert!(app.assistant.running);
        let _ = app.on_assistant(AssistantMsg::Event(AssistantEvent::AssistantText("**hi**".into())));
        let _ = app.on_assistant(AssistantMsg::Event(AssistantEvent::ToolCall {
            id: "t1".into(),
            name: "ocs_read".into(),
            input: json!({"op": "state"}),
        }));
        let _ = app.on_assistant(AssistantMsg::Event(AssistantEvent::ToolResult {
            id: "t1".into(),
            ok: true,
            text: "{\"ok\":true}".into(),
            image_png: None,
        }));
        let _ = app.on_assistant(AssistantMsg::Event(AssistantEvent::Usage(Usage {
            input_tokens: 12,
            output_tokens: 3,
        })));
        let _ = app.on_assistant(AssistantMsg::Event(AssistantEvent::TurnFinished("end_turn".into())));
        assert!(!app.assistant.running);
        assert_eq!(app.assistant.usage.input_tokens, 12);
        assert_eq!(app.assistant.entries.len(), 2);
        match &app.assistant.entries[1] {
            Entry::Tool { summary, result, .. } => {
                assert_eq!(summary, "read state");
                assert!(result.as_ref().is_some_and(|r| r.ok));
            }
            other => panic!("unexpected entry {other:?}"),
        }
        let _ = app.on_assistant(AssistantMsg::ToggleEntry(1));
        assert!(matches!(&app.assistant.entries[1], Entry::Tool { expanded: true, .. }));
        let _ = app.on_assistant(AssistantMsg::NewChat);
        assert!(app.assistant.entries.is_empty());
    }

    #[test]
    fn control_events_reach_the_automation_dispatcher() {
        let mut app = OpenCADStudio::new();
        let (tx, rx) = std::sync::mpsc::channel();
        let envelope = Envelope {
            request: json!({"op": "state"}),
            reply: crate::app::control::Reply::Native(tx),
        };
        let _ = app.on_assistant(AssistantMsg::Event(AssistantEvent::Control(envelope)));
        let response = rx.try_recv().expect("the dispatcher answers synchronously");
        assert_eq!(response["ok"], true);
        assert!(response["document_id"].is_number());
    }

    #[test]
    fn changing_provider_resets_the_transcript_and_persists() {
        let mut app = OpenCADStudio::new();
        let _ = app.on_assistant(AssistantMsg::Event(AssistantEvent::AssistantText("x".into())));
        let _ = app.on_assistant(AssistantMsg::Provider(Provider::OpenAiCompatible));
        assert!(app.assistant.entries.is_empty());
        assert_eq!(app.current_settings().assistant.provider, Provider::OpenAiCompatible);
        let _ = app.on_assistant(AssistantMsg::Model("local-model".into()));
        assert_eq!(app.current_settings().assistant.model, "local-model");
    }
}
