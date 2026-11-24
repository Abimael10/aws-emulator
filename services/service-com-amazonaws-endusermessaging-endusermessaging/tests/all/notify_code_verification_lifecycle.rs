// SPDX-License-Identifier: CC0-1.0

//! The one-time passcode verification pair, end to end over the wire.
//!
//! Every assertion goes through `MultiServiceRouter::dispatch`, so it exercises the
//! same restJson1 path a real SDK client takes: routing, deserialization, boundary
//! constraint validation, the handler, and error rendering. Where a test needs the
//! passcode itself — which no response ever carries — it reads the stored
//! verification off the shared engine, the same store the handlers write.
//!
//! The rules under test, each over a SET of inputs rather than one recorded request:
//!
//! * `ValidateNotifyCodeVerification` reports `VALID` only for the passcode of the
//!   verification in flight for that recipient and `referenceId`, inside its
//!   validity window and inside its attempt ceiling, and `INVALID` for everything
//!   else — a passcode that was never sent, one sent to another recipient or under
//!   another reference, an elapsed one, one past the ceiling, and one already
//!   validated;
//! * `SendNotifyCodeVerification` resolves the passcode policy member by member —
//!   the request's overrides over the referenced configuration's parameters over
//!   this service's defaults — and the passcode it generates has the length and the
//!   character set that policy names;
//! * a send supersedes the passcode already in flight for the same recipient and
//!   `referenceId`, so only the newest one validates;
//! * a send that names no stored configuration is `ResourceNotFoundException`
//!   (404), one that supplies neither a configuration nor channel parameters is
//!   `ValidationException` (400), and the bounded store refuses a send past its
//!   ceiling with `ServiceQuotaExceededException` rather than growing without
//!   bound;
//! * no response carries the passcode.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_endusermessaging_endusermessaging::ops::types::CodeConfigurationParameters;
use service_com_amazonaws_endusermessaging_endusermessaging::state::{
    ComAmazonawsEndusermessagingEndusermessagingState as ServiceState, CodePolicy, Verification,
    CODE_LENGTH_RANGE, DEFAULT_CODE_LENGTH, DEFAULT_CODE_TYPE, DEFAULT_MAX_ATTEMPTS,
    DEFAULT_VALIDITY_PERIOD_MINUTES, MAX_ATTEMPTS_RANGE, MAX_VERIFICATIONS,
    VALIDITY_PERIOD_MINUTES_RANGE,
};
use service_com_amazonaws_endusermessaging_endusermessaging::EmulatedService;

/// This service's sigv4 signing name, from the model (`OPERATIONS.md`, "Wire protocol").
const SIGNING_NAME: &str = "end-user-messaging";

/// The channel parameters a send carries when it references no configuration: one
/// route, the text one, with the template the passcode is rendered into — which carries
/// the code placeholder every supplied template body has to.
fn text_channel_parameters() -> Value {
    json!({ "text": { "inlineTemplateBody": "Your verification code is {{code}}." } })
}

/// One response, reduced to what the assertions read.
#[derive(Debug)]
struct Reply {
    status: u16,
    /// The wire error code (`x-amzn-errortype`, else the body's `__type`), with any
    /// `namespace#` prefix stripped. `None` on success.
    error: Option<String>,
    body: Value,
}

impl Reply {
    fn string(&self, name: &str) -> String {
        self.body[name]
            .as_str()
            .unwrap_or_else(|| panic!("member {name} is not a string in {}", self.body))
            .to_string()
    }
}

/// One emulator over one shared engine: every call sees the same store, and the test
/// can read that store for the values no response reports.
struct Emulator {
    router: MultiServiceRouter,
    engine: Arc<engine::InMemoryEngine>,
    region: String,
}

impl Emulator {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        router.set_request_authentication(false);
        router.register_service(EmulatedService::new(Arc::clone(&engine)));
        Self {
            router,
            engine,
            region: "us-east-1".to_string(),
        }
    }

    async fn call(&self, method: &str, path_and_query: &str, body: Option<Value>) -> Reply {
        let authorization = format!(
            "AWS4-HMAC-SHA256 Credential=AKIDTESTONLY/20260101/{}/{SIGNING_NAME}/aws4_request, \
             SignedHeaders=host, Signature=00",
            self.region
        );
        let payload = body
            .map(|value| Bytes::from(serde_json::to_vec(&value).expect("a serializable body")))
            .unwrap_or_default();
        let request = http::Request::builder()
            .method(method)
            .uri(format!(
                "https://{SIGNING_NAME}.{}.amazonaws.com{path_and_query}",
                self.region
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

    async fn create_configuration(&self, body: Value) -> Reply {
        self.call("POST", "/v1/notify-code-configurations", Some(body))
            .await
    }

    async fn send(&self, body: Value) -> Reply {
        self.call(
            "POST",
            "/v1/notify-code-verifications/send",
            Some(body),
        )
        .await
    }

    async fn validate(&self, body: Value) -> Reply {
        self.call(
            "POST",
            "/v1/notify-code-verifications/validate",
            Some(body),
        )
        .await
    }

    /// The stored verification `id` names, cloned out of the shared store.
    fn stored(&self, id: &str) -> Option<Verification> {
        self.engine
            .state::<ServiceState>()
            .lock()
            .verifications
            .get(id)
            .cloned()
    }

    fn stored_or_panic(&self, case: &str, id: &str) -> Verification {
        self.stored(id)
            .unwrap_or_else(|| panic!("case {case:?}: the store holds no verification {id}"))
    }

    fn held_verifications(&self) -> usize {
        self.engine.state::<ServiceState>().lock().verifications.len()
    }

    /// Move the stored verification's validity window into the past, the way the
    /// clock would once the period elapsed. The test drives the clock instead of
    /// waiting for it.
    fn elapse_validity_period(&self, id: &str) {
        let state = self.engine.state::<ServiceState>();
        let mut store = state.lock();
        let verification = store
            .verifications
            .get_mut(id)
            .unwrap_or_else(|| panic!("the store holds no verification {id}"));
        verification.expires_at = verification
            .created_at
            .checked_sub(Duration::from_secs(1))
            .expect("an instant before the record was created");
    }
}

/// The characters a passcode of each `CodeType` is drawn from, as
/// `smithy_query(query="doc",shape="CodeConfigurationParameters",member="codeType")`
/// documents them: `NUMERIC` digits only, `ALPHA` uppercase letters only, and
/// `ALPHANUMERIC` uppercase letters and digits.
fn alphabet_of(code_type: &str) -> &'static str {
    match code_type {
        "NUMERIC" => "0123456789",
        "ALPHA" => "ABCDEFGHIJKLMNOPQRSTUVWXYZ",
        "ALPHANUMERIC" => "ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789",
        other => panic!("{other} is not one of the modelled CodeType values"),
    }
}

/// A passcode of the same length and character set as `code` that is certainly NOT
/// `code`: every character is advanced `by` places through its own alphabet.
fn shifted_code(code: &str, by: usize) -> String {
    let step = 1 + by;
    code.chars()
        .map(|character| {
            if character.is_ascii_digit() {
                let digits = alphabet_of("NUMERIC").as_bytes();
                let at = (character as usize - '0' as usize + step) % digits.len();
                char::from(digits[at])
            } else {
                let letters = alphabet_of("ALPHA").as_bytes();
                let at = (character as usize - 'A' as usize + step) % letters.len();
                char::from(letters[at])
            }
        })
        .collect()
}

/// One row of the policy table: where each passcode policy member comes from, and
/// what the resolved policy therefore is.
#[derive(Debug)]
struct PolicyCase {
    case: &'static str,
    /// `codeConfigurationParameters` of the configuration the send references, or
    /// `None` when the send references no configuration at all.
    configured: Option<Value>,
    /// `overrideCodeConfigurationParameters` the send carries.
    overrides: Option<Value>,
    channel: &'static str,
    reference_id: Option<String>,
    expected_code_type: &'static str,
    expected_code_length: usize,
    expected_validity_minutes: u64,
    expected_max_attempts: i32,
}

/// The policy table: the rows differ in which layer supplies each member (the send's
/// overrides, the referenced configuration, this service's default), in the
/// `CodeType` drawn from, in each published bound of `CodeLength` (@range(4..8)),
/// `ValidityPeriodMinutes` (@range(1..60)) and `MaxVerificationAttempts`
/// (@range(1..5)), in the delivery channel, and in whether the send binds a
/// `referenceId`.
fn policy_cases() -> Vec<PolicyCase> {
    vec![
        PolicyCase {
            case: "a configuration with no parameters leaves every default in place",
            configured: Some(json!({})),
            overrides: None,
            channel: "TEXT",
            reference_id: None,
            expected_code_type: DEFAULT_CODE_TYPE,
            expected_code_length: DEFAULT_CODE_LENGTH as usize,
            expected_validity_minutes: DEFAULT_VALIDITY_PERIOD_MINUTES as u64,
            expected_max_attempts: DEFAULT_MAX_ATTEMPTS,
        },
        PolicyCase {
            case: "the configuration supplies the shortest ALPHA passcode",
            configured: Some(json!({ "codeType": "ALPHA", "codeLength": 4 })),
            overrides: None,
            channel: "VOICE",
            reference_id: Some("order-4711".to_string()),
            expected_code_type: "ALPHA",
            expected_code_length: 4,
            expected_validity_minutes: DEFAULT_VALIDITY_PERIOD_MINUTES as u64,
            expected_max_attempts: DEFAULT_MAX_ATTEMPTS,
        },
        PolicyCase {
            case: "the configuration supplies the longest ALPHANUMERIC passcode at both \
                   outer bounds",
            configured: Some(json!({
                "codeType": "ALPHANUMERIC",
                "codeLength": 8,
                "validityPeriodMinutes": 60,
                "maxAttempts": 5,
            })),
            overrides: None,
            channel: "WHATSAPP",
            reference_id: None,
            expected_code_type: "ALPHANUMERIC",
            expected_code_length: 8,
            expected_validity_minutes: 60,
            expected_max_attempts: 5,
        },
        PolicyCase {
            case: "the send override wins over the configuration member for member",
            configured: Some(json!({
                "codeType": "ALPHA",
                "codeLength": 4,
                "validityPeriodMinutes": 60,
                "maxAttempts": 5,
            })),
            overrides: Some(json!({
                "codeType": "NUMERIC",
                "codeLength": 8,
                "validityPeriodMinutes": 1,
                "maxAttempts": 2,
            })),
            channel: "TEXT",
            reference_id: Some("S".repeat(256)),
            expected_code_type: "NUMERIC",
            expected_code_length: 8,
            expected_validity_minutes: 1,
            expected_max_attempts: 2,
        },
        PolicyCase {
            case: "no configuration at all: every member is this service's default",
            configured: None,
            overrides: None,
            channel: "TEXT",
            reference_id: Some("signin".to_string()),
            expected_code_type: DEFAULT_CODE_TYPE,
            expected_code_length: DEFAULT_CODE_LENGTH as usize,
            expected_validity_minutes: DEFAULT_VALIDITY_PERIOD_MINUTES as u64,
            expected_max_attempts: DEFAULT_MAX_ATTEMPTS,
        },
        PolicyCase {
            case: "no configuration, one overridden member, the rest defaulted",
            configured: None,
            overrides: Some(json!({ "validityPeriodMinutes": 45, "codeType": "ALPHA" })),
            channel: "VOICE",
            reference_id: None,
            expected_code_type: "ALPHA",
            expected_code_length: DEFAULT_CODE_LENGTH as usize,
            expected_validity_minutes: 45,
            expected_max_attempts: DEFAULT_MAX_ATTEMPTS,
        },
    ]
}

/// The recipient every single-verification case sends to: an E.164 number inside
/// `DestinationIdentity`'s `@length(1..20)` and `^[\x21-\x22\x24-\x7E]+$` pattern.
const RECIPIENT: &str = "+12065550100";

/// The sender every case sends from, inside `OriginationIdentity`'s pattern.
const SENDER: &str = "+12065550199";

/// The body of a send for one policy case, including the configuration it
/// references when the case has one.
async fn send_for(emulator: &Emulator, case: &PolicyCase, destination: &str) -> Reply {
    let mut body = json!({
        "channel": case.channel,
        "destinationIdentity": destination,
        "originationIdentity": SENDER,
    });
    match case.configured.clone() {
        Some(parameters) => {
            let created = emulator
                .create_configuration(json!({
                    "notifyCodeConfigurationName": "policy-under-test",
                    "codeConfigurationParameters": parameters,
                    "channelParameters": text_channel_parameters(),
                }))
                .await;
            assert_eq!(
                created.status, 200,
                "case {:?}: the configuration create expected 200, got {} with {}",
                case.case, created.status, created.body
            );
            let id = created.body["notifyCodeConfiguration"]["notifyCodeConfigurationId"]
                .as_str()
                .unwrap_or_else(|| {
                    panic!(
                        "case {:?}: the create reported no id, got {}",
                        case.case, created.body
                    )
                })
                .to_string();
            body["notifyCodeConfiguration"] = json!(id);
        }
        // With no configuration to take the template from, the send has to carry the
        // channel parameters itself.
        None => body["overrideChannelParameters"] = text_channel_parameters(),
    }
    if let Some(overrides) = case.overrides.clone() {
        body["overrideCodeConfigurationParameters"] = overrides;
    }
    if let Some(reference) = case.reference_id.clone() {
        body["referenceId"] = json!(reference);
    }
    emulator.send(body).await
}

/// A validate body for `destination`, `reference_id` and `code`.
fn validate_body(destination: &str, reference_id: Option<&str>, code: &str) -> Value {
    let mut body = json!({ "destinationIdentity": destination, "code": code });
    if let Some(reference) = reference_id {
        body["referenceId"] = json!(reference);
    }
    body
}

#[tokio::test]
async fn a_sent_passcode_validates_once_under_the_resolved_policy() {
    for case in policy_cases() {
        let emulator = Emulator::new();
        let sent = send_for(&emulator, &case, RECIPIENT).await;
        assert_eq!(
            sent.status, 200,
            "case {:?}: the send expected 200, got {} with {}",
            case.case, sent.status, sent.body
        );
        let verification_id = sent.string("verificationId");
        let message_id = sent.string("messageId");
        assert!(
            !verification_id.is_empty() && verification_id.len() <= 64,
            "case {:?}: verificationId must satisfy @length(1..64), got {verification_id:?}",
            case.case
        );
        assert!(
            !message_id.is_empty() && message_id.len() <= 256,
            "case {:?}: messageId must satisfy @length(1..256), got {message_id:?}",
            case.case
        );
        assert_ne!(
            verification_id, message_id,
            "case {:?}: the verification and the message are different resources, both \
             reported {verification_id:?}",
            case.case
        );

        let record = emulator.stored_or_panic(case.case, &verification_id);
        assert_eq!(
            record.code.chars().count(),
            case.expected_code_length,
            "case {:?}: expected a {}-character passcode, got {} characters",
            case.case,
            case.expected_code_length,
            record.code.chars().count()
        );
        let alphabet = alphabet_of(case.expected_code_type);
        assert!(
            record.code.chars().all(|character| alphabet.contains(character)),
            "case {:?}: expected every character from the {} set {alphabet:?}, got a passcode \
             outside it",
            case.case,
            case.expected_code_type
        );
        assert_eq!(
            record.policy.code_type, case.expected_code_type,
            "case {:?}: expected codeType {:?}, got {:?}",
            case.case, case.expected_code_type, record.policy.code_type
        );
        assert_eq!(
            record.policy.max_attempts, case.expected_max_attempts,
            "case {:?}: expected maxAttempts {}, got {}",
            case.case, case.expected_max_attempts, record.policy.max_attempts
        );
        assert_eq!(
            record
                .expires_at
                .duration_since(record.created_at)
                .expect("the validity window ends after the send"),
            Duration::from_secs(case.expected_validity_minutes * 60),
            "case {:?}: expected a {}-minute validity window, got one of {:?}",
            case.case,
            case.expected_validity_minutes,
            record.expires_at.duration_since(record.created_at)
        );
        assert_eq!(
            record.attempts, 0,
            "case {:?}: a fresh verification has taken no attempt, got {}",
            case.case, record.attempts
        );
        assert_eq!(
            record.channel, case.channel,
            "case {:?}: expected the channel {:?} to be recorded, got {:?}",
            case.case, case.channel, record.channel
        );
        assert_eq!(
            record.reference_id,
            case.reference_id,
            "case {:?}: expected referenceId {:?} to be recorded, got {:?}",
            case.case,
            case.reference_id,
            record.reference_id
        );
        assert!(
            !sent.body.to_string().contains(&record.code),
            "case {:?}: the send response must not carry the passcode, got {}",
            case.case,
            sent.body
        );

        let accepted = emulator
            .validate(validate_body(
                RECIPIENT,
                case.reference_id.as_deref(),
                &record.code,
            ))
            .await;
        assert_eq!(
            accepted.status, 200,
            "case {:?}: the validate expected 200, got {} with {}",
            case.case, accepted.status, accepted.body
        );
        assert_eq!(
            accepted.string("status"),
            "VALID",
            "case {:?}: the sent passcode must validate, got {}",
            case.case,
            accepted.body
        );
        assert!(
            !accepted.body.to_string().contains(&record.code),
            "case {:?}: the validate response must not carry the passcode, got {}",
            case.case,
            accepted.body
        );

        // One passcode validates once: the verification is consumed.
        assert!(
            emulator.stored(&verification_id).is_none(),
            "case {:?}: a validated verification must not stay in flight",
            case.case
        );
        let replayed = emulator
            .validate(validate_body(
                RECIPIENT,
                case.reference_id.as_deref(),
                &record.code,
            ))
            .await;
        assert_eq!(
            replayed.string("status"),
            "INVALID",
            "case {:?}: a passcode already validated must not validate again, got {}",
            case.case,
            replayed.body
        );
    }
}

/// One row of the "nothing to match" table: what was sent (if anything), and what is
/// then submitted.
#[derive(Debug)]
struct MissCase {
    case: &'static str,
    /// The recipient and reference a passcode was sent to first, or `None` when
    /// nothing was sent at all.
    sent_to: Option<(&'static str, Option<&'static str>)>,
    submitted_to: &'static str,
    submitted_reference: Option<&'static str>,
}

#[tokio::test]
async fn a_passcode_with_no_verification_to_match_is_invalid() {
    let cases = vec![
        MissCase {
            case: "nothing was ever sent",
            sent_to: None,
            submitted_to: RECIPIENT,
            submitted_reference: None,
        },
        MissCase {
            case: "nothing was ever sent, and the submission carries a reference",
            sent_to: None,
            submitted_to: RECIPIENT,
            submitted_reference: Some("signin"),
        },
        MissCase {
            case: "the passcode was sent to another recipient",
            sent_to: Some(("+12065550101", None)),
            submitted_to: RECIPIENT,
            submitted_reference: None,
        },
        MissCase {
            case: "the passcode was sent under another reference",
            sent_to: Some((RECIPIENT, Some("signin"))),
            submitted_to: RECIPIENT,
            submitted_reference: Some("checkout"),
        },
        MissCase {
            case: "the passcode was sent under a reference and submitted without one",
            sent_to: Some((RECIPIENT, Some("signin"))),
            submitted_to: RECIPIENT,
            submitted_reference: None,
        },
        MissCase {
            case: "the passcode was sent without a reference and submitted under one",
            sent_to: Some((RECIPIENT, None)),
            submitted_to: RECIPIENT,
            submitted_reference: Some("signin"),
        },
    ];

    for case in cases {
        let emulator = Emulator::new();
        let mut sent_code = None;
        if let Some((destination, reference)) = case.sent_to {
            let mut body = json!({
                "channel": "TEXT",
                "destinationIdentity": destination,
                "originationIdentity": SENDER,
                "overrideChannelParameters": text_channel_parameters(),
            });
            if let Some(reference) = reference {
                body["referenceId"] = json!(reference);
            }
            let sent = emulator.send(body).await;
            assert_eq!(
                sent.status, 200,
                "case {:?}: the send expected 200, got {} with {}",
                case.case, sent.status, sent.body
            );
            let id = sent.string("verificationId");
            sent_code = Some((id.clone(), emulator.stored_or_panic(case.case, &id).code));
        }

        // The submitted value is a passcode the model accepts: either the one that
        // was actually sent elsewhere, or a well-formed passcode of its own.
        let submitted = sent_code
            .as_ref()
            .map(|(_, code)| code.clone())
            .unwrap_or_else(|| "123456".to_string());
        let answered = emulator
            .validate(validate_body(
                case.submitted_to,
                case.submitted_reference,
                &submitted,
            ))
            .await;
        assert_eq!(
            answered.status, 200,
            "case {:?}: a validate with nothing to match is a success, got {} with {}",
            case.case, answered.status, answered.body
        );
        assert_eq!(
            answered.string("status"),
            "INVALID",
            "case {:?}: expected INVALID, got {}",
            case.case,
            answered.body
        );

        // The verification that WAS sent is untouched: a submission under another
        // key neither consumes it nor counts an attempt against it.
        if let Some((id, _)) = sent_code {
            let record = emulator.stored_or_panic(case.case, &id);
            assert_eq!(
                record.attempts, 0,
                "case {:?}: a submission under another key must not count an attempt, got {}",
                case.case, record.attempts
            );
        }
    }
}

#[tokio::test]
async fn attempts_are_counted_and_the_ceiling_locks_the_verification() {
    // Every value `MaxVerificationAttempts` accepts (@range(1..5)).
    for ceiling in 1..=5i32 {
        let case = format!("maxAttempts {ceiling}");
        let emulator = Emulator::new();
        let sent = emulator
            .send(json!({
                "channel": "TEXT",
                "destinationIdentity": RECIPIENT,
                "originationIdentity": SENDER,
                "overrideChannelParameters": text_channel_parameters(),
                "overrideCodeConfigurationParameters": { "maxAttempts": ceiling },
            }))
            .await;
        assert_eq!(
            sent.status, 200,
            "case {case:?}: the send expected 200, got {} with {}",
            sent.status, sent.body
        );
        let id = sent.string("verificationId");
        let code = emulator.stored_or_panic(&case, &id).code;

        for attempt in 1..=ceiling {
            let wrong = shifted_code(&code, attempt as usize);
            let answered = emulator
                .validate(validate_body(RECIPIENT, None, &wrong))
                .await;
            assert_eq!(
                answered.string("status"),
                "INVALID",
                "case {case:?}: attempt {attempt} submitted {wrong:?}, which is not the \
                 passcode, got {}",
                answered.body
            );
            assert_eq!(
                emulator.stored_or_panic(&case, &id).attempts,
                attempt,
                "case {case:?}: after {attempt} submissions the verification has taken \
                 {attempt} attempts, got {}",
                emulator.stored_or_panic(&case, &id).attempts
            );
        }

        // The ceiling is reached, so the verification is locked: even the passcode
        // that was sent is INVALID, and no further attempt is counted.
        let locked = emulator
            .validate(validate_body(RECIPIENT, None, &code))
            .await;
        assert_eq!(
            locked.status, 200,
            "case {case:?}: a locked verification still answers 200, got {} with {}",
            locked.status, locked.body
        );
        assert_eq!(
            locked.string("status"),
            "INVALID",
            "case {case:?}: past the attempt ceiling the sent passcode must not validate, \
             got {}",
            locked.body
        );
        assert_eq!(
            emulator.stored_or_panic(&case, &id).attempts,
            ceiling,
            "case {case:?}: a submission past the ceiling counts no further attempt, got {}",
            emulator.stored_or_panic(&case, &id).attempts
        );
    }
}

#[tokio::test]
async fn an_elapsed_validity_period_is_invalid() {
    // Each published bound of `ValidityPeriodMinutes` (@range(1..60)) and one value
    // between them; `None` is the defaulted window.
    for requested in [None, Some(1), Some(30), Some(60)] {
        let case = format!("validityPeriodMinutes {requested:?}");
        let emulator = Emulator::new();
        let mut body = json!({
            "channel": "TEXT",
            "destinationIdentity": RECIPIENT,
            "originationIdentity": SENDER,
            "overrideChannelParameters": text_channel_parameters(),
        });
        if let Some(minutes) = requested {
            body["overrideCodeConfigurationParameters"] =
                json!({ "validityPeriodMinutes": minutes });
        }
        let sent = emulator.send(body).await;
        assert_eq!(
            sent.status, 200,
            "case {case:?}: the send expected 200, got {} with {}",
            sent.status, sent.body
        );
        let id = sent.string("verificationId");
        let record = emulator.stored_or_panic(&case, &id);
        let expected_minutes =
            u64::try_from(requested.unwrap_or(DEFAULT_VALIDITY_PERIOD_MINUTES)).unwrap();
        assert_eq!(
            record
                .expires_at
                .duration_since(record.created_at)
                .expect("the validity window ends after the send"),
            Duration::from_secs(expected_minutes * 60),
            "case {case:?}: expected a {expected_minutes}-minute window, got {:?}",
            record.expires_at.duration_since(record.created_at)
        );

        // Drive the clock past the window rather than waiting for it.
        emulator.elapse_validity_period(&id);
        let answered = emulator
            .validate(validate_body(RECIPIENT, None, &record.code))
            .await;
        assert_eq!(
            answered.status, 200,
            "case {case:?}: an elapsed verification still answers 200, got {} with {}",
            answered.status, answered.body
        );
        assert_eq!(
            answered.string("status"),
            "INVALID",
            "case {case:?}: a passcode past its validity period must not validate, got {}",
            answered.body
        );
        assert!(
            emulator.stored(&id).is_none(),
            "case {case:?}: an elapsed verification must not stay in the store"
        );
    }
}

#[tokio::test]
async fn a_resend_supersedes_the_passcode_in_flight() {
    // The rows differ in whether the resend is bound to a reference, and in whether
    // the two sends take the same channel.
    for (case, reference, channels) in [
        ("no reference, same channel", None, ["TEXT", "TEXT"]),
        ("a reference, same channel", Some("signin"), ["TEXT", "TEXT"]),
        (
            "a reference, another channel",
            Some("checkout"),
            ["TEXT", "VOICE"],
        ),
    ] {
        let emulator = Emulator::new();
        let mut codes = Vec::new();
        for channel in channels {
            let mut body = json!({
                "channel": channel,
                "destinationIdentity": RECIPIENT,
                "originationIdentity": SENDER,
                "overrideChannelParameters": text_channel_parameters(),
                // ALPHANUMERIC at the longest length, so two draws collide with
                // vanishing probability.
                "overrideCodeConfigurationParameters": {
                    "codeType": "ALPHANUMERIC",
                    "codeLength": 8,
                },
            });
            if let Some(reference) = reference {
                body["referenceId"] = json!(reference);
            }
            let sent = emulator.send(body).await;
            assert_eq!(
                sent.status, 200,
                "case {case:?}: the send expected 200, got {} with {}",
                sent.status, sent.body
            );
            let id = sent.string("verificationId");
            codes.push(emulator.stored_or_panic(case, &id).code);
        }
        assert_eq!(
            emulator.held_verifications(),
            1,
            "case {case:?}: one recipient and reference hold one verification, got {}",
            emulator.held_verifications()
        );

        let superseded = emulator
            .validate(validate_body(RECIPIENT, reference, &codes[0]))
            .await;
        assert_eq!(
            superseded.string("status"),
            "INVALID",
            "case {case:?}: the superseded passcode must not validate, got {}",
            superseded.body
        );
        let current = emulator
            .validate(validate_body(RECIPIENT, reference, &codes[1]))
            .await;
        assert_eq!(
            current.string("status"),
            "VALID",
            "case {case:?}: the passcode sent last must validate, got {}",
            current.body
        );
    }
}

#[tokio::test]
async fn a_send_resolves_the_configuration_by_id_or_arn_and_refuses_an_unknown_one() {
    let emulator = Emulator::new();
    let created = emulator
        .create_configuration(json!({
            "notifyCodeConfigurationName": "signin-otp",
            "codeConfigurationParameters": { "codeType": "ALPHA", "codeLength": 5 },
            "channelParameters": text_channel_parameters(),
        }))
        .await;
    assert_eq!(
        created.status, 200,
        "the configuration create expected 200, got {} with {}",
        created.status, created.body
    );
    let configuration = &created.body["notifyCodeConfiguration"];
    let id = configuration["notifyCodeConfigurationId"]
        .as_str()
        .expect("a created configuration reports its id")
        .to_string();
    let arn = configuration["notifyCodeConfigurationArn"]
        .as_str()
        .expect("a created configuration reports its ARN")
        .to_string();

    // Both forms `NotifyCodeConfigurationIdOrArn` accepts resolve the same policy.
    for (case, identifier, destination) in [
        ("the bare id", id.clone(), "+12065550111"),
        ("the full ARN", arn.clone(), "+12065550112"),
    ] {
        let sent = emulator
            .send(json!({
                "channel": "TEXT",
                "destinationIdentity": destination,
                "originationIdentity": SENDER,
                "notifyCodeConfiguration": identifier,
            }))
            .await;
        assert_eq!(
            sent.status, 200,
            "case {case:?}: the send expected 200, got {} with {}",
            sent.status, sent.body
        );
        let record = emulator.stored_or_panic(case, &sent.string("verificationId"));
        assert_eq!(
            record.notify_code_configuration_id.as_deref(),
            Some(id.as_str()),
            "case {case:?}: expected the policy to be captured from {id}, got {:?}",
            record.notify_code_configuration_id
        );
        assert_eq!(
            record.code.chars().count(),
            5,
            "case {case:?}: expected the configuration's 5-character passcode, got {}",
            record.code.chars().count()
        );
    }

    // An identifier that names nothing is this operation's ResourceNotFoundException,
    // with the message this error class templates.
    for (case, identifier) in [
        ("an unknown id", "ncc-does-not-exist".to_string()),
        (
            "an unknown ARN",
            format!(
                "arn:aws:end-user-messaging:us-east-1:{}:notify-code-configuration/ncc-missing",
                engine::EMULATOR_ACCOUNT_ID
            ),
        ),
    ] {
        let refused = emulator
            .send(json!({
                "channel": "TEXT",
                "destinationIdentity": RECIPIENT,
                "originationIdentity": SENDER,
                "notifyCodeConfiguration": identifier,
            }))
            .await;
        assert_eq!(
            refused.status, 404,
            "case {case:?}: expected 404, got {} with {}",
            refused.status, refused.body
        );
        assert_eq!(
            refused.error.as_deref(),
            Some("ResourceNotFoundException"),
            "case {case:?}: expected ResourceNotFoundException, got {:?} with {}",
            refused.error,
            refused.body
        );
        assert_eq!(
            refused.body["message"].as_str(),
            Some(
                format!(
                    "Resource not found - ResourceType=\"notify-code-configuration\" \
                     ResourceId=\"{identifier}\""
                )
                .as_str()
            ),
            "case {case:?}: expected this error class's templated sentence, got {}",
            refused.body
        );
        assert_eq!(
            emulator.held_verifications(),
            2,
            "case {case:?}: a refused send persists no verification, the store now holds {}",
            emulator.held_verifications()
        );
    }
}

#[tokio::test]
async fn a_send_with_neither_a_configuration_nor_channel_parameters_is_refused() {
    let emulator = Emulator::new();
    for channel in ["TEXT", "VOICE", "WHATSAPP"] {
        let refused = emulator
            .send(json!({
                "channel": channel,
                "destinationIdentity": RECIPIENT,
                "originationIdentity": SENDER,
            }))
            .await;
        assert_eq!(
            refused.status, 400,
            "case {channel:?}: expected 400, got {} with {}",
            refused.status, refused.body
        );
        assert_eq!(
            refused.error.as_deref(),
            Some("ValidationException"),
            "case {channel:?}: expected ValidationException, got {:?} with {}",
            refused.error,
            refused.body
        );
        assert_eq!(
            refused.body["fieldList"][0]["path"].as_str(),
            Some("overrideChannelParameters"),
            "case {channel:?}: the refusal names the member that must be supplied, got {}",
            refused.body
        );
        assert_eq!(
            emulator.held_verifications(),
            0,
            "case {channel:?}: a refused send persists no verification"
        );
    }
}

#[tokio::test]
async fn values_outside_the_modelled_constraints_are_refused() {
    let emulator = Emulator::new();
    // One value past each published bound of the passcode policy members, plus a
    // `code` on either side of `VerificationCode`'s @length(4..8).
    let send_cases = vec![
        ("codeLength below the range", json!({ "codeLength": 3 })),
        ("codeLength above the range", json!({ "codeLength": 9 })),
        (
            "validityPeriodMinutes below the range",
            json!({ "validityPeriodMinutes": 0 }),
        ),
        (
            "validityPeriodMinutes above the range",
            json!({ "validityPeriodMinutes": 61 }),
        ),
        ("maxAttempts below the range", json!({ "maxAttempts": 0 })),
        ("maxAttempts above the range", json!({ "maxAttempts": 6 })),
    ];
    for (case, parameters) in send_cases {
        let refused = emulator
            .send(json!({
                "channel": "TEXT",
                "destinationIdentity": RECIPIENT,
                "originationIdentity": SENDER,
                "overrideChannelParameters": text_channel_parameters(),
                "overrideCodeConfigurationParameters": parameters,
            }))
            .await;
        assert_eq!(
            refused.status, 400,
            "case {case:?}: expected 400, got {} with {}",
            refused.status, refused.body
        );
        assert_eq!(
            refused.error.as_deref(),
            Some("ValidationException"),
            "case {case:?}: expected ValidationException, got {:?} with {}",
            refused.error,
            refused.body
        );
    }

    // `VerificationCode` is @length(4..8): both bounds are accepted and answered
    // with a status, one character either side is refused.
    for (case, code, expected_status) in [
        ("the shortest accepted code", "0000".to_string(), 200),
        ("the longest accepted code", "0".repeat(8), 200),
        ("one character too short", "000".to_string(), 400),
        ("one character too long", "0".repeat(9), 400),
    ] {
        let answered = emulator
            .validate(validate_body(RECIPIENT, None, &code))
            .await;
        assert_eq!(
            answered.status, expected_status,
            "case {case:?}: expected {expected_status}, got {} with {}",
            answered.status, answered.body
        );
        if expected_status == 200 {
            assert_eq!(
                answered.string("status"),
                "INVALID",
                "case {case:?}: no passcode was sent, so the submission is INVALID, got {}",
                answered.body
            );
        }
    }
}

#[tokio::test]
async fn the_verification_store_is_bounded() {
    let emulator = Emulator::new();
    let mut destinations = BTreeSet::new();
    for index in 0..MAX_VERIFICATIONS {
        let destination = format!("+1206555{index:04}");
        destinations.insert(destination.clone());
        let sent = emulator
            .send(json!({
                "channel": "TEXT",
                "destinationIdentity": destination,
                "originationIdentity": SENDER,
                "overrideChannelParameters": text_channel_parameters(),
            }))
            .await;
        assert_eq!(
            sent.status, 200,
            "send {index} inside the ceiling expected 200, got {} with {}",
            sent.status, sent.body
        );
    }
    assert_eq!(
        emulator.held_verifications(),
        MAX_VERIFICATIONS,
        "the store holds one verification per recipient, got {}",
        emulator.held_verifications()
    );

    let refused = emulator
        .send(json!({
            "channel": "TEXT",
            "destinationIdentity": "+12065559999",
            "originationIdentity": SENDER,
            "overrideChannelParameters": text_channel_parameters(),
        }))
        .await;
    assert_eq!(
        refused.status, 400,
        "a send past the ceiling expected 400, got {} with {}",
        refused.status, refused.body
    );
    assert_eq!(
        refused.error.as_deref(),
        Some("ServiceQuotaExceededException"),
        "expected ServiceQuotaExceededException, got {:?} with {}",
        refused.error,
        refused.body
    );

    // A resend for a recipient already in flight replaces one record with another,
    // so it does not grow the store and is not refused at the ceiling.
    let resent = emulator
        .send(json!({
            "channel": "TEXT",
            "destinationIdentity": destinations.iter().next().expect("a recipient"),
            "originationIdentity": SENDER,
            "overrideChannelParameters": text_channel_parameters(),
        }))
        .await;
    assert_eq!(
        resent.status, 200,
        "a resend at the ceiling expected 200, got {} with {}",
        resent.status, resent.body
    );
    assert_eq!(
        emulator.held_verifications(),
        MAX_VERIFICATIONS,
        "a resend replaces the record in flight, got {} records",
        emulator.held_verifications()
    );
}

/// Each numeric passcode-policy member, resolved from a value outside the range the
/// model declares for it, stays inside that range: the wire refuses such a value at
/// the boundary (asserted above), and the resolved policy is clamped as well, so no
/// stored policy can size a passcode draw or a validity window past the modelled
/// bound.
#[test]
fn the_resolved_policy_stays_inside_the_modelled_ranges() {
    // One row per bound of each member: one value below it and one above it, plus
    // the bounds themselves, which must come through unchanged.
    let cases: Vec<(&str, CodeConfigurationParameters, i32, i32, i32)> = vec![
        (
            "every member far below its range",
            CodeConfigurationParameters {
                code_length: Some(i32::MIN),
                validity_period_minutes: Some(i32::MIN),
                max_attempts: Some(i32::MIN),
                ..CodeConfigurationParameters::default()
            },
            *CODE_LENGTH_RANGE.start(),
            *VALIDITY_PERIOD_MINUTES_RANGE.start(),
            *MAX_ATTEMPTS_RANGE.start(),
        ),
        (
            "every member far above its range",
            CodeConfigurationParameters {
                code_length: Some(i32::MAX),
                validity_period_minutes: Some(i32::MAX),
                max_attempts: Some(i32::MAX),
                ..CodeConfigurationParameters::default()
            },
            *CODE_LENGTH_RANGE.end(),
            *VALIDITY_PERIOD_MINUTES_RANGE.end(),
            *MAX_ATTEMPTS_RANGE.end(),
        ),
        (
            "every member one past its lower bound",
            CodeConfigurationParameters {
                code_length: Some(CODE_LENGTH_RANGE.start() - 1),
                validity_period_minutes: Some(VALIDITY_PERIOD_MINUTES_RANGE.start() - 1),
                max_attempts: Some(MAX_ATTEMPTS_RANGE.start() - 1),
                ..CodeConfigurationParameters::default()
            },
            *CODE_LENGTH_RANGE.start(),
            *VALIDITY_PERIOD_MINUTES_RANGE.start(),
            *MAX_ATTEMPTS_RANGE.start(),
        ),
        (
            "every member one past its upper bound",
            CodeConfigurationParameters {
                code_length: Some(CODE_LENGTH_RANGE.end() + 1),
                validity_period_minutes: Some(VALIDITY_PERIOD_MINUTES_RANGE.end() + 1),
                max_attempts: Some(MAX_ATTEMPTS_RANGE.end() + 1),
                ..CodeConfigurationParameters::default()
            },
            *CODE_LENGTH_RANGE.end(),
            *VALIDITY_PERIOD_MINUTES_RANGE.end(),
            *MAX_ATTEMPTS_RANGE.end(),
        ),
        (
            "every member at its lower bound comes through unchanged",
            CodeConfigurationParameters {
                code_length: Some(*CODE_LENGTH_RANGE.start()),
                validity_period_minutes: Some(*VALIDITY_PERIOD_MINUTES_RANGE.start()),
                max_attempts: Some(*MAX_ATTEMPTS_RANGE.start()),
                ..CodeConfigurationParameters::default()
            },
            *CODE_LENGTH_RANGE.start(),
            *VALIDITY_PERIOD_MINUTES_RANGE.start(),
            *MAX_ATTEMPTS_RANGE.start(),
        ),
        (
            "every member at its upper bound comes through unchanged",
            CodeConfigurationParameters {
                code_length: Some(*CODE_LENGTH_RANGE.end()),
                validity_period_minutes: Some(*VALIDITY_PERIOD_MINUTES_RANGE.end()),
                max_attempts: Some(*MAX_ATTEMPTS_RANGE.end()),
                ..CodeConfigurationParameters::default()
            },
            *CODE_LENGTH_RANGE.end(),
            *VALIDITY_PERIOD_MINUTES_RANGE.end(),
            *MAX_ATTEMPTS_RANGE.end(),
        ),
    ];

    for (case, parameters, length, minutes, attempts) in cases {
        // Once as the send's overrides, once as the referenced configuration's
        // parameters: both layers are clamped.
        for (layer, policy) in [
            ("overrides", CodePolicy::resolve(Some(&parameters), None)),
            ("configuration", CodePolicy::resolve(None, Some(&parameters))),
        ] {
            assert_eq!(
                policy.code_length, length,
                "case {case:?}/{layer}: expected codeLength {length}, got {}",
                policy.code_length
            );
            assert_eq!(
                policy.validity_period_minutes, minutes,
                "case {case:?}/{layer}: expected validityPeriodMinutes {minutes}, got {}",
                policy.validity_period_minutes
            );
            assert_eq!(
                policy.max_attempts, attempts,
                "case {case:?}/{layer}: expected maxAttempts {attempts}, got {}",
                policy.max_attempts
            );
            assert_eq!(
                policy.draw_code().chars().count(),
                usize::try_from(length).expect("a positive length"),
                "case {case:?}/{layer}: the draw takes its length from the clamped policy"
            );
            assert_eq!(
                policy.validity_period(),
                Duration::from_secs(u64::try_from(minutes).expect("a positive window") * 60),
                "case {case:?}/{layer}: the window follows the clamped policy, got {:?}",
                policy.validity_period()
            );
        }
    }
}

/// The invariant, over drawn policies and drawn submissions: a submission validates
/// exactly when it equals the passcode of the verification in flight for that
/// recipient and reference, inside the attempt ceiling; everything else is INVALID,
/// and a validated passcode never validates twice.
#[tokio::test]
async fn validation_matches_only_the_sent_passcode_property() {
    let seed: u64 = rand::random();
    // The seed, so a failing case can be replayed exactly.
    println!("notify code verification property seed: {seed}");
    let mut rng = StdRng::seed_from_u64(seed);
    /// Drawn cases: bounded, so the suite stays fast.
    const CASES: usize = 24;

    for case in 0..CASES {
        let emulator = Emulator::new();
        let code_type = ["NUMERIC", "ALPHA", "ALPHANUMERIC"][rng.random_range(0..3)];
        let code_length = rng.random_range(4..=8i32);
        let ceiling = rng.random_range(1..=5i32);
        let reference = if rng.random_bool(0.5) {
            Some(format!("reference-{case}"))
        } else {
            None
        };
        let destination = format!("+1206555{case:04}");
        let channel = ["TEXT", "VOICE", "WHATSAPP"][rng.random_range(0..3)];

        let mut body = json!({
            "channel": channel,
            "destinationIdentity": destination,
            "originationIdentity": SENDER,
            "overrideChannelParameters": text_channel_parameters(),
            "overrideCodeConfigurationParameters": {
                "codeType": code_type,
                "codeLength": code_length,
                "maxAttempts": ceiling,
            },
        });
        if let Some(reference) = reference.clone() {
            body["referenceId"] = json!(reference);
        }
        let sent = emulator.send(body).await;
        assert_eq!(
            sent.status, 200,
            "seed {seed}, case {case}: the send expected 200, got {} with {}",
            sent.status, sent.body
        );
        let id = sent.string("verificationId");
        let code = emulator.stored_or_panic(&format!("case {case}"), &id).code;
        assert_eq!(
            code.chars().count(),
            code_length as usize,
            "seed {seed}, case {case}: expected a {code_length}-character passcode, got {}",
            code.chars().count()
        );

        // Up to one fewer wrong submission than the ceiling, so the matching one is
        // still inside it.
        let wrong_submissions = rng.random_range(0..ceiling);
        for attempt in 0..wrong_submissions {
            let wrong = shifted_code(&code, attempt as usize);
            let answered = emulator
                .validate(validate_body(&destination, reference.as_deref(), &wrong))
                .await;
            assert_eq!(
                answered.string("status"),
                "INVALID",
                "seed {seed}, case {case}: {wrong:?} is not the passcode, got {}",
                answered.body
            );
        }
        let accepted = emulator
            .validate(validate_body(&destination, reference.as_deref(), &code))
            .await;
        assert_eq!(
            accepted.string("status"),
            "VALID",
            "seed {seed}, case {case}: the sent passcode inside the ceiling must validate, \
             got {}",
            accepted.body
        );
        let replayed = emulator
            .validate(validate_body(&destination, reference.as_deref(), &code))
            .await;
        assert_eq!(
            replayed.string("status"),
            "INVALID",
            "seed {seed}, case {case}: a validated passcode must not validate twice, got {}",
            replayed.body
        );
    }
}
