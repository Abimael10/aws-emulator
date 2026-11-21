// SPDX-License-Identifier: CC0-1.0

//! `EvaluateMappingTemplate`, end to end over HTTP: the Velocity template a call carries is
//! RENDERED over the context it carries, and the response is what that render produced.
//!
//! The rules asserted here are stated over CLASSES of input rather than over the one request
//! that exposed the defect:
//!
//! 1. "Evaluates a given template and returns the response": the `evaluationResult` is the
//!    text the caller's own template rendered over the caller's own context, over a table
//!    spanning the language — literal text, a reference walking into the context by member
//!    and by index, both names the context is read under, a method of the language, the
//!    `$util` helpers, `#set`, `#if`/`#else`, `#foreach`, the comments and the unparsed
//!    block;
//! 2. the `stash` "lives through a single resolver run", so the response reports it as the
//!    render left it, and `outErrors` reports the errors the render appended without ending
//!    it;
//! 3. a template that RAISES (`$util.error`, `$util.unauthorized`) is a SERVED call whose
//!    `error` carries that error's own message, and no `evaluationResult`;
//! 4. a template that cannot be evaluated at all — it does not parse, or it names a helper
//!    this reader does not publish — is a 400 `BadRequestException` naming what went wrong,
//!    and a `context` that is not a map is a 400 `BadRequestException` naming that member;
//! 5. the evaluation is a READ of nothing: the identical call answers identically, and
//!    nothing one render left behind is reachable from the next.
//!
//! Every assertion goes through the router the production emulator mounts, so these exercise
//! routing, restJson1 (de)serialization, the handler and the template reader together.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The router and the request helper are the crate's shared test harness, reused rather than
// copied.
use super::error_contract::{call, router, Answer};

/// The operation's own path: `POST /v1/dataplane-evaluatetemplate`
/// (`smithy_query(query="operation", operation="EvaluateMappingTemplate")`).
const PATH: &str = "/v1/dataplane-evaluatetemplate";

/// The sentences the tree's one reader of this language states for text it cannot parse,
/// named from that reader rather than written out here.
use service_common::velocity_template::{NOT_AN_ASSIGNMENT, NOT_A_DIRECTIVE, NO_END};

/// Render `template` over `context` and return what the emulator answered.
async fn evaluate(router: &MultiServiceRouter, template: &str, context: &str) -> Answer {
    call(
        router,
        "POST",
        PATH,
        Some(json!({"template": template, "context": context})),
    )
    .await
}

/// The `evaluationResult` of an answer, or `None` where the response carries none.
fn result(answer: &Answer) -> Option<&str> {
    answer.body["evaluationResult"].as_str()
}

/// Rule 1: the `evaluationResult` is the text the template rendered over the context the call
/// supplied.
///
/// The rows differ along what the rule generalises over: which construct of the language the
/// template is written with, which member of the context it reads, and what kind of value
/// that member holds.
#[tokio::test]
async fn the_evaluation_result_is_what_the_template_rendered() {
    let router = router();

    let items = r#"{"result":{"items":[10,20,30]}}"#;
    let one_argument = r#"{"arguments":{"id":"7"}}"#;
    let branch = "#set($id = $ctx.arguments.id)#if($id == \"7\")matched#else missed#end";
    let cases: &[(&str, &str, &str, &str)] = &[
        (
            "text carrying no reference at all",
            "a line of text",
            "{}",
            "a line of text",
        ),
        (
            "the whole context, rendered as JSON",
            "$util.toJson($context)",
            one_argument,
            r#"{"arguments":{"id":"7"}}"#,
        ),
        (
            "a reference walking into the context by member",
            "$context.arguments.id",
            one_argument,
            "7",
        ),
        (
            "the $ctx alias of that same value",
            "$ctx.arguments.id",
            one_argument,
            "7",
        ),
        (
            "a formal reference inside surrounding text",
            "id=${ctx.arguments.id}.",
            one_argument,
            "id=7.",
        ),
        (
            "a quiet reference that names nothing",
            "[$!{ctx.arguments.missing}]",
            one_argument,
            "[]",
        ),
        (
            "a reference that names nothing, written out as Velocity renders one",
            "[$ctx.arguments.missing]",
            one_argument,
            "[$ctx.arguments.missing]",
        ),
        (
            "an index into a list of the context",
            "$ctx.result.items[1]",
            items,
            "20",
        ),
        (
            "a method of the language on that list",
            "$ctx.result.items.size()",
            items,
            "3",
        ),
        (
            "a $util helper over a value of the context",
            "$util.toJson($ctx.result.items)",
            items,
            "[10,20,30]",
        ),
        ("a #set and the #if branch it decides", branch, one_argument, "matched"),
        (
            "the #else branch of that same template",
            branch,
            r#"{"arguments":{"id":"8"}}"#,
            " missed",
        ),
        (
            "a #foreach over a list of the context",
            "#foreach($n in $ctx.result.items)$n#if($foreach.hasNext),#end#end",
            items,
            "10,20,30",
        ),
        ("a comment, which renders nothing", "kept#* dropped *#", "{}", "kept"),
        (
            "an unparsed block, written out as it stands",
            "#[[$ctx.arguments.id]]#",
            one_argument,
            "$ctx.arguments.id",
        ),
        (
            "a request template building a data source operation",
            "{\"operation\": \"GetItem\", \"key\": {\"id\": $util.toJson($ctx.arguments.id)}}",
            one_argument,
            r#"{"operation": "GetItem", "key": {"id": "7"}}"#,
        ),
    ];

    for (case, template, context, expected) in cases {
        let answer = evaluate(&router, template, context).await;
        assert_eq!(
            answer.status, 200,
            "case {case:?}: expected 200, got {} {:?}",
            answer.status, answer.body
        );
        assert_eq!(
            result(&answer),
            Some(*expected),
            "case {case:?}: expected {expected:?}, got {:?}",
            answer.body
        );
        assert!(
            answer.body["error"].is_null(),
            "case {case:?}: a template that rendered reports no error, got {:?}",
            answer.body
        );
        // "A list of logs that were generated by calls to util.log.info and util.log.error":
        // these renders write no line, so the list they report is empty.
        assert_eq!(
            answer.body["logs"],
            json!([]),
            "case {case:?}: expected the lines the render wrote, got {:?}",
            answer.body
        );
    }
}

/// Rule 2: the response reports the stash as the render left it, and the errors it appended.
#[tokio::test]
async fn the_stash_and_the_appended_errors_are_what_the_render_left() {
    let router = router();

    // The stash the REQUEST carried travels back unchanged when the template leaves it alone.
    let carried = evaluate(&router, "kept", r#"{"stash":{"from":"request"}}"#).await;
    assert_eq!(
        (result(&carried), carried.body["stash"].as_str()),
        (Some("kept"), Some(r#"{"from":"request"}"#)),
        "case a stash the request carried: expected it reported back as it stands, got {:?}",
        carried.body
    );

    // A template that WRITES the stash is reported with what it wrote.
    let written = evaluate(
        &router,
        "#set($ctx.stash.carried = \"yes\")rendered",
        r#"{"stash":{}}"#,
    )
    .await;
    assert_eq!(
        (result(&written), written.body["stash"].as_str()),
        (Some("rendered"), Some(r#"{"carried":"yes"}"#)),
        "case a template writing the stash: expected the stash it left, got {:?}",
        written.body
    );

    // A context carrying no stash, and a template writing none, leave nothing to report.
    let none = evaluate(&router, "rendered", "{}").await;
    assert!(
        none.body.get("stash").is_none(),
        "case a run that left no stash: expected no stash reported, got {:?}",
        none.body
    );
    assert!(
        none.body.get("outErrors").is_none(),
        "case a run that appended no error: expected no outErrors reported, got {:?}",
        none.body
    );

    // `$util.appendError` "does not interrupt the evaluation": the render finishes and the
    // errors it appended travel beside its text, in the order it appended them.
    let appended = evaluate(
        &router,
        "$util.appendError(\"noted\")$util.appendError(\"flagged\", \"Flagged\")kept",
        "{}",
    )
    .await;
    assert_eq!(
        result(&appended),
        Some("kept"),
        "case a template appending errors: expected the text it rendered, got {:?}",
        appended.body
    );
    let out_errors: Value = appended.body["outErrors"]
        .as_str()
        .map(|text| serde_json::from_str(text).expect("outErrors is a JSON document"))
        .unwrap_or(Value::Null);
    assert_eq!(
        out_errors,
        json!([
            {"message": "noted", "errorType": null, "data": null, "errorInfo": null},
            {"message": "flagged", "errorType": "Flagged", "data": null, "errorInfo": null},
        ]),
        "case a template appending errors: expected both errors in order, got {:?}",
        appended.body
    );
}

/// Rule 3: a template that RAISES is a served call reporting that error.
#[tokio::test]
async fn a_template_that_raises_is_a_served_call_reporting_the_error() {
    let router = router();

    let field = r#"{"info":{"fieldName":"field","parentTypeName":"Query"}}"#;
    let cases: &[(&str, &str, &str, &str)] = &[
        (
            "the sentence $util.error was given",
            "$util.error(\"boom\")",
            "{}",
            "boom",
        ),
        (
            "the same with an errorType beside it",
            "$util.error(\"boom\", \"Refused\")",
            "{}",
            "boom",
        ),
        (
            "a raise reached through a branch, after text was rendered",
            "#if($ctx.info.fieldName)$util.error(\"refused in a branch\")#end",
            field,
            "refused in a branch",
        ),
        (
            // The refusal AppSync words for $util.unauthorized names the field and the type
            // it belongs to, so it is read out of the context the call supplied.
            "the refusal $util.unauthorized raises",
            "$util.unauthorized()",
            field,
            "Not Authorized to access field on type Query",
        ),
    ];

    for (case, template, context, expected) in cases {
        let answer = evaluate(&router, template, context).await;
        assert_eq!(
            answer.status, 200,
            "case {case:?}: a raise is a served call, got {} {:?}",
            answer.status, answer.body
        );
        assert_eq!(
            answer.body["error"]["message"].as_str(),
            Some(*expected),
            "case {case:?}: expected the error {expected:?}, got {:?}",
            answer.body
        );
        assert!(
            result(&answer).is_none(),
            "case {case:?}: a template that raised rendered no result, got {:?}",
            answer.body
        );
    }
}

/// Rule 4: a template this cannot evaluate, and a context that is not a map, are refused 400
/// `BadRequestException` naming what the request got wrong.
#[tokio::test]
async fn a_request_this_cannot_evaluate_is_a_bad_request() {
    let router = router();

    // Each row states the sentence it must be refused with where the reader of the language
    // publishes one, and `None` where the refusal only has to name the fault itself.
    let cases: &[(&str, &str, &str, Option<&str>)] = &[
        (
            "a block that is never closed",
            "#if($ctx.arguments.id)open",
            r#"{"arguments":{"id":"7"}}"#,
            Some(NO_END),
        ),
        (
            "a loop that is never closed",
            "#foreach($n in [1, 2])$n",
            "{}",
            Some(NO_END),
        ),
        (
            "a directive written without its parentheses",
            "#if $ctx.arguments.id",
            r#"{"arguments":{"id":"7"}}"#,
            Some(NOT_A_DIRECTIVE),
        ),
        (
            "a #set that is no assignment",
            "#set($ctx.arguments.id)",
            r#"{"arguments":{"id":"7"}}"#,
            Some(NOT_AN_ASSIGNMENT),
        ),
        ("an #end closing no block", "rendered#end", "{}", None),
        ("an unterminated string", "#set($x = \"unclosed)", "{}", None),
        (
            "an expression with an operand missing",
            "#if($x ==)yes#end",
            "{}",
            None,
        ),
        (
            "a helper this reader does not publish",
            "$util.noSuchHelper()",
            "{}",
            None,
        ),
        (
            "a context that is a list rather than a map",
            "rendered",
            "[1]",
            Some("Invalid context given."),
        ),
        (
            "a context that is a number",
            "rendered",
            "12",
            Some("Invalid context given."),
        ),
        (
            "a context that is a quoted string",
            "rendered",
            "\"text\"",
            Some("Invalid context given."),
        ),
        (
            "a context that is not JSON at all",
            "rendered",
            "not json",
            Some("Invalid context given."),
        ),
    ];

    for (case, template, context, expected) in cases {
        let answer = evaluate(&router, template, context).await;
        assert_eq!(
            (answer.status, answer.error_type.as_str()),
            (400, "BadRequestException"),
            "case {case:?}: expected 400 BadRequestException, got {} {} {:?}",
            answer.status,
            answer.error_type,
            answer.body
        );
        match expected {
            Some(sentence) => assert_eq!(
                answer.message, *sentence,
                "case {case:?}: expected {sentence:?}, got {:?}",
                answer.message
            ),
            None => assert!(
                !answer.message.is_empty(),
                "case {case:?}: expected the refusal to name the fault, got {:?}",
                answer.body
            ),
        }
        assert!(
            result(&answer).is_none(),
            "case {case:?}: a refused request renders nothing, got {:?}",
            answer.body
        );
    }
}

/// Rule 5: the evaluation is a read of nothing — the identical call answers identically, and
/// what one render left behind is not reachable from the next.
#[tokio::test]
async fn one_render_leaves_nothing_behind_for_the_next() {
    let router = router();

    let template = "#set($ctx.stash.seen = \"once\")$util.toJson($ctx.stash)";
    let first = evaluate(&router, template, "{}").await;
    let second = evaluate(&router, template, "{}").await;
    assert_eq!(
        (first.status, result(&first)),
        (200, Some(r#"{"seen":"once"}"#)),
        "case the first render: expected the stash it wrote, got {} {:?}",
        first.status,
        first.body
    );
    assert_eq!(
        (second.status, result(&second)),
        (first.status, result(&first)),
        "case the identical render again: expected the same answer, got {} {:?}",
        second.status,
        second.body
    );

    // A later call that reads the stash without writing it sees the context IT supplied, so
    // nothing of the renders above reached it.
    let read = evaluate(&router, "$util.toJson($ctx.stash)", r#"{"stash":{}}"#).await;
    assert_eq!(
        (read.status, result(&read)),
        (200, Some("{}")),
        "case a later render reading the stash: expected the caller's own stash, got {} {:?}",
        read.status,
        read.body
    );
}
