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
use std::time::{Duration, Instant};

use tau_proto::{
    Ack, ActionError, ActionInvoke, ActionOutput, ActionResult, AgentContextKey, AgentContextValue,
    CborValue, ConfigError, Event, EventLogSeq, ExtAgentContextPublish, ExtPromptFragmentPublish,
    ExtensionContextReady, HarnessInputMessage, HarnessOutputMessage, PeerInputReader,
    PeerOutputWriter, PromptContent, PromptFragment, PromptPriority, SessionAgentLoaded,
    SessionStarted, ToolCancelled, ToolResult, ToolResultKind, ToolSpec,
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
const SLOW_LOCK_WAIT_THRESHOLD_SECS: u64 = 5;
const LOCK_WAIT_DURATION_SECONDS_HEADER: &str = "lock_wait_duration_seconds";

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
    let mut reader = PeerInputReader::new(BufReader::new(reader));
    let mut writer = PeerOutputWriter::new(BufWriter::new(writer));

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
                 Prefer one full read. Pass inclusive `start_line`/`end_line` only to \
                 fetch one specific known slice, or `ranges` for up to 100 slices; \
                 range chunks are separated by one empty line and may overlap. `start_line` past EOF errors, \
                 while `end_line` past EOF returns available lines. Returned content lines are prefixed \
                 by their 1-based line number and a space; \
                 CRLF, CR, and missing final line endings are marked after the number, e.g. \
                 `2(crlf)`, `3(cr)`, or `4(no_nl)`. Invalid UTF-8 is shown with \
                 Unicode replacement characters and an `invalid-utf8` line flag. Lines that would exceed \
                 the 50 KB output budget are marker-only, e.g. `1(truncated)`. Truncated results include `truncated: true`, `total_lines`, \
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
                        "minimum": 1,
                        "description": "Optional, 1-based inclusive. Omit to start at line 1 (the default)."
                    },
                    "end_line": {
                        "type": "integer",
                        "minimum": 1,
                        "description": "Optional, 1-based inclusive. Omit to read to end of file (the default and preferred mode). Set this only to continue past a previous truncation, or to fetch a known specific slice of a large file — do NOT pre-slice an ordinary file you haven't already established is large."
                    },
                    "ranges": {
                        "type": "array",
                        "description": "Optional list of inclusive line ranges to read. Cannot be combined with top-level start_line or end_line. Each chunk is separated by one empty line in the output, and overlapping ranges are returned redundantly.",
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
                                "end_line": {
                                    "type": "integer",
                                    "minimum": 1,
                                    "description": "1-based inclusive end line to read."
                                }
                            },
                            "required": ["start_line", "end_line"],
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
                "Edit a file using line-oriented replacements. Each edit fully replaces \
                 the range between `after_line` and `before_line` with `newText`. \
                 `after_line` is the line before the range, with 0 meaning the start of \
                 the file; `before_line` is the preserved line after the range, with \
                 total_lines + 1 meaning EOF. Empty insertion ranges use \
                 `before_line == after_line + 1`. All ranges use the original file \
                 numbering as if applied simultaneously. Non-empty replacements are kept \
                 as whole lines. Ranges must be non-overlapping. Missing files are \
                 treated as empty and missing parent directories are created. Per-edit \
                 `guard` must exactly match the first original line content for \
                 non-empty ranges; empty insertion ranges have no first line and must \
                 use an empty guard."
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
                                "after_line": {
                                    "type": "integer",
                                    "minimum": 0,
                                    "description": "Boundary line before the replacement range. Use 0 for the start of the file. The replaced range starts at after_line + 1. Use together with before_line."
                                },
                                "before_line": {
                                    "type": "integer",
                                    "minimum": 1,
                                    "description": "Boundary line after the replacement range. The line numbered before_line is preserved. Use total_lines + 1 for EOF append. Empty insertion ranges have before_line == after_line + 1. Use together with after_line."
                                },
                                "newText": {
                                    "type": "string",
                                    "description": "Replacement text. Non-empty replacements stay whole-line."
                                },
                                "guard": {
                                    "type": "string",
                                    "description": "For non-empty ranges, exact expected content of the first original line in the replaced range, including spaces and tabs. Empty insertion ranges have no first original line and must use an empty string. For non-empty ranges, an empty guard only matches an actually empty first line. If it does not match, the edit fails and returns correct current content of the guard line with context around it."
                                }
                            },
                            "required": ["after_line", "before_line", "newText", "guard"],
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
                "Use the `apply_patch` tool to edit files."
                    .to_owned(),
            ),
            tool_type: tau_proto::ToolType::Custom,
            parameters: None,
            format: Some(tau_proto::ToolFormat::Text),
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
                 for directories. Includes dotfiles. Output lines are prefixed with 1-based \
                 entry numbers plus flags such as `escaped`, `invalid-utf8`, or `truncated`; \
                 output is capped at `limit` entries, 2000 lines, or 50KB with standard truncation headers."
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
                        "minimum": 1,
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
                 are returned as structured command results with output details. Output is capped at 2000 lines / \
                 50 KB; truncated output keeps the first 1000 and last 1000 lines \
                 separated by a literal `...` line. Output lines are prefixed with `out ` \
                 for stdout or `err ` for stderr; missing trailing newlines are marked, e.g. \
                 `out(no_nl)`; CRLF and CR line endings are marked as `out(crlf)` \
                 or `out(cr)`. Invalid UTF-8 is shown with Unicode replacement characters and \
                 an `invalid-utf8` line flag. Lines that would exceed the 50 KB output budget \
                 are marker-only, e.g. `err(truncated)`. Truncated results include `truncated: true`, `total_lines`, and `total_bytes`. \
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
                "Run a shell command. Output is capped at 2000 lines / 50 KB; \
                 Output lines are prefixed with `out ` for stdout or `err ` for stderr; missing \
                 trailing newlines are marked with `(no_nl)`. For file changes, prefer apply_patch."
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
    let shell_tool_group = tau_proto::ToolGroup {
        name: tau_proto::ToolGroupName::new("shell"),
        prompt_fragment: None,
    };
    let test_tool_group = tau_proto::ToolGroup {
        name: tau_proto::ToolGroupName::new("test"),
        prompt_fragment: None,
    };
    for tool in tools {
        let tool_group = if tool.name.as_str() == "echo" {
            test_tool_group.clone()
        } else {
            shell_tool_group.clone()
        };
        handshake =
            handshake.register_tool_with_group_and_prompt_fragment(tool, Some(tool_group), None);
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

    // Response channel: worker threads send protocol messages here; writer
    // thread drains them onto the wire.
    let (tx, rx) = mpsc::channel::<HarnessInputMessage>();
    let sem = Arc::new(Semaphore::new(16));
    let running_shells = Arc::new(Mutex::new(
        HashMap::<tau_proto::ToolCallId, mpsc::Sender<()>>::new(),
    ));
    let lock_manager = DirLockManager::default();
    let mut start_agent_owners = HashMap::<String, tau_proto::AgentId>::new();

    // Writer thread: drains response messages and writes them to the wire.
    let writer_handle = std::thread::spawn(move || -> Result<(), Box<dyn Error + Send>> {
        for message in rx {
            writer
                .write_message(&message)
                .map_err(|e| -> Box<dyn Error + Send> { Box::new(e) })?;
            writer
                .flush()
                .map_err(|e| -> Box<dyn Error + Send> { Box::new(e) })?;
        }
        Ok(())
    });

    // Reader loop: dispatch each owned tool invocation to a worker thread.
    // ToolStarted is a subscribed committed delivery, so it carries an ack
    // sequence that must be acknowledged after processing like other subscribed
    // events.
    while let Some(message) = reader.read_message()? {
        match message {
            HarnessOutputMessage::Configure(msg) => {
                match tau_extension::parse_config::<ExtConfig>(&msg.config) {
                    Ok(mut cfg) => {
                        if cfg.working_directory.is_none() {
                            cfg.working_directory = config.working_directory.clone();
                        }
                        if let Err(message) = apply_working_directory(&config, &cfg) {
                            tx.send(HarnessInputMessage::ConfigError(ConfigError { message }))?;
                            continue;
                        }
                        let dir_lock_changed = config.dir_lock.enable != cfg.dir_lock.enable;
                        config = cfg;
                        if dir_lock_changed {
                            tx.send(HarnessInputMessage::emit(Event::ToolRegister(
                                tau_proto::ToolRegister {
                                    tool: dir_lock_tool_spec(config.dir_lock.enable),
                                    tool_group: Some(tau_proto::ToolGroup {
                                        name: tau_proto::ToolGroupName::new("shell"),
                                        prompt_fragment: None,
                                    }),
                                    prompt_fragment: None,
                                },
                            )))?;
                        }
                    }
                    Err(message) => {
                        tx.send(HarnessInputMessage::ConfigError(ConfigError { message }))?;
                    }
                }
            }
            HarnessOutputMessage::Deliver(delivery) => {
                let (event, log_id, _) = delivery.into_parts();
                match event {
                    Event::ToolStarted(invoke) => {
                        if !is_shell_tool(invoke.tool_name.as_str()) {
                            ack_if_logged(log_id, &tx)?;
                            continue;
                        }
                        let tx = tx.clone();
                        let shell_config = config.shell.clone();
                        let enforce_ro_mode = config.enforce_ro_mode;
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
                                    enforce_ro_mode,
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
                                dispatch_tool_invoke(
                                    invoke,
                                    shell_config,
                                    &tx,
                                    &running_shells,
                                    None,
                                    enforce_ro_mode,
                                );
                            });
                        }
                    }
                    Event::SessionStarted(started) => {
                        dispatch_session_started(started, &tx);
                    }
                    Event::SessionAgentLoaded(loaded) => {
                        dispatch_session_agent_loaded(loaded, &tx);
                    }
                    Event::SessionAgentUnloaded(unloaded) => {
                        lock_manager.release_agent(&unloaded.agent_id);
                        start_agent_owners.retain(|_, agent_id| agent_id != &unloaded.agent_id);
                    }
                    Event::SessionShutdown(_) => {
                        lock_manager.release_all_manual();
                        start_agent_owners.clear();
                    }
                    Event::StartAgentAccepted(accepted) => {
                        start_agent_owners.insert(accepted.query_id, accepted.agent_id);
                    }
                    Event::StartAgentResult(result) => {
                        if let Some(agent_id) = start_agent_owners.remove(&result.query_id) {
                            lock_manager.release_agent(&agent_id);
                        }
                    }
                    Event::ActionInvoke(invoke) => {
                        tx.send(HarnessInputMessage::emit(dispatch_action_invoke(
                            invoke,
                            &lock_manager,
                        )))?;
                    }
                    Event::ToolCancelRequest(request) => {
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
                    Event::UiShellCommand(cmd) => {
                        // User-initiated `!`/`!!` — run on a worker thread and
                        // stream chunks out via the same tx writer.
                        let permit = sem.acquire();
                        let tx = tx.clone();
                        let shell_config = config.shell.clone();
                        std::thread::spawn(move || {
                            let _permit = permit;
                            crate::tools::shell::dispatch_user_shell_command(
                                cmd,
                                shell_config,
                                &tx,
                            );
                        });
                    }
                    _ => {}
                }
                if let Some(id) = log_id {
                    ack_log_event(id, &tx);
                }
            }
            HarnessOutputMessage::Disconnect(_) => break,
            _ => {}
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

fn apply_working_directory(current: &ExtConfig, next: &ExtConfig) -> Result<(), String> {
    match (&current.working_directory, &next.working_directory) {
        (None, Some(working_directory)) => set_process_working_directory(working_directory),
        (Some(current), Some(next)) if current == next => Ok(()),
        (Some(current), Some(next)) => Err(format!(
            "ext-shell working_directory cannot be changed after startup (current: {}, requested: {})",
            current.display(),
            next.display()
        )),
        _ => Ok(()),
    }
}

fn set_process_working_directory(working_directory: &Path) -> Result<(), String> {
    std::env::set_current_dir(working_directory).map_err(|err| {
        format!(
            "failed to set ext-shell working_directory to {}: {err}",
            working_directory.display()
        )
    })
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
    tx: &mpsc::Sender<HarnessInputMessage>,
    running_shells: &Arc<Mutex<HashMap<tau_proto::ToolCallId, mpsc::Sender<()>>>>,
    lock_manager: &DirLockManager,
    sem: &Arc<Semaphore>,
    enforce_ro_mode: bool,
) {
    let dirs = match crate::dir_lock::automatic_lock_dirs_for_tool(
        invoke.tool_name.as_str(),
        &invoke.arguments,
    ) {
        Ok(Some(dirs)) => crate::dir_lock::normalize_lock_dirs(dirs),
        Ok(None) => {
            let _permit = sem.acquire();
            dispatch_tool_invoke(
                invoke,
                shell_config,
                tx,
                running_shells,
                None,
                enforce_ro_mode,
            );
            return;
        }
        Err(error) => {
            send_tool_failure(invoke, error, tx);
            return;
        }
    };

    let lock_wait_started = Instant::now();
    let wait_invoke = invoke.clone();
    let wait_dirs = dirs.clone();
    let wait_tx = tx.clone();
    let guard = match lock_manager.acquire_auto(
        invoke.call_id.clone(),
        invoke.agent_id.clone(),
        dirs,
        move || {
            let _ = wait_tx.send(HarnessInputMessage::emit(
                crate::dir_lock::waiting_progress_event(&wait_invoke, &wait_dirs),
            ));
        },
    ) {
        Ok(guard) => guard,
        Err(crate::dir_lock::LockAcquireError::Cancelled) => {
            let _ = tx.send(HarnessInputMessage::emit(Event::ToolCancelled(
                ToolCancelled {
                    call_id: invoke.call_id,
                    tool_name: invoke.tool_name,
                    tool_type: tau_proto::ToolType::Function,
                },
            )));
            return;
        }
        Err(crate::dir_lock::LockAcquireError::Abandoned(lock)) => {
            send_tool_failure(invoke, lock.tool_failure(), tx);
            return;
        }
    };

    let lock_wait_duration_seconds =
        reported_lock_wait_duration_seconds(lock_wait_started.elapsed());
    let _permit = sem.acquire();
    dispatch_tool_invoke(
        invoke,
        shell_config,
        tx,
        running_shells,
        lock_wait_duration_seconds,
        enforce_ro_mode,
    );
    drop(guard);
}

fn send_tool_failure(
    invoke: tau_proto::ToolStarted,
    failure: crate::display::ToolFailure,
    tx: &mpsc::Sender<HarnessInputMessage>,
) {
    let crate::display::ToolFailure {
        message,
        details,
        display,
    } = failure;
    let _ = tx.send(HarnessInputMessage::emit(Event::ToolError(
        tau_proto::ToolError {
            call_id: invoke.call_id,
            tool_name: invoke.tool_name,
            tool_type: tau_proto::ToolType::Function,
            message,
            details: details.map(|details| *details),
            display: Some(*display),
            originator: invoke.originator,
        },
    )));
}

fn reported_lock_wait_duration_seconds(elapsed: Duration) -> Option<u64> {
    if elapsed <= Duration::from_secs(SLOW_LOCK_WAIT_THRESHOLD_SECS) {
        return None;
    }

    let whole_seconds = elapsed.as_secs();
    if Duration::from_secs(whole_seconds) < elapsed {
        Some(whole_seconds.saturating_add(1))
    } else {
        Some(whole_seconds)
    }
}

fn with_lock_wait_duration(event: Event, lock_wait_duration_seconds: Option<u64>) -> Event {
    let Some(seconds) = lock_wait_duration_seconds else {
        return event;
    };

    match event {
        Event::ToolResult(mut result) => {
            result.result = cbor_value_with_lock_wait_duration(result.result, seconds, "output");
            Event::ToolResult(result)
        }
        Event::ToolError(mut error) => {
            error.details = Some(match error.details {
                Some(details) => cbor_value_with_lock_wait_duration(details, seconds, "details"),
                None => CborValue::Map(vec![lock_wait_duration_entry(seconds)]),
            });
            Event::ToolError(error)
        }
        event => event,
    }
}

fn cbor_value_with_lock_wait_duration(
    value: CborValue,
    seconds: u64,
    non_map_payload_key: &str,
) -> CborValue {
    match value {
        CborValue::Map(mut entries) => {
            prepend_lock_wait_duration(&mut entries, seconds);
            CborValue::Map(entries)
        }
        value => CborValue::Map(vec![
            lock_wait_duration_entry(seconds),
            (CborValue::Text(non_map_payload_key.to_owned()), value),
        ]),
    }
}

fn prepend_lock_wait_duration(entries: &mut Vec<(CborValue, CborValue)>, seconds: u64) {
    entries.retain(|(key, _)| match key {
        CborValue::Text(key) => key != LOCK_WAIT_DURATION_SECONDS_HEADER,
        _ => true,
    });
    entries.insert(0, lock_wait_duration_entry(seconds));
}

fn lock_wait_duration_entry(seconds: u64) -> (CborValue, CborValue) {
    let seconds = i64::try_from(seconds).unwrap_or(i64::MAX);
    (
        CborValue::Text(LOCK_WAIT_DURATION_SECONDS_HEADER.to_owned()),
        CborValue::Integer(seconds.into()),
    )
}

/// Execute a single tool invocation and send the response event(s).
fn dispatch_tool_invoke(
    invoke: tau_proto::ToolStarted,
    shell_config: ShellConfig,
    tx: &mpsc::Sender<HarnessInputMessage>,
    running_shells: &Arc<Mutex<HashMap<tau_proto::ToolCallId, mpsc::Sender<()>>>>,
    lock_wait_duration_seconds: Option<u64>,
    enforce_ro_mode: bool,
) {
    let vcr_config = tau_vcr::VcrConfig::from_env();
    let world = match crate::tools::world::ShellWorld::for_tool(
        invoke.tool_name.as_str(),
        invoke.call_id.as_str(),
        &invoke.arguments,
        vcr_config,
    ) {
        Ok(world) => world,
        Err(crate::display::ToolFailure {
            message,
            details,
            display,
        }) => {
            let event = Event::ToolError(tau_proto::ToolError {
                call_id: invoke.call_id.clone(),
                tool_name: invoke.tool_name.clone(),
                tool_type: tau_proto::ToolType::Function,
                message,
                details: details.map(|details| *details),
                display: Some(*display),
                originator: invoke.originator.clone(),
            });
            let event = with_lock_wait_duration(event, lock_wait_duration_seconds);
            let _ = tx.send(HarnessInputMessage::emit(event));
            return;
        }
    };

    if invoke.tool_name == SHELL_TOOL_NAME || invoke.tool_name == GPT_SHELL_TOOL_NAME {
        dispatch_cancellable_shell_tool(
            invoke,
            shell_config,
            tx,
            running_shells,
            lock_wait_duration_seconds,
            enforce_ro_mode,
            world,
        );
        return;
    }

    if let Some(display) = crate::tools::initial_display(&invoke) {
        let _ = tx.send(HarnessInputMessage::emit(Event::ToolProgress(
            tau_proto::ToolProgress {
                call_id: invoke.call_id.clone(),
                tool_name: invoke.tool_name.clone(),
                message: None,
                progress: None,
                display: Some(display),
            },
        )));
    }

    let events = execute_tool(invoke, world);
    for event in events {
        let event = with_lock_wait_duration(event, lock_wait_duration_seconds);
        let _ = tx.send(HarnessInputMessage::emit(event));
    }
}

fn dispatch_cancellable_shell_tool(
    invoke: tau_proto::ToolStarted,
    shell_config: ShellConfig,
    tx: &mpsc::Sender<HarnessInputMessage>,
    running_shells: &Arc<Mutex<HashMap<tau_proto::ToolCallId, mpsc::Sender<()>>>>,
    lock_wait_duration_seconds: Option<u64>,
    enforce_ro_mode: bool,
    mut world: crate::tools::world::ShellWorld,
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

    let _ = tx.send(HarnessInputMessage::emit(Event::ToolProgress(
        tau_proto::ToolProgress {
            call_id: invoke.call_id.clone(),
            tool_name: invoke.tool_name.clone(),
            message: None,
            progress: None,
            display: Some(crate::tools::shell::initial_display(&invoke.arguments)),
        },
    )));
    let result = crate::tools::shell::run_command_cancellable(
        invoke.call_id.as_str(),
        &invoke.arguments,
        &shell_config,
        enforce_ro_mode,
        Some(cancel_rx),
        &mut world,
    );
    let outcome = match (result, world.finish()) {
        (Ok(outcome), Ok(())) => Ok(outcome),
        (Ok(_), Err(failure)) | (Err(failure), Ok(())) | (Err(failure), Err(_)) => Err(failure),
    };
    let event = match outcome {
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
    let event = with_lock_wait_duration(event, lock_wait_duration_seconds);
    if tx.send(HarnessInputMessage::emit(event)).is_err() {
        debug!(call_id = %invoke.call_id, "failed to send terminal shell event to harness");
    }
}

fn dispatch_session_started(started: SessionStarted, tx: &mpsc::Sender<HarnessInputMessage>) {
    for event in build_session_started_events(started) {
        let _ = tx.send(HarnessInputMessage::emit(event));
    }
}

fn dispatch_session_agent_loaded(
    loaded: SessionAgentLoaded,
    tx: &mpsc::Sender<HarnessInputMessage>,
) {
    if let Ok(cwd) = std::env::current_dir() {
        let _ = tx.send(HarnessInputMessage::emit(Event::ExtAgentContextPublish(
            ExtAgentContextPublish {
                agent_id: loaded.agent_id.clone(),
                key: AgentContextKey::new("cwd"),
                value: AgentContextValue(serde_json::Value::String(cwd.display().to_string())),
            },
        )));
    }
    let _ = tx.send(HarnessInputMessage::emit(Event::ExtensionContextReady(
        ExtensionContextReady {
            session_id: loaded.session_id,
            agent_id: loaded.agent_id,
        },
    )));
}

fn ack_if_logged(
    id: Option<EventLogSeq>,
    tx: &mpsc::Sender<HarnessInputMessage>,
) -> Result<(), Box<mpsc::SendError<HarnessInputMessage>>> {
    if let Some(id) = id {
        tx.send(HarnessInputMessage::Ack(Ack { up_to: id }))
            .map_err(Box::new)?;
    }
    Ok(())
}

fn ack_log_event(id: EventLogSeq, tx: &mpsc::Sender<HarnessInputMessage>) {
    let _ = tx.send(HarnessInputMessage::Ack(Ack { up_to: id }));
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
