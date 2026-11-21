// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP tests for what `CreateLaunchConfigurationTemplate` MINTS and what it
//! refuses once the store is full — the part of the create rule the lifecycle suite
//! (`crate::launch_configuration_templates`) does not reach, which only ever holds a
//! handful of templates at a time.
//!
//! The rules under test, each asserted over a SET of creates rather than one request:
//!
//! 1. Every create mints a template the store did not hold: its id is fresh (no id is
//!    ever handed out twice) and a create leaves every template already stored exactly
//!    as it was.
//! 2. The store is bounded. A create is served while the store holds fewer templates
//!    than the service's bound, and the create that would push it past the bound is the
//!    modelled `ServiceQuotaExceededException` (HTTP 402) naming this service and its
//!    name for the resource — never a 500, never a 201 the store did not back, and
//!    never an endlessly growing store.
//! 3. The bound is on what the store HOLDS, not on how many creates were made: a delete
//!    frees a slot, and the next create is served again and reports a template the read
//!    then returns.

use std::collections::BTreeSet;

use serde_json::{json, Value};
use service_com_amazonaws_drs_elasticdisasterrecoveryservice::state::launch_configuration_templates::QUOTA;
use smithy_server_runtime::MultiServiceRouter;

use crate::launch_configuration_templates::is_template_id;
use crate::replication_configuration_template_lifecycle::{call, drs, items, Answer};

/// The code and status the model gives `CreateLaunchConfigurationTemplate` for a request
/// the service cannot complete within its quota
/// (`smithy_query(query="operation",operation="CreateLaunchConfigurationTemplate")`
/// lists `ServiceQuotaExceededException`, HTTP 402) — written as the literals a client
/// reads rather than taken from the implementation's constants.
const QUOTA_EXCEEDED_CODE: &str = "ServiceQuotaExceededException";
const QUOTA_EXCEEDED_STATUS: u16 = 402;

/// How this service names the resource in that refusal: its `resourceType`, and the
/// `serviceCode` that says whose quota was reached (`drs`, the service's ARN namespace
/// and SigV4 signing name).
const RESOURCE_TYPE: &str = "Launch Configuration Template";
const SERVICE_CODE: &str = "drs";

/// A string member of a JSON object, or the empty string when it is absent.
fn text(value: &Value, name: &str) -> String {
    value
        .get(name)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// One create, answered as the service answered it.
async fn create(router: &MultiServiceRouter, body: &Value) -> Answer {
    call(router, "CreateLaunchConfigurationTemplate", body).await
}

/// The id of the template a create reports, failing the test on anything but the
/// modelled 201 carrying a template.
async fn create_id(router: &MultiServiceRouter, case: &str, body: &Value) -> String {
    let answer = create(router, body).await;
    assert_eq!(
        answer.status, 201,
        "case {case:?}: expected 201, got {answer:?}"
    );
    let template = answer
        .member("launchConfigurationTemplate")
        .unwrap_or_else(|| panic!("case {case:?}: expected a created template, got {answer:?}"));
    let id = text(template, "launchConfigurationTemplateID");
    assert!(
        is_template_id(&id),
        "case {case:?}: expected an lct- id of the modelled form, got {id:?}"
    );
    id
}

/// The ids every stored template is reported under, walking every page of the
/// `@paginated` read.
async fn stored_ids(router: &MultiServiceRouter) -> Vec<String> {
    let mut ids = Vec::new();
    let mut body = json!({});
    loop {
        let answer = call(router, "DescribeLaunchConfigurationTemplates", &body).await;
        assert_eq!(
            answer.status, 200,
            "reading the store: expected 200, got {answer:?}"
        );
        for item in items(&answer) {
            ids.push(text(item, "launchConfigurationTemplateID"));
        }
        match answer.member("nextToken").and_then(Value::as_str) {
            Some(token) => body = json!({"nextToken": token}),
            None => return ids,
        }
    }
}

// ---------------------------------------------------------------------------
// Rule 1 — every create mints a template the store did not hold
// ---------------------------------------------------------------------------

/// A create never hands out an id it handed out before, and never disturbs a template
/// already stored: after a run of creates whose bodies differ along the members the
/// operation accepts, every id is distinct and every earlier template is still reported
/// with the members it was created with.
#[tokio::test]
async fn every_create_mints_a_fresh_id_and_leaves_the_stored_templates_alone() {
    let router = drs();

    // The rows differ along which member carries the value that tells them apart, so
    // "fresh id" is not read off one body repeated: a tag value, a boolean, and a body
    // naming nothing at all.
    let mut minted: Vec<(String, Value)> = Vec::new();
    for index in 0..8 {
        let (case, body) = match index % 4 {
            0 => (
                format!("create #{index} with a tag"),
                json!({"tags": {"i": index.to_string()}}),
            ),
            1 => (
                format!("create #{index} with copyTags set"),
                json!({"copyTags": true}),
            ),
            2 => (
                format!("create #{index} with copyPrivateIp cleared"),
                json!({"copyPrivateIp": false}),
            ),
            _ => (format!("create #{index} naming nothing"), json!({})),
        };
        let id = create_id(&router, &case, &body).await;

        // Every template minted earlier is still reported, unchanged by this create.
        for (earlier_case, earlier) in &minted {
            let earlier_id = text(earlier, "launchConfigurationTemplateID");
            assert_ne!(
                earlier_id, id,
                "case {case:?}: id {id} was already minted for {earlier_case:?}"
            );
            let answer = call(
                &router,
                "DescribeLaunchConfigurationTemplates",
                &json!({"launchConfigurationTemplateIDs": [earlier_id]}),
            )
            .await;
            assert_eq!(
                items(&answer),
                &vec![earlier.clone()],
                "case {case:?}: the create must leave {earlier_case:?} as it was, got {answer:?}"
            );
        }

        let answer = call(
            &router,
            "DescribeLaunchConfigurationTemplates",
            &json!({"launchConfigurationTemplateIDs": [id]}),
        )
        .await;
        let stored = items(&answer)
            .first()
            .unwrap_or_else(|| panic!("case {case:?}: the created template must be read back"))
            .clone();
        minted.push((case, stored));
    }

    let reported = stored_ids(&router).await;
    assert_eq!(
        reported.len(),
        minted.len(),
        "the store must hold one template per create, got {reported:?}"
    );
    assert_eq!(
        reported.iter().collect::<BTreeSet<_>>().len(),
        minted.len(),
        "every minted id must be distinct, got {reported:?}"
    );
}

// ---------------------------------------------------------------------------
// Rules 2 and 3 — the store is bounded, and the bound is on what it holds
// ---------------------------------------------------------------------------

/// The service bounds what a caller can make it hold: creates are served up to
/// `QUOTA` templates (the bound this service's state publishes for this store), the one
/// past it is the modelled `ServiceQuotaExceededException` (402) and stores nothing, and
/// once a delete frees a slot the next create is served again.
///
/// The bound is driven from the service's own constant rather than a number written
/// here, and it is exercised at the last create it allows, at one past it, and at the
/// slot a delete gives back.
#[tokio::test]
async fn the_store_holds_at_most_its_bound_of_templates() {
    let router = drs();

    let mut ids = Vec::with_capacity(QUOTA);
    for index in 0..QUOTA {
        // The last create under the bound is asserted on in full; the ones before it
        // only have to be served, so the bound itself is what this walk is about.
        let answer = create(&router, &json!({"tags": {"i": index.to_string()}})).await;
        assert_eq!(
            answer.status, 201,
            "case create #{index} (the store holds {index} of {QUOTA}): expected 201, \
             got {answer:?}"
        );
        ids.push(text(
            answer
                .member("launchConfigurationTemplate")
                .unwrap_or_else(|| panic!("case create #{index}: no template in {answer:?}")),
            "launchConfigurationTemplateID",
        ));
    }

    // One past the bound: the modelled refusal, naming this service and its name for
    // the resource whose quota was reached.
    let refused = create(&router, &json!({})).await;
    assert_eq!(
        refused.status, QUOTA_EXCEEDED_STATUS,
        "case create #{QUOTA} (at the bound): expected {QUOTA_EXCEEDED_STATUS}, got {refused:?}"
    );
    assert_eq!(
        refused.error_code.as_deref(),
        Some(QUOTA_EXCEEDED_CODE),
        "case create #{QUOTA} (at the bound): expected {QUOTA_EXCEEDED_CODE}, got {refused:?}"
    );
    assert_eq!(
        text(&refused.body, "resourceType"),
        RESOURCE_TYPE,
        "case create #{QUOTA} (at the bound): wrong resourceType in {refused:?}"
    );
    assert_eq!(
        text(&refused.body, "serviceCode"),
        SERVICE_CODE,
        "case create #{QUOTA} (at the bound): wrong serviceCode in {refused:?}"
    );
    assert!(
        refused.member("launchConfigurationTemplate").is_none(),
        "case create #{QUOTA} (at the bound): a refusal must report no template, got {refused:?}"
    );

    // The refusal stored nothing: the store still reports exactly the templates the
    // served creates minted, each once and in creation order.
    assert_eq!(
        stored_ids(&router).await,
        ids,
        "a refused create must leave the store exactly as it was"
    );

    // Rule 3 — the bound counts what the store HOLDS. Giving a slot back makes the
    // next create servable again, and the template it mints is one the read returns.
    let freed = ids.remove(0);
    let deleted = call(
        &router,
        "DeleteLaunchConfigurationTemplate",
        &json!({"launchConfigurationTemplateID": freed}),
    )
    .await;
    assert_eq!(
        deleted.status, 204,
        "deleting {freed} to free a slot: expected 204, got {deleted:?}"
    );

    let replacement = create_id(&router, "the create that fills the freed slot", &json!({})).await;
    assert!(
        !ids.contains(&replacement),
        "the freed slot must be filled by a fresh id, got {replacement}"
    );
    ids.push(replacement);
    assert_eq!(
        stored_ids(&router).await,
        ids,
        "the store must hold the templates the served creates minted, and no other"
    );

    // And the store is full again, so the create after it is refused once more.
    let refused_again = create(&router, &json!({})).await;
    assert_eq!(
        refused_again.status, QUOTA_EXCEEDED_STATUS,
        "the create past the bound again: expected {QUOTA_EXCEEDED_STATUS}, got {refused_again:?}"
    );
    assert_eq!(
        refused_again.error_code.as_deref(),
        Some(QUOTA_EXCEEDED_CODE),
        "the create past the bound again: expected {QUOTA_EXCEEDED_CODE}, got {refused_again:?}"
    );
}
