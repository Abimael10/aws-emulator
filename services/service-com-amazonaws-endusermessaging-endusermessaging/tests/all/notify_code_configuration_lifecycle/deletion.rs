// SPDX-License-Identifier: CC0-1.0

//! `DeleteNotifyCodeConfiguration`: a stored notify code configuration removed over the
//! wire, and the deletion protection that refuses to remove it.
//!
//! Every assertion goes through `MultiServiceRouter::dispatch`, so it exercises the same
//! restJson1 path a real SDK client takes: routing, deserialization, boundary constraint
//! validation, the handler, and error rendering. The configuration each case deletes is
//! created by `CreateNotifyCodeConfiguration` over that same wire and read back through
//! `GetNotifyCodeConfiguration` and `ListNotifyCodeConfigurations`, so what is asserted
//! is the state a caller actually wrote. Where a case needs the passcode a send
//! generated — which no response ever carries — it reads the stored verification off the
//! shared engine, the same store the handlers write.
//!
//! The rules under test, each over a SET of inputs rather than one recorded request:
//!
//! * a delete of a resource whose deletion protection is switched on is refused with
//!   `ConflictException` (409) carrying this error class's templated sentence, and the
//!   resource is left exactly as it was; switching the protection off makes the same
//!   delete succeed;
//! * a served delete removes the record: the operation's modelled output carries no
//!   member, and every later read of that resource — by its bare id, by its full ARN, in
//!   the list, and through its tags — reports that it is gone, while every other stored
//!   configuration is untouched;
//! * an identifier that names no stored configuration is `ResourceNotFoundException`
//!   (404) carrying this error class's templated sentence, which is also how a repeat of
//!   a delete already served answers, while a value that is no identifier of this
//!   resource at all is classified before the lookup and refused as
//!   `ValidationException` (400) naming the member it arrived on;
//! * a verification already in flight keeps the policy and passcode it captured when it
//!   was sent, so deleting the configuration it was resolved from does not affect it.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_endusermessaging_endusermessaging::state::{
    ComAmazonawsEndusermessagingEndusermessagingState as ServiceState, Verification,
    NOTIFY_CODE_CONFIGURATION_RESOURCE_TYPE,
};
use service_com_amazonaws_endusermessaging_endusermessaging::EmulatedService;

/// This service's sigv4 signing name, from the model (`OPERATIONS.md`, "Wire protocol").
const SIGNING_NAME: &str = "end-user-messaging";

/// The account id every generated emulator presents, as `engine` defines it.
const ACCOUNT_ID: &str = engine::EMULATOR_ACCOUNT_ID;

/// The hyphenated resource type a notify-code-configuration refusal reports, as the
/// service exports it for its handlers.
const CONFIGURATION_TYPE: &str = NOTIFY_CODE_CONFIGURATION_RESOURCE_TYPE;

/// The `originationIdentity` every send of this suite is made from.
const SENDER: &str = "ORIG-1";

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
    /// A top-level member of the body, as a string.
    fn top(&self, name: &str) -> String {
        self.body[name]
            .as_str()
            .unwrap_or_else(|| panic!("member {name} is not a string in {}", self.body))
            .to_string()
    }

    /// The `notifyCodeConfiguration` an output carries.
    fn resource(&self) -> &Value {
        &self.body["notifyCodeConfiguration"]
    }

    /// The `notifyCodeConfigurationId` an output's resource carries.
    fn id(&self) -> String {
        self.resource()["notifyCodeConfigurationId"]
            .as_str()
            .unwrap_or_else(|| panic!("the reply reports no configuration id: {}", self.body))
            .to_string()
    }

    /// The `notifyCodeConfigurationArn` an output's resource carries.
    fn arn(&self) -> String {
        self.resource()["notifyCodeConfigurationArn"]
            .as_str()
            .unwrap_or_else(|| panic!("the reply reports no configuration ARN: {}", self.body))
            .to_string()
    }
}

/// One emulator over one shared engine: every call sees the same store, and the test can
/// read that store for the values no response reports.
struct Emulator {
    router: MultiServiceRouter,
    engine: Arc<engine::InMemoryEngine>,
    region: String,
}

impl Emulator {
    fn in_region(region: &str) -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        router.set_request_authentication(false);
        router.register_service(EmulatedService::new(Arc::clone(&engine)));
        Self {
            router,
            engine,
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

    async fn get(&self, id_or_arn: &str) -> Reply {
        self.call(
            "GET",
            &format!("/v1/notify-code-configurations/{id_or_arn}"),
            None,
        )
        .await
    }

    async fn update(&self, id_or_arn: &str, body: Value) -> Reply {
        self.call(
            "PUT",
            &format!("/v1/notify-code-configurations/{id_or_arn}"),
            Some(body),
        )
        .await
    }

    async fn delete(&self, id_or_arn: &str) -> Reply {
        self.call(
            "DELETE",
            &format!("/v1/notify-code-configurations/{id_or_arn}"),
            None,
        )
        .await
    }

    /// Every stored configuration's id, as `ListNotifyCodeConfigurations` reports them.
    async fn listed_ids(&self) -> Vec<String> {
        let listed = self.call("GET", "/v1/notify-code-configurations", None).await;
        assert_eq!(
            listed.status, 200,
            "the list must be served, got {} / {:?}",
            listed.status, listed.body
        );
        listed.body["notifyCodeConfigurations"]
            .as_array()
            .unwrap_or_else(|| panic!("the list reports no configurations: {}", listed.body))
            .iter()
            .map(|item| {
                item["notifyCodeConfigurationId"]
                    .as_str()
                    .unwrap_or_else(|| panic!("a listed configuration reports no id: {item}"))
                    .to_string()
            })
            .collect()
    }

    /// `ListTagsForResource`. `resourceArn` is a NON-greedy `@httpLabel`, so the ARN
    /// travels percent-encoded as one path segment, the way an SDK client sends it.
    async fn list_tags(&self, resource_arn: &str) -> Reply {
        self.call(
            "GET",
            &format!(
                "/v1/tags/{}",
                crate::brand_profile_default_attributes::path_label(resource_arn)
            ),
            None,
        )
        .await
    }

    async fn send(&self, body: Value) -> Reply {
        self.call("POST", "/v1/notify-code-verifications/send", Some(body))
            .await
    }

    async fn validate(&self, body: Value) -> Reply {
        self.call("POST", "/v1/notify-code-verifications/validate", Some(body))
            .await
    }

    /// The stored verification `id` names, cloned out of the shared store.
    fn stored_verification(&self, case: &str, id: &str) -> Verification {
        self.engine
            .state::<ServiceState>()
            .lock()
            .verifications
            .get(id)
            .cloned()
            .unwrap_or_else(|| panic!("case {case}: the store holds no verification {id}"))
    }
}

/// The sentence this API refuses an identifier that names no stored configuration with.
fn not_found_message(identifier: &str) -> String {
    format!("Resource not found - ResourceType=\"{CONFIGURATION_TYPE}\" ResourceId=\"{identifier}\"")
}

/// The sentence this API refuses a request the resource's deletion protection forbids
/// with: the `Reason` token names the state that caused the conflict, and no
/// request-supplied value appears in it.
fn deletion_protection_message() -> String {
    format!(
        "Conflict occurred - Reason=\"DELETION_PROTECTION_ENABLED\" \
         ResourceType=\"{CONFIGURATION_TYPE}\""
    )
}

/// An ARN of this service's notify-code-configuration resource in `region`, the form
/// `GetNotifyCodeConfiguration` reports for a stored one.
fn configuration_arn(region: &str, id: &str) -> String {
    format!("arn:aws:{SIGNING_NAME}:{region}:{ACCOUNT_ID}:{CONFIGURATION_TYPE}/{id}")
}

/// Assert that `reply` carries no member at all, which is what
/// `DeleteNotifyCodeConfigurationOutput` models.
fn assert_output_carries_no_member(case: &str, reply: &Reply) {
    let empty = match &reply.body {
        Value::Null => true,
        Value::Object(members) => members.is_empty(),
        _ => false,
    };
    assert!(
        empty,
        "case {case}: the modelled output carries no member, got {:?}",
        reply.body
    );
}

/// Assert that `reply` is this API's not-found refusal for `identifier`.
fn assert_not_found(case: &str, identifier: &str, reply: &Reply) {
    assert_eq!(
        reply.status, 404,
        "case {case}: expected 404 for {identifier}, got {} / {:?}",
        reply.status, reply.body
    );
    assert_eq!(
        reply.error.as_deref(),
        Some("ResourceNotFoundException"),
        "case {case}: expected ResourceNotFoundException for {identifier}, got {:?} / {:?}",
        reply.error,
        reply.body
    );
    assert_eq!(
        reply.top("message"),
        not_found_message(identifier),
        "case {case}: the refusal must carry this error class's sentence, got {:?}",
        reply.body
    );
}

/// One row of the deletion-protection table: how the resource is addressed, how its
/// protection got switched on, what else the record carries, and which Region it lives
/// in.
struct Case {
    what: &'static str,
    region: &'static str,
    /// Whether the delete addresses the configuration by its full ARN rather than by its
    /// bare id — both forms `NotifyCodeConfigurationIdOrArn` accepts.
    by_arn: bool,
    /// Whether the create switched deletion protection on, or a later update did.
    protect_at_create: bool,
    /// The `tags` the create carried, if any: they live on the record, so a delete takes
    /// them with it.
    tags: Option<Value>,
}

/// The deletion-protection table. The rows differ in the identifier form the delete uses,
/// in the Region the resource was created in (which its ARN carries), in whether the
/// protection was set by the create or by a later update, and in whether the record also
/// carries tags.
fn cases() -> Vec<Case> {
    vec![
        Case {
            what: "protected at create, deleted by bare id, untagged",
            region: "us-east-1",
            by_arn: false,
            protect_at_create: true,
            tags: None,
        },
        Case {
            what: "protected at create, deleted by full ARN, two tags",
            region: "us-west-2",
            by_arn: true,
            protect_at_create: true,
            tags: Some(json!([
                { "key": "team", "value": "messaging" },
                { "key": "empty", "value": "" },
            ])),
        },
        Case {
            what: "protected by a later update, deleted by bare id, one tag",
            region: "eu-west-1",
            by_arn: false,
            protect_at_create: false,
            tags: Some(json!([{ "key": "env", "value": "test" }])),
        },
        Case {
            what: "protected by a later update, deleted by full ARN, untagged",
            region: "ap-southeast-2",
            by_arn: true,
            protect_at_create: false,
            tags: None,
        },
    ]
}

#[tokio::test]
async fn deletion_protection_refuses_the_delete_until_it_is_switched_off() {
    for case in cases() {
        let what = case.what;
        let em = Emulator::in_region(case.region);

        let mut body = json!({
            "notifyCodeConfigurationName": "signin-otp",
            "deletionProtectionEnabled": case.protect_at_create,
        });
        if let Some(tags) = case.tags.as_ref() {
            body["tags"] = tags.clone();
        }
        let created = em.create(body).await;
        assert_eq!(
            created.status, 200,
            "case {what}: the create must be served, got {} / {:?}",
            created.status, created.body
        );
        let id = created.id();
        let arn = created.arn();
        assert_eq!(
            arn,
            configuration_arn(case.region, &id),
            "case {what}: the ARN must name this Region's resource, got {:?}",
            created.body
        );
        if !case.protect_at_create {
            let protected = em
                .update(&id, json!({ "deletionProtectionEnabled": true }))
                .await;
            assert_eq!(
                protected.status, 200,
                "case {what}: switching protection on must be served, got {} / {:?}",
                protected.status, protected.body
            );
        }

        let reference = if case.by_arn { arn.clone() } else { id.clone() };
        let before = em.get(&id).await;
        assert_eq!(
            before.status, 200,
            "case {what}: the protected configuration must read back, got {} / {:?}",
            before.status, before.body
        );

        // Protected: the delete is refused, and nothing about the resource moves.
        let refused = em.delete(&reference).await;
        assert_eq!(
            refused.status, 409,
            "case {what}: expected 409 while protected, got {} / {:?}",
            refused.status, refused.body
        );
        assert_eq!(
            refused.error.as_deref(),
            Some("ConflictException"),
            "case {what}: expected ConflictException while protected, got {:?} / {:?}",
            refused.error,
            refused.body
        );
        let message = refused.top("message");
        assert_eq!(
            message,
            deletion_protection_message(),
            "case {what}: the refusal must carry this error class's sentence, got {:?}",
            refused.body
        );
        assert!(
            !message.contains(&id) && !message.contains(&arn),
            "case {what}: the conflict sentence carries no request-supplied value, got \
             {message:?}"
        );
        assert_eq!(
            refused.top("resourceType"),
            CONFIGURATION_TYPE,
            "case {what}: expected resourceType {CONFIGURATION_TYPE:?}, got {:?}",
            refused.body
        );
        assert_eq!(
            refused.top("resourceId"),
            id,
            "case {what}: resourceId must name the conflicting configuration, got {:?}",
            refused.body
        );
        let after_refusal = em.get(&id).await;
        assert_eq!(
            after_refusal.resource(),
            before.resource(),
            "case {what}: a refused delete must leave the configuration exactly as it was, \
             got {:?}",
            after_refusal.body
        );

        // Switched off, the very same delete is served.
        let released = em
            .update(&id, json!({ "deletionProtectionEnabled": false }))
            .await;
        assert_eq!(
            released.status, 200,
            "case {what}: switching protection off must be served, got {} / {:?}",
            released.status, released.body
        );
        let deleted = em.delete(&reference).await;
        assert_eq!(
            deleted.status, 200,
            "case {what}: expected 200 once unprotected, got {} / {:?}",
            deleted.status, deleted.body
        );
        assert_output_carries_no_member(what, &deleted);

        // Gone from every read of it, by either identifier form, and from the list.
        assert_not_found(what, &id, &em.get(&id).await);
        assert_not_found(what, &arn, &em.get(&arn).await);
        let listed = em.listed_ids().await;
        assert!(
            listed.is_empty(),
            "case {what}: the deleted configuration must not be listed, got {listed:?}"
        );
        // The tags lived on the record, so the ARN now names no taggable resource.
        if case.tags.is_some() {
            let tags = em.list_tags(&arn).await;
            assert_eq!(
                tags.status, 404,
                "case {what}: the deleted resource's tags must be gone, got {} / {:?}",
                tags.status, tags.body
            );
        }
        // A repeat of a delete already served names nothing.
        assert_not_found(what, &reference, &em.delete(&reference).await);
    }
}

#[tokio::test]
async fn a_delete_removes_only_the_configuration_it_names() {
    let em = Emulator::new();

    // Three configurations, so the delete has a neighbour on either side of it in the
    // store's order.
    let mut created = Vec::new();
    for name in ["first-config", "second-config", "third-config"] {
        let reply = em
            .create(json!({
                "notifyCodeConfigurationName": name,
                "codeConfigurationParameters": { "codeType": "ALPHA", "codeLength": 8 },
                "channelParameters": { "text": { "inlineTemplateBody": "Code: {{code}}" } },
            }))
            .await;
        assert_eq!(
            reply.status, 200,
            "case {name}: the create must be served, got {} / {:?}",
            reply.status, reply.body
        );
        created.push((name, reply));
    }

    for (index, (name, reply)) in created.iter().enumerate() {
        let case = format!("deleting {name}");
        let id = reply.id();
        // Addressed by the bare id for one row and by the full ARN for the next, so both
        // forms delete.
        let reference = if index % 2 == 0 { id.clone() } else { reply.arn() };
        let before = em.listed_ids().await;

        let deleted = em.delete(&reference).await;
        assert_eq!(
            deleted.status, 200,
            "case {case}: expected 200, got {} / {:?}",
            deleted.status, deleted.body
        );
        assert_output_carries_no_member(&case, &deleted);

        let remaining = em.listed_ids().await;
        let expected: Vec<String> = before.iter().filter(|held| **held != id).cloned().collect();
        assert_eq!(
            remaining, expected,
            "case {case}: exactly the named configuration must go, got {remaining:?}"
        );
        // Every survivor still reads back whole, member for member.
        for (other_name, other) in &created[index + 1..] {
            let read_back = em.get(&other.id()).await;
            assert_eq!(
                read_back.status, 200,
                "case {case}: {other_name} must survive, got {} / {:?}",
                read_back.status, read_back.body
            );
            assert_eq!(
                read_back.resource(),
                other.resource(),
                "case {case}: {other_name} must be untouched, got {:?}",
                read_back.body
            );
        }
    }
}

#[tokio::test]
async fn an_identifier_that_names_no_configuration_is_not_found() {
    let em = Emulator::new();
    // A configuration exists, so what is refused is the identifier and not an empty
    // store.
    let created = em
        .create(json!({ "notifyCodeConfigurationName": "live-config" }))
        .await;
    let live_id = created.id();
    // A configuration that WAS stored and has since been deleted: its identifier names
    // nothing, the same as one no create ever minted.
    let short_lived = em
        .create(json!({ "notifyCodeConfigurationName": "short-lived" }))
        .await;
    let deleted_id = short_lived.id();
    assert_eq!(
        em.delete(&deleted_id).await.status,
        200,
        "the first delete must be served, so the identifier names nothing afterwards"
    );

    let unknown = "ncc-000000000deadbeef";
    for (case, identifier) in [
        ("a bare id no create minted", unknown.to_string()),
        (
            "an ARN of this Region naming no record",
            configuration_arn("us-east-1", unknown),
        ),
        (
            "an ARN of this Region naming a deleted record",
            configuration_arn("us-east-1", &deleted_id),
        ),
    ] {
        assert_not_found(case, &identifier, &em.delete(&identifier).await);
        // The refusal changed nothing: the live configuration is still there.
        let read_back = em.get(&live_id).await;
        assert_eq!(
            read_back.status, 200,
            "case {case}: a refused delete must leave the store alone, got {} / {:?}",
            read_back.status, read_back.body
        );
    }
}

/// Values a delete can carry that are no identifier of this resource at all, so they can
/// name a configuration in no state. The rows differ along what the rule generalises
/// over: the form the value takes (bare value or ARN) and which part of it is wrong —
/// another resource's prefix, a prefix with no body, a body one past
/// `NotifyCodeConfigurationId`'s `@length(1..64)`, punctuation that shape's
/// `^[A-Za-z0-9_-]+$` excludes, an ARN of another vendor, and an ARN of this service
/// whose resource type is another one. Each stays inside
/// `NotifyCodeConfigurationIdOrArn`'s own `@length(1..256)` and `^[A-Za-z0-9_:/-]+$`, so
/// the value reaches the handler rather than the boundary constraint walk.
fn malformed_identifiers() -> Vec<(&'static str, String)> {
    vec![
        ("a bare value with no resource prefix", "x".to_string()),
        (
            "another resource's prefix",
            "bp-00000000000000000".to_string(),
        ),
        ("the prefix with no body", "ncc-".to_string()),
        (
            "a body one past the bare identifier's maximum length",
            format!("ncc-{}", "z".repeat(61)),
        ),
        (
            "punctuation a bare identifier excludes",
            "ncc-a:b/c".to_string(),
        ),
        (
            "an ARN of another vendor",
            format!("arn:aws:sns:us-east-1:{ACCOUNT_ID}:{CONFIGURATION_TYPE}/ncc-1"),
        ),
        (
            "an ARN of another resource type of this service",
            format!("arn:aws:{SIGNING_NAME}:us-east-1:{ACCOUNT_ID}:brand-profile/bp-00000000000000000"),
        ),
    ]
}

#[tokio::test]
async fn a_value_that_is_no_identifier_is_refused_before_the_delete() {
    let em = Emulator::new();
    let created = em
        .create(json!({ "notifyCodeConfigurationName": "live-config" }))
        .await;
    let id = created.id();
    let stored = created.resource().clone();

    for (case, identifier) in malformed_identifiers() {
        let refused = em.delete(&identifier).await;
        assert_eq!(
            refused.status, 400,
            "case {case}: expected 400 for {identifier}, got {} / {:?}",
            refused.status, refused.body
        );
        assert_eq!(
            refused.error.as_deref(),
            Some("ValidationException"),
            "case {case}: expected ValidationException for {identifier}, got {:?} / {:?}",
            refused.error,
            refused.body
        );
        assert_eq!(
            refused.top("message"),
            format!("invalid notify code configuration ID format: {identifier}"),
            "case {case}: the refusal must state the format failure against the value \
             given, got {:?}",
            refused.body
        );
        assert_eq!(
            refused.body["fieldList"][0]["path"].as_str(),
            Some("notifyCodeConfigurationId"),
            "case {case}: the refusal must name the member the value arrived on, got {:?}",
            refused.body
        );
    }

    // Every refusal was ahead of the delete: the stored configuration is still there,
    // exactly as it was.
    let read_back = em.get(&id).await;
    assert_eq!(
        read_back.status, 200,
        "the refused deletes must leave the configuration readable, got {} / {:?}",
        read_back.status, read_back.body
    );
    assert_eq!(
        read_back.resource(),
        &stored,
        "the refused deletes must leave the configuration unchanged, got {:?}",
        read_back.body
    );
}

#[tokio::test]
async fn deleting_a_configuration_leaves_a_verification_in_flight_untouched() {
    let em = Emulator::new();

    // Each row sends its passcode against the configuration addressed the way that row
    // names, to its own recipient, so one row's verification cannot be read as another's.
    for (case, by_arn, destination) in [
        ("referenced by its bare id", false, "+12065550101"),
        ("referenced by its full ARN", true, "+12065550102"),
    ] {
        let created = em
            .create(json!({
                "notifyCodeConfigurationName": "signin-otp",
                "codeConfigurationParameters": {
                    "codeType": "ALPHA",
                    "codeLength": 8,
                    "validityPeriodMinutes": 60,
                    "maxAttempts": 5,
                },
                "channelParameters": { "text": { "inlineTemplateBody": "Code: {{code}}" } },
            }))
            .await;
        assert_eq!(
            created.status, 200,
            "case {case}: the create must be served, got {} / {:?}",
            created.status, created.body
        );
        let id = created.id();
        let reference = if by_arn { created.arn() } else { id.clone() };

        let sent = em
            .send(json!({
                "channel": "TEXT",
                "destinationIdentity": destination,
                "originationIdentity": SENDER,
                "notifyCodeConfiguration": reference,
            }))
            .await;
        assert_eq!(
            sent.status, 200,
            "case {case}: the send must be served, got {} / {:?}",
            sent.status, sent.body
        );
        let verification_id = sent.top("verificationId");
        let in_flight = em.stored_verification(case, &verification_id);

        let deleted = em.delete(&reference).await;
        assert_eq!(
            deleted.status, 200,
            "case {case}: expected 200, got {} / {:?}",
            deleted.status, deleted.body
        );

        // The verification captured its policy when the passcode was sent, so the
        // configuration going away does not reach it.
        let after = em.stored_verification(case, &verification_id);
        assert_eq!(
            after.policy, in_flight.policy,
            "case {case}: the captured policy must survive the delete, got {:?}",
            after.policy
        );
        assert_eq!(
            after.attempts, in_flight.attempts,
            "case {case}: the delete must consume no attempt, got {}",
            after.attempts
        );
        let validated = em
            .validate(json!({
                "destinationIdentity": destination,
                "code": in_flight.code,
            }))
            .await;
        assert_eq!(
            validated.status, 200,
            "case {case}: the validate must be served, got {} / {:?}",
            validated.status, validated.body
        );
        assert_eq!(
            validated.top("status"),
            "VALID",
            "case {case}: the passcode sent before the delete must still validate, got {:?}",
            validated.body
        );
    }
}
