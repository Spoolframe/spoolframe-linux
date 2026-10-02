//! The part of Spoolframe Linux that does not need GTK: the themes, and the
//! backend the UI reads its data from. Everything here is tested without a
//! display; the app crate only draws it
//! (`docs/agent_guide/crate_layering.md`).

pub mod artcache;
pub mod backend;
pub mod fake;
pub mod format;
pub mod reader;
pub mod sdk;
pub mod storage;
pub mod theme;
