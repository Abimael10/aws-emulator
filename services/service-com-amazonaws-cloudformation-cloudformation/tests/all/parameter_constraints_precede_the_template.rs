// SPDX-License-Identifier: CC0-1.0

//! WHEN a stack operation weighs a parameter VALUE against the constraints its own
//! declaration states, end to end over the awsQuery wire.
//!
//! RULE: every value in force for a declared parameter is held to that declaration's own
//! constraints BEFORE the template that uses the value is processed, so a value outside them
//! refuses the call with HTTP 400 `ValidationError` naming the PARAMETER — whatever the
//! template would later have done with the value — and the template's own processing decides
//! the call only once every value satisfies its declaration.
//!
//! The rows below are read through one template whose `Outputs` resolve an `Fn::FindInMap`
//! keyed by the parameter, so the two passes are both reachable from one body and WHICH of
//! them answers is what each row pins: a value its declaration refuses is answered by the
//! parameter pass even where the lookup would have succeeded, and a value its declaration
//! admits is answered by the lookup — the refusal there names the mapping, since nothing is
//! wrong with the parameter.
//!
//! The table spans what the rule generalises over: WHICH constraint the value fails
//! (`AllowedValues`, `AllowedPattern`, `MinLength`/`MaxLength`, `MinValue`/`MaxValue`, the
//! declared `Type` itself, and a declaration that words its own refusal), whether the
//! template's own lookup of that value would have resolved, where the value in force comes
//! from (the request, or the declaration's own `Default`), and which operation submits it —
//! `CreateStack`, `CreateChangeSet` and `UpdateStack`, which hold a value to the same
//! contract. `stack_input_contracts.rs` owns the constraint catalogue itself; this suite owns
//! the ORDER the two passes run in.

use service_com_amazonaws_cloudformation_cloudformation::state::status;

use crate::harness::{
    call, create_stack, member, refusal, router, settled_stack_status, xml_text, REGION,
};
use crate::stack_outputs::{output_for, wire_outputs};

/// The parameter every row declares.
const PARAMETER: &str = "Stage";

/// The map every row's template looks the parameter's value up in, and the entries it
/// carries: `dev` and `prod`, each with one `Tier`.
const MAPPINGS: &str =
    r#""Mappings":{"StageConfig":{"dev":{"Tier":"small"},"prod":{"Tier":"large"}}}"#;

/// The resource every row's template declares: a type no owning service has to be asked
/// about, so a case about the REQUEST does not depend on a deployment.
const RESOURCE: &str =
    r#""Resources":{"Handle":{"Type":"AWS::CloudFormation::WaitConditionHandle"}}"#;

/// The output whose value the parameter decides, and the name it is reported under.
const OUTPUT: &str = "Tier";

/// A template declaring one parameter under `declaration`, a `Mappings` section carrying
/// two entries, and an output that looks the parameter's value up in it.
fn template(declaration: &str) -> String {
    format!(
        r#"{{"Parameters":{{"{PARAMETER}":{declaration}}},{MAPPINGS},{RESOURCE},
            "Outputs":{{"{OUTPUT}":{{"Value":{{"Fn::FindInMap":
                ["StageConfig",{{"Ref":"{PARAMETER}"}},"Tier"]}}}}}}}}"#
    )
}

/// The members one value for that parameter is passed as.
fn passing(value: &str) -> Vec<(String, String)> {
    vec![
        member("Parameters.member.1.ParameterKey", PARAMETER),
        member("Parameters.member.1.ParameterValue", value),
    ]
}

/// What a declaration-and-value pair is answered with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Answer {
    /// Refused `ValidationError` with this message, and nothing written.
    Refused(&'static str),
    /// Admitted: the operation goes through, and the lookup the value decides resolves to
    /// this `Tier`.
    Admitted(&'static str),
}

/// One row: how the parameter is declared, the value the request states for it, and what the
/// pair is answered with.
struct Row {
    case: &'static str,
    declaration: &'static str,
    value: &'static str,
    answer: Answer,
}

/// The class the rule covers. Every clause quoted here is the one this service words that
/// constraint's refusal with (`crate::state::parameter_constraints`), and the mapping clause
/// is the one its template pass words a missed lookup with.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a value outside AllowedValues that the Mappings section also lacks",
            declaration: r#"{"Type":"String","AllowedValues":["dev","prod"]}"#,
            value: "delta",
            answer: Answer::Refused("Parameter 'Stage' must be one of AllowedValues"),
        },
        Row {
            // The lookup WOULD have resolved, so this row is the ordering itself: the
            // parameter pass answers first.
            case: "a value outside AllowedValues that the Mappings section does carry",
            declaration: r#"{"Type":"String","AllowedValues":["prod"]}"#,
            value: "dev",
            answer: Answer::Refused("Parameter 'Stage' must be one of AllowedValues"),
        },
        Row {
            case: "a value the AllowedPattern does not match",
            declaration: r#"{"Type":"String","AllowedPattern":"dev|prod"}"#,
            value: "delta",
            answer: Answer::Refused("Parameter 'Stage' must match pattern dev|prod"),
        },
        Row {
            case: "a value longer than MaxLength",
            declaration: r#"{"Type":"String","MaxLength":4}"#,
            value: "delta",
            answer: Answer::Refused("Parameter 'Stage' must have length not greater than 4"),
        },
        Row {
            case: "a value shorter than MinLength, which the Mappings section does carry",
            declaration: r#"{"Type":"String","MinLength":4}"#,
            value: "dev",
            answer: Answer::Refused("Parameter 'Stage' must have length not less than 4"),
        },
        Row {
            case: "a number above MaxValue",
            declaration: r#"{"Type":"Number","MaxValue":3}"#,
            value: "9",
            answer: Answer::Refused("Parameter 'Stage' must be a number not greater than 3"),
        },
        Row {
            case: "a number below MinValue",
            declaration: r#"{"Type":"Number","MinValue":2}"#,
            value: "1",
            answer: Answer::Refused("Parameter 'Stage' must be a number not less than 2"),
        },
        Row {
            case: "a Number parameter passed something that is no number",
            declaration: r#"{"Type":"Number"}"#,
            value: "delta",
            answer: Answer::Refused("Parameter 'Stage' must be a number."),
        },
        Row {
            case: "a declaration that words its own constraint",
            declaration: r#"{"Type":"String","AllowedValues":["dev","prod"],
                "ConstraintDescription":"Stage must be dev or prod."}"#,
            value: "delta",
            answer: Answer::Refused(
                "Parameter Stage failed to satisfy constraint: Stage must be dev or prod.",
            ),
        },
        Row {
            // The parameter pass admits the value, so the template's own processing is what
            // answers — and it names the mapping, not the parameter.
            case: "a value AllowedValues admits that the Mappings section does not carry",
            declaration: r#"{"Type":"String","AllowedValues":["dev","prod","staging"]}"#,
            value: "staging",
            answer: Answer::Refused(
                "Template error: Unable to get mapping for StageConfig::staging::Tier",
            ),
        },
        Row {
            case: "a value AllowedValues admits that the Mappings section carries",
            declaration: r#"{"Type":"String","AllowedValues":["dev","prod"]}"#,
            value: "prod",
            answer: Answer::Admitted("large"),
        },
        Row {
            case: "a value inside every constraint its declaration states",
            declaration: r#"{"Type":"String","MinLength":3,"MaxLength":4,
                "AllowedPattern":"[a-z]+","AllowedValues":["dev","prod"]}"#,
            value: "dev",
            answer: Answer::Admitted("small"),
        },
    ]
}

/// Assert `body` is the `ValidationError` refusal `message`.
fn assert_refused(case: &str, code: u16, body: &str, message: &str) {
    assert_eq!(
        code, 400,
        "case {case:?}: the call must be refused, got {code} {body}"
    );
    let answered = refusal(body);
    let expected = ("ValidationError".to_string(), message.to_string());
    assert_eq!(
        answered, expected,
        "case {case:?}: expected {expected:?}, got {answered:?}"
    );
}

/// The `Tier` output the stack reports once its operation has settled.
async fn settled_tier(
    case: &str,
    router: &smithy_server_runtime::MultiServiceRouter,
    stack: &str,
) -> Option<String> {
    let (code, body) = call(router, "DescribeStacks", &[member("StackName", stack)]).await;
    assert_eq!(
        code, 200,
        "case {case:?}: the stack must be readable, got {code} {body}"
    );
    output_for(&wire_outputs(&body), OUTPUT).and_then(|output| output.value.clone())
}

#[tokio::test]
async fn a_create_weighs_every_parameter_value_before_it_processes_the_template() {
    let router = router();
    for (index, row) in rows().into_iter().enumerate() {
        let case = row.case;
        let name = format!("pcpt-create-{index}");
        let (code, body) = create_stack(
            &router,
            &name,
            &template(row.declaration),
            &passing(row.value),
        )
        .await;
        match row.answer {
            Answer::Refused(message) => {
                assert_refused(case, code, &body, message);
                // The refusal is of the REQUEST: no stack is left behind under that name.
                let (read, read_body) =
                    call(&router, "DescribeStacks", &[member("StackName", &name)]).await;
                assert_eq!(
                    read, 400,
                    "case {case:?}: a refused create leaves no stack, got {read} {read_body}"
                );
            }
            Answer::Admitted(tier) => {
                assert_eq!(
                    code, 200,
                    "case {case:?}: the declaration admits the value, got {code} {body}"
                );
                let settled = settled_stack_status(case, &router, REGION, &name).await;
                assert_eq!(
                    settled,
                    status::CREATE_COMPLETE,
                    "case {case:?}: expected the create to complete, got {settled:?}"
                );
                // ...and the lookup the admitted value keys is what the output resolves to,
                // so the template really was processed with that value.
                let reported = settled_tier(case, &router, &name).await;
                assert_eq!(
                    reported.as_deref(),
                    Some(tier),
                    "case {case:?}: expected the output {tier:?}, got {reported:?}"
                );
            }
        }
    }
}

#[tokio::test]
async fn a_change_set_weighs_the_same_values_on_the_same_contract() {
    let router = router();
    for (index, row) in rows().into_iter().enumerate() {
        let case = row.case;
        let stack = format!("pcpt-plan-{index}");
        let plan = format!("plan-{index}");
        let mut members = vec![
            member("StackName", &stack),
            member("ChangeSetName", &plan),
            member("ChangeSetType", "CREATE"),
            member("TemplateBody", &template(row.declaration)),
        ];
        members.extend(passing(row.value));
        let (code, body) = call(&router, "CreateChangeSet", &members).await;
        match row.answer {
            Answer::Refused(message) => {
                assert_refused(case, code, &body, message);
                // Nothing was written: the refused plan brought no stack into existence.
                let (read, read_body) =
                    call(&router, "DescribeStacks", &[member("StackName", &stack)]).await;
                assert_eq!(
                    read, 400,
                    "case {case:?}: a refused plan leaves no stack, got {read} {read_body}"
                );
            }
            Answer::Admitted(_) => {
                assert_eq!(
                    code, 200,
                    "case {case:?}: the declaration admits the value, got {code} {body}"
                );
                let (read, read_body) =
                    call(&router, "ListChangeSets", &[member("StackName", &stack)]).await;
                assert_eq!(
                    read, 200,
                    "case {case:?}: the stack's plans must be readable, got {read} {read_body}"
                );
                assert!(
                    read_body.contains(&plan),
                    "case {case:?}: expected the plan {plan:?} to be recorded, got {read_body}"
                );
            }
        }
    }
}

#[tokio::test]
async fn an_update_weighs_the_same_values_on_the_same_contract() {
    let router = router();
    let stack = "pcpt-update";
    // The stack the updates are submitted against, deployed from a value its declaration
    // admits so that every refusal below is the UPDATE's own.
    let (code, body) = create_stack(
        &router,
        stack,
        &template(r#"{"Type":"String","Default":"dev"}"#),
        &passing("dev"),
    )
    .await;
    assert_eq!(
        code, 200,
        "the stack the updates are applied to must exist first, got {code} {body}"
    );
    let settled = settled_stack_status("the stack under test", &router, REGION, stack).await;
    assert_eq!(
        settled,
        status::CREATE_COMPLETE,
        "the stack under test must settle created, got {settled:?}"
    );

    for row in rows() {
        let case = row.case;
        let Answer::Refused(message) = row.answer else {
            // An admitted value is exercised once, after the loop: an update that changes
            // nothing is a different question (`change_set_lifecycle.rs` owns it).
            continue;
        };
        let mut members = vec![
            member("StackName", stack),
            member("TemplateBody", &template(row.declaration)),
        ];
        members.extend(passing(row.value));
        let (code, body) = call(&router, "UpdateStack", &members).await;
        assert_refused(case, code, &body, message);
        // A refused update leaves the stack with the value and the status it had.
        let (read, read_body) =
            call(&router, "DescribeStacks", &[member("StackName", stack)]).await;
        assert_eq!(
            read, 200,
            "case {case:?}: the stack must still be readable, got {read} {read_body}"
        );
        let held = (
            xml_text(&read_body, "ParameterValue"),
            xml_text(&read_body, "StackStatus"),
        );
        let expected = (
            Some("dev".to_string()),
            Some(status::CREATE_COMPLETE.to_string()),
        );
        assert_eq!(
            held, expected,
            "case {case:?}: expected the stack untouched {expected:?}, got {held:?}"
        );
    }

    // ...and an update whose value every constraint admits goes through, with the output the
    // new value keys: the pass refuses values, it does not refuse the operation.
    let case = "an update to a value AllowedValues admits";
    let mut members = vec![
        member("StackName", stack),
        member(
            "TemplateBody",
            &template(r#"{"Type":"String","AllowedValues":["dev","prod"]}"#),
        ),
    ];
    members.extend(passing("prod"));
    let (code, body) = call(&router, "UpdateStack", &members).await;
    assert_eq!(
        code, 200,
        "case {case:?}: the declaration admits the value, got {code} {body}"
    );
    let settled = settled_stack_status(case, &router, REGION, stack).await;
    assert_eq!(
        settled,
        status::UPDATE_COMPLETE,
        "case {case:?}: expected the update to complete, got {settled:?}"
    );
    let reported = settled_tier(case, &router, stack).await;
    assert_eq!(
        reported.as_deref(),
        Some("large"),
        "case {case:?}: expected the output \"large\", got {reported:?}"
    );
}

/// One row of the default table: the declaration whose own `Default` is the value in force,
/// and what submitting it with no values at all is answered with.
struct DefaultRow {
    case: &'static str,
    declaration: &'static str,
    answer: Answer,
}

#[tokio::test]
async fn a_declarations_own_default_is_weighed_before_the_template_too() {
    let router = router();
    let rows: Vec<DefaultRow> = vec![
        DefaultRow {
            case: "a Default outside the declaration's own AllowedValues",
            declaration: r#"{"Type":"String","Default":"delta",
                "AllowedValues":["dev","prod"]}"#,
            answer: Answer::Refused("Parameter 'Stage' must be one of AllowedValues"),
        },
        DefaultRow {
            case: "a Default the declaration's own AllowedPattern does not match",
            declaration: r#"{"Type":"String","Default":"Delta","AllowedPattern":"[a-z]+"}"#,
            answer: Answer::Refused("Parameter 'Stage' must match pattern [a-z]+"),
        },
        DefaultRow {
            case: "a Default its own AllowedValues admits that the Mappings section lacks",
            declaration: r#"{"Type":"String","Default":"staging",
                "AllowedValues":["dev","staging"]}"#,
            answer: Answer::Refused(
                "Template error: Unable to get mapping for StageConfig::staging::Tier",
            ),
        },
        DefaultRow {
            case: "a Default every constraint its declaration states admits",
            declaration: r#"{"Type":"String","Default":"prod","AllowedValues":["dev","prod"]}"#,
            answer: Answer::Admitted("large"),
        },
    ];

    for (index, row) in rows.into_iter().enumerate() {
        let case = row.case;
        let name = format!("pcpt-default-{index}");
        // No `Parameters` member at all: the declaration's own `Default` is the value in
        // force, and is held to the same constraints a supplied value is.
        let (code, body) = create_stack(&router, &name, &template(row.declaration), &[]).await;
        match row.answer {
            Answer::Refused(message) => {
                assert_refused(case, code, &body, message);
                let (read, read_body) =
                    call(&router, "DescribeStacks", &[member("StackName", &name)]).await;
                assert_eq!(
                    read, 400,
                    "case {case:?}: a refused create leaves no stack, got {read} {read_body}"
                );
            }
            Answer::Admitted(tier) => {
                assert_eq!(
                    code, 200,
                    "case {case:?}: the declaration admits its own default, got {code} {body}"
                );
                let settled = settled_stack_status(case, &router, REGION, &name).await;
                assert_eq!(
                    settled,
                    status::CREATE_COMPLETE,
                    "case {case:?}: expected the create to complete, got {settled:?}"
                );
                let reported = settled_tier(case, &router, &name).await;
                assert_eq!(
                    reported.as_deref(),
                    Some(tier),
                    "case {case:?}: expected the output {tier:?}, got {reported:?}"
                );
            }
        }
    }
}
