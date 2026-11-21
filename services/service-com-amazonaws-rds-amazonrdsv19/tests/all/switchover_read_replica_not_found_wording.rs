// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for how `SwitchoverReadReplica` WORDS the DB-instance-not-found fault it
//! models.
//!
//! The rule under test: Amazon RDS words a `DBInstanceNotFoundFault` message PER FRONT DOOR
//! rather than from one crate-wide template, so each operation answers the sentence that
//! operation answers — for `SwitchoverReadReplica` "The DB instance wasn't found: <id>.",
//! whose only variable part is the `DBInstanceIdentifier` the request carried, in the spelling
//! the caller wrote it in (only the lookup folds the identifier, which "is stored as a
//! lowercase string"). The status and the code are the modelled pair: HTTP 404 with the
//! `awsQueryError` code `DBInstanceNotFound`
//! (`smithy_query(query="operation", operation="SwitchoverReadReplica")`).
//!
//! The table spans what the rule generalises over — the shape of an identifier that names no
//! DB instance of the signed region (a fresh one, a near miss of a stored one, the shortest
//! the member admits, mixed case, one at the published 63-character bound) and how it came to
//! name nothing (never created, created only in another region, or created and then deleted) —
//! and the second test pins the "per front door" half of the rule: the sibling role change on
//! the same resource does not answer this sentence.
//!
//! Every call goes over the wire through the harness in `db_instances.rs`, so the assertions
//! read the XML the emulator itself answered with. Nothing sleeps for a transition: the one
//! wait is a poll loop on a REPORTING call, through this test target's `poll_until` helper.

use smithy_server_runtime::MultiServiceRouter;

use crate::db_instances::{call, call_in, emulator, expect_error, expect_ok, text, REGION};
use crate::lifecycle_poll_loops::poll_until;

/// A second region, so an instance that exists but is out of the signed region's scope is a
/// row of the table.
const OTHER_REGION: &str = "ap-south-1";

/// The sentence Amazon RDS answers a `SwitchoverReadReplica` naming no DB instance with,
/// spelled out here so the wording is pinned by the test rather than by the code under test:
/// fixed text, with the identifier taken from the request.
fn not_found_wording(named: &str) -> String {
    format!("The DB instance wasn't found: {named}.")
}

/// `SwitchoverReadReplica` naming one instance, signed for [`REGION`].
async fn switchover(router: &MultiServiceRouter, identifier: &str) -> (u16, String) {
    call(
        router,
        "SwitchoverReadReplica",
        &[("DBInstanceIdentifier", identifier)],
    )
    .await
}

/// `CreateDBInstance` of an Oracle instance in `region` — the engine a switchover applies to,
/// so every refusal below is about the identifier and nothing else.
async fn create_instance(
    router: &MultiServiceRouter,
    region: &str,
    identifier: &str,
) -> (u16, String) {
    call_in(
        router,
        region,
        "CreateDBInstance",
        &[
            ("DBInstanceIdentifier", identifier),
            ("Engine", "oracle-ee"),
            ("LicenseModel", "bring-your-own-license"),
            ("DBInstanceClass", "db.m5.large"),
            ("AllocatedStorage", "20"),
            ("MasterUsername", "appadmin"),
            ("MasterUserPassword", "an-example-password"),
        ],
    )
    .await
}

/// Poll `DescribeDBInstances` in [`REGION`] until it reports `identifier` as `available`.
async fn awaited_instance(router: &MultiServiceRouter, case: &str, identifier: &str) {
    poll_until(
        case,
        &format!("DB instance {identifier}"),
        || async {
            call(
                router,
                "DescribeDBInstances",
                &[("DBInstanceIdentifier", identifier)],
            )
            .await
        },
        |status, body| status == 200 && text(body, "DBInstanceStatus") == "available",
    )
    .await;
}

/// A `DBInstanceIdentifier` of exactly the 63 characters its member admits ("Must contain from
/// 1 to 63 letters, numbers, or hyphens. First character must be a letter",
/// `smithy_query(query="doc", shape="CreateDBInstanceMessage", member="DBInstanceIdentifier")`),
/// so the published bound is one of the rows.
fn identifier_at_the_published_bound() -> String {
    let mut identifier = String::from("s");
    identifier.extend(std::iter::repeat_n('x', 62));
    assert_eq!(
        identifier.chars().count(),
        63,
        "the row at the published bound must be 63 characters, got {}",
        identifier.chars().count()
    );
    identifier
}

#[tokio::test]
async fn an_absent_db_instance_is_refused_with_this_operations_own_wording() {
    let router = emulator();

    // One instance this region holds, so a near miss of its identifier is a row; one that only
    // ANOTHER region holds; and one this region held until a delete removed it.
    let held = "switchover-wording-held";
    expect_ok(
        "the stored instance",
        create_instance(&router, REGION, held).await,
    );
    expect_ok(
        "the other region's instance",
        create_instance(&router, OTHER_REGION, "switchover-wording-elsewhere").await,
    );
    let deleted = "switchover-wording-deleted";
    expect_ok(
        "the instance that is about to be deleted",
        create_instance(&router, REGION, deleted).await,
    );
    awaited_instance(&router, "the instance that is about to be deleted", deleted).await;
    expect_ok(
        "the delete of an instance that exists",
        call(
            &router,
            "DeleteDBInstance",
            &[
                ("DBInstanceIdentifier", deleted),
                ("SkipFinalSnapshot", "true"),
            ],
        )
        .await,
    );
    // A delete reports `deleting` and removes the record when the clock settles it, so the row
    // below waits for the CONDITION it needs: the identifier naming nothing at all.
    poll_until(
        "the instance that is about to be deleted",
        &format!("DB instance {deleted} to be gone"),
        || async {
            call(
                &router,
                "DescribeDBInstances",
                &[("DBInstanceIdentifier", deleted)],
            )
            .await
        },
        |status, _| status == 404,
    )
    .await;

    let at_the_bound = identifier_at_the_published_bound();
    let rows: Vec<(&str, &str)> = vec![
        (
            "an identifier nothing was created under",
            "switchover-wording-absent",
        ),
        (
            "a near miss of a stored identifier",
            "switchover-wording-held-2",
        ),
        ("the shortest identifier the member admits", "s"),
        (
            "an identifier in mixed case, folded only on the way in",
            "Switchover-Wording-Absent-Mixed",
        ),
        (
            "an identifier at the published 63-character bound",
            at_the_bound.as_str(),
        ),
        (
            "an instance only another region holds",
            "switchover-wording-elsewhere",
        ),
        ("an identifier whose instance a delete removed", deleted),
    ];
    for (case, identifier) in rows {
        let refused = expect_error(
            case,
            switchover(&router, identifier).await,
            404,
            "DBInstanceNotFound",
        );
        let message = text(&refused, "Message");
        assert_eq!(
            message,
            not_found_wording(identifier),
            "case {case:?}: expected this operation's own not-found sentence, got {message:?}"
        );
    }

    // The instance this region DOES hold is not refused as absent: it resolves, and is refused
    // for what it is — which is what makes every row above about existence.
    let resolved = expect_error(
        "the stored instance",
        switchover(&router, held).await,
        400,
        "InvalidDBInstanceState",
    );
    let message = text(&resolved, "Message");
    assert_ne!(
        message,
        not_found_wording(held),
        "case {:?}: an instance that exists must not be reported absent, got {message:?}",
        "the stored instance"
    );
}

#[tokio::test]
async fn the_switchovers_not_found_sentence_is_not_the_promotions() {
    let router = emulator();
    let case = "an identifier naming no DB instance";
    // Mixed case and plain, so the comparison does not rest on one spelling.
    for identifier in ["switchover-wording-per-door", "Switchover-Wording-Per-Door"] {
        let switched = text(
            &expect_error(
                case,
                switchover(&router, identifier).await,
                404,
                "DBInstanceNotFound",
            ),
            "Message",
        );
        assert_eq!(
            switched,
            not_found_wording(identifier),
            "case {case:?} / {identifier:?}: expected the switchover's own sentence, got \
             {switched:?}"
        );

        // `PromoteReadReplica` is the other role change on the same resource and models the
        // same fault; Amazon RDS words it differently, so the two must not answer one
        // undifferentiated string. What the promotion's own sentence is belongs to the
        // promotion, which is why only the difference is asserted here.
        let promoted = text(
            &expect_error(
                case,
                call(
                    &router,
                    "PromoteReadReplica",
                    &[("DBInstanceIdentifier", identifier)],
                )
                .await,
                404,
                "DBInstanceNotFound",
            ),
            "Message",
        );
        assert_ne!(
            switched, promoted,
            "case {case:?} / {identifier:?}: each front door words the one fault its own way, \
             both answered {switched:?}"
        );
    }
}
