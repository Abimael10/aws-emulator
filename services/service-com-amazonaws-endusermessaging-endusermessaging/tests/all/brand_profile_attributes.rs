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
//! * a repeated `clientToken` reports the attributes the first call created instead of
//!   creating a second set;
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
                Attribute::media("Logo", "IMAGE", Some(PNG)).described("The logo", "BRAND_ASSET"),
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

    // A name the profile holds no attribute under.
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
            format!(
                "Resource not found - ResourceType=\"brand-profile-attribute\" \
                 ResourceId=\"{unknown}\""
            ),
            "case {unknown}: unexpected not-found message"
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
