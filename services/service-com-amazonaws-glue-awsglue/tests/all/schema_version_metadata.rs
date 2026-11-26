// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the metadata key value pairs a SCHEMA VERSION carries:
//! `PutSchemaVersionMetadata`, `RemoveSchemaVersionMetadata` and the
//! `QuerySchemaVersionMetadata` that reads them back.
//!
//! The rule under test: a metadata pair is addressed by BOTH its members and lives on the ONE
//! schema version the request addresses — by its own `SchemaVersionId`, or by a `SchemaId`
//! plus which of that schema set's versions is meant (`smithy_query
//! operation=PutSchemaVersionMetadata,RemoveSchemaVersionMetadata,QuerySchemaVersionMetadata`).
//! A key therefore holds as many values as were put under it ("a maximum of 10 key value
//! pairs will be allowed per schema version. They can be added over one or more calls"), the
//! read reports for each key the value most recently put under it plus the ones it replaced
//! as "other metadata belonging to the same metadata key"
//! (`MetadataInfo$OtherMetadataValueList`), a pair the version already holds is a collision
//! that writes nothing, a pair it does not hold removes nothing, and the pair past the
//! documented maximum is refused rather than displacing one.
//!
//! The suites are TABLES whose rows differ along what the rule generalises over: the FORM the
//! version is addressed by, how many values one key holds, how the ten pairs are spread over
//! keys, which member of a pair the request gets wrong, what a search names, and the page
//! size a read asks for.
//!
//! Every call goes over the wire through the shared [`crate::wire`] harness.

use serde_json::{json, Value};
use service_com_amazonaws_glue_awsglue::state::{
    messages::SCHEMA_VERSION_METADATA_LIMIT_EXCEEDED, DEFAULT_REGISTRY_NAME,
    MAX_METADATA_PAIRS_PER_SCHEMA_VERSION,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::wire::{call, emulator, error_code, error_message, ok};

/// The first version every schema set here is created with: the smallest Avro record the
/// `AVRO` data format accepts.
const BASE: &str = r#"{"type":"record","name":"User","fields":[{"name":"id","type":"string"}]}"#;

/// A second definition for a set that takes one, so a suite can address a version that is NOT
/// the set's latest.
const SECOND: &str = concat!(
    r#"{"type":"record","name":"User","fields":[{"name":"id","type":"string"},"#,
    r#"{"name":"tag","type":["null","string"],"default":null}]}"#
);

/// Create the schema set `schema` holding [`BASE`], and report the create's answer.
///
/// `Compatibility` is `NONE` — "any new version added will be accepted without undergoing a
/// compatibility check" — so a suite that needs a second version is not testing compatibility
/// here.
async fn given_schema(router: &MultiServiceRouter, schema: &str) -> Value {
    ok(
        router,
        "CreateSchema",
        &json!({
            "SchemaName": schema,
            "DataFormat": "AVRO",
            "Compatibility": "NONE",
            "SchemaDefinition": BASE,
        }),
    )
    .await
}

/// Register `definition` as a further version of `schema`, and report its answer.
async fn register(router: &MultiServiceRouter, schema: &str, definition: &str) -> Value {
    ok(
        router,
        "RegisterSchemaVersion",
        &json!({
            "SchemaId": { "SchemaName": schema },
            "SchemaDefinition": definition,
        }),
    )
    .await
}

/// A string member of a response body.
fn text(case: &str, body: &Value, member: &str) -> String {
    body.get(member)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case}: {member} must be a string, got {body}"))
        .to_string()
}

/// A `LatestVersion` flag, which awsJson omits when it carries the modelled default.
fn flag(body: &Value, member: &str) -> bool {
    body.get(member).and_then(Value::as_bool).unwrap_or(false)
}

/// The `MetadataInfoMap` of a read, as a map member.
fn info_map(case: &str, body: &Value) -> serde_json::Map<String, Value> {
    body.get("MetadataInfoMap")
        .and_then(Value::as_object)
        .unwrap_or_else(|| panic!("case {case}: MetadataInfoMap must be a map, got {body}"))
        .clone()
}

/// The values one key of a read's `MetadataInfoMap` holds: the value the key reports, then its
/// `OtherMetadataValueList` in the order served.
fn values_of(case: &str, body: &Value, key: &str) -> (String, Vec<String>) {
    let map = info_map(case, body);
    let info = map
        .get(key)
        .unwrap_or_else(|| panic!("case {case}: the read must report key {key}, got {body}"));
    let current = text(case, info, "MetadataValue");
    let others = info
        .get("OtherMetadataValueList")
        .and_then(Value::as_array)
        .map(|others| {
            others
                .iter()
                .map(|other| text(case, other, "MetadataValue"))
                .collect()
        })
        .unwrap_or_default();
    (current, others)
}

/// Put the pair `key`/`value` on the version `address` names, and report the answer.
async fn put(
    router: &MultiServiceRouter,
    address: &Value,
    key: &str,
    value: &str,
) -> (u16, Value) {
    call(
        router,
        "PutSchemaVersionMetadata",
        &request(address, json!({ "MetadataKey": key, "MetadataValue": value })),
    )
    .await
}

/// The same for a remove.
async fn remove(
    router: &MultiServiceRouter,
    address: &Value,
    key: &str,
    value: &str,
) -> (u16, Value) {
    call(
        router,
        "RemoveSchemaVersionMetadata",
        &request(address, json!({ "MetadataKey": key, "MetadataValue": value })),
    )
    .await
}

/// One metadata request: the members addressing the version, plus the `MetadataKeyValue` the
/// call is about.
fn request(address: &Value, pair: Value) -> Value {
    let mut body = address.clone();
    body["MetadataKeyValue"] = pair;
    body
}

/// Read the metadata of the version `address` names, with the extra members `extra` carries
/// (a `MetadataList` search, a `MaxResults`, a `NextToken`).
async fn query(router: &MultiServiceRouter, address: &Value, extra: Value) -> Value {
    let mut body = address.clone();
    if let Some(members) = extra.as_object() {
        for (member, value) in members {
            body[member] = value.clone();
        }
    }
    ok(router, "QuerySchemaVersionMetadata", &body).await
}

// ---------------------------------------------------------------------------
// What a put stores, and how a read reports it
// ---------------------------------------------------------------------------

/// A pair put on a version is readable back off THAT version, whichever of the three forms
/// the request addressed it by — and each answer reports the same identity of the version it
/// acted on, read off the stored records rather than off the request.
#[tokio::test]
async fn a_pair_put_on_a_version_reads_back_through_every_way_of_addressing_it() {
    let router = emulator();

    // The rows differ along the FORM a version is addressed by. Each row stages its own
    // schema set, so what one row puts cannot be seen by another.
    let schema = "metadata-addressing";
    let created = given_schema(&router, schema).await;
    let version_id = text("staging", &created, "SchemaVersionId");
    let schema_arn = text("staging", &created, "SchemaArn");

    let rows: Vec<(&str, Value)> = vec![
        ("by the version's own id", json!({ "SchemaVersionId": version_id })),
        (
            "by the schema set and a version number",
            json!({
                "SchemaId": { "SchemaName": schema, "RegistryName": DEFAULT_REGISTRY_NAME },
                "SchemaVersionNumber": { "VersionNumber": 1 },
            }),
        ),
        (
            "by the schema set's ARN and the latest version",
            json!({
                "SchemaId": { "SchemaArn": schema_arn },
                "SchemaVersionNumber": { "LatestVersion": true },
            }),
        ),
    ];

    for (index, (case, address)) in rows.iter().enumerate() {
        // One key per row, so the rows' pairs do not collide on the one version.
        let key = format!("owner-{index}");
        let value = "data.platform@example.com";
        let (status, answer) = put(&router, address, &key, value).await;
        assert_eq!(
            status, 200,
            "case {case}: a put onto a held version must be served, got {answer}"
        );
        assert_eq!(
            (
                text(case, &answer, "MetadataKey"),
                text(case, &answer, "MetadataValue"),
            ),
            (key.clone(), value.to_string()),
            "case {case}: the put reports the pair it stored, got {answer}"
        );
        // The identity of the version written to is the same whichever form addressed it.
        assert_eq!(
            (
                text(case, &answer, "SchemaVersionId"),
                text(case, &answer, "SchemaArn"),
                text(case, &answer, "RegistryName"),
                text(case, &answer, "SchemaName"),
                answer.get("VersionNumber").and_then(Value::as_i64),
                flag(&answer, "LatestVersion"),
            ),
            (
                version_id.clone(),
                schema_arn.clone(),
                DEFAULT_REGISTRY_NAME.to_string(),
                schema.to_string(),
                Some(1),
                true,
            ),
            "case {case}: the answer reports the version it wrote to, got {answer}"
        );

        // Read back through a DIFFERENT form than the put used: the pair is on the version,
        // not on the way it was addressed.
        let read = query(&router, &json!({ "SchemaVersionId": version_id }), json!({})).await;
        assert_eq!(
            text(case, &read, "SchemaVersionId"),
            version_id,
            "case {case}: the read reports the version it answered for, got {read}"
        );
        assert_eq!(
            values_of(case, &read, &key),
            (value.to_string(), Vec::new()),
            "case {case}: the pair put is the one read back, got {read}"
        );
    }

    // Every row's pair is still held: a put adds a pair, it does not replace the ones there.
    let read = query(&router, &json!({ "SchemaVersionId": version_id }), json!({})).await;
    assert_eq!(
        info_map("all rows", &read).len(),
        rows.len(),
        "the version holds one key per row, got {read}"
    );
}

/// A key holds as many values as were put under it: the read reports the value most recently
/// put as the key's own and the ones it replaced as its other values, removing one value
/// leaves the rest, and a key whose last value is removed is reported no more.
#[tokio::test]
async fn a_key_reports_its_most_recent_value_and_the_ones_it_replaced() {
    let router = emulator();

    // The rows differ in HOW MANY values one key holds.
    for (case, values) in [
        ("one value", vec!["v1"]),
        ("two values", vec!["v1", "v2"]),
        ("four values", vec!["v1", "v2", "v3", "v4"]),
    ] {
        let schema = format!("metadata-values-{}", values.len());
        let created = given_schema(&router, &schema).await;
        let version_id = text(case, &created, "SchemaVersionId");
        let address = json!({ "SchemaVersionId": version_id });
        let key = "Release";

        for value in &values {
            let (status, answer) = put(&router, &address, key, value).await;
            assert_eq!(
                status, 200,
                "case {case}: putting {value} under {key} must be served, got {answer}"
            );
        }

        let read = query(&router, &address, json!({})).await;
        let (current, others) = values_of(case, &read, key);
        let expected_current = (*values.last().expect("a row puts at least one value")).to_string();
        let expected_others: Vec<String> = values[..values.len() - 1]
            .iter()
            .map(|value| (*value).to_string())
            .collect();
        assert_eq!(
            (current, others),
            (expected_current.clone(), expected_others.clone()),
            "case {case}: the key reports its most recent value and the ones it replaced, got \
             {read}"
        );

        // The same pair again is a collision, and it writes nothing.
        let (status, refused) = put(&router, &address, key, &expected_current).await;
        assert_eq!(
            (status, error_code(&refused).as_str()),
            (400, "AlreadyExistsException"),
            "case {case}: a pair the version holds is a collision, got {refused}"
        );
        let read = query(&router, &address, json!({})).await;
        assert_eq!(
            values_of(case, &read, key),
            (expected_current.clone(), expected_others.clone()),
            "case {case}: the refused put changed nothing, got {read}"
        );

        // Removing the value the key reports leaves the others, and the key then reports the
        // most recent of what is left.
        let (status, removed) = remove(&router, &address, key, &expected_current).await;
        assert_eq!(
            status, 200,
            "case {case}: removing a held pair must be served, got {removed}"
        );
        assert_eq!(
            (
                text(case, &removed, "MetadataKey"),
                text(case, &removed, "MetadataValue"),
            ),
            (key.to_string(), expected_current.clone()),
            "case {case}: the remove reports the pair it took off, got {removed}"
        );
        let read = query(&router, &address, json!({})).await;
        match expected_others.split_last() {
            Some((promoted, rest)) => assert_eq!(
                values_of(case, &read, key),
                (promoted.clone(), rest.to_vec()),
                "case {case}: the key reports what is left of it, got {read}"
            ),
            // The key held one value, so removing it leaves no key.
            None => assert!(
                !info_map(case, &read).contains_key(key),
                "case {case}: a key whose last value was removed is reported no more, got {read}"
            ),
        }
    }
}

// ---------------------------------------------------------------------------
// What a version will not take, and what a remove will not find
// ---------------------------------------------------------------------------

/// "A maximum of 10 key value pairs will be allowed per schema version": the pair past that is
/// refused and displaces nothing, however the ten already held are spread over keys.
#[tokio::test]
async fn a_version_holds_at_most_the_documented_number_of_metadata_pairs() {
    let router = emulator();
    let limit = MAX_METADATA_PAIRS_PER_SCHEMA_VERSION;

    // The rows differ in how the ten pairs are spread: one value under each of ten keys, two
    // keys sharing them, and every one of them under a single key.
    for (case, keys) in [
        ("one pair per key", limit),
        ("two keys sharing the pairs", 2),
        ("every pair under one key", 1),
    ] {
        let schema = format!("metadata-limit-{keys}");
        let created = given_schema(&router, &schema).await;
        let version_id = text(case, &created, "SchemaVersionId");
        let address = json!({ "SchemaVersionId": version_id });

        for pair in 0..limit {
            let key = format!("k{}", pair % keys);
            let value = format!("v{pair}");
            let (status, answer) = put(&router, &address, &key, &value).await;
            assert_eq!(
                status, 200,
                "case {case}: pair {pair} is within the maximum of {limit}, got {answer}"
            );
        }

        // The pair past the maximum is refused, under the operation's own limit error.
        let (status, refused) = put(&router, &address, "one-too-many", "v").await;
        assert_eq!(
            (status, error_code(&refused).as_str()),
            (400, "ResourceNumberLimitExceededException"),
            "case {case}: the pair past the maximum must be refused, got {refused}"
        );
        assert_eq!(
            error_message(&refused),
            SCHEMA_VERSION_METADATA_LIMIT_EXCEEDED,
            "case {case}: the refusal states the metadata limit, got {refused}"
        );

        // Nothing was displaced: the version still holds exactly the pairs it took.
        let read = query(&router, &address, json!({ "MaxResults": 50 })).await;
        let map = info_map(case, &read);
        assert_eq!(
            map.len(),
            keys,
            "case {case}: the version holds the keys it took, got {read}"
        );
        let held: usize = map
            .values()
            .map(|info| {
                1 + info
                    .get("OtherMetadataValueList")
                    .and_then(Value::as_array)
                    .map_or(0, Vec::len)
            })
            .sum();
        assert_eq!(
            held, limit,
            "case {case}: the version holds {limit} pairs and no more, got {read}"
        );

        // A pair freed by a remove is a pair the version will take again.
        let (status, removed) = remove(&router, &address, "k0", "v0").await;
        assert_eq!(
            status, 200,
            "case {case}: removing a held pair must be served, got {removed}"
        );
        let (status, answer) = put(&router, &address, "room-again", "v").await;
        assert_eq!(
            status, 200,
            "case {case}: a pair removed makes room for another, got {answer}"
        );
    }
}

/// A request that names no pair, or names a pair the version does not hold, is refused — and a
/// remove that is refused takes nothing off the version.
#[tokio::test]
async fn a_metadata_request_that_addresses_no_held_pair_is_refused() {
    let router = emulator();
    let schema = "metadata-refusals";
    let created = given_schema(&router, schema).await;
    let version_id = text("staging", &created, "SchemaVersionId");
    let address = json!({ "SchemaVersionId": version_id });
    ok(
        &router,
        "PutSchemaVersionMetadata",
        &request(
            &address,
            json!({ "MetadataKey": "Owner", "MetadataValue": "analytics" }),
        ),
    )
    .await;

    // A second version of the same set holds its own pairs, so a pair of one version is not a
    // pair of the other.
    let second = register(&router, schema, SECOND).await;
    let second_id = text("staging", &second, "SchemaVersionId");
    let second_address = json!({ "SchemaVersionId": second_id });

    // The rows differ in WHICH part of the address is wrong: a member of the pair, the key,
    // the value, the version the pair is on, and the version itself.
    let rows: Vec<(&str, Value, Value, &str)> = vec![
        (
            "a pair carrying no value",
            address.clone(),
            json!({ "MetadataKey": "Owner" }),
            "InvalidInputException",
        ),
        (
            "a pair carrying no key",
            address.clone(),
            json!({ "MetadataValue": "analytics" }),
            "InvalidInputException",
        ),
        (
            "a key the version does not hold",
            address.clone(),
            json!({ "MetadataKey": "Steward", "MetadataValue": "analytics" }),
            "EntityNotFoundException",
        ),
        (
            "a held key with another value",
            address.clone(),
            json!({ "MetadataKey": "Owner", "MetadataValue": "marketing" }),
            "EntityNotFoundException",
        ),
        (
            "a pair held by another version of the same schema set",
            second_address.clone(),
            json!({ "MetadataKey": "Owner", "MetadataValue": "analytics" }),
            "EntityNotFoundException",
        ),
        (
            "a version id no schema version carries",
            json!({ "SchemaVersionId": "00000000-0000-0000-0000-000000000000" }),
            json!({ "MetadataKey": "Owner", "MetadataValue": "analytics" }),
            "EntityNotFoundException",
        ),
        (
            "a request addressing no version at all",
            json!({}),
            json!({ "MetadataKey": "Owner", "MetadataValue": "analytics" }),
            "InvalidInputException",
        ),
    ];

    for (case, address, pair, expected) in &rows {
        let (status, refused) = call(
            &router,
            "RemoveSchemaVersionMetadata",
            &request(address, pair.clone()),
        )
        .await;
        assert_eq!(
            (status, error_code(&refused)),
            (400, (*expected).to_string()),
            "case {case}: expected {expected}, got {refused}"
        );
    }

    // Every refusal left the pair where it was.
    let read = query(&router, &address, json!({})).await;
    assert_eq!(
        values_of("after the refusals", &read, "Owner"),
        ("analytics".to_string(), Vec::new()),
        "the refused removes took nothing off the version, got {read}"
    );
    // What a pair miss reports is the metadata of the version it RESOLVED, by that version's
    // own id — so a request that addressed the version by the schema set and a version number
    // names the same version in its refusal as one that addressed it by id.
    for (case, address, version) in [
        ("addressed by id", address.clone(), version_id.clone()),
        (
            "addressed by the schema set and a version number",
            json!({
                "SchemaId": { "SchemaName": schema, "RegistryName": DEFAULT_REGISTRY_NAME },
                "SchemaVersionNumber": { "VersionNumber": 2 },
            }),
            second_id.clone(),
        ),
    ] {
        let (status, refused) = remove(&router, &address, "Owner", "nothing-holds-this").await;
        assert_eq!(
            (status, error_code(&refused).as_str()),
            (400, "EntityNotFoundException"),
            "case {case}: a pair the version does not hold is a miss, got {refused}"
        );
        assert!(
            error_message(&refused).contains(&version),
            "case {case}: the miss names the version it resolved ({version}), got {refused}"
        );
    }
    // And the other version still holds none.
    let read = query(&router, &second_address, json!({})).await;
    assert!(
        info_map("the other version", &read).is_empty(),
        "a put onto one version writes nothing onto another, got {read}"
    );
}

// ---------------------------------------------------------------------------
// What a read reports: per version, searched, and paged
// ---------------------------------------------------------------------------

/// A read reports the pairs of the ONE version it addresses, filtered by what its
/// `MetadataList` names, and paged in key order under the continuation token it hands back.
#[tokio::test]
async fn a_read_reports_one_versions_pairs_searched_and_paged() {
    let router = emulator();
    let schema = "metadata-reads";
    let created = given_schema(&router, schema).await;
    let first_id = text("staging", &created, "SchemaVersionId");
    let first = json!({ "SchemaVersionId": first_id });
    let second = register(&router, schema, SECOND).await;
    let second_id = text("staging", &second, "SchemaVersionId");

    // Five keys on the first version, one of them holding two values; one pair on the second.
    let staged = [
        ("alpha", "1"),
        ("beta", "2"),
        ("gamma", "3"),
        ("delta", "4"),
        ("epsilon", "5"),
        ("alpha", "6"),
    ];
    for (key, value) in staged {
        let (status, answer) = put(&router, &first, key, value).await;
        assert_eq!(
            status, 200,
            "staging {key}={value} must be served, got {answer}"
        );
    }
    put(&router, &json!({ "SchemaVersionId": second_id }), "beta", "9")
        .await;

    // Addressing the version by the set and the version number reports the same map as
    // addressing it by id: the pairs are the version's.
    let by_number = query(
        &router,
        &json!({
            "SchemaId": { "SchemaName": schema, "RegistryName": DEFAULT_REGISTRY_NAME },
            "SchemaVersionNumber": { "VersionNumber": 1 },
        }),
        json!({ "MaxResults": 50 }),
    )
    .await;
    let by_id = query(&router, &first, json!({ "MaxResults": 50 })).await;
    assert_eq!(
        info_map("by number", &by_number),
        info_map("by id", &by_id),
        "the two ways of addressing one version report the same pairs"
    );
    assert_eq!(
        values_of("the key with two values", &by_id, "alpha"),
        ("6".to_string(), vec!["1".to_string()]),
        "the key reports its most recent value, got {by_id}"
    );

    // The second version reports only its own pair.
    let other = query(&router, &json!({ "SchemaVersionId": second_id }), json!({})).await;
    let other_keys: Vec<String> = info_map("the second version", &other)
        .keys()
        .cloned()
        .collect();
    assert_eq!(
        other_keys,
        vec!["beta".to_string()],
        "the second version reports its own pairs only, got {other}"
    );
    assert_eq!(
        values_of("the second version", &other, "beta"),
        ("9".to_string(), Vec::new()),
        "the second version's own value is what it reports, got {other}"
    );

    // "Search key-value pairs for metadata": the rows differ in what a search names — a key, a
    // value, a whole pair, several entries, and a pair nothing holds.
    for (case, search, expected) in [
        ("a key", json!([{ "MetadataKey": "beta" }]), vec!["beta"]),
        ("a value", json!([{ "MetadataValue": "3" }]), vec!["gamma"]),
        (
            "a whole pair",
            json!([{ "MetadataKey": "alpha", "MetadataValue": "1" }]),
            vec!["alpha"],
        ),
        (
            "two entries",
            json!([{ "MetadataKey": "delta" }, { "MetadataValue": "5" }]),
            vec!["delta", "epsilon"],
        ),
        (
            "a pair nothing holds",
            json!([{ "MetadataKey": "alpha", "MetadataValue": "nothing" }]),
            vec![],
        ),
    ] {
        let read = query(
            &router,
            &first,
            json!({ "MetadataList": search, "MaxResults": 50 }),
        )
        .await;
        // `MetadataInfoMap` is a MAP, so what a read reports is which keys it holds rather
        // than an order; the order one page follows another in is the paging rows below.
        let mut keys: Vec<String> = info_map(case, &read).keys().cloned().collect();
        keys.sort();
        assert_eq!(
            keys,
            expected
                .iter()
                .map(|key| (*key).to_string())
                .collect::<Vec<String>>(),
            "case {case}: the search keeps the pairs that satisfy it, got {read}"
        );
    }

    // A search naming a single value of a key reports that key with that value, and not the
    // key's other values: the search is over PAIRS.
    let read = query(
        &router,
        &first,
        json!({ "MetadataList": [{ "MetadataKey": "alpha", "MetadataValue": "1" }] }),
    )
    .await;
    assert_eq!(
        values_of("one value of a key", &read, "alpha"),
        ("1".to_string(), Vec::new()),
        "the search keeps the pair it names and not the key's others, got {read}"
    );

    // Paging: the rows differ in the page size asked for. `MetadataInfoMap` is a map, so what
    // a page reports is which keys it holds rather than an order; what the pages must do
    // together is partition the whole map, every key on exactly one of them.
    let mut every_key: Vec<String> = info_map("the whole map", &by_id).keys().cloned().collect();
    every_key.sort();
    for page_size in [1_usize, 2, 5, 50] {
        let mut seen: Vec<String> = Vec::new();
        let mut token: Option<String> = None;
        let mut pages = 0;
        loop {
            let mut extra = json!({ "MaxResults": page_size });
            if let Some(token) = &token {
                extra["NextToken"] = json!(token);
            }
            let page = query(&router, &first, extra).await;
            let keys: Vec<String> = info_map("a page", &page).keys().cloned().collect();
            assert!(
                keys.len() <= page_size,
                "case page size {page_size}: a page holds at most what was asked for, got {page}"
            );
            seen.extend(keys);
            pages += 1;
            assert!(
                pages <= every_key.len() + 1,
                "case page size {page_size}: the enumeration must terminate, got {seen:?}"
            );
            match page.get("NextToken").and_then(Value::as_str) {
                Some(next) => token = Some(next.to_string()),
                None => break,
            }
        }
        assert_eq!(
            {
                seen.sort();
                seen.clone()
            },
            every_key,
            "case page size {page_size}: the pages together are the whole map, once each"
        );
    }

    // A continuation token this store did not mint for this listing names no position.
    let (status, refused) = call(
        &router,
        "QuerySchemaVersionMetadata",
        &json!({ "SchemaVersionId": first_id, "NextToken": "alpha" }),
    )
    .await;
    assert_eq!(
        (status, error_code(&refused).as_str()),
        (400, "InvalidInputException"),
        "a token this store did not mint must be refused, got {refused}"
    );
}
