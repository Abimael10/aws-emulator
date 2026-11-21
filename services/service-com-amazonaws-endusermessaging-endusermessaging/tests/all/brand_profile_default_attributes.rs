// SPDX-License-Identifier: CC0-1.0

//! The default attribute template a created brand profile is seeded with, and the
//! paginated attribute list that reports it, end to end over the wire.
//!
//! Every assertion here goes through `MultiServiceRouter::dispatch`, so it exercises
//! the same restJson1 path a real SDK client takes: routing, deserialization,
//! boundary constraint validation, the handler, and error rendering.
//!
//! The rules under test, each over a SET of inputs rather than one recorded request:
//!
//! * `CreateBrandProfile` seeds the service's default attribute template into the
//!   new profile, so `attributesCreated` is positive and equals the number of
//!   entries `ListBrandProfileAttributes` reports for that profile right after —
//!   for every name the modelled `BrandProfileName` accepts, with or without
//!   `deletionProtectionEnabled`;
//! * every seeded entry carries a distinct, non-blank `attributeName` within
//!   `BrandProfileAttributeName`'s modelled pattern, an `attributeType` from the
//!   modelled `BrandProfileAttributeType` values, and the `createdAt`/`updatedAt`
//!   of the create that seeded it;
//! * the list pages the seeded entries whole, exactly once each, for every page
//!   size, and emits a resumable token only while entries remain;
//! * the list resolves its `BrandProfileIdOrArn` by bare id or by full ARN, refuses
//!   an identifier that names no profile with the service's templated
//!   `ResourceNotFoundException`, and refuses a `nextToken` it never issued with
//!   `ValidationException`;
//! * each profile holds its own copy of the template, so one profile's list never
//!   reports another's;
//! * the list reports the attributes the profile HOLDS, so an attribute a write
//!   added is paged back as it was stored, one a delete removed is gone, and one
//!   re-created under the same name is reported again with the value the request
//!   supplied;
//! * `maxResults` outside its modelled range is refused, and paging the list — by
//!   any page size, to exhaustion — changes nothing a later caller sees.

use std::collections::BTreeSet;
use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_endusermessaging_endusermessaging::state::{
    BRAND_PROFILE_RESOURCE_TYPE, DEFAULT_BRAND_PROFILE_ATTRIBUTES,
};
use service_com_amazonaws_endusermessaging_endusermessaging::EmulatedService;

/// This service's sigv4 signing name, from the model (`OPERATIONS.md`, "Wire protocol").
const SIGNING_NAME: &str = "end-user-messaging";

/// The account id every generated emulator presents, as `engine` defines it.
const ACCOUNT_ID: &str = engine::EMULATOR_ACCOUNT_ID;

/// The region every request here is signed for.
const REGION: &str = "us-east-1";

/// The values the modelled `BrandProfileAttributeType` enum declares, as this
/// service exports them for its handlers (`BRAND_PROFILE_ATTRIBUTE_TYPE_VALUES`).
const ATTRIBUTE_TYPES: &[&str] = &["DOCUMENT", "IMAGE", "TEXT"];

/// A ceiling on how many pages one list walk may take, so a token that never
/// advances fails the test instead of looping forever.
const MAX_PAGES: usize = 64;

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

    /// The `brandProfileAttributes` items of a list page.
    fn attributes(&self) -> Vec<Value> {
        self.member("brandProfileAttributes")
            .as_array()
            .unwrap_or_else(|| {
                panic!("brandProfileAttributes is not a list in {}", self.body)
            })
            .clone()
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
                .map(|code| code.rsplit('#').next().unwrap_or(code.as_str()).to_string())
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

    async fn delete(&self, id_or_arn: &str) -> Reply {
        self.call("DELETE", &format!("/v1/brand-profiles/{id_or_arn}"), None)
            .await
    }

    async fn list_attributes(&self, id_or_arn: &str, query: &str) -> Reply {
        self.call(
            "GET",
            &format!(
                "/v1/brand-profiles/{}/attributes{query}",
                path_label(id_or_arn)
            ),
            None,
        )
        .await
    }

    /// Every attribute of `id_or_arn`, walked over pages of `page_size` (the whole
    /// list in one call when it is `None`), with each page checked for the page-size
    /// and token invariants as it goes.
    async fn all_attributes(&self, case: &str, id_or_arn: &str, page_size: Option<usize>) -> Vec<Value> {
        let mut collected: Vec<Value> = Vec::new();
        let mut token: Option<String> = None;
        for page_index in 0..MAX_PAGES {
            let mut query = match page_size {
                Some(size) => format!("?maxResults={size}"),
                None => String::new(),
            };
            if let Some(ref next) = token {
                let separator = if query.is_empty() { '?' } else { '&' };
                query.push(separator);
                query.push_str("nextToken=");
                query.push_str(next);
            }
            let page = self.list_attributes(id_or_arn, &query).await;
            assert_eq!(
                page.status, 200,
                "case {case}: page {page_index} must be a 200, got {} / {:?}",
                page.status, page.body
            );
            let items = page.attributes();
            if let Some(size) = page_size {
                assert!(
                    items.len() <= size,
                    "case {case}: page {page_index} returned {} items for maxResults={size}",
                    items.len()
                );
            }
            let next_token = page.member("nextToken").as_str().map(str::to_string);
            if next_token.is_some() {
                assert!(
                    !items.is_empty(),
                    "case {case}: page {page_index} emitted a token for an empty page, got {:?}",
                    page.body
                );
            }
            collected.extend(items);
            match next_token {
                Some(next) => token = Some(next),
                None => return collected,
            }
        }
        panic!("case {case}: the list never ran out of pages within {MAX_PAGES} pages");
    }

    /// `CreateBrandProfileAttributes`, over the same route the list reads back.
    async fn create_attributes(&self, id_or_arn: &str, entries: Vec<Value>) -> Reply {
        self.call(
            "POST",
            &format!(
                "/v1/brand-profiles/{}/attributes",
                path_label(id_or_arn)
            ),
            Some(json!({ "attributes": entries })),
        )
        .await
    }

    /// `DeleteBrandProfileAttribute` for one attribute of a profile.
    async fn delete_attribute(&self, id_or_arn: &str, name: &str) -> Reply {
        self.call(
            "DELETE",
            &format!(
                "/v1/brand-profiles/{}/attributes/{}",
                path_label(id_or_arn),
                attribute_segment(name)
            ),
            None,
        )
        .await
    }

    /// `GetBrandProfileAttribute` for one attribute of a profile.
    async fn get_attribute(&self, id_or_arn: &str, name: &str) -> Reply {
        self.call(
            "GET",
            &format!(
                "/v1/brand-profiles/{}/attributes/{}",
                path_label(id_or_arn),
                attribute_segment(name)
            ),
            None,
        )
        .await
    }
}

/// `name` as one path segment of a request URI: `BrandProfileAttributeName`'s
/// modelled pattern admits an interior space, which a URI does not carry literally.
fn attribute_segment(name: &str) -> String {
    name.replace(' ', "%20")
}

/// The names in this table are the classes `BrandProfileName`'s modelled
/// `@length(1..64)` and `^[A-Za-z0-9_ -]*[A-Za-z0-9_-][A-Za-z0-9_ -]*$` pattern
/// accept: a single character, the alphanumeric case, every punctuation the pattern
/// names, an interior space, and exactly the maximum length.
fn accepted_names() -> Vec<String> {
    vec![
        "a".to_string(),
        "seed-me".to_string(),
        "My_Brand profile-1".to_string(),
        "0".to_string(),
        "A".repeat(64),
    ]
}

/// `value` as one path segment of a request URI.
///
/// `brandProfileId` is a non-greedy `@httpLabel` in this route, so a client sends
/// any `/` the identifier contains percent-encoded rather than as a segment
/// separator; an identifier that is a full ARN carries one.
///
/// `pub(crate)` because the suites of this crate's one test target share it: every
/// non-greedy `@httpLabel` this service binds (`brandProfileId`, `resourceArn`) is
/// sent this way, so the encoding is written once.
pub(crate) fn path_label(value: &str) -> String {
    value.replace('/', "%2F")
}

/// The name of one listed attribute.
fn attribute_name(case: &str, item: &Value) -> String {
    item["attributeName"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case}: attributeName is not a string in {item:?}"))
        .to_string()
}

/// Whether `name` is within `BrandProfileAttributeName`'s modelled
/// `^[A-Za-z0-9_ -]*[A-Za-z0-9_-][A-Za-z0-9_ -]*$` pattern and `@length(1..256)`.
fn is_modelled_attribute_name(name: &str) -> bool {
    (1..=256).contains(&name.len())
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == ' ')
        && name
            .chars()
            .any(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

#[tokio::test]
async fn a_created_profile_is_seeded_with_the_default_attribute_template() {
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
            let seeded = created.number("attributesCreated");
            assert!(
                seeded > 0.0,
                "case {case}: a create seeds the default attribute template, so \
                 attributesCreated is positive, got {seeded:?}"
            );

            // Round trip: the reader reports exactly the attributes the create seeded.
            let listed = em.all_attributes(&case, &id, None).await;
            assert_eq!(
                listed.len() as f64, seeded,
                "case {case}: attributesCreated must equal the number of attributes \
                 listed, got {seeded:?} vs {}",
                listed.len()
            );

            let mut names = BTreeSet::new();
            for item in &listed {
                let attribute = attribute_name(&case, item);
                assert!(
                    !attribute.trim().is_empty(),
                    "case {case}: a seeded attributeName is never blank, got {item:?}"
                );
                assert!(
                    is_modelled_attribute_name(&attribute),
                    "case {case}: {attribute:?} is outside BrandProfileAttributeName's \
                     modelled pattern"
                );
                assert!(
                    names.insert(attribute.clone()),
                    "case {case}: {attribute:?} was listed twice"
                );
                let attribute_type = item["attributeType"].as_str().unwrap_or_else(|| {
                    panic!("case {case}: attributeType is not a string in {item:?}")
                });
                assert!(
                    ATTRIBUTE_TYPES.contains(&attribute_type),
                    "case {case}: {attribute:?} has attributeType {attribute_type:?}, \
                     not one of {ATTRIBUTE_TYPES:?}"
                );
                // A seeded attribute came into being with the profile and has never
                // been written since, so it carries the create's own instant.
                assert_eq!(
                    item["createdAt"],
                    created.member("createdAt").clone(),
                    "case {case}: {attribute:?} must be created with the profile, got {item:?}"
                );
                assert_eq!(
                    item["updatedAt"],
                    item["createdAt"],
                    "case {case}: a never-written {attribute:?} has updatedAt == createdAt, \
                     got {item:?}"
                );
            }
            assert_eq!(
                names.len(),
                DEFAULT_BRAND_PROFILE_ATTRIBUTES.len(),
                "case {case}: every slot of the default template is seeded exactly once, \
                 got {names:?}"
            );
        }
    }
}

#[tokio::test]
async fn the_seeded_attributes_page_whole_for_every_page_size() {
    let em = Emulator::new();
    let created = em.create_named("paged").await;
    let id = created.string("brandProfileId");

    let whole: Vec<String> = em
        .all_attributes("whole list", &id, None)
        .await
        .iter()
        .map(|item| attribute_name("whole list", item))
        .collect();
    assert!(
        !whole.is_empty(),
        "the seeded template is not empty, got {whole:?}"
    );

    // Page sizes spanning the class: one at a time, every size up to and past the
    // number of seeded attributes, and `maxResults`' modelled maximum.
    let sizes: Vec<usize> = (1..=whole.len() + 1).chain([100]).collect();
    for size in sizes {
        let case = format!("maxResults={size}");
        let paged: Vec<String> = em
            .all_attributes(&case, &id, Some(size))
            .await
            .iter()
            .map(|item| attribute_name(&case, item))
            .collect();
        assert_eq!(
            paged, whole,
            "case {case}: paging must report every attribute exactly once, in the same \
             order as the unpaged list, got {paged:?}"
        );
    }
}

#[tokio::test]
async fn the_attribute_list_resolves_a_profile_by_bare_id_and_by_arn() {
    let em = Emulator::new();
    let created = em.create_named("resolve-attributes").await;
    let id = created.string("brandProfileId");
    let arn = created.string("brandProfileArn");

    let by_id = em.all_attributes("bare id", &id, None).await;
    for reference in [id.clone(), arn.clone()] {
        let case = format!("reference={reference:?}");
        let listed = em.all_attributes(&case, &reference, None).await;
        assert_eq!(
            listed, by_id,
            "case {case}: a BrandProfileIdOrArn resolves the same profile by id or ARN"
        );
    }
}

#[tokio::test]
async fn each_profile_holds_its_own_copy_of_the_template() {
    let em = Emulator::new();
    let first = em.create_named("first-profile").await;
    let second = em.create_named("second-profile").await;
    let first_id = first.string("brandProfileId");
    let second_id = second.string("brandProfileId");

    let first_names: Vec<String> = em
        .all_attributes("first", &first_id, None)
        .await
        .iter()
        .map(|item| attribute_name("first", item))
        .collect();
    let second_names: Vec<String> = em
        .all_attributes("second", &second_id, None)
        .await
        .iter()
        .map(|item| attribute_name("second", item))
        .collect();
    assert_eq!(
        first_names, second_names,
        "every profile is seeded with the same template, got {first_names:?} vs \
         {second_names:?}"
    );

    // Deleting one profile takes its attributes with it and leaves the other's alone.
    assert_eq!(
        em.delete(&first_id).await.status,
        200,
        "the delete must succeed"
    );
    let gone = em.list_attributes(&first_id, "").await;
    assert_eq!(
        gone.status, 404,
        "a deleted profile has no attribute list, got {} / {:?}",
        gone.status, gone.body
    );
    let survivor: Vec<String> = em
        .all_attributes("survivor", &second_id, None)
        .await
        .iter()
        .map(|item| attribute_name("survivor", item))
        .collect();
    assert_eq!(
        survivor, second_names,
        "the surviving profile keeps its own attributes, got {survivor:?}"
    );
}

/// The identifiers a `BrandProfileIdOrArn` accepts, in the documented form of a bare
/// `brandProfileId` or of the ARN of one, that name nothing: a minted-shaped id of
/// digits, the bare-ID form the shape's own documentation gives as its example, one
/// whose body mixes both cases, and a well-formed ARN for a profile that was never
/// created.
fn unknown_identifiers() -> Vec<String> {
    vec![
        "bp-00000000000000000".to_string(),
        "bp-abc12345678901234".to_string(),
        "bp-AbCdEfGhIjKlMnOpQ".to_string(),
        format!("arn:aws:{SIGNING_NAME}:{REGION}:{ACCOUNT_ID}:brand-profile/bp-11111111111111111"),
    ]
}

#[tokio::test]
async fn an_unknown_profile_refuses_the_attribute_list_with_the_templated_not_found() {
    let em = Emulator::new();

    for identifier in unknown_identifiers() {
        let case = format!("identifier={identifier:?}");
        let refused = em.list_attributes(&identifier, "").await;
        assert_eq!(
            refused.status, 404,
            "case {case}: expected 404, got {} / {:?}",
            refused.status, refused.body
        );
        assert_eq!(
            refused.error.as_deref(),
            Some("ResourceNotFoundException"),
            "case {case}: unexpected error code, got {:?}",
            refused.error
        );
        let expected = format!(
            "Resource not found - ResourceType=\"{BRAND_PROFILE_RESOURCE_TYPE}\" \
             ResourceId=\"{identifier}\""
        );
        assert_eq!(
            refused.string("message"),
            expected,
            "case {case}: expected message {expected:?}, got {:?}",
            refused.body
        );
    }
}

#[tokio::test]
async fn a_token_this_service_never_issued_is_refused() {
    let em = Emulator::new();
    let created = em.create_named("token-rules").await;
    let id = created.string("brandProfileId");

    // Rows: a value that is not hex at all, hex that decodes to something this
    // operation never issued, and a token issued by the other paginated list.
    let other_list = em.call("GET", "/v1/brand-profiles?maxResults=1", None).await;
    let foreign = other_list
        .member("nextToken")
        .as_str()
        .map(str::to_string)
        .unwrap_or_else(|| hex::encode("ListBrandProfiles/1/bp-00000000000000000"));
    let tokens = vec![
        "not-a-token".to_string(),
        hex::encode("some other service token"),
        foreign,
    ];

    for token in tokens {
        let case = format!("nextToken={token:?}");
        let refused = em
            .list_attributes(&id, &format!("?nextToken={token}"))
            .await;
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
    }
}

#[tokio::test]
async fn the_attribute_list_refuses_a_max_results_outside_the_modelled_range() {
    let em = Emulator::new();
    let created = em.create_named("page-size-bounds").await;
    let id = created.string("brandProfileId");

    // `maxResults` targets `MaxResults`, modelled `@range(1..100)`
    // (`smithy_query(query="shape", shape="MaxResults")`): each published bound is
    // accepted, as is an interior value.
    for accepted in [1, 2, 99, 100] {
        let case = format!("maxResults={accepted}");
        let page = em
            .list_attributes(&id, &format!("?maxResults={accepted}"))
            .await;
        assert_eq!(
            page.status, 200,
            "case {case}: a published bound must be accepted, got {} / {:?}",
            page.status, page.body
        );
        assert!(
            page.attributes().len() <= accepted,
            "case {case}: a page holds at most maxResults attributes, got {}",
            page.attributes().len()
        );
    }

    // One past each published bound, and far past the upper one.
    for refused_size in ["0", "-1", "101", "1000"] {
        let case = format!("maxResults={refused_size}");
        let refused = em
            .list_attributes(&id, &format!("?maxResults={refused_size}"))
            .await;
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
        let expected = "1 validation error detected. Value at '/maxResults' failed to \
                        satisfy constraint: Member must be between 1 and 100, inclusive";
        assert_eq!(
            refused.string("message"),
            expected,
            "case {case}: expected message {expected:?}, got {:?}",
            refused.body
        );
    }
}

/// The attributes to add on top of the seeded template: one call's worth, since
/// `CreateBrandProfileAttributes`' own `attributes` list is `@length(1..10)`.
///
/// The rows span what `BrandProfileAttributeName`'s modelled `@length(1..256)` and
/// `^[A-Za-z0-9_ -]*[A-Za-z0-9_-][A-Za-z0-9_ -]*$` pattern admit — a single
/// character, a digit, each punctuation the pattern names, an interior space and
/// exactly the maximum length — every value of the modelled
/// `BrandProfileAttributeType` enum, and the optional members a summary projects,
/// present and absent. Source: `smithy_query(query="shape",
/// shape="BrandProfileAttributeInput,BrandProfileAttributeSummary")`.
fn added_attributes() -> Vec<Value> {
    vec![
        json!({ "attributeName": "z", "attributeType": "TEXT", "attributeValue": "one character" }),
        json!({ "attributeName": "0", "attributeType": "TEXT", "attributeValue": "" }),
        json!({
            "attributeName": "added_text",
            "attributeType": "TEXT",
            "attributeValue": "stored",
            "description": "the described one",
            "category": "BUSINESS_IDENTITY",
        }),
        json!({ "attributeName": "added-text-2", "attributeType": "TEXT" }),
        json!({
            "attributeName": "Added Text Three",
            "attributeType": "TEXT",
            "attributeValue": "spaced",
            "category": "BUSINESS_ADDRESS",
        }),
        json!({ "attributeName": "A".repeat(256), "attributeType": "TEXT", "attributeValue": "the longest name" }),
        json!({ "attributeName": "added-image", "attributeType": "IMAGE", "description": "no media yet" }),
        json!({ "attributeName": "added-document", "attributeType": "DOCUMENT" }),
    ]
}

/// The `attributeName` each entry of a request or a list page carries, in order.
fn requested_names(entries: &[Value]) -> Vec<String> {
    entries
        .iter()
        .map(|entry| {
            entry["attributeName"]
                .as_str()
                .unwrap_or_else(|| panic!("attributeName is not a string in {entry:?}"))
                .to_string()
        })
        .collect()
}

#[tokio::test]
async fn the_attribute_list_reports_the_attributes_the_writes_stored() {
    let em = Emulator::new();
    let created = em.create_named("grown-template").await;
    let id = created.string("brandProfileId");
    let seeded = requested_names(&em.all_attributes("seeded", &id, None).await);

    let added = added_attributes();
    let create = em.create_attributes(&id, added.clone()).await;
    assert_eq!(
        create.status, 200,
        "the batch must be created, got {} / {:?}",
        create.status, create.body
    );

    // Every name the profile now holds, seeded and added, reported exactly once.
    let listed = em.all_attributes("grown", &id, None).await;
    let listed_names = requested_names(&listed);
    let mut expected: BTreeSet<String> = seeded.iter().cloned().collect();
    for name in requested_names(&added) {
        assert!(
            expected.insert(name.clone()),
            "the added {name:?} collides with the seeded template {seeded:?}"
        );
    }
    assert_eq!(
        listed_names.iter().cloned().collect::<BTreeSet<String>>(),
        expected,
        "the list reports the attributes the profile holds, got {listed_names:?}"
    );
    assert_eq!(
        listed_names.len(),
        expected.len(),
        "each attribute is reported once, got {listed_names:?}"
    );

    // Each added attribute is paged back as the request stored it, including the
    // optional members a summary projects and the ones the request left out.
    for entry in &added {
        let name = entry["attributeName"].as_str().expect("a requested name");
        let case = format!("attributeName={name:?}");
        let summary = listed
            .iter()
            .find(|item| item["attributeName"] == entry["attributeName"])
            .unwrap_or_else(|| panic!("case {case}: not reported in {listed_names:?}"));
        for member in ["attributeType", "description", "category"] {
            assert_eq!(
                summary[member],
                entry[member],
                "case {case}: {member} must be what was stored, got {summary:?}"
            );
        }
        assert_eq!(
            summary["updatedAt"],
            summary["createdAt"],
            "case {case}: a never-updated attribute has updatedAt == createdAt, got {summary:?}"
        );
    }

    // The grown set is more than one page, so paging it spans several: one at a
    // time, an interior size, each side of the set's own size, and `maxResults`'
    // modelled maximum.
    let total = listed_names.len();
    let sizes: Vec<usize> = vec![1, 3, total - 1, total, total + 1, 100];
    for size in sizes {
        let case = format!("maxResults={size}");
        let paged = requested_names(&em.all_attributes(&case, &id, Some(size)).await);
        assert_eq!(
            paged, listed_names,
            "case {case}: paging must report every attribute exactly once, in the same \
             order as the unpaged list, got {paged:?}"
        );
    }
}

#[tokio::test]
async fn a_listed_attribute_can_be_deleted_and_recreated_with_a_value() {
    let em = Emulator::new();
    let created = em.create_named("delete-and-recreate").await;
    let id = created.string("brandProfileId");
    let before = requested_names(&em.all_attributes("before", &id, None).await);

    // Each row is one name the list reports: a seeded one, and one a write added.
    let added = vec![json!({
        "attributeName": "added-text",
        "attributeType": "TEXT",
        "attributeValue": "first value",
    })];
    assert_eq!(
        em.create_attributes(&id, added.clone()).await.status,
        200,
        "the added attribute must be created"
    );
    let seeded_name = before.first().expect("a seeded template").clone();
    let rows = vec![seeded_name, "added-text".to_string()];

    for name in rows {
        let case = format!("attributeName={name:?}");
        let held = requested_names(&em.all_attributes(&case, &id, None).await);
        assert!(
            held.contains(&name),
            "case {case}: the list must report it before the delete, got {held:?}"
        );

        assert_eq!(
            em.delete_attribute(&id, &name).await.status,
            200,
            "case {case}: the delete must succeed"
        );
        let after_delete = requested_names(&em.all_attributes(&case, &id, None).await);
        assert!(
            !after_delete.contains(&name),
            "case {case}: a deleted attribute is not listed, got {after_delete:?}"
        );
        let survivors: Vec<String> = held.iter().filter(|n| **n != name).cloned().collect();
        assert_eq!(
            after_delete, survivors,
            "case {case}: the delete leaves every other attribute listed, got {after_delete:?}"
        );
        assert_eq!(
            em.get_attribute(&id, &name).await.status,
            404,
            "case {case}: the reader of a deleted attribute answers not found"
        );

        // Re-created under the same name, with a value the request supplies.
        let recreated = em
            .create_attributes(
                &id,
                vec![json!({
                    "attributeName": name,
                    "attributeType": "TEXT",
                    "attributeValue": "recreated value",
                    "category": "OTHER",
                })],
            )
            .await;
        assert_eq!(
            recreated.status, 200,
            "case {case}: the name is free again, got {} / {:?}",
            recreated.status, recreated.body
        );
        let after_create = requested_names(&em.all_attributes(&case, &id, None).await);
        assert_eq!(
            after_create, held,
            "case {case}: the re-created attribute is listed again, got {after_create:?}"
        );
        let read = em.get_attribute(&id, &name).await;
        assert_eq!(
            read.status, 200,
            "case {case}: the re-created attribute reads back, got {} / {:?}",
            read.status, read.body
        );
        assert_eq!(
            read.string("attributeValue"),
            "recreated value",
            "case {case}: the stored value is the one supplied, got {:?}",
            read.body
        );
        assert_eq!(
            read.string("attributeType"),
            "TEXT",
            "case {case}: the stored type is the one supplied, got {:?}",
            read.body
        );
    }
}

#[tokio::test]
async fn paging_the_attribute_list_changes_nothing_a_later_caller_sees() {
    let em = Emulator::new();
    let created = em.create_named("read-only-list").await;
    let id = created.string("brandProfileId");
    let before = em.list_attributes(&id, "").await.attributes();

    // Every way a client walks the list: whole, and page by page to exhaustion.
    for page_size in [None, Some(1), Some(2), Some(100)] {
        let case = format!("maxResults={page_size:?}");
        em.all_attributes(&case, &id, page_size).await;
    }

    let after = em.list_attributes(&id, "").await.attributes();
    assert_eq!(
        after, before,
        "a read leaves the stored attributes alone, got {after:?} vs {before:?}"
    );
}
