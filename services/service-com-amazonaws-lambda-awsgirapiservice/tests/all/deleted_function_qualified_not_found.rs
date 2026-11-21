// SPDX-License-Identifier: CC0-1.0

//! What a QUALIFIED read of a function that was DELETED unqualified says it could not
//! find.
//!
//! The rule under test: an unqualified `DeleteFunction` removes the function together with
//! every version and alias of it, and the resource a later read addresses is still a
//! function AT a qualifier — so the modelled `ResourceNotFoundException` (HTTP 404) names
//! the ARN the request RESOLVED: the fixed text `Function not found: ` followed by the
//! function's ARN in the Region of the endpoint, qualified by the qualifier the request
//! named (in the `Qualifier` parameter or in the `name:qualifier` form of `FunctionName`)
//! and left unqualified where it named none. A caller cleaning up versions cannot tell
//! which of the two is gone when the suffix is dropped, which is why it belongs in the
//! sentence whether the version alone or the whole function is absent.
//!
//! The rows differ along what the rule generalises over: the qualifier named (single and
//! two digit published version numbers, the alias the function carried, the unpublished
//! version named explicitly, and none at all), where that qualifier travels (the query
//! parameter and the `FunctionName` itself), the documented `FunctionName` form the request
//! addresses the function by, and which read it is (`GetFunction` and
//! `GetFunctionConfiguration`, which resolve the same pair). A control creates the function
//! again under the same name and reads it back at version 1 and at the alias it never
//! regained, so the sentence is shown to follow the resolution rather than the name having
//! ever been used.

use std::sync::Arc;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The ONE function/alias harness this test binary has.
use crate::alias_update::{
    call, create_alias, create_function, router_on, Answer, ACCOUNT, FUNCTIONS_URI, LATEST, REGION,
};

/// The function this suite creates, publishes versions of, aliases and then deletes.
const FUNCTION: &str = "deleted-qualified";

/// How many versions it publishes before the delete, so a two-digit version number is
/// among the qualifiers a read can name.
const PUBLISHED_VERSIONS: usize = 10;

/// The alias it carries before the delete.
const ALIAS: &str = "live";

/// The fixed text of the sentence, which is the whole message bar the resource it names.
const NOT_FOUND_PREFIX: &str = "Function not found: ";

/// The error code the model declares for a resource one of these reads cannot find
/// (`smithy_query(query="operation", operation="GetFunction,GetFunctionConfiguration")`).
const NOT_FOUND_CODE: &str = "ResourceNotFoundException";

/// The unqualified ARN of `name` in this endpoint's Region.
fn function_arn(name: &str) -> String {
    format!("arn:aws:lambda:{REGION}:{ACCOUNT}:function:{name}")
}

/// The resource the rule says a refusal names: the function's ARN qualified by the
/// qualifier the request named, and unqualified where it named none or named the
/// unpublished version — which is how this service spells the unpublished version wherever
/// it reports an ARN for it.
fn resolved_arn(name: &str, qualifier: Option<&str>) -> String {
    match qualifier {
        Some(qualifier) if qualifier != LATEST => format!("{}:{qualifier}", function_arn(name)),
        _ => function_arn(name),
    }
}

/// The URI one of these reads is bound to.
fn lookup_uri(reference: &str, qualifier: Option<&str>, suffix: &str) -> String {
    let query = qualifier.map_or_else(String::new, |value| format!("?Qualifier={value}"));
    format!("{FUNCTIONS_URI}/{reference}{suffix}{query}")
}

/// `GetFunction` on `reference`, with the `Qualifier` query parameter when one is given.
async fn get_function(
    router: &MultiServiceRouter,
    reference: &str,
    qualifier: Option<&str>,
) -> Answer {
    call(router, "GET", &lookup_uri(reference, qualifier, ""), &Value::Null).await
}

/// `GetFunctionConfiguration` on the same pair.
async fn get_configuration(
    router: &MultiServiceRouter,
    reference: &str,
    qualifier: Option<&str>,
) -> Answer {
    call(
        router,
        "GET",
        &lookup_uri(reference, qualifier, "/configuration"),
        &Value::Null,
    )
    .await
}

/// `DeleteFunction` on `reference`, carrying no qualifier.
async fn delete_function(router: &MultiServiceRouter, reference: &str) -> Answer {
    call(
        router,
        "DELETE",
        &format!("{FUNCTIONS_URI}/{reference}"),
        &Value::Null,
    )
    .await
}

/// One row: how the request addresses the function, where the qualifier travels, and the
/// qualifier the resolved ARN carries.
struct Case {
    label: &'static str,
    reference: String,
    qualifier: Option<&'static str>,
    named: Option<&'static str>,
}

/// Every row of the table.
fn cases() -> Vec<Case> {
    vec![
        Case {
            label: "the first published version, in the Qualifier parameter",
            reference: FUNCTION.to_string(),
            qualifier: Some("1"),
            named: Some("1"),
        },
        Case {
            label: "the first published version, inside the FunctionName",
            reference: format!("{FUNCTION}:1"),
            qualifier: None,
            named: Some("1"),
        },
        Case {
            label: "a two-digit published version, in the Qualifier parameter",
            reference: FUNCTION.to_string(),
            qualifier: Some("10"),
            named: Some("10"),
        },
        Case {
            label: "the alias the function carried",
            reference: FUNCTION.to_string(),
            qualifier: Some(ALIAS),
            named: Some(ALIAS),
        },
        Case {
            label: "no qualifier at all",
            reference: FUNCTION.to_string(),
            qualifier: None,
            named: None,
        },
        Case {
            label: "the unpublished version named explicitly",
            reference: FUNCTION.to_string(),
            qualifier: Some(LATEST),
            named: Some(LATEST),
        },
        Case {
            label: "addressed by its full ARN, qualified in the name",
            reference: format!("{}:2", function_arn(FUNCTION)),
            qualifier: None,
            named: Some("2"),
        },
        Case {
            label: "addressed by its partial ARN",
            reference: format!("{ACCOUNT}:function:{FUNCTION}"),
            qualifier: Some("3"),
            named: Some("3"),
        },
        Case {
            label: "addressed by the function: prefix form",
            reference: format!("function:{FUNCTION}"),
            qualifier: Some("10"),
            named: Some("10"),
        },
    ]
}

/// Assert one answer is the modelled 404 naming the ARN the pair resolved.
fn assert_resolved_refusal(label: &str, operation: &str, named: Option<&str>, answer: &Answer) {
    let expected = format!("{NOT_FOUND_PREFIX}{}", resolved_arn(FUNCTION, named));
    assert_eq!(
        (answer.status, answer.error_code()),
        (404, NOT_FOUND_CODE.to_string()),
        "case {label:?} on {operation}: expected 404 {NOT_FOUND_CODE}, got {} {}",
        answer.status,
        answer.text()
    );
    assert_eq!(
        answer.message(),
        expected,
        "case {label:?} on {operation}: expected {expected:?}, got {:?}",
        answer.message()
    );
}

#[tokio::test]
async fn a_qualified_read_of_a_deleted_function_names_the_qualified_arn() {
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = router_on(&engine);
    create_function(&router, FUNCTION, PUBLISHED_VERSIONS).await;
    let aliased = create_alias(
        &router,
        FUNCTION,
        &json!({ "Name": ALIAS, "FunctionVersion": "1" }),
    )
    .await;
    assert_eq!(
        aliased.status,
        201,
        "the alias the delete removes must be created, got {} {}",
        aliased.status,
        aliased.text()
    );

    let deleted = delete_function(&router, FUNCTION).await;
    assert_eq!(
        deleted.status,
        204,
        "the unqualified DeleteFunction must succeed, got {} {}",
        deleted.status,
        deleted.text()
    );

    for case in cases() {
        let read = get_function(&router, &case.reference, case.qualifier).await;
        assert_resolved_refusal(case.label, "GetFunction", case.named, &read);
        let configuration = get_configuration(&router, &case.reference, case.qualifier).await;
        assert_resolved_refusal(
            case.label,
            "GetFunctionConfiguration",
            case.named,
            &configuration,
        );
    }

    // The control: the same name created again holds version 1 of the NEW function, and
    // still holds no alias — so the sentence tracks what resolves now, not what the name
    // once was.
    create_function(&router, FUNCTION, 1).await;
    let present = get_function(&router, FUNCTION, Some("1")).await;
    assert_eq!(
        present.status,
        200,
        "case \"the recreated function at version 1\": the read must succeed, got {} {}",
        present.status,
        present.text()
    );
    let regained = get_function(&router, FUNCTION, Some(ALIAS)).await;
    assert_resolved_refusal(
        "the recreated function at the alias it never regained",
        "GetFunction",
        Some(ALIAS),
        &regained,
    );
}
