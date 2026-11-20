// SPDX-License-Identifier: CC0-1.0

//! What THIS service states about its own constraint clauses, and which of its operations
//! are served through the layer that states them.
//!
//! The rule, the wrapper and every guard around it are the tree's shared ones
//! (`service_common::submitted_collections`, the one home for the half of a constraint
//! refusal the framework's walk cannot state — the COLLECTION a clause about a collection
//! member is about). This file supplies only the facts that are this service's own: the
//! error code its constraint refusals carry, the member an error body reports its text in
//! ([`super::error_message_member`]), the order it states a map's entries in
//! ([`super::submitted_map`]), and which operations pay for the collection pass.

use std::sync::Arc;

use service_common::submitted_collections::{StatedClause, StatedClauseTable};
use smithy_server_runtime::{ErasedOperation, Operation, OperationRoutingTable};

use super::data_endpoint::AtTheDataEndpoint;

/// The error code a constraint refusal of this service carries, which is the one
/// [`super::EmulatedService::override_refusal`] picks for the same class: the restated
/// message travels with the code it would have travelled with, because a client branches
/// on the code.
const INVALID_INPUT: &str = "InvalidInput";

/// What this service states about one of its constraint clauses: the modelled error code,
/// the member an error body reports its text in, and the order a quoted map's entries are
/// stated in.
fn stated_clause() -> StatedClause {
    StatedClause::under(INVALID_INPUT)
        .ordering_entries(super::submitted_map::in_iteration_order)
        .answered(super::error_message_member::with_modelled_message_member)
}

/// Serve `$operation` through the shared layer, each built from the shared engine: one
/// `crate::ops::<module>::<Operation>` per line, filed under its own modelled name.
///
/// `quoting` is for an operation whose model declares a BOUNDED COLLECTION member, the only
/// kind that can produce a clause about a collection — it re-reads the request to state
/// that clause. `answering` is every other operation: its responses are answered as this
/// service answers them, and its body is read once.
macro_rules! serve {
    ($table:expr, $engine:expr, $served:ident, $($module:ident :: $operation:ident),+ $(,)?) => {
        $(
            $table.$served(
                crate::ops::$module::$operation::new(::std::sync::Arc::clone($engine)),
            );
        )+
    };
}

/// The same modelled operations the generated table holds, each wrapped so that the error
/// bodies it answers with are the ones this service answers with, and built from the SAME
/// shared engine, so a wrapped operation reads and writes the one store.
///
/// EVERY operation of this model is listed, because the member an error body reports its
/// text in is a property of this service's error bodies rather than of any one operation.
/// The collection pass is listed separately: `UpdateServiceAttributes` is the only
/// operation whose model declares a bounded collection member whose clause the framework's
/// walk cannot state — `ServiceAttributesMap` (`@length` 1..30), whose rejected value the
/// walk renders as its entry count. The `TagList` members of the tag writes and the creates
/// are `@length` 0..200 as well, so they are served quoting too; every other operation
/// takes the cheap path.
///
/// The operations the model hosts on an endpoint of their OWN are listed last, served
/// through the layer that words their constraint refusals the way the validation in front
/// of that endpoint words one ([`super::data_endpoint`]) — the member an error body reports
/// its text in is unchanged for them.
pub(super) fn stated_error_bodies<Eng: Send + Sync + 'static>(
    engine: &Arc<Eng>,
) -> OperationRoutingTable {
    let mut table = StatedClauseTable::new(stated_clause());
    serve!(
        table,
        engine,
        quoting,
        create_http_namespace::CreateHttpNamespace,
        create_private_dns_namespace::CreatePrivateDnsNamespace,
        create_public_dns_namespace::CreatePublicDnsNamespace,
        create_service::CreateService,
        tag_resource::TagResource,
        untag_resource::UntagResource,
        update_service_attributes::UpdateServiceAttributes,
    );
    serve!(
        table,
        engine,
        answering,
        delete_namespace::DeleteNamespace,
        delete_service::DeleteService,
        delete_service_attributes::DeleteServiceAttributes,
        deregister_instance::DeregisterInstance,
        get_instance::GetInstance,
        get_instances_health_status::GetInstancesHealthStatus,
        get_namespace::GetNamespace,
        get_operation::GetOperation,
        get_service::GetService,
        get_service_attributes::GetServiceAttributes,
        list_instances::ListInstances,
        list_namespaces::ListNamespaces,
        list_operations::ListOperations,
        list_services::ListServices,
        list_tags_for_resource::ListTagsForResource,
        register_instance::RegisterInstance,
        update_http_namespace::UpdateHttpNamespace,
        update_instance_custom_health_status::UpdateInstanceCustomHealthStatus,
        update_private_dns_namespace::UpdatePrivateDnsNamespace,
        update_public_dns_namespace::UpdatePublicDnsNamespace,
        update_service::UpdateService,
    );
    let mut table = table.finish();
    // The operations the model hosts on an endpoint of their own, where the generic
    // request-validation layer words a constraint refusal instead of this service
    // ([`super::data_endpoint`], whose census test pins which operations those are).
    at_the_data_endpoint(
        &mut table,
        crate::ops::discover_instances::DiscoverInstances::new(Arc::clone(engine)),
    );
    at_the_data_endpoint(
        &mut table,
        crate::ops::discover_instances_revision::DiscoverInstancesRevision::new(Arc::clone(engine)),
    );
    table
}

/// Serve `operation` at the data endpoint, under its modelled name: a request outside the
/// constraints the model declares is refused as that endpoint's validation refuses one, and
/// its responses are answered as this service answers them — the same `AnsweredAs` every
/// other operation above is served with.
fn at_the_data_endpoint<O: Operation + ErasedOperation + 'static>(
    table: &mut OperationRoutingTable,
    operation: O,
) {
    table.add_operation(
        O::NAME,
        Box::new(AtTheDataEndpoint::new(
            operation,
            super::error_message_member::with_modelled_message_member,
        )),
    );
}
