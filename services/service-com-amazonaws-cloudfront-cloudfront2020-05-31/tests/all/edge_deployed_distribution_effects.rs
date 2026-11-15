// SPDX-License-Identifier: CC0-1.0

//! What a DEPLOYED distribution's edge domain answers when every per-behaviour group its
//! configuration can carry is in force AT ONCE — end to end over HTTP, with Amazon S3
//! mounted beside CloudFront on one engine so the origin is a real object store and the
//! bytes a viewer gets are an origin's own.
//!
//! RULE: once a distribution reports `Deployed`, a viewer request addressed to its
//! `DomainName` is served by the edge from the matched behaviour's origin, and that
//! behaviour's own configuration decides the answer.
//!
//! * The CACHE POLICY's cache key is what separates one cached object from another, so an
//!   answer is reused only for a request the key says is the same object, and every distinct
//!   key keeps an entry of its own: two requests differing only in a parameter the policy
//!   does not name share one entry, one differing in a named value gets another, and the
//!   entry made first is still held when a request for it comes back. Nothing the policy
//!   does not name separates two requests — the parameters that carry a signature included.
//! * The RESPONSE HEADERS POLICY states the headers of whatever the edge serves — the
//!   origin's answer, the same answer out of the edge cache, and a page CloudFront generated
//!   itself — and each header's `Override` decides it against a value the answer already
//!   carries: `true` replaces the origin's own value of that header, `false` leaves it
//!   alone. Its strict-transport-security group is emitted as
//!   `max-age=<AccessControlMaxAgeSec>`.
//! * `TrustedKeyGroups` makes the behaviour signature-gated: a URL signed by a key one of
//!   those groups holds is served the object's own bytes, and a request carrying no
//!   signature at all is refused by CloudFront itself with `403` and the error code
//!   `MissingKey` — decided before the edge cache is read, so an entry a signed request made
//!   is never handed to an unsigned one.
//!
//! The rows span what the rule generalises over: which parameter two successive requests
//! differ in (one the cache policy names, one it does not, the named one absent altogether),
//! the position in the sequence (a key made, a key reused, an earlier key returned to), both
//! settings of a custom header's `Override` against a value the ORIGIN itself states, the
//! `AccessControlMaxAgeSec` the policy carries, and the object's own bytes.
//!
//! Every harness here is a sibling suite's, reused rather than copied: the two services on
//! one engine, the staged object and the viewer are
//! [`crate::edge_origin_response_headers`]'s, the cache policy document
//! [`crate::edge_request_shaping`]'s, the signer and the signed query
//! [`crate::edge_signed_access`]'s, and the response headers policy document
//! [`crate::response_headers_policy_lifecycle`]'s.

use crate::distribution_lifecycle::awaited_deployment;
use crate::edge_origin_response_headers::{
    create_distribution_over_bucket, create_origin_access_control, view, Emulator, Staged, Viewed,
    HIT, MISS,
};
use crate::edge_request_shaping::cache_policy;
use crate::edge_signed_access::{
    create_key_group, custom_policy, custom_query, far_future, upload_public_key, Signer, ERROR,
};
use crate::response_headers_policy_lifecycle::{create_policy, Hsts, Policy, Security};

/// The object every row reads, in each deployment's own bucket.
const OBJECT_KEY: &str = "index.html";

/// The query string the cache policy NAMES, so a different value of it is a different object.
const KEYED: &str = "v";

/// A query string the cache policy does not name, so a different value of it is the SAME
/// object.
const UNKEYED: &str = "junk";

/// How long the cache policy holds an answer: long enough that every request of a row's
/// sequence is decided by the cache key rather than by an entry that expired meanwhile.
const CACHED_SECONDS: i64 = 60;

/// The value the ORIGIN states for the two pieces of user metadata the object carries, which
/// is what the policy's `Override` is decided against.
const FROM_ORIGIN: &str = "from-origin";

/// The header the policy ADDS, which the origin states nothing for.
const ADDED: &str = "x-fidelity";

/// The header the policy restates with `Override` true: the policy's value wins over the
/// origin's.
const OVERRIDDEN: &str = "x-amz-meta-conflict";

/// The header the policy restates with `Override` false: the origin's value stands.
const YIELDED: &str = "x-amz-meta-passthrough";

/// The error code CloudFront's own page carries for a request to a signature-gated behaviour
/// that names no key at all.
const MISSING_KEY: &str = "MissingKey";

// ---------------------------------------------------------------------------
// The deployments
// ---------------------------------------------------------------------------

/// One deployment: a bucket holding one object, and the three policies in front of it.
#[derive(Debug)]
struct Deployment {
    case: &'static str,
    bucket: &'static str,
    /// The object's bytes, so a row's answer is this deployment's object and no other's.
    body: &'static str,
    /// `AccessControlMaxAgeSec` of the policy's strict-transport-security group.
    max_age: i32,
    /// The value the policy states for each custom header it carries.
    edge_value: &'static str,
}

/// The deployments. `AccessControlMaxAgeSec` is a span the policy carries and the header
/// restates, so the rows carry two different ones, and the policy's own header values and the
/// object's bytes differ between them as well: no assertion below can pass on a constant.
fn deployments() -> Vec<Deployment> {
    vec![
        Deployment {
            case: "a year of strict transport security",
            bucket: "edge-effects-year",
            body: "<p>the first origin's own bytes</p>",
            max_age: 31_536_000,
            edge_value: "edge",
        },
        Deployment {
            case: "ten minutes of it, and other header values",
            bucket: "edge-effects-minutes",
            body: "<p>another origin, other bytes</p>",
            max_age: 600,
            edge_value: "stated-at-the-edge",
        },
    ]
}

/// The object a deployment stages: `body` under two pieces of user metadata of its own, which
/// is what makes the policy's `Override` decidable against a value the ORIGIN really sent.
fn object(body: &'static str) -> Staged {
    Staged {
        key: OBJECT_KEY,
        body,
        content_type: Some("text/html"),
        cache_control: None,
        content_disposition: None,
        metadata: &[("conflict", FROM_ORIGIN), ("passthrough", FROM_ORIGIN)],
    }
}

/// Create the response headers policy of `deployment` and answer its minted id: one header
/// the origin states nothing for, the same header the origin states with `Override` true, it
/// again with `Override` false, and the strict-transport-security group.
async fn response_headers_policy(emulator: &Emulator, index: usize, deployment: &Deployment) -> String {
    let policy = Policy {
        case: deployment.case,
        name: format!("edge-effects-{index}"),
        security: Some(Security {
            hsts: Some(Hsts {
                r#override: true,
                include_subdomains: None,
                preload: None,
                max_age: deployment.max_age,
            }),
            ..Security::default()
        }),
        custom_headers: Some(vec![
            (ADDED.to_string(), deployment.edge_value.to_string(), true),
            (OVERRIDDEN.to_string(), deployment.edge_value.to_string(), true),
            (YIELDED.to_string(), deployment.edge_value.to_string(), false),
        ]),
        ..Policy::default()
    };
    let created = create_policy(&emulator.router, &policy.xml()).await;
    assert_eq!(
        created.status, 201,
        "case {:?}: CreateResponseHeadersPolicy answered {}: {}",
        deployment.case, created.status, created.raw
    );
    created
        .root()
        .find_text("Id")
        .unwrap_or_else(|| panic!("case {:?}: the created policy reports no id", deployment.case))
        .to_string()
}

/// What one deployment left behind for the viewer phase.
#[derive(Debug)]
struct Prepared {
    case: &'static str,
    body: &'static str,
    max_age: i32,
    edge_value: &'static str,
    id: String,
    domain: String,
}

// ---------------------------------------------------------------------------
// Viewing
// ---------------------------------------------------------------------------

/// One VIEWER request for `query`, signed the way a client signs a CloudFront URL: a custom
/// policy naming the exact URL the viewer asks for — the three parameters that carry the
/// signature are not part of it — appended to the viewer's own query string.
async fn signed_view(
    emulator: &Emulator,
    signer: &Signer,
    key_pair_id: &str,
    domain: &str,
    query: &str,
) -> Viewed {
    let resource = format!("https://{domain}/{OBJECT_KEY}{query}");
    let policy = custom_policy(&resource, far_future(), None, None);
    let signed = custom_query(&policy, &signer.sign(&policy), key_pair_id);
    view(
        &emulator.router,
        "GET",
        domain,
        &format!("/{OBJECT_KEY}{query}&{signed}"),
        &[],
    )
    .await
}

/// The sequence every deployment is driven through: `(the step, the viewer's query string,
/// the X-Cache the answer must carry)`.
///
/// The rows differ along which parameter the request differs from the one before it in — the
/// named one, the unnamed one, the named one being absent — and along where in the sequence
/// they sit, so the table decides both that a key separates entries and that the entries
/// COEXIST: the fifth row returns to the key the first row made, two keys later.
const SEQUENCE: &[(&str, &str, &str)] = &[
    (
        "the first request for a key reaches the origin",
        "?v=1&junk=a",
        MISS,
    ),
    (
        "a request differing only in a parameter the policy does not name is the same object",
        "?v=1&junk=b",
        HIT,
    ),
    (
        "a request differing in the named value is another object",
        "?v=2&junk=a",
        MISS,
    ),
    (
        "the entry that request made serves the next request for the same key",
        "?v=2&junk=c",
        HIT,
    ),
    (
        "the entry the FIRST key made is still held beside it",
        "?v=1&junk=d",
        HIT,
    ),
    (
        "a request carrying no named parameter at all is another object again",
        "?junk=a",
        MISS,
    ),
    (
        "and the entry that one made serves the next request for it",
        "?junk=b",
        HIT,
    ),
];

/// Assert the headers the response headers policy states on one answer `row` got.
///
/// `origin_stated` says whether the answer is one the ORIGIN produced (or the cache held),
/// which is the answer that carries the origin's own value of the overridable header. On a
/// page CloudFront generated itself there is no such value, so the header whose `Override` is
/// false is written as well: an `Override` decides against a value the answer ALREADY carries,
/// and that page carries none.
fn assert_policy_headers(answer: &Viewed, case: &str, leg: &str, row: &Prepared, origin_stated: bool) {
    let expected_hsts = format!("max-age={}", row.max_age);
    let yielded = if origin_stated {
        FROM_ORIGIN
    } else {
        row.edge_value
    };
    for (header, expected) in [
        (ADDED, row.edge_value),
        (OVERRIDDEN, row.edge_value),
        (YIELDED, yielded),
        ("strict-transport-security", expected_hsts.as_str()),
    ] {
        let actual = answer.header(header);
        assert_eq!(
            actual,
            Some(expected),
            "case {case:?} ({leg}): expected {header:?} {expected:?}, got {actual:?} (every \
             header {:?})",
            answer.headers
        );
    }
}

// ---------------------------------------------------------------------------
// The rule
// ---------------------------------------------------------------------------

/// The whole rule over the table: a deployed distribution's domain serves its origin, the
/// cache key separates the entries, the response headers policy states the answer's headers,
/// and the trusted key group admits a signed request and refuses an unsigned one.
#[tokio::test]
async fn a_deployed_distributions_domain_serves_its_origin_under_every_group_its_behaviour_carries()
{
    let emulator = Emulator::new();
    let signer = Signer::generate();
    let trusted = upload_public_key(&emulator.router, "edge-effects-trusted", &signer).await;
    let (group, _etag) = create_key_group(&emulator.router, "edge-effects", &[&trusted]).await;

    let mut prepared: Vec<Prepared> = Vec::new();
    for (index, deployment) in deployments().into_iter().enumerate() {
        let case = deployment.case;
        emulator.staged_bucket(case, deployment.bucket).await;
        emulator
            .staged_object(case, deployment.bucket, &object(deployment.body))
            .await;
        let control =
            create_origin_access_control(&emulator.router, case, &format!("edge-effects-{index}"))
                .await;
        // A policy that CACHES and keys on ONE named query string, so the sequence can tell an
        // entry reused from an entry made.
        let cache = cache_policy(
            &emulator.router,
            &format!("edge-effects-{index}"),
            "whitelist",
            &[KEYED],
            CACHED_SECONDS,
            false,
            false,
        )
        .await;
        let headers = response_headers_policy(&emulator, index, &deployment).await;
        let (id, domain, arn) = create_distribution_over_bucket(
            &emulator.router,
            case,
            &format!("edge-effects-{index}"),
            deployment.bucket,
            &control,
            &format!(
                "<TrustedKeyGroups><Enabled>true</Enabled><Quantity>1</Quantity>\
                 <Items><KeyGroup>{group}</KeyGroup></Items></TrustedKeyGroups>\
                 <CachePolicyId>{cache}</CachePolicyId>\
                 <ResponseHeadersPolicyId>{headers}</ResponseHeadersPolicyId>"
            ),
        )
        .await;
        // The grant is written after the distribution exists, because it is scoped to that
        // distribution's own ARN — the order a client has to follow.
        emulator
            .granted_to_cloudfront(case, deployment.bucket, &arn)
            .await;
        prepared.push(Prepared {
            case,
            body: deployment.body,
            max_age: deployment.max_age,
            edge_value: deployment.edge_value,
            id,
            domain,
        });
    }

    for row in &prepared {
        // A configuration that has not reached the edge locations serves no viewer, and the
        // CLOCK owes that propagation: the condition is polled for, never a span waited out.
        awaited_deployment(&emulator.router, row.case, &row.id).await;

        for (step, query, expected) in SEQUENCE {
            let answer = signed_view(&emulator, &signer, &trusted, &row.domain, query).await;
            let leg = format!("{step} ({query})");
            assert_eq!(
                (answer.status, answer.x_cache()),
                (200, *expected),
                "case {:?} ({leg}): expected the object through the edge with X-Cache \
                 {expected:?}, got {} with {:?} (body {:?})",
                row.case,
                answer.status,
                answer.x_cache(),
                answer.body
            );
            assert_eq!(
                answer.body, row.body,
                "case {:?} ({leg}): expected the origin's own bytes {:?}, got {:?}",
                row.case, row.body, answer.body
            );
            assert_policy_headers(&answer, row.case, &leg, row, true);
        }

        // The same URL as the sequence's first row, with no signature: refused by CloudFront
        // itself, under the code it states for a request that names no key — and NOT answered
        // out of the entry the signed request for that very key already made, the decision
        // being taken in front of the cache.
        let unsigned = view(
            &emulator.router,
            "GET",
            &row.domain,
            &format!("/{OBJECT_KEY}?{KEYED}=1&{UNKEYED}=a"),
            &[],
        )
        .await;
        assert_eq!(
            (unsigned.status, unsigned.x_cache()),
            (403, ERROR),
            "case {:?} (unsigned): expected CloudFront's own 403 with X-Cache {ERROR:?}, got \
             {} with {:?} (body {:?})",
            row.case,
            unsigned.status,
            unsigned.x_cache(),
            unsigned.body
        );
        assert!(
            unsigned
                .body
                .contains(&format!("<Code>{MISSING_KEY}</Code>")),
            "case {:?} (unsigned): expected the error code {MISSING_KEY:?}, got {:?}",
            row.case,
            unsigned.body
        );
        assert!(
            !unsigned.body.contains(row.body),
            "case {:?} (unsigned): the refusal carries the object's bytes, so a cached entry \
             was served to it: {:?}",
            row.case,
            unsigned.body
        );
        // The policy states the headers of that page too: it is an answer the edge serves.
        assert_policy_headers(&unsigned, row.case, "unsigned", row, false);
    }
}
