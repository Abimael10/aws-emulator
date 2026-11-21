// SPDX-License-Identifier: CC0-1.0

//! The bodies this service puts on a brand-profile refusal, over the wire.
//!
//! Every assertion goes through `MultiServiceRouter::dispatch`, so it reads the
//! rendered restJson1 error body a real SDK client would parse.
//!
//! The rules under test, each over a SET of inputs rather than one recorded request:
//!
//! * a `ResourceNotFoundException` body carries this error class's fixed template
//!   `Resource not found - ResourceType="<type>" ResourceId="<id>"`, where `<type>`
//!   is the resource's hyphenated type and `<id>` is the identifier the request
//!   gave, verbatim — whichever operation refuses and whatever form the identifier
//!   took;
//! * an identifier member that accepts either a bare id or an ARN is CLASSIFIED
//!   before it is looked up: a value that is neither — so it can name the resource in
//!   no state — is `ValidationException` (400) reporting the format failure against
//!   the value given, and only a well-formed identifier that reaches no record is
//!   `ResourceNotFoundException` (404);
//! * a `ConflictException` body carries the fixed template
//!   `Conflict occurred - Reason="<REASON>" ResourceType="<type>"`, with the reason
//!   token naming the state that caused the conflict and no request-supplied value
//!   anywhere in the sentence;
//! * both report `resourceType` in that same hyphenated vocabulary, and
//!   `resourceId` names the resource;
//! * an update whose updatable members are each optional on their own refuses a
//!   request carrying none of them with `ValidationException` (400), and leaves the
//!   stored record exactly as it was;
//! * a value outside a constraint the MODEL declares is refused with the modelled
//!   `ValidationException` (400) whose `message` is
//!   `<N> validation error(s) detected. ` over one clause per failing member —
//!   `Value at '<JSON pointer>' failed to satisfy constraint: <requirement>`, with no
//!   value of the caller's in it — and whose `fieldList` carries that same clause under
//!   the pointer naming its member;
//! * a clause about a `@length` states one measurement of the value besides: the LENGTH
//!   that check counted — `Value with length <n> at '<JSON pointer>' …` — whatever shape
//!   kind carried it (a string's characters, a list's elements) and whichever bound it
//!   broke, while every other constraint class names the member and the requirement alone;
//! * a refusal this service words ITSELF names the member at fault in `fieldList` by the
//!   member's own name, and the identifier-format gate states there the one sentence it
//!   states in `message`.
//!
//! The constraint rows reach operations beyond the brand profiles, because the envelope is
//! the service's rather than one operation's: `call` addresses any of them.

use std::sync::{Arc, LazyLock};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_endusermessaging_endusermessaging::EmulatedService;

/// This service's sigv4 signing name, from the model (`OPERATIONS.md`, "Wire protocol").
const SIGNING_NAME: &str = "end-user-messaging";

/// The account id every generated emulator presents, as `engine` defines it.
const ACCOUNT_ID: &str = engine::EMULATOR_ACCOUNT_ID;

/// The region every request here is signed for.
const REGION: &str = "us-east-1";

/// The hyphenated resource type a brand-profile refusal reports, as the service
/// exports it for its handlers.
const BRAND_PROFILE_TYPE: &str =
    service_com_amazonaws_endusermessaging_endusermessaging::state::BRAND_PROFILE_RESOURCE_TYPE;

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

/// One emulator over one shared engine: every call sees the same store.
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

    async fn call(&self, method: &str, path: &str, body: Option<Value>) -> Reply {
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
                "https://{SIGNING_NAME}.{REGION}.amazonaws.com{path}"
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
        self.call("POST", "/v1/brand-profiles", Some(body)).await
    }

    async fn delete(&self, id_or_arn: &str) -> Reply {
        self.call("DELETE", &format!("/v1/brand-profiles/{id_or_arn}"), None)
            .await
    }

    async fn update(&self, id_or_arn: &str, body: Value) -> Reply {
        self.call("PUT", &format!("/v1/brand-profiles/{id_or_arn}"), Some(body))
            .await
    }

    async fn get(&self, id_or_arn: &str) -> Reply {
        self.call("GET", &format!("/v1/brand-profiles/{id_or_arn}"), None)
            .await
    }

    async fn list_attributes(&self, id_or_arn: &str) -> Reply {
        self.call(
            "GET",
            &format!("/v1/brand-profiles/{id_or_arn}/attributes"),
            None,
        )
        .await
    }

    async fn get_attribute(&self, id_or_arn: &str, name: &str) -> Reply {
        self.call(
            "GET",
            &format!("/v1/brand-profiles/{id_or_arn}/attributes/{name}"),
            None,
        )
        .await
    }
}

/// The identifiers a `BrandProfileIdOrArn` accepts, in the documented form of a bare
/// `brandProfileId` or of the ARN of one, that name no stored profile: a
/// minted-shaped id of digits, the bare-ID form the shape's own documentation gives
/// as its example, one whose body mixes both cases, and a well-formed ARN for a
/// profile that was never created. Each row is reported back verbatim.
fn unknown_identifiers() -> Vec<String> {
    vec![
        "bp-00000000000000000".to_string(),
        "bp-abc12345678901234".to_string(),
        "bp-AbCdEfGhIjKlMnOpQ".to_string(),
        format!("arn:aws:{SIGNING_NAME}:{REGION}:{ACCOUNT_ID}:brand-profile/bp-11111111111111111"),
    ]
}

/// Values a `BrandProfileIdOrArn` member accepts — each satisfies its
/// `^[A-Za-z0-9_:/-]+$` pattern and `@length(1..256)` — that are no identifier of a
/// brand profile at all, so none of them can name one in any state.
///
/// Rows span the ways a value misses the documented form (`BrandProfileIdOrArn`:
/// "Accepts either a bare ID (e.g., \"bp-abc12345678901234\") or a full ARN"): a
/// human-facing handle such as a profile's name, a single character, the longest
/// value the member admits, the right prefix with a body of the wrong length or
/// carrying a character the identifier body has none of, an ARN of another vendor,
/// an ARN of this service naming another of its resource types, and an ARN of this
/// service's brand profiles whose resource field is not an identifier.
fn malformed_identifiers() -> Vec<String> {
    vec![
        "a-profile-name".to_string(),
        "x".to_string(),
        "z".repeat(256),
        // 16 and 18 characters: one short of, and one past, the documented body.
        "bp-0000000000000000".to_string(),
        "bp-000000000000000000".to_string(),
        "bp-0000000000000000_".to_string(),
        "arn:aws:s3:::a-bucket".to_string(),
        format!(
            "arn:aws:{SIGNING_NAME}:{REGION}:{ACCOUNT_ID}:notify-code-configuration/ncc-00000000000000000"
        ),
        format!("arn:aws:{SIGNING_NAME}:{REGION}:{ACCOUNT_ID}:brand-profile/not-an-identifier"),
    ]
}

/// Values of the previous list that carry no `/`, so they also address the
/// attribute operations, whose `{brandProfileId}` label is a single path segment.
fn malformed_single_segment_identifiers() -> Vec<String> {
    malformed_identifiers()
        .into_iter()
        .filter(|identifier| !identifier.contains('/'))
        .collect()
}

/// Every brand-profile operation that resolves a `BrandProfileIdOrArn`, with the
/// request body it needs.
fn not_found_calls() -> Vec<(&'static str, Option<Value>)> {
    vec![
        ("GET", None),
        ("DELETE", None),
        ("PUT", Some(json!({ "brandProfileName": "renamed" }))),
    ]
}

#[tokio::test]
async fn a_not_found_body_is_the_templated_sentence_with_the_requested_identifier() {
    let em = Emulator::new();

    for identifier in unknown_identifiers() {
        for (method, body) in not_found_calls() {
            let case = format!("{method} identifier={identifier:?}");
            let reply = em
                .call(
                    method,
                    &format!("/v1/brand-profiles/{identifier}"),
                    body.clone(),
                )
                .await;

            assert_eq!(
                reply.status, 404,
                "case {case}: expected 404, got {} / {:?}",
                reply.status, reply.body
            );
            assert_eq!(
                reply.error.as_deref(),
                Some("ResourceNotFoundException"),
                "case {case}: unexpected error code, got {:?}",
                reply.error
            );
            let expected = format!(
                "Resource not found - ResourceType=\"{BRAND_PROFILE_TYPE}\" ResourceId=\"{identifier}\""
            );
            assert_eq!(
                reply.string("message"),
                expected,
                "case {case}: expected message {expected:?}, got {:?}",
                reply.body
            );
            assert_eq!(
                reply.string("resourceType"),
                BRAND_PROFILE_TYPE,
                "case {case}: expected resourceType {BRAND_PROFILE_TYPE:?}, got {:?}",
                reply.body
            );
            assert_eq!(
                reply.string("resourceId"),
                identifier,
                "case {case}: resourceId must be the requested identifier, got {:?}",
                reply.body
            );
        }
    }
}

#[tokio::test]
async fn the_reported_resource_type_is_the_arn_resource_vocabulary() {
    // The type token is the resource segment of the resource's own ARN, so a
    // refusal and the ARN of a live profile spell the resource the same way.
    let em = Emulator::new();
    let created = em
        .create(json!({ "brandProfileName": "vocabulary" }))
        .await;
    let arn = created.string("brandProfileArn");
    let id = created.string("brandProfileId");
    assert_eq!(
        arn,
        format!("arn:aws:{SIGNING_NAME}:{REGION}:{ACCOUNT_ID}:{BRAND_PROFILE_TYPE}/{id}"),
        "the ARN must name the resource with the same token a refusal reports, got {arn:?}"
    );
}

#[tokio::test]
async fn a_deletion_protection_conflict_body_is_the_templated_sentence_with_no_request_value() {
    let em = Emulator::new();

    // Rows differ in how protection came to be on and in the identifier form the
    // refused delete names: a create with it set, a create followed by an update
    // that sets it, and the same profile addressed by its ARN. The refusal body is
    // the same fixed sentence in every one.
    let cases: Vec<(&str, bool, bool)> = vec![
        ("set at create, deleted by id", true, false),
        ("set by update, deleted by id", false, false),
        ("set at create, deleted by ARN", true, true),
    ];

    for (case, protect_at_create, delete_by_arn) in cases {
        let created = em
            .create(json!({
                "brandProfileName": "protected",
                "deletionProtectionEnabled": protect_at_create,
            }))
            .await;
        assert_eq!(
            created.status, 200,
            "case {case}: the create must succeed, got {} / {:?}",
            created.status, created.body
        );
        let id = created.string("brandProfileId");
        let arn = created.string("brandProfileArn");
        if !protect_at_create {
            let updated = em
                .update(&id, json!({ "deletionProtectionEnabled": true }))
                .await;
            assert_eq!(
                updated.status, 200,
                "case {case}: switching protection on must succeed, got {} / {:?}",
                updated.status, updated.body
            );
        }

        let reference = if delete_by_arn { arn.clone() } else { id.clone() };
        let refused = em.delete(&reference).await;
        assert_eq!(
            refused.status, 409,
            "case {case}: expected 409, got {} / {:?}",
            refused.status, refused.body
        );
        assert_eq!(
            refused.error.as_deref(),
            Some("ConflictException"),
            "case {case}: unexpected error code, got {:?}",
            refused.error
        );
        let expected = format!(
            "Conflict occurred - Reason=\"DELETION_PROTECTION_ENABLED\" ResourceType=\"{BRAND_PROFILE_TYPE}\""
        );
        let message = refused.string("message");
        assert_eq!(
            message, expected,
            "case {case}: expected message {expected:?}, got {:?}",
            refused.body
        );
        assert!(
            !message.contains(&id) && !message.contains(&arn),
            "case {case}: the conflict sentence carries no request-supplied value, got {message:?}"
        );
        assert_eq!(
            refused.string("resourceType"),
            BRAND_PROFILE_TYPE,
            "case {case}: expected resourceType {BRAND_PROFILE_TYPE:?}, got {:?}",
            refused.body
        );
        assert_eq!(
            refused.string("resourceId"),
            id,
            "case {case}: resourceId must name the conflicting profile, got {:?}",
            refused.body
        );

        // Leave the store as it was found, so one row cannot shadow the next.
        assert_eq!(
            em.update(&id, json!({ "deletionProtectionEnabled": false }))
                .await
                .status,
            200,
            "case {case}: switching protection off must succeed"
        );
        assert_eq!(
            em.delete(&id).await.status,
            200,
            "case {case}: tearing the profile down must succeed"
        );
    }
}

/// The message a format failure of a brand-profile identifier is worded with: fixed
/// text plus the value the request gave, verbatim.
fn invalid_format_message(identifier: &str) -> String {
    format!("invalid brand profile ID format: {identifier}")
}

#[tokio::test]
async fn a_value_that_is_no_identifier_at_all_is_a_400_validation_exception() {
    let em = Emulator::new();
    // A live profile, so the refusal cannot be mistaken for an empty store. Its name
    // is one of the rows below: a name satisfies the identifier member's pattern but
    // is not an identifier, so naming a profile that exists changes nothing.
    let created = em.create(json!({ "brandProfileName": "a-profile-name" })).await;
    assert_eq!(
        created.status, 200,
        "the create must succeed, got {} / {:?}",
        created.status, created.body
    );

    for identifier in malformed_identifiers() {
        for (method, body) in not_found_calls() {
            let case = format!("{method} identifier={identifier:?}");
            let reply = em
                .call(
                    method,
                    &format!("/v1/brand-profiles/{identifier}"),
                    body.clone(),
                )
                .await;

            assert_eq!(
                reply.status, 400,
                "case {case}: expected 400, got {} / {:?}",
                reply.status, reply.body
            );
            assert_eq!(
                reply.error.as_deref(),
                Some("ValidationException"),
                "case {case}: unexpected error code, got {:?}",
                reply.error
            );
            let expected = invalid_format_message(&identifier);
            assert_eq!(
                reply.string("message"),
                expected,
                "case {case}: expected message {expected:?}, got {:?}",
                reply.body
            );
        }
    }
}

#[tokio::test]
async fn the_identifier_classification_does_not_depend_on_the_operation_called() {
    // Every operation taking a `BrandProfileIdOrArn` classifies it the same way, so
    // the attribute reads — which address the profile by the same member — answer the
    // same 400 for a value that is no identifier, and the same 404 for a well-formed
    // one that names nothing.
    let em = Emulator::new();

    for identifier in malformed_single_segment_identifiers() {
        for (operation, reply) in [
            ("ListBrandProfileAttributes", em.list_attributes(&identifier).await),
            (
                "GetBrandProfileAttribute",
                em.get_attribute(&identifier, "CompanyName").await,
            ),
        ] {
            let case = format!("{operation} identifier={identifier:?}");
            assert_eq!(
                reply.status, 400,
                "case {case}: expected 400, got {} / {:?}",
                reply.status, reply.body
            );
            assert_eq!(
                reply.error.as_deref(),
                Some("ValidationException"),
                "case {case}: unexpected error code, got {:?}",
                reply.error
            );
            let expected = invalid_format_message(&identifier);
            assert_eq!(
                reply.string("message"),
                expected,
                "case {case}: expected message {expected:?}, got {:?}",
                reply.body
            );
        }
    }

    for identifier in unknown_identifiers()
        .into_iter()
        .filter(|identifier| !identifier.contains('/'))
    {
        for (operation, reply) in [
            ("ListBrandProfileAttributes", em.list_attributes(&identifier).await),
            (
                "GetBrandProfileAttribute",
                em.get_attribute(&identifier, "CompanyName").await,
            ),
        ] {
            let case = format!("{operation} identifier={identifier:?}");
            assert_eq!(
                reply.status, 404,
                "case {case}: expected 404, got {} / {:?}",
                reply.status, reply.body
            );
            assert_eq!(
                reply.error.as_deref(),
                Some("ResourceNotFoundException"),
                "case {case}: unexpected error code, got {:?}",
                reply.error
            );
        }
    }
}

#[tokio::test]
async fn a_well_formed_identifier_still_resolves_in_both_of_its_forms() {
    // The classification refuses no identifier the shape documents: the same profile
    // is read by its bare id and by its full ARN, and the two reads agree member for
    // member.
    let em = Emulator::new();
    let created = em.create(json!({ "brandProfileName": "both-forms" })).await;
    let id = created.string("brandProfileId");
    let arn = created.string("brandProfileArn");

    let by_id = em.get(&id).await;
    let by_arn = em.get(&arn).await;
    assert_eq!(
        by_id.status, 200,
        "a read by bare id must succeed, got {} / {:?}",
        by_id.status, by_id.body
    );
    assert_eq!(
        by_arn.status, 200,
        "a read by ARN must succeed, got {} / {:?}",
        by_arn.status, by_arn.body
    );
    assert_eq!(
        by_id.body, by_arn.body,
        "the two identifier forms must report the identical record, got {:?} and {:?}",
        by_id.body, by_arn.body
    );
}

#[tokio::test]
async fn an_update_carrying_no_updatable_member_is_refused_and_changes_nothing() {
    let em = Emulator::new();
    // The fixed text of this refusal: it names no member and carries no value from
    // the request or from state.
    let expected_message = "At least one field must be provided for update";

    // Rows differ in the state of the profile the refused update names and in the
    // form the identifier takes: an untouched profile, a renamed one, one under
    // deletion protection, and one addressed by its full ARN rather than its bare id.
    let cases: Vec<(&str, bool, bool, bool)> = vec![
        ("as created", false, false, false),
        ("after a rename", true, false, false),
        ("under deletion protection", false, true, false),
        ("addressed by ARN", false, false, true),
    ];

    for (case, rename_first, protect, by_arn) in cases {
        let created = em
            .create(json!({ "brandProfileName": "update-guard" }))
            .await;
        assert_eq!(
            created.status, 200,
            "case {case}: the create must succeed, got {} / {:?}",
            created.status, created.body
        );
        let id = created.string("brandProfileId");
        let reference = if by_arn {
            created.string("brandProfileArn")
        } else {
            id.clone()
        };
        if rename_first {
            assert_eq!(
                em.update(&id, json!({ "brandProfileName": "update-guard-renamed" }))
                    .await
                    .status,
                200,
                "case {case}: the rename must succeed"
            );
        }
        if protect {
            assert_eq!(
                em.update(&id, json!({ "deletionProtectionEnabled": true }))
                    .await
                    .status,
                200,
                "case {case}: switching protection on must succeed"
            );
        }
        let before = em.get(&id).await;

        // An empty body carries neither updatable member: the request asks for no
        // change at all.
        let refused = em.update(&reference, json!({})).await;
        assert_eq!(
            refused.status, 400,
            "case {case}: expected 400, got {} / {:?}",
            refused.status, refused.body
        );
        assert_eq!(
            refused.error.as_deref(),
            Some("ValidationException"),
            "case {case}: unexpected error code, got {:?}",
            refused.error
        );
        assert_eq!(
            refused.string("message"),
            expected_message,
            "case {case}: expected message {expected_message:?}, got {:?}",
            refused.body
        );

        // The refusal is total: the record a later read reports is the one the refused
        // call found, `updatedAt` included.
        let after = em.get(&id).await;
        assert_eq!(
            after.body, before.body,
            "case {case}: a refused update must leave the record untouched, got {:?} after {:?}",
            after.body, before.body
        );

        if protect {
            assert_eq!(
                em.update(&id, json!({ "deletionProtectionEnabled": false }))
                    .await
                    .status,
                200,
                "case {case}: switching protection off must succeed"
            );
        }
        assert_eq!(
            em.delete(&id).await.status,
            200,
            "case {case}: tearing the profile down must succeed"
        );
    }
}

#[tokio::test]
async fn an_update_carrying_one_updatable_member_still_applies() {
    // The positive control of the guard above: any ONE of the updatable members is
    // enough, so only a request carrying none of them is refused.
    let em = Emulator::new();
    let cases: Vec<(&str, Value, Option<&str>, Option<bool>)> = vec![
        (
            "name only",
            json!({ "brandProfileName": "renamed-only" }),
            Some("renamed-only"),
            Some(false),
        ),
        (
            "protection only",
            json!({ "deletionProtectionEnabled": true }),
            Some("one-member"),
            Some(true),
        ),
        (
            "both members",
            json!({ "brandProfileName": "renamed-both", "deletionProtectionEnabled": true }),
            Some("renamed-both"),
            Some(true),
        ),
    ];

    for (case, body, expected_name, expected_protection) in cases {
        let created = em.create(json!({ "brandProfileName": "one-member" })).await;
        let id = created.string("brandProfileId");
        let updated = em.update(&id, body.clone()).await;
        assert_eq!(
            updated.status, 200,
            "case {case}: expected 200, got {} / {:?}",
            updated.status, updated.body
        );
        if let Some(name) = expected_name {
            assert_eq!(
                updated.string("brandProfileName"),
                name,
                "case {case}: expected name {name:?}, got {:?}",
                updated.body
            );
        }
        assert_eq!(
            updated.body["deletionProtectionEnabled"].as_bool(),
            expected_protection,
            "case {case}: expected deletionProtectionEnabled {expected_protection:?}, got {:?}",
            updated.body
        );
        // The update is persisted, not just reported.
        let read = em.get(&id).await;
        assert_eq!(
            read.body, updated.body,
            "case {case}: a read must report what the update answered, got {:?} and {:?}",
            read.body, updated.body
        );

        assert_eq!(
            em.update(&id, json!({ "deletionProtectionEnabled": false }))
                .await
                .status,
            200,
            "case {case}: switching protection off must succeed"
        );
        assert_eq!(
            em.delete(&id).await.status,
            200,
            "case {case}: tearing the profile down must succeed"
        );
    }
}

#[tokio::test]
async fn an_update_carrying_nothing_is_refused_before_the_identifier_is_resolved() {
    // The request is refused on its own members, so the refusal does not depend on
    // which profile it names: a well-formed identifier that names nothing answers the
    // same 400 as a live one, rather than the 404 a request with something to update
    // would get.
    let em = Emulator::new();
    for identifier in unknown_identifiers() {
        let case = format!("identifier={identifier:?}");
        let refused = em.update(&identifier, json!({})).await;
        assert_eq!(
            refused.status, 400,
            "case {case}: expected 400, got {} / {:?}",
            refused.status, refused.body
        );
        assert_eq!(
            refused.error.as_deref(),
            Some("ValidationException"),
            "case {case}: the refusal must name its error class on the wire, got {:?}",
            refused.error
        );
        assert_eq!(
            refused.string("message"),
            "At least one field must be provided for update",
            "case {case}: unexpected message, got {:?}",
            refused.body
        );
    }
}

/// The clause this service states about one failing member: the value — with the LENGTH a
/// `@length` check counted, where it counted one — the member as the JSON pointer that names
/// it in the request body, and the requirement it failed.
fn constraint_clause(length: Option<usize>, pointer: &str, requirement: &str) -> String {
    let subject = match length {
        Some(counted) => format!("Value with length {counted} "),
        None => "Value ".to_string(),
    };
    format!("{subject}at '{pointer}' failed to satisfy constraint: {requirement}")
}

/// `count` well-formed entries for a `CreateBrandProfileAttributes` batch: each satisfies
/// every constraint `BrandProfileAttributeInput` declares, so the only thing a request
/// carrying them can break is the `@length` on the LIST that holds them
/// (`smithy_query(query="shape", shape="BrandProfileAttributeInput")`).
fn attribute_entries(count: usize) -> Vec<Value> {
    (0..count)
        .map(|index| {
            json!({
                "attributeName": format!("Entry_{index}"),
                "attributeType": "TEXT",
                "attributeValue": "v",
            })
        })
        .collect()
}

/// A `ListBrandProfiles` request whose query-bound `nextToken` is one character past the
/// `@length` maximum of 2048 the member declares
/// (`smithy_query(query="operation", operation="ListBrandProfiles")`), built once because a
/// row of the table below names a `&'static str` path.
static NEXT_TOKEN_PAST_ITS_MAXIMUM: LazyLock<String> =
    LazyLock::new(|| format!("/v1/brand-profiles?nextToken={}", "t".repeat(2049)));

/// One row of the constraint table below: the case it names, the request that breaks one
/// modelled constraint (method, path, body), the length the refusal must state about the
/// value (`None` where the class states none), and the clause's member pointer and
/// requirement.
type ConstraintRow = (
    &'static str,
    &'static str,
    &'static str,
    Option<Value>,
    Option<usize>,
    &'static str,
    String,
);

#[tokio::test]
async fn a_modelled_constraint_refusal_is_the_counted_pointer_envelope_with_a_field_list() {
    let em = Emulator::new();

    // One row per class of modelled constraint, spread over the operations and depths
    // this service declares them at rather than over the one request a report cited.
    // Sources for the bounds and value sets: `smithy_query(query="shape",
    // shape="RegistrationIdOrArn,BrandProfileName,CodeConfigurationParameters,CodeType,\
    // MaxResults,Tag,CreateBrandProfileAttributesInput,ClientToken")`.
    let rows: Vec<ConstraintRow> = vec![
        (
            "a body member outside its @pattern",
            "POST",
            "/v1/brand-profiles/create-from-registration",
            Some(json!({
                "registrationId": "not a valid registration id!!",
                "brandProfileName": "from-registration",
            })),
            None,
            "/registrationId",
            "Member must satisfy regular expression pattern: ^[A-Za-z0-9_:/-]+$".to_string(),
        ),
        (
            "a required body member absent",
            "POST",
            "/v1/brand-profiles",
            Some(json!({})),
            None,
            "/brandProfileName",
            "Member must not be null".to_string(),
        ),
        (
            "a body member past its @length maximum",
            "POST",
            "/v1/brand-profiles",
            Some(json!({ "brandProfileName": "n".repeat(65) })),
            Some(65),
            "/brandProfileName",
            "Member must have length between 1 and 64, inclusive".to_string(),
        ),
        (
            "a body member below its @length minimum",
            "PUT",
            "/v1/brand-profiles/bp-00000000000000000/attributes/CompanyName",
            Some(json!({ "category": "" })),
            Some(0),
            "/category",
            "Member must have length between 1 and 64, inclusive".to_string(),
        ),
        (
            "a @sensitive body member past its @length maximum",
            "POST",
            "/v1/brand-profiles",
            Some(json!({ "brandProfileName": "tokened", "clientToken": "t".repeat(129) })),
            Some(129),
            "/clientToken",
            "Member must have length between 1 and 128, inclusive".to_string(),
        ),
        (
            "a LIST member past its @length maximum: its elements are counted",
            "POST",
            "/v1/brand-profiles/bp-00000000000000000/attributes",
            Some(json!({ "attributes": attribute_entries(11) })),
            Some(11),
            "/attributes",
            "Member must have length between 1 and 10, inclusive".to_string(),
        ),
        (
            "an empty LIST member, below the same @length minimum",
            "POST",
            "/v1/brand-profiles/bp-00000000000000000/attributes",
            Some(json!({ "attributes": attribute_entries(0) })),
            Some(0),
            "/attributes",
            "Member must have length between 1 and 10, inclusive".to_string(),
        ),
        (
            "a nested member above its @range",
            "POST",
            "/v1/notify-code-configurations",
            Some(json!({
                "notifyCodeConfigurationName": "policy",
                "codeConfigurationParameters": { "codeLength": 9 },
            })),
            None,
            "/codeConfigurationParameters/codeLength",
            "Member must be between 4 and 8, inclusive".to_string(),
        ),
        (
            "the same nested member below its @range: the same window",
            "POST",
            "/v1/notify-code-configurations",
            Some(json!({
                "notifyCodeConfigurationName": "policy",
                "codeConfigurationParameters": { "codeLength": 3 },
            })),
            None,
            "/codeConfigurationParameters/codeLength",
            "Member must be between 4 and 8, inclusive".to_string(),
        ),
        (
            "another nested member of the same structure",
            "POST",
            "/v1/notify-code-configurations",
            Some(json!({
                "notifyCodeConfigurationName": "policy",
                "codeConfigurationParameters": { "validityPeriodMinutes": 61 },
            })),
            None,
            "/codeConfigurationParameters/validityPeriodMinutes",
            "Member must be between 1 and 60, inclusive".to_string(),
        ),
        (
            "a nested member past its own @length maximum",
            "POST",
            "/v1/notify-code-configurations",
            Some(json!({
                "notifyCodeConfigurationName": "policy",
                "channelParameters": { "text": { "inlineTemplateBody": "b".repeat(6001) } },
            })),
            Some(6001),
            "/channelParameters/text/inlineTemplateBody",
            "Member must have length between 1 and 6000, inclusive".to_string(),
        ),
        (
            "a nested member outside its enumerated value set",
            "POST",
            "/v1/notify-code-configurations",
            Some(json!({
                "notifyCodeConfigurationName": "policy",
                "codeConfigurationParameters": { "codeType": "ROMAN" },
            })),
            None,
            "/codeConfigurationParameters/codeType",
            "Member must satisfy enum value set: [NUMERIC, ALPHA, ALPHANUMERIC]".to_string(),
        ),
        (
            "a member of a list element, reported at its 0-based position",
            "POST",
            "/v1/brand-profiles",
            Some(json!({
                "brandProfileName": "tagged",
                "tags": [{ "key": "k".repeat(129), "value": "v" }],
            })),
            Some(129),
            "/tags/0/key",
            "Member must have length between 1 and 128, inclusive".to_string(),
        ),
        (
            "a member of a LATER list element",
            "POST",
            "/v1/brand-profiles",
            Some(json!({
                "brandProfileName": "tagged",
                "tags": [
                    { "key": "env", "value": "prod" },
                    { "key": "k".repeat(129), "value": "v" },
                ],
            })),
            Some(129),
            "/tags/1/key",
            "Member must have length between 1 and 128, inclusive".to_string(),
        ),
        (
            "a query-bound member below its @range",
            "GET",
            "/v1/brand-profiles?maxResults=0",
            None,
            None,
            "/maxResults",
            "Member must be between 1 and 100, inclusive".to_string(),
        ),
        (
            "a query-bound member above the same @range",
            "GET",
            "/v1/brand-profiles?maxResults=101",
            None,
            None,
            "/maxResults",
            "Member must be between 1 and 100, inclusive".to_string(),
        ),
        (
            "a query-bound member past its @length maximum",
            "GET",
            NEXT_TOKEN_PAST_ITS_MAXIMUM.as_str(),
            None,
            Some(2049),
            "/nextToken",
            "Member must have length between 1 and 2048, inclusive".to_string(),
        ),
    ];

    for (case, method, path, body, length, pointer, requirement) in rows {
        let refused = em.call(method, path, body).await;
        assert_eq!(
            refused.status, 400,
            "case {case:?}: expected 400, got {} / {:?}",
            refused.status, refused.body
        );
        assert_eq!(
            refused.error.as_deref(),
            Some("ValidationException"),
            "case {case:?}: unexpected error code, got {:?}",
            refused.error
        );
        let clause = constraint_clause(length, pointer, &requirement);
        let expected = format!("1 validation error detected. {clause}");
        assert_eq!(
            refused.string("message"),
            expected,
            "case {case:?}: expected message {expected:?}, got {:?}",
            refused.body
        );
        assert_eq!(
            refused.body["fieldList"],
            json!([{ "path": pointer, "message": clause }]),
            "case {case:?}: the refusal must list the member at fault, got {:?}",
            refused.body
        );
    }
}

#[tokio::test]
async fn a_length_clause_states_the_size_the_request_itself_carried() {
    // The number in a `@length` clause is a MEASUREMENT of the request, so it moves with
    // the value: the rows send the same member at several sizes, in characters for a string
    // and in elements for a list, so a clause stating a fixed number — the bound, or the
    // size one recorded request happened to send — fails here.
    let em = Emulator::new();

    // (case, method, path, body, the member's pointer, its declared window, the length the
    //  clause must state)
    let rows: Vec<(String, &str, String, Value, &str, &str, usize)> = [65usize, 100, 1000]
        .into_iter()
        .map(|characters| {
            (
                format!("a name of {characters} characters"),
                "POST",
                "/v1/brand-profiles".to_string(),
                json!({ "brandProfileName": "n".repeat(characters) }),
                "/brandProfileName",
                "Member must have length between 1 and 64, inclusive",
                characters,
            )
        })
        .chain(std::iter::once((
            // `BrandProfileAttributeCategory` is bounded 1..64 and declares no `@pattern`
            // (`smithy_query(query="shape", shape="BrandProfileAttributeCategory")`), so an
            // empty value breaks its minimum alone.
            "an empty category".to_string(),
            "PUT",
            "/v1/brand-profiles/bp-00000000000000000/attributes/CompanyName".to_string(),
            json!({ "category": "" }),
            "/category",
            "Member must have length between 1 and 64, inclusive",
            0,
        )))
        .chain(std::iter::once((
            // `InlineTemplateBody`'s `@pattern` is `^[\s\S]*\S[\s\S]*$`, which admits any
            // character, so a multi-byte value breaks its length alone — and `@length`
            // counts CHARACTERS, not the bytes they take on the wire.
            "a template body of 6001 multi-byte characters".to_string(),
            "POST",
            "/v1/notify-code-configurations".to_string(),
            json!({
                "notifyCodeConfigurationName": "policy",
                "channelParameters": { "text": { "inlineTemplateBody": "\u{e9}".repeat(6001) } },
            }),
            "/channelParameters/text/inlineTemplateBody",
            "Member must have length between 1 and 6000, inclusive",
            6001,
        )))
        .chain([11usize, 12, 20].into_iter().map(|entries| {
            (
                format!("a batch of {entries} attributes"),
                "POST",
                "/v1/brand-profiles/bp-00000000000000000/attributes".to_string(),
                json!({ "attributes": attribute_entries(entries) }),
                "/attributes",
                "Member must have length between 1 and 10, inclusive",
                entries,
            )
        }))
        .collect();

    for (case, method, path, body, pointer, requirement, length) in rows {
        let refused = em.call(method, &path, Some(body)).await;
        assert_eq!(
            (refused.status, refused.error.as_deref()),
            (400, Some("ValidationException")),
            "case {case:?}: expected 400 ValidationException, got {} / {:?}",
            refused.status,
            refused.body
        );
        let clause = constraint_clause(Some(length), pointer, requirement);
        let expected = format!("1 validation error detected. {clause}");
        assert_eq!(
            refused.string("message"),
            expected,
            "case {case:?}: expected message {expected:?}, got {:?}",
            refused.body
        );
        assert_eq!(
            refused.body["fieldList"],
            json!([{ "path": pointer, "message": clause }]),
            "case {case:?}: the entry must state the same clause, got {:?}",
            refused.body
        );
    }
}

#[tokio::test]
async fn a_constraint_refusal_states_no_value_the_request_carried() {
    // The summary message reports the member and the rule, never the input: a caller's
    // own bytes — which can carry anything, and which some members of this service are
    // modelled `@sensitive` for — are not echoed back inside it.
    let em = Emulator::new();

    // (case, method, path, body, the submitted value no part of the refusal may carry)
    let rows: Vec<(&str, &str, &str, Option<Value>, String)> = vec![
        (
            "an identifier outside its pattern",
            "POST",
            "/v1/brand-profiles/create-from-registration",
            Some(json!({
                "registrationId": "not a valid registration id!!",
                "brandProfileName": "from-registration",
            })),
            "not a valid registration id!!".to_string(),
        ),
        (
            "a sensitive member outside its length",
            "POST",
            "/v1/notify-code-configurations",
            Some(json!({
                "notifyCodeConfigurationName": "policy",
                "clientToken": "t".repeat(129),
            })),
            "t".repeat(129),
        ),
        (
            "an over-long name",
            "POST",
            "/v1/brand-profiles",
            Some(json!({ "brandProfileName": "n".repeat(65) })),
            "n".repeat(65),
        ),
    ];

    for (case, method, path, body, submitted) in rows {
        let refused = em.call(method, path, body).await;
        assert_eq!(
            refused.status, 400,
            "case {case:?}: expected 400, got {} / {:?}",
            refused.status, refused.body
        );
        let rendered = refused.body.to_string();
        assert!(
            !rendered.contains(&submitted),
            "case {case:?}: the refusal must carry no submitted value, got {rendered}"
        );
    }
}

#[tokio::test]
async fn a_refusal_over_several_members_counts_them_and_lists_each() {
    // Two members of one request outside their own constraints: the envelope counts the
    // clauses, pluralises with the count, joins them, and lists every one of them.
    let em = Emulator::new();
    let refused = em
        .call(
            "POST",
            "/v1/brand-profiles/create-from-registration",
            Some(json!({
                "registrationId": "not a valid registration id!!",
                "brandProfileName": "a name that is far too long to be accepted here, and then \
                                     some more of it",
            })),
        )
        .await;
    assert_eq!(
        refused.status, 400,
        "expected 400, got {} / {:?}",
        refused.status, refused.body
    );
    let listed = refused.body["fieldList"]
        .as_array()
        .unwrap_or_else(|| panic!("fieldList is not an array in {}", refused.body))
        .clone();
    assert!(
        listed.len() >= 2,
        "both members at fault must be listed, got {:?}",
        refused.body
    );
    let clauses: Vec<String> = listed
        .iter()
        .map(|field| {
            field["message"]
                .as_str()
                .unwrap_or_else(|| panic!("a fieldList entry carries no message in {}", refused.body))
                .to_string()
        })
        .collect();
    let expected = format!(
        "{} validation errors detected. {}",
        listed.len(),
        clauses.join("; ")
    );
    assert_eq!(
        refused.string("message"),
        expected,
        "the envelope must count and join its clauses, got {:?}",
        refused.body
    );
    let pointers: Vec<&str> = listed
        .iter()
        .filter_map(|field| field["path"].as_str())
        .collect();
    for member in ["/registrationId", "/brandProfileName"] {
        assert!(
            pointers.contains(&member),
            "case {member:?}: every member at fault is listed, got {pointers:?}"
        );
    }
}

#[tokio::test]
async fn the_identifier_format_gate_names_its_member_and_states_one_sentence() {
    // The gate this service applies before any lookup states its one sentence twice: as
    // the summary, and as the entry for the member the value arrived on — named by the
    // member's own name, the spelling every refusal this service words itself uses.
    let em = Emulator::new();

    for identifier in malformed_single_segment_identifiers() {
        for (operation, reply) in [
            ("GetBrandProfile", em.get(&identifier).await),
            ("DeleteBrandProfile", em.delete(&identifier).await),
            (
                "UpdateBrandProfile",
                em.update(&identifier, json!({ "brandProfileName": "renamed" }))
                    .await,
            ),
            (
                "ListBrandProfileAttributes",
                em.list_attributes(&identifier).await,
            ),
            (
                "GetBrandProfileAttribute",
                em.get_attribute(&identifier, "CompanyName").await,
            ),
        ] {
            let case = format!("{operation} identifier={identifier:?}");
            assert_eq!(
                reply.status, 400,
                "case {case}: expected 400, got {} / {:?}",
                reply.status, reply.body
            );
            assert_eq!(
                reply.error.as_deref(),
                Some("ValidationException"),
                "case {case}: unexpected error code, got {:?}",
                reply.error
            );
            let stated = invalid_format_message(&identifier);
            assert_eq!(
                reply.string("message"),
                stated,
                "case {case}: expected message {stated:?}, got {:?}",
                reply.body
            );
            assert_eq!(
                reply.body["fieldList"],
                json!([{ "path": "brandProfileId", "message": stated }]),
                "case {case}: the gate must name the member it refused, got {:?}",
                reply.body
            );
        }
    }
}
