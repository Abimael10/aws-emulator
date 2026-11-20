// SPDX-License-Identifier: CC0-1.0

//! The Amazon Bedrock AgentCore SANDBOX resource types in a stack's `Outputs`.
//!
//! The rule under test is the one `stack_outputs.rs` states, applied to the AgentCore
//! types the return-value catalogue did not carry: `DescribeStacks` reports each declared
//! `Output` as a COMPLETE pair — an `OutputKey` AND its resolved `OutputValue` — and the
//! value of a `Ref`/`Fn::GetAtt` output is the return value the resource TYPE publishes,
//! in the identifier and ARN form the owning API's model fixes, so a client that deploys
//! a template can read the identifiers of what it just created. An expression whose value
//! nothing here can compute is reported with its key and NO value instead of an invented
//! one.
//!
//! Every resource these rows declare is really created in the Amazon Bedrock AgentCore
//! control plane the stack calls (`state::provisioner::bedrock_agentcore_gateways`), which
//! MINTS each identifier the stack then publishes — so each declaration carries the
//! properties that service's own create requires, and what the outputs carry is what it
//! answered. The provisioning half of the rule, read back through that service's own wire,
//! is `core/router/tests/all/cloudformation_provisions_agentcore_resources.rs`.
//!
//! What the rows span: both sandbox types (`AWS::BedrockAgentCore::CodeInterpreter` and
//! `AWS::BedrockAgentCore::Browser`), `Ref` and each `Fn::GetAtt` attribute, both template
//! spellings, both sandbox network modes, a named and an unnamed resource, two resources of
//! one type in one stack, and an attribute nothing publishes. The forms asserted are the ones
//! the AgentCore control plane's own model fixes — `smithy_query(query="operation",
//! operation="CreateCodeInterpreter,CreateBrowser,CreateAgentRuntime")` on
//! `services/service-com-amazonaws-bedrockagentcorecontrol-amazonbedrockagentcorecontrol`
//! — never a literal taken from one recorded stack.

use crate::stack_lifecycle::{
    create_stack, describe_stack, router, wait_for_the_transition, xml_text,
};
use crate::stack_outputs::{
    agentcore_id_parts, is_agentcore_arn, is_agentcore_runtime_id, output_for, wire_outputs,
};
use service_com_amazonaws_cloudformation_cloudformation::state::status;

/// The LABEL half of an AgentCore identifier, when `value` is one at all.
///
/// The identifier and ARN FORMS are `stack_outputs.rs`'s — `agentcore_id_parts` and
/// `is_agentcore_runtime_id`, which is the `^[a-zA-Z][a-zA-Z0-9_]{0,47}-[a-zA-Z0-9]{10}$`
/// form `CodeInterpreterId` and `BrowserId` share with `AgentRuntimeId`, and
/// `is_agentcore_arn` — so this suite holds no second copy of them. What it adds is the
/// LABEL check those predicates cannot express: that the label is the name the template
/// declared, which is the name the control plane created the sandbox under.
fn label_of(value: &str) -> Option<&str> {
    agentcore_id_parts(value).map(|(label, _)| label)
}

/// The id portion of an AgentCore ARN, `arn:…:<segment>/<id>`.
fn id_in_arn(value: &str) -> Option<&str> {
    value.rsplit('/').next()
}

/// What one declared output of an AgentCore stack must carry.
enum Expect {
    /// A modelled identifier whose label is EXACTLY this one — the name the template
    /// declared — and whose suffix no row can spell, because the owning service mints it.
    Id(&'static str),
    /// A modelled identifier whose label BEGINS with this, for a resource the template does
    /// not name: CloudFormation generates `<stack>-<logical id>-<unique id>`, whose `-` the
    /// name member's alphabet does not admit, so the resource is created under that name
    /// reduced to it — a label no row can spell past its stem, and a digest it cannot spell
    /// at all.
    IdLabelled(&'static str),
    /// An AgentCore ARN whose resource portion is `<segment>/<a modelled id labelled
    /// with this>`.
    Arn {
        segment: &'static str,
        label: &'static str,
    },
    /// The output is reported (its key is there) and carries NO value, because nothing
    /// here can compute the expression.
    NoValue,
}

impl Expect {
    /// Whether `value` satisfies this expectation, and the English of what was required.
    fn check(&self, value: Option<&str>) -> (bool, String) {
        match self {
            Self::Id(label) => (
                value.is_some_and(|actual| {
                    is_agentcore_runtime_id(actual) && label_of(actual) == Some(*label)
                }),
                format!("a modelled identifier labelled {label:?} + `-` + 10 characters"),
            ),
            Self::IdLabelled(prefix) => (
                value.is_some_and(|actual| {
                    is_agentcore_runtime_id(actual)
                        && label_of(actual).is_some_and(|label| label.starts_with(prefix))
                }),
                format!("a modelled identifier whose label begins {prefix:?}"),
            ),
            Self::Arn { segment, label } => {
                let ok = value.is_some_and(|actual| {
                    is_agentcore_arn(actual, segment, is_agentcore_runtime_id)
                        && id_in_arn(actual).and_then(label_of) == Some(*label)
                });
                (
                    ok,
                    format!(
                        "an agentcore {segment} ARN naming a modelled identifier labelled \
                         {label:?}"
                    ),
                )
            }
            Self::NoValue => (value.is_none(), "no OutputValue at all".to_string()),
        }
    }
}

/// One row: a template, and what each of its declared outputs must carry.
struct Case {
    case: &'static str,
    stack_name: &'static str,
    template: &'static str,
    expected: Vec<(&'static str, Expect)>,
}

fn cases() -> Vec<Case> {
    vec![
        Case {
            // A code interpreter and a browser named by the template, in the YAML
            // spelling a CFN-driven integration writes. `Ref` answers the type's primary
            // identifier, its own id, and `Fn::GetAtt` the id and the ARN; the `-custom`
            // ARN segment is the one a CUSTOMER-created sandbox carries (the bare
            // `code-interpreter`/`browser` segment is the service's built-in
            // `aws.codeinterpreter.v1` / `aws.browser.v1`, which no template declares).
            //
            // `NetworkConfiguration` is `@required` on both creates, and each type's own
            // default network mode is the one declared here (`SANDBOX` for a code
            // interpreter, `PUBLIC` for a browser).
            case: "a named code interpreter and browser: id, arn and Ref",
            stack_name: "agentcore-sandboxes",
            template: concat!(
                "Resources:\n",
                "  Interpreter:\n",
                "    Type: AWS::BedrockAgentCore::CodeInterpreter\n",
                "    Properties:\n",
                "      Name: probeInterpreter\n",
                "      NetworkConfiguration:\n",
                "        NetworkMode: SANDBOX\n",
                "  Browser:\n",
                "    Type: AWS::BedrockAgentCore::Browser\n",
                "    Properties:\n",
                // A `_` IS inside the sandbox name's alphabet
                // (`SandboxName`: `^[a-zA-Z][a-zA-Z0-9_]{0,47}$`).
                "      Name: probe_browser\n",
                "      NetworkConfiguration:\n",
                "        NetworkMode: PUBLIC\n",
                "Outputs:\n",
                "  InterpreterId:\n",
                "    Value: !GetAtt Interpreter.CodeInterpreterId\n",
                "  InterpreterArn:\n",
                "    Value: !GetAtt Interpreter.CodeInterpreterArn\n",
                "  InterpreterRef:\n",
                "    Value: !Ref Interpreter\n",
                "  BrowserId:\n",
                "    Value: !GetAtt Browser.BrowserId\n",
                "  BrowserArn:\n",
                "    Value: !GetAtt Browser.BrowserArn\n",
                "  BrowserRef:\n",
                "    Value: !Ref Browser\n",
            ),
            expected: vec![
                ("InterpreterId", Expect::Id("probeInterpreter")),
                (
                    "InterpreterArn",
                    Expect::Arn {
                        segment: "code-interpreter-custom",
                        label: "probeInterpreter",
                    },
                ),
                ("InterpreterRef", Expect::Id("probeInterpreter")),
                ("BrowserId", Expect::Id("probe_browser")),
                (
                    "BrowserArn",
                    Expect::Arn {
                        segment: "browser-custom",
                        label: "probe_browser",
                    },
                ),
                ("BrowserRef", Expect::Id("probe_browser")),
            ],
        },
        Case {
            // The JSON spelling with long-form intrinsics, and the whole of the property
            // document reaching the owning create: a sandbox declared with a description
            // and an execution role beside its network configuration still publishes the
            // identifier and the ARN that service minted.
            case: "json, long-form GetAtt: a fully configured code interpreter",
            stack_name: "agentcore-json-sandbox",
            template: r#"{"Resources":{
                 "Interpreter":{"Type":"AWS::BedrockAgentCore::CodeInterpreter",
                   "Properties":{"Name":"jsonInterpreter",
                     "Description":"declared in the JSON spelling",
                     "ExecutionRoleArn":"arn:aws:iam::000000000000:role/SandboxRole",
                     "NetworkConfiguration":{"NetworkMode":"SANDBOX"}}}},
               "Outputs":{
                 "InterpreterId":{"Value":{"Fn::GetAtt":["Interpreter","CodeInterpreterId"]}},
                 "InterpreterArn":{"Value":{"Fn::GetAtt":["Interpreter","CodeInterpreterArn"]}}}}"#,
            expected: vec![
                ("InterpreterId", Expect::Id("jsonInterpreter")),
                (
                    "InterpreterArn",
                    Expect::Arn {
                        segment: "code-interpreter-custom",
                        label: "jsonInterpreter",
                    },
                ),
            ],
        },
        Case {
            // An UNNAMED sandbox still resolves: the type's name member admits no `-`, so
            // the resource is created under CloudFormation's generated
            // `<stack name>-<logical id>-<unique id>` name REDUCED to that alphabet, and the
            // identifier the control plane mints carries that label — never reported as no
            // value, and never a name the owning model forbids.
            case: "an unnamed code interpreter still publishes a modelled arn",
            stack_name: "agentcore-unnamed",
            template: concat!(
                "Resources:\n",
                "  Anon:\n",
                "    Type: AWS::BedrockAgentCore::CodeInterpreter\n",
                "    Properties:\n",
                "      NetworkConfiguration:\n",
                "        NetworkMode: SANDBOX\n",
                "Outputs:\n",
                "  InterpreterId:\n",
                "    Value: !GetAtt Anon.CodeInterpreterId\n",
                "  InterpreterRef:\n",
                "    Value: !Ref Anon\n",
            ),
            expected: vec![
                ("InterpreterId", Expect::IdLabelled("agentcoreunnamedAnon")),
                ("InterpreterRef", Expect::IdLabelled("agentcoreunnamedAnon")),
            ],
        },
        Case {
            // The complement, which keeps the rule honest: an attribute NOTHING here
            // publishes is reported with its key and no value. A sandbox's RUNNING state
            // is one — `Status` is no property of either type's return values.
            case: "an attribute nothing publishes carries no value",
            stack_name: "agentcore-unknowable",
            template: concat!(
                "Resources:\n",
                "  Interpreter:\n",
                "    Type: AWS::BedrockAgentCore::CodeInterpreter\n",
                "    Properties:\n",
                // A name of its own: one code interpreter per name, so a row cannot
                // re-declare the name an earlier row's stack already holds.
                "      Name: silentInterpreter\n",
                "      NetworkConfiguration:\n",
                "        NetworkMode: SANDBOX\n",
                "Outputs:\n",
                "  InterpreterId:\n",
                "    Value: !GetAtt Interpreter.CodeInterpreterId\n",
                "  Status:\n",
                "    Value: !GetAtt Interpreter.Status\n",
            ),
            expected: vec![
                ("InterpreterId", Expect::Id("silentInterpreter")),
                ("Status", Expect::NoValue),
            ],
        },
    ]
}

#[tokio::test]
async fn an_agentcore_stacks_declared_outputs_carry_their_resolved_values() {
    let router = router();

    for case in cases() {
        let (status_code, body) = create_stack(&router, case.stack_name, case.template, &[]).await;
        assert_eq!(
            status_code, 200,
            "case {:?}: expected CreateStack to succeed, got {status_code} {body}",
            case.case
        );
        wait_for_the_transition().await;

        let (status_code, body) = describe_stack(&router, case.stack_name).await;
        assert_eq!(
            status_code, 200,
            "case {:?}: expected DescribeStacks to succeed, got {status_code} {body}",
            case.case
        );
        assert_eq!(
            xml_text(&body, "StackStatus").as_deref(),
            Some(status::CREATE_COMPLETE),
            "case {:?}: the stack must have reached CREATE_COMPLETE, got {body}",
            case.case
        );

        let outputs = wire_outputs(&body);
        for (key, expected) in &case.expected {
            let output = output_for(&outputs, key).unwrap_or_else(|| {
                panic!(
                    "case {:?}: output {key:?} must be reported, got {outputs:?}",
                    case.case
                )
            });
            let (ok, required) = expected.check(output.value.as_deref());
            assert!(
                ok,
                "case {:?}: output {key:?} must carry {required}, got {:?}",
                case.case, output.value
            );
        }
    }
}

/// Two sandboxes of one type in one stack get DIFFERENT identifiers, and a second read of
/// the same stack reports the SAME ones: an identifier belongs to the resource the owning
/// service created, not to the read.
#[tokio::test]
async fn agentcore_sandbox_identifiers_are_unique_per_resource_and_stable_across_reads() {
    let router = router();
    let template = concat!(
        "Resources:\n",
        "  First:\n",
        "    Type: AWS::BedrockAgentCore::CodeInterpreter\n",
        "    Properties:\n",
        "      Name: firstInterpreter\n",
        "      NetworkConfiguration:\n",
        "        NetworkMode: SANDBOX\n",
        "  Second:\n",
        "    Type: AWS::BedrockAgentCore::CodeInterpreter\n",
        "    Properties:\n",
        "      Name: secondInterpreter\n",
        "      NetworkConfiguration:\n",
        "        NetworkMode: SANDBOX\n",
        "Outputs:\n",
        "  FirstId:\n",
        "    Value: !GetAtt First.CodeInterpreterId\n",
        "  SecondId:\n",
        "    Value: !GetAtt Second.CodeInterpreterId\n",
    );
    let (status_code, body) = create_stack(&router, "agentcore-sandbox-ids", template, &[]).await;
    assert_eq!(
        status_code, 200,
        "case \"two code interpreters\": expected CreateStack to succeed, got {status_code} {body}"
    );
    wait_for_the_transition().await;

    let (status_code, body) = describe_stack(&router, "agentcore-sandbox-ids").await;
    assert_eq!(
        status_code, 200,
        "case \"two code interpreters\": expected DescribeStacks to succeed, got {status_code} \
         {body}"
    );
    let outputs = wire_outputs(&body);
    let first = output_for(&outputs, "FirstId").and_then(|output| output.value.clone());
    let second = output_for(&outputs, "SecondId").and_then(|output| output.value.clone());
    assert!(
        first.as_deref().is_some_and(is_agentcore_runtime_id)
            && second.as_deref().is_some_and(is_agentcore_runtime_id),
        "case \"two code interpreters\": both identifiers must be reported in the modelled \
         form, got {outputs:?}"
    );
    assert_ne!(
        first, second,
        "case \"two code interpreters\": two resources of one type must not share an \
         identifier, got {first:?} and {second:?}"
    );

    let (status_code, body) = describe_stack(&router, "agentcore-sandbox-ids").await;
    assert_eq!(
        status_code, 200,
        "case \"two code interpreters\": expected the second DescribeStacks to succeed, got \
         {status_code} {body}"
    );
    let again = wire_outputs(&body);
    assert_eq!(
        output_for(&again, "FirstId").and_then(|output| output.value.clone()),
        first,
        "case \"two code interpreters\": a second read must report the same identifier, got \
         {again:?}"
    );
}
