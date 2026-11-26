// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP coverage of `StartMeetingTranscription`.
//!
//! Every case drives the real wire path — `MultiServiceRouter::dispatch` over a restJson1
//! `POST /meetings/{MeetingId}/transcription?operation=start` signed for the `chime`
//! signing name — so what is asserted is what an AWS SDK client would see.
//!
//! The rule under test: the call PERSISTS the `TranscriptionConfiguration` it accepted on
//! the meeting it names, as the configuration in force for that meeting, replacing any
//! earlier one; the ONLY configuration it refuses is one that does not name exactly one
//! engine's settings block, with `BadRequestException` (HTTP 400), and a `MeetingId` naming
//! no live meeting is refused with `NotFoundException` (HTTP 404), and a refused call
//! changes nothing.
//!
//! How the parameters INSIDE one engine's settings block combine is not this API's
//! judgement: the model routes an invalid combination of them to a `TranscriptFailed` event
//! carrying the `BadRequestException` Amazon Transcribe generated, so such a configuration
//! is accepted and becomes the one in force like any other. That holds for what those
//! parameters SAY as much as for which of them are present: a `PreferredLanguage` the
//! request's `LanguageOptions` do not list, and `LanguageOptions` naming more than one
//! dialect of one language, are Amazon Transcribe's rules too — see
//! [`transcribe_language_content_rows`]. What the model constrains on a member itself (an
//! enum, a length, a pattern) is refused before the handler runs, which
//! [`the_modelled_constraints_on_the_language_members_are_refused`] pins from both sides of
//! each published bound.
//!
//! The accepted table spans the class the rule generalises over: which engine the
//! configuration names (Amazon Transcribe, Amazon Transcribe Medical), whether the
//! language is stated or identified, and which optional members ride along (Region,
//! vocabularies and filters in both their singular and plural spellings, content
//! identification, redaction, PII entity types, a custom language model, partial-results
//! stabilization, a medical specialty and type) — and, in
//! [`transcribe_combination_rows`], one row per parameter-combination rule stated in the
//! prose of `EngineTranscribeSettings`' members. The refused table carries the one rule
//! `TranscriptionConfiguration` itself states.
//!
//! The persisted record is read back from the shared engine the router hands the service,
//! because this model gives transcription no reporting operation: neither the `Meeting`
//! shape nor any output shape of the service carries a transcription member, so the state
//! this call writes is the only place its effect exists.

use std::sync::Arc;

use serde_json::{json, Map, Value};
use service_com_amazonaws_chimesdkmeetings_chimemeetingssdkservice::ops::types::TranscriptionConfiguration;
use service_com_amazonaws_chimesdkmeetings_chimemeetingssdkservice::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// A SigV4 `Authorization` header whose credential scope names the `chime` signing name
/// this service publishes (`OPERATIONS.md`, `## Wire protocol`). The signature itself is
/// never checked here, since the harness turns request authentication off.
const SIGNED_FOR_CHIME: &str = "AWS4-HMAC-SHA256 \
     Credential=AKIDTESTCHIME/20260101/us-east-1/chime/aws4_request, \
     SignedHeaders=host;x-amz-date, Signature=0000";

/// A well-formed `GuidString` (the modelled pattern
/// `^[a-fA-F0-9]{8}(?:-[a-fA-F0-9]{4}){3}-[a-fA-F0-9]{12}$`) that names no meeting, so the
/// request reaches the handler and is refused by it rather than by the boundary validator.
const UNKNOWN_MEETING_ID: &str = "00000000-0000-4000-8000-000000000000";

/// One response, reduced to what a client branches on.
#[derive(Debug)]
struct Reply {
    status: u16,
    error_code: Option<String>,
    body: Value,
}

impl Reply {
    /// The error message a refusal carries, or the empty string when it carries none.
    fn message(&self) -> String {
        self.body
            .get("Message")
            .or_else(|| self.body.get("message"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    }
}

/// The emulator under test: ONE shared engine with this service mounted on it, reached the
/// way the production router reaches it. The engine is kept so a test can read the record
/// the handler wrote, which no operation of this model reports.
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

    async fn call(&self, method: &str, path: &str, body: Option<&Value>) -> Reply {
        let payload = body.map_or_else(Vec::new, |value| {
            serde_json::to_vec(value).expect("a request body serializes")
        });
        let request = http::Request::builder()
            .method(method)
            .uri(format!("https://meetings-chime.us-east-1.amazonaws.com{path}"))
            .header("authorization", SIGNED_FOR_CHIME)
            .header("content-type", "application/json")
            .body(bytes::Bytes::from(payload))
            .expect("a well-formed request");

        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let header_code = response
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
        // restJson1 carries the wire error code in `x-amzn-errortype`; fall back to the
        // body's `__type` so a missing header never reads as "no error code".
        let error_code = header_code.or_else(|| {
            body.get("__type")
                .and_then(Value::as_str)
                .map(|wire| wire.rsplit('#').next().unwrap_or(wire).to_string())
        });
        Reply {
            status,
            error_code,
            body,
        }
    }

    /// A meeting to transcribe, created through the wire path like any other.
    async fn create_meeting(&self, token: &str, media_region: &str) -> String {
        let reply = self
            .call(
                "POST",
                "/meetings",
                Some(&json!({
                    "ClientRequestToken": token,
                    "MediaRegion": media_region,
                    "ExternalMeetingId": format!("ext-{token}"),
                })),
            )
            .await;
        assert_eq!(
            reply.status, 200,
            "creating the meeting expected 200, got {} with {:?}",
            reply.status, reply.body
        );
        reply
            .body
            .get("Meeting")
            .and_then(|meeting| meeting.get("MeetingId"))
            .and_then(Value::as_str)
            .expect("a created meeting reports its MeetingId")
            .to_string()
    }

    /// The operation under test, at the URI the model binds it to.
    async fn start_transcription(&self, meeting_id: &str, configuration: &Value) -> Reply {
        self.call(
            "POST",
            &format!("/meetings/{meeting_id}/transcription?operation=start"),
            Some(&json!({ "TranscriptionConfiguration": configuration })),
        )
        .await
    }

    async fn delete_meeting(&self, meeting_id: &str) -> Reply {
        self.call("DELETE", &format!("/meetings/{meeting_id}"), None)
            .await
    }

    async fn get_meeting(&self, meeting_id: &str) -> Reply {
        self.call("GET", &format!("/meetings/{meeting_id}"), None)
            .await
    }

    /// The configuration in force for a meeting, as the handler persisted it on the shared
    /// engine, rendered back into the JSON a request carries.
    fn transcription_of(&self, meeting_id: &str) -> Option<Value> {
        let state = service_com_amazonaws_chimesdkmeetings_chimemeetingssdkservice::state::service_state(
            &*self.engine,
        )
        .expect("this service's own store on the shared engine");
        let store = state.lock();
        store
            .meetings
            .get(meeting_id)
            .and_then(|stored| stored.transcription.as_ref())
            .map(stored_as_json)
    }
}

/// A stored `TranscriptionConfiguration`, rendered as the JSON a request carries: every
/// member the model declares on it, with the two `@default(false)` booleans of
/// `EngineTranscribeSettings` (`EnablePartialResultsStabilization`, `IdentifyLanguage`)
/// always present, since an absent one means `false` on the wire.
fn stored_as_json(configuration: &TranscriptionConfiguration) -> Value {
    let mut rendered = Map::new();
    if let Some(settings) = configuration.engine_transcribe_settings.as_ref() {
        let mut members = Map::new();
        let optional: [(&str, Option<&String>); 14] = [
            ("LanguageCode", settings.language_code.as_ref()),
            (
                "VocabularyFilterMethod",
                settings.vocabulary_filter_method.as_ref(),
            ),
            ("VocabularyFilterName", settings.vocabulary_filter_name.as_ref()),
            ("VocabularyName", settings.vocabulary_name.as_ref()),
            ("Region", settings.region.as_ref()),
            (
                "PartialResultsStability",
                settings.partial_results_stability.as_ref(),
            ),
            (
                "ContentIdentificationType",
                settings.content_identification_type.as_ref(),
            ),
            (
                "ContentRedactionType",
                settings.content_redaction_type.as_ref(),
            ),
            ("PiiEntityTypes", settings.pii_entity_types.as_ref()),
            ("LanguageModelName", settings.language_model_name.as_ref()),
            ("LanguageOptions", settings.language_options.as_ref()),
            ("PreferredLanguage", settings.preferred_language.as_ref()),
            ("VocabularyNames", settings.vocabulary_names.as_ref()),
            (
                "VocabularyFilterNames",
                settings.vocabulary_filter_names.as_ref(),
            ),
        ];
        for (member, value) in optional {
            if let Some(value) = value {
                members.insert(member.to_string(), json!(value));
            }
        }
        members.insert(
            "EnablePartialResultsStabilization".to_string(),
            json!(settings.enable_partial_results_stabilization),
        );
        members.insert(
            "IdentifyLanguage".to_string(),
            json!(settings.identify_language),
        );
        rendered.insert("EngineTranscribeSettings".to_string(), Value::Object(members));
    }
    if let Some(medical) = configuration.engine_transcribe_medical_settings.as_ref() {
        let mut members = Map::new();
        members.insert("LanguageCode".to_string(), json!(medical.language_code));
        members.insert("Specialty".to_string(), json!(medical.specialty));
        members.insert("Type".to_string(), json!(medical.r#type));
        let optional: [(&str, Option<&String>); 3] = [
            ("VocabularyName", medical.vocabulary_name.as_ref()),
            ("Region", medical.region.as_ref()),
            (
                "ContentIdentificationType",
                medical.content_identification_type.as_ref(),
            ),
        ];
        for (member, value) in optional {
            if let Some(value) = value {
                members.insert(member.to_string(), json!(value));
            }
        }
        rendered.insert(
            "EngineTranscribeMedicalSettings".to_string(),
            Value::Object(members),
        );
    }
    Value::Object(rendered)
}

/// The configuration a request sent, with those same two booleans made explicit, so the
/// two sides of the comparison differ only where the store lost or invented something.
fn as_persisted(sent: &Value) -> Value {
    let mut normalized = sent.clone();
    if let Some(settings) = normalized
        .get_mut("EngineTranscribeSettings")
        .and_then(Value::as_object_mut)
    {
        for boolean in ["EnablePartialResultsStabilization", "IdentifyLanguage"] {
            settings.entry(boolean.to_string()).or_insert(json!(false));
        }
    }
    normalized
}

/// One accepted configuration: the row name and the `TranscriptionConfiguration` sent.
struct Accepted {
    name: &'static str,
    configuration: Value,
}

/// The accepted rows, spanning the engines and the optional members of each.
///
/// Every enum value below is a member of the enum the model declares for that member
/// (`smithy_query(query="shape",shape="EngineTranscribeSettings")` and
/// `shape="EngineTranscribeMedicalSettings"`): `TranscribeLanguageCode`,
/// `TranscribeRegion`, `TranscribeVocabularyFilterMethod` (`remove | mask | tag`),
/// `TranscribePartialResultsStability` (`low | medium | high`),
/// `TranscribeContentIdentificationType` / `TranscribeContentRedactionType` (`PII`),
/// `TranscribeMedicalLanguageCode` (`en-US`), `TranscribeMedicalSpecialty`,
/// `TranscribeMedicalType` and `TranscribeMedicalContentIdentificationType` (`PHI`).
fn accepted_rows() -> Vec<Accepted> {
    vec![
        Accepted {
            name: "Amazon Transcribe, a stated language and nothing else",
            configuration: json!({ "EngineTranscribeSettings": { "LanguageCode": "en-US" } }),
        },
        Accepted {
            name: "a stated language with a Region, a vocabulary and a filter",
            configuration: json!({
                "EngineTranscribeSettings": {
                    "LanguageCode": "de-DE",
                    "Region": "eu-central-1",
                    "VocabularyName": "vocab-de",
                    "VocabularyFilterName": "filter-de",
                    "VocabularyFilterMethod": "mask",
                },
            }),
        },
        Accepted {
            name: "an identified language with its options, preference and plural vocabularies",
            configuration: json!({
                "EngineTranscribeSettings": {
                    "IdentifyLanguage": true,
                    "LanguageOptions": "en-US,fr-FR",
                    "PreferredLanguage": "fr-FR",
                    "VocabularyNames": "vocab-a,vocab-b",
                    "VocabularyFilterNames": "filter-a",
                    "VocabularyFilterMethod": "tag",
                },
            }),
        },
        Accepted {
            name: "content identification with PII entity types and stabilization",
            configuration: json!({
                "EngineTranscribeSettings": {
                    "LanguageCode": "en-GB",
                    "ContentIdentificationType": "PII",
                    "PiiEntityTypes": "NAME, EMAIL",
                    "EnablePartialResultsStabilization": true,
                    "PartialResultsStability": "high",
                },
            }),
        },
        Accepted {
            name: "redaction with a custom language model",
            configuration: json!({
                "EngineTranscribeSettings": {
                    "LanguageCode": "es-US",
                    "ContentRedactionType": "PII",
                    "LanguageModelName": "model-es",
                },
            }),
        },
        Accepted {
            name: "a language the other rows do not use, stated explicitly",
            configuration: json!({
                "EngineTranscribeSettings": {
                    "LanguageCode": "hi-IN",
                    "IdentifyLanguage": false,
                    "Region": "auto",
                },
            }),
        },
        Accepted {
            name: "Amazon Transcribe Medical, required members only",
            configuration: json!({
                "EngineTranscribeMedicalSettings": {
                    "LanguageCode": "en-US",
                    "Specialty": "PRIMARYCARE",
                    "Type": "CONVERSATION",
                },
            }),
        },
        Accepted {
            name: "Amazon Transcribe Medical, every member",
            configuration: json!({
                "EngineTranscribeMedicalSettings": {
                    "LanguageCode": "en-US",
                    "Specialty": "CARDIOLOGY",
                    "Type": "DICTATION",
                    "VocabularyName": "vocab-med",
                    "Region": "ca-central-1",
                    "ContentIdentificationType": "PHI",
                },
            }),
        },
    ]
}

/// One refused configuration: the row name, the configuration sent, and the members its
/// refusal has to name, so a client reading the message learns which rule it broke.
struct Refused {
    name: &'static str,
    configuration: Value,
    mentions: &'static [&'static str],
}

/// One row per half of the one rule `TranscriptionConfiguration` itself states
/// (`smithy_query(query="shape",shape="TranscriptionConfiguration")`: "Must contain
/// EngineTranscribeSettings or EngineTranscribeMedicalSettings.") — the exclusive choice
/// between the two engines' settings blocks, broken by naming neither or both.
fn refused_rows() -> Vec<Refused> {
    vec![
        Refused {
            name: "neither engine's settings",
            configuration: json!({}),
            mentions: &["EngineTranscribeSettings", "EngineTranscribeMedicalSettings"],
        },
        Refused {
            name: "both engines' settings",
            configuration: json!({
                "EngineTranscribeSettings": { "LanguageCode": "en-US" },
                "EngineTranscribeMedicalSettings": {
                    "LanguageCode": "en-US",
                    "Specialty": "PRIMARYCARE",
                    "Type": "CONVERSATION",
                },
            }),
            mentions: &["EngineTranscribeSettings", "EngineTranscribeMedicalSettings"],
        },
    ]
}

/// One row per parameter-combination rule stated in the prose of `EngineTranscribeSettings`'
/// members (`smithy_query(query="shape",shape="EngineTranscribeSettings")`): neither or both
/// of `LanguageCode`/`IdentifyLanguage`, `ContentIdentificationType` with
/// `ContentRedactionType`, `PiiEntityTypes` without either of them, `LanguageOptions`
/// without `IdentifyLanguage`, `PreferredLanguage` without `LanguageOptions`,
/// `IdentifyLanguage` with `LanguageModelName` or `ContentRedactionType`, and each
/// vocabulary member in the spelling the other language mode asks for.
///
/// Amazon Transcribe is the one that judges these, and the shape documents how: "If you
/// specify an invalid combination of parameters, a `TranscriptFailed` event will be sent
/// with the contents of the `BadRequestException` generated by Amazon Transcribe." So every
/// row here is ACCEPTED by this API and becomes the configuration in force.
fn transcribe_combination_rows() -> Vec<Accepted> {
    vec![
        Accepted {
            name: "neither a stated nor an identified language",
            configuration: json!({
                "EngineTranscribeSettings": { "Region": "us-east-1" },
            }),
        },
        Accepted {
            name: "both a stated and an identified language",
            configuration: json!({
                "EngineTranscribeSettings": {
                    "LanguageCode": "en-US",
                    "IdentifyLanguage": true,
                },
            }),
        },
        Accepted {
            name: "content identification together with redaction",
            configuration: json!({
                "EngineTranscribeSettings": {
                    "LanguageCode": "en-US",
                    "ContentIdentificationType": "PII",
                    "ContentRedactionType": "PII",
                },
            }),
        },
        Accepted {
            name: "PII entity types with neither identification nor redaction",
            configuration: json!({
                "EngineTranscribeSettings": {
                    "LanguageCode": "en-US",
                    "PiiEntityTypes": "ALL",
                },
            }),
        },
        Accepted {
            name: "language options without language identification",
            configuration: json!({
                "EngineTranscribeSettings": {
                    "LanguageCode": "en-US",
                    "LanguageOptions": "en-US,fr-FR",
                },
            }),
        },
        Accepted {
            name: "a preferred language without language options",
            configuration: json!({
                "EngineTranscribeSettings": {
                    "IdentifyLanguage": true,
                    "PreferredLanguage": "en-US",
                },
            }),
        },
        Accepted {
            name: "a preferred language without language identification",
            configuration: json!({
                "EngineTranscribeSettings": {
                    "LanguageCode": "en-US",
                    "PreferredLanguage": "en-US",
                },
            }),
        },
        Accepted {
            name: "language identification with a custom language model",
            configuration: json!({
                "EngineTranscribeSettings": {
                    "IdentifyLanguage": true,
                    "LanguageModelName": "model-en",
                },
            }),
        },
        Accepted {
            name: "language identification with redaction",
            configuration: json!({
                "EngineTranscribeSettings": {
                    "IdentifyLanguage": true,
                    "ContentRedactionType": "PII",
                },
            }),
        },
        Accepted {
            name: "language identification with the singular vocabulary member",
            configuration: json!({
                "EngineTranscribeSettings": {
                    "IdentifyLanguage": true,
                    "VocabularyName": "vocab-one",
                },
            }),
        },
        Accepted {
            name: "language identification with the singular filter member",
            configuration: json!({
                "EngineTranscribeSettings": {
                    "IdentifyLanguage": true,
                    "VocabularyFilterName": "filter-one",
                },
            }),
        },
        Accepted {
            name: "a stated language with the plural vocabulary member",
            configuration: json!({
                "EngineTranscribeSettings": {
                    "LanguageCode": "en-US",
                    "VocabularyNames": "vocab-a,vocab-b",
                },
            }),
        },
        Accepted {
            name: "a stated language with the plural filter member",
            configuration: json!({
                "EngineTranscribeSettings": {
                    "LanguageCode": "en-US",
                    "VocabularyFilterNames": "filter-a,filter-b",
                },
            }),
        },
        Accepted {
            name: "several rules broken at once, with both vocabulary spellings",
            configuration: json!({
                "EngineTranscribeSettings": {
                    "LanguageCode": "ko-KR",
                    "IdentifyLanguage": true,
                    "LanguageModelName": "model-ko",
                    "ContentRedactionType": "PII",
                    "ContentIdentificationType": "PII",
                    "VocabularyName": "vocab-one",
                    "VocabularyNames": "vocab-a,vocab-b",
                    "VocabularyFilterName": "filter-one",
                    "VocabularyFilterNames": "filter-a,filter-b",
                },
            }),
        },
        Accepted {
            name: "no parameter at all beyond the engine's own block",
            configuration: json!({ "EngineTranscribeSettings": {} }),
        },
    ]
}

/// An `EngineTranscribeSettings` block that identifies the language, carrying the language
/// set under test and nothing else, so each row below breaks ONLY the content rule it is
/// about and none of the presence rules.
fn identified_language(language_options: &str, preferred_language: Option<&str>) -> Value {
    let mut settings = Map::new();
    settings.insert("IdentifyLanguage".to_string(), json!(true));
    settings.insert("LanguageOptions".to_string(), json!(language_options));
    if let Some(preferred) = preferred_language {
        settings.insert("PreferredLanguage".to_string(), json!(preferred));
    }
    json!({ "EngineTranscribeSettings": Value::Object(settings) })
}

/// One row per rule the model states about the CONTENT of the two language members of
/// `EngineTranscribeSettings`, as opposed to their mere presence
/// (`smithy_query(query="shape",shape="EngineTranscribeSettings")`): `PreferredLanguage` is
/// "a preferred language from the subset of languages codes you specified in
/// `LanguageOptions`", and `LanguageOptions` "can only include one language dialect per
/// language. For example, you cannot include `en-US` and `en-AU`."
///
/// Both are Amazon Transcribe parameter rules, and the shape documents how every invalid
/// combination of its parameters is reported: "a `TranscriptFailed` event will be sent with
/// the contents of the `BadRequestException` generated by Amazon Transcribe". So every row
/// here is ACCEPTED by this API and becomes the configuration in force, exactly like the
/// presence rows above.
///
/// The codes are values of the model's `TranscribeLanguageCode` enum (same `smithy_query`:
/// `en-US`, `en-GB`, `es-US`, `fr-CA`, `fr-FR`, `en-AU`, `it-IT`, `de-DE`, `pt-BR`,
/// `ja-JP`, `ko-KR`, `zh-CN`, `th-TH`, `hi-IN`) — except in the row that deliberately lists
/// a code outside it, which `LanguageOptions` can carry because it is a plain string
/// constrained only by `length(1..200)` and `pattern(^[a-zA-Z-,]+$)`
/// (`smithy_query(query="shape",shape="TranscribeLanguageOptions")`).
fn transcribe_language_content_rows() -> Vec<Accepted> {
    vec![
        Accepted {
            name: "a preferred language the options do not list",
            configuration: identified_language("en-US,fr-FR", Some("de-DE")),
        },
        Accepted {
            name: "a preferred language that is another dialect of a listed language",
            configuration: identified_language("en-US,fr-FR", Some("en-AU")),
        },
        Accepted {
            name: "a preferred language outside a five-code list",
            configuration: identified_language("it-IT,ja-JP,ko-KR,th-TH,zh-CN", Some("hi-IN")),
        },
        Accepted {
            name: "a preferred language sharing no language with the listed ones",
            configuration: identified_language("pt-BR,es-US", Some("fr-CA")),
        },
        Accepted {
            name: "options naming two dialects of one language",
            configuration: identified_language("en-US,en-AU", None),
        },
        Accepted {
            name: "options naming two dialects of another language",
            configuration: identified_language("fr-FR,fr-CA", None),
        },
        Accepted {
            name: "options naming three dialects of one language",
            configuration: identified_language("en-US,en-GB,en-AU", None),
        },
        Accepted {
            name: "options naming one code twice",
            configuration: identified_language("en-US,en-US", None),
        },
        Accepted {
            name: "options naming two dialects, one of them preferred",
            configuration: identified_language("en-US,en-GB", Some("en-GB")),
        },
        Accepted {
            name: "both content rules broken at once",
            configuration: identified_language("en-US,en-AU", Some("de-DE")),
        },
        Accepted {
            name: "options naming a single language code",
            configuration: identified_language("ja-JP", Some("ja-JP")),
        },
        Accepted {
            name: "options naming a code the language-code enum does not carry",
            configuration: identified_language("en-US,xh-ZA", Some("en-US")),
        },
    ]
}

/// Every accepted configuration is persisted on the meeting it names, member for member,
/// and the meeting's own projection is untouched by it.
#[tokio::test]
async fn an_accepted_configuration_is_persisted_on_the_meeting() {
    let emulator = Emulator::new();

    for (index, row) in accepted_rows().into_iter().enumerate() {
        let Accepted { name, configuration } = row;
        // A meeting of its own per row, so what is read back is this row's write.
        let meeting_id = emulator
            .create_meeting(&format!("tok-accepted-{index}"), "us-east-1")
            .await;
        let before = emulator.get_meeting(&meeting_id).await;
        assert_eq!(
            emulator.transcription_of(&meeting_id),
            None,
            "case {name:?}: a new meeting carries no transcription configuration"
        );

        let reply = emulator.start_transcription(&meeting_id, &configuration).await;
        assert_eq!(
            (reply.status, reply.error_code.as_deref()),
            (200, None),
            "case {name:?}: expected HTTP 200 with no error code, got {} {:?} with {:?}",
            reply.status,
            reply.error_code,
            reply.body
        );

        let stored = emulator
            .transcription_of(&meeting_id)
            .unwrap_or_else(|| panic!("case {name:?}: the meeting is gone"));
        assert_eq!(
            stored,
            as_persisted(&configuration),
            "case {name:?}: expected the configuration the call accepted to be in force, got \
             {stored:?}"
        );

        // Transcription is not part of the meeting's projection: this model gives it no
        // member, so a reader of the meeting must see exactly what it saw before.
        let after = emulator.get_meeting(&meeting_id).await;
        assert_eq!(
            after.body, before.body,
            "case {name:?}: GetMeeting must report the same meeting as before the call"
        );
    }
}

/// A configuration that does not name exactly one engine's settings is refused with the
/// modelled `BadRequestException`, and the configuration already in force is left alone.
#[tokio::test]
async fn a_configuration_that_names_no_single_engine_is_refused() {
    let emulator = Emulator::new();
    let meeting_id = emulator.create_meeting("tok-refused", "us-west-2").await;

    // The meeting is already transcribing something, so each refusal is also a check that
    // nothing was overwritten.
    let in_force = json!({ "EngineTranscribeSettings": { "LanguageCode": "en-US" } });
    let accepted = emulator.start_transcription(&meeting_id, &in_force).await;
    assert_eq!(
        accepted.status, 200,
        "putting a configuration in force expected 200, got {} with {:?}",
        accepted.status, accepted.body
    );

    for row in refused_rows() {
        let Refused {
            name,
            configuration,
            mentions,
        } = row;
        let reply = emulator.start_transcription(&meeting_id, &configuration).await;
        assert_eq!(
            (reply.status, reply.error_code.as_deref()),
            (400, Some("BadRequestException")),
            "case {name:?}: expected HTTP 400 BadRequestException, got {} {:?} with {:?}",
            reply.status,
            reply.error_code,
            reply.body
        );
        let message = reply.message();
        for member in mentions {
            assert!(
                message.contains(member),
                "case {name:?}: expected the refusal to name {member:?}, got {message:?}"
            );
        }
        assert_eq!(
            emulator.transcription_of(&meeting_id),
            Some(as_persisted(&in_force)),
            "case {name:?}: a refused call must leave the configuration in force untouched"
        );
    }
}

/// Every parameter combination Amazon Transcribe itself rejects is nevertheless ACCEPTED by
/// this API and becomes the configuration in force for the meeting, replacing the one that
/// was in force before: the model reports such a combination as a `TranscriptFailed` event
/// on the transcription session, not as a failure of this call.
#[tokio::test]
async fn a_combination_amazon_transcribe_rejects_is_accepted_and_put_in_force() {
    let emulator = Emulator::new();
    let meeting_id = emulator.create_meeting("tok-combination", "us-west-2").await;

    // The meeting starts out transcribing a configuration every rule agrees with, so each
    // row below also shows the accepted one REPLACING what was in force.
    let valid = json!({ "EngineTranscribeSettings": { "LanguageCode": "en-US" } });
    let accepted = emulator.start_transcription(&meeting_id, &valid).await;
    assert_eq!(
        accepted.status, 200,
        "putting a configuration in force expected 200, got {} with {:?}",
        accepted.status, accepted.body
    );

    for row in transcribe_combination_rows() {
        let Accepted { name, configuration } = row;
        let reply = emulator.start_transcription(&meeting_id, &configuration).await;
        assert_eq!(
            (reply.status, reply.error_code.as_deref()),
            (200, None),
            "case {name:?}: expected HTTP 200 with no error code, got {} {:?} with {:?}",
            reply.status,
            reply.error_code,
            reply.body
        );
        let stored = emulator
            .transcription_of(&meeting_id)
            .unwrap_or_else(|| panic!("case {name:?}: the meeting is gone"));
        assert_eq!(
            stored,
            as_persisted(&configuration),
            "case {name:?}: expected the configuration the call accepted to be in force, got \
             {stored:?}"
        );

        // Back to the configuration every rule agrees with, so the next row's replacement
        // starts from a known one.
        let restored = emulator.start_transcription(&meeting_id, &valid).await;
        assert_eq!(
            restored.status, 200,
            "case {name:?}: restoring the valid configuration expected 200, got {} with {:?}",
            restored.status, restored.body
        );
    }
}

/// A language set Amazon Transcribe itself rejects — a `PreferredLanguage` the request's
/// `LanguageOptions` do not list, or `LanguageOptions` naming more than one dialect of one
/// language — is nevertheless ACCEPTED by this API and becomes the configuration in force,
/// replacing the one that was in force before: those are rules about this engine's own
/// parameters, which the model reports as a `TranscriptFailed` event on the transcription
/// session rather than as a failure of this call.
#[tokio::test]
async fn a_language_set_amazon_transcribe_rejects_is_accepted_and_put_in_force() {
    let emulator = Emulator::new();
    let meeting_id = emulator
        .create_meeting("tok-language-content", "eu-west-1")
        .await;

    // The meeting starts out transcribing a language set every rule agrees with — a
    // preferred language the options list, and two distinct languages — so each row below
    // also shows the accepted one REPLACING what was in force.
    let valid = identified_language("en-US,fr-FR", Some("fr-FR"));
    let accepted = emulator.start_transcription(&meeting_id, &valid).await;
    assert_eq!(
        accepted.status, 200,
        "putting a language set in force expected 200, got {} with {:?}",
        accepted.status, accepted.body
    );

    for row in transcribe_language_content_rows() {
        let Accepted { name, configuration } = row;
        let reply = emulator.start_transcription(&meeting_id, &configuration).await;
        assert_eq!(
            (reply.status, reply.error_code.as_deref()),
            (200, None),
            "case {name:?}: expected HTTP 200 with no error code, got {} {:?} with {:?}",
            reply.status,
            reply.error_code,
            reply.body
        );
        let stored = emulator
            .transcription_of(&meeting_id)
            .unwrap_or_else(|| panic!("case {name:?}: the meeting is gone"));
        assert_eq!(
            stored,
            as_persisted(&configuration),
            "case {name:?}: expected the configuration the call accepted to be in force, got \
             {stored:?}"
        );

        // Back to the language set every rule agrees with, so the next row's replacement
        // starts from a known one.
        let restored = emulator.start_transcription(&meeting_id, &valid).await;
        assert_eq!(
            restored.status, 200,
            "case {name:?}: restoring the valid language set expected 200, got {} with {:?}",
            restored.status, restored.body
        );
    }
}

/// Where the line runs: what the model constrains on the language members THEMSELVES — the
/// `TranscribeLanguageCode` enum behind `PreferredLanguage`, and the length and pattern of
/// `TranscribeLanguageOptions` (`length(1..200)`, `pattern(^[a-zA-Z-,]+$)`, from
/// `smithy_query(query="shape",shape="TranscribeLanguageOptions")`) — is refused with HTTP
/// 400 before the handler runs, naming the member it is about, with each published bound
/// tested from both sides, and a refused call leaves the configuration in force untouched.
#[tokio::test]
async fn the_modelled_constraints_on_the_language_members_are_refused() {
    let emulator = Emulator::new();
    let meeting_id = emulator
        .create_meeting("tok-language-constraints", "us-east-1")
        .await;

    let in_force = identified_language("en-US,fr-FR", Some("fr-FR"));
    let accepted = emulator.start_transcription(&meeting_id, &in_force).await;
    assert_eq!(
        accepted.status, 200,
        "putting a language set in force expected 200, got {} with {:?}",
        accepted.status, accepted.body
    );

    // Each bound of `TranscribeLanguageOptions`' length, from the inside: the shortest and
    // the longest value the model admits are accepted, whatever they say about languages.
    let at_minimum = "a".to_string();
    let at_maximum = "a".repeat(200);
    for (name, options) in [
        ("options at the modelled minimum length", at_minimum),
        ("options at the modelled maximum length", at_maximum),
    ] {
        let configuration = identified_language(&options, None);
        let reply = emulator.start_transcription(&meeting_id, &configuration).await;
        assert_eq!(
            (reply.status, reply.error_code.as_deref()),
            (200, None),
            "case {name:?}: expected HTTP 200 with no error code, got {} {:?} with {:?}",
            reply.status,
            reply.error_code,
            reply.body
        );
        let restored = emulator.start_transcription(&meeting_id, &in_force).await;
        assert_eq!(
            restored.status, 200,
            "case {name:?}: restoring the language set in force expected 200, got {} with {:?}",
            restored.status, restored.body
        );
    }

    // And from the outside: one character past each bound, a value the pattern forbids, and
    // a `PreferredLanguage` the enum does not carry. Each row names the member whose own
    // constraint it breaks, so the refusal is shown to be about THAT member — these are
    // refused by the boundary validator with the framework's `ValidationException`, which is
    // what tells them apart from the handler's own modelled refusals
    // (`BadRequestException` for the engine choice, `NotFoundException` for the meeting).
    let past_minimum = String::new();
    let past_maximum = "a".repeat(201);
    let refused: Vec<(&str, Value, &str)> = vec![
        (
            "options one character below the modelled minimum length",
            identified_language(&past_minimum, None),
            "languageOptions",
        ),
        (
            "options one character past the modelled maximum length",
            identified_language(&past_maximum, None),
            "languageOptions",
        ),
        (
            "options breaking the modelled pattern",
            identified_language("en_US,fr_FR", None),
            "languageOptions",
        ),
        (
            "a preferred language the language-code enum does not carry",
            identified_language("en-US,fr-FR", Some("xh-ZA")),
            "preferredLanguage",
        ),
    ];
    for (name, configuration, member) in refused {
        let reply = emulator.start_transcription(&meeting_id, &configuration).await;
        assert_eq!(
            (reply.status, reply.error_code.as_deref()),
            (400, Some("ValidationException")),
            "case {name:?}: expected the boundary validator's HTTP 400 ValidationException, \
             got {} {:?} with {:?}",
            reply.status,
            reply.error_code,
            reply.body
        );
        let message = reply.message();
        assert!(
            message.contains(member),
            "case {name:?}: expected the refusal to name the member {member:?}, got {message:?}"
        );
        assert_eq!(
            emulator.transcription_of(&meeting_id),
            Some(as_persisted(&in_force)),
            "case {name:?}: a refused call must leave the configuration in force untouched"
        );
    }
}

/// A `MeetingId` that names no live meeting is refused with the modelled
/// `NotFoundException`, worded for the meeting that was never created and for the one that
/// has ended.
#[tokio::test]
async fn a_meeting_id_that_names_no_live_meeting_is_refused() {
    let emulator = Emulator::new();
    let configuration = json!({ "EngineTranscribeSettings": { "LanguageCode": "en-US" } });

    // A meeting that IS transcribing, so each refusal below is also a check that a call
    // naming another id left this meeting's configuration in force alone.
    let witness_meeting_id = emulator.create_meeting("tok-witness", "us-east-1").await;
    let witnessed = emulator
        .start_transcription(&witness_meeting_id, &configuration)
        .await;
    assert_eq!(
        witnessed.status, 200,
        "putting a configuration in force expected 200, got {} with {:?}",
        witnessed.status, witnessed.body
    );

    // And a meeting that was transcribing and has since been deleted: its record goes
    // with it, which is what makes the refusal below about an ENDED meeting.
    let ended_meeting_id = emulator.create_meeting("tok-ended", "eu-west-1").await;
    let started = emulator
        .start_transcription(&ended_meeting_id, &configuration)
        .await;
    assert_eq!(
        started.status, 200,
        "putting a configuration in force expected 200, got {} with {:?}",
        started.status, started.body
    );
    assert_eq!(
        emulator.transcription_of(&ended_meeting_id),
        Some(as_persisted(&configuration)),
        "the meeting about to be deleted must be transcribing first"
    );
    let deleted = emulator.delete_meeting(&ended_meeting_id).await;
    assert_eq!(
        deleted.status, 204,
        "deleting the meeting expected 204, got {} with {:?}",
        deleted.status, deleted.body
    );
    assert_eq!(
        emulator.transcription_of(&ended_meeting_id),
        None,
        "a deleted meeting keeps no transcription configuration"
    );

    let cases: Vec<(&str, String, String)> = vec![
        (
            "an id that never named a meeting",
            UNKNOWN_MEETING_ID.to_string(),
            format!("Meeting <{UNKNOWN_MEETING_ID}> not found"),
        ),
        (
            "an id whose meeting has ended",
            ended_meeting_id.clone(),
            "The meeting has ended".to_string(),
        ),
    ];

    for (name, meeting_id, expected_message) in cases {
        let reply = emulator.start_transcription(&meeting_id, &configuration).await;
        assert_eq!(
            (reply.status, reply.error_code.as_deref()),
            (404, Some("NotFoundException")),
            "case {name:?}: expected HTTP 404 NotFoundException, got {} {:?} with {:?}",
            reply.status,
            reply.error_code,
            reply.body
        );
        assert_eq!(
            reply.message(),
            expected_message,
            "case {name:?}: expected the refusal the service words for this case, got {:?}",
            reply.message()
        );
        assert_eq!(
            emulator.transcription_of(&meeting_id),
            None,
            "case {name:?}: a refused call must create no record for the id it named"
        );
        assert_eq!(
            emulator.transcription_of(&witness_meeting_id),
            Some(as_persisted(&configuration)),
            "case {name:?}: a refused call must leave another meeting's configuration alone"
        );
    }
}

/// The configuration in force is the one the LAST accepted call named: starting
/// transcription again replaces it rather than keeping the first or holding both.
#[tokio::test]
async fn starting_transcription_again_replaces_the_configuration_in_force() {
    let emulator = Emulator::new();
    let meeting_id = emulator.create_meeting("tok-replaced", "us-east-2").await;

    // The sequence spans the kinds of change a restart can make: another language, a move
    // to the other engine, and back again with different optional members.
    let sequence: Vec<(&str, Value)> = vec![
        (
            "a stated language",
            json!({ "EngineTranscribeSettings": { "LanguageCode": "en-US" } }),
        ),
        (
            "another stated language and Region",
            json!({
                "EngineTranscribeSettings": { "LanguageCode": "ja-JP", "Region": "ap-northeast-1" },
            }),
        ),
        (
            "the medical engine",
            json!({
                "EngineTranscribeMedicalSettings": {
                    "LanguageCode": "en-US",
                    "Specialty": "ONCOLOGY",
                    "Type": "DICTATION",
                },
            }),
        ),
        (
            "an identified language",
            json!({
                "EngineTranscribeSettings": {
                    "IdentifyLanguage": true,
                    "LanguageOptions": "en-US,pt-BR",
                },
            }),
        ),
    ];

    for (name, configuration) in sequence {
        let reply = emulator.start_transcription(&meeting_id, &configuration).await;
        assert_eq!(
            (reply.status, reply.error_code.as_deref()),
            (200, None),
            "case {name:?}: expected HTTP 200 with no error code, got {} {:?} with {:?}",
            reply.status,
            reply.error_code,
            reply.body
        );
        let stored = emulator
            .transcription_of(&meeting_id)
            .unwrap_or_else(|| panic!("case {name:?}: the meeting is gone"));
        assert_eq!(
            stored,
            as_persisted(&configuration),
            "case {name:?}: the last accepted configuration is the one in force, got {stored:?}"
        );
    }

    // And the record belongs to the meeting: deleting the meeting takes it with it.
    let deleted = emulator.delete_meeting(&meeting_id).await;
    assert_eq!(
        deleted.status, 204,
        "deleting the meeting expected 204, got {} with {:?}",
        deleted.status, deleted.body
    );
    assert_eq!(
        emulator.transcription_of(&meeting_id),
        None,
        "a deleted meeting keeps no transcription configuration"
    );
}
