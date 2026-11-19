// SPDX-License-Identifier: CC0-1.0

//! A custom policy whose `Resource` is a PATTERN, carried in SIGNED COOKIES, in front of a
//! private Amazon S3 origin — end to end over HTTP with Amazon S3 mounted beside CloudFront on
//! one engine.
//!
//! RULE: a custom policy's `Resource` names the URLs the signature covers and may carry the
//! `*` and `?` wildcards, so one signature admits EVERY viewer request whose URL the pattern
//! matches and no other — the same decision whichever carrier it travels in (the `CloudFront-`
//! prefixed signed cookies, or the query string of a signed URL) — and a request the pattern
//! does not cover, or one that carries no signature at all, is CloudFront's own `403`
//! (`AccessDenied`, and `MissingKey` when nothing was presented) carrying none of the object's
//! bytes.
//!
//! What this suite adds to the signed-access suites beside it: [`crate::edge_signed_access`]
//! decides one URL at a time against an origin that needs no credential and
//! [`crate::edge_signed_s3_origin`] signs each private object for ITSELF, so neither says that
//! ONE signature over a prefix pattern admits a whole set of objects while still refusing what
//! lies outside that prefix — which is the shape a client uses signed cookies for in the first
//! place (a session that may read everything under one prefix).
//!
//! The rows span what the rule generalises over: the form of the pattern (a trailing wildcard
//! over a prefix, a single-character wildcard, a resource named exactly, a wildcard over every
//! path), the object the viewer asks for under it (a flat key, a sibling key, a deeply nested
//! key, and a key outside the protected prefix), the carrier the signature travels in, and
//! whether the policy's `DateLessThan` still holds.
//!
//! The harness is [`crate::edge_signed_s3_origin`]'s two planes over one engine and the signer
//! is [`crate::edge_signed_access`]'s, so there is one implementation of CloudFront's signing
//! procedure in this crate and one staging harness for a private S3 origin.

use crate::distribution_lifecycle::awaited_deployment;
use crate::edge_signed_access::{
    create_key_group, custom_policy, custom_query, far_future, far_past, upload_public_key,
    url_safe_base64, view, Signer, Viewed, ERROR, MISS,
};
use crate::edge_signed_s3_origin::{
    create_distribution, create_origin_access_control, grant, Emulator, MISSING_KEY,
};

/// CloudFront's own code for a request whose policy does not cover it, or whose conditions no
/// longer hold — the refusal that is the edge's, not the origin's.
const ACCESS_DENIED: &str = "AccessDenied";

/// The private bucket this suite's distribution fronts.
const BUCKET: &str = "signed-cookie-prefix-origin";

/// The prefix the policy protects, and the one object outside it: every object is private in
/// the bucket, so what separates them is only the `Resource` the policy names.
const OBJECTS: [(&str, &str); 4] = [
    ("private/a.txt", "cookie-protected-object-a"),
    ("private/b.txt", "cookie-protected-object-b"),
    ("private/deep/nested/c.txt", "cookie-protected-object-c"),
    ("outside/d.txt", "object-outside-the-protected-prefix"),
];

/// One `Resource` pattern, and the objects of [`OBJECTS`] it covers.
///
/// The wildcards are the ones a custom policy's `Resource` documents: `*` stands for any run of
/// characters (it is not bounded by a path separator) and `?` for exactly one.
struct Pattern {
    case: &'static str,
    /// The path part of the `Resource`, appended to `https://<DomainName>`.
    path: &'static str,
    /// The keys this pattern admits; every other key of [`OBJECTS`] it refuses.
    admits: &'static [&'static str],
}

/// The table of patterns.
const PATTERNS: [Pattern; 4] = [
    Pattern {
        case: "a trailing wildcard over the protected prefix",
        path: "/private/*",
        admits: &[
            "private/a.txt",
            "private/b.txt",
            "private/deep/nested/c.txt",
        ],
    },
    Pattern {
        case: "a single-character wildcard inside the prefix",
        path: "/private/?.txt",
        admits: &["private/a.txt", "private/b.txt"],
    },
    Pattern {
        case: "one object named exactly",
        path: "/private/a.txt",
        admits: &["private/a.txt"],
    },
    Pattern {
        case: "a wildcard over every path",
        path: "/*",
        admits: &[
            "private/a.txt",
            "private/b.txt",
            "private/deep/nested/c.txt",
            "outside/d.txt",
        ],
    },
];

/// Where one row's signature travels.
#[derive(Debug, Clone, Copy)]
enum Carrier {
    /// The `CloudFront-`-prefixed signed cookies, which is what a custom policy is usually
    /// carried in: one set of cookies for a whole prefix.
    Cookies,
    /// The query string of a signed URL, carrying the same policy — the same decision.
    Query,
}

impl Carrier {
    fn label(self) -> &'static str {
        match self {
            Carrier::Cookies => "signed cookies",
            Carrier::Query => "a signed URL",
        }
    }
}

/// One viewer request for `key`, presenting `policy` signed as `signature` under
/// `key_pair_id`, in `carrier`.
async fn present(
    emulator: &Emulator,
    domain: &str,
    key: &str,
    carrier: Carrier,
    policy: &str,
    signature: &str,
    key_pair_id: &str,
) -> Viewed {
    match carrier {
        Carrier::Cookies => {
            let cookies = format!(
                "CloudFront-Policy={}; CloudFront-Signature={signature}; \
                 CloudFront-Key-Pair-Id={key_pair_id}",
                url_safe_base64(policy.as_bytes())
            );
            view(
                &emulator.viewers,
                domain,
                &format!("/{key}"),
                &[("cookie", &cookies)],
            )
            .await
        }
        Carrier::Query => {
            let query = custom_query(policy, signature, key_pair_id);
            view(
                &emulator.viewers,
                domain,
                &format!("/{key}?{query}"),
                &[],
            )
            .await
        }
    }
}

/// Assert that `answer` is the origin's `200` carrying exactly `body`.
fn assert_served(answer: &Viewed, case: &str, body: &str) {
    assert_eq!(
        (answer.status, answer.x_cache.as_str()),
        (200, MISS),
        "case {case}: expected the origin's 200 with X-Cache {MISS:?}, got {} with {:?} (body \
         {:?})",
        answer.status,
        answer.x_cache,
        answer.body
    );
    assert_eq!(
        answer.body, body,
        "case {case}: expected the stored object's own bytes, got {:?}",
        answer.body
    );
}

/// Assert that `answer` is CloudFront's own `403` under `code`, carrying none of `body`.
fn assert_refused(answer: &Viewed, case: &str, code: &str, body: &str) {
    assert_eq!(
        (answer.status, answer.x_cache.as_str()),
        (403, ERROR),
        "case {case}: expected CloudFront's own 403 with X-Cache {ERROR:?}, got {} with {:?} \
         (body {:?})",
        answer.status,
        answer.x_cache,
        answer.body
    );
    assert!(
        answer.carries_code(code),
        "case {case}: expected the error code {code:?}, got {:?}",
        answer.body
    );
    assert!(
        !answer.body.contains(body),
        "case {case}: a refusal must carry none of the object's bytes, got {:?}",
        answer.body
    );
}

/// The whole rule over the table: one signature over a pattern admits every object the pattern
/// covers, in either carrier, and refuses everything else — and a viewer presenting nothing, or
/// a policy whose expiry has passed, is refused with no object bytes at all.
#[tokio::test]
async fn one_signed_policy_admits_every_object_its_resource_pattern_covers() {
    let emulator = Emulator::new();
    let signer = Signer::generate();
    let case = "a wildcard policy over a private prefix";
    let key_pair_id = upload_public_key(&emulator.setup, "signed-cookie-prefix", &signer).await;
    let (group, _etag) =
        create_key_group(&emulator.setup, "signed-cookie-prefix", &[&key_pair_id]).await;

    for (key, body) in OBJECTS {
        emulator.staged_object(case, BUCKET, key, body).await;
    }
    let control =
        create_origin_access_control(&emulator.setup, case, "signed-cookie-prefix").await;
    let (id, domain, arn) = create_distribution(
        &emulator.setup,
        case,
        "signed-cookie-prefix-reference",
        BUCKET,
        &control,
        &group,
    )
    .await;
    // The grant is scoped to the distribution's own ARN, so it is written once the
    // distribution exists — the order a client has to follow.
    emulator
        .staged_policy(case, BUCKET, &grant(BUCKET, &arn))
        .await;
    // A configuration that has not reached the edge locations serves no viewer, and the CLOCK
    // owes that propagation: the condition is polled for, never a span waited out.
    awaited_deployment(&emulator.setup, case, &id).await;

    for pattern in PATTERNS {
        let resource = format!("https://{domain}{}", pattern.path);
        let policy = custom_policy(&resource, far_future(), None, None);
        let signature = signer.sign(&policy);
        for (key, body) in OBJECTS {
            for carrier in [Carrier::Cookies, Carrier::Query] {
                let case = format!(
                    "{} asking for {key} through {}",
                    pattern.case,
                    carrier.label()
                );
                let answer = present(
                    &emulator,
                    &domain,
                    key,
                    carrier,
                    &policy,
                    &signature,
                    &key_pair_id,
                )
                .await;
                if pattern.admits.contains(&key) {
                    assert_served(&answer, &case, body);
                } else {
                    // The pattern does not cover this URL, so the signature says nothing about
                    // it: CloudFront refuses it itself and the origin is never asked.
                    assert_refused(&answer, &case, ACCESS_DENIED, body);
                }
            }
        }
    }

    // A viewer that presents nothing at all cannot be told apart from any other, whatever the
    // prefix: the behaviour requires a signature, so there is no key to judge by.
    for (key, body) in OBJECTS {
        let answer = view(&emulator.viewers, &domain, &format!("/{key}"), &[]).await;
        assert_refused(
            &answer,
            &format!("no signature at all asking for {key}"),
            MISSING_KEY,
            body,
        );
    }

    // The same wildcard policy once its `DateLessThan` has passed: a condition that no longer
    // holds refuses every object the pattern covers, in either carrier.
    let expired = custom_policy(&format!("https://{domain}/private/*"), far_past(), None, None);
    let expired_signature = signer.sign(&expired);
    for (key, body) in OBJECTS {
        for carrier in [Carrier::Cookies, Carrier::Query] {
            let answer = present(
                &emulator,
                &domain,
                key,
                carrier,
                &expired,
                &expired_signature,
                &key_pair_id,
            )
            .await;
            assert_refused(
                &answer,
                &format!(
                    "a policy whose expiry has passed asking for {key} through {}",
                    carrier.label()
                ),
                ACCESS_DENIED,
                body,
            );
        }
    }
}
