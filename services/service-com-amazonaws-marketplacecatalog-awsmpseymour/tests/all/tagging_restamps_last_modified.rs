// SPDX-License-Identifier: CC0-1.0

//! What a tag write leaves on the record it writes: the tag set, AND the instant that
//! record reports as its last modification.
//!
//! The rule under test: writing a resource's tags through the tagging surface MODIFIES
//! that resource, so a `TagResource` or `UntagResource` leaving the record holding a
//! different tag set restamps the record's last-modified instant to the instant of that
//! write — the restamp the tag write a change set carries already performs — while a
//! write that leaves the set it found modifies nothing and so moves no instant. A record
//! whose kind publishes no last-modified instant (a change set, whose timestamps are
//! those of the run it is) takes the tags alone.
//!
//! The instant is ONE field of the record, so every read of an entity agrees about it
//! (`DescribeEntity`, `BatchDescribeEntities`, `ListEntities`), and the entity list
//! filters and orders on the restamped value.
//!
//! The rows differ along what the rule generalises over: which operation writes, the
//! entity type, how many tags the write names, the form of the list it names them in,
//! whether the write changes the stored set at all, and the resource KIND addressed.
//!
//! Nothing here waits a fixed span or asserts on the wall clock: each wait polls the
//! SERVICE until the condition it waits for holds, under a generous deadline. The one
//! clock the cases compare against is the service's own, read back from a `VALIDATE`
//! change set's `StartTime` — a change set that applies nothing, so reading the clock
//! writes no entity.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use service_com_amazonaws_marketplacecatalog_awsmpseymour::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// The catalog every operation's `Catalog` member documents as its fixed value.
const CATALOG: &str = "AWSMarketplace";

/// A SigV4 credential scope naming this service's signing name (`aws-marketplace`): what
/// routes a request here, and whose region the ARNs this service mints name.
const SIGNED_FOR_EU_WEST_1: &str = "AWS4-HMAC-SHA256 \
     Credential=AKIDEXAMPLE/20260101/eu-west-1/aws-marketplace/aws4_request, \
     SignedHeaders=host, Signature=abc";

/// The restJson1 document media type.
const JSON: &str = "application/json";

/// How long a poll may keep looking before the test gives up. Generous on purpose: it
/// bounds a broken emulator, never the host's speed.
const POLL_DEADLINE: Duration = Duration::from_secs(60);

/// How long a poll waits between two looks. Only ever slept INSIDE a poll loop, so no
/// case's outcome depends on this span.
const POLL_PAUSE: Duration = Duration::from_millis(25);

/// One emulator: a router carrying only this service, over one shared engine.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(EmulatedService::new(engine));
    router
}

/// What the emulator answered: the status and the JSON body.
struct Answer {
    status: u16,
    body: Value,
}

impl Answer {
    /// The string at `pointer`, or the empty string.
    fn text(&self, pointer: &str) -> String {
        self.body
            .pointer(pointer)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    }

    /// The tags this answer reports, as `(key, value)` pairs sorted by key — the SET the
    /// resource carries, whatever order the list happens to report it in.
    fn tag_pairs(&self) -> Vec<(String, String)> {
        let mut pairs: Vec<(String, String)> = self
            .body
            .pointer("/Tags")
            .and_then(Value::as_array)
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

    /// The entity ids `/EntitySummaryList` reports, in the order served.
    fn listed_ids(&self) -> Vec<String> {
        self.body
            .pointer("/EntitySummaryList")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
            .iter()
            .map(|item| item["EntityId"].as_str().unwrap_or_default().to_string())
            .collect()
    }

    /// The `LastModifiedDate` `/EntitySummaryList` reports for `entity_id`.
    fn listed_stamp(&self, entity_id: &str) -> String {
        self.body
            .pointer("/EntitySummaryList")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
            .iter()
            .find(|item| item["EntityId"] == json!(entity_id))
            .and_then(|item| item["LastModifiedDate"].as_str().map(str::to_string))
            .unwrap_or_default()
    }
}

async fn call(router: &MultiServiceRouter, method: &str, uri: &str, body: Option<Value>) -> Answer {
    let payload = body.map(|value| value.to_string()).unwrap_or_default();
    let mut builder = ::http::Request::builder()
        .method(method)
        .uri(uri)
        .header("host", "catalog.marketplace.eu-west-1.amazonaws.com")
        .header("authorization", SIGNED_FOR_EU_WEST_1);
    if !payload.is_empty() {
        builder = builder.header("content-type", JSON);
    }
    let request = builder
        .body(Bytes::from(payload))
        .expect("the test request is well formed");

    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let bytes = response.into_body();
    let body = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or_else(|error| {
            panic!(
                "{method} {uri} answered a body that is not JSON ({error}): {:?}",
                String::from_utf8_lossy(&bytes)
            )
        })
    };
    Answer { status, body }
}

async fn list_tags(router: &MultiServiceRouter, resource_arn: &str) -> Answer {
    call(
        router,
        "POST",
        "/ListTagsForResource",
        Some(json!({"ResourceArn": resource_arn})),
    )
    .await
}

async fn tag_resource(router: &MultiServiceRouter, resource_arn: &str, tags: Value) -> Answer {
    call(
        router,
        "POST",
        "/TagResource",
        Some(json!({"ResourceArn": resource_arn, "Tags": tags})),
    )
    .await
}

async fn untag_resource(router: &MultiServiceRouter, resource_arn: &str, keys: Value) -> Answer {
    call(
        router,
        "POST",
        "/UntagResource",
        Some(json!({"ResourceArn": resource_arn, "TagKeys": keys})),
    )
    .await
}

async fn describe_change_set(router: &MultiServiceRouter, change_set_id: &str) -> Answer {
    call(
        router,
        "GET",
        &format!("/DescribeChangeSet?catalog={CATALOG}&changeSetId={change_set_id}"),
        None,
    )
    .await
}

async fn describe_entity(router: &MultiServiceRouter, entity_id: &str) -> Answer {
    call(
        router,
        "GET",
        &format!("/DescribeEntity?catalog={CATALOG}&entityId={entity_id}"),
        None,
    )
    .await
}

/// The second the SERVICE's clock is in, as the `DateTimeISO8601` it stamps on a change
/// set handed to it now. The change set's `Intent` is `VALIDATE`, which applies nothing,
/// so reading the clock leaves no entity behind.
async fn service_now(router: &MultiServiceRouter) -> String {
    let started = call(
        router,
        "POST",
        "/StartChangeSet",
        Some(json!({
            "Catalog": CATALOG,
            "Intent": "VALIDATE",
            "ChangeSet": [{
                "ChangeType": "CreateProduct",
                "Entity": {"Type": "AmiProduct@1.0"},
                "Details": json!({"ProductTitle": "clock probe"}).to_string(),
            }],
        })),
    )
    .await;
    assert_eq!(
        started.status, 200,
        "the clock probe must be accepted, got {} with {:?}",
        started.status, started.body
    );
    let described = describe_change_set(router, &started.text("/ChangeSetId")).await;
    let stamp = described.text("/StartTime");
    assert_eq!(
        stamp.len(),
        20,
        "the clock probe must report a DateTimeISO8601 StartTime, got {stamp:?} in {:?}",
        described.body
    );
    stamp
}

/// Wait until the service's own clock has LEFT the second `stamp` names, so that any
/// write made afterwards carries a strictly later stamp than it. `DateTimeISO8601` is
/// fixed-width and UTC, so byte order is chronological order.
async fn wait_past(router: &MultiServiceRouter, stamp: &str) {
    let deadline = Instant::now() + POLL_DEADLINE;
    loop {
        let reading = service_now(router).await;
        if reading.as_str() > stamp {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "the service clock must leave {stamp:?} behind, still reading {reading:?}"
        );
        std::thread::sleep(POLL_PAUSE);
    }
}

/// Wait until change set `change_set_id` has reached `SUCCEEDED`, polling the service for
/// the status rather than sleeping out the spans it publishes.
async fn wait_for_success(router: &MultiServiceRouter, change_set_id: &str) {
    let deadline = Instant::now() + POLL_DEADLINE;
    loop {
        let described = describe_change_set(router, change_set_id).await;
        let status = described.text("/Status");
        if status == "SUCCEEDED" {
            return;
        }
        assert!(
            status == "PREPARING" || status == "APPLYING",
            "change set {change_set_id} must run to SUCCEEDED, got status {status:?} with {:?}",
            described.body
        );
        assert!(
            Instant::now() < deadline,
            "change set {change_set_id} must reach SUCCEEDED, still {status:?}"
        );
        std::thread::sleep(POLL_PAUSE);
    }
}

/// One entity of `entity_type`, created through an applied change set that puts
/// `entity_tags` on it: its id, its own ARN, and the ARN of the change set that made it.
async fn created_entity(
    router: &MultiServiceRouter,
    entity_type: &str,
    title: &str,
    entity_tags: &[(&str, &str)],
) -> (String, String, String) {
    let mut change = json!({
        "ChangeType": "CreateProduct",
        "Entity": {"Type": format!("{entity_type}@1.0")},
        "Details": json!({"ProductTitle": title}).to_string(),
    });
    if !entity_tags.is_empty() {
        change["EntityTags"] = Value::Array(
            entity_tags
                .iter()
                .map(|(key, value)| json!({"Key": key, "Value": value}))
                .collect(),
        );
    }
    let started = call(
        router,
        "POST",
        "/StartChangeSet",
        Some(json!({"Catalog": CATALOG, "ChangeSet": [change]})),
    )
    .await;
    assert_eq!(
        started.status, 200,
        "case {title:?}: StartChangeSet must answer 200, got {} with {:?}",
        started.status, started.body
    );
    let change_set_id = started.text("/ChangeSetId");
    wait_for_success(router, &change_set_id).await;

    let described = describe_change_set(router, &change_set_id).await;
    let change_set_arn = described.text("/ChangeSetArn");
    let listed = call(router, "POST", "/ListChangeSets", Some(json!({"Catalog": CATALOG}))).await;
    let entity_id = listed
        .body
        .pointer("/ChangeSetSummaryList")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .find(|item| item["ChangeSetId"] == json!(change_set_id))
        .and_then(|item| item["EntityIdList"][0].as_str().map(str::to_string))
        .unwrap_or_else(|| {
            panic!(
                "case {title:?}: the change set must report its entity, got {:?}",
                listed.body
            )
        });

    let entity = describe_entity(router, &entity_id).await;
    assert_eq!(
        entity.status, 200,
        "case {title:?}: DescribeEntity must report the created entity, got {} with {:?}",
        entity.status, entity.body
    );
    (entity_id, entity.text("/EntityArn"), change_set_arn)
}

/// The instant every entity read reports for `entity_id`, asserted to be the SAME instant
/// in all three of them: one field of one record, however it is read.
async fn last_modified_date(router: &MultiServiceRouter, entity_id: &str, case: &str) -> String {
    let single = describe_entity(router, entity_id).await;
    assert_eq!(
        single.status, 200,
        "case {case:?}: DescribeEntity must serve {entity_id}, got {} with {:?}",
        single.status, single.body
    );
    let from_single = single.text("/LastModifiedDate");
    assert_eq!(
        from_single.len(),
        20,
        "case {case:?}: DescribeEntity must report a DateTimeISO8601 LastModifiedDate, got \
         {from_single:?}"
    );

    let batch = call(
        router,
        "POST",
        "/BatchDescribeEntities",
        Some(json!({"EntityRequestList": [{"Catalog": CATALOG, "EntityId": entity_id}]})),
    )
    .await;
    let from_batch = batch.text(&format!("/EntityDetails/{entity_id}/LastModifiedDate"));
    assert_eq!(
        from_batch, from_single,
        "case {case:?}: the batch read must report the instant the single read reports, got \
         {from_batch:?} beside {from_single:?}"
    );

    let listed = call(
        router,
        "POST",
        "/ListEntities",
        Some(json!({"Catalog": CATALOG, "EntityType": entity_type_of(router, entity_id).await})),
    )
    .await;
    let from_list = listed.listed_stamp(entity_id);
    assert_eq!(
        from_list, from_single,
        "case {case:?}: the entity list must report the instant the single read reports, got \
         {from_list:?} beside {from_single:?}"
    );
    from_single
}

/// The entity TYPE `entity_id` is of, read back from the catalog: `DescribeEntity` reports
/// it at its schema version (`EntityType@Version`), and the list is addressed by the type
/// alone.
async fn entity_type_of(router: &MultiServiceRouter, entity_id: &str) -> String {
    let described = describe_entity(router, entity_id).await;
    let reference = described.text("/EntityType");
    reference
        .split('@')
        .next()
        .unwrap_or_default()
        .to_string()
}

/// The write one case makes through the tagging surface.
enum Write {
    /// `TagResource` with this `Tags` list.
    Tag(Value),
    /// `UntagResource` with this `TagKeys` list.
    Untag(Value),
}

/// One row: the case, the entity type created, the tags the creating change puts on the
/// entity, the write the case then makes, whether that write leaves a DIFFERENT tag set
/// (and so modifies the entity), and the set the entity carries afterwards.
struct Row {
    case: &'static str,
    entity_type: &'static str,
    created_with: &'static [(&'static str, &'static str)],
    write: Write,
    changes_the_set: bool,
    left: &'static [(&'static str, &'static str)],
}

/// A tag write that changes an entity's tag set restamps the entity's `LastModifiedDate`,
/// and one that leaves the set it found does not.
#[tokio::test]
async fn a_tag_write_that_changes_the_set_restamps_the_entity() {
    let rows = [
        Row {
            case: "one tag added to an entity carrying none",
            entity_type: "AmiProduct",
            created_with: &[],
            write: Write::Tag(json!([{"Key": "env", "Value": "test"}])),
            changes_the_set: true,
            left: &[("env", "test")],
        },
        Row {
            case: "a second key added beside the one held",
            entity_type: "SaaSProduct",
            created_with: &[("owner", "team-a")],
            write: Write::Tag(json!([{"Key": "cost.centre", "Value": "4711"}])),
            changes_the_set: true,
            left: &[("cost.centre", "4711"), ("owner", "team-a")],
        },
        Row {
            case: "a new value for the key already held",
            entity_type: "ContainerProduct",
            created_with: &[("owner", "team-a")],
            write: Write::Tag(json!([{"Key": "owner", "Value": "team-b"}])),
            changes_the_set: true,
            left: &[("owner", "team-b")],
        },
        Row {
            case: "four keys written at once",
            entity_type: "DataProduct",
            created_with: &[],
            write: Write::Tag(json!([
                {"Key": "a", "Value": "1"},
                {"Key": "b", "Value": "2"},
                {"Key": "c", "Value": "3"},
                {"Key": "d", "Value": "4"},
            ])),
            changes_the_set: true,
            left: &[("a", "1"), ("b", "2"), ("c", "3"), ("d", "4")],
        },
        Row {
            case: "the key and value the entity already carries",
            entity_type: "MachineLearningProduct",
            created_with: &[("owner", "team-a")],
            write: Write::Tag(json!([{"Key": "owner", "Value": "team-a"}])),
            changes_the_set: false,
            left: &[("owner", "team-a")],
        },
        Row {
            case: "a list naming one key twice, whose last value is the one held",
            entity_type: "AmiProduct",
            created_with: &[("owner", "team-a")],
            write: Write::Tag(json!([
                {"Key": "owner", "Value": "team-z"},
                {"Key": "owner", "Value": "team-a"},
            ])),
            changes_the_set: false,
            left: &[("owner", "team-a")],
        },
        Row {
            case: "one of two keys untagged",
            entity_type: "SaaSProduct",
            created_with: &[("a", "1"), ("b", "2")],
            write: Write::Untag(json!(["a"])),
            changes_the_set: true,
            left: &[("b", "2")],
        },
        Row {
            case: "every key untagged at once",
            entity_type: "ContainerProduct",
            created_with: &[("a", "1"), ("b", "2")],
            write: Write::Untag(json!(["a", "b"])),
            changes_the_set: true,
            left: &[],
        },
        Row {
            case: "a key the entity does not carry untagged",
            entity_type: "DataProduct",
            created_with: &[("a", "1")],
            write: Write::Untag(json!(["absent-key"])),
            changes_the_set: false,
            left: &[("a", "1")],
        },
    ];

    for row in rows {
        let case = row.case;
        let router = emulator();
        let (entity_id, entity_arn, _) =
            created_entity(&router, row.entity_type, case, row.created_with).await;
        let created_at = last_modified_date(&router, &entity_id, case).await;

        // The write must land in a LATER second than the creation did, or the two stamps
        // would be equal whatever the service wrote. Waiting for the service's own clock
        // to leave that second is what makes the comparison below decide the rule.
        wait_past(&router, &created_at).await;

        let answered = match &row.write {
            Write::Tag(tags) => tag_resource(&router, &entity_arn, tags.clone()).await,
            Write::Untag(keys) => untag_resource(&router, &entity_arn, keys.clone()).await,
        };
        assert_eq!(
            answered.status, 200,
            "case {case:?}: the tag write must answer 200, got {} with {:?}",
            answered.status, answered.body
        );

        let expected_left: Vec<(String, String)> = row
            .left
            .iter()
            .map(|(key, value)| (key.to_string(), value.to_string()))
            .collect();
        let tags = list_tags(&router, &entity_arn).await;
        assert_eq!(
            tags.tag_pairs(),
            expected_left,
            "case {case:?}: the write must leave the set the rule describes, got {:?}",
            tags.body
        );

        let after = last_modified_date(&router, &entity_id, case).await;
        if row.changes_the_set {
            assert!(
                after > created_at,
                "case {case:?}: a write that changed the tag set must restamp the entity, \
                 got {after:?} after {created_at:?}"
            );
        } else {
            assert_eq!(
                after, created_at,
                "case {case:?}: a write that left the tag set it found must move no instant, \
                 got {after:?} after {created_at:?}"
            );
        }
    }
}

/// The instant a tag write restamps is the one the entity list FILTERS and ORDERS on: a
/// `LastModifiedDate` date range that excluded the entity before the write serves it
/// afterwards, and the default ordering moves it to the front.
#[tokio::test]
async fn the_restamped_instant_is_what_the_entity_list_filters_and_orders_on() {
    const ENTITY_TYPE: &str = "SaaSProduct";
    let router = emulator();
    let (tagged_id, tagged_arn, _) =
        created_entity(&router, ENTITY_TYPE, "the tagged product", &[]).await;
    let (untouched_id, _, _) =
        created_entity(&router, ENTITY_TYPE, "the untouched product", &[]).await;

    let created_at = last_modified_date(&router, &tagged_id, "list filter").await;
    let untouched_at = last_modified_date(&router, &untouched_id, "list filter").await;
    // The window must open after BOTH entities were created, so what it serves can only
    // be an entity a later write restamped.
    let baseline = created_at.clone().max(untouched_at);
    wait_past(&router, &baseline).await;
    // A window opening at the service's clock NOW: it excludes both entities, which were
    // created in earlier seconds, and includes anything modified from here on. The range
    // is inclusive of its bounds (`DateRange` is "dates between which" the attribute
    // falls), so the bound is read from the service rather than invented.
    let window_opens = service_now(&router).await;
    let window = json!({"SaaSProductFilters": {"LastModifiedDate": {"DateRange": {
        "AfterValue": window_opens,
    }}}});

    let before = call(
        &router,
        "POST",
        "/ListEntities",
        Some(json!({
            "Catalog": CATALOG,
            "EntityType": ENTITY_TYPE,
            "EntityTypeFilters": window.clone(),
        })),
    )
    .await;
    assert!(
        before.listed_ids().is_empty(),
        "a window opening after both entities were created must serve neither of them yet \
         ({tagged_id}, {untouched_id}), got {:?}",
        before.body
    );

    let tagged = tag_resource(
        &router,
        &tagged_arn,
        json!([{"Key": "owner", "Value": "team-a"}]),
    )
    .await;
    assert_eq!(
        tagged.status, 200,
        "TagResource must answer 200, got {} with {:?}",
        tagged.status, tagged.body
    );

    let after = call(
        &router,
        "POST",
        "/ListEntities",
        Some(json!({
            "Catalog": CATALOG,
            "EntityType": ENTITY_TYPE,
            "EntityTypeFilters": window,
        })),
    )
    .await;
    assert_eq!(
        after.listed_ids(),
        vec![tagged_id.clone()],
        "the tagged entity must fall inside the window its write restamped it into, and the \
         untouched entity {untouched_id} must not, got {:?}",
        after.body
    );

    // `Sort$SortBy` publishes `LastModifiedDate` as the attribute an entity list orders
    // by by default, so the entity whose write restamped it is the most recently modified
    // one: first descending, last ascending.
    for (order, expect_first) in [("DESCENDING", true), ("ASCENDING", false)] {
        let page = call(
            &router,
            "POST",
            "/ListEntities",
            Some(json!({
                "Catalog": CATALOG,
                "EntityType": ENTITY_TYPE,
                "Sort": {"SortBy": "LastModifiedDate", "SortOrder": order},
            })),
        )
        .await;
        let ids = page.listed_ids();
        assert_eq!(
            ids.len(),
            2,
            "ordering by LastModifiedDate {order}: the page must carry both entities \
             ({tagged_id}, {untouched_id}), got {ids:?}"
        );
        let position = ids.iter().position(|id| id == &tagged_id);
        let expected = if expect_first { Some(0) } else { Some(ids.len() - 1) };
        assert_eq!(
            position, expected,
            "ordering by LastModifiedDate {order}: the just-tagged entity must sort at \
             {expected:?}, got {position:?} in {ids:?}"
        );
    }
}

/// A change set publishes no instant of its own last modification — `StartTime` and
/// `EndTime` are the instants of the run it is — so tagging one writes its tags and moves
/// no timestamp, of it or of the entity it created.
#[tokio::test]
async fn tagging_a_change_set_moves_no_timestamp() {
    let router = emulator();
    let (entity_id, _, change_set_arn) =
        created_entity(&router, "AmiProduct", "a change set to tag", &[]).await;
    let change_set_id = change_set_arn
        .rsplit('/')
        .next()
        .expect("a change set ARN names the change set")
        .to_string();

    let before = describe_change_set(&router, &change_set_id).await;
    let (start_time, end_time) = (before.text("/StartTime"), before.text("/EndTime"));
    assert_eq!(
        (start_time.len(), end_time.len()),
        (20, 20),
        "a succeeded change set reports both instants of its run, got {:?}",
        before.body
    );
    let entity_before = last_modified_date(&router, &entity_id, "change set tagging").await;
    // The write must land in a second later than every instant this assertion compares,
    // so a timestamp that did move could not be mistaken for one that did not.
    wait_past(&router, &end_time.clone().max(entity_before.clone())).await;

    let tagged = tag_resource(
        &router,
        &change_set_arn,
        json!([{"Key": "wave", "Value": "one"}]),
    )
    .await;
    assert_eq!(
        tagged.status, 200,
        "TagResource must answer 200 for a change set, got {} with {:?}",
        tagged.status, tagged.body
    );
    let tags = list_tags(&router, &change_set_arn).await;
    assert_eq!(
        tags.tag_pairs(),
        vec![("wave".to_string(), "one".to_string())],
        "the change set must report the tag that was written, got {:?}",
        tags.body
    );

    let after = describe_change_set(&router, &change_set_id).await;
    assert_eq!(
        (after.text("/StartTime"), after.text("/EndTime")),
        (start_time, end_time),
        "tagging a change set must move neither of the instants of its run, got {:?}",
        after.body
    );
    assert_eq!(
        last_modified_date(&router, &entity_id, "change set tagging").await,
        entity_before,
        "tagging a change set must not restamp the entity it created"
    );
}
