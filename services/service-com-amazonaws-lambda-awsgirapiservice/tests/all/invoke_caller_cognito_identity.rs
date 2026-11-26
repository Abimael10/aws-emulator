// SPDX-License-Identifier: CC0-1.0

//! WHO an invocation reports itself as having been made BY: `context.identity`, over the
//! restJson1 wire.
//!
//! RULE: an invocation signed with credentials an Amazon Cognito identity pool VENDED runs
//! with that caller's identity in its context — the identity the pool holds for the caller
//! and the pool that vended the credentials, read off the web-identity claims the session the
//! vend minted carries — and an invocation signed with any other credential (an IAM user's
//! long-term key, an assumed-role session, the account's own key) runs with NO Cognito
//! identity in its context, each member reading as absent. Each vended session is reported on
//! its own: two identities, and two pools, are never conflated.
//!
//! The identity is the REAL one: the pool is created with Amazon Cognito Identity's own
//! `CreateIdentityPool` / `SetIdentityPoolRoles`, the identity with its own `GetId`, and the
//! credentials with its own `GetCredentialsForIdentity` — the vend that assumes the pool's
//! role through the token service, which is where the claims this reads live. The contrast
//! principals are made with IAM's and STS's own operations over the same engine. Nothing
//! about the identity is passed in by the caller: the only thing the invoke carries is the
//! access key id of whichever credential it is signed with.
//!
//! The rows differ along what the rule generalises over: the KIND of credential the invoke is
//! signed with, WHICH identity of a pool it was vended to, WHICH pool vended it, and whether
//! the same identity's second, independent vend reports the same identity.

use std::sync::Arc;
use std::time::Duration;

use base64::Engine as _;
use serde_json::{json, Value};
use service_com_amazonaws_lambda_awsgirapiservice::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

// This suite RUNS handlers, so it takes this target's shared invocation stream: see the
// helper's own documentation for why a suite that runs handlers holds it.
use crate::invoke_request_id::one_invocation_stream_at_a_time;

/// The Region every call below is signed for.
const REGION: &str = "us-east-1";

/// Where `CreateFunction` and `Invoke` are bound.
const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// The emulator's own address, for the legs a browser client sends UNSIGNED: `GetId` and
/// `GetCredentialsForIdentity` list no auth scheme, so the target alone names their service.
const EMULATOR_HOST: &str = "127.0.0.1:9297";

/// The access key id every signed setup call carries: neither STS nor IAM holds a record for
/// it, so it resolves to the account's own identity — which is also one of the contrast rows.
const ACCOUNT_KEY: &str = "EMULATORTESTKEY12345";

/// The module and entry point of the package below.
const HANDLER: &str = "lambda_function.lambda_handler";

/// The function's execution role, which satisfies the model's `RoleArn` pattern.
const EXECUTION_ROLE: &str = "arn:aws:iam::000000000000:role/service-role/lambda-exec";

/// The handler under test: it REPORTS what the runtime put on `context.identity`, which is
/// what a function authorizing on its caller's Cognito identity reads.
const SOURCE: &str = "def lambda_handler(event, context):\n    \
     return {\n        \
     'cognito_identity_id': context.identity.cognito_identity_id,\n        \
     'cognito_identity_pool_id': context.identity.cognito_identity_pool_id,\n    }\n";

/// The IAM API version every `awsQuery` call to IAM carries.
const IAM_API_VERSION: &str = "2010-05-08";

/// The STS API version every `awsQuery` call to STS carries.
const STS_API_VERSION: &str = "2011-06-15";

/// How long this suite waits for a transition another component owes — the function becoming
/// invocable — before giving up. Generous: what is waited for is a CONDITION, looked at again
/// until it holds (see [`poll_until_active`]), so a slow host makes this suite slower and
/// never flakier.
const READY_DEADLINE: Duration = Duration::from_secs(60);

/// How long each look at that condition waits before the next.
const POLL: Duration = Duration::from_millis(50);

/// What one call answered.
struct Answer {
    status: u16,
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

/// The emulator this suite drives: ONE engine carrying Lambda beside the Amazon Cognito
/// Identity, IAM and token services the credentials are made with.
struct Emulator {
    router: MultiServiceRouter,
}

impl Emulator {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        // The suite signs nothing: the `Authorization` header carries only the credential
        // scope, which is where a service reads the caller's access key id from.
        router.set_request_authentication(false);
        router.register_service(EmulatedService::new(Arc::clone(&engine)));
        router.register_service(
            service_com_amazonaws_cognitoidentity_awscognitoidentityservice::EmulatedService::new(
                Arc::clone(&engine),
            ),
        );
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
        Self { router }
    }

    /// One call to Lambda, with `access_key_id` in the SigV4 credential scope.
    async fn lambda(
        &self,
        access_key_id: &str,
        method: &str,
        uri: &str,
        body: &[u8],
        headers: &[(&str, &str)],
    ) -> Answer {
        let mut builder = http::Request::builder()
            .method(method)
            .uri(format!("https://lambda.{REGION}.amazonaws.com{uri}"))
            .header("host", format!("lambda.{REGION}.amazonaws.com"))
            .header("content-type", "application/json")
            .header(
                "authorization",
                format!(
                    "AWS4-HMAC-SHA256 Credential={access_key_id}/20260101/{REGION}/lambda/\
                     aws4_request, SignedHeaders=host, Signature=not-verified"
                ),
            );
        for (name, value) in headers {
            builder = builder.header(*name, *value);
        }
        let request = builder
            .body(bytes::Bytes::from(body.to_vec()))
            .expect("the test request must build");
        let response = self.router.dispatch(request).await;
        let (parts, body) = response.into_parts();
        Answer {
            status: parts.status.as_u16(),
            body,
        }
    }

    /// One awsJson1_1 call to Amazon Cognito Identity: signed for that service when
    /// `signed` is set, and sent with no credential scope at all — the way a client sends an
    /// operation whose model lists no auth scheme — when it is not.
    async fn cognito(&self, signed: bool, target: &str, body: &Value) -> Answer {
        let host = if signed {
            format!("cognito-identity.{REGION}.amazonaws.com")
        } else {
            EMULATOR_HOST.to_string()
        };
        let mut builder = http::Request::builder()
            .method("POST")
            .uri(format!("https://{host}/"))
            .header("host", host.clone())
            .header("content-type", "application/x-amz-json-1.1")
            .header("x-amz-target", format!("AWSCognitoIdentityService.{target}"));
        if signed {
            builder = builder.header(
                "authorization",
                format!(
                    "AWS4-HMAC-SHA256 Credential={ACCOUNT_KEY}/20260101/{REGION}/\
                     cognito-identity/aws4_request, SignedHeaders=host, Signature=not-verified"
                ),
            );
        }
        let request = builder
            .body(bytes::Bytes::from(body.to_string()))
            .expect("the test request must build");
        let response = self.router.dispatch(request).await;
        let (parts, body) = response.into_parts();
        Answer {
            status: parts.status.as_u16(),
            body,
        }
    }

    /// One awsQuery call, which is how IAM and the token service are called, signed with
    /// `access_key_id`.
    async fn query(
        &self,
        signing_name: &str,
        access_key_id: &str,
        action: &str,
        body: String,
    ) -> String {
        let request = http::Request::builder()
            .method("POST")
            .uri(format!("https://{signing_name}.{REGION}.amazonaws.com/"))
            .header("host", format!("{signing_name}.{REGION}.amazonaws.com"))
            .header("content-type", "application/x-www-form-urlencoded")
            .header(
                "authorization",
                format!(
                    "AWS4-HMAC-SHA256 Credential={access_key_id}/20260101/{REGION}/\
                     {signing_name}/aws4_request, SignedHeaders=host, Signature=not-verified"
                ),
            )
            .body(bytes::Bytes::from(body))
            .expect("a well-formed awsQuery request");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let text = String::from_utf8_lossy(response.body()).to_string();
        assert_eq!(
            status, 200,
            "setup {action:?}: the call must succeed, got {status} with {text}"
        );
        text
    }

    /// `cognito-identity:CreateIdentityPool` for a pool that admits the unauthenticated flow,
    /// answering the pool's id.
    async fn identity_pool(&self, pool_name: &str) -> String {
        let created = self
            .cognito(
                true,
                "CreateIdentityPool",
                &json!({
                    "IdentityPoolName": pool_name,
                    "AllowUnauthenticatedIdentities": true,
                }),
            )
            .await;
        assert_eq!(
            created.status,
            200,
            "setup: CreateIdentityPool({pool_name:?}) must succeed, got {} with {}",
            created.status,
            created.text()
        );
        created.json()["IdentityPoolId"]
            .as_str()
            .unwrap_or_else(|| {
                panic!(
                    "CreateIdentityPool reports an IdentityPoolId, got {}",
                    created.text()
                )
            })
            .to_string()
    }

    /// `cognito-identity:SetIdentityPoolRoles`: the role a vend of `pool_id` assumes.
    async fn pool_roles(&self, pool_id: &str, unauthenticated: &str) {
        let configured = self
            .cognito(
                true,
                "SetIdentityPoolRoles",
                &json!({
                    "IdentityPoolId": pool_id,
                    "Roles": {"unauthenticated": unauthenticated},
                }),
            )
            .await;
        assert_eq!(
            configured.status,
            200,
            "setup: SetIdentityPoolRoles({pool_id:?}) must succeed, got {} with {}",
            configured.status,
            configured.text()
        );
    }

    /// An IAM role `role_name` carrying `trust` as its trust policy and the invoking grant as
    /// its inline policy, answering its ARN.
    async fn role(&self, role_name: &str, trust: &str) -> String {
        let created = self
            .query(
                "iam",
                ACCOUNT_KEY,
                "CreateRole",
                format!(
                    "Action=CreateRole&Version={IAM_API_VERSION}&RoleName={role_name}\
                     &AssumeRolePolicyDocument={trust}"
                ),
            )
            .await;
        self.query(
            "iam",
            ACCOUNT_KEY,
            "PutRolePolicy",
            format!(
                "Action=PutRolePolicy&Version={IAM_API_VERSION}&RoleName={role_name}\
                 &PolicyName=invoke&PolicyDocument={}",
                invoke_grant()
            ),
        )
        .await;
        xml_element(&created, "Arn")
    }

    /// An IAM user holding the invoking grant, answering the access key id it signs with.
    async fn user(&self, user_name: &str) -> String {
        self.query(
            "iam",
            ACCOUNT_KEY,
            "CreateUser",
            format!("Action=CreateUser&Version={IAM_API_VERSION}&UserName={user_name}"),
        )
        .await;
        self.query(
            "iam",
            ACCOUNT_KEY,
            "PutUserPolicy",
            format!(
                "Action=PutUserPolicy&Version={IAM_API_VERSION}&UserName={user_name}\
                 &PolicyName=invoke&PolicyDocument={}",
                invoke_grant()
            ),
        )
        .await;
        let minted = self
            .query(
                "iam",
                ACCOUNT_KEY,
                "CreateAccessKey",
                format!("Action=CreateAccessKey&Version={IAM_API_VERSION}&UserName={user_name}"),
            )
            .await;
        xml_element(&minted, "AccessKeyId")
    }

    /// `sts:AssumeRole` of the role `role_arn`, answering the session's access key id.
    async fn assumed_session(&self, role_arn: &str) -> String {
        let assumed = self
            .query(
                "sts",
                ACCOUNT_KEY,
                "AssumeRole",
                format!(
                    "Action=AssumeRole&Version={STS_API_VERSION}&RoleArn={role_arn}\
                     &RoleSessionName=probe"
                ),
            )
            .await;
        xml_element(&assumed, "AccessKeyId")
    }

    /// An unsigned `GetId` on `pool_id`, answering the identity it holds for this caller.
    async fn get_id(&self, pool_id: &str) -> String {
        let minted = self
            .cognito(false, "GetId", &json!({"IdentityPoolId": pool_id}))
            .await;
        assert_eq!(
            minted.status,
            200,
            "setup: an unsigned GetId must be served, got {} with {}",
            minted.status,
            minted.text()
        );
        minted.json()["IdentityId"]
            .as_str()
            .unwrap_or_else(|| panic!("GetId reports an IdentityId, got {}", minted.text()))
            .to_string()
    }

    /// An unsigned `GetCredentialsForIdentity` for `identity_id`, answering the access key id
    /// of the session it vended.
    async fn vended_key(&self, identity_id: &str) -> String {
        let vended = self
            .cognito(
                false,
                "GetCredentialsForIdentity",
                &json!({"IdentityId": identity_id}),
            )
            .await;
        assert_eq!(
            vended.status,
            200,
            "setup: the vend for {identity_id:?} must be served, got {} with {}",
            vended.status,
            vended.text()
        );
        vended.json()["Credentials"]["AccessKeyId"]
            .as_str()
            .unwrap_or_else(|| {
                panic!(
                    "GetCredentialsForIdentity reports an AccessKeyId, got {}",
                    vended.text()
                )
            })
            .to_string()
    }

    /// Create the function `name` running [`SOURCE`] and wait until Lambda reports it
    /// invocable.
    async fn function(&self, name: &str) {
        let package = crate::synchronous_invoke_run::package(SOURCE);
        let created = self
            .lambda(
                ACCOUNT_KEY,
                "POST",
                FUNCTIONS_URI,
                &serde_json::to_vec(&json!({
                    "FunctionName": name,
                    "Role": EXECUTION_ROLE,
                    "Runtime": "python3.12",
                    "Handler": HANDLER,
                    "Timeout": 5,
                    "Code": {
                        "ZipFile": base64::engine::general_purpose::STANDARD.encode(package),
                    },
                }))
                .expect("the create body serializes"),
                &[],
            )
            .await;
        assert_eq!(
            created.status,
            201,
            "setup: CreateFunction({name:?}) must succeed, got {} with {}",
            created.status,
            created.text()
        );
        self.poll_until_active(name).await;
    }

    /// Look at the function's reported `State` until it is the one an invocation is served in,
    /// or [`READY_DEADLINE`] passes: the transition is one Lambda itself owes, so what this
    /// waits for is the CONDITION rather than a span.
    async fn poll_until_active(&self, name: &str) {
        let deadline = std::time::Instant::now() + READY_DEADLINE;
        loop {
            let reported = self
                .lambda(
                    ACCOUNT_KEY,
                    "GET",
                    &format!("{FUNCTIONS_URI}/{name}/configuration"),
                    b"",
                    &[],
                )
                .await
                .json();
            if reported.get("State").and_then(Value::as_str) == Some("Active") {
                return;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "setup: {name:?} never became invocable, it reports {reported}"
            );
            tokio::time::sleep(POLL).await;
        }
    }

    /// `Invoke` the function `name` as whoever holds `access_key_id`, answering what the
    /// handler reported about its own `context.identity`.
    async fn reported_identity(&self, case: &str, access_key_id: &str, name: &str) -> Value {
        let answer = self
            .lambda(
                access_key_id,
                "POST",
                &format!("{FUNCTIONS_URI}/{name}/invocations"),
                b"{}",
                &[],
            )
            .await;
        assert_eq!(
            answer.status,
            200,
            "case {case:?}: the invocation must be served, got {} with {}",
            answer.status,
            answer.text()
        );
        answer.json()
    }
}

/// The inline policy every principal below carries: the invoking grant, so what each row
/// reports is the IDENTITY the run saw and never an authorization refusal.
///
/// Written without the characters that are significant in an
/// `application/x-www-form-urlencoded` body (`&`, `=`, `+`, `%`, space), so it needs no
/// encoding.
fn invoke_grant() -> String {
    "{\"Version\":\"2012-10-17\",\"Statement\":[{\"Effect\":\"Allow\",\
     \"Action\":\"lambda:InvokeFunction\",\"Resource\":\"*\"}]}"
        .to_string()
}

/// A role trust policy admitting the Amazon Cognito Identity principal for the identity pool
/// `audience`, written the way the Amazon Cognito Developer Guide writes one: the `Federated`
/// principal, the web-identity assume, and the `:aud` condition restricting the assume to the
/// users of that one pool.
fn pool_trust_policy(audience: &str) -> String {
    format!(
        "{{\"Version\":\"2012-10-17\",\"Statement\":[{{\"Effect\":\"Allow\",\
         \"Principal\":{{\"Federated\":\"cognito-identity.amazonaws.com\"}},\
         \"Action\":\"sts:AssumeRoleWithWebIdentity\",\"Condition\":{{\"StringEquals\":\
         {{\"cognito-identity.amazonaws.com:aud\":\"{audience}\"}}}}}}]}}"
    )
}

/// A role trust policy admitting the account's own principals to assume the role, which is
/// what the contrast row's `sts:AssumeRole` needs.
fn account_trust_policy() -> String {
    let account = engine::emulator_account_id();
    format!(
        "{{\"Version\":\"2012-10-17\",\"Statement\":[{{\"Effect\":\"Allow\",\
         \"Principal\":{{\"AWS\":\"arn:aws:iam::{account}:root\"}},\
         \"Action\":\"sts:AssumeRole\"}}]}}"
    )
}

/// The text of the first `<name>…</name>` element of an XML body.
fn xml_element(body: &str, name: &str) -> String {
    let open = format!("<{name}>");
    let close = format!("</{name}>");
    let start = body
        .find(&open)
        .unwrap_or_else(|| panic!("no <{name}> in {body}"))
        + open.len();
    let end = body[start..]
        .find(&close)
        .unwrap_or_else(|| panic!("no </{name}> in {body}"))
        + start;
    body[start..end].to_string()
}

/// What one row expects the handler to have read off its context: an identity and a pool, or
/// neither.
struct Expected {
    identity_id: Option<String>,
    identity_pool_id: Option<String>,
}

impl Expected {
    /// The identity `identity_id` of the pool `pool_id`.
    fn of(identity_id: &str, pool_id: &str) -> Self {
        Self {
            identity_pool_id: Some(pool_id.to_string()),
            identity_id: Some(identity_id.to_string()),
        }
    }

    /// No Cognito identity at all, which is what a caller holding any other credential is.
    fn none() -> Self {
        Self {
            identity_id: None,
            identity_pool_id: None,
        }
    }

    /// This expectation as the handler reports it: the two members, each a string or the JSON
    /// null a `None` member of `context.identity` is serialized as.
    fn document(&self) -> Value {
        json!({
            "cognito_identity_id": self.identity_id,
            "cognito_identity_pool_id": self.identity_pool_id,
        })
    }
}

#[tokio::test]
async fn an_invocation_reports_the_cognito_identity_its_credentials_were_vended_to() {
    let _stream = one_invocation_stream_at_a_time().await;
    let emulator = Emulator::new();

    // Two pools, each with a role of its own whose trust policy admits only that pool's
    // users: which pool vended the credentials is one of the things the rule generalises
    // over.
    let first_pool = emulator.identity_pool("reporters").await;
    let second_pool = emulator.identity_pool("other-reporters").await;
    let first_role = emulator
        .role("pool-one-unauth", &pool_trust_policy(&first_pool))
        .await;
    let second_role = emulator
        .role("pool-two-unauth", &pool_trust_policy(&second_pool))
        .await;
    emulator.pool_roles(&first_pool, &first_role).await;
    emulator.pool_roles(&second_pool, &second_role).await;

    // Three identities: two of the first pool, so two sessions of ONE pool are told apart,
    // and one of the second, so the pool reported is the pool that vended.
    let one = emulator.get_id(&first_pool).await;
    let two = emulator.get_id(&first_pool).await;
    let elsewhere = emulator.get_id(&second_pool).await;

    // The contrast principals: an IAM user's long-term key, a session of a role assumed
    // through the token service, and the account's own key.
    let user_key = emulator.user("reporter-user").await;
    let plain_role = emulator
        .role("assumed-by-the-account", &account_trust_policy())
        .await;
    let session_key = emulator.assumed_session(&plain_role).await;

    emulator.function("identity-reporter").await;

    // The rows differ along the kind of credential the invoke is signed with, the identity it
    // was vended to, the pool that vended it, and — for one identity — a second, independent
    // vend.
    let rows: Vec<(&str, String, Expected)> = vec![
        (
            "a session vended to one identity of a pool",
            emulator.vended_key(&one).await,
            Expected::of(&one, &first_pool),
        ),
        (
            "a session vended to ANOTHER identity of the same pool",
            emulator.vended_key(&two).await,
            Expected::of(&two, &first_pool),
        ),
        (
            "a second, independent vend to the first identity",
            emulator.vended_key(&one).await,
            Expected::of(&one, &first_pool),
        ),
        (
            "a session vended by another pool",
            emulator.vended_key(&elsewhere).await,
            Expected::of(&elsewhere, &second_pool),
        ),
        (
            "an IAM user's own long-term access key",
            user_key,
            Expected::none(),
        ),
        (
            "an assumed-role session of the same account",
            session_key,
            Expected::none(),
        ),
        (
            "the account's own credential",
            ACCOUNT_KEY.to_string(),
            Expected::none(),
        ),
    ];

    for (case, access_key_id, expected) in rows {
        let reported = emulator
            .reported_identity(case, &access_key_id, "identity-reporter")
            .await;
        let want = expected.document();
        assert_eq!(
            reported, want,
            "case {case:?}: expected the handler to read {want}, got {reported}"
        );
    }
}
