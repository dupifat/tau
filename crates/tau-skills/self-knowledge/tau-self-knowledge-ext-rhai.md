---
name: tau-self-knowledge-ext-rhai
description: Use this extension skill when the user asks about Tau's disabled std-rhai scripting extension, Rhai event hooks, script config, subscriptions, interceptions, host functions, or scripting limitations.
advertise: false
---

# Tau std-rhai extension self-knowledge

`std-rhai` is Tau's disabled-by-default trusted local scripting extension. It runs `tau-ext-rhai` and lets a user-provided Rhai script observe Tau events, optionally intercept matching events, and emit Tau events through a small host API. The Rust extension owns Tau protocol framing; scripts see JSON-shaped event maps matching Serde's event form.


## Configuration

Enable it under `extensions.std-rhai` and provide a script path:

```yaml
extensions:
  std-rhai:
    enable: true
    config:
      script: "/home/me/.config/tau/hooks/demo.rhai"
      vars:
        greeting: "hello"
      limits:
        max_operations: 1000000
```

`script` is required. Absolute paths are preferred; relative paths are resolved from the extension process current working directory. `vars` is arbitrary JSON-compatible data passed into `init(config)`. The harness-provided `state_dir`, when present, is also passed to `init`.

If config parsing, script reading, compilation, or `init` fails, the extension sends `ConfigError`, then `Ready` with a `rhai disabled: ...` message, and stays alive inert instead of exiting in a restart loop.


## Script callbacks

Scripts use JSON-shaped event maps such as:

```rhai
#{ event: "harness.info", payload: #{ message: "hi", level: "normal" } }
```

Supported callbacks:

```rhai
fn init(config) {
    return #{
        subscribe: [#{ kind: "exact", value: "agent.prompt_submitted" }],
        intercept: [#{
            selectors: [#{ kind: "prefix", value: "agent." }],
            priority: 0,
        }],
        ready_message: "rhai ready",
    };
}

fn on_event(event, meta) {
    if event.event == "agent.prompt_submitted" {
        tau_info(`saw prompt: ${event.payload.text}`);
    }
}

fn on_intercept(event, transient) {
    event.payload.text = event.payload.text.replace("tao", "tau");
    return #{ kind: "pass", event: event };
}
```

`init(config)` is optional. Missing `init` or unit/no-op return means no subscriptions, no intercepts, and the default ready message. `subscribe` uses selector maps with `kind: "exact"` or `kind: "prefix"`. Multiple `intercept` entries are allowed only when they share the same priority; their selectors are merged into one registration because the harness supports one interceptor registration per extension connection.

`on_event(event, meta)` is optional and is called for delivered subscribed events. `meta.seq` and `meta.recorded_at` are present when the harness supplies them. Sequenced deliveries are acknowledged after the callback attempt, even if the script errors, so a bad script cannot wedge delivery.

`on_intercept(event, transient)` is optional and returns one of:

- `()` / `"pass"` / `#{ kind: "pass" }` to pass the original event.
- `#{ kind: "pass", event: event }` to pass a replacement event.
- `"drop"` / `#{ kind: "drop" }` to drop the event.

On script errors or invalid intercept returns, Tau reports a transient important `harness.info` diagnostic and defaults to passing the original event.


## Host functions

Host functions are registered only after `init` succeeds, not during `init`:

- `tau_emit(event)` emits a durable Tau event map.
- `tau_emit_transient(event)` emits a transient Tau event map.
- `tau_info(message)` and `tau_info(message, level)` emit transient `harness.info`; `level` is `normal` or `important`.
- `tau_log(level, message)` writes only to extension logs.


## Safety and limitations

`std-rhai` runs trusted local scripts. Tau does not register filesystem, network, or process APIs for Rhai in this prototype, but scripts can still affect Tau by emitting or intercepting events. Execution limits include `limits.max_operations`, `limits.max_call_levels`, and `limits.max_expr_depth`.

Event conversion supports the JSON-compatible subset of Tau payloads. Arbitrary CBOR bytes, tags, and non-string map keys are not faithfully represented yet. The prototype does not register model tools or slash actions from Rhai scripts.
