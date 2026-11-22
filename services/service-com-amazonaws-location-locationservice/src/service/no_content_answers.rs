// SPDX-License-Identifier: CC0-1.0

//! Which answers of this service carry NO CONTENT, and where their framing comes from.
//!
//! An operation whose modelled output structure declares no member leaves its caller nothing
//! to parse, and such a success answers `204 No Content`. Two facts carry that, and this
//! module is where this service NAMES them — it holds no copy of either:
//!
//! * the STATUS, stated per operation on `Operation::SUCCESS_STATUS`: the two mutations of
//!   this service's resource-tagging plane (`src/ops/tag_resource.rs`,
//!   `src/ops/untag_resource.rs`) answer at it;
//! * the FRAMING, which an operation cannot state: the `restJson1` serializer renders a
//!   members-less output structure as the empty JSON object `{}` — it is a `structure`, not
//!   `smithy.api#Unit`, the one shape the renderers treat as bodyless — so the rendered
//!   response is handed through [`without_content`] at the protocol seam
//!   [`super::date_time_timestamps::DateTimeRestJson`], the one place every response a
//!   modelled operation of this service produces passes through.
//!
//! Both are `service_common::no_content_responses`', this tree's one reading of that framing.
//! Naming them here once is what keeps the operations that answer the status and the seam that
//! frames it from drifting apart.
//!
//! A response that DOES carry content — every reader's, every create's, every modelled
//! failure's — is returned exactly as its renderer wrote it.

pub(crate) use ::service_common::no_content_responses::{without_content, NO_CONTENT_STATUS};
