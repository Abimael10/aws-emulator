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
#[path = "all/batch_quota_refusal.rs"]
mod batch_quota_refusal;
#[path = "all/common.rs"]
mod common;
#[path = "all/control_plane_registry.rs"]
mod control_plane_registry;
#[path = "all/data_plane_authorization.rs"]
mod data_plane_authorization;
#[path = "all/malformed_arn_refusal.rs"]
mod malformed_arn_refusal;
#[path = "all/published_cluster_endpoint.rs"]
mod published_cluster_endpoint;
#[path = "all/routing_control_toggle.rs"]
mod routing_control_toggle;
#[path = "all/safety_rule_enforcement.rs"]
mod safety_rule_enforcement;
#[path = "all/safety_rule_refusal_message.rs"]
mod safety_rule_refusal_message;
#[path = "all/state_roundtrip.rs"]
mod state_roundtrip;
