// SPDX-License-Identifier: CC0-1.0

//! What a template's NUMBER and BOOLEAN literals become in the members AWS Lambda types as
//! collections of TEXT — a function's environment variables and its tags — read back through
//! Lambda itself.
//!
//! The rule this suite holds the service to is the one `api_gateway_string_map_literals.rs`
//! holds API Gateway to, on the other service a SAM template deploys into: **a property value a
//! template states as a JSON number or boolean literal travels to the service that owns the
//! resource as the TEXT that literal spells, wherever that service's own member takes a
//! string.** "All property values in a template are strings", and
//! `Environment$Variables` is a map of name to STRING
//! (`smithy_query(query="shape",shape="Environment")`), so `Environment: {Variables: {RETRIES:
//! 3}}` configures `RETRIES` with `"3"` and the stack deploys, rather than being refused as a
//! number where a string was expected.
//!
//! The rows differ along what the rule generalises over: which literal the declaration states
//! (an integer, a negative integer, a decimal, both booleans, and a string, which is already
//! its own text), and which collection carries it — the function's `Environment.Variables` and
//! its `Tags`, both maps the requester keys.

use std::sync::Arc;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::change_set_lifecycle::settle_the_store;
use crate::elbv2_resource_provisioning::poll_to_terminal;
use crate::hotswapped_function_code::lambda;
use crate::sam_transform::sam_template;
use crate::stack_resource_provisioning::{physical_id, stage_code_objects};
use crate::stack_lifecycle::{create_stack, kv, router_with_engine, xml_text, REGION};

/// The literals one declaration states, each beside the TEXT it must reach Lambda as.
///
/// Source: the JSON literal kinds a template can carry in a property value — an integer, a
/// negative integer, a decimal, the two booleans — beside a string, which is already the text
/// it spells. [`declared_literals`] states these same six entries as the declaration spells
/// them.
const LITERALS: &[(&str, &str)] = &[
    ("RETRIES", "3"),
    ("OFFSET", "-2"),
    ("RATIO", "0.25"),
    ("DEBUG", "true"),
    ("QUIET", "false"),
    ("STAGE", "prod"),
];

/// The entries of [`LITERALS`] as the declaration spells them: the JSON value a template
/// states for each.
fn declared_literals() -> Value {
    json!({
        "RETRIES": 3,
        "OFFSET": -2,
        "RATIO": 0.25,
        "DEBUG": true,
        "QUIET": false,
        "STAGE": "prod"
    })
}

/// `GetFunction` (`GET /2015-03-31/functions/{FunctionName}`): what Lambda holds for the
/// function the stack created.
async fn get_function(router: &MultiServiceRouter, name: &str) -> (u16, Value) {
    let answer = lambda(
        router,
        "GET",
        &format!("https://lambda.{REGION}.amazonaws.com/2015-03-31/functions/{name}"),
        b"",
        &[],
    )
    .await;
    (answer.status, answer.body)
}

/// Deploy `template` as stack `stack` and assert the stack completed.
async fn deploy(
    router: &MultiServiceRouter,
    engine: &Arc<engine::InMemoryEngine>,
    case: &str,
    stack: &str,
    template: &str,
) {
    stage_code_objects(router, case, template).await;
    let (code, body) = create_stack(
        router,
        stack,
        template,
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
    settle_the_store(router, engine).await;
    let described = poll_to_terminal(router, case, stack).await;
    assert_eq!(
        xml_text(&described, "StackStatus").as_deref(),
        Some("CREATE_COMPLETE"),
        "case {case:?}: a literal where AWS Lambda takes a string must travel as its text, not \
         fail the stack, got {described}"
    );
}

#[tokio::test]
async fn a_literal_in_a_functions_environment_travels_as_its_text() {
    let case = "environment variables stated as number and boolean literals";
    let (router, engine) = router_with_engine();
    let stack = "lambdalit-environment";
    let template = sam_template(&format!(
        r#"{{"Reader":{{"Type":"AWS::Serverless::Function","Properties":{{
             "Handler":"index.handler","Runtime":"python3.12",
             "CodeUri":"s3://code-bucket/Reader.zip",
             "Environment":{{"Variables":{}}}}}}}}}"#,
        declared_literals()
    ));
    deploy(&router, &engine, case, stack, &template).await;

    let name = physical_id(&router, stack, "Reader").await;
    let (code, document) = get_function(&router, &name).await;
    assert_eq!(
        code, 200,
        "case {case:?}: expected Lambda to hold the function the stack created, got {code} with \
         {document}"
    );
    let reported = document
        .pointer("/Configuration/Environment/Variables")
        .cloned()
        .unwrap_or(Value::Null);
    for (name, text) in LITERALS {
        assert_eq!(
            reported.get(*name).and_then(Value::as_str),
            Some(*text),
            "case {case:?}: expected the variable {name:?} to carry {text:?}, got {reported}"
        );
    }
}

#[tokio::test]
async fn a_literal_in_a_functions_tags_travels_as_its_text() {
    let case = "tags stated as number and boolean literals";
    let (router, engine) = router_with_engine();
    let stack = "lambdalit-tags";
    let template = sam_template(&format!(
        r#"{{"Reader":{{"Type":"AWS::Serverless::Function","Properties":{{
             "Handler":"index.handler","Runtime":"python3.12",
             "CodeUri":"s3://code-bucket/Reader.zip",
             "Tags":{}}}}}}}"#,
        declared_literals()
    ));
    deploy(&router, &engine, case, stack, &template).await;

    let name = physical_id(&router, stack, "Reader").await;
    let (code, document) = get_function(&router, &name).await;
    assert_eq!(
        code, 200,
        "case {case:?}: expected Lambda to hold the function the stack created, got {code} with \
         {document}"
    );
    let reported = document.get("Tags").cloned().unwrap_or(Value::Null);
    for (name, text) in LITERALS {
        assert_eq!(
            reported.get(*name).and_then(Value::as_str),
            Some(*text),
            "case {case:?}: expected the tag {name:?} to carry {text:?}, got {reported}"
        );
    }
}
