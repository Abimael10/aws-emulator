// SPDX-License-Identifier: CC0-1.0

//! What this service answers a body that states no value for SEVERAL members its input
//! requires, driven end to end over the wire.
//!
//! THE RULE: a request that states no value for a member the input requires — the member
//! carried as an explicit `null`, or left out of a structure the body does carry — is
//! refused 400 `ValidationException` with ONE clause per such member inside the
//! `<N> validation error(s) detected: ` envelope, where `N` is the number of clauses. The
//! report is the WHOLE body's: a request with several members to fix states every one of
//! them rather than the first the reader stopped at, and the two spellings of one absence
//! earn the same answer, because a `null` is how a client marshals a member it is not
//! stating.
//!
//! The capacity pair is the one structure whose clause ORDER is published — the service
//! states its WRITE dimension first, where the model declares `ReadCapacityUnits` then
//! `WriteCapacityUnits` (`smithy_query(query="shape",
//! shape="com.amazonaws.dynamodb#ProvisionedThroughput")`, both members `@required`) — so
//! the suites below read the whole message back for that structure and, for every other
//! one, read back that each member with no value earns exactly one clause and that the
//! envelope counts them.
//!
//! Each suite is a TABLE spanning what the rule generalises over rather than one request:
//! HOW the body states each absence (both members left out, both stated `null`, one of
//! each, either way round), WHERE the structure stands (a top-level member of a table
//! update, of a table create, a structure inside a list of index updates, two such
//! structures at once), the modelled type of the member with no value (a long, a boolean, a
//! string, an enumerated string) and HOW MANY members one body leaves with no value (one,
//! two, four).

use serde_json::{json, Value};

use crate::emulator::{absent_clause, call, emulator, error_code, message, validation_envelope};
use crate::table_items::create_active_table;

/// The table the rows address.
const TABLE: &str = "Aggregated";

/// The ARN of that table, which the ARN-addressed tagging surface names it by.
fn table_arn() -> String {
    format!(
        "arn:aws:dynamodb:{}:000000000000:table/{TABLE}",
        crate::emulator::REGION
    )
}

/// Refuse `request` and answer the `message` the refusal carries, having read back that it
/// IS this service's 400 constraint refusal.
async fn refusal(
    router: &smithy_server_runtime::MultiServiceRouter,
    case: &str,
    operation: &str,
    request: Value,
) -> String {
    let (status, refused) = call(router, operation, request).await;
    assert_eq!(
        status, 400,
        "case {case:?} ({operation}): expected HTTP 400, got {status} with {refused}"
    );
    assert_eq!(
        error_code(&refused),
        "ValidationException",
        "case {case:?} ({operation}): a member with no value is a constraint violation, got \
         {refused}"
    );
    message(&refused)
}

/// A table update whose capacity pair is `throughput`.
fn update_with(throughput: Value) -> Value {
    json!({"TableName": TABLE, "ProvisionedThroughput": throughput})
}

/// A table create whose capacity pair is `throughput`, every other member of the
/// declaration well formed.
fn create_with(throughput: Value) -> Value {
    json!({"TableName": "Declared",
           "BillingMode": "PROVISIONED",
           "KeySchema": [{"AttributeName": "pk", "KeyType": "HASH"}],
           "AttributeDefinitions": [{"AttributeName": "pk", "AttributeType": "S"}],
           "ProvisionedThroughput": throughput})
}

/// A table update whose one index update carries `throughput`.
fn index_update_with(throughput: Value) -> Value {
    json!({"TableName": TABLE,
           "GlobalSecondaryIndexUpdates": [
               {"Update": {"IndexName": "by-label", "ProvisionedThroughput": throughput}}]})
}

/// The four spellings of "this capacity pair states neither of its members": both left out,
/// both stated `null`, and one of each either way round.
fn capacity_pairs_with_no_value() -> Vec<(&'static str, Value)> {
    vec![
        ("both members left out", json!({})),
        (
            "both members stated null",
            json!({"ReadCapacityUnits": null, "WriteCapacityUnits": null}),
        ),
        (
            "the read dimension stated null and the write one left out",
            json!({"ReadCapacityUnits": null}),
        ),
        (
            "the write dimension stated null and the read one left out",
            json!({"WriteCapacityUnits": null}),
        ),
    ]
}

/// One place a capacity pair stands in a request: the operation that carries it, the request
/// it stands in and the member path its two members are reported under.
struct Place {
    where_it_stands: &'static str,
    operation: &'static str,
    request: fn(Value) -> Value,
    at: &'static str,
}

#[tokio::test]
async fn a_capacity_pair_with_no_value_states_both_its_members() {
    let router = emulator();
    create_active_table(&router, TABLE, &[("pk", "HASH", "S")]).await;

    // The places the pair stands — the clauses are the WRITE dimension's then the READ one's,
    // whichever of the spellings below the body states the absence with.
    let places = [
        Place {
            where_it_stands: "a table update",
            operation: "UpdateTable",
            request: update_with,
            at: "provisionedThroughput",
        },
        Place {
            where_it_stands: "a table create",
            operation: "CreateTable",
            request: create_with,
            at: "provisionedThroughput",
        },
        Place {
            where_it_stands: "an index update inside a list",
            operation: "UpdateTable",
            request: index_update_with,
            at: "globalSecondaryIndexUpdates.1.member.update.provisionedThroughput",
        },
    ];

    for place in places {
        let expected = validation_envelope(&[
            absent_clause(&format!("{}.writeCapacityUnits", place.at)),
            absent_clause(&format!("{}.readCapacityUnits", place.at)),
        ]);
        for (spelling, throughput) in capacity_pairs_with_no_value() {
            let case = format!(
                "{} whose capacity pair has {spelling}",
                place.where_it_stands
            );
            let stated =
                refusal(&router, &case, place.operation, (place.request)(throughput)).await;
            assert_eq!(
                stated, expected,
                "case {case:?}: expected the message {expected:?}, got {stated:?}"
            );
        }
    }
}

#[tokio::test]
async fn every_member_with_no_value_earns_exactly_one_clause() {
    let router = emulator();
    create_active_table(&router, TABLE, &[("pk", "HASH", "S")]).await;

    // (case, operation, request, the paths the refusal must name) — the structures beyond
    // the capacity pair, whose clause order this service publishes nothing about: what is
    // read back is that every member with no value is named, once, and that the envelope
    // counts exactly those clauses.
    let rows: Vec<(&str, &str, Value, Vec<&str>)> = vec![
        (
            "one member of a capacity pair, the other one stated",
            "UpdateTable",
            update_with(json!({"ReadCapacityUnits": 5})),
            vec!["provisionedThroughput.writeCapacityUnits"],
        ),
        (
            "a required boolean and a required string, both left out of a structure",
            "UpdateTimeToLive",
            json!({"TableName": TABLE, "TimeToLiveSpecification": {}}),
            vec![
                "timeToLiveSpecification.enabled",
                "timeToLiveSpecification.attributeName",
            ],
        ),
        (
            "two required strings left out of a structure inside a list",
            "TagResource",
            json!({"ResourceArn": table_arn(), "Tags": [{}]}),
            vec!["tags.1.member.key", "tags.1.member.value"],
        ),
        (
            "a required string and a required enumerated string, left out of a key schema \
             element",
            "CreateTable",
            json!({"TableName": "Declared",
                   "BillingMode": "PAY_PER_REQUEST",
                   "KeySchema": [{}],
                   "AttributeDefinitions": [{"AttributeName": "pk", "AttributeType": "S"}]}),
            vec![
                "keySchema.1.member.attributeName",
                "keySchema.1.member.keyType",
            ],
        ),
        (
            "two capacity pairs with no value, in two elements of one list",
            "UpdateTable",
            json!({"TableName": TABLE,
                   "GlobalSecondaryIndexUpdates": [
                       {"Update": {"IndexName": "by-label", "ProvisionedThroughput": {}}},
                       {"Update": {"IndexName": "by-rank",
                                   "ProvisionedThroughput": {"ReadCapacityUnits": null}}}]}),
            vec![
                "globalSecondaryIndexUpdates.1.member.update.provisionedThroughput.\
                 readCapacityUnits",
                "globalSecondaryIndexUpdates.1.member.update.provisionedThroughput.\
                 writeCapacityUnits",
                "globalSecondaryIndexUpdates.2.member.update.provisionedThroughput.\
                 readCapacityUnits",
                "globalSecondaryIndexUpdates.2.member.update.provisionedThroughput.\
                 writeCapacityUnits",
            ],
        ),
    ];

    for (case, operation, request, paths) in rows {
        let stated = refusal(&router, case, operation, request).await;
        let counted = format!("{} validation error", paths.len());
        assert!(
            stated.starts_with(&counted),
            "case {case:?}: {} member(s) have no value, so the envelope counts {} of them, \
             got {stated:?}",
            paths.len(),
            paths.len()
        );
        for path in &paths {
            let clause = absent_clause(path);
            assert_eq!(
                stated.matches(clause.as_str()).count(),
                1,
                "case {case:?}: expected the clause {clause:?} stated once, got {stated:?}"
            );
        }
        // Exactly those clauses and nothing else: the envelope's own count, and one clause
        // per `; `-joined clause in it.
        let clauses = stated.split("; ").count();
        assert_eq!(
            clauses,
            paths.len(),
            "case {case:?}: expected {} clause(s), got {clauses} in {stated:?}",
            paths.len()
        );
    }
}

#[tokio::test]
async fn a_body_with_a_value_for_every_required_member_is_served() {
    let router = emulator();
    create_active_table(&router, TABLE, &[("pk", "HASH", "S")]).await;

    // The other half of the rule: asking what a body leaves with no value changes nothing
    // about a body that leaves nothing with no value. One row per place the suites above
    // report an absence in, each carrying a value for every member the input requires. The
    // table update comes last, because it is the one that leaves the table UPDATING.
    let (status, answered) = call(
        &router,
        "UpdateTimeToLive",
        json!({"TableName": TABLE,
               "TimeToLiveSpecification": {"Enabled": true, "AttributeName": "ttl"}}),
    )
    .await;
    assert_eq!(
        status, 200,
        "a time-to-live write stating both members is served, got {status} with {answered}"
    );
    let (status, answered) = call(
        &router,
        "TagResource",
        json!({"ResourceArn": table_arn(), "Tags": [{"Key": "owner", "Value": "team"}]}),
    )
    .await;
    assert_eq!(
        status, 200,
        "a tag write stating both members is served, got {status} with {answered}"
    );
    let (status, answered) = call(
        &router,
        "UpdateTable",
        json!({"TableName": TABLE,
               "BillingMode": "PROVISIONED",
               "ProvisionedThroughput": {"ReadCapacityUnits": 3, "WriteCapacityUnits": 4}}),
    )
    .await;
    assert_eq!(
        status, 200,
        "a table update stating both capacity members is served, got {status} with {answered}"
    );
}
