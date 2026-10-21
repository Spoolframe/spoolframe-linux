//! Closing the window while downloads run (PX-38). The user decided it is
//! asked each time: Keep in background hides the window and holds the
//! application until the last download stops moving, and Quit quits, the
//! SDK going on from where each download got to at the next start. What
//! closing does is worked out here, apart from the window and the dialog.

use std::rc::Rc;

use adw::prelude::*;

/// What a close of the window does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OnClose {
    /// The window closes, and the app with it.
    Close,
    /// Downloads run: ask first, and stay open meanwhile.
    Ask,
}

/// Where closing stands: how many downloads run, whether the app is kept
/// in the background for them, and whether the user chose Quit.
#[derive(Debug, Default)]
pub struct Closing {
    running: usize,
    kept: bool,
    quitting: bool,
}

impl Closing {
    /// What closing the window does now.
    pub fn on_close(&self) -> OnClose {
        match self.quitting || self.running == 0 {
            true => OnClose::Close,
            false => OnClose::Ask,
        }
    }

    /// How many downloads run, for the question.
    pub fn running(&self) -> usize {
        self.running
    }

    /// Keep in background: true when the application is to be held now,
    /// false when it already is.
    pub fn keep(&mut self) -> bool {
        !std::mem::replace(&mut self.kept, true)
    }

    /// Quit: the next close goes through, downloads or not.
    pub fn quit(&mut self) {
        self.quitting = true;
    }

    /// `running` downloads run now. True when the last one stopped while
    /// the app was kept for them: the hold goes, and a hidden window
    /// closes as it would have.
    pub fn downloads(&mut self, running: usize) -> bool {
        self.running = running;
        running == 0 && std::mem::take(&mut self.kept)
    }
}

/// The question's heading.
pub const HEADING: &str = "Downloads are still running";

/// The question's body: how many, and what quitting does to them.
pub fn body(running: usize) -> String {
    match running {
        1 => "1 download is still under way. Keep Spoolframe in the background to finish \
              it, or quit: it goes on from where it got to the next time Spoolframe opens."
            .to_owned(),
        n => format!(
            "{n} downloads are still under way. Keep Spoolframe in the background to \
             finish them, or quit: they go on from where they got to the next time \
             Spoolframe opens."
        ),
    }
}

/// What the user answered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Answer {
    Keep,
    Quit,
}

/// Asks over `window` what to do with `running` downloads; `answer` hears
/// the choice. Escape, or closing the question, keeps the window open
/// and does nothing.
pub fn ask(window: &impl IsA<gtk::Widget>, running: usize, answer: Rc<dyn Fn(Answer)>) {
    let alert = adw::AlertDialog::builder()
        .heading(HEADING)
        .body(body(running))
        .default_response("keep")
        .close_response("stay")
        .build();
    alert.add_response("quit", "Quit");
    alert.add_response("keep", "Keep in background");
    alert.set_response_appearance("quit", adw::ResponseAppearance::Destructive);
    alert.set_response_appearance("keep", adw::ResponseAppearance::Suggested);
    alert.connect_response(None, move |_, response| match response {
        "keep" => answer(Answer::Keep),
        "quit" => answer(Answer::Quit),
        _ => {}
    });
    alert.present(Some(window));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with_running(running: usize) -> Closing {
        let mut closing = Closing::default();
        closing.downloads(running);
        closing
    }

    #[test]
    fn closing_with_no_download_running_closes() {
        assert_eq!(with_running(0).on_close(), OnClose::Close);
    }

    #[test]
    fn closing_with_downloads_running_asks() {
        assert_eq!(with_running(2).on_close(), OnClose::Ask);
    }

    #[test]
    fn quit_closes_with_downloads_running() {
        let mut closing = with_running(2);
        closing.quit();
        assert_eq!(closing.on_close(), OnClose::Close);
    }

    #[test]
    fn keep_holds_the_app_once() {
        let mut closing = with_running(1);
        assert_eq!((closing.keep(), closing.keep()), (true, false));
    }

    #[test]
    fn the_last_download_finishing_releases_the_hold() {
        let mut closing = with_running(2);
        closing.keep();
        assert_eq!(
            (
                closing.downloads(1),
                closing.downloads(0),
                closing.on_close()
            ),
            (false, true, OnClose::Close)
        );
    }

    #[test]
    fn a_download_finishing_with_no_hold_releases_nothing() {
        let mut closing = with_running(1);
        assert!(!closing.downloads(0));
    }

    #[test]
    fn kept_again_after_a_release_holds_again() {
        let mut closing = with_running(1);
        closing.keep();
        closing.downloads(0);
        closing.downloads(1);
        assert!(closing.keep());
    }

    #[test]
    fn the_question_names_how_many() {
        assert_eq!(
            (
                body(1).starts_with("1 download is"),
                body(3).starts_with("3 downloads are")
            ),
            (true, true)
        );
    }
}
