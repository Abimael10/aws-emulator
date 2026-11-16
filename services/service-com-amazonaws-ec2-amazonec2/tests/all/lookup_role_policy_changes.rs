// SPDX-License-Identifier: CC0-1.0

//! What a CHANGE to an assumed role's identity-based policies does to the EC2 `Describe`
//! reads a context provider walks: the estate those reads resolve while the role is
//! permitted, and the refusal they answer with once a statement that denies one of them is
//! stored on the role — plus the read being served again once that statement is deleted.
//!
//! THE RULE UNDER TEST: EC2 decides every `Describe` request against the identity-based
//! policy documents in force on the principal the request's credential speaks for AT THE
//! INSTANT OF THE CALL, judged over every resource (a `Describe` action supports no
//! resource-level permissions — "the Amazon EC2 API actions that begin with `Describe` do
//! not support resource-level permissions", Amazon EC2 User Guide). So a document stored on
//! a role decides the NEXT request made with that role's session credentials, whether the
//! session was minted before or after the store: a statement about the request that says
//! `Deny` refuses it with HTTP 403 `UnauthorizedOperation` naming the session principal, the
//! `ec2:` action and the rule the decision was taken under, a read no statement of it covers
//! is decided exactly as before, and deleting the document lifts the decision again. Every
//! such request is ANSWERED — served or refused — so a caller polling for a permission
//! change observes it rather than waiting.
//!
//! The table in [`super::account_wide_read_authorization`] spans the documents a read is
//! decided under and the kind of credential that presented it, and this suite is driven
//! through that suite's harness. The rows here span the other dimension, the documents
//! changing under a live session: which action the new statement names and HOW it names it
//! (exactly, under a run wildcard, under a single-character wildcard, in another case, or as
//! the whole service), and therefore which of the four reads it leaves untouched.

use smithy_server_runtime::MultiServiceRouter;

use super::account_wide_read_authorization::{
    ACCOUNT_KEY_ID, Expect, READS, STS_API_VERSION, account, assert_read, call_as, ec2_as,
    element, emulator, iam_call, session_of, statement,
};
use super::ec2_query::elements;

/// The identity-based grant the permitted role carries: the `Describe` family over every
/// resource, which is what a lookup role is given to read an account's network with.
fn allow_describes() -> (&'static str, String) {
    ("allow-describes", statement("Allow", "ec2:Describe*", "*"))
}

/// The tag key the seeded VPCs carry, which the tag-filtered read selects one of them by.
const MARKER_KEY: &str = "lookup-suite-marker";

/// One EC2 call made by the account's own published credential — the identity that seeds
/// the estate the reads under test resolve.
async fn seed(
    router: &MultiServiceRouter,
    case: &str,
    action: &str,
    params: &[(&str, &str)],
) -> String {
    let (status, body) = ec2_as(router, ACCOUNT_KEY_ID, action, params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: {action} must be served for the seeding credential, got {status} {body}"
    );
    body
}

/// Assume `role_name` again, as a client re-assuming the role to retry a read does: answers
/// the new session's access key id and the ARN a refusal of its calls must name.
async fn assume_again(
    router: &MultiServiceRouter,
    case: &str,
    role_name: &str,
) -> (String, String) {
    let role_arn = format!("arn:aws:iam::{}:role/{role_name}", account());
    let session_name = format!("{role_name}-retry");
    let (status, assumed) = call_as(
        router,
        "sts",
        STS_API_VERSION,
        ACCOUNT_KEY_ID,
        "AssumeRole",
        &[("RoleArn", &role_arn), ("RoleSessionName", &session_name)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the role must still be assumable, got {status} {assumed}"
    );
    (
        element(case, &assumed, "AccessKeyId"),
        element(case, &assumed, "Arn"),
    )
}

// ---------------------------------------------------------------------------
// What the permitted role's reads resolve
// ---------------------------------------------------------------------------

/// One seeded network: a VPC carrying the marker tag, its subnets and one route table of
/// its own, as the ids the reads must report.
struct Network {
    case: &'static str,
    marker: &'static str,
    vpc_cidr: &'static str,
    subnet_cidrs: Vec<&'static str>,
    vpc_id: String,
    subnet_ids: Vec<String>,
    route_table_id: String,
}

/// Seed every row's network through the account's own credential, so each tag-filtered read
/// below selects out of an account that really holds the other rows' VPCs too.
async fn seeded_networks(router: &MultiServiceRouter) -> Vec<Network> {
    // (case, marker tag value, the VPC's block, the subnets to cut out of it)
    let rows: Vec<(&str, &str, &str, Vec<&str>)> = vec![
        ("one subnet", "network-a", "10.93.0.0/16", vec!["10.93.0.0/24"]),
        (
            "two subnets",
            "network-b",
            "172.21.0.0/16",
            vec!["172.21.1.0/24", "172.21.8.0/21"],
        ),
        (
            "a smaller VPC with a /26 subnet",
            "network-c",
            "192.168.0.0/20",
            vec!["192.168.3.64/26"],
        ),
    ];

    let mut networks = Vec::new();
    for (case, marker, vpc_cidr, subnet_cidrs) in rows {
        let created = seed(router, case, "CreateVpc", &[("CidrBlock", vpc_cidr)]).await;
        let vpc_id = element(case, &created, "vpcId");
        seed(
            router,
            case,
            "CreateTags",
            &[
                ("ResourceId.1", &vpc_id),
                ("Tag.1.Key", MARKER_KEY),
                ("Tag.1.Value", marker),
            ],
        )
        .await;
        let mut subnet_ids = Vec::new();
        for subnet_cidr in &subnet_cidrs {
            let body = seed(
                router,
                case,
                "CreateSubnet",
                &[("VpcId", &vpc_id), ("CidrBlock", subnet_cidr)],
            )
            .await;
            subnet_ids.push(element(case, &body, "subnetId"));
        }
        let body = seed(router, case, "CreateRouteTable", &[("VpcId", &vpc_id)]).await;
        let route_table_id = element(case, &body, "routeTableId");
        networks.push(Network {
            case,
            marker,
            vpc_cidr,
            subnet_cidrs,
            vpc_id,
            subnet_ids,
            route_table_id,
        });
    }
    networks
}

/// RULE: while the documents in force on the assumed role allow the `Describe` family, the
/// session's reads report the account's own estate — the VPC a tag filter selects, with its
/// stored CIDR block, and the subnets and route tables of exactly that VPC.
#[tokio::test]
async fn a_permitted_session_resolves_the_network_the_reads_select() {
    let router = emulator();
    let networks = seeded_networks(&router).await;
    let (akid, _) = session_of(
        &router,
        "a permitted lookup role",
        "permitted-lookup-role",
        &[allow_describes()],
        &[],
    )
    .await;

    for network in &networks {
        let case = network.case;
        let (status, body) = ec2_as(
            &router,
            &akid,
            "DescribeVpcs",
            &[
                ("Filter.1.Name", &format!("tag:{MARKER_KEY}")),
                ("Filter.1.Value.1", network.marker),
            ],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: the tag-filtered read must be served, got {status} {body}"
        );
        assert_eq!(
            elements(&body, "vpcId"),
            vec![network.vpc_id.clone()],
            "case {case:?}: expected exactly {} to carry the marker {:?}, got {body}",
            network.vpc_id,
            network.marker
        );
        assert_eq!(
            elements(&body, "cidrBlock").first().map(String::as_str),
            Some(network.vpc_cidr),
            "case {case:?}: expected the stored block {}, got {body}",
            network.vpc_cidr
        );

        let (status, body) = ec2_as(
            &router,
            &akid,
            "DescribeSubnets",
            &[
                ("Filter.1.Name", "vpc-id"),
                ("Filter.1.Value.1", &network.vpc_id),
            ],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: the subnet read must be served, got {status} {body}"
        );
        let mut reported = elements(&body, "subnetId");
        reported.sort();
        let mut expected = network.subnet_ids.clone();
        expected.sort();
        assert_eq!(
            reported, expected,
            "case {case:?}: expected the subnets cut out of {} ({:?}), got {body}",
            network.vpc_id, network.subnet_cidrs
        );

        let (status, body) = ec2_as(
            &router,
            &akid,
            "DescribeRouteTables",
            &[
                ("Filter.1.Name", "vpc-id"),
                ("Filter.1.Value.1", &network.vpc_id),
            ],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: the route table read must be served, got {status} {body}"
        );
        let reported = elements(&body, "routeTableId");
        assert!(
            reported.contains(&network.route_table_id),
            "case {case:?}: expected {} among the route tables of {}, got {reported:?}",
            network.route_table_id,
            network.vpc_id
        );
    }
}

// ---------------------------------------------------------------------------
// A document stored on the role decides the next read
// ---------------------------------------------------------------------------

/// One row: the statement stored on the permitted role mid-session, and which of [`READS`]
/// it is about.
struct DenyRow {
    case: &'static str,
    role: &'static str,
    /// The `Action` of the `Deny` statement stored on the role.
    deny_action: &'static str,
    /// For each of [`READS`], in that order, whether the stored statement refuses it.
    refuses: [bool; 4],
}

/// What each read must answer under `row`, in the order [`READS`] lists them.
fn expectations(row: &DenyRow) -> [Expect; 4] {
    let mut expected = [Expect::Served; 4];
    for (slot, refused) in expected.iter_mut().zip(row.refuses.iter()) {
        if *refused {
            *slot = Expect::ExplicitDeny;
        }
    }
    expected
}

/// RULE: a `Deny` statement stored on the assumed role decides the NEXT read made with that
/// role's credentials — the session already minted and one minted afterwards alike — for
/// exactly the actions the statement's `Action` element matches (IAM compares action names
/// case-insensitively, with `*` matching any run and `?` one character), and deleting the
/// statement lifts the decision again.
#[tokio::test]
async fn a_deny_stored_mid_session_decides_the_next_read_and_deleting_it_lifts_the_decision() {
    let router = emulator();
    let rows = [
        DenyRow {
            case: "the exact action one read is",
            role: "deny-exact-role",
            deny_action: "ec2:DescribeVpcs",
            refuses: [false, false, false, true],
        },
        DenyRow {
            case: "a run wildcard over one read's name",
            role: "deny-run-wildcard-role",
            deny_action: "ec2:DescribeSub*",
            refuses: [false, false, true, false],
        },
        DenyRow {
            case: "a single-character wildcard over one read's name",
            role: "deny-character-wildcard-role",
            deny_action: "ec2:DescribeRouteTable?",
            refuses: [false, true, false, false],
        },
        DenyRow {
            case: "one read's action in another case",
            role: "deny-mixed-case-role",
            deny_action: "EC2:describeavailabilityzones",
            refuses: [true, false, false, false],
        },
        DenyRow {
            case: "every action of this service",
            role: "deny-whole-service-role",
            deny_action: "ec2:*",
            refuses: [true, true, true, true],
        },
    ];

    for row in &rows {
        let case = row.case;
        let (akid, principal) =
            session_of(&router, case, row.role, &[allow_describes()], &[]).await;

        // Before the statement is stored, the grant the role carries serves every read.
        for read in READS {
            assert_read(&router, case, &akid, &principal, read, Expect::Served).await;
        }

        let (status, put) = iam_call(
            &router,
            "PutRolePolicy",
            &[
                ("RoleName", row.role),
                ("PolicyName", "deny-one-read"),
                ("PolicyDocument", &statement("Deny", row.deny_action, "*")),
            ],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: the deny of {} must be stored, got {status} {put}",
            row.deny_action
        );

        // The session minted BEFORE the statement was stored is decided under it.
        let expected = expectations(row);
        for (read, expectation) in READS.iter().zip(expected.iter()) {
            assert_read(&router, case, &akid, &principal, read, *expectation).await;
        }

        // So is a session minted after it: the documents are the role's, not the session's.
        let (retry_akid, retry_principal) = assume_again(&router, case, row.role).await;
        for (read, expectation) in READS.iter().zip(expected.iter()) {
            assert_read(
                &router,
                case,
                &retry_akid,
                &retry_principal,
                read,
                *expectation,
            )
            .await;
        }

        let (status, deleted) = iam_call(
            &router,
            "DeleteRolePolicy",
            &[("RoleName", row.role), ("PolicyName", "deny-one-read")],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: the deny must be deletable, got {status} {deleted}"
        );

        // With the statement gone, both sessions are back on the grant's side of the gate.
        for read in READS {
            assert_read(&router, case, &akid, &principal, read, Expect::Served).await;
            assert_read(
                &router,
                case,
                &retry_akid,
                &retry_principal,
                read,
                Expect::Served,
            )
            .await;
        }
    }
}

/// RULE: the decision is taken on the request's credential, so a statement stored on ONE
/// role says nothing about another role's session — the estate a refused session cannot
/// read is still reported, unchanged, to a session the documents allow.
#[tokio::test]
async fn a_deny_on_one_role_leaves_another_role_and_the_estate_alone() {
    let router = emulator();
    let networks = seeded_networks(&router).await;
    let network = networks
        .first()
        .expect("the seeded estate carries at least one network");

    let (denied_akid, denied_principal) = session_of(
        &router,
        "the denied role",
        "isolated-denied-role",
        &[
            allow_describes(),
            ("deny-vpcs", statement("Deny", "ec2:DescribeVpcs", "*")),
        ],
        &[],
    )
    .await;
    let (permitted_akid, _) = session_of(
        &router,
        "the untouched role",
        "isolated-permitted-role",
        &[allow_describes()],
        &[],
    )
    .await;

    assert_read(
        &router,
        "the denied role",
        &denied_akid,
        &denied_principal,
        "DescribeVpcs",
        Expect::ExplicitDeny,
    )
    .await;

    let (status, body) = ec2_as(
        &router,
        &permitted_akid,
        "DescribeVpcs",
        &[
            ("Filter.1.Name", &format!("tag:{MARKER_KEY}")),
            ("Filter.1.Value.1", network.marker),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case \"the untouched role\": the read must be served, got {status} {body}"
    );
    assert_eq!(
        elements(&body, "vpcId"),
        vec![network.vpc_id.clone()],
        "case \"the untouched role\": expected {} to still be reported, got {body}",
        network.vpc_id
    );
}
