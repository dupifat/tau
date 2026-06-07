---
name: tau-self-knowledge-harness
description: >
  Use this skill when the user asks about the Tau harness daemon, including how
  it starts, accepts UI clients, uses Unix sockets, handles activation modes,
  socket activation, readiness signaling, attach behavior, or embedded harness
  runs.
advertise: false
---

# Tau harness daemon

The harness is Tau's central daemon. It owns session state, extension supervision, event routing, agent prompt orchestration, tool routing, and durable logs. UI clients connect to the harness; extensions connect to the harness; the harness is the single coordinator between them.

## Runtime-dir daemon mode

When the CLI starts a harness child, it creates a runtime-dir daemon unless it is attaching to an existing daemon.

Common startup flow:

1. The CLI chooses or resumes a session id.
2. The CLI creates a child `tau ext harness` process and passes session metadata through `TAU_SESSION_ID` and `TAU_SESSION_STATUS`.
3. The harness creates a per-process runtime directory under Tau's runtime root and binds `tau.sock` inside it.
4. After startup is ready for discovery, the harness writes runtime markers: project directory, pid, and session id.
5. Later UI clients discover the daemon through those markers and connect to the runtime-dir Unix socket.

The runtime-dir harness path always binds its generated socket path itself. It does not use socket activation, because Tau attach/discovery expects the socket to exist at the generated runtime-dir path.

## CLI readiness-pipe runtime-dir startup

CLI-managed non-chat/helper launches use the runtime-dir socket plus a readiness pipe. The parent passes `TAU_READY_FD=0` to the child and wires child stdin to a pipe. After the harness binds its runtime-dir socket and writes discovery markers, it writes one readiness byte to that fd. The parent then connects over the runtime-dir Unix socket.

This path is used when child stdin/stdout are not needed for the initial UI protocol.

## CLI-spawned initial UI stdio mode

When the terminal UI itself starts the harness, the CLI uses the hidden `tau ext harness --initial-ui-stdio` mode.

In this mode:

- child stdin/stdout are reserved for the initial UI protocol connection,
- the CLI does not use those pipes for the readiness byte,
- the harness bridges process stdin/stdout into an internal UnixStream client,
- extension and session startup wait until that initial UI has connected and subscribed,
- runtime markers are written after the startup state is ready for later socket attaches.

This prevents startup events from being missed by the UI that spawned the daemon. Later UIs still attach over the normal runtime-dir Unix socket.

CLI-managed daemon spawns explicitly remove `LISTEN_FDS`, `LISTEN_PID`, `LISTEN_FDS_FIRST_FD`, and `LISTEN_FDNAMES` from the child environment so unrelated socket-activation wrappers cannot accidentally change normal Tau startup.

## Attach mode

`tau --attach` does not start a new harness. It discovers an existing runtime-dir daemon for the current project and opens a Unix socket connection to that daemon.

Attach mode depends on runtime markers being accurate. If no matching daemon exists, attach fails instead of silently starting a new one.

## Foreground daemon APIs

The harness crate also exposes foreground daemon helpers such as `run_daemon`, `run_daemon_with_config`, and test-only echo variants. These APIs take an explicit socket path from the caller.

Foreground daemon APIs bind the provided path directly unless socket activation provides a listener.

## Socket activation

Foreground daemon APIs support socket activation via the `listenfd` crate.

Behavior:

- the harness checks `ListenFd::from_env().take_unix_listener(0)`,
- if no listener is present, it binds the requested socket path normally,
- if a listener is present, it must be a Unix stream listener,
- the listener's local pathname must exactly match the requested socket path,
- Tau does not remove the socket path on shutdown when the listener was externally provided.

This is intended for externally supervised foreground harness processes where the supervisor owns the socket. It is not used by the normal CLI-managed runtime-dir harness path.

## Direct `tau ext harness`

Running `tau ext harness` directly starts the harness component without the terminal UI parent. It uses the default session id when `TAU_SESSION_ID` is not set and binds its own runtime-dir socket.

This path is useful for debugging or embedding the harness component, but it does not receive an initial UI over stdio unless `--initial-ui-stdio` is supplied by the CLI-managed startup path.

## Embedded one-shot runs

Embedded helpers such as `run_embedded_message` do not create a daemon socket. They construct a harness in-process, run one interaction, and shut it down. Socket activation and runtime-dir attach discovery do not apply to embedded one-shot runs.
