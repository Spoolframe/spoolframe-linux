//! What the UI works out about downloads (PX-38) without a server or a
//! display: which tracks a downloaded file holds, and whether the SDK is
//! still moving anything.

use spoolframe_types::MediaTracks;

use crate::backend::{DownloadPhase, LocalDownload};

/// The tracks in `download`'s file, as the player and the track menus
/// should list them: the download's `tracks` cut down to what the file
/// carries, in the file's order. Audio and subtitles are named by their
/// place in the item's tracks (`DownloadFile::tracks`), so a place that is
/// not in the item is skipped, never guessed at.
///
/// Until the server says what the file is (`file` is `None`), the file is
/// what was asked for: every track for `All`, else the `Chosen` ones.
pub fn file_tracks(download: &LocalDownload) -> MediaTracks {
    use crate::backend::DownloadTracks;

    let (audio, subtitles) = match (&download.file, download.chosen.normalized()) {
        (Some(file), _) => (file.tracks.audio.clone(), file.tracks.subtitles.clone()),
        (None, DownloadTracks::Chosen { audio, subtitles }) => (audio, subtitles),
        (None, _) => (
            (0..download.tracks.audio.len() as u32).collect(),
            (0..download.tracks.subtitles.len() as u32).collect(),
        ),
    };
    MediaTracks {
        audio: pick(&download.tracks.audio, &audio),
        subtitles: pick(&download.tracks.subtitles, &subtitles),
        ..download.tracks.clone()
    }
}

/// The tracks of `all` at `places`, in that order; a place past the end is
/// skipped.
fn pick<T: Clone>(all: &[T], places: &[u32]) -> Vec<T> {
    places
        .iter()
        .filter_map(|&place| all.get(place as usize).cloned())
        .collect()
}

/// Whether the SDK is still moving a download (preparing on the server or
/// coming to this device): the app keeps listening for its events, and
/// holds back from closing the client, while this is so.
pub fn keeps_running(downloads: &[LocalDownload]) -> bool {
    downloads.iter().any(|d| {
        matches!(
            d.phase,
            DownloadPhase::Preparing { .. } | DownloadPhase::Transferring
        )
    })
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use spoolframe_types::{MediaId, MediaKind, Namespace};

    use super::*;
    use crate::backend::{
        Client, DownloadFile, DownloadSource, DownloadState, DownloadTracks, FileTracks, Session,
    };
    use crate::fake::FakeClient;
    use crate::testing::ready;

    fn alien() -> MediaId {
        MediaId::new(Namespace::Plex, MediaKind::Movie, "1").unwrap()
    }

    fn andrey(client: &FakeClient) -> Arc<dyn Session> {
        client.session(&FakeClient::profile("andrey")).unwrap()
    }

    fn started(tracks: DownloadTracks) -> LocalDownload {
        let client = FakeClient::new(None);
        ready(andrey(&client).start_download(&alien(), DownloadSource::Original, tracks)).unwrap()
    }

    fn names(tracks: &MediaTracks) -> (Vec<Option<String>>, Vec<Option<String>>) {
        (
            tracks.audio.iter().map(|a| a.title.clone()).collect(),
            tracks.subtitles.iter().map(|s| s.title.clone()).collect(),
        )
    }

    #[test]
    fn the_file_lists_only_the_tracks_it_holds() {
        let download = started(DownloadTracks::Chosen {
            audio: vec![2, 0],
            subtitles: vec![1],
        });
        assert_eq!(
            names(&file_tracks(&download)),
            (
                vec![None, Some("Audio Description".into())],
                vec![Some("Forced".into())]
            )
        );
    }

    #[test]
    fn the_file_decides_over_what_was_asked_for() {
        // The server may carry other tracks than the ones chosen.
        let mut download = started(DownloadTracks::All);
        download.file = Some(DownloadFile {
            container: spoolframe_types::Container::Matroska,
            tracks: FileTracks {
                audio: vec![1],
                subtitles: Vec::new(),
            },
        });
        assert_eq!(
            names(&file_tracks(&download)),
            (vec![Some("Commentary".into())], Vec::new())
        );
    }

    #[test]
    fn every_track_is_there_when_all_were_asked_and_the_file_is_not_known() {
        let mut download = started(DownloadTracks::All);
        download.file = None;
        assert_eq!(
            (
                file_tracks(&download).audio.len(),
                file_tracks(&download).subtitles.len()
            ),
            (3, 3)
        );
    }

    #[test]
    fn a_place_the_item_does_not_have_is_skipped() {
        let mut download = started(DownloadTracks::All);
        download.file = Some(DownloadFile {
            container: spoolframe_types::Container::Matroska,
            tracks: FileTracks {
                audio: vec![0, 7],
                subtitles: vec![9],
            },
        });
        assert_eq!(
            (
                file_tracks(&download).audio.len(),
                file_tracks(&download).subtitles.len()
            ),
            (1, 0)
        );
    }

    #[test]
    fn the_video_stays_as_the_item_has_it() {
        let download = started(DownloadTracks::Chosen {
            audio: Vec::new(),
            subtitles: Vec::new(),
        });
        assert_eq!(file_tracks(&download).video, download.tracks.video);
    }

    fn in_phase(phase: DownloadPhase) -> LocalDownload {
        LocalDownload {
            phase,
            ..started(DownloadTracks::All)
        }
    }

    #[test]
    fn preparing_and_transferring_downloads_keep_running() {
        let preparing = DownloadPhase::Preparing {
            server: DownloadState::Waiting,
        };
        assert_eq!(
            [preparing, DownloadPhase::Transferring].map(|p| keeps_running(&[in_phase(p)])),
            [true, true]
        );
    }

    #[test]
    fn paused_failed_and_done_downloads_do_not() {
        let phases = [
            DownloadPhase::Paused,
            DownloadPhase::Failed {
                reason: "gone".into(),
            },
            DownloadPhase::Done,
        ];
        assert_eq!(
            phases.map(|p| keeps_running(&[in_phase(p)])),
            [false, false, false]
        );
    }

    #[test]
    fn one_download_under_way_among_finished_ones_keeps_it_running() {
        let list = [
            in_phase(DownloadPhase::Done),
            in_phase(DownloadPhase::Transferring),
        ];
        assert_eq!((keeps_running(&list), keeps_running(&[])), (true, false));
    }
}
