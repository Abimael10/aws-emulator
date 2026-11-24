// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP coverage of HOW `AssociateTargetsWithJob` refuses a job that takes no
//! further targets because of its `targetSelection`.
//!
//! "Targets can be added to a job that has target selection CONTINUOUS"
//! (`smithy_query(query="operation",operation="AssociateTargetsWithJob")`): a job that is
//! not continuous "will be complete after all those things specified as targets have
//! completed the job" (`Job$targetSelection`), so nothing joins it later. The refusal is
//! `InvalidRequestException` (400) and it NAMES the two things a caller needs to tell one
//! refused association from another in a batch of them: the `jobId` from the request's own
//! path, and the selection the job holds. `TargetSelection` defines exactly two values
//! (`smithy_query(query="shape",shape="TargetSelection")`), so a job that is not continuous
//! is a snapshot job — which is what a job created without naming a selection is too.
//!
//! The rows span what the rule generalises over: the job's id, whether its create named
//! `SNAPSHOT` or named no selection at all, and how many targets the refused call tries to
//! add. A CONTINUOUS job is the control row: it takes the same targets, so the refusal is
//! about the selection and not about the targets.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use service_com_amazonaws_iot_awsiotservice::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// The account id every emulated ARN carries (`emulator_server::EMULATOR_ACCOUNT_ID`).
const ACCOUNT_ID: &str = "000000000000";

/// The region this suite signs for.
const REGION: &str = "eu-west-2";

/// One emulator with this service mounted on a shared engine.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(EmulatedService::new(engine));
    router
}

/// One restJson1 call to this service, signed for the `iot` signing name.
async fn call(
    router: &MultiServiceRouter,
    method: &str,
    path: &str,
    body: Option<&Value>,
) -> (u16, Value) {
    let host = format!("iot.{REGION}.amazonaws.com");
    let payload = body.map_or_else(Bytes::new, |value| Bytes::from(value.to_string()));
    let request = http::Request::builder()
        .method(method)
        .uri(format!("https://{host}{path}"))
        .header("host", host)
        .header("content-type", "application/json")
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/iot/aws4_request, \
                 SignedHeaders=host, Signature=0123456789abcdef"
            ),
        )
        .body(payload)
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let bytes = response.into_body();
    let body = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, body)
}

/// The thing ARN a `targets` entry names a device by.
fn thing_arn(thing_name: &str) -> String {
    format!("arn:aws:iot:{REGION}:{ACCOUNT_ID}:thing/{thing_name}")
}

/// RULE: a job whose `targetSelection` is not CONTINUOUS refuses further targets, and the
/// refusal names the job the request addressed and the selection that job holds.
#[tokio::test]
async fn the_refusal_names_the_job_and_its_target_selection() {
    let router = emulator();

    // (case, job id, the selection its create named, the selection the refusal must state)
    let refusing: [(&str, &str, Option<&str>, &str); 3] = [
        (
            "a snapshot job",
            "snapshot-rollout",
            Some("SNAPSHOT"),
            "SNAPSHOT",
        ),
        (
            "another snapshot job, under a different id",
            "Fleet_Update-7",
            Some("SNAPSHOT"),
            "SNAPSHOT",
        ),
        (
            // A create that names no selection names no continuous rollout either, so
            // nothing joins the job later: it is reported as the snapshot job it is.
            "a job whose create named no selection",
            "unselected-rollout",
            None,
            "SNAPSHOT",
        ),
    ];

    for (case, job_id, created_with, _) in &refusing {
        let mut body = json!({ "targets": [thing_arn("laser-1")], "document": "{}" });
        if let Some(selection) = created_with {
            body["targetSelection"] = json!(selection);
        }
        let (status, answered) = call(&router, "PUT", &format!("/jobs/{job_id}"), Some(&body)).await;
        assert_eq!(
            status, 200,
            "case {case:?}: CreateJob must succeed, got {status} ({answered})"
        );
    }
    // The control: a CONTINUOUS job takes the very same targets.
    let (status, answered) = call(
        &router,
        "PUT",
        "/jobs/continuous-rollout",
        Some(&json!({
            "targets": [thing_arn("laser-1")],
            "document": "{}",
            "targetSelection": "CONTINUOUS",
        })),
    )
    .await;
    assert_eq!(
        status, 200,
        "case \"a continuous job\": CreateJob must succeed, got {status} ({answered})"
    );

    // How many targets one call adds is no part of the rule, so the rows vary it.
    let additions: [(&str, Vec<String>); 2] = [
        ("one more device", vec![thing_arn("laser-2")]),
        (
            "two more devices at once",
            vec![thing_arn("laser-3"), thing_arn("laser-4")],
        ),
    ];

    for (case, job_id, _, stated_selection) in refusing {
        for (addition, targets) in &additions {
            let (status, answered) = call(
                &router,
                "POST",
                &format!("/jobs/{job_id}/targets"),
                Some(&json!({ "targets": targets })),
            )
            .await;
            assert_eq!(
                status, 400,
                "case {case:?} + {addition:?}: expected HTTP 400, got {status} ({answered})"
            );
            assert_eq!(
                answered["__type"].as_str().unwrap_or_default(),
                "InvalidRequestException",
                "case {case:?} + {addition:?}: expected the modelled refusal, got {answered}"
            );
            assert_eq!(
                answered["message"].as_str().unwrap_or_default(),
                format!(
                    "Targets cannot be associated with a job {job_id} with {stated_selection} \
                     target selection."
                ),
                "case {case:?} + {addition:?}: expected the refusal to name the job and its \
                 selection, got {answered}"
            );
        }
    }

    // The control row takes the same targets, so what the rows above were refused for is
    // the selection and nothing else.
    for (addition, targets) in &additions {
        let (status, answered) = call(
            &router,
            "POST",
            "/jobs/continuous-rollout/targets",
            Some(&json!({ "targets": targets })),
        )
        .await;
        assert_eq!(
            status, 200,
            "case \"a continuous job\" + {addition:?}: expected HTTP 200, got {status} \
             ({answered})"
        );
        assert_eq!(
            answered["jobId"].as_str().unwrap_or_default(),
            "continuous-rollout",
            "case \"a continuous job\" + {addition:?}: expected the answer to name the job, got \
             {answered}"
        );
    }

    // The refused calls left every job pointed at exactly the one device its create named.
    for (case, job_id, _, _) in refusing {
        let (status, answered) = call(&router, "GET", &format!("/jobs/{job_id}"), None).await;
        assert_eq!(
            status, 200,
            "case {case:?}: DescribeJob must answer 200, got {status} ({answered})"
        );
        assert_eq!(
            answered["job"]["targets"],
            json!([thing_arn("laser-1")]),
            "case {case:?}: expected the refused calls to leave the targets alone, got \
             {answered}"
        );
    }
}
