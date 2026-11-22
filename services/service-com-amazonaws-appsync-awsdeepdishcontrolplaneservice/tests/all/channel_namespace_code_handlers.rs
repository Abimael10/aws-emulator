// SPDX-License-Identifier: CC0-1.0

//! The APPSYNC_JS `codeHandlers` a channel namespace is defined by, end to end over HTTP.
//!
//! RULE: a call that would store `codeHandlers` the APPSYNC_JS runtime cannot compile is
//! refused — 400 `BadRequestException` with AppSync's own sentence for this member — and
//! stores nothing, while handlers that runtime reads to the end are stored and read back
//! verbatim. The rule is over the MEMBER, so `CreateChannelNamespace` and
//! `UpdateChannelNamespace` are held to it alike: a refused create leaves the `Api` without
//! that namespace, and a refused update leaves the stored namespace exactly as it was.
//!
//! The tables span what the rule generalises over rather than the one module that exposed
//! the defect: for the refused half, several ways for a module not to be readable (a plain
//! sentence, an unclosed brace, an unterminated literal, a stray parenthesis, a reserved
//! word where a name belongs) at several lengths; for the served half, each handler alone,
//! both together, a module importing the `util` helpers AppSync's own handlers import, a
//! module carrying non-ASCII text, one at the modelled maximum length of `Code`, and a
//! module the engine reads to the end that declares NEITHER handler — because what the
//! compile judges is whether the runtime can read the module, not which functions it
//! exports.
//!
//! Every assertion goes through the router the production emulator mounts, so these
//! exercise routing, restJson1 (de)serialization, the boundary validator, the handlers and
//! the shared engine together. The harness is the error-contract suite's `call`/`router`
//! with the channel-namespace suite's create and read helpers.

use serde_json::{json, Value};
use service_com_amazonaws_appsync_awsdeepdishcontrolplaneservice::state;
use smithy_server_runtime::MultiServiceRouter;

use crate::channel_namespaces::{create_event_api, create_namespace, get_namespace};
use crate::error_contract::{call, router, Answer};

/// `Code`'s modelled maximum length, the bound on `codeHandlers` (`@length` 1..32768,
/// `smithy_query(query="shape", shape="Code")`).
const CODE_MAX_LEN: usize = 32_768;

/// The authorization type the `Api` these namespaces hang off is configured with; which
/// type it is makes no difference to the handler rule, so one is enough.
const AUTH_TYPES: &[&str] = &["API_KEY"];

/// Modules the APPSYNC_JS runtime cannot read, in several forms and at several lengths.
fn broken_handlers() -> Vec<(String, String)> {
    let mut rows: Vec<(String, String)> = [
        (
            "a plain sentence, the form a declaration carried",
            "this is not a valid APPSYNC_JS module".to_string(),
        ),
        (
            "a handler missing its closing brace",
            "export function onPublish(ctx) {\n  return ctx.events\n".to_string(),
        ),
        (
            "an unterminated string literal",
            "export function onPublish(ctx) { return 'unterminated }".to_string(),
        ),
        (
            "a stray parenthesis",
            "export function onPublish(ctx) { return (((( }".to_string(),
        ),
        (
            "a reserved word where a parameter name belongs",
            "export function onPublish(class) { return [] }".to_string(),
        ),
        (
            "an assignment with no target",
            "export const = 5".to_string(),
        ),
    ]
    .into_iter()
    .map(|(label, code)| (label.to_string(), code))
    .collect();

    // LENGTH is one of the things the rule generalises over: a module the engine cannot
    // read is refused whether it is one character or as long as `Code` admits.
    rows.push((
        "a single character that is no module at all".to_string(),
        "}".to_string(),
    ));
    let unreadable_tail = "\nexport function onPublish(ctx) { return 'unterminated }";
    rows.push((
        format!("an unreadable module at the modelled maximum of {CODE_MAX_LEN} characters"),
        format!("{}{unreadable_tail}", comment_of(CODE_MAX_LEN - unreadable_tail.len())),
    ));
    rows
}

/// A readable JavaScript line comment of exactly `length` ASCII characters, for a row that
/// has to reach a length bound without changing what the module says.
fn comment_of(length: usize) -> String {
    let mut comment = String::from("// ");
    comment.push_str(&"f".repeat(length));
    comment.truncate(length);
    comment
}

/// Modules the runtime reads to the end, in the forms a real channel-namespace handler
/// takes.
fn served_handlers() -> Vec<(String, String)> {
    let mut rows: Vec<(String, String)> = [
        (
            "both handlers",
            "export function onPublish(ctx){ return ctx.events; } export function \
             onSubscribe(ctx){}"
                .to_string(),
        ),
        (
            "the publish handler alone",
            "export function onPublish(ctx) { return ctx.events.filter(e => e.payload.keep) }"
                .to_string(),
        ),
        (
            "the subscribe handler alone",
            "export function onSubscribe(ctx) { return null }".to_string(),
        ),
        (
            "a module importing the util helpers",
            "import { util } from '@aws-appsync/utils';\n\
             export function onPublish(ctx) { util.log.info('seen'); return ctx.events }\n"
                .to_string(),
        ),
        (
            "a module whose literals carry non-ASCII text",
            "export function onPublish(ctx) {\n  // héllo 世界 🎉\n  return [{ id: 'Ω≈ç√' }];\n}\n"
                .to_string(),
        ),
        // The compile judges whether the module can be READ, not which functions it
        // exports: a namespace whose module declares neither handler serves a publish the
        // way a namespace with no handlers at all does.
        (
            "a module declaring neither handler",
            "export const unused = 1;\n".to_string(),
        ),
    ]
    .into_iter()
    .map(|(label, code)| (label.to_string(), code))
    .collect();

    // A readable module at the modelled maximum length is served, so the refusals above
    // are about readability rather than about size.
    let handler = "\nexport function onPublish(ctx) { return ctx.events }";
    rows.push((
        format!("a readable module at the modelled maximum of {CODE_MAX_LEN} characters"),
        format!("{}{handler}", comment_of(CODE_MAX_LEN - handler.len())),
    ));
    rows
}

/// A `CreateChannelNamespace` / `UpdateChannelNamespace` body carrying `code_handlers`.
fn body(name: &str, code_handlers: &str) -> Value {
    json!({ "name": name, "codeHandlers": code_handlers })
}

/// `POST /v2/apis/{apiId}/channelNamespaces/{name}`, the update of one namespace.
async fn update_namespace(
    router: &MultiServiceRouter,
    api_id: &str,
    name: &str,
    code_handlers: &str,
) -> Answer {
    call(
        router,
        "POST",
        &format!("/v2/apis/{api_id}/channelNamespaces/{name}"),
        Some(json!({ "codeHandlers": code_handlers })),
    )
    .await
}

/// Assert `answer` is the refusal handlers the runtime could not read take.
fn assert_handler_refusal(case: &str, answer: &Answer) {
    assert_eq!(
        (
            answer.status,
            answer.error_type.as_str(),
            answer.message.as_str()
        ),
        (400, "BadRequestException", state::CODE_HANDLERS_MESSAGE),
        "case {case:?}: expected 400 BadRequestException {:?}, got {answer:?}",
        state::CODE_HANDLERS_MESSAGE
    );
}

/// The namespaces one `Api` holds, as `ListChannelNamespaces` reports them.
async fn listed_names(router: &MultiServiceRouter, api_id: &str) -> Vec<String> {
    let listed = call(
        router,
        "GET",
        &format!("/v2/apis/{api_id}/channelNamespaces"),
        None,
    )
    .await;
    listed.body["channelNamespaces"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|namespace| namespace["name"].as_str().map(str::to_string))
        .collect()
}

/// A create whose handlers the runtime cannot read is refused and leaves the `Api`
/// without that namespace.
#[tokio::test]
async fn a_create_whose_handlers_do_not_compile_is_refused_and_stores_nothing() {
    let router = router();
    let api_id = create_event_api(&router, "Events_bad_handlers", AUTH_TYPES).await;

    for (index, (case, code)) in broken_handlers().into_iter().enumerate() {
        let name = format!("ns-{index}");
        let answer = create_namespace(&router, &api_id, body(&name, &code)).await;
        assert_handler_refusal(&format!("CreateChannelNamespace with {case}"), &answer);

        // Nothing was stored for the refused create: the namespace is not readable and
        // not listed.
        let read = get_namespace(&router, &api_id, &name).await;
        assert_eq!(
            read.status, 404,
            "case {case:?}: expected the Api to hold no namespace {name:?}, got {read:?}"
        );
        let names = listed_names(&router, &api_id).await;
        assert!(
            !names.contains(&name),
            "case {case:?}: expected {name:?} to be absent from the listing, got {names:?}"
        );
    }

    // The refusals left the Api with no namespaces at all.
    let names = listed_names(&router, &api_id).await;
    assert!(
        names.is_empty(),
        "case \"the listing after every refused create\": expected no namespace, got {names:?}"
    );
}

/// An update whose handlers the runtime cannot read is refused the same way and leaves the
/// stored namespace exactly as it was.
#[tokio::test]
async fn an_update_whose_handlers_do_not_compile_is_refused_and_changes_nothing() {
    let router = router();
    let api_id = create_event_api(&router, "Events_update_handlers", AUTH_TYPES).await;
    let name = "default";
    let stored = "export function onPublish(ctx) { return ctx.events }";
    let created = create_namespace(&router, &api_id, body(name, stored)).await;
    assert_eq!(
        created.status, 200,
        "case \"create the namespace the updates leave alone\": expected 200, got {created:?}"
    );

    for (case, code) in broken_handlers() {
        let answer = update_namespace(&router, &api_id, name, &code).await;
        assert_handler_refusal(&format!("UpdateChannelNamespace with {case}"), &answer);

        let read = get_namespace(&router, &api_id, name).await;
        assert_eq!(
            (
                read.status,
                read.body["channelNamespace"]["codeHandlers"].as_str()
            ),
            (200, Some(stored)),
            "case {case:?}: expected the stored handlers unchanged, got {read:?}"
        );
    }
}

/// Handlers the runtime reads to the end are stored and read back verbatim, by a create and
/// by an update alike.
#[tokio::test]
async fn handlers_that_compile_are_stored_and_read_back() {
    let router = router();
    let api_id = create_event_api(&router, "Events_good_handlers", AUTH_TYPES).await;

    for (index, (case, code)) in served_handlers().into_iter().enumerate() {
        let name = format!("ns-{index}");
        let created = create_namespace(&router, &api_id, body(&name, &code)).await;
        assert_eq!(
            (
                created.status,
                created.body["channelNamespace"]["codeHandlers"].as_str()
            ),
            (200, Some(code.as_str())),
            "case CreateChannelNamespace with {case:?}: expected 200 echoing the handlers, got \
             {} {} {:?}",
            created.status,
            created.error_type,
            created.body["channelNamespace"]["codeHandlers"]
                .as_str()
                .map(str::len)
        );

        let read = get_namespace(&router, &api_id, &name).await;
        assert_eq!(
            (
                read.status,
                read.body["channelNamespace"]["codeHandlers"].as_str()
            ),
            (200, Some(code.as_str())),
            "case {case:?}: expected the stored handlers read back, got {} {:?}",
            read.status,
            read.body["channelNamespace"]["codeHandlers"]
                .as_str()
                .map(str::len)
        );

        // The same module is accepted by the UPDATE path, which is the same rule over the
        // same member.
        let updated = update_namespace(&router, &api_id, &name, &code).await;
        assert_eq!(
            (
                updated.status,
                updated.body["channelNamespace"]["codeHandlers"].as_str()
            ),
            (200, Some(code.as_str())),
            "case UpdateChannelNamespace with {case:?}: expected 200 echoing the handlers, got \
             {} {}",
            updated.status,
            updated.error_type
        );
    }
}
