// SPDX-License-Identifier: CC0-1.0

//! A constraint a LIST's ELEMENTS break, stated the way this service states it: at the list
//! member, quoting the list the request sent.
//!
//! ## The rule
//!
//! The framework's constraint walk reports such a violation at the ELEMENT it found it on —
//! `'<list>.<1-based position>.member'`, quoting that one element and naming only the
//! constraint that element broke — because that is where the value is. SageMaker reports it
//! at the list MEMBER: the subject is the WHOLE list that member held, rendered the way a
//! list renders and in the order the request sent it, and the requirement is the element
//! shape's own complete constraint set nested inside `Member must satisfy constraint: [ … ]`.
//! So a list with two offending entries is ONE validation error, and a caller is told
//! everything an entry has to satisfy rather than the one bound that happened to fail.
//!
//! That is one rule about every list member of every operation rather than one about a
//! named list, so the clause is built once, over the violations the framework's own walk
//! found ([`super::validation::refusal`]), rather than in any handler: nothing here
//! re-checks anything, and a request the framework would accept is never refused by it.
//!
//! The grouping, the nested set and the envelope are shared with the rest of the tree —
//! the set is read off the MODEL (the operation's own input schema, since the refusal
//! carries only the constraints that FAILED) through
//! [`::service_common::validation_message::element_constraint_set`], and the wrapper around
//! it is that module's [`::service_common::validation_message::element_set_requirement`].
//! What this module owns is the collecting half.
//!
//! ## Where it is applied
//!
//! In an operation wrapper every routable request of this service passes through
//! ([`StatedFromRequest`]), because the collecting half needs the REQUEST:
//! `Service::override_refusal` is handed violations alone, and the value half of a clause
//! stated at a list member is that member's own value, which no violation carries. The cost
//! is that a request whose operation declares such a list is read twice — once here and once
//! in the operation's own sequence — which `services/common/src/validation.rs` records as the
//! core seam this re-read exists only until.
//!
//! Only the operations whose input declares a list of SCALARS of its OWN are collected
//! ([`collector`]): an operation with no such member has no list element to report a
//! constraint at, so it is not read twice for nothing. That is also the reach of the rule
//! as it stands, and the reason is the SET rather than the collecting: the element shape's
//! constraints are read off the operation's own input shape, which is the schema codegen
//! attaches a top-level list member's element constraints to, so a list nested INSIDE a
//! structure the input carries resolves to no set and keeps the framework's own per-element
//! clause. A recording that shows such a nested list stated at its member is the signal to
//! hand the declaring structures' schemas over too, which is the one change that needs.

use std::collections::{HashMap, HashSet};

use ::aws_smithy_runtime_api::http::{Request as RtRequest, Response as RtResponse};
use ::aws_smithy_schema_server::ProtocolError;
use ::service_common::validation::{list_of_element, refusing_violations_with_lists};
use ::smithy_server_runtime::{
    BoxFuture, ErasedOperation, Operation, OperationContext, Refused, ServerHttpProtocol, Service,
    Violation,
};

/// How one operation's refusing violations and submitted lists are collected: the
/// framework's own walk over that operation's input schema, which is why a collector is
/// needed per operation rather than one for the service.
type Collect = fn(&dyn ServerHttpProtocol, &RtRequest) -> Option<(Vec<Violation>, Refused)>;

/// The collector for each operation this module states the refusal of, resolved by the
/// operation's own wire name ([`Operation::NAME`], so no name is spelled twice).
macro_rules! collected_per_operation {
    ( $( $module:ident :: $operation:ident ),* $(,)? ) => {
        /// The collector for the operation `key` names, or `None` for one whose input
        /// declares no list of scalar values of its own — the members a clause of this
        /// class can be about at all.
        fn collector(key: &str) -> Option<Collect> {
            $(
                if key == <crate::ops::$module::$operation<()> as Operation>::NAME {
                    return Some(stated_from_request::<crate::ops::$module::$operation<()>>);
                }
            )*
            None
        }
    };
}

// Every operation of this service whose own input declares a list of scalars — the node and
// logical-id lists of the batch node calls, the model-package ARNs of a batch describe, a
// tag-key or device-name list, the subnets and security groups of a Studio domain, a
// notebook instance's code repositories and accelerators, an image's aliases, a hub's search
// keywords, a lineage query's start ARNs, a training plan's target resources, and the
// "remove these" lists the lineage and metadata updates carry. The operation TYPE is named
// with a placeholder engine, because nothing here constructs one: only the operation's name
// and its input schema are read, and both are associated constants of the type.
collected_per_operation!(
    batch_delete_cluster_nodes::BatchDeleteClusterNodes,
    batch_describe_model_package::BatchDescribeModelPackage,
    batch_reboot_cluster_nodes::BatchRebootClusterNodes,
    batch_replace_cluster_nodes::BatchReplaceClusterNodes,
    create_domain::CreateDomain,
    create_hub::CreateHub,
    create_image_version::CreateImageVersion,
    create_mlflow_app::CreateMlflowApp,
    create_notebook_instance::CreateNotebookInstance,
    create_optimization_job::CreateOptimizationJob,
    delete_tags::DeleteTags,
    deregister_devices::DeregisterDevices,
    import_hub_content::ImportHubContent,
    query_lineage::QueryLineage,
    search_training_plan_offerings::SearchTrainingPlanOfferings,
    update_action::UpdateAction,
    update_artifact::UpdateArtifact,
    update_cluster::UpdateCluster,
    update_context::UpdateContext,
    update_domain::UpdateDomain,
    update_feature_metadata::UpdateFeatureMetadata,
    update_hub::UpdateHub,
    update_hub_content::UpdateHubContent,
    update_image::UpdateImage,
    update_image_version::UpdateImageVersion,
    update_mlflow_app::UpdateMlflowApp,
    update_model_package::UpdateModelPackage,
    update_notebook_instance::UpdateNotebookInstance,
    update_trial_component::UpdateTrialComponent,
);

/// The refusal this service states for `req` under the operation `O`, or `None` when the
/// request breaks no constraint of a list's ELEMENTS — which leaves the framework's own
/// answer, and this service's other wording of it, exactly as they are.
fn stated_from_request<O: Operation>(
    protocol: &dyn ServerHttpProtocol,
    req: &RtRequest,
) -> Option<(Vec<Violation>, Refused)> {
    // The violations AND the lists the request carried, out of ONE walk: the value half of
    // a clause stated at a list member is that member's own value, which no violation
    // carries.
    let (violations, submitted) = refusing_violations_with_lists::<O>(protocol, req);
    // Only this class is reworded here. Everything else — an absent required member, a
    // bound on a plain member, a value outside an enum a plain member targets — keeps the
    // answer the centralized seam already gives it, in this service's own wording.
    if !violations
        .iter()
        .any(|violation| list_of_element(&violation.path).is_some())
    {
        return None;
    }
    let quoted = quotable_lists(&violations, submitted);
    let stated = super::validation::refusal(&violations, &[O::INPUT_SCHEMA], &quoted)?;
    Some((violations, stated))
}

/// The list each clause may quote: `submitted`, the lists the request carried rendered the
/// way AWS quotes a list-valued member (`[<entry>, <entry>]`,
/// [`::service_common::validation::submitted_lists`]), minus every one this refusal is not
/// allowed to report.
///
/// The clause's value half is the WHOLE submitted list, in the order the request sent it: it
/// is the value that member held, which is what tells a caller which list it submitted and
/// which of its entries it has to fix. The collecting walk owns the rendering and every
/// bound on it, and offers a list only when it collected that list in FULL, so a clause can
/// never quote a list the caller did not send.
///
/// A collection one of whose violations carries NO value (a `@sensitive` element, whose
/// value the validating walk withholds) is dropped here, so no clause can quote a value the
/// refusal may not report; such a clause states the member alone. BOUNDED by the violation
/// count the walk itself bounds (`::service_common::validation::MAX_VIOLATIONS`).
fn quotable_lists(
    violations: &[Violation],
    submitted: HashMap<String, String>,
) -> HashMap<String, String> {
    let withheld: HashSet<&str> = violations
        .iter()
        .filter(|violation| violation.value.is_none())
        .filter_map(|violation| list_of_element(&violation.path))
        .collect();
    submitted
        .into_iter()
        .filter(|(collection, _)| !withheld.contains(collection.as_str()))
        .collect()
}

/// The refusal this service states for one request from the request itself, by the operation
/// its routing key names, or `None` for an operation this class cannot arise in and for a
/// request that breaks no such constraint.
fn stated(
    protocol: &dyn ServerHttpProtocol,
    req: &RtRequest,
) -> Option<(Vec<Violation>, Refused)> {
    let key = protocol.routing_key(req).ok()?;
    collector(key)?(protocol, req)
}

/// The modelled operation this service's routing table names for this request, or `None`
/// when the request names none.
fn modeled_operation<'s>(svc: &'s dyn Service, req: &RtRequest) -> Option<&'s dyn ErasedOperation> {
    let key = svc.definition().protocol().routing_key(req).ok()?;
    svc.definition().routing_table().get(key)
}

/// Serves a modelled operation through the framework exactly as the routing table would,
/// after answering the one refusal this service states from the REQUEST rather than from the
/// violations alone (see the module header).
///
/// It stores no operation of its own: the one it delegates to is the one the service's
/// routing table names for the request, so the framework still owns the whole
/// deserialize → validate → handle → serialize sequence, every status, code and message it
/// answers with, and that operation's own telemetry.
pub struct StatedFromRequest;

impl ErasedOperation for StatedFromRequest {
    fn run<'r>(
        &'r self,
        svc: &'r dyn Service,
        req: &'r RtRequest,
        ctx: &'r OperationContext<'r>,
    ) -> BoxFuture<'r, RtResponse> {
        Box::pin(async move {
            // The definition is read, used and dropped inside each statement:
            // `&dyn ErasedServiceDefinition` is not `Sync`, so holding it across the await
            // below would make this future non-`Send`.
            let refused = stated(svc.definition().protocol(), req);
            if let Some((violations, stated)) = refused {
                return svc.definition().refusal_response(
                    ProtocolError::Invalid(violations),
                    Some(stated),
                    req,
                );
            }
            match modeled_operation(svc, req) {
                Some(operation) => operation.run(svc, req, ctx).await,
                // Unreachable through `Service::override_operation`, which hands this
                // wrapper back only for a request the routing table holds the key of;
                // answered as the framework answers an unroutable request rather than
                // guessed at.
                None => svc.definition().routing_error_response(req),
            }
        })
    }

    fn auth_schemes(&self) -> Option<&'static [&'static str]> {
        // `None` is "defer to the modelled operation the routing key names" — the operation
        // this wrapper delegates to, so its model-declared schemes decide and wrapping an
        // operation cannot weaken them.
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: an operation whose own input declares a list of scalars is collected, and one
    /// that declares none is not — so no request is read twice for a clause that cannot
    /// arise in it.
    #[test]
    fn the_operations_collected_are_the_ones_with_a_list_input() {
        for key in [
            "BatchDeleteClusterNodes",
            "BatchRebootClusterNodes",
            "BatchReplaceClusterNodes",
            "BatchDescribeModelPackage",
            "DeleteTags",
            "CreateOptimizationJob",
            "UpdateTrialComponent",
        ] {
            assert!(
                collector(key).is_some(),
                "case {key:?}: expected the operation to be collected"
            );
        }
        for key in [
            "AttachClusterNodeVolume",
            "CreateComputeQuota",
            "CreateMlflowTrackingServer",
            "DescribeCluster",
            "NotAnOperation",
        ] {
            assert!(
                collector(key).is_none(),
                "case {key:?}: expected the operation not to be collected"
            );
        }
    }
}
