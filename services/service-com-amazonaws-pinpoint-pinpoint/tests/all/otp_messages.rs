// SPDX-License-Identifier: CC0-1.0

//! One-time passcodes (`SendOTPMessage` and `VerifyOTPMessage`), end to end over HTTP.
//!
//! Every case drives the real restJson1 wire through the `MultiServiceRouter` the
//! production front door uses, so routing, deserialization, the handler,
//! serialization and the HTTP status are all exercised. Each suite in this crate
//! mounts its own router over its own shared engine, the way the sibling suites do.
//!
//! The rules under test, each asserted over a SET of inputs rather than one request:
//!
//! * a send mints a code of the requested length, stores it as the challenge for the
//!   request's `ReferenceId` and `DestinationIdentity`, and answers one result under
//!   the destination it was sent to;
//! * whether that destination took the code is decided by the request's
//!   `OriginationIdentity` — an identity the account does not hold is reported for the
//!   destination with a 400 and this service's fixed origination-identity sentence,
//!   whichever channels the application has configured, and either way the destination
//!   carries the `MessageId` of the message the send minted for it;
//! * a verification is valid only for the exact code stored for that same pair, only
//!   while the challenge is inside its validity period and still answers an attempt,
//!   and only once — every other verification is a 200 reporting `Valid: false`;
//! * each verification that reaches the comparison spends one of the challenge's
//!   `AllowedAttempts`;
//! * a request this service cannot mint a code for is refused through the modelled
//!   client-error channel and stores nothing, and an unknown application is a 404;
//! * the challenge store is bounded, so repeated sending cannot grow it without limit.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use bytes::Bytes;
use serde_json::{json, Value};
use service_com_amazonaws_pinpointsmsvoicev2_pinpointsmsvoicev2 as sms_voice;
use smithy_server_runtime::MultiServiceRouter;

/// The region every case in this suite signs for; the suite asserts OTP behaviour,
/// not ARNs.
const REGION: &str = "us-east-1";

/// The origination identity every send in this suite is sent from, leased to the
/// account before the send so the message has a number behind it.
const ORIGINATION_NUMBER: &str = "+15550000000";

/// The identifier the lease of [`ORIGINATION_NUMBER`] is held under.
const ORIGINATION_ID: &str = "phone-0000000000000000000000000000002a";

/// A mounted emulator holding one shared engine, exactly as the router builds it.
struct Emulator {
    router: MultiServiceRouter,
    engine: Arc<engine::InMemoryEngine>,
}

impl Emulator {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        // The suite signs nothing: it asserts service behaviour, not SigV4.
        router.set_request_authentication(false);
        router.register_service(service_com_amazonaws_pinpoint_pinpoint::EmulatedService::new(
            Arc::clone(&engine),
        ));
        Self { router, engine }
    }

    /// Dispatch one request and return the status plus the parsed JSON body
    /// (`Value::Null` for an empty body).
    async fn call(&self, method: &str, uri: &str, body: Option<Value>) -> (u16, Value) {
        let payload = body.map_or_else(Bytes::new, |v| {
            Bytes::from(serde_json::to_vec(&v).expect("a test body always serializes"))
        });
        let request = ::http::Request::builder()
            .method(method)
            .uri(uri)
            .header("content-type", "application/json")
            // The routing key: Pinpoint's sigv4 signing name is `mobiletargeting`.
            .header(
                "authorization",
                format!(
                    "AWS4-HMAC-SHA256 Credential=AKIDTESTONLY/20260101/{REGION}/mobiletargeting/aws4_request, SignedHeaders=host, Signature=0000"
                ),
            )
            .body(payload)
            .expect("a well-formed test request");

        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let bytes = response.into_body();
        let parsed = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap_or(Value::Null)
        };
        (status, parsed)
    }

    /// Create one application and return its minted id.
    async fn create_app(&self, name: &str) -> String {
        let (status, body) = self
            .call("POST", "/v1/apps", Some(json!({"Name": name})))
            .await;
        assert_eq!(status, 201, "CreateApp must answer 201, got {body:?}");
        body["Id"]
            .as_str()
            .expect("CreateApp answers with the minted Id")
            .to_string()
    }

    /// `SendOTPMessage`: the request parameters are the whole payload.
    async fn send_otp(&self, app: &str, parameters: Value) -> (u16, Value) {
        self.call("POST", &format!("/v1/apps/{app}/otp"), Some(parameters))
            .await
    }

    /// `VerifyOTPMessage` for one reference, destination and code.
    async fn verify_otp(
        &self,
        app: &str,
        reference_id: &str,
        destination: &str,
        code: &str,
    ) -> (u16, Value) {
        self.call(
            "POST",
            &format!("/v1/apps/{app}/verify-otp"),
            Some(json!({
                "ReferenceId": reference_id,
                "DestinationIdentity": destination,
                "Otp": code,
            })),
        )
        .await
    }

    /// Lease one phone number to the account, so an OTP send naming it as its
    /// `OriginationIdentity` has a number behind it.
    ///
    /// The account's origination identities belong to the SMS and voice API, which
    /// leases them through `RequestPhoneNumber`; this is the record that lease leaves
    /// behind, written straight into that service's own store so this suite stays a
    /// single-service suite. The cross-service wire path — lease there, send here — is
    /// covered by the router suite `pinpoint_otp_origination_identity`.
    fn lease_origination_identity(&self, id: &str, number: &str) -> String {
        let arn = format!("arn:aws:sms-voice:{REGION}:000000000000:phone-number/{id}");
        let store = sms_voice::state::service_state(&*self.engine)
            .expect("the shared engine holds the SMS and voice store");
        let mut guard = store.lock();
        guard.phone_numbers.insert(
            id.to_string(),
            sms_voice::state::PhoneNumberRecord {
                id: id.to_string(),
                arn: arn.clone(),
                phone_number: number.to_string(),
                status: "ACTIVE".to_string(),
                iso_country_code: "US".to_string(),
                message_type: "TRANSACTIONAL".to_string(),
                number_capabilities: vec!["SMS".to_string()],
                number_type: "LONG_CODE".to_string(),
                monthly_leasing_price: "0.00".to_string(),
                two_way_enabled: false,
                two_way_channel_arn: None,
                two_way_channel_role: None,
                self_managed_opt_outs_enabled: false,
                opt_out_list_name: "Default".to_string(),
                international_sending_enabled: false,
                deletion_protection_enabled: false,
                pool_id: None,
                registration_id: None,
                tags: Vec::new(),
                created_timestamp: SystemTime::now(),
            },
        );
        arn
    }

    /// Give one leased number back, the way `ReleasePhoneNumber` does.
    fn release_origination_identity(&self, id: &str) {
        let store = sms_voice::state::service_state(&*self.engine)
            .expect("the shared engine holds the SMS and voice store");
        let mut guard = store.lock();
        guard.phone_numbers.remove(id);
    }

    /// Enable or disable the application's SMS channel: the write `UpdateSmsChannel`
    /// will make once it is implemented, made here through the same state method every
    /// channel update goes through. An OTP send is NOT gated on it, which is what the
    /// channel-independence case uses this for.
    fn set_sms_channel(&self, app: &str, enabled: bool) {
        let store = service_com_amazonaws_pinpoint_pinpoint::state::service_state(&*self.engine)
            .expect("the shared engine holds this service's state");
        let mut guard = store.lock();
        let stored = guard
            .apps
            .get_mut(app)
            .expect("the application under test is stored");
        stored.upsert_channel(
            service_com_amazonaws_pinpoint_pinpoint::state::CHANNEL_TYPE_SMS,
            enabled,
            Some("sender-id".to_string()),
        );
    }

    /// The code stored for one reference and destination, read straight out of this
    /// service's store: nothing on the wire reports it, because the real service
    /// delivers it to the destination instead.
    fn stored_code(&self, app: &str, reference_id: &str, destination: &str) -> Option<String> {
        let store = service_com_amazonaws_pinpoint_pinpoint::state::service_state(&*self.engine)
            .expect("the shared engine holds this service's state");
        let guard = store.lock();
        let key = service_com_amazonaws_pinpoint_pinpoint::state::otp_key(reference_id, destination);
        guard.apps.get(app)?.otps.get(&key).map(|held| held.code.clone())
    }

    /// How many live challenges the application holds.
    fn challenge_count(&self, app: &str) -> usize {
        let store = service_com_amazonaws_pinpoint_pinpoint::state::service_state(&*self.engine)
            .expect("the shared engine holds this service's state");
        let guard = store.lock();
        guard.apps.get(app).map_or(0, |stored| stored.otps.len())
    }

    /// Move one stored challenge's expiry into the past: the suite drives the clock
    /// through the store rather than waiting for time to pass.
    fn expire_challenge(&self, app: &str, reference_id: &str, destination: &str) {
        let store = service_com_amazonaws_pinpoint_pinpoint::state::service_state(&*self.engine)
            .expect("the shared engine holds this service's state");
        let mut guard = store.lock();
        let key = service_com_amazonaws_pinpoint_pinpoint::state::otp_key(reference_id, destination);
        let held = guard
            .apps
            .get_mut(app)
            .and_then(|stored| stored.otps.get_mut(&key))
            .expect("the challenge under test is stored");
        held.expires_at = SystemTime::now()
            .checked_sub(Duration::from_secs(1))
            .expect("an instant one second ago is representable");
    }

    /// The deliveries recorded against `app`, as `(address, channel, status, message_id)`
    /// rows read straight out of this service's store.
    fn recorded_deliveries(&self, app: &str) -> Vec<(String, String, String, String)> {
        let store = service_com_amazonaws_pinpoint_pinpoint::state::service_state(&*self.engine)
            .expect("the shared engine holds this service's state");
        let guard = store.lock();
        let stored = guard
            .apps
            .get(app)
            .expect("the application under test is stored");
        stored
            .sends
            .iter()
            .flat_map(|send| send.deliveries.iter())
            .map(|delivery| {
                (
                    delivery.address.clone().unwrap_or_default(),
                    delivery.channel_type.clone().unwrap_or_default(),
                    delivery.delivery_status.clone(),
                    delivery.message_id.clone().unwrap_or_default(),
                )
            })
            .collect()
    }
}

/// The modelled error code carried by a restJson1 error body (`__type`).
fn error_code(body: &Value) -> String {
    body.get("__type")
        .and_then(Value::as_str)
        .unwrap_or("<no __type>")
        .rsplit(['#', '/'])
        .next()
        .unwrap_or("<no __type>")
        .to_string()
}

/// A code of the same shape as `code` that is never equal to it: every `0` becomes a
/// `1` and every other digit a `0`, so no digit maps to itself.
fn wrong_code(code: &str) -> String {
    code.chars()
        .map(|digit| if digit == '0' { '1' } else { '0' })
        .collect()
}

/// `Valid` out of a `VerifyOTPMessage` response that answered 200.
fn valid_flag(status: u16, body: &Value) -> bool {
    assert_eq!(status, 200, "VerifyOTPMessage must answer 200, got {body:?}");
    body["Valid"]
        .as_bool()
        .unwrap_or_else(|| panic!("VerifyOTPMessage answers with Valid, got {body:?}"))
}

/// The request parameters a send needs, before the row under test adds its own.
fn base_send(destination: &str, reference_id: &str) -> serde_json::Map<String, Value> {
    let mut parameters = serde_json::Map::new();
    parameters.insert("BrandName".to_string(), json!("Example Corp"));
    parameters.insert("Channel".to_string(), json!("SMS"));
    parameters.insert("DestinationIdentity".to_string(), json!(destination));
    parameters.insert("OriginationIdentity".to_string(), json!(ORIGINATION_NUMBER));
    parameters.insert("ReferenceId".to_string(), json!(reference_id));
    parameters
}

/// One row of the send table: the optional parameters it names, and the code length it
/// must therefore get.
struct SendCase {
    case: &'static str,
    channel: &'static str,
    destination: &'static str,
    reference: &'static str,
    code_length: Option<i64>,
    allowed_attempts: Option<i64>,
    validity_period: Option<i64>,
    expected_code_length: usize,
}

#[tokio::test]
async fn a_sent_code_verifies_once_for_the_pair_it_was_sent_for() {
    let emulator = Emulator::new();
    let app = emulator.create_app("otp-round-trip").await;
    emulator.lease_origination_identity(ORIGINATION_ID, ORIGINATION_NUMBER);
    let shortest = service_com_amazonaws_pinpoint_pinpoint::state::MIN_CODE_LENGTH;
    let longest = service_com_amazonaws_pinpoint_pinpoint::state::MAX_CODE_LENGTH;
    let shortest_asked = i64::try_from(shortest).expect("a code length fits an i64");
    let longest_asked = i64::try_from(longest).expect("a code length fits an i64");

    // The rows differ along everything the rule generalises over: the length asked
    // for (absent, each bound of the accepted range, and a value inside it), the
    // attempt budget, the validity period, the destination's form and the casing of
    // the channel.
    let cases = [
        SendCase {
            case: "every optional parameter left out",
            channel: "SMS",
            destination: "+15551234567",
            reference: "order-1",
            code_length: None,
            allowed_attempts: None,
            validity_period: None,
            expected_code_length: 6,
        },
        SendCase {
            case: "the shortest accepted code",
            channel: "SMS",
            destination: "+442071838750",
            reference: "order-2",
            code_length: Some(shortest_asked),
            allowed_attempts: Some(1),
            validity_period: Some(1),
            expected_code_length: shortest,
        },
        SendCase {
            case: "the longest accepted code",
            channel: "SMS",
            destination: "+819012345678",
            reference: "order-3",
            code_length: Some(longest_asked),
            allowed_attempts: Some(10),
            validity_period: Some(60),
            expected_code_length: longest,
        },
        SendCase {
            case: "a channel value in another casing",
            channel: "sms",
            destination: "+5511987654321",
            reference: "order-4",
            code_length: Some(8),
            allowed_attempts: Some(3),
            validity_period: Some(1440),
            expected_code_length: 8,
        },
        SendCase {
            case: "the same destination under a second reference",
            channel: "SMS",
            destination: "+15551234567",
            reference: "order-5",
            code_length: Some(7),
            allowed_attempts: Some(2),
            validity_period: Some(5),
            expected_code_length: 7,
        },
    ];

    let mut codes: BTreeSet<String> = BTreeSet::new();
    let mut message_ids: BTreeSet<String> = BTreeSet::new();
    for case in cases {
        let mut parameters = base_send(case.destination, case.reference);
        parameters.insert("Channel".to_string(), json!(case.channel));
        if let Some(length) = case.code_length {
            parameters.insert("CodeLength".to_string(), json!(length));
        }
        if let Some(attempts) = case.allowed_attempts {
            parameters.insert("AllowedAttempts".to_string(), json!(attempts));
        }
        if let Some(minutes) = case.validity_period {
            parameters.insert("ValidityPeriod".to_string(), json!(minutes));
        }
        let (status, body) = emulator.send_otp(&app, Value::Object(parameters)).await;
        assert_eq!(
            status, 200,
            "case {:?}: SendOTPMessage must answer 200, got {body:?}",
            case.case
        );
        assert_eq!(
            body["ApplicationId"].as_str(),
            Some(app.as_str()),
            "case {:?}: the response names the application it was sent for, got {body:?}",
            case.case
        );
        let request_id = body["RequestId"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        assert!(
            !request_id.is_empty(),
            "case {:?}: the send answers with a RequestId, got {body:?}",
            case.case
        );
        let result = &body["Result"];
        assert_eq!(
            result
                .as_object()
                .map(|results| results.keys().cloned().collect::<Vec<_>>()),
            Some(vec![case.destination.to_string()]),
            "case {:?}: the result is reported under the destination the code went to, got {body:?}",
            case.case
        );
        let outcome = &result[case.destination];
        assert_eq!(
            (
                outcome["DeliveryStatus"].as_str(),
                outcome["StatusCode"].as_i64()
            ),
            (Some("SUCCESSFUL"), Some(200)),
            "case {:?}: an accepted OTP send reports SUCCESSFUL/200, got {outcome:?}",
            case.case
        );
        let message_id = outcome["MessageId"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        assert!(
            message_ids.insert(message_id.clone()),
            "case {:?}: every accepted send mints its own MessageId, got {message_id:?}",
            case.case
        );

        // The stored challenge: a code of the length that was asked for, all digits,
        // and never a code another send already minted.
        let code = emulator
            .stored_code(&app, case.reference, case.destination)
            .unwrap_or_else(|| {
                panic!(
                    "case {:?}: the send stores the challenge it minted",
                    case.case
                )
            });
        assert_eq!(
            code.chars().count(),
            case.expected_code_length,
            "case {:?}: expected a {}-digit code, got {code:?}",
            case.case,
            case.expected_code_length
        );
        assert!(
            code.chars().all(char::is_numeric),
            "case {:?}: a minted code is decimal digits, got {code:?}",
            case.case
        );
        assert!(
            codes.insert(code.clone()),
            "case {:?}: each send mints its own code, got {code:?}",
            case.case
        );

        // The round trip: the stored code verifies for the pair it was sent for, and
        // only once — a one-time passcode is spent by the verification that accepts it.
        let (status, body) = emulator
            .verify_otp(&app, case.reference, case.destination, &code)
            .await;
        assert!(
            valid_flag(status, &body),
            "case {:?}: the code that was sent must verify, got {body:?}",
            case.case
        );
        let (status, body) = emulator
            .verify_otp(&app, case.reference, case.destination, &code)
            .await;
        assert!(
            !valid_flag(status, &body),
            "case {:?}: a code that already verified cannot verify again, got {body:?}",
            case.case
        );
        assert_eq!(
            emulator.stored_code(&app, case.reference, case.destination),
            None,
            "case {:?}: the challenge is consumed by the verification that accepted it",
            case.case
        );
    }
}

#[tokio::test]
async fn a_wrong_code_spends_one_of_the_allowed_attempts() {
    let emulator = Emulator::new();
    let app = emulator.create_app("otp-attempts").await;
    emulator.lease_origination_identity(ORIGINATION_ID, ORIGINATION_NUMBER);
    let destination = "+15557654321";

    // Each row names an attempt budget and how many wrong codes are tried before the
    // right one: the right code verifies while the budget holds and is refused once
    // the wrong tries have spent it.
    for (case, allowed, wrong_tries, expected) in [
        ("the budget is spent exactly", 1_i64, 1_usize, false),
        ("one try left", 2, 1, true),
        ("every try spent", 2, 2, false),
        ("two of three spent", 3, 2, true),
        ("three of three spent", 3, 3, false),
    ] {
        let reference = format!("attempts-{allowed}-{wrong_tries}");
        let mut parameters = base_send(destination, &reference);
        parameters.insert("AllowedAttempts".to_string(), json!(allowed));
        let (status, body) = emulator.send_otp(&app, Value::Object(parameters)).await;
        assert_eq!(status, 200, "case {case:?}: expected 200, got {body:?}");
        let code = emulator
            .stored_code(&app, &reference, destination)
            .unwrap_or_else(|| panic!("case {case:?}: the send stores its challenge"));

        for try_index in 0..wrong_tries {
            let (status, body) = emulator
                .verify_otp(&app, &reference, destination, &wrong_code(&code))
                .await;
            assert!(
                !valid_flag(status, &body),
                "case {case:?}: wrong code {try_index} must not verify, got {body:?}"
            );
        }
        let (status, body) = emulator
            .verify_otp(&app, &reference, destination, &code)
            .await;
        assert_eq!(
            valid_flag(status, &body),
            expected,
            "case {case:?}: after {wrong_tries} wrong tries of {allowed} allowed, the right code must report Valid {expected}, got {body:?}"
        );

        // A fresh send for the same pair replaces the spent challenge, so the newest
        // code is the one that verifies and the one before it is gone.
        let mut parameters = base_send(destination, &reference);
        parameters.insert("AllowedAttempts".to_string(), json!(allowed));
        let (status, body) = emulator.send_otp(&app, Value::Object(parameters)).await;
        assert_eq!(status, 200, "case {case:?}: the resend expected 200, got {body:?}");
        let resent = emulator
            .stored_code(&app, &reference, destination)
            .unwrap_or_else(|| panic!("case {case:?}: the resend stores its challenge"));
        let (status, body) = emulator
            .verify_otp(&app, &reference, destination, &resent)
            .await;
        assert!(
            valid_flag(status, &body),
            "case {case:?}: the code last sent for the pair verifies, got {body:?}"
        );
    }
}

#[tokio::test]
async fn a_verification_that_does_not_match_the_stored_challenge_is_not_valid() {
    let emulator = Emulator::new();
    let app = emulator.create_app("otp-rejections").await;
    emulator.lease_origination_identity(ORIGINATION_ID, ORIGINATION_NUMBER);
    let destination = "+15550001111";
    let reference = "checkout-7";
    let mut parameters = base_send(destination, reference);
    // Room for the two rows below that reach the comparison, so the surviving
    // challenge is what the final assertion reads rather than a spent budget.
    parameters.insert("AllowedAttempts".to_string(), json!(10));
    let (status, body) = emulator.send_otp(&app, Value::Object(parameters)).await;
    assert_eq!(status, 200, "the setup send expected 200, got {body:?}");
    let code = emulator
        .stored_code(&app, reference, destination)
        .expect("the setup send stores its challenge");

    // Every row is answered the same way: a 200 reporting Valid false. The first is
    // the recorded case — a code quoted against a reference nothing was sent for.
    for (case, row_reference, row_destination, row_code) in [
        ("a reference nothing was sent for", "never-sent", destination, code.clone()),
        ("a destination the code was not sent to", reference, "+15559998888", code.clone()),
        ("a blank reference", "", destination, code.clone()),
        ("a blank destination", reference, "", code.clone()),
        ("a wrong code", reference, destination, wrong_code(&code)),
        ("a blank code", reference, destination, String::new()),
    ] {
        let (status, body) = emulator
            .verify_otp(&app, row_reference, row_destination, &row_code)
            .await;
        assert!(
            !valid_flag(status, &body),
            "case {case:?}: expected Valid false, got {body:?}"
        );
    }

    // None of those rejections touched the challenge the code belongs to.
    let (status, body) = emulator
        .verify_otp(&app, reference, destination, &code)
        .await;
    assert!(
        valid_flag(status, &body),
        "the challenge survives every verification that did not match it, got {body:?}"
    );
}

#[tokio::test]
async fn a_challenge_past_its_validity_period_is_not_valid_and_is_dropped() {
    let emulator = Emulator::new();
    let app = emulator.create_app("otp-expiry").await;
    emulator.lease_origination_identity(ORIGINATION_ID, ORIGINATION_NUMBER);
    let destination = "+15552223333";

    for (case, validity) in [
        ("the shortest validity period", Some(1_i64)),
        ("the default validity period", None),
    ] {
        let reference = format!("expiry-{}", validity.unwrap_or_default());
        let mut parameters = base_send(destination, &reference);
        if let Some(minutes) = validity {
            parameters.insert("ValidityPeriod".to_string(), json!(minutes));
        }
        let (status, body) = emulator.send_otp(&app, Value::Object(parameters)).await;
        assert_eq!(status, 200, "case {case:?}: expected 200, got {body:?}");
        let code = emulator
            .stored_code(&app, &reference, destination)
            .unwrap_or_else(|| panic!("case {case:?}: the send stores its challenge"));

        // Inside the validity period the code is still the valid one, so the only
        // thing that changes below is the instant the challenge expires at.
        emulator.expire_challenge(&app, &reference, destination);
        let (status, body) = emulator
            .verify_otp(&app, &reference, destination, &code)
            .await;
        assert!(
            !valid_flag(status, &body),
            "case {case:?}: a code past its validity period must not verify, got {body:?}"
        );
        assert_eq!(
            emulator.stored_code(&app, &reference, destination),
            None,
            "case {case:?}: the expired challenge is dropped, not kept"
        );
    }
}

#[tokio::test]
async fn a_send_this_service_cannot_mint_a_code_for_is_refused_and_stores_nothing() {
    let emulator = Emulator::new();
    let app = emulator.create_app("otp-refusals").await;
    emulator.lease_origination_identity(ORIGINATION_ID, ORIGINATION_NUMBER);
    let destination = "+15554445555";
    let shortest = i64::try_from(service_com_amazonaws_pinpoint_pinpoint::state::MIN_CODE_LENGTH)
        .expect("a code length fits an i64");
    let longest = i64::try_from(service_com_amazonaws_pinpoint_pinpoint::state::MAX_CODE_LENGTH)
        .expect("a code length fits an i64");

    // Each row overrides one member of a request that would otherwise be served: a
    // channel other than the one OTP messages are sent over, a blank required
    // identity, or a numeric parameter outside the values a code can be minted for —
    // including one step past each bound of the accepted code length.
    let rows: Vec<(&str, &str, Value)> = vec![
        ("an email channel", "Channel", json!("EMAIL")),
        ("a voice channel", "Channel", json!("VOICE")),
        ("a push channel", "Channel", json!("GCM")),
        ("a blank channel", "Channel", json!("")),
        ("a blank brand name", "BrandName", json!("   ")),
        ("a blank destination", "DestinationIdentity", json!("")),
        ("a blank origination identity", "OriginationIdentity", json!("")),
        ("a blank reference", "ReferenceId", json!("")),
        ("no digits at all", "CodeLength", json!(shortest - 1)),
        ("a negative code length", "CodeLength", json!(-6)),
        ("one digit past the ceiling", "CodeLength", json!(longest + 1)),
        ("no attempt allowed", "AllowedAttempts", json!(0)),
        ("a negative attempt budget", "AllowedAttempts", json!(-1)),
        ("no validity at all", "ValidityPeriod", json!(0)),
        ("a negative validity period", "ValidityPeriod", json!(-5)),
    ];

    for (case, member, value) in rows {
        let mut parameters = base_send(destination, "refused");
        parameters.insert(member.to_string(), value);
        let (status, body) = emulator.send_otp(&app, Value::Object(parameters)).await;
        assert_eq!(
            (status, error_code(&body).as_str()),
            (400, "BadRequestException"),
            "case {case:?}: {member} must be refused as a 400 BadRequestException, got {body:?}"
        );
        assert_eq!(
            emulator.challenge_count(&app),
            0,
            "case {case:?}: a refused send stores no challenge"
        );
    }

    // And the same request, with nothing overridden, is served — so every refusal
    // above is about the member it changed and not about the rest of the request.
    let (status, body) = emulator
        .send_otp(&app, Value::Object(base_send(destination, "refused")))
        .await;
    assert_eq!(status, 200, "the unmodified request is served, got {body:?}");
    assert_eq!(
        emulator.challenge_count(&app),
        1,
        "the served send stores exactly one challenge"
    );
}

/// The sentence this service reports for a destination whose send names an origination
/// identity the account cannot send from, written out in full so that changing the text
/// has to be deliberate. It carries no variable part at all.
const ORIGINATION_IDENTITY_NOT_USABLE: &str = "The provided number does not exist or does \
     not belong to the account, or its resource policy does not allow Pinpoint to use it";

/// One row of the origination-identity table: the identity the send is sent FROM, and
/// whether the account can send from it.
struct OriginationCase {
    case: &'static str,
    identity: String,
    sendable: bool,
}

#[tokio::test]
async fn the_origination_identity_decides_whether_the_code_is_sent_at_all() {
    let emulator = Emulator::new();
    let app = emulator.create_app("otp-origination-gate").await;
    let destination = "+15553334444";
    let arn = emulator.lease_origination_identity(ORIGINATION_ID, ORIGINATION_NUMBER);
    // A second lease, so a row can send from an identity of the account other than the
    // one every other case in this suite uses.
    let other_id = "phone-000000000000000000000000000000ff";
    let other_number = "+15557770001";
    emulator.lease_origination_identity(other_id, other_number);

    // The rows differ along every form one of the account's identities is named by — the
    // number, the same number without its leading `+`, the lease's identifier, the
    // lease's ARN, a second leased number — and along forms that name no lease of this
    // account at all: a number it never leased, a number one character short of a leased
    // one, a pool identifier and a sender id.
    let cases = [
        OriginationCase {
            case: "the leased number in E.164",
            identity: ORIGINATION_NUMBER.to_string(),
            sendable: true,
        },
        OriginationCase {
            case: "the leased number without its leading plus",
            identity: ORIGINATION_NUMBER.trim_start_matches('+').to_string(),
            sendable: true,
        },
        OriginationCase {
            case: "the identifier the lease is held under",
            identity: ORIGINATION_ID.to_string(),
            sendable: true,
        },
        OriginationCase {
            case: "the ARN of the lease",
            identity: arn.clone(),
            sendable: true,
        },
        OriginationCase {
            case: "a second number the account leases",
            identity: other_number.to_string(),
            sendable: true,
        },
        OriginationCase {
            case: "a number the account never leased",
            identity: "+15559990000".to_string(),
            sendable: false,
        },
        OriginationCase {
            case: "one digit short of a leased number",
            identity: ORIGINATION_NUMBER[..ORIGINATION_NUMBER.len() - 1].to_string(),
            sendable: false,
        },
        OriginationCase {
            case: "a pool nothing created",
            identity: "pool-0000000000000000000000000000000a".to_string(),
            sendable: false,
        },
        OriginationCase {
            case: "a sender id the account does not hold",
            identity: "EXAMPLECORP".to_string(),
            sendable: false,
        },
    ];

    let mut message_ids: BTreeSet<String> = BTreeSet::new();
    for (index, case) in cases.iter().enumerate() {
        let reference = format!("origination-{index}");
        let mut parameters = base_send(destination, &reference);
        parameters.insert("OriginationIdentity".to_string(), json!(case.identity));
        let (status, body) = emulator.send_otp(&app, Value::Object(parameters)).await;
        assert_eq!(
            status, 200,
            "case {:?}: an OTP send is answered 200 whatever the destination's outcome, got {body:?}",
            case.case
        );
        let outcome = &body["Result"][destination];
        let expected = if case.sendable {
            ("SUCCESSFUL", 200)
        } else {
            ("PERMANENT_FAILURE", 400)
        };
        assert_eq!(
            (
                outcome["DeliveryStatus"].as_str(),
                outcome["StatusCode"].as_i64()
            ),
            (Some(expected.0), Some(i64::from(expected.1))),
            "case {:?}: expected {expected:?}, got {outcome:?}",
            case.case
        );

        // The destination carries the handle this send is correlated by whatever became
        // of the delivery, and no two sends share one.
        let message_id = outcome["MessageId"].as_str().unwrap_or_default().to_string();
        assert!(
            !message_id.is_empty(),
            "case {:?}: the destination's result carries a MessageId, got {outcome:?}",
            case.case
        );
        assert!(
            message_ids.insert(message_id.clone()),
            "case {:?}: every send mints its own MessageId, got {message_id:?}",
            case.case
        );

        let stored = emulator.stored_code(&app, &reference, destination);
        if case.sendable {
            let code = stored.unwrap_or_else(|| {
                panic!(
                    "case {:?}: an accepted send stores the challenge it minted",
                    case.case
                )
            });
            let (status, body) = emulator
                .verify_otp(&app, &reference, destination, &code)
                .await;
            assert!(
                valid_flag(status, &body),
                "case {:?}: the code that went out verifies, got {body:?}",
                case.case
            );
        } else {
            let status_message = outcome["StatusMessage"].as_str().unwrap_or_default();
            assert_eq!(
                status_message, ORIGINATION_IDENTITY_NOT_USABLE,
                "case {:?}: the destination reports the origination-identity refusal, got {status_message:?}",
                case.case
            );
            for interpolated in [app.as_str(), destination, case.identity.as_str()] {
                assert!(
                    !status_message.contains(interpolated),
                    "case {:?}: the refusal interpolates nothing, yet it carries {interpolated:?}: {status_message:?}",
                    case.case
                );
            }
            assert_eq!(
                stored, None,
                "case {:?}: a send that delivered nothing mints no code",
                case.case
            );
            // And with no challenge stored, no code can verify for that reference.
            let (status, body) = emulator
                .verify_otp(&app, &reference, destination, "123456")
                .await;
            assert!(
                !valid_flag(status, &body),
                "case {:?}: a reference nothing was sent for cannot verify, got {body:?}",
                case.case
            );
        }
    }
}

#[tokio::test]
async fn the_applications_channel_configuration_does_not_decide_an_otp_send() {
    let emulator = Emulator::new();
    let app = emulator.create_app("otp-channel-independence").await;
    let destination = "+15552224444";
    let unleased = "+15559990000";
    emulator.lease_origination_identity(ORIGINATION_ID, ORIGINATION_NUMBER);

    // The verdict follows the origination identity across every state the application's
    // SMS channel can be in: never configured, enabled, disabled again.
    for (case, channel) in [
        ("with the SMS channel never configured", None),
        ("with the SMS channel enabled", Some(true)),
        ("with the SMS channel disabled again", Some(false)),
    ] {
        if let Some(enabled) = channel {
            emulator.set_sms_channel(&app, enabled);
        }
        for (identity, sendable) in [(ORIGINATION_NUMBER, true), (unleased, false)] {
            let reference = format!("channel-{case}-{identity}");
            let mut parameters = base_send(destination, &reference);
            parameters.insert("OriginationIdentity".to_string(), json!(identity));
            let (status, body) = emulator.send_otp(&app, Value::Object(parameters)).await;
            assert_eq!(status, 200, "case {case:?}: expected 200, got {body:?}");
            let outcome = &body["Result"][destination];
            let expected = if sendable {
                ("SUCCESSFUL", 200)
            } else {
                ("PERMANENT_FAILURE", 400)
            };
            assert_eq!(
                (
                    outcome["DeliveryStatus"].as_str(),
                    outcome["StatusCode"].as_i64()
                ),
                (Some(expected.0), Some(i64::from(expected.1))),
                "case {case:?}: sending from {identity:?} expected {expected:?}, got {outcome:?}"
            );
        }
    }
}

#[tokio::test]
async fn a_released_origination_identity_stops_being_sendable_from() {
    let emulator = Emulator::new();
    let app = emulator.create_app("otp-origination-release").await;
    let destination = "+15551112222";

    // The identity is read off the inventory that owns it on every send, so giving the
    // lease back — and taking it again — changes what the next send answers.
    for (case, leased) in [
        ("before the number is leased", false),
        ("while the number is leased", true),
        ("after the lease is given back", false),
        ("once it is leased again", true),
    ] {
        if leased {
            emulator.lease_origination_identity(ORIGINATION_ID, ORIGINATION_NUMBER);
        } else {
            emulator.release_origination_identity(ORIGINATION_ID);
        }
        let reference = format!("release-{case}");
        let (status, body) = emulator
            .send_otp(&app, Value::Object(base_send(destination, &reference)))
            .await;
        assert_eq!(status, 200, "case {case:?}: expected 200, got {body:?}");
        let outcome = &body["Result"][destination];
        let expected = if leased {
            ("SUCCESSFUL", 200)
        } else {
            ("PERMANENT_FAILURE", 400)
        };
        assert_eq!(
            (
                outcome["DeliveryStatus"].as_str(),
                outcome["StatusCode"].as_i64()
            ),
            (Some(expected.0), Some(i64::from(expected.1))),
            "case {case:?}: expected {expected:?}, got {outcome:?}"
        );
        assert_eq!(
            emulator
                .stored_code(&app, &reference, destination)
                .is_some(),
            leased,
            "case {case:?}: a code is minted only for a send that went out"
        );
    }
}

#[tokio::test]
async fn an_unknown_application_is_a_404_for_both_otp_operations() {
    let emulator = Emulator::new();
    let destination = "+15556667777";

    for case in ["00000000000000000000000000000000", "not-an-application-id"] {
        let (status, body) = emulator
            .send_otp(case, Value::Object(base_send(destination, "unknown-app")))
            .await;
        assert_eq!(
            (status, error_code(&body).as_str()),
            (404, "NotFoundException"),
            "case {case:?}: SendOTPMessage for an unknown application is a 404, got {body:?}"
        );
        assert_eq!(
            body["Message"].as_str().or_else(|| body["message"].as_str()),
            Some("Application not found"),
            "case {case:?}: the refusal names the class of resource, got {body:?}"
        );

        let (status, body) = emulator
            .verify_otp(case, "unknown-app", destination, "123456")
            .await;
        assert_eq!(
            (status, error_code(&body).as_str()),
            (404, "NotFoundException"),
            "case {case:?}: VerifyOTPMessage for an unknown application is a 404, got {body:?}"
        );
    }
}

#[tokio::test]
async fn every_accepted_send_is_recorded_against_the_application() {
    let emulator = Emulator::new();
    let app = emulator.create_app("otp-send-log").await;
    emulator.lease_origination_identity(ORIGINATION_ID, ORIGINATION_NUMBER);
    let destination = "+15558889999";

    let mut expected: Vec<(String, String, String, String)> = Vec::new();
    for round in 0..3 {
        let reference = format!("receipt-{round}");
        let (status, body) = emulator
            .send_otp(&app, Value::Object(base_send(destination, &reference)))
            .await;
        assert_eq!(status, 200, "round {round}: expected 200, got {body:?}");
        let message_id = body["Result"][destination]["MessageId"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        assert!(
            !message_id.is_empty(),
            "round {round}: an accepted send carries a MessageId, got {body:?}"
        );
        expected.push((
            destination.to_string(),
            "SMS".to_string(),
            "SUCCESSFUL".to_string(),
            message_id,
        ));
    }

    assert_eq!(
        emulator.recorded_deliveries(&app),
        expected,
        "the store records every accepted OTP send, in order, under the message id it answered with"
    );
}

#[tokio::test]
async fn the_challenge_store_is_bounded() {
    let emulator = Emulator::new();
    let app = emulator.create_app("otp-bound").await;
    emulator.lease_origination_identity(ORIGINATION_ID, ORIGINATION_NUMBER);
    let destination = "+15551110000";
    let limit = service_com_amazonaws_pinpoint_pinpoint::state::MAX_OTPS_PER_APP;

    // Past the ceiling: sending codes for fresh references cannot grow the store
    // without limit, and the challenge nearest the end of its validity is the one
    // dropped to make room.
    for round in 0..(limit + 5) {
        let reference = format!("bound-{round}");
        let (status, body) = emulator
            .send_otp(&app, Value::Object(base_send(destination, &reference)))
            .await;
        assert_eq!(status, 200, "round {round}: expected 200, got {body:?}");
        let expected = (round + 1).min(limit);
        assert_eq!(
            emulator.challenge_count(&app),
            expected,
            "round {round}: expected {expected} live challenges, got {}",
            emulator.challenge_count(&app)
        );
    }

    // The code last sent still verifies, and the first one sent is gone.
    let newest = format!("bound-{}", limit + 4);
    let code = emulator
        .stored_code(&app, &newest, destination)
        .expect("the newest challenge is one the store kept");
    let (status, body) = emulator.verify_otp(&app, &newest, destination, &code).await;
    assert!(
        valid_flag(status, &body),
        "the code last sent verifies, got {body:?}"
    );
    assert_eq!(
        emulator.stored_code(&app, "bound-0", destination),
        None,
        "the challenge nearest the end of its validity was dropped to make room"
    );
}
