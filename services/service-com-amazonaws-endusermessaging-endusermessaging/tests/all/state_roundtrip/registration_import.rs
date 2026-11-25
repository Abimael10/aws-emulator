// SPDX-License-Identifier: CC0-1.0

//! `UpdateBrandProfileFromRegistration`: the asynchronous import of a registration's
//! fields into a brand profile's attributes, and `ListRegistrationsFromBrandProfile`, the
//! reader of the registrations a profile is associated with — both end to end over the
//! wire.
//!
//! Every call goes through `MultiServiceRouter::dispatch`, so it exercises the same
//! restJson1 path a real SDK client takes: routing, deserialization, boundary constraint
//! validation, the handler, and error rendering. The registrations themselves are written
//! into the service's own store the way `CreateRegistrationsFromBrandProfile` writes one —
//! `mint_registration_id`, the fields a profile's attributes prefill, then
//! `record_registration` — because that operation is still a scaffold, so no call can
//! create one yet, exactly as `job_listing.rs` beside this file seeds the jobs it reports.
//!
//! The rules under test, each over a SET of inputs rather than one recorded request:
//!
//! * a `registrationId` that is not an identifier of a registration at all — whatever it
//!   is instead — is `ValidationException` (400) naming the member it arrived on, while a
//!   well-formed identifier this store holds no registration for is
//!   `ResourceNotFoundException` (404), and both spellings the shape documents reach a
//!   stored registration;
//! * the profile is resolved by the same two-way rule, and before the registration;
//! * the import is the WORK of the job the call starts, performed when the clock makes
//!   that job's transition due and not before: until then the attributes are what they
//!   were, and from then on the job reports `SUCCESS` and the profile it updated;
//! * `onAttributeConflict` decides whether an attribute that already has a value keeps it,
//!   for every value the enum declares and for a request that omits the member;
//! * `smartMatch` decides which attributes map at all — every one of them, or only the
//!   standard fields every profile is seeded with — for every value and for a request that
//!   omits the member;
//! * a repeat of a `clientToken` reports the job the first call started rather than
//!   starting a second import, and a token reused for different parameters is refused;
//! * `ListRegistrationsFromBrandProfile` reports exactly the registrations associated with
//!   the profile asked about, member for member, each exactly once across pages at every
//!   page size, and refuses a token it never issued.
//!
//! The clock is DRIVEN rather than waited on: a test that needs a job's work applied asks
//! this service's own store to perform what the clock owes as of a chosen instant, which is
//! what a later call does when the clock reaches it. One test keeps the real-clock path and
//! polls for the condition.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_endusermessaging_endusermessaging::state::{
    registration_arn, registration_fields, service_state, Registration, JOB_SETTLEMENT_DELAY,
};
use service_com_amazonaws_endusermessaging_endusermessaging::EmulatedService;

/// This service's sigv4 signing name, from the model (`OPERATIONS.md`, "Wire protocol").
const SIGNING_NAME: &str = "end-user-messaging";

/// The Region every request of this suite is signed for.
const REGION: &str = "us-east-1";

/// The account id every generated emulator presents, as `engine` defines it.
const ACCOUNT_ID: &str = engine::EMULATOR_ACCOUNT_ID;

/// A default attribute every created profile is seeded with, which is therefore one of the
/// standard fields a mapping made without smart matching is limited to
/// (`crate::state::DEFAULT_BRAND_PROFILE_ATTRIBUTES`, read back by
/// `ListBrandProfileAttributes`).
const STANDARD_ATTRIBUTE: &str = "Company Name";

/// A second seeded default attribute, so a test can tell "every standard field" from "the
/// one the registration carried".
const OTHER_STANDARD_ATTRIBUTE: &str = "Business Type";

/// An attribute name the service seeds no profile with: a caller's own, which maps only
/// under smart matching.
const CUSTOM_ATTRIBUTE: &str = "Loyalty Program";

/// How long to keep looking for a job's terminal status before giving up, in the one test
/// that waits for the real clock: generous, because the condition is the clock reaching the
/// instant the transition was owed at, not a bound on how fast the host serves a request.
const SETTLEMENT_DEADLINE: Duration = Duration::from_secs(120);

/// How long to wait between looks while waiting for a job to reach its terminal status.
const SETTLEMENT_POLL_INTERVAL: Duration = Duration::from_millis(50);

/// One response, reduced to what the assertions read.
#[derive(Debug)]
struct Reply {
    status: u16,
    /// The wire error code (`x-amzn-errortype`, else the body's `__type`), with any
    /// `namespace#` prefix stripped. `None` on success.
    error: Option<String>,
    body: Value,
}

/// One emulator over one shared engine: every call, and every seeded registration, sees the
/// same store.
struct Emulator {
    router: MultiServiceRouter,
    engine: Arc<engine::InMemoryEngine>,
}

impl Emulator {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        router.set_request_authentication(false);
        router.register_service(EmulatedService::new(Arc::clone(&engine)));
        Self { router, engine }
    }

    async fn call(&self, method: &str, path_and_query: &str, body: Option<Value>) -> Reply {
        let authorization = format!(
            "AWS4-HMAC-SHA256 Credential=AKIDTESTONLY/20260101/{REGION}/{SIGNING_NAME}/aws4_request, \
             SignedHeaders=host, Signature=00"
        );
        let payload = body
            .map(|value| Bytes::from(serde_json::to_vec(&value).expect("a serializable body")))
            .unwrap_or_default();
        let request = http::Request::builder()
            .method(method)
            .uri(format!(
                "https://{SIGNING_NAME}.{REGION}.amazonaws.com{path_and_query}"
            ))
            .header("authorization", authorization)
            .header("content-type", "application/json")
            .body(payload)
            .expect("a well-formed test request");

        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let header_error = response
            .headers()
            .get("x-amzn-errortype")
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        let bytes = response.into_body();
        let body: Value = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap_or(Value::Null)
        };
        let error = if status < 400 {
            None
        } else {
            header_error
                .or_else(|| body["__type"].as_str().map(str::to_string))
                .map(|code| {
                    code.rsplit('#')
                        .next()
                        .unwrap_or(code.as_str())
                        .split('/')
                        .next()
                        .unwrap_or(code.as_str())
                        .to_string()
                })
        };
        Reply {
            status,
            error,
            body,
        }
    }

    /// `CreateBrandProfile`, reporting the created profile's id.
    async fn create_profile(&self, name: &str) -> String {
        let created = self
            .call(
                "POST",
                "/v1/brand-profiles",
                Some(json!({ "brandProfileName": name })),
            )
            .await;
        assert_eq!(
            created.status, 200,
            "creating the profile {name:?} must succeed, got {} / {:?}",
            created.status, created.body
        );
        created.body["brandProfileId"]
            .as_str()
            .unwrap_or_else(|| panic!("a created profile reports its id, got {:?}", created.body))
            .to_string()
    }

    /// `UpdateBrandProfileAttribute`: the value a caller writes into an attribute the
    /// profile already holds.
    async fn set_attribute(&self, profile_id: &str, name: &str, value: &str) {
        let written = self
            .call(
                "PUT",
                &format!(
                    "/v1/brand-profiles/{profile_id}/attributes/{}",
                    path_segment(name)
                ),
                Some(json!({ "attributeValue": value })),
            )
            .await;
        assert_eq!(
            written.status, 200,
            "writing {name:?} = {value:?} must succeed, got {} / {:?}",
            written.status, written.body
        );
    }

    /// `CreateBrandProfileAttributes`: an attribute of the caller's own, which the service
    /// seeds no profile with.
    async fn create_attribute(&self, profile_id: &str, name: &str, value: &str) {
        let created = self
            .call(
                "POST",
                &format!("/v1/brand-profiles/{profile_id}/attributes"),
                Some(json!({
                    "attributes": [{
                        "attributeName": name,
                        "attributeType": "TEXT",
                        "attributeValue": value,
                    }],
                })),
            )
            .await;
        assert_eq!(
            created.status, 200,
            "creating {name:?} = {value:?} must succeed, got {} / {:?}",
            created.status, created.body
        );
    }

    /// `GetBrandProfileAttribute`.
    async fn get_attribute(&self, profile_id: &str, name: &str) -> Reply {
        self.call(
            "GET",
            &format!(
                "/v1/brand-profiles/{profile_id}/attributes/{}",
                path_segment(name)
            ),
            None,
        )
        .await
    }

    /// `DeleteBrandProfileAttribute`: the attribute a profile no longer holds, so what an
    /// import does with a field whose attribute is gone is visible.
    async fn delete_attribute(&self, profile_id: &str, name: &str) {
        let deleted = self
            .call(
                "DELETE",
                &format!(
                    "/v1/brand-profiles/{profile_id}/attributes/{}",
                    path_segment(name)
                ),
                None,
            )
            .await;
        assert_eq!(
            deleted.status, 200,
            "deleting {name:?} must succeed, got {} / {:?}",
            deleted.status, deleted.body
        );
    }

    /// The `attributeValue` a stored attribute reports, or `None` for one the profile does
    /// not hold or that carries no value.
    async fn attribute_value(&self, profile_id: &str, name: &str) -> Option<String> {
        let read = self.get_attribute(profile_id, name).await;
        read.body["attributeValue"].as_str().map(str::to_string)
    }

    /// `UpdateBrandProfileFromRegistration`.
    async fn update_from_registration(&self, profile_id: &str, body: Value) -> Reply {
        self.call(
            "POST",
            &format!("/v1/brand-profiles/{profile_id}/update-from-registration"),
            Some(body),
        )
        .await
    }

    /// `GetBrandProfile`.
    async fn get_profile(&self, profile_id: &str) -> Reply {
        self.call("GET", &format!("/v1/brand-profiles/{profile_id}"), None)
            .await
    }

    /// `ListJobs`, with `query` as the request's whole query string.
    async fn list_jobs(&self, query: &str) -> Reply {
        self.call("GET", &format!("/v1/jobs{query}"), None).await
    }

    /// `ListRegistrationsFromBrandProfile`, with `query` as the request's whole query
    /// string.
    async fn list_registrations(&self, profile_id: &str, query: &str) -> Reply {
        self.call(
            "GET",
            &format!("/v1/brand-profiles/{profile_id}/registrations{query}"),
            None,
        )
        .await
    }

    /// Store one registration the way `CreateRegistrationsFromBrandProfile` does: mint an
    /// id, build its ARN, and record it with the fields the profile's attributes prefill
    /// under `smart_match`.
    fn seed_registration(
        &self,
        brand_profile_id: &str,
        registration_type: &str,
        smart_match: bool,
    ) -> (String, String) {
        let state = service_state(&*self.engine).expect("this service's own store");
        let mut store = state.lock();
        let profile = store
            .brand_profiles
            .get(brand_profile_id)
            .cloned()
            .unwrap_or_else(|| panic!("the profile {brand_profile_id} a registration is created from"));
        let fields = registration_fields(&profile, smart_match);
        self.record_registration(&mut store, brand_profile_id, registration_type, smart_match, fields)
    }

    /// Store one registration carrying exactly `fields`, for a case that needs a field the
    /// source profile no longer has.
    fn seed_registration_with_fields(
        &self,
        brand_profile_id: &str,
        fields: &[(&str, &str)],
    ) -> (String, String) {
        let state = service_state(&*self.engine).expect("this service's own store");
        let mut store = state.lock();
        let fields: BTreeMap<String, String> = fields
            .iter()
            .map(|(name, value)| ((*name).to_string(), (*value).to_string()))
            .collect();
        self.record_registration(&mut store, brand_profile_id, "SENDER_ID", true, fields)
    }

    fn record_registration(
        &self,
        store: &mut service_com_amazonaws_endusermessaging_endusermessaging::state::ComAmazonawsEndusermessagingEndusermessagingState,
        brand_profile_id: &str,
        registration_type: &str,
        smart_match: bool,
        fields: BTreeMap<String, String>,
    ) -> (String, String) {
        let id = store.mint_registration_id();
        let arn = registration_arn(REGION, &id);
        store.record_registration(Registration {
            id: id.clone(),
            arn: arn.clone(),
            registration_type: registration_type.to_string(),
            brand_profile_id: brand_profile_id.to_string(),
            created_at: SystemTime::now(),
            smart_match_used: smart_match,
            fields,
        });
        (id, arn)
    }

    /// Perform the work, and apply the terminal status, every stored job's transition is
    /// due at by `as_of` — which is what a later call of this service does once the clock
    /// reaches that instant. The test drives the clock rather than waiting for it.
    fn settle_jobs_as_of(&self, as_of: SystemTime) {
        let state = service_state(&*self.engine).expect("this service's own store");
        state.lock().settle_jobs(as_of);
    }

    /// The stored job `job_id`, as this service's own store holds it.
    fn stored_job_status(&self, job_id: &str) -> String {
        let state = service_state(&*self.engine).expect("this service's own store");
        let store = state.lock();
        store
            .jobs
            .get(job_id)
            .unwrap_or_else(|| panic!("the stored job {job_id}"))
            .status
            .clone()
    }

    /// The value a stored attribute holds, read out of the store rather than over the wire:
    /// a wire read performs whatever the clock owes AS OF NOW, so it is the store that says
    /// what the clock had reached at the instant a test drove it to.
    fn stored_attribute_value(&self, profile_id: &str, name: &str) -> Option<String> {
        let state = service_state(&*self.engine).expect("this service's own store");
        let store = state.lock();
        store
            .brand_profiles
            .get(profile_id)
            .and_then(|profile| profile.attributes.get(name))
            .and_then(|attribute| attribute.value.clone())
    }
}

/// `name` as one path segment: the attribute names of this service carry interior spaces,
/// which an SDK percent-encodes on the way into the URI.
fn path_segment(name: &str) -> String {
    name.replace(' ', "%20")
}

/// The `jobId` the single result of an accepted call reports.
fn accepted_job_id(case: &str, reply: &Reply) -> String {
    assert_eq!(
        reply.status, 200,
        "case {case:?}: the call must be accepted, got {} / {:?}",
        reply.status, reply.body
    );
    let results = reply.body["results"]
        .as_array()
        .unwrap_or_else(|| panic!("case {case:?}: results must be a list, got {:?}", reply.body));
    assert_eq!(
        results.len(),
        1,
        "case {case:?}: one job per identifier the request asked about, got {:?}",
        reply.body
    );
    results[0]["jobId"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: a result reports its jobId, got {:?}", reply.body))
        .to_string()
}

/// A profile carrying a value in each of the two standard attributes under test, and a
/// registration that captured both.
async fn profile_and_registration(em: &Emulator, name: &str) -> (String, String, String) {
    let profile_id = em.create_profile(name).await;
    em.set_attribute(&profile_id, STANDARD_ATTRIBUTE, "Captured Industries")
        .await;
    em.set_attribute(&profile_id, OTHER_STANDARD_ATTRIBUTE, "Captured Type")
        .await;
    let (registration_id, registration_arn) = em.seed_registration(&profile_id, "SENDER_ID", true);
    (profile_id, registration_id, registration_arn)
}

#[tokio::test]
async fn a_registration_identifier_is_classified_before_it_is_looked_up() {
    let em = Emulator::new();
    let (profile_id, registration_id, registration_arn) =
        profile_and_registration(&em, "identifier gates").await;

    // What a value that reaches no registration is refused as. The rows differ along what
    // the value is instead of an identifier of a registration: the wrong shape, the wrong
    // resource, the wrong vendor, or an identifier of the right shape that names nothing.
    let unknown_arn = format!(
        "arn:aws:{SIGNING_NAME}:{REGION}:{ACCOUNT_ID}:registration/reg-00000000000000000"
    );
    let other_vendor_arn = format!(
        "arn:aws:sms-voice:{REGION}:{ACCOUNT_ID}:registration/reg-abc12345678901234"
    );
    let profile_arn = em.get_profile(&profile_id).await.body["brandProfileArn"]
        .as_str()
        .expect("a profile reports its ARN")
        .to_string();
    let invalid: [(&str, String); 6] = [
        (
            "no prefix and body at all",
            "notaregistration".to_string(),
        ),
        ("a prefix with an empty body", "reg-".to_string()),
        ("an empty prefix", "-abc12345678901234".to_string()),
        (
            "a body that is itself written in several parts",
            "not-a-registration".to_string(),
        ),
        ("another resource's ARN", profile_arn),
        ("another vendor's ARN", other_vendor_arn),
    ];
    for (case, value) in invalid {
        let refused = em
            .update_from_registration(&profile_id, json!({ "registrationId": value }))
            .await;
        // The whole sentence is this refusal's fixed text and the member's own name its one
        // variable part: no value of the caller's appears in it.
        let stated = "registrationId must be a valid registration identifier";
        assert_eq!(
            (refused.status, refused.error.as_deref()),
            (400, Some("ValidationException")),
            "case {case:?}: a value that is no registration identifier is refused as input, \
             got {} / {:?}",
            refused.status,
            refused.body
        );
        assert_eq!(
            refused.body["message"].as_str(),
            Some(stated),
            "case {case:?} (value {value:?}): expected {stated:?}, got {:?}",
            refused.body
        );
        assert_eq!(
            refused.body["fieldList"],
            json!([{ "path": "registrationId", "message": stated }]),
            "case {case:?}: the member at fault is named by its own name, got {:?}",
            refused.body
        );
    }

    // A well-formed identifier this store holds no registration for is the missing
    // resource, in either spelling the shape documents, and whatever the opaque body and
    // prefix of that identifier are: a registration is created in the account's
    // registration service too, so an identifier this service did not mint is still an
    // identifier.
    for (case, value) in [
        (
            "a bare identifier naming nothing",
            "reg-00000000000000000",
        ),
        (
            "a bare identifier whose opaque body is written at another length",
            "reg-doesnotexist",
        ),
        (
            "a bare identifier carrying a prefix this service does not mint",
            "r-00000000000000000000000000000000",
        ),
        ("an ARN naming nothing", unknown_arn.as_str()),
    ] {
        let refused = em
            .update_from_registration(&profile_id, json!({ "registrationId": value }))
            .await;
        assert_eq!(
            (refused.status, refused.error.as_deref()),
            (404, Some("ResourceNotFoundException")),
            "case {case:?}: a well-formed identifier naming nothing is not found, got {} / {:?}",
            refused.status,
            refused.body
        );
        assert_eq!(
            refused.body["message"].as_str(),
            Some(
                format!("Resource not found - ResourceType=\"registration\" ResourceId=\"{value}\"")
                    .as_str()
            ),
            "case {case:?}: the refusal names the resource type and the value given, got {:?}",
            refused.body
        );
        assert_eq!(
            (
                refused.body["resourceType"].as_str(),
                refused.body["resourceId"].as_str()
            ),
            (Some("registration"), Some(value)),
            "case {case:?}: the refusal reports the resource it is about, got {:?}",
            refused.body
        );
    }

    // Both spellings of a stored registration are served.
    for (case, value) in [
        ("a stored registration by its id", registration_id.as_str()),
        ("a stored registration by its ARN", registration_arn.as_str()),
    ] {
        let accepted = em
            .update_from_registration(&profile_id, json!({ "registrationId": value }))
            .await;
        let job_id = accepted_job_id(case, &accepted);
        assert_eq!(
            accepted.body["results"][0]["resourceIdentifier"].as_str(),
            Some(value),
            "case {case:?}: the result pairs the job with the identifier the request gave, \
             got {:?}",
            accepted.body
        );
        assert!(
            !job_id.is_empty(),
            "case {case:?}: the job the call started is named, got {:?}",
            accepted.body
        );
    }
}

#[tokio::test]
async fn the_profile_is_resolved_by_the_same_rule_and_before_the_registration() {
    let em = Emulator::new();
    let (profile_id, registration_id, _) = profile_and_registration(&em, "profile gates").await;

    // The rows differ along what the profile identifier is instead of one of a stored
    // profile, and each carries a registration identifier that is itself at fault, so what
    // the refusal is about also says which resource is resolved first.
    let rows: [(&str, &str, u16, &str); 3] = [
        (
            "a profile identifier of no profile at all",
            "notaprofile",
            400,
            "ValidationException",
        ),
        (
            "a well-formed profile identifier naming nothing",
            "bp-00000000000000000",
            404,
            "ResourceNotFoundException",
        ),
        (
            "a stored profile, so the registration is reached",
            profile_id.as_str(),
            200,
            "",
        ),
    ];
    for (case, profile, status, error) in rows {
        let reply = em
            .update_from_registration(
                profile,
                json!({ "registrationId": registration_id.as_str() }),
            )
            .await;
        assert_eq!(
            (reply.status, reply.error.as_deref().unwrap_or("")),
            (status, error),
            "case {case:?}: expected {status} {error:?}, got {} / {:?}",
            reply.status,
            reply.body
        );
    }

    // A request whose profile AND registration are both unreachable is refused about the
    // profile: it is the resource this operation is addressed at.
    let refused = em
        .update_from_registration(
            "bp-00000000000000000",
            json!({ "registrationId": "notaregistration" }),
        )
        .await;
    assert_eq!(
        (refused.status, refused.error.as_deref()),
        (404, Some("ResourceNotFoundException")),
        "the profile is resolved before the registration, got {} / {:?}",
        refused.status,
        refused.body
    );
}

#[tokio::test]
async fn the_import_is_the_work_of_the_job_and_lands_when_the_clock_makes_it_due() {
    let em = Emulator::new();
    let profile_id = em.create_profile("deferred import").await;
    em.set_attribute(&profile_id, STANDARD_ATTRIBUTE, "Captured Industries")
        .await;
    let (registration_id, _) = em.seed_registration(&profile_id, "US_TOLL_FREE_REGISTRATION", true);
    // The profile drifts from what the registration captured, which is what a refresh from
    // that registration undoes.
    em.set_attribute(&profile_id, STANDARD_ATTRIBUTE, "Drifted Industries")
        .await;
    let profile_updated_before = em.get_profile(&profile_id).await.body["updatedAt"]
        .as_f64()
        .expect("a profile reports when it was updated");

    let accepted_at = SystemTime::now();
    let accepted = em
        .update_from_registration(
            &profile_id,
            json!({ "registrationId": registration_id.as_str() }),
        )
        .await;
    let job_id = accepted_job_id("an accepted import", &accepted);

    // The call records the work; it does not perform it. Read from the store rather than
    // from a wire read, so the assertion is about the state the clock has reached and not
    // about how fast the host served the call.
    assert_eq!(
        em.stored_job_status(&job_id),
        "PROCESSING",
        "an accepted job is reported as processing until its transition comes due"
    );
    // An instant no later than the one the call was made at, so it is strictly before the
    // instant the job's transition is due however slowly the host ran.
    em.settle_jobs_as_of(accepted_at);
    assert_eq!(
        em.stored_attribute_value(&profile_id, STANDARD_ATTRIBUTE),
        Some("Drifted Industries".to_string()),
        "the attributes are what they were until the job's work comes due"
    );

    // At the instant the transition was owed, the work is performed: the registration's
    // captured value is back in the attribute, the job reports it finished, and it names
    // the profile it updated. No later than that instant is reached by taking the clock
    // now, which the call that started the job precedes.
    em.settle_jobs_as_of(SystemTime::now() + JOB_SETTLEMENT_DELAY);
    assert_eq!(
        em.attribute_value(&profile_id, STANDARD_ATTRIBUTE).await,
        Some("Captured Industries".to_string()),
        "the registration's captured value is imported once the job's work comes due"
    );
    let listed = em.list_jobs("").await;
    let jobs = listed.body["jobs"]
        .as_array()
        .unwrap_or_else(|| panic!("ListJobs reports a list, got {:?}", listed.body));
    let job = jobs
        .iter()
        .find(|job| job["jobId"].as_str() == Some(job_id.as_str()))
        .unwrap_or_else(|| panic!("the job the call started, got {:?}", listed.body));
    assert_eq!(
        (
            job["status"].as_str(),
            job["operationType"].as_str(),
            job["brandProfileId"].as_str()
        ),
        (
            Some("SUCCESS"),
            Some("UpdateBrandProfileFromRegistration"),
            Some(profile_id.as_str())
        ),
        "the settled job reports what it did, got {job:?}"
    );
    assert_eq!(
        job["resources"][0]["resourceType"].as_str(),
        Some("BRAND_PROFILE"),
        "the job reports the profile it updated, got {job:?}"
    );
    assert_eq!(
        job["resources"][0]["resourceId"].as_str(),
        Some(profile_id.as_str()),
        "the job reports the profile it updated, got {job:?}"
    );
    let profile_updated_after = em.get_profile(&profile_id).await.body["updatedAt"]
        .as_f64()
        .expect("a profile reports when it was updated");
    assert!(
        profile_updated_after >= profile_updated_before,
        "the imported-into profile is reported as updated no earlier than before the \
         import, got {profile_updated_after} after {profile_updated_before}"
    );
}

#[tokio::test]
async fn a_job_reaches_its_terminal_status_on_the_clock_alone() {
    let em = Emulator::new();
    let profile_id = em.create_profile("clock-owed import").await;
    em.set_attribute(&profile_id, STANDARD_ATTRIBUTE, "Captured Industries")
        .await;
    let (registration_id, _) = em.seed_registration(&profile_id, "SENDER_ID", true);
    em.set_attribute(&profile_id, STANDARD_ATTRIBUTE, "Drifted Industries")
        .await;

    let accepted = em
        .update_from_registration(
            &profile_id,
            json!({ "registrationId": registration_id.as_str() }),
        )
        .await;
    let job_id = accepted_job_id("an import nothing but the clock settles", &accepted);

    // Look again until the work the clock owes has been performed, or a generous deadline
    // passes: the condition is the transition coming due, never a bound on how fast the
    // host serves a request.
    let give_up_at = Instant::now() + SETTLEMENT_DEADLINE;
    loop {
        let value = em.attribute_value(&profile_id, STANDARD_ATTRIBUTE).await;
        if value.as_deref() == Some("Captured Industries") {
            break;
        }
        assert!(
            Instant::now() < give_up_at,
            "the job {job_id} must perform its import once the clock makes it due; the \
             attribute still reads {value:?}"
        );
        tokio::time::sleep(SETTLEMENT_POLL_INTERVAL).await;
    }
    assert_eq!(
        em.stored_job_status(&job_id),
        "SUCCESS",
        "a job that performed its work reports it finished"
    );
}

#[tokio::test]
async fn on_attribute_conflict_decides_whether_a_stored_value_is_overwritten() {
    let em = Emulator::new();

    // The rows differ along the value `onAttributeConflict` carries — each the enum
    // declares, and the member absent — and along whether the attribute being imported
    // into already has a value.
    let rows: [(&str, Option<&str>, bool, &str); 6] = [
        (
            "REPLACE over a value already there",
            Some("REPLACE"),
            true,
            "Captured Industries",
        ),
        (
            "REPLACE into an attribute with no value",
            Some("REPLACE"),
            false,
            "Captured Industries",
        ),
        (
            "PRESERVE over a value already there",
            Some("PRESERVE"),
            true,
            "Drifted Industries",
        ),
        (
            "PRESERVE into an attribute with no value",
            Some("PRESERVE"),
            false,
            "Captured Industries",
        ),
        (
            "the member absent, over a value already there: REPLACE is the default",
            None,
            true,
            "Captured Industries",
        ),
        (
            "the member absent, into an attribute with no value",
            None,
            false,
            "Captured Industries",
        ),
    ];
    for (index, (case, on_conflict, holds_a_value, expected)) in rows.into_iter().enumerate() {
        // Each row gets its own profile and registration, so one row's import cannot be
        // what another row reads. The name is an index because `BrandProfileName`'s pattern
        // admits no punctuation, and a row's case is prose.
        let profile_id = em.create_profile(&format!("conflict row {index}")).await;
        let (registration_id, _) = em.seed_registration_with_fields(
            &profile_id,
            &[(STANDARD_ATTRIBUTE, "Captured Industries")],
        );
        if holds_a_value {
            em.set_attribute(&profile_id, STANDARD_ATTRIBUTE, "Drifted Industries")
                .await;
        }
        let mut body = json!({ "registrationId": registration_id.as_str() });
        if let Some(on_conflict) = on_conflict {
            body["onAttributeConflict"] = json!(on_conflict);
        }
        let accepted = em.update_from_registration(&profile_id, body).await;
        accepted_job_id(case, &accepted);
        em.settle_jobs_as_of(SystemTime::now() + JOB_SETTLEMENT_DELAY);
        let actual = em.attribute_value(&profile_id, STANDARD_ATTRIBUTE).await;
        assert_eq!(
            actual.as_deref(),
            Some(expected),
            "case {case:?}: expected {expected:?}, got {actual:?}"
        );
    }
}

#[tokio::test]
async fn smart_match_decides_which_attributes_map() {
    let em = Emulator::new();

    // The rows differ along the value `smartMatch` carries — true, false, and the member
    // absent, whose default is true — and the expectations differ along whether the
    // attribute a field maps to is one the service seeds every profile with.
    let rows: [(&str, Option<bool>, bool); 3] = [
        ("smart matching on", Some(true), true),
        ("smart matching off", Some(false), false),
        ("the member absent: the default is true", None, true),
    ];
    for (index, (case, smart_match, maps_a_custom_attribute)) in rows.into_iter().enumerate() {
        // Each row gets its own profile and registration; the name is an index, as above.
        let profile_id = em
            .create_profile(&format!("smart match row {index}"))
            .await;
        // One standard field, and one attribute of the caller's own: the registration
        // captures both, the way a registration created from this profile is prefilled.
        em.set_attribute(&profile_id, STANDARD_ATTRIBUTE, "Captured Industries")
            .await;
        em.create_attribute(&profile_id, CUSTOM_ATTRIBUTE, "Gold")
            .await;
        let (registration_id, _) = em.seed_registration(&profile_id, "SENDER_ID", true);
        // The profile then loses the caller's own attribute altogether and the standard one
        // drifts, so what the import does to each is visible.
        em.delete_attribute(&profile_id, CUSTOM_ATTRIBUTE).await;
        em.set_attribute(&profile_id, STANDARD_ATTRIBUTE, "Drifted Industries")
            .await;
        let mut body = json!({ "registrationId": registration_id.as_str() });
        if let Some(smart_match) = smart_match {
            body["smartMatch"] = json!(smart_match);
        }
        let accepted = em.update_from_registration(&profile_id, body).await;
        accepted_job_id(case, &accepted);
        em.settle_jobs_as_of(SystemTime::now() + JOB_SETTLEMENT_DELAY);

        // A standard field maps however the request asked.
        let standard = em.attribute_value(&profile_id, STANDARD_ATTRIBUTE).await;
        assert_eq!(
            standard.as_deref(),
            Some("Captured Industries"),
            "case {case:?}: a standard field maps whatever smartMatch asked, got {standard:?}"
        );
        // An attribute of the caller's own maps only under smart matching, and the import
        // creates the attribute the profile does not hold yet.
        let custom = em.get_attribute(&profile_id, CUSTOM_ATTRIBUTE).await;
        if maps_a_custom_attribute {
            assert_eq!(
                (custom.status, custom.body["attributeValue"].as_str()),
                (200, Some("Gold")),
                "case {case:?}: smart matching maps every field, creating the attribute the \
                 profile does not hold, got {} / {:?}",
                custom.status,
                custom.body
            );
            assert_eq!(
                custom.body["attributeType"].as_str(),
                Some("TEXT"),
                "case {case:?}: an imported field is an inline value, got {:?}",
                custom.body
            );
        } else {
            assert_eq!(
                (custom.status, custom.error.as_deref()),
                (404, Some("ResourceNotFoundException")),
                "case {case:?}: without smart matching only the standard fields map, so no \
                 attribute is created, got {} / {:?}",
                custom.status,
                custom.body
            );
        }
    }
}

#[tokio::test]
async fn a_repeated_client_token_reports_the_job_the_first_call_started() {
    let em = Emulator::new();
    let (profile_id, registration_id, _) =
        profile_and_registration(&em, "idempotent import").await;
    let token = "import-token-1";
    let request = json!({
        "registrationId": registration_id.as_str(),
        "clientToken": token,
    });

    let first = em
        .update_from_registration(&profile_id, request.clone())
        .await;
    let job_id = accepted_job_id("the first call", &first);

    // The same request again reports the same job rather than starting a second import —
    // before the job's work comes due and after it has been performed.
    for case in ["a repeat while the job is processing", "a repeat after it finished"] {
        let repeat = em
            .update_from_registration(&profile_id, request.clone())
            .await;
        let repeated_job = accepted_job_id(case, &repeat);
        assert_eq!(
            repeated_job, job_id,
            "case {case:?}: a repeat reports the job the first call started, got {:?}",
            repeat.body
        );
        let listed = em.list_jobs("").await;
        assert_eq!(
            listed.body["jobs"].as_array().map(Vec::len),
            Some(1),
            "case {case:?}: a repeat starts no second job, got {:?}",
            listed.body
        );
        em.settle_jobs_as_of(SystemTime::now() + JOB_SETTLEMENT_DELAY);
    }

    // The token reused for different parameters is refused rather than served the first
    // request's job as if the new parameters had been applied. The rows differ along which
    // parameter was changed.
    let mut other_conflict = request.clone();
    other_conflict["onAttributeConflict"] = json!("PRESERVE");
    let mut other_smart_match = request.clone();
    other_smart_match["smartMatch"] = json!(false);
    let (other_registration_id, _) = em.seed_registration(&profile_id, "SENDER_ID", true);
    let mut other_registration = request.clone();
    other_registration["registrationId"] = json!(other_registration_id.as_str());
    for (case, body) in [
        ("another onAttributeConflict", other_conflict),
        ("another smartMatch", other_smart_match),
        ("another registration", other_registration),
    ] {
        let refused = em.update_from_registration(&profile_id, body).await;
        assert_eq!(
            (refused.status, refused.error.as_deref()),
            (400, Some("ValidationException")),
            "case {case:?}: a token reused for other parameters is refused, got {} / {:?}",
            refused.status,
            refused.body
        );
        assert_eq!(
            refused.body["message"].as_str(),
            Some("The idempotency token and parameters presented do not match"),
            "case {case:?}: the refusal states that the token and the request disagree, \
             got {:?}",
            refused.body
        );
    }
}

#[tokio::test]
async fn a_job_whose_resource_is_gone_when_its_work_comes_due_reports_it_failed() {
    let em = Emulator::new();
    let profile_id = em.create_profile("deleted before the import").await;
    em.set_attribute(&profile_id, STANDARD_ATTRIBUTE, "Captured Industries")
        .await;
    let (registration_id, _) = em.seed_registration(&profile_id, "SENDER_ID", true);

    let accepted = em
        .update_from_registration(
            &profile_id,
            json!({ "registrationId": registration_id.as_str() }),
        )
        .await;
    let job_id = accepted_job_id("an import of a profile about to be deleted", &accepted);

    // The profile goes away before the job's work comes due, so there is nothing left to
    // import into: the job is reported as having failed rather than as having imported
    // something nothing holds.
    let deleted = em
        .call("DELETE", &format!("/v1/brand-profiles/{profile_id}"), None)
        .await;
    assert_eq!(
        deleted.status, 200,
        "deleting the profile must succeed, got {} / {:?}",
        deleted.status, deleted.body
    );
    em.settle_jobs_as_of(SystemTime::now() + JOB_SETTLEMENT_DELAY);

    let listed = em.list_jobs("").await;
    let jobs = listed.body["jobs"]
        .as_array()
        .unwrap_or_else(|| panic!("ListJobs reports a list, got {:?}", listed.body));
    let job = jobs
        .iter()
        .find(|job| job["jobId"].as_str() == Some(job_id.as_str()))
        .unwrap_or_else(|| panic!("the job the call started, got {:?}", listed.body));
    assert_eq!(
        job["status"].as_str(),
        Some("FAILED"),
        "a job whose resource is gone reports it failed, got {job:?}"
    );
    assert_eq!(
        job["errorCode"].as_str(),
        Some("ResourceNotFoundException"),
        "the failure names the class of error it belongs to, got {job:?}"
    );
    assert_eq!(
        job["errorMessage"].as_str(),
        Some(
            format!(
                "Resource not found - ResourceType=\"brand-profile\" ResourceId=\"{profile_id}\""
            )
            .as_str()
        ),
        "the failure says which resource could not be reached, got {job:?}"
    );
    assert!(
        job["resources"].is_null(),
        "a job that created or updated nothing reports no resources, got {job:?}"
    );
}

#[tokio::test]
async fn the_registrations_of_one_profile_are_reported_and_paged() {
    let em = Emulator::new();
    let profile_id = em.create_profile("associated registrations").await;
    em.set_attribute(&profile_id, STANDARD_ATTRIBUTE, "Captured Industries")
        .await;
    // The rows differ along the registration type and along whether smart matching was used
    // to create the association, which are the members the summary reports beside the
    // identifier.
    let seeded: Vec<(String, &str, bool)> = [
        ("US_TOLL_FREE_REGISTRATION", true),
        ("SENDER_ID", false),
        ("TEN_DLC_CAMPAIGN", true),
    ]
    .into_iter()
    .map(|(registration_type, smart_match)| {
        let (id, _) = em.seed_registration(&profile_id, registration_type, smart_match);
        (id, registration_type, smart_match)
    })
    .collect();
    // A registration of ANOTHER profile, which this profile's list must not report.
    let other_profile_id = em.create_profile("another profile").await;
    let (other_registration_id, _) = em.seed_registration(&other_profile_id, "SENDER_ID", true);

    let listed = em.list_registrations(&profile_id, "").await;
    assert_eq!(
        listed.status, 200,
        "listing a profile's registrations must succeed, got {} / {:?}",
        listed.status, listed.body
    );
    let associations = listed.body["registrationAssociations"]
        .as_array()
        .unwrap_or_else(|| panic!("the list reports an array, got {:?}", listed.body))
        .clone();
    assert_eq!(
        associations.len(),
        seeded.len(),
        "every registration of this profile, and nothing else, is reported, got {:?}",
        listed.body
    );
    for (id, registration_type, smart_match) in &seeded {
        let reported = associations
            .iter()
            .find(|association| association["registrationId"].as_str() == Some(id.as_str()))
            .unwrap_or_else(|| panic!("the registration {id} must be reported, got {associations:?}"));
        assert_eq!(
            (
                reported["registrationType"].as_str(),
                reported["smartMatchUsed"].as_bool()
            ),
            (Some(*registration_type), Some(*smart_match)),
            "case {id:?}: the summary reports the registration as stored, got {reported:?}"
        );
        assert!(
            reported["createdAt"].is_number(),
            "case {id:?}: the summary reports when the registration was created, got \
             {reported:?}"
        );
    }
    assert!(
        associations
            .iter()
            .all(|association| association["registrationId"].as_str()
                != Some(other_registration_id.as_str())),
        "another profile's registration is not reported here, got {associations:?}"
    );
    assert!(
        listed.body["nextToken"].is_null(),
        "a page that returns everything emits no token, got {:?}",
        listed.body
    );

    // Every registration is returned exactly once across pages, in one order, at every page
    // size — including one that pages the list one registration at a time.
    for page_size in 1..=seeded.len() + 1 {
        let mut walked: Vec<String> = Vec::new();
        let mut query = format!("?maxResults={page_size}");
        loop {
            let page = em.list_registrations(&profile_id, &query).await;
            assert_eq!(
                page.status, 200,
                "case {page_size}: a page must be served, got {} / {:?}",
                page.status, page.body
            );
            let rows = page.body["registrationAssociations"]
                .as_array()
                .unwrap_or_else(|| panic!("case {page_size}: a page reports an array, got {:?}", page.body))
                .clone();
            assert!(
                rows.len() <= page_size,
                "case {page_size}: a page returns no more than it was asked for, got {:?}",
                page.body
            );
            walked.extend(
                rows.iter()
                    .filter_map(|row| row["registrationId"].as_str().map(str::to_string)),
            );
            match page.body["nextToken"].as_str() {
                Some(token) => query = format!("?maxResults={page_size}&nextToken={token}"),
                None => break,
            }
        }
        let mut expected: Vec<String> = seeded.iter().map(|(id, _, _)| id.clone()).collect();
        expected.sort();
        assert_eq!(
            walked, expected,
            "case {page_size}: every registration is returned exactly once, in one order"
        );
    }

    // A profile with no registrations reports an empty list rather than a refusal.
    let empty = em.list_registrations(&other_profile_id, "").await;
    assert_eq!(
        (
            empty.status,
            empty.body["registrationAssociations"].as_array().map(Vec::len)
        ),
        (200, Some(1)),
        "the other profile reports its own one registration, got {:?}",
        empty.body
    );
    let bare = em.create_profile("profile with no registrations").await;
    let none = em.list_registrations(&bare, "").await;
    assert_eq!(
        (
            none.status,
            none.body["registrationAssociations"].as_array().map(Vec::len)
        ),
        (200, Some(0)),
        "a profile with no registrations reports an empty list, got {:?}",
        none.body
    );

    // The profile is resolved by the same two-way rule every reader of one uses, and a
    // token this service never issued is refused rather than read as some resume point.
    let rows: [(&str, &str, &str, u16, &str); 3] = [
        (
            "an identifier of no profile at all",
            "notaprofile",
            "",
            400,
            "ValidationException",
        ),
        (
            "a well-formed identifier naming no profile",
            "bp-00000000000000000",
            "",
            404,
            "ResourceNotFoundException",
        ),
        (
            "a token this service never issued",
            profile_id.as_str(),
            "?nextToken=deadbeef",
            400,
            "ValidationException",
        ),
    ];
    for (case, profile, query, status, error) in rows {
        let refused = em.list_registrations(profile, query).await;
        assert_eq!(
            (refused.status, refused.error.as_deref()),
            (status, Some(error)),
            "case {case:?}: expected {status} {error:?}, got {} / {:?}",
            refused.status,
            refused.body
        );
    }
}
