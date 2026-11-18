// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the `Backfilling` member every index description carries: whether
//! an index still has the items its table ALREADY held to read.
//!
//! The rule pinned here, over both index kinds that report the member: an index ADDED to a
//! table that already holds items is described as backfilling — `IndexStatus` `CREATING`
//! with `Backfilling` true — until those items have been processed, and only then as
//! `ACTIVE` and not backfilling, in that order; an index added to a table holding no items
//! has nothing to read and never reports a backfill; and an index defined WITH its table
//! reports no `Backfilling` member at all. That is what the two docs state:
//! "Backfilling is the process of reading items from the table and determining whether
//! they can be added to the index. (Not all items will qualify ...) ... After all items
//! have been processed, the backfilling operation is complete and Backfilling is false" and
//! "For indexes that were created during a CreateTable operation, the Backfilling attribute
//! does not appear in the DescribeTable output"
//! (`GlobalSecondaryIndexDescription$Backfilling`), and "Specifies whether the index is
//! currently backfilling. During backfill, SearchVectors operations might return incomplete
//! results" (`VectorIndexDescription$Backfilling`).
//!
//! Each rule is asserted over a SET of inputs rather than one example — which KIND of index
//! is added (vector, global secondary), how many items the table already held when it was
//! added, and whether those items even carry the attribute the index indexes — and the item
//! written while the window is open is read back through the index once it is `ACTIVE`,
//! because a write accepted mid-backfill is indexed too.
//!
//! Every read is driven over the wire (`POST /` with
//! `X-Amz-Target: DynamoDB_20120810.<OperationName>`) through the same
//! `MultiServiceRouter` the emulator's front doors use. HOW LONG the window stays open is
//! the service's own span, which no test waits for: the suite below moves the stored
//! instants the way a later request's clock moves them (the pattern the metered-counter
//! suite uses), so what a loaded host does to the schedule cannot decide whether the window
//! could be observed. One test leaves the span alone and asserts only what holds whenever it
//! is polled.

use std::sync::Arc;
use std::time::{Duration, SystemTime};

use serde_json::{json, Value};

use service_com_amazonaws_dynamodb_dynamodb_20120810::state::{
    service_state, IndexBackfill,
};

use crate::emulator::{
    call, describe_when_active, emulator, emulator_and_engine, ok, ACTIVE_DEADLINE, POLL_INTERVAL,
    REGION,
};

/// The two index kinds whose description carries `Backfilling`
/// (`GlobalSecondaryIndexDescription$Backfilling`, `VectorIndexDescription$Backfilling`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Vector,
    Global,
}

impl Kind {
    /// The `TableDescription` member this kind's descriptions arrive in.
    fn member(self) -> &'static str {
        match self {
            Self::Vector => "VectorIndexes",
            Self::Global => "GlobalSecondaryIndexes",
        }
    }
}

/// The name every test adds its index under.
const ADDED: &str = "late-index";

/// How far out the stored window is moved while the assertions inside it are made. Nothing
/// waits for it: it is a deadline the test sets and then closes itself, generous so that a
/// loaded host cannot walk past it mid-assertion.
const HELD_OPEN: Duration = Duration::from_secs(600);

/// One embedding, in the DynamoDB list format a vector attribute is written in.
fn embedding(values: &[f64]) -> Value {
    Value::Array(
        values
            .iter()
            .map(|value| json!({"N": value.to_string()}))
            .collect(),
    )
}

/// One item of the tables below: a `pk`, the `tenant` a global secondary index keys on, a
/// payload, and — when `vector` — the four-dimensional `emb` a vector index indexes.
fn item(index: usize, vector: bool) -> Value {
    let mut item = json!({
        "pk": {"S": format!("item-{index}")},
        "tenant": {"S": "tenant-1"},
        "payload": {"S": format!("payload {index}")},
    });
    if vector {
        let offset = index as f64;
        item["emb"] = json!({"L": embedding(&[1.0 + offset, 0.0, 0.5, -1.0])});
    }
    item
}

/// Create one on-demand table with a `pk` partition key and, when `with_table` names a
/// kind, that kind's index DEFINED with the table; then wait for it to be `ACTIVE`.
///
/// Only the attributes some key schema uses are declared: "Some AttributeDefinitions are
/// not used in any KeySchema" refuses a definition no schema names, so the `tenant` a
/// global secondary index keys on is declared only by the call that declares that index.
async fn create_table(
    router: &smithy_server_runtime::MultiServiceRouter,
    case: &str,
    name: &str,
    with_table: Option<Kind>,
) {
    let mut definitions = vec![json!({"AttributeName": "pk", "AttributeType": "S"})];
    if with_table == Some(Kind::Global) {
        definitions.push(json!({"AttributeName": "tenant", "AttributeType": "S"}));
    }
    let mut request = json!({
        "TableName": name,
        "BillingMode": "PAY_PER_REQUEST",
        "KeySchema": [{"AttributeName": "pk", "KeyType": "HASH"}],
        "AttributeDefinitions": definitions,
    });
    if let Some(kind) = with_table {
        request[kind.member()] = json!([declaration(kind)]);
    }
    ok(router, "CreateTable", request).await;
    describe_when_active(router, case, name).await;
}

/// Write `count` items into `table`, the first being item 0.
async fn fill(
    router: &smithy_server_runtime::MultiServiceRouter,
    table: &str,
    count: usize,
    vector: bool,
) {
    for index in 0..count {
        ok(
            router,
            "PutItem",
            json!({"TableName": table, "Item": item(index, vector)}),
        )
        .await;
    }
}

/// The declaration of the index this suite adds, for one kind.
fn declaration(kind: Kind) -> Value {
    match kind {
        Kind::Vector => json!({
            "IndexName": ADDED,
            "VectorAttribute": {"AttributeName": "emb"},
            "Dimensions": 4,
            "DistanceFunction": "EUCLIDEAN",
            "Projection": {"ProjectionType": "ALL"},
        }),
        Kind::Global => json!({
            "IndexName": ADDED,
            "KeySchema": [{"AttributeName": "tenant", "KeyType": "HASH"}],
            "Projection": {"ProjectionType": "ALL"},
        }),
    }
}

/// The `UpdateTable` request that ADDS one index of `kind` to `table`. A created global
/// secondary index needs its key element declared in the request itself ("If you are adding
/// a new global secondary index to the table, AttributeDefinitions must include the key
/// element(s) of the new index" — `UpdateTable$AttributeDefinitions`).
fn add_request(table: &str, kind: Kind) -> Value {
    match kind {
        Kind::Vector => json!({
            "TableName": table,
            "VectorIndexUpdates": [{"Create": declaration(kind)}],
        }),
        Kind::Global => json!({
            "TableName": table,
            "AttributeDefinitions": [{"AttributeName": "tenant", "AttributeType": "S"}],
            "GlobalSecondaryIndexUpdates": [{"Create": declaration(kind)}],
        }),
    }
}

/// Move the stored index's backfill and its transient span to `complete_at`: when the items
/// the table already held are done being read, which is also when the index is `ACTIVE`.
///
/// The span's LENGTH is the service's own and no test waits for it, so it is driven here the
/// way a later request's clock drives it, through the stored table — the pattern the
/// metered-counter suite uses for a refresh no test can wait for. Everything the assertions
/// then read is what the store actually holds, over the wire.
fn move_window(
    engine: &Arc<engine::InMemoryEngine>,
    table: &str,
    kind: Kind,
    complete_at: SystemTime,
) {
    let state = service_state(&**engine).expect("the shared engine holds this service's store");
    let mut store = state.lock();
    let table = store
        .table_mut(REGION, table)
        .expect("the table the update was accepted for");
    match kind {
        Kind::Vector => {
            for index in table.vector_indexes.iter_mut().filter(|i| i.name == ADDED) {
                index.active_at = complete_at;
                index.backfill = IndexBackfill::Added { complete_at };
            }
        }
        Kind::Global => {
            for index in table
                .global_secondary_indexes
                .iter_mut()
                .filter(|i| i.name == ADDED)
            {
                index.active_at = complete_at;
                index.backfill = IndexBackfill::Added { complete_at };
            }
        }
    }
}

/// The description `table_description` carries for the index named [`ADDED`] of `kind`, or
/// `Value::Null` when it reports none.
fn described_index(table_description: &Value, kind: Kind) -> Value {
    table_description[kind.member()]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .find(|index| index["IndexName"] == json!(ADDED))
        .unwrap_or(Value::Null)
}

/// The `(IndexStatus, Backfilling)` pair a client reads off one index description, where an
/// absent `Backfilling` is no backfill — which is how a client that gates on the member
/// reads it.
fn state(index: &Value) -> (String, bool) {
    (
        index["IndexStatus"]
            .as_str()
            .unwrap_or("<no IndexStatus>")
            .to_string(),
        index["Backfilling"].as_bool().unwrap_or(false),
    )
}

/// That pair as a settled index reports it.
fn settled() -> (String, bool) {
    ("ACTIVE".to_string(), false)
}

/// That pair as an index still reading the table's items reports it.
fn backfilling() -> (String, bool) {
    ("CREATING".to_string(), true)
}

/// The pair `DescribeTable` reports for the index named [`ADDED`] right now.
async fn reported_state(
    router: &smithy_server_runtime::MultiServiceRouter,
    case: &str,
    table: &str,
    kind: Kind,
) -> (String, bool) {
    let described = ok(router, "DescribeTable", json!({"TableName": table})).await;
    let index = described_index(&described["Table"], kind);
    assert!(
        !index.is_null(),
        "case {case:?}: the table reports the index the accepted update added, got {described}"
    );
    state(&index)
}

/// Poll `DescribeTable` until the index named [`ADDED`] is `ACTIVE` and not backfilling, and
/// answer every distinct `(IndexStatus, Backfilling)` pair it was seen in, in order.
///
/// The waiter loop a client runs after adding an index, bounded by the shared deadline: an
/// index that never settles panics naming the pairs it was seen in rather than hanging.
async fn observed_states(
    router: &smithy_server_runtime::MultiServiceRouter,
    case: &str,
    table: &str,
    kind: Kind,
) -> Vec<(String, bool)> {
    let started = std::time::Instant::now();
    let mut states: Vec<(String, bool)> = Vec::new();
    while started.elapsed() < ACTIVE_DEADLINE {
        let pair = reported_state(router, case, table, kind).await;
        if states.last() != Some(&pair) {
            states.push(pair.clone());
        }
        if pair == settled() {
            return states;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
    panic!(
        "case {case:?}: the index never reached ACTIVE and not backfilling within \
         {ACTIVE_DEADLINE:?}, states {states:?}"
    )
}

/// The `pk` of every item a `SearchVectors` through the added index answered.
async fn searched_keys(
    router: &smithy_server_runtime::MultiServiceRouter,
    case: &str,
    table: &str,
) -> Vec<String> {
    let (status, response) = call(
        router,
        "SearchVectors",
        json!({
            "TableName": table,
            "IndexName": ADDED,
            "SearchVector": embedding(&[1.0, 0.0, 0.5, -1.0]),
            "TopK": 50,
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: a settled vector index serves SearchVectors, got {status} with {response}"
    );
    let mut keys: Vec<String> = response["SearchResults"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|result| {
            result["Item"]["pk"]
                .get("S")
                .and_then(Value::as_str)
                .unwrap_or("<no pk>")
                .to_string()
        })
        .collect();
    keys.sort();
    keys
}

/// The `pk` of every item a `Scan` of the added global secondary index answered.
async fn scanned_keys(
    router: &smithy_server_runtime::MultiServiceRouter,
    case: &str,
    table: &str,
) -> Vec<String> {
    let (status, response) = call(
        router,
        "Scan",
        json!({"TableName": table, "IndexName": ADDED}),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: a settled secondary index serves Scan, got {status} with {response}"
    );
    let mut keys: Vec<String> = response["Items"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|item| {
            item["pk"]
                .get("S")
                .and_then(Value::as_str)
                .unwrap_or("<no pk>")
                .to_string()
        })
        .collect();
    keys.sort();
    keys
}

#[tokio::test]
async fn an_index_added_over_pre_existing_items_backfills_them_before_it_is_active() {
    // Rows differ along what the rule generalises over: which KIND of index reports the
    // member, how many items the table already held when the index was added, and whether
    // those items carry the attribute the index indexes at all ("Not all items will
    // qualify" — the `Backfilling` doc), which does not change that the table's items have
    // to be read.
    let cases: [(&str, Kind, usize, bool); 4] = [
        (
            "a vector index over one pre-existing item",
            Kind::Vector,
            1,
            true,
        ),
        (
            "a vector index over thirty pre-existing items",
            Kind::Vector,
            30,
            true,
        ),
        (
            "a vector index over items that carry no vector",
            Kind::Vector,
            4,
            false,
        ),
        (
            "a global secondary index over pre-existing items",
            Kind::Global,
            5,
            true,
        ),
    ];

    for (position, (case, kind, existing, vectors)) in cases.into_iter().enumerate() {
        let (router, engine) = emulator_and_engine();
        let table = format!("Backfill{position}");
        create_table(&router, case, &table, None).await;
        fill(&router, &table, existing, vectors).await;

        // The call that adds the index already reports it reading the table's items.
        let accepted = ok(&router, "UpdateTable", add_request(&table, kind)).await;
        let added = described_index(&accepted["TableDescription"], kind);
        assert_eq!(
            state(&added),
            backfilling(),
            "case {case:?}: the accepted update reports the added index backfilling the items \
             the table already held, got {accepted}"
        );

        // Hold the window open for the assertions inside it, then read it back over the
        // wire: DescribeTable reports the same pair the update did.
        move_window(&engine, &table, kind, SystemTime::now() + HELD_OPEN);
        assert_eq!(
            reported_state(&router, case, &table, kind).await,
            backfilling(),
            "case {case:?}: an index with the table's items still to read is described \
             backfilling"
        );

        // A write accepted inside that window is indexed too, and does not end the window.
        ok(
            &router,
            "PutItem",
            json!({"TableName": table, "Item": item(existing, true)}),
        )
        .await;
        assert_eq!(
            reported_state(&router, case, &table, kind).await,
            backfilling(),
            "case {case:?}: a write made inside the window leaves the index still backfilling"
        );

        // Once the items have been processed the index is ACTIVE and not backfilling — the
        // pair every assertion above has just been shown to precede.
        move_window(&engine, &table, kind, SystemTime::now());
        assert_eq!(
            reported_state(&router, case, &table, kind).await,
            settled(),
            "case {case:?}: with the table's items processed the index is ACTIVE and not \
             backfilling"
        );

        // The settled index holds the items the table already carried AND the one written
        // while it was backfilling. A vector index holds only the items that carry a
        // vector, which is what makes the third row's expectation its own.
        let expected: Vec<String> = match (kind, vectors) {
            (Kind::Vector, false) => vec![format!("item-{existing}")],
            _ => {
                let mut keys: Vec<String> =
                    (0..=existing).map(|index| format!("item-{index}")).collect();
                keys.sort();
                keys
            }
        };
        let found = match kind {
            Kind::Vector => searched_keys(&router, case, &table).await,
            Kind::Global => scanned_keys(&router, case, &table).await,
        };
        assert_eq!(
            found, expected,
            "case {case:?}: the settled index holds the pre-existing items and the one written \
             while it was backfilling, got {found:?}"
        );
    }
}

#[tokio::test]
async fn the_backfill_window_runs_the_whole_creating_phase_of_the_services_own_span() {
    // The same rule on the service's OWN span, with nothing driving the clock: whenever a
    // client's waiter looks, an index added over pre-existing items that is still CREATING
    // is backfilling — the pair the defect reported, ('CREATING', false), is never a state
    // such an index is in — and the wait ends on the settled pair. Both kinds, because both
    // report the member.
    let cases: [(&str, Kind); 2] = [
        ("a vector index on the service's own span", Kind::Vector),
        (
            "a global secondary index on the service's own span",
            Kind::Global,
        ),
    ];

    for (position, (case, kind)) in cases.into_iter().enumerate() {
        let router = emulator();
        let table = format!("BackfillSpan{position}");
        create_table(&router, case, &table, None).await;
        fill(&router, &table, 3, true).await;

        let accepted = ok(&router, "UpdateTable", add_request(&table, kind)).await;
        let added = described_index(&accepted["TableDescription"], kind);
        assert_eq!(
            state(&added),
            backfilling(),
            "case {case:?}: the accepted update reports the added index backfilling, got \
             {accepted}"
        );

        let states = observed_states(&router, case, &table, kind).await;
        assert!(
            !states.contains(&("CREATING".to_string(), false)),
            "case {case:?}: an index added over pre-existing items is never CREATING without \
             backfilling, got states {states:?}"
        );
        assert_eq!(
            states.last(),
            Some(&settled()),
            "case {case:?}: the wait ends on the settled pair, got states {states:?}"
        );
    }
}

#[tokio::test]
async fn an_index_added_to_a_table_holding_no_items_never_reports_a_backfill() {
    // Nothing to read, so no backfill — for either kind of index.
    let cases: [(&str, Kind); 2] = [
        ("a vector index on an empty table", Kind::Vector),
        ("a global secondary index on an empty table", Kind::Global),
    ];

    for (position, (case, kind)) in cases.into_iter().enumerate() {
        let router = emulator();
        let table = format!("BackfillEmpty{position}");
        create_table(&router, case, &table, None).await;

        let accepted = ok(&router, "UpdateTable", add_request(&table, kind)).await;
        let added = described_index(&accepted["TableDescription"], kind);
        assert_eq!(
            state(&added),
            ("CREATING".to_string(), false),
            "case {case:?}: an index with nothing to read is CREATING and not backfilling, got \
             {accepted}"
        );

        let states = observed_states(&router, case, &table, kind).await;
        assert!(
            !states.contains(&backfilling()),
            "case {case:?}: an index added to a table holding no items never backfills, got \
             states {states:?}"
        );
        assert_eq!(
            states.last(),
            Some(&settled()),
            "case {case:?}: the index still settles ACTIVE and not backfilling, got states \
             {states:?}"
        );
    }
}

#[tokio::test]
async fn an_index_defined_with_its_table_reports_no_backfilling_member() {
    // "For indexes that were created during a CreateTable operation, the Backfilling
    // attribute does not appear in the DescribeTable output" (the `Backfilling` doc) — for
    // either kind, and whether or not the table has since been filled with items.
    let cases: [(&str, Kind); 2] = [
        ("a vector index declared with the table", Kind::Vector),
        (
            "a global secondary index declared with the table",
            Kind::Global,
        ),
    ];

    for (position, (case, kind)) in cases.into_iter().enumerate() {
        let router = emulator();
        let table = format!("BackfillWithTable{position}");
        create_table(&router, case, &table, Some(kind)).await;

        // Before any item is written, and again after the table has been filled: the member
        // is a fact about where the index was DEFINED, not about what the table holds.
        for (stage, items) in [("an empty table", 0usize), ("a filled table", 3)] {
            fill(&router, &table, items, true).await;
            let described = ok(&router, "DescribeTable", json!({"TableName": table})).await;
            let index = described_index(&described["Table"], kind);
            assert!(
                index.get("Backfilling").is_none(),
                "case {case:?} on {stage:?}: an index defined with its table reports no \
                 Backfilling member, got {index}"
            );
            assert_eq!(
                index["IndexStatus"].as_str().unwrap_or_default(),
                "ACTIVE",
                "case {case:?} on {stage:?}: the index is settled, got {index}"
            );
        }
    }
}
