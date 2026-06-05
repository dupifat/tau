use std::sync::{Arc, Mutex};

use crate::{EditorContext, PROMPT_TRAILER_MARKER, append_prompt_trailer, strip_prompt_trailer};

fn ctx(ec: EditorContext) -> Arc<Mutex<EditorContext>> {
    Arc::new(Mutex::new(ec))
}

#[test]
fn no_context_returns_buffer_unchanged() {
    let out = append_prompt_trailer("hello", &ctx(EditorContext::default()));
    assert_eq!(out, "hello");
}

#[test]
fn roundtrip_strips_trailer_with_current_response() {
    let edited = append_prompt_trailer(
        "draft body",
        &ctx(EditorContext {
            current_response: Some("agent draft".to_owned()),
            last_response: None,
            previous_prompt: None,
        }),
    );
    assert!(edited.contains(PROMPT_TRAILER_MARKER));
    assert!(edited.contains("agent draft"));
    assert_eq!(strip_prompt_trailer(&edited), "draft body");
}

#[test]
fn roundtrip_strips_trailer_with_all_sections() {
    let edited = append_prompt_trailer(
        "user body",
        &ctx(EditorContext {
            current_response: Some("in progress".to_owned()),
            last_response: Some("last".to_owned()),
            previous_prompt: Some("prev".to_owned()),
        }),
    );
    assert!(edited.contains("Current response in progress"));
    assert!(edited.contains("Last response"));
    assert!(edited.contains("Previous prompt"));
    assert_eq!(strip_prompt_trailer(&edited), "user body");
}

#[test]
fn empty_section_strings_are_skipped() {
    let edited = append_prompt_trailer(
        "body",
        &ctx(EditorContext {
            current_response: Some(String::new()),
            last_response: Some("kept".to_owned()),
            previous_prompt: Some(String::new()),
        }),
    );
    assert!(!edited.contains("Current response in progress"));
    assert!(edited.contains("Last response"));
    assert!(!edited.contains("Previous prompt"));
}

#[test]
fn strip_without_marker_is_identity() {
    assert_eq!(strip_prompt_trailer("just text"), "just text");
}

#[test]
fn user_text_containing_marker_is_truncated() {
    // Documents the *current* behavior: if the user's own draft
    // happens to contain the trailer marker, `strip_prompt_trailer`
    // truncates at the first occurrence. The marker is verbose
    // enough that this is unlikely in practice, but pinning the
    // behavior makes the trade-off explicit.
    let mut user_text = String::from("body with marker: ");
    user_text.push_str(PROMPT_TRAILER_MARKER);
    user_text.push_str(" and more");
    let stripped = strip_prompt_trailer(&user_text);
    assert_eq!(stripped, "body with marker: ");
}
