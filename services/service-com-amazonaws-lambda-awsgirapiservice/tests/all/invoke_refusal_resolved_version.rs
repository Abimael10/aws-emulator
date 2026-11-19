// SPDX-License-Identifier: CC0-1.0

//! The VERSION an invocation's not-found refusal names, end to end over the wire.
//!
//! The rule under test: **an invocation resolves a VERSION of a function to run — `Qualifier`
//! "specif[ies] a version or alias to invoke a published version of the function" and an
//! invocation naming none runs the unpublished version, which such an invocation reports as
//! its `ExecutedVersion` — so the resource an invoking operation says it could not find is
//! always the function's ARN QUALIFIED by that version: `Function not found: ` followed by
//! `arn:aws:lambda:<region>:<account>:function:<name>:<qualifier>`, carrying the qualifier the
//! request named and `$LATEST` where it named none.** A LOOKUP of a function addresses the
//! function rather than a version it would run, so its own sentence leaves the unpublished
//! version unqualified — asserted here as the boundary of the rule.
//!
//! The rows differ along what the rule generalises over: the operation that invokes
//! (`Invoke` and `InvokeWithResponseStream`), the documented `FunctionName` form the request
//! addresses the function by (the bare name, the full ARN, the partial ARN, the `function:`
//! prefix form), where the qualifier is named (nowhere, the `Qualifier` parameter, the name
//! itself), which qualifier it is (none, the unpublished version named explicitly, a published
//! version number, an alias name) and what is missing (the function itself, or a version of a
//! function that does exist). Two controls keep the table honest: an invocation that RESOLVES
//! is not refused as not found, and the lookup of the same absent function carries the
//! unqualified sentence.

use std::sync::Arc;

use serde_json::json;
use smithy_server_runtime::MultiServiceRouter;

// The ONE function harness this test binary has.
use crate::alias_update::{
    call, create_function, router_on, Answer, ACCOUNT, FUNCTIONS_URI, LATEST, REGION,
};

/// The function this suite holds, with one published version, so a row can address a version
/// it does NOT have.
const HELD: &str = "resolved-version-held";

/// A name no function of this store is held under.
const ABSENT: &str = "resolved-version-absent";

/// The fixed text of the sentence, which is the whole message bar the resource it names.
const NOT_FOUND_PREFIX: &str = "Function not found: ";

/// The error code the model declares for a resource an invocation cannot find.
const NOT_FOUND_CODE: &str = "ResourceNotFoundException";

/// The unqualified ARN of `name` in this endpoint's Region, as every emulator ARN spells one.
fn function_arn(name: &str) -> String {
    format!("arn:aws:lambda:{REGION}:{ACCOUNT}:function:{name}")
}

/// Where each invoking operation is bound, for the `FunctionName` label `reference`:
/// `Invoke` is `POST /2015-03-31/functions/{FunctionName}/invocations` and
/// `InvokeWithResponseStream` is
/// `POST /2021-11-15/functions/{FunctionName}/response-streaming-invocations`.
fn invocation_uri(operation: &str, reference: &str, qualifier: Option<&str>) -> String {
    let query = qualifier.map_or_else(String::new, |value| format!("?Qualifier={value}"));
    match operation {
        "InvokeWithResponseStream" => format!(
            "/2021-11-15/functions/{reference}/response-streaming-invocations{query}"
        ),
        _ => format!("{FUNCTIONS_URI}/{reference}/invocations{query}"),
    }
}

/// Invoke `reference` through `operation`, with the `Qualifier` parameter `qualifier`.
async fn invoke(
    router: &MultiServiceRouter,
    operation: &str,
    reference: &str,
    qualifier: Option<&str>,
) -> Answer {
    call(
        router,
        "POST",
        &invocation_uri(operation, reference, qualifier),
        &json!({}),
    )
    .await
}

/// The two operations that INVOKE, which address the same resource and so word the same
/// refusal.
const INVOKING: [&str; 2] = ["Invoke", "InvokeWithResponseStream"];

/// One row: how the request addresses a function that cannot be resolved, and the qualifier the
/// refused ARN must carry.
struct Row {
    /// What this row is, named in every assertion it makes.
    case: &'static str,
    /// The `FunctionName` path label, built from the name of the function that EXISTS.
    reference: fn(&str) -> String,
    /// The `Qualifier` query parameter the request carries, if any.
    qualifier: Option<&'static str>,
    /// The function the refused ARN must name.
    function: &'static str,
    /// The version the refused ARN must be qualified by.
    version: &'static str,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a function that does not exist, addressed by the bare name",
            reference: |_| ABSENT.to_string(),
            qualifier: None,
            function: ABSENT,
            version: LATEST,
        },
        Row {
            case: "a full function ARN that names no function",
            reference: |_| format!("arn:aws:lambda:{REGION}:{ACCOUNT}:function:{ABSENT}"),
            qualifier: None,
            function: ABSENT,
            version: LATEST,
        },
        Row {
            case: "a partial ARN that names no function",
            reference: |_| format!("{ACCOUNT}:function:{ABSENT}"),
            qualifier: None,
            function: ABSENT,
            version: LATEST,
        },
        Row {
            case: "the function: prefix form naming no function",
            reference: |_| format!("function:{ABSENT}"),
            qualifier: None,
            function: ABSENT,
            version: LATEST,
        },
        Row {
            case: "a missing function whose name states the unpublished version",
            reference: |_| format!("{ABSENT}:{LATEST}"),
            qualifier: None,
            function: ABSENT,
            version: LATEST,
        },
        Row {
            case: "a missing function whose Qualifier states the unpublished version",
            reference: |_| ABSENT.to_string(),
            qualifier: Some(LATEST),
            function: ABSENT,
            version: LATEST,
        },
        Row {
            case: "a missing function at a published version named in the Qualifier",
            reference: |_| ABSENT.to_string(),
            qualifier: Some("7"),
            function: ABSENT,
            version: "7",
        },
        Row {
            case: "a missing function at an alias named in the name",
            reference: |_| format!("{ABSENT}:live"),
            qualifier: None,
            function: ABSENT,
            version: "live",
        },
        Row {
            case: "a version a function that exists has not published",
            reference: |held| held.to_string(),
            qualifier: Some("9"),
            function: HELD,
            version: "9",
        },
        Row {
            case: "an alias a function that exists does not hold",
            reference: |held| format!("{held}:absent-alias"),
            qualifier: None,
            function: HELD,
            version: "absent-alias",
        },
    ]
}

/// RULE: an invoking operation's `ResourceNotFoundException` names the function at the version
/// the invocation resolved, `$LATEST` included.
#[tokio::test]
async fn an_invocation_that_resolves_nothing_names_the_version_it_resolved() {
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = router_on(&engine);
    create_function(&router, HELD, 1).await;

    for row in rows() {
        let case = row.case;
        let reference = (row.reference)(HELD);
        let expected = format!(
            "{NOT_FOUND_PREFIX}{}:{}",
            function_arn(row.function),
            row.version
        );
        for operation in INVOKING {
            let answer = invoke(&router, operation, &reference, row.qualifier).await;
            assert_eq!(
                answer.status,
                404,
                "case {case:?} ({operation}): expected the modelled 404, got {} {}",
                answer.status,
                answer.text()
            );
            assert!(
                answer.error_code().contains(NOT_FOUND_CODE),
                "case {case:?} ({operation}): expected {NOT_FOUND_CODE}, got {}",
                answer.error_code()
            );
            assert_eq!(
                answer.message(),
                expected,
                "case {case:?} ({operation}): expected {expected:?}, got {:?}",
                answer.message()
            );
        }
    }
}

/// The CONTROL for the table: an invocation that RESOLVES is not refused as not found, in every
/// form the rows address a function by — so no row passes against a handler that refuses
/// everything.
#[tokio::test]
async fn an_invocation_that_resolves_is_not_refused_as_not_found() {
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = router_on(&engine);
    create_function(&router, HELD, 1).await;

    for (case, reference, qualifier) in [
        ("the bare name", HELD.to_string(), None),
        (
            "the full function ARN",
            format!("arn:aws:lambda:{REGION}:{ACCOUNT}:function:{HELD}"),
            None,
        ),
        ("the partial ARN", format!("{ACCOUNT}:function:{HELD}"), None),
        ("the unpublished version", HELD.to_string(), Some(LATEST)),
        ("a published version", HELD.to_string(), Some("1")),
        ("a version named in the name", format!("{HELD}:1"), None),
    ] {
        let answer = invoke(&router, "Invoke", &reference, qualifier).await;
        assert!(
            !answer.error_code().contains(NOT_FOUND_CODE),
            "case {case:?}: an invocation that resolves must not be refused as not found, got \
             {} {}",
            answer.status,
            answer.text()
        );
    }
}

/// The BOUNDARY of the rule: a LOOKUP addresses the function rather than a version to run, so
/// its own not-found sentence leaves the unpublished version unqualified — the same absent
/// function, named two ways by the two kinds of call.
#[tokio::test]
async fn a_lookup_of_the_same_absent_function_leaves_the_unpublished_version_unqualified() {
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = router_on(&engine);

    for (case, uri, expected) in [
        (
            "GetFunction with no qualifier",
            format!("{FUNCTIONS_URI}/{ABSENT}"),
            format!("{NOT_FOUND_PREFIX}{}", function_arn(ABSENT)),
        ),
        (
            "GetFunctionConfiguration with no qualifier",
            format!("{FUNCTIONS_URI}/{ABSENT}/configuration"),
            format!("{NOT_FOUND_PREFIX}{}", function_arn(ABSENT)),
        ),
        (
            "GetFunction at a published version",
            format!("{FUNCTIONS_URI}/{ABSENT}?Qualifier=3"),
            format!("{NOT_FOUND_PREFIX}{}:3", function_arn(ABSENT)),
        ),
    ] {
        let answer = call(&router, "GET", &uri, &serde_json::Value::Null).await;
        assert_eq!(
            answer.status,
            404,
            "case {case:?}: expected the modelled 404, got {} {}",
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
}
