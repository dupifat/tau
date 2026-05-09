## Workspace layout

- `crates/tau` — main end-user binary that bundles first-party components
- `crates/tau-agent` — first-party LLM conversation driver and provider adapters
- `crates/tau-blocking-notify-channel` — blocking notification channel utility
- `crates/tau-cli` — CLI entrypoint: starts harness daemon and connects UI clients
- `crates/tau-cli-term` — terminal UI client
- `crates/tau-cli-term-raw` — raw terminal rendering/input layer
- `crates/tau-config` — user and project configuration loading
- `crates/tau-core` — event bus, routing, state, sessions, policy, and tool registry
- `crates/tau-ext-core-delegate` — built-in delegate/sub-agent extension
- `crates/tau-ext-core-notifications` — built-in notification extension
- `crates/tau-ext-shell` — shell- and filesystem-oriented extension
- `crates/tau-ext-test-dummy` — test-only dummy extension
- `crates/tau-ext-websearch-exa` — opt-in Exa web search extension
- `crates/tau-extension` — extension-side protocol/runtime helpers
- `crates/tau-harness` — harness daemon: extensions, bus, sessions, socket server
- `crates/tau-provider` — provider credential/config support
- `crates/tau-proto` — shared protocol types and CBOR codec helpers
- `crates/tau-skills` — skill discovery/loading support
- `crates/tau-socket` — Unix socket transport glue
- `crates/tau-supervisor` — supervised child-process and stdio transport glue
- `crates/tau-test-support` — reusable end-to-end test utilities
- `crates/tau-themes` — themed text/style types

## Design docs

- `README.md` — project overview, install, configuration, and contact info
- `FEATURES.md` — major feature tour; update after editing any new major features
- `docs/` — focused design and feature notes
- `crates/*/README.md` — crate-specific documentation where present

## Common commands

- `nix develop` — enter the dev shell
- `cargo check --workspace --all-targets` or `just check` — check Rust code
- `cargo test` or `just test` — run tests
- `treefmt` or `just format` — format code
- `selfci check` — full local verification

## Definition of done

- Code is formatted.
- Relevant tests pass.
- Run `selfci check` after every major change.
- Update `FEATURES.md` after editing any new major features.
