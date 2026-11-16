// SPDX-License-Identifier: CC0-1.0

//! The three `AWS::ApiGateway::*` types that METER a REST API — the usage plan, the api key it
//! meters and the association that puts the key in the plan — end to end: a stack declaring
//! them settles `CREATE_COMPLETE`, publishes the identifiers Amazon API Gateway assigned, and
//! that service holds all three under those identifiers.
//!
//! RULE under test: **a template may leave out the name of a resource whose type publishes its
//! name property as optional — the name is required of the owning API, not of the template —
//! so CloudFormation creates such a resource under the name it generated for it
//! (`<stack name>-<logical id>-<unique id>`) rather than failing the resource and rolling the
//! stack back.**
//!
//! The rows differ along what the rule generalises over: WHICH of the declared resources is
//! left unnamed — the plan, the key, both, or neither — over one template that declares the
//! whole metering set (an api with a key-required method, a deployment, a stage, the plan, the
//! key and the association).
//!
//! Why it matters: a usage plan and an api key with no name of their own is what every
//! CDK/SAM-generated template declares, and it is the documented way to declare API key
//! metering at all. A stack that rolls back for want of a name the template never had to
//! supply leaves no api, no key and no plan behind, so nothing downstream of the deploy can be
//! exercised.
//!
//! What the deployed stage then DOES with the key — admitting a call that presents it and
//! refusing one that does not — is the router suite's subject, where API Gateway's data plane
//! is mounted beside this service. What a DELETE of such a stack does is the usage plan's own
//! deletion contract (`DeleteUsagePlan` refuses a plan that still meters api stages) and not
//! this rule's subject.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::apigateway_vpc_link_resources::{apigateway, is_api_gateway_id, member_text};
use crate::stack_lifecycle::{
    create_stack, output_value, router, settled_stack, xml_text,
};

/// How many characters a minted api-key or usage-plan id carries: API Gateway assigns a
/// record of the account ten lowercase alphanumeric characters, the width its `RestApi` ids
/// carry too.
const MINTED_ID_CHARACTERS: usize = 10;

/// The stage the api is deployed to, which the plan meters.
const STAGE: &str = "prod";

/// The throttle the declared plan carries, as the template states it.
const RATE_LIMIT: f64 = 5.0;
const BURST_LIMIT: u64 = 2;

/// The quota the declared plan carries, as the template states it.
const QUOTA_LIMIT: u64 = 1000;
const QUOTA_PERIOD: &str = "DAY";

/// One row: which of the two nameable resources the template leaves unnamed.
struct Row {
    case: &'static str,
    stack: &'static str,
    /// The `UsagePlanName` the template declares, or `None` to declare none.
    plan_name: Option<&'static str>,
    /// The api key's `Name`, or `None` to declare none.
    key_name: Option<&'static str>,
}

const ROWS: &[Row] = &[
    Row {
        case: "both the plan and the key named by the template",
        stack: "apigw-metering-named",
        plan_name: Some("named-plan"),
        key_name: Some("named-key"),
    },
    Row {
        case: "a plan the template does not name",
        stack: "apigw-metering-unnamed-plan",
        plan_name: None,
        key_name: Some("plan-less-name"),
    },
    Row {
        case: "a key the template does not name",
        stack: "apigw-metering-unnamed-key",
        plan_name: Some("key-less-name"),
        key_name: None,
    },
    Row {
        case: "neither the plan nor the key named, as a generated template declares them",
        stack: "apigw-metering-unnamed-both",
        plan_name: None,
        key_name: None,
    },
];

/// The logical ids the template declares its metering resources under, which are half of the
/// name CloudFormation generates for an unnamed one.
const PLAN_ID: &str = "UsagePlan";
const KEY_ID: &str = "TestApiKey";

/// The template one row deploys: the api, a resource under its root, a key-required method
/// with a MOCK integration, a deployment, the stage it serves, the usage plan over that stage,
/// the api key and the association that puts the key in the plan.
fn template(row: &Row) -> String {
    let mut plan = json!({
        "ApiStages": [{ "ApiId": { "Ref": "Api" }, "Stage": { "Ref": "ProdStage" } }],
        "Throttle": { "RateLimit": RATE_LIMIT, "BurstLimit": BURST_LIMIT },
        "Quota": { "Limit": QUOTA_LIMIT, "Period": QUOTA_PERIOD }
    });
    if let Some(name) = row.plan_name {
        plan["UsagePlanName"] = json!(name);
    }
    let mut key = json!({ "Enabled": true });
    if let Some(name) = row.key_name {
        key["Name"] = json!(name);
    }
    let mut resources = serde_json::Map::new();
    resources.insert(
        "Api".to_string(),
        json!({
            "Type": "AWS::ApiGateway::RestApi",
            "Properties": { "Name": format!("{}-api", row.stack) }
        }),
    );
    resources.insert(
        "Items".to_string(),
        json!({
            "Type": "AWS::ApiGateway::Resource",
            "Properties": {
                "RestApiId": { "Ref": "Api" },
                "ParentId": { "Fn::GetAtt": ["Api", "RootResourceId"] },
                "PathPart": "ping"
            }
        }),
    );
    resources.insert(
        "Method".to_string(),
        json!({
            "Type": "AWS::ApiGateway::Method",
            "Properties": {
                "RestApiId": { "Ref": "Api" },
                "ResourceId": { "Ref": "Items" },
                "HttpMethod": "GET",
                "AuthorizationType": "NONE",
                "ApiKeyRequired": true,
                "Integration": {
                    "Type": "MOCK",
                    "RequestTemplates": { "application/json": "{\"statusCode\": 200}" },
                    "IntegrationResponses": [{
                        "StatusCode": "200",
                        "ResponseTemplates": { "application/json": "{\"pong\": true}" }
                    }]
                },
                "MethodResponses": [{ "StatusCode": "200" }]
            }
        }),
    );
    resources.insert(
        "Deployment".to_string(),
        json!({
            "Type": "AWS::ApiGateway::Deployment",
            "DependsOn": ["Method"],
            "Properties": { "RestApiId": { "Ref": "Api" } }
        }),
    );
    resources.insert(
        "ProdStage".to_string(),
        json!({
            "Type": "AWS::ApiGateway::Stage",
            "Properties": {
                "RestApiId": { "Ref": "Api" },
                "DeploymentId": { "Ref": "Deployment" },
                "StageName": STAGE
            }
        }),
    );
    resources.insert(
        PLAN_ID.to_string(),
        json!({ "Type": "AWS::ApiGateway::UsagePlan", "Properties": plan }),
    );
    resources.insert(
        KEY_ID.to_string(),
        json!({ "Type": "AWS::ApiGateway::ApiKey", "Properties": key }),
    );
    resources.insert(
        "PlanKey".to_string(),
        json!({
            "Type": "AWS::ApiGateway::UsagePlanKey",
            "Properties": {
                "KeyId": { "Ref": KEY_ID },
                "KeyType": "API_KEY",
                "UsagePlanId": { "Ref": PLAN_ID }
            }
        }),
    );
    let mut outputs = serde_json::Map::new();
    outputs.insert("RestApiId".to_string(), json!({ "Value": { "Ref": "Api" } }));
    outputs.insert("ApiKeyId".to_string(), json!({ "Value": { "Ref": KEY_ID } }));
    outputs.insert(
        "UsagePlanId".to_string(),
        json!({ "Value": { "Ref": PLAN_ID } }),
    );
    json!({
        "Resources": Value::Object(resources),
        "Outputs": Value::Object(outputs)
    })
    .to_string()
}

/// Whether `name` is the name CloudFormation generates for a resource a template leaves
/// unnamed: `<stack name>-<logical id>-<unique id>`, with a unique segment of its own.
fn is_generated_name(name: &str, stack: &str, logical_id: &str) -> bool {
    name.strip_prefix(&format!("{stack}-{logical_id}-"))
        .is_some_and(|unique| !unique.is_empty() && unique.chars().all(char::is_alphanumeric))
}

/// Assert the resource was created under the name the rule says: the one the template
/// declared, or — for a declaration that names it nowhere — the name CloudFormation generates,
/// asserted by its FORM, since its unique segment is CloudFormation's to derive.
fn assert_name(
    case: &str,
    reported: Option<String>,
    declared: Option<&str>,
    stack: &str,
    logical_id: &str,
) {
    match declared {
        Some(name) => assert_eq!(
            reported.as_deref(),
            Some(name),
            "case {case:?}: expected the declared name {name:?}, got {reported:?}"
        ),
        None => {
            let reported = reported.unwrap_or_default();
            assert!(
                is_generated_name(&reported, stack, logical_id),
                "case {case:?}: expected the name CloudFormation generates for an unnamed \
                 resource ({stack}-{logical_id}- plus a unique segment), got {reported:?}"
            );
        }
    }
}

/// The entries of one of API Gateway's own collection reads, as its `item` member carries them.
async fn collection(router: &MultiServiceRouter, case: &str, uri: &str) -> Vec<Value> {
    let (status, document) = apigateway(router, "GET", uri).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected GET {uri} to answer 200, got {status} with {document}"
    );
    document
        .get("item")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

#[tokio::test]
async fn a_declared_usage_plan_api_key_and_association_are_created_in_api_gateway() {
    let router = router();
    for row in ROWS {
        let case = row.case;
        let (status, answered) = create_stack(&router, row.stack, &template(row), &[]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: CreateStack must answer 200, got {status} {answered}"
        );
        let described = settled_stack(&router, case, row.stack).await;
        assert_eq!(
            xml_text(&described, "StackStatus").as_deref(),
            Some("CREATE_COMPLETE"),
            "case {case:?}: expected the create to complete, got {described}"
        );

        // The identifiers the stack publishes are the ones API Gateway MINTED: a plan and a
        // key are addressed by that service's own id, not by any name.
        let api_id = output_value(&described, "RestApiId")
            .unwrap_or_else(|| panic!("case {case:?}: no RestApiId output in {described}"));
        let key_id = output_value(&described, "ApiKeyId")
            .unwrap_or_else(|| panic!("case {case:?}: no ApiKeyId output in {described}"));
        let plan_id = output_value(&described, "UsagePlanId")
            .unwrap_or_else(|| panic!("case {case:?}: no UsagePlanId output in {described}"));
        for (what, id) in [("the api key", &key_id), ("the usage plan", &plan_id)] {
            assert!(
                is_api_gateway_id(id, MINTED_ID_CHARACTERS),
                "case {case:?}: expected {what} to publish an id of the form API Gateway mints, \
                 got {id:?}"
            );
        }

        // API Gateway holds the PLAN under that id, with the name the rule says it was created
        // under and the limits the declaration asked it to meter with.
        let (status, plan) = apigateway(&router, "GET", &format!("/usageplans/{plan_id}")).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected GetUsagePlan({plan_id}) to answer 200, got {status} with \
             {plan}"
        );
        assert_name(
            case,
            member_text(&plan, "name"),
            row.plan_name,
            row.stack,
            PLAN_ID,
        );
        assert_eq!(
            plan.get("throttle")
                .and_then(|throttle| throttle.get("rateLimit"))
                .and_then(Value::as_f64),
            Some(RATE_LIMIT),
            "case {case:?}: expected the declared rate limit, got {plan}"
        );
        assert_eq!(
            plan.get("throttle")
                .and_then(|throttle| throttle.get("burstLimit"))
                .and_then(Value::as_u64),
            Some(BURST_LIMIT),
            "case {case:?}: expected the declared burst limit, got {plan}"
        );
        assert_eq!(
            plan.get("quota")
                .and_then(|quota| quota.get("limit"))
                .and_then(Value::as_u64),
            Some(QUOTA_LIMIT),
            "case {case:?}: expected the declared quota limit, got {plan}"
        );
        assert_eq!(
            plan.get("quota")
                .and_then(|quota| quota.get("period"))
                .and_then(Value::as_str),
            Some(QUOTA_PERIOD),
            "case {case:?}: expected the declared quota period, got {plan}"
        );
        // The plan meters the STAGE the template named, by the api id that service assigned.
        let stages = plan
            .get("apiStages")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let metered: Vec<(Option<String>, Option<String>)> = stages
            .iter()
            .map(|entry| (member_text(entry, "apiId"), member_text(entry, "stage")))
            .collect();
        assert_eq!(
            metered,
            vec![(Some(api_id.clone()), Some(STAGE.to_string()))],
            "case {case:?}: expected the plan to meter the deployed stage, got {plan}"
        );

        // And the KEY under its own id, enabled as declared and named by the same rule.
        let (status, key) = apigateway(&router, "GET", &format!("/apikeys/{key_id}")).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected GetApiKey({key_id}) to answer 200, got {status} with {key}"
        );
        assert_name(
            case,
            member_text(&key, "name"),
            row.key_name,
            row.stack,
            KEY_ID,
        );
        assert_eq!(
            key.get("enabled").and_then(Value::as_bool),
            Some(true),
            "case {case:?}: expected the key to be enabled as declared, got {key}"
        );

        // The ASSOCIATION really puts that key in that plan, which is what the plan's own
        // collection read answers.
        let keys = collection(&router, case, &format!("/usageplans/{plan_id}/keys")).await;
        let held: Vec<(Option<String>, Option<String>)> = keys
            .iter()
            .map(|entry| (member_text(entry, "id"), member_text(entry, "type")))
            .collect();
        assert_eq!(
            held,
            vec![(Some(key_id.clone()), Some("API_KEY".to_string()))],
            "case {case:?}: expected the plan to hold the key the stack created, got {keys:?}"
        );

        // Both records are the ACCOUNT's, so each is in that service's own collection read of
        // its kind — a resource that exists only in the stack's records would be in neither.
        let plans = collection(&router, case, "/usageplans").await;
        assert!(
            plans
                .iter()
                .any(|entry| member_text(entry, "id").as_deref() == Some(plan_id.as_str())),
            "case {case:?}: expected GetUsagePlans to list the plan the stack created, got \
             {plans:?}"
        );
        let listed = collection(&router, case, "/apikeys").await;
        assert!(
            listed
                .iter()
                .any(|entry| member_text(entry, "id").as_deref() == Some(key_id.as_str())),
            "case {case:?}: expected GetApiKeys to list the key the stack created, got {listed:?}"
        );
    }
}
