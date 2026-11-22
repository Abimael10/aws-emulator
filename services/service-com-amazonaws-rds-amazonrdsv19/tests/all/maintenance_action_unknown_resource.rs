// SPDX-License-Identifier: CC0-1.0

//! That the `ApplyPendingMaintenanceAction` refusal for an unresolvable `ResourceIdentifier`
//! depends on nothing but the value: not on what the store holds, and not on how often it is
//! asked.
//!
//! RULE: Amazon RDS judges `ResourceIdentifier` as a parameter VALUE rather than resolving it
//! as a resource, so a value that resolves to no resource of the account is refused with
//! `InvalidParameterValue` (HTTP 400) and `Unknown resource: ` followed by that value — a
//! refusal, so it creates nothing, and the SAME refusal whether the account holds no resource
//! at all or several.
//!
//! The shapes such a value can take are the sibling suite
//! `all/maintenance_action_absent_resource.rs`; what this one varies is everything AROUND the
//! value: the store (empty, then holding instances and a cluster), the repetition (the same
//! call again, and again), and whether a resolving value in the very same emulator is still
//! served afterwards.
//!
//! Every call goes over the wire through the harness `db_instances.rs` keeps.

use smithy_server_runtime::MultiServiceRouter;

use crate::db_cluster_subject_operations::create_available_cluster;
use crate::db_instances::{
    call, emulator, expect_error, expect_ok, text, texts, wait_for_settle, REGION,
};
// The rule's own suite owns the wording and the call; this one reads both from there so the
// sentence is stated in exactly one place.
use crate::maintenance_action_absent_resource::{
    apply, ACCOUNT, INVALID_PARAMETER_VALUE, UNKNOWN_RESOURCE,
};

/// The one value every call below carries: a well-formed DB instance ARN of this account and
/// region whose identifier is never created.
fn unresolvable() -> String {
    format!("arn:aws:rds:{REGION}:{ACCOUNT}:db:emu-maint-steady-absent")
}

/// Assert the one refusal: its status, its code and its whole sentence.
async fn expect_unknown(router: &MultiServiceRouter, case: &str, resource: &str) {
    let body = expect_error(
        case,
        apply(router, resource).await,
        400,
        INVALID_PARAMETER_VALUE,
    );
    let message = text(&body, "Message");
    assert_eq!(
        message,
        format!("{UNKNOWN_RESOURCE}{resource}"),
        "case {case:?}: expected the refusal to echo {resource:?}, got {message:?}"
    );
}

#[tokio::test]
async fn the_refusal_is_the_same_whatever_the_store_holds_and_however_often_it_is_asked() {
    let router = emulator();
    let absent = unresolvable();

    // An EMPTY store: nothing resolves, and the answer is already the refusal rather than a
    // complaint about an account that holds nothing.
    expect_unknown(&router, "an empty store", &absent).await;
    // Asked again, the same answer: a refused call leaves nothing behind that a later call
    // could resolve.
    expect_unknown(&router, "the same call a second time", &absent).await;

    // Now the account holds resources — two DB instances and a DB cluster, none of them the
    // one the value names — and the answer does not move.
    for identifier in ["emu-maint-steady-one", "emu-maint-steady-two"] {
        expect_ok(
            identifier,
            call(
                &router,
                "CreateDBInstance",
                &[
                    ("DBInstanceIdentifier", identifier),
                    ("Engine", "postgres"),
                    ("DBInstanceClass", "db.t3.micro"),
                ],
            )
            .await,
        );
    }
    create_available_cluster(&router, "emu-maint-steady-cluster", "aurora-postgresql", &[]).await;
    wait_for_settle();

    expect_unknown(&router, "a store holding other resources", &absent).await;
    expect_unknown(&router, "that store, asked again", &absent).await;

    // The refusals created nothing: the account still holds exactly the two instances the
    // test created, and the value it refused resolves to no DB instance.
    let page = expect_ok(
        "the instances the account holds",
        call(&router, "DescribeDBInstances", &[("", ""); 0]).await,
    );
    let mut stored = texts(&page, "DBInstanceIdentifier");
    stored.sort();
    assert_eq!(
        stored,
        vec![
            "emu-maint-steady-one".to_string(),
            "emu-maint-steady-two".to_string()
        ],
        "a refused maintenance action must store nothing, got {page}"
    );
    expect_error(
        "describe the instance the refused value named",
        call(
            &router,
            "DescribeDBInstances",
            &[("DBInstanceIdentifier", "emu-maint-steady-absent")],
        )
        .await,
        404,
        "DBInstanceNotFound",
    );

    // And a value that DOES resolve is served in the same emulator, so the refusals above
    // are about the value and not about the call.
    let resolved = expect_ok(
        "a stored instance's own ARN",
        apply(
            &router,
            &format!("arn:aws:rds:{REGION}:{ACCOUNT}:db:emu-maint-steady-one"),
        )
        .await,
    );
    assert_eq!(
        text(&resolved, "ResourceIdentifier"),
        format!("arn:aws:rds:{REGION}:{ACCOUNT}:db:emu-maint-steady-one"),
        "expected the report to name the resolved resource, got {resolved}"
    );
}
