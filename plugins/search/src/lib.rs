//! Literal buffer search. Matching runs on one joined worker, never the UI or
//! shared plugin dispatch thread. Only named byte requests cross the ABI.
use std::sync::{
    Arc, Condvar, Mutex,
    atomic::{AtomicU64, Ordering},
};
use velk_plugin_sdk::{Plugin, Registrar, hooks};
use velk_search::{self as model, Query, Reply, Request, Results};

struct Job {
    id: u64,
    revision: u64,
    query: Query,
    text: String,
    ticket: u64,
}
#[derive(Default)]
struct Queue {
    job: Option<Job>,
    reply: Option<Result<Results, String>>,
    stop: bool,
}
pub struct Search {
    upload: Option<Job>,
    id: Option<u64>,
    running: bool,
    ticket: Arc<AtomicU64>,
    shared: Arc<(Mutex<Queue>, Condvar)>,
    worker: Option<std::thread::JoinHandle<()>>,
}
impl Default for Search {
    fn default() -> Self {
        let shared = Arc::new((Mutex::new(Queue::default()), Condvar::new()));
        let ticket = Arc::new(AtomicU64::new(0));
        let state = shared.clone();
        let current = ticket.clone();
        let worker = std::thread::Builder::new()
            .name("velk-search".into())
            .spawn(move || {
                loop {
                    let (lock, wake) = &*state;
                    let job = {
                        let mut q = lock.lock().unwrap();
                        while q.job.is_none() && !q.stop {
                            q = wake.wait(q).unwrap();
                        }
                        if q.stop {
                            break;
                        }
                        q.job.take().unwrap()
                    };
                    let result = find(&job, &current);
                    let mut q = lock.lock().unwrap();
                    if current.load(Ordering::Acquire) == job.ticket && !q.stop {
                        q.reply = Some(result);
                    }
                }
            })
            .ok();
        Self {
            upload: None,
            id: None,
            running: false,
            ticket,
            shared,
            worker,
        }
    }
}
impl Drop for Search {
    fn drop(&mut self) {
        self.ticket.fetch_add(1, Ordering::AcqRel);
        self.shared.0.lock().unwrap().stop = true;
        self.shared.1.notify_one();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}
impl Plugin for Search {
    const ID: &'static str = model::DEFAULT_BACKEND;
    const VERSION: &'static str = env!("CARGO_PKG_VERSION");
    fn register(reg: &mut Registrar<Self>) -> Result<(), String> {
        reg.provide(
            hooks::Request(model::HOOK),
            |this: &mut Self, bytes: &[u8]| model::encode(&this.request(model::decode(bytes)?)?),
        )
    }
    fn describe(&mut self) -> String {
        "Unicode literal buffer search".into()
    }
}

impl Search {
    fn request(&mut self, request: Request) -> Result<Reply, String> {
        match request {
            Request::Begin {
                id,
                revision,
                query,
            } => {
                query.validate()?;
                let ticket = self.ticket.fetch_add(1, Ordering::AcqRel) + 1;
                self.upload = Some(Job {
                    id,
                    revision,
                    query,
                    text: String::new(),
                    ticket,
                });
                self.id = Some(id);
                self.running = false;
                let mut q = self.shared.0.lock().unwrap();
                q.job = None;
                q.reply = None;
                Ok(Reply::Accepted)
            }
            Request::Append { id, text } => {
                let job = self
                    .upload
                    .as_mut()
                    .filter(|j| j.id == id)
                    .ok_or("Search upload has retired")?;
                if text.len() > model::CHUNK_BYTES
                    || job.text.len() + text.len() > model::MAX_DOCUMENT_BYTES
                {
                    return Err("Search document exceeds its upload limit".into());
                }
                job.text.push_str(&text);
                Ok(Reply::Accepted)
            }
            Request::Start { id } => {
                if self.id != Some(id) || self.upload.is_none() {
                    return Err("Search upload has retired".into());
                }
                if self.worker.as_ref().is_none_or(|w| w.is_finished()) {
                    return Err("Search worker unavailable".into());
                }
                self.shared.0.lock().unwrap().job = self.upload.take();
                self.running = true;
                self.shared.1.notify_one();
                Ok(Reply::Accepted)
            }
            Request::Poll { id } => {
                if self.id != Some(id) || !self.running {
                    return Err("Search has retired or was not started".into());
                }
                let mut q = self.shared.0.lock().unwrap();
                if let Some(result) = q.reply.take() {
                    return result.map(Reply::Ready);
                }
                if self.worker.as_ref().is_none_or(|w| w.is_finished()) {
                    return Err("Search worker stopped".into());
                }
                Ok(Reply::Pending)
            }
            Request::Cancel { id } => {
                if self.id == Some(id) {
                    self.ticket.fetch_add(1, Ordering::AcqRel);
                    self.id = None;
                    self.upload = None;
                    let mut q = self.shared.0.lock().unwrap();
                    q.job = None;
                    q.reply = None;
                }
                Ok(Reply::Accepted)
            }
        }
    }
}
fn find(job: &Job, current: &AtomicU64) -> Result<Results, String> {
    let literal = regex::escape(&job.query.text);
    let pattern = if job.query.whole_word {
        format!(r"\b(?:{literal})\b")
    } else {
        literal
    };
    let regex = regex::RegexBuilder::new(&pattern)
        .case_insensitive(!job.query.case_sensitive)
        .size_limit(1024 * 1024)
        .dfa_size_limit(1024 * 1024)
        .build()
        .map_err(|e| e.to_string())?;
    let mut result = Results {
        id: job.id,
        revision: job.revision,
        matches: vec![],
        truncated: false,
    };
    let mut byte = 0;
    let mut chars = 0;
    for found in regex.find_iter(&job.text) {
        if current.load(Ordering::Acquire) != job.ticket {
            return Err("Search cancelled".into());
        }
        if result.matches.len() == model::MAX_MATCHES {
            result.truncated = true;
            break;
        }
        chars += job.text[byte..found.start()].chars().count();
        let start = chars;
        chars += found.as_str().chars().count();
        byte = found.end();
        result.matches.push(model::Match { start, end: chars });
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn search(text: &str, query: &str, case_sensitive: bool, whole_word: bool) -> Results {
        let job = Job {
            id: 1,
            revision: 7,
            query: Query {
                text: query.into(),
                case_sensitive,
                whole_word,
            },
            text: text.into(),
            ticket: 0,
        };
        find(&job, &AtomicU64::new(0)).unwrap()
    }
    #[test]
    fn literal_unicode_case_word_boundaries_newlines_and_scalar_offsets() {
        let r = search("🦀 café CAFÉ caféine\ncafé", "café", false, true);
        assert_eq!(
            r.matches,
            vec![
                model::Match { start: 2, end: 6 },
                model::Match { start: 7, end: 11 },
                model::Match { start: 20, end: 24 }
            ]
        );
        assert_eq!(search("[a] a", "[a]", true, false).matches.len(), 1);
        assert_eq!(search("A\na", "A", true, false).matches.len(), 1);
        assert_eq!(search("a\nb", "a\nb", true, false).matches.len(), 1);
        assert!(search("a", "x", true, false).matches.is_empty());
    }
    #[test]
    fn caps_results_and_rejects_retired_uploads() {
        assert!(search(&"a".repeat(model::MAX_MATCHES + 1), "a", true, false).truncated);
        let mut provider = Search::default();
        provider
            .request(Request::Begin {
                id: 1,
                revision: 0,
                query: Query {
                    text: "a".into(),
                    ..Default::default()
                },
            })
            .unwrap();
        provider.request(Request::Cancel { id: 1 }).unwrap();
        assert!(
            provider
                .request(Request::Append {
                    id: 1,
                    text: "a".into()
                })
                .is_err()
        );
        assert!(provider.request(Request::Poll { id: 1 }).is_err());
    }
}
