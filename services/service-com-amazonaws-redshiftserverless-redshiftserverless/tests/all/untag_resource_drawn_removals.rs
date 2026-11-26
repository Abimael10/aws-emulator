// SPDX-License-Identifier: CC0-1.0

//! `UntagResource` over tag sets and key lists drawn by a GENERATOR rather than written
//! out by hand, on the `awsJson1_1` wire.
//!
//! THE RULE: the tags a resource carries are state, and an untagging call removes exactly
//! the keys it names from the set STORED for the resource its `resourceArn` resolves to,
//! keeping every key it does not name — so the set a later `ListTagsForResource` reports
//! is the stored set MINUS the named keys, a key the resource does not carry is nothing to
//! remove, and removing the same keys again leaves the same set.
//!
//! `resource_tags.rs` pins that rule over a hand-written table. This suite is its PROPERTY
//! form: what it checks is an INVARIANT of a removal (the remaining set is a set
//! difference, and the removal is idempotent), so the inputs the rule generalises over —
//! how many tags the resource carries, each key's and value's form and length, how many of
//! the held keys one call names, how many keys it names that are not held, whether it
//! repeats a key, and the order the keys arrive in — are DRAWN instead of enumerated. One
//! row of a table can be satisfied by a branch keyed on that row's shape; a drawn removal
//! cannot.
//!
//! The draws stay inside the model's own bounds
//! (`smithy_query(query="shape", shape="Tag,TagKeyList")`): `Tag$key` is 1..128
//! characters, `Tag$value` is 0..256, and `TagKeyList` holds up to 200 keys, so the drawn
//! lengths and counts below are all admitted shapes rather than invented ones. The
//! published bounds themselves are a row of the table in `resource_tags.rs`.
//!
//! The case count is bounded and each population is drawn from a FIXED seed that every
//! assertion message carries, so the suite draws the same populations on every run, a
//! failure names the draw that produced it and re-running the suite reproduces it;
//! `REDSHIFT_SERVERLESS_DRAWN_UNTAG_SEED` points the draw at one seed of the caller's
//! choosing for an exploratory run. Nothing is written outside the engine's own store.
//!
//! Every call goes through `MultiServiceRouter::dispatch`, so the modelled constraints and
//! the `awsJson1_1` envelope take part rather than being bypassed. The harness (the
//! emulator and the signed call) is the one `namespace_workgroup_lifecycle.rs` owns.

use std::collections::{BTreeMap, BTreeSet};

use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::namespace_workgroup_lifecycle::{call, emulator};

/// The seeds the populations are drawn from — one population per seed, so the case count is
/// bounded and every run draws the SAME six populations: a failure is reproduced by
/// re-running the suite, and the seed it names locates the population that produced it.
const SEEDS: [u64; 6] = [
    0x5ed_0000_0001,
    0x5ed_0000_0002,
    0x5ed_0000_0003,
    0x5ed_0000_0005,
    0x5ed_0000_0008,
    0x5ed_0000_000d,
];

/// The environment variable an exploratory run names ONE seed in, in decimal.
const SEED_OVERRIDE: &str = "REDSHIFT_SERVERLESS_DRAWN_UNTAG_SEED";

/// How many resources one drawn population tags and then untags. Each one is a record of
/// its own in the same store, so a removal that reached the wrong record cannot pass.
const DRAWS_PER_SEED: usize = 4;

/// The most tags one drawn resource is created with. Far under `TagList`'s 200, because
/// what varies here is the SHAPE of a removal and not the size of the set — the set's
/// published ceiling is `resource_tags.rs`'s own case.
const MAX_DRAWN_TAGS: usize = 6;

/// The most keys one drawn removal names that the resource does not carry.
const MAX_ABSENT_KEYS: usize = 2;

/// The characters a drawn tag key or value is built from: ASCII letters, digits and the
/// separators AWS tag keys are commonly written with.
const ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789-._:/+=@";

/// The seeds this run draws its populations from: the fixed set, or the single seed
/// [`SEED_OVERRIDE`] names.
fn seeds() -> Vec<u64> {
    match std::env::var(SEED_OVERRIDE) {
        Ok(named) => vec![named
            .parse::<u64>()
            .unwrap_or_else(|error| panic!("{SEED_OVERRIDE}={named:?} is not a u64 seed: {error}"))],
        Err(_) => SEEDS.to_vec(),
    }
}

/// A drawn string out of [`ALPHABET`], of a drawn length inside `lengths`.
fn drawn_text(rng: &mut StdRng, lengths: std::ops::RangeInclusive<usize>) -> String {
    let length = rng.random_range(lengths);
    (0..length)
        .map(|_| char::from(ALPHABET[rng.random_range(0..ALPHABET.len())]))
        .collect()
}

/// A drawn tag key: 1..=24 characters, inside `Tag$key`'s published 1..128.
fn drawn_key(rng: &mut StdRng) -> String {
    drawn_text(rng, 1..=24)
}

/// A drawn tag value: 0..=32 characters, inside `Tag$value`'s published 0..256 — including
/// the empty value, which the member admits.
fn drawn_value(rng: &mut StdRng) -> String {
    drawn_text(rng, 0..=32)
}

/// A drawn tag set: between none and [`MAX_DRAWN_TAGS`] tags with DISTINCT keys, each key
/// 1..=24 characters and each value 0..=32 — inside the model's 1..128 / 0..256, and
/// including the empty value, which `Tag$value` admits.
fn drawn_tags(rng: &mut StdRng) -> Vec<(String, String)> {
    let count = rng.random_range(0..=MAX_DRAWN_TAGS);
    let mut keys: BTreeSet<String> = BTreeSet::new();
    // A key that collides with one already drawn is simply redrawn; the bound keeps the
    // draw finite whatever the generator answers.
    for _ in 0..(count * 8) {
        if keys.len() == count {
            break;
        }
        keys.insert(drawn_key(rng));
    }
    keys.into_iter()
        .map(|key| {
            let value = drawn_value(rng);
            (key, value)
        })
        .collect()
}

/// The `tagKeys` one drawn removal names: a drawn subset of the keys the resource carries,
/// plus up to [`MAX_ABSENT_KEYS`] keys it does not carry, with a drawn key sometimes
/// repeated, shuffled — so the invariant is about the SET of keys named and not about
/// their order or multiplicity.
fn drawn_removal(rng: &mut StdRng, held: &[(String, String)]) -> Vec<String> {
    let mut keys: Vec<String> = held
        .iter()
        .filter(|_| rng.random_ratio(1, 2))
        .map(|(key, _)| key.clone())
        .collect();
    for _ in 0..rng.random_range(0..=MAX_ABSENT_KEYS) {
        let absent = drawn_key(rng);
        if !held.iter().any(|(key, _)| *key == absent) {
            keys.push(absent);
        }
    }
    if !keys.is_empty() && rng.random_ratio(1, 3) {
        let repeated = keys[rng.random_range(0..keys.len())].clone();
        keys.push(repeated);
    }
    for position in (1..keys.len()).rev() {
        keys.swap(position, rng.random_range(0..=position));
    }
    keys
}

/// `tags` as the modelled `TagList` a create carries.
fn tag_list(tags: &[(String, String)]) -> Value {
    Value::Array(
        tags.iter()
            .map(|(key, value)| json!({ "key": key, "value": value }))
            .collect(),
    )
}

/// The tags `ListTagsForResource` reports for `arn`, as a sorted key/value table.
async fn listed_tags(
    router: &MultiServiceRouter,
    arn: &str,
    case: &str,
) -> Vec<(String, String)> {
    let (status, body) = call(router, "ListTagsForResource", json!({ "resourceArn": arn })).await;
    assert_eq!(
        status, 200,
        "case {case}: ListTagsForResource on {arn} expected 200, got {status} with {body}"
    );
    let reported = body["tags"].as_array().cloned().unwrap_or_else(|| {
        panic!("case {case}: ListTagsForResource must report a tags list, got {body}")
    });
    let mut table: Vec<(String, String)> = reported
        .iter()
        .map(|tag| {
            (
                tag["key"].as_str().unwrap_or_default().to_string(),
                tag["value"].as_str().unwrap_or_default().to_string(),
            )
        })
        .collect();
    table.sort();
    table
}

/// `UntagResource` for `arn` with `keys`, asserting it was SERVED (not refused and not the
/// scaffold's 501).
async fn untag(router: &MultiServiceRouter, arn: &str, keys: &[String], case: &str) {
    let (status, body) = call(
        router,
        "UntagResource",
        json!({ "resourceArn": arn, "tagKeys": Value::Array(keys.iter().map(|key| json!(key)).collect()) }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case}: UntagResource on {arn} with {keys:?} expected 200, got {status} with {body}"
    );
}

/// The ARN member of a created resource, asserting the create answered one.
fn arn_of(created: &Value, member: &str, case: &str) -> String {
    created[member]
        .as_str()
        .unwrap_or_else(|| panic!("case {case}: the create must answer {member}, got {created}"))
        .to_string()
}

/// The set the stored tags become once `removed` is applied: every held key the removal
/// does not name, with its value, sorted.
fn expected_remainder(held: &[(String, String)], removed: &[String]) -> Vec<(String, String)> {
    let named: BTreeSet<&str> = removed.iter().map(String::as_str).collect();
    let mut remaining: Vec<(String, String)> = held
        .iter()
        .filter(|(key, _)| !named.contains(key.as_str()))
        .cloned()
        .collect();
    remaining.sort();
    remaining
}

/// THE PROPERTY: over each drawn population, every removal is served, the set a later read
/// reports is exactly the stored set minus the keys the call named, a second identical
/// removal leaves that same set, and every OTHER resource in the store keeps its own set
/// whole.
#[tokio::test]
async fn a_drawn_removal_leaves_the_stored_set_minus_the_keys_it_names() {
    for seed in seeds() {
        let case_seed = format!("seed {seed:#x}");
        let mut rng = StdRng::seed_from_u64(seed);
        let router = emulator();

        // What every resource of this population must report once its removal has run, so
        // a removal that reached more than one record cannot pass.
        let mut expected: BTreeMap<String, Vec<(String, String)>> = BTreeMap::new();

        for draw in 0..DRAWS_PER_SEED {
            let held = drawn_tags(&mut rng);
            let removed = drawn_removal(&mut rng, &held);
            let remainder = expected_remainder(&held, &removed);
            // Alternating the resource KIND, because the rule is stated over the record
            // the ARN resolves to and not over one kind of resource.
            let kind = if draw % 2 == 0 { "namespace" } else { "workgroup" };
            let case = format!(
                "{case_seed} draw {draw} ({kind}, held {held:?}, removing {removed:?})"
            );

            let arn = if kind == "namespace" {
                let name = format!("untag-drawn-ns-{seed:x}-{draw}");
                let (status, body) = call(
                    &router,
                    "CreateNamespace",
                    json!({ "namespaceName": name, "tags": tag_list(&held) }),
                )
                .await;
                assert_eq!(
                    status, 200,
                    "case {case}: CreateNamespace {name} expected 200, got {status} with {body}"
                );
                arn_of(&body["namespace"], "namespaceArn", &case)
            } else {
                // A namespace of its own, carrying no tags, so it is not one of the
                // resources under test: a namespace holds one workgroup.
                let host = format!("untag-host-{seed:x}-{draw}");
                let (status, body) =
                    call(&router, "CreateNamespace", json!({ "namespaceName": host })).await;
                assert_eq!(
                    status, 200,
                    "case {case}: CreateNamespace {host} expected 200, got {status} with {body}"
                );
                let name = format!("untag-drawn-wg-{seed:x}-{draw}");
                let (status, body) = call(
                    &router,
                    "CreateWorkgroup",
                    json!({
                        "workgroupName": name,
                        "namespaceName": host,
                        "tags": tag_list(&held),
                    }),
                )
                .await;
                assert_eq!(
                    status, 200,
                    "case {case}: CreateWorkgroup {name} expected 200, got {status} with {body}"
                );
                arn_of(&body["workgroup"], "workgroupArn", &case)
            };

            untag(&router, &arn, &removed, &case).await;
            let reported = listed_tags(&router, &arn, &case).await;
            assert_eq!(
                reported, remainder,
                "case {case}: expected the stored set minus the named keys {remainder:?}, got \
                 {reported:?}"
            );

            // Idempotence: the same keys again are nothing left to remove.
            untag(&router, &arn, &removed, &case).await;
            let again = listed_tags(&router, &arn, &case).await;
            assert_eq!(
                again, remainder,
                "case {case}: removing the same keys again must leave {remainder:?}, got {again:?}"
            );

            expected.insert(arn, remainder);
        }

        // Every resource of the population still reports its OWN remainder once all the
        // removals have run.
        for (arn, remainder) in &expected {
            let case = format!("{case_seed} after every removal ({arn})");
            let reported = listed_tags(&router, arn, &case).await;
            assert_eq!(
                reported, *remainder,
                "case {case}: expected {remainder:?}, got {reported:?}"
            );
        }
    }
}
