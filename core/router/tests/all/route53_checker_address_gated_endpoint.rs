// SPDX-License-Identifier: CC0-1.0

//! An endpoint whose access is conditioned on the ADDRESS a request is made from, read by the
//! service that serves it and by the Route 53 health checker that probes it, end to end over
//! the assembled router.
//!
//! The rule under test: **the address a request was made from is part of its condition
//! context, so a resource-policy clause over `aws:SourceIp` ("the requester's IP address", AWS
//! General Reference, *Global condition context keys*) is DECIDED against that address — a
//! grant conditioned on a set of addresses admits a request from inside the set and refuses
//! one from outside it — and a Route 53 health checker's probe is made FROM the published
//! checker address the observation is reported under, an address inside a block
//! `GetCheckerIpRanges` publishes ("a list of the IP ranges used by Amazon Route 53 health
//! checkers to check the health of your resources").** Publishing those blocks exists so a
//! customer can admit the checkers and nobody else, which is the documented way to keep a
//! monitored endpoint off the public internet; so a site whose only grant is conditioned on
//! them is checked normally, and what the checkers report follows the body it serves rather
//! than a refusal. Such a grant is not a PUBLIC policy either — "The Meaning of 'Public'" (the
//! classification `GetBucketPolicyStatus` reports) holds a statement restricted to fixed
//! values of `aws:SourceIp` back from being public — so the Block Public Access settings a new
//! bucket carries, which restrict the grants of a bucket with a public policy, do not withhold
//! it.
//!
//! Two crates have to agree for that to hold (S3 decides the grant and serves the endpoint,
//! Route 53 addresses the probe), so the suite lives here: a crate-local suite can only ever
//! see one side of it.
//!
//! The first table spans the GRANT: the rows differ along the operator (`IpAddress` and its
//! negation), the form of the value (one block, a list of blocks, a single-address block, the
//! whole address space), whether the address the request states is inside the set, whether the
//! statement is guarded at all (an unconditional grant to every principal is a PUBLIC policy,
//! which `RestrictPublicBuckets` does withhold), the Block Public Access posture the site is
//! published under (the four switches a new bucket carries, and turned off), and the two
//! surfaces an anonymous read reaches a bucket through (the website endpoint and the REST
//! object read). The second spans the PROBE: the published checker blocks, a block that
//! publishes no checker, and the body the admitted endpoint serves, rewritten under a health
//! check nobody touched.
//!
//! The XML element reader and the block/address reading are the test crate's own
//! ([`crate::query_wire`]), not another copy.

use std::time::{Duration, Instant};

use bytes::Bytes;
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_route53_awsdnsv20130401::state::endpoint_probe::STATUS_STRING_MATCH_FAILURE;
use service_com_amazonaws_route53_awsdnsv20130401::state::{
    STATUS_FAILURE_PREFIX, STATUS_HTTP_OK, STATUS_SUCCESS_PREFIX,
};

use crate::query_wire::{block_contains, elements};

/// The Region every call here is signed for, and so the Region of the buckets and of the
/// website hosts they are served on.
const REGION: &str = "us-west-2";

/// The XML namespace a client's S3 payload carries.
const S3_XMLNS: &str = "http://s3.amazonaws.com/doc/2006-03-01/";

/// The endpoint host a Route 53 request addresses, which is also the routing signal the
/// router resolves that service from.
const ROUTE53_HOST: &str = "route53.amazonaws.com";

/// The key each site's index document is at, and the string the health checks search the
/// answered body for.
const INDEX_KEY: &str = "index.html";
const MARKER: &str = "MARKER";

/// The header a request states the address it was made from in, whose first entry is the
/// client's own — the one every service of this emulator reads a request's source address
/// from, and the one a checker's probe states its own address in.
const FORWARDED_FOR: &str = "x-forwarded-for";

/// A documentation address range (RFC 5737, "203.0.113.0/24 TEST-NET-3") and one address of
/// it: a range reserved for documentation, so nothing of this emulator and no published
/// health checker is inside it.
const DOCUMENTATION_BLOCK: &str = "203.0.113.0/24";
const DOCUMENTATION_ADDRESS: &str = "203.0.113.9";

/// A second documentation range (RFC 5737, "192.0.2.0/24 TEST-NET-1") and one address of it,
/// for the rows that need two disjoint sets.
const OTHER_DOCUMENTATION_BLOCK: &str = "192.0.2.0/24";
const OTHER_DOCUMENTATION_ADDRESS: &str = "192.0.2.10";

/// The probe cadence the health checks below are created with: the fastest published
/// `RequestInterval` (modelled `range 10..30`) and the smallest `FailureThreshold`
/// (`range 1..10`), so one probe decides what the checkers report.
const REQUEST_INTERVAL: i64 = 10;
const FAILURE_THRESHOLD: i64 = 1;

/// How long a phase waits for the checkers to report the probe of the body it wrote, and how
/// often it looks.
///
/// Generous on purpose: a phase waits for the next probe of a check whose interval is
/// [`REQUEST_INTERVAL`] seconds, so the deadline is only here so a failure is a failure rather
/// than a hang.
const DEADLINE: Duration = Duration::from_secs(120);
const POLL: Duration = Duration::from_millis(100);

/// The whole emulator over one engine, exactly as the binary assembles it — so which service
/// claims each call, the checker's own probe of the website endpoint included, is decided by
/// the production mount rather than by a hand-picked set.
fn router() -> MultiServiceRouter {
    let engine = aws_emulator_router::new_engine();
    let mut router = aws_emulator_router::build_router(engine);
    // Signature verification is the front door's concern, not this suite's: what is under
    // test is which requests a grant conditioned on the requester's address admits, and the
    // anonymous reads below — a browser's, a checker's — sign nothing.
    router.set_request_authentication(false);
    router
}

/// A SigV4 `Authorization` header whose credential scope names `service`, which is what
/// routes the request. The signature is never checked: request authentication is off.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{service}/aws4_request, \
         SignedHeaders=host, Signature=not-verified"
    )
}

/// The website host of `bucket`: one of the two spellings Amazon S3 publishes
/// (`<bucket>.s3-website-<Region>.amazonaws.com`), which is the endpoint a static site is
/// served on.
fn website_host(bucket: &str) -> String {
    format!("{bucket}.s3-website-{REGION}.amazonaws.com")
}

/// One answer: its status and its body as text.
struct Answer {
    status: u16,
    body: String,
}

/// Dispatch one request against S3's REST surface, path-style, as the bucket's OWNER.
async fn s3(router: &MultiServiceRouter, method: &str, path: &str, body: &[u8]) -> Answer {
    let host = format!("s3.{REGION}.amazonaws.com");
    let request = http::Request::builder()
        .method(method)
        .uri(format!("https://{host}{path}"))
        .header("host", host)
        .header("authorization", authorization("s3"))
        .header("content-type", "text/html")
        .body(Bytes::from(body.to_vec()))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    Answer {
        status: response.status().as_u16(),
        body: String::from_utf8_lossy(response.body()).into_owned(),
    }
}

/// One ANONYMOUS request — no `Authorization` header at all — to `host` and `path`, stating
/// `made_from` as the address it was made from where the row names one.
///
/// This is how a browser fetches a static site and how a health checker's probe reaches an
/// endpoint: what it observes is what the bucket grants to everyone.
async fn anonymous(
    router: &MultiServiceRouter,
    host: &str,
    path: &str,
    made_from: Option<&str>,
) -> Answer {
    let mut request = http::Request::builder()
        .method("GET")
        .uri(format!("http://{host}{path}"))
        .header("host", host);
    if let Some(address) = made_from {
        request = request.header(FORWARDED_FOR, address);
    }
    let response = router
        .dispatch(
            request
                .body(Bytes::new())
                .expect("a well-formed request"),
        )
        .await;
    Answer {
        status: response.status().as_u16(),
        body: String::from_utf8_lossy(response.body()).into_owned(),
    }
}

/// What a site does with the Block Public Access settings a NEW bucket carries: "Amazon S3
/// automatically enables S3 Block Public Access … for new buckets" (Amazon S3 User Guide,
/// *Blocking public access to your Amazon S3 storage*), so all four switches are on unless a
/// client turns them off.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Blocking {
    /// The settings are left exactly as the create made them — the posture the recorded
    /// configuration publishes its site under, and the one a grant restricted to a set of
    /// addresses is the documented way through.
    AsCreated,
    /// All four switches turned off, which is what a site published to EVERYONE needs.
    Off,
    /// Turned off while the policy is put and turned back on afterwards: the one way to have a
    /// bucket carrying a PUBLIC policy while `RestrictPublicBuckets` is on, since
    /// `BlockPublicPolicy` refuses the put itself.
    OffWhileThePolicyIsPut,
}

/// The `PublicAccessBlockConfiguration` payload with every switch set to `on`.
fn public_access_block(on: bool) -> String {
    format!(
        "<PublicAccessBlockConfiguration>\
         <BlockPublicAcls>{on}</BlockPublicAcls><IgnorePublicAcls>{on}</IgnorePublicAcls>\
         <BlockPublicPolicy>{on}</BlockPublicPolicy>\
         <RestrictPublicBuckets>{on}</RestrictPublicBuckets>\
         </PublicAccessBlockConfiguration>"
    )
}

/// `PUT /<bucket>?publicAccessBlock` with every switch set to `on`.
async fn put_public_access_block(
    router: &MultiServiceRouter,
    case: &str,
    bucket: &str,
    on: bool,
) {
    let answered = s3(
        router,
        "PUT",
        &format!("/{bucket}?publicAccessBlock"),
        public_access_block(on).as_bytes(),
    )
    .await;
    assert_eq!(
        answered.status, 200,
        "case {case:?}: PutPublicAccessBlock({on}) must succeed, got {} with {}",
        answered.status, answered.body
    );
}

/// Stand up one static site: the bucket, its public-access settings as `blocking` describes,
/// the bucket policy `policy`, its website configuration and the index document carrying
/// `body`.
///
/// Every call is the owner's, so the site is real before anything is asserted about what an
/// anonymous reader — or a checker — is served.
async fn publish_site(
    router: &MultiServiceRouter,
    case: &str,
    bucket: &str,
    policy: &str,
    body: &str,
    blocking: Blocking,
) {
    let created = s3(
        router,
        "PUT",
        &format!("/{bucket}"),
        format!(
            "<CreateBucketConfiguration xmlns=\"{S3_XMLNS}\">\
             <LocationConstraint>{REGION}</LocationConstraint></CreateBucketConfiguration>"
        )
        .as_bytes(),
    )
    .await;
    assert_eq!(
        created.status, 200,
        "case {case:?}: CreateBucket({bucket:?}) must succeed, got {} with {}",
        created.status, created.body
    );
    if blocking != Blocking::AsCreated {
        put_public_access_block(router, case, bucket, false).await;
    }
    let written = s3(router, "PUT", &format!("/{bucket}?policy"), policy.as_bytes()).await;
    assert_eq!(
        written.status, 204,
        "case {case:?}: PutBucketPolicy must succeed, got {} with {}",
        written.status, written.body
    );
    let site = s3(
        router,
        "PUT",
        &format!("/{bucket}?website"),
        format!(
            "<WebsiteConfiguration><IndexDocument><Suffix>{INDEX_KEY}</Suffix></IndexDocument>\
             </WebsiteConfiguration>"
        )
        .as_bytes(),
    )
    .await;
    assert_eq!(
        site.status, 200,
        "case {case:?}: PutBucketWebsite must succeed, got {} with {}",
        site.status, site.body
    );
    if blocking == Blocking::OffWhileThePolicyIsPut {
        put_public_access_block(router, case, bucket, true).await;
    }
    serve_body(router, case, bucket, body).await;
}

/// Rewrite one site's index document so it serves `body`, and prove through the OWNER's own
/// read that the endpoint really holds it — the control that keeps a refusal of an anonymous
/// read from being read as an endpoint that serves nothing.
async fn serve_body(router: &MultiServiceRouter, case: &str, bucket: &str, body: &str) {
    let written = s3(
        router,
        "PUT",
        &format!("/{bucket}/{INDEX_KEY}"),
        body.as_bytes(),
    )
    .await;
    assert_eq!(
        written.status, 200,
        "case {case:?}: PutObject must succeed, got {} with {}",
        written.status, written.body
    );
    let read = s3(router, "GET", &format!("/{bucket}/{INDEX_KEY}"), b"").await;
    assert_eq!(
        (read.status, read.body.as_str()),
        (200, body),
        "case {case:?}: the owner's own read must serve the body that was just written, got {} \
         with {:?}",
        read.status,
        read.body
    );
}

/// A bucket policy whose single statement allows `s3:GetObject` on every object of `bucket`
/// to EVERY principal, guarded by `condition` where the row names one — the shape a site
/// published to a set of addresses carries, and, with no condition at all, the shape of a
/// PUBLIC policy.
fn published_policy(bucket: &str, condition: Option<&str>) -> String {
    let guard = condition.map_or_else(String::new, |condition| format!(",\"Condition\":{condition}"));
    format!(
        "{{\"Version\":\"2012-10-17\",\"Statement\":[\
         {{\"Sid\":\"ReadFromTheAdmittedAddresses\",\"Effect\":\"Allow\",\"Principal\":\"*\",\
         \"Action\":\"s3:GetObject\",\"Resource\":\"arn:aws:s3:::{bucket}/*\"{guard}}}]}}"
    )
}

/// One row of the grant table: the condition the publishing statement carries, the Block
/// Public Access posture the site is published under, the address the anonymous read states it
/// was made from, and whether the site serves it.
struct GrantRow {
    case: &'static str,
    bucket: &'static str,
    /// The guard on the grant, or `None` for an unconditional grant to every principal — a
    /// PUBLIC policy.
    condition: Option<String>,
    blocking: Blocking,
    made_from: Option<&'static str>,
    served: bool,
}

#[tokio::test]
async fn a_grant_conditioned_on_the_requesters_address_admits_exactly_the_addresses_it_names() {
    let router = router();
    let body = format!("<html>{MARKER}</html>");
    let rows: Vec<GrantRow> = vec![
        GrantRow {
            case: "one block, read from an address inside it",
            bucket: "address-grant-inside",
            condition: Some(format!(
                "{{\"IpAddress\":{{\"aws:SourceIp\":\"{DOCUMENTATION_BLOCK}\"}}}}"
            )),
            blocking: Blocking::AsCreated,
            made_from: Some(DOCUMENTATION_ADDRESS),
            served: true,
        },
        GrantRow {
            case: "one block, read from an address outside it",
            bucket: "address-grant-outside",
            condition: Some(format!(
                "{{\"IpAddress\":{{\"aws:SourceIp\":\"{DOCUMENTATION_BLOCK}\"}}}}"
            )),
            blocking: Blocking::AsCreated,
            made_from: Some(OTHER_DOCUMENTATION_ADDRESS),
            served: false,
        },
        GrantRow {
            case: "a list of blocks, read from an address inside the second",
            bucket: "address-grant-list",
            condition: Some(format!(
                "{{\"IpAddress\":{{\"aws:SourceIp\":\
                 [\"{DOCUMENTATION_BLOCK}\",\"{OTHER_DOCUMENTATION_BLOCK}\"]}}}}"
            )),
            blocking: Blocking::AsCreated,
            made_from: Some(OTHER_DOCUMENTATION_ADDRESS),
            served: true,
        },
        GrantRow {
            case: "a single-address block, read from that address",
            bucket: "address-grant-one-address",
            condition: Some(format!(
                "{{\"IpAddress\":{{\"aws:SourceIp\":\"{DOCUMENTATION_ADDRESS}/32\"}}}}"
            )),
            blocking: Blocking::AsCreated,
            made_from: Some(DOCUMENTATION_ADDRESS),
            served: true,
        },
        GrantRow {
            case: "a single-address block, read from the address next to it",
            bucket: "address-grant-one-address-missed",
            condition: Some(format!(
                "{{\"IpAddress\":{{\"aws:SourceIp\":\"{DOCUMENTATION_ADDRESS}/32\"}}}}"
            )),
            blocking: Blocking::AsCreated,
            made_from: Some("203.0.113.10"),
            served: false,
        },
        GrantRow {
            case: "an address-restricted grant with the four switches turned back on",
            bucket: "address-grant-blocked",
            condition: Some(format!(
                "{{\"IpAddress\":{{\"aws:SourceIp\":\"{DOCUMENTATION_BLOCK}\"}}}}"
            )),
            blocking: Blocking::OffWhileThePolicyIsPut,
            made_from: Some(DOCUMENTATION_ADDRESS),
            served: true,
        },
        GrantRow {
            case: "the whole address space, read from a request stating no address at all",
            bucket: "address-grant-everywhere",
            condition: Some("{\"IpAddress\":{\"aws:SourceIp\":\"0.0.0.0/0\"}}".to_string()),
            blocking: Blocking::Off,
            made_from: None,
            served: true,
        },
        GrantRow {
            case: "the negated operator, read from an address inside the named block",
            bucket: "address-grant-not-inside",
            condition: Some(format!(
                "{{\"NotIpAddress\":{{\"aws:SourceIp\":\"{DOCUMENTATION_BLOCK}\"}}}}"
            )),
            blocking: Blocking::Off,
            made_from: Some(DOCUMENTATION_ADDRESS),
            served: false,
        },
        GrantRow {
            case: "the negated operator, read from an address outside the named block",
            bucket: "address-grant-not-outside",
            condition: Some(format!(
                "{{\"NotIpAddress\":{{\"aws:SourceIp\":\"{DOCUMENTATION_BLOCK}\"}}}}"
            )),
            blocking: Blocking::Off,
            made_from: Some(OTHER_DOCUMENTATION_ADDRESS),
            served: true,
        },
        GrantRow {
            case: "an unconditional grant to everyone, with public access unblocked",
            bucket: "address-grant-public-open",
            condition: None,
            blocking: Blocking::Off,
            made_from: Some(DOCUMENTATION_ADDRESS),
            served: true,
        },
        GrantRow {
            case: "an unconditional grant to everyone, under RestrictPublicBuckets",
            bucket: "address-grant-public-restricted",
            condition: None,
            blocking: Blocking::OffWhileThePolicyIsPut,
            made_from: Some(DOCUMENTATION_ADDRESS),
            served: false,
        },
    ];

    for row in &rows {
        let (case, bucket) = (row.case, row.bucket);
        publish_site(
            &router,
            case,
            bucket,
            &published_policy(bucket, row.condition.as_deref()),
            &body,
            row.blocking,
        )
        .await;
        // BOTH surfaces an anonymous reader reaches the object through are decided by the one
        // grant: the website endpoint the site is published on, and the REST object read.
        let surfaces: [(&str, String, String); 2] = [
            ("the website endpoint", website_host(bucket), "/".to_string()),
            (
                "the REST object read",
                format!("s3.{REGION}.amazonaws.com"),
                format!("/{bucket}/{INDEX_KEY}"),
            ),
        ];
        for (surface, host, path) in surfaces {
            let answer = anonymous(&router, &host, &path, row.made_from).await;
            if row.served {
                assert_eq!(
                    (answer.status, answer.body.as_str()),
                    (200, body.as_str()),
                    "case {case:?} ({surface}, made from {:?}): expected the site to serve the \
                     published body, got {} with {:?}",
                    row.made_from,
                    answer.status,
                    answer.body
                );
            } else {
                assert_eq!(
                    answer.status, 403,
                    "case {case:?} ({surface}, made from {:?}): expected the read to be refused \
                     403, got {} with {:?}",
                    row.made_from, answer.status, answer.body
                );
            }
        }
    }
}

/// Call one Route 53 operation over its restXml wire, requiring the status `expected`.
async fn route53(
    router: &MultiServiceRouter,
    case: &str,
    method: &str,
    uri: &str,
    body: &str,
    expected: u16,
) -> String {
    let request = http::Request::builder()
        .method(method)
        .uri(format!("https://{ROUTE53_HOST}{uri}"))
        .header("host", ROUTE53_HOST)
        .header("authorization", authorization("route53"))
        .header("content-type", "application/xml")
        .body(Bytes::from(body.to_owned()))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let read = String::from_utf8_lossy(response.body()).to_string();
    assert_eq!(
        status, expected,
        "case {case:?}: expected HTTP {expected} from Route 53 {method} {uri}, got {status} \
         with body {read}"
    );
    read
}

/// The blocks `GetCheckerIpRanges` publishes, as a client reads them.
async fn checker_ip_ranges(router: &MultiServiceRouter) -> Vec<String> {
    let read = route53(
        router,
        "the published checker blocks",
        "GET",
        "/2013-04-01/checkeripranges",
        "",
        200,
    )
    .await;
    let blocks = elements(&read, "member");
    assert!(
        !blocks.is_empty(),
        "GetCheckerIpRanges must publish the blocks its checkers check from, got {read}"
    );
    blocks
}

/// `CreateHealthCheck` of an `HTTP_STR_MATCH` check of `host`, answering the id it minted.
async fn create_string_match_check(
    router: &MultiServiceRouter,
    case: &str,
    caller_reference: &str,
    host: &str,
) -> String {
    let created = route53(
        router,
        case,
        "POST",
        "/2013-04-01/healthcheck",
        &format!(
            "<CreateHealthCheckRequest xmlns=\"https://route53.amazonaws.com/doc/2013-04-01/\">\
             <CallerReference>{caller_reference}</CallerReference><HealthCheckConfig>\
             <Type>HTTP_STR_MATCH</Type>\
             <FullyQualifiedDomainName>{host}</FullyQualifiedDomainName><Port>80</Port>\
             <ResourcePath>/</ResourcePath><SearchString>{MARKER}</SearchString>\
             <RequestInterval>{REQUEST_INTERVAL}</RequestInterval>\
             <FailureThreshold>{FAILURE_THRESHOLD}</FailureThreshold>\
             <MeasureLatency>false</MeasureLatency><Inverted>false</Inverted>\
             </HealthCheckConfig></CreateHealthCheckRequest>"
        ),
        201,
    )
    .await;
    elements(&created, "Id")
        .into_iter()
        .next()
        .unwrap_or_else(|| panic!("case {case:?}: the create must report the new id, got {created}"))
}

/// What every checker of `id` is reporting: the `StatusReport$Status` and the
/// `HealthCheckObservation$IPAddress` of each observation, in wire order.
async fn observations(router: &MultiServiceRouter, case: &str, id: &str) -> (Vec<String>, Vec<String>) {
    let read = route53(
        router,
        case,
        "GET",
        &format!("/2013-04-01/healthcheck/{id}/status"),
        "",
        200,
    )
    .await;
    (elements(&read, "Status"), elements(&read, "IPAddress"))
}

/// Wait until every checker of `id` reports a status opening with `prefix`, and answer what
/// they report.
///
/// What is waited for is the CONDITION — the checkers reporting the probe of the body the
/// phase wrote — rather than a span of time.
async fn observations_opening_with(
    router: &MultiServiceRouter,
    case: &str,
    id: &str,
    prefix: &str,
) -> (Vec<String>, Vec<String>) {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let (statuses, addresses) = observations(router, case, id).await;
        let settled =
            !statuses.is_empty() && statuses.iter().all(|status| status.starts_with(prefix));
        if settled || Instant::now() >= deadline {
            assert!(
                settled,
                "case {case:?}: expected every checker to report a status opening with \
                 {prefix:?}, got {statuses:?}"
            );
            return (statuses, addresses);
        }
        ::tokio::time::sleep(POLL).await;
    }
}

/// The `HealthCheckVersion` and the `SearchString` the health check `id` carries, which no
/// rewrite of the monitored body may change.
async fn version_and_search_string(
    router: &MultiServiceRouter,
    case: &str,
    id: &str,
) -> (String, String) {
    let read = route53(
        router,
        case,
        "GET",
        &format!("/2013-04-01/healthcheck/{id}"),
        "",
        200,
    )
    .await;
    let version = elements(&read, "HealthCheckVersion")
        .into_iter()
        .next()
        .unwrap_or_default();
    let search = elements(&read, "SearchString")
        .into_iter()
        .next()
        .unwrap_or_default();
    (version, search)
}

/// One phase of the gated site: the body it is rewritten to serve, and whether the string the
/// health check searches for appears in it.
struct Phase {
    case: &'static str,
    body: &'static str,
    carries_the_string: bool,
}

#[tokio::test]
async fn a_site_that_admits_the_published_checker_blocks_is_checked_by_its_body() {
    let router = router();
    let blocks = checker_ip_ranges(&router).await;
    // The site the check monitors admits EXACTLY the published checker blocks and nobody
    // else, which is the configuration the publication of those blocks exists for — and it
    // keeps the Block Public Access settings the create gave it, because a grant restricted to
    // a set of addresses is not a public policy and is the documented way to publish content
    // to named networks with those switches on.
    let admitted = "checker-admitted-site";
    let quoted: Vec<String> = blocks.iter().map(|block| format!("\"{block}\"")).collect();
    publish_site(
        &router,
        "a site admitting the published checker blocks",
        admitted,
        &published_policy(
            admitted,
            Some(&format!(
                "{{\"IpAddress\":{{\"aws:SourceIp\":[{}]}}}}",
                quoted.join(",")
            )),
        ),
        &format!("<html>{MARKER}</html>"),
        Blocking::AsCreated,
    )
    .await;
    // And a control site admitting one documentation block instead: no checker checks from
    // inside it, so its probe is refused however the body reads.
    let refused = "checker-refused-site";
    publish_site(
        &router,
        "a site admitting a block no checker checks from",
        refused,
        &published_policy(
            refused,
            Some(&format!(
                "{{\"IpAddress\":{{\"aws:SourceIp\":\"{DOCUMENTATION_BLOCK}\"}}}}"
            )),
        ),
        &format!("<html>{MARKER}</html>"),
        Blocking::AsCreated,
    )
    .await;

    let admitted_check = create_string_match_check(
        &router,
        "a string-match check of the site that admits the checkers",
        "checker-admitted",
        &website_host(admitted),
    )
    .await;
    let refused_check = create_string_match_check(
        &router,
        "a string-match check of the site that does not",
        "checker-refused",
        &website_host(refused),
    )
    .await;
    let configured = version_and_search_string(&router, "as created", &admitted_check).await;

    // Phase by phase, the SAME key is rewritten and nothing else: no call is made to Route 53
    // between the write and the read, so what changes the reported status is the body alone.
    // The phases alternate, so the status a phase waits for can only come from a probe of the
    // body that phase wrote.
    let phases: &[Phase] = &[
        Phase {
            case: "the body the site was published with, which carries the string",
            body: "<html>MARKER</html>",
            carries_the_string: true,
        },
        Phase {
            case: "a body that no longer carries the string",
            body: "<html>maintenance in progress</html>",
            carries_the_string: false,
        },
        Phase {
            case: "the string restored",
            body: "<html><p>MARKER</p></html>",
            carries_the_string: true,
        },
    ];
    for phase in phases {
        serve_body(&router, phase.case, admitted, phase.body).await;
        let prefix = if phase.carries_the_string {
            STATUS_SUCCESS_PREFIX
        } else {
            STATUS_FAILURE_PREFIX
        };
        let (statuses, addresses) =
            observations_opening_with(&router, phase.case, &admitted_check, prefix).await;
        assert!(
            statuses.len() >= 3,
            "case {:?}: the checkers reporting on an endpoint are the health-checking Regions, \
             got {statuses:?}",
            phase.case
        );
        assert_eq!(
            addresses.len(),
            statuses.len(),
            "case {:?}: every observation names the checker's own address, got {addresses:?} \
             against {statuses:?}",
            phase.case
        );
        // Every checker reports from an address inside a block the policy admitted, which is
        // what makes the probe it describes one the site answered at all.
        for address in &addresses {
            assert!(
                blocks.iter().any(|block| block_contains(block, address)),
                "case {:?}: expected the checker address {address:?} inside a published block, \
                 got none of {blocks:?}",
                phase.case
            );
        }
        for status in &statuses {
            if phase.carries_the_string {
                // The answered probe is described by the answer it got: the status the
                // endpoint served, not a refusal.
                assert_eq!(
                    status, STATUS_HTTP_OK,
                    "case {:?}: expected the description of an answered 200, got {status:?}",
                    phase.case
                );
            } else {
                // A body that does not carry the string is a different fact about the endpoint
                // from a connection that could not be made or an answer the request rejected.
                assert_eq!(
                    status, STATUS_STRING_MATCH_FAILURE,
                    "case {:?}: expected the string-match failure, got {status:?}",
                    phase.case
                );
            }
        }
        // Nothing about the health check itself moved across the flip.
        let after = version_and_search_string(&router, phase.case, &admitted_check).await;
        assert_eq!(
            after, configured,
            "case {:?}: rewriting the monitored body must leave the health check itself \
             untouched, got {after:?} against {configured:?}",
            phase.case
        );
    }

    // The control: the site that admits no checker's address refuses the probe, and that
    // refusal is what every checker reports — the status the endpoint answered, never the
    // string match, whose body the checker never got to search.
    let (refused_statuses, _) = observations_opening_with(
        &router,
        "a site admitting a block no checker checks from",
        &refused_check,
        STATUS_FAILURE_PREFIX,
    )
    .await;
    for status in &refused_statuses {
        assert!(
            status.contains("403") && *status != STATUS_STRING_MATCH_FAILURE,
            "a probe the endpoint refused is reported as that answer, got {status:?}"
        );
    }
    // And that site really does serve the body to an address it admits, so what the checkers
    // report is the refusal of the CHECKER's address and not an endpoint serving nothing.
    let served = anonymous(
        &router,
        &website_host(refused),
        "/",
        Some(DOCUMENTATION_ADDRESS),
    )
    .await;
    assert_eq!(
        served.status, 200,
        "the control site must serve a reader it admits, got {} with {:?}",
        served.status, served.body
    );
}
