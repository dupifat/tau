use tau_config::settings::CliTheme;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TerminalShade {
    Dark,
    Light,
}

const THEME_ENV: &str = "TAU_THEME";

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
mod tests {
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
}
