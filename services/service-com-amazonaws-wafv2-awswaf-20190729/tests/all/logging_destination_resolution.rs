// SPDX-License-Identifier: CC0-1.0

//! Whether a web ACL's logging destination EXISTS, as `PutLoggingConfiguration` resolves
//! it, end to end over the wire.
//!
//! The rule under test: a `LoggingConfiguration` "defines an association between logging
//! destinations and a web ACL resource, for logging from WAF" and the put "enables the
//! specified LoggingConfiguration, to start logging from a web ACL"
//! (`smithy_query(query="doc",shape="LoggingConfiguration")`), so WAF resolves every
//! destination the configuration names BEFORE it stores anything — in the Region the web
//! ACL itself lives in, which for a CLOUDFRONT-scoped web ACL is the one Region that scope
//! is served at, and with the service that OWNS the destination. A destination established
//! not to be there is a resource that doesn't exist: the put is refused with
//! `WAFNonexistentItemException` (HTTP 400) and nothing is written, so the subsequent
//! `GetLoggingConfiguration` is refused the same way — while a destination the owning
//! service holds in that Region is accepted and reads back.
//!
//! The rows differ along what the rule generalises over: the KIND of destination (a
//! CloudWatch Logs log group, an Amazon Data Firehose delivery stream, an Amazon S3
//! bucket), the web ACL's SCOPE and so the Region its destinations are resolved in, and WHY
//! a destination is not there — nothing created it, it was created only in another Region,
//! or it is addressed in another Region than the web ACL's. Each refused row is paired with
//! the same destination made resolvable, so what the row changes is that one fact. A
//! sibling suite takes the same rule over a LIST of destinations, where every entry is
//! resolved and an entry repeated in the list is resolved once.
//!
//! Only a multi-service suite can pin this: the destinations belong to CloudWatch Logs,
//! Amazon Data Firehose and Amazon S3, so each one is created through the OWNING service's
//! own wire and resolved by WAF through the same in-process emulator the assembled one
//! mounts. The last suite pins the other half of the rule: an emulator with no owning
//! service to ask establishes nothing, so the put is accepted rather than refused for a
//! destination nobody was asked about.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The Region a REGIONAL web ACL of this suite lives in, which is where its destinations
/// are resolved.
const REGIONAL_REGION: &str = "us-west-2";

/// The ONE Region endpoint the CLOUDFRONT scope is served at ("API and SDKs - For all
/// calls, use the Region endpoint us-east-1", `Scope`'s own doc), which is therefore the
/// Region a CLOUDFRONT-scoped web ACL lives in and the one its destinations are resolved
/// in.
const CLOUDFRONT_REGION: &str = "us-east-1";

/// The prefix a logging destination's own name must carry
/// (`smithy_query(query="doc",shape="LoggingConfiguration")`: "the name that you give the
/// destination must start with `aws-waf-logs-`"), so every destination below is named past
/// the one refusal this suite is not about.
const WAF_LOGS_PREFIX: &str = "aws-waf-logs-";

/// The sentence `WAFNonexistentItemException` carries, as the recorded WAF responses word
/// it — with the typographic apostrophes AWS uses. Spelled out here independently of the
/// service's own constant, so this suite pins the wording and not merely its reuse.
const NONEXISTENT_ITEM_MESSAGE: &str =
    "AWS WAF couldn’t perform the operation because your resource doesn’t exist.";

/// Amazon S3's own default Region, the one a `CreateBucketConfiguration` must not name
/// ("if you don't specify a Region, the bucket is created in the US East (N. Virginia)
/// Region").
const S3_DEFAULT_REGION: &str = "us-east-1";

/// One emulator with WAFv2 and the three services a logging destination can live in, all on
/// one shared engine, as the assembled emulator mounts them.
struct Emulator {
    router: MultiServiceRouter,
}

impl Emulator {
    /// WAFv2 beside CloudWatch Logs, Amazon Data Firehose and Amazon S3.
    fn new() -> Self {
        Self {
            router: Self::router_with_destinations(true),
        }
    }

    /// WAFv2 alone: no service that owns a destination is mounted, so nothing can be asked
    /// about one.
    fn without_destination_services() -> Self {
        Self {
            router: Self::router_with_destinations(false),
        }
    }

    fn router_with_destinations(mounted: bool) -> MultiServiceRouter {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        // The suite exercises the services, not SigV4: every call carries a credential
        // scope so the router can name the target service, and no signature is verified.
        router.set_request_authentication(false);
        router.register_service(
            service_com_amazonaws_wafv2_awswaf_20190729::EmulatedService::new(Arc::clone(&engine)),
        );
        if mounted {
            router.register_service(
                service_com_amazonaws_cloudwatchlogs_logs_20140328::EmulatedService::new(
                    Arc::clone(&engine),
                ),
            );
            router.register_service(
                service_com_amazonaws_firehose_firehose_20150804::EmulatedService::new(
                    Arc::clone(&engine),
                ),
            );
            router.register_service(service_com_amazonaws_s3_amazons3::EmulatedService::new(
                engine,
            ));
        }
        router
    }

    /// A SigV4 `Authorization` whose credential scope names `signing_name` in `region`,
    /// which is what routes the request and names the Region it is made in. The signature
    /// itself is never checked here.
    fn authorization(signing_name: &str, region: &str) -> String {
        format!(
            "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{region}/{signing_name}/\
             aws4_request, SignedHeaders=host, Signature=0000"
        )
    }

    /// Dispatch one request as it arrives on the wire.
    async fn dispatch(
        &self,
        method: &str,
        host: &str,
        path: &str,
        headers: &[(&str, &str)],
        body: Bytes,
    ) -> (u16, Vec<u8>) {
        let mut builder = http::Request::builder()
            .method(method)
            .uri(format!("https://{host}{path}"))
            .header("host", host);
        for (name, value) in headers {
            builder = builder.header(*name, *value);
        }
        let response = self
            .router
            .dispatch(builder.body(body).expect("a well-formed request"))
            .await;
        (response.status().as_u16(), response.into_body().to_vec())
    }

    /// One awsJson1_1 call of the service `signing_name`, made in `region`.
    async fn json_rpc(
        &self,
        signing_name: &str,
        target_prefix: &str,
        region: &str,
        operation: &str,
        body: Value,
    ) -> (u16, Value) {
        let (status, bytes) = self
            .dispatch(
                "POST",
                &format!("{signing_name}.{region}.amazonaws.com"),
                "/",
                &[
                    ("content-type", "application/x-amz-json-1.1"),
                    ("authorization", &Self::authorization(signing_name, region)),
                    ("x-amz-target", &format!("{target_prefix}.{operation}")),
                ],
                Bytes::from(body.to_string()),
            )
            .await;
        (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
    }

    /// One WAFv2 call made at the endpoint of `region`.
    async fn waf(&self, region: &str, operation: &str, body: Value) -> (u16, Value) {
        self.json_rpc("wafv2", "AWSWAF_20190729", region, operation, body)
            .await
    }

    /// One WAFv2 call that has to succeed.
    async fn waf_ok(&self, region: &str, operation: &str, body: Value) -> Value {
        let (status, parsed) = self.waf(region, operation, body).await;
        assert_eq!(
            status, 200,
            "setup {operation} in {region}: expected HTTP 200, got {status} with {parsed}"
        );
        parsed
    }

    /// Create the log group `name` through CloudWatch Logs' own wire, in `region` — so the
    /// group the account holds is one of THAT Region.
    async fn given_log_group(&self, region: &str, name: &str) {
        let (status, payload) = self
            .json_rpc(
                "logs",
                "Logs_20140328",
                region,
                "CreateLogGroup",
                json!({ "logGroupName": name }),
            )
            .await;
        assert_eq!(
            status, 200,
            "setup CreateLogGroup({name}) in {region}: expected HTTP 200, got {status} with \
             {payload}"
        );
    }

    /// Create the bucket `name` through Amazon S3's own (REST, path-style) wire, in
    /// `region`.
    ///
    /// A `CreateBucketConfiguration` states the Region for any Region but Amazon S3's own
    /// default one, `us-east-1`, which refuses a constraint naming it.
    async fn given_bucket(&self, region: &str, name: &str) {
        let body = if region == S3_DEFAULT_REGION {
            String::new()
        } else {
            format!(
                "<CreateBucketConfiguration xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\">\
                 <LocationConstraint>{region}</LocationConstraint></CreateBucketConfiguration>"
            )
        };
        let (status, answered) = self
            .dispatch(
                "PUT",
                &format!("s3.{region}.amazonaws.com"),
                &format!("/{name}"),
                &[("authorization", &Self::authorization("s3", region))],
                Bytes::from(body),
            )
            .await;
        assert_eq!(
            status,
            200,
            "setup CreateBucket({name}) in {region}: expected HTTP 200, got {status} with {:?}",
            String::from_utf8_lossy(&answered)
        );
    }

    /// Create the delivery stream `name` in `region`, delivering into a bucket of its own.
    ///
    /// What this suite needs of the stream is that Firehose HOLDS it, which
    /// `DescribeDeliveryStream` answers whatever status the stream is in, so nothing here
    /// waits for one.
    async fn given_delivery_stream(&self, region: &str, name: &str) {
        let sink = format!("{name}-sink");
        self.given_bucket(region, &sink).await;
        let (status, payload) = self
            .json_rpc(
                "firehose",
                "Firehose_20150804",
                region,
                "CreateDeliveryStream",
                json!({
                    "DeliveryStreamName": name,
                    "ExtendedS3DestinationConfiguration": {
                        "RoleARN": format!(
                            "arn:aws:iam::{}:role/firehose-delivery",
                            engine::emulator_account_id()
                        ),
                        "BucketARN": format!("arn:aws:s3:::{sink}"),
                    },
                }),
            )
            .await;
        assert_eq!(
            status, 200,
            "setup CreateDeliveryStream({name}) in {region}: expected HTTP 200, got {status} \
             with {payload}"
        );
    }

    /// Create a web ACL of `scope` named `name` at the endpoint that serves that scope, and
    /// answer its ARN.
    async fn given_web_acl(&self, scope: &str, name: &str) -> String {
        let created = self
            .waf_ok(
                endpoint_region(scope),
                "CreateWebACL",
                json!({
                    "Name": name,
                    "Scope": scope,
                    "DefaultAction": { "Allow": {} },
                    "VisibilityConfig": {
                        "SampledRequestsEnabled": true,
                        "CloudWatchMetricsEnabled": true,
                        "MetricName": name,
                    },
                }),
            )
            .await;
        created["Summary"]["ARN"]
            .as_str()
            .expect("Summary.ARN")
            .to_string()
    }
}

/// The Region the endpoint a call for `scope` goes to is in.
fn endpoint_region(scope: &str) -> &'static str {
    if scope == "CLOUDFRONT" {
        CLOUDFRONT_REGION
    } else {
        REGIONAL_REGION
    }
}

/// A CloudWatch Logs log group's ARN in `region`.
fn log_group_arn(region: &str, name: &str) -> String {
    format!(
        "arn:aws:logs:{region}:{}:log-group:{name}",
        engine::emulator_account_id()
    )
}

/// An Amazon Data Firehose delivery stream's ARN in `region`.
fn delivery_stream_arn(region: &str, name: &str) -> String {
    format!(
        "arn:aws:firehose:{region}:{}:deliverystream/{name}",
        engine::emulator_account_id()
    )
}

/// Where a destination of a row is created, if anywhere.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Created {
    /// Nothing created the destination at all.
    Nowhere,
    /// The owning service holds it in this Region.
    In(&'static str),
}

/// The kind of destination a row names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    LogGroup,
    DeliveryStream,
    Bucket,
}

/// One destination-resolution row.
struct Row {
    case: &'static str,
    /// The web ACL's scope, which decides the Region its destinations are resolved in.
    scope: &'static str,
    kind: Kind,
    /// Where the owning service holds the destination.
    created: Created,
    /// The Region the destination's ARN names; ignored for a bucket, whose ARN form
    /// carries none.
    addressed: &'static str,
    /// Whether WAF must accept the configuration.
    accepted: bool,
}

/// The rows: every destination kind, both scopes, and every way a destination is not there
/// — nothing created it, it exists only in another Region, or it is addressed in another
/// Region than the one the web ACL's destinations are resolved in. Each refused row has an
/// accepted sibling that differs in that one fact alone.
const ROWS: &[Row] = &[
    Row {
        case: "a log group the account holds in the web ACL's own Region",
        scope: "REGIONAL",
        kind: Kind::LogGroup,
        created: Created::In(REGIONAL_REGION),
        addressed: REGIONAL_REGION,
        accepted: true,
    },
    Row {
        case: "a log group nothing created",
        scope: "REGIONAL",
        kind: Kind::LogGroup,
        created: Created::Nowhere,
        addressed: REGIONAL_REGION,
        accepted: false,
    },
    Row {
        case: "a log group of that name created only in another Region",
        scope: "REGIONAL",
        kind: Kind::LogGroup,
        created: Created::In(CLOUDFRONT_REGION),
        addressed: REGIONAL_REGION,
        accepted: false,
    },
    Row {
        case: "a log group addressed in another Region than the web ACL's",
        scope: "REGIONAL",
        kind: Kind::LogGroup,
        created: Created::In(CLOUDFRONT_REGION),
        addressed: CLOUDFRONT_REGION,
        accepted: false,
    },
    Row {
        case: "a log group the account holds in the Region the CLOUDFRONT scope is served at",
        scope: "CLOUDFRONT",
        kind: Kind::LogGroup,
        created: Created::In(CLOUDFRONT_REGION),
        addressed: CLOUDFRONT_REGION,
        accepted: true,
    },
    Row {
        case: "a CLOUDFRONT web ACL's log group living in another Region",
        scope: "CLOUDFRONT",
        kind: Kind::LogGroup,
        created: Created::In(REGIONAL_REGION),
        addressed: REGIONAL_REGION,
        accepted: false,
    },
    Row {
        case: "a delivery stream the account holds in the web ACL's own Region",
        scope: "REGIONAL",
        kind: Kind::DeliveryStream,
        created: Created::In(REGIONAL_REGION),
        addressed: REGIONAL_REGION,
        accepted: true,
    },
    Row {
        case: "a delivery stream nothing created",
        scope: "REGIONAL",
        kind: Kind::DeliveryStream,
        created: Created::Nowhere,
        addressed: REGIONAL_REGION,
        accepted: false,
    },
    Row {
        case: "a delivery stream addressed in another Region than the web ACL's",
        scope: "REGIONAL",
        kind: Kind::DeliveryStream,
        created: Created::In(CLOUDFRONT_REGION),
        addressed: CLOUDFRONT_REGION,
        accepted: false,
    },
    Row {
        case: "a bucket the account holds",
        scope: "REGIONAL",
        kind: Kind::Bucket,
        created: Created::In(REGIONAL_REGION),
        addressed: "",
        accepted: true,
    },
    Row {
        case: "a bucket nothing created",
        scope: "REGIONAL",
        kind: Kind::Bucket,
        created: Created::Nowhere,
        addressed: "",
        accepted: false,
    },
];

/// The `PutLoggingConfiguration` request naming `destination` in front of `resource_arn`.
fn put_request(resource_arn: &str, destination: &str) -> Value {
    put_request_for(resource_arn, &[destination])
}

/// The `PutLoggingConfiguration` request naming `destinations` in front of `resource_arn`.
fn put_request_for(resource_arn: &str, destinations: &[&str]) -> Value {
    json!({
        "LoggingConfiguration": {
            "ResourceArn": resource_arn,
            "LogDestinationConfigs": destinations,
        }
    })
}

/// Every destination of a LIST is resolved, so a list is accepted only when each of them is
/// there — and what is stored is the list the request wrote, a value repeated in it
/// included, because resolving a destination once is about the resolution and not about the
/// configuration.
#[tokio::test]
async fn every_destination_of_a_list_is_resolved() {
    let emulator = Emulator::new();
    let held = format!("{WAF_LOGS_PREFIX}listed-group");
    let bucket = format!("{WAF_LOGS_PREFIX}listed-bucket");
    let absent = format!("{WAF_LOGS_PREFIX}listed-absent-group");
    emulator.given_log_group(REGIONAL_REGION, &held).await;
    emulator.given_bucket(REGIONAL_REGION, &bucket).await;
    let web_acl = emulator.given_web_acl("REGIONAL", "acl-listed").await;

    let group_destination = log_group_arn(REGIONAL_REGION, &held);
    let bucket_destination = format!("arn:aws:s3:::{bucket}");
    let absent_destination = log_group_arn(REGIONAL_REGION, &absent);

    // A list whose destinations are all there, with one of them named TWICE: accepted, and
    // stored exactly as the request wrote it.
    let case = "a list of destinations that are all there, one of them twice";
    let named = [
        group_destination.as_str(),
        bucket_destination.as_str(),
        group_destination.as_str(),
    ];
    let put = emulator
        .waf_ok(
            REGIONAL_REGION,
            "PutLoggingConfiguration",
            put_request_for(&web_acl, &named),
        )
        .await;
    assert_eq!(
        put["LoggingConfiguration"]["LogDestinationConfigs"],
        json!(named),
        "case {case:?}: the put must answer the list as given, got {put}"
    );
    let read = emulator
        .waf_ok(
            REGIONAL_REGION,
            "GetLoggingConfiguration",
            json!({ "ResourceArn": web_acl }),
        )
        .await;
    assert_eq!(
        read["LoggingConfiguration"]["LogDestinationConfigs"],
        json!(named),
        "case {case:?}: the read must answer the stored list, got {read}"
    );

    // One entry of the list is not there: the whole configuration is refused, and the list
    // already stored is left alone.
    let case = "a list whose last destination is not there";
    let (status, body) = emulator
        .waf(
            REGIONAL_REGION,
            "PutLoggingConfiguration",
            put_request_for(
                &web_acl,
                &[group_destination.as_str(), absent_destination.as_str()],
            ),
        )
        .await;
    assert_eq!(
        status, 400,
        "case {case:?}: a list carrying a destination that is not there must be refused, got \
         status {status} with {body}"
    );
    assert_eq!(
        body["__type"],
        json!("WAFNonexistentItemException"),
        "case {case:?}: expected WAFNonexistentItemException, got {body}"
    );
    let still = emulator
        .waf_ok(
            REGIONAL_REGION,
            "GetLoggingConfiguration",
            json!({ "ResourceArn": web_acl }),
        )
        .await;
    assert_eq!(
        still["LoggingConfiguration"]["LogDestinationConfigs"],
        json!(named),
        "case {case:?}: a refused put must leave the stored list in place, got {still}"
    );
}

/// A destination WAF must resolve, and never find, resolves to the nonexistent-item refusal
/// and stores nothing; one the owning service holds in the web ACL's own Region is accepted
/// and reads back.
#[tokio::test]
async fn a_logging_destination_is_resolved_before_the_configuration_is_stored() {
    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case;
        let emulator = Emulator::new();
        let name = format!("{WAF_LOGS_PREFIX}destination-{index}");
        let destination = match row.kind {
            Kind::LogGroup => {
                if let Created::In(region) = row.created {
                    emulator.given_log_group(region, &name).await;
                }
                log_group_arn(row.addressed, &name)
            }
            Kind::DeliveryStream => {
                if let Created::In(region) = row.created {
                    emulator.given_delivery_stream(region, &name).await;
                }
                delivery_stream_arn(row.addressed, &name)
            }
            Kind::Bucket => {
                if let Created::In(region) = row.created {
                    emulator.given_bucket(region, &name).await;
                }
                format!("arn:aws:s3:::{name}")
            }
        };
        let web_acl = emulator
            .given_web_acl(row.scope, &format!("acl-destination-{index}"))
            .await;
        let region = endpoint_region(row.scope);

        let (status, body) = emulator
            .waf(
                region,
                "PutLoggingConfiguration",
                put_request(&web_acl, &destination),
            )
            .await;
        let (read_status, read_body) = emulator
            .waf(
                region,
                "GetLoggingConfiguration",
                json!({ "ResourceArn": web_acl }),
            )
            .await;

        if row.accepted {
            assert_eq!(
                status, 200,
                "case {case:?}: a destination the owning service holds in the web ACL's \
                 Region must be accepted, got status {status} with {body}"
            );
            assert_eq!(
                body["LoggingConfiguration"]["LogDestinationConfigs"],
                json!([destination]),
                "case {case:?}: the put must answer the destination as given, got {body}"
            );
            assert_eq!(
                read_status, 200,
                "case {case:?}: the stored configuration must read back, got status \
                 {read_status} with {read_body}"
            );
            assert_eq!(
                read_body["LoggingConfiguration"]["LogDestinationConfigs"],
                json!([destination]),
                "case {case:?}: the read must answer the stored destination, got {read_body}"
            );
        } else {
            assert_eq!(
                status, 400,
                "case {case:?}: a destination WAF cannot resolve must be refused, got status \
                 {status} with {body}"
            );
            assert_eq!(
                body["__type"],
                json!("WAFNonexistentItemException"),
                "case {case:?}: expected WAFNonexistentItemException, got {body}"
            );
            assert_eq!(
                body["Message"],
                json!(NONEXISTENT_ITEM_MESSAGE),
                "case {case:?}: expected the nonexistent-item sentence, got {body}"
            );

            // A refused put wrote nothing, so the web ACL carries no configuration at all
            // — which is the absent case a client branches on, refused the same way.
            assert_eq!(
                read_status, 400,
                "case {case:?}: a refused put must store nothing, got status {read_status} \
                 with {read_body}"
            );
            assert_eq!(
                read_body["__type"],
                json!("WAFNonexistentItemException"),
                "case {case:?}: a web ACL with no configuration must answer \
                 WAFNonexistentItemException, got {read_body}"
            );
            assert_eq!(
                read_body["Message"],
                json!(NONEXISTENT_ITEM_MESSAGE),
                "case {case:?}: the read must carry the nonexistent-item sentence, got \
                 {read_body}"
            );
        }
    }
}

/// A destination addressed in the web ACL's own Region, created there AFTER a first put was
/// refused for it, is accepted on the next put: the refusal is about where the destination
/// is, not about the value the request wrote.
#[tokio::test]
async fn a_destination_created_in_the_web_acls_region_is_accepted_on_the_next_put() {
    let case = "a log group created after the first put was refused";
    let emulator = Emulator::new();
    let group = format!("{WAF_LOGS_PREFIX}appearing-group");
    let destination = log_group_arn(REGIONAL_REGION, &group);
    let web_acl = emulator.given_web_acl("REGIONAL", "acl-appearing").await;

    let (refused, body) = emulator
        .waf(
            REGIONAL_REGION,
            "PutLoggingConfiguration",
            put_request(&web_acl, &destination),
        )
        .await;
    assert_eq!(
        refused, 400,
        "case {case:?}: a group nothing holds must be refused, got status {refused} with \
         {body}"
    );

    emulator.given_log_group(REGIONAL_REGION, &group).await;
    let accepted = emulator
        .waf_ok(
            REGIONAL_REGION,
            "PutLoggingConfiguration",
            put_request(&web_acl, &destination),
        )
        .await;
    assert_eq!(
        accepted["LoggingConfiguration"]["LogDestinationConfigs"],
        json!([destination]),
        "case {case:?}: the put must answer the destination as given, got {accepted}"
    );
}

/// An emulator with no service that owns the destination has nobody to ask about it, so
/// nothing is established and the configuration is accepted: a refusal has to be grounded
/// in what the owning service said.
#[tokio::test]
async fn a_destination_nobody_can_be_asked_about_is_not_refused() {
    let case = "no CloudWatch Logs mounted to resolve the destination";
    let emulator = Emulator::without_destination_services();
    let destination = log_group_arn(
        REGIONAL_REGION,
        &format!("{WAF_LOGS_PREFIX}unresolvable-group"),
    );
    let web_acl = emulator.given_web_acl("REGIONAL", "acl-unasked").await;

    let put = emulator
        .waf_ok(
            REGIONAL_REGION,
            "PutLoggingConfiguration",
            put_request(&web_acl, &destination),
        )
        .await;
    assert_eq!(
        put["LoggingConfiguration"]["LogDestinationConfigs"],
        json!([destination]),
        "case {case:?}: an unasked destination must be stored as given, got {put}"
    );
    let read = emulator
        .waf_ok(
            REGIONAL_REGION,
            "GetLoggingConfiguration",
            json!({ "ResourceArn": web_acl }),
        )
        .await;
    assert_eq!(
        read["LoggingConfiguration"]["LogDestinationConfigs"],
        json!([destination]),
        "case {case:?}: the read must answer the stored destination, got {read}"
    );
}
