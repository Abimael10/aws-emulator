// SPDX-License-Identifier: CC0-1.0

//! Whether IAM lets a call to this service through, driven end to end over the wire against
//! the services that hold the identity and its policies.
//!
//! The RULE under test, one level above the recorded case (one role, no policy, one create):
//!
//! > Every operation of this service is authorized against the IDENTITY-BASED policies in
//! > force on the principal the request's credential names, on the action
//! > `chime:<ApiName>` and the resource the request addresses, BEFORE the operation runs: a
//! > call no statement allows (IAM's default deny) and one an explicit `Deny` covers are
//! > both refused `AccessDeniedException` (HTTP 403) naming the principal, the action, the
//! > resource and which of IAM's rules refused it, with nothing minted, moved or reported,
//! > while a call a statement allows is served. The account's own published credential,
//! > which holds no IAM policies, is the position AWS account root credentials are in and is
//! > served.
//!
//! The rows span what the rule generalises over: the statements a policy can carry (no
//! policy at all, an allow of the exact action, a service-wide wildcard allow, an allow of
//! another action only, an allow scoped to another resource, an explicit deny beside a
//! wildcard allow), the operation asked for (a create, a listing, a read, and one still
//! scaffolded), and the credential the call is signed with.
//!
//! The harness is `pub(crate)`: the sibling suite
//! [`crate::resource_access_role_resolution`] needs the same three services mounted on one
//! engine and must not build a second copy of it.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use service_com_amazonaws_chimesdkmediapipelines_chimesdkmediapipelinesservice::service::authorization::GATED_OPERATIONS;
use service_com_amazonaws_chimesdkmediapipelines_chimesdkmediapipelinesservice::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// This service's own Smithy model, and the service shape the gated operations are bound to
/// — the model is the authority on which operations exist, so the suite reads it rather than
/// restating a list.
const MODEL_PATH: &str =
    "smithy-model/com-amazonaws-chimesdkmediapipelines-chimesdkmediapipelinesservice.json";
const SERVICE_SHAPE: &str = "com.amazonaws.chimesdkmediapipelines#ChimeSDKMediaPipelinesService";

/// The region every request in this suite is signed for.
pub(crate) const REGION: &str = "us-west-2";

/// The host this service's own endpoint is addressed on.
const ENDPOINT_HOST: &str = "media-pipelines-chime.us-west-2.amazonaws.com";

/// The emulator's own published long-term access key: the account credential, which holds no
/// IAM policies.
pub(crate) const ACCOUNT_KEY: &str = "AKIDEXAMPLE";

/// The code and status AWS refuses a call IAM does not permit with — the pair recorded
/// behaviour shows, for which this model declares no shape.
const DENIED_CODE: &str = "AccessDeniedException";
const DENIED_STATUS: u16 = 403;

/// The collection routes the rows drive, from `smithy_query(query="digest")`.
pub(crate) const POOLS: &str = "/media-pipeline-kinesis-video-stream-pools";
pub(crate) const CONFIGURATIONS: &str = "/media-insights-pipeline-configurations";

/// The resource TYPE a Kinesis Video Stream pool's ARN carries, as this service's own ARN
/// builder derives it.
const POOL_RESOURCE_TYPE: &str = "media-pipeline-kinesis-video-stream-pool";

/// The router every case drives: this service, and the two that own the identity a request
/// speaks for (STS) and the policies in force on it (IAM) — all on ONE engine.
pub(crate) struct Harness {
    router: MultiServiceRouter,
}

impl Harness {
    pub(crate) fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        router.set_request_authentication(false);
        router.register_service(EmulatedService::new(Arc::clone(&engine)));
        router.register_service(
            service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(
                Arc::clone(&engine),
            ),
        );
        router.register_service(
            service_com_amazonaws_sts_awssecuritytokenservicev20110615::EmulatedService::new(
                engine,
            ),
        );
        Self { router }
    }

    /// The SigV4 credential scope a request signed with `access_key_id` carries. Its service
    /// field is the routing key; this router verifies no signature, so the credential is read
    /// for its key id and its Region alone — which is what the gate resolves an identity and
    /// builds an ARN from.
    fn authorization(access_key_id: &str, signing_name: &str) -> String {
        format!(
            "AWS4-HMAC-SHA256 Credential={access_key_id}/20261101/{REGION}/{signing_name}/\
             aws4_request, SignedHeaders=host;x-amz-date, Signature=0000"
        )
    }

    /// One call to this service, signed with `access_key_id`.
    pub(crate) async fn chime(
        &self,
        access_key_id: &str,
        method: &str,
        path_and_query: &str,
        body: Option<Value>,
    ) -> Answer {
        let payload = body.map_or_else(Vec::new, |value| {
            serde_json::to_vec(&value).expect("a JSON value serializes")
        });
        let request = http::Request::builder()
            .method(method)
            .uri(format!("https://{ENDPOINT_HOST}{path_and_query}"))
            .header("host", ENDPOINT_HOST)
            .header("content-type", "application/json")
            .header("authorization", Self::authorization(access_key_id, "chime"))
            .body(Bytes::from(payload))
            .expect("a well-formed request");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let error_type = response
            .headers()
            .get("x-amzn-errortype")
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        let raw = response.into_body();
        let body = if raw.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&raw).unwrap_or(Value::Null)
        };
        Answer {
            status,
            error_type,
            body,
        }
    }

    /// One awsQuery call to IAM or STS, always with the account's own credential:
    /// `signing_name` picks the service, `version` its API version.
    async fn query(
        &self,
        signing_name: &str,
        version: &str,
        action: &str,
        params: &[(&str, String)],
    ) -> (u16, String) {
        let mut body = format!("Action={action}&Version={version}");
        for (name, value) in params {
            body.push('&');
            body.push_str(name);
            body.push('=');
            body.push_str(&form_encoded(value));
        }
        let host = format!("{signing_name}.amazonaws.com");
        let request = http::Request::builder()
            .method("POST")
            .uri(format!("https://{host}/"))
            .header("host", host)
            .header("content-type", "application/x-www-form-urlencoded")
            .header(
                "authorization",
                Self::authorization(ACCOUNT_KEY, signing_name),
            )
            .body(Bytes::from(body))
            .expect("a well-formed request");
        let response = self.router.dispatch(request).await;
        (
            response.status().as_u16(),
            String::from_utf8_lossy(&response.into_body()).to_string(),
        )
    }

    /// Create an IAM role named `role_name`, under `path` when one is given, carrying
    /// `policy` as its inline policy when one is given, and answer its ARN.
    pub(crate) async fn create_role(
        &self,
        role_name: &str,
        path: Option<&str>,
        policy: Option<&str>,
    ) -> String {
        let trust = json!({
            "Version": "2012-10-17",
            "Statement": [{
                "Effect": "Allow",
                "Principal": { "AWS": format!("arn:aws:iam::{}:root", account()) },
                "Action": "sts:AssumeRole",
            }],
        })
        .to_string();
        let mut params = vec![
            ("RoleName", role_name.to_string()),
            ("AssumeRolePolicyDocument", trust),
        ];
        if let Some(path) = path {
            params.push(("Path", path.to_string()));
        }
        let (status, body) = self.query("iam", "2010-05-08", "CreateRole", &params).await;
        assert_eq!(
            status, 200,
            "setup: creating the role {role_name:?} must succeed, got {status} {body}"
        );
        if let Some(policy) = policy {
            let (status, body) = self
                .query(
                    "iam",
                    "2010-05-08",
                    "PutRolePolicy",
                    &[
                        ("RoleName", role_name.to_string()),
                        ("PolicyName", "inline".to_string()),
                        ("PolicyDocument", policy.to_string()),
                    ],
                )
                .await;
            assert_eq!(
                status, 200,
                "setup: putting the inline policy of {role_name:?} must succeed, got \
                 {status} {body}"
            );
        }
        xml_element(&body, "Arn")
            .unwrap_or_else(|| panic!("setup: CreateRole must answer an Arn, got {body}"))
    }

    /// Delete the role `role_name` holds, so a later call names a role IAM no longer holds.
    pub(crate) async fn delete_role(&self, role_name: &str) {
        let (status, body) = self
            .query(
                "iam",
                "2010-05-08",
                "DeleteRole",
                &[("RoleName", role_name.to_string())],
            )
            .await;
        assert_eq!(
            status, 200,
            "setup: deleting the role {role_name:?} must succeed, got {status} {body}"
        );
    }

    /// Assume the role `role_name` and answer `(access key id, the session's principal ARN)`
    /// — the credential a caller then signs with, and the principal a refusal names.
    async fn assume_role(&self, role_name: &str) -> (String, String) {
        let (status, body) = self
            .query(
                "sts",
                "2011-06-15",
                "AssumeRole",
                &[
                    (
                        "RoleArn",
                        format!("arn:aws:iam::{}:role/{role_name}", account()),
                    ),
                    ("RoleSessionName", "probe".to_string()),
                ],
            )
            .await;
        assert_eq!(
            status, 200,
            "setup: assuming {role_name:?} must succeed, got {status} {body}"
        );
        let key = xml_element(&body, "AccessKeyId")
            .unwrap_or_else(|| panic!("setup: AssumeRole must answer an AccessKeyId, got {body}"));
        let principal = xml_element(&body, "Arn")
            .unwrap_or_else(|| panic!("setup: AssumeRole must answer the session Arn, got {body}"));
        (key, principal)
    }

    /// Create a role carrying `policy` (none at all when `None`), assume it, and answer the
    /// session's `(access key id, principal ARN)`.
    pub(crate) async fn session(&self, role_name: &str, policy: Option<&str>) -> (String, String) {
        self.create_role(role_name, None, policy).await;
        self.assume_role(role_name).await
    }
}

/// One answer from this service.
#[derive(Debug)]
pub(crate) struct Answer {
    pub(crate) status: u16,
    error_type: Option<String>,
    pub(crate) body: Value,
}

impl Answer {
    /// The wire error shape name, read from `x-amzn-errortype` (which restJson1 resolves the
    /// shape from) and falling back to the body's `__type`.
    pub(crate) fn error_code(&self) -> String {
        self.error_type
            .clone()
            .or_else(|| {
                self.body
                    .get("__type")
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
            .unwrap_or_default()
    }

    /// The `Message` of an error answer, whichever casing the layer that produced it spells.
    pub(crate) fn message(&self) -> String {
        self.body
            .pointer("/Message")
            .or_else(|| self.body.pointer("/message"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    }
}

/// The account every emulated ARN carries.
pub(crate) fn account() -> &'static str {
    engine::EMULATOR_ACCOUNT_ID
}

/// One form field, percent-encoded the way an awsQuery body carries it.
fn form_encoded(value: &str) -> String {
    let mut encoded = String::new();
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                encoded.push(byte as char)
            }
            other => encoded.push_str(&format!("%{other:02X}")),
        }
    }
    encoded
}

/// The text of the first `<name>` element of an awsQuery XML response.
fn xml_element(document: &str, name: &str) -> Option<String> {
    let open = format!("<{name}>");
    let close = format!("</{name}>");
    let start = document.find(&open)? + open.len();
    let end = document[start..].find(&close)? + start;
    Some(document[start..end].to_string())
}

/// An identity-based policy with one statement.
fn policy(effect: &str, actions: &[&str], resources: &[&str]) -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": effect,
            "Action": actions,
            "Resource": resources,
        }],
    })
    .to_string()
}

/// A create body for a pool named `name`.
fn pool_body(name: &str) -> Value {
    json!({
        "PoolName": name,
        "StreamConfiguration": { "Region": REGION, "DataRetentionInHours": 2 },
    })
}

/// The ARN the pool actions of a request addressing the COLLECTION are judged against: the
/// kind's own ARN with `*` in place of an identifier, which is how IAM writes the resource
/// of an action whose resource does not exist yet.
fn any_pool() -> String {
    format!(
        "arn:aws:chime:{REGION}:{}:{POOL_RESOURCE_TYPE}/*",
        account()
    )
}

/// The sentence IAM refuses a call no readable statement allows with.
fn no_policy_allows(principal: &str, action: &str, resource: &str) -> String {
    format!(
        "User: {principal} is not authorized to perform: {action} on resource: {resource} \
         because no identity-based policy allows the {action} action"
    )
}

/// Assert `answer` is the authorization refusal carrying `expected_message`.
fn assert_denied(case: &str, answer: &Answer, expected_message: &str) {
    assert_eq!(
        answer.status, DENIED_STATUS,
        "case {case:?}: expected {DENIED_STATUS}, got {} body {:?}",
        answer.status, answer.body
    );
    assert_eq!(
        answer.error_code(),
        DENIED_CODE,
        "case {case:?}: expected {DENIED_CODE}, got {:?}",
        answer.body
    );
    assert_eq!(
        answer.message(),
        expected_message,
        "case {case:?}: expected IAM's wording, got {:?}",
        answer.message()
    );
}

/// RULE: the gate judges EVERY operation the model binds to this service — read off the
/// model rather than restated here, so an operation the model gains is judged rather than
/// quietly served to a caller no policy allows.
#[test]
fn every_operation_the_model_binds_is_judged_by_the_gate() {
    let model = std::fs::read_to_string(MODEL_PATH).expect("the service's model is on disk");
    let model: Value = serde_json::from_str(&model).expect("the model is JSON");
    let bound: Vec<String> = model
        .pointer("/shapes")
        .and_then(Value::as_object)
        .and_then(|shapes| shapes.get(SERVICE_SHAPE))
        .and_then(|service| service.pointer("/operations"))
        .and_then(Value::as_array)
        .map(|operations| {
            operations
                .iter()
                .filter_map(|operation| operation.get("target").and_then(Value::as_str))
                .filter_map(|target| target.rsplit('#').next())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    assert!(
        !bound.is_empty(),
        "setup: expected the model to bind operations to {SERVICE_SHAPE}, read none"
    );

    let mut judged: Vec<String> = GATED_OPERATIONS.iter().map(|op| (*op).to_string()).collect();
    judged.sort();
    let mut bound_sorted = bound.clone();
    bound_sorted.sort();
    assert_eq!(
        judged, bound_sorted,
        "case \"the gated set\": expected the gate to judge exactly the operations the model \
         binds, got {judged:?} against {bound_sorted:?}"
    );
}

/// RULE: a create no identity-based policy allows is refused with IAM's sentence — naming
/// the session principal, the action and the resource the request addressed — and no pool is
/// created, while the same create under the account's own credential is served.
#[tokio::test]
async fn a_create_no_policy_allows_is_refused_and_mints_nothing() {
    let harness = Harness::new();
    let (key, principal) = harness.session("pool-creator-unpermissioned", None).await;

    let refused = harness
        .chime(&key, "POST", POOLS, Some(pool_body("denied-pool")))
        .await;
    assert_denied(
        "an assumed role with no attached policy",
        &refused,
        &no_policy_allows(
            &principal,
            "chime:CreateMediaPipelineKinesisVideoStreamPool",
            &any_pool(),
        ),
    );

    // Nothing was minted: the account's own credential, which this gate does not bound, sees
    // an empty listing.
    let listed = harness.chime(ACCOUNT_KEY, "GET", POOLS, None).await;
    assert_eq!(
        listed.status, 200,
        "the account credential must be served the listing, got {} body {:?}",
        listed.status, listed.body
    );
    let pools = listed
        .body
        .pointer("/KinesisVideoStreamPools")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    assert!(
        pools.is_empty(),
        "case \"a refused create\": expected no pool to exist, got {pools:?}"
    );

    // And the same create under the account's own credential is served, so the refusal is
    // about the caller's permissions and nothing else.
    let served = harness
        .chime(ACCOUNT_KEY, "POST", POOLS, Some(pool_body("served-pool")))
        .await;
    assert_eq!(
        served.status, 201,
        "case \"the account credential\": expected 201, got {} body {:?}",
        served.status, served.body
    );
}

/// RULE: the decision follows the STATEMENTS in force on the principal — an allow about the
/// action and the resource grants the call, anything else leaves IAM's default deny, and an
/// explicit deny refuses whatever else allows it.
#[tokio::test]
async fn the_decision_follows_the_statements_in_force() {
    // Each row: the case, the policy the session's role carries, and whether the create is
    // served.
    let rows: Vec<(&str, Option<String>, bool)> = vec![
        ("no policy at all", None, false),
        (
            "an allow of the exact action on every resource",
            Some(policy(
                "Allow",
                &["chime:CreateMediaPipelineKinesisVideoStreamPool"],
                &["*"],
            )),
            true,
        ),
        (
            "a service-wide wildcard allow",
            Some(policy("Allow", &["chime:*"], &["*"])),
            true,
        ),
        (
            "an allow scoped to the pool kind this create addresses",
            Some(policy(
                "Allow",
                &["chime:CreateMediaPipelineKinesisVideoStreamPool"],
                &[&any_pool()],
            )),
            true,
        ),
        (
            "an allow of another action only",
            Some(policy("Allow", &["chime:ListMediaPipelines"], &["*"])),
            false,
        ),
        (
            "an allow scoped to another service's resources",
            Some(policy(
                "Allow",
                &["chime:CreateMediaPipelineKinesisVideoStreamPool"],
                &["arn:aws:chime:us-east-1:000000000000:media-pipeline/*"],
            )),
            false,
        ),
    ];

    for (index, (case, document, served)) in rows.into_iter().enumerate() {
        let harness = Harness::new();
        let (key, _) = harness
            .session(&format!("pool-creator-{index}"), document.as_deref())
            .await;
        let answer = harness
            .chime(&key, "POST", POOLS, Some(pool_body("row-pool")))
            .await;
        if served {
            assert_eq!(
                answer.status, 201,
                "case {case:?}: expected the create to be served, got {} body {:?}",
                answer.status, answer.body
            );
        } else {
            assert_eq!(
                answer.status, DENIED_STATUS,
                "case {case:?}: expected {DENIED_STATUS}, got {} body {:?}",
                answer.status, answer.body
            );
            assert_eq!(
                answer.error_code(),
                DENIED_CODE,
                "case {case:?}: expected {DENIED_CODE}, got {:?}",
                answer.body
            );
        }
    }

    // An explicit deny beside a wildcard allow: the deny wins, and the refusal names the
    // document that refused it rather than the action nothing granted.
    let harness = Harness::new();
    let document = json!({
        "Version": "2012-10-17",
        "Statement": [
            { "Effect": "Allow", "Action": "chime:*", "Resource": "*" },
            {
                "Effect": "Deny",
                "Action": "chime:CreateMediaPipelineKinesisVideoStreamPool",
                "Resource": "*",
            },
        ],
    })
    .to_string();
    let (key, principal) = harness
        .session("pool-creator-denied", Some(&document))
        .await;
    let refused = harness
        .chime(&key, "POST", POOLS, Some(pool_body("explicitly-denied")))
        .await;
    let action = "chime:CreateMediaPipelineKinesisVideoStreamPool";
    assert_denied(
        "an explicit deny beside a wildcard allow",
        &refused,
        &format!(
            "User: {principal} is not authorized to perform: {action} on resource: {} with an \
             explicit deny in an identity-based policy",
            any_pool()
        ),
    );
}

/// RULE: the decision is taken for EVERY operation of this service and BEFORE the operation
/// runs — so a reporting call, a read of something that does not exist and an operation that
/// is still a scaffold are all refused the same way, each naming its own action and the
/// resource it addressed.
#[tokio::test]
async fn every_operation_is_judged_before_it_runs() {
    let harness = Harness::new();
    let (key, principal) = harness.session("reader-unpermissioned", None).await;

    for (case, method, path, action, resource) in [
        (
            "a listing",
            "GET",
            POOLS.to_string(),
            "chime:ListMediaPipelineKinesisVideoStreamPools",
            any_pool(),
        ),
        (
            "a read of a pool that does not exist",
            "GET",
            format!("{POOLS}/nothing-here"),
            "chime:GetMediaPipelineKinesisVideoStreamPool",
            format!(
                "arn:aws:chime:{REGION}:{}:{POOL_RESOURCE_TYPE}/nothing-here",
                account()
            ),
        ),
        (
            "a read of a configuration that does not exist",
            "GET",
            format!("{CONFIGURATIONS}/nothing-here"),
            "chime:GetMediaInsightsPipelineConfiguration",
            format!(
                "arn:aws:chime:{REGION}:{}:media-insights-pipeline-configuration/nothing-here",
                account()
            ),
        ),
        (
            "an operation that is still a scaffold",
            "GET",
            "/media-insights-pipelines/00000000-0000-4000-8000-000000000000/\
             voice-tone-analysis-tasks/11111111-1111-4111-8111-111111111111"
                .to_string(),
            "chime:GetVoiceToneAnalysisTask",
            format!(
                "arn:aws:chime:{REGION}:{}:media-pipeline/\
                 00000000-0000-4000-8000-000000000000",
                account()
            ),
        ),
    ] {
        let refused = harness.chime(&key, method, &path, None).await;
        assert_denied(
            case,
            &refused,
            &no_policy_allows(&principal, action, &resource),
        );
    }
}
