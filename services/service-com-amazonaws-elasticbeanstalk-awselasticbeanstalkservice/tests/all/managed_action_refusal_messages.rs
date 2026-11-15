// SPDX-License-Identifier: CC0-1.0

//! How `ApplyEnvironmentManagedAction` WORDS the refusal of an `ActionId` it
//! cannot resolve, driven end to end over the service's awsQuery HTTP surface
//! with the shared harness.
//!
//! RULE under test: when the `ActionId` a request names resolves to no managed
//! action of the environment it names, the call is refused with
//! `ElasticBeanstalkServiceException` (HTTP 400) and the FIXED sentence
//! `Managed update data not found for this environment.` — the refusal reports
//! the absence of managed-update data to resolve the id against, so it carries
//! no part taken from the request or from the store: neither the id asked for
//! nor the environment it was asked about appears in it. An id that DOES resolve
//! to one of the environment's own scheduled actions is not in this class: it is
//! applied, and applying it again is the modelled invalid-state refusal instead.
//! An environment whose termination has completed is not in it either: it is no
//! environment this operation acts on, so naming one is refused as naming no
//! environment at all is — `InvalidParameterValue` with the service's shared
//! not-found wording.
//!
//! The sentence is written out HERE rather than imported from the service, so a
//! change to the handler's wording fails this suite instead of moving with it.
//!
//! The test is a TABLE over what the rule generalises over: which managed-update
//! configuration the environment carries and therefore whether it has any
//! managed-update data at all (none configured, configured with nothing to do,
//! a platform update due, a weekly instance replacement due), which identifier
//! member names the environment (`EnvironmentName` / `EnvironmentId`), and the
//! FORM of the unresolvable id (a lowercase UUID, the same UUID in upper case, a
//! token in no UUID form, a one-character value, a long value, and the id of a
//! managed action that is real but belongs to ANOTHER environment).
//!
//! Nothing here reaches into the store: every precondition is created over the
//! wire and every assertion is made over what the wire answered.

use crate::harness::{wait_for_status, Answer, Emulator};
use crate::managed_actions::option_params;

/// The sentence the service refuses an unresolvable `ActionId` with. Fixed text
/// end to end: no member of the request and no value from the store appears in
/// it.
const MANAGED_UPDATE_DATA_NOT_FOUND: &str = "Managed update data not found for this environment.";

/// The message the modelled invalid-state refusal carries, for the one case that
/// is NOT in the class above: an action that resolved and has already been
/// applied.
const INVALID_STATE_MESSAGE: &str =
    "Managed action is scheduled to execute imminently, is going to be retried, or is already \
     running";

/// The managed-update configuration options this suite sets, from Elastic
/// Beanstalk's published configuration-option reference (the namespaces and
/// names `DescribeConfigurationOptions` publishes).
const MANAGED_ACTIONS: &str = "aws:elasticbeanstalk:managedactions";
/// See [`MANAGED_ACTIONS`].
const PLATFORM_UPDATE: &str = "aws:elasticbeanstalk:managedactions:platformupdate";
/// See [`MANAGED_ACTIONS`].
const MANAGED_ACTIONS_ENABLED: &str = "ManagedActionsEnabled";
/// See [`MANAGED_ACTIONS`].
const PREFERRED_START_TIME: &str = "PreferredStartTime";
/// See [`MANAGED_ACTIONS`].
const INSTANCE_REFRESH_ENABLED: &str = "InstanceRefreshEnabled";

/// A maintenance window, as `PreferredStartTime` states one.
const WINDOW: &str = "Tue:09:00";

/// The application every fixture environment of this suite belongs to.
const APPLICATION: &str = "managed-refusal-app";

/// One fixture environment: the managed-update configuration it carries, whether
/// it runs the release its platform branch has superseded, and how many managed
/// actions that combination leaves it with.
struct Case {
    case: &'static str,
    name: &'static str,
    managed_actions_enabled: bool,
    instance_refresh: bool,
    /// Launch on the release the branch superseded (so a platform update is
    /// available) rather than on the branch's current one.
    superseded: bool,
    /// How many managed actions the environment has, which is what decides
    /// whether it holds managed-update data at all.
    scheduled_actions: usize,
}

/// The fixture environments, one per shape of managed-update data an environment
/// can be in.
const CASES: &[Case] = &[
    Case {
        case: "no managed updates configured",
        name: "refusal-env-none",
        managed_actions_enabled: false,
        instance_refresh: false,
        superseded: true,
        scheduled_actions: 0,
    },
    Case {
        case: "managed updates configured with nothing to update",
        name: "refusal-env-current",
        managed_actions_enabled: true,
        instance_refresh: false,
        superseded: false,
        scheduled_actions: 0,
    },
    Case {
        case: "managed updates with a platform update due",
        name: "refusal-env-update",
        managed_actions_enabled: true,
        instance_refresh: false,
        superseded: true,
        scheduled_actions: 1,
    },
    Case {
        case: "weekly instance replacement due",
        name: "refusal-env-refresh",
        managed_actions_enabled: true,
        instance_refresh: true,
        superseded: false,
        scheduled_actions: 1,
    },
];

/// The forms of an `ActionId` that names no managed action at all. `ActionId` is
/// modelled as a plain string with no constraint, so every one of these reaches
/// the handler.
fn unresolvable_ids() -> Vec<(&'static str, String)> {
    vec![
        (
            "a lowercase UUID",
            "7f1d3b2a-0c4e-4d6f-8a91-2b3c4d5e6f70".to_string(),
        ),
        (
            "the same UUID in upper case",
            "7F1D3B2A-0C4E-4D6F-8A91-2B3C4D5E6F70".to_string(),
        ),
        ("a token in no UUID form", "action-1".to_string()),
        ("a one-character id", "x".to_string()),
        ("a long id", "a".repeat(256)),
    ]
}

/// One platform branch of the service's own catalogue that publishes a release
/// above an earlier one, by the two ARNs an environment can be launched on: the
/// superseded release (so a platform update is available) and the branch's
/// current one. Read off the catalogue through the suite that already does it
/// (`crate::managed_actions::branches`), so no second reading of it lives here.
fn branch() -> (String, String) {
    let branch = crate::managed_actions::branches(1)
        .pop()
        .expect("fixture: one branch publishing a superseded release");
    (branch.from_arn, branch.to_arn)
}

/// Launch the fixture environment `row` describes and wait for the launch the
/// clock owes, answering its `EnvironmentId`.
async fn launch(emulator: &Emulator, row: &Case, superseded_arn: &str, current_arn: &str) -> String {
    let settings = [
        (
            MANAGED_ACTIONS,
            MANAGED_ACTIONS_ENABLED,
            if row.managed_actions_enabled {
                "true"
            } else {
                "false"
            },
        ),
        (MANAGED_ACTIONS, PREFERRED_START_TIME, WINDOW),
        (
            PLATFORM_UPDATE,
            INSTANCE_REFRESH_ENABLED,
            if row.instance_refresh { "true" } else { "false" },
        ),
    ];
    let platform_arn = if row.superseded {
        superseded_arn
    } else {
        current_arn
    };
    let mut params = vec![
        ("ApplicationName".to_string(), APPLICATION.to_string()),
        ("EnvironmentName".to_string(), row.name.to_string()),
        ("PlatformArn".to_string(), platform_arn.to_string()),
    ];
    params.extend(option_params(&settings));
    let borrowed: Vec<(&str, &str)> = params
        .iter()
        .map(|(name, value)| (name.as_str(), value.as_str()))
        .collect();
    let launched = emulator.call("CreateEnvironment", &borrowed).await;
    assert_eq!(
        launched.status, 200,
        "case {:?}: CreateEnvironment answered {} with body {}",
        row.case, launched.status, launched.body
    );
    let environment_id = launched
        .value("EnvironmentId")
        .expect("fixture: a launch reports the environment's id")
        .to_string();
    wait_for_status(emulator, row.case, row.name, "Ready").await;
    environment_id
}

/// Every `ActionId` the environment `name` reports a managed action under.
async fn scheduled_action_ids(emulator: &Emulator, case: &str, name: &str) -> Vec<String> {
    let described = emulator
        .call(
            "DescribeEnvironmentManagedActions",
            &[("EnvironmentName", name)],
        )
        .await;
    assert_eq!(
        described.status, 200,
        "case {case:?}: DescribeEnvironmentManagedActions answered {} with body {}",
        described.status, described.body
    );
    described
        .values_at("ManagedActions/member/ActionId")
        .into_iter()
        .map(str::to_string)
        .collect()
}

/// `ApplyEnvironmentManagedAction` naming the environment by `member` = `value`
/// and the action by `action_id`.
async fn apply(emulator: &Emulator, member: &str, value: &str, action_id: &str) -> Answer {
    emulator
        .call(
            "ApplyEnvironmentManagedAction",
            &[(member, value), ("ActionId", action_id)],
        )
        .await
}

/// Assert that `answer` is the unresolvable-id refusal, worded with no part of
/// the request or the store in it.
fn assert_data_not_found(answer: &Answer, case: &str, action_id: &str, environment: &str) {
    assert_eq!(
        answer.status, 400,
        "case {case:?}: expected HTTP 400, got {} and body {}",
        answer.status, answer.body
    );
    assert_eq!(
        answer.error_code(),
        "ElasticBeanstalkServiceException",
        "case {case:?}: unexpected error code in body {}",
        answer.body
    );
    let message = answer.error_message();
    assert_eq!(
        message, MANAGED_UPDATE_DATA_NOT_FOUND,
        "case {case:?}: expected {MANAGED_UPDATE_DATA_NOT_FOUND:?}, got {message:?}"
    );
    assert!(
        !message.contains(action_id),
        "case {case:?}: the refusal must not name the requested ActionId, got {message:?}"
    );
    assert!(
        !message.contains(environment),
        "case {case:?}: the refusal must not name the environment, got {message:?}"
    );
}

/// RULE: an `ActionId` that resolves to no managed action of the environment it
/// is asked about is refused with `ElasticBeanstalkServiceException` (400) and
/// the fixed sentence `Managed update data not found for this environment.`,
/// whatever the environment's managed-update configuration, whichever member
/// named it and whatever form the id took.
#[tokio::test]
async fn an_unresolvable_action_id_is_refused_with_the_fixed_managed_update_data_sentence() {
    let (superseded_arn, current_arn) = branch();
    let emulator = Emulator::new();
    let created = emulator
        .call("CreateApplication", &[("ApplicationName", APPLICATION)])
        .await;
    assert_eq!(
        created.status, 200,
        "fixture: CreateApplication answered {} with body {}",
        created.status, created.body
    );

    // Every fixture environment, with the managed actions it really has: the
    // ids below are the service's own, read back over the wire.
    let mut environments: Vec<(&Case, String, Vec<String>)> = Vec::new();
    for row in CASES {
        let environment_id = launch(&emulator, row, &superseded_arn, &current_arn).await;
        let action_ids = scheduled_action_ids(&emulator, row.case, row.name).await;
        assert_eq!(
            action_ids.len(),
            row.scheduled_actions,
            "case {:?}: expected {} managed action(s) for this configuration, got {action_ids:?}",
            row.case,
            row.scheduled_actions
        );
        environments.push((row, environment_id, action_ids));
    }

    for (row, environment_id, _) in &environments {
        // The ids of every managed action of ANOTHER environment: real ids of
        // this account, which still resolve to nothing for THIS environment.
        let elsewhere: Vec<(&'static str, String)> = environments
            .iter()
            .filter(|(other, _, _)| other.name != row.name)
            .flat_map(|(_, _, action_ids)| action_ids.clone())
            .map(|action_id| ("another environment's action id", action_id))
            .collect();
        for (form, action_id) in unresolvable_ids().into_iter().chain(elsewhere) {
            for (member, value) in [
                ("EnvironmentName", row.name),
                ("EnvironmentId", environment_id.as_str()),
            ] {
                let case = &format!("{} / {form} by {member}", row.case);
                let refused = apply(&emulator, member, value, &action_id).await;
                assert_data_not_found(&refused, case, &action_id, row.name);
            }
        }
    }

    // The boundary of the rule: an id that DOES resolve to one of the
    // environment's own scheduled actions is applied rather than refused, and
    // applying it a second time is the modelled invalid-state refusal — so the
    // fixed sentence above is the answer to an unresolvable id alone.
    for (row, _, action_ids) in &environments {
        for action_id in action_ids {
            let case = &format!("{} / its own scheduled action", row.case);
            let applied = apply(&emulator, "EnvironmentName", row.name, action_id).await;
            assert_eq!(
                applied.status, 200,
                "case {case:?}: expected the action to be applied, got {} and body {}",
                applied.status, applied.body
            );
            assert_eq!(
                applied.value("ActionId"),
                Some(action_id.as_str()),
                "case {case:?}: the applied action is the one asked for, got body {}",
                applied.body
            );
            let again = apply(&emulator, "EnvironmentName", row.name, action_id).await;
            assert_eq!(
                again.error_code(),
                "ManagedActionInvalidStateException",
                "case {case:?}: an action already applied is refused for its STATE, got body {}",
                again.body
            );
            assert_eq!(
                again.error_message(),
                INVALID_STATE_MESSAGE,
                "case {case:?}: unexpected message in body {}",
                again.body
            );
        }
    }
}

/// RULE: a managed action is maintenance the service performs on a LIVE
/// environment, so an environment whose termination has completed is no
/// environment this operation acts on: naming one is refused exactly as naming
/// no environment at all is — `InvalidParameterValue` (400) with the service's
/// shared not-found wording, naming the member the request used and the value it
/// carried.
#[tokio::test]
async fn a_terminated_environment_is_no_environment_a_managed_action_can_be_applied_to() {
    let (superseded_arn, current_arn) = branch();
    let emulator = Emulator::new();
    let created = emulator
        .call("CreateApplication", &[("ApplicationName", APPLICATION)])
        .await;
    assert_eq!(
        created.status, 200,
        "fixture: CreateApplication answered {} with body {}",
        created.status, created.body
    );

    // Every shape of managed-update data, so the refusal does not depend on
    // whether the environment had a managed action before it was terminated.
    for row in CASES {
        let environment_id = launch(&emulator, row, &superseded_arn, &current_arn).await;
        // The id of a managed action that was real while the environment lived,
        // beside the ids that were never real: after the termination both are
        // refused for the ENVIRONMENT, not for the action.
        let mut ids = unresolvable_ids();
        for action_id in scheduled_action_ids(&emulator, row.case, row.name).await {
            ids.push(("an action id of the live environment", action_id));
        }

        let terminated = emulator
            .call("TerminateEnvironment", &[("EnvironmentName", row.name)])
            .await;
        assert_eq!(
            terminated.status, 200,
            "case {:?}: TerminateEnvironment answered {} with body {}",
            row.case, terminated.status, terminated.body
        );
        wait_for_status(&emulator, row.case, row.name, "Terminated").await;

        for (form, action_id) in ids {
            for (member, value) in [
                ("EnvironmentName", row.name),
                ("EnvironmentId", environment_id.as_str()),
            ] {
                let case = &format!("terminated {} / {form} by {member}", row.case);
                let refused = apply(&emulator, member, value, &action_id).await;
                assert_eq!(
                    refused.status, 400,
                    "case {case:?}: expected HTTP 400, got {} and body {}",
                    refused.status, refused.body
                );
                assert_eq!(
                    refused.error_code(),
                    "InvalidParameterValue",
                    "case {case:?}: unexpected error code in body {}",
                    refused.body
                );
                let expected = format!("No Environment found for {member} = '{value}'.");
                assert_eq!(
                    refused.error_message(),
                    expected,
                    "case {case:?}: expected {expected:?}, got {:?}",
                    refused.error_message()
                );
            }
        }
    }
}
