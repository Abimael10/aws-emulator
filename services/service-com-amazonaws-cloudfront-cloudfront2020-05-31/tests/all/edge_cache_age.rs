// SPDX-License-Identifier: CC0-1.0

//! How OLD the edge says the copy it just served is, end to end over HTTP.
//!
//! RULE: a response an HTTP cache serves out of its storage states, in `Age`, the whole
//! number of seconds since that response was generated at the origin (RFC 9111 §4.2.3 and
//! §5.1) — the span the entry has been resident plus whatever age the origin itself stated
//! about the bytes when they were fetched — while an answer the origin produced for the
//! request being served states no age of this cache's own. So every `Hit from cloudfront`
//! carries an `Age`, two hits of one entry held for different spans report the longer span
//! as the larger `Age`, and an entry whose TTL has run out is not served at all: the request
//! goes to the origin and is a `Miss from cloudfront` again.
//!
//! The rows span what the rule generalises over: HOW LONG the entry has been held (from no
//! time at all through seconds, a minute and an hour), WHETHER the origin's own answer
//! already stated an age (and whether what it stated is a number at all), and whether the
//! answer the viewer gets came out of the cache or from the origin.
//!
//! The span an entry has been held is a PARAMETER here rather than something this suite
//! waits out: the entry the first request filled is moved further into the past in the store
//! the edge reads it from, so one case states an age of an hour without an hour passing and
//! no case's outcome depends on how fast the host runs. The one thing the clock is asked for
//! is the deployment every suite waits for, through the shared poll loop.
//!
//! The origin is this service's own control-plane endpoint, which `GET
//! /2020-05-31/distribution` answers a `DistributionList` from — an origin like any other,
//! reached over the same in-process transport, and one that states no `Age` of its own, so a
//! case about an origin-stated age states it on the entry the edge holds.

use std::sync::Arc;
use std::time::{Duration, SystemTime};

use bytes::Bytes;
use service_com_amazonaws_cloudfront_cloudfront2020_05_31::state::ComAmazonawsCloudfrontCloudfront20200531State;
use service_com_amazonaws_cloudfront_cloudfront2020_05_31::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

use crate::distribution_lifecycle::{awaited_deployment, call, API_VERSION, HOST};

/// The AWS-managed cache policy `Managed-CachingOptimized`, whose `DefaultTTL` is a day: a
/// behaviour under it answers a second request for the same object out of the edge cache.
const CACHING_OPTIMIZED: &str = "658327ea-f89d-4fab-a63d-7e88639e58f6";

/// An origin this emulator SERVES: this service's own control-plane endpoint.
const SERVED_ORIGIN: &str = HOST;

/// A viewer path that origin answers.
const SERVED_PATH: &str = "/2020-05-31/distribution";

/// `X-Cache` for an answer the edge cache held, and for one the origin produced.
const HIT: &str = "Hit from cloudfront";
const MISS: &str = "Miss from cloudfront";

/// The response header a cache states the age of what it served in.
const AGE: &str = "age";

/// Half a second, added to the span a case holds an entry for so the whole-second age the
/// edge reports lands in the middle of a second rather than on its boundary.
const HALF_SECOND: Duration = Duration::from_millis(500);

/// How much larger than the span a case holds the entry for its reported age may be: the
/// request still has to be served, and a host under load may cross a second doing it. Only
/// the lower bound is the rule; this keeps the upper one off the host's speed.
const AGE_SLACK_SECONDS: u64 = 3;

/// One emulator on an engine this suite KEEPS: a case states how long the edge has HELD an
/// entry by moving that entry into the past in the store the edge reads it from, which needs
/// this handle.
fn emulator_with_its_engine() -> (MultiServiceRouter, Arc<engine::InMemoryEngine>) {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(EmulatedService::new(Arc::clone(&engine)));
    (router, engine)
}

/// What the edge answered one viewer request with.
struct Viewed {
    status: u16,
    x_cache: String,
    /// `Age` exactly as the answer carried it, or `None` when it carried none.
    age: Option<String>,
    body: String,
}

impl Viewed {
    /// The age the answer stated, as the seconds it is, or `None` when it stated none.
    fn age_seconds(&self) -> Option<u64> {
        self.age.as_deref().and_then(|value| value.parse().ok())
    }
}

/// Send one VIEWER request: no AWS credential, addressed to `host`, exactly as a browser
/// reaches a distribution.
async fn view(router: &MultiServiceRouter, host: &str, path: &str) -> Viewed {
    let request = ::http::Request::builder()
        .method("GET")
        .uri(path)
        .header("host", host)
        .body(Bytes::new())
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let header = |name: &str| {
        response
            .headers()
            .get(name)
            .and_then(|value| value.to_str().ok())
            .map(ToString::to_string)
    };
    let (x_cache, age) = (header("x-cache").unwrap_or_default(), header(AGE));
    Viewed {
        status,
        x_cache,
        age,
        body: String::from_utf8_lossy(&response.into_body()).to_string(),
    }
}

/// Create one distribution over the served origin under the caching policy and let the clock
/// deploy it: its id and the domain name CloudFront assigned it.
async fn deployed(router: &MultiServiceRouter, caller_reference: &str) -> (String, String) {
    let created = call(
        router,
        "POST",
        &format!("/{API_VERSION}/distribution"),
        Some(&format!(
            "<DistributionConfig xmlns=\"http://cloudfront.amazonaws.com/doc/{API_VERSION}/\">\
             <CallerReference>{caller_reference}</CallerReference>\
             <Origins><Quantity>1</Quantity><Items><Origin>\
             <Id>primary</Id><DomainName>{SERVED_ORIGIN}</DomainName>\
             <CustomOriginConfig><HTTPPort>80</HTTPPort><HTTPSPort>443</HTTPSPort>\
             <OriginProtocolPolicy>https-only</OriginProtocolPolicy></CustomOriginConfig>\
             </Origin></Items></Origins>\
             <DefaultCacheBehavior><TargetOriginId>primary</TargetOriginId>\
             <ViewerProtocolPolicy>allow-all</ViewerProtocolPolicy>\
             <AllowedMethods><Quantity>2</Quantity>\
             <Items><Method>GET</Method><Method>HEAD</Method></Items>\
             <CachedMethods><Quantity>2</Quantity>\
             <Items><Method>GET</Method><Method>HEAD</Method></Items></CachedMethods>\
             </AllowedMethods>\
             <CachePolicyId>{CACHING_OPTIMIZED}</CachePolicyId>\
             </DefaultCacheBehavior>\
             <CacheBehaviors><Quantity>0</Quantity></CacheBehaviors>\
             <Comment>the age of a cached object</Comment>\
             <Enabled>true</Enabled>\
             </DistributionConfig>"
        )),
        None,
    )
    .await;
    assert_eq!(
        created.status, 201,
        "case {caller_reference:?}: creating the distribution answered {} with {}",
        created.status, created.raw
    );
    let id = created
        .root()
        .find_text("Id")
        .expect("the created distribution reports an id")
        .to_string();
    let domain = created
        .root()
        .find_text("DomainName")
        .expect("the created distribution reports a domain name")
        .to_string();
    awaited_deployment(router, caller_reference, &id).await;
    (id, domain)
}

/// State that every object this distribution's edge holds was fetched `held` ago, and that
/// the origin's own answer stated `origin_age` about its age (or stated none).
///
/// This is how a case states the span an entry has been resident without waiting for it. The
/// entry's expiry is left where it is, so the object is still servable however long a case
/// says it has been held.
fn held_for(
    engine: &Arc<engine::InMemoryEngine>,
    distribution_id: &str,
    held: Duration,
    origin_age: Option<&str>,
) {
    let stored_at = SystemTime::now()
        .checked_sub(held + HALF_SECOND)
        .expect("the clock reaches back over the span a case holds an entry for");
    let state = engine.state::<ComAmazonawsCloudfrontCloudfront20200531State>();
    let mut store = state.lock();
    let distribution = store
        .distributions
        .get_mut(distribution_id)
        .unwrap_or_else(|| panic!("the store holds distribution {distribution_id}"));
    assert!(
        !distribution.edge_cache.is_empty(),
        "the edge cache holds nothing, so there is no entry for a case to age"
    );
    for entry in distribution.edge_cache.values_mut() {
        entry.stored_at = stored_at;
        entry.headers.retain(|(name, _)| name != AGE);
        if let Some(stated) = origin_age {
            entry
                .headers
                .push((AGE.to_string(), stated.to_string()));
        }
    }
}

/// State that every object this distribution's edge holds has run out of TTL.
fn expired(engine: &Arc<engine::InMemoryEngine>, distribution_id: &str) {
    let expires_at = SystemTime::now()
        .checked_sub(Duration::from_secs(1))
        .expect("the clock reaches back a second");
    let state = engine.state::<ComAmazonawsCloudfrontCloudfront20200531State>();
    let mut store = state.lock();
    let distribution = store
        .distributions
        .get_mut(distribution_id)
        .unwrap_or_else(|| panic!("the store holds distribution {distribution_id}"));
    for entry in distribution.edge_cache.values_mut() {
        entry.expires_at = expires_at;
    }
}

/// A HIT states how long the edge has held the object: the span it has been resident plus
/// whatever age the origin stated about the bytes, in whole seconds.
#[tokio::test]
async fn a_hit_states_the_age_of_the_object_the_edge_held() {
    let (router, engine) = emulator_with_its_engine();
    let (id, domain) = deployed(&router, "edge-cache-age").await;

    // The first request fills the cache, which is the entry every case below ages.
    let first = view(&router, &domain, SERVED_PATH).await;
    assert_eq!(
        (first.status, first.x_cache.as_str()),
        (200, MISS),
        "the first request must reach the origin: got {} with {:?} (body {:?})",
        first.status,
        first.x_cache,
        first.body
    );

    // (case, how long the entry has been held, what the origin said about its own age,
    // the age the hit must state at least). The rows differ along the span held — none of
    // it, one second, seconds, a minute, an hour — and along the origin's own statement:
    // none, a number that adds to the span, and a value that is no number of seconds at
    // all, which states no age to add.
    let cases: [(&str, u64, Option<&str>, u64); 8] = [
        ("held no time at all", 0, None, 0),
        ("held a second", 1, None, 1),
        ("held seven seconds", 7, None, 7),
        ("held just under a minute", 59, None, 59),
        ("held an hour", 3_600, None, 3_600),
        ("held five seconds behind an origin age of ten", 5, Some("10"), 15),
        ("held no time behind an origin age of two", 0, Some("2"), 2),
        ("held four seconds behind an origin age of nonsense", 4, Some("later"), 4),
    ];

    for (case, held, origin_age, least) in cases {
        held_for(&engine, &id, Duration::from_secs(held), origin_age);
        let answer = view(&router, &domain, SERVED_PATH).await;
        assert_eq!(
            (answer.status, answer.x_cache.as_str()),
            (200, HIT),
            "case {case:?}: expected the object out of the cache, got {} with {:?} (body \
             {:?})",
            answer.status,
            answer.x_cache,
            answer.body
        );
        let stated = answer.age_seconds().unwrap_or_else(|| {
            panic!(
                "case {case:?}: expected an Age of at least {least} seconds, got {:?}",
                answer.age
            )
        });
        assert!(
            stated >= least && stated <= least + AGE_SLACK_SECONDS,
            "case {case:?}: expected an Age of {least} seconds (and no more than {} \
             while the request is served), got {stated:?}",
            least + AGE_SLACK_SECONDS
        );
        assert_eq!(
            answer.body, first.body,
            "case {case:?}: the hit must serve the object the origin answered, got {:?}",
            answer.body
        );
    }
}

/// Two hits of ONE cached object report the longer residence as the larger age, and the
/// answer the origin produced for the request being served states no age of this cache's
/// own.
#[tokio::test]
async fn an_age_grows_with_the_time_the_object_has_been_held() {
    let (router, engine) = emulator_with_its_engine();
    let (id, domain) = deployed(&router, "edge-cache-age-grows").await;

    let miss = view(&router, &domain, SERVED_PATH).await;
    assert_eq!(
        (miss.status, miss.x_cache.as_str()),
        (200, MISS),
        "the first request must reach the origin: got {} with {:?}",
        miss.status,
        miss.x_cache
    );
    assert_eq!(
        miss.age, None,
        "the origin produced this answer for this request, so the edge states no age of \
         its own about it, got {:?}",
        miss.age
    );

    // The same entry, read after each of a growing sequence of residences: what the edge
    // states about it grows strictly with the span it has been held.
    let mut reported: Vec<(u64, u64)> = Vec::new();
    for held in [0u64, 2, 11, 90] {
        held_for(&engine, &id, Duration::from_secs(held), None);
        let answer = view(&router, &domain, SERVED_PATH).await;
        assert_eq!(
            answer.x_cache, HIT,
            "case held {held}s: expected the object out of the cache, got {:?}",
            answer.x_cache
        );
        let stated = answer.age_seconds().unwrap_or_else(|| {
            panic!("case held {held}s: expected an Age, got {:?}", answer.age)
        });
        reported.push((held, stated));
    }
    for pair in reported.windows(2) {
        let [(earlier, earlier_age), (later, later_age)] = [pair[0], pair[1]];
        assert!(
            later_age > earlier_age,
            "an object held {later}s must report a larger Age than the same object held \
             {earlier}s, got {later_age} after {earlier_age}"
        );
    }
}

/// An entry whose TTL has run out is not served from the cache at all: the request goes to
/// the origin, so it is a miss again and carries no age of this cache's own.
#[tokio::test]
async fn an_object_held_past_its_ttl_is_a_miss_again() {
    let (router, engine) = emulator_with_its_engine();
    let (id, domain) = deployed(&router, "edge-cache-age-expiry").await;

    let miss = view(&router, &domain, SERVED_PATH).await;
    assert_eq!(
        (miss.status, miss.x_cache.as_str()),
        (200, MISS),
        "the first request must reach the origin: got {} with {:?}",
        miss.status,
        miss.x_cache
    );

    held_for(&engine, &id, Duration::from_secs(30), None);
    let hit = view(&router, &domain, SERVED_PATH).await;
    assert!(
        hit.x_cache == HIT
            && hit
                .age_seconds()
                .is_some_and(|stated| (30..=30 + AGE_SLACK_SECONDS).contains(&stated)),
        "within its TTL the object is served from the cache under an age of 30 seconds, \
         got {:?} with {:?}",
        hit.x_cache,
        hit.age
    );

    expired(&engine, &id);
    let refetched = view(&router, &domain, SERVED_PATH).await;
    assert_eq!(
        (refetched.status, refetched.x_cache.as_str()),
        (200, MISS),
        "past its TTL the object is fetched again, got {} with {:?} (body {:?})",
        refetched.status,
        refetched.x_cache,
        refetched.body
    );
    assert_eq!(
        refetched.age, None,
        "the origin produced this answer for this request, so the edge states no age of \
         its own about it, got {:?}",
        refetched.age
    );
}
