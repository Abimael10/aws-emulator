// SPDX-License-Identifier: CC0-1.0

//! What the AWS SAM transform does with a `Policies` entry that names an AWS managed policy
//! by its NAME rather than by its ARN, read back out of IAM over that service's own wire.
//!
//! The rule every row here holds the service to: **a `Policies` string entry names a managed
//! policy to ATTACH to the role the transform generates, either by that policy's ARN — which
//! is attached exactly as the declaration spells it — or by its NAME, which is attached under
//! the ARN AWS publishes that policy at, `arn:<partition>:iam::aws:policy<path><name>`,
//! carrying the path AWS files the policy under; a name AWS publishes no policy under resolves
//! to no ARN at all, so the role IAM is asked for is one it refuses and the stack rolls
//! back.** `ManagedPolicyArns` takes ARNs, so a name left as the declaration wrote it is a
//! role IAM refuses and a stack that never completes.
//!
//! The recorded trace this suite was written for is ONE row of the table: a function whose
//! `Policies` carries the single bare name `AmazonS3ReadOnlyAccess`. The rows differ along
//! what the rule generalises over — whether the entry is a name or an ARN, which PATH AWS
//! files the named policy under (the root and `/service-role/`), whether AWS publishes a
//! policy under the name at all, how many entries there are and in which order, the
//! single-value and the list spelling of the property, mixing names with the other two forms
//! the property accepts (an ARN, a policy template), and which serverless resource the role is
//! generated for (a function and a state machine).
//!
//! Every expected ARN is computed here from the name the row declares and the path AWS
//! publishes that policy at, named in a comment beside the row, never read back out of the
//! answer being checked.

use std::sync::Arc;

use smithy_server_runtime::MultiServiceRouter;

use crate::change_set_lifecycle::settle_the_store;
use crate::elbv2_resource_provisioning::poll_to_terminal;
use crate::sam_policies_shorthand::function_with_policies;
use crate::sam_transform::sam_template;
use crate::stack_resource_provisioning::{physical_id, stage_code_objects};
use crate::stack_lifecycle::{
    create_stack, iam, kv, resource_event_reason, router_with_engine, xml_text, xml_texts,
};

/// The AWS managed policy every Lambda execution role the transform generates carries, as the
/// ARN IAM holds it under: AWS files it under `/service-role/`.
const BASIC_EXECUTION_ARN: &str =
    "arn:aws:iam::aws:policy/service-role/AWSLambdaBasicExecutionRole";

/// One row of the table: the `Policies` the declaration states, and the managed policies the
/// generated role must therefore have attached.
struct Row {
    case: &'static str,
    stack: &'static str,
    template: String,
    /// The declaration whose generated role the row reads: the role's logical id is that id
    /// plus `Role`.
    owner: &'static str,
    /// The managed policies the role must have attached, beside nothing else.
    attached: Vec<String>,
    /// The ARNs among those that name a policy AWS's catalogue publishes, which IAM must
    /// therefore resolve to a readable policy of that name.
    published: Vec<(String, &'static str)>,
}

/// The ARN of an AWS managed policy AWS files at the ROOT of its own namespace.
fn root_policy(name: &str) -> String {
    format!("arn:aws:iam::aws:policy/{name}")
}

/// The ARN of an AWS managed policy AWS files under the `/service-role/` path.
fn service_role_policy(name: &str) -> String {
    format!("arn:aws:iam::aws:policy/service-role/{name}")
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            // Source: the AWS Managed Policy Reference entry for `AmazonS3ReadOnlyAccess`,
            // which AWS publishes at the root of its policy namespace.
            case: "one bare managed-policy name, the single-value spelling of the property",
            stack: "sambare-one-name",
            template: sam_template(&format!(
                "{{{}}}",
                function_with_policies("Reader", r#""AmazonS3ReadOnlyAccess""#)
            )),
            owner: "Reader",
            attached: vec![
                BASIC_EXECUTION_ARN.to_string(),
                root_policy("AmazonS3ReadOnlyAccess"),
            ],
            published: vec![(
                root_policy("AmazonS3ReadOnlyAccess"),
                "AmazonS3ReadOnlyAccess",
            )],
        },
        Row {
            // Source: the AWS Managed Policy Reference entries for
            // `AWSLambdaVPCAccessExecutionRole` and `AmazonECSTaskExecutionRolePolicy`, both
            // of which AWS files under `/service-role/` — the path is part of the published
            // ARN, so a name is not attachable under the root.
            case: "two bare names AWS files under a path of its own",
            stack: "sambare-service-role-path",
            template: sam_template(&format!(
                "{{{}}}",
                function_with_policies(
                    "Worker",
                    r#"["AWSLambdaVPCAccessExecutionRole",
                        "AmazonECSTaskExecutionRolePolicy"]"#
                )
            )),
            owner: "Worker",
            attached: vec![
                BASIC_EXECUTION_ARN.to_string(),
                service_role_policy("AWSLambdaVPCAccessExecutionRole"),
                service_role_policy("AmazonECSTaskExecutionRolePolicy"),
            ],
            published: vec![
                (
                    service_role_policy("AWSLambdaVPCAccessExecutionRole"),
                    "AWSLambdaVPCAccessExecutionRole",
                ),
                (
                    service_role_policy("AmazonECSTaskExecutionRolePolicy"),
                    "AmazonECSTaskExecutionRolePolicy",
                ),
            ],
        },
        Row {
            // Source: the AWS Managed Policy Reference entry for `CloudWatchLogsFullAccess`
            // (root). A name beside an ARN, an inline document and a policy template: the
            // three forms the property accepts, each entry keeping the position it was
            // declared at.
            case: "a bare name beside an ARN, a document and a policy template",
            stack: "sambare-mixed",
            template: sam_template(&format!(
                "{{{}}}",
                function_with_policies(
                    "Mixed",
                    &format!(
                        r#"["CloudWatchLogsFullAccess",
                            "{}",
                            {{"Version":"2012-10-17","Statement":[{{"Effect":"Allow",
                              "Action":"sns:Publish","Resource":"*"}}]}},
                            {{"SQSSendMessagePolicy":{{"QueueName":"jobs"}}}}]"#,
                        root_policy("AmazonS3ReadOnlyAccess")
                    )
                )
            )),
            owner: "Mixed",
            attached: vec![
                BASIC_EXECUTION_ARN.to_string(),
                root_policy("CloudWatchLogsFullAccess"),
                root_policy("AmazonS3ReadOnlyAccess"),
            ],
            published: vec![
                (
                    root_policy("CloudWatchLogsFullAccess"),
                    "CloudWatchLogsFullAccess",
                ),
                (
                    root_policy("AmazonS3ReadOnlyAccess"),
                    "AmazonS3ReadOnlyAccess",
                ),
            ],
        },
        Row {
            // Source: the AWS Managed Policy Reference entry for `AWSXrayWriteOnlyAccess`
            // (root). A state machine's role is generated by the same expansion, so the same
            // rule holds for the policies it attaches — and that role carries no managed
            // policy of the transform's own.
            case: "a bare name on the role generated for a state machine",
            stack: "sambare-state-machine",
            template: sam_template(
                r#"{"Flow":{"Type":"AWS::Serverless::StateMachine","Properties":{
                     "Definition":{"StartAt":"Done","States":{"Done":{"Type":"Succeed"}}},
                     "Policies":["AWSXrayWriteOnlyAccess"]}}}"#,
            ),
            owner: "Flow",
            attached: vec![root_policy("AWSXrayWriteOnlyAccess")],
            published: vec![(
                root_policy("AWSXrayWriteOnlyAccess"),
                "AWSXrayWriteOnlyAccess",
            )],
        },
    ]
}

/// Deploy `template` as stack `stack`, settle the store and assert the stack completed.
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
    let body = poll_to_terminal(router, case, stack).await;
    assert_eq!(
        xml_text(&body, "StackStatus").as_deref(),
        Some("CREATE_COMPLETE"),
        "case {case:?}: a declaration naming a managed policy by NAME must deploy the role \
         the transform generated, got {body}"
    );
}

/// The managed policies IAM reports as attached to `role`, sorted.
async fn attached_policies(router: &MultiServiceRouter, case: &str, role: &str) -> Vec<String> {
    let (code, body) = iam(router, "ListAttachedRolePolicies", &[("RoleName", role)]).await;
    assert_eq!(
        code, 200,
        "case {case:?}: expected ListAttachedRolePolicies({role:?}) to answer, got {code} {body}"
    );
    let mut arns = xml_texts(&body, "PolicyArn");
    arns.sort();
    arns
}

#[tokio::test]
async fn a_bare_managed_policy_name_is_attached_under_the_arn_aws_publishes_it_at() {
    let (router, engine) = router_with_engine();
    for row in rows() {
        let case = row.case;
        deploy(&router, &engine, case, row.stack, &row.template).await;
        let role = physical_id(&router, row.stack, &format!("{}Role", row.owner)).await;

        let mut expected = row.attached.clone();
        expected.sort();
        let attached = attached_policies(&router, case, &role).await;
        assert_eq!(
            attached, expected,
            "case {case:?}: expected exactly the managed policies the declaration names, each \
             under the ARN AWS publishes it at ({expected:?}), got {attached:?}"
        );

        // ...and each of those ARNs names the policy AWS publishes under that name: the
        // expansion resolves the name to the published policy rather than to any ARN that
        // merely parses.
        for (arn, name) in &row.published {
            let (code, body) = iam(&router, "GetPolicy", &[("PolicyArn", arn.as_str())]).await;
            assert_eq!(
                code, 200,
                "case {case:?}: expected IAM to hold {arn:?}, got {code} {body}"
            );
            assert_eq!(
                xml_text(&body, "PolicyName").as_deref(),
                Some(*name),
                "case {case:?}: expected {arn:?} to name the published policy {name:?}, got \
                 {body}"
            );
        }
    }
}

/// RULE (the unresolvable half): a `Policies` entry naming a managed policy AWS publishes
/// none of resolves to no ARN at all, so the role IAM is asked for carries a value
/// `ManagedPolicyArns` does not take — IAM refuses it, and the stack rolls back rather than
/// deploying a role the declaration could not grant.
#[tokio::test]
async fn a_name_aws_publishes_no_policy_under_rolls_the_stack_back() {
    let (router, engine) = router_with_engine();
    // The rows differ in HOW the name fails to name a published policy: one no AWS namespace
    // ever carried, a misspelling of one AWS does publish, and the name of an AWS SAM POLICY
    // TEMPLATE, which the property accepts only as the object that applies it.
    for (case, stack, name) in [
        (
            "a name no AWS managed policy is published under",
            "sambare-unknown",
            "NotARealManagedPolicy",
        ),
        (
            "a misspelling of a policy AWS does publish",
            "sambare-misspelled",
            "AmazonS3ReadOnlyAccesss",
        ),
        (
            "the name of a policy TEMPLATE, which is no managed policy",
            "sambare-template-name",
            "S3ReadPolicy",
        ),
    ] {
        let template = sam_template(&format!(
            "{{{}}}",
            function_with_policies("Reader", &format!(r#"["{name}"]"#))
        ));
        stage_code_objects(&router, case, &template).await;
        let (code, body) = create_stack(
            &router,
            stack,
            &template,
            &kv(&[
                ("Capabilities.member.1", "CAPABILITY_IAM"),
                ("Capabilities.member.2", "CAPABILITY_AUTO_EXPAND"),
            ]),
        )
        .await;
        assert_eq!(
            code, 200,
            "case {case:?}: expected CreateStack to be accepted — the stack reports the outcome \
             — got {code} {body}"
        );
        settle_the_store(&router, &engine).await;
        let described = poll_to_terminal(&router, case, stack).await;
        assert_eq!(
            xml_text(&described, "StackStatus").as_deref(),
            Some("ROLLBACK_COMPLETE"),
            "case {case:?}: expected the unresolved name to fail the role and roll the stack \
             back, got {described}"
        );
        let reason = resource_event_reason(&router, stack, "ReaderRole", "CREATE_FAILED").await;
        assert!(
            reason.contains("Service: Iam") && reason.contains("Status Code: 400"),
            "case {case:?}: expected the role to fail on IAM's own refusal of the unresolved \
             entry {name:?}, got {reason:?}"
        );
    }
}

/// RULE (the other half): the role a bare name is attached to is the one the function
/// executes as, so the function the same declaration generates is deployed too — a name that
/// never resolved left the stack with no function at all.
#[tokio::test]
async fn the_function_whose_role_carries_a_named_policy_is_deployed_with_it() {
    let case = "a function whose only policy is a bare managed-policy name";
    let (router, engine) = router_with_engine();
    let template = sam_template(&format!(
        "{{{}}}",
        function_with_policies("Deployed", r#"["AWSLambdaExecute"]"#)
    ));
    deploy(&router, &engine, case, "sambare-function", &template).await;

    let function = physical_id(&router, "sambare-function", "Deployed").await;
    assert!(
        !function.is_empty(),
        "case {case:?}: expected the stack to publish the function it created"
    );
    let role = physical_id(&router, "sambare-function", "DeployedRole").await;
    let attached = attached_policies(&router, case, &role).await;
    // Source: the AWS Managed Policy Reference entry for `AWSLambdaExecute`, which AWS
    // publishes at the root of its namespace.
    let mut expected = vec![
        BASIC_EXECUTION_ARN.to_string(),
        root_policy("AWSLambdaExecute"),
    ];
    expected.sort();
    assert_eq!(
        attached, expected,
        "case {case:?}: expected the deployed role to carry the named policy, got {attached:?}"
    );
}
