//! `apply_patch` custom tool: parse Codex-style patch text and apply it.

use std::path::{Path, PathBuf};

use tau_proto::{CborValue, ToolUsePayload};

use crate::diff::compute_diff;
use crate::display::{ToolFailure, ToolOutput};
use crate::tools::world::ShellWorld;

const SUMMARY_HEADER: &str = "Success. Updated the following files:";

#[expect(unused)]
pub(crate) const APPLY_PATCH_LARK_GRAMMAR: &str = include_str!("apply_patch.lark");

pub(crate) fn apply_patch(
    arguments: &CborValue,
    world: &mut ShellWorld,
) -> Result<ToolOutput, ToolFailure> {
    let patch = patch_text(arguments)?;

    let hunks = parse_patch(patch).map_err(ToolFailure::new)?;
    let changes = match apply_hunks(&hunks, world) {
        Ok(changes) => changes,
        Err(failure) => {
            return Err(ToolFailure::new(failure.message)
                .with_payload(display_payload_for_failure(&failure.changes)));
        }
    };

    let summary = format_summary(&changes);
    let payload = display_payload_for_changes(&changes, &summary);
    let result = CborValue::Text(summary.clone());

    let mut display = crate::display::ok_display("apply_patch");
    display.payload = payload;
    Ok(ToolOutput { result, display })
}

pub(crate) fn lock_directories(arguments: &CborValue) -> Result<Vec<PathBuf>, ToolFailure> {
    let patch = patch_text(arguments)?;
    let hunks = parse_patch(patch).map_err(ToolFailure::new)?;
    let cwd = std::env::current_dir().map_err(|error| ToolFailure::from(error.to_string()))?;
    let mut dirs = Vec::new();

    for hunk in &hunks {
        match hunk {
            Hunk::Add { path, .. } => {
                let abs = resolve_path(&cwd, path);
                dirs.push(crate::dir_lock::canonical_write_lock_dir(&abs)?);
            }
            Hunk::Delete { path } => {
                let abs = resolve_path(&cwd, path);
                dirs.push(crate::dir_lock::canonical_path_parent(&abs)?);
            }
            Hunk::Update {
                path, move_path, ..
            } => {
                let abs = resolve_path(&cwd, path);
                if let Some(move_path) = move_path {
                    dirs.push(crate::dir_lock::canonical_path_parent(&abs)?);
                    let dest_abs = resolve_path(&cwd, move_path);
                    dirs.push(crate::dir_lock::canonical_write_lock_dir(&dest_abs)?);
                } else {
                    dirs.push(crate::dir_lock::canonical_update_lock_dir(&abs)?);
                }
            }
        }
    }

    Ok(dirs)
}

fn patch_text(arguments: &CborValue) -> Result<&str, ToolFailure> {
    match arguments {
        CborValue::Text(text) => Ok(text),
        _ => Err(ToolFailure::new(
            "apply_patch expects freeform patch text, not a structured payload",
        )),
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum Hunk {
    Add {
        path: PathBuf,
        contents: String,
    },
    Delete {
        path: PathBuf,
    },
    Update {
        path: PathBuf,
        move_path: Option<PathBuf>,
        chunks: Vec<UpdateChunk>,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct UpdateChunk {
    change_context: Option<String>,
    old_lines: Vec<String>,
    new_lines: Vec<String>,
    is_end_of_file: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ChangeStatus {
    Add,
    Modify,
    Delete,
}

impl ChangeStatus {
    fn short_name(self) -> &'static str {
        match self {
            Self::Add => "A",
            Self::Modify => "M",
            Self::Delete => "D",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct AppliedChange {
    display_path: String,
    path: PathBuf,
    status: ChangeStatus,
    old_content: String,
    new_content: Option<String>,
}

#[derive(Debug, Eq, PartialEq)]
struct ApplyPatchFailure {
    message: String,
    changes: Vec<AppliedChange>,
}

impl ApplyPatchFailure {
    fn new(message: impl Into<String>, changes: &[AppliedChange]) -> Self {
        Self {
            message: message.into(),
            changes: changes.to_vec(),
        }
    }
}

fn apply_hunks(
    hunks: &[Hunk],
    world: &mut ShellWorld,
) -> Result<Vec<AppliedChange>, ApplyPatchFailure> {
    if hunks.is_empty() {
        return Err(ApplyPatchFailure::new("No files were modified.", &[]));
    }

    let cwd =
        std::env::current_dir().map_err(|error| ApplyPatchFailure::new(error.to_string(), &[]))?;
    let mut changes = Vec::with_capacity(hunks.len());

    for hunk in hunks {
        match hunk {
            Hunk::Add { path, contents } => {
                let abs = resolve_path(&cwd, path);
                if read_optional_file(&abs, world)
                    .map_err(|message| ApplyPatchFailure::new(message, &changes))?
                    .is_some()
                {
                    return Err(ApplyPatchFailure::new(
                        format!("Add File target already exists: {}", abs.display()),
                        &changes,
                    ));
                }
                let old_content = String::new();
                write_file_creating_parent(&abs, contents, world).map_err(|error| {
                    ApplyPatchFailure::new(
                        format!("Failed to write file {}: {error}", abs.display()),
                        &changes,
                    )
                })?;
                changes.push(AppliedChange {
                    display_path: path.display().to_string(),
                    path: abs.clone(),
                    status: ChangeStatus::Add,
                    old_content,
                    new_content: Some(contents.clone()),
                });
            }
            Hunk::Delete { path } => {
                let abs = resolve_path(&cwd, path);
                if world.is_dir(&abs).map_err(|_| {
                    ApplyPatchFailure::new(
                        format!("Failed to delete file {}", abs.display()),
                        &changes,
                    )
                })? {
                    return Err(ApplyPatchFailure::new(
                        format!("Failed to delete file {}", abs.display()),
                        &changes,
                    ));
                }
                let old_content = world.read_to_string(&abs).map_err(|_| {
                    ApplyPatchFailure::new(
                        format!("Failed to delete file {}", abs.display()),
                        &changes,
                    )
                })?;
                world.remove_file(&abs).map_err(|_| {
                    ApplyPatchFailure::new(
                        format!("Failed to delete file {}", abs.display()),
                        &changes,
                    )
                })?;
                changes.push(AppliedChange {
                    display_path: path.display().to_string(),
                    path: abs.clone(),
                    status: ChangeStatus::Delete,
                    old_content,
                    new_content: None,
                });
            }
            Hunk::Update {
                path,
                move_path,
                chunks,
            } => {
                let abs = resolve_path(&cwd, path);
                let old_content = world.read_to_string(&abs).map_err(|error| {
                    ApplyPatchFailure::new(
                        format!("Failed to read file to update {}: {error}", abs.display()),
                        &changes,
                    )
                })?;
                let new_content = derive_new_contents_from_chunks(&abs, &old_content, chunks)
                    .map_err(|message| ApplyPatchFailure::new(message, &changes))?;

                let (change_path, display_path) = if let Some(move_path) = move_path {
                    let dest_abs = resolve_path(&cwd, move_path);
                    let overwritten_move_content = read_optional_file(&dest_abs, world)
                        .map_err(|message| ApplyPatchFailure::new(message, &changes))?;
                    write_file_creating_parent(&dest_abs, &new_content, world).map_err(
                        |error| {
                            ApplyPatchFailure::new(
                                format!("Failed to write file {}: {error}", dest_abs.display()),
                                &changes,
                            )
                        },
                    )?;
                    let dest_write_change_index = changes.len();
                    changes.push(AppliedChange {
                        display_path: move_path.display().to_string(),
                        path: dest_abs.clone(),
                        status: ChangeStatus::Add,
                        old_content: overwritten_move_content.unwrap_or_default(),
                        new_content: Some(new_content.clone()),
                    });
                    if world.is_dir(&abs).map_err(|_| {
                        ApplyPatchFailure::new(
                            format!("Failed to remove original {}", abs.display()),
                            &changes,
                        )
                    })? {
                        return Err(ApplyPatchFailure::new(
                            format!("Failed to remove original {}", abs.display()),
                            &changes,
                        ));
                    }
                    world.remove_file(&abs).map_err(|_| {
                        ApplyPatchFailure::new(
                            format!("Failed to remove original {}", abs.display()),
                            &changes,
                        )
                    })?;
                    changes[dest_write_change_index] = AppliedChange {
                        display_path: move_path.display().to_string(),
                        path: abs.clone(),
                        status: ChangeStatus::Modify,
                        old_content: old_content.clone(),
                        new_content: Some(new_content.clone()),
                    };
                    continue;
                } else {
                    world
                        .write_file(&abs, new_content.as_bytes())
                        .map_err(|error| {
                            ApplyPatchFailure::new(
                                format!("Failed to write file {}: {error}", abs.display()),
                                &changes,
                            )
                        })?;
                    (abs.clone(), path.display().to_string())
                };

                changes.push(AppliedChange {
                    display_path,
                    path: change_path,
                    status: ChangeStatus::Modify,
                    old_content,
                    new_content: Some(new_content),
                });
            }
        }
    }

    Ok(changes)
}

fn display_payload_for_changes(changes: &[AppliedChange], summary: &str) -> Option<ToolUsePayload> {
    if changes.len() == 1 {
        let change = &changes[0];
        let new_content = change.new_content.as_deref().unwrap_or_default();
        return Some(ToolUsePayload::Diff(compute_diff(
            &change.old_content,
            new_content,
        )));
    }
    Some(ToolUsePayload::Text {
        text: summary.to_owned(),
    })
}

fn display_payload_for_failure(changes: &[AppliedChange]) -> Option<ToolUsePayload> {
    if changes.is_empty() {
        return None;
    }

    let mut lines = vec!["Partial changes applied before failure:".to_owned()];
    for status in [
        ChangeStatus::Add,
        ChangeStatus::Modify,
        ChangeStatus::Delete,
    ] {
        for change in changes.iter().filter(|change| change.status == status) {
            lines.push(format!(
                "{} {}",
                change.status.short_name(),
                change.display_path
            ));
        }
    }
    Some(ToolUsePayload::Text {
        text: lines.join("\n"),
    })
}

fn format_summary(changes: &[AppliedChange]) -> String {
    let mut lines = vec![SUMMARY_HEADER.to_owned()];
    for status in [
        ChangeStatus::Add,
        ChangeStatus::Modify,
        ChangeStatus::Delete,
    ] {
        for change in changes.iter().filter(|change| change.status == status) {
            lines.push(format!(
                "{} {}",
                change.status.short_name(),
                change.display_path
            ));
        }
    }
    lines.join("\n")
}

fn read_optional_file(path: &Path, world: &mut ShellWorld) -> Result<Option<String>, String> {
    match world.read_to_string(path) {
        Ok(content) => Ok(Some(content)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.to_string()),
    }
}

fn write_file_creating_parent(
    path: &Path,
    contents: &str,
    world: &mut ShellWorld,
) -> Result<(), std::io::Error> {
    if let Some(parent) = path.parent() {
        world.create_dir_all(parent)?;
    }
    world.write_file(path, contents.as_bytes())
}

fn resolve_path(cwd: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        cwd.join(path)
    }
}

fn derive_new_contents_from_chunks(
    path: &Path,
    original_contents: &str,
    chunks: &[UpdateChunk],
) -> Result<String, String> {
    let mut original_lines: Vec<String> = original_contents.split('\n').map(String::from).collect();
    if original_lines.last().is_some_and(String::is_empty) {
        original_lines.pop();
    }

    let replacements = compute_replacements(&original_lines, path, chunks)?;
    let mut new_lines = apply_replacements(original_lines, &replacements);
    if !new_lines.last().is_some_and(String::is_empty) {
        new_lines.push(String::new());
    }
    Ok(new_lines.join("\n"))
}

fn compute_replacements(
    original_lines: &[String],
    path: &Path,
    chunks: &[UpdateChunk],
) -> Result<Vec<(usize, usize, Vec<String>)>, String> {
    let mut replacements = Vec::new();
    let mut line_index = 0usize;

    for chunk in chunks {
        if let Some(ctx_line) = &chunk.change_context {
            if let Some(idx) = seek_sequence(
                original_lines,
                std::slice::from_ref(ctx_line),
                line_index,
                false,
            ) {
                line_index = idx + 1;
            } else {
                return Err(format!(
                    "Failed to find context '{}' in {}",
                    ctx_line,
                    path.display()
                ));
            }
        }

        if chunk.old_lines.is_empty() {
            let insertion_idx = if original_lines.last().is_some_and(String::is_empty) {
                original_lines.len().saturating_sub(1)
            } else {
                original_lines.len()
            };
            replacements.push((insertion_idx, 0, chunk.new_lines.clone()));
            continue;
        }

        let mut pattern: &[String] = &chunk.old_lines;
        let mut new_slice: &[String] = &chunk.new_lines;
        let mut found = seek_sequence(original_lines, pattern, line_index, chunk.is_end_of_file);

        if found.is_none() && pattern.last().is_some_and(String::is_empty) {
            pattern = &pattern[..pattern.len() - 1];
            if new_slice.last().is_some_and(String::is_empty) {
                new_slice = &new_slice[..new_slice.len() - 1];
            }
            found = seek_sequence(original_lines, pattern, line_index, chunk.is_end_of_file);
        }

        if let Some(start_idx) = found {
            replacements.push((start_idx, pattern.len(), new_slice.to_vec()));
            line_index = start_idx + pattern.len();
        } else {
            return Err(format!(
                "Failed to find expected lines in {}:\n{}",
                path.display(),
                chunk.old_lines.join("\n")
            ));
        }
    }

    replacements.sort_by_key(|(start_idx, _, _)| *start_idx);
    Ok(replacements)
}

fn apply_replacements(
    mut lines: Vec<String>,
    replacements: &[(usize, usize, Vec<String>)],
) -> Vec<String> {
    for (start_idx, old_len, new_segment) in replacements.iter().rev() {
        for _ in 0..*old_len {
            if *start_idx < lines.len() {
                lines.remove(*start_idx);
            }
        }
        for (offset, new_line) in new_segment.iter().enumerate() {
            lines.insert(*start_idx + offset, new_line.clone());
        }
    }
    lines
}

fn seek_sequence(lines: &[String], pattern: &[String], start: usize, eof: bool) -> Option<usize> {
    if pattern.is_empty() {
        return Some(start);
    }
    if pattern.len() > lines.len() {
        return None;
    }

    let search_start = if eof && lines.len() >= pattern.len() {
        lines.len() - pattern.len()
    } else {
        start
    };

    for i in search_start..=lines.len().saturating_sub(pattern.len()) {
        if lines[i..i + pattern.len()] == *pattern {
            return Some(i);
        }
    }
    for i in search_start..=lines.len().saturating_sub(pattern.len()) {
        let mut ok = true;
        for (p_idx, pat) in pattern.iter().enumerate() {
            if lines[i + p_idx].trim_end() != pat.trim_end() {
                ok = false;
                break;
            }
        }
        if ok {
            return Some(i);
        }
    }
    for i in search_start..=lines.len().saturating_sub(pattern.len()) {
        let mut ok = true;
        for (p_idx, pat) in pattern.iter().enumerate() {
            if lines[i + p_idx].trim() != pat.trim() {
                ok = false;
                break;
            }
        }
        if ok {
            return Some(i);
        }
    }
    None
}

fn parse_patch(patch: &str) -> Result<Vec<Hunk>, String> {
    let trimmed = patch.trim();
    let lines: Vec<&str> = trimmed.lines().collect();
    if lines.first().copied() != Some("*** Begin Patch") {
        return Err("invalid patch: missing '*** Begin Patch' header".to_owned());
    }
    if lines.last().copied() != Some("*** End Patch") {
        return Err("invalid patch: missing '*** End Patch' footer".to_owned());
    }

    let mut index = 1usize;
    let mut hunks = Vec::new();
    while index + 1 < lines.len() {
        let line = lines[index];
        if let Some(path) = line.strip_prefix("*** Add File: ") {
            index += 1;
            let mut contents = Vec::new();
            while index + 1 < lines.len() && !lines[index].starts_with("*** ") {
                let Some(content) = lines[index].strip_prefix('+') else {
                    return Err(format!("invalid add-file line: {}", lines[index]));
                };
                contents.push(content.to_owned());
                index += 1;
            }
            if contents.is_empty() {
                return Err(format!(
                    "Add File hunk for {} must contain at least one line",
                    path
                ));
            }
            hunks.push(Hunk::Add {
                path: PathBuf::from(path),
                contents: contents.join("\n") + "\n",
            });
            continue;
        }

        if let Some(path) = line.strip_prefix("*** Delete File: ") {
            hunks.push(Hunk::Delete {
                path: PathBuf::from(path),
            });
            index += 1;
            continue;
        }

        if let Some(path) = line.strip_prefix("*** Update File: ") {
            index += 1;
            let mut move_path = None;
            if index + 1 < lines.len()
                && let Some(dest) = lines[index].strip_prefix("*** Move to: ")
            {
                move_path = Some(PathBuf::from(dest));
                index += 1;
            }

            let mut chunks = Vec::new();
            while index + 1 < lines.len() && !lines[index].starts_with("*** ") {
                let header = lines[index];
                let change_context = if header == "@@" {
                    None
                } else if let Some(context) = header.strip_prefix("@@ ") {
                    Some(context.to_owned())
                } else {
                    return Err(format!("invalid update hunk header: {header}"));
                };
                index += 1;

                let mut old_lines = Vec::new();
                let mut new_lines = Vec::new();
                let mut is_end_of_file = false;
                while index + 1 < lines.len()
                    && !lines[index].starts_with("@@")
                    && !lines[index].starts_with("*** ")
                {
                    if lines[index] == "*** End of File" {
                        is_end_of_file = true;
                        index += 1;
                        break;
                    }
                    let mut chars = lines[index].chars();
                    match chars.next() {
                        None => {
                            old_lines.push(String::new());
                            new_lines.push(String::new());
                        }
                        Some(' ') => {
                            let rest = chars.as_str().to_owned();
                            old_lines.push(rest.clone());
                            new_lines.push(rest);
                        }
                        Some('-') => {
                            let rest = chars.as_str().to_owned();
                            old_lines.push(rest);
                        }
                        Some('+') => {
                            let rest = chars.as_str().to_owned();
                            new_lines.push(rest);
                        }
                        _ => return Err(format!("invalid update hunk line: {}", lines[index])),
                    }
                    index += 1;
                }

                if old_lines.is_empty() && new_lines.is_empty() {
                    return Err(format!(
                        "Update File hunk for {} must contain at least one line",
                        path
                    ));
                }
                chunks.push(UpdateChunk {
                    change_context,
                    old_lines,
                    new_lines,
                    is_end_of_file,
                });
            }

            if chunks.is_empty() {
                return Err(format!(
                    "Update File hunk for {} must contain at least one chunk",
                    path
                ));
            }
            hunks.push(Hunk::Update {
                path: PathBuf::from(path),
                move_path,
                chunks,
            });
            continue;
        }

        return Err(format!("invalid patch operation: {line}"));
    }

    if hunks.is_empty() {
        return Err("invalid patch: no file operations found".to_owned());
    }
    Ok(hunks)
}

#[cfg(test)]
mod tests;
