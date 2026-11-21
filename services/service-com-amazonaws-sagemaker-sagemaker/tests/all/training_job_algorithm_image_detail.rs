// SPDX-License-Identifier: CC0-1.0

//! End-to-end wire tests for what `DescribeTrainingJob` reports about the ALGORITHM and the
//! CONTAINER IMAGE a job runs, and for the channel list it echoes.
//!
//! Every case goes over the wire (`POST /` with `X-Amz-Target: SageMaker.<Op>`,
//! `awsJson1_1`) through the same `MultiServiceRouter` the production router uses, on the
//! harness `tests/all/inference_deployment_lifecycle.rs` owns, so what these tests exercise
//! is what a real SDK client gets.
//!
//! The rules under test, each over a TABLE of inputs rather than one call:
//!
//! * the metric definitions a job is reported with are the ALGORITHM's — an algorithm AWS
//!   publishes brings the set it already emits, so a job launched from that algorithm's image
//!   and stating no metrics of its own is reported with that set, while a job running a
//!   container of the caller's own is reported with none and a job stating its own keeps
//!   exactly those;
//! * the image a job runs is reported as the create RESOLVED it — the path as given, that
//!   path at the digest it resolves to, and the instant of the resolution — the same on every
//!   read;
//! * the channel list is the authoritative echo of what the create was given, whatever form
//!   its channels take.

use serde_json::{json, Value};

use crate::inference_deployment_lifecycle::{str_field, Emulator, ACCOUNT};
use crate::training_job_admission::training_body;

/// The metric names SageMaker's built-in gradient-boosted tree algorithm publishes, and the
/// order a read reports them in — the set the service answers for a job launched from that
/// algorithm's image (recorded behaviour; that algorithm's published metric table names the
/// same `<channel>:<metric>` metrics).
///
/// Written out here rather than read from `state::builtin_algorithm_metrics`, so this suite
/// states the expectation independently: a set edited in the implementation fails this test
/// instead of silently moving with it.
const PUBLISHED_METRICS: &[&str] = &[
    "train:mae",
    "train:merror",
    "validation:mae",
    "validation:logloss",
    "validation:macro_precision",
    "train:mlogloss",
    "validation:binary_f_beta",
    "validation:recall",
    "validation:precision",
    "train:error",
    "train:ndcg",
    "validation:map",
    "validation:auc",
    "train:auc",
    "validation:error",
];

/// A digest in the form `ImageDigest` carries (`^[Ss][Hh][Aa]256:[0-9a-fA-F]{64}$`), for the
/// rows that PIN the image they name.
const PINNED_DIGEST: &str =
    "sha256:1111111111111111111111111111111111111111111111111111111111111111";

/// An image path into the Amazon ECR registry `registry` of `region`, which is the form
/// `AlgorithmSpecification$TrainingImage` documents ("SageMaker supports both
/// `registry/repository[:tag]` and `registry/repository[@digest]` image path formats").
fn image_path(registry: &str, region: &str, repository: &str) -> String {
    format!("{registry}.dkr.ecr.{region}.amazonaws.com/{repository}")
}

/// An `AlgorithmSpecification` naming the algorithm by the image path `image`.
fn specification(image: &str) -> Value {
    json!({ "TrainingImage": image, "TrainingInputMode": "File" })
}

/// RULE: the metric definitions a training job is reported with are the ALGORITHM's — for an
/// algorithm AWS itself publishes, the fixed set that algorithm emits, resolved onto the job
/// even though the create supplied none.
#[tokio::test]
async fn a_job_from_a_published_algorithm_image_reports_that_algorithms_metric_set() {
    // Rows differ along what the rule generalises over: which registry (and so which Region)
    // publishes the image, whether the path carries a tag, a digest or neither, and whether
    // the repository names an algorithm AWS publishes at all. The last column is whether the
    // read must report the published metric set.
    let rows: Vec<(&str, String, bool)> = vec![
        (
            "the published algorithm image of one Region",
            format!(
                "{}:1",
                image_path("433757028032", "us-west-2", "xgboost")
            ),
            true,
        ),
        (
            "the published framework image of another Region",
            format!(
                "{}:1.7-1",
                image_path("683313688378", "us-east-1", "sagemaker-xgboost")
            ),
            true,
        ),
        (
            "the same repository with no tag",
            image_path("433757028032", "us-west-2", "xgboost"),
            true,
        ),
        (
            "the same repository pinned by digest",
            format!(
                "{}@{PINNED_DIGEST}",
                image_path("433757028032", "us-west-2", "xgboost")
            ),
            true,
        ),
        (
            "a published algorithm whose metric set this service does not carry",
            format!(
                "{}:1",
                image_path("433757028032", "us-west-2", "linear-learner")
            ),
            false,
        ),
        (
            "a container of the caller's own",
            format!("{}:1", image_path(ACCOUNT, "us-east-1", "trainer")),
            false,
        ),
        (
            "a repository whose name merely starts the same",
            format!("{}:1", image_path(ACCOUNT, "us-east-1", "xgboostish")),
            false,
        ),
    ];

    for (index, (case, image, published)) in rows.into_iter().enumerate() {
        let emulator = Emulator::new();
        let name = format!("metric-set-{index}");
        let body = training_body(&name, specification(&image), "s3://demo/output/");
        emulator.ok("CreateTrainingJob", &body).await;

        let described = emulator
            .ok("DescribeTrainingJob", &json!({ "TrainingJobName": name }))
            .await;
        let reported = described.pointer("/AlgorithmSpecification/MetricDefinitions");
        if !published {
            assert_eq!(
                reported, None,
                "case {case:?}: an image naming no algorithm this service carries the metric \
                 set of must report no metric definitions, got {described:?}"
            );
            continue;
        }
        let reported = reported
            .and_then(Value::as_array)
            .unwrap_or_else(|| panic!("case {case:?}: expected MetricDefinitions in {described}"));
        assert_eq!(
            reported.len(),
            PUBLISHED_METRICS.len(),
            "case {case:?}: one entry per metric the algorithm publishes, got {reported:?}"
        );
        // `MetricDefinitionList` is `@length 0..40` and `MetricRegex` `@length 1..500`, so
        // what is reported is carriable by the members that carry it.
        assert!(
            reported.len() <= 40,
            "case {case:?}: MetricDefinitionList is @length 0..40, got {} entries",
            reported.len()
        );
        for (entry, expected) in reported.iter().zip(PUBLISHED_METRICS) {
            assert_eq!(
                entry.get("Name").and_then(Value::as_str),
                Some(*expected),
                "case {case:?}: expected the metric {expected:?}, got {entry:?}"
            );
            let regex = entry
                .get("Regex")
                .and_then(Value::as_str)
                .unwrap_or_else(|| panic!("case {case:?}: expected a Regex in {entry}"));
            // The expression reads the metric out of the algorithm's own log line, which
            // writes `<channel>-<metric>:<value>` behind the tab an Amazon CloudWatch line
            // carries as `#011`.
            let logged = expected.replace(':', "-");
            assert!(
                regex.contains(&format!("#011{logged}:")),
                "case {case:?}: {expected:?}'s expression must read the logged token \
                 {logged:?}, got {regex:?}"
            );
            assert!(
                !regex.is_empty() && regex.len() <= 500,
                "case {case:?}: MetricRegex is @length 1..500, got {regex:?}"
            );
        }
    }
}

/// RULE: a job that states metric definitions of its own keeps exactly those, whatever
/// algorithm it runs — the published set fills an ABSENT member, it does not override a
/// stated one.
#[tokio::test]
async fn a_job_that_states_its_own_metric_definitions_keeps_exactly_those() {
    // Rows differ along the stated set: one entry, several, and the empty list the member's
    // own bound admits (`MetricDefinitionList` is `@length 0..40`).
    let rows: Vec<(&str, Value)> = vec![
        (
            "one stated metric",
            json!([{ "Name": "objective", "Regex": "objective=(\\S+)" }]),
        ),
        (
            "several stated metrics",
            json!([
                { "Name": "train:loss", "Regex": "loss=(\\S+)" },
                { "Name": "validation:loss", "Regex": "val_loss=(\\S+)" },
            ]),
        ),
        ("no metric at all", json!([])),
    ];

    for (index, (case, stated)) in rows.into_iter().enumerate() {
        let emulator = Emulator::new();
        let name = format!("stated-metrics-{index}");
        // On the published algorithm image, so the stated set is kept against a set the
        // service would otherwise resolve.
        let image = format!(
            "{}:1",
            image_path("433757028032", "us-west-2", "xgboost")
        );
        let mut algorithm = specification(&image);
        algorithm["MetricDefinitions"] = stated.clone();
        let body = training_body(&name, algorithm, "s3://demo/output/");
        emulator.ok("CreateTrainingJob", &body).await;

        let described = emulator
            .ok("DescribeTrainingJob", &json!({ "TrainingJobName": name }))
            .await;
        assert_eq!(
            described.pointer("/AlgorithmSpecification/MetricDefinitions"),
            Some(&stated),
            "case {case:?}: the request's own metric set must be kept, got {described:?}"
        );
    }
}

/// RULE: a training job's read reports the container image it RUNS as the create resolved the
/// reference — the path as given, the same path at the digest it resolved to, and the instant
/// of that resolution — and reports the same resolution on every read.
#[tokio::test]
async fn a_training_jobs_read_reports_the_image_it_runs_as_resolved() {
    // Rows differ along what the rule generalises over: whether the reference carries a tag,
    // a digest or neither, whose registry it names, and whether the job names the image
    // itself or the registered algorithm that trains with it.
    let rows: Vec<(&str, bool, String)> = vec![
        (
            "a published algorithm image with a tag",
            false,
            format!(
                "{}:1",
                image_path("433757028032", "us-west-2", "xgboost")
            ),
        ),
        (
            "a reference that pins the image by digest",
            false,
            format!(
                "{}@{PINNED_DIGEST}",
                image_path("433757028032", "us-west-2", "xgboost")
            ),
        ),
        (
            "a container of the caller's own with no tag",
            false,
            image_path(ACCOUNT, "us-east-1", "trainer"),
        ),
        (
            "the image a registered algorithm trains with",
            true,
            format!("{}:3", image_path(ACCOUNT, "us-east-1", "registered-trainer")),
        ),
    ];

    for (index, (case, by_algorithm, image)) in rows.into_iter().enumerate() {
        let emulator = Emulator::new();
        let name = format!("image-detail-{index}");
        let algorithm = if by_algorithm {
            let algorithm_name = format!("registered-{index}");
            register_algorithm(&emulator, &algorithm_name, &image, None).await;
            json!({ "AlgorithmName": algorithm_name, "TrainingInputMode": "File" })
        } else {
            specification(&image)
        };
        let body = training_body(&name, algorithm, "s3://demo/output/");
        emulator.ok("CreateTrainingJob", &body).await;

        let described = emulator
            .ok("DescribeTrainingJob", &json!({ "TrainingJobName": name }))
            .await;
        let reported = described
            .get("ImageMetadata")
            .unwrap_or_else(|| panic!("case {case:?}: expected ImageMetadata in {described}"));
        assert_eq!(
            reported.get("SpecifiedImage").and_then(Value::as_str),
            Some(image.as_str()),
            "case {case:?}: the image the job runs is reported as specified, got {reported:?}"
        );
        // "the path resolves to a path of the form registry/repository[@digest]"
        // (`DeployedImage`), so the resolved path is the repository the reference names, with
        // its tag or digest suffix replaced by the digest it resolved to.
        let repository = image
            .rsplit_once('/')
            .map(|(registry, component)| {
                let component = component
                    .find(['@', ':'])
                    .map_or(component, |suffix| &component[..suffix]);
                format!("{registry}/{component}")
            })
            .unwrap_or_else(|| image.clone());
        let resolved = reported
            .get("ResolvedImage")
            .and_then(Value::as_str)
            .unwrap_or_else(|| panic!("case {case:?}: expected a ResolvedImage in {reported}"));
        let (path, digest) = resolved
            .rsplit_once('@')
            .unwrap_or_else(|| panic!("case {case:?}: expected a digest path, got {resolved:?}"));
        assert_eq!(
            path, repository,
            "case {case:?}: the resolved path is the repository the reference names, \
             got {resolved:?}"
        );
        assert!(
            digest.strip_prefix("sha256:").is_some_and(|hex| {
                hex.len() == 64 && hex.chars().all(|digit| digit.is_ascii_hexdigit())
            }),
            "case {case:?}: ImageDigest is ^sha256:[0-9a-fA-F]{{64}}$, got {digest:?}"
        );
        // A reference that PINS the image names its digest, so nothing has to be resolved to
        // know it: the reported digest is the pinned one.
        if let Some(pinned) = image.rsplit_once('@').map(|(_, digest)| digest) {
            assert_eq!(
                digest, pinned,
                "case {case:?}: a pinned reference reports the digest it pins, \
                 got {resolved:?}"
            );
        }
        assert!(
            reported.get("ResolutionTime").is_some(),
            "case {case:?}: the instant the image resolved, got {reported:?}"
        );
        // One resolution per job: a second read reports exactly what the first did, so no
        // read re-resolves the image.
        let again = emulator
            .ok("DescribeTrainingJob", &json!({ "TrainingJobName": name }))
            .await;
        assert_eq!(
            again.get("ImageMetadata"),
            Some(reported),
            "case {case:?}: every read reports the one resolution, got {again:?}"
        );
    }
}

/// Register an algorithm through the implemented `CreateAlgorithm`, training with `image` and
/// declaring `metrics` as its training specification's metric set when the row declares one,
/// so a job can name it instead of an image of its own.
async fn register_algorithm(emulator: &Emulator, name: &str, image: &str, metrics: Option<Value>) {
    let mut training = json!({
        "TrainingImage": image,
        "SupportedTrainingInstanceTypes": ["ml.m5.large"],
        "TrainingChannels": [{
            "Name": "train",
            "SupportedContentTypes": ["text/csv"],
            "SupportedInputModes": ["File"],
        }],
    });
    if let Some(metrics) = metrics {
        training["MetricDefinitions"] = metrics;
    }
    let created = emulator
        .ok(
            "CreateAlgorithm",
            &json!({ "AlgorithmName": name, "TrainingSpecification": training }),
        )
        .await;
    assert!(
        !str_field(&created, "AlgorithmArn").is_empty(),
        "the algorithm {name} must be registered, got {created:?}"
    );
}

/// The expression the built-in gradient-boosted tree algorithm's metric `name` is read out of
/// its logs with: the boosting round in brackets, the metric's logged `<channel>-<metric>`
/// token behind the tab a CloudWatch line carries as `#011`, and the value behind it.
fn published_regex(name: &str) -> String {
    let logged = name.replace(':', "-");
    format!(".*\\[[0-9]+\\].*#011{logged}:([-+]?[0-9]*\\.?[0-9]+(?:[eE][-+]?[0-9]+)?).*")
}

/// RULE: a training job a HYPERPARAMETER TUNING JOB launches is a training job of the
/// algorithm its definition names, so it is reported with that algorithm's metric set and with
/// the detail of the image it runs — the same resolution a caller's own `CreateTrainingJob`
/// makes.
#[tokio::test]
async fn a_job_a_tuning_job_launches_reports_the_algorithm_and_image_it_runs() {
    let declared = json!([{ "Name": "validation:rmse", "Regex": "rmse: ([0-9.]+)" }]);
    /// One row: the case, whether the definition names a REGISTERED algorithm rather than an
    /// image, the image the child ends up running, what that registration declares as its
    /// metric set, and the metric set the launched job must be reported with.
    struct Row {
        case: &'static str,
        by_algorithm: bool,
        image: String,
        registered: Option<Value>,
        expected: Option<Value>,
    }
    // Rows differ along how the definition names its algorithm — the two ways
    // `HyperParameterAlgorithmSpecification` can name what the children run: a published
    // algorithm image, a registered algorithm that declares a metric set, and one that
    // declares none.
    let rows: Vec<Row> = vec![
        Row {
            case: "a definition naming the published algorithm image",
            by_algorithm: false,
            image: format!("{}:1", image_path("433757028032", "us-west-2", "xgboost")),
            registered: None,
            expected: Some(Value::Array(
                PUBLISHED_METRICS
                    .iter()
                    .map(|name| json!({ "Name": name, "Regex": published_regex(name) }))
                    .collect(),
            )),
        },
        Row {
            case: "a definition naming a registered algorithm with its own metric set",
            by_algorithm: true,
            image: format!("{}:2", image_path(ACCOUNT, "us-east-1", "tuned-trainer")),
            registered: Some(declared.clone()),
            expected: Some(declared),
        },
        Row {
            case: "a definition naming a registered algorithm that declares none",
            by_algorithm: true,
            image: format!("{}:2", image_path(ACCOUNT, "us-east-1", "tuned-trainer")),
            registered: None,
            expected: None,
        },
    ];

    for (index, row) in rows.into_iter().enumerate() {
        let Row {
            case,
            by_algorithm,
            image,
            registered,
            expected,
        } = row;
        let emulator = Emulator::new();
        let tuning = format!("tuning-{index}");
        let mut body = crate::hyperparameter_tuning_job::valid_body(&tuning);
        let algorithm = if by_algorithm {
            let algorithm_name = format!("tuned-algo-{index}");
            register_algorithm(&emulator, &algorithm_name, &image, registered).await;
            json!({ "AlgorithmName": algorithm_name, "TrainingInputMode": "File" })
        } else {
            specification(&image)
        };
        body["TrainingJobDefinition"]["AlgorithmSpecification"] = algorithm;
        emulator.ok("CreateHyperParameterTuningJob", &body).await;

        // The jobs the tuning job launched are training jobs like any other, so the
        // implemented listing reports them; each carries the tuning job's name as its own
        // prefix (`state::child_training_job_name`).
        let listed = emulator.ok("ListTrainingJobs", &json!({})).await;
        let child = listed
            .get("TrainingJobSummaries")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|summary| summary.get("TrainingJobName").and_then(Value::as_str))
            .find(|name| name.starts_with(&tuning))
            .unwrap_or_else(|| {
                panic!("case {case:?}: expected a launched training job in {listed}")
            })
            .to_string();

        let described = emulator
            .ok("DescribeTrainingJob", &json!({ "TrainingJobName": child }))
            .await;
        assert_eq!(
            described.pointer("/AlgorithmSpecification/MetricDefinitions"),
            expected.as_ref(),
            "case {case:?}: the launched job reports the metric set of the algorithm it runs, \
             got {described:?}"
        );
        assert_eq!(
            described
                .pointer("/ImageMetadata/SpecifiedImage")
                .and_then(Value::as_str),
            Some(image.as_str()),
            "case {case:?}: the launched job reports the image it runs, got {described:?}"
        );
    }
}

/// One channel addressing an Amazon S3 prefix, named `name`.
fn s3_channel(name: &str) -> Value {
    json!({
        "ChannelName": name,
        "DataSource": {
            "S3DataSource": {
                "S3DataType": "S3Prefix",
                "S3Uri": format!("s3://demo/{name}/"),
                "S3DataDistributionType": "FullyReplicated",
            },
        },
        "ContentType": "text/csv",
    })
}

/// RULE: `InputDataConfig` is the authoritative echo of what `CreateTrainingJob` was given,
/// so every describe of the job reports the submitted channel list — whatever each channel
/// addresses, however many there are, and as an empty list for a job given none.
#[tokio::test]
async fn a_training_jobs_read_echoes_the_channel_list_it_was_created_with() {
    // Rows differ along what the rule generalises over: how a channel addresses its data
    // (an S3 prefix, a manifest, an augmented manifest with attribute names, a file system),
    // the optional members it carries, and how many channels the list holds — none, one,
    // several and the twenty `InputDataConfig` is `@length 1..20` for.
    let rows: Vec<(&str, Vec<Value>)> = vec![
        ("no channel at all", Vec::new()),
        ("one s3 prefix channel", vec![s3_channel("train")]),
        (
            "a manifest channel, compressed and record-wrapped",
            vec![json!({
                "ChannelName": "train",
                "DataSource": {
                    "S3DataSource": {
                        "S3DataType": "ManifestFile",
                        "S3Uri": "s3://demo/train.manifest",
                        "S3DataDistributionType": "ShardedByS3Key",
                    },
                },
                "ContentType": "application/x-recordio-protobuf",
                "CompressionType": "Gzip",
                "RecordWrapperType": "RecordIO",
                "InputMode": "Pipe",
            })],
        ),
        (
            "an augmented manifest channel with attribute names and a shuffle seed",
            vec![json!({
                "ChannelName": "train",
                "DataSource": {
                    "S3DataSource": {
                        "S3DataType": "AugmentedManifestFile",
                        "S3Uri": "s3://demo/train.manifest",
                        "AttributeNames": ["source-ref", "label"],
                    },
                },
                "InputMode": "Pipe",
                "ShuffleConfig": { "Seed": 42 },
            })],
        ),
        (
            "a file-system channel",
            vec![json!({
                "ChannelName": "train",
                "DataSource": {
                    "FileSystemDataSource": {
                        "FileSystemId": "fs-0123456789abcdef0",
                        "FileSystemAccessMode": "ro",
                        "FileSystemType": "EFS",
                        "DirectoryPath": "/datasets/train",
                    },
                },
                "ContentType": "text/csv",
            })],
        ),
        (
            "three channels",
            vec![
                s3_channel("train"),
                s3_channel("validation"),
                s3_channel("test"),
            ],
        ),
        (
            "the twenty channels the list admits",
            (0..20)
                .map(|index| s3_channel(&format!("channel-{index}")))
                .collect(),
        ),
    ];

    for (index, (case, channels)) in rows.into_iter().enumerate() {
        let emulator = Emulator::new();
        let name = format!("channel-echo-{index}");
        let image = format!("{}:1", image_path(ACCOUNT, "us-east-1", "trainer"));
        let mut body = training_body(&name, specification(&image), "s3://demo/output/");
        if channels.is_empty() {
            // A row that declares no channel sends NO `InputDataConfig`: the member is
            // optional, and `@length 1..20` would refuse an empty list on the REQUEST.
            body.as_object_mut()
                .expect("a JSON object body")
                .remove("InputDataConfig");
        } else {
            body["InputDataConfig"] = Value::Array(channels.clone());
        }
        emulator.ok("CreateTrainingJob", &body).await;

        let described = emulator
            .ok("DescribeTrainingJob", &json!({ "TrainingJobName": name }))
            .await;
        let reported = described
            .get("InputDataConfig")
            .and_then(Value::as_array)
            .unwrap_or_else(|| panic!("case {case:?}: expected InputDataConfig in {described}"));
        assert_eq!(
            reported.len(),
            channels.len(),
            "case {case:?}: every submitted channel must be reported, and only those, \
             got {described:?}"
        );
        for (position, (sent, reported)) in channels.iter().zip(reported).enumerate() {
            assert_submitted_members_echoed(
                sent,
                reported,
                &format!("case {case:?} channel {position}"),
            );
        }
    }
}

/// Assert that every value `submitted` carries is reported at the same place in `reported`,
/// so the echo loses nothing. The reverse is deliberately NOT asserted: a read also reports
/// what the service RESOLVED for the channel (its record wrapping, the distribution of its S3
/// data, the mode it reads in), which the submission did not carry.
fn assert_submitted_members_echoed(submitted: &Value, reported: &Value, at: &str) {
    match submitted {
        Value::Object(members) => {
            for (member, value) in members {
                let reported = reported.get(member).unwrap_or_else(|| {
                    panic!("{at}: {member} must be echoed, got {reported:?}")
                });
                assert_submitted_members_echoed(value, reported, &format!("{at}.{member}"));
            }
        }
        Value::Array(entries) => {
            let reported = reported
                .as_array()
                .unwrap_or_else(|| panic!("{at}: expected a list, got {reported:?}"));
            assert_eq!(
                reported.len(),
                entries.len(),
                "{at}: every submitted entry must be echoed, got {reported:?}"
            );
            for (index, (entry, reported)) in entries.iter().zip(reported).enumerate() {
                assert_submitted_members_echoed(entry, reported, &format!("{at}[{index}]"));
            }
        }
        scalar => assert_eq!(
            reported, scalar,
            "{at}: must be echoed as submitted, got {reported:?}"
        ),
    }
}
