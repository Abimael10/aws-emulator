// SPDX-License-Identifier: CC0-1.0

//! What a SYNCHRONOUS `Invoke` of a function that was DELETED says it could not find:
//! `POST /2015-03-31/functions/{FunctionName}/invocations`.
//!
//! The rule under test: **an unqualified `DeleteFunction` removes the function together with
//! every version and alias of it, and a synchronous invocation still RESOLVES a version to run
//! — `Qualifier` "specif[ies] a version or alias to invoke a published version of the function"
//! and an invocation naming none runs the unpublished version, which such an invocation reports
//! as its `ExecutedVersion` — so the modelled `ResourceNotFoundException` (HTTP 404,
//! `smithy_query(query="operation", operation="Invoke")`) names the ARN the request RESOLVED:
//! the fixed text `Function not found: ` followed by
//! `arn:aws:lambda:<region>:<account>:function:<name>:<qualifier>`, carrying the qualifier the
//! request named and `$LATEST` where it named none.** The fixed text is the whole message bar
//! that ARN; its variable parts — Region, account, function name and the resolved qualifier —
//! come from the request and the credential scope, so a caller that re-derives the identity it
//! invoked from the refusal reads back the version that was looked up.
//!
//! This suite holds the LIFECYCLE half of the rule: the function existed, was published,
//! aliased and then deleted, so every qualifier a row names once resolved to something. The
//! rows differ along what the rule generalises over: which qualifier is named (none, the
//! unpublished version named explicitly, a one-digit and a two-digit published version number,
//! the alias the function carried, an alias it never held), where that qualifier travels (the
//! `Qualifier` query parameter and the `name:qualifier` form of `FunctionName`), and the
//! documented `FunctionName` form the request addresses the function by (the bare name, the
//! full ARN, the partial ARN, the `function:` prefix form). Two controls keep the table honest:
//! before the delete the same invocations are not refused as not found, and the name created
//! again resolves once more while the alias it never regained still carries its own qualifier.
//!
//! (The never-created function is [`crate::invoke_refusal_resolved_version`]'s table, and the
//! `Event` invocation — handed on rather than run, so its refusal names the identifier the
//! request ADDRESSED and leaves it unqualified where the request named no qualifier — is
//! [`crate::invoke_addressed_function`]'s.)

use std::sync::Arc;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The ONE function/alias harness this test binary has.
use crate::alias_update::{
    call, create_alias, create_function, router_on, Answer, ACCOUNT, FUNCTIONS_URI, LATEST, REGION,
};

/// The function this suite creates, publishes versions of, aliases and then deletes.
const FUNCTION: &str = "invoke-after-delete";

/// How many versions it publishes before the delete, so a two-digit version number is among
/// the qualifiers a row can name.
const PUBLISHED_VERSIONS: usize = 10;

/// The alias it carries before the delete.
const ALIAS: &str = "live";

/// An alias the function never held, so a row names a qualifier that never resolved.
const ABSENT_ALIAS: &str = "never-pointed";

/// The fixed text of the sentence, which is the whole message bar the resource it names.
const NOT_FOUND_PREFIX: &str = "Function not found: ";

/// The error code the model declares for a resource an invocation cannot find.
const NOT_FOUND_CODE: &str = "ResourceNotFoundException";

/// The unqualified ARN of `name` in this endpoint's Region, as every emulator ARN spells one.
fn function_arn(name: &str) -> String {
    format!("arn:aws:lambda:{REGION}:{ACCOUNT}:function:{name}")
}

/// The URI `Invoke` is bound to for the `FunctionName` label `reference`, with the `Qualifier`
/// query parameter when the row names one there.
fn invocation_uri(reference: &str, qualifier: Option<&str>) -> String {
    let query = qualifier.map_or_else(String::new, |value| format!("?Qualifier={value}"));
    format!("{FUNCTIONS_URI}/{reference}/invocations{query}")
}

/// One synchronous `Invoke` — the default invocation type, the request carrying no
/// `X-Amz-Invocation-Type` header at all.
async fn invoke(
    router: &MultiServiceRouter,
    reference: &str,
    qualifier: Option<&str>,
) -> Answer {
    call(
        router,
        "POST",
        &invocation_uri(reference, qualifier),
        &json!({}),
    )
    .await
}

/// `DeleteFunction` on `reference`, carrying no qualifier: the form that removes the function
/// with every version and alias of it.
async fn delete_function(router: &MultiServiceRouter, reference: &str) -> Answer {
    call(
        router,
        "DELETE",
        &format!("{FUNCTIONS_URI}/{reference}"),
        &Value::Null,
    )
    .await
}

/// One row: how the request addresses the deleted function, where the qualifier travels, and
/// the version the refused ARN must be qualified by.
struct Row {
    /// What this row is, named in every assertion it makes.
    case: &'static str,
    /// The `FunctionName` path label.
    reference: String,
    /// The `Qualifier` query parameter the request carries, if any.
    qualifier: Option<&'static str>,
    /// The version the refused ARN must carry — the one the request named, and the unpublished
    /// version's own label where it named none, that being the version a synchronous
    /// invocation resolves.
    resolved: &'static str,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "the bare name with no qualifier at all",
            reference: FUNCTION.to_string(),
            qualifier: None,
            resolved: LATEST,
        },
        Row {
            case: "the unpublished version named explicitly in the Qualifier parameter",
            reference: FUNCTION.to_string(),
            qualifier: Some(LATEST),
            resolved: LATEST,
        },
        Row {
            case: "the first published version, in the Qualifier parameter",
            reference: FUNCTION.to_string(),
            qualifier: Some("1"),
            resolved: "1",
        },
        Row {
            case: "a two-digit published version, in the Qualifier parameter",
            reference: FUNCTION.to_string(),
            qualifier: Some("10"),
            resolved: "10",
        },
        Row {
            case: "a published version inside the FunctionName",
            reference: format!("{FUNCTION}:2"),
            qualifier: None,
            resolved: "2",
        },
        Row {
            case: "the alias the function carried",
            reference: FUNCTION.to_string(),
            qualifier: Some(ALIAS),
            resolved: ALIAS,
        },
        Row {
            case: "an alias the function never held",
            reference: format!("{FUNCTION}:{ABSENT_ALIAS}"),
            qualifier: None,
            resolved: ABSENT_ALIAS,
        },
        Row {
            case: "the full function ARN with no qualifier",
            reference: function_arn(FUNCTION),
            qualifier: None,
            resolved: LATEST,
        },
        Row {
            case: "the full function ARN qualified in the name",
            reference: format!("{}:3", function_arn(FUNCTION)),
            qualifier: None,
            resolved: "3",
        },
        Row {
            case: "the partial ARN with a published version in the Qualifier parameter",
            reference: format!("{ACCOUNT}:function:{FUNCTION}"),
            qualifier: Some("7"),
            resolved: "7",
        },
        Row {
            case: "the function: prefix form with no qualifier",
            reference: format!("function:{FUNCTION}"),
            qualifier: None,
            resolved: LATEST,
        },
    ]
}

/// Assert one answer is the modelled 404 naming the function at the version the invocation
/// resolved.
fn assert_resolved_refusal(case: &str, resolved: &str, answer: &Answer) {
    let expected = format!("{NOT_FOUND_PREFIX}{}:{resolved}", function_arn(FUNCTION));
    assert_eq!(
        answer.status,
        404,
        "case {case:?}: expected the modelled 404, got {} {}",
        answer.status,
        answer.text()
    );
    assert_eq!(
        answer.error_code(),
        NOT_FOUND_CODE,
        "case {case:?}: expected {NOT_FOUND_CODE}, got {} {}",
        answer.error_code(),
        answer.text()
    );
    assert_eq!(
        answer.message(),
        expected,
        "case {case:?}: expected {expected:?}, got {:?}",
        answer.message()
    );
}

/// RULE: a synchronous invocation of a function the delete removed names the ARN it resolved,
/// the unpublished version's own label included.
#[tokio::test]
async fn an_invocation_of_a_deleted_function_names_the_version_it_resolved() {
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
        "case \"the alias the delete removes\": CreateAlias must succeed, got {} {}",
        aliased.status,
        aliased.text()
    );

    // The CONTROL taken BEFORE the delete: while the function is held, none of these
    // invocations is refused as not found — so no row below passes against a handler that
    // refuses every invocation.
    for row in rows() {
        if row.resolved == ABSENT_ALIAS {
            // The one row whose qualifier never resolved: it is not found before the delete
            // either, and what it pins is that the sentence carries the qualifier the request
            // named rather than the one the function held.
            continue;
        }
        let answer = invoke(&router, &row.reference, row.qualifier).await;
        assert_ne!(
            answer.error_code(),
            NOT_FOUND_CODE,
            "case {:?}: an invocation of the held function must not be refused as not found, \
             got {} {}",
            row.case,
            answer.status,
            answer.text()
        );
    }

    let deleted = delete_function(&router, FUNCTION).await;
    assert_eq!(
        deleted.status,
        204,
        "case \"the unqualified delete\": DeleteFunction must succeed, got {} {}",
        deleted.status,
        deleted.text()
    );

    for row in rows() {
        let answer = invoke(&router, &row.reference, row.qualifier).await;
        assert_resolved_refusal(row.case, row.resolved, &answer);
    }

    // The CONTROL taken AFTER it: the same name created again resolves once more, and still
    // holds no alias — so the sentence tracks what resolves now, not what the name once was.
    create_function(&router, FUNCTION, 1).await;
    let present = invoke(&router, FUNCTION, None).await;
    assert_ne!(
        present.error_code(),
        NOT_FOUND_CODE,
        "case \"the recreated function with no qualifier\": the invocation must resolve, got {} \
         {}",
        present.status,
        present.text()
    );
    let regained = invoke(&router, FUNCTION, Some(ALIAS)).await;
    assert_resolved_refusal(
        "the recreated function at the alias it never regained",
        ALIAS,
        &regained,
    );
}
