//! The part of Spoolframe Linux that does not need GTK: the themes, and the
//! backend the UI reads its data from. Everything here is tested without a
//! display; the app crate only draws it.

pub mod artcache;
pub mod backend;
pub mod catalog;
pub mod downloads;
// A server answering from fixed data, for tests only: the app talks to
// real servers, through the SDK.
#[cfg(any(test, feature = "testing"))]
pub mod fake;
pub mod fido2;
pub mod format;
pub mod helper;
pub mod history;
pub mod identity;
pub mod markers;
pub mod next_episode;
pub mod passkey;
pub mod paths;
pub mod playlists;
pub mod progressive;
pub mod reader;
pub mod sdk;
pub mod security_key;
pub mod storage;
#[cfg(any(test, feature = "testing"))]
pub mod testing;
pub mod theme;

pub mod playback;
