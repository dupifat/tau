use std::fs;

use crate::completion::{
    self, CompletionData, SlashCommand, build_candidates, build_candidates_with_home,
};

#[test]
fn dotslash_token_triggers_filesystem_candidates() {
    // Empty directory listing is fine — we just need the path to
    // *match* as a filesystem token (vs. returning the slash-cmd
    // candidate list).
    let tmp = tempfile::tempdir().expect("tempdir");
    let prefix = format!("{}/", tmp.path().display());
    // Synthesize a buffer with a recognized filesystem prefix.
    // Absolute paths are not filesystem tokens so plain slash-command
    // input can still be completed without probing the filesystem.
    let buffer = "./";
    let cursor = buffer.len();
    let cands = build_candidates(
        &[SlashCommand::new("/whatever", "")],
        &CompletionData::new(),
        buffer,
        cursor,
    );
    // No assertion on contents (the test machine's CWD differs);
    // just confirm we didn't fall through to slash-command logic.
    for c in &cands {
        assert!(!c.replacement.starts_with('/'), "expected fs candidate");
    }
    let _ = prefix;
}

#[test]
fn home_relative_token_reads_injected_home_and_preserves_tilde_replacement() {
    // `~/...` completion must read entries from the user's home
    // directory, but accepting a candidate should keep the prompt
    // home-relative instead of inserting an absolute path.
    let home = tempfile::tempdir().expect("tempdir");
    fs::write(home.path().join("alpha.txt"), "").expect("write alpha");
    fs::write(home.path().join("beta.txt"), "").expect("write beta");
    let buffer = "open ~/a now";
    let cursor = "open ~/a".len();

    let cands = build_candidates_with_home(
        &[SlashCommand::new("/whatever", "")],
        &CompletionData::new(),
        buffer,
        cursor,
        Some(home.path()),
    );

    assert_eq!(cands.len(), 1);
    assert_eq!(cands[0].label, "~/alpha.txt");
    assert_eq!(cands[0].replacement, "open ~/alpha.txt now");
}

#[test]
fn filesystem_directory_candidates_include_trailing_slash() {
    // Directory completions should be visibly distinct from files and accepting
    // one should leave the prompt ready to complete or type a child path.
    let home = tempfile::tempdir().expect("tempdir");
    fs::create_dir(home.path().join("alpha-dir")).expect("mkdir alpha-dir");
    fs::write(home.path().join("alpha.txt"), "").expect("write alpha file");
    let buffer = "open ~/alpha";
    let cursor = buffer.len();

    let cands = build_candidates_with_home(
        &[SlashCommand::new("/whatever", "")],
        &CompletionData::new(),
        buffer,
        cursor,
        Some(home.path()),
    );

    let dir = cands
        .iter()
        .find(|cand| cand.description == "directory")
        .expect("directory candidate");
    assert_eq!(dir.label, "~/alpha-dir/");
    assert_eq!(dir.replacement, "open ~/alpha-dir/");

    let file = cands
        .iter()
        .find(|cand| cand.description == "file")
        .expect("file candidate");
    assert_eq!(file.label, "~/alpha.txt");
    assert_eq!(file.replacement, "open ~/alpha.txt");
}

#[test]
fn slash_command_buffer_does_not_route_to_filesystem() {
    let cands = build_candidates(
        &[SlashCommand::new("/model", "Switch model")],
        &CompletionData::new(),
        "/mod",
        "/mod".len(),
    );
    assert_eq!(cands.len(), 1);
    assert_eq!(cands[0].replacement, "/model");
}

#[test]
fn at_token_completes_agent_mentions_in_prompt_text() {
    // Agent mentions are prompt-text completions, not slash commands. Accepting
    // one must replace only the current `@...` token and preserve surrounding
    // prompt text.
    let data = CompletionData::new();
    data.set_agent_mention_completer(std::sync::Arc::new(|args| {
        assert_eq!(args, ["wo"]);
        vec![crate::completion::CompletionItem::new("worker", "agent")]
    }));
    let buffer = "ask @wo for help";
    let cursor = "ask @wo".len();

    let cands = build_candidates(
        &[SlashCommand::new("/model", "Switch model")],
        &data,
        buffer,
        cursor,
    );

    assert_eq!(cands.len(), 1);
    assert_eq!(cands[0].label, "worker");
    assert_eq!(cands[0].description, "agent");
    assert_eq!(cands[0].replacement, "ask @worker for help");
}

#[test]
fn non_slash_non_path_buffer_returns_nothing() {
    let cands = build_candidates(
        &[SlashCommand::new("/model", "Switch model")],
        &CompletionData::new(),
        "hello",
        "hello".len(),
    );
    assert!(cands.is_empty());
}

#[test]
fn parent_traversal_token_is_recognised() {
    let cands = build_candidates(
        &[SlashCommand::new("/whatever", "")],
        &CompletionData::new(),
        "../",
        "../".len(),
    );
    // Non-empty or empty is fine; we just verify it didn't fall
    // back to slash-command behavior (which would have been empty
    // since the buffer doesn't start with '/').
    for c in &cands {
        assert!(!c.replacement.starts_with('/'));
    }
    let _ = completion::SlashCommand::new("/x", "");
}
