// SPDX-License-Identifier: CC0-1.0

//! The wire harness every suite in this crate drives the emulator through.
//!
//! One place for the router mount, the signed `POST /` dispatch, the decoded answer
//! and the modelled-shape predicates, so a new suite adds test ROWS rather than a
//! second copy of the transport. Every suite calls the real wire path — `POST /` with
//! `X-Amz-Target: TransferService.<Op>` through [`MultiServiceRouter`] — so what it
//! asserts is what an SDK sees, not what a handler returns.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use service_com_amazonaws_iam_awsidentitymanagementv20100508::ops::types::{
    Policy, PolicyVersion, Role, User,
};
use service_com_amazonaws_iam_awsidentitymanagementv20100508::state as iam;
use service_com_amazonaws_transfer_transferservice::state as st;
use smithy_server_runtime::MultiServiceRouter;

/// The region the test signs for. It is read back out of ARNs, so it must not be the
/// handler's fallback default everywhere.
pub(crate) const REGION: &str = "us-east-1";

/// The canonical emulator account id every ARN carries.
pub(crate) const ACCOUNT: &str = "000000000000";

/// The slack a suite waits on top of a span before reading the state the clock owes.
///
/// The span is measured from an instant the HANDLER took, before the response reached
/// the test, so a sleep of exactly the span already ends past the due instant; this
/// only keeps a suite off the boundary.
pub(crate) const SETTLE_MARGIN: Duration = Duration::from_millis(400);

/// How often [`poll_until`] re-reads a stage it is waiting on — a client's poll, kept
/// short so the wait is bounded by the stage and not by the interval.
const POLL_INTERVAL: Duration = Duration::from_millis(100);

/// How long [`poll_until`] waits for a stage before calling it stalled.
///
/// Deliberately NOT derived from the spans in `src/state.rs`: a client knows none of
/// them, so this is a client's patience — far longer than any span this emulator
/// chooses, and short enough that a stage which never settles fails the suite instead
/// of hanging it.
const SETTLE_DEADLINE: Duration = Duration::from_secs(30);

/// Wait out the span a server spends `STARTING`, whatever `src/state.rs` sets it to:
/// the span is read from the crate under test rather than copied, so a suite cannot
/// wait out a span the service no longer has.
pub(crate) const PAST_START_SPAN: Duration = st::SERVER_START_SPAN.saturating_add(SETTLE_MARGIN);

/// Wait out the span a server spends `STOPPING`, on the same basis as
/// [`PAST_START_SPAN`].
pub(crate) const PAST_STOP_SPAN: Duration = st::SERVER_STOP_SPAN.saturating_add(SETTLE_MARGIN);

/// An SSH public key whose body satisfies `SshPublicKeyBody`'s modelled pattern
/// `^\s*(ssh|ecdsa)-[a-z0-9-]+[ \t]+(([A-Za-z0-9+/]{4})*...)$`.
pub(crate) const SSH_KEY: &str = "ssh-rsa AAAAB3NzaC1yc2EAAAADAQABAAABAQ==";

/// A role ARN satisfying `Role`'s modelled pattern `^arn:.*role/\S+$` and `len 20..2048`.
pub(crate) const ROLE: &str = "arn:aws:iam::000000000000:role/transfer-s3";

/// The access key id every request this harness signs carries, so a test that seeds
/// an IAM principal seeds it against this key.
pub(crate) const ACCESS_KEY_ID: &str = "AKIDTEST";

/// A directory identifier satisfying `DirectoryId`'s modelled `len 12..12`, pattern
/// `^d-[0-9a-f]{10}$` — what an `AWS_DIRECTORY_SERVICE` server authenticates against,
/// and so what every suite about ACCESS GRANTS configures its server with.
pub(crate) const DIRECTORY_ID: &str = "d-0123456789";

/// A function ARN satisfying `Function`'s modelled `len 1..170`, pattern
/// `^arn:[a-z-]+:lambda:.*$`: the one `IdentityProviderDetails` member an `AWS_LAMBDA`
/// server is configured through.
pub(crate) const LAMBDA_IDP: &str = "arn:aws:lambda:us-east-1:000000000000:function:idp";

/// The shortest `ExternalId` the modelled `len 1..256`, pattern `^S-1-[\d-]+$` admits:
/// the low end of the group-SID space, beside the full-length SIDs a suite's own rows
/// name.
pub(crate) const SHORT_SID: &str = "S-1-0";

/// A `ServerId` satisfying the modelled `len 19..19`, pattern `^s-([0-9a-f]{17})$` that
/// no `CreateServer` ever minted, so every operation addressed at it resolves nothing.
pub(crate) const UNMINTED_SERVER_ID: &str = "s-0000000000000dead";

/// This service's own store in the shared engine, as a suite seeds and reads it.
pub(crate) type TransferState =
    service_com_amazonaws_transfer_transferservice::ComAmazonawsTransferTransferserviceState;

/// Identifiers satisfying `CertificateIds`' member shape `CertificateId`
/// (`len 22..22`, pattern `^cert-([0-9a-f]{17})$`, from
/// `smithy_query(query="shape",shape="CertificateIds")`): the low end of the id space,
/// a mid value and the high end.
pub(crate) const CERTIFICATE_IDS: [&str; 3] = [
    "cert-00000000000000000",
    "cert-0123456789abcdef0",
    "cert-fffffffffffffffff",
];

/// `ProfileType`'s whole modelled value set (`smithy_query(query="shape",
/// shape="ProfileType")` reports `[LOCAL, PARTNER]`).
pub(crate) const PROFILE_TYPES: [&str; 2] = ["LOCAL", "PARTNER"];

/// The IAM Identity Center instances the account holds in every suite of this crate.
///
/// A web app's Identity Center application can only be registered in an instance the
/// account holds (`src/state/identity_center.rs`), so a suite that creates a web app
/// names one of these — seeded into the Identity Center store the way that service's
/// own `CreateInstance` writes them, which is where this service's gate reads them.
/// An ARN outside this list names no instance of the account, which is the refusal
/// `identity_center_dependency.rs` pins.
pub(crate) const IDENTITY_CENTER_INSTANCES: [&str; 3] = [
    "arn:aws:sso:::instance/ssoins-1234567890abcdef",
    "arn:aws:sso:::instance/ins-abcdef0123456789",
    "arn:aws:sso:::instance/ssoins-a.b-c.d-e.f-g1h2",
];

/// One emulator: the shared engine plus the transfer service mounted on a router.
pub(crate) struct Emulator {
    router: MultiServiceRouter,
    /// The SAME engine the service was built from, so a test can reach this
    /// service's own store and can seed the account's IAM store — the one this
    /// service's authorization gate and its API_GATEWAY identity-provider
    /// verification read.
    engine: Arc<engine::InMemoryEngine>,
}

/// Another emulated service a suite needs mounted beside the transfer one, because the
/// transfer handlers CALL it over the in-process callback.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Peer {
    SecretsManager,
    S3,
    /// CloudWatch Logs, which `CreateServer` / `UpdateServer` call to provision the
    /// service-managed log group a server configured with a `LoggingRole` publishes to.
    Logs,
    /// AWS Lambda, which holds the functions this service INVOKES over the in-process
    /// callback: the identity provider an `AWS_LAMBDA` server's
    /// `IdentityProviderDetails.Function` names, and the `Target` of a workflow's
    /// `CUSTOM` step.
    Lambda,
}

/// One HTTP answer, decoded.
pub(crate) struct Answer {
    pub(crate) status: u16,
    pub(crate) body: Value,
}

impl Answer {
    /// The awsJson error code from the body's `__type`, without any shape-id prefix.
    pub(crate) fn error_code(&self) -> String {
        self.body
            .get("__type")
            .and_then(Value::as_str)
            .map(|t| t.rsplit('#').next().unwrap_or(t).to_string())
            .unwrap_or_default()
    }

    /// A member of a successful response body.
    pub(crate) fn get(&self, member: &str) -> &Value {
        member_of(&self.body, member)
    }
}

/// The JSON null a missing member reads as, so member access chains without `Option`.
static NULL: Value = Value::Null;

/// A member of a JSON object, or null when it is absent.
pub(crate) fn member_of<'a>(value: &'a Value, member: &str) -> &'a Value {
    value.get(member).unwrap_or(&NULL)
}

/// Put the account's IAM Identity Center instances into the Identity Center store in
/// `engine`, exactly where that service's own `CreateInstance` writes them.
///
/// Seeded at mount, before any request, because a Transfer web app's Identity Center
/// application is registered in an instance the ACCOUNT holds and a create naming any
/// other instance is refused — so a suite about web apps needs the instances it names
/// to exist, the same way it needs a server to exist before it creates a user.
/// `ambient_instance_enabled` is set with them: the account's enablement is then
/// decided, so the instances it holds are exactly these.
fn seed_identity_center_instances(engine: &Arc<engine::InMemoryEngine>) {
    use service_com_amazonaws_ssoadmin_swbexternalservice::state as identity_center;

    let state = engine.state::<identity_center::ComAmazonawsSsoadminSwbexternalserviceState>();
    let mut guard = state.lock();
    for instance_arn in IDENTITY_CENTER_INSTANCES {
        let instance_id = instance_arn
            .rsplit('/')
            .next()
            .expect("the seeded instance ARN names an instance");
        let instance = identity_center::Instance::active(
            instance_id,
            identity_center::SEED_IDENTITY_STORE_ID,
            identity_center::SEED_INSTANCE_TYPE,
            Some(identity_center::SEED_INSTANCE_NAME.to_string()),
            ::aws_smithy_types::DateTime::from_secs(1_700_000_000),
            REGION,
        );
        guard
            .instances
            .insert(instance.instance_arn.clone(), instance);
    }
    guard.ambient_instance_enabled = true;
}

impl Emulator {
    pub(crate) fn new() -> Self {
        Self::mounting(&[])
    }
    /// One emulator that also mounts the two services the CONNECTOR DATA PLANE reaches
    /// over the in-process callback: Secrets Manager, which holds the credentials an
    /// SFTP connector's session authenticates with, and Amazon S3, which holds the
    /// files an accepted transfer moves. A suite that drives a connector against an
    /// emulated server needs both to be the real emulated services, since that is what
    /// the handlers call.
    pub(crate) fn with_connector_peers() -> Self {
        Self::mounting(&[Peer::SecretsManager, Peer::S3])
    }

    /// One emulator that also mounts CloudWatch Logs, the service a server write calls
    /// over the in-process callback to provision the server's service-managed log group.
    pub(crate) fn with_logs() -> Self {
        Self::mounting(&[Peer::Logs])
    }

    /// One emulator that also mounts AWS Lambda: a suite that drives a provider or a
    /// workflow step whose function this emulator holds needs the real Lambda emulation
    /// beside this service, because that is what the handlers invoke.
    pub(crate) fn with_lambda() -> Self {
        Self::mounting(&[Peer::Lambda])
    }

    /// One emulator mounting the connector data plane's peers AND Lambda: the connector
    /// delivery reads its secret and moves its bytes through the first two, and the
    /// workflow a delivered file starts invokes its `CUSTOM` step's function through the
    /// third.
    pub(crate) fn with_connector_peers_and_lambda() -> Self {
        Self::mounting(&[Peer::SecretsManager, Peer::S3, Peer::Lambda])
    }

    /// The transfer service, plus each `peers` entry, over ONE shared engine.
    fn mounting(peers: &[Peer]) -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        seed_identity_center_instances(&engine);
        let mut router = MultiServiceRouter::new();
        router.set_request_authentication(false);
        router.register_service(
            service_com_amazonaws_transfer_transferservice::EmulatedService::new(Arc::clone(
                &engine,
            )),
        );
        for peer in peers {
            match peer {
                Peer::SecretsManager => {
                    router.register_service(
                        service_com_amazonaws_secretsmanager_secretsmanager::EmulatedService::new(
                            Arc::clone(&engine),
                        ),
                    );
                }
                Peer::S3 => {
                    router.register_service(
                        service_com_amazonaws_s3_amazons3::EmulatedService::new(Arc::clone(
                            &engine,
                        )),
                    );
                }
                Peer::Logs => {
                    router.register_service(
                        service_com_amazonaws_cloudwatchlogs_logs_20140328::EmulatedService::new(
                            Arc::clone(&engine),
                        ),
                    );
                }
                Peer::Lambda => {
                    router.register_service(
                        service_com_amazonaws_lambda_awsgirapiservice::EmulatedService::new(
                            Arc::clone(&engine),
                        ),
                    );
                }
            }
        }
        Self { router, engine }
    }

    /// A callback into this same emulator, for a suite that drives one of the mounted
    /// peers through its own SDK client — the very seam the handlers call them over.
    pub(crate) fn callback(&self) -> smithy_server_runtime::EmbeddedEmulator {
        smithy_server_runtime::EmbeddedEmulator::from_router(self.router.clone())
    }

    /// This emulator's store — the SAME one the handlers reach through
    /// `crate::state::service_state(&*self.engine)`, since `state::<T>()` is keyed by
    /// type on the one shared engine (see `state_roundtrip.rs`).
    ///
    /// For a suite that has to SEED a resource whose write operation is still a
    /// scaffold stub; a suite whose write operation exists goes over the wire instead.
    pub(crate) fn store(&self) -> engine::ServiceState<TransferState> {
        self.engine.state()
    }

    /// The account's IAM store in this emulator's engine.
    fn iam(
        &self,
    ) -> engine::ServiceState<iam::ComAmazonawsIamAwsidentitymanagementv20100508State> {
        self.engine
            .state::<iam::ComAmazonawsIamAwsidentitymanagementv20100508State>()
    }

    /// Put an IAM role into the account carrying one inline policy, exactly where
    /// IAM's own `CreateRole` + `PutRolePolicy` would have written it.
    ///
    /// This is what the `API_GATEWAY` identity-provider verification reads: a role
    /// the account does not hold, or holds without the invoke permission, cannot
    /// invoke the identity provider's API.
    pub(crate) fn seed_role(&self, role_arn: &str, policy_document: &str) {
        let role_name = role_arn
            .rsplit('/')
            .next()
            .expect("the seeded role ARN names a role");
        let key = iam::name_key(role_name);
        let state = self.iam();
        let mut guard = state.lock();
        guard.roles.insert(
            key.clone(),
            Role {
                path: "/".to_string(),
                role_name: role_name.to_string(),
                role_id: format!("AROA{}", role_name.to_ascii_uppercase()),
                arn: role_arn.to_string(),
                create_date: iam::now(),
                assume_role_policy_document: None,
                description: None,
                max_session_duration: None,
                permissions_boundary: None,
                tags: None,
                role_last_used: None,
                source_role_template: None,
            },
        );
        guard.role_inline_policies.entry(key).or_default().insert(
            iam::name_key("seeded"),
            iam::InlinePolicy {
                policy_name: "seeded".to_string(),
                policy_document: policy_document.to_string(),
            },
        );
    }

    /// Put an IAM role into the account carrying NO inline policy and ONE attached
    /// managed policy, exactly where IAM's own `CreateRole` + `AttachRolePolicy`
    /// (+ `CreatePolicy`, when the account holds the policy) would have written it.
    ///
    /// `document` is the attached policy's default-version document when the account
    /// HOLDS that policy, and `None` when it does not — an AWS-managed policy, which
    /// the account does not own, so its document can neither be read nor be ruled
    /// out. Both are what the `API_GATEWAY` verification reads through
    /// `role_attached_policies`.
    pub(crate) fn seed_role_with_attached_policy(
        &self,
        role_arn: &str,
        policy_arn: &str,
        document: Option<&str>,
    ) {
        // The role itself, with an inline policy that decides nothing, so what the
        // verification reads is the ATTACHED policy alone.
        self.seed_role(role_arn, r#"{"Version":"2012-10-17","Statement":[]}"#);
        let role_name = role_arn
            .rsplit('/')
            .next()
            .expect("the seeded role ARN names a role");
        let state = self.iam();
        let mut guard = state.lock();
        guard
            .role_attached_policies
            .entry(iam::name_key(role_name))
            .or_default()
            .insert(policy_arn.to_string());
        let Some(document) = document else {
            return;
        };
        let policy_name = policy_arn
            .rsplit('/')
            .next()
            .expect("the seeded policy ARN names a policy");
        let created = iam::now();
        guard.policies.insert(
            iam::name_key(policy_name),
            iam::ManagedPolicy {
                policy: Policy {
                    arn: Some(policy_arn.to_string()),
                    policy_id: Some(format!("ANPA{}", policy_name.to_ascii_uppercase())),
                    policy_name: Some(policy_name.to_string()),
                    path: Some("/".to_string()),
                    default_version_id: Some("v1".to_string()),
                    attachment_count: Some(1),
                    permissions_boundary_usage_count: Some(0),
                    is_attachable: true,
                    description: None,
                    create_date: Some(created),
                    update_date: Some(created),
                    tags: None,
                },
                versions: [(
                    "v1".to_string(),
                    PolicyVersion {
                        document: Some(document.to_string()),
                        version_id: Some("v1".to_string()),
                        is_default_version: true,
                        create_date: Some(created),
                    },
                )]
                .into_iter()
                .collect(),
            },
        );
    }

    /// Put an IAM user holding [`ACCESS_KEY_ID`] into the account, with one inline
    /// identity policy — where IAM's own `CreateUser` + `CreateAccessKey` +
    /// `PutUserPolicy` would have written it. This is the calling principal the
    /// authorization gate resolves the request's access key to.
    pub(crate) fn seed_calling_principal(&self, user_name: &str, policy_document: &str) {
        let key = iam::name_key(user_name);
        let state = self.iam();
        let mut guard = state.lock();
        guard.users.insert(
            key.clone(),
            User {
                path: "/".to_string(),
                user_name: user_name.to_string(),
                user_id: format!("AIDA{}", user_name.to_ascii_uppercase()),
                arn: iam::user_arn("/", user_name),
                create_date: iam::now(),
                password_last_used: None,
                permissions_boundary: None,
                tags: None,
            },
        );
        guard.access_keys.insert(
            ACCESS_KEY_ID.to_string(),
            iam::AccessKeyRecord {
                user_key: key.clone(),
                status: "Active".to_string(),
                create_date: iam::now(),
            },
        );
        guard.user_inline_policies.entry(key).or_default().insert(
            iam::name_key("seeded"),
            iam::InlinePolicy {
                policy_name: "seeded".to_string(),
                policy_document: policy_document.to_string(),
            },
        );
    }

    pub(crate) async fn call(&self, op: &str, body: Value) -> Answer {
        self.call_in(REGION, op, body).await
    }

    /// Dispatch `op` signed for `region`, so a handler that reads the credential
    /// scope sees it.
    pub(crate) async fn call_in(&self, region: &str, op: &str, body: Value) -> Answer {
        self.call_service(region, "transfer", &format!("TransferService.{op}"), body)
            .await
    }

    /// Dispatch an awsJson1_1 call to ANY service mounted on this router: `signing_name`
    /// is the credential scope's service and `target` the `X-Amz-Target` that service
    /// claims. A suite reads the state a transfer handler wrote into a PEER through the
    /// peer's own API this way, rather than reaching into that service's store.
    pub(crate) async fn call_service(
        &self,
        region: &str,
        signing_name: &str,
        target: &str,
        body: Value,
    ) -> Answer {
        let request = http::Request::builder()
            .method("POST")
            .uri("/")
            .header("content-type", "application/x-amz-json-1.1")
            .header("x-amz-target", target)
            .header(
                "authorization",
                format!(
                    "AWS4-HMAC-SHA256 Credential={ACCESS_KEY_ID}/20240101/{region}/{signing_name}/\
                     aws4_request, SignedHeaders=host, Signature=0123456789abcdef"
                ),
            )
            .body(bytes::Bytes::from(
                serde_json::to_vec(&body).expect("the test request body serializes"),
            ))
            .expect("a well-formed test request");

        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let bytes = response.into_body();
        let body = if bytes.is_empty() {
            Value::Object(serde_json::Map::new())
        } else {
            serde_json::from_slice(&bytes).unwrap_or_else(|e| {
                panic!(
                    "target {target}: response body is not JSON ({e}): {}",
                    String::from_utf8_lossy(&bytes)
                )
            })
        };
        Answer { status, body }
    }

    /// Create a server from `request` and return its `ServerId`.
    pub(crate) async fn create_server(&self, request: Value) -> String {
        let answer = self.call("CreateServer", request).await;
        assert_eq!(
            answer.status, 200,
            "CreateServer: expected 200, got {} with body {}",
            answer.status, answer.body
        );
        answer
            .get("ServerId")
            .as_str()
            .unwrap_or_else(|| panic!("CreateServer returned no ServerId: {}", answer.body))
            .to_string()
    }

    /// Create a user and assert the create succeeded.
    pub(crate) async fn create_user(&self, server_id: &str, user_name: &str) {
        let answer = self
            .call(
                "CreateUser",
                json!({"ServerId": server_id, "UserName": user_name, "Role": ROLE}),
            )
            .await;
        assert_eq!(
            answer.status, 200,
            "CreateUser {user_name}: expected 200, got {} with body {}",
            answer.status, answer.body
        );
    }

    /// Create a server whose identity provider is the DIRECTORY one an access grant
    /// needs — the only `IdentityProviderType` an `ExternalId` can name a group in — and
    /// return its `ServerId`.
    pub(crate) async fn directory_server(&self) -> String {
        self.create_server(json!({
            "IdentityProviderType": "AWS_DIRECTORY_SERVICE",
            "IdentityProviderDetails": {"DirectoryId": DIRECTORY_ID},
        }))
        .await
    }

    /// `DescribeAccess` for one grant, asserted to have been served and to answer the
    /// `ServerId` it was addressed at, with the `DescribedAccess` it reported.
    pub(crate) async fn described_access(&self, server_id: &str, external_id: &str) -> Value {
        let answer = self
            .call(
                "DescribeAccess",
                json!({"ServerId": server_id, "ExternalId": external_id}),
            )
            .await;
        assert_eq!(
            answer.status, 200,
            "DescribeAccess {external_id}: expected 200, got {} with body {}",
            answer.status, answer.body
        );
        assert_eq!(
            answer.get("ServerId").as_str(),
            Some(server_id),
            "DescribeAccess {external_id}: expected ServerId {server_id:?}, got {}",
            answer.body
        );
        answer.get("Access").clone()
    }

    /// Create an AS2 profile from `request` and return its `ProfileId`.
    pub(crate) async fn create_profile(&self, request: Value) -> String {
        let answer = self.call("CreateProfile", request).await;
        assert_eq!(
            answer.status, 200,
            "CreateProfile: expected 200, got {} with body {}",
            answer.status, answer.body
        );
        answer
            .get("ProfileId")
            .as_str()
            .unwrap_or_else(|| panic!("CreateProfile returned no ProfileId: {}", answer.body))
            .to_string()
    }

    /// Create an AS2 agreement from `request` and return its `AgreementId`, asserting
    /// the minted identifier has the modelled form.
    pub(crate) async fn create_agreement(&self, case: &str, request: Value) -> String {
        let answer = self.call("CreateAgreement", request).await;
        assert_eq!(
            answer.status, 200,
            "case {case:?}: expected CreateAgreement 200, got {} with body {}",
            answer.status, answer.body
        );
        let agreement_id = answer
            .get("AgreementId")
            .as_str()
            .unwrap_or_else(|| {
                panic!("case {case:?}: expected an AgreementId, got {}", answer.body)
            })
            .to_string();
        assert!(
            is_modelled_agreement_id(&agreement_id),
            "case {case:?}: expected an AgreementId matching ^a-([0-9a-f]{{17}})$, got \
             {agreement_id:?}"
        );
        agreement_id
    }

    /// Read one agreement back through `DescribeAgreement`, answering the whole response
    /// so a caller can assert either the record or the refusal.
    pub(crate) async fn describe_agreement(&self, server_id: &str, agreement_id: &str) -> Answer {
        self.call(
            "DescribeAgreement",
            json!({"ServerId": server_id, "AgreementId": agreement_id}),
        )
        .await
    }
}

/// A `CustomDirectoriesType` under `prefix`: every one of the five `@required` members
/// the shape declares (`smithy_query(query="shape",shape="CustomDirectoriesType")`).
///
/// The one place this crate builds that structure, so every suite that configures an
/// agreement with custom directories configures it the same way.
pub(crate) fn custom_directories(prefix: &str) -> Value {
    json!({
        "FailedFilesDirectory": format!("{prefix}/failed"),
        "MdnFilesDirectory": format!("{prefix}/mdn"),
        "PayloadFilesDirectory": format!("{prefix}/payload"),
        "StatusFilesDirectory": format!("{prefix}/status"),
        "TemporaryFilesDirectory": format!("{prefix}/temporary"),
    })
}

/// The five `CustomDirectoriesType` members in MODEL order, each with the
/// lowerCamelCase label a refusal that reports the structure names it under
/// (`smithy_query(query="shape",shape="CustomDirectoriesType")` reports
/// `FailedFilesDirectory`, `MdnFilesDirectory`, `PayloadFilesDirectory`,
/// `StatusFilesDirectory`, `TemporaryFilesDirectory`, all `@required`).
pub(crate) const CUSTOM_DIRECTORY_MEMBERS: [(&str, &str); 5] = [
    ("FailedFilesDirectory", "failedFilesDirectory"),
    ("MdnFilesDirectory", "mdnFilesDirectory"),
    ("PayloadFilesDirectory", "payloadFilesDirectory"),
    ("StatusFilesDirectory", "statusFilesDirectory"),
    ("TemporaryFilesDirectory", "temporaryFilesDirectory"),
];

/// The `Message` the service reports for a request that configures BOTH of an
/// agreement's alternative directory configurations, which `CreateAgreement` and
/// `UpdateAgreement` both forbid ("Specify either BaseDirectory or CustomDirectories,
/// but not both. Specifying both causes the command to fail.").
///
/// The FIXED parts are written out here — the sentence, the shape name, the five
/// lowerCamelCase labels in model order and the separators; the VARIABLE parts are the
/// `base_directory` and the `custom_directories` structure the request carried, so a
/// suite builds the expectation from its own request rather than from a recorded run.
/// `agreement_directory_conflict_message.rs` asserts the rule over a table of
/// conflicts; this is the one place the template lives.
pub(crate) fn both_directories_configured_message(
    base_directory: &str,
    custom_directories: &Value,
) -> String {
    let rendered = CUSTOM_DIRECTORY_MEMBERS
        .iter()
        .map(|(member, label)| {
            let value = member_of(custom_directories, member)
                .as_str()
                .unwrap_or_default();
            format!("{label}={value}")
        })
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "Both BaseDirectory and CustomDirectories are configured. \
         BaseDirectory: {base_directory}, CustomDirectories: CustomDirectoriesType({rendered})"
    )
}

/// A `Tags` list on the wire as the key -> value map it denotes.
///
/// An absent member reads as the empty map, as the empty list does — a reader reports
/// the EMPTY list for a resource with no tags.
pub(crate) fn tag_map(tags: &Value) -> BTreeMap<String, String> {
    tags.as_array()
        .map(|list| {
            list.iter()
                .map(|tag| {
                    (
                        member_of(tag, "Key").as_str().unwrap_or_default().to_string(),
                        member_of(tag, "Value")
                            .as_str()
                            .unwrap_or_default()
                            .to_string(),
                    )
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The tag set `ListTagsForResource` reports for `arn` in ONE unbounded call, which is
/// the whole set for every caller here (a resource carries at most
/// `MAX_TAGS_PER_RESOURCE` = 50 tags, far below the page bound), so the answer must
/// carry no `NextToken`. Paging itself is asserted over [`drain_pages`].
pub(crate) async fn listed_tags(
    emulator: &Emulator,
    case: &str,
    arn: &str,
) -> BTreeMap<String, String> {
    let answer = emulator
        .call("ListTagsForResource", json!({"Arn": arn}))
        .await;
    assert_eq!(
        answer.status, 200,
        "case {case:?}: expected ListTagsForResource 200, got {} with body {}",
        answer.status, answer.body
    );
    assert_eq!(
        answer.get("Arn").as_str(),
        Some(arn),
        "case {case:?}: expected the requested Arn echoed, got {}",
        answer.body
    );
    assert!(
        answer.get("NextToken").is_null(),
        "case {case:?}: expected one unbounded page with no NextToken, got {}",
        answer.body
    );
    tag_map(answer.get("Tags"))
}

/// Assert `answer` is this service's not-found refusal for the agreement
/// `agreement_id`, whichever operation of the agreement plane answered it.
///
/// The one place this crate states that refusal, so the suites that read an agreement
/// after a removal, and the ones that address a pair that was never created, cannot
/// disagree about what it looks like. The status is the one the service answers a
/// resource reference it cannot resolve with
/// (`crate::state::RESOURCE_NOT_FOUND_HTTP_STATUS`, whose own documentation records
/// the recorded wire status the shape's `@httpError` does not describe), so this
/// expectation follows the service's single decision rather than restating it.
pub(crate) fn assert_agreement_not_found(
    case: &str,
    op: &str,
    answer: &Answer,
    agreement_id: &str,
) {
    assert_eq!(
        (answer.status, answer.error_code().as_str()),
        (400, "ResourceNotFoundException"),
        "case {case:?}: expected {op} to answer 400 ResourceNotFoundException, got {} with \
         body {}",
        answer.status,
        answer.body
    );
    assert_eq!(
        member_of(&answer.body, "ResourceType").as_str(),
        Some("Agreement"),
        "case {case:?}: expected {op} to report the agreement kind, got {}",
        answer.body
    );
    assert_eq!(
        member_of(&answer.body, "Resource").as_str(),
        Some(agreement_id),
        "case {case:?}: expected {op} to report the requested identifier, got {}",
        answer.body
    );
    let message = member_of(&answer.body, "Message")
        .as_str()
        .unwrap_or_default()
        .to_string();
    assert!(
        !message.is_empty() && !message.contains(agreement_id),
        "case {case:?}: expected {op} to answer a non-empty Message carrying no \
         identifier, got {message:?}"
    );
}

/// `ServerId` is modelled `len 19..19`, pattern `^s-([0-9a-f]{17})$`.
pub(crate) fn is_modelled_server_id(id: &str) -> bool {
    is_prefixed_lower_hex_id(id, "s-", 17)
}

/// `SshPublicKeyId` is modelled `len 21..21`, pattern `^key-[0-9a-f]{17}$`.
pub(crate) fn is_modelled_key_id(id: &str) -> bool {
    is_prefixed_lower_hex_id(id, "key-", 17)
}

/// `WebAppId` is modelled `len 24..24`, pattern `^webapp-[0-9a-f]{17}$`.
pub(crate) fn is_modelled_web_app_id(id: &str) -> bool {
    is_prefixed_lower_hex_id(id, "webapp-", 17)
}

/// `DescribedWebAppVpcConfig.VpcEndpointId` is modelled `len 22..22`, pattern
/// `^vpce-[0-9a-f]{17}$`.
pub(crate) fn is_modelled_vpc_endpoint_id(id: &str) -> bool {
    is_prefixed_lower_hex_id(id, "vpce-", 17)
}

/// `ConnectorId` is modelled `len 19..19`, pattern `^c-([0-9a-f]{17})$`.
pub(crate) fn is_modelled_connector_id(id: &str) -> bool {
    id.len() == 19
        && id.starts_with("c-")
        && id[2..].chars().all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c))
}

/// `ProfileId` is modelled `len 19..19`, pattern `^p-([0-9a-f]{17})$`.
pub(crate) fn is_modelled_profile_id(id: &str) -> bool {
    is_prefixed_lower_hex_id(id, "p-", 17)
}

/// `AgreementId` is modelled `len 19..19`, pattern `^a-([0-9a-f]{17})$`.
pub(crate) fn is_modelled_agreement_id(id: &str) -> bool {
    is_prefixed_lower_hex_id(id, "a-", 17)
}

/// `CertificateId` is modelled `len 22..22`, pattern `^cert-([0-9a-f]{17})$`.
pub(crate) fn is_modelled_certificate_id(id: &str) -> bool {
    is_prefixed_lower_hex_id(id, "cert-", 17)
}

/// `HostKeyId` is modelled `len 25..25`, pattern `^hostkey-[0-9a-f]{17}$`.
pub(crate) fn is_modelled_host_key_id(id: &str) -> bool {
    is_prefixed_lower_hex_id(id, "hostkey-", 17)
}

/// Whether `id` is `prefix` followed by exactly `digits` lowercase hex digits — the
/// ONE copy of the `<prefix>-[0-9a-f]{n}` rule every minted identifier in this model
/// follows, so each predicate above is just its prefix and digit count.
fn is_prefixed_lower_hex_id(id: &str, prefix: &str, digits: usize) -> bool {
    id.strip_prefix(prefix).is_some_and(|rest| {
        rest.len() == digits
            && rest
                .chars()
                .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c))
    })
}

/// An OpenSSH Ed25519 private key and the fingerprint this service's readers report for
/// it, both produced here from the key's own bytes — so the expectation comes from the
/// key, not from the handler under test.
pub(crate) fn generated_host_key() -> (String, String) {
    let keypair = ssh_key::private::Ed25519Keypair::random(&mut ssh_key::rand_core::OsRng);
    let key = ssh_key::PrivateKey::from(keypair);
    let material = key
        .to_openssh(ssh_key::LineEnding::LF)
        .expect("the generated key encodes as an OpenSSH private key")
        .to_string();
    (material, reported_fingerprint(key.public_key()))
}

/// The fingerprint a reader of `key` reports: the literal `SHA256:` followed by the
/// Base64 encoding of the SHA-256 digest of the key's OpenSSH public-key blob, which is
/// what "the Base64-encoded SHA256 fingerprint" of a key is.
///
/// Computed HERE, from the key's own bytes, with a hash crate and a Base64 crate rather
/// than through anything the code under test calls: the digest is taken over the blob
/// the key encodes to, and the encoding is the standard one, whose final group of the
/// 32-byte digest is completed by a `=` pad.
pub(crate) fn reported_fingerprint(key: &ssh_key::PublicKey) -> String {
    use base64::Engine as _;
    use sha2::Digest as _;

    let blob = key.to_bytes().expect("the key encodes as an OpenSSH blob");
    let digest = sha2::Sha256::digest(blob);
    format!(
        "SHA256:{}",
        base64::engine::general_purpose::STANDARD.encode(digest)
    )
}

/// ONE real RSA 2048 private key in the two PEM containers the service's own documented
/// host-key generation commands write — the PKCS#1 one `ssh-keygen -t rsa -m PEM` writes
/// and the PKCS#8 one `-m PKCS8` writes — generated here, so a suite that needs RSA host
/// key material pastes no key in. Both strings carry the SAME key, which is what lets a
/// suite send one key in two containers.
pub(crate) fn generated_rsa_host_key_pems() -> (String, String) {
    use aws_lc_rs::encoding::AsDer as _;

    let keypair = aws_lc_rs::rsa::KeyPair::generate(aws_lc_rs::rsa::KeySize::Rsa2048)
        .expect("an RSA 2048 key pair");
    let pkcs8 = keypair
        .as_der()
        .expect("the generated key encodes as PKCS#8 DER");
    let info = pkcs8::PrivateKeyInfo::try_from(pkcs8.as_ref()).expect("the PKCS#8 key");
    // A PKCS#8 RSA key's `privateKey` octets ARE the PKCS#1 structure, so the same key in
    // the PKCS#1 container is that inner DER under the PKCS#1 label.
    (
        pem_armour("RSA PRIVATE KEY", info.private_key),
        pem_armour("PRIVATE KEY", pkcs8.as_ref()),
    )
}

/// PEM armour around `der`, written by the crate that implements the container.
fn pem_armour(label: &str, der: &[u8]) -> String {
    pkcs1::pem::encode_string(label, pkcs1::pem::LineEnding::LF, der).expect("the PEM")
}

/// An OpenSSH ED25519 PUBLIC key, in the `key-type key-body comment` form
/// `ssh-keygen` writes, generated here by the OpenSSH key-format crate — so a suite
/// that needs a real trusted host key does not paste one in.
pub(crate) fn generated_public_key() -> String {
    let keypair = ssh_key::private::Ed25519Keypair::random(&mut ssh_key::rand_core::OsRng);
    ssh_key::PrivateKey::from(keypair)
        .public_key()
        .to_openssh()
        .expect("the generated key encodes as an OpenSSH public key")
}

/// The SSH algorithm name of private key MATERIAL, read back out of the material by
/// the OpenSSH key-format crate — one of the values `DescribedHostKey.Type` documents
/// (`ssh-rsa`, `ssh-ed25519`, `ecdsa-sha2-nistp256/384/521`).
pub(crate) fn host_key_algorithm(material: &str) -> String {
    ssh_key::PrivateKey::from_openssh(material)
        .expect("the test's own key material parses as an OpenSSH private key")
        .algorithm()
        .as_str()
        .to_string()
}

/// Seconds since the Unix epoch, at the clock's own precision — the unit the awsJson
/// protocol puts a timestamp on the wire in. Shared by every suite that asserts on a
/// timestamp member.
pub(crate) fn unix_seconds(at: std::time::SystemTime) -> f64 {
    at.duration_since(std::time::SystemTime::UNIX_EPOCH)
        .expect("the test clock is past the Unix epoch")
        .as_secs_f64()
}

// ---------------------------------------------------------------------------
// The Lambda functions this service invokes, as a suite deploys them
// ---------------------------------------------------------------------------

/// The runtime and the handler attribute every function a suite of this crate deploys
/// is created with: `Runtime` names the managed runtime that runs the package, and
/// `Handler` is "the name of the method within your code that Lambda calls", which for a
/// python package is `<module>.<function>`.
pub(crate) const PYTHON_RUNTIME: &str = "python3.12";
pub(crate) const PYTHON_HANDLER: &str = "lambda_function.lambda_handler";

/// How long a suite waits for AWS Lambda to report a created function `Active`, and how
/// often it looks.
///
/// The span is ANOTHER service's, so this is a client's patience rather than a span read
/// out of either crate: generous enough that a loaded host cannot decide the outcome,
/// and bounded so a function that never provisions fails the suite instead of hanging
/// it. Nothing is asserted about how long the wait took.
const FUNCTION_ACTIVE_DEADLINE: Duration = Duration::from_secs(120);
/// See [`FUNCTION_ACTIVE_DEADLINE`].
const FUNCTION_LOOK: Duration = Duration::from_millis(50);

/// The `State` an invocation needs the function to be in ("`Active`: the function is
/// ready to be invoked").
const FUNCTION_STATE_ACTIVE: &str = "Active";

/// A .zip deployment package holding `source` as the module [`PYTHON_HANDLER`] names,
/// written in memory the way a caller uploads one through `FunctionCode$ZipFile`.
pub(crate) fn python_package(source: &str) -> Vec<u8> {
    use std::io::Write as _;

    let mut buffer = std::io::Cursor::new(Vec::new());
    {
        let mut writer = zip::ZipWriter::new(&mut buffer);
        let options: zip::write::FileOptions<'_, '_, ()> = zip::write::FileOptions::default();
        let module = PYTHON_HANDLER
            .split_once('.')
            .map(|(module, _)| module)
            .expect("the handler names a module and a function");
        writer
            .start_file(format!("{module}.py"), options)
            .expect("the package entry starts");
        writer
            .write_all(source.as_bytes())
            .expect("the package entry takes the handler's source");
        writer.finish().expect("a finished archive");
    }
    buffer.into_inner()
}

impl Emulator {
    /// A Lambda client over this emulator's own in-process callback — the same seam the
    /// handlers invoke a function over, so a function a suite creates through it is the
    /// one they reach.
    pub(crate) fn lambda(&self) -> aws_sdk_lambda::Client {
        let callback = self.callback();
        let config = aws_sdk_lambda::Config::builder()
            .behavior_version(aws_sdk_lambda::config::BehaviorVersion::latest())
            .region(aws_sdk_lambda::config::Region::new(REGION))
            .endpoint_url(format!("https://lambda.{REGION}.amazonaws.com"))
            .credentials_provider(callback.aws_credentials_provider())
            .http_client(callback.http_client())
            .retry_config(aws_sdk_lambda::config::retry::RetryConfig::disabled())
            .timeout_config(aws_sdk_lambda::config::timeout::TimeoutConfig::disabled())
            .identity_cache(aws_sdk_lambda::config::IdentityCache::no_cache())
            .stalled_stream_protection(
                aws_sdk_lambda::config::StalledStreamProtectionConfig::disabled(),
            )
            .build();
        aws_sdk_lambda::Client::from_conf(config)
    }

    /// Deploy `source` as a python function named `function_name`, running as the role
    /// `role_arn` — seeded into the account's IAM store with `role_policy`, the grants
    /// its own SDK calls are judged against — wait until Lambda reports it `Active`, and
    /// answer the `FunctionArn` that service published for it.
    ///
    /// The function is a real one of the Lambda emulation mounted beside this service
    /// ([`Peer::Lambda`]), so what a transfer handler invokes is a function that runs.
    pub(crate) async fn deploy_python_function(
        &self,
        function_name: &str,
        role_arn: &str,
        role_policy: &str,
        source: &str,
    ) -> String {
        self.seed_role(role_arn, role_policy);
        let lambda = self.lambda();
        let created = lambda
            .create_function()
            .function_name(function_name)
            .runtime(aws_sdk_lambda::types::Runtime::from(PYTHON_RUNTIME))
            .role(role_arn)
            .handler(PYTHON_HANDLER)
            .code(
                aws_sdk_lambda::types::FunctionCode::builder()
                    .zip_file(aws_sdk_lambda::primitives::Blob::new(python_package(source)))
                    .build(),
            )
            .send()
            .await
            .unwrap_or_else(|error| {
                panic!(
                    "setup: CreateFunction({function_name:?}) must succeed, got {}",
                    aws_smithy_types::error::display::DisplayErrorContext(&error)
                )
            });
        let function_arn = created
            .function_arn()
            .unwrap_or_else(|| panic!("setup: Lambda must publish an ARN for {function_name:?}"))
            .to_string();

        // An invocation needs the function `Active`, which the clock owes: waited for by
        // LOOKING at what Lambda reports rather than by sleeping a span of this suite's
        // own choosing.
        let started = std::time::Instant::now();
        let mut last = String::new();
        while started.elapsed() < FUNCTION_ACTIVE_DEADLINE {
            let read = lambda
                .get_function()
                .function_name(function_name)
                .send()
                .await
                .unwrap_or_else(|error| {
                    panic!(
                        "setup: GetFunction({function_name:?}) must succeed, got {}",
                        aws_smithy_types::error::display::DisplayErrorContext(&error)
                    )
                });
            last = read
                .configuration()
                .and_then(|configuration| configuration.state())
                .map(|state| state.as_str().to_string())
                .unwrap_or_default();
            if last == FUNCTION_STATE_ACTIVE {
                return function_arn;
            }
            tokio::time::sleep(FUNCTION_LOOK).await;
        }
        panic!(
            "setup: Lambda must report {function_name:?} {FUNCTION_STATE_ACTIVE}, last reported \
             {last:?}"
        )
    }
}

/// Re-read one stage of a flow until `probe` reports it settled, and answer what it
/// last saw.
///
/// For a suite that waits the way a CLIENT waits: on what the API serves, not on a span
/// read out of the crate under test. `probe` answers `(settled, what it read)`, so a
/// stage that never settles fails with the value the API was still serving — the
/// evidence a stall leaves — rather than hanging the suite.
pub(crate) async fn poll_until<F, Fut>(case: &str, stage: &str, probe: F) -> Value
where
    F: Fn() -> Fut,
    Fut: std::future::Future<Output = (bool, Value)>,
{
    let started = std::time::Instant::now();
    loop {
        let (settled, last) = probe().await;
        if settled {
            return last;
        }
        assert!(
            started.elapsed() < SETTLE_DEADLINE,
            "case {case:?}: {stage} did not settle within {SETTLE_DEADLINE:?}; the last \
             answer was {last:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Assert that `actual` carries each member `expected` names, where a `null`
/// expectation means "the response must not carry this member at all".
pub(crate) fn assert_members(case: &str, actual: &Value, expected: &Value) {
    let expected = expected.as_object().expect("the expectation is an object");
    for (member, want) in expected {
        let got = actual.get(member).unwrap_or(&Value::Null);
        if want.is_null() {
            assert!(
                got.is_null(),
                "case {case:?}: expected {member} to be absent, got {got:?}"
            );
        } else {
            assert_eq!(
                got, want,
                "case {case:?}: expected {member} to be {want:?}, got {got:?}"
            );
        }
    }
}

/// Drain every page of a `List*` operation, returning the per-page item counts and the
/// concatenated item key at `key_member`.
///
/// Shared by every `@paginated` suite in this crate, so the paging walk is written
/// once: `base` is the request without paging members, `max_results` the page bound to
/// send (`None` sends none), `items_member` the output's list member and `key_member`
/// the identifier to collect from each item.
pub(crate) async fn drain_pages(
    emulator: &Emulator,
    op: &str,
    base: &Value,
    items_member: &str,
    key_member: &str,
    max_results: Option<i64>,
) -> (Vec<usize>, Vec<String>) {
    let mut page_sizes = Vec::new();
    let mut keys = Vec::new();
    let mut next: Option<String> = None;

    loop {
        let mut request = base.clone();
        if let Some(limit) = max_results {
            request["MaxResults"] = json!(limit);
        }
        if let Some(token) = &next {
            request["NextToken"] = json!(token);
        }
        let answer = emulator.call(op, request).await;
        assert_eq!(
            answer.status, 200,
            "{op} (MaxResults {max_results:?}): expected 200, got {} with body {}",
            answer.status, answer.body
        );
        let items = answer
            .get(items_member)
            .as_array()
            .cloned()
            .unwrap_or_default();
        page_sizes.push(items.len());
        for item in &items {
            keys.push(
                item.get(key_member)
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
            );
        }
        match answer.get("NextToken").as_str() {
            Some(token) => next = Some(token.to_string()),
            None => break,
        }
        assert!(
            page_sizes.len() < 64,
            "{op} (MaxResults {max_results:?}): paging did not terminate"
        );
    }

    (page_sizes, keys)
}
