// SPDX-License-Identifier: CC0-1.0

//! End-to-end wire tests for SageMaker HyperPod's cluster deletion: `DeleteCluster`,
//! proved through the readers a deletion has to show up in — `DescribeCluster` and
//! `ListClusters` — and through `CreateCluster`, which gets the name back once the
//! deletion has completed.
//!
//! Every case goes over the wire (`POST /` with `X-Amz-Target: SageMaker.<Op>`,
//! `awsJson1_1`) through the same `MultiServiceRouter` the production router uses, so
//! what these tests exercise is what a real SDK client gets. The harness is the one
//! `inference_deployment_lifecycle` declares.
//!
//! The tests assert RULES over a TABLE of inputs rather than one recorded call:
//!
//! * a delete answers the `ClusterArn` the create minted — never a second ARN, and the
//!   same one whether the cluster was named by name or by ARN — for clusters differing in
//!   name shape, instance-group count and declared instance counts;
//! * the cluster is `Deleting` from the call that asked for it, and the clock owes the
//!   teardown from that instant, so every reader after it answers as if the cluster were
//!   gone and the name is free for another cluster, which gets a DIFFERENT ARN;
//! * a cluster the service is already moving — one still `Creating`, or one already
//!   `Deleting` — is refused with the modelled `ConflictException` and keeps its status;
//! * an identifier this account holds no cluster under is refused with the modelled
//!   `ResourceNotFound`, whatever form it takes;
//! * a delete touches only the cluster it names: every other cluster in the store is
//!   still readable afterwards, with its own status.

use std::time::SystemTime;

use serde_json::{json, Value};
use service_com_amazonaws_sagemaker_sagemaker::state as sm;

use crate::hyperpod_cluster_audit_record::{audit_capture, seed_not_due};
use crate::hyperpod_cluster_clock::{owe_removal_at, store, FAR_FROM_DUE};
use crate::inference_deployment_lifecycle::{error_code, message, str_field, Emulator, ACCOUNT};

/// An IAM role ARN matching `RoleArn`'s modelled pattern
/// (`^arn:aws[a-z\-]*:iam::\d{12}:role/?[a-zA-Z_0-9+=,.@\-_/]+$`).
const EXECUTION_ROLE: &str = "arn:aws:iam::000000000000:role/HyperPodClusterRole";

/// The access key id the harness's `authorization` header presents
/// (`Credential=AKIDEXAMPLE/...`), which is what an audit record must attribute a change
/// to.
const PRESENTED_AKID: &str = "AKIDEXAMPLE";

/// One instance group: the members `ClusterInstanceGroupSpecification` marks required
/// (smithy_query shape=ClusterInstanceGroupSpecification). The instance types are
/// modelled `ClusterInstanceType` members.
fn instance_group(name: &str, count: i64, instance_type: &str) -> Value {
    json!({
        "InstanceGroupName": name,
        "InstanceCount": count,
        "InstanceType": instance_type,
        "ExecutionRole": EXECUTION_ROLE,
    })
}

/// A create request for `name` holding one group per `(group, count, type)` row.
fn create_body(name: &str, groups: &[(&str, i64, &str)]) -> Value {
    let instance_groups: Vec<Value> = groups
        .iter()
        .map(|(group, count, instance_type)| instance_group(group, *count, instance_type))
        .collect();
    json!({ "ClusterName": name, "InstanceGroups": instance_groups })
}

/// Create the cluster `body` describes, answering its minted `ClusterArn`.
async fn create_cluster(emulator: &Emulator, body: &Value) -> String {
    let created = emulator.ok("CreateCluster", body).await;
    str_field(&created, "ClusterArn")
}

/// `DeleteCluster` for `name_or_arn`, requiring a 200 and answering the `ClusterArn` it
/// reported.
async fn delete_cluster(emulator: &Emulator, name_or_arn: &str) -> String {
    let deleted = emulator
        .ok("DeleteCluster", &json!({ "ClusterName": name_or_arn }))
        .await;
    str_field(&deleted, "ClusterArn")
}

/// The `ClusterStatus` `DescribeCluster` reports for `name_or_arn`, or `None` when the
/// read is refused with `ResourceNotFound`. Any other answer panics: those are the only
/// two outcomes a reader of a deleted cluster may give.
async fn cluster_status(emulator: &Emulator, case: &str, name_or_arn: &str) -> Option<String> {
    let (status, body) = emulator
        .call("DescribeCluster", &json!({ "ClusterName": name_or_arn }))
        .await;
    match status {
        200 => Some(str_field(&body, "ClusterStatus")),
        400 => {
            assert_eq!(
                error_code(&body),
                "ResourceNotFound",
                "case {case:?}: a cluster that is gone must read as ResourceNotFound, \
                 got {body}"
            );
            None
        }
        other => panic!("case {case:?}: unexpected HTTP {other} from DescribeCluster: {body}"),
    }
}

/// The `ClusterName`s `ListClusters` reports, over every page.
async fn listed_names(emulator: &Emulator) -> Vec<String> {
    let mut names = Vec::new();
    let mut request = json!({});
    loop {
        let listed = emulator.ok("ListClusters", &request).await;
        let summaries = listed
            .get("ClusterSummaries")
            .and_then(Value::as_array)
            .unwrap_or_else(|| panic!("expected ClusterSummaries in {listed}"))
            .clone();
        for summary in &summaries {
            names.push(str_field(summary, "ClusterName"));
        }
        match listed.get("NextToken").and_then(Value::as_str) {
            Some(token) => request = json!({ "NextToken": token }),
            None => return names,
        }
    }
}

/// Seed one cluster whose teardown has been asked for and is owed [`FAR_FROM_DUE`] out,
/// straight into the shared store the handler reads — the seam `seed_not_due` uses for a
/// cluster still `Creating`. Answers its `ClusterArn`.
///
/// `DeleteCluster` owes the teardown from the instant it answers
/// (`state::transition_owed_at`), so a cluster the next request still finds `Deleting` is
/// seeded rather than raced for.
fn seed_deleting(emulator: &Emulator, name: &str) -> String {
    let arn = seed_not_due(emulator, name);
    {
        let state = store(&emulator.engine);
        let mut guard = state.lock();
        let cluster = guard
            .clusters
            .get_mut(name)
            .unwrap_or_else(|| panic!("expected the seeded cluster {name:?}"));
        cluster.status = sm::STATUS_DELETING.to_string();
    }
    owe_removal_at(&emulator.engine, name, SystemTime::now() + FAR_FROM_DUE);
    arn
}

/// One row of the delete round trip: a case name, the cluster's name, and the instance
/// groups it is created with.
struct DeleteCase {
    case: &'static str,
    name: String,
    groups: Vec<(&'static str, i64, &'static str)>,
}

#[tokio::test]
async fn a_delete_answers_the_created_arn_for_either_identifier_form() {
    // The rows differ along what the rule generalises over: the name's shape and length,
    // how many instance groups the cluster holds, and how many instances they declare.
    let longest = format!("d{}", "x".repeat(62));
    let cases = [
        DeleteCase {
            case: "one minimal group",
            name: "solo".to_string(),
            groups: vec![("workers", 1, "ml.m5.large")],
        },
        DeleteCase {
            case: "a group declaring no instances",
            name: "empty-group".to_string(),
            groups: vec![("idle", 0, "ml.t3.medium")],
        },
        DeleteCase {
            case: "three groups of different types",
            name: "trio".to_string(),
            groups: vec![
                ("alpha", 2, "ml.c5.large"),
                ("beta", 1, "ml.m5.large"),
                ("gamma", 3, "ml.g5.xlarge"),
            ],
        },
        DeleteCase {
            case: "the longest name the pattern admits",
            name: longest.clone(),
            groups: vec![("workers", 1, "ml.m5.large")],
        },
        DeleteCase {
            case: "a digits-only name",
            name: "90210".to_string(),
            groups: vec![("workers", 2, "ml.m5.large")],
        },
    ];

    // Both forms of `ClusterNameOrArn` must reach the one record and answer its ARN.
    for by_arn in [false, true] {
        for DeleteCase { case, name, groups } in &cases {
            let emulator = Emulator::new();
            let created = create_cluster(&emulator, &create_body(name, groups)).await;

            let identifier = if by_arn { created.clone() } else { name.clone() };
            let deleted = delete_cluster(&emulator, &identifier).await;
            assert_eq!(
                deleted, created,
                "case {case:?} by_arn={by_arn}: the delete must answer the cluster's own \
                 ARN, got {deleted:?}"
            );

            // The teardown is owed from the instant the delete answered, so every reader
            // after it answers as if the cluster were gone — by either identifier form.
            assert_eq!(
                cluster_status(&emulator, case, &created).await,
                None,
                "case {case:?} by_arn={by_arn}: expected the cluster to be gone"
            );
            assert_eq!(
                cluster_status(&emulator, case, name).await,
                None,
                "case {case:?} by_arn={by_arn}: expected the name to resolve to nothing"
            );
            assert_eq!(
                listed_names(&emulator).await,
                Vec::<String>::new(),
                "case {case:?} by_arn={by_arn}: expected no cluster left to list"
            );
        }
    }
}

#[tokio::test]
async fn a_completed_deletion_frees_the_name_for_a_new_cluster() {
    // Repeated, so the rule is the cycle rather than one pass: each create under the
    // same name mints its own cluster, and each delete releases the name again.
    let emulator = Emulator::new();
    let mut seen: Vec<String> = Vec::new();
    for round in 0..3 {
        let created = create_cluster(
            &emulator,
            &create_body("recycled", &[("workers", 1, "ml.m5.large")]),
        )
        .await;
        assert!(
            !seen.contains(&created),
            "round {round}: expected a freshly minted ARN, got {created:?} again"
        );
        seen.push(created.clone());

        // The name is in use while the cluster is: a create under it is refused.
        let (status, refused) = emulator
            .call(
                "CreateCluster",
                &create_body("recycled", &[("workers", 1, "ml.m5.large")]),
            )
            .await;
        assert_eq!(
            status, 400,
            "round {round}: a create against a live cluster of that name must be refused, \
             got {status} with {refused}"
        );
        assert_eq!(
            error_code(&refused),
            "ResourceInUse",
            "round {round}: expected ResourceInUse, got {refused}"
        );

        let deleted = delete_cluster(&emulator, "recycled").await;
        assert_eq!(
            deleted, created,
            "round {round}: the delete must answer the live cluster's ARN"
        );
        // The teardown is owed from that call, so the name is free for the next round's
        // create — which the loop proves by getting a distinct ARN back.
        assert_eq!(
            cluster_status(&emulator, "recycled is gone", &created).await,
            None,
            "round {round}: expected the deleted cluster to be gone"
        );
    }

    // Every round's ARN was distinct, and the last round's cluster is gone too.
    assert_eq!(seen.len(), 3, "expected three distinct clusters, got {seen:?}");
    assert_eq!(
        listed_names(&emulator).await,
        Vec::<String>::new(),
        "expected the store to be empty after the last deletion completed"
    );
}

#[tokio::test]
async fn a_cluster_the_service_is_already_moving_is_refused_as_a_conflict() {
    let emulator = Emulator::new();

    // Row 1: a cluster still provisioning. Seeded with its `InService` far from due, so
    // "still Creating" is a fact rather than a race on a loaded host.
    let provisioning = seed_not_due(&emulator, "provisioning");
    // Row 2: a cluster whose deletion has already been asked for, seeded the same way —
    // the teardown a `DeleteCluster` owes is due from the instant it answers, so a
    // cluster a later request still finds `Deleting` is one whose removal is not yet due.
    let deleting_arn = seed_deleting(&emulator, "deleting");

    let cases = [
        ("still Creating, by name", "provisioning".to_string(), "Creating"),
        ("still Creating, by ARN", provisioning.clone(), "Creating"),
        ("already Deleting, by name", "deleting".to_string(), "Deleting"),
        ("already Deleting, by ARN", deleting_arn.clone(), "Deleting"),
    ];
    for (case, identifier, status_before) in cases {
        let (status, body) = emulator
            .call("DeleteCluster", &json!({ "ClusterName": identifier }))
            .await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} with {body}"
        );
        assert_eq!(
            error_code(&body),
            "ConflictException",
            "case {case:?}: expected ConflictException, got {body}"
        );
        let refusal = message(&body);
        assert!(
            refusal.contains(status_before),
            "case {case:?}: the refusal must name the status that blocks it, got \
             {refusal:?}"
        );

        // The refusal changed nothing: the cluster keeps the status it had.
        assert_eq!(
            cluster_status(&emulator, case, &identifier).await.as_deref(),
            Some(status_before),
            "case {case:?}: expected the status to survive the refusal"
        );
    }

    // Neither cluster was touched by the other's refusals.
    assert_eq!(
        cluster_status(&emulator, "provisioning survives", &provisioning)
            .await
            .as_deref(),
        Some("Creating"),
        "the untouched cluster must survive every refusal"
    );
    assert_eq!(
        cluster_status(&emulator, "deleting survives", &deleting_arn)
            .await
            .as_deref(),
        Some("Deleting"),
        "the cluster whose teardown is not yet due must still report Deleting"
    );
}

#[tokio::test]
async fn an_unknown_identifier_is_refused_as_resource_not_found() {
    let emulator = Emulator::new();
    let known = create_cluster(
        &emulator,
        &create_body("known", &[("workers", 1, "ml.m5.large")]),
    )
    .await;
    let id = known
        .rsplit("cluster/")
        .next()
        .expect("a cluster ARN carries an id")
        .to_string();

    // A cluster that was deleted is one of the rows: the teardown is owed from that call,
    // so from the next request on it is an identifier nothing answers to.
    let gone = create_cluster(
        &emulator,
        &create_body("gone", &[("workers", 1, "ml.m5.large")]),
    )
    .await;
    delete_cluster(&emulator, "gone").await;

    // Every row satisfies `ClusterNameOrArn`'s pattern, so the refusal comes from the
    // handler rather than from boundary validation.
    let cases = [
        ("an unknown name", "unknown".to_string()),
        (
            "a well-formed ARN no cluster carries",
            format!("arn:aws:sagemaker:us-east-1:{ACCOUNT}:cluster/aaaaaaaaaaaa"),
        ),
        (
            "a known ARN in another region",
            format!("arn:aws:sagemaker:eu-west-2:{ACCOUNT}:cluster/{id}"),
        ),
        ("a known cluster's id used as a name", id.clone()),
        ("a prefix of a known name", "know".to_string()),
        ("a cluster whose deletion completed, by name", "gone".to_string()),
        ("a cluster whose deletion completed, by ARN", gone.clone()),
    ];
    for (case, identifier) in cases {
        let (status, body) = emulator
            .call("DeleteCluster", &json!({ "ClusterName": identifier }))
            .await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} with {body}"
        );
        assert_eq!(
            error_code(&body),
            "ResourceNotFound",
            "case {case:?}: expected ResourceNotFound, got {body}"
        );
    }

    // None of the refusals touched the cluster this account does hold.
    assert_eq!(
        cluster_status(&emulator, "the known cluster", &known)
            .await
            .as_deref(),
        Some("InService"),
        "expected the known cluster to survive every refusal"
    );
    assert_eq!(
        listed_names(&emulator).await,
        vec!["known".to_string()],
        "expected only the undeleted cluster to be listed"
    );
}

#[tokio::test]
async fn a_delete_emits_one_audit_record_and_a_refusal_emits_none() {
    // RULE: a delete CHANGES state, so it leaves exactly one record naming the operation,
    // the ARN of the cluster it changed and the access key id the request presented; a
    // refused delete changed nothing, so it leaves none.
    let capture = audit_capture();
    let emulator = Emulator::new();

    // Rows differ along the identifier form the delete named the cluster by, so the
    // record is one per CHANGE rather than one per identifier.
    for (case, by_arn) in [("by name", false), ("by ARN", true)] {
        let name = format!("audited-delete-{}", usize::from(by_arn));
        let created = create_cluster(
            &emulator,
            &create_body(&name, &[("workers", 1, "ml.m5.large")]),
        )
        .await;

        let identifier = if by_arn { created.clone() } else { name.clone() };
        assert_eq!(
            delete_cluster(&emulator, &identifier).await,
            created,
            "case {case:?}: the delete must answer the cluster's own ARN"
        );
        let own = vec![created.clone()];
        let records = capture.records_for("DeleteCluster", &own);
        assert_eq!(
            records.len(),
            1,
            "case {case:?}: exactly one DeleteCluster record for the cluster it deleted, \
             got {records:?}"
        );
        assert_eq!(
            records[0].field("access_key_id"),
            Some(PRESENTED_AKID),
            "case {case:?}: the record must attribute the change to the presented access \
             key id, got {:?}",
            records[0]
        );
    }

    // A refused delete records nothing: a second delete of a cluster whose teardown is
    // not yet due is refused for the `Deleting` status — and the one record the first
    // delete left is still the only one. The cluster is seeded in that state, since the
    // teardown a real delete owes is due from the instant it answers.
    let repeat_name = "audited-delete-repeat";
    let repeated = seed_deleting(&emulator, repeat_name);
    let own = vec![repeated.clone()];
    assert!(
        capture.records_for("DeleteCluster", &own).is_empty(),
        "case repeat: the seeded cluster has no delete record of its own yet"
    );
    let (status, refused) = emulator
        .call("DeleteCluster", &json!({ "ClusterName": repeat_name }))
        .await;
    assert_eq!(
        status, 400,
        "case repeat: expected HTTP 400, got {status} with {refused}"
    );
    assert_eq!(
        error_code(&refused),
        "ConflictException",
        "case repeat: expected ConflictException, got {refused}"
    );
    let after_refusal = capture.records_for("DeleteCluster", &own);
    assert!(
        after_refusal.is_empty(),
        "case repeat: a refused delete must emit no record, got {after_refusal:?}"
    );
}

#[tokio::test]
async fn a_delete_touches_only_the_cluster_it_names() {
    let emulator = Emulator::new();
    let names = ["first", "second", "third", "fourth"];
    let mut arns = Vec::new();
    for name in names {
        arns.push(create_cluster(&emulator, &create_body(name, &[("workers", 1, "ml.m5.large")])).await);
    }

    // Delete them one at a time, in a different order than they were created: after each
    // delete exactly the remaining clusters are listed, each still `InService`.
    let order = [2usize, 0, 3, 1];
    let mut remaining: Vec<usize> = (0..names.len()).collect();
    for index in order {
        let deleted = delete_cluster(&emulator, names[index]).await;
        assert_eq!(
            deleted, arns[index],
            "case {:?}: the delete must answer that cluster's ARN",
            names[index]
        );
        remaining.retain(|held| *held != index);

        let mut expected: Vec<String> = remaining.iter().map(|held| names[*held].to_string()).collect();
        expected.sort();
        let mut listed = listed_names(&emulator).await;
        listed.sort();
        assert_eq!(
            listed, expected,
            "case {:?}: expected exactly the undeleted clusters to be listed",
            names[index]
        );
        for held in &remaining {
            assert_eq!(
                cluster_status(&emulator, names[*held], &arns[*held])
                    .await
                    .as_deref(),
                Some("InService"),
                "case {:?}: {:?} must keep its own status",
                names[index],
                names[*held]
            );
        }
    }
}
