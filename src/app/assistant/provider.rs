//! LLM provider adapters for the built-in assistant.
//!
//! Two wire formats are supported: the Anthropic Messages API (the default,
//! spoken natively over HTTP) and the OpenAI-compatible Chat Completions
//! shape that most local inference servers and model gateways expose. Every
//! function here is a pure transformation between JSON values so the request
//! and response shapes are covered by unit tests without a network.
//!
//! The assistant keeps one provider-native `messages` array per conversation;
//! switching provider starts a fresh conversation because the two formats are
//! not interchangeable (assistant turns are echoed back verbatim, thinking
//! blocks included, so the server sees exactly what it produced).

use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::fmt;

/// Which HTTP API the assistant talks to.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Provider {
    /// Anthropic Messages API (`POST {base}/v1/messages`).
    #[default]
    Anthropic,
    /// OpenAI-style Chat Completions (`POST {base}/chat/completions`): local
    /// inference servers, model gateways and other hosted vendors.
    OpenAiCompatible,
}

impl Provider {
    pub const ALL: [Provider; 2] = [Provider::Anthropic, Provider::OpenAiCompatible];

    pub fn label(self) -> &'static str {
        match self {
            Provider::Anthropic => "Anthropic (Claude)",
            Provider::OpenAiCompatible => "OpenAI-compatible",
        }
    }

    pub fn default_base_url(self) -> &'static str {
        match self {
            Provider::Anthropic => "https://api.anthropic.com",
            Provider::OpenAiCompatible => "https://api.openai.com/v1",
        }
    }

    pub fn default_model(self) -> &'static str {
        match self {
            Provider::Anthropic => "claude-opus-5-5",
            Provider::OpenAiCompatible => "",
        }
    }

    /// Environment variable consulted when no key is stored in settings.
    pub fn env_key(self) -> &'static str {
        match self {
            Provider::Anthropic => "ANTHROPIC_API_KEY",
            Provider::OpenAiCompatible => "OPENAI_API_KEY",
        }
    }
}

impl fmt::Display for Provider {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

/// Reasoning depth for the Anthropic API (`output_config.effort`). Ignored
/// by OpenAI-compatible servers.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Effort {
    /// Let the server pick its default.
    #[default]
    Default,
    Low,
    Medium,
    High,
    XHigh,
    Max,
}

impl Effort {
    pub const ALL: [Effort; 6] = [
        Effort::Default,
        Effort::Low,
        Effort::Medium,
        Effort::High,
        Effort::XHigh,
        Effort::Max,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Effort::Default => "Default",
            Effort::Low => "Low",
            Effort::Medium => "Medium",
            Effort::High => "High",
            Effort::XHigh => "Extra high",
            Effort::Max => "Max",
        }
    }

    /// The value sent on the wire; `None` leaves the parameter out.
    pub fn wire(self) -> Option<&'static str> {
        match self {
            Effort::Default => None,
            Effort::Low => Some("low"),
            Effort::Medium => Some("medium"),
            Effort::High => Some("high"),
            Effort::XHigh => Some("xhigh"),
            Effort::Max => Some("max"),
        }
    }
}

impl fmt::Display for Effort {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

/// Persisted assistant preferences (`UserSettings::assistant`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AssistantSettings {
    pub provider: Provider,
    /// Empty means the provider default.
    pub base_url: String,
    /// Empty means the provider default.
    pub model: String,
    /// Stored in the user's settings file; empty falls back to the
    /// provider's environment variable.
    pub api_key: String,
    pub effort: Effort,
    /// Tool-call rounds per user message before the assistant stops and asks.
    pub max_tool_rounds: u32,
    /// `max_tokens` per response.
    pub max_tokens: u32,
    /// Anthropic only: let the server re-run a declined request on a
    /// fallback model (`fallbacks: "default"`).
    pub refusal_fallback: bool,
}

impl Default for AssistantSettings {
    fn default() -> Self {
        Self {
            provider: Provider::Anthropic,
            base_url: String::new(),
            model: String::new(),
            api_key: String::new(),
            effort: Effort::Default,
            max_tool_rounds: 40,
            max_tokens: 16_000,
            refusal_fallback: true,
        }
    }
}

impl AssistantSettings {
    pub fn effective_base_url(&self) -> String {
        let raw = self.base_url.trim();
        let url = if raw.is_empty() {
            self.provider.default_base_url()
        } else {
            raw
        };
        url.trim_end_matches('/').to_string()
    }

    pub fn effective_model(&self) -> String {
        let raw = self.model.trim();
        if raw.is_empty() {
            self.provider.default_model().to_string()
        } else {
            raw.to_string()
        }
    }

    /// The key from settings, else the provider's environment variable.
    pub fn resolved_api_key(&self) -> Option<String> {
        let stored = self.api_key.trim();
        if !stored.is_empty() {
            return Some(stored.to_string());
        }
        std::env::var(self.provider.env_key())
            .ok()
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
    }

    /// The request can only be built when a model is known.
    pub fn validate(&self) -> Result<(), String> {
        if self.effective_model().is_empty() {
            return Err("Set a model name in the assistant settings".into());
        }
        let url = self.effective_base_url();
        if !(url.starts_with("http://") || url.starts_with("https://")) {
            return Err(format!("Base URL must start with http:// or https:// (got {url})"));
        }
        Ok(())
    }

    fn official_anthropic_host(&self) -> bool {
        self.provider == Provider::Anthropic
            && self
                .effective_base_url()
                .trim_start_matches("https://")
                .trim_start_matches("http://")
                .starts_with("api.anthropic.com")
    }
}

/// A tool exposed to the model: name, description and JSON Schema input.
#[derive(Clone, Debug, PartialEq)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
}

/// One tool invocation requested by the model.
#[derive(Clone, Debug, PartialEq)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    /// `Null` when the arguments were not valid JSON.
    pub input: Value,
}

#[derive(Clone, Debug, PartialEq)]
pub enum StopReason {
    EndTurn,
    ToolUse,
    MaxTokens,
    Refusal(String),
    Other(String),
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
}

impl std::ops::AddAssign for Usage {
    fn add_assign(&mut self, rhs: Self) {
        self.input_tokens += rhs.input_tokens;
        self.output_tokens += rhs.output_tokens;
    }
}

/// A parsed model response.
#[derive(Clone, Debug, PartialEq)]
pub struct Turn {
    /// All text blocks joined; may be empty when the model only calls tools.
    pub text: String,
    pub tool_calls: Vec<ToolCall>,
    pub stop: StopReason,
    pub usage: Usage,
    /// The provider-native assistant message to append to the history.
    pub message: Value,
}

/// The result of running one tool call.
#[derive(Clone, Debug, PartialEq)]
pub struct ToolOutcome {
    pub call_id: String,
    pub text: String,
    pub is_error: bool,
    /// PNG bytes for captures; shown to the model as an image.
    pub image_png: Option<Vec<u8>>,
}

pub fn endpoint(settings: &AssistantSettings) -> String {
    let base = settings.effective_base_url();
    match settings.provider {
        Provider::Anthropic => format!("{base}/v1/messages"),
        Provider::OpenAiCompatible => format!("{base}/chat/completions"),
    }
}

/// Models that accept `fallbacks: "default"` on the official API.
fn supports_default_fallback(model: &str) -> bool {
    model.starts_with("claude-opus-5")
        || model.starts_with("claude-fable-5")
        || model.starts_with("claude-sonnet-5-5")
}

fn uses_fallback(settings: &AssistantSettings) -> bool {
    settings.refusal_fallback
        && settings.official_anthropic_host()
        && supports_default_fallback(&settings.effective_model())
}

pub fn headers(settings: &AssistantSettings, api_key: &str) -> Vec<(&'static str, String)> {
    let mut out = vec![("content-type", "application/json".to_string())];
    match settings.provider {
        Provider::Anthropic => {
            out.push(("x-api-key", api_key.to_string()));
            out.push(("anthropic-version", "2023-06-01".to_string()));
            if uses_fallback(settings) {
                out.push(("anthropic-beta", "server-side-fallback-2026-07-01".to_string()));
            }
        }
        Provider::OpenAiCompatible => {
            out.push(("authorization", format!("Bearer {api_key}")));
        }
    }
    out
}

/// Build the request body for one model call.
pub fn build_request(
    settings: &AssistantSettings,
    system: &str,
    messages: &[Value],
    tools: &[ToolSpec],
) -> Value {
    match settings.provider {
        Provider::Anthropic => {
            let mut tool_values: Vec<Value> = tools
                .iter()
                .map(|t| {
                    json!({
                        "name": t.name,
                        "description": t.description,
                        "input_schema": t.input_schema,
                    })
                })
                .collect();
            // Cache breakpoints: tools → system → the conversation so far.
            if let Some(last) = tool_values.last_mut() {
                last["cache_control"] = json!({"type": "ephemeral"});
            }
            let mut messages: Vec<Value> = messages.to_vec();
            if let Some(last) = messages.last_mut() {
                mark_last_block_cached(last);
            }
            let mut body = json!({
                "model": settings.effective_model(),
                "max_tokens": settings.max_tokens.max(256),
                "system": [{
                    "type": "text",
                    "text": system,
                    "cache_control": {"type": "ephemeral"},
                }],
                "tools": tool_values,
                "messages": messages,
            });
            if let Some(effort) = settings.effort.wire() {
                body["output_config"] = json!({"effort": effort});
            }
            if uses_fallback(settings) {
                body["fallbacks"] = json!("default");
            }
            body
        }
        Provider::OpenAiCompatible => {
            let mut all = Vec::with_capacity(messages.len() + 1);
            all.push(json!({"role": "system", "content": system}));
            all.extend(messages.iter().cloned());
            let tool_values: Vec<Value> = tools
                .iter()
                .map(|t| {
                    json!({
                        "type": "function",
                        "function": {
                            "name": t.name,
                            "description": t.description,
                            "parameters": t.input_schema,
                        }
                    })
                })
                .collect();
            let mut body = json!({
                "model": settings.effective_model(),
                "messages": all,
                "max_tokens": settings.max_tokens.max(256),
            });
            if !tool_values.is_empty() {
                body["tools"] = Value::Array(tool_values);
                body["tool_choice"] = json!("auto");
            }
            body
        }
    }
}

/// Put a cache breakpoint on the last content block of a message, turning a
/// plain string body into a block list when needed.
fn mark_last_block_cached(message: &mut Value) {
    let Some(object) = message.as_object_mut() else { return };
    match object.get_mut("content") {
        Some(Value::Array(blocks)) => {
            if let Some(Value::Object(block)) = blocks.last_mut() {
                block.insert("cache_control".into(), json!({"type": "ephemeral"}));
            }
        }
        Some(Value::String(text)) => {
            let text = std::mem::take(text);
            object.insert(
                "content".into(),
                json!([{"type": "text", "text": text, "cache_control": {"type": "ephemeral"}}]),
            );
        }
        _ => {}
    }
}

pub fn user_message(provider: Provider, text: &str) -> Value {
    match provider {
        Provider::Anthropic => json!({"role": "user", "content": [{"type": "text", "text": text}]}),
        Provider::OpenAiCompatible => json!({"role": "user", "content": text}),
    }
}

fn png_data_url(bytes: &[u8]) -> String {
    use base64::Engine as _;
    format!(
        "data:image/png;base64,{}",
        base64::engine::general_purpose::STANDARD.encode(bytes)
    )
}

/// Messages carrying tool results back to the model. Anthropic takes every
/// result in one user message; the OpenAI shape wants one `tool` message per
/// call and cannot carry images there, so captures follow in a user message.
pub fn tool_result_messages(provider: Provider, outcomes: &[ToolOutcome]) -> Vec<Value> {
    use base64::Engine as _;
    match provider {
        Provider::Anthropic => {
            let blocks: Vec<Value> = outcomes
                .iter()
                .map(|o| {
                    let mut content = vec![json!({"type": "text", "text": o.text})];
                    if let Some(png) = &o.image_png {
                        content.push(json!({
                            "type": "image",
                            "source": {
                                "type": "base64",
                                "media_type": "image/png",
                                "data": base64::engine::general_purpose::STANDARD.encode(png),
                            }
                        }));
                    }
                    let mut block = json!({
                        "type": "tool_result",
                        "tool_use_id": o.call_id,
                        "content": content,
                    });
                    if o.is_error {
                        block["is_error"] = json!(true);
                    }
                    block
                })
                .collect();
            vec![json!({"role": "user", "content": blocks})]
        }
        Provider::OpenAiCompatible => {
            let mut out: Vec<Value> = outcomes
                .iter()
                .map(|o| {
                    json!({
                        "role": "tool",
                        "tool_call_id": o.call_id,
                        "content": o.text,
                    })
                })
                .collect();
            let images: Vec<&ToolOutcome> =
                outcomes.iter().filter(|o| o.image_png.is_some()).collect();
            if !images.is_empty() {
                let mut parts = vec![json!({
                    "type": "text",
                    "text": format!(
                        "Captured image(s) for tool call(s): {}",
                        images.iter().map(|o| o.call_id.as_str()).collect::<Vec<_>>().join(", ")
                    ),
                })];
                for o in images {
                    if let Some(png) = &o.image_png {
                        parts.push(json!({
                            "type": "image_url",
                            "image_url": {"url": png_data_url(png)},
                        }));
                    }
                }
                out.push(json!({"role": "user", "content": parts}));
            }
            out
        }
    }
}

/// Human-readable error from a non-success HTTP body.
pub fn error_message(status: u16, body: &str) -> String {
    let detail = serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|v| {
            v["error"]["message"]
                .as_str()
                .or_else(|| v["error"].as_str())
                .or_else(|| v["message"].as_str())
                .map(str::to_owned)
        })
        .unwrap_or_else(|| {
            let trimmed = body.trim();
            if trimmed.len() > 400 {
                format!("{}…", &trimmed[..trimmed.floor_char_boundary(400)])
            } else {
                trimmed.to_string()
            }
        });
    if detail.is_empty() {
        format!("HTTP {status}")
    } else {
        format!("HTTP {status}: {detail}")
    }
}

pub fn parse_response(provider: Provider, body: &Value) -> Result<Turn, String> {
    match provider {
        Provider::Anthropic => parse_anthropic(body),
        Provider::OpenAiCompatible => parse_openai(body),
    }
}

fn parse_anthropic(body: &Value) -> Result<Turn, String> {
    if body["type"] == "error" {
        return Err(body["error"]["message"]
            .as_str()
            .unwrap_or("unknown API error")
            .to_string());
    }
    let content = body["content"]
        .as_array()
        .ok_or_else(|| "response has no content array".to_string())?;
    let mut texts = Vec::new();
    let mut tool_calls = Vec::new();
    for block in content {
        match block["type"].as_str() {
            Some("text") => {
                if let Some(t) = block["text"].as_str() {
                    if !t.trim().is_empty() {
                        texts.push(t.to_string());
                    }
                }
            }
            Some("tool_use") => tool_calls.push(ToolCall {
                id: block["id"].as_str().unwrap_or_default().to_string(),
                name: block["name"].as_str().unwrap_or_default().to_string(),
                input: block.get("input").cloned().unwrap_or(Value::Null),
            }),
            _ => {}
        }
    }
    let stop = match body["stop_reason"].as_str() {
        Some("tool_use") => StopReason::ToolUse,
        Some("end_turn") | Some("stop_sequence") | None => StopReason::EndTurn,
        Some("max_tokens") => StopReason::MaxTokens,
        Some("refusal") => {
            let details = &body["stop_details"];
            let category = details["category"].as_str().unwrap_or("policy");
            let explanation = details["explanation"].as_str().unwrap_or("");
            StopReason::Refusal(if explanation.is_empty() {
                category.to_string()
            } else {
                format!("{category}: {explanation}")
            })
        }
        Some(other) => StopReason::Other(other.to_string()),
    };
    let stop = if stop == StopReason::EndTurn && !tool_calls.is_empty() {
        StopReason::ToolUse
    } else {
        stop
    };
    let usage = &body["usage"];
    let usage = Usage {
        input_tokens: usage["input_tokens"].as_u64().unwrap_or(0)
            + usage["cache_read_input_tokens"].as_u64().unwrap_or(0)
            + usage["cache_creation_input_tokens"].as_u64().unwrap_or(0),
        output_tokens: usage["output_tokens"].as_u64().unwrap_or(0),
    };
    Ok(Turn {
        text: texts.join("\n\n"),
        tool_calls,
        stop,
        usage,
        message: json!({"role": "assistant", "content": content}),
    })
}

fn parse_openai(body: &Value) -> Result<Turn, String> {
    if let Some(message) = body["error"]["message"].as_str() {
        return Err(message.to_string());
    }
    let choice = body["choices"]
        .as_array()
        .and_then(|c| c.first())
        .ok_or_else(|| "response has no choices".to_string())?;
    let message = &choice["message"];
    let text = match &message["content"] {
        Value::String(s) => s.clone(),
        Value::Array(parts) => parts
            .iter()
            .filter_map(|p| p["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    };
    let tool_calls: Vec<ToolCall> = message["tool_calls"]
        .as_array()
        .map(|calls| {
            calls
                .iter()
                .map(|call| {
                    let arguments = call["function"]["arguments"].clone();
                    let input = match arguments {
                        Value::String(s) if s.trim().is_empty() => json!({}),
                        Value::String(s) => serde_json::from_str(&s).unwrap_or(Value::Null),
                        Value::Object(o) => Value::Object(o),
                        _ => json!({}),
                    };
                    ToolCall {
                        id: call["id"].as_str().unwrap_or_default().to_string(),
                        name: call["function"]["name"]
                            .as_str()
                            .unwrap_or_default()
                            .to_string(),
                        input,
                    }
                })
                .collect()
        })
        .unwrap_or_default();
    let stop = match choice["finish_reason"].as_str() {
        _ if !tool_calls.is_empty() => StopReason::ToolUse,
        Some("stop") | None => StopReason::EndTurn,
        Some("length") => StopReason::MaxTokens,
        Some("content_filter") => StopReason::Refusal("content_filter".into()),
        Some(other) => StopReason::Other(other.to_string()),
    };
    let usage = &body["usage"];
    let usage = Usage {
        input_tokens: usage["prompt_tokens"].as_u64().unwrap_or(0),
        output_tokens: usage["completion_tokens"].as_u64().unwrap_or(0),
    };
    // Echo the assistant turn as the server produced it, minus vendor
    // reasoning fields that several servers reject when sent back.
    let mut echoed: Map<String, Value> = message.as_object().cloned().unwrap_or_default();
    echoed.remove("reasoning_content");
    echoed.remove("reasoning");
    echoed.entry("role").or_insert(json!("assistant"));
    if echoed.get("content").is_none_or(Value::is_null) {
        echoed.insert("content".into(), Value::String(String::new()));
    }
    Ok(Turn {
        text,
        tool_calls,
        stop,
        usage,
        message: Value::Object(echoed),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings(provider: Provider) -> AssistantSettings {
        AssistantSettings {
            provider,
            ..Default::default()
        }
    }

    fn tools() -> Vec<ToolSpec> {
        vec![ToolSpec {
            name: "ocs_read".into(),
            description: "Read state".into(),
            input_schema: json!({"type": "object", "properties": {"op": {"type": "string"}}}),
        }]
    }

    #[test]
    fn defaults_resolve_per_provider() {
        let s = settings(Provider::Anthropic);
        assert_eq!(s.effective_model(), "claude-opus-5-5");
        assert_eq!(endpoint(&s), "https://api.anthropic.com/v1/messages");
        let mut o = settings(Provider::OpenAiCompatible);
        o.base_url = "http://localhost:11434/v1/".into();
        o.model = "qwen3".into();
        assert_eq!(endpoint(&o), "http://localhost:11434/v1/chat/completions");
        assert!(settings(Provider::OpenAiCompatible).validate().is_err());
        assert!(o.validate().is_ok());
    }

    #[test]
    fn anthropic_request_has_cache_breakpoints_effort_and_fallback() {
        let mut s = settings(Provider::Anthropic);
        s.effort = Effort::High;
        let messages = vec![user_message(Provider::Anthropic, "draw a line")];
        let body = build_request(&s, "SYSTEM", &messages, &tools());
        assert_eq!(body["model"], "claude-opus-5-5");
        assert_eq!(body["system"][0]["cache_control"]["type"], "ephemeral");
        assert_eq!(body["tools"][0]["input_schema"]["type"], "object");
        assert_eq!(body["tools"][0]["cache_control"]["type"], "ephemeral");
        assert_eq!(body["messages"][0]["content"][0]["cache_control"]["type"], "ephemeral");
        assert_eq!(body["output_config"]["effort"], "high");
        assert_eq!(body["fallbacks"], "default");
        assert!(body.get("thinking").is_none(), "adaptive thinking is the server default");
        let headers = headers(&s, "sk-test");
        assert!(headers.iter().any(|(k, v)| *k == "x-api-key" && v == "sk-test"));
        assert!(headers
            .iter()
            .any(|(k, v)| *k == "anthropic-beta" && v == "server-side-fallback-2026-07-01"));
    }

    #[test]
    fn fallback_is_skipped_off_the_official_host_or_on_older_models() {
        let mut s = settings(Provider::Anthropic);
        s.base_url = "https://gateway.example.com".into();
        let body = build_request(&s, "S", &[], &[]);
        assert!(body.get("fallbacks").is_none());
        assert!(!headers(&s, "k").iter().any(|(k, _)| *k == "anthropic-beta"));
        let mut s = settings(Provider::Anthropic);
        s.model = "claude-haiku-4-5".into();
        assert!(build_request(&s, "S", &[], &[]).get("fallbacks").is_none());
    }

    #[test]
    fn openai_request_wraps_tools_as_functions() {
        let mut s = settings(Provider::OpenAiCompatible);
        s.model = "gpt-test".into();
        let messages = vec![user_message(Provider::OpenAiCompatible, "hi")];
        let body = build_request(&s, "SYSTEM", &messages, &tools());
        assert_eq!(body["messages"][0]["role"], "system");
        assert_eq!(body["messages"][1]["content"], "hi");
        assert_eq!(body["tools"][0]["type"], "function");
        assert_eq!(body["tools"][0]["function"]["name"], "ocs_read");
        assert_eq!(body["tool_choice"], "auto");
        let headers = headers(&s, "sk-x");
        assert!(headers.iter().any(|(k, v)| *k == "authorization" && v == "Bearer sk-x"));
    }

    #[test]
    fn anthropic_response_parses_text_tools_and_usage() {
        let body = json!({
            "type": "message",
            "stop_reason": "tool_use",
            "content": [
                {"type": "thinking", "thinking": "", "signature": "sig"},
                {"type": "text", "text": "Reading state."},
                {"type": "tool_use", "id": "toolu_1", "name": "ocs_read", "input": {"op": "state"}}
            ],
            "usage": {"input_tokens": 10, "cache_read_input_tokens": 90, "output_tokens": 5}
        });
        let turn = parse_response(Provider::Anthropic, &body).unwrap();
        assert_eq!(turn.text, "Reading state.");
        assert_eq!(turn.stop, StopReason::ToolUse);
        assert_eq!(turn.tool_calls.len(), 1);
        assert_eq!(turn.tool_calls[0].id, "toolu_1");
        assert_eq!(turn.tool_calls[0].input["op"], "state");
        assert_eq!(turn.usage, Usage { input_tokens: 100, output_tokens: 5 });
        // The echoed turn keeps the thinking block for the next request.
        assert_eq!(turn.message["content"][0]["type"], "thinking");
    }

    #[test]
    fn anthropic_refusal_and_errors_surface() {
        let body = json!({
            "type": "message",
            "stop_reason": "refusal",
            "stop_details": {"type": "refusal", "category": "cyber", "explanation": "declined"},
            "content": [],
            "usage": {}
        });
        let turn = parse_response(Provider::Anthropic, &body).unwrap();
        assert_eq!(turn.stop, StopReason::Refusal("cyber: declined".into()));
        let err = json!({"type": "error", "error": {"type": "authentication_error", "message": "bad key"}});
        assert_eq!(parse_response(Provider::Anthropic, &err).unwrap_err(), "bad key");
        assert_eq!(
            error_message(401, r#"{"error":{"message":"invalid x-api-key"}}"#),
            "HTTP 401: invalid x-api-key"
        );
    }

    #[test]
    fn openai_response_parses_tool_calls_and_strips_reasoning() {
        let body = json!({
            "choices": [{
                "finish_reason": "tool_calls",
                "message": {
                    "role": "assistant",
                    "content": null,
                    "reasoning_content": "secret",
                    "tool_calls": [{
                        "id": "call_1",
                        "type": "function",
                        "function": {"name": "ocs_execute", "arguments": "{\"request\":{\"op\":\"undo\"}}"}
                    }]
                }
            }],
            "usage": {"prompt_tokens": 7, "completion_tokens": 3}
        });
        let turn = parse_response(Provider::OpenAiCompatible, &body).unwrap();
        assert_eq!(turn.stop, StopReason::ToolUse);
        assert_eq!(turn.tool_calls[0].name, "ocs_execute");
        assert_eq!(turn.tool_calls[0].input["request"]["op"], "undo");
        assert_eq!(turn.usage, Usage { input_tokens: 7, output_tokens: 3 });
        assert!(turn.message.get("reasoning_content").is_none());
        assert_eq!(turn.message["content"], "");
        let bad = json!({"choices": [{"finish_reason": "stop", "message": {"role": "assistant", "tool_calls": [{"id": "c", "function": {"name": "f", "arguments": "{oops"}}]}}]});
        let turn = parse_response(Provider::OpenAiCompatible, &bad).unwrap();
        assert_eq!(turn.tool_calls[0].input, Value::Null);
    }

    #[test]
    fn tool_results_follow_each_wire_shape() {
        let outcomes = vec![
            ToolOutcome { call_id: "a".into(), text: "ok".into(), is_error: false, image_png: None },
            ToolOutcome { call_id: "b".into(), text: "meta".into(), is_error: true, image_png: Some(vec![1, 2, 3]) },
        ];
        let anthropic = tool_result_messages(Provider::Anthropic, &outcomes);
        assert_eq!(anthropic.len(), 1);
        let blocks = anthropic[0]["content"].as_array().unwrap();
        assert_eq!(blocks[0]["tool_use_id"], "a");
        assert!(blocks[0].get("is_error").is_none());
        assert_eq!(blocks[1]["is_error"], true);
        assert_eq!(blocks[1]["content"][1]["type"], "image");
        assert_eq!(blocks[1]["content"][1]["source"]["media_type"], "image/png");
        let openai = tool_result_messages(Provider::OpenAiCompatible, &outcomes);
        assert_eq!(openai.len(), 3);
        assert_eq!(openai[0]["role"], "tool");
        assert_eq!(openai[1]["tool_call_id"], "b");
        assert_eq!(openai[2]["role"], "user");
        assert!(openai[2]["content"][1]["image_url"]["url"]
            .as_str()
            .unwrap()
            .starts_with("data:image/png;base64,"));
    }

    #[test]
    fn settings_round_trip_through_json_with_defaults() {
        let parsed: AssistantSettings = serde_json::from_str(r#"{"provider":"open_ai_compatible","model":"m"}"#).unwrap();
        assert_eq!(parsed.provider, Provider::OpenAiCompatible);
        assert_eq!(parsed.model, "m");
        assert_eq!(parsed.max_tool_rounds, 40);
        assert!(parsed.refusal_fallback);
        let text = serde_json::to_string(&AssistantSettings::default()).unwrap();
        assert!(text.contains("\"effort\":\"default\""));
    }
}
