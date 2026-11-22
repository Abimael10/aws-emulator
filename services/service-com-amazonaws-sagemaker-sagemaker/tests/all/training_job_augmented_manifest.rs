// SPDX-License-Identifier: CC0-1.0

//! What a training job whose channel carries an AUGMENTED MANIFEST does: the records it builds
//! out of that manifest's JSON lines, and the metric the completed job reports for the channel
//! they came from.
//!
//! Driven end to end through the same harness `training_job_execution` builds — SageMaker,
//! Amazon S3 and IAM on ONE shared engine, every call travelling the real wire — so what is
//! asserted is what an SDK client would see.
//!
//! RULE ONE — a channel whose `S3DataSource.S3DataType` is `AugmentedManifestFile` carries its
//! records INSIDE the object the location names: "`S3Uri` identifies an object that is an
//! augmented manifest file in JSON lines format. This file contains the data you want to use for
//! model training" (`S3DataSource$S3DataType`), out of which the job takes
//! `S3DataSource.AttributeNames` — "a list of one or more attribute names to use that are found
//! in a specified augmented manifest file" — in order, the last of them the record's label. So
//! such a channel's records are the manifest's own lines, and a line that is not of that form is
//! counted rather than guessed at.
//!
//! RULE TWO — the metric a completed job reports per channel is the one the ALGORITHM its
//! `AlgorithmSpecification` names emits for the learning task its `HyperParameters` declare
//! (`MetricData`: "the name, value, and date and time of a metric that was emitted to Amazon
//! CloudWatch"), named `<ChannelName>:<metric>`: the built-in text algorithm's supervised mode
//! is a classification, so it emits `accuracy`, while the boosted-tree algorithm emits the
//! metric its objective implies and a container of the caller's own what this emulator's
//! default train instructions measured.
//!
//! RULE THREE — every metric VALUE is measured over the records the run really read: the model
//! the artifact carries, applied to the manifest's own lines, gives exactly the number the job
//! reports.
//!
//! The table spans what the rules generalise over: the attribute count and the attribute names,
//! whether the channel names attributes at all, the value form (text, a number, a class named in
//! text), how many channels carry a manifest, which algorithm the job names, and whether every
//! line of the manifest holds a record.
//!
//! Nothing here waits for a fixed time: every wait is a poll that looks again until the job
//! reaches a status nothing leaves on its own, or a generous deadline passes.

use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::training_job_execution::{read_and_write, str_field, training_body, Emulator, BUCKET};

/// The statuses nothing leaves on its own, so a poll that reaches one is over.
const TERMINAL: [&str; 3] = ["Completed", "Failed", "Stopped"];

/// How long a poll gives the clock to settle a job: far longer than the span the state gives a
/// job's work, so a loaded host still settles well inside it.
const SETTLE_DEADLINE: Duration = Duration::from_secs(120);

/// How often a poll looks again. Only the loop's condition decides when it stops.
const POLL_INTERVAL: Duration = Duration::from_millis(100);

/// The width of the record one TEXT attribute contributes, out of the crate's own state rather
/// than restated here.
const TEXT_WIDTH: usize =
    service_com_amazonaws_sagemaker_sagemaker::state::TEXT_FEATURE_WIDTH;

/// The region every image path below is scoped to, matching the harness's own.
const REGION: &str = "us-east-1";

/// An image path naming the built-in algorithm published under `repository`: SageMaker publishes
/// each built-in algorithm as an image whose repository is the algorithm's own name ("Docker
/// registry paths and example code").
fn builtin_image(repository: &str) -> String {
    format!("811284229777.dkr.ecr.{REGION}.amazonaws.com/{repository}:1")
}

/// One channel carrying an augmented manifest: the data type the manifest is addressed as, the
/// attributes the channel takes out of its lines, and the `Pipe` input mode that data type "can
/// only be used if the Channel's input mode is" — with the record framing SageMaker hands the
/// container the records in.
fn manifest_channel(name: &str, uri: &str, attributes: &[&str]) -> Value {
    let mut source = json!({
        "S3DataType": "AugmentedManifestFile",
        "S3Uri": uri,
        "S3DataDistributionType": "FullyReplicated",
    });
    if !attributes.is_empty() {
        source["AttributeNames"] = json!(attributes);
    }
    json!({
        "ChannelName": name,
        "DataSource": { "S3DataSource": source },
        "RecordWrapperType": "RecordIO",
        "InputMode": "Pipe",
    })
}

/// A `CreateTrainingJob` body naming `image` as its algorithm, `parameters` as its
/// hyperparameters and `channels` as its input data configuration, in the `Pipe` input mode an
/// augmented manifest is read in.
fn manifest_body(
    job: &str,
    role: &str,
    image: &str,
    parameters: Value,
    channels: Value,
    output: &str,
) -> Value {
    let mut body = training_body(job, role, channels, output);
    body["AlgorithmSpecification"] = json!({
        "TrainingImage": image,
        "TrainingInputMode": "Pipe",
    });
    body["HyperParameters"] = parameters;
    body
}

/// The sentences each class of the text manifests below is written in: one class's records carry
/// its own words, so a model fitted over them has something to separate the classes by.
const CLASS_WORDS: [&str; 2] = ["alpha beta gamma", "delta epsilon zeta"];

/// `rows` JSON lines of the form a text classification job's augmented manifest carries: the
/// sentence under `source`, and the class under the attribute `label_name` — as the number of
/// the class when `named` is false, and as the class's own name when it is true.
fn text_manifest(rows: usize, label_name: &str, named: bool) -> String {
    (0..rows)
        .map(|row| {
            let class = row % CLASS_WORDS.len();
            let label = if named {
                json!(format!("class-{class}"))
            } else {
                json!(class + 1)
            };
            format!(
                "{}\n",
                json!({ "source": CLASS_WORDS[class], label_name: label })
            )
        })
        .collect()
}

/// `rows` JSON lines whose attributes are numbers: two features and the target they add up to.
fn numeric_manifest(rows: usize) -> String {
    (0..rows)
        .map(|row| {
            let x = row as f64;
            let y = x * 2.0;
            format!("{}\n", json!({ "x": x, "y": y, "target": 3.0 + x + y }))
        })
        .collect()
}

/// `rows` JSON lines of ONE feature attribute and one label attribute, named so that the label
/// is the last attribute whatever order the attributes are read in.
fn two_attribute_manifest(rows: usize) -> String {
    (0..rows)
        .map(|row| {
            let feature = row as f64;
            format!(
                "{}\n",
                json!({ "feature": feature, "label": (row % 2) as f64 })
            )
        })
        .collect()
}

/// Poll `DescribeTrainingJob` until `job` reaches a status nothing leaves on its own.
async fn poll_until_terminal(emulator: &Emulator, job: &str, case: &str) -> Value {
    let deadline = Instant::now() + SETTLE_DEADLINE;
    loop {
        let described = emulator
            .sagemaker_ok("DescribeTrainingJob", &json!({ "TrainingJobName": job }))
            .await;
        if TERMINAL.contains(&str_field(&described, "TrainingJobStatus").as_str()) {
            return described;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the job {job} must reach a terminal status, last read {described}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The `(MetricName, Value)` pairs a describe reports, in the order it reports them.
fn reported_metrics(described: &Value) -> Vec<(String, f64)> {
    described
        .get("FinalMetricDataList")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|metric| {
            (
                str_field(metric, "MetricName"),
                metric.get("Value").and_then(Value::as_f64).unwrap_or(f64::NAN),
            )
        })
        .collect()
}

/// The document the model archive holds: the gzip-compressed tar is decompressed and the first
/// entry that parses as JSON is read, so what is asserted is what a client reading the artifact
/// out of Amazon S3 would read — whatever file name the algorithm's container writes it under.
fn artifact_document(archive: &[u8]) -> Value {
    use std::io::Read;

    let decoder = flate2::read::GzDecoder::new(archive);
    let mut tar = tar::Archive::new(decoder);
    for entry in tar.entries().expect("the archive lists its entries") {
        let mut entry = entry.expect("an archive entry");
        let mut body = String::new();
        if entry.read_to_string(&mut body).is_err() {
            continue;
        }
        if let Ok(document) = serde_json::from_str::<Value>(&body) {
            return document;
        }
    }
    panic!("the archive holds no model document: {} bytes", archive.len());
}

/// The artifact of `job` as the completed describe names it, read back out of Amazon S3.
async fn artifact_of(emulator: &Emulator, described: &Value, case: &str) -> Value {
    let uri = described
        .pointer("/ModelArtifacts/S3ModelArtifacts")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let key = uri
        .strip_prefix(&format!("s3://{BUCKET}/"))
        .unwrap_or_else(|| {
            panic!("case {case:?}: the artifact must be in the harness bucket, got {uri:?}")
        })
        .to_string();
    let archive = emulator
        .object_bytes(BUCKET, &key)
        .await
        .unwrap_or_else(|| panic!("case {case:?}: Amazon S3 must hold the artifact {uri}"));
    artifact_document(&archive)
}

/// What one channel's entry of the artifact reports: the records read out of it and the rows
/// that held none.
fn channel_counts(document: &Value) -> Vec<(String, u64, u64)> {
    document
        .get("channels")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|entry| {
            (
                str_field(entry, "name"),
                entry.get("records").and_then(Value::as_u64).unwrap_or(0),
                entry
                    .get("skippedRows")
                    .and_then(Value::as_u64)
                    .unwrap_or(0),
            )
        })
        .collect()
}

/// RULE ONE and RULE TWO: an augmented-manifest channel's records are the manifest's own JSON
/// lines, and the completed job reports the metric its algorithm emits for each channel they
/// came from.
#[tokio::test]
async fn an_augmented_manifest_channel_trains_on_the_lines_the_manifest_holds() {
    /// One case: the job, the algorithm image and hyperparameters, the channels, and what the
    /// completed job must report.
    struct Row {
        case: &'static str,
        job: &'static str,
        image: String,
        parameters: Value,
        channels: Value,
        /// The metric names the job must report, in order.
        metrics: Vec<String>,
        /// Per channel: its name, the records read out of its manifest, and the lines that held
        /// none.
        counts: Vec<(&'static str, u64, u64)>,
        /// How many weights the fitted model must carry, i.e. the width of the records.
        features: usize,
    }

    let emulator = Emulator::new();
    emulator.given_bucket(BUCKET).await;
    let role = emulator
        .given_role("sagemaker-manifest-role", Some(&read_and_write()))
        .await;

    // The manifests every case below reads: a text manifest with numeric labels, one whose
    // classes are named in text, a pair for a train/validation split, one of numbers only, one
    // whose attributes the channel does not name, and one holding lines that are no record.
    emulator
        .given_object(BUCKET, "manifest/train.jsonl", &text_manifest(60, "label", false))
        .await;
    emulator
        .given_object(BUCKET, "manifest/classes.jsonl", &text_manifest(20, "class", true))
        .await;
    emulator
        .given_object(BUCKET, "manifest/split-train.jsonl", &text_manifest(30, "label", false))
        .await;
    emulator
        .given_object(
            BUCKET,
            "manifest/split-validation.jsonl",
            &text_manifest(10, "label", false),
        )
        .await;
    emulator
        .given_object(BUCKET, "manifest/numbers.jsonl", &numeric_manifest(12))
        .await;
    emulator
        .given_object(BUCKET, "manifest/unnamed.jsonl", &two_attribute_manifest(8))
        .await;
    emulator
        .given_object(
            BUCKET,
            "manifest/ragged.jsonl",
            &format!(
                "{}not json\n{}\n{}",
                text_manifest(5, "label", false),
                json!({ "source": "alpha beta gamma" }),
                text_manifest(5, "label", false),
            ),
        )
        .await;

    let text_image = builtin_image("blazingtext");
    let supervised = json!({
        "mode": "supervised",
        "epochs": "5",
        "min_count": "1",
        "word_ngrams": "1",
        "vector_dim": "10",
    });

    let rows = vec![
        Row {
            case: "a text manifest and a numeric label, in the supervised text mode",
            job: "manifest-text",
            image: text_image.clone(),
            parameters: supervised.clone(),
            channels: json!([manifest_channel(
                "train",
                &format!("s3://{BUCKET}/manifest/train.jsonl"),
                &["source", "label"]
            )]),
            metrics: vec!["train:accuracy".to_string()],
            counts: vec![("train", 60, 0)],
            features: TEXT_WIDTH,
        },
        Row {
            case: "the classes named in text rather than numbered",
            job: "manifest-classes",
            image: text_image.clone(),
            parameters: supervised.clone(),
            channels: json!([manifest_channel(
                "train",
                &format!("s3://{BUCKET}/manifest/classes.jsonl"),
                &["source", "class"]
            )]),
            metrics: vec!["train:accuracy".to_string()],
            counts: vec![("train", 20, 0)],
            features: TEXT_WIDTH,
        },
        Row {
            case: "a train manifest beside a validation manifest",
            job: "manifest-split",
            image: text_image.clone(),
            parameters: supervised.clone(),
            channels: json!([
                manifest_channel(
                    "train",
                    &format!("s3://{BUCKET}/manifest/split-train.jsonl"),
                    &["source", "label"]
                ),
                manifest_channel(
                    "validation",
                    &format!("s3://{BUCKET}/manifest/split-validation.jsonl"),
                    &["source", "label"]
                ),
            ]),
            metrics: vec![
                "train:accuracy".to_string(),
                "validation:accuracy".to_string(),
            ],
            counts: vec![("train", 30, 0), ("validation", 10, 0)],
            features: TEXT_WIDTH,
        },
        Row {
            case: "a manifest of numeric attributes under the boosted-trees algorithm",
            job: "manifest-numbers",
            image: builtin_image("xgboost"),
            parameters: json!({ "objective": "reg:squarederror", "num_round": "10" }),
            channels: json!([manifest_channel(
                "train",
                &format!("s3://{BUCKET}/manifest/numbers.jsonl"),
                &["x", "y", "target"]
            )]),
            metrics: vec!["train:rmse".to_string()],
            counts: vec![("train", 12, 0)],
            features: 2,
        },
        Row {
            case: "a channel naming no attribute, so every attribute of the line is taken",
            job: "manifest-unnamed",
            image: format!("000000000000.dkr.ecr.{REGION}.amazonaws.com/my-trainer:1"),
            parameters: json!({}),
            channels: json!([manifest_channel(
                "train",
                &format!("s3://{BUCKET}/manifest/unnamed.jsonl"),
                &[]
            )]),
            metrics: vec!["train:rmse".to_string()],
            counts: vec![("train", 8, 0)],
            features: 1,
        },
        Row {
            case: "a manifest holding lines that are no record of what the channel named",
            job: "manifest-ragged",
            image: text_image.clone(),
            parameters: supervised.clone(),
            channels: json!([manifest_channel(
                "train",
                &format!("s3://{BUCKET}/manifest/ragged.jsonl"),
                &["source", "label"]
            )]),
            metrics: vec!["train:accuracy".to_string()],
            counts: vec![("train", 10, 2)],
            features: TEXT_WIDTH,
        },
    ];

    // Every job is submitted first, so the clock settles them alongside one another rather than
    // one span per case.
    let output = format!("s3://{BUCKET}/manifest-output/");
    for row in &rows {
        let body = manifest_body(
            row.job,
            &role,
            &row.image,
            row.parameters.clone(),
            row.channels.clone(),
            &output,
        );
        emulator.sagemaker_ok("CreateTrainingJob", &body).await;
    }

    for row in &rows {
        let settled = poll_until_terminal(&emulator, row.job, row.case).await;
        assert_eq!(
            (
                str_field(&settled, "TrainingJobStatus"),
                settled.get("FailureReason").cloned(),
            ),
            ("Completed".to_string(), None),
            "case {:?}: the job must complete with no failure, got {settled}",
            row.case
        );

        let metrics = reported_metrics(&settled);
        let names: Vec<String> = metrics.iter().map(|(name, _)| name.clone()).collect();
        assert_eq!(
            names, row.metrics,
            "case {:?}: expected the metrics {:?}, got {settled}",
            row.case, row.metrics
        );
        for (name, value) in &metrics {
            assert!(
                value.is_finite(),
                "case {:?}: the metric {name:?} must carry the value it measured, got {settled}",
                row.case
            );
            // A share of the records is a share: a metric named for the classification accuracy
            // cannot report a number outside the unit interval.
            if name.ends_with(":accuracy") {
                assert!(
                    (0.0..=1.0).contains(value),
                    "case {:?}: the accuracy {name:?} must lie in [0, 1], got {value}",
                    row.case
                );
            }
        }

        let document = artifact_of(&emulator, &settled, row.case).await;
        let counts = channel_counts(&document);
        let expected: Vec<(String, u64, u64)> = row
            .counts
            .iter()
            .map(|(name, records, skipped)| ((*name).to_string(), *records, *skipped))
            .collect();
        assert_eq!(
            counts, expected,
            "case {:?}: the records the run read out of the manifest, got {document}",
            row.case
        );
        let weights = document
            .pointer("/model/weights")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        assert_eq!(
            weights.len(),
            row.features,
            "case {:?}: the fitted model must carry one weight per feature of the record, got \
             {document}",
            row.case
        );
    }
}

/// RULE THREE: the metric value is MEASURED over the records the manifest holds — the model the
/// artifact carries, applied to those same lines, gives exactly the number the job reported.
#[tokio::test]
async fn the_reported_accuracy_is_measured_over_the_manifests_own_records() {
    let emulator = Emulator::new();
    emulator.given_bucket(BUCKET).await;
    let role = emulator
        .given_role("sagemaker-measured-role", Some(&read_and_write()))
        .await;

    let job = "manifest-measured";
    let output = format!("s3://{BUCKET}/measured-output/");
    let manifest = text_manifest(24, "label", false);
    emulator
        .given_object(BUCKET, "measured/train.jsonl", &manifest)
        .await;
    emulator
        .sagemaker_ok(
            "CreateTrainingJob",
            &manifest_body(
                job,
                &role,
                &builtin_image("blazingtext"),
                json!({ "mode": "supervised", "epochs": "5" }),
                json!([manifest_channel(
                    "train",
                    &format!("s3://{BUCKET}/measured/train.jsonl"),
                    &["source", "label"]
                )]),
                &output,
            ),
        )
        .await;

    let case = "the accuracy measured over the manifest's lines";
    let settled = poll_until_terminal(&emulator, job, case).await;
    assert_eq!(
        str_field(&settled, "TrainingJobStatus"),
        "Completed",
        "case {case:?}: the job must complete, got {settled}"
    );
    let reported = reported_metrics(&settled);
    let (name, value) = reported
        .first()
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: expected one metric, got {settled}"));
    assert_eq!(
        name, "train:accuracy",
        "case {case:?}: the supervised text mode emits the channel's accuracy, got {settled}"
    );

    // The model the artifact carries, applied to the manifest's own lines through the same
    // reading of a text attribute the service took ([`state::text_features`]): the share of the
    // records whose class it predicts right is the number the job reported.
    let document = artifact_of(&emulator, &settled, case).await;
    let intercept = document
        .pointer("/model/intercept")
        .and_then(Value::as_f64)
        .unwrap_or_else(|| panic!("case {case:?}: expected a fitted intercept in {document}"));
    let weights: Vec<f64> = document
        .pointer("/model/weights")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|weight| weight.as_f64().unwrap_or(f64::NAN))
        .collect();
    let mut right = 0.0_f64;
    let mut records = 0.0_f64;
    for line in manifest.lines().filter(|line| !line.trim().is_empty()) {
        let parsed: Value = serde_json::from_str(line).expect("a manifest line is JSON");
        let features = service_com_amazonaws_sagemaker_sagemaker::state::text_features(
            parsed.get("source").and_then(Value::as_str).unwrap_or_default(),
        );
        let label = parsed.get("label").and_then(Value::as_f64).unwrap_or(f64::NAN);
        let prediction = intercept
            + weights
                .iter()
                .zip(&features)
                .map(|(weight, feature)| weight * feature)
                .sum::<f64>();
        if prediction.round() == label.round() {
            right += 1.0;
        }
        records += 1.0;
    }
    let measured = right / records;
    assert!(
        (value - measured).abs() < 1e-6,
        "case {case:?}: expected the accuracy {measured} the model measures over the \
         manifest's {records} records, got {value}"
    );
}
