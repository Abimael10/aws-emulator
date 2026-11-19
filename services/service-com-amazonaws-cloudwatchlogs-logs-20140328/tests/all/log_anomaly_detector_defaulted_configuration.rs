// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the configuration a created log anomaly detector RUNS with, and the
//! status it runs in, as BOTH read paths report it — `GetLogAnomalyDetector` for one detector
//! and `ListLogAnomalyDetectors` for the collection.
//!
//! The rule under test: a created detector begins its lifecycle TRAINING on the log group it
//! watches and only the CLOCK carries it on to ANALYZING, and the two configuration members
//! the service defaults — `evaluationFrequency` and `anomalyVisibilityTime` — are present on
//! every read of it, carrying the value the create named or, when the create named none, the
//! value the service runs the detector with. Both read paths report ONE stored record, so an
//! element of the collection carries exactly what the single-detector read reports for the
//! same ARN.
//!
//! Every case drives the service the way a client does — `POST /` with
//! `X-Amz-Target: Logs_20140328.<Operation>` through the router that mounts it — so a
//! response that is not computed from the shared store cannot pass.
//!
//! The table spans what the rule generalises over rather than one request: a create naming
//! neither defaulted member, each one on its own, and both together; EVERY value of the
//! model's `EvaluationFrequency` enum (source:
//! `smithy_query(query="shape",shape="EvaluationFrequency")`); and `anomalyVisibilityTime` at
//! both published bounds of its `@range(min: 7, max: 90)` and one between them. Each row
//! watches its own log group, so the collection read is exercised both unfiltered and through
//! `filterLogGroupArn`.

use serde_json::{json, Map, Value};

use service_com_amazonaws_cloudwatchlogs_logs_20140328::state::{
    log_group_arn, ANOMALY_DETECTOR_STATUS_ANALYZING, ANOMALY_DETECTOR_STATUS_TRAINING,
    DEFAULT_ANOMALY_VISIBILITY_TIME_DAYS, DEFAULT_EVALUATION_FREQUENCY,
};

use crate::harness::{admissible_detector_statuses, now_ms, Logs, REGION};

/// The operations under test: the create, and the two reads of what it stored.
const CREATE: &str = "CreateLogAnomalyDetector";
const GET: &str = "GetLogAnomalyDetector";
const LIST: &str = "ListLogAnomalyDetectors";

/// Every value of the model's `EvaluationFrequency` enum, in the order the model lists them.
/// Source: `smithy_query(query="shape",shape="EvaluationFrequency")`.
const EVALUATION_FREQUENCIES: [&str; 6] = [
    "ONE_MIN",
    "FIVE_MIN",
    "TEN_MIN",
    "FIFTEEN_MIN",
    "THIRTY_MIN",
    "ONE_HOUR",
];

/// The published bounds of `AnomalyVisibilityTime`'s `@range(min: 7, max: 90)`, and one value
/// between them.
const VISIBILITY_TIMES: [i64; 3] = [7, 21, 90];

/// How long a poll for the status the clock owes keeps looking, and how long it waits between
/// looks: a generous deadline, because the case is about WHETHER the clock gets there and
/// never about how fast this host runs.
const POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(25);
const POLL_ATTEMPTS: usize = 2_400;

/// One accepted create: which of the two defaulted members it names, if either.
struct Row {
    /// What this row varies, for the assertion messages.
    case: String,
    evaluation_frequency: Option<&'static str>,
    anomaly_visibility_time: Option<i64>,
}

/// The corpus: neither defaulted member named, each on its own over every value the model
/// admits for it, and both together.
fn rows() -> Vec<Row> {
    let mut rows = vec![Row {
        case: "neither defaulted member named".to_string(),
        evaluation_frequency: None,
        anomaly_visibility_time: None,
    }];
    for frequency in EVALUATION_FREQUENCIES {
        rows.push(Row {
            case: format!("only the evaluation frequency {frequency}"),
            evaluation_frequency: Some(frequency),
            anomaly_visibility_time: None,
        });
    }
    for days in VISIBILITY_TIMES {
        rows.push(Row {
            case: format!("only the anomaly visibility time {days}"),
            evaluation_frequency: None,
            anomaly_visibility_time: Some(days),
        });
    }
    rows.push(Row {
        case: "both defaulted members named".to_string(),
        evaluation_frequency: Some("THIRTY_MIN"),
        anomaly_visibility_time: Some(45),
    });
    rows
}

impl Row {
    /// The `evaluationFrequency` the detector of this row runs with: the one the create named,
    /// or the service's default when it named none.
    fn expected_frequency(&self) -> &str {
        self.evaluation_frequency
            .unwrap_or(DEFAULT_EVALUATION_FREQUENCY)
    }

    /// The `anomalyVisibilityTime` the detector of this row runs with.
    fn expected_visibility(&self) -> i64 {
        self.anomaly_visibility_time
            .unwrap_or(DEFAULT_ANOMALY_VISIBILITY_TIME_DAYS)
    }

    /// The create body: the log group it watches plus only the members this row names, so a
    /// member this row leaves to the service is absent on the wire too.
    fn create_body(&self, group_arn: &str) -> Value {
        let mut body = Map::new();
        body.insert("logGroupArnList".to_string(), json!([group_arn]));
        if let Some(frequency) = self.evaluation_frequency {
            body.insert("evaluationFrequency".to_string(), json!(frequency));
        }
        if let Some(days) = self.anomaly_visibility_time {
            body.insert("anomalyVisibilityTime".to_string(), json!(days));
        }
        Value::Object(body)
    }
}

/// One string member of a response, or `None` when the response does not carry it.
fn member<'a>(body: &'a Value, key: &str) -> Option<&'a str> {
    body.get(key).and_then(Value::as_str)
}

/// The detectors one `ListLogAnomalyDetectors` request reports. Every case holds far fewer
/// detectors than one page carries, so a single call sees all of them.
async fn listed(logs: &Logs, request: &Value) -> Vec<Value> {
    let page = logs.ok(LIST, request).await;
    assert_eq!(
        page.get("nextToken"),
        None,
        "case {request}: the detectors of a case fit one page, got {page}"
    );
    page.get("anomalyDetectors")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("case {request}: {LIST} reports anomalyDetectors, got {page}"))
        .clone()
}

/// The element the collection read reports for `arn`.
fn element<'a>(case: &str, rows: &'a [Value], arn: &str) -> &'a Value {
    rows.iter()
        .find(|detector| member(detector, "anomalyDetectorArn") == Some(arn))
        .unwrap_or_else(|| panic!("case {case:?}: the detector {arn} is listed, got {rows:?}"))
}

/// A log group created for one row, and the ARN form the modelled `LogGroupArn` pattern
/// admits (`^[\w#+=/:,.@-]*$` has no `*`, so the `LogGroup$arn` form ending in `:*` is not
/// one of them).
async fn watched_group(logs: &Logs, name: &str) -> String {
    logs.ok("CreateLogGroup", &json!({"logGroupName": name}))
        .await;
    log_group_arn(REGION, name, false)
}

/// Every read of a created detector reports the configuration it RUNS with — the value the
/// create named, or the service's default where the create named none — and reports it in the
/// status the clock has made due; and the collection read reports exactly what the
/// single-detector read reports for the same ARN.
#[tokio::test]
async fn both_read_paths_report_the_defaulted_configuration_and_one_status() {
    let logs = Logs::new();

    for (index, row) in rows().iter().enumerate() {
        let case = row.case.clone();
        let group = watched_group(&logs, &format!("anomaly-detector-defaults/{index}")).await;

        let before = now_ms();
        let created = logs.ok(CREATE, &row.create_body(&group)).await;
        let arn = member(&created, "anomalyDetectorArn")
            .unwrap_or_else(|| {
                panic!("case {case:?}: a create answers anomalyDetectorArn, got {created}")
            })
            .to_string();

        let read = logs.ok(GET, &json!({"anomalyDetectorArn": &arn})).await;
        let filtered = listed(&logs, &json!({"filterLogGroupArn": &group})).await;
        let after = now_ms();

        // The two defaulted members, on the single-detector read and on the collection's
        // element alike: present either way, carrying what the detector runs with.
        let expected_frequency = row.expected_frequency();
        let expected_visibility = row.expected_visibility();
        let row_element = element(&case, &filtered, &arn);
        for (path, body) in [("the single-detector read", &read), ("the element", row_element)] {
            assert_eq!(
                member(body, "evaluationFrequency"),
                Some(expected_frequency),
                "case {case:?}: {path} reports evaluationFrequency {expected_frequency:?}, got \
                 {body}"
            );
            assert_eq!(
                body.get("anomalyVisibilityTime").and_then(Value::as_i64),
                Some(expected_visibility),
                "case {case:?}: {path} reports anomalyVisibilityTime {expected_visibility}, got \
                 {body}"
            );
        }

        // The status is the one the clock has made due: a detector this fresh is still
        // TRAINING, and only a read window that reaches past the instant the create armed
        // admits ANALYZING.
        let creation = read
            .get("creationTimeStamp")
            .and_then(Value::as_i64)
            .unwrap_or_else(|| panic!("case {case:?}: a detector carries creationTimeStamp, got {read}"));
        let admissible = admissible_detector_statuses(creation, before, after);
        for (path, body) in [("the single-detector read", &read), ("the element", row_element)] {
            let status = member(body, "anomalyDetectorStatus").unwrap_or_else(|| {
                panic!("case {case:?}: {path} carries anomalyDetectorStatus, got {body}")
            });
            assert!(
                admissible.contains(&status),
                "case {case:?}: {path} expected one of {admissible:?}, got {status:?}"
            );
        }

        // One record, two readers: the element is the single-detector read plus the ARN that
        // identifies it. The status is compared above, against the window each read fell in,
        // so it is left out here rather than racing the clock between the two reads.
        let mut without_arn = row_element
            .as_object()
            .unwrap_or_else(|| panic!("case {case:?}: a listed detector is a structure, got {row_element}"))
            .clone();
        without_arn.remove("anomalyDetectorArn");
        without_arn.remove("anomalyDetectorStatus");
        let mut read_members = read
            .as_object()
            .unwrap_or_else(|| panic!("case {case:?}: a reported detector is a structure, got {read}"))
            .clone();
        read_members.remove("anomalyDetectorStatus");
        assert_eq!(
            Value::Object(without_arn),
            Value::Object(read_members),
            "case {case:?}: the element must carry what {GET} reports, got {row_element} and \
             {read}"
        );

        // The element the unfiltered enumeration carries for this detector is the same one.
        let whole_account = listed(&logs, &json!({})).await;
        assert_eq!(
            element(&case, &whole_account, &arn).get("evaluationFrequency"),
            Some(&json!(expected_frequency)),
            "case {case:?}: the unfiltered enumeration reports the same configuration, got \
             {whole_account:?}"
        );
    }
}

/// The lifecycle is the CLOCK's: a detector is reported TRAINING while the span the create
/// armed is outstanding, reaches ANALYZING once it has passed, and stays there — reading it
/// never moves it on.
#[tokio::test]
async fn the_clock_carries_a_created_detector_from_training_to_analyzing() {
    let logs = Logs::new();
    let group = watched_group(&logs, "anomaly-detector-defaults/lifecycle").await;

    let before = now_ms();
    let created = logs
        .ok(CREATE, &json!({"logGroupArnList": [&group]}))
        .await;
    let arn = member(&created, "anomalyDetectorArn")
        .unwrap_or_else(|| panic!("a create answers anomalyDetectorArn, got {created}"))
        .to_string();

    // Read straight after the create: the detector is in the status the create handed back,
    // which is the start of the lifecycle and never its end.
    let read = logs.ok(GET, &json!({"anomalyDetectorArn": &arn})).await;
    let after = now_ms();
    let creation = read
        .get("creationTimeStamp")
        .and_then(Value::as_i64)
        .unwrap_or_else(|| panic!("a detector carries creationTimeStamp, got {read}"));
    let admissible = admissible_detector_statuses(creation, before, after);
    let status = member(&read, "anomalyDetectorStatus")
        .unwrap_or_else(|| panic!("a detector carries anomalyDetectorStatus, got {read}"));
    assert!(
        admissible.contains(&status),
        "case \"a freshly created detector\": expected one of {admissible:?}, got {status:?}"
    );
    if admissible == vec![ANOMALY_DETECTOR_STATUS_TRAINING] {
        assert_eq!(
            status, ANOMALY_DETECTOR_STATUS_TRAINING,
            "case \"a freshly created detector\": a detector the clock owes its next status \
             is still {ANOMALY_DETECTOR_STATUS_TRAINING}, got {status:?}"
        );
    }

    // Wait for the CONDITION — the status the clock carries the detector to — rather than for
    // a fixed time.
    let mut reached = None;
    for _ in 0..POLL_ATTEMPTS {
        let read = logs.ok(GET, &json!({"anomalyDetectorArn": &arn})).await;
        let status = member(&read, "anomalyDetectorStatus")
            .unwrap_or_else(|| panic!("a detector carries anomalyDetectorStatus, got {read}"))
            .to_string();
        if status == ANOMALY_DETECTOR_STATUS_ANALYZING {
            reached = Some(read);
            break;
        }
        assert_eq!(
            status, ANOMALY_DETECTOR_STATUS_TRAINING,
            "case \"before the clock makes analysis due\": a detector passes through \
             {ANOMALY_DETECTOR_STATUS_TRAINING} and no other status, got {status:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
    let analyzing = reached.unwrap_or_else(|| {
        panic!(
            "case \"the clock's own transition\": the detector must reach \
             {ANOMALY_DETECTOR_STATUS_ANALYZING} and still has not after {POLL_ATTEMPTS} looks"
        )
    });

    // A status the clock owed is not a modification of the detector, and reading twice reports
    // the same thing: the terminal status is where the lifecycle stops.
    assert_eq!(
        analyzing.get("lastModifiedTimeStamp").and_then(Value::as_i64),
        Some(creation),
        "case \"the clock's own transition\": it is no modification of the detector, so the \
         detector was last modified when it was created ({creation}), got {analyzing}"
    );
    let again = logs.ok(GET, &json!({"anomalyDetectorArn": &arn})).await;
    assert_eq!(
        again, analyzing,
        "case \"a detector the clock owes nothing\": reading twice reports the same thing, \
         expected {analyzing}, got {again}"
    );
    let element_of = listed(&logs, &json!({"filterLogGroupArn": &group})).await;
    assert_eq!(
        member(element("the clock's own transition", &element_of, &arn), "anomalyDetectorStatus"),
        Some(ANOMALY_DETECTOR_STATUS_ANALYZING),
        "case \"a detector the clock owes nothing\": the collection read agrees with the \
         single-detector read, got {element_of:?}"
    );
}
