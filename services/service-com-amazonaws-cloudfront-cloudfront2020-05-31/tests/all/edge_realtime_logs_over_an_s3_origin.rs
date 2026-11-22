// SPDX-License-Identifier: CC0-1.0

//! The REAL-TIME LOG RECORD a viewer request served out of a PRIVATE Amazon S3 ORIGIN delivers
//! to the Kinesis data stream its cache behaviour's real-time log configuration names — the
//! whole pipeline a client configures, with CloudFront, the object store, Kinesis and IAM on
//! one engine.
//!
//! RULE: **every viewer request the edge SERVES for a cache behaviour that names a real-time log
//! configuration puts exactly ONE record on each Kinesis data stream that configuration's
//! `EndPoints` name — one tab-delimited line carrying one value per field the configuration
//! reports, in the order it reports them (`RealtimeLogConfig$Fields` is "a list of fields that
//! are included in each real-time log record. In an API response, the fields are provided in the
//! same order in which they are sent to the Amazon Kinesis data stream"), and each value is read
//! off THAT request and the answer the edge served for it, whoever produced that answer.** A
//! behaviour that names no configuration delivers nothing.
//!
//! The sibling `edge_realtime_logs` suite pins that rule against an origin this service serves
//! itself, and `dialled_edge_address_realtime_logs` pins it for a viewer that dials the
//! published address. Neither can say that the pipeline a client actually builds — a private
//! bucket fronted by an origin access control, under the AWS-managed caching-disabled cache
//! policy, whose record travels to a data stream under an IAM delivery role — produces that
//! record: the origin fetch goes out to another service and comes back, and the record is built
//! from the answer it came back with, so only a suite that mounts the object store beside the
//! stream can state that one viewer request on such a distribution yields one log line naming
//! that request.
//!
//! The rows span what the rule generalises over: the key's form (a bare name, a nested prefix),
//! the order the field SET is submitted in (the order a client lists them in, and the reverse of
//! it — the reported order is the service's own either way, and it is the reported order the
//! columns follow), how many requests the row serves (one, and the same read twice under a cache
//! policy that caches nothing, so each served request owes its own record), who produced the
//! answer and of which class (the origin's object, and the origin's own not-found for a key the
//! bucket does not hold), the size of the configured field set (six fields, and two), and
//! whether the behaviour names the configuration at all.
//!
//! Every expected value is read off what the edge answered: the status the viewer was served and
//! the identifier the edge stamped on that answer are what the record is checked against, so a
//! record describing another request — or a value invented for one — fails the row. The column
//! ORDER expected is computed from the service's own published field catalogue
//! (`crate::state::REALTIME_LOG_FIELDS`) filtered to the row's submitted set, never spelled out.
//!
//! Every viewer request goes through a router with request authentication ON, which is what
//! makes the origin half real: a viewer presents no credential, so the only thing that can make
//! the object store answer the origin fetch is the signature the edge puts on it for the
//! CloudFront service principal. The setup calls go through a second router over the SAME engine
//! with authentication off, so each carries a credential scope without a computed signature.

use std::sync::Arc;

use service_com_amazonaws_cloudfront_cloudfront2020_05_31::state::REALTIME_LOG_FIELDS;
use smithy_server_runtime::{EmbeddedEmulator, MultiServiceRouter};

use crate::distribution_lifecycle::{awaited_deployment, call, API_VERSION};
use crate::edge_realtime_logs::{
    assert_record, awaited_active, data_stream, delivery_role, policy, realtime_log_config,
    stream_records, window, EndPoint, Probe, DEFAULT_BEHAVIOR_PATTERN,
};

/// The Region the origin buckets and the data streams are created in, which is the Region every
/// setup call's credential scope names and so the Region the streams' ARNs carry.
const REGION: &str = "us-east-1";

/// The service principal an origin access control makes CloudFront's origin request as, and so
/// the principal an origin bucket's policy grants the read to.
const CLOUDFRONT_PRINCIPAL: &str = "cloudfront.amazonaws.com";

/// The AWS-managed cache policy `Managed-CachingDisabled`, whose TTLs are all zero: every viewer
/// request under it reaches the origin, so each served request is an answer the origin produced
/// for it rather than one the edge cache held.
const CACHING_DISABLED: &str = "4135ea2d-6df8-44a3-9df3-4b5a84be39ad";

/// What the edge reports in `X-Cache` for an answer an ORIGIN produced for this request.
const MISS_FROM_CLOUDFRONT: &str = "Miss from cloudfront";

/// How a record classifies an answer an origin produced for the request, and how it classifies
/// one of an error class ("Error — usually means the HTTP status code is 4xx or 5xx").
const MISS: &str = "Miss";
const ERROR: &str = "Error";

/// The action a `PutRecord` to a data stream is authorized by, which is what a delivery role has
/// to allow on the stream for the record to be written to it
/// (`KinesisStreamConfig$RoleARN`: "the ARN of an IAM role that CloudFront can use to send
/// real-time log data to your Kinesis data stream").
const PUT_RECORD: &str = "kinesis:PutRecord";

/// The fields a client that polls for its first log line configures, as the CloudFront
/// documentation's own real-time-logging walkthrough lists them, plus the identifier the edge
/// stamps on the answer it served — which is what ties one record to one request. Every name is
/// one of the published catalogue's ([`REALTIME_LOG_FIELDS`]).
const POLLED_FIELDS: &[&str] = &[
    "timestamp",
    "c-ip",
    "cs-uri-stem",
    "sc-status",
    "x-edge-response-result-type",
    "x-edge-request-id",
];

/// The same SET submitted the other way round, which has to come back — and be read — in the
/// service's own order all the same.
const POLLED_FIELDS_REVERSED: &[&str] = &[
    "x-edge-request-id",
    "x-edge-response-result-type",
    "sc-status",
    "cs-uri-stem",
    "c-ip",
    "timestamp",
];

/// A two-field configuration, submitted in the reverse of the catalogue's order: a record carries
/// as many values as the configuration includes fields, and no more.
const TWO_FIELDS: &[&str] = &["cs-uri-stem", "sc-status"];

// ---------------------------------------------------------------------------
// The emulator: CloudFront, the object store its origin lives in, and the
// delivery pipeline the record travels down
// ---------------------------------------------------------------------------

/// CloudFront, Amazon S3, Kinesis and IAM over ONE shared engine, as two planes: the SETUP
/// plane, whose calls carry a credential scope without a computed signature, and the VIEWER
/// plane, which authenticates — so what makes the origin fetch succeed is the signature the edge
/// applies to it.
///
/// All four share the engine: the edge fetches the origin out of the object store, delivers the
/// record into the data stream Kinesis holds, and asks IAM what the delivery role allows, so only
/// a router carrying all of them can serve the pipeline end to end.
struct Emulator {
    setup: MultiServiceRouter,
    viewers: MultiServiceRouter,
    callback: EmbeddedEmulator,
}

impl Emulator {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let setup = mounted(&engine, false);
        let viewers = mounted(&engine, true);
        let callback = EmbeddedEmulator::from_router(setup.clone());
        Self {
            setup,
            viewers,
            callback,
        }
    }

    /// The Amazon S3 client, built by the one shared constructor over the in-process callback, so
    /// the bucket this suite stages is the one the edge's origin fetch reads.
    fn s3(&self) -> ::aws_sdk_s3::Client {
        service_common::s3_client::s3_client(&self.callback, REGION)
    }

    /// Create `bucket`, put `body` at `key` in it and grant CloudFront the read — through Amazon
    /// S3's OWN API, so the object the edge fetches is one a client really put there.
    async fn staged_origin(&self, case: &str, bucket: &str, key: &str, body: &str) {
        let s3 = self.s3();
        s3.create_bucket()
            .bucket(bucket)
            .send()
            .await
            .unwrap_or_else(|error| {
                panic!("case {case:?}: creating {bucket} must succeed, got {error:?}")
            });
        s3.put_object()
            .bucket(bucket)
            .key(key)
            .body(::aws_sdk_s3::primitives::ByteStream::from(
                body.as_bytes().to_vec(),
            ))
            .send()
            .await
            .unwrap_or_else(|error| {
                panic!("case {case:?}: putting {bucket}/{key} must succeed, got {error:?}")
            });
        s3.put_bucket_policy()
            .bucket(bucket)
            .policy(grant(bucket))
            .send()
            .await
            .unwrap_or_else(|error| {
                panic!("case {case:?}: the policy of {bucket} must be stored, got {error:?}")
            });
    }
}

/// The four services on `engine`, with request authentication as `authenticated` says.
fn mounted(engine: &Arc<engine::InMemoryEngine>, authenticated: bool) -> MultiServiceRouter {
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(authenticated);
    router.register_service(
        service_com_amazonaws_cloudfront_cloudfront2020_05_31::EmulatedService::new(Arc::clone(
            engine,
        )),
    );
    router.register_service(service_com_amazonaws_s3_amazons3::EmulatedService::new(
        Arc::clone(engine),
    ));
    router.register_service(
        service_com_amazonaws_kinesis_kinesis_20131202::EmulatedService::new(Arc::clone(engine)),
    );
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(Arc::clone(
            engine,
        )),
    );
    router
}

/// The host a bucket of this Region is addressed at virtual-hosted style, which is what an S3
/// origin's `DomainName` carries.
fn origin_domain(bucket: &str) -> String {
    format!("{bucket}.s3.{REGION}.amazonaws.com")
}

/// A bucket policy granting the CloudFront service principal the reads its signed origin request
/// makes: `s3:GetObject` on every object of `bucket`, and `s3:ListBucket` on the bucket itself —
/// which is what decides how the ORIGIN answers a read of a key it does not hold ("if you have
/// the `s3:ListBucket` permission on the bucket, Amazon S3 returns an HTTP status code 404 Not
/// Found error. If you don't have the `s3:ListBucket` permission, Amazon S3 returns an HTTP
/// status code 403 Access Denied error", `GetObject`'s own documentation), so the absent-key row
/// is answered by the origin's own not-found.
fn grant(bucket: &str) -> String {
    format!(
        "{{\"Version\":\"2012-10-17\",\"Statement\":[{{\
         \"Sid\":\"AllowCloudFrontServicePrincipal\",\"Effect\":\"Allow\",\
         \"Principal\":{{\"Service\":\"{CLOUDFRONT_PRINCIPAL}\"}},\
         \"Action\":[\"s3:GetObject\",\"s3:ListBucket\"],\
         \"Resource\":[\"arn:aws:s3:::{bucket}/*\",\"arn:aws:s3:::{bucket}\"]}}]}}"
    )
}

/// `CreateOriginAccessControl` for an `s3` origin CloudFront signs every request to, answering
/// the control's id.
async fn origin_access_control(setup: &MultiServiceRouter, case: &str, name: &str) -> String {
    let created = call(
        setup,
        "POST",
        &format!("/{API_VERSION}/origin-access-control"),
        Some(&format!(
            "<OriginAccessControlConfig \
             xmlns=\"http://cloudfront.amazonaws.com/doc/{API_VERSION}/\">\
             <Name>{name}</Name>\
             <SigningProtocol>sigv4</SigningProtocol>\
             <SigningBehavior>always</SigningBehavior>\
             <OriginAccessControlOriginType>s3</OriginAccessControlOriginType>\
             </OriginAccessControlConfig>"
        )),
        None,
    )
    .await;
    assert_eq!(
        created.status, 201,
        "case {case:?}: expected HTTP 201 from CreateOriginAccessControl, got {}: {}",
        created.status, created.raw
    );
    created
        .root()
        .find_text("Id")
        .unwrap_or_else(|| panic!("case {case:?}: the created origin access control reports no Id"))
        .to_string()
}

/// `CreateDistribution` over the S3 origin `bucket` fronted by the origin access control
/// `control`, whose DEFAULT cache behaviour names `realtime_log_config_arn` when the row attaches
/// one; answers the distribution's `(Id, DomainName)`.
async fn distribution(
    setup: &MultiServiceRouter,
    case: &str,
    caller_reference: &str,
    bucket: &str,
    control: &str,
    realtime_log_config_arn: Option<&str>,
) -> (String, String) {
    let logging = realtime_log_config_arn
        .map(|arn| format!("<RealtimeLogConfigArn>{arn}</RealtimeLogConfigArn>"))
        .unwrap_or_default();
    let created = call(
        setup,
        "POST",
        &format!("/{API_VERSION}/distribution"),
        Some(&format!(
            "<DistributionConfig xmlns=\"http://cloudfront.amazonaws.com/doc/{API_VERSION}/\">\
             <CallerReference>{caller_reference}</CallerReference>\
             <Origins><Quantity>1</Quantity><Items><Origin>\
             <Id>origin-bucket</Id><DomainName>{}</DomainName>\
             <S3OriginConfig><OriginAccessIdentity></OriginAccessIdentity></S3OriginConfig>\
             <OriginAccessControlId>{control}</OriginAccessControlId>\
             </Origin></Items></Origins>\
             <DefaultCacheBehavior><TargetOriginId>origin-bucket</TargetOriginId>\
             <ViewerProtocolPolicy>allow-all</ViewerProtocolPolicy>{logging}\
             <CachePolicyId>{CACHING_DISABLED}</CachePolicyId>\
             </DefaultCacheBehavior>\
             <Comment>real-time logs over a private s3 origin</Comment>\
             <Enabled>true</Enabled>\
             </DistributionConfig>",
            origin_domain(bucket)
        )),
        None,
    )
    .await;
    assert_eq!(
        created.status, 201,
        "case {case:?}: expected HTTP 201 from CreateDistribution, got {}: {}",
        created.status, created.raw
    );
    let read = |member: &str| {
        created
            .root()
            .find_text(member)
            .unwrap_or_else(|| panic!("case {case:?}: the created distribution reports no {member}"))
            .to_string()
    };
    (read("Id"), read("DomainName"))
}

/// What the edge answered one viewer request with.
struct Viewed {
    status: u16,
    /// `X-Cache`, which says who produced the answer.
    x_cache: String,
    /// `X-Amz-Cf-Id`, the identifier the edge stamped on the request it served.
    request_id: String,
    body: String,
}

/// One VIEWER request, addressed host-style at the name the distribution published and carrying
/// no credential of any kind — which is what a viewer is.
async fn view(router: &MultiServiceRouter, domain: &str, path: &str) -> Viewed {
    let request = ::http::Request::builder()
        .method("GET")
        .uri(path)
        .header("host", domain)
        .body(::bytes::Bytes::new())
        .expect("the viewer request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let header = |name: &str| {
        response
            .headers()
            .get(name)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_string()
    };
    let x_cache = header("x-cache");
    let request_id = header("x-amz-cf-id");
    let body = String::from_utf8_lossy(&response.into_body()).to_string();
    Viewed {
        status,
        x_cache,
        request_id,
        body,
    }
}

/// The submitted field SET in the order the service sends the fields in, which is the order its
/// reads report and the order a record's columns follow: the published catalogue filtered to the
/// set ([`REALTIME_LOG_FIELDS`]).
fn expected_order(submitted: &[&str]) -> Vec<String> {
    REALTIME_LOG_FIELDS
        .iter()
        .filter(|known| submitted.contains(*known))
        .map(|known| (*known).to_string())
        .collect()
}

// ---------------------------------------------------------------------------
// The cases
// ---------------------------------------------------------------------------

/// One viewer request a row serves: the path it asks for, the status the edge owes it, and how a
/// record classifies the answer it got.
struct Request {
    path: &'static str,
    status: u16,
    result_type: &'static str,
}

/// One row: a private bucket holding one object, a data stream and the delivery role that may
/// write to it, a configuration over the row's field set, and a distribution over the bucket
/// whose default behaviour names that configuration where the row attaches it.
struct Row {
    case: &'static str,
    /// This row's own suffix: its bucket, stream, role, configuration and distribution are all
    /// named from it, so no row's state can answer another row's request.
    name: &'static str,
    /// The key the bucket HOLDS, and the bytes it holds there.
    key: &'static str,
    body: &'static str,
    /// The fields `CreateRealtimeLogConfig` is called with, in the order the REQUEST states them.
    submitted: &'static [&'static str],
    /// Whether the distribution's default cache behaviour names the configuration at all.
    attached: bool,
    requests: &'static [Request],
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "the fields a polling client configures, over a bare key",
            name: "rtl-s3-bare",
            key: "probe.txt",
            body: "the object the edge served",
            submitted: POLLED_FIELDS,
            attached: true,
            requests: &[Request {
                path: "/probe.txt",
                status: 200,
                result_type: MISS,
            }],
        },
        Row {
            case: "the same set submitted in reverse, over a nested key read twice",
            name: "rtl-s3-nested",
            key: "assets/2026/held.css",
            body: "body { content: 'held'; }",
            submitted: POLLED_FIELDS_REVERSED,
            attached: true,
            requests: &[
                Request {
                    path: "/assets/2026/held.css",
                    status: 200,
                    result_type: MISS,
                },
                Request {
                    path: "/assets/2026/held.css",
                    status: 200,
                    result_type: MISS,
                },
            ],
        },
        Row {
            case: "a two-field configuration, over a key the bucket does not hold",
            name: "rtl-s3-absent",
            key: "present.txt",
            body: "the object that is there",
            submitted: TWO_FIELDS,
            attached: true,
            requests: &[Request {
                path: "/never-written.txt",
                status: 404,
                result_type: ERROR,
            }],
        },
        // Everything about this row is the first row's except the attachment, so what decides
        // whether anything is delivered is the behaviour naming a configuration and nothing else.
        Row {
            case: "a behaviour naming no configuration at all",
            name: "rtl-s3-unattached",
            key: "probe.txt",
            body: "nothing is logged for this one",
            submitted: POLLED_FIELDS,
            attached: false,
            requests: &[Request {
                path: "/probe.txt",
                status: 200,
                result_type: MISS,
            }],
        },
    ]
}

/// What one row left behind for the viewer phase.
struct Prepared {
    case: &'static str,
    name: &'static str,
    body: &'static str,
    /// The field list the service reported for the configuration, which is the list each record's
    /// values are read by.
    reported: Vec<String>,
    stream_arn: String,
    id: String,
    domain: String,
    requests: &'static [Request],
    /// How many records the row's requests owe its stream.
    records: usize,
}

/// The whole rule, over the table: one served viewer request on a distribution over a private S3
/// origin puts exactly one record, carrying this request's own values in the configuration's own
/// order, on the data stream its behaviour's real-time log configuration names.
#[tokio::test]
async fn a_served_s3_origin_request_delivers_one_real_time_log_record_to_its_stream() {
    let emulator = Emulator::new();

    let mut prepared: Vec<Prepared> = Vec::new();
    for row in rows() {
        emulator
            .staged_origin(row.case, row.name, row.key, row.body)
            .await;
        // The stream, then the role that may write to it, then the configuration naming both:
        // the order a client has to follow, each resource created through its own service's API.
        let stream_arn = data_stream(&emulator.setup, row.name).await;
        let role_arn = delivery_role(
            &emulator.setup,
            row.name,
            &policy(&[("Allow", PUT_RECORD)], &stream_arn),
        )
        .await;
        let (config_arn, reported) = realtime_log_config(
            &emulator.setup,
            row.name,
            100,
            row.submitted,
            &[EndPoint {
                role_arn,
                stream_arn: stream_arn.clone(),
            }],
        )
        .await;
        assert_eq!(
            reported,
            expected_order(row.submitted),
            "case {:?}: expected the submitted field set in the order the fields are sent to the \
             data stream, got {reported:?}",
            row.case
        );
        let control =
            origin_access_control(&emulator.setup, row.case, &format!("{}-oac", row.name)).await;
        let (id, domain) = distribution(
            &emulator.setup,
            row.case,
            &format!("{}-reference", row.name),
            row.name,
            &control,
            row.attached.then_some(config_arn.as_str()),
        )
        .await;
        prepared.push(Prepared {
            case: row.case,
            name: row.name,
            body: row.body,
            reported,
            stream_arn,
            id,
            domain,
            requests: row.requests,
            records: if row.attached { row.requests.len() } else { 0 },
        });
    }

    // The caller's own clock window, which the records' timestamps have to fall inside.
    let window = window();

    for row in &prepared {
        // Neither the stream nor the distribution is ready the moment it was asked for, and the
        // CLOCK owes both: the CONDITION each service reports is polled for, never a span waited
        // out. Both were asked for in the loop above, so one wait covers every row.
        awaited_active(&emulator.setup, row.name).await;
        awaited_deployment(&emulator.setup, row.case, &row.id).await;

        let mut probes: Vec<Probe> = Vec::new();
        for request in row.requests {
            let viewed = view(&emulator.viewers, &row.domain, request.path).await;
            assert_eq!(
                viewed.status, request.status,
                "case {:?}: expected the edge to answer {} for {:?}, got {} with {:?}",
                row.case, request.status, request.path, viewed.status, viewed.body
            );
            if request.status == 200 {
                assert_eq!(
                    viewed.body, row.body,
                    "case {:?}: expected the ORIGIN's own bytes for {:?}, got {:?}",
                    row.case, request.path, viewed.body
                );
                assert_eq!(
                    viewed.x_cache, MISS_FROM_CLOUDFRONT,
                    "case {:?}: expected the answer to be the origin's, got X-Cache {:?}",
                    row.case, viewed.x_cache
                );
            }
            assert!(
                !viewed.request_id.is_empty(),
                "case {:?}: the edge must stamp an X-Amz-Cf-Id on the answer it served for \
                 {:?}, got none (status {})",
                row.case,
                request.path,
                viewed.status
            );
            probes.push(Probe {
                method: "GET",
                path: request.path.to_string(),
                query: "",
                domain: row.domain.clone(),
                status: request.status,
                result_type: request.result_type,
                request_id: viewed.request_id,
                pattern: DEFAULT_BEHAVIOR_PATTERN.to_string(),
                window,
            });
        }

        // Read back through Kinesis itself — the shards, an iterator at the horizon and the
        // records it answers — so a record this suite sees is a record a client polling that
        // stream sees.
        let records = stream_records(&emulator.setup, &row.stream_arn).await;
        assert_eq!(
            records.len(),
            row.records,
            "case {:?}: expected {} record(s) on the stream, got {records:?}",
            row.case,
            row.records
        );
        // The records arrive in the order the requests were served, a stream holding one
        // producer's records in the order they were written.
        for (record, probe) in records.iter().zip(&probes) {
            assert!(
                !record.contains('\n') && !record.contains('\r'),
                "case {:?}: one record is ONE line, got {record:?}",
                row.case
            );
            assert_record(row.case, &row.reported, record, probe);
        }
    }
}
