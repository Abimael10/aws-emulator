// SPDX-License-Identifier: CC0-1.0

//! How a function test reports a FAILURE, end to end over HTTP.
//!
//! Two rules, each asserted over a table that spans the class rather than the one script or
//! the one payload that first showed the divergence:
//!
//! * a `TestResult` that is served at all reports BOTH halves of the run, and the error
//!   message of a run that FAILED is the service's FIXED sentence identifying the function as
//!   one that is invalid or could not run, followed by the account the runtime gave of the
//!   failure \u2014 which never repeats the generic error label that sentence ends in, and keeps
//!   the error's name where that name says which KIND of failure it was; the event object such
//!   a run reports is the empty JSON object, because the function's result IS the object it
//!   returned and a failed run returned none. A run that failed at nothing reports the EMPTY
//!   STRING, not an absent member;
//! * an event object the function cannot be run against is no result at all, so it is
//!   answered with the modelled server-side test failure (`TestFunctionFailed`, HTTP 500,
//!   fault `Receiver`, carrying that error's own fixed sentence) rather than with argument
//!   validation — on both surfaces of this service that test a function, since both model
//!   that error.
//!
//! The rows of the first table differ along what the rule generalises over: HOW the run
//! failed (the code raised an `Error` whose message it computed from the event object, raised
//! a bare value, raised an error of a named kind, raised nothing with a message at all, read a
//! global that does not exist, never compiled, declared no handler, returned a value that is
//! no event object, returned an event object past the reportable ceiling, or never returned) —
//! because the sentence is a property of "the run failed", not of any one way of failing. The
//! rows of the second table differ along the FORM of the unusable payload (absent, not text,
//! not JSON, truncated JSON, and each JSON type that is not an object).

use service_com_amazonaws_cloudfront_cloudfront2020_05_31::state::MAX_OUTPUT_BYTES;
use smithy_server_runtime::MultiServiceRouter;

use crate::connection_function_test_run::{
    create_one as create_connection_function, result_text as connection_result_text,
    test_connection_function, CONNECTION_OBJECT,
};
use crate::distribution_lifecycle::{emulator, Answer, Node};
use crate::function_lifecycle::{create_function, Create};
use crate::function_test_run::{test_function, VIEWER_REQUEST_EVENT};

/// The FIXED part of the error message a test reports for a run that failed, spelled out here
/// as a literal so a wrong constant in the crate cannot make these assertions pass. The
/// variable part that follows it is whatever the runtime reported about the failure.
const FUNCTION_FAILED_SENTENCE: &str =
    "The CloudFront function associated with the CloudFront distribution is invalid or could \
     not run. Error: ";

/// `TestFunctionFailed`'s sentence — entirely fixed text, no variable parts — as a literal
/// for the same reason.
const TEST_FUNCTION_FAILED_SENTENCE: &str = "The test returned an unexpected result.";

/// The wire code and fault of the modelled server-side test failure
/// (`smithy_query(query="shape", shape="TestFunctionFailed")`: `errorKind` server, `httpError`
/// 500), spelled out rather than derived.
const TEST_FUNCTION_FAILED_CODE: &str = "TestFunctionFailed";
const SERVER_FAULT: &str = "Receiver";
const TEST_FUNCTION_FAILED_STATUS: u16 = 500;

/// The document a result reports for a run that returned no event object.
const EMPTY_EVENT_OBJECT: &str = "{}";

/// Every payload no function can be run against, by the FORM that makes it unusable. A
/// request carrying one of these is well formed on the wire — the blob member is a blob of the
/// modelled length — and it is the TEST that produces no result.
fn unusable_payloads() -> Vec<(&'static str, &'static [u8])> {
    vec![
        ("no bytes at all", &b""[..]),
        ("text that is not JSON", &b"not an event"[..]),
        ("a truncated document", &b"{\"request\":"[..]),
        ("a JSON array", &b"[{\"request\":{}}]"[..]),
        ("a JSON string", &b"\"request\""[..]),
        ("a JSON number", &b"42"[..]),
        ("a JSON boolean", &b"true"[..]),
        ("a JSON null", &b"null"[..]),
        ("bytes that are not UTF-8", &[0x00, 0xff, 0xfe][..]),
    ]
}

/// One failing run: the code a function was stored with and what the VARIABLE part of the
/// reported message must be.
struct Failure {
    case: &'static str,
    code: &'static str,
    reported: Reported,
}

/// What the variable part of a reported message must be, for a row whose failure fixes it.
enum Reported {
    /// Exactly this text: the account the runtime gave, with no label of this service's own
    /// added to it and the generic error name the fixed sentence already states not repeated.
    Exactly(&'static str),
    /// Opens with this error KIND and then says something: a failure whose kind is what
    /// identifies it keeps the name the engine gave it, because that name says more than the
    /// label the fixed sentence ends in.
    Kind(&'static str),
    /// Some account of the failure, whose wording is the engine's own: only its presence is
    /// the rule.
    Any,
}

/// Every way a run can fail, each of which reports the same fixed sentence.
fn failure_table() -> Vec<Failure> {
    vec![
        Failure {
            case: "a handler that raises an Error whose message it computed from the event",
            code: "function handler(event) {\n\
                   \x20   throw new Error('no catalog for ' + event.request.uri);\n\
                   }\n",
            reported: Reported::Exactly("no catalog for /old"),
        },
        Failure {
            case: "a handler that raises a bare string",
            code: "function handler(event) { throw 'rejected'; }\n",
            reported: Reported::Exactly("rejected"),
        },
        Failure {
            case: "a handler that raises an Error with no message",
            code: "function handler(event) { throw new Error(); }\n",
            reported: Reported::Exactly("Error"),
        },
        Failure {
            case: "a handler that raises an error of a named kind",
            code: "function handler(event) { throw new TypeError('bad shape'); }\n",
            reported: Reported::Exactly("TypeError: bad shape"),
        },
        Failure {
            case: "a handler that reads a global that does not exist",
            code: "function handler(event) { return absentGlobal.value; }\n",
            reported: Reported::Kind("ReferenceError"),
        },
        Failure {
            case: "code that never compiled",
            code: "function handler(event) { return event.request\n",
            reported: Reported::Kind("SyntaxError"),
        },
        Failure {
            case: "code that declares no handler",
            code: "var unused = 1;\n",
            reported: Reported::Any,
        },
        Failure {
            case: "a handler that returns a value that is no event object",
            code: "function handler(event) { return 'rewritten'; }\n",
            reported: Reported::Any,
        },
        Failure {
            case: "a handler that returns an event object past the reportable ceiling",
            code: "function handler(event) {\n\
                   \x20   var request = event.request;\n\
                   \x20   request.uri = '/' + 'x'.repeat(41000);\n\
                   \x20   return request;\n\
                   }\n",
            reported: Reported::Any,
        },
        Failure {
            case: "a handler that never returns",
            code: "function handler(event) { while (true) {} }\n",
            reported: Reported::Any,
        },
    ]
}

/// A result that is served at all reports both halves of the run: a run that FAILED reports
/// the fixed sentence followed by what the runtime said, beside the empty event object, and a
/// run that failed at nothing reports the empty string.
#[tokio::test]
async fn a_failed_run_reports_the_fixed_sentence_and_the_runtime_message() {
    let router = emulator();

    for (index, failure) in failure_table().into_iter().enumerate() {
        let case = failure.case;
        let name = format!("failed_run_{index}");
        let etag = create_one(&router, &name, failure.code).await;
        let answer =
            test_function(&router, &name, None, VIEWER_REQUEST_EVENT.as_bytes(), &etag).await;

        assert_eq!(
            answer.status, 200,
            "case {case}: a run that failed is still a run, got {} with {}",
            answer.status, answer.raw
        );
        let result = answer.root();

        let output = member(result, "FunctionOutput", case, &answer);
        assert_eq!(
            output, EMPTY_EVENT_OBJECT,
            "case {case}: a failed run returned no event object, got {output:?}"
        );

        let message = member(result, "FunctionErrorMessage", case, &answer);
        let Some(reported) = message.strip_prefix(FUNCTION_FAILED_SENTENCE) else {
            panic!(
                "case {case}: expected a message opening with \
                 {FUNCTION_FAILED_SENTENCE:?}, got {message:?}"
            );
        };
        assert!(
            !reported.trim().is_empty(),
            "case {case}: expected the runtime's account of the failure after the sentence, \
             got {message:?}"
        );
        assert!(
            !reported.contains(FUNCTION_FAILED_SENTENCE),
            "case {case}: the sentence must be reported once, got {message:?}"
        );
        match failure.reported {
            Reported::Exactly(expected) => assert_eq!(
                reported, expected,
                "case {case}: expected the variable part to be exactly {expected:?}, got \
                 {reported:?}"
            ),
            Reported::Kind(kind) => {
                let said = reported.strip_prefix(kind).and_then(|rest| {
                    rest.strip_prefix(": ").filter(|said| !said.trim().is_empty())
                });
                assert!(
                    said.is_some(),
                    "case {case}: expected the variable part to open with {kind:?} and then \
                     say something, got {reported:?}"
                );
            }
            Reported::Any => {}
        }
    }
}

/// The ceiling on the event object a run may report is a failure like any other, so its
/// message names the ceiling AND opens with the fixed sentence: the sentence does not replace
/// the account of what failed.
#[tokio::test]
async fn the_variable_part_is_what_the_runtime_reported() {
    let router = emulator();
    let name = "oversized_output_sentence";
    let etag = create_one(
        &router,
        name,
        "function handler(event) {\n\
         \x20   var request = event.request;\n\
         \x20   request.uri = '/' + 'x'.repeat(41000);\n\
         \x20   return request;\n\
         }\n",
    )
    .await;

    let answer = test_function(&router, name, None, VIEWER_REQUEST_EVENT.as_bytes(), &etag).await;
    let message = member(answer.root(), "FunctionErrorMessage", "the ceiling", &answer);
    let reported = message
        .strip_prefix(FUNCTION_FAILED_SENTENCE)
        .unwrap_or_else(|| {
            panic!("expected a message opening with {FUNCTION_FAILED_SENTENCE:?}, got {message:?}")
        });
    assert!(
        reported.contains(&MAX_OUTPUT_BYTES.to_string()),
        "expected the variable part to name the {MAX_OUTPUT_BYTES} byte ceiling, got \
         {reported:?}"
    );
}

/// A run that failed at nothing reports the error message member as the EMPTY STRING, present
/// on the wire — not an absent member — whatever the code did with the event object.
#[tokio::test]
async fn a_run_that_failed_at_nothing_reports_the_empty_string() {
    let router = emulator();

    for (index, (case, code)) in [
        (
            "a handler that rewrites the request it was handed",
            "function handler(event) {\n\
             \x20   var request = event.request;\n\
             \x20   request.uri = '/new';\n\
             \x20   return request;\n\
             }\n",
        ),
        (
            "a handler that generates a response",
            "function handler(event) {\n\
             \x20   return { statusCode: 302, statusDescription: 'Found',\n\
             \x20            headers: { location: { value: '/elsewhere' } } };\n\
             }\n",
        ),
        (
            "a handler that logs and returns the event it was handed",
            "function handler(event) {\n\
             \x20   console.log('seen');\n\
             \x20   return event.request;\n\
             }\n",
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let name = format!("quiet_run_{index}");
        let etag = create_one(&router, &name, code).await;
        let answer =
            test_function(&router, &name, None, VIEWER_REQUEST_EVENT.as_bytes(), &etag).await;

        assert_eq!(
            answer.status, 200,
            "case {case}: expected HTTP 200, got {} with {}",
            answer.status, answer.raw
        );
        let message = member(answer.root(), "FunctionErrorMessage", case, &answer);
        assert_eq!(
            message, "",
            "case {case}: a run that failed at nothing reports the empty string, got \
             {message:?}"
        );
        assert!(
            !member(answer.root(), "FunctionOutput", case, &answer).is_empty(),
            "case {case}: expected the event object the run returned, got nothing: {}",
            answer.raw
        );
    }
}

/// An event object the function cannot be run against produces NO result, which is the
/// modelled server-side test failure: HTTP 500, fault `Receiver`, code `TestFunctionFailed`
/// and that error's own fixed sentence — never argument validation, whatever makes the
/// payload unusable.
#[tokio::test]
async fn an_unusable_event_object_is_the_modeled_test_failure() {
    let router = emulator();
    let name = "unusable_event_object";
    let etag = create_one(
        &router,
        name,
        "function handler(event) { return event.request; }\n",
    )
    .await;

    for (case, event) in unusable_payloads() {
        let answer = test_function(&router, name, None, event, &etag).await;
        assert_test_failure(&answer, case);
    }
}

/// The same rule on the other surface of this service that tests a function: a connection
/// object the connection function cannot be run over is the same modelled test failure,
/// because that operation models the same error.
#[tokio::test]
async fn an_unusable_connection_object_is_the_modeled_test_failure() {
    let router = emulator();
    let created = create_connection_function(
        &router,
        "unusable_connection_object",
        "connection.allow();\n",
    )
    .await;

    for (case, object) in unusable_payloads() {
        let answer =
            test_connection_function(&router, &created.id, None, object, &created.etag).await;
        assert_test_failure(&answer, case);
    }
}

/// A connection function that raises reports the same fixed sentence, so one wording answers
/// for every function test of this service.
#[tokio::test]
async fn a_failed_connection_run_reports_the_fixed_sentence() {
    let router = emulator();
    let created = create_connection_function(
        &router,
        "failed_connection_run",
        "throw new Error('no route for this connection');\n",
    )
    .await;

    let answer = test_connection_function(
        &router,
        &created.id,
        None,
        CONNECTION_OBJECT.as_bytes(),
        &created.etag,
    )
    .await;
    assert_eq!(
        answer.status, 200,
        "a run that failed is still a run, got {} with {}",
        answer.status, answer.raw
    );
    let message = connection_result_text(answer.root(), "ConnectionFunctionErrorMessage")
        .unwrap_or_else(|| {
            panic!(
                "the result carries no ConnectionFunctionErrorMessage: {}",
                answer.raw
            )
        });
    let reported = message
        .strip_prefix(FUNCTION_FAILED_SENTENCE)
        .unwrap_or_else(|| {
            panic!("expected a message opening with {FUNCTION_FAILED_SENTENCE:?}, got {message:?}")
        });
    assert!(
        reported.contains("no route for this connection"),
        "expected the variable part to carry what the runtime reported, got {reported:?}"
    );
}

/// Assert `answer` is the modelled server-side test failure, in full: the status, the fault,
/// the code and the one fixed sentence that error carries.
fn assert_test_failure(answer: &Answer, case: &str) {
    assert_eq!(
        answer.status, TEST_FUNCTION_FAILED_STATUS,
        "case {case}: expected HTTP {TEST_FUNCTION_FAILED_STATUS}, got {} with {}",
        answer.status, answer.raw
    );
    assert_eq!(
        answer.error_code(),
        TEST_FUNCTION_FAILED_CODE,
        "case {case}: expected {TEST_FUNCTION_FAILED_CODE}, got {}",
        answer.raw
    );
    let fault = answer.root().find_text("Type").unwrap_or_default();
    assert_eq!(
        fault, SERVER_FAULT,
        "case {case}: expected the {SERVER_FAULT} fault, got {fault:?} in {}",
        answer.raw
    );
    let message = answer.root().find_text("Message").unwrap_or_default();
    assert_eq!(
        message, TEST_FUNCTION_FAILED_SENTENCE,
        "case {case}: expected the sentence {TEST_FUNCTION_FAILED_SENTENCE:?}, got {message:?}"
    );
}

/// One member of the `TestResult` document, or a panic naming the case: both members this
/// suite reads are present on every result that is served at all.
fn member<'a>(result: &'a Node, name: &str, case: &str, answer: &Answer) -> &'a str {
    result.text_at(name).unwrap_or_else(|| {
        panic!("case {case}: the result carries no {name}: {}", answer.raw)
    })
}

/// Create the function `name` with `code` and answer the version token of its `DEVELOPMENT`
/// stage, which is the version a test of that stage is checked against.
async fn create_one(router: &MultiServiceRouter, name: &str, code: &str) -> String {
    let create = Create {
        case: "a function whose test is under test",
        name: name.to_string(),
        comment: "runs under test".to_string(),
        runtime: "cloudfront-js-2.0",
        code: code.as_bytes().to_vec(),
        associations: Vec::new(),
        tags: Vec::new(),
    };
    let answer = create_function(router, &create.xml()).await;
    assert_eq!(
        answer.status, 201,
        "the create of {name} must answer HTTP 201, got {} with {}",
        answer.status, answer.raw
    );
    answer.header("etag").to_string()
}
