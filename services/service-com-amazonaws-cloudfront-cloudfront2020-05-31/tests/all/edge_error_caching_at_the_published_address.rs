// SPDX-License-Identifier: CC0-1.0

//! ERROR CACHING observed where a client observes it: on the wire, at the address a deployed
//! distribution PUBLISHED as its `DomainName`, over a Lambda function URL origin.
//!
//! RULE, two halves of one contract:
//!
//! * `Distribution$DomainName` is "the distribution's CloudFront domain name" — the value a
//!   client DIALS — so what a distribution publishes has to be an address this emulator
//!   ANSWERS on: a `host:port` a client on this machine dials as given, served by the listener
//!   this emulator opened for that distribution, and reported identically by every read of the
//!   distribution;
//! * and a viewer that dials it is served the distribution's whole data-plane contract, error
//!   caching included: `CustomErrorResponses`' `ErrorCachingMinTTL` is "the minimum amount of
//!   time, in seconds, that you want CloudFront to cache the HTTP status code specified in
//!   `ErrorCode`", so a repeat inside that span is answered from the edge's own store and the
//!   origin is not reached again — unless the ORIGIN's own `Cache-Control` forbids a shared
//!   cache from storing its answer, in which case the error is held for no span at all and the
//!   origin is reached again.
//!
//! "The origin was not reached again" is read off the OWNING service's records — Lambda's own
//! `Invocations` series for the function behind the origin — rather than off an absence in the
//! answer, so a held error is one invocation for two viewer requests and an unheld one is two.
//!
//! The rows differ along what the rule generalises over: the status the origin answered (a
//! `5xx` gateway error, another `5xx`, a `4xx`, and content, each one a status the
//! configuration states a span for apart from the content), and what the origin said about
//! storing that answer (nothing at all, and each of the three directives that forbid a shared
//! cache from storing it). The recorded case — a plain `502` against a `no-store` `502` — is
//! two of those rows.
//!
//! The topology, the origin handler, the configuration and the dial are the siblings' own,
//! reused rather than copied: [`crate::edge_lambda_url_origin`] stages the function URL and
//! reads one answer off the connection, and [`crate::edge_origin_cache_directives`] owns the
//! handler that answers the status and the directive a viewer path names, and the distribution
//! configuration those spans are stated in. What is new here is where the rule is observed —
//! a socket this emulator opened, dialled by a client with no credential — and the record the
//! origin's own service keeps of having been reached.

use crate::distribution_lifecycle::{is_published_edge_address, API_VERSION};
use crate::edge_lambda_url_origin::{element, Answered, Edge, Topology, MISS};
use crate::edge_origin_cache_directives::{config_xml, error_caching, HANDLER_SOURCE};

/// The function behind the origin of every distribution below.
const FUNCTION: &str = "cf-edge-error-caching-published";

/// How long the configuration asks for an error to be held, and how long the behaviour holds
/// content: a span no row can outlive, so a row that observed one origin invocation for two
/// viewer requests observed a HELD answer rather than an expired one.
const HELD_SECONDS: u64 = 3_600;

/// `X-Cache` for an answer the edge served out of its own store, and for one that is an ERROR
/// however it reached it ([`MISS`], the outcome an origin-produced object carries, is the
/// sibling's).
const HIT: &str = "Hit from cloudfront";
const ERROR: &str = "Error from cloudfront";

/// The statuses among the rows' that CloudFront's own cache list carries — "CloudFront caches
/// ... responses with the following status codes: 200, 203, 204, 206, 300, 301, 302, 303, 304,
/// 307, 308, 404, 405, 410, 414, and 501" — so an answer carrying one of them reports WHICH of
/// the origin and the cache produced it, while every other error answer reports only that it is
/// an error.
const REPORTS_WHICH_PRODUCED_IT: [u16; 2] = [CONTENT, 404];

/// What `X-Cache` states for an answer carrying `status` that the edge's own store did or did
/// not produce.
fn expected_outcome(status: u16, from_the_store: bool) -> &'static str {
    if !REPORTS_WHICH_PRODUCED_IT.contains(&status) {
        return ERROR;
    }
    if from_the_store {
        HIT
    } else {
        MISS
    }
}

/// The statuses the rows drive the rule over: two gateway errors, a `4xx`, and content.
///
/// Each error is one the configuration names an `ErrorCachingMinTTL` for; `ErrorCode` is "the
/// HTTP status code for which you want to specify a custom error page and/or a caching
/// duration", documented for "HTTP status codes in the 4xx and 5xx range", so an entry may name
/// any of them.
const HELD_ERRORS: [u16; 3] = [502, 503, 404];

/// The status that is not an error at all, held by the behaviour's own TTL rather than by
/// `ErrorCachingMinTTL`.
const CONTENT: u16 = 200;

/// One row: the status the origin answers, what it says about storing that answer, and whether
/// the edge holds it.
struct Row {
    case: String,
    /// The first viewer path segment, which is what the handler reads the directive off.
    directive: &'static str,
    status: u16,
    /// Whether the immediate repeat is answered from the edge's own store, without the origin
    /// being reached again.
    held: bool,
}

/// The `DistributionConfig` every row is driven over: the sibling's own distribution over the
/// function URL `origin` — the legacy per-behaviour TTLs, a minimum TTL of no span at all, so
/// nothing but the origin's own directive and the configuration's error caching duration
/// decides what is held — with a `CustomErrorResponses` naming every status of [`HELD_ERRORS`].
fn config(caller_reference: &str, origin: &str) -> String {
    config_xml(
        caller_reference,
        origin,
        0,
        &error_caching(&HELD_ERRORS, HELD_SECONDS),
    )
}

/// The rows: every status of [`HELD_ERRORS`] and [`CONTENT`], each one answered with no
/// directive (held) and with each directive that forbids a shared cache from storing the answer
/// (not held).
///
/// The directives are the ones RFC 9111 gives a shared cache: `no-store`, `no-cache` and
/// `private`. `"none"` is no directive at all, which the handler states no `Cache-Control` for.
fn rows() -> Vec<Row> {
    let mut rows = Vec::new();
    for status in HELD_ERRORS.iter().copied().chain([CONTENT]) {
        let what = if status == CONTENT { "content" } else { "error" };
        rows.push(Row {
            case: format!("a {status} {what} the origin states nothing about"),
            directive: "none",
            status,
            held: true,
        });
        for directive in ["no-store", "no-cache", "private"] {
            rows.push(Row {
                case: format!("a {status} {what} under {directive}"),
                directive,
                status,
                held: false,
            });
        }
    }
    rows
}

/// One viewer request at the published address: dialled in the clear with no AWS credential,
/// stating the published `DomainName` as the host it addressed, which is what a client handed a
/// domain name does.
async fn viewed(case: &str, edge: &Edge, path: &str) -> Answered {
    Answered::of_stating(case, edge, "GET", path, &[]).await
}

/// RULE: at the address the distribution published, an error the configuration states an
/// `ErrorCachingMinTTL` for is held for that span — the repeat reaches no origin — while an
/// answer whose origin forbade a shared cache from storing it is not held, and the origin is
/// reached again.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn error_caching_is_observed_by_a_viewer_of_the_published_address() {
    let topology = Topology::new();
    let setup = "a distribution that states how long its origin's errors are held";
    let origin = topology
        .public_function_url_host_running(setup, FUNCTION, HANDLER_SOURCE)
        .await;
    let (id, _arn, edge) = topology
        .created_distribution(setup, &config("edge-error-caching-published", &origin))
        .await;
    topology.awaited_deployment(setup, &id).await;

    for row in rows() {
        let case = row.case.as_str();
        let path = format!("/{}/{}", row.directive, row.status);
        let before = topology.invocations(case, FUNCTION).await;
        let first = viewed(case, &edge, &path).await;
        let second = viewed(case, &edge, &path).await;
        let after = topology.invocations(case, FUNCTION).await;

        assert_eq!(
            (first.status, second.status),
            (row.status, row.status),
            "case {case:?}: expected the origin's own {} both times at {}, got {} then {} \
             ({:?}, {:?})",
            row.status,
            edge,
            first.status,
            second.status,
            first.raw,
            second.raw
        );
        let produced = expected_outcome(row.status, false);
        assert!(
            first.reports_cache(produced),
            "case {case:?}: the first request must be reported as {produced:?}, got {:?}",
            first.raw
        );

        let invoked = after - before;
        if row.held {
            assert_eq!(
                first.body, second.body,
                "case {case:?}: expected the repeat to be served the answer the edge already \
                 holds, got {:?} then {:?}",
                first.body, second.body
            );
            let held = expected_outcome(row.status, true);
            assert!(
                second.reports_cache(held),
                "case {case:?}: expected the repeat to be reported as {held:?}, got {:?}",
                second.raw
            );
            assert_eq!(
                invoked, 1.0,
                "case {case:?}: expected the two viewer requests to reach the origin once, \
                 Lambda counted {invoked} invocations ({before} then {after})"
            );
            continue;
        }
        assert_ne!(
            first.body, second.body,
            "case {case:?}: expected the repeat to reach the origin again, got the same answer \
             twice ({:?})",
            first.body
        );
        // WHICH outcome CloudFront words a repeat that reached the origin again by depends on
        // how it got there (a fetch, a revalidation), so what this row is held to is that the
        // origin WAS reached — the two answers above and Lambda's own count of them.
        assert_eq!(
            invoked, 2.0,
            "case {case:?}: expected each of the two viewer requests to reach the origin, \
             Lambda counted {invoked} invocations ({before} then {after})"
        );
    }
}

/// RULE: what a distribution publishes as its `DomainName` is an address this emulator answers
/// on — the `host:port` of the listener it opened for that distribution, carrying no URI path —
/// it is the SAME value every read of the distribution reports, and a viewer that dials it is
/// served the origin the matched behaviour targets.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_published_domain_name_is_an_address_this_emulator_answers_on() {
    let topology = Topology::new();
    let setup = "a deployed distribution over a function URL origin";
    let origin = topology
        .public_function_url_host_running(setup, FUNCTION, HANDLER_SOURCE)
        .await;
    let (id, _arn, edge) = topology
        .created_distribution(
            setup,
            &config("edge-error-caching-published-address", &origin),
        )
        .await;
    topology.awaited_deployment(setup, &id).await;

    // The create's own answer: a host carrying the port it answers on, and nothing else.
    assert!(
        is_published_edge_address(&edge.domain),
        "setup {setup:?}: expected an address a client on this machine dials as given, got {:?}",
        edge.domain
    );
    // And it is the address this emulator OPENED for this distribution, rather than a name
    // beside it: one address, published and served.
    let dialled = edge.address.clone().unwrap_or_else(|| {
        panic!("setup {setup:?}: this distribution was given no address of its own to dial")
    });
    assert_eq!(
        edge.domain, dialled,
        "setup {setup:?}: expected the published name to be the address the edge is served on, \
         got {:?} published and {dialled:?} opened",
        edge.domain
    );

    // Every read of the distribution reports that one value.
    for (reader, uri) in [
        ("GetDistribution", format!("/{API_VERSION}/distribution/{id}")),
        ("ListDistributions", format!("/{API_VERSION}/distribution")),
    ] {
        let (status, raw) = topology.cloudfront(setup, "GET", &uri, None).await;
        assert_eq!(
            status, 200,
            "setup {setup:?}: {reader} must answer 200, got {status} with {raw}"
        );
        let reported = element(&raw, "DomainName");
        assert_eq!(
            reported, edge.domain,
            "case {reader:?}: expected the published address {:?}, got {reported:?}",
            edge.domain
        );
    }

    // A viewer dials it with no credential and is served the origin the behaviour targets.
    let case = "a viewer of the published address";
    let answered = viewed(case, &edge, &format!("/none/{CONTENT}")).await;
    assert_eq!(
        answered.status, CONTENT,
        "case {case:?}: expected the origin's answer through the edge at {}, got {} with {:?}",
        edge, answered.status, answered.raw
    );
    assert!(
        answered.reports_cache(MISS),
        "case {case:?}: expected an answer the origin produced for this request ({MISS:?}), got \
         {:?}",
        answered.raw
    );
}
