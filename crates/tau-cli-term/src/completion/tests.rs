use super::*;

fn new_test_handle() -> (tau_cli_term_raw::Term, TermHandle) {
    let (term, handle, _input_tx) = tau_cli_term_raw::Term::new_virtual(
        80,
        24,
        "> ",
        Box::new(std::io::sink()),
        tau_cli_term_raw::CursorShape::Bar,
    );
    (term, handle)
}

#[test]
fn cycling_previews_selection_and_escape_restores_original_buffer() {
    let (_term, handle) = new_test_handle();
    let mut completer = Completer::new(
        vec![
            SlashCommand::new("/model", "Switch model"),
            SlashCommand::new("/quit", "Exit"),
        ],
        CompletionData::new(),
        Theme::builtin(),
    );

    handle.set_buffer("/m".to_owned(), 2);
    completer.on_buffer_changed(&handle);

    assert!(completer.is_active());
    assert!(!completer.has_selection());
    assert_eq!(handle.get_buffer(), "/m");
    assert_eq!(handle.get_cursor(), 2);

    completer.cycle_selection(1, &handle);

    assert!(completer.has_selection());
    assert_eq!(handle.get_buffer(), "/model");
    assert_eq!(handle.get_cursor(), "/model".len());

    completer.dismiss(&handle);

    assert!(!completer.is_active());
    assert_eq!(handle.get_buffer(), "/m");
    assert_eq!(handle.get_cursor(), 2);
}

#[test]
fn accept_selection_keeps_previewed_buffer() {
    let (_term, handle) = new_test_handle();
    let data = CompletionData::new();
    data.set_arg_completions(
        CommandName::new("/model"),
        vec![CompletionItem::new("openai/gpt-5", "Latest")],
    );
    let mut completer = Completer::new(
        vec![SlashCommand::new("/model", "Switch model")],
        data,
        Theme::builtin(),
    );

    handle.set_buffer("/model op".to_owned(), "/model op".len());
    completer.on_buffer_changed(&handle);
    completer.cycle_selection(1, &handle);

    assert_eq!(handle.get_buffer(), "/model openai/gpt-5");

    assert!(completer.accept_selection(&handle));
    assert!(!completer.is_active());
    assert_eq!(handle.get_buffer(), "/model openai/gpt-5");
    assert_eq!(handle.get_cursor(), "/model openai/gpt-5".len());
}
