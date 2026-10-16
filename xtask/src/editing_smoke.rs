use super::*;
use velk_editing::{Intent, Role};
#[allow(unsafe_code)]
pub(super) fn run(output: &Path, pack: bool) -> Result<(), String> {
    let temp = tempfile::tempdir().map_err(|e| e.to_string())?;
    let specs = if pack {
        stage_builtins(output, temp.path())?
            .into_iter()
            .filter(|s| {
                matches!(
                    s.id.as_str(),
                    "velk.indentation" | "velk.autopairs" | "velk.vim"
                )
            })
            .collect()
    } else {
        vec![
            stage_plugin(
                &output.join("examples"),
                temp.path(),
                "velk.vim",
                "velk_plugin_vim",
            )?,
            stage_plugin(
                &output.join("examples"),
                temp.path(),
                "velk.indentation",
                "velk_plugin_indentation",
            )?,
            stage_plugin(
                &output.join("examples"),
                temp.path(),
                "velk.autopairs",
                "velk_plugin_autopairs",
            )?,
        ]
    };
    publish(temp.path(), specs)?;
    let mut manager = velk_plugin_host::Manager::new(temp.path().into());
    unsafe {
        manager.load()?;
    }
    let provider = manager.editing_provider("velk.indentation", Role::Indentation)?;
    let mut session = velk_editor_model::editing::Session::default();
    session.set_providers(std::collections::BTreeMap::from([(
        Role::Indentation,
        provider,
    )]));
    let mut buffer = velk_core::Buffer::new("ab");
    buffer.set_caret(2, false);
    assert!(session.evaluate(&mut buffer, Intent::Indent)?);
    assert_eq!(buffer.text().to_string(), "ab  ");
    buffer.undo();
    assert_eq!(buffer.text().to_string(), "ab");
    let mut brackets = velk_core::Buffer::new("{}");
    brackets.set_caret(1, false);
    assert!(session.evaluate(&mut brackets, Intent::Newline)?);
    assert_eq!(brackets.text().to_string(), "{\n    \n}");
    assert_eq!(brackets.selection().head, 6);
    manager.unload("velk.indentation");
    assert!(!session.evaluate(&mut buffer, Intent::Indent)?);
    manager.set_settings(std::collections::BTreeMap::from([(
        "velk.indentation".into(),
        std::collections::BTreeMap::from([("tab_width".into(), 2.into())]),
    )]));
    unsafe {
        manager.load()?;
    }
    let provider = manager.editing_provider("velk.indentation", Role::Indentation)?;
    session.set_providers(std::collections::BTreeMap::from([(
        Role::Indentation,
        provider,
    )]));
    buffer.set_caret(1, false);
    assert!(session.evaluate(&mut buffer, Intent::Indent)?);
    assert_eq!(buffer.text().to_string(), "a b");
    let provider = manager.editing_provider("velk.autopairs", Role::Pairing)?;
    let mut pairs = velk_editor_model::editing::Session::default();
    pairs.set_providers(std::collections::BTreeMap::from([(
        Role::Pairing,
        provider,
    )]));
    let mut paired = velk_core::Buffer::new("界");
    paired.set_caret(1, false);
    assert!(pairs.evaluate(&mut paired, Intent::Type("(".into()))?);
    assert_eq!(paired.text().to_string(), "界()");
    assert_eq!(paired.selection().head, 2);
    assert!(pairs.evaluate(&mut paired, Intent::Type(")".into()))?);
    assert_eq!(paired.text().to_string(), "界()");
    assert_eq!(paired.selection().head, 3);
    paired.set_caret(2, false);
    assert!(pairs.evaluate(&mut paired, Intent::Backspace)?);
    assert_eq!(paired.text().to_string(), "界");
    paired.undo();
    assert_eq!(paired.text().to_string(), "界()");
    assert!(!pairs.evaluate(&mut paired, Intent::Backspace)?);
    paired.undo();
    assert_eq!(paired.text().to_string(), "界");
    manager.unload("velk.autopairs");
    assert!(!pairs.evaluate(&mut paired, Intent::Type("(".into()))?);
    let mut modal = velk_editor_model::editing::Session::default();
    modal.set_providers(std::collections::BTreeMap::from([(
        Role::Keymap,
        manager.editing_provider("velk.vim", Role::Keymap)?,
    )]));
    let mut document = velk_core::Buffer::new("one two three");
    let input = |key: &str| {
        Intent::Key(velk_editing::input::Key {
            key: key.into(),
            text: !key.is_empty() && key != "Escape",
            control: false,
            alt: false,
            meta: false,
            state: Default::default(),
        })
    };
    for key in ["", "2", "w"] {
        assert!(modal.evaluate(&mut document, input(key))?);
    }
    assert_eq!(document.selection().head, 8);
    assert_eq!(modal.input_state().label, "NORMAL");
    assert!(modal.evaluate(&mut document, input("i"))?);
    assert!(!modal.evaluate(&mut document, input("hello"))?);
    assert_eq!(modal.input_state().label, "INSERT");
    document.insert("hello");
    assert!(modal.evaluate(&mut document, input("Escape"))?);
    assert!(modal.evaluate(&mut document, input("x"))?);
    for key in ["g", "g", "v", "w"] {
        assert!(modal.evaluate(&mut document, input(key))?);
    }
    assert_eq!(document.selected_text(), "one t");
    assert_eq!(modal.input_state().label, "VISUAL");
    assert_eq!(modal.caret_position(&document), 4);
    assert!(modal.evaluate(&mut document, input("c"))?);
    assert_eq!(modal.input_state().label, "INSERT");
    assert!(document.text().to_string().starts_with("wo"));
    assert!(modal.evaluate(&mut document, input("Escape"))?);
    assert!(modal.evaluate(&mut document, input("V"))?);
    assert_eq!(modal.input_state().label, "V-LINE");
    assert_eq!(document.selected_text(), document.text().to_string());
    manager.unload("velk.vim");
    assert!(!modal.evaluate(&mut document, input("x"))?);
    assert!(modal.input_state().label.is_empty());
    manager.unload_all();
    println!(
        "Editing provider smoke passed: pack={pack}, native Vim modes/visual selections/motions/state retirement, indentation/newlines/autopairs, undo, settings and stale lease fallback"
    );
    Ok(())
}
