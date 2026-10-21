//! Downloads for offline viewing (PX-38), as the LinuxDownloadStates,
//! LinuxDownloads and LinuxDownloadsOffline boards have them: the Download
//! action on a film's or an episode's page, and the Downloads page with
//! every download on this computer, which also opens with no server. Both
//! show a download the same way, from [`status`], and are changed in place
//! as its events come (one per 8 MiB read): a row is made once and its
//! labels, bar and buttons are set again.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use adw::prelude::*;
use gtk::glib;
use spoolframe_linux_core::backend::{
    Client, DownloadEvent, DownloadPhase, DownloadSource, DownloadSpace, DownloadState,
    DownloadTracks, LocalDownload, LocalWatch,
};
use spoolframe_linux_core::format;
use spoolframe_types::{ArtworkKind, Item, MediaId};

/// What a download's buttons do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Act {
    /// A finished one, from its file, with or without a server.
    Play,
    Pause,
    Resume,
    /// A failed download, again.
    Retry,
    /// Stops one not finished and takes it off this computer.
    Cancel,
    /// A failed one off this computer: a cancel, under the board's word.
    Dismiss,
    /// A finished one off this computer, once the user confirms.
    Delete,
}

impl Act {
    fn label(self) -> &'static str {
        match self {
            Act::Play => "Play",
            Act::Pause => "Pause",
            Act::Resume => "Resume",
            Act::Retry => "Try again",
            Act::Cancel => "Cancel",
            Act::Dismiss => "Dismiss",
            Act::Delete => "Delete download",
        }
    }

    fn icon(self) -> Option<&'static str> {
        match self {
            Act::Play => Some(crate::icons::PLAY),
            Act::Pause => Some(crate::icons::PAUSE),
            Act::Resume => Some(crate::icons::DOWNLOADS),
            Act::Delete => Some(crate::icons::FORGET),
            _ => None,
        }
    }
}

/// What a download's buttons ask of the app: `Act` on this download.
pub type Acts = Rc<dyn Fn(LocalDownload, Act)>;

/// Whether a failed download is worth trying again. The SDK keeps no
/// record of where it failed, but `size` tells: the server sets it once
/// its file is ready. A failure with no size happened while the server
/// prepared the file (it could not convert it, or the item is gone), and
/// trying again prepares the same thing the same way. With a size, it
/// failed on the way here or on this computer (the file arrived damaged,
/// the disk was full, a read went wrong), which another try may not.
/// Network trouble is never a failure: the SDK waits and tries by itself,
/// saying so in `note`.
pub fn retryable(download: &LocalDownload) -> bool {
    matches!(download.phase, DownloadPhase::Failed { .. }) && download.size.is_some()
}

/// How a download shows: a heading ("Downloading · 63%"), a line under it,
/// a bar while it moves, and its buttons.
#[derive(Debug, Clone, PartialEq)]
pub struct Status {
    pub heading: String,
    pub detail: Option<String>,
    /// 0 to 1, while there is something to show.
    pub progress: Option<f64>,
    pub acts: Vec<Act>,
}

/// How `download` shows, in its phase. While the SDK waits to try again,
/// its note says why, in place of the line under the heading.
pub fn status(download: &LocalDownload) -> Status {
    let version = version(&download.source);
    let note = download.note.clone();
    match &download.phase {
        DownloadPhase::Preparing { server } => {
            let (heading, progress, detail) = match server {
                DownloadState::Waiting => (
                    "Waiting on the server".to_owned(),
                    None,
                    format!("{version} · the server prepares it, then it downloads"),
                ),
                DownloadState::Converting {
                    done_percent,
                    eta_secs,
                } => (
                    with_percent("Converting on the server", *done_percent),
                    done_percent.map(fraction),
                    match eta_secs {
                        Some(secs) => format!("{version} · about {}", eta(*secs)),
                        None => version,
                    },
                ),
                DownloadState::Writing { done_percent } => (
                    with_percent("Writing on the server", *done_percent),
                    done_percent.map(fraction),
                    version,
                ),
                _ => ("Preparing on the server".to_owned(), None, version),
            };
            Status {
                heading,
                detail: note.or(Some(detail)),
                progress,
                acts: vec![Act::Cancel],
            }
        }
        DownloadPhase::Transferring => Status {
            heading: with_percent("Downloading", percent(download)),
            detail: note.or_else(|| amount(download)),
            progress: Some(percent(download).map_or(0.0, fraction)),
            acts: vec![Act::Pause, Act::Cancel],
        },
        DownloadPhase::Paused => Status {
            heading: match percent(download) {
                Some(p) => format!("Paused at {p}%"),
                None => "Paused".to_owned(),
            },
            detail: amount(download),
            progress: percent(download).map(fraction),
            acts: vec![Act::Resume, Act::Cancel],
        },
        DownloadPhase::Done => Status {
            heading: format!(
                "Downloaded · {}",
                size(download.size.unwrap_or(download.received))
            ),
            detail: Some(match watch_line(download) {
                Some(watch) => format!("{version} · {} · {watch}", tracks_line(download)),
                None => format!("{version} · {}", tracks_line(download)),
            }),
            progress: None,
            acts: vec![Act::Play, Act::Delete],
        },
        DownloadPhase::Failed { reason } => Status {
            heading: "Couldn’t download".to_owned(),
            detail: Some(reason.clone()),
            progress: None,
            acts: match retryable(download) {
                true => vec![Act::Retry, Act::Dismiss],
                false => vec![Act::Dismiss],
            },
        },
    }
}

/// How far the user got in a finished download: "1 h 20 min left" or
/// "Watched", as this computer has it; none before it is started. A watch
/// the server has not heard yet says so: it goes at the next sync.
pub fn watch_line(download: &LocalDownload) -> Option<String> {
    let watch = &download.watch;
    let line = match (watch.watched, watch.position_ms) {
        (Some(true), _) => "Watched".to_owned(),
        (_, Some(ms)) if ms > 0 => {
            let length = crate::views::Facts::of(&download.item).length?;
            format::remaining(length, Duration::from_millis(ms))
        }
        _ => return None,
    };
    Some(match watch.pending {
        true => format!("{line}, not sent yet"),
        false => line,
    })
}

/// Where a download plays from on its own Play: where the user stopped in
/// it, as this computer has it; the start once watched.
pub fn resume_point(watch: &LocalWatch) -> Option<Duration> {
    match watch.watched {
        Some(true) => None,
        _ => watch.position_ms.map(Duration::from_millis),
    }
}

/// Where a download plays from when its item page asked for `asked`
/// (Resume, or Play from the start with `None`): the place watched here
/// while the server has not heard it, else the server's, which the page
/// showed.
pub fn start_from(watch: &LocalWatch, asked: Option<Duration>) -> Option<Duration> {
    match (asked, watch.pending) {
        (None, _) => None,
        (Some(_), true) => resume_point(watch),
        (Some(at), false) => Some(at),
    }
}

/// The profile's finished download of `item`, to play from its file in
/// place of the server's: the newest, if there is more than one. The
/// list is the profile's own (`Client::set_download_profile`).
pub fn playable_here<'a>(
    downloads: &'a [LocalDownload],
    item: &MediaId,
) -> Option<&'a LocalDownload> {
    downloads
        .iter()
        .filter(|d| d.item.id() == item)
        .filter(|d| d.phase == DownloadPhase::Done && d.file_url.is_some())
        .max_by_key(|d| d.created_at)
}

/// What was watched offline and waits for the server, as the offline
/// board says it: "2 changes to send: Alien watched, Heat at 1:12:40.
/// They go when the server is back."; none with nothing waiting.
pub fn pending_line(downloads: &[LocalDownload]) -> Option<String> {
    let changes: Vec<String> = downloads
        .iter()
        .filter(|d| d.watch.pending)
        .map(|d| {
            let name = title(d);
            match (d.watch.watched, d.watch.position_ms) {
                (Some(true), _) => format!("{name} watched"),
                (_, Some(ms)) => {
                    format!("{name} at {}", format::timestamp(Duration::from_millis(ms)))
                }
                _ => format!("{name} not watched"),
            }
        })
        .collect();
    match changes.len() {
        0 => None,
        1 => Some(format!(
            "1 change to send: {}. It goes when the server is back.",
            changes[0]
        )),
        n => Some(format!(
            "{n} changes to send: {}. They go when the server is back.",
            changes.join(", ")
        )),
    }
}

fn with_percent(what: &str, percent: Option<impl std::fmt::Display>) -> String {
    match percent {
        Some(p) => format!("{what} · {p}%"),
        None => what.to_owned(),
    }
}

fn fraction(percent: impl Into<u64>) -> f64 {
    (percent.into().min(100) as f64) / 100.0
}

/// How much of the file is here, once its size is known.
fn percent(download: &LocalDownload) -> Option<u64> {
    let size = download.size.filter(|s| *s > 0)?;
    Some((download.received.saturating_mul(100) / size).min(100))
}

/// "2.3 of 3.6 GB", once the size is known.
fn amount(download: &LocalDownload) -> Option<String> {
    let whole = download.size?;
    Some(match (download.received, whole) {
        (r, s) if r >= GB && s >= GB => {
            format!("{:.1} of {:.1} GB", r as f64 / 1e9, s as f64 / 1e9)
        }
        (r, s) => format!("{} of {}", size(r), size(s)),
    })
}

/// "14 min left", from the server's guess in seconds.
fn eta(secs: u64) -> String {
    format::remaining(Duration::from_secs(secs), Duration::ZERO)
}

const GB: u64 = 1_000_000_000;

/// A size as the boards write it: "3.4 GB", "740 MB", in powers of ten as
/// file managers show them.
pub fn size(bytes: u64) -> String {
    let b = bytes as f64;
    match bytes {
        n if n >= GB => format!("{:.1} GB", b / 1e9),
        n if n >= 1_000_000 => format!("{:.0} MB", b / 1e6),
        n if n >= 1_000 => format!("{:.0} kB", b / 1e3),
        n => format!("{n} bytes"),
    }
}

/// The version a download was made from: Original, or its profile.
pub fn version(source: &DownloadSource) -> String {
    match source {
        DownloadSource::Original => "Original".to_owned(),
        DownloadSource::Profile { name } => name.clone(),
        DownloadSource::Aac256 => "AAC 256 kbit/s".to_owned(),
        DownloadSource::Unknown => "Another version".to_owned(),
    }
}

/// "all tracks", or how many the file carries besides the video.
fn tracks_line(download: &LocalDownload) -> String {
    if download.chosen == DownloadTracks::All {
        return "all tracks".to_owned();
    }
    let file = spoolframe_linux_core::downloads::file_tracks(download);
    match file.audio.len() + file.subtitles.len() {
        1 => "1 track".to_owned(),
        n => format!("{n} tracks"),
    }
}

/// A download's name: the film's, or "Severance · S2 E5 · Trojan's Horse".
pub fn title(download: &LocalDownload) -> String {
    match &download.item {
        Item::Episode(e) => {
            let show = e
                .show_title
                .clone()
                .or_else(|| download.show.as_ref().map(item_title));
            let episode = format!("S{} E{} · {}", e.season, e.episode, e.title);
            match show {
                Some(show) => format!("{show} · {episode}"),
                None => episode,
            }
        }
        other => item_title(other),
    }
}

fn item_title(item: &Item) -> String {
    crate::views::Facts::of(item).title
}

/// The line under a row's title: "Movie · Mobile · 2 tracks", with the
/// size once finished.
pub fn info_line(download: &LocalDownload) -> String {
    let kind = match download.item {
        Item::Episode(_) => "Episode",
        _ => "Movie",
    };
    let mut line = format!(
        "{kind} · {} · {}",
        version(&download.source),
        tracks_line(download)
    );
    if download.phase == DownloadPhase::Done {
        line.push_str(&format!(
            " · {}",
            size(download.size.unwrap_or(download.received))
        ));
    }
    line
}

/// A profile with downloads on this computer (PX-60), as a download names
/// the user fetching it: what `Client::set_download_profile` takes, and
/// the username to show.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Holder {
    pub server: String,
    pub user_id: String,
    pub username: String,
}

impl Holder {
    pub fn of(download: &LocalDownload) -> Self {
        Holder {
            server: download.server.clone(),
            user_id: download.user_id.clone(),
            username: download.username.clone(),
        }
    }

    fn same(&self, other: &Holder) -> bool {
        self.server == other.server && self.user_id == other.user_id
    }
}

/// `known` with the profiles `downloads` name added, each once, in the
/// order first seen.
pub fn holders(mut known: Vec<Holder>, downloads: &[LocalDownload]) -> Vec<Holder> {
    for holder in downloads.iter().map(Holder::of) {
        if !known.iter().any(|k| k.same(&holder)) {
            known.push(holder);
        }
    }
    known
}

/// The profile the Downloads page shows with no server: the one used
/// last, if it has downloads here, else the first that has; the one
/// used last when none is known to.
pub fn offline_holder(holders: &[Holder], last: Option<Holder>) -> Option<Holder> {
    match last {
        Some(last) if holders.iter().any(|h| h.same(&last)) || holders.is_empty() => Some(last),
        _ => holders.first().cloned(),
    }
}

/// The download an item's page shows: the profile's latest of the item.
pub fn for_item<'a>(downloads: &'a [LocalDownload], item: &MediaId) -> Option<&'a LocalDownload> {
    downloads
        .iter()
        .filter(|d| d.item.id() == item)
        .max_by_key(|d| d.created_at)
}

/// Whether a download is listed under Downloaded, not In progress.
fn finished(download: &LocalDownload) -> bool {
    download.phase == DownloadPhase::Done
}

/// Where download `key` goes in its section: oldest first, as the SDK
/// lists them.
pub fn place(downloads: &[LocalDownload], key: &str) -> usize {
    let Some(done) = downloads.iter().find(|d| d.key == key).map(finished) else {
        return 0;
    };
    downloads
        .iter()
        .filter(|d| finished(d) == done)
        .position(|d| d.key == key)
        .unwrap_or(0)
}

/// What an event did to a list of downloads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Applied {
    /// One listed changed, in place.
    Updated,
    Added,
    Removed,
    /// The removal of one not listed: nothing to do.
    Nothing,
}

/// `event` applied to `downloads`, as the SDK's own list would have it.
pub fn apply(downloads: &mut Vec<LocalDownload>, event: &DownloadEvent) -> Applied {
    match event {
        DownloadEvent::Changed(changed) => {
            match downloads.iter_mut().find(|d| d.key == changed.key) {
                Some(listed) => {
                    *listed = changed.clone();
                    Applied::Updated
                }
                None => {
                    downloads.push(changed.clone());
                    Applied::Added
                }
            }
        }
        DownloadEvent::Removed { key } => match downloads.iter().position(|d| d.key == *key) {
            Some(at) => {
                downloads.remove(at);
                Applied::Removed
            }
            None => Applied::Nothing,
        },
    }
}

/// "Delete the download of Alien?", over `parent`; `delete` on Delete.
fn confirm_delete(parent: &impl IsA<gtk::Widget>, download: &LocalDownload, delete: Rc<dyn Fn()>) {
    let alert = adw::AlertDialog::builder()
        .heading(format!("Delete the download of {}?", title(download)))
        .body(format!(
            "{} is freed on this computer. You can still stream it from the server.",
            size(download.size.unwrap_or(download.received))
        ))
        .default_response("cancel")
        .close_response("cancel")
        .build();
    alert.add_response("cancel", "Cancel");
    alert.add_response("delete", "Delete");
    alert.set_response_appearance("delete", adw::ResponseAppearance::Destructive);
    alert.connect_response(Some("delete"), move |_, _| delete());
    alert.present(Some(parent));
}

/// A download's heading, line and bar, and its buttons, set again in place
/// for each change.
struct StatusView {
    text: gtk::Box,
    heading: gtk::Label,
    detail: gtk::Label,
    progress: gtk::ProgressBar,
    actions: gtk::Box,
    /// The buttons shown: made again only when they change.
    acts: RefCell<Vec<Act>>,
    /// What the buttons act on.
    shown: Rc<RefCell<Option<LocalDownload>>>,
    act: Acts,
}

impl StatusView {
    fn new(act: Acts) -> Self {
        let text = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(4)
            .hexpand(true)
            .build();
        let heading = gtk::Label::builder()
            .xalign(0.0)
            .wrap(true)
            .css_classes(["heading"])
            .build();
        let progress = gtk::ProgressBar::builder()
            .visible(false)
            .valign(gtk::Align::Center)
            .css_classes(["download-progress"])
            .build();
        let detail = gtk::Label::builder()
            .xalign(0.0)
            .wrap(true)
            .css_classes(["dim-label", "caption"])
            .build();
        text.append(&heading);
        text.append(&progress);
        text.append(&detail);
        let actions = gtk::Box::builder()
            .spacing(8)
            .valign(gtk::Align::Center)
            .build();
        StatusView {
            text,
            heading,
            detail,
            progress,
            actions,
            acts: RefCell::default(),
            shown: Rc::default(),
            act,
        }
    }

    fn show(&self, download: &LocalDownload) {
        let status = status(download);
        self.heading.set_label(&status.heading);
        self.detail
            .set_label(status.detail.as_deref().unwrap_or(""));
        self.detail.set_visible(status.detail.is_some());
        self.progress.set_visible(status.progress.is_some());
        self.progress.set_fraction(status.progress.unwrap_or(0.0));
        *self.shown.borrow_mut() = Some(download.clone());
        if *self.acts.borrow() != status.acts {
            while let Some(child) = self.actions.first_child() {
                self.actions.remove(&child);
            }
            for act in &status.acts {
                self.actions.append(&self.button(*act));
            }
            *self.acts.borrow_mut() = status.acts;
        }
    }

    fn button(&self, act: Act) -> gtk::Button {
        let button = gtk::Button::builder()
            .valign(gtk::Align::Center)
            .css_classes(["action", "outlined"])
            .build();
        match act.icon() {
            Some(icon) => button.set_child(Some(
                &adw::ButtonContent::builder()
                    .icon_name(icon)
                    .label(act.label())
                    .build(),
            )),
            None => button.set_label(act.label()),
        }
        let (shown, run) = (self.shown.clone(), self.act.clone());
        button.connect_clicked(move |button| {
            let Some(download) = shown.borrow().clone() else {
                return;
            };
            if act == Act::Delete {
                let (run, kept) = (run.clone(), download.clone());
                confirm_delete(
                    button,
                    &download,
                    Rc::new(move || run(kept.clone(), Act::Delete)),
                );
            } else {
                run(download, act);
            }
        });
        button
    }
}

/// The Download action on a film's or an episode's page: a Download button
/// beside Resume while there is no download, and the download's state
/// under the row once there is one.
pub struct ItemDownload {
    pub item: MediaId,
    pub button: gtk::Button,
    pub status: gtk::Box,
    view: StatusView,
}

impl ItemDownload {
    /// `open` asks what to download.
    pub fn new(item: MediaId, open: Rc<dyn Fn()>, act: Acts) -> Rc<Self> {
        let button = gtk::Button::builder()
            .child(
                &adw::ButtonContent::builder()
                    .icon_name(crate::icons::DOWNLOADS)
                    .label("Download")
                    .build(),
            )
            .valign(gtk::Align::Center)
            .css_classes(["action", "outlined"])
            .build();
        button.connect_clicked(move |_| open());
        let view = StatusView::new(act);
        let status = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(10)
            .visible(false)
            .css_classes(["card", "download-status"])
            .build();
        let inner = gtk::Box::builder()
            .spacing(16)
            .margin_top(12)
            .margin_bottom(12)
            .margin_start(16)
            .margin_end(16)
            .build();
        inner.append(&gtk::Image::from_icon_name(crate::icons::DOWNLOADS));
        inner.append(&view.text);
        inner.append(&view.actions);
        status.append(&inner);
        Rc::new(ItemDownload {
            item,
            button,
            status,
            view,
        })
    }

    /// Shows `download`, or the Download button without one.
    pub fn show(&self, download: Option<&LocalDownload>) {
        self.button.set_visible(download.is_none());
        self.status.set_visible(download.is_some());
        if let Some(download) = download {
            self.view.show(download);
        }
    }

    /// Whether its page is still in the window: one left is dropped.
    pub fn in_window(&self) -> bool {
        self.button.root().is_some() || self.status.root().is_some()
    }
}

/// One section of the Downloads page: In progress or Downloaded.
struct Section {
    widget: gtk::Box,
    count: gtk::Label,
    list: gtk::ListBox,
}

impl Section {
    fn new(title: &str) -> Self {
        let widget = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(10)
            .build();
        let head = gtk::Box::builder().spacing(10).build();
        head.append(
            &gtk::Label::builder()
                .label(title)
                .xalign(0.0)
                .css_classes(["title-3"])
                .build(),
        );
        let count = gtk::Label::builder()
            .xalign(0.0)
            .valign(gtk::Align::Baseline)
            .css_classes(["dim-label"])
            .build();
        head.append(&count);
        let list = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::None)
            .css_classes(["boxed-list"])
            .build();
        widget.append(&head);
        widget.append(&list);
        Section {
            widget,
            count,
            list,
        }
    }
}

/// A download's row on the Downloads page.
struct PageRow {
    key: String,
    finished: bool,
    row: gtk::ListBoxRow,
    title: gtk::Label,
    info: gtk::Label,
    view: StatusView,
}

/// The Downloads page: how much room the downloads take, then In
/// progress and Downloaded, each a list of rows changed in place.
pub struct DownloadsPage {
    pub widget: gtk::Box,
    client: Arc<dyn Client>,
    act: Acts,
    used: gtk::Label,
    free: gtk::Label,
    bar: gtk::LevelBar,
    running: Section,
    done: Section,
    empty: adw::StatusPage,
    /// What waits for the server, on the banner over the page while no
    /// server can be reached.
    offline: RefCell<Option<gtk::Label>>,
    downloads: RefCell<Vec<LocalDownload>>,
    rows: RefCell<Vec<PageRow>>,
}

/// The banner over the Downloads page with no server (the offline board):
/// which can't be reached, and its Try now.
pub struct Offline {
    /// "Home Media can't be reached".
    pub heading: String,
    pub try_now: Rc<dyn Fn()>,
    /// The profiles with downloads here, and the one shown: offered to
    /// choose from when there is more than one (PX-60).
    pub profiles: Vec<Holder>,
    pub shown: Option<Holder>,
    pub choose: Rc<dyn Fn(Holder)>,
}

impl DownloadsPage {
    pub fn new(client: Arc<dyn Client>, act: Acts) -> Rc<Self> {
        let widget = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(20)
            .build();
        // How much room they take, and where, as the board's top card.
        let card = gtk::Box::builder()
            .spacing(16)
            .css_classes(["card", "download-space"])
            .build();
        let inner = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(6)
            .hexpand(true)
            .margin_top(14)
            .margin_bottom(14)
            .margin_end(16)
            .build();
        let line = gtk::Box::builder().spacing(12).build();
        let used = gtk::Label::builder()
            .xalign(0.0)
            .hexpand(true)
            .ellipsize(gtk::pango::EllipsizeMode::Middle)
            .build();
        let free = gtk::Label::builder()
            .xalign(1.0)
            .css_classes(["dim-label"])
            .build();
        line.append(&used);
        line.append(&free);
        let bar = gtk::LevelBar::builder()
            .min_value(0.0)
            .max_value(1.0)
            .build();
        inner.append(&line);
        inner.append(&bar);
        let icon = gtk::Image::builder()
            .icon_name(crate::icons::DOWNLOADS)
            .pixel_size(22)
            .margin_start(16)
            .css_classes(["accent"])
            .build();
        card.append(&icon);
        card.append(&inner);
        widget.append(&card);
        let running = Section::new("In progress");
        let done = Section::new("Downloaded");
        let empty = adw::StatusPage::builder()
            .icon_name(crate::icons::DOWNLOADS)
            .title("No downloads")
            .description("Download a film or an episode from its page to watch it with no server.")
            .visible(false)
            .build();
        widget.append(&running.widget);
        widget.append(&done.widget);
        widget.append(&empty);
        widget.append(
            &gtk::Label::builder()
                .label(
                    "Downloads go on while the window is closed, as long as Spoolframe runs. \
                     Anyone who can play it can download it, and a download never expires.",
                )
                .xalign(0.0)
                .wrap(true)
                .css_classes(["dim-label", "caption"])
                .build(),
        );
        Rc::new(DownloadsPage {
            widget,
            client,
            act,
            used,
            free,
            bar,
            running,
            done,
            empty,
            offline: RefCell::default(),
            downloads: RefCell::default(),
            rows: RefCell::default(),
        })
    }

    /// Opens with no server (the offline board): the banner over the
    /// page says the downloads still play, and what waits to be sent.
    pub fn set_offline(&self, offline: Offline) {
        let card = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(6)
            .css_classes(["card", "unreachable-banner"])
            .build();
        let heading = gtk::Label::builder()
            .xalign(0.0)
            .wrap(true)
            .css_classes(["heading"])
            .build();
        heading.set_text(&offline.heading);
        let body = gtk::Label::builder()
            .xalign(0.0)
            .wrap(true)
            .css_classes(["dim-label"])
            .build();
        let try_now = gtk::Button::builder()
            .label("Try now")
            .halign(gtk::Align::End)
            .css_classes(["suggested-action"])
            .build();
        let again = offline.try_now.clone();
        try_now.connect_clicked(move |_| again());
        card.append(&heading);
        card.append(&body);
        if let Some(whose) = profile_choice(&offline) {
            card.append(&whose);
        }
        card.append(&try_now);
        self.widget.prepend(&card);
        self.offline.replace(Some(body));
        self.counted();
    }

    /// Lists `downloads` afresh: at first, and after events were missed.
    pub fn fill(self: &Rc<Self>, downloads: &[LocalDownload]) {
        for row in self.rows.borrow_mut().drain(..) {
            self.section(row.finished).list.remove(&row.row);
        }
        *self.downloads.borrow_mut() = downloads.to_vec();
        for download in downloads {
            self.add(download);
        }
        self.counted();
        self.load_space();
    }

    /// One download changed or went: its row alone is changed, moved or
    /// taken away. The room is read again when the phase changes, not for
    /// each piece read.
    pub fn apply(self: &Rc<Self>, event: &DownloadEvent) {
        let before = match event {
            DownloadEvent::Changed(d) => self
                .downloads
                .borrow()
                .iter()
                .find(|listed| listed.key == d.key)
                .map(|listed| std::mem::discriminant(&listed.phase)),
            DownloadEvent::Removed { .. } => None,
        };
        let applied = apply(&mut self.downloads.borrow_mut(), event);
        match (applied, event) {
            (Applied::Updated, DownloadEvent::Changed(download)) => {
                let moved = {
                    let rows = self.rows.borrow();
                    let row = rows.iter().find(|r| r.key == download.key);
                    match row {
                        Some(row) if row.finished == finished(download) => {
                            row.view.show(download);
                            row.title.set_label(&title(download));
                            row.info.set_label(&info_line(download));
                            false
                        }
                        _ => true,
                    }
                };
                if moved {
                    self.remove(&download.key);
                    self.add(download);
                }
                if before != Some(std::mem::discriminant(&download.phase)) {
                    self.load_space();
                }
            }
            (Applied::Added, DownloadEvent::Changed(download)) => {
                self.add(download);
                self.load_space();
            }
            (Applied::Removed, DownloadEvent::Removed { key }) => {
                self.remove(key);
                self.load_space();
            }
            _ => {}
        }
        self.counted();
    }

    fn section(&self, finished: bool) -> &Section {
        if finished { &self.done } else { &self.running }
    }

    /// A row for `download`, at its place in its section.
    fn add(&self, download: &LocalDownload) {
        let view = StatusView::new(self.act.clone());
        view.show(download);
        let title = gtk::Label::builder()
            .label(title(download))
            .xalign(0.0)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .css_classes(["heading"])
            .build();
        let info = gtk::Label::builder()
            .label(info_line(download))
            .xalign(0.0)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .css_classes(["dim-label", "caption"])
            .build();
        // The title over the phase, as the board's rows.
        let texts = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(4)
            .hexpand(true)
            .build();
        texts.append(&title);
        texts.append(&info);
        texts.append(&view.text);
        let body = gtk::Box::builder()
            .spacing(16)
            .margin_top(12)
            .margin_bottom(12)
            .margin_start(16)
            .margin_end(16)
            .build();
        body.append(&poster(download));
        body.append(&texts);
        body.append(&view.actions);
        let row = gtk::ListBoxRow::builder()
            .child(&body)
            .activatable(false)
            .build();
        let done = finished(download);
        let at = place(&self.downloads.borrow(), &download.key);
        self.section(done).list.insert(&row, at as i32);
        self.rows.borrow_mut().push(PageRow {
            key: download.key.clone(),
            finished: done,
            row,
            title,
            info,
            view,
        });
    }

    fn remove(&self, key: &str) {
        let mut rows = self.rows.borrow_mut();
        if let Some(at) = rows.iter().position(|r| r.key == key) {
            let row = rows.remove(at);
            self.section(row.finished).list.remove(&row.row);
        }
    }

    /// The sections' counts, and the empty page when there is nothing.
    fn counted(&self) {
        let downloads = self.downloads.borrow();
        let (done, running): (Vec<_>, Vec<_>) = downloads.iter().partition(|d| finished(d));
        self.running.count.set_label(&running.len().to_string());
        self.running.widget.set_visible(!running.is_empty());
        let bytes: u64 = done.iter().map(|d| d.size.unwrap_or(d.received)).sum();
        self.done
            .count
            .set_label(&format!("{} · {}", done.len(), size(bytes)));
        self.done.widget.set_visible(!done.is_empty());
        self.empty.set_visible(downloads.is_empty());
        if let Some(body) = self.offline.borrow().as_ref() {
            body.set_text(&match pending_line(&downloads) {
                Some(pending) => format!("Your downloads still play. {pending}"),
                None => "Your downloads still play.".to_owned(),
            });
        }
    }

    fn load_space(self: &Rc<Self>) {
        let client = self.client.clone();
        let asked = relm4::spawn(async move { client.download_space().await });
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let Ok(space) = asked.await else { return };
            let Some(page) = weak.upgrade() else { return };
            match space {
                Ok(space) => page.show_space(space),
                Err(e) => tracing::warn!(error = %e, "cannot read the downloads' room"),
            }
        });
    }

    fn show_space(&self, space: DownloadSpace) {
        self.used
            .set_label(&space_line(space, crate::downloads_dir().as_deref()));
        self.free
            .set_label(&format!("{} free", size(space.available)));
        self.bar.set_value(used_fraction(space));
    }

    /// The title of each row, in the order shown, for tests.
    #[cfg(test)]
    fn titles(&self) -> Vec<(bool, String)> {
        let rows = self.rows.borrow();
        let mut shown = Vec::new();
        for section in [&self.running, &self.done] {
            let mut child = section.list.first_child();
            while let Some(widget) = child {
                if let Some(row) = rows
                    .iter()
                    .find(|r| r.row.upcast_ref::<gtk::Widget>() == &widget)
                {
                    shown.push((row.finished, row.title.label().to_string()));
                }
                child = widget.next_sibling();
            }
        }
        shown
    }
}

/// "Downloads of [Andrey ▾]" on the offline banner: the profile shown, of
/// those with downloads here. None with one profile or none.
fn profile_choice(offline: &Offline) -> Option<gtk::Box> {
    if offline.profiles.len() < 2 {
        return None;
    }
    let names: Vec<&str> = offline
        .profiles
        .iter()
        .map(|p| p.username.as_str())
        .collect();
    let choice = gtk::DropDown::from_strings(&names);
    let at = offline
        .shown
        .as_ref()
        .and_then(|shown| offline.profiles.iter().position(|p| p.same(shown)))
        .unwrap_or(0);
    choice.set_selected(at as u32);
    let (profiles, choose) = (offline.profiles.clone(), offline.choose.clone());
    choice.connect_selected_notify(move |choice| {
        if let Some(profile) = profiles.get(choice.selected() as usize) {
            choose(profile.clone());
        }
    });
    let line = gtk::Box::builder().spacing(8).build();
    line.append(
        &gtk::Label::builder()
            .label("Downloads of")
            .css_classes(["dim-label"])
            .build(),
    );
    line.append(&choice);
    Some(line)
}

/// The Downloads page as a page of its own, over the window, while no
/// server can be reached (PX-38): `back` goes to Choose a server.
pub fn offline_page(page: &DownloadsPage, back: Rc<dyn Fn()>) -> adw::NavigationPage {
    let header = adw::HeaderBar::new();
    let servers = gtk::Button::builder()
        .child(
            &adw::ButtonContent::builder()
                .icon_name(crate::icons::BACK)
                .label("Servers")
                .build(),
        )
        .tooltip_text("Choose a server")
        .css_classes(["flat"])
        .build();
    servers.connect_clicked(move |_| back());
    header.pack_start(&servers);
    let clamp = adw::Clamp::builder()
        .maximum_size(960)
        .child(&page.widget)
        .margin_top(28)
        .margin_bottom(28)
        .margin_start(28)
        .margin_end(28)
        .build();
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header);
    toolbar.set_content(Some(
        &gtk::ScrolledWindow::builder()
            .child(&clamp)
            .vexpand(true)
            .build(),
    ));
    adw::NavigationPage::builder()
        .title("Downloads")
        .tag(OFFLINE_TAG)
        .can_pop(false)
        .child(&toolbar)
        .build()
}

/// The offline Downloads page's tag on the window's stack.
pub const OFFLINE_TAG: &str = "offline-downloads";

/// "41.2 GB in ~/.local/share/spoolframe/downloads".
pub fn space_line(space: DownloadSpace, dir: Option<&std::path::Path>) -> String {
    match dir {
        Some(dir) => format!("{} in {}", size(space.used), home_relative(dir)),
        None => size(space.used),
    }
}

/// The share of the room the downloads take, for the bar.
pub fn used_fraction(space: DownloadSpace) -> f64 {
    let whole = space.used.saturating_add(space.available);
    if whole == 0 {
        0.0
    } else {
        space.used as f64 / whole as f64
    }
}

/// `path` with the home directory as "~".
fn home_relative(path: &std::path::Path) -> String {
    let home = glib::home_dir();
    match path.strip_prefix(&home) {
        Ok(rest) => format!("~/{}", rest.display()),
        Err(_) => path.display().to_string(),
    }
}

/// The download's poster, read from this computer: it shows offline.
fn poster(download: &LocalDownload) -> gtk::Widget {
    const SIZE: (i32, i32) = (36, 54);
    let pick = |kind: ArtworkKind| download.artwork.iter().find(|a| a.kind == kind);
    let art = pick(ArtworkKind::Poster)
        .or_else(|| pick(ArtworkKind::Thumb))
        .or_else(|| download.artwork.first());
    match art {
        Some(art) => {
            let picture = gtk::Picture::for_filename(&art.path);
            picture.set_content_fit(gtk::ContentFit::Cover);
            picture.set_size_request(SIZE.0, SIZE.1);
            picture.set_valign(gtk::Align::Center);
            picture.set_overflow(gtk::Overflow::Hidden);
            picture.add_css_class("download-poster");
            picture.upcast()
        }
        None => {
            let tile = crate::views::tile(download.item.id(), "", "poster", SIZE);
            tile.set_valign(gtk::Align::Center);
            tile.upcast()
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use spoolframe_linux_core::backend::{DownloadTracks, Session};
    use spoolframe_linux_core::fake::FakeClient;
    use spoolframe_linux_core::testing::ready;
    use spoolframe_types::{MediaKind, Namespace};

    use super::*;

    fn id(kind: MediaKind, key: &str) -> MediaId {
        MediaId::new(Namespace::Plex, kind, key).unwrap()
    }

    fn alien() -> MediaId {
        id(MediaKind::Movie, "1")
    }

    fn andrey(client: &FakeClient) -> Arc<dyn Session> {
        client.session(&FakeClient::profile("andrey")).unwrap()
    }

    fn started(client: &FakeClient, item: &MediaId) -> LocalDownload {
        ready(andrey(client).start_download(item, DownloadSource::Original, DownloadTracks::All))
            .unwrap()
    }

    fn in_phase(phase: DownloadPhase) -> LocalDownload {
        let client = FakeClient::new(None);
        LocalDownload {
            phase,
            ..started(&client, &alien())
        }
    }

    fn preparing(server: DownloadState) -> LocalDownload {
        in_phase(DownloadPhase::Preparing { server })
    }

    #[test]
    fn a_conversion_failure_on_the_server_offers_no_retry() {
        let failed = in_phase(DownloadPhase::Failed {
            reason: "the conversion failed".into(),
        });
        assert_eq!(
            (retryable(&failed), status(&failed).acts),
            (false, vec![Act::Dismiss])
        );
    }

    #[test]
    fn a_failure_on_the_way_here_offers_retry() {
        let failed = LocalDownload {
            size: Some(3_600_000_000),
            received: 1_200_000_000,
            ..in_phase(DownloadPhase::Failed {
                reason: "The file arrived damaged; try again".into(),
            })
        };
        assert_eq!(
            (retryable(&failed), status(&failed).acts),
            (true, vec![Act::Retry, Act::Dismiss])
        );
    }

    #[test]
    fn a_download_under_way_is_never_retryable() {
        let moving = LocalDownload {
            size: Some(10),
            ..in_phase(DownloadPhase::Transferring)
        };
        assert!(!retryable(&moving));
    }

    #[test]
    fn a_failed_download_says_why() {
        let failed = in_phase(DownloadPhase::Failed {
            reason: "the file is gone".into(),
        });
        let shown = status(&failed);
        assert_eq!(
            (shown.heading.as_str(), shown.detail.as_deref()),
            ("Couldn’t download", Some("the file is gone"))
        );
    }

    #[test]
    fn waiting_says_so_and_only_cancels() {
        let shown = status(&preparing(DownloadState::Waiting));
        assert_eq!(
            (shown.heading.as_str(), shown.progress, shown.acts),
            ("Waiting on the server", None, vec![Act::Cancel])
        );
    }

    #[test]
    fn converting_shows_its_share_and_the_time_left() {
        let shown = status(&LocalDownload {
            source: DownloadSource::Profile {
                name: "Mobile".into(),
            },
            ..preparing(DownloadState::Converting {
                done_percent: Some(42),
                eta_secs: Some(14 * 60),
            })
        });
        assert_eq!(
            (
                shown.heading.as_str(),
                shown.detail.as_deref(),
                shown.progress
            ),
            (
                "Converting on the server · 42%",
                Some("Mobile · about 14 min left"),
                Some(0.42)
            )
        );
    }

    #[test]
    fn writing_on_the_server_shows_its_share() {
        let shown = status(&preparing(DownloadState::Writing {
            done_percent: Some(10),
        }));
        assert_eq!(
            (shown.heading.as_str(), shown.progress),
            ("Writing on the server · 10%", Some(0.1))
        );
    }

    #[test]
    fn downloading_shows_what_arrived_and_pauses() {
        let shown = status(&LocalDownload {
            size: Some(3_600_000_000),
            received: 2_300_000_000,
            ..in_phase(DownloadPhase::Transferring)
        });
        assert_eq!(
            (shown.heading.as_str(), shown.detail.as_deref(), shown.acts),
            (
                "Downloading · 63%",
                Some("2.3 of 3.6 GB"),
                vec![Act::Pause, Act::Cancel]
            )
        );
    }

    #[test]
    fn the_sdk_waiting_to_try_again_says_why_in_place_of_the_amount() {
        let shown = status(&LocalDownload {
            size: Some(100),
            received: 10,
            note: Some("Waiting for the server: it cannot be reached".into()),
            ..in_phase(DownloadPhase::Transferring)
        });
        assert_eq!(
            shown.detail.as_deref(),
            Some("Waiting for the server: it cannot be reached")
        );
    }

    #[test]
    fn paused_resumes_from_where_it_got_to() {
        let shown = status(&LocalDownload {
            size: Some(1_000),
            received: 200,
            ..in_phase(DownloadPhase::Paused)
        });
        assert_eq!(
            (shown.heading.as_str(), shown.acts),
            ("Paused at 20%", vec![Act::Resume, Act::Cancel])
        );
    }

    #[test]
    fn downloaded_says_its_size_and_deletes() {
        let shown = status(&LocalDownload {
            size: Some(3_400_000_000),
            received: 3_400_000_000,
            ..done_with(watched(None, None, false))
        });
        assert_eq!(
            (shown.heading.as_str(), shown.detail.as_deref(), shown.acts),
            (
                "Downloaded · 3.4 GB",
                Some("Original · all tracks"),
                vec![Act::Play, Act::Delete]
            )
        );
    }

    fn watched(position_ms: Option<u64>, watched: Option<bool>, pending: bool) -> LocalWatch {
        LocalWatch {
            position_ms,
            watched,
            at: 0,
            pending,
        }
    }

    fn done_with(watch: LocalWatch) -> LocalDownload {
        LocalDownload {
            watch,
            ..in_phase(DownloadPhase::Done)
        }
    }

    #[test]
    fn a_download_started_says_how_much_is_left() {
        let download = done_with(watched(Some(10 * 60 * 1000), None, false));
        let length = crate::views::Facts::of(&download.item).length.unwrap();
        assert_eq!(
            watch_line(&download),
            Some(format::remaining(length, Duration::from_secs(600)))
        );
    }

    #[test]
    fn a_download_watched_here_says_so_and_that_it_waits() {
        let download = done_with(watched(None, Some(true), true));
        assert_eq!(
            watch_line(&download).as_deref(),
            Some("Watched, not sent yet")
        );
    }

    #[test]
    fn a_download_never_started_has_no_watch_line() {
        assert_eq!(watch_line(&done_with(watched(None, None, false))), None);
    }

    #[test]
    fn a_finished_downloads_line_carries_how_far_it_was_watched() {
        let shown = status(&done_with(watched(None, Some(true), false)));
        assert_eq!(
            shown.detail.as_deref(),
            Some("Original · all tracks · Watched")
        );
    }

    #[test]
    fn its_own_play_resumes_where_it_stopped_and_starts_over_once_watched() {
        assert_eq!(
            (
                resume_point(&watched(Some(5_000), None, true)),
                resume_point(&watched(None, Some(true), false))
            ),
            (Some(Duration::from_secs(5)), None)
        );
    }

    #[test]
    fn resume_from_the_page_takes_the_place_the_server_has_not_heard() {
        let asked = Some(Duration::from_secs(60));
        assert_eq!(
            (
                start_from(&watched(Some(90_000), None, true), asked),
                start_from(&watched(Some(90_000), None, false), asked),
                start_from(&watched(Some(90_000), None, true), None)
            ),
            (Some(Duration::from_secs(90)), asked, None)
        );
    }

    #[test]
    fn only_a_finished_download_plays_in_place_of_the_server() {
        let client = FakeClient::new(None);
        let first = started(&client, &alien());
        let listed = ready(client.downloads());
        let before = playable_here(&listed, &alien()).is_some();
        client.finish_download(&first.key);
        let listed = ready(client.downloads());
        let mut finished = listed.clone();
        // A file the fake serves for it: none here, so one is named.
        finished[0].file_url = Some("file:///tmp/alien.mkv".into());
        assert_eq!(
            (
                before,
                playable_here(&finished, &alien()).map(|d| d.key.clone())
            ),
            (false, Some(first.key))
        );
    }

    #[test]
    fn nothing_waiting_for_the_server_says_nothing() {
        assert_eq!(pending_line(&[done_with(watched(None, None, false))]), None);
    }

    #[test]
    fn what_waits_for_the_server_is_named() {
        let alien = done_with(watched(None, Some(true), true));
        let heat = done_with(watched(Some(4_360_000), None, true));
        let name = title(&alien);
        assert_eq!(
            pending_line(&[alien, heat]),
            Some(format!(
                "2 changes to send: {name} watched, {name} at 1:12:40. They go when \
                 the server is back."
            ))
        );
    }

    #[test]
    fn sizes_read_as_file_managers_write_them() {
        assert_eq!(
            [999, 74_000, 740_000_000, 38_400_000_000].map(size),
            ["999 bytes", "74 kB", "740 MB", "38.4 GB"]
        );
    }

    #[test]
    fn an_episode_is_named_with_its_show_and_place() {
        let client = FakeClient::new(None);
        let episode = started(&client, &id(MediaKind::Episode, "65"));
        let Item::Episode(e) = &episode.item else {
            panic!("65 is an episode")
        };
        assert_eq!(
            title(&episode),
            format!(
                "{} · S{} E{} · {}",
                e.show_title.clone().unwrap(),
                e.season,
                e.episode,
                e.title
            )
        );
    }

    fn bobs_episode(client: &FakeClient) -> LocalDownload {
        ready(
            client
                .session(&FakeClient::profile("bob"))
                .unwrap()
                .start_download(
                    &id(MediaKind::Episode, "65"),
                    DownloadSource::Original,
                    DownloadTracks::All,
                ),
        )
        .unwrap()
    }

    fn andrey_shown(client: &FakeClient) {
        client.set_download_profile(&FakeClient::profile("andrey").server.0, "andrey");
    }

    #[test]
    fn an_items_page_shows_the_latest_download_of_it() {
        let client = FakeClient::new(None);
        let mobile = ready(andrey(&client).start_download(
            &alien(),
            DownloadSource::Profile {
                name: "Mobile".into(),
            },
            DownloadTracks::All,
        ))
        .unwrap();
        started(&client, &alien());
        // The Mobile one newer, though listed first.
        let listed: Vec<LocalDownload> = ready(client.downloads())
            .into_iter()
            .map(|d| LocalDownload {
                created_at: d.created_at + i64::from(d.key == mobile.key),
                ..d
            })
            .collect();
        assert_eq!(
            for_item(&listed, &alien()).map(|d| d.key.clone()),
            Some(mobile.key)
        );
    }

    #[test]
    fn the_profiles_with_downloads_are_each_named_once() {
        let client = FakeClient::new(None);
        started(&client, &alien());
        started(&client, &id(MediaKind::Movie, "2"));
        bobs_episode(&client);
        let named: Vec<String> = holders(Vec::new(), &ready(client.downloads()))
            .into_iter()
            .map(|h| h.username)
            .collect();
        assert_eq!(named, ["andrey", "bob"]);
    }

    fn holder(user: &str) -> Holder {
        Holder {
            server: "home".into(),
            user_id: user.into(),
            username: user.into(),
        }
    }

    #[test]
    fn offline_the_profile_used_last_is_shown_if_it_has_downloads() {
        let known = [holder("andrey"), holder("bob")];
        assert_eq!(
            (
                offline_holder(&known, Some(holder("bob"))),
                offline_holder(&known, Some(holder("carol"))),
                offline_holder(&[], Some(holder("carol"))),
                offline_holder(&known, None),
            ),
            (
                Some(holder("bob")),
                Some(holder("andrey")),
                Some(holder("carol")),
                Some(holder("andrey"))
            )
        );
    }

    #[test]
    fn a_changed_download_is_changed_in_place() {
        let client = FakeClient::new(None);
        let first = started(&client, &alien());
        let mut listed = ready(client.downloads());
        client.advance_download(&first.key, 500);
        let changed = ready(client.download(&first.key)).unwrap();
        assert_eq!(
            (
                apply(&mut listed, &DownloadEvent::Changed(changed.clone())),
                listed
            ),
            (Applied::Updated, vec![changed])
        );
    }

    #[test]
    fn a_removed_download_leaves_the_list() {
        let client = FakeClient::new(None);
        let first = started(&client, &alien());
        let mut listed = ready(client.downloads());
        let removed = DownloadEvent::Removed {
            key: first.key.clone(),
        };
        assert_eq!(
            (
                apply(&mut listed, &removed),
                listed.len(),
                apply(&mut listed, &removed)
            ),
            (Applied::Removed, 0, Applied::Nothing)
        );
    }

    #[test]
    fn a_new_download_is_added() {
        let client = FakeClient::new(None);
        let mut listed = Vec::new();
        let first = started(&client, &alien());
        assert_eq!(
            apply(&mut listed, &DownloadEvent::Changed(first)),
            Applied::Added
        );
    }

    #[test]
    fn the_bar_shows_the_share_the_downloads_take() {
        let space = DownloadSpace {
            available: 75,
            used: 25,
        };
        assert_eq!(used_fraction(space), 0.25);
    }

    /// With no server, the page lists the profile shown's downloads alone
    /// (PX-60), a finished one with Play, the banner says what waits to be
    /// sent, and offers the other profile with downloads here.
    #[test]
    #[ignore = "needs a display; run on its own"]
    fn the_page_lists_and_plays_with_no_session() {
        gtk::init().unwrap();
        adw::init().unwrap();
        let client = Arc::new(FakeClient::new(None));
        let first = started(&client, &alien());
        client.finish_download(&first.key);
        ready(client.record_download_position(&first.key, Duration::from_secs(60))).unwrap();
        let bobs = bobs_episode(&client);
        let known = holders(Vec::new(), &ready(client.downloads()));
        andrey_shown(&client);
        let played = Rc::new(RefCell::new(Vec::new()));
        let acts: Acts = {
            let played = played.clone();
            Rc::new(move |download, act| played.borrow_mut().push((download.key, act)))
        };
        let chosen = Rc::new(RefCell::new(Vec::new()));
        let page = DownloadsPage::new(client.clone(), acts);
        page.set_offline(Offline {
            heading: "Home Media can’t be reached".into(),
            try_now: Rc::new(|| {}),
            shown: known.first().cloned(),
            profiles: known,
            choose: {
                let chosen = chosen.clone();
                Rc::new(move |h: Holder| chosen.borrow_mut().push(h.username))
            },
        });
        page.fill(&ready(client.downloads()));
        let row = page
            .rows
            .borrow()
            .iter()
            .position(|r| r.key == first.key)
            .unwrap();
        let play = page.rows.borrow()[row]
            .view
            .actions
            .first_child()
            .and_downcast::<gtk::Button>()
            .unwrap();
        play.emit_clicked();
        let banner = page.offline.borrow().as_ref().unwrap().text();
        let choice = find_drop_down(page.widget.upcast_ref()).unwrap();
        choice.set_selected(1);
        assert_eq!(
            (
                page.titles().len(),
                played.borrow().clone(),
                banner.contains("1 change to send"),
                page.rows.borrow().iter().any(|r| r.key == bobs.key),
                chosen.borrow().clone()
            ),
            (
                1,
                vec![(first.key.clone(), Act::Play)],
                true,
                false,
                vec!["bob".to_owned()]
            )
        );
    }

    fn find_drop_down(widget: &gtk::Widget) -> Option<gtk::DropDown> {
        if let Some(found) = widget.downcast_ref::<gtk::DropDown>() {
            return Some(found.clone());
        }
        let mut child = widget.first_child();
        while let Some(widget) = child {
            if let Some(found) = find_drop_down(&widget) {
                return Some(found);
            }
            child = widget.next_sibling();
        }
        None
    }

    /// Another profile's download moving on never reaches the page of the
    /// profile shown: its events are not sent (PX-60).
    #[test]
    #[ignore = "needs a display; run on its own"]
    fn another_profiles_events_do_not_reach_the_page() {
        gtk::init().unwrap();
        adw::init().unwrap();
        let client = Arc::new(FakeClient::new(None));
        let mine = started(&client, &alien());
        let bobs = bobs_episode(&client);
        andrey_shown(&client);
        let page = DownloadsPage::new(client.clone(), Rc::new(|_, _| {}));
        page.fill(&ready(client.downloads()));
        let mut events = client.download_events();
        client.advance_download(&bobs.key, 500);
        client.finish_download(&bobs.key);
        while let Ok(event) = events.try_recv() {
            page.apply(&event);
        }
        assert_eq!(page.titles(), [(false, title(&mine))]);
    }

    /// The page with the fake: a Changed event changes the row it has, a
    /// finished one moves to Downloaded, and Removed takes it away.
    #[test]
    #[ignore = "needs a display; run on its own"]
    fn the_page_changes_rows_in_place() {
        gtk::init().unwrap();
        adw::init().unwrap();
        let client = Arc::new(FakeClient::new(None));
        let first = started(&client, &alien());
        let second = started(&client, &id(MediaKind::Episode, "65"));
        andrey_shown(&client);
        let page = DownloadsPage::new(client.clone(), Rc::new(|_, _| {}));
        page.fill(&ready(client.downloads()));
        let row_of = |key: &str| {
            page.rows
                .borrow()
                .iter()
                .find(|r| r.key == key)
                .map(|r| r.row.clone())
        };
        let before = row_of(&first.key).unwrap();

        client.advance_download(&first.key, 500_000);
        let moving = ready(client.download(&first.key)).unwrap();
        page.apply(&DownloadEvent::Changed(moving));
        let after = row_of(&first.key).unwrap();
        let heading = page.rows.borrow()[0].view.heading.label();
        assert_eq!(
            (before == after, heading.as_str()),
            (true, "Downloading · 50%")
        );

        client.finish_download(&second.key);
        page.apply(&DownloadEvent::Changed(
            ready(client.download(&second.key)).unwrap(),
        ));
        assert_eq!(
            page.titles()
                .into_iter()
                .map(|(done, _)| done)
                .collect::<Vec<_>>(),
            [false, true]
        );

        page.apply(&DownloadEvent::Removed {
            key: first.key.clone(),
        });
        assert_eq!((row_of(&first.key), page.titles().len()), (None, 1));
    }
}
