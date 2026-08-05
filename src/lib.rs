//! Library side of barback.
//!
//! See [`integrations`] for the remote calendar services (Google, Microsoft,
//! CalDAV, ICS feeds). The app's local calendar access goes through EventKit
//! in the binary instead, which is a separate concern and a separate module.

pub mod integrations;
