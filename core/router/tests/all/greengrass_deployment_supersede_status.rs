// SPDX-License-Identifier: CC0-1.0

//! What happens to the deployment a new revision REPLACES, across the two services that
//! own the two halves of a Greengrass V2 deployment.
//!
//! `greengrassv2:CreateDeployment` documents that "you can define one deployment for each
//! target. When you create a new deployment for a target that has an existing deployment,
//! you replace the previous deployment", and the two members AWS exposes that replacement
//! through are `deploymentStatus` (a `DeploymentStatus`) and `isLatestForTarget`
//! ("whether or not the deployment is the latest revision for its target").
//!
//! The rule these rows pin: a target has at most ONE deployment being applied, so
//! recording a new revision for a target takes the revision it replaces out of the applied
//! set in one step — the replaced revision stops being the latest for its target and, if it
//! was still being applied, settles in the terminal `INACTIVE` status, while a revision
//! already in a terminal status keeps that status — and every reader of that deployment
//! (its own `GetDeployment` and the `ListDeployments` history) reports the stored pair. The
//! two members therefore never disagree: a revision that is not the latest for its target
//! is never reported as being applied, so a client deciding which revision is current from
//! the status alone and one deciding it from `historyFilter` reach the same answer.
//!
//! The replacement is part of the create, not work that settles later, so no row waits for
//! it: the read right after the create already reports the replaced revision parked.
//!
//! Rows differ along what the rule generalises over: the target FORM (a thing and a thing
//! group), how many revisions the target has (two, three, five), whether the revision being
//! replaced was still being applied or had already reached a terminal status, and — in the
//! second test — how deployments of two targets are interleaved, which is what shows that a
//! create replaces only the revision of the target it names.
//!
//! A deployment's target is a record of the AWS IoT control plane, so every row creates it
//! there first; that join, and the harness these rows drive the front door through, belong
//! to the sibling suite `all/greengrass_deployment_iot_job.rs`, which is why only this
//! configuration — Greengrass V2 and IoT mounted on ONE engine, which is what `core/router`
//! ships — can exercise the rule at all.

use serde_json::{json, Value};

// The one percent-encoder these suites share: an ARN in a query component arrives escaped,
// exactly as an AWS SDK sends it.
use crate::cloudfront_key_value_store_handshake::segment as percent_encode;
use crate::greengrass_deployment_iot_job::{
    thing_arn, thing_group_arn, Emulator, DEPLOYMENT_ACTIVE, DEPLOYMENT_CANCELED,
    DEPLOYMENT_INACTIVE,
};

/// One row: a target, how many revisions are created for it in turn, and which of those
/// revisions (if any) is cancelled before the next one replaces it.
struct SupersedeCase {
    case: &'static str,
    target_arn: String,
    revisions: usize,
    cancelled_revision: Option<usize>,
}

/// Every revision recorded for one target, in creation order, with the `deploymentStatus`
/// each must report at the point the rows read it.
struct History {
    ids: Vec<String>,
    statuses: Vec<String>,
}

/// The status a replaced revision settles in: one still being applied leaves the applied
/// set at `INACTIVE`, one that already reached a terminal status keeps that status.
fn status_after_supersede(current: &str) -> String {
    if current == DEPLOYMENT_ACTIVE {
        DEPLOYMENT_INACTIVE.to_string()
    } else {
        current.to_string()
    }
}

/// The deployment entries of a `ListDeployments` answer, in the order it reported them.
fn entries(body: &Value) -> Vec<Value> {
    body.pointer("/deployments")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

/// A string member of one listed deployment.
fn member(entry: &Value, name: &str) -> String {
    entry
        .get(name)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// The `deploymentId`s a listing reported, in the order it reported them.
fn ids_of(listed: &[Value]) -> Vec<String> {
    listed
        .iter()
        .map(|entry| member(entry, "deploymentId"))
        .collect()
}

/// Create `case.revisions` deployments for one target, one at a time, asserting after
/// every create that the revision it replaced left the applied set in that same step and
/// that nothing else about that revision moved.
async fn roll_out(emulator: &Emulator, case: &SupersedeCase) -> History {
    let label = case.case;
    let mut history = History {
        ids: Vec::new(),
        statuses: Vec::new(),
    };

    for revision in 1..=case.revisions {
        // What the revision this create is about to replace reports BEFORE the create:
        // the replacement may move its status and its latest flag, and nothing else.
        let replaced = match history.ids.last() {
            Some(id) => Some((id.clone(), emulator.get_deployment(id).await.body)),
            None => None,
        };

        let created = emulator
            .create_deployment(json!({
                "targetArn": case.target_arn,
                "deploymentName": format!("revision-{revision}"),
            }))
            .await;
        assert_eq!(
            created.status, 201,
            "case {label:?} revision {revision}: CreateDeployment must answer 201, got {} {:?}",
            created.status, created.body
        );
        let id = created.text("/deploymentId");
        assert!(
            !id.is_empty(),
            "case {label:?} revision {revision}: expected a deploymentId, got {:?}",
            created.body
        );

        // The new revision is the one being applied, and the latest for its target.
        let newest = emulator.get_deployment(&id).await;
        assert_eq!(
            (
                newest.status,
                newest.text("/deploymentStatus"),
                newest.flag("/isLatestForTarget"),
            ),
            (200, DEPLOYMENT_ACTIVE.to_string(), Some(true)),
            "case {label:?} revision {revision}: expected 200 {DEPLOYMENT_ACTIVE} \
             isLatestForTarget=true, got {} {:?}",
            newest.status,
            newest.body
        );

        // …and the revision it replaced is out of the applied set: both members moved, in
        // the one step, and every other member reads back exactly as before.
        if let Some((replaced_id, before)) = replaced {
            let position = history.ids.len() - 1;
            let settled = status_after_supersede(&history.statuses[position]);
            let mut expected = before;
            expected["deploymentStatus"] = json!(settled);
            expected["isLatestForTarget"] = json!(false);
            let after = emulator.get_deployment(&replaced_id).await;
            assert_eq!(
                (after.status, &after.body),
                (200, &expected),
                "case {label:?} revision {revision}: expected the replaced revision to \
                 report {expected}, got {} {:?}",
                after.status,
                after.body
            );
            history.statuses[position] = settled;
        }

        history.ids.push(id.clone());
        history.statuses.push(DEPLOYMENT_ACTIVE.to_string());

        // A row that cancels this revision does so BEFORE the next one replaces it, so the
        // next create finds a revision that is already in a terminal status.
        if case.cancelled_revision == Some(revision) {
            let cancelled = emulator.cancel_deployment(&id).await;
            assert_eq!(
                cancelled.status, 200,
                "case {label:?} revision {revision}: CancelDeployment must answer 200, got \
                 {} {:?}",
                cancelled.status, cancelled.body
            );
            *history
                .statuses
                .last_mut()
                .expect("the revision just recorded") = DEPLOYMENT_CANCELED.to_string();
        }
    }

    history
}

/// Assert both readers of a target's deployments report the pair the rolls-out left: the
/// deployment's own reader, and the history listing under each `DeploymentHistoryFilter`.
async fn readers_agree(emulator: &Emulator, case: &SupersedeCase, history: &History) {
    let label = case.case;
    let newest = history
        .ids
        .last()
        .expect("every row records at least one revision")
        .clone();

    for (position, id) in history.ids.iter().enumerate() {
        let is_latest = *id == newest;
        let expected_status = history.statuses[position].clone();
        let stored = emulator.get_deployment(id).await;
        assert_eq!(
            (
                stored.status,
                stored.text("/deploymentStatus"),
                stored.flag("/isLatestForTarget"),
            ),
            (200, expected_status.clone(), Some(is_latest)),
            "case {label:?} revision {}: expected 200 {expected_status} \
             isLatestForTarget={is_latest}, got {} {:?}",
            position + 1,
            stored.status,
            stored.body
        );
        assert!(
            is_latest || stored.text("/deploymentStatus") != DEPLOYMENT_ACTIVE,
            "case {label:?} revision {}: a revision that is not the latest for its target \
             is never {DEPLOYMENT_ACTIVE}, got {:?}",
            position + 1,
            stored.body
        );
    }

    // The whole history, newest first, with every entry carrying the pair that
    // deployment's own reader reports.
    let encoded = percent_encode(&case.target_arn);
    let whole = emulator
        .list_deployments(&format!("historyFilter=ALL&targetArn={encoded}"))
        .await;
    assert_eq!(
        whole.status, 200,
        "case {label:?}: ListDeployments(ALL) must answer 200, got {} {:?}",
        whole.status, whole.body
    );
    let listed = entries(&whole.body);
    let mut newest_first = history.ids.clone();
    newest_first.reverse();
    assert_eq!(
        ids_of(&listed),
        newest_first,
        "case {label:?}: expected every revision of the target, newest first, got {:?}",
        whole.body
    );
    for (position, id) in history.ids.iter().enumerate() {
        let entry = listed
            .iter()
            .find(|entry| member(entry, "deploymentId") == *id)
            .unwrap_or_else(|| {
                panic!(
                    "case {label:?} revision {}: the listing must carry every revision, got \
                     {:?}",
                    position + 1,
                    whole.body
                )
            });
        assert_eq!(
            (
                member(entry, "deploymentStatus"),
                entry.get("isLatestForTarget").and_then(Value::as_bool),
            ),
            (
                history.statuses[position].clone(),
                Some(*id == newest)
            ),
            "case {label:?} revision {}: the listing must report the pair the deployment's \
             own reader reports, got {entry}",
            position + 1
        );
    }

    // Reading the history by STATUS and reading it by the latest flag reach the same
    // answer: at most one revision of a target is being applied at any instant.
    let mut applied = listed
        .iter()
        .filter(|entry| member(entry, "deploymentStatus") == DEPLOYMENT_ACTIVE)
        .map(|entry| member(entry, "deploymentId"))
        .collect::<Vec<String>>();
    let mut expected_applied = history
        .ids
        .iter()
        .enumerate()
        .filter(|(position, _)| history.statuses[*position] == DEPLOYMENT_ACTIVE)
        .map(|(_, id)| id.clone())
        .collect::<Vec<String>>();
    applied.sort();
    expected_applied.sort();
    assert_eq!(
        applied, expected_applied,
        "case {label:?}: expected exactly {expected_applied:?} to be reported \
         {DEPLOYMENT_ACTIVE}, got {applied:?} in {:?}",
        whole.body
    );
    assert!(
        applied.len() <= 1,
        "case {label:?}: a target never has two deployments being applied, got {applied:?}"
    );

    // …and the filter that selects the latest revision selects exactly the newest one.
    let latest_only = emulator
        .list_deployments(&format!("historyFilter=LATEST_ONLY&targetArn={encoded}"))
        .await;
    assert_eq!(
        (latest_only.status, ids_of(&entries(&latest_only.body))),
        (200, vec![newest.clone()]),
        "case {label:?}: LATEST_ONLY must report only the newest revision, got {} {:?}",
        latest_only.status,
        latest_only.body
    );
}

/// RULE: a new revision for a target takes the revision it replaces out of the applied set
/// in one step — not the latest for its target any more, and parked in a terminal status.
#[tokio::test]
async fn a_new_revision_takes_the_one_it_replaces_out_of_the_applied_set() {
    let emulator = Emulator::new();

    let cases = vec![
        SupersedeCase {
            case: "a thing-group target, two revisions",
            target_arn: thing_group_arn("ReplacedFleet"),
            revisions: 2,
            cancelled_revision: None,
        },
        SupersedeCase {
            case: "a thing target, two revisions",
            target_arn: thing_arn("ReplacedCore"),
            revisions: 2,
            cancelled_revision: None,
        },
        SupersedeCase {
            case: "a thing-group target, three revisions",
            target_arn: thing_group_arn("RevisedFleet"),
            revisions: 3,
            cancelled_revision: None,
        },
        SupersedeCase {
            case: "a thing target, five revisions",
            target_arn: thing_arn("RevisedCore"),
            revisions: 5,
            cancelled_revision: None,
        },
        SupersedeCase {
            case: "the revision being replaced was cancelled first",
            target_arn: thing_group_arn("CancelledThenReplacedFleet"),
            revisions: 3,
            cancelled_revision: Some(2),
        },
        SupersedeCase {
            case: "the first revision was cancelled first",
            target_arn: thing_arn("CancelledThenReplacedCore"),
            revisions: 2,
            cancelled_revision: Some(1),
        },
    ];

    for case in &cases {
        // The deployment's target has to exist in the plane that owns it before any
        // deployment can be recorded for it.
        emulator.ensure_target(&case.target_arn).await;
        let history = roll_out(&emulator, case).await;
        readers_agree(&emulator, case, &history).await;
    }
}

/// RULE: the replacement is per TARGET, and it is where the revision stays: a create
/// replaces only the revision of the target it names, and reading a replaced revision
/// again never moves it back into the applied set.
#[tokio::test]
async fn a_replacement_touches_one_target_and_is_where_the_revision_stays() {
    let emulator = Emulator::new();

    let targets = [
        thing_group_arn("InterleavedFleet"),
        thing_arn("InterleavedCore"),
    ];
    for target in &targets {
        emulator.ensure_target(target).await;
    }

    // Interleaved creates, so each one is judged against every target's history rather
    // than only the target it names.
    let mut recorded: Vec<Vec<String>> = vec![Vec::new(); targets.len()];
    for round in 1..=3 {
        for index in 0..targets.len() {
            let created = emulator
                .create_deployment(json!({
                    "targetArn": targets[index],
                    "deploymentName": format!("round-{round}"),
                }))
                .await;
            assert_eq!(
                created.status, 201,
                "case {:?} round {round}: CreateDeployment must answer 201, got {} {:?}",
                targets[index], created.status, created.body
            );
            recorded[index].push(created.text("/deploymentId"));

            for (target, history) in targets.iter().zip(recorded.iter()) {
                for (position, id) in history.iter().enumerate() {
                    let is_latest = position + 1 == history.len();
                    let expected_status = if is_latest {
                        DEPLOYMENT_ACTIVE
                    } else {
                        DEPLOYMENT_INACTIVE
                    };
                    let stored = emulator.get_deployment(id).await;
                    assert_eq!(
                        (
                            stored.status,
                            stored.text("/deploymentStatus"),
                            stored.flag("/isLatestForTarget"),
                        ),
                        (200, expected_status.to_string(), Some(is_latest)),
                        "case {target:?} round {round} revision {}: expected 200 \
                         {expected_status} isLatestForTarget={is_latest}, got {} {:?}",
                        position + 1,
                        stored.status,
                        stored.body
                    );
                }
            }
        }
    }

    // A read is a report: reading the replaced revision again — and listing the whole
    // history around it — leaves it exactly where the replacement parked it.
    let replaced = recorded[0]
        .first()
        .expect("the first target has revisions")
        .clone();
    let first = emulator.get_deployment(&replaced).await;
    assert_eq!(
        (first.text("/deploymentStatus"), first.flag("/isLatestForTarget")),
        (DEPLOYMENT_INACTIVE.to_string(), Some(false)),
        "case \"the replaced revision\": expected {DEPLOYMENT_INACTIVE} \
         isLatestForTarget=false, got {:?}",
        first.body
    );
    for read in 1..=5 {
        let listing = emulator.list_deployments("historyFilter=ALL").await;
        assert_eq!(
            listing.status, 200,
            "case \"repeated reads\" read {read}: ListDeployments must answer 200, got {} {:?}",
            listing.status, listing.body
        );
        let again = emulator.get_deployment(&replaced).await;
        assert_eq!(
            (again.status, &again.body),
            (200, &first.body),
            "case \"repeated reads\" read {read}: a replaced revision reads back unchanged, \
             got {} {:?}",
            again.status,
            again.body
        );
    }
}
