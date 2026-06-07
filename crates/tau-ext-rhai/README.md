# tau-ext-rhai

Prototype trusted local scripting extension for Tau. The built-in extension is disabled by default; enable it with a script path:

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

`script` is required. Relative paths are resolved by the extension process current working directory; absolute paths are preferred. `vars` is passed to `init(config)` and `start(config)` as JSON-compatible data, along with `state_dir` when the harness provides one.

Scripts see Tau events as JSON-shaped maps matching Serde's JSON form:

```rhai
#{ event: "harness.info", payload: #{ message: "hi", level: "normal" } }
```

## Callbacks

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

fn start(config) {
    tau_info(`rhai started with greeting: ${config.vars.greeting}`);
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

`init(config)` is optional. A missing `init` or a no-op/unit return means no subscriptions, no intercepts, and the default ready message. `subscribe` accepts selector maps shaped as `#{ kind: "exact", value: "agent.prompt_submitted" }` or `#{ kind: "prefix", value: "tool." }`. Multiple `intercept` entries are merged only when they use the same `priority`; different priorities are rejected because the harness has one interceptor registration per connection.

`start(config)` is optional and runs once after `init` succeeds, subscriptions/intercepts are sent, `Ready` is sent, and host functions are registered. Use it for startup side effects such as `tau_info`; callback errors are reported as important transient diagnostics without disabling the extension.

`on_event(event, meta)` is optional. `meta.seq` and `meta.recorded_at` are present when Tau supplies them. The extension acknowledges sequenced deliveries after the callback has had a chance to run.

`on_intercept(event, transient)` is optional. Return values are:

- `()` or `"pass"` or `#{ kind: "pass" }` — pass the original event.
- `#{ kind: "pass", event: event }` — pass a replacement event.
- `"drop"` or `#{ kind: "drop" }` — drop the event.

## Host functions

- `tau_emit(event)` — emit a durable Tau event map.
- `tau_emit_transient(event)` — emit a transient Tau event map.
- `tau_info(message)` / `tau_info(message, level)` — emit transient `harness.info`; `level` is `"normal"` or `"important"`.
- `tau_log(level, message)` — write to extension logs only.

Host functions are available to `start`, `on_event`, and `on_intercept`, but not during `init`. This keeps broken init scripts inert.

## Limitations

Scripts are trusted local code. Rhai does not expose filesystem, network, or process APIs unless Tau registers them, and this prototype does not. Event conversion supports the JSON-compatible subset of Tau payloads; arbitrary CBOR bytes, tags, and non-string map keys are not faithfully represented yet. This prototype does not register tools or actions.
