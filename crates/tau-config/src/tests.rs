use tempfile::TempDir;

use super::*;

const USER_CONFIG_FIXTURE: &str = r#"
[core]
mode = "embedded"

[[extensions]]
name = "agent"
command = "tau-agent"
args = ["--model", "deterministic"]
role = "agent"

[[extensions]]
name = "shell"
command = "tau-ext-shell"
role = "tool"
"#;

const PROJECT_CONFIG_FIXTURE: &str = r#"
[core]
mode = "daemon"

[[extensions]]
name = "extra_tools"
command = "tau-ext-shell"
args = ["--login"]
role = "tool"
"#;

#[test]
fn default_user_config_path_uses_config_dir() {
    let paths = LoadPaths {
        config_dir: Some(PathBuf::from("/tmp/config")),
        current_dir: PathBuf::from("/tmp/project"),
    };

    assert_eq!(
        default_user_config_path(&paths),
        Some(PathBuf::from("/tmp/config/tau/config.toml"))
    );
}

#[test]
fn default_user_config_path_returns_none_without_config_dir() {
    let paths = LoadPaths {
        config_dir: None,
        current_dir: PathBuf::from("/tmp/project"),
    };

    assert_eq!(default_user_config_path(&paths), None);
}

#[test]
fn load_with_paths_automatically_loads_user_config_from_default_path() {
    let tempdir = TempDir::new().expect("tempdir should exist");
    let config_root = tempdir.path().join("config");
    let project_root = tempdir.path().join("project");
    fs::create_dir_all(config_root.join("tau")).expect("config path should be created");
    fs::create_dir_all(&project_root).expect("project path should be created");
    fs::write(
        config_root.join("tau").join("config.toml"),
        USER_CONFIG_FIXTURE,
    )
    .expect("user config should be written");

    let config = load_with_paths(
        &LoadOptions::default(),
        &LoadPaths {
            config_dir: Some(config_root),
            current_dir: project_root,
        },
    )
    .expect("config should load");

    assert_eq!(config.core.mode, CoreMode::Embedded);
    assert_eq!(config.extensions.len(), 2);
    assert_eq!(config.extensions[0].name, "agent");
    assert_eq!(config.extensions[1].name, "shell");
}

#[test]
fn project_config_is_appended_on_top_of_user_extensions() {
    let tempdir = TempDir::new().expect("tempdir should exist");
    let user_path = tempdir.path().join("user.toml");
    let project_path = tempdir.path().join("project.toml");
    fs::write(&user_path, USER_CONFIG_FIXTURE).expect("user config should be written");
    fs::write(&project_path, PROJECT_CONFIG_FIXTURE).expect("project config should be written");

    let config = load_with_paths(
        &LoadOptions {
            user_config_path: Some(user_path),
            enable_project_config: true,
            project_config_path: Some(project_path),
        },
        &LoadPaths {
            config_dir: None,
            current_dir: tempdir.path().to_path_buf(),
        },
    )
    .expect("config should load");

    assert_eq!(config.core.mode, CoreMode::Daemon);
    assert_eq!(config.extensions.len(), 3);
    assert_eq!(config.extensions[0].name, "agent");
    assert_eq!(config.extensions[1].name, "shell");
    assert_eq!(config.extensions[2].name, "extra_tools");
}

#[test]
fn project_config_is_ignored_when_not_enabled() {
    let tempdir = TempDir::new().expect("tempdir should exist");
    let user_path = tempdir.path().join("user.toml");
    let project_path = tempdir.path().join("project.toml");
    fs::write(&user_path, USER_CONFIG_FIXTURE).expect("user config should be written");
    fs::write(&project_path, PROJECT_CONFIG_FIXTURE).expect("project config should be written");

    let config = load_with_paths(
        &LoadOptions {
            user_config_path: Some(user_path),
            enable_project_config: false,
            project_config_path: Some(project_path),
        },
        &LoadPaths {
            config_dir: None,
            current_dir: tempdir.path().to_path_buf(),
        },
    )
    .expect("config should load");

    assert_eq!(config.core.mode, CoreMode::Embedded);
    assert_eq!(config.extensions.len(), 2);
    assert_eq!(config.extensions[0].name, "agent");
    assert_eq!(config.extensions[1].name, "shell");
}
