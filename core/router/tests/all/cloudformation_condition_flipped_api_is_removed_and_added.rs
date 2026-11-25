// SPDX-License-Identifier: CC0-1.0

//! A stack update that FLIPS a template `Condition` plans, and carries out, the removal of
//! the resources that leave the stack's resource set and the addition of the ones that enter
//! it.
//!
//! The rule under test: **the resource set a revision declares is the logical ids whose
//! `Condition` HOLDS for that revision ("the optional `Conditions` section … controls whether
//! certain resources are created"), so an update's plan reports `Action` `Remove` for a
//! logical id that leaves that set and `Action` `Add` for one that enters it — exactly as it
//! does for a logical id a template stops or starts declaring outright — and nothing at all
//! for one whose condition holds on both sides; a resource whose create-only property is
//! repointed at a logical id the same plan ADDS is a `Modify` whose detail is
//! `RequiresRecreation` `Always` with `Evaluation` `Static`, because the plan itself decides
//! the physical id that reference will resolve to, and so `Replacement` `True`. Executing
//! that plan deletes the resources it reported `Remove` for in the service that owns them and
//! creates the added ones there, so the deleted api is refused by its own read and the route
//! the update repointed lives on the new api.** Two crates have to agree for that to hold
//! (CloudFormation computes and executes the plan, API Gateway v2 holds the resources), so
//! the suite lives here rather than in either of them.
//!
//! The model is what each half is read from: `ChangeAction` is `Add | Modify | Remove |
//! Import | Dynamic | SyncWithActual`, `Replacement` is `True | False | Conditional`,
//! `ResourceTargetDefinition$RequiresRecreation` is `Never | Always | Conditionally` and
//! `EvaluationType` is `Static | Dynamic` ("for `Static` evaluations, CloudFormation can
//! determine that the target value will change, and its value"); `GetApi` answers
//! `NotFoundException` / 404 for an api the account does not hold, and `GetRoutes` lists the
//! routes of the api it names.
//!
//! The rows differ along what the rule generalises over: which way the flip goes (so each
//! logical id is once the one that LEAVES and once the one that ENTERS the set), how many
//! resources leave and enter at once, and the TYPE of the flipped resource (an
//! `AWS::ApiGatewayV2::Api`, whose identifier that service mints, and an
//! `AWS::ApiGatewayV2::Stage`, whose identifier is the name the declaration states). The
//! dependents the flip repoints span three types of their own (an integration, a route and a
//! stage), and one unconditional api is in every revision so that a row can assert the plan
//! says nothing about it.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::cloudformation_apigatewayv2_resources::{
    apigatewayv2, build_router, cloudformation, REGION,
};
use crate::query_wire::{blocks_of, elements, opt_element, outputs_of};

/// The account id every identifier this emulator mints belongs to, which is the account half
/// of the sentence `GetApi` refuses an unknown api with.
const ACCOUNT_ID: &str = "000000000000";

/// The parameter whose value decides which of the two condition-guarded revisions is
/// deployed, and its two values.
const PHASE: &str = "Phase";
const FIRST: &str = "first";
const SECOND: &str = "second";

/// The conditions the template declares, one per phase.
const ON_FIRST: &str = "OnFirst";
const ON_SECOND: &str = "OnSecond";

/// The logical ids the template declares.
const FIRST_API: &str = "FirstApi";
const SECOND_API: &str = "SecondApi";
const KEPT_API: &str = "KeptApi";
const INTEGRATION: &str = "EchoIntegration";
const ROUTE: &str = "RouteA";
const DEFAULT_STAGE: &str = "DefaultStage";
const FIRST_ONLY_STAGE: &str = "FirstOnlyStage";

/// The `Outputs` entries each revision publishes.
const ACTIVE_API_OUTPUT: &str = "ActiveApiId";
const KEPT_API_OUTPUT: &str = "KeptApiId";
const ROUTE_OUTPUT: &str = "RouteId";

/// The route every revision serves, and the two stage names.
const ROUTE_KEY: &str = "GET /a";
const DEFAULT_STAGE_NAME: &str = "$default";
const FIRST_ONLY_STAGE_NAME: &str = "legacy";

/// The statuses the two stack operations settle on.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";
const UPDATE_COMPLETE: &str = "UPDATE_COMPLETE";

/// The `ChangeAction` values this plan carries, and the `Replacement`,
/// `RequiresRecreation` and `Evaluation` values it reads off a detail.
const ADD: &str = "Add";
const MODIFY: &str = "Modify";
const REMOVE: &str = "Remove";
const REPLACEMENT_TRUE: &str = "True";
const ALWAYS: &str = "Always";
const STATIC: &str = "Static";

/// The property every dependent is repointed through: the api each of them lives in, which
/// API Gateway v2 fixes when the resource is created.
const API_ID_PROPERTY: &str = "ApiId";

/// The error code API Gateway v2 answers for an api the account does not hold.
const NOT_FOUND: &str = "NotFoundException";

/// How often a clock-owed transition is looked at again, and how long the look goes on for.
/// The deadline is generous: the condition decides the outcome, never the host's speed.
const LOOK: Duration = Duration::from_millis(50);
const LOOK_DEADLINE: Duration = Duration::from_secs(90);

/// The api the dependents of the deployed revision live in: the one the phase's own condition
/// admits.
fn active_api() -> Value {
    json!({ "Fn::If": [ON_SECOND, { "Ref": SECOND_API }, { "Ref": FIRST_API }] })
}

/// The one template every row deploys, under one phase and then the other: two
/// condition-guarded apis, one unconditional api, one condition-guarded stage, and three
/// dependents whose `ApiId` follows whichever api the phase admits.
fn template(stack: &str) -> String {
    json!({
        "Parameters": {
            PHASE: { "Type": "String", "AllowedValues": [FIRST, SECOND] },
        },
        "Conditions": {
            ON_FIRST: { "Fn::Equals": [{ "Ref": PHASE }, FIRST] },
            ON_SECOND: { "Fn::Equals": [{ "Ref": PHASE }, SECOND] },
        },
        "Resources": {
            FIRST_API: {
                "Type": "AWS::ApiGatewayV2::Api",
                "Condition": ON_FIRST,
                "Properties": { "Name": format!("{stack}-first"), "ProtocolType": "HTTP" },
            },
            SECOND_API: {
                "Type": "AWS::ApiGatewayV2::Api",
                "Condition": ON_SECOND,
                "Properties": { "Name": format!("{stack}-second"), "ProtocolType": "HTTP" },
            },
            KEPT_API: {
                "Type": "AWS::ApiGatewayV2::Api",
                "Properties": { "Name": format!("{stack}-kept"), "ProtocolType": "HTTP" },
            },
            INTEGRATION: {
                "Type": "AWS::ApiGatewayV2::Integration",
                "Properties": {
                    "ApiId": active_api(),
                    "IntegrationType": "HTTP_PROXY",
                    "IntegrationMethod": "GET",
                    "IntegrationUri": "https://upstream.example.com/a",
                    "PayloadFormatVersion": "1.0",
                },
            },
            ROUTE: {
                "Type": "AWS::ApiGatewayV2::Route",
                "Properties": {
                    "ApiId": active_api(),
                    "RouteKey": ROUTE_KEY,
                    "Target": { "Fn::Join": ["/", ["integrations", { "Ref": INTEGRATION }]] },
                },
            },
            DEFAULT_STAGE: {
                "Type": "AWS::ApiGatewayV2::Stage",
                "Properties": {
                    "ApiId": active_api(),
                    "StageName": DEFAULT_STAGE_NAME,
                    "AutoDeploy": true,
                },
            },
            FIRST_ONLY_STAGE: {
                "Type": "AWS::ApiGatewayV2::Stage",
                "Condition": ON_FIRST,
                "Properties": {
                    "ApiId": active_api(),
                    "StageName": FIRST_ONLY_STAGE_NAME,
                    "AutoDeploy": true,
                },
            },
        },
        "Outputs": {
            ACTIVE_API_OUTPUT: { "Value": active_api() },
            KEPT_API_OUTPUT: { "Value": { "Ref": KEPT_API } },
            ROUTE_OUTPUT: { "Value": { "Ref": ROUTE } },
        },
    })
    .to_string()
}

/// One `ResourceChangeDetail`, flattened to the three members this suite reads.
#[derive(Debug, PartialEq, Eq)]
struct Detail {
    property: Option<String>,
    requires_recreation: Option<String>,
    evaluation: Option<String>,
}

/// One `ResourceChange` as `DescribeChangeSet` reports it.
#[derive(Debug)]
struct ReportedChange {
    action: String,
    physical_resource_id: Option<String>,
    replacement: Option<String>,
    details: Vec<Detail>,
}

/// Every `ResourceChange` an answer carries, keyed by the logical id it is about — the plan as
/// a client reading it by logical id sees it.
fn plan_of(body: &str) -> BTreeMap<String, ReportedChange> {
    blocks_of(body, "ResourceChange")
        .into_iter()
        .map(|change| {
            let logical = opt_element(change, "LogicalResourceId").unwrap_or_default();
            let details = blocks_of(change, "Details")
                .first()
                .map(|details| {
                    blocks_of(details, "member")
                        .into_iter()
                        .map(|entry| Detail {
                            property: opt_element(entry, "Name"),
                            requires_recreation: opt_element(entry, "RequiresRecreation"),
                            evaluation: opt_element(entry, "Evaluation"),
                        })
                        .collect()
                })
                .unwrap_or_default();
            (
                logical,
                ReportedChange {
                    action: opt_element(change, "Action").unwrap_or_default(),
                    physical_resource_id: opt_element(change, "PhysicalResourceId"),
                    replacement: opt_element(change, "Replacement"),
                    details,
                },
            )
        })
        .collect()
}

/// The `Parameters.member.1.*` pair one phase is submitted as.
fn phase_members(phase: &str) -> Vec<(String, String)> {
    vec![
        (
            "Parameters.member.1.ParameterKey".to_string(),
            PHASE.to_string(),
        ),
        (
            "Parameters.member.1.ParameterValue".to_string(),
            phase.to_string(),
        ),
    ]
}

/// Call one CloudFormation operation with owned members, which is what a request carrying a
/// template body and a parameter value is built as.
async fn call(
    router: &MultiServiceRouter,
    action: &str,
    members: &[(String, String)],
) -> (u16, String) {
    let borrowed: Vec<(&str, &str)> = members
        .iter()
        .map(|(key, value)| (key.as_str(), value.as_str()))
        .collect();
    cloudformation(router, action, &borrowed).await
}

/// Poll `DescribeStacks` until the stack reports a status that is no operation in flight, and
/// answer that answer.
async fn settled_stack(router: &MultiServiceRouter, case: &str, stack: &str) -> String {
    let deadline = Instant::now() + LOOK_DEADLINE;
    loop {
        let (status, body) =
            cloudformation(router, "DescribeStacks", &[("StackName", stack)]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from DescribeStacks, got {status} with {body}"
        );
        let reported = elements(&body, "StackStatus")
            .into_iter()
            .next()
            .unwrap_or_default();
        if !reported.is_empty() && !reported.ends_with("_IN_PROGRESS") {
            return body;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected {stack:?} to settle, last saw {reported:?}"
        );
        tokio::time::sleep(LOOK).await;
    }
}

/// Poll `DescribeChangeSet` until the change set has settled, and answer that answer.
async fn settled_change_set(
    router: &MultiServiceRouter,
    case: &str,
    stack: &str,
    change_set: &str,
) -> String {
    let deadline = Instant::now() + LOOK_DEADLINE;
    loop {
        let (status, body) = cloudformation(
            router,
            "DescribeChangeSet",
            &[("ChangeSetName", change_set), ("StackName", stack)],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from DescribeChangeSet, got {status} with {body}"
        );
        let reported = elements(&body, "Status")
            .into_iter()
            .next()
            .unwrap_or_default();
        if reported == "CREATE_COMPLETE" || reported == "FAILED" {
            return body;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the change set {change_set:?} to settle, last saw \
             {reported:?}"
        );
        tokio::time::sleep(LOOK).await;
    }
}

/// The members of an answered collection (`items`), as a list.
fn items(answered: &Value) -> Vec<&Value> {
    answered
        .get("items")
        .and_then(Value::as_array)
        .map(|items| items.iter().collect())
        .unwrap_or_default()
}

/// What the `$default` stage of `api_id` is in a condition to SERVE, read through the service
/// that owns it: the deployment statuses the api holds, and whether that stage auto deploys
/// and is bound to a deployment at all. An api an update ADDED has to be in the same
/// condition as the one a create of the same revision left behind, or the stack reports a
/// resource it did not really bring up.
///
/// The statuses are the SET of them: how many deployments a revision released depends on how
/// many route and stage changes it carried on the way, which is no part of whether the stage
/// serves.
async fn serving_condition(
    router: &MultiServiceRouter,
    case: &str,
    api_id: &str,
) -> (Vec<String>, Option<bool>, bool) {
    let (status, deployments) = apigatewayv2(
        router,
        "GET",
        &format!("/v2/apis/{api_id}/deployments"),
        None,
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected 200 from GetDeployments on {api_id}, got {status} with \
         {deployments}"
    );
    let mut statuses: Vec<String> = items(&deployments)
        .iter()
        .filter_map(|item| item.get("deploymentStatus").and_then(Value::as_str))
        .map(ToString::to_string)
        .collect();
    statuses.sort();
    statuses.dedup();
    let (status, stages) =
        apigatewayv2(router, "GET", &format!("/v2/apis/{api_id}/stages"), None).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected 200 from GetStages on {api_id}, got {status} with {stages}"
    );
    let bound = items(&stages)
        .into_iter()
        .find(|item| {
            item.get("stageName").and_then(Value::as_str) == Some(DEFAULT_STAGE_NAME)
        })
        .map(|stage| {
            (
                stage.get("autoDeploy").and_then(Value::as_bool),
                stage.get("deploymentId").is_some(),
            )
        });
    (
        statuses,
        bound.and_then(|(auto, _)| auto),
        bound.is_some_and(|(_, bound)| bound),
    )
}

/// The error code an API Gateway v2 refusal carries, however the envelope spells it.
fn error_code(body: &Value) -> Option<String> {
    body.get("__type")
        .or_else(|| body.get("code"))
        .and_then(Value::as_str)
        .map(|code| code.rsplit('#').next().unwrap_or(code).to_string())
}

/// One row: which phase the stack is created under and which one the update moves it to, and
/// the logical ids that leave and enter the resource set on the way.
struct Row {
    case: &'static str,
    from: &'static str,
    to: &'static str,
    /// The logical ids the update takes OUT of the stack, each with the physical resource the
    /// plan must report it acts on — read from the stack, never from the answer being checked.
    leaving: &'static [&'static str],
    /// The logical ids the update brings INTO the stack.
    entering: &'static [&'static str],
    /// The stage names the api the update settles on must hold once it is done.
    stages_after: &'static [&'static str],
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "the condition that admitted the deployed api turns false and another's \
                   turns true: two resources leave and one enters",
            from: FIRST,
            to: SECOND,
            leaving: &[FIRST_API, FIRST_ONLY_STAGE],
            entering: &[SECOND_API],
            stages_after: &[DEFAULT_STAGE_NAME],
        },
        Row {
            case: "the same flip the other way: one resource leaves and two enter, one of \
                   them a stage whose identifier is the name it declares",
            from: SECOND,
            to: FIRST,
            leaving: &[SECOND_API],
            entering: &[FIRST_API, FIRST_ONLY_STAGE],
            stages_after: &[DEFAULT_STAGE_NAME, FIRST_ONLY_STAGE_NAME],
        },
    ]
}

/// The dependents the flip repoints, in the order their logical ids sort.
const REPOINTED: &[&str] = &[DEFAULT_STAGE, INTEGRATION, ROUTE];

#[tokio::test]
async fn a_condition_flip_plans_and_carries_out_the_removal_and_the_addition() {
    for (index, row) in rows().iter().enumerate() {
        let case = row.case;
        let router = build_router();
        let stack = format!("cfn-apigwv2-condition-flip-{index}");
        let body = template(&stack);

        // The revision the row starts from.
        let mut members = vec![
            ("StackName".to_string(), stack.clone()),
            ("TemplateBody".to_string(), body.clone()),
        ];
        members.extend(phase_members(row.from));
        let (status, answered) = call(&router, "CreateStack", &members).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from CreateStack, got {status} with {answered}"
        );
        let described = settled_stack(&router, case, &stack).await;
        assert_eq!(
            elements(&described, "StackStatus")
                .into_iter()
                .next()
                .unwrap_or_default(),
            CREATE_COMPLETE,
            "case {case:?}: expected the stack to reach {CREATE_COMPLETE}, got {described}"
        );
        let outputs = outputs_of(&described);
        let api_before = outputs.get(ACTIVE_API_OUTPUT).cloned().unwrap_or_else(|| {
            panic!("case {case:?}: expected an {ACTIVE_API_OUTPUT} output in {outputs:?}")
        });
        let kept_api = outputs.get(KEPT_API_OUTPUT).cloned().unwrap_or_else(|| {
            panic!("case {case:?}: expected a {KEPT_API_OUTPUT} output in {outputs:?}")
        });
        let route_before = outputs.get(ROUTE_OUTPUT).cloned().unwrap_or_else(|| {
            panic!("case {case:?}: expected a {ROUTE_OUTPUT} output in {outputs:?}")
        });
        // The condition the api the CREATE left behind is in to serve its route, which the
        // api the update ADDS has to be in too.
        let serving_before = serving_condition(&router, case, &api_before).await;

        // What the stack HOLDS under each logical id, which is what a `Remove` must report as
        // the physical resource it acts on.
        let (status, resources) =
            cloudformation(&router, "DescribeStackResources", &[("StackName", &stack)]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from DescribeStackResources, got {status} with \
             {resources}"
        );
        let held = crate::query_wire::physical_ids_of(&resources);

        // ---- the plan ----
        let change_set = format!("flip-to-{}", row.to);
        let mut members = vec![
            ("StackName".to_string(), stack.clone()),
            ("ChangeSetName".to_string(), change_set.clone()),
            ("ChangeSetType".to_string(), "UPDATE".to_string()),
            ("TemplateBody".to_string(), body.clone()),
        ];
        members.extend(phase_members(row.to));
        let (status, answered) = call(&router, "CreateChangeSet", &members).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from CreateChangeSet, got {status} with \
             {answered}"
        );
        let settled = settled_change_set(&router, case, &stack, &change_set).await;
        assert_eq!(
            elements(&settled, "Status").into_iter().next().unwrap_or_default(),
            "CREATE_COMPLETE",
            "case {case:?}: expected the change set to be created, got {settled}"
        );
        let plan = plan_of(&settled);

        for logical in row.leaving {
            let change = plan.get(*logical).unwrap_or_else(|| {
                panic!(
                    "case {case:?}: expected a change for {logical}, the plan reports {:?}",
                    plan.keys().collect::<Vec<_>>()
                )
            });
            assert_eq!(
                change.action, REMOVE,
                "case {case:?}: expected {REMOVE} for {logical}, got {change:?}"
            );
            assert_eq!(
                change.physical_resource_id.as_deref(),
                held.get(*logical).map(String::as_str),
                "case {case:?}: expected the {REMOVE} of {logical} to act on the resource \
                 the stack holds, got {change:?}"
            );
        }
        for logical in row.entering {
            let change = plan.get(*logical).unwrap_or_else(|| {
                panic!(
                    "case {case:?}: expected a change for {logical}, the plan reports {:?}",
                    plan.keys().collect::<Vec<_>>()
                )
            });
            assert_eq!(
                change.action, ADD,
                "case {case:?}: expected {ADD} for {logical}, got {change:?}"
            );
            // "Resources that you are adding don't have physical IDs because they haven't
            // been created."
            assert_eq!(
                change.physical_resource_id, None,
                "case {case:?}: an {ADD} names no physical resource, got {change:?}"
            );
        }
        for logical in REPOINTED {
            let change = plan.get(*logical).unwrap_or_else(|| {
                panic!(
                    "case {case:?}: expected a change for {logical}, the plan reports {:?}",
                    plan.keys().collect::<Vec<_>>()
                )
            });
            assert_eq!(
                change.action, MODIFY,
                "case {case:?}: expected {MODIFY} for {logical}, got {change:?}"
            );
            assert_eq!(
                change.replacement.as_deref(),
                Some(REPLACEMENT_TRUE),
                "case {case:?}: the api {logical} lives in is fixed when it is created and \
                 the plan decides the new one's id, so expected Replacement \
                 {REPLACEMENT_TRUE}, got {change:?}"
            );
            let expected = Detail {
                property: Some(API_ID_PROPERTY.to_string()),
                requires_recreation: Some(ALWAYS.to_string()),
                evaluation: Some(STATIC.to_string()),
            };
            assert!(
                change.details.contains(&expected),
                "case {case:?}: expected {logical} to carry the detail {expected:?}, got \
                 {change:?}"
            );
        }
        assert!(
            !plan.contains_key(KEPT_API),
            "case {case:?}: {KEPT_API}'s condition holds on both sides and its declaration \
             is unchanged, so the plan says nothing about it; got {plan:?}"
        );

        // ---- the execution ----
        let mut members = vec![
            ("StackName".to_string(), stack.clone()),
            ("TemplateBody".to_string(), body.clone()),
        ];
        members.extend(phase_members(row.to));
        let (status, answered) = call(&router, "UpdateStack", &members).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from UpdateStack, got {status} with {answered}"
        );
        let described = settled_stack(&router, case, &stack).await;
        assert_eq!(
            elements(&described, "StackStatus")
                .into_iter()
                .next()
                .unwrap_or_default(),
            UPDATE_COMPLETE,
            "case {case:?}: expected the stack to reach {UPDATE_COMPLETE}, got {described}"
        );
        let outputs = outputs_of(&described);
        let api_after = outputs.get(ACTIVE_API_OUTPUT).cloned().unwrap_or_else(|| {
            panic!("case {case:?}: expected an {ACTIVE_API_OUTPUT} output in {outputs:?}")
        });
        let route_after = outputs.get(ROUTE_OUTPUT).cloned().unwrap_or_else(|| {
            panic!("case {case:?}: expected a {ROUTE_OUTPUT} output in {outputs:?}")
        });
        assert_ne!(
            api_after, api_before,
            "case {case:?}: the api the stack settles on is the one the update ADDED, so its \
             id differs from the removed api's ({api_before})"
        );
        assert_ne!(
            route_after, route_before,
            "case {case:?}: the route lives in another api now, so it is another route \
             ({route_before})"
        );

        // The removed api is gone where it lived: refused by its own read, in the sentence
        // that read refuses an api the account does not hold.
        let (status, refused) =
            apigatewayv2(&router, "GET", &format!("/v2/apis/{api_before}"), None).await;
        assert_eq!(
            status, 404,
            "case {case:?}: expected 404 from GetApi on the removed api {api_before}, got \
             {status} with {refused}"
        );
        assert_eq!(
            error_code(&refused).as_deref(),
            Some(NOT_FOUND),
            "case {case:?}: expected {NOT_FOUND} for the removed api, got {refused}"
        );
        assert_eq!(
            refused.get("message").and_then(Value::as_str),
            Some(format!("Invalid API identifier specified {ACCOUNT_ID}:{api_before}").as_str()),
            "case {case:?}: the refusal names the account and the api asked for, got {refused}"
        );
        // ...and the api the update ADDED is in the same condition to serve as the one the
        // create left behind: the same deployment statuses, and a `$default` stage that auto
        // deploys and is bound to a deployment just as that one was.
        let serving_after = serving_condition(&router, case, &api_after).await;
        assert_eq!(
            serving_after, serving_before,
            "case {case:?}: expected the added api {api_after} to be in the same condition to \
             serve as the created api was ({serving_before:?}), got {serving_after:?}"
        );
        // ...and the api the update added really holds the route and the stages the revision
        // declares for it.
        let (status, routes) = apigatewayv2(
            &router,
            "GET",
            &format!("/v2/apis/{api_after}/routes"),
            None,
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected 200 from GetRoutes on {api_after}, got {status} with \
             {routes}"
        );
        let keys: Vec<String> = routes
            .get("items")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(|item| item.get("routeKey").and_then(Value::as_str))
                    .map(ToString::to_string)
                    .collect()
            })
            .unwrap_or_default();
        assert_eq!(
            keys,
            vec![ROUTE_KEY.to_string()],
            "case {case:?}: expected the new api to hold exactly {ROUTE_KEY:?}, got {routes}"
        );
        let (status, stages) = apigatewayv2(
            &router,
            "GET",
            &format!("/v2/apis/{api_after}/stages"),
            None,
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected 200 from GetStages on {api_after}, got {status} with \
             {stages}"
        );
        let mut names: Vec<String> = stages
            .get("items")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(|item| item.get("stageName").and_then(Value::as_str))
                    .map(ToString::to_string)
                    .collect()
            })
            .unwrap_or_default();
        names.sort();
        let mut expected: Vec<String> =
            row.stages_after.iter().map(|name| (*name).to_string()).collect();
        expected.sort();
        assert_eq!(
            names, expected,
            "case {case:?}: expected the new api to hold the stages {expected:?}, got {stages}"
        );

        // The resource no condition touched is untouched: the api both revisions declare is
        // still there, under the id the stack published for it.
        let (status, kept) =
            apigatewayv2(&router, "GET", &format!("/v2/apis/{kept_api}"), None).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the unconditional api {kept_api} to survive the update, \
             got {status} with {kept}"
        );
        assert_eq!(
            kept.get("apiId").and_then(Value::as_str),
            Some(kept_api.as_str()),
            "case {case:?}: expected {kept_api} to read back as itself in {REGION}, got {kept}"
        );
    }
}
