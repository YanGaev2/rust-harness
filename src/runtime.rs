use std::error::Error;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::thread;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::platform::ShellProfile;
use crate::request::ToolSpec;
use crate::tools::attachments::{AttachmentError, AttachmentTool};
use crate::tools::files::{FileTool, ToolError, WriteMode};
use crate::tools::shell::{DEFAULT_MAX_OUTPUT_BYTES, ShellError, ShellTool};

const DEFAULT_FILE_READ_MAX_BYTES: usize = 1024 * 1024;
const DEFAULT_FILE_TAIL_MAX_BYTES: usize = 64 * 1024;
const DEFAULT_ATTACHMENT_READ_MAX_BYTES: usize = 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: Value,
}

impl ToolCall {
    pub fn new(id: impl Into<String>, name: impl Into<String>, arguments: Value) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            arguments,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ToolCallResult {
    pub id: String,
    pub tool_name: String,
    pub ok: bool,
    pub repaired: bool,
    pub content: String,
    pub metadata: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ToolBatchResult {
    pub id: String,
    pub tool_name: String,
    pub ok: bool,
    pub repaired: bool,
    pub content: String,
    pub metadata: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// A short memo handed back to the model when the call had to be repaired,
    /// telling it the canonical tool name and argument shape to use next time.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
}

impl ToolBatchResult {
    fn from_execution(call: &ToolCall, result: Result<ToolCallResult, RuntimeError>) -> Self {
        match result {
            Ok(result) => {
                let mut batch: ToolBatchResult = result.into();
                if batch.repaired {
                    // An executor-specific memo (e.g. a shell command rewrite)
                    // beats the generic name/arguments one.
                    batch.hint = Some(
                        batch
                            .metadata
                            .get("repair_note")
                            .and_then(Value::as_str)
                            .map(str::to_string)
                            .unwrap_or_else(|| repair_hint(&call.name, &batch.tool_name)),
                    );
                }
                batch
            }
            Err(err) => Self {
                id: call.id.clone(),
                tool_name: call.name.clone(),
                ok: false,
                repaired: false,
                content: String::new(),
                metadata: json!({}),
                error: Some(err.to_string()),
                hint: None,
            },
        }
    }
}

impl From<ToolCallResult> for ToolBatchResult {
    fn from(value: ToolCallResult) -> Self {
        Self {
            id: value.id,
            tool_name: value.tool_name,
            ok: value.ok,
            repaired: value.repaired,
            content: value.content,
            metadata: value.metadata,
            error: None,
            hint: None,
        }
    }
}

pub trait ToolExecutor: Clone + Send + 'static {
    fn execute(&self, call: ToolCall) -> Result<ToolCallResult, RuntimeError>;
}

#[derive(Debug, Clone)]
pub struct ToolRuntime {
    workspace: PathBuf,
    shell_timeout: Duration,
    shell_profile: Option<ShellProfile>,
}

impl ToolRuntime {
    pub fn new(workspace: impl Into<PathBuf>) -> Self {
        Self {
            workspace: workspace.into(),
            shell_timeout: Duration::from_secs(30),
            shell_profile: Some(ShellProfile::native()),
        }
    }

    pub fn with_shell_timeout(mut self, timeout: Duration) -> Self {
        self.shell_timeout = timeout;
        self
    }

    /// Use the shell that detection actually found (or `None` when the
    /// environment has no shell at all). Callers that skip this keep the
    /// compile-time native default.
    pub fn with_shell_profile(mut self, profile: Option<ShellProfile>) -> Self {
        self.shell_profile = profile;
        self
    }

    /// Tools as advertised to the model: wire names follow the model's own
    /// priors (see [`wire_tool_name`]); the runtime keeps dotted canonical
    /// names internally. Assumes the compile-time native shell; prefer
    /// [`ToolRuntime::tool_specs_with_shell`] with the detected profile.
    pub fn tool_specs() -> Vec<ToolSpec> {
        Self::tool_specs_with_shell(Some(&ShellProfile::native()))
    }

    /// Tool specs with the shell tool described by (and gated on) the shell
    /// that detection actually found. Shell probe 2026-07-13: the model
    /// writes whichever dialect it is told about near-perfectly, and falls
    /// back to cmd idioms when the interpreter is left unnamed — so the
    /// dialect line rides on the tool description itself. `None` (no shell
    /// in the environment) drops the tool instead of advertising a broken
    /// capability.
    pub fn tool_specs_with_shell(shell: Option<&ShellProfile>) -> Vec<ToolSpec> {
        // Argument names below are the ones the model sends unprompted
        // (combat probes 2026-07-12/13: file_path, content, old_string/
        // new_string, pattern, source/destination), not our internal canon —
        // the executors accept both.
        const PATH: (&str, &str, &str) = (
            "file_path",
            "string",
            "Path relative to the workspace root (absolute paths inside the workspace are accepted)",
        );
        const MAX_BYTES: (&str, &str, &str) =
            ("max_bytes", "integer", "Byte cap for the returned content");
        let spec = |canonical: &str, description: &str, schema: Value| {
            ToolSpec::new(wire_tool_name(canonical), description).with_parameters(schema)
        };
        let mut specs = vec![
            spec(
                "file.read",
                "Read bounded UTF-8 text from a workspace-relative path.",
                tool_schema(&[PATH], &[MAX_BYTES]),
            ),
            spec(
                "file.write",
                "Write UTF-8 text to a workspace-relative path without requiring a prior read.",
                tool_schema(&[PATH, ("content", "string", "Full text to write")], &[]),
            ),
            spec(
                "file.append",
                "Append UTF-8 text to a workspace-relative path without requiring a prior read.",
                tool_schema(&[PATH, ("content", "string", "Text to append")], &[]),
            ),
            spec(
                "file.hash",
                "Compute a streaming BLAKE3 hash for a workspace file without reading it into context.",
                tool_schema(&[PATH], &[]),
            ),
            spec(
                "file.stat",
                "Read file or directory metadata without loading file content.",
                tool_schema(
                    &[("path", "string", "Workspace-relative file or directory")],
                    &[],
                ),
            ),
            spec(
                "file.tail",
                "Read a bounded UTF-8 suffix from a workspace file for logs and large outputs.",
                tool_schema(
                    &[PATH],
                    &[
                        ("lines", "integer", "Keep only the last N lines"),
                        MAX_BYTES,
                    ],
                ),
            ),
            spec(
                "file.replace",
                "Replace literal UTF-8 text in a workspace file without shelling out.",
                tool_schema(
                    &[
                        PATH,
                        ("old_string", "string", "Exact literal text to find"),
                        ("new_string", "string", "Replacement text"),
                    ],
                    &[],
                ),
            ),
            spec(
                "file.list",
                "List workspace files recursively with bounded result count.",
                tool_schema(
                    &[],
                    &[
                        (
                            "path",
                            "string",
                            "Directory to list (default: workspace root)",
                        ),
                        ("depth", "integer", "Recursion depth; 1 = top level only"),
                        (
                            "show_hidden",
                            "boolean",
                            "Include dot-directories like .git (default false)",
                        ),
                        ("max_results", "integer", "Entry cap (default 200)"),
                    ],
                ),
            ),
            spec(
                "file.search",
                "Search UTF-8 workspace files for a text pattern with bounded result count.",
                tool_schema(
                    &[(
                        "pattern",
                        "string",
                        "Literal text to search for (substring match, not regex)",
                    )],
                    &[
                        (
                            "path",
                            "string",
                            "Directory to search (default: workspace root)",
                        ),
                        (
                            "context_lines",
                            "integer",
                            "Lines of context to include around each match",
                        ),
                        ("max_results", "integer", "Match cap"),
                    ],
                ),
            ),
            spec(
                "file.delete",
                "Delete a workspace-relative file or directory without shelling out.",
                tool_schema(
                    &[("path", "string", "Workspace-relative file or directory")],
                    &[],
                ),
            ),
            spec(
                "file.move",
                "Move or rename a workspace-relative file or directory without shelling out.",
                tool_schema(
                    &[
                        ("source", "string", "Existing workspace-relative path"),
                        ("destination", "string", "New workspace-relative path"),
                    ],
                    &[(
                        "overwrite",
                        "boolean",
                        "Replace the destination if it exists (default false)",
                    )],
                ),
            ),
            spec(
                "attachment.read",
                "Read bounded clipboard/Codex attachment metadata and text content.",
                tool_schema(
                    &[("path", "string", "Attachment name or path")],
                    &[MAX_BYTES],
                ),
            ),
        ];
        if let Some(profile) = shell {
            specs.push(
                ToolSpec::new(
                    wire_tool_name("shell.exec"),
                    format!(
                        "Run a command via {} in the workspace, with timeout and bounded stdout/stderr.",
                        profile.dialect_note()
                    ),
                )
                .with_parameters(tool_schema(
                    &[("command", "string", "The command line to run")],
                    &[
                        ("timeout", "integer", "Timeout in seconds"),
                        (
                            "max_output_bytes",
                            "integer",
                            "Cap for captured stdout/stderr",
                        ),
                    ],
                )),
            );
        }
        specs
    }

    pub fn execute(&self, call: ToolCall) -> Result<ToolCallResult, RuntimeError> {
        let resolution = ToolResolution::from_name(&call.name)?;
        let argument_repair = repair_tool_arguments(&resolution.canonical, call.arguments);
        let call = ToolCall {
            arguments: argument_repair.arguments,
            ..call
        };
        let repaired = resolution.repaired || argument_repair.repaired;
        match resolution.canonical.as_str() {
            "file.write" => self.execute_file_write(call, repaired),
            "file.append" => self.execute_file_append(call, repaired),
            "file.replace" => self.execute_file_replace(call, repaired),
            "file.read" => self.execute_file_read(call, repaired),
            "file.hash" => self.execute_file_hash(call, repaired),
            "file.stat" => self.execute_file_stat(call, repaired),
            "file.tail" => self.execute_file_tail(call, repaired),
            "file.list" => self.execute_file_list(call, repaired),
            "file.search" => self.execute_file_search(call, repaired),
            "file.delete" => self.execute_file_delete(call, repaired),
            "file.move" => self.execute_file_move(call, repaired),
            "attachment.read" => self.execute_attachment_read(call, repaired),
            "shell.exec" => self.execute_shell(call, repaired),
            _ => Err(RuntimeError::UnknownTool(call.name)),
        }
    }

    fn execute_file_write(
        &self,
        call: ToolCall,
        repaired_name: bool,
    ) -> Result<ToolCallResult, RuntimeError> {
        let mode = match optional_string_arg(&call.arguments, &["mode"]).as_deref() {
            Some("append") => WriteMode::Append,
            _ => WriteMode::Replace,
        };

        self.execute_file_write_mode(call, repaired_name, "file.write", mode)
    }

    fn execute_file_append(
        &self,
        call: ToolCall,
        repaired_name: bool,
    ) -> Result<ToolCallResult, RuntimeError> {
        self.execute_file_write_mode(call, repaired_name, "file.append", WriteMode::Append)
    }

    fn execute_file_write_mode(
        &self,
        call: ToolCall,
        repaired_name: bool,
        tool_name: &str,
        mode: WriteMode,
    ) -> Result<ToolCallResult, RuntimeError> {
        let path = string_arg(
            &call.arguments,
            &[
                "path",
                "file",
                "filename",
                "file_path",
                "filepath",
                "target_file",
            ],
        )?;
        let content = string_arg(&call.arguments, &["content", "text", "body", "contents"])?;
        let result = FileTool::new(&self.workspace).write_text(&path, &content, mode)?;
        Ok(ToolCallResult {
            id: call.id,
            tool_name: tool_name.to_string(),
            ok: true,
            repaired: repaired_name,
            content: format!("wrote {}", result.path.display()),
            metadata: json!({
                "created": result.created,
                "previous_len": result.previous_len,
                "required_prior_read": result.required_prior_read,
            }),
        })
    }

    fn execute_file_replace(
        &self,
        call: ToolCall,
        repaired_name: bool,
    ) -> Result<ToolCallResult, RuntimeError> {
        let path = string_arg(
            &call.arguments,
            &[
                "path",
                "file",
                "filename",
                "file_path",
                "filepath",
                "target_file",
            ],
        )?;
        let old_text = string_arg(
            &call.arguments,
            &[
                "old_text",
                "old",
                "find",
                "search",
                "old_string",
                "old_str",
                "text_to_replace",
            ],
        )?;
        let new_text = string_arg(
            &call.arguments,
            &[
                "new_text",
                "new",
                "replacement",
                "replace",
                "with",
                "new_string",
                "new_str",
            ],
        )?;
        let max_replacements =
            optional_usize_arg(&call.arguments, &["max_replacements", "limit", "count"])
                .unwrap_or(1);
        let result = FileTool::new(&self.workspace).replace_text(
            &path,
            &old_text,
            &new_text,
            Some(max_replacements),
        )?;

        let mut metadata = json!({
            "path": result.path,
            "replacements": result.replacements,
            "previous_len": result.previous_len,
            "new_len": result.new_len,
        });
        if result.normalized_line_endings {
            metadata["repair_note"] = serde_json::Value::String(
                "old_string matched after normalizing line endings (the file uses different \
                 line endings than the provided text)"
                    .to_string(),
            );
        }
        Ok(ToolCallResult {
            id: call.id,
            tool_name: "file.replace".to_string(),
            ok: true,
            repaired: repaired_name || result.normalized_line_endings,
            content: format!(
                "replaced {} occurrence(s) in {}",
                result.replacements, result.path
            ),
            metadata,
        })
    }

    fn execute_attachment_read(
        &self,
        call: ToolCall,
        repaired_name: bool,
    ) -> Result<ToolCallResult, RuntimeError> {
        let path = string_arg(&call.arguments, &["path", "file", "filename", "image"])?;
        let max_bytes = optional_usize_arg(&call.arguments, &["max_bytes", "limit"])
            .unwrap_or(DEFAULT_ATTACHMENT_READ_MAX_BYTES);
        let result = AttachmentTool::new(&self.workspace).read(&path, max_bytes)?;

        Ok(ToolCallResult {
            id: call.id,
            tool_name: "attachment.read".to_string(),
            ok: true,
            repaired: repaired_name,
            content: result.content,
            metadata: json!({
                "path": result.path,
                "kind": result.kind,
                "mime_type": result.mime_type,
                "bytes": result.bytes,
                "bytes_read": result.bytes_read,
                "truncated": result.truncated,
                "max_bytes": max_bytes,
            }),
        })
    }

    fn execute_file_read(
        &self,
        call: ToolCall,
        repaired_name: bool,
    ) -> Result<ToolCallResult, RuntimeError> {
        let path = string_arg(
            &call.arguments,
            &[
                "path",
                "file",
                "filename",
                "file_path",
                "filepath",
                "target_file",
            ],
        )?;
        let max_bytes = optional_usize_arg(&call.arguments, &["max_bytes", "limit"])
            .unwrap_or(DEFAULT_FILE_READ_MAX_BYTES);
        let result = FileTool::new(&self.workspace).read_text_bounded(&path, max_bytes)?;
        Ok(ToolCallResult {
            id: call.id,
            tool_name: "file.read".to_string(),
            ok: true,
            repaired: repaired_name,
            content: result.content,
            metadata: json!({
                "path": result.path,
                "bytes_read": result.bytes_read,
                "total_bytes": result.total_bytes,
                "truncated": result.truncated,
                "max_bytes": max_bytes,
            }),
        })
    }

    fn execute_file_hash(
        &self,
        call: ToolCall,
        repaired_name: bool,
    ) -> Result<ToolCallResult, RuntimeError> {
        let path = string_arg(
            &call.arguments,
            &[
                "path",
                "file",
                "filename",
                "file_path",
                "filepath",
                "target_file",
            ],
        )?;
        let result = FileTool::new(&self.workspace).hash_file(&path)?;
        Ok(ToolCallResult {
            id: call.id,
            tool_name: "file.hash".to_string(),
            ok: true,
            repaired: repaired_name,
            content: result.hash.clone(),
            metadata: json!({
                "path": result.path,
                "bytes": result.bytes,
                "algorithm": result.algorithm,
                "hash": result.hash,
            }),
        })
    }

    fn execute_file_stat(
        &self,
        call: ToolCall,
        repaired_name: bool,
    ) -> Result<ToolCallResult, RuntimeError> {
        let path = string_arg(
            &call.arguments,
            &[
                "path",
                "file",
                "filename",
                "file_path",
                "filepath",
                "target_file",
            ],
        )?;
        let result = FileTool::new(&self.workspace).stat_path(&path)?;
        Ok(ToolCallResult {
            id: call.id,
            tool_name: "file.stat".to_string(),
            ok: true,
            repaired: repaired_name,
            content: format!(
                "{} {}",
                if result.is_dir { "dir" } else { "file" },
                result.path
            ),
            metadata: json!({
                "path": result.path,
                "is_file": result.is_file,
                "is_dir": result.is_dir,
                "len": result.len,
                "readonly": result.readonly,
                "modified_unix_seconds": result.modified_unix_seconds,
            }),
        })
    }

    fn execute_file_tail(
        &self,
        call: ToolCall,
        repaired_name: bool,
    ) -> Result<ToolCallResult, RuntimeError> {
        let path = string_arg(
            &call.arguments,
            &[
                "path",
                "file",
                "filename",
                "file_path",
                "filepath",
                "target_file",
            ],
        )?;
        let max_bytes = optional_usize_arg(&call.arguments, &["max_bytes", "limit"])
            .unwrap_or(DEFAULT_FILE_TAIL_MAX_BYTES);
        let max_lines = optional_usize_arg(&call.arguments, &["max_lines", "lines"]);
        let result = FileTool::new(&self.workspace).tail_text(&path, max_bytes, max_lines)?;
        Ok(ToolCallResult {
            id: call.id,
            tool_name: "file.tail".to_string(),
            ok: true,
            repaired: repaired_name,
            content: result.content,
            metadata: json!({
                "path": result.path,
                "bytes_read": result.bytes_read,
                "total_bytes": result.total_bytes,
                "truncated_prefix": result.truncated_prefix,
                "max_bytes": max_bytes,
                "max_lines": max_lines,
            }),
        })
    }

    fn execute_shell(
        &self,
        call: ToolCall,
        repaired_name: bool,
    ) -> Result<ToolCallResult, RuntimeError> {
        let command = string_arg(&call.arguments, &["command", "cmd", "script"])?;
        let max_output_bytes =
            optional_usize_arg(&call.arguments, &["max_output_bytes", "max_bytes", "limit"])
                .unwrap_or(DEFAULT_MAX_OUTPUT_BYTES);
        // `timeout` without a unit means SECONDS to models (the subprocess
        // convention); only the explicit `timeout_ms` is milliseconds.
        let shell_timeout = optional_u64_arg(&call.arguments, &["timeout_ms"])
            .map(|millis| Duration::from_millis(millis.max(1)))
            .or_else(|| {
                optional_u64_arg(
                    &call.arguments,
                    &["timeout", "timeout_seconds", "timeout_sec"],
                )
                .map(|seconds| Duration::from_secs(seconds.max(1)))
            })
            .unwrap_or(self.shell_timeout);
        // Shell calls must stay bounded. Timeout probe 2026-07-13: in build
        // contexts the model sends milliseconds ("timeout": 120000), which
        // read as seconds would hold the agent for 33 hours.
        const MAX_SHELL_TIMEOUT: Duration = Duration::from_secs(3600);
        let timeout_note = (shell_timeout > MAX_SHELL_TIMEOUT).then(|| {
            format!(
                "timeout clamped to 3600 seconds (the maximum); `timeout` is in seconds — {} looked like milliseconds",
                shell_timeout.as_secs()
            )
        });
        let shell_timeout = shell_timeout.min(MAX_SHELL_TIMEOUT);
        let Some(profile) = self.shell_profile.clone() else {
            return Ok(ToolCallResult {
                id: call.id,
                tool_name: "shell.exec".to_string(),
                ok: false,
                repaired: repaired_name,
                content: "no shell is available in this environment; use the file tools instead"
                    .to_string(),
                metadata: json!({"shell": null}),
            });
        };
        // `&&` chains and stale `cd` prefixes only break Windows PowerShell
        // 5.1; pwsh 7 and the unix shells take them as-is.
        let is_powershell = profile.kind() == crate::platform::ShellKind::WindowsPowerShell;
        let repair = repair_shell_command(&command, &self.workspace, is_powershell);
        let output = ShellTool::with_profile(&self.workspace, shell_timeout, profile)
            .with_output_limit(max_output_bytes)
            .run(&repair.command)?;

        let ok = output.exit_code == Some(0);
        // stderr carries the reason on failure (and cargo-style tools log there
        // even on success), so surface it whenever stdout alone would be blind.
        let mut content = output.stdout.clone();
        if (!ok || content.trim().is_empty()) && !output.stderr.trim().is_empty() {
            if !content.trim().is_empty() {
                content.push('\n');
            }
            content.push_str(output.stderr.trim_end());
        }
        // Quiet commands (Set-Content, mkdir, …) succeed with zero output;
        // a bare "" makes the model re-verify with extra calls (bench run6).
        if content.trim().is_empty() {
            content = match output.exit_code {
                Some(code) => format!("(command exited with code {code}; no output)"),
                None => "(command produced no output)".to_string(),
            };
        }

        // stdout/stderr already travel in `content`; repeating them here
        // would double the tokens of every shell result the model reads.
        let mut metadata = json!({
            "exit_code": output.exit_code,
            "program": output.program,
            "stdout_truncated": output.stdout_truncated,
            "stderr_truncated": output.stderr_truncated,
            "max_output_bytes": output.max_output_bytes,
        });
        let notes: Vec<String> = repair
            .note
            .iter()
            .chain(timeout_note.iter())
            .cloned()
            .collect();
        if !notes.is_empty() {
            metadata["repair_note"] = serde_json::Value::String(notes.join("; "));
        }
        if repair.note.is_some() {
            metadata["original_command"] = serde_json::Value::String(command.clone());
        }
        Ok(ToolCallResult {
            id: call.id,
            tool_name: "shell.exec".to_string(),
            ok,
            repaired: repaired_name || !notes.is_empty(),
            content,
            metadata,
        })
    }

    fn execute_file_list(
        &self,
        call: ToolCall,
        repaired_name: bool,
    ) -> Result<ToolCallResult, RuntimeError> {
        let path = optional_string_arg(&call.arguments, &["path", "dir", "directory"])
            .unwrap_or_else(|| ".".to_string());
        let max_results =
            optional_usize_arg(&call.arguments, &["max_results", "limit"]).unwrap_or(200);
        // `depth` is the model's own vocabulary (seen live): 1 = top level.
        let max_depth = optional_usize_arg(&call.arguments, &["depth", "max_depth"]);
        let show_hidden =
            optional_bool_arg(&call.arguments, &["show_hidden", "include_hidden", "all"])
                .unwrap_or(false);
        let result = FileTool::new(&self.workspace).list_files(
            &path,
            max_results,
            max_depth,
            show_hidden,
        )?;
        let content = result
            .entries
            .iter()
            .map(|entry| {
                if entry.is_dir {
                    format!("{}/", entry.path)
                } else {
                    entry.path.clone()
                }
            })
            .collect::<Vec<_>>()
            .join("\n");

        Ok(ToolCallResult {
            id: call.id,
            tool_name: "file.list".to_string(),
            ok: true,
            // `path` is optional (defaults to the workspace root), so omitting it
            // is not a deviation worth flagging; only name/argument repairs count.
            repaired: repaired_name,
            content,
            metadata: json!({
                "entries": result.entries.len(),
                "scanned": result.scanned,
                "truncated": result.truncated,
            }),
        })
    }

    fn execute_file_search(
        &self,
        call: ToolCall,
        repaired_name: bool,
    ) -> Result<ToolCallResult, RuntimeError> {
        let path = optional_string_arg(&call.arguments, &["path", "dir", "directory"])
            .unwrap_or_else(|| ".".to_string());
        let query = string_arg(
            &call.arguments,
            &[
                "query",
                "pattern",
                "text",
                "search_string",
                "literal_string",
            ],
        )?;
        let max_results =
            optional_usize_arg(&call.arguments, &["max_results", "limit"]).unwrap_or(200);
        let max_file_bytes = optional_u64_arg(&call.arguments, &["max_file_bytes", "max_bytes"])
            .unwrap_or(1024 * 1024);
        let context_lines =
            optional_usize_arg(&call.arguments, &["context_lines", "context"]).unwrap_or(0);
        let result = FileTool::new(&self.workspace).search_text_with_context(
            &path,
            &query,
            max_results,
            max_file_bytes,
            context_lines,
        )?;
        // An empty string reads as "did the search even run?" — say what
        // happened instead (bench run5: the model met bare emptiness 8
        // times while probing patterns).
        let content = if result.matches.is_empty() {
            format!(
                "no matches for '{query}' (searched {} files)",
                result.scanned_files
            )
        } else {
            result
                .matches
                .iter()
                .map(|entry| {
                    // grep -n -C rendering: context lines use dashes, the
                    // matching line uses colons.
                    let mut block = String::new();
                    for (number, text) in &entry.before {
                        block.push_str(&format!("{}-{}-{}\n", entry.path, number, text));
                    }
                    block.push_str(&format!(
                        "{}:{}:{}",
                        entry.path, entry.line_number, entry.line
                    ));
                    for (number, text) in &entry.after {
                        block.push_str(&format!("\n{}-{}-{}", entry.path, number, text));
                    }
                    block
                })
                .collect::<Vec<_>>()
                .join("\n")
        };

        Ok(ToolCallResult {
            id: call.id,
            tool_name: "file.search".to_string(),
            ok: true,
            // Accepted argument aliases (`pattern`, `text`) are first-class:
            // flagging them as repairs only generates memo noise the model
            // never learns from (seen in the bench traces).
            repaired: repaired_name,
            content,
            metadata: json!({
                "matches": result.matches.len(),
                "scanned_files": result.scanned_files,
                "skipped_large_files": result.skipped_large_files,
                "truncated": result.truncated,
            }),
        })
    }

    fn execute_file_delete(
        &self,
        call: ToolCall,
        repaired_name: bool,
    ) -> Result<ToolCallResult, RuntimeError> {
        let path = string_arg(
            &call.arguments,
            &[
                "path",
                "file",
                "filename",
                "file_path",
                "filepath",
                "target_file",
            ],
        )?;
        let result = FileTool::new(&self.workspace).delete_path(&path)?;
        Ok(ToolCallResult {
            id: call.id,
            tool_name: "file.delete".to_string(),
            ok: true,
            repaired: repaired_name,
            content: format!("deleted {}", result.path),
            metadata: json!({
                "path": result.path,
                "was_dir": result.was_dir,
            }),
        })
    }

    fn execute_file_move(
        &self,
        call: ToolCall,
        repaired_name: bool,
    ) -> Result<ToolCallResult, RuntimeError> {
        let source = string_arg(
            &call.arguments,
            &["source", "from", "path", "file", "src", "source_path"],
        )?;
        let target = string_arg(
            &call.arguments,
            &[
                "target",
                "to",
                "destination",
                "dest",
                "target_path",
                "destination_path",
            ],
        )?;
        let overwrite =
            optional_bool_arg(&call.arguments, &["overwrite", "replace"]).unwrap_or(false);
        let result = FileTool::new(&self.workspace).move_path(&source, &target, overwrite)?;
        Ok(ToolCallResult {
            id: call.id,
            tool_name: "file.move".to_string(),
            ok: true,
            repaired: repaired_name,
            content: format!("moved {} to {}", result.source_path, result.target_path),
            metadata: json!({
                "source_path": result.source_path,
                "target_path": result.target_path,
                "overwritten": result.overwritten,
            }),
        })
    }
}

impl ToolExecutor for ToolRuntime {
    fn execute(&self, call: ToolCall) -> Result<ToolCallResult, RuntimeError> {
        ToolRuntime::execute(self, call)
    }
}

#[derive(Debug, Clone)]
pub struct ToolScheduler<E = ToolRuntime> {
    executor: E,
    max_concurrency: usize,
    batch_timeout: Option<Duration>,
}

impl<E> ToolScheduler<E>
where
    E: ToolExecutor,
{
    pub fn new(executor: E) -> Self {
        Self {
            executor,
            max_concurrency: default_max_concurrency(),
            batch_timeout: None,
        }
    }

    pub fn with_max_concurrency(mut self, max_concurrency: usize) -> Self {
        self.max_concurrency = max_concurrency.max(1);
        self
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.batch_timeout = Some(timeout);
        self
    }

    pub fn execute_batch(&self, calls: Vec<ToolCall>) -> Vec<ToolBatchResult> {
        let total = calls.len();
        if total == 0 {
            return Vec::new();
        }

        let max_concurrency = self.max_concurrency.min(total);
        let originals = calls.clone();
        let mut pending = calls.into_iter().enumerate();
        let (tx, rx) = mpsc::channel();
        let mut in_flight = 0;
        let mut completed = 0;
        let mut results = vec![None; total];
        let deadline = self.batch_timeout.map(|timeout| Instant::now() + timeout);
        let mut timed_out = false;

        while in_flight < max_concurrency && !deadline_expired(deadline) {
            if let Some((index, call)) = pending.next() {
                spawn_tool_call(self.executor.clone(), index, call, tx.clone());
                in_flight += 1;
            } else {
                break;
            }
        }

        while completed < total {
            let received = match deadline {
                Some(deadline) => {
                    if deadline_expired(Some(deadline)) {
                        timed_out = true;
                        break;
                    }
                    rx.recv_timeout(deadline.saturating_duration_since(Instant::now()))
                }
                None => rx.recv().map_err(|_| RecvTimeoutError::Disconnected),
            };

            let (index, result) = match received {
                Ok(received) => received,
                Err(RecvTimeoutError::Timeout) => {
                    timed_out = true;
                    break;
                }
                Err(RecvTimeoutError::Disconnected) => {
                    panic!("tool worker terminated before reporting result");
                }
            };

            results[index] = Some(result);
            completed += 1;
            in_flight -= 1;

            while in_flight < max_concurrency {
                if deadline_expired(deadline) {
                    timed_out = true;
                    break;
                }

                if let Some((index, call)) = pending.next() {
                    spawn_tool_call(self.executor.clone(), index, call, tx.clone());
                    in_flight += 1;
                } else {
                    break;
                }
            }

            if timed_out {
                break;
            }
        }

        if timed_out {
            for (index, result) in results.iter_mut().enumerate() {
                if result.is_none() {
                    *result = Some(batch_timeout_result(&originals[index]));
                }
            }
        }

        results
            .into_iter()
            .enumerate()
            .map(|(index, result)| {
                result.unwrap_or_else(|| batch_timeout_result(&originals[index]))
            })
            .collect()
    }
}

#[derive(Debug)]
pub enum RuntimeError {
    UnknownTool(String),
    MissingArgument { names: Vec<&'static str> },
    InvalidArgument { name: &'static str },
    File(ToolError),
    Attachment(AttachmentError),
    Shell(ShellError),
}

impl fmt::Display for RuntimeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownTool(name) => write!(f, "unknown tool: {name}"),
            Self::MissingArgument { names } => {
                write!(f, "missing argument; expected one of {}", names.join(", "))
            }
            Self::InvalidArgument { name } => write!(f, "argument must be a string: {name}"),
            Self::File(err) => write!(f, "{err}"),
            Self::Attachment(err) => write!(f, "{err}"),
            Self::Shell(err) => write!(f, "{err}"),
        }
    }
}

impl Error for RuntimeError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::File(err) => Some(err),
            Self::Attachment(err) => Some(err),
            Self::Shell(err) => Some(err),
            _ => None,
        }
    }
}

impl From<ToolError> for RuntimeError {
    fn from(value: ToolError) -> Self {
        Self::File(value)
    }
}

impl From<AttachmentError> for RuntimeError {
    fn from(value: AttachmentError) -> Self {
        Self::Attachment(value)
    }
}

impl From<ShellError> for RuntimeError {
    fn from(value: ShellError) -> Self {
        Self::Shell(value)
    }
}

/// The name a canonical tool is advertised under to the model — its wire
/// name. These are the names DeepSeek (and models trained on the same
/// harness culture) produce unprompted, measured by the priors probe
/// (verb_noun order: `read_file`, not `file_read`): meeting the model's
/// training instead of making it translate ours.
pub fn wire_tool_name(canonical: &str) -> &'static str {
    match canonical {
        "file.read" => "read_file",
        "file.write" => "write_file",
        "file.append" => "append_file",
        "file.replace" => "edit_file",
        "file.list" => "list_files",
        "file.search" => "grep_search",
        "file.tail" => "tail_file",
        "file.hash" => "checksum_file",
        "file.stat" => "stat_file",
        "file.delete" => "delete_file",
        "file.move" => "move_file",
        "attachment.read" => "get_image",
        "shell.exec" => "run_shell_command",
        _ => "unknown_tool",
    }
}

/// Canonical `file.*`/`shell.exec` name for any accepted alias, for display
/// surfaces (the resolver itself stays private to the runtime). Returns `None`
/// for names the runtime would reject.
pub fn canonical_tool_name(name: &str) -> Option<String> {
    ToolResolution::from_name(name)
        .ok()
        .map(|resolution| resolution.canonical)
}

struct ToolResolution {
    canonical: String,
    repaired: bool,
}

impl ToolResolution {
    fn from_name(name: &str) -> Result<Self, RuntimeError> {
        let normalized = name.trim().to_ascii_lowercase().replace(['-', ' '], "_");
        let canonical = match normalized.as_str() {
            "file.write" | "file_write" | "write_file" => "file.write",
            "file.append" | "file_append" | "append_file" | "append_text" | "add_to_file" => {
                "file.append"
            }
            "file.replace" | "file_replace" | "file.edit" | "file_edit" | "replace_file"
            | "replace_text" | "edit_file" | "search_replace" | "edit_and_apply"
            | "replace_in_file" | "str_replace" => "file.replace",
            "file.read" | "file_read" | "read_file" => "file.read",
            "file.hash" | "file_hash" | "hash_file" | "checksum" | "checksum_file" => "file.hash",
            "file.stat" | "file_stat" | "stat_file" | "file.metadata" | "file_metadata"
            | "metadata" | "stat" | "get_file_info" => "file.stat",
            "file.tail" | "file_tail" | "tail_file" | "tail" => "file.tail",
            "file.list" | "file_list" | "list_file" | "list_files" | "ls" => "file.list",
            "file.search" | "file_search" | "search_file" | "search_files" | "grep"
            | "grep_search" => "file.search",
            "file.delete" | "file_delete" | "delete_file" | "remove_file" | "rm" => "file.delete",
            "file.move" | "file_move" | "move_file" | "rename_file" | "mv" => "file.move",
            "attachment.read" | "attachment_read" | "read_attachment" | "open_attachment"
            | "image.read" | "image_read" | "inspect_image" | "read_image" | "get_image" => {
                "attachment.read"
            }
            "shell.exec" | "shell_exec" | "run_command" | "shell" | "run_shell_command"
            | "run_shell" => "shell.exec",
            _ => return Err(RuntimeError::UnknownTool(name.to_string())),
        };

        // Tools are advertised to the model under their prior-aligned wire
        // name (`read_file`, `grep_search`, …). Calling that name is exactly
        // what was offered and is NOT a repair; only a genuinely different
        // alias (e.g. `file_read`, `grep`, `ls`) counts as one.
        let wire = wire_tool_name(canonical);
        let repaired = normalized != canonical && normalized != wire;

        Ok(Self {
            canonical: canonical.to_string(),
            repaired,
        })
    }
}

struct ShellCommandRepair {
    command: String,
    note: Option<String>,
}

/// Repair Unix-isms in a shell command before it hits Windows PowerShell 5.1:
/// drop a leading `cd <path>` into a directory that does not exist (models
/// often try `cd /mnt/<project>` — commands already run in the workspace), and
/// rewrite `&&` chains, which PowerShell 5.1 cannot parse, into
/// `; if ($?) { … }` so the run-next-only-on-success intent survives.
fn repair_shell_command(
    command: &str,
    workspace: &Path,
    is_powershell: bool,
) -> ShellCommandRepair {
    if !is_powershell {
        return ShellCommandRepair {
            command: command.to_string(),
            note: None,
        };
    }

    let mut notes: Vec<String> = Vec::new();
    let mut segments = split_outside_quotes(command, "&&");

    if segments.len() > 1
        && let Some(target) = segments[0].trim().strip_prefix("cd ").map(str::trim)
    {
        let target_path = Path::new(target);
        let resolved = if target_path.is_absolute() {
            target_path.to_path_buf()
        } else {
            workspace.join(target_path)
        };
        if !resolved.exists() {
            let target = target.to_string();
            segments.remove(0);
            notes.push(format!(
                "dropped `cd {target}` (no such directory; commands already run in the workspace)"
            ));
        }
    }

    let command = if segments.len() > 1 {
        notes.push(
            "rewrote '&&' (not supported by Windows PowerShell 5.1) as `; if ($?) {{ ... }}`"
                .to_string(),
        );
        chain_powershell_segments(&segments)
    } else {
        segments.join("").trim().to_string()
    };

    ShellCommandRepair {
        command,
        note: if notes.is_empty() {
            None
        } else {
            Some(notes.join("; "))
        },
    }
}

/// Join command segments so each next one runs only if the previous succeeded:
/// `A && B && C` becomes `A; if ($?) { B; if ($?) { C } }`.
fn chain_powershell_segments(segments: &[String]) -> String {
    let mut iter = segments.iter().rev();
    let mut chained = iter
        .next()
        .map(|s| s.trim().to_string())
        .unwrap_or_default();
    for segment in iter {
        chained = format!("{}; if ($?) {{ {chained} }}", segment.trim());
    }
    chained
}

/// Split `command` on `separator` occurrences that sit outside single/double
/// quotes, so a literal `&&` inside a quoted argument is left alone.
fn split_outside_quotes(command: &str, separator: &str) -> Vec<String> {
    let chars: Vec<char> = command.chars().collect();
    let sep: Vec<char> = separator.chars().collect();
    let mut segments = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    let mut i = 0;
    while i < chars.len() {
        let ch = chars[i];
        match quote {
            Some(open) => {
                if ch == open {
                    quote = None;
                }
                current.push(ch);
                i += 1;
            }
            None if ch == '\'' || ch == '"' => {
                quote = Some(ch);
                current.push(ch);
                i += 1;
            }
            None if chars[i..].starts_with(&sep[..]) => {
                segments.push(std::mem::take(&mut current));
                i += sep.len();
            }
            None => {
                current.push(ch);
                i += 1;
            }
        }
    }
    segments.push(current);
    segments
}

struct ArgumentRepair {
    arguments: Value,
    repaired: bool,
}

fn repair_tool_arguments(canonical: &str, arguments: Value) -> ArgumentRepair {
    match arguments {
        Value::String(raw) => raw_tool_arguments(canonical, &raw).unwrap_or(ArgumentRepair {
            arguments: Value::String(raw),
            repaired: false,
        }),
        Value::Object(mut object) => {
            let mut repaired = false;

            // Expand a leftover `_raw_arguments` string into structured keys.
            if let Some(raw) = object
                .get("_raw_arguments")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
                && let Some(raw_repair) = raw_tool_arguments(canonical, &raw)
                && let Value::Object(expanded) = raw_repair.arguments
            {
                object.remove("_raw_arguments");
                for (key, value) in expanded {
                    object.entry(key).or_insert(value);
                }
                repaired = true;
            }

            // Drop keys the model set to `null`: it passed an empty value where
            // it should have omitted the parameter entirely. Executing anyway and
            // flagging the repair lets the model learn the correct shape.
            let null_keys: Vec<String> = object
                .iter()
                .filter(|(_, value)| value.is_null())
                .map(|(key, _)| key.clone())
                .collect();
            if !null_keys.is_empty() {
                for key in &null_keys {
                    object.remove(key);
                }
                repaired = true;
            }

            ArgumentRepair {
                arguments: Value::Object(object),
                repaired,
            }
        }
        other => ArgumentRepair {
            arguments: other,
            repaired: false,
        },
    }
}

fn raw_tool_arguments(canonical: &str, raw: &str) -> Option<ArgumentRepair> {
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }

    if let Some(parsed) = parse_raw_key_value_arguments(raw) {
        return Some(ArgumentRepair {
            arguments: Value::Object(parsed),
            repaired: true,
        });
    }

    let key = match canonical {
        "shell.exec" => "command",
        "file.read" | "file.hash" | "file.stat" | "file.tail" | "file.delete"
        | "attachment.read" => "path",
        "file.list" => "path",
        "file.search" => "query",
        _ => return None,
    };

    Some(ArgumentRepair {
        arguments: json!({ key: raw }),
        repaired: true,
    })
}

fn parse_raw_key_value_arguments(raw: &str) -> Option<Map<String, Value>> {
    let mut object = Map::new();
    for line in raw.lines().map(str::trim).filter(|line| !line.is_empty()) {
        let Some((key, value)) = split_key_value(line) else {
            object.clear();
            break;
        };
        object.insert(
            normalize_argument_key(key),
            Value::String(value.to_string()),
        );
    }

    if !object.is_empty() {
        return Some(object);
    }

    for token in raw.split_whitespace() {
        let Some((key, value)) = split_key_value(token) else {
            continue;
        };
        object.insert(
            normalize_argument_key(key),
            Value::String(value.to_string()),
        );
    }

    (!object.is_empty()).then_some(object)
}

fn split_key_value(value: &str) -> Option<(&str, &str)> {
    let (key, value) = value.split_once(':').or_else(|| value.split_once('='))?;
    let key = key.trim();
    let value = value.trim().trim_matches('"').trim_matches('\'');
    (!key.is_empty() && !value.is_empty()).then_some((key, value))
}

fn normalize_argument_key(key: &str) -> String {
    key.trim()
        .to_ascii_lowercase()
        .replace(['-', ' '], "_")
        .trim_matches('_')
        .to_string()
}

fn string_arg(value: &Value, names: &[&'static str]) -> Result<String, RuntimeError> {
    for name in names {
        if let Some(value) = value.get(*name) {
            return value
                .as_str()
                .map(ToOwned::to_owned)
                .ok_or(RuntimeError::InvalidArgument { name });
        }
    }

    Err(RuntimeError::MissingArgument {
        names: names.to_vec(),
    })
}

fn optional_string_arg(value: &Value, names: &[&str]) -> Option<String> {
    names
        .iter()
        .find_map(|name| value.get(*name).and_then(Value::as_str))
        .map(ToOwned::to_owned)
}

fn optional_usize_arg(value: &Value, names: &[&str]) -> Option<usize> {
    names
        .iter()
        .find_map(|name| value.get(*name).and_then(coerce_u64))
        .and_then(|value| usize::try_from(value).ok())
}

fn optional_u64_arg(value: &Value, names: &[&str]) -> Option<u64> {
    names
        .iter()
        .find_map(|name| value.get(*name).and_then(coerce_u64))
}

fn optional_bool_arg(value: &Value, names: &[&str]) -> Option<bool> {
    names
        .iter()
        .find_map(|name| value.get(*name).and_then(coerce_bool))
}

fn coerce_u64(value: &Value) -> Option<u64> {
    match value {
        Value::Number(number) => number.as_u64(),
        Value::String(text) => text.trim().parse::<u64>().ok(),
        _ => None,
    }
}

fn coerce_bool(value: &Value) -> Option<bool> {
    match value {
        Value::Bool(value) => Some(*value),
        Value::String(text) => match text.trim().to_ascii_lowercase().as_str() {
            "true" | "yes" | "y" | "1" => Some(true),
            "false" | "no" | "n" | "0" => Some(false),
            _ => None,
        },
        _ => None,
    }
}

fn default_max_concurrency() -> usize {
    thread::available_parallelism()
        .map(|parallelism| parallelism.get().min(8))
        .unwrap_or(4)
        .max(1)
}

fn deadline_expired(deadline: Option<Instant>) -> bool {
    deadline.is_some_and(|deadline| Instant::now() >= deadline)
}

fn batch_timeout_result(call: &ToolCall) -> ToolBatchResult {
    ToolBatchResult {
        id: call.id.clone(),
        tool_name: call.name.clone(),
        ok: false,
        repaired: false,
        content: String::new(),
        metadata: json!({
            "cancelled": true,
            "reason": "batch_timeout",
        }),
        error: Some("tool batch timed out".to_string()),
        hint: None,
    }
}

/// JSON Schema for a tool's arguments: `(name, type, description)` triples.
/// `additionalProperties` stays true — the forgiving runtime accepts alias
/// names, and a strict schema would make validating providers reject them.
fn tool_schema(
    required: &[(&str, &str, &str)],
    optional: &[(&str, &str, &str)],
) -> serde_json::Value {
    let mut properties = serde_json::Map::new();
    for (name, kind, description) in required.iter().chain(optional) {
        properties.insert(
            name.to_string(),
            json!({"type": kind, "description": description}),
        );
    }
    json!({
        "type": "object",
        "properties": properties,
        "required": required.iter().map(|(name, _, _)| *name).collect::<Vec<_>>(),
        "additionalProperties": true,
    })
}

/// The canonical argument shape for each tool, used to build a repair memo.
fn canonical_tool_usage(canonical: &str) -> &'static str {
    match canonical {
        "file.write" | "file.append" => r#"{"file_path": "<file>", "content": "<text>"}"#,
        "file.replace" => {
            r#"{"file_path": "<file>", "old_string": "<text>", "new_string": "<text>"}"#
        }
        "file.read" | "file.hash" | "file.stat" | "file.tail" | "file.delete"
        | "attachment.read" => r#"{"file_path": "<file>"}"#,
        "file.list" => r#"{"path": "<dir>"}"#,
        "file.search" => r#"{"query": "<text>", "path": "<dir>"}"#,
        "file.move" => r#"{"source": "<file>", "destination": "<file>"}"#,
        "shell.exec" => r#"{"command": "<text>"}"#,
        _ => "",
    }
}

/// Build the memo returned to the model after a tolerated repair so it can
/// learn the canonical call instead of repeating the same mistake.
///
/// The memo always speaks the model's language: it references the API wire name
/// (`file_search`), never the internal dotted name (`file.search`), because dots
/// are illegal in tool names and the model could not call them. It also
/// distinguishes a genuine name alias from an argument-only correction so the
/// advice matches what actually happened.
fn repair_hint(requested: &str, canonical: &str) -> String {
    let wire = wire_tool_name(canonical).to_string();
    let usage = canonical_tool_usage(canonical);
    let normalized = requested
        .trim()
        .to_ascii_lowercase()
        .replace(['-', ' '], "_");
    let name_was_aliased = normalized != canonical && normalized != wire;
    if name_was_aliased {
        format!(
            "Call accepted after auto-correcting tool name '{requested}' to '{wire}'. Next time call '{wire}' with arguments like {usage}."
        )
    } else {
        format!(
            "Call accepted after auto-correcting its arguments. Next time call '{wire}' with arguments like {usage}."
        )
    }
}

fn spawn_tool_call<E>(
    executor: E,
    index: usize,
    call: ToolCall,
    tx: Sender<(usize, ToolBatchResult)>,
) where
    E: ToolExecutor,
{
    thread::spawn(move || {
        let original = call.clone();
        let result = ToolBatchResult::from_execution(&original, executor.execute(call));
        let _ = tx.send((index, result));
    });
}
