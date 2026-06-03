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
const GUARD_MISMATCH_CONTEXT_LINES: usize = 10;

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
        reject_legacy_line_count(edit, &display_args)?;
        let start_line = parse_required_line(edit, "start_line", &display_args)?;
        let end_line = parse_required_line(edit, "end_line", &display_args)?;
        let new_text = cbor_map_text(edit, "newText").ok_or_else(|| {
            with_display_args(
                &display_args,
                ToolFailure::new("each edit must have a string newText"),
            )
        })?;
        requested_ranges.push(format_read_range(Some(start_line), Some(end_line)));
        display_args = edit_display_args(&display_path, &requested_ranges);
        let guard = parse_required_guard(edit, &display_args)?;
        original_lines.validate_range(start_line, end_line, &display_args)?;
        let end_line_exclusive = end_line.checked_add(1).ok_or_else(|| {
            with_display_args(&display_args, ToolFailure::new("end_line is too large"))
        })?;
        let start_byte = original_lines.byte_start_for_line(start_line, original_bytes.len());
        let end_byte = original_lines.byte_start_for_line(end_line_exclusive, original_bytes.len());
        let mut new_text = new_text.as_bytes().to_vec();
        let newline_added = maybe_add_line_ending(
            &mut new_text,
            &original_bytes,
            &original_lines,
            end_line,
            end_byte,
        );
        replacements.push(LineReplacement {
            start_line,
            end_line_exclusive,
            start_byte,
            end_byte,
            new_text,
            guard,
            newline_added,
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
            replacements
                .iter()
                .any(|replacement| replacement.newline_added),
        ),
        display,
    })
}

struct LineReplacement<'a> {
    start_line: usize,
    end_line_exclusive: usize,
    start_byte: usize,
    end_byte: usize,
    new_text: Vec<u8>,
    guard: &'a str,
    newline_added: bool,
}

fn maybe_add_line_ending(
    new_text: &mut Vec<u8>,
    original_bytes: &[u8],
    original_lines: &LineIndex,
    end_line: usize,
    end_byte: usize,
) -> bool {
    if new_text.is_empty()
        || original_bytes.len() <= end_byte
        || new_text.ends_with(b"\n")
        || new_text.ends_with(b"\r")
    {
        return false;
    }

    let Some(line_ending) = original_lines.line_ending_for_line(end_line, original_bytes) else {
        return false;
    };
    new_text.extend_from_slice(line_ending);
    true
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

    fn line_ending_for_line<'a>(&self, line: usize, input: &'a [u8]) -> Option<&'a [u8]> {
        let span = self.spans.get(line.checked_sub(1)?)?;
        let next_start = self
            .spans
            .get(line)
            .map_or(input.len(), |next_span| next_span.start);
        if span.content_end == next_start {
            return None;
        }
        Some(&input[span.content_end..next_start])
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
        end_line: usize,
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
        if end_line < start_line {
            return Err(with_display_args(
                display_args,
                ToolFailure::new("end_line must be at least start_line"),
            ));
        }
        if max_valid_start_line < end_line {
            return Err(with_display_args(
                display_args,
                ToolFailure::new(format!(
                    "line range {start_line}..{end_line} exceeds max_valid_start_line {max_valid_start_line}"
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
        if pair[1].start_line < pair[0].end_line_exclusive {
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
        let guard = replacement.guard;
        if original_lines.line_content_text(replacement.start_line, original_bytes) == Some(guard) {
            continue;
        }
        return Err(guard_mismatch_failure(
            replacement,
            original_bytes,
            display_args,
        ));
    }
    Ok(())
}

fn guard_mismatch_failure(
    replacement: &LineReplacement<'_>,
    original_bytes: &[u8],
    display_args: &str,
) -> ToolFailure {
    let start_line = replacement.start_line;
    let context_start_line = start_line
        .saturating_sub(GUARD_MISMATCH_CONTEXT_LINES)
        .max(1);
    let context_end_line = start_line.saturating_add(GUARD_MISMATCH_CONTEXT_LINES);
    let ranges = vec![ReadLineRange {
        start_line: context_start_line,
        end_line: Some(context_end_line),
    }];
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
        .with_details(CborValue::Map(details));
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

fn reject_legacy_line_count(edit: &CborValue, display_args: &str) -> Result<(), ToolFailure> {
    let CborValue::Map(entries) = edit else {
        return Ok(());
    };
    if entries
        .iter()
        .any(|(key, _)| matches!(key, CborValue::Text(key) if key == "line_count"))
    {
        return Err(with_display_args(
            display_args,
            ToolFailure::new("line_count is no longer supported; use end_line"),
        ));
    }
    Ok(())
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

fn parse_required_guard<'a>(
    edit: &'a CborValue,
    display_args: &str,
) -> Result<&'a str, ToolFailure> {
    let CborValue::Map(entries) = edit else {
        return Err(with_display_args(
            display_args,
            ToolFailure::new("each edit must have a string guard"),
        ));
    };
    for (key, value) in entries {
        if let CborValue::Text(key) = key
            && key == "guard"
        {
            return match value {
                CborValue::Text(value) => {
                    let value = value.trim_end_matches(['\n', '\r']);
                    if value.contains('\n') || value.contains('\r') {
                        return Err(with_display_args(
                            display_args,
                            ToolFailure::new("guard must not include embedded newline characters"),
                        ));
                    }
                    Ok(value)
                }
                _ => Err(with_display_args(
                    display_args,
                    ToolFailure::new("guard must be a string"),
                )),
            };
        }
    }
    Err(with_display_args(
        display_args,
        ToolFailure::new("each edit must have a string guard"),
    ))
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
    newline_added: bool,
) -> CborValue {
    let mut fields = vec![
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
    ];
    if newline_added {
        fields.push((
            CborValue::Text("newline_added".to_owned()),
            CborValue::Bool(true),
        ));
    }
    CborValue::Map(fields)
}
