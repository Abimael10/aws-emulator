// SPDX-License-Identifier: CC0-1.0

//! What becomes of the usage-plan ASSOCIATIONS that named a stage once that stage is
//! deleted.
//!
//! Every case drives the real wire path — a `restJson1` HTTP request through the
//! multi-service router into the mounted service — so what is asserted is what a client
//! sees, not what a handler returns. The emulator, the request builder and the
//! `CreateRestApi` / `CreateDeployment` / `CreateStage` / `CreateUsagePlan` /
//! `GetUsagePlan` / `CreateApiKey` / `CreateUsagePlanKey` helpers are this crate's existing
//! harness, reused from `rest_api_lifecycle`, `stage_lifecycle`, `usage_plan_lifecycle` and
//! `usage_plan_key_lifecycle`.
//!
//! THE RULE UNDER TEST: a `UsagePlan`'s `apiStages` entry is an association WITH a stage —
//! "the associated API stages of a usage plan", whose throttle and quota meter the traffic
//! that stage serves — so it cannot outlive the stage it names: deleting a stage drops every
//! entry naming it from every plan that held one, and `GetUsagePlan` reports no association
//! with a stage that resolves to nothing. What the removal is NOT about goes untouched: the
//! plan itself, its name, its limits, its tags, the API keys it meters, its associations
//! with OTHER stages — of the same API and of another — and the API's own deployments.
//!
//! The rows span what the rule generalises over: how many associations the plan holds and
//! which of them is deleted (its only one, the first, a middle one, the last), how many
//! PLANS hold an association with the deleted stage (none, one, several), whether the
//! association carries a per-method throttle of its own, the call that created the stage
//! (`CreateStage` or `CreateDeployment`'s own `stageName`), and the name of a stage of
//! ANOTHER API spelled the same way as the deleted one.

use serde_json::{json, Value};

use crate::rest_api_lifecycle::{assert_failure, create_api, string_member, Answer, Gateway};
use crate::stage_lifecycle::{create_deployed_stage, create_deployment, get_stage};
use crate::usage_plan_lifecycle::{create_usage_plan, get_usage_plan};

/// `DeleteStage` for `stage_name` of `api`.
async fn delete_stage(gateway: &Gateway, api: &str, stage_name: &str) -> Answer {
    gateway
        .call(
            "DELETE",
            &format!("/restapis/{api}/stages/{stage_name}"),
            None,
        )
        .await
}

/// The `<apiId>:<stageName>` pairs a reported plan is associated with, in the order the
/// body lists them — the whole association list in one comparable value.
fn associations(body: &Value, case: &str) -> Vec<String> {
    body["apiStages"]
        .as_array()
        .unwrap_or_else(|| panic!("case {case}: expected an apiStages list, got {body:?}"))
        .iter()
        .map(|entry| {
            format!(
                "{api}:{stage}",
                api = entry["apiId"].as_str().unwrap_or_default(),
                stage = entry["stage"].as_str().unwrap_or_default()
            )
        })
        .collect()
}

/// Create a stage of `api` named `stage_name`, by `CreateStage` on a shared deployment when
/// `deployment` is given and by `CreateDeployment`'s own `stageName` when it is not.
async fn make_stage(gateway: &Gateway, api: &str, stage_name: &str, deployment: Option<&str>) {
    match deployment {
        Some(deployment) => {
            let answer = gateway
                .call(
                    "POST",
                    &format!("/restapis/{api}/stages"),
                    Some(json!({ "stageName": stage_name, "deploymentId": deployment })),
                )
                .await;
            assert_eq!(
                answer.status, 201,
                "case CreateStage({stage_name}): expected 201, got {} with {:?}",
                answer.status, answer.body
            );
        }
        None => {
            create_deployed_stage(gateway, api, stage_name).await;
        }
    }
}

#[tokio::test]
async fn deleting_a_stage_drops_the_usage_plan_associations_that_named_it() {
    let gateway = Gateway::new();
    let (api, _) = create_api(&gateway, "stage-delete-cascade").await;
    let deployment = create_deployment(
        &gateway,
        &api,
        "the shared deployment",
        json!({ "description": "shared" }),
    )
    .await;
    // A second API carrying a stage spelled exactly like one of the first API's: an entry
    // names a stage OF an API, so the pair is what a cascade matches on.
    let (other_api, _) = create_api(&gateway, "stage-delete-cascade-other").await;

    // The stages of the first API, one per row below plus the two that must survive.
    let stages = ["only", "first", "middle", "last", "kept", "shared-name"];
    for stage in stages {
        make_stage(&gateway, &api, stage, Some(&deployment)).await;
    }
    // Created by `CreateDeployment`'s own `stageName` instead, so the row's stage is not
    // always made by the same call.
    make_stage(&gateway, &api, "deployed", None).await;
    make_stage(&gateway, &other_api, "shared-name", None).await;

    // One plan per row: the associations it is created with, the stage the row deletes, and
    // the associations it must report afterwards. Rows differ in how many associations the
    // plan holds and which one goes, and one row's association carries a per-method
    // throttle of its own.
    type Row = (&'static str, Vec<Value>, &'static str, Vec<String>);
    let entry = |stage: &str| json!({ "apiId": api, "stage": stage });
    let throttled = |stage: &str| {
        json!({
            "apiId": api,
            "stage": stage,
            "throttle": { "/pets/GET": { "rateLimit": 7.0, "burstLimit": 3 } }
        })
    };
    let rows: Vec<Row> = vec![
        (
            "the plan's only association",
            vec![entry("only")],
            "only",
            Vec::new(),
        ),
        (
            "the first of three",
            vec![entry("first"), entry("kept"), entry("deployed")],
            "first",
            vec![format!("{api}:kept"), format!("{api}:deployed")],
        ),
        (
            "the middle of three",
            vec![entry("kept"), entry("middle"), entry("deployed")],
            "middle",
            vec![format!("{api}:kept"), format!("{api}:deployed")],
        ),
        (
            "the last of two",
            vec![entry("kept"), entry("last")],
            "last",
            vec![format!("{api}:kept")],
        ),
        (
            "an association carrying a per-method throttle",
            vec![throttled("deployed"), entry("kept")],
            "deployed",
            vec![format!("{api}:kept")],
        ),
        (
            "a stage of another API spelled the same way",
            vec![
                entry("shared-name"),
                json!({ "apiId": other_api, "stage": "shared-name" }),
            ],
            "shared-name",
            vec![format!("{other_api}:shared-name")],
        ),
    ];

    let mut plans: Vec<(&str, String, Value, Vec<String>, &str)> = Vec::new();
    for (case, created_with, deleted_stage, expected) in &rows {
        let created = create_usage_plan(
            &gateway,
            case,
            json!({
                "name": format!("plan-{case}"),
                "description": "metered",
                "apiStages": created_with,
                "throttle": { "rateLimit": 10.0, "burstLimit": 5 },
                "quota": { "limit": 100, "period": "DAY" },
                "tags": { "owner": "cascade" },
            }),
        )
        .await;
        let id = string_member(&created, "id", case);
        plans.push((case, id, created, expected.clone(), deleted_stage));
    }

    // Each row's stage is deleted once, and the plan is read back after: a plan whose
    // association named it no longer reports it, and every other member of the plan is
    // exactly what the create reported.
    for (case, id, created, expected, deleted_stage) in &plans {
        let deleted = delete_stage(&gateway, &api, deleted_stage).await;
        assert_eq!(
            deleted.status, 202,
            "case {case}: expected 202 from DeleteStage({deleted_stage}), got {} with {:?}",
            deleted.status, deleted.body
        );
        assert_failure(
            &get_stage(&gateway, &api, deleted_stage).await,
            404,
            "NotFoundException",
            &format!("{case} / GetStage({deleted_stage}) after the delete"),
        );

        let read = get_usage_plan(&gateway, id).await;
        assert_eq!(
            read.status, 200,
            "case {case}: the plan must survive the stage, got {} with {:?}",
            read.status, read.body
        );
        assert_eq!(
            &associations(&read.body, case),
            expected,
            "case {case}: expected the associations {expected:?} after the delete, got {:?}",
            read.body["apiStages"]
        );
        // What the delete was not about is untouched, member for member.
        for member in ["id", "name", "description", "throttle", "quota", "tags"] {
            assert_eq!(
                read.body[member], created[member],
                "case {case}: {member} must be untouched by the stage delete, got {:?} after \
                 {:?}",
                read.body[member], created[member]
            );
        }
    }

    // A plan that never named any of the deleted stages is untouched too, and the API's
    // deployments outlive every stage that served them.
    let untouched = create_usage_plan(
        &gateway,
        "a plan naming another stage only",
        json!({
            "name": "plan-untouched",
            "apiStages": [{ "apiId": other_api, "stage": "shared-name" }],
        }),
    )
    .await;
    let untouched_id = string_member(&untouched, "id", "a plan naming another stage only");
    let read = get_usage_plan(&gateway, &untouched_id).await;
    assert_eq!(
        associations(&read.body, "a plan naming another stage only"),
        vec![format!("{other_api}:shared-name")],
        "case a plan naming another stage only: expected its association to stand, got {:?}",
        read.body["apiStages"]
    );
    let deployment_read = gateway
        .call(
            "GET",
            &format!("/restapis/{api}/deployments/{deployment}"),
            None,
        )
        .await;
    assert_eq!(
        deployment_read.status, 200,
        "case the shared deployment: a deployment outlives the stages that served it, got {} \
         with {:?}",
        deployment_read.status, deployment_read.body
    );
}

#[tokio::test]
async fn a_deleted_stage_is_dropped_from_every_plan_that_named_it() {
    // The rule is the ASSOCIATION's, not one plan's: several plans may meter one stage, so
    // the delete has to reach each of them. Rows differ in how many plans name the stage.
    let gateway = Gateway::new();
    let (api, _) = create_api(&gateway, "stage-delete-cascade-fanout").await;
    let deployment = create_deployment(
        &gateway,
        &api,
        "the fan-out deployment",
        json!({ "description": "fan-out" }),
    )
    .await;
    for stage in ["metered", "spare"] {
        make_stage(&gateway, &api, stage, Some(&deployment)).await;
    }

    // Three plans naming the deleted stage and one naming only the spare: the spare's
    // association is the control, so a cascade that dropped too much is caught.
    let mut metering: Vec<String> = Vec::new();
    for index in 0..3 {
        let case = format!("plan {index} metering the stage");
        let created = create_usage_plan(
            &gateway,
            &case,
            json!({
                "name": format!("plan-fanout-{index}"),
                "apiStages": [
                    { "apiId": api, "stage": "metered" },
                    { "apiId": api, "stage": "spare" },
                ],
            }),
        )
        .await;
        metering.push(string_member(&created, "id", &case));
    }
    let spare_only = create_usage_plan(
        &gateway,
        "the plan metering the spare alone",
        json!({
            "name": "plan-fanout-spare",
            "apiStages": [{ "apiId": api, "stage": "spare" }],
        }),
    )
    .await;
    let spare_only = string_member(&spare_only, "id", "the plan metering the spare alone");

    let deleted = delete_stage(&gateway, &api, "metered").await;
    assert_eq!(
        deleted.status, 202,
        "case DeleteStage(metered): expected 202, got {} with {:?}",
        deleted.status, deleted.body
    );

    for (index, id) in metering.iter().enumerate() {
        let case = format!("plan {index} after the delete");
        let read = get_usage_plan(&gateway, id).await;
        assert_eq!(
            associations(&read.body, &case),
            vec![format!("{api}:spare")],
            "case {case}: expected only the spare association to stand, got {:?}",
            read.body["apiStages"]
        );
    }
    let read = get_usage_plan(&gateway, &spare_only).await;
    assert_eq!(
        associations(&read.body, "the plan metering the spare alone"),
        vec![format!("{api}:spare")],
        "case the plan metering the spare alone: expected its association to stand, got {:?}",
        read.body["apiStages"]
    );

    // Deleting the spare too leaves every plan associated with nothing, and each plan is
    // still readable and still deletable — which an association standing in the way of the
    // delete would prevent.
    let deleted = delete_stage(&gateway, &api, "spare").await;
    assert_eq!(
        deleted.status, 202,
        "case DeleteStage(spare): expected 202, got {} with {:?}",
        deleted.status, deleted.body
    );
    for (index, id) in metering.iter().enumerate() {
        let case = format!("plan {index} after both deletes");
        let read = get_usage_plan(&gateway, id).await;
        assert!(
            associations(&read.body, &case).is_empty(),
            "case {case}: expected no association at all, got {:?}",
            read.body["apiStages"]
        );
        let removed = gateway
            .call("DELETE", &format!("/usageplans/{id}"), None)
            .await;
        assert_eq!(
            removed.status, 202,
            "case {case}: a plan associated with nothing must be deletable, got {} with {:?}",
            removed.status, removed.body
        );
    }
}
