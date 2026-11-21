// SPDX-License-Identifier: CC0-1.0

//! The two TEXT CODECS of this runtime — `querystring` and `url` — taken from the one place
//! this emulator keeps them.
//!
//! A query codec and a URL object are pure computation over strings: no host path, socket,
//! process or environment is reachable from either, and both are the same modules every
//! service that runs caller-supplied Node.js code has to supply. Their source therefore
//! lives once in [`::service_common::node_guest_modules`] and both this runtime and the web
//! function runtime compile THAT source.

pub use ::service_common::node_guest_modules::{QUERYSTRING_SOURCE, URL_SOURCE};
