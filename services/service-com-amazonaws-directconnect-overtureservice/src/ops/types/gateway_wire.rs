// SPDX-License-Identifier: CC0-1.0

//! NOT COMPILED, and holds nothing: no module declares this file.
//!
//! It was added by an earlier round of this fix to carry the two members a Direct Connect
//! gateway record puts on the wire that the on-disk model did not then declare —
//! `directConnectGatewayArn` and `directConnectGatewayType` — because
//! `#[smithy_macros::smithy(shape = ...)]` admits exactly the model's member list. The
//! model this crate ships now declares both, so the derive emits their schema and codec
//! and the one projection of a stored gateway fills them like any other member; nothing
//! is left for this file to hold, and `types/mod.rs` no longer declares it.
//!
//! The file itself could not be removed from the fixer's checkout, which has no delete.
