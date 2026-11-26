// SPDX-License-Identifier: CC0-1.0

//! WHICH of a request's defects this service reports when it carries several, and the
//! sentence the VECTOR surface's body reader states for a member it ran out of.
//!
//! Three rules, each driven end to end over a table of inputs rather than over one
//! example:
//!
//! 1. The required TAG LIST of an ARN-addressed tagging call is read before the ARN: a
//!    write that states no tag, and a removal that names no tag key, are refused for the
//!    list whatever the `ResourceArn` turns out to be — a value that is no ARN, an ARN of
//!    another service, an ARN of a table this endpoint does not hold, and an ARN of a table
//!    it does. A list that DOES state an entry leaves the ARN the thing the request is
//!    refused for, which is what makes the order observable.
//! 2. An absent member of the vector-search input is refused by the sentence that
//!    surface's body READER states: the member named as the model spells it on the wire,
//!    and where in the body the read of the request object stopped. Every member the
//!    model marks `@required` there is a row, each over bodies of different lengths, so
//!    the position is read off the request rather than written down. The surfaces that
//!    state the service's parameter sentence and the validating walk's clause for the
//!    same absence are the rows that tell the three readings apart.
//! 3. A secondary index an `UpdateTable` CREATES is held to having its key attributes in
//!    the `AttributeDefinitions` that request carried before the table it names is
//!    resolved: the refusal is the same for a table this endpoint holds and for one it
//!    does not, so a permanently invalid request is never answered the retryable
//!    not-found class.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::emulator::{
    call, describe_when_active, emulator, error_code, message, ok, validation_envelope, REGION,
};

/// The account every ARN this emulator mints carries.
const ACCOUNT: &str = engine::EMULATOR_ACCOUNT_ID;

/// The refusal `operation` answers `request` with: its wire code and its message, over a
/// call that must not succeed.
async fn refusal(
    router: &MultiServiceRouter,
    case: &str,
    operation: &str,
    request: Value,
) -> (String, String) {
    let (status, answered) = call(router, operation, request).await;
    assert_eq!(
        status, 400,
        "case {case:?} ({operation}): expected HTTP 400, got {status} with {answered}"
    );
    (error_code(&answered), message(&answered))
}

/// `body` without its `member`, which is how a row sends a request that states no value
/// for one member and every value for the rest.
fn without(body: &Value, member: &str) -> Value {
    let mut sent = body.clone();
    sent.as_object_mut()
        .unwrap_or_else(|| panic!("a row's body is a JSON object, got {body}"))
        .remove(member)
        .unwrap_or_else(|| panic!("a row omits a member {member} its body carries, got {body}"));
    sent
}

/// Where the reader of `sent` stops reading it: the line, and the column just past the
/// request object's closing brace, counting bytes from the start of that line.
///
/// Computed from the body the row sends — serialized exactly as the harness puts it on the
/// wire — so the assertion is independent of the service's own reading of it.
fn read_end(sent: &Value) -> (usize, usize) {
    let rendered = ::serde_json::to_vec(sent).expect("a row's body serializes");
    let end = rendered
        .iter()
        .rposition(|byte| *byte == b'}')
        .map(|brace| brace + 1)
        .unwrap_or_else(|| panic!("a row's body is a JSON object, got {sent}"));
    let line = 1 + rendered[..end].iter().filter(|byte| **byte == b'\n').count();
    let start_of_line = rendered[..end]
        .iter()
        .rposition(|byte| *byte == b'\n')
        .map_or(0, |at| at + 1);
    (line, end - start_of_line)
}

// ===========================================================================
// 1. The required tag list comes before the ARN
// ===========================================================================

/// AWS's fixed sentence for a tag WRITE that states no tag.
const NO_TAGS: &str = "Atleast one Tag needs to be provided as Input.";

/// And for a tag REMOVAL that names no tag key.
const NO_TAG_KEYS: &str = "Atleast one Tag Key needs to be provided as Input.";

/// The `ResourceArn` values a row sends, spanning every way that member can be wrong and
/// one way it can be right: the empty list is refused for the LIST in all of them.
///
/// `None` is an ARN of a table this endpoint really holds, filled in by the row from the
/// table it created; the rest are literal values no state decides.
fn addressed_resources(existing: &str) -> Vec<(&'static str, String)> {
    vec![
        (
            "an ARN whose table name is shorter than a name may be",
            format!("arn:aws:dynamodb:{REGION}:{ACCOUNT}:table/ab"),
        ),
        (
            "an ARN of a table this endpoint does not hold",
            format!("arn:aws:dynamodb:{REGION}:{ACCOUNT}:table/no-such-table"),
        ),
        (
            "an ARN of another service",
            format!("arn:aws:sqs:{REGION}:{ACCOUNT}:a-queue"),
        ),
        ("a value that is no ARN at all", "not-an-arn".to_string()),
        (
            "an ARN of a table this endpoint holds",
            format!("arn:aws:dynamodb:{REGION}:{ACCOUNT}:table/{existing}"),
        ),
    ]
}

#[tokio::test]
async fn an_empty_required_tag_list_is_refused_before_the_arn_is_resolved() {
    let router = emulator();
    let existing = "precedence-tagged-table";
    ok(
        &router,
        "CreateTable",
        json!({
            "TableName": existing,
            "KeySchema": [{"AttributeName": "pk", "KeyType": "HASH"}],
            "AttributeDefinitions": [{"AttributeName": "pk", "AttributeType": "S"}],
            "BillingMode": "PAY_PER_REQUEST",
        }),
    )
    .await;
    describe_when_active(&router, "the table a row addresses", existing).await;

    for (case, arn) in addressed_resources(existing) {
        // The write and the removal state the same rule in their own vocabulary, and each
        // states it both for a list the request carries EMPTY and for one it leaves out —
        // the same absence of the same member, so the same sentence.
        let rows: Vec<(&str, &str, Value, &str)> = vec![
            (
                "an empty tag list",
                "TagResource",
                json!({"ResourceArn": arn, "Tags": []}),
                NO_TAGS,
            ),
            (
                "no tag list at all",
                "TagResource",
                json!({"ResourceArn": arn}),
                NO_TAGS,
            ),
            (
                "an empty tag-key list",
                "UntagResource",
                json!({"ResourceArn": arn, "TagKeys": []}),
                NO_TAG_KEYS,
            ),
            (
                "no tag-key list at all",
                "UntagResource",
                json!({"ResourceArn": arn}),
                NO_TAG_KEYS,
            ),
        ];
        for (stated, operation, request, expected) in rows {
            let row = format!("{case} with {stated}");
            let (code, answered) = refusal(&router, &row, operation, request).await;
            assert_eq!(
                (code.as_str(), answered.as_str()),
                ("ValidationException", expected),
                "case {row:?} ({operation}): expected the list's own sentence, got \
                 {code}/{answered:?}"
            );
        }
    }
}

#[tokio::test]
async fn a_tag_list_that_states_an_entry_leaves_the_arn_the_defect() {
    let router = emulator();
    // The same ARNs as the rows above, each now sent with a list that DOES state an entry:
    // the request has nothing wrong with its list, so what it is refused for is the ARN —
    // which is what makes the order the two are read in observable rather than incidental.
    let rows: Vec<(&str, String)> = addressed_resources("unused-here")
        .into_iter()
        .filter(|(case, _)| *case != "an ARN of a table this endpoint holds")
        .collect();
    for (case, arn) in rows {
        for (operation, request) in [
            (
                "TagResource",
                json!({"ResourceArn": arn, "Tags": [{"Key": "team", "Value": "core"}]}),
            ),
            ("UntagResource", json!({"ResourceArn": arn, "TagKeys": ["team"]})),
        ] {
            let (code, answered) = refusal(&router, case, operation, request).await;
            assert_ne!(
                answered, NO_TAGS,
                "case {case:?} ({operation}): a list that states an entry is no empty list, got \
                 {code}"
            );
            assert_ne!(
                answered, NO_TAG_KEYS,
                "case {case:?} ({operation}): a list that states an entry is no empty list, got \
                 {code}"
            );
            assert!(
                answered.contains(&arn),
                "case {case:?} ({operation}): expected the refusal to name the ARN sent, got \
                 {code}/{answered:?}"
            );
        }
    }
}

// ===========================================================================
// 2. The vector surface's body reader words an absent member
// ===========================================================================

/// Every member the vector-search input marks `@required`, as the model spells them on the
/// wire (`smithy_query(query="operation", operation="SearchVectors")` gives `TableName`,
/// `IndexName`, `SearchVector` and `TopK` required).
const REQUIRED_SEARCH_MEMBERS: &[&str] = &["TableName", "IndexName", "SearchVector", "TopK"];

/// Whole vector-search requests of different sizes, so the position the refusal states is
/// read off the body rather than being one body's constant. The last carries a multi-byte
/// character, which a byte-counted column counts as the bytes it takes.
fn search_requests() -> Vec<(&'static str, Value)> {
    vec![
        (
            "the shortest well-formed request",
            json!({
                "TableName": "vec",
                "IndexName": "ix1",
                "SearchVector": [{"N": "0.5"}],
                "TopK": 1,
            }),
        ),
        (
            "a longer request",
            json!({
                "TableName": "a-much-longer-table-name-for-vectors",
                "IndexName": "similarity-index",
                "SearchVector": [{"N": "0.5"}, {"N": "0.25"}, {"N": "0.125"}],
                "TopK": 10,
            }),
        ),
        (
            "a request carrying a multi-byte character",
            json!({
                "TableName": "vec",
                "IndexName": "ix1",
                "SearchVector": [{"N": "0.5"}],
                "TopK": 1,
                "ProjectionExpression": "naïve",
            }),
        ),
    ]
}

#[tokio::test]
async fn an_absent_vector_search_member_is_the_body_readers_own_sentence() {
    let router = emulator();
    for (shape, whole) in search_requests() {
        for member in REQUIRED_SEARCH_MEMBERS {
            let sent = without(&whole, member);
            let (line, column) = read_end(&sent);
            let case = format!("{shape} without {member}");
            let (code, answered) = refusal(&router, &case, "SearchVectors", sent).await;
            assert_eq!(
                (code.as_str(), answered.as_str()),
                (
                    "ValidationException",
                    format!("missing field `{member}` at line {line} column {column}").as_str()
                ),
                "case {case:?}: expected the reader's own sentence naming the member and \
                 where the read stopped, got {code}/{answered:?}"
            );
        }
    }
}

#[tokio::test]
async fn the_other_surfaces_keep_their_own_reading_of_the_same_absence() {
    let router = emulator();
    // The three readings of one absence, side by side: the body reader's sentence, this
    // service's parameter-required sentence, and the validating walk's clause.
    let rows: Vec<(&str, &str, Value, String)> = vec![
        (
            "the vector search",
            "SearchVectors",
            json!({"IndexName": "ix1", "SearchVector": [{"N": "0.5"}], "TopK": 1}),
            String::new(),
        ),
        (
            "an item read",
            "GetItem",
            json!({"Key": {"pk": {"S": "one"}}}),
            "The parameter 'TableName' is required but was not present in the request"
                .to_string(),
        ),
        (
            "a table description",
            "DescribeTable",
            json!({}),
            validation_envelope(&[crate::emulator::absent_clause("tableName")]),
        ),
    ];
    for (case, operation, request, expected) in rows {
        let sent = request.clone();
        let (code, answered) = refusal(&router, case, operation, request).await;
        assert_eq!(
            code, "ValidationException",
            "case {case:?} ({operation}): expected a validation refusal, got {code}/{answered:?}"
        );
        let expected = if expected.is_empty() {
            let (line, column) = read_end(&sent);
            format!("missing field `TableName` at line {line} column {column}")
        } else {
            expected
        };
        assert_eq!(
            answered, expected,
            "case {case:?} ({operation}): expected this surface's own reading of the absence, \
             got {answered:?}"
        );
    }
}

// ===========================================================================
// 3. An index a create declares is read before the table is resolved
// ===========================================================================

/// The `UpdateTable` body that creates `index` on `table`, keyed on `attribute`, with
/// `definitions` as the whole `AttributeDefinitions` the request carries.
fn index_create(table: &str, index: &str, attribute: &str, definitions: Option<Value>) -> Value {
    let mut body = json!({
        "TableName": table,
        "GlobalSecondaryIndexUpdates": [{
            "Create": {
                "IndexName": index,
                "KeySchema": [{"AttributeName": attribute, "KeyType": "HASH"}],
                "Projection": {"ProjectionType": "ALL"},
            }
        }],
    });
    if let Some(declared) = definitions {
        body["AttributeDefinitions"] = declared;
    }
    body
}

#[tokio::test]
async fn an_undeclared_index_key_is_refused_whether_or_not_the_table_exists() {
    let router = emulator();
    let existing = "index-precedence-table";
    ok(
        &router,
        "CreateTable",
        json!({
            "TableName": existing,
            "KeySchema": [{"AttributeName": "pk", "KeyType": "HASH"}],
            "AttributeDefinitions": [{"AttributeName": "pk", "AttributeType": "S"}],
            "BillingMode": "PAY_PER_REQUEST",
        }),
    )
    .await;
    describe_when_active(&router, "the table an index update addresses", existing).await;
    let absent = "index-precedence-absent-table";

    // The ways a request can fail to declare the key of the index it creates: no
    // `AttributeDefinitions` member at all, an empty list, and a list that declares other
    // attributes than the index's key.
    let rows: Vec<(&str, &str, Option<Value>)> = vec![
        ("no attribute definitions at all", "gsi-none", None),
        (
            "an empty attribute-definition list",
            "gsi-empty",
            Some(json!([])),
        ),
        (
            "definitions that declare another attribute",
            "gsi-other",
            Some(json!([{"AttributeName": "other", "AttributeType": "S"}])),
        ),
    ];
    for (case, index, definitions) in rows {
        let mut answers = Vec::new();
        for (where_, table) in [("an absent table", absent), ("a table that exists", existing)] {
            let row = format!("{case} on {where_}");
            let (code, answered) = refusal(
                &router,
                &row,
                "UpdateTable",
                index_create(table, index, "vec", definitions.clone()),
            )
            .await;
            assert_eq!(
                code, "ValidationException",
                "case {row:?}: an index key no definition declares is a defect of the REQUEST, \
                 got {code}/{answered:?}"
            );
            answers.push((row, answered));
        }
        // The one rule: the same request shape earns the same refusal whichever table it
        // names, because nothing the store holds decides it.
        assert_eq!(
            answers[0].1, answers[1].1,
            "case {case:?}: expected one refusal whether or not the table exists, got \
             {answers:?}"
        );
    }
}

#[tokio::test]
async fn a_request_that_declares_the_index_key_is_answered_by_the_table_lookup() {
    let router = emulator();
    // The control: the SAME call with the index's key attribute declared has nothing wrong
    // with its shape, so the table it names is what it is refused for — the not-found
    // class a client retries after creating that table.
    let absent = "index-lookup-absent-table";
    let (code, answered) = refusal(
        &router,
        "a declared index key on an absent table",
        "UpdateTable",
        index_create(
            absent,
            "gsi-declared",
            "vec",
            Some(json!([{"AttributeName": "vec", "AttributeType": "S"}])),
        ),
    )
    .await;
    assert_eq!(
        (code.as_str(), answered.as_str()),
        (
            "ResourceNotFoundException",
            format!("Requested resource not found: Table: {absent} not found").as_str()
        ),
        "case a declared index key on an absent table: expected the table lookup's refusal, \
         got {code}/{answered:?}"
    );
}

#[tokio::test]
async fn an_undeclared_index_key_leaves_the_table_it_named_untouched() {
    let router = emulator();
    let table = "index-precedence-unchanged";
    ok(
        &router,
        "CreateTable",
        json!({
            "TableName": table,
            "KeySchema": [{"AttributeName": "pk", "KeyType": "HASH"}],
            "AttributeDefinitions": [{"AttributeName": "pk", "AttributeType": "S"}],
            "BillingMode": "PAY_PER_REQUEST",
        }),
    )
    .await;
    let before = describe_when_active(&router, "the table before the refused update", table).await;
    let (code, answered) = refusal(
        &router,
        "a refused index create",
        "UpdateTable",
        index_create(table, "gsi-refused", "vec", None),
    )
    .await;
    assert_eq!(
        code, "ValidationException",
        "case a refused index create: expected a validation refusal, got {code}/{answered:?}"
    );
    let after = describe_when_active(&router, "the table after the refused update", table).await;
    assert_eq!(
        after["GlobalSecondaryIndexes"], before["GlobalSecondaryIndexes"],
        "case a refused index create: a refused update declares no index, got {after}"
    );
    assert_eq!(
        after["AttributeDefinitions"], before["AttributeDefinitions"],
        "case a refused index create: a refused update declares no attribute, got {after}"
    );
}
