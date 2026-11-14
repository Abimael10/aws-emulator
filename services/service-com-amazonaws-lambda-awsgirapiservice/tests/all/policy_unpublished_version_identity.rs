// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for WHICH identity of a function carries its resource-based policy.
//!
//! The RULE asserted here: a function's resource-based policy belongs to the function
//! itself, to a published version or to an alias — "You can apply the policy at the
//! function level, or specify a qualifier to restrict access to a single version or
//! alias. [...] Note: Lambda does not support adding policies to version $LATEST"
//! (`AddPermission`), and `GetPolicy` "returns the resource-based IAM policy for a
//! function, version, or alias". The unpublished version named EXPLICITLY is none of the
//! three: a grant on it is refused `InvalidParameterValueException` (400), so that
//! identity holds no document however the unqualified function is granted — a read of it
//! is `ResourceNotFoundException` (404) with the service's fixed not-found sentence, and a
//! removal on it reports no policy to act on. A client reading a policy to decide whether
//! an endpoint qualified with the unpublished version is authorized therefore gets the
//! same answer the account gives: that grant cannot exist.
//!
//! The rows differ along what the rule generalises over: WHICH documented form of the
//! identifier names the unpublished version — the `Qualifier` query member, the qualifier
//! appended to a bare name, to the `function:`-prefixed name, to a partial ARN and to a
//! full function ARN, and both halves naming it at once — and, in the control table, which
//! policy-bearing identity is addressed instead (the function itself, a published version,
//! an alias) through the same two naming forms.
//!
//! Every call goes over the wire through the router the emulator serves in production,
//! sharing the harness in `function_lifecycle.rs`, so nothing but persisted state carries
//! information between calls.

use std::sync::Arc;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::function_lifecycle::{
    create_active_function, function_arn, json_call, package_bytes, router_on, Answer, ACCOUNT,
    REGION,
};

/// Where the policy operations of a function are bound.
const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// The unpublished version, as `Version` spells it (`^(\$LATEST|[0-9]+)$`).
const VERSION_LATEST: &str = "$LATEST";

/// The one sentence a policy READ that found no document carries, repeating nothing of the
/// request. Spelled out here rather than read from the crate's own constant, so a change to
/// that constant fails this suite.
const NO_DOCUMENT: &str = "The resource you requested does not exist.";

/// The one sentence a policy REMOVAL with no document to act on carries.
const NO_POLICY: &str = "No policy is associated with the given resource.";

/// The sentence a grant on the unpublished version is refused with: "Lambda does not
/// support adding policies to version $LATEST" as the service words the refusal.
const NO_GRANT_ON_LATEST: &str = "We currently do not support adding policies for $LATEST.";

/// The action every grant below names.
const INVOKE_ACTION: &str = "lambda:InvokeFunction";

/// The body text of an answer, for a failure message.
fn text(answer: &Answer) -> String {
    String::from_utf8_lossy(&answer.body).to_string()
}

/// The `Message` of a wire error, however the protocol spells the member.
fn message(answer: &Answer) -> String {
    answer
        .string("Message")
        .or_else(|| answer.string("message"))
        .unwrap_or_default()
}

/// `GetPolicy` on a reference, optionally carrying a `Qualifier`.
async fn get_policy(
    router: &MultiServiceRouter,
    reference: &str,
    qualifier: Option<&str>,
) -> Answer {
    let query = qualifier.map_or_else(String::new, |value| format!("?Qualifier={value}"));
    let uri = format!("{FUNCTIONS_URI}/{reference}/policy{query}");
    json_call(router, "GET", &uri, &Value::Null).await
}

/// `AddPermission` on a reference, optionally carrying a `Qualifier`.
async fn add_permission(
    router: &MultiServiceRouter,
    reference: &str,
    qualifier: Option<&str>,
    statement_id: &str,
) -> Answer {
    let query = qualifier.map_or_else(String::new, |value| format!("?Qualifier={value}"));
    let uri = format!("{FUNCTIONS_URI}/{reference}/policy{query}");
    let body = json!({
        "StatementId": statement_id,
        "Action": INVOKE_ACTION,
        "Principal": "sns.amazonaws.com",
    });
    json_call(router, "POST", &uri, &body).await
}

/// `RemovePermission` of one statement id on a reference, optionally carrying a
/// `Qualifier`.
async fn remove_permission(
    router: &MultiServiceRouter,
    reference: &str,
    qualifier: Option<&str>,
    statement_id: &str,
) -> Answer {
    let query = qualifier.map_or_else(String::new, |value| format!("?Qualifier={value}"));
    let uri = format!("{FUNCTIONS_URI}/{reference}/policy/{statement_id}{query}");
    json_call(router, "DELETE", &uri, &Value::Null).await
}

/// `PublishVersion`, which mints version 1 of a function nothing has published yet.
async fn publish_version(router: &MultiServiceRouter, name: &str) {
    let uri = format!("{FUNCTIONS_URI}/{name}/versions");
    let answer = json_call(router, "POST", &uri, &json!({})).await;
    assert_eq!(
        answer.status,
        201,
        "publishing a version of {name:?} must succeed, got {} {}",
        answer.status,
        text(&answer)
    );
}

/// `CreateAlias` naming one published version.
async fn create_alias(router: &MultiServiceRouter, name: &str, alias: &str, version: &str) {
    let uri = format!("{FUNCTIONS_URI}/{name}/aliases");
    let answer = json_call(
        router,
        "POST",
        &uri,
        &json!({ "Name": alias, "FunctionVersion": version }),
    )
    .await;
    assert_eq!(
        answer.status,
        201,
        "creating the alias {alias:?} must succeed, got {} {}",
        answer.status,
        text(&answer)
    );
}

/// The statement ids a reported policy document holds, in order.
fn statement_ids(case: &str, answer: &Answer) -> Vec<String> {
    let document: Value = serde_json::from_str(
        &answer
            .string("Policy")
            .unwrap_or_else(|| panic!("case {case:?}: no Policy member in {}", text(answer))),
    )
    .unwrap_or_else(|error| panic!("case {case:?}: the Policy must be JSON: {error}"));
    document["Statement"]
        .as_array()
        .unwrap_or_else(|| panic!("case {case:?}: Statement must be an array, got {document}"))
        .iter()
        .map(|statement| statement["Sid"].as_str().unwrap_or_default().to_string())
        .collect()
}

/// One row: how the identifier names the unpublished version.
struct LatestForm {
    case: &'static str,
    /// The `FunctionName` label, built from the function's own name.
    reference: fn(&str) -> String,
    /// The `Qualifier` query member, when the row carries one.
    qualifier: Option<&'static str>,
}

/// Every documented form of `NamespacedFunctionName` that can name the unpublished
/// version, plus the `Qualifier` member and both halves naming it at once.
///
/// The forms are the member's own ("Function name - my-function. Function ARN -
/// arn:aws:lambda:us-west-2:123456789012:function:my-function. Partial ARN -
/// 123456789012:function:my-function. You can append a version number or alias to any of
/// the formats", `smithy_query(query="operation", operation="GetPolicy")`), plus the bare
/// `function:` prefix its pattern admits.
fn latest_forms() -> Vec<LatestForm> {
    vec![
        LatestForm {
            case: "the Qualifier member alone",
            reference: |name| name.to_string(),
            qualifier: Some(VERSION_LATEST),
        },
        LatestForm {
            case: "appended to the bare name",
            reference: |name| format!("{name}:{VERSION_LATEST}"),
            qualifier: None,
        },
        LatestForm {
            case: "appended to the resource-type prefix form",
            reference: |name| format!("function:{name}:{VERSION_LATEST}"),
            qualifier: None,
        },
        LatestForm {
            case: "appended to a partial ARN",
            reference: |name| format!("{ACCOUNT}:function:{name}:{VERSION_LATEST}"),
            qualifier: None,
        },
        LatestForm {
            case: "appended to a function ARN",
            reference: |name| {
                format!("arn:aws:lambda:{REGION}:{ACCOUNT}:function:{name}:{VERSION_LATEST}")
            },
            qualifier: None,
        },
        LatestForm {
            case: "named in both halves of the request",
            reference: |name| format!("{name}:{VERSION_LATEST}"),
            qualifier: Some(VERSION_LATEST),
        },
    ]
}

/// RULE: the unpublished version named explicitly carries no resource-based policy — no
/// grant can be added to it, a read of its policy finds no document, and a removal finds
/// none to change — while the unqualified function it belongs to keeps the grant it was
/// given.
#[tokio::test]
async fn the_unpublished_version_carries_no_resource_policy() {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let router = router_on(&engine);
    create_active_function(&router, &engine, "granted", &package_bytes(64)).await;

    // The FUNCTION is granted, unqualified, which is the grant an SNS subscription to the
    // unqualified endpoint reads.
    let granted = add_permission(&router, "granted", None, "sns-invoke").await;
    assert_eq!(
        granted.status,
        201,
        "the function-level grant must succeed, got {} {}",
        granted.status,
        text(&granted)
    );
    let revision = get_policy(&router, "granted", None)
        .await
        .string("RevisionId")
        .expect("GetPolicy reports the policy's RevisionId");

    for form in latest_forms() {
        let reference = (form.reference)("granted");
        let case = form.case;

        // A grant on that identity is refused, with the sentence the service words for it.
        let refused = add_permission(&router, &reference, form.qualifier, "on-latest").await;
        assert_eq!(
            (refused.status, refused.error_code()),
            (400, "InvalidParameterValueException".to_string()),
            "case {case:?}: expected 400 InvalidParameterValueException from AddPermission, \
             got {} {}",
            refused.status,
            text(&refused)
        );
        assert_eq!(
            message(&refused),
            NO_GRANT_ON_LATEST,
            "case {case:?}: expected {NO_GRANT_ON_LATEST:?}, got {:?}",
            message(&refused)
        );

        // So a READ of its policy has no document to report, even though the function
        // itself is granted.
        let read = get_policy(&router, &reference, form.qualifier).await;
        assert_eq!(
            (read.status, read.error_code()),
            (404, "ResourceNotFoundException".to_string()),
            "case {case:?}: expected 404 ResourceNotFoundException from GetPolicy, got {} {}",
            read.status,
            text(&read)
        );
        assert_eq!(
            message(&read),
            NO_DOCUMENT,
            "case {case:?}: expected {NO_DOCUMENT:?}, got {:?}",
            message(&read)
        );

        // And a REMOVAL on it reports the policy it was asked to change as absent.
        let removed = remove_permission(&router, &reference, form.qualifier, "sns-invoke").await;
        assert_eq!(
            (removed.status, removed.error_code()),
            (404, "ResourceNotFoundException".to_string()),
            "case {case:?}: expected 404 ResourceNotFoundException from RemovePermission, got \
             {} {}",
            removed.status,
            text(&removed)
        );
        assert_eq!(
            message(&removed),
            NO_POLICY,
            "case {case:?}: expected {NO_POLICY:?}, got {:?}",
            message(&removed)
        );

        // None of the three touched the function's own policy: it still holds exactly the
        // statement it was granted, at the revision that granted it.
        let held = get_policy(&router, "granted", None).await;
        assert_eq!(
            held.status,
            200,
            "case {case:?}: the function-level policy must still be reported, got {} {}",
            held.status,
            text(&held)
        );
        assert_eq!(
            statement_ids(case, &held),
            vec!["sns-invoke".to_string()],
            "case {case:?}: expected the one granted statement, got {}",
            text(&held)
        );
        assert_eq!(
            held.string("RevisionId"),
            Some(revision.clone()),
            "case {case:?}: a refused call may not move the policy's revision, got {:?}",
            held.string("RevisionId")
        );
    }
}

/// One control row: a policy-bearing identity, and how the request names it.
struct BearingCase {
    case: &'static str,
    /// The qualifier the grant is made on, or `None` for the function itself.
    qualifier: Option<&'static str>,
    /// Whether the qualifier is written into the `FunctionName` rather than the
    /// `Qualifier` member.
    in_name: bool,
}

/// RULE (the other half): every identity a policy CAN belong to — the function, a
/// published version, an alias — reports the grant made on it, through either naming form.
/// The control that shows the refusals above are about the unpublished version rather than
/// about qualifiers at all.
#[tokio::test]
async fn a_function_a_version_and_an_alias_each_report_their_own_policy() {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let router = router_on(&engine);
    create_active_function(&router, &engine, "bearing", &package_bytes(64)).await;
    publish_version(&router, "bearing").await;
    create_alias(&router, "bearing", "live", "1").await;

    let rows = [
        BearingCase {
            case: "the function itself",
            qualifier: None,
            in_name: false,
        },
        BearingCase {
            case: "a published version in the Qualifier member",
            qualifier: Some("1"),
            in_name: false,
        },
        BearingCase {
            case: "a published version appended to the name",
            qualifier: Some("1"),
            in_name: true,
        },
        BearingCase {
            case: "an alias in the Qualifier member",
            qualifier: Some("live"),
            in_name: false,
        },
        BearingCase {
            case: "an alias appended to the name",
            qualifier: Some("live"),
            in_name: true,
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        let case = row.case;
        let (reference, qualifier) = match (row.qualifier, row.in_name) {
            (Some(qualifier), true) => (format!("bearing:{qualifier}"), None),
            (Some(qualifier), false) => ("bearing".to_string(), Some(qualifier)),
            (None, _) => ("bearing".to_string(), None),
        };
        // One statement id per row, so a row's grant is told apart from the grants the
        // rows before it made on the same identity.
        let statement_id = format!("grant-{index}");

        let granted = add_permission(&router, &reference, qualifier, &statement_id).await;
        assert_eq!(
            granted.status,
            201,
            "case {case:?}: the grant must succeed, got {} {}",
            granted.status,
            text(&granted)
        );

        // Read back through the identity it was granted on: the document holds that
        // statement, on the ARN of that very identity.
        let read = get_policy(&router, &reference, qualifier).await;
        assert_eq!(
            read.status,
            200,
            "case {case:?}: expected 200 from GetPolicy, got {} {}",
            read.status,
            text(&read)
        );
        let ids = statement_ids(case, &read);
        assert!(
            ids.contains(&statement_id),
            "case {case:?}: expected the granted statement {statement_id:?} among {ids:?}"
        );
        let document: Value = serde_json::from_str(&read.string("Policy").unwrap_or_default())
            .unwrap_or_else(|error| panic!("case {case:?}: the Policy must be JSON: {error}"));
        let expected_resource = row.qualifier.map_or_else(
            || function_arn("bearing"),
            |qualifier| format!("{}:{qualifier}", function_arn("bearing")),
        );
        let resources: Vec<String> = document["Statement"]
            .as_array()
            .unwrap_or_else(|| panic!("case {case:?}: Statement must be an array"))
            .iter()
            .filter(|statement| statement["Sid"].as_str() == Some(statement_id.as_str()))
            .map(|statement| {
                statement["Resource"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string()
            })
            .collect();
        assert_eq!(
            resources,
            vec![expected_resource.clone()],
            "case {case:?}: expected the grant on {expected_resource:?}, got {resources:?}"
        );

        // The unpublished version is still no part of it: the same function, named with
        // that qualifier, reports no document at all.
        let latest = get_policy(&router, "bearing", Some(VERSION_LATEST)).await;
        assert_eq!(
            (latest.status, message(&latest)),
            (404, NO_DOCUMENT.to_string()),
            "case {case:?}: expected 404 {NO_DOCUMENT:?} for the unpublished version, got {} {}",
            latest.status,
            text(&latest)
        );
    }
}
