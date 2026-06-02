use tau_proto::DiffLine;

use super::*;

#[test]
fn multi_line_replacement_renders_removals_before_additions() {
    let diff = compute_diff("one\ntwo\nkeep\n", "alpha\nbeta\nkeep\n");

    assert_eq!(diff.removed, 2);
    assert_eq!(diff.added, 2);
    assert_eq!(diff.hunks.len(), 1);
    assert_eq!(
        diff.hunks[0].lines,
        vec![
            DiffLine::Remove { text: "one".into() },
            DiffLine::Remove { text: "two".into() },
            DiffLine::Add {
                text: "alpha".into(),
            },
            DiffLine::Add {
                text: "beta".into(),
            },
            DiffLine::Equal {
                text: "keep".into(),
            },
        ]
    );
}

#[test]
fn single_line_replacement_still_gets_inline_modify() {
    let diff = compute_diff("let count = 1;\n", "let count = 2;\n");

    assert_eq!(diff.removed, 1);
    assert_eq!(diff.added, 1);
    assert!(matches!(diff.hunks[0].lines[0], DiffLine::Modify { .. }));
}
