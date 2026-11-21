// SPDX-License-Identifier: CC0-1.0

//! The AUDIT leg of a call on ONE user, for the calls that leave the pool holding a
//! different set of users than it held when they arrived.
//!
//! RULE: a management record of a call that addresses one user of a pool names that user by
//! the `sub` the pool holds for it, whatever the call then did to the user — so a call that
//! CREATED the user names the user it made, and a call that REMOVED the user names the user
//! it removed, which is the only identifier such a record can carry because the name the
//! request used is a value these records withhold. The id is read from pool state, so the
//! record of a create and the record of a later call on that same user carry the SAME `sub`
//! — the only field a subscriber can correlate the two by.
//!
//! The table spans what the rule generalises over: which SIDE of the call the user is
//! resolvable from (only after it, for a create; both sides, for a call that leaves the user
//! in place, changes its attributes or removes one of them; only before it, for a call that
//! removes the user), and the spelling the request names the user by against the pool's
//! case-sensitivity. A call naming a user the pool does not hold on either side is the row
//! that must name no user at all.
//!
//! Every call goes over the wire through the same `MultiServiceRouter` the production router
//! uses, and the records are read back out of the shared management-event log in the engine
//! the service was built from — the one store CloudTrail's `LookupEvents` reads and the one
//! the "AWS API Call via CloudTrail" event is delivered from.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Map, Value};
use service_common::management_events::{ManagementEvent, ManagementEventLog};
use service_com_amazonaws_cognitoidentityprovider_awscognitoidentityproviderservice as cognito_idp;
use smithy_server_runtime::MultiServiceRouter;

/// The Region every call below is signed for.
const REGION: &str = "eu-central-1";

/// A temporary password satisfying a default pool's password policy. A value this suite
/// never expects in a record.
const TEMPORARY_PASSWORD: &str = "Str0ng-Temp-Pass!42";

/// What one call answered: the id the response named its request by, the status and the
/// parsed body.
struct Answer {
    status: u16,
    request_id: String,
    body: Value,
}

/// One emulator with just this service mounted, on the engine carrying the shared
/// management-event log.
struct Audited {
    router: MultiServiceRouter,
    engine: Arc<engine::InMemoryEngine>,
}

impl Audited {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        // The suite signs nothing: the `Authorization` header carries only the credential
        // scope, which is where a service reads the caller's Region from.
        router.set_request_authentication(false);
        router.register_service(cognito_idp::EmulatedService::new(Arc::clone(&engine)));
        Self { router, engine }
    }

    /// One call to this service, as an SDK drives it.
    async fn call(&self, operation: &str, body: &Value) -> Answer {
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
                    "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/cognito-idp/\
                     aws4_request, SignedHeaders=host;x-amz-target, Signature=0"
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

    /// One call that must be served, answered as its body.
    async fn served(&self, case: &str, operation: &str, body: &Value) -> Answer {
        let answered = self.call(operation, body).await;
        assert_eq!(
            answered.status, 200,
            "case {case:?}: {operation} must be served, got {} with {}",
            answered.status, answered.body
        );
        answered
    }

    /// The record of the call its caller was answered under `request_id`, which is the join
    /// a client holding a response makes.
    fn record_of(&self, case: &str, request_id: &str) -> Value {
        let log = self.engine.state::<ManagementEventLog>();
        let held = log.lock();
        let found: Vec<Value> = held
            .iter()
            .map(ManagementEvent::to_cloudtrail_event_json)
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

/// One row: a call on one user, and what its record must name that user by.
struct Row {
    case: &'static str,
    /// The operation under test, whose record must name the addressed user.
    operation: &'static str,
    /// `UsernameConfiguration.CaseSensitive` of the pool this row runs in, or `None` for a
    /// pool that configures none.
    case_sensitive: Option<bool>,
    /// The name the SETUP creates the user under, or `None` for a row that creates no user
    /// before the call (the create itself, and the call on a user nothing holds).
    stored_username: Option<&'static str>,
    /// The name the call under test addresses the user by, which may be another spelling of
    /// the stored one.
    request_username: &'static str,
    /// The request members the operation needs beyond `UserPoolId` and `Username`.
    extra: Value,
    /// The status the call under test is answered with.
    status: u16,
    /// Whether the record must name the user the call addressed.
    names_the_user: bool,
    /// Whether the pool no longer holds that user once the call is served — the rows whose
    /// `sub` is resolvable only from the state the call arrived at.
    removes_the_user: bool,
}

/// Every row of the table, built fresh so each one has a pool and a user of its own.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "the call created the user",
            operation: "AdminCreateUser",
            case_sensitive: None,
            stored_username: None,
            request_username: "created-user",
            extra: json!({"TemporaryPassword": TEMPORARY_PASSWORD, "MessageAction": "SUPPRESS"}),
            status: 200,
            names_the_user: true,
            removes_the_user: false,
        },
        Row {
            case: "the call left the user where it was",
            operation: "AdminDisableUser",
            case_sensitive: None,
            stored_username: Some("disabled-user"),
            request_username: "disabled-user",
            extra: json!({}),
            status: 200,
            names_the_user: true,
            removes_the_user: false,
        },
        Row {
            case: "the call changed the user's attributes",
            operation: "AdminUpdateUserAttributes",
            case_sensitive: None,
            stored_username: Some("updated-user"),
            request_username: "updated-user",
            extra: json!({"UserAttributes": [{"Name": "nickname", "Value": "renamed"}]}),
            status: 200,
            names_the_user: true,
            removes_the_user: false,
        },
        Row {
            case: "the call removed one of the user's attributes",
            operation: "AdminDeleteUserAttributes",
            case_sensitive: None,
            stored_username: Some("trimmed-user"),
            request_username: "trimmed-user",
            extra: json!({"UserAttributeNames": ["nickname"]}),
            status: 200,
            names_the_user: true,
            removes_the_user: false,
        },
        Row {
            case: "the call removed the user",
            operation: "AdminDeleteUser",
            case_sensitive: None,
            stored_username: Some("deleted-user"),
            request_username: "deleted-user",
            extra: json!({}),
            status: 200,
            names_the_user: true,
            removes_the_user: true,
        },
        Row {
            // A case-insensitive pool holds its users under the lowercased name, so this row
            // removes a user whose stored key is not the name the request used.
            case: "the call removed a user the pool holds under another spelling",
            operation: "AdminDeleteUser",
            case_sensitive: Some(false),
            stored_username: Some("Mixed.Case.User"),
            request_username: "mixed.case.user",
            extra: json!({}),
            status: 200,
            names_the_user: true,
            removes_the_user: true,
        },
        Row {
            case: "the call named a user the pool does not hold",
            operation: "AdminDeleteUser",
            case_sensitive: None,
            stored_username: None,
            request_username: "never-created",
            extra: json!({}),
            status: 400,
            names_the_user: false,
            removes_the_user: false,
        },
    ]
}

/// The body of a call on one user: the pool, the name and the row's own members.
fn request_body(user_pool_id: &str, username: &str, extra: &Value) -> Value {
    let mut body = Map::new();
    body.insert("UserPoolId".to_string(), json!(user_pool_id));
    body.insert("Username".to_string(), json!(username));
    for (member, value) in extra.as_object().expect("the row's extra members are an object") {
        body.insert(member.clone(), value.clone());
    }
    Value::Object(body)
}

#[tokio::test]
async fn the_record_of_a_call_on_one_user_names_it_by_the_pools_own_sub() {
    let emulator = Audited::new();
    for (index, row) in rows().iter().enumerate() {
        let case = row.case;
        let mut pool_request = Map::new();
        pool_request.insert("PoolName".to_string(), json!(format!("audit-pool-{index}")));
        if let Some(case_sensitive) = row.case_sensitive {
            pool_request.insert(
                "UsernameConfiguration".to_string(),
                json!({"CaseSensitive": case_sensitive}),
            );
        }
        let created_pool = emulator
            .served(case, "CreateUserPool", &Value::Object(pool_request))
            .await;
        let user_pool_id = created_pool.body["UserPool"]["Id"]
            .as_str()
            .unwrap_or_else(|| {
                panic!(
                    "case {case:?}: CreateUserPool answers the pool's Id: {}",
                    created_pool.body
                )
            })
            .to_string();

        // The user the row addresses, where the row does not create it with the call under
        // test: its own answer is where the expected `sub` comes from, so the assertion
        // never rests on a literal.
        let mut expected_sub = None;
        if let Some(stored_username) = row.stored_username {
            let created = emulator
                .served(
                    case,
                    "AdminCreateUser",
                    &json!({
                        "UserPoolId": user_pool_id,
                        "Username": stored_username,
                        "TemporaryPassword": TEMPORARY_PASSWORD,
                        "MessageAction": "SUPPRESS",
                        "UserAttributes": [{"Name": "nickname", "Value": "before"}],
                    }),
                )
                .await;
            let sub = answered_sub(case, &created.body);
            // The create's own record names the user it made by that same id: the field the
            // later record has to repeat for a subscriber to correlate the two calls.
            let create_record = emulator.record_of(case, &created.request_id);
            assert_eq!(
                create_record.pointer("/additionalEventData/sub"),
                Some(&json!(sub)),
                "case {case:?}: expected the create's record to name the user it made by \
                 {sub:?}, got {create_record}"
            );
            expected_sub = Some(sub);
        }

        let answered = emulator
            .call(
                row.operation,
                &request_body(&user_pool_id, row.request_username, &row.extra),
            )
            .await;
        assert_eq!(
            answered.status, row.status,
            "case {case:?}: expected status {}, got {} with {}",
            row.status, answered.status, answered.body
        );
        if row.stored_username.is_none() && row.names_the_user {
            // The create under test: the user it made is the user its record must name.
            expected_sub = Some(answered_sub(case, &answered.body));
        }

        let record = emulator.record_of(case, &answered.request_id);
        let named_user = record.pointer("/additionalEventData/sub");
        if row.names_the_user {
            let expected = expected_sub
                .as_deref()
                .unwrap_or_else(|| panic!("case {case:?}: the row's user answered its own sub"));
            assert_eq!(
                named_user,
                Some(&json!(expected)),
                "case {case:?}: expected the record to name the addressed user by its sub \
                 {expected:?}, got {record}"
            );
        } else {
            assert_eq!(
                named_user, None,
                "case {case:?}: a call that addresses no stored user names none, got {record}"
            );
        }

        // What makes the removing rows the hard ones: the user the record names is a user
        // the pool no longer holds, so the id cannot have come from the state left behind.
        if row.removes_the_user {
            let read = emulator
                .call(
                    "AdminGetUser",
                    &json!({"UserPoolId": user_pool_id, "Username": row.request_username}),
                )
                .await;
            assert_eq!(
                (read.status, read.body.get("__type")),
                (400, Some(&json!("UserNotFoundException"))),
                "case {case:?}: the call must have removed the user it named, got {} with {}",
                read.status,
                read.body
            );
        }

        // The name the request used stays withheld wherever the record carries it, and the
        // password the setup sent reaches no record at all.
        let whole_log = {
            let log = emulator.engine.state::<ManagementEventLog>();
            let held = log.lock();
            held.iter()
                .map(|event| event.to_cloudtrail_event_json().to_string())
                .collect::<Vec<String>>()
                .join("\n")
        };
        assert!(
            !whole_log.contains(TEMPORARY_PASSWORD),
            "case {case:?}: no record may carry the password the calls sent"
        );
    }
}

#[tokio::test]
async fn two_users_deleted_in_one_pool_are_each_named_by_their_own_sub() {
    // The id a removing call's record carries is READ for that call: two deletes in the
    // same pool name two different users, so the value cannot be a leftover of the last
    // user the service happened to see.
    let emulator = Audited::new();
    let created_pool = emulator
        .served("two users", "CreateUserPool", &json!({"PoolName": "audit-pair"}))
        .await;
    let user_pool_id = created_pool.body["UserPool"]["Id"]
        .as_str()
        .unwrap_or_else(|| panic!("CreateUserPool answers the pool's Id: {}", created_pool.body))
        .to_string();

    let mut subs = Vec::new();
    for username in ["first-user", "second-user"] {
        let created = emulator
            .served(
                username,
                "AdminCreateUser",
                &json!({
                    "UserPoolId": user_pool_id,
                    "Username": username,
                    "TemporaryPassword": TEMPORARY_PASSWORD,
                    "MessageAction": "SUPPRESS",
                }),
            )
            .await;
        subs.push(answered_sub(username, &created.body));
    }
    assert_ne!(
        subs[0], subs[1],
        "the pool mints one id per user, got {subs:?}"
    );

    for (username, sub) in ["first-user", "second-user"].iter().zip(&subs) {
        let deleted = emulator
            .served(
                username,
                "AdminDeleteUser",
                &json!({"UserPoolId": user_pool_id, "Username": username}),
            )
            .await;
        let record = emulator.record_of(username, &deleted.request_id);
        assert_eq!(
            record.pointer("/additionalEventData/sub"),
            Some(&json!(sub)),
            "case {username:?}: expected the delete's record to name that user by {sub:?}, \
             got {record}"
        );
    }
}
