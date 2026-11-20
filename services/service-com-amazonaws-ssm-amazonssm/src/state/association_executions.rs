// SPDX-License-Identifier: CC0-1.0

//! The EXECUTIONS of a State Manager association: what `DescribeAssociationExecutions`
//! reports for an association, and what `DescribeAssociationExecutionTargets` reports for
//! one execution of it.
//!
//! An execution IS a run of the association ([`AssociationRunRecord`]) — the id it was
//! dispatched under, the version it applied, the instant it started and the managed nodes it
//! was dispatched to — so what a create and `StartAssociationsOnce` persisted is what these
//! reads report, never a second copy of it. Everything a read adds to that is derived from
//! the stored instant: the execution's status ([`associations::ASSOCIATION_EXECUTION_DELAY`]),
//! the dates it reports and the count of its targets by status. Neither read writes
//! anything, so two callers see the same executions whether or not either of them called.
//!
//! A run that reached NO node is an execution all the same — State Manager dispatched the
//! association — but nothing reported a result for it: it has no target to report one, so it
//! reports no detail and the EMPTY count of targets by status, and its status stays the
//! `Pending` the association itself reports
//! ([`associations::AssociationRecord::overview`]). One rule holds
//! across both reads: a result is reported by the nodes a run reached, and a run that reached
//! none has no result.
//!
//! The TARGETS of an execution are the nodes it was dispatched to, each reported with the
//! kind the node store holds for that node, so a target reports what registering the node
//! persisted rather than a kind invented here. The members only the SSM Agent in the node's
//! guest reports — the location it uploaded the execution's details to — are unset, because
//! nothing has uploaded output for a run.

use std::time::SystemTime;

use crate::ops::types::{
    AssociationExecution, AssociationExecutionTarget, ASSOCIATION_EXECUTION_FILTER_KEY_VALUES,
    ASSOCIATION_EXECUTION_TARGETS_FILTER_KEY_VALUES,
};

use super::associations::{self, AssociationRecord, AssociationRunRecord};
use super::ComAmazonawsSsmAmazonssmState;

/// How this service states an execution id that names no execution of the association.
/// `AssociationExecutionDoesNotExist`'s own documentation is "The specified execution ID
/// doesn't exist. Verify the ID number and try again."
pub const ASSOCIATION_EXECUTION_DOES_NOT_EXIST: &str =
    "The specified execution ID doesn't exist. Verify the ID number and try again.";

/// The `AssociationExecutionFilterKey` that compares an execution's own id (the modelled
/// enum, `crate::ops::types::ASSOCIATION_EXECUTION_FILTER_KEY_VALUES`).
pub const FILTER_KEY_EXECUTION_ID: &str = "ExecutionId";

/// The filter key that compares an execution's status; also the
/// `AssociationExecutionTargetsFilterKey` that compares a target's (the modelled enums,
/// `crate::ops::types::ASSOCIATION_EXECUTION_TARGETS_FILTER_KEY_VALUES`).
pub const FILTER_KEY_STATUS: &str = "Status";

/// The filter key that compares when an execution started.
pub const FILTER_KEY_CREATED_TIME: &str = "CreatedTime";

/// The targets filter key that compares the resource a target is — "the managed node ID
/// where the association ran" (`AssociationExecutionTarget$ResourceId`).
pub const FILTER_KEY_RESOURCE_ID: &str = "ResourceId";

/// The targets filter key that compares the kind of resource a target is
/// (`AssociationExecutionTarget$ResourceType`).
pub const FILTER_KEY_RESOURCE_TYPE: &str = "ResourceType";

/// The `AssociationFilterOperatorType` that compares a key against the whole value a filter
/// carries (the modelled enum,
/// `crate::ops::types::ASSOCIATION_FILTER_OPERATOR_TYPE_VALUES`).
pub const OPERATOR_EQUAL: &str = "EQUAL";

/// The operator that admits what is ordered BEFORE the value a filter carries (same enum).
pub const OPERATOR_LESS_THAN: &str = "LESS_THAN";

/// The operator that admits what is ordered AFTER it (same enum).
pub const OPERATOR_GREATER_THAN: &str = "GREATER_THAN";

/// The operators an execution filter key accepts: "ExecutionId (EQUAL) Status (EQUAL)
/// CreatedTime (EQUAL, GREATER_THAN, LESS_THAN)"
/// (`DescribeAssociationExecutionsRequest$Filters`) — a time key orders, and the two keys
/// that name a value compare it whole.
fn operators_of(key: &str) -> &'static [&'static str] {
    match key {
        FILTER_KEY_CREATED_TIME => &[OPERATOR_EQUAL, OPERATOR_GREATER_THAN, OPERATOR_LESS_THAN],
        _ => &[OPERATOR_EQUAL],
    }
}

/// Why the execution filter `key`/`value` under `operator` narrows a report by nothing an
/// execution holds, or `None` when it names one.
///
/// A filter has to name a key the modelled `AssociationExecutionFilterKey` declares, carry
/// an operator that key accepts ([`operators_of`]) and — for the key that orders instants —
/// a value that names an instant at all ([`super::request_instant`]): anything else narrows
/// the report by nothing, so it is refused rather than served as if it had been applied. The
/// refusals are worded the way this service words a filter it does not accept
/// (`super::ops_item_related_items::filter_rejection`).
pub fn execution_filter_rejection(key: &str, value: &str, operator: &str) -> Option<String> {
    if !ASSOCIATION_EXECUTION_FILTER_KEY_VALUES.contains(&key) {
        return Some(super::invalid_filter_key_message(
            key,
            ASSOCIATION_EXECUTION_FILTER_KEY_VALUES,
        ));
    }
    let accepted = operators_of(key);
    if !accepted.contains(&operator) {
        return Some(format!(
            "The filter operator {operator} isn't supported for the filter key {key}. The \
             supported filter operators are {}",
            accepted.join(", ")
        ));
    }
    if key == FILTER_KEY_CREATED_TIME && super::request_instant(value).is_none() {
        return Some(format!(
            "The filter value {value} isn't a valid time for the filter key {key}"
        ));
    }
    None
}

/// Why the targets filter `key` narrows a report by nothing a target holds, or `None` when
/// it names one.
///
/// An `AssociationExecutionTargetsFilter` carries no operator at all, so every one of its
/// keys compares whole values: "Status (EQUAL) ResourceId (EQUAL) ResourceType (EQUAL)"
/// (`DescribeAssociationExecutionTargetsRequest$Filters`).
pub fn target_filter_rejection(key: &str) -> Option<String> {
    (!ASSOCIATION_EXECUTION_TARGETS_FILTER_KEY_VALUES.contains(&key)).then(|| {
        super::invalid_filter_key_message(key, ASSOCIATION_EXECUTION_TARGETS_FILTER_KEY_VALUES)
    })
}

impl AssociationRunRecord {
    /// "An aggregate status of the resources in the execution based on the status type"
    /// (`AssociationExecution$ResourceCountByStatus`): how many of the nodes this execution
    /// was dispatched to are in which status, which is every one of them in the status the
    /// execution itself has reached ([`AssociationRunRecord::status`]), rendered as the one
    /// entry `{Status=count}` the member carries it as.
    ///
    /// A run that reached NO node reports the EMPTY aggregate, `{}`: the member is a count
    /// over the resources an execution reached, and a count over none is empty rather than
    /// absent — the execution happened, and State Manager had nothing to report a status for.
    /// That is the form a client reads to tell "this execution counted nothing" from "this
    /// execution counted something", so it is reported for every execution.
    pub fn resource_count_by_status(&self, now: SystemTime) -> String {
        if self.nodes.is_empty() {
            return "{}".to_string();
        }
        format!("{{{}={}}}", self.status(now), self.nodes.len())
    }

    /// Whether the execution filter `key`/`value` under `operator` admits this execution as
    /// of `now`.
    ///
    /// One filter compares ONE attribute of the execution against the whole value it
    /// carries, under the operator it names: the id it was dispatched under, the status it
    /// has reached or the instant it started, which the two ordering operators compare as
    /// instants rather than as text. A key this service does not know admits nothing, which
    /// is a case a caller cannot reach because such a key is refused before any execution is
    /// read ([`execution_filter_rejection`]).
    pub fn matches_execution_filter(
        &self,
        key: &str,
        value: &str,
        operator: &str,
        now: SystemTime,
    ) -> bool {
        match key {
            FILTER_KEY_EXECUTION_ID => self.execution_id == value,
            FILTER_KEY_STATUS => self.status(now) == value,
            FILTER_KEY_CREATED_TIME => super::request_instant(value).is_some_and(|wanted| {
                match operator {
                    OPERATOR_LESS_THAN => self.started_at < wanted,
                    OPERATOR_GREATER_THAN => self.started_at > wanted,
                    // The instant an execution started is held to whatever precision the
                    // dispatch read the clock at, and a filter names one to the second, so
                    // equality is equality of the second they fall in — the precision the
                    // member is rendered at on the wire.
                    _ => second_of(self.started_at) == second_of(wanted),
                }
            }),
            _ => false,
        }
    }

    /// The modelled `AssociationExecution` of this run, as of `now`.
    ///
    /// Every member is what the dispatch persisted — the association it ran, the id it was
    /// dispatched under, the version it applied and the instant it started — or is derived
    /// from that instant: the status it has reached, the date it finished at and the count of
    /// its targets by status. A run that reached no node reports no detail and no count,
    /// because a result is reported by the nodes a run reached. `TriggeredAlarms` is unset
    /// because no alarm has been invoked for a run, the reading the association's own
    /// description already takes (`AssociationRecord::to_association_description`).
    pub fn to_association_execution(
        &self,
        association: &AssociationRecord,
        now: SystemTime,
    ) -> AssociationExecution {
        AssociationExecution {
            association_id: Some(association.association_id.clone()),
            association_version: Some(self.association_version.to_string()),
            execution_id: Some(self.execution_id.clone()),
            status: Some(self.status(now).to_string()),
            detailed_status: (!self.nodes.is_empty())
                .then(|| self.detailed_status(now).to_string()),
            created_time: Some(::aws_smithy_types::DateTime::from(self.started_at)),
            // "The date of the last execution": this execution ran once, so the date it has
            // is the instant it finished at — and a run that has not finished has none.
            last_execution_date: self
                .has_finished(now)
                .then(|| ::aws_smithy_types::DateTime::from(self.finished_at())),
            resource_count_by_status: Some(self.resource_count_by_status(now)),
            // The alarms this execution was dispatched to watch are the ones the version it
            // applied carries, and none has been invoked.
            alarm_configuration: association
                .version_numbered(self.association_version)
                .and_then(|version| version.alarm_configuration.clone()),
            triggered_alarms: None,
        }
    }

    /// The modelled `AssociationExecutionTarget` of ONE node this run was dispatched to, as
    /// of `now`.
    ///
    /// The identity members are the execution's own, so a target and the execution it
    /// belongs to never disagree about which association version ran. `ResourceType` is the
    /// kind the node store holds for this node, `None` for a node that is no longer
    /// registered — nothing here invents a kind for a node the account does not hold. The
    /// status is the execution's, because a run reaches every node it was dispatched to at
    /// once, and `OutputSource` is unset because nothing has uploaded this execution's
    /// details anywhere.
    pub fn to_association_execution_target(
        &self,
        association: &AssociationRecord,
        resource_id: &str,
        resource_type: Option<&str>,
        now: SystemTime,
    ) -> AssociationExecutionTarget {
        AssociationExecutionTarget {
            association_id: Some(association.association_id.clone()),
            association_version: Some(self.association_version.to_string()),
            execution_id: Some(self.execution_id.clone()),
            resource_id: Some(resource_id.to_string()),
            resource_type: resource_type.map(str::to_string),
            status: Some(self.status(now).to_string()),
            detailed_status: Some(self.detailed_status(now).to_string()),
            last_execution_date: self
                .has_finished(now)
                .then(|| ::aws_smithy_types::DateTime::from(self.finished_at())),
            output_source: None,
        }
    }

    /// Whether the targets filter `key`/`value` admits the target of this execution on the
    /// node `resource_id`, which is of kind `resource_type`, as of `now`.
    ///
    /// The keys are the modelled `AssociationExecutionTargetsFilterKey` enum and every one
    /// of them compares whole values ([`target_filter_rejection`]); a key naming an
    /// attribute the target does not carry admits it under no value.
    pub fn matches_target_filter(
        &self,
        key: &str,
        value: &str,
        resource_id: &str,
        resource_type: Option<&str>,
        now: SystemTime,
    ) -> bool {
        match key {
            FILTER_KEY_STATUS => self.status(now) == value,
            FILTER_KEY_RESOURCE_ID => resource_id == value,
            FILTER_KEY_RESOURCE_TYPE => resource_type == Some(value),
            _ => false,
        }
    }
}

/// `at` truncated to the second it falls in, which is the precision a timestamp is compared
/// at ([`AssociationRunRecord::matches_execution_filter`]).
fn second_of(at: SystemTime) -> i64 {
    ::aws_smithy_types::DateTime::from(at).secs()
}

impl AssociationRecord {
    /// The version of this association numbered `version`, or `None` when it holds no such
    /// version — an association keeps at most [`associations::MAX_ASSOCIATION_VERSIONS`] of
    /// them, so the version an older execution applied may be gone.
    pub fn version_numbered(
        &self,
        version: u64,
    ) -> Option<&associations::AssociationVersionRecord> {
        self.versions.iter().find(|held| held.version == version)
    }

    /// The executions of this association, MOST RECENT FIRST: "Views all executions for a
    /// specific association ID" (`DescribeAssociationExecutions`), which is every run State
    /// Manager has dispatched it in — including one whose targets selected no registered
    /// node, which State Manager dispatched all the same and which simply reached nothing.
    ///
    /// The order is the reverse of the order the runs were dispatched in, with the execution
    /// id breaking a tie between two dispatched at the one instant, so two reads of one
    /// association report them in the same order.
    pub fn reported_executions(&self) -> Vec<&AssociationRunRecord> {
        let mut reported: Vec<&AssociationRunRecord> = self.executions.iter().collect();
        reported.sort_by(|left, right| {
            right
                .started_at
                .cmp(&left.started_at)
                .then_with(|| left.execution_id.cmp(&right.execution_id))
        });
        reported
    }

    /// The execution of this association that `execution_id` names, or `None` when it has
    /// none — which is what `AssociationExecutionDoesNotExist` reports.
    pub fn execution(&self, execution_id: &str) -> Option<&AssociationRunRecord> {
        self.executions
            .iter()
            .find(|run| run.execution_id == execution_id)
    }
}

impl ComAmazonawsSsmAmazonssmState {
    /// The kind of resource the node `instance_id` of `region` is, or `None` when that
    /// Region holds no such node or holds no kind for it: a target reports the
    /// `ResourceType` registering the node persisted
    /// (`super::InstanceProperty::resource_type`).
    pub fn node_resource_type(&self, region: &str, instance_id: &str) -> Option<&str> {
        self.managed_node(region, instance_id)
            .and_then(|node| node.properties.resource_type.as_deref())
    }
}
