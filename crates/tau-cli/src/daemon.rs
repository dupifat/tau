//! Harness daemon lifecycle: discovery, spawning, and the
//! parent↔child readiness handshake.

use std::fs::OpenOptions;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Instant;

use tau_cli_picker::{PickerError, PickerItem, pick};
use tau_harness::{SessionLaunchStatus, runtime_dir};

use crate::{CliError, mint_short_id};

const RESUME_PICKER_LIMIT: usize = 10;

/// How this CLI invocation is related to its harness daemon.
///
/// - `Owned`: we spawned the daemon; Drop kills it unless the UI detached
///   (calls [`DaemonHandle::leak`]), in which case we forget the `Child` so the
///   daemon outlives us.
/// - `Attached`: we joined a daemon someone else owns. Drop never touches it.
pub(crate) enum DaemonHandle {
    /// `child` is `Some` until [`leak`] pulls it out.
    Owned {
        child: Option<std::process::Child>,
        daemon_dir: PathBuf,
    },
    Attached {
        daemon_dir: PathBuf,
    },
}

impl DaemonHandle {
    pub(crate) fn socket_path(&self) -> PathBuf {
        runtime_dir::socket_path(self.daemon_dir())
    }

    fn daemon_dir(&self) -> &Path {
        match self {
            Self::Owned { daemon_dir, .. } | Self::Attached { daemon_dir } => daemon_dir,
        }
    }

    /// Consume the handle without killing the child.
    ///
    /// Used by `/detach`: we want the daemon to outlive this CLI,
    /// whether we spawned it or attached to it. For `Owned` this
    /// `mem::forget`s the `Child` — on Linux its parent becomes init
    /// on our exit, which is exactly what we want for a long-lived
    /// daemon.
    pub(crate) fn leak(mut self) {
        if let Self::Owned { child, .. } = &mut self
            && let Some(child) = child.take()
        {
            std::mem::forget(child);
        }
    }
}

impl Drop for DaemonHandle {
    fn drop(&mut self) {
        if let Self::Owned {
            child: Some(child), ..
        } = self
        {
            let _ = child.kill();
            let _ = child.wait();
        }
        // Attached, or Owned-after-leak: do nothing. The daemon keeps
        // running so other UIs can still use it, or this same UI can
        // `tau -a` back in later.
    }
}

/// Resolves the session id for one `tau` invocation.
///
/// - `None` → mint `<basename(cwd)>-<rand6>`.
/// - `Some("")` (bare `-r`) → interactively pick among recent sessions whose
///   `meta.json.cwd` matches cwd; if none, mint fresh.
/// - `Some(id)` → resume that explicit id; error if it does not exist.
pub(crate) fn resolve_run_session_id(
    resume: Option<&str>,
) -> Result<(String, SessionLaunchStatus), CliError> {
    let cwd = std::env::current_dir()?;
    match resume {
        None => Ok((mint_session_id(&cwd), SessionLaunchStatus::New)),
        Some("") => match pick_resume_session(&cwd)? {
            Some(id) => Ok((id, SessionLaunchStatus::Resumed)),
            None => Ok((mint_session_id(&cwd), SessionLaunchStatus::New)),
        },
        Some(id) => {
            if session_exists(id)? {
                Ok((id.to_owned(), SessionLaunchStatus::Resumed))
            } else {
                Err(CliError::SessionNotFound(id.to_owned()))
            }
        }
    }
}

fn session_exists(id: &str) -> Result<bool, CliError> {
    let sessions_dir = tau_session_inspect::default_sessions_dir();
    let metas = tau_harness::list_session_metas(&sessions_dir)?;
    Ok(metas
        .into_iter()
        .any(|(session_id, _)| session_id.as_str() == id))
}

pub(crate) fn mint_session_id(cwd: &Path) -> String {
    let basename = cwd
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("session");
    mint_short_id(basename)
}

fn pick_resume_session(_cwd: &Path) -> Result<Option<String>, CliError> {
    let sessions_dir = tau_session_inspect::default_sessions_dir();
    let mut metas = tau_harness::list_session_metas(&sessions_dir)?;
    metas.sort_by_key(|(_, meta)| std::cmp::Reverse(meta.last_touched));
    metas.truncate(RESUME_PICKER_LIMIT);
    if metas.is_empty() {
        return Ok(None);
    }
    if metas.len() == 1 || !io::IsTerminal::is_terminal(&io::stdin()) {
        return Ok(metas.first().map(|(sid, _)| sid.as_str().to_owned()));
    }

    let rows = metas
        .into_iter()
        .map(|(sid, _meta)| {
            let locked = tau_harness::session_is_locked(&sessions_dir, sid.as_str())
                .unwrap_or_else(|error| {
                    tracing::warn!(
                        target: "tau_cli::startup",
                        session_id = sid.as_str(),
                        %error,
                        "could not determine session lock state — assuming unlocked"
                    );
                    false
                });
            let id = sid.as_str().to_owned();
            let item = sid.as_str().to_owned();
            (id, item, locked)
        })
        .collect::<Vec<_>>();
    if rows.iter().all(|(_, _, locked)| *locked) {
        return Ok(None);
    }
    let default = rows
        .iter()
        .position(|(_, _, locked)| !*locked)
        .unwrap_or_default();
    if rows.iter().filter(|(_, _, locked)| !*locked).count() == 1 {
        return Ok(Some(rows[default].0.clone()));
    }
    let items = rows
        .iter()
        .map(|(_, item, locked)| {
            if *locked {
                PickerItem::disabled(item)
            } else {
                PickerItem::enabled(item)
            }
        })
        .collect::<Vec<_>>();
    let selection = match pick("Resume session", &items) {
        Ok(selection) => selection,
        Err(PickerError::Cancelled) => return Ok(None),
        Err(e) => return Err(CliError::Participant(e.to_string())),
    };
    Ok(Some(rows[selection].0.clone()))
}

pub(crate) struct DaemonOutput {
    pub(crate) stdout: Stdio,
    pub(crate) stderr: Stdio,
    pub(crate) log_path: PathBuf,
    pub(crate) start_offset: u64,
}

pub(crate) fn daemon_output_for_session(session_id: &str) -> Result<DaemonOutput, CliError> {
    // Route the daemon's stdout+stderr (where its tracing subscriber
    // writes) into the per-session harness log so it sits next to
    // per-extension logs under `<session>/logs/`. The CLI's own tracing
    // still goes to `ui.log`; the two streams are intentionally separated
    // so a session post-mortem doesn't need to pull from two places.
    let sessions_dir = tau_session_inspect::default_sessions_dir();
    let harness_log = tau_harness::harness_log_path(&sessions_dir, session_id);
    if let Some(parent) = harness_log.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let start_offset = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&harness_log)?
        .metadata()?
        .len();
    let stdout = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&harness_log)
        .map(Stdio::from)?;
    let stderr = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&harness_log)
        .map(Stdio::from)?;
    Ok(DaemonOutput {
        stdout,
        stderr,
        log_path: harness_log,
        start_offset,
    })
}

pub(crate) fn resolve_daemon(
    attach: bool,
    session_id: &str,
    session_status: SessionLaunchStatus,
    daemon_output: Option<DaemonOutput>,
    startup_role: Option<&str>,
    role_cli_overrides: &[tau_config::settings::RoleCliOverride],
    extension_cli_overrides: &[tau_config::settings::ExtensionCliOverride],
) -> Result<DaemonHandle, CliError> {
    tracing::debug!(target: "tau_cli::startup", attach, session_id, "resolving harness daemon");
    let project_root = std::env::current_dir()?;
    if attach {
        tracing::debug!(target: "tau_cli::startup", project_root = %project_root.display(), "looking for existing harness daemon");
        let daemon_dir =
            runtime_dir::find_harness_for_dir(&project_root).ok_or(CliError::NoRunningDaemon)?;
        tracing::debug!(target: "tau_cli::startup", daemon_dir = %daemon_dir.display(), "attached harness daemon resolved");
        return Ok(DaemonHandle::Attached { daemon_dir });
    }
    start_daemon(
        session_id,
        session_status,
        daemon_output.expect("daemon output for spawned harness"),
        startup_role,
        role_cli_overrides,
        extension_cli_overrides,
    )
}

fn read_daemon_output_since(path: &Path, start_offset: u64) -> io::Result<String> {
    let mut file = OpenOptions::new().read(true).open(path)?;
    file.seek(SeekFrom::Start(start_offset))?;
    let mut output = String::new();
    file.read_to_string(&mut output)?;
    Ok(output)
}

/// Spawns a new harness daemon and waits for its socket to be ready.
///
/// Synchronization is via a pipe wired to the child process's stdin rather
/// than a polling loop: the read end is retained by the parent and the write
/// end is passed to the child. The harness writes one byte and closes its end
/// once the socket is bound and the runtime markers are in place (see
/// [`runtime_dir::signal_ready_to_parent`]);
/// the parent blocks on `read_exact` until that byte arrives. EOF
/// without a byte means the child exited early — we reap it and
/// surface its captured output.
fn start_daemon(
    session_id: &str,
    session_status: SessionLaunchStatus,
    output: DaemonOutput,
    startup_role: Option<&str>,
    role_cli_overrides: &[tau_config::settings::RoleCliOverride],
    extension_cli_overrides: &[tau_config::settings::ExtensionCliOverride],
) -> Result<DaemonHandle, CliError> {
    let tau_binary = std::env::current_exe()?;
    tracing::debug!(target: "tau_cli::startup", tau_binary = %tau_binary.display(), session_id, "spawning harness daemon");

    let (mut read_pipe, write_pipe) = io::pipe()?;

    let spawn_result = build_daemon_command(DaemonCommandSpec {
        tau_binary: &tau_binary,
        session_id,
        session_status,
        stdout: output.stdout,
        stderr: output.stderr,
        stdin: Stdio::from(write_pipe),
        startup_role,
        role_cli_overrides,
        extension_cli_overrides,
    })
    .spawn();

    let mut child = spawn_result?;

    tracing::debug!(target: "tau_cli::startup", pid = child.id(), "harness daemon spawned");
    let daemon_dir = runtime_dir::root_runtime_dir().join(child.id().to_string());
    let started_at = Instant::now();

    let mut byte = [0u8; 1];
    match read_pipe.read_exact(&mut byte) {
        Ok(()) => {
            tracing::debug!(target: "tau_cli::startup", pid = child.id(), daemon_dir = %daemon_dir.display(), elapsed_ms = started_at.elapsed().as_millis(), "harness daemon signaled ready");
            Ok(DaemonHandle::Owned {
                child: Some(child),
                daemon_dir,
            })
        }
        Err(_eof_or_err) => {
            // Read end closed without a byte. The child exited before
            // signaling ready. Reap it so we can surface the captured stderr.
            let status = child.wait()?;
            tracing::debug!(target: "tau_cli::startup", pid = child.id(), %status, elapsed_ms = started_at.elapsed().as_millis(), "harness daemon exited before signaling ready");
            let captured = read_daemon_output_since(&output.log_path, output.start_offset)?;
            let mut message = format!("exit status: {status}");
            if !captured.trim().is_empty() {
                message.push_str("\n\nHarness output:\n");
                message.push_str(captured.trim_end());
            }
            Err(CliError::DaemonExited(message))
        }
    }
}

struct DaemonCommandSpec<'a> {
    tau_binary: &'a Path,
    session_id: &'a str,
    session_status: SessionLaunchStatus,
    stdout: Stdio,
    stderr: Stdio,
    stdin: Stdio,
    startup_role: Option<&'a str>,
    role_cli_overrides: &'a [tau_config::settings::RoleCliOverride],
    extension_cli_overrides: &'a [tau_config::settings::ExtensionCliOverride],
}

/// Build the `tau ext harness` command with the readiness-pipe write end wired
/// to the child's stdin.
fn build_daemon_command(spec: DaemonCommandSpec<'_>) -> Command {
    let mut cmd = Command::new(spec.tau_binary);
    cmd.arg("ext")
        .arg("harness")
        .env("TAU_SESSION_ID", spec.session_id)
        .env("TAU_SESSION_STATUS", spec.session_status.as_str())
        // TAU_VERSION/TAU_BUILD/TAU_LAST_MODIFIED used to be forwarded
        // here; the harness child now reads its own `built` snapshot
        // (see `tau_harness::version::export_to_env`) and publishes
        // them to its own environment instead.
        .env(tau_harness::runtime_dir::READY_FD_ENV, "0")
        // Default-enable info logging in the child process so `tau`
        // captures harness logs without requiring an env var. Users
        // can still override/filter with `TAU_LOG`.
        .env(
            "TAU_LOG",
            std::env::var("TAU_LOG").unwrap_or_else(|_| {
                "tau_harness=info,tau_cli=info,provider-builtin=info".to_owned()
            }),
        )
        .stdin(spec.stdin)
        .stdout(spec.stdout)
        .stderr(spec.stderr);

    if let Some(role) = spec.startup_role.filter(|role| !role.is_empty()) {
        cmd.env(tau_harness::STARTUP_ROLE_ENV, role);
    }
    if !spec.role_cli_overrides.is_empty() {
        cmd.env(
            tau_harness::ROLE_CLI_OVERRIDES_ENV,
            serde_json::to_string(spec.role_cli_overrides).expect("role overrides serialize"),
        );
    }
    if !spec.extension_cli_overrides.is_empty() {
        cmd.env(
            tau_harness::EXTENSION_CLI_OVERRIDES_ENV,
            serde_json::to_string(spec.extension_cli_overrides)
                .expect("extension overrides serialize"),
        );
    }

    cmd
}
