// SPDX-License-Identifier: CC0-1.0

//! The value half of a constraint refusal's clause: the bytes the caller submitted, however
//! long they were.
//!
//! # The gap this closes
//!
//! An AWS constraint-validation message is built from the REQUEST — `1 validation error
//! detected: Value '<value>' at '<member path>' failed to satisfy constraint: <requirement>`
//! — and the value between the quotes is the caller's own, in full: it is the only place the
//! server reports the offending input back, so a caller comparing the echo against what it
//! sent has to find its own bytes there. The framework's own constraint walk renders that
//! value through a ceiling of its own instead (a byte-bounded prefix followed by
//! `... [<n> bytes]`), which is wire text AWS never emits — so every refusal of a member
//! value longer than that ceiling quoted something the caller never submitted. A tag value
//! one character past its `@length` maximum is exactly such a value.
//!
//! The rule is the PROTOCOL's, not this service's, and the walk it belongs in cannot be
//! edited here (`vendor/` is read-only), so it is applied at the one seam a service owns:
//! each operation is served behind the shared wrapper
//! ([`service_common::quoted_refusals::QuotedRefusals`]), which re-reads the request through
//! the framework's OWN deserializer and constraint walk (which owns every bound on what one
//! clause may quote) and hands the violations to this service's own wording
//! (`Service::override_refusal` and the generated envelope) exactly as that walk would have.
//!
//! ONE clause is touched, and only its value half: the one whose stored text IS the
//! framework's elision of the value the request carried, which is the wrapper's
//! [`service_common::quoted_refusals::QuotedRefusals::restoring_elided_values`] mode. A clause
//! the framework wrote from something else — the `<n> items` summary of a collection whose own
//! `@length` was broken, a value with no text rendering, a `@sensitive` value the walk withheld
//! — keeps the framework's own text, because what AWS quotes there is that operation's own
//! recorded fact rather than this rule.
//!
//! # Which operations are served through it
//!
//! EVERY modelled operation. The rule reaches a value only where a request can carry one
//! past the walk's ceiling, and this model's constrained string members —
//! `XmlStringMaxLen255`, `XmlStringMaxLen1600`, `AsciiStringMaxLen255`, `TagValue` (0..256)
//! and the rest — are spread across the whole surface, so there is no operation whose
//! refusals could not quote an elided value. An operation whose input declares no such
//! member pays nothing for the row: the walk finds no violation and the wrapper hands the
//! request straight to the operation behind it.
//!
//! The three operations whose response can carry a floating-point member are served behind
//! BOTH wrappers ([`crate::service::decimal_floats`] inside this one), because the two rules
//! are about opposite halves of a call: one about how a refused request's value is quoted,
//! the other about how an answered response's number is written.
//!
//! # The one request this wrapper leaves to the walk behind it
//!
//! The collecting walk reads the request with the protocol the service's own
//! [`ErasedServiceDefinition`](smithy_server_runtime::ErasedServiceDefinition) carries — the
//! framework's `awsQuery` codec — while the walk that follows it reads a boolean member's
//! text in the wider lexical space this service's protocol admits
//! ([`crate::service::xsd_boolean`]). A request spelling a boolean the numeric way is
//! therefore unreadable to the collecting walk, which reports no violation for it at all, so
//! such a request is handed on and refused (or served) by the layers behind this wrapper
//! exactly as before. It cannot be refused HERE for a value the real walk admits: a request
//! the collecting deserializer cannot read yields no violations, and no violations means no
//! refusal.

use std::sync::Arc;

use service_common::quoted_refusals::QuotedRefusals;
use smithy_server_runtime::{ErasedOperation, Operation, OperationRoutingTable};

/// Serve one already-constructed operation behind the shared wrapper, filed under the
/// operation's own [`Operation::NAME`] — the model-derived key the generated routing table
/// uses, so a wrapped operation cannot land under a key the protocol never resolves.
///
/// [`QuotedRefusals::restoring_elided_values`] is the narrower of that wrapper's two modes:
/// it restores the clause whose stored text IS the framework's elision of the submitted value
/// and leaves every other clause the framework's own text, which is what this service's
/// recorded refusals state (see the module docs above).
fn serve<O: Operation + ErasedOperation + 'static>(
    table: &mut OperationRoutingTable,
    operation: O,
) {
    table.add_operation(
        O::NAME,
        Box::new(QuotedRefusals::restoring_elided_values(operation)),
    );
}

/// File each named operation, constructed from the shared engine, through [`serve`].
macro_rules! quoting_refused_values {
    (
        $table:expr, $engine:expr,
        $( $module:ident :: $operation:ident ),* $(,)?
    ) => {{
        $(
            serve(
                $table,
                crate::ops::$module::$operation::new(::std::sync::Arc::clone($engine)),
            );
        )*
    }};
}

/// The same, for an operation whose response is also written through the decimal renderer:
/// the two wrappers compose, the decimal one inside, and the composition is still the one
/// operation's own input schema the clause's value is restored from.
macro_rules! quoting_refused_values_of_decimal_responses {
    (
        $table:expr, $engine:expr,
        $( $module:ident :: $operation:ident ),* $(,)?
    ) => {{
        $(
            serve(
                $table,
                crate::service::decimal_floats::rendering_decimals(
                    crate::ops::$module::$operation::new(::std::sync::Arc::clone($engine)),
                ),
            );
        )*
    }};
}

/// Every modelled operation of this service, each constructed from the shared engine and
/// served behind the wrapper above.
///
/// A COMPLETE table: the generated routing table stays the fallback (see
/// [`crate::service::query_error_envelope`]), so an operation this list does not name is
/// still served — unwrapped — rather than unreachable.
pub fn table<Eng: Send + Sync + 'static>(engine: &Arc<Eng>) -> OperationRoutingTable {
    let mut table = OperationRoutingTable::new();
    quoting_refused_values!(
        &mut table, engine,
        attach_instances::AttachInstances,
        attach_load_balancer_target_groups::AttachLoadBalancerTargetGroups,
        attach_load_balancers::AttachLoadBalancers,
        attach_traffic_sources::AttachTrafficSources,
        batch_delete_scheduled_action::BatchDeleteScheduledAction,
        batch_put_scheduled_update_group_action::BatchPutScheduledUpdateGroupAction,
        cancel_instance_refresh::CancelInstanceRefresh,
        complete_lifecycle_action::CompleteLifecycleAction,
        create_auto_scaling_group::CreateAutoScalingGroup,
        create_launch_configuration::CreateLaunchConfiguration,
        create_or_update_tags::CreateOrUpdateTags,
        delete_auto_scaling_group::DeleteAutoScalingGroup,
        delete_launch_configuration::DeleteLaunchConfiguration,
        delete_lifecycle_hook::DeleteLifecycleHook,
        delete_notification_configuration::DeleteNotificationConfiguration,
        delete_policy::DeletePolicy,
        delete_scheduled_action::DeleteScheduledAction,
        delete_tags::DeleteTags,
        delete_warm_pool::DeleteWarmPool,
        describe_account_limits::DescribeAccountLimits,
        describe_account_settings::DescribeAccountSettings,
        describe_adjustment_types::DescribeAdjustmentTypes,
        describe_auto_scaling_instances::DescribeAutoScalingInstances,
        describe_auto_scaling_notification_types::DescribeAutoScalingNotificationTypes,
        describe_instance_refreshes::DescribeInstanceRefreshes,
        describe_launch_configurations::DescribeLaunchConfigurations,
        describe_lifecycle_hook_types::DescribeLifecycleHookTypes,
        describe_lifecycle_hooks::DescribeLifecycleHooks,
        describe_load_balancer_target_groups::DescribeLoadBalancerTargetGroups,
        describe_load_balancers::DescribeLoadBalancers,
        describe_metric_collection_types::DescribeMetricCollectionTypes,
        describe_notification_configurations::DescribeNotificationConfigurations,
        describe_scaling_activities::DescribeScalingActivities,
        describe_scaling_process_types::DescribeScalingProcessTypes,
        describe_scheduled_actions::DescribeScheduledActions,
        describe_tags::DescribeTags,
        describe_termination_policy_types::DescribeTerminationPolicyTypes,
        describe_traffic_sources::DescribeTrafficSources,
        describe_warm_pool::DescribeWarmPool,
        detach_instances::DetachInstances,
        detach_load_balancer_target_groups::DetachLoadBalancerTargetGroups,
        detach_load_balancers::DetachLoadBalancers,
        detach_traffic_sources::DetachTrafficSources,
        disable_metrics_collection::DisableMetricsCollection,
        enable_metrics_collection::EnableMetricsCollection,
        enter_standby::EnterStandby,
        execute_policy::ExecutePolicy,
        exit_standby::ExitStandby,
        launch_instances::LaunchInstances,
        put_account_setting::PutAccountSetting,
        put_lifecycle_hook::PutLifecycleHook,
        put_notification_configuration::PutNotificationConfiguration,
        put_scaling_policy::PutScalingPolicy,
        put_scheduled_update_group_action::PutScheduledUpdateGroupAction,
        put_warm_pool::PutWarmPool,
        record_lifecycle_action_heartbeat::RecordLifecycleActionHeartbeat,
        resume_processes::ResumeProcesses,
        rollback_instance_refresh::RollbackInstanceRefresh,
        set_desired_capacity::SetDesiredCapacity,
        set_instance_health::SetInstanceHealth,
        set_instance_protection::SetInstanceProtection,
        start_instance_refresh::StartInstanceRefresh,
        suspend_processes::SuspendProcesses,
        terminate_instance_in_auto_scaling_group::TerminateInstanceInAutoScalingGroup,
        update_auto_scaling_group::UpdateAutoScalingGroup,
    );
    quoting_refused_values_of_decimal_responses!(
        &mut table, engine,
        describe_auto_scaling_groups::DescribeAutoScalingGroups,
        describe_policies::DescribePolicies,
        get_predictive_scaling_forecast::GetPredictiveScalingForecast,
    );
    table
}

#[cfg(test)]
mod tests {
    use super::table;
    use std::sync::Arc;

    /// Every operation the service shape BINDS, read off this service's own Smithy model
    /// rather than written out here, so the enumeration above cannot drift from the model: an
    /// operation left out of it fails this test instead of silently falling back to the
    /// generated table.
    fn modelled_operations() -> Vec<String> {
        let path = ::std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("smithy-model/com-amazonaws-autoscaling-autoscaling-2011-01-01.json");
        let text = ::std::fs::read_to_string(&path).expect("this service's own Smithy model");
        let model: ::serde_json::Value =
            ::serde_json::from_str(&text).expect("a Smithy JSON AST model");
        let shapes = model["shapes"].as_object().expect("the model's shapes");
        let service = shapes
            .values()
            .find(|shape| shape["type"] == "service")
            .expect("the model's service shape");
        service["operations"]
            .as_array()
            .expect("the operations the service binds")
            .iter()
            .filter_map(|bound| bound["target"].as_str())
            // The routing key the protocol resolves is the operation's shape NAME, the last
            // segment of its shape id.
            .filter_map(|target| target.split('#').next_back())
            .map(::std::string::ToString::to_string)
            .collect()
    }

    /// Every operation this service models is filed under the key the protocol resolves —
    /// its modelled operation name — including the three that are also served behind the
    /// decimal renderer, and nothing else is.
    #[test]
    fn every_modelled_operation_is_served_behind_the_wrapper() {
        let table = table(&Arc::new(()));
        let modelled = modelled_operations();
        assert!(
            modelled.len() > 60,
            "expected the model to bind this service's whole surface, got {} operation(s)",
            modelled.len()
        );
        for name in &modelled {
            assert!(
                table.get(name).is_some(),
                "case {name:?}: expected the operation to be served behind the quoting wrapper"
            );
        }
        for name in ["Nonesuch", "DescribeAutoScalingGroup", "CreateTags"] {
            assert!(
                table.get(name).is_none(),
                "case {name:?}: expected no row for a name this service does not model"
            );
        }
    }
}
