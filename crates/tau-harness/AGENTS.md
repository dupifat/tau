# tau-harness

- Do not drop, downgrade, or make startup-only any extension `Message::ConfigError`. The harness must convert it into Important `harness.info` visible in the UI.
- Important harness diagnostics, especially config parse errors, must be replayed to late UI subscribers. Daemon startup commonly finishes extension configuration before the terminal UI subscribes, so live-only publication is insufficient.
