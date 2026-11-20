// SPDX-License-Identifier: CC0-1.0

//! What a stack does with an `AWS::SSM::Parameter` declared as the one parameter type that
//! resource type does not publish, end to end over the awsQuery wire.
//!
//! The rule every case here holds the service to: **a property value outside the set its
//! resource type's contract publishes fails the stack operation before anything is
//! provisioned — `AWS::SSM::Parameter`'s `Type` publishes `String | StringList` ("CloudFormation
//! doesn't support creating a SecureString parameter type"), so a declaration of the encrypted
//! type Systems Manager's own API admits is one no stack can deploy. The operation records the
//! `PROPERTY_VALIDATION` result, nothing is asked of the owning service, and the stack reaches
//! `CREATE_FAILED`: it RESTS there when the request asked for the failed state to be kept
//! (`DisableRollback=true`, which `OnFailure=DO_NOTHING` also spells) and rolls back to
//! `ROLLBACK_COMPLETE` otherwise. Either way the resource was never created, so
//! `DescribeStackResource` for its logical id answers the documented `ValidationError`.**
//!
//! The rows differ along what the rule generalises over: the value the `Type` property carries
//! (the one outside the published set, and each of the two inside it, which are deployed and
//! read back out of Systems Manager itself), and what the request asks for when a resource
//! fails (`DisableRollback`, the `OnFailure` that spells the same thing, and the default
//! rollback). `unsupported_resource_properties.rs` is the suite for the other question this
//! stage decides — a member the type has no property for at all.

use serde_json::{json, Value};

use service_com_amazonaws_cloudformation_cloudformation::state::{resource_status, status};

use crate::dynamic_references::ssm;
use crate::harness::{
    call, create_stack, member, router, settled_stack, xml_blocks, xml_text, REGION,
};

/// The logical id every template below declares its parameter under.
const LOGICAL_ID: &str = "Param";

/// The validation CloudFormation names for the check that holds a declaration's properties to
/// its type's contract, the `EventType` its result is reported under, and the `DetailedStatus`
/// the stack's own failure event carries for a failed pre-deployment validation. Sources:
/// `state::property_validation::PROPERTY_VALIDATION`, the model's `EventType` and its
/// `DetailedStatus` enum.
const PROPERTY_VALIDATION: &str = "PROPERTY_VALIDATION";
const VALIDATION_ERROR: &str = "VALIDATION_ERROR";
const VALIDATION_FAILED: &str = "VALIDATION_FAILED";

/// The values `AWS::SSM::Parameter`'s `Type` publishes, as the resource type's own property
/// reference lists them ("Allowed values: String | StringList"). Source:
/// `state::property_validation::CONSTRAINED_PROPERTIES`.
const PUBLISHED_PARAMETER_TYPES: &str = "String, StringList";

/// The parameter type Systems Manager's API admits that this resource type does not publish.
const SECURE_STRING: &str = "SecureString";

/// One row: the parameter type declared, what the request asks for on failure, and the status
/// the stack must settle in.
struct Row {
    case: &'static str,
    stack: &'static str,
    /// The `Type` the declaration carries.
    parameter_type: &'static str,
    /// The members the `CreateStack` request carries beside the template.
    on_failure: &'static [(&'static str, &'static str)],
    settles_in: &'static str,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "the encrypted type, with the failed state kept",
            stack: "securestring-kept",
            parameter_type: SECURE_STRING,
            on_failure: &[("DisableRollback", "true")],
            settles_in: status::CREATE_FAILED,
        },
        Row {
            case: "the encrypted type, with the OnFailure that spells the same thing",
            stack: "securestring-do-nothing",
            parameter_type: SECURE_STRING,
            on_failure: &[("OnFailure", "DO_NOTHING")],
            settles_in: status::CREATE_FAILED,
        },
        Row {
            case: "the encrypted type, with the default rollback",
            stack: "securestring-rolled-back",
            parameter_type: SECURE_STRING,
            on_failure: &[],
            settles_in: status::ROLLBACK_COMPLETE,
        },
        Row {
            case: "a plain string parameter, with the failed state kept",
            stack: "securestring-control-string",
            parameter_type: "String",
            on_failure: &[("DisableRollback", "true")],
            settles_in: status::CREATE_COMPLETE,
        },
        Row {
            case: "a string list parameter, with the default rollback",
            stack: "securestring-control-list",
            parameter_type: "StringList",
            on_failure: &[],
            settles_in: status::CREATE_COMPLETE,
        },
    ]
}

impl Row {
    /// Whether this row's declaration is one the type's contract admits.
    fn admitted(&self) -> bool {
        self.parameter_type != SECURE_STRING
    }

    /// The parameter name the row's declaration states, which is also the name a client reads
    /// the deployed parameter back by — and the name Systems Manager must hold nothing under
    /// where the declaration was refused.
    fn parameter_name(&self) -> String {
        format!("/{}/probe", self.stack)
    }

    fn template(&self) -> String {
        json!({
            "Resources": {
                LOGICAL_ID: {
                    "Type": "AWS::SSM::Parameter",
                    "Properties": {
                        "Name": self.parameter_name(),
                        "Type": self.parameter_type,
                        "Value": "a,b",
                    },
                },
            },
        })
        .to_string()
    }
}

/// How this service reports a property value its type does not admit, and the summary the
/// stack carries for the stage that found it. Sources:
/// `state::property_validation::failure_reason` and `validation_failure_summary`.
fn validation_reason() -> String {
    format!(
        "Property validation failure: [Value for property {{/Type}} of resource \
         [{LOGICAL_ID}] must be one of [{PUBLISHED_PARAMETER_TYPES}]]"
    )
}

fn summary(errors: usize) -> String {
    format!(
        "Validation failed with {errors} error(s). Call DescribeEvents to retrieve the full \
         list of issues with resource and property details, resolve each error, then retry \
         the operation."
    )
}

#[tokio::test]
async fn a_parameter_type_the_resource_type_does_not_publish_fails_the_stack() {
    for row in rows() {
        let case = row.case;
        // A router of its own per row: what each case is about is its own declaration, not
        // what another row left behind.
        let router = router();
        let extra: Vec<(String, String)> = row
            .on_failure
            .iter()
            .map(|(key, value)| member(key, value))
            .collect();

        let (code, created) = create_stack(&router, row.stack, &row.template(), &extra).await;
        assert_eq!(
            code, 200,
            "case {case:?}: CreateStack must answer 200 — the request is valid and the stack \
             exists — got {code} {created}"
        );
        let described = settled_stack(case, &router, REGION, row.stack).await;
        let settled = xml_text(&described, "StackStatus").unwrap_or_default();
        assert_eq!(
            settled, row.settles_in,
            "case {case:?}: expected the stack to settle {}, got {settled:?} ({described})",
            row.settles_in
        );

        // The detail of the failure: a validation result about the resource NOTHING was done
        // to, naming the validation, how its failure was handled, the property whose value the
        // type does not admit and the set it publishes.
        let (code, events) =
            call(&router, "DescribeEvents", &[member("StackName", row.stack)]).await;
        assert_eq!(
            code, 200,
            "case {case:?}: the operation's events must be readable, got {code} {events}"
        );
        let results: Vec<Vec<String>> = xml_blocks(&events, "member")
            .iter()
            .filter(|event| xml_text(event, "ValidationName").is_some())
            .map(|event| {
                [
                    "LogicalResourceId",
                    "EventType",
                    "ValidationName",
                    "ValidationStatus",
                    "ValidationStatusReason",
                    "ValidationPath",
                ]
                .iter()
                .map(|field| xml_text(event, field).unwrap_or_default())
                .collect()
            })
            .collect();
        let expected_results: Vec<Vec<String>> = if row.admitted() {
            Vec::new()
        } else {
            vec![vec![
                LOGICAL_ID.to_string(),
                VALIDATION_ERROR.to_string(),
                PROPERTY_VALIDATION.to_string(),
                "FAILED".to_string(),
                validation_reason(),
                format!("/Resources/{LOGICAL_ID}/Properties"),
            ]]
        };
        assert_eq!(
            results, expected_results,
            "case {case:?}: expected the validation results {expected_results:?}, got \
             {results:?}"
        );

        // ...and what the STACK reports for it: its own failure event, with the detailed
        // status the model gives a validation failure and the summary over the stage's errors.
        let (code, stack_events) = call(
            &router,
            "DescribeStackEvents",
            &[member("StackName", row.stack)],
        )
        .await;
        assert_eq!(
            code, 200,
            "case {case:?}: the stack's events must be readable, got {code} {stack_events}"
        );
        let failed: Vec<(String, String)> = xml_blocks(&stack_events, "member")
            .iter()
            .filter(|event| {
                xml_text(event, "LogicalResourceId").as_deref() == Some(row.stack)
                    && xml_text(event, "ResourceStatus").as_deref()
                        == Some(resource_status::CREATE_FAILED)
            })
            .map(|event| {
                (
                    xml_text(event, "DetailedStatus").unwrap_or_default(),
                    xml_text(event, "ResourceStatusReason").unwrap_or_default(),
                )
            })
            .collect();
        let expected_failed: Vec<(String, String)> = if row.admitted() {
            Vec::new()
        } else {
            vec![(VALIDATION_FAILED.to_string(), summary(1))]
        };
        assert_eq!(
            failed, expected_failed,
            "case {case:?}: expected the stack's failure event {expected_failed:?}, got \
             {stack_events}"
        );

        // The resource itself: a declaration that was never provisioned is one the stack has
        // no resource for, whichever status the stack came to rest in, so the per-resource read
        // answers the documented ValidationError; the deployed one reports itself created.
        let (code, resource) = call(
            &router,
            "DescribeStackResource",
            &[
                member("StackName", row.stack),
                member("LogicalResourceId", LOGICAL_ID),
            ],
        )
        .await;
        if row.admitted() {
            assert_eq!(
                (code, xml_text(&resource, "ResourceStatus").unwrap_or_default()),
                (200, resource_status::CREATE_COMPLETE.to_string()),
                "case {case:?}: expected the deployed resource reported created, got {code} \
                 {resource}"
            );
        } else {
            assert_eq!(
                (
                    code,
                    xml_text(&resource, "Code").unwrap_or_default(),
                    xml_text(&resource, "Message").unwrap_or_default(),
                ),
                (
                    400,
                    "ValidationError".to_string(),
                    format!(
                        "Resource {LOGICAL_ID} does not exist for stack {}",
                        row.stack
                    ),
                ),
                "case {case:?}: expected the documented refusal for a resource the stack has \
                 none of, got {code} {resource}"
            );
        }

        // What the owning service holds is the proof: the refused declarations left no
        // parameter in Systems Manager at all, and the deployed ones are read back there by
        // the name they declared, as the type they declared.
        let (code, parameter) =
            ssm(&router, "GetParameter", &json!({"Name": row.parameter_name()})).await;
        if row.admitted() {
            assert_eq!(
                code, 200,
                "case {case:?}: the deployed parameter must be readable, got {code} {parameter}"
            );
            assert_eq!(
                parameter.pointer("/Parameter/Type").and_then(Value::as_str),
                Some(row.parameter_type),
                "case {case:?}: expected the declared parameter type, got {parameter}"
            );
        } else {
            assert_ne!(
                code, 200,
                "case {case:?}: a refused declaration must leave no parameter behind, got \
                 {code} {parameter}"
            );
        }
    }
}
