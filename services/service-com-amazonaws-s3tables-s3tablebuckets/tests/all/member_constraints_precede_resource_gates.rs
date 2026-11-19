// SPDX-License-Identifier: CC0-1.0

//! WHEN a modelled member constraint is checked, and therefore WHICH of the two
//! refusal layers a caller is told about, for a request that breaks one.
//!
//! The rule under test: a value a modelled member constraint rules out, on a
//! member this service words no sentence of its own for, is refused by the
//! parameter validator in FRONT of the service — HTTP 400 under that validator's
//! own code (`ValidationException`, which names no shape of this model) and its
//! one envelope, `1 validation error detected: Value at '<member path>' failed to
//! satisfy constraint: <requirement>` — before the operation's own resource gates
//! run. So the refusal does not depend on the resource the request addresses: the
//! same out-of-range value is answered identically whether the addressed resource
//! would have been served, refused by a gate of the operation's own, or not found
//! at all, and nothing the request asked for is created.
//!
//! The two layers are what a client branches on. This service's own refusals —
//! an invalid open table format, a non-empty namespace, an encryption algorithm
//! it will not act on — all carry the one client-input error its model declares
//! (`BadRequestException`), so a parameter refused for its SHAPE has to arrive
//! under the other code or the two classes cannot be told apart at all.
//!
//! The table spans what the rule generalises over rather than one request: which
//! CONSTRAINT class the value broke (a `@range` minimum, an enumerated set), how
//! deep in the request the member sat (two structures down in a body, one
//! structure down), which operation carried it, the width the refused number
//! arrives in on the wire, and — the point of this suite — the state of the
//! resource the call addresses, which is what decides the gate the refusal would
//! otherwise have reached. The bounds and value sets quoted below are the
//! model's own (`smithy_query(query="shape",
//! shape="TableRecordExpirationSettings,PositiveInteger,StorageClass")`).
//!
//! What this suite deliberately does NOT restate: the values each bound and each
//! enumerated set ADMITS. Those are served and read back by
//! `table_record_expiration.rs`, `record_expiration_configuration_reads.rs` and
//! `table_bucket_storage_class.rs`, and the accepted side appears here only as
//! the well-formed request each row ends with — the one that reaches the gate the
//! refusals above it never got to.

use serde_json::{json, Value};

use crate::harness::{expected_bucket_arn, Emulator, Reply};
use crate::table_record_expiration::{create_table, mark_managed, stored_expiration};

/// The region every case works in.
const REGION: &str = "us-east-1";

/// The code the parameter validator in front of the service answers with. It
/// names no shape of this model, which is what tells a client the request was
/// refused for the SHAPE of a parameter rather than by a rule of the service.
const PARAMETER_VALIDATION: &str = "ValidationException";

/// The one client-input error this model declares, and so the code of every
/// refusal the SERVICE itself words — the code a parameter refusal must NOT
/// arrive under.
const SERVICE_REFUSAL: &str = "BadRequestException";

/// The shortest retention the model admits for a table's records
/// (`TableRecordExpirationSettings.days` targets `PositiveInteger`,
/// `@range(1, 2147483647)`), and so the limit the requirement below states.
const SHORTEST_RETENTION: i64 = 1;

/// The day counts below [`SHORTEST_RETENTION`]: the first value under the
/// minimum, a negative one, a negative one of ordinary size, and one no `i32`
/// holds at all — so the refusal is about the bound and not about the width the
/// number arrived in.
const SHORTER_THAN_THE_SHORTEST: [i64; 4] = [0, -1, -365, -4_294_967_296];

/// The requirement a `@range` minimum states, over the limit the MODEL carries.
fn at_least(limit: i64) -> String {
    format!("Member must have value greater than or equal to {limit}")
}

/// The requirement an enumerated set states, over the values the TYPE admits.
/// `StorageClass` declares `STANDARD`, `INTELLIGENT_TIERING`
/// (`smithy_query(query="shape", shape="StorageClass")`), and this service states
/// such a set sorted.
fn one_of(values: &[&str]) -> String {
    format!(
        "Member must satisfy enum value set: [{}]",
        values.join(", ")
    )
}

/// The whole sentence one refused member produces.
fn expected_message(member_path: &str, requirement: &str) -> String {
    format!(
        "1 validation error detected: Value at '{member_path}' failed to satisfy constraint: \
         {requirement}"
    )
}

/// Assert that `reply` is the parameter validator's refusal of `member_path` for
/// `requirement`, and not any refusal of the service's own.
fn assert_parameter_refusal(reply: &Reply, case: &str, member_path: &str, requirement: &str) {
    assert_eq!(
        reply.status, 400,
        "case {case:?}: expected status 400, got {status} {body}",
        status = reply.status,
        body = reply.body
    );
    assert_eq!(
        reply.error_code.as_deref(),
        Some(PARAMETER_VALIDATION),
        "case {case:?}: expected the validator's code {PARAMETER_VALIDATION} rather than the \
         service's own {SERVICE_REFUSAL}, got {actual:?}",
        actual = reply.error_code
    );
    assert_eq!(
        reply.message(),
        expected_message(member_path, requirement),
        "case {case:?}: expected the validator's envelope, got {actual:?}",
        actual = reply.message()
    );
}

/// How the table a record-expiration call addresses stands, and what the
/// operation's own gate would have answered a well-formed request with — the
/// variable this suite holds the refusal constant over.
#[derive(Debug, Clone, Copy)]
enum Addressed {
    /// A customer-created table, which takes no record-expiration configuration:
    /// a well-formed put is refused 405 by the operation's own gate.
    TakesNoConfiguration,
    /// A table Amazon Web Services manages, which does take one: a well-formed
    /// put is served 204.
    TakesOne,
    /// An ARN nothing is stored under: a well-formed put is refused 404.
    NotStored,
}

impl Addressed {
    /// A short name for the resources this row creates: `TableBucketName` is
    /// `@length(3, 63)`, so the label is too long to name one with.
    fn slug(self) -> &'static str {
        match self {
            Self::TakesNoConfiguration => "gate-405",
            Self::TakesOne => "served",
            Self::NotStored => "absent",
        }
    }
}

/// The table ARN a row addresses, with the store put into the state the row
/// names.
async fn addressed_table(emulator: &Emulator, addressed: Addressed) -> String {
    let slug = addressed.slug();
    let table_arn = create_table(
        emulator,
        REGION,
        &format!("precedence-{slug}"),
        &format!("{}_table", slug.replace('-', "_")),
    )
    .await;
    match addressed {
        Addressed::TakesNoConfiguration => table_arn,
        Addressed::TakesOne => {
            mark_managed(emulator, &table_arn);
            table_arn
        }
        // An ARN of the form the member admits, under which nothing is stored:
        // the created table's, with its table id replaced.
        Addressed::NotStored => {
            let (prefix, _) = table_arn
                .rsplit_once('/')
                .expect("a table ARN carries its table id as its last segment");
            format!("{prefix}/0e8d9f0c-7a6b-4c2d-9e1f-2a3b4c5d6e7f")
        }
    }
}

/// What a well-formed put against `addressed` answers — the gate the refused
/// requests below never reach, asserted so each row's addressed state is known
/// to differ.
async fn assert_gate_decides(emulator: &Emulator, addressed: Addressed, table_arn: &str) {
    let served = emulator
        .put_table_record_expiration_configuration(
            REGION,
            table_arn,
            &json!({ "status": "enabled", "settings": { "days": 30 } }),
        )
        .await;
    let expected = match addressed {
        Addressed::TakesNoConfiguration => 405,
        Addressed::TakesOne => 204,
        Addressed::NotStored => 404,
    };
    assert_eq!(
        served.status, expected,
        "case {addressed:?}: a well-formed put must reach the operation's gate and answer \
         {expected}, got {status} {body}",
        status = served.status,
        body = served.body
    );
}

#[tokio::test]
async fn a_retention_below_the_modelled_minimum_is_the_validators_refusal_whatever_the_table() {
    let emulator = Emulator::new();

    for addressed in [
        Addressed::TakesNoConfiguration,
        Addressed::TakesOne,
        Addressed::NotStored,
    ] {
        let table_arn = addressed_table(&emulator, addressed).await;

        for days in SHORTER_THAN_THE_SHORTEST {
            let case = format!("{addressed:?} with a retention of {days} days");
            let refused = emulator
                .put_table_record_expiration_configuration(
                    REGION,
                    &table_arn,
                    &json!({ "status": "enabled", "settings": { "days": days } }),
                )
                .await;
            assert_parameter_refusal(
                &refused,
                &case,
                "value.settings.days",
                &at_least(SHORTEST_RETENTION),
            );
            // The clause states the member and the limit alone: the number the
            // request carried comes back nowhere.
            assert!(
                !refused.message().contains(&days.to_string()),
                "case {case:?}: the refusal must not echo the request's {days}, got {actual:?}",
                actual = refused.message()
            );
            // And the refused put configured nothing, whatever the table.
            assert_eq!(
                stored_expiration(&emulator, &table_arn),
                None,
                "case {case:?}: a refused put must persist nothing, got {stored:?}",
                stored = stored_expiration(&emulator, &table_arn)
            );
        }

        // The refusals above never reached the operation, which is what the
        // gate's own answer to a well-formed request shows. Asserted last, since
        // on a table that takes a configuration this one is served.
        assert_gate_decides(&emulator, addressed, &table_arn).await;
    }
}

#[tokio::test]
async fn the_read_after_a_refused_put_still_answers_the_tables_own_gate() {
    let emulator = Emulator::new();

    // The recorded instance's own shape: a customer-created table, on which
    // every record-expiration call is refused by the operation's gate. The
    // out-of-range put is refused in front of that gate, and the read afterwards
    // still answers the gate — so the refused put created no configuration.
    let customer = addressed_table(&emulator, Addressed::TakesNoConfiguration).await;
    let refused = emulator
        .put_table_record_expiration_configuration(
            REGION,
            &customer,
            &json!({ "status": "enabled", "settings": { "days": 0 } }),
        )
        .await;
    assert_parameter_refusal(
        &refused,
        "a customer table and a retention of 0 days",
        "value.settings.days",
        &at_least(SHORTEST_RETENTION),
    );
    let read = emulator
        .get_table_record_expiration_configuration(REGION, &customer)
        .await;
    assert_eq!(
        read.status, 405,
        "case \"the read after the refused put\": expected 405, got {status} {body}",
        status = read.status,
        body = read.body
    );
    assert_eq!(
        read.error_code.as_deref(),
        Some("MethodNotAllowedException"),
        "case \"the read after the refused put\": expected MethodNotAllowedException, got \
         {actual:?}",
        actual = read.error_code
    );

    // The same on a table that DOES take a configuration: the read reports
    // expiration off, because the refused put configured nothing.
    let managed = addressed_table(&emulator, Addressed::TakesOne).await;
    let refused = emulator
        .put_table_record_expiration_configuration(
            REGION,
            &managed,
            &json!({ "status": "enabled", "settings": { "days": 0 } }),
        )
        .await;
    assert_parameter_refusal(
        &refused,
        "a managed table and a retention of 0 days",
        "value.settings.days",
        &at_least(SHORTEST_RETENTION),
    );
    let read = emulator
        .get_table_record_expiration_configuration(REGION, &managed)
        .await;
    assert_eq!(
        read.status, 200,
        "case \"the read on the managed table\": expected 200, got {status} {body}",
        status = read.status,
        body = read.body
    );
    assert_eq!(
        read.body
            .get("configuration")
            .and_then(|configuration| configuration.get("settings")),
        None,
        "case \"the read on the managed table\": the refused put must have configured no \
         retention, got {body}",
        body = read.body
    );
}

#[tokio::test]
async fn another_member_class_is_refused_in_front_of_its_own_resource_lookup_too() {
    let emulator = Emulator::new();

    // The rule is not the retention member's: a second constraint class, on a
    // member one structure down in another operation's body, is refused the same
    // way and just as early — the bucket the call addresses is never looked up,
    // so a bucket that exists and an ARN nothing is stored under answer alike.
    // Created naming the class that is NOT the default a bucket falls back to,
    // so the read below distinguishes the class the create named from a class a
    // refused put might have written.
    let created_with = "INTELLIGENT_TIERING";
    let created = emulator
        .create_table_bucket(
            REGION,
            &json!({
                "name": "precedence-storage-class",
                "storageClassConfiguration": { "storageClass": created_with },
            }),
        )
        .await;
    assert_eq!(
        created.status, 200,
        "the fixture bucket must be created, got {status} {body}",
        status = created.status,
        body = created.body
    );
    let resident = created.text("arn");
    let absent = expected_bucket_arn(REGION, "precedence-nothing-created");

    for (label, bucket) in [("a resident bucket", &resident), ("no bucket", &absent)] {
        for class in ["ULTRA_FAST_TIER", "standard", ""] {
            let case = format!("{label} and the storage class {class:?}");
            let refused = emulator
                .put_table_bucket_storage_class(REGION, bucket, &json!({ "storageClass": class }))
                .await;
            assert_parameter_refusal(
                &refused,
                &case,
                "storageClassConfiguration.storageClass",
                &one_of(&["INTELLIGENT_TIERING", "STANDARD"]),
            );
        }
    }

    // The resident bucket still reports the class it was created with, so the
    // refusals above changed nothing; and the ARN nothing was created under is
    // what the lookup the refusals preceded would have answered on.
    let read = emulator
        .get_table_bucket_storage_class(REGION, &resident)
        .await;
    assert_eq!(
        read.status, 200,
        "case \"the read after the refused puts\": expected 200, got {status} {body}",
        status = read.status,
        body = read.body
    );
    assert_eq!(
        read.body
            .get("storageClassConfiguration")
            .and_then(|configuration| configuration.get("storageClass"))
            .and_then(Value::as_str),
        Some(created_with),
        "case \"the read after the refused puts\": the bucket must still report the class its \
         create named, got {body}",
        body = read.body
    );
    let absent_put = emulator
        .put_table_bucket_storage_class(
            REGION,
            &absent,
            &json!({ "storageClass": "INTELLIGENT_TIERING" }),
        )
        .await;
    assert_eq!(
        absent_put.status, 404,
        "case \"a well-formed put on no bucket\": expected the lookup's 404, got {status} {body}",
        status = absent_put.status,
        body = absent_put.body
    );
    assert_eq!(
        absent_put.error_code.as_deref(),
        Some("NotFoundException"),
        "case \"a well-formed put on no bucket\": expected the lookup's NotFoundException, got \
         {actual:?}",
        actual = absent_put.error_code
    );
}
