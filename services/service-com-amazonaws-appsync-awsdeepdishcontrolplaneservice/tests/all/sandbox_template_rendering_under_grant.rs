// SPDX-License-Identifier: CC0-1.0

//! `EvaluateMappingTemplate` as a GRANTED principal drives it, end to end over HTTP: the
//! operation behind the authorization gate RENDERS the caller's own Velocity template over the
//! caller's own context and answers 200 carrying that render.
//!
//! The rule under test: "Evaluates a given template and returns the response" — a caller whose
//! identity-based documents allow `appsync:EvaluateMappingTemplate` on the resources such a
//! call is judged against is SERVED, and what it is served is the text the template it
//! submitted renders over the context it submitted, reported through `evaluationResult`
//! (`smithy_query(query="operation", operation="EvaluateMappingTemplate")`: HTTP success 200,
//! required `template` and `context`, output `evaluationResult`/`error`/`logs`/`stash`/
//! `outErrors`). The gate in front of it decides from the documents in force at the moment of
//! the request, so the identical call is refused 403 `AccessDeniedException` once the grant is
//! removed and renders the identical text again once it is re-attached — and no answer about
//! the TEMPLATE itself (one that does not parse) is a transport refusal, because `error` is an
//! output member of that 200 response.
//!
//! The table spans what the rule generalises over: WHICH construct of the language the
//! template is written with (literal text, a reference walking into the context by member, a
//! `$util` helper over the whole context, a `#set` with an `#if`/`#else`, a `#foreach` over a
//! list the context carries) and what the context it reads holds — plus a template that does
//! not parse at all, which is the same served call reporting its diagnostic in the body.
//!
//! `evaluate_mapping_template` pins the render itself without a principal in front of it, and
//! `sandbox_evaluation_authorization` pins the gate's decision for both sandbox evaluations;
//! this suite pins the two TOGETHER — that a granted caller's answer is a real render — over
//! the same IAM harness.

use serde_json::json;
use service_com_amazonaws_appsync_awsdeepdishcontrolplaneservice::service::authorization::{
    ACTION_PREFIX, ANY_RESOURCE,
};

use crate::identity_authorization::{
    allow, arn, default_deny_message, Emulator, Principal, Reply, ACCESS_DENIED,
    ACCESS_DENIED_STATUS,
};

/// The operation's own path and the action IAM judges a call to it by
/// (`smithy_query(query="operation", operation="EvaluateMappingTemplate")`).
const PATH: &str = "/v1/dataplane-evaluatetemplate";
const OPERATION: &str = "EvaluateMappingTemplate";

/// The class an Apache Velocity parse failure names, which the `error` of a template that does
/// not parse carries.
const PARSE_ERROR: &str = "org.apache.velocity.exception.ParseErrorException";

/// One template a granted caller submits, and what the render of it must come to.
struct Row {
    case: &'static str,
    template: &'static str,
    context: &'static str,
    /// The text the template renders, or `None` for a template that cannot be evaluated at
    /// all — which is answered 200 with its diagnostic in `error` instead.
    rendered: Option<&'static str>,
}

/// A context carrying one argument, and one carrying a list, read by several rows below.
const ONE_ARGUMENT: &str = r#"{"arguments":{"id":"42"}}"#;
const A_LIST: &str = r#"{"result":{"items":[1,2,3]}}"#;

/// The `#set` + `#if`/`#else` row's template, rendered over two different contexts so both
/// branches are exercised by the same text.
const BRANCH: &str = "#set($id = $ctx.arguments.id)#if($id == \"42\")match#else nomatch#end";

const ROWS: &[Row] = &[
    Row {
        case: "text carrying no reference",
        template: "a rendered line",
        context: "{}",
        rendered: Some("a rendered line"),
    },
    Row {
        // The context a template is evaluated over is a resolver context, which always has
        // the stash a run writes through, so the whole of it renders with that member.
        case: "the whole context through $util.toJson",
        template: "$util.toJson($context)",
        context: ONE_ARGUMENT,
        rendered: Some(r#"{"arguments":{"id":"42"},"stash":{}}"#),
    },
    Row {
        case: "a reference walking into the context by member",
        template: "$context.arguments.id",
        context: ONE_ARGUMENT,
        rendered: Some("42"),
    },
    Row {
        case: "a #set deciding the #if branch",
        template: BRANCH,
        context: ONE_ARGUMENT,
        rendered: Some("match"),
    },
    Row {
        case: "the #else branch of that same template",
        template: BRANCH,
        context: r#"{"arguments":{"id":"7"}}"#,
        rendered: Some(" nomatch"),
    },
    Row {
        case: "a #foreach over a list the context carries",
        template: "#foreach($n in $ctx.result.items)$n#if($foreach.hasNext)-#end#end",
        context: A_LIST,
        rendered: Some("1-2-3"),
    },
    Row {
        case: "a template whose syntax is broken",
        template: "#if($ctx.arguments.id",
        context: ONE_ARGUMENT,
        rendered: None,
    },
];

/// The document the granted principal carries: the action of this operation on the resources a
/// call addressing no resource of this service is judged against.
fn grant() -> String {
    allow(&format!("{ACTION_PREFIX}:{OPERATION}"), ANY_RESOURCE)
}

/// Submit one row as `principal` and answer what the emulator replied.
async fn evaluate(emulator: &Emulator, principal: &Principal, row: &Row) -> Reply {
    emulator
        .call(
            &principal.access_key_id,
            "POST",
            PATH,
            Some(&json!({"template": row.template, "context": row.context})),
        )
        .await
}

/// Assert `reply` is the SERVED answer the row describes: 200, and either the text the template
/// renders or — for one that does not parse — the Velocity diagnostic in `error` with no
/// `evaluationResult`.
fn assert_served(row: &Row, reply: &Reply, phase: &str) {
    let case = format!("{} ({phase})", row.case);
    assert_eq!(
        reply.status, 200,
        "case {case:?}: expected 200, got {} {:?} with {:?}",
        reply.status, reply.error_code, reply.body
    );
    match row.rendered {
        Some(expected) => {
            assert_eq!(
                reply.body["evaluationResult"].as_str(),
                Some(expected),
                "case {case:?}: expected the text the template rendered {expected:?}, got {:?}",
                reply.body
            );
            assert!(
                reply.body["error"].is_null(),
                "case {case:?}: a template that rendered reports no error, got {:?}",
                reply.body
            );
        }
        None => {
            let message = reply.body["error"]["message"]
                .as_str()
                .unwrap_or_default()
                .to_string();
            assert!(
                message.contains(PARSE_ERROR),
                "case {case:?}: expected the Velocity parse diagnostic, got {message:?} in {:?}",
                reply.body
            );
            assert!(
                reply.body["evaluationResult"].is_null(),
                "case {case:?}: a template that did not parse rendered nothing, got {:?}",
                reply.body
            );
        }
    }
}

/// RULE: a granted caller's `EvaluateMappingTemplate` is served the render of its OWN template
/// over its OWN context, whichever construct of the language that template is written with.
#[tokio::test]
async fn a_granted_caller_is_served_the_render_of_its_own_template() {
    let emulator = Emulator::new();
    let principal = emulator.user("template_render_grant", Some(&grant())).await;

    for row in ROWS {
        let reply = evaluate(&emulator, &principal, row).await;
        assert_served(row, &reply, "granted");
        // "A single stash object lives through a single resolver run": every answer reports
        // the stash and the appended errors the run left, the empty object and the empty list
        // for a run that wrote to neither.
        assert_eq!(
            (
                reply.body["stash"].as_str(),
                reply.body["outErrors"].as_str()
            ),
            (Some("{}"), Some("[]")),
            "case {:?}: expected the members the render left, got {:?}",
            row.case,
            reply.body
        );
    }
}

/// RULE: the grant in force at the moment of the request decides the call, and the render does
/// not change with it — removing the document refuses the identical call 403
/// `AccessDeniedException` with IAM's sentence, and re-attaching the identical document serves
/// the identical render again.
#[tokio::test]
async fn the_render_survives_a_grant_removed_and_re_attached() {
    let emulator = Emulator::new();
    let user_name = "template_render_grant_cycle";
    let principal = emulator.user(user_name, Some(&grant())).await;

    // What each row renders while the grant is in force.
    let mut first = Vec::new();
    for row in ROWS {
        let reply = evaluate(&emulator, &principal, row).await;
        assert_served(row, &reply, "granted");
        first.push(reply.body.clone());
    }

    // The document is gone: every row is refused on the boundary, and the refusal carries
    // nothing of an evaluation.
    emulator.delete_user_policy(user_name).await;
    for row in ROWS {
        let reply = evaluate(&emulator, &principal, row).await;
        assert_eq!(
            (reply.status, reply.error_code.as_str()),
            (ACCESS_DENIED_STATUS, ACCESS_DENIED),
            "case {:?} once the grant is removed: expected {ACCESS_DENIED_STATUS} \
             {ACCESS_DENIED}, got {} {:?} with {:?}",
            row.case,
            reply.status,
            reply.error_code,
            reply.body
        );
        assert_eq!(
            reply.message(),
            default_deny_message(&principal.arn, OPERATION, &arn(ANY_RESOURCE)),
            "case {:?} once the grant is removed: expected IAM's default-deny sentence, got \
             {:?}",
            row.case,
            reply.message()
        );
        assert!(
            reply.body["evaluationResult"].is_null(),
            "case {:?} once the grant is removed: a refusal renders nothing, got {:?}",
            row.case,
            reply.body
        );
    }

    // And the identical document again: the same calls render exactly what they rendered
    // before, so nothing of the refusal cycle reached the evaluation.
    emulator.put_user_policy(user_name, &grant()).await;
    for (row, before) in ROWS.iter().zip(&first) {
        let reply = evaluate(&emulator, &principal, row).await;
        assert_served(row, &reply, "re-granted");
        assert_eq!(
            &reply.body, before,
            "case {:?} once the grant is re-attached: expected the identical answer, got {:?}",
            row.case, reply.body
        );
    }
}
