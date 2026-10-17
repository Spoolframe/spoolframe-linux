//! "Search for subtitles…" (PX-02), as the boards LinuxSubtitleSearch,
//! LinuxPlayerSubtitleSearch and LinuxSubtitleSearchStates have it: the
//! same panel from the item page's subtitle picker and from the player's
//! subtitle menu. The server searches its subtitle sites (OpenSubtitles,
//! SubDL) and downloads; this only shows what it says. A result picked
//! is downloaded as a track of the item, for everyone on the server, and
//! handed back by its place in the item's subtitles.
//!
//! Where the boards and the SDK differ, the SDK wins: there is no title
//! to edit (the server searches by the file and its catalog entry), a
//! download is everyone's, not the user's alone, and results come from
//! two sites, each named on its row.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;

use adw::prelude::*;
use gtk::glib;
use spoolframe_linux_core::backend::{
    BackendError, QuotaOf, Session, SubtitleDownload, SubtitleOrigin, SubtitleResult,
    SubtitleSearch, SubtitleSource, SubtitleSourceState, SubtitleSourceStatus,
};
use spoolframe_linux_core::format;
use spoolframe_types::MediaId;

/// Languages offered beside those the server searched first: the ones
/// both sites have most subtitles in.
const COMMON: [&str; 18] = [
    "en", "es", "fr", "de", "it", "pt", "pt-BR", "nl", "sv", "no", "da", "fi", "pl", "ru", "uk",
    "ja", "ko", "zh",
];

/// "31,402".
pub fn thousands(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// What a result's second line says: its language and site, then what
/// marks it (hearing impaired, who made it), its rating and downloads.
pub fn result_line(result: &SubtitleResult) -> String {
    let mut parts = vec![
        format::language(&result.language),
        SubtitleSource::name_of(&result.source).to_owned(),
    ];
    if result.hearing_impaired {
        parts.push("HI".into());
    }
    match result.origin {
        SubtitleOrigin::MachineTranslated => parts.push("Machine-translated".into()),
        SubtitleOrigin::Ai => parts.push("Made by AI".into()),
        SubtitleOrigin::Human => {}
    }
    if let Some(rating) = result.rating {
        parts.push(format!("Rated {}.{}", rating / 10, rating % 10));
    }
    match result.downloads {
        0 => {}
        1 => parts.push("1 download".into()),
        n => parts.push(format!("{} downloads", thousands(n))),
    }
    parts.join(" · ")
}

/// What a result says under its line, if anything: downloaded already,
/// or removed before as a bad one.
pub fn result_note(result: &SubtitleResult) -> Option<&'static str> {
    match (result.rejected, result.downloaded) {
        (true, _) => Some("Removed before as a bad match"),
        (false, true) => Some("Downloaded already: choosing it picks that track"),
        (false, false) => None,
    }
}

/// The results in the server's order, with those removed before as bad
/// last, whatever the order they came in.
pub fn ordered(results: &[SubtitleResult]) -> Vec<&SubtitleResult> {
    let mut ordered: Vec<&SubtitleResult> = results.iter().collect();
    ordered.sort_by_key(|r| r.rejected);
    ordered
}

/// "6 results · files that match this one come first".
pub fn count_line(results: &[SubtitleResult]) -> String {
    let count = match results.len() {
        1 => "1 result".to_owned(),
        n => format!("{n} results"),
    };
    match results.iter().any(|r| r.hash_match) {
        true => format!("{count} · files that match this one come first"),
        false => count,
    }
}

/// "From OpenSubtitles and SubDL": the sites the server asked.
pub fn sources_line(sources: &[SubtitleSourceStatus]) -> Option<String> {
    let names: Vec<&str> = sources
        .iter()
        .map(|s| SubtitleSource::name_of(&s.source))
        .collect();
    match names.as_slice() {
        [] => None,
        [one] => Some(format!("From {one}")),
        [rest @ .., last] => Some(format!("From {} and {last}", rest.join(", "))),
    }
}

/// "14:00 UTC", from Unix seconds.
pub fn utc_time(at: i64) -> String {
    let day = at.rem_euclid(86_400);
    format!("{:02}:{:02} UTC", day / 3600, day / 60 % 60)
}

/// What a site's part of a search says, where it did not just answer.
pub fn source_problem(status: &SubtitleSourceStatus) -> Option<String> {
    let name = SubtitleSource::name_of(&status.source);
    match &status.state {
        SubtitleSourceState::Found { .. } | SubtitleSourceState::Unknown => None,
        SubtitleSourceState::QuotaExhausted { resets_at } => Some(match resets_at {
            Some(at) => format!(
                "{name} has no downloads left today; more after {}.",
                utc_time(*at)
            ),
            None => format!("{name} has no downloads left today."),
        }),
        SubtitleSourceState::RateLimited { retry_after_ms } => Some(format!(
            "{name} asked to wait {} s; its results may be missing.",
            retry_after_ms.div_ceil(1000).max(1)
        )),
        SubtitleSourceState::Failed { message } => {
            Some(format!("{name} can’t be reached: {message}"))
        }
    }
}

/// Why there is nothing to show: a title, what to do, and whether Try
/// again can help.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Problem {
    pub title: String,
    pub detail: String,
    pub retry: bool,
}

/// A search refused, as the states board words it, for server `server`.
pub fn search_problem(error: &BackendError, server: &str) -> Problem {
    let problem = |title: String, detail: &str, retry| Problem {
        title,
        detail: detail.to_owned(),
        retry,
    };
    match error {
        BackendError::NoSubtitleSource => problem(
            format!("Subtitle search is off on {server}"),
            "Ask the server’s owner to turn on subtitle search.",
            false,
        ),
        BackendError::Unavailable(_) | BackendError::Unreachable(_) | BackendError::TimedOut(_) => {
            problem(
                format!("{server} can’t reach the subtitle sites"),
                "Try again in a few minutes.",
                true,
            )
        }
        BackendError::RateLimited(wait) => Problem {
            title: "The subtitle sites asked to wait".to_owned(),
            detail: format!("Try again in {} s.", wait.as_secs().max(1)),
            retry: true,
        },
        BackendError::QuotaExhausted {
            whose,
            site,
            resets_at,
        } => quota_problem(*whose, site.as_deref(), *resets_at, server),
        other => problem(
            "Couldn’t search for subtitles".to_owned(),
            &crate::app::sentence(&other.to_string()),
            true,
        ),
    }
}

/// Today's downloads used up (board state 5): the server's account at a
/// site, or the user's own daily limit.
pub fn quota_problem(
    whose: QuotaOf,
    site: Option<&str>,
    resets_at: Option<i64>,
    server: &str,
) -> Problem {
    let after = match (site, resets_at) {
        (Some(site), Some(at)) => format!(
            "{} allows more after {}.",
            SubtitleSource::name_of(site),
            utc_time(at)
        ),
        (Some(site), None) => format!("{} allows more tomorrow.", SubtitleSource::name_of(site)),
        (None, _) => "More are allowed tomorrow.".to_owned(),
    };
    let (title, detail) = match whose {
        QuotaOf::User => (
            "You’ve used your subtitle downloads for today".to_owned(),
            format!("{server}’s admin set a daily limit. Results still show; downloading waits."),
        ),
        QuotaOf::Account | QuotaOf::Unknown => (
            format!("{server} has used today’s downloads"),
            format!("{after} Results still show; downloading waits."),
        ),
    };
    Problem {
        title,
        detail,
        retry: false,
    }
}

/// What a result's row says when its download failed (board state 6).
pub fn download_problem(error: &BackendError) -> String {
    match error {
        BackendError::QuotaExhausted { .. } => {
            "No downloads are left today. Choose it again tomorrow.".to_owned()
        }
        BackendError::RateLimited(wait) => format!(
            "The site asked to wait. Choose it again in {} s.",
            wait.as_secs().max(1)
        ),
        _ => "Couldn’t download this one. Choose it again or try another.".to_owned(),
    }
}

/// "No Swedish subtitles for Alien" (board state 2).
pub fn no_results(language: &str, title: &str) -> String {
    format!("No {} subtitles for {title}", format::language(language))
}

/// The languages the picker offers: those the server searched, then the
/// common ones, each once whatever its case.
pub fn language_choices(searched: &[String]) -> Vec<String> {
    let mut choices: Vec<String> = Vec::new();
    for code in searched.iter().map(String::as_str).chain(COMMON) {
        if !choices.iter().any(|c| c.eq_ignore_ascii_case(code)) {
            choices.push(code.to_owned());
        }
    }
    choices
}

/// What the item page says of a downloaded track (PX-02): its site, who
/// downloaded it and when, and whether it was made for this file.
pub fn download_note(download: &SubtitleDownload, now: i64) -> String {
    let mut parts = vec![
        format!("From {}", SubtitleSource::name_of(&download.source)),
        format!(
            "downloaded by {} {}",
            download.by,
            crate::reachability::ago(download.at.max(0) as u64, now.max(0) as u64)
        ),
    ];
    match (download.hash_match, download.origin) {
        (true, _) => parts.push("made for this file".into()),
        (false, SubtitleOrigin::MachineTranslated) => parts.push("machine-translated".into()),
        (false, SubtitleOrigin::Ai) => parts.push("made by AI".into()),
        (false, SubtitleOrigin::Human) => {}
    }
    parts.join(" · ")
}

/// Now, Unix seconds.
pub fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

/// What the panel needs: whose item, and what to do with the track a
/// download made (its place in the item's subtitles).
pub struct Search {
    pub session: Arc<dyn Session>,
    pub item: MediaId,
    /// The item's name, as the panel's subtitle says it.
    pub title: String,
    /// The server's name, as the states say it.
    pub server: String,
    pub picked: Rc<dyn Fn(u32)>,
}

struct View {
    search: Search,
    dialog: glib::WeakRef<adw::Dialog>,
    languages: gtk::DropDown,
    /// The codes the language picker shows, in its order.
    codes: RefCell<Vec<String>>,
    /// Set while the picker is filled, so it searches nothing.
    syncing: Cell<bool>,
    sources: gtk::Label,
    status: gtk::Box,
    count: gtk::Label,
    list: gtk::ListBox,
    /// Bumped by each search, so a slow one does not replace a newer.
    asked: Cell<u64>,
    /// A download is under way: rows do nothing meanwhile.
    downloading: Cell<bool>,
}

/// Opens the panel over `parent` and searches at once, in the languages
/// the server offers first.
pub fn present(parent: &impl IsA<gtk::Widget>, search: Search) {
    let dialog = adw::Dialog::builder()
        .title("Search for subtitles")
        .content_width(560)
        .content_height(640)
        .build();
    let header = adw::HeaderBar::builder()
        .title_widget(&adw::WindowTitle::new(
            "Search for subtitles",
            &search.title,
        ))
        .build();
    let body = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(12)
        .margin_top(12)
        .margin_bottom(18)
        .margin_start(18)
        .margin_end(18)
        .build();
    let top = gtk::Box::builder().spacing(12).build();
    let picker = gtk::Box::new(gtk::Orientation::Vertical, 6);
    picker.append(&label("LANGUAGE", &["picker-label"]));
    let languages = gtk::DropDown::builder()
        .model(&gtk::StringList::new(&[]))
        .css_classes(["picker"])
        .width_request(220)
        .build();
    picker.append(&languages);
    top.append(&picker);
    let sources = label("", &["dim-label", "caption"]);
    sources.set_hexpand(true);
    sources.set_xalign(1.0);
    sources.set_valign(gtk::Align::End);
    top.append(&sources);
    body.append(&top);
    let status = gtk::Box::new(gtk::Orientation::Vertical, 8);
    body.append(&status);
    let count = label("", &["dim-label", "caption"]);
    body.append(&count);
    let list = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .css_classes(["boxed-list", "subtitle-results"])
        .valign(gtk::Align::Start)
        .build();
    let scroller = gtk::ScrolledWindow::builder()
        .child(&list)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vexpand(true)
        .build();
    body.append(&scroller);
    body.append(&crate::dialog::dim(&format!(
        "The server searches by this file and its title. A subtitle you download is a track \
         of this item for everyone on {}; one not made for this file is synced to it.",
        search.server
    )));
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header);
    toolbar.set_content(Some(&body));
    dialog.set_child(Some(&toolbar));

    let view = Rc::new(View {
        search,
        dialog: dialog.downgrade(),
        languages,
        codes: RefCell::default(),
        syncing: Cell::new(false),
        sources,
        status,
        count,
        list,
        asked: Cell::new(0),
        downloading: Cell::new(false),
    });
    {
        let weak = Rc::downgrade(&view);
        view.languages.connect_selected_notify(move |picker| {
            let Some(view) = weak.upgrade() else { return };
            if view.syncing.get() {
                return;
            }
            let code = view.codes.borrow().get(picker.selected() as usize).cloned();
            if let Some(code) = code {
                view.run(Some(code));
            }
        });
    }
    {
        let weak = Rc::downgrade(&view);
        view.list.connect_row_activated(move |_, row| {
            if let Some(view) = weak.upgrade() {
                view.download(row);
            }
        });
    }
    view.run(None);
    crate::dialog::keep_until_closed(&dialog, view);
    dialog.present(Some(parent));
}

impl View {
    /// Searches in `language`, or the server's first languages.
    fn run(self: &Rc<Self>, language: Option<String>) {
        let asked = self.asked.get() + 1;
        self.asked.set(asked);
        self.list.remove_all();
        self.count.set_label("");
        let searching = match &language {
            Some(code) => format!(
                "Searching for {} in {}…",
                self.search.title,
                format::language(code)
            ),
            None => format!("Searching for {}…", self.search.title),
        };
        self.show_status(&[spinning(&searching)]);
        let (session, id) = (self.search.session.clone(), self.search.item.clone());
        let asked_for = language.clone();
        let found =
            relm4::spawn(async move { session.search_subtitles(&id, asked_for.as_deref()).await });
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let answer = found
                .await
                .unwrap_or_else(|e| Err(BackendError::Other(format!("the search stopped: {e}"))));
            if let Some(view) = weak.upgrade()
                && view.asked.get() == asked
            {
                view.found(language, answer);
            }
        });
    }

    fn found(self: &Rc<Self>, asked: Option<String>, answer: Result<SubtitleSearch, BackendError>) {
        let search = match answer {
            Ok(search) => search,
            Err(e) => {
                let problem = search_problem(&e, &self.search.server);
                let retry = problem.retry.then(|| {
                    let (weak, asked) = (Rc::downgrade(self), asked.clone());
                    let again: Rc<dyn Fn()> = Rc::new(move || {
                        if let Some(view) = weak.upgrade() {
                            view.run(asked.clone());
                        }
                    });
                    again
                });
                let note = match e {
                    BackendError::NoSubtitleSource => "note",
                    _ => "warning",
                };
                return self.show_status(&[problem_note(&problem, note, retry)]);
            }
        };
        self.fill_languages(&search.languages);
        self.sources
            .set_label(&sources_line(&search.sources).unwrap_or_default());
        let mut notes: Vec<gtk::Widget> = search
            .sources
            .iter()
            .filter_map(source_problem)
            .map(|text| label(&text, &["warning", "caption"]).upcast())
            .collect();
        if search.results.is_empty() {
            let language = search
                .languages
                .first()
                .cloned()
                .or(asked.clone())
                .unwrap_or_default();
            let note = gtk::Box::new(gtk::Orientation::Vertical, 6);
            note.append(&label(
                &no_results(&language, &self.search.title),
                &["heading"],
            ));
            note.append(&label(
                "Try another language, or search again later: people add subtitles every day.",
                &["dim-label"],
            ));
            if !language.eq_ignore_ascii_case("en") {
                let english = gtk::Button::builder()
                    .label("Search in English")
                    .halign(gtk::Align::Start)
                    .css_classes(["action", "compact", "suggested-action"])
                    .build();
                let weak = Rc::downgrade(self);
                english.connect_clicked(move |_| {
                    if let Some(view) = weak.upgrade() {
                        view.run(Some("en".into()));
                    }
                });
                note.append(&english);
            }
            notes.push(note.upcast());
            return self.show_status(&notes);
        }
        self.show_status(&notes);
        self.count.set_label(&count_line(&search.results));
        for result in ordered(&search.results) {
            self.list.append(&result_row(result));
        }
    }

    /// The picker shows the language searched, among the others offered.
    fn fill_languages(&self, searched: &[String]) {
        let codes = language_choices(searched);
        let names: Vec<String> = codes.iter().map(|c| format::language(c)).collect();
        let names: Vec<&str> = names.iter().map(String::as_str).collect();
        self.syncing.set(true);
        self.languages
            .set_model(Some(&gtk::StringList::new(&names)));
        self.languages.set_selected(0);
        self.syncing.set(false);
        self.codes.replace(codes);
    }

    fn show_status(&self, widgets: &[gtk::Widget]) {
        while let Some(child) = self.status.first_child() {
            self.status.remove(&child);
        }
        for widget in widgets {
            self.status.append(widget);
        }
    }

    /// The row's result downloads; the others wait (board: one result
    /// downloading). Done, the panel closes and the track is handed back.
    fn download(self: &Rc<Self>, row: &gtk::ListBoxRow) {
        if self.downloading.get() {
            return;
        }
        let Some(result) = row_result(row) else {
            return;
        };
        self.downloading.set(true);
        self.list.set_sensitive(false);
        let progress = row_progress(row);
        progress.set_visible(true);
        if let Some(error) = row_error(row) {
            error.set_visible(false);
        }
        let (session, id) = (self.search.session.clone(), self.search.item.clone());
        let got = relm4::spawn(async move { session.download_subtitle(&id, &result).await });
        let (weak, row) = (Rc::downgrade(self), row.downgrade());
        glib::spawn_future_local(async move {
            let answer = got
                .await
                .unwrap_or_else(|e| Err(BackendError::Other(format!("the download stopped: {e}"))));
            let Some(view) = weak.upgrade() else { return };
            view.downloading.set(false);
            view.list.set_sensitive(true);
            match answer {
                Ok(got) => {
                    if let Some(dialog) = view.dialog.upgrade() {
                        dialog.close();
                    }
                    (view.search.picked)(got.track);
                }
                Err(e) => {
                    tracing::warn!(error = %e, "the subtitle did not download");
                    let Some(row) = row.upgrade() else { return };
                    row_progress(&row).set_visible(false);
                    if let Some(error) = row_error(&row) {
                        error.set_label(&download_problem(&e));
                        error.set_visible(true);
                    }
                    if let BackendError::QuotaExhausted {
                        whose,
                        site,
                        resets_at,
                    } = &e
                    {
                        let problem =
                            quota_problem(*whose, site.as_deref(), *resets_at, &view.search.server);
                        view.show_status(&[problem_note(&problem, "warning", None)]);
                    }
                }
            }
        });
    }
}

/// The result's id, kept on its row's widget name.
fn row_result(row: &gtk::ListBoxRow) -> Option<String> {
    let name = row.widget_name();
    (!name.is_empty()).then(|| name.to_string())
}

fn find_child(row: &gtk::ListBoxRow, class: &str) -> Option<gtk::Widget> {
    fn walk(widget: gtk::Widget, class: &str) -> Option<gtk::Widget> {
        if widget.has_css_class(class) {
            return Some(widget);
        }
        let mut child = widget.first_child();
        while let Some(c) = child {
            if let Some(found) = walk(c.clone(), class) {
                return Some(found);
            }
            child = c.next_sibling();
        }
        None
    }
    walk(row.clone().upcast(), class)
}

fn row_progress(row: &gtk::ListBoxRow) -> gtk::Widget {
    find_child(row, "subtitle-downloading").expect("every row has one")
}

fn row_error(row: &gtk::ListBoxRow) -> Option<gtk::Label> {
    find_child(row, "subtitle-row-error").and_then(|w| w.downcast().ok())
}

/// A result's row: "Matches this file" for a hash match, its release,
/// its line, a note, and the states a download goes through.
fn result_row(result: &SubtitleResult) -> gtk::ListBoxRow {
    let column = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(5)
        .margin_top(10)
        .margin_bottom(10)
        .margin_start(12)
        .margin_end(12)
        .build();
    let top = gtk::Box::builder().spacing(8).build();
    if result.hash_match {
        top.append(&label("Matches this file", &["subtitle-match"]));
    }
    let release = label(&result.release, &["monospace", "subtitle-release"]);
    release.set_ellipsize(gtk::pango::EllipsizeMode::End);
    release.set_tooltip_text(Some(&result.release));
    top.append(&release);
    column.append(&top);
    column.append(&label(&result_line(result), &["dim-label", "caption"]));
    if let Some(note) = result_note(result) {
        column.append(&label(note, &["dim-label", "caption"]));
    }
    let progress = gtk::Box::builder()
        .spacing(8)
        .visible(false)
        .css_classes(["subtitle-downloading"])
        .build();
    progress.append(&gtk::Spinner::builder().spinning(true).build());
    progress.append(&label("Downloading…", &["accent", "caption"]));
    column.append(&progress);
    let error = label("", &["error", "caption", "subtitle-row-error"]);
    error.set_visible(false);
    column.append(&error);
    let row = gtk::ListBoxRow::builder()
        .child(&column)
        .activatable(true)
        .name(&result.id)
        .build();
    if result.rejected {
        row.add_css_class("dim-label");
    }
    row
}

fn spinning(text: &str) -> gtk::Widget {
    let row = gtk::Box::builder().spacing(8).build();
    row.append(&gtk::Spinner::builder().spinning(true).build());
    row.append(&label(text, &["dim-label"]));
    row.upcast()
}

/// A note with a title and what to do; `kind` is "note" or "warning".
fn problem_note(problem: &Problem, kind: &str, retry: Option<Rc<dyn Fn()>>) -> gtk::Widget {
    let note = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(4)
        .css_classes(["card", "subtitle-note", kind])
        .build();
    let inner = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(4)
        .margin_top(12)
        .margin_bottom(12)
        .margin_start(14)
        .margin_end(14)
        .build();
    inner.append(&label(&problem.title, &["heading"]));
    inner.append(&label(&problem.detail, &[]));
    if let Some(retry) = retry {
        let again = gtk::Button::builder()
            .label("Try again")
            .halign(gtk::Align::Start)
            .margin_top(6)
            .css_classes(["action", "compact", "outlined"])
            .build();
        again.connect_clicked(move |_| retry());
        inner.append(&again);
    }
    note.append(&inner);
    note.upcast()
}

fn label(text: &str, classes: &[&str]) -> gtk::Label {
    gtk::Label::builder()
        .label(text)
        .xalign(0.0)
        .wrap(true)
        .css_classes(classes)
        .build()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn result() -> SubtitleResult {
        SubtitleResult {
            id: "opensubtitles:1".into(),
            source: "opensubtitles".into(),
            release: "Alien.1979.1080p".into(),
            language: "en".into(),
            format: Some("srt".into()),
            fps_milli: None,
            downloads: 31_402,
            rating: Some(90),
            hearing_impaired: false,
            origin: SubtitleOrigin::Human,
            hash_match: false,
            rejected: false,
            downloaded: false,
        }
    }

    #[test]
    fn a_count_is_grouped_by_thousands() {
        assert_eq!(
            [7, 4_112, 1_234_567].map(thousands),
            ["7", "4,112", "1,234,567"]
        );
    }

    #[test]
    fn a_result_says_its_language_site_rating_and_downloads() {
        assert_eq!(
            result_line(&result()),
            "English · OpenSubtitles · Rated 9.0 · 31,402 downloads"
        );
    }

    #[test]
    fn a_result_says_it_is_for_the_hard_of_hearing_and_machine_made() {
        let line = result_line(&SubtitleResult {
            source: "subdl".into(),
            hearing_impaired: true,
            origin: SubtitleOrigin::MachineTranslated,
            rating: None,
            downloads: 0,
            ..result()
        });
        assert_eq!(line, "English · SubDL · HI · Machine-translated");
    }

    #[test]
    fn a_rejected_result_is_listed_last_and_says_why() {
        let bad = SubtitleResult {
            id: "bad".into(),
            rejected: true,
            ..result()
        };
        let results = [bad.clone(), result()];
        assert_eq!(
            (
                ordered(&results).last().map(|r| r.id.as_str()),
                result_note(&bad)
            ),
            (Some("bad"), Some("Removed before as a bad match"))
        );
    }

    #[test]
    fn the_count_says_matches_come_first_only_when_there_is_one() {
        let matched = SubtitleResult {
            hash_match: true,
            ..result()
        };
        assert_eq!(
            [count_line(&[result()]), count_line(&[matched, result()])],
            [
                "1 result",
                "2 results · files that match this one come first"
            ]
        );
    }

    #[test]
    fn the_sites_asked_are_named() {
        let found = |source: &str| SubtitleSourceStatus {
            source: source.into(),
            state: SubtitleSourceState::Found { found: 1 },
        };
        assert_eq!(
            [
                sources_line(&[found("opensubtitles")]),
                sources_line(&[found("opensubtitles"), found("subdl")]),
                sources_line(&[])
            ],
            [
                Some("From OpenSubtitles".to_owned()),
                Some("From OpenSubtitles and SubDL".to_owned()),
                None
            ]
        );
    }

    #[test]
    fn a_site_that_failed_says_so_and_one_that_answered_says_nothing() {
        let status = |state| SubtitleSourceStatus {
            source: "subdl".into(),
            state,
        };
        assert_eq!(
            [
                source_problem(&status(SubtitleSourceState::Found { found: 3 })),
                source_problem(&status(SubtitleSourceState::Failed {
                    message: "no answer".into()
                })),
                source_problem(&status(SubtitleSourceState::QuotaExhausted {
                    resets_at: Some(14 * 3600)
                })),
                source_problem(&status(SubtitleSourceState::RateLimited {
                    retry_after_ms: 1500
                })),
            ],
            [
                None,
                Some("SubDL can’t be reached: no answer".to_owned()),
                Some("SubDL has no downloads left today; more after 14:00 UTC.".to_owned()),
                Some("SubDL asked to wait 2 s; its results may be missing.".to_owned()),
            ]
        );
    }

    #[test]
    fn a_server_without_a_site_says_search_is_off_with_no_retry() {
        assert_eq!(
            search_problem(&BackendError::NoSubtitleSource, "Home Media"),
            Problem {
                title: "Subtitle search is off on Home Media".into(),
                detail: "Ask the server’s owner to turn on subtitle search.".into(),
                retry: false,
            }
        );
    }

    #[test]
    fn sites_out_of_reach_offer_try_again() {
        let problem = search_problem(&BackendError::Unavailable("down".into()), "Home Media");
        assert_eq!(
            (problem.title.as_str(), problem.retry),
            ("Home Media can’t reach the subtitle sites", true)
        );
    }

    #[test]
    fn the_servers_used_up_downloads_say_when_more_come() {
        let problem = quota_problem(
            QuotaOf::Account,
            Some("opensubtitles"),
            Some(0),
            "Home Media",
        );
        assert_eq!(
            (problem.title, problem.detail),
            (
                "Home Media has used today’s downloads".to_owned(),
                "OpenSubtitles allows more after 00:00 UTC. Results still show; downloading waits."
                    .to_owned()
            )
        );
    }

    #[test]
    fn the_users_own_used_up_downloads_say_whose_limit_it_is() {
        assert_eq!(
            quota_problem(QuotaOf::User, None, None, "Home Media").title,
            "You’ve used your subtitle downloads for today"
        );
    }

    #[test]
    fn a_failed_download_says_what_to_do() {
        assert_eq!(
            [
                download_problem(&BackendError::Other("x".into())),
                download_problem(&BackendError::RateLimited(Duration::from_secs(3))),
            ],
            [
                "Couldn’t download this one. Choose it again or try another.",
                "The site asked to wait. Choose it again in 3 s.",
            ]
        );
    }

    #[test]
    fn nothing_found_names_the_language() {
        assert_eq!(no_results("sv", "Alien"), "No Swedish subtitles for Alien");
    }

    #[test]
    fn the_languages_searched_come_first_and_each_once() {
        let choices = language_choices(&["pt-br".to_owned(), "sv".to_owned()]);
        assert_eq!(
            (
                &choices[..3],
                choices
                    .iter()
                    .filter(|c| c.eq_ignore_ascii_case("pt-BR"))
                    .count()
            ),
            (
                &["pt-br".to_owned(), "sv".to_owned(), "en".to_owned()][..],
                1
            )
        );
    }

    #[test]
    fn a_downloaded_track_says_where_from_who_and_when() {
        let download = SubtitleDownload {
            id: "d1".into(),
            source: "opensubtitles".into(),
            release: "Alien.1979.1080p".into(),
            by: "Alice".into(),
            at: 0,
            hash_match: false,
            origin: SubtitleOrigin::MachineTranslated,
            can_remove: true,
        };
        assert_eq!(
            download_note(&download, 3 * 86_400),
            "From OpenSubtitles · downloaded by Alice 3 days ago · machine-translated"
        );
    }
}
