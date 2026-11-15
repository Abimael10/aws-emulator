// SPDX-License-Identifier: CC0-1.0

//! What a LOOKUP of a function says it could not find, when the request addresses that
//! function at a QUALIFIER: `GetFunction` (`GET /2015-03-31/functions/{FunctionName}`) and
//! `GetFunctionConfiguration` (`GET /2015-03-31/functions/{FunctionName}/configuration`),
//! whose `Qualifier` member "specif[y] a version or alias to get details about a published
//! version of the function"
//! (`smithy_query(query="operation", operation="GetFunction,GetFunctionConfiguration")`).
//!
//! The rule under test: **the resource one of these reads addresses is a function AT a
//! version, so the modelled `ResourceNotFoundException` (HTTP 404) for a request that
//! resolves to nothing names the ARN the request RESOLVED — the fixed text
//! `Function not found: ` followed by the ARN of the named function in the Region of the
//! endpoint, qualified by the qualifier the request named (in the `FunctionName` or in the
//! `Qualifier` parameter) and left unqualified where it named none.** The qualifier is what
//! identifies WHICH version is missing, so it belongs in the sentence whether the function
//! itself is absent or only the version is.
//!
//! The rows differ along what the rule generalises over: WHAT is missing (the function
//! itself, a version of a function that exists, an alias of a function that exists), WHERE
//! the qualifier travels (the `Qualifier` query parameter, the `name:qualifier` form of
//! `FunctionName`, nowhere at all), WHICH qualifier it is (a published version number, an
//! alias name, the unpublished version named explicitly), and the documented `FunctionName`
//! form the request addresses the function by (the bare name, the full ARN, the partial ARN,
//! the `function:` prefix form). Two controls keep the table honest: a read that DOES resolve
//! is answered 200, and the function this suite does hold keeps answering after every
//! refusal, so a refusal that disturbed the store fails here.

use std::sync::Arc;

use serde_json::Value;
use smithy_server_runtime::MultiServiceRouter;

// The ONE function/alias harness this test binary has.
use crate::alias_update::{
    call, create_alias, create_function, router_on, Answer, ACCOUNT, FUNCTIONS_URI, LATEST, REGION,
};

/// The function this suite holds, with two published versions and one alias, so a row can
/// address a version and an alias it does NOT have.
const HELD: &str = "lookup-held";

/// A name no function of this store is held under.
const ABSENT: &str = "lookup-absent";

/// The alias the held function has.
const HELD_ALIAS: &str = "live";

/// A published version the held function has.
const HELD_VERSION: &str = "2";

/// A version number no function of this suite ever published.
const MISSING_VERSION: &str = "9";

/// An alias name no function of this suite ever created.
const MISSING_ALIAS: &str = "staging";

/// The fixed text of the sentence, which is the whole message bar the resource it names.
const NOT_FOUND_PREFIX: &str = "Function not found: ";

/// The error code the model declares for a resource one of these reads cannot find.
const NOT_FOUND_CODE: &str = "ResourceNotFoundException";

/// The unqualified ARN of `name` in this endpoint's Region, as every emulator ARN spells
/// one.
fn function_arn(name: &str) -> String {
    format!("arn:aws:lambda:{REGION}:{ACCOUNT}:function:{name}")
}

/// The resource the rule says a refusal names: the function's ARN, qualified by the
/// qualifier the request named, and unqualified where it named none or named the unpublished
/// version — which is how this service spells the unpublished version wherever it reports an
/// ARN for it.
fn addressed_arn(name: &str, qualifier: Option<&str>) -> String {
    match qualifier {
        Some(qualifier) if qualifier != LATEST => format!("{}:{qualifier}", function_arn(name)),
        _ => function_arn(name),
    }
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

/// The URI one of these reads is bound to: the function collection, the reference, the
/// operation's own suffix and the optional `Qualifier` parameter.
fn lookup_uri(reference: &str, qualifier: Option<&str>, suffix: &str) -> String {
    let query = qualifier.map_or_else(String::new, |value| format!("?Qualifier={value}"));
    format!("{FUNCTIONS_URI}/{reference}{suffix}{query}")
}

/// Create the function this suite holds: two published versions and one alias on the first
/// of them.
async fn create_held(router: &MultiServiceRouter) {
    create_function(router, HELD, 2).await;
    let created = create_alias(
        router,
        HELD,
        &serde_json::json!({ "Name": HELD_ALIAS, "FunctionVersion": "1" }),
    )
    .await;
    assert_eq!(
        created.status,
        201,
        "CreateAlias {HELD_ALIAS:?} must succeed, got {} {}",
        created.status,
        created.text()
    );
}

/// One row: how the request addresses the function, where the qualifier travels, and the
/// function name and qualifier the resolved ARN is built from.
struct Case {
    /// What this row varies, for the assertion messages.
    label: &'static str,
    /// The `FunctionName` the request carries, in one of the documented forms.
    reference: String,
    /// The `Qualifier` query parameter, when the row sends one.
    qualifier: Option<&'static str>,
    /// The function the reference names, which the resolved ARN is built from.
    function: &'static str,
    /// The qualifier the pair names, from the reference or from the parameter.
    named: Option<&'static str>,
}

/// Every row of the refusal table.
fn refusal_cases() -> Vec<Case> {
    vec![
        // The function itself is missing, addressed by the bare name — the qualifier in the
        // `Qualifier` parameter, in the name, absent, and the unpublished version named
        // explicitly.
        Case {
            label: "an absent function, no qualifier at all",
            reference: ABSENT.to_string(),
            qualifier: None,
            function: ABSENT,
            named: None,
        },
        Case {
            label: "an absent function, a version in the Qualifier parameter",
            reference: ABSENT.to_string(),
            qualifier: Some(MISSING_VERSION),
            function: ABSENT,
            named: Some(MISSING_VERSION),
        },
        Case {
            label: "an absent function, a version inside the FunctionName",
            reference: format!("{ABSENT}:{MISSING_VERSION}"),
            qualifier: None,
            function: ABSENT,
            named: Some(MISSING_VERSION),
        },
        Case {
            label: "an absent function, an alias name as the qualifier",
            reference: ABSENT.to_string(),
            qualifier: Some(MISSING_ALIAS),
            function: ABSENT,
            named: Some(MISSING_ALIAS),
        },
        Case {
            label: "an absent function, the unpublished version named explicitly",
            reference: ABSENT.to_string(),
            qualifier: Some(LATEST),
            function: ABSENT,
            named: Some(LATEST),
        },
        // The same absent function through the other documented `FunctionName` forms: the
        // ARN the request carries names the same resource, so the sentence is the same.
        Case {
            label: "an absent function addressed by its full ARN, qualified in the name",
            reference: format!("{}:{MISSING_VERSION}", function_arn(ABSENT)),
            qualifier: None,
            function: ABSENT,
            named: Some(MISSING_VERSION),
        },
        Case {
            label: "an absent function addressed by its partial ARN",
            reference: format!("{ACCOUNT}:function:{ABSENT}"),
            qualifier: Some(MISSING_VERSION),
            function: ABSENT,
            named: Some(MISSING_VERSION),
        },
        Case {
            label: "an absent function addressed by the function: prefix form",
            reference: format!("function:{ABSENT}"),
            qualifier: Some(MISSING_ALIAS),
            function: ABSENT,
            named: Some(MISSING_ALIAS),
        },
        // The function EXISTS and the version does not: the same sentence, naming the
        // version the request asked about.
        Case {
            label: "a version a held function has not published",
            reference: HELD.to_string(),
            qualifier: Some(MISSING_VERSION),
            function: HELD,
            named: Some(MISSING_VERSION),
        },
        Case {
            label: "a version a held function has not published, named inside FunctionName",
            reference: format!("{HELD}:{MISSING_VERSION}"),
            qualifier: None,
            function: HELD,
            named: Some(MISSING_VERSION),
        },
        Case {
            label: "an alias a held function does not have",
            reference: HELD.to_string(),
            qualifier: Some(MISSING_ALIAS),
            function: HELD,
            named: Some(MISSING_ALIAS),
        },
        Case {
            label: "an alias a held function does not have, through its full ARN",
            reference: format!("{}:{MISSING_ALIAS}", function_arn(HELD)),
            qualifier: None,
            function: HELD,
            named: Some(MISSING_ALIAS),
        },
    ]
}

#[tokio::test]
async fn a_lookup_that_resolves_to_nothing_names_the_qualified_resource() {
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = router_on(&engine);
    create_held(&router).await;

    for case in refusal_cases() {
        let expected = format!(
            "{NOT_FOUND_PREFIX}{}",
            addressed_arn(case.function, case.named)
        );
        // Both reads resolve the same pair through the same resolution, so both state the
        // same resource for it.
        for (operation, answered) in [
            (
                "GetFunction",
                get_function(&router, &case.reference, case.qualifier).await,
            ),
            (
                "GetFunctionConfiguration",
                get_configuration(&router, &case.reference, case.qualifier).await,
            ),
        ] {
            assert_eq!(
                answered.status,
                404,
                "case {:?} ({operation}): expected the modelled 404, got {} {}",
                case.label,
                answered.status,
                answered.text()
            );
            assert_eq!(
                answered.error_code(),
                NOT_FOUND_CODE,
                "case {:?} ({operation}): expected {NOT_FOUND_CODE}, got {}",
                case.label,
                answered.text()
            );
            assert_eq!(
                answered.message(),
                expected,
                "case {:?} ({operation}): expected {expected:?}, got {:?}",
                case.label,
                answered.message()
            );
        }
    }

    // The control: every read that DOES resolve is answered, after all of the refusals
    // above — the function, each published version, the unpublished version named
    // explicitly, and the alias the function holds.
    for (label, qualifier) in [
        ("the function itself", None),
        ("the unpublished version named explicitly", Some(LATEST)),
        ("a published version", Some(HELD_VERSION)),
        ("the alias it holds", Some(HELD_ALIAS)),
    ] {
        for (operation, answered) in [
            ("GetFunction", get_function(&router, HELD, qualifier).await),
            (
                "GetFunctionConfiguration",
                get_configuration(&router, HELD, qualifier).await,
            ),
        ] {
            assert_eq!(
                answered.status,
                200,
                "case {label:?} ({operation}): a read that resolves must be answered, got {} {}",
                answered.status,
                answered.text()
            );
        }
    }
}
