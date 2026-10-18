use std::sync::Arc;
use velk_plugin_contracts::syntax::{Edit, Language};

#[allow(unsafe_code)]
pub(super) fn run() -> Result<(), String> {
    let mut manager = velk_plugin_host::Manager::new();
    // SAFETY: both artifacts are built locally from the bundled SDK provider.
    manager.load()?;
    let provider = manager.syntax_provider("velk.tree-sitter")?;
    assert!(provider.open(Language::new("custom-language")).is_err());
    let mut first = provider.open(Language::new("rust"))?;
    let mut second = provider.open(Language::new("rust"))?;
    let before = ropey::Rope::from_str("fn café() { let value = \"世界\"; }\n");
    assert!(first.update(0, before.clone(), &[], &|| false)?);
    let highlights = first.highlights(0..before.len_chars(), &|| false)?.unwrap();
    assert!(highlights.iter().any(|h| h.capture == "function"));
    assert!(highlights.iter().any(|h| h.capture == "string"));
    assert!(first.highlights(0..before.len_chars(), &|| true)?.is_none());
    assert!(
        first
            .highlights(0..before.len_chars() + 1, &|| false)
            .is_err()
    );
    let mut after = before.clone();
    after.insert(0, "// comment\n");
    let edit = Edit::between(1, &before, &after, 0..0, 11);
    assert!(first.update(1, after.clone(), &[edit], &|| false)?);
    assert!(second.update(8, after.clone(), &[], &|| false)?);
    assert_eq!(
        first.highlights(0..after.len_chars(), &|| false)?,
        second.highlights(0..after.len_chars(), &|| false)?
    );
    assert!(!first.update(2, before.clone(), &[], &|| true)?);
    manager.unload("velk.tree-sitter");
    assert!(provider.open(Language::new("rust")).is_err());
    assert!(first.highlights(0..after.len_chars(), &|| false).is_err());
    // Sessions remain destructible after logical retirement and pin their code.
    drop(first);
    drop(second);
    manager.load()?;
    let replacement = manager.syntax_provider("velk.tree-sitter")?;
    assert!(!Arc::ptr_eq(&provider, &replacement));
    let mut session = replacement.open(Language::new("rust"))?;
    assert!(session.update(0, before.clone(), &[], &|| false)?);
    assert_eq!(
        session
            .highlights(0..before.len_chars(), &|| false)?
            .unwrap(),
        highlights
    );
    drop(manager);
    assert!(
        session
            .highlights(0..before.len_chars(), &|| false)
            .is_err()
    );
    drop(session);
    println!(
        "Syntax service smoke passed: independent sessions, Unicode captures, incremental edits, cancellation, invalid ranges, retirement, replacement and teardown after unload"
    );
    Ok(())
}
