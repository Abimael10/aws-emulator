// SPDX-License-Identifier: CC0-1.0

//! `AWS::ApiGatewayV2::RoutingRule` end to end: the routing rule a stack declares exists on
//! the custom domain name it names, in Amazon API Gateway v2 itself, under the identity the
//! stack publishes.
//!
//! RULE: a resource type Amazon Web Services publishes is one a template may declare, and a
//! template declaring it is deployed by creating the resource through the owning service's own
//! API rather than refused as a type CloudFormation does not recognise — so the stack reaches
//! its terminal status, the identifier that service MINTED is what the stack publishes as the
//! resource's physical id, a changed declaration reaches the resource through the call that
//! service publishes for changing one, a changed value the service fixes when it makes the
//! resource replaces it, the stack's deletion deletes it there, and a declaration that service
//! REFUSES fails that resource with the refusal it answered rather than the whole template
//! being rejected. For a routing rule: `CreateRoutingRule`
//! (`POST /v2/domainnames/{DomainName}/routingrules`) takes the rule's priority, conditions
//! and actions and answers the id and the ARN it assigned, `PutRoutingRule` puts a whole rule
//! back under that id, `DeleteRoutingRule` removes it, and the domain name a rule lives on is
//! an `@httpLabel` of every one of those calls rather than a value of the rule.
//!
//! The rows differ along what the rule generalises over: WHICH condition a rule matches on
//! (base paths, headers, both at once), how many values a condition offers, how many members
//! an action carries, and the `Priority` — at the published MINIMUM, inside the range, and at
//! the published MAXIMUM of the member's own bounds
//! (`smithy_query(query="shape",shape="RoutingRulePriority")`: range 1..1000000). The refusal
//! half states a priority one past that maximum, which is the resource's OWN failure.
//!
//! Everything goes over the wire each service speaks through the one router `harness.rs`
//! builds — the CloudFormation calls over its awsQuery wire, the routing-rule reads over API
//! Gateway v2's own `restJson1` wire — so what is asserted is what a client that deploys the
//! template and then calls API Gateway v2 observes.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_cloudformation_cloudformation::state::status;

use crate::harness::{failed_resource_reason, quoted_and_class};
use crate::stack_lifecycle::{
    call, create_stack, member, output_value, router, settled_stack, xml_text, ACCOUNT_ID, REGION,
};

/// The published bounds of a routing rule's `Priority`, from the shape every routing-rule
/// writer's `Priority` targets (`smithy_query(query="shape",shape="RoutingRulePriority")`:
/// range 1..1000000) — the two ends a row states and the value one past the top that the
/// refusal half states.
const MIN_PRIORITY: i64 = 1;
const MAX_PRIORITY: i64 = 1_000_000;

/// The status a resource its provider could not create records, and the `HandlerErrorCode`
/// class a refusal of a request's own values falls in.
const CREATE_FAILED: &str = "CREATE_FAILED";
const INVALID_REQUEST: &str = "InvalidRequest";

/// The routing mode a custom domain name has to be in for its routing rules to decide which
/// api serves a request (`RoutingMode`: `API_MAPPING_ONLY | ROUTING_RULE_ONLY |
/// ROUTING_RULE_THEN_API_MAPPING`).
const ROUTING_RULE_ONLY: &str = "ROUTING_RULE_ONLY";

/// One Amazon API Gateway call over the `restJson1` wire both of its control planes declare,
/// on the method and URI the called operation's own `@http` binding names, signed for
/// `apigateway` in the Region the stack's resources live in.
///
/// The suite's own caller rather than the bodiless one `apigateway_vpc_link_resources.rs`
/// shares: the domain name this suite's rules live on is CREATED through that service's API,
/// which takes a request document.
async fn apigateway(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: Option<&Value>,
) -> (u16, Value) {
    let payload = match body {
        Some(document) => bytes::Bytes::from(serde_json::to_vec(document).expect("a request")),
        None => bytes::Bytes::new(),
    };
    let request = http::Request::builder()
        .method(method)
        .uri(format!("https://apigateway.{REGION}.amazonaws.com{uri}"))
        .header("host", format!("apigateway.{REGION}.amazonaws.com"))
        .header("content-type", "application/json")
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/apigateway/\
                 aws4_request, SignedHeaders=host, Signature=0123456789abcdef"
            ),
        )
        .body(payload)
        .expect("a well-formed Amazon API Gateway request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let raw = response.into_body();
    let parsed = if raw.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&raw).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// Create one custom domain name through API Gateway v2's OWN API and answer the ARN that
/// service reported for it — the handle a routing rule's declaration names its domain by.
///
/// The domain is created OUTSIDE the stack, which is the position a client is in: the custom
/// domain name and the certificate it presents are long-lived, and the routing rules that
/// decide which api answers on it are what a template deploys.
async fn custom_domain_name(router: &MultiServiceRouter, case: &str, domain_name: &str) -> String {
    let (code, document) = apigateway(
        router,
        "POST",
        "/v2/domainnames",
        Some(&json!({
            "domainName": domain_name,
            "routingMode": ROUTING_RULE_ONLY,
            "domainNameConfigurations": [{
                "certificateArn": format!(
                    "arn:aws:acm:{REGION}:{ACCOUNT_ID}:certificate/0123abcd-4567-89ef-0123-456789abcdef"
                ),
                "endpointType": "REGIONAL",
            }],
        })),
    )
    .await;
    assert_eq!(
        code, 201,
        "case {case:?}: CreateDomainName({domain_name}) must answer 201, got {code} with \
         {document}"
    );
    document
        .get("domainNameArn")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: no domainNameArn in {document}"))
        .to_string()
}

/// `GetRoutingRule` (`GET /v2/domainnames/{DomainName}/routingrules/{RoutingRuleId}`): the
/// status and the rule that domain name holds under that id.
async fn get_routing_rule(
    router: &MultiServiceRouter,
    domain_name: &str,
    rule_id: &str,
) -> (u16, Value) {
    apigateway(
        router,
        "GET",
        &format!("/v2/domainnames/{domain_name}/routingrules/{rule_id}"),
        None,
    )
    .await
}

/// `ListRoutingRules` (`GET /v2/domainnames/{DomainName}/routingrules`): the rules that domain
/// name holds, as its own collection read reports them.
async fn listed_routing_rules(
    router: &MultiServiceRouter,
    case: &str,
    domain_name: &str,
) -> Vec<Value> {
    let (code, document) = apigateway(
        router,
        "GET",
        &format!("/v2/domainnames/{domain_name}/routingrules"),
        None,
    )
    .await;
    assert_eq!(
        code, 200,
        "case {case:?}: ListRoutingRules({domain_name}) must answer 200, got {code} with \
         {document}"
    );
    document
        .get("routingRules")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

/// A one-rule template over `domain_name_arn`, publishing every value the type's "Return
/// values" section does: what `Ref` resolves to and both of its `Fn::GetAtt` attributes.
fn rule_template(domain_name_arn: &str, priority: i64, conditions: Value, actions: Value) -> String {
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": {
            "Rule": {
                "Type": "AWS::ApiGatewayV2::RoutingRule",
                "Properties": {
                    "DomainNameArn": domain_name_arn,
                    "Priority": priority,
                    "Conditions": conditions,
                    "Actions": actions,
                },
            },
        },
        "Outputs": {
            "RuleRef": { "Value": { "Ref": "Rule" } },
            "RuleArn": { "Value": { "Fn::GetAtt": ["Rule", "RoutingRuleArn"] } },
            "RuleId": { "Value": { "Fn::GetAtt": ["Rule", "RoutingRuleId"] } },
        },
    })
    .to_string()
}

/// The `PhysicalResourceId` the stack publishes for one of its logical resources.
async fn published_id(
    router: &MultiServiceRouter,
    case: &str,
    stack: &str,
    logical_id: &str,
) -> String {
    let (code, body) = call(
        router,
        "DescribeStackResource",
        &[
            member("StackName", stack),
            member("LogicalResourceId", logical_id),
        ],
    )
    .await;
    assert_eq!(
        code, 200,
        "case {case:?}: DescribeStackResource({logical_id}) must answer 200, got {code} {body}"
    );
    xml_text(&body, "PhysicalResourceId").unwrap_or_default()
}

/// Deploy one template and assert the create completed, answering the settled stack's body.
async fn deployed(router: &MultiServiceRouter, case: &str, stack: &str, template: &str) -> String {
    let (code, body) = create_stack(router, stack, template, &[]).await;
    assert_eq!(
        code, 200,
        "case {case:?}: CreateStack must answer 200, got {code} {body}"
    );
    let described = settled_stack(router, case, stack).await;
    assert_eq!(
        xml_text(&described, "StackStatus").as_deref(),
        Some(status::CREATE_COMPLETE),
        "case {case:?}: expected the stack to complete, got {described}"
    );
    described
}

/// The ARN a routing rule of `domain_name` carries under `rule_id`, as the service that owns
/// the resource builds one: API Gateway's resource path for the rule, under this emulator's
/// own account and the Region the domain lives in.
fn expected_rule_arn(domain_name: &str, rule_id: &str) -> String {
    format!(
        "arn:aws:apigateway:{REGION}:{ACCOUNT_ID}:/domainnames/{domain_name}/routingrules/{rule_id}"
    )
}

/// One row of the create table.
struct Row {
    case: &'static str,
    stack: &'static str,
    domain_name: &'static str,
    priority: i64,
    conditions: fn() -> Value,
    actions: fn() -> Value,
}

/// One base path, one value to match, one action with only its required members.
fn one_base_path() -> Value {
    json!([{ "MatchBasePaths": { "AnyOf": ["/orders"] } }])
}

/// Several values on ONE base-path condition: a rule matches if any of them does.
fn several_base_paths() -> Value {
    json!([{ "MatchBasePaths": { "AnyOf": ["/orders", "/orders/v2", "/baskets"] } }])
}

/// A HEADER condition, which is the other alternative a condition offers.
fn header_condition() -> Value {
    json!([{
        "MatchHeaders": {
            "AnyOf": [{ "Header": "x-tenant", "ValueGlob": "acme-*" }],
        },
    }])
}

/// Both alternatives at once, in separate conditions — the form the service's own bounds admit
/// (at most one `MatchBasePaths` condition and two `MatchHeaders` conditions).
fn base_path_and_headers() -> Value {
    json!([
        { "MatchBasePaths": { "AnyOf": ["/orders"] } },
        {
            "MatchHeaders": {
                "AnyOf": [
                    { "Header": "x-tenant", "ValueGlob": "acme-*" },
                    { "Header": "x-channel", "ValueGlob": "web" },
                ],
            },
        },
    ])
}

/// An action with the two members it requires.
fn invoke_api() -> Value {
    json!([{ "InvokeApi": { "ApiId": "a1b2c3d4e5", "Stage": "prod" } }])
}

/// The same action with its optional member stated as well.
fn invoke_api_stripping_base_path() -> Value {
    json!([{
        "InvokeApi": { "ApiId": "f6g7h8i9j0", "Stage": "$default", "StripBasePath": true },
    }])
}

const ROWS: &[Row] = &[
    Row {
        case: "one base path, at the published minimum priority",
        stack: "apigwv2-rule-base-path",
        domain_name: "rules-base-path.example.com",
        priority: MIN_PRIORITY,
        conditions: one_base_path,
        actions: invoke_api,
    },
    Row {
        case: "several base paths on one condition, mid-range priority",
        stack: "apigwv2-rule-several-paths",
        domain_name: "rules-several-paths.example.com",
        priority: 500,
        conditions: several_base_paths,
        actions: invoke_api_stripping_base_path,
    },
    Row {
        case: "a header condition",
        stack: "apigwv2-rule-headers",
        domain_name: "rules-headers.example.com",
        priority: 42,
        conditions: header_condition,
        actions: invoke_api,
    },
    Row {
        case: "a base path and headers at once, at the published maximum priority",
        stack: "apigwv2-rule-both",
        domain_name: "rules-both.example.com",
        priority: MAX_PRIORITY,
        conditions: base_path_and_headers,
        actions: invoke_api_stripping_base_path,
    },
];

/// The members of a rule as API Gateway v2 reports it, under the names that model spells them
/// with — which is what a declared property becomes.
fn reported(rule: &Value, member: &str) -> Value {
    rule.get(member).cloned().unwrap_or(Value::Null)
}

/// What one declared document becomes on the wire: every object key with its initial letter
/// lower-cased, which is the one difference between this type's property names and the members
/// of the calls they are (`MatchBasePaths` -> `matchBasePaths`).
fn as_members(declared: &Value) -> Value {
    match declared {
        Value::Object(members) => Value::Object(
            members
                .iter()
                .map(|(key, value)| {
                    let mut characters = key.chars();
                    let member = match characters.next() {
                        Some(first) => {
                            first.to_lowercase().collect::<String>() + characters.as_str()
                        }
                        None => String::new(),
                    };
                    (member, as_members(value))
                })
                .collect(),
        ),
        Value::Array(items) => Value::Array(items.iter().map(as_members).collect()),
        scalar => scalar.clone(),
    }
}

#[tokio::test]
async fn a_declared_routing_rule_is_created_in_api_gateway_v2() {
    for row in ROWS {
        let case = row.case;
        let router = router();
        let domain_name_arn = custom_domain_name(&router, case, row.domain_name).await;
        let conditions = (row.conditions)();
        let actions = (row.actions)();
        let described = deployed(
            &router,
            case,
            row.stack,
            &rule_template(
                &domain_name_arn,
                row.priority,
                conditions.clone(),
                actions.clone(),
            ),
        )
        .await;

        // The identity the stack publishes is the one API Gateway v2 minted: the rule's own
        // ARN, which `Ref` resolves to and which the physical id records, with the id it
        // carries published beside it.
        let rule_id = output_value(&described, "RuleId").unwrap_or_default();
        let physical_id = published_id(&router, case, row.stack, "Rule").await;
        let expected_arn = expected_rule_arn(row.domain_name, &rule_id);
        assert_eq!(
            physical_id, expected_arn,
            "case {case:?}: expected the rule's own ARN as its physical id, got \
             {physical_id:?}"
        );
        assert_eq!(
            output_value(&described, "RuleRef").as_deref(),
            Some(expected_arn.as_str()),
            "case {case:?}: expected Ref to resolve to the rule's ARN, got {described}"
        );
        assert_eq!(
            output_value(&described, "RuleArn").as_deref(),
            Some(expected_arn.as_str()),
            "case {case:?}: expected the RoutingRuleArn attribute to carry it too, got \
             {described}"
        );

        // ...and the rule really is on that domain name, with the rule the template declared.
        let (code, held) = get_routing_rule(&router, row.domain_name, &rule_id).await;
        assert_eq!(
            code, 200,
            "case {case:?}: expected GetRoutingRule({rule_id}) to answer 200, got {code} with \
             {held}"
        );
        assert_eq!(
            reported(&held, "priority"),
            json!(row.priority),
            "case {case:?}: expected the declared priority, got {held}"
        );
        assert_eq!(
            reported(&held, "conditions"),
            as_members(&conditions),
            "case {case:?}: expected the declared conditions, got {held}"
        );
        assert_eq!(
            reported(&held, "actions"),
            as_members(&actions),
            "case {case:?}: expected the declared actions, got {held}"
        );
        let listed = listed_routing_rules(&router, case, row.domain_name).await;
        assert!(
            listed
                .iter()
                .any(|rule| reported(rule, "routingRuleId") == json!(rule_id)),
            "case {case:?}: expected the domain name's own listing to carry the rule \
             {rule_id:?}, got {listed:?}"
        );
    }
}

#[tokio::test]
async fn a_changed_rule_is_put_over_the_one_the_stack_created() {
    // The other half of the rule: `PutRoutingRule` takes every property of the type but the
    // domain name that addresses it, so a changed priority, condition or action is applied to
    // the rule the stack created — which keeps the ARN and the id API Gateway v2 minted.
    let case = "a priority, a condition and an action all re-valued";
    let stack = "apigwv2-rule-updated";
    let domain_name = "rules-updated.example.com";
    let router = router();
    let domain_name_arn = custom_domain_name(&router, case, domain_name).await;
    let described = deployed(
        &router,
        case,
        stack,
        &rule_template(&domain_name_arn, 10, one_base_path(), invoke_api()),
    )
    .await;
    let rule_id = output_value(&described, "RuleId").unwrap_or_default();
    let created_id = published_id(&router, case, stack, "Rule").await;

    let revised_conditions = header_condition();
    let revised_actions = invoke_api_stripping_base_path();
    let (code, body) = call(
        &router,
        "UpdateStack",
        &[
            member("StackName", stack),
            member(
                "TemplateBody",
                &rule_template(
                    &domain_name_arn,
                    99,
                    revised_conditions.clone(),
                    revised_actions.clone(),
                ),
            ),
        ],
    )
    .await;
    assert_eq!(
        code, 200,
        "case {case:?}: UpdateStack must answer 200, got {code} {body}"
    );
    let described = settled_stack(&router, case, stack).await;
    assert_eq!(
        xml_text(&described, "StackStatus").as_deref(),
        Some(status::UPDATE_COMPLETE),
        "case {case:?}: expected the update to complete, got {described}"
    );
    let updated_id = published_id(&router, case, stack, "Rule").await;
    assert_eq!(
        updated_id, created_id,
        "case {case:?}: expected the rule to keep its identity, got {updated_id:?}"
    );
    let (code, held) = get_routing_rule(&router, domain_name, &rule_id).await;
    assert_eq!(
        code, 200,
        "case {case:?}: expected GetRoutingRule({rule_id}) to answer 200, got {code} with {held}"
    );
    assert_eq!(
        reported(&held, "priority"),
        json!(99),
        "case {case:?}: expected the revised priority, got {held}"
    );
    assert_eq!(
        reported(&held, "conditions"),
        as_members(&revised_conditions),
        "case {case:?}: expected the revised conditions, got {held}"
    );
    assert_eq!(
        reported(&held, "actions"),
        as_members(&revised_actions),
        "case {case:?}: expected the revised actions, got {held}"
    );
    assert_eq!(
        listed_routing_rules(&router, case, domain_name).await.len(),
        1,
        "case {case:?}: expected the one rule the stack declares, not a second one"
    );
}

#[tokio::test]
async fn a_rule_declared_on_another_domain_name_replaces_the_resource() {
    // The domain name a rule lives on is fixed when API Gateway v2 makes the rule — it
    // addresses the rule rather than being a value of it, and that service publishes no call
    // that moves one — so a declaration naming another domain asks for a DIFFERENT rule: the
    // stack creates it there and the one it replaced is gone.
    let case = "the domain name the rule is on, re-declared";
    let stack = "apigwv2-rule-replaced";
    let first = "rules-replaced-first.example.com";
    let second = "rules-replaced-second.example.com";
    let router = router();
    let first_arn = custom_domain_name(&router, case, first).await;
    let second_arn = custom_domain_name(&router, case, second).await;
    let described = deployed(
        &router,
        case,
        stack,
        &rule_template(&first_arn, 7, one_base_path(), invoke_api()),
    )
    .await;
    let created_id = output_value(&described, "RuleId").unwrap_or_default();

    let (code, body) = call(
        &router,
        "UpdateStack",
        &[
            member("StackName", stack),
            member(
                "TemplateBody",
                &rule_template(&second_arn, 7, one_base_path(), invoke_api()),
            ),
        ],
    )
    .await;
    assert_eq!(
        code, 200,
        "case {case:?}: UpdateStack must answer 200, got {code} {body}"
    );
    let described = settled_stack(&router, case, stack).await;
    assert_eq!(
        xml_text(&described, "StackStatus").as_deref(),
        Some(status::UPDATE_COMPLETE),
        "case {case:?}: expected the update to complete, got {described}"
    );
    let replacement_id = output_value(&described, "RuleId").unwrap_or_default();
    let physical_id = published_id(&router, case, stack, "Rule").await;
    assert_eq!(
        physical_id,
        expected_rule_arn(second, &replacement_id),
        "case {case:?}: expected the replacement's own ARN on the second domain name, got \
         {physical_id:?}"
    );
    let (code, held) = get_routing_rule(&router, second, &replacement_id).await;
    assert_eq!(
        code, 200,
        "case {case:?}: expected the replacement on {second}, got {code} with {held}"
    );
    let left_behind = listed_routing_rules(&router, case, first).await;
    assert!(
        left_behind.is_empty(),
        "case {case:?}: expected the replaced rule {created_id:?} to be deleted from {first}, \
         got {left_behind:?}"
    );
}

#[tokio::test]
async fn deleting_the_stack_deletes_the_rule() {
    let case = "a stack whose one resource is a routing rule";
    let stack = "apigwv2-rule-deleted";
    let domain_name = "rules-deleted.example.com";
    let router = router();
    let domain_name_arn = custom_domain_name(&router, case, domain_name).await;
    let described = deployed(
        &router,
        case,
        stack,
        &rule_template(&domain_name_arn, 3, one_base_path(), invoke_api()),
    )
    .await;
    let rule_id = output_value(&described, "RuleId").unwrap_or_default();

    let (code, body) = call(&router, "DeleteStack", &[member("StackName", stack)]).await;
    assert_eq!(
        code, 200,
        "case {case:?}: DeleteStack must answer 200, got {code} {body}"
    );
    let described = settled_stack(&router, case, stack).await;
    assert!(
        described.is_empty()
            || xml_text(&described, "StackStatus").as_deref() == Some(status::DELETE_COMPLETE),
        "case {case:?}: expected the deletion to complete, got {described}"
    );
    let (code, document) = get_routing_rule(&router, domain_name, &rule_id).await;
    assert_eq!(
        code, 404,
        "case {case:?}: expected the rule to be gone from API Gateway v2, got {code} with \
         {document}"
    );
    let left = listed_routing_rules(&router, case, domain_name).await;
    assert!(
        left.is_empty(),
        "case {case:?}: expected the domain name to hold no rule, got {left:?}"
    );
}

#[tokio::test]
async fn a_priority_outside_the_published_range_fails_the_resource() {
    // The template is still ACCEPTED — the type is one a template may declare — and the
    // refusal is the resource's own: API Gateway v2 refuses a `Priority` outside the range its
    // model publishes, which is reported on that resource's `CREATE_FAILED` event, and nothing
    // is created on the domain name.
    let case = "a priority one past the published maximum";
    let stack = "apigwv2-rule-over-maximum";
    let domain_name = "rules-over-maximum.example.com";
    let router = router();
    let domain_name_arn = custom_domain_name(&router, case, domain_name).await;
    let (code, body) = create_stack(
        &router,
        stack,
        &rule_template(
            &domain_name_arn,
            MAX_PRIORITY + 1,
            one_base_path(),
            invoke_api(),
        ),
        &[],
    )
    .await;
    assert_eq!(
        code, 200,
        "case {case:?}: CreateStack must answer 200 rather than refusing the template, got \
         {code} {body}"
    );
    let described = settled_stack(&router, case, stack).await;
    assert_eq!(
        xml_text(&described, "StackStatus").as_deref(),
        Some(status::ROLLBACK_COMPLETE),
        "case {case:?}: expected the refused create to roll back, got {described}"
    );
    let reason =
        failed_resource_reason(case, &router, REGION, stack, "Rule", CREATE_FAILED).await;
    let (quoted, class) = quoted_and_class(case, &reason);
    let sentence = format!("Priority must be between {MIN_PRIORITY} and 1,000,000.");
    assert!(
        quoted.contains(&sentence),
        "case {case:?}: expected API Gateway v2's own sentence {sentence:?}, got {quoted:?}"
    );
    assert_eq!(
        class, INVALID_REQUEST,
        "case {case:?}: expected the {INVALID_REQUEST} class, got {class:?} in {quoted:?}"
    );
    let left = listed_routing_rules(&router, case, domain_name).await;
    assert!(
        left.is_empty(),
        "case {case:?}: expected no rule on the domain name, got {left:?}"
    );
}
