// SPDX-License-Identifier: CC0-1.0

//! Provisioning profiles, end to end over the wire: `CreateProvisioningProfile` ->
//! `GetProvisioningProfile` -> `ListProvisioningProfiles`, plus the `ListTagsForResource` the
//! ARN a create mints resolves through.
//!
//! RULE: `CreateProvisioningProfile` (POST `/provisioning-profiles`, success 201) MINTS a
//! provisioning profile from the request and persists it: it answers the generated
//! `ProvisioningProfileId`, the `ProvisioningProfileArn` of that id in the caller's region,
//! the `Name` and `ProvisioningType` the request carried, the PEM-encoded `ClaimCertificate`
//! the profile provisions devices with — the one the request supplied, else one GENERATED
//! for it, whose `ClaimCertificatePrivateKey` is answered with it — and the terminal
//! `ProvisioningProfileStatus` `CREATED`, the profile being created inside the call. A create
//! presenting a `ClientToken` an earlier one presented answers that first call's profile and
//! mints no second one, concurrently as well as sequentially. `GetProvisioningProfile` (GET
//! `/provisioning-profiles/{Identifier}`, `@readonly`) and `ListProvisioningProfiles` (GET
//! `/provisioning-profiles`, `@readonly`) project that same STORED record — never the private
//! key, which no read shape declares — the list in creation order, cut at `MaxResults` with a
//! `NextToken` that resumes exactly where the page stopped; an identifier the caller's
//! region holds no profile under is the modelled `ResourceNotFoundException` (HTTP 404); and
//! `DeleteProvisioningProfile` (DELETE `/provisioning-profiles/{Identifier}`) removes the
//! profile INSIDE its own call: from the moment it answers no reader resolves it, the
//! `ClientToken` that minted it replays into nothing, and every operation that resolves that
//! identifier answers this resource's one fixed 404.
//!
//! The recorded units this was derived from
//! (`iot-managed-integrations.provisioning-profile-lifecycle-status`,
//! `iot-managed-integrations.provisioning-profile-concurrent-create-idempotent-client-token`)
//! are single rows of the tables below. The rows differ along what the rule generalises over:
//! the `ProvisioningType` (the model's whole value set), whether the request names a `Name`
//! and at which `@length` bound, whether it supplies a `CaCertificate` and a
//! `ClaimCertificate` of its own, how many `Tags` it carries, how many callers race for one
//! `ClientToken`, how many records are stored and at which page size they are listed, and the
//! region the request is signed for.
//!
//! Every case drives the real restJson1 request path (`MultiServiceRouter::dispatch`), so the
//! routes, statuses, bodies and modelled errors asserted here are the ones a client sees.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use base64::Engine as _;
use serde_json::{json, Value};
use service_com_amazonaws_iotmanagedintegrations_iotmanagedintegrations::ops::types::PROVISIONING_TYPE_VALUES;
use service_com_amazonaws_iotmanagedintegrations_iotmanagedintegrations::state::{
    PROVISIONING_PROFILE_ABSENT_REASON, PROVISIONING_PROFILE_PAGE_LIMIT,
    PROVISIONING_PROFILE_RESOURCE_TYPE, PROVISIONING_PROFILE_STATUS_CREATED,
};
use service_com_amazonaws_iotmanagedintegrations_iotmanagedintegrations::{
    ComAmazonawsIotmanagedintegrationsIotmanagedintegrationsState as ServiceState, EmulatedService,
};
use smithy_server_runtime::MultiServiceRouter;

/// The region the test's requests are signed for unless a case names another.
const REGION: &str = "us-east-1";

/// The account id every generated emulator presents (`engine::EMULATOR_ACCOUNT_ID`).
const ACCOUNT: &str = engine::EMULATOR_ACCOUNT_ID;

/// The collection route `CreateProvisioningProfile` is bound to
/// (`smithy_query(query="operation",operation="CreateProvisioningProfile")` -> POST
/// `/provisioning-profiles`).
const COLLECTION: &str = "/provisioning-profiles";

/// The wire error codes the model's `@httpError` shapes are rendered under.
const NOT_FOUND: &str = "ResourceNotFoundException";
const VALIDATION: &str = "ValidationException";

/// The `@length` bounds of `ProvisioningProfileId`, in characters
/// (`smithy_query(query="shape",shape="ProvisioningProfileId")` -> length 1..64, pattern
/// `^[A-Za-z0-9-_]+$`).
const PROFILE_ID_BOUNDS: (usize, usize) = (1, 64);

/// The `@length` bounds of `ProvisioningProfileName`
/// (`smithy_query(query="shape",shape="ProvisioningProfileName")` -> length 1..36, pattern
/// `^[0-9A-Za-z_-]+$`).
const PROFILE_NAME_BOUNDS: (usize, usize) = (1, 36);

/// The `@length` maximum of `TagsMap` (`smithy_query(query="shape",shape="TagsMap")` ->
/// length 1..50), the most tags one create may carry.
const MAX_TAGS: usize = 50;

/// The PEM labels the certificate and the private key are framed with.
const CERTIFICATE_LABEL: &str = "CERTIFICATE";
const PRIVATE_KEY_LABEL: &str = "PRIVATE KEY";

/// The `@range` bounds of `MaxResults`
/// (`smithy_query(query="operation",operation="ListProvisioningProfiles")` -> `MaxResults`
/// range 1..1000).
const MAX_RESULTS_BOUNDS: (i64, i64) = (1, 1000);

/// One HTTP answer, reduced to what the assertions read.
struct Reply {
    status: u16,
    /// The restJson1 error code, from `x-amzn-errortype`.
    error_type: Option<String>,
    body: Value,
}

impl Reply {
    fn member(&self, name: &str) -> Option<&Value> {
        self.body.get(name).filter(|value| !value.is_null())
    }

    fn string(&self, name: &str) -> Option<&str> {
        self.member(name).and_then(Value::as_str)
    }

    /// The reason an error body carries, on the member the model declares for it.
    fn message(&self) -> &str {
        self.string("Message")
            .or_else(|| self.string("message"))
            .unwrap_or_default()
    }

    /// The tags an answer reports, on either casing of the member.
    fn tags(&self) -> Option<BTreeMap<String, String>> {
        let value = self.member("Tags").or_else(|| self.member("tags"))?;
        let pairs = value.as_object()?;
        Some(
            pairs
                .iter()
                .map(|(key, value)| (key.clone(), value.as_str().unwrap_or_default().to_string()))
                .collect(),
        )
    }

    /// The `Items` of a list answer.
    fn items(&self) -> Vec<Value> {
        self.member("Items")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
    }

    /// The `Id` of every item a list reported, in the order it reported them.
    fn item_ids(&self) -> Vec<String> {
        self.items()
            .iter()
            .map(|item| {
                item.get("Id")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string()
            })
            .collect()
    }
}

/// A router holding one `iotmanagedintegrations` service over a fresh shared engine: the same
/// assembly `core/router` performs, with one service mounted.
fn emulator() -> MultiServiceRouter {
    emulator_over(Arc::new(engine::InMemoryEngine::new()))
}

/// [`emulator`] over an engine the caller keeps a handle to, so a test can inspect the
/// service's store directly.
fn emulator_over(engine: Arc<engine::InMemoryEngine>) -> MultiServiceRouter {
    let mut router = MultiServiceRouter::new();
    // The signature itself is not under test here, so requests are unauthenticated.
    router.set_request_authentication(false);
    router.register_service(EmulatedService::new(engine));
    router
}

/// Send one request, signed for `region`, and reduce the answer. The `Authorization`
/// credential scope is what routes the request to this service and names its region.
async fn call_in(
    router: &MultiServiceRouter,
    region: &str,
    method: &str,
    path_and_query: &str,
    body: Option<Value>,
) -> Reply {
    let payload = body.map_or_else(bytes::Bytes::new, |value| {
        bytes::Bytes::from(serde_json::to_vec(&value).expect("serialize the request body"))
    });
    let request = http::Request::builder()
        .method(method)
        .uri(format!(
            "https://iotmanagedintegrations.{region}.amazonaws.com{path_and_query}"
        ))
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDTESTTESTTESTTEST/20260101/{region}/\
                 iotmanagedintegrations/aws4_request, SignedHeaders=host, Signature=0"
            ),
        )
        .header("content-type", "application/json")
        .body(payload)
        .expect("a well-formed request");

    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let error_type = response
        .headers()
        .get("x-amzn-errortype")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    let bytes = response.into_body();
    let body = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    Reply {
        status,
        error_type,
        body,
    }
}

/// `CreateProvisioningProfile` with `body`, signed for `region`.
async fn create_in(router: &MultiServiceRouter, region: &str, body: Value) -> Reply {
    call_in(router, region, "POST", COLLECTION, Some(body)).await
}

/// [`create_in`] in [`REGION`].
async fn create(router: &MultiServiceRouter, body: Value) -> Reply {
    create_in(router, REGION, body).await
}

/// `GetProvisioningProfile` for `id`, signed for `region`.
async fn get_in(router: &MultiServiceRouter, region: &str, id: &str) -> Reply {
    call_in(router, region, "GET", &format!("{COLLECTION}/{id}"), None).await
}

/// [`get_in`] in [`REGION`].
async fn get(router: &MultiServiceRouter, id: &str) -> Reply {
    get_in(router, REGION, id).await
}

/// `ListProvisioningProfiles` with the raw `query` (empty for none), signed for `region`.
async fn list_in(router: &MultiServiceRouter, region: &str, query: &str) -> Reply {
    call_in(router, region, "GET", &format!("{COLLECTION}{query}"), None).await
}

/// [`list_in`] in [`REGION`].
async fn list(router: &MultiServiceRouter, query: &str) -> Reply {
    list_in(router, REGION, query).await
}

/// `DeleteProvisioningProfile` for `id`, signed for `region`.
async fn delete_in(router: &MultiServiceRouter, region: &str, id: &str) -> Reply {
    call_in(router, region, "DELETE", &format!("{COLLECTION}/{id}"), None).await
}

/// `ListTagsForResource`: GET `/tags/{ResourceArn}`. `ResourceArn` is a NON-greedy
/// `@httpLabel`, so the ARN fills ONE path segment and every byte outside the unreserved set
/// is percent-encoded, which is what an SDK puts on the wire.
async fn list_tags_in(router: &MultiServiceRouter, region: &str, arn: &str) -> Reply {
    let label: String = arn
        .bytes()
        .map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                (byte as char).to_string()
            }
            other => format!("%{other:02X}"),
        })
        .collect();
    call_in(router, region, "GET", &format!("/tags/{label}"), None).await
}

/// [`list_tags_in`] in [`REGION`].
async fn list_tags(router: &MultiServiceRouter, arn: &str) -> Reply {
    list_tags_in(router, REGION, arn).await
}

/// The DER bytes a PEM block of `label` carries, or `None` when `pem` carries no such block
/// or its body is not Base64.
fn pem_der(pem: &str, label: &str) -> Option<Vec<u8>> {
    let begin = format!("-----BEGIN {label}-----");
    let end = format!("-----END {label}-----");
    let mut body: String = pem
        .lines()
        .skip_while(|line| line.trim() != begin)
        .skip(1)
        .take_while(|line| line.trim() != end)
        .collect();
    body.retain(|character| !character.is_whitespace());
    if body.is_empty() {
        return None;
    }
    base64::engine::general_purpose::STANDARD.decode(body).ok()
}

/// Assert `pem` is a PEM block of `label` a client can really decode: the framing, a Base64
/// body, and DER whose outermost value is a SEQUENCE — which is what both an X.509
/// certificate and a PKCS#8 private key begin with.
fn assert_decodable_pem(case: &str, what: &str, pem: Option<&str>, label: &str) {
    let pem = pem.unwrap_or_else(|| panic!("case {case:?}: expected {what} in the answer"));
    let der = pem_der(pem, label).unwrap_or_else(|| {
        panic!("case {case:?}: expected {what} as a decodable {label} PEM block, got {pem:?}")
    });
    assert!(
        der.first() == Some(&0x30) && der.len() > 64,
        "case {case:?}: expected {what} to decode to DER starting with a SEQUENCE, got {} \
         bytes starting {:?}",
        der.len(),
        der.first()
    );
}

/// Assert an `Arn` is the provisioning-profile ARN of `id`: the modelled
/// `ProvisioningProfileArn` pattern, with the resource segment naming this id.
fn assert_arn(case: &str, arn: Option<&str>, region: &str, id: &str) {
    assert_eq!(
        arn,
        Some(
            format!("arn:aws:iotmanagedintegrations:{region}:{ACCOUNT}:provisioning-profile/{id}")
                .as_str()
        ),
        "case {case:?}: expected the provisioning-profile ARN of {id:?} in {region}, got {arn:?}"
    );
}

/// Assert one create was accepted and answer the `Id` it minted, checking the id's own form.
fn accepted_id(case: &str, reply: &Reply) -> String {
    assert_eq!(
        reply.status, 201,
        "case {case:?}: expected HTTP 201, got {} with body {:?}",
        reply.status, reply.body
    );
    let id = reply.string("Id").unwrap_or_default().to_string();
    let (min, max) = PROFILE_ID_BOUNDS;
    assert!(
        (min..=max).contains(&id.chars().count())
            && id
                .chars()
                .all(|character| character.is_ascii_alphanumeric()
                    || character == '-'
                    || character == '_'),
        "case {case:?}: expected a ProvisioningProfileId of {min}..={max} characters from the \
         modelled pattern, got {id:?} in body {:?}",
        reply.body
    );
    id
}

/// Assert one answer is the modelled refusal `status`/`code` carrying a reason.
fn assert_refused(case: &str, reply: &Reply, status: u16, code: &str) {
    assert_eq!(
        reply.status, status,
        "case {case:?}: expected HTTP {status}, got {} with body {:?}",
        reply.status, reply.body
    );
    assert_eq!(
        reply.error_type.as_deref(),
        Some(code),
        "case {case:?}: expected the modelled {code} code, got {:?}",
        reply.error_type
    );
    assert!(
        !reply.message().is_empty(),
        "case {case:?}: expected the modelled Message member to carry a reason, got body {:?}",
        reply.body
    );
}

/// A real PEM certificate to hand BACK to the service as a caller-supplied one: the claim
/// certificate a minted profile of `router` carries. Taking it from the service's own output
/// keeps the supplied-certificate rows driven by material a client could really hold, rather
/// than by text in the shape of a certificate.
async fn a_real_certificate(router: &MultiServiceRouter) -> String {
    let minted = create(router, json!({"ProvisioningType": "FLEET_PROVISIONING"})).await;
    accepted_id("certificate source", &minted);
    minted
        .string("ClaimCertificate")
        .unwrap_or_else(|| {
            panic!(
                "case \"certificate source\": expected a minted ClaimCertificate, got body {:?}",
                minted.body
            )
        })
        .to_string()
}

/// One row of the create table: a request form, and what the rule says about its answer.
struct Case {
    name: &'static str,
    /// `ProvisioningType`, the one `@required` member.
    provisioning_type: &'static str,
    /// The `Name` the request carries, if any.
    profile_name: Option<String>,
    /// Whether the request supplies its own `ClaimCertificate` (else one is generated).
    supplies_claim_certificate: bool,
    /// Whether the request supplies a `CaCertificate`.
    supplies_ca_certificate: bool,
    /// The `Tags` the request carries.
    tags: BTreeMap<String, String>,
}

/// The rows the rule is asserted over: every `ProvisioningType` the model declares, a `Name`
/// absent and at both `@length` bounds, a supplied and a generated claim certificate, a
/// supplied and an absent CA certificate, and no tags, one tag and the `TagsMap` maximum.
fn cases() -> Vec<Case> {
    let (min_name, max_name) = PROFILE_NAME_BOUNDS;
    let mut rows = vec![
        Case {
            name: "fleet provisioning, generated claim certificate, no name",
            provisioning_type: PROVISIONING_TYPE_VALUES[0],
            profile_name: None,
            supplies_claim_certificate: false,
            supplies_ca_certificate: false,
            tags: BTreeMap::new(),
        },
        Case {
            name: "JITR with a CA certificate and the shortest name",
            provisioning_type: PROVISIONING_TYPE_VALUES[1],
            profile_name: Some("a".repeat(min_name)),
            supplies_claim_certificate: false,
            supplies_ca_certificate: true,
            tags: BTreeMap::from([("owner".to_string(), "fleet-team".to_string())]),
        },
        Case {
            name: "supplied claim certificate, longest name, the tag maximum",
            provisioning_type: PROVISIONING_TYPE_VALUES[0],
            profile_name: Some(format!("{}{}", "profile_", "N".repeat(max_name - 8))),
            supplies_claim_certificate: true,
            supplies_ca_certificate: true,
            tags: (0..MAX_TAGS)
                .map(|n| (format!("key-{n}"), format!("value-{n}")))
                .collect(),
        },
    ];
    // Every declared `ProvisioningType` is covered by a row of its own, whatever the model
    // declares, so a value added to the enum is exercised rather than silently skipped.
    for provisioning_type in PROVISIONING_TYPE_VALUES {
        rows.push(Case {
            name: "one row per declared ProvisioningType",
            provisioning_type,
            profile_name: Some(format!("type-{provisioning_type}").replace('_', "-")),
            supplies_claim_certificate: false,
            supplies_ca_certificate: false,
            tags: BTreeMap::new(),
        });
    }
    rows
}

/// The request body of one row.
fn request_of(case: &Case, certificate: &str) -> Value {
    let mut body = json!({"ProvisioningType": case.provisioning_type});
    let object = body.as_object_mut().expect("a JSON object");
    if let Some(profile_name) = &case.profile_name {
        object.insert("Name".to_string(), json!(profile_name));
    }
    if case.supplies_claim_certificate {
        object.insert("ClaimCertificate".to_string(), json!(certificate));
    }
    if case.supplies_ca_certificate {
        object.insert("CaCertificate".to_string(), json!(certificate));
    }
    if !case.tags.is_empty() {
        object.insert("Tags".to_string(), json!(case.tags));
    }
    body
}

/// RULE: a create mints the profile its request describes, answers every member the response
/// shape declares from it, and a read of that id reports the SAME stored record — including
/// the claim certificate, and never the private key.
#[tokio::test]
async fn a_create_persists_the_profile_a_read_reports() {
    let router = emulator();
    let certificate = a_real_certificate(&router).await;

    let mut minted_ids: BTreeSet<String> = BTreeSet::new();
    for case in cases() {
        let label = format!("{} [{}]", case.name, case.provisioning_type);
        let created = create(&router, request_of(&case, &certificate)).await;
        let id = accepted_id(&label, &created);
        assert!(
            minted_ids.insert(id.clone()),
            "case {label:?}: expected a fresh ProvisioningProfileId, got {id:?} again"
        );
        assert_arn(&label, created.string("Arn"), REGION, &id);
        assert_eq!(
            created.string("Name"),
            case.profile_name.as_deref(),
            "case {label:?}: expected the Name the request carried, got {:?}",
            created.string("Name")
        );
        assert_eq!(
            created.string("ProvisioningType"),
            Some(case.provisioning_type),
            "case {label:?}: expected the ProvisioningType the request carried, got {:?}",
            created.string("ProvisioningType")
        );
        // The create answers the terminal status: the profile is created inside the call, so
        // nothing about it is still pending when the caller is answered.
        assert_eq!(
            created.string("Status"),
            Some(PROVISIONING_PROFILE_STATUS_CREATED),
            "case {label:?}: expected the created status, got {:?}",
            created.string("Status")
        );

        if case.supplies_claim_certificate {
            assert_eq!(
                created.string("ClaimCertificate"),
                Some(certificate.as_str()),
                "case {label:?}: expected the supplied claim certificate to be used as it \
                 stands, got {:?}",
                created.string("ClaimCertificate")
            );
            // The caller kept the private half of a certificate it supplied, so there is
            // none for the service to report.
            assert!(
                created.member("ClaimCertificatePrivateKey").is_none(),
                "case {label:?}: expected no private key for a supplied claim certificate, \
                 got body {:?}",
                created.body
            );
        } else {
            assert_decodable_pem(
                &label,
                "the generated claim certificate",
                created.string("ClaimCertificate"),
                CERTIFICATE_LABEL,
            );
            assert_decodable_pem(
                &label,
                "the generated claim certificate's private key",
                created.string("ClaimCertificatePrivateKey"),
                PRIVATE_KEY_LABEL,
            );
            assert_ne!(
                created.string("ClaimCertificate"),
                Some(certificate.as_str()),
                "case {label:?}: expected a claim certificate minted for THIS profile, got \
                 another profile's"
            );
        }

        let read = get(&router, &id).await;
        assert_eq!(
            read.status, 200,
            "case {label:?}: expected HTTP 200 from GetProvisioningProfile, got {} with body \
             {:?}",
            read.status, read.body
        );
        for member in [
            "Id",
            "Arn",
            "Name",
            "ProvisioningType",
            "Status",
            "ClaimCertificate",
        ] {
            assert_eq!(
                read.string(member),
                created.string(member),
                "case {label:?}: expected the read to report the created {member}, got {:?} \
                 for {:?}",
                read.string(member),
                created.string(member)
            );
        }
        assert!(
            read.member("ClaimCertificatePrivateKey").is_none(),
            "case {label:?}: no read shape declares the private key, got body {:?}",
            read.body
        );
        assert_eq!(
            read.tags().unwrap_or_default(),
            case.tags,
            "case {label:?}: expected the read to report the Tags the create carried, got {:?}",
            read.tags()
        );
        // The ARN the create minted resolves through the tagging surface, which is what makes
        // the profile a taggable resource of this service rather than an id with an ARN-shaped
        // string beside it.
        let tagged = list_tags(&router, created.string("Arn").unwrap_or_default()).await;
        assert_eq!(
            tagged.status, 200,
            "case {label:?}: expected HTTP 200 from ListTagsForResource, got {} with body {:?}",
            tagged.status, tagged.body
        );
        assert_eq!(
            tagged.tags().unwrap_or_default(),
            case.tags,
            "case {label:?}: expected the tagging surface to report the same Tags, got {:?}",
            tagged.tags()
        );
    }
}

/// RULE: a `ClientToken` keys at most one profile. A create presenting a token an earlier
/// create presented answers that first profile — the same id and the same certificate
/// material, down to the private key — and writes nothing; two different tokens are two
/// profiles.
#[tokio::test]
async fn one_client_token_mints_at_most_one_profile() {
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = emulator_over(Arc::clone(&engine));

    // The rows differ along the member set the retry repeats, so the replay is asserted for a
    // bare request and for one carrying every optional member.
    let rows = [
        ("bare retry", json!({"ProvisioningType": "JITR"})),
        (
            "retry of a fully-specified create",
            json!({
                "ProvisioningType": "FLEET_PROVISIONING",
                "Name": "retried-profile",
                "Tags": {"owner": "fleet-team"},
            }),
        ),
    ];
    let mut ids: BTreeSet<String> = BTreeSet::new();
    for (case, mut body) in rows {
        let token = format!("token-{}", case.replace(' ', "-"));
        body.as_object_mut()
            .expect("a JSON object")
            .insert("ClientToken".to_string(), json!(token));

        let first = create(&router, body.clone()).await;
        let id = accepted_id(case, &first);
        let records_after_first = engine.state::<ServiceState>().lock().provisioning_profiles.len();

        let retried = create(&router, body.clone()).await;
        assert_eq!(
            accepted_id(case, &retried),
            id,
            "case {case:?}: expected the retry to answer the first call's id"
        );
        for member in [
            "Arn",
            "Name",
            "ProvisioningType",
            "ClaimCertificate",
            "ClaimCertificatePrivateKey",
        ] {
            assert_eq!(
                retried.string(member),
                first.string(member),
                "case {case:?}: expected the retry to answer the first call's {member}, got \
                 {:?} for {:?}",
                retried.string(member),
                first.string(member)
            );
        }
        assert_eq!(
            engine.state::<ServiceState>().lock().provisioning_profiles.len(),
            records_after_first,
            "case {case:?}: expected the retry to write no second record"
        );
        assert!(
            ids.insert(id),
            "case {case:?}: expected a different token to mint a different profile"
        );
    }

    assert_eq!(
        engine.state::<ServiceState>().lock().provisioning_profiles.len(),
        ids.len(),
        "case \"two tokens\": expected one stored profile per distinct ClientToken, got the \
         store holding {} for {} tokens",
        engine.state::<ServiceState>().lock().provisioning_profiles.len(),
        ids.len()
    );
}

/// RULE: the same holds under CONCURRENCY — N callers presenting one `ClientToken` are all
/// answered the SAME profile, and the store holds exactly one.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_creates_sharing_one_client_token_agree_on_one_profile() {
    // The rows differ in how many callers race for the one token.
    for racers in [2usize, 3, 5] {
        let case = format!("{racers} racing callers");
        let engine = Arc::new(engine::InMemoryEngine::new());
        let router = emulator_over(Arc::clone(&engine));
        let body = json!({
            "ProvisioningType": "FLEET_PROVISIONING",
            "Name": "raced-profile",
            "ClientToken": format!("raced-token-{racers}"),
        });

        let mut racing = Vec::with_capacity(racers);
        for _ in 0..racers {
            let router = router.clone();
            let body = body.clone();
            racing.push(tokio::spawn(
                async move { create(&router, body).await },
            ));
        }
        let mut answered: Vec<Reply> = Vec::with_capacity(racers);
        for handle in racing {
            answered.push(handle.await.expect("a racing call does not panic"));
        }

        let winners: BTreeSet<String> = answered
            .iter()
            .filter(|reply| reply.status == 201)
            .map(|reply| reply.string("Id").unwrap_or_default().to_string())
            .collect();
        assert_eq!(
            winners.len(),
            1,
            "case {case:?}: expected one winning Id, got {:?}",
            answered
                .iter()
                .map(|reply| (reply.status, reply.string("Id").map(str::to_string)))
                .collect::<Vec<_>>()
        );
        for reply in &answered {
            assert_eq!(
                reply.status, 201,
                "case {case:?}: every caller presenting the token is served the profile, got \
                 {} with body {:?}",
                reply.status, reply.body
            );
        }
        let stored = engine.state::<ServiceState>().lock().provisioning_profiles.len();
        assert_eq!(
            stored, 1,
            "case {case:?}: expected exactly one stored profile for one token, got {stored}"
        );
        // The winning id is a profile a client can read back.
        let id = winners.iter().next().expect("one winner").clone();
        let read = get(&router, &id).await;
        assert_eq!(
            read.status, 200,
            "case {case:?}: expected the raced profile to be readable, got {} with body {:?}",
            read.status, read.body
        );
    }
}

/// RULE: a profile is created inside its create call and stays so: every later read reports
/// the same terminal status and the same material, so reading a profile changes nothing a
/// later caller sees.
#[tokio::test]
async fn a_created_profile_stays_created_however_often_it_is_read() {
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = emulator_over(Arc::clone(&engine));
    for provisioning_type in PROVISIONING_TYPE_VALUES {
        let case = format!("repeated reads [{provisioning_type}]");
        let created = create(&router, json!({"ProvisioningType": provisioning_type})).await;
        let id = accepted_id(&case, &created);
        assert_eq!(
            created.string("Status"),
            Some(PROVISIONING_PROFILE_STATUS_CREATED),
            "case {case:?}: expected the create to answer the created status, got {:?}",
            created.string("Status")
        );
        let stored = engine
            .state::<ServiceState>()
            .lock()
            .provisioning_profiles
            .len();

        // More than one read, so a status or a value that moved because it was read would
        // show up.
        for repeat in 0..3usize {
            let again = get(&router, &id).await;
            assert_eq!(
                again.string("Status"),
                Some(PROVISIONING_PROFILE_STATUS_CREATED),
                "case {case:?}: repeat {repeat} expected the profile to stay created, got {:?}",
                again.string("Status")
            );
            assert_eq!(
                again.string("ClaimCertificate"),
                created.string("ClaimCertificate"),
                "case {case:?}: repeat {repeat} expected the same claim certificate on every \
                 read"
            );
            assert_eq!(
                again.string("Arn"),
                created.string("Arn"),
                "case {case:?}: repeat {repeat} expected the ARN not to move"
            );
            assert_eq!(
                engine
                    .state::<ServiceState>()
                    .lock()
                    .provisioning_profiles
                    .len(),
                stored,
                "case {case:?}: repeat {repeat} expected a read to write nothing"
            );
        }
    }
}

/// RULE: the list reports the profiles the caller's region holds, in the order they were
/// created, projecting each STORED record onto a `ProvisioningProfileSummary`; `MaxResults`
/// cuts the page at that many items and the `NextToken` it answers with resumes exactly where
/// the page stopped, so the token chain reports every record exactly once and the last page
/// carries no token.
#[tokio::test]
async fn the_list_pages_over_what_the_creates_stored() {
    let router = emulator();

    // Seven records, so the page sizes below divide the store evenly and unevenly, each
    // carrying a different member set.
    let mut inserted: Vec<String> = Vec::new();
    for n in 0..7usize {
        let case = format!("seed-{n}");
        let mut body = json!({
            "ProvisioningType": PROVISIONING_TYPE_VALUES[n % PROVISIONING_TYPE_VALUES.len()],
        });
        if n % 2 == 0 {
            body.as_object_mut()
                .expect("a JSON object")
                .insert("Name".to_string(), json!(format!("profile-{n}")));
        }
        let created = create(&router, body).await;
        inserted.push(accepted_id(&case, &created));
    }
    // A profile of another region is none of this region's business, however it is listed.
    let elsewhere = create_in(&router, "eu-west-1", json!({"ProvisioningType": "JITR"})).await;
    let elsewhere_id = accepted_id("another region", &elsewhere);

    let unpaged = list(&router, "").await;
    assert_eq!(
        unpaged.status, 200,
        "case \"no MaxResults\": expected HTTP 200, got {} with body {:?}",
        unpaged.status, unpaged.body
    );
    assert_eq!(
        unpaged.item_ids(),
        inserted,
        "case \"no MaxResults\": expected every record of this region in creation order"
    );
    assert!(
        unpaged.member("NextToken").is_none(),
        "case \"no MaxResults\": a page holding the whole store carries no token, got {:?}",
        unpaged.member("NextToken")
    );
    assert!(
        !unpaged.item_ids().contains(&elsewhere_id),
        "case \"no MaxResults\": another region's profile must not be listed here, got {:?}",
        unpaged.item_ids()
    );

    // Each summary reports what the record holds, as the get reports it.
    for item in unpaged.items() {
        let id = item.get("Id").and_then(Value::as_str).unwrap_or_default();
        let read = get(&router, id).await;
        for member in ["Id", "Arn", "Name", "ProvisioningType", "Status"] {
            assert_eq!(
                item.get(member).and_then(Value::as_str),
                read.string(member),
                "case {id:?}: expected the summary's {member} to be the record's, got {:?} for \
                 {:?}",
                item.get(member),
                read.string(member)
            );
        }
        assert!(
            item.get("ClaimCertificate").is_none()
                && item.get("ClaimCertificatePrivateKey").is_none(),
            "case {id:?}: a summary declares no certificate material, got {item:?}"
        );
    }

    // The rows differ along the page size: both modelled `MaxResults` bounds, sizes that
    // divide the store and sizes that do not.
    let (min_results, max_results) = MAX_RESULTS_BOUNDS;
    for page_size in [min_results, 2, 3, 7, 8, max_results] {
        let case = format!("MaxResults {page_size}");
        let mut seen: Vec<String> = Vec::new();
        let mut token: Option<String> = None;
        let mut pages = 0usize;
        loop {
            let query = match &token {
                Some(next) => format!("?MaxResults={page_size}&NextToken={next}"),
                None => format!("?MaxResults={page_size}"),
            };
            let page = list(&router, &query).await;
            assert_eq!(
                page.status, 200,
                "case {case:?}: page {pages} expected HTTP 200, got {} with body {:?}",
                page.status, page.body
            );
            let ids = page.item_ids();
            let expected_page = usize::try_from(page_size)
                .unwrap_or(1)
                .min(PROVISIONING_PROFILE_PAGE_LIMIT)
                .min(inserted.len() - seen.len());
            assert_eq!(
                ids.len(),
                expected_page,
                "case {case:?}: page {pages} expected {expected_page} items, got {ids:?}"
            );
            seen.extend(ids);
            token = page.string("NextToken").map(str::to_string);
            pages += 1;
            if token.is_none() {
                break;
            }
            assert!(
                pages <= inserted.len(),
                "case {case:?}: the token chain must end within one page per record, it was \
                 still going after {pages} pages reporting {seen:?}"
            );
        }
        assert_eq!(
            seen, inserted,
            "case {case:?}: expected the token chain to report every record exactly once, in \
             creation order"
        );
    }

    // A cursor this service never issued is a client error, not an empty page: a caller
    // cannot page a store it was never given a cursor into. Every value matches the modelled
    // `NextToken` pattern, so the boundary validator passes it to the handler.
    for (case, token) in [
        ("not a cursor", "notacursor"),
        ("a short cursor", "00ff"),
        ("the right length but not hex", "zzzzzzzzzzzzzzzz"),
        ("a cursor beyond the store", "ffffffffffffffff"),
    ] {
        let refused = list(&router, &format!("?NextToken={token}")).await;
        assert_refused(case, &refused, 400, VALIDATION);
    }
}

/// RULE: a delete removes the profile INSIDE the call it was asked in. The call is accepted,
/// and from the moment it answers no operation resolves that identifier any more: the VERY
/// NEXT get and the very next delete answer this resource's modelled
/// `ResourceNotFoundException` (404, the `x-amzn-errortype` discriminator, the one fixed
/// reason this resource states its absence with, the identifier on `ResourceId` and the kind
/// on `ResourceType`), the list omits it, the ARN it was minted under resolves no tags, the
/// record itself is out of the store and the `ClientToken` that minted it replays into
/// nothing. There is no window in which a removed profile still reads back, and the profiles
/// the call did not name are untouched.
#[tokio::test]
async fn a_delete_removes_the_profile_from_every_reader_at_once() {
    // The rows differ along the member set the profile was created with, along which of
    // several stored profiles is deleted — the head, the middle and the tail, rather than only
    // a store of one — and along the region the whole exchange is signed for.
    let bodies = [
        json!({"ProvisioningType": "FLEET_PROVISIONING", "ClientToken": "deleted-token-0"}),
        json!({
            "ProvisioningType": "JITR",
            "Name": "deleted-profile",
            "Tags": {"owner": "fleet-team"},
            "ClientToken": "deleted-token-1",
        }),
        json!({"ProvisioningType": "JITR", "ClientToken": "deleted-token-2"}),
    ];
    for (region, victim) in [
        (REGION, 0usize),
        (REGION, 1),
        (REGION, 2),
        ("eu-west-1", 1),
        ("ap-southeast-2", 2),
    ] {
        let case = format!("delete index {victim} in {region}");
        let engine = Arc::new(engine::InMemoryEngine::new());
        let router = emulator_over(Arc::clone(&engine));
        let mut ids: Vec<String> = Vec::new();
        let mut arns: Vec<String> = Vec::new();
        for (n, body) in bodies.iter().enumerate() {
            let created = create_in(&router, region, body.clone()).await;
            ids.push(accepted_id(&format!("{case} seed-{n}"), &created));
            arns.push(created.string("Arn").unwrap_or_default().to_string());
        }

        let removed = delete_in(&router, region, &ids[victim]).await;
        assert_eq!(
            removed.status, 200,
            "case {case:?}: expected HTTP 200 from DeleteProvisioningProfile, got {} with body \
             {:?}",
            removed.status, removed.body
        );

        // The request right after the delete already resolves nothing: nothing is slept on and
        // no span is owed.
        let read = get_in(&router, region, &ids[victim]).await;
        assert_refused(&case, &read, 404, NOT_FOUND);
        assert_eq!(
            read.message(),
            PROVISIONING_PROFILE_ABSENT_REASON,
            "case {case:?}: expected this resource's fixed not-found reason, got {:?}",
            read.message()
        );
        assert_eq!(
            read.string("ResourceId"),
            Some(ids[victim].as_str()),
            "case {case:?}: expected the 404 to report the identifier it was given, got {:?}",
            read.string("ResourceId")
        );
        assert_eq!(
            read.string("ResourceType"),
            Some(PROVISIONING_PROFILE_RESOURCE_TYPE),
            "case {case:?}: expected the 404 to report this resource's type, got {:?}",
            read.string("ResourceType")
        );

        // Repeating the delete finds nothing to remove: the same 404, worded the same way.
        let repeated = delete_in(&router, region, &ids[victim]).await;
        assert_refused(&case, &repeated, 404, NOT_FOUND);
        assert_eq!(
            repeated.message(),
            PROVISIONING_PROFILE_ABSENT_REASON,
            "case {case:?}: expected the repeated delete to state the same reason, got {:?}",
            repeated.message()
        );

        // The list reports the survivors and nothing else, and the ARN the removed profile was
        // minted under resolves no tags any more.
        let listed = list_in(&router, region, "").await;
        assert!(
            !listed.item_ids().contains(&ids[victim]),
            "case {case:?}: expected the removed profile to be out of the list, got {:?}",
            listed.item_ids()
        );
        assert_eq!(
            listed.item_ids().len(),
            ids.len() - 1,
            "case {case:?}: expected only the surviving profiles to be listed, got {:?}",
            listed.item_ids()
        );
        let tagged = list_tags_in(&router, region, &arns[victim]).await;
        assert_eq!(
            tagged.status, 404,
            "case {case:?}: expected the ARN of a removed profile to resolve nothing, got {} \
             with body {:?}",
            tagged.status, tagged.body
        );

        // The record itself is out of the store, not merely hidden from the readers.
        assert!(
            !engine
                .state::<ServiceState>()
                .lock()
                .provisioning_profiles
                .contains_key(&ids[victim]),
            "case {case:?}: expected the store to hold no record for a removed profile"
        );

        // The token of the removed profile replays into nothing, so presenting it again mints
        // a NEW profile rather than answering one nothing can provision with.
        let retried = create_in(&router, region, bodies[victim].clone()).await;
        let reborn = accepted_id(&format!("{case} token after delete"), &retried);
        assert_ne!(
            reborn, ids[victim],
            "case {case:?}: expected the token to mint a new profile once its own was removed"
        );

        // Every profile the delete did not name is untouched by it.
        for (n, id) in ids.iter().enumerate() {
            if n == victim {
                continue;
            }
            let survivor = get_in(&router, region, id).await;
            assert_eq!(
                survivor.status, 200,
                "case {case:?}: expected the profile at index {n} to survive the delete, got {} \
                 with body {:?}",
                survivor.status, survivor.body
            );
            assert_eq!(
                survivor.string("Status"),
                Some(PROVISIONING_PROFILE_STATUS_CREATED),
                "case {case:?}: expected the surviving profile at index {n} to stay created, \
                 got {:?}",
                survivor.string("Status")
            );
        }
    }
}

/// RULE: an identifier the caller's region holds no profile under is the modelled 404,
/// reporting the identifier it was given and this resource's type — whether no profile of
/// that id was ever created or the one it names belongs to another region.
#[tokio::test]
async fn an_identifier_this_region_holds_no_profile_under_is_refused() {
    let router = emulator();
    let elsewhere = create_in(
        &router,
        "eu-west-1",
        json!({"ProvisioningType": "JITR", "Name": "other-region"}),
    )
    .await;
    let elsewhere_id = accepted_id("another region's profile", &elsewhere);
    assert_arn(
        "another region's profile",
        elsewhere.string("Arn"),
        "eu-west-1",
        &elsewhere_id,
    );

    // The rows differ along WHY this region holds no such profile: nothing ever created one,
    // the identifier is of a different form, and the profile exists in another region.
    for (case, identifier) in [
        ("never created", "AbsentProfile".to_string()),
        ("an identifier of another form", "a-b_C-9".to_string()),
        ("created in another region", elsewhere_id.clone()),
    ] {
        let read = get(&router, &identifier).await;
        assert_refused(case, &read, 404, NOT_FOUND);
        // The reason is FIXED text: it is the same sentence whatever identifier was asked
        // for, and the identifier is reported on `ResourceId` rather than inside it.
        assert_eq!(
            read.message(),
            PROVISIONING_PROFILE_ABSENT_REASON,
            "case {case:?}: expected this resource's fixed not-found reason, got {:?}",
            read.message()
        );
        assert_eq!(
            read.string("ResourceId"),
            Some(identifier.as_str()),
            "case {case:?}: expected the 404 to report the identifier it was given, got {:?}",
            read.string("ResourceId")
        );
        assert_eq!(
            read.string("ResourceType"),
            Some(PROVISIONING_PROFILE_RESOURCE_TYPE),
            "case {case:?}: expected the 404 to report this resource's type, got {:?}",
            read.string("ResourceType")
        );
    }

    // The profile of the other region is still there for the region that created it: a 404
    // here is about scope, not about the record.
    let read = get_in(&router, "eu-west-1", &elsewhere_id).await;
    assert_eq!(
        read.status, 200,
        "case \"created in another region\": expected its own region to still read it, got {} \
         with body {:?}",
        read.status, read.body
    );
}
