//! A library as a grid of posters that loads as it scrolls.
//!
//! A library can hold thousands of items, and the server serves them in
//! pages of at most 500. The grid is a GtkGridView over a list as long as
//! the library: it builds widgets only for the rows on screen and recycles
//! them, and a page is fetched the first time one of its items comes into
//! view. Until then its items are placeholder tiles.
//!
//! Libraries change rarely, so a session keeps each one's items in memory
//! ([`Cache`]): opening a library again shows it at once, and the pages
//! already seen are fetched again in the background to pick up changes.
//! Every library's first page is fetched in the background as soon as the
//! libraries are known, so even a first visit rarely waits.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
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
        if self.asked.borrow_mut().insert(page) {
            self.fetch(page);
        }
    }

    /// Fetches `page` and puts its items in place.
    fn fetch(self: &Rc<Self>, page: u32) {
        let session = self.session.clone();
        let id = self.library.id.clone();
        let fetch = relm4::spawn(async move { session.items(&id, page * PAGE, PAGE).await });
        let pages = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let result = fetch.await;
            let Some(pages) = pages.upgrade() else { return };
            match result {
                Ok(Ok((items, total))) => {
                    pages.resize(total);
                    pages.fill(page * PAGE, items);
                }
                Ok(Err(e)) => {
                    tracing::warn!(error = %e, page, "library page not loaded");
                    // Asked again when its items come into view again.
                    pages.asked.borrow_mut().remove(&page);
                }
                Err(e) => tracing::warn!(error = %e, page, "library page fetch failed"),
            }
        });
    }

    /// Makes the list as long as the library now is: placeholders added at
    /// the end, or cells taken off it.
    fn resize(&self, total: u32) {
        let now = self.store.n_items();
        if total > now {
            let more: Vec<glib::BoxedAnyObject> = (now..total)
                .map(|_| glib::BoxedAnyObject::new(None as Cell))
                .collect();
            self.store.splice(now, 0, &more);
        } else if total < now {
            self.store
                .splice(total, now - total, &[] as &[glib::BoxedAnyObject]);
        }
    }

    /// Fetches again every page fetched before, to pick up changes.
    fn refresh(self: &Rc<Self>) {
        let pages: Vec<u32> = self.asked.borrow().iter().copied().collect();
        for page in pages {
            self.fetch(page);
        }
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

/// The libraries of one session, as far as they have been fetched.
pub struct Cache {
    session: Arc<dyn Session>,
    libraries: RefCell<HashMap<String, Rc<Pages>>>,
}

impl Cache {
    pub fn new(session: Arc<dyn Session>) -> Rc<Self> {
        Rc::new(Self {
            session,
            libraries: RefCell::new(HashMap::new()),
        })
    }

    /// Fetches `library`'s first page in the background, if it has not
    /// been fetched yet.
    pub fn prefetch(self: &Rc<Self>, library: Library) {
        if self
            .libraries
            .borrow()
            .contains_key(&library.id.to_string())
        {
            return;
        }
        let cache = self.clone();
        glib::spawn_future_local(async move {
            if let Err(e) = cache.pages(library).await {
                tracing::debug!(error = %e, "library not fetched ahead");
            }
        });
    }

    /// `library`'s pages: kept ones, or a first page fetched now.
    async fn pages(&self, library: Library) -> Result<Rc<Pages>, BackendError> {
        let key = library.id.to_string();
        if let Some(pages) = self.libraries.borrow().get(&key) {
            return Ok(pages.clone());
        }
        let (items, total) = {
            let (session, id) = (self.session.clone(), library.id.clone());
            relm4::spawn(async move { session.items(&id, 0, PAGE).await })
                .await
                .map_err(|e| BackendError::Other(format!("the call did not finish: {e}")))??
        };
        // Fetched twice at once (ahead, and by opening it): keep the first.
        if let Some(pages) = self.libraries.borrow().get(&key) {
            return Ok(pages.clone());
        }
        let store = gio::ListStore::new::<glib::BoxedAnyObject>();
        let pages = Rc::new(Pages {
            session: self.session.clone(),
            library,
            store,
            asked: RefCell::new(HashSet::from([0])),
        });
        pages.resize(total);
        pages.fill(0, items);
        self.libraries.borrow_mut().insert(key, pages.clone());
        Ok(pages)
    }

    /// The grid for `library`, or what went wrong getting its first page.
    /// A library seen before shows at once, and refreshes behind it.
    pub async fn grid(
        &self,
        library: Library,
        cards: Rc<Cards>,
    ) -> Result<gtk::GridView, BackendError> {
        let kept = self
            .libraries
            .borrow()
            .get(&library.id.to_string())
            .cloned();
        let pages = match kept {
            Some(pages) => {
                pages.refresh();
                pages
            }
            None => self.pages(library).await?,
        };
        Ok(grid(&pages, cards))
    }
}

/// A grid over `pages`; their items fill in as they arrive.
fn grid(pages: &Rc<Pages>, cards: Rc<Cards>) -> gtk::GridView {
    let store = pages.store.clone();
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
                Some(media) => views::grid_card(media, &cards),
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

    gtk::GridView::builder()
        .model(&gtk::NoSelection::new(Some(store)))
        .factory(&factory)
        .max_columns(16)
        .min_columns(1)
        .css_classes(["library-grid"])
        .build()
}

#[cfg(test)]
mod tests {
    use spoolframe_linux_core::backend::Client;
    use spoolframe_linux_core::fake::FakeClient;

    use super::*;

    fn pages() -> Pages {
        let client = FakeClient::new(None);
        let profile = FakeClient::profile("andrey");
        let session = client.session(&profile).unwrap();
        // A context of its own: tests run on parallel threads.
        let library = glib::MainContext::new()
            .block_on(session.libraries())
            .unwrap()
            .remove(0);
        Pages {
            session,
            library,
            store: gio::ListStore::new::<glib::BoxedAnyObject>(),
            asked: RefCell::new(HashSet::new()),
        }
    }

    /// A library that grew while kept gets placeholders for its new items.
    #[test]
    fn a_grown_library_gets_more_cells() {
        let pages = pages();
        pages.resize(3);
        pages.resize(5);
        assert_eq!(pages.store.n_items(), 5);
    }

    /// One that shrank loses the cells past its end.
    #[test]
    fn a_shrunk_library_loses_cells() {
        let pages = pages();
        pages.resize(5);
        pages.resize(2);
        assert_eq!(pages.store.n_items(), 2);
    }
}
