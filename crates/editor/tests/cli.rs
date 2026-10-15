use std::process::Command;

#[test]
fn rejects_missing_and_extra_paths_before_starting_the_ui() {
    for arguments in [vec![], vec!["one.txt", "two.txt"]] {
        let output = Command::new(env!("CARGO_BIN_EXE_velk"))
            .args(arguments)
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(2));
        assert!(output.stdout.is_empty());
        assert!(
            String::from_utf8(output.stderr)
                .unwrap()
                .contains("Usage: velk <file>")
        );
    }
}

#[test]
fn reports_missing_files_and_invalid_utf8_without_creating_or_modifying_files() {
    let directory = tempfile::tempdir().unwrap();
    let missing = directory.path().join("missing file.txt");
    let invalid = directory.path().join("invalid.txt");
    std::fs::write(&invalid, [0xff, 0xfe]).unwrap();
    for path in [&missing, &invalid] {
        let output = Command::new(env!("CARGO_BIN_EXE_velk"))
            .arg(path)
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(1));
        assert!(output.stdout.is_empty());
        let error = String::from_utf8(output.stderr).unwrap();
        assert!(error.contains("cannot open"), "{error}");
        assert!(error.contains(path.to_str().unwrap()), "{error}");
    }
    assert!(!missing.exists());
    assert_eq!(std::fs::read(&invalid).unwrap(), [0xff, 0xfe]);
}
