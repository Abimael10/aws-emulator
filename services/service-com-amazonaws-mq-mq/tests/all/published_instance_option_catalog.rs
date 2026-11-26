// SPDX-License-Identifier: CC0-1.0

//! The POPULATION `DescribeBrokerInstanceOptions` reports, driven over the wire.
//!
//! RULE: the unfiltered catalog enumerates EVERY (engine type, host instance type,
//! storage type) combination Amazon MQ offers — one entry per host instance type each
//! engine is published on, with the storage types and deployment modes legal for that
//! engine on that size — so a client enumerating the catalog discovers every size it may
//! ask `CreateBroker` for, and each entry's `supportedEngineVersions` is drawn from the
//! engine catalog `DescribeBrokerEngineTypes` publishes for that engine, narrowed to the
//! release lines the entry's size runs.
//!
//! The sibling suite `broker_instance_options.rs` covers the SHAPE of an entry, the
//! filters and the paging, deriving what it expects from the served catalog; this suite
//! covers which combinations are served at all, and with which versions, which no
//! assertion derived from them can.
//!
//! WHICH combinations the service offers, and which version lines each reports, is
//! published rather than modelled (the model enumerates only `EngineType`,
//! `BrokerStorageType` and `DeploymentMode`, and no version string), so the table below is
//! written out from the recorded answers of this service's own catalogs plus Amazon MQ's
//! published instance-type tables (the Developer Guide's "Broker instance types", one per
//! engine): ActiveMQ on the burstable `micro` size and the `mq.m5` sizes, RabbitMQ on the
//! `mq.m5` sizes and the whole Graviton `mq.m7g` family. It spans every family and every
//! size in it, not one cited combination.

use serde_json::{json, Value};

use service_com_amazonaws_mq_mq::ops::types::{BROKER_STORAGE_TYPE_VALUES, ENGINE_TYPE_VALUES};
use service_com_amazonaws_mq_mq::state::{
    availability_zones_in_region, published_engine_versions,
    DEPLOYMENT_MODE_ACTIVE_STANDBY_MULTI_AZ, DEPLOYMENT_MODE_CLUSTER_MULTI_AZ,
    DEPLOYMENT_MODE_SINGLE_INSTANCE, ENGINE_TYPE_ACTIVEMQ, ENGINE_TYPE_RABBITMQ, MAX_MAX_RESULTS,
    STORAGE_TYPE_EBS, STORAGE_TYPE_EFS,
};

use crate::broker_lifecycle::Mq;

/// The operation's modelled path (`smithy_query(query="operation",
/// operation="DescribeBrokerInstanceOptions")`: `GET /v1/broker-instance-options`).
const PATH: &str = "/v1/broker-instance-options";

/// `DescribeBrokerEngineTypes`' modelled path — the engine catalog an entry's
/// `supportedEngineVersions` has to agree with.
const ENGINE_TYPES_PATH: &str = "/v1/broker-engine-types";

/// The two storage types as the catalog renders them: the lower-case spelling Amazon MQ
/// publishes a `BrokerStorageType` in, checked against the model's own value set in
/// [`the_rendered_storage_types_are_the_modelled_values`].
const EBS: &str = "ebs";
const EFS: &str = "efs";

/// The deployment-mode sets an entry can offer, from the model's `DeploymentMode` values:
/// a single instance only, a single instance or ActiveMQ's active/standby pair, and a
/// single instance or RabbitMQ's multi-AZ cluster.
const SINGLE_ONLY: &[&str] = &[DEPLOYMENT_MODE_SINGLE_INSTANCE];
const OR_PAIR: &[&str] = &[
    DEPLOYMENT_MODE_SINGLE_INSTANCE,
    DEPLOYMENT_MODE_ACTIVE_STANDBY_MULTI_AZ,
];
const OR_CLUSTER: &[&str] = &[
    DEPLOYMENT_MODE_SINGLE_INSTANCE,
    DEPLOYMENT_MODE_CLUSTER_MULTI_AZ,
];

/// One published combination: the host instance type, the engine offered on it, the
/// storage type of the entry, the deployment modes that combination offers and the engine
/// version lines it reports.
type Row = (
    &'static str,
    &'static str,
    &'static str,
    &'static [&'static str],
    &'static [&'static str],
);

/// The engine version lines the recorded instance-option catalog reports, by engine and
/// instance family — ActiveMQ's one list, and RabbitMQ's two: the Graviton sizes report its
/// current major release line, the older general-purpose sizes its earlier line only. Each
/// list is a selection of the engine's own published list (the one
/// `DescribeBrokerEngineTypes` publishes), checked in
/// [`the_recorded_version_lines_are_selections_of_the_engine_catalog`].
const ACTIVEMQ_VERSIONS: &[&str] = &["5.19", "5.18"];
const RABBITMQ_GRAVITON_VERSIONS: &[&str] = &["4.3", "4.2", "3.13"];
const RABBITMQ_LEGACY_VERSIONS: &[&str] = &["3.13"];

/// Amazon MQ's whole published instance-option catalog, in the order the service reports
/// it — by host instance type, then by engine type, then by storage type. Sources: the
/// Developer Guide's "Broker instance types" tables (one per engine) for which sizes each
/// engine is offered on; `BrokerStorageType`'s own documentation ("EFS is not supported
/// for RabbitMQ engine type") for the storage column; `DeploymentMode` for the modes —
/// ActiveMQ's active/standby pair on its shared file system, RabbitMQ's cluster on every
/// size but the burstable one; and the recorded `brokerInstanceOptions` answer for the
/// version column.
const PUBLISHED_CATALOG: &[Row] = &[
    (
        "mq.m5.2xlarge",
        ENGINE_TYPE_ACTIVEMQ,
        EBS,
        SINGLE_ONLY,
        ACTIVEMQ_VERSIONS,
    ),
    (
        "mq.m5.2xlarge",
        ENGINE_TYPE_ACTIVEMQ,
        EFS,
        OR_PAIR,
        ACTIVEMQ_VERSIONS,
    ),
    (
        "mq.m5.2xlarge",
        ENGINE_TYPE_RABBITMQ,
        EBS,
        OR_CLUSTER,
        RABBITMQ_LEGACY_VERSIONS,
    ),
    (
        "mq.m5.4xlarge",
        ENGINE_TYPE_ACTIVEMQ,
        EBS,
        SINGLE_ONLY,
        ACTIVEMQ_VERSIONS,
    ),
    (
        "mq.m5.4xlarge",
        ENGINE_TYPE_ACTIVEMQ,
        EFS,
        OR_PAIR,
        ACTIVEMQ_VERSIONS,
    ),
    (
        "mq.m5.4xlarge",
        ENGINE_TYPE_RABBITMQ,
        EBS,
        OR_CLUSTER,
        RABBITMQ_LEGACY_VERSIONS,
    ),
    (
        "mq.m5.large",
        ENGINE_TYPE_ACTIVEMQ,
        EBS,
        SINGLE_ONLY,
        ACTIVEMQ_VERSIONS,
    ),
    (
        "mq.m5.large",
        ENGINE_TYPE_ACTIVEMQ,
        EFS,
        OR_PAIR,
        ACTIVEMQ_VERSIONS,
    ),
    (
        "mq.m5.large",
        ENGINE_TYPE_RABBITMQ,
        EBS,
        OR_CLUSTER,
        RABBITMQ_LEGACY_VERSIONS,
    ),
    (
        "mq.m5.xlarge",
        ENGINE_TYPE_ACTIVEMQ,
        EBS,
        SINGLE_ONLY,
        ACTIVEMQ_VERSIONS,
    ),
    (
        "mq.m5.xlarge",
        ENGINE_TYPE_ACTIVEMQ,
        EFS,
        OR_PAIR,
        ACTIVEMQ_VERSIONS,
    ),
    (
        "mq.m5.xlarge",
        ENGINE_TYPE_RABBITMQ,
        EBS,
        OR_CLUSTER,
        RABBITMQ_LEGACY_VERSIONS,
    ),
    (
        "mq.m7g.12xlarge",
        ENGINE_TYPE_RABBITMQ,
        EBS,
        OR_CLUSTER,
        RABBITMQ_GRAVITON_VERSIONS,
    ),
    (
        "mq.m7g.16xlarge",
        ENGINE_TYPE_RABBITMQ,
        EBS,
        OR_CLUSTER,
        RABBITMQ_GRAVITON_VERSIONS,
    ),
    (
        "mq.m7g.2xlarge",
        ENGINE_TYPE_RABBITMQ,
        EBS,
        OR_CLUSTER,
        RABBITMQ_GRAVITON_VERSIONS,
    ),
    (
        "mq.m7g.4xlarge",
        ENGINE_TYPE_RABBITMQ,
        EBS,
        OR_CLUSTER,
        RABBITMQ_GRAVITON_VERSIONS,
    ),
    (
        "mq.m7g.8xlarge",
        ENGINE_TYPE_RABBITMQ,
        EBS,
        OR_CLUSTER,
        RABBITMQ_GRAVITON_VERSIONS,
    ),
    (
        "mq.m7g.large",
        ENGINE_TYPE_RABBITMQ,
        EBS,
        OR_CLUSTER,
        RABBITMQ_GRAVITON_VERSIONS,
    ),
    (
        "mq.m7g.medium",
        ENGINE_TYPE_RABBITMQ,
        EBS,
        OR_CLUSTER,
        RABBITMQ_GRAVITON_VERSIONS,
    ),
    (
        "mq.m7g.xlarge",
        ENGINE_TYPE_RABBITMQ,
        EBS,
        OR_CLUSTER,
        RABBITMQ_GRAVITON_VERSIONS,
    ),
    (
        "mq.t3.micro",
        ENGINE_TYPE_ACTIVEMQ,
        EFS,
        OR_PAIR,
        ACTIVEMQ_VERSIONS,
    ),
];

/// The Graviton instance family: Amazon MQ spells a size `mq.<family>.<size>`, and this is
/// the family whose RabbitMQ entries report the engine's current release line.
const GRAVITON_FAMILY_PREFIX: &str = "mq.m7g.";

/// The version lines the recorded catalog reports for an entry of `engine_type` on
/// `host_instance_type` — the table's version column, empty for a combination the table
/// does not publish. The sibling suites that pin this member read it from here, so the
/// recorded lists live in ONE place.
pub(crate) fn recorded_versions(engine_type: &str, host_instance_type: &str) -> Vec<String> {
    PUBLISHED_CATALOG
        .iter()
        .find(|(host, engine, _, _, _)| *host == host_instance_type && *engine == engine_type)
        .map(|(_, _, _, _, versions)| versions.iter().map(|v| (*v).to_string()).collect())
        .unwrap_or_default()
}

/// One `DescribeBrokerInstanceOptions` call, with `query` as its query string.
async fn describe(mq: &Mq, query: &str) -> (u16, Value) {
    let path = if query.is_empty() {
        PATH.to_string()
    } else {
        format!("{PATH}?{query}")
    };
    mq.call("GET", &path, None).await
}

/// The entries of a successful answer.
fn entries(case: &str, body: &Value) -> Vec<Value> {
    let Some(entries) = body.get("brokerInstanceOptions").and_then(Value::as_array) else {
        panic!("case {case:?}: expected a brokerInstanceOptions array, got {body:?}");
    };
    entries.clone()
}

/// The whole catalog of a Region, read over the wire in one page.
async fn whole_catalog(case: &str, mq: &Mq) -> Vec<Value> {
    let (status, body) = describe(mq, &format!("maxResults={MAX_MAX_RESULTS}")).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected 200 for the unfiltered catalog, got {status} {body:?}"
    );
    assert!(
        body.get("nextToken").is_none(),
        "case {case:?}: expected the whole catalog to fit one page of {MAX_MAX_RESULTS}, \
         got a nextToken in {body:?}"
    );
    entries(case, &body)
}

/// One row as the catalog reports it in `region`.
fn expected_entry(region: &str, row: &Row) -> Value {
    let (host_instance_type, engine_type, storage_type, modes, versions) = *row;
    let zones: Vec<Value> = availability_zones_in_region(region)
        .into_iter()
        .map(|name| json!({"name": name}))
        .collect();
    json!({
        "engineType": engine_type,
        "hostInstanceType": host_instance_type,
        "storageType": storage_type,
        "supportedDeploymentModes": modes,
        "supportedEngineVersions": versions,
        "availabilityZones": zones,
    })
}

/// The (host instance type, engine type, storage type) an entry names — the key that says
/// WHICH combination it is.
fn combination(entry: &Value) -> (String, String, String) {
    let member = |name: &str| {
        entry
            .get(name)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    };
    (
        member("hostInstanceType"),
        member("engineType"),
        member("storageType"),
    )
}

/// The versions one entry reports.
fn reported_versions(entry: &Value) -> Vec<String> {
    entry
        .get("supportedEngineVersions")
        .and_then(Value::as_array)
        .map(|versions| {
            versions
                .iter()
                .map(|version| version.as_str().unwrap_or_default().to_string())
                .collect()
        })
        .unwrap_or_default()
}

#[tokio::test]
async fn the_rendered_storage_types_are_the_modelled_values() {
    // The table spells `storageType` the way the catalog publishes it; the values
    // themselves are the model's (`BrokerStorageType`), so the two spellings must name
    // the same pair.
    let rendered: Vec<String> = BROKER_STORAGE_TYPE_VALUES
        .iter()
        .map(|value| value.to_ascii_lowercase())
        .collect();
    for (value, expected) in [(STORAGE_TYPE_EBS, EBS), (STORAGE_TYPE_EFS, EFS)] {
        assert_eq!(
            value.to_ascii_lowercase(),
            expected,
            "case {value:?}: the catalog's spelling of that modelled value is {expected:?}"
        );
        assert!(
            rendered.contains(&expected.to_string()),
            "case {expected:?}: must be one of the modelled storage types {rendered:?}"
        );
    }
}

#[tokio::test]
async fn the_unfiltered_catalog_enumerates_every_published_combination() {
    // Several Regions, because only the zone names of an entry follow the Region: the
    // population does not.
    for region in ["us-west-2", "us-east-1", "eu-west-1"] {
        let mq = Mq::new(region);
        let catalog = whole_catalog(region, &mq).await;
        let expected: Vec<Value> = PUBLISHED_CATALOG
            .iter()
            .map(|row| expected_entry(region, row))
            .collect();

        // Name the missing and the unexpected combinations rather than printing two whole
        // catalogs, so a divergence names the size at fault.
        let served: Vec<(String, String, String)> = catalog.iter().map(combination).collect();
        let published: Vec<(String, String, String)> = expected.iter().map(combination).collect();
        let missing: Vec<&(String, String, String)> = published
            .iter()
            .filter(|key| !served.contains(key))
            .collect();
        let unexpected: Vec<&(String, String, String)> = served
            .iter()
            .filter(|key| !published.contains(key))
            .collect();
        assert!(
            missing.is_empty() && unexpected.is_empty(),
            "case {region:?}: the catalog must enumerate every published combination; \
             missing {missing:?}, unexpected {unexpected:?}"
        );

        // Then the whole entries, in the published order, so the modes, versions and
        // zones of every combination are pinned too.
        assert_eq!(
            catalog, expected,
            "case {region:?}: expected the published catalog in the published order"
        );
    }
}

#[tokio::test]
async fn every_published_size_of_an_engine_is_discoverable_by_filter() {
    let region = "us-west-2";
    let mq = Mq::new(region);

    // One case per engine type the model enumerates: what a client narrowing the catalog
    // by engine (a sizing script choosing a `hostInstanceType` before `CreateBroker`)
    // gets to see is that engine's whole published size set.
    for engine_type in ENGINE_TYPE_VALUES {
        let expected: Vec<Value> = PUBLISHED_CATALOG
            .iter()
            .filter(|(_, engine, _, _, _)| engine == engine_type)
            .map(|row| expected_entry(region, row))
            .collect();
        assert!(
            !expected.is_empty(),
            "case {engine_type:?}: the table must publish sizes for every modelled engine"
        );
        let query = format!("maxResults={MAX_MAX_RESULTS}&engineType={engine_type}");
        let case = &query;
        let (status, body) = describe(&mq, &query).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected 200, got {status} {body:?}"
        );
        let got = entries(case, &body);
        assert_eq!(
            got, expected,
            "case {case:?}: expected the {} published options of that engine, got {}",
            expected.len(),
            got.len()
        );
    }

    // And every published combination on its own, including the sizes the recorded call
    // never named (the Graviton `large`, `medium` and `xlarge` sizes, and the burstable
    // one), so a client handed a size finds exactly the entry it may use.
    for row in PUBLISHED_CATALOG {
        let (host_instance_type, engine_type, storage_type, _, _) = *row;
        let query = format!(
            "maxResults={MAX_MAX_RESULTS}&hostInstanceType={host_instance_type}\
             &engineType={engine_type}&storageType={storage_type}"
        );
        let case = &query;
        let (status, body) = describe(&mq, &query).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected 200, got {status} {body:?}"
        );
        let got = entries(case, &body);
        assert_eq!(
            got,
            vec![expected_entry(region, row)],
            "case {case:?}: expected exactly that published option, got {got:?}"
        );
    }
}

#[tokio::test]
async fn an_entrys_versions_come_from_the_engine_catalog_the_service_publishes() {
    let region = "us-east-1";
    let mq = Mq::new(region);
    let catalog = whole_catalog(region, &mq).await;

    // The same service publishes its engine catalog through `DescribeBrokerEngineTypes`,
    // so what an instance option may report is read from THAT answer rather than written
    // out here: an entry reports a selection of its engine's published versions, in the
    // same (newest-first) order, never a version the engine catalog does not publish.
    let (status, body) = mq
        .call(
            "GET",
            &format!("{ENGINE_TYPES_PATH}?maxResults={MAX_MAX_RESULTS}"),
            None,
        )
        .await;
    assert_eq!(
        status, 200,
        "case \"engine types\": expected 200, got {status} {body:?}"
    );
    let Some(engine_types) = body.get("brokerEngineTypes").and_then(Value::as_array) else {
        panic!("case \"engine types\": expected a brokerEngineTypes array, got {body:?}");
    };
    assert_eq!(
        engine_types.len(),
        ENGINE_TYPE_VALUES.len(),
        "case \"engine types\": expected one entry per modelled engine, got {body:?}"
    );

    for engine in engine_types {
        let engine_type = engine
            .get("engineType")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let Some(versions) = engine.get("engineVersions").and_then(Value::as_array) else {
            panic!("case {engine_type:?}: expected an engineVersions array, got {engine:?}");
        };
        let published: Vec<String> = versions
            .iter()
            .map(|version| {
                version
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string()
            })
            .collect();
        assert!(
            !published.is_empty(),
            "case {engine_type:?}: the engine catalog must publish versions, got {engine:?}"
        );

        let options: Vec<&Value> = catalog
            .iter()
            .filter(|entry| entry.get("engineType").and_then(Value::as_str) == Some(&engine_type))
            .collect();
        assert!(
            !options.is_empty(),
            "case {engine_type:?}: the instance-option catalog must offer that engine"
        );
        for entry in options {
            let (host_instance_type, _, _) = combination(entry);
            let case = &format!("{engine_type}/{host_instance_type}");
            let reported: Vec<String> = reported_versions(entry);
            assert!(
                !reported.is_empty(),
                "case {case:?}: every option offers a version to create with, got {entry:?}"
            );
            let selected: Vec<String> = published
                .iter()
                .filter(|version| reported.contains(version))
                .cloned()
                .collect();
            assert_eq!(
                reported, selected,
                "case {case:?}: an option reports a selection of the engine catalog's \
                 {published:?}, in that order, got {reported:?}"
            );
            // And the selection is the one the recorded catalog reports for that size.
            assert_eq!(
                reported,
                recorded_versions(&engine_type, &host_instance_type),
                "case {case:?}: unexpected version selection for that size"
            );
        }
    }
}

#[tokio::test]
async fn the_recorded_version_lines_are_selections_of_the_engine_catalog() {
    // The table's version column is written out from the recorded answers, so it must stay
    // a selection of the version list the crate publishes for the engine (the one
    // `DescribeBrokerEngineTypes` serves): a line in the table that the engine no longer
    // offers, or an order that is not the published one, is a stale row.
    for (engine_type, lines) in [
        (ENGINE_TYPE_ACTIVEMQ, ACTIVEMQ_VERSIONS),
        (ENGINE_TYPE_RABBITMQ, RABBITMQ_GRAVITON_VERSIONS),
        (ENGINE_TYPE_RABBITMQ, RABBITMQ_LEGACY_VERSIONS),
    ] {
        let mut published: Vec<String> = published_engine_versions(engine_type)
            .iter()
            .map(|version| (*version).to_string())
            .collect();
        published.reverse();
        let case = &format!("{engine_type}: {lines:?}");
        let selected: Vec<String> = published
            .iter()
            .filter(|version| lines.contains(&version.as_str()))
            .cloned()
            .collect();
        assert_eq!(
            lines.iter().map(|v| (*v).to_string()).collect::<Vec<String>>(),
            selected,
            "case {case:?}: expected a selection of the published versions {published:?}, \
             newest first"
        );
    }
}

#[tokio::test]
async fn only_the_graviton_sizes_report_rabbitmqs_current_release_line() {
    let region = "eu-west-1";
    let mq = Mq::new(region);
    let catalog = whole_catalog(region, &mq).await;

    // Two cases, one per instance family RabbitMQ is offered on: the Graviton sizes, which
    // report the engine's current major release line, and the older general-purpose sizes,
    // which report its earlier line only.
    let mut graviton: Vec<(String, Vec<String>)> = Vec::new();
    let mut legacy: Vec<(String, Vec<String>)> = Vec::new();
    for entry in &catalog {
        let (host_instance_type, engine_type, _) = combination(entry);
        if engine_type != ENGINE_TYPE_RABBITMQ {
            continue;
        }
        let versions = reported_versions(entry);
        if host_instance_type.starts_with(GRAVITON_FAMILY_PREFIX) {
            graviton.push((host_instance_type, versions));
        } else {
            legacy.push((host_instance_type, versions));
        }
    }
    assert!(
        !graviton.is_empty() && !legacy.is_empty(),
        "case \"RabbitMQ families\": expected the catalog to offer the engine on the \
         Graviton sizes and on the older ones, got graviton {graviton:?}, other {legacy:?}"
    );

    let expected_graviton: Vec<String> = RABBITMQ_GRAVITON_VERSIONS
        .iter()
        .map(|version| (*version).to_string())
        .collect();
    let expected_legacy: Vec<String> = RABBITMQ_LEGACY_VERSIONS
        .iter()
        .map(|version| (*version).to_string())
        .collect();
    assert_ne!(
        expected_graviton, expected_legacy,
        "case \"RabbitMQ families\": the two recorded lines must differ, else this case \
         proves nothing"
    );
    for (host_instance_type, versions) in &graviton {
        let case = &format!("{ENGINE_TYPE_RABBITMQ}/{host_instance_type}");
        assert_eq!(
            *versions, expected_graviton,
            "case {case:?}: a Graviton size reports the engine's whole recorded list"
        );
    }
    for (host_instance_type, versions) in &legacy {
        let case = &format!("{ENGINE_TYPE_RABBITMQ}/{host_instance_type}");
        assert_eq!(
            *versions, expected_legacy,
            "case {case:?}: a size outside the Graviton family reports the earlier \
             recorded line only"
        );
        for version in versions {
            assert!(
                expected_graviton.contains(version),
                "case {case:?}: {version:?} must be one of the engine's lines \
                 {expected_graviton:?}"
            );
        }
    }
}

#[tokio::test]
async fn the_default_page_reaches_the_graviton_options_and_the_rest_follow_the_token() {
    let region = "us-west-2";
    let mq = Mq::new(region);
    let catalog = whole_catalog(region, &mq).await;

    // A client paging with the documented default page size must walk the same
    // population, token by token, as the one-page read above.
    let mut seen: Vec<Value> = Vec::new();
    let mut token: Option<String> = None;
    let mut pages = 0;
    loop {
        let query = match &token {
            None => String::new(),
            Some(token) => format!("nextToken={token}"),
        };
        let case = &format!("default page {pages}");
        let (status, body) = describe(&mq, &query).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected 200, got {status} {body:?}"
        );
        seen.extend(entries(case, &body));
        pages += 1;
        assert!(
            pages <= catalog.len() + 1,
            "case {case:?}: the walk must terminate, got {pages} pages for {} entries",
            catalog.len()
        );
        match body.get("nextToken").and_then(Value::as_str) {
            None => break,
            Some(next) => token = Some(next.to_string()),
        }
    }
    assert_eq!(
        seen, catalog,
        "case \"default paging\": paging must report every published entry exactly once"
    );

    // The Graviton family is where RabbitMQ's larger sizes live, and it is reached by the
    // FIRST default-size page, so a client that reads one page discovers it.
    let case = "first default page";
    let (status, body) = describe(&mq, "").await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected 200, got {status} {body:?}"
    );
    let first_page = entries(case, &body);
    let graviton: Vec<&Value> = first_page
        .iter()
        .filter(|entry| {
            entry
                .get("hostInstanceType")
                .and_then(Value::as_str)
                .is_some_and(|host| host.starts_with("mq.m7g."))
        })
        .collect();
    assert!(
        !graviton.is_empty(),
        "case {case:?}: a client reading one page must discover the Graviton options, \
         got {first_page:?}"
    );
    for entry in graviton {
        let host = entry
            .get("hostInstanceType")
            .and_then(Value::as_str)
            .unwrap_or_default();
        assert_eq!(
            entry.get("engineType").and_then(Value::as_str),
            Some(ENGINE_TYPE_RABBITMQ),
            "case {host:?}: the Graviton sizes are offered for RabbitMQ, got {entry:?}"
        );
        assert_eq!(
            entry.get("storageType").and_then(Value::as_str),
            Some(EBS),
            "case {host:?}: RabbitMQ is offered on block storage only, got {entry:?}"
        );
    }
}
