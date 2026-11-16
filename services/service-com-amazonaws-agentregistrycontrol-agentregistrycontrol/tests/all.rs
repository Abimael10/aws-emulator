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
#[path = "all/date_time_timestamp_wire_form.rs"]
mod date_time_timestamp_wire_form;
#[path = "all/error_message_wording.rs"]
mod error_message_wording;
#[path = "all/idempotency_token_binding.rs"]
mod idempotency_token_binding;
#[path = "all/idempotency_tokens.rs"]
mod idempotency_tokens;
#[path = "all/record_approval.rs"]
mod record_approval;
#[path = "all/record_patch_semantics.rs"]
mod record_patch_semantics;
#[path = "all/record_status_curation.rs"]
mod record_status_curation;
#[path = "all/registry_delete_precondition.rs"]
mod registry_delete_precondition;
#[path = "all/registry_lifecycle.rs"]
mod registry_lifecycle;
#[path = "all/resource_tagging.rs"]
mod resource_tagging;
#[path = "all/state_roundtrip.rs"]
mod state_roundtrip;
