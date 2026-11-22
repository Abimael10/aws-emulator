// SPDX-License-Identifier: CC0-1.0

//! The POINT OF PRESENCE the edge served a request from, as the planes that report an edge
//! location name it.
//!
//! The rule: CloudFront serves a viewer from one of its edge locations and NAMES the one that
//! served a request wherever it reports anything about that request. On the viewer's own answer
//! that name is the `Server-Timing` header's `cdn-pop` metric, which — with `cdn-rid`, the id
//! of the request — describes the exchange ITSELF rather than any work it caused, so a sampled
//! response carries both whatever the cache and the origin did; in the request's log record it
//! is the field reporting the edge location. One request therefore cannot be served by one
//! location and reported as another's.
//!
//! The rows differ along what the rule generalises over: whether the origin produced the
//! answer, whether the edge cache held it, whether CloudFront generated the page itself (so
//! that none of the work-dependent metrics are written at all), and which method the viewer
//! asked with.
//!
//! The harness is the `Server-Timing` suite's and the real-time log suite's, reused rather
//! than copied.

use service_com_amazonaws_cloudfront_cloudfront2020_05_31::service::edge::point_of_presence::is_pop_name;
use smithy_server_runtime::MultiServiceRouter;

use crate::distribution_lifecycle::{awaited_deployment, create_distribution, emulator, API_VERSION, HOST};
use crate::edge_realtime_logs::{
    awaited_active, data_stream, delivery_role, emulator_with_kinesis_and_iam, policy,
    realtime_log_config, stream_records, EndPoint,
};
use crate::edge_server_timing_header::{
    deployed, metric_names, metric_parameter, timing_policy, view, CACHING_DISABLED,
    CACHING_OPTIMIZED, SERVED_PATH,
};
use crate::response_headers_policy_lifecycle::ServerTiming;

/// Every metric name the `Server-Timing` header CloudFront writes can carry.
///
/// The catalog is the metric list of the CloudFront developer guide's `Server-Timing` header
/// reference (the page `ResponseHeadersPolicyServerTimingHeadersConfig` points a reader at),
/// and nothing outside it is a name this header states.
const PUBLISHED_METRICS: [&str; 11] = [
    "cdn-cache-hit",
    "cdn-cache-refresh",
    "cdn-cache-miss",
    "cdn-pop",
    "cdn-rid",
    "cdn-hit-layer",
    "cdn-upstream-layer",
    "cdn-upstream-dns",
    "cdn-upstream-connect",
    "cdn-upstream-fbl",
    "cdn-downstream-fbl",
];

/// The metrics that describe the EXCHANGE rather than work it caused, and so stand on every
/// value of the header: where it was served and which request it was.
const ALWAYS_PRESENT: [&str; 2] = ["cdn-pop", "cdn-rid"];

/// One viewer exchange this rule is asserted over.
struct Exchange {
    case: &'static str,
    /// The managed cache policy the behaviour serves it under, which is what decides whether
    /// the edge cache can hold the answer at all.
    cache_policy: &'static str,
    /// The method the viewer asks with: one the behaviour allows is served, and one it does
    /// not is the page CloudFront generates itself.
    method: &'static str,
    /// How many times the viewer asks; the answer to the LAST of them is the one read, so a
    /// second ask under a caching policy reads the answer the edge cache held.
    asks: usize,
    /// The status that last answer carries.
    status: u16,
}

/// The `cdn-pop` metric names the point of presence that served the request, on every sampled
/// response — whoever produced the answer and whatever work it took.
#[tokio::test]
async fn every_sampled_response_names_the_point_of_presence_that_served_it() {
    let router = emulator();
    let policy_id = timing_policy(
        &router,
        "timing-pop",
        Some(ServerTiming {
            enabled: true,
            sampling_rate: Some(100.0),
        }),
    )
    .await;

    let exchanges = [
        Exchange {
            case: "an answer the origin produced",
            cache_policy: CACHING_DISABLED,
            method: "GET",
            asks: 1,
            status: 200,
        },
        Exchange {
            case: "an answer the edge cache held",
            cache_policy: CACHING_OPTIMIZED,
            method: "GET",
            asks: 2,
            status: 200,
        },
        Exchange {
            // The behaviour allows this method, so the origin answers it — with its own error
            // for a path it serves no HEAD of, which is still an answer an origin produced and
            // so still a miss the edge reports.
            case: "an error the origin produced",
            cache_policy: CACHING_DISABLED,
            method: "HEAD",
            asks: 1,
            status: 404,
        },
        Exchange {
            // The behaviour allows no such method, so CloudFront answers the viewer itself:
            // nothing reached the cache or the origin, and none of the metrics that report
            // their work is written.
            case: "a page CloudFront generated itself",
            cache_policy: CACHING_DISABLED,
            method: "POST",
            asks: 1,
            status: 403,
        },
    ];

    // Every row's POP, so the suite can assert the edge reported ONE location across them.
    let mut reported: Vec<(&str, String)> = Vec::new();
    for (index, exchange) in exchanges.iter().enumerate() {
        let case = exchange.case;
        let domain = deployed(
            &router,
            &format!("timing-pop-{index}"),
            exchange.cache_policy,
            &policy_id,
        )
        .await;
        let mut answer = view(&router, &domain, exchange.method, SERVED_PATH).await;
        for _ in 1..exchange.asks {
            answer = view(&router, &domain, exchange.method, SERVED_PATH).await;
        }
        assert_eq!(
            answer.status, exchange.status,
            "case {case}: expected the answer to carry {}, got {} ({:?})",
            exchange.status, answer.status, answer.x_cache
        );
        assert!(
            !answer.timing.is_empty(),
            "case {case}: expected a Server-Timing header on every sampled response, got none"
        );
        let names = metric_names(&answer.timing);
        for always in ALWAYS_PRESENT {
            assert!(
                names.contains(&always.to_string()),
                "case {case}: expected the {always} metric on every sampled response, got \
                 {:?}",
                answer.timing
            );
        }
        for name in &names {
            assert!(
                PUBLISHED_METRICS.contains(&name.as_str()),
                "case {case}: {name:?} is no published Server-Timing metric, got {:?}",
                answer.timing
            );
        }
        let pop = metric_parameter(&answer.timing, "cdn-pop", "desc").unwrap_or_else(|| {
            panic!(
                "case {case}: expected the serving POP in the cdn-pop desc, got {:?}",
                answer.timing
            )
        });
        assert!(
            is_pop_name(&pop),
            "case {case}: expected a POP name spelled <three letters><number>, got {pop:?}"
        );
        // The id metric belongs to THIS request, which is what makes the POP this request's
        // POP rather than some other exchange's.
        assert_eq!(
            metric_parameter(&answer.timing, "cdn-rid", "desc"),
            Some(answer.cf_id.clone()),
            "case {case}: expected the request id the edge stamped ({:?}), got {:?}",
            answer.cf_id,
            answer.timing
        );
        reported.push((case, pop));
    }

    let (first_case, first_pop) = reported
        .first()
        .cloned()
        .expect("the table drives at least one exchange");
    for (case, pop) in &reported {
        assert_eq!(
            *pop, first_pop,
            "case {case}: the edge serves from one location, so it cannot report {pop:?} here \
             and {first_pop:?} for {first_case}"
        );
    }
}

/// The LOG RECORD of a served request names the same point of presence its own answer did: one
/// request is served from one location, so the field reporting the edge location and the
/// viewer's `cdn-pop` metric cannot disagree.
#[tokio::test]
async fn the_log_record_names_the_same_point_of_presence_as_the_answer() {
    let router = emulator_with_kinesis_and_iam();
    let stream = "pop-records";
    let stream_arn = data_stream(&router, stream).await;
    awaited_active(&router, stream).await;
    let role_arn = delivery_role(
        &router,
        "pop-delivery",
        &policy(&[("Allow", "kinesis:PutRecord")], &stream_arn),
    )
    .await;
    // The two fields this rule reads: the location that served the request, and the id that
    // says WHICH request the record describes.
    let (log_config_arn, fields) = realtime_log_config(
        &router,
        "pop-config",
        100,
        &["x-edge-location", "x-edge-request-id"],
        &[EndPoint {
            role_arn,
            stream_arn: stream_arn.clone(),
        }],
    )
    .await;
    let policy_id = timing_policy(
        &router,
        "pop-timing",
        Some(ServerTiming {
            enabled: true,
            sampling_rate: Some(100.0),
        }),
    )
    .await;
    let domain = deployed_with_logging(&router, "pop-both", &policy_id, &log_config_arn).await;

    let answer = view(&router, &domain, "GET", SERVED_PATH).await;
    assert_eq!(
        answer.status, 200,
        "expected the viewer request to be served, got {}",
        answer.status
    );
    let pop = metric_parameter(&answer.timing, "cdn-pop", "desc").unwrap_or_else(|| {
        panic!(
            "expected the serving POP in the cdn-pop desc, got {:?}",
            answer.timing
        )
    });

    let records = stream_records(&router, &stream_arn).await;
    assert_eq!(
        records.len(),
        1,
        "expected one record for the one served request, got {records:?}"
    );
    let record = &records[0];
    let values: Vec<&str> = record.split('\t').collect();
    assert_eq!(
        values.len(),
        fields.len(),
        "expected one value per field {fields:?}, got {record:?}"
    );
    let read = |field: &str| {
        fields
            .iter()
            .position(|held| held == field)
            .and_then(|at| values.get(at).copied())
            .unwrap_or_default()
            .to_string()
    };
    assert_eq!(
        read("x-edge-request-id"),
        answer.cf_id,
        "expected the record of the request the viewer made ({:?}), got {record:?}",
        answer.cf_id
    );
    let location = read("x-edge-location");
    assert_eq!(
        location, pop,
        "expected the record to name the location the answer reported ({pop:?}), got \
         {location:?}"
    );
    assert!(
        is_pop_name(&location),
        "expected a POP name spelled <three letters><number>, got {location:?}"
    );
}

/// Create one distribution whose default behaviour both attaches `response_headers_policy` and
/// delivers its real-time log records through `realtime_log_config_arn`, let the clock deploy
/// it, and report the domain name a viewer addresses.
///
/// The one configuration shape neither sibling suite submits: each states one of the two
/// members, and this rule is about one request reported on both planes.
async fn deployed_with_logging(
    router: &MultiServiceRouter,
    reference: &str,
    response_headers_policy: &str,
    realtime_log_config_arn: &str,
) -> String {
    let config = format!(
        "<DistributionConfig xmlns=\"http://cloudfront.amazonaws.com/doc/{API_VERSION}/\">\
         <CallerReference>{reference}</CallerReference>\
         <Origins><Quantity>1</Quantity><Items><Origin>\
         <Id>only</Id><DomainName>{HOST}</DomainName>\
         <CustomOriginConfig><HTTPPort>80</HTTPPort><HTTPSPort>443</HTTPSPort>\
         <OriginProtocolPolicy>https-only</OriginProtocolPolicy></CustomOriginConfig>\
         </Origin></Items></Origins>\
         <DefaultCacheBehavior><TargetOriginId>only</TargetOriginId>\
         <ViewerProtocolPolicy>allow-all</ViewerProtocolPolicy>\
         <CachePolicyId>{CACHING_DISABLED}</CachePolicyId>\
         <ResponseHeadersPolicyId>{response_headers_policy}</ResponseHeadersPolicyId>\
         <RealtimeLogConfigArn>{realtime_log_config_arn}</RealtimeLogConfigArn>\
         </DefaultCacheBehavior>\
         <Comment>the point of presence suite</Comment>\
         <PriceClass>PriceClass_All</PriceClass>\
         <Enabled>true</Enabled>\
         </DistributionConfig>"
    );
    let created = create_distribution(router, &config).await;
    assert_eq!(
        created.status, 201,
        "creating the distribution {reference} answered {} with {}",
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
    awaited_deployment(router, reference, &id).await;
    domain
}
