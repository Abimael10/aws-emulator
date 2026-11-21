// SPDX-License-Identifier: CC0-1.0

//! The Lake Formation `SELECT` lifecycle a reader of an AWS Glue Data Catalog table lives
//! under, read through every API that reports it — driven across the services the question
//! spans, mounted on ONE engine exactly as `core/router/src/lib.rs` mounts them.
//!
//! RULE: a Data Catalog table whose data location Lake Formation manages is readable by a
//! principal exactly while a `SELECT` grant that resolves to that principal's identity is in
//! force. The grant is consulted per call — a grant takes effect for the next query and a
//! revoke withdraws it for the next one, with nothing remembered from the last — and the
//! identity a grant resolves for is the IAM identity the signing credential speaks for: the
//! IAM user behind a long-term access key, and the ROLE behind an assumed-role session, which
//! is the ARN a Lake Formation grant names such a session by. While the grant holds, the query
//! settles `SUCCEEDED` and answers the rows held at the table's registered location. Once it
//! does not, the table is NOT THERE for that caller: the query settles `FAILED` carrying the
//! catalog's own table-not-found class for the relation rather than an authorization error,
//! the catalog's own metadata read of the table is refused `AccessDeniedException`, and Lake
//! Formation's own effective-permission listing for that principal and table carries no entry.
//!
//! Six crates have to agree for that to hold (Athena runs the statement, Glue holds the
//! catalog and governs its metadata reads, Lake Formation owns the grants, Amazon S3 holds the
//! data, and IAM and STS mint the principals a grant names), so the suite lives here rather
//! than in any of them. Every grant, revoke and registration below goes through Lake
//! Formation's own wire API and every principal through IAM's and STS's own, so what the read
//! paths consult is the state those calls wrote.
//!
//! The rows differ along what the rule generalises over: the FORM of the principal (an IAM
//! user's long-term key, and a session of a role) and the POSITION in the lifecycle (newly
//! granted, revoked, granted again). Nothing here asserts a value this emulator invented: the
//! rows are the bytes written to the location, and the failure's message is built from the
//! statement that was submitted.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_athena_amazonathena::state::{
    QUERY_STATE_FAILED, QUERY_STATE_SUCCEEDED,
};

use crate::query_wire::{element, form_encode};

/// The Region every call here is signed for.
const REGION: &str = "us-east-1";

/// The access key id the SETUP calls sign with: neither IAM nor STS holds a record of it, so
/// it resolves to the account's own ambient identity — the principal that creates the catalog
/// and owns the data lake, and deliberately NOT the one that reads below.
const ACCOUNT_KEY: &str = "AKIDEXAMPLE";

/// The name Athena reaches the account's Glue Data Catalog by, which is also the catalog a
/// table-not-found failure names.
const GLUE_CATALOG: &str = "awsdatacatalog";

/// The bucket the tables keep their data in, and the one the queries write results to.
const DATA_BUCKET: &str = "lake-formation-lifecycle-data";
const RESULTS_BUCKET: &str = "lake-formation-lifecycle-results";

/// The database holding the tables: its create hands the tables created in it NO
/// `IAM_ALLOWED_PRINCIPALS` default, which is the state a client that has handed its catalog
/// to Lake Formation is in.
const DATABASE: &str = "select_lifecycle_lake";

/// The columns every table declares, in order, with the Hive types the catalog records.
const COLUMNS: [(&str, &str); 3] = [("id", "bigint"), ("name", "string"), ("zone", "string")];

/// The object written under each table's location, and the rows a reader granted the table
/// must be served from it — the bytes, never a value this emulator chose.
const DATA: &str = "1,alpha,west\n2,beta,east\n";
const EXPECTED_ROWS: [[&str; 3]; 2] = [["1", "alpha", "west"], ["2", "beta", "east"]];

/// The role Lake Formation registers the data location under, and the service principal whose
/// trust lets it ("Choose a role that you know has permission to do this" —
/// `RegisterResource`).
const REGISTRATION_ROLE: &str = "LakeFormationDataAccessRole";
const LAKE_FORMATION_PRINCIPAL: &str = "lakeformation.amazonaws.com";

/// The identity-based permissions a reader needs before Lake Formation decides anything: it
/// has to be allowed to START a query and to READ the catalog's metadata, which is the pairing
/// AWS's own `AmazonAthenaFullAccess` grants (`athena:*` beside `glue:Get*`).
fn reader_policy() -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [
            { "Effect": "Allow", "Action": "athena:*", "Resource": "*" },
            { "Effect": "Allow", "Action": "glue:Get*", "Resource": "*" },
        ],
    })
    .to_string()
}

/// How long a poll waits for a started query to settle. Generous: what is asserted is that the
/// execution reaches a terminal state, never how fast the host got there.
const SETTLE_DEADLINE: Duration = Duration::from_secs(120);

/// How long one poll sleeps before looking again.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// The states a query is still working in.
const PENDING_STATES: [&str; 2] = ["QUEUED", "RUNNING"];

/// The permission a read of a table's data needs, and the one a metadata read needs, spelled
/// as Lake Formation's `Permission` enum spells them.
const SELECT: &str = "SELECT";
const DESCRIBE: &str = "DESCRIBE";

/// The services this question spans, over ONE shared engine.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_athena_amazonathena::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(service_com_amazonaws_glue_awsglue::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_lakeformation_awslakeformation::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(service_com_amazonaws_s3_amazons3::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_sts_awssecuritytokenservicev20110615::EmulatedService::new(engine),
    );
    router
}

/// A SigV4 `Authorization` header naming `service` in its credential scope — the routing key
/// the router claims a request by — signed with `access_key_id`, which is what names the
/// principal the request speaks for. Nothing is verified: this router runs with request
/// authentication off.
fn authorization(service: &str, access_key_id: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential={access_key_id}/20260101/{REGION}/{service}/aws4_request, \
         SignedHeaders=host;x-amz-target, Signature=not-verified"
    )
}

/// One `awsJson1_1` call to `service`, signed with `access_key_id`.
async fn json_as(
    router: &MultiServiceRouter,
    service: &str,
    target_prefix: &str,
    access_key_id: &str,
    operation: &str,
    body: &Value,
) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{service}.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("{target_prefix}.{operation}"))
        .header("authorization", authorization(service, access_key_id))
        .body(Bytes::from(body.to_string()))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// One Glue call signed with `access_key_id`.
async fn glue_as(
    router: &MultiServiceRouter,
    access_key_id: &str,
    operation: &str,
    body: &Value,
) -> (u16, Value) {
    json_as(router, "glue", "AWSGlue", access_key_id, operation, body).await
}

/// One Glue call by the account's own credential, which must succeed: the catalog's creator.
async fn glue_ok(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    let (status, answer) = glue_as(router, ACCOUNT_KEY, operation, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: Glue {operation} must answer 200, got {status} with {answer}"
    );
    answer
}

/// One Athena call signed with `access_key_id`.
async fn athena_as(
    router: &MultiServiceRouter,
    access_key_id: &str,
    operation: &str,
    body: &Value,
) -> (u16, Value) {
    json_as(
        router,
        "athena",
        "AmazonAthena",
        access_key_id,
        operation,
        body,
    )
    .await
}

/// One Lake Formation call (`restJson1`: `POST /<Operation>`), by the account's own
/// credential, which must succeed.
async fn lake_formation_ok(
    router: &MultiServiceRouter,
    case: &str,
    operation: &str,
    body: &Value,
) -> Value {
    let request = http::Request::builder()
        .method("POST")
        .uri(format!(
            "https://lakeformation.{REGION}.amazonaws.com/{operation}"
        ))
        .header("host", format!("lakeformation.{REGION}.amazonaws.com"))
        .header("content-type", "application/json")
        .header(
            "authorization",
            authorization("lakeformation", ACCOUNT_KEY),
        )
        .body(Bytes::from(body.to_string()))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    assert_eq!(
        status, 200,
        "case {case:?}: Lake Formation {operation} must answer 200, got {status} with {parsed}"
    );
    parsed
}

/// Write one object (or, with an empty path under the bucket, create the bucket) in Amazon S3.
async fn s3_put(router: &MultiServiceRouter, case: &str, path: &str, body: &str) {
    let request = http::Request::builder()
        .method("PUT")
        .uri(path)
        .header("host", format!("s3.{REGION}.amazonaws.com"))
        .header("authorization", authorization("s3", ACCOUNT_KEY))
        .body(Bytes::from(body.to_string()))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    assert!(
        response.status().is_success(),
        "case {case:?}: writing s3:/{path} must succeed, got {}",
        response.status()
    );
}

/// One `awsQuery` call to IAM or STS, signed with the account's own credential, which must
/// succeed, answering its XML body.
async fn query(
    router: &MultiServiceRouter,
    signing_name: &str,
    version: &str,
    action: &str,
    params: &[(&str, String)],
) -> String {
    let mut body = format!("Action={action}&Version={version}");
    for (name, value) in params {
        body.push('&');
        body.push_str(name);
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{signing_name}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization(signing_name, ACCOUNT_KEY))
        .body(Bytes::from(body))
        .expect("a well-formed awsQuery request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let text = String::from_utf8_lossy(response.body()).to_string();
    assert_eq!(
        status, 200,
        "case {action:?}: the setup call must succeed, got {status} with {text}"
    );
    text
}

/// A trust policy letting one AWS service principal assume the role.
fn service_trust(service_principal: &str) -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "Service": service_principal },
            "Action": "sts:AssumeRole",
        }],
    })
    .to_string()
}

/// A trust policy letting this account's own identities assume the role.
fn account_trust() -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": {
                "AWS": format!("arn:aws:iam::{}:root", ::engine::emulator_account_id()),
            },
            "Action": "sts:AssumeRole",
        }],
    })
    .to_string()
}

/// Create an IAM role carrying `trust` as its trust policy and [`reader_policy`] inline when
/// `readable`, and answer the ARN IAM holds it under.
async fn role(
    router: &MultiServiceRouter,
    role_name: &str,
    trust: &str,
    readable: bool,
) -> String {
    let created = query(
        router,
        "iam",
        "2010-05-08",
        "CreateRole",
        &[
            ("RoleName", role_name.to_string()),
            ("AssumeRolePolicyDocument", trust.to_string()),
        ],
    )
    .await;
    if readable {
        query(
            router,
            "iam",
            "2010-05-08",
            "PutRolePolicy",
            &[
                ("RoleName", role_name.to_string()),
                ("PolicyName", "reader".to_string()),
                ("PolicyDocument", reader_policy()),
            ],
        )
        .await;
    }
    element(&created, "Arn")
}

/// One reader a row drives: what it is, the table it reads, the ARN a Lake Formation grant
/// names it by (the IAM identity its credential speaks for), and the key id it signs with.
#[derive(Debug)]
struct Reader {
    case: &'static str,
    table: &'static str,
    grant_arn: String,
    access_key_id: String,
}

/// A reader whose credential is a SESSION of a role: the grant names the ROLE, while the
/// requests are signed with the temporary key id STS minted for the session.
async fn role_session_reader(router: &MultiServiceRouter) -> Reader {
    let role_arn = role(router, "lake-reader-role", &account_trust(), true).await;
    let assumed = query(
        router,
        "sts",
        "2011-06-15",
        "AssumeRole",
        &[
            ("RoleArn", role_arn.clone()),
            ("RoleSessionName", "lifecycle".to_string()),
        ],
    )
    .await;
    Reader {
        case: "an assumed-role session, granted as the role it is a session of",
        table: "t_rev_session",
        grant_arn: role_arn,
        access_key_id: element(&assumed, "AccessKeyId"),
    }
}

/// A reader whose credential is an IAM USER's long-term access key: the grant names the user.
async fn user_reader(router: &MultiServiceRouter) -> Reader {
    let user_name = "lake-reader-user";
    let created = query(
        router,
        "iam",
        "2010-05-08",
        "CreateUser",
        &[("UserName", user_name.to_string())],
    )
    .await;
    query(
        router,
        "iam",
        "2010-05-08",
        "PutUserPolicy",
        &[
            ("UserName", user_name.to_string()),
            ("PolicyName", "reader".to_string()),
            ("PolicyDocument", reader_policy()),
        ],
    )
    .await;
    let minted = query(
        router,
        "iam",
        "2010-05-08",
        "CreateAccessKey",
        &[("UserName", user_name.to_string())],
    )
    .await;
    Reader {
        case: "an IAM user's long-term access key, granted as the user",
        table: "t_rev_user",
        grant_arn: element(&created, "Arn"),
        access_key_id: element(&minted, "AccessKeyId"),
    }
}

/// The `s3://` location the table `table` keeps its data at, under the prefix the data lake
/// registration covers.
fn table_location(table: &str) -> String {
    format!("s3://{DATA_BUCKET}/data/{table}/")
}

/// The ARN of the data-lake location Lake Formation manages: the prefix every table's own
/// location sits under.
fn data_location_arn() -> String {
    format!("arn:aws:s3:::{DATA_BUCKET}/data/")
}

/// Create the buckets, the governed database, and one CSV external table per reader with its
/// data object written under the registered prefix; then register that prefix with Lake
/// Formation under a role it is trusted to use.
async fn governed_lake(router: &MultiServiceRouter, case: &str, readers: &[Reader]) {
    for bucket in [DATA_BUCKET, RESULTS_BUCKET] {
        s3_put(router, case, &format!("/{bucket}"), "").await;
    }
    glue_ok(
        router,
        case,
        "CreateDatabase",
        &json!({
            "DatabaseInput": { "Name": DATABASE, "CreateTableDefaultPermissions": [] },
        }),
    )
    .await;
    for reader in readers {
        s3_put(
            router,
            case,
            &format!("/{DATA_BUCKET}/data/{}/p.csv", reader.table),
            DATA,
        )
        .await;
        glue_ok(
            router,
            case,
            "CreateTable",
            &json!({
                "DatabaseName": DATABASE,
                "TableInput": {
                    "Name": reader.table,
                    "TableType": "EXTERNAL_TABLE",
                    "StorageDescriptor": {
                        "Location": table_location(reader.table),
                        "Columns": COLUMNS
                            .iter()
                            .map(|(name, data_type)| json!({ "Name": name, "Type": data_type }))
                            .collect::<Vec<Value>>(),
                        // The data objects are comma-separated text, which is the SerDe the
                        // table's own definition has to say it is read by.
                        "SerdeInfo": {
                            "SerializationLibrary":
                                "org.apache.hadoop.hive.serde2.lazy.LazySimpleSerDe",
                            "Parameters": { "field.delim": "," },
                        },
                    },
                },
            }),
        )
        .await;
    }

    let registration_role = role(
        router,
        REGISTRATION_ROLE,
        &service_trust(LAKE_FORMATION_PRINCIPAL),
        false,
    )
    .await;
    lake_formation_ok(
        router,
        case,
        "RegisterResource",
        &json!({
            "ResourceArn": data_location_arn(),
            "RoleArn": registration_role,
            "UseServiceLinkedRole": false,
        }),
    )
    .await;

    // Every reader may DESCRIBE the database its table lives in: a metadata permission on the
    // database, which says nothing about the DATA of a table inside it.
    for reader in readers {
        grant(
            router,
            case,
            &reader.grant_arn,
            json!({ "Database": { "Name": DATABASE } }),
            &[DESCRIBE],
        )
        .await;
    }
}

/// Grant `permissions` on `resource` to `principal`.
async fn grant(
    router: &MultiServiceRouter,
    case: &str,
    principal: &str,
    resource: Value,
    permissions: &[&str],
) {
    lake_formation_ok(
        router,
        case,
        "GrantPermissions",
        &json!({
            "Principal": { "DataLakePrincipalIdentifier": principal },
            "Resource": resource,
            "Permissions": permissions,
        }),
    )
    .await;
}

/// Revoke `permissions` on `resource` from `principal`.
async fn revoke(
    router: &MultiServiceRouter,
    case: &str,
    principal: &str,
    resource: Value,
    permissions: &[&str],
) {
    lake_formation_ok(
        router,
        case,
        "RevokePermissions",
        &json!({
            "Principal": { "DataLakePrincipalIdentifier": principal },
            "Resource": resource,
            "Permissions": permissions,
        }),
    )
    .await;
}

/// `Resource` naming one table of the governed database.
fn on_table(table: &str) -> Value {
    json!({ "Table": { "DatabaseName": DATABASE, "Name": table } })
}

/// The statement a reader submits: the three columns of its table, qualified by the database
/// and ordered, so the rows are compared in a fixed order.
fn select(table: &str) -> String {
    let projection = COLUMNS
        .iter()
        .map(|(name, _)| *name)
        .collect::<Vec<&str>>()
        .join(", ");
    format!("SELECT {projection} FROM \"{DATABASE}\".\"{table}\" ORDER BY id")
}

/// What a hidden relation's failure says: the catalog's own not-found class for the relation
/// the statement named, carrying the catalog, database and table of the request and the
/// position the submitted statement wrote the reference at.
fn table_not_found(sql: &str, table: &str) -> String {
    let column = sql
        .find(&format!("\"{DATABASE}\""))
        .map(|position| position + 1)
        .unwrap_or_default();
    format!(
        "TABLE_NOT_FOUND: line 1:{column}: Table '{GLUE_CATALOG}.{DATABASE}.{table}' does not \
         exist"
    )
}

/// Submit `sql` as `reader` and answer the `QueryExecutionId` Athena minted for it.
async fn start_as(
    router: &MultiServiceRouter,
    case: &str,
    reader: &Reader,
    sql: &str,
) -> String {
    let (status, answer) = athena_as(
        router,
        &reader.access_key_id,
        "StartQueryExecution",
        &json!({
            "QueryString": sql,
            "QueryExecutionContext": { "Database": DATABASE, "Catalog": GLUE_CATALOG },
            "ResultConfiguration": {
                "OutputLocation": format!("s3://{RESULTS_BUCKET}/results/"),
            },
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: StartQueryExecution({sql}) must be accepted, got {status} with {answer}"
    );
    answer["QueryExecutionId"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected a query execution id, got {answer}"))
        .to_string()
}

/// The `Status` of `id` once the execution has reached a TERMINAL state, polled until it does
/// or [`SETTLE_DEADLINE`] passes — the completion is owed a span after the submit, so what is
/// waited for is the condition and never a fixed time.
async fn settled_status(
    router: &MultiServiceRouter,
    case: &str,
    reader: &Reader,
    id: &str,
) -> Value {
    let started = Instant::now();
    loop {
        let (status, body) = athena_as(
            router,
            &reader.access_key_id,
            "GetQueryExecution",
            &json!({ "QueryExecutionId": id }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: GetQueryExecution must answer 200, got {status} with {body}"
        );
        let reported = body["QueryExecution"]["Status"].clone();
        let state = reported["State"].as_str().unwrap_or_default().to_string();
        if !PENDING_STATES.contains(&state.as_str()) {
            return reported;
        }
        assert!(
            started.elapsed() < SETTLE_DEADLINE,
            "case {case:?}: the execution never reached a terminal state, last saw {reported}"
        );
        ::tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The column names and the data rows `id`'s result set carries, with the label row the first
/// page of a DML result set opens with dropped.
async fn results(
    router: &MultiServiceRouter,
    case: &str,
    reader: &Reader,
    id: &str,
) -> (Vec<String>, Vec<Vec<String>>) {
    let (status, body) = athena_as(
        router,
        &reader.access_key_id,
        "GetQueryResults",
        &json!({ "QueryExecutionId": id, "MaxResults": 100 }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: GetQueryResults must answer 200, got {status} with {body}"
    );
    let result_set = &body["ResultSet"];
    let columns: Vec<String> = result_set["ResultSetMetadata"]["ColumnInfo"]
        .as_array()
        .unwrap_or(&Vec::new())
        .iter()
        .map(|column| column["Name"].as_str().unwrap_or_default().to_string())
        .collect();
    let rows: Vec<Vec<String>> = result_set["Rows"]
        .as_array()
        .unwrap_or(&Vec::new())
        .iter()
        .skip(1)
        .map(|row| {
            row["Data"]
                .as_array()
                .unwrap_or(&Vec::new())
                .iter()
                .map(|datum| datum["VarCharValue"].as_str().unwrap_or_default().to_string())
                .collect()
        })
        .collect();
    (columns, rows)
}

/// What Lake Formation reports that `principal` effectively holds on the table `table`: a
/// listing narrowed to both a principal and a resource reports effective permissions.
async fn effective_permissions(
    router: &MultiServiceRouter,
    case: &str,
    principal: &str,
    table: &str,
) -> Vec<Value> {
    let answered = lake_formation_ok(
        router,
        case,
        "ListPermissions",
        &json!({
            "Principal": { "DataLakePrincipalIdentifier": principal },
            "Resource": on_table(table),
        }),
    )
    .await;
    answered["PrincipalResourcePermissions"]
        .as_array()
        .unwrap_or_else(|| {
            panic!("case {case:?}: ListPermissions must answer a list, got {answered}")
        })
        .clone()
}

/// The columns the catalog's own read of `table` reports to `reader`, with the status and body
/// of that read.
async fn catalog_read(
    router: &MultiServiceRouter,
    reader: &Reader,
    table: &str,
) -> (u16, Value) {
    glue_as(
        router,
        &reader.access_key_id,
        "GetTable",
        &json!({ "DatabaseName": DATABASE, "Name": table }),
    )
    .await
}

/// One position in the lifecycle: what Lake Formation is made to hold, and therefore whether
/// the table is there for the reader afterwards.
#[derive(Debug, Clone, Copy)]
enum Step {
    /// `SELECT` on the table is granted to the reader.
    Granted,
    /// That grant is revoked again.
    Revoked,
    /// It is granted a second time, after the revoke.
    ReGranted,
}

impl Step {
    /// What this step is called in a failure.
    fn label(self) -> &'static str {
        match self {
            Self::Granted => "SELECT granted",
            Self::Revoked => "SELECT revoked",
            Self::ReGranted => "SELECT granted again",
        }
    }

    /// Whether the table is readable by the reader once this step has been taken.
    fn readable(self) -> bool {
        match self {
            Self::Granted | Self::ReGranted => true,
            Self::Revoked => false,
        }
    }
}

/// A `SELECT` grant decides the read for the whole lifecycle and for every form the reader's
/// credential takes: the query, the catalog's metadata read and Lake Formation's own listing
/// agree at each position, and a revoke re-hides the table instead of refusing it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_select_grant_decides_an_athena_read_over_its_whole_lifecycle() {
    let router = emulator();
    let readers = vec![role_session_reader(&router).await, user_reader(&router).await];
    governed_lake(&router, "the governed lake", &readers).await;

    for reader in &readers {
        let sql = select(reader.table);
        for step in [Step::Granted, Step::Revoked, Step::ReGranted] {
            let case = format!("{}: {}", reader.case, step.label());
            if step.readable() {
                grant(
                    &router,
                    &case,
                    &reader.grant_arn,
                    on_table(reader.table),
                    &[SELECT],
                )
                .await;
            } else {
                revoke(
                    &router,
                    &case,
                    &reader.grant_arn,
                    on_table(reader.table),
                    &[SELECT],
                )
                .await;
            }

            // What the QUERY answers.
            let id = start_as(&router, &case, reader, &sql).await;
            let status = settled_status(&router, &case, reader, &id).await;
            if step.readable() {
                assert_eq!(
                    status["State"].as_str(),
                    Some(QUERY_STATE_SUCCEEDED),
                    "case {case:?}: the granted read must succeed, got {status}"
                );
                let (columns, rows) = results(&router, &case, reader, &id).await;
                let expected_columns: Vec<String> = COLUMNS
                    .iter()
                    .map(|(name, _)| (*name).to_string())
                    .collect();
                assert_eq!(
                    columns, expected_columns,
                    "case {case:?}: expected the projected columns {expected_columns:?}, got \
                     {columns:?}"
                );
                let expected_rows: Vec<Vec<String>> = EXPECTED_ROWS
                    .iter()
                    .map(|row| row.iter().map(|cell| (*cell).to_string()).collect())
                    .collect();
                assert_eq!(
                    rows, expected_rows,
                    "case {case:?}: expected the rows written at the table's location \
                     {expected_rows:?}, got {rows:?}"
                );
            } else {
                assert_eq!(
                    status["State"].as_str(),
                    Some(QUERY_STATE_FAILED),
                    "case {case:?}: the read must stop working, got {status}"
                );
                let expected = table_not_found(&sql, reader.table);
                assert_eq!(
                    status["StateChangeReason"].as_str(),
                    Some(expected.as_str()),
                    "case {case:?}: expected the not-found reason {expected:?}, got {status}"
                );
                assert_eq!(
                    status["AthenaError"]["ErrorMessage"].as_str(),
                    Some(expected.as_str()),
                    "case {case:?}: expected every reader of the failure to carry {expected:?}, \
                     got {status}"
                );
            }

            // What the CATALOG's own metadata read answers.
            let (catalog_status, catalog_body) = catalog_read(&router, reader, reader.table).await;
            if step.readable() {
                assert_eq!(
                    catalog_status, 200,
                    "case {case:?}: the reader's GetTable must be served, got {catalog_status} \
                     with {catalog_body}"
                );
                let projected: Vec<String> = catalog_body["Table"]["StorageDescriptor"]["Columns"]
                    .as_array()
                    .unwrap_or(&Vec::new())
                    .iter()
                    .map(|column| column["Name"].as_str().unwrap_or_default().to_string())
                    .collect();
                let expected_columns: Vec<String> = COLUMNS
                    .iter()
                    .map(|(name, _)| (*name).to_string())
                    .collect();
                assert_eq!(
                    projected, expected_columns,
                    "case {case:?}: expected GetTable to project {expected_columns:?}, got \
                     {projected:?}"
                );
            } else {
                assert_eq!(
                    catalog_status, 400,
                    "case {case:?}: the reader's GetTable must be refused, got {catalog_status} \
                     with {catalog_body}"
                );
                assert_eq!(
                    catalog_body["__type"]
                        .as_str()
                        .unwrap_or_default()
                        .rsplit('#')
                        .next()
                        .unwrap_or_default(),
                    "AccessDeniedException",
                    "case {case:?}: expected AccessDeniedException, got {catalog_body}"
                );
                let expected = format!(
                    "Insufficient Lake Formation permission(s): Required Describe on {}",
                    reader.table
                );
                assert_eq!(
                    catalog_body["Message"]
                        .as_str()
                        .or_else(|| catalog_body["message"].as_str()),
                    Some(expected.as_str()),
                    "case {case:?}: expected the refusal {expected:?}, got {catalog_body}"
                );
            }

            // What LAKE FORMATION's own listing answers for that principal and table.
            let held =
                effective_permissions(&router, &case, &reader.grant_arn, reader.table).await;
            if step.readable() {
                let selects: Vec<&Value> = held
                    .iter()
                    .filter(|entry| {
                        entry["Permissions"]
                            .as_array()
                            .is_some_and(|permissions| {
                                permissions.iter().any(|value| value == SELECT)
                            })
                    })
                    .collect();
                assert_eq!(
                    selects.len(),
                    1,
                    "case {case:?}: expected exactly one entry carrying {SELECT}, got {held:?}"
                );
                assert_eq!(
                    selects[0]["Principal"]["DataLakePrincipalIdentifier"].as_str(),
                    Some(reader.grant_arn.as_str()),
                    "case {case:?}: expected the entry to name {}, got {held:?}",
                    reader.grant_arn
                );
            } else {
                assert!(
                    held.is_empty(),
                    "case {case:?}: the revoked grant must leave no entry, got {held:?}"
                );
            }
        }
    }
}

/// The data-lake location the lifecycle above registers is a RECORD Lake Formation holds, not
/// a call that was accepted and dropped: its own read of that ARN answers the registration,
/// reporting the data-access role the register named.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_registered_data_lake_location_is_read_back_from_lake_formation() {
    let case = "the registered data lake location";
    let router = emulator();
    let readers = vec![user_reader(&router).await];
    governed_lake(&router, case, &readers).await;

    let described = lake_formation_ok(
        &router,
        case,
        "DescribeResource",
        &json!({ "ResourceArn": data_location_arn() }),
    )
    .await;
    let expected_role = format!(
        "arn:aws:iam::{}:role/{REGISTRATION_ROLE}",
        ::engine::emulator_account_id()
    );
    assert_eq!(
        described["ResourceInfo"]["RoleArn"].as_str(),
        Some(expected_role.as_str()),
        "case {case:?}: expected the role the registration named, got {described}"
    );
}
