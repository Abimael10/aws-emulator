// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for how a request's INDEX DECLARATIONS are refused.
//!
//! The rules pinned here:
//!
//! 1. an index name belongs to one index of a table — "The name must be unique among all
//!    other indexes on this table" (`GlobalSecondaryIndex$IndexName`, and
//!    `LocalSecondaryIndex$IndexName` word for word) — so a request that states the same
//!    name for two indexes names no two indexes and is refused with a `ValidationException`
//!    at HTTP 400 whose message is the service's parameter-value clause followed by
//!    `Duplicate index name: <name>`: the repeated NAME and nothing of which member, which
//!    index family or which position carried it. The same sentence answers a `CreateTable`
//!    declaration and a restore's index overrides, because the fact is the same in both; an
//!    `UpdateTable` list that names one index twice asks for two CHANGES to one resource
//!    rather than declaring two indexes, and keeps that member's own refusal;
//! 2. each entry of an index-update list asks for exactly ONE action — an entry "represents
//!    one of the following" (the `GlobalSecondaryIndexUpdate` doc) — so an entry carrying
//!    none of its action members is refused over the member paths one of them has to arrive
//!    in (`One of <Shape>.<Member>, … must not be null`), and one carrying several over the
//!    same paths, for both of the shapes that model such a choice
//!    (`GlobalSecondaryIndexUpdate`'s three actions and `VectorIndexUpdate`'s two, in the
//!    order the model declares them: `smithy_query(query="shape",
//!    shape="GlobalSecondaryIndexUpdate,VectorIndexUpdate")`);
//! 3. a refused declaration writes nothing: the table the request addressed keeps exactly
//!    the indexes it had.
//!
//! Every test drives the operations over the wire (`POST /` with
//! `X-Amz-Target: DynamoDB_20120810.<OperationName>`) through the same
//! `MultiServiceRouter` the emulator's front doors use, and each rule is asserted over a
//! SET of inputs — which index family repeats the name, where in the list the repeat sits,
//! which actions an entry states and how many — rather than one example.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::emulator::{call, describe_when_active, emulator, error_code, message, ok};

/// The clause every refusal of a parameter value in this service opens with, which the
/// sentences below carry.
const INVALID_PARAMETER: &str = "One or more parameter values were invalid";

/// The refusal of a declaration that states `name` for a second index.
fn duplicate_index_name(name: &str) -> String {
    format!("{INVALID_PARAMETER}: Duplicate index name: {name}")
}

/// The refusal of an index-update entry that states NO action, over the member paths one
/// of them has to arrive in.
fn no_action_stated(shape: &str, actions: &[&str]) -> String {
    format!("{INVALID_PARAMETER}: One of {} must not be null", paths(shape, actions))
}

/// The refusal of an entry that states SEVERAL of them, over the same paths.
fn several_actions_stated(shape: &str, actions: &[&str]) -> String {
    format!("{INVALID_PARAMETER}: Only one of {} may be specified", paths(shape, actions))
}

/// `<Shape>.<Member>` for each action member, in the order given.
fn paths(shape: &str, actions: &[&str]) -> String {
    actions
        .iter()
        .map(|action| format!("{shape}.{action}"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// One global secondary index over `key`, with no throughput of its own — which is what an
/// index of an on-demand table carries.
fn global_index(name: &str, key: &str) -> Value {
    json!({
        "IndexName": name,
        "KeySchema": [{"AttributeName": key, "KeyType": "HASH"}],
        "Projection": {"ProjectionType": "ALL"},
    })
}

/// One local secondary index: the table's own partition key, and `sort` as its sort key.
fn local_index(name: &str, sort: &str) -> Value {
    json!({
        "IndexName": name,
        "KeySchema": [
            {"AttributeName": "pk", "KeyType": "HASH"},
            {"AttributeName": sort, "KeyType": "RANGE"},
        ],
        "Projection": {"ProjectionType": "KEYS_ONLY"},
    })
}

/// One on-demand `CreateTable` body over `pk`/`sk`, declaring exactly the attributes its
/// key schema and its index key schemas name — "Some AttributeDefinitions are not used in
/// any KeySchema" is its own refusal, and these rows are about index NAMES.
fn create_body(name: &str, global: Value, local: Value) -> Value {
    let mut body = json!({
        "TableName": name,
        "BillingMode": "PAY_PER_REQUEST",
        "KeySchema": [
            {"AttributeName": "pk", "KeyType": "HASH"},
            {"AttributeName": "sk", "KeyType": "RANGE"},
        ],
        "AttributeDefinitions": declared_attributes(&[&global, &local]),
    });
    if !global.is_null() {
        body["GlobalSecondaryIndexes"] = global;
    }
    if !local.is_null() {
        body["LocalSecondaryIndexes"] = local;
    }
    body
}

/// The `AttributeDefinitions` such a body carries: the table's own two keys, plus every
/// attribute the index key schemas in `lists` name, each as a string attribute.
fn declared_attributes(lists: &[&Value]) -> Value {
    let mut names = vec!["pk".to_string(), "sk".to_string()];
    for list in lists {
        for index in list.as_array().cloned().unwrap_or_default() {
            for element in index["KeySchema"].as_array().cloned().unwrap_or_default() {
                let name = element["AttributeName"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string();
                if !names.contains(&name) {
                    names.push(name);
                }
            }
        }
    }
    Value::Array(
        names
            .into_iter()
            .map(|name| json!({"AttributeName": name, "AttributeType": "S"}))
            .collect(),
    )
}

/// The `IndexName`s one index member of a description reports, in name order.
fn index_names(description: &Value, member: &str) -> Vec<String> {
    let mut names: Vec<String> = description[member]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|index| index["IndexName"].as_str().unwrap_or_default().to_string())
        .collect();
    names.sort();
    names
}

/// Drive `operation` with `body` and answer the (status, error code, message) triple a
/// refusal is read as.
async fn refusal(
    router: &MultiServiceRouter,
    operation: &str,
    body: Value,
) -> (u16, String, String) {
    let (status, parsed) = call(router, operation, body).await;
    (status, error_code(&parsed), message(&parsed))
}

/// Poll `DescribeBackup` until the backup reports `AVAILABLE`, which is the state a
/// restore reads one in — the waiter loop a client runs after a `CreateBackup`. The wait
/// is for the CONDITION, bounded generously so a loaded machine is not a defect, and a
/// backup that never settles panics naming the last description it gave.
async fn backup_when_available(router: &MultiServiceRouter, arn: &str) {
    let started = std::time::Instant::now();
    let mut last = Value::Null;
    while started.elapsed() < std::time::Duration::from_secs(30) {
        let described = ok(router, "DescribeBackup", json!({"BackupArn": arn})).await;
        if described["BackupDescription"]["BackupDetails"]["BackupStatus"] == json!("AVAILABLE") {
            return;
        }
        last = described;
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    panic!("the backup never became AVAILABLE, last described as {last}");
}

/// RULE 1 over a `CreateTable` declaration: rows differ in which index family repeats the
/// name, whether the two entries are in one list or two, where in the list the repeat
/// sits, and what the repeated name is spelt like.
#[tokio::test]
async fn a_create_table_that_declares_one_index_name_twice_is_refused_over_that_name() {
    let router = emulator();
    let cases: [(&str, Value, &str); 5] = [
        (
            "two global secondary indexes with one name",
            create_body(
                "DupGlobalPair",
                json!([global_index("dup.index", "tag"), global_index("dup.index", "note")]),
                Value::Null,
            ),
            "dup.index",
        ),
        (
            "two local secondary indexes with one name",
            create_body(
                "DupLocalPair",
                Value::Null,
                json!([local_index("lsi-dup", "tag"), local_index("lsi-dup", "note")]),
            ),
            "lsi-dup",
        ),
        (
            "a local secondary index taking a global one's name",
            create_body(
                "DupAcrossFamilies",
                json!([global_index("shared_name", "tag")]),
                json!([local_index("shared_name", "note")]),
            ),
            "shared_name",
        ),
        (
            "the repeat late in a longer list",
            create_body(
                "DupLateInList",
                json!([
                    global_index("idx.one", "tag"),
                    global_index("idx.two", "note"),
                    global_index("idx.two", "tag"),
                ]),
                Value::Null,
            ),
            "idx.two",
        ),
        (
            "a three-character name, the shortest the model admits",
            create_body(
                "DupShortestName",
                json!([global_index("abc", "tag"), global_index("abc", "note")]),
                Value::Null,
            ),
            "abc",
        ),
    ];

    for (case, body, repeated) in cases {
        let answered = refusal(&router, "CreateTable", body).await;
        assert_eq!(
            answered,
            (
                400,
                "ValidationException".to_string(),
                duplicate_index_name(repeated)
            ),
            "case {case:?}: expected the duplicate-index-name refusal over {repeated:?}, got \
             {answered:?}"
        );
    }

    // The same declaration with the names kept apart is served, so the refusals above are
    // about the repeated NAME and not about declaring two index families at once: the
    // created table reports every index back.
    let created = ok(
        &router,
        "CreateTable",
        create_body(
            "DistinctIndexNames",
            json!([global_index("idx.one", "tag"), global_index("idx.two", "note")]),
            json!([local_index("lsi.one", "note")]),
        ),
    )
    .await;
    let described = describe_when_active(&router, "distinct names", "DistinctIndexNames").await;
    for (case, description) in [
        ("the create echo", &created["TableDescription"]),
        ("the read back", &described),
    ] {
        let reported = (
            index_names(description, "GlobalSecondaryIndexes"),
            index_names(description, "LocalSecondaryIndexes"),
        );
        assert_eq!(
            reported,
            (
                vec!["idx.one".to_string(), "idx.two".to_string()],
                vec!["lsi.one".to_string()]
            ),
            "case {case:?}: expected every distinctly named index, got {reported:?} in \
             {description}"
        );
    }
}

/// One on-demand table carrying the `idx.keep` global secondary index, settled `ACTIVE`,
/// for the `UpdateTable` rows below to act on.
async fn table_with_one_index(router: &MultiServiceRouter, name: &str) {
    ok(
        router,
        "CreateTable",
        create_body(name, json!([global_index("idx.keep", "tag")]), Value::Null),
    )
    .await;
    describe_when_active(router, name, name).await;
}

/// An `UpdateTable` body carrying one index-update member.
fn update_body(table: &str, member: &str, updates: Value) -> Value {
    let mut body = json!({"TableName": table});
    body[member] = updates;
    body
}

/// A `Create` entry for a global secondary index the table does not carry.
fn create_entry(name: &str) -> Value {
    json!({"Create": global_index(name, "note")})
}

/// The boundary of RULE 1, and RULE 3: an UPDATE list that names one index twice asks for
/// two changes to one resource rather than declaring two indexes of a name, so it keeps
/// that member's own refusal — a `ValidationException` whose wording its own suite pins
/// (`tests/all/vector_index_updates.rs`) — and writes nothing. Rows differ in which pair of
/// actions names the index twice and whether the name is one the table carries or a new one.
#[tokio::test]
async fn an_index_update_list_that_names_one_index_twice_is_refused_and_writes_nothing() {
    let router = emulator();
    let table = "IndexNamedTwice";
    table_with_one_index(&router, table).await;

    let cases: [(&str, Value); 4] = [
        (
            "one index deleted twice",
            json!([{"Delete": {"IndexName": "idx.keep"}}, {"Delete": {"IndexName": "idx.keep"}}]),
        ),
        (
            "one index deleted and updated",
            json!([
                {"Delete": {"IndexName": "idx.keep"}},
                {"Update": {"IndexName": "idx.keep"}},
            ]),
        ),
        (
            "one index updated twice",
            json!([{"Update": {"IndexName": "idx.keep"}}, {"Update": {"IndexName": "idx.keep"}}]),
        ),
        (
            "a new name created and deleted in one list",
            json!([create_entry("idx.new"), {"Delete": {"IndexName": "idx.new"}}]),
        ),
    ];

    for (case, updates) in cases {
        let (status, code, stated) = refusal(
            &router,
            "UpdateTable",
            update_body(table, "GlobalSecondaryIndexUpdates", updates),
        )
        .await;
        assert_eq!(
            (status, code.as_str()),
            (400, "ValidationException"),
            "case {case:?}: expected the list to be refused, got {status} with {stated:?}"
        );
        assert!(
            stated.contains("named more than once"),
            "case {case:?}: expected the member's own duplicate sentence, got {stated:?}"
        );
    }

    // RULE 3: every one of those requests was refused before anything was written, so the
    // table still carries exactly the index it was created with.
    let described = describe_when_active(&router, "after the refusals", table).await;
    let reported = index_names(&described, "GlobalSecondaryIndexes");
    assert_eq!(
        reported,
        vec!["idx.keep".to_string()],
        "case {:?}: a refused update writes nothing, got {reported:?} in {described}",
        "after the refusals"
    );
}

/// RULE 2: rows differ in which shape's entry it is, how many of its action members the
/// entry states, which ones, and where in the list the entry sits.
#[tokio::test]
async fn an_index_update_entry_that_states_no_single_action_is_refused_over_its_action_members() {
    let router = emulator();
    let table = "IndexActionChoice";
    table_with_one_index(&router, table).await;

    // The action members of each shape, in the order the model declares them.
    let index_actions = ["Update", "Create", "Delete"];
    let vector_actions = ["Create", "Delete"];
    let cases: [(&str, &str, Value, String); 6] = [
        (
            "a secondary-index entry stating nothing",
            "GlobalSecondaryIndexUpdates",
            json!([{}]),
            no_action_stated("GlobalSecondaryIndexUpdate", &index_actions),
        ),
        (
            "an empty entry after a well-formed one",
            "GlobalSecondaryIndexUpdates",
            json!([{"Delete": {"IndexName": "idx.keep"}}, {}]),
            no_action_stated("GlobalSecondaryIndexUpdate", &index_actions),
        ),
        (
            "an entry stating a create and a delete",
            "GlobalSecondaryIndexUpdates",
            json!([{
                "Create": global_index("idx.new", "note"),
                "Delete": {"IndexName": "idx.keep"},
            }]),
            several_actions_stated("GlobalSecondaryIndexUpdate", &index_actions),
        ),
        (
            "an entry stating an update and a delete",
            "GlobalSecondaryIndexUpdates",
            json!([{"Update": {"IndexName": "idx.keep"}, "Delete": {"IndexName": "idx.keep"}}]),
            several_actions_stated("GlobalSecondaryIndexUpdate", &index_actions),
        ),
        (
            "an entry stating all three actions",
            "GlobalSecondaryIndexUpdates",
            json!([{
                "Update": {"IndexName": "idx.keep"},
                "Create": global_index("idx.new", "note"),
                "Delete": {"IndexName": "idx.keep"},
            }]),
            several_actions_stated("GlobalSecondaryIndexUpdate", &index_actions),
        ),
        (
            "a vector-index entry stating nothing",
            "VectorIndexUpdates",
            json!([{}]),
            no_action_stated("VectorIndexUpdate", &vector_actions),
        ),
    ];

    for (case, member, updates, expected) in cases {
        let answered = refusal(&router, "UpdateTable", update_body(table, member, updates)).await;
        assert_eq!(
            answered,
            (400, "ValidationException".to_string(), expected.clone()),
            "case {case:?}: expected {expected:?}, got {answered:?}"
        );
    }
}

/// RULE 1 over a RESTORE's index overrides: the surface that re-declares a backup's
/// indexes draws the same sentence, and the restore writes no table.
#[tokio::test]
async fn a_restore_override_that_names_one_index_twice_is_refused_over_that_name() {
    let router = emulator();
    let source = "RestoreOverrideSource";
    ok(
        &router,
        "CreateTable",
        create_body(
            source,
            json!([global_index("idx.keep", "tag"), global_index("idx.other", "note")]),
            Value::Null,
        ),
    )
    .await;
    describe_when_active(&router, "the restore source", source).await;
    let backup = ok(
        &router,
        "CreateBackup",
        json!({"TableName": source, "BackupName": "override-backup"}),
    )
    .await["BackupDetails"]["BackupArn"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    backup_when_available(&router, &backup).await;

    let restore = |overrides: Value| {
        json!({
            "TargetTableName": "RestoredOverrideTable",
            "BackupArn": backup.clone(),
            "GlobalSecondaryIndexOverride": overrides,
        })
    };
    let cases: [(&str, Value, &str); 2] = [
        (
            "one recorded index overridden twice",
            restore(json!([
                global_index("idx.keep", "tag"),
                global_index("idx.keep", "tag"),
            ])),
            "idx.keep",
        ),
        (
            "the repeat after another recorded index",
            restore(json!([
                global_index("idx.keep", "tag"),
                global_index("idx.other", "note"),
                global_index("idx.other", "note"),
            ])),
            "idx.other",
        ),
    ];

    for (case, body, repeated) in cases {
        let answered = refusal(&router, "RestoreTableFromBackup", body).await;
        assert_eq!(
            answered,
            (
                400,
                "ValidationException".to_string(),
                duplicate_index_name(repeated)
            ),
            "case {case:?}: expected the duplicate-index-name refusal over {repeated:?}, got \
             {answered:?}"
        );
    }

    // Nothing was restored under the target name, so the refusals wrote no table.
    let (status, body) = call(
        &router,
        "DescribeTable",
        json!({"TableName": "RestoredOverrideTable"}),
    )
    .await;
    assert_eq!(
        (status, error_code(&body)),
        (400, "ResourceNotFoundException".to_string()),
        "case {:?}: a refused restore creates no table, got {status} with {body}",
        "after the refusals"
    );
}
