# AI 助手（MCP + 内置对话面板）实施计划与执行记录

状态：**执行中**（由 Claude Code 自主执行；用户离线期间的决策与未解决问题均记录在本文末尾）。

## 1. 目标

1. 为系统所有功能提供 MCP（Model Context Protocol）接口，供大语言模型调用。
2. 在软件界面右侧新增对话窗口，用户与窗口中的大模型对话即可使用、操作本系统的功能（画图、修改、查询、出图等）。

## 2. 现状盘点（调研结论）

调研后发现仓库里 **MCP 服务端已经存在并且相当完整**，不必重写：

| 组件 | 位置 | 说明 |
|---|---|---|
| MCP stdio 服务端 | `src/mcp.rs`（`OpenCADStudio --mcp`） | JSON-RPC、tools/resources/tasks、取消、会话发现、工具 schema 摘要固定测试 |
| 操作定义（单一事实来源） | `src/mcp_ops.rs` | 每个 op 的参数、类型、校验规则，自动生成 JSON Schema |
| GUI 内控制桥 | `src/app/control/`（`control_request`） | 所有操作在 GUI 进程内执行：命令运行/逐步输入、记录读写、选择、截图、交互式拾取等 |
| REST / 无界面服务 | `src/rest.rs`、`src/app/automation.rs` | 同一调度器的 HTTP 与 stdin/TCP 传输 |
| 文档 | `docs/automation/*` | API 规范、MCP 配置、冒烟/验收脚本 |

MCP 的 4 个工具（`ocs_sessions`、`ocs_read`、`ocs_execute`、`ocs_capture`）通过 `run`/`start`/`input` 可以调用 **全部命令行命令**（命令清单由 `commands` op 动态给出），通过 `records`/`set_properties` 可以读写 **全部数据库记录**，另有查询、测量、截图、出图、保存校验等。因此"所有功能的 MCP"这一目标在功能面上已基本达成；本次工作重点是 **第 2 项：内置对话窗口**，并复用同一套工具定义，保证"外部 MCP 客户端"与"内置助手"看到的能力完全一致。

缺失的部分：

- 软件内没有任何 LLM 对话界面；没有 LLM Provider 的 HTTP 客户端；没有相关设置。
- 没有把 MCP 工具暴露给进程内调用方的"进程内桥"（现有桥只走 TCP 描述符文件）。

## 3. 方案（已采纳）

### 3.1 架构

```
┌──────────────── GUI 线程（iced update/view） ────────────────┐
│  AssistantPanel(view)  ──AssistantMsg──▶  on_assistant()      │
│        ▲                                        │ AgentCommand │
│  AssistantEvent（文本/工具调用/结果/完成/错误）    ▼              │
│        │                              ┌──────────────────┐     │
│  Message::ControlRequest(Envelope) ◀──│  agent 工作线程   │     │
│  （复用现有 control_request 调度器）    │  LLM HTTP 循环    │     │
└───────────────────────────────────────│  工具执行→Envelope│─────┘
                                        └──────────────────┘
                                                 │ HTTPS (ureq, 平台证书)
                                                 ▼
                                   Anthropic Messages API / OpenAI 兼容接口
```

- **工具面**：直接复用 `mcp::tool_definitions()`（去掉 `ocs_session_id`，因为内置助手天然绑定当前会话），系统提示复用 `mcp::INSTRUCTIONS`。任何新增 MCP 能力自动同步到内置助手。
- **执行语义**：与 `mcp.rs` 的 `GuiClient::request` 对齐——自动补 `request_id`/`client_id`/`document_id`/`revision`/`selection`；`accepted/running` 时轮询 `operation`；交互式拾取（`user_select`/`getpoint`）最长等待 10 分钟；取消时发送 `cancel`。
- **截图**：`ocs_capture` 返回 PNG，作为图像块回传给模型（Anthropic 用 `image` 块；OpenAI 兼容接口用紧随其后的 user 消息 `image_url` data URL），并在对话面板中显示缩略图。
- **Provider**：默认 Anthropic Messages API（原生 HTTP，`claude-opus-5-5`，adaptive thinking，`output_config.effort` 可选），另提供"OpenAI 兼容（Chat Completions）"选项以便接入本地模型/国内模型网关。API Key 支持环境变量回退（`ANTHROPIC_API_KEY` / `OPENAI_API_KEY`）。
- **UI**：新的停靠面板 `PanelId::Assistant`（默认停靠右侧，可拖到左侧/调宽/自动折叠，与其他面板一致）。包含：设置折叠区（Provider、Base URL、模型、API Key、Effort）、对话流（用户气泡、助手 Markdown、工具调用卡片、截图缩略图）、多行输入框（Enter 发送，Shift+Enter 换行）、发送/停止/新对话按钮、token 用量。
- **入口**：功能区 View › Palettes › "AI Assistant" 按钮；命令 `AIASSIST` / `AIASSISTCLOSE`（内部 `_AIASSISTTOGGLE`）。
- **持久化**：`UserSettings.assistant`（settings.json）。对话历史不持久化（进程内）。
- **i18n**：新增 Fluent 组 `assistant`，21 种语言全部补齐（仓库测试要求每种语言键集合一致）。
- **Web（wasm）构建**：面板可显示，但 LLM 调用仅桌面可用（提示"仅桌面版可用"）。

### 3.2 文件清单

新增：
- `src/app/assistant/mod.rs` — 面板状态、消息、`on_assistant`、打开/关闭、订阅。
- `src/app/assistant/provider.rs` — Provider 抽象，Anthropic/OpenAI 兼容的请求构造与响应解析（纯函数，带单测）。
- `src/app/assistant/agent.rs` — 工作线程：代理循环、工具执行、进程内桥（带单测）。
- `src/ui/window/assistant_panel.rs` — 视图。
- `src/modules/view/assistant.rs` — 功能区按钮。
- `src/app/commands/assistant.rs` — 命令分发。
- `docs/ai-assistant.md` — 使用与开发文档。

修改：`src/ui/dock.rs`、`src/app/view/mod.rs`、`src/app/update/dialog.rs`、`src/app/update/mod.rs`、`src/app/mod.rs`、`src/app/settings.rs`、`src/app/update/file.rs`、`src/app/commands/mod.rs`、`src/modules/view/mod.rs`、`src/ui/ribbon/{mod,widgets}.rs`、`src/mcp.rs`（放开可见性）、`src/locale_catalog.rs`、`locales/*/opencadstudio.ftl`、`README.md`、`docs/automation/README.md`。

### 3.3 步骤

1. ✅ 调研（本节以上）。
2. ✅ 安装 Rust 工具链（机器上原本没有 cargo）。基线 `cargo check` 通过。
3. Provider 层 + 单测。
4. Agent 线程 + 进程内桥 + 单测。
5. 面板 UI、停靠、功能区、命令、设置持久化。
6. i18n 21 语种。
7. `cargo check`、`cargo test`（assistant/mcp/i18n/dock 相关）、`python scripts/test_locales.py`。
8. 文档、提交。

## 4. 执行记录

- 2026-10-06：调研完成；发现 MCP 已存在，方案改为"复用 MCP 工具面 + 新增内置助手"。安装 rustup/cargo（用户目录，不影响系统）。
- 2026-10-06：完成 Provider 层（`provider.rs`，含 9 个单测）、Agent 线程与进程内桥（`agent.rs`，含 7 个单测）、面板状态与消息（`assistant/mod.rs`，含 5 个单测）、视图（`assistant_panel.rs`）、停靠/功能区/命令/设置接线、21 语种词条、文档（`docs/ai-assistant.md`、README、automation/README）。
- 2026-10-06：`scripts/test_locales.py` 在改动前就已失败（`src/app/alias.rs`、`command_driver/utilities.rs` 等既有文件里有未登记的 `t!`/`tf!` 字面量，共 30+ 处）；本次新增的词条全部登记，未新增失败项。见第 5 节。

## 5. 未解决 / 需要用户决定的问题

1. **`scripts/test_locales.py` 在本次改动前已失败**：`src/app/alias.rs`、`src/app/command_driver/utilities.rs`、`src/app/commands/{draw,fileops,layerprops,styleprops}.rs` 等既有文件里有 30+ 处 `t!`/`tf!` 字面量没有登记进 `src/locale_catalog.rs`。本次新增的词条全部登记并通过该脚本的检查逻辑（过滤后无 `assistant` 相关缺失）。是否补齐这些历史词条由用户决定。
2. **未做真机 GUI 验证**：这台机器是无显示器的服务器（单核、约 1 GB 内存、3 GB 交换、磁盘剩余约 4 GB），只能做 `cargo check` 与单元测试，无法启动窗口、无法用真实 API Key 走一遍"对话→工具调用→绘图"。请在有图形界面的机器上运行，打开 View › Palettes › AI Assistant，填入 API Key 后试一句"在原点画一个半径 50 的圆"。
3. **API Key 以明文存于 settings.json**（与现有 `font_source_url` 等设置同文件）。可改为系统钥匙串（keyring）存储；本次为降低依赖与风险未引入新 crate，提供了环境变量回退（`ANTHROPIC_API_KEY` / `OPENAI_API_KEY`）。
4. **Web（wasm）构建未检查**：本机没有 wasm 目标，磁盘也不够再装。代码里 wasm 分支只保留面板与"仅桌面可用"提示，理论上可编译，建议在 CI 里确认 `trunk build` / `cargo check --target wasm32-unknown-unknown`。
5. **未实现流式输出（SSE）**：回复在完整生成后一次显示。Anthropic 单次请求超时设为 10 分钟；若觉得等待体验差，下一步可加 SSE 解析。
6. **设置放在面板内而非 Options 对话框**：为避免改动 1,700 行的 `options.rs`，设置用齿轮按钮在面板内展开。如需与其他设置统一，可再迁移到 Options 新增 "AI" 页签。
7. **同一时刻只允许一个自动化操作**：内置助手与外部 MCP/REST 客户端共用 GUI 的 `control.pending`，并发时另一方会收到 `busy`，与现有 MCP 行为一致。
8. **提交与测试**：按用户 2026-10-07 的要求已合入 `main` 并推送。库代码与单元测试代码均通过 `cargo check`（类型检查）；但单元测试的实际运行在本机未能完成——测试二进制的完整编译（单核、1 GB 内存、磁盘剩余约 3 GB）在上一会话结束前被中断。推送后已在后台重新启动 `cargo test --lib`（assistant/dock/i18n/ribbon/settings 过滤），结果会追加到本节；也建议在 CI 或开发机上运行 `cargo test --lib assistant` 与 `cargo test --lib i18n` 复核。
