// SPDX-License-Identifier: CC0-1.0

//! WHICH REQUEST one invoke of a deployed stage is: the two ids API Gateway names a request
//! by, and which of the two a client may state for itself.
//!
//! A REST API reports two ids for every request it answers, and they differ in who decides
//! them:
//!
//! * the REQUEST id — `$context.requestId`, reported in [`CLIENT_REQUEST_ID_HEADER`] — is the
//!   id of the request, which a CLIENT MAY STATE by sending that same header: API Gateway
//!   documents it as an id clients can override, so a client that already names the call in
//!   its own records has the api name it the same way, in the context its mapping templates
//!   render, in the records its stage logs and in the header its answer carries. A request
//!   that states none is named by an id the endpoint mints for it;
//! * the EXTENDED request id — `$context.extendedRequestId` — is the id API GATEWAY mints,
//!   for every request and whatever the request carried. Nothing a client sends is echoed
//!   into it, which is exactly what makes it the id that identifies a call unambiguously
//!   while the one above can be a value the caller chose.
//!
//! So the two are minted together, ONCE per request, by this plane's one funnel
//! ([`super::serve`]), and travel from there to everything that names the request: the
//! context a mapping template renders against, the event a proxy integration's function is
//! handed, the record the stage logs and the response header the answer carries.

use super::*;

/// The header the request id travels in, in BOTH directions: API Gateway reports it on every
/// response it serves (`service_common` owns that reporting for the whole emulator), and a
/// client states the id of its own request by sending it.
///
/// HTTP header names are case-insensitive, and this is the lowercased spelling of the
/// `x-amzn-RequestId` API Gateway documents.
const CLIENT_REQUEST_ID_HEADER: &str = "x-amzn-requestid";

/// The response header API Gateway reports the EXTENDED request id in: the id of the request
/// as the ENDPOINT named it, which is why it is reported separately from the one above — a
/// client reads a call's unambiguous id off this header whatever it stated for itself.
const EXTENDED_REQUEST_ID_HEADER: &str = "x-amz-apigw-id";

/// Emulator resource guard, NOT an AWS quota: the longest id a request may state for itself.
///
/// The value travels into the context a template renders, the records the stage logs and the
/// header the answer carries, so a caller-chosen value is bounded before it is carried
/// anywhere — a request that states a longer one is named by a minted id instead.
const MAX_STATED_REQUEST_ID_CHARS: usize = 256;

/// The two ids one invoke of this plane is named by: see the module docs.
#[derive(Debug, Clone)]
pub(super) struct CallIds {
    /// `$context.requestId`, and the id the response reports: the one the REQUEST stated
    /// where it stated one.
    pub(super) request_id: String,
    /// `$context.extendedRequestId`: the id API Gateway minted for this request, which
    /// nothing the request carried decides.
    pub(super) extended_request_id: String,
}

impl CallIds {
    /// The ids `req` is named by.
    ///
    /// The extended id is minted whatever the request carries, and the request id is the one
    /// the request stated or, failing that, one minted beside it — so two requests are never
    /// named by one extended id, however a caller spells the header.
    pub(super) fn of_request(req: &RtRequest) -> Self {
        Self {
            request_id: stated_request_id(req)
                .unwrap_or_else(::service_common::query_error_envelope::new_request_id),
            extended_request_id: ::service_common::query_error_envelope::new_request_id(),
        }
    }
}

/// The id `req` STATED for itself, or `None` for a request that stated none.
///
/// Surrounding space is not part of a header's value, so it is not part of the id either. A
/// value that is empty once trimmed states nothing, and one carrying a character a header,
/// a log record or a rendered body cannot carry — a control character, a byte outside the
/// visible ASCII the header grammar allows — is not carried into any of them: such a request
/// is named by a minted id, exactly as one that stated nothing is.
fn stated_request_id(req: &RtRequest) -> Option<String> {
    let stated = req.headers().get(CLIENT_REQUEST_ID_HEADER)?.trim();
    let carried = !stated.is_empty()
        && stated.chars().count() <= MAX_STATED_REQUEST_ID_CHARS
        && stated.chars().all(|glyph| glyph.is_ascii_graphic());
    carried.then(|| stated.to_string())
}

/// `served` with this call's EXTENDED id reported in the header API Gateway reports it in.
///
/// It REPLACES what the response already carries under that name: the extended id is the
/// endpoint's own reading of which request this is, so a value a backend answered under that
/// header is not what the api reports — a proxy integration's backend keeps its own value
/// under the remapped name instead (`super::onward_endpoint`). A value the HTTP layer will
/// not carry is traced and left off rather than failing a response that is already computed.
pub(super) fn with_extended_request_id_header(mut served: RtResponse, ids: &CallIds) -> RtResponse {
    if let Err(refused) = served.headers_mut().try_insert(
        EXTENDED_REQUEST_ID_HEADER,
        ids.extended_request_id.clone(),
    ) {
        ::tracing::warn!(
            error = %refused,
            "the extended request id is not a value a header can carry, so the response does \
             not report it"
        );
    }
    served
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One request carrying `header` under the request-id header, or none at all.
    fn request(header: Option<&str>) -> RtRequest {
        let mut req = RtRequest::new(SdkBody::empty());
        if let Some(value) = header {
            req.headers_mut()
                .try_insert(CLIENT_REQUEST_ID_HEADER, value.to_string())
                .expect("the test's own header value is one a request can carry");
        }
        req
    }

    /// RULE: the request id is the one the REQUEST stated, where it stated one a header, a
    /// record and a body can carry; every other request is named by a minted id.
    #[test]
    fn the_request_id_is_the_one_the_request_stated() {
        /// (case, the header the request carried, the id it is named by)
        type Row<'a> = (&'a str, Option<&'a str>, Option<&'a str>);
        let rows: &[Row<'_>] = &[
            ("no header at all", None, None),
            (
                "an id of the client's own spelling",
                Some("sim-7f3a-order-4821"),
                Some("sim-7f3a-order-4821"),
            ),
            (
                "an id of the form the endpoint itself mints",
                Some("11111111-2222-3333-4444-555555555555"),
                Some("11111111-2222-3333-4444-555555555555"),
            ),
            ("a single character", Some("x"), Some("x")),
            (
                "space around the value, which is not part of it",
                Some("  trimmed-id  "),
                Some("trimmed-id"),
            ),
            ("an empty value", Some(""), None),
            ("space alone", Some("   "), None),
            (
                "a value with a space inside it, which no header value of an id carries",
                Some("two words"),
                None,
            ),
            (
                "a value carrying a tab",
                Some("before\tafter"),
                None,
            ),
            (
                "the longest value carried",
                Some(&"a".repeat(MAX_STATED_REQUEST_ID_CHARS)),
                Some(&"a".repeat(MAX_STATED_REQUEST_ID_CHARS)),
            ),
            (
                "one character past the longest",
                Some(&"a".repeat(MAX_STATED_REQUEST_ID_CHARS + 1)),
                None,
            ),
        ];
        for (case, header, expected) in rows {
            let ids = CallIds::of_request(&request(*header));
            match expected {
                Some(stated) => assert_eq!(
                    ids.request_id, *stated,
                    "case {case:?}: expected the request to be named {stated:?}, got {:?}",
                    ids.request_id
                ),
                None => assert!(
                    ::service_common::query_error_envelope::is_request_id(&ids.request_id),
                    "case {case:?}: expected a minted request id, got {:?}",
                    ids.request_id
                ),
            }
            // Whatever the request stated, the EXTENDED id is the endpoint's own.
            assert!(
                ::service_common::query_error_envelope::is_request_id(
                    &ids.extended_request_id
                ),
                "case {case:?}: expected a minted extended request id, got {:?}",
                ids.extended_request_id
            );
            assert_ne!(
                ids.extended_request_id, ids.request_id,
                "case {case:?}: expected the two ids to name the request separately, got \
                 {:?} twice",
                ids.request_id
            );
        }
    }

    /// RULE: the extended id names ONE request — a fresh one per call, whether or not the
    /// request stated an id of its own.
    #[test]
    fn the_extended_id_is_minted_for_every_request() {
        let stated = "a-client-stated-id";
        let mut extended = std::collections::BTreeSet::new();
        for repeat in 1..=4 {
            let ids = CallIds::of_request(&request(Some(stated)));
            assert_eq!(
                ids.request_id, stated,
                "case \"repeat {repeat}\": expected the stated id to name the request, got {:?}",
                ids.request_id
            );
            assert!(
                extended.insert(ids.extended_request_id.clone()),
                "case \"repeat {repeat}\": expected an extended id no earlier call was named \
                 by, got {:?}",
                ids.extended_request_id
            );
        }
    }
}
