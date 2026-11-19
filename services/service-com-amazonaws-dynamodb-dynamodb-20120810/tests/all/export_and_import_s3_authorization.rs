// SPDX-License-Identifier: CC0-1.0

//! Authorization of the Amazon S3 calls a table EXPORT and a table IMPORT make for the
//! caller.
//!
//! RULE pinned here: **the object calls a table export and a table import perform are made
//! with the permissions of the principal that asked for the task, so each one is authorized
//! exactly as that principal's own Amazon S3 call would be — against the identity-based
//! policies in force on it and the bucket's own policy, where an explicit `Deny` in either
//! refuses it whatever any grant says and otherwise it needs an `Allow` from one of them.
//! The task itself is accepted (both operations are asynchronous), and a task whose object
//! calls the policies in force refuse ends `FAILED` with the S3 access-denied failure code
//! and a message naming the principal, the action and the resource it addressed — with no
//! object written under the export's destination prefix, and no source data in the imported
//! table — rather than reporting a `COMPLETED` it did not achieve.**
//!
//! The rows span what the rule generalises over: WHICH half carries the statement (the
//! caller's identity policy, the bucket's own policy, both), what it says (nothing at all, a
//! grant over this bucket, a grant over another bucket, a grant with an explicit deny over
//! it, a deny naming the role), WHICH call of the task is the one refused (the object write
//! of an export, the listing that selects an import's source, the read of one source
//! object), and which caller asks (an assumed-role session, and the account's own credential,
//! which holds no identity policies at all).
//!
//! Driven over the wire through one `MultiServiceRouter` serving DynamoDB beside IAM, STS and
//! Amazon S3 on ONE shared engine — the role lives in IAM's store, its session credential is
//! minted by STS and the bucket is S3's own resource, exactly as `core/router` assembles
//! them.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use service_com_amazonaws_dynamodb_dynamodb_20120810::{state, EmulatedService};
use smithy_server_runtime::MultiServiceRouter;

use crate::emulator::{create_bucket, put_object, s3, POLL_INTERVAL, REGION};

/// The account every generated emulator presents.
const ACCOUNT: &str = engine::EMULATOR_ACCOUNT_ID;

/// The long-term key every emulator publishes, which is the account's own credential — the
/// caller that holds no identity-based policies at all.
const AMBIENT_KEY: &str = "AKIAIOSFODNN7EXAMPLE";

/// The session name every `AssumeRole` here asks for, which is the last segment of the
/// assumed-role ARN a refusal names.
const SESSION: &str = "probe";

/// The policy-language version every document here states.
const POLICY_VERSION: &str = "2012-10-17";

/// How long a poll for the `IN_PROGRESS` -> terminal transition the clock owes a started
/// task may take before a suite calls it a hang. Generous slack over the emulator's own
/// transition span: a loaded machine is not a defect, and the point of the bound is that the
/// wait ENDS.
const TERMINAL_DEADLINE: Duration = Duration::from_secs(60);

/// The reason a refusal rests on when nothing grants the call: IAM's implicit deny.
fn no_grant(action: &str) -> String {
    format!("because no identity-based policy allows the {action} action")
}

/// The reason a refusal rests on when the BUCKET's own policy denies the call.
const RESOURCE_DENY: &str = "with an explicit deny in a resource-based policy";

/// The reason a refusal rests on when the CALLER's own documents deny the call.
const IDENTITY_DENY: &str = "with an explicit deny in an identity-based policy";

/// The emulator the rows drive: DynamoDB, IAM, STS and Amazon S3 over ONE engine.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    // Unsigned requests: the `Authorization` header below still names the Region and the
    // access key id, which is what each service reads the caller's identity from.
    router.set_request_authentication(false);
    router.register_service(EmulatedService::new(Arc::clone(&engine)));
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_sts_awssecuritytokenservicev20110615::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(service_com_amazonaws_s3_amazons3::EmulatedService::new(
        engine,
    ));
    router
}

/// A SigV4 `Authorization` header whose credential scope names `signing_name` and whose
/// credential is `access_key_id`.
fn authorization(access_key_id: &str, signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential={access_key_id}/20260101/{REGION}/{signing_name}/\
         aws4_request, SignedHeaders=host;x-amz-target, Signature=0000"
    )
}

/// Send one DynamoDB operation AS `access_key_id`, answering its status and parsed body.
async fn ddb_as(
    router: &MultiServiceRouter,
    access_key_id: &str,
    operation: &str,
    body: Value,
) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("content-type", "application/x-amz-json-1.0")
        .header("x-amz-target", format!("DynamoDB_20120810.{operation}"))
        .header("authorization", authorization(access_key_id, "dynamodb"))
        .body(Bytes::from(
            serde_json::to_vec(&body).expect("the request body serializes"),
        ))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    (status, parsed)
}

/// [`ddb_as`] for a call that must be served, answering its parsed body.
async fn ddb_ok(
    router: &MultiServiceRouter,
    case: &str,
    access_key_id: &str,
    operation: &str,
    body: Value,
) -> Value {
    let (status, parsed) = ddb_as(router, access_key_id, operation, body.clone()).await;
    assert_eq!(
        status, 200,
        "case {case:?}: {operation} {body} must answer 200, got {status} with {parsed}"
    );
    parsed
}

/// Post one awsQuery form body to `signing_name` as the account's own credential.
async fn query(router: &MultiServiceRouter, signing_name: &str, body: String) -> (u16, String) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{signing_name}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization(AMBIENT_KEY, signing_name))
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (status, String::from_utf8_lossy(response.body()).to_string())
}

/// Percent-encode one `application/x-www-form-urlencoded` component through the maintained
/// encoder rather than by hand.
fn form_encode(value: &str) -> String {
    percent_encoding::utf8_percent_encode(value, percent_encoding::NON_ALPHANUMERIC).to_string()
}

/// The text of the first `<name>…</name>` element of an XML body.
fn element(case: &str, body: &str, name: &str) -> String {
    let open = format!("<{name}>");
    let close = format!("</{name}>");
    let (_, rest) = body
        .split_once(&open)
        .unwrap_or_else(|| panic!("case {case:?}: the answer must carry <{name}>, got {body}"));
    let (text, _) = rest
        .split_once(&close)
        .unwrap_or_else(|| panic!("case {case:?}: <{name}> must be closed, got {body}"));
    text.to_string()
}

/// A trust policy letting the account assume the role.
fn trust_policy() -> String {
    json!({
        "Version": POLICY_VERSION,
        "Statement": [{
            "Effect": "Allow",
            "Principal": {"AWS": format!("arn:aws:iam::{ACCOUNT}:root")},
            "Action": "sts:AssumeRole",
        }],
    })
    .to_string()
}

/// Create one IAM role trusted by the account, embed `statements` in it as its inline
/// policy, assume it, and answer the access key id of the session STS minted beside the
/// role's own ARN.
async fn role_session(
    router: &MultiServiceRouter,
    case: &str,
    role: &str,
    statements: Value,
) -> (String, String) {
    let (status, body) = query(
        router,
        "iam",
        format!(
            "Action=CreateRole&Version=2010-05-08&RoleName={}&AssumeRolePolicyDocument={}",
            form_encode(role),
            form_encode(&trust_policy())
        ),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateRole({role}) must answer 200, got {status} with {body}"
    );
    let role_arn = element(case, &body, "Arn");
    let document = json!({"Version": POLICY_VERSION, "Statement": statements}).to_string();
    let (status, body) = query(
        router,
        "iam",
        format!(
            "Action=PutRolePolicy&Version=2010-05-08&RoleName={}&PolicyName=probe&\
             PolicyDocument={}",
            form_encode(role),
            form_encode(&document)
        ),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: PutRolePolicy({role}) must answer 200, got {status} with {body}"
    );
    let (status, body) = query(
        router,
        "sts",
        format!(
            "Action=AssumeRole&Version=2011-06-15&RoleArn={}&RoleSessionName={SESSION}",
            form_encode(&role_arn)
        ),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: AssumeRole({role_arn}) must answer 200, got {status} with {body}"
    );
    (element(case, &body, "AccessKeyId"), role_arn)
}

/// The principal an assumed-role session of `role` speaks for, as `sts:GetCallerIdentity`
/// reports it and a refusal names it.
fn session_principal(role: &str) -> String {
    format!("arn:aws:sts::{ACCOUNT}:assumed-role/{role}/{SESSION}")
}

/// The ARN of an Amazon S3 bucket, which carries neither Region nor account because a bucket
/// name is global.
fn bucket_arn(bucket: &str) -> String {
    format!("arn:aws:s3:::{bucket}")
}

/// The ARN of one object in a bucket.
fn object_arn(bucket: &str, key: &str) -> String {
    format!("{}/{key}", bucket_arn(bucket))
}

/// The sentence AWS refuses an unauthorized call with, which is what a refused task reports
/// as its `FailureMessage`.
fn refusal(principal: &str, action: &str, resource: &str, reason: &str) -> String {
    format!(
        "User: {principal} is not authorized to perform: {action} on resource: {resource} {reason}"
    )
}

/// Attach `statements` to `bucket` as its bucket policy, through Amazon S3's own API.
async fn put_bucket_policy(router: &MultiServiceRouter, case: &str, bucket: &str, statements: Value) {
    let document = json!({"Version": POLICY_VERSION, "Statement": statements}).to_string();
    s3(router)
        .put_bucket_policy()
        .bucket(bucket)
        .policy(document)
        .send()
        .await
        .unwrap_or_else(|error| panic!("case {case:?}: S3 takes the policy of {bucket:?}: {error:?}"));
}

/// The keys `bucket` holds, in the order Amazon S3 lists them.
async fn listed_keys(router: &MultiServiceRouter, bucket: &str) -> Vec<String> {
    let listed = s3(router)
        .list_objects_v2()
        .bucket(bucket)
        .send()
        .await
        .unwrap_or_else(|error| panic!("S3 lists {bucket:?}: {error:?}"));
    listed
        .contents()
        .iter()
        .filter_map(|object| object.key().map(str::to_string))
        .collect()
}

/// Poll `DescribeExport` as the account's own credential until the export leaves
/// `IN_PROGRESS`, answering the description it reported then.
async fn terminal_export(router: &MultiServiceRouter, case: &str, arn: &str) -> Value {
    let deadline = Instant::now() + TERMINAL_DEADLINE;
    loop {
        let described = ddb_ok(
            router,
            case,
            AMBIENT_KEY,
            "DescribeExport",
            json!({"ExportArn": arn}),
        )
        .await["ExportDescription"]
            .clone();
        if described["ExportStatus"] != json!(state::EXPORT_STATUS_IN_PROGRESS) {
            return described;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the export never left IN_PROGRESS, last description {described}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Poll `DescribeImport` as the account's own credential until the import leaves
/// `IN_PROGRESS`, answering the description it reported then.
async fn terminal_import(router: &MultiServiceRouter, case: &str, arn: &str) -> Value {
    let deadline = Instant::now() + TERMINAL_DEADLINE;
    loop {
        let described = ddb_ok(
            router,
            case,
            AMBIENT_KEY,
            "DescribeImport",
            json!({"ImportArn": arn}),
        )
        .await["ImportTableDescription"]
            .clone();
        if described["ImportStatus"] != json!(state::IMPORT_STATUS_IN_PROGRESS) {
            return described;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the import never left IN_PROGRESS, last description {described}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Poll `DescribeTable` as the account's own credential until the table reports `ACTIVE`, so
/// a reader driven afterwards is reading a table that is ready.
async fn wait_active(router: &MultiServiceRouter, case: &str, name: &str) {
    let deadline = Instant::now() + TERMINAL_DEADLINE;
    loop {
        let described = ddb_ok(
            router,
            case,
            AMBIENT_KEY,
            "DescribeTable",
            json!({"TableName": name}),
        )
        .await["Table"]
            .clone();
        if described["TableStatus"] == json!("ACTIVE") {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the table never became ACTIVE, last description {described}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Create one on-demand table as the account's own credential, wait for it to become
/// `ACTIVE`, turn point-in-time recovery on and put one item in it — everything an export
/// needs before the rule under test decides anything.
async fn exportable_table(router: &MultiServiceRouter, case: &str, name: &str) {
    ddb_ok(
        router,
        case,
        AMBIENT_KEY,
        "CreateTable",
        json!({
            "TableName": name,
            "KeySchema": [{"AttributeName": "Part", "KeyType": "HASH"}],
            "AttributeDefinitions": [{"AttributeName": "Part", "AttributeType": "S"}],
            "BillingMode": "PAY_PER_REQUEST",
        }),
    )
    .await;
    wait_active(router, case, name).await;
    ddb_ok(
        router,
        case,
        AMBIENT_KEY,
        "UpdateContinuousBackups",
        json!({
            "TableName": name,
            "PointInTimeRecoverySpecification": {"PointInTimeRecoveryEnabled": true},
        }),
    )
    .await;
    ddb_ok(
        router,
        case,
        AMBIENT_KEY,
        "PutItem",
        json!({"TableName": name, "Item": {"Part": {"S": "alpha"}}}),
    )
    .await;
}

/// What the role's own identity policy says about the task's Amazon S3 calls, beside the
/// grant of the DynamoDB action that starts the task.
#[derive(Debug, Clone, Copy)]
enum Identity {
    /// No statement about Amazon S3 at all.
    NothingAboutS3,
    /// The named S3 actions, on the bucket the task names.
    Grants(&'static [&'static str]),
    /// The named S3 actions, on ANOTHER bucket's resources.
    GrantsElsewhere(&'static [&'static str]),
    /// Every action on every resource, with an explicit deny of the named ones over it.
    Denies(&'static [&'static str]),
    /// Every action on every resource.
    GrantsEverything,
}

/// What the bucket's own policy says about the role.
#[derive(Debug, Clone, Copy)]
enum BucketPolicy {
    /// The bucket carries no policy, which is the state a created bucket is in.
    None,
    /// An explicit `Deny` of every S3 action naming the role's ARN.
    DeniesTheRole,
    /// An `Allow` of the task's own S3 actions naming the role's ARN.
    AllowsTheRole,
}

/// The identity statements one row's role carries: the DynamoDB action that starts the task,
/// plus what the row says about the S3 actions on `bucket`.
fn identity_statements(identity: Identity, ddb_action: &str, bucket: &str) -> Value {
    let starts = json!({"Effect": "Allow", "Action": ddb_action, "Resource": "*"});
    let on = |named: &str| json!([bucket_arn(named), format!("{}/*", bucket_arn(named))]);
    match identity {
        Identity::NothingAboutS3 => json!([starts]),
        Identity::Grants(actions) => json!([
            starts,
            {"Effect": "Allow", "Action": actions, "Resource": on(bucket)},
        ]),
        Identity::GrantsElsewhere(actions) => json!([
            starts,
            {"Effect": "Allow", "Action": actions, "Resource": on("elsewhere-entirely")},
        ]),
        Identity::Denies(actions) => json!([
            starts,
            {"Effect": "Allow", "Action": "*", "Resource": "*"},
            {"Effect": "Deny", "Action": actions, "Resource": "*"},
        ]),
        Identity::GrantsEverything => json!([
            starts,
            {"Effect": "Allow", "Action": "*", "Resource": "*"},
        ]),
    }
}

/// The bucket-policy statements one row attaches to its bucket, or `None` where the row
/// leaves the bucket without a policy.
fn bucket_statements(
    policy: BucketPolicy,
    role_arn: &str,
    bucket: &str,
    actions: &[&str],
) -> Option<Value> {
    let resources = json!([bucket_arn(bucket), format!("{}/*", bucket_arn(bucket))]);
    match policy {
        BucketPolicy::None => None,
        BucketPolicy::DeniesTheRole => Some(json!([{
            "Effect": "Deny",
            "Principal": {"AWS": role_arn},
            "Action": "s3:*",
            "Resource": resources,
        }])),
        BucketPolicy::AllowsTheRole => Some(json!([{
            "Effect": "Allow",
            "Principal": {"AWS": role_arn},
            "Action": actions,
            "Resource": resources,
        }])),
    }
}

/// The Amazon S3 actions an export's write and an import's read are authorized under, which
/// are Amazon S3's own actions for those calls — named from the catalogue this crate states
/// them in (`crate::state::EXPORT_OBJECT_WRITE_ACTION`,
/// `crate::state::IMPORT_SOURCE_LISTING_ACTION`, `crate::state::IMPORT_OBJECT_READ_ACTION`).
const EXPORT_ACTIONS: &[&str] = &[state::EXPORT_OBJECT_WRITE_ACTION];
const IMPORT_ACTIONS: &[&str] = &[
    state::IMPORT_SOURCE_LISTING_ACTION,
    state::IMPORT_OBJECT_READ_ACTION,
];

/// Each of an import's two calls on its own, for the rows that grant one and not the other.
const LISTING_ONLY: &[&str] = &[state::IMPORT_SOURCE_LISTING_ACTION];
const READ_ONLY: &[&str] = &[state::IMPORT_OBJECT_READ_ACTION];

/// One export row: what each half of the decision says, and whether the task's write is
/// therefore made.
struct ExportRow {
    case: &'static str,
    identity: Identity,
    policy: BucketPolicy,
    /// The reason the refusal rests on, or `None` for a row whose export completes.
    refused: Option<String>,
}

#[tokio::test]
async fn an_export_writes_its_objects_only_where_the_callers_policies_allow_the_write() {
    let router = emulator();
    let rows = vec![
        ExportRow {
            case: "no S3 permission anywhere",
            identity: Identity::NothingAboutS3,
            policy: BucketPolicy::None,
            refused: Some(no_grant(state::EXPORT_OBJECT_WRITE_ACTION)),
        },
        ExportRow {
            case: "the identity policy grants the write on this bucket",
            identity: Identity::Grants(EXPORT_ACTIONS),
            policy: BucketPolicy::None,
            refused: None,
        },
        ExportRow {
            case: "the bucket policy denies the role over an identity-based allow",
            identity: Identity::GrantsEverything,
            policy: BucketPolicy::DeniesTheRole,
            refused: Some(RESOURCE_DENY.to_string()),
        },
        ExportRow {
            case: "the bucket policy grants the role what its identity does not",
            identity: Identity::NothingAboutS3,
            policy: BucketPolicy::AllowsTheRole,
            refused: None,
        },
        ExportRow {
            case: "the identity policy grants the write on another bucket",
            identity: Identity::GrantsElsewhere(EXPORT_ACTIONS),
            policy: BucketPolicy::None,
            refused: Some(no_grant(state::EXPORT_OBJECT_WRITE_ACTION)),
        },
        ExportRow {
            case: "an explicit deny in the identity policy over its own allow",
            identity: Identity::Denies(EXPORT_ACTIONS),
            policy: BucketPolicy::None,
            refused: Some(IDENTITY_DENY.to_string()),
        },
        ExportRow {
            case: "both halves grant the write",
            identity: Identity::Grants(EXPORT_ACTIONS),
            policy: BucketPolicy::AllowsTheRole,
            refused: None,
        },
    ];

    for (index, row) in rows.into_iter().enumerate() {
        let case = row.case;
        let bucket = format!("export-auth-{index}");
        let table = format!("ExportAuth{index}");
        let role = format!("ddb-export-auth-{index}");
        create_bucket(&router, &bucket).await;
        exportable_table(&router, case, &table).await;
        let (access_key_id, role_arn) = role_session(
            &router,
            case,
            &role,
            identity_statements(row.identity, "dynamodb:ExportTableToPointInTime", &bucket),
        )
        .await;
        if let Some(statements) =
            bucket_statements(row.policy, &role_arn, &bucket, EXPORT_ACTIONS)
        {
            put_bucket_policy(&router, case, &bucket, statements).await;
        }

        // The export is ACCEPTED whatever the policies say about the write: the operation is
        // asynchronous, so the verdict surfaces on the export's own description.
        let started = ddb_ok(
            &router,
            case,
            &access_key_id,
            "ExportTableToPointInTime",
            json!({
                "TableArn": state::table_arn(REGION, &table),
                "S3Bucket": bucket,
                "ExportFormat": "DYNAMODB_JSON",
            }),
        )
        .await["ExportDescription"]
            .clone();
        assert_eq!(
            started["ExportStatus"],
            json!(state::EXPORT_STATUS_IN_PROGRESS),
            "case {case:?}: the export is accepted and starts, got {started}"
        );
        let arn = started["ExportArn"].as_str().unwrap_or_default().to_string();
        let export_id = arn.rsplit('/').next().unwrap_or_default().to_string();

        let finished = terminal_export(&router, case, &arn).await;
        let keys = listed_keys(&router, &bucket).await;
        let Some(reason) = row.refused else {
            assert_eq!(
                finished["ExportStatus"],
                json!(state::EXPORT_STATUS_COMPLETED),
                "case {case:?}: the policies allow the write, so the export completes, got \
                 {finished}"
            );
            assert!(
                keys.contains(&format!("AWSDynamoDB/{export_id}/manifest-summary.json")),
                "case {case:?}: a completed export leaves its manifest in the bucket, got {keys:?}"
            );
            continue;
        };
        assert_eq!(
            finished["ExportStatus"],
            json!(state::EXPORT_STATUS_FAILED),
            "case {case:?}: an export whose write is not authorized cannot complete, got \
             {finished}"
        );
        assert_eq!(
            finished["FailureCode"],
            json!(state::S3_ACCESS_DENIED_FAILURE_CODE),
            "case {case:?}: the failure says the destination refused the write, got {finished}"
        );
        // The message is built from the request and from what the two stores hold: the
        // principal the session speaks for, the action the export performs, and the very
        // object it would have written first.
        assert_eq!(
            finished["FailureMessage"],
            json!(refusal(
                &session_principal(&role),
                state::EXPORT_OBJECT_WRITE_ACTION,
                &object_arn(&bucket, &format!("AWSDynamoDB/{export_id}/_started")),
                &reason,
            )),
            "case {case:?}: expected the refusal of the export's own write, got {finished}"
        );
        assert_eq!(
            keys,
            Vec::<String>::new(),
            "case {case:?}: a refused export writes nothing under its destination prefix, got \
             {keys:?}"
        );
        for member in ["ExportManifest", "ItemCount", "BilledSizeBytes"] {
            assert!(
                finished[member].is_null(),
                "case {case:?}: a failed export reports no {member}, got {finished}"
            );
        }
    }
}

#[tokio::test]
async fn the_accounts_own_credential_exports_without_an_identity_policy() {
    // The account's own published credential holds no IAM documents and owns its own
    // resources, so the write it performs is refused by nothing — the control every row of
    // the suite above is measured against.
    let router = emulator();
    let case = "the account's own credential";
    let bucket = "export-auth-ambient";
    create_bucket(&router, bucket).await;
    exportable_table(&router, case, "ExportAuthAmbient").await;

    let started = ddb_ok(
        &router,
        case,
        AMBIENT_KEY,
        "ExportTableToPointInTime",
        json!({
            "TableArn": state::table_arn(REGION, "ExportAuthAmbient"),
            "S3Bucket": bucket,
        }),
    )
    .await["ExportDescription"]
        .clone();
    let arn = started["ExportArn"].as_str().unwrap_or_default().to_string();
    let finished = terminal_export(&router, case, &arn).await;
    assert_eq!(
        finished["ExportStatus"],
        json!(state::EXPORT_STATUS_COMPLETED),
        "case {case:?}: the account's own credential is refused by nothing, got {finished}"
    );
    let keys = listed_keys(&router, bucket).await;
    assert!(
        !keys.is_empty(),
        "case {case:?}: the export's objects are in the bucket, got {keys:?}"
    );
}

/// One import row: what each half of the decision says, which of the task's two calls is
/// refused, and on which resource.
struct ImportRow {
    case: &'static str,
    identity: Identity,
    policy: BucketPolicy,
    /// The action and the reason of the refusal, or `None` for a row whose import completes.
    refused: Option<(&'static str, String)>,
}

/// The key the one source object of each import row is written under.
const SOURCE_KEY: &str = "source.json";

#[tokio::test]
async fn an_import_reads_its_source_only_where_the_callers_policies_allow_the_read() {
    let router = emulator();
    let listing = state::IMPORT_SOURCE_LISTING_ACTION;
    let read = state::IMPORT_OBJECT_READ_ACTION;
    let rows = vec![
        ImportRow {
            case: "no S3 permission anywhere",
            identity: Identity::NothingAboutS3,
            policy: BucketPolicy::None,
            refused: Some((listing, no_grant(listing))),
        },
        ImportRow {
            case: "the identity policy grants the listing and the read on this bucket",
            identity: Identity::Grants(IMPORT_ACTIONS),
            policy: BucketPolicy::None,
            refused: None,
        },
        ImportRow {
            case: "the identity policy grants them on another bucket",
            identity: Identity::GrantsElsewhere(IMPORT_ACTIONS),
            policy: BucketPolicy::None,
            refused: Some((listing, no_grant(listing))),
        },
        ImportRow {
            case: "the bucket policy denies the role over an identity-based allow",
            identity: Identity::GrantsEverything,
            policy: BucketPolicy::DeniesTheRole,
            refused: Some((listing, RESOURCE_DENY.to_string())),
        },
        ImportRow {
            case: "the bucket policy grants the role what its identity does not",
            identity: Identity::NothingAboutS3,
            policy: BucketPolicy::AllowsTheRole,
            refused: None,
        },
        ImportRow {
            case: "the listing alone is granted, so the object read is not",
            identity: Identity::Grants(LISTING_ONLY),
            policy: BucketPolicy::None,
            refused: Some((read, no_grant(read))),
        },
        ImportRow {
            case: "the object read alone is granted, so the listing is not",
            identity: Identity::Grants(READ_ONLY),
            policy: BucketPolicy::None,
            refused: Some((listing, no_grant(listing))),
        },
        ImportRow {
            case: "an explicit deny of the object read over a wildcard allow",
            identity: Identity::Denies(READ_ONLY),
            policy: BucketPolicy::None,
            refused: Some((read, IDENTITY_DENY.to_string())),
        },
    ];

    for (index, row) in rows.into_iter().enumerate() {
        let case = row.case;
        let bucket = format!("import-auth-{index}");
        let table = format!("ImportAuth{index}");
        let role = format!("ddb-import-auth-{index}");
        create_bucket(&router, &bucket).await;
        put_object(
            &router,
            &bucket,
            SOURCE_KEY,
            json!({"Item": {"Part": {"S": "alpha"}}})
                .to_string()
                .into_bytes(),
        )
        .await;
        let (access_key_id, role_arn) = role_session(
            &router,
            case,
            &role,
            identity_statements(row.identity, "dynamodb:ImportTable", &bucket),
        )
        .await;
        if let Some(statements) =
            bucket_statements(row.policy, &role_arn, &bucket, IMPORT_ACTIONS)
        {
            put_bucket_policy(&router, case, &bucket, statements).await;
        }

        let started = ddb_ok(
            &router,
            case,
            &access_key_id,
            "ImportTable",
            json!({
                "S3BucketSource": {"S3Bucket": bucket},
                "InputFormat": "DYNAMODB_JSON",
                "TableCreationParameters": {
                    "TableName": table,
                    "KeySchema": [{"AttributeName": "Part", "KeyType": "HASH"}],
                    "AttributeDefinitions": [{"AttributeName": "Part", "AttributeType": "S"}],
                    "BillingMode": "PAY_PER_REQUEST",
                },
            }),
        )
        .await["ImportTableDescription"]
            .clone();
        assert_eq!(
            started["ImportStatus"],
            json!(state::IMPORT_STATUS_IN_PROGRESS),
            "case {case:?}: the import is accepted and starts, got {started}"
        );
        let arn = started["ImportArn"].as_str().unwrap_or_default().to_string();

        let finished = terminal_import(&router, case, &arn).await;
        let Some((action, reason)) = row.refused else {
            assert_eq!(
                finished["ImportStatus"],
                json!(state::IMPORT_STATUS_COMPLETED),
                "case {case:?}: the policies allow the read, so the import completes, got \
                 {finished}"
            );
            assert_eq!(
                finished["ImportedItemCount"],
                json!(1),
                "case {case:?}: the import wrote the item its source carried, got {finished}"
            );
            // And the item is in the table the import created, read back through this
            // service's own reader.
            wait_active(&router, case, &table).await;
            let item = ddb_ok(
                &router,
                case,
                AMBIENT_KEY,
                "GetItem",
                json!({"TableName": table, "Key": {"Part": {"S": "alpha"}}}),
            )
            .await;
            assert_eq!(
                item["Item"],
                json!({"Part": {"S": "alpha"}}),
                "case {case:?}: the imported table serves the source's own item, got {item}"
            );
            continue;
        };
        assert_eq!(
            finished["ImportStatus"],
            json!(state::IMPORT_STATUS_FAILED),
            "case {case:?}: an import whose source read is not authorized cannot complete, got \
             {finished}"
        );
        assert_eq!(
            finished["FailureCode"],
            json!(state::S3_ACCESS_DENIED_FAILURE_CODE),
            "case {case:?}: the failure says the source refused the read, got {finished}"
        );
        let resource = if action == read {
            object_arn(&bucket, SOURCE_KEY)
        } else {
            bucket_arn(&bucket)
        };
        assert_eq!(
            finished["FailureMessage"],
            json!(refusal(
                &session_principal(&role),
                action,
                &resource,
                &reason
            )),
            "case {case:?}: expected the refusal of the import's own call, got {finished}"
        );
        // No source data reached the table the import created.
        wait_active(&router, case, &table).await;
        let item = ddb_ok(
            &router,
            case,
            AMBIENT_KEY,
            "GetItem",
            json!({"TableName": table, "Key": {"Part": {"S": "alpha"}}}),
        )
        .await;
        assert!(
            item["Item"].is_null(),
            "case {case:?}: a refused import imports nothing, got {item}"
        );
    }
}
