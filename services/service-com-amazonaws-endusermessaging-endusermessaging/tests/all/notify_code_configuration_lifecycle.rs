// SPDX-License-Identifier: CC0-1.0

//! The notify-code-configuration lifecycle, end to end over the wire.
//!
//! Every assertion here goes through `MultiServiceRouter::dispatch`, so it exercises
//! the same restJson1 path a real SDK client takes: routing, deserialization,
//! boundary constraint validation, the handler, and error rendering.
//!
//! The rules under test, each over a SET of inputs rather than one recorded request:
//!
//! * `CreateNotifyCodeConfiguration` persists the resource and reports it read back
//!   off the stored record, and `GetNotifyCodeConfiguration` reports the same record
//!   member for member — for every name the modelled `NotifyCodeConfigurationName`
//!   accepts, with or without `deletionProtectionEnabled` and `tags`;
//! * the passcode policy and the channel parameters are stored exactly as supplied:
//!   a member the request carries is reported back, a member it omits stays absent;
//! * a `NotifyCodeConfigurationIdOrArn` resolves a configuration by its bare id or
//!   by its full ARN, and the ARN is
//!   `arn:aws:end-user-messaging:<region>:<account>:notify-code-configuration/<id>`
//!   for whatever region the request is signed for;
//! * an identifier that names nothing is `ResourceNotFoundException` (404) carrying
//!   this error class's templated sentence, and a value outside a modelled
//!   constraint is `ValidationException` (400);
//! * a repeated `clientToken` reports the first configuration instead of creating
//!   another;
//! * the bounded store refuses a create past its ceiling with
//!   `ServiceQuotaExceededException` rather than growing without bound.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Map, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_endusermessaging_endusermessaging::EmulatedService;

/// This service's sigv4 signing name, from the model (`OPERATIONS.md`, "Wire protocol").
const SIGNING_NAME: &str = "end-user-messaging";

/// The account id every generated emulator presents, as `engine` defines it.
const ACCOUNT_ID: &str = engine::EMULATOR_ACCOUNT_ID;

/// The hyphenated resource type a notify-code-configuration refusal reports, as the
/// service exports it for its handlers.
const CONFIGURATION_TYPE: &str =
    service_com_amazonaws_endusermessaging_endusermessaging::state::NOTIFY_CODE_CONFIGURATION_RESOURCE_TYPE;

/// Every member of the modelled `NotifyCodeConfiguration` resource.
const RESOURCE_MEMBERS: &[&str] = &[
    "notifyCodeConfigurationId",
    "notifyCodeConfigurationArn",
    "notifyCodeConfigurationName",
    "codeConfigurationParameters",
    "channelParameters",
    "deletionProtectionEnabled",
    "createdAt",
    "updatedAt",
];

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
    /// The `notifyCodeConfiguration` the output carries.
    fn resource(&self) -> &Value {
        let resource = &self.body["notifyCodeConfiguration"];
        assert!(
            resource.is_object(),
            "notifyCodeConfiguration is not an object in {}",
            self.body
        );
        resource
    }

    fn member(&self, name: &str) -> &Value {
        &self.resource()[name]
    }

    fn string(&self, name: &str) -> String {
        self.member(name)
            .as_str()
            .unwrap_or_else(|| panic!("member {name} is not a string in {}", self.body))
            .to_string()
    }

    /// A top-level body member, for an error body.
    fn top(&self, name: &str) -> String {
        self.body[name]
            .as_str()
            .unwrap_or_else(|| panic!("member {name} is not a string in {}", self.body))
            .to_string()
    }
}

/// One emulator over one shared engine: every call sees the same store.
struct Emulator {
    router: MultiServiceRouter,
    region: String,
}

impl Emulator {
    fn in_region(region: &str) -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        router.set_request_authentication(false);
        router.register_service(EmulatedService::new(engine));
        Self {
            router,
            region: region.to_string(),
        }
    }

    fn new() -> Self {
        Self::in_region("us-east-1")
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

    async fn create(&self, body: Value) -> Reply {
        self.call("POST", "/v1/notify-code-configurations", Some(body))
            .await
    }

    async fn create_named(&self, name: &str) -> Reply {
        self.create(json!({ "notifyCodeConfigurationName": name }))
            .await
    }

    async fn get(&self, id_or_arn: &str) -> Reply {
        self.call(
            "GET",
            &format!("/v1/notify-code-configurations/{id_or_arn}"),
            None,
        )
        .await
    }
}

/// The names in this table are the classes `NotifyCodeConfigurationName`'s modelled
/// `@length(1..64)` and `^[A-Za-z0-9_ -]*[A-Za-z0-9_-][A-Za-z0-9_ -]*$` pattern
/// accept: a single character, the alphanumeric case, every punctuation the pattern
/// names, an interior space, and exactly the maximum length.
fn accepted_names() -> Vec<String> {
    vec![
        "a".to_string(),
        "signin-otp".to_string(),
        "My_Code configuration-1".to_string(),
        "0".to_string(),
        "N".repeat(64),
    ]
}

/// The passcode policies this table spans, each a `codeConfigurationParameters`
/// value the model accepts. The rows differ in which members are present and in the
/// values those members take: nothing at all, one member alone, every member, and
/// each published bound of `CodeLength` (@range(4..8)), `ValidityPeriodMinutes`
/// (@range(1..60)) and `MaxVerificationAttempts` (@range(1..5)); the code types are
/// the `CodeType` enum's own values.
fn accepted_code_configurations() -> Vec<Option<Value>> {
    vec![
        None,
        Some(json!({})),
        Some(json!({ "codeType": "NUMERIC" })),
        Some(json!({ "codeType": "ALPHA", "codeLength": 4 })),
        Some(json!({ "codeType": "ALPHANUMERIC", "codeLength": 8 })),
        Some(json!({ "validityPeriodMinutes": 1, "maxAttempts": 1 })),
        Some(json!({ "validityPeriodMinutes": 60, "maxAttempts": 5 })),
        Some(json!({
            "codeType": "NUMERIC",
            "codeLength": 6,
            "validityPeriodMinutes": 15,
            "maxAttempts": 3,
        })),
    ]
}

/// The channel parameter sets this table spans. Each member of `ChannelParameters`
/// configures one delivery route and any subset may be supplied, so the rows are:
/// nothing, each route on its own, and every route together.
fn accepted_channel_parameters() -> Vec<Option<Value>> {
    vec![
        None,
        Some(json!({})),
        Some(json!({ "text": { "inlineTemplateBody": "Your code is {{code}}" } })),
        Some(json!({
            "text": {
                "inlineTemplateBody": "Your code is {{code}}",
                "destinationCountryParameters": { "US": "ORIG-1" },
            }
        })),
        Some(json!({
            "voice": {
                "inlineTemplateBody": "Your code is {{code}}",
                "languageCode": "en-US",
                "voiceId": "Joanna",
                "voiceMessageBodyTextType": "TEXT",
            }
        })),
        Some(json!({ "notify": { "notifyTemplateId": "tmpl-1", "voiceId": "Matthew" } })),
        Some(json!({ "whatsApp": { "whatsAppTemplateName": "otp_template", "languageCode": "en" } })),
        Some(json!({
            "text": { "inlineTemplateBody": "Your code is {{code}}" },
            "voice": { "inlineTemplateBody": "Your code is {{code}}", "voiceId": "Joanna" },
            "notify": { "notifyTemplateId": "tmpl-2" },
            "whatsApp": { "whatsAppTemplateName": "otp_template" },
        })),
    ]
}

/// `body`, with `member` set to `value` when there is one.
fn with_member(body: &Value, member: &str, value: Option<&Value>) -> Value {
    let mut object: Map<String, Value> = body
        .as_object()
        .expect("a JSON object body")
        .clone();
    if let Some(value) = value {
        object.insert(member.to_string(), value.clone());
    }
    Value::Object(object)
}

#[tokio::test]
async fn a_created_configuration_is_persisted_and_read_back_whole() {
    let em = Emulator::new();

    for name in accepted_names() {
        for protection in [None, Some(false), Some(true)] {
            for tags in [
                None,
                Some(json!([])),
                Some(json!([{ "key": "team", "value": "messaging" }, { "key": "empty", "value": "" }])),
            ] {
                let mut body = json!({ "notifyCodeConfigurationName": name });
                if let Some(enabled) = protection {
                    body["deletionProtectionEnabled"] = json!(enabled);
                }
                body = with_member(&body, "tags", tags.as_ref());
                let case = format!(
                    "name={name:?} deletionProtectionEnabled={protection:?} tags={tags:?}"
                );

                let created = em.create(body).await;
                assert_eq!(
                    created.status, 200,
                    "case {case}: expected 200, got {} / {:?}",
                    created.status, created.body
                );
                let id = created.string("notifyCodeConfigurationId");
                assert_eq!(
                    created.string("notifyCodeConfigurationName"),
                    name,
                    "case {case}: the created name must be the one requested, got {:?}",
                    created.body
                );
                assert_eq!(
                    created.member("deletionProtectionEnabled"),
                    &json!(protection.unwrap_or(false)),
                    "case {case}: an omitted deletionProtectionEnabled defaults to false, got {:?}",
                    created.body
                );
                assert_eq!(
                    created.string("notifyCodeConfigurationArn"),
                    format!(
                        "arn:aws:{SIGNING_NAME}:us-east-1:{ACCOUNT_ID}:{CONFIGURATION_TYPE}/{id}"
                    ),
                    "case {case}: unexpected ARN, got {:?}",
                    created.body
                );
                assert_eq!(
                    created.member("createdAt"),
                    created.member("updatedAt"),
                    "case {case}: a never-updated configuration has createdAt == updatedAt, \
                     got {:?}",
                    created.body
                );

                // Round trip: the reader reports exactly what create stored.
                let fetched = em.get(&id).await;
                assert_eq!(
                    fetched.status, 200,
                    "case {case}: GetNotifyCodeConfiguration must find the configuration just \
                     created, got {} / {:?}",
                    fetched.status, fetched.body
                );
                for member in RESOURCE_MEMBERS {
                    assert_eq!(
                        fetched.member(member),
                        created.member(member),
                        "case {case}: the reader disagrees with the create on {member}: \
                         {:?} vs {:?}",
                        fetched.member(member),
                        created.member(member)
                    );
                }
            }
        }
    }
}

#[tokio::test]
async fn the_stored_policy_and_channel_parameters_are_reported_member_for_member() {
    let em = Emulator::new();

    for code_configuration in accepted_code_configurations() {
        for channel_parameters in accepted_channel_parameters() {
            let body = json!({ "notifyCodeConfigurationName": "parameters" });
            let body = with_member(
                &body,
                "codeConfigurationParameters",
                code_configuration.as_ref(),
            );
            let body = with_member(&body, "channelParameters", channel_parameters.as_ref());
            let case = format!(
                "codeConfigurationParameters={code_configuration:?} \
                 channelParameters={channel_parameters:?}"
            );

            let created = em.create(body).await;
            assert_eq!(
                created.status, 200,
                "case {case}: expected 200, got {} / {:?}",
                created.status, created.body
            );
            let id = created.string("notifyCodeConfigurationId");

            // A member the request carried is reported back as supplied; one it
            // omitted is absent, because no value is applied at create time.
            for (member, requested) in [
                ("codeConfigurationParameters", &code_configuration),
                ("channelParameters", &channel_parameters),
            ] {
                let expected = requested.clone().unwrap_or(Value::Null);
                assert_eq!(
                    created.member(member),
                    &expected,
                    "case {case}: {member} must be reported as supplied, got {:?}",
                    created.member(member)
                );
            }

            let fetched = em.get(&id).await;
            assert_eq!(
                fetched.status, 200,
                "case {case}: the reader must find the configuration, got {} / {:?}",
                fetched.status, fetched.body
            );
            assert_eq!(
                fetched.resource(),
                created.resource(),
                "case {case}: the reader must report the stored record unchanged, got {:?}",
                fetched.body
            );
        }
    }
}

#[tokio::test]
async fn a_configuration_resolves_by_bare_id_and_by_full_arn() {
    let em = Emulator::new();
    let created = em.create_named("resolve-me").await;
    let id = created.string("notifyCodeConfigurationId");
    let arn = created.string("notifyCodeConfigurationArn");

    for reference in [id.clone(), arn.clone()] {
        let fetched = em.get(&reference).await;
        assert_eq!(
            fetched.status, 200,
            "case {reference:?}: a NotifyCodeConfigurationIdOrArn resolves by id or ARN, \
             got {} / {:?}",
            fetched.status, fetched.body
        );
        assert_eq!(
            fetched.string("notifyCodeConfigurationId"),
            id,
            "case {reference:?}: resolved the wrong configuration, got {:?}",
            fetched.body
        );
    }
}

#[tokio::test]
async fn the_arn_carries_the_region_the_request_was_signed_for() {
    // Every row is a real AWS region name; the rule is the ARN's region segment is
    // the request's, not a fixed one.
    for region in ["us-east-1", "eu-west-1", "ap-southeast-2", "us-gov-west-1"] {
        let em = Emulator::in_region(region);
        let created = em.create_named("regional").await;
        let id = created.string("notifyCodeConfigurationId");
        assert_eq!(
            created.string("notifyCodeConfigurationArn"),
            format!("arn:aws:{SIGNING_NAME}:{region}:{ACCOUNT_ID}:{CONFIGURATION_TYPE}/{id}"),
            "case {region}: the ARN must carry the signed region, got {:?}",
            created.body
        );
    }
}

#[tokio::test]
async fn an_identifier_that_names_nothing_is_a_404_with_the_templated_sentence() {
    let em = Emulator::new();
    // One live configuration, so a miss is a miss rather than an empty store.
    em.create_named("live-one").await;

    // The rows are identifiers a `NotifyCodeConfigurationIdOrArn` accepts that name
    // nothing: a minted-shaped id, a short bare id, one carrying each punctuation
    // the shape's `^[A-Za-z0-9_:/-]+$` pattern allows, the maximum length its
    // `@length(1..256)` permits, and a well-formed ARN never issued.
    let unknown = vec![
        "ncc-00000000000000000".to_string(),
        "x".to_string(),
        "A_b-c:d/e".to_string(),
        "z".repeat(256),
        format!(
            "arn:aws:{SIGNING_NAME}:us-east-1:{ACCOUNT_ID}:{CONFIGURATION_TYPE}/ncc-11111111111111111"
        ),
    ];
    for identifier in unknown {
        let reply = em.get(&identifier).await;
        assert_eq!(
            reply.status, 404,
            "case {identifier:?}: expected 404, got {} / {:?}",
            reply.status, reply.body
        );
        assert_eq!(
            reply.error.as_deref(),
            Some("ResourceNotFoundException"),
            "case {identifier:?}: unexpected error code, got {:?}",
            reply.error
        );
        let expected = format!(
            "Resource not found - ResourceType=\"{CONFIGURATION_TYPE}\" ResourceId=\"{identifier}\""
        );
        assert_eq!(
            reply.top("message"),
            expected,
            "case {identifier:?}: expected message {expected:?}, got {:?}",
            reply.body
        );
        assert_eq!(
            reply.top("resourceType"),
            CONFIGURATION_TYPE,
            "case {identifier:?}: expected resourceType {CONFIGURATION_TYPE:?}, got {:?}",
            reply.body
        );
        assert_eq!(
            reply.top("resourceId"),
            identifier,
            "case {identifier:?}: resourceId must be the requested identifier, got {:?}",
            reply.body
        );
    }
}

#[tokio::test]
async fn a_value_outside_a_modelled_constraint_is_a_400_validation_exception() {
    let em = Emulator::new();

    // `notifyCodeConfigurationName` is @length(1..64) with pattern
    // ^[A-Za-z0-9_ -]*[A-Za-z0-9_-][A-Za-z0-9_ -]*$: both bounds, one past the upper
    // bound, and a character the pattern excludes.
    let bad_names = vec![
        String::new(),
        " ".to_string(),
        "bad!name".to_string(),
        "no.dots".to_string(),
        "N".repeat(65),
    ];
    for name in bad_names {
        let reply = em.create_named(&name).await;
        assert_eq!(
            reply.status, 400,
            "case name={name:?}: expected 400, got {} / {:?}",
            reply.status, reply.body
        );
        assert_eq!(
            reply.error.as_deref(),
            Some("ValidationException"),
            "case name={name:?}: unexpected error code, got {:?}",
            reply.error
        );
    }

    // Each numeric policy member is a modelled @range, so one step outside either
    // published bound is refused: CodeLength 4..8, ValidityPeriodMinutes 1..60,
    // MaxVerificationAttempts 1..5.
    let out_of_range = vec![
        json!({ "codeLength": 3 }),
        json!({ "codeLength": 9 }),
        json!({ "validityPeriodMinutes": 0 }),
        json!({ "validityPeriodMinutes": 61 }),
        json!({ "maxAttempts": 0 }),
        json!({ "maxAttempts": 6 }),
    ];
    for parameters in out_of_range {
        let reply = em
            .create(json!({
                "notifyCodeConfigurationName": "out-of-range",
                "codeConfigurationParameters": parameters,
            }))
            .await;
        assert_eq!(
            reply.status, 400,
            "case codeConfigurationParameters={parameters}: expected 400, got {} / {:?}",
            reply.status, reply.body
        );
        assert_eq!(
            reply.error.as_deref(),
            Some("ValidationException"),
            "case codeConfigurationParameters={parameters}: unexpected error code, got {:?}",
            reply.error
        );
    }

    // `clientToken` is @length(1..128) with pattern ^[\x21-\x7E]+$: the empty
    // string, one past the upper bound, and a value carrying a space.
    let bad_tokens = vec![String::new(), "t".repeat(129), "has space".to_string()];
    for token in bad_tokens {
        let reply = em
            .create(json!({
                "notifyCodeConfigurationName": "bad-token",
                "clientToken": token,
            }))
            .await;
        assert_eq!(
            reply.status, 400,
            "case clientToken={token:?}: expected 400, got {} / {:?}",
            reply.status, reply.body
        );
        assert_eq!(
            reply.error.as_deref(),
            Some("ValidationException"),
            "case clientToken={token:?}: unexpected error code, got {:?}",
            reply.error
        );
    }
}

#[tokio::test]
async fn a_repeated_client_token_reports_the_first_configuration() {
    let em = Emulator::new();

    // Rows differ in what the repeat asks for: the same body, which is the repeat an
    // idempotent retry sends and which answers the first result; and three that differ
    // from it — a different name, a different passcode policy, and a different
    // deletionProtectionEnabled — which are a reuse of the token for ANOTHER request and
    // are refused rather than served the first configuration.
    let repeats: Vec<(Value, u16)> = vec![
        (json!({ "notifyCodeConfigurationName": "idempotent" }), 200),
        (
            json!({ "notifyCodeConfigurationName": "renamed-on-repeat" }),
            400,
        ),
        (
            json!({
                "notifyCodeConfigurationName": "idempotent",
                "codeConfigurationParameters": { "codeLength": 8 },
            }),
            400,
        ),
        (
            json!({
                "notifyCodeConfigurationName": "idempotent",
                "deletionProtectionEnabled": true,
            }),
            400,
        ),
    ];

    for (index, (repeat, expected)) in repeats.into_iter().enumerate() {
        let token = format!("token-{index}");
        let case = format!("clientToken={token} repeat={repeat}");
        let first = em
            .create(json!({
                "notifyCodeConfigurationName": "idempotent",
                "clientToken": token,
            }))
            .await;
        assert_eq!(
            first.status, 200,
            "case {case}: the first create must succeed, got {} / {:?}",
            first.status, first.body
        );

        let mut body = repeat.clone();
        body["clientToken"] = json!(token);
        let again = em.create(body).await;
        assert_eq!(
            again.status, expected,
            "case {case}: expected {expected}, got {} / {:?}",
            again.status, again.body
        );
        if expected == 200 {
            assert_eq!(
                again.resource(),
                first.resource(),
                "case {case}: an idempotent repeat reports the first configuration, got {:?}",
                again.body
            );
            continue;
        }
        assert_eq!(
            again.error.as_deref(),
            Some("ValidationException"),
            "case {case}: unexpected error code, got {:?}",
            again.error
        );
        assert_eq!(
            again.top("message"),
            "The idempotency token and parameters presented do not match",
            "case {case}: unexpected refusal message, got {:?}",
            again.body
        );
        // The refused replay changed nothing: the configuration the token minted still
        // reads back exactly as the request it served left it.
        let stored = em.get(&first.string("notifyCodeConfigurationId")).await;
        assert_eq!(
            stored.resource(),
            first.resource(),
            "case {case}: a refused replay must leave the first configuration as it was, got {:?}",
            stored.body
        );
    }

    // A create carrying no token, and one carrying a fresh token, each mint their own
    // configuration rather than joining an earlier one.
    let untokened_first = em.create_named("untokened").await;
    let untokened_second = em.create_named("untokened").await;
    assert_ne!(
        untokened_first.string("notifyCodeConfigurationId"),
        untokened_second.string("notifyCodeConfigurationId"),
        "two creates without a clientToken must mint two configurations"
    );
    let fresh = em
        .create(json!({
            "notifyCodeConfigurationName": "fresh",
            "clientToken": "a-token-never-used-before",
        }))
        .await;
    assert_ne!(
        fresh.string("notifyCodeConfigurationId"),
        untokened_first.string("notifyCodeConfigurationId"),
        "a fresh clientToken must mint its own configuration"
    );
}

#[tokio::test]
async fn the_store_is_bounded_and_refuses_a_create_past_its_ceiling() {
    use service_com_amazonaws_endusermessaging_endusermessaging::state::MAX_NOTIFY_CODE_CONFIGURATIONS;

    let em = Emulator::new();
    let mut first_id = String::new();
    for index in 0..MAX_NOTIFY_CODE_CONFIGURATIONS {
        let created = em.create_named(&format!("bounded-{index}")).await;
        assert_eq!(
            created.status, 200,
            "case index={index}: a create below the ceiling must succeed, got {} / {:?}",
            created.status, created.body
        );
        if index == 0 {
            first_id = created.string("notifyCodeConfigurationId");
        }
    }

    // Rows: a plain create, one with tags, and one with a client token. None of
    // them may grow the store past the ceiling.
    let bodies = vec![
        json!({ "notifyCodeConfigurationName": "one-too-many" }),
        json!({
            "notifyCodeConfigurationName": "one-too-many",
            "tags": [{ "key": "k", "value": "v" }],
        }),
        json!({
            "notifyCodeConfigurationName": "one-too-many",
            "clientToken": "fresh-token",
        }),
    ];
    for body in bodies {
        let case = format!("body={body}");
        let refused = em.create(body).await;
        assert_eq!(
            refused.status, 400,
            "case {case}: a create at the ceiling is ServiceQuotaExceededException (400), \
             got {} / {:?}",
            refused.status, refused.body
        );
        assert_eq!(
            refused.error.as_deref(),
            Some("ServiceQuotaExceededException"),
            "case {case}: unexpected error code, got {:?}",
            refused.error
        );
    }

    // The ceiling refused the extra creates rather than dropping anything already
    // stored: the first configuration minted below it still reads back.
    let still_there = em.get(&first_id).await;
    assert_eq!(
        still_there.status, 200,
        "the first configuration must survive the refused creates, got {} / {:?}",
        still_there.status, still_there.body
    );
    assert_eq!(
        still_there.string("notifyCodeConfigurationId"),
        first_id,
        "the reader must report the configuration asked for, got {:?}",
        still_there.body
    );
}
