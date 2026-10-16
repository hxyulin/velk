use super::*;
use velk_editing::{Intent, Role};
#[allow(unsafe_code)]
pub(super) fn run(output: &Path, pack: bool) -> Result<(), String> {
    let temp = tempfile::tempdir().map_err(|e| e.to_string())?;
    let specs = if pack {
        stage_builtins(output, temp.path())?
            .into_iter()
            .filter(|s| matches!(s.id.as_str(), "velk.indentation" | "velk.autopairs"))
            .collect()
    } else {
        vec![
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
    paired.undo();
    assert_eq!(paired.text().to_string(), "界");
    manager.unload("velk.autopairs");
    assert!(!pairs.evaluate(&mut paired, Intent::Type("(".into()))?);
    manager.unload_all();
    println!(
        "Editing provider smoke passed: pack={pack}, native indentation/newlines/autopairs, undo, settings and stale lease fallback"
    );
    Ok(())
}
