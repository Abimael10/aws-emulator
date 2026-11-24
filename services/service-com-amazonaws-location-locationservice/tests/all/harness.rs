// SPDX-License-Identifier: CC0-1.0

//! The one HTTP harness this crate's suites drive the emulator through.
//!
//! Every suite sends its requests with [`call`] so each call is a separate HTTP
//! request against one shared engine, dispatched by the production router: a value
//! that survives from one call to the next survived because it was PERSISTED, not
//! because a handler echoed its own input back.
//!
//! Declared from `tests/all.rs`, so a suite reaches it as `crate::harness`.

use std::sync::Arc;

use aws_smithy_types::date_time::Format;
use aws_smithy_types::DateTime;
use percent_encoding::{percent_encode, AsciiSet, NON_ALPHANUMERIC};
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_location_locationservice::EmulatedService;

/// The account id every generated emulator presents (`engine::EMULATOR_ACCOUNT_ID`).
pub const ACCOUNT: &str = engine::EMULATOR_ACCOUNT_ID;

/// The region the tests' SigV4 credential scope names, and therefore the region the
/// generated ARNs must carry.
pub const REGION: &str = "us-west-2";

/// One emulator: a shared engine with this service mounted on the production
/// router, so the tests drive the same dispatch path the binary does.
pub fn emulator() -> MultiServiceRouter {
    emulator_with_engine().0
}

/// The same emulator, plus a handle on the engine, for the tests that assert a
/// bound on what the store holds rather than on a response.
pub fn emulator_with_engine() -> (MultiServiceRouter, Arc<engine::InMemoryEngine>) {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    // The tests assert behaviour, not signatures; the emulator verifies signatures
    // itself and a service never checks them.
    router.set_request_authentication(false);
    router.register_service(EmulatedService::new(Arc::clone(&engine)));
    (router, engine)
}

/// The RFC 3986 unreserved set: every other byte is percent-encoded.
const UNRESERVED: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'_')
    .remove(b'.')
    .remove(b'~');

/// Percent-encode a value the way an SDK encodes one it puts in a single-segment
/// `@httpLabel` or an `@httpQuery` value, so an ARN's `:` and `/` cannot be mistaken
/// for path or query structure.
///
/// Here rather than in one suite because every suite that addresses a resource by ARN
/// needs the same encoding.
pub fn encode(value: &str) -> String {
    percent_encode(value.as_bytes(), UNRESERVED).to_string()
}

/// What the emulator answered.
pub struct Answer {
    pub status: u16,
    pub error_type: Option<String>,
    pub body: String,
}

impl Answer {
    pub fn json(&self) -> Value {
        if self.body.is_empty() {
            return Value::Null;
        }
        serde_json::from_str(&self.body)
            .unwrap_or_else(|e| panic!("response body is not JSON ({e}): {:?}", self.body))
    }

    /// The wire error code a client branches on: the `x-amzn-errortype` header the
    /// restJson1 envelope carries, falling back to the body's `__type`.
    pub fn error_code(&self) -> String {
        let raw = self.error_type.clone().unwrap_or_else(|| {
            self.json()
                .get("__type")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string()
        });
        // `x-amzn-errortype` may be `shape`, `ns#shape` or `shape:url`.
        raw.rsplit('#')
            .next()
            .unwrap_or_default()
            .split(':')
            .next()
            .unwrap_or_default()
            .to_string()
    }

    /// A refusal's `message` member (every error shape in this service models it
    /// with `jsonName "message"`).
    pub fn message(&self) -> String {
        self.json()
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    }
}

/// Send one request to the emulator, signed into the `geo` credential scope so the
/// router's `can_handle` claims it for this service.
pub async fn call(
    router: &MultiServiceRouter,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> Answer {
    let bytes = match &body {
        Some(value) => bytes::Bytes::from(serde_json::to_vec(value).expect("test body serializes")),
        None => bytes::Bytes::new(),
    };
    call_in_region(router, method, path, bytes, REGION).await
}

/// One request as this crate's suites send it: addressed to `region`'s `geo`
/// endpoint and signed into that credential scope, so the router's `can_handle`
/// claims it for this service.
///
/// The ONE place a request is built, so the helpers below cannot drift apart.
fn signed_request(
    method: &str,
    path: &str,
    body: bytes::Bytes,
    region: &str,
) -> http::Request<bytes::Bytes> {
    let authorization = format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20240101/{region}/geo/aws4_request, \
         SignedHeaders=host, Signature=abc"
    );
    http::Request::builder()
        .method(method)
        .uri(format!("https://geo.{region}.amazonaws.com{path}"))
        .header("authorization", authorization)
        .header("content-type", "application/json")
        .body(body)
        .expect("test request builds")
}

/// Send one request whose SigV4 credential scope names `region`.
pub async fn call_in_region(
    router: &MultiServiceRouter,
    method: &str,
    path: &str,
    body: bytes::Bytes,
    region: &str,
) -> Answer {
    let response = router
        .dispatch(signed_request(method, path, body, region))
        .await;
    let status = response.status().as_u16();
    let error_type = response
        .headers()
        .get("x-amzn-errortype")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let body = String::from_utf8_lossy(response.body()).to_string();
    Answer {
        status,
        error_type,
        body,
    }
}

/// What the emulator answered to a request whose response body is BYTES rather
/// than JSON: a blob-payload read, where the headers carry the metadata.
pub struct RawAnswer {
    pub status: u16,
    pub headers: ::http::HeaderMap,
    pub body: Vec<u8>,
}

impl RawAnswer {
    /// One response header's value, or `None` when the answer carries no such
    /// header.
    pub fn header(&self, name: &str) -> Option<String> {
        self.headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string)
    }
}

/// Send one request and keep the response body as raw bytes.
///
/// [`call`] renders the body as text, which a PNG payload does not survive; a
/// suite that reads a blob-payload operation uses this instead.
pub async fn call_bytes(router: &MultiServiceRouter, method: &str, path: &str) -> RawAnswer {
    let response = router
        .dispatch(signed_request(method, path, bytes::Bytes::new(), REGION))
        .await;
    RawAnswer {
        status: response.status().as_u16(),
        headers: response.headers().clone(),
        body: response.body().to_vec(),
    }
}

/// Epoch milliseconds of an ISO 8601 timestamp member (`timestampFormat: date-time`).
///
/// Every timestamp member of this model targets one shape and that shape declares
/// `date-time`, so the member is an ISO 8601 STRING on the wire and this fails on any
/// other rendering: the suites that compare instants pin the form too.
pub fn millis_of(value: &Value, member: &str) -> i64 {
    let found = value
        .get(member)
        .unwrap_or_else(|| panic!("{member} must be present in {value}"));
    let text = found
        .as_str()
        .unwrap_or_else(|| panic!("{member} must be an ISO 8601 string, got {found}"));
    DateTime::from_str(text, Format::DateTime)
        .unwrap_or_else(|e| panic!("{member} {text:?} must parse as ISO 8601: {e}"))
        .to_millis()
        .unwrap_or_else(|e| panic!("{member} {text:?} must fit in epoch millis: {e}"))
}

/// Epoch milliseconds of an ISO 8601 literal written in a test table.
pub fn iso_millis(iso: &str) -> i64 {
    DateTime::from_str(iso, Format::DateTime)
        .unwrap_or_else(|e| panic!("the table's timestamp {iso:?} must be ISO 8601: {e}"))
        .to_millis()
        .expect("epoch millis")
}

/// The media type EVERY tile of the map plane is labelled with, whatever the
/// bytes inside it encode.
///
/// Source: recorded behaviour of `GetMapTile` — the model fixes no value for
/// `GetMapTileResponse$ContentType` (its doc names one only "for example"), and
/// the recorded 200 of a vector tile carries `application/octet-stream`, the
/// media type this plane labels its binary assets with (the
/// `GetMapGlyphsResponse$ContentType` doc names the same one). Stated here once,
/// so the suites that read a tile and the suites that read the document
/// declaring it cannot disagree about it.
pub const TILE_CONTENT_TYPE: &str = "application/octet-stream";

/// The media type a style descriptor is served as. The
/// `GetMapStyleDescriptorResponse$ContentType` doc: "The style descriptor's
/// content type. For example, application/json."
pub const STYLE_CONTENT_TYPE: &str = "application/json";

/// Every style the service publishes, with the provider it is sourced from and
/// the deepest zoom its tiles exist for, read from the crate's own catalog: a
/// suite spans the whole catalog rather than a sample of it, and a style added
/// to the catalog is covered without a suite being edited.
pub fn published_styles() -> Vec<(&'static str, &'static str, u32)> {
    service_com_amazonaws_location_locationservice::state::MAP_STYLES
        .iter()
        .map(|entry| (entry.style, entry.data_source, entry.max_zoom))
        .collect()
}

/// Whether a style's basemap is drawn from images rather than vector data, which
/// is what the tile BODY has to decode as.
///
/// Source: the `MapConfiguration$Style` doc, which describes `RasterEsriImagery`
/// as "A raster basemap" and `HybridHereExploreSatellite` as a map "over
/// satellite imagery", against the `Vector…` styles' "vector basemap".
pub fn raster_basemap(style: &str) -> bool {
    style.starts_with("Raster") || style.starts_with("Hybrid")
}

/// Create the map resource `name` carrying `style`, asserting the create was
/// served — the step every map-plane suite starts from.
pub async fn create_map(router: &MultiServiceRouter, name: &str, style: &str) {
    let created = call(
        router,
        "POST",
        "/maps/v0/maps",
        Some(json!({ "MapName": name, "Configuration": { "Style": style } })),
    )
    .await;
    assert_eq!(
        created.status, 200,
        "case {name:?}/{style:?}: the map must be created, got {} {:?}",
        created.status, created.body
    );
}

/// The style descriptor of `map`, decoded as the JSON document it is served as.
pub async fn style_descriptor(router: &MultiServiceRouter, map: &str) -> Value {
    let served = call_bytes(
        router,
        "GET",
        &format!("/maps/v0/maps/{map}/style-descriptor"),
    )
    .await;
    assert_eq!(
        served.status, 200,
        "case {map:?}: the style descriptor must be served, got {} {:?}",
        served.status,
        String::from_utf8_lossy(&served.body)
    );
    assert_eq!(
        served.header("content-type").as_deref(),
        Some(STYLE_CONTENT_TYPE),
        "case {map:?}: the descriptor is served as JSON, got {:?}",
        served.header("content-type")
    );
    serde_json::from_slice(&served.body).unwrap_or_else(|error| {
        panic!(
            "case {map:?}: the descriptor must be JSON, got {error}: {:?}",
            String::from_utf8_lossy(&served.body)
        )
    })
}

/// The tile at `zoom` whose centre lies over land, found by walking the grid of
/// that zoom with the same ground the service reads — so a case read against it
/// is about a tile whose land layers are drawn at all.
///
/// Here rather than in one suite because both map-plane suites that read a tile's
/// own content need the same address.
pub fn land_tile(
    zoom: u32,
) -> service_com_amazonaws_location_locationservice::state::map_tiles::TileAddress {
    tile_over_ground(zoom, false)
}

/// The tile at `zoom` whose centre lies over WATER, found the same way: a case
/// about the water layers a tile carries needs a tile that carries them.
pub fn water_tile(
    zoom: u32,
) -> service_com_amazonaws_location_locationservice::state::map_tiles::TileAddress {
    tile_over_ground(zoom, true)
}

/// The first tile of `zoom`'s grid whose centre lies over the asked-for ground,
/// walked with the same ground field the service reads.
fn tile_over_ground(
    zoom: u32,
    over_water: bool,
) -> service_com_amazonaws_location_locationservice::state::map_tiles::TileAddress {
    use service_com_amazonaws_location_locationservice::state::map_tiles;
    let axis = u32::try_from(map_tiles::TileAddress::axis_tiles(zoom)).unwrap_or(u32::MAX);
    // Walked with a stride rather than tile by tile: at a deep zoom neighbouring
    // tiles cover almost the same ground, so a stride is what reaches both sides
    // of a shoreline within a bounded number of probes.
    for step in 0..4096_u32 {
        let address = map_tiles::TileAddress {
            z: zoom,
            x: (axis / 7).saturating_add(step.saturating_mul(131)) % axis,
            y: (axis / 3).saturating_add(step.saturating_mul(173)) % axis,
        };
        let (longitude, latitude) = address.centre();
        if map_tiles::over_water(longitude, latitude) == over_water {
            return address;
        }
    }
    panic!(
        "no tile of zoom {zoom} covers {}, so a case over it cannot be read",
        if over_water { "water" } else { "land" }
    );
}

/// The wire form of a timestamp inside a JSON body.
///
/// Every timestamp member of this model targets one shape and that shape carries
/// `@timestampFormat("date-time")`, so a body timestamp is the ISO 8601 STRING the
/// tables are written in. Kept as a seam so a table reads as the API documents it
/// while the request still speaks the wire form.
pub fn wire_time(iso: &str) -> Value {
    json!(iso)
}
