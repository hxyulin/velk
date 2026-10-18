//! Bundled matching provider, sharing its algorithm with native completion.
use velk_plugin_wasm_sdk::{InvalidScores, Matching, Request, Scores};

pub struct DefaultMatching;

impl Matching for DefaultMatching {
    fn rank(request: Request<'_>, scores: &mut Scores<'_>) -> Result<(), InvalidScores> {
        let mut matcher = velk_completion_matching::Matcher::new(request.query);
        for candidate in request.candidates() {
            scores.push(matcher.score(candidate).map(|score| score.fields()))?;
        }
        Ok(())
    }
}

velk_plugin_wasm_sdk::export_matching!(crate::DefaultMatching);

#[cfg(test)]
mod tests {
    use super::*;
    use velk_plugin_wasm_api as api;

    #[test]
    fn sdk_adapter_matches_native_algorithm() {
        let labels = ["print", "println", "Print", "世界接口", "", "a\0b"];
        for query in [
            "print", "PRINT", "pritn", "pr", "界接", "", "a\0b", "missing",
        ] {
            let mut input = vec![0; api::request_size(query, &labels).unwrap()];
            api::write_request(&mut input, query, &labels).unwrap();
            let mut output = vec![0; labels.len() * api::SCORE_BYTES];
            assert_eq!(
                velk_plugin_wasm_sdk::dispatch::<DefaultMatching>(&input, &mut output),
                labels.len() as i32
            );
            assert_eq!(
                api::read_scores(&output, labels.len()).unwrap(),
                velk_completion_matching::scores(query, &labels)
            );
        }
    }
}
