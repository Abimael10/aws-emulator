// SPDX-License-Identifier: CC0-1.0

//! The CUSTOM ERROR PAGE a distribution serves in place of an ORIGIN error, over a PRIVATE
//! Amazon S3 origin fronted by an ORIGIN ACCESS CONTROL — the single-page-application shape,
//! end to end over HTTP with Amazon S3 mounted beside CloudFront on one engine.
//!
//! RULE: `DistributionConfig$CustomErrorResponses` "controls whether CloudFront replaces HTTP
//! status codes in the 4xx and 5xx range with custom error messages before returning the
//! response to the viewer", and each entry states that for ONE status: `ErrorCode` is "the HTTP
//! status code for which you want to specify a custom error page", `ResponsePagePath` is "the
//! path to the custom error page that you want CloudFront to return to a viewer when your origin
//! returns the HTTP status code specified by `ErrorCode`", and `ResponseCode` is "the HTTP status
//! code that you want CloudFront to return to the viewer along with the custom error page" ("you
//! might want to return a `200` status code (OK) and static website so your customers don't know
//! that your website is down"). So whenever the answer the edge reached on the origin's behalf
//! carries a status an entry names, the viewer receives that entry's `ResponseCode` with the
//! bytes of the object its `ResponsePagePath` names — whatever the viewer asked for — and a
//! status no entry names is served exactly as the origin gave it.
//!
//! What this suite adds to the crate's `edge_custom_error_responses` suite, which states the
//! same rule over an origin that grants everyone: the page itself lives in a bucket that grants
//! NOTHING publicly and is read only through the origin access control's signature, which is the
//! configuration a single-page application behind CloudFront actually runs. That makes two
//! things observable that a public origin cannot state. The origin's error for a key it does not
//! hold is `403` rather than `404` — "if you don't have the `s3:ListBucket` permission, Amazon S3
//! returns an HTTP status code 403 Access Denied error" — so the entry that covers it is the
//! `403` one; and the page substituted for it has to be fetched over the SAME signed origin
//! access, so a viewer receives the app shell instead of the origin's own `AccessDenied`
//! document.
//!
//! The rows span what the rule generalises over: the depth of the path the viewer asked for,
//! whether it carries a file extension at all, which page an entry names, which `ResponseCode`
//! it replaces the status with, and whether the configuration names the status the origin
//! answered at all. The status the origin answers for a key it does not hold is READ off a
//! control distribution that states no custom error response, so no row here fixes what that
//! status is.

use smithy_server_runtime::MultiServiceRouter;

use crate::distribution_lifecycle::{awaited_deployment, call, API_VERSION};
use crate::edge_custom_error_responses::{entry, FORBIDDEN, NOT_FOUND};
use crate::edge_signed_access::{view, Viewed, CACHING_DISABLED, ERROR, MISS};
use crate::edge_signed_s3_origin::{
    create_origin_access_control, grant, origin_domain, Emulator, ACCESS_DENIED,
    ORIGIN_ENVELOPE_MEMBER,
};

/// The origin's id inside every distribution below.
const ORIGIN_ID: &str = "origin-bucket";

/// The app shell: the object `DefaultRootObject` names and the object the custom error
/// responses point at, which is what makes the configuration a single-page application's.
const SHELL_KEY: &str = "index.html";
const SHELL_BODY: &str = "<html><body><div id=\"app\"></div></body></html>";

/// An asset the bucket really holds, so a row can state that an answer the configuration covers
/// no status of is served as the origin gave it.
const ASSET_KEY: &str = "app.js";
const ASSET_BODY: &str = "console.log('the application bundle');";

/// A second page, so one row can name a page and a replacement status that are not the shell's.
const MAINTENANCE_KEY: &str = "errors/maintenance.html";
const MAINTENANCE_BODY: &str = "<html>the branded maintenance page</html>";

/// The `ResponseCode` the shell is served under: the substitution `ResponseCode` documents for
/// exactly this configuration.
const SHELL_RESPONSE_CODE: u16 = 200;

/// The `ResponseCode` the second page is served under, so the two configured rows are told apart
/// by the status as well as by the bytes.
const MAINTENANCE_RESPONSE_CODE: u16 = 410;

/// `ErrorCachingMinTTL` for an error the edge holds for no time at all, so every request earns
/// the substitution again rather than being answered out of one stored error.
const NOT_HELD: u64 = 0;

/// The viewer paths the origin holds nothing at, spanning the depth of the path and whether it
/// carries a file extension at all.
const ABSENT_PATHS: &[(&str, &str)] = &[
    ("a missing asset under a prefix", "/assets/missing.js"),
    ("a missing stylesheet several segments deep", "/static/css/v2/theme.css"),
    ("an extensionless deep link", "/dashboard"),
    ("an extensionless deep link under a prefix", "/settings/profile"),
    ("a missing document at the root", "/missing.html"),
];

// ---------------------------------------------------------------------------
// Staging
// ---------------------------------------------------------------------------

/// The `CustomErrorResponses` element wrapping `items`.
///
/// The entries themselves are built by the one builder this target has for them
/// ([`crate::edge_custom_error_responses::entry`]); what varies here is WHICH entries a
/// configuration states, which the sibling suite's own wrapper does not take.
fn custom_error_responses(items: &[String]) -> String {
    format!(
        "<CustomErrorResponses><Quantity>{}</Quantity><Items>{}</Items></CustomErrorResponses>",
        items.len(),
        items.concat()
    )
}

/// Both statuses a read of an absent key can earn, each pointing at `page` under
/// `response_code`: the configuration this issue's shape states, which covers the origin's
/// answer whichever of the two it is.
fn both_statuses(page: &str, response_code: u16, caching_ttl: u64) -> String {
    custom_error_responses(&[
        entry(FORBIDDEN, page, response_code, caching_ttl),
        entry(NOT_FOUND, page, response_code, caching_ttl),
    ])
}

/// `POST /2020-05-31/distribution?WithTags` over the private bucket `bucket` fronted by the
/// origin access control `control`, stating `custom_errors` and the shell as its
/// `DefaultRootObject`; answers `(Id, DomainName, ARN)`.
async fn create_distribution_with_tags(
    router: &MultiServiceRouter,
    case: &str,
    caller_reference: &str,
    bucket: &str,
    control: &str,
    custom_errors: &str,
) -> (String, String, String) {
    let created = call(
        router,
        "POST",
        &format!("/{API_VERSION}/distribution?WithTags"),
        Some(&format!(
            "<DistributionConfigWithTags \
             xmlns=\"http://cloudfront.amazonaws.com/doc/{API_VERSION}/\">\
             <DistributionConfig>\
             <CallerReference>{caller_reference}</CallerReference>\
             <DefaultRootObject>{SHELL_KEY}</DefaultRootObject>\
             <Origins><Quantity>1</Quantity><Items><Origin>\
             <Id>{ORIGIN_ID}</Id><DomainName>{}</DomainName>\
             <S3OriginConfig><OriginAccessIdentity></OriginAccessIdentity></S3OriginConfig>\
             <OriginAccessControlId>{control}</OriginAccessControlId>\
             </Origin></Items></Origins>\
             <DefaultCacheBehavior><TargetOriginId>{ORIGIN_ID}</TargetOriginId>\
             <ViewerProtocolPolicy>allow-all</ViewerProtocolPolicy>\
             <AllowedMethods><Quantity>2</Quantity>\
             <Items><Method>GET</Method><Method>HEAD</Method></Items></AllowedMethods>\
             <CachePolicyId>{CACHING_DISABLED}</CachePolicyId>\
             </DefaultCacheBehavior>\
             <CacheBehaviors><Quantity>0</Quantity></CacheBehaviors>\
             {custom_errors}\
             <Comment>a single-page application over a private s3 origin</Comment>\
             <Enabled>true</Enabled>\
             </DistributionConfig>\
             <Tags><Items><Tag><Key>suite</Key>\
             <Value>edge-custom-error-page</Value></Tag></Items></Tags>\
             </DistributionConfigWithTags>",
            origin_domain(bucket)
        )),
        None,
    )
    .await;
    assert_eq!(
        created.status, 201,
        "case {case:?}: CreateDistributionWithTags answered {}: {}",
        created.status, created.raw
    );
    let read = |member: &str| {
        created
            .root()
            .find_text(member)
            .unwrap_or_else(|| panic!("case {case:?}: the created distribution reports no {member}"))
            .to_string()
    };
    (read("Id"), read("DomainName"), read("ARN"))
}

/// The whole site in `bucket`, a distribution over it under its own origin access control
/// stating `custom_errors`, the bucket's grant to that distribution, and the wait for the clock
/// to deploy it; answers the `DomainName` a viewer addresses.
///
/// The grant is written AFTER the distribution exists, because it is scoped to that
/// distribution's own ARN — the order a client has to follow — and it grants `s3:GetObject`
/// alone, so a read of a key the bucket does not hold is refused rather than reported missing.
async fn deployed_site(
    emulator: &Emulator,
    case: &'static str,
    bucket: &'static str,
    custom_errors: &str,
) -> String {
    for (key, body) in [
        (SHELL_KEY, SHELL_BODY),
        (ASSET_KEY, ASSET_BODY),
        (MAINTENANCE_KEY, MAINTENANCE_BODY),
    ] {
        emulator.staged_object(case, bucket, key, body).await;
    }
    let control = create_origin_access_control(&emulator.setup, case, bucket).await;
    let (id, domain, arn) = create_distribution_with_tags(
        &emulator.setup,
        case,
        bucket,
        bucket,
        &control,
        custom_errors,
    )
    .await;
    emulator
        .staged_policy(case, bucket, &grant(bucket, &arn))
        .await;
    // A configuration that has not reached the edge locations serves no viewer, and the CLOCK
    // owes that propagation: the condition is polled for, never a span waited out.
    awaited_deployment(&emulator.setup, case, &id).await;
    domain
}

/// The status the ORIGIN answers a read of a key it does not hold with, read off a distribution
/// that states no custom error response at all — and the proof that such a distribution hands
/// the viewer the origin's own error document.
async fn origin_status_for_an_absent_key(emulator: &Emulator, domain: &str) -> u16 {
    let raw = view(&emulator.viewers, domain, ABSENT_PATHS[0].1, &[]).await;
    assert!(
        raw.body.contains(ORIGIN_ENVELOPE_MEMBER),
        "expected the origin's own error document through a distribution that states no custom \
         error response, got {} / {:?}",
        raw.status,
        raw.body
    );
    assert!(
        raw.status == FORBIDDEN || raw.status == NOT_FOUND,
        "expected the origin to answer a read of a key it does not hold with {FORBIDDEN} or \
         {NOT_FOUND}, got {} / {:?}",
        raw.status,
        raw.body
    );
    raw.status
}

/// Assert that `answer` is the page `expected_body` under `expected_status`, produced by
/// CloudFront itself and carrying nothing of the origin's own failure.
fn assert_substituted(answer: &Viewed, case: &str, expected_status: u16, expected_body: &str) {
    assert_eq!(
        (answer.status, answer.body.as_str()),
        (expected_status, expected_body),
        "case {case:?}: expected {expected_status} with the page the configuration names, got {} \
         / {:?}",
        answer.status,
        answer.body
    );
    assert_eq!(
        answer.x_cache, ERROR,
        "case {case:?}: CloudFront produced this answer, so it reports {ERROR:?}; got {:?}",
        answer.x_cache
    );
    for leaked in [ORIGIN_ENVELOPE_MEMBER, ACCESS_DENIED] {
        assert!(
            !answer.body.contains(leaked),
            "case {case:?}: the viewer must not receive the origin's failure mode ({leaked:?}), \
             got {:?}",
            answer.body
        );
    }
}

// ---------------------------------------------------------------------------
// The cases
// ---------------------------------------------------------------------------

/// The substitution itself: every viewer path the private origin holds nothing at is answered
/// with the shell's bytes under the `ResponseCode` the configuration states, while the paths the
/// origin does serve are answered with their own bytes.
#[tokio::test]
async fn an_origin_error_becomes_the_page_and_status_the_configuration_states() {
    let emulator = Emulator::new();
    let control = deployed_site(&emulator, "no custom error response", "spa-origin-control", "")
        .await;
    let origin_status = origin_status_for_an_absent_key(&emulator, &control).await;

    let shell = deployed_site(
        &emulator,
        "the shell for both statuses",
        "spa-origin-shell",
        &both_statuses(SHELL_KEY, SHELL_RESPONSE_CODE, NOT_HELD),
    )
    .await;

    for (case, path) in ABSENT_PATHS {
        let answer = view(&emulator.viewers, &shell, path, &[]).await;
        assert_substituted(
            &answer,
            &format!("{case} ({path}), the origin's {origin_status}"),
            SHELL_RESPONSE_CODE,
            SHELL_BODY,
        );
    }

    // An error the configuration holds for no time at all is earned again by the next request,
    // and is substituted again: the replacement is a property of the answer, not of one entry in
    // the cache.
    let (case, path) = ABSENT_PATHS[0];
    let again = view(&emulator.viewers, &shell, path, &[]).await;
    assert_substituted(
        &again,
        &format!("{case} ({path}), asked for a second time"),
        SHELL_RESPONSE_CODE,
        SHELL_BODY,
    );

    // The paths the origin SERVES: the configuration covers no status these answers carry, so
    // each is the object's own bytes straight from the origin.
    for (case, path, expected) in [
        ("the root, which the default root object names", "/", SHELL_BODY),
        ("an asset the origin holds", "/app.js", ASSET_BODY),
    ] {
        let served = view(&emulator.viewers, &shell, path, &[]).await;
        assert_eq!(
            (served.status, served.body.as_str()),
            (200, expected),
            "case {case:?}: expected the object itself, got {} / {:?}",
            served.status,
            served.body
        );
        assert_eq!(
            served.x_cache, MISS,
            "case {case:?}: the origin produced this answer, so the edge reports {MISS:?}; got \
             {:?}",
            served.x_cache
        );
    }
}

/// WHICH page and WHICH status an entry states, and which answers it covers: a configuration
/// naming another page under another `ResponseCode` serves that one, and a configuration that
/// does not name the status the origin answered replaces nothing at all.
#[tokio::test]
async fn an_entry_covers_the_status_it_names_and_serves_the_page_it_names() {
    let emulator = Emulator::new();
    let control = deployed_site(
        &emulator,
        "no custom error response",
        "spa-page-choice-control",
        "",
    )
    .await;
    let origin_status = origin_status_for_an_absent_key(&emulator, &control).await;

    // Another page, another replacement status: the two are read off the entry rather than fixed
    // by the edge.
    let maintenance = deployed_site(
        &emulator,
        "a second page under its own status",
        "spa-page-choice-maintenance",
        &both_statuses(MAINTENANCE_KEY, MAINTENANCE_RESPONSE_CODE, NOT_HELD),
    )
    .await;
    let (case, path) = ABSENT_PATHS[1];
    let answer = view(&emulator.viewers, &maintenance, path, &[]).await;
    assert_substituted(
        &answer,
        &format!("{case} ({path}), a second page under {MAINTENANCE_RESPONSE_CODE}"),
        MAINTENANCE_RESPONSE_CODE,
        MAINTENANCE_BODY,
    );

    // A configuration naming only the status the origin did NOT answer covers this answer not at
    // all, so the viewer receives the origin's own error.
    let other_status = if origin_status == FORBIDDEN {
        NOT_FOUND
    } else {
        FORBIDDEN
    };
    let unconfigured = deployed_site(
        &emulator,
        "only the status the origin did not answer",
        "spa-page-choice-unconfigured",
        &custom_error_responses(&[entry(
            other_status,
            SHELL_KEY,
            SHELL_RESPONSE_CODE,
            NOT_HELD,
        )]),
    )
    .await;
    let passed_through = view(&emulator.viewers, &unconfigured, path, &[]).await;
    assert_eq!(
        passed_through.status, origin_status,
        "case \"a configuration naming only {other_status}\": expected the origin's own \
         {origin_status}, got {} / {:?}",
        passed_through.status, passed_through.body
    );
    assert!(
        passed_through.body.contains(ORIGIN_ENVELOPE_MEMBER)
            && !passed_through.body.contains(SHELL_BODY),
        "case \"a configuration naming only {other_status}\": expected the origin's own error \
         document rather than the page it names for another status, got {:?}",
        passed_through.body
    );
}
