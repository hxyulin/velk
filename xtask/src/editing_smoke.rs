use super::*;
use velk_editing::{Intent, Role};
#[allow(unsafe_code)]
pub(super) fn run(output: &Path, pack: bool) -> Result<(), String> {
    let temp = tempfile::tempdir().map_err(|e| e.to_string())?;
    let specs = if pack {
        stage_builtins(output, temp.path())?
            .into_iter()
            .filter(|s| s.id == "velk.indentation")
            .collect()
    } else {
        vec![stage_plugin(
            &output.join("examples"),
            temp.path(),
            "velk.indentation",
            "velk_plugin_indentation",
        )?]
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
    manager.unload_all();
    println!(
        "Editing provider smoke passed: pack={pack}, native indentation, undo, settings and stale lease fallback"
    );
    Ok(())
}
