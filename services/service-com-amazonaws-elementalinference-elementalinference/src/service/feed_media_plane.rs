// SPDX-License-Identifier: CC0-1.0

//! The feed's MEDIA data plane: what answers at the address `dataEndpoints` advertises.
//!
//! # Why it is not a modelled operation
//!
//! `smithy_query(query="digest")` reports 21 operations for this service and every one of
//! them is control plane: they create, read, update, associate, tag and delete feeds and
//! dictionaries. The media interactions a caller then performs against the endpoint a feed
//! reports — `GetFeedResponse$dataEndpoints`, "the feed receives source media at this
//! endpoint (when the calling application calls PutMedia) and returns the resulting
//! metadata to this endpoint (when the calling application calls GetMetadata)" — are not
//! modelled: they are addressed by path and HTTP method on the same signed
//! `elemental-inference` wire. Nothing in the generated routing table can name them, so
//! they are reached through [`crate::service::EmulatedService::override_operation`], the
//! one seam a service has for a request the model's routing key cannot name.
//!
//! # What is served
//!
//! `POST /v1/feed/{id}/input/{n}/metadata` — the metadata QUERY for one output of the
//! feed over one window of the media timeline. The path space is the one
//! [`crate::state::parse_feed_metadata_path`] reads and
//! [`crate::state::feed_data_endpoints`] advertises, so the address the control plane
//! hands a client is the address this plane answers at.
//!
//! The query is validated against the request and against STORED feed state, in that
//! order — what the request itself is, before any record is consulted, then what the feed
//! holds:
//!
//! * a body that is not a JSON object, an absent or empty `outputName`, an absent
//!   `timeSpecification`, a time specification that selects no window this plane reads, or
//!   a window whose bounds or timescale cannot describe a span, are each the modelled
//!   `ValidationException` (HTTP 400);
//! * a feed id that names no feed of the request's Region, and an input the feed exposes
//!   none of, are the modelled `ResourceNotFoundException` (HTTP 404) — the same refusal
//!   the control plane answers for an identifier that resolves to nothing;
//! * an `outputName` the feed holds no output under is a `ValidationException`, worded as
//!   this service words every other reference that resolves to nothing
//!   ([`crate::state::cannot_find_output_message`]).
//!
//! A query the feed admits answers `200` with the metadata document: the `items` the feed
//! holds for that output inside the requested window. Metadata exists only for media the
//! feed has RECEIVED at that input, and nothing delivers media to a feed here — this
//! service models no ingest operation and this plane mounts no ingest route — so a feed's
//! item set is empty and the document reports it empty. The list is READ, never
//! synthesised: an item this emulator did not get from delivered media would be a
//! metadata document for inference nothing ran.
//!
//! # The envelope
//!
//! This service is `restJson1`, so an answer this plane words itself carries the same
//! envelope the framework renders for a modelled error of the same class — the `__type`
//! and `message` members and the `x-amzn-errortype` header — and a client branches on it
//! exactly as it does on a control-plane refusal.

use ::aws_smithy_runtime_api::http::{Request as RtRequest, Response as RtResponse, StatusCode};
use ::aws_smithy_types::body::SdkBody;
use ::serde_json::{json, Value};
use ::smithy_server_runtime::{BoxFuture, ErasedOperation, OperationContext, Service};
use ::std::sync::Arc;

use crate::state::{service_state, Feed, FeedMetadataPath};

/// The method a metadata query is sent by: the one method this path space serves.
const METADATA_METHOD: &str = "POST";

/// The media type every answer on this plane carries, this service's wire being
/// `restJson1`.
const JSON_MEDIA_TYPE: &str = "application/json";

/// The member a metadata document reports its metadata under.
const ITEMS_MEMBER: &str = "items";

/// The member a metadata query names the feed output it is about in.
const OUTPUT_NAME_MEMBER: &str = "outputName";

/// The member a metadata query carries its media-timeline window in.
const TIME_SPECIFICATION_MEMBER: &str = "timeSpecification";

/// The window member this plane reads: a span of the media timeline given as
/// presentation timestamps.
const PTS_BASED_MEMBER: &str = "ptsBased";

/// The first presentation timestamp of a PTS-based window.
const START_PTS_MEMBER: &str = "startPts";

/// The last presentation timestamp of a PTS-based window.
const END_PTS_MEMBER: &str = "endPts";

/// The number of PTS units one second of the media timeline carries, which is what makes
/// the two timestamps a duration.
const TIMESCALE_MEMBER: &str = "timescale";

/// The modelled error a request this plane cannot read is refused with
/// (`smithy_query(query="shape",shape="ValidationException")`: client error, HTTP 400,
/// `message` required).
const VALIDATION_EXCEPTION: &str = "ValidationException";

/// The HTTP status that error carries.
const VALIDATION_STATUS: u16 = 400;

/// The modelled error an identifier that resolves to nothing is refused with
/// (`smithy_query(query="shape",shape="ResourceNotFoundException")`: client error, HTTP
/// 404, `message` required).
const RESOURCE_NOT_FOUND_EXCEPTION: &str = "ResourceNotFoundException";

/// The HTTP status that error carries.
const NOT_FOUND_STATUS: u16 = 404;

/// The status a metadata query the feed admits is answered with.
const OK_STATUS: u16 = 200;

/// How long the name of a feed output may be.
///
/// An output's name targets the modelled `ResourceName`, whose pattern is
/// `^[a-zA-Z0-9]([a-zA-Z0-9-_]{0,126}[a-zA-Z0-9])?$`
/// (`smithy_query(query="shape",shape="GetOutput")`), so 128 characters is the longest
/// name a feed can hold an output under.
const MAX_OUTPUT_NAME_LEN: usize = 128;

/// The error this plane answers with when this service's own store cannot be reached —
/// the same `InternalServerErrorException` every handler of this service answers in that
/// position.
const INTERNAL_SERVER_ERROR_EXCEPTION: &str = "InternalServerErrorException";

/// The HTTP status that error carries.
const INTERNAL_STATUS: u16 = 500;

/// The auth schemes a metadata query is verified under.
///
/// SigV4 is the only scheme this service's model declares
/// (`smithy_query(query="digest")`: `authSchemes: ["sigv4"]`), and it is the scheme a
/// client signs a request to a feed's data endpoint with. This is NOT a modelled
/// operation, so without naming the scheme here the router would find none to verify the
/// credential a metadata query carries against.
const METADATA_AUTH_SCHEMES: &[&str] = &[::emulator_server::aws_credentials::SIGV4_SCHEME];

/// Whether this plane serves the request `req` addresses: a `POST` on the metadata path
/// space a feed's advertised data endpoint is rooted at.
///
/// The claim is on the METHOD and the path shape, not on a feed the store holds: the
/// address was advertised by this service, so a query naming a feed that is gone is this
/// plane's to refuse with the service's own not-found error rather than something for the
/// front door to report as an unimplemented operation.
pub fn claims_request(req: &RtRequest) -> bool {
    req.method().eq_ignore_ascii_case(METADATA_METHOD)
        && crate::state::parse_feed_metadata_path(::service_common::request_path(req.uri()))
            .is_some()
}

/// The feed media plane as the ROUTER serves it: the operation
/// `Service::override_operation` hands back for a metadata query.
pub struct FeedMediaPlane<Eng> {
    /// The ONE shared engine this service's operations read and write, so a query served
    /// here sees exactly the feeds the control plane persisted.
    engine: Arc<Eng>,
}

impl<Eng: Send + Sync + 'static> FeedMediaPlane<Eng> {
    /// Mount the plane over the shared engine.
    pub fn new(engine: Arc<Eng>) -> Self {
        Self { engine }
    }
}

impl<Eng: Send + Sync + 'static> ErasedOperation for FeedMediaPlane<Eng> {
    fn run<'r>(
        &'r self,
        _svc: &'r dyn Service,
        req: &'r RtRequest,
        ctx: &'r OperationContext<'r>,
    ) -> BoxFuture<'r, RtResponse> {
        Box::pin(async move { serve(&*self.engine, req, ctx) })
    }

    fn auth_schemes(&self) -> Option<&'static [&'static str]> {
        Some(METADATA_AUTH_SCHEMES)
    }
}

/// Serve one metadata query.
fn serve<Eng: Send + Sync + 'static>(
    engine: &Eng,
    req: &RtRequest,
    ctx: &OperationContext<'_>,
) -> RtResponse {
    let Some(addressed) =
        crate::state::parse_feed_metadata_path(::service_common::request_path(req.uri()))
    else {
        // Unreachable: `claims_request` routes only a path this reader accepts. Answering
        // the service's own not-found rather than panicking keeps the plane total.
        return refusal(
            NOT_FOUND_STATUS,
            RESOURCE_NOT_FOUND_EXCEPTION,
            crate::state::NOT_FOUND_MESSAGE,
        );
    };

    // WHAT THE REQUEST IS, read before any record is consulted, so each of these
    // refusals is the same whatever the store holds. The router hands this seam a
    // request whose body it has already buffered, so an absent body is an empty one.
    let body = req.body().bytes().unwrap_or_default();
    let output_name = match read_query(body) {
        Ok(output_name) => output_name,
        Err(message) => return refusal(VALIDATION_STATUS, VALIDATION_EXCEPTION, &message),
    };

    // WHAT THE FEED HOLDS. The Region is the one the request was signed for, exactly as
    // every control-plane read resolves a feed id: this service is regional, so an id
    // that names a feed of another Region names no feed of this endpoint.
    let region = crate::state::request_region(ctx.parts());
    let Some(state) = service_state(engine) else {
        return refusal(
            INTERNAL_STATUS,
            INTERNAL_SERVER_ERROR_EXCEPTION,
            "the com.amazonaws.elementalinference state store is unavailable",
        );
    };
    let mut state = state.lock();
    // The one write a query makes: the status transitions the clock already owed these
    // feeds, which were due at this instant whether or not anybody queried.
    state.settle_due_transitions(::std::time::Instant::now());
    let Some(feed) = state.feed(&region, &addressed.feed_id) else {
        return refusal(
            NOT_FOUND_STATUS,
            RESOURCE_NOT_FOUND_EXCEPTION,
            crate::state::NOT_FOUND_MESSAGE,
        );
    };
    if addressed_input(feed, &addressed).is_none() {
        return refusal(
            NOT_FOUND_STATUS,
            RESOURCE_NOT_FOUND_EXCEPTION,
            crate::state::NOT_FOUND_MESSAGE,
        );
    }
    if feed.output_named(&output_name).is_none() {
        return refusal(
            VALIDATION_STATUS,
            VALIDATION_EXCEPTION,
            &crate::state::cannot_find_output_message(&output_name),
        );
    }

    // The metadata the feed holds for that output inside the requested window. A feed
    // holds metadata only for media it has RECEIVED at the input, and nothing delivers
    // media to a feed in this emulator, so the document reports what the feed really has:
    // no items. Nothing is synthesised into it.
    served(json!({ITEMS_MEMBER: Value::Array(Vec::new())}))
}

/// The media input the query's path addresses, or `None` when the feed exposes none
/// there.
///
/// The path spells the input's POSITION in the feed's input set, so a spelling that is
/// not a position names no input at all — the same answer as a position past the set.
fn addressed_input<'a>(feed: &'a Feed, addressed: &FeedMetadataPath) -> Option<&'a str> {
    let position = addressed.input.parse::<usize>().ok()?;
    feed.audio_input_at(position)
}

/// Read one metadata query off the request body and answer with the feed output it is
/// about, its media-timeline window validated ([`validate_window`]).
///
/// Every clause is about the request alone: the body being a JSON object, the output name
/// being present and shaped like an output's name, the time specification selecting a
/// window this plane reads, and that window describing a span. None of them consults the
/// store, so each of these refusals is the same whatever the store holds.
fn read_query(body: &[u8]) -> Result<String, String> {
    let Ok(Value::Object(members)) = ::serde_json::from_slice::<Value>(body) else {
        return Err("The request body must be a JSON object.".to_string());
    };
    let output_name = members
        .get(OUTPUT_NAME_MEMBER)
        .and_then(Value::as_str)
        .filter(|name| is_output_name(name))
        .ok_or_else(|| format!("{OUTPUT_NAME_MEMBER} must be the name of an output of the feed."))?
        .to_string();
    let Some(Value::Object(specification)) = members.get(TIME_SPECIFICATION_MEMBER) else {
        return Err(unreadable_window());
    };
    validate_window(specification)?;
    Ok(output_name)
}

/// What this endpoint answers a `timeSpecification` it cannot read a window out of.
///
/// The media data plane is not in this service's model, so which window forms the real
/// service takes is not stated anywhere this emulator can read: the sentence therefore
/// says which form THIS endpoint reads rather than calling the other forms invalid.
fn unreadable_window() -> String {
    format!(
        "{TIME_SPECIFICATION_MEMBER} must name {PTS_BASED_MEMBER}: this endpoint reads \
         only presentation-timestamp windows."
    )
}

/// Whether `value` is a name a feed can hold an output under: the modelled
/// `ResourceName` an output's name targets — an alphanumeric character at each end,
/// alphanumeric, `-` or `_` in between, and at most [`MAX_OUTPUT_NAME_LEN`] characters.
///
/// A value outside it names no output of any feed, so the query is refused on the member
/// itself rather than on a lookup of the feed — which is also what keeps a refusal's
/// sentence to a name a feed could really hold.
fn is_output_name(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.is_empty() || bytes.len() > MAX_OUTPUT_NAME_LEN {
        return false;
    }
    let ends_alphanumeric = bytes
        .first()
        .is_some_and(|byte| byte.is_ascii_alphanumeric())
        && bytes
            .last()
            .is_some_and(|byte| byte.is_ascii_alphanumeric());
    ends_alphanumeric
        && bytes
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

/// Check the media-timeline window a time specification selects.
///
/// A time specification selects ONE kind of window, and `ptsBased` — a span of
/// presentation timestamps in units of `timescale` per second — is the one this plane
/// reads: a specification carrying another member, or several, selects no window this
/// plane can answer for. The span itself has to be describable: a timescale turns the two
/// timestamps into a duration, so it has to be positive, and the start cannot sit after
/// the end.
fn validate_window(specification: &::serde_json::Map<String, Value>) -> Result<(), String> {
    let Some(Value::Object(pts_based)) = specification.get(PTS_BASED_MEMBER) else {
        return Err(unreadable_window());
    };
    if specification.len() != 1 {
        return Err(format!(
            "{TIME_SPECIFICATION_MEMBER} names one kind of window, so it must carry \
             {PTS_BASED_MEMBER} alone."
        ));
    }
    let member = |name: &str| -> Result<i64, String> {
        pts_based
            .get(name)
            .and_then(Value::as_i64)
            .ok_or_else(|| format!("{PTS_BASED_MEMBER}.{name} must be a whole number."))
    };
    let start_pts = member(START_PTS_MEMBER)?;
    let end_pts = member(END_PTS_MEMBER)?;
    let timescale = member(TIMESCALE_MEMBER)?;
    if timescale <= 0 {
        return Err(format!(
            "{PTS_BASED_MEMBER}.{TIMESCALE_MEMBER} must be greater than 0."
        ));
    }
    if start_pts > end_pts {
        return Err(format!(
            "{PTS_BASED_MEMBER}.{START_PTS_MEMBER} must not be greater than \
             {PTS_BASED_MEMBER}.{END_PTS_MEMBER}."
        ));
    }
    Ok(())
}

/// The document a query the feed admits is answered with.
fn served(document: Value) -> RtResponse {
    rendered(OK_STATUS, &document, None)
}

/// One refusal, in the envelope this service's `restJson1` wire renders a modelled error
/// of the same class in: the `__type` and `message` members, and the `x-amzn-errortype`
/// header a `restJson1` client resolves the error shape from.
fn refusal(status: u16, code: &str, message: &str) -> RtResponse {
    rendered(
        status,
        &json!({"__type": code, "message": message}),
        Some(code),
    )
}

/// One answer: a status, a JSON document, and the error type when the answer is a
/// refusal.
fn rendered(status: u16, document: &Value, error_code: Option<&str>) -> RtResponse {
    let body = document.to_string().into_bytes();
    let length = body.len();
    let code = StatusCode::try_from(status).unwrap_or_else(|_| {
        StatusCode::try_from(INTERNAL_STATUS).expect("500 is a status code")
    });
    let mut answer = RtResponse::new(code, SdkBody::from(body));
    answer
        .headers_mut()
        .insert("content-type", JSON_MEDIA_TYPE.to_string());
    answer
        .headers_mut()
        .insert("content-length", length.to_string());
    if let Some(code) = error_code {
        answer
            .headers_mut()
            .insert("x-amzn-errortype", code.to_string());
    }
    answer
}
