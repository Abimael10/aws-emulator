// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the SENTENCE four refusal classes of this front door carry: the
//! fixed text the real service words them with, plus only the variable parts it takes from
//! the request and from this service's own state.
//!
//! The classes under test, and the rule each pins:
//!
//! * an `Engine` outside the value list a CLUSTER's own member publishes
//!   (`smithy_query(query="doc", shape="CreateDBClusterMessage", member="Engine")`: "Valid
//!   Values: aurora-mysql aurora-postgresql mysql postgres neptune", plus the `docdb` of the
//!   sibling control plane this front door serves) is refused with the ONE fixed sentence
//!   this service answers an `Engine` that names no engine it serves with — the sentence
//!   states no rule, names no value list and echoes nothing the request carried, exactly as
//!   the instance-level members' refusal does;
//! * the `DBClusterSnapshotAlreadyExistsFault` (HTTP 400,
//!   `smithy_query(query="operation", operation="CreateDBClusterSnapshot")`) is a statement
//!   about the snapshot that cannot be created, whose ONE variable part is the identifier
//!   the snapshot would be stored under — the same sentence from every operation that mints
//!   a cluster snapshot;
//! * a parameter group NAME outside the rule the API publishes for it
//!   (`DBClusterParameterGroupNameMessage$DBClusterParameterGroupName`: "Must be from 1 to
//!   255 letters or numbers. The first character must be a letter. Cannot end with a hyphen
//!   or contain two consecutive hyphens") is refused in the wording that MEMBER carries: a
//!   DB CLUSTER parameter group's name by its SUBJECT, echoing the rejected value after the
//!   two spaces that are part of the service's own text, and the DB parameter group's by the
//!   sentence pair that states the rule and names the member;
//! * the `SubscriptionCategoryNotFoundFault` (HTTP 404,
//!   `smithy_query(query="operation", operation="CreateEventSubscription")`) reports that
//!   the CATEGORY could not be found FOR the requested source type, naming both as the
//!   request spelled them and saying nothing about the categories that type does publish.
//!
//! Each table spans what its rule generalises over — the ways a value can fail to name an
//! engine, the shape and case of a snapshot identifier, every clause of the name rule and
//! each side of its published length bound, and the source types and categories of the
//! published event map — and every row is asserted against BOTH operations of its class, so
//! no sentence is pinned to the one call a recording happened to carry.
//!
//! Every call goes over the wire through the harness in `db_instances.rs`, so each
//! assertion reads the XML the emulator itself answered with, and nothing here waits for a
//! fixed span: the one transition a case needs is POLLED for, through this test target's own
//! shared poll loop.

use service_com_amazonaws_rds_amazonrdsv19::state::{
    is_multi_az_db_cluster, DB_CLUSTER_ENGINES, MAX_PARAMETER_GROUP_NAME_LEN,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::db_instances::{call, emulator, expect_error, expect_ok, text};
use crate::lifecycle_poll_loops::poll_until;

/// The Query API common error a value outside its member's published domain is refused with.
const INVALID_PARAMETER_VALUE: &str = "InvalidParameterValue";

/// An SNS topic ARN of this account, the form `SnsTopicArn` documents.
const TOPIC_ARN: &str = "arn:aws:sns:us-west-2:000000000000:emu-fdrs-events";

/// The four settings a Multi-AZ DB cluster is required to be created with
/// (`AllocatedStorage`, `DBClusterInstanceClass`, `Iops` and `StorageType`, each documented
/// "This setting is required to create a Multi-AZ DB cluster"), with an `Iops` inside the
/// documented ratio of the allocated storage.
const MULTI_AZ_SETTINGS: &[(&str, &str)] = &[
    ("AllocatedStorage", "400"),
    ("DBClusterInstanceClass", "db.m5d.large"),
    ("Iops", "3000"),
    ("StorageType", "gp3"),
];

// ---------------------------------------------------------------------------
// 1. An Engine no DB cluster can run
// ---------------------------------------------------------------------------

/// The ONE fixed sentence this front door refuses an `Engine` that names no engine it serves
/// with — written out here rather than read from the crate, so a drift in it fails this test.
const INVALID_DB_ENGINE: &str = "Invalid DB engine";

/// The ways a value can fail to name an engine a DB cluster can run: a graph query language,
/// another one, a published engine name with its version glued on, a neighbouring API's
/// engine name, an engine this front door publishes that has no CLUSTER form at all, and the
/// empty string.
const NOT_A_CLUSTER_ENGINE: &[(&str, &str)] = &[
    ("a graph query language", "gremlin"),
    ("another graph query language", "sparql"),
    ("a published engine with its version glued on", "neptune1.4"),
    ("a neighbouring API's engine name", "neptune-graph"),
    ("an engine that has no cluster form", "oracle-ee"),
    ("the empty string", ""),
];

/// The two operations whose `Engine` resolves the engine of the CLUSTER they create, with
/// the other members each needs. Neither reads the resource it would act on before the
/// engine is resolved, so the row needs no stored snapshot.
fn cluster_engine_calls(identifier: &str, engine: &str) -> Vec<(&'static str, Vec<(&'static str, String)>)> {
    vec![
        (
            "CreateDBCluster",
            vec![
                ("DBClusterIdentifier", identifier.to_string()),
                ("Engine", engine.to_string()),
            ],
        ),
        (
            "RestoreDBClusterFromSnapshot",
            vec![
                ("DBClusterIdentifier", identifier.to_string()),
                ("SnapshotIdentifier", "emu-fdrs-no-such-snapshot".to_string()),
                ("Engine", engine.to_string()),
            ],
        ),
    ]
}

#[tokio::test]
async fn an_engine_no_db_cluster_can_run_is_refused_with_the_one_fixed_sentence() {
    let router = emulator();

    for (index, (case, engine)) in NOT_A_CLUSTER_ENGINE.iter().enumerate() {
        let identifier = format!("emu-fdrs-engine-{index}");
        for (action, params) in cluster_engine_calls(&identifier, engine) {
            let case = format!("{action} with an Engine that is {case}");
            let body = expect_error(
                &case,
                call(&router, action, &params).await,
                400,
                INVALID_PARAMETER_VALUE,
            );
            let message = text(&body, "Message");
            assert_eq!(
                message, INVALID_DB_ENGINE,
                "case {case:?}: expected {INVALID_DB_ENGINE:?}, got {message:?}"
            );
            assert!(
                engine.is_empty() || !message.contains(*engine),
                "case {case:?}: the sentence echoes no value, got {message:?}"
            );
            // The refused create stored nothing: the cluster it would have created is not
            // there to be read back.
            expect_error(
                &case,
                call(
                    &router,
                    "DescribeDBClusters",
                    &[("DBClusterIdentifier", identifier.as_str())],
                )
                .await,
                404,
                "DBClusterNotFoundFault",
            );
        }
    }

    // The positive control: every engine the member's own value list publishes is served and
    // reads back, so the refusal above is keyed to that list rather than to the values this
    // table happens to name. A Multi-AZ DB cluster engine carries the four settings its own
    // members document as required to create one; which engines those are is the crate's own
    // rule, asked rather than restated here.
    for engine in DB_CLUSTER_ENGINES {
        let case = format!("CreateDBCluster with the published engine {engine:?}");
        let identifier = format!("emu-fdrs-ok-{}", engine.replace('-', ""));
        let mut params: Vec<(&str, String)> = vec![
            ("DBClusterIdentifier", identifier.clone()),
            ("Engine", (*engine).to_string()),
        ];
        if is_multi_az_db_cluster(engine) {
            params.extend(
                MULTI_AZ_SETTINGS
                    .iter()
                    .map(|(member, value)| (*member, (*value).to_string())),
            );
        }
        expect_ok(&case, call(&router, "CreateDBCluster", &params).await);
        let described = expect_ok(
            &case,
            call(
                &router,
                "DescribeDBClusters",
                &[("DBClusterIdentifier", identifier.as_str())],
            )
            .await,
        );
        assert_eq!(
            text(&described, "Engine"),
            *engine,
            "case {case:?}: expected the created cluster to report its engine, got {described}"
        );
    }
}

// ---------------------------------------------------------------------------
// 2. A DB cluster snapshot identifier the region already holds
// ---------------------------------------------------------------------------

/// The sentence every operation that MINTS a cluster snapshot answers the conflict with.
fn snapshot_conflict_sentence(stored_as: &str) -> String {
    format!(
        "Cannot create the cluster snapshot because one with the identifier {stored_as} \
         already exists."
    )
}

/// Read `identifier` until the cluster reports `available`, through the one poll loop this
/// test target shares (`lifecycle_poll_loops::poll_until`): the CONDITION is waited for,
/// never a fixed span.
async fn poll_cluster_available(router: &MultiServiceRouter, case: &str, identifier: &str) {
    let params = [("DBClusterIdentifier", identifier)];
    poll_until(
        case,
        &format!("the cluster {identifier}"),
        || call(router, "DescribeDBClusters", &params),
        |status, body| status == 200 && text(body, "Status") == "available",
    )
    .await;
}

/// Create a cluster of `engine` and poll it up to `available`, which is the state a snapshot
/// can be taken of.
async fn available_cluster(router: &MultiServiceRouter, identifier: &str, engine: &str) {
    expect_ok(
        identifier,
        call(
            router,
            "CreateDBCluster",
            &[
                ("DBClusterIdentifier", identifier),
                ("Engine", engine),
            ],
        )
        .await,
    );
    poll_cluster_available(router, identifier, identifier).await;
}

/// One snapshot-conflict row: the identifier the first snapshot was taken under, the
/// spelling the second call sends, and the spelling the sentence has to carry — the folded
/// one the snapshot is stored under, since the identifier "isn't case-sensitive".
struct SnapshotConflict {
    case: &'static str,
    taken_as: &'static str,
    requested_as: &'static str,
    stored_as: &'static str,
}

/// The rows the conflict sentence is asserted over: the shapes a snapshot identifier takes
/// (plain, digit-bearing, a single letter) and a request that names a held snapshot in
/// another case.
const SNAPSHOT_CONFLICTS: &[SnapshotConflict] = &[
    SnapshotConflict {
        case: "a plain identifier",
        taken_as: "emu-fdrs-snap",
        requested_as: "emu-fdrs-snap",
        stored_as: "emu-fdrs-snap",
    },
    SnapshotConflict {
        case: "an identifier carrying digits and hyphens",
        taken_as: "emu-fdrs-snap-2024-01",
        requested_as: "emu-fdrs-snap-2024-01",
        stored_as: "emu-fdrs-snap-2024-01",
    },
    SnapshotConflict {
        case: "a single-letter identifier",
        taken_as: "e",
        requested_as: "e",
        stored_as: "e",
    },
    SnapshotConflict {
        case: "a held snapshot named in another case",
        taken_as: "emu-fdrs-snap-mixed",
        requested_as: "EMU-FDRS-Snap-Mixed",
        stored_as: "emu-fdrs-snap-mixed",
    },
];

#[tokio::test]
async fn a_cluster_snapshot_identifier_the_region_holds_is_refused_with_the_services_sentence() {
    let router = emulator();
    let cluster = "emu-fdrs-snapshot-source";
    available_cluster(&router, cluster, "aurora-postgresql").await;

    for row in SNAPSHOT_CONFLICTS {
        expect_ok(
            row.case,
            call(
                &router,
                "CreateDBClusterSnapshot",
                &[
                    ("DBClusterIdentifier", cluster),
                    ("DBClusterSnapshotIdentifier", row.taken_as),
                ],
            )
            .await,
        );

        let expected = snapshot_conflict_sentence(row.stored_as);

        // The create that would mint a second snapshot under the held identifier...
        let refused = expect_error(
            row.case,
            call(
                &router,
                "CreateDBClusterSnapshot",
                &[
                    ("DBClusterIdentifier", cluster),
                    ("DBClusterSnapshotIdentifier", row.requested_as),
                ],
            )
            .await,
            400,
            "DBClusterSnapshotAlreadyExistsFault",
        );
        let message = text(&refused, "Message");
        assert_eq!(
            message, expected,
            "case {:?}: expected {expected:?} from the create, got {message:?}",
            row.case
        );

        // ...and the COPY that would put one there answer the one sentence.
        let refused = expect_error(
            row.case,
            call(
                &router,
                "CopyDBClusterSnapshot",
                &[
                    ("SourceDBClusterSnapshotIdentifier", SNAPSHOT_CONFLICTS[0].taken_as),
                    ("TargetDBClusterSnapshotIdentifier", row.requested_as),
                ],
            )
            .await,
            400,
            "DBClusterSnapshotAlreadyExistsFault",
        );
        let message = text(&refused, "Message");
        assert_eq!(
            message, expected,
            "case {:?}: expected {expected:?} from the copy, got {message:?}",
            row.case
        );

        // The refused calls stored nothing extra: the snapshot the region holds is still
        // the one that was taken.
        let described = expect_ok(
            row.case,
            call(
                &router,
                "DescribeDBClusterSnapshots",
                &[("DBClusterSnapshotIdentifier", row.requested_as)],
            )
            .await,
        );
        assert_eq!(
            text(&described, "DBClusterIdentifier"),
            cluster,
            "case {:?}: expected the held snapshot to be the one taken, got {described}",
            row.case
        );
    }

    // The final snapshot a cluster DELETE takes is a manual cluster snapshot like any
    // other, so it answers the same sentence for the same conflict.
    let doomed = "emu-fdrs-final-snapshot-cluster";
    available_cluster(&router, doomed, "neptune").await;
    let held = SNAPSHOT_CONFLICTS[0].stored_as;
    let refused = expect_error(
        "a delete whose final snapshot names a held identifier",
        call(
            &router,
            "DeleteDBCluster",
            &[
                ("DBClusterIdentifier", doomed),
                ("FinalDBSnapshotIdentifier", held),
                ("SkipFinalSnapshot", "false"),
            ],
        )
        .await,
        400,
        "DBClusterSnapshotAlreadyExistsFault",
    );
    let message = text(&refused, "Message");
    let expected = snapshot_conflict_sentence(held);
    assert_eq!(
        message, expected,
        "case \"a delete whose final snapshot names a held identifier\": expected \
         {expected:?}, got {message:?}"
    );
    // The refused delete left the cluster alone.
    let described = expect_ok(
        "the cluster the refused delete named",
        call(
            &router,
            "DescribeDBClusters",
            &[("DBClusterIdentifier", doomed)],
        )
        .await,
    );
    assert_eq!(
        text(&described, "Status"),
        "available",
        "case \"the cluster the refused delete named\": expected the refused delete to leave \
         the cluster available, got {described}"
    );
}

// ---------------------------------------------------------------------------
// 3. A parameter group name outside the published rule
// ---------------------------------------------------------------------------

/// The sentence pair this service family refuses a member whose value is no identifier with,
/// naming the member at fault — written out rather than read from the crate.
fn states_the_rule(member: &str) -> String {
    format!(
        "The parameter {member} is not a valid identifier. Identifiers must begin with a \
         letter; must contain only ASCII letters, digits, and hyphens; and must not end with \
         a hyphen or contain two consecutive hyphens."
    )
}

/// The sentence a DB CLUSTER parameter group's name is refused by its SUBJECT with. The two
/// spaces after the colon are part of the service's own wording.
fn invalid_cluster_parameter_group_name(value: &str) -> String {
    format!("Invalid cluster parameter group name:  {value}")
}

/// Every clause of the name rule, and each side of its published length bound.
fn malformed_names() -> Vec<(String, String)> {
    let longest = format!("e{}", "a".repeat(MAX_PARAMETER_GROUP_NAME_LEN - 1));
    vec![
        ("a leading digit".to_string(), "9emu-fdrs-cpg".to_string()),
        ("a leading hyphen".to_string(), "-emu-fdrs-cpg".to_string()),
        ("a trailing hyphen".to_string(), "emu-fdrs-cpg-".to_string()),
        (
            "two consecutive hyphens".to_string(),
            "emu--fdrs-cpg".to_string(),
        ),
        (
            "an underscore, outside the alphabet".to_string(),
            "emu_fdrs_cpg".to_string(),
        ),
        (
            "a period, the character a default group's name carries".to_string(),
            "default.aurora-mysql8.0".to_string(),
        ),
        ("the empty string".to_string(), String::new()),
        (
            format!("one character past the {MAX_PARAMETER_GROUP_NAME_LEN}-character bound"),
            format!("{longest}a"),
        ),
    ]
}

#[tokio::test]
async fn a_cluster_parameter_group_name_outside_the_rule_is_refused_by_its_subject() {
    let router = emulator();

    for (case, value) in malformed_names() {
        let expected = invalid_cluster_parameter_group_name(&value);

        // The create names the group it would create...
        let case_create = format!("CreateDBClusterParameterGroup with {case}");
        let refused = expect_error(
            &case_create,
            call(
                &router,
                "CreateDBClusterParameterGroup",
                &[
                    ("DBClusterParameterGroupName", value.as_str()),
                    ("DBParameterGroupFamily", "aurora-mysql8.0"),
                    ("Description", "a group under test"),
                ],
            )
            .await,
            400,
            INVALID_PARAMETER_VALUE,
        );
        let message = text(&refused, "Message");
        assert_eq!(
            message, expected,
            "case {case_create:?}: expected {expected:?}, got {message:?}"
        );

        // ...and the COPY names the group its target would be created under, which is a
        // cluster parameter group's name too, so it answers the same subject.
        let case_copy = format!("CopyDBClusterParameterGroup with {case}");
        let refused = expect_error(
            &case_copy,
            call(
                &router,
                "CopyDBClusterParameterGroup",
                &[
                    (
                        "SourceDBClusterParameterGroupIdentifier",
                        "default.aurora-mysql8.0",
                    ),
                    ("TargetDBClusterParameterGroupIdentifier", value.as_str()),
                    (
                        "TargetDBClusterParameterGroupDescription",
                        "a copy under test",
                    ),
                ],
            )
            .await,
            400,
            INVALID_PARAMETER_VALUE,
        );
        let message = text(&refused, "Message");
        assert_eq!(
            message, expected,
            "case {case_copy:?}: expected {expected:?}, got {message:?}"
        );

        // The DB PARAMETER group's name is held to the same rule and worded the other way:
        // the sentence pair, which names the member and echoes nothing. The wording is the
        // member's, which is what this control pins.
        let case_instance_level = format!("CreateDBParameterGroup with {case}");
        let refused = expect_error(
            &case_instance_level,
            call(
                &router,
                "CreateDBParameterGroup",
                &[
                    ("DBParameterGroupName", value.as_str()),
                    ("DBParameterGroupFamily", "mysql8.0"),
                    ("Description", "a group under test"),
                ],
            )
            .await,
            400,
            INVALID_PARAMETER_VALUE,
        );
        let message = text(&refused, "Message");
        let expected_rule = states_the_rule("DBParameterGroupName");
        assert_eq!(
            message, expected_rule,
            "case {case_instance_level:?}: expected {expected_rule:?}, got {message:?}"
        );
    }

    // The positive control: a name the rule admits is created and read back, so the refusals
    // above are the rule's and not a blanket one.
    let admitted = format!("emu-fdrs-cpg{}", "x".repeat(8));
    expect_ok(
        "a name the rule admits",
        call(
            &router,
            "CreateDBClusterParameterGroup",
            &[
                ("DBClusterParameterGroupName", admitted.as_str()),
                ("DBParameterGroupFamily", "aurora-mysql8.0"),
                ("Description", "a group under test"),
            ],
        )
        .await,
    );
    let described = expect_ok(
        "a name the rule admits",
        call(
            &router,
            "DescribeDBClusterParameterGroups",
            &[("DBClusterParameterGroupName", admitted.as_str())],
        )
        .await,
    );
    assert_eq!(
        text(&described, "DBClusterParameterGroupName"),
        admitted,
        "case \"a name the rule admits\": expected the created group to read back, got \
         {described}"
    );
}

// ---------------------------------------------------------------------------
// 4. An event category the source type does not publish
// ---------------------------------------------------------------------------

/// The sentence the `SubscriptionCategoryNotFoundFault` carries. The space before the colon
/// is part of the service's own wording.
fn category_not_found(category: &str, source_type: &str) -> String {
    format!("Category : {category} not found for source type {source_type}.")
}

/// One category row: the source type the subscription names, a category that type does not
/// publish, and one it does.
struct CategoryCase {
    case: &'static str,
    source_type: &'static str,
    absent: &'static str,
    published: &'static str,
}

/// The rows the refusal is asserted over.
///
/// Each source type is one the published event-category map carries
/// (`DescribeEventCategories`, `crate::state::EVENT_CATEGORIES`), and each `absent` value
/// spans a way a category can belong to no entry of that type: a category ANOTHER source
/// type publishes, one this map carries nowhere, a published category spelled in another
/// case (matching is exact), and a category of the type's own neighbour in the map.
const CATEGORY_CASES: &[CategoryCase] = &[
    CategoryCase {
        case: "a parameter group and a category nothing publishes",
        source_type: "db-parameter-group",
        absent: "not-a-real-category",
        published: "configuration change",
    },
    CategoryCase {
        case: "a snapshot and a cluster's failover category",
        source_type: "db-snapshot",
        absent: "failover",
        published: "creation",
    },
    CategoryCase {
        case: "a cluster and an instance's low-storage category",
        source_type: "db-cluster",
        absent: "low storage",
        published: "serverless",
    },
    CategoryCase {
        case: "an instance and a cluster's serverless category",
        source_type: "db-instance",
        absent: "serverless",
        published: "low storage",
    },
    CategoryCase {
        case: "a proxy and a backup category it does not publish",
        source_type: "db-proxy",
        absent: "backup",
        published: "creation",
    },
    CategoryCase {
        case: "a custom engine version and a notification category it does not publish",
        source_type: "custom-engine-version",
        absent: "notification",
        published: "failure",
    },
    CategoryCase {
        case: "a published category spelled in another case",
        source_type: "db-cluster-snapshot",
        absent: "Backup",
        published: "backup",
    },
];

#[tokio::test]
async fn a_category_a_source_type_does_not_publish_is_refused_with_the_services_sentence() {
    let router = emulator();

    for (index, row) in CATEGORY_CASES.iter().enumerate() {
        let expected = category_not_found(row.absent, row.source_type);

        // The create of a subscription that names the category...
        let name = format!("emu-fdrs-sub-{index}");
        let case_create = format!("CreateEventSubscription with {}", row.case);
        let refused = expect_error(
            &case_create,
            call(
                &router,
                "CreateEventSubscription",
                &[
                    ("SubscriptionName", name.as_str()),
                    ("SnsTopicArn", TOPIC_ARN),
                    ("SourceType", row.source_type),
                    ("EventCategories.EventCategory.1", row.absent),
                ],
            )
            .await,
            404,
            "SubscriptionCategoryNotFound",
        );
        let message = text(&refused, "Message");
        assert_eq!(
            message, expected,
            "case {case_create:?}: expected {expected:?}, got {message:?}"
        );
        // The refused create stored nothing.
        expect_error(
            &case_create,
            call(
                &router,
                "DescribeEventSubscriptions",
                &[("SubscriptionName", name.as_str())],
            )
            .await,
            404,
            "SubscriptionNotFound",
        );

        // ...and the MODIFY that would leave a stored subscription naming it answer the one
        // sentence, so the wording belongs to the fault and not to the create.
        let created = expect_ok(
            &case_create,
            call(
                &router,
                "CreateEventSubscription",
                &[
                    ("SubscriptionName", name.as_str()),
                    ("SnsTopicArn", TOPIC_ARN),
                    ("SourceType", row.source_type),
                    ("EventCategories.EventCategory.1", row.published),
                ],
            )
            .await,
        );
        assert_eq!(
            text(&created, "EventCategory"),
            row.published,
            "case {case_create:?}: expected the published category to be stored, got {created}"
        );
        let case_modify = format!("ModifyEventSubscription with {}", row.case);
        let refused = expect_error(
            &case_modify,
            call(
                &router,
                "ModifyEventSubscription",
                &[
                    ("SubscriptionName", name.as_str()),
                    ("SourceType", row.source_type),
                    ("EventCategories.EventCategory.1", row.absent),
                ],
            )
            .await,
            404,
            "SubscriptionCategoryNotFound",
        );
        let message = text(&refused, "Message");
        assert_eq!(
            message, expected,
            "case {case_modify:?}: expected {expected:?}, got {message:?}"
        );
        // The refused modify changed nothing: the subscription still carries the category it
        // was created with.
        let described = expect_ok(
            &case_modify,
            call(
                &router,
                "DescribeEventSubscriptions",
                &[("SubscriptionName", name.as_str())],
            )
            .await,
        );
        assert_eq!(
            text(&described, "EventCategory"),
            row.published,
            "case {case_modify:?}: expected the refused modify to leave the subscription \
             alone, got {described}"
        );
    }

    // Every source type a subscription can name publishes a category map, including the
    // newer ones, so a category of THAT type's map is served and one outside it is refused by
    // the same sentence: the refusal is the map's, and the map is the whole published set.
    expect_ok(
        "a newer source type, with a category its own map publishes",
        call(
            &router,
            "CreateEventSubscription",
            &[
                ("SubscriptionName", "emu-fdrs-sub-newer"),
                ("SnsTopicArn", TOPIC_ARN),
                ("SourceType", "blue-green-deployment"),
                ("EventCategories.EventCategory.1", "deletion"),
            ],
        )
        .await,
    );
    let served = expect_ok(
        "a newer source type, with a category its own map publishes",
        call(
            &router,
            "DescribeEventSubscriptions",
            &[("SubscriptionName", "emu-fdrs-sub-newer")],
        )
        .await,
    );
    assert_eq!(
        text(&served, "EventCategory"),
        "deletion",
        "case \"a newer source type, with a category its own map publishes\": expected the \
         stored subscription to carry the category it named, got {served}"
    );
    let refused = expect_error(
        "a newer source type, with a category its map does not publish",
        call(
            &router,
            "CreateEventSubscription",
            &[
                ("SubscriptionName", "emu-fdrs-sub-newer-bad"),
                ("SnsTopicArn", TOPIC_ARN),
                ("SourceType", "blue-green-deployment"),
                ("EventCategories.EventCategory.1", "not-a-real-category"),
            ],
        )
        .await,
        404,
        "SubscriptionCategoryNotFound",
    );
    let message = text(&refused, "Message");
    assert_eq!(
        message,
        "Category : not-a-real-category not found for source type blue-green-deployment.",
        "case \"a newer source type, with a category its map does not publish\": expected the \
         service's own sentence, got {message:?}"
    );
}
