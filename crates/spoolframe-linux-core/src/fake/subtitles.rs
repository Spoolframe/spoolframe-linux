//! The fake server's subtitle sites (PX-02): what a search finds, the
//! tracks downloaded from it, for everyone on the server, and how the
//! sites answer, which a test can change. The rules are the server's as
//! the protocol states them: results best first (hash matches, then by
//! rating and downloads; machine-translated and AI-made after people's;
//! rejected ones last), one site failing leaves the other's results, and
//! a download of the same result again gives the same track.

use std::collections::HashMap;

use spoolframe_types::{
    MediaId, QuotaOf, SubtitleDownload, SubtitleDownloaded, SubtitleOrigin, SubtitleResult,
    SubtitleRole, SubtitleSearch, SubtitleSource, SubtitleSourceState, SubtitleSourceStatus,
    SubtitleTrack,
};

use crate::backend::BackendError;

/// When the fake's OpenSubtitles quota comes back, Unix seconds.
pub const QUOTA_RESETS_AT: i64 = 1_760_054_400;

/// How the fake server's subtitle sites answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Sites {
    /// OpenSubtitles and SubDL both answer.
    #[default]
    Working,
    /// The server has no site set up: every call is `NoSubtitleSource`.
    NoSource,
    /// SubDL cannot be reached: OpenSubtitles' results alone.
    SubDlDown,
    /// Neither can be reached: the search is `Unavailable`.
    AllDown,
    /// OpenSubtitles' downloads are used up for today: results still
    /// show, a download is `QuotaExhausted`.
    QuotaSpent,
}

/// One download, as the server keeps it.
#[derive(Debug, Clone)]
struct Downloaded {
    id: String,
    result: String,
    release: String,
    source: String,
    language: String,
    by: String,
    by_username: String,
    at: i64,
    hash_match: bool,
    origin: SubtitleOrigin,
    hearing_impaired: bool,
}

/// Who asks.
#[derive(Debug, Clone, Copy)]
pub(in crate::fake) struct Asker<'a> {
    pub username: &'a str,
    pub display_name: &'a str,
    pub admin: bool,
}

#[derive(Default)]
pub(in crate::fake) struct Store {
    pub sites: Sites,
    downloads: HashMap<MediaId, Vec<Downloaded>>,
    next: u64,
}

impl Store {
    fn check(&self) -> Result<(), BackendError> {
        match self.sites {
            Sites::NoSource => Err(BackendError::NoSubtitleSource),
            _ => Ok(()),
        }
    }

    /// Every result the fake's sites have for an item called `title`,
    /// in English only, best first.
    fn found(&self, id: &MediaId, title: &str) -> Vec<SubtitleResult> {
        let dotted = title.replace(' ', ".");
        let downloaded = |result: &str| {
            self.downloads
                .get(id)
                .is_some_and(|d| d.iter().any(|d| d.result == result))
        };
        let result = |id: &str, release: String| SubtitleResult {
            id: id.to_owned(),
            source: id.split(':').next().unwrap_or_default().to_owned(),
            release,
            language: "en".into(),
            format: Some("srt".into()),
            fps_milli: Some(23_976),
            downloads: 0,
            rating: None,
            hearing_impaired: false,
            origin: SubtitleOrigin::Human,
            hash_match: false,
            rejected: false,
            downloaded: downloaded(id),
        };
        let mut results = vec![
            SubtitleResult {
                downloads: 31_402,
                rating: Some(90),
                hash_match: true,
                ..result(
                    "opensubtitles:1001",
                    format!("{dotted}.2160p.UHD.BluRay.REMUX.HDR.HEVC"),
                )
            },
            SubtitleResult {
                hearing_impaired: true,
                ..result("subdl:2001-3001", format!("{dotted}.1080p.BluRay.x264"))
            },
            SubtitleResult {
                downloads: 412,
                rating: Some(60),
                origin: SubtitleOrigin::MachineTranslated,
                ..result("opensubtitles:1002", format!("{dotted}.1080p.WEB-DL"))
            },
            SubtitleResult {
                downloads: 22,
                rating: Some(20),
                rejected: true,
                ..result("opensubtitles:1003", format!("{dotted}.720p.HDTV"))
            },
        ];
        if self.sites == Sites::SubDlDown {
            results.retain(|r| r.source != SubtitleSource::SubDl.id());
        }
        results
    }

    /// A search for `language` (`None`: English, the fake server's
    /// first), as the server answers it.
    pub fn search(
        &self,
        id: &MediaId,
        title: &str,
        language: Option<&str>,
    ) -> Result<SubtitleSearch, BackendError> {
        self.check()?;
        if self.sites == Sites::AllDown {
            return Err(BackendError::Unavailable(
                "OpenSubtitles cannot be reached".into(),
            ));
        }
        let language = language.unwrap_or("en").to_ascii_lowercase();
        let results: Vec<SubtitleResult> = match language.as_str() {
            "en" => self.found(id, title),
            _ => Vec::new(),
        };
        let found = |source: SubtitleSource| SubtitleSourceStatus {
            source: source.id().into(),
            state: SubtitleSourceState::Found {
                found: results.iter().filter(|r| r.source == source.id()).count() as u32,
            },
        };
        let mut sources = vec![
            found(SubtitleSource::OpenSubtitles),
            found(SubtitleSource::SubDl),
        ];
        match self.sites {
            Sites::SubDlDown => {
                sources[1].state = SubtitleSourceState::Failed {
                    message: "SubDL did not answer".into(),
                }
            }
            Sites::QuotaSpent => {
                sources[0].state = SubtitleSourceState::QuotaExhausted {
                    resets_at: Some(QUOTA_RESETS_AT),
                }
            }
            _ => {}
        }
        Ok(SubtitleSearch {
            languages: vec![language],
            results,
            sources,
        })
    }

    /// Downloads `result` as a track of item `id`, after its `before`
    /// tracks of its own: the same result again gives the same track.
    pub fn download(
        &mut self,
        id: &MediaId,
        title: &str,
        result: &str,
        before: usize,
        asker: Asker<'_>,
    ) -> Result<SubtitleDownloaded, BackendError> {
        self.check()?;
        let found = self
            .found(id, title)
            .into_iter()
            .find(|r| r.id == result)
            .ok_or_else(|| BackendError::NotFound(format!("subtitle result {result}")))?;
        if self.sites == Sites::QuotaSpent && found.source == SubtitleSource::OpenSubtitles.id() {
            return Err(BackendError::QuotaExhausted {
                whose: QuotaOf::Account,
                site: Some(found.source),
                resets_at: Some(QUOTA_RESETS_AT),
            });
        }
        let kept = self.downloads.entry(id.clone()).or_default();
        let at = match kept.iter().position(|d| d.result == result) {
            Some(at) => at,
            None => {
                self.next += 1;
                kept.push(Downloaded {
                    id: format!("dl{}", self.next),
                    result: result.to_owned(),
                    release: found.release,
                    source: found.source,
                    language: found.language,
                    by: asker.display_name.to_owned(),
                    by_username: asker.username.to_owned(),
                    at: 1_760_000_000,
                    hash_match: found.hash_match,
                    origin: found.origin,
                    hearing_impaired: found.hearing_impaired,
                });
                kept.len() - 1
            }
        };
        Ok(SubtitleDownloaded {
            track: (before + at) as u32,
            subtitle: track(&kept[at], asker, true),
            account_remaining: Some(19),
            user_remaining: None,
        })
    }

    /// Removes item `id`'s download `download`: its downloader's or an
    /// admin's to do.
    pub fn remove(
        &mut self,
        id: &MediaId,
        download: &str,
        asker: Asker<'_>,
    ) -> Result<(), BackendError> {
        self.check()?;
        let kept = self.downloads.entry(id.clone()).or_default();
        let at = kept
            .iter()
            .position(|d| d.id == download)
            .ok_or_else(|| BackendError::NotFound(format!("download {download}")))?;
        if kept[at].by_username != asker.username && !asker.admin {
            return Err(BackendError::NotAllowed);
        }
        kept.remove(at);
        Ok(())
    }

    /// Item `id`'s downloaded tracks, after its own, in the order they
    /// were downloaded; each says where it came from to a client that
    /// listed subtitle search (`described`).
    pub fn tracks(&self, id: &MediaId, asker: Asker<'_>, described: bool) -> Vec<SubtitleTrack> {
        self.downloads
            .get(id)
            .map(|kept| kept.iter().map(|d| track(d, asker, described)).collect())
            .unwrap_or_default()
    }

    /// Whether item `id`'s download is the track after `before` of its
    /// own, and its site's name: what its subtitle file says.
    pub fn site_of(&self, id: &MediaId, track: usize, before: usize) -> Option<&str> {
        let kept = self.downloads.get(id)?;
        let download = kept.get(track.checked_sub(before)?)?;
        Some(SubtitleSource::name_of(&download.source))
    }
}

fn track(download: &Downloaded, asker: Asker<'_>, described: bool) -> SubtitleTrack {
    SubtitleTrack {
        language: Some(download.language.clone()),
        title: Some(SubtitleSource::name_of(&download.source).to_owned()),
        forced: false,
        default: false,
        external: true,
        role: match download.hearing_impaired {
            true => SubtitleRole::Sdh,
            false => SubtitleRole::Full,
        },
        timing: None,
        download: described.then(|| SubtitleDownload {
            id: download.id.clone(),
            source: download.source.clone(),
            release: download.release.clone(),
            by: download.by.clone(),
            at: download.at,
            hash_match: download.hash_match,
            origin: download.origin,
            can_remove: download.by_username == asker.username || asker.admin,
        }),
    }
}
