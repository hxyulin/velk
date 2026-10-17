//! Independent backend state for each consuming plugin generation.
use crate::contracts::backend::{Call, Consumer};
use std::collections::BTreeMap;

pub struct Sessions<T>(BTreeMap<Consumer, T>);
impl<T> Default for Sessions<T> {
    fn default() -> Self {
        Self(BTreeMap::new())
    }
}
impl<T: Default> Sessions<T> {
    pub fn release(&mut self, bytes: &[u8]) -> Result<(), String> {
        let consumer: Consumer = serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
        self.0.remove(&consumer);
        Ok(())
    }
    /// The host constructs the envelope. A consumer reload drops its old session;
    /// unrelated frontends keep their own jobs, caches, and cancellation state.
    pub fn request(
        &mut self,
        bytes: &[u8],
        run: impl FnOnce(&mut T, &[u8]) -> Result<Vec<u8>, String>,
    ) -> Result<Vec<u8>, String> {
        let call: Call = serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
        self.0.retain(|consumer, _| {
            consumer.plugin != call.consumer.plugin || *consumer == call.consumer
        });
        if !self.0.contains_key(&call.consumer) && self.0.len() >= 16 {
            return Err("backend consumer limit reached (16)".into());
        }
        run(self.0.entry(call.consumer).or_default(), &call.input)
    }
}

use std::sync::{
    Arc, Condvar, Mutex,
    atomic::{AtomicBool, Ordering},
};
struct Job<I> {
    id: u64,
    input: I,
    cancelled: Arc<AtomicBool>,
}
struct Queue<I, O> {
    stop: bool,
    job: Option<Job<I>>,
    result: Option<(u64, Result<O, String>)>,
    invalidated: bool,
}
/// A bounded latest-request worker for frontend adapters. No backend calls or
/// polling waits run inside plugin dispatch; Drop cancels and joins owned work.
pub struct Tasks<I, O> {
    queue: Arc<(Mutex<Queue<I, O>>, Condvar)>,
    current: Option<(u64, Arc<AtomicBool>)>,
    thread: Option<std::thread::JoinHandle<()>>,
}
impl<I, O> Default for Tasks<I, O> {
    fn default() -> Self {
        Self {
            queue: Arc::new((
                Mutex::new(Queue {
                    stop: false,
                    job: None,
                    result: None,
                    invalidated: false,
                }),
                Condvar::new(),
            )),
            current: None,
            thread: None,
        }
    }
}
impl<I: Send + 'static, O: Send + 'static> Tasks<I, O> {
    pub fn start(
        &mut self,
        host: crate::Host,
        run: impl Fn(&crate::Host, I, &AtomicBool) -> Result<O, String> + Send + 'static,
        idle: impl Fn(&crate::Host) -> bool + Send + 'static,
    ) {
        if self.thread.is_some() {
            return;
        }
        let shared = self.queue.clone();
        self.thread = std::thread::Builder::new()
            .name("velk-frontend".into())
            .spawn(move || {
                loop {
                    let job = {
                        let mut queue = shared.0.lock().unwrap();
                        if queue.stop {
                            break;
                        }
                        if queue.job.is_none() {
                            let (next, _) = shared
                                .1
                                .wait_timeout(queue, std::time::Duration::from_millis(200))
                                .unwrap();
                            queue = next;
                        }
                        if queue.stop {
                            break;
                        }
                        queue.job.take()
                    };
                    if let Some(job) = job {
                        let result = run(&host, job.input, &job.cancelled);
                        let mut queue = shared.0.lock().unwrap();
                        if !queue.stop && !job.cancelled.load(Ordering::Acquire) {
                            queue.result = Some((job.id, result));
                        }
                    } else if idle(&host) {
                        shared.0.lock().unwrap().invalidated = true;
                    }
                }
            })
            .ok();
    }
    pub fn begin(&mut self, id: u64, input: I) -> Result<(), String> {
        if self.thread.as_ref().is_none_or(|t| t.is_finished()) {
            return Err("frontend worker unavailable".into());
        }
        if let Some((_, cancelled)) = self.current.take() {
            cancelled.store(true, Ordering::Release);
        }
        let cancelled = Arc::new(AtomicBool::new(false));
        self.current = Some((id, cancelled.clone()));
        let mut queue = self.queue.0.lock().unwrap();
        queue.result = None;
        queue.job = Some(Job {
            id,
            input,
            cancelled,
        });
        self.queue.1.notify_one();
        Ok(())
    }
    pub fn poll(&mut self, id: u64) -> Result<Option<O>, String> {
        if self
            .current
            .as_ref()
            .is_none_or(|(current, _)| *current != id)
        {
            return Err("frontend request retired".into());
        }
        let mut queue = self.queue.0.lock().unwrap();
        if queue
            .result
            .as_ref()
            .is_some_and(|(current, _)| *current == id)
        {
            return queue.result.take().unwrap().1.map(Some);
        }
        if self
            .thread
            .as_ref()
            .is_none_or(|thread| thread.is_finished())
        {
            return Err("frontend worker unavailable".into());
        }
        Ok(None)
    }
    pub fn cancel(&mut self, id: u64) {
        if self
            .current
            .as_ref()
            .is_some_and(|(current, _)| *current == id)
        {
            self.current
                .take()
                .unwrap()
                .1
                .store(true, Ordering::Release);
            let mut queue = self.queue.0.lock().unwrap();
            queue.job = None;
            queue.result = None;
        }
    }
    pub fn changed(&mut self) -> bool {
        std::mem::take(&mut self.queue.0.lock().unwrap().invalidated)
    }
}
impl<I, O> Drop for Tasks<I, O> {
    fn drop(&mut self) {
        if let Some((_, cancelled)) = &self.current {
            cancelled.store(true, Ordering::Release);
        }
        self.queue.0.lock().unwrap().stop = true;
        self.queue.1.notify_one();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn call(sessions: &mut Sessions<u64>, plugin: &str, generation: u64) -> u64 {
        let input = serde_json::to_vec(&Call {
            consumer: Consumer {
                plugin: plugin.into(),
                generation,
            },
            input: vec![],
        })
        .unwrap();
        let output = sessions
            .request(&input, |count, _| {
                *count += 1;
                Ok(count.to_le_bytes().to_vec())
            })
            .unwrap();
        u64::from_le_bytes(output.try_into().unwrap())
    }
    #[test]
    fn consumers_are_independent_and_reload_and_release_destroy_old_sessions() {
        let mut sessions = Sessions::default();
        assert_eq!(call(&mut sessions, "test.first", 1), 1);
        assert_eq!(call(&mut sessions, "test.second", 1), 1);
        assert_eq!(call(&mut sessions, "test.first", 1), 2);
        assert_eq!(call(&mut sessions, "test.first", 2), 1);
        assert_eq!(sessions.0.len(), 2);
        // A late release from the old generation cannot delete its replacement.
        sessions
            .release(
                &serde_json::to_vec(&Consumer {
                    plugin: "test.first".into(),
                    generation: 1,
                })
                .unwrap(),
            )
            .unwrap();
        assert_eq!(call(&mut sessions, "test.first", 2), 2);
        sessions
            .release(
                &serde_json::to_vec(&Consumer {
                    plugin: "test.first".into(),
                    generation: 2,
                })
                .unwrap(),
            )
            .unwrap();
        assert_eq!(call(&mut sessions, "test.first", 2), 1);
        assert_eq!(call(&mut sessions, "test.second", 1), 2);
    }
}
