// SPDX-License-Identifier: CC0-1.0

//! Parameter Store's reserved `/aws/reference/secretsmanager` front door, driven end to end
//! over the four services that decide it: Secrets Manager holds the secret and its versions,
//! KMS holds the key its value is sealed under, IAM holds the role and the document in force
//! on it, STS mints the session the read is signed with, and Systems Manager serves the read.
//!
//! RULE under test: a `GetParameter` name under that reserved hierarchy names a SECRET rather
//! than a parameter, so the read is resolved against Secrets Manager and reported as a
//! `SecureString` parameter of the SECRET's identity — the reference path as the request wrote
//! it, the secret's ARN, no parameter version of its own, and the source's own answer in
//! `SourceResult` — with `WithDecryption` asking for the secret's bytes. WHETHER those bytes
//! are served is the SECRETS MANAGER decision, taken on `secretsmanager:GetSecretValue` for
//! that secret against the documents in force on the caller at the instant of the request, not
//! on the caller's Parameter Store permission: a caller that may call `ssm:GetParameter` on
//! every parameter but may not read the secret is refused HTTP 400 `ValidationException`
//! carrying Parameter Store's one sentence for a downstream call that failed, and is served no
//! secret bytes, while its reads of the account's own parameters are untouched; adding that one
//! grant makes the identical read succeed.
//!
//! Only a cross-service suite can pin this as a client meets it: the secret, its key, the role,
//! the document in force on it and the session the read is signed with each belong to a
//! different service, and the front door's answer is the composition of all of them.
//!
//! The rows span what the rule generalises over: the form of the secret's name (flat,
//! hierarchical, punctuated) and of its value (a plain token, a JSON document, a multi-line
//! string), whether the read asks for decryption, whether the reference resolves at all, and
//! what the document in force says about the Secrets Manager read — nothing, another action of
//! that service, this action on this secret's ARN, this action on another secret's ARN, this
//! action on everything, and a broad grant an explicit `Deny` overrides.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The form-encoder and the XML element reader every awsQuery suite of this crate shares,
// written ONCE in the sibling that owns them.
use crate::query_wire::{element, form_encode};

/// The Region every request below is signed for.
const REGION: &str = "us-west-2";

/// The long-term key every emulator publishes, which is the account's own credential: no IAM
/// document bounds it, so it is the unrestricted caller every fixture is built with and the
/// baseline reference read is served to.
const AMBIENT_KEY: &str = "AKIAIOSFODNN7EXAMPLE";

/// The reserved hierarchy, as a client writes it.
const REFERENCE_ROOT: &str = "/aws/reference/secretsmanager";

/// `iam`'s and `sts`' own awsQuery API versions, the `Version` each call carries.
const IAM_API_VERSION: &str = "2010-05-08";
const STS_API_VERSION: &str = "2011-06-15";

/// The role whose document decides the reads of the second suite, and the name of its inline
/// policy.
const ROLE: &str = "emu-reference-reader";
const ROLE_POLICY_NAME: &str = "inline";

/// An ordinary `String` parameter of the account, read beside every reference read so a
/// caller's Parameter Store reads are seen to be untouched by the Secrets Manager decision.
const ORDINARY_NAME: &str = "/emu/front-door/ordinary";
const ORDINARY_VALUE: &str = "ordinary-parameter-value";

/// The sentence Parameter Store refuses a reference read with when the service it depends on
/// would not serve the value: its one wrapper for a failed downstream call, whichever way that
/// call failed. Written out HERE rather than read from the crate: the wording is the
/// requirement, so a drift in what the handler answers fails this suite instead of travelling
/// with it.
const DEPENDENCY_FAILURE: &str = "An error occurred while calling one AWS dependency service.";

/// The `ParameterType` a reference reports, from the modelled enum
/// (`smithy_query(query="shape", shape="ParameterType")`).
const REFERENCE_TYPE: &str = "SecureString";

/// The `ParameterDataType` it reports: the default (`smithy_query(query="doc",
/// shape="Parameter", member="DataType")` — "The data type of the parameter, such as text or
/// aws:ec2:image. The default is text").
const REFERENCE_DATA_TYPE: &str = "text";

/// The `Parameter$Version` a reference reports: the member's own modelled default
/// (`smithy_query(query="shape", shape="Parameter")` reads `"Version": { "default": 0 }`), a
/// reference carrying no parameter version of its own.
const REFERENCE_VERSION: i64 = 0;

/// The five services this front door needs, over ONE shared engine, as
/// `core/router/src/lib.rs` mounts them.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    // These cases exercise operation behaviour, not request signing: the `Authorization`
    // header carries only the credential scope, which is where each service reads the Region
    // and the caller's access key id.
    router.set_request_authentication(false);
    router.register_service(service_com_amazonaws_ssm_amazonssm::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_secretsmanager_secretsmanager::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(service_com_amazonaws_kms_trentservice::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_sts_awssecuritytokenservicev20110615::EmulatedService::new(engine),
    );
    router
}

/// A SigV4 `Authorization` header whose credential scope names `signing_name` and whose
/// credential is `access_key_id` — the key a service reads the caller's identity from. The
/// signature is never checked: request authentication is off on this router.
fn authorization(access_key_id: &str, signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential={access_key_id}/20260101/{REGION}/{signing_name}/\
         aws4_request, SignedHeaders=host;x-amz-target, Signature=0000"
    )
}

/// One `awsJson1_1` call to a service of this emulator, signed with `access_key_id`, reported
/// as `(status, the parsed body)`.
async fn json_call(
    router: &MultiServiceRouter,
    signing_name: &str,
    target_prefix: &str,
    access_key_id: &str,
    operation: &str,
    body: &Value,
) -> (u16, Value) {
    let host = format!("{signing_name}.{REGION}.amazonaws.com");
    let request = http::Request::builder()
        .method("POST")
        .uri(format!("https://{host}/"))
        .header("host", &host)
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("{target_prefix}.{operation}"))
        .header("authorization", authorization(access_key_id, signing_name))
        .body(Bytes::from(
            serde_json::to_vec(body).expect("the request body serializes"),
        ))
        .expect("a well-formed awsJson1_1 request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let raw = response.into_body();
    let parsed: Value = if raw.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&raw).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// One Systems Manager call, signed with `access_key_id`.
async fn ssm(
    router: &MultiServiceRouter,
    access_key_id: &str,
    operation: &str,
    body: &Value,
) -> (u16, Value) {
    json_call(router, "ssm", "AmazonSSM", access_key_id, operation, body).await
}

/// One Secrets Manager call, signed with the account's own credential.
async fn secrets(router: &MultiServiceRouter, operation: &str, body: &Value) -> (u16, Value) {
    json_call(
        router,
        "secretsmanager",
        "secretsmanager",
        AMBIENT_KEY,
        operation,
        body,
    )
    .await
}

/// One awsQuery form call to `signing_name`, signed with the account's own credential, reported
/// as `(status, the XML text the wire carried)`.
async fn query(router: &MultiServiceRouter, signing_name: &str, body: String) -> (u16, String) {
    let host = format!("{signing_name}.{REGION}.amazonaws.com");
    let request = http::Request::builder()
        .method("POST")
        .uri(format!("https://{host}/"))
        .header("host", &host)
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization(AMBIENT_KEY, signing_name))
        .body(Bytes::from(body))
        .expect("a well-formed awsQuery request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (status, String::from_utf8_lossy(response.body()).to_string())
}

/// The wire error code of an error response, as the short shape name a client reads.
fn error_code(body: &Value) -> String {
    let raw = body
        .get("__type")
        .and_then(Value::as_str)
        .unwrap_or_default();
    raw.split('#').next_back().unwrap_or(raw).to_string()
}

/// The error body's `message`, under either casing the envelope may carry it in.
fn error_message(body: &Value) -> String {
    body.get("message")
        .or_else(|| body.get("Message"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// A required string member of a body.
fn text(parsed: &Value, member: &str, case: &str) -> String {
    parsed
        .get(member)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: no {member} in {parsed}"))
        .to_string()
}

/// The ARN the account holds [`ROLE`] under.
fn role_arn() -> String {
    format!("arn:aws:iam::{}:role/{ROLE}", ::engine::emulator_account_id())
}

/// A role trust policy admitting this account's own principals to `sts:AssumeRole`.
fn trust_policy() -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "AWS": format!("arn:aws:iam::{}:root", ::engine::emulator_account_id()) },
            "Action": "sts:AssumeRole",
        }],
    })
    .to_string()
}

/// A statement list allowing each `(action, resource)` pair and nothing else, as
/// `iam:PutRolePolicy` stores one.
fn allow(grants: &[(&str, &str)]) -> String {
    let statements: Vec<Value> = grants
        .iter()
        .map(|(action, resource)| {
            json!({ "Effect": "Allow", "Action": action, "Resource": resource })
        })
        .collect();
    json!({ "Version": "2012-10-17", "Statement": statements }).to_string()
}

/// The secret a case reads through the front door, and what it holds.
struct Reference {
    /// What a failing assertion calls this row.
    case: &'static str,
    /// The secret's name, which is also the leaf of its reference path.
    name: &'static str,
    /// The `SecretString` the create stores.
    value: &'static str,
}

/// The reference the recorded check reads, beside the other name and value forms the rule
/// covers: the hierarchy level below the reserved path is the whole of the secret's id,
/// whatever shape that id has, and the value is served as the source holds it.
const REFERENCES: &[Reference] = &[
    Reference {
        case: "a flat name holding a plain token",
        name: "emu-front-door-token",
        value: "reference-guarded-value",
    },
    Reference {
        case: "a hierarchical name holding a JSON document",
        name: "emu/front-door/db-credentials",
        value: r#"{"username":"admin","password":"first-one"}"#,
    },
    Reference {
        case: "a punctuated name holding a multi-line value",
        name: "emu.front_door-1",
        value: "line-one\nline-two\nline-three",
    },
];

/// The reference path a read asks for `name` by.
fn reference_path(name: &str) -> String {
    format!("{REFERENCE_ROOT}/{name}")
}

/// The `ClientRequestToken` a create names its first version by, of the modelled length
/// (`smithy_query(query="shape", shape="SecretVersionIdType")` reads `{"min": 32, "max":
/// 64}`), built from the secret's own name so a failing case names the version it meant. A
/// client that is not an AWS SDK supplies this itself: "If you use the Amazon Web Services CLI
/// or one of the Amazon Web Services SDKs to call this operation, then you can leave this
/// parameter empty" (`CreateSecretRequest$ClientRequestToken`).
fn request_token(seed: &str) -> String {
    let readable: String = seed
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character
            } else {
                '-'
            }
        })
        .collect();
    format!("{readable:0<36}")
}

/// Create the secret through Secrets Manager's own `CreateSecret`, reported as
/// `(the secret's ARN, the version id of the value it stored)`.
async fn given_secret(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    value: &str,
) -> (String, String) {
    let (status, created) = secrets(
        router,
        "CreateSecret",
        &json!({
            "Name": name,
            "SecretString": value,
            "ClientRequestToken": request_token(name),
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: setup CreateSecret({name:?}) expected 200, got {status} {created}"
    );
    (
        text(&created, "ARN", case),
        text(&created, "VersionId", case),
    )
}

/// Store [`ORDINARY_NAME`] through Parameter Store's own `PutParameter`.
async fn given_ordinary_parameter(router: &MultiServiceRouter) {
    let (status, body) = ssm(
        router,
        AMBIENT_KEY,
        "PutParameter",
        &json!({ "Name": ORDINARY_NAME, "Value": ORDINARY_VALUE, "Type": "String" }),
    )
    .await;
    assert_eq!(
        status, 200,
        "setup PutParameter({ORDINARY_NAME:?}) expected 200, got {status} {body}"
    );
}

/// Read [`ORDINARY_NAME`] as `access_key_id`: the account's own parameter, which Parameter
/// Store's own permission decides and which the Secrets Manager decision never touches.
async fn ordinary_read_is_served(router: &MultiServiceRouter, access_key_id: &str, case: &str) {
    let (status, body) = ssm(
        router,
        access_key_id,
        "GetParameter",
        &json!({ "Name": ORDINARY_NAME }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the ordinary parameter must still read, got {status} {body}"
    );
    assert_eq!(
        body["Parameter"]["Value"].as_str(),
        Some(ORDINARY_VALUE),
        "case {case:?}: expected {ORDINARY_VALUE:?} from {ORDINARY_NAME}, got {body}"
    );
}

#[tokio::test]
async fn the_reserved_hierarchy_serves_the_secret_to_the_accounts_own_credential() {
    let router = build_router();
    given_ordinary_parameter(&router).await;

    for reference in REFERENCES {
        let case = reference.case;
        let (arn, version_id) =
            given_secret(&router, case, reference.name, reference.value).await;
        let path = reference_path(reference.name);

        // The baseline read of the recorded check: the account's own unrestricted credential,
        // asking for decryption, is served the secret's bytes as a parameter of the SECRET's
        // identity.
        let (status, body) = ssm(
            &router,
            AMBIENT_KEY,
            "GetParameter",
            &json!({ "Name": path, "WithDecryption": true }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: the reference {path} must resolve, got {status} {body}"
        );
        let parameter = &body["Parameter"];
        assert_eq!(
            parameter["Value"].as_str(),
            Some(reference.value),
            "case {case:?}: expected the secret's own bytes, got {body}"
        );
        assert_eq!(
            parameter["Name"].as_str(),
            Some(path.as_str()),
            "case {case:?}: the read is answered under the name it asked for, got {body}"
        );
        assert_eq!(
            parameter["Type"].as_str(),
            Some(REFERENCE_TYPE),
            "case {case:?}: expected {REFERENCE_TYPE}, got {body}"
        );
        assert_eq!(
            parameter["DataType"].as_str(),
            Some(REFERENCE_DATA_TYPE),
            "case {case:?}: expected {REFERENCE_DATA_TYPE}, got {body}"
        );
        assert_eq!(
            parameter["ARN"].as_str(),
            Some(arn.as_str()),
            "case {case:?}: the parameter's ARN is the SECRET's, got {body}"
        );
        assert_eq!(
            parameter["Version"].as_i64(),
            Some(REFERENCE_VERSION),
            "case {case:?}: a reference carries no parameter version, got {body}"
        );

        // `SourceResult` is "the raw result or response from the source", so it names the
        // version Secrets Manager resolved and carries that version's own value.
        let source: Value = serde_json::from_str(
            parameter["SourceResult"]
                .as_str()
                .unwrap_or_else(|| panic!("case {case:?}: no SourceResult in {body}")),
        )
        .unwrap_or_else(|_| panic!("case {case:?}: SourceResult is not a document: {body}"));
        assert_eq!(
            source["versionId"].as_str(),
            Some(version_id.as_str()),
            "case {case:?}: the source's answer names the version it served, got {source}"
        );
        assert_eq!(
            source["secretString"].as_str(),
            Some(reference.value),
            "case {case:?}: the source's answer carries the version's value, got {source}"
        );

        // What the front door served is what Secrets Manager itself answers for that version.
        let (status, direct) = secrets(
            &router,
            "GetSecretValue",
            &json!({ "SecretId": reference.name, "VersionId": version_id }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: GetSecretValue({:?}) expected 200, got {status} {direct}",
            reference.name
        );
        assert_eq!(
            direct["SecretString"].as_str(),
            parameter["Value"].as_str(),
            "case {case:?}: the reference serves what GetSecretValue answers, got {direct} \
             against {parameter}"
        );

        // `WithDecryption` is what asks for those bytes: Parameter Store holds no ciphertext
        // of its own for a reference, so a read that asks for none is served the reference's
        // identity and no value at all.
        let (status, undecrypted) =
            ssm(&router, AMBIENT_KEY, "GetParameter", &json!({ "Name": path })).await;
        assert_eq!(
            status, 200,
            "case {case:?}: an undecrypted reference read must still resolve, got {status} \
             {undecrypted}"
        );
        assert_eq!(
            undecrypted["Parameter"]["Value"].as_str(),
            None,
            "case {case:?}: a read asking for no decryption serves no value, got {undecrypted}"
        );
        assert_eq!(
            undecrypted["Parameter"]["Type"].as_str(),
            Some(REFERENCE_TYPE),
            "case {case:?}: expected {REFERENCE_TYPE}, got {undecrypted}"
        );
    }

    // A reference naming a secret this Region holds none of resolves to nothing, and the
    // refusal names the reference it could not resolve.
    let missing = reference_path("emu-front-door-absent");
    let (status, body) = ssm(
        &router,
        AMBIENT_KEY,
        "GetParameter",
        &json!({ "Name": missing, "WithDecryption": true }),
    )
    .await;
    assert_eq!(
        (status, error_code(&body).as_str()),
        (400, "ParameterNotFound"),
        "case \"a reference nothing resolves\": expected the modelled 400, got {status} {body}"
    );
    assert!(
        error_message(&body).contains("emu-front-door-absent"),
        "case \"a reference nothing resolves\": the refusal must name the reference, got {body}"
    );

    // Reading a reference creates nothing: the reserved hierarchy is not the account's.
    let (status, listed) = ssm(&router, AMBIENT_KEY, "DescribeParameters", &json!({})).await;
    assert_eq!(
        status, 200,
        "case \"the account's own parameters\": DescribeParameters expected 200, got {status} \
         {listed}"
    );
    let names: Vec<String> = listed["Parameters"]
        .as_array()
        .map(|parameters| {
            parameters
                .iter()
                .map(|parameter| parameter["Name"].as_str().unwrap_or_default().to_string())
                .collect()
        })
        .unwrap_or_default();
    assert!(
        !names.iter().any(|name| name.starts_with(REFERENCE_ROOT)),
        "case \"the account's own parameters\": no parameter is created by reading a \
         reference, got {names:?}"
    );
}

/// The credential one session was minted under: the access key id a later request signs with,
/// and the ARN `sts:AssumeRole` reports for it.
struct Session {
    access_key_id: String,
    arn: String,
}

/// Hold [`ROLE`] through IAM's own `CreateRole` and mint a session of it through a real
/// `sts:AssumeRole`.
async fn given_session(router: &MultiServiceRouter) -> Session {
    let (status, created) = query(
        router,
        "iam",
        format!(
            "Action=CreateRole&Version={IAM_API_VERSION}&RoleName={ROLE}\
             &AssumeRolePolicyDocument={}",
            form_encode(&trust_policy())
        ),
    )
    .await;
    assert_eq!(
        status, 200,
        "setup CreateRole({ROLE:?}): expected 200, got {status} {created}"
    );
    let (status, assumed) = query(
        router,
        "sts",
        format!(
            "Action=AssumeRole&Version={STS_API_VERSION}&RoleArn={}&RoleSessionName=front-door",
            form_encode(&role_arn())
        ),
    )
    .await;
    assert_eq!(
        status, 200,
        "setup AssumeRole({ROLE:?}): expected 200, got {status} {assumed}"
    );
    Session {
        access_key_id: element(&assumed, "AccessKeyId"),
        arn: element(&assumed, "Arn"),
    }
}

/// Put `document` in force on the role as its inline policy, replacing whatever it carried,
/// through IAM's own `PutRolePolicy` — so the next request is decided against it.
async fn given_role_policy(router: &MultiServiceRouter, document: &str) {
    let (status, body) = query(
        router,
        "iam",
        format!(
            "Action=PutRolePolicy&Version={IAM_API_VERSION}&RoleName={ROLE}\
             &PolicyName={ROLE_POLICY_NAME}&PolicyDocument={}",
            form_encode(document)
        ),
    )
    .await;
    assert_eq!(
        status, 200,
        "setup PutRolePolicy({ROLE:?}): expected 200, got {status} {body}"
    );
}

#[tokio::test]
async fn the_secrets_manager_grant_in_force_decides_each_reference_read() {
    let router = build_router();
    given_ordinary_parameter(&router).await;

    let guarded = &REFERENCES[0];
    let case = guarded.case;
    let (arn, _) = given_secret(&router, case, guarded.name, guarded.value).await;
    let path = reference_path(guarded.name);
    // A second secret of the same account, so a row can grant the read on a secret that is
    // really held and is not the one the read addresses.
    let (other_arn, _) = given_secret(
        &router,
        "another secret of this account",
        "emu-front-door-elsewhere",
        "another-guarded-value",
    )
    .await;
    let session = given_session(&router).await;

    // Each row is the WHOLE authority of one session, rewritten in force before the identical
    // read: every row may call `ssm:GetParameter` on every parameter, so what differs is only
    // what the document says about the Secrets Manager read of this secret — which is what
    // decides whether the bytes are served.
    let rows: Vec<(&str, String, bool)> = vec![
        (
            "every Parameter Store read allowed, nothing of Secrets Manager",
            allow(&[("ssm:GetParameter", "*")]),
            false,
        ),
        (
            "every Parameter Store read allowed, another Secrets Manager action",
            allow(&[
                ("ssm:GetParameter", "*"),
                ("secretsmanager:DescribeSecret", "*"),
            ]),
            false,
        ),
        (
            "the Secrets Manager read allowed on this secret's ARN",
            allow(&[
                ("ssm:GetParameter", "*"),
                ("secretsmanager:DescribeSecret", "*"),
                ("secretsmanager:GetSecretValue", arn.as_str()),
            ]),
            true,
        ),
        (
            "the Secrets Manager read allowed on another secret only",
            allow(&[
                ("ssm:GetParameter", "*"),
                ("secretsmanager:GetSecretValue", other_arn.as_str()),
            ]),
            false,
        ),
        (
            "the Secrets Manager read allowed on everything",
            allow(&[("ssm:GetParameter", "*"), ("secretsmanager:GetSecretValue", "*")]),
            true,
        ),
        (
            "a broad grant an explicit deny of the Secrets Manager read overrides",
            json!({
                "Version": "2012-10-17",
                "Statement": [
                    { "Effect": "Allow", "Action": "*", "Resource": "*" },
                    {
                        "Effect": "Deny",
                        "Action": "secretsmanager:GetSecretValue",
                        "Resource": "*",
                    },
                ],
            })
            .to_string(),
            false,
        ),
    ];

    for (case, document, served) in rows {
        given_role_policy(&router, &document).await;
        let (status, body) = ssm(
            &router,
            &session.access_key_id,
            "GetParameter",
            &json!({ "Name": path, "WithDecryption": true }),
        )
        .await;

        if served {
            assert_eq!(
                status, 200,
                "case {case:?}: {} must be served the reference, got {status} {body}",
                session.arn
            );
            assert_eq!(
                body["Parameter"]["Value"].as_str(),
                Some(guarded.value),
                "case {case:?}: the served value is the secret's own, got {body}"
            );
        } else {
            assert_eq!(
                (status, error_code(&body).as_str()),
                (400, "ValidationException"),
                "case {case:?}: {} must be refused as the dependency failing, got {status} \
                 {body}",
                session.arn
            );
            assert_eq!(
                error_message(&body),
                DEPENDENCY_FAILURE,
                "case {case:?}: expected {DEPENDENCY_FAILURE:?}, got {body}"
            );
            assert!(
                !body.to_string().contains(guarded.value),
                "case {case:?}: a refused read serves no secret bytes, got {body}"
            );
        }

        // Whatever the Secrets Manager decision was, this session's read of the account's own
        // parameter is Parameter Store's own decision and is served.
        ordinary_read_is_served(&router, &session.access_key_id, case).await;
    }
}
