// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for `ApplyPendingMaintenanceAction`.
//!
//! The rule these pin: a member that SELECTS the resource an operation acts on is resolved
//! against state, and a value that selects nothing is answered with that operation's own
//! modelled not-found error — here `ResourceNotFoundFault`, HTTP 404, awsQuery
//! `Error/Code` `ResourceNotFoundFault`, `Error/Type` `Sender`, and the message
//! `Resource not found: <the resource the value addresses>` — never with a
//! parameter-value refusal and never with an ARN the emulator rendered from its own
//! region and account.
//!
//! It is asserted over a TABLE spanning the forms a `ResourceIdentifier` takes (a plain
//! identifier, either case, at the published identifier bound, a cluster ARN, an instance
//! ARN, an ARN of another region, another resource type and another vendor, an ARN carrying
//! no identifier), with the store both empty and holding a cluster and an instance that are
//! simply not the ones asked for, so a 404 can never come from "the store is empty". The
//! resolving side is asserted the same way: every addressing form that DOES select a stored
//! resource is answered from that resource, over every documented `ApplyAction` and
//! `OptInType` value.
//!
//! Every call goes over the real wire protocol (awsQuery form bodies in, XML out) through
//! the same [`MultiServiceRouter`](smithy_server_runtime::MultiServiceRouter) the production
//! router uses, over the shared harness in [`crate::cluster_lifecycle`].

use std::time::{Duration, Instant};

use crate::cluster_lifecycle::{count, error_code, text, texts, Docdb, REGION};

use service_com_amazonaws_docdb_amazonrdsv19::state;

/// The operation every test here drives.
const ACTION: &str = "ApplyPendingMaintenanceAction";

/// The cluster the seeded store holds.
const STORED_CLUSTER: &str = "alpha-cluster";

/// The instance the seeded store holds, a member of [`STORED_CLUSTER`].
const STORED_INSTANCE: &str = "alpha-instance-1";

/// One `ApplyAction` value this service accepts, and one `OptInType` value, for the tests
/// that vary something else. Both come from the lists below.
const AN_APPLY_ACTION: &str = "system-update";
/// See [`AN_APPLY_ACTION`].
const AN_OPT_IN_TYPE: &str = "immediate";

/// The `ApplyAction` values `ApplyPendingMaintenanceActionMessage$ApplyAction` publishes:
/// "Valid values: `system-update`, `db-upgrade`"
/// (`smithy_query(query="doc",shape="ApplyPendingMaintenanceActionMessage",member="ApplyAction")`).
const APPLY_ACTIONS: [&str; 2] = ["system-update", "db-upgrade"];

/// The `OptInType` values `ApplyPendingMaintenanceActionMessage$OptInType` publishes:
/// "`immediate` … `next-maintenance` … `undo-opt-in`"
/// (`smithy_query(query="doc",shape="ApplyPendingMaintenanceActionMessage",member="OptInType")`).
const OPT_IN_TYPES: [&str; 3] = ["immediate", "next-maintenance", "undo-opt-in"];

/// The ARN form a cluster carries, as `DbClusterRecord::arn` builds it.
fn cluster_arn(identifier: &str) -> String {
    format!("arn:aws:rds:{REGION}:000000000000:cluster:{identifier}")
}

/// The ARN form an instance carries, as `DbInstanceRecord::arn` builds it.
fn instance_arn(identifier: &str) -> String {
    format!("arn:aws:rds:{REGION}:000000000000:db:{identifier}")
}

/// Store a cluster and one instance in it, so every lookup below runs against state some
/// other operation wrote.
async fn seed(docdb: &Docdb) {
    docdb
        .ok(
            "seed cluster",
            "CreateDBCluster",
            &[
                ("DBClusterIdentifier", STORED_CLUSTER),
                ("Engine", "docdb"),
                ("MasterUsername", "admin"),
                ("MasterUserPassword", "secret99"),
            ],
        )
        .await;
    docdb
        .ok(
            "seed instance",
            "CreateDBInstance",
            &[
                ("DBInstanceIdentifier", STORED_INSTANCE),
                ("DBInstanceClass", "db.r5.large"),
                ("Engine", "docdb"),
                ("DBClusterIdentifier", STORED_CLUSTER),
            ],
        )
        .await;
}

/// How long [`poll_until_available`] keeps looking: far longer than the window the service
/// owes a new resource, so a loaded or slow host cannot decide the outcome.
const AVAILABLE_DEADLINE: Duration = Duration::from_secs(30);

/// Describe `identifier` until it reports `available`, and answer with that response.
///
/// A new cluster or instance is created `creating` and the clock owes it `available`; the
/// service never sleeps for that, so a test that needs the settled resource looks again
/// until the CONDITION holds rather than waiting a fixed time.
async fn poll_until_available(
    docdb: &Docdb,
    case: &str,
    action: &str,
    naming_member: &str,
    identifier: &str,
    status_element: &str,
) -> String {
    let deadline = Instant::now() + AVAILABLE_DEADLINE;
    loop {
        let xml = docdb.ok(case, action, &[(naming_member, identifier)]).await;
        if text(&xml, status_element) == "available" {
            return xml;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: {identifier} never reached `available`, last answer {xml}"
        );
        // Inside the poll loop: look again shortly rather than spinning the CPU. Each test
        // runs on its own thread, so blocking it is harmless.
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// A `ResourceIdentifier` that selects no resource of this service is the modelled 404,
/// naming the resource the value addresses — whatever form the value takes, and whether or
/// not other resources exist.
#[tokio::test]
async fn a_resource_identifier_that_selects_nothing_is_the_modelled_not_found() {
    // `CreateDBClusterMessage$DBClusterIdentifier` bounds an identifier at 63 characters
    // (`state::MAX_IDENTIFIER_LEN`); an identifier at that published bound still selects
    // nothing.
    let longest = format!("c{}", "x".repeat(state::MAX_IDENTIFIER_LEN - 1));
    // One character past the bound is no identifier at all, and selects nothing either.
    let past_the_bound = format!("c{}", "x".repeat(state::MAX_IDENTIFIER_LEN));
    let unknown_cluster_arn = cluster_arn("no-such-cluster");
    let unknown_instance_arn = instance_arn("no-such-instance");

    // (case, the value the request carries, the resource the message must name)
    let rows: [(&str, &str, &str); 12] = [
        ("a plain identifier", "no-such-cluster", "no-such-cluster"),
        // Identifiers are stored lowercased, so case cannot turn a miss into a hit, and
        // the message echoes the value as the request supplied it.
        (
            "the same identifier upper-cased",
            "NO-SUCH-CLUSTER",
            "NO-SUCH-CLUSTER",
        ),
        (
            "one character past a stored cluster",
            "alpha-clusterx",
            "alpha-clusterx",
        ),
        (
            "one character past a stored instance",
            "alpha-instance-11",
            "alpha-instance-11",
        ),
        (
            "a cluster ARN of this service",
            unknown_cluster_arn.as_str(),
            "no-such-cluster",
        ),
        (
            "an instance ARN of this service",
            unknown_instance_arn.as_str(),
            "no-such-instance",
        ),
        (
            "a cluster ARN in another region",
            "arn:aws:rds:eu-central-1:000000000000:cluster:elsewhere-cluster",
            "elsewhere-cluster",
        ),
        (
            "an ARN of another resource type",
            "arn:aws:rds:us-east-1:000000000000:subgrp:no-such-group",
            "no-such-group",
        ),
        // No ARN of this service addresses a resource of it, so the whole value is what
        // the caller has to correct.
        (
            "an ARN of another vendor",
            "arn:aws:s3:::no-such-bucket",
            "arn:aws:s3:::no-such-bucket",
        ),
        (
            "an ARN carrying no identifier",
            "arn:aws:rds:us-east-1:000000000000:cluster",
            "arn:aws:rds:us-east-1:000000000000:cluster",
        ),
        ("the longest allowed identifier", &longest, &longest),
        // The value is resolved as submitted: padding is part of it, and no identifier of
        // this service carries a space, so a padded one names nothing.
        (
            "a stored identifier padded with spaces",
            " alpha-cluster ",
            " alpha-cluster ",
        ),
    ];

    // Every request id the refusals carried: an awsQuery error envelope names ONE request,
    // so no two of them may be the same.
    let mut request_ids = std::collections::BTreeSet::new();

    // Both store states: nothing stored at all, and a cluster and an instance stored that
    // simply are not the ones asked for.
    for seeded in [false, true] {
        let docdb = Docdb::new();
        if seeded {
            seed(&docdb).await;
        }
        for (form, requested, addressed) in rows
            .iter()
            .copied()
            .chain([("one character past the identifier bound", past_the_bound.as_str(), past_the_bound.as_str())])
        {
            let case = format!("{form} (store seeded: {seeded})");
            let reply = docdb
                .call(
                    ACTION,
                    &[
                        ("ResourceIdentifier", requested),
                        ("ApplyAction", AN_APPLY_ACTION),
                        ("OptInType", AN_OPT_IN_TYPE),
                    ],
                )
                .await;
            assert_eq!(
                reply.status, 404,
                "case {case:?}: expected the modelled ResourceNotFoundFault's 404, got {} \
                 with {}",
                reply.status, reply.xml
            );
            assert!(
                reply.content_type.starts_with("text/xml"),
                "case {case:?}: an awsQuery error is text/xml, got {:?} with {}",
                reply.content_type,
                reply.xml
            );
            assert_eq!(
                error_code(&reply.xml),
                "ResourceNotFoundFault",
                "case {case:?}: got {}",
                reply.xml
            );
            assert_eq!(
                text(&reply.xml, "Type"),
                "Sender",
                "case {case:?}: a client error is a Sender fault, got {}",
                reply.xml
            );
            assert_eq!(
                text(&reply.xml, "Message"),
                format!("Resource not found: {addressed}"),
                "case {case:?}: expected the fixed text and the addressed resource, got {}",
                reply.xml
            );
            // The emulator's own region and account have no business in this message: the
            // caller gets back the resource it named, not an ARN built from emulator state.
            assert!(
                !text(&reply.xml, "Message").contains("000000000000")
                    || requested.contains("000000000000"),
                "case {case:?}: the message must not carry an emulator-rendered ARN, got {}",
                reply.xml
            );
            let request_id = text(&reply.xml, "RequestId");
            assert_eq!(
                request_id.split('-').map(str::len).collect::<Vec<_>>(),
                vec![8, 4, 4, 4, 12],
                "case {case:?}: expected an 8-4-4-4-12 request id, got {}",
                reply.xml
            );
            assert!(
                request_ids.insert(request_id.clone()),
                "case {case:?}: every response names its own request, but {request_id:?} \
                 was already seen"
            );
        }
    }
}

/// Every form that DOES select a stored resource is answered from that resource: its own
/// ARN, and the pending-action list it really carries — which is empty, because nothing in
/// this service schedules a maintenance action.
#[tokio::test]
async fn a_resource_that_exists_is_answered_with_its_own_pending_action_list() {
    let docdb = Docdb::new();
    seed(&docdb).await;

    let stored_cluster_arn = cluster_arn(STORED_CLUSTER);
    let stored_instance_arn = instance_arn(STORED_INSTANCE);
    let upper_cased_cluster = STORED_CLUSTER.to_ascii_uppercase();

    // (case, the value the request carries, the ARN the answer must report)
    let rows: [(&str, &str, &str); 5] = [
        (
            "the cluster's identifier",
            STORED_CLUSTER,
            stored_cluster_arn.as_str(),
        ),
        (
            "the cluster's identifier upper-cased",
            upper_cased_cluster.as_str(),
            stored_cluster_arn.as_str(),
        ),
        (
            "the cluster's ARN",
            stored_cluster_arn.as_str(),
            stored_cluster_arn.as_str(),
        ),
        (
            "the instance's identifier",
            STORED_INSTANCE,
            stored_instance_arn.as_str(),
        ),
        (
            "the instance's ARN",
            stored_instance_arn.as_str(),
            stored_instance_arn.as_str(),
        ),
    ];

    for (form, requested, expected_arn) in rows {
        for apply_action in APPLY_ACTIONS {
            for opt_in_type in OPT_IN_TYPES {
                let case = format!("{form} with {apply_action}/{opt_in_type}");
                let xml = docdb
                    .ok(
                        &case,
                        ACTION,
                        &[
                            ("ResourceIdentifier", requested),
                            ("ApplyAction", apply_action),
                            ("OptInType", opt_in_type),
                        ],
                    )
                    .await;
                assert_eq!(
                    text(&xml, "ResourceIdentifier"),
                    expected_arn,
                    "case {case:?}: the answer must name the resource that was resolved, \
                     got {xml}"
                );
                // Nothing in this service schedules a maintenance action, so the resolved
                // resource reports the list it really carries: the member is PRESENT and
                // holds no detail, which is how a caller reads "nothing is pending" — never
                // a detail for work nothing ran.
                assert_eq!(
                    count(&xml, "PendingMaintenanceActionDetails"),
                    1,
                    "case {case:?}: expected the action list member to be reported, got {xml}"
                );
                assert_eq!(
                    count(&xml, "PendingMaintenanceAction"),
                    0,
                    "case {case:?}: expected no pending action detail, got {xml}"
                );
            }
        }
    }
}

/// The call reports: it does not move the resource it names. The cluster and the instance
/// read back exactly as they did before, over every documented `OptInType`.
#[tokio::test]
async fn applying_an_action_does_not_change_what_a_later_caller_sees() {
    let docdb = Docdb::new();
    seed(&docdb).await;

    // Read both resources once they have settled, so the comparison below cannot be
    // decided by a transition the clock owed in the middle of the test.
    let before_cluster = poll_until_available(
        &docdb,
        "cluster before",
        "DescribeDBClusters",
        "DBClusterIdentifier",
        STORED_CLUSTER,
        "Status",
    )
    .await;
    let before_instance = poll_until_available(
        &docdb,
        "instance before",
        "DescribeDBInstances",
        "DBInstanceIdentifier",
        STORED_INSTANCE,
        "DBInstanceStatus",
    )
    .await;

    for opt_in_type in OPT_IN_TYPES {
        for (resource, requested) in [
            ("cluster", STORED_CLUSTER),
            ("instance", STORED_INSTANCE),
        ] {
            let case = format!("{resource} with {opt_in_type}");
            docdb
                .ok(
                    &case,
                    ACTION,
                    &[
                        ("ResourceIdentifier", requested),
                        ("ApplyAction", AN_APPLY_ACTION),
                        ("OptInType", opt_in_type),
                    ],
                )
                .await;
        }
    }

    let after_cluster = docdb
        .ok(
            "cluster after",
            "DescribeDBClusters",
            &[("DBClusterIdentifier", STORED_CLUSTER)],
        )
        .await;
    let after_instance = docdb
        .ok(
            "instance after",
            "DescribeDBInstances",
            &[("DBInstanceIdentifier", STORED_INSTANCE)],
        )
        .await;

    // The members a maintenance opt-in would have moved if the call wrote anything: the
    // status, the engine version and the maintenance window, plus the identity members.
    for member in [
        "Status",
        "DBClusterIdentifier",
        "DbClusterResourceId",
        "EngineVersion",
        "PreferredMaintenanceWindow",
    ] {
        assert_eq!(
            texts(&before_cluster, member),
            texts(&after_cluster, member),
            "case {member:?}: applying a pending action must not move the cluster, \
             before {before_cluster} after {after_cluster}"
        );
    }
    for member in [
        "DBInstanceStatus",
        "DBInstanceIdentifier",
        "DbiResourceId",
        "EngineVersion",
        "PreferredMaintenanceWindow",
    ] {
        assert_eq!(
            texts(&before_instance, member),
            texts(&after_instance, member),
            "case {member:?}: applying a pending action must not move the instance, \
             before {before_instance} after {after_instance}"
        );
    }
}

/// A value outside the list a member publishes is a parameter-value refusal — and the
/// not-found answer above comes FIRST, because `ResourceIdentifier` is what selects the
/// resource the other two members describe work on.
#[tokio::test]
async fn a_value_outside_a_members_published_list_is_a_parameter_refusal() {
    let docdb = Docdb::new();
    seed(&docdb).await;

    // (case, ApplyAction, OptInType, the member the refusal must name)
    let rows: [(&str, &str, &str, &str); 5] = [
        (
            "an unpublished apply action",
            "hardware-maintenance",
            AN_OPT_IN_TYPE,
            "ApplyAction",
        ),
        (
            "an apply action in the wrong case",
            "System-Update",
            AN_OPT_IN_TYPE,
            "ApplyAction",
        ),
        ("an empty apply action", "", AN_OPT_IN_TYPE, "ApplyAction"),
        (
            "an unpublished opt-in type",
            AN_APPLY_ACTION,
            "next-window",
            "OptInType",
        ),
        (
            "an opt-in type in the wrong case",
            AN_APPLY_ACTION,
            "Immediate",
            "OptInType",
        ),
    ];

    for (case, apply_action, opt_in_type, member) in rows {
        let reply = docdb
            .call(
                ACTION,
                &[
                    ("ResourceIdentifier", STORED_CLUSTER),
                    ("ApplyAction", apply_action),
                    ("OptInType", opt_in_type),
                ],
            )
            .await;
        assert_eq!(
            reply.status, 400,
            "case {case:?}: a value outside a published list is a client refusal, got {} \
             with {}",
            reply.status, reply.xml
        );
        assert_eq!(
            error_code(&reply.xml),
            "InvalidParameterValue",
            "case {case:?}: got {}",
            reply.xml
        );
        assert!(
            text(&reply.xml, "Message").contains(member),
            "case {case:?}: the refusal must name the member at fault, got {}",
            reply.xml
        );

        // The same request against a resource that does not exist is the not-found answer:
        // the resource is resolved before anything else about the request is judged.
        let missing = docdb
            .call(
                ACTION,
                &[
                    ("ResourceIdentifier", "no-such-cluster"),
                    ("ApplyAction", apply_action),
                    ("OptInType", opt_in_type),
                ],
            )
            .await;
        assert_eq!(
            missing.status, 404,
            "case {case:?}: an absent resource is answered 404 before the other members \
             are judged, got {} with {}",
            missing.status, missing.xml
        );
        assert_eq!(
            error_code(&missing.xml),
            "ResourceNotFoundFault",
            "case {case:?}: got {}",
            missing.xml
        );
    }
}
