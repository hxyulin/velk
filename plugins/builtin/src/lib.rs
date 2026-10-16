//! Built-ins share one link; each retains its own identity and lifecycle.
velk_plugin_sdk::export_plugin_pack!(
    velk_plugin_markdown::CommonMark,
    velk_plugin_lsp::Lsp,
    velk_plugin_rust_analyzer::RustAnalyzer,
    velk_plugin_search::Search,
    velk_plugin_files::Files,
    velk_plugin_terminal::Terminal,
    velk_plugin_indentation::Indentation,
    velk_plugin_autopairs::Autopairs,
);
