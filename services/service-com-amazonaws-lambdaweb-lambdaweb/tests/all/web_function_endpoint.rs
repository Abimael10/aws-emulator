// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for a web function's endpoints: the initial one
//! `CreateWebFunction` creates from `endpointConfig`, the ones
//! `CreateWebFunctionEndpoint` adds to a stored function, and the
//! `GetWebFunctionEndpoint` read that reports either.
//!
//! Every assertion goes over the wire through the same [`Emulator`] harness the
//! `web_function_lifecycle` suite uses: a request is built, dispatched through the
//! production router, and only the HTTP status, the `x-amzn-errortype` header and
//! the JSON body are inspected. Nothing reaches into the service's state struct, so
//! an `endpoint` member that is not computed from persisted state cannot pass.
//!
//! The tests are tables: each row is one input along the axis the rule under test
//! generalises over (endpoint type, the Regions asked for, which optional members
//! the configuration carries, the name's form), and the `HomeRegion` + `IamAuth`
//! case from the issue is only one row.

use std::collections::BTreeSet;
use std::time::Duration;

use serde_json::{json, Value};

use service_com_amazonaws_lambdaweb_lambdaweb::state::endpoint_plane::serve_request;
use service_com_amazonaws_lambdaweb_lambdaweb::state::{
    service_state, MAX_ENDPOINTS_PER_FUNCTION, PROVISIONING_DELAY,
};

use super::endpoint_data_plane::{forbidden_page, REQUEST_ID_HEADER};
use super::web_function_lifecycle::{
    build_config, expected_arn, percent_encoded, role_arn, Emulator, Reply,
};

/// The region every test signs with: the endpoint's HOME Region, "the Region where
/// you created the web function".
const REGION: &str = "us-east-1";

/// How long a test waits before expecting an endpoint's clock-driven
/// `Pending -> Active` transition. An endpoint serves a revision, so it is ready one
/// span after that revision is itself ready — two spans in all — plus slack for a
/// loaded test machine.
fn endpoint_settle_delay() -> Duration {
    PROVISIONING_DELAY + PROVISIONING_DELAY + Duration::from_millis(250)
}

/// Whether `value` is an ADDRESS this emulator answers on, which is what a published
/// `domainName` has to be: the `host:port` of one of the emulator's own listeners, within
/// the model's own length bound for the member (4..256, source:
/// `smithy_query(query="shape", shape="DomainName")`).
///
/// The model's `DomainName` PATTERN (`^([a-z0-9-]+\.)+[a-z]+$`) is deliberately not
/// asserted, and the published value does not satisfy it: that shape is a property of
/// AWS's own environment — where the endpoint is served at port 443 under a zone AWS runs
/// — and what a client DOES with the value is dial it. A name under a zone this emulator
/// does not serve resolves to nothing on the machine it runs on, so the address the
/// emulator answers on is the one value that keeps the member's documented meaning. The
/// deviation is stated where the value is minted (`src/state/endpoints.rs`).
fn is_an_address_the_emulator_answers_on(value: &str) -> bool {
    let host = service_common::data_plane_addressing::host_only(value);
    let port = service_common::data_plane_addressing::addressed_port(Some(value));
    (4..=256).contains(&value.chars().count())
        && port.is_some_and(|port| port > 0)
        && !host.is_empty()
        && !host.contains(':')
}

/// A `revisionConfig` good enough to create the initial revision an endpoint
/// serves.
fn initial_revision(case: &str) -> Value {
    json!({
        "buildConfig": build_config("nodejs24.x", "demo-bucket", "app.zip", None),
        "serviceConfig": { "executionRoleArn": role_arn(case) },
    })
}

/// `PUT /web-functions` with the function, its initial revision and its initial
/// endpoint — the create path the API documents.
async fn create_with_endpoint(
    emulator: &Emulator,
    function_name: &str,
    endpoint_config: Value,
) -> Reply {
    emulator
        .send(
            "PUT",
            "/web-functions",
            REGION,
            Some(json!({
                "functionName": function_name,
                "revisionConfig": initial_revision(function_name),
                "endpointConfig": endpoint_config,
            })),
        )
        .await
}

/// `GET /web-functions/{functionName}/endpoints/{endpointName}`.
async fn get_endpoint(emulator: &Emulator, function_name: &str, endpoint_name: &str) -> Reply {
    emulator
        .send(
            "GET",
            &format!("/web-functions/{function_name}/endpoints/{endpoint_name}"),
            REGION,
            None,
        )
        .await
}

/// `GET /web-functions/{functionName}`.
async fn get_function(emulator: &Emulator, function_name: &str) -> Reply {
    emulator
        .send("GET", &format!("/web-functions/{function_name}"), REGION, None)
        .await
}

/// The `endpoint` member of a create answer.
fn endpoint_of(reply: &Reply) -> Value {
    reply
        .field("endpoint")
        .cloned()
        .unwrap_or_else(|| panic!("the create answer carries an endpoint, got {:?}", reply.body))
}

/// A member of a JSON object, or `Value::Null` when it is absent.
fn member(object: &Value, name: &str) -> Value {
    object.get(name).cloned().unwrap_or(Value::Null)
}

/// The `regions` list of an endpoint object, as a set — the order a listing takes is
/// not what the rule is about.
fn region_set(object: &Value) -> BTreeSet<String> {
    member(object, "regions")
        .as_array()
        .map(|regions| {
            regions
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// One row of the create-and-read-back table.
struct EndpointCase {
    case: &'static str,
    endpoint_name: &'static str,
    endpoint_type: &'static str,
    auth_type: &'static str,
    /// The `regions` member the request carries, or `None` when it omits it.
    requested_regions: Option<&'static [&'static str]>,
    /// The optional members the configuration carries, merged into it as sent.
    extra: fn() -> Value,
    /// The Regions the endpoint must report, the home one included.
    expected_regions: &'static [&'static str],
    /// The `autoDeploymentMode` the endpoint must report.
    expected_auto_deployment_mode: &'static str,
}

/// The table: the rule generalises over the endpoint type, the Regions asked for and
/// which optional members the configuration carries, so the rows differ along all
/// three. `scalingConfig.maxEnvironments` is `@range(2..10000)` and
/// `throttleConfig.rateLimit` is `@range(0..)` (model: shapes `ScalingConfig` /
/// `ThrottleConfig`), so the rows that carry them sit on those published bounds.
fn endpoint_cases() -> Vec<EndpointCase> {
    vec![
        EndpointCase {
            case: "home-region-minimal",
            endpoint_name: "prod",
            endpoint_type: "HomeRegion",
            auth_type: "IamAuth",
            requested_regions: None,
            extra: || json!({}),
            expected_regions: &[REGION],
            expected_auto_deployment_mode: "Disabled",
        },
        EndpointCase {
            case: "home-region-named-explicitly",
            endpoint_name: "home_1",
            endpoint_type: "HomeRegion",
            auth_type: "ApplicationManaged",
            requested_regions: Some(&[REGION]),
            extra: || {
                json!({
                    "description": "the home endpoint",
                    "autoDeploymentMode": "LatestRevision",
                    "scalingConfig": { "maxEnvironments": 2 },
                    "throttleConfig": { "rateLimit": 0 },
                })
            },
            expected_regions: &[REGION],
            expected_auto_deployment_mode: "LatestRevision",
        },
        EndpointCase {
            case: "multi-region",
            endpoint_name: "edge",
            endpoint_type: "MultiRegion",
            auth_type: "ApplicationManaged",
            requested_regions: Some(&["eu-west-1"]),
            extra: || {
                json!({
                    "scalingConfig": { "maxEnvironments": 10000 },
                    "throttleConfig": { "rateLimit": 10000 },
                })
            },
            // "The home Region is added automatically if you don't include it."
            expected_regions: &["eu-west-1", REGION],
            expected_auto_deployment_mode: "Disabled",
        },
        EndpointCase {
            case: "per-region",
            endpoint_name: "regional-endpoint",
            endpoint_type: "PerRegion",
            auth_type: "IamAuth",
            requested_regions: Some(&["us-west-2", "eu-central-1", REGION]),
            extra: || json!({ "description": "" }),
            expected_regions: &["eu-central-1", "us-west-2", REGION],
            expected_auto_deployment_mode: "Disabled",
        },
        EndpointCase {
            // The longest name the member documents: exactly 64 characters.
            case: "longest-endpoint-name",
            endpoint_name: "e0123456789012345678901234567890123456789012345678901234567890ab",
            endpoint_type: "HomeRegion",
            auth_type: "IamAuth",
            requested_regions: None,
            extra: || json!({}),
            expected_regions: &[REGION],
            expected_auto_deployment_mode: "Disabled",
        },
    ]
}

/// The `endpointConfig` a row sends.
fn endpoint_config(row: &EndpointCase) -> Value {
    let mut config = json!({
        "endpointName": row.endpoint_name,
        "endpointType": row.endpoint_type,
        "authType": row.auth_type,
    });
    if let Some(regions) = row.requested_regions {
        config["regions"] = json!(regions);
    }
    for (name, value) in (row.extra)().as_object().expect("extra is an object") {
        config[name] = value.clone();
    }
    config
}

#[tokio::test]
async fn an_initial_endpoint_is_created_with_the_function_and_read_back() {
    for row in endpoint_cases() {
        let case = row.case;
        let emulator = Emulator::new();
        let function_name = format!("fn-{case}");
        let created = create_with_endpoint(&emulator, &function_name, endpoint_config(&row)).await;
        assert_eq!(
            created.status, 202,
            "case {case:?}: CreateWebFunction with endpointConfig expected 202, got {} with {:?}",
            created.status, created.body
        );

        let endpoint = endpoint_of(&created);
        let function_arn = expected_arn(REGION, &function_name);
        assert_eq!(
            member(&endpoint, "endpointArn"),
            json!(format!("{function_arn}/endpoint/{}", row.endpoint_name)),
            "case {case:?}: the endpoint ARN is derived from the function's, got {endpoint:?}"
        );
        assert_eq!(
            member(&endpoint, "endpointName"),
            json!(row.endpoint_name),
            "case {case:?}: expected the stored endpoint name, got {endpoint:?}"
        );
        assert_eq!(
            member(&endpoint, "endpointType"),
            json!(row.endpoint_type),
            "case {case:?}: expected the stored endpoint type, got {endpoint:?}"
        );
        assert_eq!(
            member(&endpoint, "authType"),
            json!(row.auth_type),
            "case {case:?}: expected the stored auth type, got {endpoint:?}"
        );
        assert_eq!(
            member(&endpoint, "autoDeploymentMode"),
            json!(row.expected_auto_deployment_mode),
            "case {case:?}: expected autoDeploymentMode {:?}, got {endpoint:?}",
            row.expected_auto_deployment_mode
        );
        assert_eq!(
            region_set(&endpoint),
            row.expected_regions
                .iter()
                .map(|region| (*region).to_string())
                .collect::<BTreeSet<String>>(),
            "case {case:?}: expected the Regions {:?}, got {endpoint:?}",
            row.expected_regions
        );
        assert_eq!(
            member(&endpoint, "state"),
            json!("Pending"),
            "case {case:?}: a just-created endpoint is still Pending, got {endpoint:?}"
        );
        assert!(
            member(&endpoint, "stateReason")
                .as_str()
                .is_some_and(|reason| !reason.is_empty()),
            "case {case:?}: expected a non-empty stateReason, got {endpoint:?}"
        );
        let domain_name = member(&endpoint, "domainName")
            .as_str()
            .unwrap_or_default()
            .to_string();
        assert!(
            is_an_address_the_emulator_answers_on(&domain_name),
            "case {case:?}: expected a domainName the emulator answers on, got \
             {domain_name:?}"
        );

        // The initial endpoint serves the initial revision, with all of the traffic:
        // it is the only revision there is to route to.
        let revision_id = created
            .field("revision")
            .and_then(|revision| revision.get("revisionId"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        assert!(
            !revision_id.is_empty(),
            "case {case:?}: the create reports its initial revision, got {:?}",
            created.body
        );
        assert_eq!(
            member(&endpoint, "revisionWeights"),
            json!([{ "revisionId": revision_id, "weight": 100 }]),
            "case {case:?}: expected the initial revision to carry all the traffic, got \
             {endpoint:?}"
        );

        // The read-back: the endpoint operations of the same service see the
        // endpoint the create stored.
        let fetched = get_endpoint(&emulator, &function_name, row.endpoint_name).await;
        assert_eq!(
            fetched.status, 200,
            "case {case:?}: GetWebFunctionEndpoint expected 200, got {} with {:?}",
            fetched.status, fetched.body
        );
        assert_eq!(
            fetched.string("functionArn"),
            Some(function_arn.clone()),
            "case {case:?}: the endpoint belongs to the created function, got {:?}",
            fetched.body
        );
        for name in [
            "endpointArn",
            "endpointName",
            "description",
            "endpointType",
            "domainName",
            "authType",
            "autoDeploymentMode",
            "revisionWeights",
            "regions",
            "scalingConfig",
            "throttleConfig",
            "createdAt",
        ] {
            assert_eq!(
                member(&fetched.body, name),
                member(&endpoint, name),
                "case {case:?}: GetWebFunctionEndpoint reports a different {name} than the \
                 create did, got {:?}",
                fetched.body
            );
        }

        // One regional entry per configured Region. Every Region of one endpoint is
        // served at the ONE address that endpoint publishes — this emulator answers all
        // of its traffic there — so each regional entry reports that address;
        // `MultiRegion` routes one domain to the nearest Region, so its regional entries
        // carry none.
        let regional = member(&fetched.body, "regionalEndpoints");
        let regional = regional
            .as_object()
            .unwrap_or_else(|| panic!("case {case:?}: expected regionalEndpoints, got {regional:?}"));
        assert_eq!(
            regional.keys().cloned().collect::<BTreeSet<String>>(),
            region_set(&endpoint),
            "case {case:?}: expected one regional entry per Region, got {regional:?}"
        );
        for (region, entry) in regional {
            assert_eq!(
                member(entry, "authType"),
                json!(row.auth_type),
                "case {case:?}: regional entry {region} reports another auth type, got {entry:?}"
            );
            assert_eq!(
                member(entry, "revisionWeights"),
                member(&endpoint, "revisionWeights"),
                "case {case:?}: regional entry {region} serves another revision, got {entry:?}"
            );
            match member(entry, "domainName").as_str() {
                None => assert_eq!(
                    row.endpoint_type, "MultiRegion",
                    "case {case:?}: regional entry {region} must carry its own domain, got \
                     {entry:?}"
                ),
                Some(regional_domain) => assert_eq!(
                    regional_domain, domain_name,
                    "case {case:?}: regional entry {region} must report the address the \
                     endpoint is served at, got {regional_domain:?}"
                ),
            }
        }
    }
}

#[tokio::test]
async fn an_endpoint_can_be_named_by_name_or_by_arn_and_an_unknown_one_is_404() {
    // The member's documentation says the caller "can specify either of the
    // following: The endpoint name, for example my-endpoint. The endpoint ARN."
    // (source: smithy_query doc for GetWebFunctionEndpointRequest$endpointName).
    // The ARN rides in the URI label percent-encoded, the way an SDK sends it.
    let emulator = Emulator::new();
    let function_name = "named-either-way";
    let created = create_with_endpoint(
        &emulator,
        function_name,
        json!({ "endpointName": "prod", "endpointType": "HomeRegion", "authType": "IamAuth" }),
    )
    .await;
    assert_eq!(
        created.status, 202,
        "CreateWebFunction expected 202, got {} with {:?}",
        created.status, created.body
    );
    let endpoint_arn = member(&endpoint_of(&created), "endpointArn")
        .as_str()
        .unwrap_or_default()
        .to_string();
    let encode = percent_encoded;

    for spelling in ["prod".to_string(), encode(&endpoint_arn)] {
        let fetched = get_endpoint(&emulator, function_name, &spelling).await;
        assert_eq!(
            fetched.status, 200,
            "case {spelling:?}: GetWebFunctionEndpoint expected 200, got {} with {:?}",
            fetched.status, fetched.body
        );
        assert_eq!(
            fetched.string("endpointArn"),
            Some(endpoint_arn.clone()),
            "case {spelling:?}: both spellings name one record, got {:?}",
            fetched.body
        );
    }
    // The function may equally be named by its ARN.
    let by_function_arn =
        get_endpoint(&emulator, &encode(&expected_arn(REGION, function_name)), "prod").await;
    assert_eq!(
        by_function_arn.status, 200,
        "case function-arn: GetWebFunctionEndpoint expected 200, got {} with {:?}",
        by_function_arn.status, by_function_arn.body
    );

    // An endpoint nobody created, and an endpoint on a function nobody created, are
    // both 404 ResourceNotFoundException — never the scaffold's 501.
    let cases = [(function_name, "absent"), ("no-such-function", "prod")];
    for (function, endpoint) in cases {
        let missing = get_endpoint(&emulator, function, endpoint).await;
        assert_eq!(
            missing.status, 404,
            "case {function:?}/{endpoint:?}: expected 404, got {} with {:?}",
            missing.status, missing.body
        );
        assert_eq!(
            missing.error(),
            "ResourceNotFoundException",
            "case {function:?}/{endpoint:?}: expected ResourceNotFoundException, got {:?}",
            missing.body
        );
    }
}

#[tokio::test]
async fn an_endpoint_configuration_outside_the_documented_rules_is_refused_and_stores_nothing() {
    // The rules, neither of which the model can express:
    //
    // * `endpointName`: "The name can contain letters, numbers, hyphens (-), and
    //   underscores (_), and can't begin or end with a hyphen or an underscore. The
    //   name is limited to 64 characters in length."
    // * `regions`: "Required when the endpoint type is MultiRegion or PerRegion:
    //   specify at least one Region other than the Region where you create the
    //   endpoint (the home Region). ... When the endpoint type is HomeRegion, omit
    //   this field or specify only the home Region."
    //
    // (source: smithy_query doc for EndpointConfig$endpointName / $regions.) The
    // rows walk each clause, including a name one character past the published 64.
    let long_name = "e".repeat(65);
    let cases: Vec<(&str, Value)> = vec![
        (
            "name-leading-hyphen",
            json!({ "endpointName": "-prod", "endpointType": "HomeRegion", "authType": "IamAuth" }),
        ),
        (
            "name-trailing-underscore",
            json!({ "endpointName": "prod_", "endpointType": "HomeRegion", "authType": "IamAuth" }),
        ),
        (
            "name-illegal-character",
            json!({ "endpointName": "pro.d", "endpointType": "HomeRegion", "authType": "IamAuth" }),
        ),
        (
            "name-one-past-the-bound",
            json!({ "endpointName": long_name, "endpointType": "HomeRegion", "authType": "IamAuth" }),
        ),
        (
            "home-region-with-another-region",
            json!({
                "endpointName": "prod",
                "endpointType": "HomeRegion",
                "authType": "IamAuth",
                "regions": ["eu-west-1"],
            }),
        ),
        (
            "multi-region-without-regions",
            json!({ "endpointName": "edge", "endpointType": "MultiRegion", "authType": "IamAuth" }),
        ),
        (
            "per-region-with-only-the-home-region",
            json!({
                "endpointName": "edge",
                "endpointType": "PerRegion",
                "authType": "IamAuth",
                "regions": [REGION],
            }),
        ),
    ];

    for (case, config) in cases {
        let emulator = Emulator::new();
        let function_name = format!("refused-{case}");
        let reply = create_with_endpoint(&emulator, &function_name, config).await;
        assert_eq!(
            reply.status, 400,
            "case {case:?}: expected 400, got {} with {:?}",
            reply.status, reply.body
        );
        assert_eq!(
            reply.error(),
            "ValidationException",
            "case {case:?}: expected ValidationException, got {:?}",
            reply.body
        );
        let fetched = get_function(&emulator, &function_name).await;
        assert_eq!(
            fetched.status, 404,
            "case {case:?}: a refused create stores nothing, got {} with {:?}",
            fetched.status, fetched.body
        );
    }

    // An endpoint serves revisions, and the model requires at least one
    // `revisionWeights` entry on every endpoint it reports, so an initial endpoint
    // without an initial revision is refused the same way.
    let emulator = Emulator::new();
    let reply = emulator
        .send(
            "PUT",
            "/web-functions",
            REGION,
            Some(json!({
                "functionName": "endpoint-without-revision",
                "endpointConfig": {
                    "endpointName": "prod",
                    "endpointType": "HomeRegion",
                    "authType": "IamAuth",
                },
            })),
        )
        .await;
    assert_eq!(
        reply.status, 400,
        "case no-revision-config: expected 400, got {} with {:?}",
        reply.status, reply.body
    );
    assert_eq!(
        reply.error(),
        "ValidationException",
        "case no-revision-config: expected ValidationException, got {:?}",
        reply.body
    );
    let fetched = get_function(&emulator, "endpoint-without-revision").await;
    assert_eq!(
        fetched.status, 404,
        "case no-revision-config: a refused create stores nothing, got {} with {:?}",
        fetched.status, fetched.body
    );
}

#[tokio::test]
async fn a_create_that_asks_for_no_endpoint_reports_none() {
    // The `endpoint` member is the endpoint the call created: a create that asked
    // for none reports none, and no endpoint reader finds one.
    let emulator = Emulator::new();
    let created = emulator
        .send(
            "PUT",
            "/web-functions",
            REGION,
            Some(json!({
                "functionName": "no-endpoint",
                "revisionConfig": initial_revision("no-endpoint"),
            })),
        )
        .await;
    assert_eq!(
        created.status, 202,
        "CreateWebFunction expected 202, got {} with {:?}",
        created.status, created.body
    );
    assert!(
        created.field("endpoint").is_none(),
        "case no-endpoint: expected no endpoint member, got {:?}",
        created.body
    );
    let fetched = get_endpoint(&emulator, "no-endpoint", "prod").await;
    assert_eq!(
        fetched.status, 404,
        "case no-endpoint: expected 404, got {} with {:?}",
        fetched.status, fetched.body
    );
}

#[tokio::test]
async fn an_endpoint_reaches_active_on_the_clock_and_then_stays_put() {
    // The rule: a resource this service reports in a non-terminal state reaches its
    // terminal one after a span the clock measures — never instantly, and never
    // because somebody read it. The rows differ in the endpoint type, which is what
    // changes how many Regions the endpoint spans.
    for (case, config) in [
        (
            "home-region",
            json!({ "endpointName": "prod", "endpointType": "HomeRegion", "authType": "IamAuth" }),
        ),
        (
            "multi-region",
            json!({
                "endpointName": "edge",
                "endpointType": "MultiRegion",
                "authType": "ApplicationManaged",
                "regions": ["eu-west-1"],
            }),
        ),
    ] {
        let emulator = Emulator::new();
        let function_name = format!("clock-{case}");
        let endpoint_name = config
            .get("endpointName")
            .and_then(Value::as_str)
            .expect("the row names its endpoint")
            .to_string();
        let created = create_with_endpoint(&emulator, &function_name, config.clone()).await;
        assert_eq!(
            created.status, 202,
            "case {case:?}: CreateWebFunction expected 202, got {} with {:?}",
            created.status, created.body
        );
        // Straight after the call the endpoint is still Pending: the span is never
        // zero, so a caller acting immediately sees the transient state.
        let pending = get_endpoint(&emulator, &function_name, &endpoint_name).await;
        assert_eq!(
            member(&pending.body, "state"),
            json!("Pending"),
            "case {case:?}: expected Pending before the span elapsed, got {:?}",
            pending.body
        );

        tokio::time::sleep(endpoint_settle_delay()).await;

        let active = get_endpoint(&emulator, &function_name, &endpoint_name).await;
        assert_eq!(
            member(&active.body, "state"),
            json!("Active"),
            "case {case:?}: expected Active once the span elapsed, got {:?}",
            active.body
        );
        for (region, entry) in member(&active.body, "regionalEndpoints")
            .as_object()
            .expect("regionalEndpoints")
        {
            assert_eq!(
                member(entry, "state"),
                json!("Active"),
                "case {case:?}: regional entry {region} lags the endpoint, got {entry:?}"
            );
        }
        assert_eq!(
            member(&active.body, "createdAt"),
            member(&pending.body, "createdAt"),
            "case {case:?}: createdAt moved, got {:?}",
            active.body
        );

        // A second read reports the same instant: the transition was owed to the
        // clock, not minted by the read.
        let again = get_endpoint(&emulator, &function_name, &endpoint_name).await;
        assert_eq!(
            member(&again.body, "updatedAt"),
            member(&active.body, "updatedAt"),
            "case {case:?}: updatedAt moved between two reads, got {:?}",
            again.body
        );
    }
}

#[tokio::test]
async fn a_deleted_function_takes_its_endpoint_with_it() {
    // The rule generalises over the endpoint type: whatever the endpoint spans, it
    // lives under the function and goes when the function goes.
    for (case, config) in [
        (
            "home-region",
            json!({ "endpointName": "prod", "endpointType": "HomeRegion", "authType": "IamAuth" }),
        ),
        (
            "per-region",
            json!({
                "endpointName": "regional",
                "endpointType": "PerRegion",
                "authType": "IamAuth",
                "regions": ["us-west-2"],
            }),
        ),
    ] {
        let emulator = Emulator::new();
        let function_name = format!("cascade-{case}");
        let endpoint_name = config
            .get("endpointName")
            .and_then(Value::as_str)
            .expect("the row names its endpoint")
            .to_string();
        let created = create_with_endpoint(&emulator, &function_name, config.clone()).await;
        assert_eq!(
            created.status, 202,
            "case {case:?}: CreateWebFunction expected 202, got {} with {:?}",
            created.status, created.body
        );

        let deleted = emulator
            .send(
                "DELETE",
                &format!("/web-functions/{function_name}"),
                REGION,
                None,
            )
            .await;
        assert_eq!(
            deleted.status, 204,
            "case {case:?}: DeleteWebFunction expected 204, got {} with {:?}",
            deleted.status, deleted.body
        );

        let fetched = get_endpoint(&emulator, &function_name, &endpoint_name).await;
        assert_eq!(
            fetched.status, 404,
            "case {case:?}: the endpoint outlived its function, got {} with {:?}",
            fetched.status, fetched.body
        );

        // And the name is free again, endpoint included.
        let recreated = create_with_endpoint(&emulator, &function_name, config).await;
        assert_eq!(
            recreated.status, 202,
            "case {case:?}: re-creating the function expected 202, got {} with {:?}",
            recreated.status, recreated.body
        );
        assert_eq!(
            member(&endpoint_of(&recreated), "endpointName"),
            json!(endpoint_name),
            "case {case:?}: the fresh function carries the endpoint it asked for, got {:?}",
            recreated.body
        );
    }
}

// ---------------------------------------------------------------------------
// `CreateWebFunctionEndpoint`: an endpoint added to a web function that already
// exists. It is stored in the SAME endpoint slice `CreateWebFunction`'s initial
// endpoint is stored in, so every rule asserted above holds of it too, and the
// endpoint readers of this service report one copy of either.
// ---------------------------------------------------------------------------

/// `PUT /web-functions/{functionName}/endpoints` — `CreateWebFunctionEndpoint`
/// (model: `smithy_query(query="operation", operation="CreateWebFunctionEndpoint")`).
async fn create_endpoint(emulator: &Emulator, function_name: &str, body: Value) -> Reply {
    emulator
        .send(
            "PUT",
            &format!("/web-functions/{function_name}/endpoints"),
            REGION,
            Some(body),
        )
        .await
}

/// `POST /web-functions/{functionName}/revisions`, for a row that needs a second
/// revision to split traffic across.
async fn create_revision(emulator: &Emulator, function_name: &str) -> String {
    let created = emulator
        .send(
            "POST",
            &format!("/web-functions/{function_name}/revisions"),
            REGION,
            Some(initial_revision(function_name)),
        )
        .await;
    assert_eq!(
        created.status, 202,
        "CreateWebFunctionRevision {function_name}: expected 202, got {} with {:?}",
        created.status, created.body
    );
    created
        .string("revisionId")
        .unwrap_or_else(|| panic!("the revision create reports a revisionId, got {:?}", created.body))
}

/// A web function with one revision and NO endpoint — what
/// `CreateWebFunctionEndpoint` adds one to — and that revision's `revisionId`.
async fn function_with_a_revision(emulator: &Emulator, function_name: &str) -> String {
    let created = emulator
        .send(
            "PUT",
            "/web-functions",
            REGION,
            Some(json!({
                "functionName": function_name,
                "revisionConfig": initial_revision(function_name),
            })),
        )
        .await;
    assert_eq!(
        created.status, 202,
        "CreateWebFunction {function_name}: expected 202, got {} with {:?}",
        created.status, created.body
    );
    created
        .field("revision")
        .and_then(|revision| revision.get("revisionId"))
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("the create reports its initial revision, got {:?}", created.body))
        .to_string()
}

/// Poll `GetWebFunctionEndpoint` until the endpoint reports `Active`, and report that
/// read.
///
/// The `Pending -> Active` transition is owed to the CLOCK, so the test waits for the
/// condition to hold rather than for a duration: it looks again until it does, or
/// until a deadline far longer than the service's own span has passed.
pub(crate) async fn poll_until_active(
    emulator: &Emulator,
    function_name: &str,
    endpoint_name: &str,
) -> Value {
    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    loop {
        let read = get_endpoint(emulator, function_name, endpoint_name).await;
        if member(&read.body, "state") == json!("Active") {
            return read.body;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "case {function_name}/{endpoint_name}: the endpoint never reached Active, last \
             read {:?}",
            read.body
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

/// One row of the `CreateWebFunctionEndpoint` table: the request's own members, and
/// what the stored endpoint has to report.
struct CreateCase {
    case: &'static str,
    endpoint_name: &'static str,
    endpoint_type: &'static str,
    auth_type: &'static str,
    /// The `regions` member the request carries, or `None` when it omits it.
    requested_regions: Option<&'static [&'static str]>,
    /// The optional members the request carries, merged into it as sent.
    extra: fn() -> Value,
    /// The Regions the endpoint must report, the home one included.
    expected_regions: &'static [&'static str],
    /// The `autoDeploymentMode` the endpoint must report.
    expected_auto_deployment_mode: &'static str,
    /// Whether the request names the function by its ARN rather than by its name.
    by_function_arn: bool,
}

/// The table: the rule generalises over the endpoint type, the Regions asked for,
/// which optional members the request carries, the form of the name and how the
/// function is identified, so the rows differ along all five. The published bounds of
/// `scalingConfig.maxEnvironments` (`@range(2..10000)`) and `throttleConfig.rateLimit`
/// (`@range(0..)`) carry the rows that set them.
fn create_cases() -> Vec<CreateCase> {
    vec![
        CreateCase {
            case: "home-region-minimal",
            endpoint_name: "second",
            endpoint_type: "HomeRegion",
            auth_type: "IamAuth",
            requested_regions: None,
            extra: || json!({}),
            expected_regions: &[REGION],
            expected_auto_deployment_mode: "Disabled",
            by_function_arn: false,
        },
        CreateCase {
            case: "home-region-every-optional-member",
            endpoint_name: "canary_1",
            endpoint_type: "HomeRegion",
            auth_type: "ApplicationManaged",
            requested_regions: Some(&[REGION]),
            extra: || {
                json!({
                    "description": "an endpoint added later",
                    "autoDeploymentMode": "LatestRevision",
                    "scalingConfig": { "maxEnvironments": 2 },
                    "throttleConfig": { "rateLimit": 0 },
                })
            },
            expected_regions: &[REGION],
            expected_auto_deployment_mode: "LatestRevision",
            by_function_arn: false,
        },
        CreateCase {
            case: "multi-region-by-function-arn",
            endpoint_name: "edge",
            endpoint_type: "MultiRegion",
            auth_type: "ApplicationManaged",
            requested_regions: Some(&["eu-west-1", "ap-south-1"]),
            extra: || {
                json!({
                    "scalingConfig": { "maxEnvironments": 10000 },
                    "throttleConfig": { "rateLimit": 10000 },
                })
            },
            // "The home Region is added automatically if you don't include it."
            expected_regions: &["ap-south-1", "eu-west-1", REGION],
            expected_auto_deployment_mode: "Disabled",
            by_function_arn: true,
        },
        CreateCase {
            case: "per-region",
            endpoint_name: "regional-endpoint",
            endpoint_type: "PerRegion",
            auth_type: "IamAuth",
            requested_regions: Some(&["us-west-2", REGION]),
            extra: || json!({ "description": "" }),
            expected_regions: &["us-west-2", REGION],
            expected_auto_deployment_mode: "Disabled",
            by_function_arn: false,
        },
        CreateCase {
            // The longest name the member documents: exactly 64 characters.
            case: "longest-endpoint-name",
            endpoint_name: "e0123456789012345678901234567890123456789012345678901234567890ab",
            endpoint_type: "HomeRegion",
            auth_type: "IamAuth",
            requested_regions: None,
            extra: || json!({}),
            expected_regions: &[REGION],
            expected_auto_deployment_mode: "Disabled",
            by_function_arn: false,
        },
    ]
}

/// The request body a row sends.
fn create_body(row: &CreateCase) -> Value {
    let mut body = json!({
        "endpointName": row.endpoint_name,
        "endpointType": row.endpoint_type,
        "authType": row.auth_type,
    });
    if let Some(regions) = row.requested_regions {
        body["regions"] = json!(regions);
    }
    for (name, value) in (row.extra)().as_object().expect("extra is an object") {
        body[name] = value.clone();
    }
    body
}

/// RULE: a create of an endpoint on a stored web function persists that endpoint in
/// the function's own endpoint slice and reports it from there — its ARN derived from
/// the function's, its Regions and configuration as the documented rules settle them,
/// `Pending` until the clock owes it `Active`, and a `domainName` this emulator
/// answers on — so the endpoint readers of this service report the same record.
#[tokio::test]
async fn an_endpoint_created_on_a_stored_function_is_persisted_and_read_back() {
    for row in create_cases() {
        let case = row.case;
        let emulator = Emulator::new();
        let function_name = format!("added-{case}");
        let revision_id = function_with_a_revision(&emulator, &function_name).await;
        let function_arn = expected_arn(REGION, &function_name);
        let identifier = if row.by_function_arn {
            percent_encoded(&function_arn)
        } else {
            function_name.clone()
        };

        let created = create_endpoint(&emulator, &identifier, create_body(&row)).await;
        assert_eq!(
            created.status, 202,
            "case {case:?}: CreateWebFunctionEndpoint expected 202, got {} with {:?}",
            created.status, created.body
        );
        assert_eq!(
            created.string("functionArn"),
            Some(function_arn.clone()),
            "case {case:?}: the endpoint belongs to the named function, got {:?}",
            created.body
        );
        assert_eq!(
            created.string("endpointArn"),
            Some(format!("{function_arn}/endpoint/{}", row.endpoint_name)),
            "case {case:?}: the endpoint ARN is derived from the function's, got {:?}",
            created.body
        );
        assert_eq!(
            member(&created.body, "endpointName"),
            json!(row.endpoint_name),
            "case {case:?}: expected the stored endpoint name, got {:?}",
            created.body
        );
        assert_eq!(
            member(&created.body, "endpointType"),
            json!(row.endpoint_type),
            "case {case:?}: expected the stored endpoint type, got {:?}",
            created.body
        );
        assert_eq!(
            member(&created.body, "authType"),
            json!(row.auth_type),
            "case {case:?}: expected the stored auth type, got {:?}",
            created.body
        );
        assert_eq!(
            member(&created.body, "autoDeploymentMode"),
            json!(row.expected_auto_deployment_mode),
            "case {case:?}: expected autoDeploymentMode {:?}, got {:?}",
            row.expected_auto_deployment_mode,
            created.body
        );
        assert_eq!(
            region_set(&created.body),
            row.expected_regions
                .iter()
                .map(|region| (*region).to_string())
                .collect::<BTreeSet<String>>(),
            "case {case:?}: expected the Regions {:?}, got {:?}",
            row.expected_regions,
            created.body
        );
        // A request that names no `revisionWeights` serves the function's newest
        // revision with all of the traffic: every endpoint this service reports serves
        // at least one revision.
        assert_eq!(
            member(&created.body, "revisionWeights"),
            json!([{ "revisionId": revision_id, "weight": 100 }]),
            "case {case:?}: expected the newest revision to carry all the traffic, got {:?}",
            created.body
        );
        assert_eq!(
            member(&created.body, "state"),
            json!("Pending"),
            "case {case:?}: a just-created endpoint is still Pending, got {:?}",
            created.body
        );
        assert!(
            member(&created.body, "stateReason")
                .as_str()
                .is_some_and(|reason| !reason.is_empty()),
            "case {case:?}: expected a non-empty stateReason, got {:?}",
            created.body
        );
        assert!(
            created.field("updateStatus").is_none() && created.field("updateStatusReason").is_none(),
            "case {case:?}: an endpoint nobody updated reports no update status, got {:?}",
            created.body
        );
        let domain_name = created.string("domainName").unwrap_or_default();
        assert!(
            is_an_address_the_emulator_answers_on(&domain_name),
            "case {case:?}: expected a domainName the emulator answers on, got {domain_name:?}"
        );

        // The read-back: the endpoint readers of this service report the record the
        // create stored, member for member.
        let fetched = get_endpoint(&emulator, &function_name, row.endpoint_name).await;
        assert_eq!(
            fetched.status, 200,
            "case {case:?}: GetWebFunctionEndpoint expected 200, got {} with {:?}",
            fetched.status, fetched.body
        );
        for name in [
            "functionArn",
            "endpointArn",
            "endpointName",
            "description",
            "endpointType",
            "domainName",
            "authType",
            "autoDeploymentMode",
            "revisionWeights",
            "regions",
            "scalingConfig",
            "throttleConfig",
            "regionalEndpoints",
            "createdAt",
        ] {
            assert_eq!(
                member(&fetched.body, name),
                member(&created.body, name),
                "case {case:?}: GetWebFunctionEndpoint reports a different {name} than the \
                 create did, got {:?}",
                fetched.body
            );
        }

        // One regional entry per configured Region, each reporting the endpoint's own
        // configuration; `MultiRegion` routes one domain to the nearest Region, so its
        // regional entries declare none of their own.
        let regional = member(&created.body, "regionalEndpoints");
        let regional = regional
            .as_object()
            .unwrap_or_else(|| panic!("case {case:?}: expected regionalEndpoints, got {regional:?}"));
        assert_eq!(
            regional.keys().cloned().collect::<BTreeSet<String>>(),
            region_set(&created.body),
            "case {case:?}: expected one regional entry per Region, got {regional:?}"
        );
        for (region, entry) in regional {
            assert_eq!(
                member(entry, "authType"),
                json!(row.auth_type),
                "case {case:?}: regional entry {region} reports another auth type, got {entry:?}"
            );
            assert_eq!(
                member(entry, "revisionWeights"),
                member(&created.body, "revisionWeights"),
                "case {case:?}: regional entry {region} serves another revision, got {entry:?}"
            );
            match member(entry, "domainName").as_str() {
                None => assert_eq!(
                    row.endpoint_type, "MultiRegion",
                    "case {case:?}: regional entry {region} must carry its own domain, got \
                     {entry:?}"
                ),
                Some(regional_domain) => assert_eq!(
                    regional_domain, domain_name,
                    "case {case:?}: regional entry {region} must report the address the \
                     endpoint is served at, got {regional_domain:?}"
                ),
            }
        }

        // The initial-endpoint path and this one write ONE slice: a function created
        // with an endpoint keeps it when another is added beside it.
        let second = create_endpoint(
            &emulator,
            &function_name,
            json!({
                "endpointName": "beside-it",
                "endpointType": "HomeRegion",
                "authType": "IamAuth",
            }),
        )
        .await;
        assert_eq!(
            second.status, 202,
            "case {case:?}: a second endpoint expected 202, got {} with {:?}",
            second.status, second.body
        );
        assert_ne!(
            second.string("domainName"),
            Some(domain_name.clone()),
            "case {case:?}: two endpoints must not publish one address, got {:?}",
            second.body
        );
        let still_there = get_endpoint(&emulator, &function_name, row.endpoint_name).await;
        assert_eq!(
            still_there.status, 200,
            "case {case:?}: the first endpoint survived the second, got {} with {:?}",
            still_there.status, still_there.body
        );
    }
}

/// RULE: an endpoint serves the revisions its `revisionWeights` name, and those have
/// to be the function's own revisions — "each entry maps a revision to a weight from 1
/// to 100", for up to two revisions.
#[tokio::test]
async fn revision_weights_name_the_functions_own_revisions() {
    let emulator = Emulator::new();
    let function_name = "weighted";
    let first = function_with_a_revision(&emulator, function_name).await;
    let second = create_revision(&emulator, function_name).await;
    assert_ne!(
        first, second,
        "two revisions of one function must not share an id, got {first:?}"
    );

    // The rows span one revision at full weight, two revisions split, and a split that
    // is not an even one.
    let accepted: Vec<(&str, Value)> = vec![
        (
            "one-revision",
            json!([{ "revisionId": first, "weight": 100 }]),
        ),
        (
            "two-revisions-even",
            json!([
                { "revisionId": first, "weight": 50 },
                { "revisionId": second, "weight": 50 },
            ]),
        ),
        (
            "two-revisions-canary",
            json!([
                { "revisionId": first, "weight": 99 },
                { "revisionId": second, "weight": 1 },
            ]),
        ),
    ];
    for (case, weights) in accepted {
        let endpoint_name = format!("weights-{case}");
        let created = create_endpoint(
            &emulator,
            function_name,
            json!({
                "endpointName": endpoint_name,
                "endpointType": "HomeRegion",
                "authType": "IamAuth",
                "revisionWeights": weights,
            }),
        )
        .await;
        assert_eq!(
            created.status, 202,
            "case {case:?}: expected 202, got {} with {:?}",
            created.status, created.body
        );
        assert_eq!(
            member(&created.body, "revisionWeights"),
            weights,
            "case {case:?}: expected the weights the request named, got {:?}",
            created.body
        );
        let fetched = get_endpoint(&emulator, function_name, &endpoint_name).await;
        assert_eq!(
            member(&fetched.body, "revisionWeights"),
            weights,
            "case {case:?}: the read reports other weights, got {:?}",
            fetched.body
        );
    }

    // (case, the request body, the status and error shape the refusal carries)
    let unknown_revision = "00000000-0000-0000-0000-000000000000";
    let refused: Vec<(&str, Value, u16, &str)> = vec![
        (
            "revision-nobody-created",
            json!({
                "endpointName": "refused-unknown",
                "endpointType": "HomeRegion",
                "authType": "IamAuth",
                "revisionWeights": [{ "revisionId": unknown_revision, "weight": 100 }],
            }),
            404,
            "ResourceNotFoundException",
        ),
        (
            "one-known-one-unknown-revision",
            json!({
                "endpointName": "refused-half-unknown",
                "endpointType": "HomeRegion",
                "authType": "IamAuth",
                "revisionWeights": [
                    { "revisionId": first, "weight": 90 },
                    { "revisionId": unknown_revision, "weight": 10 },
                ],
            }),
            404,
            "ResourceNotFoundException",
        ),
        (
            "one-revision-named-twice",
            json!({
                "endpointName": "refused-twice",
                "endpointType": "HomeRegion",
                "authType": "IamAuth",
                "revisionWeights": [
                    { "revisionId": first, "weight": 60 },
                    { "revisionId": first, "weight": 40 },
                ],
            }),
            400,
            "ValidationException",
        ),
    ];
    for (case, body, status, error) in refused {
        let endpoint_name = body
            .get("endpointName")
            .and_then(Value::as_str)
            .expect("the row names its endpoint")
            .to_string();
        let reply = create_endpoint(&emulator, function_name, body).await;
        assert_eq!(
            reply.status, status,
            "case {case:?}: expected {status}, got {} with {:?}",
            reply.status, reply.body
        );
        assert_eq!(
            reply.error(),
            error,
            "case {case:?}: expected {error}, got {:?}",
            reply.body
        );
        let fetched = get_endpoint(&emulator, function_name, &endpoint_name).await;
        assert_eq!(
            fetched.status, 404,
            "case {case:?}: a refused create stores nothing, got {} with {:?}",
            fetched.status, fetched.body
        );
    }

    // A function with no revisions has nothing for an endpoint to route to, and the
    // request cannot name a weight either.
    let bare = "no-revisions";
    let created = emulator
        .send(
            "PUT",
            "/web-functions",
            REGION,
            Some(json!({ "functionName": bare })),
        )
        .await;
    assert_eq!(
        created.status, 202,
        "case no-revisions: CreateWebFunction expected 202, got {} with {:?}",
        created.status, created.body
    );
    let reply = create_endpoint(
        &emulator,
        bare,
        json!({
            "endpointName": "nothing-to-serve",
            "endpointType": "HomeRegion",
            "authType": "IamAuth",
        }),
    )
    .await;
    assert_eq!(
        reply.status, 400,
        "case no-revisions: expected 400, got {} with {:?}",
        reply.status, reply.body
    );
    assert_eq!(
        reply.error(),
        "ValidationException",
        "case no-revisions: expected ValidationException, got {:?}",
        reply.body
    );
}

/// RULE: an endpoint's name is its identity on its function, and the endpoint belongs
/// to a function that exists — so a create under a name the function already carries
/// conflicts with the stored endpoint, and a create on a function nobody created finds
/// nothing. Neither changes what is stored.
#[tokio::test]
async fn a_create_that_cannot_own_its_name_is_refused_and_changes_nothing() {
    let emulator = Emulator::new();
    let function_name = "owned-names";
    function_with_a_revision(&emulator, function_name).await;
    let body = json!({
        "endpointName": "prod",
        "endpointType": "HomeRegion",
        "authType": "IamAuth",
        "description": "the first one",
    });
    let created = create_endpoint(&emulator, function_name, body.clone()).await;
    assert_eq!(
        created.status, 202,
        "the first create expected 202, got {} with {:?}",
        created.status, created.body
    );

    // The rows differ in what the second request asks for: the same configuration, and
    // a different one. Either way the name is taken.
    for (case, second) in [
        ("identical", body.clone()),
        (
            "different-configuration",
            json!({
                "endpointName": "prod",
                "endpointType": "MultiRegion",
                "authType": "ApplicationManaged",
                "regions": ["eu-west-1"],
            }),
        ),
    ] {
        let conflict = create_endpoint(&emulator, function_name, second).await;
        assert_eq!(
            conflict.status, 409,
            "case {case:?}: expected 409, got {} with {:?}",
            conflict.status, conflict.body
        );
        assert_eq!(
            conflict.error(),
            "ConflictException",
            "case {case:?}: expected ConflictException, got {:?}",
            conflict.body
        );
    }
    // The stored endpoint is the one the first create stored, untouched.
    let fetched = get_endpoint(&emulator, function_name, "prod").await;
    assert_eq!(
        member(&fetched.body, "description"),
        json!("the first one"),
        "a refused create must not overwrite the stored endpoint, got {:?}",
        fetched.body
    );
    assert_eq!(
        member(&fetched.body, "endpointType"),
        json!("HomeRegion"),
        "a refused create must not overwrite the stored endpoint, got {:?}",
        fetched.body
    );

    // A function nobody created: 404, by either spelling of the identifier.
    for identifier in [
        "no-such-function".to_string(),
        percent_encoded(&expected_arn(REGION, "no-such-function")),
    ] {
        let missing = create_endpoint(
            &emulator,
            &identifier,
            json!({
                "endpointName": "prod",
                "endpointType": "HomeRegion",
                "authType": "IamAuth",
            }),
        )
        .await;
        assert_eq!(
            missing.status, 404,
            "case {identifier:?}: expected 404, got {} with {:?}",
            missing.status, missing.body
        );
        assert_eq!(
            missing.error(),
            "ResourceNotFoundException",
            "case {identifier:?}: expected ResourceNotFoundException, got {:?}",
            missing.body
        );
    }
}

/// RULE: the documented `endpointName` and `regions` rules hold of every endpoint this
/// service creates, whichever operation creates it — and a refused create stores
/// nothing.
#[tokio::test]
async fn a_configuration_outside_the_documented_rules_is_refused_on_this_path_too() {
    let emulator = Emulator::new();
    let function_name = "rules-apply";
    function_with_a_revision(&emulator, function_name).await;

    let long_name = "e".repeat(65);
    let cases: Vec<(&str, Value)> = vec![
        (
            "name-leading-underscore",
            json!({ "endpointName": "_prod", "endpointType": "HomeRegion", "authType": "IamAuth" }),
        ),
        (
            "name-trailing-hyphen",
            json!({ "endpointName": "prod-", "endpointType": "HomeRegion", "authType": "IamAuth" }),
        ),
        (
            "name-illegal-character",
            json!({ "endpointName": "pro/d", "endpointType": "HomeRegion", "authType": "IamAuth" }),
        ),
        (
            "name-one-past-the-bound",
            json!({ "endpointName": long_name, "endpointType": "HomeRegion", "authType": "IamAuth" }),
        ),
        (
            "home-region-with-another-region",
            json!({
                "endpointName": "prod",
                "endpointType": "HomeRegion",
                "authType": "IamAuth",
                "regions": ["eu-west-1"],
            }),
        ),
        (
            "multi-region-without-regions",
            json!({ "endpointName": "edge", "endpointType": "MultiRegion", "authType": "IamAuth" }),
        ),
        (
            "per-region-with-only-the-home-region",
            json!({
                "endpointName": "edge",
                "endpointType": "PerRegion",
                "authType": "IamAuth",
                "regions": [REGION],
            }),
        ),
    ];
    for (case, body) in cases {
        let reply = create_endpoint(&emulator, function_name, body).await;
        assert_eq!(
            reply.status, 400,
            "case {case:?}: expected 400, got {} with {:?}",
            reply.status, reply.body
        );
        assert_eq!(
            reply.error(),
            "ValidationException",
            "case {case:?}: expected ValidationException, got {:?}",
            reply.body
        );
    }
    // Nothing above was stored: the function still carries no endpoint at all.
    for endpoint_name in ["prod", "edge", "_prod", &long_name] {
        let fetched = get_endpoint(&emulator, function_name, endpoint_name).await;
        assert_eq!(
            fetched.status, 404,
            "case {endpoint_name:?}: a refused create stores nothing, got {} with {:?}",
            fetched.status, fetched.body
        );
    }
}

/// RULE: a function carries at most the endpoints the account's
/// `maxEndpointsPerFunction` quota allows, and a create past it is refused with the
/// modelled quota error rather than stored.
#[tokio::test]
async fn a_create_past_the_endpoint_quota_is_refused() {
    let emulator = Emulator::new();
    let function_name = "at-the-quota";
    function_with_a_revision(&emulator, function_name).await;
    let quota = usize::try_from(MAX_ENDPOINTS_PER_FUNCTION).expect("a positive quota");
    for index in 0..quota {
        let reply = create_endpoint(
            &emulator,
            function_name,
            json!({
                "endpointName": format!("ep-{index}"),
                "endpointType": "HomeRegion",
                "authType": "IamAuth",
            }),
        )
        .await;
        assert_eq!(
            reply.status, 202,
            "case ep-{index}: expected 202 below the quota, got {} with {:?}",
            reply.status, reply.body
        );
    }
    let refused = create_endpoint(
        &emulator,
        function_name,
        json!({
            "endpointName": "one-too-many",
            "endpointType": "HomeRegion",
            "authType": "IamAuth",
        }),
    )
    .await;
    assert_eq!(
        refused.status, 402,
        "case one-too-many: expected 402, got {} with {:?}",
        refused.status, refused.body
    );
    assert_eq!(
        refused.error(),
        "ServiceQuotaExceededException",
        "case one-too-many: expected ServiceQuotaExceededException, got {:?}",
        refused.body
    );
    let fetched = get_endpoint(&emulator, function_name, "one-too-many").await;
    assert_eq!(
        fetched.status, 404,
        "case one-too-many: a refused create stores nothing, got {} with {:?}",
        fetched.status, fetched.body
    );
}

/// RULE: an endpoint created on a stored function publishes a data plane of its own —
/// it reaches `Active` on the clock, and at its published address its `authType`
/// decides: `IamAuth` ("requests ... must be signed with Signature Version 4") refuses
/// a request presenting no SigV4 credential with the `403 Forbidden` page naming the
/// response it is, while `ApplicationManaged` ("your application handles authorization")
/// refuses nothing of its own.
///
/// What an authorized request is answered with is the web function's own handler
/// response, and this emulator runs no web function code, so it answers `501` and
/// synthesises nothing in the handler's name.
#[tokio::test]
async fn an_endpoint_created_on_a_stored_function_serves_its_own_address() {
    let emulator = Emulator::new();
    let function_name = "auth-types";
    function_with_a_revision(&emulator, function_name).await;

    for (case, auth_type) in [
        ("iam-authorized", "IamAuth"),
        ("application-managed", "ApplicationManaged"),
    ] {
        let endpoint_name = format!("ep-{case}");
        let created = create_endpoint(
            &emulator,
            function_name,
            json!({
                "endpointName": endpoint_name,
                "endpointType": "HomeRegion",
                "authType": auth_type,
            }),
        )
        .await;
        assert_eq!(
            created.status, 202,
            "case {case:?}: expected 202, got {} with {:?}",
            created.status, created.body
        );
        let domain_name = created.string("domainName").unwrap_or_default();
        assert!(
            is_an_address_the_emulator_answers_on(&domain_name),
            "case {case:?}: expected a domainName the emulator answers on, got {domain_name:?}"
        );

        // Nothing is published at the address until the endpoint is serving traffic.
        let pending = request_endpoint(
            &emulator,
            function_name,
            &endpoint_name,
            "/",
            &[("authorization", SIGNED_AUTHORIZATION)],
        );
        assert_eq!(
            pending.status().as_u16(),
            404,
            "case {case:?}: an endpoint that is not serving traffic publishes nothing, got {}",
            pending.status()
        );

        let active = poll_until_active(&emulator, function_name, &endpoint_name).await;
        assert_eq!(
            member(&active, "domainName"),
            json!(domain_name),
            "case {case:?}: the serving endpoint reports another address, got {active:?}"
        );

        // (request case, the request's URI, its headers, the status the address answers)
        let unsigned = if auth_type == "IamAuth" { 403 } else { 501 };
        let rows: &[AddressRow<'_>] = &[
            (
                "a signed GET",
                "/",
                &[("authorization", SIGNED_AUTHORIZATION)],
                501,
            ),
            (
                "a presigned GET",
                "/?X-Amz-Credential=AKIDEXAMPLE&X-Amz-Signature=deadbeef",
                &[],
                501,
            ),
            ("an unsigned GET", "/", &[], unsigned),
            ("an unsigned GET deeper in the path", "/items/42", &[], unsigned),
        ];
        for (request_case, uri, headers, expected) in rows {
            let answered = request_endpoint(&emulator, function_name, &endpoint_name, uri, headers);
            assert_eq!(
                answered.status().as_u16(),
                *expected,
                "case {case:?}/{request_case:?}: expected {expected}, got {} with {:?}",
                answered.status(),
                String::from_utf8_lossy(answered.body())
            );
            if *expected == 403 {
                // The refusal names the response it is, in its own header, and the page it
                // serves states that same id.
                let request_id = answered
                    .headers()
                    .get(REQUEST_ID_HEADER)
                    .and_then(|value| value.to_str().ok())
                    .unwrap_or_default()
                    .to_string();
                assert!(
                    !request_id.is_empty(),
                    "case {case:?}/{request_case:?}: a 403 response must carry a \
                     {REQUEST_ID_HEADER} header, got the headers {:?}",
                    answered.headers()
                );
                assert_eq!(
                    String::from_utf8_lossy(answered.body()),
                    forbidden_page(&request_id),
                    "case {case:?}/{request_case:?}: expected the Forbidden page naming \
                     {request_id:?}"
                );
            }
        }
    }
}

/// One row of the published-address table: the case, the request URI, the headers it
/// carries, and the status the address answers it with.
type AddressRow<'a> = (&'a str, &'a str, &'a [(&'a str, &'a str)], u16);

/// A SigV4 `Authorization` header, as a signed client sends one.
const SIGNED_AUTHORIZATION: &str = "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/us-east-1/\
                                    lambda/aws4_request, SignedHeaders=host, Signature=deadbeef";

/// One request to a published endpoint's address, answered by the decision the core's
/// listener serves there — read out of the ONE store this service's handlers write.
fn request_endpoint(
    emulator: &Emulator,
    function_name: &str,
    endpoint_name: &str,
    uri: &str,
    headers: &[(&str, &str)],
) -> ::http::Response<Vec<u8>> {
    let mut builder = ::http::Request::builder().method("GET").uri(uri);
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    let request = builder.body(Vec::new()).expect("a well-formed request");
    let state = service_state(&**emulator.engine()).expect("this service's store");
    serve_request(&state, function_name, endpoint_name, &request)
}

