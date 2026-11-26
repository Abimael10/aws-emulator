// SPDX-License-Identifier: CC0-1.0

//! What a stack reports when CloudFormation cannot RESOLVE a `{{resolve:ssm…}}` dynamic
//! reference a template's resource property carries, end to end over the wire.
//!
//! RULE: a `{{resolve:ssm…}}` or `{{resolve:ssm-secure…}}` DYNAMIC REFERENCE in a resource
//! property is resolved by CLOUDFORMATION ITSELF, before the deployment asks any owning
//! service for anything. A reference naming a parameter the store holds NOTHING under
//! therefore refuses the whole deployment: nothing is created, the operation in flight reports
//! the diagnostic `Parameters: [` … `] cannot be found.` on the STACK's own event — the
//! brackets naming every reference it could not resolve as `<store prefix>:<reference key>`,
//! which is also the stack's `StackStatusReason` — and the create rolls back. Any OTHER misuse
//! of a reference is reported the same way, in the resolver's own words.
//!
//! The rows differ along what the rule generalises over: the store prefix the reference is
//! written with (`ssm`, `ssm-secure`), how many references of one deployment cannot be found
//! (one, two in one property, two in different resources), the resource type and property the
//! reference stands in, and whether the refusal is a missing parameter or another misuse —
//! with a resolvable reference as the control, which reaches `CREATE_COMPLETE` and reports no
//! failure at all.

use service_com_amazonaws_cloudformation_cloudformation::state::{
    resource_status, status, USER_INITIATED,
};

use crate::dynamic_references::put_parameter;
use crate::harness::{
    create_stack, router, settled_stack, stack_own_events, xml_text, StackOwnEvent, REGION,
};

/// The logical id of the resource whose property carries the reference.
const CARRIER: &str = "Carrier";

/// The logical id of a SECOND resource carrying a reference of its own.
const SECOND: &str = "Second";

/// How a row's reference is refused, and so what the stack's own events have to say.
enum Refused {
    /// Parameter Store holds no parameter of that name: the fixed sentence, naming every such
    /// reference as the template wrote it.
    NotFound(&'static [&'static str]),
    /// Another misuse of the reference, reported in the resolver's own words: the fragments
    /// the reason has to carry.
    Diagnosed(&'static [&'static str]),
    /// The reference resolved: the deployment is not refused at all.
    Resolved,
}

/// One row: a stack, the parameters written before it is deployed, the template it deploys and
/// what the deployment then has to report.
struct Row {
    case: &'static str,
    stack: &'static str,
    /// The parameters the row writes first, each as `(name suffix, type, value)`.
    held: &'static [(&'static str, &'static str, &'static str)],
    /// The template, with `{stack}` standing for the row's stack name.
    template: &'static str,
    refused: Refused,
}

/// A parameter name of this row's own, so no two rows can collide in the store.
fn parameter_name(stack: &str, suffix: &str) -> String {
    format!("/{stack}/{suffix}")
}

/// An `AWS::SSM::Parameter` whose `Value` is `property`.
fn parameter_carrier(logical_id: &str, name: &str, property: &str) -> String {
    format!(
        r#""{logical_id}": {{"Type": "AWS::SSM::Parameter",
            "Properties": {{"Name": "{name}", "Type": "String", "Value": "{property}"}}}}"#
    )
}

const ROWS: &[Row] = &[
    Row {
        case: "the recorded case: one ssm reference naming a parameter nobody created",
        stack: "dynref-event-missing-one",
        held: &[],
        template: r#"{"Resources": {"Carrier": {"Type": "AWS::SSM::Parameter",
            "Properties": {"Name": "/{stack}/copy", "Type": "String",
                "Value": "{{resolve:ssm:/{stack}/absent}}"}}}}"#,
        refused: Refused::NotFound(&["ssm:/{stack}/absent"]),
    },
    Row {
        case: "two missing references in ONE property are named in one sentence",
        stack: "dynref-event-missing-two",
        held: &[],
        template: r#"{"Resources": {"Carrier": {"Type": "AWS::SSM::Parameter",
            "Properties": {"Name": "/{stack}/copy", "Type": "String",
                "Value": "{{resolve:ssm:/{stack}/alpha}}-{{resolve:ssm:/{stack}/beta}}"}}}}"#,
        refused: Refused::NotFound(&["ssm:/{stack}/alpha", "ssm:/{stack}/beta"]),
    },
    Row {
        case: "a missing ssm-secure reference in a property AWS publishes it for",
        stack: "dynref-event-secure-missing",
        held: &[],
        template: r#"{"Resources": {"Carrier": {"Type": "AWS::RDS::DBInstance",
            "Properties": {"DBInstanceIdentifier": "{stack}-db", "Engine": "mysql",
                "DBInstanceClass": "db.t3.micro", "MasterUsername": "probeadmin",
                "MasterUserPassword": "{{resolve:ssm-secure:/{stack}/absent}}"}}}}"#,
        refused: Refused::NotFound(&["ssm-secure:/{stack}/absent"]),
    },
    Row {
        case: "a reference of a type the prefix does not resolve is reported in the \
               resolver's own words",
        stack: "dynref-event-misuse",
        held: &[("source", "SecureString", "a-real-secret")],
        template: r#"{"Resources": {"Carrier": {"Type": "AWS::SSM::Parameter",
            "Properties": {"Name": "/{stack}/copy", "Type": "String",
                "Value": "{{resolve:ssm:/{stack}/source}}"}}}}"#,
        refused: Refused::Diagnosed(&[
            "ssm prefix was used for secure parameter",
            "/{stack}/source",
        ]),
    },
    Row {
        case: "the control: a reference the store answers is deployed and nothing fails",
        stack: "dynref-event-resolved",
        held: &[("source", "String", "plainly-stored")],
        template: r#"{"Resources": {"Carrier": {"Type": "AWS::SSM::Parameter",
            "Properties": {"Name": "/{stack}/copy", "Type": "String",
                "Value": "{{resolve:ssm:/{stack}/source}}"}}}}"#,
        refused: Refused::Resolved,
    },
];

/// The reasons the stack's own PROGRESS events carry — the events of the operation in flight,
/// other than the one that reports the operation was started.
///
/// This is where a client polling a deployment reads a diagnostic CloudFormation reached about
/// the template it is deploying.
fn progress_reasons(own_events: &[StackOwnEvent]) -> Vec<&str> {
    own_events
        .iter()
        .filter(|event| {
            event.resource_status == resource_status::CREATE_IN_PROGRESS
                && event.reason != USER_INITIATED
        })
        .map(|event| event.reason.as_str())
        .collect()
}

/// The reasons the stack's own FAILED events carry.
fn failed_reasons(own_events: &[StackOwnEvent]) -> Vec<&str> {
    own_events
        .iter()
        .filter(|event| event.resource_status == resource_status::CREATE_FAILED)
        .map(|event| event.reason.as_str())
        .collect()
}

#[tokio::test]
async fn a_reference_the_store_cannot_answer_is_reported_on_the_stacks_own_event() {
    for row in ROWS {
        let case = row.case;
        let router = router();
        for (suffix, parameter_type, value) in row.held {
            put_parameter(
                &router,
                case,
                &parameter_name(row.stack, suffix),
                parameter_type,
                value,
            )
            .await;
        }
        let template = row.template.replace("{stack}", row.stack);
        let (code, created) = create_stack(&router, row.stack, &template, &[]).await;
        assert_eq!(
            code, 200,
            "case {case:?}: CreateStack must answer 200 — the deployment is what reports the \
             outcome — got {code} {created}"
        );

        let described = settled_stack(case, &router, REGION, row.stack).await;
        let reported = xml_text(&described, "StackStatus").unwrap_or_default();
        let reason = xml_text(&described, "StackStatusReason").unwrap_or_default();
        let own_events = stack_own_events(&router, case, row.stack).await;
        let progress = progress_reasons(&own_events);
        let failed = failed_reasons(&own_events);

        let expected_fragments = match &row.refused {
            Refused::Resolved => {
                assert_eq!(
                    reported,
                    status::CREATE_COMPLETE,
                    "case {case:?}: expected the stack to complete, got {reported:?} in \
                     {described}"
                );
                assert!(
                    progress.is_empty() && failed.is_empty(),
                    "case {case:?}: expected no diagnostic and no failed event of the \
                     stack's own, got {own_events:?}"
                );
                continue;
            }
            Refused::NotFound(named) => {
                // The FIXED sentence, with every reference the deployment could not resolve
                // between the brackets, named as the template wrote it.
                let listed: Vec<String> = named
                    .iter()
                    .map(|name| name.replace("{stack}", row.stack))
                    .collect();
                let expected = format!("Parameters: [{}] cannot be found.", listed.join(", "));
                assert_eq!(
                    progress.as_slice(),
                    [expected.as_str()],
                    "case {case:?}: expected the operation in flight to report {expected:?} \
                     on the stack's own event, got {own_events:?}"
                );
                listed
            }
            Refused::Diagnosed(fragments) => fragments
                .iter()
                .map(|fragment| fragment.replace("{stack}", row.stack))
                .collect(),
        };

        assert_eq!(
            reported,
            status::ROLLBACK_COMPLETE,
            "case {case:?}: expected a refused deployment to roll back, got {reported:?} in \
             {described}"
        );
        // ONE diagnostic and ONE failed event of the stack's own, both carrying what the
        // deployment could not do, and the stack settles reporting the same thing.
        let ([diagnostic], [recorded]) = (progress.as_slice(), failed.as_slice()) else {
            panic!(
                "case {case:?}: expected one diagnostic and one failed event of the stack's \
                 own, got {own_events:?}"
            );
        };
        for fragment in &expected_fragments {
            for (where_it_stands, reported) in [
                ("the stack's own diagnostic event", *diagnostic),
                ("the stack's own failed event", *recorded),
                ("StackStatusReason", reason.as_str()),
            ] {
                assert!(
                    reported.contains(fragment.as_str()),
                    "case {case:?}: expected {where_it_stands} to carry {fragment:?}, got \
                     {reported:?}"
                );
            }
        }
    }
}

/// The same refusal names EVERY reference the deployment could not resolve, even when they
/// stand in DIFFERENT resources: the deployment is refused ONCE, over the whole set, rather
/// than at the first reference it reached — so a client fixing the template reads every name it
/// has to fix from one failed deployment.
#[tokio::test]
async fn every_reference_a_deployment_cannot_resolve_is_named_once() {
    let case = "two resources, each carrying a reference nobody created";
    let stack = "dynref-event-missing-across";
    let router = router();
    let template = format!(
        r#"{{"Resources": {{{}, {}}}}}"#,
        parameter_carrier(
            CARRIER,
            &parameter_name(stack, "copy"),
            &format!("{{{{resolve:ssm:{}}}}}", parameter_name(stack, "alpha"))
        ),
        parameter_carrier(
            SECOND,
            &parameter_name(stack, "second-copy"),
            &format!("{{{{resolve:ssm:{}}}}}", parameter_name(stack, "beta"))
        ),
    );
    let (code, created) = create_stack(&router, stack, &template, &[]).await;
    assert_eq!(
        code, 200,
        "case {case:?}: CreateStack must answer 200, got {code} {created}"
    );

    let described = settled_stack(case, &router, REGION, stack).await;
    let reported = xml_text(&described, "StackStatus").unwrap_or_default();
    assert_eq!(
        reported,
        status::ROLLBACK_COMPLETE,
        "case {case:?}: expected a refused deployment to roll back, got {reported:?} in \
         {described}"
    );
    let own_events = stack_own_events(&router, case, stack).await;
    let progress = progress_reasons(&own_events);
    let [diagnostic] = progress.as_slice() else {
        panic!(
            "case {case:?}: expected one diagnostic event of the stack's own, got \
             {own_events:?}"
        );
    };
    assert!(
        diagnostic.starts_with("Parameters: [") && diagnostic.ends_with("] cannot be found."),
        "case {case:?}: expected CloudFormation's sentence for a reference nothing resolves, \
         got {diagnostic:?}"
    );
    for suffix in ["alpha", "beta"] {
        let named = format!("ssm:{}", parameter_name(stack, suffix));
        assert!(
            diagnostic.contains(&named),
            "case {case:?}: expected the sentence to name {named:?}, got {diagnostic:?}"
        );
    }
}
