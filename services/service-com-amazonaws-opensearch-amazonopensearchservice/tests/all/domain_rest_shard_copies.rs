// SPDX-License-Identifier: CC0-1.0

//! The SHARD COPIES a domain's own data plane reports — `GET /_cat/shards[/{index}]` — over
//! the real wire path: a signed request to the host `DescribeDomain` published, through the
//! multi-service router, into the mounted service. What is asserted is what a client sees.
//!
//! The rule, over a SET of topologies rather than one recorded call: the listing reports one
//! row per COPY of every shard of the indexes it is about — the primary of each shard and
//! one row per replica the index asked for — and each copy is `STARTED` on a data node of
//! the cluster the stored `ClusterConfig` provisions for as many copies as that topology can
//! hold, `UNASSIGNED` with no node and no documents for the rest; no two copies of one shard
//! share a node, every node a row names is a node the cluster's own node listing reports, and
//! the documents a copy reports are the ones the engine's routing puts on its shard, so the
//! primaries' counts add up to the documents the index holds. It is the same allocation the
//! cluster health counts, read copy by copy, so the two can never disagree — including after
//! a scale-out, which places the copies the smaller cluster had nowhere to put while every
//! document stays queryable.

use serde_json::{json, Value};
use service_com_amazonaws_opensearch_amazonopensearchservice::state::cluster_health::MAX_SHARDS_PER_INDEX;

use super::domain_lifecycle::{poll_until, settled_domain, Emulator, DEFAULT_ACCESS_KEY_ID};
use super::domain_rest_data_plane::{endpoint_of, error_type, rest, rest_payload, rest_prefixed};

/// How many documents each case loads, so every shard of every topology below has a count of
/// its own to report and the per-shard counts have something to add up to.
const DOCUMENTS: usize = 24;

/// The index every case writes into.
const INDEX: &str = "catalog";

/// One topology to read the listing of: the data nodes the domain is created with, and the
/// shards and replica copies the index asks for.
struct Topology {
    case: &'static str,
    nodes: i64,
    shards: i64,
    replicas: i64,
}

/// The topologies the rule is read over: they differ in the node count, in the shard count
/// and in how many of the replica copies the cluster has nodes for — none, some and all.
const TOPOLOGIES: [Topology; 5] = [
    Topology {
        case: "one node and an index asking for no copy",
        nodes: 1,
        shards: 1,
        replicas: 0,
    },
    Topology {
        case: "one node and a copy it has nowhere to put",
        nodes: 1,
        shards: 2,
        replicas: 1,
    },
    Topology {
        case: "two nodes holding one copy of each of three shards",
        nodes: 2,
        shards: 3,
        replicas: 1,
    },
    Topology {
        case: "three nodes holding both copies of each shard",
        nodes: 3,
        shards: 2,
        replicas: 2,
    },
    Topology {
        case: "three nodes and more copies than there are nodes for",
        nodes: 3,
        shards: 1,
        replicas: 4,
    },
];

/// A domain of `nodes` data nodes, as the host it published its data plane at.
async fn cluster(emulator: &Emulator, name: &str, nodes: i64) -> String {
    let created = emulator
        .create(json!({
            "DomainName": name,
            "EngineVersion": "OpenSearch_2.13",
            "ClusterConfig": { "InstanceType": "m5.large.search", "InstanceCount": nodes },
            "EBSOptions": { "EBSEnabled": true, "VolumeType": "gp3", "VolumeSize": 10 },
        }))
        .await;
    assert_eq!(
        created.status, 200,
        "case CreateDomain({name:?}, {nodes} data nodes): expected 200, got {} with {:?}",
        created.status, created.body
    );
    endpoint_of(emulator, name)
}

/// An index of `shards` primary shards asking for `replicas` copies of each.
async fn given_index(emulator: &Emulator, host: &str, shards: i64, replicas: i64) {
    let created = rest(
        emulator,
        DEFAULT_ACCESS_KEY_ID,
        host,
        "PUT",
        &format!("/{INDEX}"),
        Some(json!({
            "settings": {
                "index": { "number_of_shards": shards, "number_of_replicas": replicas },
            },
        })),
    )
    .await;
    assert_eq!(
        (created.status, created.body["acknowledged"].as_bool()),
        (200, Some(true)),
        "case the index of {shards} shards and {replicas} copies: expected the create to be \
         acknowledged, got {} with {:?}",
        created.status,
        created.body
    );
}

/// Load [`DOCUMENTS`] documents into the index through the bulk API, answering the ids they
/// were written under.
async fn given_documents(emulator: &Emulator, host: &str) -> Vec<String> {
    let ids: Vec<String> = (0..DOCUMENTS).map(|at| format!("doc-{at}")).collect();
    let mut payload = String::new();
    for id in &ids {
        payload.push_str(&json!({ "index": { "_id": id } }).to_string());
        payload.push('\n');
        payload.push_str(&json!({ "title": format!("title of {id}") }).to_string());
        payload.push('\n');
    }
    let loaded = rest_payload(
        emulator,
        DEFAULT_ACCESS_KEY_ID,
        host,
        "POST",
        &format!("/{INDEX}/_bulk"),
        payload,
    )
    .await;
    assert_eq!(
        (loaded.status, loaded.body["errors"].as_bool()),
        (200, Some(false)),
        "case the bulk load of {DOCUMENTS} documents: expected every item to succeed, got {} \
         with {:?}",
        loaded.status,
        loaded.body
    );
    ids
}

/// The rows a shard listing answers, as the JSON rendering carries them.
async fn shard_rows(emulator: &Emulator, host: &str, path: &str) -> Vec<Value> {
    let answer = rest(
        emulator,
        DEFAULT_ACCESS_KEY_ID,
        host,
        "GET",
        &format!("{path}?format=json"),
        None,
    )
    .await;
    assert_eq!(
        answer.status, 200,
        "case \"GET {path}\": expected 200, got {} with {:?}",
        answer.status, answer.body
    );
    answer
        .body
        .as_array()
        .unwrap_or_else(|| panic!("case \"GET {path}\": expected a list of rows, got {:?}", answer.body))
        .clone()
}

/// One column of one row, as the listing reports every value: a string.
fn column(row: &Value, name: &str) -> String {
    row[name].as_str().unwrap_or_default().to_string()
}

/// The names the cluster's own node listing reports for its DATA nodes — the nodes a shard
/// copy can sit on, read through the listing beside the one under test so the two are
/// compared rather than one of them restated.
async fn data_node_names(emulator: &Emulator, host: &str) -> Vec<String> {
    let rows = shard_rows(emulator, host, "/_cat/nodes").await;
    rows.iter()
        .filter(|row| column(row, "node.role").contains('d'))
        .map(|row| column(row, "name"))
        .collect()
}

/// The cluster health of the one index, as `(active shard copies, unassigned ones)`.
async fn health_counts(emulator: &Emulator, host: &str) -> (u64, u64) {
    let health = rest(
        emulator,
        DEFAULT_ACCESS_KEY_ID,
        host,
        "GET",
        &format!("/_cluster/health/{INDEX}"),
        None,
    )
    .await;
    assert_eq!(
        health.status, 200,
        "case \"GET /_cluster/health/{INDEX}\": expected 200, got {} with {:?}",
        health.status, health.body
    );
    (
        health.body["active_shards"].as_u64().unwrap_or_default(),
        health.body["unassigned_shards"].as_u64().unwrap_or_default(),
    )
}

/// The ids a match-all search answers, sorted, so a continuity check compares document SETS.
async fn searched_ids(emulator: &Emulator, host: &str) -> Vec<String> {
    let answer = rest(
        emulator,
        DEFAULT_ACCESS_KEY_ID,
        host,
        "POST",
        &format!("/{INDEX}/_search"),
        Some(json!({ "query": { "match_all": {} }, "size": 100 })),
    )
    .await;
    assert_eq!(
        answer.status, 200,
        "case \"POST /{INDEX}/_search\": expected 200, got {} with {:?}",
        answer.status, answer.body
    );
    let mut ids: Vec<String> = answer.body["hits"]["hits"]
        .as_array()
        .unwrap_or(&Vec::new())
        .iter()
        .map(|hit| column(hit, "_id"))
        .collect();
    ids.sort();
    ids
}

/// Every assertion of the rule about one listing, for a cluster of `nodes` data nodes whose
/// index asked for `shards` shards and `replicas` copies of each.
///
/// `nodes_named` is what the cluster's own node listing reports, so the nodes the rows name
/// are checked against the cluster rather than against a name this test builds.
fn assert_copies(
    case: &str,
    rows: &[Value],
    nodes_named: &[String],
    shards: i64,
    replicas: i64,
    documents: usize,
) -> (usize, usize) {
    let copies_per_shard = usize::try_from(replicas + 1).unwrap_or(1);
    let shard_count = usize::try_from(shards).unwrap_or(1);
    assert_eq!(
        rows.len(),
        shard_count * copies_per_shard,
        "case {case:?}: expected one row per copy of every shard, got {rows:?}"
    );

    let mut started = 0;
    let mut unassigned = 0;
    let mut primary_documents = 0;
    for shard in 0..shards {
        let of_shard: Vec<&Value> = rows
            .iter()
            .filter(|row| column(row, "shard") == shard.to_string())
            .collect();
        assert_eq!(
            of_shard.len(),
            copies_per_shard,
            "case {case:?}: expected {copies_per_shard} copies of shard {shard}, got {of_shard:?}"
        );
        let primaries: Vec<&&Value> = of_shard
            .iter()
            .filter(|row| column(row, "prirep") == "p")
            .collect();
        assert_eq!(
            primaries.len(),
            1,
            "case {case:?}: expected exactly one primary of shard {shard}, got {of_shard:?}"
        );
        let primary = primaries[0];
        assert_eq!(
            column(primary, "state"),
            "STARTED",
            "case {case:?}: expected the primary of shard {shard} to be started, got {primary:?}"
        );
        let held: usize = column(primary, "docs").parse().unwrap_or_else(|_| {
            panic!("case {case:?}: expected the primary of shard {shard} to report a document count, got {primary:?}")
        });
        primary_documents += held;

        let mut nodes_of_shard: Vec<String> = Vec::new();
        for row in &of_shard {
            let state = column(row, "state");
            let node = column(row, "node");
            let docs = column(row, "docs");
            if state == "STARTED" {
                started += 1;
                assert!(
                    nodes_named.contains(&node),
                    "case {case:?}: expected the copy on a node the cluster reports, got \
                     {node:?} of {nodes_named:?} in {row:?}"
                );
                assert_eq!(
                    docs,
                    held.to_string(),
                    "case {case:?}: expected every copy of shard {shard} to hold the shard's \
                     {held} documents, got {row:?}"
                );
                nodes_of_shard.push(node);
            } else {
                unassigned += 1;
                assert_eq!(
                    (state.as_str(), node.as_str(), docs.as_str()),
                    ("UNASSIGNED", "", ""),
                    "case {case:?}: expected a copy with nowhere to go to name no node and no \
                     documents, got {row:?}"
                );
            }
        }
        let placed = nodes_of_shard.len();
        nodes_of_shard.sort();
        nodes_of_shard.dedup();
        assert_eq!(
            nodes_of_shard.len(),
            placed,
            "case {case:?}: expected no two copies of shard {shard} on one node, got {of_shard:?}"
        );
    }
    assert_eq!(
        primary_documents, documents,
        "case {case:?}: expected the primaries to hold every document the index holds, got \
         {primary_documents} over {rows:?}"
    );
    (started, unassigned)
}

/// RULE: the listing reports one row per copy of every shard, started on a data node of its
/// own for every copy the topology can hold and unassigned for the rest — the same allocation
/// the cluster health counts.
#[tokio::test]
async fn a_shard_listing_reports_every_copy_the_cluster_can_place() {
    let emulator = Emulator::new();
    for (at, topology) in TOPOLOGIES.iter().enumerate() {
        let case = topology.case;
        let name = format!("shard-copies-{at}");
        let host = cluster(&emulator, &name, topology.nodes).await;
        given_index(&emulator, &host, topology.shards, topology.replicas).await;
        given_documents(&emulator, &host).await;

        let nodes_named = data_node_names(&emulator, &host).await;
        assert_eq!(
            nodes_named.len(),
            usize::try_from(topology.nodes).unwrap_or(1),
            "case {case:?}: expected the cluster to report {} data nodes, got {nodes_named:?}",
            topology.nodes
        );

        let rows = shard_rows(&emulator, &host, &format!("/_cat/shards/{INDEX}")).await;
        let (started, unassigned) = assert_copies(
            case,
            &rows,
            &nodes_named,
            topology.shards,
            topology.replicas,
            DOCUMENTS,
        );

        // A copy needs a node of its own, so the copies a cluster of this many nodes can
        // place are the primary of each shard plus one replica per further node.
        let placeable = topology.replicas.min(topology.nodes - 1).max(0);
        let expected_started = usize::try_from(topology.shards * (placeable + 1)).unwrap_or(0);
        let expected_unassigned =
            usize::try_from(topology.shards * (topology.replicas - placeable)).unwrap_or(0);
        assert_eq!(
            (started, unassigned),
            (expected_started, expected_unassigned),
            "case {case:?}: expected {expected_started} started and {expected_unassigned} \
             unassigned copies, got {rows:?}"
        );

        // ... and that is the very allocation the cluster health counts, so the two readings
        // of one cluster agree copy for copy.
        let (active, health_unassigned) = health_counts(&emulator, &host).await;
        assert_eq!(
            (active, health_unassigned),
            (
            u64::try_from(started).unwrap_or_default(),
            u64::try_from(unassigned).unwrap_or_default(),
        ),
            "case {case:?}: expected the health to count the copies the listing reports, got \
             {active} active and {health_unassigned} unassigned"
        );
    }
}

/// RULE (the same rule across a change of topology): scaling the cluster out places the
/// copies it had no node for, on nodes of their own, while every document the index held
/// stays queryable.
#[tokio::test]
async fn a_scale_out_places_the_copies_the_smaller_cluster_could_not_hold() {
    let emulator = Emulator::new();
    let name = "shard-copies-scale-out";
    let host = cluster(&emulator, name, 2).await;
    given_index(&emulator, &host, 3, 2).await;
    let loaded = {
        let mut ids = given_documents(&emulator, &host).await;
        ids.sort();
        ids
    };

    let before = shard_rows(&emulator, &host, &format!("/_cat/shards/{INDEX}")).await;
    let nodes_before = data_node_names(&emulator, &host).await;
    assert_eq!(
        assert_copies("two nodes", &before, &nodes_before, 3, 2, DOCUMENTS),
        (6, 3),
        "case \"two nodes\": expected one copy of each shard to have nowhere to go, got \
         {before:?}"
    );

    // The cluster takes one change at a time, so the scale-out is asked for once the create
    // has landed — the CONDITION a client polls for, never a stretch of time.
    settled_domain(&emulator, name).await;
    let scaled = emulator
        .update_config(
            name,
            json!({ "ClusterConfig": { "InstanceType": "m5.large.search", "InstanceCount": 3 } }),
        )
        .await;
    assert_eq!(
        scaled.status, 200,
        "case the scale-out to three nodes: expected 200 from UpdateDomainConfig, got {} with \
         {:?}",
        scaled.status, scaled.body
    );

    // The third node joins the cluster, which is what the copies that had nowhere to go are
    // waiting for.
    poll_until("the third data node joins the cluster", || async {
        data_node_names(&emulator, &host).await.len() == 3
    })
    .await;

    let after = shard_rows(&emulator, &host, &format!("/_cat/shards/{INDEX}")).await;
    let nodes_after = data_node_names(&emulator, &host).await;
    assert_eq!(
        assert_copies("three nodes", &after, &nodes_after, 3, 2, DOCUMENTS),
        (9, 0),
        "case \"three nodes\": expected every copy to be placed, got {after:?}"
    );
    assert_eq!(
        searched_ids(&emulator, &host).await,
        loaded,
        "case \"three nodes\": expected every document to stay queryable across the change"
    );
}

/// RULE (the addressings and the refusals of the same listing): the listing is answered for
/// every index of the cluster and for the indexes one name resolves to, in both renderings
/// and at both addressings of the domain, and a name the cluster holds no index for is
/// refused exactly as an absent index is anywhere else.
#[tokio::test]
async fn the_shard_listing_answers_every_addressing_and_refuses_an_absent_index() {
    let emulator = Emulator::new();
    let name = "shard-copies-addressing";
    let host = cluster(&emulator, name, 2).await;
    given_index(&emulator, &host, 2, 1).await;
    given_documents(&emulator, &host).await;
    let other = rest(
        &emulator,
        DEFAULT_ACCESS_KEY_ID,
        &host,
        "PUT",
        "/journal",
        Some(json!({ "settings": { "index": { "number_of_shards": 1, "number_of_replicas": 0 } } })),
    )
    .await;
    assert_eq!(
        other.status, 200,
        "case the second index: expected 200 from the create, got {} with {:?}",
        other.status, other.body
    );

    // The whole cluster's copies: both indexes' rows, and the per-index form selects one.
    let every = shard_rows(&emulator, &host, "/_cat/shards").await;
    assert_eq!(
        every.len(),
        5,
        "case \"GET /_cat/shards\": expected the copies of both indexes, got {every:?}"
    );
    let of_index = shard_rows(&emulator, &host, &format!("/_cat/shards/{INDEX}")).await;
    assert_eq!(
        of_index.len(),
        4,
        "case \"GET /_cat/shards/{INDEX}\": expected the copies of that index alone, got \
         {of_index:?}"
    );
    assert!(
        of_index
            .iter()
            .all(|row| column(row, "index") == INDEX),
        "case \"GET /_cat/shards/{INDEX}\": expected every row to be of that index, got \
         {of_index:?}"
    );

    // The same listing read at the PREFIXED addressing of the same domain reports the same
    // rows: two addressings of one cluster cannot diverge.
    let prefixed = rest_prefixed(
        &emulator,
        name,
        "GET",
        &format!("/_cat/shards/{INDEX}?format=json"),
        None,
    )
    .await;
    assert_eq!(
        (prefixed.status, &prefixed.body),
        (200, &Value::Array(of_index.clone())),
        "case the prefixed addressing: expected the rows the published host answers, got {} \
         with {:?}",
        prefixed.status,
        prefixed.body
    );

    // The aligned text rendering is the same listing in the other form: one line per row.
    let (status, text) = text_listing(&emulator, &host, &format!("/_cat/shards/{INDEX}")).await;
    assert_eq!(
        (status, text.lines().count()),
        (200, of_index.len()),
        "case the text rendering: expected one line per copy, got {status} with {text:?}"
    );

    // A name the cluster holds no index for selects nothing, which is the absent-index
    // refusal rather than an empty listing.
    let absent = rest(
        &emulator,
        DEFAULT_ACCESS_KEY_ID,
        &host,
        "GET",
        "/_cat/shards/absent?format=json",
        None,
    )
    .await;
    assert_eq!(
        (absent.status, error_type(&absent).as_str()),
        (404, "index_not_found_exception"),
        "case an absent index: expected the absent-index refusal, got {} with {:?}",
        absent.status,
        absent.body
    );

    // ... and the path is a listing, so a method that would write is refused by the path
    // rather than served.
    let written = rest(
        &emulator,
        DEFAULT_ACCESS_KEY_ID,
        &host,
        "POST",
        "/_cat/shards",
        None,
    )
    .await;
    assert_eq!(
        written.status, 405,
        "case \"POST /_cat/shards\": expected the method refusal, got {} with {:?}",
        written.status, written.body
    );
}

/// RULE (the bound on the same listing): the copies one index's listing materialises are
/// bounded by the engine's own ceiling on the shards and copies one index may ask for, so an
/// index whose settings ask for more does not let a caller size the answer.
#[tokio::test]
async fn a_shard_listing_is_bounded_by_the_ceiling_on_one_index() {
    let emulator = Emulator::new();
    let host = cluster(&emulator, "shard-copies-bounded", 2).await;
    // Past the ceiling in BOTH settings at once: multiplied out, these ask for over four
    // million copies of one index.
    let past = MAX_SHARDS_PER_INDEX + 1;
    given_index(&emulator, &host, past, past).await;

    let rows = shard_rows(&emulator, &host, &format!("/_cat/shards/{INDEX}")).await;
    assert_eq!(
        i64::try_from(rows.len()).unwrap_or(i64::MAX),
        MAX_SHARDS_PER_INDEX,
        "case the settings past the ceiling: expected the listing to stop at the ceiling, got \
         {} rows",
        rows.len()
    );
    // ... and the rows it does report are the listing's own rows, not a truncated shape: each
    // is a copy of a shard of that index.
    assert!(
        rows.iter().all(|row| column(row, "index") == INDEX
            && !column(row, "shard").is_empty()
            && ["p", "r"].contains(&column(row, "prirep").as_str())),
        "case the settings past the ceiling: expected every row to be a copy of that index, \
         got {:?}",
        rows.first()
    );
}

/// One listing read in the ALIGNED TEXT rendering — the answer a `_cat` request that asks
/// for no JSON is served, which is not JSON and so is read as the bytes the client got.
async fn text_listing(emulator: &Emulator, host: &str, path: &str) -> (u16, String) {
    let request = ::http::Request::builder()
        .method("GET")
        .uri(path)
        .header("host", host)
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 \
                 Credential={DEFAULT_ACCESS_KEY_ID}/20260101/us-east-1/es/aws4_request, \
                 SignedHeaders=host, Signature=0"
            ),
        )
        .body(::bytes::Bytes::new())
        .expect("the test request is well formed");
    emulator.dispatch_bytes(request).await
}
