//! Project query backends return locations and source previews, never picker UI.
use serde::{Deserialize, Serialize};
use velk_commands::Location;
pub const FILES_PROVIDER: &str = "velk.project.files";
pub const SEARCH_PROVIDER: &str = "velk.project.search";
pub const HOOK: &str = "velk.project.query";
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Request {
    Begin { session: u64, id: u64, query: Query },
    Poll { id: u64 },
    Cancel { id: u64 },
}
pub use velk_search::Query;
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Match {
    pub location: Location,
    /// Project-relative display path. The location remains authoritative.
    pub relative: String,
    pub preview: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Reply {
    Pending,
    Cancelled,
    Ready { matches: Vec<Match>, notice: String },
}
