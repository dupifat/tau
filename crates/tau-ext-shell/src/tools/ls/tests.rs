use super::*;

fn ls_args(path: &std::path::Path) -> CborValue {
    CborValue::Map(vec![(
        CborValue::Text("path".to_owned()),
        CborValue::Text(path.display().to_string()),
    )])
}

#[test]
fn empty_ls_display_uses_zero_line_and_byte_stats() {
    let tempdir = tempfile::TempDir::new().expect("tempdir");

    let output = run_ls(&ls_args(tempdir.path())).expect("ls output");

    assert!(output.display.info_chips.is_empty());
    assert_eq!(output.display.stats.lines, Some(0));
    assert_eq!(output.display.stats.bytes, Some(0));
    assert!(matches!(
        &output.result,
        CborValue::Map(entries)
            if entries.iter().any(|(key, value)| matches!(
                (key, value),
                (CborValue::Text(key), CborValue::Text(value))
                    if key == "output" && value == "(empty directory)"
            ))
    ));
}

#[test]
fn ls_display_uses_line_and_byte_stats_instead_of_entry_chip() {
    let tempdir = tempfile::TempDir::new().expect("tempdir");
    std::fs::write(tempdir.path().join("alpha"), "a").expect("write alpha");
    std::fs::write(tempdir.path().join("beta"), "b").expect("write beta");

    let output = run_ls(&ls_args(tempdir.path())).expect("ls output");

    assert!(output.display.info_chips.is_empty());
    assert_eq!(output.display.stats.lines, Some(2));
    assert_eq!(output.display.stats.bytes, Some("alpha\nbeta".len() as u64));
}
