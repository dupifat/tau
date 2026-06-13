//! `find` tool: glob-based file search rooted at a directory.

use std::fs;
use std::path::{Path, PathBuf};

use globset::{Glob, GlobSet, GlobSetBuilder};
use ignore::WalkBuilder;
use tau_proto::CborValue;

use crate::argument::{argument_text, optional_argument_int_strict, optional_argument_text};
use crate::display::{ToolFailure, ToolOutput, text_stats};
use crate::truncate::{MAX_OUTPUT_BYTES, MAX_OUTPUT_LINES, truncate_line_oriented};

pub(crate) const DEFAULT_FIND_LIMIT: usize = 1000;
const MAX_FIND_LIMIT: usize = MAX_OUTPUT_LINES;

pub(crate) fn run_find(arguments: &CborValue) -> Result<ToolOutput, ToolFailure> {
    let pattern = argument_text(arguments, "pattern").map_err(ToolFailure::from)?;
    let path = optional_argument_text(arguments, "path").unwrap_or_else(|| ".".to_owned());
    let limit = match optional_argument_int_strict(arguments, "limit").map_err(ToolFailure::from)? {
        Some(value) if value < 1 => return Err(ToolFailure::new("limit must be >= 1")),
        Some(value) => {
            let limit =
                usize::try_from(value).map_err(|_| ToolFailure::new("limit is too large"))?;
            if MAX_FIND_LIMIT < limit {
                return Err(ToolFailure::new(format!(
                    "limit must be <= {MAX_FIND_LIMIT}"
                )));
            }
            limit
        }
        None => DEFAULT_FIND_LIMIT,
    };
    let search_path = PathBuf::from(&path);
    let display_args = format!("{pattern} in {}", search_path.display());
    let with_args = |f: ToolFailure| f.with_args(display_args.clone());

    let metadata = fs::metadata(&search_path).map_err(|e| {
        with_args(ToolFailure::from(format!(
            "failed to access {}: {e}",
            search_path.display()
        )))
    })?;
    if !metadata.is_dir() {
        return Err(with_args(ToolFailure::from(format!(
            "not a directory: {}",
            search_path.display()
        ))));
    }

    let glob = compile_find_glob(&pattern).map_err(|e| with_args(ToolFailure::from(e)))?;
    let mut matches = Vec::new();
    let collection_cap = limit.saturating_add(1);
    for entry in WalkBuilder::new(&search_path)
        .hidden(false)
        .parents(true)
        .ignore(true)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .build()
    {
        let entry = entry.map_err(|e| {
            with_args(ToolFailure::from(format!(
                "failed to walk {}: {e}",
                search_path.display()
            )))
        })?;
        let file_type = match entry.file_type() {
            Some(file_type) => file_type,
            None => continue,
        };
        if !file_type.is_file() {
            continue;
        }

        let Ok(relative_path) = entry.path().strip_prefix(&search_path) else {
            continue;
        };
        if glob.is_match(relative_path) {
            matches.push(path_to_slash(relative_path));
            if collection_cap <= matches.len() {
                break;
            }
        }
    }
    matches.sort_by_key(|entry| entry.to_lowercase());

    if matches.is_empty() {
        let mut display = crate::display::ok_display(display_args.clone());
        display.stats.matches = Some(0);
        return Ok(ToolOutput {
            result: CborValue::Map(vec![
                (
                    CborValue::Text("matches".to_owned()),
                    CborValue::Integer(0.into()),
                ),
                (
                    CborValue::Text("output".to_owned()),
                    CborValue::Text("no files found matching pattern".to_owned()),
                ),
            ]),
            display,
        });
    }

    let observed_matches = matches.len();
    let displayed: Vec<String> = matches.into_iter().take(limit).collect();
    let limit_reached = observed_matches > displayed.len();
    let mut output_text = displayed.join("\n");
    let truncated = truncate_line_oriented(&output_text);
    if truncated.was_truncated {
        output_text = truncated.content;
    }

    let mut notices = Vec::new();
    if limit_reached {
        notices.push(limit_reached_notice(limit));
    }
    if truncated.was_truncated {
        notices.push("50KB/2000 line output limit reached.".to_owned());
    }

    output_text = append_notices_within_cap(output_text, &notices);

    let mut display = crate::display::ok_display(display_args);
    display.stats = text_stats(&output_text);
    let mut result_entries = vec![
        (
            CborValue::Text("matches".to_owned()),
            CborValue::Integer((displayed.len() as i64).into()),
        ),
        (
            CborValue::Text("output".to_owned()),
            CborValue::Text(output_text),
        ),
    ];
    if limit_reached {
        result_entries.push((
            CborValue::Text("limit_reached".to_owned()),
            CborValue::Bool(true),
        ));
    }
    Ok(ToolOutput {
        result: CborValue::Map(result_entries),
        display,
    })
}

fn limit_reached_notice(limit: usize) -> String {
    if limit >= MAX_FIND_LIMIT {
        format!("{limit} results limit reached. Maximum limit reached; refine pattern/path.")
    } else {
        format!(
            "{limit} results limit reached. Use limit={} for more, or refine pattern.",
            (limit * 2).min(MAX_FIND_LIMIT)
        )
    }
}

fn append_notices_within_cap(mut output_text: String, notices: &[String]) -> String {
    if notices.is_empty() {
        return output_text;
    }
    let notice = format!("\n\n[{}]", notices.join(" "));
    if output_text.len().saturating_add(notice.len()) <= MAX_OUTPUT_BYTES {
        output_text.push_str(&notice);
        return output_text;
    }
    let Some(budget) = MAX_OUTPUT_BYTES.checked_sub(notice.len()) else {
        return notice.chars().take(MAX_OUTPUT_BYTES).collect();
    };
    let mut end = budget.min(output_text.len());
    while !output_text.is_char_boundary(end) {
        end -= 1;
    }
    output_text.truncate(end);
    output_text.push_str(&notice);
    output_text
}

fn compile_find_glob(pattern: &str) -> Result<GlobSet, String> {
    let glob = Glob::new(pattern).map_err(|e| format!("invalid glob pattern {pattern:?}: {e}"))?;
    let mut builder = GlobSetBuilder::new();
    builder.add(glob);
    builder
        .build()
        .map_err(|e| format!("failed to compile glob pattern {pattern:?}: {e}"))
}

fn path_to_slash(path: &Path) -> String {
    render_path(path)
}

fn render_path(path: &Path) -> String {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        return render_path_bytes(path.as_os_str().as_bytes());
    }
    #[cfg(not(unix))]
    {
        escape_path_text(&path.to_string_lossy())
    }
}

pub(crate) fn render_path_bytes(bytes: &[u8]) -> String {
    match std::str::from_utf8(bytes) {
        Ok(text) => escape_path_text(text),
        Err(_) => format!(
            "(invalid-utf8) {}",
            escape_path_text(&String::from_utf8_lossy(bytes))
        ),
    }
}

pub(crate) fn escape_path_text(text: &str) -> String {
    let mut escaped = String::new();
    for ch in text.chars() {
        match ch {
            '\\' => escaped.push_str("\\\\"),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            ch if ch.is_control() => escaped.extend(ch.escape_default()),
            ch => escaped.push(ch),
        }
    }
    escaped
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(limit: CborValue) -> CborValue {
        CborValue::Map(vec![
            (
                CborValue::Text("pattern".to_owned()),
                CborValue::Text("*".to_owned()),
            ),
            (CborValue::Text("limit".to_owned()), limit),
        ])
    }

    /// Ensures find rejects wrong-typed limits instead of silently using the
    /// default result cap.
    #[test]
    fn find_rejects_wrong_type_limit() {
        let err = run_find(&args(CborValue::Text("10".to_owned())))
            .expect_err("string limit should be rejected");

        assert_eq!(err.message, "argument `limit` must be an integer");
    }

    /// Ensures find rejects non-positive limits instead of coercing them to a
    /// surprising positive default.
    #[test]
    fn find_rejects_non_positive_limit() {
        let err = run_find(&args(CborValue::Integer(0.into())))
            .expect_err("zero limit should be rejected");

        assert_eq!(err.message, "limit must be >= 1");
    }

    /// Ensures find stops after collecting one match past the requested limit,
    /// bounding memory/traversal work while still adding the user-visible limit
    /// notice.
    #[test]
    fn find_limit_bounds_collected_matches() {
        let tempdir = tempfile::TempDir::new().expect("tempdir");
        for name in ["alpha.txt", "beta.txt", "gamma.txt"] {
            std::fs::write(tempdir.path().join(name), "x").expect("write file");
        }
        let args = CborValue::Map(vec![
            (
                CborValue::Text("pattern".to_owned()),
                CborValue::Text("*.txt".to_owned()),
            ),
            (
                CborValue::Text("path".to_owned()),
                CborValue::Text(tempdir.path().display().to_string()),
            ),
            (
                CborValue::Text("limit".to_owned()),
                CborValue::Integer(1.into()),
            ),
        ]);

        let result = run_find(&args).expect("find").result;
        let CborValue::Map(entries) = result else {
            panic!("expected result map");
        };
        let output = entries
            .iter()
            .find_map(|(key, value)| match (key, value) {
                (CborValue::Text(key), CborValue::Text(value)) if key == "output" => Some(value),
                _ => None,
            })
            .expect("output");
        let matches: i64 = entries
            .iter()
            .find_map(|(key, value)| match (key, value) {
                (CborValue::Text(key), CborValue::Integer(value)) if key == "matches" => {
                    i128::from(*value).try_into().ok()
                }
                _ => None,
            })
            .expect("matches");
        let limit_reached = entries.iter().any(|(key, value)| {
            matches!(
                (key, value),
                (CborValue::Text(key), CborValue::Bool(true)) if key == "limit_reached"
            )
        });

        assert_eq!(
            output.lines().take_while(|line| !line.is_empty()).count(),
            1
        );
        assert_eq!(matches, 1);
        assert!(limit_reached);
        assert!(output.contains("1 results limit reached"));
    }

    /// Ensures large caller limits cannot force collection far beyond the
    /// documented display cap before final output truncation.
    #[test]
    fn find_rejects_limit_above_output_cap() {
        let err = run_find(&args(CborValue::Integer(
            (MAX_FIND_LIMIT as i64 + 1).into(),
        )))
        .expect_err("limit over cap");

        assert_eq!(err.message, format!("limit must be <= {MAX_FIND_LIMIT}"));
    }

    /// Ensures max-limit notices do not suggest limits that argument parsing
    /// rejects.
    #[test]
    fn find_max_limit_notice_asks_to_refine() {
        let notice = limit_reached_notice(MAX_FIND_LIMIT);

        assert!(notice.contains("Maximum limit reached"));
        assert!(!notice.contains(&format!("limit={}", MAX_FIND_LIMIT * 2)));
    }

    /// Protects find output from path line injection by escaping control
    /// characters before rendering file names as one logical record per line.
    #[test]
    fn find_escapes_control_characters_in_paths() {
        let tempdir = tempfile::TempDir::new().expect("tempdir");
        std::fs::write(tempdir.path().join("line\nbreak.txt"), "x").expect("write file");
        let args = CborValue::Map(vec![
            (
                CborValue::Text("pattern".to_owned()),
                CborValue::Text("*.txt".to_owned()),
            ),
            (
                CborValue::Text("path".to_owned()),
                CborValue::Text(tempdir.path().display().to_string()),
            ),
        ]);

        let result = run_find(&args).expect("find").result;
        let CborValue::Map(entries) = result else {
            panic!("expected result map");
        };
        let output = entries
            .iter()
            .find_map(|(key, value)| match (key, value) {
                (CborValue::Text(key), CborValue::Text(value)) if key == "output" => Some(value),
                _ => None,
            })
            .expect("output");

        assert_eq!(output, "line\\nbreak.txt");
    }
    /// Ensures find notices are included without exceeding the documented 50KB
    /// output budget.
    #[test]
    fn find_notices_stay_within_output_cap() {
        let notice = "50KB/2000 line output limit reached.".to_owned();
        let suffix_len = format!("\n\n[{notice}]").len();
        let output = append_notices_within_cap(
            format!("{}étail", "x".repeat(MAX_OUTPUT_BYTES - suffix_len - 1)),
            &[notice.clone()],
        );

        assert!(output.len() <= MAX_OUTPUT_BYTES);
        assert!(output.contains(&notice));
    }
}
