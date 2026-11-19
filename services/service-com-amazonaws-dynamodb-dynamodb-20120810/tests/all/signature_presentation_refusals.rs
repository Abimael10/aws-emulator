// SPDX-License-Identifier: CC0-1.0

//! How this endpoint answers a request whose SigV4 signature PRESENTATION is at fault,
//! driven end to end over the `awsJson1_0` wire.
//!
//! RULE: the front door reads HOW a request presents its signature before any credential is
//! resolved, and a fault in that presentation is a fault in the REQUEST — answered HTTP 400
//! naming the fault, never as an unrecognised credential:
//!
//! * a request that presents no signature at all — no `Authorization` header naming an AWS
//!   signature, and no signature in the query string — is refused with the missing-token
//!   class and its fixed sentence;
//! * a presentation with components MISSING is refused with the incomplete-signature class,
//!   whose sentence lists one clause for every component the request left out, and none for
//!   a component it supplied;
//! * a request presenting its signature in BOTH of SigV4's forms names two auth mechanisms
//!   where exactly one is allowed, and is refused with the one-member document this endpoint
//!   answers that fault with — the signature class and nothing else — ahead of the
//!   operation, so nothing is served;
//! * a complete presentation whose signature does not verify is refused with the signature
//!   class and the SigV4 layer's own sentence, at the same 400.
//!
//! The IDENTITY half is unchanged and is the contrast row: an access key id that resolves to
//! no credential is a fault of the CALLER, not of the request's signature material, and keeps
//! this service's unknown-credential answer (`tests/all/authentication_refusal_wording.rs`
//! owns that rule in full).
//!
//! The rows differ along what the rule generalises over: WHICH form presented the signature
//! (the header, the query string, both, neither), WHICH components of it are missing (all of
//! them, two, one, none), whether the header names an AWS signature at all, and the
//! operation the request named.

use bytes::Bytes;
use serde_json::{json, Value};
use service_common::authentication_refusal::SIGNATURE_MISMATCH_MESSAGE;
use smithy_server_runtime::MultiServiceRouter;

use crate::emulator::{call, emulator, error_type, message, signed_at, verifying_emulator, REGION};

/// The class AWS's front door names a request that presents no signature at all.
const MISSING_TOKEN: &str = "com.amazon.coral.service#MissingAuthenticationTokenException";

/// Its fixed sentence.
const MISSING_TOKEN_MESSAGE: &str = "Request is missing Authentication Token";

/// The class it names a presentation with components missing.
const INCOMPLETE_SIGNATURE: &str = "com.amazon.coral.service#IncompleteSignatureException";

/// The class this `awsJson` endpoint names a fault in the signature a request carried, in
/// the namespace this endpoint's SigV4 layer names its own classes in.
const INVALID_SIGNATURE: &str = "com.amazon.coral.service#InvalidSignatureException";

/// The contrast class: what this service answers an access key id that resolves to no
/// credential with — which this suite only shows is NOT answered for a signature fault.
const UNRECOGNIZED_CLIENT: &str = "UnrecognizedClientException";

/// A signature value of the right shape (64 hex digits) that is not the one this emulator
/// computes for any request below, so what fails is the comparison and not the parse.
const WRONG_SIGNATURE: &str = "0000000000000000000000000000000000000000000000000000000000000000";

/// The host every request below addresses.
const HOST: &str = "dynamodb.us-east-1.amazonaws.com";

/// The table a row that names one addresses. Nothing creates it: every request below is
/// refused ahead of the handler, so what the table would hold is never reached.
const TABLE: &str = "SignaturePresentationRefusals";

/// One request a row sends: how it presents its signature, and the call it names.
struct Probe<'a> {
    /// The `Authorization` header it carries, if any.
    authorization: Option<String>,
    /// The query string it is sent with, if any.
    query: Option<String>,
    /// Whether it states the instant it was signed at in `x-amz-date`.
    dated: bool,
    /// The operation it names, as the whole `X-Amz-Target` value's suffix.
    operation: &'a str,
    /// Its body.
    body: Value,
}

/// Send one probe and read back the status and the parsed document.
async fn send(router: &MultiServiceRouter, probe: &Probe<'_>) -> (u16, Value) {
    let (signing_instant, _) = signed_at();
    let uri = match &probe.query {
        Some(query) => format!("https://{HOST}/?{query}"),
        None => format!("https://{HOST}/"),
    };
    let mut request = http::Request::builder()
        .method("POST")
        .uri(uri)
        .header("host", HOST)
        .header("content-type", "application/x-amz-json-1.0")
        .header(
            "x-amz-target",
            format!("DynamoDB_20120810.{}", probe.operation),
        );
    if let Some(authorization) = &probe.authorization {
        request = request.header("authorization", authorization);
    }
    if probe.dated {
        request = request.header("x-amz-date", &signing_instant);
    }
    let request = request
        .body(Bytes::from(
            serde_json::to_vec(&probe.body).expect("the request body serializes"),
        ))
        .expect("a well-formed awsJson request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    (status, parsed)
}

/// The `__type` and `message` of a refusal document, as a client reads them, through the
/// harness's own readers.
fn reported(answer: &Value) -> (String, String) {
    (error_type(answer), message(answer))
}

/// A complete `Authorization` header signed with `access_key_id` for this region, carrying a
/// signature that is not the one this emulator computes.
fn complete_header(access_key_id: &str) -> String {
    let (_, signing_date) = signed_at();
    format!(
        "AWS4-HMAC-SHA256 Credential={access_key_id}/{signing_date}/{REGION}/dynamodb/\
         aws4_request, SignedHeaders=host;x-amz-date, Signature={WRONG_SIGNATURE}"
    )
}

/// The query string of a presigned presentation signed with `access_key_id`, with the
/// parameters in `without` left out — the way a presigned request arrives with one component
/// missing.
fn presigned_query(access_key_id: &str, without: &[&str]) -> String {
    let (signing_instant, signing_date) = signed_at();
    let scope = format!(
        "{access_key_id}%2F{signing_date}%2F{REGION}%2Fdynamodb%2Faws4_request"
    );
    [
        ("X-Amz-Algorithm", "AWS4-HMAC-SHA256".to_string()),
        ("X-Amz-Credential", scope),
        ("X-Amz-Date", signing_instant),
        ("X-Amz-Expires", "900".to_string()),
        ("X-Amz-SignedHeaders", "host".to_string()),
        ("X-Amz-Signature", WRONG_SIGNATURE.to_string()),
    ]
    .into_iter()
    .filter(|(name, _)| !without.contains(name))
    .map(|(name, value)| format!("{name}={value}"))
    .collect::<Vec<String>>()
    .join("&")
}

/// One row: how the request presents its signature, and the status, class and sentence the
/// front door answers it with.
struct Row<'a> {
    case: &'static str,
    probe: Probe<'a>,
    status: u16,
    class: &'static str,
    message: String,
}

/// Every way of presenting a signature badly is answered with the class AWS names for THAT
/// fault, at the status a request fault carries — and never as an unrecognised credential.
#[tokio::test]
async fn a_signature_presentation_fault_is_answered_with_the_class_aws_names_for_it() {
    let (router, published) = verifying_emulator();
    let never_issued = "AKIAIOSFODNN7EXAMPLE";

    let rows = vec![
        Row {
            case: "no Authorization header and no presigned query at all",
            probe: Probe {
                authorization: None,
                query: None,
                dated: false,
                operation: "ListTables",
                body: json!({}),
            },
            status: 400,
            class: MISSING_TOKEN,
            message: MISSING_TOKEN_MESSAGE.to_string(),
        },
        Row {
            case: "a header of another authentication scheme, which presents no AWS signature",
            probe: Probe {
                authorization: Some("Bearer 0123456789abcdef".to_string()),
                query: None,
                dated: true,
                operation: "DescribeTable",
                body: json!({"TableName": TABLE}),
            },
            status: 400,
            class: MISSING_TOKEN,
            message: MISSING_TOKEN_MESSAGE.to_string(),
        },
        Row {
            case: "a query string carrying a signature but not the algorithm naming the \
                   mechanism, which presents none either",
            probe: Probe {
                authorization: None,
                query: Some("X-Amz-Signature=abc&X-Amz-SignedHeaders=host".to_string()),
                dated: false,
                operation: "ListTables",
                body: json!({}),
            },
            status: 400,
            class: MISSING_TOKEN,
            message: MISSING_TOKEN_MESSAGE.to_string(),
        },
        Row {
            case: "a bare AWS4 family token: every component of the presentation missing",
            probe: Probe {
                authorization: Some("AWS4-".to_string()),
                query: None,
                dated: false,
                operation: "ListTables",
                body: json!({}),
            },
            status: 400,
            class: INCOMPLETE_SIGNATURE,
            message: "Authorization header requires 'Credential' parameter. Authorization \
                      header requires 'Signature' parameter. Authorization header requires \
                      'SignedHeaders' parameter. Authorization header requires existence of \
                      either a 'X-Amz-Date' or a 'Date' header."
                .to_string(),
        },
        Row {
            case: "a header stating its credential scope alone, dated: two components missing",
            probe: Probe {
                authorization: Some(format!(
                    "AWS4-HMAC-SHA256 Credential={never_issued}/20260101/{REGION}/dynamodb/\
                     aws4_request"
                )),
                query: None,
                dated: true,
                operation: "CreateTable",
                body: json!({
                    "TableName": TABLE,
                    "KeySchema": [{"AttributeName": "pk", "KeyType": "HASH"}],
                    "AttributeDefinitions": [{"AttributeName": "pk", "AttributeType": "S"}],
                    "BillingMode": "PAY_PER_REQUEST",
                }),
            },
            status: 400,
            class: INCOMPLETE_SIGNATURE,
            message: "Authorization header requires 'Signature' parameter. Authorization \
                      header requires 'SignedHeaders' parameter."
                .to_string(),
        },
        Row {
            case: "a presigned presentation missing the headers it signed: that one named",
            probe: Probe {
                authorization: None,
                query: Some(presigned_query(&published, &["X-Amz-SignedHeaders"])),
                dated: false,
                operation: "ListTables",
                body: json!({}),
            },
            status: 400,
            class: INCOMPLETE_SIGNATURE,
            message: "AWS query-string parameters must include 'X-Amz-SignedHeaders'. \
                      Re-examine the query-string parameters."
                .to_string(),
        },
        Row {
            case: "a presigned presentation missing its credential scope and its instant",
            probe: Probe {
                authorization: None,
                query: Some(presigned_query(
                    &published,
                    &["X-Amz-Credential", "X-Amz-Date"],
                )),
                dated: false,
                operation: "ListTables",
                body: json!({}),
            },
            status: 400,
            class: INCOMPLETE_SIGNATURE,
            message: "AWS query-string parameters must include 'X-Amz-Credential'. AWS \
                      query-string parameters must include 'X-Amz-Date'. Re-examine the \
                      query-string parameters."
                .to_string(),
        },
        Row {
            case: "a complete header presentation whose signature does not verify",
            probe: Probe {
                authorization: Some(complete_header(&published)),
                query: None,
                dated: true,
                operation: "DescribeTable",
                body: json!({"TableName": TABLE}),
            },
            status: 400,
            class: INVALID_SIGNATURE,
            message: SIGNATURE_MISMATCH_MESSAGE.to_string(),
        },
        Row {
            case: "a complete presigned presentation whose signature does not verify",
            probe: Probe {
                authorization: None,
                query: Some(presigned_query(&published, &[])),
                dated: false,
                operation: "ListTables",
                body: json!({}),
            },
            status: 400,
            class: INVALID_SIGNATURE,
            message: SIGNATURE_MISMATCH_MESSAGE.to_string(),
        },
        Row {
            // The contrast row: the presentation is complete, so what failed is the
            // IDENTITY it named, which keeps its own answer.
            case: "a complete presentation naming a key id that resolves to no credential",
            probe: Probe {
                authorization: Some(complete_header(never_issued)),
                query: None,
                dated: true,
                operation: "ListTables",
                body: json!({}),
            },
            status: 403,
            class: UNRECOGNIZED_CLIENT,
            message: "The security token included in the request is invalid.".to_string(),
        },
    ];

    for Row {
        case,
        probe,
        status: expected_status,
        class: expected_class,
        message: expected_message,
    } in rows
    {
        let (status, answer) = send(&router, &probe).await;
        let (class, message) = reported(&answer);
        assert_eq!(
            (status, class.as_str(), message.as_str()),
            (expected_status, expected_class, expected_message.as_str()),
            "case {case:?}: expected {expected_status} {expected_class} with AWS's own \
             wording, got {status} {answer}"
        );
    }
}

/// A request presenting its signature in BOTH forms is refused ahead of the operation, with
/// the whole document this endpoint answers that fault with, so the call it named is never
/// served — while a request presenting it in one form (or, with verification off, in none)
/// still reaches its operation.
///
/// The document is the signature class and NOTHING else: the measured answer to this fault
/// carries no `message` member, unlike the three faults the table above pins AWS's sentences
/// for, which is why this row asserts the whole body rather than a code and a sentence.
#[tokio::test]
async fn a_signature_presented_in_both_forms_is_refused_ahead_of_the_operation() {
    // Verification OFF, which is how a request carrying a VALID header presentation beside a
    // query-string one reaches the gate: a verifier reads one form, so a request whose
    // header verifies is served unless the two forms are read ahead of it.
    let router = emulator();

    // The call is served when its signature is presented in ONE form, which is what makes
    // the refusal below about the second presentation and not about the call.
    let (served, listed) = call(&router, "ListTables", json!({})).await;
    assert_eq!(
        served, 200,
        "case one presentation: expected the call served, got {served} with {listed}"
    );

    let both = Probe {
        authorization: Some(complete_header("AKIDTEST")),
        query: Some(presigned_query("AKIDTEST", &[])),
        dated: true,
        operation: "DescribeTable",
        body: json!({"TableName": TABLE}),
    };
    let (status, answer) = send(&router, &both).await;
    assert_eq!(
        (status, &answer),
        (400, &json!({"__type": INVALID_SIGNATURE})),
        "case both forms: expected 400 with the one-member document this endpoint answers \
         this fault with, got {status} {answer}"
    );

    // And the operation it named was not served: the refusal is the whole answer, with no
    // trace of the call's own result or of the table it addressed.
    assert!(
        answer.get("Table").is_none(),
        "case both forms: the operation must not be served, got {answer}"
    );
}
