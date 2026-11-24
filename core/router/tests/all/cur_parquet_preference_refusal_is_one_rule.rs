// SPDX-License-Identifier: CC0-1.0

//! The Parquet-preference refusal of the Cost and Usage Report Service, driven through
//! BOTH operations that take a `ReportDefinition`.
//!
//! RULE under test: **a definition whose Parquet preference is not usable — Parquet stated
//! by only one of `Format` and `Compression`, or stated by both beside a manifest built
//! from a delimited-text report — is refused under ONE rule, so `PutReportDefinition` and
//! `ModifyReportDefinition` answer it with the same HTTP status, the same modelled error
//! code and the same message, and a definition that satisfies it is stored by both.** The
//! two handlers no longer spell that refusal each: they call the one constructor in the
//! crate's `state` module, so the two answers this suite compares cannot drift apart.
//!
//! The rows span what the rule generalises over — which member states Parquet, the rest of
//! each member's modelled catalog (`smithy_query(shape="ReportFormat,CompressionFormat")`:
//! `textORcsv`/`Parquet` and `ZIP`/`GZIP`/`Parquet`), and which and how many manifests are
//! subscribed to over the whole modelled catalog
//! (`smithy_query(shape="AdditionalArtifact")`: `REDSHIFT`, `QUICKSIGHT`, `ATHENA`),
//! including none at all and the empty list. Each refused row's two answers are required
//! to be EQUAL to each other as well as to the wording, which is the invariant the shared
//! constructor exists for; each refused row is also required to leave the store exactly as
//! it was.
//!
//! Why it lives beside the router's suites: the service crate's one test target is rooted
//! at a hand-declared `tests/all.rs` that a fix may not edit, while a suite here needs no
//! declaration. The service is mounted on its own router, so what is asserted is what a
//! request to the assembled emulator observes.

use std::sync::Arc;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_costandusagereportservice_awsorigamiservicegatewayservice as cur;

/// The fixed text both paths state: it names the whole rule, and carries nothing from the
/// request.
const PARQUET_REFUSAL: &str =
    "For valid Parquet preference, report format and compression must be 'Parquet' \
     and cannot subscribe to QuickSight and/or Redshift";

/// The modelled error the condition is reported under — this is `ModifyReportDefinition`'s
/// only client-input error (`smithy_query(operation="ModifyReportDefinition")`).
const VALIDATION_EXCEPTION: &str = "ValidationException";

/// The one report name every row uses, so an update never also renames the report.
const REPORT_NAME: &str = "parquet-rule";

/// What one call answered, compared as a whole: a refused row's two paths must agree on
/// every part of it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Answer {
    status: u16,
    code: String,
    message: String,
}

/// One row: the Parquet preference a definition states, and whether it is usable.
struct Case {
    name: &'static str,
    format: &'static str,
    compression: &'static str,
    /// The `AdditionalArtifacts` the definition subscribes to, or `None` for a definition
    /// that leaves the member out altogether.
    artifacts: Option<Vec<&'static str>>,
    usable: bool,
}

/// One emulator: the shared engine and a router carrying only this service.
struct Cur {
    router: MultiServiceRouter,
}

impl Cur {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        // These requests are unsigned; signature verification is not what is under test.
        router.set_request_authentication(false);
        router.register_service(cur::EmulatedService::new(engine));
        Self { router }
    }

    /// Call one operation on the awsJson1_1 wire: `POST /` with
    /// `X-Amz-Target: AWSOrigamiServiceGatewayService.<OperationName>`, signed for the
    /// `cur` signing name the router routes on.
    async fn call(&self, operation: &str, body: &Value) -> (Answer, Value) {
        let request = http::Request::builder()
            .method("POST")
            .uri("/")
            .header("content-type", "application/x-amz-json-1.1")
            .header(
                "x-amz-target",
                format!("AWSOrigamiServiceGatewayService.{operation}"),
            )
            .header(
                "authorization",
                "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20240101/us-east-1/cur/aws4_request, \
                 SignedHeaders=host, Signature=deadbeef",
            )
            .body(bytes::Bytes::from(body.to_string()))
            .expect("build the request");

        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let header_type = response
            .headers()
            .get("x-amzn-errortype")
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        let parsed: Value = if response.body().is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(response.body()).unwrap_or(Value::Null)
        };
        let code = parsed
            .get("__type")
            .and_then(Value::as_str)
            .map(str::to_string)
            .or(header_type)
            .unwrap_or_default()
            // An error `__type` may be namespaced (`com.amazonaws.costandusagereportservice#Foo`).
            .rsplit('#')
            .next()
            .unwrap_or_default()
            .to_string();
        // The model names the member `Message`; the JSON protocols also render it lowercase.
        let message = ["Message", "message"]
            .into_iter()
            .find_map(|key| parsed.get(key))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        (
            Answer {
                status,
                code,
                message,
            },
            parsed,
        )
    }

    async fn put(&self, definition: &Value) -> (Answer, Value) {
        self.call(
            "PutReportDefinition",
            &json!({ "ReportDefinition": definition }),
        )
        .await
    }

    async fn modify(&self, definition: &Value) -> (Answer, Value) {
        self.call(
            "ModifyReportDefinition",
            &json!({ "ReportName": REPORT_NAME, "ReportDefinition": definition }),
        )
        .await
    }

    /// Every definition the account holds, as the service reports it.
    async fn stored(&self, case: &str) -> Vec<Value> {
        let (answer, body) = self.call("DescribeReportDefinitions", &json!({})).await;
        assert_eq!(
            answer.status, 200,
            "case {case:?}: reading the store back must answer HTTP 200, got {answer:?}"
        );
        body["ReportDefinitions"]
            .as_array()
            .unwrap_or_else(|| panic!("case {case:?}: expected a definition list, got {body:?}"))
            .clone()
    }
}

/// The required members of a report definition, with no Parquet preference of its own.
fn baseline_definition() -> Value {
    json!({
        "ReportName": REPORT_NAME,
        "TimeUnit": "DAILY",
        "Format": "textORcsv",
        "Compression": "GZIP",
        "AdditionalSchemaElements": ["RESOURCES"],
        "S3Bucket": "my-bucket",
        "S3Prefix": "cur/",
        "S3Region": "us-east-1",
    })
}

/// The row's definition: the baseline with the Parquet preference the row states.
fn definition_of(case: &Case) -> Value {
    let mut definition = baseline_definition();
    definition["Format"] = json!(case.format);
    definition["Compression"] = json!(case.compression);
    if let Some(artifacts) = &case.artifacts {
        definition["AdditionalArtifacts"] = json!(artifacts);
    }
    definition
}

/// What the Parquet preference of a stored definition reads back as: the two members that
/// state it and the manifests subscribed to, which the service reports as the empty list
/// for a definition that named none.
fn preference_of(definition: &Value) -> (Value, Value, Value) {
    (
        definition["Format"].clone(),
        definition["Compression"].clone(),
        definition["AdditionalArtifacts"].clone(),
    )
}

/// The preference a row's definition must read back as once stored.
fn expected_preference(case: &Case) -> (Value, Value, Value) {
    (
        json!(case.format),
        json!(case.compression),
        json!(case.artifacts.clone().unwrap_or_default()),
    )
}

/// The rows, spanning both members' modelled catalogs and the whole manifest catalog.
fn cases() -> Vec<Case> {
    vec![
        Case {
            name: "no Parquet preference at all",
            format: "textORcsv",
            compression: "GZIP",
            artifacts: None,
            usable: true,
        },
        Case {
            name: "no Parquet preference, subscribing to every manifest",
            format: "textORcsv",
            compression: "ZIP",
            artifacts: Some(vec!["REDSHIFT", "QUICKSIGHT", "ATHENA"]),
            usable: true,
        },
        Case {
            name: "a whole Parquet preference",
            format: "Parquet",
            compression: "Parquet",
            artifacts: None,
            usable: true,
        },
        Case {
            name: "a whole Parquet preference subscribing to no manifest",
            format: "Parquet",
            compression: "Parquet",
            artifacts: Some(Vec::new()),
            usable: true,
        },
        Case {
            name: "a whole Parquet preference with the Athena manifest",
            format: "Parquet",
            compression: "Parquet",
            artifacts: Some(vec!["ATHENA"]),
            usable: true,
        },
        Case {
            name: "Parquet stated by the format alone, gzipped",
            format: "Parquet",
            compression: "GZIP",
            artifacts: None,
            usable: false,
        },
        Case {
            name: "Parquet stated by the format alone, zipped",
            format: "Parquet",
            compression: "ZIP",
            artifacts: Some(vec!["ATHENA"]),
            usable: false,
        },
        Case {
            name: "Parquet stated by the compression alone",
            format: "textORcsv",
            compression: "Parquet",
            artifacts: None,
            usable: false,
        },
        Case {
            name: "a Parquet report subscribing to the Redshift manifest",
            format: "Parquet",
            compression: "Parquet",
            artifacts: Some(vec!["REDSHIFT"]),
            usable: false,
        },
        Case {
            name: "a Parquet report subscribing to the QuickSight manifest",
            format: "Parquet",
            compression: "Parquet",
            artifacts: Some(vec!["QUICKSIGHT"]),
            usable: false,
        },
        Case {
            name: "a Parquet report subscribing to Athena beside a delimited-text manifest",
            format: "Parquet",
            compression: "Parquet",
            artifacts: Some(vec!["ATHENA", "REDSHIFT"]),
            usable: false,
        },
        Case {
            name: "a Parquet report subscribing to every manifest",
            format: "Parquet",
            compression: "Parquet",
            artifacts: Some(vec!["REDSHIFT", "QUICKSIGHT", "ATHENA"]),
            usable: false,
        },
    ]
}

#[tokio::test]
async fn the_create_and_the_update_path_answer_one_parquet_preference_rule() {
    for case in cases() {
        let definition = definition_of(&case);
        let name = case.name;

        // The create path: an empty account taking the row's definition.
        let created = Cur::new();
        let (create_answer, create_body) = created.put(&definition).await;
        let after_create = created.stored(name).await;

        // The update path: an account holding the baseline definition, asked to replace it
        // with the row's one under the same name, so nothing but the preference changes.
        let updated = Cur::new();
        let (setup, setup_body) = updated.put(&baseline_definition()).await;
        assert_eq!(
            setup.status, 200,
            "case {name:?}: the setup create must answer HTTP 200, got {setup:?} {setup_body:?}"
        );
        let (update_answer, update_body) = updated.modify(&definition).await;
        let after_update = updated.stored(name).await;

        if case.usable {
            assert_eq!(
                (create_answer.status, update_answer.status),
                (200, 200),
                "case {name:?}: a usable Parquet preference must be accepted by both paths, \
                 got {create_answer:?} {create_body:?} and {update_answer:?} {update_body:?}"
            );
            let expected = expected_preference(&case);
            let paths = [
                ("PutReportDefinition", &after_create),
                ("ModifyReportDefinition", &after_update),
            ];
            for (path, stored) in paths {
                assert_eq!(
                    stored.len(),
                    1,
                    "case {name:?} via {path}: expected the one stored definition, got {stored:?}"
                );
                assert_eq!(
                    preference_of(&stored[0]),
                    expected,
                    "case {name:?} via {path}: expected the stored preference {expected:?}, \
                     got {:?}",
                    stored[0]
                );
            }
        } else {
            let refusal = Answer {
                status: 400,
                code: VALIDATION_EXCEPTION.to_string(),
                message: PARQUET_REFUSAL.to_string(),
            };
            assert_eq!(
                create_answer, refusal,
                "case {name:?} via PutReportDefinition: expected {refusal:?}, got \
                 {create_answer:?} {create_body:?}"
            );
            assert_eq!(
                update_answer, refusal,
                "case {name:?} via ModifyReportDefinition: expected {refusal:?}, got \
                 {update_answer:?} {update_body:?}"
            );
            // The rule itself: one condition, one refusal — whichever operation answered it.
            assert_eq!(
                create_answer, update_answer,
                "case {name:?}: both paths must answer the same refusal, got \
                 {create_answer:?} and {update_answer:?}"
            );
            assert!(
                after_create.is_empty(),
                "case {name:?}: a refused create must store nothing, got {after_create:?}"
            );
            assert_eq!(
                after_update.len(),
                1,
                "case {name:?}: a refused update must leave the stored definition, got \
                 {after_update:?}"
            );
            let baseline = preference_of(&json!({
                "Format": "textORcsv",
                "Compression": "GZIP",
                "AdditionalArtifacts": [],
            }));
            assert_eq!(
                preference_of(&after_update[0]),
                baseline,
                "case {name:?}: a refused update must leave the stored preference {baseline:?}, \
                 got {:?}",
                after_update[0]
            );
        }
    }
}
