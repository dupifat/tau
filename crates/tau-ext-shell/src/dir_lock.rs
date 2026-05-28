//! Directory update lock manager for shell-owned mutating tools.
//!
//! The lock is advisory and lives inside `tau-ext-shell`: reads never wait,
//! while shell/file update tools coordinate on canonical absolute directory
//! paths. Manual `dir_lock update` calls reserve a subtree for their owning
//! agent, and automatic writer locks serialize concrete mutating operations.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, mpsc};

use tau_proto::{
    AgentId, CborValue, Event, Frame, ToolCallId, ToolCancelled, ToolDisplay, ToolDisplayPayload,
    ToolDisplayStatus, ToolError, ToolProgress, ToolResult, ToolResultKind, ToolStarted, ToolType,
};

use crate::argument::argument_text;
use crate::display::{ToolFailure, ok_display};
use crate::tools::{
    APPLY_PATCH_TOOL_NAME, EDIT_TOOL_NAME, GPT_SHELL_TOOL_NAME, SHELL_TOOL_NAME, WRITE_TOOL_NAME,
};

/// Agent-facing name of the directory locking tool.
pub(crate) const DIR_LOCK_TOOL_NAME: &str = "dir_lock";

/// Shared state used by all ext-shell workers that participate in directory
/// update locking.
#[derive(Clone, Debug, Default)]
pub(crate) struct DirLockManager {
    inner: Arc<DirLockInner>,
}

#[derive(Debug, Default)]
struct DirLockInner {
    state: Mutex<LockState>,
    changed: Condvar,
}

#[derive(Debug, Default)]
struct LockState {
    manual: Vec<ManualLock>,
    automatic: Vec<AutomaticLock>,
    waiters: VecDeque<Waiter>,
    next_waiter_id: u64,
    next_auto_id: u64,
}

#[derive(Clone, Debug)]
struct ManualLock {
    owner: AgentId,
    dir: PathBuf,
    count: usize,
}

#[derive(Clone, Debug)]
struct AutomaticLock {
    id: u64,
    dirs: Vec<PathBuf>,
}

#[derive(Clone, Debug)]
struct Waiter {
    id: u64,
    call_id: ToolCallId,
    owner: AgentId,
    dirs: Vec<PathBuf>,
    kind: WaitKind,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum WaitKind {
    Manual,
    Automatic,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum LockAcquireError {
    Cancelled,
}

/// RAII guard for an automatic writer lock. Dropping it releases the active
/// lock and wakes the next FIFO waiter.
#[derive(Debug)]
pub(crate) struct AutoDirLockGuard {
    manager: DirLockManager,
    id: u64,
}

impl Drop for AutoDirLockGuard {
    fn drop(&mut self) {
        self.manager.release_auto(self.id);
    }
}

impl DirLockManager {
    /// Acquire an automatic update lock for one mutating tool invocation.
    pub(crate) fn acquire_auto<F>(
        &self,
        call_id: ToolCallId,
        owner: AgentId,
        dirs: Vec<PathBuf>,
        on_wait: F,
    ) -> Result<AutoDirLockGuard, LockAcquireError>
    where
        F: FnOnce(),
    {
        let dirs = normalize_lock_dirs(dirs);
        let mut on_wait = Some(on_wait);
        let mut state = self.inner.state.lock().expect("dir lock state poisoned");
        if state.can_grant_now(&owner, &dirs, WaitKind::Automatic) {
            let id = state.add_auto(owner, dirs);
            return Ok(AutoDirLockGuard {
                manager: self.clone(),
                id,
            });
        }

        let waiter = state.push_waiter(call_id, owner, dirs, WaitKind::Automatic);
        drop(state);
        if let Some(on_wait) = on_wait.take() {
            on_wait();
        }
        let mut state = self.inner.state.lock().expect("dir lock state poisoned");
        loop {
            let Some(pos) = state.waiters.iter().position(|queued| queued.id == waiter) else {
                return Err(LockAcquireError::Cancelled);
            };
            if pos == 0 {
                let queued = state.waiters.front().expect("position says front exists");
                if !state.has_conflict(&queued.owner, &queued.dirs, queued.kind) {
                    let queued = state.waiters.pop_front().expect("front exists");
                    let id = state.add_auto(queued.owner, queued.dirs);
                    return Ok(AutoDirLockGuard {
                        manager: self.clone(),
                        id,
                    });
                }
            }
            state = self
                .inner
                .changed
                .wait(state)
                .expect("dir lock state poisoned");
        }
    }

    /// Acquire and retain a manual lock owned by `owner`.
    pub(crate) fn acquire_manual<F>(
        &self,
        call_id: ToolCallId,
        owner: AgentId,
        dir: PathBuf,
        on_wait: F,
    ) -> Result<(), LockAcquireError>
    where
        F: FnOnce(),
    {
        let dirs = vec![dir];
        let mut on_wait = Some(on_wait);
        let mut state = self.inner.state.lock().expect("dir lock state poisoned");
        if state.can_grant_now(&owner, &dirs, WaitKind::Manual) {
            state.add_manual(owner, dirs);
            self.inner.changed.notify_all();
            return Ok(());
        }

        let waiter = state.push_waiter(call_id, owner, dirs, WaitKind::Manual);
        drop(state);
        if let Some(on_wait) = on_wait.take() {
            on_wait();
        }
        let mut state = self.inner.state.lock().expect("dir lock state poisoned");
        loop {
            let Some(pos) = state.waiters.iter().position(|queued| queued.id == waiter) else {
                return Err(LockAcquireError::Cancelled);
            };
            if pos == 0 {
                let queued = state.waiters.front().expect("position says front exists");
                if !state.has_conflict(&queued.owner, &queued.dirs, queued.kind) {
                    let queued = state.waiters.pop_front().expect("front exists");
                    state.add_manual(queued.owner, queued.dirs);
                    self.inner.changed.notify_all();
                    return Ok(());
                }
            }
            state = self
                .inner
                .changed
                .wait(state)
                .expect("dir lock state poisoned");
        }
    }

    /// Release one exact manual lock held by `owner` for `dir`.
    pub(crate) fn unlock_manual(&self, owner: &AgentId, dir: &Path) -> Result<usize, String> {
        let mut state = self.inner.state.lock().expect("dir lock state poisoned");
        let Some(pos) = state
            .manual
            .iter()
            .position(|lock| &lock.owner == owner && lock.dir == dir)
        else {
            return Err(format!(
                "agent `{owner}` does not hold a directory lock for {}",
                dir.display()
            ));
        };
        let remaining = if 1 < state.manual[pos].count {
            state.manual[pos].count -= 1;
            state.manual[pos].count
        } else {
            state.manual.remove(pos);
            0
        };
        self.inner.changed.notify_all();
        Ok(remaining)
    }

    /// Cancel a queued lock waiter for `call_id`, if one exists.
    pub(crate) fn cancel_waiting_call(&self, call_id: &ToolCallId) -> bool {
        let mut state = self.inner.state.lock().expect("dir lock state poisoned");
        let before = state.waiters.len();
        state.waiters.retain(|waiter| &waiter.call_id != call_id);
        let removed = state.waiters.len() != before;
        if removed {
            self.inner.changed.notify_all();
        }
        removed
    }

    /// Release all manual locks owned by an unloaded agent.
    pub(crate) fn release_agent(&self, owner: &AgentId) -> usize {
        let mut state = self.inner.state.lock().expect("dir lock state poisoned");
        let before = state.manual.len();
        state.manual.retain(|lock| &lock.owner != owner);
        let removed = before - state.manual.len();
        if 0 < removed {
            self.inner.changed.notify_all();
        }
        removed
    }

    /// Drop all manual locks, used when the whole session is shutting down.
    pub(crate) fn release_all_manual(&self) -> usize {
        let mut state = self.inner.state.lock().expect("dir lock state poisoned");
        let removed = state.manual.len();
        state.manual.clear();
        if 0 < removed {
            self.inner.changed.notify_all();
        }
        removed
    }

    fn release_auto(&self, id: u64) {
        let mut state = self.inner.state.lock().expect("dir lock state poisoned");
        state.automatic.retain(|lock| lock.id != id);
        self.inner.changed.notify_all();
    }
}

impl LockState {
    fn push_waiter(
        &mut self,
        call_id: ToolCallId,
        owner: AgentId,
        dirs: Vec<PathBuf>,
        kind: WaitKind,
    ) -> u64 {
        let id = self.next_waiter_id;
        self.next_waiter_id += 1;
        self.waiters.push_back(Waiter {
            id,
            call_id,
            owner,
            dirs,
            kind,
        });
        id
    }

    fn can_grant_now(&self, owner: &AgentId, dirs: &[PathBuf], kind: WaitKind) -> bool {
        let bypass_queue = self.can_bypass_queue(owner, dirs, kind);
        (bypass_queue || self.waiters.is_empty()) && !self.has_conflict(owner, dirs, kind)
    }

    fn can_bypass_queue(&self, owner: &AgentId, dirs: &[PathBuf], kind: WaitKind) -> bool {
        match kind {
            WaitKind::Manual => self.manual.iter().any(|lock| {
                &lock.owner == owner && dirs.iter().any(|dir| paths_overlap(&lock.dir, dir))
            }),
            WaitKind::Automatic => dirs.iter().all(|dir| {
                self.manual
                    .iter()
                    .any(|lock| &lock.owner == owner && dir.starts_with(&lock.dir))
            }),
        }
    }

    fn has_conflict(&self, owner: &AgentId, dirs: &[PathBuf], kind: WaitKind) -> bool {
        if self.automatic.iter().any(|lock| {
            lock.dirs
                .iter()
                .any(|active| dirs.iter().any(|dir| paths_overlap(active, dir)))
        }) {
            return true;
        }

        self.manual.iter().any(|lock| {
            if &lock.owner == owner {
                return false;
            }
            match kind {
                WaitKind::Manual | WaitKind::Automatic => {
                    dirs.iter().any(|dir| paths_overlap(&lock.dir, dir))
                }
            }
        })
    }

    fn add_manual(&mut self, owner: AgentId, dirs: Vec<PathBuf>) {
        for dir in dirs {
            if let Some(lock) = self
                .manual
                .iter_mut()
                .find(|lock| lock.owner == owner && lock.dir == dir)
            {
                lock.count += 1;
            } else {
                self.manual.push(ManualLock {
                    owner: owner.clone(),
                    dir,
                    count: 1,
                });
            }
        }
    }

    fn add_auto(&mut self, _owner: AgentId, dirs: Vec<PathBuf>) -> u64 {
        let id = self.next_auto_id;
        self.next_auto_id += 1;
        self.automatic.push(AutomaticLock { id, dirs });
        id
    }
}

/// Handle the agent-visible `dir_lock` tool and stream any waiting progress
/// before the lock is granted.
pub(crate) fn dispatch_dir_lock_tool(
    invoke: ToolStarted,
    manager: &DirLockManager,
    enabled: bool,
    tx: &mpsc::Sender<Frame>,
) {
    if !enabled {
        send_event(
            tx,
            tool_error(
                &invoke,
                "dir_lock is disabled; set ext-shell config `dir_lock.enable` to true to use it"
                    .to_owned(),
                None,
            ),
        );
        return;
    }
    if invoke.agent_id.is_empty() {
        send_event(
            tx,
            tool_error(
                &invoke,
                "dir_lock requires a non-empty tool owner agent_id".to_owned(),
                None,
            ),
        );
        return;
    }

    let command = match argument_text(&invoke.arguments, "command") {
        Ok(command) => command,
        Err(message) => {
            send_event(
                tx,
                tool_error(&invoke, message, Some(invoke.arguments.clone())),
            );
            return;
        }
    };
    let dir_arg = match argument_text(&invoke.arguments, "directory") {
        Ok(directory) => directory,
        Err(message) => {
            send_event(
                tx,
                tool_error(&invoke, message, Some(invoke.arguments.clone())),
            );
            return;
        }
    };
    let dir = match canonical_existing_dir(Path::new(&dir_arg)) {
        Ok(dir) => dir,
        Err(message) => {
            send_event(
                tx,
                tool_error(&invoke, message, Some(invoke.arguments.clone())),
            );
            return;
        }
    };

    match command.as_str() {
        "update" => {
            let wait_invoke = invoke.clone();
            let wait_dir = dir.clone();
            let wait_tx = tx.clone();
            match manager.acquire_manual(
                invoke.call_id.clone(),
                invoke.agent_id.clone(),
                dir.clone(),
                move || send_event(&wait_tx, waiting_progress_event(&wait_invoke, &[wait_dir])),
            ) {
                Ok(()) => send_event(
                    tx,
                    tool_result(
                        &invoke,
                        dir_lock_result_value("update", &dir, Some(true), None),
                        locked_display("locked", &dir),
                    ),
                ),
                Err(LockAcquireError::Cancelled) => {
                    send_event(tx, cancelled_event(invoke));
                }
            }
        }
        "unlock" => match manager.unlock_manual(&invoke.agent_id, &dir) {
            Ok(remaining) => send_event(
                tx,
                tool_result(
                    &invoke,
                    dir_lock_result_value("unlock", &dir, Some(false), Some(remaining)),
                    locked_display("unlocked", &dir),
                ),
            ),
            Err(message) => send_event(
                tx,
                tool_error(&invoke, message, Some(invoke.arguments.clone())),
            ),
        },
        _ => send_event(
            tx,
            tool_error(
                &invoke,
                "argument `command` must be `update` or `unlock`".to_owned(),
                Some(invoke.arguments.clone()),
            ),
        ),
    }
}

/// Return the canonical update-lock directories for a mutating ext-shell tool.
pub(crate) fn automatic_lock_dirs_for_tool(
    tool_name: &str,
    arguments: &CborValue,
) -> Result<Option<Vec<PathBuf>>, ToolFailure> {
    match tool_name {
        WRITE_TOOL_NAME => {
            let path = argument_text(arguments, "path").map_err(ToolFailure::from)?;
            Ok(Some(vec![canonical_write_lock_dir(Path::new(&path))?]))
        }
        EDIT_TOOL_NAME => {
            let path = argument_text(arguments, "path").map_err(ToolFailure::from)?;
            Ok(Some(vec![canonical_existing_file_parent(Path::new(
                &path,
            ))?]))
        }
        SHELL_TOOL_NAME | GPT_SHELL_TOOL_NAME => Ok(Some(vec![canonical_shell_cwd(arguments)?])),
        APPLY_PATCH_TOOL_NAME => Ok(Some(crate::tools::apply_patch::lock_directories(
            arguments,
        )?)),
        _ => Ok(None),
    }
}

/// Build a progress event that replaces the live tool block while waiting for
/// a directory update lock.
pub(crate) fn waiting_progress_event(invoke: &ToolStarted, dirs: &[PathBuf]) -> Event {
    let dirs_display = display_dirs(dirs);
    Event::ToolProgress(ToolProgress {
        call_id: invoke.call_id.clone(),
        tool_name: invoke.tool_name.clone(),
        message: Some(format!("waiting for directory lock: {dirs_display}")),
        progress: None,
        display: Some(ToolDisplay {
            args: dirs_display,
            info_chips: vec!["dir lock".to_owned()],
            status: ToolDisplayStatus::InProgress,
            status_text: "waiting".to_owned(),
            ..Default::default()
        }),
    })
}

/// Canonicalize `path` as an existing directory.
pub(crate) fn canonical_existing_dir(path: &Path) -> Result<PathBuf, String> {
    let canonical = path
        .canonicalize()
        .map_err(|error| format!("directory {} does not exist: {error}", path.display()))?;
    let metadata = std::fs::metadata(&canonical)
        .map_err(|error| format!("failed to stat directory {}: {error}", canonical.display()))?;
    if !metadata.is_dir() {
        return Err(format!("{} is not a directory", canonical.display()));
    }
    Ok(canonical)
}

/// Return a stable human-readable lock directory list.
pub(crate) fn display_dirs(dirs: &[PathBuf]) -> String {
    dirs.iter()
        .map(|dir| dir.display().to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

/// Canonical write-target lock directory, following a final symlink when the
/// destination path is already a symlink. Missing parents lock the deepest
/// existing ancestor so `write` can keep creating parent directories safely.
pub(crate) fn canonical_write_lock_dir(path: &Path) -> Result<PathBuf, ToolFailure> {
    let lock_path = match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            let target = std::fs::read_link(path).map_err(|error| {
                ToolFailure::from(format!(
                    "failed to read symlink {}: {error}",
                    path.display()
                ))
                .with_args(path.display().to_string())
            })?;
            let resolved = if target.is_absolute() {
                target
            } else {
                path.parent().unwrap_or_else(|| Path::new(".")).join(target)
            };
            absolute_path(&resolved).map_err(|error| {
                ToolFailure::from(format!("failed to resolve {}: {error}", resolved.display()))
                    .with_args(path.display().to_string())
            })?
        }
        _ => absolute_path(path).map_err(|error| {
            ToolFailure::from(format!("failed to resolve {}: {error}", path.display()))
                .with_args(path.display().to_string())
        })?,
    };
    let parent = lock_path.parent().ok_or_else(|| {
        ToolFailure::from(format!(
            "path {} has no parent directory",
            lock_path.display()
        ))
        .with_args(path.display().to_string())
    })?;
    canonical_deepest_existing_ancestor(parent)
        .map_err(|message| ToolFailure::from(message).with_args(path.display().to_string()))
}

/// Canonical parent directory for an existing file, following symlinks to the
/// actual file that will be modified by `edit`.
pub(crate) fn canonical_existing_file_parent(path: &Path) -> Result<PathBuf, ToolFailure> {
    let canonical = path.canonicalize().map_err(|error| {
        ToolFailure::from(format!("file {} does not exist: {error}", path.display()))
            .with_args(path.display().to_string())
    })?;
    let metadata = std::fs::metadata(&canonical).map_err(|error| {
        ToolFailure::from(format!(
            "failed to stat file {}: {error}",
            canonical.display()
        ))
        .with_args(path.display().to_string())
    })?;
    if metadata.is_dir() {
        return Err(ToolFailure::from(format!(
            "{} is a directory, not a file",
            canonical.display()
        ))
        .with_args(path.display().to_string()));
    }
    canonical.parent().map(Path::to_path_buf).ok_or_else(|| {
        ToolFailure::from(format!(
            "file {} has no parent directory",
            canonical.display()
        ))
        .with_args(path.display().to_string())
    })
}

/// Canonical lock directory for an apply_patch in-place update.
///
/// Existing final symlinks are followed because `fs::write` updates their
/// target. Missing files and directories lock the canonical requested parent so
/// apply_patch can preserve its normal partial-failure behavior.
pub(crate) fn canonical_update_lock_dir(path: &Path) -> Result<PathBuf, ToolFailure> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => canonical_existing_file_parent(path),
        Ok(_) => canonical_path_parent(path),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => canonical_path_parent(path),
        Err(error) => Err(ToolFailure::from(format!(
            "failed to stat file {}: {error}",
            path.display()
        ))
        .with_args(path.display().to_string())),
    }
}

/// Canonical parent for a path whose final component may be removed or
/// replaced without following a final symlink.
pub(crate) fn canonical_path_parent(path: &Path) -> Result<PathBuf, ToolFailure> {
    let abs = absolute_path(path).map_err(|error| {
        ToolFailure::from(format!("failed to resolve {}: {error}", path.display()))
            .with_args(path.display().to_string())
    })?;
    let parent = abs.parent().ok_or_else(|| {
        ToolFailure::from(format!("path {} has no parent directory", abs.display()))
            .with_args(path.display().to_string())
    })?;
    canonical_existing_dir(parent)
        .map_err(|message| ToolFailure::from(message).with_args(path.display().to_string()))
}

/// Canonical lock directory for a shell command's cwd argument.
pub(crate) fn canonical_shell_cwd(arguments: &CborValue) -> Result<PathBuf, ToolFailure> {
    let cwd = crate::argument::optional_argument_text(arguments, "cwd");
    let display_arg = cwd.clone().unwrap_or_else(|| ".".to_owned());
    let path = cwd
        .as_deref()
        .map(Path::new)
        .unwrap_or_else(|| Path::new("."));
    canonical_existing_dir(path)
        .map_err(|message| ToolFailure::from(message).with_args(display_arg))
}

/// Convert a possibly relative path to an absolute path without requiring the
/// final component to exist.
fn absolute_path(path: &Path) -> std::io::Result<PathBuf> {
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        std::env::current_dir().map(|cwd| cwd.join(path))
    }
}

fn canonical_deepest_existing_ancestor(path: &Path) -> Result<PathBuf, String> {
    let mut candidate = path.to_path_buf();
    loop {
        match canonical_existing_dir(&candidate) {
            Ok(dir) => return Ok(dir),
            Err(_) => {
                if !candidate.pop() {
                    return Err(format!(
                        "no existing ancestor directory for {}",
                        path.display()
                    ));
                }
            }
        }
    }
}

pub(crate) fn normalize_lock_dirs(mut dirs: Vec<PathBuf>) -> Vec<PathBuf> {
    dirs.sort_by(|a, b| {
        a.components()
            .count()
            .cmp(&b.components().count())
            .then_with(|| a.cmp(b))
    });
    dirs.dedup();
    let mut normalized: Vec<PathBuf> = Vec::new();
    'next: for dir in dirs {
        for existing in &normalized {
            if dir.starts_with(existing) {
                continue 'next;
            }
        }
        normalized.push(dir);
    }
    normalized
}

fn paths_overlap(a: &Path, b: &Path) -> bool {
    a.starts_with(b) || b.starts_with(a)
}

fn dir_lock_result_value(
    command: &str,
    dir: &Path,
    locked: Option<bool>,
    remaining: Option<usize>,
) -> CborValue {
    let mut entries = vec![
        (
            CborValue::Text("command".to_owned()),
            CborValue::Text(command.to_owned()),
        ),
        (
            CborValue::Text("directory".to_owned()),
            CborValue::Text(dir.display().to_string()),
        ),
    ];
    if let Some(locked) = locked {
        entries.push((
            CborValue::Text("locked".to_owned()),
            CborValue::Bool(locked),
        ));
    }
    if let Some(remaining) = remaining {
        entries.push((
            CborValue::Text("remaining".to_owned()),
            CborValue::Integer((remaining as i64).into()),
        ));
    }
    CborValue::Map(entries)
}

fn locked_display(status_text: &str, dir: &Path) -> ToolDisplay {
    let mut display = ok_display(dir.display().to_string());
    display.status_text = status_text.to_owned();
    display.payload = Some(ToolDisplayPayload::Text {
        text: dir.display().to_string(),
    });
    display
}

fn tool_result(invoke: &ToolStarted, result: CborValue, display: ToolDisplay) -> Event {
    Event::ToolResult(ToolResult {
        call_id: invoke.call_id.clone(),
        tool_name: invoke.tool_name.clone(),
        tool_type: ToolType::Function,
        result,
        kind: ToolResultKind::Final,
        display: Some(display),
        originator: invoke.originator.clone(),
    })
}

fn tool_error(invoke: &ToolStarted, message: String, details: Option<CborValue>) -> Event {
    Event::ToolError(ToolError {
        call_id: invoke.call_id.clone(),
        tool_name: invoke.tool_name.clone(),
        tool_type: ToolType::Function,
        message,
        details,
        display: Some(ToolDisplay {
            status: ToolDisplayStatus::Error,
            status_text: "dir_lock failed".to_owned(),
            ..Default::default()
        }),
        originator: invoke.originator.clone(),
    })
}

fn cancelled_event(invoke: ToolStarted) -> Event {
    Event::ToolCancelled(ToolCancelled {
        call_id: invoke.call_id,
        tool_name: invoke.tool_name,
        tool_type: ToolType::Function,
    })
}

fn send_event(tx: &mpsc::Sender<Frame>, event: Event) {
    let _ = tx.send(Frame::Event(event));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn path(value: &str) -> PathBuf {
        PathBuf::from(value)
    }

    #[test]
    fn path_conflicts_include_ancestors_and_children() {
        assert!(paths_overlap(Path::new("/tmp/a"), Path::new("/tmp/a")));
        assert!(paths_overlap(Path::new("/tmp/a"), Path::new("/tmp/a/b")));
        assert!(paths_overlap(Path::new("/tmp/a/b"), Path::new("/tmp/a")));
        assert!(!paths_overlap(Path::new("/tmp/a"), Path::new("/tmp/b")));
    }

    #[test]
    fn fifo_front_waiter_blocks_later_independent_request() {
        let manager = DirLockManager::default();
        manager
            .acquire_manual("manual-a".into(), "agent-a".into(), path("/repo/a"), || {})
            .expect("manual lock");

        let first = std::thread::spawn({
            let manager = manager.clone();
            move || {
                manager.acquire_manual("manual-root".into(), "agent-b".into(), path("/repo"), || {})
            }
        });
        wait_until(|| manager.inner.state.lock().expect("state").waiters.len() == 1);

        let second = std::thread::spawn({
            let manager = manager.clone();
            move || {
                manager.acquire_auto(
                    "auto-b".into(),
                    "agent-c".into(),
                    vec![path("/other")],
                    || {},
                )
            }
        });
        wait_until(|| manager.inner.state.lock().expect("state").waiters.len() == 2);
        assert_eq!(
            manager.inner.state.lock().expect("state").automatic.len(),
            0,
            "later independent auto lock must not jump a blocked front waiter"
        );

        manager
            .unlock_manual(&"agent-a".into(), Path::new("/repo/a"))
            .expect("unlock");
        first.join().expect("first").expect("first acquired");
        manager
            .unlock_manual(&"agent-b".into(), Path::new("/repo"))
            .expect("unlock root");
        let guard = second.join().expect("second").expect("second acquired");
        drop(guard);
    }

    fn wait_until(mut predicate: impl FnMut() -> bool) {
        let start = std::time::Instant::now();
        while !predicate() {
            assert!(start.elapsed() < std::time::Duration::from_secs(2));
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }
}
