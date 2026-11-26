// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP coverage of the OPTIMISTIC-CONCURRENCY loop a caller runs
//! against a table's own replication configuration: refuse, re-read, retry.
//!
//! The rule under test: `DeleteTableReplication` preconditions the removal of a
//! table's own replication configuration on the `versionToken` that
//! configuration currently carries — the member is `@required` on this
//! operation's input
//! (`smithy_query(query="operation", operation="DeleteTableReplication")`). A
//! token a later accepted write has superseded is the modelled
//! `ConflictException` (HTTP 409), stated with the one fixed sentence this
//! service words a TABLE version-token conflict with, and the refusal writes
//! nothing: the configuration, its role and its current token are exactly what
//! the last accepted write left. The caller therefore converges by re-reading
//! the token and retrying — and a retry whose re-read token was itself
//! superseded in the meantime is refused the same way again, so only the token
//! that is current when the delete arrives removes the record, after which the
//! delete answers the modelled 204 and the read of the configuration is the
//! modelled `NotFoundException` (HTTP 404).
//!
//! The two sentences below are not in the model, which carries only each
//! error's class documentation: they are the fixed text this service is
//! recorded stating these two refusal classes with, and they have no variable
//! parts at all — neither the presented token, the current one, the role nor
//! the table appears in either.
//!
//! The model bounds the shapes the rows span
//! (`smithy_query(query="closure", operation="PutTableReplication")`):
//! `TableReplicationRules` is `@length(1, 1)` and a rule's `destinations`
//! `@length(1, 5)`. So the rows differ in the region the table lives in, the
//! role the configuration names, how many destinations its one rule carries —
//! one, several and the modelled maximum of five — how many accepted writes
//! precede the first refusal (hence how many superseded tokens there are to
//! present), and how many further accepted writes land between the refusal and
//! the retry, which is what makes the loop take one round in some rows and
//! several in others.

use serde_json::{json, Value};

use crate::harness::{Emulator, Reply};
use crate::table_record_expiration::create_table;

/// The fixed sentence this service states a presented `versionToken` that is
/// not the current one with when the refusal is stated against the TABLE the
/// request addresses, as `DeleteTableReplication`'s is.
const STALE_VERSION_TOKEN: &str = "Provided version token does not match the table version token.";

/// The fixed sentence this service states a table that holds no replication
/// configuration of its own with.
const NO_REPLICATION_CONFIGURATION: &str = "The specified replication configuration does not exist.";

/// One row of the loop: a table in `bucket` whose own configuration names
/// `role` and `destinations` destination table buckets, written `writes` times
/// before the first delete is attempted, with `races` further accepted writes
/// landing between each refusal and the caller's retry.
#[derive(Debug)]
struct Case {
    label: &'static str,
    region: &'static str,
    /// The source table bucket's name — lowercase letters, digits and hyphens,
    /// 3..63 characters, as `CreateTableBucket` accepts one.
    bucket: &'static str,
    /// The table's name (`^[0-9a-z_]*$`).
    table: &'static str,
    /// An IAM role ARN of the form the modelled `IAMRole` pattern accepts
    /// (`^arn:.+:iam::[0-9]{12}:role/.+$`).
    role: &'static str,
    /// How many destination table buckets the one rule carries;
    /// `ReplicationDestinations` is `@length(min: 1, max: 5)`.
    destinations: usize,
    /// How many accepted writes precede the first delete attempt.
    writes: usize,
    /// How many further accepted writes supersede the token the caller re-read,
    /// each costing the loop one more round before it converges.
    races: usize,
}

fn cases() -> Vec<Case> {
    vec![
        Case {
            label: "one destination, one write, no racing writer",
            region: "us-east-1",
            bucket: "repl-del-retry-east",
            table: "orders_east",
            role: "arn:aws:iam::000000000000:role/s3tables-delete-retry",
            destinations: 1,
            writes: 1,
            races: 0,
        },
        Case {
            label: "two destinations, two writes, one racing writer",
            region: "us-west-2",
            bucket: "repl-del-retry-west",
            table: "orders_west",
            role: "arn:aws:iam::000000000000:role/delete-retry-west",
            destinations: 2,
            writes: 2,
            races: 1,
        },
        Case {
            label: "three destinations, four writes, a role under a path, two racing writers",
            region: "eu-west-1",
            bucket: "repl-del-retry-eu",
            table: "clicks_eu",
            role: "arn:aws:iam::000000000000:role/delete/retry/eu-nested-path",
            destinations: 3,
            writes: 4,
            races: 2,
        },
        Case {
            label: "the modelled maximum of five destinations, three writes, three racing writers",
            region: "ap-south-1",
            bucket: "repl-del-retry-ap",
            table: "sensor_readings",
            role: "arn:aws:iam::000000000000:role/delete-retry-ap",
            destinations: 5,
            writes: 3,
            races: 3,
        },
    ]
}

/// A configuration whose ONE rule — `TableReplicationRules` is `@length(1, 1)` —
/// replicates to `destinations`.
fn configuration(role: &str, destinations: &[String]) -> Value {
    json!({
        "role": role,
        "rules": [{
            "destinations": destinations
                .iter()
                .map(|arn| json!({ "destinationTableBucketARN": arn }))
                .collect::<Vec<Value>>(),
        }],
    })
}

/// Create `count` destination table buckets named after `prefix` in `region`,
/// and answer their ARNs.
async fn destination_buckets(
    emulator: &Emulator,
    region: &str,
    prefix: &str,
    count: usize,
    case: &str,
) -> Vec<String> {
    let mut arns = Vec::with_capacity(count);
    for index in 0..count {
        let name = format!("{prefix}-{index}");
        let created = emulator
            .create_table_bucket(region, &json!({ "name": name }))
            .await;
        assert_eq!(
            created.status, 200,
            "case {case:?}: creating the destination bucket {name:?} must succeed, got {status} \
             {body}",
            status = created.status,
            body = created.body
        );
        arns.push(created.text("arn"));
    }
    arns
}

/// What one row set up: the table, the configuration its writes applied, every
/// token an accepted write handed back (last one current) and the destinations.
struct Written {
    label: &'static str,
    region: &'static str,
    role: &'static str,
    table_arn: String,
    configuration: Value,
    /// Every token an accepted write reported, in order; the last is current.
    tokens: Vec<String>,
}

impl Written {
    /// The token the last accepted write reported.
    fn current(&self) -> &str {
        self.tokens
            .last()
            .expect("every row performs at least one write")
    }

    /// The tokens a later write superseded.
    fn superseded(&self) -> &[String] {
        &self.tokens[..self.tokens.len() - 1]
    }
}

/// Apply one accepted write of the row's configuration, presenting `token`
/// where the caller holds one, and answer the token it reports.
async fn accepted_write(
    emulator: &Emulator,
    row: &Written,
    token: Option<&str>,
    case: &str,
) -> String {
    let mut query = vec![("tableArn", row.table_arn.clone())];
    if let Some(held) = token {
        query.push(("versionToken", held.to_string()));
    }
    let put = emulator
        .put_table_replication(row.region, &query, &row.configuration)
        .await;
    assert_eq!(
        put.status, 200,
        "case {case:?}: the write must be accepted, got {status} {body}",
        status = put.status,
        body = put.body
    );
    put.text("versionToken")
}

/// Apply one row: the table, its destinations and the row's successive accepted
/// writes, chaining each reported token into the next write.
async fn write_row(emulator: &Emulator, case: &Case) -> Written {
    let label = case.label;
    let table_arn = create_table(emulator, case.region, case.bucket, case.table).await;
    let destinations = destination_buckets(
        emulator,
        case.region,
        &format!("{bucket}-dst", bucket = case.bucket),
        case.destinations,
        label,
    )
    .await;
    let mut row = Written {
        label,
        region: case.region,
        role: case.role,
        table_arn,
        configuration: configuration(case.role, &destinations),
        tokens: Vec::new(),
    };
    for write in 0..case.writes {
        let held = row.tokens.last().cloned();
        let minted = accepted_write(emulator, &row, held.as_deref(), label).await;
        assert!(
            !row.tokens.contains(&minted),
            "case {label:?}: write {write} must mint a token no earlier write reported, got \
             {minted:?} among {earlier:?}",
            earlier = row.tokens
        );
        row.tokens.push(minted);
    }
    row
}

/// Attempt the delete presenting `token`.
async fn delete_with(emulator: &Emulator, row: &Written, token: &str) -> Reply {
    emulator
        .delete_table_replication(
            row.region,
            &[
                ("tableArn", row.table_arn.clone()),
                ("versionToken", token.to_string()),
            ],
        )
        .await
}

/// Assert `reply` is the modelled conflict, stated with the fixed
/// table-version-token sentence and carrying no value of the request or the
/// store.
fn assert_conflict(reply: &Reply, row: &Written, presented: &str, case: &str) {
    assert_eq!(
        reply.status, 409,
        "case {case:?}: expected status 409, got {actual} {body}",
        actual = reply.status,
        body = reply.body
    );
    assert_eq!(
        reply.error_code.as_deref(),
        Some("ConflictException"),
        "case {case:?}: expected ConflictException, got {actual:?}",
        actual = reply.error_code
    );
    assert_eq!(
        reply.message(),
        STALE_VERSION_TOKEN,
        "case {case:?}: expected the fixed sentence {STALE_VERSION_TOKEN:?}, got {actual:?}",
        actual = reply.message()
    );
    for value in [presented, row.current(), row.table_arn.as_str(), row.role] {
        assert!(
            !reply.message().contains(value),
            "case {case:?}: the sentence has no variable part, got {value:?} in {actual:?}",
            actual = reply.message()
        );
    }
}

/// Read the configuration back and assert it is what the last accepted write
/// left — the token `expected` and the row's role — answering the token the
/// read reports, which is the token a retrying caller would present next.
async fn re_read_token(emulator: &Emulator, row: &Written, expected: &str, case: &str) -> String {
    let read = emulator
        .get_table_replication(row.region, &row.table_arn)
        .await;
    assert_eq!(
        read.status, 200,
        "case {case:?}: the configuration must still be served, got {status} {body}",
        status = read.status,
        body = read.body
    );
    let reported = read.text("versionToken");
    assert_eq!(
        reported, expected,
        "case {case:?}: expected the read to report the current token {expected:?}, got \
         {reported:?}"
    );
    let role = read
        .body
        .get("configuration")
        .and_then(|configuration| configuration.get("role"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    assert_eq!(
        role, row.role,
        "case {case:?}: expected the read to report the role {role_expected:?}, got {role:?}",
        role_expected = row.role
    );
    reported
}

/// Assert the record is gone: the delete's modelled 204, then the modelled
/// not-found of a table holding no configuration of its own.
async fn assert_removed(emulator: &Emulator, row: &Written, deleted: &Reply, case: &str) {
    assert_eq!(
        deleted.status, 204,
        "case {case:?}: the accepted delete answers the modelled 204, got {status} {body}",
        status = deleted.status,
        body = deleted.body
    );
    let read = emulator
        .get_table_replication(row.region, &row.table_arn)
        .await;
    assert_eq!(
        read.status, 404,
        "case {case:?}: the table must hold no configuration afterwards, got {status} {body}",
        status = read.status,
        body = read.body
    );
    assert_eq!(
        read.error_code.as_deref(),
        Some("NotFoundException"),
        "case {case:?}: expected NotFoundException, got {actual:?}",
        actual = read.error_code
    );
    assert_eq!(
        read.message(),
        NO_REPLICATION_CONFIGURATION,
        "case {case:?}: expected the fixed sentence {NO_REPLICATION_CONFIGURATION:?}, got \
         {actual:?}",
        actual = read.message()
    );
}

#[tokio::test]
async fn every_superseded_token_is_the_table_version_token_conflict_and_removes_nothing() {
    let emulator = Emulator::new();
    for spec in cases() {
        let row = write_row(&emulator, &spec).await;
        for (index, stale) in row.superseded().iter().enumerate() {
            let case = format!(
                "{label} / the token write {index} reported",
                label = row.label
            );
            let refused = delete_with(&emulator, &row, stale).await;
            assert_conflict(&refused, &row, stale, &case);
            // The refusal is a precondition failure, so it leaves the record
            // exactly as the last accepted write left it: the caller re-reads
            // the same current token after as before.
            re_read_token(&emulator, &row, row.current(), &case).await;
        }
    }
}

#[tokio::test]
async fn the_retry_loop_converges_only_on_the_token_that_is_current_when_the_delete_arrives() {
    let emulator = Emulator::new();
    for spec in cases() {
        let row = write_row(&emulator, &spec).await;
        let label = row.label;

        // The caller starts out holding the token of the FIRST write, which
        // every later write of the row superseded, and then runs the loop a
        // client doing optimistic concurrency runs: present, and on the
        // conflict re-read and retry. A racing writer lands `races` further
        // accepted writes, each superseding the token just re-read, so the loop
        // takes one round per race before it converges.
        assert!(
            spec.races == 0 || row.tokens.len() > 1,
            "case {label:?}: a row with a racing writer must start the caller on a token a later \
             write superseded"
        );
        let mut held = row.tokens[0].clone();
        let mut current = row.current().to_string();
        let mut remaining_races = spec.races;
        let mut rounds = 0usize;
        // One round per superseded token, one per racing write, and the final
        // accepted round: a generous bound that still ends the loop if the
        // delete neither refuses nor converges.
        let bound = row.tokens.len() + spec.races + 2;
        loop {
            rounds += 1;
            assert!(
                rounds <= bound,
                "case {label:?}: the retry loop must converge within {bound} rounds"
            );
            let case = format!("{label} / round {rounds}");
            let attempted = delete_with(&emulator, &row, &held).await;
            if held == current {
                assert_removed(&emulator, &row, &attempted, &case).await;
                break;
            }
            assert_conflict(&attempted, &row, &held, &case);
            held = re_read_token(&emulator, &row, &current, &case).await;
            if remaining_races > 0 {
                remaining_races -= 1;
                // A writer that got in first: the token the caller just read is
                // superseded before the retry reaches the delete.
                current = accepted_write(&emulator, &row, Some(&held), &case).await;
                assert_ne!(
                    held, current,
                    "case {case:?}: the racing write must mint a token of its own"
                );
            }
        }
        // One refusal for the superseded token the caller started on, one more
        // for each racing write that superseded the token it re-read, and then
        // the round the delete is accepted on.
        let expected_rounds = usize::from(row.tokens.len() > 1) + spec.races + 1;
        assert_eq!(
            rounds, expected_rounds,
            "case {label:?}: expected the loop to converge in {expected_rounds} rounds, took \
             {rounds}"
        );
    }
}
