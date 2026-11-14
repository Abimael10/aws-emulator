// SPDX-License-Identifier: CC0-1.0

//! What `ApplyPendingMaintenanceAction` answers a `ResourceIdentifier` that names no
//! resource of the account.
//!
//! RULE: the member carries "the RDS Amazon Resource Name (ARN) of the resource that the
//! pending maintenance action applies to", so a value that resolves to no resource is
//! reported ABSENT under the fault this operation's own model declares for exactly that —
//! `ResourceNotFoundFault`, "The specified resource ID was not found", HTTP 404
//! (`smithy_query(query="operation",operation="ApplyPendingMaintenanceAction")`) — carrying
//! the fixed text `Resource not found: ` and the resource the value addresses: the resource
//! identifier an RDS ARN carries, or the value itself when it is no ARN of this service. The
//! answer does not depend on which resource TYPE the value names, nor on whether it is an
//! ARN at all: one condition, one fault.
//!
//! The rows span what the rule generalises over: the resource type the ARN names (a DB
//! instance, a DB cluster, a snapshot, a reserved-instance id), the account and the region
//! the ARN names, another vendor's ARN, and values that are no ARN at all (a bare name, an
//! empty-looking word, a near-miss of a stored identifier). A resolving identifier is the
//! control: it is answered with that resource's own pending-action report, so the rows above
//! are about the identifier and not about the call.
//!
//! Every call goes over the wire through the harness `db_instances.rs` keeps, and the
//! instance the control resolves against is stood up through the same implemented operations
//! a client would use.

use smithy_server_runtime::MultiServiceRouter;

use crate::db_instances::{call, emulator, expect_error, expect_ok, text, wait_for_settle, REGION};

/// The account every ARN this service mints belongs to.
const ACCOUNT: &str = "000000000000";

/// The code and the fixed half of the sentence an absent resource is reported with, written
/// out here rather than read off the crate under test.
const RESOURCE_NOT_FOUND_FAULT: &str = "ResourceNotFoundFault";
const RESOURCE_NOT_FOUND: &str = "Resource not found: ";

/// The DB instance this suite stands up, so every row below fails for the identifier it
/// names rather than because the account holds nothing at all.
const STORED: &str = "emu-maint-absent-stored";

/// `ApplyPendingMaintenanceAction` over `resource`, with one documented action and opt-in
/// pairing.
async fn apply(router: &MultiServiceRouter, resource: &str) -> (u16, String) {
    call(
        router,
        "ApplyPendingMaintenanceAction",
        &[
            ("ResourceIdentifier", resource),
            ("ApplyAction", "system-update"),
            ("OptInType", "immediate"),
        ],
    )
    .await
}

#[tokio::test]
async fn every_identifier_that_names_no_resource_is_reported_absent() {
    let router = emulator();

    expect_ok(
        "the one instance the account holds",
        call(
            &router,
            "CreateDBInstance",
            &[
                ("DBInstanceIdentifier", STORED),
                ("Engine", "postgres"),
                ("DBInstanceClass", "db.t3.micro"),
            ],
        )
        .await,
    );
    wait_for_settle();

    // (case, the identifier, the resource the report must name)
    let rows: Vec<(&str, String, String)> = vec![
        (
            "a DB instance ARN for an instance never created",
            format!("arn:aws:rds:{REGION}:{ACCOUNT}:db:emu-maint-absent-db"),
            "emu-maint-absent-db".to_string(),
        ),
        (
            "a DB cluster ARN for a cluster never created",
            format!("arn:aws:rds:{REGION}:{ACCOUNT}:cluster:emu-maint-absent-cluster"),
            "emu-maint-absent-cluster".to_string(),
        ),
        (
            "a DB snapshot ARN, a resource type that carries no maintenance action",
            format!("arn:aws:rds:{REGION}:{ACCOUNT}:snapshot:emu-maint-absent-snap"),
            "emu-maint-absent-snap".to_string(),
        ),
        (
            "a reserved DB instance ARN",
            format!("arn:aws:rds:{REGION}:{ACCOUNT}:ri:emu-maint-absent-ri"),
            "emu-maint-absent-ri".to_string(),
        ),
        (
            "the stored instance's ARN in another account",
            format!("arn:aws:rds:{REGION}:210987654321:db:{STORED}"),
            STORED.to_string(),
        ),
        (
            "the stored instance's ARN in another region",
            format!("arn:aws:rds:eu-west-1:{ACCOUNT}:db:{STORED}"),
            STORED.to_string(),
        ),
        (
            "another vendor's ARN, which is no ARN of this service",
            format!("arn:aws:ec2:{REGION}:{ACCOUNT}:instance:i-0a1b2c3d4e5f60718"),
            format!("arn:aws:ec2:{REGION}:{ACCOUNT}:instance:i-0a1b2c3d4e5f60718"),
        ),
        (
            "the stored instance's bare name, which is no ARN",
            STORED.to_string(),
            STORED.to_string(),
        ),
        (
            "a near-miss of the stored instance's ARN",
            format!("arn:aws:rds:{REGION}:{ACCOUNT}:db:{STORED}-2"),
            format!("{STORED}-2"),
        ),
        (
            "a word of no identifier shape at all",
            "not-an-rds-resource".to_string(),
            "not-an-rds-resource".to_string(),
        ),
    ];

    for (case, resource, named) in &rows {
        let body = expect_error(
            case,
            apply(&router, resource).await,
            404,
            RESOURCE_NOT_FOUND_FAULT,
        );
        let message = text(&body, "Message");
        assert_eq!(
            message,
            format!("{RESOURCE_NOT_FOUND}{named}"),
            "case {case:?}: expected the report to name {named:?}, got {message:?}"
        );
    }

    // The control: the stored instance's own ARN resolves, and is answered with its
    // pending-action report rather than with the fault above.
    let resolved = expect_ok(
        "the stored instance's ARN",
        apply(&router, &format!("arn:aws:rds:{REGION}:{ACCOUNT}:db:{STORED}")).await,
    );
    assert_eq!(
        text(&resolved, "ResourceIdentifier"),
        format!("arn:aws:rds:{REGION}:{ACCOUNT}:db:{STORED}"),
        "expected the report to name the resolved resource, got {resolved}"
    );
}
