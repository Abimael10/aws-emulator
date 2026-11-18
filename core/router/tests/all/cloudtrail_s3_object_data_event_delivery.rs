// SPDX-License-Identifier: CC0-1.0

//! What a LOGGING trail delivers for the account's S3 OBJECT calls, including the ones Amazon
//! S3 REFUSED: the audit stream a consumer of the trail's bucket reconstructs a denied read
//! from.
//!
//! RULE: while a trail is logging, every call on the CONTENTS of one object that one of that
//! trail's event selectors admits — served or refused, whichever principal made it — is
//! delivered into that trail's bucket as a gzipped log file under the documented key layout
//! (`<key-prefix>/AWSLogs/<account>/CloudTrail/<region>/YYYY/MM/DD/`), and a call no selector
//! of that trail admits is delivered nowhere. A refused call is answered to its caller with
//! the status, the error code and the error message AWS words for that refusal class, which is
//! what the record of it carries and so what makes the refusal auditable.
//!
//! Four services are mounted on ONE engine, exactly as `core/router/src/lib.rs` mounts them,
//! because the behaviour spans them: IAM holds the role and the document in force on it, STS
//! mints the role session a bucket policy denies, Amazon S3 serves (or refuses) the object
//! calls and is where the log files land, and CloudTrail holds the trail whose advanced event
//! selector decides the delivery. No crate-local suite can reach this: a role session has to
//! be minted by STS and denied by a bucket policy before S3 can refuse a call that CloudTrail
//! then has to deliver.
//!
//! The rows span what the rule generalises over rather than the one call that motivated it:
//! WHICH object operation served the call (a write, a read, a metadata read, a delete), WHAT
//! the call was answered with (served, the absent-key refusal, the policy refusal), WHICH
//! principal made it (the account's own credential, an assumed-role session), and WHETHER the
//! trail's selector admits it at all (another bucket, a bucket-level call).
//!
//! What a delivered record CARRIES is pinned where the gzip reader lives — the CloudTrail
//! crate's own `tests/all/trail_log_file_delivery_of_s3_object_data_events.rs`, which reads
//! each record's fields. `core/router/Cargo.toml` is a pinned path in this repository, so this
//! test crate can take no decompressor (see `query_wire.rs`'s own note): here a delivered file
//! is asserted by its key layout and by its gzip framing, and the code and message the caller
//! was answered with are asserted on the caller's own answer.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{elements, form_encode, opt_element};

/// The Region every call below is signed for, which is the Region the trail is homed in and so
/// the Region its log files land under.
const REGION: &str = "us-east-1";

/// The account every generated emulator presents, which owns every bucket, role and trail this
/// suite creates and names every delivered log file's prefix.
const ACCOUNT: &str = ::engine::EMULATOR_ACCOUNT_ID;

/// The access key id the SETUP and the account-credential rows carry. A key no session and no
/// IAM user holds speaks for the account itself, which no identity-based document bounds.
const ACCOUNT_KEY: &str = "AKIDTEST";

/// CloudTrail's `X-Amz-Target` prefix, and the API versions the two awsQuery services take.
const CLOUDTRAIL_TARGET_PREFIX: &str = "CloudTrail_20131101.";
const IAM_API_VERSION: &str = "2010-05-08";
const STS_API_VERSION: &str = "2011-06-15";

/// The `eventCategory` of a data event, as an advanced event selector pins it.
const DATA_CATEGORY: &str = "Data";

/// The type an object is named by in a delivered record's `resources` and in an advanced event
/// selector's `resources.type` field.
const OBJECT_RESOURCE_TYPE: &str = "AWS::S3::Object";

/// The segments of a CloudTrail log-file key, and what such a file's name ends with.
const LOG_FILE_ROOT_SEGMENT: &str = "AWSLogs";
const LOG_FILE_SERVICE_SEGMENT: &str = "CloudTrail";
const LOG_FILE_SUFFIX: &str = ".json.gz";

/// The first two bytes of a gzip member (RFC 1952 section 2.3.1: `ID1` `0x1f`, `ID2` `0x8b`),
/// which is what a log file's object is framed in.
const GZIP_MAGIC: [u8; 2] = [0x1f, 0x8b];

/// The name of the one inline document the role carries.
const ROLE_POLICY_NAME: &str = "object-grants";

/// The session name the denied rounds assume the role under.
const ROLE_SESSION_NAME: &str = "ctdeny";

/// The two IAM actions an object read and an object write that name no version are authorized
/// under, as each operation's own documentation names them.
const GET_OBJECT_ACTION: &str = "s3:GetObject";
const PUT_OBJECT_ACTION: &str = "s3:PutObject";

/// The key prefix the data bucket's policy explicitly denies the role session.
const DENIED_PREFIX: &str = "denied/";

/// How often, and how many times at most, a poll looks for a log file the delivery writes while
/// the call is being served. One look is normally enough; the loop is what keeps a loaded host
/// waiting rather than failing.
const LOOK: Duration = Duration::from_millis(50);
const LOOKS: usize = 400;

/// One request as this suite builds it, so the one dispatcher takes a value rather than eight
/// positional arguments.
struct Call<'a> {
    /// The access key id in the credential scope, which is what names the caller.
    access_key_id: &'a str,
    /// The signing name in that scope, which is what routes the request.
    signing_name: &'a str,
    method: &'a str,
    path: &'a str,
    content_type: &'a str,
    /// The `X-Amz-Target` an awsJson call carries, and nothing for the other wires.
    target: Option<&'a str>,
    body: Vec<u8>,
}

/// One answer as its caller reads it: the status, the raw bytes (a log file is binary) and
/// those bytes as text.
struct Answered {
    status: u16,
    bytes: Bytes,
    text: String,
}

/// IAM, STS, CloudTrail and Amazon S3 over ONE shared engine.
fn router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    // Signing is not the subject here, so the signatures are not verified; the credential scope
    // is still carried, because it is what names the Region and the caller.
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(
        service_com_amazonaws_sts_awssecuritytokenservicev20110615::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(
        service_com_amazonaws_cloudtrail_cloudtrail_20131101::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(service_com_amazonaws_s3_amazons3::EmulatedService::new(
        engine,
    ));
    router
}

/// A SigV4 `Authorization` whose credential scope names `signing_name` and whose credential is
/// `access_key_id` — the key a service reads the caller's identity from.
fn authorization(access_key_id: &str, signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential={access_key_id}/20260101/{REGION}/{signing_name}/\
         aws4_request, SignedHeaders=host, Signature=not-verified"
    )
}

/// Dispatch one request and read back its status and body.
async fn dispatch(router: &MultiServiceRouter, call: Call<'_>) -> Answered {
    let host = format!("{}.{REGION}.amazonaws.com", call.signing_name);
    let mut builder = http::Request::builder()
        .method(call.method)
        .uri(call.path)
        .header("host", host)
        .header("content-type", call.content_type)
        .header(
            "authorization",
            authorization(call.access_key_id, call.signing_name),
        );
    if let Some(target) = call.target {
        builder = builder.header("x-amz-target", target);
    }
    let request = builder
        .body(Bytes::from(call.body))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let bytes = response.into_body();
    let text = String::from_utf8_lossy(&bytes).to_string();
    Answered {
        status,
        bytes,
        text,
    }
}

/// One S3 call on the path-style address, presented as signed by `access_key_id`.
async fn s3(
    router: &MultiServiceRouter,
    access_key_id: &str,
    method: &str,
    path: &str,
    body: &str,
) -> Answered {
    dispatch(
        router,
        Call {
            access_key_id,
            signing_name: "s3",
            method,
            path,
            content_type: "application/xml",
            target: None,
            body: body.as_bytes().to_vec(),
        },
    )
    .await
}

/// One awsQuery call to `signing_name`, as the account's own credential, every component
/// percent-encoded so a policy document travels verbatim.
async fn query(
    router: &MultiServiceRouter,
    signing_name: &str,
    version: &str,
    action: &str,
    members: &[(&str, &str)],
) -> Answered {
    let mut body = format!("Action={action}&Version={version}");
    for (name, value) in members {
        body.push('&');
        body.push_str(&form_encode(name));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    dispatch(
        router,
        Call {
            access_key_id: ACCOUNT_KEY,
            signing_name,
            method: "POST",
            path: "/",
            content_type: "application/x-www-form-urlencoded",
            target: None,
            body: body.into_bytes(),
        },
    )
    .await
}

/// One CloudTrail `awsJson1_1` call that has to be served, as its parsed answer.
async fn cloudtrail(router: &MultiServiceRouter, operation: &str, body: &Value) -> Value {
    let target = format!("{CLOUDTRAIL_TARGET_PREFIX}{operation}");
    let answered = dispatch(
        router,
        Call {
            access_key_id: ACCOUNT_KEY,
            signing_name: "cloudtrail",
            method: "POST",
            path: "/",
            content_type: "application/x-amz-json-1.1",
            target: Some(&target),
            body: body.to_string().into_bytes(),
        },
    )
    .await;
    assert_eq!(
        answered.status, 200,
        "setup: CloudTrail {operation} must be served, got {} with {}",
        answered.status, answered.text
    );
    serde_json::from_slice(&answered.bytes).unwrap_or(Value::Null)
}

/// The ARN of one object, as a bucket policy, a selector and a refusal all name it: an S3 ARN
/// carries no Region and no account, because a bucket name is globally unique.
fn object_arn(bucket: &str, key: &str) -> String {
    format!("arn:aws:s3:::{bucket}/{key}")
}

/// Store one object as the account's credential, so a row that READS it reads an object that is
/// there.
///
/// A read of an absent key is answered on the `s3:ListBucket` permission over the BUCKET
/// instead — "if the object you request doesn't exist … Amazon S3 returns `HTTP 403`
/// (`AccessDenied`)" when the caller holds no listing grant, which is what keeps a read from
/// disclosing whether a key exists — so the object a row asserts the OBJECT-level decision on is
/// seeded first.
async fn seed_object(router: &MultiServiceRouter, named: &str, key: &str) {
    let stored = s3(
        router,
        ACCOUNT_KEY,
        "PUT",
        &format!("/{named}/{key}"),
        "seeded",
    )
    .await;
    assert_eq!(
        stored.status, 200,
        "setup: PutObject({named:?}, {key:?}) must be served, got {} with {}",
        stored.status, stored.text
    );
}

/// Create `named` through S3's own API as the account's credential.
async fn create_bucket(router: &MultiServiceRouter, named: &str) {
    let created = s3(router, ACCOUNT_KEY, "PUT", &format!("/{named}"), "").await;
    assert_eq!(
        created.status, 200,
        "setup: CreateBucket({named:?}) must be served, got {} with {}",
        created.status, created.text
    );
}

/// `PUT /<bucket>?policy` with `document`, which S3 answers with no content.
async fn put_bucket_policy(router: &MultiServiceRouter, named: &str, document: &str) {
    let policied = s3(
        router,
        ACCOUNT_KEY,
        "PUT",
        &format!("/{named}?policy"),
        document,
    )
    .await;
    assert!(
        (200..300).contains(&policied.status),
        "setup: PutBucketPolicy({named:?}) must be served, got {} with {}",
        policied.status,
        policied.text
    );
}

/// Create the bucket a trail delivers into, carrying the policy CloudTrail's own documentation
/// states for a log bucket: the ACL check on the bucket, and the object write under the key
/// prefix the trail is created with — `<key-prefix>/AWSLogs/<account>/*`, which is exactly the
/// area the delivery writes into.
async fn delivery_bucket(router: &MultiServiceRouter, named: &str, key_prefix: &str) {
    create_bucket(router, named).await;
    let document = json!({
        "Version": "2012-10-17",
        "Statement": [
            {
                "Sid": "AWSCloudTrailAclCheck",
                "Effect": "Allow",
                "Principal": {"Service": "cloudtrail.amazonaws.com"},
                "Action": "s3:GetBucketAcl",
                "Resource": format!("arn:aws:s3:::{named}"),
            },
            {
                "Sid": "AWSCloudTrailWrite",
                "Effect": "Allow",
                "Principal": {"Service": "cloudtrail.amazonaws.com"},
                "Action": "s3:PutObject",
                "Resource": format!(
                    "arn:aws:s3:::{named}/{key_prefix}/{LOG_FILE_ROOT_SEGMENT}/{ACCOUNT}/*"
                ),
                "Condition": {
                    "StringEquals": {"s3:x-amz-acl": "bucket-owner-full-control"},
                },
            },
        ],
    })
    .to_string();
    put_bucket_policy(router, named, &document).await;
}

/// Create the role the denied rounds are made by, with the one document in force on it — the
/// object actions over the data bucket, which is what makes the policy-denied rows a DENY
/// overriding a grant rather than IAM's default deny — and answer its ARN.
async fn role_allowed_on(router: &MultiServiceRouter, name: &str, data_bucket: &str) -> String {
    let trust = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": {"AWS": format!("arn:aws:iam::{ACCOUNT}:root")},
            "Action": "sts:AssumeRole",
        }],
    })
    .to_string();
    let created = query(
        router,
        "iam",
        IAM_API_VERSION,
        "CreateRole",
        &[("RoleName", name), ("AssumeRolePolicyDocument", &trust)],
    )
    .await;
    assert_eq!(
        created.status, 200,
        "setup: CreateRole({name:?}) must be served, got {} with {}",
        created.status, created.text
    );
    let role_arn = opt_element(&created.text, "Arn").unwrap_or_else(|| {
        panic!(
            "setup: CreateRole({name:?}) must answer the role's ARN, got {}",
            created.text
        )
    });
    let grants = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Action": [GET_OBJECT_ACTION, PUT_OBJECT_ACTION],
            "Resource": format!("arn:aws:s3:::{data_bucket}/*"),
        }],
    })
    .to_string();
    let embedded = query(
        router,
        "iam",
        IAM_API_VERSION,
        "PutRolePolicy",
        &[
            ("RoleName", name),
            ("PolicyName", ROLE_POLICY_NAME),
            ("PolicyDocument", &grants),
        ],
    )
    .await;
    assert_eq!(
        embedded.status, 200,
        "setup: PutRolePolicy({name:?}) must be served, got {} with {}",
        embedded.status, embedded.text
    );
    role_arn
}

/// Assume `role_arn` through STS's own `AssumeRole` and answer `(the session's access key id,
/// the session's principal ARN)` — the ARN a refusal of a call made with it names.
///
/// The ANSWER carries credential material, so nothing here quotes it: a failure names what was
/// looked for instead.
async fn role_session(router: &MultiServiceRouter, role_arn: &str) -> (String, String) {
    let assumed = query(
        router,
        "sts",
        STS_API_VERSION,
        "AssumeRole",
        &[
            ("RoleArn", role_arn),
            ("RoleSessionName", ROLE_SESSION_NAME),
        ],
    )
    .await;
    assert_eq!(
        assumed.status, 200,
        "setup: AssumeRole({role_arn:?}) must be served, got {}",
        assumed.status
    );
    let access_key_id = opt_element(&assumed.text, "AccessKeyId").unwrap_or_else(|| {
        panic!("setup: AssumeRole({role_arn:?}) must answer an access key id")
    });
    let session_arn = opt_element(&assumed.text, "Arn")
        .unwrap_or_else(|| panic!("setup: AssumeRole({role_arn:?}) must answer the session's ARN"));
    (access_key_id, session_arn)
}

/// The data bucket's policy: an explicit `Deny` of the two object actions under
/// [`DENIED_PREFIX`], named at the ROLE, which is the spelling a resource-based policy names a
/// role's sessions by.
async fn deny_prefix_to(router: &MultiServiceRouter, data_bucket: &str, role_arn: &str) {
    let document = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Sid": "DenyTheRoleUnderTheDeniedPrefix",
            "Effect": "Deny",
            "Principal": {"AWS": role_arn},
            "Action": [GET_OBJECT_ACTION, PUT_OBJECT_ACTION],
            "Resource": format!("arn:aws:s3:::{data_bucket}/{DENIED_PREFIX}*"),
        }],
    })
    .to_string();
    put_bucket_policy(router, data_bucket, &document).await;
}

/// The advanced event selector this suite's trail carries: the `Data` category, the object
/// resource type, and the data bucket's own object ARN prefix.
fn object_selector(data_bucket: &str) -> Value {
    json!([{
        "Name": "s3-objects-in-data-bucket",
        "FieldSelectors": [
            {"Field": "eventCategory", "Equals": [DATA_CATEGORY]},
            {"Field": "resources.type", "Equals": [OBJECT_RESOURCE_TYPE]},
            {"Field": "resources.ARN", "StartsWith": [format!("arn:aws:s3:::{data_bucket}/")]},
        ],
    }])
}

/// Point a trail at `log_bucket` under `key_prefix`, install `selectors` on it and start it
/// logging — the four calls a client makes, each of which must be served, and the status read
/// back so the rows below run against a trail that reports itself logging.
async fn logging_trail(
    router: &MultiServiceRouter,
    trail: &str,
    log_bucket: &str,
    key_prefix: &str,
    selectors: Value,
) {
    let created = cloudtrail(
        router,
        "CreateTrail",
        &json!({
            "Name": trail,
            "S3BucketName": log_bucket,
            "S3KeyPrefix": key_prefix,
            "IsMultiRegionTrail": false,
            "IncludeGlobalServiceEvents": false,
        }),
    )
    .await;
    let trail_arn = created["TrailARN"]
        .as_str()
        .unwrap_or_else(|| panic!("setup: CreateTrail must answer a TrailARN, got {created}"))
        .to_string();
    cloudtrail(
        router,
        "PutEventSelectors",
        &json!({"TrailName": trail_arn, "AdvancedEventSelectors": selectors}),
    )
    .await;
    cloudtrail(router, "StartLogging", &json!({"Name": trail_arn})).await;
    let status = cloudtrail(router, "GetTrailStatus", &json!({"Name": trail_arn})).await;
    assert_eq!(
        status.get("IsLogging"),
        Some(&json!(true)),
        "setup: the trail must report itself logging, got {status}"
    );
}

/// The whole delivery area of a trail created with `key_prefix`, which is where a consumer of
/// that trail looks for its files.
fn delivery_prefix(key_prefix: &str) -> String {
    format!("{key_prefix}/{LOG_FILE_ROOT_SEGMENT}/{ACCOUNT}/{LOG_FILE_SERVICE_SEGMENT}/")
}

/// Every object key `bucket` holds under `prefix`, as Amazon S3's own `ListObjectsV2` reports
/// them, sorted so two readings compare.
async fn keys_under(
    router: &MultiServiceRouter,
    case: &str,
    named: &str,
    prefix: &str,
) -> BTreeSet<String> {
    let listed = s3(
        router,
        ACCOUNT_KEY,
        "GET",
        &format!("/{named}?list-type=2&prefix={prefix}"),
        "",
    )
    .await;
    assert_eq!(
        listed.status, 200,
        "case {case:?}: listing {named}/{prefix} must be served, got {} with {}",
        listed.status, listed.text
    );
    elements(&listed.text, "Key").into_iter().collect()
}

/// The keys the trail has delivered once it holds at least `want` of them, or whatever it holds
/// when the deadline passes — so a loaded host waits rather than failing, and a row expecting
/// NO new file reads the one count there is.
async fn delivered_at_least(
    router: &MultiServiceRouter,
    case: &str,
    named: &str,
    prefix: &str,
    want: usize,
) -> BTreeSet<String> {
    let mut keys = keys_under(router, case, named, prefix).await;
    for _ in 0..LOOKS {
        if keys.len() >= want {
            break;
        }
        tokio::time::sleep(LOOK).await;
        keys = keys_under(router, case, named, prefix).await;
    }
    keys
}

/// Assert `key` is laid out the way a CloudTrail log file of a trail under `key_prefix` is: the
/// trail's own delivery area, then the Region, the day, and a file named for the account, the
/// service and the Region.
fn assert_log_file_key(case: &str, key: &str, key_prefix: &str) {
    let prefix = format!("{}{REGION}/", delivery_prefix(key_prefix));
    assert!(
        key.starts_with(&prefix),
        "case {case:?}: expected a log file under {prefix:?}, got {key:?}"
    );
    let segments: Vec<&str> = key[prefix.len()..].split('/').collect();
    assert_eq!(
        segments.len(),
        4,
        "case {case:?}: expected <year>/<month>/<day>/<file> under {prefix:?}, got {key:?}"
    );
    for (index, width) in [(0_usize, 4_usize), (1, 2), (2, 2)] {
        let segment = segments[index];
        assert!(
            segment.len() == width && segment.chars().all(|digit| digit.is_ascii_digit()),
            "case {case:?}: expected a {width}-digit date segment, got {segment:?} in {key:?}"
        );
    }
    let file = segments[3];
    let expected_start = format!("{ACCOUNT}_{LOG_FILE_SERVICE_SEGMENT}_{REGION}_");
    assert!(
        file.starts_with(&expected_start) && file.ends_with(LOG_FILE_SUFFIX),
        "case {case:?}: expected a file named {expected_start}<minute>_<unique>\
         {LOG_FILE_SUFFIX}, got {file:?}"
    );
}

/// Read the delivered object `key` back through S3's own API and assert it is the gzip stream a
/// log file is.
async fn assert_gzip_log_file(router: &MultiServiceRouter, case: &str, named: &str, key: &str) {
    let read = s3(router, ACCOUNT_KEY, "GET", &format!("/{named}/{key}"), "").await;
    assert_eq!(
        read.status, 200,
        "case {case:?}: reading the delivered {key:?} must be served, got {} with {}",
        read.status, read.text
    );
    assert!(
        read.bytes.starts_with(&GZIP_MAGIC),
        "case {case:?}: expected the delivered {key:?} to be a gzip stream beginning \
         {GZIP_MAGIC:02x?}, got {} byte(s) beginning {:02x?}",
        read.bytes.len(),
        read.bytes.iter().take(2).collect::<Vec<&u8>>()
    );
}

/// The sentence AWS refuses an object call with when an explicit `Deny` in the bucket's own
/// policy decides it: the principal that signed, the action it asked for, the object's ARN
/// double-quoted, and the clause naming which document withheld the grant.
fn resource_deny_message(session_arn: &str, action: &str, bucket: &str, key: &str) -> String {
    format!(
        "User: {session_arn} is not authorized to perform: {action} on resource: \"{}\" \
         with an explicit deny in a resource-based policy",
        object_arn(bucket, key)
    )
}

/// Which principal a row's call is made by.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Caller {
    /// The account's own credential, which no identity-based document bounds.
    Account,
    /// The assumed-role session STS minted, which the data bucket's policy denies under
    /// [`DENIED_PREFIX`].
    Session,
}

/// One row of the delivery table.
struct Row {
    case: &'static str,
    caller: Caller,
    method: &'static str,
    /// Whether the call addresses the bucket the trail's selector names; the other bucket is
    /// the bystander an ARN-scoped selector must not admit.
    subject_bucket: bool,
    /// What follows the bucket in the request's path: an object key, or a bucket-level query.
    addressed: &'static str,
    body: &'static str,
    status: u16,
    /// The error code and message the caller must be answered with, and nothing for a call that
    /// is served.
    refusal: Option<(&'static str, Refusal)>,
    /// How many log files the trail must have delivered for the call.
    delivered: usize,
}

/// How a row's expected refusal message is worded: a fixed sentence, or the resource-deny
/// sentence built from the session's own ARN and the object the call addressed.
#[derive(Clone, Copy)]
enum Refusal {
    /// One fixed sentence, carrying nothing of the request.
    Fixed(&'static str),
    /// The resource-policy denial of `action` on the object the row addressed.
    ResourceDeny(&'static str),
}

/// RULE: while a trail is logging, every call on the CONTENTS of one object an event selector of
/// that trail admits — served or refused, whichever principal made it — is delivered into that
/// trail's bucket as a gzipped log file under the documented key layout, and a call the trail's
/// selectors do not admit is delivered nowhere.
#[tokio::test]
async fn an_object_call_is_delivered_to_the_trail_whose_selector_admits_it() {
    let router = router();
    let (logs, data_bucket, other_bucket) = (
        "ct-object-audit-logs",
        "ct-object-audit-subject",
        "ct-object-audit-bystander",
    );
    let key_prefix = "obs/ct-objects";
    let prefix = delivery_prefix(key_prefix);
    delivery_bucket(&router, logs, key_prefix).await;
    create_bucket(&router, data_bucket).await;
    create_bucket(&router, other_bucket).await;
    let role_arn = role_allowed_on(&router, "ct-object-audit-reader", data_bucket).await;
    let (session_key, session_arn) = role_session(&router, &role_arn).await;
    // Seeded BEFORE the trail logs, so the setup contributes no log file: the row that asserts
    // the OBJECT-level denial has to address an object that is there (see [`seed_object`]).
    seed_object(&router, data_bucket, "denied/report.csv").await;
    deny_prefix_to(&router, data_bucket, &role_arn).await;
    logging_trail(
        &router,
        "ct-object-audit-trail",
        logs,
        key_prefix,
        object_selector(data_bucket),
    )
    .await;

    // The rows differ along the operation, the outcome, the principal that made the call, and
    // whether the trail's selector admits the call at all. The delete comes last of the rows
    // on `obj/held.csv`, so the reads above it address an object that is there.
    let rows: Vec<Row> = vec![
        Row {
            case: "a write of an object in the bucket the selector names",
            caller: Caller::Account,
            method: "PUT",
            subject_bucket: true,
            addressed: "/obj/held.csv",
            body: "region,amount\nus-east-1,17\n",
            status: 200,
            refusal: None,
            delivered: 1,
        },
        Row {
            case: "a read of that object",
            caller: Caller::Account,
            method: "GET",
            subject_bucket: true,
            addressed: "/obj/held.csv",
            body: "",
            status: 200,
            refusal: None,
            delivered: 1,
        },
        Row {
            case: "a metadata read of that object",
            caller: Caller::Account,
            method: "HEAD",
            subject_bucket: true,
            addressed: "/obj/held.csv",
            body: "",
            status: 200,
            refusal: None,
            delivered: 1,
        },
        Row {
            case: "a read of a key the bucket does not hold",
            caller: Caller::Account,
            method: "GET",
            subject_bucket: true,
            addressed: "/obj/absent.csv",
            body: "",
            status: 404,
            refusal: Some((
                "NoSuchKey",
                Refusal::Fixed("The specified key does not exist."),
            )),
            delivered: 1,
        },
        Row {
            case: "a read by the role session its own document allows",
            caller: Caller::Session,
            method: "GET",
            subject_bucket: true,
            addressed: "/obj/held.csv",
            body: "",
            status: 200,
            refusal: None,
            delivered: 1,
        },
        Row {
            case: "a read the bucket policy explicitly denies that session",
            caller: Caller::Session,
            method: "GET",
            subject_bucket: true,
            addressed: "/denied/report.csv",
            body: "",
            status: 403,
            refusal: Some((
                "AccessDenied",
                Refusal::ResourceDeny(GET_OBJECT_ACTION),
            )),
            delivered: 1,
        },
        Row {
            case: "a write the same statement denies that session",
            caller: Caller::Session,
            method: "PUT",
            subject_bucket: true,
            addressed: "/denied/upload.csv",
            body: "hush",
            status: 403,
            refusal: Some((
                "AccessDenied",
                Refusal::ResourceDeny(PUT_OBJECT_ACTION),
            )),
            delivered: 1,
        },
        Row {
            case: "a delete of the object in the bucket the selector names",
            caller: Caller::Account,
            method: "DELETE",
            subject_bucket: true,
            addressed: "/obj/held.csv",
            body: "",
            status: 204,
            refusal: None,
            delivered: 1,
        },
        Row {
            case: "a write in ANOTHER bucket, which the selector's ARN prefix leaves out",
            caller: Caller::Account,
            method: "PUT",
            subject_bucket: false,
            addressed: "/obj/held.csv",
            body: "elsewhere",
            status: 200,
            refusal: None,
            delivered: 0,
        },
        Row {
            case: "a listing of the same bucket, which is a call on no object's contents",
            caller: Caller::Account,
            method: "GET",
            subject_bucket: true,
            addressed: "?list-type=2",
            body: "",
            status: 200,
            refusal: None,
            delivered: 0,
        },
    ];

    let mut seen: BTreeSet<String> = BTreeSet::new();
    for row in &rows {
        let addressed_bucket = if row.subject_bucket {
            data_bucket
        } else {
            other_bucket
        };
        let access_key_id = match row.caller {
            Caller::Account => ACCOUNT_KEY,
            Caller::Session => session_key.as_str(),
        };
        let answered = s3(
            &router,
            access_key_id,
            row.method,
            &format!("/{addressed_bucket}{}", row.addressed),
            row.body,
        )
        .await;
        assert_eq!(
            answered.status, row.status,
            "case {:?}: expected HTTP {}, got {} with {}",
            row.case, row.status, answered.status, answered.text
        );
        match row.refusal {
            Some((code, message)) => {
                let expected = match message {
                    Refusal::Fixed(sentence) => sentence.to_string(),
                    Refusal::ResourceDeny(action) => resource_deny_message(
                        &session_arn,
                        action,
                        addressed_bucket,
                        row.addressed.trim_start_matches('/'),
                    ),
                };
                let answered_code = elements(&answered.text, "Code");
                let answered_message = elements(&answered.text, "Message");
                assert_eq!(
                    (
                        answered_code.first().map(String::as_str),
                        answered_message.first().map(String::as_str),
                    ),
                    (Some(code), Some(expected.as_str())),
                    "case {:?}: expected the refusal {code} / {expected:?}, got {}",
                    row.case,
                    answered.text
                );
            }
            None => assert!(
                elements(&answered.text, "Code").is_empty(),
                "case {:?}: a served call carries no error document, got {}",
                row.case,
                answered.text
            ),
        }

        let want = seen.len() + row.delivered;
        let delivered = delivered_at_least(&router, row.case, logs, &prefix, want).await;
        assert_eq!(
            delivered.len(),
            want,
            "case {:?}: expected the trail to hold {want} log file(s) after this call, got \
             {delivered:?}",
            row.case
        );
        for key in delivered.difference(&seen) {
            assert_log_file_key(row.case, key, key_prefix);
            assert_gzip_log_file(&router, row.case, logs, key).await;
        }
        seen = delivered;
    }

    assert_eq!(
        seen.len(),
        rows.iter().map(|row| row.delivered).sum::<usize>(),
        "every admitted call is delivered once and no other call is, got {seen:?}"
    );
}

/// RULE's other side: an object call is a DATA event, so it reaches the trail's bucket and NEVER
/// the event history a lookup reports — "data events are not logged by default when you create a
/// trail", and a lookup serves management and Insights events — while the account's management
/// calls stay in that history.
///
/// Served on a MULTI-THREADED runtime, as the emulator's own entrypoint runs: the record of a
/// call is handed from the operation that served it to the layer that delivers it, so a
/// hand-over that only held while one thread drove everything would pass the table above and
/// fail the moment a second worker existed.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_object_data_event_is_delivered_and_never_enters_event_history() {
    let router = router();
    let (logs, data_bucket) = ("ct-object-history-logs", "ct-object-history-subject");
    let key_prefix = "obs/ct-history";
    let prefix = delivery_prefix(key_prefix);
    delivery_bucket(&router, logs, key_prefix).await;
    create_bucket(&router, data_bucket).await;
    logging_trail(
        &router,
        "ct-object-history-trail",
        logs,
        key_prefix,
        object_selector(data_bucket),
    )
    .await;

    let case = "a trail logging every object data event of the bucket";
    for (method, body) in [("PUT", "ledger"), ("GET", "")] {
        let answered = s3(
            &router,
            ACCOUNT_KEY,
            method,
            &format!("/{data_bucket}/obj/history.csv"),
            body,
        )
        .await;
        assert_eq!(
            answered.status, 200,
            "case {case:?}: the {method} must be served, got {} with {}",
            answered.status, answered.text
        );
    }

    let delivered = delivered_at_least(&router, case, logs, &prefix, 2).await;
    assert_eq!(
        delivered.len(),
        2,
        "case {case:?}: expected one log file per object call, got {delivered:?}"
    );
    for key in &delivered {
        assert_log_file_key(case, key, key_prefix);
        assert_gzip_log_file(&router, case, logs, key).await;
    }

    let looked_up = cloudtrail(&router, "LookupEvents", &json!({})).await;
    let names: Vec<String> = looked_up["Events"]
        .as_array()
        .map(|events| {
            events
                .iter()
                .filter_map(|event| event["EventName"].as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    for object_call in ["PutObject", "GetObject"] {
        assert!(
            !names.contains(&object_call.to_string()),
            "case {case:?}: no object data event may reach event history, got {names:?}"
        );
    }
    assert!(
        names.contains(&"CreateTrail".to_string()),
        "case {case:?}: the account's management calls stay in event history, got {names:?}"
    );
}
