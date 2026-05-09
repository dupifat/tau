//! Higher-level terminal prompt with slash-command completion.
//!
//! This crate is now a thin shell around [`tau_cli_term_raw`]: the
//! raw layer owns the input state machine (history navigation,
//! completion menu lifecycle, key dispatch). This crate plugs in the
//! *content* (which candidates exist for a given buffer) and the
//! *presentation* (how the menu is rendered as a styled block under
//! the prompt). It also handles `$EDITOR` integration, which doesn't
//! belong in the raw layer.

pub mod completion;
pub mod resolve;
#[cfg(test)]
mod tests;

use std::io;
use std::sync::Arc;

pub use completion::{CommandName, CompletionData, CompletionItem, SlashCommand};
#[cfg(test)]
pub(crate) use tau_cli_term_raw::RawEvent as TestRawEvent;
pub use tau_cli_term_raw::{
    Align, BlockId, Cell, Color, CursorShape, Span, Style, StyledBlock, StyledText, TermHandle,
};
use tau_cli_term_raw::{Candidate, Event as RawEvent};
use tau_themes::Theme;

/// High-level events surfaced to the caller.
pub enum Event {
    /// The user submitted a line (pressed Enter, no completion preview).
    Line(String),
    /// The user signalled EOF (Ctrl-D on empty line).
    Eof,
    /// The terminal was resized.
    Resize { width: u16, height: u16 },
    /// The input buffer changed (or the completion menu cycled,
    /// opened, or closed). Caller should redraw any prompt-derived
    /// UI.
    BufferChanged,
    /// Shift+Tab pressed outside an open completion menu — caller
    /// decides what to do with it (Pi-style: cycle effort).
    BackTab,
}

/// Higher-level terminal prompt with completion support.
pub struct HighTerm {
    term: tau_cli_term_raw::Term,
    handle: TermHandle,
    theme: Theme,
    /// Block id for the completion menu, allocated lazily on first
    /// open. Reused across opens; content swapped to empty when the
    /// menu is hidden.
    menu_block_id: Option<BlockId>,
}

impl HighTerm {
    /// Creates a new terminal with the given prompt and slash commands.
    ///
    /// Returns the terminal, a thread-safe handle for rendering, and a
    /// [`CompletionData`] handle for pushing dynamic argument completions
    /// from background threads.
    pub fn new(
        left_prompt: impl Into<StyledText>,
        commands: Vec<SlashCommand>,
        theme: Theme,
        cursor_shape: CursorShape,
        bindings: impl IntoIterator<Item = (String, String)>,
    ) -> io::Result<(Self, TermHandle, CompletionData)> {
        let (mut term, handle) = tau_cli_term_raw::Term::new(left_prompt, cursor_shape)?;
        term.set_bindings(bindings);
        let handle_clone = handle.clone();
        let data = CompletionData::new();
        let data_clone = data.clone();
        term.set_completion_source(Some(make_completion_source(commands, data)));
        Ok((
            Self {
                term,
                handle,
                theme,
                menu_block_id: None,
            },
            handle_clone,
            data_clone,
        ))
    }

    #[cfg(test)]
    pub(crate) fn new_for_test(
        mut term: tau_cli_term_raw::Term,
        handle: TermHandle,
        commands: Vec<SlashCommand>,
        theme: Theme,
        bindings: impl IntoIterator<Item = (String, String)>,
    ) -> (Self, CompletionData) {
        let data = CompletionData::new();
        let data_clone = data.clone();
        term.set_completion_source(Some(make_completion_source(commands, data)));
        term.set_bindings(bindings);
        (
            Self {
                term,
                handle,
                theme,
                menu_block_id: None,
            },
            data_clone,
        )
    }

    /// Returns a reference to the [`TermHandle`].
    pub fn handle(&self) -> &TermHandle {
        &self.handle
    }

    /// Triggers a redraw.
    pub fn redraw(&self) {
        self.handle.redraw();
    }

    /// Appends persistent output to history.
    pub fn print_output(&self, block: impl Into<StyledBlock>) -> BlockId {
        self.handle.print_output(block)
    }

    /// Blocks until the next high-level event, syncing the
    /// completion menu block to the raw term's current state.
    pub fn get_next_event(&mut self) -> io::Result<Event> {
        loop {
            let raw = self.term.get_next_event()?;

            match raw {
                RawEvent::BufferChanged => {
                    self.sync_menu_block();
                    self.handle.redraw();
                    return Ok(Event::BufferChanged);
                }

                RawEvent::CompletionAccept => {
                    // Accept-without-submit: the buffer already
                    // reflects the chosen candidate. Sync the menu
                    // (now closed) and loop so the user has to press
                    // Enter again to actually submit.
                    self.sync_menu_block();
                    self.handle.redraw();
                    continue;
                }

                RawEvent::BackTab => return Ok(Event::BackTab),

                RawEvent::Line(line) => {
                    self.sync_menu_block();
                    self.handle.redraw();
                    return Ok(Event::Line(line));
                }

                RawEvent::Eof => {
                    self.sync_menu_block();
                    return Ok(Event::Eof);
                }

                RawEvent::Resize { width, height } => {
                    self.sync_menu_block();
                    self.handle.redraw();
                    return Ok(Event::Resize { width, height });
                }

                RawEvent::ExternalEditor => {
                    self.sync_menu_block();
                    self.run_external_editor();
                    self.handle.redraw_sync();
                    return Ok(Event::BufferChanged);
                }

                RawEvent::Binding(action) => {
                    self.sync_menu_block();
                    self.run_binding(&action);
                    self.handle.redraw_sync();
                    return Ok(Event::BufferChanged);
                }
            }
        }
    }

    /// Updates the suggestion block to match the raw term's
    /// completion state: renders the menu when one is open, hides
    /// the block otherwise.
    fn sync_menu_block(&mut self) {
        match self.term.completion_state() {
            Some(view) => {
                let block = completion::render_menu_block(&view, &self.theme);
                let id = match self.menu_block_id {
                    Some(id) => id,
                    None => {
                        let id = self.handle.new_block("");
                        self.handle.push_suggestions(id);
                        self.menu_block_id = Some(id);
                        id
                    }
                };
                self.handle.set_block(id, block);
            }
            None => {
                if let Some(id) = self.menu_block_id.take() {
                    self.handle.remove_suggestions(id);
                    self.handle.remove_block(id);
                }
            }
        }
    }

    /// Spawns `$VISUAL || $EDITOR` synchronously with the current
    /// input buffer in a tempfile, releases raw mode while it runs,
    /// and replaces the buffer with the result on success. Errors
    /// (no editor, spawn failure, non-zero exit) surface as a themed
    /// info line above the prompt.
    fn run_external_editor(&self) {
        match run_prompt_shell_action(
            &self.term,
            &self.handle,
            PromptShellAction::Edit(PromptShellCommand {
                command: "${VISUAL:-${EDITOR:-}} \"$TAU_PROMPT_PATH\"".to_owned(),
                trim: false,
            }),
        ) {
            Ok(Some(PromptShellResult::Replace(new_text))) => {
                let cursor = new_text.len();
                self.handle.set_buffer(new_text, cursor);
            }
            Ok(Some(PromptShellResult::Insert(_))) => {}
            Ok(None) => {} // editor exited non-zero or text unchanged.
            Err(msg) => self.print_local(&format!("external editor: {msg}")),
        }
    }

    fn run_binding(&self, action: &str) {
        tracing::trace!(target: "tau_cli::input", action, "running prompt binding");
        let Some(action) = PromptShellAction::parse(action) else {
            self.print_local(&format!("binding: unknown action `{action}`"));
            return;
        };
        match run_prompt_shell_action(&self.term, &self.handle, action) {
            Ok(Some(PromptShellResult::Replace(new_text))) => {
                let cursor = new_text.len();
                self.handle.set_buffer(new_text, cursor);
            }
            Ok(Some(PromptShellResult::Insert(text))) => {
                let mut buffer = self.handle.get_buffer();
                let cursor = self.handle.get_cursor();
                buffer.insert_str(cursor, &text);
                self.handle.set_buffer(buffer, cursor + text.len());
            }
            Ok(None) => {}
            Err(msg) => self.print_local(&format!("binding: {msg}")),
        }
    }

    fn print_local(&self, message: &str) {
        let block = resolve::themed_block(
            &self.theme,
            tau_themes::names::SYSTEM_INFO,
            message.to_owned(),
        );
        self.handle.print_output(block);
    }
}

fn make_completion_source(
    commands: Vec<SlashCommand>,
    data: CompletionData,
) -> Box<dyn tau_cli_term_raw::CompletionSource> {
    let commands = Arc::new(commands);
    Box::new(move |buffer: &str, cursor: usize| -> Vec<Candidate> {
        completion::build_candidates(&commands, &data, buffer, cursor)
    })
}

struct PromptShellCommand {
    command: String,
    trim: bool,
}

enum PromptShellAction {
    Insert(PromptShellCommand),
    Edit(PromptShellCommand),
}

enum PromptShellResult {
    Insert(String),
    Replace(String),
}

impl PromptShellAction {
    fn parse(action: &str) -> Option<Self> {
        let mut parts = action.splitn(3, ':');
        let name = parts.next()?;
        let mode = parts.next()?;
        let command = parts.next()?.to_owned();
        let command = PromptShellCommand {
            command,
            trim: mode == "trim",
        };
        match name {
            "shell-prompt-insert" => Some(Self::Insert(command)),
            "shell-prompt-edit" => Some(Self::Edit(command)),
            _ => None,
        }
    }
}

fn run_prompt_shell_action(
    term: &tau_cli_term_raw::Term,
    handle: &TermHandle,
    action: PromptShellAction,
) -> Result<Option<PromptShellResult>, String> {
    let current = handle.get_buffer();
    let cursor = handle.get_cursor();
    let tmp = tempfile::Builder::new()
        .prefix("tau-prompt-")
        .suffix(".tau.md")
        .tempfile()
        .map_err(|e| format!("could not create tempfile: {e}"))?;
    std::fs::write(tmp.path(), current.as_bytes())
        .map_err(|e| format!("could not write tempfile: {e}"))?;

    let shell = match &action {
        PromptShellAction::Insert(shell) | PromptShellAction::Edit(shell) => shell,
    };
    let command = shell.command.as_str();
    tracing::trace!(
        target: "tau_cli::input",
        command,
        prompt_path = %tmp.path().display(),
        cursor,
        "spawning prompt shell action"
    );
    if command.trim().is_empty() {
        return Err("empty shell command".to_owned());
    }

    term.pause_for_external()
        .map_err(|e| format!("could not release terminal: {e}"))?;
    // RAII so a spawn error / panic still restores raw mode.
    struct ResumeGuard<'a>(&'a tau_cli_term_raw::Term);
    impl Drop for ResumeGuard<'_> {
        fn drop(&mut self) {
            let _ = self.0.resume_after_external();
        }
    }
    let _guard = ResumeGuard(term);

    let output = std::process::Command::new("sh")
        .arg("-c")
        .arg(command)
        .env("TAU_PROMPT_PATH", tmp.path())
        .env("TAU_PROMPT_COLUMN", (cursor + 1).to_string())
        .env("TAU_PROMPT_ROW", "1")
        .output()
        .map_err(|e| format!("could not spawn shell: {e}"))?;
    if !output.status.success() {
        return Ok(None);
    }

    match action {
        PromptShellAction::Insert(_) => {
            let text = String::from_utf8(output.stdout)
                .map_err(|e| format!("command output was not utf-8: {e}"))?;
            let text = if shell.trim {
                text.trim().to_owned()
            } else {
                text
            };
            Ok(Some(PromptShellResult::Insert(text)))
        }
        PromptShellAction::Edit(_) => {
            let new_text = std::fs::read_to_string(tmp.path())
                .map_err(|e| format!("could not read tempfile: {e}"))?;
            let new_text = new_text.strip_suffix('\n').unwrap_or(&new_text).to_owned();
            Ok(Some(PromptShellResult::Replace(new_text)))
        }
    }
}
