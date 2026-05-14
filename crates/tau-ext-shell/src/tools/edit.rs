//! `edit` tool: targeted exact-string replacements on a file.

use std::fs;
use std::path::PathBuf;

use tau_proto::{CborValue, ToolDisplay, ToolDisplayPayload, ToolDisplayStatus};

use crate::argument::{argument_array, argument_text, cbor_map_int, cbor_map_text};
use crate::diff::{compute_diff, encode_diff};
use crate::display::{ToolFailure, ToolOutput};

pub(crate) fn edit_file(arguments: &CborValue) -> Result<ToolOutput, ToolFailure> {
    let path = argument_text(arguments, "path").map_err(ToolFailure::from)?;
    let path_buf = PathBuf::from(&path);
    let display_args = path_buf.display().to_string();
    let with_args = |f: ToolFailure| f.with_args(display_args.clone());

    let original =
        fs::read_to_string(&path_buf).map_err(|e| with_args(ToolFailure::from(e.to_string())))?;

    let edits = argument_array(arguments, "edits").map_err(|e| with_args(ToolFailure::from(e)))?;
    if edits.is_empty() {
        return Err(with_args(ToolFailure::new("edits array must not be empty")));
    }

    // Collect all replacements and validate against the original.
    let mut replacements: Vec<(usize, usize, &str)> = Vec::new();
    for edit in edits {
        let old_text = cbor_map_text(edit, "oldText")
            .ok_or_else(|| with_args(ToolFailure::new("each edit must have a string oldText")))?;
        let new_text = cbor_map_text(edit, "newText")
            .ok_or_else(|| with_args(ToolFailure::new("each edit must have a string newText")))?;
        let expected_matches = match cbor_map_int(edit, "expected_matches") {
            Some(n) if n < 0 => {
                return Err(with_args(ToolFailure::new(
                    "expected_matches must not be negative",
                )));
            }
            Some(n) => usize::try_from(n)
                .map_err(|_| with_args(ToolFailure::new("expected_matches is too large")))?,
            None => 1,
        };

        if old_text.is_empty() {
            return Err(with_args(ToolFailure::new("oldText must not be empty")));
        }

        let matches: Vec<(usize, &str)> = original.match_indices(old_text).collect();
        let actual_matches = matches.len();
        if actual_matches != expected_matches {
            return Err(match_count_failure(
                display_args.clone(),
                old_text,
                expected_matches,
                actual_matches,
            ));
        }

        for (start, matched) in matches {
            let end = start + matched.len();
            replacements.push((start, end, new_text));
        }
    }

    // Sort by start position (descending) so we can apply from end to start
    // without invalidating earlier offsets.
    replacements.sort_by_key(|entry| std::cmp::Reverse(entry.0));

    // Check for overlapping ranges.
    for pair in replacements.windows(2) {
        // After descending sort: pair[0].start >= pair[1].start.
        // Overlap if pair[1].end > pair[0].start (pair[1] is earlier in file).
        if pair[1].1 > pair[0].0 {
            return Err(with_args(ToolFailure::new("overlapping edits")));
        }
    }

    // Apply replacements from end to start.
    let mut result = original.clone();
    for (start, end, new_text) in &replacements {
        result.replace_range(*start..*end, new_text);
    }

    fs::write(&path_buf, &result).map_err(|e| with_args(ToolFailure::from(e.to_string())))?;

    let diff = compute_diff(&original, &result);

    let display = ToolDisplay {
        args: display_args.clone(),
        status: ToolDisplayStatus::Success,
        status_text: "ok".to_owned(),
        payload: Some(ToolDisplayPayload::Diff(diff.clone())),
        ..Default::default()
    };
    Ok(ToolOutput {
        result: CborValue::Map(vec![
            (
                CborValue::Text("path".to_owned()),
                CborValue::Text(display_args),
            ),
            (
                CborValue::Text("edits_applied".to_owned()),
                CborValue::Integer((replacements.len() as i64).into()),
            ),
            (CborValue::Text("diff".to_owned()), encode_diff(&diff)),
        ]),
        display,
    })
}

fn match_count_failure(
    path: String,
    old_text: &str,
    expected_matches: usize,
    actual_matches: usize,
) -> ToolFailure {
    ToolFailure::new(format!(
        "oldText match count mismatch: expected {expected_matches}, found {actual_matches}; no changes written"
    ))
    .with_args(path.clone())
    .with_details(CborValue::Map(vec![
        (CborValue::Text("path".to_owned()), CborValue::Text(path)),
        (
            CborValue::Text("oldText".to_owned()),
            CborValue::Text(old_text.to_owned()),
        ),
        (
            CborValue::Text("expected_matches".to_owned()),
            CborValue::Integer((expected_matches as i64).into()),
        ),
        (
            CborValue::Text("actual_matches".to_owned()),
            CborValue::Integer((actual_matches as i64).into()),
        ),
    ]))
}
