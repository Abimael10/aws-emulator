// SPDX-License-Identifier: CC0-1.0

//! The self-managed license-configuration lifecycle over the wire:
//! `CreateLicenseConfiguration`, `GetLicenseConfiguration`,
//! `ListLicenseConfigurations`, `UpdateLicenseConfiguration`, `TagResource`,
//! `UntagResource`, `ListTagsForResource` and `DeleteLicenseConfiguration`, driven as
//! awsJson1_1 requests (`POST /`, `X-Amz-Target: AWSLicenseManager.<Operation>`)
//! through the same `MultiServiceRouter` the production emulator mounts.
//!
//! The rule under test: each of these operations answers from the ONE persisted
//! license-configuration record, so whatever a create or an update stored is what a
//! later Get, List or ListTagsForResource reports, and an ARN that was never stored
//! (or has since been deleted) is a modelled 400 `InvalidParameterValueException`.
//!
//! Every assertion is about STORED state: a response is checked against what an
//! earlier call persisted, never against a value this file hands the service and
//! reads straight back out of the same response.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The `X-Amz-Target` prefix: the service shape's name,
/// `com.amazonaws.licensemanager#AWSLicenseManager`
/// (`smithy_query(query="service_info")` reports `AWSLicenseManager`).
const TARGET_PREFIX: &str = "AWSLicenseManager";

/// The region the harness signs for unless a case picks another.
const DEFAULT_REGION: &str = "us-east-1";

/// The canonical account id every emulated ARN carries
/// (`engine::EMULATOR_ACCOUNT_ID`).
const ACCOUNT_ID: &str = "000000000000";

/// The dimensions a license configuration can count by.
///
/// The catalog is the modelled `LicenseCountingType` enum
/// (`smithy_query(query="shape",shape="LicenseCountingType")`): `vCPU`, `Instance`,
/// `Core`, `Socket`.
const LICENSE_COUNTING_TYPES: &[&str] = &["vCPU", "Instance", "Core", "Socket"];

/// One emulator: a single shared engine behind the router, exactly as
/// `core/router` assembles it.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    // The harness signs nothing, so authentication is off for this router only.
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_licensemanager_awslicensemanager::EmulatedService::new(engine),
    );
    router
}

/// Send one awsJson1_1 request signed for `region` and return its status and body.
async fn call_in(
    router: &MultiServiceRouter,
    region: &str,
    operation: &str,
    body: &Value,
) -> (u16, Value) {
    let request = ::http::Request::builder()
        .method("POST")
        .uri("/")
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("{TARGET_PREFIX}.{operation}"))
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDTESTHARNESS/20260101/{region}/license-manager/\
                 aws4_request, SignedHeaders=host;x-amz-target, Signature=0"
            ),
        )
        .body(Bytes::from(
            serde_json::to_vec(body).expect("the request body serializes"),
        ))
        .expect("a well-formed request");

    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let bytes = response.into_body();
    let parsed = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or_else(|e| {
            panic!(
                "{operation}: response body is not JSON ({e}): {}",
                String::from_utf8_lossy(&bytes)
            )
        })
    };
    (status, parsed)
}

/// Send one request in [`DEFAULT_REGION`].
async fn call(router: &MultiServiceRouter, operation: &str, body: &Value) -> (u16, Value) {
    call_in(router, DEFAULT_REGION, operation, body).await
}

/// The wire error code of a failure body: the last segment of awsJson's `__type`.
fn error_code(body: &Value) -> String {
    body.get("__type")
        .and_then(Value::as_str)
        .map(|t| t.split('#').next_back().unwrap_or(t).to_string())
        .unwrap_or_else(|| format!("<no __type in {body}>"))
}

/// Whether `arn` is the ARN of a license configuration in `region`:
/// `arn:aws:license-manager:<region>:<account>:license-configuration:lic-<32 hex>`.
fn is_configuration_arn(arn: &str, region: &str) -> bool {
    let prefix = format!("arn:aws:license-manager:{region}:{ACCOUNT_ID}:license-configuration:");
    arn.strip_prefix(&prefix)
        .and_then(|id| id.strip_prefix("lic-"))
        .is_some_and(|hex| hex.len() == 32 && hex.chars().all(|c| c.is_ascii_hexdigit()))
}

/// Create one configuration and return the ARN the service minted, failing the case
/// when the call did not succeed.
async fn create_ok(router: &MultiServiceRouter, case: &str, body: &Value) -> String {
    let (status, response) = call(router, "CreateLicenseConfiguration", body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected CreateLicenseConfiguration to succeed, got {status} {response}"
    );
    response["LicenseConfigurationArn"]
        .as_str()
        .unwrap_or_else(|| {
            panic!("case {case:?}: no LicenseConfigurationArn in {response}")
        })
        .to_string()
}

/// `GetLicenseConfiguration` for `arn`, failing the case when it did not succeed.
async fn get_ok(router: &MultiServiceRouter, case: &str, arn: &str) -> Value {
    let (status, response) = call(
        router,
        "GetLicenseConfiguration",
        &json!({"LicenseConfigurationArn": arn}),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected GetLicenseConfiguration to succeed, got {status} {response}"
    );
    response
}

/// A `Tags` list from a response, as a key -> value map.
fn tag_map(case: &str, tags: &Value) -> BTreeMap<String, String> {
    tags.as_array()
        .unwrap_or_else(|| panic!("case {case:?}: expected a Tags list, got {tags}"))
        .iter()
        .map(|tag| {
            let key = tag["Key"]
                .as_str()
                .unwrap_or_else(|| panic!("case {case:?}: a tag has no Key: {tag}"));
            let value = tag["Value"]
                .as_str()
                .unwrap_or_else(|| panic!("case {case:?}: a tag has no Value: {tag}"));
            (key.to_string(), value.to_string())
        })
        .collect()
}

/// `pairs` as the modelled `TagList`.
fn tag_list(pairs: &[(&str, &str)]) -> Value {
    Value::Array(
        pairs
            .iter()
            .map(|(key, value)| json!({"Key": key, "Value": value}))
            .collect(),
    )
}

/// Every ARN `ListLicenseConfigurations` reports for `body`, paging with `max_results`
/// until the service stops handing out a `NextToken`.
///
/// Also asserts the pagination invariants as it walks: no page is larger than the
/// page size, only the last page carries no token, and the walk terminates.
async fn list_all_arns(
    router: &MultiServiceRouter,
    case: &str,
    max_results: Option<i64>,
    body: &Value,
) -> Vec<String> {
    let page_cap = max_results.map_or(usize::MAX, |n| n.max(1) as usize);
    let mut arns = Vec::new();
    let mut next_token: Option<String> = None;
    for page in 0..64 {
        let mut request = body.clone();
        if let Some(max) = max_results {
            request["MaxResults"] = json!(max);
        }
        if let Some(token) = &next_token {
            request["NextToken"] = json!(token);
        }
        let (status, response) = call(router, "ListLicenseConfigurations", &request).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected ListLicenseConfigurations to succeed, got {status} {response}"
        );
        let page_items = response["LicenseConfigurations"]
            .as_array()
            .unwrap_or_else(|| {
                panic!("case {case:?}: no LicenseConfigurations list in {response}")
            })
            .clone();
        assert!(
            page_items.len() <= page_cap,
            "case {case:?}: page {page} holds {} items, more than the requested {page_cap}",
            page_items.len()
        );
        for item in &page_items {
            arns.push(
                item["LicenseConfigurationArn"]
                    .as_str()
                    .unwrap_or_else(|| {
                        panic!("case {case:?}: a listed item has no ARN: {item}")
                    })
                    .to_string(),
            );
        }
        next_token = response
            .get("NextToken")
            .and_then(Value::as_str)
            .map(str::to_string);
        match &next_token {
            None => return arns,
            Some(token) => assert!(
                !token.is_empty(),
                "case {case:?}: page {page} handed back an empty NextToken"
            ),
        }
    }
    panic!("case {case:?}: ListLicenseConfigurations never stopped handing out a NextToken");
}

/// One row of the create/get round-trip table: the request body, and the name the
/// failure messages use.
struct RoundTripCase {
    case: &'static str,
    body: Value,
}

/// One step of the tag lifecycle: the tagging call to make (none, for the state a
/// create left behind), and the tags the resource must carry afterwards.
struct TagStep {
    case: &'static str,
    operation: Option<&'static str>,
    body: Value,
    expected: Vec<(&'static str, &'static str)>,
}

/// One row of the list-scope or list-filter table: what the request scopes on, and
/// the ARNs the service must then report.
struct ListCase<T> {
    case: &'static str,
    scope: T,
    expected: BTreeSet<String>,
}

/// The table spans what the round-trip rule generalises over: every counting type in
/// the modelled catalog, a present and an absent value for each optional scalar, and
/// a list member empty, of one entry and of several.
fn round_trip_cases() -> Vec<RoundTripCase> {
    let mut cases = vec![
        RoundTripCase {
            case: "only the two required members",
            body: json!({"Name": "minimal", "LicenseCountingType": "Core"}),
        },
        RoundTripCase {
            case: "every member set",
            body: json!({
                "Name": "fully-populated",
                "LicenseCountingType": "vCPU",
                "Description": "a configuration carrying every member",
                "LicenseCount": 10,
                "LicenseCountHardLimit": true,
                "LicenseRules": ["#allowedTenancy=EC2-Default", "#minimumVcpus=2", "#maximumVcpus=16"],
                "DisassociateWhenNotFound": true,
                "LicenseExpiry": 1_800_000_000_i64,
                "Tags": tag_list(&[("env", "dev"), ("owner", "platform")]),
                "ProductInformationList": [{
                    "ResourceType": "SSM_MANAGED",
                    "ProductInformationFilterList": [{
                        "ProductInformationFilterName": "Application Name",
                        "ProductInformationFilterValue": ["SQL Server Enterprise"],
                        "ProductInformationFilterComparator": "EQUALS"
                    }]
                }]
            }),
        },
        RoundTripCase {
            case: "soft limit, one rule, one tag",
            body: json!({
                "Name": "soft-limited",
                "LicenseCountingType": "Socket",
                "LicenseCount": 0,
                "LicenseCountHardLimit": false,
                "LicenseRules": ["#licenseAffinityToHost=90"],
                "DisassociateWhenNotFound": false,
                "Tags": tag_list(&[("team", "licensing")])
            }),
        },
        RoundTripCase {
            case: "a very large license count",
            body: json!({
                "Name": "huge",
                "LicenseCountingType": "Instance",
                "LicenseCount": 9_007_199_254_740_991_i64
            }),
        },
    ];
    // Every entry of the modelled catalog is exercised, including the ones the issue
    // did not cite.
    for counting_type in LICENSE_COUNTING_TYPES {
        cases.push(RoundTripCase {
            case: "one counting type per catalog entry",
            body: json!({
                "Name": format!("by-{counting_type}"),
                "LicenseCountingType": counting_type,
                "LicenseCount": 3
            }),
        });
    }
    cases
}

#[tokio::test]
async fn create_persists_every_member_a_later_get_reports() {
    let router = emulator();
    for RoundTripCase { case, body } in round_trip_cases() {
        let arn = create_ok(&router, case, &body).await;
        assert!(
            is_configuration_arn(&arn, DEFAULT_REGION),
            "case {case:?}: expected a license-configuration ARN, got {arn:?}"
        );

        let stored = get_ok(&router, case, &arn).await;

        // The id is the ARN's last segment, so the two agree by construction.
        let id = stored["LicenseConfigurationId"].as_str().unwrap_or("");
        assert!(
            arn.ends_with(id) && id.starts_with("lic-"),
            "case {case:?}: expected the id to be the ARN's last segment, got id {id:?} for {arn:?}"
        );
        assert_eq!(
            stored["LicenseConfigurationArn"].as_str(),
            Some(arn.as_str()),
            "case {case:?}: Get reported another ARN: {stored}"
        );

        // Every member the create supplied is reported back as stored.
        for member in [
            "Name",
            "LicenseCountingType",
            "Description",
            "LicenseCount",
            "LicenseCountHardLimit",
            "LicenseRules",
            "DisassociateWhenNotFound",
            "LicenseExpiry",
            "ProductInformationList",
        ] {
            if let Some(supplied) = body.get(member) {
                assert_eq!(
                    stored.get(member),
                    Some(supplied),
                    "case {case:?}: {member} was not stored as supplied, got {stored}"
                );
            }
        }
        // A list member the create omitted reads back as the empty list, not as a
        // missing member.
        for member in ["LicenseRules", "ProductInformationList"] {
            assert!(
                stored[member].is_array(),
                "case {case:?}: expected {member} to be a list, got {}",
                stored[member]
            );
        }

        // Tags are reported from the same ARN-keyed store the tagging operations use.
        let expected_tags = body
            .get("Tags")
            .map(|tags| tag_map(case, tags))
            .unwrap_or_default();
        assert_eq!(
            tag_map(case, &stored["Tags"]),
            expected_tags,
            "case {case:?}: Get reported other tags: {stored}"
        );

        // Computed members: a fresh configuration is AVAILABLE, owned by the
        // emulator account, and consumes nothing because nothing is associated.
        assert_eq!(
            stored["Status"].as_str(),
            Some("AVAILABLE"),
            "case {case:?}: expected Status AVAILABLE, got {}",
            stored["Status"]
        );
        assert_eq!(
            stored["OwnerAccountId"].as_str(),
            Some(ACCOUNT_ID),
            "case {case:?}: expected OwnerAccountId {ACCOUNT_ID}, got {}",
            stored["OwnerAccountId"]
        );
        assert_eq!(
            stored["ConsumedLicenses"].as_i64(),
            Some(0),
            "case {case:?}: expected ConsumedLicenses 0, got {}",
            stored["ConsumedLicenses"]
        );
        for member in ["ConsumedLicenseSummaryList", "ManagedResourceSummaryList"] {
            assert_eq!(
                stored[member].as_array().map(Vec::len),
                Some(0),
                "case {case:?}: expected {member} to be empty, got {}",
                stored[member]
            );
        }
    }
}

#[tokio::test]
async fn each_create_mints_its_own_configuration() {
    let router = emulator();
    let mut arns = BTreeSet::new();
    let mut ids = BTreeSet::new();
    // The same request body twice over: two creates are two configurations even when
    // nothing distinguishes the requests.
    for round in 0..6 {
        let case = "repeated identical create";
        let arn = create_ok(
            &router,
            case,
            &json!({"Name": "same-name", "LicenseCountingType": "Core", "LicenseCount": 1}),
        )
        .await;
        assert!(
            arns.insert(arn.clone()),
            "case {case:?}: round {round} reused the ARN {arn:?}"
        );
        let stored = get_ok(&router, case, &arn).await;
        let id = stored["LicenseConfigurationId"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        assert!(
            ids.insert(id.clone()),
            "case {case:?}: round {round} reused the id {id:?}"
        );
    }
    let listed: BTreeSet<String> = list_all_arns(&router, "repeated identical create", None, &json!({}))
        .await
        .into_iter()
        .collect();
    assert_eq!(
        listed, arns,
        "every created configuration must be listed exactly once"
    );
}

#[tokio::test]
async fn a_configuration_arn_names_the_region_the_request_was_signed_for() {
    // The ARN's region field is read off the request's own SigV4 credential scope,
    // so it is not fixed to one region.
    for region in ["us-east-1", "us-west-2", "eu-central-1", "ap-southeast-2"] {
        let router = emulator();
        let (status, response) = call_in(
            &router,
            region,
            "CreateLicenseConfiguration",
            &json!({"Name": "regional", "LicenseCountingType": "Instance"}),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {region:?}: expected the create to succeed, got {status} {response}"
        );
        let arn = response["LicenseConfigurationArn"].as_str().unwrap_or("");
        assert!(
            is_configuration_arn(arn, region),
            "case {region:?}: expected an ARN in {region}, got {arn:?}"
        );
    }
}

#[tokio::test]
async fn list_pages_cover_every_configuration_exactly_once() {
    let router = emulator();
    let mut created = BTreeSet::new();
    for n in 0..5 {
        created.insert(
            create_ok(
                &router,
                "pagination",
                &json!({"Name": format!("config-{n}"), "LicenseCountingType": "Core"}),
            )
            .await,
        );
    }

    // The rule generalises over the page size: a page smaller than, equal to and
    // larger than the store.
    for max_results in [Some(1), Some(2), Some(3), Some(5), Some(7), None] {
        let case = format!("MaxResults={max_results:?}");
        let walked = list_all_arns(&router, &case, max_results, &json!({})).await;
        assert_eq!(
            walked.len(),
            created.len(),
            "case {case:?}: the walk reported {} ARNs for {} configurations: {walked:?}",
            walked.len(),
            created.len()
        );
        assert_eq!(
            walked.iter().cloned().collect::<BTreeSet<String>>(),
            created,
            "case {case:?}: the walk did not cover every configuration exactly once: {walked:?}"
        );
    }
}

#[tokio::test]
async fn list_scopes_to_the_requested_arns() {
    let router = emulator();
    let mut arns = Vec::new();
    for n in 0..4 {
        arns.push(
            create_ok(
                &router,
                "ARN scope",
                &json!({"Name": format!("scoped-{n}"), "LicenseCountingType": "Socket"}),
            )
            .await,
        );
    }

    // The rule generalises over how many ARNs are named, and over an ARN that names
    // nothing.
    let unknown = format!(
        "arn:aws:license-manager:{DEFAULT_REGION}:{ACCOUNT_ID}:license-configuration:lic-{}",
        "f".repeat(32)
    );
    let rows = vec![
        ListCase {
            case: "one ARN",
            scope: vec![arns[1].clone()],
            expected: [arns[1].clone()].into(),
        },
        ListCase {
            case: "two ARNs",
            scope: vec![arns[0].clone(), arns[3].clone()],
            expected: [arns[0].clone(), arns[3].clone()].into(),
        },
        ListCase {
            case: "every ARN",
            scope: arns.clone(),
            expected: arns.iter().cloned().collect::<BTreeSet<String>>(),
        },
        ListCase {
            case: "an ARN that names nothing",
            scope: vec![unknown.clone()],
            expected: BTreeSet::new(),
        },
        ListCase {
            case: "a known and an unknown ARN",
            scope: vec![arns[2].clone(), unknown.clone()],
            expected: [arns[2].clone()].into(),
        },
    ];

    for ListCase {
        case,
        scope: requested,
        expected,
    } in rows
    {
        let walked = list_all_arns(
            &router,
            case,
            Some(1),
            &json!({"LicenseConfigurationArns": requested}),
        )
        .await;
        assert_eq!(
            walked.iter().cloned().collect::<BTreeSet<String>>(),
            expected,
            "case {case:?}: scoping to {requested:?} reported {walked:?}"
        );
    }
}

#[tokio::test]
async fn list_filters_scope_on_the_documented_names() {
    let router = emulator();
    // Four configurations that differ exactly along what the three documented
    // filters scope on.
    let hard_vcpu = create_ok(
        &router,
        "filters",
        &json!({"Name": "hard-vcpu", "LicenseCountingType": "vCPU", "LicenseCount": 4, "LicenseCountHardLimit": true}),
    )
    .await;
    let soft_vcpu = create_ok(
        &router,
        "filters",
        &json!({"Name": "soft-vcpu", "LicenseCountingType": "vCPU", "LicenseCount": 4, "LicenseCountHardLimit": false}),
    )
    .await;
    // A hard limit needs a count to enforce, so this row carries one; what it
    // differs from `hard_vcpu` in is the counting type.
    let hard_socket = create_ok(
        &router,
        "filters",
        &json!({"Name": "hard-socket", "LicenseCountingType": "Socket", "LicenseCount": 2, "LicenseCountHardLimit": true}),
    )
    .await;
    let plain_core = create_ok(
        &router,
        "filters",
        &json!({"Name": "plain-core", "LicenseCountingType": "Core"}),
    )
    .await;

    // Names and values from the `Filters` member's own documentation.
    let rows = vec![
        ListCase {
            case: "licenseCountingType=vCPU",
            scope: json!([{"Name": "licenseCountingType", "Values": ["vCPU"]}]),
            expected: [hard_vcpu.clone(), soft_vcpu.clone()].into(),
        },
        ListCase {
            case: "licenseCountingType=Socket",
            scope: json!([{"Name": "licenseCountingType", "Values": ["Socket"]}]),
            expected: [hard_socket.clone()].into(),
        },
        ListCase {
            case: "licenseCountingType=Core",
            scope: json!([{"Name": "licenseCountingType", "Values": ["Core"]}]),
            expected: [plain_core.clone()].into(),
        },
        ListCase {
            case: "licenseCountingType=Instance matches nothing",
            scope: json!([{"Name": "licenseCountingType", "Values": ["Instance"]}]),
            expected: BTreeSet::new(),
        },
        ListCase {
            case: "enforceLicenseCount=true",
            scope: json!([{"Name": "enforceLicenseCount", "Values": ["true"]}]),
            expected: [hard_vcpu.clone(), hard_socket.clone()].into(),
        },
        ListCase {
            case: "enforceLicenseCount=false",
            scope: json!([{"Name": "enforceLicenseCount", "Values": ["false"]}]),
            expected: [soft_vcpu.clone(), plain_core.clone()].into(),
        },
        ListCase {
            case: "usagelimitExceeded=false",
            scope: json!([{"Name": "usagelimitExceeded", "Values": ["false"]}]),
            expected: [
                hard_vcpu.clone(),
                soft_vcpu.clone(),
                hard_socket.clone(),
                plain_core.clone(),
            ]
            .into(),
        },
        ListCase {
            case: "usagelimitExceeded=true",
            scope: json!([{"Name": "usagelimitExceeded", "Values": ["true"]}]),
            expected: BTreeSet::new(),
        },
        ListCase {
            case: "two filters together",
            scope: json!([
                {"Name": "licenseCountingType", "Values": ["vCPU"]},
                {"Name": "enforceLicenseCount", "Values": ["true"]}
            ]),
            expected: [hard_vcpu.clone()].into(),
        },
    ];

    for ListCase {
        case,
        scope: filters,
        expected,
    } in rows
    {
        let walked = list_all_arns(&router, case, Some(2), &json!({"Filters": filters})).await;
        assert_eq!(
            walked.iter().cloned().collect::<BTreeSet<String>>(),
            expected,
            "case {case:?}: the filter reported {walked:?}"
        );
    }
}

#[tokio::test]
async fn list_refuses_a_filter_or_token_it_did_not_issue() {
    let router = emulator();
    create_ok(
        &router,
        "refusals",
        &json!({"Name": "only", "LicenseCountingType": "Core"}),
    )
    .await;

    // Filter names are case-sensitive, per the member's own documentation, and only
    // one value may be given per filter.
    let rows: Vec<(&str, Value)> = vec![
        (
            "an unknown filter name",
            json!({"Filters": [{"Name": "licenseName", "Values": ["x"]}]}),
        ),
        (
            "a documented name in the wrong case",
            json!({"Filters": [{"Name": "LicenseCountingType", "Values": ["Core"]}]}),
        ),
        (
            "two values for one filter",
            json!({"Filters": [{"Name": "licenseCountingType", "Values": ["Core", "vCPU"]}]}),
        ),
        (
            "no value for one filter",
            json!({"Filters": [{"Name": "licenseCountingType", "Values": []}]}),
        ),
        (
            "a Boolean filter given something that is not a Boolean",
            json!({"Filters": [{"Name": "enforceLicenseCount", "Values": ["yes"]}]}),
        ),
        (
            "a Boolean filter given a Boolean in the wrong case",
            json!({"Filters": [{"Name": "usagelimitExceeded", "Values": ["TRUE"]}]}),
        ),
        (
            "a Boolean filter given a number",
            json!({"Filters": [{"Name": "enforceLicenseCount", "Values": ["1"]}]}),
        ),
        (
            "licenseCountingType given a dimension outside the documented set",
            json!({"Filters": [{"Name": "licenseCountingType", "Values": ["CPU"]}]}),
        ),
        (
            "licenseCountingType given a documented dimension in the wrong case",
            json!({"Filters": [{"Name": "licenseCountingType", "Values": ["core"]}]}),
        ),
        ("MaxResults of zero", json!({"MaxResults": 0})),
        ("a negative MaxResults", json!({"MaxResults": -1})),
        (
            "a MaxResults past the page ceiling",
            json!({"MaxResults": 101}),
        ),
        (
            "a NextToken this operation never issued",
            json!({"NextToken": "not-a-token"}),
        ),
    ];

    for (case, body) in rows {
        let (status, response) = call(&router, "ListLicenseConfigurations", &body).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected a 400, got {status} {response}"
        );
        assert_eq!(
            error_code(&response),
            "InvalidParameterValueException",
            "case {case:?}: expected InvalidParameterValueException, got {response}"
        );
    }
}

#[tokio::test]
async fn a_page_token_resumes_only_the_query_it_was_issued_for() {
    let router = emulator();
    for n in 0..4 {
        create_ok(
            &router,
            "token scope",
            &json!({
                "Name": format!("scoped-{n}"),
                // Two of each dimension, so every scope below has more than one page.
                "LicenseCountingType": if n % 2 == 0 { "Core" } else { "vCPU" }
            }),
        )
        .await;
    }

    // A token is issued under one scope, then presented under another. The rule
    // generalises over what changed about the scope: the filters, the ARN list, or
    // the absence of either.
    let core_only = json!({"Filters": [{"Name": "licenseCountingType", "Values": ["Core"]}]});
    let (status, first_page) = call(
        &router,
        "ListLicenseConfigurations",
        &json!({"MaxResults": 1, "Filters": core_only["Filters"]}),
    )
    .await;
    assert_eq!(
        status, 200,
        "expected the first page to succeed, got {status} {first_page}"
    );
    let token = first_page["NextToken"]
        .as_str()
        .unwrap_or_else(|| panic!("expected a NextToken on the first page: {first_page}"))
        .to_string();
    let resumed_arn = first_page["LicenseConfigurations"][0]["LicenseConfigurationArn"]
        .as_str()
        .unwrap_or_default()
        .to_string();

    // Presented under the SAME scope it was issued for, it resumes.
    let (status, second_page) = call(
        &router,
        "ListLicenseConfigurations",
        &json!({"MaxResults": 1, "Filters": core_only["Filters"], "NextToken": token}),
    )
    .await;
    assert_eq!(
        status, 200,
        "case \"same scope\": expected the token to resume, got {status} {second_page}"
    );
    let next_arn = second_page["LicenseConfigurations"][0]["LicenseConfigurationArn"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    assert_ne!(
        next_arn, resumed_arn,
        "case \"same scope\": the second page repeated the first page's configuration"
    );

    // Presented under any other scope, it is refused rather than silently resuming
    // a different walk at that ARN.
    let other_scopes: Vec<(&str, Value)> = vec![
        ("no filter at all", json!({})),
        (
            "another dimension",
            json!({"Filters": [{"Name": "licenseCountingType", "Values": ["vCPU"]}]}),
        ),
        (
            "an extra filter",
            json!({"Filters": [
                {"Name": "licenseCountingType", "Values": ["Core"]},
                {"Name": "enforceLicenseCount", "Values": ["false"]}
            ]}),
        ),
        (
            "an ARN scope added",
            json!({
                "Filters": [{"Name": "licenseCountingType", "Values": ["Core"]}],
                "LicenseConfigurationArns": [resumed_arn]
            }),
        ),
    ];
    for (case, mut body) in other_scopes {
        body["MaxResults"] = json!(1);
        body["NextToken"] = json!(token);
        let (status, response) = call(&router, "ListLicenseConfigurations", &body).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected a 400, got {status} {response}"
        );
        assert_eq!(
            error_code(&response),
            "InvalidParameterValueException",
            "case {case:?}: expected InvalidParameterValueException, got {response}"
        );
    }
}

#[tokio::test]
async fn update_replaces_only_the_members_it_carries() {
    // Every member `UpdateLicenseConfiguration` accepts, one row each: the row's
    // member must change in the store and every other member must be untouched.
    let rows: Vec<(&str, Value)> = vec![
        ("Name", json!({"Name": "renamed"})),
        ("Description", json!({"Description": "a new description"})),
        ("LicenseCount", json!({"LicenseCount": 20})),
        ("LicenseCountHardLimit", json!({"LicenseCountHardLimit": true})),
        (
            "LicenseRules",
            json!({"LicenseRules": ["#licenseAffinityToHost=180"]}),
        ),
        ("DisassociateWhenNotFound", json!({"DisassociateWhenNotFound": true})),
        ("LicenseExpiry", json!({"LicenseExpiry": 1_900_000_000_i64})),
        ("Status", json!({"LicenseConfigurationStatus": "DISABLED"})),
        (
            "ProductInformationList",
            // SSM_MANAGED, because the stored configuration below counts by Core:
            // RDS product information belongs to a vCPU configuration only.
            json!({"ProductInformationList": [{
                "ResourceType": "SSM_MANAGED",
                "ProductInformationFilterList": [{
                    "ProductInformationFilterName": "Application Name",
                    "ProductInformationFilterValue": ["SQL Server Standard"],
                    "ProductInformationFilterComparator": "EQUALS"
                }]
            }]}),
        ),
        ("nothing at all", json!({})),
    ];

    for (case, patch) in rows {
        let router = emulator();
        let arn = create_ok(
            &router,
            case,
            &json!({
                "Name": "before",
                "LicenseCountingType": "Core",
                "Description": "the original description",
                "LicenseCount": 5,
                "LicenseCountHardLimit": false,
                "LicenseRules": ["#minimumCores=1"],
                "DisassociateWhenNotFound": false,
                "LicenseExpiry": 1_700_000_000_i64,
                "Tags": tag_list(&[("env", "prod")])
            }),
        )
        .await;
        let before = get_ok(&router, case, &arn).await;

        let mut request = patch.clone();
        request["LicenseConfigurationArn"] = json!(arn);
        let (status, response) = call(&router, "UpdateLicenseConfiguration", &request).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the update to succeed, got {status} {response}"
        );

        let after = get_ok(&router, case, &arn).await;

        // What the patch named changed, read back out of the store.
        for (member, value) in patch.as_object().expect("the patch is an object") {
            let reported_as = if member == "LicenseConfigurationStatus" {
                "Status"
            } else {
                member.as_str()
            };
            assert_eq!(
                after.get(reported_as),
                Some(value),
                "case {case:?}: {reported_as} was not updated to {value}, got {after}"
            );
        }

        // Everything the patch did not name is exactly as it was.
        let patched = patch.as_object().expect("the patch is an object");
        // The output member each patched member is reported under.
        let patched_members: BTreeSet<&str> = patched
            .keys()
            .map(|member| {
                if member == "LicenseConfigurationStatus" {
                    "Status"
                } else {
                    member.as_str()
                }
            })
            .collect();
        let untouched: Vec<&str> = before
            .as_object()
            .expect("Get answers an object")
            .keys()
            .map(String::as_str)
            .filter(|member| !patched_members.contains(member))
            .collect();
        for member in untouched {
            assert_eq!(
                after.get(member),
                before.get(member),
                "case {case:?}: {member} changed from {:?} to {:?}",
                before.get(member),
                after.get(member)
            );
        }
    }
}

#[tokio::test]
async fn tags_round_trip_through_the_tagging_operations() {
    let router = emulator();
    let case = "tag lifecycle";
    let arn = create_ok(
        &router,
        case,
        &json!({
            "Name": "tagged",
            "LicenseCountingType": "vCPU",
            "Tags": tag_list(&[("env", "dev")])
        }),
    )
    .await;

    // What each step must leave stored for the ARN, read back through
    // ListTagsForResource and GetLicenseConfiguration.
    let steps = vec![
        TagStep {
            case: "the create-time tag",
            operation: None,
            body: json!({}),
            expected: vec![("env", "dev")],
        },
        TagStep {
            case: "a second tag added",
            operation: Some("TagResource"),
            body: json!({"Tags": tag_list(&[("team", "plat")])}),
            expected: vec![("env", "dev"), ("team", "plat")],
        },
        TagStep {
            case: "two more tags at once",
            operation: Some("TagResource"),
            body: json!({"Tags": tag_list(&[("tier", "gold"), ("cost", "42")])}),
            expected: vec![
                ("env", "dev"),
                ("team", "plat"),
                ("tier", "gold"),
                ("cost", "42"),
            ],
        },
        TagStep {
            case: "an existing key given a new value",
            operation: Some("TagResource"),
            body: json!({"Tags": tag_list(&[("env", "staging")])}),
            expected: vec![
                ("env", "staging"),
                ("team", "plat"),
                ("tier", "gold"),
                ("cost", "42"),
            ],
        },
        TagStep {
            case: "one key removed",
            operation: Some("UntagResource"),
            body: json!({"TagKeys": ["tier"]}),
            expected: vec![("env", "staging"), ("team", "plat"), ("cost", "42")],
        },
        TagStep {
            case: "a key the resource does not carry removed",
            operation: Some("UntagResource"),
            body: json!({"TagKeys": ["tier"]}),
            expected: vec![("env", "staging"), ("team", "plat"), ("cost", "42")],
        },
        TagStep {
            case: "every remaining key removed",
            operation: Some("UntagResource"),
            body: json!({"TagKeys": ["env", "team", "cost"]}),
            expected: Vec::new(),
        },
    ];

    for TagStep {
        case: step,
        operation,
        mut body,
        expected,
    } in steps
    {
        let expected: BTreeMap<String, String> = expected
            .into_iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        if let Some(operation) = operation {
            body["ResourceArn"] = json!(arn);
            let (status, response) = call(&router, operation, &body).await;
            assert_eq!(
                status, 200,
                "case {step:?}: expected {operation} to succeed, got {status} {response}"
            );
        }

        let (status, response) = call(
            &router,
            "ListTagsForResource",
            &json!({"ResourceArn": arn}),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {step:?}: expected ListTagsForResource to succeed, got {status} {response}"
        );
        assert_eq!(
            tag_map(step, &response["Tags"]),
            expected,
            "case {step:?}: ListTagsForResource reported {response}"
        );

        // The same tags reach the configuration's own view.
        let stored = get_ok(&router, step, &arn).await;
        assert_eq!(
            tag_map(step, &stored["Tags"]),
            expected,
            "case {step:?}: GetLicenseConfiguration reported other tags: {stored}"
        );
    }
}

#[tokio::test]
async fn delete_removes_the_configuration_from_every_later_read() {
    let router = emulator();
    let case = "delete";
    let kept = create_ok(
        &router,
        case,
        &json!({"Name": "kept", "LicenseCountingType": "Core"}),
    )
    .await;
    let doomed = create_ok(
        &router,
        case,
        &json!({"Name": "doomed", "LicenseCountingType": "Core", "Tags": tag_list(&[("a", "b")])}),
    )
    .await;

    let (status, response) = call(
        &router,
        "DeleteLicenseConfiguration",
        &json!({"LicenseConfigurationArn": doomed}),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the delete to succeed, got {status} {response}"
    );

    let listed: BTreeSet<String> = list_all_arns(&router, case, Some(1), &json!({}))
        .await
        .into_iter()
        .collect();
    assert_eq!(
        listed,
        [kept.clone()].into(),
        "case {case:?}: the deleted configuration is still listed"
    );

    // Every operation that reaches the record now refuses the ARN, including a
    // second delete — the tags went with it.
    for (operation, body) in [
        (
            "GetLicenseConfiguration",
            json!({"LicenseConfigurationArn": doomed}),
        ),
        (
            "DeleteLicenseConfiguration",
            json!({"LicenseConfigurationArn": doomed}),
        ),
        (
            "UpdateLicenseConfiguration",
            json!({"LicenseConfigurationArn": doomed, "Name": "zombie"}),
        ),
        ("ListTagsForResource", json!({"ResourceArn": doomed})),
    ] {
        let (status, response) = call(&router, operation, &body).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected {operation} to answer 400 after the delete, got {status} {response}"
        );
        assert_eq!(
            error_code(&response),
            "InvalidParameterValueException",
            "case {case:?}: expected InvalidParameterValueException from {operation}, got {response}"
        );
    }
}

#[tokio::test]
async fn an_arn_the_store_does_not_hold_is_a_modelled_400() {
    let router = emulator();
    // The rule holds for any ARN the store does not hold, whatever its shape: a
    // well-formed one that was never minted, one for another region or account, one
    // naming another service's resource, and text that is no ARN at all.
    let unknown_arns = [
        format!(
            "arn:aws:license-manager:{DEFAULT_REGION}:{ACCOUNT_ID}:license-configuration:lic-{}",
            "0".repeat(32)
        ),
        format!(
            "arn:aws:license-manager:eu-west-1:{ACCOUNT_ID}:license-configuration:lic-{}",
            "a".repeat(32)
        ),
        format!(
            "arn:aws:license-manager:{DEFAULT_REGION}:123456789012:license-configuration:lic-{}",
            "b".repeat(32)
        ),
        format!("arn:aws:s3:::a-bucket/{ACCOUNT_ID}"),
        "lic-not-an-arn".to_string(),
        String::new(),
    ];

    for arn in unknown_arns {
        for (operation, body) in [
            (
                "GetLicenseConfiguration",
                json!({"LicenseConfigurationArn": arn}),
            ),
            (
                "UpdateLicenseConfiguration",
                json!({"LicenseConfigurationArn": arn, "LicenseCount": 1}),
            ),
            (
                "DeleteLicenseConfiguration",
                json!({"LicenseConfigurationArn": arn}),
            ),
            (
                "TagResource",
                json!({"ResourceArn": arn, "Tags": tag_list(&[("k", "v")])}),
            ),
            (
                "UntagResource",
                json!({"ResourceArn": arn, "TagKeys": ["k"]}),
            ),
            ("ListTagsForResource", json!({"ResourceArn": arn})),
        ] {
            let case = format!("{operation} with {arn:?}");
            let (status, response) = call(&router, operation, &body).await;
            assert_eq!(
                status, 400,
                "case {case:?}: expected a 400, got {status} {response}"
            );
            assert_eq!(
                error_code(&response),
                "InvalidParameterValueException",
                "case {case:?}: expected InvalidParameterValueException, got {response}"
            );
        }
    }
}

#[tokio::test]
async fn a_counting_type_or_status_outside_the_modelled_catalog_is_refused() {
    let router = emulator();
    // Anything outside the modelled `LicenseCountingType` catalog is refused with a
    // 400, and nothing is stored for it. The catalog is a modelled enum, so the
    // centralized constraint walk is what finds the value — and this operation
    // declares no `ValidationException`, so the refusal is stated as the parameter
    // value it will not accept, naming the value and the member.
    for counting_type in ["CPU", "vcpu", "INSTANCE", "Cores", ""] {
        let case = format!("LicenseCountingType={counting_type:?}");
        let (status, response) = call(
            &router,
            "CreateLicenseConfiguration",
            &json!({"Name": "rejected", "LicenseCountingType": counting_type}),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected a 400, got {status} {response}"
        );
        assert_eq!(
            error_code(&response),
            "InvalidParameterValueException",
            "case {case:?}: expected InvalidParameterValueException, got {response}"
        );
        assert_eq!(
            message(&case, &response),
            format!("Invalid parameter value {counting_type} for licenseCountingType"),
            "case {case:?}: unexpected refusal sentence in {response}"
        );
    }
    assert!(
        list_all_arns(&router, "nothing stored", None, &json!({}))
            .await
            .is_empty(),
        "a refused create must store nothing"
    );

    // Same rule for the modelled `LicenseConfigurationStatus` catalog on update.
    let arn = create_ok(
        &router,
        "status catalog",
        &json!({"Name": "live", "LicenseCountingType": "Core"}),
    )
    .await;
    for status_value in ["ENABLED", "available", "DELETED", ""] {
        let case = format!("LicenseConfigurationStatus={status_value:?}");
        let (status, response) = call(
            &router,
            "UpdateLicenseConfiguration",
            &json!({"LicenseConfigurationArn": arn, "LicenseConfigurationStatus": status_value}),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected a 400, got {status} {response}"
        );
        assert_eq!(
            error_code(&response),
            "InvalidParameterValueException",
            "case {case:?}: expected InvalidParameterValueException, got {response}"
        );
        assert_eq!(
            message(&case, &response),
            format!("Invalid parameter value {status_value} for licenseConfigurationStatus"),
            "case {case:?}: unexpected refusal sentence in {response}"
        );
        // The refused update left the stored status alone.
        let stored = get_ok(&router, &case, &arn).await;
        assert_eq!(
            stored["Status"].as_str(),
            Some("AVAILABLE"),
            "case {case:?}: a refused update changed the stored Status: {stored}"
        );
    }
    // Both catalog values are accepted and persisted.
    for status_value in ["DISABLED", "AVAILABLE"] {
        let case = format!("LicenseConfigurationStatus={status_value:?}");
        let (status, response) = call(
            &router,
            "UpdateLicenseConfiguration",
            &json!({"LicenseConfigurationArn": arn, "LicenseConfigurationStatus": status_value}),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the update to succeed, got {status} {response}"
        );
        let stored = get_ok(&router, &case, &arn).await;
        assert_eq!(
            stored["Status"].as_str(),
            Some(status_value),
            "case {case:?}: Status was not persisted, got {stored}"
        );
    }
}

#[tokio::test]
async fn a_value_a_modelled_constraint_refuses_is_stated_as_an_invalid_parameter_value() {
    let router = emulator();
    // The rule generalises over the CONSTRAINT CLASS and over the operation: an
    // enumerated value set, a pattern and a numeric range, on three operations
    // whose model declares no `ValidationException` (two of them not cited by the
    // issue, one of them still a stub — the refusal is stated before any handler
    // runs, so an unwritten handler words nothing).
    let rows: Vec<(&str, &str, Value, &str, &str)> = vec![
        (
            "an enumerated value set",
            "CreateLicenseConfiguration",
            json!({"Name": "rejected", "LicenseCountingType": "Cores"}),
            "licenseCountingType",
            "Cores",
        ),
        (
            "a pattern",
            "GetLicenseConversionTask",
            json!({"LicenseConversionTaskId": "not-a-task-id"}),
            "licenseConversionTaskId",
            "not-a-task-id",
        ),
        (
            "a numeric range below its minimum",
            "ListLicenseVersions",
            json!({
                "LicenseArn": format!(
                    "arn:aws:license-manager:{DEFAULT_REGION}:{ACCOUNT_ID}:license:l-{}",
                    "c".repeat(32)
                ),
                "MaxResults": 0
            }),
            "maxResults",
            "0",
        ),
        (
            "a numeric range above its maximum",
            "ListLicenseVersions",
            json!({
                "LicenseArn": format!(
                    "arn:aws:license-manager:{DEFAULT_REGION}:{ACCOUNT_ID}:license:l-{}",
                    "c".repeat(32)
                ),
                "MaxResults": 101
            }),
            "maxResults",
            "101",
        ),
    ];

    for (class, operation, body, member, value) in rows {
        let case = format!("{operation} breaking {class}");
        let (status, response) = call(&router, operation, &body).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected a 400, got {status} {response}"
        );
        assert_eq!(
            error_code(&response),
            "InvalidParameterValueException",
            "case {case:?}: expected InvalidParameterValueException, got {response}"
        );
        assert_eq!(
            message(&case, &response),
            format!("Invalid parameter value {value} for {member}"),
            "case {case:?}: unexpected refusal sentence in {response}"
        );
    }
}

#[tokio::test]
async fn a_collection_past_the_store_bound_is_refused() {
    let router = emulator();
    let over = service_com_amazonaws_licensemanager_awslicensemanager::state::MAX_TAGS + 1;
    let pairs: Vec<(String, String)> = (0..over)
        .map(|n| (format!("key-{n}"), format!("value-{n}")))
        .collect();
    let tags = Value::Array(
        pairs
            .iter()
            .map(|(key, value)| json!({"Key": key, "Value": value}))
            .collect(),
    );

    let case = "more tags than the store accepts";
    let (status, response) = call(
        &router,
        "CreateLicenseConfiguration",
        &json!({"Name": "over-tagged", "LicenseCountingType": "Core", "Tags": tags}),
    )
    .await;
    assert_eq!(
        status, 400,
        "case {case:?}: expected a 400, got {status} {response}"
    );
    assert_eq!(
        error_code(&response),
        "InvalidParameterValueException",
        "case {case:?}: expected InvalidParameterValueException, got {response}"
    );
    assert!(
        list_all_arns(&router, case, None, &json!({})).await.is_empty(),
        "case {case:?}: a refused create must store nothing"
    );

    // The same rule on TagResource: a merge that would take the resource past the
    // cap changes nothing, so the tags it already carried are still exactly there.
    let arn = create_ok(
        &router,
        case,
        &json!({"Name": "tagged", "LicenseCountingType": "Core", "Tags": tag_list(&[("env", "dev")])}),
    )
    .await;
    let bulk = Value::Array(
        pairs
            .iter()
            .map(|(key, value)| json!({"Key": key, "Value": value}))
            .collect(),
    );
    let (status, response) = call(
        &router,
        "TagResource",
        &json!({"ResourceArn": arn, "Tags": bulk}),
    )
    .await;
    assert_eq!(
        status, 400,
        "case {case:?}: expected TagResource to answer 400, got {status} {response}"
    );
    assert_eq!(
        error_code(&response),
        "InvalidParameterValueException",
        "case {case:?}: expected InvalidParameterValueException from TagResource, got {response}"
    );
    let (_, tags_after) = call(
        &router,
        "ListTagsForResource",
        &json!({"ResourceArn": arn}),
    )
    .await;
    assert_eq!(
        tag_map(case, &tags_after["Tags"]),
        [("env".to_string(), "dev".to_string())].into(),
        "case {case:?}: a refused TagResource changed the stored tags: {tags_after}"
    );
}

#[tokio::test]
async fn a_member_outside_the_store_rule_is_refused_on_create_and_on_update() {
    let router = emulator();
    let arn = create_ok(
        &router,
        "member rules",
        &json!({"Name": "baseline", "LicenseCountingType": "Core", "LicenseCount": 5}),
    )
    .await;
    let before = get_ok(&router, "member rules", &arn).await;

    // The rule generalises over the member and over how it is out of bounds: a
    // count or an instant below zero, and a string past the length the store takes.
    let over_long = "x".repeat(
        service_com_amazonaws_licensemanager_awslicensemanager::state::MAX_TEXT_LEN + 1,
    );
    let rows: Vec<(&str, Value)> = vec![
        ("a negative LicenseCount", json!({"LicenseCount": -1})),
        (
            "a large negative LicenseCount",
            json!({"LicenseCount": -9_007_199_254_740_991_i64}),
        ),
        ("a negative LicenseExpiry", json!({"LicenseExpiry": -1})),
        ("an over-long Name", json!({"Name": over_long.clone()})),
        (
            "an over-long Description",
            json!({"Description": over_long.clone()}),
        ),
        (
            "an over-long LicenseRules entry",
            json!({"LicenseRules": [over_long.clone()]}),
        ),
        (
            "an over-long ProductInformationFilterName",
            json!({"ProductInformationList": [{
                "ResourceType": "RDS",
                "ProductInformationFilterList": [{
                    "ProductInformationFilterName": over_long.clone(),
                    "ProductInformationFilterComparator": "EQUALS"
                }]
            }]}),
        ),
        (
            "an over-long ProductInformationFilterValue entry",
            json!({"ProductInformationList": [{
                "ResourceType": "RDS",
                "ProductInformationFilterList": [{
                    "ProductInformationFilterName": "Engine Edition",
                    "ProductInformationFilterValue": [over_long.clone()],
                    "ProductInformationFilterComparator": "EQUALS"
                }]
            }]}),
        ),
    ];

    for (case, member) in rows {
        // Create and update persist the same members into the same record, so both
        // apply the same rule.
        let mut create = member.clone();
        create["Name"] = create
            .get("Name")
            .cloned()
            .unwrap_or_else(|| json!("rejected"));
        create["LicenseCountingType"] = json!("Core");
        let (status, response) = call(&router, "CreateLicenseConfiguration", &create).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected the create to answer 400, got {status} {response}"
        );
        assert_eq!(
            error_code(&response),
            "InvalidParameterValueException",
            "case {case:?}: expected InvalidParameterValueException from the create, got {response}"
        );

        let mut update = member.clone();
        update["LicenseConfigurationArn"] = json!(arn);
        let (status, response) = call(&router, "UpdateLicenseConfiguration", &update).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected the update to answer 400, got {status} {response}"
        );
        assert_eq!(
            error_code(&response),
            "InvalidParameterValueException",
            "case {case:?}: expected InvalidParameterValueException from the update, got {response}"
        );
    }

    // Nothing the refusals touched reached the store.
    assert_eq!(
        get_ok(&router, "member rules", &arn).await,
        before,
        "a refused create or update must leave the stored configuration unchanged"
    );
    assert_eq!(
        list_all_arns(&router, "member rules", None, &json!({})).await,
        vec![arn],
        "a refused create must store nothing"
    );
}

#[tokio::test]
async fn a_create_past_the_store_cap_is_refused_and_the_store_stays_at_its_cap() {
    let router = emulator();
    let cap = service_com_amazonaws_licensemanager_awslicensemanager::state::MAX_LICENSE_CONFIGURATIONS;
    let case = "the store at its cap";

    // Fill the store to exactly its cap: every one of these must succeed.
    for n in 0..cap {
        let (status, response) = call(
            &router,
            "CreateLicenseConfiguration",
            &json!({"Name": format!("filler-{n}"), "LicenseCountingType": "Core"}),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: create {n} of {cap} must succeed, got {status} {response}"
        );
    }

    // The next few are refused with the modelled resource-limit error, and the
    // store neither grows nor loses anything.
    for attempt in 0..3 {
        let (status, response) = call(
            &router,
            "CreateLicenseConfiguration",
            &json!({"Name": format!("over-{attempt}"), "LicenseCountingType": "Core"}),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: attempt {attempt} past the cap must answer 400, got {status} {response}"
        );
        assert_eq!(
            error_code(&response),
            "ResourceLimitExceededException",
            "case {case:?}: attempt {attempt} expected ResourceLimitExceededException, got {response}"
        );
    }

    let listed = list_all_arns(&router, case, Some(100), &json!({})).await;
    assert_eq!(
        listed.len(),
        cap,
        "case {case:?}: the store holds {} configurations, not the {cap} it was filled to",
        listed.len()
    );
    assert_eq!(
        listed.iter().collect::<BTreeSet<&String>>().len(),
        cap,
        "case {case:?}: the store reported a duplicate ARN"
    );

    // A delete frees exactly one slot, and the next create takes it.
    let (status, response) = call(
        &router,
        "DeleteLicenseConfiguration",
        &json!({"LicenseConfigurationArn": listed[0]}),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the delete to succeed, got {status} {response}"
    );
    let (status, response) = call(
        &router,
        "CreateLicenseConfiguration",
        &json!({"Name": "after-delete", "LicenseCountingType": "Core"}),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: a create into the freed slot must succeed, got {status} {response}"
    );
    assert_eq!(
        list_all_arns(&router, case, Some(100), &json!({})).await.len(),
        cap,
        "case {case:?}: the store must be back at its cap"
    );
}

// ---------------------------------------------------------------------------
// How this surface REFUSES a request: the identity of the error, and the
// sentence it carries.
// ---------------------------------------------------------------------------

/// The `Message` member of a refusal body, as the modelled error shapes of this
/// service carry it.
fn message(case: &str, body: &Value) -> String {
    body.get("Message")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: no Message member in {body}"))
        .to_string()
}

/// An ARN of the right shape that no configuration was ever stored under.
fn unstored_configuration_arn(suffix: &str) -> String {
    format!(
        "arn:aws:license-manager:{DEFAULT_REGION}:{ACCOUNT_ID}:license-configuration:lic-{:0>32}",
        suffix
    )
}

/// One row of the absent-required-member table: the operation, a body that omits
/// one of its `@required` members, and that member's wire name.
struct AbsentMemberCase {
    operation: &'static str,
    body: Value,
    member: &'static str,
}

#[tokio::test]
async fn an_absent_required_member_is_refused_under_a_code_the_operation_models() {
    let router = emulator();
    // The table spans the operations whose model declares no `ValidationException`
    // — so `InvalidParameterValueException` is the only client error they can
    // answer with — and, for each, a `@required` member the request omits. The
    // last two rows are operations the issue did not cite, one of them still a
    // stub: the rule is keyed on the model's error list, not on the handler.
    let rows = vec![
        AbsentMemberCase {
            operation: "CreateLicenseConfiguration",
            body: json!({"LicenseCountingType": "Core"}),
            member: "name",
        },
        AbsentMemberCase {
            operation: "CreateLicenseConfiguration",
            body: json!({"Name": "no-counting-type"}),
            member: "licenseCountingType",
        },
        AbsentMemberCase {
            operation: "GetLicenseConfiguration",
            body: json!({}),
            member: "licenseConfigurationArn",
        },
        AbsentMemberCase {
            operation: "UpdateLicenseConfiguration",
            body: json!({"LicenseCount": 1}),
            member: "licenseConfigurationArn",
        },
        AbsentMemberCase {
            operation: "DeleteLicenseConfiguration",
            body: json!({}),
            member: "licenseConfigurationArn",
        },
        AbsentMemberCase {
            operation: "ListLicenseSpecificationsForResource",
            body: json!({"MaxResults": 10}),
            member: "resourceArn",
        },
        AbsentMemberCase {
            operation: "ListAssociationsForLicenseConfiguration",
            body: json!({}),
            member: "licenseConfigurationArn",
        },
    ];

    for AbsentMemberCase {
        operation,
        body,
        member,
    } in rows
    {
        let case = format!("{operation} without {member}");
        let (status, response) = call(&router, operation, &body).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected a 400, got {status} {response}"
        );
        assert_eq!(
            error_code(&response),
            "InvalidParameterValueException",
            "case {case:?}: expected InvalidParameterValueException, got {response}"
        );
        assert_eq!(
            message(&case, &response),
            format!("Required parameter {member} must not be null."),
            "case {case:?}: unexpected refusal sentence in {response}"
        );
    }

    // Nothing was stored for any of them.
    assert!(
        list_all_arns(&router, "absent members", None, &json!({}))
            .await
            .is_empty(),
        "a refused request must store nothing"
    );
}

#[tokio::test]
async fn an_operation_that_models_validation_exception_keeps_that_envelope() {
    let router = emulator();
    // The contrast rows: these three operations DO declare `ValidationException`
    // (`smithy_query(query="digest")`), so an absent required member keeps the
    // centralized walk's own refusal instead of being restated as a parameter
    // value this service will not accept.
    for (operation, body) in [
        ("ListTagsForResource", json!({})),
        ("TagResource", json!({"Tags": tag_list(&[("k", "v")])})),
        ("UntagResource", json!({"TagKeys": ["k"]})),
    ] {
        let case = format!("{operation} without ResourceArn");
        let (status, response) = call(&router, operation, &body).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected a 400, got {status} {response}"
        );
        assert_eq!(
            error_code(&response),
            "ValidationException",
            "case {case:?}: expected ValidationException, got {response}"
        );
    }
}

#[tokio::test]
async fn a_value_below_zero_is_refused_naming_the_field_and_echoing_nothing() {
    // The rule generalises over the member and over how far below zero the value
    // is; the accepted boundary is zero itself.
    let rows: Vec<(&str, &str, Value)> = vec![
        (
            "licenseCount",
            "LicenseCount of -1",
            json!({"Name": "neg", "LicenseCountingType": "Core", "LicenseCount": -1}),
        ),
        (
            "licenseCount",
            "LicenseCount of -5",
            json!({"Name": "neg", "LicenseCountingType": "Core", "LicenseCount": -5}),
        ),
        (
            "licenseCount",
            "a very negative LicenseCount",
            json!({"Name": "neg", "LicenseCountingType": "vCPU", "LicenseCount": -9_007_199_254_740_991_i64}),
        ),
        (
            "licenseExpiry",
            "LicenseExpiry of -1",
            json!({"Name": "neg", "LicenseCountingType": "Socket", "LicenseExpiry": -1}),
        ),
        (
            "licenseExpiry",
            "LicenseExpiry of -2_000_000_000",
            json!({"Name": "neg", "LicenseCountingType": "Instance", "LicenseExpiry": -2_000_000_000_i64}),
        ),
    ];

    for (member, case, body) in rows {
        let router = emulator();
        let (status, response) = call(&router, "CreateLicenseConfiguration", &body).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected a 400, got {status} {response}"
        );
        assert_eq!(
            error_code(&response),
            "InvalidParameterValueException",
            "case {case:?}: expected InvalidParameterValueException, got {response}"
        );
        let stated = message(case, &response);
        assert_eq!(
            stated,
            format!("Field '{member}' cannot be lesser than 0."),
            "case {case:?}: unexpected refusal sentence in {response}"
        );
        // The sentence is fixed text around the member name: the submitted number
        // is never echoed into it.
        let submitted = body
            .get(if member == "licenseCount" {
                "LicenseCount"
            } else {
                "LicenseExpiry"
            })
            .and_then(Value::as_i64)
            .expect("the row carries the member it is about");
        assert!(
            !stated.contains(&submitted.to_string()),
            "case {case:?}: the refusal echoed the submitted {submitted}: {stated:?}"
        );
        assert!(
            list_all_arns(&router, case, None, &json!({})).await.is_empty(),
            "case {case:?}: a refused create must store nothing"
        );
    }

    // The same sentence on the update path, and zero is accepted on both.
    let router = emulator();
    let arn = create_ok(
        &router,
        "zero is accepted",
        &json!({"Name": "zeroed", "LicenseCountingType": "Core", "LicenseCount": 0, "LicenseExpiry": 0}),
    )
    .await;
    let (status, response) = call(
        &router,
        "UpdateLicenseConfiguration",
        &json!({"LicenseConfigurationArn": arn, "LicenseCount": -7}),
    )
    .await;
    assert_eq!(
        status, 400,
        "case \"update with a negative LicenseCount\": expected a 400, got {status} {response}"
    );
    assert_eq!(
        message("update with a negative LicenseCount", &response),
        "Field 'licenseCount' cannot be lesser than 0.",
        "case \"update with a negative LicenseCount\": unexpected sentence in {response}"
    );
    let stored = get_ok(&router, "update with a negative LicenseCount", &arn).await;
    assert_eq!(
        stored["LicenseCount"].as_i64(),
        Some(0),
        "a refused update changed the stored LicenseCount: {stored}"
    );
}

/// One row of the enforcement table: the `LicenseCountHardLimit` and `LicenseCount`
/// a create carries, and whether the configuration it describes is one this service
/// stores.
struct EnforcementCase {
    case: &'static str,
    hard_limit: Option<bool>,
    license_count: Option<i64>,
    stored: bool,
}

/// The sentence this service refuses an enforced count it cannot enforce with.
const ENFORCEMENT_REFUSAL: &str =
    "License count should be non-null and greater than zero, if license count has been enforced.";

#[tokio::test]
async fn an_enforced_license_count_must_be_positive() {
    // The table spans both members: the limit absent, off and on, against a count
    // absent, zero and positive. Only an ON limit without a positive count is
    // refused.
    let rows = vec![
        EnforcementCase {
            case: "a hard limit with no count",
            hard_limit: Some(true),
            license_count: None,
            stored: false,
        },
        EnforcementCase {
            case: "a hard limit over a zero count",
            hard_limit: Some(true),
            license_count: Some(0),
            stored: false,
        },
        EnforcementCase {
            case: "a hard limit over a count of one",
            hard_limit: Some(true),
            license_count: Some(1),
            stored: true,
        },
        EnforcementCase {
            case: "a hard limit over a large count",
            hard_limit: Some(true),
            license_count: Some(9_007_199_254_740_991),
            stored: true,
        },
        EnforcementCase {
            case: "a soft limit with no count",
            hard_limit: Some(false),
            license_count: None,
            stored: true,
        },
        EnforcementCase {
            case: "a soft limit over a zero count",
            hard_limit: Some(false),
            license_count: Some(0),
            stored: true,
        },
        EnforcementCase {
            case: "no limit at all",
            hard_limit: None,
            license_count: None,
            stored: true,
        },
        EnforcementCase {
            case: "no limit over a zero count",
            hard_limit: None,
            license_count: Some(0),
            stored: true,
        },
    ];

    for EnforcementCase {
        case,
        hard_limit,
        license_count,
        stored,
    } in rows
    {
        let router = emulator();
        let mut body = json!({"Name": "enforced", "LicenseCountingType": "Core"});
        if let Some(hard_limit) = hard_limit {
            body["LicenseCountHardLimit"] = json!(hard_limit);
        }
        if let Some(license_count) = license_count {
            body["LicenseCount"] = json!(license_count);
        }
        let (status, response) = call(&router, "CreateLicenseConfiguration", &body).await;
        if stored {
            assert_eq!(
                status, 200,
                "case {case:?}: expected the create to succeed, got {status} {response}"
            );
            // What was accepted is what a later read reports.
            let arn = response["LicenseConfigurationArn"]
                .as_str()
                .unwrap_or_else(|| panic!("case {case:?}: no ARN in {response}"));
            let read_back = get_ok(&router, case, arn).await;
            assert_eq!(
                read_back["LicenseCountHardLimit"].as_bool(),
                hard_limit,
                "case {case:?}: LicenseCountHardLimit was not stored as supplied: {read_back}"
            );
            assert_eq!(
                read_back["LicenseCount"].as_i64(),
                license_count,
                "case {case:?}: LicenseCount was not stored as supplied: {read_back}"
            );
            continue;
        }
        assert_eq!(
            status, 400,
            "case {case:?}: expected a 400, got {status} {response}"
        );
        assert_eq!(
            error_code(&response),
            "InvalidParameterValueException",
            "case {case:?}: expected InvalidParameterValueException, got {response}"
        );
        assert_eq!(
            message(case, &response),
            ENFORCEMENT_REFUSAL,
            "case {case:?}: unexpected refusal sentence in {response}"
        );
        assert!(
            list_all_arns(&router, case, None, &json!({})).await.is_empty(),
            "case {case:?}: a refused create must mint no configuration"
        );
    }
}

#[tokio::test]
async fn an_update_is_judged_on_the_configuration_it_would_leave_behind() {
    // Each row: what the store holds, the patch, and whether the configuration the
    // patch would leave behind is one this service holds. A member the patch omits
    // keeps its stored value, so the same patch is acceptable over one record and
    // refused over another.
    let rows: Vec<(&str, Value, Value, bool)> = vec![
        (
            "the limit turned on over a stored count",
            json!({"LicenseCount": 5, "LicenseCountHardLimit": false}),
            json!({"LicenseCountHardLimit": true}),
            true,
        ),
        (
            "the limit turned on over a stored zero count",
            json!({"LicenseCount": 0, "LicenseCountHardLimit": false}),
            json!({"LicenseCountHardLimit": true}),
            false,
        ),
        (
            "the limit turned on over no stored count",
            json!({}),
            json!({"LicenseCountHardLimit": true}),
            false,
        ),
        (
            "the limit and a count turned on together",
            json!({}),
            json!({"LicenseCountHardLimit": true, "LicenseCount": 3}),
            true,
        ),
        (
            "the count dropped to zero under a stored limit",
            json!({"LicenseCount": 4, "LicenseCountHardLimit": true}),
            json!({"LicenseCount": 0}),
            false,
        ),
        (
            "the count raised under a stored limit",
            json!({"LicenseCount": 4, "LicenseCountHardLimit": true}),
            json!({"LicenseCount": 40}),
            true,
        ),
        (
            "the limit turned off over a stored zero count",
            json!({"LicenseCount": 0, "LicenseCountHardLimit": false}),
            json!({"LicenseCountHardLimit": false}),
            true,
        ),
    ];

    for (case, seed, patch, accepted) in rows {
        let router = emulator();
        let mut body = json!({"Name": "updated", "LicenseCountingType": "Instance"});
        for (member, value) in seed.as_object().expect("the seed is an object") {
            body[member] = value.clone();
        }
        let arn = create_ok(&router, case, &body).await;
        let before = get_ok(&router, case, &arn).await;

        let mut request = patch.clone();
        request["LicenseConfigurationArn"] = json!(arn);
        let (status, response) = call(&router, "UpdateLicenseConfiguration", &request).await;
        if accepted {
            assert_eq!(
                status, 200,
                "case {case:?}: expected the update to succeed, got {status} {response}"
            );
            let after = get_ok(&router, case, &arn).await;
            for (member, value) in patch.as_object().expect("the patch is an object") {
                assert_eq!(
                    after.get(member),
                    Some(value),
                    "case {case:?}: {member} was not persisted: {after}"
                );
            }
            continue;
        }
        assert_eq!(
            status, 400,
            "case {case:?}: expected a 400, got {status} {response}"
        );
        assert_eq!(
            error_code(&response),
            "InvalidParameterValueException",
            "case {case:?}: expected InvalidParameterValueException, got {response}"
        );
        assert_eq!(
            message(case, &response),
            ENFORCEMENT_REFUSAL,
            "case {case:?}: unexpected refusal sentence in {response}"
        );
        assert_eq!(
            get_ok(&router, case, &arn).await,
            before,
            "case {case:?}: a refused update changed the stored configuration"
        );
    }
}

#[tokio::test]
async fn an_unresolvable_arn_is_refused_in_the_sentence_its_operation_states() {
    let router = emulator();
    // One configuration exists, so the ARNs below are refused because the store
    // resolves them to nothing — not because nothing is stored at all.
    let live = create_ok(
        &router,
        "unresolvable ARNs",
        &json!({"Name": "live", "LicenseCountingType": "Core"}),
    )
    .await;
    // A configuration that WAS stored and has since been deleted, so one of the
    // rows below is refused on state this test changed rather than on the ARN's
    // spelling.
    let deleted = create_ok(
        &router,
        "unresolvable ARNs",
        &json!({"Name": "doomed", "LicenseCountingType": "Core"}),
    )
    .await;
    let (status, response) = call(
        &router,
        "DeleteLicenseConfiguration",
        &json!({"LicenseConfigurationArn": deleted}),
    )
    .await;
    assert_eq!(
        status, 200,
        "expected the delete to succeed, got {status} {response}"
    );

    // The rule generalises over the ARN: one never stored, one deleted, one in
    // another region, and one naming a license rather than a configuration.
    let arns = vec![
        unstored_configuration_arn("1"),
        deleted,
        format!(
            "arn:aws:license-manager:eu-west-1:{ACCOUNT_ID}:license-configuration:lic-{}",
            "a".repeat(32)
        ),
        format!(
            "arn:aws:license-manager:{DEFAULT_REGION}:{ACCOUNT_ID}:license:l-{}",
            "b".repeat(32)
        ),
    ];

    for arn in &arns {
        // Each operation states the sentence AWS words for ITS surface: the
        // configuration readers name the ARN, the tagging surface names a generic
        // resource, and the update path states ownership and status without naming
        // anything.
        let rows: Vec<(&str, Value, String)> = vec![
            (
                "GetLicenseConfiguration",
                json!({"LicenseConfigurationArn": arn}),
                format!("Invalid license configuration ARN {arn}."),
            ),
            (
                "DeleteLicenseConfiguration",
                json!({"LicenseConfigurationArn": arn}),
                format!("Invalid license configuration ARN {arn}."),
            ),
            (
                "UpdateLicenseConfiguration",
                json!({"LicenseConfigurationArn": arn, "LicenseCount": 1}),
                "Please check license configurations for ownership and status.".to_string(),
            ),
            (
                "ListTagsForResource",
                json!({"ResourceArn": arn}),
                format!("Invalid resource ARN {arn}."),
            ),
            (
                "TagResource",
                json!({"ResourceArn": arn, "Tags": tag_list(&[("k", "v")])}),
                format!("Invalid resource ARN {arn}."),
            ),
            (
                "UntagResource",
                json!({"ResourceArn": arn, "TagKeys": ["k"]}),
                format!("Invalid resource ARN {arn}."),
            ),
        ];
        for (operation, body, expected) in rows {
            let case = format!("{operation} on {arn}");
            let (status, response) = call(&router, operation, &body).await;
            assert_eq!(
                status, 400,
                "case {case:?}: expected a 400, got {status} {response}"
            );
            assert_eq!(
                error_code(&response),
                "InvalidParameterValueException",
                "case {case:?}: expected InvalidParameterValueException, got {response}"
            );
            assert_eq!(
                message(&case, &response),
                expected,
                "case {case:?}: unexpected refusal sentence in {response}"
            );
        }
    }

    // The configuration that IS stored is still served, so the refusals above are
    // about resolution and not about the operations themselves.
    let stored = get_ok(&router, "the live configuration", &live).await;
    assert_eq!(
        stored["Name"].as_str(),
        Some("live"),
        "the stored configuration is still reported: {stored}"
    );
}

#[tokio::test]
async fn the_update_refusal_withholds_the_arn_it_was_asked_about() {
    let router = emulator();
    // The ownership sentence names nothing about the request, so an ARN that
    // resolves to nothing is not disclosed back to the caller — whatever ARN was
    // asked about.
    for suffix in ["1", "2", "deadbeef"] {
        let arn = unstored_configuration_arn(suffix);
        let case = format!("UpdateLicenseConfiguration on {arn}");
        let (status, response) = call(
            &router,
            "UpdateLicenseConfiguration",
            &json!({"LicenseConfigurationArn": arn, "Name": "renamed"}),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected a 400, got {status} {response}"
        );
        let stated = message(&case, &response);
        assert!(
            !stated.contains(&arn),
            "case {case:?}: the refusal disclosed the ARN: {stated:?}"
        );
    }
}

#[tokio::test]
async fn list_refuses_a_page_size_outside_the_range_it_pages_in() {
    let router = emulator();
    create_ok(
        &router,
        "page sizes",
        &json!({"Name": "only", "LicenseCountingType": "Core"}),
    )
    .await;

    // The range is 1 to 100 — the bound the model itself declares on this
    // service's `MaxSize100` page member
    // (`smithy_query(query="operation",operation="ListLicenseVersions")`), applied
    // here because this operation's own `MaxResults` declares none. Each published
    // bound, and one value past each, plus the extremes.
    let refused: Vec<(&str, Value)> = vec![
        ("MaxResults of 0", json!(0)),
        ("a negative MaxResults", json!(-1)),
        ("one past the page ceiling", json!(101)),
        ("a very large MaxResults", json!(2_147_483_647_i64)),
        ("a very negative MaxResults", json!(-2_147_483_648_i64)),
    ];
    for (case, max_results) in refused {
        let (status, response) = call(
            &router,
            "ListLicenseConfigurations",
            &json!({"MaxResults": max_results}),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected a 400, got {status} {response}"
        );
        assert_eq!(
            error_code(&response),
            "InvalidParameterValueException",
            "case {case:?}: expected InvalidParameterValueException, got {response}"
        );
        let stated = message(case, &response);
        assert_eq!(
            stated, "Invalid parameter value. Valid range is from 1 to 100 for MaxResults",
            "case {case:?}: unexpected refusal sentence in {response}"
        );
        // The sentence above is the whole of the refusal: fixed text stating the
        // range and the member, with no echo of the size that was asked for.
    }

    // Both published bounds are served.
    for max_results in [1_i64, 100] {
        let case = format!("MaxResults={max_results}");
        let (status, response) = call(
            &router,
            "ListLicenseConfigurations",
            &json!({"MaxResults": max_results}),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the list to succeed, got {status} {response}"
        );
    }
}

#[tokio::test]
async fn list_refuses_an_arn_selection_outside_the_size_it_accepts() {
    let router = emulator();
    let arn = create_ok(
        &router,
        "selection sizes",
        &json!({"Name": "only", "LicenseCountingType": "Core"}),
    )
    .await;

    // The accepted range is 1 to 100 entries: a request that carries the member at
    // all has to carry a usable selection. Both ends, and one past each.
    let refused: Vec<(&str, Value)> = vec![
        ("an empty selection", json!([])),
        (
            "one entry past the selection ceiling",
            Value::Array(
                (0..101)
                    .map(|n| json!(unstored_configuration_arn(&n.to_string())))
                    .collect(),
            ),
        ),
        (
            "a selection far past the ceiling",
            Value::Array(
                (0..250)
                    .map(|n| json!(unstored_configuration_arn(&n.to_string())))
                    .collect(),
            ),
        ),
    ];
    for (case, selection) in refused {
        let (status, response) = call(
            &router,
            "ListLicenseConfigurations",
            &json!({"LicenseConfigurationArns": selection}),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected a 400, got {status} {response}"
        );
        assert_eq!(
            error_code(&response),
            "InvalidParameterValueException",
            "case {case:?}: expected InvalidParameterValueException, got {response}"
        );
        assert_eq!(
            message(case, &response),
            "Invalid list size for parameter LicenseConfigurationArns. Valid range is range 1 to 100.",
            "case {case:?}: unexpected refusal sentence in {response}"
        );
    }

    // One entry and a selection at the ceiling are both served, and the page
    // reports exactly the stored configuration the selection names.
    for (case, selection) in [
        ("one entry", vec![arn.clone()]),
        (
            "a selection at the ceiling",
            std::iter::once(arn.clone())
                .chain((0..99).map(|n| unstored_configuration_arn(&n.to_string())))
                .collect(),
        ),
    ] {
        let walked = list_all_arns(
            &router,
            case,
            Some(100),
            &json!({"LicenseConfigurationArns": selection}),
        )
        .await;
        assert_eq!(
            walked,
            vec![arn.clone()],
            "case {case:?}: the selection reported {walked:?}"
        );
    }
}

/// One row of the license-rules table: the dimension the configuration counts by,
/// the rule list, and the restriction the refusal must name (`None` when the rules
/// are ones that dimension admits).
struct RuleCase {
    counting_type: &'static str,
    rules: &'static [&'static str],
    refused: Option<&'static str>,
}

#[tokio::test]
async fn a_license_rule_must_be_a_restriction_its_dimension_admits() {
    // The table walks the documented catalog: for every dimension, a restriction it
    // admits, a restriction another dimension admits and it does not, and a name no
    // dimension has. The catalog is the `LicenseRules` member's own documentation
    // (`smithy_query(query="doc",shape="CreateLicenseConfigurationRequest",member="LicenseRules")`).
    let rows: &[RuleCase] = &[
        RuleCase {
            counting_type: "Core",
            rules: &["#allowedTenancy=EC2-Default", "#minimumCores=1", "#maximumCores=8"],
            refused: None,
        },
        RuleCase {
            counting_type: "Core",
            rules: &["#licenseAffinityToHost=30"],
            refused: None,
        },
        RuleCase {
            counting_type: "Core",
            rules: &["#minimumSockets=1"],
            refused: Some("minimumSockets"),
        },
        RuleCase {
            counting_type: "Instance",
            rules: &["#minimumVcpus=2", "#maximumVcpus=16"],
            refused: None,
        },
        RuleCase {
            counting_type: "Instance",
            rules: &["#licenseAffinityToHost=30"],
            refused: Some("licenseAffinityToHost"),
        },
        RuleCase {
            counting_type: "Socket",
            rules: &["#minimumSockets=1", "#licenseAffinityToHost=180"],
            refused: None,
        },
        RuleCase {
            counting_type: "Socket",
            rules: &["#honorVcpuOptimization=True"],
            refused: Some("honorVcpuOptimization"),
        },
        RuleCase {
            counting_type: "vCPU",
            rules: &["#honorVcpuOptimization=True", "#minimumVcpus=1"],
            refused: None,
        },
        RuleCase {
            counting_type: "vCPU",
            rules: &["#maximumCores=2"],
            refused: Some("maximumCores"),
        },
        RuleCase {
            counting_type: "vCPU",
            rules: &["#bogusRule=1"],
            refused: Some("bogusRule"),
        },
        // The documented syntax is `#name=value`; an entry that spells neither is
        // reported as the restriction it asks for rather than quietly stored.
        RuleCase {
            counting_type: "Core",
            rules: &["bogusRule"],
            refused: Some("bogusRule"),
        },
        // The second entry is the offending one, so the walk does not stop at the
        // first.
        RuleCase {
            counting_type: "Core",
            rules: &["#minimumCores=1", "#maximumVcpus=4"],
            refused: Some("maximumVcpus"),
        },
    ];

    for RuleCase {
        counting_type,
        rules,
        refused,
    } in rows
    {
        let router = emulator();
        let case = format!("{counting_type} with {rules:?}");
        let body = json!({
            "Name": "ruled",
            "LicenseCountingType": counting_type,
            "LicenseRules": rules
        });
        let (status, response) = call(&router, "CreateLicenseConfiguration", &body).await;
        match refused {
            None => {
                assert_eq!(
                    status, 200,
                    "case {case:?}: expected the create to succeed, got {status} {response}"
                );
                let arn = response["LicenseConfigurationArn"]
                    .as_str()
                    .unwrap_or_else(|| panic!("case {case:?}: no ARN in {response}"));
                let stored = get_ok(&router, &case, arn).await;
                assert_eq!(
                    stored["LicenseRules"],
                    json!(rules),
                    "case {case:?}: the rules were not stored as supplied: {stored}"
                );
            }
            Some(restriction) => {
                assert_eq!(
                    status, 400,
                    "case {case:?}: expected a 400, got {status} {response}"
                );
                assert_eq!(
                    error_code(&response),
                    "InvalidParameterValueException",
                    "case {case:?}: expected InvalidParameterValueException, got {response}"
                );
                assert_eq!(
                    message(&case, &response),
                    format!(
                        "Restriction {restriction} not allowed for license type {counting_type}."
                    ),
                    "case {case:?}: unexpected refusal sentence in {response}"
                );
            }
        }
    }

    // An update cannot change the dimension, so a replacement rule list is judged
    // against the stored one — and a refused update leaves the stored rules alone.
    let router = emulator();
    let arn = create_ok(
        &router,
        "update rules",
        &json!({
            "Name": "ruled",
            "LicenseCountingType": "Socket",
            "LicenseRules": ["#minimumSockets=1"]
        }),
    )
    .await;
    let (status, response) = call(
        &router,
        "UpdateLicenseConfiguration",
        &json!({"LicenseConfigurationArn": arn, "LicenseRules": ["#minimumVcpus=2"]}),
    )
    .await;
    assert_eq!(
        status, 400,
        "case \"update to a rule the stored dimension does not admit\": expected a 400, \
         got {status} {response}"
    );
    assert_eq!(
        message("update to a foreign rule", &response),
        "Restriction minimumVcpus not allowed for license type Socket.",
        "case \"update to a rule the stored dimension does not admit\": unexpected \
         sentence in {response}"
    );
    let stored = get_ok(&router, "update rules", &arn).await;
    assert_eq!(
        stored["LicenseRules"],
        json!(["#minimumSockets=1"]),
        "a refused update changed the stored rules: {stored}"
    );
}

/// One row of the product-information table: the dimension the configuration counts
/// by, one `ProductInformationList` entry, and the sentence the refusal must state
/// (`None` when the entry is one this service stores).
struct ProductInformationCase {
    case: &'static str,
    counting_type: &'static str,
    entry: Value,
    refused: Option<String>,
}

/// The filter names the SSM_MANAGED surface supports, as the refusal lists them.
const SSM_FILTERS: &str = "Application Name, Application Publisher, Application Version, \
                           Platform Name, Platform Type, Tag:key, AccountId, License Included";

/// One `ProductInformation` entry carrying one filter.
fn product_information(resource_type: &str, name: &str, comparator: &str) -> Value {
    json!({
        "ResourceType": resource_type,
        "ProductInformationFilterList": [{
            "ProductInformationFilterName": name,
            "ProductInformationFilterValue": ["a value"],
            "ProductInformationFilterComparator": comparator
        }]
    })
}

#[tokio::test]
async fn product_information_must_name_a_resource_type_and_its_own_filters() {
    // The table spans the three rules product information is judged by — the
    // resource type itself, the dimension it can be licensed by, and the filters
    // and logical operators that resource type supports — with a refused and an
    // accepted row for each. The catalogs are the model's own
    // (`smithy_query(query="doc",shape="ProductInformation",member="ProductInformationFilterList")`).
    let rows: Vec<ProductInformationCase> = vec![
        ProductInformationCase {
            case: "a resource type that does not exist",
            counting_type: "Core",
            entry: product_information("EC2", "Application Name", "EQUALS"),
            refused: Some("Invalid or non-existent resource type in Product Information.".into()),
        },
        ProductInformationCase {
            case: "a resource type in the wrong case",
            counting_type: "Core",
            entry: product_information("ssm_managed", "Application Name", "EQUALS"),
            refused: Some("Invalid or non-existent resource type in Product Information.".into()),
        },
        ProductInformationCase {
            case: "an empty resource type",
            counting_type: "Core",
            entry: product_information("", "Application Name", "EQUALS"),
            refused: Some("Invalid or non-existent resource type in Product Information.".into()),
        },
        ProductInformationCase {
            case: "RDS on a configuration counting by cores",
            counting_type: "Core",
            entry: product_information("RDS", "Engine Edition", "EQUALS"),
            refused: Some("RDS product information is only supported vCPU license type.".into()),
        },
        ProductInformationCase {
            case: "RDS on a configuration counting by sockets",
            counting_type: "Socket",
            entry: product_information("RDS", "License Pack", "EQUALS"),
            refused: Some("RDS product information is only supported vCPU license type.".into()),
        },
        ProductInformationCase {
            case: "RDS on a configuration counting by vCPU",
            counting_type: "vCPU",
            entry: product_information("RDS", "Engine Edition", "EQUALS"),
            refused: None,
        },
        ProductInformationCase {
            case: "a filter name the SSM surface does not support",
            counting_type: "Core",
            entry: product_information("SSM_MANAGED", "Bogus Field", "EQUALS"),
            refused: Some(format!(
                "Unsupported filter name - Bogus Field - provided in product information \
                 for SSM_MANAGED. Supported values are [{SSM_FILTERS}]."
            )),
        },
        ProductInformationCase {
            case: "an SSM filter name asked of the RDS surface",
            counting_type: "vCPU",
            entry: product_information("RDS", "Application Name", "EQUALS"),
            refused: Some(
                "Unsupported filter name - Application Name - provided in product \
                 information for RDS. Supported values are [Engine Edition, License Pack]."
                    .into(),
            ),
        },
        ProductInformationCase {
            case: "a supported filter name in the wrong case",
            counting_type: "Core",
            entry: product_information("SSM_MANAGED", "application name", "EQUALS"),
            refused: Some(format!(
                "Unsupported filter name - application name - provided in product \
                 information for SSM_MANAGED. Supported values are [{SSM_FILTERS}]."
            )),
        },
        ProductInformationCase {
            case: "a comparator in the wrong case",
            counting_type: "Core",
            entry: product_information("SSM_MANAGED", "Application Name", "Equals"),
            refused: Some(
                "Unsupported filter comparator Equals provided for SSM Managed Instance. \
                 Supported comparators are [\"EQUALS\"]."
                    .into(),
            ),
        },
        ProductInformationCase {
            case: "an operator the filter is not applied with",
            counting_type: "Core",
            entry: product_information("SSM_MANAGED", "AccountId", "EQUALS"),
            refused: Some(
                "Unsupported filter comparator EQUALS provided for SSM Managed Instance. \
                 Supported comparators are [\"NOT_EQUALS\"]."
                    .into(),
            ),
        },
        ProductInformationCase {
            case: "the operator that filter IS applied with",
            counting_type: "Core",
            entry: product_information("SSM_MANAGED", "AccountId", "NOT_EQUALS"),
            refused: None,
        },
        ProductInformationCase {
            case: "a filter applied with either operator",
            counting_type: "Core",
            entry: product_information("SSM_MANAGED", "License Included", "NOT_EQUALS"),
            refused: None,
        },
        ProductInformationCase {
            case: "the tag filter with its key appended",
            counting_type: "Core",
            entry: product_information("SSM_MANAGED", "Tag:owner", "NOT_EQUALS"),
            refused: None,
        },
        ProductInformationCase {
            case: "the tag filter with an operator it is not applied with",
            counting_type: "Core",
            entry: product_information("SSM_MANAGED", "Tag:owner", "EQUALS"),
            refused: Some(
                "Unsupported filter comparator EQUALS provided for SSM Managed Instance. \
                 Supported comparators are [\"NOT_EQUALS\"]."
                    .into(),
            ),
        },
        ProductInformationCase {
            case: "a supported filter of the SSM surface",
            counting_type: "Instance",
            entry: product_information("SSM_MANAGED", "Platform Type", "EQUALS"),
            refused: None,
        },
    ];

    for ProductInformationCase {
        case,
        counting_type,
        entry,
        refused,
    } in rows
    {
        let router = emulator();
        let body = json!({
            "Name": "described",
            "LicenseCountingType": counting_type,
            "ProductInformationList": [entry.clone()]
        });
        let (status, response) = call(&router, "CreateLicenseConfiguration", &body).await;
        match &refused {
            None => {
                assert_eq!(
                    status, 200,
                    "case {case:?}: expected the create to succeed, got {status} {response}"
                );
                let arn = response["LicenseConfigurationArn"]
                    .as_str()
                    .unwrap_or_else(|| panic!("case {case:?}: no ARN in {response}"));
                let stored = get_ok(&router, case, arn).await;
                assert_eq!(
                    stored["ProductInformationList"],
                    json!([entry]),
                    "case {case:?}: the product information was not stored as supplied: {stored}"
                );
            }
            Some(expected) => {
                assert_eq!(
                    status, 400,
                    "case {case:?}: expected a 400, got {status} {response}"
                );
                assert_eq!(
                    error_code(&response),
                    "InvalidParameterValueException",
                    "case {case:?}: expected InvalidParameterValueException, got {response}"
                );
                assert_eq!(
                    &message(case, &response),
                    expected,
                    "case {case:?}: unexpected refusal sentence in {response}"
                );
                assert!(
                    list_all_arns(&router, case, None, &json!({})).await.is_empty(),
                    "case {case:?}: a refused create must store nothing"
                );
            }
        }
    }
}

#[tokio::test]
async fn list_refuses_a_filter_name_it_does_not_scope_on() {
    let router = emulator();
    create_ok(
        &router,
        "filter names",
        &json!({"Name": "only", "LicenseCountingType": "Core"}),
    )
    .await;

    // The rule generalises over the name: one no list scopes on, a documented name
    // in the wrong case, and an empty one.
    for name in ["bogusFilter", "LicenseCountingType", ""] {
        let case = format!("Filters Name={name:?}");
        let (status, response) = call(
            &router,
            "ListLicenseConfigurations",
            &json!({"Filters": [{"Name": name, "Values": ["Core"]}]}),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected a 400, got {status} {response}"
        );
        assert_eq!(
            error_code(&response),
            "InvalidParameterValueException",
            "case {case:?}: expected InvalidParameterValueException, got {response}"
        );
        assert_eq!(
            message(&case, &response),
            format!("{name} is not a supported filter."),
            "case {case:?}: unexpected refusal sentence in {response}"
        );
    }
}

#[tokio::test]
async fn list_refuses_a_page_token_it_did_not_issue() {
    let router = emulator();
    for n in 0..3 {
        create_ok(
            &router,
            "page tokens",
            &json!({"Name": format!("token-{n}"), "LicenseCountingType": "Core"}),
        )
        .await;
    }
    // A token this walk DID issue, to be presented against another scope below.
    let (status, response) = call(
        &router,
        "ListLicenseConfigurations",
        &json!({"MaxResults": 1}),
    )
    .await;
    assert_eq!(
        status, 200,
        "expected the first page to succeed, got {status} {response}"
    );
    let issued = response["NextToken"]
        .as_str()
        .expect("the first of three pages hands out a token")
        .to_string();

    // The rule generalises over what the token is: text this service never issued,
    // a truncated one, and one issued for a DIFFERENT scope.
    let rows: Vec<(&str, Value)> = vec![
        ("text that is not a token", json!({"NextToken": "not-a-token"})),
        ("an empty token", json!({"NextToken": ""})),
        (
            "a truncated token",
            json!({"NextToken": issued[..issued.len() / 2].to_string()}),
        ),
        (
            "a token issued for another scope",
            json!({
                "NextToken": issued.clone(),
                "Filters": [{"Name": "licenseCountingType", "Values": ["Core"]}]
            }),
        ),
    ];
    for (case, body) in rows {
        let (status, response) = call(&router, "ListLicenseConfigurations", &body).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected a 400, got {status} {response}"
        );
        assert_eq!(
            error_code(&response),
            "InvalidParameterValueException",
            "case {case:?}: expected InvalidParameterValueException, got {response}"
        );
        assert_eq!(
            message(case, &response),
            "Invalid value for nextToken parameter.",
            "case {case:?}: unexpected refusal sentence in {response}"
        );
    }

    // The token this walk issued still resumes the walk it was issued for.
    let (status, response) = call(
        &router,
        "ListLicenseConfigurations",
        &json!({"MaxResults": 1, "NextToken": issued}),
    )
    .await;
    assert_eq!(
        status, 200,
        "expected the issued token to resume the walk, got {status} {response}"
    );
}
