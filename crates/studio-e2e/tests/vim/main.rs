//! Vim editing, saved preferences and shortcut ownership.

#[global_allocator]
static AUDIO_CALLBACK_ALLOCATOR: rustel_audio::tripwire::TripwireAlloc =
    rustel_audio::tripwire::TripwireAlloc;

use crossterm::event::{KeyCode, KeyModifiers, ModifierKeyCode};
use rustel_studio::editor::KeyboardCapabilities;
use rustel_studio_e2e::{Hermetic, PanelKind, hermetic, hermetic_with, reopen_over};

fn open_editor_keybindings(studio: &mut Hermetic) {
    studio.chord("ctrl+o");
    for _ in 0..9 {
        studio.press(KeyCode::Home, KeyModifiers::NONE);
        if studio.rows().iter().any(|row| row.contains("▸ Terminal")) {
            studio.press(KeyCode::Down, KeyModifiers::NONE);
            assert!(
                studio
                    .rows()
                    .iter()
                    .any(|row| row.contains("▸ Editor keybindings"))
            );
            return;
        }
        studio.press(KeyCode::Tab, KeyModifiers::NONE);
    }
    panic!("the editor keybindings row is missing");
}

fn enable_vim(studio: &mut Hermetic) {
    open_editor_keybindings(studio);
    studio.press(KeyCode::Right, KeyModifiers::NONE);
    studio.press(KeyCode::Esc, KeyModifiers::NONE);
}

fn open_keybinding(studio: &mut Hermetic, action: &str) -> String {
    open_editor_keybindings(studio);
    let selected = format!("▸ {action}");
    for _ in 0..60 {
        if let Some(row) = studio
            .rows()
            .into_iter()
            .find(|row| row.contains(&selected))
        {
            return row;
        }
        studio.press(KeyCode::Down, KeyModifiers::NONE);
    }
    panic!("Keybinds is missing {action}");
}

fn ex(studio: &mut Hermetic, command: &str) {
    studio.type_text(command);
    studio.press(KeyCode::Enter, KeyModifiers::NONE);
}

#[test]
fn vim_modes_and_commands_stay_visible_beside_a_diagnostic() {
    let mut studio = hermetic();
    enable_vim(&mut studio);
    studio.set_score("$: s(\"bd\"");
    studio.settle();

    for (width, show_footer) in [(150, true), (80, true), (80, false)] {
        if !show_footer {
            studio.press(KeyCode::F(1), KeyModifiers::NONE);
            studio.type_text("of");
        }
        studio.resize(width, 24);
        for (keys, label) in [("", "NORMAL"), ("i", "INSERT"), (":2", ":2")] {
            studio.type_text(keys);
            let rows = studio.rows();
            assert!(
                rows.iter()
                    .any(|row| row.contains(&format!("VIM {label}")) && row.contains("✗ Expected")),
                "{width} columns, {label}:\n{}",
                rows.join("\n")
            );
            studio.press(KeyCode::Esc, KeyModifiers::NONE);
        }
    }
    studio.press(KeyCode::F(12), KeyModifiers::NONE);
    let screen = studio.rows().join("\n");
    assert!(
        screen.contains("PIANO") && !screen.contains("VIM"),
        "{screen}"
    );
    studio.press(KeyCode::Esc, KeyModifiers::NONE);
    assert!(studio.rows().iter().any(|row| row.contains("VIM NORMAL")));
}

#[test]
fn editor_keybindings_default_to_typing_and_vim_survives_reopening() {
    let mut studio = hermetic();
    open_editor_keybindings(&mut studio);
    assert!(
        studio
            .rows()
            .iter()
            .any(|row| row.contains("Editor keybindings: Default"))
    );
    studio.press(KeyCode::Right, KeyModifiers::NONE);
    studio.press(KeyCode::Esc, KeyModifiers::NONE);

    std::thread::sleep(std::time::Duration::from_millis(700));
    studio.pump();
    let preferences: serde_json::Value = serde_json::from_slice(
        &std::fs::read(studio.config_home().join("studio.json")).expect("saved preferences"),
    )
    .expect("valid preferences");
    assert_eq!(preferences["editor_keybindings"], "vim");
    studio.quit();
    let mut studio = reopen_over(studio);

    open_editor_keybindings(&mut studio);
    assert!(
        studio
            .rows()
            .iter()
            .any(|row| row.contains("Editor keybindings: Vim"))
    );
    studio.press(KeyCode::Esc, KeyModifiers::NONE);
    studio.set_score("// first\n// second\n");
    studio.set_caret(0);
    studio.type_text("dd");
    assert_eq!(studio.source(), "// second\n");

    open_editor_keybindings(&mut studio);
    studio.press(KeyCode::Delete, KeyModifiers::NONE);
    studio.press(KeyCode::Esc, KeyModifiers::NONE);
    studio.set_score("// ");
    studio.set_caret(3);
    studio.type_text("hjkl");
    assert_eq!(studio.source(), "// hjkl");
}

#[test]
fn motions_line_commands_and_insert_edits_share_undo_history() {
    let mut studio = hermetic();
    enable_vim(&mut studio);
    studio.set_score("// last");
    studio.set_caret(0);
    let (column, row) = studio.cursor().expect("the editor caret is visible");
    for modifiers in [KeyModifiers::NONE, KeyModifiers::CONTROL] {
        studio.press(KeyCode::End, modifiers);
        assert_eq!(studio.cursor(), Some((column + 6, row)));
        studio.press(KeyCode::Home, KeyModifiers::CONTROL);
    }

    let source = "// alpha\n// bravo\n// charlie\n// delta\n";
    studio.set_score(source);
    studio.set_caret(0);
    let (column, row) = studio.cursor().expect("the editor caret is visible");

    studio.type_text("l");
    assert_eq!(studio.cursor(), Some((column + 1, row)));
    studio.type_text("j");
    assert_eq!(studio.cursor(), Some((column + 1, row + 1)));
    studio.type_text("kh");
    assert_eq!(studio.cursor(), Some((column, row)));
    studio.type_text("2j");
    assert_eq!(studio.cursor(), Some((column, row + 2)));
    assert_eq!(studio.source(), source);

    studio.type_text("dd");
    assert_eq!(studio.source(), "// alpha\n// bravo\n// delta\n");
    studio.type_text("u");
    assert_eq!(studio.source(), source);
    ex(&mut studio, ":2");
    studio.type_text("2dd");
    assert_eq!(studio.source(), "// alpha\n// delta\n");
    studio.type_text("u");
    assert_eq!(studio.source(), source);

    ex(&mut studio, ":3");
    studio.type_text("i// added ");
    studio.press(KeyCode::Esc, KeyModifiers::NONE);
    let edited = "// alpha\n// bravo\n// added // charlie\n// delta\n";
    assert_eq!(studio.source(), edited);
    studio.type_text("u");
    assert_eq!(
        studio.source(),
        source,
        "one undo removes the insert session"
    );
    studio.chord("ctrl+r");
    assert_eq!(studio.source(), edited);
    studio.type_text("uv");
    studio.chord("ctrl+r");
    assert_eq!(studio.source(), edited);
    studio.type_text("u");
    ex(&mut studio, ":redo");
    assert_eq!(studio.source(), edited);

    let source = "// a b\n  // second\n";
    studio.set_score(source);
    studio.set_caret(3);
    studio.type_text("cwx");
    studio.press(KeyCode::Esc, KeyModifiers::NONE);
    assert_eq!(studio.source(), "// x b\n  // second\n");
    studio.type_text("u");
    assert_eq!(studio.source(), source);
    ex(&mut studio, ":2");
    studio.type_text("cc// changed");
    studio.press(KeyCode::Esc, KeyModifiers::NONE);
    assert_eq!(studio.source(), "// a b\n  // changed\n");
    studio.type_text("u");
    assert_eq!(studio.source(), source);
    studio.type_text("yyp");
    assert_eq!(studio.source(), "// a b\n  // second\n  // second\n");
    studio.type_text("u");
    assert_eq!(studio.source(), source);
}

#[test]
fn a_normal_mode_paste_is_literal_and_undoes_as_one_edit() {
    let mut studio = hermetic();
    enable_vim(&mut studio);
    let source = "// score\n";
    studio.set_score(source);
    studio.set_caret(0);

    studio.type_text("d");
    studio.paste("// dd hjkl :2\r\n// pasted\n");
    assert_eq!(studio.source(), "// dd hjkl :2\n// pasted\n// score\n");
    studio.type_text("u");
    assert_eq!(studio.source(), source);
    studio.type_text("d");
    assert_eq!(
        studio.source(),
        source,
        "paste cancelled the pending delete"
    );
    studio.press(KeyCode::Esc, KeyModifiers::NONE);
}

#[test]
fn final_lines_and_graphemes_survive_delete_put_and_visual_paste() {
    let mut studio = hermetic();
    enable_vim(&mut studio);
    for put in ["p", "P"] {
        let source = "// only";
        studio.set_score(source);
        studio.set_caret(0);
        studio.type_text("dd");
        assert_eq!(studio.source(), "");
        studio.type_text(put);
        assert_eq!(studio.source(), source, "{put}");
        studio.type_text("u");
        assert_eq!(studio.source(), "", "undo {put}");
        studio.type_text("u");
        assert_eq!(studio.source(), source, "undo dd");
    }
    let source = "// top\n// e\u{301}🎹x";
    studio.set_score(source);
    studio.set_caret(0);

    studio.type_text("Gdd");
    assert_eq!(studio.source(), "// top");
    studio.type_text("p");
    assert_eq!(studio.source(), source);
    studio.type_text("03lx");
    assert_eq!(studio.source(), "// top\n// 🎹x");
    studio.type_text("u");
    assert_eq!(studio.source(), source);

    studio.type_text("vl");
    studio.paste("z");
    assert_eq!(studio.source(), "// top\n// zx");
    studio.type_text("hx");
    assert_eq!(studio.source(), "// top\n// x");

    let source = "$: note(60).s(\"sine\").gain(slider(0.8,0,1,0.1))";
    studio.set_score(source);
    studio.chord("ctrl+s");
    studio.settle();
    for key in [KeyCode::Char('x'), KeyCode::Delete] {
        studio.set_caret(source.find("slider(").expect("the slider call"));
        studio.press(key, KeyModifiers::NONE);
        assert_eq!(studio.source(), "$: note(60).s(\"sine\").gain()", "{key:?}");
        studio.type_text("u");
        assert_eq!(studio.source(), source);
        studio.chord("ctrl+s");
        studio.settle();
    }
    studio.set_caret(source.find("slider(").expect("the slider call"));
    studio.type_text("dx");
    assert_eq!(studio.source(), source);
    studio.type_text("2x");
    assert_eq!(studio.source(), "$: note(60).s(\"sine\").gain(");
    studio.type_text("u");
    assert_eq!(studio.source(), source);
}

#[test]
fn studio_shortcuts_and_panel_focus_keep_their_meaning() {
    let mut studio = hermetic();
    enable_vim(&mut studio);
    let source = "$: s(\"bd\")\n";
    studio.set_score(source);
    studio.set_caret(0);

    studio.press(KeyCode::Char('ø'), KeyModifiers::ALT);
    assert_eq!(studio.source(), source);
    studio.type_text("i");
    studio.press(KeyCode::Char('ø'), KeyModifiers::ALT);
    studio.press(KeyCode::Esc, KeyModifiers::NONE);
    assert_eq!(studio.source(), "ø$: s(\"bd\")\n");
    studio.type_text("u");
    assert_eq!(studio.source(), source);

    studio.type_text("d");
    studio.press(KeyCode::F(1), KeyModifiers::NONE);
    studio.type_text("i9");
    studio.press(KeyCode::Esc, KeyModifiers::NONE);
    assert_eq!(studio.source(), "9$: s(\"bd\")\n");
    studio.type_text("u");
    assert_eq!(studio.source(), source);
    studio.press(KeyCode::F(1), KeyModifiers::NONE);
    studio.type_text("s");
    let rename = studio
        .rows()
        .into_iter()
        .find(|row| row.contains("Rename scene"))
        .expect("Scene menu has Rename scene");
    assert!(!rename.contains("^R"), "{rename}");
    studio.type_text("r");
    studio.type_text("dd");
    studio.press(KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(studio.scene_names(), vec!["dd"]);
    assert_eq!(studio.source(), source);
    studio.type_text("d");
    assert_eq!(
        studio.source(),
        source,
        "rename cancelled the pending delete"
    );
    studio.press(KeyCode::Esc, KeyModifiers::NONE);

    studio.chord("ctrl+o");
    assert_eq!(studio.focus(), Some(PanelKind::Settings));
    studio.type_text("hjkl");
    assert_eq!(studio.source(), source);
    studio.chord("ctrl+s");
    studio.settle();
    assert!(
        studio.is_playing(),
        "update still works from the settings sheet"
    );
    studio.chord("ctrl+g");
    studio.chord("ctrl+g");
    studio.settle();
    assert!(!studio.is_playing() && !studio.is_stopping());
    studio.press(KeyCode::Esc, KeyModifiers::NONE);

    studio.set_caret(4);
    studio.chord("ctrl+d");
    assert_eq!(studio.focus(), Some(PanelKind::Reference));
    assert_eq!(studio.status(), "reference - s");
    let (row, text) = studio
        .rows()
        .into_iter()
        .enumerate()
        .find(|(_, row)| row.contains(source.trim()))
        .expect("the score is visible beside the reference");
    let column = text[..text.find("$:").unwrap()].chars().count();
    studio.click(column as u16, row as u16);
    assert_eq!(studio.focus(), None);
    ex(&mut studio, ":1");
    studio.type_text("i9");
    studio.press(KeyCode::Esc, KeyModifiers::NONE);
    assert_eq!(studio.source(), "9$: s(\"bd\")\n");
    studio.type_text("u");
    assert_eq!(studio.source(), source);
    assert_eq!(studio.reference_tab(), Some("reference"));

    studio.set_caret(4);
    studio.type_text("i");
    studio.press(KeyCode::Esc, KeyModifiers::NONE);
    assert_eq!(studio.source(), source);
    assert_eq!(
        studio.reference_tab(),
        Some("reference"),
        "leaving an empty Insert session keeps the reference open"
    );
    studio.set_caret(4);
    studio.type_text("i9");
    studio.press(KeyCode::Esc, KeyModifiers::NONE);
    assert_eq!(studio.source(), "$: s9(\"bd\")\n");
    assert_eq!(
        studio.reference_tab(),
        Some("reference"),
        "leaving Insert keeps the reference open"
    );
    studio.type_text("u");
    assert_eq!(studio.source(), source);
}

#[test]
fn vim_controls_leave_scene_actions_reachable() {
    let mut studio = hermetic_with(KeyboardCapabilities::enhanced());
    enable_vim(&mut studio);
    studio.set_score("// first\n");
    studio.chord("ctrl+n");
    assert!(studio.status().contains("Scene > Rename scene"));
    studio.set_score("// second\n");
    studio.set_caret(0);
    studio.type_text("i");
    studio.chord("ctrl+r");
    studio.type_text("// ");
    studio.press(KeyCode::Char('['), KeyModifiers::CONTROL);
    assert_eq!(studio.source(), "// // second\n");
    assert_eq!(studio.current_scene().as_deref(), Some("scene 1"));
    studio.type_text("dd");
    assert_eq!(studio.source(), "");
    studio.press(KeyCode::F(6), KeyModifiers::NONE);
    assert_eq!(studio.source(), "// first\n");

    let rename = open_keybinding(&mut studio, "rename scene");
    assert!(!rename.contains("^R"), "{rename}");
    studio.press(KeyCode::Enter, KeyModifiers::NONE);
    studio.chord("ctrl+r");
    studio.press(KeyCode::Esc, KeyModifiers::NONE);
    studio.set_caret(0);
    studio.type_text("ddu");
    studio.chord("ctrl+r");
    assert_eq!(studio.source(), "");
    assert_eq!(studio.current_scene().as_deref(), Some("first"));
    studio.type_text("u");
    studio.chord("ctrl+o");
    studio.chord("ctrl+r");
    studio.type_text("custom");
    studio.press(KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(studio.current_scene().as_deref(), Some("custom"));
    studio.press(KeyCode::Esc, KeyModifiers::NONE);

    let rename = open_keybinding(&mut studio, "rename scene");
    assert!(rename.contains("^R"), "{rename}");
    studio.press(KeyCode::Enter, KeyModifiers::NONE);
    studio.press(KeyCode::F(9), KeyModifiers::CONTROL | KeyModifiers::SHIFT);
    studio.press(KeyCode::Esc, KeyModifiers::NONE);
    studio.press(KeyCode::F(1), KeyModifiers::NONE);
    studio.type_text("s");
    let rename = studio
        .rows()
        .into_iter()
        .find(|row| row.contains("Rename scene"))
        .expect("Scene menu has Rename scene");
    assert!(rename.contains("F9") && !rename.contains("^R"), "{rename}");
    studio.press(KeyCode::Esc, KeyModifiers::NONE);
    studio.press(KeyCode::Esc, KeyModifiers::NONE);
    studio.press(KeyCode::F(9), KeyModifiers::CONTROL | KeyModifiers::SHIFT);
    studio.type_text("renamed");
    studio.press(KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(studio.current_scene().as_deref(), Some("renamed"));
    assert_eq!(studio.source(), "// first\n");

    open_keybinding(&mut studio, "undo");
    studio.press(KeyCode::Enter, KeyModifiers::NONE);
    studio.chord("ctrl+r");
    studio.press(KeyCode::Esc, KeyModifiers::NONE);
    studio.set_caret(0);
    studio.type_text("dd");
    assert_eq!(studio.source(), "");
    studio.chord("ctrl+r");
    assert_eq!(
        studio.source(),
        "",
        "the Studio override cannot replace Vim redo"
    );
    studio.type_text("u");
    assert_eq!(studio.source(), "// first\n");
    studio.chord("ctrl+r");
    assert_eq!(studio.source(), "");
}

#[test]
fn visual_lines_and_text_objects_edit_complete_ranges() {
    let mut studio = hermetic();
    enable_vim(&mut studio);
    let lines = "// one\n// two\n// three";
    studio.set_score(lines);
    studio.set_caret(0);
    studio.type_text("GD");
    assert_eq!(studio.source(), "// one\n// two\n");
    studio.type_text("uggVjd");
    assert_eq!(studio.source(), "// three");
    studio.type_text("u");
    studio.set_caret(3);
    studio.type_text("Cnew");
    studio.press(KeyCode::Esc, KeyModifiers::NONE);
    assert_eq!(studio.source(), "// new\n// two\n// three");
    studio.type_text("u");
    assert_eq!(studio.source(), lines);

    for (source, inside, keys, expected) in [
        (
            "// outer(inner(é🎹), tail)",
            "é",
            "vi(d",
            "// outer(inner(), tail)",
        ),
        (
            "// outer(inner(é🎹), tail)",
            "é",
            "vi)d",
            "// outer(inner(), tail)",
        ),
        ("// outer(inner(é🎹), tail)", "é", "d2i(", "// outer()"),
        (
            "// outer(inner(é🎹), tail)",
            "é",
            "da(",
            "// outer(inner, tail)",
        ),
        ("// [a, {b: é🎹}]", "é", "di}", "// [a, {}]"),
        ("// [a, {b: é🎹}]", "é", "va]d", "// "),
        ("// one [two] three", "one", "vi[d", "// one [] three"),
        ("// one {two} three", "one", "di{", "// one {} three"),
        ("// one <two> three", "one", "di<", "// one <> three"),
        ("// one ((two)) three", "one", "d2i(", "// one (()) three"),
        (
            "// one (a) (b(c)) three",
            "one",
            "d2i(",
            "// one (a) (b()) three",
        ),
        (r#"// "bd \"sd\" é🎹" tail"#, "é", "vi\"d", r#"// "" tail"#),
        (r#"// "bd"  tail"#, "bd", "va\"d", "// tail"),
        (
            r#"// "two" three "four""#,
            "three",
            "vi\"d",
            r#"// "two""four""#,
        ),
        (
            r#"// "two" three "four""#,
            "three",
            "da\"",
            r#"// "twofour""#,
        ),
        ("// 'é🎹'", "é", "yi'P", "// 'é🎹é🎹'"),
        ("// `é🎹`", "é", "di`", "// ``"),
        ("// call(\n  é🎹\n)", "é", "di(", "// call()"),
        (r#"// call(é🎹, ")")"#, "é", "di(", "// call()"),
        (r#"// call("foo ) bar")"#, "foo", "di(", "// call()"),
        (r#"// call("foo ( bar")"#, "foo", "di(", "// call()"),
        (r#"// call("(foo)")"#, "foo", "di(", r#"// call("()")"#),
        (r#"// s("[é🎹]")"#, "é", "di[", r#"// s("[]")"#),
    ] {
        studio.set_score(source);
        studio.set_caret(source.find(inside).expect("text object caret"));
        studio.type_text(keys);
        assert_eq!(studio.source(), expected, "{keys} in {source}");
        studio.type_text("u");
        assert_eq!(studio.source(), source, "undo {keys}");
    }

    let source = r#"// play("bd \"sd\" é🎹")"#;
    studio.set_score(source);
    studio.set_caret(source.find("é").expect("quoted caret"));
    studio.type_text("ci\"hh");
    studio.press(KeyCode::Esc, KeyModifiers::NONE);
    assert_eq!(studio.source(), r#"// play("hh")"#);
    studio.type_text("u");
    assert_eq!(studio.source(), source);

    studio.set_score("// missing");
    studio.set_caret(3);
    studio.type_text("di(");
    assert_eq!(studio.source(), "// missing");
    studio.type_text("x");
    assert_eq!(studio.source(), "// issing");
}

mod mappings;
mod search;
