// SPDX-License-Identifier: CC0-1.0

//! One SAM template carrying all three of the things a serverless stack is commonly authored
//! with — a bare AWS managed-policy NAME in `Policies`, number and boolean LITERALS in the
//! collections the owning services take as text, and a deployment STAGE — deployed and then
//! deleted end to end.
//!
//! The suites beside this one each hold the service to ONE rule over a table of inputs:
//! `sam_bare_managed_policy_names.rs` to the resolution of a `Policies` name,
//! `lambda_environment_literals.rs` and `api_gateway_string_map_literals.rs` to what a literal
//! travels as where the owning member takes a string, and
//! `api_deployment_held_inside_its_api.rs` to the teardown of a deployment its api still holds.
//! This one is the whole scenario those rules meet in: the template deploys to
//! `CREATE_COMPLETE` with every resource readable in the service that owns it, and the stack's
//! deletion then removes them — which is what a client that deploys such a template and tears
//! it down again observes.

use serde_json::Value;

use crate::apigateway_vpc_link_resources::apigateway;
use crate::change_set_lifecycle::settle_the_store;
use crate::elbv2_resource_provisioning::poll_to_terminal;
use crate::hotswapped_function_code::lambda;
use crate::stack_resource_provisioning::{physical_id, stage_code_objects};
use crate::stack_lifecycle::{
    call, create_stack, iam, kv, member, router_with_engine, xml_text, xml_texts, REGION,
};

/// The stack this suite deploys, and the stage the api is deployed to.
const STACK: &str = "samwhole-stack";
const STAGE: &str = "v1";

/// The AWS managed policy the declaration names BY NAME, and the ARN AWS publishes it at.
///
/// Source: the AWS Managed Policy Reference entry for `AmazonS3ReadOnlyAccess`, which AWS
/// publishes at the root of its own policy namespace.
const NAMED_POLICY: &str = "AmazonS3ReadOnlyAccess";
const NAMED_POLICY_ARN: &str = "arn:aws:iam::aws:policy/AmazonS3ReadOnlyAccess";

/// The managed policy every Lambda execution role the transform generates carries.
const BASIC_EXECUTION_ARN: &str =
    "arn:aws:iam::aws:policy/service-role/AWSLambdaBasicExecutionRole";

/// The template: an api with a stage of its own and a function routed through it, both tagged
/// with number literals, the function's environment stated with them too, and the function's
/// permissions named by the bare managed-policy name above.
fn template() -> String {
    format!(
        r#"{{"Transform":"AWS::Serverless-2016-10-31","Resources":{{
          "Api":{{"Type":"AWS::Serverless::Api","Properties":{{
            "StageName":"{STAGE}","Tags":{{"version":2}},
            "Variables":{{"build":7,"canary":false}}}}}},
          "Reader":{{"Type":"AWS::Serverless::Function","Properties":{{
            "Handler":"index.handler","Runtime":"python3.12",
            "CodeUri":"s3://code-bucket/Reader.zip",
            "Tags":{{"version":2}},
            "Environment":{{"Variables":{{"RETRIES":3,"DEBUG":true}}}},
            "Policies":["{NAMED_POLICY}"],
            "Events":{{"Get":{{"Type":"Api","Properties":{{
              "Path":"/items","Method":"get","RestApiId":{{"Ref":"Api"}}}}}}}}}}}}}}}}"#
    )
}

#[tokio::test]
async fn a_sam_stack_with_a_named_policy_literals_and_a_stage_deploys_and_tears_down() {
    let case = "a SAM template with a named policy, number literals and a deployment stage";
    let (router, engine) = router_with_engine();
    let template = template();
    stage_code_objects(&router, case, &template).await;
    let (code, body) = create_stack(
        &router,
        STACK,
        &template,
        &kv(&[
            ("Capabilities.member.1", "CAPABILITY_IAM"),
            ("Capabilities.member.2", "CAPABILITY_AUTO_EXPAND"),
        ]),
    )
    .await;
    assert_eq!(
        code, 200,
        "case {case:?}: expected CreateStack to be accepted, got {code} {body}"
    );
    let stack_id = xml_text(&body, "StackId")
        .unwrap_or_else(|| panic!("case {case:?}: the create published no StackId in {body}"));
    settle_the_store(&router, &engine).await;
    let described = poll_to_terminal(&router, case, STACK).await;
    assert_eq!(
        xml_text(&described, "StackStatus").as_deref(),
        Some("CREATE_COMPLETE"),
        "case {case:?}: expected the stack to deploy, got {described}"
    );

    // The ROLE the transform generated: the named policy is attached under the ARN AWS
    // publishes it at, beside the basic execution policy every generated role carries.
    let role = physical_id(&router, STACK, "ReaderRole").await;
    let (code, body) = iam(
        &router,
        "ListAttachedRolePolicies",
        &[("RoleName", role.as_str())],
    )
    .await;
    assert_eq!(
        code, 200,
        "case {case:?}: expected ListAttachedRolePolicies({role:?}) to answer, got {code} {body}"
    );
    let mut attached = xml_texts(&body, "PolicyArn");
    attached.sort();
    let mut expected = vec![
        BASIC_EXECUTION_ARN.to_string(),
        NAMED_POLICY_ARN.to_string(),
    ];
    expected.sort();
    assert_eq!(
        attached, expected,
        "case {case:?}: expected the generated role to carry {expected:?}, got {attached:?}"
    );

    // The FUNCTION: its environment carries the literals as the text they spell.
    let function = physical_id(&router, STACK, "Reader").await;
    let answer = lambda(
        &router,
        "GET",
        &format!("https://lambda.{REGION}.amazonaws.com/2015-03-31/functions/{function}"),
        b"",
        &[],
    )
    .await;
    assert_eq!(
        answer.status, 200,
        "case {case:?}: expected Lambda to hold {function:?}, got {} with {}",
        answer.status, answer.body
    );
    let variables = answer
        .body
        .pointer("/Configuration/Environment/Variables")
        .cloned()
        .unwrap_or(Value::Null);
    for (name, text) in [("RETRIES", "3"), ("DEBUG", "true")] {
        assert_eq!(
            variables.get(name).and_then(Value::as_str),
            Some(text),
            "case {case:?}: expected the variable {name:?} to carry {text:?}, got {variables}"
        );
    }

    // The STAGE: its variables carry theirs the same way.
    let api_id = physical_id(&router, STACK, "Api").await;
    let (code, document) =
        apigateway(&router, "GET", &format!("/restapis/{api_id}/stages/{STAGE}")).await;
    assert_eq!(
        code, 200,
        "case {case:?}: expected GetStage({api_id}, {STAGE:?}) to answer, got {code} with \
         {document}"
    );
    let reported = document.get("variables").cloned().unwrap_or(Value::Null);
    for (name, text) in [("build", "7"), ("canary", "false")] {
        assert_eq!(
            reported.get(name).and_then(Value::as_str),
            Some(text),
            "case {case:?}: expected the stage variable {name:?} to carry {text:?}, got \
             {reported}"
        );
    }

    // ...and the stack's own deletion removes every one of them, including the deployment the
    // stage its generated deployment created still points at.
    let (code, body) = call(&router, "DeleteStack", &[member("StackName", &stack_id)]).await;
    assert_eq!(
        code, 200,
        "case {case:?}: expected DeleteStack to be accepted, got {code} {body}"
    );
    settle_the_store(&router, &engine).await;
    let described = poll_to_terminal(&router, case, &stack_id).await;
    assert_eq!(
        xml_text(&described, "StackStatus").as_deref(),
        Some("DELETE_COMPLETE"),
        "case {case:?}: expected the deletion to remove every resource the stack held, got \
         {described}"
    );
    let (code, document) = apigateway(&router, "GET", &format!("/restapis/{api_id}")).await;
    assert_eq!(
        code, 404,
        "case {case:?}: expected API Gateway to hold no api the deletion removed, got {code} \
         with {document}"
    );
}
