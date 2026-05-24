use super::*;

#[test]
fn parses_theme_env_values() {
    assert_eq!(parse_theme_name("auto"), Some(CliTheme::Auto));
    assert_eq!(parse_theme_name("DARK"), Some(CliTheme::Dark));
    assert_eq!(parse_theme_name(" light "), Some(CliTheme::Light));
    assert_eq!(parse_theme_name("solarized"), None);
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
    let prompt = prompt_input_placeholder(&theme, Some("engineer"), None);
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

    let prompt = prompt_input_placeholder(&theme, Some("engineer"), Some("engineer_abc"));
    let spans = prompt.spans();
    assert_eq!(spans[0].text, "Write a message to ");
    assert_eq!(spans[1].text, "engineer_abc");
    assert_eq!(spans[2].text, "...");
    assert_eq!(spans[2].style.fg, Some(tau_cli_term::Color::DarkGrey));
    assert!(spans[2].style.italic);
}

#[test]
fn cwd_right_prompt_uses_prompt_cwd_style() {
    let prompt = cwd_right_prompt(
        &tau_themes::Theme::builtin(),
        Path::new("/tmp/project"),
        None,
    );

    assert_eq!(prompt.spans()[0].text, "/tmp/project");
    assert_eq!(
        prompt.spans()[0].style.fg,
        Some(tau_cli_term::Color::DarkGrey)
    );
}
