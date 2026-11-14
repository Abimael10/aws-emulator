// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for the ADDRESS a pipeline publishes as its `IngestEndpointUrls`, and
//! for what the ingest surface served at that address does with the records posted to it.
//!
//! The rule, one level above the recorded case that filed the issue (a client pointed at the
//! emulator's loopback endpoint reading the endpoint back and posting to it):
//!
//! > The ingestion endpoint this service publishes for a pipeline is an address THIS emulator
//! > answers on, so a client that follows `IngestEndpointUrls` verbatim — "the ingestion
//! > endpoints for the pipeline, which you can send data to" — reaches the pipeline that
//! > published it. A client configured with the emulator's own endpoint is published that
//! > endpoint with the pipeline named behind the reserved data-plane path; a client that
//! > resolves AWS's own hosts on an emulator served on no port at all is published the
//! > `<pipeline>-<id>.<region>.osis.amazonaws.com` host AWS publishes, because there is no own
//! > address to put in its place. Whichever form it is, the value is resolved once at creation,
//! > so `CreatePipeline` and `GetPipeline` report the one address — and a POST of records to
//! > the path the pipeline's own Data Prepper `http` source declares is written into the sink
//! > that configuration names, where a reader of that sink finds it.
//!
//! Every case drives the service the way a client does — one HTTP request per call, through
//! the same `MultiServiceRouter` the emulator boots, with this emulator's own Amazon S3
//! mounted on the SAME engine — so the object a delivery writes is a real object of the
//! object store, read back through its own API. The rows differ along what the rule
//! generalises over: the endpoint the control-plane call was addressed at, the path the
//! source declares (one named, the documented default, and a deeper one), the sink's key
//! prefix, the Region the sink is addressed in, the codec the object is written under, and
//! how many records one POST carries.

use std::sync::Arc;

use serde_json::{json, Value};
use smithy_server_runtime::{EmbeddedEmulator, MultiServiceRouter};

/// The Region every control-plane call here is signed for: not the emulator default, so a
/// value built from the request's own credential scope is distinguishable from a constant.
const REGION: &str = "eu-west-2";

/// The reserved data-plane path segment the emulator's own endpoint carries a plane behind
/// (`service_common::data_plane_addressing::EMULATOR_PATH_NAMESPACE`), and the segment that
/// names THIS plane behind it (this service's own signing name).
const LOCAL_MARKER: &str = "/_aws/osis";

/// The path an `http` source that names none accepts records at, from the service crate's own
/// reading of the Data Prepper default.
const DEFAULT_SOURCE_PATH: &str =
    service_com_amazonaws_osis_amazonopensearchingestionservice::state::DEFAULT_HTTP_SOURCE_PATH;

/// The front door's envelope for a request no service claimed. A published address nothing
/// serves is the defect this suite pins, so this is never a legitimate answer here.
const UNKNOWN_SERVICE: &str = "UnknownService";

/// What one call answered: its status, the modelled error code (empty on success) and the
/// bytes of its body.
#[derive(Debug)]
struct Answer {
    status: u16,
    error_type: String,
    body: Vec<u8>,
}

impl Answer {
    /// The body as JSON (`Null` when it is not JSON at all).
    fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or(Value::Null)
    }

    /// The body as text, for an answer this emulator words itself.
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).to_string()
    }

    /// A one-line rendering for an assertion message.
    fn summary(&self) -> String {
        format!(
            "HTTP {} {} {}",
            self.status,
            self.error_type,
            String::from_utf8_lossy(&self.body)
        )
    }
}

/// What one dispatched call answered, read the one way for every call in this suite.
fn answered(response: http::Response<bytes::Bytes>) -> Answer {
    let status = response.status().as_u16();
    let error_type = response
        .headers()
        .get("x-amzn-errortype")
        .and_then(|value| value.to_str().ok())
        .map(|value| {
            value
                .split(['/', ':'])
                .next()
                .unwrap_or(value)
                .trim()
                .to_string()
        })
        .unwrap_or_default();
    Answer {
        status,
        error_type,
        body: response.into_body().to_vec(),
    }
}

/// OpenSearch Ingestion and this emulator's own Amazon S3, on one shared engine.
struct Aws {
    router: MultiServiceRouter,
}

impl Aws {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        // Signature verification is the emulator's own concern and is exercised elsewhere;
        // these cases are about this service's behaviour.
        router.set_request_authentication(false);
        router.register_service(
            service_com_amazonaws_osis_amazonopensearchingestionservice::EmulatedService::new(
                Arc::clone(&engine),
            ),
        );
        // The sink a pipeline writes to is a bucket of THIS object store.
        router.register_service(service_com_amazonaws_s3_amazons3::EmulatedService::new(
            Arc::clone(&engine),
        ));
        Self { router }
    }

    /// A handle to this emulator for driving it through an AWS SDK client: the same
    /// in-process seam the handlers reach the object store through.
    fn callback(&self) -> EmbeddedEmulator {
        EmbeddedEmulator::from_router(self.router.clone())
    }

    /// An Amazon S3 client over that seam, built the one way the tree builds it.
    fn s3(&self) -> ::aws_sdk_s3::Client {
        ::service_common::s3_client::s3_client(&self.callback(), REGION)
    }

    /// One request, addressed at the endpoint `host` names and signed for `REGION`.
    async fn send(
        &self,
        method: &str,
        host: &str,
        path: &str,
        body: Vec<u8>,
        content_type: &str,
    ) -> Answer {
        let request = http::Request::builder()
            .method(method)
            .uri(format!("https://{host}{path}"))
            .header("host", host)
            // The credential scope is how a handler learns the Region its ARNs name, and
            // how a client signs every call to this service — an ingest POST included.
            .header(
                "authorization",
                format!(
                    "AWS4-HMAC-SHA256 Credential=AKIDTEST/20260101/{REGION}/osis/aws4_request, \
                     SignedHeaders=host, Signature=abc"
                ),
            )
            .header("content-type", content_type)
            .body(bytes::Bytes::from(body))
            .expect("a well-formed request");
        let response = self.router.dispatch(request).await;
        answered(response)
    }

    /// One control-plane call carrying a JSON body.
    async fn control(&self, method: &str, host: &str, path: &str, body: Option<Value>) -> Answer {
        let payload = match body {
            Some(value) => serde_json::to_vec(&value).expect("the request body serializes"),
            None => Vec::new(),
        };
        self.send(method, host, path, payload, "application/json")
            .await
    }

    /// Create `bucket`, which is the sink a row's pipeline writes to.
    async fn given_bucket(&self, case: &str, bucket: &str) {
        let created = self
            .s3()
            .create_bucket()
            .bucket(bucket)
            .create_bucket_configuration(
                ::aws_sdk_s3::types::CreateBucketConfiguration::builder()
                    .location_constraint(::aws_sdk_s3::types::BucketLocationConstraint::from(
                        REGION,
                    ))
                    .build(),
            )
            .send()
            .await;
        assert!(
            created.is_ok(),
            "case {case:?}: Amazon S3 must create the sink bucket {bucket}, got {created:?}"
        );
    }

    /// What `bucket` holds now, as (key, bytes) pairs in key order.
    async fn bucket_contents(&self, case: &str, bucket: &str) -> Vec<(String, Vec<u8>)> {
        let listed = self
            .s3()
            .list_objects_v2()
            .bucket(bucket)
            .send()
            .await
            .unwrap_or_else(|error| {
                panic!("case {case:?}: Amazon S3 must list {bucket}, got {error:?}")
            });
        let mut held = Vec::new();
        for object in listed.contents() {
            let key = object.key().unwrap_or_default().to_string();
            let fetched = self
                .s3()
                .get_object()
                .bucket(bucket)
                .key(&key)
                .send()
                .await
                .unwrap_or_else(|error| {
                    panic!("case {case:?}: Amazon S3 must serve {key}, got {error:?}")
                });
            let bytes = fetched
                .body
                .collect()
                .await
                .unwrap_or_else(|error| {
                    panic!("case {case:?}: {key} must have a body, got {error:?}")
                })
                .into_bytes()
                .to_vec();
            held.push((key, bytes));
        }
        held.sort_by(|left, right| left.0.cmp(&right.0));
        held
    }
}

/// Which address a row's pipeline must publish.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Published {
    /// The emulator's own endpoint, with the pipeline behind the reserved data-plane path.
    EmulatorEndpoint,
    /// The `<pipeline>-<id>.<region>.osis.amazonaws.com` host AWS publishes.
    AwsHost,
}

/// The format a row's sink writes its object in.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Codec {
    /// The sink names no codec at all.
    Unnamed,
    /// `ndjson`: one record per line.
    Ndjson,
    /// `json`: the delivery's records as one document.
    Json,
}

impl Codec {
    /// The configuration lines this codec contributes to the sink.
    fn configured(self) -> &'static str {
        match self {
            Self::Unnamed => "",
            Self::Ndjson => "        codec:\n          ndjson:\n",
            Self::Json => "        codec:\n          json:\n",
        }
    }

    /// The records an object written under this codec holds.
    fn records_in(self, case: &str, body: &[u8]) -> Vec<Value> {
        match self {
            // A sink that names no codec is written as newline-delimited JSON, like one
            // that names `ndjson`.
            Self::Unnamed | Self::Ndjson => String::from_utf8_lossy(body)
                .lines()
                .map(|line| {
                    serde_json::from_str(line).unwrap_or_else(|error| {
                        panic!("case {case:?}: a delivered line is not JSON: {error}")
                    })
                })
                .collect(),
            Self::Json => match serde_json::from_slice(body) {
                Ok(Value::Array(records)) => records,
                other => panic!(
                    "case {case:?}: the delivered object must be one JSON array of records, \
                     got {other:?}"
                ),
            },
        }
    }
}

/// One row: who called, the ingest surface the pipeline declares, and what must be published.
struct Row {
    case: &'static str,
    /// The `Host` the control-plane call carries — the endpoint the client is configured with.
    host: &'static str,
    published: Published,
    /// The path the `http` source declares, or `None` to leave it at the documented default.
    source_path: Option<&'static str>,
    /// The sink's `object_key.path_prefix`, or `None` for the root of the bucket.
    path_prefix: Option<&'static str>,
    /// The Region the sink names, or `None` to leave it at the pipeline's own.
    sink_region: Option<&'static str>,
    codec: Codec,
    /// How many records the one POST carries.
    records: usize,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "the recorded case: a client on the emulator's loopback endpoint, an http \
                   source at /ingest and an s3 sink",
            host: "127.0.0.1:41657",
            published: Published::EmulatorEndpoint,
            source_path: Some("/ingest"),
            path_prefix: None,
            sink_region: None,
            codec: Codec::Ndjson,
            records: 1,
        },
        Row {
            case: "the same endpoint named by a loopback NAME, a source naming no path, a \
                   sink naming no codec, and three records in one POST",
            host: "localhost:9297",
            published: Published::EmulatorEndpoint,
            source_path: None,
            path_prefix: None,
            sink_region: None,
            codec: Codec::Unnamed,
            records: 3,
        },
        Row {
            case: "a bracketed IPv6 endpoint, a deeper source path, a sink naming a key \
                   prefix and its own Region, written as one JSON document",
            host: "[::1]:9297",
            published: Published::EmulatorEndpoint,
            source_path: Some("/log/orders/ingest"),
            path_prefix: Some("records/orders/"),
            sink_region: Some("us-east-1"),
            codec: Codec::Json,
            records: 2,
        },
        Row {
            case: "an endpoint named by a bare IPv4 literal with no port",
            host: "127.0.0.1",
            published: Published::EmulatorEndpoint,
            source_path: Some("/ingest"),
            path_prefix: Some("flat"),
            sink_region: None,
            codec: Codec::Ndjson,
            records: 1,
        },
        Row {
            case: "a client that resolves AWS's own hosts, this emulator served on no port: \
                   the documented ingestion endpoint host",
            host: "osis.eu-west-2.amazonaws.com",
            published: Published::AwsHost,
            source_path: Some("/ingest"),
            path_prefix: None,
            sink_region: None,
            codec: Codec::Ndjson,
            records: 1,
        },
    ]
}

/// The name of the one sub-pipeline every configuration below declares.
const SUB_PIPELINE: &str = "orders-pipeline";

/// A configuration body declaring one sub-pipeline whose `http` source accepts records at
/// `/ingest` and whose sinks are the YAML `sinks` names (indented as a `sink:` sequence).
fn configuration_with_sinks(sinks: &str) -> String {
    format!(
        "version: \"2\"\n{SUB_PIPELINE}:\n  source:\n    http:\n      path: \"/ingest\"\n  \
         sink:\n{sinks}"
    )
}

/// The configuration body a row's pipeline is created with: one sub-pipeline whose `http`
/// source declares the row's path and whose `s3` sink names the row's bucket.
fn configuration(row: &Row, bucket: &str) -> String {
    let mut body = format!("version: \"2\"\n{SUB_PIPELINE}:\n  source:\n    http:\n");
    if let Some(path) = row.source_path {
        body.push_str(&format!("      path: \"{path}\"\n"));
    }
    body.push_str("  processor:\n    - date:\n        from_time_received: true\n");
    body.push_str(&format!("  sink:\n    - s3:\n        bucket: \"{bucket}\"\n"));
    if let Some(region) = row.sink_region {
        body.push_str(&format!("        aws:\n          region: \"{region}\"\n"));
    }
    if let Some(prefix) = row.path_prefix {
        body.push_str(&format!(
            "        object_key:\n          path_prefix: \"{prefix}\"\n"
        ));
    }
    body.push_str(row.codec.configured());
    body
}

/// The records one POST carries: each one marked so the object it lands in can be checked to
/// hold exactly these.
fn records(row_index: usize, count: usize) -> Vec<Value> {
    (0..count)
        .map(|index| {
            json!({
                "marker": format!("row-{row_index}-record-{index}"),
                "message": "hello-osis-ingest",
            })
        })
        .collect()
}

/// The one `IngestEndpointUrls` entry a reporter answered.
fn published_endpoint(case: &str, reporter: &str, answer: &Answer) -> String {
    let urls = answer.json()["Pipeline"]["IngestEndpointUrls"]
        .as_array()
        .cloned()
        .unwrap_or_else(|| {
            panic!(
                "case {case:?}: {reporter} must answer IngestEndpointUrls, got {}",
                answer.summary()
            )
        });
    assert_eq!(
        urls.len(),
        1,
        "case {case:?}: {reporter} must publish exactly one ingestion endpoint, got {urls:?}"
    );
    urls[0]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: {reporter} published {:?}", urls[0]))
        .to_string()
}

/// The authority a published endpoint is dialled at, and the path it carries (empty for a
/// value that is a bare host).
fn published_parts(published: &str) -> (String, String) {
    let without_scheme = published
        .split_once("://")
        .map_or(published, |(_scheme, rest)| rest);
    match without_scheme.find('/') {
        Some(at) => (
            without_scheme[..at].to_string(),
            without_scheme[at..].to_string(),
        ),
        None => (without_scheme.to_string(), String::new()),
    }
}

/// Assert `published` is the address this row's client must be given, and answer the
/// authority and base path a client posts to it at.
fn assert_published(row: &Row, pipeline: &str, reporter: &str, published: &str) -> (String, String) {
    let case = row.case;
    let (authority, base_path) = published_parts(published);
    match row.published {
        Published::EmulatorEndpoint => {
            assert_eq!(
                authority, row.host,
                "case {case:?}: {reporter} must publish the endpoint the request reached, got \
                 {published:?}"
            );
            assert!(
                !authority.ends_with("amazonaws.com"),
                "case {case:?}: {reporter} must not publish an AWS host no client of this \
                 emulator resolves, got {published:?}"
            );
            assert_eq!(
                base_path,
                format!("{LOCAL_MARKER}/{pipeline}"),
                "case {case:?}: {reporter} must name the pipeline behind the reserved \
                 data-plane path {LOCAL_MARKER}, got {published:?}"
            );
        }
        Published::AwsHost => {
            let suffix = format!(".{REGION}.osis.amazonaws.com");
            let label = authority.strip_suffix(&suffix).unwrap_or_else(|| {
                panic!(
                    "case {case:?}: {reporter} must publish the documented \
                     <pipeline>-<id>{suffix} host, got {published:?}"
                )
            });
            let id = label.strip_prefix(&format!("{pipeline}-")).unwrap_or_else(|| {
                panic!(
                    "case {case:?}: {reporter} must name the pipeline in the published host, \
                     got {published:?}"
                )
            });
            assert!(
                !id.is_empty() && id.bytes().all(|byte| byte.is_ascii_hexdigit()),
                "case {case:?}: the published host's id segment must be the content-derived \
                 label, got {id:?}"
            );
            assert_eq!(
                base_path, "",
                "case {case:?}: the documented endpoint carries no path of its own, got \
                 {published:?}"
            );
        }
    }
    (authority, base_path)
}

/// RULE: every reporter of a pipeline publishes one ingestion endpoint, and a POST of records
/// to the path its configuration declares is written into the sink that configuration names.
#[tokio::test]
async fn the_published_ingestion_endpoint_is_an_address_this_emulator_serves() {
    for (index, row) in rows().into_iter().enumerate() {
        // One emulator per row, so the sink bucket holds exactly this row's delivery.
        let aws = Aws::new();
        let case = row.case;
        let pipeline = format!("ingest-pipeline-{index}");
        let bucket = format!("osis-sink-{index}");
        aws.given_bucket(case, &bucket).await;

        let created = aws
            .control(
                "POST",
                row.host,
                "/2022-01-01/osis/createPipeline",
                Some(json!({
                    "PipelineName": pipeline,
                    "MinUnits": 1,
                    "MaxUnits": 1,
                    "PipelineConfigurationBody": configuration(&row, &bucket),
                })),
            )
            .await;
        assert_eq!(
            created.status,
            200,
            "case {case:?}: CreatePipeline must answer 200, got {}",
            created.summary()
        );
        let published = published_endpoint(case, "CreatePipeline", &created);
        let (authority, base_path) = assert_published(&row, &pipeline, "CreatePipeline", &published);

        // The implemented reader of the same pipeline reports the one published address.
        let fetched = aws
            .control(
                "GET",
                row.host,
                &format!("/2022-01-01/osis/getPipeline/{pipeline}"),
                None,
            )
            .await;
        assert_eq!(
            fetched.status,
            200,
            "case {case:?}: GetPipeline must answer 200, got {}",
            fetched.summary()
        );
        assert_eq!(
            published_endpoint(case, "GetPipeline", &fetched),
            published,
            "case {case:?}: GetPipeline must report the published endpoint, got {}",
            fetched.summary()
        );

        // A client that resolves AWS's own hosts is told AWS's host, which this emulator
        // does not serve: there is nothing to post to, and the rest of the row is about the
        // address that is served.
        if row.published == Published::AwsHost {
            continue;
        }

        let posted = records(index, row.records);
        let source_path = row.source_path.unwrap_or(DEFAULT_SOURCE_PATH);
        let answer = aws
            .send(
                "POST",
                &authority,
                &format!("{base_path}{source_path}"),
                serde_json::to_vec(&Value::Array(posted.clone())).expect("records serialize"),
                "application/json",
            )
            .await;
        assert_ne!(
            answer.error_type, UNKNOWN_SERVICE,
            "case {case:?}: the published address must be one this emulator serves, but \
             {authority}{base_path}{source_path} is claimed by no service: {}",
            answer.summary()
        );
        assert_eq!(
            answer.status,
            200,
            "case {case:?}: a POST of records to the configured source path must be accepted, \
             got {}",
            answer.summary()
        );

        // What the pipeline's own sink holds: one object per delivery, under the prefix the
        // sink named, carrying exactly the records that were posted.
        let held = aws.bucket_contents(case, &bucket).await;
        assert_eq!(
            held.len(),
            1,
            "case {case:?}: the sink bucket must hold the one delivered object, got {:?}",
            held.iter().map(|(key, _)| key).collect::<Vec<_>>()
        );
        let (key, body) = &held[0];
        // A prefix the configuration wrote without a trailing separator still names a key
        // SEGMENT, so the object's own name follows it as one.
        let prefix = row
            .path_prefix
            .map(|prefix| match prefix.ends_with('/') {
                true => prefix.to_string(),
                false => format!("{prefix}/"),
            })
            .unwrap_or_default();
        assert!(
            key.starts_with(&prefix) && key[prefix.len()..].starts_with("events-"),
            "case {case:?}: the delivered object must land under the sink's prefix \
             {prefix:?}, got {key:?}"
        );
        let delivered = row.codec.records_in(case, body);
        assert_eq!(
            delivered, posted,
            "case {case:?}: the sink must hold the posted records, got {delivered:?} under \
             {key:?}"
        );
    }
}

/// RULE: the ingest surface serves only what the pipeline's configuration declares — the
/// source's own path, by the method an `http` source takes, carrying records — and every
/// other request is answered for what it is, with nothing written to the sink.
#[tokio::test]
async fn an_ingest_request_the_configuration_does_not_declare_is_answered_for_what_it_is() {
    /// One refusal case: what was sent, and the status the endpoint answers.
    struct Case {
        case: &'static str,
        method: &'static str,
        /// The path below the published base.
        path: &'static str,
        body: Vec<u8>,
        status: u16,
    }

    let host = "127.0.0.1:9297";
    let aws = Aws::new();
    let pipeline = "refusal-pipeline";
    let bucket = "osis-refusal-sink";
    aws.given_bucket("the suite's pipeline", bucket).await;
    let row = Row {
        case: "the suite's pipeline",
        host,
        published: Published::EmulatorEndpoint,
        source_path: Some("/ingest"),
        path_prefix: None,
        sink_region: None,
        codec: Codec::Ndjson,
        records: 1,
    };
    let created = aws
        .control(
            "POST",
            host,
            "/2022-01-01/osis/createPipeline",
            Some(json!({
                "PipelineName": pipeline,
                "MinUnits": 1,
                "MaxUnits": 1,
                "PipelineConfigurationBody": configuration(&row, bucket),
            })),
        )
        .await;
    assert_eq!(
        created.status,
        200,
        "CreatePipeline must answer 200, got {}",
        created.summary()
    );
    let base_path = format!("{LOCAL_MARKER}/{pipeline}");

    let cases = vec![
        Case {
            case: "a path no http source declares",
            method: "POST",
            path: "/not-a-source",
            body: b"[{\"marker\":\"x\"}]".to_vec(),
            status: 404,
        },
        Case {
            case: "the endpoint's own root, which no source declares either",
            method: "POST",
            path: "",
            body: b"[{\"marker\":\"x\"}]".to_vec(),
            status: 404,
        },
        Case {
            case: "a method an http source does not take",
            method: "GET",
            path: "/ingest",
            body: Vec::new(),
            status: 405,
        },
        Case {
            case: "a body that is not a JSON array of records",
            method: "POST",
            path: "/ingest",
            body: b"{\"marker\":\"not-an-array\"}".to_vec(),
            status: 400,
        },
        Case {
            case: "a body that is not JSON at all",
            method: "POST",
            path: "/ingest",
            body: b"marker=x".to_vec(),
            status: 400,
        },
    ];
    for Case {
        case,
        method,
        path,
        body,
        status,
    } in cases
    {
        let answer = aws
            .send(
                method,
                host,
                &format!("{base_path}{path}"),
                body,
                "application/json",
            )
            .await;
        assert_eq!(
            answer.status, status,
            "case {case:?}: expected HTTP {status}, got {}",
            answer.summary()
        );
        assert_ne!(
            answer.error_type, UNKNOWN_SERVICE,
            "case {case:?}: the published address is this emulator's, so its own plane \
             answers: {}",
            answer.summary()
        );
        assert!(
            !answer.text().is_empty(),
            "case {case:?}: a refusal must say why, got {}",
            answer.summary()
        );
    }

    // A pipeline this service does not hold serves nothing at all, however the address is
    // shaped.
    let unknown = aws
        .send(
            "POST",
            host,
            &format!("{LOCAL_MARKER}/no-such-pipeline/ingest"),
            b"[{\"marker\":\"x\"}]".to_vec(),
            "application/json",
        )
        .await;
    assert_eq!(
        unknown.status,
        404,
        "case \"an address naming a pipeline nothing created\": expected HTTP 404, got {}",
        unknown.summary()
    );

    // Not one of those requests wrote anything: the sink is still empty.
    let held = aws.bucket_contents("the refused requests", bucket).await;
    assert!(
        held.is_empty(),
        "case \"the refused requests\": nothing may be written to the sink, got {:?}",
        held.iter().map(|(key, _)| key).collect::<Vec<_>>()
    );
}

/// RULE: a POST is answered 200 only once EVERY sink the sub-pipeline declares holds the
/// records, so a route this emulator cannot write in full is answered for that and nothing is
/// written anywhere.
///
/// The rows differ along why a route cannot be written in full: a kind this emulator does not
/// write beside one it does, such a kind alone, a codec it does not write, a sink naming no
/// destination, a sequence entry naming no sink at all, no sink key at all, and a fan-out past
/// the bound on how many sinks one POST writes.
#[tokio::test]
async fn a_route_this_emulator_cannot_write_in_full_delivers_nothing() {
    /// The bound on how many sinks one POST is written to, named from its own definition
    /// rather than copied.
    const MAX_SINKS: usize =
        service_com_amazonaws_osis_amazonopensearchingestionservice::service::ingest_plane::MAX_SINKS_PER_DELIVERY;

    /// One row: the `sink:` sequence the configuration declares, and the status a POST of
    /// records to the source path answers.
    struct Row {
        case: &'static str,
        sinks: String,
        status: u16,
    }

    let host = "127.0.0.1:9297";
    let bucket = "osis-partial-sink";
    // The one bucket every row that names a destination at all writes to, so "nothing was
    // written" is checkable over all of them at once.
    let s3_sink = format!("    - s3:\n        bucket: \"{bucket}\"\n");
    let rows = vec![
        Row {
            case: "a sink kind this emulator does not write, beside one it does",
            sinks: format!("{s3_sink}    - opensearch:\n        index: \"logs\"\n"),
            // 501: the records would reach the bucket but never the opensearch destination.
            status: 501,
        },
        Row {
            case: "a sink kind this emulator does not write, alone",
            sinks: "    - opensearch:\n        index: \"logs\"\n".to_string(),
            status: 501,
        },
        Row {
            case: "a codec this emulator does not write",
            sinks: format!("{s3_sink}        codec:\n          parquet:\n            schema: \"{{}}\"\n"),
            status: 501,
        },
        Row {
            case: "an s3 sink naming no destination",
            sinks: "    - s3:\n        object_key:\n          path_prefix: \"logs/\"\n".to_string(),
            status: 500,
        },
        Row {
            case: "a sink sequence entry naming no sink at all",
            sinks: "    - \"a-bucket-maybe\"\n".to_string(),
            status: 500,
        },
        Row {
            case: "a sub-pipeline declaring no sink at all",
            sinks: "    []\n".to_string(),
            status: 500,
        },
        Row {
            case: "more sinks than one POST is written to",
            sinks: s3_sink.repeat(MAX_SINKS + 1),
            status: 501,
        },
    ];

    for (index, Row { case, sinks, status }) in rows.into_iter().enumerate() {
        // One emulator per row, so "the bucket holds nothing" is this row's own claim.
        let aws = Aws::new();
        aws.given_bucket(case, bucket).await;
        let pipeline = format!("partial-sink-{index}");
        let created = aws
            .control(
                "POST",
                host,
                "/2022-01-01/osis/createPipeline",
                Some(json!({
                    "PipelineName": pipeline,
                    "MinUnits": 1,
                    "MaxUnits": 1,
                    "PipelineConfigurationBody": configuration_with_sinks(&sinks),
                })),
            )
            .await;
        assert_eq!(
            created.status,
            200,
            "case {case:?}: CreatePipeline must answer 200, got {}",
            created.summary()
        );
        let answer = aws
            .send(
                "POST",
                host,
                &format!("{LOCAL_MARKER}/{pipeline}/ingest"),
                b"[{\"marker\":\"never-delivered\"}]".to_vec(),
                "application/json",
            )
            .await;
        assert_eq!(
            answer.status, status,
            "case {case:?}: expected HTTP {status}, got {}",
            answer.summary()
        );
        assert!(
            !answer.text().is_empty(),
            "case {case:?}: the answer must say why nothing was delivered, got {}",
            answer.summary()
        );
        let held = aws.bucket_contents(case, bucket).await;
        assert!(
            held.is_empty(),
            "case {case:?}: a route that cannot be written in full must write nothing, got {:?}",
            held.iter().map(|(key, _)| key).collect::<Vec<_>>()
        );
    }
}

/// RULE: an ingest POST is authenticated exactly as every other call to this service is —
/// the emulator verifies the SigV4 credential before a record is accepted, so a signed POST
/// is served and an unsigned one reaches no sink.
///
/// Request authentication is ON here (every other test in this suite turns it off, as the
/// rest of this service's suites do), which is the one condition under which the plane's own
/// declared auth scheme means anything.
#[tokio::test]
async fn an_ingest_request_is_authenticated_like_every_other_call() {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    // ON, explicitly: this is the one suite here that asks the emulator to verify a
    // credential, and a router registers no authenticator until it is asked to.
    router.set_request_authentication(true);
    router.register_service(
        service_com_amazonaws_osis_amazonopensearchingestionservice::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(service_com_amazonaws_s3_amazons3::EmulatedService::new(
        Arc::clone(&engine),
    ));
    let callback = EmbeddedEmulator::from_router(router.clone());

    /// One call, signed with the emulator's own credential the way an in-process caller
    /// signs — the signature this emulator's verifier computes over the same request.
    async fn signed(
        router: &MultiServiceRouter,
        callback: &EmbeddedEmulator,
        method: &str,
        host: &str,
        path: &str,
        body: Vec<u8>,
    ) -> Answer {
        let mut request = http::Request::builder()
            .method(method)
            .uri(format!("https://{host}{path}"))
            .header("host", host)
            .header("content-type", "application/json")
            .body(body)
            .expect("a well-formed request");
        ::service_common::in_process_sigv4::sign_callback_request(
            &mut request, "osis", REGION, callback,
        )
        .await
        .expect("the request signs with the emulator's own credential");
        let (parts, body) = request.into_parts();
        let signed = http::Request::from_parts(parts, bytes::Bytes::from(body));
        answered(router.dispatch(signed).await)
    }

    let host = "127.0.0.1:9297";
    let bucket = "osis-signed-sink";
    let pipeline = "signed-pipeline";
    let created_bucket = ::service_common::s3_client::s3_client(&callback, REGION)
        .create_bucket()
        .bucket(bucket)
        .create_bucket_configuration(
            ::aws_sdk_s3::types::CreateBucketConfiguration::builder()
                .location_constraint(::aws_sdk_s3::types::BucketLocationConstraint::from(REGION))
                .build(),
        )
        .send()
        .await;
    assert!(
        created_bucket.is_ok(),
        "Amazon S3 must create the sink bucket, got {created_bucket:?}"
    );

    let sinks = format!("    - s3:\n        bucket: \"{bucket}\"\n");
    let created = signed(
        &router,
        &callback,
        "POST",
        host,
        "/2022-01-01/osis/createPipeline",
        serde_json::to_vec(&json!({
            "PipelineName": pipeline,
            "MinUnits": 1,
            "MaxUnits": 1,
            "PipelineConfigurationBody": configuration_with_sinks(&sinks),
        }))
        .expect("the request body serializes"),
    )
    .await;
    assert_eq!(
        created.status,
        200,
        "case \"a signed CreatePipeline\": expected 200, got {}",
        created.summary()
    );
    let base_path = format!("{LOCAL_MARKER}/{pipeline}");

    // UNSIGNED: the address is this emulator's and the plane claims it, so the request is
    // refused for its credential rather than answered — and nothing reaches the sink.
    let unsigned_request = http::Request::builder()
        .method("POST")
        .uri(format!("https://{host}{base_path}/ingest"))
        .header("host", host)
        .header("content-type", "application/json")
        .body(bytes::Bytes::from_static(b"[{\"marker\":\"unsigned\"}]"))
        .expect("a well-formed request");
    let unsigned = answered(router.dispatch(unsigned_request).await);
    assert!(
        (400..500).contains(&unsigned.status),
        "case \"an unsigned ingest POST\": expected the credential to be refused, got {}",
        unsigned.summary()
    );

    // SIGNED: served, and the records are in the sink.
    let accepted = signed(
        &router,
        &callback,
        "POST",
        host,
        &format!("{base_path}/ingest"),
        b"[{\"marker\":\"signed\"}]".to_vec(),
    )
    .await;
    assert_eq!(
        accepted.status,
        200,
        "case \"a signed ingest POST\": expected the records to be accepted, got {}",
        accepted.summary()
    );

    let listed = ::service_common::s3_client::s3_client(&callback, REGION)
        .list_objects_v2()
        .bucket(bucket)
        .send()
        .await
        .expect("Amazon S3 lists the sink bucket");
    let keys: Vec<String> = listed
        .contents()
        .iter()
        .map(|object| object.key().unwrap_or_default().to_string())
        .collect();
    assert_eq!(
        keys.len(),
        1,
        "case \"an authenticated ingest flow\": only the signed POST may have delivered, got \
         {keys:?}"
    );
    let fetched = ::service_common::s3_client::s3_client(&callback, REGION)
        .get_object()
        .bucket(bucket)
        .key(&keys[0])
        .send()
        .await
        .expect("Amazon S3 serves the delivered object");
    let body = fetched
        .body
        .collect()
        .await
        .expect("the delivered object has a body")
        .into_bytes();
    let text = String::from_utf8_lossy(&body).to_string();
    assert!(
        text.contains("signed") && !text.contains("unsigned"),
        "case \"an authenticated ingest flow\": the sink holds the signed POST's records \
         only, got {text:?}"
    );
}

/// RULE: the ingestion endpoint answers out of the pipeline's own stored status — a pipeline
/// that is not running serves no records — and accepting records moves no status of its own.
#[tokio::test]
async fn a_pipeline_that_is_not_running_accepts_no_records() {
    let host = "127.0.0.1:9297";
    let aws = Aws::new();
    let pipeline = "stopped-pipeline";
    let bucket = "osis-stopped-sink";
    aws.given_bucket("a stopped pipeline", bucket).await;
    let row = Row {
        case: "a stopped pipeline",
        host,
        published: Published::EmulatorEndpoint,
        source_path: Some("/ingest"),
        path_prefix: None,
        sink_region: None,
        codec: Codec::Ndjson,
        records: 1,
    };
    let created = aws
        .control(
            "POST",
            host,
            "/2022-01-01/osis/createPipeline",
            Some(json!({
                "PipelineName": pipeline,
                "MinUnits": 1,
                "MaxUnits": 1,
                "PipelineConfigurationBody": configuration(&row, bucket),
            })),
        )
        .await;
    assert_eq!(
        created.status,
        200,
        "CreatePipeline must answer 200, got {}",
        created.summary()
    );
    let base_path = format!("{LOCAL_MARKER}/{pipeline}");

    // While it is running, the endpoint takes records...
    let accepted = aws
        .send(
            "POST",
            host,
            &format!("{base_path}/ingest"),
            b"[{\"marker\":\"before-the-stop\"}]".to_vec(),
            "application/json",
        )
        .await;
    assert_eq!(
        accepted.status,
        200,
        "case \"a running pipeline\": expected the records to be accepted, got {}",
        accepted.summary()
    );

    // ... and once it is stopped, it does not, and the status a read reports is the stopped
    // one — serving records moved nothing.
    let stopped = aws
        .control(
            "PUT",
            host,
            &format!("/2022-01-01/osis/stopPipeline/{pipeline}"),
            None,
        )
        .await;
    assert_eq!(
        stopped.status,
        200,
        "StopPipeline must answer 200, got {}",
        stopped.summary()
    );
    let refused = aws
        .send(
            "POST",
            host,
            &format!("{base_path}/ingest"),
            b"[{\"marker\":\"after-the-stop\"}]".to_vec(),
            "application/json",
        )
        .await;
    assert_eq!(
        refused.status,
        503,
        "case \"a stopped pipeline\": expected HTTP 503, got {}",
        refused.summary()
    );
    assert!(
        refused.text().contains("STOPPED"),
        "case \"a stopped pipeline\": the answer must name the status it read, got {}",
        refused.summary()
    );

    let read_back = aws
        .control(
            "GET",
            host,
            &format!("/2022-01-01/osis/getPipeline/{pipeline}"),
            None,
        )
        .await;
    assert_eq!(
        read_back.json()["Pipeline"]["Status"].as_str(),
        Some("STOPPED"),
        "case \"a stopped pipeline\": a read must still report STOPPED, got {}",
        read_back.summary()
    );

    // Exactly the one delivery the running pipeline accepted is in the sink.
    let held = aws.bucket_contents("a stopped pipeline", bucket).await;
    assert_eq!(
        held.len(),
        1,
        "case \"a stopped pipeline\": only the accepted delivery may be in the sink, got {:?}",
        held.iter().map(|(key, _)| key).collect::<Vec<_>>()
    );
    assert!(
        String::from_utf8_lossy(&held[0].1).contains("before-the-stop"),
        "case \"a stopped pipeline\": the sink holds the records the running pipeline took, \
         got {:?}",
        String::from_utf8_lossy(&held[0].1)
    );
}
