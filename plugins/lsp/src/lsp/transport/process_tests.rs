use super::*;
use rustix::{
    fs::{FlockOperation, flock},
    process::{Pid, Signal, kill_process},
};
use std::{fs::File, time::Instant};

struct ChildCleanup {
    pid: Pid,
    lock: File,
}
impl Drop for ChildCleanup {
    fn drop(&mut self) {
        if locked(&self.lock) {
            let _ = kill_process(self.pid, Signal::KILL);
        }
    }
}
fn locked(file: &File) -> bool {
    match flock(file, FlockOperation::NonBlockingLockExclusive) {
        Ok(()) => {
            flock(file, FlockOperation::Unlock).unwrap();
            false
        }
        Err(rustix::io::Errno::WOULDBLOCK) => true,
        Err(error) => panic!("cannot check fixture lock: {error}"),
    }
}
async fn fixture(mode: &str) -> (tempfile::TempDir, Connection, File, ChildCleanup) {
    let root = tempfile::tempdir().unwrap();
    let lock = File::create(root.path().join(".cargo-lock")).unwrap();
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/descendant-server.py");
    let mut connection = Connection::spawn(
        OsStr::new("python3"),
        &[script.into_os_string(), root.path().into(), mode.into()],
        root.path(),
        Value::Null,
    )
    .unwrap();
    let ready = tokio::time::timeout(Duration::from_secs(5), connection.read())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(ready["method"], "fixture/ready");
    let child = Pid::from_raw(ready["params"]["child"].as_i64().unwrap() as i32).unwrap();
    let cleanup = ChildCleanup {
        pid: child,
        lock: lock.try_clone().unwrap(),
    };
    assert!(locked(&lock));
    (root, connection, lock, cleanup)
}
async fn assert_released(lock: &File) {
    let deadline = Instant::now() + Duration::from_secs(2);
    while locked(lock) && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(
        !locked(lock),
        "language-server descendant still holds the build lock"
    );
}
#[tokio::test(flavor = "current_thread")]
async fn graceful_shutdown_releases_descendant_build_locks() {
    let (_root, mut connection, lock, _cleanup) = fixture("graceful").await;
    connection.shutdown().await;
    assert!(connection.child.try_wait().unwrap().is_some());
    assert_released(&lock).await;
}
#[tokio::test(flavor = "current_thread")]
async fn forced_shutdown_releases_descendant_build_locks() {
    let (_root, mut connection, lock, _cleanup) = fixture("unresponsive").await;
    connection.shutdown().await;
    assert!(connection.child.try_wait().unwrap().is_some());
    assert_released(&lock).await;
}
#[tokio::test(flavor = "current_thread")]
async fn dropping_transport_releases_descendant_build_locks() {
    let (_root, connection, lock, _cleanup) = fixture("graceful").await;
    drop(connection);
    assert_released(&lock).await;
}

#[tokio::test(flavor = "current_thread")]
async fn shutdown_preserves_another_servers_processes_and_build_lock() {
    let (_first_root, mut first, first_lock, _first_cleanup) = fixture("graceful").await;
    let (_second_root, mut second, second_lock, _second_cleanup) = fixture("graceful").await;
    first.shutdown().await;
    assert_released(&first_lock).await;
    first.shutdown().await;
    assert!(locked(&second_lock));
    assert!(second.child.try_wait().unwrap().is_none());
    second.shutdown().await;
    assert_released(&second_lock).await;
}
