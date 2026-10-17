//! In-process host fixtures for plugin behavior tests. These use the normal byte
//! request adapter; capability enforcement and loader retirement need host tests.
use crate::{
    Host,
    host::{Request, Response},
};
use std::sync::{Arc, Mutex};

pub struct MockHost {
    host: Host,
    requests: Arc<Mutex<Vec<Request>>>,
}
impl MockHost {
    pub fn new(
        handler: impl Fn(Request) -> Result<Response, String> + Send + Sync + 'static,
    ) -> Self {
        let requests = Arc::new(Mutex::new(Vec::new()));
        let recorded = requests.clone();
        let host = crate::ffi::host::mock(move |request| {
            recorded.lock().unwrap().push(request.clone());
            handler(request)
        });
        Self { host, requests }
    }
    pub fn host(&self) -> Host {
        self.host.clone()
    }
    pub fn take_requests(&self) -> Vec<Request> {
        std::mem::take(&mut *self.requests.lock().unwrap())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn clones_cross_threads_record_valid_requests_and_preserve_host_errors() {
        let fixture = MockHost::new(|request| match request {
            Request::Buffers => Ok(Response::Buffers { buffers: vec![] }),
            _ => Err("fixture failure".into()),
        });
        let host = fixture.host();
        std::thread::spawn(move || {
            assert!(matches!(host.request(Request::Buffers).unwrap(), Response::Buffers { buffers } if buffers.is_empty()));
            assert_eq!(host.request(Request::Workspace).unwrap_err(), "fixture failure");
        }).join().unwrap();
        assert_eq!(
            fixture.take_requests(),
            vec![Request::Buffers, Request::Workspace]
        );
        assert!(fixture.take_requests().is_empty());
    }
    #[test]
    fn invalid_requests_do_not_reach_handler_and_last_host_releases_captured_state() {
        let marker = Arc::new(());
        let weak = Arc::downgrade(&marker);
        let fixture = MockHost::new(move |_| {
            let _keep_alive = &marker;
            Ok(Response::Accepted)
        });
        let host = fixture.host();
        assert!(
            host.request(Request::OpenBuffer {
                path: "relative".into()
            })
            .is_err()
        );
        assert!(fixture.take_requests().is_empty());
        drop(fixture);
        assert!(weak.upgrade().is_some());
        drop(host);
        assert!(weak.upgrade().is_none());
    }
}
