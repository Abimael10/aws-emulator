// SPDX-License-Identifier: CC0-1.0

//! The CUSTOM ERROR RESPONSES the distribution's configuration states: what a viewer receives
//! when the answer the edge holds for its request is an error, and how long that error is held.
//!
//! `DistributionConfig$CustomErrorResponses` "controls whether CloudFront replaces HTTP status
//! codes in the 4xx and 5xx range with custom error messages before returning the response to
//! the viewer" and "how long CloudFront caches HTTP status codes in the 4xx and 5xx range", and
//! each entry of it names one status and what to do about it:
//!
//! * `ErrorCode` — "the HTTP status code for which you want to specify a custom error page
//!   and/or a caching duration": the status the entry covers, matched against the status the
//!   answer carries;
//! * `ResponsePagePath` — "the path to the custom error page that you want CloudFront to return
//!   to a viewer when your origin returns the HTTP status code specified by `ErrorCode`", which
//!   is a VIEWER path of this same distribution, so the page is fetched through the cache
//!   behaviour whose `PathPattern` matches it and from that behaviour's own origin ("your
//!   distribution must include a cache behavior for which … the value of `PathPattern` matches
//!   the path to your custom error messages");
//! * `ResponseCode` — "the HTTP status code that you want CloudFront to return to the viewer
//!   along with the custom error page", which replaces the status the answer carried. The two
//!   are stated together — "if you specify a value for `ResponsePagePath`, you must also
//!   specify a value for `ResponseCode`" — so an entry carrying neither replaces nothing and
//!   states only the caching duration;
//! * `ErrorCachingMinTTL` — "the minimum amount of time, in seconds, that you want CloudFront
//!   to cache the HTTP status code specified in `ErrorCode`. When this time period has elapsed,
//!   CloudFront queries your origin to see whether the problem that caused the error has been
//!   resolved", which is how long the edge holds the ERROR rather than the behaviour's own TTL:
//!   a status no cache policy would hold at all ([`super::is_cacheable_status`]) is held for
//!   this span once the configuration names it ([`caching_ttl`]).
//!
//! The replacement is applied to the answer the edge reached ON AN ORIGIN'S BEHALF — the one
//! the origin produced, or the same answer out of the edge cache — because the status it is
//! keyed on is the origin's. A page CloudFront generated itself for a request it never passed
//! to an origin (a viewer its geographic restriction refuses, a signature that did not hold, a
//! method the behaviour does not allow, an origin it could not reach) is CloudFront's own
//! refusal of that viewer and is served as it is.
//!
//! One level only: the page fetch is planned and made like any other request of this
//! distribution, and whatever IT answers is not run through this module again, so a
//! configuration whose error page is itself missing cannot make one viewer request fetch
//! without end. A page the distribution cannot serve leaves the answer the origin gave,
//! which is the error the viewer would have received anyway.

use ::aws_smithy_runtime_api::http::{Request as RtRequest, Response as RtResponse};
use ::aws_smithy_types::body::SdkBody;
use ::smithy_server_runtime::OperationContext;
use ::std::time::{Duration, Instant, SystemTime};

use super::functions;
use super::{status_code, Plan, CACHE_ERROR, CACHE_HIT, X_CACHE};
use crate::ops::types::{CustomErrorResponse, DistributionConfig};
use crate::state::DEFAULT_ERROR_CACHING_MIN_TTL;

/// The statuses a custom error response may be stated for, from `ErrorCode`'s own documentation
/// ("HTTP status codes in the 4xx and 5xx range"): an entry naming anything else covers no
/// answer the edge serves.
const ERROR_STATUSES: ::std::ops::RangeInclusive<u16> = 400..=599;

/// What the configuration states about an answer carrying one status.
#[derive(Debug)]
pub struct Configured {
    /// `ResponseCode`, when the entry states one the wire can carry.
    response_code: Option<u16>,
    /// `ResponsePagePath`, when the entry names one.
    page_path: Option<String>,
    /// `ErrorCachingMinTTL` as a span.
    caching_ttl: Duration,
}

/// The custom error responses `config` states, in the order it lists them.
pub fn of_config(config: &DistributionConfig) -> Vec<CustomErrorResponse> {
    config
        .custom_error_responses
        .as_ref()
        .and_then(|responses| responses.items.clone())
        .unwrap_or_default()
}

/// What `responses` states about an answer carrying `status`, or `None` when the configuration
/// names that status not at all.
///
/// The FIRST entry naming the status decides: `ErrorCode` is one status per entry, so a
/// configuration cannot state two things about one.
pub fn configured(responses: &[CustomErrorResponse], status: u16) -> Option<Configured> {
    if !ERROR_STATUSES.contains(&status) {
        return None;
    }
    let stated = responses
        .iter()
        .find(|response| u16::try_from(response.error_code).ok() == Some(status))?;
    Some(Configured {
        response_code: stated
            .response_code
            .as_deref()
            .and_then(|code| code.trim().parse::<u16>().ok()),
        page_path: stated
            .response_page_path
            .clone()
            .filter(|path| !path.trim().is_empty()),
        caching_ttl: Duration::from_secs(
            u64::try_from(
                stated
                    .error_caching_min_ttl
                    .unwrap_or(DEFAULT_ERROR_CACHING_MIN_TTL),
            )
            .unwrap_or_default(),
        ),
    })
}

/// How long the edge holds an answer carrying `status`, when the configuration names that
/// status, or `None` when it names it not at all.
///
/// This is what makes the error caching duration the configuration's rather than the cache
/// policy's: "the minimum amount of time … that you want CloudFront to cache the HTTP status
/// code specified in `ErrorCode`".
pub fn caching_ttl(responses: &[CustomErrorResponse], status: u16) -> Option<Duration> {
    configured(responses, status).map(|configured| configured.caching_ttl)
}

/// What the edge still needs about the request it is answering in order to fetch the page the
/// configuration names: the request itself, the identifier of the request being served, the
/// viewer path it resolved to, and the clock readings every plan and fetch is made under.
#[derive(Debug)]
pub struct Serving<'a> {
    pub req: &'a RtRequest,
    pub cf_id: &'a str,
    pub viewer_path: &'a str,
    pub now: SystemTime,
    pub received: Instant,
    /// The staging distribution a continuous deployment policy routed this request to, when one
    /// did: the page is served by the SAME distribution the answer came from.
    pub staging: Option<&'a str>,
}

/// Replace `response` with the custom error response the configuration states for the status it
/// carries.
///
/// The status becomes `ResponseCode` and the bytes become the page `ResponsePagePath` names,
/// fetched through this distribution's own behaviours. An entry that names no page (or whose
/// page this distribution cannot serve) leaves the answer exactly as it is — it still states
/// how long the error is held, which [`caching_ttl`] answers where the answer is cached.
pub async fn apply<Eng: Send + Sync + 'static>(
    engine: &Eng,
    ctx: &OperationContext<'_>,
    serving: &Serving<'_>,
    configured: &Configured,
    response: &mut RtResponse,
) {
    let (Some(response_code), Some(page_path)) =
        (configured.response_code, configured.page_path.as_deref())
    else {
        return;
    };
    let Some(page) = page(engine, ctx, serving, page_path).await else {
        ::tracing::warn!(
            page = %page_path,
            "this distribution's custom error response names a page it does not serve, so the \
             viewer receives the answer the origin gave"
        );
        return;
    };
    // The page itself has to BE a page: an answer that is an error too is no custom error
    // message, so the viewer receives the error its own request produced rather than a second
    // one about the page.
    if ERROR_STATUSES.contains(&page.status().as_u16()) {
        ::tracing::warn!(
            page = %page_path,
            status = page.status().as_u16(),
            "the page this distribution's custom error response names answered an error itself, \
             so the viewer receives the answer the origin gave"
        );
        return;
    }
    let body = page
        .body()
        .bytes()
        .map(<[u8]>::to_vec)
        .unwrap_or_default();
    let mut replaced = RtResponse::new(status_code(response_code), SdkBody::from(body));
    for (name, value) in page.headers().iter() {
        let _ = replaced
            .headers_mut()
            .try_append(name.to_string(), value.to_string());
    }
    // CloudFront produced this answer — the page is the configuration's, under a status the
    // configuration chose — so the outcome names CloudFront rather than the cache or the origin
    // the page's bytes came from.
    let _ = replaced
        .headers_mut()
        .try_insert(X_CACHE, CACHE_ERROR.to_string());
    *response = replaced;
}

/// The answer this distribution serves for the viewer path `page_path`, or `None` when it
/// serves none.
///
/// Planned and fetched exactly as the request it is the error page FOR was: the behaviour whose
/// path pattern matches the page's path decides its origin, its cache and everything CloudFront
/// enforces in front of them, and a page the cache already holds is answered from there. Which
/// is also what keeps the fetch bounded: the page is cached under its own cache key, so a
/// distribution answering many errors fetches its page once per TTL.
async fn page<Eng: Send + Sync + 'static>(
    engine: &Eng,
    ctx: &OperationContext<'_>,
    serving: &Serving<'_>,
    page_path: &str,
) -> Option<RtResponse> {
    let state = crate::state::service_state(engine)?;
    // The same request, for the page's path and no query string of the viewer's: the page is one
    // object of this distribution, so what is asked for is that object.
    let request = super::rewritten_request(
        serving.req,
        serving.viewer_path,
        &functions::Request {
            uri: page_path.to_string(),
            query: String::new(),
            headers: serving
                .req
                .headers()
                .iter()
                .map(|(name, value)| (name.to_string(), value.to_string()))
                .collect(),
        },
    )?;
    // Planned under the store's lock, which is released before the fetch is made.
    let plan = {
        let store = state.lock();
        let (_, plan) = super::plan_request(
            &store,
            &request,
            serving.cf_id,
            serving.now,
            serving.staging,
        );
        plan
    };
    match plan {
        Plan::Hit { answer, encode } => Some(super::served_origin_answer(
            &answer,
            encode,
            CACHE_HIT,
            serving.cf_id,
        )),
        Plan::Fetch(fetch) => Some(
            super::fetch_origin(
                engine,
                ctx,
                *fetch,
                serving.cf_id,
                &request,
                serving.received,
            )
            .await
            .response,
        ),
        // The page's own behaviour refuses the fetch, or there is no origin to make it to:
        // nothing of this distribution answers that path.
        Plan::Generated { .. } | Plan::Refused { .. } | Plan::Redirect { .. } => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ops::types::CustomErrorResponses;

    /// One configuration's entries, as a stored configuration carries them.
    fn responses(items: Vec<CustomErrorResponse>) -> DistributionConfig {
        DistributionConfig {
            custom_error_responses: Some(CustomErrorResponses {
                quantity: i32::try_from(items.len()).unwrap_or_default(),
                items: Some(items),
            }),
            ..DistributionConfig::default()
        }
    }

    /// One entry.
    fn entry(
        error_code: i32,
        page: Option<&str>,
        code: Option<&str>,
        ttl: Option<i64>,
    ) -> CustomErrorResponse {
        CustomErrorResponse {
            error_code,
            response_page_path: page.map(ToString::to_string),
            response_code: code.map(ToString::to_string),
            error_caching_min_ttl: ttl,
        }
    }

    /// WHICH answers a configuration covers, and what it states about each: the status it
    /// names, the status it does not, a status outside the range an entry may name at all, and
    /// an entry that states only a caching duration.
    #[test]
    fn a_configuration_states_what_it_names_and_nothing_else() {
        let config = responses(vec![
            entry(403, Some("/error.html"), Some("200"), Some(10)),
            entry(404, Some("/missing.html"), Some("404"), None),
            entry(503, None, None, Some(7)),
            entry(200, Some("/never.html"), Some("200"), Some(1)),
        ]);
        let stated = of_config(&config);

        let covered = configured(&stated, 403).expect("the configuration names 403");
        assert_eq!(
            (
                covered.response_code,
                covered.page_path.as_deref(),
                covered.caching_ttl
            ),
            (Some(200), Some("/error.html"), Duration::from_secs(10)),
            "expected the entry's own members, got {covered:?}"
        );

        let defaulted = configured(&stated, 404).expect("the configuration names 404");
        assert_eq!(
            defaulted.caching_ttl,
            Duration::from_secs(
                u64::try_from(DEFAULT_ERROR_CACHING_MIN_TTL).expect("the default is a span")
            ),
            "expected the service-supplied caching duration, got {:?}",
            defaulted.caching_ttl
        );

        let caching_only = configured(&stated, 503).expect("the configuration names 503");
        assert_eq!(
            (
                caching_only.response_code,
                caching_only.page_path.as_deref(),
                caching_only.caching_ttl
            ),
            (None, None, Duration::from_secs(7)),
            "expected an entry that replaces nothing, got {caching_only:?}"
        );

        for status in [200u16, 304, 399, 600] {
            assert!(
                configured(&stated, status).is_none(),
                "expected no custom error response for {status}, got one"
            );
        }
        assert!(
            configured(&stated, 500).is_none(),
            "expected no custom error response for a status the configuration does not name"
        );
        assert_eq!(
            caching_ttl(&stated, 403),
            Some(Duration::from_secs(10)),
            "expected the caching duration of the entry naming 403, got {:?}",
            caching_ttl(&stated, 403)
        );
        assert_eq!(
            caching_ttl(&stated, 500),
            None,
            "expected no caching duration for a status the configuration does not name"
        );
    }

    /// A configuration that names nothing covers nothing.
    #[test]
    fn a_configuration_naming_nothing_covers_nothing() {
        let stated = of_config(&DistributionConfig::default());
        assert!(
            stated.is_empty() && configured(&stated, 404).is_none(),
            "expected a configuration with no custom error responses to cover no answer"
        );
    }
}
