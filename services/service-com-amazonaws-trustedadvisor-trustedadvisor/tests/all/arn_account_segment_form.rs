// SPDX-License-Identifier: CC0-1.0

//! What the ACCOUNT POSITION of an ARN names, and what follows from it for the
//! one operation whose input is an arbitrary AWS resource ARN
//! (`ListRecommendationsForResource`), driven over HTTP through the same
//! `MultiServiceRouter` the emulator serves with.
//!
//! The rule: the account position of an ARN names an AWS account only when it
//! holds a value of the account form the model states — `AccountId`,
//! `length 12..12, pattern ^\d+$`
//! (`smithy_query(query="shape",shape="AccountId",
//! model_path="services/service-com-amazonaws-trustedadvisor-trustedadvisor/smithy-model/com-amazonaws-trustedadvisor-trustedadvisor.json")`)
//! — so a position holding anything else (empty, as a resource type whose ARNs
//! carry no account has it; an AWS-managed owner token; a digit string of
//! another length) names NO account, and the resource it belongs to is not
//! another account's resource. Only an ARN whose position holds an account id
//! OTHER than the calling account's is refused for the resource.
//!
//! `awsResourceArn` is the one input of this service the model leaves that
//! position open on (`AwsResourceArn`, pattern
//! `^arn:aws(-\w+)*:[\w\d-]+:([\w\d-]*)?:[\w\d_-]*([:/].+)*$` — letters,
//! digits, `_` and `-`), which is why the rows below are driven through it; the
//! service's own identifiers pin it to `\d{12}`, and their refusal is pinned in
//! `tests/all/support_level_gate.rs`.
//!
//! Every call is made as an account whose support plan includes the Trusted
//! Advisor API, except the one row that pins the ORDER of the two refusals.

use serde_json::json;
use service_com_amazonaws_trustedadvisor_trustedadvisor::state::{
    arn_account_id, is_account_id, is_foreign_resource, SupportPlan,
    ACCESS_DENIED_SUPPORT_LEVEL,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::recommendation_loop::{dispatch, emulator_on_plan, error_code, label};

/// One value the account position of an ARN can hold, and whether it is an
/// account id.
struct Segment {
    case: &'static str,
    /// The value in the 5th (account) position.
    value: &'static str,
    /// Whether that value is a value the modelled `AccountId` can carry:
    /// exactly twelve decimal digits.
    is_account: bool,
}

/// The values the account position is tested on, differing along the FORM of the
/// value: an account id (the calling account's and other accounts'), the empty
/// position of a resource type whose ARNs carry no account, the AWS-managed
/// owner tokens real ARNs carry there, and digit strings of the lengths either
/// side of twelve.
///
/// Every value is one the account position of `AwsResourceArn` accepts
/// (`[\w\d_-]*`), so each row below reaches the handler rather than the
/// framework's constraint walk.
const SEGMENTS: &[Segment] = &[
    Segment {
        case: "the calling account",
        value: engine::EMULATOR_ACCOUNT_ID,
        is_account: true,
    },
    Segment {
        case: "another account",
        value: "111122223333",
        is_account: true,
    },
    Segment {
        case: "another account whose id has leading zeros",
        value: "000000000012",
        is_account: true,
    },
    Segment {
        case: "no account position at all, as an S3 bucket's ARN has it",
        value: "",
        is_account: false,
    },
    Segment {
        case: "the AWS-managed owner token",
        value: "aws",
        is_account: false,
    },
    Segment {
        case: "that token in upper case",
        value: "AWS",
        is_account: false,
    },
    Segment {
        case: "a non-account token with an underscore",
        value: "my_scope",
        is_account: false,
    },
    Segment {
        case: "a non-account token with a hyphen",
        value: "not-an-account",
        is_account: false,
    },
    Segment {
        case: "one digit short of an account id",
        value: "11112222333",
        is_account: false,
    },
    Segment {
        case: "one digit past an account id",
        value: "1111222233334",
        is_account: false,
    },
    Segment {
        case: "twelve characters, not all of them digits",
        value: "1111aaaa3333",
        is_account: false,
    },
];

/// An EC2 instance ARN in `value`'s account position, naming an instance id no
/// seeded check flagged — so the answer of a row that is NOT refused is the
/// operation's own empty list rather than a seeded row.
fn probe_arn(value: &str) -> String {
    format!("arn:aws:ec2:us-east-1:{value}:instance/i-0fffffffffffffff0")
}

/// A mounted emulator whose account's support plan includes the Trusted Advisor
/// API.
fn emulator() -> MultiServiceRouter {
    emulator_on_plan(SupportPlan::Business).0
}

/// The modelled URI of `ListRecommendationsForResource` for one AWS resource ARN.
fn path_for(arn: &str) -> String {
    format!("/v1/recommendations-for-resource/{}", label(arn))
}

#[test]
fn the_account_position_names_an_account_only_in_the_modelled_account_id_form() {
    for segment in SEGMENTS {
        let Segment {
            case,
            value,
            is_account,
        } = segment;
        assert_eq!(
            is_account_id(value),
            *is_account,
            "case {case:?}: expected {value:?} to be an account id: {is_account}, got {}",
            is_account_id(value)
        );

        let arn = probe_arn(value);
        let expected = is_account.then_some(*value);
        let owner = arn_account_id(&arn);
        assert_eq!(
            owner.as_deref(),
            expected,
            "case {case:?}: expected {arn:?} to name the owning account {expected:?}, got \
             {owner:?}"
        );

        // A resource is another account's only when its ARN names an account
        // and that account is not the calling one.
        let expected_foreign = *is_account && *value != engine::EMULATOR_ACCOUNT_ID;
        let foreign = is_foreign_resource(&arn);
        assert_eq!(
            foreign, expected_foreign,
            "case {case:?}: expected {arn:?} to be another account's resource: \
             {expected_foreign}, got {foreign}"
        );
    }
}

#[test]
fn a_string_with_no_account_position_names_no_account() {
    // Strings shorter than an ARN's account position: the derivation reports no
    // account rather than reading a shorter segment as one.
    let rows = [
        "arn:aws:trustedadvisor:us-east-1",
        "arn:aws:ec2",
        "arn",
        "",
    ];
    for arn in rows {
        let owner = arn_account_id(arn);
        assert!(
            owner.is_none(),
            "case {arn:?}: expected no owning account, got {owner:?}"
        );
        assert!(
            !is_foreign_resource(arn),
            "case {arn:?}: expected a string naming no account not to be another account's \
             resource"
        );
    }
}

#[tokio::test]
async fn only_an_arn_in_another_account_is_refused_for_the_resource() {
    let router = emulator();

    for segment in SEGMENTS {
        let Segment {
            case,
            value,
            is_account,
        } = segment;
        let arn = probe_arn(value);
        let (status, errortype, body) = dispatch(&router, "GET", &path_for(&arn), None).await;

        if *is_account && *value != engine::EMULATOR_ACCOUNT_ID {
            assert_eq!(
                status, 403,
                "case {case:?}: expected 403 for a resource in another account, got {status} \
                 with {body}"
            );
            assert_eq!(
                errortype.as_deref(),
                Some("AccessDeniedException"),
                "case {case:?}: expected x-amzn-errortype AccessDeniedException, got \
                 {errortype:?} with {body}"
            );
            assert_eq!(
                error_code(&body),
                "AccessDeniedException",
                "case {case:?}: expected __type AccessDeniedException, got {body}"
            );
            assert_eq!(
                body["message"],
                json!(format!("Access denied to resource: {arn}")),
                "case {case:?}: expected the refusal to name the resource, got {body}"
            );
        } else {
            assert_eq!(
                status, 200,
                "case {case:?}: expected the operation's own answer for an ARN naming no other \
                 account, got {status} with {body}"
            );
            assert_eq!(
                body["recommendationForResourceSummaries"],
                json!([]),
                "case {case:?}: expected an empty list for an ARN no recommendation flagged, got \
                 {body}"
            );
            assert!(
                body["nextToken"].is_null(),
                "case {case:?}: expected no nextToken, got {body}"
            );
        }
    }
}

#[tokio::test]
async fn an_aws_managed_resource_arn_reports_an_empty_list() {
    let router = emulator();

    // The ARN forms AWS itself publishes with a non-account owner token in the
    // account position (an AWS managed policy, a service-linked role): each is
    // a well-formed `AwsResourceArn` naming no account, so each reaches the
    // operation and reports what the store holds about it — nothing.
    let rows = [
        (
            "an AWS managed policy",
            "arn:aws:iam::aws:policy/ReadOnlyAccess",
        ),
        (
            "a service-linked role path",
            "arn:aws:iam::aws:role/aws-service-role/support.amazonaws.com/AWSServiceRoleForSupport",
        ),
        (
            "an AWS-owned resource in another partition",
            "arn:aws-us-gov:iam::aws:policy/AdministratorAccess",
        ),
    ];

    for (case, arn) in rows {
        let (status, _errortype, body) = dispatch(&router, "GET", &path_for(arn), None).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected 200 for {arn:?}, got {status} with {body}"
        );
        assert_eq!(
            body["recommendationForResourceSummaries"],
            json!([]),
            "case {case:?}: expected an empty list, got {body}"
        );
    }
}

#[tokio::test]
async fn an_arn_naming_no_account_reaches_the_support_level_gate() {
    // The other side of the same rule: an ARN naming no account is not refused
    // FOR THE RESOURCE, so an unentitled caller passing one is answered by the
    // service-wide support-level stage instead — the ARN never produces the
    // resource sentence.
    let router = emulator_on_plan(SupportPlan::Developer).0;

    for segment in SEGMENTS.iter().filter(|segment| !segment.is_account) {
        let arn = probe_arn(segment.value);
        let case = segment.case;
        let (status, _errortype, body) = dispatch(&router, "GET", &path_for(&arn), None).await;
        assert_eq!(
            status, 403,
            "case {case:?}: expected the support-level refusal, got {status} with {body}"
        );
        assert_eq!(
            body["message"],
            json!(ACCESS_DENIED_SUPPORT_LEVEL),
            "case {case:?}: expected the support-level sentence, got {body}"
        );
    }
}
