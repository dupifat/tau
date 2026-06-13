//! Supervised child-process management and stdio transport adapters.
//!
//! The initial implementation focuses on one supervised child process connected
//! over stdin/stdout using the shared CBOR event protocol.

use std::io::{self, BufReader, BufWriter};
#[cfg(unix)]
use std::os::unix::process::ExitStatusExt;
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};
use std::{fmt, thread};

use tau_proto::{
    DecodeError, Event, ExtensionExited, ExtensionName, ExtensionReady, ExtensionStarting,
    HarnessInputMessage, HarnessInputReader, HarnessOutputMessage, HarnessOutputWriter,
};

const STDOUT_FRAME_BUFFER: usize = 64;

/// Child stderr handling policy.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum StderrPolicy {
    /// Inherit the supervisor process stderr handle.
    Inherit,
    /// Discard child stderr output.
    Null,
}

/// One configured supervised extension command.
///
/// Spawned children inherit the supervisor process environment except variables
/// whose names start with `TAU_SECRET_`; those are removed before launch.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExtensionCommand {
    /// Stable extension identity used in lifecycle events.
    pub name: ExtensionName,
    /// Executable path passed to `Command::new` when spawning the child.
    pub program: PathBuf,
    /// Command-line arguments passed to the child after `program`.
    pub args: Vec<String>,
    /// Optional working directory for the child process.
    pub working_dir: Option<PathBuf>,
    /// Policy for child stderr output.
    pub stderr: StderrPolicy,
}

impl ExtensionCommand {
    /// Returns the argv used to launch the child process.
    #[must_use]
    pub fn argv(&self) -> Vec<String> {
        let mut argv = Vec::with_capacity(1 + self.args.len());
        argv.push(self.program.display().to_string());
        argv.extend(self.args.iter().cloned());
        argv
    }

    /// Creates the lifecycle event emitted before the child starts.
    #[must_use]
    pub fn starting_event(
        &self,
        instance_id: tau_proto::ExtensionInstanceId,
        pid: Option<u32>,
    ) -> Event {
        Event::ExtensionStarting(ExtensionStarting {
            instance_id,
            extension_name: self.name.clone(),
            pid,
        })
    }
}

/// One detected child-process exit.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ChildExit {
    /// Numeric process exit code when the platform reports normal exit.
    pub exit_code: Option<i32>,
    /// Unix signal number when the process was terminated by signal.
    pub signal: Option<i32>,
}

impl ChildExit {
    fn from_status(status: std::process::ExitStatus) -> Self {
        Self {
            exit_code: status.code(),
            signal: exit_signal(status),
        }
    }
}

/// Outcome of a timed receive attempt from a supervised child's stdout.
#[derive(Clone, Debug, PartialEq)]
pub enum ReceiveOutcome {
    /// A complete extension-to-harness protocol message was decoded.
    Message(HarnessInputMessage),
    /// No stdout message arrived before the requested timeout elapsed.
    Timeout,
    /// The child closed stdout cleanly at a protocol message boundary.
    Closed,
}

/// Errors produced by the supervised stdio transport.
#[derive(Debug)]
pub enum SupervisionError {
    /// The child process could not be spawned.
    Spawn(io::Error),
    /// The spawned child did not provide a piped stdin handle.
    MissingStdin,
    /// The spawned child did not provide a piped stdout handle.
    MissingStdout,
    /// A harness-to-extension protocol message could not be encoded.
    Encode(tau_proto::EncodeError),
    /// The child stdin writer could not be flushed after sending a message.
    Flush(io::Error),
    /// The child stdout reader thread could not be started.
    ReaderThread(io::Error),
    /// The child stdout reader observed corrupt or truncated protocol data.
    Decode(DecodeError),
    /// The child process could not be killed during hard termination.
    Kill(io::Error),
    /// Waiting for child process status failed.
    Wait(io::Error),
    /// The child did not exit before the requested timeout elapsed.
    Timeout {
        /// Timeout duration requested by the caller.
        duration: Duration,
    },
}

impl fmt::Display for SupervisionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Spawn(source) => write!(f, "failed to spawn child process: {source}"),
            Self::MissingStdin => f.write_str("spawned child process did not expose stdin"),
            Self::MissingStdout => f.write_str("spawned child process did not expose stdout"),
            Self::Encode(source) => write!(f, "failed to encode event for child stdin: {source}"),
            Self::Flush(source) => write!(f, "failed to flush child stdin: {source}"),
            Self::ReaderThread(source) => {
                write!(f, "failed to start child stdout reader thread: {source}")
            }
            Self::Decode(source) => write!(f, "failed to decode event from child stdout: {source}"),
            Self::Kill(source) => write!(f, "failed to kill child process: {source}"),
            Self::Wait(source) => write!(f, "failed to wait for child process: {source}"),
            Self::Timeout { duration } => {
                write!(f, "timed out waiting for child exit after {duration:?}")
            }
        }
    }
}

impl std::error::Error for SupervisionError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Spawn(source) => Some(source),
            Self::MissingStdin => None,
            Self::MissingStdout => None,
            Self::Encode(source) => Some(source),
            Self::Flush(source) => Some(source),
            Self::ReaderThread(source) => Some(source),
            Self::Decode(source) => Some(source),
            Self::Kill(source) => Some(source),
            Self::Wait(source) => Some(source),
            Self::Timeout { .. } => None,
        }
    }
}

/// One supervised child process connected over stdin/stdout.
///
/// The child is owned by this value. Callers should prefer explicit graceful
/// protocol shutdown or `terminate`; `Drop` only hard-kills live children,
/// waits best-effort, and ignores cleanup errors.
/// Child stdout is handed through a bounded buffer, so callers supervising a
/// child that can emit during shutdown must continue calling `recv_timeout` or
/// otherwise drain stdout before waiting indefinitely for exit.
pub struct SupervisedChild {
    command: ExtensionCommand,
    child: Child,
    stdin: HarnessOutputWriter<BufWriter<ChildStdin>>,
    stdout_frames: Receiver<Result<StdoutFrame, DecodeError>>,
}
impl SupervisedChild {
    /// Spawns one supervised child process with piped stdin/stdout.
    pub fn spawn(command: ExtensionCommand) -> Result<Self, SupervisionError> {
        let mut child_command = Command::new(&command.program);
        child_command
            .args(&command.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(match command.stderr {
                StderrPolicy::Inherit => Stdio::inherit(),
                StderrPolicy::Null => Stdio::null(),
            });
        if let Some(working_dir) = &command.working_dir {
            child_command.current_dir(working_dir);
        }
        remove_secret_env(&mut child_command);

        let mut child = child_command.spawn().map_err(SupervisionError::Spawn)?;

        let stdin = child.stdin.take().ok_or(SupervisionError::MissingStdin)?;
        let stdout = child.stdout.take().ok_or(SupervisionError::MissingStdout)?;
        let stdout_frames = match spawn_stdout_reader(stdout) {
            Ok(receiver) => receiver,
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(error);
            }
        };

        Ok(Self {
            command,
            child,
            stdin: HarnessOutputWriter::new(BufWriter::new(stdin)),
            stdout_frames,
        })
    }

    /// Returns the extension command used to launch this child.
    #[must_use]
    pub fn command(&self) -> &ExtensionCommand {
        &self.command
    }

    /// Returns the child process ID.
    #[must_use]
    pub fn pid(&self) -> u32 {
        self.child.id()
    }

    /// Creates the lifecycle event emitted when the child becomes connected.
    #[must_use]
    pub fn ready_event(
        &self,
        instance_id: tau_proto::ExtensionInstanceId,
        pid: Option<u32>,
    ) -> Event {
        Event::ExtensionReady(ExtensionReady {
            instance_id,
            extension_name: self.command.name.clone(),
            pid,
        })
    }

    /// Sends one harness → extension protocol message to the child over stdin.
    pub fn send(&mut self, message: &HarnessOutputMessage) -> Result<(), SupervisionError> {
        self.stdin
            .write_message(message)
            .map_err(SupervisionError::Encode)?;
        self.stdin.flush().map_err(SupervisionError::Flush)
    }

    /// Reads one extension → harness protocol message from the child.
    ///
    /// Timeouts, clean stdout closure, and decoded messages are returned as
    /// distinct outcomes. Truncated or corrupt frames are reported as decode
    /// errors.
    pub fn recv_timeout(&mut self, timeout: Duration) -> Result<ReceiveOutcome, SupervisionError> {
        match self.stdout_frames.recv_timeout(timeout) {
            Ok(Ok(StdoutFrame::Message(frame))) => Ok(ReceiveOutcome::Message(frame)),
            Ok(Ok(StdoutFrame::Closed)) => Ok(ReceiveOutcome::Closed),
            Ok(Err(error)) => Err(SupervisionError::Decode(error)),
            Err(RecvTimeoutError::Timeout) => Ok(ReceiveOutcome::Timeout),
            Err(RecvTimeoutError::Disconnected) => Ok(ReceiveOutcome::Closed),
        }
    }

    /// Checks whether the child has already exited.
    pub fn try_wait(&mut self) -> Result<Option<ChildExit>, SupervisionError> {
        self.child
            .try_wait()
            .map_err(SupervisionError::Wait)
            .map(|status| status.map(ChildExit::from_status))
    }

    /// Waits until the child exits or the timeout elapses.
    pub fn wait_for_exit(&mut self, timeout: Duration) -> Result<ChildExit, SupervisionError> {
        let started_at = Instant::now();
        loop {
            if let Some(exit) = self.try_wait()? {
                return Ok(exit);
            }
            if timeout <= started_at.elapsed() {
                return Err(SupervisionError::Timeout { duration: timeout });
            }
            thread::sleep(Duration::from_millis(10));
        }
    }

    /// Creates the lifecycle event emitted when the child exits.
    #[must_use]
    pub fn exited_event(
        &self,
        instance_id: tau_proto::ExtensionInstanceId,
        pid: Option<u32>,
        exit: &ChildExit,
    ) -> Event {
        Event::ExtensionExited(ExtensionExited {
            instance_id,
            extension_name: self.command.name.clone(),
            pid,
            exit_code: exit.exit_code,
            signal: exit.signal,
        })
    }

    /// Forcibly terminates the child process and waits for its exit.
    ///
    /// This is the explicit hard-shutdown API for callers that decide graceful
    /// protocol shutdown is no longer possible or no longer desired.
    pub fn terminate(&mut self, timeout: Duration) -> Result<ChildExit, SupervisionError> {
        if let Some(exit) = self.try_wait()? {
            return Ok(exit);
        }
        self.child.kill().map_err(SupervisionError::Kill)?;
        self.wait_for_exit(timeout)
    }
}

impl Drop for SupervisedChild {
    /// Performs last-resort cleanup for children that callers did not shut
    /// down.
    fn drop(&mut self) {
        match self.child.try_wait() {
            Ok(Some(_)) => {}
            Ok(None) => {
                let _ = self.child.kill();
                let _ = self.child.wait();
            }
            Err(_) => {}
        }
    }
}

fn remove_secret_env(command: &mut Command) {
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("TAU_SECRET_") {
            command.env_remove(key);
        }
    }
}

enum StdoutFrame {
    Message(HarnessInputMessage),
    Closed,
}

fn spawn_stdout_reader(
    stdout: std::process::ChildStdout,
) -> Result<Receiver<Result<StdoutFrame, DecodeError>>, SupervisionError> {
    let (sender, receiver) = mpsc::sync_channel(STDOUT_FRAME_BUFFER);
    thread::Builder::new()
        .name("tau-supervisor-stdout".to_owned())
        .spawn(move || {
            let mut reader = HarnessInputReader::new(BufReader::new(stdout));
            loop {
                match reader.read_message() {
                    Ok(Some(frame)) => {
                        if sender.send(Ok(StdoutFrame::Message(frame))).is_err() {
                            return;
                        }
                    }
                    Ok(None) => {
                        let _ = sender.send(Ok(StdoutFrame::Closed));
                        return;
                    }
                    Err(error) => {
                        let _ = sender.send(Err(error));
                        return;
                    }
                }
            }
        })
        .map_err(SupervisionError::ReaderThread)?;
    Ok(receiver)
}

#[cfg(unix)]
fn exit_signal(status: std::process::ExitStatus) -> Option<i32> {
    status.signal()
}

#[cfg(not(unix))]
fn exit_signal(_status: std::process::ExitStatus) -> Option<i32> {
    None
}
