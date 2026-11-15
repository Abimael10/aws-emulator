// SPDX-License-Identifier: CC0-1.0

//! `AWS::Bedrock::Flow` is a resource type a template may DECLARE: every operation that
//! submits a template admits a body declaring one, and the catalogue entries behind that say
//! what a resource of the type reports.
//!
//! The rule under test: **a resource type Amazon Web Services publishes belongs to the
//! catalogue a submitted template is held to, so a body declaring it is admitted and the type
//! reports the identity and the return values its own "Return values" section publishes** —
//! the half of the rule this crate owns on its own. It is the mirror of
//! `unrecognized_resource_types.rs`, which pins the other direction: a type CloudFormation
//! does NOT know is refused `Template format error: Unrecognized resource types: [<types>]`,
//! and that refusal must never be the answer for a type AWS publishes.
//!
//! What the stack then DOES with the declaration — the call to Amazon Bedrock Agent's own
//! `CreateFlow`, the preparation that makes the flow invocable, the flow that service holds
//! afterwards, its removal with the stack and the invocation of it through the Bedrock Agent
//! runtime plane — needs those services mounted beside this one, so it lives in the router's
//! own cross-service suite
//! (`core/router/tests/all/cloudformation_provisions_bedrock_flows.rs`). This suite asserts
//! admission alone, which is what a client sees before any resource is attempted.
//!
//! The rows differ along what the rule generalises over: whether the declaration names the
//! flow, whether it carries a description, a customer-managed key or the type's own `Tags`,
//! the shape of the declared `Definition` (one output node, two output nodes, none at all),
//! the template spelling (JSON and YAML), how many flows one body declares, and whether the
//! declaration stands beside a resource of another type.

use service_com_amazonaws_cloudformation_cloudformation::state::resource_types::{
    is_published, is_recognized, AWS_RESOURCE_TYPES,
};
use service_com_amazonaws_cloudformation_cloudformation::state::template::{
    identity_is_service_assigned, name_is_create_only, name_property_candidates,
    published_attribute_names, BEDROCK_FLOW,
};

use crate::stack_lifecycle::{call, create_stack, member, router, xml_text};

/// The fixed form of the refusal a type CloudFormation does not know is answered with, which
/// no row here may ever see.
const UNRECOGNIZED_PREFIX: &str = "Template format error: Unrecognized resource types: [";

/// The execution role the flow runs as, of the form `FlowExecutionRoleArn`'s own pattern
/// requires (`^arn:aws(-[^:]+)?:iam::([0-9]{12})?:role/(service-role/)?.+$`).
const EXECUTION_ROLE_ARN: &str = "arn:aws:iam::000000000000:role/service-role/cfn-flow";

/// A customer-managed key a declaration may state, of the form `KmsKeyArn`'s own pattern
/// requires.
const KEY_ARN: &str = "arn:aws:kms:us-east-1:000000000000:key/1234abcd-12ab-34cd-56ef-1234567890ab";

/// Every `Fn::GetAtt` attribute these naming rules compute for the type, in the sorted order a
/// resource's own attribute map holds them in: the ARN the owning service mints, the id inside
/// it, and the working version a created flow carries. The flow's `Status` and its two instants
/// are that service's to report and reach the resource off its own answer.
const PUBLISHED_ATTRIBUTES: &[&str] = &["Arn", "Id", "Version"];

/// The `Definition` document in the TEMPLATE's spelling — every property is the API member of
/// the same name with an upper-cased initial — with `outputs` output nodes fed from the one
/// input node.
fn definition(outputs: usize) -> String {
    let mut nodes = vec![
        r#"{"Name": "FlowInput", "Type": "Input", "Configuration": {"Input": {}},
            "Outputs": [{"Name": "document", "Type": "String"}]}"#
            .to_string(),
    ];
    let mut connections = Vec::new();
    for index in 0..outputs {
        nodes.push(format!(
            r#"{{"Name": "FlowOutput{index}", "Type": "Output", "Configuration": {{"Output": {{}}}},
                "Inputs": [{{"Name": "document", "Type": "String", "Expression": "$.data"}}]}}"#
        ));
        connections.push(format!(
            r#"{{"Name": "InputToOutput{index}", "Type": "Data", "Source": "FlowInput",
                "Target": "FlowOutput{index}",
                "Configuration": {{"Data": {{"SourceOutput": "document", "TargetInput": "document"}}}}}}"#
        ));
    }
    format!(
        r#"{{"Nodes": [{}], "Connections": [{}]}}"#,
        nodes.join(", "),
        connections.join(", ")
    )
}

/// One row: a template body declaring the type, and the name of the stack it is submitted
/// under.
struct Row {
    case: &'static str,
    stack_name: &'static str,
    template: String,
}

fn rows() -> Vec<Row> {
    vec![
        // A named flow with a one-hop definition, its ARN read back through `Ref`.
        Row {
            case: "a named flow with one output node, with Ref read back",
            stack_name: "bedrock-flow-admitted",
            template: format!(
                r#"{{"AWSTemplateFormatVersion": "2010-09-09",
                     "Resources": {{"Flow": {{"Type": "{BEDROCK_FLOW}",
                       "Properties": {{"Name": "cfn-flow-admitted",
                         "Description": "The flow this stack deploys.",
                         "ExecutionRoleArn": "{EXECUTION_ROLE_ARN}",
                         "Definition": {}}}}}}},
                     "Outputs": {{"FlowArn": {{"Value": {{"Ref": "Flow"}}}}}}}}"#,
                definition(1)
            ),
        },
        // The flow's name left to CloudFormation, a customer-managed key declared, and the
        // definition fanning the input out to two output nodes.
        Row {
            case: "an unnamed flow over a customer-managed key, two output nodes",
            stack_name: "bedrock-flow-generated-name",
            template: format!(
                r#"{{"Resources": {{"Flow": {{"Type": "{BEDROCK_FLOW}",
                     "Properties": {{"ExecutionRoleArn": "{EXECUTION_ROLE_ARN}",
                       "CustomerEncryptionKeyArn": "{KEY_ARN}",
                       "Definition": {}}}}}}},
                     "Outputs": {{"FlowId": {{"Value": {{"Fn::GetAtt": ["Flow", "Id"]}}}}}}}}"#,
                definition(2)
            ),
        },
        // A declaration carrying no definition at all: `definition` is an optional member of
        // `CreateFlow`, so the body is as admissible as one that states it.
        Row {
            case: "a flow that declares no definition",
            stack_name: "bedrock-flow-no-definition",
            template: format!(
                r#"{{"Resources": {{"Flow": {{"Type": "{BEDROCK_FLOW}",
                     "Properties": {{"Name": "cfn-flow-bare",
                       "ExecutionRoleArn": "{EXECUTION_ROLE_ARN}"}}}}}}}}"#
            ),
        },
        // The YAML spelling, with the type's own `Tags` and the tags of its test alias.
        Row {
            case: "the YAML spelling, declared tags and test-alias tags",
            stack_name: "bedrock-flow-yaml",
            template: format!(
                "\
Resources:
  Flow:
    Type: {BEDROCK_FLOW}
    Properties:
      Name: cfn-flow-yaml
      ExecutionRoleArn: {EXECUTION_ROLE_ARN}
      Tags:
        team: platform
      TestAliasTags:
        team: platform
      Definition:
        Nodes:
          - Name: FlowInput
            Type: Input
            Configuration:
              Input: {{}}
            Outputs:
              - Name: document
                Type: String
          - Name: FlowOutput0
            Type: Output
            Configuration:
              Output: {{}}
            Inputs:
              - Name: document
                Type: String
                Expression: $.data
        Connections:
          - Name: InputToOutput0
            Type: Data
            Source: FlowInput
            Target: FlowOutput0
            Configuration:
              Data:
                SourceOutput: document
                TargetInput: document
Outputs:
  FlowArn:
    Value: !Ref Flow
  FlowStatus:
    Value: !GetAtt Flow.Status
"
            ),
        },
        // Two flows in one body, beside a resource of another type: the catalogue admits every
        // type a body declares or none of them.
        Row {
            case: "two flows beside a resource of another type",
            stack_name: "bedrock-flow-mixed",
            template: format!(
                r#"{{"Resources": {{
                     "First": {{"Type": "{BEDROCK_FLOW}",
                       "Properties": {{"Name": "cfn-flow-first",
                         "ExecutionRoleArn": "{EXECUTION_ROLE_ARN}",
                         "Definition": {}}}}},
                     "Second": {{"Type": "{BEDROCK_FLOW}",
                       "Properties": {{"Name": "cfn-flow-second",
                         "ExecutionRoleArn": "{EXECUTION_ROLE_ARN}"}}}},
                     "Topic": {{"Type": "AWS::SNS::Topic",
                       "Properties": {{"TopicName": "bedrock-flow-topic"}}}}}}}}"#,
                definition(1)
            ),
        },
    ]
}

/// Assert one answer admitted the body rather than refusing its resource types.
fn assert_admitted(case: &str, status: u16, body: &str) {
    let message = xml_text(body, "Message").unwrap_or_default();
    assert!(
        !message.starts_with(UNRECOGNIZED_PREFIX),
        "case {case:?}: expected a type Amazon Web Services publishes to be recognised, got \
         {message:?}"
    );
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 for a body declaring a published type, got {status} \
         with body {body}"
    );
}

/// Every operation that submits a template for deployment admits a body declaring the type,
/// and so does the operation whose whole answer is a verdict on the body.
#[tokio::test]
async fn every_operation_that_takes_a_template_admits_a_bedrock_flow() {
    for row in rows() {
        let router = router();
        let (status, body) = create_stack(&router, row.stack_name, &row.template, &[]).await;
        assert_admitted(row.case, status, &body);
        let stack_id = xml_text(&body, "StackId").unwrap_or_default();
        assert!(
            stack_id.contains(":stack/"),
            "case {:?}: expected CreateStack to answer a stack id, got {body}",
            row.case
        );

        let (status, body) = call(
            &router,
            "ValidateTemplate",
            &[member("TemplateBody", &row.template)],
        )
        .await;
        assert_admitted(row.case, status, &body);

        let (status, body) = call(
            &router,
            "CreateChangeSet",
            &[
                member("StackName", &format!("{}-change-set", row.stack_name)),
                member("ChangeSetName", "a-change-set"),
                member("ChangeSetType", "CREATE"),
                member("TemplateBody", &row.template),
            ],
        )
        .await;
        assert_admitted(row.case, status, &body);
    }
}

/// The catalogue entries the rule rests on: the type is published and recognised, a near-miss
/// spelling of it still is not, and the type reports the identity and the return values its own
/// "Return values" section publishes.
#[test]
fn the_catalogue_publishes_the_bedrock_flow_type_and_what_it_returns() {
    assert!(
        AWS_RESOURCE_TYPES.contains(&BEDROCK_FLOW),
        "expected {BEDROCK_FLOW:?} in the published catalogue, it is absent"
    );
    assert!(
        is_published(BEDROCK_FLOW) && is_recognized(BEDROCK_FLOW),
        "expected {BEDROCK_FLOW:?} to be a type a template may declare, it is not"
    );
    // The catalogue is a list of names, not a shape test: a name it does not carry stays
    // unrecognised however close it is to one it does.
    for misspelled in [
        "AWS::Bedrock::Flows",
        "AWS::Bedrock::FlowDefinition",
        "AWS::BedrockFlow::Flow",
    ] {
        assert!(
            !is_recognized(misspelled),
            "expected {misspelled:?} to stay unrecognised, it is recognised"
        );
    }
    // "Ref returns the flow ARN": Amazon Bedrock Agent mints the id and the ARN around it, so
    // the identity in force is the one that service assigned rather than a name these rules
    // could compute.
    assert!(
        identity_is_service_assigned(BEDROCK_FLOW),
        "expected the flow's identity to be the owning service's, it is not"
    );
    // The NAME a declaration states is still CloudFormation's, and `UpdateFlow` takes a new
    // one by the flow's own identifier — so renaming a flow modifies it rather than recreating
    // it.
    assert!(
        name_property_candidates(BEDROCK_FLOW).iter().any(|property| property == "Name"),
        "expected the flow's Name property to be among the name candidates, got {:?}",
        name_property_candidates(BEDROCK_FLOW)
    );
    assert!(
        !name_is_create_only(BEDROCK_FLOW),
        "expected a renamed flow to be updated in place, it is reported as create-only"
    );
    assert_eq!(
        published_attribute_names(BEDROCK_FLOW),
        PUBLISHED_ATTRIBUTES,
        "expected the type's published attributes, got {:?}",
        published_attribute_names(BEDROCK_FLOW)
    );
}
