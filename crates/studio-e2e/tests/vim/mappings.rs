use std::time::Duration;

use super::*;

#[test]
fn external_mappings_reload_and_survive_mode_changes_and_reopening() {
    let mut studio = hermetic();
    let default_path = studio.config_home().join("vimrc");
    assert!(!default_path.exists());
    enable_vim(&mut studio);
    let starter = std::fs::read_to_string(&default_path).expect("starter file");
    assert!(
        starter.starts_with('"'),
        "the starter is a Vimscript comment: {starter:?}"
    );

    let custom = studio.config_home().join("custom-vimrc");
    std::fs::write(&custom, "nnoremap Q dd\n").expect("custom mappings");
    open_keybinding(&mut studio, "Vim mappings path");
    studio.press(KeyCode::Enter, KeyModifiers::NONE);
    studio.paste(custom.to_str().expect("mapping path"));
    studio.press(KeyCode::Enter, KeyModifiers::NONE);
    studio.press(KeyCode::Esc, KeyModifiers::NONE);
    let source = "// first\n// second";
    studio.set_score(source);
    studio.set_caret(0);
    studio.type_text("Q");
    assert_eq!(studio.source(), "// second");
    studio.type_text("u");

    write_vimrc_reload(&mut studio, &custom, "nnoremap Q x\n");
    studio.set_caret(0);
    studio.type_text("Q");
    assert_eq!(studio.source(), "/ first\n// second");
    studio.type_text("u");

    write_vimrc_reload(&mut studio, &custom, "\" no mappings here\n");
    let unchanged = studio.source();
    studio.set_caret(0);
    studio.type_text("Q");
    assert_eq!(
        studio.source(),
        unchanged,
        "Q returns to the shipped Stop mapping"
    );
    write_vimrc_reload(&mut studio, &custom, "nnoremap Q x\n");

    open_editor_keybindings(&mut studio);
    studio.press(KeyCode::Right, KeyModifiers::NONE);
    studio.press(KeyCode::Esc, KeyModifiers::NONE);
    studio.set_score("// ");
    studio.type_text("Q");
    assert_eq!(studio.source(), "// Q");
    enable_vim(&mut studio);
    studio.set_score(source);
    studio.set_caret(0);
    studio.type_text("Q");
    assert_eq!(studio.source(), "/ first\n// second");

    studio.pump_until(Duration::from_secs(5), "mapping path saved", |studio| {
        std::fs::read(studio.config_home().join("studio.json"))
            .ok()
            .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
            .is_some_and(|prefs| {
                prefs["editor_keybindings"] == "vim"
                    && prefs["vim_mappings_path"].as_str() == custom.to_str()
            })
    });
    studio.quit();
    let mut studio = reopen_over(studio);
    studio.set_score(source);
    studio.set_caret(0);
    studio.type_text("Q");
    assert_eq!(studio.source(), "/ first\n// second");
}

#[test]
fn mappings_are_nonrecursive_and_own_keys_in_their_editor_mode() {
    let mut studio = hermetic();
    std::fs::write(
        studio.config_home().join("vimrc"),
        "nnoremap <C-q> dd\n\
         nnoremap Q x\n\
         nnoremap x dd\n\
         nnoremap U <F5>\n\
         nnoremap S <F8>\n\
         nnoremap X hx\n\
         nnoremap L <Right>x\n\
         nnoremap J jx\n\
         nnoremap K kx\n\
         nnoremap Z 0x\n\
         vnoremap q d\n\
         onoremap q i(\n",
    )
    .expect("mode mappings");
    enable_vim(&mut studio);
    let source = "// first\n// second";
    studio.set_score(source);
    studio.set_caret(0);
    studio.chord("ctrl+q");
    assert!(!studio.wants_quit());
    assert_eq!(studio.source(), "// second");
    studio.type_text("u");
    studio.set_caret(0);
    studio.type_text("Q");
    assert_eq!(studio.source(), "/ first\n// second");
    studio.type_text("u");
    studio.set_caret(0);
    studio.type_text("x");
    assert_eq!(studio.source(), "// second");

    for (caret, keys, expected) in [
        (0, "X", "abc\ndef"),
        (1, "X", "bc\ndef"),
        (1, "9X", "bc\ndef"),
        (2, "L", "abc\ndef"),
        (4, "J", "abc\ndef"),
        (0, "K", "abc\ndef"),
        (0, "9J", "abc\nef"),
        (0, "Z", "bc\ndef"),
    ] {
        studio.set_score("abc\ndef");
        studio.set_caret(caret);
        studio.type_text(keys);
        assert_eq!(studio.source(), expected, "{keys} at {caret}");
    }

    studio.set_score("// one two");
    studio.set_caret(3);
    studio.type_text("veq");
    assert_eq!(studio.source(), "//  two");
    studio.type_text("u");
    assert_eq!(studio.source(), "// one two");
    studio.set_score("// call(alpha)");
    studio.set_caret(8);
    studio.type_text("dq");
    assert_eq!(studio.source(), "// call()");
    studio.type_text("u");
    assert_eq!(studio.source(), "// call(alpha)");

    let source = "$: note(60).s(\"sine\")";
    studio.set_score(source);
    studio.type_text("U");
    studio.pump_until(Duration::from_secs(5), "mapped update plays", |studio| {
        studio.is_playing()
    });
    assert_eq!(studio.source(), source);
    studio.type_text("S");
    studio.pump_until(Duration::from_secs(5), "mapped stop settles", |studio| {
        !studio.is_playing() && !studio.is_stopping()
    });
    assert_eq!(studio.source(), source);
}

#[test]
fn modifier_key_reports_do_not_break_a_mapping_prefix() {
    let mut studio = hermetic_with(KeyboardCapabilities::enhanced());
    std::fs::write(
        studio.config_home().join("vimrc"),
        "nnoremap <Space>V <Action:visualizers>\n",
    )
    .expect("mapping");
    enable_vim(&mut studio);
    let before = studio.inline_visualizers();

    studio.press(KeyCode::Char(' '), KeyModifiers::NONE);
    studio.press(
        KeyCode::Modifier(ModifierKeyCode::LeftShift),
        KeyModifiers::SHIFT,
    );
    studio.press(KeyCode::Char('v'), KeyModifiers::SHIFT);
    assert_ne!(
        studio.inline_visualizers(),
        before,
        "the Shift report is not part of the mapped sequence"
    );
}

#[test]
fn an_action_mapping_cancels_a_pending_operator() {
    let mut studio = hermetic();
    std::fs::write(
        studio.config_home().join("vimrc"),
        "onoremap j <Action:visualizers>\n",
    )
    .expect("operator-pending action mapping");
    enable_vim(&mut studio);
    studio.set_score("// one\n// two\n");
    studio.set_caret(0);
    let before = studio.inline_visualizers();

    studio.type_text("dj");
    assert_ne!(
        studio.inline_visualizers(),
        before,
        "the action ran instead of finishing the operator"
    );
    assert_eq!(
        studio.source(),
        "// one\n// two\n",
        "the pending delete was cancelled"
    );
    studio.type_text("j");
    assert_eq!(studio.caret(), 7, "j is a plain motion again");
}

#[test]
fn an_action_mapping_arms_quit_and_a_second_press_quits() {
    let mut studio = hermetic();
    std::fs::write(
        studio.config_home().join("vimrc"),
        "nnoremap Z <Action:quit>\n",
    )
    .expect("quit mapping");
    enable_vim(&mut studio);
    studio.set_score("// one\n");
    studio.set_caret(0);

    studio.type_text("Z");
    assert!(!studio.wants_quit(), "the first press only arms quit");
    studio.type_text("Z");
    assert!(studio.wants_quit(), "the second press quits");
}

#[test]
fn mapping_prefixes_preserve_insert_text_and_native_shortcuts() {
    let mut studio = hermetic();
    std::fs::write(
        studio.config_home().join("vimrc"),
        "nnoremap <F24>x dd\ninoremap jj <Esc>\ninoremap jk <Esc>\n",
    )
    .expect("sequence mappings");
    enable_vim(&mut studio);
    studio.set_score("// ");
    studio.type_text("Atextjj");
    assert_eq!(studio.source(), "// text");
    studio.type_text("u");
    assert_eq!(studio.source(), "// ");

    studio.type_text("Aj");
    studio.repeat(KeyCode::F(1), KeyModifiers::NONE);
    assert_eq!(studio.source(), "// j");
    assert!(!studio.menu_open());
    studio.press(KeyCode::Esc, KeyModifiers::NONE);
    studio.type_text("u");
    assert_eq!(studio.source(), "// ");

    studio.type_text("Aja");
    assert_eq!(studio.source(), "// ja");
    studio.press(KeyCode::Esc, KeyModifiers::NONE);
    studio.type_text("u");
    assert_eq!(studio.source(), "// ");

    studio.type_text("Aj");
    studio.paste("jj");
    assert_eq!(studio.source(), "// jjj");
    studio.press(KeyCode::Esc, KeyModifiers::NONE);
    studio.set_score("// ");
    studio.type_text("Aj");
    studio.pump_until(
        Duration::from_secs(5),
        "prefix inserted after timeout",
        |studio| studio.source() == "// j",
    );
    studio.type_text("a");
    studio.press(KeyCode::Esc, KeyModifiers::NONE);
    assert_eq!(studio.source(), "// ja");
    studio.type_text("u");
    assert_eq!(studio.source(), "// ");

    studio.set_score("$: note(60).s(\"sine\")\n// ");
    studio.type_text("Aj");
    studio.chord("ctrl+s");
    studio.pump_until(
        Duration::from_secs(5),
        "native update after a prefix",
        |studio| studio.is_playing(),
    );
    assert_eq!(studio.source(), "$: note(60).s(\"sine\")\n// j");
    studio.press(KeyCode::Esc, KeyModifiers::NONE);

    studio.set_score("// first\n// second\n// third");
    studio.set_caret(0);
    studio.press(KeyCode::F(24), KeyModifiers::NONE);
    studio.type_text("jdd");
    assert_eq!(studio.source(), "// first\n// third");
    studio.type_text("u");
    studio.set_caret(0);
    studio.press(KeyCode::F(24), KeyModifiers::NONE);
    studio.type_text("x");
    assert_eq!(studio.source(), "// second\n// third");

    studio.attach_midi_controller("e2e pads");
    open_keybinding(&mut studio, "argument values, reference");
    studio.press(KeyCode::Enter, KeyModifiers::NONE);
    studio.midi_note_on("e2e pads", 0, 37, 100);
    studio.press(KeyCode::Esc, KeyModifiers::NONE);
    std::thread::sleep(Duration::from_millis(60));
    studio.set_score("// ");
    studio.type_text("Aj");
    studio.midi_note_on("e2e pads", 0, 37, 100);
    assert_eq!(studio.source(), "// j");
    assert_eq!(studio.focus(), Some(PanelKind::Reference));
}

/// A reload notice can repeat the last one. Clear it first, so the wait
/// below sees the notice for this write, not the one before it.
fn write_vimrc_reload(studio: &mut Hermetic, path: &std::path::Path, text: &str) {
    studio.clear_status();
    std::fs::write(path, text).expect("mapping file");
    studio.pump_until(Duration::from_secs(5), "Vim mappings reloaded", |studio| {
        studio.status() == "Vim mappings reloaded"
    });
}
