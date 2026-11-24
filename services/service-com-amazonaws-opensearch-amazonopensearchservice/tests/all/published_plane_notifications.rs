// SPDX-License-Identifier: CC0-1.0

//! What a monitor run DOES when it arrives on the listener a domain publishes, rather than at
//! the emulator's front door.
//!
//! The rule, one level above the recorded case: **a domain's data plane does the same thing at
//! every address the domain is reached at — so a monitor run accepted on the listener
//! `DomainStatus$Endpoint` publishes performs its firing trigger's actions, handing each
//! rendered notification to the channel its action names, exactly as the same run accepted at
//! the front door does; a trigger whose condition does not hold publishes nothing at either
//! address.**
//!
//! A client is CONFIGURED with the published endpoint — it is "the domain-specific endpoint
//! used to submit index, search, and data upload requests to the domain" — so that address is
//! where a client's alerting traffic really arrives: a run served there that evaluated its
//! trigger, rendered its template and then handed its notification to nobody leaves a monitor
//! that looks configured and a subscribed queue that stays empty, with no error anywhere.
//!
//! The rows span what the rule generalises over: the ADDRESS the execute arrives at (the
//! listener the domain published, and the front door's prefixed path), the documents the index
//! holds at that run (several, none, one — the trajectory a bulk load, a `_delete_by_query` and
//! a re-index leave behind), the query the monitor's input asks (a term on a keyword field, and
//! `match_all`), and whether the trigger's condition holds over the total it produced.
//!
//! Amazon SNS and Amazon SQS are mounted beside this service on ONE shared engine, so the
//! notification is asserted where a client sees it: on the queue subscribed to the channel's
//! topic. The harness is this crate's own ([`super::alerting_monitors`],
//! [`super::domain_lifecycle`], [`super::domain_rest_bulk_and_index_apis`]).

use serde_json::{json, Value};
use service_com_amazonaws_opensearch_amazonopensearchservice::service::domain_rest::serve_published;

use super::alerting_monitors::{
    given_channel, given_monitor, given_target, monitor_document, published_message, received,
};
use super::domain_lifecycle::{
    poll_until, settled_domain, Emulator, DEFAULT_ACCESS_KEY_ID, REGION,
};
use super::domain_rest_bulk_and_index_apis::{counted, created_index};
use super::domain_rest_data_plane::{endpoint_of, rest, rest_payload, rest_prefixed};
use super::published_domain_address::{dialable_authority, published_endpoint};

/// The domain every row below is served by.
const DOMAIN: &str = "published-plane-alerting";

/// The index the monitors query, and the mapping it is created with: the field the term query
/// selects on is a `keyword`, and the counter beside it an `integer`.
const INDEX: &str = "published-plane-source";

/// The marker every message template carries beside the count it renders, so a delivered body
/// is recognisable as this suite's own.
const MARKER: &str = "alerting-probe";

/// Which ADDRESS a run arrives at.
#[derive(Clone, Copy, Debug)]
enum Arrives {
    /// On the listener the domain published, which is what a client configured with
    /// `DomainStatus$Endpoint` dials: the core hands that request to this service with no
    /// operation context at all.
    PublishedListener,
    /// At the emulator's front door, under the prefixed path addressing.
    FrontDoor,
}

/// One row: where the execute arrives, how many documents the index holds at that run, the
/// query the monitor asks, the condition over the total it produced, and whether that holds.
struct Row {
    case: &'static str,
    arrives: Arrives,
    /// How many documents carrying `group: "a"` the index holds for this run.
    held: i64,
    /// The query the monitor's one input asks.
    query: Value,
    /// The trigger condition, as the caller wrote it.
    condition: &'static str,
    /// Whether that condition holds over the total the query produced.
    fires: bool,
}

/// The table.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a term query over several documents, executed on the published listener",
            arrives: Arrives::PublishedListener,
            held: 6,
            query: json!({ "size": 0, "query": { "term": { "group": "a" } } }),
            condition: "ctx.results[0].hits.total.value > 0",
            fires: true,
        },
        Row {
            case: "the same monitor on the published listener once the index is empty",
            arrives: Arrives::PublishedListener,
            held: 0,
            query: json!({ "size": 0, "query": { "term": { "group": "a" } } }),
            condition: "ctx.results[0].hits.total.value > 0",
            fires: false,
        },
        Row {
            case: "one re-indexed document, executed on the published listener",
            arrives: Arrives::PublishedListener,
            held: 1,
            query: json!({ "size": 0, "query": { "term": { "group": "a" } } }),
            condition: "ctx.results[0].hits.total.value > 0",
            fires: true,
        },
        Row {
            case: "a match_all query whose condition the published listener's run does not meet",
            arrives: Arrives::PublishedListener,
            held: 2,
            query: json!({ "size": 0, "query": { "match_all": {} } }),
            condition: "ctx.results[0].hits.total.value > 5",
            fires: false,
        },
        Row {
            case: "a match_all query on the published listener, over the documents it counts",
            arrives: Arrives::PublishedListener,
            held: 3,
            query: json!({ "size": 0, "query": { "match_all": {} } }),
            condition: "ctx.results[0].hits.total.value == 3",
            fires: true,
        },
        Row {
            case: "the same term query and condition at the front door",
            arrives: Arrives::FrontDoor,
            held: 4,
            query: json!({ "size": 0, "query": { "term": { "group": "a" } } }),
            condition: "ctx.results[0].hits.total.value > 0",
            fires: true,
        },
        Row {
            case: "a condition the front door's run does not meet",
            arrives: Arrives::FrontDoor,
            held: 0,
            query: json!({ "size": 0, "query": { "term": { "group": "a" } } }),
            condition: "ctx.results[0].hits.total.value > 0",
            fires: false,
        },
    ]
}

/// The address the domain published for its own data plane, once it is a bare `host:port` a
/// client can dial.
///
/// The wait is for the CONDITION — the listener the domain publishes is open, which is what
/// gives the member that shape — and not for a span of time.
async fn dialable_endpoint(emulator: &Emulator, domain: &str) -> String {
    poll_until(&format!("the published address of {domain:?}"), || async {
        dialable_authority(&published_endpoint(emulator, domain, domain).await)
    })
    .await;
    published_endpoint(emulator, domain, domain).await
}

/// `POST /_plugins/_alerting/monitors/{id}/_execute` as it arrives at `arrives`.
///
/// The listener row goes through the very function the core hands a request accepted on the
/// domain's own socket, with the handle onto this emulator that listener was opened with — so
/// what it asserts is the path a client configured with the published endpoint really takes.
async fn executed(
    emulator: &Emulator,
    arrives: Arrives,
    address: &str,
    monitor_id: &str,
) -> (u16, Value) {
    let path = format!("/_plugins/_alerting/monitors/{monitor_id}/_execute");
    match arrives {
        Arrives::FrontDoor => {
            let answer = rest_prefixed(emulator, DOMAIN, "POST", &path, None).await;
            (answer.status, answer.body)
        }
        Arrives::PublishedListener => {
            let request = http::Request::builder()
                .method("POST")
                .uri(format!("https://{address}{path}"))
                .header("host", address)
                .header("content-type", "application/json")
                .header(
                    "authorization",
                    format!(
                        "AWS4-HMAC-SHA256 \
                         Credential={DEFAULT_ACCESS_KEY_ID}/20260101/{REGION}/es/aws4_request, \
                         SignedHeaders=host, Signature=0"
                    ),
                )
                .body(Vec::new())
                .expect("the test request is well formed");
            let callback = emulator.callback();
            let answered =
                serve_published(&**emulator.engine(), DOMAIN, &request, Some(&callback)).await;
            let status = answered.status().as_u16();
            let body: Value = serde_json::from_slice(answered.body()).unwrap_or(Value::Null);
            (status, body)
        }
    }
}

/// Load the index so it holds exactly `held` documents carrying `group: "a"`: every document
/// is removed, then `held` of them are written, through the API a client uses for each.
async fn given_documents(emulator: &Emulator, host: &str, case: &str, held: i64) {
    let emptied = rest(
        emulator,
        DEFAULT_ACCESS_KEY_ID,
        host,
        "POST",
        &format!("/{INDEX}/_delete_by_query?refresh=true"),
        Some(json!({ "query": { "match_all": {} } })),
    )
    .await;
    assert_eq!(
        emptied.status, 200,
        "case {case:?}: expected 200 from the delete by query, got {} with {:?}",
        emptied.status, emptied.body
    );
    if held > 0 {
        let mut payload = String::new();
        for at in 0..held {
            payload.push_str(
                &json!({ "index": { "_index": INDEX, "_id": format!("{case}-{at}") } })
                    .to_string(),
            );
            payload.push('\n');
            payload.push_str(&json!({ "group": "a", "n": at }).to_string());
            payload.push('\n');
        }
        let loaded = rest_payload(
            emulator,
            DEFAULT_ACCESS_KEY_ID,
            host,
            "POST",
            "/_bulk?refresh=true",
            payload,
        )
        .await;
        assert_eq!(
            (loaded.status, loaded.body["errors"].as_bool()),
            (200, Some(false)),
            "case {case:?}: expected the bulk load to answer 200 with no failed item, got {} \
             with {:?}",
            loaded.status,
            loaded.body
        );
    }
    let held_now = counted(emulator, host, INDEX).await;
    assert_eq!(
        held_now, held,
        "case {case:?}: expected the index to hold {held} document(s) for this run, got \
         {held_now}"
    );
}

/// RULE: a monitor run performs its firing trigger's actions at EVERY address the domain is
/// reached at — the listener it published as well as the front door — and withholds the
/// notification at both when the condition does not hold.
#[tokio::test]
async fn a_run_on_the_published_address_publishes_what_a_run_at_the_front_door_publishes() {
    let emulator = Emulator::with_notification_channels();
    let created = emulator
        .create(json!({
            "DomainName": DOMAIN,
            "EngineVersion": "OpenSearch_2.11",
            "ClusterConfig": { "InstanceType": "t3.small.search", "InstanceCount": 1 },
            "EBSOptions": { "EBSEnabled": true, "VolumeType": "gp3", "VolumeSize": 10 },
        }))
        .await;
    assert_eq!(
        created.status, 200,
        "setup CreateDomain({DOMAIN:?}): expected 200, got {} with {:?}",
        created.status, created.body
    );
    settled_domain(&emulator, DOMAIN).await;
    let host = endpoint_of(&emulator, DOMAIN);
    let address = dialable_endpoint(&emulator, DOMAIN).await;
    created_index(
        &emulator,
        &host,
        INDEX,
        json!({
            "settings": { "number_of_shards": 1, "number_of_replicas": 0 },
            "mappings": {
                "properties": { "group": { "type": "keyword" }, "n": { "type": "integer" } }
            },
        }),
    )
    .await;

    for (at, row) in rows().into_iter().enumerate() {
        let case = row.case;
        given_documents(&emulator, &host, case, row.held).await;

        // The channel and the monitor through the plugins' own APIs, and the topic and the
        // queue through Amazon SNS's and Amazon SQS's — one of each per row, so a row's
        // silence is read on a queue nothing else writes to.
        let name = format!("published-plane-{at}");
        let target = given_target(&emulator, &name).await;
        let channel_id = given_channel(&emulator, DOMAIN, &name, &target.topic_arn).await;
        let monitor_id = given_monitor(
            &emulator,
            DOMAIN,
            case,
            monitor_document(
                &name,
                INDEX,
                row.query,
                row.condition,
                &channel_id,
                &format!("{MARKER} hits={{{{ctx.results.0.hits.total.value}}}}"),
            ),
        )
        .await;

        let (status, answered) = executed(&emulator, row.arrives, &address, &monitor_id).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected 200 from the monitor run at {:?}, got {status} with \
             {answered:?}",
            row.arrives
        );
        let results = answered["trigger_results"]
            .as_object()
            .unwrap_or_else(|| {
                panic!("case {case:?}: expected the run to report its triggers, got {answered:?}")
            })
            .clone();
        assert_eq!(
            results.len(),
            1,
            "case {case:?}: expected exactly one trigger result, got {results:?}"
        );
        let result = results.values().next().cloned().unwrap_or(Value::Null);
        assert_eq!(
            result["triggered"].as_bool(),
            Some(row.fires),
            "case {case:?}: expected triggered {} over {} document(s), got {result:?}",
            row.fires,
            row.held
        );

        if row.fires {
            let rendered = format!("{MARKER} hits={}", row.held);
            let delivered = published_message(&emulator, case, &target.queue_url).await;
            assert_eq!(
                delivered, rendered,
                "case {case:?}: expected the queue subscribed to the channel's topic to carry \
                 the rendered count of the run at {:?}, got {delivered:?}",
                row.arrives
            );
            // And the action reports the send rather than an error: a run that handed its
            // notification to nobody is what leaves the queue empty with nothing to see.
            let errors: Vec<&Value> = result["action_results"]
                .as_object()
                .map(|actions| {
                    actions
                        .values()
                        .filter(|action| !action["error"].is_null())
                        .collect()
                })
                .unwrap_or_default();
            assert!(
                errors.is_empty(),
                "case {case:?}: expected the firing action to report no error at {:?}, got \
                 {errors:?}",
                row.arrives
            );
        } else {
            // The publish happens before the run answers, so a queue empty now was never
            // written to.
            let delivered = received(&emulator, &target.queue_url).await;
            assert!(
                delivered.is_empty(),
                "case {case:?}: expected the trigger to withhold its notification over {} \
                 document(s), got {delivered:?} on the queue",
                row.held
            );
        }
    }
}
