// SPDX-License-Identifier: CC0-1.0

//! Configuring a `Test` on a service, end to end over HTTP: `CreateTest` -> `GetTest`.
//!
//! The rule under test: a test is created by configuring one of the test templates this
//! service publishes, and what it measures is the service's recovery against the
//! recovery-time objective in force for that service — so `CreateTest` is served only when
//! the resilience policy the service references states a multi-AZ RTO, and is refused
//! `ValidationException` (HTTP 400) when it does not, however the objective comes to be
//! missing: no policy attached, a `policyArn` that resolves to no stored policy, or a policy
//! that states other targets but not this one.
//!
//! What follows from the same reading:
//!
//! * the created `Test` is projected from what was stored — a minted `testId`, the
//!   configured template's ARN and NAME, the service it belongs to, the configuration the
//!   request supplied, and two run counters that start at zero because nothing has run it —
//!   and `GetTest` answers that same projection, so a create and a read cannot drift;
//! * "each service has one test per template", so a second create for a template the service
//!   already holds a test for is refused `ConflictException` (HTTP 409) while the same
//!   template on another service, or another template on the same service, is served;
//! * every reference is resolved before anything is stored: a `serviceArn` naming no service
//!   and a `testTemplateArn` naming no published template are refused
//!   `ResourceNotFoundException` (HTTP 404), and a refused create stores nothing.
//!
//! Every suite drives the real wire path (`MultiServiceRouter::dispatch`), so the modelled
//! constraints, the restJson1 (de)serializers and the HTTP status codes are exercised
//! exactly as a client would exercise them.
//!
//! The suites are TABLES: each row is one input of the class the rule covers (which targets
//! the policy states, which template is configured, which optional members the
//! configuration carries), not one recorded request.

use std::sync::Arc;

use serde_json::{json, Value};
use service_com_amazonaws_resiliencehubv2_ngrhservicecore::state::{
    multi_az_rto_required_message, test_for_template_exists_message,
};
use service_com_amazonaws_resiliencehubv2_ngrhservicecore::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// The modelled routes this suite drives.
const CREATE_POLICY: &str = "/v2/create-policy";
const DELETE_POLICY: &str = "/v2/delete-policy";
const CREATE_SERVICE: &str = "/v2/create-service";
const UPDATE_SERVICE: &str = "/v2/update-service";
const CREATE_TEST: &str = "/v2/create-test";
const GET_TEST: &str = "/v2/get-test";
const LIST_TEST_TEMPLATES: &str = "/v2/list-test-templates";

/// The Region the requests of this suite are signed for, and the one its ARNs are minted
/// in.
const REGION: &str = "us-east-1";

/// A SigV4 `Authorization` header whose credential scope names this service's signing name
/// (`resiliencehub`) and the Region ARNs are minted in. Signature verification is off for
/// these tests, so the value only has to parse.
const SIGNED: &str = "AWS4-HMAC-SHA256 \
     Credential=AKIDTESTONLY00000000/20260101/us-east-1/resiliencehub/aws4_request, \
     SignedHeaders=host;x-amz-date, Signature=0123456789abcdef";

/// A `serviceArn` of the modelled form (`Arn`: a 12-digit account segment) that names no
/// stored service.
const UNSTORED_SERVICE_ARN: &str =
    "arn:aws:resiliencehub:us-east-1:123456789012:service/00000000-0000-4000-8000-000000000000";

/// A `testTemplateArn` of the modelled form (`ServiceOwnedArn`, whose account segment may be
/// `aws`) that names no template this service publishes.
const UNPUBLISHED_TEMPLATE_ARN: &str =
    "arn:aws:resiliencehub:us-east-1:aws:test-template/aws-not-published:rt000";

/// An emulator holding only this service, over one shared engine.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(EmulatedService::new(engine));
    router
}

async fn call(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (u16, Value) {
    let payload = body
        .map(|value| serde_json::to_vec(&value).expect("a JSON body serializes"))
        .unwrap_or_default();
    let request = http::Request::builder()
        .method(method)
        .uri(uri)
        .header("authorization", SIGNED)
        .header("content-type", "application/json")
        .body(bytes::Bytes::from(payload))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let raw = response.into_body();
    let parsed = if raw.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&raw).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// The modelled error code of a refusal, from the restJson1 `__type` discriminator.
fn error_code(body: &Value) -> String {
    body["__type"]
        .as_str()
        .unwrap_or("<no __type>")
        .rsplit(['#', ':'])
        .next()
        .unwrap_or("<no __type>")
        .to_string()
}

/// Create the policy `targets` describes under `name` and answer its minted ARN.
async fn create_policy(router: &MultiServiceRouter, case: &str, name: &str, targets: &Value) -> String {
    let mut body = json!({"name": name});
    for (member, value) in targets.as_object().expect("the targets are an object") {
        body[member] = value.clone();
    }
    let (status, answered) = call(router, "POST", CREATE_POLICY, Some(body)).await;
    assert_eq!(
        status, 200,
        "case {case:?}: creating the policy expected HTTP 200, got {status} {answered:?}"
    );
    answered["policy"]["policyArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: no policy.policyArn in {answered:?}"))
        .to_string()
}

/// Create a service named `name`, optionally attaching `policy_arn`, and answer its ARN.
async fn create_service(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    policy_arn: Option<&str>,
) -> String {
    let mut body = json!({
        "name": name,
        "regions": [REGION],
        "permissionModel": {"invokerRoleName": "ResilienceHubRole"},
    });
    if let Some(policy_arn) = policy_arn {
        body["policyArn"] = json!(policy_arn);
    }
    let (status, answered) = call(router, "POST", CREATE_SERVICE, Some(body)).await;
    assert_eq!(
        status, 200,
        "case {case:?}: creating the service expected HTTP 200, got {status} {answered:?}"
    );
    answered["service"]["serviceArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: no service.serviceArn in {answered:?}"))
        .to_string()
}

async fn create_test(router: &MultiServiceRouter, body: Value) -> (u16, Value) {
    call(router, "POST", CREATE_TEST, Some(body)).await
}

async fn get_test(router: &MultiServiceRouter, service_arn: &str, test_id: &str) -> (u16, Value) {
    call(
        router,
        "GET",
        &format!("{GET_TEST}?serviceArn={service_arn}&testId={test_id}"),
        None,
    )
    .await
}

/// The templates this service publishes, as `(name, arn)` pairs, read through
/// `ListTestTemplates` rather than written out here — so every row below configures a
/// template the service really offers.
async fn published_templates(router: &MultiServiceRouter) -> Vec<(String, String)> {
    let (status, body) = call(router, "GET", LIST_TEST_TEMPLATES, None).await;
    assert_eq!(
        status, 200,
        "listing the test templates expected HTTP 200, got {status} {body:?}"
    );
    let listed = body["testTemplates"]
        .as_array()
        .unwrap_or_else(|| panic!("the @required testTemplates member is absent from {body:?}"))
        .clone();
    assert!(
        listed.len() >= 2,
        "the catalog needs at least two templates to test one-test-per-template, got {listed:?}"
    );
    listed
        .iter()
        .map(|entry| {
            (
                entry["name"]
                    .as_str()
                    .unwrap_or_else(|| panic!("no name in {entry:?}"))
                    .to_string(),
                entry["testTemplateArn"]
                    .as_str()
                    .unwrap_or_else(|| panic!("no testTemplateArn in {entry:?}"))
                    .to_string(),
            )
        })
        .collect()
}

/// The `test` member of a successful create or read.
fn test_of(case: &str, status: u16, body: &Value) -> Value {
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200, got {status} {body:?}"
    );
    let test = &body["test"];
    assert!(
        test.is_object(),
        "case {case:?}: the @required test member is absent from {body:?}"
    );
    test.clone()
}

/// One row of the policy table: which targets the policy the service references states, and
/// whether a create for that service is served.
struct PolicyRow {
    case: &'static str,
    /// The policy to create and attach, or `None` for a service with no policy at all.
    targets: Option<Value>,
    /// Whether the policy is DELETED after the service was created, leaving the service's
    /// `policyArn` pointing at no stored policy.
    then_deleted: bool,
    served: bool,
}

/// The policy table: rows differ along which of a policy's four target blocks are stated,
/// and along whether the policy the service points at still exists.
///
/// SOURCE of the target member names and their bounds:
/// `smithy_query(query="shape", shape="MultiAzTargets,MultiRegionTargets")` — `rtoInMinutes`
/// and `rpoInMinutes` are integers with `range min 0`, so `0` is the modelled minimum of an
/// RTO. The availability targets this service offers are the ones
/// `ALLOWED_AVAILABILITY_SLO_TARGETS` holds.
fn policy_rows() -> Vec<PolicyRow> {
    vec![
        PolicyRow {
            case: "no policy attached at all",
            targets: None,
            then_deleted: false,
            served: false,
        },
        PolicyRow {
            case: "a policy that states no targets",
            targets: Some(json!({})),
            then_deleted: false,
            served: false,
        },
        PolicyRow {
            case: "a policy that states only an availability SLO",
            targets: Some(json!({"availabilitySlo": {"target": 99.99}})),
            then_deleted: false,
            served: false,
        },
        PolicyRow {
            case: "a policy whose multi-AZ block states only an RPO",
            targets: Some(json!({"multiAz": {"rpoInMinutes": 5}})),
            then_deleted: false,
            served: false,
        },
        PolicyRow {
            case: "a policy whose multi-AZ block states only a DR approach",
            targets: Some(json!({"multiAz": {"disasterRecoveryApproach": "WARM_STANDBY"}})),
            then_deleted: false,
            served: false,
        },
        PolicyRow {
            case: "a policy that states a multi-Region RTO but no multi-AZ one",
            targets: Some(json!({"multiRegion": {"rtoInMinutes": 60}})),
            then_deleted: false,
            served: false,
        },
        PolicyRow {
            case: "a policy that states only a data-recovery target",
            targets: Some(json!({"dataRecovery": {"timeBetweenBackupsInMinutes": 1440}})),
            then_deleted: false,
            served: false,
        },
        PolicyRow {
            case: "a multi-AZ RTO at the member's modelled minimum",
            targets: Some(json!({"multiAz": {"rtoInMinutes": 0}})),
            then_deleted: false,
            served: true,
        },
        PolicyRow {
            case: "a multi-AZ RTO beside an RPO",
            targets: Some(json!({"multiAz": {"rtoInMinutes": 15, "rpoInMinutes": 5}})),
            then_deleted: false,
            served: true,
        },
        PolicyRow {
            case: "a policy that states every target",
            targets: Some(json!({
                "availabilitySlo": {"target": 99.9},
                "multiAz": {"rtoInMinutes": 30, "rpoInMinutes": 10, "disasterRecoveryApproach": "HOT_STANDBY"},
                "multiRegion": {"rtoInMinutes": 240, "rpoInMinutes": 60},
                "dataRecovery": {"timeBetweenBackupsInMinutes": 720},
            })),
            then_deleted: false,
            served: true,
        },
        PolicyRow {
            case: "a policy with a multi-AZ RTO that was deleted after it was attached",
            targets: Some(json!({"multiAz": {"rtoInMinutes": 45}})),
            then_deleted: true,
            served: false,
        },
    ]
}

/// RULE: a test is served only for a service whose resilience policy puts a multi-AZ RTO in
/// force; every other state of that policy is refused `ValidationException`, in the wording
/// this crate words that refusal with.
#[tokio::test]
async fn a_test_is_configured_only_against_a_multi_az_rto_in_force() {
    let router = emulator();
    let (_, template_arn) = published_templates(&router).await.remove(0);

    for (index, row) in policy_rows().into_iter().enumerate() {
        let case = row.case;
        let policy_arn = match row.targets.as_ref() {
            Some(targets) => {
                Some(create_policy(&router, case, &format!("Policy-{index}"), targets).await)
            }
            None => None,
        };
        let service_arn = create_service(
            &router,
            case,
            &format!("Service-{index}"),
            policy_arn.as_deref(),
        )
        .await;
        if row.then_deleted {
            let arn = policy_arn.as_deref().expect("the row states a policy");
            let (status, body) = call(
                &router,
                "POST",
                DELETE_POLICY,
                Some(json!({"policyArn": arn})),
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case:?}: deleting the policy expected HTTP 200, got {status} {body:?}"
            );
        }

        let (status, body) = create_test(
            &router,
            json!({"serviceArn": service_arn, "testTemplateArn": template_arn}),
        )
        .await;

        if row.served {
            let test = test_of(case, status, &body);
            assert_eq!(
                test["serviceArn"].as_str(),
                Some(service_arn.as_str()),
                "case {case:?}: expected the test to belong to {service_arn}, got {test:?}"
            );
            continue;
        }
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} {body:?}"
        );
        assert_eq!(
            error_code(&body),
            "ValidationException",
            "case {case:?}: expected a ValidationException, got {body:?}"
        );
        assert_eq!(
            body["message"].as_str(),
            Some(multi_az_rto_required_message(&service_arn).as_str()),
            "case {case:?}: unexpected refusal wording {:?}",
            body["message"]
        );
    }
}

/// One row of the configuration table: the optional members a `CreateTest` request carries
/// beside the two references.
struct ConfigurationRow {
    case: &'static str,
    configuration: Value,
}

/// The configuration table: rows differ along which optional members the request states —
/// none at all, each logging destination, the stop-condition sources the model publishes,
/// an execution role in both forms its pattern admits, and parameter values.
///
/// SOURCE of the member names and the values they admit:
/// `smithy_query(query="closure", operation="CreateTest")` — `StopConditionSource` is an enum
/// of exactly `aws:cloudwatch:alarm` and `none`, `IamRoleName` matches
/// `^([A-Za-z0-9_+=,.@\-]+/)*[A-Za-z0-9_+=,.@\-]+$` (so a path-prefixed name is admitted),
/// and `TestParameters` is a map of `len ..20` whose values are lists of `len ..10`.
fn configuration_rows() -> Vec<ConfigurationRow> {
    vec![
        ConfigurationRow {
            case: "no optional member at all",
            configuration: json!({}),
        },
        ConfigurationRow {
            case: "an S3 logging destination and a plain role name",
            configuration: json!({
                "loggingConfiguration": {"s3BucketName": "resilience-test-logs"},
                "roleName": "ResilienceHubTestRole",
            }),
        },
        ConfigurationRow {
            case: "a CloudWatch logging destination, a path-prefixed role and stop conditions",
            configuration: json!({
                "loggingConfiguration": {
                    "cloudWatchLogGroupArn":
                        "arn:aws:logs:us-east-1:123456789012:log-group/resilience-tests",
                    "logSchemaVersion": "1",
                },
                "roleName": "service-role/ResilienceHubTestRole",
                "stopConditions": [
                    {
                        "source": "aws:cloudwatch:alarm",
                        "value": "arn:aws:cloudwatch:us-east-1:123456789012:alarm:checkout-5xx",
                    },
                    {"source": "none", "value": "none"},
                ],
            }),
        },
        ConfigurationRow {
            case: "parameter values beside every other member",
            configuration: json!({
                "loggingConfiguration": {
                    "s3BucketName": "resilience-test-logs",
                    "logSchemaVersion": "2",
                },
                "roleName": "ResilienceHubTestRole",
                "stopConditions": [{"source": "none", "value": "none"}],
                "parameters": {
                    "availabilityZone": ["us-east-1a"],
                    "duration": ["PT10M"],
                    "targets": ["checkout", "cart", "catalog"],
                },
            }),
        },
    ]
}

/// RULE: the created `Test` is projected from what was stored — the configured template's
/// ARN and name, the service it belongs to, the configuration as supplied, and run counters
/// that start at zero — and `GetTest` answers that same projection.
#[tokio::test]
async fn a_configured_test_reads_back_through_get_test() {
    let router = emulator();
    let templates = published_templates(&router).await;
    let policy_arn = create_policy(
        &router,
        "round trip",
        "RoundTripPolicy",
        &json!({"multiAz": {"rtoInMinutes": 20}}),
    )
    .await;
    let service_arn =
        create_service(&router, "round trip", "RoundTripService", Some(&policy_arn)).await;

    // One service holding one test per template, so the reads below also prove the records
    // stay apart: a test is addressed by its own id within its service.
    for (row, (template_name, template_arn)) in
        configuration_rows().into_iter().zip(templates.iter())
    {
        let case = row.case;
        let mut body = json!({"serviceArn": service_arn, "testTemplateArn": template_arn});
        for (member, value) in row
            .configuration
            .as_object()
            .expect("the configuration is an object")
        {
            body[member] = value.clone();
        }

        let (status, answered) = create_test(&router, body.clone()).await;
        let created = test_of(case, status, &answered);

        let test_id = created["testId"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: no testId in {created:?}"));
        assert!(
            !test_id.is_empty(),
            "case {case:?}: testId is modelled len 1.., got {test_id:?}"
        );
        assert_eq!(
            created["testTemplateArn"].as_str(),
            Some(template_arn.as_str()),
            "case {case:?}: expected the configured template {template_arn}, got {created:?}"
        );
        assert_eq!(
            created["serviceArn"].as_str(),
            Some(service_arn.as_str()),
            "case {case:?}: expected the test to belong to {service_arn}, got {created:?}"
        );
        assert_eq!(
            created["name"].as_str(),
            Some(template_name.as_str()),
            "case {case:?}: expected the configured template's name {template_name:?}, got {:?}",
            created["name"]
        );
        assert_eq!(
            created["totalTestRuns"].as_i64(),
            Some(0),
            "case {case:?}: nothing has run the test, got {created:?}"
        );
        assert_eq!(
            created["successfulTestRuns"].as_i64(),
            Some(0),
            "case {case:?}: nothing has run the test, got {created:?}"
        );
        assert!(
            created["creationTime"].is_number(),
            "case {case:?}: the @required creationTime is absent from {created:?}"
        );
        // Every member the request configured comes back exactly as it was supplied, and a
        // member it did not configure is not reported at all.
        for member in ["loggingConfiguration", "stopConditions", "roleName", "parameters"] {
            assert_eq!(
                created[member], body[member],
                "case {case:?}: expected {member} to read back as configured, got {:?}",
                created[member]
            );
        }

        let (status, read) = get_test(&router, &service_arn, test_id).await;
        let read_back = test_of(case, status, &read);
        assert_eq!(
            read_back, created,
            "case {case:?}: GetTest answered a different test from the create"
        );
    }
}

/// RULE: "each service has one test per template" — a second create for a template the
/// service already holds a test for is a conflict, while another template on the same
/// service, and the same template on another service, are served.
#[tokio::test]
async fn a_service_holds_one_test_per_template() {
    let router = emulator();
    let templates = published_templates(&router).await;
    let policy_arn = create_policy(
        &router,
        "one per template",
        "OnePerTemplatePolicy",
        &json!({"multiAz": {"rtoInMinutes": 25}}),
    )
    .await;
    let first_service =
        create_service(&router, "one per template", "FirstService", Some(&policy_arn)).await;
    let second_service =
        create_service(&router, "one per template", "SecondService", Some(&policy_arn)).await;

    let (name, arn) = &templates[0];
    let (other_name, other_arn) = &templates[1];

    let (status, answered) = create_test(
        &router,
        json!({"serviceArn": first_service, "testTemplateArn": arn}),
    )
    .await;
    let created = test_of("the first test", status, &answered);
    let test_id = created["testId"].as_str().expect("a minted testId").to_string();

    // The same template again, configured differently: still the same one test per
    // template, so it is a conflict rather than a second test.
    let (status, body) = create_test(
        &router,
        json!({
            "serviceArn": first_service,
            "testTemplateArn": arn,
            "roleName": "AnotherRole",
        }),
    )
    .await;
    assert_eq!(
        status, 409,
        "a repeat for the same template expected HTTP 409, got {status} {body:?}"
    );
    assert_eq!(
        error_code(&body),
        "ConflictException",
        "a repeat for the same template expected a ConflictException, got {body:?}"
    );
    assert_eq!(
        body["message"].as_str(),
        Some(test_for_template_exists_message(arn, &first_service).as_str()),
        "unexpected refusal wording {:?}",
        body["message"]
    );

    // The refused repeat changed nothing: the stored test still reads back as it was
    // created, carrying the template's name and no `roleName`.
    let (status, read) = get_test(&router, &first_service, &test_id).await;
    let read_back = test_of("after the conflict", status, &read);
    assert_eq!(
        read_back, created,
        "the refused repeat must leave the stored test alone, got {read_back:?}"
    );

    // Another template on the SAME service is a different test.
    let (status, answered) = create_test(
        &router,
        json!({"serviceArn": first_service, "testTemplateArn": other_arn}),
    )
    .await;
    let other = test_of("another template", status, &answered);
    assert_eq!(
        other["name"].as_str(),
        Some(other_name.as_str()),
        "expected the other template's name {other_name:?}, got {other:?}"
    );
    assert_ne!(
        other["testId"], created["testId"],
        "two tests of one service carry different ids, got {other:?}"
    );

    // The SAME template on another service is a different test too: the one-per-template
    // rule is per service.
    let (status, answered) = create_test(
        &router,
        json!({"serviceArn": second_service, "testTemplateArn": arn}),
    )
    .await;
    let elsewhere = test_of("the same template on another service", status, &answered);
    assert_eq!(
        elsewhere["name"].as_str(),
        Some(name.as_str()),
        "expected the configured template's name {name:?}, got {elsewhere:?}"
    );
    assert_eq!(
        elsewhere["serviceArn"].as_str(),
        Some(second_service.as_str()),
        "expected the test to belong to {second_service}, got {elsewhere:?}"
    );
}

/// RULE: every reference is resolved against the store before anything is written, and a
/// value of the modelled form that names nothing stored is refused
/// `ResourceNotFoundException` naming what was not found.
#[tokio::test]
async fn references_that_name_nothing_are_refused() {
    let router = emulator();
    let (_, template_arn) = published_templates(&router).await.remove(0);
    let policy_arn = create_policy(
        &router,
        "unresolved references",
        "UnresolvedPolicy",
        &json!({"multiAz": {"rtoInMinutes": 35}}),
    )
    .await;
    let service_arn = create_service(
        &router,
        "unresolved references",
        "UnresolvedService",
        Some(&policy_arn),
    )
    .await;
    let (status, answered) = create_test(
        &router,
        json!({"serviceArn": service_arn, "testTemplateArn": template_arn}),
    )
    .await;
    let created = test_of("the stored test", status, &answered);
    let test_id = created["testId"].as_str().expect("a minted testId").to_string();

    // (case, the call, the resourceType the refusal reports)
    let rows: Vec<(&str, (u16, Value), &str)> = vec![
        (
            "a create whose serviceArn names no service",
            create_test(
                &router,
                json!({
                    "serviceArn": UNSTORED_SERVICE_ARN,
                    "testTemplateArn": template_arn,
                }),
            )
            .await,
            "Service",
        ),
        (
            "a create whose testTemplateArn names no published template",
            create_test(
                &router,
                json!({
                    "serviceArn": service_arn,
                    "testTemplateArn": UNPUBLISHED_TEMPLATE_ARN,
                }),
            )
            .await,
            "TestTemplate",
        ),
        (
            "a read whose serviceArn names no service",
            get_test(&router, UNSTORED_SERVICE_ARN, &test_id).await,
            "Service",
        ),
        (
            "a read whose testId names no test of the service",
            get_test(&router, &service_arn, "not-a-stored-test").await,
            "Test",
        ),
    ];

    for (case, (status, body), resource_type) in rows {
        assert_eq!(
            status, 404,
            "case {case:?}: expected HTTP 404, got {status} {body:?}"
        );
        assert_eq!(
            error_code(&body),
            "ResourceNotFoundException",
            "case {case:?}: expected a ResourceNotFoundException, got {body:?}"
        );
        assert_eq!(
            body["resourceType"].as_str(),
            Some(resource_type),
            "case {case:?}: expected resourceType {resource_type:?}, got {body:?}"
        );
    }

    // None of the refusals touched the stored test.
    let (status, read) = get_test(&router, &service_arn, &test_id).await;
    let read_back = test_of("after the refusals", status, &read);
    assert_eq!(
        read_back, created,
        "a refused call must leave the store alone, got {read_back:?}"
    );
}

/// RULE: a refused create stores nothing — once the service's policy states a multi-AZ RTO,
/// the very same create is served, which it could not be if the refused attempt had already
/// taken the service's one test for that template.
#[tokio::test]
async fn a_refused_create_stores_nothing() {
    let router = emulator();
    let (_, template_arn) = published_templates(&router).await.remove(0);
    let service_arn = create_service(&router, "refused then served", "RetriedService", None).await;
    let request = json!({"serviceArn": service_arn, "testTemplateArn": template_arn});

    let (status, body) = create_test(&router, request.clone()).await;
    assert_eq!(
        status, 400,
        "a service with no policy expected HTTP 400, got {status} {body:?}"
    );

    let policy_arn = create_policy(
        &router,
        "refused then served",
        "RetriedPolicy",
        &json!({"multiAz": {"rtoInMinutes": 40}}),
    )
    .await;
    let (status, body) = call(
        &router,
        "POST",
        UPDATE_SERVICE,
        Some(json!({"serviceArn": service_arn, "policyArn": policy_arn})),
    )
    .await;
    assert_eq!(
        status, 200,
        "attaching the policy expected HTTP 200, got {status} {body:?}"
    );

    let (status, answered) = create_test(&router, request).await;
    let created = test_of("the retried create", status, &answered);
    let test_id = created["testId"].as_str().expect("a minted testId");
    let (status, read) = get_test(&router, &service_arn, test_id).await;
    let read_back = test_of("the retried create", status, &read);
    assert_eq!(
        read_back, created,
        "the retried create must be readable through GetTest, got {read_back:?}"
    );
}
