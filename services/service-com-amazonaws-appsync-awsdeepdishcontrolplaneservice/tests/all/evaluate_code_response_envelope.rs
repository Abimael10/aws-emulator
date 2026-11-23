// SPDX-License-Identifier: CC0-1.0

//! What an `EvaluateCode` answer carries BESIDE the evaluated function's own result: the log
//! of the run, the stash and the appended errors it left, and the diagnostics of code that
//! never ran.
//!
//! The rules asserted here, each one level above the single request that exposed it:
//!
//! 1. the ENVELOPE of a run that RETURNED: `stash` is the object the run left, as a JSON
//!    object string ("{}" for a run that wrote no key), `outErrors` the errors it appended, as
//!    a JSON array string ("[]" for a run that appended none) — both always reported, because
//!    a run has both whatever it did with them — and `logs` the lines it wrote, the empty list
//!    for a run that wrote none;
//! 2. a run that ended by RAISING — the error the code worded, the `runtime.earlyReturn` that
//!    ends a step — is reported with what it ended on and with the stash and out-errors it
//!    left, and with NO log list: the lines a response reports are the lines of a run that
//!    returned;
//! 3. code that never ran — it does not compile, or it breaks a rule of the APPSYNC_JS rule
//!    set — is SERVED (HTTP 200) with no result and no envelope at all, and its `error` carries
//!    one `CodeError` per diagnostic plus a `message` stating every diagnostic in turn: the
//!    diagnostic as the compiler or the rule worded it, the class it falls under, and the first
//!    and the last position of the stretch of code it is about;
//! 4. one entry of `outErrors` names what the `util.appendError` call carried: its message, the
//!    type it classified the error under, the data it carried and the information beside it,
//!    each `null` where the call named none.
//!
//! Every assertion goes through the router the production emulator mounts, so these exercise
//! routing, restJson1 (de)serialization, the handler, the code reader and the embedded engine
//! together.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The router, the request helper and the module completion are the crate's shared test
// harness, reused rather than copied.
use super::error_contract::{call, router, Answer};
use super::evaluate_code::{mapping, position_of};

/// The operation's own path: `POST /v1/dataplane-evaluatecode`
/// (`smithy_query(query="operation", operation="EvaluateCode")`).
const PATH: &str = "/v1/dataplane-evaluatecode";

/// The only runtime AppSync supports, as `AppSyncRuntime` requires both its members.
const RUNTIME: (&str, &str) = ("APPSYNC_JS", "1.0.0");

/// Evaluate `code` over `context`, the module completed the way a mapping is defined.
async fn evaluate(router: &MultiServiceRouter, code: &str, context: &str) -> Answer {
    evaluate_module(router, &mapping(code), context).await
}

/// Evaluate `code` VERBATIM, whatever it exports: the call the diagnostics rows are made
/// through, because the positions they state are positions in the code as submitted.
async fn evaluate_module(router: &MultiServiceRouter, code: &str, context: &str) -> Answer {
    let body = json!({
        "runtime": {"name": RUNTIME.0, "runtimeVersion": RUNTIME.1},
        "code": code,
        "context": context,
        "function": "request",
    });
    call(router, "POST", PATH, Some(body)).await
}

/// The `logs` an answer carries, or `None` for an answer carrying no list at all.
fn logs(answer: &Answer) -> Option<Vec<String>> {
    let lines = answer.body.get("logs")?.as_array()?;
    Some(
        lines
            .iter()
            .map(|line| line.as_str().unwrap_or_default().to_string())
            .collect(),
    )
}

/// One row of the envelope table: what the code does, and what the answer carries for it.
struct Envelope {
    case: &'static str,
    code: &'static str,
    /// The `evaluationResult`, or `None` for an answer that carries none.
    result: Option<&'static str>,
    /// The `error.message`, or `None` for an answer that carries no error.
    error: Option<&'static str>,
    /// Whether the answer carries a log list, which a run that RETURNED has and a run that
    /// ended by raising has not.
    logs: bool,
    /// The `stash` and the `outErrors` the run left.
    stash: &'static str,
    out_errors: &'static str,
}

/// Rules 1, 2 and 4: the envelope of a run, over the ways a run can end.
#[tokio::test]
async fn the_envelope_is_what_the_run_left_behind() {
    let router = router();

    // The rows span what the rule generalises over: how the run ended (returned, ended its
    // step early, refused through the util module, raised by itself) and what it wrote to each
    // of the three members while it ran.
    let rows: &[Envelope] = &[
        Envelope {
            case: "a run that returned and wrote nothing",
            code: "export function request(ctx) { return {ok: true} }",
            result: Some(r#"{"ok":true}"#),
            error: None,
            logs: true,
            stash: "{}",
            out_errors: "[]",
        },
        Envelope {
            case: "a run that wrote a stash key",
            code: "export function request(ctx) { ctx.stash.seen = 3; return {ok: true} }",
            result: Some(r#"{"ok":true}"#),
            error: None,
            logs: true,
            stash: r#"{"seen":3}"#,
            out_errors: "[]",
        },
        Envelope {
            case: "a run that appended an error and went on",
            code: "import { util } from '@aws-appsync/utils';\n\
                   export function request(ctx) { util.appendError('noted', 'Noted', \
                   {q: 'zzz'}, {hint: 'widen'}); return {count: 0} }\n",
            result: Some(r#"{"count":0}"#),
            error: None,
            logs: true,
            stash: "{}",
            out_errors: r#"[{"message":"noted","type":"Noted","data":{"q":"zzz"},"info":{"hint":"widen"}}]"#,
        },
        Envelope {
            case: "a run that appended an error naming nothing else",
            code: "import { util } from '@aws-appsync/utils';\n\
                   export function request(ctx) { util.appendError('bare'); return {} }\n",
            result: Some("{}"),
            error: None,
            logs: true,
            stash: "{}",
            out_errors: r#"[{"message":"bare","type":null,"data":null,"info":null}]"#,
        },
        Envelope {
            case: "a run that ended its step through runtime.earlyReturn",
            code: "import { runtime } from '@aws-appsync/utils';\n\
                   export function request(ctx) { runtime.earlyReturn({hit: true}); \
                   return {hit: false} }\n",
            result: Some(r#"{"hit":true}"#),
            error: None,
            logs: false,
            stash: "{}",
            out_errors: "[]",
        },
        Envelope {
            case: "a run that ended its step after writing to the stash",
            code: "import { runtime } from '@aws-appsync/utils';\n\
                   export function request(ctx) { ctx.stash.tried = true; \
                   runtime.earlyReturn({done: true}) }\n",
            result: Some(r#"{"done":true}"#),
            error: None,
            logs: false,
            stash: r#"{"tried":true}"#,
            out_errors: "[]",
        },
        Envelope {
            case: "a run the code refused with util.error",
            code: "import { util } from '@aws-appsync/utils';\n\
                   export function request(ctx) { util.error('id is required', 'Validation') }\n",
            result: None,
            error: Some("id is required"),
            logs: false,
            stash: "{}",
            out_errors: "[]",
        },
        Envelope {
            case: "a run that refused after appending an error",
            code: "import { util } from '@aws-appsync/utils';\n\
                   export function request(ctx) { util.appendError('first'); \
                   util.error('no') }\n",
            result: None,
            error: Some("no"),
            logs: false,
            stash: "{}",
            out_errors: r#"[{"message":"first","type":null,"data":null,"info":null}]"#,
        },
        Envelope {
            case: "a run that raised an error of its own",
            code: "export function request(ctx) { throw new Error('boom') }",
            result: None,
            error: Some("boom"),
            logs: false,
            stash: "{}",
            out_errors: "[]",
        },
    ];

    for row in rows {
        let case = row.case;
        let answer = evaluate(&router, row.code, "{}").await;
        assert_eq!(
            answer.status, 200,
            "case {case:?}: expected the call itself to be served, got {answer:?}"
        );
        assert_eq!(
            answer.body["evaluationResult"].as_str(),
            row.result,
            "case {case:?}: expected the run's own result, got {answer:?}"
        );
        assert_eq!(
            answer.body["error"]["message"].as_str(),
            row.error,
            "case {case:?}: expected the error the run ended on, got {answer:?}"
        );
        assert_eq!(
            logs(&answer),
            row.logs.then(Vec::new),
            "case {case:?}: expected the log of the run, got {answer:?}"
        );
        assert_eq!(
            (
                answer.body["stash"].as_str(),
                answer.body["outErrors"].as_str()
            ),
            (Some(row.stash), Some(row.out_errors)),
            "case {case:?}: expected the stash and the appended errors the run left, \
             got {answer:?}"
        );
    }
}

/// Rule 1, on the log lines themselves: each one names the level, the position of the call
/// that wrote it and what that call was handed, as the JSON of what it is.
#[tokio::test]
async fn a_log_line_names_the_call_that_wrote_it() {
    let router = router();

    // Several calls, each at a different position and with a different set of arguments: one
    // in the module's own body, two on one line, one inside a callback, and both loggers.
    let code = "import { util } from '@aws-appsync/utils';\n\
                console.log('loaded');\n\
                export function request(ctx) {\n\
                \u{20}\u{20}console.log('kept', 2, 'of', 3);\n\
                \u{20}\u{20}console.error({total: 26});\n\
                \u{20}\u{20}util.log.info('deep'); util.log.error('beside it');\n\
                \u{20}\u{20}[1].forEach((n) => { console.log('each', n) });\n\
                \u{20}\u{20}return {ok: true};\n\
                }\n";
    let answer = evaluate(&router, code, "{}").await;
    assert_eq!(
        answer.status, 200,
        "case the logging handler: expected status 200, got {answer:?}"
    );

    let module = mapping(code);
    let expected: Vec<String> = [
        ("INFO", "console.log('loaded')", r#""loaded""#),
        ("INFO", "console.log('kept'", r#""kept" 2 "of" 3"#),
        ("ERROR", "console.error({total: 26})", r#"{"total":26}"#),
        ("INFO", "util.log.info('deep')", r#""deep""#),
        ("ERROR", "util.log.error('beside it')", r#""beside it""#),
        ("INFO", "console.log('each', n)", r#""each" 1"#),
    ]
    .iter()
    .map(|(level, call, rendered)| {
        let (line, column) = position_of(&module, call);
        format!("{level} - code.js:{line}:{column}: {rendered}")
    })
    .collect();
    assert_eq!(
        logs(&answer),
        Some(expected),
        "case the logging handler: expected one line per call, each naming where the call \
         sits, got {answer:?}"
    );
}

/// One diagnostic an answer states: the whole user-presentable error, and the line, the
/// column and the length its location carries.
type Stated<'a> = (&'a str, i64, i64, i64);

/// Rule 3: code the COMPILER could not read is served as its own diagnostics, each stated the
/// way the compiler a definition is compiled with states one.
#[tokio::test]
async fn code_that_does_not_compile_is_served_as_the_compiler_states_it() {
    let router = router();

    // (case, the code, every diagnostic the compiler states for it, in order: the whole
    // diagnostic, its line, its column and its length)
    let rows: &[(&str, &str, &[Stated<'_>])] = &[
        (
            "a function body that never closes",
            "export function request(ctx) {\n  return {};\n",
            &[("code.js(3,1): error TS1005: '}' expected.\n", 3, 1, 0)],
        ),
        (
            "an unterminated string literal, which the compiler states twice",
            "export function request(ctx) { return { payload: 'unterminated }; }",
            &[
                (
                    "code.js(1,50): error TS1002: Unterminated string literal.\n",
                    1,
                    50,
                    18,
                ),
                ("code.js(1,68): error TS1005: ',' expected.\n", 1, 68, 0),
            ],
        ),
        (
            "a parenthesis that never closes",
            "export function request(ctx) {\n  return ((((\n",
            &[("code.js(3,1): error TS1109: Expression expected.\n", 3, 1, 0)],
        ),
    ];

    for (case, code, stated) in rows {
        let answer = evaluate_module(&router, code, "{}").await;
        served_as_diagnostics(&answer, case, "PARSE_ERROR", stated);
    }
}

/// Rule 3: code that compiles but breaks a rule of the APPSYNC_JS rule set is served as that
/// rule's diagnostic, over the position AND the length of the construct the rule is about.
#[tokio::test]
async fn code_that_breaks_a_rule_is_served_as_that_rule_states_it() {
    let router = router();

    // The rows span the rule set's own kinds: a loop statement, a statement that catches what
    // a run raises (so code that could swallow the sandbox's own stop never runs), a class,
    // and an operator that both reads and writes. `crate::appsync_js_code_rules` walks the
    // whole set; these rows are about how one broken rule reaches this operation's answer. The
    // construct each row names is the stretch of its own code the diagnostic is about.
    let rows: &[(&str, &str, &str, &str, &str)] = &[
        (
            "a for statement",
            "export function request(ctx) {\n  for (let i = 0; i < 3; i = i + 1) {}\n  \
             return {};\n}\n",
            "for (let i = 0; i < 3; i = i + 1) {}",
            "@aws-appsync/no-for",
            "For statements are not supported.",
        ),
        (
            "a while statement",
            "export function request(ctx) {\n  while (ctx.args.go) {}\n  return {};\n}\n",
            "while (ctx.args.go) {}",
            "@aws-appsync/no-while",
            "While statements are not supported.",
        ),
        (
            "a try statement",
            "export function request(ctx) {\n  try { return {}; } catch (e) { return {}; }\n}\n",
            "try { return {}; } catch (e) { return {}; }",
            "@aws-appsync/no-try",
            "Try statements are not supported.",
        ),
        (
            "a class declaration",
            "class Key { }\nexport function request(ctx) { return {}; }\n",
            "class Key { }",
            "@aws-appsync/no-classes",
            "Classes are not supported.",
        ),
        (
            "an increment operator",
            "export function request(ctx) {\n  let n = 0;\n  n++;\n  return {n};\n}\n",
            "n++",
            "@aws-appsync/no-disallowed-unary-operators",
            "The operator ++ is not supported",
        ),
        (
            "a decrement operator",
            "export function request(ctx) {\n  let n = 1;\n  n--;\n  return {n};\n}\n",
            "n--",
            "@aws-appsync/no-disallowed-unary-operators",
            "The operator -- is not supported",
        ),
    ];

    for (case, code, construct, rule, sentence) in rows {
        let answer = evaluate_module(&router, code, "{}").await;
        // The diagnostic's position and length are the construct's own, read out of the
        // submitted code rather than written out here.
        let (line, column) = position_of(code, construct);
        let (line, column) = (line as i64, column as i64);
        let span = construct.encode_utf16().count() as i64;
        let value = format!("code.js({line},{column}): error {rule}: {sentence}");
        served_as_diagnostics(
            &answer,
            case,
            "LINT_ERROR",
            &[(value.as_str(), line, column, span)],
        );
    }
}

/// Hold `answer` to rule 3: HTTP 200, no member of a run at all, one `CodeError` per
/// diagnostic, and a `message` stating each one with its class and the stretch of code it is
/// about — its first position and the position just past its last character.
fn served_as_diagnostics(answer: &Answer, case: &str, error_type: &str, stated: &[Stated<'_>]) {
    assert_eq!(
        answer.status, 200,
        "case {case:?}: expected the call itself to be served, got {answer:?}"
    );
    for member in ["evaluationResult", "logs", "stash", "outErrors"] {
        assert!(
            answer.body.get(member).is_none(),
            "case {case:?}: expected no {member} for code that never ran, got {answer:?}"
        );
    }
    let errors: Vec<Value> = stated
        .iter()
        .map(|(value, line, column, span)| {
            json!({
                "errorType": error_type,
                "value": value,
                "location": {"line": line, "column": column, "span": span},
            })
        })
        .collect();
    assert_eq!(
        answer.body["error"]["codeErrors"],
        Value::Array(errors),
        "case {case:?}: expected one code error per diagnostic, got {answer:?}"
    );
    let message = stated
        .iter()
        .map(|(value, line, column, span)| {
            format!(
                "Error: {value} [code: {error_type}] [start: line: {line}, col: {column}] \
                 [end: line: {line}, col: {}]",
                column + span
            )
        })
        .collect::<Vec<String>>()
        .join(" ");
    assert_eq!(
        answer.body["error"]["message"].as_str(),
        Some(message.as_str()),
        "case {case:?}: expected the message to state every diagnostic and its stretch, \
         got {answer:?}"
    );
}

/// Rule 3, over code breaking SEVERAL rules at once: one diagnostic per broken rule, in the
/// order the constructs sit in the code, and one message stating them all in that order.
#[tokio::test]
async fn code_breaking_several_rules_is_served_one_diagnostic_for_each() {
    let router = router();

    let answer = evaluate_module(
        &router,
        "export function request(ctx) {\n  for (let i = 0; i < 3; i++) {}\n  return {};\n}\n",
        "{}",
    )
    .await;
    assert_eq!(
        answer.status, 200,
        "case a for loop using ++: expected the call itself to be served, got {answer:?}"
    );
    assert_eq!(
        answer.body["error"]["codeErrors"],
        json!([
            {
                "errorType": "LINT_ERROR",
                "value": "code.js(2,3): error @aws-appsync/no-for: For statements are not supported.",
                "location": {"line": 2, "column": 3, "span": 30},
            },
            {
                "errorType": "LINT_ERROR",
                "value": "code.js(2,26): error @aws-appsync/no-disallowed-unary-operators: The operator ++ is not supported",
                "location": {"line": 2, "column": 26, "span": 3},
            },
        ]),
        "case a for loop using ++: expected one diagnostic per broken rule, got {answer:?}"
    );
    assert_eq!(
        answer.body["error"]["message"].as_str(),
        Some(
            "Error: code.js(2,3): error @aws-appsync/no-for: For statements are not supported. \
             [code: LINT_ERROR] [start: line: 2, col: 3] [end: line: 2, col: 33] \
             Error: code.js(2,26): error @aws-appsync/no-disallowed-unary-operators: The \
             operator ++ is not supported [code: LINT_ERROR] [start: line: 2, col: 26] \
             [end: line: 2, col: 29]"
        ),
        "case a for loop using ++: expected both diagnostics stated in order, got {answer:?}"
    );
}

/// Rule 3, the other way round: code the compiler reads to the end and that breaks no rule is
/// EVALUATED rather than answered with diagnostics, whatever it is written like.
#[tokio::test]
async fn code_the_reader_admits_is_evaluated() {
    let router = router();

    // The rows are the forms a real APPSYNC_JS handler takes: the helpers imported, a list
    // walked with `map`/`filter`/`reduce` (the rule set forbids the loop statements, not these
    // calls), a template literal and non-ASCII text.
    let rows: &[(&str, &str, &str)] = &[
        (
            "the smallest handler",
            "export function request(ctx) { return {} }",
            "{}",
        ),
        (
            "a handler importing the helpers",
            "import { util } from '@aws-appsync/utils';\n\
             export function request(ctx) { return {minted: util.autoId().length} }\n",
            r#"{"minted":36}"#,
        ),
        (
            "a list walked with map, filter and reduce",
            "export function request(ctx) { const kept = [1, 2, 3].filter((n) => n > 1)\
             .map((n) => n * 2); return {sum: kept.reduce((a, b) => a + b, 0)} }",
            r#"{"sum":10}"#,
        ),
        (
            "a template literal over non-ASCII text",
            "export function request(ctx) { const who = 'wörld'; return {hi: `héllo ${who}`} }",
            r#"{"hi":"héllo wörld"}"#,
        ),
    ];

    for (case, code, expected) in rows {
        let answer = evaluate(&router, code, "{}").await;
        assert!(
            answer.body.get("error").is_none(),
            "case {case:?}: expected the code to be evaluated, got {answer:?}"
        );
        let result: Value = serde_json::from_str(
            answer.body["evaluationResult"]
                .as_str()
                .unwrap_or_else(|| panic!("case {case:?}: expected a result, got {answer:?}")),
        )
        .expect("the evaluation result is a JSON document");
        let expected: Value =
            serde_json::from_str(expected).expect("the expected result is a JSON document");
        assert_eq!(
            result, expected,
            "case {case:?}: expected the run's own answer, got {answer:?}"
        );
    }
}
