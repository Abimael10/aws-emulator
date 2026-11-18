// SPDX-License-Identifier: CC0-1.0

//! The IoT JOB TEMPLATE surface on the ASSEMBLED emulator: the router claims
//! `PUT /job-templates/{jobTemplateId}` and `GET /job-templates/{jobTemplateId}` for the
//! `iot` signing name, and the template one request creates is the template the next
//! request reads.
//!
//! The crate's own suite (`services/service-com-amazonaws-iot-awsiotservice/tests/all/
//! job_description.rs`) owns the rules of the store itself — what a created template
//! reports member for member and how a request it cannot take is refused. What this suite
//! adds is the one thing a single-service harness cannot see: that the routes reach that
//! store once EVERY service is mounted, so a client of the assembled emulator addresses
//! the same records.
//!
//! Assembling the router costs stack (see `boot_serve.rs`), so it is built — and the
//! requests are driven — on an explicitly sized thread.

use std::sync::Arc;

use aws_emulator_router::{build_router, InMemoryEngine};
use serde_json::{json, Value};

/// Keep this at or above `boot_serve.rs`'s stack: the same assembly runs here.
const BOOT_STACK_SIZE: usize = 64 * 1024 * 1024;

/// The region every request below is signed for; the ARN assertion reads it back.
const REGION: &str = "eu-west-2";

/// The account id every emulated ARN carries (`emulator_server::EMULATOR_ACCOUNT_ID`).
const ACCOUNT_ID: &str = "000000000000";

/// A SigV4 credential scope naming `iot` in [`REGION`] — the signal the router picks the
/// owning service by.
fn authorization() -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/iot/aws4_request, \
         SignedHeaders=host;x-amz-date, Signature=deadbeef"
    )
}

#[test]
fn the_assembled_emulator_serves_the_iot_job_template_routes() {
    std::thread::Builder::new()
        .name("iot-job-template-routes".to_string())
        .stack_size(BOOT_STACK_SIZE)
        .spawn(|| {
            let engine: Arc<InMemoryEngine> = Arc::new(InMemoryEngine::new());
            let mut router = build_router(engine);
            router.set_request_authentication(false);

            let runtime = tokio::runtime::Builder::new_current_thread()
                .build()
                .expect("a current-thread runtime");

            let control = |method: &str, path: String, body: String| {
                let request = http::Request::builder()
                    .method(method)
                    .uri(format!("https://iot.{REGION}.amazonaws.com{path}"))
                    .header("authorization", authorization())
                    .header("content-type", "application/json")
                    .body(bytes::Bytes::from(body))
                    .expect("a well-formed request");
                let response = runtime.block_on(router.dispatch(request));
                let status = response.status().as_u16();
                let rendered = String::from_utf8_lossy(response.body()).to_string();
                let parsed: Value = serde_json::from_str(&rendered).unwrap_or(Value::Null);
                (status, rendered, parsed)
            };

            let id = "assembled-template";
            let request = json!({
                "description": "the template a client of the assembled emulator creates",
                "document": r#"{"operation":"reboot"}"#,
                "timeoutConfig": { "inProgressTimeoutInMinutes": 30 },
            });
            let (status, rendered, created) =
                control("PUT", format!("/job-templates/{id}"), request.to_string());
            assert_eq!(
                status, 200,
                "creating a job template on the assembled router must answer 200, got \
                 {status} with {rendered}"
            );
            assert_eq!(
                (
                    created["jobTemplateId"].as_str(),
                    created["jobTemplateArn"].as_str()
                ),
                (
                    Some(id),
                    Some(
                        format!("arn:aws:iot:{REGION}:{ACCOUNT_ID}:jobtemplate/{id}").as_str()
                    )
                ),
                "expected the created id and the ARN this plane mints, got {rendered}"
            );

            let (status, rendered, described) =
                control("GET", format!("/job-templates/{id}"), String::new());
            assert_eq!(
                status, 200,
                "reading it back on the assembled router must answer 200, got {status} \
                 with {rendered}"
            );
            assert_eq!(
                (
                    described["jobTemplateId"].as_str(),
                    described["document"].as_str(),
                    described["timeoutConfig"]["inProgressTimeoutInMinutes"].as_i64()
                ),
                (Some(id), Some(r#"{"operation":"reboot"}"#), Some(30)),
                "expected the record the create persisted, got {rendered}"
            );
        })
        .expect("spawning the router thread")
        .join()
        .expect("the router thread runs to completion");
}
