//! The assistant's agent loop: one worker thread that talks HTTP to the
//! model and executes the model's tool calls against the live editor.
//!
//! Tool calls never touch the document from this thread. Each one becomes a
//! [`Envelope`] delivered to the GUI through the iced subscription, exactly
//! like the TCP automation bridge and the MCP stdio server do, so the same
//! `control_request` dispatcher validates, runs, records and undoes it. The
//! request/poll semantics mirror `mcp::GuiClient::request`: identifiers and
//! the optimistic-state fields are filled in from the last known state,
//! `accepted`/`running` answers are polled through `operation`, and
//! interactive picks (`user_select`, `getpoint`) may wait up to ten minutes
//! for the person at the screen.

use super::memory::MemoryStore;
use super::provider::{self, AssistantSettings, ModelProfile, Provider, StopReason, ToolCall, ToolOutcome, ToolSpec, Usage};
pub use super::AssistantEvent;
use crate::app::control::{Envelope, Reply};
use iced::futures::{channel::mpsc as fmpsc, SinkExt, Stream};
use serde_json::{json, Map, Value};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

/// Largest tool result text handed to the model; the GUI can return whole
/// record tables, and the model has `limit`/`offset`/`fields` to page.
const MAX_TOOL_TEXT: usize = 120_000;
/// Per-exchange ceiling for one GUI answer (the GUI answers synchronously
/// inside `update`, so this only trips when the UI thread is wedged).
const GUI_EXCHANGE_TIMEOUT: Duration = Duration::from_secs(30);
/// Interactive picks wait for a person, so they get the MCP ceiling.
const INTERACTIVE_MAX_WAIT: Duration = Duration::from_secs(600);
/// A model call can legitimately take minutes on hard tasks.
const LLM_TIMEOUT: Duration = Duration::from_secs(600);

/// Instructions the GUI sends to the worker.
#[derive(Debug, Clone)]
pub enum AgentCommand {
    /// Run one user turn (and its tool rounds) with these settings.
    Send {
        settings: AssistantSettings,
        text: String,
    },
    /// Forget the conversation.
    Reset,
}


static INBOX: OnceLock<Mutex<Option<mpsc::Sender<AgentCommand>>>> = OnceLock::new();
static CANCEL: AtomicBool = AtomicBool::new(false);

fn inbox() -> &'static Mutex<Option<mpsc::Sender<AgentCommand>>> {
    INBOX.get_or_init(|| Mutex::new(None))
}

/// Hand a command to the worker; fails until the subscription has started.
pub fn submit(command: AgentCommand) -> Result<(), String> {
    let guard = inbox().lock().map_err(|_| "assistant worker lock poisoned".to_string())?;
    let sender = guard
        .as_ref()
        .ok_or_else(|| "The assistant is still starting; try again in a moment".to_string())?;
    sender
        .send(command)
        .map_err(|_| "The assistant worker has stopped".to_string())
}

/// Ask the running turn to stop after its current step.
pub fn request_cancel() {
    CANCEL.store(true, Ordering::SeqCst);
}

fn cancelled() -> bool {
    CANCEL.load(Ordering::SeqCst)
}

/// The iced subscription that owns the worker thread for the app's lifetime.
pub fn subscribe() -> iced::Subscription<AssistantEvent> {
    iced::Subscription::run(worker)
}

fn worker() -> impl Stream<Item = AssistantEvent> {
    iced::stream::channel(64, |mut sender: fmpsc::Sender<AssistantEvent>| async move {
        let (tx, rx) = mpsc::channel();
        if let Ok(mut guard) = inbox().lock() {
            *guard = Some(tx);
        }
        let _ = sender.send(AssistantEvent::Ready).await;
        std::thread::Builder::new()
            .name("ocs-assistant".into())
            .spawn(move || run_loop(rx, sender))
            .expect("spawn assistant worker");
        iced::futures::future::pending::<()>().await;
    })
}

/// Everything the worker remembers between turns.
struct Session {
    provider: Provider,
    messages: Vec<Value>,
    /// Last `state` seen from the GUI: document_id, revision, selection…
    state: Value,
    client_id: String,
    usage: Usage,
    serial: u64,
    events: fmpsc::Sender<AssistantEvent>,
    /// `agent/memory` beside the executable: notes the model keeps and the
    /// transcript of this conversation. `None` when no location is writable.
    memory: Option<MemoryStore>,
    /// The endpoint rejected image content once; captures travel as text
    /// metadata only from then on.
    images_unsupported: bool,
}

fn run_loop(rx: mpsc::Receiver<AgentCommand>, events: fmpsc::Sender<AssistantEvent>) {
    let mut session = Session {
        provider: Provider::Anthropic,
        messages: Vec::new(),
        state: json!({}),
        client_id: format!("assistant-{}", std::process::id()),
        usage: Usage::default(),
        serial: 0,
        events,
        memory: MemoryStore::open_default(),
        images_unsupported: false,
    };
    if let Some(memory) = &session.memory {
        log::info!("agent memory at {}", memory.root().display());
    }
    while let Ok(command) = rx.recv() {
        match command {
            AgentCommand::Reset => {
                session.messages.clear();
                session.usage = Usage::default();
                session.images_unsupported = false;
                if let Some(memory) = session.memory.as_mut() {
                    memory.new_session();
                }
            }
            AgentCommand::Send { settings, text } => {
                CANCEL.store(false, Ordering::SeqCst);
                let chat_provider = settings.active().provider;
                if session.provider != chat_provider {
                    session.provider = chat_provider;
                    session.messages.clear();
                    session.usage = Usage::default();
                    session.images_unsupported = false;
                    if let Some(memory) = session.memory.as_mut() {
                        memory.new_session();
                    }
                }
                session.run_turn(&settings, &text);
            }
        }
    }
}

/// The MCP tool surface minus session plumbing: the built-in assistant is
/// bound to this editor, so `ocs_sessions` and `ocs_session_id` vanish, and
/// capture options that only make sense as MCP resources go too.
pub fn tool_specs() -> Vec<ToolSpec> {
    let Value::Array(tools) = crate::mcp::tool_definitions() else {
        return Vec::new();
    };
    tools
        .into_iter()
        .filter(|tool| tool["name"] != "ocs_sessions")
        .map(|tool| {
            let mut schema = tool["inputSchema"].clone();
            if let Some(properties) = schema["properties"].as_object_mut() {
                properties.remove("ocs_session_id");
                if tool["name"] == "ocs_capture" {
                    for key in [
                        "delivery",
                        "tile",
                        "pyramid_manifest",
                        "diff",
                        "diff_mode",
                        "reset_diff_baseline",
                        "request_id",
                    ] {
                        properties.remove(key);
                    }
                    properties.insert(
                        "question".into(),
                        json!({"type": "string", "description": "What to look for in the capture. When a separate vision model describes the image for you (because your own model takes text only), this steers its description; be specific (alignment, overlaps, labels, proportions)."}),
                    );
                    schema["additionalProperties"] = json!(false);
                }
            }
            if let Some(required) = schema["required"].as_array_mut() {
                required.retain(|r| r != "ocs_session_id");
                if required.is_empty() {
                    schema.as_object_mut().map(|o| o.remove("required"));
                }
            }
            let description = tool["description"].as_str().unwrap_or_default();
            let description = match tool["name"].as_str() {
                Some("ocs_capture") => format!(
                    "{description} The image is returned to you directly when your model accepts images; otherwise a configured vision model describes it (pass question) and you receive the description plus the _spatial metadata."
                ),
                _ => description.to_string(),
            };
            ToolSpec {
                name: tool["name"].as_str().unwrap_or_default().to_string(),
                description,
                input_schema: schema,
            }
        })
        .chain(std::iter::once(ToolSpec {
            name: "ocs_memory".into(),
            description: "Read and write your persistent memory directory (agent/memory beside the application): list notes and session transcripts, read one, write/append/delete a note. Save durable facts (preferences, drawing conventions, how recurring tasks were done) and a short summary when a multi-step task finishes.".into(),
            input_schema: MemoryStore::tool_schema(),
        }))
        .collect()
}

/// The system prompt: the MCP instructions minus the session bootstrap, plus
/// what differs when the model lives inside the editor.
pub fn system_prompt() -> String {
    let base = crate::mcp::INSTRUCTIONS
        .replace(
            "Call ocs_sessions, then pass its session_id as ocs_session_id to ocs_read, ocs_execute and ocs_capture. ",
            "",
        )
        .replace(
            " When you first connect, announce the build you are working with to the user from the `bridge` object on ocs_sessions states and hello/capabilities responses (OpenCADStudio version, build_rev, tool_schema digest); repeat the announcement if a later handshake reports a different build.",
            "",
        );
    format!(
        "You are the AI assistant built into OpenCADStudio, a DWG/DXF CAD application. \
         You are talking to the person who has the drawing open in front of them, in the chat panel docked beside the drawing. \
         Use the ocs_read, ocs_execute and ocs_capture tools to inspect and change their drawing; the session is already bound, so no session id is needed. \
         Answer in the language the person writes in. Keep replies short and concrete; describe what you changed (entity types, layers, coordinates) rather than restating tool output. \
         Before a destructive or large change, confirm with the person unless they already asked for it explicitly. \
         When the person refers to \"this\" or \"the selected\" objects, read the current selection from state first; when you need them to pick objects or points, use user_select or getpoint and wait.\n\n{base}"
    )
}

fn clip(text: String) -> String {
    if text.len() <= MAX_TOOL_TEXT {
        return text;
    }
    let cut = text.floor_char_boundary(MAX_TOOL_TEXT);
    format!(
        "{}\n…[truncated {} bytes; narrow the query with limit, offset, fields or filters]",
        &text[..cut],
        text.len() - cut
    )
}

impl Session {
    fn emit(&mut self, event: AssistantEvent) {
        self.remember(&event);
        // The transcript in cad.log: every event the panel sees, once.
        match &event {
            AssistantEvent::Ready => log::info!("assistant worker ready"),
            AssistantEvent::TurnStarted => {}
            AssistantEvent::AssistantText(text) => {
                log::info!("assistant: {}", crate::applog::preview(text, 2000));
            }
            AssistantEvent::ToolCall { id, name, input } => log::info!(
                "tool call {id}: {name} {}",
                crate::applog::preview(&input.to_string(), 1000)
            ),
            AssistantEvent::ToolResult { id, ok, text, image_png } => log::info!(
                "tool result {id}: ok={ok} {} bytes{}: {}",
                text.len(),
                image_png
                    .as_ref()
                    .map(|png| format!(" + image {} bytes", png.len()))
                    .unwrap_or_default(),
                crate::applog::preview(text, 500)
            ),
            AssistantEvent::Usage(usage) => log::debug!(
                "usage so far: in={} out={}",
                usage.input_tokens,
                usage.output_tokens
            ),
            AssistantEvent::TurnFinished(reason) => log::info!("turn finished: {reason}"),
            AssistantEvent::Error(error) => log::error!("{error}"),
            AssistantEvent::Control(envelope) => log::debug!(
                "gui request: {}",
                crate::applog::preview(&envelope.request.to_string(), 500)
            ),
        }
        let _ = pollster::block_on(self.events.send(event));
    }

    /// Mirror the conversation into the session transcript under
    /// `agent/memory/sessions`, as it happens.
    fn remember(&mut self, event: &AssistantEvent) {
        let Some(memory) = self.memory.as_mut() else { return };
        match event {
            AssistantEvent::AssistantText(text) => memory.record("Assistant", text),
            AssistantEvent::ToolCall { id, name, input } => memory.record(
                "Tool call",
                &format!(
                    "`{name}` ({id})\n\n```json\n{}\n```",
                    crate::applog::preview(&input.to_string(), 2000)
                ),
            ),
            AssistantEvent::ToolResult { id, ok, text, image_png } => memory.record(
                "Tool result",
                &format!(
                    "{id}: {}{}\n\n```\n{}\n```",
                    if *ok { "ok" } else { "FAILED" },
                    image_png
                        .as_ref()
                        .map(|png| format!(", image {} bytes", png.len()))
                        .unwrap_or_default(),
                    crate::applog::preview(text, 1500)
                ),
            ),
            AssistantEvent::Error(error) => memory.record("Error", error),
            AssistantEvent::TurnFinished(reason) => memory.record("Turn finished", reason),
            AssistantEvent::Ready
            | AssistantEvent::TurnStarted
            | AssistantEvent::Usage(_)
            | AssistantEvent::Control(_) => {}
        }
    }

    fn next_id(&mut self, prefix: &str) -> String {
        self.serial += 1;
        format!("{prefix}-{}-{}", std::process::id(), self.serial)
    }

    /// One synchronous exchange with the GUI dispatcher.
    fn exchange(&mut self, request: Value) -> Result<Value, String> {
        let (tx, rx) = mpsc::channel();
        self.emit(AssistantEvent::Control(Envelope {
            request,
            reply: Reply::Native(tx),
        }));
        rx.recv_timeout(GUI_EXCHANGE_TIMEOUT)
            .map_err(|_| "The editor did not answer the automation request in time".to_string())
    }

    /// Send one request with the fields the GUI requires, then poll a
    /// long-running answer until it settles, the wait ends or the user
    /// cancels. Mirrors `mcp::GuiClient::request`.
    fn gui_request(&mut self, request: Value, wait: Duration) -> Result<Value, String> {
        let mut object: Map<String, Value> = request
            .as_object()
            .cloned()
            .ok_or_else(|| "request must be an object".to_string())?;
        let op = object
            .get("op")
            .and_then(Value::as_str)
            .ok_or_else(|| "request must contain op".to_string())?
            .to_string();
        let is_read = crate::mcp::READ_OPS.contains(&op.as_str());
        if !is_read || op == "capture" {
            if !object.contains_key("request_id") {
                let id = self.next_id("ai");
                object.insert("request_id".into(), Value::String(id));
            }
        }
        if op == "capture" {
            insert_default(&mut object, "document_id", self.state["document_id"].clone());
        }
        if !is_read {
            insert_default(&mut object, "client_id", Value::String(self.client_id.clone()));
            if op != "entities_copy_to" {
                insert_default(&mut object, "document_id", self.state["document_id"].clone());
            }
            insert_default(&mut object, "revision", self.state["revision"].clone());
            if ["input", "property", "run", "action", "save", "save_verified", "undo", "redo"]
                .contains(&op.as_str())
            {
                insert_default(&mut object, "selection", self.state["selection"].clone());
            }
        }
        let request_id = object.get("request_id").cloned();
        let mut response = self.exchange(Value::Object(object))?;
        let interactive = matches!(op.as_str(), "user_select" | "getpoint");
        let deadline = Instant::now() + if interactive { INTERACTIVE_MAX_WAIT.max(wait) } else { wait };
        while matches!(response["status"].as_str(), Some("accepted" | "running"))
            && Instant::now() < deadline
        {
            let Some(request_id) = request_id.clone() else { break };
            if cancelled() {
                self.dismiss();
                return Ok(json!({
                    "ok": false,
                    "status": "cancelled",
                    "request_id": request_id,
                    "error": "cancelled by the user"
                }));
            }
            std::thread::sleep(Duration::from_millis(60));
            response = self.exchange(json!({"op": "operation", "request_id": request_id}))?;
        }
        if interactive
            && matches!(response["status"].as_str(), Some("accepted" | "running"))
        {
            self.dismiss();
            return Err("timed out waiting for the user (10 min); the prompt was dismissed".into());
        }
        if response.get("state").is_some() {
            self.state = response["state"].clone();
        } else if matches!(op.as_str(), "hello" | "state") && response["ok"] == true {
            self.state = response.clone();
        }
        Ok(response)
    }

    /// Dismiss a pending interactive prompt in the GUI (best effort).
    fn dismiss(&mut self) {
        let cancel = json!({
            "op": "cancel",
            "request_id": self.next_id("ai-cancel"),
            "client_id": self.client_id,
            "document_id": self.state["document_id"],
            "revision": self.state["revision"],
        });
        let _ = self.exchange(cancel);
    }

    fn refresh_state(&mut self) -> Result<(), String> {
        let state = self.exchange(json!({"op": "hello"}))?;
        if state["ok"] != true {
            return Err(state["error"]
                .as_str()
                .unwrap_or("the editor refused the handshake")
                .to_string());
        }
        self.state = state;
        Ok(())
    }

    fn run_turn(&mut self, settings: &AssistantSettings, text: &str) {
        self.emit(AssistantEvent::TurnStarted);
        let chat = settings.active().clone();
        if let Err(error) = chat.validate() {
            self.emit(AssistantEvent::Error(error));
            self.emit(AssistantEvent::TurnFinished("error".into()));
            return;
        }
        let Some(api_key) = chat.resolved_api_key() else {
            self.emit(AssistantEvent::Error(format!(
                "No API key for {:?}: enter one in the assistant settings or set {}",
                chat.display_name(),
                chat.provider.env_key()
            )));
            self.emit(AssistantEvent::TurnFinished("error".into()));
            return;
        };
        if let Err(error) = self.refresh_state() {
            self.emit(AssistantEvent::Error(error));
            self.emit(AssistantEvent::TurnFinished("error".into()));
            return;
        }
        let tools = tool_specs();
        let mut system = system_prompt();
        if let Some(memory) = self.memory.as_mut() {
            memory.record("User", text);
            system.push_str("\n\n");
            system.push_str(&memory.prompt_section());
        }
        log::info!(
            "turn start: profile={:?} provider={:?} model={} endpoint={} vision={} user: {}",
            chat.display_name(),
            chat.provider,
            chat.effective_model(),
            provider::endpoint(&chat),
            settings
                .vision()
                .map(|v| v.display_name())
                .unwrap_or_else(|| if chat.vision { "self".into() } else { "none".into() }),
            crate::applog::preview(text, 2000)
        );
        self.messages.push(provider::user_message(chat.provider, text));
        let rounds = settings.max_tool_rounds.max(1);
        for round in 0..rounds {
            if cancelled() {
                self.emit(AssistantEvent::TurnFinished("cancelled".into()));
                return;
            }
            let started = Instant::now();
            let response = match self.call_model_round(&chat, settings.max_tokens, &api_key, &system, &tools, round) {
                Ok(response) => response,
                Err(error) => {
                    self.emit(AssistantEvent::Error(error));
                    self.emit(AssistantEvent::TurnFinished("error".into()));
                    return;
                }
            };
            let turn = match provider::parse_response(chat.provider, &response) {
                Ok(turn) => turn,
                Err(error) => {
                    self.emit(AssistantEvent::Error(error));
                    self.emit(AssistantEvent::TurnFinished("error".into()));
                    return;
                }
            };
            log::info!(
                "model response round {round}: stop={:?} tokens in={} out={} tool_calls={} in {} ms",
                turn.stop,
                turn.usage.input_tokens,
                turn.usage.output_tokens,
                turn.tool_calls.len(),
                started.elapsed().as_millis()
            );
            self.messages.push(turn.message.clone());
            self.usage += turn.usage;
            self.emit(AssistantEvent::Usage(self.usage));
            if !turn.text.is_empty() {
                self.emit(AssistantEvent::AssistantText(turn.text.clone()));
            }
            match &turn.stop {
                StopReason::Refusal(reason) => {
                    self.emit(AssistantEvent::Error(format!("The model declined this request ({reason})")));
                    self.emit(AssistantEvent::TurnFinished("refusal".into()));
                    return;
                }
                StopReason::MaxTokens if turn.tool_calls.is_empty() => {
                    self.emit(AssistantEvent::Error(
                        "The reply hit the max_tokens limit; raise it in settings or ask for less at once".into(),
                    ));
                    self.emit(AssistantEvent::TurnFinished("max_tokens".into()));
                    return;
                }
                _ => {}
            }
            if turn.tool_calls.is_empty() {
                self.emit(AssistantEvent::TurnFinished("end_turn".into()));
                return;
            }
            let mut outcomes = self.run_tool_calls(&turn.tool_calls);
            if !chat.vision || self.images_unsupported {
                // The chat model cannot look at the capture itself: a vision
                // profile describes it, or the metadata alone has to do.
                let vision = settings.vision().cloned();
                for outcome in &mut outcomes {
                    let Some(png) = outcome.image_png.take() else { continue };
                    let described = vision.as_ref().and_then(|v| {
                        match self.describe_image(v, settings.max_tokens, &png, outcome.vision_question.as_deref(), &outcome.text) {
                            Ok(description) => Some(format!(
                                "\n\nDescription of the capture by the vision model ({}):\n{description}",
                                v.display_name()
                            )),
                            Err(error) => {
                                log::warn!("vision model failed: {error}");
                                self.emit(AssistantEvent::Error(format!("Vision model {}: {error}", v.display_name())));
                                None
                            }
                        }
                    });
                    match described {
                        Some(text) => outcome.text.push_str(&text),
                        None => {
                            outcome.text.push('\n');
                            outcome.text.push_str(provider::IMAGE_UNSUPPORTED_NOTE);
                        }
                    }
                }
            }
            self.messages
                .extend(provider::tool_result_messages(chat.provider, &outcomes));
            if cancelled() {
                self.emit(AssistantEvent::TurnFinished("cancelled".into()));
                return;
            }
        }
        self.emit(AssistantEvent::Error(format!(
            "Stopped after {rounds} tool rounds; send another message to continue"
        )));
        self.emit(AssistantEvent::TurnFinished("max_rounds".into()));
    }

    /// One model call for `round`. An OpenAI-compatible endpoint that
    /// answers 400 while the history carries an image is treated as
    /// text-only: the images are replaced by a note and the call retried
    /// once, so a capture never ends the conversation.
    fn call_model_round(
        &mut self,
        chat: &ModelProfile,
        max_tokens: u32,
        api_key: &str,
        system: &str,
        tools: &[ToolSpec],
        round: u32,
    ) -> Result<Value, String> {
        let body = provider::build_request(chat, max_tokens, system, &self.messages, tools);
        log::info!(
            "model request round {round}: {} messages, {} bytes",
            self.messages.len(),
            body.to_string().len()
        );
        match call_model(chat, api_key, &body) {
            Ok(response) => Ok(response),
            Err(error)
                if chat.provider == Provider::OpenAiCompatible
                    && !self.images_unsupported
                    && error.starts_with("HTTP 400")
                    && provider::strip_images(&mut self.messages) > 0 =>
            {
                self.images_unsupported = true;
                log::warn!(
                    "model endpoint rejected image content ({error}); retrying round {round} with captures as text only"
                );
                let body = provider::build_request(chat, max_tokens, system, &self.messages, tools);
                call_model(chat, api_key, &body)
            }
            Err(error) => Err(error),
        }
    }

    /// Ask the vision profile to describe a capture for the chat model.
    fn describe_image(
        &mut self,
        vision: &ModelProfile,
        max_tokens: u32,
        png: &[u8],
        question: Option<&str>,
        metadata: &str,
    ) -> Result<String, String> {
        vision.validate()?;
        let api_key = vision
            .resolved_api_key()
            .ok_or_else(|| format!("no API key (set one or {})", vision.provider.env_key()))?;
        let annotations = annotation_summary(metadata);
        let prompt = format!(
            "You are describing a screenshot of a CAD drawing viewport for another assistant that cannot see images. \
             Describe precisely what is drawn: the shapes, how they are arranged, text labels, apparent proportions and \
             dimensions, and anything that looks wrong (overlaps, gaps, misplaced or unreadable text, parts outside the view). \
             Numbered Set-of-Marks tags, when present, identify entities; refer to them by tag and handle. \
             Be concrete and concise; use the same language as the question when one is given.{}{}",
            question
                .filter(|q| !q.trim().is_empty())
                .map(|q| format!("\n\nQuestion from the assistant: {q}"))
                .unwrap_or_default(),
            if annotations.is_empty() { String::new() } else { format!("\n\nEntities in view (tag: handle type layer):\n{annotations}") }
        );
        let messages = provider::vision_messages(vision.provider, &prompt, png);
        let body = provider::build_request(
            vision,
            max_tokens.clamp(256, 4096),
            "You describe CAD viewport images accurately for a text-only assistant.",
            &messages,
            &[],
        );
        let started = Instant::now();
        log::info!(
            "vision request: profile={:?} model={} image {} bytes",
            vision.display_name(),
            vision.effective_model(),
            png.len()
        );
        let response = call_model(vision, &api_key, &body)?;
        let turn = provider::parse_response(vision.provider, &response)?;
        log::info!(
            "vision response: tokens in={} out={} in {} ms: {}",
            turn.usage.input_tokens,
            turn.usage.output_tokens,
            started.elapsed().as_millis(),
            crate::applog::preview(&turn.text, 500)
        );
        if let Some(memory) = self.memory.as_mut() {
            memory.record("Vision", &turn.text);
        }
        if turn.text.trim().is_empty() {
            return Err("the vision model returned no text".into());
        }
        Ok(turn.text)
    }

    fn run_tool_calls(&mut self, calls: &[ToolCall]) -> Vec<ToolOutcome> {
        let mut outcomes = Vec::with_capacity(calls.len());
        for call in calls {
            self.emit(AssistantEvent::ToolCall {
                id: call.id.clone(),
                name: call.name.clone(),
                input: call.input.clone(),
            });
            let outcome = if cancelled() {
                ToolOutcome {
                    call_id: call.id.clone(),
                    text: "cancelled by the user before this tool ran".into(),
                    is_error: true,
                    image_png: None,
                    vision_question: None,
                }
            } else {
                match self.execute_tool(call) {
                    Ok(outcome) => outcome,
                    Err(error) => ToolOutcome {
                        call_id: call.id.clone(),
                        text: error,
                        is_error: true,
                        image_png: None,
                    vision_question: None,
                    },
                }
            };
            self.emit(AssistantEvent::ToolResult {
                id: outcome.call_id.clone(),
                ok: !outcome.is_error,
                text: outcome.text.clone(),
                image_png: outcome.image_png.clone().map(Arc::new),
            });
            outcomes.push(outcome);
        }
        outcomes
    }

    fn execute_tool(&mut self, call: &ToolCall) -> Result<ToolOutcome, String> {
        if call.input.is_null() {
            return Err("tool arguments were not valid JSON; send the arguments again".into());
        }
        let arguments = &call.input;
        let result = match call.name.as_str() {
            "ocs_read" => self.tool_read(arguments)?,
            "ocs_execute" => self.tool_execute(arguments)?,
            "ocs_capture" => return self.tool_capture(call, arguments),
            "ocs_memory" => {
                let memory = self
                    .memory
                    .as_ref()
                    .ok_or_else(|| "the memory directory is unavailable (no writable agent/memory folder)".to_string())?;
                memory.call(arguments)?
            }
            other => return Err(format!("unknown tool {other}; use ocs_read, ocs_execute, ocs_capture or ocs_memory")),
        };
        let is_error = result["ok"] == false;
        Ok(ToolOutcome {
            call_id: call.id.clone(),
            text: clip(result.to_string()),
            is_error,
            image_png: None,
                    vision_question: None,
        })
    }

    fn tool_read(&mut self, arguments: &Value) -> Result<Value, String> {
        let op = arguments["op"].as_str().unwrap_or("state");
        if !crate::mcp::READ_OPS.contains(&op) {
            return Err("Use ocs_execute for mutations".into());
        }
        if op == "capture" {
            return Err("Viewport captures run through the ocs_capture tool".into());
        }
        if op == "tools" {
            let specs = tool_specs();
            return Ok(json!({
                "ok": true,
                "status": "completed",
                "tools": specs.iter().map(|s| json!({"name": s.name, "description": s.description, "input_schema": s.input_schema})).collect::<Vec<_>>(),
            }));
        }
        let mut request = arguments["parameters"].as_object().cloned().unwrap_or_default();
        request.insert("op".into(), Value::String(op.into()));
        self.gui_request(Value::Object(request), Duration::from_secs(30))
    }

    fn tool_execute(&mut self, arguments: &Value) -> Result<Value, String> {
        let request = arguments["request"]
            .as_object()
            .cloned()
            .ok_or_else(|| "ocs_execute needs a request object with op".to_string())?;
        let op = request
            .get("op")
            .and_then(Value::as_str)
            .ok_or_else(|| "request must contain op".to_string())?
            .to_string();
        if crate::mcp::READ_OPS.contains(&op.as_str()) {
            return Err(format!("{op} is a read operation; use ocs_read"));
        }
        let request = Value::Object(request);
        let validation = crate::mcp::validate_execute_request(&request, &op)?;
        let wait = Duration::from_secs_f64(arguments["wait_seconds"].as_f64().unwrap_or(30.0).clamp(0.0, 60.0));
        let detail = arguments["response_detail"].as_str().unwrap_or("compact");
        let mut response = if op == "batch" {
            self.execute_batch(request, wait)?
        } else {
            self.gui_request(request, wait)?
        };
        if detail == "changed_entities" {
            let handles = crate::mcp::response_handles(&response);
            if !handles.is_empty() {
                let entities = self.gui_request(
                    json!({"op": "query", "handles": handles, "detail": "geometry", "limit": crate::mcp::MAX_BATCH_STEPS * 100}),
                    Duration::from_secs(30),
                )?;
                if let Some(object) = response.as_object_mut() {
                    object.insert("changed_entities".into(), entities["entities"].clone());
                }
            }
        }
        if detail != "full" {
            if let Some(state) = response.get("state").cloned() {
                if let Some(object) = response.as_object_mut() {
                    object.insert("state".into(), crate::mcp::compact_state(&state));
                }
            }
        }
        if !validation.warnings.is_empty() {
            if let Some(object) = response.as_object_mut() {
                object.insert("warnings".into(), json!(validation.warnings));
            }
        }
        Ok(response)
    }

    /// Run batch steps one after another, stopping at the first failure or
    /// at a prompt the next step does not answer. The whole batch shares the
    /// caller's wait budget.
    fn execute_batch(&mut self, request: Value, wait: Duration) -> Result<Value, String> {
        let id = request["request_id"]
            .as_str()
            .map(str::to_owned)
            .unwrap_or_else(|| self.next_id("ai-batch"));
        let steps = request["steps"]
            .as_array()
            .cloned()
            .ok_or_else(|| "batch requires a steps array".to_string())?;
        let deadline = Instant::now() + wait;
        let mut results = Vec::new();
        let mut changes = Vec::new();
        let mut state = Value::Null;
        let mut status = "completed";
        let mut ok = true;
        let mut next = 0;
        while next < steps.len() {
            if cancelled() {
                status = "cancelled";
                ok = false;
                break;
            }
            let mut step = steps[next]
                .as_object()
                .cloned()
                .ok_or_else(|| format!("batch step {next} must be an object"))?;
            for key in ["revision", "geometry_revision", "camera_revision", "selection", "client_id"] {
                step.remove(key);
            }
            step.insert("request_id".into(), Value::String(format!("{id}-{next}")));
            let remaining = deadline.saturating_duration_since(Instant::now());
            let response = self.gui_request(Value::Object(step), remaining)?;
            if matches!(response["status"].as_str(), Some("accepted" | "running")) {
                status = "running";
                break;
            }
            if let Some(s) = response.get("state") {
                state = s.clone();
            }
            if let Some(list) = response["changes"].as_array() {
                changes.extend(list.iter().cloned().map(|mut change| {
                    if let Some(object) = change.as_object_mut() {
                        object.insert("step".into(), Value::from(next));
                    }
                    change
                }));
            }
            let mut compact = response.clone();
            if let Some(object) = compact.as_object_mut() {
                object.remove("state");
                object.remove("changes");
                object.insert("step".into(), Value::from(next));
                object.insert("op".into(), steps[next]["op"].clone());
            }
            results.push(compact);
            next += 1;
            if response["ok"] == false || matches!(response["status"].as_str(), Some("failed" | "cancelled")) {
                status = if response["status"] == "cancelled" { "cancelled" } else { "failed" };
                ok = false;
                break;
            }
            if response["status"] == "waiting_input"
                && steps
                    .get(next)
                    .and_then(|s| s["op"].as_str())
                    .is_none_or(|op| !matches!(op, "input" | "cancel"))
            {
                status = "waiting_input";
                break;
            }
        }
        if status == "completed" && !state["command"].is_null() {
            status = "waiting_input";
        }
        Ok(json!({
            "ok": ok,
            "status": status,
            "request_id": id,
            "completed_steps": next,
            "total_steps": steps.len(),
            "next_step": (next < steps.len()).then_some(next),
            "results": results,
            "changes": changes,
            "state": state,
        }))
    }

    fn tool_capture(&mut self, call: &ToolCall, arguments: &Value) -> Result<ToolOutcome, String> {
        let path = std::env::temp_dir().join(format!(
            "ocs-assistant-capture-{}-{}.png",
            std::process::id(),
            self.serial
        ));
        let mut request = json!({
            "op": "capture",
            "path": path.to_string_lossy(),
            "scope": arguments["scope"].as_str().unwrap_or("viewport"),
            "max_dimension": arguments["max_dimension"].as_u64().unwrap_or(1600).clamp(256, 4096),
        });
        for key in ["view", "bounds", "focus_handles", "highlight_handles", "annotate"] {
            if let Some(value) = arguments.get(key) {
                if !value.is_null() {
                    request[key] = value.clone();
                }
            }
        }
        let mut result = self.gui_request(request.clone(), Duration::from_secs(30))?;
        // The first frame after a camera change (view: extents) can come
        // back empty on some drivers; one short retry covers it.
        if result["ok"] != true
            && result["error"]
                .as_str()
                .is_some_and(|e| e.contains("minimized or has no size"))
        {
            log::info!("capture returned no frame; retrying once");
            std::thread::sleep(Duration::from_millis(300));
            let mut retry = request;
            retry["request_id"] = Value::String(self.next_id("ai"));
            result = self.gui_request(retry, Duration::from_secs(30))?;
        }
        if result["ok"] != true || result["status"] != "completed" {
            let _ = std::fs::remove_file(&path);
            return Ok(ToolOutcome {
                call_id: call.id.clone(),
                text: clip(result.to_string()),
                is_error: true,
                image_png: None,
                    vision_question: None,
            });
        }
        let bytes = std::fs::read(&path).map_err(|error| format!("capture file unreadable: {error}"))?;
        let _ = std::fs::remove_file(&path);
        let mut meta = result.get("result").cloned().unwrap_or_else(|| json!({}));
        if let Some(object) = meta.as_object_mut() {
            object.remove("path");
        }
        Ok(ToolOutcome {
            call_id: call.id.clone(),
            text: clip(meta.to_string()),
            is_error: false,
            image_png: Some(bytes),
            vision_question: arguments["question"].as_str().map(str::to_owned),
        })
    }
}

/// `tag: handle type layer` lines from a capture's `_spatial.annotations`,
/// so a vision model can name what it sees.
fn annotation_summary(metadata: &str) -> String {
    let Ok(meta) = serde_json::from_str::<Value>(metadata) else {
        return String::new();
    };
    meta["_spatial"]["annotations"]
        .as_array()
        .map(|items| {
            items
                .iter()
                .take(80)
                .filter_map(|a| {
                    Some(format!(
                        "{}: {} {} {}",
                        a["tag"].as_u64()?,
                        a["handle"].as_str().unwrap_or("?"),
                        a["type"].as_str().unwrap_or("?"),
                        a["layer"].as_str().unwrap_or("")
                    ))
                })
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default()
}

fn insert_default(object: &mut Map<String, Value>, key: &str, value: Value) {
    if !value.is_null() && !object.contains_key(key) {
        object.insert(key.into(), value);
    }
}

/// One HTTP round trip to the model.
fn call_model(profile: &ModelProfile, api_key: &str, body: &Value) -> Result<Value, String> {
    let agent = crate::network::agent_keeping_status_bodies(LLM_TIMEOUT);
    let endpoint = provider::endpoint(profile);
    let mut request = agent.post(&endpoint);
    for (name, value) in provider::headers(profile, api_key) {
        request = request.header(name, value.as_str());
    }
    let payload = serde_json::to_string(body).map_err(|error| error.to_string())?;
    let mut response = request
        .send(payload.as_str())
        .map_err(|error| format!("request to {endpoint} failed: {error}"))?;
    let status = response.status().as_u16();
    let text = response
        .body_mut()
        .read_to_string()
        .map_err(|error| format!("the model's reply could not be read: {error}"))?;
    if !(200..300).contains(&status) {
        return Err(provider::error_message(status, &text));
    }
    serde_json::from_str(&text).map_err(|error| format!("the model returned unreadable JSON: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_specs_drop_session_plumbing_but_keep_the_three_tools() {
        let specs = tool_specs();
        let names: Vec<&str> = specs.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["ocs_read", "ocs_execute", "ocs_capture", "ocs_memory"]);
        let memory = specs.iter().find(|s| s.name == "ocs_memory").unwrap();
        assert_eq!(memory.input_schema["required"], json!(["op"]));
        for spec in &specs {
            assert!(spec.input_schema["properties"].get("ocs_session_id").is_none(), "{}", spec.name);
            let required = spec.input_schema["required"].as_array();
            assert!(required.is_none_or(|r| !r.iter().any(|v| v == "ocs_session_id")));
        }
        let execute = specs.iter().find(|s| s.name == "ocs_execute").unwrap();
        assert_eq!(execute.input_schema["required"], json!(["request"]));
        let capture = specs.iter().find(|s| s.name == "ocs_capture").unwrap();
        for gone in ["delivery", "tile", "diff", "pyramid_manifest"] {
            assert!(capture.input_schema["properties"].get(gone).is_none(), "{gone}");
        }
        assert!(capture.input_schema["properties"].get("annotate").is_some());
    }

    #[test]
    fn system_prompt_drops_session_bootstrap_and_keeps_guidance() {
        let prompt = system_prompt();
        assert!(!prompt.contains("Call ocs_sessions"));
        assert!(!prompt.contains("announce the build"));
        assert!(prompt.contains("record_schema"));
        assert!(prompt.contains("built into OpenCADStudio"));
    }

    #[test]
    fn clip_marks_truncation_on_a_char_boundary() {
        let short = clip("abc".into());
        assert_eq!(short, "abc");
        let long = clip("é".repeat(MAX_TOOL_TEXT));
        assert!(long.contains("[truncated"));
        assert!(long.len() < MAX_TOOL_TEXT + 200);
    }

    #[test]
    fn submit_before_the_worker_starts_is_a_clean_error() {
        if let Ok(mut guard) = inbox().lock() {
            *guard = None;
        }
        assert!(submit(AgentCommand::Reset).is_err());
    }

    /// A fake GUI: answers envelopes from the worker thread the way
    /// `control_request` would, so request shaping and polling are covered.
    fn session_with_fake_gui(
        script: impl Fn(&Value, &mut u32) -> Value + Send + 'static,
    ) -> (Session, std::thread::JoinHandle<Vec<Value>>) {
        let (tx, mut rx) = fmpsc::channel::<AssistantEvent>(64);
        let gui = std::thread::spawn(move || {
            let mut seen = Vec::new();
            let mut polls = 0;
            while let Some(event) = pollster::block_on(async {
                use iced::futures::StreamExt;
                rx.next().await
            }) {
                if let AssistantEvent::Control(envelope) = event {
                    let response = script(&envelope.request, &mut polls);
                    seen.push(envelope.request);
                    envelope.reply.send(response);
                }
            }
            seen
        });
        let session = Session {
            provider: Provider::Anthropic,
            messages: Vec::new(),
            state: json!({"document_id": 7, "revision": 3, "selection": ["2A"]}),
            client_id: "test-client".into(),
            usage: Usage::default(),
            serial: 0,
            events: tx,
            memory: MemoryStore::open_default(),
            images_unsupported: false,
        };
        (session, gui)
    }

    #[test]
    fn mutations_get_ids_and_state_fields_and_are_polled_to_completion() {
        let (mut session, gui) = session_with_fake_gui(|request, polls| {
            match request["op"].as_str() {
                Some("run") => json!({"ok": true, "status": "accepted", "request_id": request["request_id"]}),
                Some("operation") => {
                    *polls += 1;
                    if *polls < 2 {
                        json!({"ok": true, "status": "running"})
                    } else {
                        json!({"ok": true, "status": "completed", "state": {"document_id": 7, "revision": 4, "selection": []}})
                    }
                }
                _ => json!({"ok": false, "status": "failed", "error": "unexpected"}),
            }
        });
        let response = session
            .gui_request(json!({"op": "run", "cmd": "LINE 0,0 10,0"}), Duration::from_secs(5))
            .unwrap();
        assert_eq!(response["status"], "completed");
        assert_eq!(session.state["revision"], 4);
        drop(session);
        let seen = gui.join().unwrap();
        assert_eq!(seen[0]["op"], "run");
        assert_eq!(seen[0]["document_id"], 7);
        assert_eq!(seen[0]["revision"], 3);
        assert_eq!(seen[0]["selection"], json!(["2A"]));
        assert_eq!(seen[0]["client_id"], "test-client");
        assert!(seen[0]["request_id"].as_str().unwrap().starts_with("ai-"));
        assert_eq!(seen[1]["op"], "operation");
        assert_eq!(seen[2]["op"], "operation");
    }

    #[test]
    fn reads_carry_no_envelope_fields_and_execute_rejects_reads() {
        let (mut session, gui) = session_with_fake_gui(|request, _| {
            json!({"ok": true, "status": "completed", "echo": request.clone()})
        });
        let read = session
            .tool_read(&json!({"op": "query", "parameters": {"type": "LINE", "limit": 5}}))
            .unwrap();
        assert_eq!(read["echo"]["op"], "query");
        assert_eq!(read["echo"]["type"], "LINE");
        assert!(read["echo"].get("request_id").is_none());
        assert!(read["echo"].get("document_id").is_none());
        let err = session.tool_execute(&json!({"request": {"op": "state"}})).unwrap_err();
        assert!(err.contains("read operation"));
        let err = session.tool_execute(&json!({"request": {"op": "run"}})).unwrap_err();
        assert!(err.contains("Missing cmd"), "{err}");
        drop(session);
        gui.join().unwrap();
    }

    #[test]
    fn memory_tool_writes_notes_and_the_prompt_carries_the_index() {
        let (mut session, gui) = session_with_fake_gui(|_, _| json!({"ok": true}));
        let call = ToolCall {
            id: "m1".into(),
            name: "ocs_memory".into(),
            input: json!({"op": "write", "name": "Drawing conventions", "description": "layers in use", "content": "# Layers\n\nWalls on A-WALL."}),
        };
        let outcome = session.execute_tool(&call).unwrap();
        assert!(!outcome.is_error, "{}", outcome.text);
        assert!(outcome.text.contains("notes/drawing-conventions.md"));
        {
            let memory = session.memory.as_ref().unwrap();
            assert!(memory.read_note("drawing-conventions").unwrap().contains("A-WALL"));
            assert!(memory.prompt_section().contains("layers in use"));
        }
        let bad = ToolCall { id: "m2".into(), name: "ocs_memory".into(), input: json!({"op": "read", "name": "nope"}) };
        assert!(session.execute_tool(&bad).is_err());
        let _ = session.memory.as_ref().unwrap().delete_note("drawing-conventions");
        drop(session);
        gui.join().unwrap();
    }

    #[test]
    fn batch_runs_steps_in_order_and_stops_on_failure() {
        let (mut session, gui) = session_with_fake_gui(|request, _| {
            match request["cmd"].as_str() {
                Some("BAD") => json!({"ok": false, "status": "failed", "error": "nope", "request_id": request["request_id"]}),
                _ => json!({"ok": true, "status": "completed", "request_id": request["request_id"], "changes": [{"handle": "2B"}], "state": {"document_id": 7, "revision": 9, "command": null}}),
            }
        });
        let result = session
            .tool_execute(&json!({"request": {"op": "batch", "request_id": "b1", "steps": [
                {"op": "run", "cmd": "LINE 0,0 1,0"},
                {"op": "run", "cmd": "BAD"},
                {"op": "run", "cmd": "NEVER"}
            ]}}))
            .unwrap();
        assert_eq!(result["ok"], false);
        assert_eq!(result["status"], "failed");
        assert_eq!(result["completed_steps"], 2);
        assert_eq!(result["results"].as_array().unwrap().len(), 2);
        assert_eq!(result["results"][0]["step"], 0);
        assert_eq!(result["changes"][0]["step"], 0);
        drop(session);
        let seen = gui.join().unwrap();
        assert_eq!(seen[0]["request_id"], "b1-0");
        assert_eq!(seen[1]["request_id"], "b1-1");
        assert_eq!(seen.len(), 2);
    }
}
