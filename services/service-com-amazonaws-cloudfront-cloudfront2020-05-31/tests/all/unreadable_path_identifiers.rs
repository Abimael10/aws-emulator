// SPDX-License-Identifier: CC0-1.0

//! What CloudFront answers a request whose PATH it cannot read, end to end over HTTP.
//!
//! Two rules, both about a request that is refused in front of — or at the very start of —
//! the operation it addressed:
//!
//! * the front door answers a request whose method and path name NONE of the actions this API
//!   serves with the unknown action it is: HTTP 404, `<Code>InvalidAction</Code>`, and no
//!   `Message` at all — nothing read the request as an action, so there is no operation,
//!   member or value for the refusal to name, and the request's own path is never echoed back.
//!   An identifier carrying a `/` (an ARN where an id belongs) is exactly such a request: it
//!   spills into the next path segment and matches no route, while the operation itself is
//!   served for an identifier that fits one segment;
//! * an operation whose recorded contract reads its path identifier before resolving it
//!   answers an identifier that is no identifier of the addressed resource kind — one outside
//!   the form CloudFront issues that kind's identifiers in, or one this account holds as a
//!   resource of ANOTHER kind of the same identifier family — under its own
//!   unresolvable-identifier code with the message "<Member> format is invalid", naming the
//!   model's name for the member the identifier arrived in. The absent-resource sentence is
//!   kept for an identifier that COULD address one, which is the contrast that makes the rule
//!   about the identifier rather than about absence.
//!
//! The rows span what the rules generalise over: for the first rule, four resource families,
//! both an unmatched identifier and an unmatched resource word, a path longer than its route,
//! a method no route of the path serves and a path outside this API's version prefix, against
//! matched routes as the positive control; for the second, all three operations the recorded
//! behaviour puts under it (two reads and one mutation), both member names those identifiers
//! arrive in (`Id` and `DistributionId`), two resource kinds whose identifiers share one form,
//! and seven forms that are no identifier of the addressed kind — the other kind's own minted
//! identifier, a lower-cased identifier, a body one character short and one character long,
//! another namespace letter, a UUID and a bare word — beside an identifier of the right form
//! the account does not hold.
//!
//! Every assertion travels the router, so each case exercises routing, the restXml reader, the
//! handler, the persisted state and serialization. The request harness and the create helpers
//! are the crate's own.

use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_cloudfront_cloudfront2020_05_31::state::{
    id_format_invalid_message, NO_SUCH_DISTRIBUTION_MESSAGE,
    NO_SUCH_ORIGIN_ACCESS_CONTROL_MESSAGE,
};

use crate::distribution_lifecycle::{
    batch_xml, call, config, create_distribution, emulator, Answer, API_VERSION, ACCOUNT_ID,
};
use crate::origin_access_control_lifecycle::{create_control, table};

/// The code the front door names an action it could not find.
const INVALID_ACTION: &str = "InvalidAction";

/// An invalidation batch every mutation row below submits: a well-formed document, so what the
/// call is answered with is decided by the identifier in its path and nothing else.
fn batch(reference: &str) -> String {
    batch_xml(reference, &["/*"])
}

// ---------------------------------------------------------------------------
// Rule 1 — a path that names no action of this API
// ---------------------------------------------------------------------------

/// One request whose path names no action, and the one it is contrasted with.
struct Unmatched {
    case: &'static str,
    method: &'static str,
    uri: String,
    body: Option<String>,
}

#[tokio::test]
async fn a_path_that_names_no_action_is_the_front_doors_unknown_action() {
    let router = emulator();
    let created = create_distribution(
        &router,
        &config("the distribution the matched routes address", "unknown-action").xml(),
    )
    .await;
    assert_eq!(
        created.status, 201,
        "expected HTTP 201 from CreateDistribution, got {} with {}",
        created.status, created.raw
    );
    let id = created
        .root()
        .text_at("Id")
        .unwrap_or_default()
        .to_string();

    // The ARNs of three resource families, each carrying the `/` that makes it span two path
    // segments — which is why a request naming one where an identifier belongs matches no
    // route at all.
    let distribution_arn = format!("arn:aws:cloudfront::{ACCOUNT_ID}:distribution/{id}");
    let function_arn = format!("arn:aws:cloudfront::{ACCOUNT_ID}:function/unknown-action-fn");
    let origin_access_control_arn =
        format!("arn:aws:cloudfront::{ACCOUNT_ID}:origin-access-control/E1AAAAAAAAAAAA");

    let rows = vec![
        Unmatched {
            case: "an ARN where a distribution id belongs, on a read",
            method: "GET",
            uri: format!("/{API_VERSION}/distribution/{distribution_arn}/config"),
            body: None,
        },
        Unmatched {
            case: "an ARN where a function name belongs",
            method: "GET",
            uri: format!("/{API_VERSION}/function/{function_arn}/describe"),
            body: None,
        },
        Unmatched {
            case: "an ARN where a distribution id belongs, on a mutation",
            method: "POST",
            uri: format!("/{API_VERSION}/distribution/{distribution_arn}/invalidation"),
            body: Some(batch("unknown-action-arn")),
        },
        Unmatched {
            case: "an ARN where an origin access control id belongs",
            method: "GET",
            uri: format!("/{API_VERSION}/origin-access-control/{origin_access_control_arn}"),
            body: None,
        },
        Unmatched {
            case: "a segment past the end of a route",
            method: "GET",
            uri: format!("/{API_VERSION}/distribution/{id}/config/and-more"),
            body: None,
        },
        Unmatched {
            case: "a resource word no action of this API serves",
            method: "GET",
            uri: format!("/{API_VERSION}/not-a-cloudfront-resource/{id}"),
            body: None,
        },
        Unmatched {
            case: "a method no action of this path serves",
            method: "DELETE",
            uri: format!("/{API_VERSION}/distribution/{id}/config"),
            body: None,
        },
        Unmatched {
            case: "a path outside this API's version prefix",
            method: "GET",
            uri: format!("/not-an-api-version/distribution/{id}"),
            body: None,
        },
    ];

    for Unmatched {
        case,
        method,
        uri,
        body,
    } in &rows
    {
        let answer = call(&router, method, uri, body.as_deref(), None).await;
        assert_eq!(
            answer.status, 404,
            "case {case}: expected HTTP 404, got {} with {}",
            answer.status, answer.raw
        );
        assert_eq!(
            answer.error_code(),
            INVALID_ACTION,
            "case {case}: expected Code {INVALID_ACTION}, got {:?} with {}",
            answer.error_code(),
            answer.raw
        );
        // The refusal states its code and nothing else: no `Message`, and in particular
        // nothing taken from the request.
        assert_eq!(
            answer.root().find_text("Message"),
            None,
            "case {case}: expected no Message member, got {:?} with {}",
            answer.root().find_text("Message"),
            answer.raw
        );
        assert!(
            !answer.raw.contains(uri.as_str()),
            "case {case}: the refusal echoed the request path back in {}",
            answer.raw
        );
        // The envelope still names the request it answers, as every other refusal of this
        // service does.
        let request_id = answer.root().find_text("RequestId").unwrap_or_default();
        assert!(
            !request_id.is_empty(),
            "case {case}: expected the envelope to carry a RequestId, got {}",
            answer.raw
        );
    }

    // The positive control: a path that DOES name an action is served by that action — the
    // refusal above is about the action lookup, not about the identifier the path carries.
    let served = call(
        &router,
        "GET",
        &format!("/{API_VERSION}/distribution/{id}/config"),
        None,
        None,
    )
    .await;
    assert_eq!(
        served.status, 200,
        "case a matched route: expected HTTP 200, got {} with {}",
        served.status, served.raw
    );
    let absent = call(
        &router,
        "GET",
        &format!("/{API_VERSION}/distribution/EABCDEFGH12345/config"),
        None,
        None,
    )
    .await;
    assert_eq!(
        absent.error_code(),
        "NoSuchDistribution",
        "case a matched route addressing nothing: expected the operation's own refusal, got \
         {:?} with {}",
        absent.error_code(),
        absent.raw
    );
}

// ---------------------------------------------------------------------------
// Rule 2 — the identifier is read before the store
// ---------------------------------------------------------------------------

/// One operation that reads its path identifier before resolving it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Op {
    GetDistributionConfig,
    CreateInvalidation,
    GetOriginAccessControl,
}

impl Op {
    /// The modelled error code this operation answers an identifier it cannot resolve.
    fn code(self) -> &'static str {
        match self {
            Self::GetDistributionConfig | Self::CreateInvalidation => "NoSuchDistribution",
            Self::GetOriginAccessControl => "NoSuchOriginAccessControl",
        }
    }

    /// The model's name for the input member this operation's identifier arrives in.
    fn member(self) -> &'static str {
        match self {
            Self::CreateInvalidation => "DistributionId",
            Self::GetDistributionConfig | Self::GetOriginAccessControl => "Id",
        }
    }

    /// The sentence this operation answers an identifier that could address its resource but
    /// names none.
    fn absent(self) -> &'static str {
        match self {
            Self::GetDistributionConfig | Self::CreateInvalidation => {
                NO_SUCH_DISTRIBUTION_MESSAGE
            }
            Self::GetOriginAccessControl => NO_SUCH_ORIGIN_ACCESS_CONTROL_MESSAGE,
        }
    }

    /// The status this operation answers a request it serves.
    fn served_status(self) -> u16 {
        match self {
            Self::CreateInvalidation => 201,
            Self::GetDistributionConfig | Self::GetOriginAccessControl => 200,
        }
    }

    /// Address `identifier` through this operation.
    async fn call(self, router: &MultiServiceRouter, case: &str, identifier: &str) -> Answer {
        match self {
            Self::GetDistributionConfig => call(
                router,
                "GET",
                &format!("/{API_VERSION}/distribution/{identifier}/config"),
                None,
                None,
            )
            .await,
            Self::CreateInvalidation => call(
                router,
                "POST",
                &format!("/{API_VERSION}/distribution/{identifier}/invalidation"),
                Some(&batch(&format!("batch-for-{case}"))),
                None,
            )
            .await,
            Self::GetOriginAccessControl => call(
                router,
                "GET",
                &format!("/{API_VERSION}/origin-access-control/{identifier}"),
                None,
                None,
            )
            .await,
        }
    }
}

/// Every operation of rule 2, with the identifier that addresses the resource it really holds
/// and the identifier of the OTHER resource kind drawn from the same form.
fn operations() -> [Op; 3] {
    [
        Op::GetDistributionConfig,
        Op::CreateInvalidation,
        Op::GetOriginAccessControl,
    ]
}

#[tokio::test]
async fn an_identifier_of_no_such_kind_is_reported_unreadable() {
    let router = emulator();

    let created = create_distribution(
        &router,
        &config("the distribution these rows address", "unreadable-id").xml(),
    )
    .await;
    assert_eq!(
        created.status, 201,
        "expected HTTP 201 from CreateDistribution, got {} with {}",
        created.status, created.raw
    );
    let distribution_id = created
        .root()
        .text_at("Id")
        .unwrap_or_default()
        .to_string();

    let control = table()[0].with_name("the control these rows address", "unreadable-id-oac");
    let control_created = create_control(&router, &control.xml()).await;
    assert_eq!(
        control_created.status, 201,
        "expected HTTP 201 from CreateOriginAccessControl, got {} with {}",
        control_created.status, control_created.raw
    );
    let control_id = control_created
        .root()
        .text_at("Id")
        .unwrap_or_default()
        .to_string();

    for op in operations() {
        // The identifier this operation's resource is really held under, and the identifier of
        // the other kind drawn from the SAME form — which is what the store, not the value,
        // tells apart.
        let (held, other_kind) = match op {
            Op::GetDistributionConfig | Op::CreateInvalidation => {
                (distribution_id.clone(), control_id.clone())
            }
            Op::GetOriginAccessControl => (control_id.clone(), distribution_id.clone()),
        };
        let mut short = held.clone();
        short.pop();
        let long = format!("{held}Z");

        // Values that are no identifier of the addressed kind at all.
        let unreadable: [(&str, String); 7] = [
            ("the other kind's own minted identifier", other_kind),
            ("the identifier in lower case", held.to_lowercase()),
            ("a body one character short", short),
            ("a body one character long", long),
            (
                "another namespace letter",
                format!("I{}", &held[1..]),
            ),
            (
                "a UUID",
                "e220a839-7b1d-4daf-ae78-9e6aa1b965f4".to_string(),
            ),
            ("a bare word", "not-an-identifier".to_string()),
        ];
        for (row, identifier) in unreadable {
            let case = format!("{op:?}/{row}");
            let answer = op.call(&router, &case, &identifier).await;
            assert_eq!(
                answer.status, 404,
                "case {case}: expected HTTP 404 for {identifier:?}, got {} with {}",
                answer.status, answer.raw
            );
            assert_eq!(
                answer.error_code(),
                op.code(),
                "case {case}: expected Code {}, got {:?} with {}",
                op.code(),
                answer.error_code(),
                answer.raw
            );
            assert_eq!(
                answer.root().find_text("Message"),
                Some(id_format_invalid_message(op.member()).as_str()),
                "case {case}: expected the identifier-format sentence naming {}, got {:?} \
                 with {}",
                op.member(),
                answer.root().find_text("Message"),
                answer.raw
            );
            assert!(
                !answer.raw.contains(identifier.as_str()),
                "case {case}: the refusal echoed {identifier:?} back in {}",
                answer.raw
            );
        }

        // An identifier of the right form the account holds nothing under: the resource is
        // ABSENT, which is a different sentence under the same code and status.
        let case = format!("{op:?}/an identifier of the right form nothing holds");
        let answer = op.call(&router, &case, "EABCDEFGH12345").await;
        assert_eq!(
            answer.status, 404,
            "case {case}: expected HTTP 404, got {} with {}",
            answer.status, answer.raw
        );
        assert_eq!(
            answer.error_code(),
            op.code(),
            "case {case}: expected Code {}, got {:?} with {}",
            op.code(),
            answer.error_code(),
            answer.raw
        );
        assert_eq!(
            answer.root().find_text("Message"),
            Some(op.absent()),
            "case {case}: expected the absent-resource sentence, got {:?} with {}",
            answer.root().find_text("Message"),
            answer.raw
        );

        // The positive control: the identifier the resource IS held under is served, so the
        // precheck refuses none of the calls this operation has to answer.
        let case = format!("{op:?}/the identifier the resource is held under");
        let served = op.call(&router, &case, &held).await;
        assert_eq!(
            served.status,
            op.served_status(),
            "case {case}: expected HTTP {}, got {} with {}",
            op.served_status(),
            served.status,
            served.raw
        );
    }
}
