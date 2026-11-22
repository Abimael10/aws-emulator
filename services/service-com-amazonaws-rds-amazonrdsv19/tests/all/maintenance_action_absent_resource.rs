// SPDX-License-Identifier: CC0-1.0

//! What `ApplyPendingMaintenanceAction` answers a `ResourceIdentifier` that names no
//! resource of the account.
//!
//! RULE: Amazon RDS judges `ResourceIdentifier` as a PARAMETER VALUE rather than resolving
//! it as a resource, so a value that resolves to no resource of the account is refused with
//! the Query API's `InvalidParameterValue` at HTTP 400 and the fixed text
//! `Unknown resource: ` followed by the value the member carried, WHOLE — the ARN itself,
//! never the resource identifier parsed out of it. The answer does not depend on which
//! resource TYPE the value names, nor on whether it is an ARN at all: one condition, one
//! refusal. The not-found fault the operation's model declares
//! (`smithy_query(query="operation",operation="ApplyPendingMaintenanceAction")` lists
//! `ResourceNotFoundFault` at 404) therefore reports no unresolvable identifier here.
//!
//! The rows span what the rule generalises over: the resource type the ARN names (a DB
//! instance, a DB cluster, a snapshot, a reserved-instance id, a cluster snapshot), the
//! partition, the account and the region the ARN names, the length and the case of the
//! identifier inside it, another vendor's ARN, and values that are no ARN at all (a bare
//! name, a near-miss of a stored identifier, a word of no identifier shape, a value carrying
//! the error envelope's own metacharacters). Each row asserts both halves of the sentence:
//! that it echoes the value whole, and that it is not the bare identifier an ARN carries.
//!
//! The refusal is also a REFUSAL: nothing it names comes into existence, so the store still
//! reports exactly the one instance this suite created. A resolving identifier is the
//! control: it is answered with that resource's own pending-action report, so the rows above
//! are about the identifier and not about the call.
//!
//! Every call goes over the wire through the harness `db_instances.rs` keeps, and the
//! instance the control resolves against is stood up through the same implemented operations
//! a client would use.

use smithy_server_runtime::MultiServiceRouter;

use crate::db_instances::{
    call, emulator, expect_error, expect_ok, text, texts, wait_for_settle, REGION,
};

/// The account every ARN this service mints belongs to.
///
/// `pub(crate)` here and below: this suite owns the rule, so the sibling suite that varies
/// what surrounds the value reads the refusal's wording and the call from here rather than
/// re-stating either.
pub(crate) const ACCOUNT: &str = "000000000000";

/// The code and the fixed half of the sentence a value that names no resource is refused
/// with, written out here rather than read off the crate under test.
pub(crate) const INVALID_PARAMETER_VALUE: &str = "InvalidParameterValue";
pub(crate) const UNKNOWN_RESOURCE: &str = "Unknown resource: ";

/// The DB instance this suite stands up, so every row below fails for the identifier it
/// names rather than because the account holds nothing at all.
const STORED: &str = "emu-maint-absent-stored";

/// The longest identifier `CreateDBInstance` documents for a DB instance: 1–63 letters,
/// digits or hyphens (`smithy_query(query="doc",shape="CreateDBInstanceMessage",
/// member="DBInstanceIdentifier")`). Used as the identifier of an ARN that names nothing, so
/// the echo is asserted at the far end of the length the member admits.
fn longest_identifier() -> String {
    let mut identifier = String::from("emu-maint-absent-");
    while identifier.len() < 63 {
        identifier.push('a');
    }
    identifier
}

/// `ApplyPendingMaintenanceAction` over `resource`, with one documented action and opt-in
/// pairing.
pub(crate) async fn apply(router: &MultiServiceRouter, resource: &str) -> (u16, String) {
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
async fn every_identifier_that_names_no_resource_is_refused_as_a_parameter_value() {
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

    let long = longest_identifier();
    // (case, the value the request carries, the identifier an RDS ARN carries inside it)
    let rows: Vec<(&str, String, Option<String>)> = vec![
        (
            "a DB instance ARN for an instance never created",
            format!("arn:aws:rds:{REGION}:{ACCOUNT}:db:emu-maint-absent-db"),
            Some("emu-maint-absent-db".to_string()),
        ),
        (
            "a DB cluster ARN for a cluster never created",
            format!("arn:aws:rds:{REGION}:{ACCOUNT}:cluster:emu-maint-absent-cluster"),
            Some("emu-maint-absent-cluster".to_string()),
        ),
        (
            "a DB snapshot ARN, a resource type that carries no maintenance action",
            format!("arn:aws:rds:{REGION}:{ACCOUNT}:snapshot:emu-maint-absent-snap"),
            Some("emu-maint-absent-snap".to_string()),
        ),
        (
            "a DB cluster snapshot ARN",
            format!("arn:aws:rds:{REGION}:{ACCOUNT}:cluster-snapshot:emu-maint-absent-cs"),
            Some("emu-maint-absent-cs".to_string()),
        ),
        (
            "a reserved DB instance ARN",
            format!("arn:aws:rds:{REGION}:{ACCOUNT}:ri:emu-maint-absent-ri"),
            Some("emu-maint-absent-ri".to_string()),
        ),
        (
            "the stored instance's ARN in another account",
            format!("arn:aws:rds:{REGION}:210987654321:db:{STORED}"),
            Some(STORED.to_string()),
        ),
        (
            "the stored instance's ARN in another region",
            format!("arn:aws:rds:eu-west-1:{ACCOUNT}:db:{STORED}"),
            Some(STORED.to_string()),
        ),
        (
            "the stored instance's ARN in the China partition",
            format!("arn:aws-cn:rds:cn-north-1:{ACCOUNT}:db:{STORED}"),
            Some(STORED.to_string()),
        ),
        (
            "a DB instance ARN in a GovCloud partition",
            format!("arn:aws-us-gov:rds:us-gov-west-1:{ACCOUNT}:db:emu-maint-absent-gov"),
            Some("emu-maint-absent-gov".to_string()),
        ),
        (
            "a DB instance ARN whose identifier is as long as the member admits",
            format!("arn:aws:rds:{REGION}:{ACCOUNT}:db:{long}"),
            Some(long.clone()),
        ),
        (
            "a DB instance ARN whose identifier differs from the stored one only in case",
            format!("arn:aws:rds:{REGION}:{ACCOUNT}:db:EMU-MAINT-ABSENT-OTHER"),
            Some("EMU-MAINT-ABSENT-OTHER".to_string()),
        ),
        (
            "a DB instance ARN with no identifier after its resource type",
            format!("arn:aws:rds:{REGION}:{ACCOUNT}:db:"),
            None,
        ),
        (
            "another vendor's ARN, which is no ARN of this service",
            format!("arn:aws:ec2:{REGION}:{ACCOUNT}:instance:i-0a1b2c3d4e5f60718"),
            None,
        ),
        (
            "the stored instance's bare name, which is no ARN",
            STORED.to_string(),
            None,
        ),
        (
            "a near-miss of the stored instance's ARN",
            format!("arn:aws:rds:{REGION}:{ACCOUNT}:db:{STORED}-2"),
            Some(format!("{STORED}-2")),
        ),
        (
            "a word of no identifier shape at all",
            "not an rds resource?".to_string(),
            None,
        ),
        (
            "a value carrying the error envelope's own metacharacters",
            format!("arn:aws:rds:{REGION}:{ACCOUNT}:db:<probe&echo>"),
            None,
        ),
    ];

    for (case, resource, inner) in &rows {
        let body = expect_error(
            case,
            apply(&router, resource).await,
            400,
            INVALID_PARAMETER_VALUE,
        );
        let message = text(&body, "Message");
        assert_eq!(
            message,
            format!("{UNKNOWN_RESOURCE}{resource}"),
            "case {case:?}: expected the refusal to echo {resource:?} whole, got {message:?}"
        );
        // The second half of the rule: where the value is an ARN, the sentence must not
        // shrink to the resource identifier inside it.
        if let Some(inner) = inner {
            assert_ne!(
                message,
                format!("{UNKNOWN_RESOURCE}{inner}"),
                "case {case:?}: expected the whole value and not the identifier {inner:?}, \
                 got {message:?}"
            );
        }
        // A refusal is typed as the caller's own to correct on this protocol.
        let fault_type = text(&body, "Type");
        assert_eq!(
            fault_type, "Sender",
            "case {case:?}: expected a Sender fault, got {fault_type:?} in {body}"
        );
        // A value the caller influences is ENCODED for the envelope it is carried in rather
        // than pasted into it, so the document a client parses still has one `Message`
        // element whose text is the value.
        if resource.contains('<') || resource.contains('&') {
            assert!(
                body.contains("&lt;") && body.contains("&amp;"),
                "case {case:?}: expected the envelope to escape the value's metacharacters, \
                 got {body}"
            );
        }
    }

    // The control: the stored instance's own ARN resolves, and is answered with its
    // pending-action report rather than with the refusal above.
    let resolved = expect_ok(
        "the stored instance's ARN",
        apply(&router, &format!("arn:aws:rds:{REGION}:{ACCOUNT}:db:{STORED}")).await,
    );
    assert_eq!(
        text(&resolved, "ResourceIdentifier"),
        format!("arn:aws:rds:{REGION}:{ACCOUNT}:db:{STORED}"),
        "expected the report to name the resolved resource, got {resolved}"
    );

    // And every refusal above was a refusal: the store still holds just the one instance.
    let page = expect_ok(
        "the instances the account holds after the refusals",
        call(&router, "DescribeDBInstances", &[("", ""); 0]).await,
    );
    assert_eq!(
        texts(&page, "DBInstanceIdentifier"),
        vec![STORED.to_string()],
        "a refused maintenance action must store nothing, got {page}"
    );
}
