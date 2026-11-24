// SPDX-License-Identifier: CC0-1.0

//! What a user pool refuses about ONE user's memberships, about a projection of
//! its users' attributes and about the value a request names a user by — over the
//! wire, driven exactly as an SDK drives it (`POST /` with `X-Amz-Target:
//! AWSCognitoIdentityProviderService.<Op>`).
//!
//! Three rules, each read back from stored state:
//!
//! * A user belongs to at most `MAX_GROUPS_PER_USER` groups, Amazon Cognito's
//!   published per-user quota: the membership past it is refused
//!   `LimitExceededException` with the sentence that quota's refusal states, and
//!   the user's memberships stay as they were. An add of a group the user is
//!   already in stores nothing new, so it is served at the quota too.
//! * `ListUsers` serves an `AttributesToGet` only while EVERY user the search
//!   selects holds a value for every attribute it names — "Amazon Cognito returns
//!   an error if not all users in the results have set a value for the attribute
//!   you request" (`ListUsers.AttributesToGet`) — whether or not a server-side
//!   search covers that attribute; otherwise the whole call is refused
//!   `InvalidParameterException` and no page is served.
//! * A request names a user by a NAME of the pool's own username namespace: its
//!   stored username, an active alias value of the pool, or the username a link to
//!   a third-party identity gives it. A user's `sub` is the id the pool holds as an
//!   ATTRIBUTE of it rather than such a name, so a read under the very id it
//!   reports answers `UserNotFoundException` — except in a pool that signs its
//!   users in with an attribute value, which mints each username FROM that id, so
//!   there the id IS the username.
//!
//! Each table row names the case it asserts and every assertion reports the actual
//! value.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use cognito_idp::state::{groups_per_user_limit_message, MAX_GROUPS_PER_USER};
use service_com_amazonaws_cognitoidentityprovider_awscognitoidentityproviderservice as cognito_idp;

/// A request's SigV4 credential scope is what routes it to this service; the
/// signature itself is never checked here.
const AUTHORIZATION: &str = "AWS4-HMAC-SHA256 \
     Credential=AKIDEXAMPLE/20260101/us-east-1/cognito-idp/aws4_request, \
     SignedHeaders=host;x-amz-date, Signature=0000";

/// The sentence Amazon Cognito answers input it will not accept for a member of
/// `ListUsers` with: it states that the submitted input missed the constraints,
/// naming neither the member nor the value.
const CONSTRAINT_MESSAGE: &str = "Input fails to satisfy the constraints.";

/// The fixed sentence a read of a user the pool does not hold answers, which
/// names no username.
const USER_NOT_FOUND_MESSAGE: &str = "User does not exist.";

/// One emulator with just this service mounted.
fn router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.register_service(cognito_idp::EmulatedService::new(engine));
    router.set_request_authentication(false);
    router
}

/// Call `operation` with `body` and return its status and JSON response body.
async fn call(router: &MultiServiceRouter, operation: &str, body: &Value) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", "cognito-idp.us-east-1.amazonaws.com")
        .header("content-type", "application/x-amz-json-1.1")
        .header(
            "x-amz-target",
            format!("AWSCognitoIdentityProviderService.{operation}"),
        )
        .header("authorization", AUTHORIZATION)
        .body(Bytes::from(body.to_string()))
        .expect("the request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let payload = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or_else(|error| {
            panic!(
                "{operation} answered a body that is not JSON ({error}): {:?}",
                String::from_utf8_lossy(response.body())
            )
        })
    };
    (status, payload)
}

/// Call `operation`, requiring success, and return its response body.
async fn ok(router: &MultiServiceRouter, operation: &str, body: &Value) -> Value {
    let (status, payload) = call(router, operation, body).await;
    assert_eq!(
        status, 200,
        "{operation} must succeed, got {status} with {payload}"
    );
    payload
}

/// The `__type` of an error body, which is the wire error code.
fn error_code(payload: &Value) -> &str {
    payload
        .get("__type")
        .and_then(Value::as_str)
        .unwrap_or("<no __type>")
}

/// The `message` of an error body.
fn error_message(payload: &Value) -> &str {
    payload
        .get("message")
        .and_then(Value::as_str)
        .or_else(|| payload.get("Message").and_then(Value::as_str))
        .unwrap_or("<no message>")
}

/// A pool named `name` with `configuration` merged into its create, returned as
/// its minted `UserPoolId`.
async fn pool(router: &MultiServiceRouter, name: &str, configuration: Value) -> String {
    let mut body = json!({ "PoolName": name });
    if let Some(members) = configuration.as_object() {
        for (member, value) in members {
            body[member] = value.clone();
        }
    }
    let created = ok(router, "CreateUserPool", &body).await;
    created["UserPool"]["Id"]
        .as_str()
        .unwrap_or_else(|| panic!("a minted pool id, got {created}"))
        .to_string()
}

/// `(name, value)` pairs as the wire's `AttributeListType`.
fn attribute_list(attributes: &[(&str, &str)]) -> Value {
    Value::Array(
        attributes
            .iter()
            .map(|(name, value)| json!({ "Name": name, "Value": value }))
            .collect(),
    )
}

/// Create `username` holding `attributes` and return the stored user.
async fn user(
    router: &MultiServiceRouter,
    user_pool_id: &str,
    username: &str,
    attributes: &[(&str, &str)],
) -> Value {
    let created = ok(
        router,
        "AdminCreateUser",
        &json!({
            "UserPoolId": user_pool_id,
            "Username": username,
            "UserAttributes": attribute_list(attributes),
        }),
    )
    .await;
    created["User"].clone()
}

/// The value `rendered` holds for the attribute `name`.
fn stored_attribute_of(rendered: &Value, name: &str) -> Option<String> {
    rendered["Attributes"]
        .as_array()
        .or_else(|| rendered["UserAttributes"].as_array())?
        .iter()
        .find(|attribute| attribute["Name"] == json!(name))
        .and_then(|attribute| attribute["Value"].as_str())
        .map(str::to_string)
}

/// The group names `username` belongs to, as the membership listing serves them.
async fn memberships(router: &MultiServiceRouter, user_pool_id: &str, username: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut next_token: Option<String> = None;
    loop {
        let mut body = json!({ "UserPoolId": user_pool_id, "Username": username });
        if let Some(token) = &next_token {
            body["NextToken"] = json!(token);
        }
        let listed = ok(router, "AdminListGroupsForUser", &body).await;
        for group in listed["Groups"].as_array().cloned().unwrap_or_default() {
            names.push(
                group["GroupName"]
                    .as_str()
                    .unwrap_or_else(|| panic!("a listed group names itself, got {group}"))
                    .to_string(),
            );
        }
        next_token = listed["NextToken"].as_str().map(str::to_string);
        if next_token.is_none() {
            return names;
        }
    }
}

#[tokio::test]
async fn a_user_belongs_to_at_most_the_per_user_group_quota() {
    let router = router();
    let user_pool_id = pool(&router, "group-quota", json!({})).await;
    user(&router, &user_pool_id, "joiner", &[]).await;
    // One group more than the quota, so the add past it has a real group to name.
    let group_names: Vec<String> = (0..=MAX_GROUPS_PER_USER)
        .map(|index| format!("group-{index:03}"))
        .collect();
    for group_name in &group_names {
        ok(
            &router,
            "CreateGroup",
            &json!({ "UserPoolId": user_pool_id, "GroupName": group_name }),
        )
        .await;
    }

    // Up to the quota every membership is stored.
    for group_name in group_names.iter().take(MAX_GROUPS_PER_USER) {
        let (status, payload) = call(
            &router,
            "AdminAddUserToGroup",
            &json!({
                "UserPoolId": user_pool_id,
                "Username": "joiner",
                "GroupName": group_name,
            }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {group_name:?}: a membership inside the quota must be served, got {status} with \
             {payload}"
        );
    }
    let held = memberships(&router, &user_pool_id, "joiner").await;
    assert_eq!(
        held.len(),
        MAX_GROUPS_PER_USER,
        "the user must hold every membership inside the quota, got {} of them",
        held.len()
    );

    // The membership past it is refused, and the count stays where it was.
    let past_the_quota = &group_names[MAX_GROUPS_PER_USER];
    let (status, payload) = call(
        &router,
        "AdminAddUserToGroup",
        &json!({
            "UserPoolId": user_pool_id,
            "Username": "joiner",
            "GroupName": past_the_quota,
        }),
    )
    .await;
    let expected = groups_per_user_limit_message();
    assert_eq!(
        (status, error_code(&payload), error_message(&payload)),
        (400, "LimitExceededException", expected.as_str()),
        "case \"the membership past the quota\": got {status} with {payload}"
    );
    let after = memberships(&router, &user_pool_id, "joiner").await;
    assert_eq!(
        after, held,
        "case \"the membership past the quota\": a refused add stores nothing, got {} \
         memberships",
        after.len()
    );

    // An add of a group the user is already in stores nothing new, so the quota
    // has nothing to refuse: membership is a set.
    let already_in = &group_names[0];
    let (status, payload) = call(
        &router,
        "AdminAddUserToGroup",
        &json!({
            "UserPoolId": user_pool_id,
            "Username": "joiner",
            "GroupName": already_in,
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case \"a repeated add at the quota\": must be served, got {status} with {payload}"
    );
    let repeated = memberships(&router, &user_pool_id, "joiner").await;
    assert_eq!(
        repeated, held,
        "case \"a repeated add at the quota\": the memberships are unchanged, got {} of them",
        repeated.len()
    );
}

/// One row of the projection table: the attributes the request asks for, and
/// whether every user the search selects holds a value for each of them.
struct ProjectionRow {
    case: &'static str,
    /// The `Filter` the search carries, or `None` for a search over every user.
    filter: Option<&'static str>,
    /// The attribute names the request projects, or `None` where it carries no
    /// `AttributesToGet` at all.
    attributes_to_get: Option<&'static [&'static str]>,
    /// The attribute names the one selected user's entry must then carry, or
    /// `None` where the call is refused.
    served: Option<&'static [&'static str]>,
}

/// The rows span what the rule generalises over: an attribute no server-side
/// search covers and one it does (`given_name` is in the searchable catalog of
/// `ListUsers.Filter`), each set for every selected user and each missing from
/// one of them; several attributes at once; the empty projection; no projection
/// at all; and a name the pool's schema declares for nobody.
const PROJECTION_ROWS: [ProjectionRow; 8] = [
    ProjectionRow {
        case: "no projection at all, which returns every stored attribute",
        filter: Some("username = \"complete\""),
        attributes_to_get: None,
        served: Some(&["given_name", "nickname", "sub"]),
    },
    ProjectionRow {
        case: "a searchable attribute set for every selected user",
        filter: Some("username = \"complete\""),
        attributes_to_get: Some(&["given_name"]),
        served: Some(&["given_name"]),
    },
    ProjectionRow {
        case: "an attribute no server-side search covers, set for every selected user",
        filter: Some("username = \"complete\""),
        attributes_to_get: Some(&["nickname"]),
        served: Some(&["nickname"]),
    },
    ProjectionRow {
        case: "two attributes, both set for every selected user",
        filter: Some("username = \"complete\""),
        attributes_to_get: Some(&["given_name", "nickname"]),
        served: Some(&["given_name", "nickname"]),
    },
    ProjectionRow {
        case: "the empty projection, which carries no attribute at all",
        filter: Some("username = \"complete\""),
        attributes_to_get: Some(&[]),
        served: Some(&[]),
    },
    ProjectionRow {
        case: "a searchable attribute one selected user leaves unset",
        filter: None,
        attributes_to_get: Some(&["given_name"]),
        served: None,
    },
    ProjectionRow {
        case: "an attribute no search covers that one selected user leaves unset",
        filter: None,
        attributes_to_get: Some(&["nickname"]),
        served: None,
    },
    ProjectionRow {
        case: "a custom attribute the pool's schema declares for nobody",
        filter: Some("username = \"complete\""),
        attributes_to_get: Some(&["custom:undeclared"]),
        served: None,
    },
];

#[tokio::test]
async fn a_projection_is_served_only_where_every_selected_user_holds_its_attributes() {
    let router = router();
    let user_pool_id = pool(&router, "projection", json!({})).await;
    user(
        &router,
        &user_pool_id,
        "complete",
        &[("given_name", "Jane"), ("nickname", "Janey")],
    )
    .await;
    // A second user holding neither attribute, so a search over every user
    // selects a user that cannot serve either projection.
    user(&router, &user_pool_id, "incomplete", &[]).await;

    for row in PROJECTION_ROWS {
        let case = row.case;
        let mut body = json!({ "UserPoolId": user_pool_id });
        if let Some(filter) = row.filter {
            body["Filter"] = json!(filter);
        }
        if let Some(names) = row.attributes_to_get {
            body["AttributesToGet"] = json!(names);
        }
        let (status, payload) = call(&router, "ListUsers", &body).await;

        let Some(expected) = row.served else {
            assert_eq!(
                (status, error_code(&payload), error_message(&payload)),
                (400, "InvalidParameterException", CONSTRAINT_MESSAGE),
                "case {case:?}: the projection must be refused, got {status} with {payload}"
            );
            continue;
        };
        assert_eq!(
            status, 200,
            "case {case:?}: the projection must be served, got {status} with {payload}"
        );
        let users = payload["Users"]
            .as_array()
            .unwrap_or_else(|| panic!("case {case:?}: a listing serves its users: {payload}"));
        assert_eq!(
            users.len(),
            1,
            "case {case:?}: the search selects exactly one user, got {payload}"
        );
        let mut names: Vec<String> = users[0]["Attributes"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .map(|attribute| {
                attribute["Name"]
                    .as_str()
                    .unwrap_or_else(|| panic!("case {case:?}: an attribute names itself"))
                    .to_string()
            })
            .collect();
        names.sort();
        let expected: Vec<String> = expected.iter().map(|name| (*name).to_string()).collect();
        assert_eq!(
            names, expected,
            "case {case:?}: expected {expected:?}, got {names:?}"
        );
    }
}

#[tokio::test]
async fn a_user_is_read_by_a_name_of_the_pool_and_not_by_the_id_it_reports() {
    let router = router();
    // A pool whose users have usernames of their own, aliasing the address so a
    // verified alias value is a name too.
    let named = pool(
        &router,
        "named-users",
        json!({ "AliasAttributes": ["email"] }),
    )
    .await;
    let stored = user(
        &router,
        &named,
        "ada",
        &[("email", "ada@example.test"), ("email_verified", "true")],
    )
    .await;
    let sub = stored_attribute_of(&stored, "sub")
        .unwrap_or_else(|| panic!("a created user carries its sub, got {stored}"));

    // (case, the value the read is addressed by, the user it must report)
    for (case, addressed_by) in [
        ("the stored username", "ada".to_string()),
        ("a verified alias value of the pool", "ada@example.test".to_string()),
    ] {
        let payload = ok(
            &router,
            "AdminGetUser",
            &json!({ "UserPoolId": named, "Username": addressed_by }),
        )
        .await;
        assert_eq!(
            payload["Username"], json!("ada"),
            "case {case:?}: expected the stored user, got {payload}"
        );
        assert_eq!(
            stored_attribute_of(&payload, "sub").as_deref(),
            Some(sub.as_str()),
            "case {case:?}: the read reports the same id, got {payload}"
        );
    }

    // The very id those reads report is no name of this pool, so a read under it
    // answers that the user does not exist — and the listing still reports that
    // same id for the user.
    let (status, payload) = call(
        &router,
        "AdminGetUser",
        &json!({ "UserPoolId": named, "Username": sub }),
    )
    .await;
    assert_eq!(
        (status, error_code(&payload), error_message(&payload)),
        (400, "UserNotFoundException", USER_NOT_FOUND_MESSAGE),
        "case \"the id the read reports\": got {status} with {payload}"
    );
    let listed = ok(
        &router,
        "ListUsers",
        &json!({ "UserPoolId": named, "Filter": format!("sub = \"{sub}\"") }),
    )
    .await;
    let found: Vec<String> = listed["Users"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|entry| entry["Username"].as_str().map(str::to_string))
        .collect();
    assert_eq!(
        found,
        vec!["ada".to_string()],
        "case \"the id the read reports\": the listing still finds the user by it, got {found:?}"
    );

    // In a pool that signs its users in with an attribute value the username is
    // MINTED from that id, so there the id is the user's name and the read is
    // served under it.
    let minted = pool(
        &router,
        "minted-usernames",
        json!({ "UsernameAttributes": ["email"] }),
    )
    .await;
    let stored = user(
        &router,
        &minted,
        "grace@example.test",
        &[("email_verified", "true")],
    )
    .await;
    let minted_sub = stored_attribute_of(&stored, "sub")
        .unwrap_or_else(|| panic!("a created user carries its sub, got {stored}"));
    let payload = ok(
        &router,
        "AdminGetUser",
        &json!({ "UserPoolId": minted, "Username": minted_sub.clone() }),
    )
    .await;
    assert_eq!(
        payload["Username"].as_str(),
        Some(minted_sub.as_str()),
        "case \"a pool that mints its usernames from the id\": the id IS the username, got \
         {payload}"
    );
}
