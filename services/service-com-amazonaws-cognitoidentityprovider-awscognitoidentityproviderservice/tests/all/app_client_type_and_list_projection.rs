// SPDX-License-Identifier: CC0-1.0

//! What a rendered app client reports about itself, driven through the router
//! exactly as an SDK drives it (`POST /` with `X-Amz-Target:
//! AWSCognitoIdentityProviderService.<Op>`) over all three app-client read paths
//! — `CreateUserPoolClient`, `DescribeUserPoolClient` and
//! `UpdateUserPoolClient`.
//!
//! Two rules are under test.
//!
//! * Every app client carries the KIND of client it is, and every path that
//!   renders one reports that type from the stored client. An app client minted
//!   through `CreateUserPoolClient` is an ordinary app client of its pool, so the
//!   type it carries is the service's token for that kind.
//! * A configuration list member of an app client is optional with no modelled
//!   default, and a list with no entries configures nothing: "Amazon Cognito only
//!   populates `ReadAttributes` in the API response if you have specified your own
//!   custom set of read attributes" (`UserPoolClientType.ReadAttributes`, and
//!   `WriteAttributes` in the same words), so a member holding no entry is ABSENT
//!   from every render of the client rather than reported as an empty collection —
//!   while a member holding entries is reported with exactly those entries.
//!
//! Each table row names the case it asserts and every assertion reports the value
//! it actually saw.

use bytes::Bytes;
use serde_json::{json, Map, Value};
use smithy_server_runtime::MultiServiceRouter;
use std::sync::Arc;

/// A request's SigV4 credential scope is what routes it to this service. The
/// signature is never checked here: the suite turns request authentication off.
const AUTHORIZATION: &str = "AWS4-HMAC-SHA256 \
     Credential=AKIDEXAMPLE/20260101/us-east-1/cognito-idp/aws4_request, \
     SignedHeaders=host;x-amz-date, Signature=0000";

/// The `ClientType` the service reports for an ordinary app client — the kind
/// `CreateUserPoolClient` mints. Recorded behavior: this copy of the model
/// declares no `ClientType` member, and the service reports this token on every
/// app client it answers with.
const REGULAR_CLIENT_TYPE: &str = "REGULAR";

/// The `ExplicitAuthFlows` a create that names none leaves the app client
/// supporting: "If you don't specify a value for `ExplicitAuthFlows`, your app
/// client supports `ALLOW_REFRESH_TOKEN_AUTH`, `ALLOW_USER_SRP_AUTH`, and
/// `ALLOW_CUSTOM_AUTH`" (`smithy_query(query="shape",
/// shape="UserPoolClientType")`, member `ExplicitAuthFlows`).
const DEFAULT_AUTH_FLOWS: &[&str] = &[
    "ALLOW_REFRESH_TOKEN_AUTH",
    "ALLOW_USER_SRP_AUTH",
    "ALLOW_CUSTOM_AUTH",
];

/// One emulator with just this service mounted.
fn router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.register_service(
        service_com_amazonaws_cognitoidentityprovider_awscognitoidentityproviderservice::EmulatedService::new(engine),
    );
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

/// A created user pool's id.
async fn pool(router: &MultiServiceRouter, name: &str) -> String {
    ok(router, "CreateUserPool", &json!({ "PoolName": name })).await["UserPool"]["Id"]
        .as_str()
        .expect("a minted pool id")
        .to_string()
}

/// The app client `CreateUserPoolClient` mints for `members`, as that operation
/// renders it.
async fn created_client(
    router: &MultiServiceRouter,
    user_pool_id: &str,
    client_name: &str,
    members: &Value,
) -> Value {
    let mut body = Map::new();
    body.insert("UserPoolId".to_string(), json!(user_pool_id));
    body.insert("ClientName".to_string(), json!(client_name));
    for (member, value) in members.as_object().expect("the members are an object") {
        body.insert(member.clone(), value.clone());
    }
    ok(router, "CreateUserPoolClient", &Value::Object(body)).await["UserPoolClient"].clone()
}

/// The app client `client_id` names, as `DescribeUserPoolClient` renders it.
async fn described_client(
    router: &MultiServiceRouter,
    user_pool_id: &str,
    client_id: &str,
) -> Value {
    ok(
        router,
        "DescribeUserPoolClient",
        &json!({ "UserPoolId": user_pool_id, "ClientId": client_id }),
    )
    .await["UserPoolClient"]
        .clone()
}

/// The app client `UpdateUserPoolClient` leaves behind for `members`, as that
/// operation renders it.
async fn updated_client(
    router: &MultiServiceRouter,
    user_pool_id: &str,
    client_id: &str,
    members: &Value,
) -> Value {
    let mut body = Map::new();
    body.insert("UserPoolId".to_string(), json!(user_pool_id));
    body.insert("ClientId".to_string(), json!(client_id));
    for (member, value) in members.as_object().expect("the members are an object") {
        body.insert(member.clone(), value.clone());
    }
    ok(router, "UpdateUserPoolClient", &Value::Object(body)).await["UserPoolClient"].clone()
}

/// The `ClientId` of a rendered app client.
fn client_id(case: &str, client: &Value) -> String {
    client["ClientId"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: a rendered app client carries a ClientId: {client}"))
        .to_string()
}

/// Assert `client` reports the one client type every app client of this service
/// carries.
fn assert_client_type(case: &str, path: &str, client: &Value) {
    let reported = client.get("ClientType").and_then(Value::as_str);
    assert_eq!(
        reported,
        Some(REGULAR_CLIENT_TYPE),
        "case {case:?}: {path} must report ClientType {REGULAR_CLIENT_TYPE:?}, got {reported:?} \
         in {client}"
    );
}

/// Assert `client` reports `member` as `expected`, where `None` means the member
/// must be absent from the rendered client altogether.
fn assert_list_member(case: &str, path: &str, client: &Value, member: &str, expected: Option<Value>) {
    let actual = client.get(member);
    match &expected {
        Some(expected) => assert_eq!(
            actual,
            Some(expected),
            "case {case:?}: {path} must report {member} as {expected}, got {actual:?} in {client}"
        ),
        None => assert!(
            actual.is_none(),
            "case {case:?}: {path} must omit {member} entirely, got {actual:?} in {client}"
        ),
    }
}

/// One app-client configuration this suite creates, for the client-type rule: the
/// members the create carries beyond the pool id and the name.
struct ClientCase {
    case: &'static str,
    members: Value,
}

/// The configurations. The rows differ along what the rule generalises over: a
/// client with nothing configured, a confidential one holding a secret, one
/// configured for machine-to-machine authorization, and one running the OAuth
/// authorization server with its grants, scopes and redirect URLs.
fn client_cases() -> Vec<ClientCase> {
    vec![
        ClientCase {
            case: "a public app client with nothing else configured",
            members: json!({}),
        },
        ClientCase {
            case: "a confidential app client holding a generated secret",
            members: json!({ "GenerateSecret": true }),
        },
        ClientCase {
            case: "an app client configured for machine-to-machine authorization",
            members: json!({
                "GenerateSecret": true,
                "ExplicitAuthFlows": ["ALLOW_CLIENT_TOKEN_AUTH"],
            }),
        },
        ClientCase {
            case: "an app client running the OAuth authorization server",
            members: json!({
                "AllowedOAuthFlowsUserPoolClient": true,
                "AllowedOAuthFlows": ["code"],
                "AllowedOAuthScopes": ["openid", "email"],
                "CallbackURLs": ["https://example.com/cb"],
                "LogoutURLs": ["https://example.com/out"],
                "ReadAttributes": ["email"],
                "WriteAttributes": ["email"],
                "SupportedIdentityProviders": ["COGNITO"],
            }),
        },
    ]
}

#[tokio::test]
async fn every_app_client_read_path_reports_the_client_type_it_carries() {
    let router = router();
    let user_pool_id = pool(&router, "client-type-pool").await;

    for (index, ClientCase { case, members }) in client_cases().into_iter().enumerate() {
        let created = created_client(
            &router,
            &user_pool_id,
            &format!("client-type-{index}"),
            &members,
        )
        .await;
        assert_client_type(case, "CreateUserPoolClient", &created);

        let id = client_id(case, &created);
        let described = described_client(&router, &user_pool_id, &id).await;
        assert_client_type(case, "DescribeUserPoolClient", &described);

        // The type is the STORED client's, so a write that changes the client's
        // configuration reports the same one, and so does the read after it.
        let updated = updated_client(
            &router,
            &user_pool_id,
            &id,
            &json!({ "ClientName": format!("renamed-{index}") }),
        )
        .await;
        assert_client_type(case, "UpdateUserPoolClient", &updated);
        let described_again = described_client(&router, &user_pool_id, &id).await;
        assert_client_type(case, "DescribeUserPoolClient after the update", &described_again);
    }
}

/// One configuration list member of an app client, with a value it accepts.
///
/// The member set is `UserPoolClientType`'s optional list members, from
/// `smithy_query(query="shape", shape="UserPoolClientType")`: every one of them
/// targets a list shape and carries no `@default`.
struct ListMemberCase {
    member: &'static str,
    /// A value the member accepts, in the order a read of the client reports it.
    configured: &'static [&'static str],
    /// What a create that carries NO such member reports for it: nothing at all,
    /// except for `ExplicitAuthFlows`, whose documented default set a new app
    /// client starts with.
    absent: Option<&'static [&'static str]>,
}

fn list_member_cases() -> Vec<ListMemberCase> {
    vec![
        ListMemberCase {
            member: "ReadAttributes",
            configured: &["email", "phone_number"],
            absent: None,
        },
        ListMemberCase {
            member: "WriteAttributes",
            configured: &["email"],
            absent: None,
        },
        ListMemberCase {
            member: "ExplicitAuthFlows",
            configured: &["ALLOW_USER_SRP_AUTH", "ALLOW_REFRESH_TOKEN_AUTH"],
            absent: Some(DEFAULT_AUTH_FLOWS),
        },
        ListMemberCase {
            member: "SupportedIdentityProviders",
            configured: &["COGNITO"],
            absent: None,
        },
        ListMemberCase {
            member: "CallbackURLs",
            configured: &["https://one.example.com/cb", "https://two.example.com/cb"],
            absent: None,
        },
        ListMemberCase {
            member: "LogoutURLs",
            configured: &["https://one.example.com/out"],
            absent: None,
        },
        ListMemberCase {
            member: "AllowedOAuthFlows",
            configured: &["code"],
            absent: None,
        },
        ListMemberCase {
            member: "AllowedOAuthScopes",
            configured: &["email"],
            absent: None,
        },
    ]
}

#[tokio::test]
async fn a_configuration_list_with_no_entries_is_absent_from_every_render() {
    let router = router();
    let user_pool_id = pool(&router, "list-projection-pool").await;

    for (index, row) in list_member_cases().into_iter().enumerate() {
        let member = row.member;
        let configured = json!(row.configured);
        let absent = row.absent.map(|entries| json!(entries));

        // A member the request sets is reported with exactly those entries, by
        // the write's own answer and by the read after it.
        let case = format!("{member} set to {configured}");
        let created = created_client(
            &router,
            &user_pool_id,
            &format!("set-{index}"),
            &json!({ member: configured.clone() }),
        )
        .await;
        assert_list_member(&case, "CreateUserPoolClient", &created, member, Some(configured.clone()));
        let id = client_id(&case, &created);
        let described = described_client(&router, &user_pool_id, &id).await;
        assert_list_member(&case, "DescribeUserPoolClient", &described, member, Some(configured.clone()));

        // An update that sends the member EMPTY replaces those entries with none,
        // which is observable exactly because the member goes from present to
        // absent.
        let case = format!("{member} emptied by an update");
        let updated = updated_client(
            &router,
            &user_pool_id,
            &id,
            &json!({ member: json!([]) }),
        )
        .await;
        assert_list_member(&case, "UpdateUserPoolClient", &updated, member, None);
        let described = described_client(&router, &user_pool_id, &id).await;
        assert_list_member(&case, "DescribeUserPoolClient", &described, member, None);

        // A create that sends the member EMPTY configures nothing for it either.
        let case = format!("{member} set to the empty list");
        let created = created_client(
            &router,
            &user_pool_id,
            &format!("empty-{index}"),
            &json!({ member: json!([]) }),
        )
        .await;
        assert_list_member(&case, "CreateUserPoolClient", &created, member, None);
        let described = described_client(
            &router,
            &user_pool_id,
            &client_id(&case, &created),
        )
        .await;
        assert_list_member(&case, "DescribeUserPoolClient", &described, member, None);

        // A create that sends no such member at all: nothing for every member but
        // `ExplicitAuthFlows`, whose documented default set a new client starts
        // with.
        let case = format!("{member} left out of the request");
        let created = created_client(
            &router,
            &user_pool_id,
            &format!("unset-{index}"),
            &json!({}),
        )
        .await;
        assert_list_member(&case, "CreateUserPoolClient", &created, member, absent.clone());
        let described = described_client(
            &router,
            &user_pool_id,
            &client_id(&case, &created),
        )
        .await;
        assert_list_member(&case, "DescribeUserPoolClient", &described, member, absent);
    }
}

#[tokio::test]
async fn a_create_that_empties_every_configuration_list_reports_none_of_them() {
    let router = router();
    let user_pool_id = pool(&router, "every-empty-list-pool").await;
    let case = "every configuration list sent as the empty list at once";

    let mut members = Map::new();
    for row in list_member_cases() {
        members.insert(row.member.to_string(), json!([]));
    }
    let created = created_client(
        &router,
        &user_pool_id,
        "every-empty-list",
        &Value::Object(members),
    )
    .await;
    let id = client_id(case, &created);
    let described = described_client(&router, &user_pool_id, &id).await;

    for row in list_member_cases() {
        assert_list_member(case, "CreateUserPoolClient", &created, row.member, None);
        assert_list_member(case, "DescribeUserPoolClient", &described, row.member, None);
    }

    // The app client itself is there and reports the rest of its configuration,
    // so the omissions above are the member projection and not an empty answer.
    assert_client_type(case, "CreateUserPoolClient", &created);
    assert_client_type(case, "DescribeUserPoolClient", &described);
    assert_eq!(
        described["ClientName"],
        json!("every-empty-list"),
        "case {case:?}: the stored client must keep its name, got {described}"
    );
    assert_eq!(
        created, described,
        "case {case:?}: the create's answer must be what DescribeUserPoolClient renders, \
         got {created} and {described}"
    );
}
