//! Features linked into the editor executable.

pub fn entries() -> Vec<velk_plugin_sdk::api::PluginApi> {
    vec![
        velk_plugin_sdk::builtin::<velk_plugin_rust::Rust>(),
        velk_plugin_sdk::builtin::<velk_plugin_snippets::Snippets>(),
        velk_plugin_sdk::builtin::<velk_plugin_completion::Completion>(),
        velk_plugin_sdk::builtin::<velk_plugin_completion::frontend::CompactFrontend>(),
        velk_plugin_sdk::builtin::<velk_plugin_completion::Words>(),
        velk_plugin_sdk::builtin::<velk_plugin_undo::Undo>(),
        velk_plugin_sdk::builtin::<velk_plugin_markdown::CommonMark>(),
        velk_plugin_sdk::builtin::<velk_plugin_tree_sitter::TreeSitter>(),
        velk_plugin_sdk::builtin::<velk_plugin_problems::Problems>(),
        velk_plugin_sdk::builtin::<velk_plugin_lsp::Lsp>(),
        velk_plugin_sdk::builtin::<velk_plugin_rust_analyzer::RustAnalyzer>(),
        velk_plugin_sdk::builtin::<velk_plugin_search::Search>(),
        velk_plugin_sdk::builtin::<velk_plugin_files::FilesBackend>(),
        velk_plugin_sdk::builtin::<velk_plugin_files::Files>(),
        velk_plugin_sdk::builtin::<velk_plugin_project::FileIndex>(),
        velk_plugin_sdk::builtin::<velk_plugin_project::WorkspaceSearch>(),
        velk_plugin_sdk::builtin::<velk_plugin_terminal::Terminal>(),
        velk_plugin_sdk::builtin::<velk_plugin_vim::Vim>(),
        velk_plugin_sdk::builtin::<velk_plugin_indentation::Indentation>(),
        velk_plugin_sdk::builtin::<velk_plugin_autopairs::Autopairs>(),
        velk_plugin_sdk::builtin::<velk_plugin_autosave::Autosave>(),
        velk_plugin_sdk::builtin::<velk_plugin_project::QuickOpen>(),
        velk_plugin_sdk::builtin::<velk_plugin_project::WorkspaceFind>(),
    ]
}
