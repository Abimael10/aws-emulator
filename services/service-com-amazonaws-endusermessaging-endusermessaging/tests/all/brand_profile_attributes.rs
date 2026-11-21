// SPDX-License-Identifier: CC0-1.0

//! Brand-profile ATTRIBUTES, end to end over the wire: creating them, reading one back,
//! and downloading the media of one.
//!
//! Every assertion here goes through `MultiServiceRouter::dispatch`, so it exercises the
//! same restJson1 path a real SDK client takes: routing, deserialization, boundary
//! constraint validation, the handler, and error rendering. The media download goes
//! through the same router at the very URL the service reported, with no credential of its
//! own, as a presigned URL is followed.
//!
//! The rules under test, each over a SET of inputs rather than one recorded request:
//!
//! * `CreateBrandProfileAttributes` persists every entry of the batch it is given — for
//!   every `attributeType`, with and without each optional member, for one entry and for
//!   the modelled maximum of ten — and `GetBrandProfileAttribute` and
//!   `ListBrandProfileAttributes` report exactly what was stored;
//! * `mediaDownloadUrl` is reported for the media an `IMAGE` or `DOCUMENT` attribute
//!   HOLDS, and for nothing else, and a `GET` of it serves the very bytes
//!   `attachmentBody` carried, under the media type those bytes announce;
//! * an `attributeName` the profile already holds, or one the request names twice, is
//!   `ConflictException` (409), and the batch is atomic: nothing is created;
//! * an entry carrying content its `attributeType` does not take is `ValidationException`
//!   (400);
//! * a `brandProfileId` that names nothing is `ResourceNotFoundException` (404), as is an
//!   `attributeName` the profile does not hold;
//! * `UpdateBrandProfileAttribute` applies the members its request carries to the stored
//!   attribute and leaves every other member alone, so the update's own answer and every
//!   later read report the same stored record; a refused update writes nothing;
//! * a `category` outside the set the service supports is `ValidationException` (400) on
//!   both the create and the update, and every supported value round-trips;
//! * a repeated `clientToken` reports the attributes the first call created instead of
//!   creating a second set;
//! * `DeleteBrandProfileAttribute` removes exactly the attribute named, with the media it
//!   held, and leaves the profile's other attributes alone; being `@idempotent`, a name the
//!   profile does not hold is reported as done rather than refused, while the readers of
//!   that name still answer not found;
//! * deleting the profile takes its attributes and their media with it.

use std::sync::Arc;

use base64::Engine as _;
use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_endusermessaging_endusermessaging::EmulatedService;

/// This service's sigv4 signing name, from the model (`OPERATIONS.md`, "Wire protocol").
const SIGNING_NAME: &str = "end-user-messaging";

/// The generic "these are bytes" media type, which is what media announcing no format of
/// its own is served as.
const GENERIC_MEDIA_TYPE: &str = "application/octet-stream";

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

    fn attributes(&self) -> &Vec<Value> {
        self.member("attributes")
            .as_array()
            .unwrap_or_else(|| panic!("attributes is not a list in {}", self.body))
    }
}

/// One fetch of a published URL: the status, the media type and the bytes served.
#[derive(Debug)]
struct Fetched {
    status: u16,
    content_type: Option<String>,
    bytes: Vec<u8>,
}

/// One emulator over one shared engine: every call sees the same store.
struct Emulator {
    router: MultiServiceRouter,
    region: String,
}

impl Emulator {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        router.set_request_authentication(false);
        router.register_service(EmulatedService::new(engine));
        Self {
            router,
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

    /// Follow a URL the service published, exactly as published and with no credential of
    /// its own: a presigned URL carries its own authentication in the query.
    async fn fetch(&self, url: &str) -> Fetched {
        let request = http::Request::builder()
            .method("GET")
            .uri(url)
            .body(Bytes::new())
            .expect("a published URL is a well-formed request target");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let content_type = response
            .headers()
            .get("content-type")
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        Fetched {
            status,
            content_type,
            bytes: response.into_body().to_vec(),
        }
    }

    /// A fresh profile to hang attributes on, by its minted id.
    async fn profile(&self, name: &str) -> String {
        let created = self
            .call("POST", "/v1/brand-profiles", Some(json!({ "brandProfileName": name })))
            .await;
        assert_eq!(
            created.status, 200,
            "case {name:?}: a profile must be created, got {} / {:?}",
            created.status, created.body
        );
        created.string("brandProfileId")
    }

    async fn create_attributes(&self, profile_id: &str, body: Value) -> Reply {
        self.call(
            "POST",
            &format!("/v1/brand-profiles/{profile_id}/attributes"),
            Some(body),
        )
        .await
    }

    async fn get_attribute(&self, profile_id: &str, name: &str) -> Reply {
        self.call(
            "GET",
            &format!(
                "/v1/brand-profiles/{profile_id}/attributes/{}",
                encode_segment(name)
            ),
            None,
        )
        .await
    }

    /// `UpdateBrandProfileAttribute` on one attribute of one profile: `PUT` of the
    /// members to apply, at the very URI the read addresses the attribute by.
    async fn update_attribute(&self, profile_id: &str, name: &str, body: Value) -> Reply {
        self.call(
            "PUT",
            &format!(
                "/v1/brand-profiles/{profile_id}/attributes/{}",
                encode_segment(name)
            ),
            Some(body),
        )
        .await
    }

    /// `DeleteBrandProfileAttribute` on one attribute of one profile, addressed by
    /// either spelling of a `BrandProfileIdOrArn`: the profile reference is sent with
    /// any `/` it carries percent-encoded, as a client sends a non-greedy `@httpLabel`.
    async fn delete_attribute(&self, profile_reference: &str, name: &str) -> Reply {
        self.call(
            "DELETE",
            &format!(
                "/v1/brand-profiles/{}/attributes/{}",
                crate::brand_profile_default_attributes::path_label(profile_reference),
                encode_segment(name)
            ),
            None,
        )
        .await
    }

    async fn list_attributes(&self, profile_id: &str) -> Reply {
        self.call(
            "GET",
            &format!("/v1/brand-profiles/{profile_id}/attributes"),
            None,
        )
        .await
    }

    /// Every `attributeName` the profile's list reports.
    async fn listed_names(&self, profile_id: &str) -> Vec<String> {
        let listed = self.list_attributes(profile_id).await;
        listed
            .member("brandProfileAttributes")
            .as_array()
            .unwrap_or_else(|| {
                panic!(
                    "brandProfileAttributes is not a list in {:?}",
                    listed.body
                )
            })
            .iter()
            .map(|summary| {
                summary["attributeName"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string()
            })
            .collect()
    }
}

/// A path segment with the one character `BrandProfileAttributeName` admits that a URI
/// does not carry literally.
fn encode_segment(segment: &str) -> String {
    segment.replace(' ', "%20")
}

/// One attribute to ask for, and what it must read back as.
#[derive(Debug, Clone)]
struct Attribute {
    name: &'static str,
    attribute_type: &'static str,
    value: Option<&'static str>,
    media: Option<&'static [u8]>,
    description: Option<&'static str>,
    category: Option<&'static str>,
}

impl Attribute {
    fn text(name: &'static str, value: Option<&'static str>) -> Self {
        Self {
            name,
            attribute_type: "TEXT",
            value,
            media: None,
            description: None,
            category: None,
        }
    }

    fn media(name: &'static str, attribute_type: &'static str, media: Option<&'static [u8]>) -> Self {
        Self {
            name,
            attribute_type,
            value: None,
            media,
            description: None,
            category: None,
        }
    }

    fn described(mut self, description: &'static str, category: &'static str) -> Self {
        self.description = Some(description);
        self.category = Some(category);
        self
    }

    /// The `BrandProfileAttributeInput` document for this attribute, with
    /// `attachmentBody` base64-encoded as restJson1 carries a blob member.
    fn document(&self) -> Value {
        let mut entry = json!({
            "attributeName": self.name,
            "attributeType": self.attribute_type,
        });
        if let Some(value) = self.value {
            entry["attributeValue"] = json!(value);
        }
        if let Some(media) = self.media {
            entry["attachmentBody"] =
                json!(base64::engine::general_purpose::STANDARD.encode(media));
        }
        if let Some(description) = self.description {
            entry["description"] = json!(description);
        }
        if let Some(category) = self.category {
            entry["category"] = json!(category);
        }
        entry
    }

    fn carries_media(&self) -> bool {
        matches!(self.attribute_type, "IMAGE" | "DOCUMENT")
    }

    /// Whether this attribute holds media once created, which is what decides whether a
    /// download URL is reported for it: a media type alone is not enough — the media has
    /// to have been supplied.
    fn holds_media(&self) -> bool {
        self.carries_media() && self.media.is_some()
    }
}

/// The request body for a batch of attributes.
fn batch(attributes: &[Attribute]) -> Value {
    json!({
        "attributes": attributes.iter().map(Attribute::document).collect::<Vec<_>>(),
    })
}

/// A PNG, a JPEG, a GIF and a PDF, each reduced to the magic-number signature its format
/// publishes (PNG, RFC 2083 §5.2; JPEG, ISO/IEC 10918-1; GIF89a; PDF, ISO 32000-1 §7.5.2),
/// which is what a media type is read from when the request declares none.
const PNG: &[u8] = b"\x89PNG\r\n\x1a\n\x00\x00\x00\rIHDR payload";
const JPEG: &[u8] = b"\xff\xd8\xff\xe0\x00\x10JFIF\x00 payload";
const GIF: &[u8] = b"GIF89a\x01\x00\x01\x00 payload";
const PDF: &[u8] = b"%PDF-1.7\n1 0 obj\n<<>>\nendobj\n";

/// Bytes announcing no format at all, which are served as unnamed binary.
const UNKNOWN_MEDIA: &[u8] = b"not any published file format";

/// The media type each payload above announces: the `mediaContentType` of an attribute
/// carrying it, and the `Content-Type` its download is served under.
fn expected_media_type(media: &[u8]) -> &'static str {
    match media {
        PNG => "image/png",
        JPEG => "image/jpeg",
        GIF => "image/gif",
        PDF => "application/pdf",
        _ => GENERIC_MEDIA_TYPE,
    }
}

/// The batches the create rule is asserted over: they differ in the type of each entry,
/// in which optional members are present, and in how many entries the batch holds — one,
/// a few, and the modelled maximum of ten.
fn batches() -> Vec<(&'static str, Vec<Attribute>)> {
    let ten_names: [&'static str; 10] = [
        "First", "Second", "Third", "Fourth", "Fifth", "Sixth", "Seventh", "Eighth", "Ninth",
        "Tenth",
    ];
    vec![
        (
            "one TEXT attribute with a value",
            vec![Attribute::text("Company Name", Some("Example Corp"))],
        ),
        (
            "TEXT attributes with and without every optional member",
            vec![
                Attribute::text("Contact Email", Some("ops@example.test"))
                    .described("Where to reach us", "BUSINESS_CONTACT"),
                Attribute::text("Address Line 2", Some("")),
                Attribute::text("Legal Name", None),
            ],
        ),
        (
            "an IMAGE and a DOCUMENT carrying media",
            vec![
                Attribute::media("Logo", "IMAGE", Some(PNG)).described("The logo", "BRAND_DISPLAY"),
                Attribute::media("Charter", "DOCUMENT", Some(PDF)),
            ],
        ),
        (
            "a media attribute whose media has not been supplied",
            vec![Attribute::media("Banner", "IMAGE", None)],
        ),
        (
            "a name with an interior space and one at the modelled maximum length",
            vec![
                Attribute::text("Doing Business As", Some("Example")),
                Attribute::text(LONGEST_NAME, Some("at the length ceiling")),
            ],
        ),
        (
            "the modelled maximum of ten entries, of every type",
            ten_names
                .iter()
                .enumerate()
                .map(|(index, name)| match index % 3 {
                    0 => Attribute::text(name, Some("inline")),
                    1 => Attribute::media(name, "IMAGE", Some(JPEG)),
                    _ => Attribute::media(name, "DOCUMENT", Some(UNKNOWN_MEDIA)),
                })
                .collect(),
        ),
    ]
}

/// A name of exactly `BrandProfileAttributeName`'s modelled `@length(1..256)` maximum.
const LONGEST_NAME: &str = "NameAtTheModelledCeiling0123456789012345678901234567890123456789\
                            0123456789012345678901234567890123456789012345678901234567890123\
                            4567890123456789012345678901234567890123456789012345678901234567\
                            8901234567890123456789012345678901234567890123456789012345678901";

#[tokio::test]
async fn longest_name_is_at_the_modelled_ceiling() {
    assert_eq!(
        LONGEST_NAME.len(),
        256,
        "the ceiling case must sit exactly on BrandProfileAttributeName's maximum, got {}",
        LONGEST_NAME.len()
    );
}

#[tokio::test]
async fn created_attributes_are_persisted_and_read_back_whole() {
    let em = Emulator::new();

    for (case, attributes) in batches() {
        let profile = em.profile("Batch owner").await;
        let created = em.create_attributes(&profile, batch(&attributes)).await;
        assert_eq!(
            created.status, 200,
            "case {case}: expected 200, got {} / {:?}",
            created.status, created.body
        );
        let reported = created.attributes();
        assert_eq!(
            reported.len(),
            attributes.len(),
            "case {case}: every requested attribute must be reported, got {:?}",
            created.body
        );

        for (index, requested) in attributes.iter().enumerate() {
            let answered = &reported[index];
            assert_eq!(
                answered["attributeName"],
                json!(requested.name),
                "case {case}: entry {index} must be reported in request order, got {answered:?}"
            );
            assert_eq!(
                answered["attributeType"],
                json!(requested.attribute_type),
                "case {case}: entry {index} must keep its type, got {answered:?}"
            );
            // The URL addresses media that EXISTS: an attribute of a media type that
            // nothing has supplied media for has nothing to download.
            assert_eq!(
                answered["mediaDownloadUrl"].is_string(),
                requested.holds_media(),
                "case {case}: entry {index} of type {} must{} report mediaDownloadUrl, got \
                 {answered:?}",
                requested.attribute_type,
                if requested.holds_media() { "" } else { " not" }
            );

            // The round trip: the stored attribute, read back through the reader that
            // reports every member of one.
            let read = em.get_attribute(&profile, requested.name).await;
            assert_eq!(
                read.status, 200,
                "case {case}: entry {index} must read back, got {} / {:?}",
                read.status, read.body
            );
            assert_eq!(
                read.member("attributeValue"),
                &requested.value.map_or(Value::Null, |value| json!(value)),
                "case {case}: entry {index} must keep its value, got {:?}",
                read.body
            );
            assert_eq!(
                read.member("description"),
                &requested
                    .description
                    .map_or(Value::Null, |value| json!(value)),
                "case {case}: entry {index} must keep its description, got {:?}",
                read.body
            );
            assert_eq!(
                read.member("category"),
                &requested.category.map_or(Value::Null, |value| json!(value)),
                "case {case}: entry {index} must keep its category, got {:?}",
                read.body
            );
            assert_eq!(
                read.member("mediaContentType"),
                &requested
                    .media
                    .map_or(Value::Null, |media| json!(expected_media_type(media))),
                "case {case}: entry {index} must report the media type its bytes announce, \
                 got {:?}",
                read.body
            );
            assert_eq!(
                read.member("mediaSizeBytes"),
                &requested
                    .media
                    .map_or(Value::Null, |media| json!(media.len())),
                "case {case}: entry {index} must report the size of the media held, got {:?}",
                read.body
            );
            assert_eq!(
                read.member("mediaDownloadUrl").is_string(),
                requested.holds_media(),
                "case {case}: entry {index} must report a download URL exactly when it holds \
                 media, got {:?}",
                read.body
            );
            assert!(
                read.member("createdAt").is_number() && read.member("updatedAt").is_number(),
                "case {case}: entry {index} must carry both timestamps, got {:?}",
                read.body
            );
        }

        // And the profile's own list reports every created attribute beside the template
        // it was seeded with.
        let listed = em.listed_names(&profile).await;
        for requested in &attributes {
            assert!(
                listed.iter().any(|name| name == requested.name),
                "case {case}: {:?} must appear in the profile's attribute list, got {listed:?}",
                requested.name
            );
        }
    }
}

#[tokio::test]
async fn media_is_served_from_the_url_the_service_reports() {
    let em = Emulator::new();

    // The rows differ in the format the bytes announce, in their size, and in the
    // attribute type carrying them: one byte is `AttachmentBody`'s modelled minimum.
    let payloads: Vec<(&str, &'static str, Vec<u8>, &'static str)> = vec![
        ("a PNG image", "IMAGE", PNG.to_vec(), "image/png"),
        ("a JPEG image", "IMAGE", JPEG.to_vec(), "image/jpeg"),
        ("a GIF image", "IMAGE", GIF.to_vec(), "image/gif"),
        ("a PDF document", "DOCUMENT", PDF.to_vec(), "application/pdf"),
        (
            "bytes of no published format",
            "DOCUMENT",
            UNKNOWN_MEDIA.to_vec(),
            GENERIC_MEDIA_TYPE,
        ),
        (
            "the smallest body the model admits",
            "IMAGE",
            vec![0x42],
            GENERIC_MEDIA_TYPE,
        ),
        (
            "a body of many kilobytes",
            "DOCUMENT",
            (0..64 * 1024).map(|index| (index % 251) as u8).collect(),
            GENERIC_MEDIA_TYPE,
        ),
    ];

    for (case, attribute_type, bytes, media_type) in payloads {
        let profile = em.profile("Media owner").await;
        let body = json!({
            "attributes": [{
                "attributeName": "Asset",
                "attributeType": attribute_type,
                "attachmentBody": base64::engine::general_purpose::STANDARD.encode(&bytes),
            }],
        });
        let created = em.create_attributes(&profile, body).await;
        assert_eq!(
            created.status, 200,
            "case {case}: expected 200, got {} / {:?}",
            created.status, created.body
        );
        let url = created.attributes()[0]["mediaDownloadUrl"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case}: no mediaDownloadUrl in {:?}", created.body))
            .to_string();
        assert!(
            url.contains("X-Amz-Signature=") && url.contains("X-Amz-Credential="),
            "case {case}: the reported URL must be presigned, got {url}"
        );

        // The very bytes that were uploaded, under the media type they announce.
        let fetched = em.fetch(&url).await;
        assert_eq!(
            fetched.status, 200,
            "case {case}: the reported URL must serve the media, got {} / {:?}",
            fetched.status,
            String::from_utf8_lossy(&fetched.bytes)
        );
        assert_eq!(
            fetched.bytes, bytes,
            "case {case}: the download must serve the uploaded bytes, got {} bytes",
            fetched.bytes.len()
        );
        assert_eq!(
            fetched.content_type.as_deref(),
            Some(media_type),
            "case {case}: unexpected served media type"
        );

        // The reader of one attribute reports the same address, and it serves the same
        // bytes.
        let read = em.get_attribute(&profile, "Asset").await;
        let from_read = read.string("mediaDownloadUrl");
        let refetched = em.fetch(&from_read).await;
        assert_eq!(
            (refetched.status, refetched.bytes.as_slice()),
            (200, bytes.as_slice()),
            "case {case}: the URL a read reports must serve the same media, got {} / {} bytes",
            refetched.status,
            refetched.bytes.len()
        );
    }
}

#[tokio::test]
async fn a_media_address_with_nothing_behind_it_serves_nothing() {
    let em = Emulator::new();
    let profile = em.profile("Media owner").await;
    let created = em
        .create_attributes(
            &profile,
            batch(&[Attribute::media("Logo", "IMAGE", Some(PNG))]),
        )
        .await;
    let url = created.attributes()[0]["mediaDownloadUrl"]
        .as_str()
        .unwrap_or_else(|| panic!("no mediaDownloadUrl in {:?}", created.body))
        .to_string();

    // An address naming an attribute, or a profile, this store holds no media for
    // serves nothing: no bytes are invented for it.
    for (case, address) in [
        ("another attribute name", url.replace("/Logo?", "/Nothing?")),
        (
            "another profile id",
            url.replace(&profile, "bp-00000000000000000"),
        ),
    ] {
        let missing = em.fetch(&address).await;
        assert_eq!(
            missing.status, 404,
            "case {case}: an address naming no stored media serves nothing, got {} / {:?}",
            missing.status,
            String::from_utf8_lossy(&missing.bytes)
        );
    }
}

/// The message AWS words a conflict of this class with: fixed text, the quoted reason
/// token for the state that caused it, and the resource's hyphenated type.
const ALREADY_EXISTS_MESSAGE: &str =
    "Conflict occurred - Reason=\"RESOURCE_ALREADY_EXISTS\" ResourceType=\"brand-profile-attribute\"";

#[tokio::test]
async fn a_name_that_is_already_taken_is_refused_as_a_conflict() {
    let em = Emulator::new();

    // The rows span where the duplicate comes from: a name an earlier create stored, a
    // name the seeded default template holds, and a name one request asks for twice —
    // with the duplicate first and last in the batch, so the atomicity below is tested
    // on a batch whose earlier entries are all acceptable.
    let profile = em.profile("Conflict owner").await;
    let seeded = em
        .listed_names(&profile)
        .await
        .first()
        .cloned()
        .expect("a created profile is seeded with a default attribute template");
    let stored = em
        .create_attributes(&profile, batch(&[Attribute::text("Taken", Some("first"))]))
        .await;
    assert_eq!(stored.status, 200, "the first create must succeed");

    let cases: Vec<(String, Value)> = vec![
        (
            "a name an earlier create stored".to_string(),
            batch(&[Attribute::text("Taken", Some("second"))]),
        ),
        (
            "a name of the seeded default template".to_string(),
            json!({ "attributes": [{ "attributeName": seeded, "attributeType": "TEXT" }] }),
        ),
        (
            "a name the request asks for twice".to_string(),
            batch(&[
                Attribute::text("Twice", Some("a")),
                Attribute::text("Twice", Some("b")),
            ]),
        ),
        (
            "a batch whose LAST entry duplicates a stored name".to_string(),
            batch(&[
                Attribute::text("Fresh One", Some("a")),
                Attribute::media("Fresh Two", "IMAGE", Some(PNG)),
                Attribute::text("Taken", Some("c")),
            ]),
        ),
    ];

    for (case, body) in cases {
        let before = em.listed_names(&profile).await;
        let refused = em.create_attributes(&profile, body).await;
        assert_eq!(
            (refused.status, refused.error.as_deref()),
            (409, Some("ConflictException")),
            "case {case}: expected 409 ConflictException, got {} / {:?}",
            refused.status,
            refused.body
        );
        assert_eq!(
            refused.string("message"),
            ALREADY_EXISTS_MESSAGE,
            "case {case}: unexpected conflict message"
        );
        assert_eq!(
            refused.member("resourceType"),
            &json!("brand-profile-attribute"),
            "case {case}: unexpected resourceType, got {:?}",
            refused.body
        );
        // Atomic: a refused batch creates nothing, not even the entries ahead of the one
        // at fault.
        let after = em.listed_names(&profile).await;
        assert_eq!(
            after, before,
            "case {case}: a refused batch must leave the profile's attributes unchanged"
        );
    }
}

#[tokio::test]
async fn content_the_attribute_type_does_not_take_is_refused() {
    let em = Emulator::new();
    let profile = em.profile("Validation owner").await;

    // `attributeValue` applies to TEXT, and media to IMAGE and DOCUMENT; each type admits
    // exactly one of the two members, in a batch of one and in a batch whose later entry
    // is the one at fault. Each row states the path the refusal must name: the member at
    // fault under the ONE-based position of its entry, which is how a list element's path
    // is spelled, so the index base and the member named are both pinned.
    let cases: Vec<(&str, Value, &str, &str)> = vec![
        (
            "media on a TEXT attribute",
            json!({ "attributes": [{
                "attributeName": "Inline",
                "attributeType": "TEXT",
                "attachmentBody": base64::engine::general_purpose::STANDARD.encode(PNG),
            }] }),
            "attributes.1.member.attachmentBody",
            "TEXT",
        ),
        (
            "an inline value on an IMAGE attribute",
            json!({ "attributes": [{
                "attributeName": "Picture",
                "attributeType": "IMAGE",
                "attributeValue": "not a picture",
            }] }),
            "attributes.1.member.attributeValue",
            "IMAGE",
        ),
        (
            "an inline value on a DOCUMENT attribute, after an acceptable entry",
            json!({ "attributes": [
                { "attributeName": "Fine", "attributeType": "TEXT", "attributeValue": "ok" },
                { "attributeName": "Paper", "attributeType": "DOCUMENT", "attributeValue": "x" },
            ] }),
            "attributes.2.member.attributeValue",
            "DOCUMENT",
        ),
    ];

    for (case, body, expected_path, attribute_type) in cases {
        let before = em.listed_names(&profile).await;
        let refused = em.create_attributes(&profile, body).await;
        assert_eq!(
            (refused.status, refused.error.as_deref()),
            (400, Some("ValidationException")),
            "case {case}: expected 400 ValidationException, got {} / {:?}",
            refused.status,
            refused.body
        );
        let requirement = format!(
            "Member must not be specified for an attribute of type {attribute_type}; supply {} \
             instead",
            if attribute_type == "TEXT" {
                "attributeValue"
            } else {
                "attachmentBody"
            }
        );
        assert_eq!(
            refused.body["fieldList"],
            json!([{ "path": expected_path, "message": requirement }]),
            "case {case}: the refusal must name the entry and member at fault, got {:?}",
            refused.body
        );
        assert_eq!(
            refused.string("message"),
            format!(
                "1 validation error detected: Value at '{expected_path}' failed to satisfy \
                 constraint: {requirement}"
            ),
            "case {case}: unexpected refusal message"
        );
        assert_eq!(
            em.listed_names(&profile).await,
            before,
            "case {case}: a refused batch must create nothing"
        );
    }
}

#[tokio::test]
async fn an_identifier_that_names_nothing_is_not_found() {
    let em = Emulator::new();
    let profile = em.profile("Lookup owner").await;

    // A profile the store does not hold, in each form a `BrandProfileIdOrArn` takes.
    for unknown in ["bp-00000000000000000", "bp-abc12345678901234"] {
        let created = em
            .create_attributes(unknown, batch(&[Attribute::text("Any", Some("v"))]))
            .await;
        assert_eq!(
            (created.status, created.error.as_deref()),
            (404, Some("ResourceNotFoundException")),
            "case {unknown}: expected 404, got {} / {:?}",
            created.status,
            created.body
        );
        assert_eq!(
            created.string("message"),
            format!(
                "Resource not found - ResourceType=\"brand-profile\" ResourceId=\"{unknown}\""
            ),
            "case {unknown}: unexpected not-found message"
        );
    }

    // A name the profile holds no attribute under. `BrandProfileAttributeName` is
    // `@sensitive`, so the refusal states the name in the `resourceId` member modelled
    // for it and not in its own text.
    for unknown in ["Nothing", "Not Stored"] {
        let read = em.get_attribute(&profile, unknown).await;
        assert_eq!(
            (read.status, read.error.as_deref()),
            (404, Some("ResourceNotFoundException")),
            "case {unknown}: expected 404, got {} / {:?}",
            read.status,
            read.body
        );
        assert_eq!(
            read.string("message"),
            attribute_not_found_message(),
            "case {unknown}: unexpected not-found message"
        );
        assert_eq!(
            (read.member("resourceType"), read.member("resourceId")),
            (&json!("brand-profile-attribute"), &json!(unknown)),
            "case {unknown}: the refusal must report the resource it could not find, got {:?}",
            read.body
        );
    }
}

#[tokio::test]
async fn a_repeated_client_token_creates_one_set_of_attributes() {
    let em = Emulator::new();

    // The rows differ in the shape of the batch the token was first used for.
    let cases: Vec<(&str, Vec<Attribute>)> = vec![
        ("one TEXT attribute", vec![Attribute::text("Once", Some("v"))]),
        (
            "a batch of several, including media",
            vec![
                Attribute::text("Alpha", Some("a")),
                Attribute::media("Beta", "DOCUMENT", Some(PDF)),
            ],
        ),
    ];

    for (case, attributes) in cases {
        let profile = em.profile("Token owner").await;
        let token = format!("token-{case}").replace(' ', "-");
        let mut body = batch(&attributes);
        body["clientToken"] = json!(token);

        let first = em.create_attributes(&profile, body.clone()).await;
        assert_eq!(
            first.status, 200,
            "case {case}: expected 200, got {} / {:?}",
            first.status, first.body
        );
        let after_first = em.listed_names(&profile).await;

        let repeat = em.create_attributes(&profile, body).await;
        assert_eq!(
            repeat.status, 200,
            "case {case}: a repeat must be served, got {} / {:?}",
            repeat.status, repeat.body
        );
        let names = |reply: &Reply| -> Vec<Value> {
            reply
                .attributes()
                .iter()
                .map(|entry| entry["attributeName"].clone())
                .collect()
        };
        assert_eq!(
            names(&repeat),
            names(&first),
            "case {case}: a repeat must report the attributes the first call created"
        );
        assert_eq!(
            em.listed_names(&profile).await,
            after_first,
            "case {case}: a repeat must not create a second set"
        );

        // A DIFFERENT token naming the same attributes is not a repeat of that request:
        // the names are taken, so it conflicts.
        let mut other = batch(&attributes);
        other["clientToken"] = json!("some-other-token");
        let conflicted = em.create_attributes(&profile, other).await;
        assert_eq!(
            (conflicted.status, conflicted.error.as_deref()),
            (409, Some("ConflictException")),
            "case {case}: another token must not re-create a stored name, got {} / {:?}",
            conflicted.status,
            conflicted.body
        );
    }
}

#[tokio::test]
async fn deleting_the_profile_takes_its_attributes_with_it() {
    let em = Emulator::new();
    let profile = em.profile("Doomed").await;
    let created = em
        .create_attributes(
            &profile,
            batch(&[
                Attribute::text("Company Name", Some("Example Corp")),
                Attribute::media("Logo", "IMAGE", Some(PNG)),
            ]),
        )
        .await;
    assert_eq!(created.status, 200, "the create must succeed");
    let url = created.attributes()[1]["mediaDownloadUrl"]
        .as_str()
        .expect("a media attribute reports a download URL")
        .to_string();
    assert_eq!(
        em.fetch(&url).await.status,
        200,
        "the media must be served while the profile exists"
    );

    let deleted = em
        .call("DELETE", &format!("/v1/brand-profiles/{profile}"), None)
        .await;
    assert_eq!(
        deleted.status, 200,
        "the profile must be deleted, got {} / {:?}",
        deleted.status, deleted.body
    );

    // The attributes went with the profile: each reader of one answers for the profile
    // that no longer exists, and the media it held is served no more.
    for (case, reply) in [
        ("the attribute list", em.list_attributes(&profile).await),
        ("a read of one attribute", em.get_attribute(&profile, "Logo").await),
        (
            "a create of another attribute",
            em.create_attributes(&profile, batch(&[Attribute::text("New", Some("v"))]))
                .await,
        ),
    ] {
        assert_eq!(
            (reply.status, reply.error.as_deref()),
            (404, Some("ResourceNotFoundException")),
            "case {case}: expected 404 after the profile was deleted, got {} / {:?}",
            reply.status,
            reply.body
        );
    }
    assert_eq!(
        em.fetch(&url).await.status,
        404,
        "the media of a deleted profile's attribute is served no more"
    );
}

/// The attributes kept beside the one each delete case removes: a delete names ONE
/// attribute, so every case also asserts what it must leave alone — an inline value and
/// media that is still served afterwards.
const NEIGHBOUR_TEXT: &str = "Untouched Text";
const NEIGHBOUR_MEDIA: &str = "Untouched Logo";

/// The message AWS words a not-found of this class with: fixed text and the resource's
/// hyphenated type. The `attributeName` asked for is NOT part of it —
/// `BrandProfileAttributeName` is `@sensitive`, so the refusal reports that value in the
/// `resourceId` member modelled for it rather than in its own text.
fn attribute_not_found_message() -> String {
    "Resource not found - ResourceType=\"brand-profile-attribute\"".to_string()
}

#[tokio::test]
async fn deleting_an_attribute_removes_it_and_the_media_it_held() {
    let em = Emulator::new();

    // The rows span what one attribute can be: each `attributeType`, with and without the
    // content that type takes, with and without the optional members, and each form of
    // name `BrandProfileAttributeName` admits — an interior space, and the length ceiling.
    let cases: Vec<(&str, Attribute)> = vec![
        (
            "a TEXT attribute with a value",
            Attribute::text("Company Name", Some("Example Corp")),
        ),
        (
            "a TEXT attribute with no value",
            Attribute::text("Legal Name", None),
        ),
        (
            "a TEXT attribute carrying every optional member",
            Attribute::text("Contact Email", Some("ops@example.test"))
                .described("Where to reach us", "BUSINESS_CONTACT"),
        ),
        (
            "an IMAGE holding media",
            Attribute::media("Logo", "IMAGE", Some(PNG)),
        ),
        (
            "a DOCUMENT holding media",
            Attribute::media("Charter", "DOCUMENT", Some(PDF)),
        ),
        (
            "an IMAGE whose media was never supplied",
            Attribute::media("Banner", "IMAGE", None),
        ),
        (
            "a name with an interior space",
            Attribute::text("Doing Business As", Some("Example")),
        ),
        (
            "a name at the modelled length ceiling",
            Attribute::text(LONGEST_NAME, Some("at the length ceiling")),
        ),
    ];

    for (case, target) in cases {
        let profile = em.profile("Delete owner").await;
        let created = em
            .create_attributes(
                &profile,
                batch(&[
                    target.clone(),
                    Attribute::text(NEIGHBOUR_TEXT, Some("stays")),
                    Attribute::media(NEIGHBOUR_MEDIA, "IMAGE", Some(GIF)),
                ]),
            )
            .await;
        assert_eq!(
            created.status, 200,
            "case {case}: the create must succeed, got {} / {:?}",
            created.status, created.body
        );
        let target_media_url = created.attributes()[0]["mediaDownloadUrl"]
            .as_str()
            .map(str::to_string);
        assert_eq!(
            target_media_url.is_some(),
            target.holds_media(),
            "case {case}: the created attribute must report a download URL exactly when it \
             holds media, got {:?}",
            created.body
        );
        let neighbour_media_url = created.attributes()[2]["mediaDownloadUrl"]
            .as_str()
            .unwrap_or_else(|| {
                panic!(
                    "case {case}: the neighbour holding media must report a download URL, got \
                     {:?}",
                    created.body
                )
            })
            .to_string();

        let deleted = em.delete_attribute(&profile, target.name).await;
        assert_eq!(
            (deleted.status, deleted.error.as_deref()),
            (200, None),
            "case {case}: expected 200, got {} / {:?}",
            deleted.status,
            deleted.body
        );
        assert_eq!(
            deleted.string("attributeName"),
            target.name,
            "case {case}: the delete must report the attribute it removed, got {:?}",
            deleted.body
        );
        assert_eq!(
            deleted.string("brandProfileId"),
            profile,
            "case {case}: the delete must report the profile it removed it from, got {:?}",
            deleted.body
        );

        // Gone from the reader of one attribute, and from the profile's own list.
        let read = em.get_attribute(&profile, target.name).await;
        assert_eq!(
            (read.status, read.error.as_deref()),
            (404, Some("ResourceNotFoundException")),
            "case {case}: a deleted attribute reads back as missing, got {} / {:?}",
            read.status,
            read.body
        );
        assert_eq!(
            read.string("message"),
            attribute_not_found_message(),
            "case {case}: unexpected not-found message"
        );
        let listed = em.listed_names(&profile).await;
        assert!(
            !listed.contains(&target.name.to_string()),
            "case {case}: {:?} must be gone from the profile's attribute list, got {listed:?}",
            target.name
        );

        // The media it held went with it: the address its own URL named serves nothing.
        if let Some(url) = target_media_url {
            let fetched = em.fetch(&url).await;
            assert_eq!(
                fetched.status, 404,
                "case {case}: the media of a deleted attribute is served no more, got {} / {}",
                fetched.status,
                String::from_utf8_lossy(&fetched.bytes)
            );
        }

        // And nothing else went with it: the neighbours still read back, are still listed,
        // and the media of the one holding media is still served.
        for neighbour in [NEIGHBOUR_TEXT, NEIGHBOUR_MEDIA] {
            let kept = em.get_attribute(&profile, neighbour).await;
            assert_eq!(
                kept.status, 200,
                "case {case}: {neighbour:?} must survive the delete, got {} / {:?}",
                kept.status, kept.body
            );
            assert!(
                listed.contains(&neighbour.to_string()),
                "case {case}: {neighbour:?} must still be listed, got {listed:?}"
            );
        }
        assert_eq!(
            em.fetch(&neighbour_media_url).await.status,
            200,
            "case {case}: the neighbour's media must still be served"
        );

        // The name is free again: it takes a fresh attribute, which reads back as its own
        // rather than as anything the deleted one held.
        let recreated = em
            .create_attributes(
                &profile,
                batch(&[Attribute::text(target.name, Some("recreated"))]),
            )
            .await;
        assert_eq!(
            recreated.status, 200,
            "case {case}: the freed name must take a fresh attribute, got {} / {:?}",
            recreated.status, recreated.body
        );
        let reread = em.get_attribute(&profile, target.name).await;
        assert_eq!(
            (reread.status, reread.member("attributeValue")),
            (200, &json!("recreated")),
            "case {case}: the fresh attribute must read back as stored, got {} / {:?}",
            reread.status,
            reread.body
        );
    }
}

#[tokio::test]
async fn deleting_a_name_the_profile_does_not_hold_is_reported_as_done() {
    let em = Emulator::new();
    let profile = em.profile("Idempotent owner").await;
    let elsewhere = em.profile("Another owner").await;
    assert_eq!(
        em.create_attributes(
            &elsewhere,
            batch(&[Attribute::text("Held Elsewhere", Some("v"))])
        )
        .await
        .status,
        200,
        "the other profile's attribute must be created"
    );
    // A name this profile held until a delete removed it: a second delete of it is the
    // repeat the rule is about.
    assert_eq!(
        em.create_attributes(
            &profile,
            batch(&[Attribute::media("Transient", "DOCUMENT", Some(PDF))])
        )
        .await
        .status,
        200,
        "the transient attribute must be created"
    );
    assert_eq!(
        em.delete_attribute(&profile, "Transient").await.status,
        200,
        "the first delete must be served"
    );

    // The rows span the ways a name reaches no attribute of THIS profile: one nothing ever
    // created, one carrying the interior space the model admits, one at the length ceiling,
    // one a delete already removed, and one only another profile holds.
    let before = em.listed_names(&profile).await;
    for name in [
        "Nothing",
        "Not Stored At All",
        LONGEST_NAME,
        "Transient",
        "Held Elsewhere",
    ] {
        let deleted = em.delete_attribute(&profile, name).await;
        assert_eq!(
            (deleted.status, deleted.error.as_deref()),
            (200, None),
            "case {name:?}: an idempotent delete reports the state asked for, got {} / {:?}",
            deleted.status,
            deleted.body
        );
        assert_eq!(
            deleted.string("attributeName"),
            name,
            "case {name:?}: the delete must report the name it was given, got {:?}",
            deleted.body
        );
        assert_eq!(
            deleted.string("brandProfileId"),
            profile,
            "case {name:?}: the delete must report the profile it addressed, got {:?}",
            deleted.body
        );

        // Nothing was created to stand in for it: the reader still answers not found, and
        // the profile's attributes are exactly what they were.
        let read = em.get_attribute(&profile, name).await;
        assert_eq!(
            (read.status, read.error.as_deref()),
            (404, Some("ResourceNotFoundException")),
            "case {name:?}: a delete must create nothing, got {} / {:?}",
            read.status,
            read.body
        );
        assert_eq!(
            read.string("message"),
            attribute_not_found_message(),
            "case {name:?}: unexpected not-found message"
        );
        assert_eq!(
            em.listed_names(&profile).await,
            before,
            "case {name:?}: a delete of a name the profile does not hold changes nothing"
        );
    }

    // The profile that does hold that name still holds it.
    let held = em.get_attribute(&elsewhere, "Held Elsewhere").await;
    assert_eq!(
        held.status, 200,
        "another profile's attribute of the same name must be untouched, got {} / {:?}",
        held.status, held.body
    );
}

#[tokio::test]
async fn a_delete_resolves_the_profile_by_bare_id_and_by_arn() {
    let em = Emulator::new();
    let created = em
        .call(
            "POST",
            "/v1/brand-profiles",
            Some(json!({ "brandProfileName": "Reference owner" })),
        )
        .await;
    assert_eq!(
        created.status, 200,
        "the profile must be created, got {} / {:?}",
        created.status, created.body
    );
    let id = created.string("brandProfileId");
    let arn = created.string("brandProfileArn");

    // Both documented spellings of a `BrandProfileIdOrArn` reach the same profile.
    for reference in [id.clone(), arn.clone()] {
        let case = format!("reference={reference:?}");
        let stored = em
            .create_attributes(&id, batch(&[Attribute::media("Asset", "IMAGE", Some(PNG))]))
            .await;
        assert_eq!(
            stored.status, 200,
            "case {case}: the create must succeed, got {} / {:?}",
            stored.status, stored.body
        );
        let url = stored.attributes()[0]["mediaDownloadUrl"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case}: no mediaDownloadUrl in {:?}", stored.body))
            .to_string();

        let deleted = em.delete_attribute(&reference, "Asset").await;
        assert_eq!(
            (deleted.status, deleted.error.as_deref()),
            (200, None),
            "case {case}: either spelling must address the profile, got {} / {:?}",
            deleted.status,
            deleted.body
        );
        // The output names the profile by its own unique identifier, whichever spelling
        // the request reached it by.
        assert_eq!(
            deleted.string("brandProfileId"),
            id,
            "case {case}: unexpected reported brandProfileId, got {:?}",
            deleted.body
        );
        assert_eq!(
            deleted.string("attributeName"),
            "Asset",
            "case {case}: unexpected reported attributeName, got {:?}",
            deleted.body
        );
        assert_eq!(
            em.get_attribute(&id, "Asset").await.status,
            404,
            "case {case}: the attribute must be gone whichever spelling removed it"
        );
        assert_eq!(
            em.fetch(&url).await.status,
            404,
            "case {case}: its media must be gone with it"
        );
    }
}

#[tokio::test]
async fn a_delete_against_a_profile_that_cannot_be_reached_is_refused() {
    let em = Emulator::new();

    // Identifiers of the documented forms — a bare id, and the ARN of one — that name no
    // stored profile: the resource is missing, so the refusal names the value given.
    let unknown = [
        "bp-00000000000000000".to_string(),
        "bp-AbCdEfGhIjKlMnOpQ".to_string(),
        format!(
            "arn:aws:{SIGNING_NAME}:us-east-1:{}:brand-profile/bp-11111111111111111",
            engine::EMULATOR_ACCOUNT_ID
        ),
    ];
    for identifier in unknown {
        let refused = em.delete_attribute(&identifier, "Anything").await;
        assert_eq!(
            (refused.status, refused.error.as_deref()),
            (404, Some("ResourceNotFoundException")),
            "case {identifier:?}: expected 404, got {} / {:?}",
            refused.status,
            refused.body
        );
        assert_eq!(
            refused.string("message"),
            format!("Resource not found - ResourceType=\"brand-profile\" ResourceId=\"{identifier}\""),
            "case {identifier:?}: unexpected not-found message"
        );
    }

    // A value that is no identifier of a brand profile at all can name no profile in any
    // state, so it is refused as input rather than as a missing resource.
    for identifier in ["not-a-brand-profile", "bp-tooshort", "ncc-0000000000000000"] {
        let refused = em.delete_attribute(identifier, "Anything").await;
        assert_eq!(
            (refused.status, refused.error.as_deref()),
            (400, Some("ValidationException")),
            "case {identifier:?}: expected 400, got {} / {:?}",
            refused.status,
            refused.body
        );
        let stated = format!("invalid brand profile ID format: {identifier}");
        assert_eq!(
            refused.string("message"),
            stated,
            "case {identifier:?}: unexpected refusal message"
        );
        assert_eq!(
            refused.body["fieldList"],
            json!([{ "path": "brandProfileId", "message": stated }]),
            "case {identifier:?}: the refusal must name the member at fault, got {:?}",
            refused.body
        );
    }
}

/// The members `GetBrandProfileAttribute` projects that `UpdateBrandProfileAttribute`
/// projects too: everything the two outputs share, which is what an update's answer and
/// the following read have to agree on member for member.
const SHARED_PROJECTION: [&str; 8] = [
    "attributeName",
    "attributeType",
    "attributeValue",
    "description",
    "category",
    "mediaContentType",
    "mediaSizeBytes",
    "createdAt",
];

/// The members a refused write must leave exactly as they were: everything the read
/// projects of the stored record, except the signed `mediaDownloadUrl`, which addresses
/// the same media however many times it is minted.
fn stored_projection(reply: &Reply) -> Value {
    let mut members = serde_json::Map::new();
    for member in SHARED_PROJECTION.iter().chain(&["updatedAt"]) {
        members.insert((*member).to_string(), reply.member(member).clone());
    }
    Value::Object(members)
}

/// `createdAt`/`updatedAt` as the modelled `Timestamp` the wire carries: a whole count of
/// epoch seconds, which is the form every projection of a stored instant takes.
fn epoch_seconds(reply: &Reply, member: &str, case: &str) -> i64 {
    let value = reply.member(member);
    assert_eq!(
        value.as_f64().map(f64::fract),
        Some(0.0),
        "case {case}: {member} must be a whole count of epoch seconds, got {value:?}"
    );
    value
        .as_i64()
        .unwrap_or_else(|| panic!("case {case}: {member} is not a number in {:?}", reply.body))
}

/// One update to apply to one created attribute, and what the attribute must then
/// project.
struct Update {
    case: &'static str,
    /// The attribute to create first.
    created: Attribute,
    /// The `UpdateBrandProfileAttribute` request body.
    change: Value,
    /// The members the update must have written, as they must read back.
    expected: Vec<(&'static str, Value)>,
}

#[tokio::test]
async fn an_update_is_what_every_later_read_reports() {
    let em = Emulator::new();

    // The rule holds over which member is written, how many are written at once, whether
    // the member was set at create time, and the type of the attribute written to — so
    // the rows differ along exactly those, with the recorded case (a description written
    // over a description) as one row among them.
    let updates = vec![
        Update {
            case: "a description written over the one the create stored",
            created: Attribute::text("Note", Some("first")).described("first", "OTHER"),
            change: json!({ "description": "second" }),
            expected: vec![
                ("description", json!("second")),
                ("attributeValue", json!("first")),
                ("category", json!("OTHER")),
            ],
        },
        Update {
            case: "a description written where the create stored none",
            created: Attribute::text("Legal Name", Some("Example Corp")),
            change: json!({ "description": "a custom note" }),
            expected: vec![
                ("description", json!("a custom note")),
                ("attributeValue", json!("Example Corp")),
                ("category", Value::Null),
            ],
        },
        Update {
            case: "an inline value written over the one the create stored",
            created: Attribute::text("Company Name", Some("Old Corp")),
            change: json!({ "attributeValue": "New Corp" }),
            expected: vec![("attributeValue", json!("New Corp"))],
        },
        Update {
            case: "an empty inline value, at the modelled length floor",
            created: Attribute::text("Address Line 2", Some("Suite 7")),
            change: json!({ "attributeValue": "" }),
            expected: vec![("attributeValue", json!(""))],
        },
        Update {
            case: "every updatable member of a TEXT attribute at once",
            created: Attribute::text("Contact Email", Some("ops@example.test")),
            change: json!({
                "attributeValue": "support@example.test",
                "description": "Where to reach us",
                "category": "BUSINESS_CONTACT",
            }),
            expected: vec![
                ("attributeValue", json!("support@example.test")),
                ("description", json!("Where to reach us")),
                ("category", json!("BUSINESS_CONTACT")),
            ],
        },
        Update {
            case: "a category written over the one the create stored",
            created: Attribute::text("Keywords", Some("STOP")).described("the keywords", "OTHER"),
            change: json!({ "category": "COMPLIANCE_KEYWORDS" }),
            expected: vec![
                ("category", json!("COMPLIANCE_KEYWORDS")),
                ("description", json!("the keywords")),
            ],
        },
        Update {
            case: "a description on an IMAGE attribute holding media",
            created: Attribute::media("Logo", "IMAGE", Some(PNG)),
            change: json!({ "description": "the logo" }),
            expected: vec![
                ("description", json!("the logo")),
                ("mediaContentType", json!("image/png")),
                ("mediaSizeBytes", json!(PNG.len())),
            ],
        },
        Update {
            case: "media written over the media an IMAGE attribute held",
            created: Attribute::media("Banner", "IMAGE", Some(PNG)),
            change: json!({
                "attachmentBody": base64::engine::general_purpose::STANDARD.encode(JPEG),
                "category": "BRAND_DISPLAY",
            }),
            expected: vec![
                ("mediaContentType", json!("image/jpeg")),
                ("mediaSizeBytes", json!(JPEG.len())),
                ("category", json!("BRAND_DISPLAY")),
            ],
        },
        Update {
            case: "media written where a DOCUMENT attribute held none",
            created: Attribute::media("Charter", "DOCUMENT", None),
            change: json!({
                "attachmentBody": base64::engine::general_purpose::STANDARD.encode(PDF),
                "description": "the charter",
            }),
            expected: vec![
                ("mediaContentType", json!("application/pdf")),
                ("mediaSizeBytes", json!(PDF.len())),
                ("description", json!("the charter")),
            ],
        },
    ];

    for (index, update) in updates.into_iter().enumerate() {
        let case = update.case;
        let profile = em.profile(&format!("Update case {index}")).await;
        let created = em
            .create_attributes(&profile, batch(std::slice::from_ref(&update.created)))
            .await;
        assert_eq!(
            created.status, 200,
            "case {case}: the create must succeed, got {} / {:?}",
            created.status, created.body
        );
        let name = update.created.name;
        let before = em.get_attribute(&profile, name).await;
        assert_eq!(
            before.status, 200,
            "case {case}: the created attribute must be readable, got {} / {:?}",
            before.status, before.body
        );

        let updated = em.update_attribute(&profile, name, update.change).await;
        assert_eq!(
            (updated.status, updated.error.as_deref()),
            (200, None),
            "case {case}: the update must succeed, got {} / {:?}",
            updated.status,
            updated.body
        );

        // The write is what the READ reports: every member the request carried came back
        // changed, and nothing the request left out changed with it.
        let after = em.get_attribute(&profile, name).await;
        assert_eq!(
            after.status, 200,
            "case {case}: the updated attribute must be readable, got {} / {:?}",
            after.status, after.body
        );
        for (member, expected) in &update.expected {
            assert_eq!(
                after.member(member),
                expected,
                "case {case}: {member} must read back as the update wrote it, got {:?}",
                after.body
            );
        }
        assert_eq!(
            after.member("attributeName"),
            &json!(name),
            "case {case}: an update must not rename the attribute, got {:?}",
            after.body
        );
        assert_eq!(
            after.member("attributeType"),
            &json!(update.created.attribute_type),
            "case {case}: an update must not retype the attribute, got {:?}",
            after.body
        );

        // One stored attribute has ONE rendering: the update's own answer and the read
        // that follows it agree on every member the two outputs share.
        for member in SHARED_PROJECTION {
            assert_eq!(
                updated.member(member),
                after.member(member),
                "case {case}: {member} differs between the update's answer and the read, \
                 {:?} vs {:?}",
                updated.body,
                after.body
            );
        }

        // `createdAt` is the instant the attribute was created at, which an update does
        // not move; `updatedAt` is the instant of the write, so it is never behind it.
        // Both are whole epoch seconds, in both projections.
        let created_at = epoch_seconds(&after, "createdAt", case);
        let updated_at = epoch_seconds(&after, "updatedAt", case);
        let _ = epoch_seconds(&updated, "updatedAt", case);
        assert_eq!(
            created_at,
            epoch_seconds(&before, "createdAt", case),
            "case {case}: an update must not move createdAt"
        );
        assert!(
            updated_at >= created_at,
            "case {case}: updatedAt {updated_at} must not be behind createdAt {created_at}"
        );
    }
}

#[tokio::test]
async fn the_last_update_of_a_sequence_is_the_one_a_read_reports() {
    let em = Emulator::new();
    let profile = em.profile("Sequence owner").await;
    let created = em
        .create_attributes(&profile, batch(&[Attribute::text("Note", Some("first"))]))
        .await;
    assert_eq!(
        created.status, 200,
        "the create must succeed, got {} / {:?}",
        created.status, created.body
    );

    // Each write is visible to the read that follows it, so a read after N writes reports
    // the Nth — never an earlier one.
    for description in ["first", "second", "third"] {
        let updated = em
            .update_attribute(&profile, "Note", json!({ "description": description }))
            .await;
        assert_eq!(
            (updated.status, updated.error.as_deref()),
            (200, None),
            "case {description:?}: the update must succeed, got {} / {:?}",
            updated.status,
            updated.body
        );
        let read = em.get_attribute(&profile, "Note").await;
        assert_eq!(
            read.member("description"),
            &json!(description),
            "case {description:?}: the read must report the most recent write, got {:?}",
            read.body
        );
    }
}

#[tokio::test]
async fn an_update_of_an_attribute_that_is_not_there_is_not_found() {
    let em = Emulator::new();
    let profile = em.profile("Update lookup owner").await;
    let created = em
        .create_attributes(&profile, batch(&[Attribute::text("Held", Some("v"))]))
        .await;
    assert_eq!(
        created.status, 200,
        "the create must succeed, got {} / {:?}",
        created.status, created.body
    );

    // Names the profile holds no attribute under: never created, differently cased than
    // one it holds (`attributeName` is unique and compared as written), and one carrying
    // an interior space.
    for name in ["fresh", "held", "HELD", "Not Held"] {
        let refused = em
            .update_attribute(&profile, name, json!({ "description": "anything" }))
            .await;
        assert_eq!(
            (refused.status, refused.error.as_deref()),
            (404, Some("ResourceNotFoundException")),
            "case {name:?}: expected 404, got {} / {:?}",
            refused.status,
            refused.body
        );
        assert_eq!(
            refused.string("message"),
            "Resource not found - ResourceType=\"brand-profile-attribute\"",
            "case {name:?}: unexpected not-found message"
        );
        assert_eq!(
            refused.member("resourceId"),
            &json!(name),
            "case {name:?}: the refusal must name the attribute asked for, got {:?}",
            refused.body
        );
        assert_eq!(
            refused.member("resourceType"),
            &json!("brand-profile-attribute"),
            "case {name:?}: unexpected resourceType, got {:?}",
            refused.body
        );
        // An update creates nothing: the name is still not there afterwards.
        assert_eq!(
            em.get_attribute(&profile, name).await.status,
            404,
            "case {name:?}: a refused update must not create the attribute"
        );
    }

    // The attribute belongs to one profile, so a profile that cannot be reached is
    // refused against the profile rather than against the attribute.
    for identifier in ["bp-00000000000000000", "bp-AbCdEfGhIjKlMnOpQ"] {
        let refused = em
            .update_attribute(identifier, "Held", json!({ "description": "anything" }))
            .await;
        assert_eq!(
            (refused.status, refused.error.as_deref()),
            (404, Some("ResourceNotFoundException")),
            "case {identifier:?}: expected 404, got {} / {:?}",
            refused.status,
            refused.body
        );
        assert_eq!(
            refused.string("message"),
            format!(
                "Resource not found - ResourceType=\"brand-profile\" ResourceId=\"{identifier}\""
            ),
            "case {identifier:?}: unexpected not-found message"
        );
    }
}

#[tokio::test]
async fn an_update_that_asks_for_no_change_is_refused_and_changes_nothing() {
    let em = Emulator::new();
    let profile = em.profile("No-change owner").await;
    let created = em
        .create_attributes(
            &profile,
            batch(&[Attribute::text("Note", Some("v")).described("a note", "OTHER")]),
        )
        .await;
    assert_eq!(
        created.status, 200,
        "the create must succeed, got {} / {:?}",
        created.status, created.body
    );
    let before = em.get_attribute(&profile, "Note").await;

    let refused = em.update_attribute(&profile, "Note", json!({})).await;
    assert_eq!(
        (refused.status, refused.error.as_deref()),
        (400, Some("ValidationException")),
        "expected 400 ValidationException, got {} / {:?}",
        refused.status,
        refused.body
    );
    assert_eq!(
        refused.string("message"),
        "At least one field must be provided for update",
        "unexpected refusal message"
    );
    // Refused before anything is written: `updatedAt` included, the record is as it was.
    let after = em.get_attribute(&profile, "Note").await;
    assert_eq!(
        stored_projection(&after),
        stored_projection(&before),
        "a refused update must leave the stored attribute exactly as it was"
    );
}

/// A `category` of exactly `BrandProfileAttributeCategory`'s modelled `@length(1..64)`
/// maximum, which the supported set does not hold: the published bound is admitted by the
/// boundary and refused by the runtime gate.
const CATEGORY_AT_THE_LENGTH_CEILING: &str =
    "CATEGORY_AT_THE_MODELLED_LENGTH_CEILING_012345678901234567890123";

#[tokio::test]
async fn every_supported_category_round_trips_and_nothing_else_is_stored() {
    let em = Emulator::new();
    let profile = em.profile("Category owner").await;

    // The whole catalog the service supports, from the one place this emulator states it
    // (`state::SUPPORTED_ATTRIBUTE_CATEGORIES`, the set `BrandProfileAttributeCategory`
    // documents as "validated against supported values at runtime"), so every entry is
    // covered and the test cannot drift from the gate.
    let supported = service_com_amazonaws_endusermessaging_endusermessaging::state::SUPPORTED_ATTRIBUTE_CATEGORIES;
    for (index, category) in supported.iter().enumerate() {
        let name = format!("Created {index}");
        let created = em
            .create_attributes(
                &profile,
                json!({ "attributes": [{
                    "attributeName": name,
                    "attributeType": "TEXT",
                    "attributeValue": "v",
                    "category": category,
                }] }),
            )
            .await;
        assert_eq!(
            (created.status, created.error.as_deref()),
            (200, None),
            "case {category:?}: a supported category must be accepted, got {} / {:?}",
            created.status,
            created.body
        );
        let read = em.get_attribute(&profile, &name).await;
        assert_eq!(
            read.member("category"),
            &json!(category),
            "case {category:?}: the stored category must read back, got {:?}",
            read.body
        );
        // And the same value is accepted by an update of an attribute created without
        // one.
        let target = format!("Updated {index}");
        let created_blank = em
            .create_attributes(
                &profile,
                json!({ "attributes": [{
                    "attributeName": target,
                    "attributeType": "TEXT",
                    "attributeValue": "v",
                }] }),
            )
            .await;
        assert_eq!(
            created_blank.status, 200,
            "case {category:?}: the create must succeed, got {} / {:?}",
            created_blank.status, created_blank.body
        );
        let updated = em
            .update_attribute(&profile, &target, json!({ "category": category }))
            .await;
        assert_eq!(
            (updated.status, updated.error.as_deref()),
            (200, None),
            "case {category:?}: a supported category must be accepted by an update, got {} / {:?}",
            updated.status,
            updated.body
        );
        assert_eq!(
            em.get_attribute(&profile, &target).await.member("category"),
            &json!(category),
            "case {category:?}: the updated category must read back"
        );
    }
}

#[tokio::test]
async fn a_category_outside_the_supported_set_is_refused() {
    let em = Emulator::new();
    let profile = em.profile("Category gate owner").await;
    let created = em
        .create_attributes(
            &profile,
            batch(&[Attribute::text("Note", Some("v")).described("a note", "OTHER")]),
        )
        .await;
    assert_eq!(
        created.status, 200,
        "the create must succeed, got {} / {:?}",
        created.status, created.body
    );
    let supported = service_com_amazonaws_endusermessaging_endusermessaging::state::SUPPORTED_ATTRIBUTE_CATEGORIES;
    let listed = supported.join(", ");
    assert_eq!(
        CATEGORY_AT_THE_LENGTH_CEILING.len(),
        64,
        "the ceiling case must sit exactly on the modelled @length(1..64) maximum"
    );

    // Values the supported set does not hold: a plausible name that is not one of them,
    // a supported name in another case (the values are spelled one way), one with
    // surrounding space, and one at the modelled length ceiling.
    let unsupported = [
        "MARKETING",
        "business_contact",
        "Other",
        " OTHER",
        CATEGORY_AT_THE_LENGTH_CEILING,
    ];
    for value in unsupported {
        let expected = format!("Invalid category '{value}'. Supported values: {listed}");

        // On the create, reported against the entry of `attributes` at fault.
        let refused = em
            .create_attributes(
                &profile,
                json!({ "attributes": [
                    { "attributeName": "Fine", "attributeType": "TEXT", "attributeValue": "ok" },
                    {
                        "attributeName": "Bad Category",
                        "attributeType": "TEXT",
                        "attributeValue": "v",
                        "category": value,
                    },
                ] }),
            )
            .await;
        assert_eq!(
            (refused.status, refused.error.as_deref()),
            (400, Some("ValidationException")),
            "case {value:?}: expected 400 ValidationException from the create, got {} / {:?}",
            refused.status,
            refused.body
        );
        assert_eq!(
            refused.string("message"),
            expected,
            "case {value:?}: unexpected create refusal message"
        );
        assert_eq!(
            refused.body["fieldList"],
            json!([{ "path": "/attributes/1/category", "message": expected }]),
            "case {value:?}: the refusal must name the entry and member at fault, got {:?}",
            refused.body
        );
        // Atomic, and nothing of the batch is stored.
        for name in ["Fine", "Bad Category"] {
            assert_eq!(
                em.get_attribute(&profile, name).await.status,
                404,
                "case {value:?}: a refused batch must create nothing, {name} exists"
            );
        }

        // On the update, reported against the request's own `category` member, and the
        // stored category is left as it was.
        let refused = em
            .update_attribute(&profile, "Note", json!({ "category": value }))
            .await;
        assert_eq!(
            (refused.status, refused.error.as_deref()),
            (400, Some("ValidationException")),
            "case {value:?}: expected 400 ValidationException from the update, got {} / {:?}",
            refused.status,
            refused.body
        );
        assert_eq!(
            refused.string("message"),
            expected,
            "case {value:?}: unexpected update refusal message"
        );
        assert_eq!(
            refused.body["fieldList"],
            json!([{ "path": "/category", "message": expected }]),
            "case {value:?}: the refusal must name the member at fault, got {:?}",
            refused.body
        );
        assert_eq!(
            em.get_attribute(&profile, "Note").await.member("category"),
            &json!("OTHER"),
            "case {value:?}: a refused update must leave the stored category unchanged"
        );
    }
}

#[tokio::test]
async fn an_update_carrying_content_the_attribute_type_does_not_take_is_refused() {
    let em = Emulator::new();
    let profile = em.profile("Update validation owner").await;

    // `attributeValue` applies to TEXT and media to IMAGE and DOCUMENT, so each stored
    // type takes exactly one of the two members — asserted for every type, and for a
    // media attribute both with and without media already held.
    let cases: Vec<(&str, Attribute, Value, &str, &str)> = vec![
        (
            "media on a TEXT attribute",
            Attribute::text("Inline", Some("v")),
            json!({ "attachmentBody": base64::engine::general_purpose::STANDARD.encode(PNG) }),
            "attachmentBody",
            "TEXT",
        ),
        (
            "an inline value on an IMAGE attribute holding media",
            Attribute::media("Picture", "IMAGE", Some(PNG)),
            json!({ "attributeValue": "not a picture" }),
            "attributeValue",
            "IMAGE",
        ),
        (
            "an inline value on a DOCUMENT attribute holding no media",
            Attribute::media("Paper", "DOCUMENT", None),
            json!({ "attributeValue": "x" }),
            "attributeValue",
            "DOCUMENT",
        ),
    ];

    for (case, attribute, change, member, attribute_type) in cases {
        let created = em
            .create_attributes(&profile, batch(std::slice::from_ref(&attribute)))
            .await;
        assert_eq!(
            created.status, 200,
            "case {case}: the create must succeed, got {} / {:?}",
            created.status, created.body
        );
        let before = em.get_attribute(&profile, attribute.name).await;

        let refused = em
            .update_attribute(&profile, attribute.name, change)
            .await;
        assert_eq!(
            (refused.status, refused.error.as_deref()),
            (400, Some("ValidationException")),
            "case {case}: expected 400 ValidationException, got {} / {:?}",
            refused.status,
            refused.body
        );
        let expected = format!("{member} cannot be provided for {attribute_type} attribute");
        assert_eq!(
            refused.string("message"),
            expected,
            "case {case}: unexpected refusal message"
        );
        assert_eq!(
            refused.body["fieldList"],
            json!([{ "path": format!("/{member}"), "message": expected }]),
            "case {case}: the refusal must name the member at fault, got {:?}",
            refused.body
        );
        assert_eq!(
            stored_projection(&em.get_attribute(&profile, attribute.name).await),
            stored_projection(&before),
            "case {case}: a refused update must leave the stored attribute as it was"
        );
    }
}
