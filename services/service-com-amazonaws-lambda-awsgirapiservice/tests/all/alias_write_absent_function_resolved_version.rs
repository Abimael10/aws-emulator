// SPDX-License-Identifier: CC0-1.0

//! The ARN an alias WRITE names when the function it has to resolve a version of is not
//! there.
//!
//! The rule under test: an alias write resolves the function version its request names
//! before it touches the alias, so the resource it could not resolve is the function AT that
//! version — the modelled `ResourceNotFoundException` (HTTP 404) whose message is the fixed
//! `Function not found: ` followed by the function's own ARN qualified by exactly the version
//! the request named. That spelling does not depend on WHY the version could not be
//! resolved: a function this service holds without that version
//! (`crate::alias_version_not_found_sentence` pins that leg) and a function this service
//! does not hold at all are refused with the same ARN, because a function that is not there
//! has the requested version no more than one that is there without it. A write that names
//! NO version resolves the unpublished version, which this service spells as the unqualified
//! function ARN, so that request keeps the function's own ARN.
//!
//! The rows differ along what the rule generalises over: the version named (the first
//! published number, a far number, zero, the largest 64-bit number, one wider than 64 bits,
//! the unpublished version, and no version member at all), the operation that resolves it
//! (`CreateAlias`, whose `FunctionVersion` is required, and `UpdateAlias`, whose is
//! optional), the documented `FunctionName` form the request addresses the function by (bare
//! name, function ARN, partial ARN), and how the function came to be absent (never created,
//! or created, published, aliased and then deleted). `GetFunction` with the same
//! `Qualifier` on the same absent function is asserted beside each write, because both name
//! the version the call resolved and so must name one ARN.

use std::sync::Arc;

use serde_json::{json, Value};
use service_com_amazonaws_lambda_awsgirapiservice::state::ComAmazonawsLambdaAwsgirapiserviceState;

use crate::alias_update::{
    call, create_alias, create_function, get_alias, router_on, update_alias, Answer, ACCOUNT,
    FUNCTIONS_URI, LATEST, REGION,
};

/// The error code the model declares, at HTTP 404, for a resource an alias write cannot
/// find: `smithy_query(query="operation", operation="UpdateAlias")` lists
/// `ResourceNotFoundException` — "The resource specified in the request does not exist."
const RESOURCE_NOT_FOUND: &str = "ResourceNotFoundException";

/// The fixed lead-in of the sentence, which the ARN the call resolved follows.
const NOT_FOUND_PREFIX: &str = "Function not found: ";

/// How many versions the function that is later deleted publishes.
const PUBLISHED_VERSIONS: usize = 2;

/// The unqualified ARN a function of `name` carries in this suite's Region and account.
fn function_arn(name: &str) -> String {
    format!("arn:aws:lambda:{REGION}:{ACCOUNT}:function:{name}")
}

/// The ARN a call that resolved `version` of `function` names: the function's own ARN with
/// the version appended, and the unqualified ARN for the unpublished version — the one
/// spelling this service gives the version a call resolved.
fn resolved_arn(function: &str, version: Option<&str>) -> String {
    match version {
        Some(version) if version != LATEST => format!("{}:{version}", function_arn(function)),
        _ => function_arn(function),
    }
}

/// Version values an alias write can name, spanning the class the member admits: the
/// `FunctionVersion` of `CreateAlias`/`UpdateAlias` is `VersionWithLatestPublished`, pattern
/// `^(\$LATEST(\.PUBLISHED)?|[0-9]+)$` (from `smithy_query(query="shape",
/// shape="com.amazonaws.lambda#VersionWithLatestPublished")`), so the digits and the
/// unpublished label are what a request can carry here. `None` is a request that names no
/// version at all, which only `UpdateAlias` admits (`CreateAlias` requires the member).
fn resolved_versions() -> Vec<(String, Option<String>)> {
    vec![
        (
            "the first published version number".to_string(),
            Some("1".to_string()),
        ),
        (
            "a version number far past any publish".to_string(),
            Some("97".to_string()),
        ),
        (
            "zero, which no publish ever mints".to_string(),
            Some("0".to_string()),
        ),
        (
            "the largest 64-bit version number".to_string(),
            Some("18446744073709551615".to_string()),
        ),
        (
            "a version number wider than 64 bits".to_string(),
            Some("999999999999999999999999999999".to_string()),
        ),
        (
            "the unpublished version".to_string(),
            Some(LATEST.to_string()),
        ),
        ("no version member at all".to_string(), None),
    ]
}

/// The documented `FunctionName` forms an alias write addresses a function by ("Function
/// name", "Function ARN", "Partial ARN", from `smithy_query(query="doc",
/// shape="com.amazonaws.lambda#UpdateAliasRequest", member="FunctionName")`). The ARN in the
/// refusal is the function's own, whichever form the request used.
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

/// Assert one answer is the modelled 404 naming `version` of `function`.
fn assert_resolved_refusal(case: &str, function: &str, version: Option<&str>, answer: &Answer) {
    assert_eq!(
        (answer.status, answer.error_code()),
        (404, RESOURCE_NOT_FOUND.to_string()),
        "case {case:?}: expected 404 {RESOURCE_NOT_FOUND}, got {} {}",
        answer.status,
        answer.text()
    );
    let expected = format!("{NOT_FOUND_PREFIX}{}", resolved_arn(function, version));
    assert_eq!(
        answer.message(),
        expected,
        "case {case:?}: expected {expected:?}, got {:?}",
        answer.message()
    );
}

/// `GetFunction` on `function`, qualified by `version` when the request names one: the read
/// whose refusal already names the version it resolved, and which every write resolving the
/// same version has to agree with.
async fn get_function(
    router: &smithy_server_runtime::MultiServiceRouter,
    function: &str,
    version: Option<&str>,
) -> Answer {
    let uri = match version {
        Some(version) => format!("{FUNCTIONS_URI}/{function}?Qualifier={version}"),
        None => format!("{FUNCTIONS_URI}/{function}"),
    };
    call(router, "GET", &uri, &Value::Null).await
}

/// `DeleteFunction` with no `Qualifier`, which takes the function's versions and aliases
/// with it.
async fn delete_function(
    router: &smithy_server_runtime::MultiServiceRouter,
    function: &str,
) -> Answer {
    call(
        router,
        "DELETE",
        &format!("{FUNCTIONS_URI}/{function}"),
        &Value::Null,
    )
    .await
}

/// RULE: `UpdateAlias` on a function this service does not hold names the function's ARN at
/// the version the request asked it to resolve, and the unqualified ARN when it asked for
/// none.
#[tokio::test]
async fn update_alias_on_an_absent_function_names_the_version_it_resolved() {
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = router_on(&engine);
    let function = "aw-never-created";

    for (label, version) in resolved_versions() {
        for (addressed, reference) in references(function) {
            let case = format!("{label} addressed by {addressed}");
            let body = match version.as_deref() {
                Some(version) => json!({ "FunctionVersion": version }),
                None => json!({ "Description": "retarget nothing" }),
            };
            let answer = update_alias(&router, &reference, "live", &body).await;
            assert_resolved_refusal(&case, function, version.as_deref(), &answer);

            // The read of the same version of the same absent function names the same ARN,
            // which is what pins the spelling: both calls resolved the same resource.
            let read = get_function(&router, &reference, version.as_deref()).await;
            assert_resolved_refusal(&format!("{case}, read back"), function, version.as_deref(), &read);
        }
    }

    // The refused writes created nothing: no function, and so no alias either.
    let state = engine.state::<ComAmazonawsLambdaAwsgirapiserviceState>();
    let store = state.lock();
    assert!(
        store.functions.is_empty(),
        "a refused alias write must create no function, got {:?}",
        store.functions.keys().collect::<Vec<_>>()
    );
}

/// RULE: `CreateAlias`'s `FunctionVersion` is required, so every refusal of it on an absent
/// function names a version — the same ARN the update names for the same pair.
#[tokio::test]
async fn create_alias_on_an_absent_function_names_the_version_it_resolved() {
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = router_on(&engine);
    let function = "aw-create-absent";

    for (label, version) in resolved_versions() {
        let Some(version) = version else {
            // `CreateAlias` requires the member, so "no version at all" is not a request it
            // admits; the update covers that row.
            continue;
        };
        for (addressed, reference) in references(function) {
            let case = format!("{label} addressed by {addressed}");
            let answer = create_alias(
                &router,
                &reference,
                &json!({ "Name": "live", "FunctionVersion": version }),
            )
            .await;
            assert_resolved_refusal(&case, function, Some(&version), &answer);
        }
    }

    // And the control: once the function exists, a version it holds is served through the
    // same labels, so the rows above are refused for the resolution rather than wholesale.
    create_function(&router, function, PUBLISHED_VERSIONS).await;
    let created = create_alias(
        &router,
        function,
        &json!({ "Name": "live", "FunctionVersion": "1" }),
    )
    .await;
    assert_eq!(
        (created.status, created.string("FunctionVersion")),
        (201, Some("1".to_string())),
        "a version the function holds must be served, got {} {}",
        created.status,
        created.text()
    );
    let read = get_alias(&router, function, "live").await;
    assert_eq!(
        (read.status, read.string("AliasArn")),
        (200, Some(format!("{}:live", function_arn(function)))),
        "GetAlias must report the alias the create stored, got {} {}",
        read.status,
        read.text()
    );
}

/// RULE: the spelling is about what the call could resolve, not about how the function came
/// not to be there — a function that was created, published, aliased and then deleted is
/// refused exactly as one that never existed.
#[tokio::test]
async fn an_alias_write_after_the_function_was_deleted_names_the_version_it_resolved() {
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = router_on(&engine);
    let function = "aw-deleted";

    create_function(&router, function, PUBLISHED_VERSIONS).await;
    let created = create_alias(
        &router,
        function,
        &json!({ "Name": "live", "FunctionVersion": "1" }),
    )
    .await;
    assert_eq!(
        created.status,
        201,
        "the alias the delete takes with it must be created, got {} {}",
        created.status,
        created.text()
    );
    // The control: while the function is there, the version the rows name is resolvable and
    // the update is served.
    let served = update_alias(
        &router,
        function,
        "live",
        &json!({ "FunctionVersion": "2" }),
    )
    .await;
    assert_eq!(
        (served.status, served.string("FunctionVersion")),
        (200, Some("2".to_string())),
        "the update must be served while the function is there, got {} {}",
        served.status,
        served.text()
    );

    let deleted = delete_function(&router, function).await;
    assert_eq!(
        deleted.status,
        204,
        "DeleteFunction must succeed, got {} {}",
        deleted.status,
        deleted.text()
    );

    for (label, version) in resolved_versions() {
        let case = format!("{label} after the function was deleted");
        let body = match version.as_deref() {
            Some(version) => json!({ "FunctionVersion": version }),
            None => json!({ "Description": "retarget nothing" }),
        };
        let update = update_alias(&router, function, "live", &body).await;
        assert_resolved_refusal(&case, function, version.as_deref(), &update);

        // The versions the function DID publish are gone with it, so a number that used to
        // resolve is refused as the absent version too — and the read agrees.
        let read = get_function(&router, function, version.as_deref()).await;
        assert_resolved_refusal(&format!("{case}, read back"), function, version.as_deref(), &read);

        if let Some(version) = version.as_deref() {
            let create = create_alias(
                &router,
                function,
                &json!({ "Name": "live", "FunctionVersion": version }),
            )
            .await;
            assert_resolved_refusal(&format!("{case}, created"), function, Some(version), &create);
        }
    }

    let state = engine.state::<ComAmazonawsLambdaAwsgirapiserviceState>();
    let store = state.lock();
    assert!(
        store.functions.is_empty(),
        "a refused alias write must not resurrect the deleted function, got {:?}",
        store.functions.keys().collect::<Vec<_>>()
    );
}
