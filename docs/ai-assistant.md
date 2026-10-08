# AI Assistant (built-in chat panel)

OpenCADStudio ships an AI assistant docked beside the drawing. You describe
what to draw, change, measure or check; the model does it on the open drawing
through the **same tool surface the MCP server exposes** (`ocs_read`,
`ocs_execute`, `ocs_capture`), so anything an external MCP client can do, the
built-in assistant can do too — no more, no less.

## Opening the panel

The panel is on the right edge from the first launch, collapsed to a narrow
rail: hover or click the rail to expand it, and use the pin in its title bar
to keep it open. Closing it with × remembers that choice; reopen it with:

- Ribbon: **View › Palettes › AI Assistant**
- Commands: `AIASSIST` (open), `AIASSISTCLOSE` (close)

It is an ordinary dock panel: drag its title bar to the left or right edge,
resize it with the divider, pin it to stay expanded or auto-collapse.

## Settings (gear icon in the panel)

| Field | Meaning |
|---|---|
| Provider | **Anthropic (Claude)** — the Anthropic Messages API, spoken natively. **OpenAI-compatible** — any server speaking Chat Completions (`/chat/completions` with `tools`): local inference servers, model gateways, other vendors. |
| Base URL | Empty uses the provider default (`https://api.anthropic.com` or `https://api.openai.com/v1`). Set it for a gateway or a local server, e.g. `http://localhost:11434/v1`. |
| Model | Empty uses the provider default (`claude-opus-5-5` for Anthropic; OpenAI-compatible needs a name). |
| API key | Stored **in plain text** in the user settings file (`settings.json`). Leave it empty to use the `ANTHROPIC_API_KEY` / `OPENAI_API_KEY` environment variable instead. |
| Effort | Anthropic only: `output_config.effort` (`low` … `max`). *Default* leaves it to the server. |

Switching provider starts a new conversation: the two wire formats do not
share a transcript.

Advanced values live in `settings.json` under `"assistant"`:
`max_tool_rounds` (default 40 tool rounds per message), `max_tokens`
(default 16000 per reply) and `refusal_fallback` (Anthropic: send
`fallbacks: "default"` so a declined request is retried on a fallback model).

## Using it

- Type in the composer. **Enter** sends, **Shift+Enter** adds a line.
- Every tool call shows as a card in the transcript (op and key argument,
  ✓/✗ once finished). Click it to see the arguments and the raw result.
  Viewport captures the model requested are shown inline.
- **Stop** ends the current turn after the running step; a pending
  interactive prompt (object or point pick) is dismissed.
- **New chat** clears the transcript and the model's context.
- Each tool call is also logged on the command line as
  `automation <op>: <status>`, the same trail an external MCP client leaves.
- When the model asks you to pick objects or a point, answer in the drawing
  as you would for any command (Enter / Escape finish a selection).

Ask in any language; the assistant answers in the language you write in.

## How it works

```
GUI thread (iced)                     ocs-assistant thread
───────────────────────────────       ────────────────────────────────────
AssistantPanel ──AssistantMsg──▶ on_assistant()
                                 │ AgentCommand::Send ───────────▶ run_turn()
                                 │                                  │ HTTPS (ureq, platform certs)
Message::ControlRequest(Envelope)◀── AssistantEvent::Control ───── execute_tool()
   → control_request() ──reply──▶                                   │
AssistantEvent::{Text,ToolCall,…}◀────────────────────────────────── loop until end_turn
```

- `src/app/assistant/provider.rs` — the two wire formats (request bodies,
  response parsing, tool-result messages), pure functions with tests.
- `src/app/assistant/agent.rs` — the worker thread: model loop, tool
  execution, the in-process bridge to the GUI dispatcher. Request shaping
  (ids, `document_id`, `revision`, `selection`), polling of `accepted` /
  `running` answers and the ten-minute ceiling for interactive picks mirror
  `mcp::GuiClient::request`, so an operation behaves identically whether it
  arrives from the panel or from an MCP client.
- `src/app/assistant/mod.rs` — panel state, messages and the handlers on
  `OpenCADStudio`.
- `src/ui/window/assistant_panel.rs` — the view.

Tool definitions come from `mcp::tool_definitions()` with the session
plumbing removed (`ocs_sessions`, `ocs_session_id`) and the MCP-resource-only
capture options dropped (`delivery`, `tile`, `diff`). The system prompt is
`mcp::INSTRUCTIONS` minus the session bootstrap, plus a short preamble
describing the in-editor situation. Adding an op in `mcp_ops.rs` therefore
reaches the built-in assistant automatically.

Model calls are plain HTTPS through `crate::network::agent` (operating-system
certificate verifier, 10-minute timeout). Anthropic requests omit `thinking`
(adaptive is the server default on current models), place cache breakpoints
on the tool list, the system prompt and the newest message, and echo
assistant turns back verbatim so thinking blocks stay valid. Tool results
larger than ~120 KB are truncated with a hint to page or filter.

## Memory

The assistant keeps a memory directory at **`agent/memory` next to the
executable** (user config directory when that folder is read-only):

```
agent/memory/
├── MEMORY.md            index: one line per note, included in every system prompt
├── notes/<name>.md      durable facts the model saves with the ocs_memory tool
└── sessions/<time>.md   transcript of each conversation, written as it happens
```

- **Sessions** are written automatically: every user message, reply, tool
  call (arguments) and tool result (outcome) is appended to the current
  session file while the task runs, so the record survives a crash or a
  closed window. *New chat* starts a new file.
- **Notes** are the model's long-term memory. The system prompt asks it to
  save durable facts — your preferences, the drawing's layer and block
  conventions, how a recurring task was done — and to write a short summary
  when a multi-step task finishes. The `ocs_memory` tool offers `list`,
  `read`, `write`, `append` and `delete`; note names become lowercase slugs
  (`Layer conventions` → `notes/layer-conventions.md`) and are capped at
  64 KB. Everything is Markdown, so you can read or edit it yourself.

## Logging

Every run appends to **`cad.log` next to the executable** (or in the user
config directory when that folder is read-only, e.g. under `Program Files`).
The assistant writes one line per step, so a conversation can be replayed
after the fact:

```
2026-10-08T03:14:15.120Z INFO  OpenCADStudio::app::assistant::agent: turn start: provider=Anthropic model=claude-opus-5-5 endpoint=https://api.anthropic.com/v1/messages user: 在原点画一个半径50的圆
2026-10-08T03:14:15.121Z INFO  OpenCADStudio::app::assistant::agent: model request round 0: 1 messages, 29211 bytes
2026-10-08T03:14:19.870Z INFO  OpenCADStudio::app::assistant::agent: model response round 0: stop=ToolUse tokens in=7120 out=96 tool_calls=1 in 4749 ms
2026-10-08T03:14:19.871Z INFO  OpenCADStudio::app::assistant::agent: tool call toolu_01…: ocs_execute {"request":{"op":"run","cmd":"CIRCLE 0,0 50"}}
2026-10-08T03:14:19.902Z INFO  OpenCADStudio::automation: run: completed
2026-10-08T03:14:19.903Z INFO  OpenCADStudio::app::assistant::agent: tool result toolu_01…: ok=true 412 bytes: {"ok":true,"status":"completed",…}
2026-10-08T03:14:23.440Z INFO  OpenCADStudio::app::assistant::agent: assistant: 已在原点绘制半径 50 的圆（图层 0）。
2026-10-08T03:14:23.441Z INFO  OpenCADStudio::app::assistant::agent: turn finished: end_turn
```

Automation requests from MCP / REST clients land in the same file under the
`OpenCADStudio::automation` target, as do panics and third-party warnings.
API keys are never logged. The file rotates to `cad.log.1` at 10 MB.
`--log debug` (or `RUST_LOG=debug`, env_logger-style directives accepted)
raises the level and also echoes the lines to the console.

## Limits and notes

- Desktop builds only. The web build shows the panel but cannot call a model.
- One operation at a time: while the assistant runs an operation, an
  external MCP/REST client gets `busy`, and vice versa.
- The conversation is not persisted; closing the application forgets it.
- Streaming is not used; a long reply appears when it is complete.
- The API key is stored in plain text. Prefer the environment variable on
  shared machines.
