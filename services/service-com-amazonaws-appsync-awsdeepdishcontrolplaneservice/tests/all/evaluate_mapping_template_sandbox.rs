// SPDX-License-Identifier: CC0-1.0

//! `EvaluateMappingTemplate`, end to end over HTTP: the SANDBOX's own answers — the resolver
//! context a template is evaluated against, the directives that end an evaluation early, the
//! `$util` families a template marshals and logs through, and the envelope each outcome is
//! reported in.
//!
//! The rules asserted here are stated over CLASSES of input rather than over the one request
//! that exposed each defect:
//!
//! 1. the field's arguments are ONE value of the context under both names a resolver context
//!    gives it, so a template written with either name reads the arguments the call stated
//!    under either name (rows over which name each side writes and over what the map holds);
//! 2. `$util.dynamodb.<helper>` renders a value as the attribute value DynamoDB holds it as,
//!    and the `…Json` sibling of each the same representation serialized — rows over the
//!    attribute types of the catalog this service marshals
//!    (`graphql_api_dynamodb_marshalling` asserts the same table on the data plane);
//! 3. `#foreach` runs its body once per element and `#set` arithmetic accumulates across
//!    those passes, so a projection over a list of the context renders one entry per element
//!    and the totals the passes added up;
//! 4. `#return` ends the evaluation where it stands: the result is the value it names (or the
//!    text rendered up to it, where it names none) and nothing written after it is rendered;
//! 5. a mock `context` whose properties are not of the kind a resolver context gives them is a
//!    SERVED answer naming each offending property by its dotted path, with nothing evaluated
//!    over it — rows over each property of the shape and each kind that is not a map, and over
//!    BOTH halves of the sandbox, which take the same member of the same context;
//! 6. the envelope of every run that HAPPENED carries the stash it left and the errors it
//!    appended, and the log lines only of a run that reached the end of its template: a run
//!    an early `#return` or a RAISE cut short reports no log list, exactly as the APPSYNC_JS
//!    half of this sandbox reports one (`evaluate_code_response_envelope`).
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

/// The `outErrors` of an answer, read back as the JSON list it carries.
fn out_errors(answer: &Answer) -> Value {
    answer.body["outErrors"]
        .as_str()
        .map(|text| serde_json::from_str(text).expect("outErrors is a JSON document"))
        .unwrap_or(Value::Null)
}

/// Rule 1: the field's arguments are one value of the context read under either of its two
/// names.
///
/// The rows differ along which name the CONTEXT states the map under, which name the TEMPLATE
/// reads it with, and what the map holds — a scalar, a list walked by index, the length of
/// that list and a nested map — so the rule is asserted over the pairing rather than over one
/// spelling of it.
#[tokio::test]
async fn the_arguments_map_is_read_under_either_of_its_names() {
    let router = router();

    let holdings = r#"{"id":"7","tags":["a","b"],"nested":{"deep":true}}"#;
    let stated_long = format!("{{\"arguments\":{holdings}}}");
    let stated_short = format!("{{\"args\":{holdings}}}");
    let stated_both =
        format!("{{\"arguments\":{holdings},\"args\":{holdings},\"source\":null}}");

    // (what the member reads, what it must render to)
    let members: &[(&str, &str)] = &[
        ("id", "7"),
        ("tags[1]", "b"),
        ("tags.size()", "2"),
        ("nested.deep", "true"),
    ];

    for (stated, context) in [
        ("arguments", &stated_long),
        ("args", &stated_short),
        ("both names", &stated_both),
    ] {
        for read in ["arguments", "args"] {
            for (member, expected) in members {
                let case =
                    format!("the context states {stated} and the template reads {read}.{member}");
                let template = format!("$ctx.{read}.{member}");
                let answer = evaluate(&router, &template, context).await;
                assert_eq!(
                    (answer.status, result(&answer)),
                    (200, Some(*expected)),
                    "case {case:?}: expected 200 rendering {expected:?}, got {} {:?}",
                    answer.status,
                    answer.body
                );
            }
        }
    }

    // A `#set` through one name is read back through the other: one value, two names.
    let written = evaluate(
        &router,
        "#set($ctx.args.id = \"9\")$ctx.arguments.id",
        &stated_long,
    )
    .await;
    assert_eq!(
        (written.status, result(&written)),
        (200, Some("9")),
        "case a #set through the alias: expected the one value it wrote, got {} {:?}",
        written.status,
        written.body
    );

    // A variable of the template's own is NOT a context, so nothing of it is aliased.
    let local = evaluate(
        &router,
        "#set($own = {\"arguments\": {\"id\": \"7\"}})[$!{own.args.id}]",
        "{}",
    )
    .await;
    assert_eq!(
        (local.status, result(&local)),
        (200, Some("[]")),
        "case a local variable: expected no aliasing of its own members, got {} {:?}",
        local.status,
        local.body
    );
}

/// Rule 2: each `$util.dynamodb` helper renders the attribute value DynamoDB holds a value
/// as, and its `…Json` sibling the same representation serialized.
///
/// The catalog is the attribute types a DynamoDB attribute value is held under — `S`, `N`,
/// `B`, `BOOL`, `NULL`, `L`, `M`, `SS`, `NS`, `BS`, the set this crate's own marshalling
/// states (`crate::service::graphql_api::mapping_template::utilities::dynamodb`, which the
/// data-plane suite `graphql_api_dynamodb_marshalling` drives through a resolver) — and every
/// row is driven through a value of the CONTEXT, which is how a request template marshals a
/// field's own arguments.
#[tokio::test]
async fn the_dynamodb_helpers_marshal_a_value_of_the_context() {
    let router = router();

    let context = r#"{"arguments":{"n":3,"f":9.5,"s":"Widget","b":true,"z":null,
                      "l":["a","b"],"m":{"k":"v"},"ss":["a","b"],"ns":[1,2.5,3]}}"#;
    let cases: &[(&str, &str, &str)] = &[
        ("a number through the general converter", "toDynamoDB($ctx.args.n)", r#"{"N":3}"#),
        ("a fractional number", "toDynamoDB($ctx.args.f)", r#"{"N":9.5}"#),
        ("a string", "toDynamoDB($ctx.args.s)", r#"{"S":"Widget"}"#),
        ("a boolean", "toDynamoDB($ctx.args.b)", r#"{"BOOL":true}"#),
        ("a null", "toDynamoDB($ctx.args.z)", r#"{"NULL":null}"#),
        (
            "a list, each element marshalled",
            "toDynamoDB($ctx.args.l)",
            r#"{"L":[{"S":"a"},{"S":"b"}]}"#,
        ),
        (
            "a map, each member marshalled",
            "toDynamoDB($ctx.args.m)",
            r#"{"M":{"k":{"S":"v"}}}"#,
        ),
        ("a string set", "toStringSet($ctx.args.ss)", r#"{"SS":["a","b"]}"#),
        ("a number set", "toNumberSet($ctx.args.ns)", r#"{"NS":[1,2.5,3]}"#),
        ("a binary set", "toBinarySet($ctx.args.ss)", r#"{"BS":["a","b"]}"#),
        ("the typed string helper", "toString($ctx.args.s)", r#"{"S":"Widget"}"#),
        ("the typed number helper", "toNumber($ctx.args.n)", r#"{"N":3}"#),
        ("the typed binary helper", "toBinary($ctx.args.s)", r#"{"B":"Widget"}"#),
        ("the typed boolean helper", "toBoolean($ctx.args.b)", r#"{"BOOL":true}"#),
        ("the typed null helper", "toNull($ctx.args.z)", r#"{"NULL":null}"#),
        ("the list helper", "toList($ctx.args.l)", r#"{"L":[{"S":"a"},{"S":"b"}]}"#),
        ("the map helper", "toMap($ctx.args.m)", r#"{"M":{"k":{"S":"v"}}}"#),
        (
            "the map-values helper, which marshals the members and not the map",
            "toMapValues($ctx.args.m)",
            r#"{"k":{"S":"v"}}"#,
        ),
    ];

    for (case, call, expected) in cases {
        // The `…Json` sibling answers the SAME representation as text, which is what lets a
        // template inline it into the request object it is writing.
        for (shape, template, expected) in [
            ("as a value", format!("$util.dynamodb.{call}"), (*expected).to_string()),
            (
                "as its serialized sibling",
                format!("$util.dynamodb.{}", call.replacen('(', "Json(", 1)),
                (*expected).to_string(),
            ),
        ] {
            let answer = evaluate(&router, &template, context).await;
            assert_eq!(
                (answer.status, result(&answer)),
                (200, Some(expected.as_str())),
                "case {case:?} {shape}: expected 200 rendering {expected:?}, got {} {:?}",
                answer.status,
                answer.body
            );
        }
    }

    // A whole request object, which is what a DynamoDB resolver template writes: the
    // marshalled members are inlined into the text around them.
    let document = evaluate(
        &router,
        "{\"n\": $util.dynamodb.toDynamoDBJson($ctx.args.n), \
         \"ss\": $util.dynamodb.toStringSetJson($ctx.args.ss)}",
        context,
    )
    .await;
    assert_eq!(
        (document.status, result(&document)),
        (200, Some(r#"{"n": {"N":3}, "ss": {"SS":["a","b"]}}"#)),
        "case a request object built from the helpers: got {} {:?}",
        document.status,
        document.body
    );

    // A helper handed a value it cannot marshal is a template this reader cannot run, which is
    // reported through the output `error` member rather than refused.
    let refused = evaluate(&router, "$util.dynamodb.toStringSet($ctx.args.s)", context).await;
    assert_eq!(
        (
            refused.status,
            refused.body["error"]["message"].as_str(),
            result(&refused)
        ),
        (200, Some("This value is not a list: Widget"), None),
        "case a set helper over a string: expected the served diagnostic, got {} {:?}",
        refused.status,
        refused.body
    );
}

/// Rule 3: `#foreach` runs its body once per element and `#set` arithmetic accumulates across
/// the passes.
///
/// The rows differ along the SIZE of the collection the loop runs over (none, one, several),
/// which is what the rule generalises over: the projection has one entry per element and the
/// accumulators hold what the passes added up.
#[tokio::test]
async fn a_foreach_body_runs_once_per_element_and_set_arithmetic_accumulates() {
    let router = router();

    let template = "#set($skus = [])\
                    #set($total = 0)\
                    #foreach($line in $ctx.args.lines)\
                    #set($sku = \"$line.code\" + \"x\" + $line.qty)\
                    $util.qr($skus.add($sku))\
                    #set($total = $total + $line.qty * $line.price)\
                    #end\
                    #set($ctx.stash.lineCount = $skus.size())\
                    {\"skus\": $util.toJson($skus), \"total\": $total}";
    let cases: &[(&str, &str, &str, &str)] = &[
        (
            "no element at all",
            r#"{"arguments":{"lines":[]}}"#,
            r#"{"skus": [], "total": 0}"#,
            r#"{"lineCount":0}"#,
        ),
        (
            "one element",
            r#"{"arguments":{"lines":[{"code":"A1","qty":2,"price":3}]}}"#,
            r#"{"skus": ["A1x2"], "total": 6}"#,
            r#"{"lineCount":1}"#,
        ),
        (
            "two elements",
            r#"{"arguments":{"lines":[{"code":"A1","qty":2,"price":3},
                                      {"code":"C3","qty":5,"price":4}]}}"#,
            r#"{"skus": ["A1x2","C3x5"], "total": 26}"#,
            r#"{"lineCount":2}"#,
        ),
        (
            "three elements, the arguments stated under the short name",
            r#"{"args":{"lines":[{"code":"A1","qty":1,"price":1},
                                 {"code":"B2","qty":2,"price":2},
                                 {"code":"C3","qty":3,"price":3}]}}"#,
            r#"{"skus": ["A1x1","B2x2","C3x3"], "total": 14}"#,
            r#"{"lineCount":3}"#,
        ),
    ];

    for (case, context, expected, stash) in cases {
        let answer = evaluate(&router, template, context).await;
        assert_eq!(
            (answer.status, result(&answer)),
            (200, Some(*expected)),
            "case {case:?}: expected 200 rendering {expected:?}, got {} {:?}",
            answer.status,
            answer.body
        );
        assert_eq!(
            answer.body["stash"].as_str(),
            Some(*stash),
            "case {case:?}: expected the stash the template wrote, got {:?}",
            answer.body
        );
    }

    // The loop's own position variables are read by the body, so a separator between entries
    // is written for every element but the last.
    let joined = evaluate(
        &router,
        "#foreach($n in $ctx.args.items)$n#if($foreach.hasNext)-#end#end",
        r#"{"arguments":{"items":[1,2,3]}}"#,
    )
    .await;
    assert_eq!(
        (joined.status, result(&joined)),
        (200, Some("1-2-3")),
        "case a separator written per pass: got {} {:?}",
        joined.status,
        joined.body
    );

    // Each logical and relational operator of the language is written either way — as the
    // symbol or as the word — and one spelling decides what the other does. The rows are the
    // pairs the grammar carries: `&&`/`and`, `||`/`or`, `!`/`not`, `==`/`eq`, `!=`/`ne`,
    // `<`/`lt`, `<=`/`le`, `>`/`gt`, `>=`/`ge`.
    let spellings: &[(&str, &str, &str)] = &[
        ("and", "$ctx.args.n > 1 && $ctx.args.n < 5", "$ctx.args.n gt 1 and $ctx.args.n lt 5"),
        ("or", "$ctx.args.n == 9 || $ctx.args.n == 3", "$ctx.args.n eq 9 or $ctx.args.n eq 3"),
        ("not", "!($ctx.args.n == 9)", "not ($ctx.args.n eq 9)"),
        ("equal", "$ctx.args.n == 3", "$ctx.args.n eq 3"),
        ("not equal", "$ctx.args.n != 9", "$ctx.args.n ne 9"),
        ("less", "$ctx.args.n < 4", "$ctx.args.n lt 4"),
        ("less or equal", "$ctx.args.n <= 3", "$ctx.args.n le 3"),
        ("greater", "$ctx.args.n > 2", "$ctx.args.n gt 2"),
        ("greater or equal", "$ctx.args.n >= 3", "$ctx.args.n ge 3"),
    ];
    for (case, symbols, words) in spellings {
        for (spelling, condition) in [("as symbols", symbols), ("as words", words)] {
            let template = format!("#if({condition})yes#{{else}}no#end");
            let answer = evaluate(&router, &template, r#"{"arguments":{"n":3}}"#).await;
            assert_eq!(
                (answer.status, result(&answer)),
                (200, Some("yes")),
                "case {case:?} {spelling}: expected the branch it decides, got {} {:?}",
                answer.status,
                answer.body
            );
        }
    }

    // A word that merely STARTS like an operator is part of the name it belongs to, not an
    // operator: the reference is walked as written.
    let named = evaluate(
        &router,
        "$ctx.args.length $ctx.args.notes $ctx.args.organisation",
        r#"{"arguments":{"length":2,"notes":"kept","organisation":"here"}}"#,
    )
    .await;
    assert_eq!(
        (named.status, result(&named)),
        (200, Some("2 kept here")),
        "case names that start like operators: got {} {:?}",
        named.status,
        named.body
    );
}

/// Rule 4: `#return` ends the evaluation where it stands.
///
/// The rows differ along what the directive names (a map, a scalar, a value built by the
/// template, nothing at all), where it sits (at the top level, inside a branch, inside a
/// loop) and whether it is reached at all.
#[tokio::test]
async fn an_early_return_ends_the_evaluation_where_it_stands() {
    let router = router();

    let cached = r#"{"arguments":{"cached":true}}"#;
    let uncached = r#"{"arguments":{"cached":false}}"#;
    let cases: &[(&str, &str, &str, &str)] = &[
        (
            "a map the template built, returned from inside a branch",
            "#if($ctx.args.cached)\
             #set($out = {})\
             #set($out.early = 5)\
             #set($out.from = \"cache\")\
             #return($out)\
             #end\
             {\"late\": true}",
            cached,
            r#"{"early":5,"from":"cache"}"#,
        ),
        (
            "that same template on the branch it does not take",
            "#if($ctx.args.cached)\
             #set($out = {})\
             #set($out.early = 5)\
             #set($out.from = \"cache\")\
             #return($out)\
             #end\
             {\"late\": true}",
            uncached,
            "{\"late\": true}",
        ),
        (
            "a scalar",
            "#return(42)after",
            "{}",
            "42",
        ),
        (
            "a value read out of the context",
            "#return($ctx.args.cached)after",
            cached,
            "true",
        ),
        (
            "a return from inside a loop, which ends the whole evaluation",
            "#foreach($n in $ctx.args.items)#if($n == 2)#return($n)#end#end tail",
            r#"{"arguments":{"items":[1,2,3]}}"#,
            "2",
        ),
        (
            "a return naming no value, which ends it with what was rendered",
            "kept#return
dropped",
            "{}",
            "kept",
        ),
    ];

    for (case, template, context, expected) in cases {
        let answer = evaluate(&router, template, context).await;
        assert_eq!(
            (answer.status, result(&answer)),
            (200, Some(*expected)),
            "case {case:?}: expected 200 rendering {expected:?}, got {} {:?}",
            answer.status,
            answer.body
        );
    }
}

/// Rule 5: a mock `context` whose properties are not of the kind a resolver context gives
/// them is a served answer naming each of them, and nothing is evaluated over it.
///
/// The rows span the properties of that shape — the arguments map under both of its names, the
/// parent's value, the caller's identity, the request's own fields, where the field sits, the
/// stash and the previous step's result, the set the data plane builds a resolver step's `ctx`
/// from (`crate::service::graphql_api::execution`'s `step_context`) — and the kinds that are
/// not a map (a string, a number, a boolean, a list), plus the properties a resolver context
/// gives no kind, which are not reported. The issue's own case cited one of these properties;
/// every other row is new.
#[tokio::test]
async fn a_mock_context_that_is_not_shaped_like_a_resolver_context_is_reported() {
    let router = router();

    // The one property this template reads, so a served evaluation is visible as a rendering.
    let template = "{\"key\": $util.dynamodb.toDynamoDBJson($ctx.args.key)}";
    let properties = [
        "arguments", "args", "source", "identity", "request", "info", "stash", "prev",
    ];
    let kinds = [
        ("a string", "\"text\""),
        ("a number", "12"),
        ("a boolean", "true"),
        ("a list", "[1]"),
    ];

    for property in properties {
        for (kind, written) in kinds {
            let case = format!("{property} carrying {kind}");
            let context = format!("{{\"{property}\":{written}}}");
            let answer = evaluate(&router, template, &context).await;
            let expected = format!(
                "The following properties in your mock context object failed validation: \
                 ctx.{property}."
            );
            assert_eq!(
                (answer.status, answer.body["error"]["message"].as_str()),
                (200, Some(expected.as_str())),
                "case {case:?}: expected the served validation answer, got {} {:?}",
                answer.status,
                answer.body
            );
            for absent in ["evaluationResult", "logs", "stash", "outErrors"] {
                assert!(
                    answer.body[absent].is_null(),
                    "case {case:?}: nothing was evaluated, so {absent} is not reported, got {:?}",
                    answer.body
                );
            }
        }
    }

    // Every offending property is named, in the order the shape states them.
    let several = evaluate(
        &router,
        template,
        r#"{"identity":1,"arguments":"text","source":[],"result":5}"#,
    )
    .await;
    assert_eq!(
        (several.status, several.body["error"]["message"].as_str()),
        (
            200,
            Some(
                "The following properties in your mock context object failed validation: \
                 ctx.arguments, ctx.source, ctx.identity."
            )
        ),
        "case several offending properties: got {} {:?}",
        several.status,
        several.body
    );

    // A context whose properties ARE of the kind a resolver context gives them is evaluated,
    // whatever else it carries: `result` is whatever the data source produced, a null property
    // states nothing about itself, and a property the shape does not name is not checked.
    let served: &[(&str, &str)] = &[
        ("the empty map", "{}"),
        ("every property a map", r#"{"arguments":{},"source":{},"stash":{},"prev":{}}"#),
        ("null properties", r#"{"source":null,"prev":null,"identity":null}"#),
        ("a result that is a list", r#"{"result":[1,2],"arguments":{}}"#),
        ("a result that is a scalar", r#"{"result":"text","arguments":{}}"#),
        ("a property of no resolver context", r#"{"whatever":"text","arguments":{}}"#),
    ];
    for (case, context) in served {
        let answer = evaluate(&router, template, context).await;
        assert_eq!(
            (answer.status, result(&answer)),
            (200, Some(r#"{"key": {"NULL":null}}"#)),
            "case {case:?}: expected the template rendered over it, got {} {:?}",
            answer.status,
            answer.body
        );
        assert!(
            answer.body["error"].is_null(),
            "case {case:?}: a context of the right shape failed nothing, got {:?}",
            answer.body
        );
    }

    // BOTH halves of the sandbox take the same `context` member of the same resolver context,
    // so one check covers both: the APPSYNC_JS evaluation beside this one reports the same
    // properties the same way, and evaluates nothing over the mock either.
    let in_code = call(
        &router,
        "POST",
        "/v1/dataplane-evaluatecode",
        Some(json!({
            "runtime": {"name": "APPSYNC_JS", "runtimeVersion": "1.0.0"},
            "code": "export function request(ctx) { return {} }\n\
                     export function response(ctx) { return ctx.result }\n",
            "context": r#"{"arguments":"text"}"#,
            "function": "request",
        })),
    )
    .await;
    assert_eq!(
        (in_code.status, in_code.body["error"]["message"].as_str()),
        (
            200,
            Some(
                "The following properties in your mock context object failed validation: \
                 ctx.arguments."
            )
        ),
        "case the same mock context through the APPSYNC_JS half: got {} {:?}",
        in_code.status,
        in_code.body
    );
    assert!(
        in_code.body["evaluationResult"].is_null(),
        "case the same mock context through the APPSYNC_JS half: nothing was evaluated over \
         it, got {:?}",
        in_code.body
    );
}

/// Rule 6: the envelope of a run that happened carries the stash and the appended errors, and
/// a log list only where the run reached the end of its template.
///
/// The rows are the ways one run can END — through to the end, cut short by an early
/// `#return`, cut short by a raise — each paired with the members the response must carry.
#[tokio::test]
async fn the_envelope_reports_what_the_run_left_and_logs_only_a_run_that_ran_through() {
    let router = router();

    // A run that reached the END of its template: its result, the lines it logged, and the
    // stash and appended errors it left.
    let through = evaluate(
        &router,
        "$util.log.info(\"reading {} of {}\", $ctx.args.id, $ctx.args.total)\
         $util.log.error(\"flagged\")\
         $util.appendError(\"partial failure\", \"PartialError\", {\"id\": \"x1\"})\
         #set($ctx.stash.seen = true)\
         rendered",
        r#"{"arguments":{"id":"7","total":2}}"#,
    )
    .await;
    assert_eq!(
        (through.status, result(&through)),
        (200, Some("rendered")),
        "case a run that ran through: got {} {:?}",
        through.status,
        through.body
    );
    assert_eq!(
        through.body["logs"],
        json!(["INFO - reading 7 of 2", "ERROR - flagged"]),
        "case a run that ran through: expected the lines it logged, got {:?}",
        through.body
    );
    assert_eq!(
        (
            through.body["stash"].as_str(),
            out_errors(&through)
        ),
        (
            Some(r#"{"seen":true}"#),
            json!([{
                "message": "partial failure",
                "type": "PartialError",
                "data": {"id": "x1"},
                "info": null,
            }])
        ),
        "case a run that ran through: expected what it left behind, got {:?}",
        through.body
    );

    // A run an early `#return` CUT SHORT: the result it returned, the stash and the appended
    // errors it left, and no log list at all.
    let returned = evaluate(
        &router,
        "$util.log.info(\"reading\")#set($ctx.stash.seen = true)#return(\"early\")late",
        "{}",
    )
    .await;
    assert_eq!(
        (
            returned.status,
            result(&returned),
            returned.body["stash"].as_str(),
            returned.body["outErrors"].as_str(),
        ),
        (200, Some("early"), Some(r#"{"seen":true}"#), Some("[]")),
        "case a run an early return cut short: got {} {:?}",
        returned.status,
        returned.body
    );
    assert!(
        returned.body["logs"].is_null(),
        "case a run an early return cut short: it reports no log list, got {:?}",
        returned.body
    );

    // A run a RAISE cut short: the error it raised, the stash and the appended errors it left
    // by then, no rendered result and no log list.
    let raised = evaluate(
        &router,
        "$util.log.info(\"reading\")\
         $util.appendError(\"partial failure\", \"PartialError\", {\"id\": \"x1\"})\
         #if($ctx.args.qty <= 0)$util.error(\"qty must be positive\")#end unreached",
        r#"{"arguments":{"qty":-1}}"#,
    )
    .await;
    assert_eq!(
        (
            raised.status,
            raised.body["error"]["message"].as_str(),
            result(&raised),
            raised.body["stash"].as_str(),
        ),
        (200, Some("qty must be positive"), None, Some("{}")),
        "case a run a raise cut short: got {} {:?}",
        raised.status,
        raised.body
    );
    assert_eq!(
        out_errors(&raised),
        json!([{
            "message": "partial failure",
            "type": "PartialError",
            "data": {"id": "x1"},
            "info": null,
        }]),
        "case a run a raise cut short: expected the errors it had appended, got {:?}",
        raised.body
    );
    assert!(
        raised.body["logs"].is_null(),
        "case a run a raise cut short: it reports no log list, got {:?}",
        raised.body
    );

    // The same rule for the refusal `$util.unauthorized` states, which carries the word on its
    // own in a sandbox that is resolving no field.
    let refused = evaluate(&router, "$util.unauthorized()", "{}").await;
    assert_eq!(
        (
            refused.status,
            refused.body["error"]["message"].as_str(),
            refused.body["stash"].as_str(),
            refused.body["outErrors"].as_str(),
        ),
        (200, Some("Unauthorized"), Some("{}"), Some("[]")),
        "case the refusal $util.unauthorized states: got {} {:?}",
        refused.status,
        refused.body
    );
}

/// Rule 6, the other half: the run HAS a stash whether the mock context states one or not, so
/// every way a template reaches into it reaches the one the response reports.
///
/// The rows differ along HOW the template writes the stash — a `#set` into a member of it, the
/// `put` of the map it is, the whole map replaced — and along whether the call stated a stash
/// of its own.
#[tokio::test]
async fn the_run_has_a_stash_whether_the_call_stated_one_or_not() {
    let router = router();

    let cases: &[(&str, &str, &str, &str)] = &[
        (
            "a #set into a member of a stash the call did not state",
            "#set($ctx.stash.lineCount = 2)rendered",
            "{}",
            r#"{"lineCount":2}"#,
        ),
        (
            "the put of a stash the call did not state",
            "$util.qr($ctx.stash.put(\"lineCount\", 2))rendered",
            "{}",
            r#"{"lineCount":2}"#,
        ),
        (
            "the put of a stash the call did state",
            "$util.qr($ctx.stash.put(\"lineCount\", 2))rendered",
            r#"{"stash":{"from":"request"}}"#,
            r#"{"from":"request","lineCount":2}"#,
        ),
        (
            "the whole stash replaced",
            "#set($ctx.stash = {\"lineCount\": 2})rendered",
            "{}",
            r#"{"lineCount":2}"#,
        ),
        (
            "a stash the template only reads",
            "$ctx.stash.isEmpty()",
            "{}",
            "{}",
        ),
        (
            "a stash the call stated and the template left alone",
            "rendered",
            r#"{"stash":{"from":"request"}}"#,
            r#"{"from":"request"}"#,
        ),
    ];

    for (case, template, context, stash) in cases {
        let answer = evaluate(&router, template, context).await;
        assert_eq!(
            (answer.status, answer.body["stash"].as_str()),
            (200, Some(*stash)),
            "case {case:?}: expected the stash the run left, got {} {:?}",
            answer.status,
            answer.body
        );
    }
}
