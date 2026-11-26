// SPDX-License-Identifier: CC0-1.0

//! Deploying a stack whose template `Transform` names a REGISTERED macro, end to end over the
//! wire.
//!
//! RULE: "the optional `Transform` section … specifies one or more macros that CloudFormation
//! uses to process your template", and CloudFormation processes them BEFORE it does anything
//! with the template's resources — so an operation that DEPLOYS such a template invokes the
//! function each macro's `AWS::CloudFormation::Macro` resource names, in the order the
//! template declares them, and deploys the fragment the macro answered: a resource the macro
//! INJECTED is created in the service that owns it exactly as one the body declares itself is,
//! and a property the macro REWROTE reaches that service rewritten. A macro that does not
//! succeed is the stack's own template-processing failure: nothing is deployed, the stack
//! reports the transform's own sentence on its own failed event, and the create rolls back.
//!
//! The rows differ along what the rule generalises over: what the macro DOES to the fragment
//! (injects a resource, rewrites a declared property, reads the `params` its own `Transform`
//! declaration carried) and how it ENDS (answering `success`, answering a failure). Every
//! completed row is checked against the SERVICE THAT OWNS the resource — the parameter the
//! macro's output declares is read back out of AWS Systems Manager — so a row passes only
//! where the macro's output really deployed.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_cloudformation_cloudformation::state::{resource_status, status};

use crate::dynamic_references::ssm;
use crate::harness::{
    call, create_stack, member, router, settled_stack, stack_own_events, xml_text, xml_texts,
    REGION,
};
use crate::private_macro_change_sets::{handler, register_macro};

/// The logical id of the resource the consumer template declares ITSELF.
const CARRIER: &str = "Carrier";

/// The logical id of the resource a macro INJECTS into the fragment.
const INJECTED: &str = "AddedByTheMacro";

/// The value a macro writes, which no template of this suite declares: a parameter holding it
/// can only have been deployed from the macro's own output.
const MADE_BY_THE_MACRO: &str = "made-by-the-macro";

/// The value the consumer template declares for its own resource, which a rewriting macro
/// replaces.
const AS_DECLARED: &str = "as-declared";

/// The parameter the consumer template's own resource is deployed as.
fn carrier_parameter(stack: &str) -> String {
    format!("/{stack}/carrier")
}

/// The parameter an INJECTED resource is deployed as.
fn injected_parameter(stack: &str) -> String {
    format!("/{stack}/injected")
}

/// The value AWS Systems Manager holds for `name`, or `None` where it holds no such parameter
/// — which is how a row checks the macro's output against the service that OWNS the resource.
async fn parameter_value(router: &MultiServiceRouter, name: &str) -> Option<String> {
    let (code, body) = ssm(
        router,
        "GetParameter",
        &json!({"Name": name, "WithDecryption": true}),
    )
    .await;
    if code != 200 {
        return None;
    }
    body.pointer("/Parameter/Value")
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// A macro that INJECTS a parameter resource of its own into the fragment.
fn injects_a_resource(stack: &str) -> String {
    handler(&format!(
        r#"    fragment["Resources"]["{INJECTED}"] = {{
        "Type": "AWS::SSM::Parameter",
        "Properties": {{
            "Name": "{}",
            "Type": "String",
            "Value": "{MADE_BY_THE_MACRO}",
        }},
    }}"#,
        injected_parameter(stack)
    ))
}

/// A macro that REWRITES a property of the resource the consumer template declares.
fn rewrites_a_property(_stack: &str) -> String {
    handler(&format!(
        r#"    fragment["Resources"]["{CARRIER}"]["Properties"]["Value"] = "{MADE_BY_THE_MACRO}""#
    ))
}

/// A macro that writes the `params` its own `Transform` declaration carried.
fn reads_its_params(_stack: &str) -> String {
    handler(&format!(
        r#"    fragment["Resources"]["{CARRIER}"]["Properties"]["Value"] = params["Value"]"#
    ))
}

/// What the macro answers when it REFUSES the fragment.
const REFUSED_BY_THE_MACRO: &str = "the macro refused this fragment";

/// A macro that answers a FAILURE status rather than `success`.
fn answers_a_failure(_stack: &str) -> String {
    format!(
        r#"
def handler(event, context):
    return {{
        "requestId": event["requestId"],
        "status": "failure",
        "errorMessage": "{REFUSED_BY_THE_MACRO}",
    }}
"#
    )
}

/// The consumer template: the macro named as a bare string, and one resource of its own.
fn invoking(macro_name: &str, stack: &str) -> String {
    json!({
        "Transform": macro_name,
        "Resources": { CARRIER: parameter(&carrier_parameter(stack), AS_DECLARED) }
    })
    .to_string()
}

/// The same template, invoking the macro WITH the parameters its declaration carries.
fn invoking_with_params(macro_name: &str, stack: &str) -> String {
    json!({
        "Transform": { "Name": macro_name, "Parameters": { "Value": MADE_BY_THE_MACRO } },
        "Resources": { CARRIER: parameter(&carrier_parameter(stack), AS_DECLARED) }
    })
    .to_string()
}

/// One `AWS::SSM::Parameter` declaration.
fn parameter(name: &str, value: &str) -> Value {
    json!({
        "Type": "AWS::SSM::Parameter",
        "Properties": { "Name": name, "Type": "String", "Value": value }
    })
}

/// One parameter a row reads back out of AWS Systems Manager: the name, built from the
/// row's stack, and the value that service has to hold for it (`None` for a parameter nothing
/// may have created).
type Held = (fn(&str) -> String, Option<&'static str>);

/// One row: the macro's handler, the template that invokes it, and what deploying that
/// template has to leave behind.
struct Row {
    case: &'static str,
    /// The stack the row deploys, which also seeds every parameter name it uses.
    stack: &'static str,
    /// The macro's Python handler, built from the stack name.
    handler: fn(&str) -> String,
    /// The consumer template, built from the macro's name and the stack name.
    template: fn(&str, &str) -> String,
    /// The `StackStatus` the deployment has to settle on.
    status: &'static str,
    /// The logical ids the deployed stack has to report.
    deployed: &'static [&'static str],
    /// What AWS Systems Manager has to hold once the deployment settles.
    held: &'static [Held],
    /// What the stack's own failed event has to carry, for a macro that did not succeed.
    reason: Option<&'static str>,
}

const ROWS: &[Row] = &[
    Row {
        case: "a macro that injects a resource into the fragment",
        stack: "macro-deploy-injects",
        handler: injects_a_resource,
        template: invoking,
        status: status::CREATE_COMPLETE,
        deployed: &[CARRIER, INJECTED],
        held: &[
            (carrier_parameter, Some(AS_DECLARED)),
            (injected_parameter, Some(MADE_BY_THE_MACRO)),
        ],
        reason: None,
    },
    Row {
        case: "a macro that rewrites a property of the resource the template declares",
        stack: "macro-deploy-rewrites",
        handler: rewrites_a_property,
        template: invoking,
        status: status::CREATE_COMPLETE,
        deployed: &[CARRIER],
        held: &[
            (carrier_parameter, Some(MADE_BY_THE_MACRO)),
            (injected_parameter, None),
        ],
        reason: None,
    },
    Row {
        case: "a macro that reads the params its own Transform declaration carried",
        stack: "macro-deploy-params",
        handler: reads_its_params,
        template: invoking_with_params,
        status: status::CREATE_COMPLETE,
        deployed: &[CARRIER],
        held: &[(carrier_parameter, Some(MADE_BY_THE_MACRO))],
        reason: None,
    },
    Row {
        case: "a macro that answers a failure deploys nothing",
        stack: "macro-deploy-failure",
        handler: answers_a_failure,
        template: invoking,
        status: status::ROLLBACK_COMPLETE,
        deployed: &[],
        held: &[(carrier_parameter, None), (injected_parameter, None)],
        reason: Some(REFUSED_BY_THE_MACRO),
    },
];

#[tokio::test]
async fn a_stack_deploys_the_template_its_macro_answered() {
    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case;
        let router = router();
        let macro_name = format!("MacroUnderTest{index}");
        register_macro(
            &router,
            &format!("macro-deploy-registration-{index}"),
            &macro_name,
            &(row.handler)(row.stack),
        )
        .await;

        let template = (row.template)(&macro_name, row.stack);
        let (code, created) = create_stack(
            &router,
            row.stack,
            &template,
            &[member("Capabilities.member.1", "CAPABILITY_AUTO_EXPAND")],
        )
        .await;
        assert_eq!(
            code, 200,
            "case {case:?}: CreateStack must answer 200 — the deployment is what reports the \
             outcome — got {code} {created}"
        );

        let described = settled_stack(case, &router, REGION, row.stack).await;
        let reported = xml_text(&described, "StackStatus").unwrap_or_default();
        assert_eq!(
            reported, row.status,
            "case {case:?}: expected the stack to reach {}, got {reported:?} in {described}",
            row.status
        );

        // The resources the stack REPORTS are the ones the macro's output declared.
        let (code, resources) = call(
            &router,
            "DescribeStackResources",
            &[member("StackName", row.stack)],
        )
        .await;
        assert_eq!(
            code, 200,
            "case {case:?}: DescribeStackResources must answer 200, got {code} {resources}"
        );
        let logical_ids = xml_texts(&resources, "LogicalResourceId");
        for expected in row.deployed {
            assert!(
                logical_ids.contains(&(*expected).to_string()),
                "case {case:?}: expected the stack to report {expected:?} — the macro's own \
                 output — got {logical_ids:?}"
            );
        }
        if row.deployed.is_empty() {
            assert!(
                logical_ids.is_empty(),
                "case {case:?}: expected a stack whose macro failed to report no resource, got \
                 {logical_ids:?}"
            );
        }

        // ...and each of them really exists in the service that OWNS it, carrying the value
        // the macro's output declared.
        for (name_of, expected) in row.held {
            let name = name_of(row.stack);
            let held = parameter_value(&router, &name).await;
            assert_eq!(
                held.as_deref(),
                *expected,
                "case {case:?}: expected Systems Manager to hold {expected:?} for {name:?}, got \
                 {held:?}"
            );
        }

        // A macro that did not succeed is the stack's OWN failure: no resource was asked of
        // any owning service, so the transform's sentence is on the stack's own event.
        let own_events = stack_own_events(&router, case, row.stack).await;
        let failed: Vec<&str> = own_events
            .iter()
            .filter(|event| event.resource_status == resource_status::CREATE_FAILED)
            .map(|event| event.reason.as_str())
            .collect();
        match row.reason {
            None => assert!(
                failed.is_empty(),
                "case {case:?}: expected no failed event of the stack's own, got {failed:?}"
            ),
            Some(reason) => {
                let [recorded] = failed.as_slice() else {
                    panic!(
                        "case {case:?}: expected one failed event of the stack's own, got \
                         {own_events:?}"
                    );
                };
                assert!(
                    recorded.contains("failed with:") && recorded.contains(reason),
                    "case {case:?}: expected the transform's own failure naming {reason:?}, got \
                     {recorded:?}"
                );
            }
        }
    }
}

/// The same stage runs on an UPDATE: an update submits a template of its own, so the macros it
/// names are performed over it and the document the update deploys is the one they produced.
#[tokio::test]
async fn an_update_deploys_the_template_its_macro_answered() {
    let case = "an update whose template invokes a registered macro";
    let router = router();
    let stack = "macro-deploy-update";
    let macro_name = "MacroUnderUpdate";
    register_macro(
        &router,
        "macro-update-registration",
        macro_name,
        &injects_a_resource(stack),
    )
    .await;

    // A first deployment with no transform at all: the stack exists, and the macro has
    // injected nothing into it.
    let plain = json!({
        "Resources": { CARRIER: parameter(&carrier_parameter(stack), AS_DECLARED) }
    })
    .to_string();
    let (code, created) = create_stack(&router, stack, &plain, &[]).await;
    assert_eq!(
        code, 200,
        "case {case:?}: CreateStack must answer 200, got {code} {created}"
    );
    let reported = settled_stack(case, &router, REGION, stack).await;
    assert_eq!(
        xml_text(&reported, "StackStatus").unwrap_or_default(),
        status::CREATE_COMPLETE,
        "case {case:?}: expected the first deployment to complete, got {reported}"
    );
    let injected = parameter_value(&router, &injected_parameter(stack)).await;
    assert_eq!(
        injected, None,
        "case {case:?}: expected nothing to be injected before the macro is invoked, got \
         {injected:?}"
    );

    // ...and then the SAME template with the macro's `Transform`: the update deploys what the
    // macro answered, so the injected resource is created in the service that owns it.
    let (code, updated) = call(
        &router,
        "UpdateStack",
        &[
            member("StackName", stack),
            member("TemplateBody", &invoking(macro_name, stack)),
            member("Capabilities.member.1", "CAPABILITY_AUTO_EXPAND"),
        ],
    )
    .await;
    assert_eq!(
        code, 200,
        "case {case:?}: UpdateStack must answer 200, got {code} {updated}"
    );
    let reported = settled_stack(case, &router, REGION, stack).await;
    assert_eq!(
        xml_text(&reported, "StackStatus").unwrap_or_default(),
        status::UPDATE_COMPLETE,
        "case {case:?}: expected the update to complete, got {reported}"
    );
    let held = parameter_value(&router, &injected_parameter(stack)).await;
    assert_eq!(
        held.as_deref(),
        Some(MADE_BY_THE_MACRO),
        "case {case:?}: expected the update to deploy the resource the macro injected, got \
         {held:?}"
    );
}
