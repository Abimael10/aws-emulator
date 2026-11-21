// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for WHEN a function's `RevisionId` advances, and what a precondition
//! carrying an earlier one is answered with.
//!
//! RULE: a `RevisionId` names ONE state of a Lambda function, so it advances at every call
//! that changes which state that is — an update to the function's code or configuration,
//! and publishing a version, which leaves the function holding a version it did not hold
//! before. A request whose `RevisionId` is not the function's current one is refused
//! `PreconditionFailedException` (HTTP 412, `Type` `User`) with the fixed sentence naming
//! the reads that report the current revision, and nothing is written; the current one is
//! served.
//!
//! The rows differ along what the rule generalises over: the CALL that advances the
//! revision (an update of the configuration, an update of the code, a publish), the
//! operation that CARRIES the precondition (`PublishVersion`,
//! `UpdateFunctionConfiguration`, `UpdateFunctionCode`), and which identity's revision is
//! supplied (the function's own before the call, and a published version's).
//!
//! Nothing here sleeps: the clock-owed `Pending` -> `Active` and `InProgress` ->
//! `Successful` moves are brought forward by moving the stored due instants into the past.

use std::sync::Arc;

use serde_json::{json, Value};

use crate::function_lifecycle::{
    create_function, json_call, make_activation_due, make_update_due, package_bytes, router_on,
    zip_file, Answer, ROLE,
};

/// Where the function operations are bound.
const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// The refusal a `RevisionId` precondition that did not hold gets on every operation
/// taking one for a function's configuration. Source: the service's own
/// `PreconditionFailedException` wording for this branch
/// (`smithy_query(query="doc", shape="PreconditionFailedException")`: "For all other API
/// operations: Call GetFunction or GetAlias to retrieve the latest RevisionId").
const PRECONDITION_MESSAGE: &str = "The Revision Id provided does not match the latest \
                                    Revision Id. Call the GetFunction/GetAlias API to \
                                    retrieve the latest Revision Id";

/// The `Type` a caller-caused refusal carries.
const USER_FAULT: &str = "User";

/// `CreateFunction` for one row, then the activation the clock owes, so the row starts
/// from the settled function a caller that waited would see.
async fn created_function(
    router: &smithy_server_runtime::MultiServiceRouter,
    engine: &Arc<engine::InMemoryEngine>,
    name: &str,
) -> String {
    let answer = create_function(
        router,
        &json!({
            "FunctionName": name,
            "Role": ROLE,
            "Runtime": "python3.12",
            "Handler": "index.handler",
            "Timeout": 7,
            "MemorySize": 256,
            "Code": { "ZipFile": zip_file(&package_bytes(64)) },
        }),
    )
    .await;
    assert_eq!(
        answer.status,
        201,
        "case {name:?}: expected 201 from CreateFunction, got {} {}",
        answer.status,
        String::from_utf8_lossy(&answer.body)
    );
    make_activation_due(engine);
    read_revision(router, name).await
}

/// The `RevisionId` a `FunctionConfiguration` answer carries.
fn answered_revision(case: &str, where_from: &str, answer: &Answer) -> String {
    answer.string("RevisionId").unwrap_or_else(|| {
        panic!(
            "case {case:?}: {where_from} must report a RevisionId, got {}",
            String::from_utf8_lossy(&answer.body)
        )
    })
}

/// The function's CURRENT unpublished revision, read back through
/// `GetFunctionConfiguration`.
async fn read_revision(
    router: &smithy_server_runtime::MultiServiceRouter,
    name: &str,
) -> String {
    let uri = format!("{FUNCTIONS_URI}/{name}/configuration");
    let answer = json_call(router, "GET", &uri, &Value::Null).await;
    assert_eq!(
        answer.status,
        200,
        "case {name:?}: expected 200 from GetFunctionConfiguration, got {} {}",
        answer.status,
        String::from_utf8_lossy(&answer.body)
    );
    answered_revision(name, "GetFunctionConfiguration", &answer)
}

/// `PublishVersion`, optionally carrying a `RevisionId` precondition.
async fn publish_version(
    router: &smithy_server_runtime::MultiServiceRouter,
    name: &str,
    revision: Option<&str>,
) -> Answer {
    let uri = format!("{FUNCTIONS_URI}/{name}/versions");
    let body = match revision {
        Some(revision) => json!({ "RevisionId": revision }),
        None => json!({}),
    };
    json_call(router, "POST", &uri, &body).await
}

/// The numbered versions `ListVersionsByFunction` reports, which is what says whether a
/// refused publish wrote anything.
async fn published_versions(
    router: &smithy_server_runtime::MultiServiceRouter,
    name: &str,
) -> Vec<String> {
    let uri = format!("{FUNCTIONS_URI}/{name}/versions");
    let answer = json_call(router, "GET", &uri, &Value::Null).await;
    assert_eq!(
        answer.status,
        200,
        "case {name:?}: expected 200 from ListVersionsByFunction, got {} {}",
        answer.status,
        String::from_utf8_lossy(&answer.body)
    );
    let body = answer.json();
    let entries = body["Versions"].as_array().cloned().unwrap_or_default();
    entries
        .iter()
        .filter_map(|entry| entry.get("Version").and_then(Value::as_str))
        .filter(|version| *version != "$LATEST")
        .map(str::to_string)
        .collect()
}

/// Assert `answer` is the modelled refusal of a precondition that did not hold.
fn assert_precondition_refusal(case: &str, which: &str, answer: &Answer) {
    assert_eq!(
        answer.status,
        412,
        "case {case:?}: {which} with a revision that is not the current one must answer \
         412, got {} {}",
        answer.status,
        String::from_utf8_lossy(&answer.body)
    );
    assert!(
        answer.error_code().contains("PreconditionFailedException"),
        "case {case:?}: {which} must answer PreconditionFailedException, got {}",
        answer.error_code()
    );
    assert_eq!(
        answer.string("message").as_deref(),
        Some(PRECONDITION_MESSAGE),
        "case {case:?}: {which} must state the revision-mismatch sentence, got {:?}",
        answer.string("message")
    );
    assert_eq!(
        answer.string("Type").as_deref(),
        Some(USER_FAULT),
        "case {case:?}: {which} must carry the User fault type, got {:?}",
        answer.string("Type")
    );
}

/// One row of the update table: the operation that opens an update window, and the
/// request that drives it.
struct UpdateCase {
    /// Names the row, and names its function, so rows never share state.
    case: &'static str,
    /// The method and URI suffix of the operation under test.
    method: &'static str,
    suffix: &'static str,
    /// Its request body.
    body: Value,
}

/// The operations that mutate a function and open an update window: one replaces
/// settings, the other replaces the deployment package.
fn update_cases() -> Vec<UpdateCase> {
    vec![
        UpdateCase {
            case: "update-function-configuration",
            method: "PUT",
            suffix: "configuration",
            body: json!({ "Timeout": 11 }),
        },
        UpdateCase {
            case: "update-function-code",
            method: "PUT",
            suffix: "code",
            body: json!({ "ZipFile": zip_file(&package_bytes(96)) }),
        },
    ]
}

#[tokio::test]
async fn an_accepted_update_advances_the_revision_and_strands_the_one_before_it() {
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = router_on(&engine);

    for case in update_cases() {
        let name = case.case;
        let created = created_function(&router, &engine, name).await;

        let uri = format!("{FUNCTIONS_URI}/{name}/{}", case.suffix);
        let accepted = json_call(&router, case.method, &uri, &case.body).await;
        assert_eq!(
            accepted.status,
            200,
            "case {name:?}: expected 200 from the update, got {} {}",
            accepted.status,
            String::from_utf8_lossy(&accepted.body)
        );
        let updated = answered_revision(name, "the update's own answer", &accepted);
        assert_ne!(
            updated, created,
            "case {name:?}: accepting an update must advance the revision, got {updated:?} \
             for both"
        );
        // A read reports, it does not advance: every read of the updated function answers
        // the revision the update left it at.
        assert_eq!(
            read_revision(&router, name).await,
            updated,
            "case {name:?}: a read must report the revision the update answered with"
        );
        make_update_due(&engine);
        let current = read_revision(&router, name).await;

        // The revision from before the update is stale, and the refusal writes nothing.
        let refused = publish_version(&router, name, Some(&created)).await;
        assert_precondition_refusal(name, "PublishVersion with the pre-update revision", &refused);
        let held = published_versions(&router, name).await;
        assert!(
            held.is_empty(),
            "case {name:?}: a refused PublishVersion must publish nothing, got {held:?}"
        );

        // The current one is served.
        let published = publish_version(&router, name, Some(&current)).await;
        assert_eq!(
            published.status,
            201,
            "case {name:?}: expected 201 publishing at the current revision, got {} {}",
            published.status,
            String::from_utf8_lossy(&published.body)
        );
        assert_eq!(
            published.json()["Version"],
            json!("1"),
            "case {name:?}: expected version 1, got {:?}",
            published.json()["Version"]
        );
    }
}

/// One row of the precondition table: an operation that takes a `RevisionId` for a
/// function's configuration, and what it answers at the current revision.
struct PreconditionCase {
    /// Names the row, and names its function.
    case: &'static str,
    /// The method and URI suffix of the operation.
    method: &'static str,
    suffix: &'static str,
    /// Its request body, which the revision under test is added to.
    body: Value,
    /// The status it answers when the precondition holds.
    served: u16,
}

/// Every operation that takes a `RevisionId` precondition on a function's configuration.
fn precondition_cases() -> Vec<PreconditionCase> {
    vec![
        PreconditionCase {
            case: "publish-version",
            method: "POST",
            suffix: "versions",
            body: json!({ "Description": "a second snapshot" }),
            served: 201,
        },
        PreconditionCase {
            case: "update-function-configuration",
            method: "PUT",
            suffix: "configuration",
            body: json!({ "Timeout": 13 }),
            served: 200,
        },
        PreconditionCase {
            case: "update-function-code",
            method: "PUT",
            suffix: "code",
            body: json!({ "ZipFile": zip_file(&package_bytes(128)) }),
            served: 200,
        },
    ]
}

#[tokio::test]
async fn publishing_a_version_advances_the_unpublished_revision() {
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = router_on(&engine);

    for case in precondition_cases() {
        let name = case.case;
        let created = created_function(&router, &engine, name).await;

        let published = publish_version(&router, name, Some(&created)).await;
        assert_eq!(
            published.status,
            201,
            "case {name:?}: expected 201 from PublishVersion, got {} {}",
            published.status,
            String::from_utf8_lossy(&published.body)
        );
        let version_revision = answered_revision(name, "the published version", &published);
        let current = read_revision(&router, name).await;
        assert_ne!(
            current, created,
            "case {name:?}: publishing a version must advance the unpublished revision, got \
             {current:?} for both"
        );
        // The version is its own identity: its revision is neither the one it was
        // published from nor the one the function was left at.
        assert_ne!(
            version_revision, created,
            "case {name:?}: the version's revision must differ from the one it was published \
             from, got {version_revision:?} for both"
        );
        assert_ne!(
            version_revision, current,
            "case {name:?}: the version's revision must differ from the unpublished one, got \
             {version_revision:?} for both"
        );

        // The pre-publish revision is stale for every operation that takes one ...
        let uri = format!("{FUNCTIONS_URI}/{name}/{}", case.suffix);
        let mut stale = case.body.clone();
        stale["RevisionId"] = json!(created);
        let refused = json_call(&router, case.method, &uri, &stale).await;
        assert_precondition_refusal(name, "the operation with the pre-publish revision", &refused);
        let held = published_versions(&router, name).await;
        assert_eq!(
            held,
            vec!["1".to_string()],
            "case {name:?}: a refused precondition must leave the versions it found, got \
             {held:?}"
        );
        assert_eq!(
            read_revision(&router, name).await,
            current,
            "case {name:?}: a refused precondition must leave the revision it found"
        );

        // ... and the current one is served.
        let mut fresh = case.body.clone();
        fresh["RevisionId"] = json!(current);
        let served = json_call(&router, case.method, &uri, &fresh).await;
        assert_eq!(
            served.status,
            case.served,
            "case {name:?}: expected {} at the current revision, got {} {}",
            case.served,
            served.status,
            String::from_utf8_lossy(&served.body)
        );
    }
}

#[tokio::test]
async fn publishing_twice_with_nothing_changed_answers_the_version_already_held() {
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = router_on(&engine);
    let name = "unchanged-since-its-version";
    let created = created_function(&router, &engine, name).await;

    let first = publish_version(&router, name, Some(&created)).await;
    assert_eq!(
        first.status,
        201,
        "expected 201 from the first PublishVersion, got {} {}",
        first.status,
        String::from_utf8_lossy(&first.body)
    );

    // Nothing has changed since that version, so publishing again at the revision the
    // publish left the function at adds nothing: the version already held is answered.
    let current = read_revision(&router, name).await;
    let again = publish_version(&router, name, Some(&current)).await;
    assert_eq!(
        again.status,
        201,
        "expected 201 republishing an unchanged function, got {} {}",
        again.status,
        String::from_utf8_lossy(&again.body)
    );
    assert_eq!(
        again.json()["Version"],
        json!("1"),
        "expected the version already held, got {:?}",
        again.json()["Version"]
    );
    let held = published_versions(&router, name).await;
    assert_eq!(
        held,
        vec!["1".to_string()],
        "expected the function to hold exactly the one version, got {held:?}"
    );
    assert_eq!(
        read_revision(&router, name).await,
        current,
        "publishing nothing changes nothing, so the revision must be the one it found"
    );
}
