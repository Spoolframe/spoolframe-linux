//! The part of Spoolframe Linux that does not need GTK: the themes, and the
//! backend the UI reads its data from. Everything here is tested without a
//! display; the app crate only draws it
//! (`docs/agent_guide/crate_layering.md`).

pub mod artcache;
pub mod backend;
// A server answering from fixed data, for tests only: the app talks to
// real servers, through the SDK.
#[cfg(any(test, feature = "testing"))]
pub mod fake;
pub mod format;
pub mod markers;
pub mod next_episode;
pub mod passkey;
pub mod paths;
pub mod progressive;
pub mod reader;
pub mod sdk;
pub mod storage;
#[cfg(any(test, feature = "testing"))]
pub mod testing;
pub mod theme;

pub mod playback;
