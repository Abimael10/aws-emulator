// SPDX-License-Identifier: CC0-1.0

//! A grant, or a deny, scoped to the TRANSPORT a real signed IAM call arrived over.
//!
//! RULE under test: IAM decides a statement's `Condition` against the request context it
//! fills itself, and whether the request arrived over an encrypted transport is one of the
//! values it fills (`aws:SecureTransport`, "whether the request was sent using SSL"). So a
//! `Bool` clause over that key is DECIDED from the transport the call was really made over
//! — it is not left undecided, which would make every grant guarded by one inert and apply
//! every plaintext-only deny to an encrypted call. In particular the TLS-enforcement idiom
//! — an unconditional `Allow` of an action beside a `Deny` of the same action conditioned
//! on the key being `false` — serves an encrypted call and refuses a plaintext one.
//!
//! The suites are TABLES whose rows differ along what the rule generalises over: which
//! `Effect` the transport clause guards, which boolean literal it states and in which case,
//! which operator reads the key (`Bool`, `Null`), how the hop the request was made over is
//! stated (not at all, `https`, `http`, several hops, in upper case), and which operation
//! the call is — the rule is about the context every signed request carries, not about one
//! action. The simulator leg pins the agreement the key exists to give: the decision
//! `SimulatePrincipalPolicy` reports for a supplied value is the one the real call gets
//! when it arrives that way.
//!
//! Every assertion drives a `MultiServiceRouter` holding this service over one shared
//! `InMemoryEngine`, exactly as the production router does, so what is under test is the
//! HTTP behaviour of a signed call against PERSISTED policies. The awsQuery harness,
//! including the headers a proxy states a forwarded request's hop in, is the one
//! `tests/all/user_lifecycle.rs` owns, and the simulation harness the one
//! `tests/all/policy_simulation.rs` owns.

use crate::policy_simulation::{reported, simulate, ALLOWED, EXPLICIT_DENY};
use crate::request_condition_context::user_arn;
use crate::user_lifecycle::{call, call_signed_with_headers, error_code, router, HARNESS_REGION};

/// The header a proxy states the scheme of the hop it forwarded in, which is where this
/// gate reads the transport of a forwarded request from.
const FORWARDED_PROTO: &str = "x-forwarded-proto";

/// The condition key every row's clause is written over: "`aws:SecureTransport` — whether
/// the request was sent using SSL" (AWS General Reference, *Global condition context
/// keys*).
const TRANSPORT_KEY: &str = "aws:SecureTransport";

/// A clause that holds for a request that arrived encrypted.
const OVER_TLS: &str = r#"{"Bool":{"aws:SecureTransport":"true"}}"#;

/// A clause that holds for a request that did not — the one AWS's published TLS-enforcement
/// idiom guards its `Deny` with.
const OVER_PLAINTEXT: &str = r#"{"Bool":{"aws:SecureTransport":"false"}}"#;

/// The operations the table is run over, each with the action a statement about it names:
/// the rule is about the context of every signed request, so it holds for a read of the
/// caller's tags exactly as for a read of its access keys.
const OPERATIONS: &[(&str, &str)] = &[
    ("ListUserTags", "iam:ListUserTags"),
    ("ListAccessKeys", "iam:ListAccessKeys"),
];

/// One row: the document's two statements, the hop the request states it was made over, and
/// whether the call is then served.
struct Case {
    case: &'static str,
    /// The `Condition` the `Allow` carries, or `None` for an unconditional one.
    allow_condition: Option<&'static str>,
    /// The `Condition` of a `Deny` of the same action beside it, or `None` for a document
    /// carrying no `Deny` at all.
    deny_condition: Option<&'static str>,
    /// The scheme a proxy states it forwarded the request over, or `None` for a request
    /// nothing forwarded — which arrived over this emulator's own TLS listener.
    over: Option<&'static str>,
    /// Whether the documents in force then serve the call.
    served: bool,
}

/// Rows spanning the two effects a transport clause can guard, the two boolean literals,
/// the `Null` reading of the same key, and the ways the hop the request was made over is
/// stated.
const CASES: &[Case] = &[
    Case {
        case: "the TLS-enforcement idiom, over the listener's own transport",
        allow_condition: None,
        deny_condition: Some(OVER_PLAINTEXT),
        over: None,
        served: true,
    },
    Case {
        case: "the same pair, over a hop stated as encrypted",
        allow_condition: None,
        deny_condition: Some(OVER_PLAINTEXT),
        over: Some("https"),
        served: true,
    },
    Case {
        case: "the same pair, over a plaintext hop",
        allow_condition: None,
        deny_condition: Some(OVER_PLAINTEXT),
        over: Some("http"),
        served: false,
    },
    Case {
        case: "the same pair, over a plaintext hop stated in upper case",
        allow_condition: None,
        deny_condition: Some(OVER_PLAINTEXT),
        over: Some("HTTP"),
        served: false,
    },
    Case {
        // The first entry of the list is the hop the client itself made, which is the one
        // the key describes.
        case: "the same pair, where the client's own hop is the plaintext one",
        allow_condition: None,
        deny_condition: Some(OVER_PLAINTEXT),
        over: Some("http, https"),
        served: false,
    },
    Case {
        case: "the same pair, where the client's own hop is the encrypted one",
        allow_condition: None,
        deny_condition: Some(OVER_PLAINTEXT),
        over: Some("https, http"),
        served: true,
    },
    Case {
        case: "a deny conditioned on an encrypted request refuses the encrypted call",
        allow_condition: None,
        deny_condition: Some(OVER_TLS),
        over: None,
        served: false,
    },
    Case {
        case: "that same deny stands aside for a plaintext hop",
        allow_condition: None,
        deny_condition: Some(OVER_TLS),
        over: Some("http"),
        served: true,
    },
    Case {
        case: "a grant conditioned on an encrypted request serves the encrypted call",
        allow_condition: Some(OVER_TLS),
        deny_condition: None,
        over: None,
        served: true,
    },
    Case {
        case: "that same grant withholds a plaintext hop",
        allow_condition: Some(OVER_TLS),
        deny_condition: None,
        over: Some("http"),
        served: false,
    },
    Case {
        case: "a grant conditioned on a plaintext request withholds the encrypted call",
        allow_condition: Some(OVER_PLAINTEXT),
        deny_condition: None,
        over: None,
        served: false,
    },
    Case {
        case: "that same grant serves a plaintext hop",
        allow_condition: Some(OVER_PLAINTEXT),
        deny_condition: None,
        over: Some("http"),
        served: true,
    },
    Case {
        // "The two literals are compared case-insensitively, which is how a document may
        // spell them" — the shared evaluation's reading of IAM's `Bool` operator.
        case: "the boolean literal is compared without case",
        allow_condition: Some(r#"{"Bool":{"aws:SecureTransport":"TRUE"}}"#),
        deny_condition: None,
        over: Some("https"),
        served: true,
    },
    Case {
        // `Null: {key: "false"}` holds exactly where the request CARRIES a value for the
        // key, so a grant guarded by it is served only because the context carries one.
        case: "a grant conditioned on the key carrying a value at all",
        allow_condition: Some(r#"{"Null":{"aws:SecureTransport":"false"}}"#),
        deny_condition: None,
        over: None,
        served: true,
    },
    Case {
        case: "a grant conditioned on the key carrying none withholds the call",
        allow_condition: Some(r#"{"Null":{"aws:SecureTransport":"true"}}"#),
        deny_condition: None,
        over: Some("http"),
        served: false,
    },
];

/// RULE over every row and every operation: the statements in force are decided against the
/// transport the request arrived over, so the call is served exactly where that transport
/// satisfies the clauses.
#[tokio::test]
async fn a_transport_scoped_statement_decides_a_real_signed_call() {
    let router = router();

    for (operation, action) in OPERATIONS {
        for (row, case) in CASES.iter().enumerate() {
            let user_name = format!("transport-{operation}-{row}");
            let document = document(action, &user_arn(&user_name), case);
            let access_key_id = principal(&router, case.case, &user_name, &document).await;

            let stated: Vec<(&str, &str)> = case
                .over
                .map(|scheme| vec![(FORWARDED_PROTO, scheme)])
                .unwrap_or_default();
            let (status, body) = call_signed_with_headers(
                &router,
                Some(&access_key_id),
                HARNESS_REGION,
                &stated,
                operation,
                &[("UserName", &user_name)],
            )
            .await;

            if case.served {
                assert_eq!(
                    status, 200,
                    "case {:?} on {operation}: expected the call made over {:?} to be \
                     served, got {status}: {body}",
                    case.case, case.over
                );
            } else {
                assert_eq!(
                    (status, error_code(&body)),
                    (403, "AccessDenied".to_string()),
                    "case {:?} on {operation}: expected the call made over {:?} to be \
                     withheld, got {status}: {body}",
                    case.case, case.over
                );
            }
        }
    }
}

/// RULE: the transport is a fact of the CONNECTION and of nothing the request's own
/// parameters say, so a request that names the plaintext scheme among its own parameters is
/// still decided against the transport it arrived over.
#[tokio::test]
async fn the_transport_is_read_from_the_connection_and_not_from_the_request() {
    let router = router();
    // A `userNameType` is `^[\w+=,.@-]+$`, which the scheme's own name satisfies, so the
    // request can carry it as one of its own parameters.
    let user_name = "http";
    let document = document(
        "iam:ListUserTags",
        &user_arn(user_name),
        &Case {
            case: "the connection decides",
            allow_condition: None,
            deny_condition: Some(OVER_PLAINTEXT),
            over: None,
            served: true,
        },
    );
    let access_key_id = principal(&router, "the connection decides", user_name, &document).await;

    let (status, body) = call_signed_with_headers(
        &router,
        Some(&access_key_id),
        HARNESS_REGION,
        &[],
        "ListUserTags",
        &[("UserName", user_name)],
    )
    .await;
    assert_eq!(
        status, 200,
        "expected a call that arrived over the emulator's TLS listener to be served \
         however its parameters are spelled, got {status}: {body}"
    );

    // The same call, forwarded over a plaintext hop: the connection is what the clause is
    // decided against, so now the plaintext-only deny applies.
    let (status, refused) = call_signed_with_headers(
        &router,
        Some(&access_key_id),
        HARNESS_REGION,
        &[(FORWARDED_PROTO, "http")],
        "ListUserTags",
        &[("UserName", user_name)],
    )
    .await;
    assert_eq!(
        (status, error_code(&refused)),
        (403, "AccessDenied".to_string()),
        "expected the same call forwarded over a plaintext hop to be withheld, got \
         {status}: {refused}"
    );
}

/// RULE: the simulator and the real call answer the same stored document the same way for
/// the same transport — the contradiction a request context that carries the key rules out.
///
/// Each row supplies one value for the key to `SimulatePrincipalPolicy` and makes the real
/// signed call over the matching transport; the decision reported and the outcome served
/// must agree.
#[tokio::test]
async fn the_simulator_and_the_real_call_agree_on_the_transport() {
    /// (the value supplied to the simulation, the hop the real call states, the decision,
    /// whether the real call is served).
    const ROWS: &[(&str, Option<&str>, &str, bool)] = &[
        ("true", None, ALLOWED, true),
        ("true", Some("https"), ALLOWED, true),
        ("false", Some("http"), EXPLICIT_DENY, false),
    ];
    let router = router();

    for (row, (supplied, over, decision, served)) in ROWS.iter().enumerate() {
        let user_name = format!("agreed{row}");
        let arn = user_arn(&user_name);
        let document = document(
            "iam:ListUserTags",
            &arn,
            &Case {
                case: "the idiom both legs decide",
                allow_condition: None,
                deny_condition: Some(OVER_PLAINTEXT),
                over: None,
                served: true,
            },
        );
        let access_key_id = principal(&router, supplied, &user_name, &document).await;

        let context = vec![
            (
                "ContextEntries.member.1.ContextKeyName".to_string(),
                TRANSPORT_KEY.to_string(),
            ),
            (
                "ContextEntries.member.1.ContextKeyValues.member.1".to_string(),
                (*supplied).to_string(),
            ),
            (
                "ContextEntries.member.1.ContextKeyType".to_string(),
                "boolean".to_string(),
            ),
        ];
        let (status, simulated) =
            simulate(&router, &arn, &["iam:ListUserTags"], &[&arn], &context).await;
        assert_eq!(
            status, 200,
            "case {supplied:?}: the simulation must be served, got {status}: {simulated}"
        );
        assert_eq!(
            reported(&simulated),
            vec![("iam:ListUserTags".to_string(), (*decision).to_string())],
            "case {supplied:?}: expected the simulation to report {decision}, got \
             {simulated}"
        );

        let stated: Vec<(&str, &str)> = over
            .map(|scheme| vec![(FORWARDED_PROTO, scheme)])
            .unwrap_or_default();
        let (status, body) = call_signed_with_headers(
            &router,
            Some(&access_key_id),
            HARNESS_REGION,
            &stated,
            "ListUserTags",
            &[("UserName", &user_name)],
        )
        .await;
        if *served {
            assert_eq!(
                status, 200,
                "case {supplied:?}: the real call over {over:?} must agree with the \
                 simulation's {decision}, got {status}: {body}"
            );
        } else {
            assert_eq!(
                (status, error_code(&body)),
                (403, "AccessDenied".to_string()),
                "case {supplied:?}: the real call over {over:?} must agree with the \
                 simulation's {decision}, got {status}: {body}"
            );
        }
    }
}

/// The row's document: an `Allow` of `action` on `resource`, and beside it — for a row that
/// carries one — a `Deny` of the same action on the same resource.
fn document(action: &str, resource: &str, case: &Case) -> String {
    let mut statements = vec![statement("Allow", action, resource, case.allow_condition)];
    if let Some(condition) = case.deny_condition {
        statements.push(statement("Deny", action, resource, Some(condition)));
    }
    format!(
        r#"{{"Version":"2012-10-17","Statement":[{}]}}"#,
        statements.join(",")
    )
}

/// One statement of that document, with its `Condition` element where it has one.
fn statement(effect: &str, action: &str, resource: &str, condition: Option<&str>) -> String {
    match condition {
        Some(condition) => format!(
            r#"{{"Effect":"{effect}","Action":"{action}","Resource":"{resource}","Condition":{condition}}}"#
        ),
        None => format!(
            r#"{{"Effect":"{effect}","Action":"{action}","Resource":"{resource}"}}"#
        ),
    }
}

/// Create the user `user_name`, put `document` on it as its only policy, and answer the
/// access key id a request signs with to speak for it.
///
/// That document is the ONLY one in force, so what serves — or withholds — a call is the
/// clause under test and nothing else.
async fn principal(
    router: &smithy_server_runtime::MultiServiceRouter,
    case: &str,
    user_name: &str,
    document: &str,
) -> String {
    let (status, created) = call(router, "CreateUser", &[("UserName", user_name)]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: the fixture CreateUser must succeed, got {status}: {created}"
    );
    let (status, put) = call(
        router,
        "PutUserPolicy",
        &[
            ("UserName", user_name),
            ("PolicyName", "transport-scoped"),
            ("PolicyDocument", document),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the fixture PutUserPolicy must succeed, got {status}: {put}"
    );
    let (status, key) = call(router, "CreateAccessKey", &[("UserName", user_name)]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: the fixture CreateAccessKey must succeed, got {status}: {key}"
    );
    crate::user_lifecycle::element(&key, "AccessKeyId")
}
