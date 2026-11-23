// SPDX-License-Identifier: CC0-1.0

//! The APPSYNC_JS rule set a written definition's `code` is checked against, end to end over
//! HTTP: what a call that would store code breaking a rule answers, and what the API holds
//! afterwards.
//!
//! The APPSYNC_JS runtime runs a restricted JavaScript, so a definition is read twice as it
//! is written — compiled, then checked against the runtime's own rule set — and a definition
//! that breaks either is refused rather than stored. The rules asserted here are stated over
//! CLASSES of input rather than over the one request that exposed the defect:
//!
//! 1. code that compiles but uses a construct the rule set forbids is refused: 400
//!    `BadRequestException` with the `CODE_ERROR` reason, AppSync's fixed code-errors
//!    sentence and one `detail.codeErrors` entry per broken rule, each with the `LINT_ERROR`
//!    `errorType`, the diagnostic `code.js(<line>,<column>): error @aws-appsync/<rule>:
//!    <sentence>` as its `value` and a `location` carrying the line, the column and the
//!    LENGTH of the construct the rule is about. Asserted over a table spanning the rule
//!    set — loops, jumps, labels, `try`, classes, async functions, `await`, generators,
//!    `yield`, regular expressions, `this`, promises and the operators that both read and
//!    write — and over every operation that stores code (`CreateFunction`,
//!    `UpdateFunction`, `CreateResolver`, `UpdateResolver`);
//! 2. a refused call stores NOTHING: the function is absent from the API's listing, the
//!    field carries no resolver, and an update leaves the stored definition as it was;
//! 3. the two passes are ordered the way a compiler orders them: code that does not compile
//!    is refused on the compiler's own diagnostics
//!    (`code.js(<line>,<column>): error TS<code>: <sentence>`, `PARSE_ERROR`) and the rule
//!    set is not reached, so a definition that both fails to compile and would break a rule
//!    reports only the compile;
//! 4. code that compiles and breaks no rule is stored and read back verbatim.
//!
//! Every assertion goes through the router the production emulator mounts, so these exercise
//! routing, restJson1 (de)serialization, the boundary validator, the handlers and the shared
//! engine together. The harness (`call`, `create_api`, `router`, `Answer`) is the
//! error-contract suite's, and the data source every definition resolves against is created
//! through the function suite's own helper.

use serde_json::{json, Value};
use service_com_amazonaws_appsync_awsdeepdishcontrolplaneservice::state;
use smithy_server_runtime::MultiServiceRouter;

use super::error_contract::{call, create_api, router, Answer};
use super::functions::{create_data_source, RUNTIME};

/// The data source the definitions under test resolve against.
const DATA_SOURCE: &str = "ds_rules";

/// The type the resolvers under test are attached to.
const TYPE: &str = "Query";

/// One row of the rule-set table: the code a definition would be written with, and, for
/// every rule it breaks, that rule's name, the sentence it states and the STRETCH of the
/// code it is about.
struct Row {
    /// What the row is about, named in every assertion it makes.
    case: &'static str,
    /// The definition's `code`.
    code: &'static str,
    /// The rules the code breaks: `(rule, sentence, construct)`.
    broken: &'static [(&'static str, &'static str, &'static str)],
}

/// The diagnostics `row` is expected to be refused with, derived from its own code: a rule
/// is reported where its construct SITS in the submitted code and over exactly that
/// construct's length, so a row states the construct rather than a position counted by
/// hand — and a diagnostic that drifts off its construct fails the row.
fn expected(row: &Row) -> Vec<(String, i64, i64, i64)> {
    let mut stated: Vec<(String, i64, i64, i64)> = row
        .broken
        .iter()
        .map(|(rule, sentence, construct)| {
            let offset = row.code.find(construct).unwrap_or_else(|| {
                panic!(
                    "case {:?}: the construct {construct:?} is not in the row's code",
                    row.case
                )
            });
            let read = &row.code[..offset];
            let line = i64::try_from(read.matches('\n').count().saturating_add(1)).unwrap_or(-1);
            let column = i64::try_from(
                read.rsplit_once('\n')
                    .map_or(read, |(_, last)| last)
                    .chars()
                    .count()
                    .saturating_add(1),
            )
            .unwrap_or(-1);
            let span = i64::try_from(construct.encode_utf16().count()).unwrap_or(-1);
            (
                format!("code.js({line},{column}): error {rule}: {sentence}"),
                line,
                column,
                span,
            )
        })
        .collect();
    stated.sort_by_key(|(_, line, column, _)| (*line, *column));
    stated
}

/// The rule-set table: one row per construct the set forbids, each written as a handler a
/// client would really submit.
///
/// The first row is the one the recording exercised — a `for` statement whose step uses
/// `++` — which breaks two rules at once and so pins the order the diagnostics come in as
/// well; `the_recorded_constructs_are_reported_as_recorded` states that row's diagnostics
/// literally.
const ROWS: &[Row] = &[
    Row {
        case: "a for statement whose step increments",
        code: "export function request(ctx) {\n  for (let i = 0; i < 3; i++) {}\n  return {};\n}\n",
        broken: &[
            (
                "@aws-appsync/no-for",
                "For statements are not supported.",
                "for (let i = 0; i < 3; i++) {}",
            ),
            (
                "@aws-appsync/no-disallowed-unary-operators",
                "The operator ++ is not supported",
                "i++",
            ),
        ],
    },
    Row {
        case: "a for-of statement over the arguments",
        code: "export function request(ctx) {\n  for (const id of ctx.args.ids) {}\n  return {};\n}\n",
        broken: &[(
            "@aws-appsync/no-for",
            "For statements are not supported.",
            "for (const id of ctx.args.ids) {}",
        )],
    },
    Row {
        case: "a for-in statement over the arguments",
        code: "export function request(ctx) {\n  for (const k in ctx.args) {}\n  return {};\n}\n",
        broken: &[(
            "@aws-appsync/no-for",
            "For statements are not supported.",
            "for (const k in ctx.args) {}",
        )],
    },
    Row {
        case: "a while statement that continues",
        code: "export function request(ctx) {\n  while (ctx.args.go) { continue; }\n  return {};\n}\n",
        broken: &[
            (
                "@aws-appsync/no-while",
                "While statements are not supported.",
                "while (ctx.args.go) { continue; }",
            ),
            (
                "@aws-appsync/no-continue",
                "Continue statements are not supported.",
                "continue;",
            ),
        ],
    },
    Row {
        case: "a do-while statement",
        code: "export function request(ctx) {\n  do {} while (ctx.args.go);\n  return {};\n}\n",
        broken: &[(
            "@aws-appsync/no-while",
            "While statements are not supported.",
            "do {} while (ctx.args.go);",
        )],
    },
    Row {
        case: "a labeled statement",
        code: "export function request(ctx) {\n  top: { break top; }\n  return {};\n}\n",
        broken: &[(
            "@aws-appsync/no-labels",
            "Labeled statements are not supported.",
            "top: { break top; }",
        )],
    },
    Row {
        case: "a try statement",
        code: "export function request(ctx) {\n  try { return {}; } catch (e) { return {}; }\n}\n",
        broken: &[(
            "@aws-appsync/no-try",
            "Try statements are not supported.",
            "try { return {}; } catch (e) { return {}; }",
        )],
    },
    Row {
        case: "a class declaration",
        code: "class Key {}\nexport function request(ctx) {\n  return {};\n}\n",
        broken: &[(
            "@aws-appsync/no-classes",
            "Classes are not supported.",
            "class Key {}",
        )],
    },
    Row {
        case: "an async handler that awaits",
        code: "export async function request(ctx) {\n  return await ctx.args;\n}\n",
        broken: &[
            (
                "@aws-appsync/no-async",
                "Async functions are not supported.",
                "async function request(ctx) {\n  return await ctx.args;\n}",
            ),
            (
                "@aws-appsync/no-await",
                "Await expressions are not supported.",
                "await ctx.args",
            ),
        ],
    },
    Row {
        case: "a generator that yields",
        code: "export function request(ctx) {\n  function* ids() { yield 1; }\n  return {};\n}\n",
        broken: &[
            (
                "@aws-appsync/no-generators",
                "Generator functions are not supported.",
                "function* ids() { yield 1; }",
            ),
            (
                "@aws-appsync/no-yield",
                "Yield expressions are not supported.",
                "yield 1",
            ),
        ],
    },
    Row {
        case: "a regular expression literal",
        code: "export function request(ctx) {\n  return { ok: /^a+$/.test(ctx.args.s) };\n}\n",
        broken: &[(
            "@aws-appsync/no-regex",
            "Regular expressions are not supported.",
            "/^a+$/",
        )],
    },
    Row {
        case: "the this keyword",
        code: "export function request(ctx) {\n  return this;\n}\n",
        broken: &[(
            "@aws-appsync/no-this",
            "The this keyword is not supported.",
            "this",
        )],
    },
    Row {
        case: "a promise",
        code: "export function request(ctx) {\n  return Promise.resolve({});\n}\n",
        broken: &[(
            "@aws-appsync/no-promise",
            "Promises are not supported.",
            "Promise",
        )],
    },
    Row {
        case: "a decrementing operator",
        code: "export function request(ctx) {\n  let n = ctx.args.n;\n  n--;\n  return { n };\n}\n",
        broken: &[(
            "@aws-appsync/no-disallowed-unary-operators",
            "The operator -- is not supported",
            "n--",
        )],
    },
];

/// Code that compiles and breaks no rule, in the forms a real APPSYNC_JS definition takes —
/// including the constructs the rules are about where they are NOT constructs at all (a
/// property named like a statement, a keyword inside a string or a comment).
const ACCEPTED_CODE: &[(&str, &str)] = &[
    (
        "both handlers",
        "export function request(ctx) { return { payload: ctx.args }; }\n\
         export function response(ctx) { return ctx.result; }\n",
    ),
    (
        "a handler mapping over the result",
        "export function response(ctx) { return ctx.result.items.map((i) => i.id); }\n",
    ),
    (
        "a handler whose properties are named like statements",
        "export function request(ctx) {\n  const o = { for: 1, this: 2, class: 3 };\n  \
         return { v: o.for + o.this + o.class };\n}\n",
    ),
    (
        "a handler whose text mentions the constructs",
        "export function request(ctx) {\n  // for (;;) { this ++ }\n  \
         return { s: 'for while try this ++' };\n}\n",
    ),
];

/// A `CreateFunction` / `UpdateFunction` body defining the function `name` by `code`.
fn function_body(name: &str, code: &str) -> Value {
    json!({
        "name": name,
        "dataSourceName": DATA_SOURCE,
        "code": code,
        "runtime": {"name": RUNTIME.0, "runtimeVersion": RUNTIME.1},
    })
}

/// A `CreateResolver` / `UpdateResolver` body defining the resolver on `field` by `code`.
fn resolver_body(field: &str, code: &str) -> Value {
    json!({
        "fieldName": field,
        "dataSourceName": DATA_SOURCE,
        "code": code,
        "runtime": {"name": RUNTIME.0, "runtimeVersion": RUNTIME.1},
    })
}

/// Send one `CreateFunction` defining `name` by `code`.
async fn create_function(
    router: &MultiServiceRouter,
    api_id: &str,
    name: &str,
    code: &str,
) -> Answer {
    call(
        router,
        "POST",
        &format!("/v1/apis/{api_id}/functions"),
        Some(function_body(name, code)),
    )
    .await
}

/// Send one `CreateResolver` defining the resolver on `field` by `code`.
async fn create_resolver(
    router: &MultiServiceRouter,
    api_id: &str,
    field: &str,
    code: &str,
) -> Answer {
    call(
        router,
        "POST",
        &format!("/v1/apis/{api_id}/types/{TYPE}/resolvers"),
        Some(resolver_body(field, code)),
    )
    .await
}

/// List the functions of `api_id`.
async fn list_functions(router: &MultiServiceRouter, api_id: &str) -> Vec<Value> {
    let answer = call(router, "GET", &format!("/v1/apis/{api_id}/functions"), None).await;
    answer.body["functions"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

/// Assert `answer` is the refusal a definition breaking the rule set takes, reporting
/// exactly `diagnostics`.
fn assert_rule_errors(case: &str, answer: &Answer, diagnostics: &[(String, i64, i64, i64)]) {
    assert_eq!(
        (
            answer.status,
            answer.error_type.as_str(),
            answer.message.as_str(),
            answer.body["reason"].as_str(),
        ),
        (
            400,
            "BadRequestException",
            state::CODE_ERRORS_MESSAGE,
            Some(state::CODE_ERROR_REASON),
        ),
        "case {case:?}: expected 400 BadRequestException {:?} with the {:?} reason, got \
         {answer:?}",
        state::CODE_ERRORS_MESSAGE,
        state::CODE_ERROR_REASON
    );

    let reported: Vec<(String, i64, i64, i64)> = answer.body["detail"]["codeErrors"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|fault| {
            assert_eq!(
                fault["errorType"].as_str(),
                Some(state::LINT_ERROR),
                "case {case:?}: expected the rule-set errorType, got {fault:?}"
            );
            (
                fault["value"].as_str().unwrap_or_default().to_string(),
                fault["location"]["line"].as_i64().unwrap_or_default(),
                fault["location"]["column"].as_i64().unwrap_or_default(),
                fault["location"]["span"].as_i64().unwrap_or_default(),
            )
        })
        .collect();
    assert_eq!(
        reported, diagnostics,
        "case {case:?}: expected one diagnostic per broken rule, got {answer:?}"
    );
}

/// Rules 1 and 2: a create whose code breaks the rule set is refused with one diagnostic
/// per broken rule and stores nothing — neither a function in the listing nor a resolver on
/// the field.
#[tokio::test]
async fn a_create_whose_code_breaks_a_rule_is_refused_and_stores_nothing() {
    for (index, row) in ROWS.iter().enumerate() {
        let router = router();
        let api_id = create_api(&router, &format!("rules-create-{index}")).await;
        create_data_source(&router, &api_id, DATA_SOURCE).await;

        let answer = create_function(&router, &api_id, "fn_bad", row.code).await;
        assert_rule_errors(
            &format!("CreateFunction with {}", row.case),
            &answer,
            &expected(row),
        );
        let listed = list_functions(&router, &api_id).await;
        assert!(
            listed.is_empty(),
            "case CreateFunction with {:?}: expected no function in the listing, got {listed:?}",
            row.case
        );

        let field = "ruleBreaker";
        let answer = create_resolver(&router, &api_id, field, row.code).await;
        assert_rule_errors(
            &format!("CreateResolver with {}", row.case),
            &answer,
            &expected(row),
        );
        let read = call(
            &router,
            "GET",
            &format!("/v1/apis/{api_id}/types/{TYPE}/resolvers/{field}"),
            None,
        )
        .await;
        assert_eq!(
            read.status, 404,
            "case CreateResolver with {:?}: expected the field to carry no resolver, got {read:?}",
            row.case
        );
    }
}

/// Rules 1 and 2 on the update path: an update whose code breaks the rule set is refused the
/// same way and leaves the stored definition exactly as it was.
#[tokio::test]
async fn an_update_whose_code_breaks_a_rule_is_refused_and_changes_nothing() {
    let router = router();
    let api_id = create_api(&router, "rules-update").await;
    create_data_source(&router, &api_id, DATA_SOURCE).await;

    let good = ACCEPTED_CODE[0].1;
    let created = create_function(&router, &api_id, "fn_ok", good).await;
    assert_eq!(
        created.status, 200,
        "case create the function the updates leave alone: expected 200, got {created:?}"
    );
    let function_id = created.body["functionConfiguration"]["functionId"]
        .as_str()
        .unwrap_or_default()
        .to_string();

    let field = "okCode";
    let attached = create_resolver(&router, &api_id, field, good).await;
    assert_eq!(
        attached.status, 200,
        "case attach the resolver the updates leave alone: expected 200, got {attached:?}"
    );

    for row in ROWS {
        let answer = call(
            &router,
            "POST",
            &format!("/v1/apis/{api_id}/functions/{function_id}"),
            Some(function_body("fn_ok", row.code)),
        )
        .await;
        assert_rule_errors(
            &format!("UpdateFunction with {}", row.case),
            &answer,
            &expected(row),
        );
        let read = call(
            &router,
            "GET",
            &format!("/v1/apis/{api_id}/functions/{function_id}"),
            None,
        )
        .await;
        assert_eq!(
            (
                read.status,
                read.body["functionConfiguration"]["code"].as_str()
            ),
            (200, Some(good)),
            "case UpdateFunction with {:?}: expected the stored code unchanged, got {read:?}",
            row.case
        );

        let answer = call(
            &router,
            "POST",
            &format!("/v1/apis/{api_id}/types/{TYPE}/resolvers/{field}"),
            Some(resolver_body(field, row.code)),
        )
        .await;
        assert_rule_errors(
            &format!("UpdateResolver with {}", row.case),
            &answer,
            &expected(row),
        );
        let read = call(
            &router,
            "GET",
            &format!("/v1/apis/{api_id}/types/{TYPE}/resolvers/{field}"),
            None,
        )
        .await;
        assert_eq!(
            (read.status, read.body["resolver"]["code"].as_str()),
            (200, Some(good)),
            "case UpdateResolver with {:?}: expected the stored code unchanged, got {read:?}",
            row.case
        );
    }
}

/// Rule 3: the compile comes first. Code that does not compile is refused on the compiler's
/// own diagnostic — naming the TypeScript error the fault falls under at the position in the
/// submitted code — and the rule set is not reached, even where the code would break one.
#[tokio::test]
async fn code_that_does_not_compile_is_refused_before_the_rule_set_is_reached() {
    let router = router();
    let api_id = create_api(&router, "rules-compile-first").await;
    create_data_source(&router, &api_id, DATA_SOURCE).await;

    // (case, code, the diagnostic the compiler states first)
    let cases: &[(&str, &str, &str)] = &[
        (
            "a handler whose body never closes",
            "export function request(ctx) {\n  return {};\n",
            "code.js(3,1): error TS1005: '}' expected.\n",
        ),
        (
            "a handler whose object literal never closes, with a later export inside it",
            "export function request(ctx) {\n  return {\n    operation: 'GetItem',\n    \
             key: { id: ctx.args.id },\n  };\nexport function response(ctx) { return \
             ctx.result; }\n",
            "code.js(7,1): error TS1005: '}' expected.\n",
        ),
        (
            "a loop inside a handler whose body never closes",
            "export function request(ctx) {\n  for (let i = 0; i < 3; i++) {}\n",
            "code.js(3,1): error TS1005: '}' expected.\n",
        ),
    ];

    for (case, code, stated) in cases {
        for (operation, answer) in [
            (
                "CreateFunction",
                create_function(&router, &api_id, "fn_bad", code).await,
            ),
            (
                "CreateResolver",
                create_resolver(&router, &api_id, "brokenCode", code).await,
            ),
        ] {
            assert_eq!(
                (
                    answer.status,
                    answer.message.as_str(),
                    answer.body["reason"].as_str(),
                    answer.body["detail"]["codeErrors"][0]["errorType"].as_str(),
                    answer.body["detail"]["codeErrors"][0]["value"].as_str(),
                ),
                (
                    400,
                    state::CODE_ERRORS_MESSAGE,
                    Some(state::CODE_ERROR_REASON),
                    Some(state::PARSE_ERROR),
                    Some(*stated),
                ),
                "case {operation} with {case:?}: expected the compiler's own diagnostic, got \
                 {answer:?}"
            );
        }
    }

    // Nothing was stored for any of them.
    let listed = list_functions(&router, &api_id).await;
    assert!(
        listed.is_empty(),
        "case the listing after the refused creates: expected no function, got {listed:?}"
    );
}

/// Rule 4: code that compiles and breaks no rule is stored and read back verbatim, and the
/// listing holds exactly the definitions that were accepted.
#[tokio::test]
async fn code_that_breaks_no_rule_is_stored_and_read_back() {
    let router = router();
    let api_id = create_api(&router, "rules-accepted").await;
    create_data_source(&router, &api_id, DATA_SOURCE).await;

    for (index, (case, code)) in ACCEPTED_CODE.iter().enumerate() {
        let answer = create_function(&router, &api_id, "fn_ok", code).await;
        assert_eq!(
            (
                answer.status,
                answer.body["functionConfiguration"]["code"].as_str()
            ),
            (200, Some(*code)),
            "case CreateFunction with {case:?}: expected 200 echoing the code, got {answer:?}"
        );
        let function_id = answer.body["functionConfiguration"]["functionId"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        let read = call(
            &router,
            "GET",
            &format!("/v1/apis/{api_id}/functions/{function_id}"),
            None,
        )
        .await;
        assert_eq!(
            (
                read.status,
                read.body["functionConfiguration"]["code"].as_str()
            ),
            (200, Some(*code)),
            "case CreateFunction with {case:?}: expected the stored code read back, got {read:?}"
        );

        let field = format!("okCode{index}");
        let answer = create_resolver(&router, &api_id, &field, code).await;
        assert_eq!(
            (answer.status, answer.body["resolver"]["code"].as_str()),
            (200, Some(*code)),
            "case CreateResolver with {case:?}: expected 200 echoing the code, got {answer:?}"
        );
        let read = call(
            &router,
            "GET",
            &format!("/v1/apis/{api_id}/types/{TYPE}/resolvers/{field}"),
            None,
        )
        .await;
        assert_eq!(
            (read.status, read.body["resolver"]["code"].as_str()),
            (200, Some(*code)),
            "case CreateResolver with {case:?}: expected the stored code read back, got {read:?}"
        );
    }

    let listed = list_functions(&router, &api_id).await;
    assert_eq!(
        listed.len(),
        ACCEPTED_CODE.len(),
        "case the listing after the accepted creates: expected the {} that were accepted, got \
         {listed:?}",
        ACCEPTED_CODE.len()
    );
}

/// Rule 1, stated literally for the construct pair a recording exercised: a `for` statement
/// whose step increments is refused with those two diagnostics, value for value, so the
/// wording, the order, the positions and the lengths are all pinned here and not only
/// derived from the row.
#[tokio::test]
async fn the_recorded_constructs_are_reported_value_for_value() {
    let router = router();
    let api_id = create_api(&router, "rules-recorded").await;
    create_data_source(&router, &api_id, DATA_SOURCE).await;

    let code = "export function request(ctx) {\n  for (let i = 0; i < 3; i++) {}\n  return {};\n}\n";
    let recorded = [
        (
            "code.js(2,3): error @aws-appsync/no-for: For statements are not supported."
                .to_string(),
            2,
            3,
            30,
        ),
        (
            "code.js(2,26): error @aws-appsync/no-disallowed-unary-operators: The operator ++ \
             is not supported"
                .to_string(),
            2,
            26,
            3,
        ),
    ];

    let answer = create_function(&router, &api_id, "fn_bad", code).await;
    assert_rule_errors(
        "CreateFunction with a for statement whose step increments",
        &answer,
        &recorded,
    );
    let answer = create_resolver(&router, &api_id, "incrementing", code).await;
    assert_rule_errors(
        "CreateResolver with a for statement whose step increments",
        &answer,
        &recorded,
    );

    // Neither call stored anything: the function is absent from the listing the client
    // inventories the API with.
    let listed = list_functions(&router, &api_id).await;
    assert!(
        listed.is_empty(),
        "case the listing after the refused create: expected no function, got {listed:?}"
    );
}

/// The read of a definition holds along the ways code gets DEEP without ever leaving a
/// bracket unclosed — the chains that nest without a bracket in sight, statements nested
/// inside statements, blocks inside blocks and a template interpolated into a template — so a
/// definition the compiler reads to the end is STORED and read back, however deep it goes.
///
/// The rows differ along the construct that nests (conditional, assignment, arrow and
/// arithmetic chains; `if` statements; blocks; template interpolations) and are each built as
/// deep as the modelled maximum length of `code` allows for that construct, which is where a
/// reader that recursed on the host's own stack would stop answering.
#[tokio::test]
async fn a_definition_whose_code_nests_without_unclosed_brackets_is_stored() {
    let router = router();
    let api_id = create_api(&router, "rules-deep").await;
    create_data_source(&router, &api_id, DATA_SOURCE).await;

    // A template interpolated into a template, to the depth the row asks for.
    let mut templates = "0".to_string();
    for _ in 0..1_000 {
        templates = format!("`${{{templates}}}`");
    }

    let deep: &[(&str, String)] = &[
        ("a conditional chain", format!("{}0", "ctx?0:".repeat(4_000))),
        ("an assignment chain", format!("{}0", "n=".repeat(10_000))),
        ("an arrow chain", format!("{}0", "f=>".repeat(8_000))),
        ("an arithmetic chain", format!("{}0", "1+".repeat(12_000))),
        ("nested template interpolations", templates),
    ];

    let mut created = 0;
    for (case, nested) in deep {
        let code = format!("export function request(ctx) {{ return {nested}; }}");
        let answer = create_function(&router, &api_id, "fn_deep", &code).await;
        assert_eq!(
            (
                answer.status,
                answer.body["functionConfiguration"]["code"].as_str()
            ),
            (200, Some(code.as_str())),
            "case {case:?}: expected 200 echoing the code, got {answer:?}"
        );
        created += 1;
        let function_id = answer.body["functionConfiguration"]["functionId"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        let read = call(
            &router,
            "GET",
            &format!("/v1/apis/{api_id}/functions/{function_id}"),
            None,
        )
        .await;
        assert_eq!(
            (
                read.status,
                read.body["functionConfiguration"]["code"].as_str()
            ),
            (200, Some(code.as_str())),
            "case {case:?}: expected the stored code read back, got {read:?}"
        );
    }

    // Statements nested in statements, and a binding pattern nested in a binding pattern,
    // which all leave every bracket closed: one definition whose handler is a stack of `if`s,
    // one whose handler is a stack of blocks, and one whose handler destructures through a
    // deeply nested array pattern.
    for (case, statements) in [
        ("nested if statements", "if (ctx) ".repeat(3_000) + "0;"),
        (
            "nested blocks",
            format!("{}0;{}", "{".repeat(8_000), "}".repeat(8_000)),
        ),
        (
            "a nested destructuring target",
            format!(
                "const {}x{} = ctx.args.nested;",
                "[".repeat(4_000),
                "]".repeat(4_000)
            ),
        ),
    ] {
        let code = format!("export function request(ctx) {{ {statements} return {{}}; }}");
        let answer = create_function(&router, &api_id, "fn_deep", &code).await;
        assert_eq!(
            (
                answer.status,
                answer.body["functionConfiguration"]["code"].as_str()
            ),
            (200, Some(code.as_str())),
            "case {case:?}: expected 200 echoing the code, got {answer:?}"
        );
        created += 1;
    }

    // Every one of them was accepted, so the listing holds one function per row.
    let listed = list_functions(&router, &api_id).await;
    assert_eq!(
        listed.len(),
        created,
        "case the listing after the deeply nested definitions: expected one function per row, \
         got {listed:?}"
    );
}
