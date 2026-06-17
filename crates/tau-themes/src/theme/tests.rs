use super::*;

#[test]
fn empty_theme_resolves_to_defaults() {
    let theme = Theme::new();
    let mut text = ThemedText::new();
    let s = text.add_style("whatever");
    text.push(s, "hello");

    let resolved = theme.resolve(&text);
    assert_eq!(resolved.len(), 1);
    assert_eq!(resolved[0].text, "hello");
    assert_eq!(resolved[0].style, ThemeStyle::default());
}

#[test]
fn named_style_resolves() {
    let theme: Theme = Theme::parse(
        r#"{
                styles: {
                    prompt: { fg: "green", bold: true, strikethrough: true },
                }
            }"#,
    )
    .expect("valid theme");

    let mut text = ThemedText::new();
    let prompt = text.add_style("prompt");
    text.push(prompt, ">");

    let resolved = theme.resolve(&text);
    assert_eq!(resolved[0].style.fg, Some(Color::Green));
    assert!(resolved[0].style.bold);
    assert!(!resolved[0].style.italic);
    assert!(resolved[0].style.strikethrough);
}

#[test]
fn default_idx_resolves_to_default_style() {
    let theme: Theme = Theme::parse(
        r#"{
                styles: {
                    prompt: { fg: "red" },
                }
            }"#,
    )
    .expect("valid theme");

    let mut text = ThemedText::new();
    text.push_default("plain text");

    let resolved = theme.resolve(&text);
    assert_eq!(resolved[0].style, ThemeStyle::default());
}

#[test]
fn hex_color_in_theme() {
    let theme: Theme = Theme::parse(
        r##"{
                styles: {
                    custom: { fg: "#ff8800", bg: "#001122" },
                }
            }"##,
    )
    .expect("valid theme");

    let mut text = ThemedText::new();
    let s = text.add_style("custom");
    text.push(s, "colored");

    let resolved = theme.resolve(&text);
    assert_eq!(
        resolved[0].style.fg,
        Some(Color::Rgb {
            r: 0xff,
            g: 0x88,
            b: 0x00
        })
    );
    assert_eq!(
        resolved[0].style.bg,
        Some(Color::Rgb {
            r: 0x00,
            g: 0x11,
            b: 0x22
        })
    );
}

#[test]
fn multiple_spans_resolve_independently() {
    let theme: Theme = Theme::parse(
        r#"{
                styles: {
                    error: { fg: "red", bold: true },
                    muted: { fg: "dark_grey" },
                }
            }"#,
    )
    .expect("valid theme");

    let mut text = ThemedText::new();
    let error = text.add_style("error");
    let muted = text.add_style("muted");
    text.push(error, "ERROR: ");
    text.push(muted, "details here");
    text.push_default(" (ok)");

    let resolved = theme.resolve(&text);
    assert_eq!(resolved.len(), 3);

    assert_eq!(resolved[0].style.fg, Some(Color::Red));
    assert!(resolved[0].style.bold);

    assert_eq!(resolved[1].style.fg, Some(Color::DarkGrey));
    assert!(!resolved[1].style.bold);

    assert_eq!(resolved[2].style, ThemeStyle::default());
}

#[test]
fn nested_spans_inherit_and_override_styles() {
    let theme: Theme = Theme::parse(
        r#"{
                styles: {
                    outer: { fg: "red", bg: "dark_blue", bold: true },
                    inner: { fg: "green", italic: true },
                }
            }"#,
    )
    .expect("valid theme");

    let mut text = ThemedText::new();
    let outer = text.add_style("outer");
    let inner = text.add_style("inner");
    text.push_tree(SpanTree::span(
        outer,
        vec![
            SpanTree::text("outer "),
            SpanTree::span(inner, vec![SpanTree::text("inner")]),
        ],
    ));

    let resolved = theme.resolve(&text);
    assert_eq!(resolved.len(), 2);
    assert_eq!(resolved[0].text, "outer ");
    assert_eq!(resolved[0].style.fg, Some(Color::Red));
    assert_eq!(resolved[0].style.bg, Some(Color::DarkBlue));
    assert!(resolved[0].style.bold);
    assert!(!resolved[0].style.italic);

    assert_eq!(resolved[1].text, "inner");
    assert_eq!(resolved[1].style.fg, Some(Color::Green));
    assert_eq!(resolved[1].style.bg, Some(Color::DarkBlue));
    assert!(resolved[1].style.bold);
    assert!(resolved[1].style.italic);
}

/// Ensures the default built-in theme parses and remains free of hard-coded
/// palette assumptions for representative styles that would otherwise be easy
/// to make unreadable on unusual terminal color schemes.
#[test]
fn builtin_default_theme_parses_and_stays_palette_safe() {
    let theme = Theme::builtin();

    let prompt = theme.resolve_style(&StyleName::new("user.prompt"));
    assert!(prompt.bold);
    assert!(prompt.fg.is_none());
    assert!(prompt.bg.is_none());

    let selected = theme.resolve_style(&StyleName::new("completion.selected"));
    assert!(selected.bold);
    assert!(selected.underline);
    assert!(selected.fg.is_none());
    assert!(selected.bg.is_none());

    let tool_err = theme.resolve_style(&StyleName::new("tool.status.error"));
    assert_eq!(tool_err, ThemeStyle::default());
}

/// Ensures the personalized `dpc` built-in theme parses without snapshotting
/// any visual choices, so future theme tuning does not churn test expectations.
#[test]
fn builtin_dpc_theme_parses() {
    let theme = Theme::builtin_dpc();
    let _ = theme.resolve_style(&StyleName::new("user.prompt"));
}

#[test]
fn theme_rejects_unknown_fields() {
    // Theme files are user-authored config. Unknown top-level or style fields
    // should fail fast instead of silently ignoring misspelled keys.
    let error = Theme::parse(
        r#"{
                styles: {
                    prompt: { foreground: "green" },
                }
            }"#,
    )
    .expect_err("unknown style field should fail");

    assert!(error.to_string().contains("unknown field"), "got: {error}");
}

/// Ensures the light built-in theme parses without snapshotting its visual
/// choices, which are intentionally independent from renderer behavior tests.
#[test]
fn builtin_light_theme_parses() {
    let theme = Theme::builtin_light();
    let _ = theme.resolve_style(&StyleName::new("user.prompt"));
}

#[test]
fn builtin_theme_missing_style_is_default() {
    let theme = Theme::builtin();
    let style = theme.resolve_style(&StyleName::new("nonexistent.style"));
    assert_eq!(style, ThemeStyle::default());
}
