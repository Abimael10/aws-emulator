// SPDX-License-Identifier: CC0-1.0

//! What the BRACKETED SET of a request-validation clause is a function of.
//!
//! The rule under test: a refusal of a value a modelled constraint rules out
//! states the requirement read off the MODEL and not off the one value that
//! failed, so the set a clause brackets is decided by the MEMBER alone —
//!
//! * a value outside an enumerated type is refused with that type's WHOLE value
//!   set, listed in ascending order of the values themselves, whatever order the
//!   model declares them in;
//! * a value a constraint declared on a COLLECTION's element shape rules out is
//!   refused at the collection member, with the element shape's WHOLE declared
//!   constraint set.
//!
//! Either way the clause is the same sentence for every request that breaks the
//! same member's constraint: it cannot vary with the value sent, with the
//! element's position in a collection, with how many values offended, or with
//! which operation and which depth carried the member — and it quotes none of
//! what the request carried.
//!
//! So the rows here are GENERATED rather than listed: the suites beside this one
//! pin the sentences against the spellings a recording happened to send
//! (`parameter_validation_refusals.rs` for the enumerated types,
//! `collection_element_constraint_refusals.rs` and
//! `tagging_member_refusal_layers.rs` for the list member), while this one draws
//! a bounded sample of the value space those sentences have to be INVARIANT over
//! and compares each refusal against an expectation built from the model:
//!
//! * the enumerated sets come from this crate's own declaration of the model's
//!   values (`StorageClass::ALL`, `TableBucketType::ALL`, each documented as the
//!   values of its `com.amazonaws.s3tables#` enum), sorted here rather than
//!   written out, so a model revision that reorders or extends a type needs no
//!   literal changed;
//! * the element set comes from the generated input schema of the operation that
//!   carries the collection, read through the one shared layer that states such a
//!   clause — and one assertion holds that reading to the bounds the model
//!   publishes for `TagKey`, so neither can drift alone.
//!
//! The generator's seed is printed, each test is bounded to a fixed number of
//! drawn values, and nothing is written outside the emulator's own in-memory
//! state. The accepted rows keep the refusals honest: every value each
//! enumerated type admits is served, and a well-formed removal still works, so
//! what is refused is the values outside the set and not the member.

use std::collections::BTreeSet;

use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};
use serde_json::{json, Value};
use service_com_amazonaws_s3tables_s3tablebuckets as s3tables;

use s3tables::state::{OpenTableFormat, StorageClass, TableBucketType};

use crate::harness::{listed_tags, served, tag_map, tags_body, Emulator, Reply};

/// The region every case works in.
const REGION: &str = "us-east-1";

/// The namespace the table-create site addresses.
const NAMESPACE: &str = "warehouse";

/// The code the request validator answers under: none of this model's shapes,
/// which is what tells a client the request was refused for the SHAPE of a
/// parameter rather than by a rule of the service.
const PARAMETER_VALIDATION: &str = "ValidationException";

/// The collection member of this service's inputs that carries a constraint
/// declared on its ELEMENT shape: `UntagResource.tagKeys`, whose member targets
/// `TagKey`.
const TAG_KEYS: &str = "tagKeys";

/// `TagKey`'s length bounds as the model publishes them
/// (`smithy_query(query="shape", shape="TagKeyList,TagKey")`), for the one
/// assertion that holds the sentence read off the generated schema to the
/// published numbers.
const TAG_KEY_MIN: usize = 1;
const TAG_KEY_MAX: usize = 128;

/// How many drawn values each test grades. Bounded, so the suite stays a fast
/// in-process suite rather than an open-ended search.
const CASES: usize = 8;

/// The seed the generator runs from when the environment names none, so a run is
/// reproducible by default and a failure is reproducible from the seed the test
/// prints.
const DEFAULT_SEED: u64 = 0x5331_5441_424c_4553;

/// The environment variable that replays another seed.
const SEED_VARIABLE: &str = "S3TABLES_CONSTRAINT_SET_SEED";

/// The characters a drawn value is built from: the alphabet an enumerated value
/// and a tag key of this model are spelled in, so a drawn value is one a client
/// could plausibly send rather than an escaping test.
const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789_";

/// The seed this run generates from.
fn seed() -> u64 {
    std::env::var(SEED_VARIABLE)
        .ok()
        .and_then(|named| named.parse().ok())
        .unwrap_or(DEFAULT_SEED)
}

/// A drawn word of `length` characters.
fn word(rng: &mut StdRng, length: usize) -> String {
    (0..length)
        .map(|_| char::from(ALPHABET[rng.random_range(0..ALPHABET.len())]))
        .collect()
}

/// Up to [`CASES`] spellings of a value `admitted` does not hold: words of its
/// own, an admitted value in another case, one with a suffix, a prefix of one,
/// and the empty string — anything a draw happens to spell exactly like an
/// admitted value is dropped, so every row really is outside the set.
fn outside_the_set(rng: &mut StdRng, admitted: &[&str]) -> Vec<String> {
    let mut drawn = vec![String::new()];
    while drawn.len() < CASES - 3 {
        let length = rng.random_range(1..=12);
        drawn.push(word(rng, length));
    }
    let one = admitted[rng.random_range(0..admitted.len())];
    drawn.push(one.to_ascii_lowercase());
    drawn.push(format!("{one}_TIER"));
    drawn.push(one.chars().take(3).collect());
    drawn.retain(|value| !admitted.contains(&value.as_str()));
    drawn
}

/// Where a request carries a member targeting an enumerated type, so one drawn
/// value is sent at every depth and in every operation that takes it.
#[derive(Clone, Copy)]
enum Site {
    /// `CreateTableBucket`: the configuration travels in the body of a create.
    BucketCreate,
    /// `PutTableBucketStorageClass`: the same structure, on a bucket that exists.
    StorageClassPut,
    /// `CreateTable`: the same structure again, in another operation's body.
    TableCreate,
    /// `ListTableBuckets`: an enumerated member carried as a QUERY parameter
    /// rather than inside a body.
    BucketListing,
}

impl Site {
    /// How a case names this site.
    fn label(self) -> &'static str {
        match self {
            Self::BucketCreate => "a bucket create",
            Self::StorageClassPut => "a storage-class put",
            Self::TableCreate => "a table create",
            Self::BucketListing => "a bucket listing",
        }
    }

    /// The member path a clause about this site's enumerated member states: the
    /// path of the member the model declares the constraint on, which is the same
    /// for every operation that nests the same structure.
    fn member_path(self) -> &'static str {
        match self {
            Self::BucketCreate | Self::StorageClassPut | Self::TableCreate => {
                "storageClassConfiguration.storageClass"
            }
            Self::BucketListing => "type",
        }
    }

    /// The name a refused bucket create would have given its bucket, so a later
    /// listing can show the refusal left nothing behind.
    const REFUSED_BUCKET: &'static str = "constraint-set-refused";

    /// The same for the table a refused table create named.
    const REFUSED_TABLE: &'static str = "constraint_set_refused";

    /// Send `value` at this site.
    async fn send(self, emulator: &Emulator, bucket: &str, value: &str) -> Reply {
        match self {
            Self::BucketCreate => {
                emulator
                    .create_table_bucket(
                        REGION,
                        &json!({
                            "name": Self::REFUSED_BUCKET,
                            "storageClassConfiguration": { "storageClass": value },
                        }),
                    )
                    .await
            }
            Self::StorageClassPut => {
                emulator
                    .put_table_bucket_storage_class(
                        REGION,
                        bucket,
                        &json!({ "storageClass": value }),
                    )
                    .await
            }
            Self::TableCreate => {
                emulator
                    .create_table(
                        REGION,
                        bucket,
                        NAMESPACE,
                        &json!({
                            "name": Self::REFUSED_TABLE,
                            "format": OpenTableFormat::Iceberg.as_wire(),
                            "storageClassConfiguration": { "storageClass": value },
                        }),
                    )
                    .await
            }
            Self::BucketListing => {
                emulator
                    .list_table_buckets(REGION, &[("type", value.to_string())])
                    .await
            }
        }
    }
}

/// The whole sentence a refusal of a value outside an enumerated type carries:
/// the member path and the type's value set, in the order the set is stated in.
fn expected_enum_message(member_path: &str, stated: &[String]) -> String {
    format!(
        "1 validation error detected: Value at '{member_path}' failed to satisfy constraint: \
         Member must satisfy enum value set: [{}]",
        stated.join(", ")
    )
}

/// The values a clause listed, taken out of its `enum value set: [...]` half.
fn stated_set(message: &str, case: &str) -> Vec<String> {
    let (_, tail) = message.split_once("enum value set: [").unwrap_or_else(|| {
        panic!("case {case:?}: the message states no enum value set, got {message:?}")
    });
    let (set, _) = tail.split_once(']').unwrap_or_else(|| {
        panic!("case {case:?}: the stated set is not bracketed, got {message:?}")
    });
    set.split(", ").map(str::to_string).collect()
}

/// Assert `reply` is the request validator's refusal of `member_path`, stating
/// `admitted` — the type's whole value set, ascending — and nothing of what the
/// request carried.
fn assert_enum_refusal(reply: &Reply, case: &str, sent: &str, member_path: &str, admitted: &[&str]) {
    assert_eq!(
        reply.status, 400,
        "case {case:?}: expected status 400, got {status} {body}",
        status = reply.status,
        body = reply.body
    );
    assert_eq!(
        reply.error_code.as_deref(),
        Some(PARAMETER_VALIDATION),
        "case {case:?}: expected the request validator's code, got {actual:?}",
        actual = reply.error_code
    );
    let message = reply.message();
    let stated = stated_set(&message, case);
    assert!(
        stated.windows(2).all(|pair| pair[0] < pair[1]),
        "case {case:?}: the stated set must ascend, got {stated:?}"
    );
    assert_eq!(
        stated.iter().map(String::as_str).collect::<BTreeSet<&str>>(),
        admitted.iter().copied().collect::<BTreeSet<&str>>(),
        "case {case:?}: the stated set must be the type's whole value set, got {stated:?}"
    );
    let mut ascending: Vec<String> = admitted.iter().map(|value| (*value).to_string()).collect();
    ascending.sort_unstable();
    assert_eq!(
        message,
        expected_enum_message(member_path, &ascending),
        "case {case:?}: expected the member path and the sorted set, got {message:?}"
    );
    // The clause quotes nothing of the request. A value that is itself a piece of
    // an admitted value — the prefixes among the drawn rows — would be found in
    // the stated set either way, so the whole-sentence assertion above is what
    // covers those.
    let a_piece_of_the_set = admitted.iter().any(|value| value.contains(sent));
    if !sent.is_empty() && !a_piece_of_the_set {
        assert!(
            !message.contains(sent),
            "case {case:?}: the refusal must quote no value, got {message:?}"
        );
    }
}

/// The whole sentence a refusal of a `tagKeys` element carries, read off the
/// generated input schema of the operation that carries the list: the element
/// shape's declared set, stated at the list member, inside the validator's
/// envelope.
fn element_message_from_the_model() -> String {
    let declared = service_common::validation_message::element_constraint_set(
        &[s3tables::ops::types::UNTAG_RESOURCE_INPUT_SCHEMA],
        TAG_KEYS,
    )
    .unwrap_or_else(|| {
        panic!("the model must declare a constraint set on the `{TAG_KEYS}` element shape")
    });
    let requirements: Vec<String> = declared
        .into_iter()
        .map(service_common::validation_message::requirement)
        .collect();
    let clause = format!(
        "Value at '{TAG_KEYS}' failed to satisfy constraint: {}",
        service_common::validation_message::element_set_requirement(&requirements)
    );
    service_common::validation_message::envelope_over(std::slice::from_ref(&clause))
        .unwrap_or_else(|| panic!("one clause must render an envelope"))
}

/// One drawn removal: the keys it names and which of them no `TagKey` admits.
struct Removal {
    label: String,
    keys: Vec<String>,
    offending: Vec<String>,
}

/// A drawn removal: a list of one to four keys, one or two of them outside
/// `TagKey`'s bounds — over the maximum or under the minimum — at drawn
/// positions, the rest well formed.
fn drawn_removal(rng: &mut StdRng, index: usize) -> Removal {
    let count: usize = rng.random_range(1..=4);
    let offenders = rng.random_range(1..=count.min(2));
    let mut keys: Vec<String> = Vec::new();
    for _ in 0..count {
        let length = rng.random_range(TAG_KEY_MIN..=20);
        keys.push(word(rng, length));
    }
    let mut positions: Vec<usize> = (0..count).collect();
    let mut offending = Vec::new();
    for _ in 0..offenders {
        let drawn = rng.random_range(0..positions.len());
        let position = positions.remove(drawn);
        // Either end of the bound: nothing at all, or a key past the maximum.
        let offender = if rng.random_bool(0.25) {
            String::new()
        } else {
            let length = rng.random_range(TAG_KEY_MAX + 1..=TAG_KEY_MAX * 3);
            word(rng, length)
        };
        keys[position] = offender.clone();
        offending.push(offender);
    }
    Removal {
        label: format!("drawn removal {index} of {count} keys, {offenders} outside the bound"),
        keys,
        offending,
    }
}

/// A table bucket with a namespace in it and two tags on it, so a refused
/// request has something to have left unchanged.
async fn fixture(emulator: &Emulator, name: &str) -> String {
    let arn = served(
        emulator
            .create_table_bucket(REGION, &json!({ "name": name }))
            .await,
        "CreateTableBucket",
        name,
    )
    .text("arn");
    served(
        emulator
            .create_namespace(REGION, &arn, &json!({ "namespace": [NAMESPACE] }))
            .await,
        "CreateNamespace",
        name,
    );
    served(
        emulator
            .tag_resource(REGION, &arn, &tags_body(&tag_map(&["owner", "env"])))
            .await,
        "TagResource",
        name,
    );
    arn
}

/// The storage class a read reports for `arn`.
async fn reported_storage_class(emulator: &Emulator, arn: &str, case: &str) -> String {
    let read = emulator.get_table_bucket_storage_class(REGION, arn).await;
    assert_eq!(
        read.status, 200,
        "case {case:?}: GetTableBucketStorageClass must succeed, got {status} {body}",
        status = read.status,
        body = read.body
    );
    read.body
        .get("storageClassConfiguration")
        .and_then(|configuration| configuration.get("storageClass"))
        .and_then(Value::as_str)
        .unwrap_or_else(|| {
            panic!(
                "case {case:?}: the read must carry storageClassConfiguration.storageClass: {body}",
                body = read.body
            )
        })
        .to_string()
}

#[tokio::test]
async fn an_enumerated_set_is_stated_whole_and_ascending_whatever_was_sent() {
    let seed = seed();
    // The seed, so a failing run is replayed by setting it back.
    println!("{SEED_VARIABLE}={seed}");
    let mut rng = StdRng::seed_from_u64(seed);

    let emulator = Emulator::new();
    let arn = fixture(&emulator, "constraint-set-enum").await;
    let before = reported_storage_class(&emulator, &arn, "before any refusal").await;

    // The two enumerated types a REQUEST member of this service targets, each
    // with the sites that carry it. Both are declared in an order of their own,
    // so neither reads back in declaration order.
    let storage_classes: Vec<&str> = StorageClass::ALL
        .iter()
        .map(|class| class.as_wire())
        .collect();
    let bucket_types: Vec<&str> = TableBucketType::ALL
        .iter()
        .map(|kind| kind.as_wire())
        .collect();

    for (type_name, admitted, sites) in [
        (
            "StorageClass",
            &storage_classes,
            &[Site::BucketCreate, Site::StorageClassPut, Site::TableCreate][..],
        ),
        ("TableBucketType", &bucket_types, &[Site::BucketListing][..]),
    ] {
        for value in outside_the_set(&mut rng, admitted) {
            for site in sites {
                let case = format!(
                    "seed {seed}: {type_name} {value:?} at {site}",
                    site = site.label()
                );
                let refused = site.send(&emulator, &arn, &value).await;
                assert_enum_refusal(&refused, &case, &value, site.member_path(), admitted);
            }
        }
    }

    // Every refusal above was stated in front of the service, so nothing it
    // addressed moved: the bucket's class is what it was, and neither the bucket
    // nor the table a refused create named exists.
    let after = reported_storage_class(&emulator, &arn, "after the refusals").await;
    assert_eq!(
        after, before,
        "case \"after the refusals\": the bucket's storage class must be unchanged, got {after:?}"
    );
    let listed = emulator.list_table_buckets(REGION, &[]).await;
    let names: Vec<String> = listed
        .list("tableBuckets")
        .iter()
        .filter_map(|row| row.get("name").and_then(Value::as_str))
        .map(str::to_string)
        .collect();
    assert!(
        !names.iter().any(|name| name == Site::REFUSED_BUCKET),
        "case \"after the refusals\": a refused create must have created nothing, got {names:?}"
    );
    let tables = emulator
        .list_tables(REGION, &arn, &[("namespace", NAMESPACE.to_string())])
        .await;
    assert!(
        tables
            .list("tables")
            .iter()
            .filter_map(|row| row.get("name").and_then(Value::as_str))
            .all(|name| name != Site::REFUSED_TABLE),
        "case \"after the refusals\": a refused table create must have created nothing, got {body}",
        body = tables.body
    );
}

#[tokio::test]
async fn every_value_an_enumerated_type_admits_is_served() {
    let emulator = Emulator::new();
    let arn = fixture(&emulator, "constraint-set-admitted").await;

    // The whole set each type admits, so what the refusals above reject is the
    // values outside it and not the member itself.
    for class in StorageClass::ALL {
        let case = format!("the storage class {value:?}", value = class.as_wire());
        served(
            emulator
                .put_table_bucket_storage_class(
                    REGION,
                    &arn,
                    &json!({ "storageClass": class.as_wire() }),
                )
                .await,
            "PutTableBucketStorageClass",
            &case,
        );
        let read = reported_storage_class(&emulator, &arn, &case).await;
        assert_eq!(
            read,
            class.as_wire(),
            "case {case:?}: the put must read back, got {read:?}"
        );
    }

    for kind in TableBucketType::ALL {
        let case = format!("the bucket type {value:?}", value = kind.as_wire());
        let listed = emulator
            .list_table_buckets(REGION, &[("type", kind.as_wire().to_string())])
            .await;
        assert_eq!(
            listed.status, 200,
            "case {case:?}: a listing filtered by an admitted type must be served, got {status} \
             {body}",
            status = listed.status,
            body = listed.body
        );
    }
}

#[tokio::test]
async fn an_element_constraint_states_the_whole_declared_set_at_the_list_member() {
    let seed = seed();
    println!("{SEED_VARIABLE}={seed}");
    let mut rng = StdRng::seed_from_u64(seed);

    let expected = element_message_from_the_model();
    // The sentence read off the generated schema, held to the bounds the model
    // publishes for the element shape: both bounds, the maximum first, stated as
    // a set whichever one a value missed.
    assert_eq!(
        expected,
        format!(
            "1 validation error detected: Value at '{TAG_KEYS}' failed to satisfy constraint: \
             Member must satisfy constraint: [Member must have length less than or equal to \
             {TAG_KEY_MAX}, Member must have length greater than or equal to {TAG_KEY_MIN}]"
        ),
        "case \"the model-derived sentence\": it must state the published bounds, got {expected:?}"
    );

    let emulator = Emulator::new();
    let arn = fixture(&emulator, "constraint-set-element").await;
    let resident = listed_tags(&emulator, REGION, &arn, "before any refusal").await;

    for index in 0..CASES {
        let removal = drawn_removal(&mut rng, index);
        let case = format!("seed {seed}: {label}", label = removal.label);
        let keys: Vec<&str> = removal.keys.iter().map(String::as_str).collect();
        let refused = emulator.untag_resource(REGION, &arn, &keys).await;
        assert_eq!(
            refused.status, 400,
            "case {case:?}: expected status 400, got {status} {body}",
            status = refused.status,
            body = refused.body
        );
        assert_eq!(
            refused.error_code.as_deref(),
            Some(PARAMETER_VALIDATION),
            "case {case:?}: expected the request validator's code, got {actual:?}",
            actual = refused.error_code
        );
        let message = refused.message();
        assert_eq!(
            message, expected,
            "case {case:?}: expected the element set stated at the list member, got {message:?}"
        );
        // The clause names the list the model declares the constraint on, so no
        // element position and no element shape name travels with it.
        assert!(
            !message.contains(&format!("{TAG_KEYS}.")),
            "case {case:?}: the clause must name the list alone, got {message:?}"
        );
        for offender in removal.offending.iter().filter(|key| !key.is_empty()) {
            assert!(
                !message.contains(offender.as_str()),
                "case {case:?}: the refusal must quote no value, got {message:?}"
            );
        }
    }

    // Each refused removal took nothing — not even the well-formed keys that
    // travelled with the offending element — and a well-formed removal still
    // works afterwards.
    let after_refusals = listed_tags(&emulator, REGION, &arn, "after the refusals").await;
    assert_eq!(
        after_refusals, resident,
        "case \"after the refusals\": every tag must survive, got {after_refusals:?}"
    );
    let served_removal = emulator.untag_resource(REGION, &arn, &["env"]).await;
    assert_eq!(
        served_removal.status, 204,
        "case \"a well-formed removal\": expected 204, got {status} {body}",
        status = served_removal.status,
        body = served_removal.body
    );
    let after = listed_tags(&emulator, REGION, &arn, "a well-formed removal").await;
    assert_eq!(
        after,
        tag_map(&["owner"]),
        "case \"a well-formed removal\": exactly the named key must go, got {after:?}"
    );
}
