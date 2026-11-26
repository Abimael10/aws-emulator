// SPDX-License-Identifier: CC0-1.0

//! A property value outside the pattern its resource type publishes is refused BEFORE anything
//! is provisioned, and the refusal is readable as the operation's own validation event.
//!
//! The rule under test: **a declared property value outside the pattern its resource type's
//! schema publishes for that property fails the stage that runs before any provider is
//! invoked, which records one `VALIDATION_ERROR` operation event for the resource that
//! declared it — `PROPERTY_VALIDATION`, `FAILED`, handled by `FAIL`, the value as far as the
//! type's own document may disclose it, the pattern it had to match, and the JSON pointer of
//! the property in the submitted template — and the stack rolls back having created nothing;
//! a value the pattern admits passes the stage.** How much of the judged value the reason
//! says is the type's to decide: a type carrying CREDENTIALS among its properties has its
//! values reported redacted, a type whose whole document is the template's own text has them
//! quoted back.
//!
//! The rows differ along what the rule generalises over: the PROPERTY judged (a name over an
//! alphabet, a host, two ARN-shaped properties), the form of the rejected value (a character
//! outside an alphabet, an empty value, text that is no ARN at all, an ARN of another
//! resource), the alternatives a pattern admits (a key ARN and a bare alias, a log-group ARN
//! and the empty value that clears it), the RESOURCE TYPE (one that carries credentials among
//! its properties and one that does not), and whether the value is admitted at all.
//!
//! Nothing here waits out a span: every stack is polled to its terminal status.

use serde_json::{json, Value};
use service_com_amazonaws_cloudformation_cloudformation::state::property_validation::enum_properties::ENUM_PROPERTIES;
use service_com_amazonaws_cloudformation_cloudformation::state::property_validation::patterned_properties::{
    pattern_failure_reason, PATTERNED_PROPERTIES,
};
use service_com_amazonaws_cloudformation_cloudformation::state::property_validation::{
    CREDENTIAL_PROPERTIES, PROPERTY_VALIDATION, REDACTED_VALUE,
};
use service_com_amazonaws_cloudformation_cloudformation::state::status;
use service_com_amazonaws_cloudformation_cloudformation::state::template::{
    BACKUP_GATEWAY_HYPERVISOR, CLOUDHSM_CLUSTER,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::harness::{
    call, create_stack, member, router, settled_stack_status, xml_blocks, xml_text, REGION,
};

/// The logical id every body below declares its resource under.
const DECLARED: &str = "Declared";

/// The `EventType` a validation result is reported under, from the model's own `EventType`
/// enum (`smithy_query(query="shape", shape="EventType")`).
const VALIDATION_ERROR: &str = "VALIDATION_ERROR";

/// `OperationEvent$ValidationStatus` for a validation the declaration did not pass, and
/// `OperationEvent$ValidationFailureMode` for one whose failure was handled by FAILING the
/// operation.
const FAILED: &str = "FAILED";
const FAIL: &str = "FAIL";

/// A key ARN of the form Backup Gateway publishes for a hypervisor's `KmsKeyArn`.
const KMS_KEY_ARN: &str =
    "arn:aws:kms:us-east-1:000000000000:key/1234abcd-12ab-34cd-56ef-1234567890ab";

/// A log-group ARN of the form `UpdateHypervisor` publishes for a hypervisor's `LogGroupArn`.
const LOG_GROUP_ARN: &str =
    "arn:aws:logs:us-east-1:000000000000:log-group:/aws/backup-gateway/hypervisors:*";

/// The pattern the catalogue the stage judges against holds for one property of one type, read
/// from that catalogue rather than restated here, so the suite and the rule cannot drift apart.
fn pattern_of(resource_type: &str, property: &str) -> &'static str {
    PATTERNED_PROPERTIES
        .iter()
        .find(|row| row.resource_type == resource_type && row.property == [property])
        .map(|row| row.pattern)
        .unwrap_or_else(|| panic!("the pattern of {resource_type} {property} is catalogued"))
}

/// Whether the type carries credentials among its properties, which is what decides how much
/// of a judged value a failure says.
fn carries_credentials(resource_type: &str) -> bool {
    CREDENTIAL_PROPERTIES
        .iter()
        .any(|row| row.resource_type == resource_type)
}

/// One row: the property document a template declares for a resource of `resource_type`, and
/// the property whose value the stage must refuse — `None` for a document every value of which
/// its pattern admits.
struct Row {
    case: &'static str,
    stack_name: &'static str,
    resource_type: &'static str,
    properties: Value,
    /// The property the stage must report, and the value it judged there.
    refused: Option<(&'static str, &'static str)>,
}

fn rows() -> Vec<Row> {
    vec![
        // The reported case: a hypervisor name carrying a character its alphabet does not.
        Row {
            case: "a hypervisor name carrying an underscore, which its alphabet does not admit",
            stack_name: "hypervisor-name-underscored",
            resource_type: BACKUP_GATEWAY_HYPERVISOR,
            properties: json!({
                "Host": "10.0.0.5",
                "Name": "a_hypervisor",
                "Username": "svc-backup-admin",
                "Password": "a-pass-phrase",
            }),
            refused: Some(("Name", "a_hypervisor")),
        },
        // The same property, another character outside the alphabet, and no credential stated
        // beside it: how much of the value the reason says is the TYPE's to decide, not this
        // one declaration's.
        Row {
            case: "a hypervisor name carrying a space",
            stack_name: "hypervisor-name-spaced",
            resource_type: BACKUP_GATEWAY_HYPERVISOR,
            properties: json!({ "Host": "10.0.0.6", "Name": "a hypervisor" }),
            refused: Some(("Name", "a hypervisor")),
        },
        // ...and a name its alphabet does admit, which the stage lets through.
        Row {
            case: "a hypervisor name over letters, digits and the hyphen",
            stack_name: "hypervisor-name-admitted",
            resource_type: BACKUP_GATEWAY_HYPERVISOR,
            properties: json!({ "Host": "10.0.0.7", "Name": "vcenter-01" }),
            refused: None,
        },
        // A second property of the same type, judged on the opposite form: its host publishes
        // any non-empty value, so the empty one is the value it refuses.
        Row {
            case: "a hypervisor host stated as the empty value",
            stack_name: "hypervisor-host-empty",
            resource_type: BACKUP_GATEWAY_HYPERVISOR,
            properties: json!({ "Host": "", "Name": "empty-host" }),
            refused: Some(("Host", "")),
        },
        Row {
            case: "a hypervisor host stated as a fully-qualified domain name",
            stack_name: "hypervisor-host-fqdn",
            resource_type: BACKUP_GATEWAY_HYPERVISOR,
            properties: json!({ "Host": "vcenter.example.internal", "Name": "fqdn-host" }),
            refused: None,
        },
        // An ARN-shaped property: text that is no ARN at all.
        Row {
            case: "a hypervisor encryption key that is no ARN at all",
            stack_name: "hypervisor-key-not-an-arn",
            resource_type: BACKUP_GATEWAY_HYPERVISOR,
            properties: json!({
                "Host": "10.0.0.8",
                "Name": "bad-key",
                "KmsKeyArn": "not-a-key-arn",
            }),
            refused: Some(("KmsKeyArn", "not-a-key-arn")),
        },
        // ...an ARN of the right service naming another kind of resource.
        Row {
            case: "a hypervisor encryption key naming no key or alias of that service",
            stack_name: "hypervisor-key-wrong-resource",
            resource_type: BACKUP_GATEWAY_HYPERVISOR,
            properties: json!({
                "Host": "10.0.0.9",
                "Name": "wrong-key-resource",
                "KmsKeyArn": "arn:aws:kms:us-east-1:000000000000:grant/0123456789abcdef",
            }),
            refused: Some((
                "KmsKeyArn",
                "arn:aws:kms:us-east-1:000000000000:grant/0123456789abcdef",
            )),
        },
        // ...and the two forms it admits: the key ARN and the bare alias.
        Row {
            case: "a hypervisor encryption key stated as a key ARN",
            stack_name: "hypervisor-key-arn",
            resource_type: BACKUP_GATEWAY_HYPERVISOR,
            properties: json!({
                "Host": "10.0.0.10",
                "Name": "key-arn",
                "KmsKeyArn": KMS_KEY_ARN,
            }),
            refused: None,
        },
        Row {
            case: "a hypervisor encryption key stated as a bare alias",
            stack_name: "hypervisor-key-alias",
            resource_type: BACKUP_GATEWAY_HYPERVISOR,
            properties: json!({
                "Host": "10.0.0.11",
                "Name": "key-alias",
                "KmsKeyArn": "alias/backup-gateway",
            }),
            refused: None,
        },
        // The log group, the one property only the follow-up update carries: text that is no
        // log-group ARN, the ARN form it admits, and the empty value that clears it.
        Row {
            case: "a hypervisor log group that is no log-group ARN",
            stack_name: "hypervisor-log-group-bad",
            resource_type: BACKUP_GATEWAY_HYPERVISOR,
            properties: json!({
                "Host": "10.0.0.12",
                "Name": "bad-log-group",
                "LogGroupArn": "not-a-log-group-arn",
            }),
            refused: Some(("LogGroupArn", "not-a-log-group-arn")),
        },
        Row {
            case: "a hypervisor log group stated as a log-group ARN",
            stack_name: "hypervisor-log-group-arn",
            resource_type: BACKUP_GATEWAY_HYPERVISOR,
            properties: json!({
                "Host": "10.0.0.13",
                "Name": "logged",
                "LogGroupArn": LOG_GROUP_ARN,
            }),
            refused: None,
        },
        Row {
            case: "a hypervisor log group stated as the empty value its pattern admits",
            stack_name: "hypervisor-log-group-empty",
            resource_type: BACKUP_GATEWAY_HYPERVISOR,
            properties: json!({ "Host": "10.0.0.14", "Name": "unlogged", "LogGroupArn": "" }),
            refused: None,
        },
        // Another TYPE in the same catalogue, carrying no credential among its properties: the
        // same stage refuses the same class of value and QUOTES it back, which is the other
        // side of the rule about how much of a judged value a reason says.
        Row {
            case: "a cluster HSM type that is no HSM type at all",
            stack_name: "cluster-hsm-type-bad",
            resource_type: CLOUDHSM_CLUSTER,
            properties: json!({
                "HsmType": "not-a-valid-hsm-type",
                "SubnetIds": ["subnet-0a1b2c3d"],
            }),
            refused: Some(("HsmType", "not-a-valid-hsm-type")),
        },
        Row {
            case: "a cluster HSM type its published form admits",
            stack_name: "cluster-hsm-type-admitted",
            resource_type: CLOUDHSM_CLUSTER,
            properties: json!({
                "HsmType": "hsm2m.medium",
                "SubnetIds": ["subnet-0a1b2c3d"],
            }),
            refused: None,
        },
    ]
}

/// The validation RESULTS the operation's own events report for `stack`: one tuple per event
/// carrying a validation, in the order they are reported.
async fn validation_results(
    router: &MultiServiceRouter,
    case: &str,
    stack: &str,
) -> Vec<(String, String, String, String, String, String)> {
    let (code, body) = call(router, "DescribeEvents", &[member("StackName", stack)]).await;
    assert_eq!(
        code, 200,
        "case {case:?}: the operation's events must be readable, got {code} {body}"
    );
    xml_blocks(&body, "member")
        .iter()
        .filter(|event| xml_text(event, "EventType").as_deref() == Some(VALIDATION_ERROR))
        .map(|event| {
            (
                xml_text(event, "LogicalResourceId").unwrap_or_default(),
                xml_text(event, "ValidationName").unwrap_or_default(),
                xml_text(event, "ValidationStatus").unwrap_or_default(),
                xml_text(event, "ValidationFailureMode").unwrap_or_default(),
                xml_text(event, "ValidationStatusReason").unwrap_or_default(),
                xml_text(event, "ValidationPath").unwrap_or_default(),
            )
        })
        .collect()
}

/// A value outside the pattern its type publishes for the property fails the stage that runs
/// before any provider is invoked — naming the resource, the pattern, the property's own path
/// and the value as far as the type's document may disclose it — while a value the pattern
/// admits passes it, whatever the stack then does about the service that owns the resource.
#[tokio::test]
async fn a_value_outside_its_published_pattern_fails_property_validation() {
    for row in rows() {
        let case = row.case;
        // A router of its own per row: what each case is about is its own declaration, not
        // what another row left behind.
        let router = router();
        let template = json!({
            "Resources": {
                DECLARED: { "Type": row.resource_type, "Properties": row.properties }
            }
        })
        .to_string();

        let (code, created) = create_stack(&router, row.stack_name, &template, &[]).await;
        assert_eq!(
            code, 200,
            "case {case:?}: CreateStack must answer 200 — the body declares a type AWS \
             publishes — got {code} {created}"
        );

        let settled = settled_stack_status(case, &router, REGION, row.stack_name).await;
        let results = validation_results(&router, case, row.stack_name).await;
        let Some((property, judged)) = row.refused else {
            assert!(
                results.is_empty(),
                "case {case:?}: expected a value its pattern admits to pass the stage, got \
                 {results:?}"
            );
            continue;
        };
        assert_eq!(
            settled,
            status::ROLLBACK_COMPLETE,
            "case {case:?}: expected a value outside its pattern to roll the stack back, got \
             {settled:?}"
        );
        let reported = if carries_credentials(row.resource_type) {
            REDACTED_VALUE
        } else {
            judged
        };
        assert_eq!(
            results,
            vec![(
                DECLARED.to_string(),
                PROPERTY_VALIDATION.to_string(),
                FAILED.to_string(),
                FAIL.to_string(),
                pattern_failure_reason(reported, pattern_of(row.resource_type, property)),
                format!("/Resources/{DECLARED}/Properties/{property}"),
            )],
            "case {case:?}: expected the one validation result the refused value owes, got \
             {results:?}"
        );
    }
}

/// The invariant the redaction rests on: no value this stage ever judges is itself a
/// credential, so what a failure names is a property of the declaration and never a secret.
/// The catalogues are read rather than restated, so a row added later is held to it too.
#[test]
fn no_judged_property_of_a_credential_carrying_type_is_a_credential() {
    for row in CREDENTIAL_PROPERTIES {
        for credential in row.credentials {
            let patterned = PATTERNED_PROPERTIES.iter().any(|judged| {
                judged.resource_type == row.resource_type
                    && judged.property.contains(credential)
            });
            assert!(
                !patterned,
                "case {:?} {credential:?}: expected no pattern row to judge a credential, one \
                 does",
                row.resource_type
            );
            let in_enum = ENUM_PROPERTIES.iter().any(|judged| {
                judged.resource_type == row.resource_type
                    && judged.property.first() == Some(credential)
            });
            assert!(
                !in_enum,
                "case {:?} {credential:?}: expected no value-set row to judge a credential, one \
                 does",
                row.resource_type
            );
        }
    }
}
