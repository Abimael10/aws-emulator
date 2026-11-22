// SPDX-License-Identifier: CC0-1.0

//! A domain is ONE resource published through TWO API versions — end to end over the real
//! `MultiServiceRouter` with Amazon OpenSearch Service (2021-01-01), the legacy Amazon
//! Elasticsearch Service (2015-01-01) and Amazon CloudWatch Logs mounted on one engine, so
//! what is asserted is what a client of either API sees.
//!
//! The RULE pinned here: **which API version a client speaks decides the SHAPE it reads a
//! domain in, never whether the domain is there: every read of the 2015-01-01 surface
//! resolves a domain created through the 2021-01-01 surface, and reports the configuration
//! that create recorded — the same log publishing options, the same cluster, the same
//! endpoint, the same tags.**
//!
//! Both models publish the same resource behind the one `es` endpoint
//! (`smithy_query(query="service_info")` on either gives sigv4Name, endpointPrefix and
//! arnNamespace `es`), and the legacy model's own reads are documented in terms of "the
//! specified Elasticsearch domain" — the domain, not the domain this API version happened to
//! create. The legacy surface reports the engine release the domain runs as its
//! `ElasticsearchVersion`, whichever engine that release belongs to, which is also what
//! `ListDomainNames` carries its `EngineType` filter for.
//!
//! The rows differ along what the rule generalises over: which surface created the domain,
//! the engine release it runs (an OpenSearch release and an Elasticsearch one, both created
//! through the modern API), how many log types it publishes (two, one, none), whether it
//! carries tags, and the read taken against it — the single describe, the batch describe,
//! the configuration read, the listing, the tag read and the compatible-version read. A name
//! NEITHER surface holds is the control row: still the legacy not-found refusal.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The Region every request below is signed for.
const REGION: &str = "us-east-1";

/// The status the legacy surface answers a name it resolves no domain under, and the shape
/// that refusal carries.
const NOT_FOUND_STATUS: u16 = 404;
const NOT_FOUND: &str = "ResourceNotFoundException";

/// The service principal Amazon OpenSearch Service delivers a domain's logs under, which is
/// what each destination group's resource policy has to admit.
const DELIVERY_PRINCIPAL: &str = "es.amazonaws.com";

/// The two calls that delivery is made of, each needing its own grant.
const DELIVERY_ACTIONS: [&str; 2] = ["logs:CreateLogStream", "logs:PutLogEvents"];

/// The log groups the rows publish to.
const INDEX_SLOW_GROUP: &str = "/aws/opensearch/domains/shared-index-slow";
const SEARCH_SLOW_GROUP: &str = "/aws/opensearch/domains/shared-search-slow";

/// Amazon OpenSearch Service, the legacy Amazon Elasticsearch Service and Amazon CloudWatch
/// Logs over ONE shared engine, as `core/router/src/lib.rs` mounts them.
fn router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_opensearch_amazonopensearchservice::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_elasticsearchservice_amazonelasticsearchservice2015::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(
        service_com_amazonaws_cloudwatchlogs_logs_20140328::EmulatedService::new(engine),
    );
    router
}

/// A SigV4 `Authorization` whose credential scope names `signing_name`, which is what
/// routes the request. The signature is never checked here.
fn authorization(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{signing_name}/aws4_request, \
         SignedHeaders=host, Signature=0000"
    )
}

/// One `restJson1` call to the `es` endpoint — either API version, since both are bound
/// under it — as its status, its `x-amzn-errortype` and its parsed body.
async fn call(
    router: &MultiServiceRouter,
    method: &str,
    path: &str,
    body: Option<&Value>,
) -> (u16, Option<String>, Value) {
    let mut builder = http::Request::builder()
        .method(method)
        .uri(format!("https://es.{REGION}.amazonaws.com{path}"))
        .header("host", format!("es.{REGION}.amazonaws.com"))
        .header("authorization", authorization("es"));
    let payload = match body {
        Some(body) => {
            builder = builder.header("content-type", "application/json");
            Bytes::from(body.to_string())
        }
        None => Bytes::new(),
    };
    let request = builder.body(payload).expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let error = response
        .headers()
        .get("x-amzn-errortype")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    let parsed: Value = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    (status, error, parsed)
}

/// One call that must answer HTTP 200, as its parsed body.
async fn served(
    router: &MultiServiceRouter,
    case: &str,
    method: &str,
    path: &str,
    body: Option<&Value>,
) -> Value {
    let (status, error, parsed) = call(router, method, path, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from {method} {path}, got {status} ({error:?}) with \
         {parsed}"
    );
    parsed
}

/// One CloudWatch Logs call (`awsJson1_1`, `Logs_20140328.<Operation>`) that must answer
/// 200.
async fn logs(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("logs.{REGION}.amazonaws.com"))
        .header("authorization", authorization("logs"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("Logs_20140328.{operation}"))
        .body(Bytes::from(body.to_string()))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed: Value = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CloudWatch Logs {operation}, got {status} with \
         {parsed}"
    );
    parsed
}

/// A log group CloudWatch Logs holds, whose own resource policy lets this service deliver
/// into it, so an enabled option naming it is a configuration the create accepts. Answers
/// the group's ARN, read back off the group rather than built here.
async fn given_granted_group(router: &MultiServiceRouter, name: &str) -> String {
    let case = "granting a destination group";
    logs(router, case, "CreateLogGroup", &json!({ "logGroupName": name })).await;
    let described = logs(
        router,
        case,
        "DescribeLogGroups",
        &json!({ "logGroupNamePrefix": name }),
    )
    .await;
    let arn = described["logGroups"][0]["arn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected the group's arn, got {described}"))
        .to_string();
    let under_the_group = match arn.strip_suffix(":*") {
        Some(group) => format!("{group}:*"),
        None => format!("{arn}:*"),
    };
    let document = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "Service": DELIVERY_PRINCIPAL },
            "Action": DELIVERY_ACTIONS.to_vec(),
            "Resource": under_the_group,
        }],
    });
    logs(
        router,
        case,
        "PutResourcePolicy",
        &json!({
            "policyName": format!("{name}-opensearch"),
            "policyDocument": document.to_string(),
            "resourceArn": arn,
        }),
    )
    .await;
    arn.strip_suffix(":*").unwrap_or(&arn).to_string()
}

/// One domain a row creates through the MODERN API, and what the legacy surface must then
/// report for it.
struct Row {
    case: &'static str,
    name: &'static str,
    /// The engine release the domain is CREATED on, as the modern API spells it.
    engine_version: &'static str,
    /// The same release as the LEGACY surface spells it — its `ElasticsearchVersion`, whose
    /// own pattern admits `X.Y`, `OpenSearch_X.Y` and `OS_X.Y`
    /// (`smithy_query(query="shape",shape="ElasticsearchVersionString")`), so a release of
    /// the Elasticsearch line carries the engine's name on the modern surface only. It is
    /// also the source version the upgrade paths are computed from.
    reported_version: &'static str,
    /// The engine the release belongs to, which is what `ListDomainNames` filters on.
    engine_type: &'static str,
    /// The log types the domain publishes, keyed by `LogType`, or `None` for a domain that
    /// publishes none.
    log_publishing: Option<Value>,
    /// The tags the create carries, as `(key, value)` pairs.
    tags: Vec<(&'static str, &'static str)>,
}

/// The table: domains of the same account and Region, created through the modern API,
/// differing in the release they run, what they publish and whether they carry tags.
fn rows(index_slow: &str, search_slow: &str) -> Vec<Row> {
    vec![
        Row {
            case: "an OpenSearch-release domain publishing two log types, tagged",
            name: "shared-audit-domain",
            engine_version: "OpenSearch_2.11",
            reported_version: "OpenSearch_2.11",
            engine_type: "OpenSearch",
            log_publishing: Some(json!({
                "INDEX_SLOW_LOGS": { "Enabled": true, "CloudWatchLogsLogGroupArn": index_slow },
                "SEARCH_SLOW_LOGS": { "Enabled": true, "CloudWatchLogsLogGroupArn": search_slow },
            })),
            tags: vec![("estate", "search"), ("owner", "platform")],
        },
        Row {
            case: "an Elasticsearch-release domain publishing one log type",
            name: "shared-legacy-release",
            // The modern API creates domains on either line: `EngineVersion`'s pattern
            // admits `Elasticsearch_X.Y` beside `OpenSearch_X.Y`.
            engine_version: "Elasticsearch_7.10",
            reported_version: "7.10",
            engine_type: "Elasticsearch",
            log_publishing: Some(json!({
                "ES_APPLICATION_LOGS": { "Enabled": true, "CloudWatchLogsLogGroupArn": index_slow },
            })),
            tags: vec![],
        },
        Row {
            case: "a domain publishing nothing, untagged",
            name: "shared-quiet-domain",
            engine_version: "OpenSearch_2.11",
            reported_version: "OpenSearch_2.11",
            engine_type: "OpenSearch",
            log_publishing: None,
            tags: vec![],
        },
    ]
}

/// Create one row's domain through the MODERN API and answer the `DomainStatus` it reported.
async fn created(router: &MultiServiceRouter, row: &Row) -> Value {
    let mut body = json!({
        "DomainName": row.name,
        "EngineVersion": row.engine_version,
        "ClusterConfig": { "InstanceType": "r6g.large.search", "InstanceCount": 2 },
        "EBSOptions": { "EBSEnabled": true, "VolumeType": "gp3", "VolumeSize": 20 },
    });
    if let Some(options) = &row.log_publishing {
        body["LogPublishingOptions"] = options.clone();
    }
    if !row.tags.is_empty() {
        body["TagList"] = Value::Array(
            row.tags
                .iter()
                .map(|(key, value)| json!({ "Key": key, "Value": value }))
                .collect(),
        );
    }
    let created = served(
        router,
        row.case,
        "POST",
        "/2021-01-01/opensearch/domain",
        Some(&body),
    )
    .await;
    created["DomainStatus"].clone()
}

#[tokio::test]
async fn the_legacy_surface_reads_a_domain_the_modern_surface_created() {
    let router = router();
    let index_slow = given_granted_group(&router, INDEX_SLOW_GROUP).await;
    let search_slow = given_granted_group(&router, SEARCH_SLOW_GROUP).await;
    let table = rows(&index_slow, &search_slow);

    for row in &table {
        let modern = created(&router, row).await;
        let case = row.case;

        // 1. The SINGLE describe of the legacy surface reports the same resource: the same
        //    identity, the same endpoint, and the configuration the modern create recorded.
        let legacy = served(
            &router,
            case,
            "GET",
            &format!("/2015-01-01/es/domain/{}", row.name),
            None,
        )
        .await;
        let status = &legacy["DomainStatus"];
        for (member, expected) in [
            ("DomainName", modern["DomainName"].clone()),
            ("DomainId", modern["DomainId"].clone()),
            ("ARN", modern["ARN"].clone()),
            ("Created", modern["Created"].clone()),
        ] {
            assert_eq!(
                status[member], expected,
                "case {case:?}: the legacy describe must report {member} as the modern one \
                 does, got {status}"
            );
        }
        assert_eq!(
            status["ElasticsearchVersion"].as_str(),
            Some(row.reported_version),
            "case {case:?}: the legacy describe must report the release the domain runs, got \
             {status}"
        );
        assert_eq!(
            status["ElasticsearchClusterConfig"]["InstanceCount"],
            modern["ClusterConfig"]["InstanceCount"],
            "case {case:?}: the legacy describe must report the cluster the domain runs, got \
             {status}"
        );
        assert_eq!(
            status.get("LogPublishingOptions").cloned(),
            row.log_publishing.clone(),
            "case {case:?}: the legacy describe must report the same log publishing options, \
             got {status}"
        );

        // 2. The BATCH describe resolves the same name the same way.
        let batch = served(
            &router,
            case,
            "POST",
            "/2015-01-01/es/domain-info",
            Some(&json!({ "DomainNames": [row.name] })),
        )
        .await;
        assert_eq!(
            batch["DomainStatusList"][0]["DomainName"].as_str(),
            Some(row.name),
            "case {case:?}: the legacy batch describe must report the domain, got {batch}"
        );
        assert_eq!(
            batch["DomainStatusList"][0]
                .get("LogPublishingOptions")
                .cloned(),
            row.log_publishing.clone(),
            "case {case:?}: the legacy batch describe must report the same log publishing \
             options, got {batch}"
        );

        // 3. The CONFIGURATION read reports every option wrapped in its own status, the
        //    log-publishing container among them.
        let config = served(
            &router,
            case,
            "GET",
            &format!("/2015-01-01/es/domain/{}/config", row.name),
            None,
        )
        .await;
        let config = &config["DomainConfig"];
        assert_eq!(
            config["ElasticsearchVersion"]["Options"].as_str(),
            Some(row.reported_version),
            "case {case:?}: the legacy config read must report the release, got {config}"
        );
        let published = config["LogPublishingOptions"]["Options"].clone();
        let published = (!published.is_null()
            && published.as_object().is_some_and(|types| !types.is_empty()))
        .then_some(published);
        assert_eq!(
            published,
            row.log_publishing.clone(),
            "case {case:?}: the legacy config read must report the same log publishing \
             options, got {config}"
        );
        assert!(
            config["ElasticsearchClusterConfig"]["Status"]["UpdateVersion"].is_number(),
            "case {case:?}: every option of the legacy config read carries its own status, \
             got {config}"
        );

        // 4. The TAG read reports the tags the modern create carried.
        let arn = modern["ARN"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: expected the domain's ARN, got {modern}"));
        let tags = served(
            &router,
            case,
            "GET",
            &format!("/2015-01-01/tags?arn={arn}"),
            None,
        )
        .await;
        let mut read: Vec<(String, String)> = tags["TagList"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .map(|tag| {
                (
                    tag["Key"].as_str().unwrap_or_default().to_string(),
                    tag["Value"].as_str().unwrap_or_default().to_string(),
                )
            })
            .collect();
        read.sort();
        let mut expected: Vec<(String, String)> = row
            .tags
            .iter()
            .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
            .collect();
        expected.sort();
        assert_eq!(
            read, expected,
            "case {case:?}: the legacy tag read must report the tags the create carried, got \
             {tags}"
        );

        // 5. The COMPATIBLE-VERSION read computes the upgrade paths from the release THAT
        //    record holds.
        let compatible = served(
            &router,
            case,
            "GET",
            &format!("/2015-01-01/es/compatibleVersions?domainName={}", row.name),
            None,
        )
        .await;
        assert_eq!(
            compatible["CompatibleElasticsearchVersions"][0]["SourceVersion"].as_str(),
            Some(row.reported_version),
            "case {case:?}: the legacy compatible-version read must start from the release \
             the domain runs, got {compatible}"
        );
    }

    // 6. The LISTING reports the domains of this account and Region whichever API version
    //    created them, and its `EngineType` filter selects on the engine each one's release
    //    belongs to.
    let listed = served(&router, "the listing", "GET", "/2015-01-01/domain", None).await;
    let names: Vec<&str> = listed["DomainNames"]
        .as_array()
        .map(|entries| {
            entries
                .iter()
                .filter_map(|entry| entry["DomainName"].as_str())
                .collect()
        })
        .unwrap_or_default();
    for row in &table {
        assert!(
            names.contains(&row.name),
            "case {:?}: the legacy listing must name the domain, got {listed}",
            row.case
        );
    }
    for engine_type in ["OpenSearch", "Elasticsearch"] {
        let filtered = served(
            &router,
            engine_type,
            "GET",
            &format!("/2015-01-01/domain?engineType={engine_type}"),
            None,
        )
        .await;
        let selected: Vec<&str> = filtered["DomainNames"]
            .as_array()
            .map(|entries| {
                entries
                    .iter()
                    .filter_map(|entry| entry["DomainName"].as_str())
                    .collect()
            })
            .unwrap_or_default();
        for row in &table {
            assert_eq!(
                selected.contains(&row.name),
                row.engine_type == engine_type,
                "case {:?}: the {engine_type} listing must {} the domain, got {filtered}",
                row.case,
                if row.engine_type == engine_type {
                    "name"
                } else {
                    "leave out"
                }
            );
        }
    }

    // 7. The control row: a name NEITHER surface holds is still the legacy not-found
    //    refusal, which is what the resolution above must not make unreachable.
    let (status, error, body) = call(
        &router,
        "GET",
        "/2015-01-01/es/domain/no-such-shared-domain",
        None,
    )
    .await;
    assert_eq!(
        status, NOT_FOUND_STATUS,
        "case \"a name neither surface holds\": expected HTTP {NOT_FOUND_STATUS}, got {status} \
         with {body}"
    );
    assert_eq!(
        error.as_deref(),
        Some(NOT_FOUND),
        "case \"a name neither surface holds\": expected {NOT_FOUND}, got {error:?} with {body}"
    );
    assert_eq!(
        body["message"].as_str(),
        Some("Domain not found: no-such-shared-domain"),
        "case \"a name neither surface holds\": expected the not-found sentence, got {body}"
    );
}

#[tokio::test]
async fn the_legacy_surface_still_reads_its_own_domains() {
    let router = router();

    // A domain created through the LEGACY API, which the resolution above must not displace:
    // this surface's own store answers first, and a name it holds is reported from there.
    let name = "legacy-own-domain";
    let created = served(
        &router,
        "a legacy-created domain",
        "POST",
        "/2015-01-01/es/domain",
        Some(&json!({
            "DomainName": name,
            "ElasticsearchVersion": "7.10",
            "ElasticsearchClusterConfig": {
                "InstanceType": "m5.large.elasticsearch",
                "InstanceCount": 1,
            },
            "TagList": [{ "Key": "estate", "Value": "legacy" }],
        })),
    )
    .await;
    let arn = created["DomainStatus"]["ARN"]
        .as_str()
        .expect("the created domain's ARN")
        .to_string();

    let described = served(
        &router,
        "a legacy-created domain",
        "GET",
        &format!("/2015-01-01/es/domain/{name}"),
        None,
    )
    .await;
    assert_eq!(
        described["DomainStatus"]["ElasticsearchVersion"].as_str(),
        Some("7.10"),
        "case \"a legacy-created domain\": expected the release the create named, got \
         {described}"
    );
    let tags = served(
        &router,
        "a legacy-created domain",
        "GET",
        &format!("/2015-01-01/tags?arn={arn}"),
        None,
    )
    .await;
    assert_eq!(
        tags["TagList"][0]["Key"].as_str(),
        Some("estate"),
        "case \"a legacy-created domain\": expected the tags the create carried, got {tags}"
    );
    let listed = served(
        &router,
        "a legacy-created domain",
        "GET",
        "/2015-01-01/domain",
        None,
    )
    .await;
    assert_eq!(
        listed["DomainNames"][0]["EngineType"].as_str(),
        Some("Elasticsearch"),
        "case \"a legacy-created domain\": a bare X.Y release is an Elasticsearch domain, got \
         {listed}"
    );
}
