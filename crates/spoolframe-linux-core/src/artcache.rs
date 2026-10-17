//! Posters and backdrops kept on this device, in memory and on disk, so
//! they are not fetched again over the network on every start (P1-05
//! caches them on the server).
//!
//! It keeps the server's compressed bytes (JPEG or PNG, tens of KiB a
//! poster), not decoded images (hundreds of KiB each): the app decodes
//! what is on screen. An image is looked for in memory, then on disk,
//! then asked of the server; what the server sends is kept in both.
//!
//! On disk each image is one file in the cache's directory, named by the
//! SHA-256 of `server|size|artwork id` in hex, as the Apple apps name
//! theirs. An artwork id names one version of an image (Plex puts the
//! update time in it), so a kept file never goes stale and has no expiry.
//! Reading a file sets its modification time, so the time says when it
//! was last shown; [`ArtworkCache::trim`], run once a little after start,
//! deletes the least recently shown when the files are over the cap.
//!
//! Scrub-preview frames (`preview:` ids) are many and seen once: they are
//! never written to disk.

use std::collections::{BTreeMap, HashMap};
use std::future::Future;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use sha2::{Digest, Sha256};
use spoolframe_types::{ArtworkId, Namespace};

use crate::backend::{ArtworkSize, BackendError, ServerKey};

/// How much the cache may hold.
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    /// Bytes kept in memory while the app runs.
    pub memory: usize,
    /// Bytes of files on disk before [`ArtworkCache::trim`] deletes some.
    pub disk: u64,
}

impl Default for Limits {
    /// About 1,500 Small posters in memory; 400 MB on disk, as the Apple
    /// apps keep.
    fn default() -> Self {
        Self {
            memory: 64 * 1024 * 1024,
            disk: 400 * 1024 * 1024,
        }
    }
}

/// How long after start [`ArtworkCache::trim`] runs: late enough not to
/// slow the first pages, early enough to run in a short session.
pub const TRIM_AFTER: Duration = Duration::from_secs(20);

/// A temporary file older than this is from a write that never finished
/// (the app was killed mid-write); trimming deletes it.
const ABANDONED: Duration = Duration::from_secs(60 * 60);

/// Why the cache could not be opened.
#[derive(Debug, thiserror::Error)]
#[error("artwork cache at {path}: {message}")]
pub struct CacheError {
    pub path: PathBuf,
    pub message: String,
}

/// What [`ArtworkCache::trim`] did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Trimmed {
    /// Images deleted to come under the cap.
    pub removed: usize,
    /// What the kept images take now, in bytes.
    pub kept: u64,
}

pub struct ArtworkCache {
    dir: PathBuf,
    limits: Limits,
    memory: Mutex<Memory>,
    /// One lock per image being looked up, so an image asked for twice at
    /// once is read or fetched once.
    looking: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
}

impl ArtworkCache {
    /// `artwork` in the app's cache directory ([`crate::paths::cache_dir`]).
    pub fn default_dir() -> Option<PathBuf> {
        crate::paths::cache_dir().map(|dir| dir.join("artwork"))
    }

    /// Opens the cache in `dir`, with what an earlier run left there.
    pub fn open(dir: &Path, limits: Limits) -> Result<Self, CacheError> {
        std::fs::create_dir_all(dir).map_err(|e| CacheError {
            path: dir.to_path_buf(),
            message: e.to_string(),
        })?;
        Ok(Self {
            dir: dir.to_path_buf(),
            limits,
            memory: Mutex::new(Memory::new(limits.memory)),
            looking: Mutex::new(HashMap::new()),
        })
    }

    /// The image's file name: the SHA-256 of `server|size|artwork id`, in
    /// lowercase hex.
    pub fn file_name(server: &ServerKey, id: &ArtworkId, size: ArtworkSize) -> String {
        let digest = Sha256::digest(format!("{}|{}|{}", server.0, size.as_str(), id));
        digest.iter().map(|b| format!("{b:02x}")).collect()
    }

    /// Where the image is kept on disk.
    pub fn path(&self, server: &ServerKey, id: &ArtworkId, size: ArtworkSize) -> PathBuf {
        self.dir.join(Self::file_name(server, id, size))
    }

    /// The image, from memory, from disk, or from `fetch` (then kept).
    /// Asked twice at once, it is fetched once. Runs on a tokio runtime:
    /// files are read and written on its blocking threads.
    pub async fn get_or_fetch<F>(
        &self,
        server: &ServerKey,
        id: &ArtworkId,
        size: ArtworkSize,
        fetch: F,
    ) -> Result<Vec<u8>, BackendError>
    where
        F: Future<Output = Result<Vec<u8>, BackendError>> + Send + 'static,
    {
        let name = Self::file_name(server, id, size);
        if let Some(bytes) = self.memory.lock().expect("memory").get(&name) {
            return Ok(bytes);
        }
        let lock = self
            .looking
            .lock()
            .expect("looking")
            .entry(name.clone())
            .or_default()
            .clone();
        let found = {
            let _held = lock.lock().await;
            self.look_up(&name, id, fetch).await
        };
        self.looking.lock().expect("looking").remove(&name);
        found
    }

    async fn look_up<F>(
        &self,
        name: &str,
        id: &ArtworkId,
        fetch: F,
    ) -> Result<Vec<u8>, BackendError>
    where
        F: Future<Output = Result<Vec<u8>, BackendError>> + Send + 'static,
    {
        // Another lookup of the same image may have kept it meanwhile.
        if let Some(bytes) = self.memory.lock().expect("memory").get(name) {
            return Ok(bytes);
        }
        let on_disk = kept_on_disk(id);
        let path = self.dir.join(name);
        if on_disk {
            let read = {
                let path = path.clone();
                tokio::task::spawn_blocking(move || read_and_touch(&path)).await
            };
            match read {
                Ok(Ok(bytes)) => {
                    self.remember(name, &bytes);
                    return Ok(bytes);
                }
                Ok(Err(e)) if e.kind() == io::ErrorKind::NotFound => {}
                Ok(Err(e)) => tracing::warn!(error = %e, "artwork file not read"),
                Err(e) => tracing::warn!(error = %e, "artwork file read panicked"),
            }
        }
        let bytes = fetch.await?;
        self.remember(name, &bytes);
        if on_disk {
            let (dir, name, kept) = (self.dir.clone(), name.to_owned(), bytes.clone());
            let wrote = tokio::task::spawn_blocking(move || write_atomically(&dir, &name, &kept));
            match wrote.await {
                Ok(Ok(())) => {}
                Ok(Err(e)) => tracing::warn!(error = %e, "artwork not kept on disk"),
                Err(e) => tracing::warn!(error = %e, "artwork file write panicked"),
            }
        }
        Ok(bytes)
    }

    fn remember(&self, name: &str, bytes: &[u8]) {
        self.memory
            .lock()
            .expect("memory")
            .insert(name.to_owned(), bytes.to_vec());
    }

    /// When the files on disk are over the cap, deletes the least recently
    /// shown until they are at 80% of it. Also deletes what is not one of
    /// its images: a write that never finished, and what the earlier cache
    /// (foyer's block files) left in the directory. Blocks: run it off the
    /// main thread.
    pub fn trim(&self) -> io::Result<Trimmed> {
        let now = SystemTime::now();
        let mut images = Vec::new();
        for entry in std::fs::read_dir(&self.dir)? {
            let entry = entry?;
            let meta = entry.metadata()?;
            if !meta.is_file() {
                continue;
            }
            let name = entry.file_name();
            let name = name.to_string_lossy();
            let modified = meta.modified()?;
            if is_image_name(&name) {
                images.push((modified, meta.len(), entry.path()));
                continue;
            }
            let in_progress = name.ends_with(".tmp")
                && now.duration_since(modified).unwrap_or_default() < ABANDONED;
            if !in_progress {
                remove(&entry.path());
            }
        }
        let mut kept: u64 = images.iter().map(|(_, len, _)| len).sum();
        let mut removed = 0;
        if kept > self.limits.disk {
            let target = self.limits.disk / 10 * 8;
            images.sort_by_key(|(modified, ..)| *modified);
            for (_, len, path) in images {
                if kept <= target {
                    break;
                }
                if remove(&path) {
                    kept -= len;
                    removed += 1;
                }
            }
        }
        Ok(Trimmed { removed, kept })
    }
}

/// Whether the image may be written to disk: scrub-preview frames are not.
fn kept_on_disk(id: &ArtworkId) -> bool {
    id.namespace() != Namespace::Preview
}

/// A name [`ArtworkCache::file_name`] makes: 64 lowercase hex digits.
fn is_image_name(name: &str) -> bool {
    name.len() == 64 && name.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// Deletes `path`; whether it went (or was gone already).
fn remove(path: &Path) -> bool {
    match std::fs::remove_file(path) {
        Ok(()) => true,
        Err(e) if e.kind() == io::ErrorKind::NotFound => true,
        Err(e) => {
            tracing::warn!(error = %e, path = %path.display(), "artwork file not deleted");
            false
        }
    }
}

/// The file's bytes, its modification time set to now: it was shown.
fn read_and_touch(path: &Path) -> io::Result<Vec<u8>> {
    let bytes = std::fs::read(path)?;
    let touched = std::fs::File::options()
        .write(true)
        .open(path)
        .and_then(|file| file.set_modified(SystemTime::now()));
    if let Err(e) = touched {
        // Still shown; it may only be trimmed sooner than it should be.
        tracing::debug!(error = %e, "artwork file's time not set");
    }
    Ok(bytes)
}

/// Writes `bytes` to `name` in `dir` through a temporary file renamed into
/// place, so a reader never sees half an image.
fn write_atomically(dir: &Path, name: &str, bytes: &[u8]) -> io::Result<()> {
    static WRITES: AtomicU64 = AtomicU64::new(0);
    let n = WRITES.fetch_add(1, Ordering::Relaxed);
    let temp = dir.join(format!(".{name}.{}-{n}.tmp", std::process::id()));
    let written =
        std::fs::write(&temp, bytes).and_then(|()| std::fs::rename(&temp, dir.join(name)));
    if written.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    written
}

/// The images kept in memory, the least recently read leaving first when
/// they are over `limit` bytes.
struct Memory {
    limit: usize,
    used: usize,
    tick: u64,
    entries: HashMap<String, (Vec<u8>, u64)>,
    /// Entries by when they were last read.
    order: BTreeMap<u64, String>,
}

impl Memory {
    fn new(limit: usize) -> Self {
        Self {
            limit,
            used: 0,
            tick: 0,
            entries: HashMap::new(),
            order: BTreeMap::new(),
        }
    }

    fn get(&mut self, name: &str) -> Option<Vec<u8>> {
        self.tick += 1;
        let (bytes, last) = self.entries.get_mut(name)?;
        self.order.remove(last);
        *last = self.tick;
        self.order.insert(self.tick, name.to_owned());
        Some(bytes.clone())
    }

    fn insert(&mut self, name: String, bytes: Vec<u8>) {
        if bytes.len() > self.limit {
            return;
        }
        self.tick += 1;
        if let Some((old, last)) = self.entries.remove(&name) {
            self.used -= old.len();
            self.order.remove(&last);
        }
        self.used += bytes.len();
        self.order.insert(self.tick, name.clone());
        self.entries.insert(name, (bytes, self.tick));
        while self.used > self.limit {
            let Some((_, oldest)) = self.order.pop_first() else {
                break;
            };
            if let Some((old, _)) = self.entries.remove(&oldest) {
                self.used -= old.len();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicUsize;

    use super::*;

    /// A directory of the test's own (removed when dropped), and the
    /// cache's place in it, not made yet.
    async fn temp_dir(name: &str) -> (spoolframe_test_dir::TestDir, PathBuf) {
        let tmp = spoolframe_test_dir::TestDir::new(&format!("spoolframe-artcache-{name}")).await;
        let path = tmp.as_ref().join("cache");
        (tmp, path)
    }

    fn small() -> Limits {
        Limits {
            memory: 1024 * 1024,
            disk: 16 * 1024 * 1024,
        }
    }

    fn server() -> ServerKey {
        ServerKey("k5d2r8w1tqz7m3c0vnh4q7fa".into())
    }

    fn poster() -> ArtworkId {
        ArtworkId::new(Namespace::Plex, "/library/metadata/1/thumb/9").unwrap()
    }

    fn frame() -> ArtworkId {
        "preview:0f3a/2".parse().unwrap()
    }

    /// A fetch that counts its calls.
    fn counted(
        calls: &Arc<AtomicUsize>,
    ) -> impl Future<Output = Result<Vec<u8>, BackendError>> + Send + 'static {
        let calls = calls.clone();
        async move {
            calls.fetch_add(1, Ordering::SeqCst);
            Ok(b"jpeg bytes".to_vec())
        }
    }

    /// A fetch that must not happen.
    async fn unreachable() -> Result<Vec<u8>, BackendError> {
        Err(BackendError::Unreachable("the server".into()))
    }

    #[test]
    fn a_file_is_named_by_the_sha256_of_server_size_and_id() {
        // printf '%s' 'k5d2r8w1tqz7m3c0vnh4q7fa|small|plex:/library/metadata/1/thumb/9' | sha256sum
        assert_eq!(
            ArtworkCache::file_name(&server(), &poster(), ArtworkSize::Small),
            "74377d546862037bd380626c2769823d00b8f5ac93028ada374172b7ed676758"
        );
    }

    #[tokio::test]
    async fn a_second_get_does_not_fetch_again() {
        let (_tmp, dir) = temp_dir("again").await;
        let cache = ArtworkCache::open(&dir, small()).unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        for _ in 0..2 {
            let bytes = cache
                .get_or_fetch(&server(), &poster(), ArtworkSize::Small, counted(&calls))
                .await
                .unwrap();
            assert_eq!(bytes, b"jpeg bytes");
        }
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn two_gets_at_once_fetch_once() {
        let (_tmp, dir) = temp_dir("at-once").await;
        let cache = ArtworkCache::open(&dir, small()).unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let slow = |calls: &Arc<AtomicUsize>| {
            let calls = calls.clone();
            async move {
                calls.fetch_add(1, Ordering::SeqCst);
                tokio::task::yield_now().await;
                Ok(b"jpeg bytes".to_vec())
            }
        };
        let (server, poster) = (server(), poster());
        let (a, b) = tokio::join!(
            cache.get_or_fetch(&server, &poster, ArtworkSize::Small, slow(&calls)),
            cache.get_or_fetch(&server, &poster, ArtworkSize::Small, slow(&calls)),
        );
        assert_eq!(
            (a.unwrap(), b.unwrap()),
            (b"jpeg bytes".to_vec(), b"jpeg bytes".to_vec())
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn another_size_is_another_entry() {
        let (_tmp, dir) = temp_dir("sizes").await;
        let cache = ArtworkCache::open(&dir, small()).unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        for size in [ArtworkSize::Small, ArtworkSize::Large] {
            cache
                .get_or_fetch(&server(), &poster(), size, counted(&calls))
                .await
                .unwrap();
        }
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn a_failed_fetch_is_an_error_and_is_not_kept() {
        let (_tmp, dir) = temp_dir("failed").await;
        let cache = ArtworkCache::open(&dir, small()).unwrap();
        let failed = cache
            .get_or_fetch(&server(), &poster(), ArtworkSize::Small, async {
                Err(BackendError::NotFound("poster".into()))
            })
            .await;
        assert!(failed.is_err());
        assert!(
            !cache
                .path(&server(), &poster(), ArtworkSize::Small)
                .exists()
        );
    }

    #[tokio::test]
    async fn what_the_server_sends_is_written_to_its_file() {
        let (_tmp, dir) = temp_dir("written").await;
        let cache = ArtworkCache::open(&dir, small()).unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        cache
            .get_or_fetch(&server(), &poster(), ArtworkSize::Small, counted(&calls))
            .await
            .unwrap();
        let path = cache.path(&server(), &poster(), ArtworkSize::Small);
        assert_eq!(std::fs::read(path).unwrap(), b"jpeg bytes");
    }

    #[tokio::test]
    async fn the_cache_outlives_the_app() {
        let (_tmp, dir) = temp_dir("restart").await;
        let calls = Arc::new(AtomicUsize::new(0));
        ArtworkCache::open(&dir, small())
            .unwrap()
            .get_or_fetch(&server(), &poster(), ArtworkSize::Small, counted(&calls))
            .await
            .unwrap();
        let cache = ArtworkCache::open(&dir, small()).unwrap();
        let bytes = cache
            .get_or_fetch(&server(), &poster(), ArtworkSize::Small, unreachable())
            .await
            .unwrap();
        assert_eq!(bytes, b"jpeg bytes");
    }

    #[tokio::test]
    async fn memory_is_looked_in_before_disk() {
        let (_tmp, dir) = temp_dir("memory-first").await;
        let cache = ArtworkCache::open(&dir, small()).unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        cache
            .get_or_fetch(&server(), &poster(), ArtworkSize::Small, counted(&calls))
            .await
            .unwrap();
        // Gone from disk, still shown from memory.
        std::fs::remove_file(cache.path(&server(), &poster(), ArtworkSize::Small)).unwrap();
        let bytes = cache
            .get_or_fetch(&server(), &poster(), ArtworkSize::Small, unreachable())
            .await
            .unwrap();
        assert_eq!(bytes, b"jpeg bytes");
    }

    #[tokio::test]
    async fn disk_is_looked_in_before_the_server() {
        let (_tmp, dir) = temp_dir("disk-first").await;
        let cache = ArtworkCache::open(&dir, small()).unwrap();
        std::fs::write(
            cache.path(&server(), &poster(), ArtworkSize::Small),
            b"kept bytes",
        )
        .unwrap();
        let bytes = cache
            .get_or_fetch(&server(), &poster(), ArtworkSize::Small, unreachable())
            .await
            .unwrap();
        assert_eq!(bytes, b"kept bytes");
    }

    #[tokio::test]
    async fn reading_a_file_marks_it_shown() {
        let (_tmp, dir) = temp_dir("touched").await;
        let cache = ArtworkCache::open(&dir, small()).unwrap();
        let path = cache.path(&server(), &poster(), ArtworkSize::Small);
        std::fs::write(&path, b"kept bytes").unwrap();
        let long_ago = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000_000);
        set_modified(&path, long_ago);
        cache
            .get_or_fetch(&server(), &poster(), ArtworkSize::Small, unreachable())
            .await
            .unwrap();
        assert!(std::fs::metadata(&path).unwrap().modified().unwrap() > long_ago);
    }

    #[tokio::test]
    async fn a_preview_frame_is_not_written_to_disk() {
        let (_tmp, dir) = temp_dir("preview").await;
        let cache = ArtworkCache::open(&dir, small()).unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        cache
            .get_or_fetch(&server(), &frame(), ArtworkSize::Original, counted(&calls))
            .await
            .unwrap();
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0);
    }

    fn set_modified(path: &Path, at: SystemTime) {
        std::fs::File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(at)
            .unwrap();
    }

    /// Ten 100-byte images, the first shown longest ago.
    fn ten_images(dir: &Path) -> Vec<PathBuf> {
        (0..10u64)
            .map(|n| {
                let id = ArtworkId::new(Namespace::Plex, format!("/library/metadata/{n}")).unwrap();
                let path = dir.join(ArtworkCache::file_name(&server(), &id, ArtworkSize::Small));
                std::fs::write(&path, [0u8; 100]).unwrap();
                set_modified(
                    &path,
                    SystemTime::UNIX_EPOCH + Duration::from_secs(1_000 + n),
                );
                path
            })
            .collect()
    }

    fn capped(disk: u64) -> Limits {
        Limits { disk, ..small() }
    }

    #[tokio::test]
    async fn under_the_cap_nothing_is_trimmed() {
        let (_tmp, dir) = temp_dir("under").await;
        let cache = ArtworkCache::open(&dir, capped(1000)).unwrap();
        ten_images(&dir);
        assert_eq!(
            cache.trim().unwrap(),
            Trimmed {
                removed: 0,
                kept: 1000
            }
        );
    }

    #[tokio::test]
    async fn over_the_cap_the_least_recently_shown_go_until_80_percent() {
        let (_tmp, dir) = temp_dir("over").await;
        let cache = ArtworkCache::open(&dir, capped(999)).unwrap();
        let images = ten_images(&dir);
        cache.trim().unwrap();
        // 1000 bytes over a 999 cap: down to 792 (80%) or less, oldest first.
        let left: Vec<bool> = images.iter().map(|p| p.exists()).collect();
        assert_eq!(
            left,
            [
                false, false, false, true, true, true, true, true, true, true
            ]
        );
    }

    #[tokio::test]
    async fn trimming_deletes_what_is_not_an_image() {
        let (_tmp, dir) = temp_dir("leftovers").await;
        let cache = ArtworkCache::open(&dir, small()).unwrap();
        let foyer = dir.join("foyer-storage-direct-fs-0");
        std::fs::write(&foyer, b"block").unwrap();
        cache.trim().unwrap();
        assert!(!foyer.exists());
    }

    #[tokio::test]
    async fn trimming_leaves_a_write_in_progress() {
        let (_tmp, dir) = temp_dir("in-progress").await;
        let cache = ArtworkCache::open(&dir, small()).unwrap();
        let temp = dir.join(".abc.1-0.tmp");
        std::fs::write(&temp, b"half").unwrap();
        cache.trim().unwrap();
        assert!(temp.exists());
    }

    #[test]
    fn memory_lets_the_least_recently_read_go_first() {
        let mut memory = Memory::new(20);
        memory.insert("a".into(), vec![0; 10]);
        memory.insert("b".into(), vec![0; 10]);
        memory.get("a");
        memory.insert("c".into(), vec![0; 10]);
        assert_eq!(
            (
                memory.get("a").is_some(),
                memory.get("b").is_some(),
                memory.get("c").is_some()
            ),
            (true, false, true)
        );
    }
}
