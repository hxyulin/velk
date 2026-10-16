use velk_plugin_contracts::{PluginMetadata, settings};

#[test]
fn existing_metadata_json_keeps_its_shape_without_native_or_sdk_types() {
    let payload = serde_json::json!({
        "name": "Example",
        "readme": "# Example",
        "capabilities": [{
            "id": "editor.buffer.read",
            "reason": "Read supplied document snapshots.",
            "optional": false
        }]
    });
    let mut metadata: PluginMetadata = serde_json::from_value(payload.clone()).unwrap();
    metadata.validate().unwrap();
    assert!(metadata.settings.is_empty());
    assert_eq!(serde_json::to_value(&metadata).unwrap(), payload);

    metadata.settings.push(settings::Definition {
        key: "enabled".into(),
        title: "Enabled".into(),
        description: "Enable the feature.".into(),
        kind: settings::Kind::Boolean,
        default: true.into(),
    });
    metadata.validate().unwrap();
    let encoded = serde_json::to_vec(&metadata).unwrap();
    let decoded: PluginMetadata = serde_json::from_slice(&encoded).unwrap();
    assert_eq!(decoded, metadata);
    assert_eq!(
        settings::resolve(&decoded.settings, &settings::Values::new()).unwrap(),
        settings::Values::from([("enabled".into(), true.into())])
    );
}
