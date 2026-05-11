use tau_config::settings::{ExtensionEntry, HarnessSettings, load_harness_settings_in};
use tempfile::TempDir;

use super::*;

fn builtins() -> Vec<BuiltinExtension> {
    vec![
        BuiltinExtension {
            name: "core-agent",
            command: vec!["tau".into(), "ext".into(), "agent".into()],
            role: Some("agent"),
            enable: true,
            config: serde_json::json!({}),
        },
        BuiltinExtension {
            name: "core-shell",
            command: vec!["tau".into(), "ext".into(), "ext-shell".into()],
            role: Some("tool"),
            enable: true,
            config: serde_json::json!({}),
        },
        BuiltinExtension {
            name: "test-dummy",
            command: vec!["tau".into(), "ext".into(), "ext-test-dummy".into()],
            role: Some("tool"),
            enable: false,
            config: serde_json::json!({}),
        },
        BuiltinExtension {
            name: "std-notifications",
            command: vec!["tau".into(), "ext".into(), "ext-std-notifications".into()],
            role: Some("tool"),
            enable: true,
            config: serde_json::json!({ "idle_seconds": 60 }),
        },
    ]
}

#[test]
fn resolve_extensions_returns_builtins_when_user_config_empty() {
    let s = HarnessSettings::default();
    let resolved = resolve_extensions(&s, builtins()).expect("resolve");
    assert_eq!(resolved.len(), 3);
    assert_eq!(resolved[0].name, "core-agent");
    assert_eq!(resolved[0].command, "tau");
    assert_eq!(resolved[0].args, vec!["ext", "agent"]);
    assert_eq!(resolved[0].role.as_deref(), Some("agent"));
    assert_eq!(resolved[1].name, "core-shell");
    assert_eq!(resolved[2].name, "std-notifications");
}

#[test]
fn resolve_extensions_builtin_can_start_disabled() {
    let s = HarnessSettings::default();
    let resolved = resolve_extensions(&s, builtins()).expect("resolve");
    assert!(resolved.iter().all(|e| e.name != "test-dummy"));
}

#[test]
fn resolve_extensions_disable_drops_entry() {
    let mut s = HarnessSettings::default();
    s.extensions.insert(
        "core-shell".into(),
        ExtensionEntry {
            enable: Some(false),
            ..Default::default()
        },
    );
    let resolved = resolve_extensions(&s, builtins()).expect("resolve");
    assert_eq!(resolved.len(), 2);
    assert_eq!(resolved[0].name, "core-agent");
    assert_eq!(resolved[1].name, "std-notifications");
}

#[test]
fn resolve_extensions_prefix_wraps_builtin_command() {
    let mut s = HarnessSettings::default();
    s.extensions.insert(
        "core-agent".into(),
        ExtensionEntry {
            prefix: Some(vec!["ssh".into(), "user@host".into()]),
            ..Default::default()
        },
    );
    let resolved = resolve_extensions(&s, builtins()).expect("resolve");
    let agent = resolved
        .iter()
        .find(|e| e.name == "core-agent")
        .expect("agent");
    // argv[0] is the wrapper; original command moves into args.
    assert_eq!(agent.command, "ssh");
    assert_eq!(agent.args, vec!["user@host", "tau", "ext", "agent"]);
}

#[test]
fn resolve_extensions_user_command_replaces_builtin_command() {
    let mut s = HarnessSettings::default();
    s.extensions.insert(
        "core-agent".into(),
        ExtensionEntry {
            command: Some(vec!["/usr/local/bin/my-agent".into(), "--flag".into()]),
            ..Default::default()
        },
    );
    let resolved = resolve_extensions(&s, builtins()).expect("resolve");
    let agent = resolved
        .iter()
        .find(|e| e.name == "core-agent")
        .expect("agent");
    assert_eq!(agent.command, "/usr/local/bin/my-agent");
    assert_eq!(agent.args, vec!["--flag"]);
    // Role is preserved from the built-in default.
    assert_eq!(agent.role.as_deref(), Some("agent"));
}

#[test]
fn resolve_extensions_adds_user_extension_keys() {
    let mut s = HarnessSettings::default();
    s.extensions.insert(
        "mything".into(),
        ExtensionEntry {
            command: Some(vec!["/usr/local/bin/mything".into()]),
            ..Default::default()
        },
    );
    let resolved = resolve_extensions(&s, builtins()).expect("resolve");
    assert_eq!(resolved.len(), 4);
    let mything = resolved
        .iter()
        .find(|e| e.name == "mything")
        .expect("mything");
    assert_eq!(mything.command, "/usr/local/bin/mything");
    assert!(mything.role.is_none());
}

#[test]
fn resolve_extensions_empty_entry_does_not_re_enable_disabled_builtin() {
    // `extensions: { "test-dummy": {} }` MUST leave the
    // builtin's `enable: false` intact — absent fields mean "no
    // override", not "use the wire default". See review item #4.
    let mut s = HarnessSettings::default();
    s.extensions
        .insert("test-dummy".into(), ExtensionEntry::default());
    let resolved = resolve_extensions(&s, builtins()).expect("resolve");
    assert!(resolved.iter().all(|e| e.name != "test-dummy"));
}

#[test]
fn resolve_extensions_user_extension_without_command_errors() {
    let mut s = HarnessSettings::default();
    s.extensions.insert(
        "broken".into(),
        ExtensionEntry {
            ..Default::default()
        },
    );
    let err = resolve_extensions(&s, builtins()).expect_err("must err");
    match err {
        ResolveExtensionsError::EmptyCommand(name) => assert_eq!(name, "broken"),
    }
}

#[test]
fn resolve_extensions_loads_from_json5() {
    // End-to-end: a realistic harness.json5 round-trips through the
    // tau-config loader into the tau-harness resolver.
    let td = TempDir::new().expect("tempdir");
    let dir = td.path();
    std::fs::write(
        dir.join("harness.json5"),
        r#"{
                extensions: {
                    "core-shell": { enable: false },
                    "test-dummy": { enable: true },
                    "core-agent": { prefix: ["ssh", "host"] },
                    mything: { command: ["/bin/foo"] },
                },
            }"#,
    )
    .expect("write");

    let dirs = tau_config::settings::TauDirs {
        config_dir: Some(dir.to_owned()),
        state_dir: None,
    };
    let s = load_harness_settings_in(&dirs).expect("load");
    let resolved = resolve_extensions(&s, builtins()).expect("resolve");
    let names: Vec<&str> = resolved.iter().map(|e| e.name.as_str()).collect();
    // core-shell dropped (disable). test-dummy enabled. core-agent
    // kept (prefix-wrapped). mything appended.
    assert_eq!(
        names,
        vec!["core-agent", "test-dummy", "std-notifications", "mything"]
    );
    let agent = &resolved[0];
    assert_eq!(agent.command, "ssh");
    assert_eq!(agent.args, vec!["host", "tau", "ext", "agent"]);
}
