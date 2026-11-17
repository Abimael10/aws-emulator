// SPDX-License-Identifier: CC0-1.0

//! The handle a stack publishes for an `AWS::Cognito::UserPool` is one Amazon Cognito's
//! ADMIN operations accept, so a client that deploys a pool with CloudFormation and then
//! drives it through the Cognito API addresses the pool it just created.
//!
//! The rule under test: **the physical resource id of a resource whose identity the OWNING
//! service assigns is the identifier that service minted, so every value a stack publishes
//! for the resource — `Ref`, an `Fn::Sub` reading it, the `PhysicalResourceId` of
//! `DescribeStackResources` — is a handle every operation of that service resolves, never
//! the name the template declared, which is only what the resource was created UNDER.**
//!
//! For a user pool the handle is `UserPoolIdType` (`@length 1..55`, `@pattern
//! ^[\w-]+_[0-9a-zA-Z]+$`), the type EVERY `UserPoolId` member targets —
//! `smithy_query(query="operation", operation="AdminCreateUser,AdminGetUser,AdminInitiateAuth")`
//! — while the declared name is `UserPoolNameType` (`@pattern ^[\w\s+=,.@-]+$`): a name
//! without an underscore satisfies neither shape the other does, so a stack that published a
//! name would leave the pool it created unaddressable.
//!
//! The rows of the first test differ along what the rule generalises over: the template
//! language (JSON and YAML), the intrinsic the handle is published with (`Ref` and `Fn::Sub`),
//! whether the template NAMES the pool and the client or leaves both names to CloudFormation,
//! whether the declared name would itself satisfy the id pattern, the Region deployed in, and
//! the user and password the admin lifecycle then runs with. Each row drives the whole
//! lifecycle the deployed pool exists for: `AdminCreateUser`, `AdminSetUserPassword`,
//! `AdminGetUser` and `AdminInitiateAuth`.
//!
//! The second test is the other half of the rule: the name the template declared addresses no
//! pool, whether or not that name happens to satisfy the id pattern.
//!
//! Every expected value is computed from what the row declares and from what the SAME answers
//! report; nothing is a literal taken from a recording. Two crates have to agree for the rule
//! to hold (CloudFormation makes the create call, Amazon Cognito serves it and the admin calls
//! that follow), so the suite lives here rather than in either of them.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The awsQuery form encoder and the XML readers this crate writes ONCE
// (`tests/all/query_wire.rs`): "Nothing new should write another one — call this module."
use super::query_wire::{form_encode, opt_element, outputs_of, physical_ids_of};
// The compact-JWS claim-set reader this test tree already writes once, rather than a copy of it.
use super::cognito_group_role_mapping::claims_of;
// The predicate for an identifier Amazon Cognito minted for a user pool, and the accessor for
// one output of a stack, both stated once by the sibling suite that first needed them.
use super::cloudformation_provisions_cognito_user_pools::{is_minted_pool_id, output};

/// How long one `DescribeStacks` poll waits before the next, and how many are made: a stack's
/// status transition is owed on its own clock, so the suite waits for the STATUS rather than
/// for a span, with a deadline generous enough for a loaded host.
const POLL_INTERVAL: Duration = Duration::from_millis(50);
/// See [`POLL_INTERVAL`].
const POLLS: usize = 1200;

/// The HTTP status this user-pools endpoint answers a refusal of the CALLER with, whatever
/// `@httpError` the error shape declares: the error class travels in the body's `__type` on
/// this RPC wire, which is what a client branches on.
const REFUSAL_STATUS: u16 = 400;

/// The emulator with the two services this suite exercises, mounted on ONE engine exactly as
/// `core/router/src/lib.rs` mounts them. Request authentication is off, as in every other
/// suite here; the in-process call CloudFormation makes signs its request either way (the
/// credential scope is what routes it).
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_cloudformation_cloudformation::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_cognitoidentityprovider_awscognitoidentityproviderservice::
            EmulatedService::new(engine),
    );
    router
}

/// A SigV4 `Authorization` header whose credential scope names `service` in `region`, which is
/// what routes the request and what each handler reads its Region from. The signature is never
/// checked: this router runs with request authentication off.
fn authorization(service: &str, region: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{region}/{service}/aws4_request, \
         SignedHeaders=host, Signature=0000000000000000000000000000000000000000000000000000000000000000"
    )
}

/// Call one CloudFormation operation over its awsQuery wire, signed for `region`.
async fn cloudformation(
    router: &MultiServiceRouter,
    region: &str,
    action: &str,
    members: &[(&str, &str)],
) -> (u16, String) {
    let mut body = format!("Action={action}&Version=2010-05-15");
    for (key, value) in members {
        body.push('&');
        body.push_str(&form_encode(key));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("cloudformation.{region}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization("cloudformation", region))
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let payload = String::from_utf8_lossy(response.body()).to_string();
    (status, payload)
}

/// Call one Amazon Cognito user-pools operation over its awsJson1_1 wire, signed for `region`.
async fn cognito(
    router: &MultiServiceRouter,
    region: &str,
    operation: &str,
    body: &Value,
) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("cognito-idp.{region}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header(
            "x-amz-target",
            format!("AWSCognitoIdentityProviderService.{operation}"),
        )
        .header("authorization", authorization("cognito-idp", region))
        .body(Bytes::from(
            serde_json::to_vec(body).expect("the request body serializes"),
        ))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let payload: Value = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    (status, payload)
}

/// One Amazon Cognito call that has to succeed, with the case in the panic of one that did not.
async fn cognito_ok(
    router: &MultiServiceRouter,
    region: &str,
    operation: &str,
    body: &Value,
    case: &str,
) -> Value {
    let (status, answer) = cognito(router, region, operation, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from {operation}, got {status} with body {answer}"
    );
    answer
}

/// The error code an awsJson1_1 refusal carries, as its `__type` names it (without the
/// namespace a shape id may qualify it with), or `""` on a body carrying none.
fn error_code(body: &Value) -> &str {
    body.get("__type")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .rsplit('#')
        .next()
        .unwrap_or_default()
}

/// The `message` of a refusal, under either spelling an awsJson envelope uses for it.
fn error_message(body: &Value) -> &str {
    body.get("message")
        .or_else(|| body.get("Message"))
        .and_then(Value::as_str)
        .unwrap_or_default()
}

/// A string member of an answer, or a panic naming the case and what was there instead.
fn text(answer: &Value, member: &str, case: &str) -> String {
    answer
        .get(member)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: expected a string {member:?}, got {answer}"))
        .to_string()
}

/// Wait for `stack` to report `wanted`, looking again until it does or the deadline passes.
async fn wait_for_stack_status(
    router: &MultiServiceRouter,
    region: &str,
    stack: &str,
    wanted: &str,
) -> String {
    let mut body = String::new();
    for _ in 0..POLLS {
        let (status, reported) =
            cloudformation(router, region, "DescribeStacks", &[("StackName", stack)]).await;
        assert_eq!(
            status, 200,
            "DescribeStacks {stack:?}: expected HTTP 200, got {status} with body {reported}"
        );
        body = reported;
        if opt_element(&body, "StackStatus").as_deref() == Some(wanted) {
            return body;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
    panic!("stack {stack:?} never reached {wanted:?}; last answer was {body}");
}

/// Deploy `template` as `stack` in `region` and answer with the outputs the completed stack
/// publishes.
async fn deploy(
    router: &MultiServiceRouter,
    region: &str,
    stack: &str,
    template: &str,
    case: &str,
) -> BTreeMap<String, String> {
    let (status, body) = cloudformation(
        router,
        region,
        "CreateStack",
        &[("StackName", stack), ("TemplateBody", template)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CreateStack, got {status} with body {body}"
    );
    let described = wait_for_stack_status(router, region, stack, "CREATE_COMPLETE").await;
    outputs_of(&described)
}

/// One claim of a token's claim set, as a string.
fn claim(claims: &Value, name: &str, case: &str) -> String {
    claims
        .get(name)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: expected a string {name:?} claim, got {claims}"))
        .to_string()
}

/// One row of the first table: a template that deploys a pool and an app client, and the user
/// the admin lifecycle then runs through the handle the stack published.
struct Row {
    case: &'static str,
    /// The Region the row deploys in, which is the Region its pool lives in.
    region: &'static str,
    /// The stack name the row deploys under.
    stack: &'static str,
    /// The template body, in the language the row writes it in.
    template: &'static str,
    /// The `UserPoolName` the template declares, or `None` for a pool CloudFormation names.
    declared_pool_name: Option<&'static str>,
    /// The output key the pool handle is published under, and the intrinsic reading it.
    pool_output: &'static str,
    /// The output key the app client's id is published under.
    client_output: &'static str,
    /// The logical id of the pool resource, which `DescribeStackResources` reports the
    /// physical id of.
    pool_logical_id: &'static str,
    /// The user this row creates in the pool it deployed.
    username: &'static str,
    /// The password this row then sets for that user and signs it in with.
    password: &'static str,
}

const ROWS: &[Row] = &[
    Row {
        case: "a JSON template naming the pool, handle read with Ref",
        region: "us-east-1",
        stack: "cognito-admin-json",
        template: r#"{
            "Resources": {
                "Pool": {
                    "Type": "AWS::Cognito::UserPool",
                    "Properties": {
                        "UserPoolName": "storefront-directory",
                        "Policies": { "PasswordPolicy": { "MinimumLength": 8 } }
                    }
                },
                "Client": {
                    "Type": "AWS::Cognito::UserPoolClient",
                    "Properties": {
                        "UserPoolId": { "Ref": "Pool" },
                        "ClientName": "storefront-client",
                        "GenerateSecret": false,
                        "ExplicitAuthFlows": [
                            "ALLOW_ADMIN_USER_PASSWORD_AUTH",
                            "ALLOW_REFRESH_TOKEN_AUTH"
                        ]
                    }
                }
            },
            "Outputs": {
                "UserPoolId": { "Value": { "Ref": "Pool" } },
                "ClientId": { "Value": { "Ref": "Client" } }
            }
        }"#,
        declared_pool_name: Some("storefront-directory"),
        pool_output: "UserPoolId",
        client_output: "ClientId",
        pool_logical_id: "Pool",
        username: "ada",
        password: "Storefront#Pass9",
    },
    Row {
        case: "a YAML template whose declared name would itself satisfy the id pattern, \
               handle read with Fn::Sub",
        region: "us-west-2",
        stack: "cognito-admin-underscored",
        template: r#"Resources:
  Directory:
    Type: AWS::Cognito::UserPool
    Properties:
      UserPoolName: team_directory
  SignIn:
    Type: AWS::Cognito::UserPoolClient
    Properties:
      UserPoolId: !Ref Directory
      ClientName: team-sign-in
      ExplicitAuthFlows:
        - ALLOW_ADMIN_USER_PASSWORD_AUTH
        - ALLOW_REFRESH_TOKEN_AUTH
Outputs:
  PoolHandle:
    Value: !Sub '${Directory}'
  SignInClient:
    Value: !Ref SignIn
"#,
        declared_pool_name: Some("team_directory"),
        pool_output: "PoolHandle",
        client_output: "SignInClient",
        pool_logical_id: "Directory",
        username: "grace.hopper",
        password: "TeamDirectory#9ab",
    },
    Row {
        case: "a YAML template naming neither the pool nor the client",
        region: "eu-central-1",
        stack: "cognito-admin-generated",
        template: r#"Resources:
  Pool:
    Type: AWS::Cognito::UserPool
    Properties:
      MfaConfiguration: 'OFF'
  Client:
    Type: AWS::Cognito::UserPoolClient
    Properties:
      UserPoolId: !Ref Pool
      ExplicitAuthFlows:
        - ALLOW_ADMIN_USER_PASSWORD_AUTH
        - ALLOW_REFRESH_TOKEN_AUTH
Outputs:
  PoolRef:
    Value: !Ref Pool
  ClientRef:
    Value: !Ref Client
"#,
        declared_pool_name: None,
        pool_output: "PoolRef",
        client_output: "ClientRef",
        pool_logical_id: "Pool",
        username: "k.johnson",
        password: "Generated#Pass42",
    },
];

/// Every row: the handle the stack publishes for its user pool is the id Amazon Cognito
/// minted, the stack reports that same value as the resource's physical id, and the whole
/// admin lifecycle a client drives next — create the user, set its password, read it back,
/// sign it in — resolves that pool.
#[tokio::test]
async fn a_published_pool_handle_drives_the_admin_user_lifecycle() {
    let router = build_router();

    for row in ROWS {
        let case = row.case;
        let region = row.region;
        let outputs = deploy(&router, region, row.stack, row.template, case).await;

        // The handle: an id of the form the owning service mints, which is not the name the
        // template declared.
        let pool = output(&outputs, row.pool_output, case);
        assert!(
            is_minted_pool_id(&pool, region),
            "case {case:?}: expected the published pool handle to be the id Amazon Cognito \
             minted, got {pool:?}"
        );
        if let Some(name) = row.declared_pool_name {
            assert_ne!(
                pool, name,
                "case {case:?}: expected the published pool handle to be the minted id, not the \
                 declared UserPoolName"
            );
        }

        // The stack reports the same value as the resource's physical id, which is what every
        // reference to the resource resolves from.
        let (status, resources) = cloudformation(
            &router,
            region,
            "DescribeStackResources",
            &[("StackName", row.stack)],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from DescribeStackResources, got {status} with \
             body {resources}"
        );
        let physical = physical_ids_of(&resources);
        assert_eq!(
            physical.get(row.pool_logical_id),
            Some(&pool),
            "case {case:?}: expected the pool's PhysicalResourceId to be the published handle \
             {pool:?}, got {physical:?}"
        );

        // The pool that handle resolves to is the one the template declared.
        let read = cognito_ok(
            &router,
            region,
            "DescribeUserPool",
            &json!({ "UserPoolId": &pool }),
            case,
        )
        .await;
        let described = &read["UserPool"];
        assert_eq!(
            described.get("Id").and_then(Value::as_str),
            Some(pool.as_str()),
            "case {case:?}: expected DescribeUserPool to report the handle as the pool's Id, \
             got {described}"
        );
        if let Some(name) = row.declared_pool_name {
            assert_eq!(
                described.get("Name").and_then(Value::as_str),
                Some(name),
                "case {case:?}: expected the pool's Name to be the declared UserPoolName, got \
                 {described}"
            );
        }

        // The lifecycle the deployed pool exists for, every call addressed by the handle.
        let created = cognito_ok(
            &router,
            region,
            "AdminCreateUser",
            &json!({
                "UserPoolId": &pool,
                "Username": row.username,
                "MessageAction": "SUPPRESS"
            }),
            case,
        )
        .await;
        assert_eq!(
            created["User"].get("Username").and_then(Value::as_str),
            Some(row.username),
            "case {case:?}: expected AdminCreateUser to answer with the user it created, got {}",
            created["User"]
        );
        assert_eq!(
            created["User"].get("UserStatus").and_then(Value::as_str),
            Some("FORCE_CHANGE_PASSWORD"),
            "case {case:?}: expected a created user to owe a password change, got {}",
            created["User"]
        );

        cognito_ok(
            &router,
            region,
            "AdminSetUserPassword",
            &json!({
                "UserPoolId": &pool,
                "Username": row.username,
                "Password": row.password,
                "Permanent": true
            }),
            case,
        )
        .await;

        let fetched = cognito_ok(
            &router,
            region,
            "AdminGetUser",
            &json!({ "UserPoolId": &pool, "Username": row.username }),
            case,
        )
        .await;
        assert_eq!(
            text(&fetched, "Username", case),
            row.username,
            "case {case:?}: expected AdminGetUser to read back the user created through the \
             published handle, got {fetched}"
        );
        assert_eq!(
            text(&fetched, "UserStatus", case),
            "CONFIRMED",
            "case {case:?}: expected a permanent password to confirm the user, got {fetched}"
        );

        let client = output(&outputs, row.client_output, case);
        let signed_in = cognito_ok(
            &router,
            region,
            "AdminInitiateAuth",
            &json!({
                "UserPoolId": &pool,
                "ClientId": &client,
                "AuthFlow": "ADMIN_USER_PASSWORD_AUTH",
                "AuthParameters": { "USERNAME": row.username, "PASSWORD": row.password }
            }),
            case,
        )
        .await;
        assert!(
            signed_in.get("ChallengeName").is_none(),
            "case {case:?}: expected a completed sign-in rather than a challenge, got {signed_in}"
        );
        let result = &signed_in["AuthenticationResult"];
        let id_token = text(result, "IdToken", case);
        assert!(
            !text(result, "AccessToken", case).is_empty(),
            "case {case:?}: expected a sign-in to mint an access token, got {result}"
        );
        // The token was minted BY the pool the stack published: a pool's issuer is its own
        // id under the service's issuer host, and the audience is the client that signed in.
        let claims = claims_of(&id_token);
        let issuer = claim(&claims, "iss", case);
        assert!(
            issuer.ends_with(&format!("/{pool}")),
            "case {case:?}: expected the id token's issuer to name the published pool {pool:?}, \
             got {issuer:?}"
        );
        assert_eq!(
            claim(&claims, "aud", case),
            client,
            "case {case:?}: expected the id token's audience to be the published client id"
        );
        assert_eq!(
            claim(&claims, "cognito:username", case),
            row.username,
            "case {case:?}: expected the id token to name the signed-in user"
        );
    }
}

/// The other half of the rule: the name a template declared is what the pool was created
/// UNDER, not a handle — whether or not that name happens to satisfy the id pattern, no pool
/// answers to it, while the handle the same stack published does.
#[tokio::test]
async fn the_declared_pool_name_is_no_handle() {
    let router = build_router();
    let region = "ap-northeast-1";
    let case = "two pools in one stack, one declared name per id-pattern outcome";

    // One stack, two pools: a name no `UserPoolIdType` admits, and a name it does admit.
    let template = r#"Resources:
  Hyphenated:
    Type: AWS::Cognito::UserPool
    Properties:
      UserPoolName: directory-of-staff
  Underscored:
    Type: AWS::Cognito::UserPool
    Properties:
      UserPoolName: staff_directory
Outputs:
  HyphenatedRef:
    Value: !Ref Hyphenated
  UnderscoredRef:
    Value: !Ref Underscored
"#;
    let outputs = deploy(&router, region, "cognito-name-is-no-handle", template, case).await;

    // (case, the declared name, the output its pool's handle is published under, the error
    // code the admin call answers the NAME with, and the fixed text of its message)
    let rows: [(&str, &str, &str, &str, String); 2] = [
        (
            "a declared name no id pattern admits",
            "directory-of-staff",
            "HyphenatedRef",
            "InvalidParameterException",
            "1 validation error detected: Value 'directory-of-staff' at 'userPoolId' failed to \
             satisfy constraint: Member must satisfy regular expression pattern: "
                .to_string(),
        ),
        (
            "a declared name the id pattern admits, held by no pool",
            "staff_directory",
            "UnderscoredRef",
            "ResourceNotFoundException",
            "User pool staff_directory does not exist.".to_string(),
        ),
    ];

    for (name_case, name, handle_output, code_wanted, message_wanted) in rows {
        let handle = output(&outputs, handle_output, name_case);
        assert_ne!(
            handle, name,
            "case {name_case:?}: expected the published handle to be the minted id, not the \
             declared name"
        );
        assert!(
            is_minted_pool_id(&handle, region),
            "case {name_case:?}: expected the published handle to be a minted pool id, got \
             {handle:?}"
        );

        // The handle resolves the pool the name describes.
        let read = cognito_ok(
            &router,
            region,
            "DescribeUserPool",
            &json!({ "UserPoolId": &handle }),
            name_case,
        )
        .await;
        assert_eq!(
            read["UserPool"].get("Name").and_then(Value::as_str),
            Some(name),
            "case {name_case:?}: expected the handle to resolve the pool declared under {name:?}, \
             got {read}"
        );

        // The name resolves nothing.
        let (status, refused) = cognito(
            &router,
            region,
            "AdminCreateUser",
            &json!({ "UserPoolId": name, "Username": "staffer", "MessageAction": "SUPPRESS" }),
        )
        .await;
        assert_eq!(
            status, REFUSAL_STATUS,
            "case {name_case:?}: expected HTTP {REFUSAL_STATUS} for a pool addressed by name, got \
             {status} with body {refused}"
        );
        assert_eq!(
            error_code(&refused),
            code_wanted,
            "case {name_case:?}: expected {code_wanted}, got body {refused}"
        );
        let message = error_message(&refused);
        assert!(
            message.starts_with(&message_wanted),
            "case {name_case:?}: expected a message opening {message_wanted:?}, got {message:?}"
        );
        if code_wanted == "InvalidParameterException" {
            // The constraint the refusal names is the one the model states for every
            // `UserPoolId` member.
            assert!(
                message.contains(r"[\w-]+_[0-9a-zA-Z]+"),
                "case {name_case:?}: expected the refusal to name the UserPoolIdType pattern, \
                 got {message:?}"
            );
        }
    }
}
