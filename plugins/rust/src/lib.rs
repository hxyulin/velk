use velk_plugin_sdk::{Plugin, file_types::FileType};
#[derive(Default)]
pub struct Rust;
impl Plugin for Rust {
    const ID: &'static str = "velk.language.rust";
    const VERSION: &'static str = env!("CARGO_PKG_VERSION");
    const NAME: &'static str = "Rust";
    const README: &'static str = include_str!("../README.md");
    fn file_types() -> Vec<FileType> {
        vec![FileType {
            id: "rust".into(),
            name: "Rust".into(),
            extensions: vec!["rs".into()],
            filenames: vec![],
        }]
    }
    fn describe(&mut self) -> String {
        "Rust file identity and associations".into()
    }
}
