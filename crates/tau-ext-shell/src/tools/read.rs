//! `read` tool: read a file (optionally a line slice).

use std::fs;
use std::path::{Path, PathBuf};

use tau_proto::CborValue;

use crate::argument::{argument_text, cbor_map_int, optional_argument_int};
use crate::display::{ToolFailure, ToolOutput, ok_display, text_stats};
use crate::truncate::truncate_line_oriented;

const MAX_READ_RANGES_PER_CALL: usize = 100;

pub(crate) fn read_file(arguments: &CborValue) -> Result<ToolOutput, ToolFailure> {
    let path = argument_text(arguments, "path").map_err(ToolFailure::from)?;
    let request = parse_read_request(arguments)?;
    let path_buf = PathBuf::from(&path);
    let display_path = path_buf.display().to_string();
    let display_args = format!("{} {}", display_path, request.display_ranges.join(","));

    let file_bytes = fs::metadata(&path_buf)
        .map_err(|error| ToolFailure::from(error.to_string()).with_args(display_args.clone()))?
        .len() as usize;
    let sliced = stream_slice_line_ranges(&path_buf, &request.ranges)
        .map_err(|error| ToolFailure::from(error.to_string()).with_args(display_args.clone()))?;
    validate_ranges_with_total(&request.ranges, sliced.total_lines, &display_args)?;
    let total_lines = sliced.total_lines;
    let truncated = truncate_line_oriented(&sliced.content);
    let content_value = CborValue::Text(truncated.content.clone());
    let mut entries = vec![(
        CborValue::Text("line-numbered content".to_owned()),
        content_value,
    )];
    if !sliced.valid_utf8 {
        entries.push((
            CborValue::Text("valid_utf8".to_owned()),
            CborValue::Bool(false),
        ));
    }
    if truncated.was_truncated || total_lines == 0 {
        if truncated.was_truncated {
            entries.push((
                CborValue::Text("truncated".to_owned()),
                CborValue::Bool(true),
            ));
        }
        entries.push((
            CborValue::Text("total_lines".to_owned()),
            CborValue::Integer((total_lines as i64).into()),
        ));
        entries.push((
            CborValue::Text("total_bytes".to_owned()),
            CborValue::Integer((file_bytes as i64).into()),
        ));
    }
    let mut display = ok_display(display_args);
    display.stats = text_stats(&truncated.content);
    Ok(ToolOutput {
        result: CborValue::Map(entries),
        display,
    })
}

/// One 1-based line range requested from a file.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ReadLineRange {
    /// 1-based inclusive first line to include.
    pub(crate) start_line: usize,
    /// Number of lines to include, or `None` for the rest of the file.
    pub(crate) line_count: Option<usize>,
}

impl ReadLineRange {
    fn contains_line(&self, line: usize) -> bool {
        if line < self.start_line {
            return false;
        }
        match self.line_count {
            Some(line_count) => line < self.start_line.saturating_add(line_count),
            None => true,
        }
    }

    fn end_line(&self) -> Option<usize> {
        self.line_count
            .map(|line_count| self.start_line.saturating_add(line_count))
    }
}

struct ReadRequest {
    ranges: Vec<ReadLineRange>,
    display_ranges: Vec<String>,
}

pub(crate) struct ReadSlice {
    pub(crate) content: String,
    #[cfg_attr(not(test), expect(dead_code))]
    pub(crate) line_count: usize,
    pub(crate) valid_utf8: bool,
    /// Total lines in the source. For [`slice_line_ranges`] this is computed
    /// by scanning the whole file after collecting requested ranges.
    pub(crate) total_lines: usize,
}

/// Stream line ranges from `path` while reading the file contents once.
fn stream_slice_line_ranges(path: &Path, ranges: &[ReadLineRange]) -> std::io::Result<ReadSlice> {
    let bytes = fs::read(path)?;
    Ok(slice_line_ranges(&bytes, ranges))
}

/// Render line ranges from already-loaded file bytes using `read` output rules.
pub(crate) fn slice_line_ranges(input: &[u8], ranges: &[ReadLineRange]) -> ReadSlice {
    let mut state = SliceState::new(ranges);

    let mut line_start = 0usize;
    let mut index = 0usize;
    while index < input.len() {
        match input[index] {
            b'\r' => {
                let is_crlf = index + 1 < input.len() && input[index + 1] == b'\n';
                let ending = if is_crlf {
                    LineEndingKind::Crlf
                } else {
                    LineEndingKind::Cr
                };
                state.push_line(&input[line_start..index], Some(ending));
                index += if is_crlf { 2 } else { 1 };
                line_start = index;
            }
            b'\n' => {
                state.push_line(&input[line_start..index], Some(LineEndingKind::Lf));
                index += 1;
                line_start = index;
            }
            _ => index += 1,
        }
    }

    if line_start < input.len() {
        state.push_line(&input[line_start..], None);
    }

    state.finish()
}

struct SliceState {
    ranges: Vec<ReadLineRange>,
    chunks: Vec<Vec<ReadLine>>,
    total_lines: usize,
    valid_utf8: bool,
}

struct ReadLine {
    number: usize,
    content: Option<String>,
    ending: Option<LineEndingKind>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum LineEndingKind {
    Lf,
    Crlf,
    Cr,
}

impl SliceState {
    fn new(ranges: &[ReadLineRange]) -> Self {
        Self {
            ranges: ranges.to_vec(),
            chunks: ranges.iter().map(|_| Vec::new()).collect(),
            total_lines: 0,
            valid_utf8: true,
        }
    }

    fn push_line(&mut self, line: &[u8], ending: Option<LineEndingKind>) {
        self.total_lines += 1;
        let valid_line = std::str::from_utf8(line).ok();
        if valid_line.is_none() {
            self.valid_utf8 = false;
        }
        for (range, chunk) in self.ranges.iter().zip(self.chunks.iter_mut()) {
            if range.contains_line(self.total_lines) {
                chunk.push(ReadLine {
                    number: self.total_lines,
                    content: valid_line.map(ToOwned::to_owned),
                    ending,
                });
            }
        }
    }

    fn finish(self) -> ReadSlice {
        let rendered_chunks = self
            .chunks
            .iter()
            .map(|lines| {
                lines
                    .iter()
                    .map(render_read_line)
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .collect::<Vec<_>>();
        let line_count = self.chunks.iter().map(Vec::len).sum();
        ReadSlice {
            content: rendered_chunks.join("\n\n"),
            line_count,
            valid_utf8: self.valid_utf8,
            total_lines: self.total_lines,
        }
    }
}

fn render_read_line(line: &ReadLine) -> String {
    let mut markers = Vec::new();
    if line.content.is_none() {
        markers.push("invalid-utf8");
    }
    match line.ending {
        Some(LineEndingKind::Lf) => {}
        Some(LineEndingKind::Crlf) => markers.push("crlf"),
        Some(LineEndingKind::Cr) => markers.push("cr"),
        None => markers.push("no_nl"),
    }

    let marker = if markers.is_empty() {
        String::new()
    } else {
        format!("({})", markers.join(","))
    };
    match &line.content {
        Some(content) => format!("{}{marker} {content}", line.number),
        None => format!("{}{marker}", line.number),
    }
}

fn parse_read_request(arguments: &CborValue) -> Result<ReadRequest, ToolFailure> {
    if let Some(ranges) = optional_argument_array(arguments, "ranges")? {
        return parse_disjoint_read_ranges(arguments, ranges);
    }

    let start_line_arg = optional_argument_int(arguments, "start_line");
    let line_count_arg = optional_argument_int(arguments, "line_count");
    let start_line = parse_read_start_line(start_line_arg)?;
    let line_count = parse_read_line_count(line_count_arg)?;
    Ok(ReadRequest {
        ranges: vec![ReadLineRange {
            start_line,
            line_count,
        }],
        display_ranges: vec![format_read_range(
            start_line_arg.map(|_| start_line),
            line_count,
        )],
    })
}

fn parse_disjoint_read_ranges(
    arguments: &CborValue,
    ranges: &[CborValue],
) -> Result<ReadRequest, ToolFailure> {
    if has_argument(arguments, "start_line") || has_argument(arguments, "line_count") {
        return Err(ToolFailure::new(
            "ranges cannot be combined with start_line or line_count",
        ));
    }
    if ranges.is_empty() {
        return Err(ToolFailure::new("ranges array must not be empty"));
    }
    if MAX_READ_RANGES_PER_CALL < ranges.len() {
        return Err(ToolFailure::new(format!(
            "requested range count exceeds limit of {MAX_READ_RANGES_PER_CALL}"
        )));
    }

    let mut parsed = Vec::new();
    let mut display_ranges = Vec::new();
    for range in ranges {
        let start_line = parse_required_range_line(range, "start_line")?;
        let line_count = parse_required_range_line(range, "line_count")?;
        parsed.push(ReadLineRange {
            start_line,
            line_count: Some(line_count),
        });
        display_ranges.push(format_read_range(Some(start_line), Some(line_count)));
    }
    validate_non_overlapping_read_ranges(&parsed)?;
    Ok(ReadRequest {
        ranges: parsed,
        display_ranges,
    })
}

fn optional_argument_array<'a>(
    arguments: &'a CborValue,
    key: &str,
) -> Result<Option<&'a [CborValue]>, ToolFailure> {
    let CborValue::Map(entries) = arguments else {
        return Ok(None);
    };
    for (entry_key, value) in entries {
        if let CborValue::Text(entry_key) = entry_key
            && entry_key == key
        {
            return match value {
                CborValue::Array(array) => Ok(Some(array)),
                _ => Err(ToolFailure::new(format!(
                    "argument `{key}` must be an array"
                ))),
            };
        }
    }
    Ok(None)
}

fn has_argument(arguments: &CborValue, key: &str) -> bool {
    let CborValue::Map(entries) = arguments else {
        return false;
    };
    entries
        .iter()
        .any(|(entry_key, _)| matches!(entry_key, CborValue::Text(entry_key) if entry_key == key))
}

fn parse_required_range_line(range: &CborValue, key: &str) -> Result<usize, ToolFailure> {
    match cbor_map_int(range, key) {
        Some(value) if value < 1 => Err(ToolFailure::new(format!("{key} must be >= 1"))),
        Some(value) => {
            usize::try_from(value).map_err(|_| ToolFailure::new(format!("{key} is too large")))
        }
        None => Err(ToolFailure::new(format!(
            "each range must have an integer {key}"
        ))),
    }
}

fn validate_non_overlapping_read_ranges(ranges: &[ReadLineRange]) -> Result<(), ToolFailure> {
    let mut sorted: Vec<_> = ranges.iter().collect();
    sorted.sort_by_key(|range| range.start_line);
    for pair in sorted.windows(2) {
        let Some(end_line) = pair[0].end_line() else {
            return Err(ToolFailure::new(
                "open-ended read ranges cannot be combined with other ranges",
            ));
        };
        if pair[1].start_line < end_line {
            return Err(ToolFailure::new("overlapping ranges"));
        }
    }
    Ok(())
}

fn validate_ranges_with_total(
    ranges: &[ReadLineRange],
    total_lines: usize,
    display_args: &str,
) -> Result<(), ToolFailure> {
    let max_read_start_line = total_lines.max(1);
    for range in ranges {
        if max_read_start_line < range.start_line {
            return Err(ToolFailure::new(format!(
                "start_line {} is past end of file (total_lines: {total_lines})",
                range.start_line
            ))
            .with_args(display_args.to_owned()));
        }
    }
    Ok(())
}

fn parse_read_start_line(value: Option<i64>) -> Result<usize, ToolFailure> {
    match value {
        None => Ok(1),
        Some(value) if value < 1 => Err(ToolFailure::new("start_line must be >= 1")),
        Some(value) => Ok(value as usize),
    }
}

fn parse_read_line_count(value: Option<i64>) -> Result<Option<usize>, ToolFailure> {
    match value {
        None => Ok(None),
        Some(value) if value < 1 => Err(ToolFailure::new("line_count must be >= 1")),
        Some(value) => Ok(Some(value as usize)),
    }
}

pub(crate) fn format_read_range(start_line: Option<usize>, line_count: Option<usize>) -> String {
    match (start_line, line_count) {
        (None, None) => "..".to_owned(),
        (Some(start), None) => format!("{start}.."),
        (None, Some(count)) => format!("1..{}", 1usize.saturating_add(count)),
        (Some(start), Some(count)) => format!("{start}..{}", start.saturating_add(count)),
    }
}

/// In-memory equivalent of single-range file slicing, retained for tests
/// that exercise the slicing logic on a string rather than a file.
#[cfg(test)]
pub(crate) fn slice_lines(input: &str, start_line: usize, line_count: Option<usize>) -> ReadSlice {
    let all_lines: Vec<&str> = input.lines().collect();
    let total_lines = all_lines.len();
    let start_idx = start_line.saturating_sub(1).min(total_lines);
    let end_idx = match line_count {
        Some(count) => start_idx.saturating_add(count).min(total_lines),
        None => total_lines,
    };
    ReadSlice {
        content: all_lines[start_idx..end_idx]
            .iter()
            .enumerate()
            .map(|(index, line)| format!("{} {line}", start_idx + index + 1))
            .collect::<Vec<_>>()
            .join("\n"),
        line_count: end_idx.saturating_sub(start_idx),
        valid_utf8: true,
        total_lines,
    }
}
