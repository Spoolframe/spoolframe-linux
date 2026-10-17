//! An external text subtitle's timing (PX-03) in the player's subtitle
//! menu: how it is synced, its offset with − and + in 0.1 s steps and
//! Reset, who set it and when, Sync and Use original timing. The server
//! syncs and offsets the track for everyone and serves it so: the player
//! reads it again when its timing changes, and never adds the offset
//! itself. No board draws this; it follows the menu's own look.

use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

use gtk::glib;
use gtk::prelude::*;
use spoolframe_linux_core::backend::{MAX_SUBTITLE_OFFSET_MS, SubtitleSync, SubtitleTiming};

/// One press of − or +.
pub const STEP_MS: i64 = 100;

/// How long after the last press the offset is sent: a run of presses is
/// one change for everyone, not one each.
const SETTLE: Duration = Duration::from_millis(500);

/// What the menu says of a sync.
pub fn sync_line(sync: &SubtitleSync) -> Option<String> {
    match sync {
        SubtitleSync::Original => Some("Original timing".to_owned()),
        SubtitleSync::Syncing => Some("Syncing to this file…".to_owned()),
        SubtitleSync::Synced {
            offset_ms,
            factor_ppm,
            splits,
            ..
        } => {
            let mut line = format!("Synced to this file · {}", shift(*offset_ms));
            if *factor_ppm != 1_000_000 {
                line.push_str(" · frame rate matched");
            }
            match splits {
                0 => {}
                1 => line.push_str(" · 1 cut"),
                n => line.push_str(&format!(" · {n} cuts")),
            }
            Some(line)
        }
        SubtitleSync::Uncertain { score } => Some(format!(
            "Sync uncertain ({score} of 100): it may be another film’s subtitle, so it keeps \
             its original timing"
        )),
        SubtitleSync::Failed { reason } => {
            Some(format!("Couldn’t sync: {}", reason.trim_end_matches('.')))
        }
        SubtitleSync::Unknown => None,
    }
}

/// "moved 3.2 s earlier", "moved 0.4 s later", "not moved".
fn shift(ms: i64) -> String {
    let seconds = seconds(ms.abs());
    match ms {
        0 => "not moved".to_owned(),
        ..0 => format!("moved {seconds} s earlier"),
        _ => format!("moved {seconds} s later"),
    }
}

/// "3.2", from ms, to a tenth.
fn seconds(ms: i64) -> String {
    let tenths = (ms + 50) / 100;
    format!("{}.{}", tenths / 10, tenths % 10)
}

/// The offset as the control shows it: "+1.5 s", "−0.3 s", "0.0 s".
pub fn offset_label(ms: i64) -> String {
    match ms {
        0 => "0.0 s".to_owned(),
        ..0 => format!("−{} s", seconds(-ms)),
        _ => format!("+{} s", seconds(ms)),
    }
}

/// The offset after `steps` presses from `ms` (+ for later), within the
/// server's ten minutes either way.
pub fn stepped(ms: i64, steps: i64) -> i64 {
    (ms + steps * STEP_MS).clamp(-MAX_SUBTITLE_OFFSET_MS, MAX_SUBTITLE_OFFSET_MS)
}

/// Who set the offset and when, where someone has.
pub fn offset_by_line(timing: &SubtitleTiming, now: i64) -> Option<String> {
    let by = timing.offset_by.as_deref()?;
    Some(match timing.offset_at {
        Some(at) => format!(
            "Set by {by} {}",
            crate::reachability::ago(at.max(0) as u64, now.max(0) as u64)
        ),
        None => format!("Set by {by}"),
    })
}

/// The sync button's label: Sync, or Sync again after one; none while
/// one runs.
pub fn sync_button(sync: &SubtitleSync) -> Option<&'static str> {
    match sync {
        SubtitleSync::Syncing => None,
        SubtitleSync::Original | SubtitleSync::Unknown => Some("Sync to this file"),
        _ => Some("Sync again"),
    }
}

/// Whether "Use original timing" is offered: after a sync, or while one
/// runs.
pub fn offers_original(sync: &SubtitleSync) -> bool {
    !matches!(sync, SubtitleSync::Original | SubtitleSync::Unknown)
}

/// Whether the track's subtitle must be read again for `now`: what the
/// server serves changed. A sync starting serves the track as it was, so
/// it changes nothing until it ends.
pub fn rereads(before: Option<&SubtitleTiming>, now: &SubtitleTiming) -> bool {
    let Some(before) = before else {
        return true;
    };
    if before == now {
        return false;
    }
    before.offset_ms != now.offset_ms || !matches!(now.sync, SubtitleSync::Syncing)
}

/// What the section does, for the track it shows.
pub struct TimingMenu {
    /// The chosen external text track's place and timing now; `None`
    /// when what shows is no such track.
    pub current: Rc<dyn Fn() -> Option<(u32, SubtitleTiming)>>,
    pub sync: Rc<dyn Fn(u32)>,
    pub original: Rc<dyn Fn(u32)>,
    pub offset: Rc<dyn Fn(u32, i64)>,
}

/// The section, and what draws it again from [`TimingMenu::current`]:
/// call it each time the menu opens.
pub fn section(menu: TimingMenu) -> (gtk::Box, Rc<dyn Fn()>) {
    let column = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(6)
        .css_classes(["subtitle-timing"])
        .build();
    column.append(
        &gtk::Label::builder()
            .label("TIMING")
            .xalign(0.0)
            .margin_top(6)
            .css_classes(["caption-heading", "dim-label"])
            .build(),
    );
    let state = label(&["caption"]);
    column.append(&state);
    let controls = gtk::Box::builder().spacing(6).build();
    let earlier = button("−", "Show the subtitle 0.1 s earlier");
    let shown = gtk::Label::builder()
        .width_chars(8)
        .css_classes(["monospace"])
        .build();
    let later = button("+", "Show the subtitle 0.1 s later");
    let reset = gtk::Button::builder()
        .label("Reset")
        .css_classes(["flat"])
        .build();
    controls.append(&earlier);
    controls.append(&shown);
    controls.append(&later);
    controls.append(&reset);
    column.append(&controls);
    let by = label(&["dim-label", "caption"]);
    column.append(&by);
    let actions = gtk::Box::builder().spacing(6).build();
    let sync = gtk::Button::builder().css_classes(["flat"]).build();
    let original = gtk::Button::builder()
        .label("Use original timing")
        .css_classes(["flat"])
        .build();
    actions.append(&sync);
    actions.append(&original);
    column.append(&actions);

    // The offset shown, ahead of the server while presses settle; and a
    // count of presses, so only the last one's wait sends.
    let pending = Rc::new(Cell::new(0_i64));
    let presses = Rc::new(Cell::new(0_u64));
    let menu = Rc::new(menu);
    let draw: Rc<dyn Fn()> = {
        let (column, state, shown, by, sync, original, menu, pending) = (
            column.downgrade(),
            state.downgrade(),
            shown.downgrade(),
            by.downgrade(),
            sync.downgrade(),
            original.downgrade(),
            menu.clone(),
            pending.clone(),
        );
        Rc::new(move || {
            let (Some(column), Some(state), Some(shown), Some(by), Some(sync), Some(original)) = (
                column.upgrade(),
                state.upgrade(),
                shown.upgrade(),
                by.upgrade(),
                sync.upgrade(),
                original.upgrade(),
            ) else {
                return;
            };
            let Some((_, timing)) = (menu.current)() else {
                return column.set_visible(false);
            };
            column.set_visible(true);
            let line = sync_line(&timing.sync);
            state.set_visible(line.is_some());
            state.set_label(&line.unwrap_or_default());
            pending.set(timing.offset_ms);
            shown.set_label(&offset_label(timing.offset_ms));
            let set_by = offset_by_line(&timing, crate::subtitle_search::now());
            by.set_visible(set_by.is_some());
            by.set_label(&set_by.unwrap_or_default());
            match sync_button(&timing.sync) {
                Some(text) => {
                    sync.set_label(text);
                    sync.set_visible(true);
                }
                None => sync.set_visible(false),
            }
            original.set_visible(offers_original(&timing.sync));
        })
    };
    let press = {
        let (menu, pending, presses, shown) = (
            menu.clone(),
            pending.clone(),
            presses.clone(),
            shown.downgrade(),
        );
        move |to: Box<dyn Fn(i64) -> i64>| {
            let Some((track, _)) = (menu.current)() else {
                return;
            };
            let ms = to(pending.get());
            pending.set(ms);
            if let Some(shown) = shown.upgrade() {
                shown.set_label(&offset_label(ms));
            }
            let press = presses.get() + 1;
            presses.set(press);
            let (menu, presses) = (menu.clone(), presses.clone());
            glib::timeout_add_local_once(SETTLE, move || {
                if presses.get() == press {
                    (menu.offset)(track, ms);
                }
            });
        }
    };
    let press = Rc::new(press);
    {
        let press = press.clone();
        earlier.connect_clicked(move |_| press(Box::new(|ms| stepped(ms, -1))));
    }
    {
        let press = press.clone();
        later.connect_clicked(move |_| press(Box::new(|ms| stepped(ms, 1))));
    }
    reset.connect_clicked(move |_| press(Box::new(|_| 0)));
    {
        let (menu, state) = (menu.clone(), state.downgrade());
        sync.connect_clicked(move |button| {
            if let Some((track, _)) = (menu.current)() {
                (menu.sync)(track);
                button.set_visible(false);
                if let Some(state) = state.upgrade() {
                    state.set_label(&sync_line(&SubtitleSync::Syncing).unwrap_or_default());
                }
            }
        });
    }
    {
        let (menu, state) = (menu, state.downgrade());
        original.connect_clicked(move |button| {
            if let Some((track, _)) = (menu.current)() {
                (menu.original)(track);
                button.set_visible(false);
                if let Some(state) = state.upgrade() {
                    state.set_label(&sync_line(&SubtitleSync::Original).unwrap_or_default());
                }
            }
        });
    }
    draw();
    (column, draw)
}

fn label(classes: &[&str]) -> gtk::Label {
    gtk::Label::builder()
        .xalign(0.0)
        .wrap(true)
        .max_width_chars(36)
        .css_classes(classes)
        .build()
}

fn button(text: &str, tooltip: &str) -> gtk::Button {
    gtk::Button::builder()
        .label(text)
        .tooltip_text(tooltip)
        .css_classes(["flat", "circular"])
        .build()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn timing(sync: SubtitleSync, offset_ms: i64) -> SubtitleTiming {
        SubtitleTiming {
            sync,
            offset_ms,
            offset_by: None,
            offset_at: None,
        }
    }

    #[test]
    fn a_synced_subtitle_says_how_far_it_was_moved() {
        let synced = SubtitleSync::Synced {
            offset_ms: -3_200,
            factor_ppm: 1_042_709,
            splits: 2,
            score: 87,
        };
        assert_eq!(
            sync_line(&synced).as_deref(),
            Some("Synced to this file · moved 3.2 s earlier · frame rate matched · 2 cuts")
        );
    }

    #[test]
    fn an_uncertain_and_a_failed_sync_say_why() {
        assert_eq!(
            [
                sync_line(&SubtitleSync::Uncertain { score: 31 }),
                sync_line(&SubtitleSync::Failed {
                    reason: "no speech found.".into()
                }),
                sync_line(&SubtitleSync::Unknown),
            ],
            [
                Some(
                    "Sync uncertain (31 of 100): it may be another film’s subtitle, so it keeps \
                     its original timing"
                        .to_owned()
                ),
                Some("Couldn’t sync: no speech found".to_owned()),
                None,
            ]
        );
    }

    #[test]
    fn the_offset_shows_its_sign_and_a_tenth() {
        assert_eq!(
            [1_500, -300, 0, 49].map(offset_label),
            ["+1.5 s", "−0.3 s", "0.0 s", "+0.0 s"]
        );
    }

    #[test]
    fn a_press_moves_a_tenth_and_stops_at_ten_minutes() {
        assert_eq!(
            [stepped(0, 1), stepped(0, -3), stepped(599_950, 1)],
            [100, -300, 600_000]
        );
    }

    #[test]
    fn who_set_the_offset_is_said_with_when() {
        let set = SubtitleTiming {
            offset_by: Some("Alice".into()),
            offset_at: Some(0),
            ..timing(SubtitleSync::Original, 500)
        };
        assert_eq!(
            [
                offset_by_line(&set, 300),
                offset_by_line(&timing(SubtitleSync::Original, 0), 300)
            ],
            [Some("Set by Alice 5 minutes ago".to_owned()), None]
        );
    }

    #[test]
    fn the_buttons_follow_the_sync() {
        assert_eq!(
            [
                (
                    sync_button(&SubtitleSync::Original),
                    offers_original(&SubtitleSync::Original)
                ),
                (
                    sync_button(&SubtitleSync::Syncing),
                    offers_original(&SubtitleSync::Syncing)
                ),
                (
                    sync_button(&SubtitleSync::Failed { reason: "x".into() }),
                    offers_original(&SubtitleSync::Failed { reason: "x".into() })
                ),
            ],
            [
                (Some("Sync to this file"), false),
                (None, true),
                (Some("Sync again"), true),
            ]
        );
    }

    #[test]
    fn a_sync_starting_does_not_read_the_subtitle_again() {
        let before = timing(SubtitleSync::Original, 0);
        assert!(!rereads(Some(&before), &timing(SubtitleSync::Syncing, 0)));
    }

    #[test]
    fn a_sync_ending_or_an_offset_reads_it_again() {
        let syncing = timing(SubtitleSync::Syncing, 0);
        let synced = timing(
            SubtitleSync::Synced {
                offset_ms: 0,
                factor_ppm: 1_000_000,
                splits: 0,
                score: 90,
            },
            0,
        );
        assert_eq!(
            [
                rereads(Some(&syncing), &synced),
                rereads(Some(&syncing), &timing(SubtitleSync::Syncing, 400)),
                rereads(Some(&synced), &synced),
            ],
            [true, true, false]
        );
    }
}
