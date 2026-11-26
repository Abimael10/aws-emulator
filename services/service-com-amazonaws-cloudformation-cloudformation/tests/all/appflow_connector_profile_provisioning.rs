// SPDX-License-Identifier: CC0-1.0

//! The Amazon AppFlow CONNECTOR PROFILE a stack declares, end to end — beside the Amazon
//! Redshift Serverless namespace and workgroup it reads through, and the Secrets Manager secret
//! Amazon AppFlow holds its credentials in.
//!
//! RULE: a resource type Amazon Web Services publishes is one a template may DECLARE, and a
//! declaration of it is deployed by calling the service that OWNS the type — so the handle the
//! stack publishes (`Ref`) resolves in that service, the values the type publishes (`Fn::GetAtt`)
//! are the ones that service reported, deleting the stack takes the resource away there, and what
//! that service REFUSES is a failed resource rolled back rather than a template-format error.
//! `Template format error: Unrecognized resource types: [<types>]` is reserved for a type
//! CloudFormation does not know, and no assertion here may ever see it for one of the three types
//! below.
//!
//! The rows of the deploy table differ along what the rule generalises over: the connection mode
//! the profile is created in, whether it names a Data API role at all, the database it reads, and
//! the published value each output reads (`Ref`, `Fn::GetAtt ConnectorProfileArn`,
//! `Fn::GetAtt CredentialsArn`). The rollback leg differs in the one thing that decides it: the
//! trust policy of the Data API role the UPDATE points the profile at.
//!
//! Everything goes over the wire each service speaks through the one router `harness.rs` builds —
//! the CloudFormation calls over its awsQuery wire, the AppFlow calls over its `restJson1` wire,
//! the Redshift Serverless and Secrets Manager calls over their `awsJson1_1` wires — so what is
//! asserted is what a client that deploys the template and then calls those services observes.

use std::sync::Arc;

use serde_json::{json, Value};
use service_com_amazonaws_iam_awsidentitymanagementv20100508::ops::types::Role;
use service_com_amazonaws_iam_awsidentitymanagementv20100508::state::{
    name_key, role_arn, ComAmazonawsIamAwsidentitymanagementv20100508State as IamState,
    DEFAULT_PATH,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::harness::{
    create_stack, member, router_with_engine, settled_stack, settled_stack_status,
    stack_event_reasons, xml_text, REGION,
};
use crate::stack_lifecycle::output_value;

/// The three resource types under test.
const CONNECTOR_PROFILE: &str = "AWS::AppFlow::ConnectorProfile";
const NAMESPACE: &str = "AWS::RedshiftServerless::Namespace";
const WORKGROUP: &str = "AWS::RedshiftServerless::Workgroup";

/// The fixed text CloudFormation refuses a template declaring a type it does NOT know with,
/// which no assertion here may ever see.
const UNRECOGNIZED_TYPES: &str = "Unrecognized resource types";

/// The account this emulator serves, which every ARN it mints carries.
const ACCOUNT: &str = ::engine::EMULATOR_ACCOUNT_ID;

/// The terminal `StackStatus` values this suite waits for.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";
const DELETE_COMPLETE: &str = "DELETE_COMPLETE";
const UPDATE_ROLLBACK_COMPLETE: &str = "UPDATE_ROLLBACK_COMPLETE";

/// The service principal a Data API role's trust policy has to admit for Amazon AppFlow to reach
/// a Redshift database through it.
const APPFLOW_PRINCIPAL: &str = "appflow.amazonaws.com";

/// The status Amazon Redshift Serverless reports a usable workgroup in.
const AVAILABLE: &str = "AVAILABLE";

/// The logical ids the template declares, and the output keys it publishes the profile's three
/// handles under.
const NAMESPACE_ID: &str = "Warehouse";
const WORKGROUP_ID: &str = "WarehouseCompute";
const PROFILE_ID: &str = "WarehouseProfile";
const PROFILE_REF_OUTPUT: &str = "ProfileRef";
const PROFILE_ARN_OUTPUT: &str = "ProfileArn";
const CREDENTIALS_ARN_OUTPUT: &str = "ProfileCredentialsArn";

/// Call one Amazon AppFlow operation over the `restJson1` wire its model declares.
async fn appflow(router: &MultiServiceRouter, uri: &str, body: &Value) -> (u16, Value) {
    json_call(router, "appflow", "application/json", None, uri, body).await
}

/// Call one Amazon Redshift Serverless operation over the `awsJson1_1` wire its model declares.
async fn redshift_serverless(
    router: &MultiServiceRouter,
    operation: &str,
    body: &Value,
) -> (u16, Value) {
    json_call(
        router,
        "redshift-serverless",
        "application/x-amz-json-1.1",
        Some(&format!("RedshiftServerless.{operation}")),
        "/",
        body,
    )
    .await
}

/// Call one Secrets Manager operation over the `awsJson1_1` wire its model declares — the service
/// that HOLDS a connector profile's credentials.
async fn secretsmanager(
    router: &MultiServiceRouter,
    operation: &str,
    body: &Value,
) -> (u16, Value) {
    json_call(
        router,
        "secretsmanager",
        "application/x-amz-json-1.1",
        Some(&format!("secretsmanager.{operation}")),
        "/",
        body,
    )
    .await
}

/// The ONE JSON call the three above are: a POST to `signing_name`'s endpoint in [`REGION`],
/// signed (in the routing sense) with this crate's own harness credential, carrying the
/// `X-Amz-Target` an RPC protocol needs and none for a REST one.
async fn json_call(
    router: &MultiServiceRouter,
    signing_name: &str,
    content_type: &str,
    target: Option<&str>,
    uri: &str,
    body: &Value,
) -> (u16, Value) {
    let mut request = http::Request::builder()
        .method("POST")
        .uri(format!(
            "https://{signing_name}.{REGION}.amazonaws.com{uri}"
        ))
        .header("host", format!("{signing_name}.{REGION}.amazonaws.com"))
        .header("content-type", content_type)
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{signing_name}/\
                 aws4_request, SignedHeaders=host, Signature=0123456789abcdef"
            ),
        );
    if let Some(target) = target {
        request = request.header("x-amz-target", target);
    }
    let request = request
        .body(bytes::Bytes::from(body.to_string()))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed: Value = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// The profile Amazon AppFlow holds under `name`, or `None` when it holds none — that service's
/// own read, which is what says whether the deployment really created anything.
async fn held_profile(router: &MultiServiceRouter, name: &str) -> Option<Value> {
    let (status, body) = appflow(
        router,
        "/describe-connector-profiles",
        &json!({ "connectorProfileNames": [name] }),
    )
    .await;
    assert_eq!(
        status, 200,
        "DescribeConnectorProfiles {name:?}: expected HTTP 200, got {status} with {body}"
    );
    body.get("connectorProfileDetails")
        .and_then(Value::as_array)
        .and_then(|details| details.first())
        .cloned()
}

/// Seed `role_name` into the account's IAM carrying `trust` as its `AssumeRolePolicyDocument` —
/// the record `CreateRole` writes — and answer the ARN IAM holds it under.
///
/// Seeded rather than declared in the template because what this suite is about is the AppFlow
/// resource type, and a Data API role is routinely provisioned out of band: the role's trust
/// policy is the one input that decides the rollback leg, so it is set directly.
fn seed_role(engine: &Arc<engine::InMemoryEngine>, role_name: &str, trust: Option<Value>) -> String {
    let state = engine.state::<IamState>();
    let mut store = state.lock();
    let arn = role_arn(DEFAULT_PATH, role_name);
    store.roles.insert(
        name_key(role_name),
        Role {
            path: DEFAULT_PATH.to_string(),
            role_name: role_name.to_string(),
            role_id: format!("AROA{}", role_name.to_uppercase()),
            arn: arn.clone(),
            create_date: ::aws_smithy_types::DateTime::from_secs(1_700_000_000),
            assume_role_policy_document: trust.map(|trust| trust.to_string()),
            description: None,
            max_session_duration: None,
            permissions_boundary: None,
            tags: None,
            role_last_used: None,
            source_role_template: None,
        },
    );
    arn
}

/// A trust document with one statement: `Allow` for the service principal `principal` performing
/// `sts:AssumeRole`.
fn trusting(principal: &str) -> Value {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "Service": principal },
            "Action": "sts:AssumeRole",
        }],
    })
}

/// One row of [`a_declared_connector_profile_is_created_in_appflow`]: the connection mode the
/// profile is declared in, the database it reads, and whether it names a Data API role at all.
struct DeployRow {
    case: &'static str,
    stack: &'static str,
    namespace: &'static str,
    workgroup: &'static str,
    profile: &'static str,
    connection_mode: &'static str,
    database: &'static str,
    names_a_data_api_role: bool,
}

/// The template a row deploys: the Redshift Serverless namespace, the workgroup that serves it and
/// the AppFlow connector profile that reads through that workgroup, with the profile's three
/// published handles as outputs.
fn template(row: &DeployRow, s3_role: &str, data_api_role: &str) -> String {
    let mut redshift = json!({
        "BucketName": "appflow-redshift-staging",
        "RoleArn": s3_role,
        "IsRedshiftServerless": true,
        "WorkgroupName": { "Ref": WORKGROUP_ID },
        "DatabaseName": row.database,
    });
    if row.names_a_data_api_role {
        redshift["DataApiRoleArn"] = json!(data_api_role);
    }
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": {
            NAMESPACE_ID: {
                "Type": NAMESPACE,
                "Properties": {
                    "NamespaceName": row.namespace,
                    "DbName": row.database,
                    "AdminUsername": "warehouse_admin",
                },
            },
            WORKGROUP_ID: {
                "Type": WORKGROUP,
                "Properties": {
                    "WorkgroupName": row.workgroup,
                    "NamespaceName": { "Ref": NAMESPACE_ID },
                    "BaseCapacity": 8,
                    "PubliclyAccessible": false,
                },
            },
            PROFILE_ID: {
                "Type": CONNECTOR_PROFILE,
                "Properties": {
                    "ConnectorProfileName": row.profile,
                    "ConnectorType": "Redshift",
                    "ConnectionMode": row.connection_mode,
                    "ConnectorProfileConfig": {
                        "ConnectorProfileProperties": { "Redshift": redshift },
                        "ConnectorProfileCredentials": {
                            "Redshift": {
                                "Username": "warehouse_admin",
                                "Password": "not-a-real-password",
                            },
                        },
                    },
                },
            },
        },
        "Outputs": {
            PROFILE_REF_OUTPUT: { "Value": { "Ref": PROFILE_ID } },
            PROFILE_ARN_OUTPUT: {
                "Value": { "Fn::GetAtt": [PROFILE_ID, "ConnectorProfileArn"] },
            },
            CREDENTIALS_ARN_OUTPUT: {
                "Value": { "Fn::GetAtt": [PROFILE_ID, "CredentialsArn"] },
            },
        },
    })
    .to_string()
}

/// A declared `AWS::AppFlow::ConnectorProfile` is CREATED in Amazon AppFlow itself, under the name
/// the stack published and beside the Redshift Serverless estate it reads through, with its
/// credentials in a secret Secrets Manager holds — and deleting the stack takes both away there.
#[tokio::test]
async fn a_declared_connector_profile_is_created_in_appflow() {
    let rows = [
        DeployRow {
            case: "a public profile naming a Data API role",
            stack: "appflow-warehouse-public",
            namespace: "appflow-ns-public",
            workgroup: "appflow-wg-public",
            profile: "warehouse-public",
            connection_mode: "Public",
            database: "dev",
            names_a_data_api_role: true,
        },
        DeployRow {
            case: "a private profile naming a Data API role",
            stack: "appflow-warehouse-private",
            namespace: "appflow-ns-private",
            workgroup: "appflow-wg-private",
            profile: "warehouse-private",
            connection_mode: "Private",
            database: "reporting",
            names_a_data_api_role: true,
        },
        DeployRow {
            case: "a profile naming no Data API role",
            stack: "appflow-warehouse-plain",
            namespace: "appflow-ns-plain",
            workgroup: "appflow-wg-plain",
            profile: "warehouse-plain",
            connection_mode: "Public",
            database: "staging",
            names_a_data_api_role: false,
        },
    ];

    // ONE emulator for every row: assembling the service set is the costly part of a deployment
    // test, and the rows are kept apart by naming every resource they declare distinctly.
    let (router, engine) = router_with_engine();
    let s3_role = seed_role(&engine, "redshift-s3-access", None);
    let data_api_role = seed_role(&engine, "appflow-data-api", Some(trusting(APPFLOW_PRINCIPAL)));

    for row in &rows {
        // BOXED: the per-row deployment's own future is large (a whole stack lifecycle over the
        // assembled emulator), so it is built on the heap rather than inside this test's future.
        Box::pin(deployed_and_torn_down(&router, row, &s3_role, &data_api_role)).await;
    }
}

/// Deploy `row`'s stack on `router`, assert everything the rule says about it, and tear it down
/// again — one row of [`a_declared_connector_profile_is_created_in_appflow`]'s table.
async fn deployed_and_torn_down(
    router: &MultiServiceRouter,
    row: &DeployRow,
    s3_role: &str,
    data_api_role: &str,
) {
    let (s3_role, data_api_role) = (s3_role.to_string(), data_api_role.to_string());
    let case = row.case;
    let (status, created) = create_stack(
        router,
        row.stack,
        &template(row, &s3_role, &data_api_role),
        &[],
    )
    .await;
    assert!(
        !created.contains(UNRECOGNIZED_TYPES),
        "case {case:?}: {CONNECTOR_PROFILE} is a type CloudFormation publishes, so the \
         template-format refusal is never the answer; got {created}"
    );
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CreateStack, got {status} with {created}"
    );

    let described = settled_stack(case, router, REGION, row.stack).await;
    assert_eq!(
        xml_text(&described, "StackStatus").unwrap_or_default(),
        CREATE_COMPLETE,
        "case {case:?}: expected the stack to reach {CREATE_COMPLETE}, got {described}; the \
         events: {:?}",
        stack_event_reasons(router, REGION, row.stack).await
    );
    // The id the create published, which is what still addresses the stack once a completed
    // DELETE has taken its NAME out of the account's live stacks.
    let stack_id = xml_text(&created, "StackId").unwrap_or_default();

    // The three handles the type publishes, read where a client reads them.
    assert_eq!(
        output_value(&described, PROFILE_REF_OUTPUT).as_deref(),
        Some(row.profile),
        "case {case:?}: expected Ref to publish the profile's name, got {described}"
    );
    assert_eq!(
        output_value(&described, PROFILE_ARN_OUTPUT).as_deref(),
        Some(
            format!(
                "arn:aws:appflow:{REGION}:{ACCOUNT}:connectorprofile/{}",
                row.profile
            )
            .as_str()
        ),
        "case {case:?}: expected the ARN AppFlow minted for the profile, got {described}"
    );
    // The handle the type publishes for the profile's credentials RESOLVES in the service that
    // holds them: Secrets Manager's own read of that ARN answers the secret AppFlow created
    // there, carrying the credentials the declaration stated.
    let credentials_arn = output_value(&described, CREDENTIALS_ARN_OUTPUT).unwrap_or_default();
    let (secret_status, secret) = secretsmanager(
        router,
        "GetSecretValue",
        &json!({ "SecretId": credentials_arn }),
    )
    .await;
    assert_eq!(
        (secret_status, secret.get("ARN").and_then(Value::as_str)),
        (200, Some(credentials_arn.as_str())),
        "case {case:?}: expected the published credentials handle to resolve in Secrets Manager, \
         got {secret}"
    );
    assert_eq!(
        secret
            .get("SecretString")
            .and_then(Value::as_str)
            .and_then(|held| serde_json::from_str::<Value>(held).ok())
            .as_ref()
            .and_then(|held| held.pointer("/Redshift/username"))
            .and_then(Value::as_str),
        Some("warehouse_admin"),
        "case {case:?}: expected the declared credentials in the secret, got {secret}"
    );

    // The profile EXISTS in the service that owns it, carrying what the declaration said.
    let profile = held_profile(router, row.profile)
        .await
        .unwrap_or_else(|| panic!("case {case:?}: AppFlow holds no profile {:?}", row.profile));
    assert_eq!(
        (
            profile.get("connectorType").and_then(Value::as_str),
            profile.get("connectionMode").and_then(Value::as_str),
            profile
                .pointer("/connectorProfileProperties/Redshift/databaseName")
                .and_then(Value::as_str),
            profile
                .pointer("/connectorProfileProperties/Redshift/workgroupName")
                .and_then(Value::as_str),
            profile
                .pointer("/connectorProfileProperties/Redshift/dataApiRoleArn")
                .and_then(Value::as_str),
        ),
        (
            Some("Redshift"),
            Some(row.connection_mode),
            Some(row.database),
            Some(row.workgroup),
            row.names_a_data_api_role.then_some(data_api_role.as_str()),
        ),
        "case {case:?}: expected the declared configuration in AppFlow, got {profile:?}"
    );

    // And so does the estate it reads through: a profile whose workgroup nothing created would be
    // a stack reporting a resource that does not exist.
    let (workgroup_status, workgroup) = redshift_serverless(
        router,
        "GetWorkgroup",
        &json!({ "workgroupName": row.workgroup }),
    )
    .await;
    assert_eq!(
        (
            workgroup_status,
            workgroup.pointer("/workgroup/status").and_then(Value::as_str)
        ),
        (200, Some(AVAILABLE)),
        "case {case:?}: expected the stack's workgroup up in Redshift Serverless, got {workgroup}"
    );

    // Deleting the stack takes the profile away in the service that holds it.
    let (deleted, body) = crate::harness::call(
        router,
        "DeleteStack",
        &[member("StackName", row.stack)],
    )
    .await;
    assert_eq!(
        deleted, 200,
        "case {case:?}: expected HTTP 200 from DeleteStack, got {deleted} with {body}"
    );
    assert_eq!(
        settled_stack_status(case, router, REGION, &stack_id).await,
        DELETE_COMPLETE,
        "case {case:?}: expected the stack to reach {DELETE_COMPLETE}"
    );
    assert_eq!(
        held_profile(router, row.profile).await,
        None,
        "case {case:?}: expected AppFlow to hold no profile once the stack is gone"
    );
    // And the credentials go with it: the secret the stack published a handle for is gone in the
    // service that held it.
    let (gone, answered) = secretsmanager(
        router,
        "GetSecretValue",
        &json!({ "SecretId": credentials_arn }),
    )
    .await;
    assert_ne!(
        gone, 200,
        "case {case:?}: expected Secrets Manager to hold no secret {credentials_arn:?} once the \
         stack is gone, got {answered}"
    );
}

/// An update that points the connector profile at a Data API role Amazon AppFlow cannot use is
/// that service's own refusal, reported on the resource that failed and rolled back — never a
/// template-format error, and never a success.
#[tokio::test]
async fn an_update_onto_an_unusable_data_api_role_rolls_the_stack_back() {
    let (router, engine) = router_with_engine();
    let s3_role = seed_role(&engine, "redshift-s3-access", None);
    let usable = seed_role(&engine, "appflow-data-api", Some(trusting(APPFLOW_PRINCIPAL)));
    // A role AppFlow cannot take on: its trust policy admits Amazon Redshift and nobody else, so
    // AppFlow could never reach the database through it.
    let unusable = seed_role(
        &engine,
        "redshift-only-data-api",
        Some(trusting("redshift.amazonaws.com")),
    );
    // BOXED for the reason the deploy table's row is: the leg's own future is large, so it is
    // built on the heap rather than inside this test's future.
    Box::pin(rolled_back(&router, &s3_role, &usable, &unusable)).await;
}

/// The rollback leg itself: deploy the profile on a role AppFlow can take on, update it onto one
/// it cannot, and assert what the deployment reports.
async fn rolled_back(router: &MultiServiceRouter, s3_role: &str, usable: &str, unusable: &str) {
    let (s3_role, usable, unusable) = (
        s3_role.to_string(),
        usable.to_string(),
        unusable.to_string(),
    );
    let row = DeployRow {
        case: "the rollback leg",
        stack: "appflow-warehouse-rollback",
        namespace: "appflow-ns-rollback",
        workgroup: "appflow-wg-rollback",
        profile: "warehouse-rollback",
        connection_mode: "Public",
        database: "dev",
        names_a_data_api_role: true,
    };
    let (status, created) =
        create_stack(router, row.stack, &template(&row, &s3_role, &usable), &[]).await;
    assert_eq!(
        status, 200,
        "setup: expected HTTP 200 from CreateStack, got {status} with {created}"
    );
    assert_eq!(
        settled_stack_status("setup", router, REGION, row.stack).await,
        CREATE_COMPLETE,
        "setup: expected the stack to reach {CREATE_COMPLETE}"
    );

    let (status, updated) = crate::harness::call(
        router,
        "UpdateStack",
        &[
            member("StackName", row.stack),
            member("TemplateBody", &template(&row, &s3_role, &unusable)),
        ],
    )
    .await;
    assert!(
        !updated.contains(UNRECOGNIZED_TYPES),
        "case rolled back: the type is one CloudFormation publishes, so the template-format \
         refusal is never the answer; got {updated}"
    );
    assert_eq!(
        status, 200,
        "case rolled back: expected HTTP 200 from UpdateStack, got {status} with {updated}"
    );

    let settled = settled_stack_status("rolled back", router, REGION, row.stack).await;
    let reasons = stack_event_reasons(router, REGION, row.stack).await;
    assert_eq!(
        settled, UPDATE_ROLLBACK_COMPLETE,
        "case rolled back: expected the stack to reach {UPDATE_ROLLBACK_COMPLETE}; the events: \
         {reasons:?}"
    );
    assert!(
        reasons.iter().any(|reason| {
            reason.contains("Update Connector Profile request failed")
                && reason.contains(&unusable)
                && reason.contains(APPFLOW_PRINCIPAL)
        }),
        "case rolled back: expected AppFlow's own refusal naming the role and the principal as \
         the resource's failure reason, got {reasons:?}"
    );
    assert!(
        !reasons
            .iter()
            .any(|reason| reason.contains(UNRECOGNIZED_TYPES)),
        "case rolled back: the template-format refusal is never a resource's failure reason, got \
         {reasons:?}"
    );

    // The rollback put the profile back on the role it could use, so the stack's resource is the
    // one the last successful deployment described.
    let profile = held_profile(router, row.profile)
        .await
        .expect("the profile survives the rolled-back update");
    assert_eq!(
        profile
            .pointer("/connectorProfileProperties/Redshift/dataApiRoleArn")
            .and_then(Value::as_str),
        Some(usable.as_str()),
        "case rolled back: expected the profile back on the role the stack last deployed, got \
         {profile:?}"
    );
}
