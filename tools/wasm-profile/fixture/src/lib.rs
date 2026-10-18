use velk_plugin_sdk::{Plugin, Registrar};

#[derive(Debug, Default)]
pub struct Matching;

impl Plugin for Matching {
    const ID: &'static str = "velk.matching.profile";
    const VERSION: &'static str = "0.0.0-dev";

    fn register(registrar: &mut Registrar<Self>) -> Result<(), String> {
        registrar.matching::<Self>()
    }

    fn describe(&mut self) -> String {
        "Matching-only measurement fixture".into()
    }
}

impl velk_plugin_sdk::commands::completion::matching::Policy for Matching {
    fn scores(&self, query: &str, candidates: &[&str]) -> Result<Vec<Option<[u64; 4]>>, String> {
        Ok(velk_completion_matching::scores(query, candidates))
    }
}

velk_plugin_sdk::export_plugin!(Matching);
