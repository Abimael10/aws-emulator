// SPDX-License-Identifier: CC0-1.0

//! How this plane reads the `mode` a GRAPH SUMMARY request asks for.
//!
//! # The rule
//!
//! The summary endpoints bind `mode` to the query string
//! (`smithy_query(query="operation", operation="GetPropertygraphSummary,GetRDFGraphSummary")`
//! -> `mode` targeting `GraphSummaryType` with `httpQuery: "mode"` and the default
//! `basic`), and the member's own documentation states the values it takes in UPPER case —
//! "Mode can take one of two values: BASIC (the default), and DETAILED" — while the model's
//! value set is `[basic, detailed]`. The documented spelling and the modelled spelling are
//! the same value in two cases, so the value a client sends is matched to the modelled
//! value set WITHOUT regard to ASCII case, and the summary it names is the one that is
//! served. A value that matches none of the modelled values still names no mode, in any
//! case, and is refused with one of the operation's own modelled exception classes:
//! `IllegalArgumentException` — "raised when an argument in a request is not supported" —
//! naming the member, the value and the modes the member accepts, worded by the one builder
//! the handlers of this crate refuse an unsupported member value with
//! ([`crate::state::unsupported_graph_summary_mode`]).
//!
//! Nothing else about the request is touched: the value set enforced is the model's, the
//! other query parameters and every header and body byte are passed on byte for byte, and
//! an operation this wrapper does not front is served exactly as before.
//!
//! # The gap this closes
//!
//! The framework's walk validates a member against the modelled enum as case-SENSITIVE
//! equality, and the one loose-case exception the vendored codegen carries is keyed to two
//! `@httpHeader` names (`vendor/smithy-macros/src/codegen.rs`), so a request asking for the
//! documented `DETAILED` summary is refused before the handler runs — with HTTP 400 and the
//! framework's own `ValidationException`, which is not a shape this service's model declares
//! at all, so an SDK's error-shape dispatch cannot match it either. A wrapper, unlike the
//! `Service::override_refusal` seam, is handed the request AND the operation: it can resolve
//! the spelling before the walk reads the request, and answer the refusal in the modelled
//! shape, serialized by this service's own protocol.

use ::aws_smithy_runtime_api::http::{Request as RtRequest, Response as RtResponse};
use ::service_common::response_media_type::ResponseMediaTypeOperation;
use ::smithy_server_runtime::{BoxFuture, ErasedOperation, OperationContext, Service};

use crate::ops::types::{IllegalArgumentException, SCHEMA_ILLEGAL_ARGUMENT_EXCEPTION};

/// The query parameter the summary operations bind their mode to: the `@httpQuery` name of
/// the member, which is what a request carries it under.
const MODE_QUERY_PARAMETER: &str = "mode";

/// The HTTP status the modelled `IllegalArgumentException` is answered with
/// (`@error client`, `@httpError 400`) — restated here because serializing a modelled error
/// takes its status as an argument.
const ILLEGAL_ARGUMENT_STATUS: u16 = 400;

/// The operations that bind `GraphSummaryType` to `@httpQuery("mode")`, by the name the
/// protocol resolves as their routing key.
///
/// Source: the model's two graph-summary operations
/// (`smithy_query(query="operation", operation="GetPropertygraphSummary,GetRDFGraphSummary")`).
pub const GRAPH_SUMMARY_OPERATIONS: &[&str] = &["GetPropertygraphSummary", "GetRDFGraphSummary"];

/// One graph-summary operation, served with its `mode` read the way the member is
/// documented, and with a value that names no mode refused in the modelled shape.
pub struct GraphSummaryMode {
    /// The service-wide JSON announcement. It serves the modelled operation the routing key
    /// names — the refusal the framework renders inside that walk included — and announces
    /// this service's media type on what it answered.
    announced: ResponseMediaTypeOperation,
}

impl GraphSummaryMode {
    /// Read the mode, then serve the walk through `announced`.
    pub const fn new(announced: ResponseMediaTypeOperation) -> Self {
        Self { announced }
    }
}

impl ErasedOperation for GraphSummaryMode {
    fn run<'r>(
        &'r self,
        svc: &'r dyn Service,
        req: &'r RtRequest,
        ctx: &'r OperationContext<'r>,
    ) -> BoxFuture<'r, RtResponse> {
        Box::pin(async move {
            match read_mode(req) {
                Mode::AsSent => self.announced.run(svc, req, ctx).await,
                // The rewritten request lives for the call it is read by, which is all the
                // framework's walk borrows it for; `ctx` still carries the request parts
                // the client sent, so a handler reading the URI sees the original.
                Mode::Rewritten(rewritten) => self.announced.run(svc, &rewritten, ctx).await,
                // Refused here rather than by the walk, so the caller gets one of the
                // operation's own modelled error classes; the answer is announced like
                // every other response of this service.
                Mode::Unaccepted(value) => self.announced.announce(unsupported_mode_response(
                    svc,
                    req,
                    &crate::state::unsupported_graph_summary_mode(&value),
                )),
            }
        })
    }

    fn auth_schemes(&self) -> Option<&'static [&'static str]> {
        // Not an operation of its own: the request is signed as the MODEL says for the
        // operation this wrapper stands in front of, so this wrapper cannot weaken it.
        self.announced.auth_schemes()
    }
}

/// What reading a request's `mode` answers.
enum Mode {
    /// The request carries no mode, or carries one in a spelling the modelled value set
    /// lists, so it is served exactly as it arrived.
    AsSent,
    /// The request with the mode rewritten into the modelled spelling of the SAME value.
    /// BOXED: a whole request dwarfs the other two answers, and this value is built on the
    /// one path that needs it.
    Rewritten(Box<RtRequest>),
    /// The value names no mode at all, carried here as the client sent it so the refusal
    /// can name the value it refused.
    Unaccepted(String),
}

/// Read the `mode` the query string of `req` binds.
///
/// The value is read the way the framework's query-value deserializer reads it — decoded as
/// one `application/x-www-form-urlencoded` component, which is what makes the verdict here
/// and the binding downstream agree about what the client sent — and the rewritten query
/// keeps every other pair byte for byte.
fn read_mode(req: &RtRequest) -> Mode {
    let Some((path, query)) = req.uri().split_once('?') else {
        return Mode::AsSent;
    };

    let mut rewritten = String::with_capacity(query.len());
    let mut changed = false;
    for (index, pair) in query.split('&').enumerate() {
        if index > 0 {
            rewritten.push('&');
        }
        let (raw_name, raw_value) = pair.split_once('=').unwrap_or((pair, ""));
        if smithy_server_runtime::percent_decode(raw_name) != MODE_QUERY_PARAMETER {
            rewritten.push_str(pair);
            continue;
        }
        let value = smithy_server_runtime::percent_decode(raw_value);
        // The modelled spelling of the value the client sent, which is what the walk
        // downstream binds. A value naming no mode is refused on the spot: nothing
        // downstream can serve it.
        let Some(modelled) = crate::state::graph_summary_mode_wire_value(&value) else {
            return Mode::Unaccepted(value.to_string());
        };
        if modelled == value {
            rewritten.push_str(pair);
            continue;
        }
        changed = true;
        rewritten.push_str(raw_name);
        rewritten.push('=');
        rewritten.push_str(modelled);
    }
    if !changed {
        return Mode::AsSent;
    }

    let Some(mut resolved) = req.try_clone() else {
        // A body this request cannot replay cannot be carried into a rewritten one. The
        // request is then served exactly as it arrived, which is the framework's own
        // refusal of the spelling it does not take — traced, because it is the one path
        // where this module does not apply its rule.
        ::tracing::warn!("a graph-summary mode was left unresolved: the request cannot be rebuilt");
        return Mode::AsSent;
    };
    match resolved.set_uri(format!("{path}?{rewritten}")) {
        Ok(()) => Mode::Rewritten(::std::boxed::Box::new(resolved)),
        Err(_) => {
            ::tracing::warn!(
                "a graph-summary mode was left unresolved: the rewritten URI does not parse"
            );
            Mode::AsSent
        }
    }
}

/// The answer a request naming no mode gets: the modelled `IllegalArgumentException`
/// (HTTP 400), rendered in this service's own protocol envelope.
///
/// Rendered from the SAME four facts the framework's own error tail renders a modelled
/// failure from — the error shape, its schema, its wire code and its modelled HTTP status —
/// so this is the answer the handler itself would have given for a value it could not
/// serve. `req` is the request being refused: the framework's own renderer, the fallback
/// when the modelled shape cannot be serialized, reads the protocol it is served in off it.
fn unsupported_mode_response(
    svc: &dyn Service,
    req: &RtRequest,
    refusal: &IllegalArgumentException,
) -> RtResponse {
    let definition = svc.definition();
    match definition.protocol().serialize_error(
        refusal,
        &SCHEMA_ILLEGAL_ARGUMENT_EXCEPTION,
        IllegalArgumentException::shape_name(),
        ILLEGAL_ARGUMENT_STATUS,
    ) {
        Ok(response) => response,
        Err(unserializable) => definition.framework_error_response(unserializable, req),
    }
}
