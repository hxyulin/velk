use velk_editing::{Intent, Role};
#[allow(unsafe_code)]
pub(super) fn run() -> Result<(), String> {
    let mut manager = velk_plugin_host::Manager::new();
    manager.load()?;
    let provider = manager.editing_provider("velk.indentation", Role::Indentation)?;
    let mut session = velk_editor_model::editing::Session::default();
    session.set_providers(std::collections::BTreeMap::from([(
        Role::Indentation,
        provider,
    )]));
    let mut buffer =
        velk_core::Buffer::with_history("ab", std::sync::Arc::new(velk_undo_tree::Tree));
    buffer.set_caret(2, false);
    assert!(session.evaluate(&mut buffer, Intent::Indent)?);
    assert_eq!(buffer.text().to_string(), "ab  ");
    buffer.undo();
    assert_eq!(buffer.text().to_string(), "ab");
    let mut brackets =
        velk_core::Buffer::with_history("{}", std::sync::Arc::new(velk_undo_tree::Tree));
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
    manager.load()?;
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
    let mut paired =
        velk_core::Buffer::with_history("界", std::sync::Arc::new(velk_undo_tree::Tree));
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
    let mut document =
        velk_core::Buffer::with_history("one two three", std::sync::Arc::new(velk_undo_tree::Tree));
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
    assert!(modal.evaluate(&mut document, input("Escape"))?);
    let mut block = velk_editor_model::editing::Session::default();
    block.set_providers(std::collections::BTreeMap::from([(
        Role::Keymap,
        manager.editing_provider("velk.vim", Role::Keymap)?,
    )]));
    let mut rectangle =
        velk_core::Buffer::with_history("abcd\nefgh", std::sync::Arc::new(velk_undo_tree::Tree));
    rectangle.set_caret(1, false);
    assert!(block.evaluate(&mut rectangle, input(""))?);
    let control = Intent::Key(velk_editing::input::Key {
        key: "v".into(),
        text: false,
        control: true,
        alt: false,
        meta: false,
        state: Default::default(),
    });
    assert!(block.evaluate(&mut rectangle, control)?);
    for key in ["l", "j", "c"] {
        assert!(block.evaluate(&mut rectangle, input(key))?);
    }
    assert!(block.has_multiple_carets());
    assert!(block.edit_command(&mut rectangle, &velk_editor_model::EditCommand::Insert("X"))?);
    assert_eq!(rectangle.text().to_string(), "aXd\neXh");
    assert!(block.evaluate(&mut rectangle, input("Escape"))?);
    assert!(!block.has_multiple_carets());
    manager.unload("velk.vim");
    assert!(!modal.evaluate(&mut document, input("x"))?);
    assert!(modal.input_state().label.is_empty());
    manager.unload_all();
    println!(
        "Editing provider smoke passed: native Vim modes/block selections/multiple carets/motions/state retirement, indentation/newlines/autopairs, undo, settings and stale lease fallback"
    );
    Ok(())
}
