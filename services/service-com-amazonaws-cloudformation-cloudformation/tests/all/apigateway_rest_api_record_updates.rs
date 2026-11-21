// SPDX-License-Identifier: CC0-1.0

//! An `UpdateStack` that re-declares one of the records a REST API is built out of — the api,
//! a resource of it, a method on that resource, a deployment of them and the stage that serves
//! one — end to end: the CloudFormation calls over its awsQuery wire, the records over Amazon
//! API Gateway's own `restJson1` wire.
//!
//! RULE under test: **a property a deployment states differently for a record the stack
//! already holds is applied to that record through the PATCH call API Gateway publishes for it
//! (`UpdateRestApi`, `UpdateResource`, `UpdateMethod`, `UpdateDeployment`, `UpdateStage`, each
//! taking the `patchOperations` that write the pointers that service lets a caller write) — the
//! record keeps the identity it was created under, that service really holds the new value, and
//! the stack reaches `UPDATE_COMPLETE`. A property that is FIXED when API Gateway creates the
//! record is the other half of the same rule: such a declaration asks for a different record,
//! so CloudFormation creates a new physical resource for the logical one and takes the old one
//! down under its `UpdateReplacePolicy`.**
//!
//! Which properties those are is read off the calls that service publishes: `restApiId` is an
//! `@httpLabel` of every call on a resource, a method, a deployment and a stage — the create
//! included — and no call moves one to another api; `UpdateMethod` addresses a method by the
//! resource and the verb and takes no new value for either; `UpdateDeployment` patches the
//! description alone, so the stage a deployment releases to is fixed when the snapshot is
//! taken; and a resource's parent and path segment are the two the type itself publishes with
//! "Update requires: Replacement".
//!
//! The rows differ along what the rule generalises over: WHICH record of the five the
//! declaration restates, the FORM of the changed value (a scalar, a nested structure, a
//! collection the caller keys), and — for the replacing half — whether the record's identity
//! is one API Gateway assigns (a resource, a deployment), one CloudFormation generates (a
//! method) or the name the declaration states, which is unique only inside the api that holds
//! it (a stage).
//!
//! Every assertion is a client's: the stack through `DescribeStacks` and
//! `DescribeStackResource`, the records through API Gateway's own reads.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::apigateway_vpc_link_resources::apigateway;
use crate::stack_lifecycle::{
    call, create_stack, describe_stack, member, resource_event_statuses, router,
    wait_for_the_transition, xml_blocks, xml_text,
};
use service_com_amazonaws_cloudformation_cloudformation::state::{resource_status, status};

/// How many reads a case gives a stack operation to reach its terminal status in: a client
/// polls for it, and what is asserted is the status the stack SETTLED on, never how long it
/// took to get there.
const POLL_ATTEMPTS: usize = 40;

/// The stage name every revision below declares, which is a stage's own identity.
const STAGE_NAME: &str = "live";

/// The logical ids the template declares, in the order the assertions walk them.
const API_A: &str = "ApiA";
const API_B: &str = "ApiB";
const RESOURCE: &str = "Res";
const METHOD: &str = "Method";
const DEPLOYMENT: &str = "Deployment";
const STAGE: &str = "Stage";

/// One revision of the stack this suite deploys: two REST APIs, each with a resource and a
/// method on it, one deployment of the methods of the api it is declared in, and the stage
/// that serves that deployment. Every field is a value a row restates.
#[derive(Clone, Copy, Debug)]
struct Shape {
    /// `AWS::ApiGateway::RestApi`'s `Description` — one `replace` of `/description`.
    api_description: &'static str,
    /// `AWS::ApiGateway::Resource`'s `PathPart`: the segment the resource adds under the
    /// api's root, which is create-only.
    path_part: &'static str,
    /// `AWS::ApiGateway::Method`'s `HttpMethod`: the verb the method is declared under, which
    /// is create-only.
    http_method: &'static str,
    /// That method's `OperationName` — one `replace` of `/operationName`.
    operation_name: &'static str,
    /// The template the method's MOCK integration maps its 200 response with, which is the
    /// nested structure a put of the integration writes.
    response_template: &'static str,
    /// `AWS::ApiGateway::Deployment`'s `Description` — the one pointer that call publishes.
    deployment_description: &'static str,
    /// `AWS::ApiGateway::Stage`'s `Description` — one `replace` of `/description`.
    stage_description: &'static str,
    /// That stage's `Variables`, which that call writes one entry at a time
    /// (`/variables/{name}`).
    variables: &'static [(&'static str, &'static str)],
    /// Which of the two apis the deployment — and the stage that serves it — belongs to,
    /// which is the api every call on either addresses it inside.
    serves: &'static str,
}

/// The revision every row is deployed from, and the one each row restates ONE value of.
const CREATED: Shape = Shape {
    api_description: "the api as created",
    path_part: "items",
    http_method: "GET",
    operation_name: "ListItems",
    response_template: "{\"items\":[]}",
    deployment_description: "the deployment as created",
    stage_description: "the stage as created",
    variables: &[("Backend", "primary")],
    serves: API_A,
};

/// The `Variables` map a declaration states as an object.
fn variables(entries: &[(&str, &str)]) -> Value {
    let mut map = serde_json::Map::new();
    for (name, value) in entries {
        map.insert((*name).to_string(), json!(value));
    }
    Value::Object(map)
}

/// The resource that adds `path_part` under one api's root, and the method on it, as the two
/// declarations a revision states them with.
fn api_resources(
    shape: &Shape,
    api: &str,
    suffix: &str,
    path_part: &str,
    verb: &str,
) -> Vec<(String, Value)> {
    let resource = format!("{RESOURCE}{suffix}");
    vec![
        (
            resource.clone(),
            json!({
                "Type": "AWS::ApiGateway::Resource",
                "Properties": {
                    "RestApiId": { "Ref": api },
                    "ParentId": { "Fn::GetAtt": [api, "RootResourceId"] },
                    "PathPart": path_part
                }
            }),
        ),
        (
            format!("{METHOD}{suffix}"),
            json!({
                "Type": "AWS::ApiGateway::Method",
                "Properties": {
                    "RestApiId": { "Ref": api },
                    "ResourceId": { "Ref": resource },
                    "HttpMethod": verb,
                    "AuthorizationType": "NONE",
                    "OperationName": shape.operation_name,
                    "MethodResponses": [{ "StatusCode": "200" }],
                    "Integration": {
                        "Type": "MOCK",
                        "RequestTemplates": { "application/json": "{\"statusCode\":200}" },
                        "IntegrationResponses": [{
                            "StatusCode": "200",
                            "ResponseTemplates": {
                                "application/json": shape.response_template
                            }
                        }]
                    }
                }
            }),
        ),
    ]
}

/// One revision of this suite's stack, as the template a client submits.
///
/// The second api is declared in every revision, so the row that moves the deployment and the
/// stage to it restates the api they belong to and nothing else.
fn template(stack: &str, shape: &Shape) -> String {
    let mut resources = serde_json::Map::new();
    resources.insert(
        API_A.to_string(),
        json!({
            "Type": "AWS::ApiGateway::RestApi",
            "Properties": {
                "Name": format!("{stack}-a"),
                "Description": shape.api_description,
                "EndpointConfiguration": { "Types": ["REGIONAL"] }
            }
        }),
    );
    resources.insert(
        API_B.to_string(),
        json!({
            "Type": "AWS::ApiGateway::RestApi",
            "Properties": {
                "Name": format!("{stack}-b"),
                "EndpointConfiguration": { "Types": ["REGIONAL"] }
            }
        }),
    );
    for (logical_id, declaration) in api_resources(shape, API_A, "", shape.path_part, shape.http_method)
        .into_iter()
        .chain(api_resources(shape, API_B, "B", "orders", "POST"))
    {
        resources.insert(logical_id, declaration);
    }
    resources.insert(
        DEPLOYMENT.to_string(),
        json!({
            "Type": "AWS::ApiGateway::Deployment",
            "DependsOn": [METHOD, format!("{METHOD}B")],
            // The old half of a replacement is KEPT, so a row that moves the deployment to
            // the other api leaves the snapshot the stage it served was serving in place:
            // "with the UpdateReplacePolicy attribute you can specify how CloudFormation
            // handles the old physical resource".
            "UpdateReplacePolicy": "Retain",
            "Properties": {
                "RestApiId": { "Ref": shape.serves },
                "Description": shape.deployment_description
            }
        }),
    );
    resources.insert(
        STAGE.to_string(),
        json!({
            "Type": "AWS::ApiGateway::Stage",
            "UpdateReplacePolicy": "Retain",
            "Properties": {
                "RestApiId": { "Ref": shape.serves },
                "DeploymentId": { "Ref": DEPLOYMENT },
                "StageName": STAGE_NAME,
                "Description": shape.stage_description,
                "Variables": variables(shape.variables)
            }
        }),
    );
    json!({ "Resources": Value::Object(resources) }).to_string()
}

/// Read a stack until it reports a status that is no longer in progress, and answer that
/// status beside the reason it carries.
async fn settled(router: &MultiServiceRouter, case: &str, stack: &str) -> (String, String) {
    let mut described = String::new();
    for _ in 0..POLL_ATTEMPTS {
        wait_for_the_transition().await;
        let (code, body) = describe_stack(router, stack).await;
        assert_eq!(
            code, 200,
            "case {case:?}: expected DescribeStacks to answer 200, got {code} {body}"
        );
        described = body;
        if !xml_text(&described, "StackStatus")
            .unwrap_or_default()
            .ends_with("_IN_PROGRESS")
        {
            break;
        }
    }
    (
        xml_text(&described, "StackStatus").unwrap_or_default(),
        xml_text(&described, "StackStatusReason").unwrap_or_default(),
    )
}

/// The `PhysicalResourceId` the stack publishes for one of its logical resources — the handle
/// a client reaches that record by.
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
        "case {case:?}: expected DescribeStackResource({logical_id}) to answer 200, got {code} \
         {body}"
    );
    xml_text(&body, "PhysicalResourceId").unwrap_or_default()
}

/// Every logical resource's published identifier, in the order the assertions walk them.
async fn published_ids(
    router: &MultiServiceRouter,
    case: &str,
    stack: &str,
) -> Vec<(&'static str, String)> {
    let mut ids = Vec::new();
    for logical_id in [API_A, API_B, RESOURCE, METHOD, DEPLOYMENT, STAGE] {
        ids.push((logical_id, published_id(router, case, stack, logical_id).await));
    }
    ids
}

/// One identifier out of [`published_ids`].
fn id_of(ids: &[(&'static str, String)], logical_id: &str) -> String {
    ids.iter()
        .find(|(held, _)| *held == logical_id)
        .map(|(_, id)| id.clone())
        .unwrap_or_else(|| panic!("the stack publishes an id for {logical_id}"))
}

/// One API Gateway read, asserted to answer 200, as the record that service holds.
async fn held(router: &MultiServiceRouter, case: &str, uri: &str) -> Value {
    let (code, document) = apigateway(router, "GET", uri).await;
    assert_eq!(
        code, 200,
        "case {case:?}: expected API Gateway to report {uri}, got {code} {document}"
    );
    document
}

/// Assert that API Gateway really holds the configuration `shape` declares, for every record
/// the stack publishes an identifier for: each is read on the URI that service addresses it by,
/// which is what says the stack's record resolves to a record that exists there.
async fn holds_the_declaration(
    router: &MultiServiceRouter,
    case: &str,
    ids: &[(&'static str, String)],
    shape: &Shape,
) {
    let api_a = id_of(ids, API_A);
    let serving_api = id_of(ids, shape.serves);
    let resource_id = id_of(ids, RESOURCE);
    let deployment_id = id_of(ids, DEPLOYMENT);

    let api = held(router, case, &format!("/restapis/{api_a}")).await;
    assert_eq!(
        api["description"],
        json!(shape.api_description),
        "case {case:?}: expected the api to hold the declared description, got {api}"
    );

    let resource = held(
        router,
        case,
        &format!("/restapis/{api_a}/resources/{resource_id}"),
    )
    .await;
    assert_eq!(
        resource["pathPart"],
        json!(shape.path_part),
        "case {case:?}: expected the resource to hold the declared path segment, got {resource}"
    );

    let method = held(
        router,
        case,
        &format!(
            "/restapis/{api_a}/resources/{resource_id}/methods/{}",
            shape.http_method
        ),
    )
    .await;
    assert_eq!(
        method["operationName"],
        json!(shape.operation_name),
        "case {case:?}: expected the method to hold the declared operation name, got {method}"
    );
    assert_eq!(
        method["methodIntegration"]["integrationResponses"]["200"]["responseTemplates"]
            ["application/json"],
        json!(shape.response_template),
        "case {case:?}: expected the integration to map its response with the declared \
         template, got {method}"
    );

    let deployment = held(
        router,
        case,
        &format!("/restapis/{serving_api}/deployments/{deployment_id}"),
    )
    .await;
    assert_eq!(
        deployment["description"],
        json!(shape.deployment_description),
        "case {case:?}: expected the deployment to hold the declared description, got \
         {deployment}"
    );

    let stage = held(
        router,
        case,
        &format!("/restapis/{serving_api}/stages/{STAGE_NAME}"),
    )
    .await;
    assert_eq!(
        stage["description"],
        json!(shape.stage_description),
        "case {case:?}: expected the stage to hold the declared description, got {stage}"
    );
    assert_eq!(
        stage["variables"],
        variables(shape.variables),
        "case {case:?}: expected the stage to hold the declared variables, got {stage}"
    );
    assert_eq!(
        stage["deploymentId"],
        json!(deployment_id),
        "case {case:?}: expected the stage to serve the deployment the stack records, got \
         {stage}"
    );
}

/// Deploy a row's first revision and answer the identifiers it published.
async fn created(
    router: &MultiServiceRouter,
    case: &str,
    stack: &str,
) -> Vec<(&'static str, String)> {
    let (code, body) = create_stack(router, stack, &template(stack, &CREATED), &[]).await;
    assert_eq!(
        code, 200,
        "case {case:?}: expected CreateStack to succeed, got {code} {body}"
    );
    let (reported, reason) = settled(router, case, stack).await;
    assert_eq!(
        reported,
        status::CREATE_COMPLETE,
        "case {case:?}: expected the create to complete, got {reported:?} ({reason:?})"
    );
    let ids = published_ids(router, case, stack).await;
    holds_the_declaration(router, case, &ids, &CREATED).await;
    ids
}

/// Submit a row's revision and answer the identifiers the update left published.
async fn revised(
    router: &MultiServiceRouter,
    case: &str,
    stack: &str,
    shape: &Shape,
) -> Vec<(&'static str, String)> {
    let (code, body) = call(
        router,
        "UpdateStack",
        &[
            member("StackName", stack),
            member("TemplateBody", &template(stack, shape)),
        ],
    )
    .await;
    assert_eq!(
        code, 200,
        "case {case:?}: expected UpdateStack to succeed, got {code} {body}"
    );
    let (reported, reason) = settled(router, case, stack).await;
    assert_eq!(
        reported,
        status::UPDATE_COMPLETE,
        "case {case:?}: expected the update to complete, got {reported:?} ({reason:?})"
    );
    published_ids(router, case, stack).await
}

/// The `PhysicalResourceId` every `DELETE_SKIPPED` event the stack recorded for `logical_id`
/// names — what says WHICH instance a skipped deletion was about.
async fn skipped_instances(
    router: &MultiServiceRouter,
    case: &str,
    stack: &str,
    logical_id: &str,
) -> Vec<String> {
    let (code, body) = call(router, "DescribeStackEvents", &[member("StackName", stack)]).await;
    assert_eq!(
        code, 200,
        "case {case:?}: expected DescribeStackEvents to answer 200, got {code} {body}"
    );
    xml_blocks(&body, "member")
        .iter()
        .filter(|event| xml_text(event, "LogicalResourceId").as_deref() == Some(logical_id))
        .filter(|event| {
            xml_text(event, "ResourceStatus").as_deref() == Some(resource_status::DELETE_SKIPPED)
        })
        .map(|event| xml_text(event, "PhysicalResourceId").unwrap_or_default())
        .collect()
}

/// One row of the in-place table: the revision it states, and which record's own patch call
/// carries the change.
struct InPlace {
    case: &'static str,
    stack: &'static str,
    shape: Shape,
}

fn in_place_rows() -> Vec<InPlace> {
    vec![
        InPlace {
            case: "the api's description, which UpdateRestApi patches",
            stack: "apigw-rest-in-place-api-description",
            shape: Shape {
                api_description: "the api as revised",
                ..CREATED
            },
        },
        InPlace {
            case: "the method's operation name, which UpdateMethod patches",
            stack: "apigw-rest-in-place-method-operation",
            shape: Shape {
                operation_name: "ListItemsRevised",
                ..CREATED
            },
        },
        InPlace {
            case: "the method's mock integration, which a put of it writes",
            stack: "apigw-rest-in-place-method-integration",
            shape: Shape {
                response_template: "{\"items\":[{\"id\":\"1\"}]}",
                ..CREATED
            },
        },
        InPlace {
            case: "the deployment's description, the one pointer UpdateDeployment publishes",
            stack: "apigw-rest-in-place-deployment-description",
            shape: Shape {
                deployment_description: "the deployment as revised",
                ..CREATED
            },
        },
        InPlace {
            case: "the stage's description, which UpdateStage patches",
            stack: "apigw-rest-in-place-stage-description",
            shape: Shape {
                stage_description: "the stage as revised",
                ..CREATED
            },
        },
        InPlace {
            case: "the stage's variables, which UpdateStage writes one entry at a time",
            stack: "apigw-rest-in-place-stage-variables",
            shape: Shape {
                variables: &[("Backend", "standby"), ("Added", "yes")],
                ..CREATED
            },
        },
    ]
}

/// RULE: see the module header — a change the record's own patch call can express is applied
/// to the record that exists, which keeps the identity it was created under.
#[tokio::test]
async fn a_changed_property_of_a_rest_api_record_is_applied_in_place() {
    for InPlace { case, stack, shape } in in_place_rows() {
        let router = router();
        let before = created(&router, case, stack).await;
        let after = revised(&router, case, stack, &shape).await;
        assert_eq!(
            after, before,
            "case {case:?}: expected every record to keep the identity it was created under, \
             got {after:?}"
        );
        holds_the_declaration(&router, case, &after, &shape).await;
    }
}

/// One row of the replacing table: the revision it states, which logical resources the
/// replacement moves to a new physical resource, and whether it moves the stage to the other
/// api.
struct Replacing {
    case: &'static str,
    stack: &'static str,
    shape: Shape,
    moved: &'static [&'static str],
    moves_the_stage: bool,
}

fn replacing_rows() -> Vec<Replacing> {
    vec![
        Replacing {
            case: "the path segment of a resource, which the type publishes as replacement",
            stack: "apigw-rest-replace-path-part",
            shape: Shape {
                path_part: "catalogue",
                ..CREATED
            },
            // The method on it is replaced with the resource: its declaration names the
            // resource it is put on, and that resource is a new one.
            moved: &[RESOURCE, METHOD],
            moves_the_stage: false,
        },
        Replacing {
            case: "the verb a method is declared under, which UpdateMethod addresses it by",
            stack: "apigw-rest-replace-http-method",
            shape: Shape {
                http_method: "PUT",
                ..CREATED
            },
            moved: &[METHOD],
            moves_the_stage: false,
        },
        Replacing {
            case: "the api a deployment and the stage that serves it belong to",
            stack: "apigw-rest-replace-serving-api",
            shape: Shape {
                serves: API_B,
                ..CREATED
            },
            // A deployment's identity is API Gateway's to assign, so the replacement carries
            // one of its own; a stage's is the name the declaration states, which does not
            // move — what moves is the api that name identifies it inside.
            moved: &[DEPLOYMENT],
            moves_the_stage: true,
        },
    ]
}

/// RULE: see the module header — a declaration that restates a property API Gateway fixes
/// when it creates the record asks for a different record, which CloudFormation replaces.
#[tokio::test]
async fn a_restated_create_only_property_replaces_the_rest_api_record() {
    for Replacing {
        case,
        stack,
        shape,
        moved,
        moves_the_stage,
    } in replacing_rows()
    {
        let router = router();
        let before = created(&router, case, stack).await;
        let after = revised(&router, case, stack, &shape).await;

        for (logical_id, published) in &after {
            let was = id_of(&before, logical_id);
            if moved.contains(logical_id) {
                assert_ne!(
                    published, &was,
                    "case {case:?}: expected {logical_id} to be replaced by a physical \
                     resource of its own, got {published:?}"
                );
                continue;
            }
            assert_eq!(
                published, &was,
                "case {case:?}: expected {logical_id} to keep the identity it was created \
                 under, got {published:?}"
            );
        }

        // Whatever was replaced, the records the stack now publishes are records API Gateway
        // really holds, under the configuration the revision declares.
        holds_the_declaration(&router, case, &after, &shape).await;

        if !moves_the_stage {
            continue;
        }
        // The stage was created inside the OTHER api, under the name it always had, and the
        // stage it replaced is kept: `UpdateReplacePolicy: Retain` "keeps the resource
        // without deleting" it, so that api still serves it and the one thing the stack
        // records about it is that its deletion was SKIPPED.
        let served_before = id_of(&before, API_A);
        let retained = held(
            &router,
            case,
            &format!("/restapis/{served_before}/stages/{STAGE_NAME}"),
        )
        .await;
        assert_eq!(
            retained["deploymentId"],
            json!(id_of(&before, DEPLOYMENT)),
            "case {case:?}: expected the retained stage to go on serving the snapshot it was \
             created with, got {retained}"
        );
        for logical_id in [DEPLOYMENT, STAGE] {
            let statuses = resource_event_statuses(&router, case, stack, logical_id).await;
            assert!(
                statuses
                    .iter()
                    .any(|reported| reported == resource_status::DELETE_SKIPPED),
                "case {case:?}: expected the retained {logical_id} to report its deletion as \
                 skipped, got {statuses:?}"
            );
            assert!(
                !statuses.iter().any(|reported| reported
                    == resource_status::DELETE_COMPLETE
                    || reported == resource_status::DELETE_IN_PROGRESS),
                "case {case:?}: expected nothing to be deleted for the retained {logical_id}, \
                 got {statuses:?}"
            );
        }
        // ...and each of those events names the INSTANCE it is about: the deployment the
        // replacement superseded carries an identifier of its own, which is what its event
        // reports, while the stage's identifier is the one the replacement kept — it now
        // names the stage the stack records, so the old half's event names no instance
        // rather than that one.
        let skipped = |logical_id: &'static str| {
            let expected = if logical_id == STAGE {
                String::new()
            } else {
                id_of(&before, logical_id)
            };
            (logical_id, expected)
        };
        for (logical_id, expected) in [skipped(DEPLOYMENT), skipped(STAGE)] {
            let named = skipped_instances(&router, case, stack, logical_id).await;
            assert_eq!(
                named,
                vec![expected.clone()],
                "case {case:?}: expected the skipped deletion of {logical_id} to name \
                 {expected:?}, got {named:?}"
            );
        }
    }
}
