// SPDX-License-Identifier: CC0-1.0

//! The SDK catalog the runtime REPORTS and the catalog it BINDS are one: every service token
//! a handler can read out of the runtime's own catalog builds a client that resolves to that
//! service, end to end over the wire.
//!
//! RULE: a managed `python3.x` runtime ships the AWS SDK for Python with a client for every
//! service it can call, so `boto3.client(<token>)` for a token the runtime's call seam binds
//! resolves to THAT service's own entry and is never refused as an unknown service — the
//! `botocore.exceptions.UnknownServiceError` sentence lists the catalog, and a token the
//! sentence lists is therefore a token the SDK answers to. A listed token that is bound to
//! nothing would make that sentence a lie: a handler would be told the service is callable
//! and then refused for naming it.
//!
//! This suite differs from `crate::handler_sdk_service_inventory`, which spans the rule over
//! hand-picked rows (one token per protocol class, both spellings of a two-word SDK id, and
//! tokens no service answers to): here the catalog ITSELF is the input, so EVERY token the
//! seam binds is probed — whichever service crate contributed it, and whichever protocol it
//! speaks — and a service that is listed without being bound fails here whether or not anyone
//! thought to write a row for it. The tokens are read from the seam rather than written down,
//! so a service the emulator gains is covered the day it is bound.
//!
//! What each probe asserts is that the client resolved to the ENTRY the token names: the
//! runtime presigns an object URL for the object store alone, and for every other service it
//! answers that no URL was signed and NAMES the service it resolved the token to — so a token
//! that resolved to another service's entry, or to none, is visible in the sentence the guest
//! read.
//!
//! Every token is probed in ONE invocation: presigning sends nothing, so no probed service
//! needs to be mounted, and an invocation is the dearest thing this target does. The suite
//! RUNS a handler, so it takes the target's one invocation stream
//! ([`crate::invoke_request_id::one_invocation_stream_at_a_time`]).

use std::sync::Arc;

use base64::Engine as _;
use serde_json::json;
use service_com_amazonaws_lambda_awsgirapiservice::state::execution::aws_api::reachable_services;
use smithy_server_runtime::MultiServiceRouter;

use crate::event_source_mappings::{await_function_active, call, router_on};
// The one .zip writer this crate's suites package a handler with, exported by the suite that
// first needed it rather than copied here.
use crate::node_invoke_run::package;

/// Where `CreateFunction` is bound.
const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// A well-formed execution role ARN, satisfying the model's `RoleArn` pattern.
const ROLE: &str = "arn:aws:iam::000000000000:role/service-role/lambda-exec";

/// The one service whose URLs this runtime presigns: the object store, whose presigned URL is
/// the one a handler hands out. Every other token resolves and answers that no URL was signed.
const PRESIGNING_TOKEN: &str = "s3";

/// The bucket and key the probe presigns a read of.
const BUCKET: &str = "sdk-catalog-probe";
const KEY: &str = "probe/one.txt";

/// The exception class the SDK raises for a token no service answers to, which no token the
/// catalog lists may be raised as.
const UNKNOWN_SERVICE: &str = "UnknownServiceError";

/// The class a client that RESOLVED raises when the runtime signs no URL for its service.
const NOT_MADE: &str = "SdkCallNotMade";

/// The handler: for every token the event carries it builds a client and asks it to presign a
/// read, which resolves the token against the runtime's SDK and sends nothing. What it reports
/// per token is the exception class the SDK raised and its sentence, so what is asserted is
/// what the SDK gave the guest.
const HANDLER_SOURCE: &str = r#"
import boto3
import botocore.exceptions


def lambda_handler(event, context):
    probed = {}
    for token in event["tokens"]:
        client = boto3.client(token)
        try:
            url = client.generate_presigned_url(
                "get_object",
                Params={"Bucket": event["bucket"], "Key": event["key"]},
                ExpiresIn=120,
            )
        except botocore.exceptions.BotoCoreError as error:
            probed[token] = {"raised": type(error).__name__, "sentence": str(error)}
        else:
            probed[token] = {"raised": None, "url": url}
    return {"probed": probed}
"#;

/// Lambda alone: the probe presigns rather than sends, so no probed service is mounted.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    router_on(&engine)
}

#[tokio::test]
async fn every_token_the_runtimes_catalog_lists_resolves_to_its_own_service() {
    let _stream = crate::invoke_request_id::one_invocation_stream_at_a_time().await;
    let case = "a handler probing every token the runtime's catalog lists";
    let router = emulator();

    let tokens = reachable_services();
    assert!(
        tokens.len() > 1,
        "case {case:?}: expected the catalog to list the services the seam binds, got {tokens:?}"
    );

    let caller = "sdk-catalog-completeness-caller";
    let created = call(
        &router,
        "POST",
        FUNCTIONS_URI,
        &json!({
            "FunctionName": caller,
            "Role": ROLE,
            "Runtime": "python3.12",
            "Handler": "lambda_function.lambda_handler",
            "Timeout": 30,
            "Code": {
                "ZipFile": base64::engine::general_purpose::STANDARD
                    .encode(package(&[("lambda_function.py", HANDLER_SOURCE)])),
            },
        }),
    )
    .await;
    assert_eq!(
        created.status,
        201,
        "case {case:?}: expected CreateFunction to answer 201, got {} {}",
        created.status,
        created.text()
    );
    await_function_active(&router, case, caller).await;

    let answered = call(
        &router,
        "POST",
        &format!("{FUNCTIONS_URI}/{caller}/invocations"),
        &json!({ "tokens": tokens, "bucket": BUCKET, "key": KEY }),
    )
    .await;
    assert_eq!(
        answered.status,
        200,
        "case {case:?}: expected HTTP 200 from Invoke, got {} {}",
        answered.status,
        answered.text()
    );
    let probed = answered.json()["probed"].clone();

    for token in &tokens {
        let reported = probed[token].clone();
        assert!(
            reported.is_object(),
            "case {case:?}: expected the handler to report the token {token:?}, got {reported:?}"
        );
        let raised = reported["raised"].as_str();
        assert_ne!(
            raised,
            Some(UNKNOWN_SERVICE),
            "case {case:?}: the catalog lists {token:?}, so the SDK answers to it; got \
             {reported:?}"
        );
        if *token == PRESIGNING_TOKEN {
            let url = reported["url"].as_str().unwrap_or_default();
            assert!(
                url.contains(&format!("/{BUCKET}/{KEY}?")) && url.contains("X-Amz-Signature="),
                "case {case:?}: expected a presigned object URL for {token:?}, got {reported:?}"
            );
            continue;
        }
        // A client the SDK answers to is one BOUND to the service its token names: the
        // runtime signs no URL for this service and says so, naming the service it resolved
        // the token to.
        assert_eq!(
            raised,
            Some(NOT_MADE),
            "case {case:?}: expected the runtime to report that no URL was signed for \
             {token:?}, got {reported:?}"
        );
        let sentence = reported["sentence"].as_str().unwrap_or_default();
        assert!(
            sentence.contains(token),
            "case {case:?}: expected the sentence for {token:?} to name the service the token \
             resolved to, got {sentence:?}"
        );
    }
}
