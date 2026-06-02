//! `AGENTS.md` and `AGENTS.*.md` discovery used at `SessionStarted` time.

use std::fs;
use std::path::{Path, PathBuf};

pub(crate) struct DiscoveredAgentsFile {
    pub(crate) file_path: PathBuf,
    pub(crate) content: String,
}

pub(crate) fn discover_session_agents_files() -> Vec<DiscoveredAgentsFile> {
    let mut roots = Vec::new();
    if let Some(home) = dirs::home_dir() {
        roots.push(home.join(".agents"));
        roots.push(home.join(".agents.local"));
    }
    if let Ok(cwd) = std::env::current_dir() {
        roots.extend(ancestor_agents_roots(&cwd));
    }
    discover_agents_files_from_roots(roots)
}

#[cfg(test)]
pub(crate) fn discover_agents_files_from(cwd: &Path) -> Vec<DiscoveredAgentsFile> {
    discover_agents_files_from_roots(ancestor_agents_roots(cwd))
}

pub(crate) fn discover_agents_files_from_roots(
    roots: impl IntoIterator<Item = PathBuf>,
) -> Vec<DiscoveredAgentsFile> {
    let mut seen = std::collections::HashSet::new();
    let mut discovered = Vec::new();
    for dir in roots {
        for candidate in agents_file_candidates(&dir) {
            let Ok(metadata) = fs::metadata(&candidate) else {
                continue;
            };
            if !metadata.is_file() {
                continue;
            }

            let Ok(content) = fs::read_to_string(&candidate) else {
                continue;
            };
            if content.trim().is_empty() {
                continue;
            }

            let file_path = candidate.canonicalize().unwrap_or(candidate);
            if !seen.insert(file_path.clone()) {
                continue;
            }
            discovered.push(DiscoveredAgentsFile { file_path, content });
        }
    }

    discovered
}

fn agents_file_candidates(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut candidates = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(is_agents_file_name)
        })
        .collect::<Vec<_>>();
    candidates.sort_by_key(|path| agents_file_sort_key(path));
    candidates
}

fn is_agents_file_name(name: &str) -> bool {
    name == "AGENTS.md" || (name.starts_with("AGENTS.") && name.ends_with(".md"))
}

fn agents_file_sort_key(path: &Path) -> (u8, String) {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default();
    let rank = if name == "AGENTS.md" { 0 } else { 1 };
    (rank, name.to_owned())
}

fn ancestor_agents_roots(cwd: &Path) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    for dir in ancestor_dirs(cwd) {
        dirs.push(dir.clone());
        dirs.push(dir.join(".agents.local"));
    }
    dirs
}

pub(crate) fn ancestor_dirs(cwd: &Path) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    let mut current = cwd.to_path_buf();
    loop {
        dirs.push(current.clone());
        let Some(parent) = current.parent() else {
            break;
        };
        if parent == current {
            break;
        }
        current = parent.to_path_buf();
    }
    dirs.reverse();
    dirs
}
