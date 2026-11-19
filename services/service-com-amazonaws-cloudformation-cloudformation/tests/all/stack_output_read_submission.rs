// SPDX-License-Identifier: CC0-1.0

//! A template that READS another stack's output is a template every operation taking a body
//! accepts, end to end over the awsQuery wire.
//!
//! The rule under test: `Fn::GetStackOutput` is one of the functions this service advertises
//! as supported and resolves itself, so no operation that takes a template body answers a
//! template using it with the unsupported-function refusal — the body is accepted, and a
//! deployment resolves the reference to the value the named stack's output carries, whichever
//! of the two spellings the function's arguments take: the NAMED members
//! (`{"StackName": …, "OutputName": …}`) or the POSITIONAL pair (`[stack, output]`), the
//! stack first. A reference naming an output no stack of the Region carries has no value, so
//! the deployment fails with CloudFormation's own reason instead of deploying a blank.
//!
//! `stack_output_reads.rs` is the suite that spans WHAT one read resolves to through
//! `CreateStack` (the stack addressed by name and by id, the snapshot the reader keeps, the
//! value reaching the service that owns a resource). This suite spans the other two axes the
//! rule generalises over: the OPERATION the body was submitted to — the three that submit one
//! for deployment, the one whose whole answer is a verdict on it, and the one that only
//! summarises it — and the SPELLING of the function's two arguments, each row read back
//! through the stack the deployment produced.

use smithy_server_runtime::MultiServiceRouter;

use crate::stack_lifecycle::{
    call, create_stack, member, router, settled_stack, xml_text, xml_texts,
};
use service_com_amazonaws_cloudformation_cloudformation::state::stack_outputs::{
    no_output_named, Reference,
};
use service_com_amazonaws_cloudformation_cloudformation::state::{change_set_status, status};

/// The stack every row reads an output off.
const PRODUCER: &str = "submitted-read-producer";

/// The value of the producer's output that carries NO `Export`.
const PLAIN_VALUE: &str = "plain-submitted-value";

/// The value of the producer's exported output.
const EXPORTED_VALUE: &str = "exported-submitted-value";

/// A template that is well-formed and deploys, used as the stack the revision-submitting
/// operations are held against.
const DEPLOYABLE: &str = r#"{"Resources":{"Holder":{"Type":
    "AWS::CloudFormation::WaitConditionHandle"}}}"#;

/// The producer's template: one output with a `Value` and no `Export`, and one exported.
fn producer_template() -> String {
    format!(
        r#"{{"Resources":{{"Handle":{{"Type":"AWS::CloudFormation::WaitConditionHandle"}}}},
            "Outputs":{{
              "Plain":{{"Value":"{PLAIN_VALUE}"}},
              "Shared":{{"Value":"{EXPORTED_VALUE}",
                         "Export":{{"Name":"submitted-read-shared"}}}}}}}}"#
    )
}

/// Deploy the producer and answer its stack id, once its outputs are published.
async fn deploy_producer(router: &MultiServiceRouter) -> String {
    let (status_code, body) = create_stack(router, PRODUCER, &producer_template(), &[]).await;
    assert_eq!(
        status_code, 200,
        "expected the producer's CreateStack to succeed, got {status_code} {body}"
    );
    let stack_id = xml_text(&body, "StackId").expect("CreateStack answers a StackId");
    let settled = settled_stack(router, "the producer", PRODUCER).await;
    assert_eq!(
        xml_text(&settled, "StackStatus").as_deref(),
        Some(status::CREATE_COMPLETE),
        "expected the producer to deploy, got {settled}"
    );
    stack_id
}

/// The `(OutputKey, OutputValue)` pairs the settled stack reports.
async fn outputs_of(
    case: &str,
    router: &MultiServiceRouter,
    stack_name: &str,
) -> Vec<(String, String)> {
    let body = settled_stack(router, case, stack_name).await;
    let keys = xml_texts(&body, "OutputKey");
    let values = xml_texts(&body, "OutputValue");
    assert_eq!(
        keys.len(),
        values.len(),
        "case {case:?}: every reported output must carry an OutputValue, got keys {keys:?} and \
         values {values:?}"
    );
    keys.into_iter().zip(values).collect()
}

/// The value one output of a settled stack carries, or `None` where the stack reports no
/// such output — or reports it without a value.
async fn output_of(
    case: &str,
    router: &MultiServiceRouter,
    stack_name: &str,
    key: &str,
) -> Option<String> {
    outputs_of(case, router, stack_name)
        .await
        .into_iter()
        .find(|(reported, _)| reported == key)
        .map(|(_, value)| value)
}

/// The `StackStatus` and `StackStatusReason` a settled stack reports.
async fn status_of(case: &str, router: &MultiServiceRouter, stack_name: &str) -> (String, String) {
    let body = settled_stack(router, case, stack_name).await;
    (
        xml_text(&body, "StackStatus").unwrap_or_default(),
        xml_text(&body, "StackStatusReason").unwrap_or_default(),
    )
}

/// How long a case waits for a change set's creation to settle, and how often it looks: far
/// longer than the span this service gives that transition, so a loaded host cannot fail a
/// row — what is asserted is the STATUS appearing, never the time it took.
const PLAN_DEADLINE: std::time::Duration = std::time::Duration::from_secs(60);
/// See [`PLAN_DEADLINE`].
const PLAN_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(100);

/// Poll `DescribeChangeSet` until the plan's creation has settled, and answer the body that
/// reported it.
async fn settled_change_set(
    router: &MultiServiceRouter,
    case: &str,
    stack_name: &str,
    change_set_name: &str,
) -> String {
    let deadline = std::time::Instant::now() + PLAN_DEADLINE;
    let mut seen: Vec<String> = Vec::new();
    loop {
        let (status_code, body) = call(
            router,
            "DescribeChangeSet",
            &[
                member("StackName", stack_name),
                member("ChangeSetName", change_set_name),
            ],
        )
        .await;
        assert_eq!(
            status_code, 200,
            "case {case:?}: expected the plan to be describable, got {status_code} {body}"
        );
        let reported = xml_text(&body, "Status").unwrap_or_default();
        if reported != change_set_status::CREATE_PENDING
            && reported != change_set_status::CREATE_IN_PROGRESS
        {
            return body;
        }
        if seen.last().map(String::as_str) != Some(reported.as_str()) {
            seen.push(reported);
        }
        assert!(
            std::time::Instant::now() < deadline,
            "case {case:?}: expected the plan {change_set_name} to settle, saw {seen:?}"
        );
        tokio::time::sleep(PLAN_POLL_INTERVAL).await;
    }
}

/// One row of the resolution table: a template reading the producer's output, the output it
/// publishes and the value that output must carry.
struct ReadCase {
    case: &'static str,
    stack_name: &'static str,
    template: String,
    parameters: Vec<(String, String)>,
    expected: String,
}

/// The rows, built against the producer's identity (`stack_id`).
///
/// They differ along what the rule generalises over: the SPELLING of the function's two
/// arguments (positional and named), the stack addressed by name and by stack id, an output
/// that is exported and one that is not, the reference standing alone and nested inside
/// another function, its arguments written out and supplied by a parameter, and both template
/// spellings.
fn read_cases(stack_id: &str) -> Vec<ReadCase> {
    vec![
        ReadCase {
            case: "the positional pair, by stack name, reading an unexported output",
            stack_name: "submitted-positional",
            template: format!(
                r#"{{"Resources":{{"H":{{"Type":"AWS::CloudFormation::WaitConditionHandle"}}}},
                   "Outputs":{{"Read":{{"Value":{{"Fn::GetStackOutput":
                     ["{PRODUCER}","Plain"]}}}}}}}}"#
            ),
            parameters: Vec::new(),
            expected: PLAIN_VALUE.to_string(),
        },
        ReadCase {
            case: "the positional pair, by stack id",
            stack_name: "submitted-positional-by-id",
            template: format!(
                r#"{{"Resources":{{"H":{{"Type":"AWS::CloudFormation::WaitConditionHandle"}}}},
                   "Outputs":{{"Read":{{"Value":{{"Fn::GetStackOutput":
                     ["{stack_id}","Plain"]}}}}}}}}"#
            ),
            parameters: Vec::new(),
            expected: PLAIN_VALUE.to_string(),
        },
        ReadCase {
            case: "the positional pair reading an EXPORTED output",
            stack_name: "submitted-positional-exported",
            template: format!(
                r#"{{"Resources":{{"H":{{"Type":"AWS::CloudFormation::WaitConditionHandle"}}}},
                   "Outputs":{{"Read":{{"Value":{{"Fn::GetStackOutput":
                     ["{PRODUCER}","Shared"]}}}}}}}}"#
            ),
            parameters: Vec::new(),
            expected: EXPORTED_VALUE.to_string(),
        },
        ReadCase {
            case: "the positional pair nested inside another function",
            stack_name: "submitted-positional-nested",
            template: format!(
                r#"{{"Resources":{{"H":{{"Type":"AWS::CloudFormation::WaitConditionHandle"}}}},
                   "Outputs":{{"Read":{{"Value":{{"Fn::Join":["/",
                     [{{"Fn::GetStackOutput":["{PRODUCER}","Plain"]}},"tail"]]}}}}}}}}"#
            ),
            parameters: Vec::new(),
            expected: format!("{PLAIN_VALUE}/tail"),
        },
        ReadCase {
            case: "the positional pair with both arguments supplied by parameters",
            stack_name: "submitted-positional-parameters",
            template: r#"{"Parameters":{"Which":{"Type":"String"},"Key":{"Type":"String"}},
                 "Resources":{"H":{"Type":"AWS::CloudFormation::WaitConditionHandle"}},
                 "Outputs":{"Read":{"Value":{"Fn::GetStackOutput":
                   [{"Ref":"Which"},{"Fn::Sub":"${Key}"}]}}}}"#
                .to_string(),
            parameters: vec![
                member("Parameters.member.1.ParameterKey", "Which"),
                member("Parameters.member.1.ParameterValue", PRODUCER),
                member("Parameters.member.2.ParameterKey", "Key"),
                member("Parameters.member.2.ParameterValue", "Shared"),
            ],
            expected: EXPORTED_VALUE.to_string(),
        },
        ReadCase {
            case: "the positional pair in the yaml spelling of the body",
            stack_name: "submitted-positional-yaml",
            template: format!(
                "Resources:\n  H:\n    Type: AWS::CloudFormation::WaitConditionHandle\n\
                 Outputs:\n  Read:\n    Value:\n      Fn::GetStackOutput:\n        \
                 - {PRODUCER}\n        - Plain\n"
            ),
            parameters: Vec::new(),
            expected: PLAIN_VALUE.to_string(),
        },
        ReadCase {
            case: "the named members, the other spelling of the same two arguments",
            stack_name: "submitted-named",
            template: format!(
                r#"{{"Resources":{{"H":{{"Type":"AWS::CloudFormation::WaitConditionHandle"}}}},
                   "Outputs":{{"Read":{{"Value":{{"Fn::GetStackOutput":
                     {{"StackName":"{PRODUCER}","OutputName":"Plain"}}}}}}}}}}"#
            ),
            parameters: Vec::new(),
            expected: PLAIN_VALUE.to_string(),
        },
    ]
}

/// The `OutputKey` every row of the table publishes its read under.
const READ: &str = "Read";

#[tokio::test]
async fn a_read_of_another_stacks_output_resolves_in_either_spelling_of_its_arguments() {
    let router = router();
    let stack_id = deploy_producer(&router).await;

    let cases = read_cases(&stack_id);
    for case in &cases {
        let (status_code, body) =
            create_stack(&router, case.stack_name, &case.template, &case.parameters).await;
        assert_eq!(
            status_code, 200,
            "case {:?}: expected CreateStack to be accepted, got {status_code} {body}",
            case.case
        );
    }

    for case in &cases {
        let (stack_status, reason) = status_of(case.case, &router, case.stack_name).await;
        assert_eq!(
            stack_status, status::CREATE_COMPLETE,
            "case {:?}: expected the read to deploy, got {stack_status:?} ({reason:?})",
            case.case
        );
        let reported = output_of(case.case, &router, case.stack_name, READ).await;
        assert_eq!(
            reported.as_deref(),
            Some(case.expected.as_str()),
            "case {:?}: expected the output {READ:?} to carry {:?}, got {reported:?}",
            case.case,
            case.expected
        );
    }
}

/// One spelling of the function's two arguments, as a body submitted to every operation that
/// takes one: `slug` keeps the records one row writes apart from the next row's, and is the
/// value of a second output, so each row's body is a REVISION of the stack it is submitted
/// against rather than the body already in force.
struct Spelling {
    case: &'static str,
    slug: &'static str,
    template: String,
}

/// The two spellings, each reading the producer's unexported output.
fn submitted_spellings() -> Vec<Spelling> {
    [
        ("the positional pair", "positional", format!(r#"["{PRODUCER}","Plain"]"#)),
        (
            "the named members",
            "named",
            format!(r#"{{"StackName":"{PRODUCER}","OutputName":"Plain"}}"#),
        ),
    ]
    .into_iter()
    .map(|(case, slug, argument)| Spelling {
        case,
        slug,
        template: format!(
            r#"{{"Resources":{{"H":{{"Type":"AWS::CloudFormation::WaitConditionHandle"}}}},
               "Outputs":{{"Read":{{"Value":{{"Fn::GetStackOutput":{argument}}}}},
                 "Marker":{{"Value":"{slug}"}}}}}}"#
        ),
    })
    .collect()
}

#[tokio::test]
async fn no_operation_taking_a_body_refuses_a_read_as_an_unsupported_function() {
    let router = router();
    deploy_producer(&router).await;
    // The stack the revision-submitting operations are held against, deployed from a body
    // that resolves.
    let (status_code, body) = create_stack(&router, "submitted-read-holder", DEPLOYABLE, &[]).await;
    assert_eq!(
        status_code, 200,
        "expected the holder stack to be created, got {status_code} {body}"
    );
    settled_stack(&router, "the holder", "submitted-read-holder").await;

    for spelling in submitted_spellings() {
        let case = spelling.case;
        let template = spelling.template.as_str();
        // One submission per operation that takes a template body: the three that submit one
        // for deployment, the one whose whole answer IS a verdict on it, and the one that
        // summarises it.
        for (action, members) in [
            (
                "CreateStack",
                vec![
                    member("StackName", &format!("accepted-create-{}", spelling.slug)),
                    member("TemplateBody", template),
                ],
            ),
            (
                "UpdateStack",
                vec![
                    member("StackName", "submitted-read-holder"),
                    member("TemplateBody", template),
                ],
            ),
            (
                "CreateChangeSet",
                vec![
                    member("StackName", &format!("planned-read-{}", spelling.slug)),
                    member("ChangeSetName", "read-plan"),
                    member("ChangeSetType", "CREATE"),
                    member("TemplateBody", template),
                ],
            ),
            ("ValidateTemplate", vec![member("TemplateBody", template)]),
            ("GetTemplateSummary", vec![member("TemplateBody", template)]),
        ] {
            let (status_code, body) = call(&router, action, &members).await;
            let message = xml_text(&body, "Message").unwrap_or_default();
            assert!(
                !message.contains("unsupported function"),
                "case {case:?} through {action}: a function the service advertises as \
                 supported must not be refused as unsupported, got {message:?}"
            );
            assert_eq!(
                status_code, 200,
                "case {case:?} through {action}: expected the body accepted, got \
                 {status_code} {body}"
            );
            // The holder is left settled before the next operation reads or revises it.
            if action == "UpdateStack" {
                settled_stack(&router, case, "submitted-read-holder").await;
            }
        }

        // The accepted bodies were really DEPLOYED, so the acceptance is more than a 200:
        // both the stack this row created and the holder it revised report the value the
        // read resolved to.
        for stack_name in [
            format!("accepted-create-{}", spelling.slug),
            "submitted-read-holder".to_string(),
        ] {
            let reported = output_of(case, &router, &stack_name, READ).await;
            assert_eq!(
                reported.as_deref(),
                Some(PLAIN_VALUE),
                "case {case:?}: expected {stack_name} to report the value it read, got \
                 {reported:?}"
            );
        }
    }
}

#[tokio::test]
async fn an_executed_change_set_resolves_the_read_it_planned() {
    let router = router();
    deploy_producer(&router).await;

    let case = "a CREATE change set whose body reads another stack's output";
    let template = format!(
        r#"{{"Resources":{{"H":{{"Type":"AWS::CloudFormation::WaitConditionHandle"}}}},
           "Outputs":{{"Read":{{"Value":{{"Fn::GetStackOutput":["{PRODUCER}","Plain"]}}}}}}}}"#
    );
    let (status_code, body) = call(
        &router,
        "CreateChangeSet",
        &[
            member("StackName", "executed-read"),
            member("ChangeSetName", "executed-read-plan"),
            member("ChangeSetType", "CREATE"),
            member("TemplateBody", &template),
        ],
    )
    .await;
    assert_eq!(
        status_code, 200,
        "case {case:?}: expected CreateChangeSet to be accepted, got {status_code} {body}"
    );

    // A plan is executable once its own creation has settled, so the case polls for THAT
    // status rather than waiting a fixed time — and asserts the plan was computed rather
    // than failed.
    let plan = settled_change_set(&router, case, "executed-read", "executed-read-plan").await;    assert_eq!(
        xml_text(&plan, "Status").as_deref(),
        Some(change_set_status::CREATE_COMPLETE),
        "case {case:?}: expected the plan to be computed, got {plan}"
    );
    let (status_code, executed) = call(
        &router,
        "ExecuteChangeSet",
        &[
            member("StackName", "executed-read"),
            member("ChangeSetName", "executed-read-plan"),
        ],
    )
    .await;
    assert_eq!(
        status_code, 200,
        "case {case:?}: expected the plan to be executable, got {status_code} {executed}"
    );
    let (stack_status, reason) = status_of(case, &router, "executed-read").await;
    assert_eq!(
        stack_status,
        status::CREATE_COMPLETE,
        "case {case:?}: expected the executed plan to deploy, got {stack_status:?} ({reason:?})"
    );
    let reported = output_of(case, &router, "executed-read", READ).await;
    assert_eq!(
        reported.as_deref(),
        Some(PLAIN_VALUE),
        "case {case:?}: expected the deployed stack to report the value it read, got \
         {reported:?}"
    );
}

#[tokio::test]
async fn a_read_naming_no_output_fails_the_deployment_in_either_spelling() {
    let router = router();
    deploy_producer(&router).await;

    // One row per way a reference can name nothing, in both spellings of the arguments: an
    // output the named stack does not declare, and a stack this Region does not hold.
    let rows = [
        (
            "the positional pair naming an output the stack does not declare",
            "unresolved-positional-output",
            format!(r#"["{PRODUCER}","NoSuchOutput"]"#),
            Reference::new(PRODUCER, "NoSuchOutput"),
        ),
        (
            "the positional pair naming a stack this Region does not hold",
            "unresolved-positional-stack",
            r#"["no-such-producer","Plain"]"#.to_string(),
            Reference::new("no-such-producer", "Plain"),
        ),
        (
            "the named members naming an output the stack does not declare",
            "unresolved-named-output",
            format!(r#"{{"StackName":"{PRODUCER}","OutputName":"NoSuchOutput"}}"#),
            Reference::new(PRODUCER, "NoSuchOutput"),
        ),
    ];
    for (case, stack_name, argument, reference) in rows {
        let template = format!(
            r#"{{"Resources":{{"Topic":{{"Type":"AWS::SNS::Topic","Properties":{{
                 "DisplayName":{{"Fn::GetStackOutput":{argument}}}}}}}}}}}"#
        );
        let (status_code, body) = create_stack(&router, stack_name, &template, &[]).await;
        assert_eq!(
            status_code, 200,
            "case {case:?}: expected the CreateStack call itself to be accepted, got \
             {status_code} {body}"
        );
        let (stack_status, reason) = status_of(case, &router, stack_name).await;
        assert!(
            stack_status.starts_with("ROLLBACK") || stack_status == status::CREATE_FAILED,
            "case {case:?}: expected the create to fail over the unresolvable read, got \
             {stack_status:?} ({reason:?})"
        );
        assert!(
            reason.contains(&no_output_named(&reference)),
            "case {case:?}: expected the reason to name the output that could not be read \
             ({:?}), got {reason:?}",
            no_output_named(&reference)
        );
    }
}
