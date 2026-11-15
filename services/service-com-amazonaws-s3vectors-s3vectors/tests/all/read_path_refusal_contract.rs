// SPDX-License-Identifier: CC0-1.0

//! The refusal contract of the operations that READ an index — `ListVectors` and
//! `QueryVectors` — and the two rules of it that hold service-wide.
//!
//! Five rules, each asserted over a TABLE whose rows differ along what it generalises
//! over:
//!
//! 1. A `@length` violation states the LENGTH the check counted in front of the member
//!    (`Value with length <n> at '<json pointer>' failed to satisfy constraint: …`). The
//!    rows differ in the member, the operation, the shape kind the length was counted on
//!    (a string's characters, a list's elements), which side of the bound was broken, and
//!    whether the characters are single- or multi-byte; the published bounds themselves
//!    are accepted.
//! 2. A resource name outside the form its kind of resource is named by is refused on NAME
//!    grounds — 400 `ValidationException` with that kind's fixed sentence — BEFORE the
//!    lookup that would report the resource absent, on every operation that addresses a
//!    resource by name. The rows differ in the operation, the kind of name, and how the
//!    name is ill-formed; a well-formed name for a resource that does not exist is still
//!    the 404.
//! 3. `QueryVectors` without a `queryVector` reports the operation's OWN sentence rather
//!    than the framework's generic missing-member clause. The rows differ in the other
//!    members the request carries; another absent required member keeps the framework's
//!    clause.
//! 4. A metadata `filter` the service will not evaluate is refused with ONE fixed opaque
//!    sentence, in the top-level `message` and in the `fieldList` entry alike. The rows
//!    walk the filter grammar — an unsupported operator, operands of the wrong kind, a
//!    filter that is not a filter object, a non-filterable key — and a filter the service
//!    does evaluate is served. A document past this emulator's OWN evaluation ceiling is
//!    refused by a sentence naming that ceiling instead, since the ceiling is not a rule
//!    the service states.
//! 5. The three LAYERS that refuse spell a `fieldList` path three ways: a modelled
//!    constraint names its member by a JSON pointer, a cross-field rule of `ListVectors`
//!    names the service's own snake_case field, and a query vector's own defect names the
//!    field the vector reached the validation layer as.
//!
//! Every case drives real HTTP through the production front door, so what is asserted is
//! what an SDK client sees. Sentences and paths are LITERALS here rather than the
//! handlers' own constants: a suite asserting the constant would agree with any wording
//! the handler chose.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::vector_lifecycle::{
    call, call_reporting_errortype, error_code, field_list, message_of, ok, with_index,
};

/// The wire error code of an input refusal, and the status `ValidationException`'s
/// `@httpError` declares.
const VALIDATION_EXCEPTION: &str = "ValidationException";
/// See [`VALIDATION_EXCEPTION`].
const BAD_REQUEST: u16 = 400;
/// The wire error code of a refusal about an absent resource, and its status.
const NOT_FOUND_EXCEPTION: &str = "NotFoundException";
/// See [`NOT_FOUND_EXCEPTION`].
const NOT_FOUND: u16 = 404;

/// The bucket and index every case addresses, and the index's dimension.
const BUCKET: &str = "read-path-bucket";
/// See [`BUCKET`].
const INDEX: &str = "read-path-index";
/// See [`BUCKET`]: the dimension the index is created with, so a query vector of this
/// width is the one the index can be searched with.
const DIMENSION: u32 = 4;

/// The fixed sentence for a name outside the form a vector bucket is named by.
const INVALID_BUCKET_NAME: &str = "Invalid vector bucket name";
/// The fixed sentence for a name outside the form a vector index is named by.
const INVALID_INDEX_NAME: &str = "Invalid index name";
/// The fixed sentence a query that carries no `queryVector` is refused with.
const QUERY_VECTOR_REQUIRED: &str = "Query must provide a queryVector";
/// The fixed sentence every metadata-filter defect is refused with.
const INVALID_FILTER: &str = "Invalid filter";
/// The opaque sentence a query vector the index cannot be searched with is refused with.
const QUERY_VECTOR_SENTENCE: &str =
    "Query vector contains invalid values or is invalid for this index";
/// The fixed sentence a `segmentIndex` not below its `segmentCount` is refused with.
const SEGMENT_SENTENCE: &str = "Segment index is zero-indexed and must be smaller than segment count";

/// The index this suite's cases run against, holding two vectors with metadata.
async fn seeded() -> MultiServiceRouter {
    let router = with_index(BUCKET, INDEX, DIMENSION, "cosine").await;
    ok(
        &router,
        "PutVectors",
        &json!({
            "vectorBucketName": BUCKET,
            "indexName": INDEX,
            "vectors": [
                { "key": "a", "data": { "float32": [1.0, 0.0, 0.0, 0.0] },
                  "metadata": { "color": "red", "size": 2 } },
                { "key": "b", "data": { "float32": [0.0, 1.0, 0.0, 0.0] },
                  "metadata": { "color": "blue", "size": 7 } },
            ],
        }),
    )
    .await;
    router
}

/// A query vector of the index's own width, so a case about another member is not also
/// refused for its vector.
fn query_vector() -> Value {
    json!({ "float32": vec![0.5_f32; DIMENSION as usize] })
}

/// The clause this service states for a `@length` violation at `pointer`: the length the
/// check counted, then the member and the requirement.
fn length_clause(pointer: &str, length: usize, requirement: &str) -> String {
    format!("Value with length {length} at '{pointer}' failed to satisfy constraint: {requirement}")
}

// =============================================================================
// 1. A `@length` clause states the length that was counted
// =============================================================================

/// One row: the request, the member the clause names by its JSON pointer, the length it
/// states, and the requirement the member had to satisfy.
struct Length {
    case: &'static str,
    operation: &'static str,
    request: Value,
    pointer: &'static str,
    length: usize,
    requirement: &'static str,
}

#[tokio::test]
async fn a_length_violation_states_the_length_that_was_counted() {
    let router = seeded().await;

    // The bounds come from the members' own modelled constraints
    // (`smithy_query(query="operation",operation="ListVectors,QueryVectors,PutVectors")`):
    // `IndexName` and `VectorBucketName` are `len 3..63`, `ListVectorsNextToken`
    // `len 1..2048`, and `PutVectorsInputList` `len 1..500`.
    let names = "Member must have length between 3 and 63, inclusive";
    let token = "Member must have length between 1 and 2048, inclusive";
    let batch = "Member must have length between 1 and 500, inclusive";

    // A name of 129 single-byte characters, one of 64, one of 70 MULTI-BYTE characters
    // (140 bytes, so a length counted in bytes would state 140), and one two characters
    // long.
    let long_name = "a".repeat(129);
    let just_over = "b".repeat(64);
    let multi_byte = "\u{f6}".repeat(70);
    let short_name = "ab";
    let long_token = "t".repeat(2049);

    let rows: Vec<Length> = vec![
        Length {
            case: "an index name far over its maximum",
            operation: "ListVectors",
            request: json!({ "vectorBucketName": BUCKET, "indexName": long_name }),
            pointer: "/indexName",
            length: 129,
            requirement: names,
        },
        Length {
            case: "an index name one character over its maximum",
            operation: "ListVectors",
            request: json!({ "vectorBucketName": BUCKET, "indexName": just_over }),
            pointer: "/indexName",
            length: 64,
            requirement: names,
        },
        Length {
            case: "an index name of multi-byte characters, counted in characters",
            operation: "QueryVectors",
            request: json!({
                "vectorBucketName": BUCKET,
                "indexName": multi_byte,
                "topK": 1,
                "queryVector": query_vector(),
            }),
            pointer: "/indexName",
            length: 70,
            requirement: names,
        },
        Length {
            case: "an index name under its minimum",
            operation: "QueryVectors",
            request: json!({
                "vectorBucketName": BUCKET,
                "indexName": short_name,
                "topK": 1,
                "queryVector": query_vector(),
            }),
            pointer: "/indexName",
            length: 2,
            requirement: names,
        },
        Length {
            case: "a bucket name over its maximum",
            operation: "ListVectors",
            request: json!({ "vectorBucketName": "c".repeat(100), "indexName": INDEX }),
            pointer: "/vectorBucketName",
            length: 100,
            requirement: names,
        },
        Length {
            case: "a pagination token over its maximum",
            operation: "ListVectors",
            request: json!({
                "vectorBucketName": BUCKET,
                "indexName": INDEX,
                "nextToken": long_token,
            }),
            pointer: "/nextToken",
            length: 2049,
            requirement: token,
        },
        Length {
            case: "an empty pagination token, under its minimum",
            operation: "ListVectors",
            request: json!({ "vectorBucketName": BUCKET, "indexName": INDEX, "nextToken": "" }),
            pointer: "/nextToken",
            length: 0,
            requirement: token,
        },
        Length {
            // `@length` on a LIST counts its ELEMENTS, so the clause states how many
            // records the batch carried rather than any string's size.
            case: "an empty batch, whose length is counted in records",
            operation: "PutVectors",
            request: json!({ "vectorBucketName": BUCKET, "indexName": INDEX, "vectors": [] }),
            pointer: "/vectors",
            length: 0,
            requirement: batch,
        },
    ];

    for row in rows {
        let (status, errortype, body) = call_reporting_errortype(
            &router,
            "POST",
            &format!("/{}", row.operation),
            Some(&row.request),
        )
        .await;
        let expected = length_clause(row.pointer, row.length, row.requirement);
        assert_eq!(
            (status, error_code(&body).as_str(), errortype.as_deref()),
            (BAD_REQUEST, VALIDATION_EXCEPTION, Some(VALIDATION_EXCEPTION)),
            "case {:?}: must be the modelled 400, got {status} {body}",
            row.case
        );
        assert_eq!(
            (message_of(&body), field_list(&body)),
            (
                format!("1 validation error detected. {expected}"),
                vec![(row.pointer.to_string(), expected.clone())]
            ),
            "case {:?}: both halves of the body must state the counted length, got {body}",
            row.case
        );
    }
}

/// The published bounds themselves are accepted, so what the clauses above report is a
/// value PAST the bound rather than the bound.
#[tokio::test]
async fn a_value_at_a_published_length_bound_is_served() {
    let router = seeded().await;
    // (case, the index name to address, whether the index exists under it)
    let rows: [(&str, String); 2] = [
        ("the shortest accepted index name", "abc".to_string()),
        ("the longest accepted index name", format!("i{}", "0".repeat(62))),
    ];
    for (case, name) in rows {
        let (status, body) = call(
            &router,
            "ListVectors",
            &json!({ "vectorBucketName": BUCKET, "indexName": name }),
        )
        .await;
        // The name is inside every bound and well formed, so the only thing left to
        // refuse it for is the index not existing.
        assert_eq!(
            (status, error_code(&body).as_str()),
            (NOT_FOUND, NOT_FOUND_EXCEPTION),
            "case {case:?}: a name at the published bound must reach the lookup, \
             got {status} {body}"
        );
    }
}

// =============================================================================
// 2. An ill-formed name is refused before the lookup
// =============================================================================

/// Names within every modelled bound but outside the form the ARN patterns encode for a
/// bucket or an index segment (`[a-z0-9][a-z0-9-.]{1,61}[a-z0-9]`), and why each is.
fn ill_formed() -> [(&'static str, &'static str); 5] {
    [
        ("upper case", "Read-Path-Name"),
        ("an underscore", "read_path_name"),
        ("a leading hyphen", "-readpathname"),
        ("a trailing dot", "readpathname."),
        ("a space", "read path name"),
    ]
}

/// A request addressing the index `index` in bucket `bucket` through `operation`, with
/// every other member the model requires.
fn addressing(operation: &str, bucket: &str, index: &str) -> Value {
    let mut request = json!({ "vectorBucketName": bucket, "indexName": index });
    let members = match operation {
        "QueryVectors" => json!({ "topK": 1, "queryVector": query_vector() }),
        "GetVectors" | "DeleteVectors" => json!({ "keys": ["a"] }),
        "PutVectors" => json!({
            "vectors": [{ "key": "a", "data": query_vector() }],
        }),
        _ => json!({}),
    };
    for (member, value) in members.as_object().expect("an object of members") {
        request[member] = value.clone();
    }
    request
}

/// A request addressing the bucket `bucket` alone through `operation`.
fn addressing_bucket(operation: &str, bucket: &str) -> Value {
    let mut request = json!({ "vectorBucketName": bucket });
    if operation == "PutVectorBucketDefaultIndexMode" {
        request["defaultIndexMode"] = json!("CLASSIC");
    }
    request
}

/// Every operation that addresses a resource BY NAME refuses a name outside that kind's
/// form with the kind's own fixed sentence, before any lookup: the sentence does not
/// depend on whether a resource of that name could have existed, and it echoes no part of
/// the rejected name.
#[tokio::test]
async fn an_ill_formed_name_is_refused_on_name_grounds_by_every_addressing_operation() {
    let router = seeded().await;

    // The index-addressing operations, each naming the index by `indexName`.
    let index_operations = [
        "ListVectors",
        "QueryVectors",
        "GetVectors",
        "PutVectors",
        "DeleteVectors",
        "GetIndex",
        "DeleteIndex",
    ];
    for operation in index_operations {
        for (why, name) in ill_formed() {
            let request = addressing(operation, BUCKET, name);
            let (status, errortype, body) =
                call_reporting_errortype(&router, "POST", &format!("/{operation}"), Some(&request))
                    .await;
            let case = format!("{operation} naming an index with {why}");
            assert_eq!(
                (
                    status,
                    error_code(&body).as_str(),
                    errortype.as_deref(),
                    message_of(&body).as_str()
                ),
                (
                    BAD_REQUEST,
                    VALIDATION_EXCEPTION,
                    Some(VALIDATION_EXCEPTION),
                    INVALID_INDEX_NAME
                ),
                "case {case:?}: an inadmissible index name must be the name refusal, \
                 got {status} {body}"
            );
            assert_eq!(
                field_list(&body),
                vec![("indexName".to_string(), INVALID_INDEX_NAME.to_string())],
                "case {case:?}: fieldList must name the member the name travelled in, \
                 got {body}"
            );
            assert!(
                !message_of(&body).contains(name),
                "case {case:?}: the sentence must echo no part of the rejected name, got {body}"
            );
        }
    }

    // The bucket-addressing operations, each naming the bucket by `vectorBucketName`.
    let bucket_operations = [
        "ListIndexes",
        "GetVectorBucket",
        "DeleteVectorBucket",
        "GetVectorBucketPolicy",
        "DeleteVectorBucketPolicy",
        "PutVectorBucketDefaultIndexMode",
    ];
    for operation in bucket_operations {
        for (why, name) in ill_formed() {
            let request = addressing_bucket(operation, name);
            let (status, body) = call(&router, operation, &request).await;
            let case = format!("{operation} naming a bucket with {why}");
            assert_eq!(
                (
                    status,
                    error_code(&body).as_str(),
                    message_of(&body).as_str()
                ),
                (BAD_REQUEST, VALIDATION_EXCEPTION, INVALID_BUCKET_NAME),
                "case {case:?}: an inadmissible bucket name must be the name refusal, \
                 got {status} {body}"
            );
            assert_eq!(
                field_list(&body),
                vec![(
                    "vectorBucketName".to_string(),
                    INVALID_BUCKET_NAME.to_string()
                )],
                "case {case:?}: fieldList must name the member the name travelled in, \
                 got {body}"
            );
        }
    }

    // A request that gets BOTH names wrong is told about the bucket, which is the
    // resource the index is named within.
    let (status, body) = call(
        &router,
        "ListVectors",
        &json!({ "vectorBucketName": "Bad_Bucket", "indexName": "Bad_Index" }),
    )
    .await;
    assert_eq!(
        (
            status,
            error_code(&body).as_str(),
            message_of(&body).as_str()
        ),
        (BAD_REQUEST, VALIDATION_EXCEPTION, INVALID_BUCKET_NAME),
        "two inadmissible names must report the bucket's, got {status} {body}"
    );
}

/// The CONTROL for the rule above: a name that IS admissible and names nothing is still
/// the 404 about the absent resource, so the name check has not swallowed the lookup.
#[tokio::test]
async fn a_well_formed_name_for_an_absent_resource_is_still_not_found() {
    let router = seeded().await;
    // (case, operation, request, the resource the refusal is about)
    let rows: Vec<(&str, &str, Value)> = vec![
        (
            "ListVectors on an index that does not exist",
            "ListVectors",
            addressing("ListVectors", BUCKET, "absent-index"),
        ),
        (
            "QueryVectors on an index that does not exist",
            "QueryVectors",
            addressing("QueryVectors", BUCKET, "absent-index"),
        ),
        (
            "GetVectors on an index that does not exist",
            "GetVectors",
            addressing("GetVectors", BUCKET, "absent-index"),
        ),
        (
            "ListVectors in a bucket that does not exist",
            "ListVectors",
            addressing("ListVectors", "absent-bucket", INDEX),
        ),
        (
            "ListIndexes in a bucket that does not exist",
            "ListIndexes",
            addressing_bucket("ListIndexes", "absent-bucket"),
        ),
        (
            "GetVectorBucket on a bucket that does not exist",
            "GetVectorBucket",
            addressing_bucket("GetVectorBucket", "absent-bucket"),
        ),
    ];
    for (case, operation, request) in rows {
        let (status, body) = call(&router, operation, &request).await;
        assert_eq!(
            (status, error_code(&body).as_str()),
            (NOT_FOUND, NOT_FOUND_EXCEPTION),
            "case {case:?}: an admissible name must reach the lookup, got {status} {body}"
        );
    }
}

// =============================================================================
// 3. A query with no query vector
// =============================================================================

/// A `QueryVectors` request that reaches the service without a `queryVector` is refused
/// with the operation's own fixed sentence — naming no member path, so there is no
/// `fieldList` half of it — whatever else the request carries.
#[tokio::test]
async fn a_query_without_a_query_vector_reports_the_operations_own_sentence() {
    let router = seeded().await;
    // (case, the request, which carries every member except `queryVector`)
    let rows: Vec<(&str, Value)> = vec![
        (
            "the smallest request the operation takes",
            json!({ "vectorBucketName": BUCKET, "indexName": INDEX, "topK": 1 }),
        ),
        (
            "a request asking for distances and metadata",
            json!({
                "vectorBucketName": BUCKET,
                "indexName": INDEX,
                "topK": 5,
                "returnDistance": true,
                "returnMetadata": true,
            }),
        ),
        (
            "a request carrying a metadata filter",
            json!({
                "vectorBucketName": BUCKET,
                "indexName": INDEX,
                "topK": 2,
                "filter": { "color": "red" },
            }),
        ),
        (
            "a request addressing the index by ARN",
            json!({
                "indexArn": crate::vector_lifecycle::expected_index_arn(BUCKET, INDEX),
                "topK": 1,
            }),
        ),
    ];

    for (case, request) in rows {
        let (status, errortype, body) =
            call_reporting_errortype(&router, "POST", "/QueryVectors", Some(&request)).await;
        assert_eq!(
            (
                status,
                error_code(&body).as_str(),
                errortype.as_deref(),
                message_of(&body).as_str()
            ),
            (
                BAD_REQUEST,
                VALIDATION_EXCEPTION,
                Some(VALIDATION_EXCEPTION),
                QUERY_VECTOR_REQUIRED
            ),
            "case {case:?}: must carry the operation's own sentence, got {status} {body}"
        );
        assert_eq!(
            field_list(&body),
            Vec::new(),
            "case {case:?}: a sentence naming no member has no fieldList, got {body}"
        );
    }

    // The CONTROL: another absent required member of the same operation keeps the
    // framework's generic clause, so the sentence above belongs to `queryVector` rather
    // than to the operation's whole input.
    let (status, body) = call(
        &router,
        "QueryVectors",
        &json!({
            "vectorBucketName": BUCKET,
            "indexName": INDEX,
            "queryVector": query_vector(),
        }),
    )
    .await;
    assert_eq!(
        (status, message_of(&body)),
        (
            BAD_REQUEST,
            "1 validation error detected. Value at '/topK' failed to satisfy constraint: \
             Member must not be null"
                .to_string()
        ),
        "an absent topK must keep the framework's clause, got {status} {body}"
    );
}

// =============================================================================
// 4. Every metadata-filter defect reports one fixed sentence
// =============================================================================

/// A `filter` nested `levels` deep: each level wraps the one below in a `$and`.
fn nested_filter(levels: u32) -> Value {
    let mut filter = json!({ "color": "red" });
    for _ in 0..levels {
        filter = json!({ "$and": [filter] });
    }
    filter
}

/// Every defect a metadata `filter` can carry collapses to ONE fixed opaque sentence, in
/// the top-level `message` and in the `fieldList` entry alike, which discloses nothing
/// about which clause failed.
#[tokio::test]
async fn every_metadata_filter_defect_reports_one_fixed_sentence() {
    let router = seeded().await;
    // The index that declares a metadata key non-filterable, so a filter on that key is
    // one more defect of the same kind.
    ok(
        &router,
        "CreateIndex",
        &json!({
            "vectorBucketName": BUCKET,
            "indexName": "read-path-guarded",
            "dataType": "float32",
            "dimension": DIMENSION,
            "distanceMetric": "cosine",
            "metadataConfiguration": { "nonFilterableMetadataKeys": ["note"] },
        }),
    )
    .await;

    // (case, the index to query, the filter)
    let rows: Vec<(&str, &str, Value)> = vec![
        (
            "an operator the service does not implement",
            INDEX,
            json!({ "$like": { "color": "re%" } }),
        ),
        (
            "an unimplemented operator inside a key's clause",
            INDEX,
            json!({ "color": { "$regex": "^re" } }),
        ),
        (
            "an operand that is not the list its operator takes",
            INDEX,
            json!({ "color": { "$in": "red" } }),
        ),
        (
            "an operand that is not the boolean its operator takes",
            INDEX,
            json!({ "color": { "$exists": "yes" } }),
        ),
        (
            "a logical operator whose operand is not an array of filters",
            INDEX,
            json!({ "$and": { "color": "red" } }),
        ),
        (
            "a filter that is not a filter object at all",
            INDEX,
            json!("color = red"),
        ),
        (
            "a filter on a metadata key the index declares non-filterable",
            "read-path-guarded",
            json!({ "note": "kept" }),
        ),
    ];

    for (case, index, filter) in rows {
        let (status, errortype, body) = call_reporting_errortype(
            &router,
            "POST",
            "/QueryVectors",
            Some(&json!({
                "vectorBucketName": BUCKET,
                "indexName": index,
                "topK": 2,
                "queryVector": query_vector(),
                "filter": filter,
            })),
        )
        .await;
        let message = message_of(&body);
        assert_eq!(
            (
                status,
                error_code(&body).as_str(),
                errortype.as_deref(),
                message.as_str()
            ),
            (
                BAD_REQUEST,
                VALIDATION_EXCEPTION,
                Some(VALIDATION_EXCEPTION),
                INVALID_FILTER
            ),
            "case {case:?}: must be the modelled 400 carrying the one filter sentence, \
             got {status} {body}"
        );
        assert_eq!(
            field_list(&body),
            vec![("filter".to_string(), INVALID_FILTER.to_string())],
            "case {case:?}: the field entry must carry the same sentence under the member \
             the filter travelled in, got {body}"
        );
        assert!(
            !message.contains('$') && !message.contains("note"),
            "case {case:?}: the sentence must disclose no part of the filter, got {message:?}"
        );
    }

    // The CONTROL: a filter the service does evaluate selects the matching vectors.
    for (case, filter, expected) in [
        ("an equality clause", json!({ "color": "red" }), 1),
        ("a list clause", json!({ "color": { "$in": ["red", "blue"] } }), 2),
        (
            "a nested logical clause",
            json!({ "$and": [{ "color": "red" }, { "size": { "$lte": 5 } }] }),
            1,
        ),
        ("a clause nothing matches", json!({ "color": "green" }), 0),
    ] {
        let answered = ok(
            &router,
            "QueryVectors",
            &json!({
                "vectorBucketName": BUCKET,
                "indexName": INDEX,
                "topK": 10,
                "queryVector": query_vector(),
                "filter": filter,
            }),
        )
        .await;
        assert_eq!(
            answered["vectors"].as_array().map(Vec::len),
            Some(expected),
            "case {case:?}: a filter the service evaluates must select {expected} vectors, \
             got {answered}"
        );
    }

    // And the one refusal that is NOT a filter defect: a document past this emulator's own
    // evaluation ceiling is refused by a sentence naming that ceiling, so a caller can tell
    // "this emulator will not evaluate your filter" from "your filter is not valid".
    let (status, body) = call(
        &router,
        "QueryVectors",
        &json!({
            "vectorBucketName": BUCKET,
            "indexName": INDEX,
            "topK": 2,
            "queryVector": query_vector(),
            "filter": nested_filter(40),
        }),
    )
    .await;
    let message = message_of(&body);
    assert_eq!(
        (status, error_code(&body).as_str()),
        (BAD_REQUEST, VALIDATION_EXCEPTION),
        "a filter past the evaluation ceiling must be the modelled 400, got {status} {body}"
    );
    assert_ne!(
        message, INVALID_FILTER,
        "the evaluation ceiling is this emulator's own, so it must not be stated as the \
         service's filter-defect sentence, got {message:?}"
    );
    assert!(
        message.contains("evaluates"),
        "the refusal must name the ceiling this service evaluates within, got {message:?}"
    );
}

// =============================================================================
// 5. Each refusing layer spells its own `fieldList` path
// =============================================================================

/// The three layers that refuse an input spell a `fieldList` path three ways, and each
/// spelling is the one the layer that refused uses: the boundary names a modelled
/// member by a JSON pointer, a cross-field rule names the service's own field, and a
/// query vector's own defect names the field the vector reached that layer as.
#[tokio::test]
async fn each_refusing_layer_spells_its_own_field_path() {
    let router = seeded().await;
    // (case, the request, the top-level sentence, the fieldList entry)
    let rows: Vec<(&str, Value, String, (String, String))> = vec![
        (
            "a modelled constraint, named by its JSON pointer",
            json!({ "vectorBucketName": BUCKET, "indexName": INDEX, "segmentCount": 17 }),
            "1 validation error detected. Value at '/segmentCount' failed to satisfy \
             constraint: Member must be between 1 and 16, inclusive"
                .to_string(),
            (
                "/segmentCount".to_string(),
                "Value at '/segmentCount' failed to satisfy constraint: Member must be \
                 between 1 and 16, inclusive"
                    .to_string(),
            ),
        ),
        (
            "a cross-field rule, named by the service's own field",
            json!({
                "vectorBucketName": BUCKET,
                "indexName": INDEX,
                "segmentCount": 2,
                "segmentIndex": 2,
            }),
            SEGMENT_SENTENCE.to_string(),
            (
                "segment_index".to_string(),
                SEGMENT_SENTENCE.to_string(),
            ),
        ),
    ];

    for (case, request, message, field) in rows {
        let (status, body) = call(&router, "ListVectors", &request).await;
        assert_eq!(
            (status, error_code(&body).as_str(), message_of(&body)),
            (BAD_REQUEST, VALIDATION_EXCEPTION, message),
            "case {case:?}: must be the modelled 400 with its own sentence, got {status} {body}"
        );
        assert_eq!(
            field_list(&body),
            vec![field],
            "case {case:?}: the path must be the refusing layer's own spelling, got {body}"
        );
    }

    // The query vector's own defect: the opaque sentence at the top level, the detail
    // under the field the vector reached the validation layer as, prefixed as that layer
    // prefixes a finding about one input.
    for (case, width) in [
        ("a query vector one value short", DIMENSION as usize - 1),
        ("a query vector one value over", DIMENSION as usize + 1),
        ("an empty query vector", 0),
    ] {
        let (status, body) = call(
            &router,
            "QueryVectors",
            &json!({
                "vectorBucketName": BUCKET,
                "indexName": INDEX,
                "topK": 2,
                "queryVector": { "float32": vec![0.5_f32; width] },
            }),
        )
        .await;
        assert_eq!(
            (status, error_code(&body).as_str(), message_of(&body)),
            (
                BAD_REQUEST,
                VALIDATION_EXCEPTION,
                QUERY_VECTOR_SENTENCE.to_string()
            ),
            "case {case:?}: the top-level sentence must stay opaque, got {status} {body}"
        );
        assert_eq!(
            field_list(&body),
            vec![(
                "vector".to_string(),
                format!("Invalid input: vector must have length {DIMENSION}, but has length {width}")
            )],
            "case {case:?}: the field entry must name the service's field and carry the \
             prefixed detail, got {body}"
        );
    }
}
