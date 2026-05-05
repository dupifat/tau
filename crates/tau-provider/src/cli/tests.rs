use std::os::unix::fs::PermissionsExt;

use super::*;

#[test]
#[cfg(unix)]
fn write_provider_to_models_json5_preserves_symlink() {
    let temp_dir = tempfile::tempdir().unwrap();
    let target = temp_dir.path().join("target.json5");
    let link = temp_dir.path().join("models.json5");
    std::fs::write(&target, r#"{ providers: {} }"#).unwrap();
    std::os::unix::fs::symlink(&target, &link).unwrap();
    let permissions = std::fs::Permissions::from_mode(0o640);
    std::fs::set_permissions(&target, permissions).unwrap();

    write_provider_to_models_json5(
        &link,
        "openai",
        &serde_json::json!({
            "auth": "api-key",
            "api": "openai-chat",
            "models": [],
        }),
    )
    .unwrap();

    assert!(
        std::fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink()
    );

    let updated = std::fs::read_to_string(&target).unwrap();
    assert!(updated.contains("openai"));
    assert_eq!(
        std::fs::metadata(&target).unwrap().permissions().mode() & 0o777,
        0o640
    );
}
