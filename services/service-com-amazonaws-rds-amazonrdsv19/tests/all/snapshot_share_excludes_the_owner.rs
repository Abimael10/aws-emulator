// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for WHO a manual snapshot's `restore` attribute can name: the accounts
//! authorized to copy or restore it, which never include the account that OWNS it.
//!
//! The rule under test: `restore` is "the list of Amazon Web Services accounts that have
//! permission to copy or restore the manual DB cluster snapshot", and the way to grow it is
//! "to share a manual DB cluster snapshot with OTHER Amazon Web Services accounts … use the
//! ValuesToAdd parameter to add a list of IDs of the Amazon Web Services accounts that are
//! authorized to restore the manual DB cluster snapshot". The owner copies and restores its
//! own snapshots by owning them, so it is never a grantee of one: naming its own account id
//! is ACCEPTED — the call answers 200 and every other value it carries is authorized — and
//! grants nothing, so the owner is absent from the attribute the call echoes, from every
//! later read of it, and from the count the published sharing quota is measured against.
//!
//! The tables span what the rule generalises over: both snapshot kinds and both of the
//! operations that write their attribute (`ModifyDBSnapshotAttribute` on a DB snapshot and
//! `ModifyDBClusterSnapshotAttribute` on a DB cluster snapshot), and the FORM the request
//! carries the owner's account id in — alone, beside other accounts, twice over in one call,
//! beside the `all` that makes a snapshot public, and as the last value of a request that
//! already fills the sharing quota with other accounts. The recorded request is ONE row.
//!
//! Every call goes over the wire through `db_instances::emulator`, as a real awsQuery form
//! POST, and the snapshots are TAKEN over the wire too, so every assertion reads state this
//! emulator computed. Nothing waits a span of wall-clock: a snapshot being taken is a change
//! this service's clock owes, and the suite polls a REPORTING call until the snapshot reports
//! `available` ([`crate::lifecycle_poll_loops::poll_until`]).

use service_com_amazonaws_rds_amazonrdsv19::state::MAX_SNAPSHOT_SHARED_ACCOUNTS;
use smithy_server_runtime::MultiServiceRouter;

use crate::db_instances::{call, emulator, expect_ok, optional_text, texts};
use crate::lifecycle_poll_loops::poll_until;

/// `[(name, value.to_string()), ..]` — the query parameters of one call.
macro_rules! params {
    ($($name:expr => $value:expr),* $(,)?) => {
        [$(($name, $value.to_string())),*]
    };
}

/// The account every resource of this emulator belongs to, which is therefore the OWNER of
/// each snapshot below — the one account id the rule keeps out of the attribute.
const OWNER: &str = "000000000000";

/// The `all` value that makes a manual snapshot public, which is no account id at all and is
/// authorized like any other value.
const ALL: &str = "all";

/// Account ids that belong to nobody, of the published 12-digit shape: the values' SHAPE is
/// what the rule is about, never the accounts themselves.
const OTHER_ACCOUNTS: &[&str] = &["111122223333", "444455556666", "777788889999"];

/// Which snapshot kind a row drives, which decides the operations and members it names.
#[derive(Debug, Clone, Copy)]
enum Kind {
    /// A DB snapshot of a DB instance: `CreateDBSnapshot`, `ModifyDBSnapshotAttribute` and
    /// `DescribeDBSnapshotAttributes`.
    DbSnapshot,
    /// A DB CLUSTER snapshot: `CreateDBClusterSnapshot`, `ModifyDBClusterSnapshotAttribute`
    /// and `DescribeDBClusterSnapshotAttributes`.
    DbClusterSnapshot,
}

impl Kind {
    /// The member every call of this kind names its snapshot by.
    fn snapshot_member(self) -> &'static str {
        match self {
            Self::DbSnapshot => "DBSnapshotIdentifier",
            Self::DbClusterSnapshot => "DBClusterSnapshotIdentifier",
        }
    }

    /// The operation that writes the attribute.
    fn modify_action(self) -> &'static str {
        match self {
            Self::DbSnapshot => "ModifyDBSnapshotAttribute",
            Self::DbClusterSnapshot => "ModifyDBClusterSnapshotAttribute",
        }
    }

    /// The operation that reports it.
    fn describe_action(self) -> &'static str {
        match self {
            Self::DbSnapshot => "DescribeDBSnapshotAttributes",
            Self::DbClusterSnapshot => "DescribeDBClusterSnapshotAttributes",
        }
    }
}

/// One row of the ValuesToAdd table: what the request authorizes, and the `restore` values
/// the snapshot carries afterwards.
struct ShareCase {
    case: &'static str,
    /// `ValuesToAdd`, as the request carries them.
    add: &'static [&'static str],
    /// The values the attribute holds afterwards, in the ascending order a set reports.
    authorized: &'static [&'static str],
}

/// Each row is applied to a snapshot of its own, so no row reads another's attribute.
const SHARE_CASES: &[ShareCase] = &[
    ShareCase {
        case: "the owner's own account alone",
        add: &[OWNER],
        authorized: &[],
    },
    ShareCase {
        case: "the owner's own account twice in one call",
        add: &[OWNER, OWNER],
        authorized: &[],
    },
    ShareCase {
        case: "the owner's own account beside two others",
        add: &["111122223333", OWNER, "444455556666"],
        authorized: &["111122223333", "444455556666"],
    },
    ShareCase {
        case: "the owner's own account beside the public value",
        add: &[OWNER, ALL],
        authorized: &[ALL],
    },
];

/// Create an `available` DB instance a DB snapshot can be taken of.
async fn an_instance(router: &MultiServiceRouter, case: &str, identifier: &str) {
    expect_ok(
        case,
        call(
            router,
            "CreateDBInstance",
            &params![
                "DBInstanceIdentifier" => identifier,
                "Engine" => "postgres",
                "DBInstanceClass" => "db.t3.micro",
            ],
        )
        .await,
    );
    let read = params!["DBInstanceIdentifier" => identifier];
    poll_until(
        case,
        "the DB instance reaching available",
        || call(router, "DescribeDBInstances", &read),
        |status, body| {
            status == 200
                && optional_text(body, "DBInstanceStatus").as_deref() == Some("available")
        },
    )
    .await;
}

/// Create an `available` DB cluster a DB cluster snapshot can be taken of.
async fn a_cluster(router: &MultiServiceRouter, case: &str, identifier: &str) {
    expect_ok(
        case,
        call(
            router,
            "CreateDBCluster",
            &params![
                "DBClusterIdentifier" => identifier,
                "Engine" => "aurora-mysql",
            ],
        )
        .await,
    );
    let read = params!["DBClusterIdentifier" => identifier];
    poll_until(
        case,
        "the DB cluster reaching available",
        || call(router, "DescribeDBClusters", &read),
        |status, body| {
            status == 200 && optional_text(body, "Status").as_deref() == Some("available")
        },
    )
    .await;
}

/// Take the snapshot of the row's kind and poll a REPORTING call until it is `available`,
/// which is the state a snapshot can be shared from.
async fn an_available_snapshot(
    router: &MultiServiceRouter,
    case: &str,
    kind: Kind,
    source: &str,
    snapshot: &str,
) {
    match kind {
        Kind::DbSnapshot => {
            expect_ok(
                case,
                call(
                    router,
                    "CreateDBSnapshot",
                    &params![
                        "DBInstanceIdentifier" => source,
                        "DBSnapshotIdentifier" => snapshot,
                    ],
                )
                .await,
            );
            let read = params!["DBSnapshotIdentifier" => snapshot];
            poll_until(
                case,
                "the DB snapshot reaching available",
                || call(router, "DescribeDBSnapshots", &read),
                |status, body| {
                    status == 200 && optional_text(body, "Status").as_deref() == Some("available")
                },
            )
            .await;
        }
        Kind::DbClusterSnapshot => {
            expect_ok(
                case,
                call(
                    router,
                    "CreateDBClusterSnapshot",
                    &params![
                        "DBClusterIdentifier" => source,
                        "DBClusterSnapshotIdentifier" => snapshot,
                    ],
                )
                .await,
            );
            let read = params!["DBClusterSnapshotIdentifier" => snapshot];
            poll_until(
                case,
                "the DB cluster snapshot reaching available",
                || call(router, "DescribeDBClusterSnapshots", &read),
                |status, body| {
                    status == 200 && optional_text(body, "Status").as_deref() == Some("available")
                },
            )
            .await;
        }
    }
}

/// The write of the `restore` attribute of `snapshot`, authorizing `add`.
async fn authorize(
    router: &MultiServiceRouter,
    kind: Kind,
    snapshot: &str,
    add: &[&str],
) -> (u16, String) {
    let mut request: Vec<(String, String)> = vec![
        (kind.snapshot_member().to_string(), snapshot.to_string()),
        ("AttributeName".to_string(), "restore".to_string()),
    ];
    for (index, value) in add.iter().enumerate() {
        request.push((
            format!("ValuesToAdd.AttributeValue.{}", index + 1),
            (*value).to_string(),
        ));
    }
    call(router, kind.modify_action(), &request).await
}

/// The `restore` values a report carries, asserted to be the ONE attribute AWS defines.
fn restore_values(case: &str, body: &str) -> Vec<String> {
    assert_eq!(
        texts(body, "AttributeName"),
        vec!["restore".to_string()],
        "case {case:?}: `restore` is the one attribute a snapshot carries, got {body}"
    );
    texts(body, "AttributeValue")
}

/// Read the attribute back, which is where the rule has to hold too: the owner is no grantee
/// in STATE, not only in the answer the write composed.
async fn read_back(router: &MultiServiceRouter, case: &str, kind: Kind, snapshot: &str) -> String {
    expect_ok(
        case,
        call(
            router,
            kind.describe_action(),
            &params![kind.snapshot_member() => snapshot],
        )
        .await,
    )
}

#[tokio::test]
async fn the_owner_account_is_never_a_grantee_of_its_own_snapshot() {
    for kind in [Kind::DbSnapshot, Kind::DbClusterSnapshot] {
        // Each kind gets its own emulator, so one kind's snapshots cannot be read as the
        // other's.
        let router = emulator();
        let source = "emu-own-share-source";
        match kind {
            Kind::DbSnapshot => an_instance(&router, "the source instance", source).await,
            Kind::DbClusterSnapshot => a_cluster(&router, "the source cluster", source).await,
        }

        for (index, row) in SHARE_CASES.iter().enumerate() {
            let case = format!("{kind:?} — {}", row.case);
            let snapshot = format!("emu-own-share-{index}");
            an_available_snapshot(&router, &case, kind, source, &snapshot).await;

            let expected: Vec<String> = row
                .authorized
                .iter()
                .map(|value| (*value).to_string())
                .collect();
            // The owner's own account id is no reason to refuse the call: the request is
            // accepted and every OTHER value it carries is authorized.
            let answered = expect_ok(&case, authorize(&router, kind, &snapshot, row.add).await);
            let echoed = restore_values(&case, &answered);
            assert_eq!(
                echoed, expected,
                "case {case:?}: expected the echoed restore attribute {expected:?}, got \
                 {echoed:?}"
            );
            let reported = read_back(&router, &case, kind, &snapshot).await;
            let stored = restore_values(&case, &reported);
            assert_eq!(
                stored, expected,
                "case {case:?}: expected a later read to report {expected:?}, got {stored:?}"
            );
            for values in [&echoed, &stored] {
                assert!(
                    !values.contains(&OWNER.to_string()),
                    "case {case:?}: the owning account is no grantee of its own snapshot, got \
                     {values:?}"
                );
            }
        }
    }
}

#[tokio::test]
async fn the_owner_account_counts_toward_no_sharing_quota() {
    for kind in [Kind::DbSnapshot, Kind::DbClusterSnapshot] {
        let router = emulator();
        let source = "emu-own-quota-source";
        match kind {
            Kind::DbSnapshot => an_instance(&router, "the source instance", source).await,
            Kind::DbClusterSnapshot => a_cluster(&router, "the source cluster", source).await,
        }
        let snapshot = "emu-own-quota-snap";
        let case = format!("{kind:?} — the quota filled, then the owner named too");
        an_available_snapshot(&router, &case, kind, source, snapshot).await;

        // The published allowance, filled with accounts that are really grantees: each one
        // is "an Amazon Web Services account … authorized to restore" the snapshot, and the
        // published limit is on those.
        let grantees: Vec<String> = (0..MAX_SNAPSHOT_SHARED_ACCOUNTS)
            .map(|nth| format!("{:012}", 100_000_000_000u64 + nth as u64))
            .collect();
        let filling: Vec<&str> = grantees.iter().map(String::as_str).collect();
        let answered = expect_ok(&case, authorize(&router, kind, snapshot, &filling).await);
        assert_eq!(
            restore_values(&case, &answered).len(),
            MAX_SNAPSHOT_SHARED_ACCOUNTS,
            "case {case:?}: the published allowance must be authorizable in full, got \
             {answered}"
        );
        // One more value — the OWNER's own account — authorizes nobody, so it cannot take
        // the snapshot past the allowance: the call is served and the attribute is unchanged.
        let answered = expect_ok(&case, authorize(&router, kind, snapshot, &[OWNER]).await);
        let echoed = restore_values(&case, &answered);
        assert_eq!(
            echoed, grantees,
            "case {case:?}: expected the attribute to stay the {MAX_SNAPSHOT_SHARED_ACCOUNTS} \
             grantees, got {echoed:?}"
        );
        let reported = read_back(&router, &case, kind, snapshot).await;
        let stored = restore_values(&case, &reported);
        assert_eq!(
            stored, grantees,
            "case {case:?}: expected a later read to report the same grantees, got {stored:?}"
        );
    }
}

/// The other half of the rule, so the suite cannot pass by dropping every value: an account
/// that is NOT the owner is authorized and read back, on both snapshot kinds.
#[tokio::test]
async fn an_account_other_than_the_owner_is_authorized_and_read_back() {
    for kind in [Kind::DbSnapshot, Kind::DbClusterSnapshot] {
        let router = emulator();
        let source = "emu-other-share-source";
        match kind {
            Kind::DbSnapshot => an_instance(&router, "the source instance", source).await,
            Kind::DbClusterSnapshot => a_cluster(&router, "the source cluster", source).await,
        }
        let snapshot = "emu-other-share-snap";
        let case = format!("{kind:?} — accounts other than the owner");
        an_available_snapshot(&router, &case, kind, source, snapshot).await;

        let answered = expect_ok(&case, authorize(&router, kind, snapshot, OTHER_ACCOUNTS).await);
        let expected: Vec<String> = OTHER_ACCOUNTS
            .iter()
            .map(|value| (*value).to_string())
            .collect();
        let echoed = restore_values(&case, &answered);
        assert_eq!(
            echoed, expected,
            "case {case:?}: expected {expected:?} authorized, got {echoed:?}"
        );
        let reported = read_back(&router, &case, kind, snapshot).await;
        let stored = restore_values(&case, &reported);
        assert_eq!(
            stored, expected,
            "case {case:?}: expected a later read to report {expected:?}, got {stored:?}"
        );
    }
}
