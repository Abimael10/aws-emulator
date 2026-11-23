// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for `DescribeDBSnapshotAttributes` and `ModifyDBSnapshotAttribute`, the
//! two operations that read and write a DB snapshot's `restore` attribute.
//!
//! Every call goes over the wire through `db_instances::emulator`, the one mounting helper
//! this test target has, as a real awsQuery form POST carrying the SigV4 credential scope
//! the router keys on. The snapshots the calls act on are TAKEN over the wire too, by
//! `CreateDBSnapshot` and by the final snapshot `DeleteDBInstance` takes, so every assertion
//! reads state this emulator computed rather than a record planted behind the API.
//!
//! The rule under test: a manual DB snapshot carries ONE attribute, `restore`, whose values
//! are the Amazon Web Services accounts authorized to copy or restore it — a 12-digit
//! account id, or `all`, which "means that it can be copied or restored by all Amazon Web
//! Services accounts". `ModifyDBSnapshotAttribute` adds to and removes from that set and
//! answers with it as it then stands; `DescribeDBSnapshotAttributes` reports the same set,
//! always naming the attribute and reporting an empty value list for a snapshot nobody is
//! authorized for. Neither invents a snapshot: an identifier the Region holds no DB snapshot
//! under is the modelled `DBSnapshotNotFoundFault` (HTTP 404) on both, and a snapshot that
//! cannot be shared at all — one still being taken, and one the service took for an
//! instance's automated backups — is the modelled `InvalidDBSnapshotStateFault` (HTTP 400).
//!
//! The tables span what the rule generalises over: how the snapshot was taken (a caller's
//! `CreateDBSnapshot` and the final snapshot of a `DeleteDBInstance`), the source instance's
//! engine, the form the identifier takes (the identifier the snapshot was taken under and
//! its ARN), the CLASS of value (an account id, several account ids, and the `all` that makes
//! a snapshot public), the direction of the change (added, removed, both in one call, and
//! added twice over), the COUNT of authorized accounts up to and one past the published
//! sharing quota, and the snapshot's own state.

use service_com_amazonaws_rds_amazonrdsv19::state::MAX_SNAPSHOT_SHARED_ACCOUNTS;
use smithy_server_runtime::MultiServiceRouter;

use crate::db_instances::{
    call, call_in, emulator, expect_error, expect_ok, optional_text, subtrees, text, texts, REGION,
};
use crate::lifecycle_poll_loops::poll_until;

/// `[(name, value.to_string()), ..]` — the query parameters of one call.
macro_rules! params {
    ($($name:expr => $value:expr),* $(,)?) => {
        [$(($name, $value.to_string())),*]
    };
}

/// Which call took the snapshot a row works on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TakenBy {
    /// `CreateDBSnapshot`, the snapshot a caller asks for.
    ACaller,
    /// The final snapshot `DeleteDBInstance` takes on the way out, which outlives the
    /// instance it was taken of.
    TheInstancesDelete,
}

/// One snapshot the sharing table below is replayed against.
struct SnapshotCase {
    case: &'static str,
    instance: &'static str,
    engine: &'static str,
    snapshot: &'static str,
    taken_by: TakenBy,
}

const SNAPSHOT_CASES: &[SnapshotCase] = &[
    SnapshotCase {
        case: "a snapshot a caller asked for, of a MySQL instance",
        instance: "emu-dsa-mysql",
        engine: "mysql",
        snapshot: "emu-dsa-mysql-snap",
        taken_by: TakenBy::ACaller,
    },
    SnapshotCase {
        case: "the final snapshot a PostgreSQL instance's own delete took",
        instance: "emu-dsa-postgres",
        engine: "postgres",
        snapshot: "emu-dsa-postgres-final",
        taken_by: TakenBy::TheInstancesDelete,
    },
];

/// One row of the sharing table, applied IN ORDER to one snapshot: each row reads the
/// attribute the row before it left, which is what makes the set semantics observable.
struct ShareCase {
    case: &'static str,
    /// `ValuesToAdd`.
    add: &'static [&'static str],
    /// `ValuesToRemove`.
    remove: &'static [&'static str],
    /// The `restore` values the snapshot carries afterwards, in the order a report gives
    /// them — the set's own ascending order, so `all` comes after every account id.
    authorized: &'static [&'static str],
}

/// The account ids below are well-formed 12-digit ids (`ACCOUNT_ID_DIGITS`) that belong to
/// nobody: the values' SHAPE is what the rule is about, never the accounts themselves.
const SHARE_CASES: &[ShareCase] = &[
    ShareCase {
        case: "one account authorized",
        add: &["111122223333"],
        remove: &[],
        authorized: &["111122223333"],
    },
    ShareCase {
        case: "a second account authorized beside the first",
        add: &["444455556666"],
        remove: &[],
        authorized: &["111122223333", "444455556666"],
    },
    ShareCase {
        case: "the same account authorized twice over",
        add: &["111122223333"],
        remove: &[],
        authorized: &["111122223333", "444455556666"],
    },
    ShareCase {
        case: "two accounts in one call",
        add: &["777788889999", "222233334444"],
        remove: &[],
        authorized: &[
            "111122223333",
            "222233334444",
            "444455556666",
            "777788889999",
        ],
    },
    ShareCase {
        case: "all, which makes the snapshot public",
        add: &["all"],
        remove: &[],
        authorized: &[
            "111122223333",
            "222233334444",
            "444455556666",
            "777788889999",
            "all",
        ],
    },
    ShareCase {
        case: "one account de-authorized",
        add: &[],
        remove: &["444455556666"],
        authorized: &["111122223333", "222233334444", "777788889999", "all"],
    },
    ShareCase {
        case: "all removed, which makes the snapshot private again",
        add: &[],
        remove: &["all"],
        authorized: &["111122223333", "222233334444", "777788889999"],
    },
    ShareCase {
        case: "one account authorized and another de-authorized in one call",
        add: &["555566667777"],
        remove: &["222233334444"],
        authorized: &["111122223333", "555566667777", "777788889999"],
    },
    ShareCase {
        case: "an account removed that was never authorized",
        add: &[],
        remove: &["999988887777"],
        authorized: &["111122223333", "555566667777", "777788889999"],
    },
    ShareCase {
        case: "every value removed, leaving the attribute empty",
        add: &[],
        remove: &["111122223333", "555566667777", "777788889999"],
        authorized: &[],
    },
];

/// `CreateDBInstance` polled until the instance reports `available` — the status a snapshot
/// can be taken from.
async fn available_instance(
    router: &MultiServiceRouter,
    case: &str,
    identifier: &str,
    engine: &str,
) {
    expect_ok(
        case,
        call(
            router,
            "CreateDBInstance",
            &params![
                "DBInstanceIdentifier" => identifier,
                "Engine" => engine,
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
            status == 200 && optional_text(body, "DBInstanceStatus").as_deref() == Some("available")
        },
    )
    .await;
}

/// Take the row's snapshot, by whichever call the row says takes it.
async fn take_the_snapshot(router: &MultiServiceRouter, row: &SnapshotCase) {
    match row.taken_by {
        TakenBy::ACaller => expect_ok(
            row.case,
            call(
                router,
                "CreateDBSnapshot",
                &params![
                    "DBInstanceIdentifier" => row.instance,
                    "DBSnapshotIdentifier" => row.snapshot,
                ],
            )
            .await,
        ),
        TakenBy::TheInstancesDelete => expect_ok(
            row.case,
            call(
                router,
                "DeleteDBInstance",
                &params![
                    "DBInstanceIdentifier" => row.instance,
                    "FinalDBSnapshotIdentifier" => row.snapshot,
                ],
            )
            .await,
        ),
    };
}

/// The `<DBSnapshot>` subtree of `snapshot`, polled for until the snapshot reports
/// `available`: being taken is a change the clock owes, so the loop waits for the CONDITION
/// rather than for any span of time.
async fn available_snapshot(router: &MultiServiceRouter, case: &str, snapshot: &str) -> String {
    let read = params!["DBSnapshotIdentifier" => snapshot];
    let (body, _) = poll_until(
        case,
        "the DB snapshot reaching available",
        || call(router, "DescribeDBSnapshots", &read),
        |status, body| status == 200 && optional_text(body, "Status").as_deref() == Some("available"),
    )
    .await;
    body
}

/// `DescribeDBSnapshotAttributes` for `identifier`, asserted served.
async fn describe_attributes(router: &MultiServiceRouter, case: &str, identifier: &str) -> String {
    expect_ok(
        case,
        call(
            router,
            "DescribeDBSnapshotAttributes",
            &params!["DBSnapshotIdentifier" => identifier],
        )
        .await,
    )
}

/// `ModifyDBSnapshotAttribute` of the `restore` attribute of `identifier`, adding and
/// removing the values given, as the HTTP status and body it answered.
async fn modify_attribute(
    router: &MultiServiceRouter,
    identifier: &str,
    add: &[&str],
    remove: &[&str],
) -> (u16, String) {
    let mut request: Vec<(String, String)> = vec![
        ("DBSnapshotIdentifier".to_string(), identifier.to_string()),
        ("AttributeName".to_string(), "restore".to_string()),
    ];
    for (index, value) in add.iter().enumerate() {
        request.push((
            format!("ValuesToAdd.AttributeValue.{}", index + 1),
            (*value).to_string(),
        ));
    }
    for (index, value) in remove.iter().enumerate() {
        request.push((
            format!("ValuesToRemove.AttributeValue.{}", index + 1),
            (*value).to_string(),
        ));
    }
    call(router, "ModifyDBSnapshotAttribute", &request).await
}

/// The `restore` values a `DBSnapshotAttributesResult` carries, asserted to be the ONE
/// attribute AWS defines for a DB snapshot, reported for `snapshot`.
fn restore_values(case: &str, body: &str, snapshot: &str) -> Vec<String> {
    assert_eq!(
        texts(body, "DBSnapshotIdentifier"),
        vec![snapshot.to_string()],
        "case {case:?}: the result must name the snapshot it is about, got {body}"
    );
    assert_eq!(
        texts(body, "AttributeName"),
        vec!["restore".to_string()],
        "case {case:?}: `restore` is the one attribute a DB snapshot carries, got {body}"
    );
    texts(body, "AttributeValue")
}

#[tokio::test]
async fn the_restore_attribute_is_written_by_the_modify_and_read_back_by_the_describe() {
    for row in SNAPSHOT_CASES {
        // Each row gets its own emulator, so one row's snapshots cannot be read as another's.
        let router = emulator();
        available_instance(&router, row.case, row.instance, row.engine).await;
        take_the_snapshot(&router, row).await;
        let described = available_snapshot(&router, row.case, row.snapshot).await;
        let arn = text(&described, "DBSnapshotArn");

        // A snapshot nobody has been authorized for carries the attribute with no values:
        // that is how a caller reads "shared with no account", and it is not the same answer
        // as "this emulator does not know".
        let before = describe_attributes(&router, row.case, row.snapshot).await;
        assert!(
            restore_values(row.case, &before, row.snapshot).is_empty(),
            "case {:?}: a snapshot shared with nobody carries no values, got {before}",
            row.case
        );

        for share in SHARE_CASES {
            let case = format!("{} — {}", row.case, share.case);
            let expected: Vec<String> = share
                .authorized
                .iter()
                .map(|value| (*value).to_string())
                .collect();
            let answered = expect_ok(
                &case,
                modify_attribute(&router, row.snapshot, share.add, share.remove).await,
            );
            assert_eq!(
                restore_values(&case, &answered, row.snapshot),
                expected,
                "case {case:?}: the modify must answer the attribute as it now stands, \
                 got {answered}"
            );
            // The write is in STATE, not only in the answer: the read reports the same set.
            let read_back = describe_attributes(&router, &case, row.snapshot).await;
            assert_eq!(
                restore_values(&case, &read_back, row.snapshot),
                expected,
                "case {case:?}: the describe must report what the modify stored, \
                 got {read_back}"
            );
            // And the ARN names the same snapshot as the identifier does, so the two
            // identifier forms cannot report different attributes.
            let by_arn = describe_attributes(&router, &case, &arn).await;
            assert_eq!(
                restore_values(&case, &by_arn, row.snapshot),
                expected,
                "case {case:?}: the snapshot's ARN must name the same snapshot, got {by_arn}"
            );
        }
    }
}

/// Every operation that names a DB snapshot's attributes, as `(action, the other members the
/// model requires)` (`smithy_query(query="operation",
/// operation="DescribeDBSnapshotAttributes,ModifyDBSnapshotAttribute")`).
const ATTRIBUTE_OPERATIONS: [(&str, &[(&str, &str)]); 2] = [
    ("DescribeDBSnapshotAttributes", &[]),
    (
        "ModifyDBSnapshotAttribute",
        &[("AttributeName", "restore"), ("ValuesToAdd.AttributeValue.1", "111122223333")],
    ),
];

#[tokio::test]
async fn both_attribute_operations_refuse_an_identifier_that_names_no_snapshot() {
    let case = "an identifier that names no snapshot";
    let router = emulator();
    available_instance(&router, case, "emu-dsa-absent", "mysql").await;
    expect_ok(
        case,
        call(
            &router,
            "CreateDBSnapshot",
            &params![
                "DBInstanceIdentifier" => "emu-dsa-absent",
                "DBSnapshotIdentifier" => "emu-dsa-absent-snap",
            ],
        )
        .await,
    );
    let described = available_snapshot(&router, case, "emu-dsa-absent-snap").await;
    let arn = text(&described, "DBSnapshotArn");
    // The same ARN with another Region in it: a well-formed ARN of a snapshot this Region
    // does not hold, which is a different row from a plain identifier that names nothing.
    let foreign_arn = arn.replace(REGION, "eu-west-1");

    // Take and delete a second snapshot, so one row names an identifier that DID name a
    // snapshot: a deleted snapshot is as absent as one never taken.
    expect_ok(
        case,
        call(
            &router,
            "CreateDBSnapshot",
            &params![
                "DBInstanceIdentifier" => "emu-dsa-absent",
                "DBSnapshotIdentifier" => "emu-dsa-absent-gone",
            ],
        )
        .await,
    );
    available_snapshot(&router, case, "emu-dsa-absent-gone").await;
    expect_ok(
        case,
        call(
            &router,
            "DeleteDBSnapshot",
            &params!["DBSnapshotIdentifier" => "emu-dsa-absent-gone"],
        )
        .await,
    );

    // (case, the identifier to send, the Region to sign the request for)
    let rows: [(&str, &str, &str); 5] = [
        ("never taken", "emu-dsa-never-taken", REGION),
        ("already deleted", "emu-dsa-absent-gone", REGION),
        // The identifier of the DB INSTANCE the snapshot was taken of: a real resource of
        // this Region, and no DB snapshot.
        ("the source DB instance's identifier", "emu-dsa-absent", REGION),
        ("an ARN naming another Region", &foreign_arn, REGION),
        // RDS is regional: the snapshot IS stored, just not in the Region signed for.
        ("the right identifier in another Region", "emu-dsa-absent-snap", "eu-west-1"),
    ];

    for (row, identifier, region) in rows {
        for (action, extra) in ATTRIBUTE_OPERATIONS {
            let label = format!("{action}: {row}");
            let mut request: Vec<(&str, &str)> = vec![("DBSnapshotIdentifier", identifier)];
            request.extend_from_slice(extra);
            let refused = expect_error(
                &label,
                call_in(&router, region, action, &request).await,
                404,
                // The wire code is the shape's `awsQueryError` code, which drops the
                // `Fault` suffix (`smithy_query`: `DBSnapshotNotFound`).
                "DBSnapshotNotFound",
            );
            assert!(
                text(&refused, "Message").contains(identifier),
                "case {label:?}: the refusal must name the snapshot it did not find, got {:?}",
                text(&refused, "Message")
            );
        }
    }

    // None of the refusals above wrote anything: the snapshot that IS stored is still shared
    // with nobody.
    let attributes = describe_attributes(&router, case, "emu-dsa-absent-snap").await;
    assert!(
        restore_values(case, &attributes, "emu-dsa-absent-snap").is_empty(),
        "case {case:?}: a refused call must store nothing, got {attributes}"
    );
}

#[tokio::test]
async fn sharing_refuses_values_outside_the_documented_rule_and_stops_at_the_quota() {
    let case = "values outside the documented rule";
    let router = emulator();
    available_instance(&router, case, "emu-dsa-values", "mariadb").await;
    expect_ok(
        case,
        call(
            &router,
            "CreateDBSnapshot",
            &params![
                "DBInstanceIdentifier" => "emu-dsa-values",
                "DBSnapshotIdentifier" => "emu-dsa-values-snap",
            ],
        )
        .await,
    );
    available_snapshot(&router, case, "emu-dsa-values-snap").await;

    // (case, the members to send beside the snapshot identifier)
    let refused: [(&str, &[(&str, &str)]); 7] = [
        // `AttributeName`: `restore` is the only attribute AWS defines for a snapshot.
        (
            "an attribute other than restore",
            &[("AttributeName", "share")],
        ),
        (
            "the right attribute name in the wrong case",
            &[("AttributeName", "RESTORE")],
        ),
        // A value is an account id or `all`, and nothing else.
        (
            "a value that is not an account id",
            &[
                ("AttributeName", "restore"),
                ("ValuesToAdd.AttributeValue.1", "not-an-account"),
            ],
        ),
        (
            "an account id one digit short",
            &[
                ("AttributeName", "restore"),
                ("ValuesToAdd.AttributeValue.1", "11112222333"),
            ],
        ),
        (
            "an account id one digit long",
            &[
                ("AttributeName", "restore"),
                ("ValuesToAdd.AttributeValue.1", "1111222233334"),
            ],
        ),
        (
            "a removal that is not an account id either",
            &[
                ("AttributeName", "restore"),
                ("ValuesToRemove.AttributeValue.1", "everyone"),
            ],
        ),
        (
            // The result would depend on which of the two lists were applied first.
            "the same value added and removed",
            &[
                ("AttributeName", "restore"),
                ("ValuesToAdd.AttributeValue.1", "111122223333"),
                ("ValuesToRemove.AttributeValue.1", "111122223333"),
            ],
        ),
    ];

    for (row, members) in refused {
        let mut request: Vec<(&str, &str)> =
            vec![("DBSnapshotIdentifier", "emu-dsa-values-snap")];
        request.extend_from_slice(members);
        expect_error(
            row,
            call(&router, "ModifyDBSnapshotAttribute", &request).await,
            400,
            "ValidationException",
        );
    }

    // Nothing above reached the store: the attribute is still empty.
    let attributes = describe_attributes(&router, case, "emu-dsa-values-snap").await;
    assert!(
        restore_values(case, &attributes, "emu-dsa-values-snap").is_empty(),
        "case {case:?}: a refused call must store nothing, got {attributes}"
    );

    // One account per published slot is authorized, and the one past the limit is the
    // modelled quota fault; the limit itself is `state::MAX_SNAPSHOT_SHARED_ACCOUNTS`.
    for index in 0..MAX_SNAPSHOT_SHARED_ACCOUNTS {
        let account = format!("{:012}", 100_000_000_000_u64 + index as u64);
        let filling = format!("filling the sharing quota, slot {index}");
        let body = expect_ok(
            &filling,
            modify_attribute(&router, "emu-dsa-values-snap", &[account.as_str()], &[]).await,
        );
        assert_eq!(
            restore_values(&filling, &body, "emu-dsa-values-snap").len(),
            index + 1,
            "case {filling:?}: every account authorized so far must be reported, got {body}"
        );
    }
    expect_error(
        "one account past the quota",
        modify_attribute(&router, "emu-dsa-values-snap", &["999988887777"], &[]).await,
        400,
        // The wire code is the shape's `awsQueryError` code, which drops the `Fault` suffix
        // (`smithy_query`: `SharedSnapshotQuotaExceeded`).
        "SharedSnapshotQuotaExceeded",
    );
    let still = describe_attributes(&router, case, "emu-dsa-values-snap").await;
    assert_eq!(
        restore_values(case, &still, "emu-dsa-values-snap").len(),
        MAX_SNAPSHOT_SHARED_ACCOUNTS,
        "case {case:?}: the account past the quota must not have been stored, got {still}"
    );
}

#[tokio::test]
async fn a_snapshot_that_cannot_be_shared_is_the_modelled_state_fault() {
    let case = "a snapshot that cannot be shared";
    let router = emulator();
    available_instance(&router, case, "emu-dsa-state", "mysql").await;
    expect_ok(
        case,
        call(
            &router,
            "CreateDBSnapshot",
            &params![
                "DBInstanceIdentifier" => "emu-dsa-state",
                "DBSnapshotIdentifier" => "emu-dsa-state-snap",
            ],
        )
        .await,
    );

    // Inside the span the clock owes the snapshot it is still being taken, so it cannot be
    // shared yet — while the REPORT of its attributes is served throughout, since reading an
    // attribute asks nothing of the snapshot's state.
    let refused = expect_error(
        "a snapshot still being taken",
        modify_attribute(&router, "emu-dsa-state-snap", &["111122223333"], &[]).await,
        400,
        "InvalidDBSnapshotState",
    );
    assert!(
        text(&refused, "Message").contains("emu-dsa-state-snap"),
        "case {case:?}: the refusal must name the snapshot, got {:?}",
        text(&refused, "Message")
    );
    let while_taken = describe_attributes(&router, case, "emu-dsa-state-snap").await;
    assert!(
        restore_values(case, &while_taken, "emu-dsa-state-snap").is_empty(),
        "case {case:?}: the refused share must have stored nothing, got {while_taken}"
    );

    available_snapshot(&router, case, "emu-dsa-state-snap").await;

    // The snapshot the instance's own automated backups took is not the account's to share:
    // the documented way to share its data is to copy it into a manual snapshot first.
    let automated_read = params![
        "DBInstanceIdentifier" => "emu-dsa-state",
        "SnapshotType" => "automated",
    ];
    let (automated_body, _) = poll_until(
        case,
        "the instance's automated snapshot being reported",
        || call(&router, "DescribeDBSnapshots", &automated_read),
        |status, body| status == 200 && !subtrees(body, "DBSnapshot").is_empty(),
    )
    .await;
    let automated = text(&subtrees(&automated_body, "DBSnapshot")[0], "DBSnapshotIdentifier");
    let refused = expect_error(
        "an automated snapshot",
        modify_attribute(&router, &automated, &["111122223333"], &[]).await,
        400,
        "InvalidDBSnapshotState",
    );
    assert!(
        text(&refused, "Message").contains(&automated),
        "case {case:?}: the refusal must name the automated snapshot, got {:?}",
        text(&refused, "Message")
    );
    // Its attributes are still reported, and they are empty: the refusal is about sharing it,
    // not about reading it.
    let automated_attributes = describe_attributes(&router, case, &automated).await;
    assert!(
        restore_values(case, &automated_attributes, &automated).is_empty(),
        "case {case:?}: an automated snapshot is authorized for nobody, got \
         {automated_attributes}"
    );

    // `all` on the available manual snapshot makes it public, which is what the `public`
    // snapshot type selects — and a COPY of it inherits none of its authorizations.
    expect_ok(
        "making the snapshot public",
        modify_attribute(&router, "emu-dsa-state-snap", &["all"], &[]).await,
    );
    expect_ok(
        "copying the public snapshot",
        call(
            &router,
            "CopyDBSnapshot",
            &params![
                "SourceDBSnapshotIdentifier" => "emu-dsa-state-snap",
                "TargetDBSnapshotIdentifier" => "emu-dsa-state-copy",
            ],
        )
        .await,
    );
    available_snapshot(&router, case, "emu-dsa-state-copy").await;
    let copied = describe_attributes(&router, case, "emu-dsa-state-copy").await;
    assert!(
        restore_values(case, &copied, "emu-dsa-state-copy").is_empty(),
        "case {case:?}: a copy is shared with nobody until it is shared, got {copied}"
    );

    // (case, SnapshotType, the snapshots it selects)
    let type_rows: [(&str, &str, &[&str]); 4] = [
        (
            "manual",
            "manual",
            &["emu-dsa-state-copy", "emu-dsa-state-snap"],
        ),
        ("public", "public", &["emu-dsa-state-snap"]),
        ("automated", "automated", &[automated.as_str()]),
        ("shared", "shared", &[]),
    ];
    for (row, snapshot_type, expected) in type_rows {
        let page = expect_ok(
            row,
            call(
                &router,
                "DescribeDBSnapshots",
                &params!["SnapshotType" => snapshot_type],
            )
            .await,
        );
        let reported = texts(&page, "DBSnapshotIdentifier");
        assert_eq!(
            reported,
            expected
                .iter()
                .map(|identifier| (*identifier).to_string())
                .collect::<Vec<String>>(),
            "case {row:?}: expected {expected:?}, got {reported:?} in {page}"
        );
    }
}
