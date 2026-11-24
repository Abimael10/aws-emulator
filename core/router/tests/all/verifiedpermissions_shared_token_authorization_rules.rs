// SPDX-License-Identifier: CC0-1.0

//! Verified Permissions answers a presented token the SAME way on both of its
//! token-based authorization operations.
//!
//! The rule under test: **the rules for reading a presented token belong to the TOKEN and
//! not to the operation that presented it, so `IsAuthorizedWithToken` and
//! `BatchIsAuthorizedWithToken` share one implementation of them** — one computation of
//! the principal entity types a token call's `entities` slice may not carry (the identity
//! source's own types plus the ones the policy store's schema declares), and one
//! translation of a refused token onto each operation's modelled errors. Every row below
//! is therefore asked of BOTH operations and their answers are compared to each other, not
//! only to the expectation: a rule that drifts on one path fails here.
//!
//! The suite lives with the cross-service tests because the tokens are minted and SIGNED
//! by the Amazon Cognito user-pool service, mounted on the same engine — a crate-local test
//! can only see one side of that — so every signature, audience and claim the rows turn on
//! is the real one the pool issued.

use std::sync::Arc;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The region every request here is signed for.
const REGION: &str = "us-east-1";

/// A password satisfying a default user pool's password policy.
const PASSWORD: &str = "Passw0rd!2024";

/// The entity type the policy store's identity source generates principals as.
const PRINCIPAL_TYPE: &str = "App::User";

/// The entity type it maps the pool's groups to.
const GROUP_TYPE: &str = "App::UserGroup";

/// The group the store's permit policy is written over.
const PERMITTED_GROUP: &str = "admins";

/// The resource every request asks about, and its entity type.
const DOCUMENT: &str = "doc-1";
const DOCUMENT_TYPE: &str = "App::Doc";

/// The action every request asks about, as `(actionType, actionId)`.
const READ: (&str, &str) = ("App::Action", "read");

/// The one fixed sentence an audience no identity source client matches is refused with.
const AUDIENCE_NOT_ACCEPTED: &str = "Token audience claim contains no valid audience";

/// The emulator with the two services this suite exercises, on ONE engine: Verified
/// Permissions decides, and the Amazon Cognito user-pool service owns the pool that issues
/// and signs the tokens it decides for.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_verifiedpermissions_verifiedpermissions::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(
        service_com_amazonaws_cognitoidentityprovider_awscognitoidentityproviderservice::EmulatedService::new(
            engine,
        ),
    );
    router
}

/// A SigV4 `Authorization` header whose credential scope names `service`, which is what
/// routes the request and says which Region it belongs to. The signature is never checked:
/// request authentication is off.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIAEXAMPLE/20260101/{REGION}/{service}/aws4_request, \
         SignedHeaders=host;x-amz-target, Signature=0"
    )
}

/// Call one Verified Permissions operation over its awsJson1_0 wire and return
/// `(http status, parsed body)`.
async fn avp(router: &MultiServiceRouter, operation: &str, body: &Value) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("verifiedpermissions.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.0")
        .header("x-amz-target", format!("VerifiedPermissions.{operation}"))
        .header("authorization", authorization("verifiedpermissions"))
        .body(bytes::Bytes::from(
            serde_json::to_vec(body).expect("a test request body serializes"),
        ))
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

/// The same call, required to succeed.
async fn avp_ok(router: &MultiServiceRouter, operation: &str, body: &Value) -> Value {
    let (status, parsed) = avp(router, operation, body).await;
    assert_eq!(status, 200, "{operation} failed: {parsed}");
    parsed
}

/// Call one Amazon Cognito user-pool operation over its awsJson1_1 wire, required to
/// succeed.
async fn cognito(router: &MultiServiceRouter, operation: &str, body: &Value) -> Value {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("cognito-idp.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header(
            "x-amz-target",
            format!("AWSCognitoIdentityProviderService.{operation}"),
        )
        .header("authorization", authorization("cognito-idp"))
        .body(bytes::Bytes::from(
            serde_json::to_vec(body).expect("a test request body serializes"),
        ))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed: Value = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    assert_eq!(status, 200, "{operation} failed: {parsed}");
    parsed
}

/// The error code an awsJson1_0 fault carries in `__type`, without its namespace.
fn error_code(body: &Value) -> String {
    body["__type"]
        .as_str()
        .unwrap_or_default()
        .rsplit(['#', ':'])
        .next()
        .unwrap_or_default()
        .to_string()
}

/// The `message` a fault carries.
fn fault_message(body: &Value) -> String {
    body.get("message")
        .or_else(|| body.get("Message"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// The `(path, message)` pairs a fault's `fieldList` carries, in wire order.
fn field_list(body: &Value) -> Vec<(String, String)> {
    body.get("fieldList")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|field| {
            (
                field["path"].as_str().unwrap_or_default().to_string(),
                field["message"].as_str().unwrap_or_default().to_string(),
            )
        })
        .collect()
}

/// Everything a row needs: the router, the policy store, the identity whose tokens the
/// store accepts, and the tokens the rows present.
struct Fixture {
    router: MultiServiceRouter,
    store: String,
    /// The user pool the store's identity source references.
    pool: String,
    /// The principal entity id Verified Permissions generates for the signed-in user.
    entity_id: String,
    /// The access and identity tokens the LISTED app client issued.
    access_token: String,
    id_token: String,
    /// An identity token issued to an app client the identity source does not list.
    unlisted_id_token: String,
    /// An identity token of another user of the same pool, so two presented tokens can
    /// name two different principals.
    other_id_token: String,
    /// An identity token from a pool the policy store references nowhere.
    foreign_id_token: String,
    /// That pool's issuer, which is how such a call references an identity source.
    foreign_issuer: String,
}

/// A Cedar schema declaring, in its single action's `appliesTo`, which entity types are
/// PRINCIPALS of it and which are its RESOURCES — the second place a policy store says an
/// entity type names principals. `App::Reviewer` sits on both sides on purpose: a type that
/// is some action's resource is a legitimate resource entity.
fn kinds_schema() -> String {
    serde_json::to_string(&json!({
        "App": {
            "entityTypes": {
                "User": { "memberOfTypes": ["UserGroup"] },
                "UserGroup": {},
                "Admin": {},
                "Reviewer": {},
                "Doc": {},
            },
            "actions": {
                "read": {
                    "appliesTo": {
                        "principalTypes": ["User", "Admin", "Reviewer"],
                        "resourceTypes": ["Doc", "Reviewer"],
                    },
                },
            },
        },
    }))
    .expect("the schema document serializes")
}

/// Sign `username` in through `client` and return its `(access token, identity token)`.
async fn sign_in(
    router: &MultiServiceRouter,
    pool: &str,
    client: &str,
    username: &str,
) -> (String, String) {
    let result = cognito(
        router,
        "AdminInitiateAuth",
        &json!({
            "UserPoolId": pool,
            "ClientId": client,
            "AuthFlow": "ADMIN_USER_PASSWORD_AUTH",
            "AuthParameters": { "USERNAME": username, "PASSWORD": PASSWORD },
        }),
    )
    .await;
    let token = |member: &str| {
        result["AuthenticationResult"][member]
            .as_str()
            .unwrap_or_else(|| panic!("a sign-in returns {member}, got {result}"))
            .to_string()
    };
    (token("AccessToken"), token("IdToken"))
}

/// Create `username` in `pool`, put it in `groups` and sign it in through `client`.
async fn user(
    router: &MultiServiceRouter,
    pool: &str,
    client: &str,
    username: &str,
    groups: &[&str],
) -> (String, (String, String)) {
    let created = cognito(
        router,
        "AdminCreateUser",
        &json!({ "UserPoolId": pool, "Username": username, "UserAttributes": [] }),
    )
    .await;
    let sub = created["User"]["Attributes"]
        .as_array()
        .expect("the stored user attributes")
        .iter()
        .find(|attribute| attribute["Name"] == "sub")
        .and_then(|attribute| attribute["Value"].as_str())
        .expect("a generated sub")
        .to_string();
    cognito(
        router,
        "AdminSetUserPassword",
        &json!({
            "UserPoolId": pool,
            "Username": username,
            "Password": PASSWORD,
            "Permanent": true,
        }),
    )
    .await;
    for group in groups {
        cognito(
            router,
            "CreateGroup",
            &json!({ "UserPoolId": pool, "GroupName": group }),
        )
        .await;
        cognito(
            router,
            "AdminAddUserToGroup",
            &json!({ "UserPoolId": pool, "Username": username, "GroupName": group }),
        )
        .await;
    }
    (sub, sign_in(router, pool, client, username).await)
}

/// Create a user pool named `name` and return the id it minted.
async fn user_pool(router: &MultiServiceRouter, name: &str) -> String {
    cognito(router, "CreateUserPool", &json!({ "PoolName": name })).await["UserPool"]["Id"]
        .as_str()
        .expect("a minted user pool id")
        .to_string()
}

/// Create an app client of `pool` that admin password sign-in is enabled for.
async fn app_client(router: &MultiServiceRouter, pool: &str, name: &str) -> String {
    cognito(
        router,
        "CreateUserPoolClient",
        &json!({
            "UserPoolId": pool,
            "ClientName": name,
            "ExplicitAuthFlows": ["ALLOW_ADMIN_USER_PASSWORD_AUTH"],
        }),
    )
    .await["UserPoolClient"]["ClientId"]
        .as_str()
        .expect("a minted app client id")
        .to_string()
}

/// Seed [`Fixture`]: two pools, two app clients of the first, two users, and a policy store
/// holding a schema, an identity source for the first pool and a permit policy for the
/// group.
async fn fixture() -> Fixture {
    let router = build_router();

    let pool = user_pool(&router, "shared-token-rules-pool").await;
    let listed = app_client(&router, &pool, "listed-client").await;
    let unlisted = app_client(&router, &pool, "unlisted-client").await;
    let (sub, (access_token, id_token)) =
        user(&router, &pool, &listed, "member", &[PERMITTED_GROUP]).await;
    let (_, (_, other_id_token)) = user(&router, &pool, &listed, "other", &[]).await;
    let unlisted_id_token = sign_in(&router, &pool, &unlisted, "member").await.1;

    let foreign_pool = user_pool(&router, "shared-token-rules-foreign-pool").await;
    let foreign_client = app_client(&router, &foreign_pool, "foreign-client").await;
    let (_, (_, foreign_id_token)) =
        user(&router, &foreign_pool, &foreign_client, "member", &[]).await;

    let store = avp_ok(
        &router,
        "CreatePolicyStore",
        &json!({ "validationSettings": { "mode": "OFF" } }),
    )
    .await["policyStoreId"]
        .as_str()
        .expect("a minted policyStoreId")
        .to_string();
    avp_ok(
        &router,
        "PutSchema",
        &json!({
            "policyStoreId": store,
            "definition": { "cedarJson": kinds_schema() },
        }),
    )
    .await;
    avp_ok(
        &router,
        "CreateIdentitySource",
        &json!({
            "policyStoreId": store,
            "principalEntityType": PRINCIPAL_TYPE,
            "configuration": { "cognitoUserPoolConfiguration": {
                "userPoolArn": format!(
                    "arn:aws:cognito-idp:{REGION}:{}:userpool/{pool}",
                    engine::emulator_account_id()
                ),
                "clientIds": [listed],
                "groupConfiguration": { "groupEntityType": GROUP_TYPE },
            } },
        }),
    )
    .await;
    avp_ok(
        &router,
        "CreatePolicy",
        &json!({
            "policyStoreId": store,
            "definition": { "static": { "statement": format!(
                "permit(principal in {GROUP_TYPE}::\"{pool}|{PERMITTED_GROUP}\", \
                 action == {}::\"{}\", resource == {DOCUMENT_TYPE}::\"{DOCUMENT}\");",
                READ.0, READ.1
            ) } },
        }),
    )
    .await;

    Fixture {
        router,
        store,
        pool: pool.clone(),
        entity_id: format!("{pool}|{sub}"),
        access_token,
        id_token,
        unlisted_id_token,
        other_id_token,
        foreign_id_token,
        foreign_issuer: format!("https://cognito-idp.{REGION}.amazonaws.com/{foreign_pool}"),
    }
}

/// What a row must be answered with, on each of the two operations.
enum Expect {
    /// The call is served and decides this.
    Decision(&'static str),
    /// A 400 `ValidationException` whose `message` is EXACTLY this sentence: a refusal the
    /// service words as fixed text.
    RefusedExactly(String),
    /// A 400 `ValidationException` whose `message` begins with this, which is how the
    /// service words a refusal it composes about the token.
    RefusedStartingWith(String),
    /// A 400 `ValidationException` whose `fieldList` names these pointers, in this order,
    /// each reason carrying the clause beside it.
    InvalidMembers(Vec<(&'static str, String)>),
    /// A 404 `ResourceNotFoundException` naming this resource.
    NotFound {
        resource_type: &'static str,
        resource_id: String,
    },
}

/// One row: what the call presents, and what both operations must answer.
struct Row {
    case: &'static str,
    /// The token members the request carries, as a JSON object.
    tokens: Value,
    /// The `entities` member, when the row carries one.
    entities: Option<Value>,
    expect: Expect,
}

/// The `IsAuthorizedWithToken` and `BatchIsAuthorizedWithToken` bodies asking the SAME
/// question: one request, the same token members and the same entity slice.
fn bodies(fixture: &Fixture, row: &Row) -> [(&'static str, Value); 2] {
    let action = json!({ "actionType": READ.0, "actionId": READ.1 });
    let resource = json!({ "entityType": DOCUMENT_TYPE, "entityId": DOCUMENT });
    let mut single = json!({
        "policyStoreId": fixture.store,
        "action": action,
        "resource": resource,
    });
    let mut batch = json!({
        "policyStoreId": fixture.store,
        "requests": [{ "action": action, "resource": resource }],
    });
    for (member, value) in row.tokens.as_object().expect("the tokens are an object") {
        single[member.clone()] = value.clone();
        batch[member.clone()] = value.clone();
    }
    if let Some(entities) = &row.entities {
        single["entities"] = entities.clone();
        batch["entities"] = entities.clone();
    }
    [
        ("IsAuthorizedWithToken", single),
        ("BatchIsAuthorizedWithToken", batch),
    ]
}

/// The decision an operation answered: the response's own for the single-request call, the
/// batch's only result's for the batch.
fn decision(operation: &str, answered: &Value) -> Option<String> {
    let decided = match operation {
        "BatchIsAuthorizedWithToken" => &answered["results"][0]["decision"],
        _ => &answered["decision"],
    };
    decided.as_str().map(str::to_string)
}

/// What an answer says, as the tuple the two operations are compared by: for a refusal the
/// HTTP status, the error code, the message and the whole `fieldList`; for a served call
/// the status, the decision and the principal it was decided for. Both operations of one
/// row take the same branch, so the comparison is between like and like.
type Answer = (u16, String, String, Vec<(String, String)>);

/// The refusal `answered` carries, as that tuple.
fn refusal(status: u16, answered: &Value) -> Answer {
    (
        status,
        error_code(answered),
        fault_message(answered),
        field_list(answered),
    )
}

/// Drive every row through both operations, asserting the expectation AND that the two
/// operations answered the same thing.
async fn assert_rows(fixture: &Fixture, rows: &[Row]) {
    for row in rows {
        let mut answers: Vec<Answer> = Vec::new();
        for (operation, body) in bodies(fixture, row) {
            let (status, answered) = avp(&fixture.router, operation, &body).await;
            let case = format!("case {:?} on {operation}", row.case);

            match &row.expect {
                Expect::Decision(expected) => {
                    assert_eq!(
                        status, 200,
                        "{case}: expected a decision, got {status} with {answered}"
                    );
                    assert_eq!(
                        decision(operation, &answered).as_deref(),
                        Some(*expected),
                        "{case}: expected decision {expected}, got {answered}"
                    );
                    assert_eq!(
                        answered["principal"]["entityId"].as_str(),
                        Some(fixture.entity_id.as_str()),
                        "{case}: expected the decision to be made for the token's principal \
                         {:?}, got {answered}",
                        fixture.entity_id
                    );
                    answers.push((
                        status,
                        decision(operation, &answered).unwrap_or_default(),
                        answered["principal"]["entityId"]
                            .as_str()
                            .unwrap_or_default()
                            .to_string(),
                        Vec::new(),
                    ));
                    continue;
                }
                Expect::NotFound {
                    resource_type,
                    resource_id,
                } => {
                    assert_eq!(
                        (status, error_code(&answered).as_str()),
                        (404, "ResourceNotFoundException"),
                        "{case}: expected a 404 ResourceNotFoundException, got {status} with \
                         {answered}"
                    );
                    assert_eq!(
                        (
                            answered["resourceType"].as_str(),
                            answered["resourceId"].as_str()
                        ),
                        (Some(*resource_type), Some(resource_id.as_str())),
                        "{case}: expected the missing resource to be named, got {answered}"
                    );
                }
                _ => {
                    assert_eq!(
                        (status, error_code(&answered).as_str()),
                        (400, "ValidationException"),
                        "{case}: expected a 400 ValidationException, got {status} with \
                         {answered}"
                    );
                }
            }

            let message = fault_message(&answered);
            match &row.expect {
                Expect::RefusedExactly(sentence) => assert_eq!(
                    &message, sentence,
                    "{case}: expected exactly {sentence:?}, got {message:?}"
                ),
                Expect::RefusedStartingWith(prefix) => assert!(
                    message.starts_with(prefix.as_str()),
                    "{case}: expected a message starting {prefix:?}, got {message:?}"
                ),
                Expect::InvalidMembers(expected) => {
                    let fields = field_list(&answered);
                    let named: Vec<&str> = fields.iter().map(|(path, _)| path.as_str()).collect();
                    let wanted: Vec<&str> = expected.iter().map(|(path, _)| *path).collect();
                    assert_eq!(
                        named, wanted,
                        "{case}: expected fieldList to name {wanted:?}, got {fields:?}"
                    );
                    for ((_, reported), (path, clause)) in fields.iter().zip(expected) {
                        assert!(
                            reported.contains(clause.as_str()),
                            "{case}: expected the reason for {path} to state {clause:?}, got \
                             {reported:?}"
                        );
                    }
                }
                Expect::NotFound { .. } | Expect::Decision(_) => {}
            }
            answers.push(refusal(status, &answered));
        }

        // The point of the suite: ONE implementation of the token rules, so the two
        // operations' answers are identical, member for member.
        assert_eq!(
            answers[0], answers[1],
            "case {:?}: the two token operations must answer the same thing, got {:?} and {:?}",
            row.case, answers[0], answers[1]
        );
    }
}

/// RULE: the principal entity types a token call's `entities` slice may not carry — the
/// ones the identity source that issued the token generates, and the ones the policy
/// store's schema declares as a principal — are computed once for both token operations,
/// and the token's own entities join the slice only after that check.
///
/// The rows differ along WHERE the store says the type names principals (the source's
/// `principalEntityType`, the group type it maps, the schema alone), along the accepting
/// side (a type the schema declares as a resource, an action entity naming itself) and
/// along what an action entity describes beyond its identity.
#[tokio::test]
async fn both_token_operations_check_one_set_of_principal_entity_types() {
    let fixture = fixture().await;

    let listed = |entity_type: &str, entity_id: &str, described: Value| {
        let mut item = json!({
            "identifier": { "entityType": entity_type, "entityId": entity_id },
        });
        for (member, value) in described.as_object().expect("an object") {
            item[member.clone()] = value.clone();
        }
        Some(json!({ "entityList": [item] }))
    };
    let supplied_principal = |entity_type: &str| {
        format!(
            "it carries an entity of type {entity_type}, which is an entity type the identity \
             source that issued the token generates"
        )
    };

    let rows = vec![
        Row {
            case: "an entity of the type the identity source generates principals as",
            tokens: json!({ "identityToken": fixture.id_token }),
            entities: listed(PRINCIPAL_TYPE, &fixture.entity_id, json!({})),
            expect: Expect::InvalidMembers(vec![(
                "/entities",
                supplied_principal(PRINCIPAL_TYPE),
            )]),
        },
        Row {
            case: "an entity of the type the identity source maps its groups to",
            tokens: json!({ "accessToken": fixture.access_token }),
            entities: listed(
                GROUP_TYPE,
                &format!("{}|{PERMITTED_GROUP}", fixture.pool),
                json!({}),
            ),
            expect: Expect::InvalidMembers(vec![("/entities", supplied_principal(GROUP_TYPE))]),
        },
        Row {
            case: "an entity of a type only the store's SCHEMA declares as a principal",
            tokens: json!({ "identityToken": fixture.id_token }),
            entities: listed(
                "App::Admin",
                "root",
                json!({ "attributes": { "admin": { "boolean": true } } }),
            ),
            expect: Expect::InvalidMembers(vec![(
                "/entities",
                "which the policy store's schema declares as a principal type".to_string(),
            )]),
        },
        Row {
            case: "an action entity describing more than its own identity",
            tokens: json!({ "identityToken": fixture.id_token }),
            entities: listed(
                READ.0,
                READ.1,
                json!({ "attributes": { "risk": { "long": 1 } } }),
            ),
            expect: Expect::InvalidMembers(vec![(
                "/entities",
                "an action entity can carry only its identifier and its entity type".to_string(),
            )]),
        },
        // The accepting side: a slice of resource and action entities is carried, and the
        // decision is still the token principal's.
        Row {
            case: "a resource entity of the type the schema declares as a resource",
            tokens: json!({ "identityToken": fixture.id_token }),
            entities: listed(
                DOCUMENT_TYPE,
                DOCUMENT,
                json!({ "attributes": { "owner": { "string": "member" } } }),
            ),
            expect: Expect::Decision("ALLOW"),
        },
        Row {
            case: "a type the schema declares as a principal AND as a resource",
            tokens: json!({ "accessToken": fixture.access_token }),
            entities: listed(
                "App::Reviewer",
                "rev-1",
                json!({ "attributes": { "level": { "long": 2 } } }),
            ),
            expect: Expect::Decision("ALLOW"),
        },
        Row {
            case: "an action entity carrying only its identifier",
            tokens: json!({ "identityToken": fixture.id_token }),
            entities: listed(READ.0, READ.1, json!({})),
            expect: Expect::Decision("ALLOW"),
        },
        Row {
            case: "no entity slice at all, which is the token's entities alone",
            tokens: json!({ "identityToken": fixture.id_token }),
            entities: None,
            expect: Expect::Decision("ALLOW"),
        },
    ];

    assert_rows(&fixture, &rows).await;
}

/// RULE: which answer a presented token the service could not use is refused with — the
/// status, the error code and the sentence — is decided by the TOKEN's rule and lifted into
/// each operation's own modelled errors, so both token operations answer the same refusal
/// for the same token.
///
/// The rows differ along the stage the token failed at (it carried no claim set, its
/// signature did not verify, it was presented in the wrong member, its audience is none the
/// source accepts, its issuer is no identity source of the store) and along the call's own
/// token members (none at all, two naming different principals, one that is good).
#[tokio::test]
async fn both_token_operations_refuse_a_token_the_same_way() {
    let fixture = fixture().await;

    // The issued token with its body replaced by a JSON object that is not a claim set —
    // its real header and real signature over characters this row chose.
    let segments: Vec<&str> = fixture.id_token.split('.').collect();
    assert_eq!(
        segments.len(),
        3,
        "an issued token is three '.'-separated segments, got {:?}",
        fixture.id_token
    );
    let claimless: &[u8] = br#"{"sub":"member"}"#;
    let undecodable = format!(
        "{}.{}.{}",
        segments[0],
        aws_smithy_types::base64::encode(claimless)
            .trim_end_matches('=')
            .replace('+', "-")
            .replace('/', "_"),
        segments[2]
    );
    // The issued token with the FIRST character of its signature changed, inside the
    // base64url alphabet: that character's six bits all carry signature bytes, so the
    // signature really differs whatever the pool's key happened to be.
    let tampered = {
        let signature = segments[2];
        let flipped = if signature.starts_with('A') { 'B' } else { 'A' };
        format!(
            "{}.{}.{flipped}{}",
            segments[0],
            segments[1],
            &signature[1..]
        )
    };

    let token_required = "an authorization request takes its principal from a token, so \
                          either an access token or an identity token is required"
        .to_string();
    let different_principals =
        "the access token and the identity token name different principals".to_string();

    let rows = vec![
        Row {
            case: "a token whose body is no claim set",
            tokens: json!({ "identityToken": undecodable }),
            entities: None,
            expect: Expect::RefusedStartingWith("Failed to decode IdentityToken: ".to_string()),
        },
        Row {
            case: "a token whose signature was tampered with",
            tokens: json!({ "identityToken": tampered }),
            entities: None,
            expect: Expect::RefusedExactly(
                "Failed to verify IdentityToken: signature is invalid".to_string(),
            ),
        },
        Row {
            case: "an access token presented as the identity token",
            tokens: json!({ "identityToken": fixture.access_token }),
            entities: None,
            expect: Expect::RefusedStartingWith("Failed to verify IdentityToken: ".to_string()),
        },
        Row {
            case: "a token issued to an app client the identity source does not list",
            tokens: json!({ "identityToken": fixture.unlisted_id_token }),
            entities: None,
            expect: Expect::RefusedExactly(AUDIENCE_NOT_ACCEPTED.to_string()),
        },
        Row {
            case: "a token from a user pool the policy store references nowhere",
            tokens: json!({ "identityToken": fixture.foreign_id_token }),
            entities: None,
            expect: Expect::NotFound {
                resource_type: "IDENTITY_SOURCE",
                resource_id: fixture.foreign_issuer.clone(),
            },
        },
        Row {
            case: "a call presenting no token at all",
            tokens: json!({}),
            entities: None,
            expect: Expect::InvalidMembers(vec![
                ("/accessToken", token_required.clone()),
                ("/identityToken", token_required),
            ]),
        },
        Row {
            case: "two tokens naming two different principals",
            tokens: json!({
                "accessToken": fixture.access_token,
                "identityToken": fixture.other_id_token,
            }),
            entities: None,
            expect: Expect::InvalidMembers(vec![
                ("/accessToken", different_principals.clone()),
                ("/identityToken", different_principals),
            ]),
        },
        // The control: both tokens of ONE principal still decide, so the rows above are
        // refusals of what they name and not of presenting two members.
        Row {
            case: "both tokens of the same principal",
            tokens: json!({
                "accessToken": fixture.access_token,
                "identityToken": fixture.id_token,
            }),
            entities: None,
            expect: Expect::Decision("ALLOW"),
        },
    ];

    assert_rows(&fixture, &rows).await;
}
