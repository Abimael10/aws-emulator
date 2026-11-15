// SPDX-License-Identifier: CC0-1.0

//! The whole ALERTING flow a client wires to Amazon SNS, seeded the way the bulk API seeds
//! it: a signed `POST /_bulk` to the host `DomainStatus$Endpoint` published, then a monitor
//! whose query counts what that stream left in the index, then the message its trigger
//! renders arriving on the queue subscribed to the channel's topic.
//!
//! The rule: a first path segment beginning with `_` names an API of the engine and never an
//! index, so the bulk API is served on the domain's published endpoint — `POST /_bulk` and
//! `POST /{index}/_bulk` answer HTTP 200 with `took`, `errors` and one item per action — and
//! the documents the stream leaves behind are exactly what a monitor's query then counts: its
//! trigger publishes the rendered count to the channel's topic while its condition holds over
//! that count, and publishes nothing at all while it does not.
//!
//! The rows differ along the bulk addressing (the cluster-wide path, whose actions name their
//! own index, and the index-scoped path, whose actions name none), the action vocabulary
//! (`index`, `create`, `delete`), the NET number of documents one stream leaves (none, one,
//! several), and whether the trigger's condition holds over that number — so the message is
//! asserted against what the stream really loaded rather than against one recorded count.
//!
//! Everything is driven over the real wire path, with Amazon SNS and Amazon SQS mounted
//! beside this service on one shared engine; the harness is this crate's own
//! ([`super::alerting_monitors`], [`super::domain_rest_bulk_and_index_apis`]).

use serde_json::{json, Value};

use super::alerting_monitors::{
    given_channel, given_monitor, given_target, monitor_document, published_message, received,
};
use super::data_source_registry::created_domain;
use super::domain_lifecycle::Emulator;
use super::domain_rest_bulk_and_index_apis::{bulk, counted, created_index, items};
use super::domain_rest_data_plane::endpoint_of;

/// The domain every row of the table below is served by.
const DOMAIN: &str = "bulk-seeded-alerting";

/// The message template every monitor's action carries: the total its own query reported,
/// which is what the stream loaded.
const MESSAGE: &str = "{{ctx.results.0.hits.total.value}} documents loaded";

/// One row: a bulk stream, the documents it leaves behind, and the trigger condition taken
/// over them.
struct Row {
    case: &'static str,
    /// The index the stream loads and the monitor queries.
    index: &'static str,
    /// Whether the stream is sent to the INDEX-SCOPED bulk path, whose actions name no index
    /// of their own, rather than to the cluster-wide one.
    scoped: bool,
    /// The newline-delimited stream, as its lines.
    lines: Vec<Value>,
    /// The `(action, status)` each item of the answer must report, in the stream's order.
    items: Vec<(&'static str, i64)>,
    /// How many documents the index holds once the stream has been applied.
    held: i64,
    /// The trigger condition, over the total that count produces.
    condition: &'static str,
    /// Whether that condition holds.
    fires: bool,
}

/// The table.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "three index actions on the cluster-wide bulk path, each naming its index",
            index: "seeded-three",
            scoped: false,
            lines: vec![
                json!({ "index": { "_index": "seeded-three", "_id": "one" } }),
                json!({ "status": "error", "service": "checkout" }),
                json!({ "index": { "_index": "seeded-three", "_id": "two" } }),
                json!({ "status": "error", "service": "search" }),
                json!({ "index": { "_index": "seeded-three", "_id": "three" } }),
                json!({ "status": "ok", "service": "checkout" }),
            ],
            items: vec![("index", 201), ("index", 201), ("index", 201)],
            held: 3,
            condition: "ctx.results[0].hits.total.value > 0",
            fires: true,
        },
        Row {
            case: "one create action on the index-scoped bulk path, naming no index itself",
            index: "seeded-one",
            scoped: true,
            lines: vec![
                json!({ "create": { "_id": "only" } }),
                json!({ "status": "error", "service": "cart" }),
            ],
            items: vec![("create", 201)],
            held: 1,
            condition: "ctx.results[0].hits.total.value == 1",
            fires: true,
        },
        Row {
            case: "a stream that indexes two documents and deletes one of them again",
            index: "seeded-net",
            scoped: false,
            lines: vec![
                json!({ "index": { "_index": "seeded-net", "_id": "kept" } }),
                json!({ "status": "error", "service": "payments" }),
                json!({ "index": { "_index": "seeded-net", "_id": "dropped" } }),
                json!({ "status": "error", "service": "payments" }),
                json!({ "delete": { "_index": "seeded-net", "_id": "dropped" } }),
            ],
            items: vec![("index", 201), ("index", 201), ("delete", 200)],
            held: 1,
            condition: "ctx.results[0].hits.total.value > 0",
            fires: true,
        },
        Row {
            case: "a stream that leaves the index holding nothing at all",
            index: "seeded-none",
            scoped: true,
            lines: vec![
                json!({ "index": { "_id": "transient" } }),
                json!({ "status": "error", "service": "shipping" }),
                json!({ "delete": { "_id": "transient" } }),
            ],
            items: vec![("index", 201), ("delete", 200)],
            held: 0,
            condition: "ctx.results[0].hits.total.value > 0",
            fires: false,
        },
        Row {
            case: "a condition asking for more documents than the stream loaded",
            index: "seeded-below",
            scoped: false,
            lines: vec![
                json!({ "index": { "_index": "seeded-below", "_id": "one" } }),
                json!({ "status": "error", "service": "auth" }),
                json!({ "index": { "_index": "seeded-below", "_id": "two" } }),
                json!({ "status": "ok", "service": "auth" }),
            ],
            items: vec![("index", 201), ("index", 201)],
            held: 2,
            condition: "ctx.results[0].hits.total.value > 2",
            fires: false,
        },
    ]
}

/// RULE: the bulk API is served on the domain's published endpoint — a reserved `_`-prefixed
/// first segment names an engine API and never an index — and the documents one stream leaves
/// behind are what a monitor's query then counts: its trigger publishes the rendered count to
/// the channel's topic exactly while its condition holds over that count.
#[tokio::test]
async fn a_bulk_seeded_index_decides_what_the_monitor_publishes_to_its_topic() {
    let emulator = Emulator::with_notification_channels();
    created_domain(&emulator, DOMAIN).await;
    let host = endpoint_of(&emulator, DOMAIN);

    for (at, row) in rows().into_iter().enumerate() {
        let case = row.case;
        created_index(
            &emulator,
            &host,
            row.index,
            json!({ "settings": { "number_of_shards": 1, "number_of_replicas": 0 } }),
        )
        .await;

        // The bulk load, on the addressing the domain published. A reserved first segment
        // read as an index name is what makes this the index route's refusal instead of a
        // load, so the status is asserted before anything that depends on the documents.
        let path = if row.scoped {
            format!("/{}/_bulk", row.index)
        } else {
            "/_bulk".to_string()
        };
        let loaded = bulk(&emulator, &host, &path, &row.lines).await;
        assert_eq!(
            loaded.status, 200,
            "case {case:?}: expected the bulk API to serve \"POST {path}\" with 200, got {} \
             with {:?}",
            loaded.status, loaded.body
        );
        assert_eq!(
            loaded.body["errors"].as_bool(),
            Some(false),
            "case {case:?}: expected no item of the stream to fail, got {:?}",
            loaded.body
        );
        assert!(
            loaded.body["took"].is_u64(),
            "case {case:?}: expected the answer to report how long the load took, got {:?}",
            loaded.body
        );
        let reported: Vec<(String, i64)> = items(&loaded)
            .into_iter()
            .map(|(action, status, _, _)| (action, status))
            .collect();
        let expected: Vec<(String, i64)> = row
            .items
            .iter()
            .map(|(action, status)| ((*action).to_string(), *status))
            .collect();
        assert_eq!(
            reported, expected,
            "case {case:?}: expected one item per action in the stream's order, got {:?}",
            loaded.body
        );

        // What the stream LEFT, as the index's own count reports it — which is the total the
        // monitor's query is about to produce.
        let held = counted(&emulator, &host, row.index).await;
        assert_eq!(
            held, row.held,
            "case {case:?}: expected the index to hold {} document(s) after the load, got \
             {held}",
            row.held
        );

        // The monitor over that index, and the Amazon SNS channel its action addresses.
        let name = format!("bulk-seeded-{at}");
        let target = given_target(&emulator, &name).await;
        let channel_id = given_channel(&emulator, DOMAIN, &name, &target.topic_arn).await;
        let monitor_id = given_monitor(
            &emulator,
            DOMAIN,
            case,
            monitor_document(
                &name,
                row.index,
                json!({ "size": 0, "query": { "match_all": {} } }),
                row.condition,
                &channel_id,
                MESSAGE,
            ),
        )
        .await;

        let run = super::domain_rest_data_plane::rest_prefixed(
            &emulator,
            DOMAIN,
            "POST",
            &format!("/_plugins/_alerting/monitors/{monitor_id}/_execute"),
            None,
        )
        .await;
        assert_eq!(
            run.status, 200,
            "case {case:?}: expected 200 from the monitor run, got {} with {:?}",
            run.status, run.body
        );
        let result = run.body["trigger_results"]
            .as_object()
            .and_then(|results| results.values().next().cloned())
            .unwrap_or_else(|| {
                panic!("case {case:?}: expected one trigger result, got {:?}", run.body)
            });
        assert_eq!(
            result["triggered"].as_bool(),
            Some(row.fires),
            "case {case:?}: expected triggered {} over {held} document(s), got {result:?}",
            row.fires
        );

        if row.fires {
            let rendered = format!("{held} documents loaded");
            let delivered = published_message(&emulator, case, &target.queue_url).await;
            assert_eq!(
                delivered, rendered,
                "case {case:?}: expected the queue subscribed to the channel's topic to carry \
                 the count the stream loaded, got {delivered:?}"
            );
        } else {
            // The publish happens before the run answers, so a queue empty now was never
            // written to.
            let delivered = received(&emulator, &target.queue_url).await;
            assert!(
                delivered.is_empty(),
                "case {case:?}: expected the trigger to withhold its notification over \
                 {held} document(s), got {delivered:?} on the queue"
            );
        }
    }
}
