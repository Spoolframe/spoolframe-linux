//! What tests here and the app's share, behind the `testing` feature: a
//! test's own directory (spoolframe-test-dir, tmpdir's random name,
//! removed when dropped), media files, and driving the fake backend's
//! futures without a runtime.

use std::future::Future;
use std::io::Write;
use std::path::PathBuf;
use std::pin::pin;
use std::sync::Arc;
use std::task::{Context, Poll, Waker};

pub use spoolframe_test_dir::TestDir;

use crate::reader::Spawn;

/// The value of a future that is ready at once, as the fake backend's
/// are.
pub fn ready<T>(future: impl Future<Output = T>) -> T {
    match pin!(future).poll(&mut Context::from_waker(Waker::noop())) {
        Poll::Ready(value) => value,
        Poll::Pending => panic!("the fake backend never waits"),
    }
}

/// Runs each future on a thread of its own, polling until it is done.
pub fn thread_spawn() -> Spawn {
    Arc::new(|future| {
        runtime().spawn(future);
    })
}
fn runtime() -> &'static tokio::runtime::Runtime {
    static RUNTIME: std::sync::OnceLock<tokio::runtime::Runtime> = std::sync::OnceLock::new();
    RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap()
    })
}
pub fn managed(
    session: Arc<dyn crate::backend::Session>,
    item: spoolframe_types::MediaId,
    selection: crate::backend::Selection,
    start: crate::backend::PlaybackStart,
) -> spoolframe_client_sdk::ManagedPlayback {
    let _enter = runtime().enter();
    spoolframe_client_sdk::ManagedPlayback::adopt(
        crate::playback::backend(session),
        item,
        Some(selection),
        Default::default(),
        start,
    )
}

/// `bytes` in a file named `media` in a directory of its own.
pub fn file_with(name: &str, bytes: &[u8]) -> (TestDir, PathBuf) {
    let dir = TestDir::new_blocking(&format!("spoolframe-{name}"));
    let path = dir.join("media");
    std::fs::File::create(&path)
        .unwrap()
        .write_all(bytes)
        .unwrap();
    (dir, path)
}

/// A file of `len` bytes where each byte is its offset modulo 251, so a
/// misplaced range shows.
pub fn media_file(name: &str, len: usize) -> (TestDir, PathBuf, Vec<u8>) {
    let bytes: Vec<u8> = (0..len).map(|i| (i % 251) as u8).collect();
    let (dir, path) = file_with(name, &bytes);
    (dir, path, bytes)
}
