// SPDX-License-Identifier: CC0-1.0

//! The AUDIT leg of this service: every call it serves is a management event of
//! `cognito-idp.amazonaws.com`, recorded in the account's event history where CloudTrail's
//! `LookupEvents` reports it.
//!
//! RULE: a service that serves a control-plane API call records that call in the account's
//! shared management-event log, whatever the call ANSWERED — the operation's own name as
//! `eventName`, this service's `eventSource`, the Region the call was served in, the
//! account as `recipientAccountId`, `readOnly` telling a call that only reports state from
//! one that changes it, the parameters the request carried (and, for a mutating call, the
//! values it answered with), and for a failed call the error code and the message its
//! caller was answered with. The record names its call by the request id the CALLER was
//! answered under, which is what lets a client holding a response find the record of that
//! very call.
//!
//! RULE, for a call refused before it was dispatched: the refusal the authorization gate
//! answers is recorded like any other outcome, and its record carries NO
//! `requestParameters` — those report what a call that was authorized and dispatched
//! carried, and such a call was neither.
//!
//! RULE, for the credential material these calls carry: a member this model declares
//! `@sensitive` is recorded with CloudTrail's own redaction literal in place of the value,
//! so the record shows that the call carried the member while the value itself reaches no
//! record. A user's name and every user ATTRIBUTE value are among those members, which is
//! why the record of a call on one user states that a user was named without stating which
//! name was used — and why it names the user the call addressed by the `sub` the pool minted
//! for that user, the id the pool answers its own readers with.
//!
//! The table below spans what those rules generalise over rather than the one call a
//! recording made: the three OUTCOMES of one operation (served, refused by the handler,
//! refused by the authorization gate), both read/write KINDS, and an operation that
//! answers nothing at all.
//!
//! Every call goes over the wire through the same `MultiServiceRouter` the production
//! router uses; the records are read back out of the SHARED log in the engine the service
//! was built from — the one store CloudTrail's `LookupEvents` reads.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use service_common::management_events::{ManagementEvent, ManagementEventLog};
use service_com_amazonaws_cognitoidentityprovider_awscognitoidentityproviderservice as cognito_idp;
use smithy_server_runtime::MultiServiceRouter;

use crate::iam_authorization::{form_encode, xml_element};

/// The Region every call below is signed for: the Region its record is filed in.
const REGION: &str = "eu-west-1";

/// The access-key id standing for the account's own published credential, which carries no
/// IAM policies and is allowed every call (the position AWS account root credentials are
/// in).
const ACCOUNT_KEY: &str = "EMULATORTESTKEY12345";

/// The `eventSource` CloudTrail stamps on a management event of this service.
const EVENT_SOURCE: &str = "cognito-idp.amazonaws.com";

/// The literal CloudTrail stores in place of a member the serving API declares sensitive.
const REDACTED: &str = "HIDDEN_DUE_TO_SECURITY_REASONS";

/// The error code an identity-based authorization refusal carries on the WIRE: the AWS
/// common error this endpoint answers its caller with.
const ACCESS_DENIED: &str = "AccessDeniedException";

/// The error code the RECORD of that refusal carries: the authorization layer's own code,
/// which is not the one the caller was answered with.
const RECORDED_ACCESS_DENIED: &str = "AccessDenied";

/// The temporary password the created user is given: a value this suite proves reaches no
/// record.
const TEMPORARY_PASSWORD: &str = "Str0ng-Temp-Pass!42";

/// What one call answered: the status, the id the response named its request by, and the
/// parsed body.
struct Answer {
    status: u16,
    request_id: String,
    body: Value,
}

/// One emulator: this service beside the IAM service the refused principal's policies are
/// written with, both on ONE shared engine — the engine carrying the management-event log.
struct Audited {
    router: MultiServiceRouter,
    engine: Arc<engine::InMemoryEngine>,
}

impl Audited {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        // The suite signs nothing: the `Authorization` header carries only the credential
        // scope, which is where a service reads the caller's access key id and Region from.
        router.set_request_authentication(false);
        router.register_service(cognito_idp::EmulatedService::new(Arc::clone(&engine)));
        router.register_service(
            service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(
                Arc::clone(&engine),
            ),
        );
        Self { router, engine }
    }

    /// One call to this service, with `access_key_id` in the SigV4 credential scope.
    async fn call(&self, access_key_id: &str, operation: &str, body: &Value) -> Answer {
        let request = http::Request::builder()
            .method("POST")
            .uri("/")
            .header("host", format!("cognito-idp.{REGION}.amazonaws.com"))
            .header("content-type", "application/x-amz-json-1.1")
            .header(
                "x-amz-target",
                format!("AWSCognitoIdentityProviderService.{operation}"),
            )
            .header(
                "authorization",
                format!(
                    "AWS4-HMAC-SHA256 Credential={access_key_id}/20260101/{REGION}/\
                     cognito-idp/aws4_request, SignedHeaders=host;x-amz-target, Signature=0"
                ),
            )
            .body(Bytes::from(body.to_string()))
            .expect("the request is well formed");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let request_id = response
            .headers()
            .get("x-amzn-RequestId")
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_string();
        let body = if response.body().is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(response.body()).unwrap_or(Value::Null)
        };
        Answer {
            status,
            request_id,
            body,
        }
    }

    /// One awsQuery setup call to IAM, signed with the account's own credential.
    async fn iam(&self, action: &str, params: &[(&str, String)]) -> String {
        let mut body = format!("Action={action}&Version=2010-05-08");
        for (name, value) in params {
            body.push('&');
            body.push_str(name);
            body.push('=');
            body.push_str(&form_encode(value));
        }
        let request = http::Request::builder()
            .method("POST")
            .uri("https://iam.amazonaws.com/")
            .header("content-type", "application/x-www-form-urlencoded")
            .header(
                "authorization",
                format!(
                    "AWS4-HMAC-SHA256 Credential={ACCOUNT_KEY}/20260101/{REGION}/iam/\
                     aws4_request, SignedHeaders=host, Signature=0"
                ),
            )
            .body(Bytes::from(body))
            .expect("a well-formed awsQuery request");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let text = String::from_utf8_lossy(response.body()).to_string();
        assert_eq!(
            status, 200,
            "case {action:?}: the setup call must succeed, got {status} with {text}"
        );
        text
    }

    /// Create an IAM user carrying `policy` as its inline policy, mint a long-term access
    /// key for it, and answer that key id: a principal whose policies the gate reads.
    async fn user(&self, user_name: &str, policy: &str) -> String {
        self.iam("CreateUser", &[("UserName", user_name.to_string())])
            .await;
        self.iam(
            "PutUserPolicy",
            &[
                ("UserName", user_name.to_string()),
                ("PolicyName", "inline".to_string()),
                ("PolicyDocument", policy.to_string()),
            ],
        )
        .await;
        let minted = self
            .iam("CreateAccessKey", &[("UserName", user_name.to_string())])
            .await;
        xml_element(&minted, "AccessKeyId")
            .unwrap_or_else(|| panic!("CreateAccessKey answers an AccessKeyId, got {minted}"))
    }

    /// Every record in the shared log, oldest first, as the CloudTrail event document a
    /// lookup reports.
    fn records(&self) -> Vec<Value> {
        let log = self.engine.state::<ManagementEventLog>();
        let held = log.lock();
        held.iter()
            .map(ManagementEvent::to_cloudtrail_event_json)
            .collect()
    }

    /// The record of the call the caller was answered under `request_id`, which is the join
    /// a client holding a response makes.
    fn record_of(&self, case: &str, request_id: &str) -> Value {
        let records = self.records();
        let found: Vec<&Value> = records
            .iter()
            .filter(|event| event.get("requestID") == Some(&json!(request_id)))
            .collect();
        assert_eq!(
            found.len(),
            1,
            "case {case:?}: expected exactly one record naming request {request_id:?}, got \
             {found:?}"
        );
        found[0].clone()
    }

    /// Every record as one block of JSON text, for asserting that a secret is in none of
    /// them.
    fn whole_log(&self) -> String {
        self.records()
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<String>>()
            .join("\n")
    }
}

/// A policy document allowing `action` on everything and nothing else.
fn allow_only(action: &str) -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{"Effect": "Allow", "Action": action, "Resource": "*"}],
    })
    .to_string()
}

/// One row: a call made against the pool, and what its record must carry.
struct Row {
    case: &'static str,
    /// The operation the call names, which is the record's `eventName`.
    operation: &'static str,
    /// Whether the call is signed with the account's own credential (allowed every action)
    /// or with the narrowly-granted principal the gate refuses.
    allowed_caller: bool,
    /// The request body, built against the pool and the user the suite created.
    body: Value,
    /// The status the caller is answered with.
    status: u16,
    /// Whether the operation only REPORTS state — the record's `readOnly`.
    read_only: bool,
    /// The error code the record must carry, and `None` for a call that was served.
    error_code: Option<&'static str>,
    /// Whether the record carries the parameters the call was dispatched with: every
    /// dispatched call does, and a call stopped at authorization does not.
    dispatched: bool,
    /// Whether the record carries what the call answered with: a mutating call that
    /// succeeded does, and nothing else does.
    answered: bool,
    /// Whether the record names the USER the request addressed by that user's own `sub`:
    /// every call that names a pool and a user the pool holds does, and a call that names
    /// no user — or one the pool does not hold — does not.
    names_the_user: bool,
}

/// The `sub` the pool minted for the user, out of the attributes a call answered with.
fn answered_sub(case: &str, answered: &Value) -> String {
    let attributes = answered
        .pointer("/User/Attributes")
        .or_else(|| answered.get("UserAttributes"))
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("case {case:?}: expected the user's attributes: {answered}"));
    attributes
        .iter()
        .find(|attribute| attribute.get("Name") == Some(&json!("sub")))
        .and_then(|attribute| attribute.get("Value"))
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: expected the user's sub: {answered}"))
        .to_string()
}

/// RULE, over every outcome and both read/write kinds: the call is recorded as a management
/// event of this service, named by the request id its caller was answered under.
#[tokio::test]
async fn every_served_call_is_recorded_as_a_management_event_of_this_service() {
    let emulator = Audited::new();

    // The pool and the user the rows below address, created through this service's own
    // operations — so the records of those creates are in the log too.
    let created_pool = emulator
        .call(ACCOUNT_KEY, "CreateUserPool", &json!({"PoolName": "audited"}))
        .await;
    assert_eq!(
        created_pool.status, 200,
        "the setup create must be served, got {} with {}",
        created_pool.status, created_pool.body
    );
    let user_pool_id = created_pool.body["UserPool"]["Id"]
        .as_str()
        .unwrap_or_else(|| panic!("CreateUserPool answers the pool's Id: {}", created_pool.body))
        .to_string();

    // A principal granted exactly one OTHER action of this service: the caller the gate
    // refuses, as a client reaching for a user read it was not granted.
    let narrow = emulator
        .user("audit-narrow", &allow_only("cognito-idp:DescribeUserPool"))
        .await;

    let known_user = "audited-user";
    let rows = vec![
        Row {
            case: "a user created",
            operation: "AdminCreateUser",
            allowed_caller: true,
            body: json!({
                "UserPoolId": user_pool_id,
                "Username": known_user,
                "TemporaryPassword": TEMPORARY_PASSWORD,
                "MessageAction": "SUPPRESS",
            }),
            status: 200,
            read_only: false,
            error_code: None,
            dispatched: true,
            answered: true,
            names_the_user: true,
        },
        Row {
            case: "the user read",
            operation: "AdminGetUser",
            allowed_caller: true,
            body: json!({"UserPoolId": user_pool_id, "Username": known_user}),
            status: 200,
            read_only: true,
            error_code: None,
            dispatched: true,
            answered: false,
            names_the_user: true,
        },
        Row {
            case: "a read of a user the pool does not hold",
            operation: "AdminGetUser",
            allowed_caller: true,
            body: json!({"UserPoolId": user_pool_id, "Username": "never-created"}),
            status: 400,
            read_only: true,
            error_code: Some("UserNotFoundException"),
            dispatched: true,
            answered: false,
            names_the_user: false,
        },
        Row {
            case: "a read IAM does not allow this caller",
            operation: "AdminGetUser",
            allowed_caller: false,
            body: json!({"UserPoolId": user_pool_id, "Username": known_user}),
            status: 400,
            read_only: true,
            error_code: Some(RECORDED_ACCESS_DENIED),
            dispatched: false,
            answered: false,
            names_the_user: false,
        },
        Row {
            case: "the pool read",
            operation: "DescribeUserPool",
            allowed_caller: true,
            body: json!({"UserPoolId": user_pool_id}),
            status: 200,
            read_only: true,
            error_code: None,
            dispatched: true,
            answered: false,
            names_the_user: false,
        },
        Row {
            case: "a write that answers nothing",
            operation: "AdminDisableUser",
            allowed_caller: true,
            body: json!({"UserPoolId": user_pool_id, "Username": known_user}),
            status: 200,
            read_only: false,
            error_code: None,
            dispatched: true,
            answered: true,
            names_the_user: true,
        },
        Row {
            case: "a write the handler refused",
            operation: "AdminDeleteUser",
            allowed_caller: true,
            body: json!({"UserPoolId": user_pool_id, "Username": "never-created"}),
            status: 400,
            read_only: false,
            error_code: Some("UserNotFoundException"),
            dispatched: true,
            answered: false,
            names_the_user: false,
        },
    ];

    let mut named_requests: Vec<String> = Vec::new();
    // The `sub` the pool minted for the user, read off the first row's own answer — the
    // create that made it — and then expected in the record of every call that addresses
    // that user.
    let mut user_sub: Option<String> = None;
    for row in &rows {
        let caller = if row.allowed_caller {
            ACCOUNT_KEY
        } else {
            narrow.as_str()
        };
        let answered = emulator.call(caller, row.operation, &row.body).await;
        assert_eq!(
            answered.status, row.status,
            "case {:?}: expected status {}, got {} with {}",
            row.case, row.status, answered.status, answered.body
        );
        assert!(
            !answered.request_id.is_empty(),
            "case {:?}: the response names the request it answers, got {:?}",
            row.case,
            answered.body
        );
        named_requests.push(answered.request_id.clone());
        match row.operation {
            // The create answers the user it made, including the `sub` the pool minted for
            // it: the id every record of a call on that user must name it by.
            "AdminCreateUser" => user_sub = Some(answered_sub(row.case, &answered.body)),
            // And the read answers that same id to its own caller, which is what makes the
            // record's `sub` the one a client holding the response can join on.
            "AdminGetUser" if row.error_code.is_none() => assert_eq!(
                Some(answered_sub(row.case, &answered.body)),
                user_sub,
                "case {:?}: expected the read to answer the sub the create did, got {}",
                row.case,
                answered.body
            ),
            _ => {}
        }

        let record = emulator.record_of(row.case, &answered.request_id);
        assert_eq!(
            (
                record.get("eventName"),
                record.get("eventSource"),
                record.get("readOnly"),
                record.get("awsRegion"),
                record.get("eventType"),
                record.get("recipientAccountId"),
            ),
            (
                Some(&json!(row.operation)),
                Some(&json!(EVENT_SOURCE)),
                Some(&json!(row.read_only)),
                Some(&json!(REGION)),
                Some(&json!("AwsApiCall")),
                Some(&json!(::engine::emulator_account_id())),
            ),
            "case {:?}: expected the call's own record, got {record}",
            row.case
        );
        assert_eq!(
            record.get("errorCode"),
            row.error_code.map(|code| json!(code)).as_ref(),
            "case {:?}: expected error code {:?}, got {record}",
            row.case,
            row.error_code
        );
        // The gate's refusal is the one call whose record names a code the caller was NOT
        // answered with: an SDK branches on the API's own error class, a detection rule
        // matches on the code the authorization layer records.
        if !row.allowed_caller {
            assert_eq!(
                answered.body.get("__type"),
                Some(&json!(ACCESS_DENIED)),
                "case {:?}: expected the caller to be answered {ACCESS_DENIED}, got {}",
                row.case,
                answered.body
            );
        }
        // The message the record carries is the one the CALLER was answered with, down to
        // the byte: a client joining an SDK failure to its audit record reads one text.
        let answered_message = answered.body.get("message").cloned();
        assert_eq!(
            record.get("errorMessage").cloned(),
            answered_message,
            "case {:?}: expected the record to carry the caller's own message {:?}, got \
             {record}",
            row.case,
            answered_message
        );
        if row.dispatched {
            assert_eq!(
                record.pointer("/requestParameters/userPoolId"),
                Some(&json!(user_pool_id)),
                "case {:?}: a dispatched call records the pool it addressed, got {record}",
                row.case
            );
        } else {
            assert_eq!(
                record.get("requestParameters"),
                Some(&Value::Null),
                "case {:?}: a call stopped at authorization records no parameters, got \
                 {record}",
                row.case
            );
        }
        let elements = record.get("responseElements");
        if row.answered {
            assert_ne!(
                elements,
                Some(&Value::Null),
                "case {:?}: a mutating call records what it answered, got {record}",
                row.case
            );
        } else {
            assert_eq!(
                elements,
                Some(&Value::Null),
                "case {:?}: a read, and a call that failed, record no response elements, \
                 got {record}",
                row.case
            );
        }
        // WHICH user the call addressed: the name the request used is withheld, so the
        // record identifies the user by the `sub` the pool holds for it.
        let named_user = record.pointer("/additionalEventData/sub");
        if row.names_the_user {
            let expected = user_sub
                .as_deref()
                .expect("the create row runs first and answers the user's sub");
            assert_eq!(
                named_user,
                Some(&json!(expected)),
                "case {:?}: expected the record to name the addressed user by its sub \
                 {expected:?}, got {record}",
                row.case
            );
        } else {
            assert_eq!(
                named_user, None,
                "case {:?}: a call that addresses no stored user names none, got {record}",
                row.case
            );
        }
    }

    // Three calls to ONE operation are three records, told apart by the id each caller was
    // answered under — which is what the per-row lookup above rests on.
    named_requests.sort();
    named_requests.dedup();
    assert_eq!(
        named_requests.len(),
        rows.len(),
        "each call is answered under an id of its own, got {named_requests:?}"
    );
}

/// RULE: a member this model declares sensitive is recorded with CloudTrail's redaction
/// literal and never with its value — on the request side and on the response side both.
#[tokio::test]
async fn a_sensitive_member_reaches_a_record_as_the_redaction_literal() {
    let emulator = Audited::new();
    let created_pool = emulator
        .call(ACCOUNT_KEY, "CreateUserPool", &json!({"PoolName": "redacted"}))
        .await;
    let user_pool_id = created_pool.body["UserPool"]["Id"]
        .as_str()
        .unwrap_or_else(|| panic!("CreateUserPool answers the pool's Id: {}", created_pool.body))
        .to_string();

    let username = "redaction-user";
    let email = "redaction-user@example.test";
    let created = emulator
        .call(
            ACCOUNT_KEY,
            "AdminCreateUser",
            &json!({
                "UserPoolId": user_pool_id,
                "Username": username,
                "TemporaryPassword": TEMPORARY_PASSWORD,
                "MessageAction": "SUPPRESS",
                "UserAttributes": [{"Name": "email", "Value": email}],
            }),
        )
        .await;
    assert_eq!(
        created.status, 200,
        "the create must be served, got {} with {}",
        created.status, created.body
    );
    let record = emulator.record_of("the create", &created.request_id);

    // (case, where the record holds the member)
    let withheld: &[(&str, &str)] = &[
        ("the user's own name", "/requestParameters/username"),
        (
            "a user attribute's value",
            "/requestParameters/userAttributes/0/value",
        ),
        ("the temporary password", "/requestParameters/temporaryPassword"),
    ];
    for (case, pointer) in withheld {
        assert_eq!(
            record.pointer(pointer),
            Some(&json!(REDACTED)),
            "case {case:?}: expected {pointer} to carry the redaction literal, got {record}"
        );
    }
    // The member that is NOT sensitive is recorded as the call carried it, so the record
    // still says WHICH attribute was set.
    assert_eq!(
        record.pointer("/requestParameters/userAttributes/0/name"),
        Some(&json!("email")),
        "case \"a user attribute's name\": expected the name the call carried, got {record}"
    );

    let log = emulator.whole_log();
    for (case, value) in [
        ("the temporary password", TEMPORARY_PASSWORD),
        ("the user's name", username),
        ("the user's email address", email),
    ] {
        assert!(
            !log.contains(value),
            "case {case:?}: no record may carry it, got {log}"
        );
    }
}
