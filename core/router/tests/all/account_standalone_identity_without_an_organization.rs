// SPDX-License-Identifier: CC0-1.0

//! Account Management's standalone-identity behaviour on an emulator nobody has created an
//! organization on: the probe a client uses to tell a standalone account from a member
//! account, and the refusal that standalone account answers a self-targeted call with.
//!
//! RULE: membership of an AWS Organizations organization is STATE a request establishes —
//! the account the `CreateOrganization` call is made from becomes the management account of
//! the organization it mints — so mounting a control plane establishes no membership and
//! every organization-scoped read answers the modelled HTTP 404
//! `AWSOrganizationsNotInUseException` ("Your account isn't a member of an organization. To
//! make this request, you must use the credentials of an account that belongs to an
//! organization.", all fixed text) until that create has happened; and in such a standalone
//! account an Account Management operation whose account-id member names the CALLER'S OWN
//! account is refused the modelled HTTP 403 `AccessDeniedException` and writes nothing, the
//! caller's own account being reached by the standalone call that omits the member.
//!
//! Only a cross-service suite can pin this: the membership lives in the Organizations store,
//! the self-target gate is Account Management's, and the client that must choose between the
//! standalone and the member path reads both over the wire. Both services are mounted on ONE
//! engine, exactly as `core/router/src/lib.rs` mounts them, and every assertion goes over
//! the wire each service publishes (`awsJson1_1` with an `X-Amz-Target` for Organizations,
//! `restJson1` paths for Account Management).
//!
//! The rows span what the rule generalises over: which organization-scoped read is probed,
//! how often, and over several independent boots; which Account Management operation names
//! the caller's own account and what else its request carries (addresses at both published
//! `PrimaryEmail` `@length` bounds and of differing form, a one-time passcode); and whether
//! an organization has since been created, over the model's whole `OrganizationFeatureSet`
//! catalogue — with the member-account call as the positive control, so the refusal is shown
//! to key on being the caller's own account rather than on the operation.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The Region both control planes are addressed in. Membership is organization-wide and the
/// account record is global, so the Region selects nothing; one is needed because the
/// credential scope is what routes a request.
const REGION: &str = "us-east-1";

/// The `X-Amz-Target` prefix AWS Organizations routes on (that crate's `OPERATIONS.md`).
const ORGANIZATIONS_TARGET_PREFIX: &str = "AWSOrganizationsV20161128";

/// The `restJson1` paths of the Account Management operations this suite drives, from
/// `smithy_query(query="operation", ..)`'s `http.uri`.
const GET_ACCOUNT_INFORMATION: &str = "/getAccountInformation";
const GET_PRIMARY_EMAIL: &str = "/getPrimaryEmail";
const START_PRIMARY_EMAIL_UPDATE: &str = "/startPrimaryEmailUpdate";
const ACCEPT_PRIMARY_EMAIL_UPDATE: &str = "/acceptPrimaryEmailUpdate";
const GET_PRIMARY_EMAIL_UPDATE_STATUS: &str = "/getPrimaryEmailUpdateStatus";
const PUT_ACCOUNT_NAME: &str = "/putAccountName";

/// The modelled refusal of an organization-scoped read made from an account that belongs to
/// no organization, from `smithy_query(query="shape",
/// shape="AWSOrganizationsNotInUseException")`: HTTP 404, and the error's own sentence,
/// spelled out here rather than imported because the wording IS the requirement.
const NOT_IN_USE: (u16, &str) = (404, "AWSOrganizationsNotInUseException");
const NOT_IN_USE_MESSAGE: &str = "Your account isn't a member of an organization. To make this request, you must use the credentials of an account that belongs to an organization.";

/// The modelled refusal an Account Management operation answers a request it may not serve
/// for the account it names with: HTTP 403 `AccessDeniedException`.
const DENIED: (u16, &str) = (403, "AccessDeniedException");

/// The modelled refusal of a call that needs an in-flight primary email update on an account
/// that has none: HTTP 400 `ValidationException`, with that condition's fixed sentence.
const NO_UPDATE: (u16, &str) = (400, "ValidationException");
const NO_UPDATE_MESSAGE: &str = "No email update found for account";

/// The values `OrganizationFeatureSet` admits, from `smithy_query(query="shape",
/// shape="OrganizationFeatureSet")` — spelled out here, not imported, so a wrong set in the
/// implementation cannot make this suite pass.
const FEATURE_SETS: [&str; 2] = ["ALL", "CONSOLIDATED_BILLING"];

/// `PrimaryEmailAddress`'s published `@length`, from `smithy_query(query="operation",
/// operation="StartPrimaryEmailUpdate")`.
const PRIMARY_EMAIL_MIN: usize = 5;
const PRIMARY_EMAIL_MAX: usize = 64;

/// A passcode inside `Otp`'s `^[a-zA-Z0-9]{6}$`, so a row carrying one is never refused for
/// a malformed member.
const WELL_FORMED_OTP: &str = "a1B2c3";

/// An account id inside `AccountId`'s `^\d{12}$` that is not the caller's own: the positive
/// control's member account.
const MEMBER_ACCOUNT: &str = "111111111111";

/// How many independent mounts of the emulator the probe is asserted over, so a value that
/// happened to be right once cannot carry the assertion.
const BOOTS: usize = 3;

/// The organization-scoped reads a client can probe membership with, each with a well-formed
/// body, so a refusal is for the MEMBERSHIP the call needs and never for its input.
fn membership_probes() -> Vec<(&'static str, Value)> {
    vec![
        ("DescribeOrganization", json!({})),
        ("ListRoots", json!({})),
    ]
}

/// What the emulator answered: the HTTP status and the parsed body.
struct Answer {
    status: u16,
    body: Value,
}

impl Answer {
    /// The wire error code, taken off the `__type` both front ends carry, or the empty
    /// string on a success.
    fn error_code(&self) -> &str {
        let code = self.body["__type"].as_str().unwrap_or_default();
        code.rsplit('#').next().unwrap_or_default()
    }

    /// The status and the error code together, which is what a refused row asserts.
    fn refusal(&self) -> (u16, &str) {
        (self.status, self.error_code())
    }

    /// One string member of the body, or the empty string when it is absent: the two error
    /// envelopes spell the sentence differently (`Message` for Organizations' `awsJson1_1`,
    /// `message` for Account Management's `restJson1`).
    fn member(&self, name: &str) -> &str {
        self.body[name].as_str().unwrap_or_default()
    }
}

/// Account Management and AWS Organizations on one engine.
struct Emulator {
    router: MultiServiceRouter,
}

impl Emulator {
    /// Both services mounted on one fresh engine, and nothing else: no request has been made
    /// yet, so this account belongs to no organization.
    fn mounted() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        // These callers sign nothing, so they must not be refused for it; the credential
        // scope below is what routes each request and names its Region.
        router.set_request_authentication(false);
        router.register_service(
            service_com_amazonaws_account_account::EmulatedService::new(Arc::clone(&engine)),
        );
        router.register_service(
            service_com_amazonaws_organizations_awsorganizationsv20161128::EmulatedService::new(
                engine,
            ),
        );
        Self { router }
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

    /// One Account Management `restJson1` call: `POST path` with `body`.
    async fn account(&self, path: &str, body: &Value) -> Answer {
        let request = ::http::Request::builder()
            .method("POST")
            .uri(path)
            .header("host", format!("account.{REGION}.amazonaws.com"))
            .header("content-type", "application/json")
            .header("authorization", credential_scope("account"))
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

    /// The caller's own account id, read off Account Management's OWN standalone identity
    /// read (the call that omits the account-id member), so the self-targeted rows below
    /// name the account this service itself says the caller is in.
    async fn own_account_id(&self, case: &str) -> String {
        let answer = self.account(GET_ACCOUNT_INFORMATION, &json!({})).await;
        assert_eq!(
            answer.status, 200,
            "case {case:?}: the standalone identity read must be served, got {} with {}",
            answer.status, answer.body
        );
        let account_id = answer.member("AccountId").to_string();
        assert!(
            account_id.len() == 12 && account_id.chars().all(|digit| digit.is_ascii_digit()),
            "case {case:?}: expected an AccountId matching ^\\d{{12}}$, got {:?}",
            answer.body["AccountId"]
        );
        account_id
    }

    /// Assert every organization-scoped probe answers the modelled not-in-use refusal, each
    /// one twice: a read must not establish the membership it needed.
    async fn assert_in_no_organization(&self, world: &str) {
        for (operation, body) in membership_probes() {
            for round in 1..=2 {
                let case = format!("{operation} {world}, call {round}");
                let answer = self.organizations(operation, &body).await;
                assert_eq!(
                    answer.refusal(),
                    NOT_IN_USE,
                    "case {case:?}: expected {NOT_IN_USE:?}, got {:?} with {}",
                    answer.refusal(),
                    answer.body
                );
                assert_eq!(
                    answer.member("Message"),
                    NOT_IN_USE_MESSAGE,
                    "case {case:?}: expected the error's own fixed sentence, got {:?}",
                    answer.body["Message"]
                );
            }
        }
    }

    /// Assert the standalone status read reports no primary email update at all: the modelled
    /// 400 with that condition's fixed sentence, which is what "the refused start recorded
    /// nothing" looks like to a client.
    async fn assert_no_update_started(&self, case: &str) {
        let answer = self
            .account(GET_PRIMARY_EMAIL_UPDATE_STATUS, &json!({}))
            .await;
        assert_eq!(
            answer.refusal(),
            NO_UPDATE,
            "case {case:?}: expected {NO_UPDATE:?}, got {:?} with {}",
            answer.refusal(),
            answer.body
        );
        assert_eq!(
            answer.member("message"),
            NO_UPDATE_MESSAGE,
            "case {case:?}: expected the absent update's fixed sentence, got {:?}",
            answer.body["message"]
        );
    }
}

/// A SigV4 `Authorization` header whose credential scope names `signing_name` in [`REGION`] —
/// the signal the router resolves a request's service from. The signature is never checked:
/// this suite runs with request authentication off.
fn credential_scope(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{signing_name}/aws4_request, \
         SignedHeaders=host, Signature=0"
    )
}

/// An address of exactly `length` characters: the domain keeps it an address, so only the
/// length varies.
fn address_of_length(length: usize) -> String {
    let domain = "@example.com";
    assert!(
        length > domain.len(),
        "case length {length}: an address needs a local part before {domain}"
    );
    format!("{}{domain}", "a".repeat(length - domain.len()))
}

/// Every Account Management call of this slice that names an account, driven at the CALLER'S
/// OWN account id: the operation, and the rest of its well-formed request.
fn self_targeted_rows(own: &str) -> Vec<(String, &'static str, Value)> {
    vec![
        (
            "StartPrimaryEmailUpdate with a plain address".to_string(),
            START_PRIMARY_EMAIL_UPDATE,
            json!({"AccountId": own, "PrimaryEmail": "ada@example.com"}),
        ),
        (
            format!(
                "StartPrimaryEmailUpdate with an address at the {PRIMARY_EMAIL_MIN}-character \
                 bound"
            ),
            START_PRIMARY_EMAIL_UPDATE,
            json!({"AccountId": own, "PrimaryEmail": "a@b.c"}),
        ),
        (
            format!(
                "StartPrimaryEmailUpdate with an address at the {PRIMARY_EMAIL_MAX}-character \
                 bound"
            ),
            START_PRIMARY_EMAIL_UPDATE,
            json!({
                "AccountId": own,
                "PrimaryEmail": address_of_length(PRIMARY_EMAIL_MAX),
            }),
        ),
        (
            "StartPrimaryEmailUpdate with a plus-tagged address on a two-label domain"
                .to_string(),
            START_PRIMARY_EMAIL_UPDATE,
            json!({"AccountId": own, "PrimaryEmail": "ada+root@mail.example.co.uk"}),
        ),
        (
            "GetPrimaryEmail".to_string(),
            GET_PRIMARY_EMAIL,
            json!({"AccountId": own}),
        ),
        (
            "AcceptPrimaryEmailUpdate".to_string(),
            ACCEPT_PRIMARY_EMAIL_UPDATE,
            json!({
                "AccountId": own,
                "PrimaryEmail": "ada@example.com",
                "Otp": WELL_FORMED_OTP,
            }),
        ),
    ]
}

/// Every fresh boot presents an account that belongs to NO organization, and says so through
/// the read a client probes membership with — so the standalone branch of that probe is
/// reachable at all. Reading Account Management's own identity does not change the answer.
#[tokio::test]
async fn every_fresh_boot_presents_an_account_the_membership_probe_reports_as_standalone() {
    for boot in 1..=BOOTS {
        let emulator = Emulator::mounted();
        let world = format!("on boot {boot} of an emulator nobody created an organization on");
        emulator.assert_in_no_organization(&world).await;

        // The account the probe is about is served by Account Management all the same: being
        // in no organization is not being unknown.
        let own = emulator.own_account_id(&world).await;

        // ...and that identity read established no membership either.
        emulator
            .assert_in_no_organization(&format!("{world}, after reading the caller's own account"))
            .await;
        assert!(
            !own.is_empty(),
            "case {world:?}: the standalone read must report the caller's own account id"
        );
    }
}

/// In that standalone account, every call naming the caller's OWN account id is refused the
/// modelled 403 — and starts nothing: the standalone status read still reports no update, the
/// standalone identity read still reports exactly what it reported before, and the membership
/// probe is still the standalone answer.
#[tokio::test]
async fn a_standalone_account_refuses_every_self_targeted_call_and_starts_nothing() {
    let emulator = Emulator::mounted();
    // The precondition the standalone branch needs, asserted rather than assumed.
    emulator
        .assert_in_no_organization("before any Account Management call")
        .await;

    let own = emulator.own_account_id("the caller's own account").await;
    let before = emulator.account(GET_ACCOUNT_INFORMATION, &json!({})).await;
    emulator
        .assert_no_update_started("the standalone status read before any start")
        .await;

    for (case, path, request) in self_targeted_rows(&own) {
        // Twice: a refusal is not a one-shot that a repeat turns into a 200.
        for attempt in 1..=2 {
            let answer = emulator.account(path, &request).await;
            assert_eq!(
                answer.refusal(),
                DENIED,
                "case {case:?} (attempt {attempt}): expected {DENIED:?}, got {:?} with {}",
                answer.refusal(),
                answer.body
            );
            assert!(
                !answer.member("message").contains(&own),
                "case {case:?} (attempt {attempt}): the refusal must not echo the account id, \
                 got {:?}",
                answer.body["message"]
            );
        }
        // The refused call recorded nothing for the account it named.
        emulator
            .assert_no_update_started(&format!("the standalone status read after {case}"))
            .await;
    }

    let after = emulator.account(GET_ACCOUNT_INFORMATION, &json!({})).await;
    assert_eq!(
        (after.status, &after.body),
        (before.status, &before.body),
        "case \"the standalone identity read after every refusal\": the refused calls must \
         have written nothing, got {} with {}",
        after.status,
        after.body
    );
    emulator
        .assert_in_no_organization("after every self-targeted call was refused")
        .await;
}

/// The create is what flips the probe: behind it the organization-scoped reads are served and
/// report the organization of THIS account, whatever feature set the create asked for — while
/// the self-target gate stands, and a MEMBER account's start is served, so the refusal is
/// shown to key on being the caller's own account.
#[tokio::test]
async fn the_create_flips_the_membership_probe_while_the_self_target_gate_stands() {
    // The model's own feature-set catalogue, plus the request that omits the optional member.
    let mut cases: Vec<Option<&str>> = vec![None];
    cases.extend(FEATURE_SETS.iter().copied().map(Some));

    for requested in cases {
        let emulator = Emulator::mounted();
        let world = match requested {
            None => "an organization created with no FeatureSet".to_string(),
            Some(value) => format!("an organization created with FeatureSet={value}"),
        };
        emulator
            .assert_in_no_organization(&format!("before {world}"))
            .await;
        let own = emulator.own_account_id(&world).await;

        let created = emulator
            .organizations(
                "CreateOrganization",
                &match requested {
                    Some(value) => json!({ "FeatureSet": value }),
                    None => json!({}),
                },
            )
            .await;
        assert_eq!(
            created.status, 200,
            "case {world:?}: the create must establish the organization, got {} with {}",
            created.status, created.body
        );

        // The probe now takes its other branch, and reports the record the create wrote: the
        // management account is the account Account Management itself reports the caller as.
        let described = emulator.organizations("DescribeOrganization", &json!({})).await;
        assert_eq!(
            described.status, 200,
            "case {world:?}: the probe must be served behind the create, got {} with {}",
            described.status, described.body
        );
        assert_eq!(
            described.body["Organization"], created.body["Organization"],
            "case {world:?}: the probe must report what the create persisted, got {}",
            described.body
        );
        assert_eq!(
            described.body["Organization"]["MasterAccountId"].as_str(),
            Some(own.as_str()),
            "case {world:?}: the create's own account is the management account, got {}",
            described.body
        );
        let roots = emulator.organizations("ListRoots", &json!({})).await;
        assert_eq!(
            roots.status, 200,
            "case {world:?}: every organization-scoped read must be served behind the create, \
             got {} with {}",
            roots.status, roots.body
        );

        // Membership changes nothing about the self-target gate: it is about WHICH account a
        // request names, not about the organization the caller is in.
        for (case, path, request) in self_targeted_rows(&own) {
            let answer = emulator.account(path, &request).await;
            assert_eq!(
                answer.refusal(),
                DENIED,
                "case {case:?} with {world}: expected {DENIED:?}, got {:?} with {}",
                answer.refusal(),
                answer.body
            );
        }
        emulator
            .assert_no_update_started(&format!("the standalone status read with {world}"))
            .await;

        // The positive control: the same operation, naming a MEMBER account, is served and
        // the update it starts is the one the status read then reports.
        let named = emulator
            .account(
                PUT_ACCOUNT_NAME,
                &json!({"AccountId": MEMBER_ACCOUNT, "AccountName": "A member account"}),
            )
            .await;
        assert_eq!(
            named.status, 200,
            "case \"PutAccountName on the member account\" with {world}: expected 200, got {} \
             with {}",
            named.status, named.body
        );
        let started = emulator
            .account(
                START_PRIMARY_EMAIL_UPDATE,
                &json!({"AccountId": MEMBER_ACCOUNT, "PrimaryEmail": "member@example.com"}),
            )
            .await;
        assert_eq!(
            started.status, 200,
            "case \"StartPrimaryEmailUpdate on the member account\" with {world}: expected 200, \
             got {} with {}",
            started.status, started.body
        );
        let reported = emulator
            .account(
                GET_PRIMARY_EMAIL_UPDATE_STATUS,
                &json!({"AccountId": MEMBER_ACCOUNT}),
            )
            .await;
        assert_eq!(
            (reported.status, reported.body["Status"].as_str()),
            (200, Some("PENDING")),
            "case \"the member account's status read\" with {world}: the started update must be \
             reported, got {} with {}",
            reported.status,
            reported.body
        );
    }
}
