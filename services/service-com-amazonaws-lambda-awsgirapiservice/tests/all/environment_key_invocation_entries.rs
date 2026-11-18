// SPDX-License-Identifier: CC0-1.0

//! Which ENTRY POINTS the key a function's ENVIRONMENT VARIABLES are encrypted with decides.
//!
//! THE RULE: a call that RUNS a function begins by decrypting that function's environment
//! variables under the customer managed key `KMSKeyArn` names — "the ARN of the Key Management
//! Service (KMS) customer managed key that's used to encrypt […] your function's environment
//! variables" — so every entry point that would run the handler is refused while that key
//! cannot decrypt them, carries nothing of the plaintext in the refusal, and serves the
//! identical request once the key can decrypt again. Nothing is recorded by the refusal: the
//! key is asked at the point of use, so the direction the key moved is the direction both
//! answers move.
//!
//! The rows differ along what the rule generalises over: WHICH entry point runs the function —
//! the buffered invocation (`Invoke`), the response-streaming invocation
//! (`InvokeWithResponseStream`, which binds the same four KMS failures at 502) and the function
//! URL plane a public URL publishes, which is an invocation of the function reached without
//! this API at all — and whether the key can decrypt (a usable key, the same key disabled, the
//! key enabled again). Each entry point is asserted on its own terms: the two API entries state
//! the modelled exception class and its sentence, and the URL plane states the gateway sentence
//! AWS answers a request it could not run with and no detail of the function's failure.
//!
//! The key really is in the state a row says it is: it is created and moved between states with
//! the key service's own operations, over the same engine.
//! `tests/all/environment_variable_key_state.rs` owns the other half of the same rule — WHICH
//! class of failure each key state is relayed as, and what every projection of the function's
//! configuration reports while the key cannot decrypt.

use std::sync::Arc;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::durable_execution_completion::kms_call;
// The harness of the suite that owns the other half of this rule: the router carrying Lambda
// beside the key service, the key it creates there, the sentence a refused invocation states
// and the envelope around the key service's answer — taken from it rather than written again.
use crate::environment_variable_key_state::{
    created_key, message_of, router_with_kms, FUNCTIONS_URI, INVOKE_DISABLED_SENTENCE,
    RELAY_HEAD, RELAY_TAIL, VARIABLE,
};
use crate::function_lifecycle::{create_function, make_activation_due, send, zip_file, ROLE};
use crate::function_url_data_plane::{given_configured_url, grant, send as plane_request, statement};
use crate::synchronous_invoke_run::package;

/// Where `InvokeWithResponseStream` is bound.
const RESPONSE_STREAMING_URI: &str = "/2021-11-15/functions";

/// The value the function's one environment variable was given — the plaintext that is
/// readable only through a decrypt.
const MARKER: &str = "env-entry-marker";

/// The handler every entry point below runs: it answers the value of that variable as the
/// whole body of a function-URL response, which is the form that serves the URL plane and the
/// two API entries alike — so an answer carrying the value can only have decrypted it.
const HANDLER_SOURCE: &str = r#"
import os


def lambda_handler(event, context):
    return {
        "statusCode": 200,
        "headers": {"content-type": "text/plain"},
        "body": os.environ["SECRET"],
    }
"#;

/// The modelled exception an invocation of a function whose environment key is DISABLED is
/// refused with, at the status the model binds it at
/// (`smithy_query(query="operation", operation="Invoke,InvokeWithResponseStream")`:
/// `KMSDisabledException`, `@httpError(502)`).
const INVOKE_REFUSAL: &str = "KMSDisabledException";
const REFUSAL_STATUS: u16 = 502;

/// The exception class the key service states a refusal of a disabled key under, which is the
/// class Lambda relays after its own sentence.
const KMS_DISABLED_CODE: &str = "DisabledException";

/// The WHOLE body a function URL answers a request it could not run with: AWS's own sentence,
/// and nothing of the function's failure.
const PLANE_REFUSAL_BODY: &str = "Internal Server Error";

/// Which entry point a row reaches the function through.
#[derive(Debug, Clone, Copy)]
enum Entry {
    /// `Invoke`, whose answer IS the handler's payload.
    Buffered,
    /// `InvokeWithResponseStream`, whose answer is the framed event stream.
    Streamed,
    /// The function URL plane, reached at the address the published URL names.
    Url,
}

/// One entry point, and what makes it its own case.
struct Reached {
    case: &'static str,
    entry: Entry,
}

const ENTRIES: &[Reached] = &[
    Reached {
        case: "the buffered invocation",
        entry: Entry::Buffered,
    },
    Reached {
        case: "the response-streaming invocation",
        entry: Entry::Streamed,
    },
    Reached {
        case: "a request to the function's own URL",
        entry: Entry::Url,
    },
];

/// One key state, the key-service call that puts the key into it, and whether the key can
/// decrypt while it holds.
struct KeyState {
    case: &'static str,
    key_call: Option<&'static str>,
    usable: bool,
}

const KEY_STATES: &[KeyState] = &[
    KeyState {
        case: "a key the key service reports as usable",
        key_call: None,
        usable: true,
    },
    KeyState {
        case: "the same key disabled",
        key_call: Some("DisableKey"),
        usable: false,
    },
    KeyState {
        case: "the key enabled again",
        key_call: Some("EnableKey"),
        usable: true,
    },
];

/// What one entry point answered, read the same way whichever channel rendered it.
struct Reply {
    status: u16,
    /// The whole answer as text — the payload for a buffered invocation, the framed event
    /// stream for a streamed one, and the response body for a URL request.
    text: String,
    /// The exception class and the sentence of a refusal this API rendered as one of its own
    /// modelled errors; `None` for an answer that carries no such document.
    refusal: Option<(String, String)>,
}

/// One API invocation of `name`, with an empty event, as the reply this suite reads.
async fn api_reply(router: &MultiServiceRouter, uri: &str) -> Reply {
    let answered = send(router, "POST", uri, "application/json", b"{}".to_vec(), &[]).await;
    let status = answered.status;
    let text = String::from_utf8_lossy(&answered.body).to_string();
    // A refusal of either API entry is one of this operation's own modelled errors, so its
    // class and its sentence are read out of the document it rendered; a served answer carries
    // no such document and is read as the bytes the run produced.
    let refusal = (status != 200).then(|| (answered.error_code(), message_of(&answered)));
    Reply {
        status,
        text,
        refusal,
    }
}

/// The reply one entry point gives for the function `name` published at `host`.
async fn reached(router: &MultiServiceRouter, entry: Entry, name: &str, host: &str) -> Reply {
    match entry {
        Entry::Buffered => api_reply(router, &format!("{FUNCTIONS_URI}/{name}/invocations")).await,
        Entry::Streamed => {
            api_reply(
                router,
                &format!("{RESPONSE_STREAMING_URI}/{name}/response-streaming-invocations"),
            )
            .await
        }
        Entry::Url => {
            let answered = plane_request(router, "GET", host, "/probe", &[], &[]).await;
            Reply {
                status: answered.status,
                text: answered.text(),
                refusal: None,
            }
        }
    }
}

/// RULE: every entry point that RUNS a function decrypts its environment variables under the
/// key `KMSKeyArn` names first, so each is refused while that key cannot decrypt — with none of
/// the plaintext in the refusal — and each serves the identical request once it can.
#[tokio::test]
async fn every_entry_point_that_runs_a_function_follows_its_environment_key() {
    // This suite RUNS handlers, so it takes the target's one invocation stream.
    let _stream = crate::invoke_request_id::one_invocation_stream_at_a_time().await;
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = router_with_kms(&engine);
    let key_arn = created_key(&router).await;
    let name = "env-key-entries";

    let created = create_function(
        &router,
        &json!({
            "FunctionName": name,
            "Role": ROLE,
            "Runtime": "python3.12",
            "Handler": "lambda_function.lambda_handler",
            "Timeout": 10,
            "KMSKeyArn": key_arn,
            "Environment": { "Variables": { VARIABLE: MARKER } },
            "Code": { "ZipFile": zip_file(&package(HANDLER_SOURCE)) },
        }),
    )
    .await;
    assert_eq!(
        created.status,
        201,
        "case \"CreateFunction\": expected 201, got {} {}",
        created.status,
        String::from_utf8_lossy(&created.body)
    );
    make_activation_due(&engine);

    // The third entry point: a public URL of the same function, which admits an anonymous
    // caller only once the function's resource-based policy grants it.
    let endpoint = given_configured_url(&router, name, None, "NONE", &Value::Null).await;
    grant(
        &router,
        name,
        None,
        &statement("public-url", "lambda:InvokeFunctionUrl", "*", "NONE"),
    )
    .await;

    // The sentence a refused invocation of this function states: Lambda's own, the key
    // service's answer about THIS key, and the relayed call's envelope.
    let refusal_head = format!(
        "{INVOKE_DISABLED_SENTENCE} KMS Exception: {KMS_DISABLED_CODE}KMS Message: {key_arn} is \
         disabled.{RELAY_HEAD}"
    );

    for state in KEY_STATES {
        // The relayed request id of every refusal under ONE key state: each refusal is its own
        // call to the key service, so no two of them may name the same call.
        let mut relayed_ids: Vec<String> = Vec::new();
        if let Some(operation) = state.key_call {
            let answered = kms_call(&router, operation, &json!({ "KeyId": key_arn })).await;
            assert_eq!(
                answered.status,
                200,
                "case {:?}: expected 200 from {operation}, got {} {}",
                state.case,
                answered.status,
                String::from_utf8_lossy(&answered.body)
            );
        }

        for reached_by in ENTRIES {
            let case = format!("{} reached through {}", state.case, reached_by.case);
            let reply = reached(&router, reached_by.entry, name, &endpoint.host).await;

            if state.usable {
                assert_eq!(
                    reply.status, 200,
                    "case {case:?}: expected the invocation to run, got {} {:?}",
                    reply.status, reply.text
                );
                match reached_by.entry {
                    Entry::Buffered => {
                        let payload: Value =
                            serde_json::from_str(&reply.text).unwrap_or_else(|error| {
                                panic!(
                                    "case {case:?}: expected the handler's payload as JSON, got \
                                     {error}: {:?}",
                                    reply.text
                                )
                            });
                        assert_eq!(
                            payload.get("body").and_then(Value::as_str),
                            Some(MARKER),
                            "case {case:?}: expected the handler to answer the variable's value, \
                             got {payload}"
                        );
                    }
                    Entry::Streamed => {
                        assert!(
                            reply.text.contains("PayloadChunk")
                                && reply.text.contains("InvokeComplete")
                                && reply.text.contains(MARKER),
                            "case {case:?}: expected the stream to carry the handler's answer, \
                             got {:?}",
                            reply.text
                        );
                    }
                    Entry::Url => {
                        assert_eq!(
                            reply.text, MARKER,
                            "case {case:?}: expected the URL to answer the body the handler \
                             returned, got {:?}",
                            reply.text
                        );
                    }
                }
                continue;
            }

            assert_eq!(
                reply.status, REFUSAL_STATUS,
                "case {case:?}: expected HTTP {REFUSAL_STATUS} while the key cannot decrypt, got \
                 {} {:?}",
                reply.status, reply.text
            );
            // The plaintext is exactly what could not be read, so no refusal carries it.
            assert!(
                !reply.text.contains(MARKER),
                "case {case:?}: expected no environment plaintext in the refusal, got {:?}",
                reply.text
            );
            match reached_by.entry {
                Entry::Url => assert_eq!(
                    reply.text, PLANE_REFUSAL_BODY,
                    "case {case:?}: expected the gateway sentence and no detail of the failure, \
                     got {:?}",
                    reply.text
                ),
                Entry::Buffered | Entry::Streamed => {
                    let (code, message) = reply.refusal.as_ref().unwrap_or_else(|| {
                        panic!(
                            "case {case:?}: expected the modelled error document, got {:?}",
                            reply.text
                        )
                    });
                    assert_eq!(
                        code, INVOKE_REFUSAL,
                        "case {case:?}: expected {INVOKE_REFUSAL}, got {code:?}"
                    );
                    assert!(
                        message.starts_with(&refusal_head) && message.ends_with(RELAY_TAIL),
                        "case {case:?}: expected the refusal to state {refusal_head:?} and end \
                         {RELAY_TAIL:?}, got {message:?}"
                    );
                    // What stands between them is the relayed call's OWN request id, which is
                    // a value of that call: present, and never the same call twice.
                    let relayed = message
                        .strip_prefix(&refusal_head)
                        .and_then(|rest| rest.strip_suffix(RELAY_TAIL))
                        .unwrap_or_default()
                        .to_string();
                    assert!(
                        !relayed.is_empty(),
                        "case {case:?}: expected the relayed call's own request id, got \
                         {message:?}"
                    );
                    assert!(
                        !relayed_ids.contains(&relayed),
                        "case {case:?}: expected each relayed call its own request id, got \
                         {relayed:?} again after {relayed_ids:?}"
                    );
                    relayed_ids.push(relayed);
                }
            }
        }
    }
}
