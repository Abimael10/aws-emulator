// SPDX-License-Identifier: CC0-1.0

//! WHICH policy owns the edge CACHE KEY of a deployed distribution — end to end over HTTP,
//! with Amazon S3 mounted beside CloudFront on one engine so the origin is a real object
//! store and every answer a viewer gets carries an origin's own bytes.
//!
//! RULE: the cache key of a viewer request is formed from the parts of it the matched cache
//! behaviour's CACHE POLICY names and from nothing else, so two requests differing only in a
//! part that policy does not name are ONE cached object however the behaviour's ORIGIN
//! REQUEST POLICY treats that part. `OriginRequestPolicyConfig` "determines the values that
//! CloudFront includes in requests that it sends to the origin" and closes with "if you want
//! to send values to the origin and also include them in the cache key, use `CachePolicy`",
//! while `CachePolicyHeadersConfig$HeaderBehavior` is what "determines whether any HTTP
//! headers are included in the cache key" — and its `none` value states that "even when this
//! field is set to `none`, any headers that are listed in an `OriginRequestPolicy` _are_
//! included in origin requests", which is the forwarding half and says nothing about the key
//! (`smithy_query(query="shape",shape="OriginRequestPolicyConfig,CachePolicyHeadersConfig")`).
//!
//! The rows span what the rule generalises over: both values
//! `CachePolicyHeaderBehavior` admits (`none`, `whitelist`), every value
//! `OriginRequestPolicyHeaderBehavior` admits (`none`, `whitelist`, `allViewer`,
//! `allViewerAndWhitelistCloudFront`, `allExcept`) and a behaviour naming NO origin request
//! policy at all, which of the two headers under test the viewers differ in (one a policy
//! names, one neither does), and the position in the request sequence (an entry made, an
//! entry reused, an earlier entry returned to). The last suite puts two behaviours of ONE
//! distribution side by side, the same header name keyed by one behaviour's cache policy and
//! merely forwarded by the other's origin request policy, so the decision is the behaviour's
//! own rather than the distribution's.
//!
//! The harness is the sibling suites' — the two services on one engine, the staged bucket,
//! the published origin access control and the viewer are
//! [`crate::edge_origin_response_headers`]'s. The policy documents are written here because
//! the rule is about the HEADERS the two policies name, which neither sibling builder
//! parameterises ([`crate::edge_request_shaping::cache_policy`] keys on query strings and
//! fixes `HeaderBehavior` at `none`).

use smithy_server_runtime::MultiServiceRouter;

use crate::distribution_lifecycle::{awaited_deployment, call, API_VERSION};
use crate::edge_origin_response_headers::{
    create_origin_access_control, origin_domain, view, Emulator, Staged, Viewed, HIT, MISS,
};

/// The viewer header the policies below name: the one two successive viewers differ in.
const VARIANT_HEADER: &str = "x-sim-variant";

/// A second viewer header, so a row can name one header in a policy while the viewers differ
/// in the other.
const OTHER_HEADER: &str = "x-sim-other";

/// A header the EDGE generates rather than receives, which only
/// `allViewerAndWhitelistCloudFront` lists ("the additional CloudFront headers that are
/// listed in the `Headers` type"). Taken from the family
/// `OriginRequestPolicyHeadersConfig$HeaderBehavior` documents, not from any recorded value.
const GENERATED_HEADER: &str = "CloudFront-Viewer-Country";

/// The id every distribution below gives its one S3 origin.
const ORIGIN_ID: &str = "origin-bucket";

/// How long each cache policy below holds an answer, in seconds: long enough that every
/// request of a sequence is decided by the cache key rather than by an entry that expired
/// meanwhile, and nothing waits for it to pass.
const CACHED_SECONDS: i64 = 3600;

/// The values `CachePolicyHeaderBehavior` and `OriginRequestPolicyHeaderBehavior` admit,
/// spelled as the model spells them
/// (`smithy_query(query="shape",shape="CachePolicyHeaderBehavior")` and
/// `...="OriginRequestPolicyHeaderBehavior"`).
const NONE: &str = "none";
const WHITELIST: &str = "whitelist";
const ALL_VIEWER: &str = "allViewer";
const ALL_VIEWER_AND_CLOUDFRONT: &str = "allViewerAndWhitelistCloudFront";
const ALL_EXCEPT: &str = "allExcept";

// ---------------------------------------------------------------------------
// The policies and the distribution a case configures
// ---------------------------------------------------------------------------

/// The `Headers` wrapper a policy lists its names in: its `Quantity` and its `Items`, each
/// name in the `Name` element the model gives `HeaderList`. A policy that lists nothing
/// carries no wrapper at all.
fn header_list(names: &[&str]) -> String {
    if names.is_empty() {
        return String::new();
    }
    let items: String = names
        .iter()
        .map(|name| format!("<Name>{name}</Name>"))
        .collect();
    format!(
        "<Headers><Quantity>{}</Quantity><Items>{items}</Items></Headers>",
        names.len()
    )
}

/// The `Id` a creation minted, or a failure naming what was being created.
fn minted_id(answer: &crate::distribution_lifecycle::Answer, case: &str, what: &str) -> String {
    assert_eq!(
        answer.status, 201,
        "case {case:?}: creating the {what} answered {}: {}",
        answer.status, answer.raw
    );
    answer
        .root()
        .find_text("Id")
        .unwrap_or_else(|| panic!("case {case:?}: the created {what} reports no Id: {}", answer.raw))
        .to_string()
}

/// `CreateCachePolicy` whose `HeadersConfig` carries `behavior` over `listed`, caching for
/// [`CACHED_SECONDS`] and keying on no cookie and no query string — so the HEADERS are the
/// only part of a viewer request its key can carry.
async fn create_cache_policy(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    behavior: &str,
    listed: &[&str],
) -> String {
    let created = call(
        router,
        "POST",
        &format!("/{API_VERSION}/cache-policy"),
        Some(&format!(
            "<CachePolicyConfig xmlns=\"http://cloudfront.amazonaws.com/doc/{API_VERSION}/\">\
             <Name>{name}</Name>\
             <DefaultTTL>{CACHED_SECONDS}</DefaultTTL><MaxTTL>{CACHED_SECONDS}</MaxTTL>\
             <MinTTL>0</MinTTL>\
             <ParametersInCacheKeyAndForwardedToOrigin>\
             <EnableAcceptEncodingGzip>false</EnableAcceptEncodingGzip>\
             <HeadersConfig><HeaderBehavior>{behavior}</HeaderBehavior>{}</HeadersConfig>\
             <CookiesConfig><CookieBehavior>none</CookieBehavior></CookiesConfig>\
             <QueryStringsConfig><QueryStringBehavior>none</QueryStringBehavior>\
             </QueryStringsConfig>\
             </ParametersInCacheKeyAndForwardedToOrigin>\
             </CachePolicyConfig>",
            header_list(listed)
        )),
        None,
    )
    .await;
    minted_id(&created, case, "cache policy")
}

/// `CreateOriginRequestPolicy` whose `HeadersConfig` carries `behavior` over `listed`, and
/// which forwards no cookie and no query string of its own.
async fn create_origin_request_policy(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    behavior: &str,
    listed: &[&str],
) -> String {
    let created = call(
        router,
        "POST",
        &format!("/{API_VERSION}/origin-request-policy"),
        Some(&format!(
            "<OriginRequestPolicyConfig \
             xmlns=\"http://cloudfront.amazonaws.com/doc/{API_VERSION}/\">\
             <Name>{name}</Name>\
             <HeadersConfig><HeaderBehavior>{behavior}</HeaderBehavior>{}</HeadersConfig>\
             <CookiesConfig><CookieBehavior>none</CookieBehavior></CookiesConfig>\
             <QueryStringsConfig><QueryStringBehavior>none</QueryStringBehavior>\
             </QueryStringsConfig>\
             </OriginRequestPolicyConfig>",
            header_list(listed)
        )),
        None,
    )
    .await;
    minted_id(&created, case, "origin request policy")
}

/// One cache behaviour of a distribution: the pattern it answers (`None` for the default
/// behaviour, which answers everything no ordered pattern matched), the cache policy that
/// decides its cache key, and the origin request policy it forwards with.
struct Behaviour<'a> {
    path_pattern: Option<&'a str>,
    cache_policy: &'a str,
    origin_request_policy: Option<&'a str>,
}

/// The members one behaviour carries, in the order the model declares them.
fn behaviour_members(behaviour: &Behaviour<'_>) -> String {
    let pattern = behaviour
        .path_pattern
        .map(|pattern| format!("<PathPattern>{pattern}</PathPattern>"))
        .unwrap_or_default();
    let forwarded = behaviour
        .origin_request_policy
        .map(|id| format!("<OriginRequestPolicyId>{id}</OriginRequestPolicyId>"))
        .unwrap_or_default();
    format!(
        "{pattern}<TargetOriginId>{ORIGIN_ID}</TargetOriginId>\
         <ViewerProtocolPolicy>allow-all</ViewerProtocolPolicy>\
         <AllowedMethods><Quantity>2</Quantity>\
         <Items><Method>GET</Method><Method>HEAD</Method></Items></AllowedMethods>\
         <CachePolicyId>{}</CachePolicyId>{forwarded}",
        behaviour.cache_policy
    )
}

/// `CreateDistribution` over the S3 origin `bucket` fronted by the origin access control
/// `control`, with `default` as its default cache behaviour and `ordered` as its ordered
/// ones; answers `(Id, DomainName, ARN)`.
///
/// Written here rather than taken from
/// [`crate::edge_origin_response_headers::create_distribution_over_bucket`] because the last
/// suite needs ORDERED behaviours, which that builder does not carry.
async fn create_distribution(
    router: &MultiServiceRouter,
    case: &str,
    caller_reference: &str,
    bucket: &str,
    control: &str,
    default: &Behaviour<'_>,
    ordered: &[Behaviour<'_>],
) -> (String, String, String) {
    let behaviours = if ordered.is_empty() {
        String::new()
    } else {
        let items: String = ordered
            .iter()
            .map(|behaviour| format!("<CacheBehavior>{}</CacheBehavior>", behaviour_members(behaviour)))
            .collect();
        format!(
            "<CacheBehaviors><Quantity>{}</Quantity><Items>{items}</Items></CacheBehaviors>",
            ordered.len()
        )
    };
    let created = call(
        router,
        "POST",
        &format!("/{API_VERSION}/distribution"),
        Some(&format!(
            "<DistributionConfig xmlns=\"http://cloudfront.amazonaws.com/doc/{API_VERSION}/\">\
             <CallerReference>{caller_reference}</CallerReference>\
             <Origins><Quantity>1</Quantity><Items><Origin>\
             <Id>{ORIGIN_ID}</Id><DomainName>{}</DomainName>\
             <S3OriginConfig><OriginAccessIdentity></OriginAccessIdentity></S3OriginConfig>\
             <OriginAccessControlId>{control}</OriginAccessControlId>\
             </Origin></Items></Origins>\
             <DefaultCacheBehavior>{}</DefaultCacheBehavior>\
             {behaviours}\
             <Comment>{caller_reference}</Comment>\
             <Enabled>true</Enabled>\
             </DistributionConfig>",
            origin_domain(bucket),
            behaviour_members(default)
        )),
        None,
    )
    .await;
    assert_eq!(
        created.status, 201,
        "case {case:?}: CreateDistribution answered {}: {}",
        created.status, created.raw
    );
    let read = |member: &str| {
        created
            .root()
            .find_text(member)
            .unwrap_or_else(|| {
                panic!("case {case:?}: the created distribution reports no {member}: {}", created.raw)
            })
            .to_string()
    };
    (read("Id"), read("DomainName"), read("ARN"))
}

/// One viewer request for `path` carrying `variant` for [`VARIANT_HEADER`] and a value of its
/// own for [`OTHER_HEADER`], asserted to be served `200` with the origin object's own bytes;
/// answers what the edge said about it.
async fn served(
    router: &MultiServiceRouter,
    case: &str,
    domain: &str,
    path: &str,
    variant: &str,
    other: &str,
    body: &str,
) -> Viewed {
    let viewed = view(
        router,
        "GET",
        domain,
        path,
        &[(VARIANT_HEADER, variant), (OTHER_HEADER, other)],
    )
    .await;
    assert_eq!(
        viewed.status, 200,
        "case {case:?} ({path}, {VARIANT_HEADER}: {variant:?}): expected the edge to serve \
         the origin object with 200, got {} with {:?}",
        viewed.status, viewed.body
    );
    assert_eq!(
        viewed.body, body,
        "case {case:?} ({path}, {VARIANT_HEADER}: {variant:?}): expected the origin's own \
         bytes {body:?}, got {:?}",
        viewed.body
    );
    viewed
}

// ---------------------------------------------------------------------------
// One behaviour: which policy's names separate two cached objects
// ---------------------------------------------------------------------------

/// One row: the two policies in front of one origin, and what the SECOND viewer — differing
/// from the first only in [`VARIANT_HEADER`] — must be answered.
struct Row {
    case: &'static str,
    /// Its own bucket, so the bucket policy that admits the origin fetch is scoped to this
    /// row's own distribution ARN rather than shared between rows.
    bucket: &'static str,
    /// `CachePolicyHeadersConfig`: the behaviour and the names it lists.
    keyed: (&'static str, &'static [&'static str]),
    /// `OriginRequestPolicyHeadersConfig`, or `None` for a behaviour naming no origin
    /// request policy at all.
    forwarded: Option<(&'static str, &'static [&'static str])>,
    /// `X-Cache` of the second viewer: a MISS only where the CACHE POLICY names the header
    /// the two viewers differ in.
    second: &'static str,
}

/// The table. Every value of both header-behaviour enums appears, in both the role of a
/// policy that names the changing header and of one that names another.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "the cache policy names the header and no origin request policy exists",
            bucket: "cache-key-owner-keyed-only",
            keyed: (WHITELIST, &[VARIANT_HEADER]),
            forwarded: None,
            second: MISS,
        },
        Row {
            case: "both policies name the header",
            bucket: "cache-key-owner-both",
            keyed: (WHITELIST, &[VARIANT_HEADER]),
            forwarded: Some((WHITELIST, &[VARIANT_HEADER])),
            second: MISS,
        },
        Row {
            case: "the cache policy names the header while the origin request policy names \
                   another",
            bucket: "cache-key-owner-keyed-other-forwarded",
            keyed: (WHITELIST, &[VARIANT_HEADER]),
            forwarded: Some((WHITELIST, &[OTHER_HEADER])),
            second: MISS,
        },
        Row {
            case: "only the origin request policy names the header",
            bucket: "cache-key-owner-forwarded-only",
            keyed: (NONE, &[]),
            forwarded: Some((WHITELIST, &[VARIANT_HEADER])),
            second: HIT,
        },
        Row {
            case: "the origin request policy forwards every viewer header",
            bucket: "cache-key-owner-all-viewer",
            keyed: (NONE, &[]),
            forwarded: Some((ALL_VIEWER, &[])),
            second: HIT,
        },
        Row {
            case: "the origin request policy forwards every viewer header and one the edge \
                   generates",
            bucket: "cache-key-owner-all-viewer-and-cloudfront",
            keyed: (NONE, &[]),
            forwarded: Some((ALL_VIEWER_AND_CLOUDFRONT, &[GENERATED_HEADER])),
            second: HIT,
        },
        Row {
            case: "the origin request policy forwards every viewer header except another one",
            bucket: "cache-key-owner-all-except",
            keyed: (NONE, &[]),
            forwarded: Some((ALL_EXCEPT, &[OTHER_HEADER])),
            second: HIT,
        },
        Row {
            case: "the origin request policy names no header at all",
            bucket: "cache-key-owner-forwards-none",
            keyed: (NONE, &[]),
            forwarded: Some((NONE, &[])),
            second: HIT,
        },
        Row {
            case: "neither policy names any header",
            bucket: "cache-key-owner-neither",
            keyed: (NONE, &[]),
            forwarded: None,
            second: HIT,
        },
        Row {
            case: "the cache policy names the OTHER header while every viewer header is \
                   forwarded",
            bucket: "cache-key-owner-keys-the-other-header",
            keyed: (WHITELIST, &[OTHER_HEADER]),
            forwarded: Some((ALL_VIEWER, &[])),
            second: HIT,
        },
    ]
}

/// The object every row reads, and the bytes its origin answers with.
const ROW_OBJECT: Staged = Staged::plain("keyed/obj.txt", "the origin's own keyed bytes");

/// RULE: the cache key carries the headers the behaviour's CACHE POLICY names and nothing
/// else, so two viewers differing only in a header that policy does not name are one cached
/// object however the ORIGIN REQUEST POLICY forwards that header.
#[tokio::test]
async fn the_cache_key_carries_only_the_headers_the_cache_policy_names() {
    let emulator = Emulator::new();

    for (index, row) in rows().into_iter().enumerate() {
        let case = row.case;
        emulator.staged_bucket(case, row.bucket).await;
        emulator.staged_object(case, row.bucket, &ROW_OBJECT).await;
        let control =
            create_origin_access_control(&emulator.router, case, &format!("cache-key-{index}")).await;
        let cache_policy = create_cache_policy(
            &emulator.router,
            case,
            &format!("cache-key-{index}"),
            row.keyed.0,
            row.keyed.1,
        )
        .await;
        let forwarded = match row.forwarded {
            Some((behavior, listed)) => Some(
                create_origin_request_policy(
                    &emulator.router,
                    case,
                    &format!("forwarded-{index}"),
                    behavior,
                    listed,
                )
                .await,
            ),
            None => None,
        };
        let (id, domain, arn) = create_distribution(
            &emulator.router,
            case,
            &format!("cache-key-owner-{index}"),
            row.bucket,
            &control,
            &Behaviour {
                path_pattern: None,
                cache_policy: &cache_policy,
                origin_request_policy: forwarded.as_deref(),
            },
            &[],
        )
        .await;
        emulator.granted_to_cloudfront(case, row.bucket, &arn).await;
        awaited_deployment(&emulator.router, case, &id).await;

        let path = format!("/{}", ROW_OBJECT.key);
        // The first viewer: nothing is cached for it yet, so the origin answers it whatever
        // the policies say.
        let first = served(
            &emulator.router,
            case,
            &domain,
            &path,
            "one",
            "constant",
            ROW_OBJECT.body,
        )
        .await;
        assert_eq!(
            first.x_cache(),
            MISS,
            "case {case:?}: the first viewer must be answered by the origin, got X-Cache \
             {:?}",
            first.x_cache()
        );
        // The second viewer differs from the first ONLY in the header under test.
        let second = served(
            &emulator.router,
            case,
            &domain,
            &path,
            "two",
            "constant",
            ROW_OBJECT.body,
        )
        .await;
        assert_eq!(
            second.x_cache(),
            row.second,
            "case {case:?} (cache policy {:?}, origin request policy {:?}): expected X-Cache \
             {:?} for a viewer differing only in {VARIANT_HEADER}, got {:?}",
            row.keyed,
            row.forwarded,
            row.second,
            second.x_cache()
        );
        // Whatever the second viewer was, repeating it is the same object: an entry it made
        // is held, and one it was served is still there.
        let third = served(
            &emulator.router,
            case,
            &domain,
            &path,
            "two",
            "constant",
            ROW_OBJECT.body,
        )
        .await;
        assert_eq!(
            third.x_cache(),
            HIT,
            "case {case:?}: repeating the second viewer must be served the cached object, got \
             X-Cache {:?}",
            third.x_cache()
        );
    }
}

// ---------------------------------------------------------------------------
// Two behaviours of one distribution
// ---------------------------------------------------------------------------

/// The object the keyed behaviour's pattern matches, and the one the forwarding behaviour's
/// does: different bytes, so an answer served under one pattern can never pass for the
/// other's.
const KEYED_OBJECT: Staged = Staged::plain("keyed/obj.txt", "the keyed behaviour's object");
const FORWARDED_OBJECT: Staged =
    Staged::plain("forwarded/obj.txt", "the forwarding behaviour's object");

/// One step of the sequence the two behaviours are driven through.
struct Step {
    case: &'static str,
    path: &'static str,
    /// The bytes the behaviour's own origin object answers with.
    body: &'static str,
    variant: &'static str,
    x_cache: &'static str,
}

/// RULE again, per BEHAVIOUR: each cache behaviour of one distribution keys on its OWN cache
/// policy, so the same header name separates two cached objects under the behaviour whose
/// cache policy names it and separates nothing under the behaviour whose origin request
/// policy alone names it.
#[tokio::test]
async fn each_behaviour_keys_on_its_own_cache_policy_and_not_on_its_origin_request_policy() {
    let emulator = Emulator::new();
    let case = "two behaviours naming the same header in different policies";
    let bucket = "cache-key-owner-two-behaviours";
    emulator.staged_bucket(case, bucket).await;
    emulator.staged_object(case, bucket, &KEYED_OBJECT).await;
    emulator.staged_object(case, bucket, &FORWARDED_OBJECT).await;
    let control = create_origin_access_control(&emulator.router, case, "two-behaviours").await;

    // One cache policy that KEYS on the header, one that names no header at all, and an
    // origin request policy that FORWARDS the header the second does not key on.
    let keying = create_cache_policy(
        &emulator.router,
        case,
        "two-behaviours-keying",
        WHITELIST,
        &[VARIANT_HEADER],
    )
    .await;
    let plain = create_cache_policy(&emulator.router, case, "two-behaviours-plain", NONE, &[]).await;
    let forwarding = create_origin_request_policy(
        &emulator.router,
        case,
        "two-behaviours-forwarding",
        WHITELIST,
        &[VARIANT_HEADER],
    )
    .await;

    let (id, domain, arn) = create_distribution(
        &emulator.router,
        case,
        "cache-key-owner-two-behaviours",
        bucket,
        &control,
        // The default behaviour keys on nothing and forwards nothing.
        &Behaviour {
            path_pattern: None,
            cache_policy: &plain,
            origin_request_policy: None,
        },
        &[
            Behaviour {
                path_pattern: Some("/keyed/*"),
                cache_policy: &keying,
                origin_request_policy: None,
            },
            Behaviour {
                path_pattern: Some("/forwarded/*"),
                cache_policy: &plain,
                origin_request_policy: Some(&forwarding),
            },
        ],
    )
    .await;
    emulator.granted_to_cloudfront(case, bucket, &arn).await;
    awaited_deployment(&emulator.router, case, &id).await;

    let steps = [
        Step {
            case: "the keyed behaviour, first value",
            path: "/keyed/obj.txt",
            body: KEYED_OBJECT.body,
            variant: "one",
            x_cache: MISS,
        },
        Step {
            case: "the keyed behaviour, another value of the header its cache policy names",
            path: "/keyed/obj.txt",
            body: KEYED_OBJECT.body,
            variant: "two",
            x_cache: MISS,
        },
        Step {
            case: "the keyed behaviour, that value again",
            path: "/keyed/obj.txt",
            body: KEYED_OBJECT.body,
            variant: "two",
            x_cache: HIT,
        },
        Step {
            case: "the keyed behaviour, back to the first value",
            path: "/keyed/obj.txt",
            body: KEYED_OBJECT.body,
            variant: "one",
            x_cache: HIT,
        },
        Step {
            case: "the forwarding behaviour, first value",
            path: "/forwarded/obj.txt",
            body: FORWARDED_OBJECT.body,
            variant: "one",
            x_cache: MISS,
        },
        Step {
            case: "the forwarding behaviour, another value of the header only its origin \
                   request policy names",
            path: "/forwarded/obj.txt",
            body: FORWARDED_OBJECT.body,
            variant: "two",
            x_cache: HIT,
        },
        Step {
            case: "the forwarding behaviour, a third value of it",
            path: "/forwarded/obj.txt",
            body: FORWARDED_OBJECT.body,
            variant: "three",
            x_cache: HIT,
        },
    ];

    for step in steps {
        let viewed = served(
            &emulator.router,
            step.case,
            &domain,
            step.path,
            step.variant,
            "constant",
            step.body,
        )
        .await;
        assert_eq!(
            viewed.x_cache(),
            step.x_cache,
            "case {:?} ({}, {VARIANT_HEADER}: {:?}): expected X-Cache {:?}, got {:?}",
            step.case,
            step.path,
            step.variant,
            step.x_cache,
            viewed.x_cache()
        );
    }
}
