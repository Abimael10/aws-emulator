// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for two rules of this service's data-plane refusal contract, each
//! asserted over a TABLE whose rows differ along what the rule generalises over.
//!
//! RULE 1 (`PutVectors`): a `PutInputVector$key` "uniquely identifies the vector in a
//! vector index", so a batch whose records do not each name a distinct key describes two
//! different vectors for one identity and the service refuses the WHOLE request with the
//! modelled `ValidationException` (HTTP 400) and ONE fixed sentence that interpolates
//! neither the repeated key nor the positions it was submitted in — and the index is left
//! exactly as it was, with not even the records that were well formed written. The rows
//! differ in how long the batch is (two records up to `PutVectorsInputList`'s modelled
//! maximum of 500), where in it the repeat sits, how many times one key repeats, how many
//! keys repeat at once, the FORM of the repeated key (both ends of `VectorKey`'s
//! `len 1..1024` and the opaque generated form), and whether the index already holds the
//! key. The served rows are the control: a batch of distinct keys — including keys that
//! differ only in case, since a key is the identity the caller gave it rather than a
//! normalized form — is accepted and every record is stored.
//!
//! RULE 2 (`QueryVectors`): a `queryVector` the bound index cannot be searched with is
//! refused with the modelled `ValidationException` (HTTP 400) carrying ONE opaque
//! top-level sentence, the same one for every defect the vector can carry and
//! interpolating nothing — not the submitted width, not the index's `dimension`, not the
//! index name — while WHICH rule was broken is the `fieldList` entry for the member the
//! vector travelled in. (That top-level sentence is what another service forwards
//! verbatim when it probes an index on a caller's behalf, so it cannot carry this
//! request's values.) The rows differ in the defect (a width under and over the index's
//! own, a width far under a wide index, a vector the metric is undefined for), in the
//! index's `dimension` and in its `distanceMetric`; a query at the index's own dimension
//! is the control.
//!
//! The segment-pairing sentence of `ListVectors` — the third member of this contract — is
//! owned by `refusal_sentence_wording.rs`.
//!
//! Every case drives real HTTP through the production front door, so what is asserted is
//! what an SDK client sees.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::vector_lifecycle::{
    call, error_code, field_list, keys_of, message_of, ok, with_index,
};

/// The wire error code every refusal in this suite carries, and the status
/// `ValidationException`'s `@httpError` declares.
const VALIDATION_EXCEPTION: &str = "ValidationException";
/// See [`VALIDATION_EXCEPTION`].
const BAD_REQUEST: u16 = 400;

/// The sentence the service answers a batch whose records do not each name a distinct
/// vector key. A LITERAL, not the handler's own constant: a suite asserting the constant
/// would agree with any wording the handler chose.
const DUPLICATE_KEYS_SENTENCE: &str = "Request must not contain duplicate keys";

/// The sentence the service answers a query whose `queryVector` the bound index cannot be
/// searched with, whichever rule the vector broke. A LITERAL, for the same reason.
const QUERY_VECTOR_SENTENCE: &str =
    "Query vector contains invalid values or is invalid for this index";

/// The bucket and index every case below addresses.
const BUCKET: &str = "batch-keys-bucket";
/// See [`BUCKET`].
const INDEX: &str = "batch-keys-index";

/// The keys the duplicate-key suite's index already holds, in the order a read reports
/// them.
const SEEDED: [&str; 2] = ["seed-0", "seed-1"];

/// `VectorKey` is `len 1..1024`
/// (`smithy_query(query="shape",shape="PutInputVector")`), so the longest key a request
/// may carry is this many characters.
const LONGEST_KEY_LENGTH: usize = 1024;

/// `PutVectorsInputList` is `len 1..500`, so this is the largest batch a request may
/// carry.
const LARGEST_BATCH: usize = 500;

/// One vector body for `key` in a `dimension`-wide index, with values that depend on
/// `nth` so no two records of a batch carry the same data.
fn record(key: &str, nth: usize, dimension: usize) -> Value {
    let data: Vec<f32> = (0..dimension)
        .map(|value| 0.25 + nth as f32 + value as f32)
        .collect();
    json!({ "key": key, "data": { "float32": data } })
}

/// A `PutVectors` body for `keys`, one record per key in the order given.
fn put_body(keys: &[String], dimension: usize) -> Value {
    let vectors: Vec<Value> = keys
        .iter()
        .enumerate()
        .map(|(nth, key)| record(key, nth, dimension))
        .collect();
    json!({
        "vectorBucketName": BUCKET,
        "indexName": INDEX,
        "vectors": vectors,
    })
}

/// Owned keys from literals, so a row can mix literal and generated keys.
fn owned(literal: &[&str]) -> Vec<String> {
    literal.iter().map(|key| (*key).to_string()).collect()
}

/// Every key the index holds, in the order `ListVectors` reports them.
async fn listed_keys(router: &MultiServiceRouter) -> Vec<String> {
    let listed = ok(
        router,
        "ListVectors",
        &json!({ "vectorBucketName": BUCKET, "indexName": INDEX }),
    )
    .await;
    keys_of(&listed)
}

/// The `float32` data the index holds under `key`, or `Value::Null` when it holds none.
async fn stored_data(router: &MultiServiceRouter, key: &str) -> Value {
    let got = ok(
        router,
        "GetVectors",
        &json!({
            "vectorBucketName": BUCKET,
            "indexName": INDEX,
            "keys": [key],
            "returnData": true,
        }),
    )
    .await;
    got["vectors"][0]["data"]["float32"].clone()
}

/// A 4-dimension index holding [`SEEDED`], the state every refused batch below must
/// leave untouched.
async fn seeded_index(dimension: usize) -> MultiServiceRouter {
    let router = with_index(
        BUCKET,
        INDEX,
        u32::try_from(dimension).expect("a dimension inside the modelled range"),
        "euclidean",
    )
    .await;
    ok(
        &router,
        "PutVectors",
        &put_body(&owned(&SEEDED), dimension),
    )
    .await;
    router
}

/// A batch that names one vector key more than once is refused whole, with the service's
/// fixed sentence, and writes nothing — not even its well-formed records, and not over
/// the vector a repeated key already names in the index.
#[tokio::test]
async fn a_batch_that_repeats_a_vector_key_is_refused_and_writes_nothing() {
    let dimension = 4_usize;
    let longest = "k".repeat(LONGEST_KEY_LENGTH);
    // A batch at `PutVectorsInputList`'s modelled maximum whose last record repeats its
    // first, so the repeat is as far from its twin as one request can put it.
    let largest: Vec<String> = (0..LARGEST_BATCH - 1)
        .map(|nth| format!("bulk-{nth}"))
        .chain(std::iter::once("bulk-0".to_string()))
        .collect();
    assert_eq!(
        largest.len(),
        LARGEST_BATCH,
        "the maximum-batch row must carry {LARGEST_BATCH} records, got {}",
        largest.len()
    );

    // (case, the batch's keys in order, the key that repeats)
    let rows: Vec<(&str, Vec<String>, String)> = vec![
        (
            "two records under one key",
            owned(&["doc-001", "doc-001"]),
            "doc-001".to_string(),
        ),
        (
            "the repeat at the end of a longer batch",
            owned(&["doc-001", "doc-002", "doc-003", "doc-001"]),
            "doc-001".to_string(),
        ),
        (
            "the repeat in the middle of a batch",
            owned(&["doc-001", "doc-002", "doc-002", "doc-003"]),
            "doc-002".to_string(),
        ),
        (
            "three records under one key",
            owned(&["doc-009", "doc-009", "doc-009"]),
            "doc-009".to_string(),
        ),
        (
            "two keys each repeated",
            owned(&["left", "right", "left", "right"]),
            "left".to_string(),
        ),
        (
            "the shortest key form repeated",
            owned(&["a", "a"]),
            "a".to_string(),
        ),
        (
            "the longest key form repeated",
            vec![longest.clone(), longest.clone()],
            longest.clone(),
        ),
        (
            "an opaque generated key repeated",
            owned(&[
                "9f41c2ab-7de0-4a11-8f3e-0b1d2c3e4f55",
                "9f41c2ab-7de0-4a11-8f3e-0b1d2c3e4f55",
            ]),
            "9f41c2ab-7de0-4a11-8f3e-0b1d2c3e4f55".to_string(),
        ),
        (
            "a key the index already holds, repeated",
            owned(&[SEEDED[0], SEEDED[0]]),
            SEEDED[0].to_string(),
        ),
        (
            "a batch at the modelled maximum with one repeat",
            largest,
            "bulk-0".to_string(),
        ),
    ];

    let router = seeded_index(dimension).await;
    let seeded_data = stored_data(&router, SEEDED[0]).await;
    for (case, keys, repeated) in rows {
        let (status, body) = call(&router, "PutVectors", &put_body(&keys, dimension)).await;
        let message = message_of(&body);
        assert_eq!(
            (status, error_code(&body).as_str(), message.as_str()),
            (BAD_REQUEST, VALIDATION_EXCEPTION, DUPLICATE_KEYS_SENTENCE),
            "case {case:?}: a repeated vector key must be the modelled 400 carrying the \
             service's fixed sentence, got {status} {body}"
        );
        // A key short enough to be a substring of ordinary English is excluded from the
        // containment check; the digit check covers every row, since no key form below
        // is both digit-free and long.
        let distinctive = repeated.len() >= 4;
        assert!(
            (!distinctive || !message.contains(repeated.as_str()))
                && !message.chars().any(|character| character.is_ascii_digit()),
            "case {case:?}: the sentence must interpolate neither the repeated key nor a \
             position, got {message:?}"
        );
        assert_eq!(
            listed_keys(&router).await,
            owned(&SEEDED),
            "case {case:?}: a refused batch must write nothing, got {:?}",
            listed_keys(&router).await
        );
        assert_eq!(
            stored_data(&router, SEEDED[0]).await,
            seeded_data,
            "case {case:?}: a refused batch must not overwrite the vector a repeated key \
             already names, got {:?}",
            stored_data(&router, SEEDED[0]).await
        );
    }
}

/// The control for the rule above: a batch whose keys are distinct is accepted and every
/// record of it is stored under the key it was submitted under — including keys that
/// differ only in case, which are two identities rather than one.
#[tokio::test]
async fn a_batch_of_distinct_vector_keys_is_accepted_and_every_record_stored() {
    let dimension = 4_usize;
    // (case, the batch's keys in order, the keys the index must then report)
    let rows: Vec<(&str, Vec<String>, Vec<String>)> = vec![
        (
            "one record alone",
            owned(&["doc-001"]),
            owned(&["doc-001", SEEDED[0], SEEDED[1]]),
        ),
        (
            "three distinct keys",
            owned(&["doc-001", "doc-002", "doc-003"]),
            owned(&["doc-001", "doc-002", "doc-003", SEEDED[0], SEEDED[1]]),
        ),
        (
            "keys differing only in case",
            owned(&["Doc", "doc", "DOC"]),
            owned(&["DOC", "Doc", "doc", SEEDED[0], SEEDED[1]]),
        ),
        (
            "a record under a key the index already holds",
            owned(&["doc-001", SEEDED[0]]),
            owned(&["doc-001", SEEDED[0], SEEDED[1]]),
        ),
    ];

    for (case, keys, expected) in rows {
        let router = seeded_index(dimension).await;
        ok(&router, "PutVectors", &put_body(&keys, dimension)).await;
        let listed = listed_keys(&router).await;
        assert_eq!(
            listed, expected,
            "case {case:?}: every record of a distinct-key batch must be stored, got \
             {listed:?}"
        );
        // And each record is stored as the vector it was submitted as, so an accepted
        // batch is not merely counted.
        for (nth, key) in keys.iter().enumerate() {
            let expected_data = record(key, nth, dimension)["data"]["float32"].clone();
            let actual = stored_data(&router, key).await;
            assert_eq!(
                actual, expected_data,
                "case {case:?}: {key} must be stored as submitted, got {actual}"
            );
        }
    }
}

/// A `queryVector` the bound index cannot be searched with is refused with ONE opaque
/// top-level sentence, whichever rule the vector broke, with the rule itself in the
/// `fieldList` entry for the member it travelled in. The sentence carries nothing from
/// the request or the index.
#[tokio::test]
async fn a_query_vector_the_index_cannot_search_reports_one_opaque_sentence() {
    // (case, the index dimension, the index metric, the submitted vector, the rule the
    // fieldList entry must state)
    let rows: Vec<(&str, usize, &str, Vec<f32>, String)> = vec![
        (
            "one value short of the index dimension",
            4,
            "euclidean",
            vec![0.5; 3],
            "vector must have length 4, but has length 3".to_string(),
        ),
        (
            "one value over the index dimension",
            4,
            "cosine",
            vec![0.5; 5],
            "vector must have length 4, but has length 5".to_string(),
        ),
        (
            "a single value against a wide index",
            16,
            "cosine",
            vec![0.5; 1],
            "vector must have length 16, but has length 1".to_string(),
        ),
        (
            "an empty vector",
            2,
            "euclidean",
            Vec::new(),
            "vector must have length 2, but has length 0".to_string(),
        ),
        (
            "a vector the metric is undefined for",
            4,
            "cosine",
            vec![0.0; 4],
            "cosine distance does not support vectors with zero norm".to_string(),
        ),
    ];

    let mut sentences: Vec<String> = Vec::new();
    let mut details: Vec<String> = Vec::new();
    for (case, dimension, metric, data, rule) in rows {
        let router = with_index(
            BUCKET,
            INDEX,
            u32::try_from(dimension).expect("a dimension inside the modelled range"),
            metric,
        )
        .await;
        ok(
            &router,
            "PutVectors",
            &put_body(&owned(&SEEDED), dimension),
        )
        .await;

        let (status, body) = call(
            &router,
            "QueryVectors",
            &json!({
                "vectorBucketName": BUCKET,
                "indexName": INDEX,
                "topK": 2,
                "queryVector": { "float32": data },
            }),
        )
        .await;
        let message = message_of(&body);
        assert_eq!(
            (status, error_code(&body).as_str(), message.as_str()),
            (BAD_REQUEST, VALIDATION_EXCEPTION, QUERY_VECTOR_SENTENCE),
            "case {case:?}: must be the modelled 400 carrying the opaque query-vector \
             sentence, got {status} {body}"
        );
        assert!(
            !message.chars().any(|character| character.is_ascii_digit())
                && !message.contains(INDEX),
            "case {case:?}: the sentence must interpolate neither a width nor the index, \
             got {message:?}"
        );
        assert_eq!(
            field_list(&body),
            vec![("queryVector".to_string(), rule.clone())],
            "case {case:?}: fieldList must name the member the vector travelled in and \
             state the rule it broke, got {body}"
        );

        // The control: at the index's own dimension the same index answers the query.
        let served = ok(
            &router,
            "QueryVectors",
            &json!({
                "vectorBucketName": BUCKET,
                "indexName": INDEX,
                "topK": 2,
                "queryVector": { "float32": vec![0.5_f32; dimension] },
            }),
        )
        .await;
        let mut answered = keys_of(&served);
        answered.sort();
        assert_eq!(
            answered,
            owned(&SEEDED),
            "case {case:?}: a query at the index's own dimension must answer normally, \
             got {served}"
        );

        sentences.push(message);
        details.push(rule);
    }

    // One sentence for every defect, and the defects themselves told apart only by the
    // field entry: that split is the rule, so it is asserted over the rows together.
    let distinct_sentences: std::collections::BTreeSet<&String> = sentences.iter().collect();
    assert_eq!(
        distinct_sentences.len(),
        1,
        "every query-vector defect must report the same top-level sentence, got \
         {sentences:?}"
    );
    let distinct_details: std::collections::BTreeSet<&String> = details.iter().collect();
    assert_eq!(
        distinct_details.len(),
        details.len(),
        "each defect must be told apart by its own field entry, got {details:?}"
    );
}
