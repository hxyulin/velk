use velk_plugin_contracts::{PluginMetadata, settings};

#[test]
fn metadata_round_trips_commands_and_settings_without_native_or_sdk_types() {
    let payload = serde_json::json!({
        "name": "Example",
        "commands": [{"id":"example.definition","title":"Go to definition","language":"rust","input":null,"shortcut":null,"handler":{"kind":"semantic","payload":{"operation":"definition"}}}],
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
    assert_eq!(metadata.commands.len(), 1);
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

#[test]
fn command_ids_are_unique_within_plugin_metadata() {
    let mut metadata = PluginMetadata {
        name: "Commands".into(),
        commands: vec![velk_plugin_contracts::commands::Declaration {
            input: None,
            shortcut: None,
            id: "test.command".into(),
            title: "Command".into(),
            language: None,
            handler: velk_plugin_contracts::commands::Handler::Request {
                hook: "test.command".into(),
            },
        }],
        ..Default::default()
    };
    metadata.validate().unwrap();
    metadata.commands.push(metadata.commands[0].clone());
    assert!(metadata.validate().is_err());
}

#[test]
fn optional_contributions_are_validated_independently_of_commands() {
    use velk_plugin_contracts::commands::{Declaration, Handler, contributions::Contribution};
    let mut metadata = PluginMetadata {
        name: "Source".into(),
        contributions: vec![Contribution {
            topic: "test.completion".into(),
            command: Declaration {
                id: "test.words".into(),
                title: "Words".into(),
                input: None,
                shortcut: None,
                language: None,
                handler: Handler::Request {
                    hook: "test.words".into(),
                },
            },
        }],
        ..Default::default()
    };
    metadata.validate().unwrap();
    metadata
        .contributions
        .push(metadata.contributions[0].clone());
    assert!(metadata.validate().is_err());
}
