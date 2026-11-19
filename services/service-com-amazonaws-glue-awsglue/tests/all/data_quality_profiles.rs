// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the Glue data quality PROFILE surface: the profile a finished ruleset
//! evaluation run records, `GetDataQualityModel`, `GetDataQualityModelResult` and
//! `PutDataQualityProfileAnnotation`.
//!
//! Every call goes over the wire through the shared [`crate::wire`] harness, so what is
//! asserted is HTTP behaviour and not a direct call into a handler. Amazon S3 is mounted on
//! the same engine, because the measurements a profile holds are taken over the objects
//! behind the data source's table: the object a test stages is the data that was measured.
//!
//! RULE under test: a profile is minted by a run that MEASURED something about its dataset,
//! and every operation of this surface addresses one by the `ProfileId` that run minted — so
//! an id no run minted, and a `StatisticId` the addressed profile holds no datapoint of,
//! resolve to no model result at all and are the modelled `EntityNotFoundException`. For a
//! profile that exists, the model over its datapoints is trained one span after the run
//! finished, and the trained model's prediction is computed from the datapoints that are
//! stored: the mean of the training set either side of the widest deviation from it, so every
//! trained datapoint lies inside the bounds. Annotating a profile out of the training set is
//! what moves that prediction.
//!
//! The recorded calls — a well-formed profile id no statistics run produced, read through both
//! model reads and annotated through the write — are rows of the miss table below; the rows
//! differ along the form of the id that resolves to nothing and which of the three operations
//! is asked.

use std::sync::Arc;
use std::time::{Duration, SystemTime};

use serde_json::{json, Value};
use service_com_amazonaws_glue_awsglue::state::{
    messages, ComAmazonawsGlueAwsglueState, DATA_QUALITY_MODEL_STATUS_RUNNING,
    DATA_QUALITY_MODEL_STATUS_SUCCEEDED, DATA_QUALITY_PROFILE_ID_PREFIX,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::wire::{call, emulator_on, error_code, error_message, ok};

/// The IAM role a start names: "an IAM role supplied to encrypt the results of the run".
const ROLE: &str = "arn:aws:iam::000000000000:role/glue-data-quality-role";

/// The bucket every table of this suite points its data at.
const DATA_BUCKET: &str = "dq-profile-data";

/// The database every table of this suite lives in.
const DATABASE: &str = "dq_profile_db";

/// How close two doubles have to be to count as the same: a mean and a bound are doubles, so
/// they are compared with a tolerance rather than bit for bit.
const TOLERANCE: f64 = 1e-9;

/// An emulator with Glue AND Amazon S3 mounted on one shared engine, plus the engine itself so
/// a test can make the status the clock owes a run or a model training due instead of waiting
/// for it.
fn emulator_with_s3() -> (MultiServiceRouter, Arc<engine::InMemoryEngine>) {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = emulator_on(engine.clone());
    router.register_service(service_com_amazonaws_s3_amazons3::EmulatedService::new(
        Arc::clone(&engine),
    ));
    (router, engine)
}

/// Store the database and a table whose data is `rows`.
async fn store_table(router: &MultiServiceRouter, table: &str, rows: &str) {
    let (status, parsed) = call(
        router,
        "CreateDatabase",
        &json!({ "DatabaseInput": { "Name": DATABASE } }),
    )
    .await;
    assert!(
        status == 200 || error_code(&parsed).ends_with("AlreadyExistsException"),
        "CreateDatabase must answer 200 or report the database is already stored, got {status} \
         with {parsed}"
    );
    stage_rows(router, table, rows).await;
    ok(
        router,
        "CreateTable",
        &json!({
            "DatabaseName": DATABASE,
            "TableInput": {
                "Name": table,
                "TableType": "EXTERNAL_TABLE",
                "StorageDescriptor": {
                    "Location": format!("s3://{DATA_BUCKET}/{table}/"),
                    "Columns": [
                        { "Name": "id", "Type": "string" },
                        { "Name": "status", "Type": "string" },
                    ],
                    "InputFormat": "org.apache.hadoop.mapred.TextInputFormat",
                    "SerdeInfo": {
                        "SerializationLibrary":
                            "org.apache.hadoop.hive.serde2.OpenCSVSerde",
                        "Parameters": { "separatorChar": "," },
                    },
                },
            },
        }),
    )
    .await;
}

/// Stage the data behind `table`: ONE object under its location, written at the same key
/// every time, so a later staging REPLACES the rows a run reads rather than adding to them.
async fn stage_rows(router: &MultiServiceRouter, table: &str, rows: &str) {
    let client = crate::wire::s3_client(router);
    // The bucket is shared by every table of this suite, so the create of an existing one is
    // no failure.
    let _ = client.create_bucket().bucket(DATA_BUCKET).send().await;
    client
        .put_object()
        .bucket(DATA_BUCKET)
        .key(format!("{table}/rows.csv"))
        .body(aws_sdk_s3::primitives::ByteStream::from(
            rows.as_bytes().to_vec(),
        ))
        .send()
        .await
        .unwrap_or_else(|error| panic!("the data of {table} must be staged, got {error:?}"));
}

/// Store a ruleset whose rules are `rules`, each entry one rule.
async fn store_ruleset(router: &MultiServiceRouter, name: &str, rules: &[&str]) {
    ok(
        router,
        "CreateDataQualityRuleset",
        &json!({
            "Name": name,
            "Ruleset": format!("Rules = [\n    {}\n]", rules.join(",\n    ")),
        }),
    )
    .await;
}

/// Make the terminal status the clock owes every unsettled run due, and the training of every
/// model that is still under way. The transitions are owed by the clock, so moving their due
/// instants is the same thing as the instants arriving.
fn make_owed_transitions_due(engine: &Arc<engine::InMemoryEngine>) {
    let state = engine.state::<ComAmazonawsGlueAwsglueState>();
    let mut store = state.lock();
    let past = SystemTime::now() - Duration::from_secs(1);
    for run in &mut store.data_quality_ruleset_evaluation_runs {
        if run.settles_at.is_some() {
            run.settles_at = Some(past);
        }
    }
    for profile in &mut store.data_quality_profiles {
        profile.model_trained_at = past;
    }
}

/// Move the instant the clock owes the model over `profile_id` its training to `due`, so a
/// row observes the training under way or finished without waiting on the wall clock. The
/// transition is owed by the clock, so moving its due instant is the same thing as the
/// instant arriving — or not having arrived.
fn set_model_training_due(
    engine: &Arc<engine::InMemoryEngine>,
    profile_id: &str,
    due: SystemTime,
) {
    let state = engine.state::<ComAmazonawsGlueAwsglueState>();
    let mut store = state.lock();
    let profile = store
        .data_quality_profile_mut(profile_id)
        .unwrap_or_else(|| panic!("the suite recorded the profile {profile_id}"));
    profile.model_trained_at = due;
}

/// Start an evaluation run of `rulesets` over `table`, settle it, and answer the `ProfileId`
/// the results it produced name — the id a client reads a profile's model by.
async fn given_profile(
    router: &MultiServiceRouter,
    engine: &Arc<engine::InMemoryEngine>,
    table: &str,
    rulesets: &[&str],
) -> String {
    let started = ok(
        router,
        "StartDataQualityRulesetEvaluationRun",
        &json!({
            "DataSource": { "GlueTable": { "DatabaseName": DATABASE, "TableName": table } },
            "Role": ROLE,
            "RulesetNames": rulesets,
        }),
    )
    .await;
    let run_id = started
        .get("RunId")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("the start must answer a RunId, got {started}"))
        .to_string();
    // The run's terminal status is owed by the clock; the profile exists from the instant it
    // settles, which is also when the model over its datapoints starts training.
    let state = engine.state::<ComAmazonawsGlueAwsglueState>();
    {
        let mut store = state.lock();
        let past = SystemTime::now() - Duration::from_secs(1);
        for run in &mut store.data_quality_ruleset_evaluation_runs {
            if run.settles_at.is_some() {
                run.settles_at = Some(past);
            }
        }
    }
    let run = ok(
        router,
        "GetDataQualityRulesetEvaluationRun",
        &json!({ "RunId": &run_id }),
    )
    .await;
    let result_id = run
        .get("ResultIds")
        .and_then(Value::as_array)
        .and_then(|ids| ids.first())
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("a settled run reports its result ids, got {run}"))
        .to_string();
    let result = ok(
        router,
        "GetDataQualityResult",
        &json!({ "ResultId": result_id }),
    )
    .await;
    let profile_id = result
        .get("ProfileId")
        .and_then(Value::as_str)
        .unwrap_or_else(|| {
            panic!("a result of a run that measured something names its profile, got {result}")
        })
        .to_string();
    assert!(
        profile_id.starts_with(DATA_QUALITY_PROFILE_ID_PREFIX),
        "a minted ProfileId carries the prefix the service spells one with, got {profile_id}"
    );
    profile_id
}

/// The `StatisticId`s the profile `profile_id` holds datapoints of, in the order they were
/// measured. Read from the store, because no implemented operation lists them yet.
fn statistic_ids(engine: &Arc<engine::InMemoryEngine>, profile_id: &str) -> Vec<String> {
    let state = engine.state::<ComAmazonawsGlueAwsglueState>();
    let store = state.lock();
    store
        .data_quality_profile(profile_id)
        .map(|profile| {
            profile
                .statistics
                .iter()
                .map(|statistic| statistic.statistic_id.clone())
                .collect()
        })
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// The model of a profile a run recorded
// ---------------------------------------------------------------------------

/// A run that measured something about its dataset records a profile, and the model over that
/// profile's datapoints is RUNNING until the clock owes it its training and SUCCEEDED
/// afterwards — reported for the profile alone and for each statistic it holds.
#[tokio::test]
async fn the_model_of_a_recorded_profile_is_trained_by_the_clock() {
    let (router, engine) = emulator_with_s3();
    store_table(&router, "trained", "1,ACTIVE\n2,ACTIVE\n3,INACTIVE\n").await;
    store_ruleset(
        &router,
        "trained-ruleset",
        &["RowCount = 3", "IsComplete \"id\""],
    )
    .await;
    let profile = given_profile(&router, &engine, "trained", &["trained-ruleset"]).await;

    // The training is owed one span out, so while that instant is still ahead a read reports
    // it under way rather than finished.
    set_model_training_due(&engine, &profile, SystemTime::now() + Duration::from_secs(600));
    let read = ok(
        &router,
        "GetDataQualityModel",
        &json!({ "ProfileId": &profile }),
    )
    .await;
    assert_eq!(
        read.get("Status").and_then(Value::as_str),
        Some(DATA_QUALITY_MODEL_STATUS_RUNNING),
        "the training of a freshly recorded profile's model is under way, got {read}"
    );
    assert!(
        read.get("StartedOn").is_some(),
        "the training started when the profile was recorded, got {read}"
    );
    assert_eq!(
        read.get("CompletedOn"),
        None,
        "a training under way has not completed, got {read}"
    );
    // A model still training has produced no result to read.
    let (status, body) = call(
        &router,
        "GetDataQualityModelResult",
        &json!({
            "ProfileId": &profile,
            "StatisticId": statistic_ids(&engine, &profile)[0].clone(),
        }),
    )
    .await;
    assert_eq!(
        status, 400,
        "a training under way has no model result, got {status} with {body}"
    );
    assert_eq!(
        error_message(&body),
        messages::DATA_QUALITY_MODEL_RESULT_NOT_FOUND,
        "expected the model-result miss, got {body}"
    );

    make_owed_transitions_due(&engine);
    let statistics = statistic_ids(&engine, &profile);
    assert!(
        !statistics.is_empty(),
        "a run that measured something records those measurements as datapoints"
    );
    for (case, body) in [
        ("the profile alone", json!({ "ProfileId": &profile })),
        (
            "one statistic of the profile",
            json!({ "ProfileId": &profile, "StatisticId": statistics[0].clone() }),
        ),
    ] {
        let read = ok(&router, "GetDataQualityModel", &body).await;
        assert_eq!(
            read.get("Status").and_then(Value::as_str),
            Some(DATA_QUALITY_MODEL_STATUS_SUCCEEDED),
            "case {case}: the training the clock owed has completed, got {read}"
        );
        assert!(
            read.get("CompletedOn").is_some(),
            "case {case}: a completed training reports when it completed, got {read}"
        );
        assert_eq!(
            read.get("FailureReason"),
            None,
            "case {case}: a training that produced a model reports no failure, got {read}"
        );
    }
}

/// The trained model's prediction is computed from the datapoints that are stored: every
/// datapoint it was trained on lies inside the bounds it predicts, the predicted value is
/// their mean, and a datapoint annotated OUT of the training set stops deciding them.
///
/// The datapoints are the measurements of one statistic across the runs that took them, so
/// the rows of this table are the runs: three runs over tables of different sizes, which is
/// what makes the statistic's history vary.
#[tokio::test]
async fn the_model_result_is_computed_from_the_stored_datapoints() {
    let (router, engine) = emulator_with_s3();
    store_ruleset(&router, "rowcount-ruleset", &["RowCount > 0"]).await;

    // Three runs over the SAME table, whose data is replaced between them: each run measures
    // the row count, so the three measurements are three datapoints of one statistic.
    let mut profiles = Vec::new();
    let mut expected_values = Vec::new();
    store_table(&router, "history", "1,ACTIVE\n").await;
    for (round, rows) in [
        "1,ACTIVE\n",
        "1,ACTIVE\n2,ACTIVE\n",
        "1,ACTIVE\n2,ACTIVE\n3,INACTIVE\n",
    ]
    .iter()
    .enumerate()
    {
        stage_rows(&router, "history", rows).await;
        profiles.push(given_profile(&router, &engine, "history", &["rowcount-ruleset"]).await);
        expected_values.push((round + 1) as f64);
    }
    make_owed_transitions_due(&engine);

    // Every profile holds a datapoint of the SAME statistic, because each run measured the
    // same thing about the same table.
    let statistic = statistic_ids(&engine, &profiles[0])[0].clone();
    for (round, profile) in profiles.iter().enumerate() {
        assert!(
            statistic_ids(&engine, profile).contains(&statistic),
            "round {round}: a second measurement of one thing is a datapoint of one statistic"
        );
    }

    let read = ok(
        &router,
        "GetDataQualityModelResult",
        &json!({ "ProfileId": &profiles[2], "StatisticId": &statistic }),
    )
    .await;
    let model = read
        .get("Model")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("the read reports the model, got {read}"))
        .clone();
    assert_eq!(
        model.len(),
        profiles.len(),
        "the model covers every datapoint of the statistic, got {read}"
    );

    let mean: f64 = expected_values.iter().sum::<f64>() / expected_values.len() as f64;
    for (index, prediction) in model.iter().enumerate() {
        let actual = prediction
            .get("ActualValue")
            .and_then(Value::as_f64)
            .unwrap_or_else(|| panic!("datapoint {index}: expected an ActualValue, got {prediction}"));
        let predicted = prediction
            .get("PredictedValue")
            .and_then(Value::as_f64)
            .unwrap_or_else(|| panic!("datapoint {index}: expected a PredictedValue, got {prediction}"));
        let lower = prediction
            .get("LowerBound")
            .and_then(Value::as_f64)
            .unwrap_or_else(|| panic!("datapoint {index}: expected a LowerBound, got {prediction}"));
        let upper = prediction
            .get("UpperBound")
            .and_then(Value::as_f64)
            .unwrap_or_else(|| panic!("datapoint {index}: expected an UpperBound, got {prediction}"));
        assert!(
            (predicted - mean).abs() < TOLERANCE,
            "datapoint {index}: expected the mean {mean} of the training set, got {predicted}"
        );
        // The invariant the bounds are for: a datapoint the model was trained on is not an
        // anomaly, so it lies inside them.
        assert!(
            lower - TOLERANCE <= actual && actual <= upper + TOLERANCE,
            "datapoint {index}: expected {actual} inside [{lower}, {upper}]"
        );
        assert!(
            prediction.get("Date").is_some(),
            "datapoint {index}: expected the instant its profile was recorded, got {prediction}"
        );
    }

    // Annotating the widest datapoint OUT of the training set moves the prediction: that is
    // what the annotation is for.
    let answer = ok(
        &router,
        "PutDataQualityProfileAnnotation",
        &json!({ "ProfileId": &profiles[2], "InclusionAnnotation": "EXCLUDE" }),
    )
    .await;
    assert_eq!(
        answer,
        json!({}),
        "PutDataQualityProfileAnnotation answers an empty structure, got {answer}"
    );
    let read = ok(
        &router,
        "GetDataQualityModelResult",
        &json!({ "ProfileId": &profiles[0], "StatisticId": &statistic }),
    )
    .await;
    let model = read
        .get("Model")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("the read reports the model, got {read}"))
        .clone();
    let trained_mean: f64 = expected_values[..2].iter().sum::<f64>() / 2.0;
    let predicted = model[0]
        .get("PredictedValue")
        .and_then(Value::as_f64)
        .unwrap_or_else(|| panic!("expected a PredictedValue, got {read}"));
    assert!(
        (predicted - trained_mean).abs() < TOLERANCE,
        "expected the annotated datapoint out of the training set, so the mean {trained_mean}, \
         got {predicted}"
    );
    // The annotated profile's own datapoint is still reported, carrying the annotation.
    let excluded = model
        .iter()
        .find(|prediction| prediction.get("InclusionAnnotation").is_some())
        .unwrap_or_else(|| panic!("the annotated datapoint reports its annotation, got {read}"));
    assert_eq!(
        excluded.get("InclusionAnnotation").and_then(Value::as_str),
        Some("EXCLUDE"),
        "expected the annotation the write stated, got {excluded}"
    );

    // Annotating every profile out leaves a model trained on nothing, which has no result.
    for profile in &profiles {
        ok(
            &router,
            "PutDataQualityProfileAnnotation",
            &json!({ "ProfileId": profile, "InclusionAnnotation": "EXCLUDE" }),
        )
        .await;
    }
    let (status, body) = call(
        &router,
        "GetDataQualityModelResult",
        &json!({ "ProfileId": &profiles[0], "StatisticId": &statistic }),
    )
    .await;
    assert_eq!(
        status, 400,
        "a model trained on nothing predicts nothing, got {status} with {body}"
    );
    assert_eq!(
        error_message(&body),
        messages::DATA_QUALITY_MODEL_RESULT_NOT_FOUND,
        "expected the model-result miss, got {body}"
    );
}

// ---------------------------------------------------------------------------
// Refusals
// ---------------------------------------------------------------------------

/// A `ProfileId` no statistics run of this account minted resolves to no profile, so each of
/// the three operations answers its own modelled miss — the two model reads with the wording
/// they share, the annotation write naming the profile it could not resolve.
///
/// The rows span the WELL-FORMED ids such a miss takes: the `dqprofile-` id of the recorded
/// case, one of another width, and the id of a profile that exists with a character appended.
/// A value outside that grammar is a different refusal
/// ([`a_malformed_profile_id_is_the_modelled_input_refusal`]).
#[tokio::test]
async fn a_profile_id_no_run_minted_is_the_modelled_miss() {
    let (router, engine) = emulator_with_s3();
    // A profile that DOES exist, so the misses below are misses of the address and not of an
    // empty store.
    store_table(&router, "present", "1,ACTIVE\n").await;
    store_ruleset(&router, "present-ruleset", &["RowCount > 0"]).await;
    let present = given_profile(&router, &engine, "present", &["present-ruleset"]).await;
    make_owed_transitions_due(&engine);
    let statistic = statistic_ids(&engine, &present)[0].clone();

    let absent_ids = [
        (
            "a well-formed profile id nothing minted",
            format!("{DATA_QUALITY_PROFILE_ID_PREFIX}0123456789abcdef0123456789abcdef01234567"),
        ),
        (
            "a profile id of another width",
            format!("{DATA_QUALITY_PROFILE_ID_PREFIX}abc123"),
        ),
        (
            "the present profile's id with a character appended",
            format!("{present}0"),
        ),
    ];

    for (case, profile_id) in &absent_ids {
        for (operation, body) in [
            ("GetDataQualityModel", json!({ "ProfileId": profile_id })),
            (
                "GetDataQualityModel with a statistic",
                json!({ "ProfileId": profile_id, "StatisticId": &statistic }),
            ),
            (
                "GetDataQualityModelResult",
                json!({ "ProfileId": profile_id, "StatisticId": &statistic }),
            ),
        ] {
            let name = operation.split_whitespace().next().expect("an operation");
            let (status, answer) = call(&router, name, &body).await;
            assert_eq!(
                status, 400,
                "case {case} through {operation}: expected 400, got {status} with {answer}"
            );
            assert!(
                error_code(&answer).contains("EntityNotFoundException"),
                "case {case} through {operation}: expected EntityNotFoundException, got {answer}"
            );
            assert_eq!(
                error_message(&answer),
                messages::DATA_QUALITY_MODEL_RESULT_NOT_FOUND,
                "case {case} through {operation}: expected the shared model-result miss, got \
                 {answer}"
            );
        }

        // The annotation write names the profile it could not resolve.
        let (status, answer) = call(
            &router,
            "PutDataQualityProfileAnnotation",
            &json!({ "ProfileId": profile_id, "InclusionAnnotation": "INCLUDE" }),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case} through PutDataQualityProfileAnnotation: expected 400, got {status} \
             with {answer}"
        );
        assert!(
            error_code(&answer).contains("EntityNotFoundException"),
            "case {case} through PutDataQualityProfileAnnotation: expected \
             EntityNotFoundException, got {answer}"
        );
        assert_eq!(
            error_message(&answer),
            messages::data_quality_profile_not_found(profile_id),
            "case {case} through PutDataQualityProfileAnnotation: expected the miss naming the \
             profile, got {answer}"
        );
    }

    // The profile that exists is still readable, and nothing above annotated it.
    let read = ok(
        &router,
        "GetDataQualityModel",
        &json!({ "ProfileId": &present }),
    )
    .await;
    assert_eq!(
        read.get("Status").and_then(Value::as_str),
        Some(DATA_QUALITY_MODEL_STATUS_SUCCEEDED),
        "the stored profile's model is untouched by the refusals, got {read}"
    );
}

/// A `StatisticId` the addressed profile holds no datapoint of resolves to no model result
/// either: the rows span an id nothing minted, the id of a statistic of ANOTHER dataset, and
/// the profile's own id offered as a statistic id.
#[tokio::test]
async fn a_statistic_the_profile_does_not_hold_is_the_modelled_miss() {
    let (router, engine) = emulator_with_s3();
    store_table(&router, "narrow", "1,ACTIVE\n").await;
    store_table(&router, "other", "1,ACTIVE\n2,ACTIVE\n").await;
    store_ruleset(&router, "narrow-ruleset", &["ColumnExists \"id\""]).await;
    store_ruleset(&router, "other-ruleset", &["RowCount > 0"]).await;
    // A rule decided off the SCHEMA measures nothing, so this profile holds no datapoint at
    // all — which is also a training that produced no model.
    let schema_only = given_profile(&router, &engine, "narrow", &["narrow-ruleset"]).await;
    let other = given_profile(&router, &engine, "other", &["other-ruleset"]).await;
    make_owed_transitions_due(&engine);
    let other_statistic = statistic_ids(&engine, &other)[0].clone();

    for (case, profile_id, statistic_id) in [
        (
            "an id nothing minted",
            &other,
            "st-ffffffffffffffffffffffffffffffff".to_string(),
        ),
        (
            "a statistic of another dataset",
            &schema_only,
            other_statistic.clone(),
        ),
        ("the profile's own id", &other, other.clone()),
    ] {
        for operation in ["GetDataQualityModel", "GetDataQualityModelResult"] {
            let (status, answer) = call(
                &router,
                operation,
                &json!({ "ProfileId": profile_id, "StatisticId": &statistic_id }),
            )
            .await;
            assert_eq!(
                status, 400,
                "case {case} through {operation}: expected 400, got {status} with {answer}"
            );
            assert!(
                error_code(&answer).contains("EntityNotFoundException"),
                "case {case} through {operation}: expected EntityNotFoundException, got {answer}"
            );
            assert_eq!(
                error_message(&answer),
                messages::DATA_QUALITY_MODEL_RESULT_NOT_FOUND,
                "case {case} through {operation}: expected the shared model-result miss, got \
                 {answer}"
            );
        }
    }

    // A profile holding no datapoint at all: the model over it was trained on nothing, which
    // is a training that produced none and says why.
    let read = ok(
        &router,
        "GetDataQualityModel",
        &json!({ "ProfileId": &schema_only }),
    )
    .await;
    assert_eq!(
        read.get("FailureReason").and_then(Value::as_str),
        Some(messages::DATA_QUALITY_MODEL_HAS_NO_STATISTICS),
        "a profile with no datapoint trains no model, got {read}"
    );
}

/// A `ProfileId` outside the grammar the service mints one with names no profile that could
/// ever exist, so each of the three operations refuses it as a MALFORMED request — a
/// different class from the miss a well-formed id nothing minted earns — and the annotation
/// write stores nothing for it.
///
/// The rows span the forms such a value takes: a bare word, another kind of this service's
/// own id (a data quality run's), a prefix that is nearly the profile one, and the minted
/// form with its prefix removed.
#[tokio::test]
async fn a_malformed_profile_id_is_the_modelled_input_refusal() {
    let (router, engine) = emulator_with_s3();
    store_table(&router, "grammar", "1,ACTIVE\n").await;
    store_ruleset(&router, "grammar-ruleset", &["RowCount > 0"]).await;
    let present = given_profile(&router, &engine, "grammar", &["grammar-ruleset"]).await;
    make_owed_transitions_due(&engine);
    let statistic = statistic_ids(&engine, &present)[0].clone();
    let without_prefix = present
        .strip_prefix(DATA_QUALITY_PROFILE_ID_PREFIX)
        .expect("a minted profile id carries the prefix")
        .to_string();

    for (case, profile_id) in [
        ("a bare word", "profile-1".to_string()),
        ("another kind of this service's ids", "dqrun-abc123".to_string()),
        ("a prefix that is nearly the profile one", "dqprofil-abc123".to_string()),
        ("the minted form without its prefix", without_prefix),
        ("an empty-looking id", "-".to_string()),
    ] {
        for (operation, body) in [
            ("GetDataQualityModel", json!({ "ProfileId": &profile_id })),
            (
                "GetDataQualityModelResult",
                json!({ "ProfileId": &profile_id, "StatisticId": &statistic }),
            ),
            (
                "PutDataQualityProfileAnnotation",
                json!({ "ProfileId": &profile_id, "InclusionAnnotation": "INCLUDE" }),
            ),
        ] {
            let (status, answer) = call(&router, operation, &body).await;
            assert_eq!(
                status, 400,
                "case {case} through {operation}: expected 400, got {status} with {answer}"
            );
            assert!(
                error_code(&answer).contains("InvalidInputException"),
                "case {case} through {operation}: expected InvalidInputException, got {answer}"
            );
            assert_eq!(
                error_message(&answer),
                messages::invalid_data_quality_profile_id(),
                "case {case} through {operation}: expected the malformed-id refusal, got {answer}"
            );
        }
    }

    // The profile that exists is untouched by the refusals: nothing annotated it.
    let read = ok(
        &router,
        "GetDataQualityModelResult",
        &json!({ "ProfileId": &present, "StatisticId": &statistic }),
    )
    .await;
    assert!(
        read.pointer("/Model/0/InclusionAnnotation").is_none(),
        "a refused annotation must annotate nothing, got {read}"
    );
}
