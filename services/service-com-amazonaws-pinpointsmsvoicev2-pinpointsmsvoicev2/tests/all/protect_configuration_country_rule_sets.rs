// SPDX-License-Identifier: CC0-1.0

//! A protect configuration's country rule sets, end to end over the wire.
//!
//! Every request goes through [`MultiServiceRouter::dispatch`] as `awsJson1_0` — `POST /`
//! with `X-Amz-Target: PinpointSMSVoiceV2.<Op>` — so these tests exercise routing,
//! deserialization, constraint validation, the handler and serialization, exactly as a
//! real SDK client would reach them.
//!
//! The rules under test:
//!
//! * a protect configuration holds one `ProtectStatus` per destination country per
//!   `NumberCapability`; a configuration no update has touched reports the documented
//!   default status (`ALLOW`) for every country, and a read reports the whole rule set,
//!   whichever capability is asked for;
//! * an update writes the status it names for each country it names, under the ONE
//!   capability it names, and changes nothing else: no other country, no other capability
//!   and no other configuration;
//! * a read reports what was written and nothing else changes it: the update's own
//!   response and every later read of that capability are the same rule set;
//! * a country code that names no country — one the published ISO 3166-1 table leaves
//!   unassigned, or a string that is not two upper-case letters — is refused in this
//!   service's modelled `ValidationException` and writes nothing;
//! * an id that names no stored configuration is refused as `ResourceNotFoundException`,
//!   in either operation and whether the request names the id or the ARN.
//!
//! Its harness is this suite's own: the sibling suites' helpers are private to their
//! modules, and a test target shares nothing it does not export.

use std::sync::Arc;

use service_com_amazonaws_pinpointsmsvoicev2_pinpointsmsvoicev2 as sms_voice;
use sms_voice::ops::types::{NUMBER_CAPABILITY_VALUES, PROTECT_STATUS_VALUES};
use sms_voice::state::DEFAULT_PROTECT_STATUS;
use smithy_server_runtime::MultiServiceRouter;

/// The region these tests sign for, so the ARNs under assertion are not the default one.
const REGION: &str = "us-west-2";

/// Countries the rule set must cover, drawn from the published ISO 3166-1 alpha-2 table
/// the rule set is keyed by (`smithy_query(query="shape",shape="IsoCountryCode")` names
/// that table: "the two-letter ISO country code"). Spread across the alphabet and
/// including entries no recorded request names.
const SOME_ASSIGNED_COUNTRIES: &[&str] = &["AQ", "BR", "CA", "DE", "IN", "JP", "NZ", "US", "ZA"];

/// Two-letter codes the same table leaves UNASSIGNED (the user-assigned ranges `AA`,
/// `QM`–`QZ`, `XA`–`XZ`, `ZZ`), so each satisfies the modelled `^[A-Z]{2}$` pattern while
/// naming no country.
const SOME_UNASSIGNED_CODES: &[&str] = &["AA", "QQ", "XB", "ZZ"];

/// Strings that are not two upper-case letters at all, so none of them can name a country
/// either: the wrong length, the wrong case, a digit.
const SOME_MALFORMED_CODES: &[&str] = &["U", "usa", "us", "U1"];

/// The sentence this service refuses a parameter it cannot use the value of with: a
/// restatement of the exception's own `Reason` and `Fields`, as its conflict and
/// not-found messages restate theirs.
const REFUSAL_SENTENCE: &str =
    "Validation Error Occurred - Reason=\"INVALID_PARAMETER\" Fields=\"countryRuleSetUpdates\"";

/// One emulator: the shared engine plus a router with only this service mounted.
struct Emulator {
    router: MultiServiceRouter,
}

impl Emulator {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        // These tests assert behaviour, not signatures; the emulator verifies signatures
        // itself and a service never checks them.
        router.set_request_authentication(false);
        router.register_service(sms_voice::EmulatedService::new(engine));
        Self { router }
    }

    /// Call one operation and answer `(http status, parsed JSON body)`.
    async fn call(&self, operation: &str, body: serde_json::Value) -> (u16, serde_json::Value) {
        let request = http::Request::builder()
            .method("POST")
            .uri("/")
            .header("content-type", "application/x-amz-json-1.0")
            .header("x-amz-target", format!("PinpointSMSVoiceV2.{operation}"))
            // The SigV4 credential scope is what routes the request to this service and
            // what names the region its ARNs carry.
            .header(
                "authorization",
                format!(
                    "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/sms-voice/aws4_request, \
                     SignedHeaders=host;x-amz-date, Signature=0123456789abcdef"
                ),
            )
            .body(bytes::Bytes::from(
                serde_json::to_vec(&body).expect("the request body serializes"),
            ))
            .expect("a valid request");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let parsed = if response.body().is_empty() {
            serde_json::Value::Null
        } else {
            serde_json::from_slice(response.body()).unwrap_or(serde_json::Value::Null)
        };
        (status, parsed)
    }

    /// Call one operation and require HTTP 200, reporting the body when it is not.
    async fn ok(&self, operation: &str, body: serde_json::Value) -> serde_json::Value {
        let (status, parsed) = self.call(operation, body).await;
        assert_eq!(
            status, 200,
            "{operation}: expected HTTP 200, got {status} with {parsed}"
        );
        parsed
    }

    /// Create one protect configuration and answer its `ProtectConfigurationId`.
    async fn create_protect_configuration(&self) -> String {
        let created = self
            .ok("CreateProtectConfiguration", serde_json::json!({}))
            .await;
        member(&created, "ProtectConfigurationId")
    }

    /// The whole rule set one capability reports, as `(country, ProtectStatus)` pairs.
    async fn rule_set(&self, id: &str, capability: &str) -> Vec<(String, String)> {
        let answered = self
            .ok(
                "GetProtectConfigurationCountryRuleSet",
                serde_json::json!({
                    "ProtectConfigurationId": id,
                    "NumberCapability": capability,
                }),
            )
            .await;
        assert_eq!(
            member(&answered, "NumberCapability"),
            capability,
            "case {capability:?}: the read reports the capability it was asked for, got {answered}"
        );
        country_rule_set(&answered)
    }
}

/// The wire error code of a failed call (`__type`, with any namespace prefix dropped).
fn error_code(body: &serde_json::Value) -> String {
    let raw = body
        .get("__type")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("<no __type>");
    raw.rsplit('#').next().unwrap_or(raw).to_string()
}

fn member(body: &serde_json::Value, name: &str) -> String {
    body.get(name)
        .and_then(serde_json::Value::as_str)
        .unwrap_or_else(|| panic!("expected a string member {name} in {body}"))
        .to_string()
}

/// A response's `CountryRuleSet` as `(country, ProtectStatus)` pairs, in country order.
fn country_rule_set(body: &serde_json::Value) -> Vec<(String, String)> {
    let map = body
        .get("CountryRuleSet")
        .and_then(serde_json::Value::as_object)
        .unwrap_or_else(|| panic!("expected a CountryRuleSet map in {body}"));
    let mut pairs: Vec<(String, String)> = map
        .iter()
        .map(|(country, information)| {
            (
                country.clone(),
                member(information, "ProtectStatus"),
            )
        })
        .collect();
    pairs.sort();
    pairs
}

/// The status one rule set holds for one country.
fn status_of(rule_set: &[(String, String)], country: &str) -> String {
    rule_set
        .iter()
        .find(|(name, _)| name == country)
        .map(|(_, status)| status.clone())
        .unwrap_or_else(|| panic!("expected {country} in the rule set, got {rule_set:?}"))
}

/// A configuration no update has touched reports the documented default status for every
/// country it covers, for EVERY capability the model declares, and a read changes nothing.
#[tokio::test]
async fn a_fresh_configuration_reports_the_default_status_for_every_country() {
    let emulator = Emulator::new();
    let id = emulator.create_protect_configuration().await;
    for capability in NUMBER_CAPABILITY_VALUES {
        let rule_set = emulator.rule_set(&id, capability).await;
        // The map is modelled `len 1..300`
        // (`smithy_query(query="shape",shape="ProtectConfigurationCountryRuleSet")`).
        assert!(
            (1..=300).contains(&rule_set.len()),
            "case {capability:?}: the rule set must hold between 1 and 300 countries, got {}",
            rule_set.len()
        );
        assert!(
            rule_set
                .iter()
                .all(|(country, _)| country.len() == 2
                    && country.chars().all(|letter| letter.is_ascii_uppercase())),
            "case {capability:?}: every key is a two-letter country code, got {rule_set:?}"
        );
        for country in SOME_ASSIGNED_COUNTRIES {
            assert_eq!(
                status_of(&rule_set, country),
                DEFAULT_PROTECT_STATUS,
                "case {capability:?}/{country}: an untouched configuration is at the default status"
            );
        }
        for code in SOME_UNASSIGNED_CODES {
            assert!(
                !rule_set.iter().any(|(name, _)| name == code),
                "case {capability:?}/{code}: an unassigned code names no country, so it is not in the rule set"
            );
        }
        // A read is a read: asking twice answers the same rule set.
        assert_eq!(
            emulator.rule_set(&id, capability).await,
            rule_set,
            "case {capability:?}: a read must not change what a later read sees"
        );
    }
}

/// An update writes the status it names for the countries it names, under the one
/// capability it names: the written countries carry the new status, every other country
/// keeps the default, and the other capabilities are untouched.
///
/// The rows differ along what the rule generalises over: the `ProtectStatus` written
/// (every value `PROTECT_STATUS_VALUES` lists), how many countries one request carries,
/// and which capability it applies to.
#[tokio::test]
async fn an_update_writes_one_capabilitys_countries_and_leaves_the_rest_alone() {
    let emulator = Emulator::new();
    let cases: Vec<(&str, &str, Vec<&str>)> = vec![
        ("SMS", "BLOCK", vec!["CA"]),
        ("VOICE", "MONITOR", vec!["DE", "JP", "BR"]),
        ("MMS", "FILTER", vec!["NZ", "ZA"]),
        ("RCS", "ALLOW", vec!["IN"]),
        ("SMS", "FILTER", vec!["AQ", "US", "DE", "IN"]),
    ];
    for (capability, status, countries) in &cases {
        let case = format!("{capability}/{status}/{countries:?}");
        // A fresh configuration per row, so each row is judged against the default state.
        let id = emulator.create_protect_configuration().await;
        let updates: serde_json::Map<String, serde_json::Value> = countries
            .iter()
            .map(|country| {
                (
                    (*country).to_string(),
                    serde_json::json!({"ProtectStatus": status}),
                )
            })
            .collect();
        let updated = emulator
            .ok(
                "UpdateProtectConfigurationCountryRuleSet",
                serde_json::json!({
                    "ProtectConfigurationId": id,
                    "NumberCapability": capability,
                    "CountryRuleSetUpdates": updates,
                }),
            )
            .await;
        assert_eq!(
            member(&updated, "ProtectConfigurationId"),
            id,
            "case {case}: the update answers for the configuration it wrote, got {updated}"
        );
        assert_eq!(
            member(&updated, "NumberCapability"),
            *capability,
            "case {case}: the update answers for the capability it wrote, got {updated}"
        );

        // What the update answered is what a later read reports: the response comes out
        // of the record, not out of the request.
        let read_back = emulator.rule_set(&id, capability).await;
        assert_eq!(
            country_rule_set(&updated),
            read_back,
            "case {case}: the update's own rule set is the one a later read reports"
        );

        for (country, status_read) in &read_back {
            let expected = if countries.contains(&country.as_str()) {
                *status
            } else {
                DEFAULT_PROTECT_STATUS
            };
            assert_eq!(
                status_read.as_str(),
                expected,
                "case {case}: {country} must be {expected}, got {status_read}"
            );
        }

        // The update named ONE capability, so the others still report the default
        // everywhere.
        for other in NUMBER_CAPABILITY_VALUES
            .iter()
            .filter(|other| *other != capability)
        {
            let untouched = emulator.rule_set(&id, other).await;
            for country in countries {
                assert_eq!(
                    status_of(&untouched, country),
                    DEFAULT_PROTECT_STATUS,
                    "case {case}: {other} must not see the {capability} update of {country}"
                );
            }
        }
    }
}

/// Repeated updates accumulate on the stored rule set, and the status last written for a
/// country is the one reported — whichever of the modelled statuses it is.
#[tokio::test]
async fn the_status_last_written_for_a_country_is_the_one_reported() {
    let emulator = Emulator::new();
    let id = emulator.create_protect_configuration().await;
    // Every value the model declares, applied in turn to one country, with a second
    // country written once and never again.
    emulator
        .ok(
            "UpdateProtectConfigurationCountryRuleSet",
            serde_json::json!({
                "ProtectConfigurationId": id,
                "NumberCapability": "SMS",
                "CountryRuleSetUpdates": {"BR": {"ProtectStatus": "MONITOR"}},
            }),
        )
        .await;
    for status in PROTECT_STATUS_VALUES {
        emulator
            .ok(
                "UpdateProtectConfigurationCountryRuleSet",
                serde_json::json!({
                    "ProtectConfigurationId": id,
                    "NumberCapability": "SMS",
                    "CountryRuleSetUpdates": {"US": {"ProtectStatus": status}},
                }),
            )
            .await;
        let rule_set = emulator.rule_set(&id, "SMS").await;
        assert_eq!(
            status_of(&rule_set, "US"),
            *status,
            "case {status}: the status last written is the one reported"
        );
        assert_eq!(
            status_of(&rule_set, "BR"),
            "MONITOR",
            "case {status}: a country this request did not name keeps its stored status"
        );
    }
}

/// Either form of `ProtectConfigurationId` names the resource — the id or the ARN — and
/// both report the stored identity back.
#[tokio::test]
async fn the_id_or_the_arn_names_the_configuration() {
    let emulator = Emulator::new();
    let created = emulator
        .ok("CreateProtectConfiguration", serde_json::json!({}))
        .await;
    let id = member(&created, "ProtectConfigurationId");
    let arn = member(&created, "ProtectConfigurationArn");
    emulator
        .ok(
            "UpdateProtectConfigurationCountryRuleSet",
            serde_json::json!({
                "ProtectConfigurationId": arn,
                "NumberCapability": "SMS",
                "CountryRuleSetUpdates": {"CA": {"ProtectStatus": "BLOCK"}},
            }),
        )
        .await;
    for named in [&id, &arn] {
        let answered = emulator
            .ok(
                "GetProtectConfigurationCountryRuleSet",
                serde_json::json!({
                    "ProtectConfigurationId": named,
                    "NumberCapability": "SMS",
                }),
            )
            .await;
        assert_eq!(
            member(&answered, "ProtectConfigurationId"),
            id,
            "case {named:?}: the stored id is reported, got {answered}"
        );
        assert_eq!(
            member(&answered, "ProtectConfigurationArn"),
            arn,
            "case {named:?}: the stored ARN is reported, got {answered}"
        );
        assert_eq!(
            status_of(&country_rule_set(&answered), "CA"),
            "BLOCK",
            "case {named:?}: the written status is reported, got {answered}"
        );
    }
}

/// A country code that names no country is refused in this service's modelled
/// `ValidationException`, and the whole update is refused: nothing it carried is written.
///
/// The rows differ along the FORM of the unusable code: a well-formed two-letter code the
/// published table leaves unassigned, and a string that is not two upper-case letters at
/// all.
#[tokio::test]
async fn a_country_code_that_names_no_country_is_refused_and_writes_nothing() {
    let emulator = Emulator::new();
    let id = emulator.create_protect_configuration().await;
    let unusable: Vec<&str> = SOME_UNASSIGNED_CODES
        .iter()
        .chain(SOME_MALFORMED_CODES)
        .copied()
        .collect();
    for code in unusable {
        // One assigned country beside the unassigned code, so the refusal is observable
        // as "nothing was written" rather than "nothing to write".
        let mut updates = serde_json::Map::new();
        updates.insert(
            "US".to_string(),
            serde_json::json!({"ProtectStatus": "BLOCK"}),
        );
        updates.insert(
            code.to_string(),
            serde_json::json!({"ProtectStatus": "BLOCK"}),
        );
        let (status, body) = emulator
            .call(
                "UpdateProtectConfigurationCountryRuleSet",
                serde_json::json!({
                    "ProtectConfigurationId": id,
                    "NumberCapability": "SMS",
                    "CountryRuleSetUpdates": updates,
                }),
            )
            .await;
        assert_eq!(
            status, 400,
            "case {code}: expected HTTP 400, got {status} with {body}"
        );
        assert_eq!(
            error_code(&body),
            "ValidationException",
            "case {code}: expected the modelled error code, got {body}"
        );
        assert_eq!(
            member(&body, "Message"),
            REFUSAL_SENTENCE,
            "case {code}: got {body}"
        );
        assert_eq!(
            member(&body, "Reason"),
            "INVALID_PARAMETER",
            "case {code}: got {body}"
        );
        let rule_set = emulator.rule_set(&id, "SMS").await;
        assert_eq!(
            status_of(&rule_set, "US"),
            DEFAULT_PROTECT_STATUS,
            "case {code}: a refused update writes none of the countries it carried"
        );
    }
    // The control: the same request with only assigned countries is served.
    emulator
        .ok(
            "UpdateProtectConfigurationCountryRuleSet",
            serde_json::json!({
                "ProtectConfigurationId": id,
                "NumberCapability": "SMS",
                "CountryRuleSetUpdates": {"US": {"ProtectStatus": "BLOCK"}},
            }),
        )
        .await;
    assert_eq!(
        status_of(&emulator.rule_set(&id, "SMS").await, "US"),
        "BLOCK",
        "a request whose every country is assigned is served"
    );
}

/// An id that names no stored configuration is refused as `ResourceNotFoundException`, in
/// both operations and whether the request names an id or an ARN.
#[tokio::test]
async fn an_unknown_configuration_is_not_found() {
    let emulator = Emulator::new();
    // A created configuration, so the store is not empty and the refusal is about the id
    // the request named rather than about an empty store.
    let _ = emulator.create_protect_configuration().await;
    let named = [
        "protect-configuration-00000000000000000000000000000000",
        "arn:aws:sms-voice:us-west-2:000000000000:protect-configuration/protect-configuration-ffffffffffffffffffffffffffffffff",
    ];
    for name in named {
        let requests = [
            (
                "GetProtectConfigurationCountryRuleSet",
                serde_json::json!({
                    "ProtectConfigurationId": name,
                    "NumberCapability": "SMS",
                }),
            ),
            (
                "UpdateProtectConfigurationCountryRuleSet",
                serde_json::json!({
                    "ProtectConfigurationId": name,
                    "NumberCapability": "SMS",
                    "CountryRuleSetUpdates": {"US": {"ProtectStatus": "BLOCK"}},
                }),
            ),
        ];
        for (operation, body) in requests {
            let (status, answered) = emulator.call(operation, body).await;
            let case = format!("{operation}/{name}");
            assert_eq!(
                status, 400,
                "case {case}: expected HTTP 400, got {status} with {answered}"
            );
            assert_eq!(
                error_code(&answered),
                "ResourceNotFoundException",
                "case {case}: got {answered}"
            );
            assert_eq!(
                member(&answered, "ResourceType"),
                "protect-configuration",
                "case {case}: got {answered}"
            );
        }
    }
}

/// One configuration's rule set is its own: an update to one leaves every other
/// configuration's rule sets as they were.
#[tokio::test]
async fn one_configurations_rule_set_does_not_leak_into_another() {
    let emulator = Emulator::new();
    let first = emulator.create_protect_configuration().await;
    let second = emulator.create_protect_configuration().await;
    emulator
        .ok(
            "UpdateProtectConfigurationCountryRuleSet",
            serde_json::json!({
                "ProtectConfigurationId": first,
                "NumberCapability": "SMS",
                "CountryRuleSetUpdates": {"US": {"ProtectStatus": "BLOCK"}},
            }),
        )
        .await;
    assert_eq!(
        status_of(&emulator.rule_set(&first, "SMS").await, "US"),
        "BLOCK",
        "the configuration that was updated holds the written status"
    );
    assert_eq!(
        status_of(&emulator.rule_set(&second, "SMS").await, "US"),
        DEFAULT_PROTECT_STATUS,
        "a configuration no update named keeps the default status"
    );
}
