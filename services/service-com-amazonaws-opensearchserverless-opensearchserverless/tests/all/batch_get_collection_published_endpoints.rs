// SPDX-License-Identifier: CC0-1.0

//! The endpoints EVERY entry of one `BatchGetCollection` response publishes.
//!
//! `BatchGetCollection` "Returns attributes for one or more collections, including the
//! collection endpoint, the OpenSearch Dashboards endpoint, and FIPS-compliant endpoints",
//! over a selector of up to a hundred `ids` or `names`
//! (`smithy_query(query="operation",operation="BatchGetCollection")`). None of those four
//! members is a label a caller reads back: each is an address a client is CONFIGURED with
//! and then DIALS, so the rule under test is that every endpoint member of EVERY
//! `collectionDetails` entry names an address this emulator answers that entry's own
//! collection on — however many entries one response carries, whichever selector asked for
//! them, and whether or not the same call also named keys nothing answers.
//!
//! The sibling suite `announced_collection_endpoints` pins the rule on the value one
//! collection publishes; this one pins it on the SHAPE of the response, which is what a
//! batch read varies: the number of entries, the selector they were asked for by, and the
//! error details beside them.

use serde_json::{json, Value};

use crate::announced_collection_endpoints::{
    admit_network_access, assert_published_addresses, authority_of, await_active, call_at,
    emulator_served_at, knn_index_body, published, rest_at_front_door, through_endpoint,
    REGION, SIGNING_NAME,
};
use crate::collection_lifecycle::text;

/// Where the emulator under test reports itself as served — the base a client reads off its
/// own front door. Stated once for both tests below; it is a reported base, not a socket,
/// and every assertion about a published address is about the port that collection's own
/// listener opened.
const SERVED_BASE: &str = "https://127.0.0.1:39355";

/// The names of the collections every case below reads, enough of them for a response that
/// carries several entries. Three is the smallest count that tells "the first entry" and
/// "the last entry" apart from "every entry".
const COLLECTION_NAMES: [&str; 3] = ["batch-read-first", "batch-read-second", "batch-read-third"];

/// One collection of the emulator under test: what a batch read may select it by, the
/// address it publishes, and the index only it holds.
struct Collection {
    /// `CollectionDetail$id`, one of the two values a batch read selects by.
    id: String,
    /// `CollectionDetail$name`, the other.
    name: String,
    /// The index created through this collection's own published endpoint, so a request
    /// that reaches the wrong collection is answered differently from one that reaches
    /// this one.
    index: String,
}

/// Create every collection in `COLLECTION_NAMES` and answer them once each is ACTIVE, with
/// an index of its own created through the address it publishes.
///
/// Every create is issued before the first poll, so the `CREATING -> ACTIVE` move the clock
/// owes each collection is outstanding at the same time; nothing asserts on how long it
/// took.
async fn collections_with_an_index_each(
    router: &smithy_server_runtime::MultiServiceRouter,
    engine: &std::sync::Arc<engine::InMemoryEngine>,
    host: &str,
) -> Vec<Collection> {
    admit_network_access(router, host).await;
    let mut created_ids: Vec<(String, String)> = Vec::new();
    for name in COLLECTION_NAMES {
        let (status, created) = call_at(
            router,
            host,
            "CreateCollection",
            json!({
                "name": name,
                "type": "VECTORSEARCH",
                "encryptionConfig": { "aWSOwnedKey": true },
            }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case CreateCollection({name}): expected 200, got {status} with {created}"
        );
        created_ids.push((text(&created, "createCollectionDetail.id"), name.to_string()));
    }

    let mut collections = Vec::new();
    for (id, name) in created_ids {
        let detail = await_active(router, host, &id).await;
        let endpoint = text(&detail, "collectionEndpoint");
        let index = format!("only-in-{id}");
        let (status, answer) = through_endpoint(
            engine,
            &id,
            &endpoint,
            "PUT",
            &format!("/{index}"),
            Some(&knn_index_body(8)),
        );
        assert_eq!(
            (status, answer.get("acknowledged")),
            (200, Some(&json!(true))),
            "case PUT {endpoint}/{index}: expected 200 acknowledged, got {status} with {answer}"
        );
        collections.push(Collection { id, name, index });
    }
    collections
}

/// RULE: every endpoint member of every `collectionDetails` entry names an address this
/// emulator answers THAT entry's collection on, whatever shape the response has.
///
/// The rows span what a batch read varies: how many entries the response carries, which
/// selector asked for them, and whether keys nothing answers are named beside them.
#[tokio::test]
async fn every_entry_of_a_batch_read_publishes_an_address_this_emulator_serves() {
    /// One case: how many of the emulator's collections the call names, whether it names
    /// them by `names` rather than `ids`, and how many keys it names that answer nothing.
    struct Row {
        case: &'static str,
        selected: usize,
        by_name: bool,
        absent: usize,
    }

    let rows = [
        Row {
            case: "one collection by id",
            selected: 1,
            by_name: false,
            absent: 0,
        },
        Row {
            case: "two collections by id",
            selected: 2,
            by_name: false,
            absent: 0,
        },
        Row {
            case: "every collection by id",
            selected: 3,
            by_name: false,
            absent: 0,
        },
        Row {
            case: "one collection by name",
            selected: 1,
            by_name: true,
            absent: 0,
        },
        Row {
            case: "every collection by name",
            selected: 3,
            by_name: true,
            absent: 0,
        },
        Row {
            case: "two collections by id beside two keys nothing answers",
            selected: 2,
            by_name: false,
            absent: 2,
        },
    ];

    let (router, engine) = emulator_served_at(Some(SERVED_BASE));
    let host = format!("{SIGNING_NAME}.{REGION}.amazonaws.com");
    let collections = collections_with_an_index_each(&router, &engine, &host).await;

    for Row {
        case,
        selected,
        by_name,
        absent,
    } in rows
    {
        let asked: Vec<&Collection> = collections.iter().take(selected).collect();
        // The keys the call names: the selected collections in order, then the ones that
        // answer nothing, which the model reports in `collectionErrorDetails` instead.
        let mut keys: Vec<String> = asked
            .iter()
            .map(|collection| {
                if by_name {
                    collection.name.clone()
                } else {
                    collection.id.clone()
                }
            })
            .collect();
        for nth in 0..absent {
            keys.push(format!("nothinganswersthiskey{nth}"));
        }
        let selector = if by_name { "names" } else { "ids" };
        let (status, read) = call_at(
            &router,
            &host,
            "BatchGetCollection",
            json!({ selector: keys }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected 200, got {status} with {read}"
        );
        let details = read
            .get("collectionDetails")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        assert_eq!(
            details.len(),
            selected,
            "case {case:?}: expected {selected} collectionDetails entries, got {read}"
        );
        let errors = read
            .get("collectionErrorDetails")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        assert_eq!(
            errors.len(),
            absent,
            "case {case:?}: expected {absent} collectionErrorDetails entries, got {read}"
        );

        let mut addresses: Vec<String> = Vec::new();
        for (nth, (detail, collection)) in details.iter().zip(&asked).enumerate() {
            let entry = format!("{case}: collectionDetails[{nth}]");
            // The entries answer the selected keys in request order, so an entry's
            // endpoints belong to the collection that key named. AWS documents no order
            // for a batch response, so this is the emulator's own guarantee (the handler
            // answers each requested key in turn), relied on here to say WHICH collection
            // an entry's addresses are read against.
            assert_eq!(
                (text(detail, "id"), text(detail, "name")),
                (collection.id.clone(), collection.name.clone()),
                "case {entry:?}: expected the collection {:?} the key named, got {detail}",
                collection.id
            );
            let endpoint = assert_published_addresses(&entry, detail);
            addresses.push(authority_of(&endpoint));

            // The published authority RESOLVES to this entry's own collection: the index
            // only it holds is served there, and the index of another collection is not.
            for other in &collections {
                let expected = if other.index == collection.index {
                    200
                } else {
                    404
                };
                let (status, answer) = rest_at_front_door(
                    &router,
                    &authority_of(&endpoint),
                    "GET",
                    &format!("/{}", other.index),
                )
                .await;
                assert_eq!(
                    status, expected,
                    "case {entry:?}: GET /{} at the published authority {:?} expected \
                     {expected}, got {status} with {answer}",
                    other.index,
                    authority_of(&endpoint)
                );
            }
        }
        // One address per collection, so a response carrying several entries carries
        // several distinct addresses rather than one shared by all of them.
        let mut distinct = addresses.clone();
        distinct.sort();
        distinct.dedup();
        assert_eq!(
            distinct.len(),
            addresses.len(),
            "case {case:?}: each entry must publish an address of its own, got {addresses:?}"
        );
    }
}

/// RULE: the endpoints an entry of a batch read publishes are the ones that collection
/// publishes, so reading several collections at once answers exactly what reading each of
/// them alone does — including when the selector is `names` rather than `ids`.
#[tokio::test]
async fn a_batch_read_publishes_what_each_collection_publishes_alone() {
    let (router, engine) = emulator_served_at(Some(SERVED_BASE));
    let host = format!("{SIGNING_NAME}.{REGION}.amazonaws.com");
    let collections = collections_with_an_index_each(&router, &engine, &host).await;

    // Every collection in ONE call, by id and then the same by name.
    let ids: Vec<String> = collections
        .iter()
        .map(|collection| collection.id.clone())
        .collect();
    let names: Vec<String> = collections
        .iter()
        .map(|collection| collection.name.clone())
        .collect();
    for (case, body) in [
        ("every collection by id", json!({ "ids": ids })),
        ("every collection by name", json!({ "names": names })),
    ] {
        let (status, read) = call_at(&router, &host, "BatchGetCollection", body).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected 200, got {status} with {read}"
        );
        let details = read
            .get("collectionDetails")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        // Counted before the entries are read, so a response that carried none — or fewer
        // than the call named — cannot satisfy the loop below by never running it.
        assert_eq!(
            details.len(),
            collections.len(),
            "case {case:?}: expected one entry per collection named, got {read}"
        );
        for (detail, collection) in details.iter().zip(&collections) {
            // Read against the rule first, so the comparison below cannot be satisfied by
            // two answers that publish nothing.
            assert_published_addresses(&format!("{case}: the entry for {}", collection.id), detail);
            let (status, alone) = call_at(
                &router,
                &host,
                "BatchGetCollection",
                json!({ "ids": [collection.id.clone()] }),
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case:?}: reading {} alone expected 200, got {status} with {alone}",
                collection.id
            );
            let read_alone = alone
                .get("collectionDetails")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            // Counted before the entry is read, so the comparison below reports the case
            // rather than failing inside the reader on an absent entry.
            assert_eq!(
                read_alone.len(),
                1,
                "case {case:?}: reading {} alone expected one entry, got {alone}",
                collection.id
            );
            let single = read_alone.first().cloned().unwrap_or(Value::Null);
            assert_eq!(
                published(detail),
                published(&single),
                "case {case:?}: the entry for {} must publish what reading it alone \
                 publishes, got {detail} beside {single}",
                collection.id
            );
        }
    }
}
