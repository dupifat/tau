//! `edit` tool: line-oriented replacements on a file.

use std::fs;
use std::path::{Path, PathBuf};

use tau_proto::{CborValue, ToolUsePayload, ToolUseState, ToolUseStatus};

use crate::argument::{argument_array, argument_text, cbor_map_int, cbor_map_text};
use crate::diff::compute_diff;
use crate::display::{ToolFailure, ToolOutput, text_stats};
use crate::tools::read::{ReadLineRange, format_read_range, slice_line_ranges};
use crate::truncate::truncate_line_oriented;

const MAX_EDITS_PER_CALL: usize = 100;

pub(crate) fn edit_file(arguments: &CborValue) -> Result<ToolOutput, ToolFailure> {
    let path = argument_text(arguments, "path").map_err(ToolFailure::from)?;
    let path_buf = PathBuf::from(&path);
    let display_path = path_buf.display().to_string();
    let mut display_args = display_path.clone();

    let edits = argument_array(arguments, "edits")
        .map_err(|error| with_display_args(&display_args, ToolFailure::from(error)))?;
    if edits.is_empty() {
        return Err(with_display_args(
            &display_args,
            ToolFailure::new("edits array must not be empty"),
        ));
    }
    if MAX_EDITS_PER_CALL < edits.len() {
        return Err(with_display_args(
            &display_args,
            ToolFailure::new(format!(
                "requested edit count exceeds limit of {MAX_EDITS_PER_CALL}"
            )),
        ));
    }

    let (original_bytes, original_missing) = read_original_or_empty(&path_buf, &display_args)?;
    let original_lines = LineIndex::new(&original_bytes);

    let mut replacements = Vec::new();
    let mut requested_ranges = Vec::new();
    for edit in edits {
        let start_line = parse_required_line(edit, "start_line", &display_args)?;
        let line_count = parse_required_line(edit, "line_count", &display_args)?;
        let new_text = cbor_map_text(edit, "newText").ok_or_else(|| {
            with_display_args(
                &display_args,
                ToolFailure::new("each edit must have a string newText"),
            )
        })?;
        requested_ranges.push(format_read_range(Some(start_line), Some(line_count)));
        display_args = edit_display_args(&display_path, &requested_ranges);
        let guard = parse_optional_guard(edit, &display_args)?;

        original_lines.validate_range(start_line, line_count, &display_args)?;
        let end_line = start_line.checked_add(line_count).ok_or_else(|| {
            with_display_args(&display_args, ToolFailure::new("line_count is too large"))
        })?;
        replacements.push(LineReplacement {
            start_line,
            end_line,
            start_byte: original_lines.byte_start_for_line(start_line, original_bytes.len()),
            end_byte: original_lines.byte_start_for_line(end_line, original_bytes.len()),
            new_text: new_text.as_bytes(),
            guard,
        });
    }

    validate_non_overlapping(&replacements, &display_args)?;
    validate_guards(
        &replacements,
        &original_bytes,
        &original_lines,
        &display_args,
    )?;

    let mut result = original_bytes.clone();
    replacements.sort_by_key(|replacement| std::cmp::Reverse(replacement.start_byte));
    for replacement in &replacements {
        result.splice(
            replacement.start_byte..replacement.end_byte,
            replacement.new_text.iter().copied(),
        );
    }

    let changed = original_missing || result != original_bytes;
    if changed {
        create_missing_parent_dirs(&path_buf, &display_args)?;
        fs::write(&path_buf, &result).map_err(|error| {
            with_display_args(&display_args, ToolFailure::from(error.to_string()))
        })?;
    }

    let diff = match (
        std::str::from_utf8(&original_bytes),
        std::str::from_utf8(&result),
    ) {
        (Ok(original), Ok(result)) => Some(compute_diff(original, result)),
        _ => None,
    };

    let display = ToolUseState {
        args: display_args.clone(),
        status: ToolUseStatus::Success,
        status_text: "ok".to_owned(),
        payload: match diff {
            Some(diff) if changed => Some(ToolUsePayload::Diff(diff)),
            None if changed => Some(ToolUsePayload::Text {
                text: "[diff skipped: file is not valid UTF-8]".to_owned(),
            }),
            _ => None,
        },
        ..Default::default()
    };
    let result_lines = LineIndex::new(&result);
    Ok(ToolOutput {
        result: edit_result_value(
            replacements.len(),
            changed,
            result_lines.max_valid_start_line(),
            result.len(),
        ),
        display,
    })
}

struct LineReplacement<'a> {
    start_line: usize,
    end_line: usize,
    start_byte: usize,
    end_byte: usize,
    new_text: &'a [u8],
    guard: Option<&'a str>,
}

struct LineIndex {
    spans: Vec<LineSpan>,
    has_trailing_line_ending: bool,
}

struct LineSpan {
    start: usize,
    content_end: usize,
}

impl LineIndex {
    fn new(input: &[u8]) -> Self {
        let mut spans = Vec::new();
        let mut line_start = 0usize;
        let mut index = 0usize;
        while index < input.len() {
            match input[index] {
                b'\r' => {
                    spans.push(LineSpan {
                        start: line_start,
                        content_end: index,
                    });
                    index += if index + 1 < input.len() && input[index + 1] == b'\n' {
                        2
                    } else {
                        1
                    };
                    line_start = index;
                }
                b'\n' => {
                    spans.push(LineSpan {
                        start: line_start,
                        content_end: index,
                    });
                    index += 1;
                    line_start = index;
                }
                _ => index += 1,
            }
        }

        let has_trailing_line_ending = !input.is_empty() && line_start == input.len();
        if line_start < input.len() {
            spans.push(LineSpan {
                start: line_start,
                content_end: input.len(),
            });
        }

        Self {
            spans,
            has_trailing_line_ending,
        }
    }

    fn max_valid_start_line(&self) -> usize {
        if self.spans.is_empty() {
            return 1;
        }
        if self.has_trailing_line_ending {
            self.spans.len().saturating_add(1)
        } else {
            self.spans.len()
        }
    }

    fn validate_range(
        &self,
        start_line: usize,
        line_count: usize,
        display_args: &str,
    ) -> Result<(), ToolFailure> {
        let max_valid_start_line = self.max_valid_start_line();
        if max_valid_start_line < start_line {
            return Err(with_display_args(
                display_args,
                ToolFailure::new(format!(
                    "start_line {start_line} is past end of file (max_valid_start_line: {max_valid_start_line})"
                )),
            ));
        }
        let end_line = start_line.checked_add(line_count).ok_or_else(|| {
            with_display_args(display_args, ToolFailure::new("line_count is too large"))
        })?;
        let max_end_line = max_valid_start_line.saturating_add(1);
        if max_end_line < end_line {
            return Err(with_display_args(
                display_args,
                ToolFailure::new(format!(
                    "line range starting at {start_line} with count {line_count} exceeds max_valid_start_line {max_valid_start_line}"
                )),
            ));
        }
        Ok(())
    }

    fn byte_start_for_line(&self, line: usize, eof: usize) -> usize {
        self.spans
            .get(line.saturating_sub(1))
            .map(|span| span.start)
            .unwrap_or(eof)
    }

    fn line_content_text<'a>(&self, line: usize, input: &'a [u8]) -> Option<&'a str> {
        let Some(span) = self.spans.get(line.saturating_sub(1)) else {
            return (line <= self.max_valid_start_line()).then_some("");
        };
        std::str::from_utf8(&input[span.start..span.content_end]).ok()
    }
}

fn validate_non_overlapping(
    replacements: &[LineReplacement<'_>],
    display_args: &str,
) -> Result<(), ToolFailure> {
    let mut ranges: Vec<_> = replacements.iter().collect();
    ranges.sort_by_key(|replacement| replacement.start_line);
    for pair in ranges.windows(2) {
        if pair[1].start_line < pair[0].end_line {
            return Err(with_display_args(
                display_args,
                ToolFailure::new("overlapping edits"),
            ));
        }
    }
    Ok(())
}

fn validate_guards(
    replacements: &[LineReplacement<'_>],
    original_bytes: &[u8],
    original_lines: &LineIndex,
    display_args: &str,
) -> Result<(), ToolFailure> {
    for replacement in replacements {
        let Some(guard) = replacement.guard else {
            continue;
        };
        if original_lines.line_content_text(replacement.start_line, original_bytes) == Some(guard) {
            continue;
        }
        return Err(guard_mismatch_failure(
            replacements,
            original_bytes,
            display_args,
            replacement.start_line,
        ));
    }
    Ok(())
}

fn guard_mismatch_failure(
    replacements: &[LineReplacement<'_>],
    original_bytes: &[u8],
    display_args: &str,
    start_line: usize,
) -> ToolFailure {
    let ranges = replacements
        .iter()
        .map(|replacement| ReadLineRange {
            start_line: replacement.start_line,
            line_count: Some(replacement.end_line.saturating_sub(replacement.start_line)),
        })
        .collect::<Vec<_>>();
    let rendered = slice_line_ranges(original_bytes, &ranges);
    let truncated = truncate_line_oriented(&rendered.content);
    let mut details = vec![
        (
            CborValue::Text("line-numbered content".to_owned()),
            CborValue::Text(truncated.content.clone()),
        ),
        (
            CborValue::Text("guard_start_line".to_owned()),
            CborValue::Integer((start_line as i64).into()),
        ),
    ];
    if !rendered.valid_utf8 {
        details.push((
            CborValue::Text("valid_utf8".to_owned()),
            CborValue::Bool(false),
        ));
    }
    if truncated.was_truncated {
        details.push((
            CborValue::Text("truncated".to_owned()),
            CborValue::Bool(true),
        ));
    }
    if truncated.was_truncated || truncated.content.is_empty() {
        details.push((
            CborValue::Text("total_lines".to_owned()),
            CborValue::Integer((rendered.total_lines as i64).into()),
        ));
        details.push((
            CborValue::Text("total_bytes".to_owned()),
            CborValue::Integer((original_bytes.len() as i64).into()),
        ));
    }

    let mut failure = ToolFailure::new(format!("guard for line {start_line} did not match"))
        .with_args(display_args.to_owned())
        .with_details(CborValue::Map(details))
        .with_payload(Some(ToolUsePayload::Text {
            text: truncated.content.clone(),
        }));
    failure.display.stats = text_stats(&truncated.content);
    failure
}

fn read_original_or_empty(path: &Path, display_args: &str) -> Result<(Vec<u8>, bool), ToolFailure> {
    match fs::read(path) {
        Ok(bytes) => Ok((bytes, false)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok((Vec::new(), true)),
        Err(error) => Err(with_display_args(
            display_args,
            ToolFailure::from(error.to_string()),
        )),
    }
}

fn create_missing_parent_dirs(path: &Path, display_args: &str) -> Result<(), ToolFailure> {
    let Some(parent) = path.parent() else {
        return Ok(());
    };
    if parent.as_os_str().is_empty() || parent.exists() {
        return Ok(());
    }
    fs::create_dir_all(parent)
        .map_err(|error| with_display_args(display_args, ToolFailure::from(error.to_string())))
}

fn parse_required_line(
    edit: &CborValue,
    key: &str,
    display_args: &str,
) -> Result<usize, ToolFailure> {
    match cbor_map_int(edit, key) {
        Some(n) if n < 1 => Err(with_display_args(
            display_args,
            ToolFailure::new(format!("{key} must be at least 1")),
        )),
        Some(n) => usize::try_from(n).map_err(|_| {
            with_display_args(
                display_args,
                ToolFailure::new(format!("{key} is too large")),
            )
        }),
        None => Err(with_display_args(
            display_args,
            ToolFailure::new(format!("each edit must have an integer {key}")),
        )),
    }
}

fn parse_optional_guard<'a>(
    edit: &'a CborValue,
    display_args: &str,
) -> Result<Option<&'a str>, ToolFailure> {
    let CborValue::Map(entries) = edit else {
        return Ok(None);
    };
    for (key, value) in entries {
        if let CborValue::Text(key) = key
            && key == "guard"
        {
            return match value {
                CborValue::Text(value) if value.contains('\n') || value.contains('\r') => {
                    Err(with_display_args(
                        display_args,
                        ToolFailure::new("guard must not include newline characters"),
                    ))
                }
                CborValue::Text(value) => Ok(Some(value.as_str())),
                _ => Err(with_display_args(
                    display_args,
                    ToolFailure::new("guard must be a string when provided"),
                )),
            };
        }
    }
    Ok(None)
}

fn with_display_args(args: &str, failure: ToolFailure) -> ToolFailure {
    failure.with_args(args.to_owned())
}

fn edit_display_args(path: &str, ranges: &[String]) -> String {
    if ranges.is_empty() {
        return path.to_owned();
    }

    let mut unique_ranges: Vec<&str> = Vec::new();
    for range in ranges {
        if unique_ranges
            .iter()
            .all(|existing| *existing != range.as_str())
        {
            unique_ranges.push(range.as_str());
        }
    }
    format!("{path} {}", unique_ranges.join(","))
}

fn edit_result_value(
    replacements: usize,
    changed: bool,
    new_max_valid_start_line: usize,
    total_bytes: usize,
) -> CborValue {
    CborValue::Map(vec![
        (
            CborValue::Text("replacements".to_owned()),
            CborValue::Integer((replacements as i64).into()),
        ),
        (
            CborValue::Text("changed".to_owned()),
            CborValue::Bool(changed),
        ),
        (
            CborValue::Text("new_max_valid_start_line".to_owned()),
            CborValue::Integer((new_max_valid_start_line as i64).into()),
        ),
        (
            CborValue::Text("total_bytes".to_owned()),
            CborValue::Integer((total_bytes as i64).into()),
        ),
    ])
}
