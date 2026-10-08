//! The assistant's on-disk memory: `agent/memory` beside the executable.
//!
//! Two kinds of memory live there, both plain Markdown so a person can read
//! and edit them:
//!
//! - **Notes** (`notes/<name>.md`, indexed in `MEMORY.md`): durable facts the
//!   model chooses to keep — drawing conventions, the person's preferences,
//!   how a recurring task is done. The index is part of every system prompt,
//!   and the model reads, writes and deletes notes through the `ocs_memory`
//!   tool.
//! - **Sessions** (`sessions/<timestamp>.md`): a running transcript of each
//!   conversation — every user message, reply, tool call and outcome — written
//!   as the task proceeds, so what the assistant did is on record even when
//!   the application closes.
//!
//! When the executable's folder is read-only (an installation under
//! `Program Files`), the same tree is kept in the per-user config directory.

use serde_json::{json, Value};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

/// Directory name under the application folder.
pub const DIR: &str = "agent/memory";
/// Index file listing every note.
pub const INDEX: &str = "MEMORY.md";
/// Most index text handed to the model per turn.
const MAX_INDEX_PROMPT: usize = 8 * 1024;
/// Largest note the tool returns or stores.
const MAX_NOTE: usize = 64 * 1024;
/// Recent sessions named in the system prompt.
const RECENT_SESSIONS: usize = 5;

/// Candidate roots, most preferred first.
pub fn candidate_roots() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            out.push(dir.join(DIR));
        }
    }
    if let Some(dir) = crate::config::config_dir() {
        out.push(dir.join(DIR));
    }
    out
}

/// A memory tree rooted at one directory.
#[derive(Debug, Clone)]
pub struct MemoryStore {
    root: PathBuf,
    /// The transcript of the current conversation, once something was said.
    session: Option<PathBuf>,
}

impl MemoryStore {
    /// Open (creating) the first candidate root that is writable.
    pub fn open_default() -> Option<Self> {
        if cfg!(test) {
            let dir = std::env::temp_dir().join(format!("ocs-test-memory-{}", std::process::id()));
            return Self::open(&dir).ok();
        }
        for root in candidate_roots() {
            match Self::open(&root) {
                Ok(store) => return Some(store),
                Err(error) => log::warn!("agent memory: cannot use {}: {error}", root.display()),
            }
        }
        None
    }

    /// Open (creating) a memory tree at `root`, proving it is writable.
    pub fn open(root: &Path) -> std::io::Result<Self> {
        fs::create_dir_all(root.join("notes"))?;
        fs::create_dir_all(root.join("sessions"))?;
        let index = root.join(INDEX);
        if !index.exists() {
            fs::write(
                &index,
                "# Assistant memory\n\nOne line per note: `- [title](notes/name.md) — what it holds`.\n",
            )?;
        }
        Ok(Self {
            root: root.to_path_buf(),
            session: None,
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn session_path(&self) -> Option<&Path> {
        self.session.as_deref()
    }

    // ── Notes ──────────────────────────────────────────────────────────────

    /// `notes/<name>.md`; names are lowercase slugs so the model cannot
    /// escape the directory or collide on case-insensitive file systems.
    pub fn sanitize_name(name: &str) -> Option<String> {
        let slug: String = name
            .trim()
            .trim_end_matches(".md")
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '-' })
            .collect::<String>()
            .split('-')
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>()
            .join("-");
        if slug.is_empty() || slug.len() > 64 {
            return None;
        }
        Some(slug)
    }

    fn note_path(&self, name: &str) -> Result<(String, PathBuf), String> {
        let slug = Self::sanitize_name(name)
            .ok_or_else(|| format!("invalid note name {name:?}; use letters, digits and dashes"))?;
        Ok((slug.clone(), self.root.join("notes").join(format!("{slug}.md"))))
    }

    pub fn index_text(&self) -> String {
        fs::read_to_string(self.root.join(INDEX)).unwrap_or_default()
    }

    pub fn list_notes(&self) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(self.root.join("notes"))
            .map(|entries| {
                entries
                    .flatten()
                    .filter_map(|entry| {
                        let name = entry.file_name().to_string_lossy().into_owned();
                        name.strip_suffix(".md").map(str::to_owned)
                    })
                    .collect()
            })
            .unwrap_or_default();
        names.sort();
        names
    }

    pub fn read_note(&self, name: &str) -> Result<String, String> {
        let (slug, path) = self.note_path(name)?;
        fs::read_to_string(&path).map_err(|_| format!("no note named {slug}"))
    }

    /// Create or replace a note and keep the index line current.
    pub fn write_note(&self, name: &str, description: &str, content: &str) -> Result<String, String> {
        if content.len() > MAX_NOTE {
            return Err(format!("note too large ({} bytes, limit {MAX_NOTE})", content.len()));
        }
        let (slug, path) = self.note_path(name)?;
        fs::write(&path, content).map_err(|e| format!("cannot write {}: {e}", path.display()))?;
        self.update_index(&slug, description, content)?;
        Ok(slug)
    }

    pub fn append_note(&self, name: &str, content: &str) -> Result<String, String> {
        let (slug, path) = self.note_path(name)?;
        let existing = fs::read_to_string(&path).unwrap_or_default();
        if existing.len() + content.len() > MAX_NOTE {
            return Err(format!("note would exceed {MAX_NOTE} bytes"));
        }
        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .map_err(|e| format!("cannot open {}: {e}", path.display()))?;
        let separator = if existing.is_empty() || existing.ends_with('\n') { "" } else { "\n" };
        write!(file, "{separator}{content}").map_err(|e| e.to_string())?;
        if !existing.contains(&slug) {
            self.update_index(&slug, "", content)?;
        }
        Ok(slug)
    }

    pub fn delete_note(&self, name: &str) -> Result<String, String> {
        let (slug, path) = self.note_path(name)?;
        fs::remove_file(&path).map_err(|_| format!("no note named {slug}"))?;
        let index = self.root.join(INDEX);
        let kept: Vec<String> = self
            .index_text()
            .lines()
            .filter(|line| !line.contains(&format!("(notes/{slug}.md)")))
            .map(str::to_owned)
            .collect();
        fs::write(&index, kept.join("\n") + "\n").map_err(|e| e.to_string())?;
        Ok(slug)
    }

    /// Replace or add the index line for `slug`. The description defaults to
    /// the note's first heading or first line.
    fn update_index(&self, slug: &str, description: &str, content: &str) -> Result<(), String> {
        let title = content
            .lines()
            .find(|line| !line.trim().is_empty())
            .map(|line| line.trim_start_matches('#').trim())
            .filter(|line| !line.is_empty())
            .unwrap_or(slug);
        let description = description.trim();
        let line = if description.is_empty() {
            format!("- [{title}](notes/{slug}.md)")
        } else {
            format!("- [{title}](notes/{slug}.md) — {description}")
        };
        let marker = format!("(notes/{slug}.md)");
        let mut lines: Vec<String> = self.index_text().lines().map(str::to_owned).collect();
        if let Some(existing) = lines.iter_mut().find(|l| l.contains(&marker)) {
            *existing = line;
        } else {
            lines.push(line);
        }
        fs::write(self.root.join(INDEX), lines.join("\n") + "\n").map_err(|e| e.to_string())
    }

    // ── Sessions ───────────────────────────────────────────────────────────

    pub fn list_sessions(&self) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(self.root.join("sessions"))
            .map(|entries| {
                entries
                    .flatten()
                    .map(|entry| entry.file_name().to_string_lossy().into_owned())
                    .filter(|name| name.ends_with(".md"))
                    .collect()
            })
            .unwrap_or_default();
        names.sort();
        names
    }

    pub fn read_session(&self, name: &str) -> Result<String, String> {
        let file = Path::new(name)
            .file_name()
            .and_then(|n| n.to_str())
            .filter(|n| n.ends_with(".md") && !n.contains(".."))
            .ok_or_else(|| format!("invalid session name {name:?}"))?;
        fs::read_to_string(self.root.join("sessions").join(file))
            .map_err(|_| format!("no session named {file}"))
    }

    /// Forget the current transcript; the next entry opens a new file.
    pub fn new_session(&mut self) {
        self.session = None;
    }

    /// Append one entry to the current conversation's transcript, opening
    /// the file (named after the moment the conversation started) on first
    /// use. Errors are logged, never raised: memory must not break a task.
    pub fn record(&mut self, heading: &str, body: &str) {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        let stamp = crate::applog::timestamp(now);
        let path = match &self.session {
            Some(path) => path.clone(),
            None => {
                let name = format!(
                    "{}-{}.md",
                    stamp[..19].replace([':', '-'], "").replace('T', "-"),
                    std::process::id()
                );
                let path = self.root.join("sessions").join(name);
                let header = format!("# Assistant session {stamp}\n");
                if let Err(error) = fs::write(&path, header) {
                    log::warn!("agent memory: cannot start session file {}: {error}", path.display());
                    return;
                }
                self.session = Some(path.clone());
                path
            }
        };
        let entry = format!("\n## {heading} ({})\n\n{}\n", &stamp[11..19], body.trim_end());
        let result = fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .and_then(|mut file| file.write_all(entry.as_bytes()));
        if let Err(error) = result {
            log::warn!("agent memory: cannot append to {}: {error}", path.display());
        }
    }

    // ── Model-facing surface ───────────────────────────────────────────────

    /// The memory section of the system prompt.
    pub fn prompt_section(&self) -> String {
        let mut index = self.index_text();
        if index.len() > MAX_INDEX_PROMPT {
            let cut = index.floor_char_boundary(MAX_INDEX_PROMPT);
            index.truncate(cut);
            index.push_str("\n…(index truncated; use ocs_memory list)");
        }
        let sessions = self.list_sessions();
        let recent: Vec<&str> = sessions
            .iter()
            .rev()
            .take(RECENT_SESSIONS)
            .map(String::as_str)
            .collect();
        format!(
            "MEMORY: You have a persistent memory directory at {root}. Notes live under notes/ and are indexed below; \
             every conversation is transcribed automatically under sessions/. Use the ocs_memory tool: \
             `list` shows notes and sessions, `read` returns a note (name) or a session (name ending in .md), \
             `write` creates or replaces a note (name, description, content), `append` adds to one, `delete` removes one. \
             Save durable facts as you learn them — the person's preferences, this drawing's layer and block conventions, \
             how a recurring task was done, decisions that were made — and, when a multi-step task finishes, write or update \
             a short note summarizing what was done and what remains. Never store API keys or secrets. \
             Read a relevant note before relying on a convention you are unsure of.\n\nMemory index ({index_file}):\n{index}\n\
             Recent sessions: {recent}",
            root = self.root.display(),
            index_file = INDEX,
            recent = if recent.is_empty() { "(none)".to_string() } else { recent.join(", ") },
        )
    }

    /// JSON Schema of the `ocs_memory` tool.
    pub fn tool_schema() -> Value {
        json!({
            "type": "object",
            "properties": {
                "op": {"type": "string", "enum": ["list", "read", "write", "append", "delete"], "description": "What to do with the memory directory."},
                "name": {"type": "string", "description": "Note name (letters, digits, dashes) or, for read, a session file name ending in .md."},
                "description": {"type": "string", "description": "write: one line for the memory index saying what the note holds."},
                "content": {"type": "string", "description": "write/append: Markdown body of the note."}
            },
            "required": ["op"],
            "additionalProperties": false
        })
    }

    /// Execute one `ocs_memory` call.
    pub fn call(&self, input: &Value) -> Result<Value, String> {
        let op = input["op"].as_str().ok_or("ocs_memory needs op")?;
        let name = input["name"].as_str().unwrap_or("");
        match op {
            "list" => Ok(json!({
                "ok": true,
                "root": self.root.display().to_string(),
                "index": self.index_text(),
                "notes": self.list_notes(),
                "sessions": self.list_sessions(),
                "current_session": self.session.as_ref().and_then(|p| p.file_name()).map(|n| n.to_string_lossy().into_owned()),
            })),
            "read" => {
                if name.is_empty() {
                    return Err("read needs name".into());
                }
                let content = if name.ends_with(".md") && self.read_session(name).is_ok() {
                    self.read_session(name)?
                } else {
                    self.read_note(name)?
                };
                Ok(json!({"ok": true, "name": name, "content": content}))
            }
            "write" => {
                let content = input["content"].as_str().ok_or("write needs content")?;
                let slug = self.write_note(name, input["description"].as_str().unwrap_or(""), content)?;
                Ok(json!({"ok": true, "name": slug, "path": format!("notes/{slug}.md"), "bytes": content.len()}))
            }
            "append" => {
                let content = input["content"].as_str().ok_or("append needs content")?;
                let slug = self.append_note(name, content)?;
                Ok(json!({"ok": true, "name": slug, "path": format!("notes/{slug}.md")}))
            }
            "delete" => {
                let slug = self.delete_note(name)?;
                Ok(json!({"ok": true, "deleted": slug}))
            }
            other => Err(format!("unknown memory op {other}; use list, read, write, append or delete")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store(tag: &str) -> MemoryStore {
        let dir = std::env::temp_dir().join(format!("ocs-memory-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        MemoryStore::open(&dir).unwrap()
    }

    #[test]
    fn names_become_safe_slugs() {
        assert_eq!(MemoryStore::sanitize_name("Layer Conventions.md"), Some("layer-conventions".into()));
        assert_eq!(MemoryStore::sanitize_name("../../etc/passwd"), Some("etc-passwd".into()));
        assert_eq!(MemoryStore::sanitize_name("---"), None);
        assert_eq!(MemoryStore::sanitize_name(&"x".repeat(70)), None);
    }

    #[test]
    fn notes_round_trip_and_keep_the_index_current() {
        let s = store("notes");
        assert!(s.index_text().starts_with("# Assistant memory"));
        let slug = s.write_note("Layer conventions", "walls, doors, glazing", "# Layers\n\nA-WALL for walls").unwrap();
        assert_eq!(slug, "layer-conventions");
        assert_eq!(s.list_notes(), vec!["layer-conventions"]);
        assert!(s.index_text().contains("- [Layers](notes/layer-conventions.md) — walls, doors, glazing"));
        s.write_note("layer-conventions", "updated", "# Layers v2\n").unwrap();
        assert_eq!(s.index_text().matches("notes/layer-conventions.md").count(), 1);
        assert!(s.index_text().contains("[Layers v2]"));
        s.append_note("layer-conventions", "A-GLAZ for windows\n").unwrap();
        assert!(s.read_note("layer-conventions").unwrap().ends_with("A-GLAZ for windows\n"));
        assert!(s.read_note("missing").is_err());
        s.delete_note("layer-conventions").unwrap();
        assert!(s.list_notes().is_empty());
        assert!(!s.index_text().contains("layer-conventions"));
        assert!(s.write_note("big", "", &"x".repeat(MAX_NOTE + 1)).is_err());
    }

    #[test]
    fn sessions_are_transcribed_as_the_task_runs() {
        let mut s = store("sessions");
        assert!(s.session_path().is_none());
        s.record("User", "draw a circle");
        let path = s.session_path().unwrap().to_path_buf();
        assert!(path.starts_with(s.root().join("sessions")));
        s.record("Tool", "- ocs_execute run · CIRCLE 0,0 50 → ok");
        s.record("Assistant", "Done.");
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.starts_with("# Assistant session "));
        assert!(text.contains("## User (") && text.contains("draw a circle"));
        assert!(text.contains("## Tool (") && text.contains("## Assistant ("));
        assert_eq!(s.list_sessions().len(), 1);
        let name = s.list_sessions()[0].clone();
        assert_eq!(s.read_session(&name).unwrap(), text);
        assert!(s.read_session("../MEMORY.md").is_err());
        s.new_session();
        assert!(s.session_path().is_none());
    }

    #[test]
    fn tool_calls_cover_every_op_and_the_prompt_lists_the_index() {
        let mut s = store("tool");
        let written = s.call(&json!({"op": "write", "name": "prefs", "description": "how the person likes replies", "content": "# Prefs\n\nShort answers in Chinese."})).unwrap();
        assert_eq!(written["name"], "prefs");
        let read = s.call(&json!({"op": "read", "name": "prefs"})).unwrap();
        assert!(read["content"].as_str().unwrap().contains("Chinese"));
        s.record("User", "hello");
        let listed = s.call(&json!({"op": "list"})).unwrap();
        assert_eq!(listed["notes"], json!(["prefs"]));
        assert_eq!(listed["sessions"].as_array().unwrap().len(), 1);
        assert!(listed["current_session"].is_string());
        let session = listed["sessions"][0].as_str().unwrap();
        let session_read = s.call(&json!({"op": "read", "name": session})).unwrap();
        assert!(session_read["content"].as_str().unwrap().contains("hello"));
        assert!(s.call(&json!({"op": "append", "name": "prefs", "content": "Metric units.\n"})).is_ok());
        let prompt = s.prompt_section();
        assert!(prompt.contains("ocs_memory"));
        assert!(prompt.contains("(notes/prefs.md) — how the person likes replies"));
        assert!(prompt.contains(session));
        assert!(s.call(&json!({"op": "delete", "name": "prefs"})).is_ok());
        assert!(s.call(&json!({"op": "read", "name": "prefs"})).is_err());
        assert!(s.call(&json!({"op": "frobnicate"})).is_err());
        assert_eq!(MemoryStore::tool_schema()["required"], json!(["op"]));
    }

    #[test]
    fn candidate_roots_start_beside_the_executable() {
        let roots = candidate_roots();
        assert!(!roots.is_empty());
        let exe_dir = std::env::current_exe().unwrap().parent().unwrap().to_path_buf();
        assert_eq!(roots[0], exe_dir.join(DIR));
    }
}
