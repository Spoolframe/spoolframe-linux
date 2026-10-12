//! Why a server can't be reached (0d0c4ff4), as on the LinuxReachability
//! board: the line under Choose a server's banner, and the Details dialog,
//! from the SDK's snapshot of what this device knows. Nothing here dials;
//! Try again does, through the app.

use std::rc::Rc;

use adw::prelude::*;
use spoolframe_linux_core::backend::{DialStage, LookupOutcome, PathKind, Reachability, SavedAddr};

/// "3 days ago", "5 minutes ago", "just now", from unix seconds.
pub fn ago(at: u64, now: u64) -> String {
    let secs = now.saturating_sub(at);
    let (n, unit) = match secs {
        0..60 => return "just now".to_owned(),
        60..3600 => (secs / 60, "minute"),
        3600..86400 => (secs / 3600, "hour"),
        _ => (secs / 86400, "day"),
    };
    match n {
        1 => format!("1 {unit} ago"),
        n => format!("{n} {unit}s ago"),
    }
}

/// "Oct 14, 18:04", in local time.
fn when(at: u64) -> String {
    gtk::glib::DateTime::from_unix_local(at as i64)
        .ok()
        .and_then(|t| t.format("%b %-d, %H:%M").ok())
        .map(|s| s.to_string())
        .unwrap_or_default()
}

/// The banner's line: what was tried, and when it last worked. "Tried its
/// 2 addresses for 10 s; relays are off for Cabin. Last connected 3 days
/// ago."
pub fn tried_line(reach: &Reachability, now: u64) -> String {
    let name = reach.name.as_deref().unwrap_or("this server");
    let mut line = match &reach.last_dial {
        Some(dial) => {
            let what = match dial.dialled.len() {
                0 => "Had no address to try".to_owned(),
                1 => "Tried its address".to_owned(),
                n => format!("Tried its {n} addresses"),
            };
            format!("{what} for {} s", dial.took_ms.div_ceil(1000))
        }
        None => "Hasn't been tried yet".to_owned(),
    };
    if !reach.relays_effective {
        line.push_str(&format!("; relays are off for {name}"));
    } else if reach.relays.is_empty() {
        line.push_str("; it names no relay");
    }
    line.push('.');
    if let Some(at) = reach.last_connected_at {
        line.push_str(&format!(" Last connected {}.", ago(at, now)));
    }
    line
}

/// "Relays now": "Off — this server: Off (app setting is On)".
pub fn relays_now(reach: &Reachability, app_on: bool) -> String {
    let now = if reach.relays_effective { "On" } else { "Off" };
    let app = if app_on { "On" } else { "Off" };
    match reach.use_relays {
        None => format!("{now} — follows the app setting ({app})"),
        Some(own) => format!(
            "{now} — this server: {} (app setting is {app})",
            if own { "On" } else { "Off" }
        ),
    }
}

/// Where an address came from, for its pill.
fn address_source(addr: &str, reach: &Reachability) -> &'static str {
    if reach.advertised_addrs.iter().any(|a| a == addr) {
        "advertised"
    } else if reach.hinted_addrs.iter().any(|a| a == addr) {
        "hinted"
    } else if reach.heard_on_lan.iter().any(|a| a == addr) {
        "heard"
    } else {
        "saved"
    }
}

fn stage_name(stage: DialStage) -> &'static str {
    match stage {
        DialStage::NoAddress => "no address",
        DialStage::Connect => "connect",
        DialStage::Timeout => "timeout",
        DialStage::Handshake => "handshake",
        DialStage::Closed => "closed",
        DialStage::Hello => "hello",
        _ => "refused",
    }
}

fn path_kind(kind: PathKind) -> &'static str {
    match kind {
        PathKind::Lan => "lan",
        PathKind::Direct => "udp",
        _ => "relay",
    }
}

fn outcome_text(outcome: &LookupOutcome) -> (String, &'static str) {
    match outcome {
        LookupOutcome::Found { relays, addrs } => (
            format!(
                "found · relays {} · addresses {}",
                list_or_none(relays),
                list_or_none(addrs)
            ),
            "success",
        ),
        LookupOutcome::NoRecord => ("no record".to_owned(), "warning"),
        LookupOutcome::Error { message } => (format!("error: {message}"), "error"),
        #[allow(unreachable_patterns)]
        _ => ("unknown".to_owned(), "dim-label"),
    }
}

fn list_or_none(list: &[String]) -> String {
    if list.is_empty() {
        "none".to_owned()
    } else {
        list.join(", ")
    }
}

fn address_line(addr: &SavedAddr, now: u64) -> String {
    format!(
        "{} · last seen {}",
        if addr.reached {
            "reached"
        } else {
            "never reached"
        },
        ago(addr.last_seen, now)
    )
}

/// `at` as ISO 8601 in UTC: `2026-10-11T21:40:02Z`.
fn iso(at: u64) -> String {
    gtk::glib::DateTime::from_unix_utc(at as i64)
        .ok()
        .and_then(|t| t.format("%Y-%m-%dT%H:%M:%SZ").ok())
        .map(|s| s.to_string())
        .unwrap_or_else(|| at.to_string())
}

/// The whole snapshot as plain text, for Copy report, line for line as the
/// Apple apps write it (Reachability.swift, mac-app 388e228f), so a report
/// reads the same from either: a header naming when it failed, the build
/// and the device, then the SDK's facts. Nothing about the account.
pub fn report(reach: &Reachability, now: u64, device: &str) -> String {
    let failed_at = reach
        .last_dial
        .as_ref()
        .filter(|d| !d.connected)
        .map_or(now, |d| d.at);
    let error = reach
        .last_dial
        .as_ref()
        .and_then(|d| d.error.clone())
        .unwrap_or_else(|| "none".to_owned());
    let mut out = vec![
        format!(
            "Spoolframe reach diagnostics · {} · build {} · {device}",
            iso(failed_at),
            env!("CARGO_PKG_VERSION")
        ),
        "network: unknown".to_owned(),
        format!("error: {error}"),
    ];
    let id: String = reach.endpoint_id.chars().take(16).collect();
    out.push(format!(
        "server: {} · {id}… · saved: {}",
        reach.name.as_deref().unwrap_or("?"),
        if reach.saved { "yes" } else { "no" }
    ));
    out.push(format!(
        "relays now: {} (this server: {})",
        if reach.relays_effective { "on" } else { "off" },
        match reach.use_relays {
            Some(true) => "on",
            Some(false) => "off",
            None => "app setting",
        }
    ));
    out.push(format!(
        "last connected: {}",
        reach.last_connected_at.map_or("never".to_owned(), iso)
    ));
    if reach.addrs.is_empty() {
        out.push("addresses: none".to_owned());
    } else {
        out.push("addresses:".to_owned());
        for addr in &reach.addrs {
            out.push(format!(
                "  {} · {} · last seen {}",
                addr.addr,
                if addr.reached {
                    "reached"
                } else {
                    "never reached"
                },
                iso(addr.last_seen)
            ));
        }
    }
    out.push(format!(
        "advertised: {}{}",
        list_or_none(&reach.advertised_addrs),
        reach
            .advertised_at
            .map_or(String::new(), |at| format!(" (as of {})", iso(at)))
    ));
    out.push(format!(
        "heard on this network: {}",
        list_or_none(&reach.heard_on_lan)
    ));
    if !reach.hinted_addrs.is_empty() {
        out.push(format!("hinted: {}", list_or_none(&reach.hinted_addrs)));
    }
    out.push(format!(
        "relays: {}{}",
        list_or_none(&reach.relays),
        if reach.relays_edited { " (edited)" } else { "" }
    ));
    if !reach.invite_relays.is_empty() {
        out.push(format!(
            "invite relays: {}",
            list_or_none(&reach.invite_relays)
        ));
    }
    out.push(format!(
        "lookup: {} · used: {}",
        reach.lookup.as_deref().unwrap_or("none"),
        list_or_none(&reach.lookups)
    ));
    for result in &reach.lookup_results {
        let outcome = match &result.outcome {
            LookupOutcome::Found { relays, addrs } => format!(
                "found · relays {} · addrs {}",
                list_or_none(relays),
                list_or_none(addrs)
            ),
            LookupOutcome::NoRecord => "no record".to_owned(),
            LookupOutcome::Error { message } => format!("error: {message}"),
            #[allow(unreachable_patterns)]
            _ => "unknown".to_owned(),
        };
        out.push(format!(
            "  {} at {}: {outcome}",
            result.lookup,
            iso(result.at)
        ));
    }
    match &reach.last_dial {
        Some(dial) => {
            let mut line = format!("last dial: {} · {} ms · ", iso(dial.at), dial.took_ms);
            if dial.connected {
                line.push_str("connected");
            } else {
                line.push_str("failed");
                if let Some(stage) = dial.stage {
                    line.push_str(&format!(" at {}", stage_name(stage)));
                }
                if let Some(error) = &dial.error {
                    line.push_str(&format!(" · {error}"));
                }
            }
            out.push(line);
            out.push(format!("  dialled: {}", list_or_none(&dial.dialled)));
            for path in &dial.known {
                out.push(format!(
                    "  known {} {}{}",
                    path_kind(path.kind),
                    path.addr,
                    if path.active { " (active)" } else { "" }
                ));
            }
        }
        None => out.push("last dial: none recorded".to_owned()),
    }
    for path in &reach.live {
        out.push(format!(
            "live {} {} · {} ms{}",
            path_kind(path.kind),
            path.addr,
            path.rtt_ms,
            if path.selected { " · selected" } else { "" }
        ));
    }
    out.join("\n")
}

/// What the Details dialog does besides show: copy, open the server's
/// relays, dial again.
pub struct Actions {
    pub relays: Rc<dyn Fn()>,
    pub try_again: Rc<dyn Fn()>,
}

/// "Why <name> can't be reached".
pub fn dialog(reach: &Reachability, app_on: bool, actions: Actions) -> adw::Dialog {
    let now = now();
    let name = reach
        .name
        .clone()
        .unwrap_or_else(|| "this server".to_owned());
    let page = adw::PreferencesPage::new();

    let this = group("This server");
    let short: String = reach.endpoint_id.chars().take(8).collect();
    this.add(&value_row(
        "Server",
        &format!(
            "{name} · {short}… · {}",
            if reach.saved { "saved" } else { "not saved" }
        ),
    ));
    this.add(&value_row("Relays now", &relays_now(reach, app_on)));
    this.add(&value_row(
        "Last connected",
        &reach.last_connected_at.map_or("never".to_owned(), |at| {
            format!("{} ({})", when(at), ago(at, now))
        }),
    ));
    page.add(&this);

    let addresses = group("Addresses");
    if reach.addrs.is_empty() {
        addresses.add(&value_row("None", "This device has no address for it"));
    }
    for addr in &reach.addrs {
        let row = value_row(&addr.addr, &address_line(addr, now));
        row.add_css_class("monospace");
        row.add_prefix(
            &gtk::Image::builder()
                .icon_name(if addr.reached {
                    crate::icons::CHECK
                } else {
                    crate::icons::REMOVE
                })
                .css_classes([if addr.reached { "success" } else { "dim-label" }])
                .build(),
        );
        row.add_suffix(&pill(address_source(&addr.addr, reach), "dim-label"));
        addresses.add(&row);
    }
    page.add(&addresses);

    let said = group("Where it says it is");
    said.add(&value_row(
        &match reach.advertised_at {
            Some(at) => format!("Advertised (as of {})", when(at)),
            None => "Advertised".to_owned(),
        },
        &list_or_none(&reach.advertised_addrs),
    ));
    said.add(&value_row(
        "Heard on this network",
        &list_or_none(&reach.heard_on_lan),
    ));
    if !reach.hinted_addrs.is_empty() {
        said.add(&value_row(
            "Hinted (from the invite)",
            &list_or_none(&reach.hinted_addrs),
        ));
    }
    page.add(&said);

    let relays = group("Relays and lookups");
    let mut relay_text = list_or_none(&reach.relays);
    if reach.relays_edited {
        relay_text.push_str(" · your list");
    }
    if !reach.relays_effective {
        relay_text.push_str(" · not used, relays off");
    }
    relays.add(&value_row("Relays", &relay_text));
    relays.add(&value_row(
        "Invite relays",
        &list_or_none(&reach.invite_relays),
    ));
    relays.add(&value_row(
        "Lookups",
        &match &reach.lookup {
            Some(lookup) => format!("{} · in use: {lookup}", list_or_none(&reach.lookups)),
            None => list_or_none(&reach.lookups),
        },
    ));
    for result in &reach.lookup_results {
        let (text, class) = outcome_text(&result.outcome);
        let row = value_row(&format!("{} · {}", result.lookup, when(result.at)), &text);
        row.add_suffix(&pill(
            match &result.outcome {
                LookupOutcome::Found { .. } => "found",
                LookupOutcome::NoRecord => "no record",
                _ => "error",
            },
            class,
        ));
        relays.add(&row);
    }
    page.add(&relays);

    let dial = group("Last dial");
    match &reach.last_dial {
        Some(last) => {
            let row = value_row(
                "When",
                &format!("{} · took {} ms", when(last.at), last.took_ms),
            );
            row.add_suffix(&match (last.connected, last.stage) {
                (true, _) => pill("connected", "success"),
                (false, Some(stage)) => pill(&format!("failed at {}", stage_name(stage)), "error"),
                (false, None) => pill("failed", "error"),
            });
            dial.add(&row);
            if let Some(error) = &last.error {
                let row = value_row("Error", error);
                row.add_css_class("monospace");
                row.add_css_class("error");
                dial.add(&row);
            }
            dial.add(&value_row("Dialled", &list_or_none(&last.dialled)));
            let known: Vec<String> = last
                .known
                .iter()
                .map(|p| {
                    format!(
                        "{} {}{}",
                        path_kind(p.kind),
                        p.addr,
                        if p.active { " (active)" } else { "" }
                    )
                })
                .collect();
            dial.add(&value_row(
                "Paths known",
                &if known.is_empty() {
                    "none".to_owned()
                } else {
                    known.join("\n")
                },
            ));
        }
        None => dial.add(&value_row("None recorded", "It hasn't been dialled yet")),
    }
    page.add(&dial);

    let live = group("Live paths");
    if reach.live.is_empty() {
        live.add(&value_row(
            "None",
            &format!("Nothing is connected to {name}."),
        ));
    }
    for path in &reach.live {
        let row = value_row(&path.addr, &format!("{} ms", path.rtt_ms));
        row.add_css_class("monospace");
        let kind = path_kind(path.kind);
        row.add_prefix(&pill(
            kind,
            if kind == "relay" {
                "invite-identity"
            } else {
                "dim-label"
            },
        ));
        if path.selected {
            row.add_suffix(&pill("selected", "invite-account"));
        }
        live.add(&row);
    }
    page.add(&live);

    let copy = gtk::Button::builder().label("Copy report").build();
    let relays_button = gtk::Button::builder().label("Relays…").build();
    let try_again = gtk::Button::builder()
        .label("Try again")
        .css_classes(["suggested-action"])
        .build();
    let buttons = gtk::Box::builder()
        .spacing(8)
        .halign(gtk::Align::End)
        .margin_top(8)
        .margin_bottom(16)
        .margin_end(16)
        .build();
    buttons.append(&copy);
    buttons.append(&relays_button);
    buttons.append(&try_again);

    let close = gtk::Button::builder()
        .label("Close")
        .css_classes(["flat", "dialog-cancel"])
        .build();
    let header = adw::HeaderBar::builder()
        .show_end_title_buttons(false)
        .show_start_title_buttons(false)
        .css_classes(["dialog-header"])
        .build();
    header.pack_start(&close);
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header);
    toolbar.set_content(Some(&page));
    toolbar.add_bottom_bar(&buttons);
    let dialog = adw::Dialog::builder()
        .title(format!("Why {name} can’t be reached"))
        .content_width(680)
        .content_height(720)
        .child(&toolbar)
        .build();

    let weak = dialog.downgrade();
    close.connect_clicked(move |_| {
        if let Some(dialog) = weak.upgrade() {
            dialog.close();
        }
    });
    let device = gtk::glib::host_name();
    let text = report(reach, now, &device);
    copy.connect_clicked(move |button| {
        button.clipboard().set_text(&text);
        button.set_label("Copied");
    });
    let (weak, relays) = (dialog.downgrade(), actions.relays);
    relays_button.connect_clicked(move |_| {
        if let Some(dialog) = weak.upgrade() {
            dialog.close();
        }
        relays();
    });
    let (weak, again) = (dialog.downgrade(), actions.try_again);
    try_again.connect_clicked(move |_| {
        if let Some(dialog) = weak.upgrade() {
            dialog.close();
        }
        again();
    });
    dialog
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

fn group(title: &str) -> adw::PreferencesGroup {
    adw::PreferencesGroup::builder()
        .title(crate::markup::plain(title))
        .build()
}

/// A label and its value, the value selectable and never markup.
fn value_row(label: &str, value: &str) -> adw::ActionRow {
    let row = adw::ActionRow::builder()
        .use_markup(false)
        .subtitle_selectable(true)
        .css_classes(["property"])
        .build();
    row.set_title(label);
    row.set_subtitle(value);
    row
}

fn pill(text: &str, class: &str) -> gtk::Label {
    gtk::Label::builder()
        .label(text)
        .valign(gtk::Align::Center)
        .css_classes(["chip", "invite-chip", class])
        .build()
}

#[cfg(test)]
pub(crate) mod tests {
    use spoolframe_linux_core::backend::{DialReport, LookupResult};

    use super::*;

    pub(crate) const NOW: u64 = 1_800_000_000;

    /// Cabin, last dialled 30 s ago and not reached: what other tests
    /// build on.
    pub(crate) fn cabin() -> Reachability {
        Reachability {
            endpoint_id: "c4b1".repeat(16),
            name: Some("Cabin".into()),
            saved: true,
            addrs: vec![
                SavedAddr {
                    addr: "203.0.113.24:41641".into(),
                    reached: true,
                    last_seen: NOW - 3 * 86400,
                },
                SavedAddr {
                    addr: "192.168.7.2:7430".into(),
                    reached: false,
                    last_seen: NOW - 86400,
                },
            ],
            advertised_addrs: vec!["203.0.113.24:41641".into()],
            advertised_at: None,
            relays: vec!["https://relay.cabin.example".into()],
            relays_edited: false,
            lookup: None,
            use_relays: Some(false),
            relays_effective: false,
            heard_on_lan: Vec::new(),
            hinted_addrs: vec!["192.168.7.2:7430".into()],
            invite_relays: Vec::new(),
            lookups: vec!["mdns".into()],
            lookup_results: vec![LookupResult {
                lookup: "https://dns.iroh.link/pkarr".into(),
                at: NOW - 60,
                outcome: LookupOutcome::NoRecord,
            }],
            last_dial: Some(DialReport {
                at: NOW - 30,
                took_ms: 10_000,
                connected: false,
                stage: Some(DialStage::Timeout),
                error: Some("timed out".into()),
                dialled: vec!["203.0.113.24:41641".into(), "192.168.7.2:7430".into()],
                known: Vec::new(),
            }),
            last_connected_at: Some(NOW - 3 * 86400),
            live: Vec::new(),
        }
    }

    #[test]
    fn the_banner_says_what_was_tried_as_the_board_does() {
        assert_eq!(
            tried_line(&cabin(), NOW),
            "Tried its 2 addresses for 10 s; relays are off for Cabin. Last connected 3 days ago."
        );
    }

    #[test]
    fn relays_now_says_this_servers_choice_against_the_apps() {
        assert_eq!(
            relays_now(&cabin(), true),
            "Off — this server: Off (app setting is On)"
        );
    }

    #[test]
    fn an_address_from_the_invite_is_hinted() {
        assert_eq!(address_source("192.168.7.2:7430", &cabin()), "hinted");
    }

    #[test]
    fn the_report_reads_as_the_apple_apps_write_it() {
        let report = report(&cabin(), NOW, "nimble");
        let lines: Vec<&str> = report.lines().collect();
        assert_eq!(
            lines,
            [
                format!(
                    "Spoolframe reach diagnostics · 2027-01-15T07:59:30Z · build {} · nimble",
                    env!("CARGO_PKG_VERSION")
                )
                .as_str(),
                "network: unknown",
                "error: timed out",
                "server: Cabin · c4b1c4b1c4b1c4b1… · saved: yes",
                "relays now: off (this server: off)",
                "last connected: 2027-01-12T08:00:00Z",
                "addresses:",
                "  203.0.113.24:41641 · reached · last seen 2027-01-12T08:00:00Z",
                "  192.168.7.2:7430 · never reached · last seen 2027-01-14T08:00:00Z",
                "advertised: 203.0.113.24:41641",
                "heard on this network: none",
                "hinted: 192.168.7.2:7430",
                "relays: https://relay.cabin.example",
                "lookup: none · used: mdns",
                "  https://dns.iroh.link/pkarr at 2027-01-15T07:59:00Z: no record",
                "last dial: 2027-01-15T07:59:30Z · 10000 ms · failed at timeout · timed out",
                "  dialled: 203.0.113.24:41641, 192.168.7.2:7430",
            ]
        );
    }

    #[test]
    fn times_read_as_how_long_ago() {
        assert_eq!(
            (
                ago(NOW - 30, NOW),
                ago(NOW - 120, NOW),
                ago(NOW - 86400, NOW)
            ),
            (
                "just now".to_owned(),
                "2 minutes ago".to_owned(),
                "1 day ago".to_owned()
            )
        );
    }

    /// The dialog builds from a snapshot with every section filled.
    #[test]
    #[ignore = "needs a display; run on its own"]
    fn the_details_dialog_builds() {
        gtk::init().unwrap();
        adw::init().unwrap();
        let dialog = dialog(
            &cabin(),
            true,
            Actions {
                relays: Rc::new(|| {}),
                try_again: Rc::new(|| {}),
            },
        );
        assert_eq!(dialog.title().as_str(), "Why Cabin can’t be reached");
    }
}
