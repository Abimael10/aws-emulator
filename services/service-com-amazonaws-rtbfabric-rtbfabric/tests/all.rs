// SPDX-License-Identifier: CC0-1.0

//! Single integration-test target for this crate.
//!
//! Put a new suite at `tests/all/<name>.rs` and declare it below, keeping the list
//! sorted.
//!
//! A suite that is not declared here compiles nowhere and its tests never run. A
//! suite left at the top level of `tests/` fails the build.

// `tests/all.rs` is a test-target CRATE ROOT, so a bare `mod` resolves to a SIBLING
// (`tests/state_roundtrip.rs`) — which the layout check refuses. The `#[path]` is what
// points it into `tests/all/`.
#[path = "all/certificate_association.rs"]
mod certificate_association;
#[path = "all/gateway_link_lifecycle.rs"]
mod gateway_link_lifecycle;
#[path = "all/link_rejection.rs"]
mod link_rejection;
#[path = "all/link_routing_rules.rs"]
mod link_routing_rules;
#[path = "all/not_found_message_wording.rs"]
mod not_found_message_wording;
#[path = "all/requester_gateway_deletion.rs"]
mod requester_gateway_deletion;
#[path = "all/requester_gateway_listing.rs"]
mod requester_gateway_listing;
#[path = "all/resource_tagging.rs"]
mod resource_tagging;
#[path = "all/responder_gateway_listing.rs"]
mod responder_gateway_listing;
#[path = "all/state_roundtrip.rs"]
mod state_roundtrip;
#[path = "all/validation_message_wording.rs"]
mod validation_message_wording;
