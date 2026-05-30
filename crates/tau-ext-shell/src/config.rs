//! Per-session configuration for the shell/file extension.

use std::collections::BTreeMap;
use std::process::Command;

use crate::isolation::{apply_command_isolation, apply_read_only_cwd_mount};

#[derive(Clone, Debug, Default, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct ExtConfig {
    pub(crate) shell: ShellConfig,
    pub(crate) dir_lock: DirLockConfig,
}

#[derive(Clone, Debug, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct DirLockConfig {
    /// Controls the agent-visible `dir_lock` tool and whether mutating
    /// ext-shell tools participate in directory update locking. Enabled by
    /// default; set to false to opt out.
    pub(crate) enable: bool,
}

impl Default for DirLockConfig {
    fn default() -> Self {
        Self { enable: true }
    }
}

#[derive(Clone, Debug, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct ShellConfig {
    /// Executable used for `shell` tool invocations and `!`/`!!` UI
    /// commands. It is invoked as `<command> -c <user command>`.
    command: String,
    /// argv prefix prepended before the shell command. The effective
    /// argv is `prefix ++ [command, "-c", user_command]`.
    prefix: Vec<String>,
    /// Maximum wall-clock seconds a user-initiated `!`/`!!` shell
    /// command may run before it is killed. Tool-side shell calls
    /// have their own per-call `timeout` argument; this one bounds
    /// the UI path where the agent isn't driving the timeout.
    pub(crate) user_command_timeout_secs: u64,
    /// Extra environment variables injected into shell-tool / `!`
    /// command children, applied after the inherited environment so
    /// they override or supplement it. Use this to set a custom
    /// `PAGER` or adjust paths. Keys with an empty value still clear
    /// the variable in the child env. Does not affect the `rg` child
    /// used by `grep`.
    extra_env: BTreeMap<String, String>,
}

impl Default for ShellConfig {
    fn default() -> Self {
        Self {
            command: "sh".to_owned(),
            prefix: Vec::new(),
            user_command_timeout_secs: 60 * 60,
            extra_env: BTreeMap::new(),
        }
    }
}

impl ShellConfig {
    fn command_for(&self, command: &str) -> Command {
        let mut argv = self.prefix.clone();
        argv.push(self.command.clone());
        let Some((program, args)) = argv.split_first() else {
            // `command` default is non-empty, and serde default prevents
            // this for missing config. An explicit empty string is still
            // a bad config; let spawn fail with a useful OS error.
            return Command::new("");
        };
        let mut child_cmd = Command::new(program);
        child_cmd.args(args).arg("-c").arg(command);
        child_cmd
    }

    /// Single spawn point for shell-style child processes: builds the
    /// configured shell invocation, attaches piped stdio, applies
    /// command isolation, and optionally sets a working directory.
    /// Used by both the agent-facing `shell` tool and the user-facing
    /// `!`/`!!` path so they can't silently diverge on isolation.
    pub(crate) fn spawn_isolated(
        &self,
        command: &str,
        cwd: Option<&str>,
        read_only_cwd: bool,
    ) -> std::io::Result<std::process::Child> {
        let mut child_cmd = self.command_for(command);
        child_cmd
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        if let Some(cwd) = cwd {
            child_cmd.current_dir(cwd);
        }
        apply_command_isolation(&mut child_cmd);
        let read_only_warning = if read_only_cwd {
            let mount_cwd = cwd.map_or_else(std::env::current_dir, |cwd| {
                let cwd = std::path::Path::new(cwd);
                if cwd.is_absolute() {
                    Ok(cwd.to_path_buf())
                } else {
                    std::env::current_dir().map(|current| current.join(cwd))
                }
            })?;
            apply_read_only_cwd_mount(&mut child_cmd, &mount_cwd)?
        } else {
            None
        };
        for (key, value) in &self.extra_env {
            child_cmd.env(key, value);
        }
        let child = child_cmd.spawn();
        if let Some(read_only_warning) = read_only_warning {
            read_only_warning.log_after_spawn();
        }
        child
    }
}
