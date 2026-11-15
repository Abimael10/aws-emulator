// SPDX-License-Identifier: CC0-1.0

//! The not-found refusal of a resource this service DID hold and no longer does.
//!
//! One sentence words every not-found refusal of this service — the resource type, the
//! identifier the request supplied, then a full stop — and it does not depend on HOW the
//! resource came to be absent. The sibling suite
//! [`super::resource_not_found_messages`] spans identifiers that were never valid (a
//! resource never created, one of another Region, one of another partition); the rows
//! here span the other half of the class: identifiers this service MINTED, for resources
//! a delete has since taken away, which is where a handler that wrote the sentence from
//! the record it just failed to find would differ.
//!
//! The rows differ along the resource type (connector, template, directory
//! registration), the operation (a read, a create, a child create) and the way the
//! removal became final: a delete, the tolerant REPEAT of that delete, the cascade that
//! carries a connector's templates with it, and a deferred removal the clock has made
//! due. Every row drives the real wire path through [`super::harness`].

use std::time::{Duration, SystemTime};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use super::connector_lifecycle::create_connector;
use super::directory_registration_authorization::set_deleted_due;
use super::harness::{call, encode_label, register_directory, router_with_engine};
use super::service_principal_name_lifecycle::assert_not_found;

/// Any past instant is due, so the next call applies the removal the clock owed.
const ALREADY_DUE: Duration = Duration::from_secs(1);

/// The body a `CreateConnector` carries: the smallest one the shape accepts
/// (`smithy_query(query="operation",operation="CreateConnector")`).
fn connector_body(directory_id: &str) -> Value {
    json!({
        "DirectoryId": directory_id,
        "CertificateAuthorityArn":
            "arn:aws:acm-pca:us-west-2:000000000000:certificate-authority/\
             11111111-2222-3333-4444-555555555555",
        "VpcInformation": { "SecurityGroupIds": ["sg-0123456789abcdef0"] },
    })
}

/// The smallest `TemplateV2` definition the `TemplateDefinition` union accepts
/// (`smithy_query(query="operation",operation="CreateTemplate")`).
fn template_definition() -> Value {
    json!({ "TemplateV2": {
        "CertificateValidity": {
            "ValidityPeriod": { "PeriodType": "YEARS", "Period": 1 },
            "RenewalPeriod": { "PeriodType": "WEEKS", "Period": 6 },
        },
        "PrivateKeyAttributes": { "MinimalKeyLength": 2048, "KeySpec": "KEY_EXCHANGE" },
        "PrivateKeyFlags": { "ClientVersion": "WINDOWS_SERVER_2016" },
        "EnrollmentFlags": { "NoSecurityExtension": true },
        "SubjectNameFlags": { "SanRequireDirectoryGuid": true },
        "GeneralFlags": { "MachineType": true },
        "Extensions": {
            "KeyUsage": { "UsageFlags": { "KeyEncipherment": true, "DigitalSignature": true } }
        },
    }})
}

/// The body a `CreateTemplate` carries for `connector_arn`.
fn create_template_body(connector_arn: &str, name: &str) -> Value {
    json!({
        "ConnectorArn": connector_arn,
        "Name": name,
        "Definition": template_definition(),
    })
}

/// `CreateTemplate` under `connector_arn`, answering the ARN it minted.
async fn create_template(
    router: &MultiServiceRouter,
    connector_arn: &str,
    name: &str,
) -> String {
    let body = create_template_body(connector_arn, name);
    let (status, code, parsed) = call(router, "POST", "/templates", Some(&body)).await;
    assert_eq!(
        status, 200,
        "setup {name}: CreateTemplate must answer the modelled 200, \
         got {status} / {code:?} / {parsed}"
    );
    parsed["TemplateArn"]
        .as_str()
        .unwrap_or_else(|| panic!("setup {name}: the create answered no TemplateArn: {parsed}"))
        .to_string()
}

/// `DELETE` on `target`, which every delete of this service answers 202 for whether the
/// resource was there or not.
async fn delete(router: &MultiServiceRouter, case: &str, target: &str) {
    let (status, code, parsed) = call(router, "DELETE", target, None).await;
    assert_eq!(
        status, 202,
        "setup {case}: the delete must answer the modelled 202, \
         got {status} / {code:?} / {parsed}"
    );
}

/// One refusal: a request addressing a resource a delete has taken away.
struct DeletedCase {
    /// Why this row is here: the operation and how the resource became absent.
    case: String,
    method: &'static str,
    target: String,
    body: Option<Value>,
    /// The `ResourceType` the refusal names, as the modelled shape spells it.
    resource_type: &'static str,
    /// The identifier the request supplied, which the refusal echoes.
    identifier: String,
}

#[tokio::test]
async fn a_resource_a_delete_took_away_is_refused_in_this_services_one_wording() {
    let (router, engine) = router_with_engine();

    // A registration and a connector this service KEEPS, so each refusal below is a
    // store holding resources answering about one it does not hold.
    let held_registration = register_directory(&router, "d-0123456789").await;
    let held_connector = create_connector(&router, &connector_body("d-0123456789")).await;

    // A connector with a template under it, deleted twice: the second delete is the
    // tolerant repeat, and the template goes with its connector.
    let deleted_connector = create_connector(&router, &connector_body("d-abcdef0123")).await;
    let cascaded_template = create_template(&router, &deleted_connector, "WebServerTemplate").await;
    let connector_target = format!("/connectors/{}", encode_label(&deleted_connector));
    delete(&router, "the first DeleteConnector", &connector_target).await;
    delete(&router, "the repeated DeleteConnector", &connector_target).await;

    // A registration whose delete is deferred, moved past the instant its removal was
    // due: only the due instant is rewritten, so the removal is still the service's.
    let removed_registration = register_directory(&router, "d-00000000ff").await;
    delete(
        &router,
        "DeleteDirectoryRegistration",
        &format!(
            "/directoryRegistrations/{}",
            encode_label(&removed_registration)
        ),
    )
    .await;
    set_deleted_due(
        &engine,
        &removed_registration,
        SystemTime::now() - ALREADY_DUE,
    );

    let rows = vec![
        DeletedCase {
            case: "GetConnector on a connector that was deleted".to_string(),
            method: "GET",
            target: connector_target.clone(),
            body: None,
            resource_type: "Connector",
            identifier: deleted_connector.clone(),
        },
        DeletedCase {
            case: "CreateTemplate naming a connector that was deleted".to_string(),
            method: "POST",
            target: "/templates".to_string(),
            body: Some(create_template_body(&deleted_connector, "SecondTemplate")),
            resource_type: "Connector",
            identifier: deleted_connector.clone(),
        },
        DeletedCase {
            case: "GetTemplate on a template its deleted connector carried".to_string(),
            method: "GET",
            target: format!("/templates/{}", encode_label(&cascaded_template)),
            body: None,
            resource_type: "Template",
            identifier: cascaded_template.clone(),
        },
        DeletedCase {
            case: "CreateServicePrincipalName naming a connector that was deleted".to_string(),
            method: "POST",
            target: format!(
                "/directoryRegistrations/{}/servicePrincipalNames/{}",
                encode_label(&held_registration),
                encode_label(&deleted_connector)
            ),
            body: Some(json!({})),
            resource_type: "Connector",
            identifier: deleted_connector.clone(),
        },
        DeletedCase {
            case: "GetDirectoryRegistration on a registration whose removal came due".to_string(),
            method: "GET",
            target: format!(
                "/directoryRegistrations/{}",
                encode_label(&removed_registration)
            ),
            body: None,
            resource_type: "DirectoryRegistration",
            identifier: removed_registration.clone(),
        },
        DeletedCase {
            case: "GetServicePrincipalName under a registration whose removal came due"
                .to_string(),
            method: "GET",
            target: format!(
                "/directoryRegistrations/{}/servicePrincipalNames/{}",
                encode_label(&removed_registration),
                encode_label(&held_connector)
            ),
            body: None,
            resource_type: "DirectoryRegistration",
            identifier: removed_registration.clone(),
        },
    ];

    for row in rows {
        let answered = call(&router, row.method, &row.target, row.body.as_ref()).await;
        assert_not_found(&row.case, answered, row.resource_type, &row.identifier);
    }

    // The refusals above are about the resources that are gone, not about a store that
    // answers everything that way: what this service still holds still reads back.
    let (status, code, parsed) = call(
        &router,
        "GET",
        &format!("/connectors/{}", encode_label(&held_connector)),
        None,
    )
    .await;
    assert_eq!(
        status, 200,
        "case {held_connector}: a connector no delete named is still read back, \
         got {status} / {code:?} / {parsed}"
    );
    assert_eq!(
        parsed["Connector"]["Arn"].as_str(),
        Some(held_connector.as_str()),
        "case {held_connector}: GetConnector must report the ARN it was addressed by, \
         got {parsed}"
    );
}
