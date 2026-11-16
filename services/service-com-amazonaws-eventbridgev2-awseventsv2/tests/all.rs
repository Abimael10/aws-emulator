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
#[path = "all/collision_wording_and_detail_json.rs"]
mod collision_wording_and_detail_json;
#[path = "all/create_subscriber_contract.rs"]
mod create_subscriber_contract;
#[path = "all/event_bus_lifecycle.rs"]
mod event_bus_lifecycle;
#[path = "all/event_bus_read_projection.rs"]
mod event_bus_read_projection;
#[path = "all/event_source_lifecycle.rs"]
mod event_source_lifecycle;
#[path = "all/modelled_input_refusal.rs"]
mod modelled_input_refusal;
#[path = "all/put_events_entry_results.rs"]
mod put_events_entry_results;
#[path = "all/resource_already_exists_wording.rs"]
mod resource_already_exists_wording;
#[path = "all/revoke_resource.rs"]
mod revoke_resource;
#[path = "all/state_roundtrip.rs"]
mod state_roundtrip;
#[path = "all/subscriber_delivery.rs"]
mod subscriber_delivery;
#[path = "all/subscriber_removal.rs"]
mod subscriber_removal;
