// SPDX-License-Identifier: CC0-1.0

//! Which bulk-operation targets this endpoint BINDS, pinned at the layer that routes them.
//!
//! A request reaches an operation by the target it names, and the table that resolves the
//! target is this service's own ([`super::management_events::audited_table`], which is what
//! [`super::EmulatedService`]'s definition answers with). A target the table holds no row
//! for is answered by the front door as one the endpoint binds nothing for
//! ([`super::front_door::unknown_operation_response`]) — which tells a caller the API does
//! not exist rather than what is wrong with its request, so an operation missing from that
//! table is invisible until someone calls it.
//!
//! The bulk-operation family is the one this has happened to, so it is pinned here: the four
//! targets it binds resolve, and names close to them do not. The end-to-end behaviour of
//! each one is its own suite's (`tests/all/bulk_operation_management.rs`,
//! `tests/all/bulk_put_items_operations.rs`); what this reads back is only that each target
//! is bound at all.

#![cfg(test)]

use smithy_server_runtime::Service;

/// The service, built from one shared engine exactly as the router builds it.
fn service() -> super::EmulatedService<::engine::InMemoryEngine> {
    super::EmulatedService::new(::std::sync::Arc::new(::engine::InMemoryEngine::new()))
}

/// Whether the table this service routes through holds a row for `target`.
fn binds(target: &str) -> bool {
    service().definition().routing_table().get(target).is_some()
}

#[test]
fn every_bulk_operation_target_resolves_to_an_operation() {
    for target in [
        "StartBulkPutItems",
        "ListBulkOperations",
        "DescribeBulkOperation",
        "CancelBulkOperation",
    ] {
        assert!(
            binds(target),
            "case {target:?}: the endpoint binds this target, so the table that routes its \
             requests must hold a row for it"
        );
    }
}

#[test]
fn a_name_this_endpoint_binds_nothing_for_resolves_to_no_operation() {
    // The control for the rows above: the table binds the targets themselves and not names
    // close to them, so a row there is the operation and not a prefix match.
    for target in [
        "DescribeBulkOperations",
        "StopBulkOperation",
        "BulkOperation",
        "",
    ] {
        assert!(
            !binds(target),
            "case {target:?}: the endpoint binds no such target, so the table must hold no \
             row for it"
        );
    }
}
