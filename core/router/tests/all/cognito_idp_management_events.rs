// SPDX-License-Identifier: CC0-1.0

//! Amazon Cognito user-pool administration in CloudTrail event history, across the services
//! that serve one audit question on one engine.
//!
//! RULE: a service that serves a management API call records it as a management event of
//! THAT service, whatever the call answered, so `cloudtrail:LookupEvents` reports the call
//! under its own `eventName` with the serving service's `eventSource` — the call that
//! succeeded, the one its handler refused and the one the authorization gate in front of
//! the operation refused, each joined to the response its caller got by the request id that
//! response named, and the served one naming the USER it addressed by the `sub` that user's
//! own read answers with. That is how a client answers "who read which user", alarms on
//! repeated probes for users that do not exist, and joins an SDK failure to its own audit
//! record.
//!
//! Only a cross-service suite can pin this: the record is written by the service that
//! SERVES the call (Amazon Cognito user pools, awsJson1_1), the refused caller's session is
//! minted by the token service against a role IAM holds, and the records are read back
//! through CloudTrail (awsJson1_1) — four services over ONE engine, as
//! `core/router/src/lib.rs` mounts them.
//!
//! Every call goes over the wire, on a router with request authentication off as every
//! suite in this crate runs it. Assembling the whole router costs stack (see
//! `boot_serve.rs`), so it is built — and the requests driven — on a sized thread.

use std::sync::Arc;

use aws_emulator_router::{build_router, InMemoryEngine};
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// Keep this at or above `boot_serve.rs`'s stack: the same assembly runs here.
const BOOT_STACK_SIZE: usize = 64 * 1024 * 1024;

/// The Region the calls below are signed for, and the Region their records are looked up
/// in.
const REGION: &str = "us-west-2";

/// The access key id the administration calls carry: a key IAM holds no record of, so it
/// resolves to the account's own identity, which carries no IAM policies and is allowed
/// every action.
const ACCOUNT_KEY: &str = "EMULATORTESTKEY12345";

/// The `eventSource` CloudTrail stamps on a management event of Amazon Cognito user pools.
const EVENT_SOURCE: &str = "cognito-idp.amazonaws.com";

/// The operation whose three outcomes this suite audits.
const OPERATION: &str = "AdminGetUser";

/// The literal CloudTrail stores in place of a member the serving API declares sensitive —
/// which a user's own name is.
const REDACTED: &str = "HIDDEN_DUE_TO_SECURITY_REASONS";

/// What one call answered.
struct Answer {
    status: u16,
    request_id: String,
    body: bytes::Bytes,
}

impl Answer {
    fn json(&self) -> Value {
        if self.body.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&self.body).unwrap_or(Value::Null)
        }
    }

    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).to_string()
    }
}

/// Dispatch one request signed for `signing_name` in [`REGION`] with `access_key_id` in the
/// credential scope.
fn dispatch(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    signing_name: &str,
    access_key_id: &str,
    headers: &[(&str, &str)],
    body: bytes::Bytes,
) -> Answer {
    let host = format!("{signing_name}.{REGION}.amazonaws.com");
    let mut builder = http::Request::builder()
        .method("POST")
        .uri(format!("https://{host}/"))
        .header("host", host.clone())
        .header(
            // The signature is never checked: this router runs with authentication off.
            // What the scope carries is the key id and the Region a handler reads.
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 \
                 Credential={access_key_id}/20260101/{REGION}/{signing_name}/aws4_request, \
                 SignedHeaders=host, Signature=not-verified"
            ),
        );
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    let request = builder.body(body).expect("a well-formed request");
    let response = runtime.block_on(router.dispatch(request));
    let request_id = response
        .headers()
        .get("x-amzn-RequestId")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string();
    let (parts, body) = response.into_parts();
    Answer {
        status: parts.status.as_u16(),
        request_id,
        body,
    }
}

/// One awsJson1_1 call to Amazon Cognito user pools, signed with `access_key_id`.
fn cognito(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    access_key_id: &str,
    operation: &str,
    body: &Value,
) -> Answer {
    dispatch(
        router,
        runtime,
        "cognito-idp",
        access_key_id,
        &[
            ("content-type", "application/x-amz-json-1.1"),
            (
                "x-amz-target",
                &format!("AWSCognitoIdentityProviderService.{operation}"),
            ),
        ],
        bytes::Bytes::from(body.to_string()),
    )
}

/// One awsQuery call, signed with the account's own credential, to the service
/// `signing_name` serves at `version`.
fn query(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
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
    let answered = dispatch(
        router,
        runtime,
        signing_name,
        ACCOUNT_KEY,
        &[("content-type", "application/x-www-form-urlencoded")],
        bytes::Bytes::from(body),
    );
    assert_eq!(
        answered.status,
        200,
        "{action} must be served, got {} with {:?}",
        answered.status,
        answered.text()
    );
    answered.text()
}

/// Percent-encode one `application/x-www-form-urlencoded` component, leaving only RFC
/// 3986's unreserved characters literal — so a policy document's braces and quotes survive
/// the round trip.
fn form_encode(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            encoded.push(char::from(byte));
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    encoded
}

/// The text of the first `<name>…</name>` element of an awsQuery response.
fn xml_element(body: &str, name: &str) -> String {
    let open = format!("<{name}>");
    let close = format!("</{name}>");
    match body.find(&open) {
        None => String::new(),
        Some(at) => {
            let start = at + open.len();
            match body[start..].find(&close) {
                None => String::new(),
                Some(end) => body[start..start + end].to_string(),
            }
        }
    }
}

/// `cloudtrail:LookupEvents` in [`REGION`], filtered on one attribute, answering each
/// reported event's envelope paired with its parsed `CloudTrailEvent` document.
fn lookup(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    attribute_key: &str,
    attribute_value: &str,
) -> Vec<(Value, Value)> {
    let answered = dispatch(
        router,
        runtime,
        "cloudtrail",
        ACCOUNT_KEY,
        &[
            ("content-type", "application/x-amz-json-1.1"),
            ("x-amz-target", "CloudTrail_20131101.LookupEvents"),
        ],
        bytes::Bytes::from(
            json!({
                "LookupAttributes": [{
                    "AttributeKey": attribute_key,
                    "AttributeValue": attribute_value,
                }],
                "MaxResults": 50,
            })
            .to_string(),
        ),
    );
    assert_eq!(
        answered.status,
        200,
        "LookupEvents({attribute_key}={attribute_value}) must succeed, got {} with {:?}",
        answered.status,
        answered.text()
    );
    answered.json()["Events"]
        .as_array()
        .unwrap_or_else(|| panic!("LookupEvents reports an Events list, got {}", answered.text()))
        .iter()
        .map(|event| {
            let raw = event["CloudTrailEvent"]
                .as_str()
                .unwrap_or_else(|| panic!("an event carries its CloudTrailEvent: {event}"));
            let document: Value = serde_json::from_str(raw)
                .unwrap_or_else(|error| panic!("CloudTrailEvent is JSON ({error}): {raw}"));
            (event.clone(), document)
        })
        .collect()
}

/// One of the three calls, and what its record must carry.
struct Recorded<'a> {
    case: &'a str,
    /// The id the caller was answered under: the join into event history.
    request_id: &'a str,
    /// The code the record carries, and `None` for the call that was served.
    error_code: Option<&'a str>,
    /// The message the record carries, which is the one the caller was answered with.
    error_message: Option<&'a str>,
    /// Whether the call was dispatched: a dispatched call's record carries the parameters
    /// it was dispatched with, and one stopped at authorization carries none.
    dispatched: bool,
    /// The `sub` the record names the addressed user by, for a call that addressed a user
    /// the pool holds.
    names_the_user: Option<&'a str>,
}

/// The one event of `records` that names request `request_id` — the join a client holding a
/// response makes.
fn joined_by_request_id(case: &str, records: &[(Value, Value)], request_id: &str) -> Value {
    let found: Vec<&Value> = records
        .iter()
        .map(|(_, document)| document)
        .filter(|document| document.get("requestID") == Some(&json!(request_id)))
        .collect();
    assert_eq!(
        found.len(),
        1,
        "case {case:?}: expected exactly one record naming request {request_id:?}, got \
         {found:?}"
    );
    found[0].clone()
}

#[test]
fn the_three_outcomes_of_an_admin_user_read_are_in_cloudtrail_event_history() {
    std::thread::Builder::new()
        .name("cognito-idp-management-events".to_string())
        .stack_size(BOOT_STACK_SIZE)
        .spawn(|| {
            let engine: Arc<InMemoryEngine> = Arc::new(InMemoryEngine::new());
            let mut router = build_router(engine);
            router.set_request_authentication(false);
            let runtime = tokio::runtime::Builder::new_current_thread()
                .build()
                .expect("a current-thread runtime");

            // A user pool holding one user, made with the account's own credential.
            let created_pool = cognito(
                &router,
                &runtime,
                ACCOUNT_KEY,
                "CreateUserPool",
                &json!({"PoolName": "audited-administration"}),
            );
            assert_eq!(
                created_pool.status,
                200,
                "the setup create must be served, got {} with {:?}",
                created_pool.status,
                created_pool.text()
            );
            let pool = created_pool.json();
            let user_pool_id = pool["UserPool"]["Id"]
                .as_str()
                .unwrap_or_else(|| panic!("CreateUserPool answers the pool's Id: {pool}"))
                .to_string();
            let pool_arn = pool["UserPool"]["Arn"]
                .as_str()
                .unwrap_or_else(|| panic!("CreateUserPool answers the pool's Arn: {pool}"))
                .to_string();
            let username = "audited-reader";
            let created_user = cognito(
                &router,
                &runtime,
                ACCOUNT_KEY,
                "AdminCreateUser",
                &json!({
                    "UserPoolId": user_pool_id,
                    "Username": username,
                    "MessageAction": "SUPPRESS",
                }),
            );
            assert_eq!(
                created_user.status,
                200,
                "the setup user create must be served, got {} with {:?}",
                created_user.status,
                created_user.text()
            );

            // A session of a role granted ONE other action of this service: the caller the
            // authorization gate refuses. The role and its policy are IAM's own records and
            // the session is the token service's own mint, both over this same engine.
            let role_name = "audited-reader-role";
            let trust = json!({
                "Version": "2012-10-17",
                "Statement": [{
                    "Effect": "Allow",
                    "Principal": {
                        "AWS": format!("arn:aws:iam::{}:root", engine::emulator_account_id()),
                    },
                    "Action": "sts:AssumeRole",
                }],
            })
            .to_string();
            let created_role = query(
                &router,
                &runtime,
                "iam",
                "2010-05-08",
                "CreateRole",
                &[
                    ("RoleName", role_name.to_string()),
                    ("AssumeRolePolicyDocument", trust),
                ],
            );
            let role_arn = xml_element(&created_role, "Arn");
            assert!(
                !role_arn.is_empty(),
                "CreateRole answers the role's Arn, got {created_role}"
            );
            query(
                &router,
                &runtime,
                "iam",
                "2010-05-08",
                "PutRolePolicy",
                &[
                    ("RoleName", role_name.to_string()),
                    ("PolicyName", "describe-only".to_string()),
                    (
                        "PolicyDocument",
                        json!({
                            "Version": "2012-10-17",
                            "Statement": [{
                                "Effect": "Allow",
                                "Action": "cognito-idp:DescribeUserPool",
                                "Resource": "*",
                            }],
                        })
                        .to_string(),
                    ),
                ],
            );
            let assumed = query(
                &router,
                &runtime,
                "sts",
                "2011-06-15",
                "AssumeRole",
                &[
                    ("RoleArn", role_arn.clone()),
                    ("RoleSessionName", "audited-session".to_string()),
                ],
            );
            let session_key = xml_element(&assumed, "AccessKeyId");
            let session_arn = xml_element(&assumed, "Arn");
            assert!(
                !session_key.is_empty() && !session_arn.is_empty(),
                "AssumeRole answers a session credential and the session's Arn, got {assumed}"
            );

            // The three calls the audit question is about: the control, the one whose user
            // does not exist, and the one IAM does not allow.
            let control = cognito(
                &router,
                &runtime,
                ACCOUNT_KEY,
                OPERATION,
                &json!({"UserPoolId": user_pool_id, "Username": username}),
            );
            assert_eq!(
                control.status,
                200,
                "the control read must be served, got {} with {:?}",
                control.status,
                control.text()
            );
            // The `sub` the read ANSWERED its caller: the id a record of a call on this user
            // names the user by, since the name the request used is withheld.
            let answered = control.json();
            let user_sub = answered["UserAttributes"]
                .as_array()
                .unwrap_or_else(|| panic!("AdminGetUser answers the user's attributes: {answered}"))
                .iter()
                .find(|attribute| attribute.get("Name") == Some(&json!("sub")))
                .and_then(|attribute| attribute.get("Value"))
                .and_then(Value::as_str)
                .unwrap_or_else(|| panic!("AdminGetUser answers the user's sub: {answered}"))
                .to_string();
            let failed = cognito(
                &router,
                &runtime,
                ACCOUNT_KEY,
                OPERATION,
                &json!({"UserPoolId": user_pool_id, "Username": "never-created"}),
            );
            let denied = cognito(
                &router,
                &runtime,
                &session_key,
                OPERATION,
                &json!({"UserPoolId": user_pool_id, "Username": username}),
            );

            // The API side first, because a record can only carry what the caller was told.
            assert_eq!(
                (
                    failed.json()["__type"].as_str().map(str::to_string),
                    failed.json()["message"].as_str().map(str::to_string),
                ),
                (
                    Some("UserNotFoundException".to_string()),
                    Some("User does not exist.".to_string()),
                ),
                "case \"the unknown user\": expected the service's own refusal, got {} with \
                 {:?}",
                failed.status,
                failed.text()
            );
            let denial = format!(
                "User: {session_arn} is not authorized to perform: cognito-idp:{OPERATION} on \
                 resource: {pool_arn} because no identity-based policy allows the \
                 cognito-idp:{OPERATION} action"
            );
            assert_eq!(
                (
                    denied.json()["__type"].as_str().map(str::to_string),
                    denied.json()["message"].as_str().map(str::to_string),
                ),
                (
                    Some("AccessDeniedException".to_string()),
                    Some(denial.clone()),
                ),
                "case \"the unauthorized session\": expected the authorization refusal, got \
                 {} with {:?}",
                denied.status,
                denied.text()
            );
            let named: Vec<&str> = vec![
                control.request_id.as_str(),
                failed.request_id.as_str(),
                denied.request_id.as_str(),
            ];
            assert!(
                named.iter().all(|id| !id.is_empty()),
                "each response names the request it answers, got {named:?}"
            );
            let mut distinct = named.clone();
            distinct.sort_unstable();
            distinct.dedup();
            assert_eq!(
                distinct.len(),
                3,
                "the three calls are answered under three ids, got {named:?}"
            );

            // Found the way a client finds the records of its own calls: a lookup by event
            // name over the account's event history.
            let records = lookup(&router, &runtime, "EventName", OPERATION);
            assert_eq!(
                records.len(),
                3,
                "exactly three {OPERATION} calls were made, got {records:?}"
            );

            let rows = [
                Recorded {
                    case: "the control",
                    request_id: &control.request_id,
                    error_code: None,
                    error_message: None,
                    dispatched: true,
                    names_the_user: Some(user_sub.as_str()),
                },
                Recorded {
                    case: "the unknown user",
                    request_id: &failed.request_id,
                    error_code: Some("UserNotFoundException"),
                    error_message: Some("User does not exist."),
                    dispatched: true,
                    names_the_user: None,
                },
                Recorded {
                    case: "the unauthorized session",
                    request_id: &denied.request_id,
                    // The RECORD of an IAM denial carries the authorization layer's own
                    // code, not the error class the caller was answered with
                    // (`AccessDeniedException`, asserted on the wire above).
                    error_code: Some("AccessDenied"),
                    error_message: Some(denial.as_str()),
                    dispatched: false,
                    names_the_user: None,
                },
            ];
            for row in rows {
                let Recorded {
                    case,
                    request_id,
                    error_code,
                    error_message,
                    dispatched,
                    names_the_user,
                } = row;
                let record = joined_by_request_id(case, &records, request_id);
                assert_eq!(
                    (
                        record.get("eventName"),
                        record.get("eventSource"),
                        record.get("readOnly"),
                        record.get("awsRegion"),
                        record.get("recipientAccountId"),
                        record.get("eventType"),
                    ),
                    (
                        Some(&json!(OPERATION)),
                        Some(&json!(EVENT_SOURCE)),
                        // A user read is a read whether or not it succeeded.
                        Some(&json!(true)),
                        Some(&json!(REGION)),
                        Some(&json!(engine::emulator_account_id())),
                        Some(&json!("AwsApiCall")),
                    ),
                    "case {case:?}: expected the call's own record, got {record}"
                );
                assert_eq!(
                    record.get("errorCode"),
                    error_code.map(|code| json!(code)).as_ref(),
                    "case {case:?}: expected error code {error_code:?}, got {record}"
                );
                assert_eq!(
                    record.get("errorMessage"),
                    error_message.map(|message| json!(message)).as_ref(),
                    "case {case:?}: expected the caller's own message {error_message:?}, got \
                     {record}"
                );
                if dispatched {
                    assert_eq!(
                        (
                            record.pointer("/requestParameters/userPoolId"),
                            record.pointer("/requestParameters/username"),
                        ),
                        (Some(&json!(user_pool_id)), Some(&json!(REDACTED))),
                        "case {case:?}: a dispatched call records the pool it addressed and \
                         withholds the user's name, got {record}"
                    );
                } else {
                    assert_eq!(
                        record.get("requestParameters"),
                        Some(&Value::Null),
                        "case {case:?}: a call stopped at authorization records no \
                         parameters, got {record}"
                    );
                }
                assert_eq!(
                    record.get("responseElements"),
                    Some(&Value::Null),
                    "case {case:?}: CloudTrail records a read with no response elements, got \
                     {record}"
                );
                assert_eq!(
                    record.pointer("/additionalEventData/sub"),
                    names_the_user.map(|sub| json!(sub)).as_ref(),
                    "case {case:?}: expected the record to name the addressed user by \
                     {names_the_user:?}, got {record}"
                );
            }

            // The refused session is recorded as the session it was made by, which is how a
            // client tells a denied attempt by one principal from another's.
            let refused = joined_by_request_id(
                "the unauthorized session",
                &records,
                &denied.request_id,
            );
            assert_eq!(
                (
                    refused.pointer("/userIdentity/type"),
                    refused.pointer("/userIdentity/arn"),
                    refused.pointer("/userIdentity/accessKeyId"),
                ),
                (
                    Some(&json!("AssumedRole")),
                    Some(&json!(session_arn)),
                    Some(&json!(session_key)),
                ),
                "case \"the unauthorized session\": expected the session that made the call, \
                 got {refused}"
            );
            assert_eq!(
                refused.pointer("/userIdentity/sessionContext/sessionIssuer/arn"),
                Some(&json!(role_arn)),
                "case \"the unauthorized session\": expected the role that issued it, got \
                 {refused}"
            );
        })
        .expect("spawn the cognito-idp-management-events thread")
        .join()
        .expect("the three outcomes of an admin user read must be in event history");
}
