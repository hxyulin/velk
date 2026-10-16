/// Owned language-server configuration, independent of a transport ABI.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LanguageServer {
    pub transport: String,
    pub language: String,
    pub executable: String,
    pub arguments: Vec<String>,
    pub root: Option<String>,
    pub initialization_options: String,
    pub configuration: String,
}
