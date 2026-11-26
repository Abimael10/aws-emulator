// SPDX-License-Identifier: CC0-1.0

//! What a DELETE of one job template leaves behind, end to end over HTTP.
//!
//! Every case drives the real wire path — `MultiServiceRouter::dispatch` over a restJson1
//! `PUT /job-templates/{jobTemplateId}` (`CreateJobTemplate`), `GET
//! /job-templates/{jobTemplateId}` (`DescribeJobTemplate`), `GET /job-templates`
//! (`ListJobTemplates`) and `DELETE /job-templates/{jobTemplateId}`
//! (`DeleteJobTemplate`), signed for the `iot` signing name — so what is asserted is what
//! a client reads back through the modelled surface.
//!
//! The rule pinned here: a delete of a resource of the jobs plane addressed by its id
//! removes exactly the record that id names from the account's state — every later reader
//! of the id reports it gone, the id is free for a create again, and no other record of
//! the family changes — while an id the account holds no such record under is refused with
//! that plane's not-found sentence `<Family> <id> cannot be found.` (HTTP 404,
//! `ResourceNotFoundException`, the error `DeleteJobTemplate` models for a missing
//! template, from
//! `smithy_query(query="operation",operation="DeleteJobTemplate")`), never reported as a
//! delete of nothing.
//!
//! The rows span what the rule generalises over: the form and length of the id within
//! `JobTemplateId`'s modelled constraint (`^[a-zA-Z0-9_-]+$`, length 1..64, so a
//! single-character id and one at the published maximum are both rows), how much
//! configuration the deleted record carried, how many templates the account holds, and
//! each way an id can name no template (never created, already deleted, deleted after a
//! recreate).

use std::sync::Arc;

use serde_json::{json, Value};
use service_com_amazonaws_iot_awsiotservice::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// The region the harness signs for.
const REGION: &str = "eu-west-1";

/// A SigV4 `Authorization` header whose credential scope names the `iot` signing name:
/// the scope is how the router picks the claiming service.
fn signed_for_iot() -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDTESTIOT/20260101/{REGION}/iot/aws4_request, \
         SignedHeaders=host;x-amz-date, Signature=0000"
    )
}

/// One response, reduced to what a client branches on.
#[derive(Debug)]
struct Reply {
    status: u16,
    error_type: Option<String>,
    body: Value,
}

impl Reply {
    /// The `message` the body carries, or `""` when it carries none.
    fn message(&self) -> &str {
        self.body
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or_default()
    }

    /// The `jobTemplateId` of every listed entry, in the order the response carries them.
    fn listed_ids(&self) -> Vec<String> {
        self.body
            .get("jobTemplates")
            .and_then(Value::as_array)
            .map(|entries| {
                entries
                    .iter()
                    .filter_map(|entry| entry.get("jobTemplateId"))
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default()
    }
}

/// The emulator under test: ONE shared engine with the IoT service mounted on it,
/// reached the way the production router reaches it.
struct Emulator {
    router: MultiServiceRouter,
}

impl Emulator {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        router.set_request_authentication(false);
        router.register_service(EmulatedService::new(engine));
        Self { router }
    }

    async fn call(&self, method: &str, path: &str, body: Option<&Value>) -> Reply {
        let payload = body.map_or_else(Vec::new, |value| {
            serde_json::to_vec(value).expect("a request body serializes")
        });
        let request = http::Request::builder()
            .method(method)
            .uri(format!("https://iot.{REGION}.amazonaws.com{path}"))
            .header("authorization", signed_for_iot())
            .header("content-type", "application/json")
            .body(bytes::Bytes::from(payload))
            .expect("a well-formed request");

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

    async fn create_job_template(&self, job_template_id: &str, request: &Value) -> Reply {
        self.call(
            "PUT",
            &format!("/job-templates/{job_template_id}"),
            Some(request),
        )
        .await
    }

    async fn describe_job_template(&self, job_template_id: &str) -> Reply {
        self.call("GET", &format!("/job-templates/{job_template_id}"), None)
            .await
    }

    async fn delete_job_template(&self, job_template_id: &str) -> Reply {
        self.call("DELETE", &format!("/job-templates/{job_template_id}"), None)
            .await
    }

    async fn list_job_templates(&self) -> Reply {
        self.call("GET", "/job-templates", None).await
    }
}

/// How the jobs plane states that a `jobTemplateId` names no job template: this family's
/// noun in two capitalised words, the id the request asked about, and the plane's fixed
/// clause — the same noun the plane's already-exists sentence for the family carries.
fn missing_template_message(job_template_id: &str) -> String {
    format!("Job Template {job_template_id} cannot be found.")
}

/// One row of the delete table: an id within `JobTemplateId`'s modelled pattern, and the
/// configuration the template it names was created with.
struct DeleteCase {
    case: &'static str,
    job_template_id: String,
    /// The optional configuration members of the create request, as an object.
    configuration: Value,
}

/// The rows differ in the FORM and LENGTH of the id (a single character, a hyphenated
/// name, mixed case with an underscore, and the modelled maximum of 64) and in how much
/// configuration the deleted record carried — from a template with none to one carrying
/// every container and a tag set.
fn delete_cases() -> Vec<DeleteCase> {
    vec![
        DeleteCase {
            case: "the shortest id the model admits, no configuration",
            job_template_id: "x".to_string(),
            configuration: json!({}),
        },
        DeleteCase {
            case: "a hyphenated id, a staged rollout",
            job_template_id: "nightly-reboot".to_string(),
            configuration: json!({ "jobExecutionsRolloutConfig": { "maximumPerMinute": 10 } }),
        },
        DeleteCase {
            case: "mixed case with an underscore, a timeout and tags",
            job_template_id: "Reboot_42".to_string(),
            configuration: json!({
                "timeoutConfig": { "inProgressTimeoutInMinutes": 30 },
                "tags": [{ "Key": "owner", "Value": "fleet-ops" }],
            }),
        },
        DeleteCase {
            case: "an id at the modelled maximum length of 64",
            job_template_id: "i".repeat(64),
            configuration: json!({
                "presignedUrlConfig": { "expiresInSec": 900 },
                "destinationPackageVersions": [],
            }),
        },
    ]
}

/// The create request of one row: its configuration plus the `@required` description and
/// the document the template deploys.
fn create_request(case: &DeleteCase) -> Value {
    let mut request = case.configuration.clone();
    let object = request
        .as_object_mut()
        .unwrap_or_else(|| panic!("case {:?}: the row's configuration is an object", case.case));
    object.insert(
        "description".to_string(),
        json!(format!("the template of case {}", case.case)),
    );
    object.insert("document".to_string(), json!(r#"{"operation":"reboot"}"#));
    request
}

/// The rule: a delete removes exactly the record its id names. Every row's template is
/// created into ONE account, then deleted one row at a time — after each delete the id
/// names no template for a reader or a listing, and every id not yet deleted still does.
#[tokio::test]
async fn deleting_a_job_template_removes_only_the_record_its_id_names() {
    let emulator = Emulator::new();
    let cases = delete_cases();

    for case in &cases {
        let created = emulator
            .create_job_template(&case.job_template_id, &create_request(case))
            .await;
        assert_eq!(
            created.status, 200,
            "case {:?}: the template must be creatable, got {} {:?}",
            case.case, created.status, created.body
        );
    }

    let mut remaining: Vec<String> = cases
        .iter()
        .map(|case| case.job_template_id.clone())
        .collect();
    remaining.sort();

    for case in &cases {
        let label = case.case;
        let id = &case.job_template_id;

        let deleted = emulator.delete_job_template(id).await;
        assert_eq!(
            deleted.status, 200,
            "case {label:?}: expected the delete to answer 200, got {} {:?}",
            deleted.status, deleted.body
        );

        // The record is gone for the reader of one template...
        let read = emulator.describe_job_template(id).await;
        assert_eq!(
            read.status, 404,
            "case {label:?}: expected the deleted template to read back as 404, got {} {:?}",
            read.status, read.body
        );
        // ...and for the listing of the family.
        remaining.retain(|listed| listed != id);
        let listed = emulator.list_job_templates().await;
        assert_eq!(
            listed.status, 200,
            "case {label:?}: expected the listing to answer 200, got {} {:?}",
            listed.status, listed.body
        );
        assert_eq!(
            listed.listed_ids(),
            remaining,
            "case {label:?}: expected the templates not yet deleted to remain, got {:?}",
            listed.body
        );

        // Every record not yet deleted still reads back with what its own create wrote:
        // the delete touched one record only.
        for other in &cases {
            if remaining.contains(&other.job_template_id) {
                let still_there = emulator.describe_job_template(&other.job_template_id).await;
                assert_eq!(
                    (
                        still_there.status,
                        still_there
                            .body
                            .get("description")
                            .and_then(Value::as_str)
                            .map(str::to_string),
                    ),
                    (
                        200,
                        Some(format!("the template of case {}", other.case))
                    ),
                    "case {label:?}: deleting it must leave {:?} as it was, got {:?}",
                    other.job_template_id,
                    still_there.body
                );
            }
        }
    }

    // Nothing of the family is left: the deletes removed records rather than hiding them.
    let emptied = emulator.list_job_templates().await;
    assert!(
        emptied.listed_ids().is_empty(),
        "expected every template to be deleted, got {:?}",
        emptied.body
    );
}

/// The other half of the rule: the id a delete freed is a id a create may take again, and
/// the template created under it is a record of its own.
#[tokio::test]
async fn the_id_a_delete_freed_can_be_created_under_again() {
    for case in delete_cases() {
        let label = case.case;
        let id = &case.job_template_id;
        // A fresh account per row: what a row asserts must depend only on what it did.
        let emulator = Emulator::new();

        let created = emulator
            .create_job_template(id, &create_request(&case))
            .await;
        assert_eq!(
            created.status, 200,
            "case {label:?}: the first template must be creatable, got {} {:?}",
            created.status, created.body
        );
        let deleted = emulator.delete_job_template(id).await;
        assert_eq!(
            deleted.status, 200,
            "case {label:?}: expected the delete to answer 200, got {} {:?}",
            deleted.status, deleted.body
        );

        // The id is free: the create that would have been the 409 conflict now succeeds.
        let recreated = emulator
            .create_job_template(
                id,
                &json!({
                    "description": "the template created under the freed id",
                    "document": r#"{"operation":"install"}"#,
                    "timeoutConfig": { "inProgressTimeoutInMinutes": 7 },
                }),
            )
            .await;
        assert_eq!(
            recreated.status, 200,
            "case {label:?}: expected the freed id to be creatable again, got {} {:?}",
            recreated.status, recreated.body
        );

        let read = emulator.describe_job_template(id).await;
        assert_eq!(
            (
                read.status,
                read.body
                    .get("description")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                read.body.get("timeoutConfig").cloned(),
            ),
            (
                200,
                Some("the template created under the freed id".to_string()),
                Some(json!({ "inProgressTimeoutInMinutes": 7 })),
            ),
            "case {label:?}: expected the new record, not the deleted one, got {:?}",
            read.body
        );
    }
}

/// One row of the refusal table: how the id came to name no template.
struct MissingCase {
    case: &'static str,
    job_template_id: String,
    /// Whether a template is created under the id (and deleted) before the delete under
    /// test.
    created_then_deleted: bool,
}

/// Each way an id can name no template this account holds, over ids of different form and
/// length within the modelled pattern.
fn missing_cases() -> Vec<MissingCase> {
    vec![
        MissingCase {
            case: "an id nothing was ever created under",
            job_template_id: "never-created".to_string(),
            created_then_deleted: false,
        },
        MissingCase {
            case: "a single-character id nothing was ever created under",
            job_template_id: "q".to_string(),
            created_then_deleted: false,
        },
        MissingCase {
            case: "an id at the modelled maximum nothing was created under",
            job_template_id: "z".repeat(64),
            created_then_deleted: false,
        },
        MissingCase {
            case: "an id whose template was already deleted",
            job_template_id: "Deleted_Once".to_string(),
            created_then_deleted: true,
        },
    ]
}

/// A delete addressing an id this account holds no job template under is refused with the
/// jobs plane's not-found sentence and HTTP 404, never answered as a delete of nothing —
/// and the refusal leaves the rest of the family alone.
#[tokio::test]
async fn deleting_a_job_template_the_account_does_not_hold_is_the_plane_404() {
    for case in missing_cases() {
        let label = case.case;
        let id = &case.job_template_id;
        let emulator = Emulator::new();

        // A bystander template: a refused delete must not reach it.
        let bystander = "bystander";
        let created = emulator
            .create_job_template(
                bystander,
                &json!({ "description": "the bystander", "document": "{}" }),
            )
            .await;
        assert_eq!(
            created.status, 200,
            "case {label:?}: the bystander must be creatable, got {} {:?}",
            created.status, created.body
        );

        if case.created_then_deleted {
            let created = emulator
                .create_job_template(
                    id,
                    &json!({ "description": "the template to delete twice", "document": "{}" }),
                )
                .await;
            assert_eq!(
                created.status, 200,
                "case {label:?}: the template must be creatable, got {} {:?}",
                created.status, created.body
            );
            let first = emulator.delete_job_template(id).await;
            assert_eq!(
                first.status, 200,
                "case {label:?}: the first delete must answer 200, got {} {:?}",
                first.status, first.body
            );
        }

        let refused = emulator.delete_job_template(id).await;
        assert_eq!(
            refused.status, 404,
            "case {label:?}: expected status 404, got {} {:?}",
            refused.status, refused.body
        );
        assert!(
            refused
                .error_type
                .as_deref()
                .is_some_and(|code| code.starts_with("ResourceNotFoundException")),
            "case {label:?}: expected the ResourceNotFoundException shape, got {:?}",
            refused.error_type
        );
        assert_eq!(
            refused.message(),
            missing_template_message(id),
            "case {label:?}: expected the jobs plane's not-found sentence, got {:?}",
            refused.body
        );

        let listed = emulator.list_job_templates().await;
        assert_eq!(
            listed.listed_ids(),
            vec![bystander.to_string()],
            "case {label:?}: the refused delete must leave the family alone, got {:?}",
            listed.body
        );
    }
}
