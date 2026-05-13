# Code review: `crates/tau-supervisor`

Reviewed `crates/tau-supervisor` at current working copy.

Validation run:

- `cargo test -p tau-supervisor` passes.
- `cargo clippy -p tau-supervisor --all-targets -- -D warnings` currently fails in dependency crate `tau-proto`, not in `tau-supervisor` itself.


## High priority

### 1. Truncated child output is silently treated as no frame

Location: `crates/tau-supervisor/src/lib.rs`, `SupervisedChild::recv_timeout`, `is_unexpected_eof`.

`FrameReader::read_frame` documents that clean EOF returns `Ok(None)` and truncated data returns `Err`. `recv_timeout` currently maps `DecodeError::Io(UnexpectedEof)` to `Ok(None)`:

```rust
Ok(Err(error)) if is_unexpected_eof(&error) => Ok(None),
```

That hides protocol corruption and makes a child that dies mid-frame look the same as a timeout or clean disconnect. The harness can then miss a broken extension and fail to surface useful diagnostics.

Action:

- Remove the `is_unexpected_eof` special case.
- Let truncated frames return `Err(SupervisionError::Decode(error))`.
- Add a test child mode that writes a partial CBOR frame and exits, then assert `recv_timeout` returns `Err(SupervisionError::Decode(_))`.


### 2. `recv_timeout` conflates timeout, clean EOF, reader-thread crash, and disconnect

Location: `crates/tau-supervisor/src/lib.rs`, `SupervisedChild::recv_timeout`.

The method returns `Ok(None)` for:

- actual timeout,
- clean child stdout EOF,
- stdout reader channel disconnect,
- currently also truncated EOF via the special case above.

Callers cannot distinguish “nothing happened yet” from “the extension stdout is gone”. That is dangerous for supervision because a caller polling with a timeout can keep treating a dead extension as idle until some other path notices process exit.

Action:

- Replace `Result<Option<Frame>, SupervisionError>` with a richer enum, for example:

```rust
pub enum RecvOutcome {
    Frame(Frame),
    Timeout,
    Eof,
}
```

- Map `RecvTimeoutError::Timeout` to `RecvOutcome::Timeout`.
- Map clean EOF / channel disconnect to `RecvOutcome::Eof`.
- Update tests to assert EOF after a graceful `Disconnect`.


### 3. The stdout reader uses an unbounded channel

Location: `crates/tau-supervisor/src/lib.rs`, `spawn_stdout_reader`.

The background thread sends decoded frames into `std::sync::mpsc::channel()`, which is unbounded. A malicious or buggy extension can emit frames faster than the harness drains them and grow memory without limit.

Action:

- Use `mpsc::sync_channel(capacity)` with a documented capacity, or move to an async/bounded transport used by the harness.
- Decide the backpressure policy explicitly: block the reader thread, disconnect the child, or drop after emitting a supervision error.
- Add a stress test with a child that floods frames while the parent does not read immediately.


## Medium priority

### 4. Failed spawn setup can leave a child process running

Location: `crates/tau-supervisor/src/lib.rs`, `SupervisedChild::spawn`.

After `Command::spawn()` succeeds, errors such as `MissingStdin` or `MissingStdout` return early. Dropping `std::process::Child` does not kill or wait for the process, so a setup failure can orphan the child.

This is unlikely with `Stdio::piped()`, but the cleanup invariant should still be correct.

Action:

- On any post-spawn setup error, kill and wait for the child before returning.
- Consider a helper that converts setup failures into `SupervisionError` only after cleanup.
- Add a regression test if practical, or at least make the code structurally impossible to return after spawn without either storing the child in `SupervisedChild` or reaping it.


### 5. `wait_for_exit` timeout can overshoot and spin-polls

Location: `crates/tau-supervisor/src/lib.rs`, `SupervisedChild::wait_for_exit`.

The loop sleeps for a fixed 10 ms. For small timeouts it can overshoot significantly, and for many supervised children it causes periodic polling.

Action:

- Sleep for `min(10 ms, remaining_timeout)`.
- Consider using a waiter thread/channel if this crate will supervise many extensions concurrently.
- Add a unit test for immediate timeout behavior.


### 6. `ToolRoute` error variant appears unused

Location: `crates/tau-supervisor/src/lib.rs`, `SupervisionError::ToolRoute`.

`cleanup_disconnect` is infallible and `ToolRoute` is not constructed in this crate. This looks like stale design residue and makes the public error API noisier.

Action:

- Remove `ToolRoute` from `SupervisionError` if cleanup is intentionally infallible.
- Or make cleanup perform the fallible routing operation that this variant was meant to represent.


## Low priority

### 7. `ExtensionCommand::argv` display format is lossy

Location: `crates/tau-supervisor/src/lib.rs`, `ExtensionCommand::argv`.

`program.display().to_string()` is intended for display, not lossless argv reconstruction. Non-UTF-8 Unix paths or paths with shell-sensitive characters can be misleading in diagnostics.

Action:

- Rename to `display_argv` if this is only for diagnostics.
- Or return `Vec<OsString>` if callers need the real argv.


### 8. Test helper protocol is too single-path

Location: `crates/tau-supervisor/src/bin/tau-supervisor-test-child.rs` and `crates/tau-supervisor/tests/supervisor.rs`.

The test child only covers the happy path plus graceful disconnect. The risky supervisor behavior is around malformed output, early exit, flooding, and parent dropping without explicit disconnect.

Action:

- Add command-line modes to the test child:
  - `--exit-immediately`,
  - `--partial-frame`,
  - `--flood`,
  - `--ignore-disconnect`.
- Add tests around each mode so process cleanup and receive semantics are locked down.
