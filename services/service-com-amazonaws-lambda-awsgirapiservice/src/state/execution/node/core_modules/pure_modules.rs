// SPDX-License-Identifier: CC0-1.0

//! The core modules of this runtime that are PURE COMPUTATION over the values a run already
//! holds — `path`, `util` and `events` — taken from the one place this emulator keeps them.
//!
//! They reach nothing of the host and nothing of Lambda: a path algebra over strings, a
//! value formatter and an event emitter are the same modules every service that runs
//! caller-supplied Node.js code has to supply, so their source lives once in
//! [`::service_common::node_guest_modules`] and both this runtime and the web function
//! runtime compile THAT source. What stays in this crate is what is this service's own: the
//! modules its execution environment answers from (`os`, `fs`, the HTTP client, the name
//! service) and the protocol a handler is invoked over.

pub use ::service_common::node_guest_modules::{EVENTS_SOURCE, PATH_SOURCE, UTIL_SOURCE};
