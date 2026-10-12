//! Preferences › Identity › Recover from a server (P7-08), as on the
//! LinuxIdentityRoots board (7–8): a computer that lost its identity, or
//! never had it, picks a saved server that knows it, then touches any one
//! of its keys. The key names the identity; the server sends its record;
//! this computer takes it and pairs itself with the same key.
//!
//! The board shows the identity found before the touch. The SDK finds it
//! only through the key (its credential's handle names it), so the touch
//! comes first and the Identity tab shows what was found.

use std::rc::Rc;
use std::sync::Arc;

use adw::prelude::*;
use gtk::glib;
use spoolframe_client_sdk::SdkError;
use spoolframe_linux_core::backend::{Client, Server};
use spoolframe_linux_core::identity::Identities;
use spoolframe_linux_core::security_key::PinPrompt;

use crate::key_prompt::{self, Action, Op};

/// A saved server's line in the list: how it is reached.
pub fn server_line(server: &Server) -> String {
    match server.addresses.first() {
        Some(address) => format!("Saved · {address}"),
        None if !server.relays.is_empty() => "Saved · through a relay".to_owned(),
        None => "Saved".to_owned(),
    }
}

/// What a server that knows no identity on the key says.
pub fn unknown_here(server: &str) -> String {
    format!(
        "{server} doesn’t know an identity this key holds. Try another server, or another of \
         your keys."
    )
}

/// The name a recovered identity gets on this computer: the person's
/// own, as the system has it.
pub(crate) fn local_name() -> String {
    let real = glib::real_name().to_string_lossy().trim().to_owned();
    if real.is_empty() || real == "Unknown" {
        glib::user_name().to_string_lossy().into_owned()
    } else {
        real
    }
}

/// The dialog, over `parent`; `done` once recovered.
pub fn open(
    parent: &impl IsA<gtk::Widget>,
    identities: Identities,
    client: Arc<dyn Client>,
    done: Rc<dyn Fn()>,
) {
    let dialog = adw::Dialog::builder()
        .title("Recover your identity")
        .content_width(460)
        .build();
    let column = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(12)
        .margin_top(12)
        .margin_bottom(24)
        .margin_start(24)
        .margin_end(24)
        .build();
    column.append(
        &gtk::Label::builder()
            .label(
                "Pick a server that knows your identity. It sends your identity record; then any \
                 one of your keys.",
            )
            .xalign(0.0)
            .wrap(true)
            .css_classes(["dim-label"])
            .build(),
    );
    let list = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .css_classes(["boxed-list"])
        .build();
    column.append(&list);
    let empty = gtk::Label::builder()
        .label("No servers saved on this computer. Add one in Preferences › Servers first.")
        .xalign(0.0)
        .wrap(true)
        .visible(false)
        .css_classes(["dim-label"])
        .build();
    column.append(&empty);
    let go = gtk::Button::builder()
        .label("Continue")
        .halign(gtk::Align::End)
        .sensitive(false)
        .css_classes(["suggested-action"])
        .build();
    column.append(&go);
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&adw::HeaderBar::new());
    toolbar.set_content(Some(&column));
    dialog.set_child(Some(&toolbar));
    dialog.set_default_widget(Some(&go));

    let chosen: Rc<std::cell::RefCell<Option<Server>>> = Rc::default();
    {
        let (list, empty, go, chosen) = (list.clone(), empty.clone(), go.clone(), chosen.clone());
        glib::spawn_future_local(async move {
            let servers: Vec<Server> = match client.servers().await {
                Ok(servers) => servers.into_iter().filter(|s| s.pinned).collect(),
                Err(e) => return tracing::warn!(error = %e, "cannot list servers"),
            };
            empty.set_visible(servers.is_empty());
            list.set_visible(!servers.is_empty());
            let mut group: Option<gtk::CheckButton> = None;
            for server in servers {
                let row = adw::ActionRow::builder()
                    .use_markup(false)
                    .activatable(true)
                    .build();
                row.set_title(&server.name);
                row.set_subtitle(&server_line(&server));
                let pick = gtk::CheckButton::new();
                pick.set_group(group.as_ref());
                group.get_or_insert_with(|| pick.clone());
                row.add_prefix(&pick);
                row.set_activatable_widget(Some(&pick));
                let (chosen, go) = (chosen.clone(), go.clone());
                pick.connect_toggled(move |pick| {
                    if pick.is_active() {
                        *chosen.borrow_mut() = Some(server.clone());
                        go.set_sensitive(true);
                    }
                });
                list.append(&row);
            }
        });
    }
    {
        let dialog_weak = dialog.downgrade();
        go.connect_clicked(move |_| {
            let (Some(dialog), Some(server)) = (dialog_weak.upgrade(), chosen.borrow().clone())
            else {
                return;
            };
            let identities = identities.clone();
            let name = local_name();
            let server_name = server.name.clone();
            let op: Op<()> = Rc::new(move |prompt: Arc<dyn PinPrompt>| {
                let key = identities.key(prompt);
                let (identities, server, name, server_name) = (
                    identities.clone(),
                    server.key.0.clone(),
                    name.clone(),
                    server_name.clone(),
                );
                Box::pin(async move {
                    identities
                        .run(move |ids| {
                            Box::pin(async move {
                                match ids.recover(&server, key.as_ref(), &name).await {
                                    Ok(_) => Ok(()),
                                    Err(SdkError::NotFound(_)) => {
                                        Err(SdkError::Unavailable(unknown_here(&server_name)))
                                    }
                                    Err(e) => Err(e),
                                }
                            })
                        })
                        .await
                })
            });
            let action = Action {
                touch: "Touch any one of your keys".into(),
                keys: Some("Then its PIN. This computer pairs itself.".into()),
                identity: None,
                other_key: None,
            };
            let (done, closing) = (done.clone(), dialog.downgrade());
            key_prompt::act(&dialog, action, op, move |()| {
                if let Some(dialog) = closing.upgrade() {
                    dialog.close();
                }
                done();
            });
        });
    }
    dialog.present(Some(parent));
}

#[cfg(test)]
mod tests {
    use super::*;
    use spoolframe_linux_core::backend::ServerKey;

    fn server(addresses: &[&str], relays: &[&str]) -> Server {
        Server {
            key: ServerKey("k".into()),
            name: "Home Media".into(),
            addresses: addresses.iter().map(|a| (*a).to_owned()).collect(),
            pinned: true,
            fingerprint: String::new(),
            verified: true,
            on_network: false,
            verified_at: None,
            last_connected_at: None,
            server_id: None,
            advertised: Vec::new(),
            relays: relays.iter().map(|r| (*r).to_owned()).collect(),
            advertised_at: None,
            use_relays: None,
            relays_edited: false,
        }
    }

    #[test]
    fn a_server_with_an_address_names_it() {
        assert_eq!(
            server_line(&server(&["192.168.1.20:7777"], &[])),
            "Saved · 192.168.1.20:7777"
        );
    }

    #[test]
    fn a_server_reached_only_by_relay_says_so() {
        assert_eq!(
            server_line(&server(&[], &["https://relay"])),
            "Saved · through a relay"
        );
    }

    #[test]
    fn a_server_that_knows_no_identity_on_the_key_says_which() {
        assert!(unknown_here("Home Media").starts_with("Home Media doesn’t know"));
    }
}
