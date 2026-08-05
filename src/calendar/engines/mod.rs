//! Protocol implementations. Pick one through
//! [`ProviderKind`](crate::calendar::providers::ProviderKind) rather than
//! constructing them directly.

pub mod caldav;
pub mod dav;
pub mod google;
pub mod graph;
pub mod ics;
