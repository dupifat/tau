//! Filesystem and shell tool extension.
//!
//! Provides `read`, `edit`, `apply_patch`, `dir_lock`, `grep`, `find`, `ls`,
//! `shell`, and `gpt_shell` tools.
//!
//! The `echo` tool is available under `cfg(test)` or the
//! `echo-agent` cargo feature for harness-side echo-agent tests.

use std::collections::HashMap;
use std::error::Error;
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::Path;
use std::sync::{Arc, Mutex, mpsc};

use tau_proto::{
    Ack, ActionError, ActionInvoke, ActionOutput, ActionResult, AgentContextKey, AgentContextValue,
    ConfigError, Event, EventLogSeq, ExtAgentContextPublish, ExtPromptFragmentPublish,
    ExtensionContextReady, Frame, FrameReader, FrameWriter, Message, PromptContent, PromptFragment,
    PromptPriority, SessionAgentLoaded, SessionStarted, ToolCancelled, ToolResult, ToolResultKind,
    ToolSpec,
};
use tracing::{debug, trace};

mod agents;
mod argument;
mod config;
mod diff;
mod dir_lock;
mod display;
mod isolation;
mod semaphore;
mod tools;
mod truncate;

#[cfg(test)]
mod tests;

use crate::agents::{ancestor_dirs, discover_session_agents_files};
use crate::config::{ExtConfig, ShellConfig};
use crate::dir_lock::{DIR_LOCK_TOOL_NAME, DirLockManager};
use crate::semaphore::Semaphore;
#[cfg(any(test, feature = "echo-agent"))]
use crate::tools::ECHO_TOOL_NAME;
use crate::tools::{
    APPLY_PATCH_TOOL_NAME, EDIT_TOOL_NAME, FIND_TOOL_NAME, GPT_SHELL_TOOL_NAME, GREP_TOOL_NAME,
    LS_TOOL_NAME, READ_TOOL_NAME, SHELL_TOOL_NAME, execute_tool,
};

const SHELL_DIR_FORCE_UNLOCK_ACTION_ID: &str = "shell.dir.force_unlock";

/// Runs the extension on stdin/stdout.
pub fn run_stdio() -> Result<(), Box<dyn Error>> {
    run_impl(std::io::stdin(), std::io::stdout())
}

/// Runs the extension over arbitrary reader/writer streams.
///
/// The test-only `echo` tool is registered when built with
/// `cfg(test)` or the `echo-agent` cargo feature.
pub fn run<R, W>(reader: R, writer: W) -> Result<(), Box<dyn Error>>
where
    R: Read,
    W: Write + Send + 'static,
{
    run_impl(reader, writer)
}

fn run_impl<R, W>(reader: R, writer: W) -> Result<(), Box<dyn Error>>
where
    R: Read,
    W: Write + Send + 'static,
{
    let mut reader = FrameReader::new(BufReader::new(reader));
    let mut writer = FrameWriter::new(BufWriter::new(writer));

    #[cfg(any(test, feature = "echo-agent"))]
    let echo_tool = Some(ToolSpec {
        name: tau_proto::ToolName::new(ECHO_TOOL_NAME),
        model_visible_name: None,
        description: Some("Echo the provided payload unchanged".to_owned()),
        tool_type: tau_proto::ToolType::Function,
        parameters: None,
        format: None,
        enabled_by_default: false,
        background_support: None,
    });
    #[cfg(not(any(test, feature = "echo-agent")))]
    let echo_tool: Option<ToolSpec> = None;
    let mut config = ExtConfig::default();
    let tools = echo_tool.into_iter().chain([
        ToolSpec {
            name: tau_proto::ToolName::new(READ_TOOL_NAME),
            model_visible_name: None,
            description: Some(
                "Reads a file. Defaults to reading the whole file in one call — \
                 output is capped at 2000 lines / 50 KB. Truncated output keeps \
                 the first 1000 and last 1000 lines separated by a literal `...` line. \
                 Prefer one full read. Pass `start_line`/`line_count` only to \
                 fetch one specific known slice, or `ranges` for up to 100 disjoint slices; \
                 range chunks are separated by one empty line. Returned content lines are prefixed \
                 by their 1-based line number and a space; \
                 CRLF, CR, and missing final line endings are marked after the number, e.g. \
                 `2(crlf)`, `3(cr)`, or `4(no_nl)`. Invalid UTF-8 and lines that would exceed \
                 the 50 KB output budget are marker-only, e.g. `1(invalid-utf8)` or \
                 `1(truncated)`. Truncated results include `truncated: true`, `total_lines`, \
                 and `total_bytes`; `valid_utf8: false` is included only when applicable."
                    .to_owned(),
            ),
            tool_type: tau_proto::ToolType::Function,
            parameters: Some(serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path to the file"
                    },
                    "start_line": {
                        "type": "integer",
                        "description": "Optional, 1-based. Omit to start at line 1 (the default)."
                    },
                    "line_count": {
                        "type": "integer",
                        "description": "Optional. Omit to read to end of file (the default and preferred mode). Set this only to continue past a previous truncation, or to fetch a known specific slice of a large file — do NOT pre-slice an ordinary file you haven't already established is large."
                    },
                    "ranges": {
                        "type": "array",
                        "description": "Optional list of disjoint line ranges to read. Cannot be combined with top-level start_line or line_count. Each chunk is separated by one empty line in the output.",
                        "minItems": 1,
                        "maxItems": 100,
                        "items": {
                            "type": "object",
                            "properties": {
                                "start_line": {
                                    "type": "integer",
                                    "minimum": 1,
                                    "description": "1-based inclusive start line to read."
                                },
                                "line_count": {
                                    "type": "integer",
                                    "minimum": 1,
                                    "description": "Number of lines to read starting at start_line."
                                }
                            },
                            "required": ["start_line", "line_count"],
                            "additionalProperties": false
                        }
                    }
                },
                "required": ["path"],
                "additionalProperties": false
            })),
            format: None,
            enabled_by_default: true,
            background_support: None,
        },
        ToolSpec {
            name: tau_proto::ToolName::new(EDIT_TOOL_NAME),
            model_visible_name: None,
            description: Some(
                "Edit a file using line-oriented replacements. Each edit replaces \
                 `line_count` original lines starting at 1-based `start_line` with \
                 `newText`, and all edits use the original file's line numbering as if \
                 applied simultaneously. Ranges must be non-overlapping and within the \
                 file's available line range; line 1 is always available for an empty \
                 or missing file, and the line after a trailing newline is available for \
                 appends. Missing files are treated as empty and missing parent \
                 directories are created, so use line 1 with line_count 1 to create a file. \
                 Optional per-edit `guard` must exactly match the first original line content, \
                 excluding the line ending, or the edit fails and returns the current range contents. \
                 Returns minimal status headers: replacements, changed, available_lines \
                 (highest valid start_line after the edit), and total_bytes."
                    .to_owned(),
            ),
            tool_type: tau_proto::ToolType::Function,
            parameters: Some(serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path to the file"
                    },
                    "edits": {
                        "type": "array",
                        "description": "One or more line ranges to replace in the original file",
                        "minItems": 1,
                        "maxItems": 100,
                        "items": {
                            "type": "object",
                            "properties": {
                                "start_line": {
                                    "type": "integer",
                                    "minimum": 1,
                                    "description": "1-based inclusive start line to replace. Line 1 is valid for an empty or missing file, and the line after a trailing newline is valid for appending."
                                },
                                "line_count": {
                                    "type": "integer",
                                    "minimum": 1,
                                    "description": "Number of original lines to replace starting at start_line. Use 1 on an empty or append line."
                                },
                                "newText": {
                                    "type": "string",
                                    "description": "Replacement text, written verbatim. Embed real newlines directly — do NOT use backslash-n escape sequences."
                                },
                                "guard": {
                                    "type": "string",
                                    "description": "Optional exact expected content of the first original line in this range, excluding the line ending. Use an empty string for an empty, missing, or append line. If it does not match, the edit fails and returns the current requested range contents."
                                }
                            },
                            "required": ["start_line", "line_count", "newText"],
                            "additionalProperties": false
                        }
                    }
                },
                "required": ["path", "edits"],
                "additionalProperties": false
            })),
            format: None,
            enabled_by_default: true,
            background_support: None,
        },
        ToolSpec {
            name: tau_proto::ToolName::new(APPLY_PATCH_TOOL_NAME),
            model_visible_name: None,
            description: Some(
                "Use the `apply_patch` tool to edit files. This is a FREEFORM tool, so do not wrap the patch in JSON."
                    .to_owned(),
            ),
            tool_type: tau_proto::ToolType::Custom,
            parameters: None,
            format: Some(tau_proto::ToolFormat::Grammar {
                syntax: tau_proto::ToolGrammarSyntax::Lark,
                definition: crate::tools::apply_patch::APPLY_PATCH_LARK_GRAMMAR.to_owned(),
            }),
            enabled_by_default: false,
            background_support: None,
        },
        dir_lock_tool_spec(config.dir_lock.enable),
        ToolSpec {
            name: tau_proto::ToolName::new(GREP_TOOL_NAME),
            model_visible_name: None,
            description: Some(
                "Search file contents for a pattern using ripgrep. Patterns are literal by default; \
                 regex metacharacters like `|` require `regex: true`. Returns matching lines \
                 with file paths and line numbers. Respects .gitignore. Output is truncated at \
                 `limit` matches or 50KB. Long lines are truncated to 500 chars."
                    .to_owned(),
            ),
            tool_type: tau_proto::ToolType::Function,
            parameters: Some(serde_json::json!({
                "type": "object",
                "properties": {
                    "pattern": {
                        "type": "string",
                        "description": "Search pattern. Treated as a literal string by default. Set `regex: true` to interpret as a regex."
                    },
                    "path": {
                        "type": "string",
                        "description": "Directory or file to search (default: current directory)"
                    },
                    "glob": {
                        "type": "string",
                        "description": "Filter files by glob pattern, e.g. '*.ts' or '**/*.rs'"
                    },
                    "ignoreCase": {
                        "type": "boolean",
                        "description": "Case-insensitive search (default: false)"
                    },
                    "regex": {
                        "type": "boolean",
                        "description": "Interpret `pattern` as a regex instead of a literal string (default: false)"
                    },
                    "context": {
                        "type": "integer",
                        "description": "Number of lines to show before and after each match (default: 0)"
                    },
                    "limit": {
                        "type": "integer",
                        "description": "Maximum number of matches to return (default: 100)"
                    }
                },
                "required": ["pattern"],
                "additionalProperties": false
            })),
            format: None,
            enabled_by_default: true,
            background_support: None,
        },
        ToolSpec {
            name: tau_proto::ToolName::new(FIND_TOOL_NAME),
            model_visible_name: None,
            description: Some(
                "Search for files by glob pattern. Returns only file paths (directories are \
                 never included, even with '**/*') relative to the search directory. Respects \
                 .gitignore. Output is truncated at `limit` results or 50KB. Use the ls tool \
                 if you want to see directory entries."
                    .to_owned(),
            ),
            tool_type: tau_proto::ToolType::Function,
            parameters: Some(serde_json::json!({
                "type": "object",
                "properties": {
                    "pattern": {
                        "type": "string",
                        "description": "Glob pattern matched against file paths relative to `path`. `**` matches any number of intermediate directories, including zero — so `**/*.rs` finds both top-level `a.rs` and nested `src/a.rs`. Directories are not returned, even with `**/*`."
                    },
                    "path": {
                        "type": "string",
                        "description": "Directory to search (default: current directory)"
                    },
                    "limit": {
                        "type": "integer",
                        "description": "Maximum number of results to return (default: 1000)"
                    }
                },
                "required": ["pattern"],
                "additionalProperties": false
            })),
            format: None,
            enabled_by_default: true,
            background_support: None,
        },
        ToolSpec {
            name: tau_proto::ToolName::new(LS_TOOL_NAME),
            model_visible_name: None,
            description: Some(
                "List directory contents. Returns entries sorted alphabetically, with '/' suffix \
                 for directories. Includes dotfiles. Output is truncated at `limit` entries or 50KB."
                    .to_owned(),
            ),
            tool_type: tau_proto::ToolType::Function,
            parameters: Some(serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Directory to list (default: current directory)"
                    },
                    "limit": {
                        "type": "integer",
                        "description": "Maximum number of entries to return (default: 500)"
                    }
                },
                "additionalProperties": false
            })),
            format: None,
            enabled_by_default: true,
            background_support: None,
        },
        ToolSpec {
            name: tau_proto::ToolName::new(SHELL_TOOL_NAME),
            model_visible_name: None,
            description: Some(
                "Execute a shell command via `sh -c`. Set `mode` to `rw` for commands \
                 that may modify files, or `ro` for read-only commands. Non-zero exits and timeouts \
                 are tool errors with output details. Output is capped at 2000 lines / \
                 50 KB; truncated output keeps the first 1000 and last 1000 lines \
                 separated by a literal `...` line. Output lines are prefixed with `out ` \
                 for stdout or `err ` for stderr; missing trailing newlines are marked, e.g. \
                 `out(no_nl)`. Invalid UTF-8 and lines that would exceed the 50 KB output \
                 budget are marker-only, e.g. `out(invalid-utf8)` or `err(truncated)`. \
                 Truncated results include `truncated: true`, `total_lines`, and `total_bytes`. \
                 Commands taking longer than 5 seconds include duration metadata. Prefer dedicated \
                 tools like `read`, `grep`, and `find` when they fit."
                    .to_owned(),
            ),
            tool_type: tau_proto::ToolType::Function,
            parameters: Some(serde_json::json!({
                "type": "object",
                "properties": {
                    "mode": {
                        "type": "string",
                        "enum": ["ro", "rw"],
                        "description": "Filesystem access intent: `ro` for read-only commands, `rw` for commands that may modify files"
                    },
                    "command": {
                        "type": "string",
                        "description": "The shell command to execute"
                    },
                    "timeout": {
                        "type": "integer",
                        "minimum": 0,
                        "description": "Timeout in seconds. The command is killed if it exceeds this. Default: 120"
                    },
                    "cwd": {
                        "type": "string",
                        "description": "Optional working directory for the command"
                    }
                },
                "required": ["mode", "command"],
                "additionalProperties": false
            })),
            format: None,
            enabled_by_default: true,
            background_support: None,
        },
        ToolSpec {
            name: tau_proto::ToolName::new(GPT_SHELL_TOOL_NAME),
            model_visible_name: Some(tau_proto::ToolName::new("shell_command")),
            description: Some(
                "Run a shell command. Set `mode` to `rw` for commands that may modify files, \
                 or `ro` for read-only commands. Non-zero exits and timeouts are returned as structured \
                 command results with output details. Output is capped at 2000 lines / 50 KB; \
                 truncated output keeps the first 1000 and last 1000 lines separated by `...`. \
                 Output lines are prefixed with `out ` for stdout or `err ` for stderr; missing \
                 trailing newlines are marked with `(no_nl)`. Invalid UTF-8 and lines that \
                 would exceed the 50 KB output budget are marker-only. Truncated results \
                 include `truncated: true`, `total_lines`, and `total_bytes`. Commands taking \
                 longer than 5 seconds include approximate duration metadata. For file changes, \
                 prefer apply_patch."
                    .to_owned(),
            ),
            tool_type: tau_proto::ToolType::Function,
            parameters: Some(serde_json::json!({
                "type": "object",
                "properties": {
                    "mode": {
                        "type": "string",
                        "enum": ["ro", "rw"],
                        "description": "Filesystem access intent: `ro` for read-only commands, `rw` for commands that may modify files"
                    },
                    "command": {
                        "type": "string",
                        "description": "The shell command to execute"
                    },
                    "timeout": {
                        "type": "integer",
                        "minimum": 0,
                        "description": "Timeout in seconds. The command is killed if it exceeds this. Default: 120"
                    },
                    "cwd": {
                        "type": "string",
                        "description": "Optional working directory for the command"
                    }
                },
                "required": ["mode", "command"],
                "additionalProperties": false
            })),
            format: None,
            enabled_by_default: false,
            background_support: None,
        },
    ]);

    // No past events requested: the shell starts from fresh live state.
    // Replaying old invokes/commands would repeat work; old session starts
    // would duplicate context publication.
    let mut handshake = tau_extension::Handshake::tool("tau-ext-shell").subscribe([
        tau_proto::EventName::TOOL_STARTED,
        tau_proto::EventName::TOOL_CANCEL_REQUEST,
        tau_proto::EventName::ACTION_INVOKE,
        tau_proto::EventName::SESSION_STARTED,
        tau_proto::EventName::SESSION_AGENT_LOADED,
        tau_proto::EventName::SESSION_AGENT_UNLOADED,
        tau_proto::EventName::SESSION_SHUTDOWN,
        tau_proto::EventName::AGENT_START_ACCEPTED,
        tau_proto::EventName::AGENT_START_RESULT,
        tau_proto::EventName::UI_SHELL_COMMAND,
    ]);
    for tool in tools {
        handshake = handshake.register_tool(tool);
    }
    handshake = handshake.announce_event(Event::ExtensionContextProviderRegister(
        tau_proto::ExtensionContextProviderRegister {},
    ));
    handshake
        .announce_event(Event::ExtPromptFragmentPublish(ExtPromptFragmentPublish {
            fragment: shell_cwd_prompt_fragment(),
        }))
        .publish_actions(shell_action_schema())
        .ready_message("filesystem and shell tools ready")
        .run(&mut writer)?;

    // Response channel: worker threads send frames here, writer thread
    // drains them onto the wire.
    let (tx, rx) = mpsc::channel::<Frame>();
    let sem = Arc::new(Semaphore::new(16));
    let running_shells = Arc::new(Mutex::new(
        HashMap::<tau_proto::ToolCallId, mpsc::Sender<()>>::new(),
    ));
    let lock_manager = DirLockManager::default();
    let mut start_agent_owners = HashMap::<String, tau_proto::AgentId>::new();

    // Writer thread: drains response frames and writes them to the wire.
    let writer_handle = std::thread::spawn(move || -> Result<(), Box<dyn Error + Send>> {
        for frame in rx {
            writer
                .write_frame(&frame)
                .map_err(|e| -> Box<dyn Error + Send> { Box::new(e) })?;
            writer
                .flush()
                .map_err(|e| -> Box<dyn Error + Send> { Box::new(e) })?;
        }
        Ok(())
    });

    // Reader loop: dispatch each owned tool invocation to a worker thread.
    // ToolStarted is a subscribed event-log delivery, so it arrives as a
    // LogEvent and must be acked after processing like other subscribed events.
    while let Some(frame) = reader.read_frame()? {
        let (log_id, inner) = frame.peel_log();
        match inner {
            Frame::Message(Message::Configure(msg)) => {
                match tau_extension::parse_config::<ExtConfig>(&msg.config) {
                    Ok(cfg) => {
                        let dir_lock_changed = config.dir_lock.enable != cfg.dir_lock.enable;
                        config = cfg;
                        if dir_lock_changed {
                            tx.send(Frame::Event(Event::ToolRegister(tau_proto::ToolRegister {
                                tool: dir_lock_tool_spec(config.dir_lock.enable),
                                prompt_fragment: None,
                            })))?;
                        }
                    }
                    Err(message) => {
                        tx.send(Frame::Message(Message::ConfigError(ConfigError {
                            message,
                        })))?;
                    }
                }
            }
            Frame::Event(Event::ToolStarted(invoke)) => {
                if !is_shell_tool(invoke.tool_name.as_str()) {
                    ack_if_logged(log_id, &tx)?;
                    continue;
                }
                let tx = tx.clone();
                let shell_config = config.shell.clone();
                let running_shells = Arc::clone(&running_shells);
                if invoke.tool_name == DIR_LOCK_TOOL_NAME {
                    let lock_manager = lock_manager.clone();
                    let enabled = config.dir_lock.enable;
                    std::thread::spawn(move || {
                        crate::dir_lock::dispatch_dir_lock_tool(
                            invoke,
                            &lock_manager,
                            enabled,
                            &tx,
                        );
                    });
                } else if config.dir_lock.enable
                    && is_dir_lock_update_tool(invoke.tool_name.as_str())
                {
                    let lock_manager = lock_manager.clone();
                    let sem = Arc::clone(&sem);
                    std::thread::spawn(move || {
                        dispatch_locked_tool_invoke(
                            invoke,
                            shell_config,
                            &tx,
                            &running_shells,
                            &lock_manager,
                            &sem,
                        );
                    });
                } else {
                    // Block here until a permit is free. This bounds the
                    // total number of in-flight worker threads — without
                    // it, a burst of ToolStarted events would spawn unbounded
                    // native threads that then serialize on the semaphore.
                    let permit = sem.acquire();
                    std::thread::spawn(move || {
                        let _permit = permit;
                        dispatch_tool_invoke(invoke, shell_config, &tx, &running_shells);
                    });
                }
            }
            Frame::Event(Event::SessionStarted(started)) => {
                dispatch_session_started(started, &tx);
            }
            Frame::Event(Event::SessionAgentLoaded(loaded)) => {
                dispatch_session_agent_loaded(loaded, &tx);
            }
            Frame::Event(Event::SessionAgentUnloaded(unloaded)) => {
                lock_manager.release_agent(&unloaded.agent_id);
                start_agent_owners.retain(|_, agent_id| agent_id != &unloaded.agent_id);
            }
            Frame::Event(Event::SessionShutdown(_)) => {
                lock_manager.release_all_manual();
                start_agent_owners.clear();
            }
            Frame::Event(Event::StartAgentAccepted(accepted)) => {
                start_agent_owners.insert(accepted.query_id, accepted.agent_id);
            }
            Frame::Event(Event::StartAgentResult(result)) => {
                if let Some(agent_id) = start_agent_owners.remove(&result.query_id) {
                    lock_manager.release_agent(&agent_id);
                }
            }
            Frame::Event(Event::ActionInvoke(invoke)) => {
                tx.send(Frame::Event(dispatch_action_invoke(invoke, &lock_manager)))?;
            }
            Frame::Event(Event::ToolCancelRequest(request)) => {
                let cancel_tx = running_shells
                    .lock()
                    .expect("running shell registry lock poisoned")
                    .get(&request.target_call_id)
                    .cloned();
                if let Some(cancel_tx) = cancel_tx {
                    debug!(call_id = %request.target_call_id, "shell cancellation requested for running call");
                    if cancel_tx.send(()).is_err() {
                        debug!(call_id = %request.target_call_id, "shell cancellation receiver already gone");
                    }
                } else if lock_manager.cancel_waiting_call(&request.target_call_id) {
                    debug!(call_id = %request.target_call_id, "cancellation requested for waiting dir-lock call");
                } else {
                    debug!(call_id = %request.target_call_id, "shell cancellation requested for unknown call");
                }
            }
            Frame::Event(Event::UiShellCommand(cmd)) => {
                // User-initiated `!`/`!!` — run on a worker thread
                // and stream chunks out via the same tx writer.
                let permit = sem.acquire();
                let tx = tx.clone();
                let shell_config = config.shell.clone();
                std::thread::spawn(move || {
                    let _permit = permit;
                    crate::tools::shell::dispatch_user_shell_command(cmd, shell_config, &tx);
                });
            }
            Frame::Message(Message::Disconnect(_)) => break,
            _ => {}
        }
        if let Some(id) = log_id {
            ack_log_event(id, &tx);
        }
    }

    // Drop the sender so the writer thread exits.
    drop(tx);
    writer_handle
        .join()
        .map_err(|_| "writer thread panicked")?
        .map_err(|e| -> Box<dyn Error> { e })?;
    Ok(())
}

fn dir_lock_tool_spec(enabled_by_default: bool) -> ToolSpec {
    ToolSpec {
        name: tau_proto::ToolName::new(DIR_LOCK_TOOL_NAME),
        model_visible_name: None,
        description: Some(
            "Acquire or release an ext-shell directory update lock. Enabled by default; set ext-shell config `dir_lock.enable` to false to opt out. Commands are `update` and `unlock`, and `directory` must be an existing directory. `unlock` normally releases the caller's lock; pass `owner_agent_id` to release an abandoned lock held by another agent."
                .to_owned(),
        ),
        tool_type: tau_proto::ToolType::Function,
        parameters: Some(serde_json::json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "enum": ["update", "unlock"],
                    "description": "Lock or unlock the directory for updates"
                },
                "directory": {
                    "type": "string",
                    "description": "Existing directory to canonicalize before locking"
                },
                "owner_agent_id": {
                    "type": "string",
                    "description": "Optional owner agent id for force-unlocking a manual lock held by another agent"
                }
            },
            "required": ["command", "directory"],
            "additionalProperties": false
        })),
        format: None,
        enabled_by_default,
        background_support: None,
    }
}

fn shell_action_schema() -> tau_actions::ActionSchema {
    tau_actions::ActionSchema {
        version: tau_actions::ACTION_SCHEMA_VERSION,
        roots: vec![tau_actions::ActionCommand {
            name: "/shell-dir-force-unlock".to_owned(),
            description: "Force-release ext-shell manual directory locks overlapping a directory"
                .to_owned(),
            action_id: Some(SHELL_DIR_FORCE_UNLOCK_ACTION_ID.to_owned()),
            args: vec![tau_actions::ActionArg {
                name: "directory".to_owned(),
                description: "Existing directory whose overlapping manual locks should be released"
                    .to_owned(),
                required: true,
                suggestions: Vec::new(),
                kind: tau_actions::ActionArgKind::RestString,
            }],
            children: Vec::new(),
        }],
    }
}

fn dispatch_action_invoke(invoke: ActionInvoke, lock_manager: &DirLockManager) -> Event {
    if invoke.action_id != SHELL_DIR_FORCE_UNLOCK_ACTION_ID {
        return action_error(invoke, "unknown shell action".to_owned());
    }
    let Some(directory) = invoke.argv.first().map(String::as_str) else {
        return action_error(invoke, "missing directory argument".to_owned());
    };
    let dir = match crate::dir_lock::canonical_existing_dir(Path::new(directory)) {
        Ok(dir) => dir,
        Err(message) => return action_error(invoke, message),
    };
    let removed = lock_manager.force_unlock_overlapping(&dir);
    if removed.is_empty() {
        return action_error(
            invoke,
            format!("no manual directory locks overlap {}", dir.display()),
        );
    }

    let mut lines = vec![format!(
        "Force-unlocked {} manual directory lock(s) overlapping {}.",
        removed.len(),
        dir.display()
    )];
    for entry in removed {
        lines.push(format!("{} owner={}", entry.dir.display(), entry.owner));
    }
    Event::ActionResult(ActionResult {
        invocation_id: invoke.invocation_id,
        action_id: invoke.action_id,
        output: ActionOutput::Text {
            text: lines.join("\n"),
        },
    })
}

fn action_error(invoke: ActionInvoke, message: String) -> Event {
    Event::ActionError(ActionError {
        invocation_id: invoke.invocation_id,
        action_id: invoke.action_id,
        message,
        details: None,
    })
}

fn dispatch_locked_tool_invoke(
    invoke: tau_proto::ToolStarted,
    shell_config: ShellConfig,
    tx: &mpsc::Sender<Frame>,
    running_shells: &Arc<Mutex<HashMap<tau_proto::ToolCallId, mpsc::Sender<()>>>>,
    lock_manager: &DirLockManager,
    sem: &Arc<Semaphore>,
) {
    let dirs = match crate::dir_lock::automatic_lock_dirs_for_tool(
        invoke.tool_name.as_str(),
        &invoke.arguments,
    ) {
        Ok(Some(dirs)) => crate::dir_lock::normalize_lock_dirs(dirs),
        Ok(None) => {
            let _permit = sem.acquire();
            dispatch_tool_invoke(invoke, shell_config, tx, running_shells);
            return;
        }
        Err(error) => {
            send_tool_failure(invoke, error, tx);
            return;
        }
    };

    let wait_invoke = invoke.clone();
    let wait_dirs = dirs.clone();
    let wait_tx = tx.clone();
    let guard = match lock_manager.acquire_auto(
        invoke.call_id.clone(),
        invoke.agent_id.clone(),
        dirs,
        move || {
            let _ = wait_tx.send(Frame::Event(crate::dir_lock::waiting_progress_event(
                &wait_invoke,
                &wait_dirs,
            )));
        },
    ) {
        Ok(guard) => guard,
        Err(crate::dir_lock::LockAcquireError::Cancelled) => {
            let _ = tx.send(Frame::Event(Event::ToolCancelled(ToolCancelled {
                call_id: invoke.call_id,
                tool_name: invoke.tool_name,
                tool_type: tau_proto::ToolType::Function,
            })));
            return;
        }
        Err(crate::dir_lock::LockAcquireError::Abandoned(lock)) => {
            send_tool_failure(invoke, lock.tool_failure(), tx);
            return;
        }
    };

    let _permit = sem.acquire();
    dispatch_tool_invoke(invoke, shell_config, tx, running_shells);
    drop(guard);
}

fn send_tool_failure(
    invoke: tau_proto::ToolStarted,
    failure: crate::display::ToolFailure,
    tx: &mpsc::Sender<Frame>,
) {
    let crate::display::ToolFailure {
        message,
        details,
        display,
    } = failure;
    let _ = tx.send(Frame::Event(Event::ToolError(tau_proto::ToolError {
        call_id: invoke.call_id,
        tool_name: invoke.tool_name,
        tool_type: tau_proto::ToolType::Function,
        message,
        details: details.map(|details| *details),
        display: Some(*display),
        originator: invoke.originator,
    })));
}

/// Execute a single tool invocation and send the response event(s).
fn dispatch_tool_invoke(
    invoke: tau_proto::ToolStarted,
    shell_config: ShellConfig,
    tx: &mpsc::Sender<Frame>,
    running_shells: &Arc<Mutex<HashMap<tau_proto::ToolCallId, mpsc::Sender<()>>>>,
) {
    if invoke.tool_name == SHELL_TOOL_NAME || invoke.tool_name == GPT_SHELL_TOOL_NAME {
        dispatch_cancellable_shell_tool(invoke, shell_config, tx, running_shells);
        return;
    }

    let events = execute_tool(invoke, &shell_config);
    for event in events {
        let _ = tx.send(Frame::Event(event));
    }
}

fn dispatch_cancellable_shell_tool(
    invoke: tau_proto::ToolStarted,
    shell_config: ShellConfig,
    tx: &mpsc::Sender<Frame>,
    running_shells: &Arc<Mutex<HashMap<tau_proto::ToolCallId, mpsc::Sender<()>>>>,
) {
    let (cancel_tx, cancel_rx) = mpsc::channel();
    debug!(
        call_id = %invoke.call_id,
        tool_name = %invoke.tool_name,
        "registering cancellable shell call"
    );
    running_shells
        .lock()
        .expect("running shell registry lock poisoned")
        .insert(invoke.call_id.clone(), cancel_tx);

    let _ = tx.send(Frame::Event(Event::ToolProgress(tau_proto::ToolProgress {
        call_id: invoke.call_id.clone(),
        tool_name: invoke.tool_name.clone(),
        message: Some("running shell command".to_owned()),
        progress: None,
        display: None,
    })));

    let event = match crate::tools::shell::run_command_cancellable(
        &invoke.arguments,
        &shell_config,
        Some(cancel_rx),
    ) {
        Ok(crate::tools::shell::CommandOutcome::Finished(output)) => {
            debug!(call_id = %invoke.call_id, tool_name = %invoke.tool_name, "cancellable shell call finished");
            Event::ToolResult(ToolResult {
                call_id: invoke.call_id.clone(),
                tool_name: invoke.tool_name.clone(),
                tool_type: tau_proto::ToolType::Function,
                result: output.result,
                kind: ToolResultKind::Final,
                display: Some(output.display),
                originator: invoke.originator.clone(),
            })
        }
        Ok(crate::tools::shell::CommandOutcome::Cancelled) => {
            debug!(call_id = %invoke.call_id, tool_name = %invoke.tool_name, "cancellable shell call cancelled");
            Event::ToolCancelled(ToolCancelled {
                call_id: invoke.call_id.clone(),
                tool_name: invoke.tool_name.clone(),
                tool_type: tau_proto::ToolType::Function,
            })
        }
        Err(crate::display::ToolFailure {
            message,
            details,
            display,
        }) => {
            debug!(
                call_id = %invoke.call_id,
                tool_name = %invoke.tool_name,
                message,
                "cancellable shell call failed"
            );
            Event::ToolError(tau_proto::ToolError {
                call_id: invoke.call_id.clone(),
                tool_name: invoke.tool_name.clone(),
                tool_type: tau_proto::ToolType::Function,
                message,
                details: details.map(|details| *details),
                display: Some(*display),
                originator: invoke.originator.clone(),
            })
        }
    };

    running_shells
        .lock()
        .expect("running shell registry lock poisoned")
        .remove(&invoke.call_id);
    trace!(call_id = %invoke.call_id, "removed shell call from cancellation registry");
    if tx.send(Frame::Event(event)).is_err() {
        debug!(call_id = %invoke.call_id, "failed to send terminal shell event to harness");
    }
}

fn dispatch_session_started(started: SessionStarted, tx: &mpsc::Sender<Frame>) {
    for event in build_session_started_events(started) {
        let _ = tx.send(Frame::Event(event));
    }
}

fn dispatch_session_agent_loaded(loaded: SessionAgentLoaded, tx: &mpsc::Sender<Frame>) {
    if let Ok(cwd) = std::env::current_dir() {
        let _ = tx.send(Frame::Event(Event::ExtAgentContextPublish(
            ExtAgentContextPublish {
                agent_id: loaded.agent_id.clone(),
                key: AgentContextKey::new("cwd"),
                value: AgentContextValue(serde_json::Value::String(cwd.display().to_string())),
            },
        )));
    }
    let _ = tx.send(Frame::Event(Event::ExtensionContextReady(
        ExtensionContextReady {
            session_id: loaded.session_id,
            agent_id: loaded.agent_id,
        },
    )));
}

fn ack_if_logged(
    id: Option<EventLogSeq>,
    tx: &mpsc::Sender<Frame>,
) -> Result<(), Box<mpsc::SendError<Frame>>> {
    if let Some(id) = id {
        tx.send(Frame::Message(Message::Ack(Ack { up_to: id })))
            .map_err(Box::new)?;
    }
    Ok(())
}

fn ack_log_event(id: EventLogSeq, tx: &mpsc::Sender<Frame>) {
    let _ = tx.send(Frame::Message(Message::Ack(Ack { up_to: id })));
}

fn is_shell_tool(name: &str) -> bool {
    matches!(
        name,
        READ_TOOL_NAME
            | EDIT_TOOL_NAME
            | APPLY_PATCH_TOOL_NAME
            | GREP_TOOL_NAME
            | FIND_TOOL_NAME
            | LS_TOOL_NAME
            | SHELL_TOOL_NAME
            | GPT_SHELL_TOOL_NAME
            | DIR_LOCK_TOOL_NAME
    ) || is_echo_tool(name)
}

fn is_dir_lock_update_tool(name: &str) -> bool {
    matches!(
        name,
        EDIT_TOOL_NAME | APPLY_PATCH_TOOL_NAME | SHELL_TOOL_NAME | GPT_SHELL_TOOL_NAME
    )
}

#[cfg(any(test, feature = "echo-agent"))]
fn is_echo_tool(name: &str) -> bool {
    name == ECHO_TOOL_NAME
}

#[cfg(not(any(test, feature = "echo-agent")))]
fn is_echo_tool(_name: &str) -> bool {
    false
}

fn build_session_started_events(_started: SessionStarted) -> Vec<Event> {
    let mut events = Vec::new();

    let skill_dirs = session_skill_dirs(std::env::current_dir().ok(), dirs::home_dir());

    let result = tau_skills::load_skills_from_skill_dirs(&skill_dirs);
    push_skill_diagnostic_events(&mut events, result.diagnostics);
    for skill in result.skills {
        let file_path = skill.file_path.canonicalize().unwrap_or(skill.file_path);
        events.push(Event::ExtSkillAvailable(tau_proto::ExtSkillAvailable {
            name: skill.name.into(),
            description: skill.description,
            file_path,
            add_to_prompt: skill.add_to_prompt,
        }));
    }

    for agents_file in discover_session_agents_files() {
        events.push(Event::ExtAgentsMdAvailable(
            tau_proto::ExtAgentsMdAvailable {
                file_path: agents_file.file_path,
                content: agents_file.content,
            },
        ));
    }

    events
}

fn shell_cwd_prompt_fragment() -> PromptFragment {
    PromptFragment::new(
        "shell.cwd",
        PromptPriority::new(900),
        PromptContent::new(
            "{{#each agent_context.cwd}}{{#if @first}}Current working directory: \
             {{value}}{{/if}}{{/each}}",
        ),
    )
}

fn push_skill_diagnostic_events(
    events: &mut Vec<Event>,
    diagnostics: Vec<tau_skills::SkillDiagnostic>,
) {
    for diagnostic in diagnostics {
        let (kind, level) = match diagnostic.kind {
            tau_skills::DiagnosticKind::Warning => ("warning", tau_proto::HarnessInfoLevel::Normal),
            tau_skills::DiagnosticKind::Collision => {
                ("collision", tau_proto::HarnessInfoLevel::Important)
            }
            tau_skills::DiagnosticKind::Skipped => {
                ("skipped", tau_proto::HarnessInfoLevel::Important)
            }
        };
        events.push(Event::HarnessInfo(tau_proto::HarnessInfo {
            message: format!(
                "skill {kind}: {}\n{}",
                diagnostic.path.display(),
                diagnostic.message
            ),
            level,
        }));
    }
}

fn session_skill_dirs(
    cwd: Option<std::path::PathBuf>,
    home: Option<std::path::PathBuf>,
) -> Vec<tau_skills::SkillDir> {
    let mut skill_dirs = Vec::new();
    if let Some(cwd) = cwd.as_deref() {
        for project_dir in project_skill_ancestor_dirs(cwd, home.as_deref()) {
            push_existing_project_skill_dir(
                &mut skill_dirs,
                project_dir.join(".agents").join("skills"),
            );
            push_existing_project_skill_dir(
                &mut skill_dirs,
                project_dir.join(".agents.local").join("skills"),
            );
        }
    }
    if let Some(home) = home {
        skill_dirs.push(user_skill_dir(home.join(".agents").join("skills")));
        skill_dirs.push(user_skill_dir(home.join(".agents.local").join("skills")));
        skill_dirs.push(user_skill_dir(
            home.join(".config").join("agents").join("skills"),
        ));
        skill_dirs.push(user_skill_dir(
            home.join(".config").join("agents.local").join("skills"),
        ));
    }
    skill_dirs
}

fn project_skill_ancestor_dirs(
    cwd: &std::path::Path,
    home: Option<&std::path::Path>,
) -> Vec<std::path::PathBuf> {
    ancestor_dirs(cwd)
        .into_iter()
        .filter(|dir| dir.parent().is_some())
        .filter(|dir| {
            let Some(home) = home else {
                return true;
            };
            !cwd.starts_with(home) || (dir.starts_with(home) && dir != home)
        })
        .collect()
}

fn push_existing_project_skill_dir(
    skill_dirs: &mut Vec<tau_skills::SkillDir>,
    path: std::path::PathBuf,
) {
    if path.is_dir() {
        skill_dirs.push(project_skill_dir(path));
    }
}

fn project_skill_dir(path: std::path::PathBuf) -> tau_skills::SkillDir {
    tau_skills::SkillDir {
        path,
        add_to_prompt_by_default: true,
    }
}

fn user_skill_dir(path: std::path::PathBuf) -> tau_skills::SkillDir {
    tau_skills::SkillDir {
        path,
        add_to_prompt_by_default: false,
    }
}
