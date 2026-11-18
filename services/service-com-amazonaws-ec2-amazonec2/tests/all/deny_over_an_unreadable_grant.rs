// SPDX-License-Identifier: CC0-1.0

//! What an explicit `Deny` on an assumed role does when that role ALSO carries a grant this
//! emulator cannot read — the position every `cdk bootstrap` lookup role is in, since a
//! bootstrapped role carries AWS-managed policies and a guardrail is then put on it inline.
//!
//! THE RULE UNDER TEST: IAM decides a request on the documents in force on the principal its
//! credential speaks for, and an explicit `Deny` about the request in a document that CAN be
//! read refuses it whatever any `Allow` says ("if a single statement denies the request, the
//! entire request is denied" — IAM, "Determining whether a request is allowed or denied
//! within an account"). An attachment whose document this emulator does not hold is a grant
//! it cannot see, so it lifts only the DEFAULT deny of a request no readable statement allows
//! — it never overturns a deny. So EC2 refuses such a request with HTTP 403
//! `UnauthorizedOperation`, naming the principal, the `ec2:` action, the resource the
//! decision was taken on when there is one, and the rule that withheld the grant, and reads
//! and changes nothing.
//!
//! The rows span what the rule generalises over: the attachments the role carries (none, one
//! whose document this emulator holds, one it does not, two it does not, and both kinds
//! together), how the `Deny` names the action (exactly, under a run wildcard, in another
//! case, as the whole service), which read it is therefore about, and both halves of the
//! decision — an action IAM judges over EVERY resource (the `Describe` family, which supports
//! no resource-level permissions) and one judged against the resource the request ADDRESSES,
//! whose refusal names that resource's ARN.
//!
//! The premise of every row is checked by two rows of its own: a role carrying ONLY the
//! unreadable attachment is served (its grant cannot be read, so the default deny is lifted),
//! while a role carrying only a readable attachment that covers another service's actions is
//! refused — which is what makes the first attachment unreadable rather than merely
//! permissive.
//!
//! Driven through the harness [`super::account_wide_read_authorization`] owns, so the
//! emulator, the role with its documents and the session minted of it are one copy.

use smithy_server_runtime::MultiServiceRouter;

use super::account_wide_read_authorization::{
    assert_read, ec2_as, element, emulator, iam_call, session_of, statement, Expect,
    ACCOUNT_KEY_ID, READS, REGION,
};
use super::ec2_query::{elements, error_code, error_message};

/// An AWS-published managed policy whose document this emulator does not carry: its name is
/// outside the catalog the IAM service holds documents for, so an attachment of it resolves
/// to no document — exactly the position an AWS-managed policy on a bootstrapped role is in.
///
/// Its real document grants `cloudformation:` actions and no `ec2:` action at all, so a read
/// served under it was served because the grant could not be READ and not because it allows
/// the read.
const UNREADABLE_GRANT: &str = "arn:aws:iam::aws:policy/AWSCloudFormationFullAccess";

/// A second one, so a row can carry more than one unreadable attachment; its real document
/// covers `dynamodb:` reads and no `ec2:` action either.
const OTHER_UNREADABLE_GRANT: &str = "arn:aws:iam::aws:policy/AmazonDynamoDBReadOnlyAccess";

/// An AWS-managed policy whose document this emulator DOES hold, and which grants the read
/// verbs of every service: a readable grant beside the unreadable one.
const READABLE_GRANT: &str = "arn:aws:iam::aws:policy/ReadOnlyAccess";

/// An AWS-managed policy whose document this emulator holds and which covers no `ec2:`
/// action at all: the contrast that shows a readable attachment is read.
const READABLE_OTHER_SERVICE_GRANT: &str = "arn:aws:iam::aws:policy/AmazonS3ReadOnlyAccess";

/// EC2's error code for a caller that may not perform the action it asked for.
const UNAUTHORIZED_OPERATION: &str = "UnauthorizedOperation";

/// EC2's fixed opening sentence of an authorization refusal.
const NOT_AUTHORIZED_SENTENCE: &str = "You are not authorized to perform this operation.";

// ---------------------------------------------------------------------------
// An action IAM judges over every resource
// ---------------------------------------------------------------------------

/// One row: the documents in force on the role a session is minted of, and what each of the
/// four [`READS`] must answer for it.
struct Row {
    case: &'static str,
    role: &'static str,
    /// The inline documents put on the role.
    inline: Vec<(&'static str, String)>,
    /// The managed policies attached to it.
    attached: Vec<&'static str>,
    /// What the four [`READS`] answer, in that order.
    expect: [Expect; 4],
}

/// RULE: an explicit `Deny` about the request in an identity-based document this emulator
/// holds refuses the read, whatever else the principal carries — a grant whose document the
/// emulator cannot read included, which lifts only the default deny of a read no readable
/// statement allows.
#[tokio::test]
async fn an_explicit_deny_refuses_a_read_whose_grant_cannot_be_read() {
    let rows = vec![
        Row {
            case: "only an attachment whose document this emulator does not hold",
            role: "unreadable-grant-only-role",
            inline: Vec::new(),
            attached: vec![UNREADABLE_GRANT],
            // The grant cannot be read, so refusing on it would deny a read AWS permits.
            expect: [Expect::Served; 4],
        },
        Row {
            case: "only a readable attachment covering another service",
            role: "readable-other-service-role",
            inline: Vec::new(),
            attached: vec![READABLE_OTHER_SERVICE_GRANT],
            // A document that IS read decides the read: it allows no `ec2:` action.
            expect: [Expect::NoAllow; 4],
        },
        Row {
            case: "an unreadable grant and an exact deny of one read",
            role: "unreadable-grant-exact-deny-role",
            inline: vec![(
                "deny-describe-vpcs",
                statement("Deny", "ec2:DescribeVpcs", "*"),
            )],
            attached: vec![UNREADABLE_GRANT],
            expect: [
                Expect::Served,
                Expect::Served,
                Expect::Served,
                Expect::ExplicitDeny,
            ],
        },
        Row {
            case: "an unreadable grant and a run wildcard over the family",
            role: "unreadable-grant-family-deny-role",
            inline: vec![("deny-describes", statement("Deny", "ec2:Describe*", "*"))],
            attached: vec![UNREADABLE_GRANT],
            expect: [Expect::ExplicitDeny; 4],
        },
        Row {
            case: "an unreadable grant and a deny of the whole service",
            role: "unreadable-grant-service-deny-role",
            inline: vec![("deny-ec2", statement("Deny", "ec2:*", "*"))],
            attached: vec![UNREADABLE_GRANT],
            expect: [Expect::ExplicitDeny; 4],
        },
        Row {
            case: "an unreadable grant, a readable grant and a mixed-case deny of one read",
            role: "both-grants-mixed-case-deny-role",
            inline: vec![(
                "deny-availability-zones",
                statement("Deny", "EC2:describeavailabilityzones", "*"),
            )],
            attached: vec![UNREADABLE_GRANT, READABLE_GRANT],
            expect: [
                Expect::ExplicitDeny,
                Expect::Served,
                Expect::Served,
                Expect::Served,
            ],
        },
        Row {
            case: "two unreadable grants and a deny of one read",
            role: "two-unreadable-grants-role",
            inline: vec![(
                "deny-describe-subnets",
                statement("Deny", "ec2:DescribeSubnets", "*"),
            )],
            attached: vec![UNREADABLE_GRANT, OTHER_UNREADABLE_GRANT],
            expect: [
                Expect::Served,
                Expect::Served,
                Expect::ExplicitDeny,
                Expect::Served,
            ],
        },
    ];

    let router = emulator();
    for row in &rows {
        let (akid, principal) =
            session_of(&router, row.case, row.role, &row.inline, &row.attached).await;
        for (read, expected) in READS.iter().zip(row.expect.iter()) {
            assert_read(&router, row.case, &akid, &principal, read, *expected).await;
        }
    }
}

/// RULE: the deny decides the read as the documents read AT THE INSTANT OF THE CALL, so one
/// stored on a role that carries an unreadable grant refuses the NEXT read made with that
/// role's session — and deleting it serves that read again.
#[tokio::test]
async fn a_deny_stored_beside_an_unreadable_grant_decides_the_next_read() {
    let router = emulator();
    let case = "a deny stored mid-session beside an unreadable grant";
    let role = "mid-session-unreadable-grant-role";
    let (akid, principal) = session_of(&router, case, role, &[], &[UNREADABLE_GRANT]).await;

    for read in READS {
        assert_read(&router, case, &akid, &principal, read, Expect::Served).await;
    }

    let (status, put) = iam_call(
        &router,
        "PutRolePolicy",
        &[
            ("RoleName", role),
            ("PolicyName", "deny-describe-vpcs"),
            ("PolicyDocument", &statement("Deny", "ec2:DescribeVpcs", "*")),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the deny must be stored, got {status} {put}"
    );

    for read in READS {
        let expected = if read == "DescribeVpcs" {
            Expect::ExplicitDeny
        } else {
            Expect::Served
        };
        assert_read(&router, case, &akid, &principal, read, expected).await;
    }

    let (status, deleted) = iam_call(
        &router,
        "DeleteRolePolicy",
        &[("RoleName", role), ("PolicyName", "deny-describe-vpcs")],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the deny must be deletable, got {status} {deleted}"
    );

    for read in READS {
        assert_read(&router, case, &akid, &principal, read, Expect::Served).await;
    }
}

// ---------------------------------------------------------------------------
// An action judged against the resource the request addresses
// ---------------------------------------------------------------------------

/// The VPC the resource-level rows address, created through the account's own credential.
async fn seeded_vpc(router: &MultiServiceRouter, case: &str, cidr: &str) -> String {
    let (status, created) = ec2_as(router, ACCOUNT_KEY_ID, "CreateVpc", &[("CidrBlock", cidr)]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: the VPC must be created, got {status} {created}"
    );
    element(case, &created, "vpcId")
}

/// The tag keys the account's own credential reads back off `vpc_id`.
async fn tag_keys(router: &MultiServiceRouter, case: &str, vpc_id: &str) -> Vec<String> {
    let (status, described) = ec2_as(
        router,
        ACCOUNT_KEY_ID,
        "DescribeTags",
        &[("Filter.1.Name", "resource-id"), ("Filter.1.Value.1", vpc_id)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the tag read must be served, got {status} {described}"
    );
    elements(&described, "key")
}

/// One row of the resource-level table: the documents in force on the role, and whether the
/// write it makes on a VPC the account holds is refused.
struct TaggingRow {
    case: &'static str,
    role: &'static str,
    /// The inline documents put on the role, beside the unreadable attachment every row
    /// carries.
    inline: Vec<(&'static str, String)>,
    refused: bool,
}

/// RULE: for an action IAM judges against the resources the request ADDRESSES, the same
/// explicit deny refuses the call — and the refusal names that resource's ARN — while a role
/// carrying only the unreadable grant is served, so nothing but the deny changed the answer.
#[tokio::test]
async fn an_explicit_deny_refuses_a_resource_level_write_whose_grant_cannot_be_read() {
    let router = emulator();

    let rows = [
        TaggingRow {
            case: "an unreadable grant alone",
            role: "tagging-unreadable-grant-role",
            inline: Vec::new(),
            refused: false,
        },
        TaggingRow {
            case: "an unreadable grant and a deny of the action",
            role: "tagging-unreadable-grant-deny-role",
            inline: vec![("deny-create-tags", statement("Deny", "ec2:CreateTags", "*"))],
            refused: true,
        },
        TaggingRow {
            case: "an unreadable grant and a deny of the whole service",
            role: "tagging-unreadable-grant-service-deny-role",
            inline: vec![("deny-ec2", statement("Deny", "ec2:*", "*"))],
            refused: true,
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        let TaggingRow {
            case,
            role,
            inline,
            refused,
        } = row;
        let vpc_id = seeded_vpc(&router, case, &format!("10.{}.0.0/16", 60 + index)).await;
        let tag_key = format!("marker-{index}");
        let (akid, principal) = session_of(&router, case, role, inline, &[UNREADABLE_GRANT]).await;
        let (status, body) = ec2_as(
            &router,
            &akid,
            "CreateTags",
            &[
                ("ResourceId.1", &vpc_id),
                ("Tag.1.Key", &tag_key),
                ("Tag.1.Value", "lookup"),
            ],
        )
        .await;

        if *refused {
            assert_eq!(
                status, 403,
                "case {case:?}: expected HTTP 403, got {status} {body}"
            );
            assert_eq!(
                error_code(&body),
                UNAUTHORIZED_OPERATION,
                "case {case:?}: expected {UNAUTHORIZED_OPERATION}, got {body}"
            );
            let resource = ::engine::emulator_arn("ec2", REGION, &format!("vpc/{vpc_id}"));
            assert_eq!(
                error_message(&body),
                format!(
                    "{NOT_AUTHORIZED_SENTENCE} User: {principal} is not authorized to \
                     perform: ec2:CreateTags on resource: {resource} with an explicit deny \
                     in an identity-based policy."
                ),
                "case {case:?}: expected IAM's refusal wording naming the resource, got {body}"
            );
            let keys = tag_keys(&router, case, &vpc_id).await;
            assert!(
                !keys.contains(&tag_key),
                "case {case:?}: a refused write leaves the resource alone, got {keys:?}"
            );
        } else {
            assert_eq!(
                status, 200,
                "case {case:?}: expected the write to be served, got {status} {body}"
            );
            let keys = tag_keys(&router, case, &vpc_id).await;
            assert!(
                keys.contains(&tag_key),
                "case {case:?}: expected {tag_key:?} among the stored keys, got {keys:?}"
            );
        }
    }
}
