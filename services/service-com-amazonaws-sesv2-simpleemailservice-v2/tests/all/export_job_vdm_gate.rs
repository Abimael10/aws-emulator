// SPDX-License-Identifier: CC0-1.0

//! The account's Virtual Deliverability Manager switch as the gate on a metrics export
//! job (`CreateExportJob`), end to end over HTTP.
//!
//! Every case drives the real wire path — `MultiServiceRouter::dispatch` with this
//! service registered — so routing, deserialization, the handler and serialization are
//! all exercised, and what a create left behind is read back both through the engine's
//! own state and through the implemented reader of the account's export jobs
//! (`ListExportJobs`).
//!
//! The rules these cases pin:
//!
//! * An export that draws on the account's Virtual Deliverability Manager data — the
//!   metrics export, whose `MetricsDataSource` names the VDM namespace — exists only
//!   while the account's VDM configuration is enabled (`VdmAttributes$VdmEnabled`,
//!   which `PutAccountVdmAttributes` writes and `GetAccount` reports). While that
//!   switch is off the create is refused 400 `BadRequestException` with the one
//!   sentence this service names the feature to enable with, and nothing is stored, so
//!   a read of the account's export jobs reports exactly the jobs it did before; as
//!   soon as the switch is on again the same request is served.
//! * The gate decides a request that is otherwise serveable: a create that is malformed
//!   in its own right — an `ExportDataSource` selecting no one source, a caller-supplied
//!   `ExportDestination$S3Url`, a window whose end precedes its start — keeps its own
//!   refusal whatever the switch says.
//! * The switch withdraws the metrics surface only: a Message Insights export is served
//!   whichever status the account's VDM switch is in.
//!
//! The tables span what the rules generalise over: every `FeatureStatus` the switch
//! takes and both directions between them, the dimension names and counts a metrics
//! source filters by, one and several metrics with both aggregations, both `DataFormat`
//! values, windows of several days, of one second and of no time at all, and each way a
//! create can be malformed in its own right.

use std::sync::Arc;

use serde_json::{json, Value};
use service_com_amazonaws_sesv2_simpleemailservice_v2::ops::types::{
    DATA_FORMAT_VALUES, FEATURE_STATUS_VALUES,
};
use service_com_amazonaws_sesv2_simpleemailservice_v2::state::{
    custom_s3_url_is_not_supported, enable_vdm_to_use_this_feature, invalid_export_data_source,
    window_runs_backwards, ComAmazonawsSesv2SimpleemailserviceV2State, FEATURE_DISABLED,
    FEATURE_ENABLED,
};
use service_com_amazonaws_sesv2_simpleemailservice_v2::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// The Region the cases execute in: the one their `Authorization` credential scope
/// names. An export job belongs to the account, so one Region is enough.
const HOME: &str = "us-east-1";

/// `MetricsDataSource$Namespace`'s only published value
/// (`smithy_query(query="shape",shape="MetricNamespace")`).
const VDM: &str = "VDM";

/// `ExportSourceType$METRICS_DATA` — the source type a metrics data source selects.
const METRICS_DATA: &str = "METRICS_DATA";

/// `ExportSourceType$MESSAGE_INSIGHTS` — the source type a Message Insights data source
/// selects.
const MESSAGE_INSIGHTS: &str = "MESSAGE_INSIGHTS";

/// `2026-01-01T00:00:00Z`, the start of the interval the cases export over, as whole
/// epoch seconds.
const DAY_START: i64 = 1_767_225_600;

/// One day later, so the default interval covers a whole UTC day.
const DAY_END: i64 = DAY_START + 86_400;

/// One answered request: the status, the error shape when there was one, and the parsed
/// body.
struct Reply {
    status: u16,
    error_type: Option<String>,
    body: Value,
}

impl Reply {
    /// The modelled error code the response names, or an empty string on success.
    ///
    /// restJson1 resolves the error shape from `x-amzn-errortype`; the JSON body's
    /// `__type` carries the same name, so either is read.
    fn error_code(&self) -> String {
        let raw = self
            .error_type
            .as_deref()
            .or_else(|| self.body.get("__type").and_then(Value::as_str))
            .unwrap_or_default();
        let name = raw.split('#').next_back().unwrap_or(raw);
        name.split(':').next().unwrap_or(name).to_string()
    }

    /// The message the error carried.
    fn message(&self) -> String {
        self.body
            .get("message")
            .or_else(|| self.body.get("Message"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    }

    /// The `JobId` a create answered with, or an empty string when it carried none.
    fn job_id(&self) -> String {
        self.body
            .get("JobId")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    }

    /// The `JobId`s a listing reported, in ascending order so two snapshots compare.
    fn job_ids(&self) -> Vec<String> {
        let mut ids: Vec<String> = self
            .body
            .get("ExportJobs")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .filter_map(|job| {
                job.get("JobId")
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
            .collect();
        ids.sort();
        ids
    }
}

/// One metrics export a case asks for.
struct MetricsRow {
    case: &'static str,
    /// The `MetricsDataSource` document the request carries.
    data_source: Value,
    /// `ExportDestination$DataFormat`.
    data_format: &'static str,
}

/// A metrics data source: `dimensions` to split the activity by, `metrics` to report
/// for each, over the interval `start`..`end`.
///
/// The dimension names are `MetricDimensionName`'s and the metric names and
/// aggregations `Metric`'s and `MetricAggregation`'s
/// (`smithy_query(query="shape",shape="MetricDimensionName,Metric,MetricAggregation")`).
fn metrics_source(dimensions: Value, metrics: Value, start: i64, end: i64) -> Value {
    json!({
        "MetricsDataSource": {
            "Dimensions": dimensions,
            "Namespace": VDM,
            "Metrics": metrics,
            "StartDate": start,
            "EndDate": end,
        }
    })
}

/// The simplest serveable metrics data source: one mailbox-provider dimension and one
/// metric over one UTC day.
fn one_metric_source() -> Value {
    metrics_source(
        json!({"ISP": ["gmail"]}),
        json!([{"Name": "SEND", "Aggregation": "VOLUME"}]),
        DAY_START,
        DAY_END,
    )
}

/// A Message Insights data source over the same interval, with no filters.
fn insights_source(start: i64, end: i64) -> Value {
    json!({"MessageInsightsDataSource": {"StartDate": start, "EndDate": end}})
}

/// The metrics exports every leg of the gate runs over: they differ along what the rule
/// generalises over — which dimension names filter the activity and how many, how many
/// metrics are asked for and with which aggregation, which data format the file is
/// written in, and how long the exported interval is (several days, one second, and no
/// time at all, both bounds being inclusive).
fn metrics_rows() -> Vec<MetricsRow> {
    vec![
        MetricsRow {
            case: "one ISP dimension, one metric, CSV, a window of one day",
            data_source: metrics_source(
                json!({"ISP": ["gmail"]}),
                json!([{"Name": "SEND", "Aggregation": "VOLUME"}]),
                DAY_START,
                DAY_END,
            ),
            data_format: "CSV",
        },
        MetricsRow {
            case: "three dimensions, several metrics with both aggregations, JSON, three days",
            data_source: metrics_source(
                json!({
                    "EMAIL_IDENTITY": ["sender.example.com", "alerts.example.com"],
                    "CONFIGURATION_SET": ["campaign"],
                    "ISP": ["example-isp"],
                }),
                json!([
                    {"Name": "DELIVERY", "Aggregation": "VOLUME"},
                    {"Name": "OPEN", "Aggregation": "RATE"},
                    {"Name": "PERMANENT_BOUNCE", "Aggregation": "RATE"},
                ]),
                DAY_START,
                DAY_START + 3 * 86_400,
            ),
            data_format: "JSON",
        },
        MetricsRow {
            case: "one configuration-set dimension, one metric, JSON, a window of one second",
            data_source: metrics_source(
                json!({"CONFIGURATION_SET": ["campaign"]}),
                json!([{"Name": "COMPLAINT", "Aggregation": "RATE"}]),
                DAY_START,
                DAY_START + 1,
            ),
            data_format: "JSON",
        },
        MetricsRow {
            case: "one mailbox-provider dimension, one metric, CSV, a window of no time at all",
            data_source: metrics_source(
                json!({"ISP": ["yahoo"]}),
                json!([{"Name": "SEND", "Aggregation": "VOLUME"}]),
                DAY_START,
                DAY_START,
            ),
            data_format: "CSV",
        },
    ]
}

/// The emulated service behind its router, plus the shared engine so a case can read
/// what a create left in the store.
struct Ses {
    engine: Arc<engine::InMemoryEngine>,
    router: MultiServiceRouter,
}

impl Ses {
    /// A service nothing has configured: the account's VDM switch is wherever a fresh
    /// account leaves it, which is what the first leg of the gate table checks.
    fn new() -> Self {
        let engine = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        // These cases exercise handler behaviour, not SigV4.
        router.set_request_authentication(false);
        router.register_service(EmulatedService::new(Arc::clone(&engine)));
        Self { engine, router }
    }

    /// Send one API request.
    async fn call(&self, method: &str, path: &str, body: Option<Value>) -> Reply {
        let payload = match &body {
            Some(value) => bytes::Bytes::from(serde_json::to_vec(value).expect("serialize body")),
            None => bytes::Bytes::new(),
        };
        let host = format!("email.{HOME}.amazonaws.com");
        let request = http::Request::builder()
            .method(method)
            .uri(format!("https://{host}{path}"))
            .header("host", host)
            .header("content-type", "application/json")
            // The SigV4 credential scope is what routes the request to the `ses`
            // signing name; the signature itself is not checked here.
            .header(
                "authorization",
                format!(
                    "AWS4-HMAC-SHA256 Credential=AKIAEMULATOR0000000/20260101/{HOME}/ses/aws4_request, SignedHeaders=host, Signature=00"
                ),
            )
            .body(payload)
            .expect("build request");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let error_type = response
            .headers()
            .get("x-amzn-errortype")
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        let bytes = response.into_body();
        let body = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap_or(Value::Null)
        };
        Reply {
            status,
            error_type,
            body,
        }
    }

    /// One `CreateExportJob` request for `data_source` written in `data_format`.
    async fn create(&self, data_source: Value, data_format: &str) -> Reply {
        self.call(
            "POST",
            "/v2/email/export-jobs",
            Some(json!({
                "ExportDataSource": data_source,
                "ExportDestination": {"DataFormat": data_format},
            })),
        )
        .await
    }

    /// One `CreateExportJob` request carrying whatever `body` says, for the cases whose
    /// request is malformed in its own right.
    async fn create_raw(&self, body: Value) -> Reply {
        self.call("POST", "/v2/email/export-jobs", Some(body)).await
    }

    /// Set the account's VDM switch, which is what a metrics export is served under.
    async fn put_vdm(&self, status: &str) -> Reply {
        self.call(
            "PUT",
            "/v2/email/account/vdm",
            Some(json!({"VdmAttributes": {"VdmEnabled": status}})),
        )
        .await
    }

    /// `VdmAttributes$VdmEnabled` as `GetAccount` reports it, which is how a caller
    /// sees the switch the gate reads.
    async fn reported_vdm_status(&self) -> String {
        let account = self.call("GET", "/v2/email/account", None).await;
        assert_eq!(
            account.status, 200,
            "the account report must be served, got {} {:?}",
            account.status, account.body
        );
        account
            .body
            .get("VdmAttributes")
            .and_then(|attributes| attributes.get("VdmEnabled"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    }

    /// The `JobId`s a read of the account's export jobs of `source_type` reports.
    async fn listed_job_ids(&self, source_type: &str) -> Vec<String> {
        let reply = self
            .call(
                "POST",
                "/v2/email/list-export-jobs",
                Some(json!({"ExportSourceType": source_type})),
            )
            .await;
        assert_eq!(
            reply.status, 200,
            "the listing must be served, got {} {:?}",
            reply.status, reply.body
        );
        reply.job_ids()
    }

    /// How many export jobs the store holds, so a refusal can be shown to have written
    /// nothing at all.
    fn stored_count(&self) -> usize {
        let state = self
            .engine
            .state::<ComAmazonawsSesv2SimpleemailserviceV2State>();
        let store = state.lock();
        store.export_jobs.len()
    }
}

/// One leg of the gate table: what the account's VDM switch is set to (`None` leaves a
/// fresh account alone), the status `GetAccount` must then report, and whether a metrics
/// export is served.
struct Leg {
    case: &'static str,
    set: Option<&'static str>,
    reported: &'static str,
    served: bool,
}

/// The legs, crossing every `FeatureStatus` the switch takes in both directions and
/// beginning with an account nothing configured.
const LEGS: &[Leg] = &[
    Leg {
        case: "an account nothing configured",
        set: None,
        reported: FEATURE_DISABLED,
        served: false,
    },
    Leg {
        case: "VDM switched on",
        set: Some(FEATURE_ENABLED),
        reported: FEATURE_ENABLED,
        served: true,
    },
    Leg {
        case: "VDM switched off",
        set: Some(FEATURE_DISABLED),
        reported: FEATURE_DISABLED,
        served: false,
    },
    Leg {
        case: "VDM switched back on",
        set: Some(FEATURE_ENABLED),
        reported: FEATURE_ENABLED,
        served: true,
    },
];

/// RULE: a metrics export draws on the account's Virtual Deliverability Manager data, so
/// while the account's VDM switch is off the create is refused 400 `BadRequestException`
/// with the one sentence naming the feature to enable and stores nothing — the account's
/// export jobs are exactly those it already held — and the same request is served again
/// as soon as the switch is back on.
#[tokio::test]
async fn a_metrics_export_is_created_only_while_the_account_vdm_is_enabled() {
    // The legs span the modelled set of feature statuses, so no value of the switch
    // goes untested.
    for status in FEATURE_STATUS_VALUES {
        assert!(
            LEGS.iter().any(|leg| leg.set == Some(*status)),
            "case {status:?}: the legs must cover every published feature status, got {:?}",
            FEATURE_STATUS_VALUES
        );
    }

    let ses = Ses::new();
    let mut served_ids: Vec<String> = Vec::new();

    for leg in LEGS {
        let case = leg.case;
        if let Some(status) = leg.set {
            let put = ses.put_vdm(status).await;
            assert_eq!(
                put.status, 200,
                "case {case:?}: expected 200 from the VDM put, got {} {:?}",
                put.status, put.body
            );
        }
        let reported = ses.reported_vdm_status().await;
        assert_eq!(
            reported, leg.reported,
            "case {case:?}: expected the account to report VdmEnabled {:?}, got {reported:?}",
            leg.reported
        );

        for row in metrics_rows() {
            let case = format!("{case} / {}", row.case);
            let before = ses.listed_job_ids(METRICS_DATA).await;
            let stored_before = ses.stored_count();
            let reply = ses.create(row.data_source.clone(), row.data_format).await;

            if !leg.served {
                assert_eq!(
                    (reply.status, reply.error_code().as_str()),
                    (400, "BadRequestException"),
                    "case {case:?}: expected the feature refusal, got {} {:?}",
                    reply.status,
                    reply.body
                );
                assert_eq!(
                    reply.message(),
                    enable_vdm_to_use_this_feature(),
                    "case {case:?}: expected the sentence naming the feature to enable, got {:?}",
                    reply.message()
                );
                assert!(
                    reply.job_id().is_empty(),
                    "case {case:?}: a refused create mints no job id, got {:?}",
                    reply.job_id()
                );
                let listed = ses.listed_job_ids(METRICS_DATA).await;
                assert_eq!(
                    listed, before,
                    "case {case:?}: a refused create leaves the listed jobs alone, got {listed:?}"
                );
                let stored = ses.stored_count();
                assert_eq!(
                    stored, stored_before,
                    "case {case:?}: a refused create stores nothing, got {stored} stored jobs"
                );
                continue;
            }

            assert_eq!(
                reply.status, 200,
                "case {case:?}: expected the create to be served, got {} {:?}",
                reply.status, reply.body
            );
            let job_id = reply.job_id();
            assert!(
                !job_id.is_empty(),
                "case {case:?}: expected the minted JobId, got {:?}",
                reply.body
            );
            served_ids.push(job_id.clone());

            let mut expected = before;
            expected.push(job_id.clone());
            expected.sort();
            let listed = ses.listed_job_ids(METRICS_DATA).await;
            assert_eq!(
                listed, expected,
                "case {case:?}: expected the served create's job to be listed, got {listed:?}"
            );
            let stored = ses.stored_count();
            assert_eq!(
                stored,
                stored_before + 1,
                "case {case:?}: expected the served create to store one job, got {stored} stored \
                 jobs"
            );
        }
    }

    // Only the legs with the feature on created anything, so the account holds exactly
    // their jobs: the refused legs added nothing at any point.
    let mut expected = served_ids.clone();
    expected.sort();
    let listed = ses.listed_job_ids(METRICS_DATA).await;
    assert_eq!(
        listed, expected,
        "expected exactly the jobs the served legs created, got {listed:?}"
    );
    assert_eq!(
        served_ids.len(),
        LEGS.iter().filter(|leg| leg.served).count() * metrics_rows().len(),
        "expected one job per served case, got {} ids",
        served_ids.len()
    );
}

/// RULE: the feature gate decides a create that is otherwise serveable, so a create that
/// is malformed in its own right keeps its own refusal — whatever the account's VDM
/// switch says.
#[tokio::test]
async fn a_malformed_create_keeps_its_own_refusal_whatever_the_vdm_switch_says() {
    // Each case is malformed in one of the ways this operation refuses before anything
    // about the account is read: no one data source selected, a caller-supplied
    // destination URL, and a window whose end precedes its start.
    let malformed: Vec<(&str, Value, String)> = vec![
        (
            "an ExportDataSource naming neither member",
            json!({
                "ExportDataSource": {},
                "ExportDestination": {"DataFormat": DATA_FORMAT_VALUES[0]},
            }),
            invalid_export_data_source(),
        ),
        (
            "an ExportDataSource naming both members",
            json!({
                "ExportDataSource": {
                    "MetricsDataSource": one_metric_source()["MetricsDataSource"].clone(),
                    "MessageInsightsDataSource":
                        insights_source(DAY_START, DAY_END)["MessageInsightsDataSource"].clone(),
                },
                "ExportDestination": {"DataFormat": DATA_FORMAT_VALUES[1]},
            }),
            invalid_export_data_source(),
        ),
        (
            "a metrics export carrying a destination URL of the caller's own",
            json!({
                "ExportDataSource": one_metric_source(),
                "ExportDestination": {
                    "DataFormat": DATA_FORMAT_VALUES[0],
                    "S3Url": "s3://example-bucket/exports/report.csv",
                },
            }),
            custom_s3_url_is_not_supported(),
        ),
        (
            "a metrics export whose window runs backwards",
            json!({
                "ExportDataSource": metrics_source(
                    json!({"ISP": ["gmail"]}),
                    json!([{"Name": "SEND", "Aggregation": "VOLUME"}]),
                    DAY_END,
                    DAY_START,
                ),
                "ExportDestination": {"DataFormat": DATA_FORMAT_VALUES[1]},
            }),
            window_runs_backwards(),
        ),
    ];

    for leg in LEGS {
        let ses = Ses::new();
        if let Some(status) = leg.set {
            let put = ses.put_vdm(status).await;
            assert_eq!(
                put.status, 200,
                "case {:?}: expected 200 from the VDM put, got {} {:?}",
                leg.case, put.status, put.body
            );
        }

        for (case, body, sentence) in &malformed {
            let case = format!("{} / {case}", leg.case);
            let reply = ses.create_raw(body.clone()).await;
            assert_eq!(
                (reply.status, reply.error_code().as_str()),
                (400, "BadRequestException"),
                "case {case:?}: expected the operation's input error, got {} {:?}",
                reply.status,
                reply.body
            );
            assert_eq!(
                &reply.message(),
                sentence,
                "case {case:?}: expected the refusal of what the request itself carries, got {:?}",
                reply.message()
            );
            let stored = ses.stored_count();
            assert_eq!(
                stored, 0,
                "case {case:?}: a refused create stores nothing, got {stored} stored jobs"
            );
        }
    }
}

/// RULE: the switch withdraws the metrics surface, so a Message Insights export is
/// created whichever status the account's VDM switch is in.
#[tokio::test]
async fn a_message_insights_export_is_created_whatever_the_vdm_switch_says() {
    let ses = Ses::new();
    let mut created: Vec<String> = Vec::new();

    for leg in LEGS {
        if let Some(status) = leg.set {
            let put = ses.put_vdm(status).await;
            assert_eq!(
                put.status, 200,
                "case {:?}: expected 200 from the VDM put, got {} {:?}",
                leg.case, put.status, put.body
            );
        }
        for data_format in DATA_FORMAT_VALUES {
            let case = format!("{} / {data_format}", leg.case);
            let reply = ses
                .create(insights_source(DAY_START, DAY_END), data_format)
                .await;
            assert_eq!(
                reply.status, 200,
                "case {case:?}: expected the create to be served, got {} {:?}",
                reply.status, reply.body
            );
            let job_id = reply.job_id();
            assert!(
                !job_id.is_empty(),
                "case {case:?}: expected the minted JobId, got {:?}",
                reply.body
            );
            created.push(job_id);
        }
    }

    created.sort();
    let listed_insights = ses.listed_job_ids(MESSAGE_INSIGHTS).await;
    assert_eq!(
        listed_insights, created,
        "expected every created Message Insights job to be listed, got {listed_insights:?}"
    );
    let listed_metrics = ses.listed_job_ids(METRICS_DATA).await;
    assert_eq!(
        listed_metrics,
        Vec::<String>::new(),
        "these cases created no metrics export, got {listed_metrics:?}"
    );
}
