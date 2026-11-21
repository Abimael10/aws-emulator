// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the Region a Customer Profiles domain belongs to.
//!
//! The rule under test: a domain is a REGIONAL resource — it belongs to the Region its
//! `CreateDomain` call was signed for, so a request signed for another Region reaches none
//! of it: every domain-scoped operation answers that Region's "no such domain" refusal, and
//! a listing reports only the domains of the Region it was signed for. The name is unique
//! within a Region only, so the same name in two Regions is two independent domains.
//!
//! Each rule is asserted over a SET of inputs — one row per domain-scoped operation, and the
//! two Regions in both directions — rather than for one recorded request. Every case goes
//! through the real router, so what is asserted is the wire behaviour a client sees. The
//! requests are built here rather than through the shared harness because what they vary is
//! the Region the SigV4 credential scope names, which that harness fixes.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;
use std::sync::Arc;

use service_com_amazonaws_customerprofiles_customerprofiles_20200815::state::ARN_SERVICE;
use service_com_amazonaws_customerprofiles_customerprofiles_20200815::EmulatedService;

/// The Region the domains under test are created in.
const REGION: &str = "us-east-1";

/// Another Region of the same account, which every refusal row is signed for.
const OTHER_REGION: &str = "eu-west-1";

/// The domain every row addresses, created in [`REGION`] only.
const DOMAIN: &str = "regiondom";

/// One emulator behind the real router, reachable from any Region: a single shared engine,
/// so every request in a test observes the same state.
struct Emulator {
    router: MultiServiceRouter,
}

impl Emulator {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        router.set_request_authentication(false);
        router.register_service(EmulatedService::new(engine));
        Self { router }
    }

    /// One request signed for `region`: the credential scope is what says which Region the
    /// caller reached, and the signature itself is never checked — these tests run with
    /// request authentication off, exactly as the emulator does by default.
    async fn send(
        &self,
        region: &str,
        method: &str,
        path: &str,
        body: Option<Value>,
    ) -> (u16, String, Value) {
        let payload = match &body {
            Some(value) => bytes::Bytes::from(serde_json::to_vec(value).expect("serialize body")),
            None => bytes::Bytes::new(),
        };
        let authorization = format!(
            "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{region}/profile/aws4_request, \
             SignedHeaders=host, Signature=0"
        );
        let request = http::Request::builder()
            .method(method)
            .uri(format!("https://profile.{region}.amazonaws.com{path}"))
            .header("authorization", authorization)
            .header("content-type", "application/json")
            .body(payload)
            .expect("build request");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let error_type = response
            .headers()
            .get("x-amzn-errortype")
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_string();
        let value = if response.body().is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(response.body()).unwrap_or(Value::Null)
        };
        (status, error_type, value)
    }

    /// `CreateDomain` in `region`, asserting it succeeded.
    async fn create_domain(&self, region: &str, domain: &str, expiration_days: u32) -> Value {
        let (status, _, body) = self
            .send(
                region,
                "POST",
                &format!("/domains/{domain}"),
                Some(json!({ "DefaultExpirationDays": expiration_days })),
            )
            .await;
        assert_eq!(
            status, 200,
            "CreateDomain {domain:?} in {region}: expected 200, got {status} {body:?}"
        );
        body
    }

    /// `CreateProfile` in `region`, asserting it succeeded, and returning the `ProfileId`.
    async fn create_profile(&self, region: &str, domain: &str, profile: Value) -> String {
        let (status, _, body) = self
            .send(
                region,
                "POST",
                &format!("/domains/{domain}/profiles"),
                Some(profile.clone()),
            )
            .await;
        assert_eq!(
            status, 200,
            "CreateProfile in {region}: expected 200, got {status} {body:?}"
        );
        body.get("ProfileId")
            .and_then(Value::as_str)
            .unwrap_or_else(|| panic!("no ProfileId in {body:?}"))
            .to_string()
    }
}

/// The wire error code restJson1 carries in the body's `__type` discriminator.
fn error_code(body: &Value) -> &str {
    body.get("__type").and_then(Value::as_str).unwrap_or("")
}

/// The `Message` member of a modelled error body.
fn error_message(body: &Value) -> &str {
    body.get("Message")
        .or_else(|| body.get("message"))
        .and_then(Value::as_str)
        .unwrap_or("")
}

/// The sentence the profile-collection operations word an absent domain with: the input
/// member, the name the request carried and the account it was looked for in.
fn profile_collection_not_found(domain: &str) -> String {
    format!(
        "DomainName {domain} is not found in account {}",
        engine::emulator_account_id()
    )
}

/// One request addressing a domain, and the sentence the service refuses it with when the
/// Region it is signed for holds no domain of that name.
struct Row {
    case: &'static str,
    method: &'static str,
    path: String,
    body: Option<Value>,
    /// The refusal's `Message`, or `None` for the operation whose refusal carries none.
    message: Option<String>,
}

/// Every domain-scoped operation this service implements, as a request against [`DOMAIN`]
/// and the sentence it words an absent domain with. The rows span the shape of the request
/// (GET, POST, PUT, DELETE; with and without a body), the resource underneath the domain the
/// request addresses, and all three sentences this service uses for an absent domain.
///
/// `profile_id` is a profile of the domain, so a row addressing one names a profile that
/// really is there when the request reaches the Region the domain lives in.
fn domain_scoped_rows(profile_id: &str) -> Vec<Row> {
    vec![
        Row {
            case: "GetDomain",
            method: "GET",
            path: format!("/domains/{DOMAIN}"),
            body: None,
            message: Some(format!("Domain {DOMAIN} is not found")),
        },
        Row {
            case: "UpdateDomain",
            method: "PUT",
            path: format!("/domains/{DOMAIN}"),
            body: Some(json!({ "DefaultExpirationDays": 10 })),
            // This operation's refusal carries no prose at all.
            message: None,
        },
        Row {
            case: "SearchProfiles",
            method: "POST",
            path: format!("/domains/{DOMAIN}/profiles/search"),
            body: Some(json!({ "KeyName": "_account", "Values": ["ACCT-1"] })),
            message: Some(profile_collection_not_found(DOMAIN)),
        },
        Row {
            case: "CreateProfile",
            method: "POST",
            path: format!("/domains/{DOMAIN}/profiles"),
            body: Some(json!({ "AccountNumber": "ACCT-2" })),
            message: Some(profile_collection_not_found(DOMAIN)),
        },
        Row {
            case: "AddProfileKey",
            method: "POST",
            path: format!("/domains/{DOMAIN}/profiles/keys"),
            body: Some(json!({
                "ProfileId": profile_id,
                "KeyName": "LoyaltyId",
                "Values": ["L-1"],
            })),
            message: Some(format!("Domain {DOMAIN} is not found")),
        },
        Row {
            case: "PutProfileObjectType",
            method: "PUT",
            path: format!("/domains/{DOMAIN}/object-types/Orders"),
            body: Some(json!({ "Description": "orders" })),
            message: Some(format!("Domain {DOMAIN} is not found")),
        },
        Row {
            case: "ListCalculatedAttributeDefinitions",
            method: "GET",
            path: format!("/domains/{DOMAIN}/calculated-attributes"),
            body: None,
            message: Some(format!("Domain {DOMAIN} is not found")),
        },
        Row {
            case: "ListEventStreams",
            method: "GET",
            path: format!("/domains/{DOMAIN}/event-streams"),
            body: None,
            message: Some(format!("Domain {DOMAIN} is not found")),
        },
        Row {
            case: "CreateUploadJob",
            method: "POST",
            path: format!("/domains/{DOMAIN}/upload-jobs"),
            body: Some(json!({
                "DisplayName": "job",
                "Fields": {
                    "accountNumber": {
                        "ContentType": "STRING",
                        "Source": "_source.accountNumber",
                        "Target": "_profile.AccountNumber",
                    },
                },
                "UniqueKey": "accountNumber",
            })),
            message: Some(format!("Domain with name {DOMAIN} could not be found")),
        },
        Row {
            case: "DeleteDomain",
            method: "DELETE",
            path: format!("/domains/{DOMAIN}"),
            body: None,
            message: Some(format!("Domain {DOMAIN} is not found")),
        },
    ]
}

/// RULE: a domain belongs to the Region it was created in, so every domain-scoped operation
/// signed for another Region is refused `ResourceNotFoundException` (404) — and the same
/// request signed for the creating Region is served.
#[tokio::test]
async fn a_domain_is_reachable_only_from_the_region_it_was_created_in() {
    let emulator = Emulator::new();
    emulator.create_domain(REGION, DOMAIN, 90).await;
    let profile_id = emulator
        .create_profile(REGION, DOMAIN, json!({ "AccountNumber": "ACCT-1" }))
        .await;

    for row in domain_scoped_rows(&profile_id) {
        let (status, error_type, body) = emulator
            .send(OTHER_REGION, row.method, &row.path, row.body.clone())
            .await;
        assert_eq!(
            (status, error_code(&body)),
            (404, "ResourceNotFoundException"),
            "case {:?}: expected 404 ResourceNotFoundException from {OTHER_REGION}, got \
             {status} {body:?}",
            row.case
        );
        assert!(
            error_type.contains("ResourceNotFoundException"),
            "case {:?}: expected the error type header to name the shape, got {error_type:?}",
            row.case
        );
        match &row.message {
            Some(message) => assert_eq!(
                error_message(&body),
                message.as_str(),
                "case {:?}: expected the service's wording, got {:?}",
                row.case,
                error_message(&body)
            ),
            None => assert!(
                body.get("Message").is_some_and(Value::is_null)
                    || body.get("Message").is_none(),
                "case {:?}: this operation's refusal carries no sentence, got {body:?}",
                row.case
            ),
        }
    }

    // Nothing above reached the domain: it is still there, with the profile it was given,
    // and the same requests signed for its own Region are served.
    for row in domain_scoped_rows(&profile_id) {
        let (status, _, body) = emulator
            .send(REGION, row.method, &row.path, row.body.clone())
            .await;
        assert_ne!(
            (status, error_code(&body)),
            (404, "ResourceNotFoundException"),
            "case {:?}: expected the request signed for {REGION} to reach the domain, got \
             {status} {body:?}",
            row.case
        );
    }
}

/// RULE: a `DomainName` is unique within one Region only, so the same name in two Regions is
/// two independent domains — each read reports its own configuration, and the profiles of one
/// are not found through the other.
#[tokio::test]
async fn the_same_name_in_two_regions_is_two_independent_domains() {
    let emulator = Emulator::new();
    // (the Region, the expiration that Region's domain was created with, the account number
    // of the profile it holds)
    let rows = [(REGION, 90_u32, "ACCT-HERE"), (OTHER_REGION, 366, "ACCT-THERE")];
    for (region, expiration_days, account) in rows {
        emulator.create_domain(region, DOMAIN, expiration_days).await;
        emulator
            .create_profile(region, DOMAIN, json!({ "AccountNumber": account }))
            .await;
    }

    for (region, expiration_days, account) in rows {
        let (status, _, body) = emulator
            .send(region, "GET", &format!("/domains/{DOMAIN}"), None)
            .await;
        assert_eq!(
            (status, body.get("DefaultExpirationDays").and_then(Value::as_i64)),
            (200, Some(i64::from(expiration_days))),
            "case {region}: expected the configuration that Region's create stored, got \
             {status} {body:?}"
        );

        // The profile of this Region's domain is found here, and the other Region's is not.
        for (_, _, searched_account) in rows {
            let expected = searched_account == account;
            let (status, _, body) = emulator
                .send(
                    region,
                    "POST",
                    &format!("/domains/{DOMAIN}/profiles/search"),
                    Some(json!({ "KeyName": "_account", "Values": [searched_account] })),
                )
                .await;
            let found = body
                .get("Items")
                .and_then(Value::as_array)
                .is_some_and(|items| !items.is_empty());
            assert_eq!(
                (status, found),
                (200, expected),
                "case {region} searching {searched_account}: expected found = \
                 {expected}, got {status} {body:?}"
            );
        }
    }

    // Deleting one Region's domain leaves the other Region's standing: they are two records.
    let (status, _, body) = emulator
        .send(OTHER_REGION, "DELETE", &format!("/domains/{DOMAIN}"), None)
        .await;
    assert_eq!(status, 200, "DeleteDomain in {OTHER_REGION}: got {status} {body:?}");
    let (status, _, body) = emulator
        .send(REGION, "GET", &format!("/domains/{DOMAIN}"), None)
        .await;
    assert_eq!(
        status, 200,
        "after the other Region's delete: expected {REGION}'s domain to stand, got {status} \
         {body:?}"
    );
    let (status, _, body) = emulator
        .send(OTHER_REGION, "GET", &format!("/domains/{DOMAIN}"), None)
        .await;
    assert_eq!(
        (status, error_code(&body)),
        (404, "ResourceNotFoundException"),
        "after its delete: expected {OTHER_REGION} to hold no such domain, got {status} {body:?}"
    );
}

/// RULE: a listing reports the domains of the Region it was signed for, and no other's.
#[tokio::test]
async fn a_listing_reports_only_the_domains_of_the_region_it_was_signed_for() {
    let emulator = Emulator::new();
    // (the Region, the domains created there)
    let rows: [(&str, &[&str]); 2] = [
        (REGION, &["here_one", "here_two"]),
        (OTHER_REGION, &["there_one"]),
    ];
    for (region, domains) in rows {
        for domain in domains {
            emulator.create_domain(region, domain, 90).await;
        }
    }

    for (region, domains) in rows {
        let (status, _, body) = emulator.send(region, "GET", "/domains", None).await;
        let listed: Vec<String> = body
            .get("Items")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(|item| item.get("DomainName").and_then(Value::as_str))
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        let expected: Vec<String> = domains.iter().map(|name| (*name).to_string()).collect();
        assert_eq!(
            (status, listed.clone()),
            (200, expected.clone()),
            "case {region}: expected exactly {expected:?}, got {status} {listed:?}"
        );
    }

    // A Region that holds no domain at all lists none, rather than another Region's.
    let (status, _, body) = emulator.send("ap-south-1", "GET", "/domains", None).await;
    assert_eq!(
        (status, body.get("Items")),
        (200, Some(&json!([]))),
        "case an unprovisioned Region: expected an empty list, got {status} {body:?}"
    );
}

/// RULE: the Region field of a credential scope is caller-written, so a value that is not a
/// Region name names no Region of its own: such a request is attributed to the emulator's
/// default Region rather than opening a store of its own.
#[tokio::test]
async fn a_scope_whose_region_field_is_no_region_name_is_the_default_region() {
    let emulator = Emulator::new();
    // The default Region is the one the harness above calls [`REGION`], so a domain created
    // there is what a request with an unusable scope must reach.
    emulator.create_domain(REGION, DOMAIN, 90).await;

    // (case, the Region field the scope carries, whether it is attributed to the default)
    for (case, signed_for, is_default) in [
        ("a Region name", REGION, true),
        ("another Region name", OTHER_REGION, false),
        ("a field with too few segments", "useast1", true),
        ("a field whose last segment is not a number", "us-east-one", true),
        ("a field with too many segments", "us-gov-east-west-1", true),
        ("a field of upper-case letters", "US-EAST-1", true),
        ("a field far longer than any Region name", &"a".repeat(4096), true),
    ] {
        let (status, _, body) = emulator
            .send(signed_for, "GET", &format!("/domains/{DOMAIN}"), None)
            .await;
        let reached = status == 200;
        assert_eq!(
            reached, is_default,
            "case {case:?}: expected the default Region's domain to be reached = \
             {is_default}, got {status} {body:?}"
        );
    }
}

/// RULE: the resources nested under a domain are addressed by an ARN that names the Region
/// the domain lives in, so the tagging surface resolves an ARN in that Region only: the ARN
/// of another Region names nothing this store holds.
#[tokio::test]
async fn a_tagging_arn_resolves_in_the_region_the_arn_names() {
    let emulator = Emulator::new();
    emulator.create_domain(REGION, DOMAIN, 90).await;

    // (case, the Region the ARN names, whether it resolves)
    for (case, arn_region, resolves) in [
        ("the Region the domain was created in", REGION, true),
        ("another Region", OTHER_REGION, false),
    ] {
        let resource_arn = engine::emulator_arn(
            ARN_SERVICE,
            arn_region,
            &format!("domains/{DOMAIN}"),
        );
        let encoded: String = resource_arn
            .chars()
            .map(|character| match character {
                'A'..='Z' | 'a'..='z' | '0'..='9' | '-' | '.' | '_' | '~' => character.to_string(),
                other => other
                    .to_string()
                    .bytes()
                    .map(|byte| format!("%{byte:02X}"))
                    .collect(),
            })
            .collect();
        let (status, _, body) = emulator
            .send(REGION, "GET", &format!("/tags/{encoded}"), None)
            .await;
        if resolves {
            assert_eq!(
                status, 200,
                "case {case:?}: expected the ARN to resolve, got {status} {body:?}"
            );
        } else {
            assert_eq!(
                (status, error_code(&body)),
                (404, "NotFoundException"),
                "case {case:?}: expected the tagging surface's not-found, got {status} {body:?}"
            );
        }
    }
}
