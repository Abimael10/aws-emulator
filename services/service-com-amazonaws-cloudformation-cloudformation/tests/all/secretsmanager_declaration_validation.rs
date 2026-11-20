// SPDX-License-Identifier: CC0-1.0

//! The PRE-DEPLOYMENT validation an `AWS::SecretsManager::*` declaration is held to against
//! the properties its type HAS, end to end over the awsQuery wire.
//!
//! RULE: CloudFormation validates each declared resource against its resource type's schema
//! before provisioning anything, so a declaration carrying a member that schema states no
//! property for fails the operation before any resource is created: the stack records a
//! `PROPERTY_VALIDATION` validation result — `FAILED`, handled by failing, reading
//! `Unsupported property [<member>]` at that member's own
//! `/Resources/<logical id>/Properties/<member>` — the stack's OWN `CREATE_FAILED` event
//! carries `DetailedStatus` `VALIDATION_FAILED` with the summary over the stage's errors, and
//! the stack rolls back to `ROLLBACK_COMPLETE` with nothing provisioned. A declaration every
//! member of which the schema states is deployed as written, less any member the schema marks
//! read-only — which is no input to any call of the type.
//!
//! `unsupported_resource_properties` holds the same rule over the types five other services
//! own, so what the two suites assert together is the STAGE rather than anything about one
//! service's types.
//!
//! The rows differ along what the rule generalises over: which `AWS::SecretsManager::*` type
//! the declaration is of, and what KIND of member the schema does not state (a name nothing
//! publishes, a member of the owning API that is not a property of the type, a property
//! another type of the same service has, and a misspelling of one the type does have) — plus
//! the two control rows, one carrying only members the schema states and one carrying the
//! read-only `Id` beside them, both of which are really created in Secrets Manager.

use serde_json::{json, Value};

use crate::harness::{
    call, create_stack, member, router, settled_stack, xml_blocks, xml_text, REGION,
};
use crate::stack_owned_secret_teardown::secretsmanager;
use service_com_amazonaws_cloudformation_cloudformation::state::{resource_status, status};

/// The validation CloudFormation names for the check that holds a declaration's properties to
/// its type's contract. Source: `state::property_validation::PROPERTY_VALIDATION`.
const PROPERTY_VALIDATION: &str = "PROPERTY_VALIDATION";

/// The `EventType` a validation result is reported under ("Validation errors - Failures from
/// CloudFormation Early Validations").
const VALIDATION_ERROR: &str = "VALIDATION_ERROR";

/// `StackEvent$DetailedStatus` for an operation a pre-deployment validation failed, from the
/// model's `DetailedStatus` enum (`[CONFIGURATION_COMPLETE, VALIDATION_FAILED]`).
const VALIDATION_FAILED: &str = "VALIDATION_FAILED";

/// How a failed validation reports that it FAILED, and how that failure was handled. Source:
/// the model's `ValidationStatus` and `ValidationFailureMode` enums.
const FAILED: &str = "FAILED";
/// See [`FAILED`].
const FAIL: &str = "FAIL";

/// The logical id every template below declares its resource under.
const LOGICAL_ID: &str = "Secret";

/// The type whose declaration most rows carry.
const SECRET: &str = "AWS::SecretsManager::Secret";

/// One row: a declaration, and the member the validation must report as unsupported — `None`
/// for a declaration every member of which its type's schema states.
struct Row {
    case: &'static str,
    stack: &'static str,
    resource_type: &'static str,
    /// The name the declaration states for its secret, which is also the name a client reads
    /// the secret back by — and the name Secrets Manager must hold NO secret under where the
    /// declaration was refused.
    secret_name: &'static str,
    /// The property document the declaration carries, beside the `Name` above.
    extra: fn() -> Value,
    unsupported: Option<&'static str>,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a secret carrying a member no resource type publishes",
            stack: "sm-validation-unknown-member",
            resource_type: SECRET,
            secret_name: "sm-validation-unknown-member-secret",
            extra: || json!({"SecretString": "v1", "ThisPropertyDoesNotExist": 1}),
            unsupported: Some("ThisPropertyDoesNotExist"),
        },
        Row {
            case: "a secret carrying a member of the owning API rather than of the type",
            stack: "sm-validation-api-member",
            resource_type: SECRET,
            secret_name: "sm-validation-api-member-secret",
            extra: || json!({"SecretString": "v1", "ClientRequestToken": "a-token"}),
            unsupported: Some("ClientRequestToken"),
        },
        Row {
            case: "a secret carrying the member another type of the same service has",
            stack: "sm-validation-sibling-member",
            resource_type: SECRET,
            secret_name: "sm-validation-sibling-member-secret",
            extra: || json!({"SecretString": "v1", "SecretId": "another-secret"}),
            unsupported: Some("SecretId"),
        },
        Row {
            case: "a secret misspelling the encryption key it names",
            stack: "sm-validation-misspelled-member",
            resource_type: SECRET,
            secret_name: "sm-validation-misspelled-member-secret",
            extra: || json!({"SecretString": "v1", "KmsKeyID": "alias/aws/secretsmanager"}),
            unsupported: Some("KmsKeyID"),
        },
        Row {
            case: "a resource policy carrying the value member the secret type has",
            stack: "sm-validation-policy-member",
            resource_type: "AWS::SecretsManager::ResourcePolicy",
            secret_name: "sm-validation-policy-member-secret",
            extra: || {
                json!({
                    "ResourcePolicy": {"Version": "2012-10-17", "Statement": []},
                    "SecretString": "v1",
                })
            },
            unsupported: Some("SecretString"),
        },
        Row {
            case: "a rotation schedule misspelling the rules it states",
            stack: "sm-validation-schedule-member",
            resource_type: "AWS::SecretsManager::RotationSchedule",
            secret_name: "sm-validation-schedule-member-secret",
            extra: || json!({"RotationRule": {"AutomaticallyAfterDays": 30}}),
            unsupported: Some("RotationRule"),
        },
        Row {
            case: "a secret carrying only members its type's schema states",
            stack: "sm-validation-control",
            resource_type: SECRET,
            secret_name: "sm-validation-control-secret",
            extra: || {
                json!({
                    "SecretString": "v1-secret-value",
                    "Description": "the control row's secret",
                    "Tags": [{"Key": "team", "Value": "ops"}],
                })
            },
            unsupported: None,
        },
        Row {
            case: "a secret carrying the read-only Id its schema states beside its own members",
            stack: "sm-validation-read-only",
            resource_type: SECRET,
            secret_name: "sm-validation-read-only-secret",
            extra: || {
                json!({
                    "SecretString": "v1-secret-value",
                    "Id": "arn:aws:secretsmanager:us-east-1:000000000000:secret:declared-AbCd12",
                })
            },
            unsupported: None,
        },
    ]
}

/// The template one row deploys: its resource, under the one logical id, naming the secret the
/// row states.
///
/// The `SecretId` of the two rows that are ABOUT a secret rather than one — a resource policy
/// and a rotation schedule — is the same name: the declaration is refused before anything is
/// asked of any service, so nothing resolves it.
fn template(row: &Row) -> String {
    let mut properties = match (row.extra)() {
        Value::Object(members) => members,
        other => panic!("case {:?}: the row's extra properties must be an object, got {other:?}", row.case),
    };
    let names_itself = row.resource_type == SECRET;
    let name_property = if names_itself { "Name" } else { "SecretId" };
    properties.insert(
        name_property.to_string(),
        Value::from(row.secret_name),
    );
    json!({
        "Resources": {
            LOGICAL_ID: {"Type": row.resource_type, "Properties": Value::Object(properties)},
        },
    })
    .to_string()
}

/// How this service summarises a failed pre-deployment property validation on the stack: the
/// count of errors and where their detail is. Source:
/// `state::property_validation::validation_failure_summary`.
fn summary(errors: usize) -> String {
    format!(
        "Validation failed with {errors} error(s). Call DescribeEvents to retrieve the full \
         list of issues with resource and property details, resolve each error, then retry \
         the operation."
    )
}

/// The validation RESULTS `DescribeEvents` reports for `stack`: one row per event carrying a
/// validation, as the fields a client reads it by.
async fn validation_results(
    case: &str,
    router: &smithy_server_runtime::MultiServiceRouter,
    stack: &str,
) -> Vec<Vec<String>> {
    let (code, events) = call(router, "DescribeEvents", &[member("StackName", stack)]).await;
    assert_eq!(
        code, 200,
        "case {case:?}: the operation's events must be readable, got {code} {events}"
    );
    xml_blocks(&events, "member")
        .iter()
        .filter(|event| xml_text(event, "ValidationName").is_some())
        .map(|event| {
            [
                "LogicalResourceId",
                "ResourceType",
                "EventType",
                "ValidationName",
                "ValidationStatus",
                "ValidationFailureMode",
                "ValidationStatusReason",
                "ValidationPath",
            ]
            .iter()
            .map(|field| xml_text(event, field).unwrap_or_default())
            .collect()
        })
        .collect()
}

/// Every row: a declaration carrying a member its type's schema does not state fails before
/// anything is provisioned, and the ones whose members the schema all states are deployed.
#[tokio::test]
async fn a_member_the_secret_type_does_not_have_rolls_the_stack_back() {
    for row in rows() {
        let case = row.case;
        // A router of its own per row: what each case is about is its own declaration, not
        // what another row left behind.
        let router = router();

        let (code, created) = create_stack(&router, row.stack, &template(&row), &[]).await;
        assert_eq!(
            code, 200,
            "case {case:?}: CreateStack must answer 200 — the request is valid and the stack \
             exists — got {code} {created}"
        );
        let described = settled_stack(case, &router, REGION, row.stack).await;
        let expected_status = match row.unsupported {
            Some(_) => status::ROLLBACK_COMPLETE,
            None => status::CREATE_COMPLETE,
        };
        let settled = xml_text(&described, "StackStatus").unwrap_or_default();
        assert_eq!(
            settled, expected_status,
            "case {case:?}: expected the stack to settle {expected_status}, got {settled:?} \
             ({described})"
        );

        // The detail of the failure: an event about the resource NOTHING was done to, naming
        // the validation, its status, how its failure was handled, the member the type does
        // not have and the path that member was carried at.
        let results = validation_results(case, &router, row.stack).await;
        let expected_results: Vec<Vec<String>> = row
            .unsupported
            .map(|property| {
                vec![vec![
                    LOGICAL_ID.to_string(),
                    row.resource_type.to_string(),
                    VALIDATION_ERROR.to_string(),
                    PROPERTY_VALIDATION.to_string(),
                    FAILED.to_string(),
                    FAIL.to_string(),
                    format!("Unsupported property [{property}]"),
                    format!("/Resources/{LOGICAL_ID}/Properties/{property}"),
                ]]
            })
            .unwrap_or_default();
        assert_eq!(
            results, expected_results,
            "case {case:?}: expected the validation results {expected_results:?}, got \
             {results:?}"
        );

        // ...and what the STACK reports for it: its OWN failure event, under the stack's own
        // name, with the detailed status the model gives a validation failure and the summary
        // over the stage's errors — which is what points a client at the detail above.
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
        let expected_failed: Vec<(String, String)> = row
            .unsupported
            .map(|_| vec![(VALIDATION_FAILED.to_string(), summary(1))])
            .unwrap_or_default();
        assert_eq!(
            failed, expected_failed,
            "case {case:?}: expected the stack's failure event {expected_failed:?}, got \
             {stack_events}"
        );

        // What the owning service holds is the proof: a refused declaration left no secret in
        // Secrets Manager at all, and a deployed one is read back there under the name it
        // declared, with the value it declared — under the ARN that service assigned, never
        // the one a declaration stated for the read-only `Id`.
        let (code, served) = secretsmanager(
            &router,
            "GetSecretValue",
            &json!({"SecretId": row.secret_name}),
        )
        .await;
        match row.unsupported {
            Some(_) => assert_ne!(
                code, 200,
                "case {case:?}: a refused declaration must leave no secret behind, got {code} \
                 {served}"
            ),
            None => {
                assert_eq!(
                    code, 200,
                    "case {case:?}: the deployed secret must be readable, got {code} {served}"
                );
                assert_eq!(
                    served.get("SecretString").and_then(Value::as_str),
                    Some("v1-secret-value"),
                    "case {case:?}: expected the declared value, got {served}"
                );
                let arn = served.get("ARN").and_then(Value::as_str).unwrap_or_default();
                let assigned = arn.starts_with(&format!(
                    "arn:aws:secretsmanager:{REGION}:000000000000:secret:{}-",
                    row.secret_name
                ));
                assert!(
                    assigned,
                    "case {case:?}: expected the ARN Secrets Manager assigned the declared \
                     name, got {arn:?}"
                );
            }
        }
    }
}
