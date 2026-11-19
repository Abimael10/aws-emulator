// SPDX-License-Identifier: CC0-1.0

//! How this service words a request the emulator's AUTHENTICATION layer refused, driven end
//! to end over the `awsJson1_0` wire with request authentication ON — the configuration the
//! emulator serves traffic in.
//!
//! RULE: a request whose SigV4 access key id resolves to no credential this emulator holds
//! failed AUTHENTICATION — no principal was found, so nothing could be authorized — and is
//! refused with this service's unknown-credential code (`UnrecognizedClientException`)
//! carrying AWS's fixed sentence; a request whose key id DOES resolve, whose signature does
//! not verify under that credential's secret, failed the SIGNATURE and is refused
//! `InvalidSignatureException` with the SigV4 layer's own sentence — at the status a fault in
//! the request's own signature material carries rather than the one an identity failure does
//! (`tests/all/signature_presentation_refusals.rs` owns that half of the rule). Neither is
//! `AccessDenied`: that is what a KNOWN principal is answered for an action its policies do
//! not permit, and an SDK branches on exactly this difference — it re-resolves its credential
//! chain on an authentication failure and reports a policy problem on an authorization one.
//! The codes are this service's own, not the identity services': an `awsJson` service and an
//! `awsQuery` one name these two failures differently, so one code cannot be right for both.
//!
//! The rows differ along what the rule generalises over: the SHAPE of the key id the request
//! presented (the long-term `AKIA` class, the temporary `ASIA` class, one of the emulator's
//! own published form, and one a single character off a key the emulator really holds),
//! which of the two credential failures it is, and the operation the refused request named —
//! an account-wide read, a read that names a table, and a MUTATION — so what is answered
//! follows from the credential rather than from the call.
//!
//! The wording itself belongs to the whole tree rather than to this suite, so the sentences
//! are read from the module that owns them (`service_common::authentication_refusal`) and
//! only the CODES — which are this service's own — are stated here.

use bytes::Bytes;
use serde_json::{json, Value};
use service_common::authentication_refusal::{
    SIGNATURE_MISMATCH_MESSAGE, UNRESOLVED_CREDENTIAL_MESSAGE,
};

use crate::emulator::{call, error_type, message, signed_at, verifying_emulator, REGION};

/// The code AWS answers a request whose signing credential resolves to nothing with, on an
/// `awsJson` endpoint. An authentication outcome, so it is NOT `AccessDenied`.
const UNRESOLVED_CREDENTIAL_CODE: &str = "UnrecognizedClientException";

/// The code AWS answers a request whose credential DID resolve and whose signature did not
/// verify with, on an `awsJson` endpoint — named, as that layer's own refusals are, in the
/// namespace this endpoint's SigV4 layer declares its classes in
/// (`tests/all/signature_presentation_refusals.rs` pins the rest of that layer).
const SIGNATURE_MISMATCH_CODE: &str = "com.amazon.coral.service#InvalidSignatureException";

/// The authorization denial, which no row below may be answered: it is what a resolved
/// principal gets for an action its policies do not permit.
const AUTHORIZATION_DENIAL: &str = "AccessDenied";

/// A signature value that is not the one the emulator computes for any of these requests:
/// 64 hex digits, the shape a SigV4 signature has, so what fails is the comparison and not
/// the parse.
const WRONG_SIGNATURE: &str = "0000000000000000000000000000000000000000000000000000000000000000";

/// The table a MUTATION row names, so the store can be read afterwards to show the refused
/// write never happened. The instant each request signs at, and the scope date it falls on,
/// are the harness's ([`signed_at`]).
const REFUSED_TABLE: &str = "AuthenticationRefusalWording";

/// Which credential a row presents.
#[derive(Debug, Clone, Copy)]
enum Presented {
    /// An access key id this emulator never issued, written out as the row names it.
    NeverIssued(&'static str),
    /// The key id the emulator PUBLISHED for this run, one character off: a credential of
    /// exactly the issued form that still resolves to nothing.
    OneCharacterOffThePublishedKey,
    /// The key id the emulator published, which resolves — so what the row is about is the
    /// signature computed under its secret.
    PublishedKey,
}

/// One operation a row presents its credential to, and the body it carries.
///
/// The set spans how a DynamoDB call reaches state: an account-wide read that names nothing
/// (`ListTables`), a read that names a table (`DescribeTable`), and a MUTATION
/// (`CreateTable`) — so a refusal is shown to land ahead of each of them rather than only
/// ahead of a read.
fn calls() -> Vec<(&'static str, Value)> {
    vec![
        ("ListTables", json!({})),
        ("DescribeTable", json!({"TableName": REFUSED_TABLE})),
        (
            "CreateTable",
            json!({
                "TableName": REFUSED_TABLE,
                "KeySchema": [{"AttributeName": "pk", "KeyType": "HASH"}],
                "AttributeDefinitions": [{"AttributeName": "pk", "AttributeType": "S"}],
                "BillingMode": "PAY_PER_REQUEST",
            }),
        ),
    ]
}

/// The published key id with its last character replaced, which is a key id of the issued
/// form that the emulator holds no credential for.
fn one_character_off(access_key_id: &str) -> String {
    let mut characters: Vec<char> = access_key_id.chars().collect();
    let last = characters
        .pop()
        .expect("the emulator's published key id is not empty");
    characters.push(if last == 'A' { 'B' } else { 'A' });
    characters.into_iter().collect()
}

/// Dispatch one `awsJson1_0` call signed with `access_key_id` and a signature that is not
/// the one the emulator computes, answering the HTTP status and the parsed body.
async fn signed_call(
    router: &smithy_server_runtime::MultiServiceRouter,
    access_key_id: &str,
    operation: &str,
    body: &Value,
) -> (u16, Value) {
    let (signing_instant, signing_date) = signed_at();
    let authorization = format!(
        "AWS4-HMAC-SHA256 Credential={access_key_id}/{signing_date}/{REGION}/dynamodb/\
         aws4_request, SignedHeaders=host;x-amz-date, Signature={WRONG_SIGNATURE}"
    );
    let request = http::Request::builder()
        .method("POST")
        .uri("https://dynamodb.us-east-1.amazonaws.com/")
        .header("host", "dynamodb.us-east-1.amazonaws.com")
        .header("content-type", "application/x-amz-json-1.0")
        .header("x-amz-target", format!("DynamoDB_20120810.{operation}"))
        .header("x-amz-date", &signing_instant)
        .header("authorization", authorization)
        .body(Bytes::from(
            serde_json::to_vec(body).expect("the request body serializes"),
        ))
        .expect("a well-formed awsJson request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    (status, parsed)
}

/// The error code and message of an `awsJson` failure document, read through the harness's
/// own readers ([`error_type`] for the whole `__type` a client reads, [`message`] for the
/// sentence).
fn reported(answer: &Value) -> (String, String) {
    (error_type(answer), message(answer))
}

/// One row: the credential the request presented, and the status, code and sentence AWS
/// answers for that credential's failure.
struct Row {
    case: &'static str,
    presented: Presented,
    status: u16,
    code: &'static str,
    message: &'static str,
}

/// The status an identity failure is refused with: no principal was found for the request,
/// so nothing could be authorized.
const IDENTITY_FAILURE_STATUS: u16 = 403;

/// The status a failure of the request's own SIGNATURE is refused with: what is at fault is
/// the material the request carried, which is the client's to fix.
const SIGNATURE_FAILURE_STATUS: u16 = 400;

/// Every credential failure is answered with the code AWS names for THAT failure on this
/// protocol, with AWS's fixed sentence — and never with the authorization denial.
#[tokio::test]
async fn a_credential_failure_is_answered_with_the_code_aws_names_for_it() {
    let (mut router, published) = verifying_emulator();
    let off_by_one = one_character_off(&published);

    let rows = vec![
        Row {
            case: "a never-issued long-term key id",
            presented: Presented::NeverIssued("AKIAIOSFODNN7EXAMPLE"),
            status: IDENTITY_FAILURE_STATUS,
            code: UNRESOLVED_CREDENTIAL_CODE,
            message: UNRESOLVED_CREDENTIAL_MESSAGE,
        },
        Row {
            case: "a never-issued temporary key id",
            presented: Presented::NeverIssued("ASIAIOSFODNN7EXAMPLE"),
            status: IDENTITY_FAILURE_STATUS,
            code: UNRESOLVED_CREDENTIAL_CODE,
            message: UNRESOLVED_CREDENTIAL_MESSAGE,
        },
        Row {
            case: "a never-issued key id of the emulator's own form",
            presented: Presented::NeverIssued("EMULATORDEADBEEF0123"),
            status: IDENTITY_FAILURE_STATUS,
            code: UNRESOLVED_CREDENTIAL_CODE,
            message: UNRESOLVED_CREDENTIAL_MESSAGE,
        },
        Row {
            case: "a key id one character off one the emulator holds",
            presented: Presented::OneCharacterOffThePublishedKey,
            status: IDENTITY_FAILURE_STATUS,
            code: UNRESOLVED_CREDENTIAL_CODE,
            message: UNRESOLVED_CREDENTIAL_MESSAGE,
        },
        Row {
            case: "a key id the emulator holds, with a signature that does not verify",
            presented: Presented::PublishedKey,
            status: SIGNATURE_FAILURE_STATUS,
            code: SIGNATURE_MISMATCH_CODE,
            message: SIGNATURE_MISMATCH_MESSAGE,
        },
    ];

    for Row {
        case,
        presented,
        status: expected_status,
        code: expected_code,
        message: expected_message,
    } in rows
    {
        let access_key_id = match presented {
            Presented::NeverIssued(key) => key.to_string(),
            Presented::OneCharacterOffThePublishedKey => off_by_one.clone(),
            Presented::PublishedKey => published.clone(),
        };
        for (operation, body) in calls() {
            let (status, answer) = signed_call(&router, &access_key_id, operation, &body).await;
            let (code, message) = reported(&answer);
            assert_eq!(
                (status, code.as_str(), message.as_str()),
                (expected_status, expected_code, expected_message),
                "case {case:?} on {operation}: expected {expected_status} {expected_code} \
                 with AWS's own wording, got {status} {answer}"
            );
            assert_ne!(
                code.as_str(), AUTHORIZATION_DENIAL,
                "case {case:?} on {operation}: an authentication failure must not be answered \
                 as an authorization denial, got {answer}"
            );
        }
    }

    // The refused MUTATION never reached a handler: with authentication off again, the
    // account holds no table of the name every refused `CreateTable` above named.
    router.set_request_authentication(false);
    let (status, listed) = call(&router, "ListTables", json!({})).await;
    assert_eq!(
        status, 200,
        "case the store after the refusals: ListTables must be served, got {status} {listed}"
    );
    let names = listed
        .get("TableNames")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    assert!(
        !names.iter().any(|name| name.as_str() == Some(REFUSED_TABLE)),
        "case the store after the refusals: a refused CreateTable must write nothing, got \
         {names:?}"
    );
}
