// SPDX-License-Identifier: CC0-1.0

//! How a read that takes a list of trace ids reads that list, end to end over
//! the wire (the `Xray` harness in [`super::xray_wire`]).
//!
//! Three rules, each asserted over a table whose rows differ along what the rule
//! generalises over rather than along the one request that showed it:
//!
//! 1. A constraint the ELEMENTS of a collection member broke is refused in AWS's
//!    validation envelope as ONE clause stated at the COLLECTION member — no
//!    element position, no `member` step — quoting the whole collection the
//!    request sent and listing every requirement the element shape DECLARES,
//!    satisfied ones included, inside `Member must satisfy constraint: [ … ]`. So
//!    one list is one validation error however many of its elements broke the
//!    rule and whichever bound they broke. Rows differ in which operation and
//!    member carries the list, the element shape's bounds, WHICH bound is broken
//!    (the maximum, the minimum), how many elements break it, and where in the
//!    list they sit.
//!
//! 2. The faults a list can carry beyond what the model declares — naming no id
//!    at all, naming more ids than one call retrieves, naming an id that is not
//!    of X-Ray's form, and naming one id twice — are found by ONE reading of the
//!    list, shared by both reads that take one; WHICH of them a read refuses,
//!    and in what words, is the read's own, because X-Ray's two queries over the
//!    same ids answer differently. Each refusal is the operation's modelled
//!    client-input error (`InvalidRequestException`, 400) beside the
//!    `x-amzn-errortype` header, and every sentence but one is FIXED text that
//!    quotes nothing of the request. Rows cover both reads, each way a list can
//!    break each rule, and the accepted boundary beside it.
//!
//! 3. `BatchGetTraces` serves every id one call carries, so it hands out no
//!    pagination cursor and refuses ANY `NextToken` with the text that says the
//!    paging is unavailable — not with the wording for a token it did not issue,
//!    which would tell a client to re-derive one and page again. Rows differ in
//!    the FORM of the token sent, including one shaped exactly like a cursor.
//!
//! The expected sentences are spelled here rather than read off the service, so
//! the test pins the text instead of restating whatever the service produced.

use serde_json::{json, Value};

use service_com_amazonaws_xray_awsxray::state::is_trace_id;

use super::xray_wire::{array, root_document, trace_id, Answer, Xray};

/// Now, in the epoch seconds every timestamp on this wire travels as.
fn now() -> f64 {
    service_com_amazonaws_xray_awsxray::state::now_epoch_seconds()
}

/// The refusal message of an answer, or the empty string.
fn said(answer: &Answer) -> &str {
    answer.message().unwrap_or_default()
}

/// The ids one `BatchGetTraces`/`GetTraceGraph` call retrieves, which the
/// service publishes in the refusal of a longer list.
const MAX_IDS: usize = service_com_amazonaws_xray_awsxray::state::MAX_TRACE_IDS_PER_CALL;

/// An ARN of this service's own namespace, for the tagging read whose list of
/// tag keys is one of the element-constraint rows: it is resolved AFTER the
/// modelled constraints are checked, so the resource it names need not exist.
const GROUP_ARN: &str = "arn:aws:xray:us-east-1:000000000000:group/absent/000000000000";

// =============================================================================
// 1. A constraint an element broke is stated at the collection member
// =============================================================================

/// AWS's one-error validation envelope over a clause stated at a COLLECTION
/// member for a constraint its ELEMENTS broke: the collection as submitted, the
/// lowerCamel member path, and the element shape's whole declared `@length` set
/// — the maximum first, then the minimum.
fn element_length_clause(collection: &str, member_path: &str, max: usize, min: usize) -> String {
    format!(
        "1 validation error detected: Value '{collection}' at '{member_path}' failed to satisfy \
         constraint: Member must satisfy constraint: [Member must have length less than or equal \
         to {max}, Member must have length greater than or equal to {min}]"
    )
}

/// The list as the clause about it quotes one: the elements in the order the
/// request sent them, comma-separated, in brackets.
fn quoted(elements: &[String]) -> String {
    format!("[{}]", elements.join(", "))
}

/// A `StartTraceRetrieval` body carrying `ids`, with the window members the
/// model requires.
fn retrieval_of(ids: &[String]) -> Value {
    json!({"StartTime": now() - 3_600.0, "EndTime": now(), "TraceIds": ids})
}

#[tokio::test]
async fn a_constraint_an_element_broke_is_stated_at_the_collection_member() {
    let xray = Xray::new();

    // The element bounds come from the model, not from this file: `TraceIdList`
    // and `TraceIdListForRetrieval` both take a `TraceId` member of @length
    // 1..=35, and `TagKeyList` a `TagKey` member of @length 1..=128
    // (`smithy_query(query="shape",shape="TraceIdList,TraceIdListForRetrieval,
    // TagKeyList")`).
    const MAX_TRACE_ID: usize = 35;
    const MIN_TRACE_ID: usize = 1;
    const MAX_TAG_KEY: usize = 128;
    const MIN_TAG_KEY: usize = 1;

    let well_formed = trace_id(7);
    // One character past `TraceId`'s maximum, and far past it.
    let long_id = format!("Root={}", trace_id(11));
    let just_long = "1".repeat(MAX_TRACE_ID + 1);
    let long_key = "k".repeat(MAX_TAG_KEY + 1);

    // (case, path, body, the collection as quoted, member path, max, min)
    type Row = (&'static str, &'static str, Value, String, &'static str, usize, usize);
    let rows: Vec<Row> = vec![
        (
            "one id past TraceId's maximum, on BatchGetTraces",
            "/Traces",
            json!({"TraceIds": [&long_id]}),
            quoted(std::slice::from_ref(&long_id)),
            "traceIds",
            MAX_TRACE_ID,
            MIN_TRACE_ID,
        ),
        (
            "the same id on GetTraceGraph",
            "/TraceGraph",
            json!({"TraceIds": [&long_id]}),
            quoted(std::slice::from_ref(&long_id)),
            "traceIds",
            MAX_TRACE_ID,
            MIN_TRACE_ID,
        ),
        (
            "an offending id one character past the maximum, beside a well-formed one",
            "/Traces",
            json!({"TraceIds": [&well_formed, &just_long]}),
            quoted(&[well_formed.clone(), just_long.clone()]),
            "traceIds",
            MAX_TRACE_ID,
            MIN_TRACE_ID,
        ),
        (
            "TWO offending ids in one list, which is still ONE validation error",
            "/TraceGraph",
            json!({"TraceIds": [&long_id, &just_long]}),
            quoted(&[long_id.clone(), just_long.clone()]),
            "traceIds",
            MAX_TRACE_ID,
            MIN_TRACE_ID,
        ),
        (
            // The OTHER bound of the same set: an empty element is below the
            // minimum, and the clause still lists both bounds.
            "an empty id, which is below TraceId's minimum",
            "/Traces",
            json!({"TraceIds": [""]}),
            quoted(&[String::new()]),
            "traceIds",
            MAX_TRACE_ID,
            MIN_TRACE_ID,
        ),
        (
            "an empty id after a well-formed one",
            "/TraceGraph",
            json!({"TraceIds": [&well_formed, ""]}),
            quoted(&[well_formed.clone(), String::new()]),
            "traceIds",
            MAX_TRACE_ID,
            MIN_TRACE_ID,
        ),
        (
            // Another operation, another list shape, the same rule.
            "one id past the maximum on the retrieval's own list",
            "/StartTraceRetrieval",
            retrieval_of(std::slice::from_ref(&long_id)),
            quoted(std::slice::from_ref(&long_id)),
            "traceIds",
            MAX_TRACE_ID,
            MIN_TRACE_ID,
        ),
        (
            // A different element shape, with different bounds: the set is the
            // element shape's, read off the model rather than fixed per member.
            "one tag key past TagKey's maximum",
            "/UntagResource",
            json!({"ResourceARN": GROUP_ARN, "TagKeys": [&long_key]}),
            quoted(std::slice::from_ref(&long_key)),
            "tagKeys",
            MAX_TAG_KEY,
            MIN_TAG_KEY,
        ),
        (
            "an empty tag key, which is below TagKey's minimum",
            "/UntagResource",
            json!({"ResourceARN": GROUP_ARN, "TagKeys": ["keep", ""]}),
            quoted(&["keep".to_string(), String::new()]),
            "tagKeys",
            MAX_TAG_KEY,
            MIN_TAG_KEY,
        ),
    ];

    for (case, path, body, collection, member_path, max, min) in rows {
        let refused = xray.call(path, &body).await;
        assert_eq!(
            (refused.status, refused.error_type.as_deref()),
            (400, Some("ValidationException")),
            "case {case:?}: expected 400 ValidationException, got {refused:?}"
        );
        let expected = element_length_clause(&collection, member_path, max, min);
        assert_eq!(
            said(&refused),
            expected,
            "case {case:?}: expected {expected:?}, got {:?}",
            said(&refused)
        );
        assert!(
            !said(&refused).contains(".member"),
            "case {case:?}: the clause names the collection and no element position, got {:?}",
            said(&refused)
        );
    }
}

#[tokio::test]
async fn an_element_inside_its_constraint_is_not_refused_for_it() {
    // The boundary of the rule above: an element AT each end of the element
    // shape's declared @length is accepted, so the clauses above are about the
    // elements that broke the bound and not about the member carrying them.
    let xray = Xray::new();
    let at_maximum = trace_id(12); // exactly 35 characters
    let shortest = "1".to_string(); // exactly 1 character

    for (case, body) in [
        ("an id at TraceId's maximum", json!({"TraceIds": [&at_maximum]})),
        ("an id at TraceId's minimum", json!({"TraceIds": [&shortest]})),
    ] {
        let answer = xray.call("/Traces", &body).await;
        // The one-character id is still refused by the HANDLER for its form —
        // which is the next rule — but never by the modelled constraint, so the
        // code tells the two apart.
        assert_ne!(
            answer.error_type.as_deref(),
            Some("ValidationException"),
            "case {case:?}: an element inside its bounds breaks no modelled constraint, got \
             {answer:?}"
        );
    }

    let served = xray.call("/Traces", &json!({"TraceIds": [&at_maximum]})).await;
    assert_eq!(
        served.status, 200,
        "an id at the maximum is a well-formed id and is served, got {served:?}"
    );
}

// =============================================================================
// 2. One reading of the list, and each read's own answer to what it finds
// =============================================================================

/// The two reads that take a `TraceIds` list, as `(operation, uri)`.
const READS: [(&str, &str); 2] = [
    ("BatchGetTraces", "/Traces"),
    ("GetTraceGraph", "/TraceGraph"),
];

/// Assert one refusal: 400 `InvalidRequestException`, the `x-amzn-errortype`
/// header restJson1 resolves the error shape from, and `expected` verbatim.
fn assert_refused(case: &str, expected: &str, refused: &Answer) {
    assert_eq!(
        (refused.status, refused.error_type.as_deref()),
        (400, Some("InvalidRequestException")),
        "case {case:?}: expected 400 InvalidRequestException, got {refused:?}"
    );
    assert_eq!(
        said(refused),
        expected,
        "case {case:?}: expected {expected:?}, got {:?}",
        said(refused)
    );
}

#[tokio::test]
async fn a_list_naming_no_id_is_answered_by_the_batch_read_and_refused_by_the_graph_read() {
    // The one fault the two reads answer DIFFERENTLY: the batch read has a place
    // to report nothing found (`Traces` and `UnprocessedTraceIds`, both empty),
    // so it answers an empty list; the graph read has nowhere to report an id it
    // could not graph, so a graph of no trace would claim the traces had no
    // services and the request is refused instead.
    for (label, xray) in [
        ("an empty store", Xray::new()),
        ("a seeded store", seeded().await),
    ] {
        let answered = xray.call("/Traces", &json!({"TraceIds": []})).await;
        assert_eq!(
            answered.status, 200,
            "case {label:?}: BatchGetTraces answers an empty list, got {answered:?}"
        );
        assert!(
            array(&answered.body, "Traces").is_empty()
                && array(&answered.body, "UnprocessedTraceIds").is_empty(),
            "case {label:?}: the answer reports nothing found, got {answered:?}"
        );

        let refused = xray.call("/TraceGraph", &json!({"TraceIds": []})).await;
        assert_refused(
            &format!("GetTraceGraph over {label}"),
            "Zero trace ids provided",
            &refused,
        );
    }
}

#[tokio::test]
async fn a_list_naming_one_id_twice_is_refused_by_every_read_that_takes_one() {
    let stored = trace_id(1);
    let other = trace_id(2);
    let absent = trace_id(99);

    // Rows differ in WHERE the repeat sits, how many times the id is repeated,
    // and whether the repeated id is one the store holds — the rule is about the
    // list, so none of that moves it.
    let rows: Vec<(&str, Vec<String>)> = vec![
        ("the same stored id twice", vec![stored.clone(), stored.clone()]),
        (
            "a repeat separated by another id",
            vec![stored.clone(), other.clone(), stored.clone()],
        ),
        (
            "an id repeated three times",
            vec![stored.clone(), stored.clone(), stored.clone()],
        ),
        (
            "a repeat of an id nothing is stored under",
            vec![absent.clone(), absent.clone()],
        ),
        (
            "a repeat at the end of a list the call accepts in full",
            vec![stored.clone(), other.clone(), absent.clone(), absent.clone()],
        ),
    ];

    for (operation, path) in READS {
        let xray = seeded().await;
        for (label, ids) in &rows {
            let case = format!("{operation} with {label}");
            let refused = xray.call(path, &json!({"TraceIds": ids})).await;
            assert_refused(&case, "Containing duplicate trace ids", &refused);
        }

        // The boundary: the same ids, each named once, are served.
        let served = xray
            .call(path, &json!({"TraceIds": [&stored, &other]}))
            .await;
        assert_eq!(
            served.status, 200,
            "{operation}: distinct ids are served, got {served:?}"
        );
    }
}

#[tokio::test]
async fn a_malformed_or_oversized_list_is_refused_with_the_rules_own_sentence() {
    // One id more than a call retrieves, every one of them stored, so the row AT
    // the limit is served rather than refused for a reason of its own (the graph
    // read refuses an id it holds no trace for).
    let too_many: Vec<String> = (0..=MAX_IDS)
        .map(|index| trace_id(u32::try_from(index).expect("the index fits") + 100))
        .collect();
    let xray = seeded_with(&too_many).await;

    // One row per way a list breaks a rule of its own, over both reads: a value
    // that is not of X-Ray's id form (whatever the shape of the departure), and
    // a list longer than the ids one call retrieves.
    let malformed: Vec<(&str, Vec<String>)> = vec![
        ("an id that is no id at all", vec!["not-a-trace-id".to_string()]),
        ("a version other than 1", vec!["2-58406520-a006649127e371903a2de979".to_string()]),
        ("a non-hexadecimal digit", vec!["1-58406520-a006649127e371903a2de97z".to_string()]),
        (
            "a malformed id after a well-formed one",
            vec![too_many[0].clone(), "1".to_string()],
        ),
    ];

    for (operation, path) in READS {
        for (label, ids) in &malformed {
            let case = format!("{operation} with {label}");
            let refused = xray.call(path, &json!({"TraceIds": ids})).await;
            // The one fault the two reads WORD differently: the batch read's
            // template names the offending id, the graph read's fixed sentence
            // names the rule and the error code and nothing of the request.
            let offending = ids
                .iter()
                .find(|id| !is_trace_id(id))
                .expect("the row carries a malformed id");
            let expected = match operation {
                "BatchGetTraces" => format!("Invalid trace id {offending}: incorrect id format"),
                _ => "Invalid traceId. ErrorCode: InvalidId".to_string(),
            };
            assert_refused(&case, &expected, &refused);
        }

        let case = format!("{operation} with one id more than a call retrieves");
        let refused = xray.call(path, &json!({"TraceIds": too_many})).await;
        assert_refused(
            &case,
            &format!("Exceeding maximum query size: {MAX_IDS}"),
            &refused,
        );

        // The boundary: exactly as many ids as a call retrieves is served.
        let served = xray
            .call(path, &json!({"TraceIds": too_many[..MAX_IDS]}))
            .await;
        assert_eq!(
            served.status, 200,
            "{operation}: a list at the limit is served, got {served:?}"
        );
    }
}

// =============================================================================
// 3. BatchGetTraces serves no pagination
// =============================================================================

#[tokio::test]
async fn batch_get_traces_refuses_any_next_token() {
    let xray = seeded().await;
    let stored = trace_id(1);

    // Rows differ in the FORM of the token: a word, the empty string, a number
    // shaped exactly like the offset cursor a paging implementation would issue,
    // one past the end of the requested list, a long token, and one carrying
    // punctuation. None of them is a token this read could resume from, because
    // it issues none.
    for token in [
        "bogus",
        "",
        "0",
        "1",
        "7",
        &"x".repeat(100),
        "tok+en/with=special&chars",
    ] {
        let case = format!("BatchGetTraces with {token:?}");
        let refused = xray
            .call("/Traces", &json!({"TraceIds": [&stored], "NextToken": token}))
            .await;
        assert_refused(&case, "NextToken is currently not supported", &refused);
        assert!(
            !said(&refused).contains(token) || token.is_empty(),
            "case {case:?}: the refusal quotes nothing of the request, got {:?}",
            said(&refused)
        );
    }
}

#[tokio::test]
async fn batch_get_traces_hands_out_no_token_for_any_list_it_serves() {
    let xray = Xray::new();

    // Every id one call retrieves, each with a trace stored under it, so the
    // served answer is as full as a request can make it — and still carries no
    // cursor, because there is no page after it.
    let ids: Vec<String> = (0..MAX_IDS)
        .map(|index| trace_id(u32::try_from(index).expect("the index fits") + 1))
        .collect();
    let documents: Vec<String> = ids
        .iter()
        .enumerate()
        .map(|(index, id)| {
            root_document(
                &format!("{:016x}", index + 1),
                id,
                "svc",
                now() - 60.0,
                Some(now() - 59.0),
            )
            .to_string()
        })
        .collect();
    let put = xray.put_segments(&documents).await;
    assert_eq!(put.status, 200, "seeding must succeed, got {put:?}");

    // Rows: one id, every id, and a mix of stored and unstored ids.
    let absent = trace_id(99);
    let rows: Vec<(&str, Vec<String>)> = vec![
        ("one stored id", vec![ids[0].clone()]),
        ("every id one call retrieves", ids.clone()),
        (
            "stored ids beside one nothing is stored under",
            vec![ids[0].clone(), absent.clone()],
        ),
    ];

    for (case, requested) in rows {
        let answer = xray.batch_get(&requested, None).await;
        assert_eq!(
            answer.status, 200,
            "case {case:?}: the call must be served, got {answer:?}"
        );
        assert!(
            answer.get("NextToken").is_null(),
            "case {case:?}: the read hands out no cursor, got {:?}",
            answer.get("NextToken")
        );
        let answered: Vec<&str> = array(&answer.body, "Traces")
            .iter()
            .filter_map(|trace| trace.get("Id").and_then(Value::as_str))
            .collect();
        let expected: Vec<&str> = requested
            .iter()
            .filter(|id| *id != &absent)
            .map(String::as_str)
            .collect();
        assert_eq!(
            answered, expected,
            "case {case:?}: one call answers for every stored id it was asked for, got \
             {answered:?}"
        );
    }
}

/// One emulator holding completed traces under `trace_id(1)` and `trace_id(2)`,
/// so a row refused for its LIST is refused with a store that could have
/// answered it.
async fn seeded() -> Xray {
    seeded_with(&[trace_id(1), trace_id(2)]).await
}

/// One emulator holding one completed trace under each of `ids`.
async fn seeded_with(ids: &[String]) -> Xray {
    let xray = Xray::new();
    let documents: Vec<String> = ids
        .iter()
        .enumerate()
        .map(|(index, id)| {
            root_document(
                &format!("{:016x}", index + 1),
                id,
                "svc",
                now() - 60.0,
                Some(now() - 59.0),
            )
            .to_string()
        })
        .collect();
    let put = xray.put_segments(&documents).await;
    assert_eq!(put.status, 200, "seeding must succeed, got {put:?}");
    xray
}
