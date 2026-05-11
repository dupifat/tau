use std::os::unix::fs::PermissionsExt;

use super::*;

#[test]
fn ollama_provider_entry_enables_llama_cpp_cache_compat() {
    let entry = build_provider_entry(&ProviderKind::Ollama);

    assert_eq!(entry["compat"]["supportsLlamaCppCache"], true);
}

#[test]
#[cfg(unix)]
fn write_provider_to_models_json5_preserves_symlink() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let target = temp_dir.path().join("target.json5");
    let link = temp_dir.path().join("models.json5");
    std::fs::write(&target, r#"{ providers: {} }"#).expect("write target");
    std::os::unix::fs::symlink(&target, &link).expect("symlink models");
    let permissions = std::fs::Permissions::from_mode(0o640);
    std::fs::set_permissions(&target, permissions).expect("set target permissions");

    write_provider_to_models_json5(
        &link,
        "openai",
        &serde_json::json!({
            "auth": "api-key",
            "api": "openai-chat",
            "models": [],
        }),
    )
    .expect("write provider");

    assert!(
        std::fs::symlink_metadata(&link)
            .expect("symlink metadata")
            .file_type()
            .is_symlink()
    );

    let updated = std::fs::read_to_string(&target).expect("read target");
    assert!(updated.contains("openai"));
    assert_eq!(
        std::fs::metadata(&target)
            .expect("target metadata")
            .permissions()
            .mode()
            & 0o777,
        0o640
    );
}
