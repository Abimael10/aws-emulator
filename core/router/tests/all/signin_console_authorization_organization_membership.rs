// SPDX-License-Identifier: CC0-1.0

//! One organization, two services: the AWS Organizations entity an AWS SignIn
//! console-authorization call is authorized for is the entity that control plane's OWN
//! API reports, and that record exists only once a request created it.
//!
//! RULE: an account's membership of an AWS Organizations organization is STATE a request
//! establishes — the account the `CreateOrganization` call is made from becomes the
//! management account of the organization it mints — so mounting the Organizations
//! control plane beside another service records no membership at all, and a service that
//! derives authority from that membership must refuse every Organizations-scoped target
//! until that create has happened and then serve exactly the entity the create recorded
//! for this account. Authority follows the record, never the FORM of an id: an `o-`/`r-`
//! value that is well formed but names no entity of the account's own organization —
//! including an id a control plane really minted, offered to a store that holds no
//! organization — is still refused.
//!
//! Only a cross-service suite can pin this: the membership is the Organizations store's,
//! the gate is AWS SignIn's, and SignIn reads that store through the shared engine — so
//! both services are mounted on ONE engine, exactly as `core/router/src/lib.rs` mounts
//! them, and every assertion goes over the wire each service publishes (`awsJson1_1` with
//! an `X-Amz-Target` for Organizations, `restJson1` paths for SignIn).
//!
//! The rows span what the rule generalises over: whether the organization has been
//! created at all, which alternative of `TargetId`'s pattern the target matches (account
//! id, organization id, organizational root id, the root id at both published length
//! bounds), which of the three console-authorization operations is called, and whether
//! the store the call is made to is the one that holds the record of the organization an
//! `o-` target names.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The Region both control planes are addressed in.
const REGION: &str = "us-east-1";

/// The `X-Amz-Target` prefix AWS Organizations routes on (that crate's `OPERATIONS.md`).
const ORGANIZATIONS_TARGET_PREFIX: &str = "AWSOrganizationsV20161128";

/// The three `restJson1` paths of AWS SignIn's console-authorization configuration
/// (that crate's `OPERATIONS.md`).
const PUT_CONFIGURATION: &str = "/put-console-authorization-configuration";
const GET_CONFIGURATION: &str = "/get-console-authorization-configuration";
const DELETE_CONFIGURATION: &str = "/delete-console-authorization-configuration";

/// The fixed sentence AWS SignIn states for a call scoped to an AWS Organizations entity
/// the caller has no authority over.
const NOT_AUTHORIZED_MESSAGE: &str = "Caller is not authorized for this organization";

/// The fixed sentence AWS Organizations states for a request made while the calling
/// account belongs to no organization.
const NOT_IN_USE_MESSAGE: &str = "Your account isn't a member of an organization. To make this request, you must use the credentials of an account that belongs to an organization.";

/// The fixed sentence AWS SignIn states for a target that has no console-authorization
/// configuration.
const CONFIGURATION_NOT_FOUND_MESSAGE: &str = "Console authorization configuration not found";

/// AWS SignIn's `error` member for a caller that may not perform the operation.
const INSUFFICIENT_PERMISSIONS_CODE: &str = "INSUFFICIENT_PERMISSIONS";

/// AWS SignIn's `error` member for a refusal of a resource that is not there.
const NOT_FOUND_CODE: &str = "RESOURCE_NOT_FOUND";

/// The `scope` AWS SignIn reports for an account-form target, and for an organization.
const ACCOUNT_SCOPE: &str = "ACCOUNT";
const ORGANIZATION_SCOPE: &str = "ORGANIZATION";

/// What the emulator answered: the HTTP status and the parsed body.
struct Answer {
    status: u16,
    body: Value,
}

impl Answer {
    /// The wire error code, taken off the `awsJson`/`restJson` `__type` both front ends
    /// carry, or the empty string on a success.
    fn error_code(&self) -> &str {
        let code = self.body["__type"].as_str().unwrap_or_default();
        code.rsplit('#').next().unwrap_or_default()
    }

    /// One string member of the body, or the empty string when it is absent: the error
    /// envelopes of the two services spell the sentence differently (`Message` for
    /// Organizations' `awsJson1_1`, `message` for SignIn's `restJson1`).
    fn member(&self, name: &str) -> &str {
        self.body[name].as_str().unwrap_or_default()
    }
}

/// AWS SignIn and AWS Organizations on one engine.
struct Emulator {
    engine: Arc<engine::InMemoryEngine>,
    router: MultiServiceRouter,
}

impl Emulator {
    /// Both services mounted on one fresh engine, and nothing else: no call has been made
    /// yet, so this account belongs to no organization.
    fn mounted() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        // These callers sign nothing, so they must not be refused for it; the credential
        // scope below is what routes each request and names its Region.
        router.set_request_authentication(false);
        router.register_service(service_com_amazonaws_signin_signin::EmulatedService::new(
            Arc::clone(&engine),
        ));
        router.register_service(
            service_com_amazonaws_organizations_awsorganizationsv20161128::EmulatedService::new(
                Arc::clone(&engine),
            ),
        );
        Self { engine, router }
    }

    /// Dispatch `request` and read the answer off it.
    async fn dispatch(&self, called: &str, request: ::http::Request<Bytes>) -> Answer {
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let bytes = response.into_body();
        let body = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap_or_else(|error| {
                panic!(
                    "case {called:?}: the answer is not JSON ({error}): {:?}",
                    String::from_utf8_lossy(&bytes)
                )
            })
        };
        Answer { status, body }
    }

    /// One AWS SignIn `restJson1` call: `POST path` with `body`.
    async fn signin(&self, path: &str, body: &Value) -> Answer {
        let request = ::http::Request::builder()
            .method("POST")
            .uri(path)
            .header("host", format!("signin.{REGION}.amazonaws.com"))
            .header("content-type", "application/json")
            .header("authorization", credential_scope("signin"))
            .body(Bytes::from(
                serde_json::to_vec(body).expect("the request body serializes"),
            ))
            .expect("a well-formed request");
        self.dispatch(&format!("POST {path}"), request).await
    }

    /// One AWS Organizations `awsJson1_1` call, routed by its `X-Amz-Target`.
    async fn organizations(&self, operation: &str, body: &Value) -> Answer {
        let request = ::http::Request::builder()
            .method("POST")
            .uri("/")
            .header("host", format!("organizations.{REGION}.amazonaws.com"))
            .header("content-type", "application/x-amz-json-1.1")
            .header(
                "x-amz-target",
                format!("{ORGANIZATIONS_TARGET_PREFIX}.{operation}"),
            )
            .header("authorization", credential_scope("organizations"))
            .body(Bytes::from(
                serde_json::to_vec(body).expect("the request body serializes"),
            ))
            .expect("a well-formed request");
        self.dispatch(operation, request).await
    }

    /// Create the organization this account manages through the Organizations control
    /// plane's own API, and answer the id THAT control plane minted — read off its answer
    /// rather than chosen here, so no assertion can pass against an id this emulator
    /// would not have.
    async fn create_organization(&self) -> String {
        let created = self.organizations("CreateOrganization", &json!({})).await;
        assert_eq!(
            created.status, 200,
            "case \"CreateOrganization\": the create must establish the organization, got {} \
             with {}",
            created.status, created.body
        );
        created.body["Organization"]["Id"]
            .as_str()
            .unwrap_or_else(|| {
                panic!(
                    "case \"CreateOrganization\": the create must report the organization it \
                     minted, got {}",
                    created.body
                )
            })
            .to_string()
    }

    /// Every target AWS SignIn's store holds a console-authorization configuration for,
    /// read straight off that store — so a case can tell "the refused call recorded
    /// nothing" from "it recorded something no reader will serve".
    fn configured_targets(&self) -> Vec<String> {
        let state = service_com_amazonaws_signin_signin::state::service_state(&*self.engine)
            .expect("the SignIn store is reachable over the shared engine");
        let mut targets: Vec<String> = state.lock().console_auth_configs.keys().cloned().collect();
        targets.sort();
        targets
    }
}

/// A SigV4 `Authorization` header whose credential scope names `signing_name` in
/// [`REGION`] — the signal the router resolves a request's service from. The signature is
/// never checked: this suite runs with request authentication off.
fn credential_scope(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{signing_name}/aws4_request, \
         SignedHeaders=host, Signature=0"
    )
}

/// Every `TargetId` of an Organizations form that names no entity of this account's own
/// organization, as `(case, targetId)`.
///
/// Written from `TargetId`'s own pattern `^(\d{12}|o-[a-z0-9]{10}|r-[0-9a-z]{4,32})$`
/// (`smithy_query(query="shape", shape="TargetId")` on the SignIn model) and its
/// `@length(min: 12, max: 32)`: the rows span both Organizations alternatives and, for a
/// root id, each published length bound, so no row can be passing because of one id's
/// shape.
fn targets_of_another_organization() -> Vec<(&'static str, String)> {
    vec![
        ("an organization id", "o-00000000zz".to_string()),
        ("another organization id", "o-abc1234567".to_string()),
        (
            "a root id at the 12-character length floor",
            "r-0123456789".to_string(),
        ),
        (
            "a root id at the 32-character length ceiling",
            format!("r-{}", "a".repeat(30)),
        ),
    ]
}

/// Assert all three console-authorization operations refuse `target_id` as one this
/// caller has no authority over.
async fn assert_denied(emulator: &Emulator, case: &str, target_id: &str) {
    let request = json!({ "targetId": target_id });
    for (operation, path) in [
        ("put", PUT_CONFIGURATION),
        ("get", GET_CONFIGURATION),
        ("delete", DELETE_CONFIGURATION),
    ] {
        let answer = emulator.signin(path, &request).await;
        assert_eq!(
            (
                answer.status,
                answer.error_code(),
                answer.member("error"),
                answer.member("message")
            ),
            (
                400,
                "AccessDeniedException",
                INSUFFICIENT_PERMISSIONS_CODE,
                NOT_AUTHORIZED_MESSAGE
            ),
            "case {case:?}: the {operation} of {target_id:?} must be denied with \
             {NOT_AUTHORIZED_MESSAGE:?}, got {} with {}",
            answer.status,
            answer.body
        );
    }
}

/// The three members every console-authorization answer reports, as a comparable triple.
fn reported(answer: &Answer) -> (&str, &str, Option<bool>) {
    (
        answer.member("targetId"),
        answer.member("scope"),
        answer.body["consoleAuthorizationEnabled"].as_bool(),
    )
}

/// Mounting the Organizations control plane beside AWS SignIn establishes no membership:
/// its own reads report this account as belonging to no organization, every
/// Organizations-scoped console-authorization target is refused, and the account-form
/// target — which names no Organizations entity — is served throughout. The create is
/// what grants the authority, over exactly the entity it minted.
#[tokio::test]
async fn an_organizations_target_is_the_caller_s_only_once_a_create_records_the_organization() {
    let emulator = Emulator::mounted();

    // The control plane's own reads say it: no organization is recorded, and a read
    // establishes none — so the same refusal comes back on the second call.
    for operation in ["DescribeOrganization", "ListRoots"] {
        for round in 1..=2 {
            let answer = emulator.organizations(operation, &json!({})).await;
            assert_eq!(
                (answer.status, answer.error_code(), answer.member("Message")),
                (
                    404,
                    "AWSOrganizationsNotInUseException",
                    NOT_IN_USE_MESSAGE
                ),
                "case {operation:?} call {round}: a mounted control plane holds no \
                 organization, got {} with {}",
                answer.status,
                answer.body
            );
        }
    }

    // So no Organizations-form target is this caller's, on the write and both reads
    // alike, and the refused write records nothing.
    for (case, target_id) in targets_of_another_organization() {
        assert_denied(&emulator, case, &target_id).await;
    }
    assert_eq!(
        emulator.configured_targets(),
        Vec::<String>::new(),
        "a denied put must record no configuration at all, got {:?}",
        emulator.configured_targets()
    );

    // An account-form target names no Organizations entity, so it carries no such
    // requirement and is served with no organization recorded.
    let own_account = emulator.signin(PUT_CONFIGURATION, &json!({})).await;
    assert_eq!(
        own_account.status, 200,
        "case \"the caller's own account\": an account-form target must be served with no \
         organization recorded, got {} with {}",
        own_account.status, own_account.body
    );
    let caller_account = own_account.member("targetId").to_string();
    assert_eq!(
        reported(&own_account),
        (caller_account.as_str(), ACCOUNT_SCOPE, Some(true)),
        "case \"the caller's own account\": the put must report the account it enabled, got {}",
        own_account.body
    );

    // The create is what establishes the membership, and the account it is made from is
    // the organization's management account — the same account SignIn addressed above.
    let created = emulator
        .organizations("CreateOrganization", &json!({}))
        .await;
    assert_eq!(
        created.status, 200,
        "case \"CreateOrganization\": the create must be served, got {} with {}",
        created.status, created.body
    );
    let organization = created.body["Organization"].clone();
    let organization_id = organization["Id"]
        .as_str()
        .unwrap_or_else(|| panic!("the create must report the organization it minted, got {organization}"))
        .to_string();
    assert_eq!(
        organization["MasterAccountId"].as_str(),
        Some(caller_account.as_str()),
        "case \"CreateOrganization\": the account SignIn serves when a call names no target \
         must be the account Organizations records as managing the organization, got \
         {organization}"
    );
    let described = emulator
        .organizations("DescribeOrganization", &json!({}))
        .await;
    assert_eq!(
        (described.status, described.body["Organization"].clone()),
        (200, organization.clone()),
        "case \"DescribeOrganization\": the read must report the organization the create \
         persisted, got {} with {}",
        described.status,
        described.body
    );

    // That id is now the caller's: the whole lifecycle is served for it.
    let request = json!({ "targetId": organization_id.clone() });
    let written = emulator.signin(PUT_CONFIGURATION, &request).await;
    assert_eq!(
        (written.status, reported(&written)),
        (
            200,
            (organization_id.as_str(), ORGANIZATION_SCOPE, Some(true))
        ),
        "case {organization_id:?}: the organization this account created must be served, got \
         {} with {}",
        written.status,
        written.body
    );
    let read = emulator.signin(GET_CONFIGURATION, &request).await;
    assert_eq!(
        (read.status, reported(&read)),
        (
            200,
            (organization_id.as_str(), ORGANIZATION_SCOPE, Some(true))
        ),
        "case {organization_id:?}: the get must report what the put persisted, got {} with {}",
        read.status,
        read.body
    );
    let released = emulator.signin(DELETE_CONFIGURATION, &request).await;
    assert_eq!(
        (released.status, reported(&released)),
        (
            200,
            (organization_id.as_str(), ORGANIZATION_SCOPE, Some(false))
        ),
        "case {organization_id:?}: the delete must report the released target as no longer \
         enabled, got {} with {}",
        released.status,
        released.body
    );
    let missing = emulator.signin(GET_CONFIGURATION, &request).await;
    assert_eq!(
        (
            missing.status,
            missing.error_code(),
            missing.member("error"),
            missing.member("message")
        ),
        (
            404,
            "ResourceNotFoundException",
            NOT_FOUND_CODE,
            CONFIGURATION_NOT_FOUND_MESSAGE
        ),
        "case {organization_id:?}: the released target must be missing again, got {} with {}",
        missing.status,
        missing.body
    );

    // The authority is over the entity that was recorded, not over the form of an id:
    // every other Organizations-form target is still refused.
    for (case, target_id) in targets_of_another_organization() {
        assert_denied(&emulator, case, &target_id).await;
    }
}

/// Authority follows the organization THIS store records, and not the id: an `o-` value
/// a control plane really minted for an organization an account created is refused in a
/// store where no account has created one — and the create is what flips that, for the
/// id that store's own create reports.
///
/// The ids are never compared between the two stores: this emulator mints from a
/// deterministic sequence, so two independently mounted control planes can mint the same
/// id, and what tells the rows apart is which store holds the record.
#[tokio::test]
async fn the_organization_one_account_created_is_not_the_caller_s_in_another_store() {
    let created = Emulator::mounted();
    let created_id = created.create_organization().await;
    let standalone = Emulator::mounted();

    // The store that holds the record serves it.
    let written = created
        .signin(PUT_CONFIGURATION, &json!({ "targetId": created_id.clone() }))
        .await;
    assert_eq!(
        (written.status, reported(&written)),
        (200, (created_id.as_str(), ORGANIZATION_SCOPE, Some(true))),
        "case \"its own organization\": the organization this store records must be served \
         for {created_id:?}, got {} with {}",
        written.status,
        written.body
    );

    // The store that holds none refuses the very same id, on the write and both reads.
    assert_denied(
        &standalone,
        "an organization no account of this store has created",
        &created_id,
    )
    .await;
    assert_eq!(
        standalone.configured_targets(),
        Vec::<String>::new(),
        "a denied put must record no configuration at all, got {:?}",
        standalone.configured_targets()
    );

    // An account-form target names no Organizations entity, so that same store serves it.
    let own_account = standalone.signin(PUT_CONFIGURATION, &json!({})).await;
    assert_eq!(
        own_account.status, 200,
        "case \"the caller's own account\": an account-form target must be served by a store \
         that records no organization, got {} with {}",
        own_account.status, own_account.body
    );

    // And once an account of that store creates its organization, the id ITS create
    // reports is served there too: the authority came from the create and nothing else.
    let standalone_id = standalone.create_organization().await;
    let served = standalone
        .signin(
            PUT_CONFIGURATION,
            &json!({ "targetId": standalone_id.clone() }),
        )
        .await;
    assert_eq!(
        (served.status, reported(&served)),
        (200, (standalone_id.as_str(), ORGANIZATION_SCOPE, Some(true))),
        "case \"the organization this account has now created\": {standalone_id:?} must be \
         served, got {} with {}",
        served.status,
        served.body
    );
}
