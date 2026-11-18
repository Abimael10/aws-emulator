// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP coverage of a TABLE's own replication configuration: the put
//! that sets one and the read that reports it.
//!
//! The rule under test: a table's replication configuration is a record of that
//! table, held independently of the configuration the table bucket holding it
//! carries. A table has one only once a put sets one on it, so a read of a table
//! that has none is refused with the modelled not-found and the one sentence this
//! service states that class with. An accepted put persists the WHOLE
//! configuration the request carries — its role and its one rule's destinations —
//! under a freshly minted version token, and reports the outcome of the CALL
//! rather than any state of the configuration; the read then reports exactly what
//! the store holds for THAT table and changes nothing a later caller sees. A
//! request that presents a `versionToken` is a precondition on the configuration
//! the caller believes is there: a table holding none refuses it as not-found and
//! creates nothing, a token that is not the stored one is the modelled conflict
//! and leaves the configuration in place, and the current token is accepted.
//!
//! The model is what bounds the shapes the rows span
//! (`smithy_query(query="closure", operation="PutTableReplication")`):
//! `TableReplicationRules` is `@length(1, 1)`, a rule's `destinations`
//! `@length(1, 5)` and `role` an `IAMRole`. So the rows differ in the region the
//! table lives in, the role the configuration names, how many destinations its
//! one rule carries — one, several and the modelled maximum of five — and whether
//! the put that REPLACES a configuration presents its token or none at all. None
//! of those may change what the put persists or what the read reports.

use serde_json::{json, Value};

use super::harness::{Emulator, Reply, ACCOUNT};
use super::table_record_expiration::create_table;

/// The fixed sentence this service states a table that has no replication
/// configuration with.
const NO_REPLICATION_CONFIGURATION: &str = "The specified replication configuration does not exist.";

/// The fixed sentence this service states a table the store does not hold with.
const NO_TABLE: &str = "The specified table does not exist.";

/// The fixed sentence this service states a presented version token that is not
/// the configuration's with.
const STALE_VERSION_TOKEN: &str = "The specified version token does not match the configuration.";

/// The outcome literal an accepted put reports, which describes the CALL.
const SUCCESS: &str = "success";

/// One row: a table in `bucket` whose replication configuration names `role` and
/// `destinations` destination table buckets, then replaced by one naming
/// `replacement_role` and `replacement_destinations` of them, the way
/// `replace_with_token` says.
#[derive(Debug)]
struct Case {
    label: &'static str,
    region: &'static str,
    /// The source table bucket's name — lowercase letters, numbers and hyphens,
    /// 3..63 characters, as `CreateTableBucket` accepts one.
    bucket: &'static str,
    /// The table's name (`^[0-9a-z_]*$`).
    table: &'static str,
    /// An IAM role ARN of the form the modelled `IAMRole` pattern accepts
    /// (`^arn:.+:iam::[0-9]{12}:role/.+$`, length 20..2048).
    role: &'static str,
    /// The role the REPLACING put names, so a replacement differs from what it
    /// replaces in its role as well as in its destinations.
    replacement_role: &'static str,
    /// How many destination table buckets the one rule carries.
    /// `ReplicationDestinations` is `@length(min: 1, max: 5)`, so a row carries
    /// between one and five.
    destinations: usize,
    /// How many destinations the REPLACING put's rule carries — a different
    /// count from `destinations` in every row, so a replacement that merged with
    /// what was there could not pass.
    replacement_destinations: usize,
    /// Whether the put that replaces the configuration presents its version
    /// token (the member is optional on the wire).
    replace_with_token: bool,
}

fn cases() -> Vec<Case> {
    vec![
        Case {
            label: "one destination, no token on the replacing put",
            region: "us-east-1",
            bucket: "table-repl-east",
            table: "orders_east",
            role: "arn:aws:iam::000000000000:role/s3tables-replication",
            replacement_role: "arn:aws:iam::000000000000:role/s3tables-replication-two",
            destinations: 1,
            replacement_destinations: 2,
            replace_with_token: false,
        },
        Case {
            label: "two destinations, the replacing put presents the token",
            region: "us-west-2",
            bucket: "table-repl-west",
            table: "orders_west",
            role: "arn:aws:iam::000000000000:role/replication-west",
            replacement_role: "arn:aws:iam::000000000000:role/replication-west-two",
            destinations: 2,
            replacement_destinations: 1,
            replace_with_token: true,
        },
        Case {
            label: "three destinations, a role under a path, in another Region",
            region: "eu-west-1",
            bucket: "table-repl-eu",
            table: "clicks_eu",
            role: "arn:aws:iam::000000000000:role/replication/eu/nested-path",
            replacement_role: "arn:aws:iam::000000000000:role/replication-eu-two",
            destinations: 3,
            replacement_destinations: 5,
            replace_with_token: true,
        },
        Case {
            label: "the modelled maximum of five destinations",
            region: "ap-south-1",
            bucket: "table-repl-ap",
            table: "sensor_readings",
            role: "arn:aws:iam::000000000000:role/replication-ap",
            replacement_role: "arn:aws:iam::000000000000:role/replication-ap-two",
            destinations: 5,
            replacement_destinations: 1,
            replace_with_token: false,
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
                .collect::<Vec<_>>(),
        }],
    })
}

/// Create `count` destination table buckets named after `prefix` in `region`, and
/// answer their ARNs.
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

/// The role a replication read reports.
fn reported_role(reply: &Reply, case: &str) -> String {
    reply
        .body
        .get("configuration")
        .and_then(|configuration| configuration.get("role"))
        .and_then(Value::as_str)
        .unwrap_or_else(|| {
            panic!(
                "case {case:?}: the read must report the configuration's role, got {body}",
                body = reply.body
            )
        })
        .to_string()
}

/// The destination table bucket ARNs a replication read reports, in the order
/// the stored rule holds them.
fn reported_destinations(reply: &Reply, case: &str) -> Vec<String> {
    let rules = reply
        .body
        .get("configuration")
        .and_then(|configuration| configuration.get("rules"))
        .and_then(Value::as_array)
        .unwrap_or_else(|| {
            panic!(
                "case {case:?}: the read must report the configuration's rules, got {body}",
                body = reply.body
            )
        })
        .clone();
    assert_eq!(
        rules.len(),
        1,
        "case {case:?}: the model bounds the rule list at exactly one rule, got {rules:?}"
    );
    rules[0]
        .get("destinations")
        .and_then(Value::as_array)
        .unwrap_or_else(|| {
            panic!(
                "case {case:?}: the reported rule must carry its destinations, got {rule}",
                rule = rules[0]
            )
        })
        .iter()
        .map(|destination| {
            destination
                .get("destinationTableBucketARN")
                .and_then(Value::as_str)
                .unwrap_or_else(|| {
                    panic!(
                        "case {case:?}: a reported destination names its table bucket ARN, got \
                         {destination}"
                    )
                })
                .to_string()
        })
        .collect()
}

/// Assert `reply` is the modelled 404 stated with `message`.
fn assert_not_found(reply: &Reply, message: &str, case: &str) {
    assert_eq!(
        reply.status, 404,
        "case {case:?}: expected status 404, got {status} {body}",
        status = reply.status,
        body = reply.body
    );
    assert_eq!(
        reply.error_code.as_deref(),
        Some("NotFoundException"),
        "case {case:?}: expected NotFoundException, got {actual:?}",
        actual = reply.error_code
    );
    assert_eq!(
        reply.message(),
        message,
        "case {case:?}: expected the fixed sentence {message:?}, got {actual:?}",
        actual = reply.message()
    );
}

/// Assert `read` reports the configuration `(role, destinations)` under `token`.
fn assert_reports(
    read: &Reply,
    role: &str,
    destinations: &[String],
    token: &str,
    case: &str,
) {
    assert_eq!(
        read.status, 200,
        "case {case:?}: the read must be served, got {status} {body}",
        status = read.status,
        body = read.body
    );
    assert_eq!(
        reported_role(read, case),
        role,
        "case {case:?}: expected the stored role {role:?}, got {actual:?}",
        actual = reported_role(read, case)
    );
    assert_eq!(
        reported_destinations(read, case),
        destinations,
        "case {case:?}: expected the stored destinations {destinations:?}, got {actual:?}",
        actual = reported_destinations(read, case)
    );
    assert_eq!(
        read.text("versionToken"),
        token,
        "case {case:?}: expected the token of the write that stored it, got {actual:?}",
        actual = read.text("versionToken")
    );
}

#[tokio::test]
async fn a_put_persists_the_whole_configuration_and_the_read_reports_it() {
    let emulator = Emulator::new();
    for case in cases() {
        let label = case.label;
        let table_arn = create_table(&emulator, case.region, case.bucket, case.table).await;

        // Nothing replicates the table until a put says so, so the read is
        // refused rather than answered with an empty configuration.
        let before = emulator.get_table_replication(case.region, &table_arn).await;
        assert_not_found(
            &before,
            NO_REPLICATION_CONFIGURATION,
            &format!("{label}, before any put"),
        );

        let first = destination_buckets(
            &emulator,
            case.region,
            &format!("{bucket}-dst", bucket = case.bucket),
            case.destinations,
            label,
        )
        .await;
        let put = emulator
            .put_table_replication(
                case.region,
                &[("tableArn", table_arn.clone())],
                &configuration(case.role, &first),
            )
            .await;
        assert_eq!(
            put.status, 200,
            "case {label:?}: the put must be served, got {status} {body}",
            status = put.status,
            body = put.body
        );
        assert_eq!(
            put.text("status"),
            SUCCESS,
            "case {label:?}: expected the call's own outcome {SUCCESS:?}, got {actual:?}",
            actual = put.text("status")
        );
        let created_token = put.text("versionToken");
        assert!(
            !created_token.is_empty(),
            "case {label:?}: an accepted put mints a version token, got {created_token:?}"
        );

        // The read reports exactly what the put persisted, and reading twice
        // over reports the same: a read changes nothing a later caller sees.
        for attempt in 1..=2 {
            let probe = format!("{label}, read {attempt} after the first put");
            let read = emulator.get_table_replication(case.region, &table_arn).await;
            assert_reports(&read, case.role, &first, &created_token, &probe);
        }

        // A second put REPLACES the whole configuration: a different role and a
        // different number of destinations, under a token of its own.
        let second = destination_buckets(
            &emulator,
            case.region,
            &format!("{bucket}-alt", bucket = case.bucket),
            case.replacement_destinations,
            label,
        )
        .await;
        let mut query = vec![("tableArn", table_arn.clone())];
        if case.replace_with_token {
            query.push(("versionToken", created_token.clone()));
        }
        let replaced = emulator
            .put_table_replication(
                case.region,
                &query,
                &configuration(case.replacement_role, &second),
            )
            .await;
        assert_eq!(
            replaced.status, 200,
            "case {label:?}: the replacing put must be served, got {status} {body}",
            status = replaced.status,
            body = replaced.body
        );
        assert_eq!(
            replaced.text("status"),
            SUCCESS,
            "case {label:?}: expected the call's own outcome {SUCCESS:?}, got {actual:?}",
            actual = replaced.text("status")
        );
        let replaced_token = replaced.text("versionToken");
        assert_ne!(
            replaced_token, created_token,
            "case {label:?}: every write of the configuration mints a fresh token, got \
             {replaced_token:?} twice"
        );
        let read = emulator.get_table_replication(case.region, &table_arn).await;
        assert_reports(
            &read,
            case.replacement_role,
            &second,
            &replaced_token,
            &format!("{label}, after the replacing put"),
        );

        // The token the first write minted now names a configuration that was
        // superseded, so presenting it is the modelled conflict and leaves the
        // stored configuration exactly as the replacement left it.
        let conflict = emulator
            .put_table_replication(
                case.region,
                &[
                    ("tableArn", table_arn.clone()),
                    ("versionToken", created_token.clone()),
                ],
                &configuration(case.role, &first),
            )
            .await;
        assert_eq!(
            conflict.status, 409,
            "case {label:?}: a superseded token must be refused with status 409, got {status} \
             {body}",
            status = conflict.status,
            body = conflict.body
        );
        assert_eq!(
            conflict.error_code.as_deref(),
            Some("ConflictException"),
            "case {label:?}: expected ConflictException, got {actual:?}",
            actual = conflict.error_code
        );
        assert_eq!(
            conflict.message(),
            STALE_VERSION_TOKEN,
            "case {label:?}: expected the fixed sentence {STALE_VERSION_TOKEN:?}, got {actual:?}",
            actual = conflict.message()
        );
        assert!(
            conflict.optional_text("status").is_none(),
            "case {label:?}: a refused put reports no outcome, got {body}",
            body = conflict.body
        );
        let after = emulator.get_table_replication(case.region, &table_arn).await;
        assert_reports(
            &after,
            case.replacement_role,
            &second,
            &replaced_token,
            &format!("{label}, after the refused put"),
        );
    }
}

/// One refused put: the table ARN it addresses, whether it presents a version
/// token, and the sentence it is refused with.
#[derive(Debug)]
struct Refusal {
    case: &'static str,
    /// The `tableArn` the put addresses.
    addressed: String,
    /// The `versionToken` the put presents, when it presents one.
    token: Option<&'static str>,
    /// The fixed sentence the refusal carries.
    message: &'static str,
}

#[tokio::test]
async fn a_put_that_is_not_accepted_persists_nothing() {
    let region = "us-east-1";
    let role = "arn:aws:iam::000000000000:role/replication-refused";
    let emulator = Emulator::new();
    let table_arn = create_table(&emulator, region, "table-repl-refused", "refused_table").await;
    let destinations =
        destination_buckets(&emulator, region, "table-repl-refused-dst", 1, "setup").await;
    let settings = configuration(role, &destinations);

    // The table bucket the held table lives in, read off its ARN, so a row can
    // name a table id that bucket holds no table under.
    let bucket_arn = table_arn
        .split("/table/")
        .next()
        .expect("a table ARN carries its bucket's")
        .to_string();
    // A table id of the form the modelled `TableARN` pattern accepts
    // (`[a-zA-Z0-9-_]{1,255}`) that nothing was ever minted for.
    let absent_in_bucket = format!("{bucket_arn}/table/11111111-2222-4333-8444-555555555555");
    let absent_bucket = format!(
        "arn:aws:s3tables:{region}:{ACCOUNT}:bucket/table-repl-never/table/\
         66666666-7777-4888-8999-aaaaaaaaaaaa"
    );

    // A row per refusal class: a table ARN nothing is stored under, in a bucket
    // that is there and in one that is not, and a token presented on a table
    // that holds no configuration to precondition.
    let rows = vec![
        Refusal {
            case: "a table id the bucket holds no table under",
            addressed: absent_in_bucket,
            token: None,
            message: NO_TABLE,
        },
        Refusal {
            case: "a table in a bucket that was never created",
            addressed: absent_bucket,
            token: None,
            message: NO_TABLE,
        },
        Refusal {
            case: "a token on a table that holds no configuration",
            addressed: table_arn.clone(),
            token: Some("0"),
            message: NO_REPLICATION_CONFIGURATION,
        },
    ];

    for row in rows {
        let Refusal {
            case,
            addressed,
            token,
            message,
        } = row;
        let mut query = vec![("tableArn", addressed.clone())];
        if let Some(presented) = token {
            query.push(("versionToken", presented.to_string()));
        }
        let refused = emulator
            .put_table_replication(region, &query, &settings)
            .await;
        assert_not_found(&refused, message, case);
        assert!(
            refused.optional_text("status").is_none(),
            "case {case:?}: a refused put reports no outcome, got {body}",
            body = refused.body
        );
        // Nothing was persisted for the table the refused put addressed, so a
        // read of it is still the not-found of a table with no configuration —
        // or of a table that is not there at all.
        let read = emulator.get_table_replication(region, &addressed).await;
        let expected = if addressed == table_arn {
            NO_REPLICATION_CONFIGURATION
        } else {
            NO_TABLE
        };
        assert_not_found(&read, expected, &format!("{case}, read afterwards"));
    }
}

#[tokio::test]
async fn a_tables_configuration_is_held_apart_from_its_buckets() {
    let region = "us-east-1";
    let bucket_role = "arn:aws:iam::000000000000:role/replication-of-the-bucket";
    let table_role = "arn:aws:iam::000000000000:role/replication-of-the-table";
    let emulator = Emulator::new();
    let table_arn = create_table(&emulator, region, "table-repl-apart", "apart_table").await;
    let bucket_arn = table_arn
        .split("/table/")
        .next()
        .expect("a table ARN carries its bucket's")
        .to_string();

    let for_bucket =
        destination_buckets(&emulator, region, "table-repl-apart-bucket", 1, "bucket").await;
    let for_table =
        destination_buckets(&emulator, region, "table-repl-apart-table", 2, "table").await;

    // A configuration on the BUCKET says nothing about the table: the table
    // still holds none of its own, so the read of it is refused.
    let bucket_put = emulator
        .put_table_bucket_replication(
            region,
            &[("tableBucketARN", bucket_arn.clone())],
            &configuration(bucket_role, &for_bucket),
        )
        .await;
    assert_eq!(
        bucket_put.status, 200,
        "case \"the bucket's own put\": must be served, got {status} {body}",
        status = bucket_put.status,
        body = bucket_put.body
    );
    let bucket_token = bucket_put.text("versionToken");
    let table_read = emulator.get_table_replication(region, &table_arn).await;
    assert_not_found(
        &table_read,
        NO_REPLICATION_CONFIGURATION,
        "the table, after only the bucket was configured",
    );

    // And a configuration on the TABLE leaves the bucket's exactly as it was:
    // the two records are independent, so each read reports its own.
    let table_put = emulator
        .put_table_replication(
            region,
            &[("tableArn", table_arn.clone())],
            &configuration(table_role, &for_table),
        )
        .await;
    assert_eq!(
        table_put.status, 200,
        "case \"the table's own put\": must be served, got {status} {body}",
        status = table_put.status,
        body = table_put.body
    );
    let table_token = table_put.text("versionToken");
    assert_ne!(
        table_token, bucket_token,
        "case \"two configurations\": each write mints its own token, got {table_token:?} twice"
    );
    assert_reports(
        &emulator.get_table_replication(region, &table_arn).await,
        table_role,
        &for_table,
        &table_token,
        "the table's own configuration",
    );
    assert_reports(
        &emulator
            .get_table_bucket_replication(region, &bucket_arn)
            .await,
        bucket_role,
        &for_bucket,
        &bucket_token,
        "the bucket's own configuration",
    );
}
