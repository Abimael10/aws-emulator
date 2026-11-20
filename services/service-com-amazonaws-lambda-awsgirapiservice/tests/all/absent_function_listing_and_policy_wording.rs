// SPDX-License-Identifier: CC0-1.0

//! What a listing of a function's sub-resources, and a read of its policy, SAY when the
//! `FunctionName` names no function they can reach.
//!
//! One rule is under test, over a table of inputs each recorded case is a single row of:
//! **a 404 `ResourceNotFoundException` carries the FIXED sentence AWS words for that
//! operation's own resource class and interpolates nothing of the request — neither the
//! name the caller sent nor the ARN the service resolved it to — unless the identifier is
//! a full ARN whose Region is not the endpoint's, which is a fact about the REQUEST settled
//! before anything is looked up and is refused instead with the sentence naming the two
//! Regions that disagree.**
//!
//! `ResourceNotFoundException` is modelled `{Type, Message}` with `httpError` 404 on all
//! three operations below (`smithy_query(query="operation",
//! operation="ListFunctionEventInvokeConfigs,ListFunctionUrlConfigs,GetPolicy")`), so the
//! `Message` is part of each one's contract.
//!
//! The rows span:
//!
//! * the operation, one per resource class the three sentences belong to — the
//!   asynchronous-invocation configurations of a function, its function URLs, and its
//!   resource-based policy;
//! * the documented `FunctionName` form (bare name, the `function:`-prefixed name, the
//!   partial ARN, this Region's own full ARN), which the sentence must be the same for
//!   because none of it may be interpolated;
//! * the shape of the absent name (one character, hyphen and underscore, the longest a
//!   name-only reference may be), which the sentence must also be the same for;
//! * for the Region half, which function the foreign ARN addresses — one this endpoint
//!   holds under that very name, one the NAMED Region holds, and one nobody holds anywhere
//!   — because the Region is settled before any of that is looked up;
//! * the endpoint Region, two of them, so neither is the one the rule is read off.
//!
//! And the controls, which prove each refusal is about what the request could not reach
//! rather than a blanket refusal: at both endpoints the created function's two listings are
//! served, its policy read is refused with its own fixed sentence while it holds no
//! statement, and the same read is served once `AddPermission` has attached one.
//!
//! The harness this crate's suites share is reused as far as it reaches
//! (`crate::function_lifecycle`); only the dispatch is local, because that harness addresses
//! one endpoint and the Region half of the rule needs two.

use std::sync::Arc;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::function_lifecycle::{
    make_activation_due, package_bytes, router_on, zip_file, Answer, ACCOUNT, ROLE,
};

/// Where `CreateFunction`, `AddPermission` and `GetPolicy` are bound.
const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// The two endpoints every row is driven at. Each holds its own function, so the Region an
/// identifier names is the only thing that differs between the cross-Region rows.
const ENDPOINTS: &[&str] = &["us-east-1", "eu-north-1"];

/// Regions an identifier can name, from the `MasterRegion` member's own pattern
/// (`^ALL|(eusc-)?[a-z]{2}((-gov)|(-iso([a-z]?)))?-[a-z]+-\d{1}$`): the two endpoints above,
/// so a cross-Region row can name a Region that DOES hold a function, plus two Regions this
/// emulator holds nothing in at all. All four are of the `aws` partition every ARN below is
/// built in, so each row is a reference a client could really send.
const REGIONS: &[&str] = &["us-east-1", "eu-north-1", "ap-southeast-2", "sa-east-1"];

/// Dispatch one JSON request to `region`'s endpoint, signed for that Region's scope. Nothing
/// in the signature is verified: the harness turns request authentication off, and the scope
/// is read only for the Region the caller addressed.
async fn call_in(
    router: &MultiServiceRouter,
    region: &str,
    method: &str,
    uri: &str,
    body: &Value,
) -> Answer {
    let payload = if body.is_null() {
        Vec::new()
    } else {
        serde_json::to_vec(body).expect("the test request body must serialize")
    };
    let host = format!("lambda.{region}.amazonaws.com");
    let authorization = format!(
        "AWS4-HMAC-SHA256 Credential=emulator-test-key/20260101/{region}/lambda/aws4_request, \
         SignedHeaders=host, Signature=not-verified"
    );
    let request = http::Request::builder()
        .method(method)
        .uri(format!("http://{host}{uri}"))
        .header("host", host)
        .header("authorization", authorization)
        .header("content-type", "application/json")
        .body(bytes::Bytes::from(payload))
        .expect("the test request must build");
    let response = router.dispatch(request).await;
    let (parts, body) = response.into_parts();
    Answer {
        status: parts.status.as_u16(),
        headers: parts.headers,
        body,
    }
}

/// The response body as text, for a failure message.
fn text(answer: &Answer) -> String {
    String::from_utf8_lossy(&answer.body).to_string()
}

/// The `Message` of a refusal, under either spelling the model's error shapes give it.
fn message(answer: &Answer) -> String {
    answer
        .string("Message")
        .or_else(|| answer.string("message"))
        .unwrap_or_default()
}

/// The function each endpoint holds, named after that endpoint's own Region, so a row can
/// address a function the emulator really holds through the WRONG Region's ARN.
fn held_function(region: &str) -> String {
    format!("absent-wording-probe-{region}")
}

/// The unqualified ARN of the function `name` in `region`.
fn function_arn(region: &str, name: &str) -> String {
    format!("arn:aws:lambda:{region}:{ACCOUNT}:function:{name}")
}

/// The refusal a `FunctionName` naming `named`'s function gets at `endpoint`.
fn not_reachable(named: &str, endpoint: &str) -> String {
    format!("Functions from '{named}' are not reachable in this region ('{endpoint}')")
}

/// `CreateFunction` in `region`, asserting it was accepted.
async fn create_function_in(router: &MultiServiceRouter, region: &str) {
    let answer = call_in(
        router,
        region,
        "POST",
        FUNCTIONS_URI,
        &json!({
            "FunctionName": held_function(region),
            "Role": ROLE,
            "Runtime": "python3.12",
            "Handler": "index.handler",
            "Code": { "ZipFile": zip_file(&package_bytes(64)) },
        }),
    )
    .await;
    assert!(
        answer.status < 400,
        "case {region}: the function must be created, got {} {}",
        answer.status,
        text(&answer)
    );
}

/// One operation taking a `FunctionName`, with the fixed sentence it refuses an identifier
/// it cannot reach a resource through.
struct Call {
    operation: &'static str,
    /// The operation's bound URI, up to the `{FunctionName}` label
    /// (`smithy_query(query="operation", operation=…)`).
    prefix: &'static str,
    /// What follows that label in the same URI.
    suffix: &'static str,
    /// The WHOLE `Message` of this operation's 404, with nothing of the request in it.
    absent_message: &'static str,
    /// Whether this operation is SERVED for a function the endpoint holds. The two
    /// listings are: a function holding no configuration and no function URL has an empty
    /// list of them. The policy read is not: a function holds no policy until a statement
    /// is added, and a read of a document that does not exist is the same 404, worded the
    /// same way.
    served_when_held: bool,
    /// The list member the served answer carries, from the operation's output shape, or
    /// `""` for the one operation that serves no list. A function holding none of the
    /// resource has the member with no entries in it, which is what distinguishes a served
    /// listing from a refusal.
    served_list_member: &'static str,
}

impl Call {
    /// This operation addressed at `reference`.
    fn uri(&self, reference: &str) -> String {
        format!("{}/{reference}{}", self.prefix, self.suffix)
    }
}

/// The three operations under test, each with the sentence recorded for its resource class.
const CALLS: &[Call] = &[
    // Recorded: the asynchronous-invocation configuration family's sentence, carried by
    // `state::FUNCTION_DOES_NOT_EXIST_MESSAGE`, whose own doc comment cites the fixtures it
    // was read off.
    Call {
        operation: "ListFunctionEventInvokeConfigs",
        prefix: "/2019-09-25/functions",
        suffix: "/event-invoke-config/list",
        absent_message: "The function doesn't exist.",
        served_when_held: true,
        served_list_member: "FunctionEventInvokeConfigs",
    },
    // Recorded: the function-URL family's sentence, carried by this operation's own
    // `FUNCTION_DOES_NOT_EXIST`, whose doc comment cites the fixture it was read off.
    Call {
        operation: "ListFunctionUrlConfigs",
        prefix: "/2021-10-31/functions",
        suffix: "/urls",
        absent_message: "Function does not exist",
        served_when_held: true,
        served_list_member: "FunctionUrlConfigs",
    },
    // This read is asked for a DOCUMENT and has none to report whether the function or its
    // policy is what is missing, so it answers ONE sentence for both — the service's
    // canonical not-found text, `state::RESOURCE_NOT_FOUND_MESSAGE`, as this operation's own
    // module header states. That is settled behaviour this file does not introduce: the rows
    // `GetPolicy on a function that holds no policy` and `GetPolicy on a function that does
    // not exist` of `missing_policy_refusal.rs` both pin it, as does
    // `refusal_message_rules.rs` through its `CANONICAL_NOT_FOUND`. What the rows here add
    // is the identifier forms, the second endpoint and the Region half, which is the cause
    // this issue filed against this operation.
    Call {
        operation: "GetPolicy",
        prefix: FUNCTIONS_URI,
        suffix: "/policy",
        absent_message: "The resource you requested does not exist.",
        served_when_held: false,
        served_list_member: "",
    },
];

/// Assert one call's refusal: the modelled 404, its error code, and its `Message` exactly.
async fn assert_refused(
    router: &MultiServiceRouter,
    endpoint: &str,
    call: &Call,
    reference: &str,
    case: &str,
    expected: &str,
) {
    let answer = call_in(router, endpoint, "GET", &call.uri(reference), &Value::Null).await;
    assert_eq!(
        answer.status,
        404,
        "case {case:?}: expected the modelled 404, got {} {}",
        answer.status,
        text(&answer)
    );
    assert!(
        answer.error_code().contains("ResourceNotFoundException"),
        "case {case:?}: expected ResourceNotFoundException, got {:?}",
        answer.error_code()
    );
    assert_eq!(
        message(&answer),
        expected,
        "case {case:?}: expected {expected:?}, got {:?}",
        message(&answer)
    );
}

/// Names no function was ever created under, spanning the shape of a name-only reference:
/// one character, hyphen and underscore together, and the longest such reference the
/// documentation admits ("If you specify only the function name, it is limited to 64
/// characters in length", `smithy_query(query="operation", operation="GetPolicy")`).
fn absent_names() -> Vec<String> {
    vec!["g".to_string(), "ghost_fn-2".to_string(), "g".repeat(64)]
}

/// Every documented `FunctionName` form that addresses the endpoint's OWN Region, labelled.
/// None of them may appear in the refusal, which is what makes them one rule's rows rather
/// than four.
fn local_references(endpoint: &str, name: &str) -> Vec<(&'static str, String)> {
    vec![
        ("the bare name", name.to_string()),
        ("the function:-prefixed name", format!("function:{name}")),
        (
            "the partial ARN, which names no Region",
            format!("{ACCOUNT}:function:{name}"),
        ),
        ("this Region's own full ARN", function_arn(endpoint, name)),
    ]
}

/// THE RULE, absent half: each operation's 404 for a function it cannot reach is that
/// operation's own fixed sentence, whatever form and whatever name the request used.
#[tokio::test]
async fn an_absent_function_is_refused_with_the_operations_own_fixed_sentence() {
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = router_on(&engine);

    for endpoint in ENDPOINTS {
        for call in CALLS {
            for name in absent_names() {
                for (form, reference) in local_references(endpoint, &name) {
                    let case = format!(
                        "{} at {endpoint} on an absent function by {form} ({} characters)",
                        call.operation,
                        name.len()
                    );
                    assert_refused(
                        &router,
                        endpoint,
                        call,
                        &reference,
                        &case,
                        call.absent_message,
                    )
                    .await;
                }
            }
        }
    }
}

/// THE RULE, Region half: a full ARN naming another Region is refused with the sentence
/// naming the two Regions, not with the operation's absent-resource sentence — whether the
/// function it addresses is one this endpoint holds, one the named Region holds, or one
/// nobody holds at all, because the Region is settled before any lookup.
#[tokio::test]
async fn a_function_identifier_of_another_region_names_both_regions_instead() {
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = router_on(&engine);
    for endpoint in ENDPOINTS {
        create_function_in(&router, endpoint).await;
    }
    // The `Pending` -> `Active` move is owed by the clock; bringing each due instant forward
    // is what waiting would do, without waiting.
    make_activation_due(&engine);

    for endpoint in ENDPOINTS {
        for named in REGIONS.iter().filter(|named| *named != endpoint) {
            // Which function the foreign ARN addresses. The second row names a function the
            // emulator really holds in the Region the ARN carries whenever that Region is the
            // other endpoint; the third names one nobody created anywhere.
            let addressed: [(&str, String); 3] = [
                (
                    "a function THIS endpoint holds under that name",
                    held_function(endpoint),
                ),
                (
                    if ENDPOINTS.contains(named) {
                        "a function the named Region holds under that name"
                    } else {
                        "a function named after a Region nothing was created in"
                    },
                    held_function(named),
                ),
                (
                    "a function nobody holds",
                    "never-created-anywhere".to_string(),
                ),
            ];
            for (addresses, name) in addressed {
                for call in CALLS {
                    let case =
                        format!("{} at {endpoint} naming {named}, {addresses}", call.operation);
                    assert_refused(
                        &router,
                        endpoint,
                        call,
                        &function_arn(named, &name),
                        &case,
                        &not_reachable(named, endpoint),
                    )
                    .await;
                }
            }
        }
    }
}

/// THE CONTROLS: each refusal above is about what the request could not reach. The listings
/// of a function that DOES exist are served at both endpoints; its policy read is refused
/// with the same fixed sentence while no statement is attached, and is served once one is.
#[tokio::test]
async fn the_same_calls_are_served_for_a_function_this_endpoint_holds() {
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = router_on(&engine);
    for endpoint in ENDPOINTS {
        create_function_in(&router, endpoint).await;
    }
    make_activation_due(&engine);

    for endpoint in ENDPOINTS {
        let held = held_function(endpoint);
        for (form, reference) in local_references(endpoint, &held) {
            for call in CALLS {
                let case = format!("{} at {endpoint} by {form}", call.operation);
                let answer =
                    call_in(&router, endpoint, "GET", &call.uri(&reference), &Value::Null).await;
                if call.served_when_held {
                    assert_eq!(
                        answer.status,
                        200,
                        "case {case:?}: expected the listing to be served, got {} {}",
                        answer.status,
                        text(&answer)
                    );
                    // The function holds none of the resource, so the served answer is the
                    // modelled list with no entries in it — not an absent member, and not a
                    // refusal.
                    let listed = answer.json();
                    let entries = listed.get(call.served_list_member).and_then(Value::as_array);
                    assert!(
                        entries.is_some_and(|entries| entries.is_empty()),
                        "case {case:?}: expected an empty {} list, got {}",
                        call.served_list_member,
                        text(&answer)
                    );
                } else {
                    // The function resolves and holds no statement, so the document this
                    // read was asked for does not exist — reported with the very sentence
                    // the absent-function rows above got, which is why those rows pin the
                    // wording rather than a distinction this read does not draw.
                    assert_eq!(
                        answer.status,
                        404,
                        "case {case:?}: expected the modelled 404, got {} {}",
                        answer.status,
                        text(&answer)
                    );
                    assert_eq!(
                        message(&answer),
                        call.absent_message,
                        "case {case:?}: expected {:?}, got {:?}",
                        call.absent_message,
                        message(&answer)
                    );
                }
            }
        }

        // One statement attached, and the SAME policy read is served: the refusal above was
        // the absence of the document, not a refusal of the call.
        let granted = call_in(
            &router,
            endpoint,
            "POST",
            &format!("{FUNCTIONS_URI}/{held}/policy"),
            &json!({
                "StatementId": "absent-wording-control",
                "Action": "lambda:InvokeFunction",
                "Principal": "s3.amazonaws.com",
            }),
        )
        .await;
        assert!(
            granted.status < 400,
            "case {endpoint}: the statement must be added, got {} {}",
            granted.status,
            text(&granted)
        );
        let served = call_in(
            &router,
            endpoint,
            "GET",
            &format!("{FUNCTIONS_URI}/{held}/policy"),
            &Value::Null,
        )
        .await;
        assert_eq!(
            served.status,
            200,
            "case {endpoint}: expected GetPolicy to be served once a statement exists, got {} {}",
            served.status,
            text(&served)
        );
        assert!(
            served
                .string("Policy")
                .is_some_and(|policy| policy.contains("absent-wording-control")),
            "case {endpoint}: expected the added statement in the reported Policy, got {}",
            text(&served)
        );
    }
}
