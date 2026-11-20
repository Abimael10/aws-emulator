// SPDX-License-Identifier: CC0-1.0

//! Clean Rooms ML's role resolution, across the two services it spans, and the wording of the
//! refusals it answers with.
//!
//! RULE: a Clean Rooms ML create that names an IAM role names the role the service ACTS AS —
//! `CreateTrainingDatasetRequest$roleArn` is "the ARN of the IAM role that Clean Rooms ML can
//! assume to read the data referred to in the dataSource field of each dataset" — so the role
//! is resolved against the ACCOUNT's IAM records before anything is persisted, and a call the
//! role does not let the service make is refused `AccessDeniedException` (HTTP 403):
//!
//! * a request carrying no role at all has no principal whose policies could allow the call,
//!   so it is an authorization failure rather than the 400 a member outside its modelled
//!   constraints produces, and it names the member the role was expected at and the IAM action
//!   the call asks for;
//! * a role the service cannot assume is refused naming that role;
//! * a role it can assume whose permissions do not admit the read of the Glue table the
//!   request points at is refused naming that role and that table.
//!
//! Nothing is created in any of the three, which the listing after each refusal asserts.
//!
//! This suite lives here because the role is resolved against IAM's own records: the roles are
//! created, and their policies written, through the real `CreateRole` / `PutRolePolicy` on the
//! same engine that serves Clean Rooms ML, so what the checks read is what IAM persisted and
//! not a copy.
//!
//! The rows differ along the form of the trust policy (which principal it names, whether the
//! grant is an `Allow`, which action it names), along whether IAM holds the role at all, along
//! the form of the role's permissions policy (which action and which resource it names,
//! whether it is a wildcard, whether an explicit `Deny` stands beside it), along the operation
//! called, and along the database and table the dataset points at.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{element, form_encode};

/// The region every Clean Rooms ML request below is signed for, and the one its ARNs name.
const REGION: &str = "us-west-2";

/// The account every emulated resource belongs to.
const ACCOUNT: &str = engine::EMULATOR_ACCOUNT_ID;

/// The service principal Clean Rooms ML assumes a caller-supplied role as, as the crate
/// states it.
const SERVICE_PRINCIPAL: &str =
    service_com_amazonaws_cleanroomsml_awsstarkcontrolservice::state::role_authorization::SERVICE_PRINCIPAL;

/// The only value `DatasetType` accepts
/// (`smithy_query(query="shape", shape="DatasetType")`).
const DATASET_TYPE: &str = "INTERACTIONS";

/// One `ColumnType` value, from the same query.
const COLUMN_TYPE: &str = "USER_ID";

/// One emulator carrying both services over ONE shared engine, exactly as the production
/// router builds them.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_cleanroomsml_awsstarkcontrolservice::EmulatedService::new(engine),
    );
    router
}

/// One awsQuery call to IAM, answered with the HTTP status and the XML body.
async fn iam(router: &MultiServiceRouter, action: &str, params: &[(&str, &str)]) -> (u16, String) {
    let mut body = format!("Action={action}&Version=2010-05-08");
    for (name, value) in params {
        body.push('&');
        body.push_str(&form_encode(name));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", "iam.amazonaws.com")
        .header("content-type", "application/x-www-form-urlencoded")
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let body = String::from_utf8_lossy(response.body()).to_string();
    (status, body)
}

/// One restJson1 call to Clean Rooms ML, answered with the HTTP status and the JSON body.
async fn cleanroomsml(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (u16, Value) {
    let payload = body.map(|value| value.to_string()).unwrap_or_default();
    let mut builder = http::Request::builder()
        .method(method)
        .uri(uri)
        .header("host", format!("cleanrooms-ml.{REGION}.amazonaws.com"))
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/cleanrooms-ml/\
                 aws4_request, SignedHeaders=host, Signature=abc"
            ),
        );
    if !payload.is_empty() {
        builder = builder.header("content-type", "application/json");
    }
    let request = builder
        .body(Bytes::from(payload))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let bytes = response.into_body();
    let parsed = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// The wire error code of a failure body (`__type`, possibly namespace-qualified).
fn error_code(payload: &Value) -> String {
    payload
        .get("__type")
        .and_then(Value::as_str)
        .unwrap_or("<no __type>")
        .rsplit(['#', ':'])
        .next()
        .unwrap_or("<no __type>")
        .to_string()
}

/// The `message` of a failure body.
fn error_message(payload: &Value) -> String {
    payload
        .get("message")
        .or_else(|| payload.get("Message"))
        .and_then(Value::as_str)
        .unwrap_or("<no message>")
        .to_string()
}

/// A trust policy naming `principals` in one `Allow` of `sts:AssumeRole`.
fn trust_allowing(principals: &[&str]) -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "Service": principals },
            "Action": "sts:AssumeRole"
        }]
    })
    .to_string()
}

/// A trust policy this service can use: one `Allow` naming Clean Rooms ML.
fn assumable_trust() -> String {
    trust_allowing(&[SERVICE_PRINCIPAL])
}

/// An identity policy with one statement of `effect` over `action` on `resource`.
fn identity_policy(effect: &str, action: &str, resource: &str) -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{ "Effect": effect, "Action": action, "Resource": resource }]
    })
    .to_string()
}

/// Create a role in IAM carrying `document` and hand back its ARN.
async fn role_with_trust(router: &MultiServiceRouter, name: &str, document: &str) -> String {
    let (status, created) = iam(
        router,
        "CreateRole",
        &[("RoleName", name), ("AssumeRolePolicyDocument", document)],
    )
    .await;
    assert_eq!(
        status, 200,
        "CreateRole {name:?}: expected the role to be persisted, got {status}: {created}"
    );
    element(&created, "Arn")
}

/// Write one inline policy on the role `name`.
async fn put_role_policy(router: &MultiServiceRouter, name: &str, document: &str) {
    let (status, answer) = iam(
        router,
        "PutRolePolicy",
        &[
            ("RoleName", name),
            ("PolicyName", "cleanroomsml-read"),
            ("PolicyDocument", document),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "PutRolePolicy {name:?}: expected the policy to be persisted, got {status}: {answer}"
    );
}

/// The ARN of the Data Catalog table `table` of `database`, as AWS Glue names it.
///
/// Spelled out here rather than imported, so a wrong ARN in the handler fails this suite.
fn table_arn(database: &str, table: &str) -> String {
    format!("arn:aws:glue:{REGION}:{ACCOUNT}:table/{database}/{table}")
}

/// One `Dataset` pointing at the Glue table `table` of `database`
/// (`CreateTrainingDatasetRequest$trainingData` is bounded `@length(min: 1, max: 1)`).
fn training_data(database: &str, table: &str) -> Value {
    json!([{
        "type": DATASET_TYPE,
        "inputConfig": {
            // `ColumnTypeList` is bounded @length(min: 1, max: 1).
            "schema": [{ "columnName": "user_id", "columnTypes": [COLUMN_TYPE] }],
            "dataSource": {
                "glueDataSource": { "tableName": table, "databaseName": database }
            },
        },
    }])
}

/// A `CreateTrainingDataset` request for `name` pointing at one table, running as `role_arn`.
fn create_dataset_body(name: &str, role_arn: &str, database: &str, table: &str) -> Value {
    json!({
        "name": name,
        "roleArn": role_arn,
        "trainingData": training_data(database, table),
    })
}

/// `POST /training-dataset`.
async fn create_dataset(router: &MultiServiceRouter, body: Value) -> (u16, Value) {
    cleanroomsml(router, "POST", "/training-dataset", Some(body)).await
}

/// `POST /configured-model-algorithms`.
async fn create_algorithm(router: &MultiServiceRouter, body: Value) -> (u16, Value) {
    cleanroomsml(router, "POST", "/configured-model-algorithms", Some(body)).await
}

/// The names `ListTrainingDatasets` reports, in the order it pages them.
async fn listed_dataset_names(router: &MultiServiceRouter) -> Vec<String> {
    let (status, listed) = cleanroomsml(router, "GET", "/training-dataset", None).await;
    assert_eq!(
        status, 200,
        "ListTrainingDatasets: expected HTTP 200, got {status} {listed}"
    );
    listed
        .get("trainingDatasets")
        .and_then(Value::as_array)
        .map(|entries| {
            entries
                .iter()
                .filter_map(|entry| entry.get("name").and_then(Value::as_str))
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// Assert that `payload` is the 403 `AccessDeniedException` carrying `expected`.
fn assert_access_denied(case: &str, status: u16, payload: &Value, expected: &str) {
    assert_eq!(
        status, 403,
        "case {case:?}: expected HTTP 403, got {status} {payload}"
    );
    assert_eq!(
        error_code(payload),
        "AccessDeniedException",
        "case {case:?}: expected the modelled authorization refusal, got {payload}"
    );
    assert_eq!(
        error_message(payload),
        expected,
        "case {case:?}: expected {expected:?}, got {:?}",
        error_message(payload)
    );
}

/// A call that names the role this service acts as, and carries none, is an AUTHORIZATION
/// failure naming the member and the IAM action — not the 400 the framework's required-member
/// walk would answer with. The rows span both operations of the class and the shapes a body
/// omitting the role can take.
#[tokio::test]
async fn a_call_that_names_no_role_is_refused_as_an_authorization_failure() {
    let router = emulator();

    // (case, operation, the body, which create it is)
    let rows: Vec<(&str, &str, Value, bool)> = vec![
        (
            "a training dataset carrying every other required member",
            "CreateTrainingDataset",
            json!({ "name": "no-role-1", "trainingData": training_data("lake", "events") }),
            true,
        ),
        (
            "a training dataset carrying only its name",
            "CreateTrainingDataset",
            json!({ "name": "no-role-2" }),
            true,
        ),
        (
            "a training dataset carrying nothing at all",
            "CreateTrainingDataset",
            json!({}),
            true,
        ),
        (
            "a configured model algorithm carrying only its name",
            "CreateConfiguredModelAlgorithm",
            json!({ "name": "no-role-3" }),
            false,
        ),
        (
            "a configured model algorithm carrying a description too",
            "CreateConfiguredModelAlgorithm",
            json!({ "name": "no-role-4", "description": "a pair of containers" }),
            false,
        ),
    ];

    for (case, operation, body, is_dataset) in rows {
        let (status, payload) = if is_dataset {
            create_dataset(&router, body).await
        } else {
            create_algorithm(&router, body).await
        };
        let expected = format!(
            "Role Arn not present in request. Please provide a valid role arn in the path: \
             roleArn. because no resource-based policy allows the cleanrooms-ml:{operation} \
             action"
        );
        assert_access_denied(case, status, &payload, &expected);
    }

    let listed = listed_dataset_names(&router).await;
    assert!(
        listed.is_empty(),
        "a refused create must persist nothing, got {listed:?}"
    );
}

/// A required member the request OMITS is stated with no value token at all, and a member the
/// request carried is still stated with its value. The rows span the required members of both
/// creates, a member nested inside a list element, and a value-carrying refusal as the control.
#[tokio::test]
async fn an_absent_required_member_is_stated_with_no_value() {
    let router = emulator();
    let role_arn = role_with_trust(&router, "validation-role", &assumable_trust()).await;

    // (case, the body, which create it is, the member the clause must name)
    let rows: Vec<(&str, Value, bool, &str)> = vec![
        (
            "a training dataset with no name",
            json!({
                "roleArn": role_arn,
                "trainingData": training_data("lake", "events"),
            }),
            true,
            "name",
        ),
        (
            "a training dataset with no training data",
            json!({ "name": "absent-member-1", "roleArn": role_arn }),
            true,
            "trainingData",
        ),
        (
            "a configured model algorithm with no name",
            json!({ "roleArn": role_arn }),
            false,
            "name",
        ),
    ];

    for (case, body, is_dataset, member) in rows {
        let (status, payload) = if is_dataset {
            create_dataset(&router, body).await
        } else {
            create_algorithm(&router, body).await
        };
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} {payload}"
        );
        assert_eq!(
            error_code(&payload),
            "ValidationException",
            "case {case:?}: expected the modelled input refusal, got {payload}"
        );
        let expected = format!(
            "1 validation error detected: Value at '{member}' failed to satisfy constraint: \
             Member must not be null"
        );
        assert_eq!(
            error_message(&payload),
            expected,
            "case {case:?}: expected {expected:?}, got {:?}",
            error_message(&payload)
        );
    }

    // A member nested inside a list element is stated the same way: no value token, and the
    // path the walk spells it at.
    let (status, payload) = create_dataset(
        &router,
        json!({
            "name": "absent-member-2",
            "roleArn": role_arn,
            "trainingData": [{ "type": DATASET_TYPE }],
        }),
    )
    .await;
    let message = error_message(&payload);
    assert_eq!(
        status, 400,
        "case \"a dataset element with no input config\": expected HTTP 400, got {status} \
         {payload}"
    );
    assert!(
        message.starts_with("1 validation error detected: Value at '")
            && message.contains("inputConfig")
            && message.ends_with("failed to satisfy constraint: Member must not be null"),
        "case \"a dataset element with no input config\": expected a valueless clause naming \
         inputConfig, got {message:?}"
    );

    // The control: a member the request DID carry keeps its value in the clause, so the token
    // that is dropped belongs to the absence and to nothing else.
    let too_long = "n".repeat(64);
    let (status, payload) = create_dataset(
        &router,
        create_dataset_body(&too_long, &role_arn, "lake", "events"),
    )
    .await;
    let message = error_message(&payload);
    assert_eq!(
        status, 400,
        "case \"a name one character past NameString's maximum\": expected HTTP 400, got \
         {status} {payload}"
    );
    assert!(
        message.contains(&format!("Value '{too_long}' at 'name'")),
        "case \"a name one character past NameString's maximum\": expected the submitted \
         value in the clause, got {message:?}"
    );
}

/// A role Clean Rooms ML cannot assume is refused naming that role, and a role it can assume
/// and that may read the table is accepted. The rows span the trust-policy forms and whether
/// IAM holds the role at all.
#[tokio::test]
async fn a_role_clean_rooms_ml_cannot_assume_is_refused() {
    let router = emulator();

    // (case, the trust document, whether the create is accepted)
    let rows: Vec<(&str, String, bool)> = vec![
        ("clean rooms ml alone", assumable_trust(), true),
        (
            "clean rooms ml among several services",
            trust_allowing(&["glue.amazonaws.com", SERVICE_PRINCIPAL]),
            true,
        ),
        (
            "another service only",
            trust_allowing(&["glue.amazonaws.com"]),
            false,
        ),
        (
            "clean rooms ml denied",
            json!({
                "Version": "2012-10-17",
                "Statement": [{
                    "Effect": "Deny",
                    "Principal": { "Service": SERVICE_PRINCIPAL },
                    "Action": "sts:AssumeRole"
                }]
            })
            .to_string(),
            false,
        ),
        (
            // A trust policy governs `sts:AssumeRole`, so a statement naming this service for
            // another action grants nothing.
            "clean rooms ml allowed an action that is not assuming the role",
            json!({
                "Version": "2012-10-17",
                "Statement": [{
                    "Effect": "Allow",
                    "Principal": { "Service": SERVICE_PRINCIPAL },
                    "Action": "s3:GetObject"
                }]
            })
            .to_string(),
            false,
        ),
    ];

    let mut accepted: Vec<String> = Vec::new();
    for (index, (case, document, is_accepted)) in rows.into_iter().enumerate() {
        let role = format!("assume-role-{index}");
        let role_arn = role_with_trust(&router, &role, &document).await;
        // Every role here may read every table, so the only thing a row varies is the trust.
        put_role_policy(&router, &role, &identity_policy("Allow", "glue:*", "*")).await;
        let name = format!("assume-dataset-{index}");
        let (status, payload) = create_dataset(
            &router,
            create_dataset_body(&name, &role_arn, "lake", "events"),
        )
        .await;
        if is_accepted {
            assert_eq!(
                status, 200,
                "case {case:?}: a role this service may assume must be accepted, got {status} \
                 {payload}"
            );
            accepted.push(name);
        } else {
            assert_access_denied(
                case,
                status,
                &payload,
                &format!("Service was not able to assume role {role_arn}."),
            );
        }
        let listed = listed_dataset_names(&router).await;
        assert_eq!(
            listed, accepted,
            "case {case:?}: expected exactly the accepted datasets, got {listed:?}"
        );
    }

    // A role ARN naming no role this account holds is refused the same way: there is nothing
    // to assume. The rows are well-formed `IamRoleArn` values naming no role.
    for (index, (case, role_arn)) in [
        (
            "a role nothing was created under",
            format!("arn:aws:iam::{ACCOUNT}:role/does-not-exist"),
        ),
        (
            "a role under a path",
            format!("arn:aws:iam::{ACCOUNT}:role/service-role/does-not-exist"),
        ),
        (
            "a role name that is a prefix of one that exists",
            format!("arn:aws:iam::{ACCOUNT}:role/assume-role-"),
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let name = format!("absent-role-dataset-{index}");
        let (status, payload) = create_dataset(
            &router,
            create_dataset_body(&name, &role_arn, "lake", "events"),
        )
        .await;
        assert_access_denied(
            case,
            status,
            &payload,
            &format!("Service was not able to assume role {role_arn}."),
        );
        let listed = listed_dataset_names(&router).await;
        assert_eq!(
            listed, accepted,
            "case {case:?}: a refused create must persist nothing, got {listed:?}"
        );
    }
}

/// A role the service can assume but whose permissions do not admit the read of the Glue table
/// the dataset points at is refused naming that role and that table. The rows span the forms a
/// permissions policy takes and the database and table the dataset names.
#[tokio::test]
async fn a_role_that_may_not_read_the_table_is_refused() {
    let router = emulator();

    // (case, the database, the table, the policy to write, whether the create is accepted)
    let rows: Vec<(&str, &str, &str, Option<String>, bool)> = vec![
        (
            "read of that very table",
            "clean_rooms_demo",
            "interactions",
            Some(identity_policy(
                "Allow",
                "glue:GetTable",
                &table_arn("clean_rooms_demo", "interactions"),
            )),
            true,
        ),
        (
            "every glue action on every resource",
            "lake",
            "events",
            Some(identity_policy("Allow", "glue:*", "*")),
            true,
        ),
        (
            "no permissions policy at all",
            "warehouse",
            "orders",
            None,
            false,
        ),
        (
            "read of another table",
            "warehouse",
            "profiles",
            Some(identity_policy(
                "Allow",
                "glue:GetTable",
                &table_arn("warehouse", "somewhere_else"),
            )),
            false,
        ),
        (
            "a glue action that is not the read",
            "warehouse",
            "sessions",
            Some(identity_policy(
                "Allow",
                "glue:CreateTable",
                &table_arn("warehouse", "sessions"),
            )),
            false,
        ),
        (
            "the read of that table denied",
            "warehouse",
            "clicks",
            Some(
                json!({
                    "Version": "2012-10-17",
                    "Statement": [
                        {
                            "Effect": "Allow",
                            "Action": "glue:*",
                            "Resource": "*"
                        },
                        {
                            "Effect": "Deny",
                            "Action": "glue:GetTable",
                            "Resource": table_arn("warehouse", "clicks")
                        }
                    ]
                })
                .to_string(),
            ),
            false,
        ),
    ];

    let mut accepted: Vec<String> = Vec::new();
    for (index, (case, database, table, policy, is_accepted)) in rows.into_iter().enumerate() {
        let role = format!("read-role-{index}");
        let role_arn = role_with_trust(&router, &role, &assumable_trust()).await;
        if let Some(policy) = policy.as_deref() {
            put_role_policy(&router, &role, policy).await;
        }
        let name = format!("read-dataset-{index}");
        let (status, payload) = create_dataset(
            &router,
            create_dataset_body(&name, &role_arn, database, table),
        )
        .await;
        if is_accepted {
            assert_eq!(
                status, 200,
                "case {case:?}: a role permitted to read the table must be accepted, got \
                 {status} {payload}"
            );
            accepted.push(name);
        } else {
            assert_access_denied(
                case,
                status,
                &payload,
                &format!(
                    "Role {role_arn} does not have read access to Glue table {}.",
                    table_arn(database, table)
                ),
            );
        }
        let listed = listed_dataset_names(&router).await;
        assert_eq!(
            listed, accepted,
            "case {case:?}: expected exactly the accepted datasets, got {listed:?}"
        );
    }
}
