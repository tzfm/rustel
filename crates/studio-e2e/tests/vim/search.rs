use super::*;

#[test]
fn slash_question_mark_and_repeats_move_the_caret() {
    let mut studio = hermetic();
    enable_vim(&mut studio);
    studio.set_score("// one\n// two\n// one\n");
    studio.set_caret(3);

    studio.type_text("/one");
    assert!(
        studio.rows().iter().any(|row| row.contains("VIM /one")),
        "the search prompt is visible"
    );
    studio.press(KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(studio.caret(), 17, "forward skips the match at the caret");
    studio.type_text("n");
    assert_eq!(studio.caret(), 3, "n wraps to the first match");
    studio.type_text("N");
    assert_eq!(studio.caret(), 17, "N reverses the search direction");

    studio.set_caret(17);
    studio.type_text("?two");
    assert!(
        studio.rows().iter().any(|row| row.contains("VIM ?two")),
        "the backward prompt is visible"
    );
    studio.press(KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(studio.caret(), 10);
}

#[test]
fn counted_search_and_empty_search_repeat() {
    let mut studio = hermetic();
    enable_vim(&mut studio);
    studio.set_score("x\nx\nx\nx\n");
    studio.set_caret(0);

    studio.type_text("3/x");
    studio.press(KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(studio.caret(), 6, "the count picks the third match");

    studio.type_text("/");
    studio.press(KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(
        studio.caret(),
        0,
        "an empty search repeats the last pattern"
    );

    studio.type_text("2n");
    assert_eq!(studio.caret(), 4);
    studio.type_text("N");
    assert_eq!(studio.caret(), 2);
}

#[test]
fn a_vimscript_mapping_drives_search_and_ctrl_c_leaves_insert() {
    let mut studio = hermetic();
    std::fs::write(
        studio.config_home().join("vimrc"),
        "nnoremap J /\\$<CR>\n\
         nnoremap K ?\\$<CR>\n\
         nnoremap M i_<C-c>l\n\
         nnoremap X hx\n",
    )
    .expect("Vim mapping file");
    enable_vim(&mut studio);
    let source = "$: a\n$: b\n";
    studio.set_score(source);
    studio.set_caret(0);

    studio.type_text("J");
    assert_eq!(studio.caret(), 5, "J searches forward for a dollar sign");
    assert_eq!(studio.source(), source, "search does not edit the score");

    studio.type_text("K");
    assert_eq!(studio.caret(), 0, "K searches backward");

    studio.type_text("M");
    assert_eq!(studio.source(), "_$: a\n$: b\n", "M types one underscore");
    assert_eq!(studio.caret(), 1, "Ctrl+C left Insert mode before l moved");

    studio.type_text("X");
    assert_eq!(
        studio.source(),
        source,
        "X deletes the underscore before the caret"
    );

    studio.set_score("// ");
    studio.type_text("A_");
    studio.chord("ctrl+shift+c");
    studio.type_text("l");
    assert_eq!(
        studio.source(),
        "// _",
        "Ctrl+Shift+C leaves Insert too, so l moves instead of typing"
    );
}
