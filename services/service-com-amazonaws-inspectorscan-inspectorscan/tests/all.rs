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
#[path = "all/scan_inspector_alt_form.rs"]
mod scan_inspector_alt_form;
#[path = "all/scan_invariants.rs"]
mod scan_invariants;
#[path = "all/scan_report_annotations.rs"]
mod scan_report_annotations;
#[path = "all/scan_report_links.rs"]
mod scan_report_links;
#[path = "all/scan_sbom.rs"]
mod scan_sbom;
#[path = "all/scan_sbom_authorization.rs"]
mod scan_sbom_authorization;
#[path = "all/scan_sbom_component_fan_out.rs"]
mod scan_sbom_component_fan_out;
#[path = "all/scan_sbom_enum_value_set.rs"]
mod scan_sbom_enum_value_set;
#[path = "all/scan_sbom_refusals.rs"]
mod scan_sbom_refusals;
#[path = "all/scan_sbom_scoring_vendors.rs"]
mod scan_sbom_scoring_vendors;
#[path = "all/scan_skip_reasons.rs"]
mod scan_skip_reasons;
#[path = "all/scanner_build_identity.rs"]
mod scanner_build_identity;
#[path = "all/state_roundtrip.rs"]
mod state_roundtrip;
