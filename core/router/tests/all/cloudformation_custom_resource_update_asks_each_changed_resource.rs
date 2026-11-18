// SPDX-License-Identifier: CC0-1.0

//! A stack update over custom resources asks their provider to UPDATE what already exists —
//! read as a provider really sees it, off the Amazon SQS queue subscribed to the Amazon SNS
//! topic the template's `ServiceToken` names.
//!
//! The RULE under test: **a stack operation that changes the declaration of a custom resource
//! that already exists applies it through the provider's `Update` request, carrying that
//! resource's current `PhysicalResourceId` and the `OldResourceProperties` it was last declared
//! with; a resource whose declaration the operation does not change is not asked about at all;
//! and the resource keeps the id it was created under unless the provider's own answer names a
//! different one.**
//!
//! Three crates have to agree for that to hold — CloudFormation publishes the request and holds
//! the resource until the answer lands, Amazon SNS delivers the publish to the subscribed
//! endpoint, Amazon SQS holds it for the provider to read — so the suite lives here.
//!
//! [`super::cloudformation_sns_custom_resource_protocol`] pins what one `Update`'s ANSWER
//! decides: an answer naming a new id is a replacement whose superseded id owes the cleanup
//! `Delete`, and an answer repeating the id owes no further request. What only this suite
//! covers is WHICH resources of a stack an update asks about, and that the requests it
//! publishes for them are `Update`s rather than a replace-by-delete. Its provider — the topic,
//! the subscribed queue, the read of the next request off it, the unsigned write that answers
//! it and the quiet window that decides nothing further is owed — is that suite's, so the two
//! cannot drift about how a provider is stood up or about what quiet means.
//!
//! The rows differ along what the rule generalises over: which of the stack's two custom
//! resources the update changes (the first, the second, or both).

use std::collections::BTreeMap;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::cloudformation_sns_custom_resource_protocol::{
    answered_success, build_router, cloudformation, create_stack, next_request, provider,
    requests_inside_the_quiet_window, settled_with_a_responsive_provider, CREATE_COMPLETE,
    UPDATE_COMPLETE,
};
use crate::query_wire::physical_ids_of;

/// The two custom resources every row's template declares, and the id the provider mints for
/// each of them on the create.
const RESOURCES: [(&str, &str); 2] = [("First", "thing-first"), ("Second", "thing-second")];

/// The property each resource is declared with before the update, and after it for a resource
/// the update changes.
const BEFORE: &str = "n1";
const AFTER: &str = "n2";

/// The template of one row: both resources over the same `ServiceToken`, each carrying the
/// `Name` the row states for it.
fn template(service_token: &str, names: [&str; 2]) -> String {
    let mut resources = serde_json::Map::new();
    for (index, (logical_id, _)) in RESOURCES.iter().enumerate() {
        resources.insert(
            (*logical_id).to_string(),
            json!({
                "Type": "Custom::Versioned",
                "Properties": {
                    "ServiceToken": service_token,
                    "Name": names[index],
                },
            }),
        );
    }
    json!({ "Resources": Value::Object(resources) }).to_string()
}

/// The `PhysicalResourceId` the stack reports for each of its resources.
async fn reported_ids(
    router: &MultiServiceRouter,
    case: &str,
    stack: &str,
) -> BTreeMap<String, String> {
    let (status, body) =
        cloudformation(router, "DescribeStackResources", &[("StackName", stack)]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: DescribeStackResources must be answered, got {status} with {body}"
    );
    physical_ids_of(&body)
}

/// An update asks the provider to `Update` each custom resource whose declaration it changed —
/// naming that resource's current physical id and the properties it was last declared with —
/// asks nothing about the resources it left alone, and leaves every id as the provider's own
/// answer states it.
#[tokio::test]
async fn an_update_asks_the_provider_about_each_changed_custom_resource() {
    // (case, the stack, which of the two resources the update changes)
    let rows: &[(&str, &str, [bool; 2])] = &[
        (
            "an update changing the first resource",
            "cr-update-first",
            [true, false],
        ),
        (
            "an update changing the second resource",
            "cr-update-second",
            [false, true],
        ),
        (
            "an update changing both resources",
            "cr-update-both",
            [true, true],
        ),
    ];
    for (case, stack, changed) in rows {
        let case = *case;
        let router = build_router();
        let mut provider = provider(&router, stack).await;
        let token = provider.topic_arn.clone();

        create_stack(&router, case, stack, &template(&token, [BEFORE, BEFORE])).await;
        // The create asks about both resources; each is answered with the id of its own row.
        for _ in RESOURCES {
            let created = next_request(&router, &mut provider, case, "a Create").await;
            assert_eq!(
                created["RequestType"].as_str(),
                Some("Create"),
                "case {case:?}: a resource nothing has created owes a Create, got {created}"
            );
            let logical = created["LogicalResourceId"].as_str().unwrap_or_default();
            let minted = RESOURCES
                .iter()
                .find(|(declared, _)| *declared == logical)
                .map(|(_, id)| *id)
                .unwrap_or_else(|| {
                    panic!("case {case:?}: the Create names a declared resource, got {created}")
                });
            answered_success(&router, case, &created, minted, &Value::Null).await;
        }
        let settled =
            settled_with_a_responsive_provider(&router, &mut provider, case, stack, CREATE_COMPLETE)
                .await;
        assert_eq!(
            settled, CREATE_COMPLETE,
            "case {case:?}: both created resources must complete the stack, got {settled:?}"
        );

        // The update under test: only the resources the row changes are re-declared.
        let names = [
            if changed[0] { AFTER } else { BEFORE },
            if changed[1] { AFTER } else { BEFORE },
        ];
        let (status, body) = cloudformation(
            &router,
            "UpdateStack",
            &[
                ("StackName", stack),
                ("TemplateBody", &template(&token, names)),
            ],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: UpdateStack must be accepted, got {status} with {body}"
        );

        // One request per CHANGED resource, each an Update naming what exists — and nothing
        // about the resource the update left alone, which is what the count pins: a request for
        // it would be read here as one of these and fail the match.
        let wanted: Vec<&str> = RESOURCES
            .iter()
            .enumerate()
            .filter(|(index, _)| changed[*index])
            .map(|(_, (logical_id, _))| *logical_id)
            .collect();
        let mut asked: Vec<String> = Vec::new();
        for _ in &wanted {
            let update = next_request(&router, &mut provider, case, "an Update").await;
            assert_eq!(
                update["RequestType"].as_str(),
                Some("Update"),
                "case {case:?}: a changed resource that exists owes an Update, got {update}"
            );
            let logical = update["LogicalResourceId"]
                .as_str()
                .unwrap_or_default()
                .to_string();
            assert!(
                wanted.contains(&logical.as_str()),
                "case {case:?}: only a resource this update changed is asked about, got {update}"
            );
            let minted = RESOURCES
                .iter()
                .find(|(declared, _)| *declared == logical)
                .map(|(_, id)| *id)
                .unwrap_or_default();
            assert_eq!(
                update["PhysicalResourceId"].as_str(),
                Some(minted),
                "case {case:?}: the Update names the resource that exists, got {update}"
            );
            assert_eq!(
                update["ResourceProperties"]["Name"].as_str(),
                Some(AFTER),
                "case {case:?}: the Update carries the declaration asked for, got {update}"
            );
            assert_eq!(
                update["OldResourceProperties"]["Name"].as_str(),
                Some(BEFORE),
                "case {case:?}: the Update carries the properties last deployed, got {update}"
            );
            // Answered IN PLACE — the id the request already names — so the update owes no
            // cleanup of a superseded id and the stack's ids are the ones the create minted.
            answered_success(&router, case, &update, minted, &Value::Null).await;
            asked.push(logical);
        }
        asked.sort();
        let mut expected: Vec<String> = wanted.iter().map(|id| (*id).to_string()).collect();
        expected.sort();
        assert_eq!(
            asked, expected,
            "case {case:?}: the update must ask about each resource it changed, once"
        );

        // ...and nothing further is owed: the resource the update left alone is not asked
        // about, and an in-place answer owes no cleanup `Delete`. Asserted over the quiet
        // window BEFORE the settle below, which would answer whatever arrived and hide it.
        let unexpected = requests_inside_the_quiet_window(&router, &mut provider).await;
        assert!(
            unexpected.is_empty(),
            "case {case:?}: the update owes no request beyond the Update of each resource it \
             changed, got {unexpected:?}"
        );

        // Every id the stack reports is the one the provider minted on the create: an in-place
        // answer replaces nothing, so no resource was taken down and made again.
        let settled =
            settled_with_a_responsive_provider(&router, &mut provider, case, stack, UPDATE_COMPLETE)
                .await;
        assert_eq!(
            settled, UPDATE_COMPLETE,
            "case {case:?}: the update must complete, got {settled:?}"
        );
        let reported = reported_ids(&router, case, stack).await;
        for (logical_id, minted) in RESOURCES {
            assert_eq!(
                reported.get(logical_id).map(String::as_str),
                Some(minted),
                "case {case:?}: {logical_id} must keep the id its provider minted, got \
                 {reported:?}"
            );
        }
    }
}
