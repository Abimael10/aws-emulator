// SPDX-License-Identifier: CC0-1.0

//! Amazon AppFlow CONNECTOR PROFILES, end to end over HTTP: `CreateConnectorProfile`,
//! `DescribeConnectorProfiles`, `UpdateConnectorProfile` and `DeleteConnectorProfile`.
//!
//! Two rules are pinned here.
//!
//! 1. A profile is an account-scoped, NAMED record: the create stores it under the
//!    `connectorProfileName` the request states, a read reports what was stored (the ARN AppFlow
//!    minted, the handle it holds the credentials under, the connector-specific PROPERTIES and
//!    the two instants), the update re-configures the record addressed by that name, and the
//!    delete takes it away — so every value a read answers comes from the store and not from the
//!    call that read it.
//!
//! 2. A profile AppFlow stores is one it could USE: every IAM role the configuration names that
//!    AppFlow itself has to assume — an Amazon Redshift profile's `dataApiRoleArn`, "an IAM role
//!    that permits Amazon AppFlow to access your Amazon Redshift database through the Data API" —
//!    must be one this account's IAM shows AppFlow can assume. A profile naming a role IAM holds
//!    nothing under, or one whose trust policy does not admit the AppFlow service principal, is
//!    refused 400 `ValidationException` when it is defined, by the create and by the update
//!    alike, and nothing is stored or changed. A value that names no role of this account at all
//!    establishes nothing, because another account's IAM is not this emulator's to ask.
//!
//! Every call goes over the wire through the same `MultiServiceRouter` the production router
//! assembles, with the emulator's own IAM mounted BESIDE AppFlow on one engine — what the role
//! question is really put to.

use std::sync::Arc;

use serde_json::{json, Value};
use service_com_amazonaws_appflow_sandstoneconfigurationservicelambda::EmulatedService;
use service_com_amazonaws_iam_awsidentitymanagementv20100508::ops::types::Role;
use service_com_amazonaws_iam_awsidentitymanagementv20100508::state::{
    name_key, role_arn, ComAmazonawsIamAwsidentitymanagementv20100508State as IamState,
    DEFAULT_PATH,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::flow_lifecycle::{
    appflow_with_engine, call, on_demand_trigger, s3_destination, salesforce_source, REGION,
};

/// The service principal a Data API role's trust policy has to admit for AppFlow to reach the
/// database through it, written out here rather than read from the crate's own constant so this
/// suite pins it.
const APPFLOW_PRINCIPAL: &str = "appflow.amazonaws.com";

/// The URIs of the four operations under test, as this service's model binds them.
const CREATE: &str = "/create-connector-profile";
const UPDATE: &str = "/update-connector-profile";
const DESCRIBE: &str = "/describe-connector-profiles";
const DELETE: &str = "/delete-connector-profile";

/// This service with the emulator's own IAM and Secrets Manager mounted BESIDE it on the same
/// engine — what `core/router` assembles, narrowed to the services a connector profile reaches:
/// IAM answers the Data API role question, and Secrets Manager HOLDS the profile's credentials.
fn appflow_with_iam() -> (MultiServiceRouter, Arc<engine::InMemoryEngine>) {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    // The signature itself is not what these cases are about; the credential scope still
    // routes the request and names the Region the ARNs are built in.
    router.set_request_authentication(false);
    router.register_service(EmulatedService::new(Arc::clone(&engine)));
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(
        service_com_amazonaws_secretsmanager_secretsmanager::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    (router, engine)
}

/// What Secrets Manager's OWN read answers for the secret `secret_id` names: its `ARN`, its
/// `Name` and the `SecretString` it holds, or `None` when that service holds no such secret.
///
/// The handle a connector profile publishes as `credentialsArn` has to resolve where a client
/// would look it up, so it is read through that service's own `GetSecretValue` rather than
/// compared against text this suite formats.
async fn held_secret(router: &MultiServiceRouter, secret_id: &str) -> Option<Value> {
    let request = http::Request::builder()
        .method("POST")
        .uri(format!("https://secretsmanager.{REGION}.amazonaws.com/"))
        .header("host", format!("secretsmanager.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", "secretsmanager.GetSecretValue")
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDAPPFLOWFIXTURE/20260101/{REGION}/\
                 secretsmanager/aws4_request, SignedHeaders=host, Signature=unverified"
            ),
        )
        .body(bytes::Bytes::from(
            json!({ "SecretId": secret_id }).to_string(),
        ))
        .expect("a well-formed Secrets Manager request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let body: Value = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    (status == 200).then_some(body)
}

/// Seed `role_name` into the shared engine's IAM store carrying `trust` as its
/// `AssumeRolePolicyDocument` — the record `CreateRole` writes — and answer the ARN IAM holds it
/// under.
fn seed_role(engine: &Arc<engine::InMemoryEngine>, role_name: &str, trust: Option<String>) -> String {
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
            assume_role_policy_document: trust,
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

/// A trust document with one statement: `effect` for the service principals `principals`
/// performing `sts:AssumeRole`.
fn trust(effect: &str, principals: &[&str]) -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": effect,
            "Principal": { "Service": principals },
            "Action": "sts:AssumeRole",
        }],
    })
    .to_string()
}

/// The `connectorProfileConfig` of an Amazon Redshift Serverless profile: the staging bucket and
/// the two roles the type takes, with the database the profile reads.
///
/// `data_api_role_arn` is the member rule 2 is about; `None` is a profile that names no Data API
/// role at all.
fn redshift_config(
    s3_role_arn: &str,
    data_api_role_arn: Option<&str>,
    database_name: &str,
) -> Value {
    let mut redshift = json!({
        "bucketName": "redshift-staging",
        "roleArn": s3_role_arn,
        "isRedshiftServerless": true,
        "workgroupName": "analytics",
        "databaseName": database_name,
    });
    if let Some(data_api_role_arn) = data_api_role_arn {
        redshift["dataApiRoleArn"] = json!(data_api_role_arn);
    }
    json!({
        "connectorProfileProperties": { "Redshift": redshift },
        "connectorProfileCredentials": {
            "Redshift": { "username": "admin", "password": "not-a-real-password" },
        },
    })
}

/// The profiles `DescribeConnectorProfiles` reports for `body`.
async fn described(router: &MultiServiceRouter, body: &Value) -> Vec<Value> {
    let answer = call(router, DESCRIBE, body).await;
    assert!(
        answer.is_ok(),
        "expected DescribeConnectorProfiles {body} to succeed, got {} {:?}",
        answer.status,
        answer.body
    );
    answer
        .get("connectorProfileDetails")
        .as_array()
        .cloned()
        .unwrap_or_default()
}

/// The one profile the account holds under `name`, or `None`.
async fn described_one(router: &MultiServiceRouter, name: &str) -> Option<Value> {
    described(router, &json!({ "connectorProfileNames": [name] }))
        .await
        .into_iter()
        .next()
}

/// A member of a reported profile, as text.
fn text(profile: &Value, member: &str) -> String {
    profile
        .get(member)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// A connector profile round-trips through the account: what the create stored is what a read
/// reports, what the update changed is what the next read reports, and what the delete removed is
/// reported by nothing.
#[tokio::test]
async fn a_connector_profile_round_trips_through_describe() {
    let (router, engine) = appflow_with_iam();
    let s3_role = seed_role(&engine, "redshift-s3-access", None);
    let data_api_role = seed_role(
        &engine,
        "appflow-data-api",
        Some(trust("Allow", &[APPFLOW_PRINCIPAL])),
    );

    let created = call(
        &router,
        CREATE,
        &json!({
            "connectorProfileName": "warehouse",
            "connectorType": "Redshift",
            "connectionMode": "Public",
            "connectorProfileConfig": redshift_config(&s3_role, Some(&data_api_role), "dev"),
        }),
    )
    .await;
    assert!(
        created.is_ok(),
        "expected CreateConnectorProfile to succeed, got {} {:?}",
        created.status,
        created.body
    );
    let arn = created.string("connectorProfileArn").to_string();
    assert_eq!(
        arn,
        format!("arn:aws:appflow:{REGION}:000000000000:connectorprofile/warehouse"),
        "case created: expected the profile's own ARN, got {arn:?}"
    );

    let stored = described_one(&router, "warehouse")
        .await
        .expect("the created profile is reported");
    assert_eq!(
        text(&stored, "connectorProfileArn"),
        arn,
        "case read back: expected the ARN the create answered, got {stored:?}"
    );
    assert_eq!(
        text(&stored, "connectorProfileName"),
        "warehouse",
        "case read back: expected the declared name, got {stored:?}"
    );
    assert_eq!(
        text(&stored, "connectorType"),
        "Redshift",
        "case read back: expected the declared connector type, got {stored:?}"
    );
    assert_eq!(
        text(&stored, "connectionMode"),
        "Public",
        "case read back: expected the declared connection mode, got {stored:?}"
    );
    // The handle the profile publishes RESOLVES in the service that holds the credentials, and
    // the secret it names holds what the create carried.
    let credentials_arn = text(&stored, "credentialsArn");
    let secret = held_secret(&router, &credentials_arn)
        .await
        .unwrap_or_else(|| {
            panic!("case read back: Secrets Manager holds no secret {credentials_arn:?}")
        });
    assert_eq!(
        secret.get("ARN").and_then(Value::as_str),
        Some(credentials_arn.as_str()),
        "case read back: expected the secret the profile's handle names, got {secret:?}"
    );
    assert!(
        secret
            .get("Name")
            .and_then(Value::as_str)
            .is_some_and(|name| name.starts_with("appflow!")),
        "case read back: expected a secret Amazon AppFlow owns, got {secret:?}"
    );
    assert_eq!(
        secret
            .get("SecretString")
            .and_then(Value::as_str)
            .and_then(|held| serde_json::from_str::<Value>(held).ok())
            .as_ref()
            .and_then(|held| held.pointer("/Redshift/username"))
            .and_then(Value::as_str),
        Some("admin"),
        "case read back: expected the credentials the create carried in the secret, got {secret:?}"
    );
    assert_eq!(
        stored
            .pointer("/connectorProfileProperties/Redshift/databaseName")
            .and_then(Value::as_str),
        Some("dev"),
        "case read back: expected the stored Redshift properties, got {stored:?}"
    );
    // The credentials the create carried leave the service in no answer: the modelled
    // `ConnectorProfile` has no member for them at all.
    assert!(
        stored.get("connectorProfileCredentials").is_none(),
        "case read back: expected no credentials in the answer, got {stored:?}"
    );
    let created_at = stored.get("createdAt").cloned().unwrap_or(Value::Null);
    assert!(
        created_at.is_number(),
        "case read back: expected the instant the profile was created, got {stored:?}"
    );

    // The update restates the credentials too, so the secret the profile publishes is re-valued
    // rather than replaced.
    let mut reconfigured_config = redshift_config(&s3_role, Some(&data_api_role), "prod");
    reconfigured_config["connectorProfileCredentials"]["Redshift"]["username"] = json!("analyst");
    let updated = call(
        &router,
        UPDATE,
        &json!({
            "connectorProfileName": "warehouse",
            "connectionMode": "Private",
            "connectorProfileConfig": reconfigured_config,
        }),
    )
    .await;
    assert!(
        updated.is_ok(),
        "expected UpdateConnectorProfile to succeed, got {} {:?}",
        updated.status,
        updated.body
    );
    let reconfigured = described_one(&router, "warehouse")
        .await
        .expect("the updated profile is reported");
    assert_eq!(
        (
            text(&reconfigured, "connectionMode"),
            reconfigured
                .pointer("/connectorProfileProperties/Redshift/databaseName")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            text(&reconfigured, "connectorProfileArn"),
            reconfigured.get("createdAt").cloned().unwrap_or(Value::Null),
        ),
        (
            "Private".to_string(),
            "prod".to_string(),
            arn.clone(),
            created_at.clone()
        ),
        "case updated: expected the new configuration under the same identity, got \
         {reconfigured:?}"
    );
    // The private-connection state is reported for NO profile: this emulator provisions no AWS
    // PrivateLink connection, so there is no state to report for one.
    assert!(
        reconfigured
            .get("privateConnectionProvisioningState")
            .is_none(),
        "case updated: expected no private-connection state to be reported, got {reconfigured:?}"
    );
    // The handle is the one the create answered, and the secret it names now holds what the
    // update stated.
    assert_eq!(
        text(&reconfigured, "credentialsArn"),
        credentials_arn,
        "case updated: expected the profile to keep its credentials handle, got {reconfigured:?}"
    );
    let re_valued = held_secret(&router, &credentials_arn)
        .await
        .expect("the secret survives the update");
    assert_eq!(
        re_valued
            .get("SecretString")
            .and_then(Value::as_str)
            .and_then(|held| serde_json::from_str::<Value>(held).ok())
            .as_ref()
            .and_then(|held| held.pointer("/Redshift/username"))
            .and_then(Value::as_str),
        Some("analyst"),
        "case updated: expected the secret re-valued with what the update stated, got \
         {re_valued:?}"
    );
    let last_updated = reconfigured
        .get("lastUpdatedAt")
        .and_then(Value::as_f64)
        .unwrap_or_default();
    assert!(
        last_updated >= created_at.as_f64().unwrap_or_default(),
        "case updated: expected the update's instant not to precede the create's, got \
         {reconfigured:?}"
    );

    let deleted = call(
        &router,
        DELETE,
        &json!({ "connectorProfileName": "warehouse" }),
    )
    .await;
    assert!(
        deleted.is_ok(),
        "expected DeleteConnectorProfile to succeed, got {} {:?}",
        deleted.status,
        deleted.body
    );
    assert_eq!(
        described_one(&router, "warehouse").await,
        None,
        "case deleted: expected the account to hold no such profile"
    );
    // The credentials go with the profile: the secret that held them is gone in the service that
    // held it, so nothing is left behind holding credentials nothing can reach.
    assert_eq!(
        held_secret(&router, &credentials_arn).await,
        None,
        "case deleted: expected Secrets Manager to hold no secret {credentials_arn:?}"
    );
    let missing = call(
        &router,
        UPDATE,
        &json!({
            "connectorProfileName": "warehouse",
            "connectionMode": "Public",
            "connectorProfileConfig": redshift_config(&s3_role, Some(&data_api_role), "dev"),
        }),
    )
    .await;
    assert_eq!(
        (missing.status, missing.error.as_str()),
        (404, "ResourceNotFoundException"),
        "case updated after delete: expected the modelled 404, got {:?}",
        missing.body
    );
}

/// One row of [`a_data_api_role_appflow_cannot_assume_is_refused`]: what the account's IAM holds
/// for the role the profile names, and whether AppFlow may store such a profile.
struct RoleRow {
    case: &'static str,
    /// The trust document the seeded role carries, or `None` for a role carrying none.
    trust: Option<String>,
    /// `false` for a row whose profile names a role this account's IAM holds NOTHING under.
    seeded: bool,
    /// `false` for a row whose profile names no Data API role at all.
    names_a_role: bool,
    /// An ARN the row supplies ITSELF rather than one IAM holds the seeded role under: a value
    /// naming another account's role, which this emulator's IAM cannot be asked about.
    foreign_arn: Option<&'static str>,
    refused: bool,
}

/// Rule 2 over the class: AppFlow refuses a Redshift connector profile whose Data API role this
/// account's IAM shows it cannot assume, and stores every other one — on the create and on the
/// update alike.
///
/// The rows differ along what the rule generalises over: which principals the trust document
/// admits, which effect it states, whether there is a document at all, whether the account holds
/// the role, whose account the ARN names, and whether the profile names a Data API role in the
/// first place.
#[tokio::test]
async fn a_data_api_role_appflow_cannot_assume_is_refused() {
    let rows = [
        RoleRow {
            case: "trusts AppFlow",
            trust: Some(trust("Allow", &[APPFLOW_PRINCIPAL])),
            seeded: true,
            names_a_role: true,
            foreign_arn: None,
            refused: false,
        },
        RoleRow {
            case: "trusts AppFlow among several principals",
            trust: Some(trust(
                "Allow",
                &["lambda.amazonaws.com", APPFLOW_PRINCIPAL, "glue.amazonaws.com"],
            )),
            seeded: true,
            names_a_role: true,
            foreign_arn: None,
            refused: false,
        },
        RoleRow {
            case: "trusts another service only",
            trust: Some(trust("Allow", &["redshift.amazonaws.com"])),
            seeded: true,
            names_a_role: true,
            foreign_arn: None,
            refused: true,
        },
        RoleRow {
            case: "denies AppFlow",
            trust: Some(trust("Deny", &[APPFLOW_PRINCIPAL])),
            seeded: true,
            names_a_role: true,
            foreign_arn: None,
            refused: true,
        },
        RoleRow {
            case: "trusts nobody at all",
            trust: Some(
                json!({ "Version": "2012-10-17", "Statement": [] }).to_string(),
            ),
            seeded: true,
            names_a_role: true,
            foreign_arn: None,
            refused: true,
        },
        RoleRow {
            case: "carries no trust document",
            trust: None,
            seeded: true,
            names_a_role: true,
            foreign_arn: None,
            refused: true,
        },
        RoleRow {
            case: "a role this account's IAM holds nothing under",
            trust: None,
            seeded: false,
            names_a_role: true,
            foreign_arn: None,
            refused: true,
        },
        RoleRow {
            case: "a role of ANOTHER account",
            trust: None,
            seeded: false,
            names_a_role: true,
            foreign_arn: Some("arn:aws:iam::210987654321:role/another-accounts-data-api"),
            refused: false,
        },
        RoleRow {
            case: "no Data API role at all",
            trust: None,
            seeded: false,
            names_a_role: false,
            foreign_arn: None,
            refused: false,
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        let (router, engine) = appflow_with_iam();
        let s3_role = seed_role(&engine, "redshift-s3-access", None);
        let data_api_role = match (row.foreign_arn, row.seeded) {
            (Some(arn), _) => arn.to_string(),
            (None, true) => seed_role(&engine, "data-api", row.trust.clone()),
            (None, false) => role_arn(DEFAULT_PATH, "data-api"),
        };
        let config = redshift_config(
            &s3_role,
            row.names_a_role.then_some(data_api_role.as_str()),
            "dev",
        );
        let name = format!("profile-{index}");
        let created = call(
            &router,
            CREATE,
            &json!({
                "connectorProfileName": name,
                "connectorType": "Redshift",
                "connectionMode": "Public",
                "connectorProfileConfig": config,
            }),
        )
        .await;
        let case = row.case;
        if row.refused {
            assert_eq!(
                (created.status, created.error.as_str()),
                (400, "ValidationException"),
                "case {case:?}: expected the create refused 400 ValidationException, got {:?}",
                created.body
            );
            let message = created.string("message").to_string();
            assert!(
                message.contains("Create Connector Profile request failed")
                    && message.contains(&data_api_role)
                    && message.contains(APPFLOW_PRINCIPAL),
                "case {case:?}: expected the refusal to name the call, the role and the \
                 principal, got {message:?}"
            );
            assert_eq!(
                described_one(&router, &name).await,
                None,
                "case {case:?}: expected the refused create to store nothing"
            );
            continue;
        }
        assert!(
            created.is_ok(),
            "case {case:?}: expected the create to succeed, got {} {:?}",
            created.status,
            created.body
        );
        assert!(
            described_one(&router, &name).await.is_some(),
            "case {case:?}: expected the stored profile to be reported"
        );
    }
}

/// The update is held to rule 2 by the SAME pre-flight the create is: a profile the account
/// already holds cannot be pointed at a Data API role AppFlow could not assume, and the refused
/// update changes nothing.
#[tokio::test]
async fn an_update_onto_an_unusable_data_api_role_changes_nothing() {
    let (router, engine) = appflow_with_iam();
    let s3_role = seed_role(&engine, "redshift-s3-access", None);
    let good = seed_role(
        &engine,
        "appflow-data-api",
        Some(trust("Allow", &[APPFLOW_PRINCIPAL])),
    );
    let bad = seed_role(
        &engine,
        "redshift-only-data-api",
        Some(trust("Allow", &["redshift.amazonaws.com"])),
    );
    let created = call(
        &router,
        CREATE,
        &json!({
            "connectorProfileName": "warehouse",
            "connectorType": "Redshift",
            "connectionMode": "Public",
            "connectorProfileConfig": redshift_config(&s3_role, Some(&good), "dev"),
        }),
    )
    .await;
    assert!(
        created.is_ok(),
        "expected the first create to succeed, got {} {:?}",
        created.status,
        created.body
    );

    let refused = call(
        &router,
        UPDATE,
        &json!({
            "connectorProfileName": "warehouse",
            "connectionMode": "Private",
            "connectorProfileConfig": redshift_config(&s3_role, Some(&bad), "prod"),
        }),
    )
    .await;
    assert_eq!(
        (refused.status, refused.error.as_str()),
        (400, "ValidationException"),
        "case refused update: expected 400 ValidationException, got {:?}",
        refused.body
    );
    let message = refused.string("message").to_string();
    assert!(
        message.contains("Update Connector Profile request failed") && message.contains(&bad),
        "case refused update: expected the refusal to name the call and the role, got \
         {message:?}"
    );
    let held = described_one(&router, "warehouse")
        .await
        .expect("the profile is still held");
    assert_eq!(
        (
            text(&held, "connectionMode"),
            held.pointer("/connectorProfileProperties/Redshift/dataApiRoleArn")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            held.pointer("/connectorProfileProperties/Redshift/databaseName")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
        ),
        ("Public".to_string(), good.clone(), "dev".to_string()),
        "case refused update: expected the stored profile untouched, got {held:?}"
    );
}

/// `DescribeConnectorProfiles` reports the profiles the account holds, filtered by every filter
/// the request states and paged by the cursor it issues.
#[tokio::test]
async fn describe_filters_and_pages_the_accounts_profiles() {
    let (router, engine) = appflow_with_iam();
    let s3_role = seed_role(&engine, "redshift-s3-access", None);
    let data_api_role = seed_role(
        &engine,
        "appflow-data-api",
        Some(trust("Allow", &[APPFLOW_PRINCIPAL])),
    );
    let creates = [
        json!({
            "connectorProfileName": "alpha-redshift",
            "connectorType": "Redshift",
            "connectionMode": "Public",
            "connectorProfileConfig": redshift_config(&s3_role, Some(&data_api_role), "dev"),
        }),
        json!({
            "connectorProfileName": "beta-salesforce",
            "connectorType": "Salesforce",
            "connectionMode": "Public",
            "connectorProfileConfig": {
                "connectorProfileProperties": {
                    "Salesforce": { "instanceUrl": "https://example.my.salesforce.com" },
                },
            },
        }),
        json!({
            "connectorProfileName": "gamma-custom",
            "connectorType": "CustomConnector",
            "connectorLabel": "in-house",
            "connectionMode": "Public",
            "connectorProfileConfig": {
                "connectorProfileProperties": {
                    "CustomConnector": { "profileProperties": { "region": "us-east-1" } },
                },
            },
        }),
    ];
    for body in &creates {
        let created = call(&router, CREATE, body).await;
        assert!(
            created.is_ok(),
            "expected the create of {body} to succeed, got {} {:?}",
            created.status,
            created.body
        );
    }

    let rows: [(&str, Value, Vec<&str>); 5] = [
        (
            "no filter",
            json!({}),
            vec!["alpha-redshift", "beta-salesforce", "gamma-custom"],
        ),
        (
            "one name",
            json!({ "connectorProfileNames": ["beta-salesforce"] }),
            vec!["beta-salesforce"],
        ),
        (
            "two names",
            json!({ "connectorProfileNames": ["gamma-custom", "alpha-redshift"] }),
            vec!["alpha-redshift", "gamma-custom"],
        ),
        (
            "one connector type",
            json!({ "connectorType": "Redshift" }),
            vec!["alpha-redshift"],
        ),
        (
            "one connector label",
            json!({ "connectorLabel": "in-house" }),
            vec!["gamma-custom"],
        ),
    ];
    for (case, body, expected) in rows {
        let reported: Vec<String> = described(&router, &body)
            .await
            .iter()
            .map(|profile| text(profile, "connectorProfileName"))
            .collect();
        assert_eq!(
            reported, expected,
            "case {case:?}: expected the filtered profiles, got {reported:?}"
        );
    }

    // One profile per page: the listing walks the whole account through the cursors it issues,
    // and the last page issues none.
    let mut walked: Vec<String> = Vec::new();
    let mut next_token: Option<String> = None;
    for _ in 0..creates.len() + 1 {
        let mut body = json!({ "maxResults": 1 });
        if let Some(token) = &next_token {
            body["nextToken"] = json!(token);
        }
        let answer = call(&router, DESCRIBE, &body).await;
        assert!(
            answer.is_ok(),
            "case paged: expected the page to succeed, got {} {:?}",
            answer.status,
            answer.body
        );
        let page = answer
            .get("connectorProfileDetails")
            .as_array()
            .cloned()
            .unwrap_or_default();
        assert!(
            page.len() <= 1,
            "case paged: expected at most the page asked for, got {page:?}"
        );
        walked.extend(page.iter().map(|profile| text(profile, "connectorProfileName")));
        next_token = answer
            .get("nextToken")
            .as_str()
            .map(ToString::to_string);
        if next_token.is_none() {
            break;
        }
    }
    assert_eq!(
        (walked.as_slice(), next_token.as_deref()),
        (
            ["alpha-redshift", "beta-salesforce", "gamma-custom"]
                .map(ToString::to_string)
                .as_slice(),
            None
        ),
        "case paged: expected every profile once and no cursor at the end"
    );

    let rejected = call(
        &router,
        DESCRIBE,
        &json!({ "nextToken": "not-a-cursor-this-listing-issued" }),
    )
    .await;
    assert_eq!(
        (rejected.status, rejected.error.as_str()),
        (400, "ValidationException"),
        "case foreign cursor: expected 400 ValidationException, got {:?}",
        rejected.body
    );
}

/// The identity rules of the record: one name per account, and a repeated
/// `@idempotencyToken` answering the first result rather than a second create.
#[tokio::test]
async fn the_name_and_the_client_token_identify_one_profile() {
    let (router, _engine) = appflow_with_iam();
    let body = json!({
        "connectorProfileName": "sales",
        "connectorType": "Salesforce",
        "connectionMode": "Public",
        "clientToken": "token-one",
        "connectorProfileConfig": {
            "connectorProfileProperties": {
                "Salesforce": { "instanceUrl": "https://example.my.salesforce.com" },
            },
        },
    });
    let first = call(&router, CREATE, &body).await;
    assert!(
        first.is_ok(),
        "expected the first create to succeed, got {} {:?}",
        first.status,
        first.body
    );

    // The same request again: the token says this is a retry, so it answers the ARN the first
    // call stored rather than the conflict a second create would raise.
    let retried = call(&router, CREATE, &body).await;
    assert_eq!(
        (
            retried.status,
            retried.string("connectorProfileArn").to_string()
        ),
        (200, first.string("connectorProfileArn").to_string()),
        "case retried: expected the first result, got {:?}",
        retried.body
    );

    // The same token with a DIFFERENT configuration is an error, not a replay.
    let mut diverged = body.clone();
    diverged["connectorProfileConfig"]["connectorProfileProperties"]["Salesforce"]
        ["instanceUrl"] = json!("https://other.my.salesforce.com");
    let refused = call(&router, CREATE, &diverged).await;
    assert_eq!(
        (refused.status, refused.error.as_str()),
        (400, "ValidationException"),
        "case diverged retry: expected 400 ValidationException, got {:?}",
        refused.body
    );

    // A new token for a name the account already holds is the conflict.
    let mut again = body.clone();
    again["clientToken"] = json!("token-two");
    let conflict = call(&router, CREATE, &again).await;
    assert_eq!(
        (conflict.status, conflict.error.as_str()),
        (409, "ConflictException"),
        "case second create: expected 409 ConflictException, got {:?}",
        conflict.body
    );
    assert!(
        conflict.string("message").contains("sales"),
        "case second create: expected the conflict to name the profile, got {:?}",
        conflict.body
    );

    // A profile nothing holds cannot be deleted.
    let missing = call(
        &router,
        DELETE,
        &json!({ "connectorProfileName": "never-created" }),
    )
    .await;
    assert_eq!(
        (missing.status, missing.error.as_str()),
        (404, "ResourceNotFoundException"),
        "case delete of an absent profile: expected the modelled 404, got {:?}",
        missing.body
    );
}

/// A profile a FLOW still names is one whose removal would break that flow, so the unforced delete
/// is refused and names the flow in the way — and `forceDelete` is what takes it anyway.
///
/// Driven over the harness that mounts the object store beside AppFlow and holds the
/// `salesforce-profile` the flow fixtures name, because a flow is stored only when every Amazon S3
/// location it names is one AppFlow could use.
#[tokio::test]
async fn a_profile_a_flow_still_uses_is_deleted_only_on_request() {
    let (router, _engine) = appflow_with_engine();
    let created = call(
        &router,
        "/create-flow",
        &json!({
            "flowName": "contacts-to-s3",
            "triggerConfig": on_demand_trigger(),
            "sourceFlowConfig": salesforce_source("Contact"),
            "destinationFlowConfigList": [s3_destination("contacts")],
            "tasks": [],
        }),
    )
    .await;
    assert!(
        created.is_ok(),
        "setup: expected CreateFlow to succeed, got {} {:?}",
        created.status,
        created.body
    );

    let refused = call(
        &router,
        DELETE,
        &json!({ "connectorProfileName": "salesforce-profile" }),
    )
    .await;
    assert_eq!(
        (refused.status, refused.error.as_str()),
        (409, "ConflictException"),
        "case unforced delete of a profile in use: expected 409 ConflictException, got {:?}",
        refused.body
    );
    let message = refused.string("message").to_string();
    assert!(
        message.contains("salesforce-profile") && message.contains("contacts-to-s3"),
        "case unforced delete of a profile in use: expected the conflict to name the profile and \
         the flow that uses it, got {message:?}"
    );
    assert!(
        described_one(&router, "salesforce-profile").await.is_some(),
        "case unforced delete of a profile in use: expected the refused delete to keep the profile"
    );

    let forced = call(
        &router,
        DELETE,
        &json!({ "connectorProfileName": "salesforce-profile", "forceDelete": true }),
    )
    .await;
    assert!(
        forced.is_ok(),
        "case forced delete: expected the delete to succeed, got {} {:?}",
        forced.status,
        forced.body
    );
    assert_eq!(
        described_one(&router, "salesforce-profile").await,
        None,
        "case forced delete: expected the account to hold no such profile"
    );
}
