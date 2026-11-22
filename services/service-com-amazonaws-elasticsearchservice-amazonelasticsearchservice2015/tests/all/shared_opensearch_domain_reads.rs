// SPDX-License-Identifier: CC0-1.0

//! What THIS surface reports for a domain created through the OTHER API version of the same
//! service — the 2021-01-01 OpenSearch API — with both mounted on one shared engine, driven
//! over the real wire path so what is asserted is what a client sees.
//!
//! The RULE: **a domain is ONE resource of one account and Region, published through two API
//! versions. Which version a client speaks decides the SHAPE it reads the domain in, never
//! whether the domain is there: every read of this surface resolves a domain the other
//! surface created and reports the configuration that create recorded.** The reads are this
//! surface's own — the single describe, the batch describe, the configuration read, the
//! listing, the tag read and the compatible-version read — and each resolves the name the
//! same way ([`service_com_amazonaws_elasticsearchservice_amazonelasticsearchservice2015::state::opensearch_surface`]).
//!
//! The release is reported in the spelling THIS surface's own member admits:
//! `ElasticsearchVersionString`'s pattern is `X.Y`, `OpenSearch_X.Y` or `OS_X.Y`
//! (`smithy_query(query="shape",shape="ElasticsearchVersionString")`), so a domain on an
//! Elasticsearch release reports a bare `X.Y` here while the other surface spells the same
//! release `Elasticsearch_X.Y`.
//!
//! The rows differ along what the rule generalises over: the engine line the release belongs
//! to and so its spelling on each surface, the cluster the domain runs, how many log types it
//! carries, and whether it is tagged. Two control rows close the rule: a domain created
//! through THIS surface is still read from this surface's own store, and a name neither
//! surface holds is still the not-found refusal.

use std::sync::Arc;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::harness::{call_in, Answer};

/// The Region every request below is signed for.
const REGION: &str = "us-east-1";

/// The status this surface answers a name it resolves no domain under.
const NOT_FOUND_STATUS: u16 = 404;

/// The shape that refusal carries.
const NOT_FOUND: &str = "ResourceNotFoundException";

/// Both API versions of this service over ONE shared engine, which is how the emulator
/// mounts them: the modern surface writes the domain and this one reads it.
fn both_surfaces() -> MultiServiceRouter {
    let engine = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.register_service(
        service_com_amazonaws_elasticsearchservice_amazonelasticsearchservice2015::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(
        service_com_amazonaws_opensearch_amazonopensearchservice::EmulatedService::new(engine),
    );
    router
}

/// One call that must answer HTTP 200, as its parsed body.
async fn served(
    router: &MultiServiceRouter,
    case: &str,
    method: &str,
    path: &str,
    body: Option<&Value>,
) -> Value {
    let answer: Answer = call_in(router, REGION, method, path, body).await;
    assert_eq!(
        answer.status, 200,
        "case {case:?}: expected HTTP 200 from {method} {path}, got {} ({}) with {}",
        answer.status, answer.error_type, answer.body
    );
    answer.body
}

/// One domain a row creates through the MODERN API, and what this surface must report for
/// it.
struct Row {
    case: &'static str,
    name: &'static str,
    /// The release the modern create names.
    engine_version: &'static str,
    /// The same release in the spelling THIS surface's `ElasticsearchVersion` admits.
    reported_version: &'static str,
    /// The engine the release belongs to, which is what `ListDomainNames` filters on.
    engine_type: &'static str,
    instance_count: i64,
    /// The log types the domain carries, keyed by `LogType`, or `None` for a domain that
    /// carries none. A DISABLED option publishes nothing, so it needs no destination the
    /// CloudWatch Logs service holds — which is why these rows need no log group.
    log_publishing: Option<Value>,
    /// The tags the create carries.
    tags: Vec<(&'static str, &'static str)>,
}

/// The table.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "an OpenSearch-release domain carrying two log types, tagged",
            name: "crossread-opensearch",
            engine_version: "OpenSearch_2.11",
            reported_version: "OpenSearch_2.11",
            engine_type: "OpenSearch",
            instance_count: 2,
            log_publishing: Some(json!({
                "INDEX_SLOW_LOGS": {
                    "Enabled": false,
                    "CloudWatchLogsLogGroupArn":
                        "arn:aws:logs:us-east-1:000000000000:log-group:/aws/opensearch/index",
                },
                "SEARCH_SLOW_LOGS": {
                    "Enabled": false,
                    "CloudWatchLogsLogGroupArn":
                        "arn:aws:logs:us-east-1:000000000000:log-group:/aws/opensearch/search",
                },
            })),
            tags: vec![("estate", "search"), ("owner", "platform")],
        },
        Row {
            // The modern API creates domains on either engine line: `EngineVersion`'s
            // pattern admits `Elasticsearch_X.Y` beside `OpenSearch_X.Y`, and this surface
            // reports that release in its own spelling.
            case: "an Elasticsearch-release domain carrying one log type",
            name: "crossread-legacy-line",
            engine_version: "Elasticsearch_7.10",
            reported_version: "7.10",
            engine_type: "Elasticsearch",
            instance_count: 1,
            log_publishing: Some(json!({
                "ES_APPLICATION_LOGS": {
                    "Enabled": false,
                    "CloudWatchLogsLogGroupArn":
                        "arn:aws:logs:us-east-1:000000000000:log-group:/aws/opensearch/app",
                },
            })),
            tags: vec![],
        },
        Row {
            case: "a domain carrying no log publishing at all, untagged",
            name: "crossread-quiet",
            engine_version: "OpenSearch_2.19",
            reported_version: "OpenSearch_2.19",
            engine_type: "OpenSearch",
            instance_count: 3,
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
        "ClusterConfig": {
            "InstanceType": "r6g.large.search",
            "InstanceCount": row.instance_count,
        },
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

/// The tag set `tags` carries, as sorted pairs, so two reads of one domain's tags compare
/// by what they hold rather than by the order they were written in.
fn tag_pairs(tags: &Value) -> Vec<(String, String)> {
    let mut pairs: Vec<(String, String)> = tags
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
    pairs.sort();
    pairs
}

#[tokio::test]
async fn every_read_of_this_surface_resolves_a_domain_the_other_api_version_created() {
    let router = both_surfaces();
    let table = rows();

    for row in &table {
        let modern = created(&router, row).await;
        let case = row.case;

        // The SINGLE describe reports the same resource: the same identity, and the
        // configuration the modern create recorded.
        let described = served(
            &router,
            case,
            "GET",
            &format!("/2015-01-01/es/domain/{}", row.name),
            None,
        )
        .await;
        let status = &described["DomainStatus"];
        for member in ["DomainName", "DomainId", "ARN", "Created", "Endpoint"] {
            assert_eq!(
                status[member], modern[member],
                "case {case:?}: the describe must report {member} as the modern surface does, \
                 got {status}"
            );
        }
        assert_eq!(
            status["ElasticsearchVersion"].as_str(),
            Some(row.reported_version),
            "case {case:?}: the describe must report the release the domain runs, in this \
             surface's own spelling, got {status}"
        );
        assert_eq!(
            status["ElasticsearchClusterConfig"]["InstanceCount"].as_i64(),
            Some(row.instance_count),
            "case {case:?}: the describe must report the cluster the domain runs, got {status}"
        );
        assert_eq!(
            status.get("LogPublishingOptions").cloned(),
            row.log_publishing.clone(),
            "case {case:?}: the describe must report the same log publishing options, got \
             {status}"
        );

        // The BATCH describe resolves the same name the same way.
        let batch = served(
            &router,
            case,
            "POST",
            "/2015-01-01/es/domain-info",
            Some(&json!({ "DomainNames": [row.name] })),
        )
        .await;
        assert_eq!(
            batch["DomainStatusList"][0], *status,
            "case {case:?}: the batch describe must report what the single describe does, got \
             {batch}"
        );

        // The CONFIGURATION read reports every option wrapped in its own status.
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
            "case {case:?}: the config read must report the release, got {config}"
        );
        assert!(
            config["ElasticsearchClusterConfig"]["Status"]["UpdateVersion"].is_number(),
            "case {case:?}: every option of the config read carries its own status, got \
             {config}"
        );
        let carried = config["LogPublishingOptions"]["Options"].clone();
        let carried = carried
            .as_object()
            .is_some_and(|types| !types.is_empty())
            .then_some(carried);
        assert_eq!(
            carried,
            row.log_publishing.clone(),
            "case {case:?}: the config read must report the same log publishing options, got \
             {config}"
        );

        // The TAG read reports the tags the modern create carried.
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
        let expected: Vec<(String, String)> = {
            let mut pairs: Vec<(String, String)> = row
                .tags
                .iter()
                .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
                .collect();
            pairs.sort();
            pairs
        };
        assert_eq!(
            tag_pairs(&tags["TagList"]),
            expected,
            "case {case:?}: the tag read must report the tags the create carried, got {tags}"
        );

        // The COMPATIBLE-VERSION read computes the upgrade paths from the release THAT
        // record holds.
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
            "case {case:?}: the compatible-version read must start from the release the \
             domain runs, got {compatible}"
        );
    }

    // The LISTING names the domains of this account and Region whichever API version
    // created them, and its `EngineType` filter selects on the engine each release belongs
    // to.
    for engine_type in [None, Some("OpenSearch"), Some("Elasticsearch")] {
        let path = match engine_type {
            Some(wanted) => format!("/2015-01-01/domain?engineType={wanted}"),
            None => "/2015-01-01/domain".to_string(),
        };
        let listed = served(&router, "the listing", "GET", &path, None).await;
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
            let wanted = engine_type.is_none_or(|wanted| wanted == row.engine_type);
            assert_eq!(
                names.contains(&row.name),
                wanted,
                "case {:?}: the listing {path} must {} the domain, got {listed}",
                row.case,
                if wanted { "name" } else { "leave out" }
            );
        }
    }

    // A name NEITHER surface holds is still this surface's not-found refusal, which the
    // resolution above must not make unreachable.
    let missing = call_in(
        &router,
        REGION,
        "GET",
        "/2015-01-01/es/domain/crossread-absent",
        None,
    )
    .await;
    assert_eq!(
        missing.status, NOT_FOUND_STATUS,
        "case \"a name neither surface holds\": expected HTTP {NOT_FOUND_STATUS}, got {} with {}",
        missing.status, missing.body
    );
    assert!(
        missing.error_type.contains(NOT_FOUND),
        "case \"a name neither surface holds\": expected {NOT_FOUND}, got {:?} with {}",
        missing.error_type,
        missing.body
    );
    assert_eq!(
        missing.body["message"].as_str(),
        Some("Domain not found: crossread-absent"),
        "case \"a name neither surface holds\": expected the not-found sentence, got {}",
        missing.body
    );
}

#[tokio::test]
async fn a_domain_created_through_this_surface_is_still_read_from_this_surface() {
    let router = both_surfaces();

    // The control row: this surface's own store answers first, so a domain created HERE is
    // reported from there — the resolution of the other surface's store displaces nothing.
    let name = "crossread-own-domain";
    let created = served(
        &router,
        "a domain created through this surface",
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
        "a domain created through this surface",
        "GET",
        &format!("/2015-01-01/es/domain/{name}"),
        None,
    )
    .await;
    assert_eq!(
        described["DomainStatus"]["ElasticsearchVersion"].as_str(),
        Some("7.10"),
        "expected the release the create named, got {described}"
    );
    assert_eq!(
        described["DomainStatus"]["ElasticsearchClusterConfig"]["InstanceType"].as_str(),
        Some("m5.large.elasticsearch"),
        "expected the cluster the create named, got {described}"
    );

    let tags = served(
        &router,
        "a domain created through this surface",
        "GET",
        &format!("/2015-01-01/tags?arn={arn}"),
        None,
    )
    .await;
    assert_eq!(
        tag_pairs(&tags["TagList"]),
        vec![("estate".to_string(), "legacy".to_string())],
        "expected the tags the create carried, got {tags}"
    );

    let listed = served(
        &router,
        "a domain created through this surface",
        "GET",
        "/2015-01-01/domain",
        None,
    )
    .await;
    assert_eq!(
        listed["DomainNames"],
        json!([{ "DomainName": name, "EngineType": "Elasticsearch" }]),
        "expected the listing to name this surface's own domain once, got {listed}"
    );
}
