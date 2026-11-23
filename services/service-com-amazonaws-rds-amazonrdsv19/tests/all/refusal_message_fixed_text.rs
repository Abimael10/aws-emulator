// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the FIXED TEXT of the refusals this front door answers.
//!
//! The rule under test: a refusal's `Message` is the sentence Amazon RDS words for that
//! error class — its fixed text exactly, with only the variable parts the service itself
//! puts in it (the value the request carried, the identifier it named, the published limit,
//! the status the resource is in) — never prose this emulator composed out of the modelled
//! member names, and never a value the real service withholds. Concretely, over the classes
//! this suite spans:
//!
//! * `InvalidParameterValue` (400) for a `BackupRetentionPeriod` outside the range the
//!   addressed resource accepts states the period the request carried and then the bounds
//!   the engine holds it to — a DB cluster's "1 to 35" and a DB instance's "0 to 35" — so a
//!   caller parses the limits out of the sentence. One wording for every operation that
//!   carries the member.
//! * `SharedSnapshotQuotaExceeded` (400, the `awsQueryError` code of
//!   `SharedSnapshotQuotaExceededFault` — `smithy_query(query="shape",
//!   shape="SharedSnapshotQuotaExceededFault")`) is one sentence carrying the PUBLISHED
//!   limit and nothing of the request.
//! * `DBProxyAlreadyExistsFault` (400) names the requested proxy SINGLE-QUOTED inside the
//!   sentence, in the spelling the request sent (`DBProxyName` is not folded).
//! * `DBSnapshotAlreadyExists` (400) is one sentence about the snapshot that will not be
//!   created, naming the identifier that is taken — the same wording from every operation
//!   that MINTS a DB snapshot.
//! * `InvalidDBClusterStateFault` (400) for a failover asked of a cluster in a state it
//!   cannot be taken in names the cluster and the status it is in; for a delete of a cluster
//!   that still holds members it is a FIXED sentence that discloses neither the cluster nor
//!   its member instances.
//! * `InvalidParameterValue` (400) for a tag carrying a reserved prefix names the member
//!   KIND (keys, values) and the prefix, double-quoted, and echoes neither the key nor the
//!   value.
//! * `DBProxyTargetGroupNotFoundFault` (404) is the proxy API's own not-found wording: the
//!   resource type and the requested name, and nothing of the proxy it was looked for under.
//! * `InvalidParameterCombination` (400) for a tenant-database call against a DB instance
//!   that does not use the multi-tenant configuration reports what could not be resolved,
//!   not the engine the instance runs.
//!
//! Each table spans what the rule generalises over: the value past the bound (below, above,
//! far above, negative) and which operation carries it; how far past the sharing quota the
//! request reaches and whether the accounts arrive in one call or across calls; the shape of
//! the identifier the sentence carries (plain, digit-bearing, single letter, mixed case) and
//! the engine family behind it; which operation minted the colliding snapshot; which status
//! blocked the failover; how many members and which engine blocked the delete; which
//! reserved prefix on which tag member against which resource type; which proxy operation
//! resolved the target group name; and which single-tenant engine the instance runs. The
//! recorded request is ONE row of each.
//!
//! Every call goes over the wire through the harness in `db_instances.rs`, so each assertion
//! reads the XML the emulator itself answered with.

use service_com_amazonaws_rds_amazonrdsv19::state::{
    BACKUP_RETENTION_RANGE, CLUSTER_BACKUP_RETENTION_RANGE, MAX_SNAPSHOT_SHARED_ACCOUNTS,
    RESERVED_TAG_PREFIXES,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::db_instances::{
    call, emulator, expect_error, expect_ok, text, wait_for_settle, REGION,
};

/// The canonical account id every generated emulator presents.
const ACCOUNT: &str = "000000000000";

/// The members one call carries, as a table row writes them down.
type Members = &'static [(&'static str, &'static str)];

/// The Query API's own code for a member value the service will not serve.
const INVALID_PARAMETER_VALUE: &str = "InvalidParameterValue";

/// The Query API's own code for members that cannot hold together.
const INVALID_PARAMETER_COMBINATION: &str = "InvalidParameterCombination";

/// An Aurora engine, whose clusters hold member DB instances of their own.
const AURORA: &str = "aurora-postgresql";

/// A DB instance class every engine in this suite is offered with.
const INSTANCE_CLASS: &str = "db.m5.large";

/// An IAM role ARN of the form `CreateDBProxy$RoleArn` takes.
const PROXY_ROLE_ARN: &str = "arn:aws:iam::000000000000:role/rds-proxy";

/// A subnet id of the published form (`subnet-` plus eight hexadecimal characters).
const SUBNET_ID: &str = "subnet-0a1b2c3d";

/// `CreateDBInstance` with its three required members plus `extra`, waiting out the
/// transition the create owes so the instance is `available` when a row uses it.
async fn create_available_instance(
    router: &MultiServiceRouter,
    identifier: &str,
    engine: &str,
    extra: &[(&str, &str)],
) {
    let mut params = vec![
        ("DBInstanceIdentifier", identifier),
        ("Engine", engine),
        ("DBInstanceClass", INSTANCE_CLASS),
    ];
    params.extend_from_slice(extra);
    expect_ok(
        &format!("create instance {identifier}"),
        call(router, "CreateDBInstance", &params).await,
    );
    wait_for_settle();
}

/// `CreateDBCluster` with its two required members, waiting out the transition the create
/// owes so the cluster is `available` when a row uses it.
async fn create_available_cluster(router: &MultiServiceRouter, identifier: &str, engine: &str) {
    expect_ok(
        &format!("create cluster {identifier}"),
        call(
            router,
            "CreateDBCluster",
            &[("DBClusterIdentifier", identifier), ("Engine", engine)],
        )
        .await,
    );
    wait_for_settle();
}

/// `CreateDBInstance` for a member of `cluster`.
async fn create_member(
    router: &MultiServiceRouter,
    identifier: &str,
    engine: &str,
    cluster: &str,
) {
    expect_ok(
        &format!("create member {identifier}"),
        call(
            router,
            "CreateDBInstance",
            &[
                ("DBInstanceIdentifier", identifier),
                ("Engine", engine),
                ("DBInstanceClass", "db.serverless"),
                ("DBClusterIdentifier", cluster),
            ],
        )
        .await,
    );
}

/// `CreateDBProxy` with its required members, for a proxy of `engine_family`.
async fn create_proxy_call(
    router: &MultiServiceRouter,
    name: &str,
    engine_family: &str,
) -> (u16, String) {
    call(
        router,
        "CreateDBProxy",
        &[
            ("DBProxyName", name),
            ("EngineFamily", engine_family),
            ("RoleArn", PROXY_ROLE_ARN),
            ("VpcSubnetIds.member.1", SUBNET_ID),
        ],
    )
    .await
}

// ---------------------------------------------------------------------------
// BackupRetentionPeriod outside the range the resource accepts
// ---------------------------------------------------------------------------

/// One row of the retention table: the operation that carried the period, the members it
/// needs besides `BackupRetentionPeriod`, the period asked for, and the bounds the addressed
/// resource publishes.
struct RetentionCase {
    case: &'static str,
    action: &'static str,
    params: Vec<(&'static str, String)>,
    period: i32,
    bounds: (i32, i32),
}

impl RetentionCase {
    fn new(
        case: &'static str,
        action: &'static str,
        params: &[(&'static str, &str)],
        period: i32,
        bounds: (i32, i32),
    ) -> Self {
        Self {
            case,
            action,
            params: params
                .iter()
                .map(|(name, value)| (*name, (*value).to_string()))
                .collect(),
            period,
            bounds,
        }
    }
}

/// One row of the accepted table: the operation, the members it needs, and the period at a
/// published bound it asks for.
struct AcceptedRetention {
    case: &'static str,
    action: &'static str,
    params: Members,
    period: i32,
}

/// The sentence a period outside `bounds` is refused with.
fn retention_sentence(period: i32, bounds: (i32, i32)) -> String {
    let (lowest, highest) = bounds;
    format!(
        "Invalid backup retention period: {period}. Retention period must be between \
         {lowest} and {highest}."
    )
}

#[tokio::test]
async fn an_out_of_range_backup_retention_period_states_the_value_and_the_bounds() {
    let router = emulator();
    create_available_cluster(&router, "emu-msg-ret-cluster", AURORA).await;
    create_available_instance(&router, "emu-msg-ret-instance", "postgres", &[]).await;

    // The two published ranges, read from the values the service holds the member to, so a
    // row cannot state bounds the service does not apply.
    let cluster_bounds = (
        *CLUSTER_BACKUP_RETENTION_RANGE.start(),
        *CLUSTER_BACKUP_RETENTION_RANGE.end(),
    );
    let instance_bounds = (
        *BACKUP_RETENTION_RANGE.start(),
        *BACKUP_RETENTION_RANGE.end(),
    );
    assert_eq!(
        (cluster_bounds, instance_bounds),
        ((1, 35), (0, 35)),
        "the published ranges are a DB cluster's \"Must be a value from 1 to 35\" and a DB \
         instance's \"Must be a value from 0 to 35\", got {cluster_bounds:?} and \
         {instance_bounds:?}"
    );

    let rows: Vec<RetentionCase> = vec![
        // The cluster create: one below its lower bound, one above its upper bound, far
        // above it, and a negative period.
        RetentionCase::new(
            "a cluster create one below the lower bound",
            "CreateDBCluster",
            &[("DBClusterIdentifier", "emu-msg-ret-lo"), ("Engine", AURORA)],
            0,
            cluster_bounds,
        ),
        RetentionCase::new(
            "a cluster create one above the upper bound",
            "CreateDBCluster",
            &[("DBClusterIdentifier", "emu-msg-ret-hi"), ("Engine", AURORA)],
            36,
            cluster_bounds,
        ),
        RetentionCase::new(
            "a cluster create far above the upper bound",
            "CreateDBCluster",
            &[("DBClusterIdentifier", "emu-msg-ret-far"), ("Engine", AURORA)],
            40,
            cluster_bounds,
        ),
        RetentionCase::new(
            "a cluster create asking for a negative period",
            "CreateDBCluster",
            &[("DBClusterIdentifier", "emu-msg-ret-neg"), ("Engine", AURORA)],
            -7,
            cluster_bounds,
        ),
        // The cluster modification holds the same range.
        RetentionCase::new(
            "a cluster modification one below the lower bound",
            "ModifyDBCluster",
            &[("DBClusterIdentifier", "emu-msg-ret-cluster")],
            0,
            cluster_bounds,
        ),
        RetentionCase::new(
            "a cluster modification one above the upper bound",
            "ModifyDBCluster",
            &[("DBClusterIdentifier", "emu-msg-ret-cluster")],
            36,
            cluster_bounds,
        ),
        // The cluster restore, whose period is checked before anything is read from the
        // store, so the snapshot it names need not exist for the parameter to be refused.
        RetentionCase::new(
            "a cluster restore above the upper bound",
            "RestoreDBClusterFromSnapshot",
            &[
                ("DBClusterIdentifier", "emu-msg-ret-restored"),
                ("SnapshotIdentifier", "emu-msg-ret-snapshot"),
                ("Engine", AURORA),
            ],
            36,
            cluster_bounds,
        ),
        // The DB instance front doors, whose range admits the 0 that "disables automated
        // backups" and so states a different lower bound.
        RetentionCase::new(
            "an instance create one below the lower bound",
            "CreateDBInstance",
            &[
                ("DBInstanceIdentifier", "emu-msg-ret-inst-lo"),
                ("Engine", "postgres"),
                ("DBInstanceClass", INSTANCE_CLASS),
            ],
            -1,
            instance_bounds,
        ),
        RetentionCase::new(
            "an instance create one above the upper bound",
            "CreateDBInstance",
            &[
                ("DBInstanceIdentifier", "emu-msg-ret-inst-hi"),
                ("Engine", "postgres"),
                ("DBInstanceClass", INSTANCE_CLASS),
            ],
            36,
            instance_bounds,
        ),
        RetentionCase::new(
            "an instance modification far above the upper bound",
            "ModifyDBInstance",
            &[("DBInstanceIdentifier", "emu-msg-ret-instance")],
            100,
            instance_bounds,
        ),
        RetentionCase::new(
            "a promotion above the upper bound",
            "PromoteReadReplica",
            &[("DBInstanceIdentifier", "emu-msg-ret-replica")],
            36,
            instance_bounds,
        ),
        RetentionCase::new(
            "an instance restore below the lower bound",
            "RestoreDBInstanceFromDBSnapshot",
            &[
                ("DBInstanceIdentifier", "emu-msg-ret-inst-restored"),
                ("DBSnapshotIdentifier", "emu-msg-ret-snapshot"),
            ],
            -1,
            instance_bounds,
        ),
    ];

    for row in &rows {
        let mut params = row.params.clone();
        params.push(("BackupRetentionPeriod", row.period.to_string()));
        let body = expect_error(
            row.case,
            call(&router, row.action, &params).await,
            400,
            INVALID_PARAMETER_VALUE,
        );
        let message = text(&body, "Message");
        let expected = retention_sentence(row.period, row.bounds);
        assert_eq!(
            message, expected,
            "case {:?}: expected {expected:?}, got {message:?}",
            row.case
        );
    }

    // Each published bound is SERVED, which is what makes the refusals above about the
    // values past them and not about the member.
    let accepted: &[AcceptedRetention] = &[
        AcceptedRetention {
            case: "a cluster at the published lower bound",
            action: "CreateDBCluster",
            params: &[("DBClusterIdentifier", "emu-msg-ret-ok-1"), ("Engine", AURORA)],
            period: 1,
        },
        AcceptedRetention {
            case: "a cluster at the published upper bound",
            action: "CreateDBCluster",
            params: &[("DBClusterIdentifier", "emu-msg-ret-ok-35"), ("Engine", AURORA)],
            period: 35,
        },
        AcceptedRetention {
            case: "an instance at the published lower bound",
            action: "CreateDBInstance",
            params: &[
                ("DBInstanceIdentifier", "emu-msg-ret-ok-inst-0"),
                ("Engine", "postgres"),
                ("DBInstanceClass", INSTANCE_CLASS),
            ],
            period: 0,
        },
        AcceptedRetention {
            case: "an instance at the published upper bound",
            action: "CreateDBInstance",
            params: &[
                ("DBInstanceIdentifier", "emu-msg-ret-ok-inst-35"),
                ("Engine", "postgres"),
                ("DBInstanceClass", INSTANCE_CLASS),
            ],
            period: 35,
        },
    ];
    for AcceptedRetention {
        case,
        action,
        params,
        period,
    } in accepted
    {
        let mut request: Vec<(&str, String)> = params
            .iter()
            .map(|(name, value)| (*name, (*value).to_string()))
            .collect();
        request.push(("BackupRetentionPeriod", period.to_string()));
        let body = expect_ok(case, call(&router, action, &request).await);
        assert_eq!(
            text(&body, "BackupRetentionPeriod"),
            period.to_string(),
            "case {case:?}: the served period must be the one asked for, got {body}"
        );
    }
}

// ---------------------------------------------------------------------------
// The accounts a manual snapshot may be shared with
// ---------------------------------------------------------------------------

/// The sentence the sharing quota is refused with: entirely fixed text carrying the
/// published limit.
const SHARED_SNAPSHOT_QUOTA_SENTENCE: &str =
    "A snapshot cannot be shared with more than 20 customers.";

/// `account_count` distinct, well-formed 12-digit account ids as `ValuesToAdd` entries.
fn values_to_add(account_count: usize) -> Vec<(String, String)> {
    (0..account_count)
        .map(|index| {
            (
                format!("ValuesToAdd.AttributeValue.{}", index + 1),
                format!("{:012}", 100_000_000_000_u64 + index as u64),
            )
        })
        .collect()
}

#[tokio::test]
async fn sharing_a_manual_snapshot_past_the_quota_states_the_published_limit() {
    let router = emulator();
    create_available_cluster(&router, "emu-msg-share-cluster", AURORA).await;
    expect_ok(
        "take the manual cluster snapshot",
        call(
            &router,
            "CreateDBClusterSnapshot",
            &[
                ("DBClusterSnapshotIdentifier", "emu-msg-share-snap"),
                ("DBClusterIdentifier", "emu-msg-share-cluster"),
            ],
        )
        .await,
    );
    wait_for_settle();

    assert_eq!(
        MAX_SNAPSHOT_SHARED_ACCOUNTS, 20,
        "the sentence carries AWS's published limit on the accounts a manual snapshot can \
         be shared with, got {MAX_SNAPSHOT_SHARED_ACCOUNTS}"
    );

    // One call that asks for more accounts than the quota allows: one past it, a few past
    // it, and the recorded row.
    for (case, accounts) in [
        ("one account past the quota", MAX_SNAPSHOT_SHARED_ACCOUNTS + 1),
        ("five accounts past the quota", MAX_SNAPSHOT_SHARED_ACCOUNTS + 5),
        ("the recorded twenty-one accounts", 21),
    ] {
        let mut params = vec![
            (
                "DBClusterSnapshotIdentifier".to_string(),
                "emu-msg-share-snap".to_string(),
            ),
            ("AttributeName".to_string(), "restore".to_string()),
        ];
        params.extend(values_to_add(accounts));
        let body = expect_error(
            case,
            call(&router, "ModifyDBClusterSnapshotAttribute", &params).await,
            400,
            "SharedSnapshotQuotaExceeded",
        );
        let message = text(&body, "Message");
        assert_eq!(
            message, SHARED_SNAPSHOT_QUOTA_SENTENCE,
            "case {case:?}: expected {SHARED_SNAPSHOT_QUOTA_SENTENCE:?}, got {message:?}"
        );
    }

    // The quota is on the accounts the snapshot ends up shared with, so it is reached ONE
    // CALL AT A TIME too — and the sentence is the same there.
    let mut params = vec![
        (
            "DBClusterSnapshotIdentifier".to_string(),
            "emu-msg-share-snap".to_string(),
        ),
        ("AttributeName".to_string(), "restore".to_string()),
    ];
    params.extend(values_to_add(MAX_SNAPSHOT_SHARED_ACCOUNTS));
    expect_ok(
        "fill the quota in one call",
        call(&router, "ModifyDBClusterSnapshotAttribute", &params).await,
    );
    let body = expect_error(
        "one more account across calls",
        call(
            &router,
            "ModifyDBClusterSnapshotAttribute",
            &[
                ("DBClusterSnapshotIdentifier", "emu-msg-share-snap"),
                ("AttributeName", "restore"),
                ("ValuesToAdd.AttributeValue.1", "999988887777"),
            ],
        )
        .await,
        400,
        "SharedSnapshotQuotaExceeded",
    );
    let message = text(&body, "Message");
    assert_eq!(
        message, SHARED_SNAPSHOT_QUOTA_SENTENCE,
        "case \"one more account across calls\": expected \
         {SHARED_SNAPSHOT_QUOTA_SENTENCE:?}, got {message:?}"
    );
}

// ---------------------------------------------------------------------------
// A DB proxy name the region already holds
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_duplicate_db_proxy_name_is_quoted_inside_the_sentence() {
    let router = emulator();

    // (case, the name both creates send, the engine family of the proxy)
    let rows: [(&str, &str, &str); 5] = [
        ("a plain hyphen-bearing name", "emu-msg-proxy", "MYSQL"),
        ("a name carrying digits", "emu-msg-proxy-2026", "POSTGRESQL"),
        ("a single letter", "e", "MYSQL"),
        // `DBProxyName` is not folded, so the sentence carries the spelling the request
        // sent — not a lower-cased one.
        ("a mixed-case name", "Emu-Msg-Proxy-Mixed", "POSTGRESQL"),
        ("a name of a SQL Server proxy", "emu-msg-proxy-sqlserver", "SQLSERVER"),
    ];

    for (case, name, engine_family) in rows {
        expect_ok(case, create_proxy_call(&router, name, engine_family).await);
        let body = expect_error(
            case,
            create_proxy_call(&router, name, engine_family).await,
            400,
            "DBProxyAlreadyExistsFault",
        );
        let message = text(&body, "Message");
        let expected = format!("The DBProxy '{name}' already exists");
        assert_eq!(
            message, expected,
            "case {case:?}: expected {expected:?}, got {message:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// A DB snapshot identifier the region already holds
// ---------------------------------------------------------------------------

/// The sentence every operation that MINTS a DB snapshot answers a taken identifier with.
fn snapshot_taken_sentence(identifier: &str) -> String {
    format!(
        "Cannot create the snapshot because a snapshot with the identifier {identifier} \
         already exists."
    )
}

#[tokio::test]
async fn a_taken_db_snapshot_identifier_is_refused_with_one_sentence_from_every_mint() {
    let router = emulator();
    for instance in [
        "emu-msg-snap-src",
        "emu-msg-snap-deleted",
        "emu-msg-snap-stopped",
    ] {
        create_available_instance(&router, instance, "postgres", &[]).await;
    }
    // The identifiers a row collides with, differing in shape: a plain name and one
    // carrying digits.
    for taken in ["emu-msg-snap-taken", "emu-msg-snap-2026"] {
        expect_ok(
            taken,
            call(
                &router,
                "CreateDBSnapshot",
                &[
                    ("DBSnapshotIdentifier", taken),
                    ("DBInstanceIdentifier", "emu-msg-snap-src"),
                ],
            )
            .await,
        );
    }
    wait_for_settle();

    // (case, the operation asked to mint the snapshot, its members)
    let rows: [(&str, &str, Members); 5] = [
        (
            "a second CreateDBSnapshot under the plain identifier",
            "CreateDBSnapshot",
            &[
                ("DBSnapshotIdentifier", "emu-msg-snap-taken"),
                ("DBInstanceIdentifier", "emu-msg-snap-src"),
            ],
        ),
        (
            "a second CreateDBSnapshot under the digit-bearing identifier",
            "CreateDBSnapshot",
            &[
                ("DBSnapshotIdentifier", "emu-msg-snap-2026"),
                ("DBInstanceIdentifier", "emu-msg-snap-src"),
            ],
        ),
        (
            "a copy onto a taken identifier",
            "CopyDBSnapshot",
            &[
                ("SourceDBSnapshotIdentifier", "emu-msg-snap-2026"),
                ("TargetDBSnapshotIdentifier", "emu-msg-snap-taken"),
            ],
        ),
        (
            "a delete whose final snapshot names a taken identifier",
            "DeleteDBInstance",
            &[
                ("DBInstanceIdentifier", "emu-msg-snap-deleted"),
                ("FinalDBSnapshotIdentifier", "emu-msg-snap-taken"),
            ],
        ),
        (
            "a stop whose snapshot names a taken identifier",
            "StopDBInstance",
            &[
                ("DBInstanceIdentifier", "emu-msg-snap-stopped"),
                ("DBSnapshotIdentifier", "emu-msg-snap-taken"),
            ],
        ),
    ];

    for (case, action, params) in rows {
        let body = expect_error(
            case,
            call(&router, action, params).await,
            400,
            "DBSnapshotAlreadyExists",
        );
        let message = text(&body, "Message");
        // The identifier in the sentence is the one the REQUEST named.
        let taken = params
            .iter()
            .find(|(name, _)| name.ends_with("DBSnapshotIdentifier") && *name != "SourceDBSnapshotIdentifier")
            .map(|(_, value)| *value)
            .expect("every row names the snapshot it would mint");
        let expected = snapshot_taken_sentence(taken);
        assert_eq!(
            message, expected,
            "case {case:?}: expected {expected:?}, got {message:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// A failover asked of a cluster in a state it cannot be taken in
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_failover_refused_for_the_clusters_state_names_the_cluster_and_the_status() {
    let router = emulator();

    // A cluster the clock has not finished provisioning: the status the create owes has
    // not settled, so the failover is refused for the status it reports now.
    expect_ok(
        "create the cluster that is still coming up",
        call(
            &router,
            "CreateDBCluster",
            &[
                ("DBClusterIdentifier", "emu-msg-fo-creating"),
                ("Engine", AURORA),
            ],
        )
        .await,
    );
    let body = expect_error(
        "a cluster still coming up",
        call(
            &router,
            "FailoverDBCluster",
            &[("DBClusterIdentifier", "emu-msg-fo-creating")],
        )
        .await,
        400,
        "InvalidDBClusterStateFault",
    );
    let message = text(&body, "Message");
    let expected = "DB cluster:emu-msg-fo-creating cannot failover in state:creating";
    assert_eq!(
        message, expected,
        "case \"a cluster still coming up\": expected {expected:?}, got {message:?}"
    );

    // A cluster whose failover is already in flight: the second request arrives while the
    // first is still moving the writer.
    create_available_cluster(&router, "emu-msg-fo-busy", AURORA).await;
    create_member(&router, "emu-msg-fo-writer", AURORA, "emu-msg-fo-busy").await;
    create_member(&router, "emu-msg-fo-reader", AURORA, "emu-msg-fo-busy").await;
    wait_for_settle();
    expect_ok(
        "the first failover",
        call(
            &router,
            "FailoverDBCluster",
            &[("DBClusterIdentifier", "emu-msg-fo-busy")],
        )
        .await,
    );
    let body = expect_error(
        "a cluster already failing over",
        call(
            &router,
            "FailoverDBCluster",
            &[("DBClusterIdentifier", "emu-msg-fo-busy")],
        )
        .await,
        400,
        "InvalidDBClusterStateFault",
    );
    let message = text(&body, "Message");
    let expected = "DB cluster:emu-msg-fo-busy cannot failover in state:failing-over";
    assert_eq!(
        message, expected,
        "case \"a cluster already failing over\": expected {expected:?}, got {message:?}"
    );

    // A cluster on its way down, and one that has arrived there: two more statuses, each
    // reported from the record as it stands.
    create_available_cluster(&router, "emu-msg-fo-stopped", AURORA).await;
    create_member(&router, "emu-msg-fo-stop-1", AURORA, "emu-msg-fo-stopped").await;
    create_member(&router, "emu-msg-fo-stop-2", AURORA, "emu-msg-fo-stopped").await;
    wait_for_settle();
    expect_ok(
        "stop the cluster",
        call(
            &router,
            "StopDBCluster",
            &[("DBClusterIdentifier", "emu-msg-fo-stopped")],
        )
        .await,
    );
    for (case, expected_status, settle) in [
        ("a cluster that is stopping", "stopping", false),
        ("a cluster that has stopped", "stopped", true),
    ] {
        if settle {
            wait_for_settle();
        }
        let body = expect_error(
            case,
            call(
                &router,
                "FailoverDBCluster",
                &[("DBClusterIdentifier", "emu-msg-fo-stopped")],
            )
            .await,
            400,
            "InvalidDBClusterStateFault",
        );
        let message = text(&body, "Message");
        let expected =
            format!("DB cluster:emu-msg-fo-stopped cannot failover in state:{expected_status}");
        assert_eq!(
            message, expected,
            "case {case:?}: expected {expected:?}, got {message:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// A delete of a cluster that still holds member DB instances
// ---------------------------------------------------------------------------

/// The FIXED sentence a delete of a cluster that still holds members is refused with: it
/// states the precondition and discloses neither the cluster nor its member instances.
const CLUSTER_STILL_HAS_MEMBERS_SENTENCE: &str =
    "Cluster can't be deleted. It still contains DB instances in a non-deleting state.";

#[tokio::test]
async fn a_cluster_that_still_holds_members_is_refused_with_one_fixed_sentence() {
    let router = emulator();

    // (case, the cluster, its engine, its members)
    let rows: [(&str, &str, &str, &[&str]); 3] = [
        (
            "one member",
            "emu-msg-members-one",
            AURORA,
            &["emu-msg-member-a"],
        ),
        (
            "two members",
            "emu-msg-members-two",
            AURORA,
            &["emu-msg-member-b", "emu-msg-member-c"],
        ),
        (
            "a cluster of the other Aurora engine",
            "emu-msg-members-mysql",
            "aurora-mysql",
            &["emu-msg-member-d"],
        ),
    ];

    for (case, cluster, engine, members) in rows {
        create_available_cluster(&router, cluster, engine).await;
        for member in members {
            create_member(&router, member, engine, cluster).await;
        }
        wait_for_settle();
        let body = expect_error(
            case,
            call(
                &router,
                "DeleteDBCluster",
                &[
                    ("DBClusterIdentifier", cluster),
                    ("SkipFinalSnapshot", "true"),
                ],
            )
            .await,
            400,
            "InvalidDBClusterStateFault",
        );
        let message = text(&body, "Message");
        assert_eq!(
            message, CLUSTER_STILL_HAS_MEMBERS_SENTENCE,
            "case {case:?}: expected {CLUSTER_STILL_HAS_MEMBERS_SENTENCE:?}, got {message:?}"
        );
        // Nothing of this service's state is in the sentence: not the cluster, and not one
        // member identifier.
        assert!(
            !message.contains(cluster),
            "case {case:?}: the sentence must not name the cluster, got {message:?}"
        );
        for member in members {
            assert!(
                !message.contains(member),
                "case {case:?}: the sentence must not disclose the member {member}, got \
                 {message:?}"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// A tag key or value carrying a prefix the service reserves
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_reserved_tag_prefix_is_refused_by_the_member_kind_and_the_prefix() {
    let router = emulator();
    create_available_instance(&router, "emu-msg-tagged", "postgres", &[]).await;
    create_available_cluster(&router, "emu-msg-tagged-cluster", AURORA).await;

    // Both resource types this suite tags, each by the ARN the service reports for it.
    let targets = [
        (
            "a DB instance",
            format!("arn:aws:rds:{REGION}:{ACCOUNT}:db:emu-msg-tagged"),
        ),
        (
            "a DB cluster",
            format!("arn:aws:rds:{REGION}:{ACCOUNT}:cluster:emu-msg-tagged-cluster"),
        ),
    ];

    // `Tag$Key` / `Tag$Value`: "can't be prefixed with `aws:` or `rds:`" — both prefixes,
    // on both members, against both resource types.
    for (target_case, resource_name) in &targets {
        for prefix in RESERVED_TAG_PREFIXES {
            for (member_case, key, value, expected) in [
                (
                    "a key that is the prefix itself",
                    prefix.to_string(),
                    "v".to_string(),
                    format!("Tag keys cannot start with the reserved prefix \"{prefix}\"."),
                ),
                (
                    "a key carrying the prefix and a suffix",
                    format!("{prefix}emu-reserved"),
                    "v".to_string(),
                    format!("Tag keys cannot start with the reserved prefix \"{prefix}\"."),
                ),
                (
                    "a value carrying the prefix",
                    "emu-plain-key".to_string(),
                    format!("{prefix}emu-reserved"),
                    format!("Tag values cannot start with the reserved prefix \"{prefix}\"."),
                ),
            ] {
                let case = format!("{target_case}: {member_case} {prefix:?}");
                let body = expect_error(
                    &case,
                    call(
                        &router,
                        "AddTagsToResource",
                        &[
                            ("ResourceName", resource_name.as_str()),
                            ("Tags.Tag.1.Key", key.as_str()),
                            ("Tags.Tag.1.Value", value.as_str()),
                        ],
                    )
                    .await,
                    400,
                    INVALID_PARAMETER_VALUE,
                );
                let message = text(&body, "Message");
                assert_eq!(
                    message, expected,
                    "case {case:?}: expected {expected:?}, got {message:?}"
                );
                // The refusal echoes neither the key nor the value back.
                assert!(
                    !message.contains("emu-reserved"),
                    "case {case:?}: the sentence must not echo the rejected tag, got \
                     {message:?}"
                );
            }
        }
    }

    // A tag carrying neither prefix is stored, which is what makes the refusals above
    // about the prefixes.
    expect_ok(
        "a tag outside both reserved prefixes",
        call(
            &router,
            "AddTagsToResource",
            &[
                ("ResourceName", targets[0].1.as_str()),
                ("Tags.Tag.1.Key", "emu-plain-key"),
                ("Tags.Tag.1.Value", "awsome"),
            ],
        )
        .await,
    );
}

// ---------------------------------------------------------------------------
// A target group name no proxy has
// ---------------------------------------------------------------------------

/// One row of the target-group table: the operation under test and the members it carries.
type ProxyTargetGroupRow = (
    &'static str,
    &'static str,
    Vec<(&'static str, String)>,
);

#[tokio::test]
async fn a_target_group_name_no_proxy_has_is_reported_as_a_missing_resource() {
    let router = emulator();
    expect_ok(
        "create the proxy",
        create_proxy_call(&router, "emu-msg-tg-proxy", "MYSQL").await,
    );
    create_available_instance(&router, "emu-msg-tg-target", "mysql", &[]).await;

    // Every operation that resolves a `TargetGroupName`, over the shapes a name that names
    // no target group takes: a plain name, the one group's name in another case, and a name
    // that merely starts with it.
    for group in ["emu-msg-tg-missing", "Default", "defaultgroup"] {
        let rows: [ProxyTargetGroupRow; 5] = [
            (
                "DescribeDBProxyTargetGroups",
                "DescribeDBProxyTargetGroups",
                vec![
                    ("DBProxyName", "emu-msg-tg-proxy".to_string()),
                    ("TargetGroupName", group.to_string()),
                ],
            ),
            (
                "DescribeDBProxyTargets",
                "DescribeDBProxyTargets",
                vec![
                    ("DBProxyName", "emu-msg-tg-proxy".to_string()),
                    ("TargetGroupName", group.to_string()),
                ],
            ),
            (
                "ModifyDBProxyTargetGroup",
                "ModifyDBProxyTargetGroup",
                vec![
                    ("DBProxyName", "emu-msg-tg-proxy".to_string()),
                    ("TargetGroupName", group.to_string()),
                ],
            ),
            (
                "RegisterDBProxyTargets",
                "RegisterDBProxyTargets",
                vec![
                    ("DBProxyName", "emu-msg-tg-proxy".to_string()),
                    ("TargetGroupName", group.to_string()),
                    (
                        "DBInstanceIdentifiers.member.1",
                        "emu-msg-tg-target".to_string(),
                    ),
                ],
            ),
            (
                "DeregisterDBProxyTargets",
                "DeregisterDBProxyTargets",
                vec![
                    ("DBProxyName", "emu-msg-tg-proxy".to_string()),
                    ("TargetGroupName", group.to_string()),
                    (
                        "DBInstanceIdentifiers.member.1",
                        "emu-msg-tg-target".to_string(),
                    ),
                ],
            ),
        ];
        for (operation, action, params) in rows {
            let case = format!("{operation} naming a target group {group:?}");
            let body = expect_error(
                &case,
                call(&router, action, &params).await,
                404,
                "DBProxyTargetGroupNotFoundFault",
            );
            let message = text(&body, "Message");
            let expected = format!("Requested resource not found - TargetGroup: {group}");
            assert_eq!(
                message, expected,
                "case {case:?}: expected {expected:?}, got {message:?}"
            );
            // The proxy the group was looked for under is not in the sentence.
            assert!(
                !message.contains("emu-msg-tg-proxy"),
                "case {case:?}: the sentence must not name the proxy, got {message:?}"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// A tenant database of an instance that holds none
// ---------------------------------------------------------------------------

/// The sentence the model publishes for a tenant database a request could not resolve
/// (`smithy_query(query="shape",shape="TenantDatabaseNotFoundFault")`).
const TENANT_DATABASE_NOT_FOUND_SENTENCE: &str =
    "The specified tenant database wasn't found in the DB instance.";

#[tokio::test]
async fn a_tenant_database_of_a_single_tenant_instance_is_reported_as_unresolved() {
    let router = emulator();

    // Every single-tenant engine in this table holds no tenant database at all: tenant
    // databases "only appl[y] to RDS for Oracle container database (CDB) engines".
    let rows: [(&str, &str, &str); 3] = [
        ("a PostgreSQL instance", "emu-msg-tenant-pg", "postgres"),
        ("a MySQL instance", "emu-msg-tenant-mysql", "mysql"),
        (
            "an Oracle instance outside the CDB engines",
            "emu-msg-tenant-oracle",
            "oracle-se2",
        ),
    ];

    for (case, identifier, engine) in rows {
        create_available_instance(&router, identifier, engine, &[]).await;
        for tenant in ["tenant1", "emu-msg-tenant-absent"] {
            let case = format!("{case}, tenant {tenant:?}");
            let body = expect_error(
                &case,
                call(
                    &router,
                    "DeleteTenantDatabase",
                    &[
                        ("DBInstanceIdentifier", identifier),
                        ("TenantDBName", tenant),
                        ("SkipFinalSnapshot", "true"),
                    ],
                )
                .await,
                400,
                INVALID_PARAMETER_COMBINATION,
            );
            let message = text(&body, "Message");
            assert_eq!(
                message, TENANT_DATABASE_NOT_FOUND_SENTENCE,
                "case {case:?}: expected {TENANT_DATABASE_NOT_FOUND_SENTENCE:?}, got \
                 {message:?}"
            );
            // Neither the instance, nor the engine it runs, nor the tenant name is in it.
            for leaked in [identifier, engine, tenant] {
                assert!(
                    !message.contains(leaked),
                    "case {case:?}: the sentence must not carry {leaked:?}, got {message:?}"
                );
            }
        }
    }
}
