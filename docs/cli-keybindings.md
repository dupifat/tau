# CLI keybindings

Keep this document in sync with `crates/tau-config/config/built-in.cli-bindings.yaml`, prompt-local action handling in `crates/tau-cli-term/src/lib.rs`, and application action handling in `crates/tau-cli/src/chat.rs`.


## Built-in bindings

| Key | Action | Description |
| --- | --- | --- |
| `Enter`, `C-Enter` | `submit-prompt` | Submit the prompt, or accept a previewed completion without submitting. |
| `C-c` | `clear-or-cancel-prompt` | Clear a non-empty prompt, or request prompt cancellation on a second press when empty. |
| `C-d` | `prompt-eof` | Signal EOF when the prompt is empty. |
| `C-a`, `Home` | `cursor-start` | Move to the beginning of the prompt. |
| `C-e`, `End` | `cursor-end` | Move to the end of the prompt. |
| `C-u` | `kill-to-start` | Kill from cursor to the beginning of the prompt. |
| `C-w` | `kill-word-left` | Kill the word before the cursor. |
| `Backspace` | `delete-backward` | Delete the character before the cursor. |
| `Delete` | `delete-forward` | Delete the character after the cursor. |
| `Left`, `Right` | `cursor-left`, `cursor-right` | Move by one character. |
| `Up`, `Down` | `cursor-up`, `cursor-down` | Move within multiline input, completion candidates, or prompt history. |
| `Esc` | `escape` | Dismiss the completion menu if open, otherwise surface Escape. |
| `C-f` | `shell-prompt-insert` | Pick a file with `fzf` and insert it at the cursor. |
| `C-k` | `agent-previous` | Switch to the previous active agent. |
| `C-j` | `agent-next` | Switch to the next active agent. |
| `C-r` | `prompt-history-search` | Search past prompts with `fzf`, preview the highlighted prompt, and replace the current prompt with the selected prompt. |
| `C-t` | `shell-prompt-insert` | Search files with ripgrep through `fzf` and insert the selected path. |
| `Tab` | `cycle-role` | Cycle roles within the current role group. |
| `BackTab` / `Shift-Tab` | `cycle-role-group` | Cycle to the first role in the next role group. |
| `C-p`, `C-Up` | `prompt-previous` | Move to the previous prompt/history entry. |
| `C-n`, `C-Down` | `prompt-next` | Move to the next prompt/history entry. |
| `C-z` | `prompt-undo` | Undo the last edit in the current prompt/history entry. |
| `C-y` | `shell-prompt-insert` | Pick a jj change or git commit with `fzf` and insert its id at the cursor. |
| `C-o`, `C-g` | `shell-prompt-edit` | Edit the current prompt in `$TAU_EDITOR`. |


## Built-in editing keys

These keys are handled by named actions in the default binding file, with raw fallback behavior when no configurable binding matches. The built-in `Enter` binding makes plain Enter submit by default; bind `Enter` to `insert-newline` to restore the raw editing fallback.

| Key | Behavior |
| --- | --- |
| `Enter` | Insert a newline when not bound; submits by default via the built-in binding. |
| `C-Enter` | Submit the prompt. |
| `Shift-Enter`, `Alt-Enter` | Insert a newline. |
| `C-d` on an empty prompt | Exit Tau when no agent/session work is in progress; otherwise print a notice to use `/quit` and keep the session running. |
| `C-c` on an empty prompt | Print `Use Ctrl+D to exit`; does not exit. |
| `C-c` on a non-empty prompt | Clear the prompt; undoable with `prompt-undo`. |
| `C-a` / `Home` | Move to the beginning of the prompt. |
| `C-e` / `End` | Move to the end of the prompt. |
| `C-u` | Kill from cursor to the beginning of the prompt. |
| `C-w` | Kill the word before the cursor. |
| `Backspace`, `Delete` | Delete text around the cursor. |
| Arrow keys | Move within multiline input, completion candidates, or prompt history. |
| `Shift-Tab` | Cycle completion candidates backward when a completion menu is open; this takes precedence over configured `BackTab` bindings. Otherwise this is configurable as `BackTab`. |
| `Esc` | Dismiss the completion menu. |


## Configurable actions

Bindings live under `cli.bind` in config. The built-in bindings are merged below user bindings, so configuring one key does not remove the rest.

- `submit-prompt` — submit the current prompt, or accept a previewed completion without submitting.
- `insert-newline` — insert a newline at the cursor.
- `prompt-eof` — signal EOF when the prompt is empty.
- `clear-prompt` — clear a non-empty prompt.
- `clear-or-cancel-prompt` — clear a non-empty prompt, or arm/trigger cancellation on an empty prompt.
- `cursor-start` / `cursor-end` — move to the beginning or end of the prompt.
- `cursor-left` / `cursor-right` — move one character left or right.
- `cursor-up` / `cursor-down` — cycle completion candidates, move vertically in multiline input, or step prompt history.
- `move-up` / `move-down` — move vertically inside multiline input only.
- `delete-backward` / `delete-forward` — delete around the cursor.
- `kill-to-start` — kill from cursor to the beginning of the prompt.
- `kill-word-left` — kill the word before the cursor.
- `select-completion-next` / `select-completion-previous` — cycle completion candidates when the menu is open.
- `accept-completion` — accept the previewed completion candidate when available.
- `dismiss-completion` — dismiss the completion menu when open.
- `escape` / `backtab` — surface Escape or BackTab to the outer UI.
- `prompt-previous` — move backward in prompt history.
- `prompt-next` — move forward in prompt history.
- `prompt-undo` — undo an edit in the current prompt/history entry.
- `prompt-redo` — redo an undone edit in the current prompt/history entry.
- `fast-toggle` — toggle fast mode without editing the prompt draft.
- `cycle-role` — cycle roles within the current role group.
- `cycle-role-group` — cycle to the first role in the next role group.
- `agent-previous` — switch to the previous active agent.
- `agent-next` — switch to the next active agent.
- `prompt-history-search` — feed indexed prompt-history rows (`<index>\t<single-line summary>`) to `command`; original prompts are also written under `$TAU_PROMPT_HISTORY_DIR/<index>` for picker previews. Replace the prompt with the selected row's original prompt. The current draft is recorded for `prompt-undo` before the picker opens.
- `shell-prompt-insert` — run `command` and insert stdout at the cursor.
- `shell-prompt-edit` — run `command` with the current prompt in `$TAU_PROMPT_PATH` and replace the prompt with the edited file content.
