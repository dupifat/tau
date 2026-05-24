use std::path::Path;

use tau_config::settings::CliTheme;
use tau_themes::{SpanTree, ThemedText};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TerminalShade {
    Dark,
    Light,
}

const THEME_ENV: &str = "TAU_THEME";

pub(crate) fn active_prompt_marker(
    theme: &tau_themes::Theme,
    prompt_symbol: &str,
    role: Option<&str>,
) -> tau_cli_term::StyledText {
    let mut text = ThemedText::new();
    let base_style = text.add_style(tau_themes::names::PROMPT_MARKER);
    let marker = format!("{prompt_symbol} ");

    let marker = if let Some(role) = role {
        let role_style = text.add_style(prompt_marker_role_style(role));
        SpanTree::span(
            base_style,
            vec![SpanTree::span(role_style, vec![SpanTree::text(marker)])],
        )
    } else {
        SpanTree::span(base_style, vec![SpanTree::text(marker)])
    };

    text.push_tree(marker);
    tau_cli_term::resolve::themed_text(theme, &text)
}

pub(crate) fn prompt_input_placeholder(
    theme: &tau_themes::Theme,
    role: Option<&str>,
    current_agent_id: Option<&str>,
) -> tau_cli_term::StyledText {
    let role = role.unwrap_or("agent");
    let mut text = ThemedText::new();
    let placeholder_style = text.add_style(tau_themes::names::PROMPT_PLACEHOLDER);
    let role_style = text.add_style(tau_themes::names::STATUS_ROLE);

    let parts = match current_agent_id {
        Some(agent_id) => vec![
            SpanTree::text("Write a message to "),
            SpanTree::span(role_style, vec![SpanTree::text(agent_id)]),
            SpanTree::text("..."),
        ],
        None => vec![
            SpanTree::text("Write a message to start a new "),
            SpanTree::span(role_style, vec![SpanTree::text(role)]),
            SpanTree::text(" agent..."),
        ],
    };

    text.push_tree(SpanTree::span(placeholder_style, parts));
    tau_cli_term::resolve::themed_text(theme, &text)
}

pub(crate) fn cwd_right_prompt(
    theme: &tau_themes::Theme,
    cwd: &Path,
    home: Option<&Path>,
) -> tau_cli_term::StyledText {
    let mut text = ThemedText::new();
    let cwd_style = text.add_style(tau_themes::names::PROMPT_CWD);
    text.push(cwd_style, display_cwd(cwd, home));
    tau_cli_term::resolve::themed_text(theme, &text)
}

fn display_cwd(cwd: &Path, home: Option<&Path>) -> String {
    if let Some(home) = home.filter(|home| !home.as_os_str().is_empty())
        && let Ok(relative) = cwd.strip_prefix(home)
    {
        if relative.as_os_str().is_empty() {
            return "~".to_owned();
        }
        return format!("~/{}", relative.display());
    }

    cwd.display().to_string()
}

fn prompt_marker_role_style(role: &str) -> String {
    format!("{}.{}", tau_themes::names::PROMPT_MARKER, role)
}

pub(crate) fn select_theme(mode: CliTheme) -> tau_themes::Theme {
    let mode = env_theme_override().unwrap_or(mode);

    match mode {
        CliTheme::Dark => tau_themes::Theme::builtin_dark(),
        CliTheme::Light => tau_themes::Theme::builtin_light(),
        CliTheme::Auto => match detect_terminal_shade() {
            Some(TerminalShade::Light) => tau_themes::Theme::builtin_light(),
            Some(TerminalShade::Dark) | None => tau_themes::Theme::builtin_dark(),
        },
    }
}

fn env_theme_override() -> Option<CliTheme> {
    let value = std::env::var(THEME_ENV).ok()?;
    parse_theme_name(&value)
}

fn parse_theme_name(value: &str) -> Option<CliTheme> {
    match value.trim().to_ascii_lowercase().as_str() {
        "auto" => Some(CliTheme::Auto),
        "dark" => Some(CliTheme::Dark),
        "light" => Some(CliTheme::Light),
        _ => None,
    }
}

fn detect_terminal_shade() -> Option<TerminalShade> {
    colorfgbg_terminal_shade()
}

fn colorfgbg_terminal_shade() -> Option<TerminalShade> {
    let value = std::env::var("COLORFGBG").ok()?;
    colorfgbg_terminal_shade_from(&value)
}

fn colorfgbg_terminal_shade_from(value: &str) -> Option<TerminalShade> {
    let bg = value.rsplit([';', ':']).next()?.parse::<u8>().ok()?;
    match bg {
        7 | 15 => Some(TerminalShade::Light),
        _ => Some(TerminalShade::Dark),
    }
}

#[cfg(test)]
mod tests;
