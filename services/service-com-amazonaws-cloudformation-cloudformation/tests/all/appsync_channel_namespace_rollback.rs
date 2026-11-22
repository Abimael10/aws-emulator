// SPDX-License-Identifier: CC0-1.0

//! What a stack that declares an `AWS::AppSync::ChannelNamespace` whose `CodeHandlers` AWS
//! AppSync will not compile reports and leaves behind, end to end over the awsQuery wire.
//!
//! RULE under test: **a declared resource whose create the OWNING service refuses is
//! reported as `CREATE_FAILED` carrying that provider's answer in CloudFormation's envelope
//! — the service's own sentence, the diagnostic that service's SDK words for the call, and
//! the `HandlerErrorCode` the answered error code falls in — and the stack ROLLS BACK:
//! it settles `ROLLBACK_COMPLETE`, publishes no outputs, and every resource the create had
//! already made is taken away again, so the deployment reports no success for a resource
//! that was never created. A declaration the same service ACCEPTS reaches
//! `CREATE_COMPLETE`, and what it published is what that service holds.**
//!
//! Why it matters: a pipeline gates on the stack's status and triages a rolled-back
//! deployment by the failed resource's reason. A stack reported `CREATE_COMPLETE` over a
//! namespace whose handlers cannot run hands the client a live Event API that silently does
//! nothing, and leaves the API and its namespace behind for the next deployment to collide
//! with.
//!
//! The rows differ along what the rule generalises over: WHICH way the declared handler
//! module fails to compile (a plain sentence, an unclosed brace, a stray parenthesis), and
//! — as the control — a module the service compiles, which must deploy and read back
//! verbatim.
//!
//! The AppSync half of every assertion is read from AWS AppSync itself over that service's
//! own `restJson1` wire, never out of the answer being checked.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::appsync_event_and_merged_api_resources::appsync;
use crate::harness::{
    assert_sdk_diagnostic, create_stack, failed_resource_reason, quoted_and_class, router,
    settled_stack, settled_stack_status, xml_text, xml_texts, REGION,
};

/// The status a resource its provider could not create records, and the two statuses a
/// stack settles in.
const CREATE_FAILED: &str = "CREATE_FAILED";
const ROLLBACK_COMPLETE: &str = "ROLLBACK_COMPLETE";
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// The SDK's own name for AWS AppSync, as the exception its SDK raises states it: that
/// model's `sdkId`, which is also its `awsService.cloudFormationName`.
const APPSYNC_SERVICE: &str = "AppSync";

/// The `HandlerErrorCode` class a refusal of the REQUEST falls in, a value of the model's own
/// `HandlerErrorCode` enum.
const INVALID_REQUEST: &str = "InvalidRequest";

/// The sentence AWS AppSync answers for `codeHandlers` its APPSYNC_JS runtime cannot
/// compile, which is what the envelope quotes.
const CODE_HANDLERS_SENTENCE: &str = "Error processing APPSYNC_JS code.";

/// The status that refusal is answered with (`BadRequestException`, `httpError` 400).
const REFUSED_STATUS: u16 = 400;

/// The logical id of the declared namespace, as the templates below spell it: the resource
/// whose own event carries the refusal.
const NAMESPACE_RESOURCE: &str = "Namespace";

/// The namespace name every row declares.
const NAMESPACE: &str = "default";

/// Handler modules AWS AppSync will not compile, each the way a declaration might carry one.
const BROKEN_HANDLERS: &[(&str, &str, &str)] = &[
    (
        "a plain sentence where a module belongs",
        "appsync-ns-sentence",
        "this is not a valid APPSYNC_JS module",
    ),
    (
        "a handler missing its closing brace",
        "appsync-ns-unclosed",
        "export function onPublish(ctx) { return ctx.events",
    ),
    (
        "a stray parenthesis",
        "appsync-ns-stray-paren",
        "export function onPublish(ctx) { return (((( }",
    ),
];

/// The module the control row declares: the handlers AppSync's own documentation writes.
const SERVED_HANDLERS: &str =
    "export function onPublish(ctx){ return ctx.events; } export function onSubscribe(ctx){}";

/// The template one row deploys: an Event API, one channel namespace on it carrying
/// `code_handlers`, and the API's id as the stack's one output.
fn template(stack: &str, code_handlers: &str) -> String {
    json!({
        "Resources": {
            "EvtApi": {
                "Type": "AWS::AppSync::Api",
                "Properties": {
                    "Name": api_name(stack),
                    "EventConfig": {
                        "AuthProviders": [{ "AuthType": "API_KEY" }],
                        "ConnectionAuthModes": [{ "AuthType": "API_KEY" }],
                        "DefaultPublishAuthModes": [{ "AuthType": "API_KEY" }],
                        "DefaultSubscribeAuthModes": [{ "AuthType": "API_KEY" }],
                    },
                },
            },
            "Namespace": {
                "Type": "AWS::AppSync::ChannelNamespace",
                "Properties": {
                    "ApiId": { "Fn::GetAtt": ["EvtApi", "ApiId"] },
                    "Name": NAMESPACE,
                    "CodeHandlers": code_handlers,
                },
            },
        },
        "Outputs": {
            "ApiId": { "Value": { "Fn::GetAtt": ["EvtApi", "ApiId"] } },
        },
    })
    .to_string()
}

/// The Event API name a row's stack declares.
fn api_name(stack: &str) -> String {
    format!("{stack}-api")
}

/// Whether AWS AppSync holds an `Api` named `name`, read from that service's own collection.
async fn event_api_exists(router: &MultiServiceRouter, case: &str, name: &str) -> bool {
    let (status, page) = appsync(router, "/v2/apis").await;
    assert_eq!(
        status, 200,
        "case {case:?}: ListApis must answer 200, got {status} {page}"
    );
    page.get("apis")
        .and_then(Value::as_array)
        .is_some_and(|apis| {
            apis.iter()
                .any(|api| api.get("name").and_then(Value::as_str) == Some(name))
        })
}

/// A stack whose namespace AppSync refused fails that resource with the provider's own
/// answer and rolls everything it had created back.
#[tokio::test]
async fn a_namespace_whose_handlers_are_refused_rolls_the_stack_back() {
    for (case, stack, code_handlers) in BROKEN_HANDLERS {
        let router = router();
        let (status_code, created) =
            create_stack(&router, stack, &template(stack, code_handlers), &[]).await;
        assert_eq!(
            status_code, 200,
            "case {case:?}: CreateStack must answer 200, got {status_code} {created}"
        );

        // The namespace's own event carries the refusal AppSync answered, in the envelope
        // CloudFormation quotes a resource provider in.
        let reason =
            failed_resource_reason(case, &router, REGION, stack, NAMESPACE_RESOURCE, CREATE_FAILED)
                .await;
        let (quoted, class) = quoted_and_class(case, &reason);
        assert_eq!(
            class, INVALID_REQUEST,
            "case {case:?}: expected the {INVALID_REQUEST} class, got {class:?} in {reason:?}"
        );
        assert!(
            quoted.starts_with(CODE_HANDLERS_SENTENCE),
            "case {case:?}: expected the owning service's own sentence \
             {CODE_HANDLERS_SENTENCE:?}, got {quoted:?}"
        );
        assert_counted_sdk_diagnostic(case, &quoted);

        // The stack rolled back: it settles there, publishes nothing, and the Event API the
        // create had already made is gone from AppSync.
        let described = settled_stack(case, &router, REGION, stack).await;
        let reported = xml_text(&described, "StackStatus").unwrap_or_default();
        assert_eq!(
            reported, ROLLBACK_COMPLETE,
            "case {case:?}: expected the failed create to roll back, got {described}"
        );
        let outputs = xml_texts(&described, "OutputKey");
        assert!(
            outputs.is_empty(),
            "case {case:?}: expected a rolled-back stack to publish no outputs, got {outputs:?}"
        );
        let name = api_name(stack);
        assert!(
            !event_api_exists(&router, case, &name).await,
            "case {case:?}: expected AppSync to hold no Api named {name:?} after the rollback"
        );
    }
}

/// The control: the identical template with handlers AppSync compiles deploys, publishes its
/// output, and the namespace AppSync holds carries that module verbatim.
#[tokio::test]
async fn a_namespace_whose_handlers_compile_deploys_and_reads_back() {
    let case = "the identical stack with a module AppSync compiles";
    let stack = "appsync-ns-served-handlers";
    let router = router();
    let (status_code, created) =
        create_stack(&router, stack, &template(stack, SERVED_HANDLERS), &[]).await;
    assert_eq!(
        status_code, 200,
        "case {case:?}: CreateStack must answer 200, got {status_code} {created}"
    );

    let settled = settled_stack_status(case, &router, REGION, stack).await;
    assert_eq!(
        settled, CREATE_COMPLETE,
        "case {case:?}: expected the stack to deploy, got {settled:?}"
    );

    let described = settled_stack(case, &router, REGION, stack).await;
    let keys = xml_texts(&described, "OutputKey");
    let values = xml_texts(&described, "OutputValue");
    let api_id = keys
        .iter()
        .zip(&values)
        .find(|(key, _)| key.as_str() == "ApiId")
        .map(|(_, value)| value.to_string())
        .unwrap_or_else(|| panic!("case {case:?}: expected the ApiId output, got {described}"));

    // The API the stack published is one AppSync holds, under the name the template asked
    // for...
    let name = api_name(stack);
    assert!(
        event_api_exists(&router, case, &name).await,
        "case {case:?}: expected AppSync to hold the Api named {name:?}"
    );

    // ...and the namespace on it carries the declared module exactly.
    let (status, read) = appsync(
        &router,
        &format!("/v2/apis/{api_id}/channelNamespaces/{NAMESPACE}"),
    )
    .await;
    assert_eq!(
        (status, read["channelNamespace"]["codeHandlers"].as_str()),
        (200, Some(SERVED_HANDLERS)),
        "case {case:?}: expected GetChannelNamespace to answer the declared handlers, got \
         {status} {read}"
    );
}

/// Hold the quoted message to the shape AWS AppSync's SDK words: the service's own sentence,
/// the service as that SDK names it, the status it answered and the id of the call, then the
/// number of attempts the SDK made at it.
///
/// The envelope itself is the harness's reader, which holds a diagnostic that closes AT the
/// call's id; this provider words one more field, so the count is split off first.
fn assert_counted_sdk_diagnostic(case: &str, quoted: &str) {
    const COUNT: &str = " (SDK Attempt Count: ";
    let (envelope, count) = quoted.rsplit_once(COUNT).unwrap_or_else(|| {
        panic!("case {case:?}: expected the SDK's attempt count past the envelope, got {quoted:?}")
    });
    let count = count.strip_suffix(')').unwrap_or_else(|| {
        panic!("case {case:?}: expected the attempt count to close, got {quoted:?}")
    });
    assert!(
        count.parse::<u32>().is_ok_and(|attempts| attempts >= 1),
        "case {case:?}: expected the attempts the SDK made at the call, got {count:?}"
    );
    assert_sdk_diagnostic(case, envelope, APPSYNC_SERVICE, REFUSED_STATUS);
}
