//! Import your identity from a file (P7-08), as on the LinuxIdentityRoots
//! board (9): the record exported on another computer, checked by its
//! fingerprint, then any one of its keys makes this computer one of its
//! devices.

use std::rc::Rc;
use std::sync::Arc;

use adw::prelude::*;
use spoolframe_client_sdk::IdentityRecord;
use spoolframe_linux_core::identity::Identities;
use spoolframe_linux_core::security_key::PinPrompt;

use crate::key_prompt::{self, Action, Op};

/// "Identity R2VN-8KQT-M4XA-H7PD".
pub fn identity_line(record: &IdentityRecord) -> String {
    format!("Identity {}", record.user.fingerprint())
}

/// The dialog, over `parent`, for `record` read from file `name`; `done`
/// once imported.
pub fn open(
    parent: &impl IsA<gtk::Widget>,
    identities: Identities,
    name: String,
    record: IdentityRecord,
    done: Rc<dyn Fn()>,
) {
    let dialog = adw::Dialog::builder()
        .title("Import your identity")
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
    let card = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .css_classes(["boxed-list"])
        .build();
    let row = adw::ActionRow::builder().use_markup(false).build();
    row.set_title(&name);
    row.set_subtitle(&identity_line(&record));
    row.add_prefix(&gtk::Image::from_icon_name(crate::icons::IDENTITY));
    card.append(&row);
    column.append(&card);
    column.append(
        &gtk::Label::builder()
            .label("Touch any one of its keys to make this computer one of your devices.")
            .xalign(0.0)
            .wrap(true)
            .css_classes(["dim-label"])
            .build(),
    );
    let buttons = gtk::Box::builder()
        .spacing(8)
        .halign(gtk::Align::End)
        .build();
    let cancel = gtk::Button::builder().label("Cancel").build();
    let go = gtk::Button::builder()
        .label("Continue")
        .css_classes(["suggested-action"])
        .build();
    buttons.append(&cancel);
    buttons.append(&go);
    column.append(&buttons);
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&adw::HeaderBar::new());
    toolbar.set_content(Some(&column));
    dialog.set_child(Some(&toolbar));
    dialog.set_default_widget(Some(&go));
    {
        let dialog = dialog.downgrade();
        cancel.connect_clicked(move |_| {
            if let Some(dialog) = dialog.upgrade() {
                dialog.close();
            }
        });
    }
    let dialog_weak = dialog.downgrade();
    go.connect_clicked(move |_| {
        let Some(dialog) = dialog_weak.upgrade() else {
            return;
        };
        let (identities, record) = (identities.clone(), record.clone());
        let name = crate::recover_identity::local_name();
        let op: Op<()> = Rc::new(move |prompt: Arc<dyn PinPrompt>| {
            let key = identities.key(prompt);
            let (identities, record, name) = (identities.clone(), record.clone(), name.clone());
            Box::pin(async move {
                identities
                    .run(move |ids| {
                        Box::pin(async move {
                            ids.import(record, &name, key.as_ref()).await.map(|_| ())
                        })
                    })
                    .await
            })
        });
        let action = Action {
            touch: "Touch any one of its keys".into(),
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
    dialog.present(Some(parent));
}

#[cfg(test)]
mod tests {
    use spoolframe_linux_core::identity::{KeyLabel, in_memory};
    use spoolframe_linux_core::security_key::PinAsk;
    use spoolframe_linux_core::testing::ready;

    use super::*;

    struct NoPrompt;
    impl PinPrompt for NoPrompt {
        fn pin(&self, _ask: &PinAsk) -> Option<String> {
            None
        }
        fn touch(&self, _key: &str) {}
    }

    #[test]
    fn an_exported_record_shows_the_identitys_fingerprint() {
        let identities = in_memory();
        let key = identities.key(Arc::new(NoPrompt));
        let label = KeyLabel {
            name: "Main key".into(),
            maker: None,
            added_at: 0,
        };
        let id = ready(identities.create("Alice", &[(key, label)])).unwrap();
        let record = Identities::read_record(&ready(identities.export(id)).unwrap()).unwrap();
        assert_eq!(
            identity_line(&record),
            format!("Identity {}", id.fingerprint())
        );
    }
}
