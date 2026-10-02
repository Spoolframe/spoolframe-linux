//! Posters and backdrops kept on this device, in memory and on disk,
//! through foyer's hybrid cache (P1-05 caches them on the server; this
//! saves fetching them again over the network on every start).
//!
//! It keeps the server's compressed bytes (JPEG or PNG, tens of KiB a
//! poster), not decoded images (hundreds of KiB each): the app decodes
//! what is on screen. Keys are server, size and artwork ID. An artwork ID
//! changes when its image does (Plex puts the update time in it), so an
//! entry never needs replacing; the oldest leave when the cache is full.

use std::future::Future;
use std::path::{Path, PathBuf};

use foyer::{
    BlockEngineConfig, DeviceBuilder, FsDeviceBuilder, HybridCache, HybridCacheBuilder,
    HybridCachePolicy, PsyncIoEngineConfig,
};
use spoolframe_types::ArtworkId;

use crate::backend::{ArtworkSize, BackendError, ServerKey};

/// How much the cache may hold.
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    pub memory: usize,
    pub disk: usize,
}

impl Default for Limits {
    /// About 1,500 Small posters in memory and 12,000 on disk.
    fn default() -> Self {
        Self {
            memory: 64 * 1024 * 1024,
            disk: 512 * 1024 * 1024,
        }
    }
}

/// Why the cache could not be opened.
#[derive(Debug, thiserror::Error)]
#[error("artwork cache at {path}: {message}")]
pub struct CacheError {
    pub path: PathBuf,
    pub message: String,
}

pub struct ArtworkCache {
    cache: HybridCache<String, Vec<u8>>,
}

impl ArtworkCache {
    /// `$XDG_CACHE_HOME/spoolframe-linux/artwork`, or under `~/.cache`.
    pub fn default_dir() -> Option<PathBuf> {
        let base = std::env::var_os("XDG_CACHE_HOME")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| Path::new(&h).join(".cache")))?;
        Some(base.join("spoolframe-linux").join("artwork"))
    }

    /// Opens the cache in `dir`, with what an earlier run left there. Runs
    /// in a tokio runtime, which it keeps using.
    pub async fn open(dir: &Path, limits: Limits) -> Result<Self, CacheError> {
        let error = |message: String| CacheError {
            path: dir.to_path_buf(),
            message,
        };
        std::fs::create_dir_all(dir).map_err(|e| error(e.to_string()))?;
        let device = FsDeviceBuilder::new(dir)
            .with_capacity(limits.disk)
            .build()
            .map_err(|e| error(e.to_string()))?;
        let cache = HybridCacheBuilder::new()
            .with_name("spoolframe-artwork")
            // Written to disk as it arrives, so it is there next start even
            // if the app does not close cleanly.
            .with_policy(HybridCachePolicy::WriteOnInsertion)
            .memory(limits.memory)
            .with_weighter(|key: &String, value: &Vec<u8>| key.len() + value.len())
            .storage()
            .with_engine_config(BlockEngineConfig::new(device))
            // Plain reads and writes on a thread pool, said outright: tens
            // of KiB a poster need nothing faster, and foyer logs a notice
            // when left to pick this default.
            .with_io_engine_config(PsyncIoEngineConfig::new())
            .build()
            .await
            .map_err(|e| error(e.to_string()))?;
        Ok(Self { cache })
    }

    fn key(server: &ServerKey, id: &ArtworkId, size: ArtworkSize) -> String {
        format!("{}/{}/{}", server.0, size.as_str(), id)
    }

    /// The image, from memory, from disk, or from `fetch` (then kept).
    /// Asked twice at once, it is fetched once.
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
        let key = Self::key(server, id, size);
        let entry = self
            .cache
            .get_or_fetch(
                &key,
                || async move { fetch.await.map_err(anyhow::Error::from) },
            )
            .await
            .map_err(|e| BackendError::Other(format!("artwork: {e}")))?;
        Ok(entry.value().clone())
    }

    /// Writes what is still in memory to disk; call before quitting.
    pub async fn close(&self) {
        if let Err(e) = self.cache.close().await {
            tracing::warn!(error = %e, "artwork cache not closed cleanly");
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use spoolframe_types::Namespace;

    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("spoolframe-artcache-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
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

    #[tokio::test]
    async fn a_second_get_does_not_fetch_again() {
        let cache = ArtworkCache::open(&temp_dir("again"), small())
            .await
            .unwrap();
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
    async fn another_size_is_another_entry() {
        let cache = ArtworkCache::open(&temp_dir("sizes"), small())
            .await
            .unwrap();
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
        let cache = ArtworkCache::open(&temp_dir("failed"), small())
            .await
            .unwrap();
        let failed = cache
            .get_or_fetch(&server(), &poster(), ArtworkSize::Small, async {
                Err(BackendError::NotFound("poster".into()))
            })
            .await;
        assert!(failed.is_err());
        let calls = Arc::new(AtomicUsize::new(0));
        cache
            .get_or_fetch(&server(), &poster(), ArtworkSize::Small, counted(&calls))
            .await
            .unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn the_cache_outlives_the_app() {
        let dir = temp_dir("restart");
        let calls = Arc::new(AtomicUsize::new(0));
        {
            let cache = ArtworkCache::open(&dir, small()).await.unwrap();
            cache
                .get_or_fetch(&server(), &poster(), ArtworkSize::Small, counted(&calls))
                .await
                .unwrap();
            cache.close().await;
        }
        let cache = ArtworkCache::open(&dir, small()).await.unwrap();
        let bytes = cache
            .get_or_fetch(&server(), &poster(), ArtworkSize::Small, counted(&calls))
            .await
            .unwrap();
        assert_eq!(
            (bytes, calls.load(Ordering::SeqCst)),
            (b"jpeg bytes".to_vec(), 1)
        );
    }
}
