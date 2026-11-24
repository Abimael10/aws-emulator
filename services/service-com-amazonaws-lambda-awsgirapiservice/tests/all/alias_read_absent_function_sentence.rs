// SPDX-License-Identifier: CC0-1.0

//! The SENTENCE `GetAlias` refuses a read with when the function itself is not there.
//!
//! The rule under test: the resource `GetAlias` addresses is the ALIAS its two URI labels
//! name, so every reason no such alias can be read is reported as that one absence — the
//! modelled `ResourceNotFoundException` (HTTP 404) whose message is the fixed lead-in
//! [`ALIAS_NOT_FOUND_PREFIX`] followed by the alias ARN the request asked about (the
//! function's unqualified ARN with the requested alias name as its qualifier). Whether the
//! function holds no alias of the name, was deleted with its aliases, was recreated under
//! the same name without them, or never existed at all, the refusal is the same sentence;
//! the operations whose subject IS the function keep the function-keyed phrase, so a caller
//! still has a read that tells it "no such function".
//!
//! The rows differ along what the rule generalises over: how the alias came not to be
//! readable (the function deleted under it, the function recreated without it, a function
//! that never existed), the alias name (its modelled length bounds, both separators the
//! pattern admits, mixed case with digits), the documented `FunctionName` form the request
//! addressed the function by (name, function ARN, partial ARN), and how many versions the
//! deleted function had published. Facts about the REQUEST are checked too: a
//! `FunctionName` that qualifies a version names no function whose aliases to look in, and
//! that stays a 400 rather than becoming this 404, so the refusal order does not depend on
//! whether the function exists.

use std::sync::Arc;

use serde_json::{json, Value};
use service_com_amazonaws_lambda_awsgirapiservice::state::ComAmazonawsLambdaAwsgirapiserviceState;

use crate::alias_update::{
    call, create_alias, create_function, get_alias, router_on, Answer, ACCOUNT, FUNCTIONS_URI,
    REGION,
};

/// The error code the model declares, at HTTP 404, for a resource `GetAlias` cannot find:
/// `smithy_query(query="operation", operation="GetAlias")` lists
/// `ResourceNotFoundException` — "The resource specified in the request does not exist."
const RESOURCE_NOT_FOUND: &str = "ResourceNotFoundException";

/// The error code the model declares, at HTTP 400, for a parameter the request may not
/// carry: `InvalidParameterValueException`.
const INVALID_PARAMETER_VALUE: &str = "InvalidParameterValueException";

/// The lead-in of the sentence the READ of an alias that cannot be found is refused with,
/// which the alias ARN follows.
const ALIAS_NOT_FOUND_PREFIX: &str = "Cannot find alias arn: ";

/// The lead-in of the sentence the read of a FUNCTION that cannot be found is refused
/// with.
const FUNCTION_NOT_FOUND_PREFIX: &str = "Function not found: ";

/// The `@length` maximum the model puts on `Alias` (`@length(min: 1, max: 128)`), from
/// `smithy_query(query="shape", shape="com.amazonaws.lambda#Alias")`.
const ALIAS_MAX_LEN: usize = 128;

/// The unqualified ARN a function of `name` carries in this suite's Region and account.
fn function_arn(name: &str) -> String {
    format!("arn:aws:lambda:{REGION}:{ACCOUNT}:function:{name}")
}

/// The alias ARN a refusal names: the function's own unqualified ARN qualified by the
/// alias name.
fn alias_arn(function: &str, alias: &str) -> String {
    format!("{}:{alias}", function_arn(function))
}

/// Alias names the modelled `Alias` constraints admit (pattern
/// `^(?!^[0-9]+$)([a-zA-Z0-9-_]+)$`, length 1..128), spanning the class the sentence
/// generalises over: the length minimum, both separators, mixed case with digits, and the
/// length maximum.
fn alias_names() -> Vec<(String, String)> {
    vec![
        (
            "one character, the length minimum".to_string(),
            "g".to_string(),
        ),
        ("a hyphenated name".to_string(), "blue-green".to_string()),
        ("an underscored name".to_string(), "LATEST_live".to_string()),
        ("mixed case with digits".to_string(), "Prod2024v2".to_string()),
        (
            "at the length maximum".to_string(),
            format!("g{}", "h".repeat(ALIAS_MAX_LEN - 1)),
        ),
    ]
}

/// The documented `FunctionName` forms a request addresses a function by ("Function name",
/// "Function ARN", "Partial ARN", from `smithy_query(query="doc",
/// shape="com.amazonaws.lambda#GetAliasRequest", member="FunctionName")`). The ARN in the
/// refusal names the function, whichever form the request used.
fn references(function: &str) -> Vec<(String, String)> {
    vec![
        ("the bare name".to_string(), function.to_string()),
        ("the function ARN".to_string(), function_arn(function)),
        (
            "the partial ARN".to_string(),
            format!("{ACCOUNT}:function:{function}"),
        ),
    ]
}

/// `DeleteFunction` with no `Qualifier`, which takes the function's versions and aliases
/// with it.
async fn delete_function(router: &smithy_server_runtime::MultiServiceRouter, name: &str) -> Answer {
    call(
        router,
        "DELETE",
        &format!("{FUNCTIONS_URI}/{name}"),
        &Value::Null,
    )
    .await
}

/// Assert one answer is the modelled 404 naming the alias of `alias` on `function`.
fn assert_alias_refusal(case: &str, function: &str, alias: &str, answer: &Answer) {
    let expected = format!("{ALIAS_NOT_FOUND_PREFIX}{}", alias_arn(function, alias));
    assert_eq!(
        (answer.status, answer.error_code()),
        (404, RESOURCE_NOT_FOUND.to_string()),
        "case {case:?}: expected 404 {RESOURCE_NOT_FOUND}, got {} {}",
        answer.status,
        answer.text()
    );
    assert_eq!(
        answer.message(),
        expected,
        "case {case:?}: expected {expected:?}, got {:?}",
        answer.message()
    );
}

/// One row of the deletion table: the function it works on, how many versions that
/// function publishes, and the alias created on it before the delete.
struct DeletionCase {
    function: &'static str,
    published: usize,
    /// The version the alias invokes, which the row's publish count has to admit.
    invokes: &'static str,
}

#[tokio::test]
async fn an_alias_of_a_deleted_function_is_refused_by_the_alias_sentence() {
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = router_on(&engine);

    let cases = [
        DeletionCase {
            function: "gone-unpublished",
            published: 0,
            invokes: "$LATEST",
        },
        DeletionCase {
            function: "gone-one-version",
            published: 1,
            invokes: "1",
        },
        DeletionCase {
            function: "gone-two-versions",
            published: 2,
            invokes: "2",
        },
    ];

    for row in &cases {
        for (form, alias) in alias_names() {
            create_function(&router, row.function, row.published).await;
            let created = create_alias(
                &router,
                row.function,
                &json!({ "Name": alias, "FunctionVersion": row.invokes }),
            )
            .await;
            assert_eq!(
                created.status,
                201,
                "case {form:?} on {}: the alias must be created, got {} {}",
                row.function,
                created.status,
                created.text()
            );
            // The control: while the function is there the alias is reported, so each
            // refusal below is about the delete rather than about a broken reader.
            let held = get_alias(&router, row.function, &alias).await;
            assert_eq!(
                (held.status, held.string("AliasArn")),
                (200, Some(alias_arn(row.function, &alias))),
                "case {form:?} on {}: the alias must be readable before the delete, got {} {}",
                row.function,
                held.status,
                held.text()
            );

            let deleted = delete_function(&router, row.function).await;
            assert_eq!(
                deleted.status,
                204,
                "case {form:?} on {}: DeleteFunction must succeed, got {} {}",
                row.function,
                deleted.status,
                deleted.text()
            );

            // The alias went with the function, so the read of it is refused — naming the
            // ALIAS the request asked about, through every documented `FunctionName` form.
            for (addressed, reference) in references(row.function) {
                let case = format!("{form} on {} addressed by {addressed}", row.function);
                let answer = get_alias(&router, &reference, &alias).await;
                assert_alias_refusal(&case, row.function, &alias, &answer);

                // The read changed nothing: asking again answers the same sentence.
                let again = get_alias(&router, &reference, &alias).await;
                assert_alias_refusal(&format!("{case}, read twice"), row.function, &alias, &again);
            }

            // The read of the FUNCTION, whose subject IS the function, keeps the
            // function-keyed phrase — at a published version it names that version's ARN.
            let function_read = call(
                &router,
                "GET",
                &format!("{FUNCTIONS_URI}/{}?Qualifier={}", row.function, row.invokes),
                &Value::Null,
            )
            .await;
            let qualified = if row.invokes == "$LATEST" {
                function_arn(row.function)
            } else {
                format!("{}:{}", function_arn(row.function), row.invokes)
            };
            let expected = format!("{FUNCTION_NOT_FOUND_PREFIX}{qualified}");
            assert_eq!(
                (function_read.status, function_read.message()),
                (404, expected.clone()),
                "case {form:?} on {}: GetFunction must answer {expected:?}, got {} {}",
                row.function,
                function_read.status,
                function_read.text()
            );

            // The second leg: the function is recreated under the SAME name and holds no
            // aliases, so the same read is refused with the same sentence.
            create_function(&router, row.function, row.published).await;
            for (addressed, reference) in references(row.function) {
                let case = format!(
                    "{form} on {} recreated, addressed by {addressed}",
                    row.function
                );
                let answer = get_alias(&router, &reference, &alias).await;
                assert_alias_refusal(&case, row.function, &alias, &answer);
            }

            // And the alias created again on the recreated function reads back, so the
            // refusals above were about absence and nothing was left poisoned.
            let recreated = create_alias(
                &router,
                row.function,
                &json!({ "Name": alias, "FunctionVersion": row.invokes }),
            )
            .await;
            assert_eq!(
                recreated.status,
                201,
                "case {form:?} on {}: the alias must be creatable again, got {} {}",
                row.function,
                recreated.status,
                recreated.text()
            );
            let readable = get_alias(&router, row.function, &alias).await;
            assert_eq!(
                (readable.status, readable.string("FunctionVersion")),
                (200, Some(row.invokes.to_string())),
                "case {form:?} on {}: the recreated alias must be readable, got {} {}",
                row.function,
                readable.status,
                readable.text()
            );

            // Leave the store empty for the next row of the table.
            let cleaned = delete_function(&router, row.function).await;
            assert_eq!(
                cleaned.status,
                204,
                "case {form:?} on {}: the clean-up delete must succeed, got {} {}",
                row.function,
                cleaned.status,
                cleaned.text()
            );
        }
    }

    let state = engine.state::<ComAmazonawsLambdaAwsgirapiserviceState>();
    let store = state.lock();
    assert!(
        store.functions.is_empty(),
        "a refused GetAlias must store nothing, got {:?}",
        store.functions.keys().collect::<Vec<_>>()
    );
}

/// RULE: the sentence does not depend on the function ever having existed — a name no
/// function was created under is refused as the missing ALIAS too.
#[tokio::test]
async fn an_alias_of_a_function_that_never_existed_is_refused_by_the_alias_sentence() {
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = router_on(&engine);

    for (form, alias) in alias_names() {
        for (addressed, reference) in references("never-created-fn") {
            let case = format!("{form} addressed by {addressed}");
            let answer = get_alias(&router, &reference, &alias).await;
            assert_alias_refusal(&case, "never-created-fn", &alias, &answer);
        }
    }

    let state = engine.state::<ComAmazonawsLambdaAwsgirapiserviceState>();
    let store = state.lock();
    assert!(
        store.functions.is_empty(),
        "a refused GetAlias must create no function, got {:?}",
        store.functions.keys().collect::<Vec<_>>()
    );
}

/// RULE: a `FunctionName` that qualifies a version or an alias names no function whose
/// aliases to look in — a fact about the REQUEST, so it is refused as the modelled 400
/// before anything is looked up, whether or not the function exists.
#[tokio::test]
async fn a_qualified_function_name_stays_a_request_refusal() {
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = router_on(&engine);
    create_function(&router, "qualified-read", 1).await;

    for (form, alias) in alias_names() {
        for (existence, function) in [
            ("a function that exists", "qualified-read"),
            ("a function that does not exist", "qualified-absent"),
        ] {
            for qualifier in ["1", "live"] {
                let case = format!("{form} on {existence} qualified by {qualifier}");
                let answer = get_alias(&router, &format!("{function}:{qualifier}"), &alias).await;
                assert_eq!(
                    (answer.status, answer.error_code()),
                    (400, INVALID_PARAMETER_VALUE.to_string()),
                    "case {case:?}: expected 400 {INVALID_PARAMETER_VALUE}, got {} {}",
                    answer.status,
                    answer.text()
                );
                assert!(
                    answer.message().contains(qualifier),
                    "case {case:?}: the refusal must name the qualifier it read, got {:?}",
                    answer.message()
                );
            }
        }
    }
}
