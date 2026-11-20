// SPDX-License-Identifier: CC0-1.0

//! Cloud Control API over the secrets AWS Secrets Manager holds.
//!
//! The rule every case here holds the service to: **a resource of a type another service of
//! this emulator owns is resolved, enumerated, provisioned, patched and removed in THAT
//! service's own state, whichever API created it — "you can use this action to return
//! information about an existing resource in your account and Amazon Web Services Region,
//! whether those resources were provisioned using Cloud Control API" (`GetResource`) — and it
//! answers to the identifier the type's schema declares.** For `AWS::SecretsManager::Secret`
//! that identifier is the secret's own ARN (`primaryIdentifier` `/properties/Id`), which
//! Secrets Manager assigns, so a secret `secretsmanager:CreateSecret` created — or a
//! CloudFormation stack declared — is one `GetResource` reports under that ARN, and one
//! Secrets Manager no longer holds is one it reports as not found.
//!
//! The rows differ along what the rule generalises over: which API the secret came into being
//! through (that service's own, a CloudFormation stack, and Cloud Control API itself), the
//! form of its name, and which properties it carries.
//!
//! `ssm_parameter_read_through`, `sns_topic_read_through`, `sqs_queue_read_through` and
//! `native_resource_resolution` hold the same rule for other owning services, so what is
//! asserted across the suites is where a resource LIVES rather than anything about one of
//! them.
//!
//! The last case holds the service to a second rule, about WHICH properties such a model
//! carries: **a resource model this service reports is serialised from the resource type's
//! schema without the properties that schema declares `writeOnlyProperties`, so a property a
//! caller only ever SETS is in no `ResourceDescription.Properties` and no
//! `ProgressEvent.ResourceModel`** — for a secret that is its VALUE, which takes effect on the
//! secret and which the owning service's own `GetSecretValue` is what answers.

use std::time::Instant;

use serde_json::{json, Value};

use super::emulator::{
    code, message, properties_of, text, Emulator, SETTLED_BUDGET, SETTLED_INTERVAL, SIGNING_REGION,
    STATUS_FAILED,
};
use super::refusal_wording::{assert_handler_not_found, generic_not_found};

/// The type whose resources live in the Secrets Manager service's own store.
const SECRET: &str = "AWS::SecretsManager::Secret";

/// The properties of this type a caller SETS and no read of the resource reports.
///
/// Catalog source: the `writeOnlyProperties` of the `AWS::SecretsManager::Secret` resource
/// provider schema, with the `/properties/` prefix of each pointer dropped. The emulator's own
/// catalogue of that list is pinned to this one below, so a case here holds the service to the
/// schema rather than to the catalogue.
const WRITE_ONLY: [&str; 2] = ["SecretString", "GenerateSecretString"];

/// The identifiers `ListResources` reports for the secrets, walking every page.
async fn inventory(emulator: &Emulator) -> Vec<String> {
    emulator.walk(SECRET, None).await
}

/// The resource model `GetResource` reports for `reference`, required to be answered, with the
/// identifier the resource answers to.
async fn resolved(emulator: &Emulator, case: &str, reference: &str) -> (String, Value) {
    let (status, body) = emulator.get(SECRET, reference).await;
    assert_eq!(
        status, 200,
        "case {case:?}: GetResource expected HTTP 200, got {status} with body {body:?}"
    );
    let description = &body["ResourceDescription"];
    (
        text(description, "Identifier"),
        properties_of(description),
    )
}

/// The value the OWNING service's own read serves for `secret_id`, required to be answered.
async fn served(emulator: &Emulator, case: &str, secret_id: &str) -> Value {
    let (status, body) = emulator
        .secretsmanager("GetSecretValue", json!({ "SecretId": secret_id }))
        .await;
    assert_eq!(
        status, 200,
        "case {case:?}: secretsmanager:GetSecretValue expected HTTP 200, got {status} with \
         body {body:?}"
    );
    body
}

/// Assert that `model` carries no property the type's schema declares write-only.
fn assert_projection(model: &Value, case: &str, source: &str) {
    for property in WRITE_ONLY {
        assert!(
            model.get(property).is_none(),
            "case {case:?} ({source}, {property}): a write-only property is in no resource \
             model this service reports, got {model:?}"
        );
    }
}

/// Wait until `reference` resolves to nothing, which is what the owning service's own removal
/// of the secret leaves behind, answering the refusal's own message.
///
/// The condition is waited for rather than a span: that service performs a forced deletion
/// "with an asynchronous background process, so there might be a short delay before the secret
/// is permanently deleted", and the wait is over as soon as the secret is gone.
async fn gone(emulator: &Emulator, case: &str, reference: &str) -> String {
    let deadline = Instant::now() + SETTLED_BUDGET;
    loop {
        let (status, body) = emulator.get(SECRET, reference).await;
        if status != 200 {
            assert!(
                code(&body).contains("ResourceNotFoundException"),
                "case {case:?}: expected the not-found refusal, got {status} with body {body:?}"
            );
            return message(&body);
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected {reference:?} to stop resolving once the owning service \
             had removed the secret, got {status} with body {body:?}"
        );
        tokio::time::sleep(SETTLED_INTERVAL).await;
    }
}

/// One row of the native table: the case, the `CreateSecret` request the OWNING service's own
/// API is called with, and the resource model Cloud Control API must then report for it.
struct Row {
    case: &'static str,
    create: fn() -> Value,
    /// The model, less the `Id` every row's model carries — that is the ARN the owning service
    /// assigned, which the case reads off its own answer rather than spelling.
    model: fn() -> Value,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a secret carrying nothing but its name and its value",
            create: || json!({"Name": "orders", "SecretString": "v1"}),
            model: || json!({"Name": "orders"}),
        },
        Row {
            case: "a secret under a hierarchical name, with a description",
            create: || {
                json!({
                    "Name": "prod/db/password",
                    "SecretString": "{\"username\":\"app\",\"password\":\"p\"}",
                    "Description": "the application's database credentials",
                })
            },
            model: || {
                json!({
                    "Name": "prod/db/password",
                    "Description": "the application's database credentials",
                })
            },
        },
        Row {
            case: "a secret under a name carrying every character a secret name admits, tagged",
            create: || {
                json!({
                    "Name": "orders_2026+eu-west.1@a-b/c=d",
                    "SecretString": "v1",
                    "Tags": [{"Key": "team", "Value": "ops"}, {"Key": "env", "Value": "prod"}],
                })
            },
            model: || {
                json!({
                    "Name": "orders_2026+eu-west.1@a-b/c=d",
                    "Tags": [{"Key": "env", "Value": "prod"}, {"Key": "team", "Value": "ops"}],
                })
            },
        },
        Row {
            case: "a secret holding a binary value, which no model carries either",
            create: || {
                json!({
                    "Name": "binary-held",
                    "SecretBinary": "AAEC",
                    "Description": "a value the type's schema states no property for",
                })
            },
            model: || {
                json!({
                    "Name": "binary-held",
                    "Description": "a value the type's schema states no property for",
                })
            },
        },
    ]
}

#[tokio::test]
async fn a_secret_the_owning_service_holds_resolves_through_this_api() {
    let emulator = Emulator::with_secretsmanager();
    let rows = rows();

    // Created through the OWNING service's own API: each of these secrets exists because
    // Secrets Manager was asked for it, not because Cloud Control API was.
    let mut arns = Vec::new();
    for (index, row) in rows.iter().enumerate() {
        let mut request = (row.create)();
        // "If you generate a raw HTTP request to the Secrets Manager service endpoint, then
        // you must generate a ClientRequestToken and include it in the request"
        // (`CreateSecretRequest$ClientRequestToken`): this harness IS such a request, and the
        // member's modelled length is 32..64.
        if let Some(members) = request.as_object_mut() {
            members.insert(
                "ClientRequestToken".to_string(),
                json!(format!("cc-read-through-token-{index:0>10}")),
            );
        }
        let created = emulator.secretsmanager_ok("CreateSecret", request).await;
        let arn = created
            .get("ARN")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        assert!(
            arn.starts_with(&format!(
                "arn:aws:secretsmanager:{SIGNING_REGION}:000000000000:secret:"
            )),
            "case {:?}: expected the ARN the owning service assigned, got {arn:?}",
            row.case
        );
        arns.push(arn);
    }

    for (row, arn) in rows.iter().zip(&arns) {
        let case = row.case;
        let (identifier, model) = resolved(&emulator, case, arn).await;
        assert_eq!(
            identifier, *arn,
            "case {case:?}: expected the resource to answer to the ARN the owning service \
             holds, got {identifier:?}"
        );
        let mut expected = (row.model)();
        expected["Id"] = json!(arn);
        assert_eq!(
            model, expected,
            "case {case:?}: expected the resource model {expected:?}, got {model:?}"
        );
        assert_projection(&model, case, "GetResource");
    }

    // ...and every one of them is in the type's inventory, under that same ARN.
    let listed = inventory(&emulator).await;
    for arn in &arns {
        assert!(
            listed.contains(arn),
            "expected ListResources to report {arn:?}, got {listed:?}"
        );
    }

    // A secret the OWNING service removed stops resolving here, and the refusal is that
    // handler's own verdict about the resource the request named.
    for (row, arn) in rows.iter().zip(&arns) {
        let case = row.case;
        emulator
            .secretsmanager_ok(
                "DeleteSecret",
                json!({ "SecretId": arn, "ForceDeleteWithoutRecovery": true }),
            )
            .await;
        let stated = gone(&emulator, case, arn).await;
        assert_handler_not_found(&stated, SECRET, &generic_not_found(SECRET, arn), case);
    }
}

#[tokio::test]
async fn a_secret_a_cloudformation_stack_provisioned_resolves_through_this_api() {
    let emulator = Emulator::with_secretsmanager_and_cloudformation();
    let case = "a secret a stack declared";
    let name = "stack-declared-secret";
    let template = json!({
        "Resources": {
            "Secret": {
                "Type": SECRET,
                "Properties": {"Name": name, "SecretString": "v1-secret-value"},
            },
        },
    })
    .to_string();
    emulator
        .cfn_ok(
            "CreateStack",
            &[("StackName", "cloudcontrol-secret"), ("TemplateBody", &template)],
        )
        .await;

    // The stack's own API is what says the secret is there: its resource's physical id is the
    // ARN Secrets Manager assigned, and that service serves the value the template declared.
    let deadline = Instant::now() + SETTLED_BUDGET;
    let arn = loop {
        let resources = emulator
            .cfn_ok(
                "ListStackResources",
                &[("StackName", "cloudcontrol-secret")],
            )
            .await;
        let physical = resources
            .split("<PhysicalResourceId>")
            .nth(1)
            .and_then(|tail| tail.split("</PhysicalResourceId>").next())
            .unwrap_or_default()
            .to_string();
        if physical.contains(":secret:") {
            break physical;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the stack to report the secret it created, got {resources}"
        );
        tokio::time::sleep(SETTLED_INTERVAL).await;
    };
    let declared_value = served(&emulator, case, &arn).await;
    assert_eq!(
        declared_value.get("SecretString").and_then(Value::as_str),
        Some("v1-secret-value"),
        "case {case:?}: expected the owning service to serve the declared value, got \
         {declared_value:?}"
    );

    // ...and the same secret resolves through this API, under that ARN, with the readable
    // model and without the write-only value.
    let (identifier, model) = resolved(&emulator, case, &arn).await;
    assert_eq!(
        identifier, arn,
        "case {case:?}: expected the resource to answer to the ARN the stack published, got \
         {identifier:?}"
    );
    assert_eq!(
        model.get("Id").and_then(Value::as_str),
        Some(arn.as_str()),
        "case {case:?}: expected the model's Id to be the secret's ARN, got {model:?}"
    );
    assert_eq!(
        model.get("Name").and_then(Value::as_str),
        Some(name),
        "case {case:?}: expected the model's Name to be the declared name, got {model:?}"
    );
    assert_projection(&model, case, "GetResource");
}

#[tokio::test]
async fn a_secret_this_api_provisions_is_one_the_owning_service_serves() {
    let emulator = Emulator::with_secretsmanager();
    let case = "a secret Cloud Control API provisioned";
    let name = "cloudcontrol-provisioned";
    let identifier = emulator
        .provision(
            SECRET,
            &json!({
                "Name": name,
                "SecretString": "v1",
                "Description": "provisioned through the uniform API",
                "Tags": [{"Key": "team", "Value": "ops"}],
            }),
        )
        .await;
    assert!(
        identifier.starts_with(&format!(
            "arn:aws:secretsmanager:{SIGNING_REGION}:000000000000:secret:{name}-"
        )),
        "case {case:?}: expected the ARN the owning service assigns a secret of this name, got \
         {identifier:?}"
    );

    // The owning service holds it: its own read serves the value this create applied, under
    // the name the desired state stated.
    let after_create = served(&emulator, case, name).await;
    assert_eq!(
        after_create.get("SecretString").and_then(Value::as_str),
        Some("v1"),
        "case {case:?}: expected the owning service to serve the value the create applied, got \
         {after_create:?}"
    );
    assert_eq!(
        after_create.get("ARN").and_then(Value::as_str),
        Some(identifier.as_str()),
        "case {case:?}: expected the owning service to hold the secret under the identifier \
         this API reported, got {after_create:?}"
    );

    // A patch is applied where the resource lives, and the value — the write-only property —
    // may be set by the one operation such a property admits.
    emulator
        .update_and_settle(
            SECRET,
            &identifier,
            &json!([
                {"op": "replace", "path": "/Description", "value": "patched"},
                {"op": "add", "path": "/SecretString", "value": "v2"},
            ]),
        )
        .await;
    let (_, model) = resolved(&emulator, case, &identifier).await;
    assert_eq!(
        model.get("Description").and_then(Value::as_str),
        Some("patched"),
        "case {case:?}: expected the patched description, got {model:?}"
    );
    assert_projection(&model, case, "GetResource");
    let after_patch = served(&emulator, case, &identifier).await;
    assert_eq!(
        after_patch.get("SecretString").and_then(Value::as_str),
        Some("v2"),
        "case {case:?}: expected the owning service to serve the value the patch applied, got \
         {after_patch:?}"
    );

    // ...and a delete removes it THERE: neither API reports it afterwards.
    emulator.delete_and_settle(SECRET, &identifier).await;
    let (status, body) = emulator
        .secretsmanager("GetSecretValue", json!({ "SecretId": &identifier }))
        .await;
    assert_ne!(
        status, 200,
        "case {case:?}: expected the owning service to hold no secret after the delete, got \
         {status} with body {body:?}"
    );
    let stated = gone(&emulator, case, &identifier).await;
    assert_handler_not_found(
        &stated,
        SECRET,
        &generic_not_found(SECRET, &identifier),
        case,
    );
}

#[tokio::test]
async fn a_write_only_property_is_in_no_resource_model_this_service_reports() {
    // The list the service projects a reported model through is the schema's own, so the cases
    // here hold this service to the resource type's schema rather than to a catalogue of its
    // own.
    let catalogued =
        service_com_amazonaws_cloudcontrol_cloudapiservice::state::write_only_properties(SECRET);
    assert_eq!(
        catalogued, WRITE_ONLY,
        "expected the schema's writeOnlyProperties, got {catalogued:?}"
    );

    let emulator = Emulator::with_secretsmanager();
    // The rows differ along which mutating request leaves the model behind, and which
    // write-only property its desired state or patch carries.
    let created = emulator
        .create_ok(
            SECRET,
            &json!({"Name": "write-only-progress", "SecretString": "v1"}),
            None,
        )
        .await;
    let created = emulator.settled(&text(&created, "RequestToken")).await;
    let identifier = text(&created, "Identifier");
    let model: Value = serde_json::from_str(&text(&created, "ResourceModel"))
        .unwrap_or_else(|_| panic!("expected a resource model, got {created:?}"));
    assert_projection(&model, "the create", "ProgressEvent.ResourceModel");

    let patched = emulator
        .update_and_settle(
            SECRET,
            &identifier,
            &json!([{"op": "add", "path": "/SecretString", "value": "v2"}]),
        )
        .await;
    let model: Value = serde_json::from_str(&text(&patched, "ResourceModel"))
        .unwrap_or_else(|_| panic!("expected a resource model, got {patched:?}"));
    assert_projection(&model, "the patch", "ProgressEvent.ResourceModel");

    // What the model leaves out is a value that DID take effect: the owning service serves the
    // value the patch applied.
    let after_patch = served(&emulator, "the patch", &identifier).await;
    assert_eq!(
        after_patch.get("SecretString").and_then(Value::as_str),
        Some("v2"),
        "expected the owning service to serve the value the patch applied, got {after_patch:?}"
    );

    // A patch that REPLACES a write-only property is refused: there is nothing in the model it
    // is applied to to replace, which is why such a property may only be added.
    let (status, body) = emulator
        .update(
            SECRET,
            &identifier,
            &json!([{"op": "replace", "path": "/SecretString", "value": "v3"}]),
            None,
        )
        .await;
    assert_eq!(
        status, 400,
        "expected a replace of a write-only property to be refused, got {status} with body \
         {body:?}"
    );
    assert_eq!(
        message(&body),
        "Invalid patch update: writeOnlyProperties [/SecretString] can only be updated using \
         'add' operation",
        "expected the patch refusal naming the write-only property, got {body:?}"
    );
}

#[tokio::test]
async fn a_model_naming_a_property_this_handler_does_not_apply_is_refused() {
    let emulator = Emulator::with_secretsmanager();
    // The rows differ along WHICH property the desired state names: one the type's schema
    // states that this handler does not apply, and one the schema does not state at all — two
    // different faults, which a caller fixes differently.
    let rows: [(&str, Value, &str); 3] = [
        (
            "the generation parameters the owning service would draw a value from",
            json!({
                "Name": "unapplied-generated",
                "GenerateSecretString": {"PasswordLength": 32},
            }),
            "GenerateSecretString is a property of AWS::SecretsManager::Secret this \
             emulator's handler does not apply",
        ),
        (
            "the Regions a secret would be replicated to",
            json!({
                "Name": "unapplied-replicated",
                "SecretString": "v1",
                "ReplicaRegions": [{"Region": "eu-west-1"}],
            }),
            "ReplicaRegions is a property of AWS::SecretsManager::Secret this emulator's \
             handler does not apply",
        ),
        (
            "a member the type's schema states no property for",
            json!({
                "Name": "unapplied-unknown",
                "SecretString": "v1",
                "ThisPropertyDoesNotExist": 1,
            }),
            "ThisPropertyDoesNotExist is not a property of AWS::SecretsManager::Secret",
        ),
    ];
    for (case, desired, detail) in rows {
        let accepted = emulator.create_ok(SECRET, &desired, None).await;
        let settled = emulator.terminal(&text(&accepted, "RequestToken")).await;
        assert_eq!(
            text(&settled, "OperationStatus"),
            STATUS_FAILED,
            "case {case:?}: expected the request to fail, got {settled:?}"
        );
        assert_eq!(
            text(&settled, "StatusMessage"),
            detail,
            "case {case:?}: expected the handler's own diagnostic, got {settled:?}"
        );
        assert_eq!(
            text(&settled, "ErrorCode"),
            "InvalidRequest",
            "case {case:?}: expected the InvalidRequest handler error code, got {settled:?}"
        );
    }
}
