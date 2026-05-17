//! `edit` tool: targeted exact-string replacements on a file.

use std::fs;
use std::path::PathBuf;

use tau_proto::{CborValue, ToolDisplay, ToolDisplayPayload, ToolDisplayStatus};

use crate::argument::{argument_array, argument_text, cbor_map_int, cbor_map_text};
use crate::diff::{compute_diff, unified_diff};
use crate::display::{ToolFailure, ToolOutput};
use crate::tools::read::format_read_range;

pub(crate) fn edit_file(arguments: &CborValue) -> Result<ToolOutput, ToolFailure> {
    let path = argument_text(arguments, "path").map_err(ToolFailure::from)?;
    let path_buf = PathBuf::from(&path);
    let display_path = path_buf.display().to_string();
    let mut display_args = display_path.clone();

    let original_bytes = fs::read(&path_buf)
        .map_err(|e| with_display_args(&display_args, ToolFailure::from(e.to_string())))?;

    let edits = argument_array(arguments, "edits")
        .map_err(|e| with_display_args(&display_args, ToolFailure::from(e)))?;
    if edits.is_empty() {
        return Err(with_display_args(
            &display_args,
            ToolFailure::new("edits array must not be empty"),
        ));
    }

    let line_starts = line_starts(&original_bytes);

    // Collect all replacements and validate against the original.
    let mut replacements: Vec<(usize, usize, &[u8])> = Vec::new();
    let mut requested_ranges = Vec::new();
    for edit in edits {
        let old_text = cbor_map_text(edit, "oldText").ok_or_else(|| {
            with_display_args(
                &display_args,
                ToolFailure::new("each edit must have a string oldText"),
            )
        })?;
        let new_text = cbor_map_text(edit, "newText").ok_or_else(|| {
            with_display_args(
                &display_args,
                ToolFailure::new("each edit must have a string newText"),
            )
        })?;
        let max_matches = parse_optional_count(edit, "max_matches", 1, &display_args)?;
        let start_line = parse_optional_line(edit, "start_line", 1, &display_args)?;
        let end_line =
            parse_optional_line_count(edit, start_line, line_starts.len() + 1, &display_args)?;
        requested_ranges.push(format_read_range(
            cbor_map_int(edit, "start_line").map(|_| start_line),
            cbor_map_int(edit, "line_count").and_then(|count| usize::try_from(count).ok()),
        ));
        display_args = edit_display_args(&display_path, &requested_ranges);

        if old_text.is_empty() {
            return Err(with_display_args(
                &display_args,
                ToolFailure::new("oldText must not be empty"),
            ));
        }

        let old_bytes = old_text.as_bytes();
        let start_byte = byte_offset_for_line(&line_starts, start_line, original_bytes.len());
        let end_byte = byte_offset_for_line(&line_starts, end_line, original_bytes.len());
        let replacements_before = replacements.len();
        for start in find_subslice_matches(&original_bytes[start_byte..end_byte], old_bytes)
            .take(max_matches)
        {
            let start = start_byte + start;
            let end = start + old_bytes.len();
            replacements.push((start, end, new_text.as_bytes()));
        }
        if replacements.len() == replacements_before {
            return Err(with_display_args(
                &display_args,
                ToolFailure::new("no matches for edit"),
            ));
        }
    }

    if replacements.is_empty() {
        return Err(with_display_args(
            &display_args,
            ToolFailure::new("no matches for edit"),
        ));
    }

    // Sort by start position (descending) so we can apply from end to start
    // without invalidating earlier offsets.
    replacements.sort_by_key(|entry| std::cmp::Reverse(entry.0));

    // Check for overlapping ranges.
    for pair in replacements.windows(2) {
        // After descending sort: pair[0].start is later in the file.
        // Overlap if pair[1].end is after pair[0].start.
        if pair[0].0 < pair[1].1 {
            return Err(with_display_args(
                &display_args,
                ToolFailure::new("overlapping edits"),
            ));
        }
    }

    // Apply replacements from end to start.
    let mut result = original_bytes.clone();
    for (start, end, new_text) in &replacements {
        result.splice(*start..*end, new_text.iter().copied());
    }

    if result != original_bytes {
        fs::write(&path_buf, &result)
            .map_err(|e| with_display_args(&display_args, ToolFailure::from(e.to_string())))?;
    }

    let diff = match (
        std::str::from_utf8(&original_bytes),
        std::str::from_utf8(&result),
    ) {
        (Ok(original), Ok(result)) => Some(compute_diff(original, result)),
        _ => None,
    };
    let changed = result != original_bytes;

    let display = ToolDisplay {
        args: display_args.clone(),
        status: ToolDisplayStatus::Success,
        status_text: "ok".to_owned(),
        payload: match diff.clone() {
            Some(diff) => Some(ToolDisplayPayload::Diff(diff)),
            None if changed => Some(ToolDisplayPayload::Text {
                text: "[diff skipped: file is not valid UTF-8]".to_owned(),
            }),
            None => None,
        },
        ..Default::default()
    };
    Ok(ToolOutput {
        result: edit_result_value(display_path, replacements.len(), changed, diff.as_ref()),
        display,
    })
}

fn parse_optional_count(
    edit: &CborValue,
    key: &str,
    default: usize,
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
        None => Ok(default),
    }
}

fn parse_optional_line(
    edit: &CborValue,
    key: &str,
    default: usize,
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
        None => Ok(default),
    }
}

fn parse_optional_line_count(
    edit: &CborValue,
    start_line: usize,
    default_end_line: usize,
    display_args: &str,
) -> Result<usize, ToolFailure> {
    match cbor_map_int(edit, "line_count") {
        Some(n) if n < 1 => Err(with_display_args(
            display_args,
            ToolFailure::new("line_count must be at least 1"),
        )),
        Some(n) => {
            let count = usize::try_from(n).map_err(|_| {
                with_display_args(display_args, ToolFailure::new("line_count is too large"))
            })?;
            start_line.checked_add(count).ok_or_else(|| {
                with_display_args(display_args, ToolFailure::new("line_count is too large"))
            })
        }
        None => Ok(default_end_line),
    }
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

fn line_starts(input: &[u8]) -> Vec<usize> {
    let mut starts = vec![0];
    for (index, byte) in input.iter().copied().enumerate() {
        if byte == b'\n' && index + 1 < input.len() {
            starts.push(index + 1);
        }
    }
    starts
}

fn find_subslice_matches<'a>(
    haystack: &'a [u8],
    needle: &'a [u8],
) -> impl Iterator<Item = usize> + 'a {
    haystack
        .windows(needle.len())
        .enumerate()
        .filter_map(move |(index, window)| (window == needle).then_some(index))
}

fn byte_offset_for_line(line_starts: &[usize], line: usize, eof: usize) -> usize {
    line_starts
        .get(line.saturating_sub(1))
        .copied()
        .unwrap_or(eof)
}

fn edit_result_value(
    path: String,
    replacements: usize,
    changed: bool,
    diff: Option<&tau_proto::DiffSummary>,
) -> CborValue {
    let mut entries = vec![
        (CborValue::Text("path".to_owned()), CborValue::Text(path)),
        (
            CborValue::Text("replacements".to_owned()),
            CborValue::Integer((replacements as i64).into()),
        ),
        (
            CborValue::Text("changed".to_owned()),
            CborValue::Bool(changed),
        ),
    ];
    if let Some(diff) = diff.and_then(unified_diff) {
        entries.push((CborValue::Text("diff".to_owned()), CborValue::Text(diff)));
    } else if changed {
        entries.push((
            CborValue::Text("diff".to_owned()),
            CborValue::Text("[diff skipped: file is not valid UTF-8]".to_owned()),
        ));
    }
    CborValue::Map(entries)
}
