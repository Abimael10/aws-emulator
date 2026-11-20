// SPDX-License-Identifier: CC0-1.0

//! The refusal CLASS an EC2 identifier that names no resource of the account is reported
//! under, driven over `ec2Query` through the crate's shared [`super::ec2_query`] harness.
//!
//! The rule under test: an identifier this service can READ that this account holds no
//! resource for — a resource of the account that is already gone, and an id nothing was ever
//! created under — is reported ABSENT, under the resource's own not-found code and sentence;
//! only a value the service cannot read is reported as one it cannot resolve to an identifier
//! at all, in that operation's own wording. The two questions are asked in that order, FORM
//! before EXISTENCE, so a client that branches on the not-found code to create what it is
//! missing is never told instead that its id generation is broken.
//!
//! WHAT the service can read is a fact of each id KIND, recorded with it: an EBS volume id is
//! read as the printed forms EC2 issues AND, because the long form is an obfuscated identifier
//! the service decodes before it resolves anything, never a long-form identifier this store's
//! mint has not reached (`ec2::state::volume_id_is_readable`), while a VPC endpoint id is read
//! by its printed form alone. So the long-form rows below answer differently for the two kinds,
//! each on that kind's own recorded refusals, and the retired SHORT form — which carries no
//! identifier to decode — is read for a volume whatever the mint has reached.
//!
//! It is asserted here over two operations, because within one kind the rule is one rule and
//! not either operation's own — and over both shapes the answer takes: `CreateSnapshot`, which
//! carries the cause as the call's own error envelope, and `DeleteVpcEndpoints`, a BULK call
//! that carries it PER ITEM inside a 200 and so reports every id it cannot resolve under one
//! code, whatever is wrong with the value.
//!
//! Each table row is a distinct input along what the rule generalises over: which id form the
//! value takes (both digit counts EC2 has issued, the all-zero placeholder, a long form whose
//! leading digit no issued id carries, a value that is not hexadecimal, the id of another kind
//! of resource), and whether the account ever held the resource the id names.

use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{call, element, elements, error_code, error_message, indexed, unescaped};

/// The Region every call here is signed for.
const REGION: &str = "us-east-1";

/// One `action` call, as its HTTP status and XML body.
async fn op(
    router: &MultiServiceRouter,
    action: &str,
    params: &[(String, String)],
) -> (u16, String) {
    call(router, REGION, action, params).await
}

/// One form field.
fn p(name: &str, value: &str) -> (String, String) {
    (name.to_string(), value.to_string())
}

/// One `action` call that has to succeed, and the body it answered.
async fn ok(
    case: &str,
    router: &MultiServiceRouter,
    action: &str,
    params: &[(String, String)],
) -> String {
    let (status, body) = op(router, action, params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from {action}, got {status} with body {body}"
    );
    body
}

/// What a refused identifier must be answered with: the EC2 error code and the whole sentence
/// beside it, as a client reads them.
#[derive(Clone, Debug, Eq, PartialEq)]
struct Answer {
    code: String,
    message: String,
}

/// The answer a volume id this service CAN read that names no volume of the account must get.
fn volume_absent(volume_id: &str) -> Answer {
    Answer {
        code: "InvalidVolume.NotFound".to_string(),
        message: format!("The volume '{volume_id}' does not exist."),
    }
}

/// The answer a value this service cannot READ as a volume id must get from `CreateSnapshot`:
/// the malformed code, in the sentence that operation's parameter layer words a value it
/// rejects with.
fn volume_unreadable(value: &str) -> Answer {
    Answer {
        code: "InvalidVolumeID.Malformed".to_string(),
        message: format!("Value ( {value} ) for parameter VOLUME is invalid. "),
    }
}

/// Create one 1 GiB volume in the Region's first zone and answer its id.
async fn create_volume(case: &str, router: &MultiServiceRouter) -> String {
    let body = ok(
        case,
        router,
        "CreateVolume",
        &[p("AvailabilityZone", &format!("{REGION}a")), p("Size", "1")],
    )
    .await;
    element(&body, "volumeId").unwrap_or_else(|| panic!("case {case:?}: no volumeId in {body}"))
}

/// Which resource the row's identifier stands for, and so how the row obtains it.
#[derive(Clone, Copy, Debug)]
enum Named {
    /// A literal value the row sends as written.
    Literal(&'static str),
    /// The id of a resource this row created and then DELETED — an identifier EC2 did issue,
    /// which the account no longer holds anything under.
    Deleted,
}

/// `CreateSnapshot` over the class: every value the operation cannot resolve to a volume of the
/// account, each with the answer the rule gives it.
#[tokio::test]
async fn create_snapshot_reports_an_issued_volume_id_absent_and_only_other_values_unreadable() {
    // Rows: what the request names, whether the value is one this service can READ, and so the
    // answer the rule gives it — an id it can read is reported absent, a value it cannot read
    // is unreadable.
    let rows: [(&str, Named, bool); 9] = [
        (
            "a long-form identifier the mint never handed out",
            Named::Literal("vol-0123456789abcdef0"),
            false,
        ),
        (
            "another long-form identifier the mint never handed out",
            Named::Literal("vol-0f0e0d0c0b0a09080"),
            false,
        ),
        (
            "a short id of the retired form naming no volume",
            Named::Literal("vol-1a2b3c4d"),
            true,
        ),
        (
            "the id of a volume the account deleted",
            Named::Deleted,
            true,
        ),
        (
            "the all-zero placeholder no mint can issue",
            Named::Literal("vol-00000000000000000"),
            false,
        ),
        (
            "a long form whose leading digit no issued id carries",
            Named::Literal("vol-fedcba98765432100"),
            false,
        ),
        (
            "a value that is not hexadecimal",
            Named::Literal("vol-not-hex"),
            false,
        ),
        (
            "a digit count between the two issued forms",
            Named::Literal("vol-0123456789"),
            false,
        ),
        (
            "the id of another kind of resource",
            Named::Literal("snap-0123456789abcdef0"),
            false,
        ),
    ];

    for (case, named, readable) in rows {
        let (router, _engine) = super::ec2_query::emulator();
        let volume_id = match named {
            Named::Literal(value) => value.to_string(),
            Named::Deleted => {
                let created = create_volume(case, &router).await;
                ok(case, &router, "DeleteVolume", &[p("VolumeId", &created)]).await;
                created
            }
        };
        let expected = if readable {
            volume_absent(&volume_id)
        } else {
            volume_unreadable(&volume_id)
        };

        let (status, body) = op(&router, "CreateSnapshot", &[p("VolumeId", &volume_id)]).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400 for {volume_id}, got {status} with body {body}"
        );
        let answered = Answer {
            code: error_code(&body),
            message: error_message(&body),
        };
        assert_eq!(
            answered, expected,
            "case {case:?}: expected {expected:?} for {volume_id}, got {answered:?} in {body}"
        );
        // A refused create stores nothing: the account's snapshot listing is still empty.
        let listed = ok(case, &router, "DescribeSnapshots", &[p("Owner.1", "self")]).await;
        let stored = elements(&listed, "snapshotId");
        assert!(
            stored.is_empty(),
            "case {case:?}: a refused create stores no snapshot, got {stored:?}"
        );
    }
}

/// `DeleteVpcEndpoints` over the same class, in the shape a BULK call carries it: inside a 200,
/// one `Unsuccessful` entry per id, all under the one per-item code — with the ABSENT sentence
/// for an id of the issued form and the NOT-OWNED sentence for a value outside it, which
/// decodes to no endpoint of any account.
#[tokio::test]
async fn delete_vpc_endpoints_reports_every_unresolved_id_per_item_under_one_code() {
    // Rows: what the request names, and which sentence the rule words the cause with.
    let rows: [(&str, Named, bool); 7] = [
        (
            "a long id of the issued form naming no endpoint",
            Named::Literal("vpce-0a1b2c3d4e5f60718"),
            true,
        ),
        (
            "another long id of the issued form naming no endpoint",
            Named::Literal("vpce-00f00000000000009"),
            true,
        ),
        (
            "the id of an endpoint the account deleted",
            Named::Deleted,
            true,
        ),
        (
            "the all-zero placeholder no mint can issue",
            Named::Literal("vpce-00000000000000000"),
            false,
        ),
        (
            "a long form whose leading digit no issued id carries",
            Named::Literal("vpce-11111111111111111"),
            false,
        ),
        (
            "a value that is not hexadecimal",
            Named::Literal("vpce-not-hex"),
            false,
        ),
        (
            "the id of the endpoint SERVICE kind, which extends the same prefix",
            Named::Literal("vpce-svc-0a1b2c3d4e5f6a7b8"),
            false,
        ),
    ];

    for (case, named, issued_form) in rows {
        let (router, _engine) = super::ec2_query::emulator();
        let endpoint_id = match named {
            Named::Literal(value) => value.to_string(),
            Named::Deleted => {
                let created = create_endpoint(case, &router).await;
                let body = ok(
                    case,
                    &router,
                    "DeleteVpcEndpoints",
                    &indexed("VpcEndpointId", &[created.as_str()]),
                )
                .await;
                assert!(
                    elements(&body, "code").is_empty(),
                    "case {case:?}: the first delete of {created} reports nothing \
                     unsuccessful, got {body}"
                );
                created
            }
        };

        let body = ok(
            case,
            &router,
            "DeleteVpcEndpoints",
            &indexed("VpcEndpointId", &[endpoint_id.as_str()]),
        )
        .await;
        let codes = elements(&body, "code");
        assert_eq!(
            codes,
            vec!["InvalidVpcEndpoint.NotFound".to_string()],
            "case {case:?}: expected the one per-item code for {endpoint_id}, got {codes:?} in \
             {body}"
        );
        let expected = if issued_form {
            format!("The Vpc Endpoint Id '{endpoint_id}' does not exist")
        } else {
            format!(
                "user {owner} does not own a resource {endpoint_id}",
                owner = engine::emulator_account_id(),
            )
        };
        let reported: Vec<String> = elements(&body, "message")
            .iter()
            .map(|message| unescaped(message))
            .collect();
        assert_eq!(
            reported,
            vec![expected.clone()],
            "case {case:?}: expected the sentence {expected:?} for {endpoint_id}, got \
             {reported:?} in {body}"
        );
        let resources = elements(&body, "resourceId");
        assert_eq!(
            resources,
            vec![endpoint_id.clone()],
            "case {case:?}: expected the report to name the id the request sent, got \
             {resources:?} in {body}"
        );
    }
}

/// Create one gateway VPC endpoint of the account and answer its id — the resource the DELETED
/// row removes, so its own minted id is an identifier EC2 issued and the account no longer
/// holds anything under.
async fn create_endpoint(case: &str, router: &MultiServiceRouter) -> String {
    let body = ok(case, router, "CreateVpc", &[p("CidrBlock", "10.80.0.0/16")]).await;
    let vpc_id =
        element(&body, "vpcId").unwrap_or_else(|| panic!("case {case:?}: no vpcId in {body}"));
    let body = ok(
        case,
        router,
        "CreateVpcEndpoint",
        &[
            p("VpcId", &vpc_id),
            p("ServiceName", &format!("com.amazonaws.{REGION}.s3")),
            p("VpcEndpointType", "Gateway"),
        ],
    )
    .await;
    element(&body, "vpcEndpointId")
        .unwrap_or_else(|| panic!("case {case:?}: no vpcEndpointId in {body}"))
}
