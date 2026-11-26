// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for the change a pipeline is going through, as
//! `GetPipelineChangeProgress` reports it.
//!
//! The RULE asserted below: the operation reports the create of the pipeline it names, read
//! off that pipeline's own record — the change started when the pipeline was created, and its
//! progress is the pipeline's status (in progress while it is still being created, completed
//! in every status past the create). A name this account holds no pipeline under is refused
//! 404 `ResourceNotFoundException`, and the report is a READ: what it answers cannot change
//! what a later caller sees.
//!
//! The rows differ along what the rule generalises over: the pipeline's name shape, its unit
//! counts, whether it carries a configuration naming a role, and which lifecycle leg it last
//! went through (fresh create, stopped, started again) — plus a name no pipeline was ever
//! created under.
//!
//! Nothing here waits: no case sleeps, measures elapsed time or bounds anything by a clock.
//! Each assertion is about the first request that follows a call.

use std::sync::Arc;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// A Region that is NOT the emulator default, so nothing here passes because it was pinned
/// to the default one.
const REGION: &str = "eu-west-2";

/// `ChangeProgressStatuses` values this cover expects (smithy_query: shape
/// ChangeProgressStatuses lists PENDING, IN_PROGRESS, COMPLETED, FAILED).
const COMPLETED: &str = "COMPLETED";

/// What one call answered: the HTTP status, the modelled error code (empty on success) and
/// the parsed body.
#[derive(Debug)]
struct Answer {
    status: u16,
    error_type: String,
    body: Value,
}

impl Answer {
    /// The `message` member of a refusal, under either casing a protocol may render it with.
    fn message(&self) -> String {
        for key in ["message", "Message"] {
            if let Some(text) = self.body.get(key).and_then(Value::as_str) {
                return text.to_string();
            }
        }
        panic!("expected an error message in {:?}", self.body)
    }
}

fn new_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    // Signature verification is the emulator's own concern and is exercised elsewhere;
    // these cases are about this service's behaviour.
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_osis_amazonopensearchingestionservice::EmulatedService::new(engine),
    );
    router
}

async fn call(router: &MultiServiceRouter, method: &str, path: &str, body: Option<Value>) -> Answer {
    let payload = match body {
        Some(value) => bytes::Bytes::from(serde_json::to_vec(&value).expect("serialize body")),
        None => bytes::Bytes::new(),
    };
    let request = http::Request::builder()
        .method(method)
        .uri(format!("https://osis.{REGION}.amazonaws.com{path}"))
        // The credential scope is how a handler learns the Region its ARNs name.
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDTEST/20260101/{REGION}/osis/aws4_request, \
                 SignedHeaders=host, Signature=abc"
            ),
        )
        .header("content-type", "application/json")
        .body(payload)
        .expect("build request");

    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let error_type = response
        .headers()
        .get("x-amzn-errortype")
        .and_then(|value| value.to_str().ok())
        .map(|value| {
            value
                .split(['/', ':'])
                .next()
                .unwrap_or(value)
                .trim()
                .to_string()
        })
        .unwrap_or_default();
    let bytes = response.into_body();
    let body = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    Answer {
        status,
        error_type,
        body,
    }
}

async fn create_pipeline(
    router: &MultiServiceRouter,
    name: &str,
    min_units: i64,
    max_units: i64,
    configuration_body: &str,
) -> Answer {
    call(
        router,
        "POST",
        "/2022-01-01/osis/createPipeline",
        Some(json!({
            "PipelineName": name,
            "MinUnits": min_units,
            "MaxUnits": max_units,
            "PipelineConfigurationBody": configuration_body,
        })),
    )
    .await
}

async fn get_pipeline(router: &MultiServiceRouter, name: &str) -> Answer {
    call(
        router,
        "GET",
        &format!("/2022-01-01/osis/getPipeline/{name}"),
        None,
    )
    .await
}

async fn change_progress(router: &MultiServiceRouter, name: &str) -> Answer {
    call(
        router,
        "GET",
        &format!("/2022-01-01/osis/getPipelineChangeProgress/{name}"),
        None,
    )
    .await
}

/// The one change the response reports, which must be exactly one.
fn only_change(case: &str, answered: &Answer) -> Value {
    let statuses = answered.body["ChangeProgressStatuses"]
        .as_array()
        .unwrap_or_else(|| {
            panic!(
                "case {case:?}: expected a ChangeProgressStatuses list, got {:?}",
                answered.body
            )
        });
    assert_eq!(
        statuses.len(),
        1,
        "case {case:?}: the create is the one change reported, got {statuses:?}"
    );
    statuses[0].clone()
}

/// One row of the reporting table.
struct Row {
    case: &'static str,
    pipeline: &'static str,
    min_units: i64,
    max_units: i64,
    configuration_body: &'static str,
}

/// A pipeline's change progress is its create, started when the pipeline was created and
/// completed once the pipeline has come up.
#[tokio::test]
async fn the_reported_change_is_the_create_of_the_pipeline_it_names() {
    let rows = [
        Row {
            case: "a pipeline whose source accepts records over HTTP",
            pipeline: "progress-http-pipeline",
            min_units: 1,
            max_units: 1,
            configuration_body: "version: \"2\"\nlog-pipeline:\n  source:\n    http:\n      \
                                 path: \"/ingest\"\n  sink:\n    - s3:\n        bucket: \
                                 \"records\"\n",
        },
        Row {
            case: "the shortest name the member admits, with a wider unit range",
            pipeline: "abc",
            min_units: 2,
            max_units: 9,
            configuration_body: "version: \"2\"\nfrom-s3:\n  source:\n    s3:\n      bucket: \
                                 \"in\"\n  sink:\n    - s3:\n        bucket: \"out\"\n",
        },
        Row {
            case: "a configuration whose sink names no destination at all",
            pipeline: "progress-no-sink-pipeline",
            min_units: 4,
            max_units: 4,
            configuration_body: "version: \"2\"\np:\n  source:\n    http:\n",
        },
    ];

    for row in rows {
        let router = new_router();
        let created = create_pipeline(
            &router,
            row.pipeline,
            row.min_units,
            row.max_units,
            row.configuration_body,
        )
        .await;
        assert_eq!(
            created.status, 200,
            "case {:?}: setup CreatePipeline answered {} / {:?}",
            row.case, created.status, created.body
        );
        let created_at = created.body["Pipeline"]["CreatedAt"].clone();

        let reported = change_progress(&router, row.pipeline).await;
        assert_eq!(
            reported.status, 200,
            "case {:?}: expected 200, got {} / {:?}",
            row.case, reported.status, reported.body
        );
        let change = only_change(row.case, &reported);
        assert_eq!(
            change["Status"].as_str(),
            Some(COMPLETED),
            "case {:?}: the create of a pipeline that has come up has completed, got {:?}",
            row.case,
            change
        );
        // The change started when the pipeline was created: the two reads of one record
        // cannot disagree about that instant.
        assert_eq!(
            change["StartTime"], created_at,
            "case {:?}: expected the pipeline's CreatedAt {created_at:?}, got {:?}",
            row.case, change["StartTime"]
        );
        assert_eq!(
            change["TotalNumberOfStages"].as_i64().unwrap_or_default(),
            0,
            "case {:?}: no stage is reported, got {:?}",
            row.case,
            change
        );
        assert_eq!(
            change["ChangeProgressStages"]
                .as_array()
                .map(Vec::len)
                .unwrap_or_default(),
            0,
            "case {:?}: no stage is reported, got {:?}",
            row.case,
            change
        );
    }
}

/// The report is a READ: what a later caller sees is the same before and after it, and it
/// reports the create whichever lifecycle leg the pipeline last went through.
#[tokio::test]
async fn reporting_the_change_does_not_change_what_a_later_caller_sees() {
    let case = "a pipeline read, stopped, read again and started again";
    let router = new_router();
    let pipeline = "progress-lifecycle-pipeline";
    let created = create_pipeline(
        &router,
        pipeline,
        1,
        3,
        "version: \"2\"\np:\n  source:\n    http:\n  sink:\n    - s3:\n        bucket: \"records\"\n",
    )
    .await;
    assert_eq!(
        created.status, 200,
        "case {case:?}: setup CreatePipeline answered {} / {:?}",
        created.status, created.body
    );

    // Settle the create first, so the comparison below is between two reads of a settled
    // record rather than across the transition the clock owed it.
    let before = get_pipeline(&router, pipeline).await;
    let reported = change_progress(&router, pipeline).await;
    assert_eq!(
        reported.status, 200,
        "case {case:?}: expected 200, got {} / {:?}",
        reported.status, reported.body
    );
    let after = get_pipeline(&router, pipeline).await;
    assert_eq!(
        after.body, before.body,
        "case {case:?}: a report must leave the record alone, {:?} became {:?}",
        before.body, after.body
    );

    // Every later leg still reports the CREATE, which has completed.
    for (leg, path) in [
        ("stopped", format!("/2022-01-01/osis/stopPipeline/{pipeline}")),
        ("started", format!("/2022-01-01/osis/startPipeline/{pipeline}")),
    ] {
        let moved = call(&router, "PUT", &path, None).await;
        assert_eq!(
            moved.status, 200,
            "case {case:?} / {leg}: the lifecycle call answered {} / {:?}",
            moved.status, moved.body
        );
        let reported = change_progress(&router, pipeline).await;
        let change = only_change(case, &reported);
        assert_eq!(
            change["Status"].as_str(),
            Some(COMPLETED),
            "case {case:?} / {leg}: the create reported is still the completed one, got \
             {change:?}"
        );
        assert_eq!(
            change["StartTime"], created.body["Pipeline"]["CreatedAt"],
            "case {case:?} / {leg}: the create started when the pipeline was created, got {:?}",
            change["StartTime"]
        );
    }
}

/// A name this account holds no pipeline under is refused, and a refused read reports no
/// change.
#[tokio::test]
async fn a_name_no_pipeline_is_held_under_is_refused() {
    let router = new_router();
    // One pipeline exists, so a refusal below is about the name asked for and not about an
    // empty account.
    let created = create_pipeline(
        &router,
        "progress-present-pipeline",
        1,
        1,
        "version: \"2\"\np:\n  source:\n    http:\n",
    )
    .await;
    assert_eq!(
        created.status, 200,
        "setup CreatePipeline answered {} / {:?}",
        created.status, created.body
    );

    // The rows differ along the name shape the member admits (`@length(3, 28)`,
    // `^[a-z][a-z0-9\-]+$`): the shortest, a hyphenated one, and the longest.
    let rows = [
        ("the shortest name the member admits", "zzz"),
        ("a hyphenated name", "progress-absent-pipeline"),
        ("the longest name the member admits", "abcdefghijklmnopqrstuvwxyz-1"),
    ];
    for (case, name) in rows {
        let refused = change_progress(&router, name).await;
        assert_eq!(
            refused.status, 404,
            "case {case:?}: expected 404, got {} / {:?}",
            refused.status, refused.body
        );
        assert_eq!(
            refused.error_type, "ResourceNotFoundException",
            "case {case:?}: expected ResourceNotFoundException, got {:?}",
            refused.error_type
        );
        assert_eq!(
            refused.message(),
            "Pipeline not found",
            "case {case:?}: expected this read's own not-found wording, got {:?}",
            refused.message()
        );
        assert!(
            refused.body.get("ChangeProgressStatuses").is_none(),
            "case {case:?}: a refusal reports no change, got {:?}",
            refused.body
        );
    }
}
