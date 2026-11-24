// SPDX-License-Identifier: CC0-1.0

//! How this endpoint refuses a media call that reached the data endpoint of ANOTHER
//! stream.
//!
//! An endpoint is discovered PER STREAM — `GetDataEndpoint` takes `StreamName`/`StreamARN`
//! so that "an endpoint for a specified stream" is what it reports — so the address a
//! client was handed serves that one stream, and a call there naming another stream used
//! an endpoint that does not serve the stream it asked for. That is the one condition this
//! service's own `InvalidEndpointException` is declared for, trigger and remedy both:
//! "Status Code: 400, Caller used wrong endpoint to write data to a stream. On receiving
//! such an exception, the user must call `GetDataEndpoint` with `AccessMode` set to
//! \"READ\" and use the endpoint Kinesis Video returns in the next `GetMedia` call"
//! (`smithy_query(query="shape", shape="InvalidEndpointException")`).
//!
//! It is a DIFFERENT condition from the one `ResourceNotFoundException` documents ("Status
//! Code: 404, The stream with the given name does not exist"), and the two are not
//! interchangeable: the remedy the wrong-endpoint class carries is to re-run discovery and
//! retry at the address it reports, which a client told its stream does not exist never
//! does. So the registry's answer decides first — an identifier it does not hold is the
//! not-found class, whichever address asked for it — and this class is for a stream that
//! DID resolve at an address announced for a different one.
//!
//! Both of this endpoint's data routes refuse it the same way: the modelled
//! [`crate::ops::get_media`] read, and the recorded `POST /putMedia` ingest beside it
//! ([`super::put_media`]), which states this service's modelled error vocabulary in its
//! own envelope. The facts of the refusal live here once so the two routes of one endpoint
//! cannot drift on how it is stated.

/// The error code both of this endpoint's data routes state a wrong-endpoint refusal
/// under: the modelled shape's own name, which is what a client's re-discovery path
/// branches on.
pub const ERROR_CODE: &str = "InvalidEndpointException";

/// The status the model gives that shape (`httpError` 400), stated here for the ingest
/// route, whose answers are built by hand because no model binds it.
pub const STATUS: u16 = 400;

/// The sentence the refusal states: the one Kinesis Video Media documents for this error
/// class, stating the condition and then the remedy — re-run `GetDataEndpoint` and retry
/// at the endpoint it reports.
///
/// The whole message is fixed text, as this family's other refusals are: no stream name,
/// ARN or address from the request or from this service's state reaches it, so a refusal
/// tells a caller nothing about which streams exist or where their endpoints are.
pub const MESSAGE: &str = "Caller used wrong endpoint to write data to a stream. On \
                           receiving such an exception, the user must call GetDataEndpoint \
                           with AccessMode set to \"READ\" and use the endpoint Kinesis \
                           Video returns in the next GetMedia call.";
