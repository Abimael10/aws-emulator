// SPDX-License-Identifier: CC0-1.0

//! What a Route 53 health checker reports about an `HTTP` / `HTTPS` health check of an
//! endpoint this emulator SERVES — a bucket serving a static website — and what happens to
//! that report when `UpdateHealthCheck` REPOINTS the live check at another path of the same
//! endpoint.
//!
//! The rule under test: **a checker's `StatusReport$Status` describes the LATEST probe of the
//! endpoint the health check's CURRENT configuration names, and for the request types the
//! criterion that probe applies is the documented status range — Route 53 "submits an HTTP
//! request and waits for an HTTP status code of 200 or greater and less than 400"
//! (`HealthCheckConfig$Type`) — so an answer inside that range is reported as a `Success: `
//! description naming the answered status and an answer outside it as a `Failure: ` one; and
//! because `ResourcePath` is "the path, if any, that you want Amazon Route 53 to request when
//! performing health checks", a write that changes it moves the probe to the new path, so what
//! every checker reports follows the answer THAT path gives.**
//! `GetHealthCheckLastFailureReason` reports the checkers whose latest observation is such a
//! failure, so the two readers of one health check describe one probe rather than two.
//!
//! The sibling `health_check_endpoint_probe` suite pins what a checker reports for an endpoint
//! this emulator serves NOTHING on, and `endpoint_probe`'s own tables pin the predicate over
//! statuses and bodies. What only this suite reaches is the whole path for the plain request
//! types — an endpoint of this emulator answering the probe, deciding what the checkers
//! report — and a check repointed while it is LIVE: the phase a client watching for a
//! monitored path to break is waiting for. Amazon S3 serves that endpoint, so it is mounted
//! beside Route 53 on the one shared engine; a suite with only this service mounted can see
//! just the unreachable half of the rule.
//!
//! The rows of the status table differ along what the rule generalises over: whether the probed
//! path is one the site serves or one it does not, the shape of that path (the site's root, a
//! document, a document asked for with a query string, a path nested under directories the site
//! does not hold), and which request type is configured — `HTTP` on the port its scheme is
//! served on and `HTTPS` on 443. The repointing table then moves ONE live check across paths
//! that ALTERNATE between served and not served, so each phase's report can only come from a
//! probe of the path that phase stored.
//!
//! The description each row expects is computed from what the monitored endpoint answers an
//! ordinary anonymous client — fetched here exactly as a browser fetches a static site, which is
//! also exactly what a checker's probe is — rather than from a value written into the suite; the
//! `200` case is additionally held to the wording the service is recorded reporting for it
//! (`STATUS_HTTP_OK`).

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_route53_awsdnsv20130401::ops::types::HEALTH_CHECK_REGION_VALUES;
use service_com_amazonaws_route53_awsdnsv20130401::state::{
    STATUS_FAILURE_PREFIX, STATUS_HTTP_OK, STATUS_SUCCESS_PREFIX,
};

use crate::health_check_endpoint_probe::{last_failure_observations, status_observations};
use crate::health_check_updates::update_health_check;
use crate::health_checks::{create_health_check, get_health_check, health_check_id};
use crate::hosted_zone_records::Reply;

/// The Region the monitored bucket is in, and so the Region of the website host it is served
/// on.
const REGION: &str = "us-west-2";

/// The XML namespace a client's S3 payload carries.
const S3_XMLNS: &str = "http://s3.amazonaws.com/doc/2006-03-01/";

/// The bucket serving the monitored site, the key its index document is at, and a second
/// document it holds — the paths a probe of this site is answered 2xx from.
const BUCKET: &str = "route53-probe-status-origin";
const INDEX_KEY: &str = "index.html";
const SERVED_KEY: &str = "healthy.html";

/// The probe cadence every check here is created with: the fastest published `RequestInterval`
/// (modelled `range 10..30`) and the smallest `FailureThreshold` (`range 1..10`), so one probe
/// decides what the checkers report.
const REQUEST_INTERVAL: i64 = 10;
const FAILURE_THRESHOLD: i64 = 1;

/// The lowest and the first excluded status of an answer the request types accept:
/// `HealthCheckConfig$Type` — Route 53 "waits for an HTTP status code of 200 or greater and
/// less than 400".
const LOWEST_ACCEPTED_STATUS: u16 = 200;
const FIRST_REJECTED_STATUS: u16 = 400;

/// How long a phase waits for the checkers to report the probe of the path it pointed the check
/// at, and how often it looks.
///
/// Generous on purpose: what a phase waits for is the CONDITION — the checkers reporting that
/// probe — and the deadline is only here so a failure is a failure rather than a hang.
const DEADLINE: Duration = Duration::from_secs(60);
const POLL: Duration = Duration::from_millis(50);

/// A router with Route 53 and Amazon S3 mounted on ONE shared engine, which is how the
/// assembled router mounts them: the checker's probe of the website endpoint is served by the
/// S3 crate over the same in-process transport a client's request takes.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.register_service(
        service_com_amazonaws_route53_awsdnsv20130401::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(service_com_amazonaws_s3_amazons3::EmulatedService::new(
        engine,
    ));
    router
}

/// The website host of the monitored bucket: one of the two spellings Amazon S3 publishes
/// (`<bucket>.s3-website-<Region>.amazonaws.com`), which is the endpoint a static site is
/// served on.
fn website_host() -> String {
    format!("{BUCKET}.s3-website-{REGION}.amazonaws.com")
}

/// One answer: its status and its body as text.
struct Answer {
    status: u16,
    body: String,
}

/// A SigV4 `Authorization` header whose credential scope names Amazon S3, which is what routes
/// the request. The signature is never checked by this harness.
fn s3_authorization() -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/s3/aws4_request, \
         SignedHeaders=host, Signature=not-verified"
    )
}

/// Dispatch one request against S3's REST surface, path-style.
async fn s3(router: &MultiServiceRouter, method: &str, path: &str, body: &[u8]) -> Answer {
    let host = format!("s3.{REGION}.amazonaws.com");
    let request = http::Request::builder()
        .method(method)
        .uri(format!("https://{host}{path}"))
        .header(http::header::HOST, host)
        .header(http::header::AUTHORIZATION, s3_authorization())
        .header(http::header::CONTENT_TYPE, "text/html")
        .body(Bytes::from(body.to_vec()))
        .expect("the test request must build");
    let response = router.dispatch(request).await;
    Answer {
        status: response.status().as_u16(),
        body: String::from_utf8_lossy(response.body()).into_owned(),
    }
}

/// One ANONYMOUS request to the bucket's WEBSITE host over `scheme` — the endpoint the health
/// checks below monitor, fetched here exactly as a browser addressing a static site fetches it,
/// and exactly as a health checker's probe does: no `Authorization` header at all.
async fn website(router: &MultiServiceRouter, scheme: &str, path: &str) -> Answer {
    let host = website_host();
    let request = http::Request::builder()
        .method("GET")
        .uri(format!("{scheme}://{host}{path}"))
        .header(http::header::HOST, host)
        .body(Bytes::new())
        .expect("the test request must build");
    let response = router.dispatch(request).await;
    Answer {
        status: response.status().as_u16(),
        body: String::from_utf8_lossy(response.body()).into_owned(),
    }
}

/// Stand up the monitored site: the bucket, the public-access settings and bucket policy that
/// publish it, its website configuration, and the two documents it serves.
async fn publish_site(router: &MultiServiceRouter) {
    let created = s3(
        router,
        "PUT",
        &format!("/{BUCKET}"),
        format!(
            "<CreateBucketConfiguration xmlns=\"{S3_XMLNS}\">\
             <LocationConstraint>{REGION}</LocationConstraint></CreateBucketConfiguration>"
        )
        .as_bytes(),
    )
    .await;
    assert_eq!(
        created.status, 200,
        "CreateBucket must succeed, got {} with {}",
        created.status, created.body
    );
    let unblocked = s3(
        router,
        "PUT",
        &format!("/{BUCKET}?publicAccessBlock"),
        b"<PublicAccessBlockConfiguration>\
          <BlockPublicAcls>false</BlockPublicAcls>\
          <IgnorePublicAcls>false</IgnorePublicAcls>\
          <BlockPublicPolicy>false</BlockPublicPolicy>\
          <RestrictPublicBuckets>false</RestrictPublicBuckets>\
          </PublicAccessBlockConfiguration>",
    )
    .await;
    assert_eq!(
        unblocked.status, 200,
        "PutPublicAccessBlock must succeed, got {} with {}",
        unblocked.status, unblocked.body
    );
    let policy = s3(
        router,
        "PUT",
        &format!("/{BUCKET}?policy"),
        format!(
            "{{\"Version\":\"2012-10-17\",\"Statement\":[\
             {{\"Sid\":\"PublicRead\",\"Effect\":\"Allow\",\"Principal\":\"*\",\
             \"Action\":\"s3:GetObject\",\"Resource\":\"arn:aws:s3:::{BUCKET}/*\"}}]}}"
        )
        .as_bytes(),
    )
    .await;
    assert_eq!(
        policy.status, 204,
        "PutBucketPolicy must succeed, got {} with {}",
        policy.status, policy.body
    );
    let site = s3(
        router,
        "PUT",
        &format!("/{BUCKET}?website"),
        format!(
            "<WebsiteConfiguration><IndexDocument><Suffix>{INDEX_KEY}</Suffix></IndexDocument>\
             </WebsiteConfiguration>"
        )
        .as_bytes(),
    )
    .await;
    assert_eq!(
        site.status, 200,
        "PutBucketWebsite must succeed, got {} with {}",
        site.status, site.body
    );
    for key in [INDEX_KEY, SERVED_KEY] {
        let written = s3(
            router,
            "PUT",
            &format!("/{BUCKET}/{key}"),
            format!("<html>served from {key}</html>").as_bytes(),
        )
        .await;
        assert_eq!(
            written.status, 200,
            "PutObject({key:?}) must succeed, got {} with {}",
            written.status, written.body
        );
    }
}

/// `CreateHealthCheck` of a check of `kind` on `port` pointed at `path` of the monitored site,
/// answering the id it minted.
async fn create_check(
    router: &MultiServiceRouter,
    case: &str,
    caller_reference: &str,
    kind: &str,
    port: u16,
    path: &str,
) -> String {
    let created = create_health_check(
        router,
        caller_reference,
        &format!(
            "<Type>{kind}</Type>\
             <FullyQualifiedDomainName>{}</FullyQualifiedDomainName><Port>{port}</Port>\
             <ResourcePath>{path}</ResourcePath>\
             <RequestInterval>{REQUEST_INTERVAL}</RequestInterval>\
             <FailureThreshold>{FAILURE_THRESHOLD}</FailureThreshold>",
            website_host()
        ),
    )
    .await;
    health_check_id(&created, case)
}

/// The `ResourcePath` and `HealthCheckVersion` a reply carrying one health check reports.
fn path_and_version(reply: &Reply, case: &str) -> (String, i64) {
    let path = reply
        .first("HealthCheckConfig/ResourcePath")
        .unwrap_or_else(|| {
            panic!(
                "case {case:?}: the reply must report the configured ResourcePath; body {}",
                reply.body
            )
        })
        .to_string();
    let version = reply
        .first("HealthCheck/HealthCheckVersion")
        .and_then(|text| text.parse().ok())
        .unwrap_or_else(|| {
            panic!(
                "case {case:?}: the reply must report a numeric HealthCheckVersion; body {}",
                reply.body
            )
        });
    (path, version)
}

/// The `ResourcePath` and `HealthCheckVersion` `GetHealthCheck` reports for `id`.
async fn configured_path_and_version(
    router: &MultiServiceRouter,
    case: &str,
    id: &str,
) -> (String, i64) {
    let read = get_health_check(router, id).await;
    assert_eq!(
        read.status, 200,
        "case {case:?}: expected 200 from GetHealthCheck, got {} with body {}",
        read.status, read.body
    );
    path_and_version(&read, case)
}

/// Wait until every checker of `id` reports `expected`, and answer the statuses they report.
///
/// What is waited for is the CONDITION — the checkers reporting the probe of the path the phase
/// pointed the check at — rather than a span of time.
async fn statuses_settling_on(
    router: &MultiServiceRouter,
    case: &str,
    id: &str,
    expected: &str,
) -> Vec<String> {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let reported: Vec<String> = status_observations(router, id, case)
            .await
            .into_iter()
            .map(|observation| observation.status)
            .collect();
        let settled = !reported.is_empty() && reported.iter().all(|status| status == expected);
        if settled || Instant::now() >= deadline {
            assert!(
                settled,
                "case {case:?}: expected every checker to report {expected:?}, got {reported:?}"
            );
            return reported;
        }
        ::tokio::time::sleep(POLL).await;
    }
}

/// The description a checker reports for an answer of `status`: the opening the probe's verdict
/// is reported under — the request types accept a status of
/// `LOWEST_ACCEPTED_STATUS..FIRST_REJECTED_STATUS` — and the status as the status line names
/// it.
fn description_of(status: u16) -> String {
    let prefix = if (LOWEST_ACCEPTED_STATUS..FIRST_REJECTED_STATUS).contains(&status) {
        STATUS_SUCCESS_PREFIX
    } else {
        STATUS_FAILURE_PREFIX
    };
    match http::StatusCode::from_u16(status)
        .ok()
        .and_then(|code| code.canonical_reason())
    {
        Some(reason) => format!("{prefix}HTTP Status Code {status}, {reason}"),
        None => format!("{prefix}HTTP Status Code {status}"),
    }
}

/// What the monitored endpoint answers an anonymous client asking for `path` over `scheme`,
/// with the row's own expectation about that answer checked first: the oracle this suite
/// compares the checkers against is the endpoint's REAL answer, so it is pinned before it is
/// used.
async fn answered_status(
    router: &MultiServiceRouter,
    case: &str,
    scheme: &str,
    path: &str,
    accepted: bool,
) -> u16 {
    let served = website(router, scheme, path).await;
    let inside = (LOWEST_ACCEPTED_STATUS..FIRST_REJECTED_STATUS).contains(&served.status);
    assert_eq!(
        inside, accepted,
        "case {case:?}: expected the monitored endpoint to answer {path:?} {} the range the \
         request types accept, got {} with {:?}",
        if accepted { "inside" } else { "outside" },
        served.status,
        served.body
    );
    served.status
}

/// One row of the status table: a request type, the port and scheme it is reached on, the path
/// the check probes and whether the site answers that path inside the accepted range.
struct Row {
    case: &'static str,
    kind: &'static str,
    scheme: &'static str,
    port: u16,
    path: &'static str,
    accepted: bool,
}

#[tokio::test]
async fn the_status_the_probed_path_answers_decides_what_every_checker_reports() {
    let router = emulator();
    publish_site(&router).await;

    let rows: &[Row] = &[
        Row {
            case: "the index document, reached at the site's root",
            kind: "HTTP",
            scheme: "http",
            port: 80,
            path: "/",
            accepted: true,
        },
        Row {
            case: "a document the site holds",
            kind: "HTTP",
            scheme: "http",
            port: 80,
            path: "/healthy.html",
            accepted: true,
        },
        Row {
            case: "a document the site holds, asked for with a query string",
            kind: "HTTP",
            scheme: "http",
            port: 80,
            path: "/healthy.html?probe=1",
            accepted: true,
        },
        Row {
            case: "a document the site does not hold",
            kind: "HTTP",
            scheme: "http",
            port: 80,
            path: "/absent.html",
            accepted: false,
        },
        Row {
            case: "a path under directories the site does not hold",
            kind: "HTTP",
            scheme: "http",
            port: 80,
            path: "/deep/missing/page.html",
            accepted: false,
        },
        Row {
            case: "a document the site holds, over the TLS request type",
            kind: "HTTPS",
            scheme: "https",
            port: 443,
            path: "/healthy.html",
            accepted: true,
        },
        Row {
            case: "a document the site does not hold, over the TLS request type",
            kind: "HTTPS",
            scheme: "https",
            port: 443,
            path: "/absent.html",
            accepted: false,
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        let case = row.case;
        let status = answered_status(&router, case, row.scheme, row.path, row.accepted).await;
        let expected = description_of(status);
        if status == 200 {
            // The wording the service is recorded reporting for an answered 200, read from the
            // one constant the emulator reports it from rather than spelled a second time here.
            assert_eq!(
                expected, STATUS_HTTP_OK,
                "case {case:?}: an answered 200 is reported in the recorded wording"
            );
        }
        let id = create_check(
            &router,
            case,
            &format!("probe-status-{index}"),
            row.kind,
            row.port,
            row.path,
        )
        .await;
        let reported = statuses_settling_on(&router, case, &id, &expected).await;
        assert_eq!(
            reported.len(),
            HEALTH_CHECK_REGION_VALUES.len(),
            "case {case:?}: every health-checking Region reports on this endpoint, got \
             {reported:?}"
        );

        let failures: Vec<String> = last_failure_observations(&router, &id, case)
            .await
            .into_iter()
            .map(|observation| observation.status)
            .collect();
        if row.accepted {
            assert!(
                failures.is_empty(),
                "case {case:?}: a checker whose latest probe passed has no failure to report, \
                 got {failures:?}"
            );
        } else {
            // The other reader of the same observations reports the failing ones, which here is
            // every one of them: the two readers of one health check cannot disagree about what
            // a checker observed.
            assert_eq!(
                failures, reported,
                "case {case:?}: every checker whose probe failed reports that failure as the \
                 last failure reason, got {failures:?} against {reported:?}"
            );
        }
    }
}

/// One phase of the repointing table: the path the live check is repointed at, and whether the
/// site answers it inside the accepted range.
struct Phase {
    case: &'static str,
    path: &'static str,
    accepted: bool,
}

#[tokio::test]
async fn repointing_a_live_checks_resource_path_moves_what_every_checker_reports() {
    let router = emulator();
    publish_site(&router).await;

    // The phases ALTERNATE between a path the site serves and one it does not, so the
    // description a phase waits for can only come from a probe of the path that phase stored —
    // never from the round before it. Nothing about the monitored endpoint changes across them:
    // the only write between two reads is the `UpdateHealthCheck` that repoints the check.
    let phases: &[Phase] = &[
        Phase {
            case: "repointed at a path the site does not hold",
            path: "/absent.html",
            accepted: false,
        },
        Phase {
            case: "repointed back at the site's root",
            path: "/",
            accepted: true,
        },
        Phase {
            case: "repointed at another path the site does not hold",
            path: "/prod/also-absent",
            accepted: false,
        },
        Phase {
            case: "repointed at the other document the site holds",
            path: "/healthy.html",
            accepted: true,
        },
    ];

    let created_case = "created on a path the site holds";
    let id = create_check(
        &router,
        created_case,
        "probe-repointed",
        "HTTPS",
        443,
        "/healthy.html",
    )
    .await;
    let served = answered_status(&router, created_case, "https", "/healthy.html", true).await;
    statuses_settling_on(&router, created_case, &id, &description_of(served)).await;
    let (path, mut version) = configured_path_and_version(&router, created_case, &id).await;
    assert_eq!(
        path, "/healthy.html",
        "case {created_case:?}: the create stores the path the probe requests"
    );

    for phase in phases {
        let case = phase.case;
        let status = answered_status(&router, case, "https", phase.path, phase.accepted).await;
        let updated = update_health_check(
            &router,
            &id,
            &format!("<ResourcePath>{}</ResourcePath>", phase.path),
        )
        .await;
        assert_eq!(
            updated.status, 200,
            "case {case:?}: expected 200 from UpdateHealthCheck, got {} with body {}",
            updated.status, updated.body
        );
        assert_eq!(
            updated.first("HealthCheck/Id"),
            Some(id.as_str()),
            "case {case:?}: an update keeps the health check's identity; body {}",
            updated.body
        );
        let (reported_path, reported_version) = path_and_version(&updated, case);
        assert_eq!(
            (reported_path.as_str(), reported_version),
            (phase.path, version + 1),
            "case {case:?}: an applied update reports the repointed path and advances the \
             version by one"
        );
        // The reconfiguration landed: the check reads BACK with the new path and that version.
        let read = configured_path_and_version(&router, case, &id).await;
        assert_eq!(
            read,
            (phase.path.to_string(), reported_version),
            "case {case:?}: GetHealthCheck must report the repointed path and the new version"
        );
        version = reported_version;

        let expected = description_of(status);
        let reported = statuses_settling_on(&router, case, &id, &expected).await;
        assert_eq!(
            reported.len(),
            HEALTH_CHECK_REGION_VALUES.len(),
            "case {case:?}: every health-checking Region reports on the repointed endpoint, got \
             {reported:?}"
        );
        if !phase.accepted {
            let failures: Vec<String> = last_failure_observations(&router, &id, case)
                .await
                .into_iter()
                .map(|observation| observation.status)
                .collect();
            assert_eq!(
                failures, reported,
                "case {case:?}: the last failure reason reports the checkers whose latest probe \
                 failed, got {failures:?} against {reported:?}"
            );
        }
    }
}
