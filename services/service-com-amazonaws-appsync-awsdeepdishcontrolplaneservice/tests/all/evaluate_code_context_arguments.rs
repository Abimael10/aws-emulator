// SPDX-License-Identifier: CC0-1.0

//! The CONTEXT `EvaluateCode` runs the caller's code over: the arguments map, under both
//! names an APPSYNC_JS `ctx` reaches it by.
//!
//! The rule asserted here, one level above the single request that exposed it:
//!
//! > The arguments a context states are ONE map with two names on the `ctx` the evaluated
//! > code reads — `ctx.arguments` and its short name `ctx.args` — so code reading either
//! > name reads the arguments the request stated, whichever name it stated them under, and
//! > a run that reads them answers an `evaluationResult` with no `error`. A member the
//! > context states itself is never overwritten.
//!
//! The rows span what the rule generalises over: which name the context states the map
//! under, which name the code reads it by, which of the two functions is evaluated, and the
//! shape of the value read out of the map (a string, a number, a nested member, a list
//! element, the whole map).
//!
//! The same walk also carries the stash, which [`crate::evaluate_code`] asserts; this suite
//! is only about the arguments map.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The router and the request helper are the crate's shared test harness, reused rather than
// copied.
use super::error_contract::{call, router, Answer};

/// The operation's own path: `POST /v1/dataplane-evaluatecode`
/// (`smithy_query(query="operation", operation="EvaluateCode")`).
const PATH: &str = "/v1/dataplane-evaluatecode";

/// The only runtime AppSync supports, as `AppSyncRuntime` requires both its members.
const RUNTIME: (&str, &str) = ("APPSYNC_JS", "1.0.0");

/// Evaluate `function` of `code` over `context`.
///
/// The module is completed the way [`super::evaluate_code::mapping`] completes one — a
/// mapping is defined by both exports — so each row below states only the function it is
/// about.
async fn evaluate(
    router: &MultiServiceRouter,
    code: &str,
    context: &str,
    function: &str,
) -> Answer {
    let body = json!({
        "runtime": {"name": RUNTIME.0, "runtimeVersion": RUNTIME.1},
        "code": super::evaluate_code::mapping(code),
        "context": context,
        "function": function,
    });
    call(router, "POST", PATH, Some(body)).await
}

/// One row: a context, the code evaluated over it, and the `evaluationResult` the run must
/// answer.
struct Row {
    case: &'static str,
    context: &'static str,
    code: &'static str,
    function: &'static str,
    result: &'static str,
}

/// The arguments map is reachable under both names, whichever name the context states.
#[tokio::test]
async fn the_arguments_a_context_states_are_read_under_either_name() {
    let router = router();

    let rows: &[Row] = &[
        Row {
            case: "arguments stated, read as ctx.args",
            context: r#"{"arguments": {"msg": "one"}}"#,
            code: "export function response(ctx) { return \"v:\" + ctx.args.msg; }",
            function: "response",
            result: "\"v:one\"",
        },
        Row {
            case: "arguments stated, read as ctx.arguments",
            context: r#"{"arguments": {"msg": "two"}}"#,
            code: "export function response(ctx) { return \"v:\" + ctx.arguments.msg; }",
            function: "response",
            result: "\"v:two\"",
        },
        Row {
            case: "args stated, read as ctx.arguments",
            context: r#"{"args": {"msg": "three"}}"#,
            code: "export function response(ctx) { return \"v:\" + ctx.arguments.msg; }",
            function: "response",
            result: "\"v:three\"",
        },
        Row {
            case: "args stated, read as ctx.args",
            context: r#"{"args": {"msg": "four"}}"#,
            code: "export function response(ctx) { return \"v:\" + ctx.args.msg; }",
            function: "response",
            result: "\"v:four\"",
        },
        Row {
            case: "the request function reading the map",
            context: r#"{"arguments": {"id": "7"}}"#,
            code: "export function request(ctx) { return {operation: \"GetItem\", \
                   key: {id: ctx.args.id}}; }",
            function: "request",
            result: r#"{"operation":"GetItem","key":{"id":"7"}}"#,
        },
        Row {
            case: "a number read out of the map",
            context: r#"{"arguments": {"limit": 25}}"#,
            code: "export function response(ctx) { return ctx.args.limit + 1; }",
            function: "response",
            result: "26",
        },
        Row {
            case: "a nested member of the map",
            context: r#"{"arguments": {"input": {"body": "deep"}}}"#,
            code: "export function response(ctx) { return ctx.args.input.body; }",
            function: "response",
            result: "\"deep\"",
        },
        Row {
            case: "an element of a list in the map",
            context: r#"{"arguments": {"tags": ["first", "second"]}}"#,
            code: "export function response(ctx) { return ctx.arguments.tags[1]; }",
            function: "response",
            result: "\"second\"",
        },
        Row {
            case: "the whole map, read under the short name",
            context: r#"{"arguments": {"a": 1}}"#,
            code: "export function response(ctx) { return ctx.args; }",
            function: "response",
            result: r#"{"a":1}"#,
        },
        Row {
            case: "both names stated alike",
            context: r#"{"arguments": {"msg": "same"}, "args": {"msg": "same"}}"#,
            code: "export function response(ctx) { return ctx.args.msg + ctx.arguments.msg; }",
            function: "response",
            result: "\"samesame\"",
        },
        Row {
            case: "a context stating both keeps the value it states under each name",
            context: r#"{"arguments": {"msg": "long"}, "args": {"msg": "short"}}"#,
            code: "export function response(ctx) { return ctx.arguments.msg + \":\" + ctx.args.msg; }",
            function: "response",
            result: "\"long:short\"",
        },
    ];

    for row in rows {
        let case = row.case;
        let answer = evaluate(&router, row.code, row.context, row.function).await;
        assert_eq!(
            answer.status, 200,
            "case {case:?}: expected HTTP 200, got {} with body {}",
            answer.status, answer.body
        );
        assert_eq!(
            answer.body["error"],
            Value::Null,
            "case {case:?}: expected no error, got {}",
            answer.body
        );
        let result = answer.body["evaluationResult"].as_str().unwrap_or_default();
        assert_eq!(
            result, row.result,
            "case {case:?}: expected the evaluationResult {:?}, got {result:?}",
            row.result
        );
    }
}

/// Nothing is ADDED for a context that states no arguments at all: the map is the caller's,
/// so a run reads it as absent rather than as an empty one the request never stated.
#[tokio::test]
async fn a_context_stating_no_arguments_has_none_invented_for_it() {
    let router = router();

    // (case, the context, the name read)
    let rows: &[(&str, &str, &str)] = &[
        ("no arguments at all", r#"{"result": {}}"#, "ctx.args"),
        (
            "no arguments, read under the long name",
            r#"{"result": {}}"#,
            "ctx.arguments",
        ),
        (
            "a context that states them, for contrast",
            r#"{"arguments": {"msg": "stated"}}"#,
            "ctx.args",
        ),
    ];

    for (case, context, read) in rows {
        let expected = match context.contains("arguments") {
            true => "\"object\"",
            false => "\"undefined\"",
        };
        let code =
            format!("export function response(ctx) {{ return typeof {read}; }}");
        let answer = evaluate(&router, &code, context, "response").await;
        assert_eq!(
            answer.body["error"],
            Value::Null,
            "case {case:?}: expected no error, got {}",
            answer.body
        );
        let result = answer.body["evaluationResult"].as_str().unwrap_or_default();
        assert_eq!(
            result, expected,
            "case {case:?}: expected the run to read {read} as {expected}, got {result:?}"
        );
    }
}
