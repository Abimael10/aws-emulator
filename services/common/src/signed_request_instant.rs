// SPDX-License-Identifier: CC0-1.0

//! Intentionally empty, and not declared as a module anywhere.
//!
//! This file briefly held a reader for the instant a request STATES it was made at — its
//! SigV4 stamp, else the day its credential scope was signed for. Nothing in this crate
//! calls it: the operation that motivated it (an ACM import held to the validity window of
//! the certificate it carries) does not compare a clock with the request's own material at
//! all any more, because an answer that depends on how the serving host's clock stands
//! against a value pinned inside the request is not a property of the request.
//!
//! A copy of the SigV4 stamp reading still lives privately in
//! `services/service-com-amazonaws-s3-amazons3/src/service/sigv4.rs` (`amz_instant`), where
//! it backs that service's own request-skew refusal; the right home for it, if a second
//! service ever needs it, is this crate.
//!
//! The file is left in place because the issue-fixer tooling can write a file but cannot
//! remove one; nothing compiles it (no module declares it), and it may be deleted outright
//! by anyone touching this crate. Same note as `sigv4_scope.rs`, `service_claim.rs`,
//! `route_claim.rs` and `modeled_route_claim.rs`.
