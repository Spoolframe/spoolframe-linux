//! A library as a grid of posters that loads as it scrolls.
//!
//! A library can hold thousands of items, and the server serves them in
//! pages of at most 500. The grid is a GtkGridView over a list as long as
//! the library: it builds widgets only for the rows on screen and recycles
//! them, and a page is fetched the first time one of its items comes into
//! view. Until then its items are placeholder tiles.

use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::Rc;
use std::sync::Arc;

use adw::prelude::*;
use gtk::{gio, glib};
use spoolframe_linux_core::backend::{BackendError, Session};
use spoolframe_types::{Item, Library};

use crate::views::{self, Cards};

/// The server's largest page.
const PAGE: u32 = 500;

/// One cell of the grid: an item once its page has loaded.
type Cell = Option<Item>;

/// Shared by the grid's factory and its page fetches.
struct Pages {
    session: Arc<dyn Session>,
    library: Library,
    store: gio::ListStore,
    /// Pages fetched or being fetched.
    asked: RefCell<HashSet<u32>>,
}

impl Pages {
    /// Fetches the page holding `position`, unless it was asked for.
    fn want(self: &Rc<Self>, position: u32) {
        let page = position / PAGE;
        if !self.asked.borrow_mut().insert(page) {
            return;
        }
        let session = self.session.clone();
        let id = self.library.id.clone();
        let fetch = relm4::spawn(async move { session.items(&id, page * PAGE, PAGE).await });
        let pages = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let result = fetch.await;
            let Some(pages) = pages.upgrade() else { return };
            match result {
                Ok(Ok((items, _total))) => pages.fill(page * PAGE, items),
                Ok(Err(e)) => {
                    tracing::warn!(error = %e, page, "library page not loaded");
                    // Asked again when its items come into view again.
                    pages.asked.borrow_mut().remove(&page);
                }
                Err(e) => tracing::warn!(error = %e, page, "library page fetch failed"),
            }
        });
    }

    /// Puts a page's items in place of its placeholders. Items past the
    /// list (the library grew since it was opened) are left out.
    fn fill(&self, offset: u32, items: Vec<Item>) {
        let room = self.store.n_items().saturating_sub(offset) as usize;
        let cells: Vec<glib::BoxedAnyObject> = items
            .into_iter()
            .take(room)
            .map(|item| glib::BoxedAnyObject::new(Some(item) as Cell))
            .collect();
        self.store.splice(offset, cells.len() as u32, &cells);
    }
}

/// The grid for `library`, or what went wrong getting its first page.
/// Opening it costs one page; the rest load as they scroll into view.
pub async fn grid(
    session: Arc<dyn Session>,
    library: Library,
    cards: Rc<Cards>,
) -> Result<gtk::GridView, BackendError> {
    let first = {
        let (session, id) = (session.clone(), library.id.clone());
        relm4::spawn(async move { session.items(&id, 0, PAGE).await })
            .await
            .map_err(|e| BackendError::Other(format!("the call did not finish: {e}")))??
    };
    let (items, total) = first;
    let store = gio::ListStore::new::<glib::BoxedAnyObject>();
    let placeholders: Vec<glib::BoxedAnyObject> = (0..total)
        .map(|_| glib::BoxedAnyObject::new(None as Cell))
        .collect();
    store.splice(0, 0, &placeholders);
    let pages = Rc::new(Pages {
        session,
        library,
        store: store.clone(),
        asked: RefCell::new(HashSet::from([0])),
    });
    pages.fill(0, items);

    let factory = gtk::SignalListItemFactory::new();
    factory.connect_setup(|_, item| {
        let item = item.downcast_ref::<gtk::ListItem>().expect("a list item");
        item.set_child(Some(&adw::Bin::new()));
        // The card inside is a button; the list item itself is not.
        item.set_activatable(false);
    });
    {
        let pages = pages.clone();
        factory.connect_bind(move |_, item| {
            let item = item.downcast_ref::<gtk::ListItem>().expect("a list item");
            let Some(bin) = item.child().and_downcast::<adw::Bin>() else {
                return;
            };
            let Some(cell) = item.item().and_downcast::<glib::BoxedAnyObject>() else {
                return;
            };
            let card = match &*cell.borrow::<Cell>() {
                Some(media) => views::poster_card(media, &cards),
                None => {
                    pages.want(item.position());
                    views::placeholder_card()
                }
            };
            bin.set_child(Some(&card));
        });
    }
    factory.connect_unbind(|_, item| {
        let item = item.downcast_ref::<gtk::ListItem>().expect("a list item");
        if let Some(bin) = item.child().and_downcast::<adw::Bin>() {
            bin.set_child(gtk::Widget::NONE);
        }
    });

    let grid = gtk::GridView::builder()
        .model(&gtk::NoSelection::new(Some(store)))
        .factory(&factory)
        .max_columns(16)
        .min_columns(1)
        .css_classes(["library-grid"])
        .build();
    Ok(grid)
}
