// SPDX-License-Identifier: CC0-1.0

//! The brand-profile lifecycle, end to end over the wire.
//!
//! Every assertion here goes through `MultiServiceRouter::dispatch`, so it exercises
//! the same restJson1 path a real SDK client takes: routing, deserialization,
//! boundary constraint validation, the handler, and error rendering.
//!
//! The rules under test, each over a SET of inputs rather than one recorded request:
//!
//! * a created profile is persisted, and `GetBrandProfile` / `ListBrandProfiles`
//!   report exactly what was stored — for every name the modelled `BrandProfileName`
//!   accepts, with or without `deletionProtectionEnabled` and `tags`;
//! * the `attributesCreated` a create reports is the number of attributes the new
//!   profile's own list holds (the template it was seeded with);
//! * a `BrandProfileIdOrArn` resolves a profile by its bare id or by its full ARN;
//! * the ARN is `arn:aws:end-user-messaging:<region>:<account>:brand-profile/<id>`
//!   for whatever region the request is signed for;
//! * `ListBrandProfiles` returns every stored profile exactly once across pages, for
//!   every page size, and emits a resumable token only while items remain;
//! * `UpdateBrandProfile` applies the members present and leaves the rest alone;
//! * `DeleteBrandProfile` removes the record, and refuses with `ConflictException`
//!   while deletion protection is on;
//! * an identifier of the documented form that names nothing is
//!   `ResourceNotFoundException` (404), and a value outside a modelled constraint is
//!   `ValidationException` (400);
//! * a repeated `clientToken` reports the first profile instead of creating another;
//! * `ListTagsForResource` reports the tags stored on the resource its `resourceArn`
//!   names — for every tag set a create may carry, and for every taggable resource
//!   type of this service;
//! * a `resourceArn` that is not the ARN of a resource of this service's own
//!   namespace — not an ARN at all, or one of another vendor — and one scoped to
//!   another Region, are `ValidationException` (400), and an ARN of this service and
//!   Region that names no stored resource — a resource type this service does not
//!   hold, an unknown identifier, a deleted resource — is `ResourceNotFoundException`
//!   (404).

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_endusermessaging_endusermessaging::EmulatedService;

/// The tag WRITE half of the same tagging contract, driven over the same wire:
/// `TagResource`, and the tag sets no request may carry. A child module of this suite,
/// in `tests/all/brand_profile_lifecycle/`. This crate's one test target declares each
/// suite DIRECTLY under `tests/all/` from `tests/all.rs`; a child module of an
/// already-declared suite is declared by its parent, here. The `#[path]` is relative to
/// this file's own directory, as it is in `all.rs`.
#[path = "brand_profile_lifecycle/tag_writes.rs"]
mod tag_writes;

/// This service's sigv4 signing name, from the model (`OPERATIONS.md`, "Wire protocol").
const SIGNING_NAME: &str = "end-user-messaging";

/// The account id every generated emulator presents, as `engine` defines it.
const ACCOUNT_ID: &str = engine::EMULATOR_ACCOUNT_ID;

/// How long to keep looking for a reported instant to advance before giving up:
/// generous, because the condition is the clock's second moving on, not a bound on
/// how fast the host serves a request.
const BUMP_DEADLINE: ::std::time::Duration = ::std::time::Duration::from_secs(30);

/// How long to wait between looks while waiting for a reported instant to advance.
const BUMP_POLL_INTERVAL: ::std::time::Duration = ::std::time::Duration::from_millis(20);

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
    fn member(&self, name: &str) -> &Value {
        &self.body[name]
    }

    fn string(&self, name: &str) -> String {
        self.member(name)
            .as_str()
            .unwrap_or_else(|| panic!("member {name} is not a string in {}", self.body))
            .to_string()
    }

    fn number(&self, name: &str) -> f64 {
        self.member(name)
            .as_f64()
            .unwrap_or_else(|| panic!("member {name} is not a number in {}", self.body))
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
        self.call("POST", "/v1/brand-profiles", Some(body)).await
    }

    async fn create_named(&self, name: &str) -> Reply {
        self.create(json!({ "brandProfileName": name })).await
    }

    async fn get(&self, id_or_arn: &str) -> Reply {
        self.call("GET", &format!("/v1/brand-profiles/{id_or_arn}"), None)
            .await
    }

    async fn update(&self, id_or_arn: &str, body: Value) -> Reply {
        self.call(
            "PUT",
            &format!("/v1/brand-profiles/{id_or_arn}"),
            Some(body),
        )
        .await
    }

    async fn delete(&self, id_or_arn: &str) -> Reply {
        self.call("DELETE", &format!("/v1/brand-profiles/{id_or_arn}"), None)
            .await
    }

    async fn list(&self, query: &str) -> Reply {
        self.call("GET", &format!("/v1/brand-profiles{query}"), None)
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

    async fn create_notify_code_configuration(&self, body: Value) -> Reply {
        self.call("POST", "/v1/notify-code-configurations", Some(body))
            .await
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
}

/// The `tags` a reply reports, as a sorted list of pairs: the API states no order for
/// the list, so every assertion below is on the SET of pairs it carries.
fn reported_tags(reply: &Reply) -> Vec<(String, String)> {
    let listed = reply
        .member("tags")
        .as_array()
        .unwrap_or_else(|| panic!("tags is not a list in {:?}", reply.body))
        .iter()
        .map(|tag| {
            (
                tag["key"]
                    .as_str()
                    .unwrap_or_else(|| panic!("a tag has no key in {:?}", reply.body))
                    .to_string(),
                tag["value"]
                    .as_str()
                    .unwrap_or_else(|| panic!("a tag has no value in {:?}", reply.body))
                    .to_string(),
            )
        });
    let mut pairs: Vec<(String, String)> = listed.collect();
    pairs.sort();
    pairs
}

/// A tag set as the wire carries it on a create.
fn tag_body(tags: &[(String, String)]) -> Value {
    Value::Array(
        tags.iter()
            .map(|(key, value)| json!({ "key": key, "value": value }))
            .collect(),
    )
}

/// `tags` sorted into the order [`reported_tags`] returns, so the two compare.
fn sorted(tags: &[(String, String)]) -> Vec<(String, String)> {
    let mut pairs = tags.to_vec();
    pairs.sort();
    pairs
}

/// The names in this table are the classes `BrandProfileName`'s modelled
/// `@length(1..64)` and `^[A-Za-z0-9_ -]*[A-Za-z0-9_-][A-Za-z0-9_ -]*$` pattern
/// accept: a single character, the alphanumeric case, every punctuation the pattern
/// names, an interior space, and exactly the maximum length.
fn accepted_names() -> Vec<String> {
    vec![
        "a".to_string(),
        "demo-brand".to_string(),
        "My_Brand profile-1".to_string(),
        "0".to_string(),
        "A".repeat(64),
    ]
}

#[tokio::test]
async fn a_created_profile_is_persisted_and_read_back_whole() {
    let em = Emulator::new();

    for name in accepted_names() {
        for protection in [None, Some(false), Some(true)] {
            let mut body = json!({ "brandProfileName": name });
            if let Some(enabled) = protection {
                body["deletionProtectionEnabled"] = json!(enabled);
            }
            let case = format!("name={name:?} deletionProtectionEnabled={protection:?}");

            let created = em.create(body).await;
            assert_eq!(
                created.status, 200,
                "case {case}: expected 200, got {} / {:?}",
                created.status, created.body
            );
            let id = created.string("brandProfileId");
            assert_eq!(
                created.string("brandProfileName"),
                name,
                "case {case}: the created name must be the one requested, got {:?}",
                created.body
            );
            assert_eq!(
                created.string("status"),
                "ACTIVE",
                "case {case}: a created profile is ACTIVE, got {:?}",
                created.body
            );
            // The create seeds the default attribute template, so the count it
            // reports is positive and is what the profile's attribute list holds.
            let seeded = created.number("attributesCreated");
            assert!(
                seeded > 0.0,
                "case {case}: a created profile is seeded with a default attribute \
                 template, so attributesCreated is positive, got {seeded:?}"
            );
            let attributes = em.list_attributes(&id).await;
            let listed = attributes
                .member("brandProfileAttributes")
                .as_array()
                .unwrap_or_else(|| {
                    panic!(
                        "case {case}: brandProfileAttributes is not a list, got {:?}",
                        attributes.body
                    )
                })
                .len();
            assert_eq!(
                listed as f64, seeded,
                "case {case}: attributesCreated must equal the number of attributes the \
                 list reports, got {seeded:?} vs {listed}"
            );
            assert_eq!(
                created.member("deletionProtectionEnabled"),
                &json!(protection.unwrap_or(false)),
                "case {case}: an omitted deletionProtectionEnabled defaults to false, got {:?}",
                created.body
            );
            assert_eq!(
                created.string("brandProfileArn"),
                format!("arn:aws:{SIGNING_NAME}:us-east-1:{ACCOUNT_ID}:brand-profile/{id}"),
                "case {case}: unexpected ARN, got {:?}",
                created.body
            );
            assert_eq!(
                created.number("createdAt"),
                created.number("updatedAt"),
                "case {case}: a never-updated profile has createdAt == updatedAt, got {:?}",
                created.body
            );

            // Round trip: the reader reports exactly what create stored.
            let fetched = em.get(&id).await;
            assert_eq!(
                fetched.status, 200,
                "case {case}: GetBrandProfile must find the profile just created, got {} / {:?}",
                fetched.status, fetched.body
            );
            for member in [
                "brandProfileId",
                "brandProfileArn",
                "brandProfileName",
                "status",
                "deletionProtectionEnabled",
                "createdAt",
                "updatedAt",
            ] {
                assert_eq!(
                    fetched.member(member),
                    created.member(member),
                    "case {case}: GetBrandProfile disagrees with CreateBrandProfile on {member}: \
                     {:?} vs {:?}",
                    fetched.member(member),
                    created.member(member)
                );
            }

            // And the same record is what the list reports.
            let listed = em.list("").await;
            let found = listed
                .member("brandProfiles")
                .as_array()
                .unwrap_or_else(|| panic!("case {case}: brandProfiles is not a list"))
                .iter()
                .find(|item| item["brandProfileId"] == json!(id))
                .cloned()
                .unwrap_or_else(|| {
                    panic!(
                        "case {case}: the created profile must be listed, got {:?}",
                        listed.body
                    )
                });
            assert_eq!(
                found["brandProfileName"],
                json!(name),
                "case {case}: the listed name must match, got {found:?}"
            );
        }
    }
}

#[tokio::test]
async fn a_profile_resolves_by_bare_id_and_by_full_arn() {
    let em = Emulator::new();
    let created = em.create_named("resolve-me").await;
    let id = created.string("brandProfileId");
    let arn = created.string("brandProfileArn");

    for reference in [id.clone(), arn.clone()] {
        let fetched = em.get(&reference).await;
        assert_eq!(
            fetched.status, 200,
            "case {reference:?}: a BrandProfileIdOrArn resolves by id or ARN, got {} / {:?}",
            fetched.status, fetched.body
        );
        assert_eq!(
            fetched.string("brandProfileId"),
            id,
            "case {reference:?}: resolved the wrong profile, got {:?}",
            fetched.body
        );
    }

    // An ARN naming a profile that does not exist is still a 404, not a synthesized one.
    let bogus = format!("arn:aws:{SIGNING_NAME}:us-east-1:{ACCOUNT_ID}:brand-profile/bp-00000000000000000");
    let missing = em.get(&bogus).await;
    assert_eq!(
        missing.status, 404,
        "case {bogus:?}: an unknown ARN is ResourceNotFoundException, got {} / {:?}",
        missing.status, missing.body
    );

    // The ARN of a profile deleted afterwards no longer resolves.
    assert_eq!(
        em.delete(&arn).await.status,
        200,
        "deleting by ARN must succeed"
    );
    assert_eq!(
        em.get(&arn).await.status,
        404,
        "a deleted profile's ARN must stop resolving"
    );
}

#[tokio::test]
async fn the_arn_carries_the_region_the_request_was_signed_for() {
    // Every row is a real AWS region name; the rule is the ARN's region segment is
    // the request's, not a fixed one.
    for region in ["us-east-1", "eu-west-1", "ap-southeast-2", "us-gov-west-1"] {
        let em = Emulator::in_region(region);
        let created = em.create_named("regional").await;
        let id = created.string("brandProfileId");
        assert_eq!(
            created.string("brandProfileArn"),
            format!("arn:aws:{SIGNING_NAME}:{region}:{ACCOUNT_ID}:brand-profile/{id}"),
            "case {region}: the ARN must carry the signed region, got {:?}",
            created.body
        );
    }
}

#[tokio::test]
async fn every_page_size_walks_the_whole_list_exactly_once() {
    const PROFILES: usize = 7;
    let em = Emulator::new();
    let mut created: Vec<String> = Vec::new();
    for index in 0..PROFILES {
        created.push(
            em.create_named(&format!("page-{index}"))
                .await
                .string("brandProfileId"),
        );
    }
    created.sort();

    // Each row is a page size: both published bounds of `maxResults` (1 and 100), an
    // interior size that divides the set, one that does not, and no `maxResults` at all.
    for page_size in [Some(1), Some(2), Some(3), Some(7), Some(100), None] {
        let case = format!("maxResults={page_size:?}");
        let mut seen: Vec<String> = Vec::new();
        let mut token: Option<String> = None;
        let mut pages = 0;

        loop {
            let mut query = Vec::new();
            if let Some(size) = page_size {
                query.push(format!("maxResults={size}"));
            }
            if let Some(ref next) = token {
                query.push(format!("nextToken={next}"));
            }
            let query = if query.is_empty() {
                String::new()
            } else {
                format!("?{}", query.join("&"))
            };

            let page = em.list(&query).await;
            assert_eq!(
                page.status, 200,
                "case {case}: ListBrandProfiles must succeed, got {} / {:?}",
                page.status, page.body
            );
            let items = page
                .member("brandProfiles")
                .as_array()
                .unwrap_or_else(|| panic!("case {case}: brandProfiles is not a list"))
                .clone();
            if let Some(size) = page_size {
                assert!(
                    items.len() <= size,
                    "case {case}: a page must not exceed maxResults, got {} items",
                    items.len()
                );
            }
            for item in &items {
                seen.push(
                    item["brandProfileId"]
                        .as_str()
                        .unwrap_or_else(|| panic!("case {case}: an item has no brandProfileId"))
                        .to_string(),
                );
            }
            pages += 1;
            assert!(
                pages <= PROFILES + 1,
                "case {case}: paging did not terminate after {pages} pages"
            );

            match page.member("nextToken").as_str() {
                Some(next) => {
                    assert!(
                        !items.is_empty(),
                        "case {case}: a token must not be emitted beside an empty page"
                    );
                    token = Some(next.to_string());
                }
                None => break,
            }
        }

        assert_eq!(
            seen, created,
            "case {case}: paging must yield every profile exactly once, in id order"
        );
    }
}

#[tokio::test]
async fn an_update_applies_only_the_members_it_carries() {
    let em = Emulator::new();

    // Each row is one update body carrying at least one updatable member; `expected`
    // is (name, deletionProtectionEnabled) after it is applied to a profile created
    // as ("before", false). A body carrying NO updatable member is refused rather
    // than applied, which `brand_profile_refusal_bodies` pins.
    let cases: Vec<(Value, &str, bool)> = vec![
        (json!({ "brandProfileName": "after" }), "after", false),
        (json!({ "deletionProtectionEnabled": true }), "before", true),
        (
            json!({ "brandProfileName": "both", "deletionProtectionEnabled": true }),
            "both",
            true,
        ),
    ];

    for (body, expected_name, expected_protection) in cases {
        let case = format!("update={body}");
        let created = em
            .create(json!({ "brandProfileName": "before", "deletionProtectionEnabled": false }))
            .await;
        let id = created.string("brandProfileId");

        // A reported instant is the whole epoch second it falls in, so an update
        // served inside the same second as the create reports that same instant.
        // Apply the update again until the reported instant advances — which it does
        // as soon as the clock's second moves on — instead of assuming the host is
        // slow enough for one update to land in a later second. Each row's update is
        // the same body applied to the same profile, so repeating it changes nothing
        // but the instant.
        let bump_deadline = ::std::time::Instant::now() + BUMP_DEADLINE;
        let mut updated = em.update(&id, body.clone()).await;
        while updated.status == 200
            && updated.number("updatedAt") <= created.number("updatedAt")
            && ::std::time::Instant::now() < bump_deadline
        {
            ::tokio::time::sleep(BUMP_POLL_INTERVAL).await;
            updated = em.update(&id, body.clone()).await;
        }
        assert_eq!(
            updated.status, 200,
            "case {case}: UpdateBrandProfile must succeed, got {} / {:?}",
            updated.status, updated.body
        );
        assert_eq!(
            updated.string("brandProfileName"),
            expected_name,
            "case {case}: unexpected name after update, got {:?}",
            updated.body
        );
        assert_eq!(
            updated.member("deletionProtectionEnabled"),
            &json!(expected_protection),
            "case {case}: unexpected deletionProtectionEnabled after update, got {:?}",
            updated.body
        );
        assert_eq!(
            updated.number("createdAt"),
            created.number("createdAt"),
            "case {case}: an update must not move createdAt, got {:?}",
            updated.body
        );
        assert!(
            updated.number("updatedAt") > created.number("updatedAt"),
            "case {case}: an update must bump updatedAt, got {} then {}",
            created.number("updatedAt"),
            updated.number("updatedAt")
        );
        assert_eq!(
            updated.string("brandProfileArn"),
            created.string("brandProfileArn"),
            "case {case}: an update must not re-mint the ARN, got {:?}",
            updated.body
        );

        // The mutation is persisted, not just reported.
        let fetched = em.get(&id).await;
        for member in ["brandProfileName", "deletionProtectionEnabled", "updatedAt"] {
            assert_eq!(
                fetched.member(member),
                updated.member(member),
                "case {case}: GetBrandProfile must reflect the update's {member}, got {:?}",
                fetched.body
            );
        }

        // Leave the store clean for the next row.
        if expected_protection {
            em.update(&id, json!({ "deletionProtectionEnabled": false }))
                .await;
        }
        assert_eq!(
            em.delete(&id).await.status,
            200,
            "case {case}: tearing the profile down must succeed"
        );
    }
}

#[tokio::test]
async fn deletion_protection_refuses_the_delete_until_it_is_turned_off() {
    let em = Emulator::new();
    let created = em
        .create(json!({ "brandProfileName": "protected", "deletionProtectionEnabled": true }))
        .await;
    let id = created.string("brandProfileId");

    let refused = em.delete(&id).await;
    assert_eq!(
        refused.status, 409,
        "a protected profile's delete is ConflictException (409), got {} / {:?}",
        refused.status, refused.body
    );
    assert_eq!(
        refused.error.as_deref(),
        Some("ConflictException"),
        "unexpected error code, got {:?}",
        refused.error
    );
    assert_eq!(
        em.get(&id).await.status,
        200,
        "a refused delete must leave the profile in place"
    );

    assert_eq!(
        em.update(&id, json!({ "deletionProtectionEnabled": false }))
            .await
            .status,
        200,
        "turning protection off must succeed"
    );

    let deleted = em.delete(&id).await;
    assert_eq!(
        deleted.status, 200,
        "an unprotected profile deletes, got {} / {:?}",
        deleted.status, deleted.body
    );
    assert_eq!(
        deleted.string("brandProfileId"),
        id,
        "delete must report the id it removed, got {:?}",
        deleted.body
    );
    assert_eq!(
        deleted.string("brandProfileArn"),
        created.string("brandProfileArn"),
        "delete must report the ARN it removed, got {:?}",
        deleted.body
    );

    assert_eq!(
        em.get(&id).await.status,
        404,
        "a deleted profile must not be gettable"
    );
    let listed = em.list("").await;
    assert_eq!(
        listed.member("brandProfiles"),
        &json!([]),
        "a deleted profile must not be listed, got {:?}",
        listed.body
    );
}

#[tokio::test]
async fn an_id_that_names_nothing_is_a_404_on_every_operation_that_takes_one() {
    let em = Emulator::new();
    // Rows: two identifiers of the documented form that were never minted, and the id
    // of a deleted profile. Each is a well-formed `brandProfileId`, so what is missing
    // is the resource and not the form of the identifier.
    let deleted = em.create_named("gone").await.string("brandProfileId");
    assert_eq!(em.delete(&deleted).await.status, 200, "teardown must work");

    for reference in [
        "bp-nosuchprofile0000",
        "bp-abc12345678901234",
        deleted.as_str(),
    ] {
        for (method, body) in [
            ("GET", None),
            ("PUT", Some(json!({ "brandProfileName": "x" }))),
            ("DELETE", None),
        ] {
            let case = format!("{method} {reference}");
            let reply = em
                .call(
                    method,
                    &format!("/v1/brand-profiles/{reference}"),
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
        }
    }
}

#[tokio::test]
async fn a_value_outside_a_modelled_constraint_is_a_400_validation_exception() {
    let em = Emulator::new();

    // `brandProfileName` is @length(1..64) with pattern
    // ^[A-Za-z0-9_ -]*[A-Za-z0-9_-][A-Za-z0-9_ -]*$: both bounds, one past the upper
    // bound, and a character the pattern excludes.
    let bad_names = vec![
        String::new(),
        " ".to_string(),
        "bad!name".to_string(),
        "no.dots".to_string(),
        "A".repeat(65),
    ];
    for name in bad_names {
        let reply = em.create(json!({ "brandProfileName": name })).await;
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

    // `maxResults` is @range(1..100): each published bound is accepted, each value
    // one step outside it is refused.
    for accepted in ["1", "100"] {
        let reply = em.list(&format!("?maxResults={accepted}")).await;
        assert_eq!(
            reply.status, 200,
            "case maxResults={accepted}: a published bound must be accepted, got {} / {:?}",
            reply.status, reply.body
        );
    }
    for refused in ["0", "101"] {
        let reply = em.list(&format!("?maxResults={refused}")).await;
        assert_eq!(
            reply.status, 400,
            "case maxResults={refused}: expected 400, got {} / {:?}",
            reply.status, reply.body
        );
        assert_eq!(
            reply.error.as_deref(),
            Some("ValidationException"),
            "case maxResults={refused}: unexpected error code, got {:?}",
            reply.error
        );
    }

    // A `nextToken` this service never issued is refused rather than silently
    // treated as "start from the beginning" or as some other resume point. The rows
    // span the ways a fabricated token can fail: not hex at all, odd-length hex,
    // hex with a separator, valid hex that decodes to text carrying no token marker
    // ("6262" -> "bb"), and valid hex carrying a marker that is not this one.
    let unissued_tokens = vec![
        "notatoken".to_string(),
        "zz".to_string(),
        "6e6f-74-hex".to_string(),
        "6262".to_string(),
        hex::encode("bp-0123456789abcdef0"),
        hex::encode("SomeOtherOperation/1/bp-0123456789abcdef0"),
    ];
    for token in unissued_tokens {
        let reply = em.list(&format!("?nextToken={token}")).await;
        assert_eq!(
            reply.status, 400,
            "case nextToken={token:?}: expected 400, got {} / {:?}",
            reply.status, reply.body
        );
        assert_eq!(
            reply.error.as_deref(),
            Some("ValidationException"),
            "case nextToken={token:?}: unexpected error code, got {:?}",
            reply.error
        );
    }
}

#[tokio::test]
async fn a_repeated_client_token_reports_the_first_profile() {
    let em = Emulator::new();

    // Rows differ in what the repeat asks for: the same body, which is the repeat an
    // idempotent retry sends and which answers the first result; and two that differ
    // from it — a different name, and a different deletionProtectionEnabled — which are
    // a reuse of the token for ANOTHER request and are refused rather than served the
    // first profile.
    let repeats: Vec<(Value, u16)> = vec![
        (json!({ "brandProfileName": "first" }), 200),
        (
            json!({ "brandProfileName": "first", "tags": [] }),
            200,
        ),
        (json!({ "brandProfileName": "second" }), 400),
        (
            json!({ "brandProfileName": "first", "deletionProtectionEnabled": true }),
            400,
        ),
        (
            json!({ "brandProfileName": "first", "tags": [{ "key": "env", "value": "prod" }] }),
            400,
        ),
    ];

    let served = repeats
        .iter()
        .filter(|(_, expected)| *expected == 200)
        .count();
    for (index, (repeat, expected)) in repeats.into_iter().enumerate() {
        let token = format!("token-{index}");
        let case = format!("clientToken={token} repeat={repeat}");
        let first = em
            .create(json!({ "brandProfileName": "first", "clientToken": token }))
            .await;
        assert_eq!(
            first.status, 200,
            "case {case}: the first create must succeed, got {} / {:?}",
            first.status, first.body
        );

        let mut repeat_body = repeat;
        repeat_body["clientToken"] = json!(token);
        let again = em.create(repeat_body).await;
        assert_eq!(
            again.status, expected,
            "case {case}: expected {expected}, got {} / {:?}",
            again.status, again.body
        );
        if expected == 200 {
            assert_eq!(
                again.body, first.body,
                "case {case}: a repeated clientToken must report the first result"
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
            again.string("message"),
            "The idempotency token and parameters presented do not match",
            "case {case}: unexpected refusal message, got {:?}",
            again.body
        );
        // The refused replay changed nothing: the profile the token minted still
        // carries the parameters of the request it served.
        let stored = em.get(&first.string("brandProfileId")).await;
        assert_eq!(
            (
                stored.string("brandProfileName"),
                stored.member("deletionProtectionEnabled").clone()
            ),
            ("first".to_string(), json!(false)),
            "case {case}: a refused replay must leave the first profile as it was, got {:?}",
            stored.body
        );
    }

    // One profile per token, and the repeats that were served created nothing extra.
    let listed = em.list("").await;
    assert_eq!(
        listed
            .member("brandProfiles")
            .as_array()
            .map(Vec::len)
            .unwrap_or_default(),
        5,
        "a repeated clientToken must not create a second profile, got {:?}",
        listed.body
    );
    assert_eq!(
        served, 2,
        "the table must cover the repeat that IS idempotent, got {served} such rows"
    );
}

#[tokio::test]
async fn the_store_is_bounded_and_refuses_a_create_past_its_ceiling() {
    use service_com_amazonaws_endusermessaging_endusermessaging::state::MAX_BRAND_PROFILES;

    let em = Emulator::new();
    for index in 0..MAX_BRAND_PROFILES {
        let reply = em.create_named(&format!("bounded-{index}")).await;
        assert_eq!(
            reply.status, 200,
            "case index={index}: a create below the ceiling must succeed, got {} / {:?}",
            reply.status, reply.body
        );
    }

    // Rows: a plain create, one with tags, and one with a client token. None of
    // them may grow the store past the ceiling.
    let bodies = vec![
        json!({ "brandProfileName": "one-too-many" }),
        json!({ "brandProfileName": "one-too-many", "tags": [{ "key": "k", "value": "v" }] }),
        json!({ "brandProfileName": "one-too-many", "clientToken": "fresh-token" }),
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
    // stored, and paging still walks exactly the stored set.
    let mut counted = 0usize;
    let mut token: Option<String> = None;
    loop {
        let query = match token {
            Some(ref next) => format!("?maxResults=100&nextToken={next}"),
            None => "?maxResults=100".to_string(),
        };
        let page = em.list(&query).await;
        counted += page
            .member("brandProfiles")
            .as_array()
            .map(Vec::len)
            .unwrap_or_default();
        match page.member("nextToken").as_str() {
            Some(next) => token = Some(next.to_string()),
            None => break,
        }
    }
    assert_eq!(
        counted, MAX_BRAND_PROFILES,
        "the store must hold exactly its ceiling, got {counted}"
    );

    // Freeing a slot lets a create succeed again.
    let first = em.list("?maxResults=1").await;
    let victim = first.member("brandProfiles")[0]["brandProfileId"]
        .as_str()
        .expect("a stored profile to delete")
        .to_string();
    assert_eq!(
        em.delete(&victim).await.status,
        200,
        "deleting a stored profile must succeed"
    );
    let after = em.create_named("room-again").await;
    assert_eq!(
        after.status, 200,
        "a create must succeed once a slot is free, got {} / {:?}",
        after.status, after.body
    );
}

/// Each row is a tag set a create may carry, spanning what the modelled `TagList` and
/// `Tag` accept (`smithy_query(query="shape",shape="TagList,Tag")`: `TagList`
/// `@length(0..50)`, `Tag$key` `@length(1..128)`, `Tag$value` `@length(0..256)`): no
/// tags at all, one pair, several, a pair whose value is the empty string the value's
/// lower bound allows, a key and a value at exactly their maximum lengths, and a list
/// at its own maximum length.
fn tag_sets() -> Vec<(&'static str, Vec<(String, String)>)> {
    vec![
        ("no tags", Vec::new()),
        ("one pair", vec![("env".to_string(), "prod".to_string())]),
        (
            "several pairs",
            vec![
                ("env".to_string(), "prod".to_string()),
                ("team".to_string(), "messaging".to_string()),
                ("cost-center".to_string(), "42".to_string()),
            ],
        ),
        (
            "an empty value",
            vec![("env".to_string(), String::new())],
        ),
        (
            "a key and a value at their maximum length",
            vec![("k".repeat(128), "v".repeat(256))],
        ),
        (
            "a list at its maximum length",
            (0..50)
                .map(|index| (format!("key-{index}"), format!("value-{index}")))
                .collect(),
        ),
    ]
}

#[tokio::test]
async fn a_brand_profiles_tags_are_reported_for_its_own_arn() {
    for (case, tags) in tag_sets() {
        let em = Emulator::new();
        // A second, differently tagged profile stands beside each row, so a reply
        // that reported the store's tags rather than this resource's would fail.
        let other = em
            .create(json!({
                "brandProfileName": "other-profile",
                "tags": [{ "key": "owner", "value": "other" }],
            }))
            .await;
        let created = em
            .create(json!({
                "brandProfileName": "tagged-profile",
                "tags": tag_body(&tags),
            }))
            .await;
        assert_eq!(
            created.status, 200,
            "case {case}: the create must succeed, got {} / {:?}",
            created.status, created.body
        );
        let arn = created.string("brandProfileArn");

        let listed = em.list_tags(&arn).await;
        assert_eq!(
            listed.status, 200,
            "case {case}: ListTagsForResource must report the created profile's tags, \
             got {} / {:?}",
            listed.status, listed.body
        );
        assert_eq!(
            reported_tags(&listed),
            sorted(&tags),
            "case {case}: unexpected tags for {arn}, got {:?}",
            listed.body
        );

        let neighbour = em.list_tags(&other.string("brandProfileArn")).await;
        assert_eq!(
            reported_tags(&neighbour),
            vec![("owner".to_string(), "other".to_string())],
            "case {case}: a resource reports only its own tags, got {:?}",
            neighbour.body
        );

        // A read changes nothing a later caller sees: the same ARN reports the same
        // tags, and the resource itself is untouched.
        let again = em.list_tags(&arn).await;
        assert_eq!(
            reported_tags(&again),
            sorted(&tags),
            "case {case}: a second read must report the same tags, got {:?}",
            again.body
        );
        let fetched = em.get(&arn).await;
        assert_eq!(
            fetched.number("updatedAt"),
            created.number("updatedAt"),
            "case {case}: reading tags must not move the resource's updatedAt, got {:?}",
            fetched.body
        );
    }
}

#[tokio::test]
async fn a_notify_code_configurations_tags_are_reported_for_its_own_arn() {
    // The rule is about the resource an ARN names, not about one resource type, so
    // every taggable resource of this service answers it; the notify code
    // configuration is the other one that carries `tags` at create time.
    for (case, tags) in tag_sets() {
        let em = Emulator::new();
        let created = em
            .create_notify_code_configuration(json!({
                "notifyCodeConfigurationName": "tagged-configuration",
                "tags": tag_body(&tags),
            }))
            .await;
        assert_eq!(
            created.status, 200,
            "case {case}: the create must succeed, got {} / {:?}",
            created.status, created.body
        );
        let arn = created.member("notifyCodeConfiguration")["notifyCodeConfigurationArn"]
            .as_str()
            .unwrap_or_else(|| {
                panic!(
                    "case {case}: the create reported no ARN, got {:?}",
                    created.body
                )
            })
            .to_string();

        let listed = em.list_tags(&arn).await;
        assert_eq!(
            listed.status, 200,
            "case {case}: ListTagsForResource must report the configuration's tags, got \
             {} / {:?}",
            listed.status, listed.body
        );
        assert_eq!(
            reported_tags(&listed),
            sorted(&tags),
            "case {case}: unexpected tags for {arn}, got {:?}",
            listed.body
        );
    }
}

#[tokio::test]
async fn an_arn_this_request_cannot_reach_is_refused_as_invalid() {
    // Every row is a real AWS region name; the rule is that a resourceArn names a
    // resource in the Region the request was made in, whichever Region that is.
    let regions = ["us-east-1", "eu-west-1", "ap-southeast-2", "us-gov-west-1"];
    for request_region in regions {
        let em = Emulator::in_region(request_region);
        let created = em
            .create(json!({
                "brandProfileName": "regional-profile",
                "tags": [{ "key": "env", "value": "prod" }],
            }))
            .await;
        let arn = created.string("brandProfileArn");

        // Its own Region reaches it.
        let listed = em.list_tags(&arn).await;
        assert_eq!(
            listed.status, 200,
            "case {request_region}: the resource's own ARN must be reachable, got {} / {:?}",
            listed.status, listed.body
        );

        for other_region in regions.iter().filter(|region| **region != request_region) {
            let foreign = arn.replace(request_region, other_region);
            let case = format!("request={request_region} arn={foreign}");
            let refused = em.list_tags(&foreign).await;
            assert_eq!(
                refused.status, 400,
                "case {case}: an ARN from another Region is a ValidationException (400), \
                 got {} / {:?}",
                refused.status, refused.body
            );
            assert_eq!(
                refused.error.as_deref(),
                Some("ValidationException"),
                "case {case}: unexpected error code, got {:?}",
                refused.error
            );
            assert_eq!(
                refused.member("fieldList")[0]["path"],
                json!("/resourceArn"),
                "case {case}: the refusal must name the member it refused, got {:?}",
                refused.body
            );
        }
    }

    // Each row is a value the modelled `^arn:[A-Za-z0-9_:/-]+$` pattern and
    // `@length(20..256)` accept, and that still cannot name a resource of this
    // service's namespace: too few of an ARN's six fields, an empty partition, an
    // empty resource, and a well-formed in-Region ARN of ANOTHER vendor — which
    // names nothing here however complete it is.
    let em = Emulator::new();
    for not_a_resource_arn in [
        "arn:aws-this-is-not-an-arn".to_string(),
        format!("arn:aws:{SIGNING_NAME}"),
        format!("arn:aws:{SIGNING_NAME}:us-east-1:{ACCOUNT_ID}"),
        format!("arn::{SIGNING_NAME}:us-east-1:{ACCOUNT_ID}:brand-profile/bp-1"),
        format!("arn:aws:{SIGNING_NAME}:us-east-1:{ACCOUNT_ID}:"),
        format!("arn:aws:s3:us-east-1:{ACCOUNT_ID}:bucket/some-bucket"),
        format!("arn:aws:sqs:us-east-1:{ACCOUNT_ID}:some-queue"),
    ] {
        let refused = em.list_tags(&not_a_resource_arn).await;
        assert_eq!(
            refused.status, 400,
            "case {not_a_resource_arn}: a value that names no resource of this service is a \
             ValidationException (400), got {} / {:?}",
            refused.status, refused.body
        );
        assert_eq!(
            refused.error.as_deref(),
            Some("ValidationException"),
            "case {not_a_resource_arn}: unexpected error code, got {:?}",
            refused.error
        );
        assert_eq!(
            refused.member("fieldList")[0]["path"],
            json!("/resourceArn"),
            "case {not_a_resource_arn}: the refusal must name the member it refused, got {:?}",
            refused.body
        );
    }
}

#[tokio::test]
async fn an_arn_naming_no_stored_resource_is_not_found() {
    let em = Emulator::new();
    let created = em
        .create(json!({
            "brandProfileName": "to-be-deleted",
            "tags": [{ "key": "env", "value": "prod" }],
        }))
        .await;
    let arn = created.string("brandProfileArn");
    let id = created.string("brandProfileId");

    // The tags of a live resource are reachable; the rows below are the ARNs that
    // reach nothing. Each is (case, ARN, the resourceType the refusal reports — the
    // ARN's own resource-type segment, the resourceId it reports — the identifier
    // that segment names).
    assert_eq!(
        em.list_tags(&arn).await.status,
        200,
        "the live profile's tags must be reachable before it is deleted"
    );
    assert_eq!(
        em.delete(&id).await.status,
        200,
        "deleting the profile must succeed"
    );

    let unknown_profile =
        format!("arn:aws:{SIGNING_NAME}:us-east-1:{ACCOUNT_ID}:brand-profile/bp-00000000000000000");
    let unknown_configuration = format!(
        "arn:aws:{SIGNING_NAME}:us-east-1:{ACCOUNT_ID}:notify-code-configuration/ncc-0000000000"
    );
    // A resource type this service's ARNs name but this store does not hold, and the
    // id of a resource of ANOTHER type: the ARN as a whole is what names a resource.
    let wrong_type = format!("arn:aws:{SIGNING_NAME}:us-east-1:{ACCOUNT_ID}:phone-number/pn-1234");
    let type_of_another_resource =
        format!("arn:aws:{SIGNING_NAME}:us-east-1:{ACCOUNT_ID}:notify-code-configuration/{id}");
    let another_account = format!("arn:aws:{SIGNING_NAME}:us-east-1:000000000000:brand-profile/{id}");

    let cases: Vec<(&str, &str, &str, &str)> = vec![
        ("a deleted profile", arn.as_str(), "brand-profile", id.as_str()),
        (
            "an unknown profile id",
            &unknown_profile,
            "brand-profile",
            "bp-00000000000000000",
        ),
        (
            "an unknown configuration id",
            &unknown_configuration,
            "notify-code-configuration",
            "ncc-0000000000",
        ),
        (
            "a resource type nothing is stored for",
            &wrong_type,
            "phone-number",
            "pn-1234",
        ),
        (
            "an id under another resource type",
            &type_of_another_resource,
            "notify-code-configuration",
            id.as_str(),
        ),
        (
            "another account",
            &another_account,
            "brand-profile",
            id.as_str(),
        ),
    ];

    for (case, resource_arn, resource_type, resource_id) in cases {
        let refused = em.list_tags(resource_arn).await;
        assert_eq!(
            refused.status, 404,
            "case {case} ({resource_arn}): an ARN naming no stored resource is \
             ResourceNotFoundException (404), got {} / {:?}",
            refused.status, refused.body
        );
        assert_eq!(
            refused.error.as_deref(),
            Some("ResourceNotFoundException"),
            "case {case}: unexpected error code, got {:?}",
            refused.error
        );
        assert_eq!(
            refused.member("resourceId"),
            &json!(resource_id),
            "case {case}: the refusal must report the identifier the ARN names, got {:?}",
            refused.body
        );
        assert_eq!(
            refused.member("resourceType"),
            &json!(resource_type),
            "case {case}: the refusal must report the ARN's resource type, got {:?}",
            refused.body
        );
        assert_eq!(
            refused.string("message"),
            format!(
                "Resource not found - ResourceType=\"{resource_type}\" \
                 ResourceId=\"{resource_id}\""
            ),
            "case {case}: unexpected refusal message, got {:?}",
            refused.body
        );
    }
}

#[tokio::test]
async fn an_empty_store_lists_nothing_and_offers_no_token() {
    let em = Emulator::new();
    let listed = em.list("").await;
    assert_eq!(
        listed.status, 200,
        "an empty list is a 200, got {} / {:?}",
        listed.status, listed.body
    );
    assert_eq!(
        listed.member("brandProfiles"),
        &json!([]),
        "an empty store lists nothing, got {:?}",
        listed.body
    );
    assert!(
        listed.member("nextToken").is_null(),
        "an empty page offers no token, got {:?}",
        listed.body
    );
}
