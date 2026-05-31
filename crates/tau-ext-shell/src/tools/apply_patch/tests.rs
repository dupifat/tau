use super::*;

#[test]
fn parse_update_hunk() {
    let patch = "*** Begin Patch\n*** Update File: hello.txt\n@@\n-old\n+new\n*** End Patch";
    let hunks = parse_patch(patch).expect("patch should parse");
    assert_eq!(hunks.len(), 1);
}

#[test]
fn compute_replacements_with_context() {
    let original = vec!["a".to_owned(), "b".to_owned(), "c".to_owned()];
    let chunks = vec![UpdateChunk {
        change_context: Some("a".to_owned()),
        old_lines: vec!["b".to_owned()],
        new_lines: vec!["B".to_owned()],
        is_end_of_file: false,
    }];
    let replacements = compute_replacements(&original, Path::new("file.txt"), &chunks)
        .expect("replacement plan should compute");
    assert_eq!(replacements, vec![(1, 1, vec!["B".to_owned()])]);
}

#[test]
fn format_single_file_diff_payload() {
    let summary = format_summary(&[AppliedChange {
        display_path: "file.txt".to_owned(),
        path: PathBuf::from("file.txt"),
        status: ChangeStatus::Modify,
        old_content: "before\n".to_owned(),
        new_content: Some("after\n".to_owned()),
    }]);
    assert!(matches!(
        display_payload_for_changes(
            &[AppliedChange {
                display_path: "file.txt".to_owned(),
                path: PathBuf::from("file.txt"),
                status: ChangeStatus::Modify,
                old_content: "before\n".to_owned(),
                new_content: Some("after\n".to_owned()),
            }],
            &summary,
        ),
        Some(ToolUsePayload::Diff(_))
    ));
}
