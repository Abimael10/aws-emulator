// SPDX-License-Identifier: CC0-1.0

//! What a started remediation execution RUNS: the Systems Manager automation document the
//! rule's remediation configuration names, against the resource the execution was started
//! for.
//!
//! A remediation target is an automation document ("the target ID is the name of the SSM
//! document", `RemediationConfiguration$TargetId`), and automation documents are owned and
//! RUN by Systems Manager — so what a remediation execution does is ask that service to
//! start an execution of the document, with the configuration's parameters resolved for the
//! resource ([`resolved_remediation_inputs`]). Nothing here performs a document's steps:
//! the run exists where its own service holds it, which is also the only place a client can
//! read it back.
//!
//! Systems Manager is reached the way this crate reaches every other service — the AWS SDK
//! driven over the in-process callback
//! ([`smithy_server_runtime::OperationContext::callback`]) — so this crate names no other
//! service crate and nothing leaves this process.
//!
//! The execution then REPORTS that automation: Config's own
//! `RemediationExecutionState` is what the automation's `AutomationExecutionStatus` maps
//! to ([`remediation_state_of_automation`]), learned by reading the automation back
//! ([`follow_remediation_automations`]) at every instant such a read is owed. Three
//! outcomes, because the start has three: Systems Manager STARTED the run, it REFUSED to
//! (no document of that name, a document that is not a runbook, parameters it does not
//! take), or it could not be asked at all — and only the first two say anything about the
//! run, so the third leaves the execution reporting the moves the clock makes due.

use std::collections::{BTreeMap, BTreeSet};
use std::time::SystemTime;

use ::aws_sdk_ssm::error::ProvideErrorMetadata as _;

use super::remediation_target::ssm_client;
use super::{
    service_state, ComAmazonawsConfigserviceStarlingdoveserviceState, ComplianceChange,
    RemediationConfigurationEntry, RemediationExecutionEntry, RemediationExecutionKey,
    COMPLIANCE_TYPE_NON_COMPLIANT, REMEDIATION_EXECUTION_STATE_FAILED,
    REMEDIATION_EXECUTION_STATE_IN_PROGRESS, REMEDIATION_EXECUTION_STATE_QUEUED,
    REMEDIATION_EXECUTION_STATE_SUCCEEDED,
};

/// The `ResourceValue` that resolves to the resource the execution is about
/// (`com.amazonaws.configservice#ResourceValueType`, whose one value this is).
const RESOURCE_VALUE_RESOURCE_ID: &str = "RESOURCE_ID";

/// The most automation executions ONE pass reads back.
///
/// A HOST bound: the store keeps at most
/// [`super::REMEDIATION_EXECUTIONS_KEPT`] executions, each of which is one in-process read,
/// so a single pass cannot turn into an unbounded run of calls.
const MAX_FOLLOWED_AUTOMATIONS: usize = 25;

/// The `RemediationExecutionState` a Config remediation execution is at while the Systems
/// Manager automation execution that RUNS it reports `status`.
///
/// `AutomationExecutionStatus` (`com.amazonaws.ssm#AutomationExecutionStatus`) is what an
/// automation reports; the value set is the one that enum declares. An automation nothing
/// has picked up yet, or that is waiting to be let through, has not started running, which
/// is what `QUEUED` says; one that ended well succeeded; one that ended any other way
/// failed; and everything else is still running.
pub fn remediation_state_of_automation(status: &str) -> &'static str {
    match status {
        "Pending" | "Scheduled" | "PendingApproval" | "PendingChangeCalendarOverride" => {
            REMEDIATION_EXECUTION_STATE_QUEUED
        }
        "Success" | "CompletedWithSuccess" => REMEDIATION_EXECUTION_STATE_SUCCEEDED,
        "Failed" | "TimedOut" | "Cancelled" | "Rejected" | "CompletedWithFailure" | "Exited"
        | "ChangeCalendarOverrideRejected" => REMEDIATION_EXECUTION_STATE_FAILED,
        _ => REMEDIATION_EXECUTION_STATE_IN_PROGRESS,
    }
}

/// The RUN of one remediation execution: the Systems Manager automation execution Config
/// asked for, and what Systems Manager has said about it.
#[derive(Debug, Clone)]
pub struct RemediationAutomation {
    /// The automation execution Systems Manager started
    /// (`StartAutomationExecutionResult$AutomationExecutionId`), absent for a start it
    /// refused — there is then no execution to read back.
    pub execution_id: Option<String>,
    /// The `RemediationExecutionState` this run is at: what the automation's own status
    /// maps to ([`remediation_state_of_automation`]), and `FAILED` for a start Systems
    /// Manager refused, because the run that remediation needed will not happen.
    pub state: &'static str,
    /// When Config asked for the run.
    pub started_at: SystemTime,
    /// When the run ENDED, absent while it has not.
    pub ended_at: Option<SystemTime>,
    /// Why the run failed, as the service that owns it stated it; absent for a run that has
    /// not failed.
    pub failure_message: Option<String>,
}

impl RemediationAutomation {
    /// The run Systems Manager started, as of `at`.
    fn started(execution_id: Option<String>, at: SystemTime) -> Self {
        Self {
            execution_id,
            // A started automation has been accepted and not yet picked up, which is the
            // state its own `Pending` maps to.
            state: REMEDIATION_EXECUTION_STATE_QUEUED,
            started_at: at,
            ended_at: None,
            failure_message: None,
        }
    }

    /// The run Systems Manager REFUSED to start, with the reason it gave.
    fn refused(why: Option<String>, at: SystemTime) -> Self {
        Self {
            execution_id: None,
            state: REMEDIATION_EXECUTION_STATE_FAILED,
            started_at: at,
            ended_at: Some(at),
            failure_message: why,
        }
    }

    /// Whether this run has ENDED, either way — so there is nothing left to read back.
    pub fn is_settled(&self) -> bool {
        matches!(
            self.state,
            REMEDIATION_EXECUTION_STATE_SUCCEEDED | REMEDIATION_EXECUTION_STATE_FAILED
        )
    }
}

/// One run a started remediation execution owes: what to run, and against which resource.
#[derive(Debug, Clone)]
pub struct RemediationAction {
    /// The rule whose remediation this runs, which is what its execution is recorded
    /// under.
    pub config_rule_name: String,
    /// The type of the resource the execution names (`ResourceKey$resourceType`).
    pub resource_type: String,
    /// The id of the resource the execution names (`ResourceKey$resourceId`).
    pub resource_id: String,
    /// The automation document the rule's remediation configuration names
    /// (`RemediationConfiguration$TargetId`).
    pub target_id: String,
    /// The version of that document the configuration names
    /// (`RemediationConfiguration$TargetVersion`); a configuration naming none runs the
    /// document's own default version.
    pub target_version: Option<String>,
    /// The configuration's parameters, each resolved to the ONE value the document runs
    /// with; see [`resolved_remediation_inputs`].
    pub inputs: BTreeMap<String, String>,
}

impl RemediationAction {
    /// The key the execution this action belongs to is stored under.
    fn key(&self) -> RemediationExecutionKey {
        (
            self.config_rule_name.clone(),
            self.resource_type.clone(),
            self.resource_id.clone(),
        )
    }
}

/// The remediation parameters of `remediation`, each resolved to the one value the
/// document runs with for the resource `resource_id`.
///
/// A `RemediationParameterValue` "is either a dynamic (resource) value or a static
/// value": a static value contributes the value it carries, and a resource value
/// contributes the resource the execution is about, which is what `RESOURCE_ID` names. A
/// parameter this emulator cannot resolve contributes nothing, so the document falls back
/// to its own declared default for it.
pub fn resolved_remediation_inputs(
    remediation: &RemediationConfigurationEntry,
    resource_id: &str,
) -> BTreeMap<String, String> {
    remediation
        .parameters
        .iter()
        .filter_map(|(name, value)| {
            let resolved = match (&value.static_value, &value.resource_value) {
                (Some(static_value), _) => static_value.values.first().cloned(),
                (None, Some(resource_value))
                    if resource_value.value.trim() == RESOURCE_VALUE_RESOURCE_ID =>
                {
                    Some(resource_id.to_string())
                }
                _ => None,
            };
            Some((name.clone(), resolved?))
        })
        .collect()
}

impl ComAmazonawsConfigserviceStarlingdoveserviceState {
    /// The remediation runs Config owes for the resources its rules currently find
    /// NON_COMPLIANT, recording an execution for each of them as of `now`; `changes` are
    /// the verdicts the pass that calls this just moved.
    ///
    /// AUTOMATIC remediation needs no call from the caller: a configuration whose
    /// `Automatic` is set is the account's standing instruction to remediate what its
    /// rule finds, so what it acts on is a FINDING — a verdict that has just placed the
    /// resource at non-compliant, or one no execution of that rule has acted on for the
    /// resource yet, which is what makes a configuration attached after the rule already
    /// found the resource act on it. A finding already acted on is not acted on again, so
    /// a resource is remediated once per finding rather than once per pass, and a rule
    /// whose configuration remediates only on request
    /// (`RemediationConfiguration$Automatic`) is left to `StartRemediationExecution`.
    ///
    /// A remediation exception is what exempts one resource from being remediated under
    /// one rule, so a resource an unexpired exception covers is skipped.
    ///
    /// The runs themselves are asked for with the store's lock RELEASED
    /// ([`run_remediation_actions`]), exactly as a started execution's are.
    pub fn automatic_remediations(
        &mut self,
        changes: &[ComplianceChange],
        now: SystemTime,
    ) -> Vec<RemediationAction> {
        // The findings this pass produced, as the keys an execution is recorded under.
        let moved_to_non_compliant: BTreeSet<RemediationExecutionKey> = changes
            .iter()
            .filter(|change| change.new_result.compliance_type == COMPLIANCE_TYPE_NON_COMPLIANT)
            .map(|change| {
                (
                    change.rule_name.clone(),
                    change.new_result.resource_type.clone(),
                    change.new_result.resource_id.clone(),
                )
            })
            .collect();

        // What is owed is decided from the state this pass arrived at, before any
        // execution is recorded, so a resource cannot be chosen twice.
        let mut owed: Vec<RemediationExecutionKey> = Vec::new();
        for (rule_name, remediation) in &self.remediation_configurations {
            if !remediation.automatic {
                continue;
            }
            let Some(rule) = self.config_rules.get(rule_name) else {
                continue;
            };
            for result in rule.evaluation_results.values() {
                if result.compliance_type != COMPLIANCE_TYPE_NON_COMPLIANT {
                    continue;
                }
                let key = (
                    rule_name.clone(),
                    result.resource_type.clone(),
                    result.resource_id.clone(),
                );
                if !moved_to_non_compliant.contains(&key)
                    && self.remediation_executions.contains_key(&key)
                {
                    continue;
                }
                if self
                    .remediation_exceptions
                    .get(&key)
                    .is_some_and(|exception| !exception.expired_by(now))
                {
                    continue;
                }
                owed.push(key);
            }
        }

        let mut actions = Vec::new();
        for (rule_name, resource_type, resource_id) in owed {
            let Some(remediation) = self.remediation_configurations.get(&rule_name).cloned() else {
                continue;
            };
            self.record_remediation_execution(
                &rule_name,
                RemediationExecutionEntry::invoked(
                    &resource_type,
                    &resource_id,
                    &remediation.target_id,
                    now,
                ),
            );
            actions.push(RemediationAction {
                config_rule_name: rule_name,
                resource_type,
                target_id: remediation.target_id.clone(),
                target_version: remediation.target_version.clone(),
                inputs: resolved_remediation_inputs(&remediation, &resource_id),
                resource_id,
            });
        }
        actions
    }

    /// Note the run `automation` as the run of the execution `action` belongs to.
    ///
    /// This is what makes the reported state follow the RUN rather than the clock; an
    /// execution the store no longer holds — one dropped past the bound it keeps, or
    /// replaced by a later start for the same resource — keeps whatever its own record
    /// says.
    pub fn note_remediation_automation(
        &mut self,
        action: &RemediationAction,
        automation: RemediationAutomation,
    ) {
        if let Some(execution) = self.remediation_executions.get_mut(&action.key()) {
            execution.automation = Some(automation);
        }
    }

    /// The next window of automation executions this account's remediation executions are
    /// still waiting on: the key of each execution and the automation it runs in.
    ///
    /// One call answers at most [`MAX_FOLLOWED_AUTOMATIONS`] of them and ROTATES: it
    /// resumes after the execution the last call stopped at and wraps round, so every run
    /// in flight is read back within a few calls however many there are — a window that
    /// always started at the same end would leave a run past it unread for as long as
    /// anything inside it stayed in flight.
    pub fn unsettled_remediation_automations(&mut self) -> Vec<(RemediationExecutionKey, String)> {
        let unsettled: Vec<(RemediationExecutionKey, String)> = self
            .remediation_executions
            .iter()
            .filter_map(|(key, execution)| {
                let automation = execution.automation.as_ref()?;
                if automation.is_settled() {
                    return None;
                }
                Some((key.clone(), automation.execution_id.clone()?))
            })
            .collect();
        if unsettled.is_empty() {
            self.followed_automations_resume_at = None;
            return unsettled;
        }
        // Where the last call stopped: the first execution ordered after it, and the start
        // of the list once nothing is left after it.
        let resume_at = self
            .followed_automations_resume_at
            .as_ref()
            .and_then(|stopped_at| unsettled.iter().position(|(key, _)| key > stopped_at))
            .unwrap_or(0);
        let window: Vec<(RemediationExecutionKey, String)> = unsettled
            .iter()
            .cycle()
            .skip(resume_at)
            .take(MAX_FOLLOWED_AUTOMATIONS.min(unsettled.len()))
            .cloned()
            .collect();
        self.followed_automations_resume_at = window.last().map(|(key, _)| key.clone());
        window
    }

    /// Note what the automation of the execution `key` has reached: the state its own
    /// status maps to, when it ended and why it failed.
    ///
    /// A run that has ENDED without the service stating when keeps `now` as that instant,
    /// because the end is what was just learned.
    pub fn note_remediation_automation_progress(
        &mut self,
        key: &RemediationExecutionKey,
        state: &'static str,
        ended_at: Option<SystemTime>,
        failure_message: Option<String>,
        now: SystemTime,
    ) {
        let Some(automation) = self
            .remediation_executions
            .get_mut(key)
            .and_then(|execution| execution.automation.as_mut())
        else {
            return;
        };
        automation.state = state;
        automation.failure_message = failure_message;
        if automation.is_settled() {
            automation.ended_at = Some(ended_at.unwrap_or(now));
        }
    }
}

/// Ask Systems Manager to run the document every action of `actions` names, noting on each
/// execution the run it belongs to.
///
/// Called by `StartRemediationExecution` and by the recording pass with the store's lock
/// RELEASED — no lock is held across an await: the guard is taken again only to note each
/// run.
pub async fn run_remediation_actions<Eng: Send + Sync + 'static>(
    callback: &smithy_server_runtime::EmbeddedEmulator,
    engine: &Eng,
    region: &str,
    actions: &[RemediationAction],
) {
    let Some(state) = service_state(engine) else {
        return;
    };
    for action in actions {
        // Nothing is noted for a start Systems Manager could not be asked for at all:
        // Config cannot conclude anything about a run from an answer it did not get, so
        // the execution keeps reporting the moves the clock makes due.
        if let Some(automation) = start_remediation_automation(callback, region, action).await {
            state.lock().note_remediation_automation(action, automation);
        }
    }
}

/// Ask Systems Manager to start an execution of the document `action` names, and answer
/// what that start came to — `None` where Systems Manager could not be asked at all.
async fn start_remediation_automation(
    callback: &smithy_server_runtime::EmbeddedEmulator,
    region: &str,
    action: &RemediationAction,
) -> Option<RemediationAutomation> {
    let client = ssm_client(callback, region);
    let mut start = client
        .start_automation_execution()
        .document_name(&action.target_id);
    // "The version of the SSM document to use for remediation": a configuration naming
    // none leaves the document's own default version to be used, which is what Systems
    // Manager does for a start that names no version.
    if let Some(version) = action
        .target_version
        .as_deref()
        .map(str::trim)
        .filter(|version| !version.is_empty())
    {
        start = start.document_version(version);
    }
    // "A key-value map of execution parameters, which match the declared parameters in the
    // Automation runbook": one value per parameter, which is what each of the
    // configuration's parameters resolved to.
    for (name, value) in &action.inputs {
        start = start.parameters(name, vec![value.clone()]);
    }
    let asked_at = SystemTime::now();
    match start.send().await {
        Ok(answer) => Some(RemediationAutomation::started(
            answer.automation_execution_id,
            asked_at,
        )),
        Err(error) => {
            // Systems Manager ANSWERING that it will not run the document is the end of
            // this run — the service that owns automation documents has refused it — so the
            // execution reports that failure with the reason it gave. Those answers are the
            // CLIENT errors the operation models: every other outcome, a server-side
            // failure and an answer this client cannot place included, says nothing about
            // the run at all, and Config cannot conclude a run failed from an answer it did
            // not get.
            if !refused_the_start(&error) {
                ::tracing::warn!(
                    config_rule_name = %action.config_rule_name,
                    target_id = %action.target_id,
                    error = %::aws_smithy_types::error::display::DisplayErrorContext(&error),
                    "Config could not ask Systems Manager to run this remediation target"
                );
                return None;
            }
            Some(RemediationAutomation::refused(
                refusal_message(error.code(), error.message()),
                asked_at,
            ))
        }
    }
}

/// Whether `error` is Systems Manager REFUSING to run the document, rather than an outcome
/// that says nothing about the run.
///
/// The refusals are the client errors `StartAutomationExecution` models — no runbook of
/// that name, no such version of it, the concurrent-execution limit, a mismatched
/// idempotency token, parameters the runbook does not take, and a target that resolves to
/// nothing — each of which means this remediation's run will not happen.
fn refused_the_start(
    error: &::aws_sdk_ssm::error::SdkError<
        ::aws_sdk_ssm::operation::start_automation_execution::StartAutomationExecutionError,
        ::aws_smithy_runtime_api::http::Response,
    >,
) -> bool {
    use ::aws_sdk_ssm::operation::start_automation_execution::StartAutomationExecutionError as Refusal;
    matches!(
        error.as_service_error(),
        Some(
            Refusal::AutomationDefinitionNotFoundException(_)
                | Refusal::AutomationDefinitionVersionNotFoundException(_)
                | Refusal::AutomationExecutionLimitExceededException(_)
                | Refusal::IdempotentParameterMismatch(_)
                | Refusal::InvalidAutomationExecutionParametersException(_)
                | Refusal::InvalidTarget(_)
        )
    )
}

/// How the step of a remediation execution reports a run Systems Manager refused to start:
/// what that service answered, carried through unmodified.
///
/// `ErrorMessage` is "an error message if the step was interrupted during execution"
/// (`RemediationExecutionStep$ErrorMessage`), and the one thing known about this
/// interruption is the refusal of the service that owns the document — so its own code and
/// sentence are what the step carries, and an answer that carried neither leaves the step
/// with no message rather than one worded here. WHICH document could not be run is the
/// step's own `Name`.
fn refusal_message(code: Option<&str>, message: Option<&str>) -> Option<String> {
    match (code, message) {
        (Some(code), Some(message)) => Some(format!("{code}: {message}")),
        (Some(code), None) => Some(code.to_string()),
        (None, Some(message)) => Some(message.to_string()),
        (None, None) => None,
    }
}

/// Read back the automation execution of every remediation run that has not ended yet, and
/// note what Systems Manager says each has reached.
///
/// What a run has got to is owed at the same instant whether or not anybody called, and
/// only a call to the service that owns the run can learn it — so this is driven by the
/// recording pass and by the read that reports those executions, with no lock held across
/// an await.
pub async fn follow_remediation_automations<Eng: Send + Sync + 'static>(
    callback: &smithy_server_runtime::EmbeddedEmulator,
    engine: &Eng,
    region: &str,
) {
    let Some(state) = service_state(engine) else {
        return;
    };
    let unsettled = state.lock().unsettled_remediation_automations();
    if unsettled.is_empty() {
        return;
    }
    let client = ssm_client(callback, region);
    for (key, execution_id) in unsettled {
        let answered = client
            .get_automation_execution()
            .automation_execution_id(&execution_id)
            .send()
            .await;
        let Ok(answer) = answered else {
            // Nothing was learned about this run, so nothing about it changes.
            continue;
        };
        let Some(automation) = answer.automation_execution else {
            continue;
        };
        let Some(status) = automation
            .automation_execution_status
            .as_ref()
            .map(|status| status.as_str().to_string())
        else {
            continue;
        };
        let ended_at = automation
            .execution_end_time
            .and_then(|instant| SystemTime::try_from(instant).ok());
        state.lock().note_remediation_automation_progress(
            &key,
            remediation_state_of_automation(&status),
            ended_at,
            automation.failure_message.clone(),
            SystemTime::now(),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ops::types::{
        RemediationConfiguration, RemediationParameterValue, ResourceValue, StaticValue,
    };

    /// A remediation configuration carrying `parameters`.
    fn configuration(
        parameters: Vec<(&str, RemediationParameterValue)>,
    ) -> RemediationConfigurationEntry {
        RemediationConfigurationEntry::new(
            "id".to_string(),
            "arn".to_string(),
            &RemediationConfiguration {
                config_rule_name: "rule".to_string(),
                target_type: "SSM_DOCUMENT".to_string(),
                target_id: "AWS-ConfigureS3BucketVersioning".to_string(),
                parameters: Some(
                    parameters
                        .into_iter()
                        .map(|(name, value)| (name.to_string(), value))
                        .collect(),
                ),
                ..RemediationConfiguration::default()
            },
        )
    }

    /// A static parameter value carrying `value`.
    fn static_value(value: &str) -> RemediationParameterValue {
        RemediationParameterValue {
            static_value: Some(StaticValue {
                values: vec![value.to_string()],
            }),
            resource_value: None,
        }
    }

    /// A resource parameter value naming `value`.
    fn resource_value(value: &str) -> RemediationParameterValue {
        RemediationParameterValue {
            static_value: None,
            resource_value: Some(ResourceValue {
                value: value.to_string(),
            }),
        }
    }

    /// Each parameter resolves to the ONE value the document runs with: a static value to
    /// what it carries, the resource value to the resource the execution is about, and a
    /// value naming neither to nothing at all. The rows differ along which form the
    /// parameter takes.
    #[test]
    fn a_parameter_resolves_to_the_value_the_document_runs_with() {
        let cases: [(&str, RemediationParameterValue, Option<&str>); 4] = [
            (
                "a static value",
                static_value("Suspended"),
                Some("Suspended"),
            ),
            (
                "the resource value",
                resource_value(RESOURCE_VALUE_RESOURCE_ID),
                Some("the-bucket"),
            ),
            (
                "a resource value of another kind",
                resource_value("OTHER"),
                None,
            ),
            (
                "neither kind of value",
                RemediationParameterValue::default(),
                None,
            ),
        ];

        for (case, value, expected) in cases {
            let resolved = resolved_remediation_inputs(
                &configuration(vec![("Parameter", value)]),
                "the-bucket",
            );
            assert_eq!(
                resolved.get("Parameter").map(String::as_str),
                expected,
                "case {case:?}: expected {expected:?}, got {resolved:?}"
            );
        }
    }

    /// Every `AutomationExecutionStatus` places the remediation execution that runs in it
    /// at one `RemediationExecutionState`: not yet picked up is queued, ended well is
    /// succeeded, ended any other way is failed, and anything else is still running.
    ///
    /// The rows are the whole value set of
    /// `com.amazonaws.ssm#AutomationExecutionStatus`, so a status this service does not
    /// place is a row that fails here.
    #[test]
    fn an_automation_status_places_the_remediation_execution() {
        let cases: [(&str, &str); 19] = [
            ("Pending", REMEDIATION_EXECUTION_STATE_QUEUED),
            ("InProgress", REMEDIATION_EXECUTION_STATE_IN_PROGRESS),
            ("Waiting", REMEDIATION_EXECUTION_STATE_IN_PROGRESS),
            ("Success", REMEDIATION_EXECUTION_STATE_SUCCEEDED),
            ("TimedOut", REMEDIATION_EXECUTION_STATE_FAILED),
            ("Cancelling", REMEDIATION_EXECUTION_STATE_IN_PROGRESS),
            ("Cancelled", REMEDIATION_EXECUTION_STATE_FAILED),
            ("Failed", REMEDIATION_EXECUTION_STATE_FAILED),
            ("PendingApproval", REMEDIATION_EXECUTION_STATE_QUEUED),
            ("Approved", REMEDIATION_EXECUTION_STATE_IN_PROGRESS),
            ("Rejected", REMEDIATION_EXECUTION_STATE_FAILED),
            ("Scheduled", REMEDIATION_EXECUTION_STATE_QUEUED),
            ("RunbookInProgress", REMEDIATION_EXECUTION_STATE_IN_PROGRESS),
            (
                "PendingChangeCalendarOverride",
                REMEDIATION_EXECUTION_STATE_QUEUED,
            ),
            (
                "ChangeCalendarOverrideApproved",
                REMEDIATION_EXECUTION_STATE_IN_PROGRESS,
            ),
            (
                "ChangeCalendarOverrideRejected",
                REMEDIATION_EXECUTION_STATE_FAILED,
            ),
            ("CompletedWithSuccess", REMEDIATION_EXECUTION_STATE_SUCCEEDED),
            ("CompletedWithFailure", REMEDIATION_EXECUTION_STATE_FAILED),
            ("Exited", REMEDIATION_EXECUTION_STATE_FAILED),
        ];

        for (status, expected) in cases {
            let placed = remediation_state_of_automation(status);
            assert_eq!(
                placed, expected,
                "case {status:?}: expected {expected}, got {placed}"
            );
        }
    }

    /// A refused start reports what the service that owns the document answered, for
    /// refusals differing along what the answer carried.
    #[test]
    fn a_refused_start_reports_what_the_service_answered() {
        /// One row: what it is called, the code and the sentence the service answered, and
        /// the message the step then carries.
        type Answered = (
            &'static str,
            Option<&'static str>,
            Option<&'static str>,
            Option<&'static str>,
        );
        let cases: [Answered; 4] = [
            (
                "a code and a sentence",
                Some("AutomationDefinitionNotFoundException"),
                Some("An Automation runbook with the specified name couldn't be found."),
                Some(
                    "AutomationDefinitionNotFoundException: An Automation runbook with the \
                     specified name couldn't be found.",
                ),
            ),
            (
                "a code alone",
                Some("InvalidTarget"),
                None,
                Some("InvalidTarget"),
            ),
            (
                "a sentence alone",
                None,
                Some("the runbook takes no such parameter"),
                Some("the runbook takes no such parameter"),
            ),
            ("neither", None, None, None),
        ];

        for (case, code, message, expected) in cases {
            let reported = refusal_message(code, message);
            assert_eq!(
                reported.as_deref(),
                expected,
                "case {case:?}: expected {expected:?}, got {reported:?}"
            );
        }
    }

    /// The window of runs read back ROTATES: with more runs in flight than one call covers,
    /// repeated calls reach every one of them, and a run that has ENDED is never read back
    /// again.
    #[test]
    fn the_window_of_runs_read_back_rotates_over_every_run_in_flight() {
        let now = SystemTime::now();
        let rule = "a-rule";
        let in_flight = MAX_FOLLOWED_AUTOMATIONS * 2 + 3;
        let mut store = ComAmazonawsConfigserviceStarlingdoveserviceState::default();
        for index in 0..in_flight {
            let mut execution = RemediationExecutionEntry::invoked(
                "AWS::S3::Bucket",
                &format!("resource-{index:03}"),
                "a-document",
                now,
            );
            execution.automation = Some(RemediationAutomation::started(
                Some(format!("automation-{index:03}")),
                now,
            ));
            store
                .remediation_executions
                .insert(execution.key(rule), execution);
        }

        // Enough calls to cover every run in flight, each answering a full window.
        let mut followed: BTreeSet<String> = BTreeSet::new();
        for call in 0..in_flight.div_ceil(MAX_FOLLOWED_AUTOMATIONS) {
            let window = store.unsettled_remediation_automations();
            assert_eq!(
                window.len(),
                MAX_FOLLOWED_AUTOMATIONS,
                "call {call}: expected a full window of {MAX_FOLLOWED_AUTOMATIONS}, got \
                 {window:?}"
            );
            followed.extend(window.into_iter().map(|(_, execution_id)| execution_id));
        }
        assert_eq!(
            followed.len(),
            in_flight,
            "expected every one of the {in_flight} runs in flight to be read back, got \
             {followed:?}"
        );

        // A run that has ended leaves the window: there is nothing left to learn about it.
        let settled = ("AWS::S3::Bucket".to_string(), "resource-000".to_string());
        store.note_remediation_automation_progress(
            &(rule.to_string(), settled.0.clone(), settled.1.clone()),
            REMEDIATION_EXECUTION_STATE_SUCCEEDED,
            Some(now),
            None,
            now,
        );
        let mut seen_after: BTreeSet<RemediationExecutionKey> = BTreeSet::new();
        for _ in 0..in_flight.div_ceil(MAX_FOLLOWED_AUTOMATIONS) {
            seen_after.extend(
                store
                    .unsettled_remediation_automations()
                    .into_iter()
                    .map(|(key, _)| key),
            );
        }
        assert!(
            !seen_after.contains(&(rule.to_string(), settled.0.clone(), settled.1.clone()))
                && seen_after.len() == in_flight - 1,
            "a run that has ended must not be read back again, got {seen_after:?}"
        );
    }
}
