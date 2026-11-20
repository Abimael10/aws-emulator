// SPDX-License-Identifier: CC0-1.0

//! Superseded by `tests/all/refusal_message_points_at_the_authorization_detail.rs`.
//!
//! This suite pinned the `AccessDenied` message of a refused IAM call as ENDING at the
//! clause saying why the decision went that way. Recorded behaviour of this API is that the
//! message carries one more clause after it — the pointer at the authorization detail of
//! that decision, naming the authorization id IAM mints per refused request — so the rule
//! this file asserted is not the one the service follows, and the table of operations and
//! resource forms it ran now lives in the suite named above, asserting the end of the
//! message AWS records.
