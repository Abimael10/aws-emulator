// SPDX-License-Identifier: CC0-1.0

//! The DRIFT VIEWS a read reports: one stack's, and one resource's, drift result as the
//! modelled structures that carry it.
//!
//! Every one of them is a projection of the stored drift record ([`super::drift`]) onto the
//! shape the operation that reports it answers with — the stack's own
//! `DriftInformation`/`DriftInformationSummary`, a resource's, and the whole
//! `StackResourceDrift` a detection's findings are read back as — so what a read answers and
//! what the store holds cannot disagree.

use super::*;

/// A stack's drift result as the modelled `Stack$DriftInformation`.
///
/// CloudFormation reports `DriftInformation` on every stack — a stack nothing has checked
/// reports `NOT_CHECKED`, which is a result in its own right — so this is always present.
/// `LastCheckTimestamp` is the instant the most recent detection on the stack was
/// INITIATED, and is absent exactly while no detection has been.
pub fn stack_drift_information(
    stack: &StoredStack,
) -> Option<crate::ops::types::StackDriftInformation> {
    Some(crate::ops::types::StackDriftInformation {
        stack_drift_status: stack.drift_status.clone(),
        last_check_timestamp: stack.drift_last_check.map(timestamp),
    })
}

/// The same result as `StackSummary$DriftInformation`, the SUMMARY spelling of the same
/// two members — so `ListStacks` and `DescribeStacks` cannot disagree about one stack's
/// drift result.
pub fn stack_drift_information_summary(
    stack: &StoredStack,
) -> Option<crate::ops::types::StackDriftInformationSummary> {
    Some(crate::ops::types::StackDriftInformationSummary {
        stack_drift_status: stack.drift_status.clone(),
        last_check_timestamp: stack.drift_last_check.map(timestamp),
    })
}

/// One resource's drift result as the modelled `StackResource$DriftInformation` /
/// `StackResourceDetail$DriftInformation`, reported on every resource for the same reason.
pub fn resource_drift_information(
    deployed: &DeployedResource,
) -> Option<crate::ops::types::StackResourceDriftInformation> {
    Some(crate::ops::types::StackResourceDriftInformation {
        stack_resource_drift_status: deployed.drift_status.clone(),
        last_check_timestamp: deployed.drift_checked_at.map(timestamp),
    })
}

/// The same result as `StackResourceSummary$DriftInformation`, the SUMMARY spelling of the
/// same two members.
pub fn resource_drift_information_summary(
    deployed: &DeployedResource,
) -> Option<crate::ops::types::StackResourceDriftInformationSummary> {
    Some(crate::ops::types::StackResourceDriftInformationSummary {
        stack_resource_drift_status: deployed.drift_status.clone(),
        last_check_timestamp: deployed.drift_checked_at.map(timestamp),
    })
}

/// One resource's drift result as the modelled `StackResourceDrift` — the shape
/// `DescribeStackResourceDrifts` and `DetectStackResourceDrift` both report a checked
/// resource through.
///
/// `None` for a resource no drift check has been carried out over: "resources that haven't
/// yet been checked for drift aren't included", and neither are the ones "that don't
/// currently support drift detection", which are exactly the resources
/// [`DeployedResource::drift_detail`] is absent for.
///
/// `Timestamp` is "time at which CloudFormation performed drift detection on the stack
/// resource" — the instant of the check this record came from. The two property structures
/// and the differences are the comparison that check carried out, so what is reported is
/// what was compared rather than a comparison recomputed on a read; `PropertyDifferences`
/// is present only where there are differences, which is "only for resources whose
/// StackResourceDriftStatus is MODIFIED".
///
/// `PhysicalResourceIdContext` and `ModuleInfo` are absent: context pairs identify a
/// resource "in cases where a resource's logical and physical IDs aren't enough", which the
/// one physical id per logical resource this emulator records always is, and module
/// information belongs to a resource "created from a module included in the stack
/// template", which nothing here deploys.
pub fn stack_resource_drift_view(
    stack_id: &str,
    deployed: &DeployedResource,
) -> Option<crate::ops::types::StackResourceDrift> {
    deployed.drift_detail.as_ref()?;
    let checked_at = deployed.drift_checked_at?;
    Some(resource_drift_record(stack_id, deployed, checked_at))
}

/// The same record for a resource a check was just carried out over, WHATEVER that check
/// found — including a resource of a type no configuration is read for here, which carries
/// the `NOT_CHECKED` status and so neither property structure and no differences
/// ([`resource_drift_status::NOT_CHECKED`]: "any resources that do not currently support
/// drift detection have a status of NOT_CHECKED").
///
/// It is the same construction [`stack_resource_drift_view`] reports, held in one place so
/// the operation that checks ONE resource and the read that lists a stack's checked
/// resources cannot report the same comparison differently. `checked_at` is the instant of
/// the check being reported.
pub fn resource_drift_record(
    stack_id: &str,
    deployed: &DeployedResource,
    checked_at: SystemTime,
) -> crate::ops::types::StackResourceDrift {
    let detail = deployed.drift_detail.clone().unwrap_or_default();
    crate::ops::types::StackResourceDrift {
        stack_id: stack_id.to_string(),
        logical_resource_id: deployed.resource.logical_id.clone(),
        physical_resource_id: physical_resource_id(&deployed.resource),
        physical_resource_id_context: None,
        resource_type: deployed.resource.resource_type.clone(),
        expected_properties: detail.expected_properties,
        actual_properties: detail.actual_properties,
        property_differences: non_empty(
            detail
                .differences
                .into_iter()
                .map(|difference| crate::ops::types::PropertyDifference {
                    property_path: difference.property_path,
                    expected_value: difference.expected_value,
                    actual_value: difference.actual_value,
                    difference_type: difference.difference_type,
                })
                .collect(),
        ),
        stack_resource_drift_status: deployed.drift_status.clone(),
        timestamp: timestamp(checked_at),
        module_info: None,
        drift_status_reason: detail.reason,
    }
}
