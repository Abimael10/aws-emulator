// SPDX-License-Identifier: CC0-1.0

//! What a stack operation reports when a declaration carries a value whose STRUCTURE its
//! resource type's schema does not state — a scalar where the schema states a list or an
//! object — end to end over the awsQuery wire.
//!
//! RULE: CloudFormation validates each declared resource against its resource type's schema
//! before invoking any provider, so a property whose schema states a structural type (a list,
//! an object) carrying a value that is not one fails the operation at that stage: nothing is
//! asked of the service that owns the resource, the stack carries CloudFormation's own summary
//! over the stage's errors, and the diagnosis is the validation result recorded for the
//! resource it failed on — `Property [<member>] expected type: <expected>, found: <actual>` at
//! that member's own path in the submitted template.
//!
//! The rows of the first table differ along what the rule generalises over: which STRUCTURE the
//! schema states (a list, an object), which resource type and property states it (three owning
//! services), the FORM of the value written instead (text, a flag, a whole number) and, as the
//! control, a declaration whose value IS the stated list, which is deployed and read back in
//! the service that owns the authorizer.
//!
//! The second test is the boundary of the question: a property whose value an INTRINSIC
//! FUNCTION produces is judged as the value that function produced, so an expression that
//! returns the stated list deploys and the owning service holds its elements.
//!
//! Nothing here waits out a span: every stack is polled to its terminal status.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::apigateway_vpc_link_resources::apigateway;
use crate::custom_named_replacement::cognito_idp;
use crate::harness::{
    call, create_stack, member, router, settled_stack, xml_blocks, xml_text, REGION,
};
use service_com_amazonaws_cloudformation_cloudformation::state::{resource_status, status};

/// The validation CloudFormation names for the check that holds a declaration's properties to
/// its type's schema. Source: `state::property_validation::PROPERTY_VALIDATION`.
const PROPERTY_VALIDATION: &str = "PROPERTY_VALIDATION";

/// The `EventType` a validation result is reported under, from the model's `EventType` enum
/// (`smithy_query(query="shape", shape="EventType")`).
const VALIDATION_ERROR: &str = "VALIDATION_ERROR";

/// `OperationEvent$ValidationFailureMode` for a validation whose failure was handled by
/// FAILING the operation, from the model's failure-mode enum.
const FAIL: &str = "FAIL";

/// `OperationEvent$ValidationStatus` for a validation the declaration did not pass.
const FAILED: &str = "FAILED";

/// `StackEvent$DetailedStatus` for an operation a pre-deployment validation failed, from the
/// model's `DetailedStatus` enum.
const VALIDATION_FAILED: &str = "VALIDATION_FAILED";

/// The logical ids the templates below declare their resources under.
const API: &str = "Api";
const AUTHORIZER: &str = "Authorizer";
const POOL: &str = "Pool";

/// What a stack reports for a failure of ANY of CloudFormation's pre-deployment validations:
/// the count of errors the stage found and where their detail is read. Source:
/// `state::early_validation::validation_failure_summary`.
fn summary(errors: usize) -> String {
    format!(
        "Validation failed with {errors} error(s). Call DescribeEvents to retrieve the full \
         list of issues with resource and property details, resolve each error, then retry \
         the operation."
    )
}

/// How the stage reports a value whose structure its property's schema does not state. Source:
/// `state::property_validation::structured_properties::type_mismatch_reason`, whose tokens are
/// the JSON type names CloudFormation's own template validator renders.
fn wrong_structure(property: &str, expected: &str, found: &str) -> String {
    format!("Property [{property}] expected type: {expected}, found: {found}")
}

/// Which service's estate a row's template provisions into, so a refused declaration can be
/// shown to have left NOTHING behind there.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Estate {
    /// An HTTP api, and the authorizer that sits inside it, in Amazon API Gateway v2.
    ApiGatewayV2,
    /// A user pool in Amazon Cognito user pools.
    CognitoUserPools,
}

/// One row: a template, and what the structural validation must report about it — `None` for a
/// declaration whose values have the structures their schemas state, which is deployed.
struct Row {
    case: &'static str,
    stack: &'static str,
    estate: Estate,
    template: fn() -> String,
    /// The `(logical id, property, expected structure, structure found)` the validation result
    /// must be about.
    refused: Option<(&'static str, &'static str, &'static str, &'static str)>,
}

/// The identity source a JWT authorizer of an HTTP api reads the bearer token from, as a list
/// of ONE such mapping expression — the shape `CreateAuthorizer$IdentitySource` takes.
fn identity_source() -> Value {
    json!(["$request.header.Authorization"])
}

/// A JWT authorizer's `JwtConfiguration`: the issuer whose tokens it accepts and the audience
/// they have to carry (`JWTConfiguration`'s two members).
fn jwt_configuration() -> Value {
    json!({"Audience": ["api-clients"], "Issuer": "https://login.example.com"})
}

/// A template declaring an HTTP api and a JWT authorizer inside it, the authorizer carrying
/// `properties` beside the api it belongs to.
fn api_and_authorizer(properties: Value) -> String {
    let mut authorizer = json!({
        "ApiId": {"Ref": API},
        "AuthorizerType": "JWT",
        "Name": "structured-properties-authorizer",
    });
    if let (Value::Object(document), Value::Object(declared)) = (&mut authorizer, &properties) {
        for (property, value) in declared {
            document.insert(property.clone(), value.clone());
        }
    }
    json!({
        "Resources": {
            API: {
                "Type": "AWS::ApiGatewayV2::Api",
                "Properties": {"Name": "structured-properties-api", "ProtocolType": "HTTP"},
            },
            AUTHORIZER: {"Type": "AWS::ApiGatewayV2::Authorizer", "Properties": authorizer},
        },
        "Outputs": {
            "ApiId": {"Value": {"Ref": API}},
            "AuthorizerId": {"Value": {"Ref": AUTHORIZER}},
        },
    })
    .to_string()
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "an authorizer whose identity source is one plain string",
            stack: "structured-identity-source-string",
            estate: Estate::ApiGatewayV2,
            template: || {
                api_and_authorizer(json!({
                    "IdentitySource": "$request.header.Authorization",
                    "JwtConfiguration": jwt_configuration(),
                }))
            },
            refused: Some((AUTHORIZER, "IdentitySource", "JSONArray", "String")),
        },
        Row {
            case: "an authorizer whose JWT configuration is the issuer's text",
            stack: "structured-jwt-configuration-string",
            estate: Estate::ApiGatewayV2,
            template: || {
                api_and_authorizer(json!({
                    "IdentitySource": identity_source(),
                    "JwtConfiguration": "https://login.example.com",
                }))
            },
            refused: Some((AUTHORIZER, "JwtConfiguration", "JSONObject", "String")),
        },
        Row {
            case: "an api whose CORS configuration is a flag",
            stack: "structured-cors-configuration-flag",
            estate: Estate::ApiGatewayV2,
            template: || {
                json!({
                    "Resources": {
                        API: {
                            "Type": "AWS::ApiGatewayV2::Api",
                            "Properties": {
                                "Name": "structured-cors-api",
                                "ProtocolType": "HTTP",
                                "CorsConfiguration": true,
                            },
                        },
                    },
                })
                .to_string()
            },
            refused: Some((API, "CorsConfiguration", "JSONObject", "Boolean")),
        },
        Row {
            case: "a user pool whose schema is a whole number",
            stack: "structured-user-pool-schema-number",
            estate: Estate::CognitoUserPools,
            template: || {
                json!({
                    "Resources": {
                        POOL: {
                            "Type": "AWS::Cognito::UserPool",
                            "Properties": {
                                "UserPoolName": "structured-properties-pool",
                                "Schema": 2,
                            },
                        },
                    },
                })
                .to_string()
            },
            refused: Some((POOL, "Schema", "JSONArray", "Integer")),
        },
        Row {
            case: "an authorizer whose identity source IS the list its schema states",
            stack: "structured-identity-source-list",
            estate: Estate::ApiGatewayV2,
            template: || {
                api_and_authorizer(json!({
                    "IdentitySource": identity_source(),
                    "JwtConfiguration": jwt_configuration(),
                }))
            },
            refused: None,
        },
    ]
}

/// The validation RESULTS `DescribeEvents` reports for `stack`: one tuple per event carrying a
/// `ValidationName`.
async fn validation_results(
    router: &MultiServiceRouter,
    case: &str,
    stack: &str,
) -> Vec<(String, String, String, String, String, String, String)> {
    let (code, body) = call(router, "DescribeEvents", &[member("StackName", stack)]).await;
    assert_eq!(
        code, 200,
        "case {case:?}: the operation's events must be readable, got {code} {body}"
    );
    xml_blocks(&body, "member")
        .iter()
        .filter(|event| xml_text(event, "ValidationName").is_some())
        .map(|event| {
            (
                xml_text(event, "LogicalResourceId").unwrap_or_default(),
                xml_text(event, "EventType").unwrap_or_default(),
                xml_text(event, "ValidationName").unwrap_or_default(),
                xml_text(event, "ValidationStatus").unwrap_or_default(),
                xml_text(event, "ValidationFailureMode").unwrap_or_default(),
                xml_text(event, "ValidationStatusReason").unwrap_or_default(),
                xml_text(event, "ValidationPath").unwrap_or_default(),
            )
        })
        .collect()
}

/// `(DetailedStatus, ResourceStatusReason)` of every stack-level `CREATE_FAILED` event of
/// `stack`, and how many events the stack recorded about its declared resources.
async fn failure_and_resource_events(
    router: &MultiServiceRouter,
    case: &str,
    stack: &str,
) -> (Vec<(String, String)>, usize) {
    let (code, body) = call(
        router,
        "DescribeStackEvents",
        &[member("StackName", stack)],
    )
    .await;
    assert_eq!(
        code, 200,
        "case {case:?}: the stack's events must be readable, got {code} {body}"
    );
    let events = xml_blocks(&body, "member");
    let failed = events
        .iter()
        .filter(|event| {
            xml_text(event, "LogicalResourceId").as_deref() == Some(stack)
                && xml_text(event, "ResourceStatus").as_deref()
                    == Some(resource_status::CREATE_FAILED)
        })
        .map(|event| {
            (
                xml_text(event, "DetailedStatus").unwrap_or_default(),
                xml_text(event, "ResourceStatusReason").unwrap_or_default(),
            )
        })
        .collect();
    let about_a_resource = events
        .iter()
        .filter(|event| {
            matches!(
                xml_text(event, "LogicalResourceId").as_deref(),
                Some(API) | Some(AUTHORIZER) | Some(POOL)
            )
        })
        .count();
    (failed, about_a_resource)
}

/// The apis Amazon API Gateway v2 holds (`GetApis`, `GET /v2/apis`), as that collection's own
/// `items` member carries them.
async fn apis(router: &MultiServiceRouter, case: &str) -> Vec<Value> {
    let (code, document) = apigateway(router, "GET", "/v2/apis").await;
    assert_eq!(
        code, 200,
        "case {case:?}: GetApis must answer 200, got {code} {document}"
    );
    document
        .get("items")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

/// The user pools Amazon Cognito holds (`ListUserPools`), as that answer's `UserPools` member
/// carries them.
async fn user_pools(router: &MultiServiceRouter, case: &str) -> Vec<Value> {
    let (code, document) = cognito_idp(router, "ListUserPools", &json!({"MaxResults": 60})).await;
    assert_eq!(
        code, 200,
        "case {case:?}: ListUserPools must answer 200, got {code} {document}"
    );
    document
        .get("UserPools")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

/// The `PhysicalResourceId` the stack publishes for one of its logical resources — the handle
/// the owning service knows that resource by.
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

/// The `IdentitySource` Amazon API Gateway v2 holds for the authorizer a stack deployed, read
/// back through that service's own wire by the ids the stack published.
async fn deployed_identity_source(
    router: &MultiServiceRouter,
    case: &str,
    stack: &str,
) -> Vec<String> {
    let api = published_id(router, case, stack, API).await;
    let authorizer = published_id(router, case, stack, AUTHORIZER).await;
    let (code, document) = apigateway(
        router,
        "GET",
        &format!("/v2/apis/{api}/authorizers/{authorizer}"),
    )
    .await;
    assert_eq!(
        code, 200,
        "case {case:?}: the deployed authorizer must be readable, got {code} {document}"
    );
    document
        .get("identitySource")
        .and_then(Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(|value| value.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// Every row: a declaration carrying a value whose structure its property's schema does not
/// state fails the stack before anything is provisioned — the summary on the stack, the
/// diagnosis on the validation result, and nothing at all in the service that owns the
/// resource — while a declaration whose value IS the stated list is deployed and read back
/// there.
#[tokio::test]
async fn a_value_of_another_structure_rolls_the_stack_back() {
    for row in rows() {
        let case = row.case;
        // A router of its own per row: what each case is about is its own declaration, not
        // what another row left behind.
        let router = router();

        let (code, created) = create_stack(&router, row.stack, &(row.template)(), &[]).await;
        assert_eq!(
            code, 200,
            "case {case:?}: CreateStack must answer 200 — the request is valid and the stack \
             exists — got {code} {created}"
        );
        let described = settled_stack(case, &router, REGION, row.stack).await;
        let expected_status = match row.refused {
            Some(_) => status::ROLLBACK_COMPLETE,
            None => status::CREATE_COMPLETE,
        };
        let settled = xml_text(&described, "StackStatus").unwrap_or_default();
        assert_eq!(
            settled, expected_status,
            "case {case:?}: expected the stack to settle {expected_status}, got {settled:?} \
             ({described})"
        );

        // What the STACK reports: its own failure event, with the detailed status the model
        // gives a validation failure and the summary over the stage's one error.
        let (failed, about_a_resource) =
            failure_and_resource_events(&router, case, row.stack).await;
        let expected_failed: Vec<(String, String)> = row
            .refused
            .map(|_| vec![(VALIDATION_FAILED.to_string(), summary(1))])
            .unwrap_or_default();
        assert_eq!(
            failed, expected_failed,
            "case {case:?}: expected the stack's failure event {expected_failed:?}, got \
             {failed:?}"
        );
        assert_eq!(
            about_a_resource > 0,
            row.refused.is_none(),
            "case {case:?}: expected resource events only where the declaration was deployed, \
             got {about_a_resource} of them"
        );

        // ...and the DETAIL of the failure: an event about the resource nothing was done to,
        // naming the validation, its status, how its failure was handled, the structures
        // expected and found, and the member it was applied at.
        let results = validation_results(&router, case, row.stack).await;
        let expected_results: Vec<(String, String, String, String, String, String, String)> = row
            .refused
            .map(|(logical_id, property, expected, found)| {
                vec![(
                    logical_id.to_string(),
                    VALIDATION_ERROR.to_string(),
                    PROPERTY_VALIDATION.to_string(),
                    FAILED.to_string(),
                    FAIL.to_string(),
                    wrong_structure(property, expected, found),
                    format!("/Resources/{logical_id}/Properties/{property}"),
                )]
            })
            .unwrap_or_default();
        assert_eq!(
            results, expected_results,
            "case {case:?}: expected the validation results {expected_results:?}, got \
             {results:?}"
        );

        // What the owning service holds is the proof: a refused declaration left nothing there
        // at all, and a deployed one carries the list its declaration stated.
        if row.refused.is_some() {
            match row.estate {
                Estate::ApiGatewayV2 => {
                    let held = apis(&router, case).await;
                    assert!(
                        held.is_empty(),
                        "case {case:?}: a refused declaration must leave no api behind, got \
                         {held:?}"
                    );
                }
                Estate::CognitoUserPools => {
                    let held = user_pools(&router, case).await;
                    assert!(
                        held.is_empty(),
                        "case {case:?}: a refused declaration must leave no user pool behind, \
                         got {held:?}"
                    );
                }
            }
            continue;
        }
        let held = apis(&router, case).await;
        assert_eq!(
            held.len(),
            1,
            "case {case:?}: expected the deployed api to be the one api held, got {held:?}"
        );
        let source = deployed_identity_source(&router, case, row.stack).await;
        assert_eq!(
            source,
            vec!["$request.header.Authorization".to_string()],
            "case {case:?}: expected API Gateway v2 to hold the declared identity source, got \
             {source:?}"
        );
    }
}

/// The boundary of the question: what a property carries is the value its INTRINSIC FUNCTION
/// produced, so a declaration whose identity source is an expression returning the stated list
/// passes the stage and deploys — and the owning service holds every element that expression
/// produced.
#[tokio::test]
async fn an_expression_that_produces_the_stated_list_is_deployed() {
    let case = "an authorizer whose identity source is a split expression";
    let router = router();
    let stack = "structured-identity-source-expression";
    let template = api_and_authorizer(json!({
        "IdentitySource": {
            "Fn::Split": [
                ",",
                "$request.header.Authorization,$request.querystring.access_token",
            ],
        },
        "JwtConfiguration": jwt_configuration(),
    }));

    let (code, created) = create_stack(&router, stack, &template, &[]).await;
    assert_eq!(
        code, 200,
        "case {case:?}: CreateStack must answer 200, got {code} {created}"
    );
    let described = settled_stack(case, &router, REGION, stack).await;
    let settled = xml_text(&described, "StackStatus").unwrap_or_default();
    assert_eq!(
        settled,
        status::CREATE_COMPLETE,
        "case {case:?}: expected the stack to settle {}, got {settled:?} ({described})",
        status::CREATE_COMPLETE
    );

    // ...and no pre-deployment validation refused anything: the stage recorded no result.
    let results = validation_results(&router, case, stack).await;
    assert!(
        results.is_empty(),
        "case {case:?}: expected no validation result at all, got {results:?}"
    );

    let source = deployed_identity_source(&router, case, stack).await;
    assert_eq!(
        source,
        vec![
            "$request.header.Authorization".to_string(),
            "$request.querystring.access_token".to_string(),
        ],
        "case {case:?}: expected API Gateway v2 to hold both elements the expression produced, \
         got {source:?}"
    );
}
