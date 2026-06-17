use super::*;

/// Ensures CLI and environment theme names share the same normalization path,
/// including external names that must not be rejected during parsing.
#[test]
fn parses_theme_env_values() {
    assert_eq!(parse_theme_name("auto"), Some(CliTheme::Auto));
    assert_eq!(parse_theme_name("DARK"), Some(CliTheme::Dark));
    assert_eq!(parse_theme_name(" light "), Some(CliTheme::Light));
    assert_eq!(
        parse_theme_name("solarized"),
        Some(CliTheme::Named("solarized".to_owned()))
    );
    assert_eq!(parse_theme_name("   "), None);
}

/// Ensures built-in theme file names remain selectable without requiring a Tau
/// config directory. This intentionally avoids asserting built-in palette
/// details so theme tuning does not churn selector tests.
#[test]
fn selected_named_builtin_theme() {
    let dirs = tau_config::settings::TauDirs {
        config_dir: None,
        state_dir: None,
    };

    let theme = select_theme(&dirs, CliTheme::Named("dpc".to_owned()))
        .expect("built-in theme loads without config dir");
    let prompt = cwd_right_prompt(&theme, Path::new("/tmp/project"), None);

    assert_eq!(prompt.spans()[0].text, "/tmp/project");
}

/// Ensures external theme names resolve to `themes/<name>.json5` under Tau's
/// config directory and affect normal terminal style resolution.
#[test]
fn selected_external_theme_from_config_themes_dir() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    let themes = temp.path().join("themes");
    std::fs::create_dir(&themes).expect("themes dir");
    std::fs::write(
        themes.join("custom.json5"),
        r##"{ styles: { "prompt.cwd": { fg: "red", bold: true } } }"##,
    )
    .expect("write theme");
    let dirs = tau_config::settings::TauDirs {
        config_dir: Some(temp.path().to_owned()),
        state_dir: None,
    };

    let theme = select_theme(&dirs, CliTheme::Named("custom".to_owned())).expect("theme loads");
    let prompt = cwd_right_prompt(&theme, Path::new("/tmp/project"), None);

    assert_eq!(prompt.spans()[0].style.fg, Some(tau_cli_term::Color::Red));
    assert!(prompt.spans()[0].style.bold);
}

/// Ensures invalid external names fail visibly instead of escaping the themes
/// directory or silently falling back to a built-in theme.
#[test]
fn selected_external_theme_rejects_path_components() {
    let dirs = tau_config::settings::TauDirs {
        config_dir: Some(Path::new("/tmp/tau-test").to_owned()),
        state_dir: None,
    };

    let err = select_theme(&dirs, CliTheme::Named("../bad".to_owned())).expect_err("rejects name");

    assert!(err.to_string().contains("invalid theme name"));
}

#[test]
fn colorfgbg_detects_light_background() {
    assert_eq!(
        colorfgbg_terminal_shade_from("0;15"),
        Some(TerminalShade::Light)
    );
    assert_eq!(
        colorfgbg_terminal_shade_from("0;7"),
        Some(TerminalShade::Light)
    );
}

#[test]
fn colorfgbg_detects_dark_background() {
    assert_eq!(
        colorfgbg_terminal_shade_from("15;0"),
        Some(TerminalShade::Dark)
    );
    assert_eq!(
        colorfgbg_terminal_shade_from("7;8"),
        Some(TerminalShade::Dark)
    );
}

#[test]
fn colorfgbg_ignores_malformed_values() {
    assert_eq!(colorfgbg_terminal_shade_from(""), None);
    assert_eq!(colorfgbg_terminal_shade_from("0;wat"), None);
}

#[test]
fn display_cwd_replaces_home_prefix() {
    assert_eq!(
        display_cwd(
            Path::new("/home/alice/project"),
            Some(Path::new("/home/alice"))
        ),
        "~/project"
    );
    assert_eq!(
        display_cwd(Path::new("/home/alice"), Some(Path::new("/home/alice"))),
        "~"
    );
    assert_eq!(
        display_cwd(
            Path::new("/home/alice2/project"),
            Some(Path::new("/home/alice"))
        ),
        "/home/alice2/project"
    );
}

#[test]
fn prompt_input_placeholder_keeps_placeholder_style_around_role_style() {
    let theme = tau_themes::Theme::parse(
        r##"
            {
                styles: {
                    "prompt.placeholder": { fg: "dark_grey", italic: true },
                    "status.role": { fg: "cyan", bold: true },
                }
            }
            "##,
    )
    .expect("test theme parses");
    let prompt = prompt_input_placeholder(&theme, Some("engineer"), None, false);
    let spans = prompt.spans();

    assert_eq!(spans.len(), 3);
    assert_eq!(spans[0].text, "Write a message to start a new ");
    assert_eq!(spans[0].style.fg, Some(tau_cli_term::Color::DarkGrey));
    assert!(spans[0].style.italic);
    assert_eq!(spans[1].text, "engineer");
    assert_eq!(spans[1].style.fg, Some(tau_cli_term::Color::Cyan));
    assert!(spans[1].style.bold);
    assert!(spans[1].style.italic);
    assert_eq!(spans[2].text, " agent...");
    assert_eq!(spans[2].style.fg, Some(tau_cli_term::Color::DarkGrey));
    assert!(spans[2].style.italic);

    let prompt = prompt_input_placeholder(&theme, Some("engineer"), Some("engineer_abc"), false);
    let spans = prompt.spans();
    assert_eq!(spans[0].text, "Write a message to ");
    assert_eq!(spans[1].text, "engineer_abc");
    assert_eq!(spans[2].text, "...");
    assert_eq!(spans[2].style.fg, Some(tau_cli_term::Color::DarkGrey));
    assert!(spans[2].style.italic);
}

#[test]
fn suspended_prompt_input_placeholder_explains_messages_are_blocked() {
    // Regression coverage for the disabled-input copy shown while the selected
    // agent is suspended. The text must make clear that users need to resume it
    // before sending messages.
    let theme = tau_themes::Theme::new();
    let prompt = prompt_input_placeholder(&theme, Some("engineer"), Some("engineer_abc"), true);
    let text: String = prompt
        .spans()
        .iter()
        .map(|span| span.text.as_str())
        .collect();

    assert_eq!(
        text,
        "This agent is suspended. Use /resume before sending messages."
    );
}

#[test]
fn cwd_right_prompt_uses_prompt_cwd_style() {
    let theme = tau_themes::Theme::parse(r##"{ styles: { "prompt.cwd": { fg: "dark_grey" } } }"##)
        .expect("test theme parses");
    let prompt = cwd_right_prompt(&theme, Path::new("/tmp/project"), None);

    assert_eq!(prompt.spans()[0].text, "/tmp/project");
    assert_eq!(
        prompt.spans()[0].style.fg,
        Some(tau_cli_term::Color::DarkGrey)
    );
}
