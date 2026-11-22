// SPDX-License-Identifier: CC0-1.0

//! The two paging rules EVERY paginated reader of this service shares, driven over the wire.
//!
//! Every call goes through the shared wire harness (`tests/all/harness.rs`), i.e. the same
//! `MultiServiceRouter::dispatch` the emulator's front door uses, so what these tests prove
//! is what a client sees.
//!
//! The rules, which `crate::state::page_size` and `crate::state::offset_page` state once for
//! the whole crate:
//!
//! * HOW LARGE a page is — the size the request asks for, the reader's own default when it
//!   asks for none (an absent `maxResults`, or the modelled range's lower bound `0`, which
//!   the service documents no meaning for), and never more than the reader's own ceiling.
//! * WHICH items that page carries and what follows it — the items after the point the
//!   previous page stopped at, with a `nextToken` handed out exactly when the sequence
//!   continues past the page, resuming where it stopped.
//! * WHICH WALK a page token belongs to — a `nextToken` names a boundary of the walk that
//!   minted it and of no other, so a reader resumes its own token and refuses one another
//!   reader minted, or one minted for a different selection of its own, rather than answering
//!   it with a page of a different result set.
//!
//! Both are asserted over a TABLE of every reader of this service whose page the request
//! sizes, each with its own default and ceiling (the eleven rows below), crossed with the
//! page sizes the rule generalises over: none at all, the range's lower bound, one item, one
//! short of the sequence, exactly the sequence, one past it, and the reader's own ceiling.
//! Nothing here asserts a page size the recorded calls happened to use; what is asserted is
//! that no reader answers a page size differently from the others.
//!
//! `GetRecommendationSummaries` is NOT one of them: its answer is one row per
//! `RecommendationSourceType` the service reports on, a set the vocabulary BOUNDS rather than
//! the account's resources growing, so it is answered whole and hands out no page boundary
//! at all (`tests/all/summary_block_sets.rs` states that rule).

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};
use service_com_amazonaws_computeoptimizer_computeoptimizerservice::state::{
    service_state, AnalysedResource, MemberAccountEnrollment, DEFAULT_PAGE_SIZE,
    ENROLLMENT_STATUSES_MAX_PAGE_SIZE, IDLE_MAX_PAGE_SIZE, MAX_PAGE_SIZE, RESOURCE_TYPE_EBS_VOLUME,
    SCOPE_ACCOUNT_ID, SCOPE_ORGANIZATION, SCOPE_RESOURCE_ARN, SOURCE_TYPE_RDS_DB_INSTANCE,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::harness::{
    call, call_ecs, create_bucket, emulator_with_analysed_services_and_engine, error_code,
    instance_arn, make_analysis_due, opt_in, put_preference, register_analysed_functions,
    seed_cloudwatch_series, ACCOUNT,
};

/// The destination bucket the export jobs below are created against. Compute Optimizer does
/// not create it; the request just names it.
const BUCKET: &str = "shared-paging-rules";

/// The cluster the analysed Amazon ECS services below run in.
const CLUSTER: &str = "paging-rules";

/// One paginated reader of this service: how to call it, where its page lives in the
/// response, what identifies an item of that page, and the two numbers it applies the shared
/// page-size rule with.
struct PagedReader {
    /// The operation name, as `X-Amz-Target` carries it.
    operation: &'static str,
    /// The request members other than `maxResults` and `nextToken`.
    request: Value,
    /// The output member the page is reported in.
    items: &'static str,
    /// The path to the member of an item that identifies it, so two reads can be compared
    /// item by item without depending on anything a clock derives.
    identity: &'static [&'static str],
    /// The page size this reader uses when the request asks for none.
    default_page: usize,
    /// The largest page this reader will build, whatever the request asks for.
    cap: usize,
}

/// Every reader of this service whose page a request sizes, each with the default and the
/// ceiling it applies the shared page-size rule with: nine of them page at the service's own
/// default under the model's 0..1000 ceiling, while `GetIdleRecommendations` and
/// `GetEnrollmentStatusesForOrganization` are bounded lower by what their own members
/// document.
fn readers() -> Vec<PagedReader> {
    vec![
        PagedReader {
            operation: "DescribeRecommendationExportJobs",
            request: json!({}),
            items: "recommendationExportJobs",
            identity: &["jobId"],
            default_page: DEFAULT_PAGE_SIZE,
            cap: MAX_PAGE_SIZE,
        },
        PagedReader {
            operation: "GetAutoScalingGroupRecommendations",
            request: json!({}),
            items: "autoScalingGroupRecommendations",
            identity: &["autoScalingGroupArn"],
            default_page: DEFAULT_PAGE_SIZE,
            cap: MAX_PAGE_SIZE,
        },
        PagedReader {
            operation: "GetEBSVolumeRecommendations",
            request: json!({}),
            items: "volumeRecommendations",
            identity: &["volumeArn"],
            default_page: DEFAULT_PAGE_SIZE,
            cap: MAX_PAGE_SIZE,
        },
        PagedReader {
            operation: "GetEC2InstanceRecommendations",
            request: json!({}),
            items: "instanceRecommendations",
            identity: &["instanceArn"],
            default_page: DEFAULT_PAGE_SIZE,
            cap: MAX_PAGE_SIZE,
        },
        PagedReader {
            operation: "GetECSServiceRecommendations",
            request: json!({}),
            items: "ecsServiceRecommendations",
            identity: &["serviceArn"],
            default_page: DEFAULT_PAGE_SIZE,
            cap: MAX_PAGE_SIZE,
        },
        PagedReader {
            operation: "GetEnrollmentStatusesForOrganization",
            request: json!({}),
            items: "accountEnrollmentStatuses",
            identity: &["accountId"],
            default_page: ENROLLMENT_STATUSES_MAX_PAGE_SIZE,
            cap: ENROLLMENT_STATUSES_MAX_PAGE_SIZE,
        },
        PagedReader {
            operation: "GetIdleRecommendations",
            request: json!({}),
            items: "idleRecommendations",
            identity: &["resourceArn"],
            default_page: IDLE_MAX_PAGE_SIZE,
            cap: IDLE_MAX_PAGE_SIZE,
        },
        PagedReader {
            operation: "GetLambdaFunctionRecommendations",
            request: json!({}),
            items: "lambdaFunctionRecommendations",
            identity: &["functionArn"],
            default_page: DEFAULT_PAGE_SIZE,
            cap: MAX_PAGE_SIZE,
        },
        PagedReader {
            operation: "GetLicenseRecommendations",
            request: json!({}),
            items: "licenseRecommendations",
            identity: &["resourceArn"],
            default_page: DEFAULT_PAGE_SIZE,
            cap: MAX_PAGE_SIZE,
        },
        PagedReader {
            operation: "GetRDSDatabaseRecommendations",
            request: json!({}),
            items: "rdsDBRecommendations",
            identity: &["resourceArn"],
            default_page: DEFAULT_PAGE_SIZE,
            cap: MAX_PAGE_SIZE,
        },
        PagedReader {
            operation: "GetRecommendationPreferences",
            request: json!({"resourceType": "Ec2Instance"}),
            items: "recommendationPreferencesDetails",
            identity: &["scope", "value"],
            default_page: DEFAULT_PAGE_SIZE,
            cap: MAX_PAGE_SIZE,
        },
    ]
}

/// A page size the request asks for, and the page it must be answered with.
#[derive(Debug)]
struct PageSizeCase {
    /// What the row is, for the assertion message.
    name: String,
    /// The `maxResults` the request carries, or `None` for a request naming none.
    max_results: Option<usize>,
    /// The items the first page must carry, by identity.
    expected: Vec<String>,
    /// Whether a `nextToken` must come with that page.
    expect_token: bool,
}

/// Whatever page size a request asks for, every reader answers it by the same rule: the size
/// asked for when it asks for one, the reader's own default when it asks for none, and never
/// more than the reader's own ceiling.
#[tokio::test]
async fn the_page_size_rule_is_the_same_for_every_paginated_reader() {
    let (router, _engine) = paged_emulator().await;

    for reader in readers() {
        let whole = page_of(&read(&router, &reader, "the unpaged read", None, None).await, &reader);
        let count = whole.len();
        assert!(
            count >= 2,
            "case {:?}: this fixture must hold at least two items for a page size to be \
             observable, got {whole:?}",
            reader.operation
        );
        assert!(
            count <= reader.default_page,
            "case {:?}: this fixture must fit one default-sized page of {}, got {count}",
            reader.operation,
            reader.default_page
        );

        // The rule generalises over the size asked for, so the rows span the forms that size
        // can take: none at all, the modelled range's lower bound (no size asked for), one
        // item, one short of the sequence, exactly it, one past it, and the ceiling.
        let cases = vec![
            PageSizeCase {
                name: "no maxResults at all".to_string(),
                max_results: None,
                expected: whole.clone(),
                expect_token: false,
            },
            PageSizeCase {
                name: "the modelled range's lower bound 0".to_string(),
                max_results: Some(0),
                expected: whole.clone(),
                expect_token: false,
            },
            PageSizeCase {
                name: "one item per page".to_string(),
                max_results: Some(1),
                expected: whole[..1].to_vec(),
                expect_token: true,
            },
            PageSizeCase {
                name: format!("one short of the sequence ({})", count - 1),
                max_results: Some(count - 1),
                expected: whole[..count - 1].to_vec(),
                expect_token: true,
            },
            PageSizeCase {
                name: format!("exactly the sequence ({count})"),
                max_results: Some(count),
                expected: whole.clone(),
                expect_token: false,
            },
            PageSizeCase {
                name: format!("one past the sequence ({})", count + 1),
                max_results: Some(count + 1),
                expected: whole.clone(),
                expect_token: false,
            },
            PageSizeCase {
                name: format!("the reader's own ceiling ({})", reader.cap),
                max_results: Some(reader.cap),
                expected: whole.clone(),
                expect_token: false,
            },
        ];

        for case in cases {
            let label = format!("{} / {}", reader.operation, case.name);
            let body = read(&router, &reader, &label, case.max_results, None).await;
            let page = page_of(&body, &reader);
            assert_eq!(
                page, case.expected,
                "case {label:?}: expected the page {:?}, got {page:?}",
                case.expected
            );
            assert!(
                page.len() <= reader.cap,
                "case {label:?}: a page may hold at most {} items, got {}",
                reader.cap,
                page.len()
            );
            assert_eq!(
                body["nextToken"].as_str().is_some(),
                case.expect_token,
                "case {label:?}: expected a nextToken: {}, got {body}",
                case.expect_token
            );
        }
    }
}

/// Whatever page size walks a reader's sequence, the walk reports that reader's unpaged
/// sequence exactly once, and only its final page comes without a token.
#[tokio::test]
async fn a_paged_walk_reports_the_unpaged_sequence_exactly_once() {
    let (router, _engine) = paged_emulator().await;

    for reader in readers() {
        let whole = page_of(&read(&router, &reader, "the unpaged read", None, None).await, &reader);
        let count = whole.len();

        // Rows: every page size that splits the sequence differently — one item at a time,
        // two at a time, and the whole sequence in one page.
        for page_size in [1, 2, count] {
            let label = format!("{} / maxResults={page_size}", reader.operation);
            let mut walked: Vec<String> = Vec::new();
            let mut pages: Vec<usize> = Vec::new();
            let mut token: Option<String> = None;
            loop {
                let body = read(&router, &reader, &label, Some(page_size), token.as_deref()).await;
                let page = page_of(&body, &reader);
                assert!(
                    page.len() <= page_size,
                    "case {label:?}: a page may hold at most {page_size} items, got {}",
                    page.len()
                );
                pages.push(page.len());
                walked.extend(page);
                token = body["nextToken"].as_str().map(ToString::to_string);
                let remaining = count - walked.len();
                assert_eq!(
                    token.is_some(),
                    remaining > 0,
                    "case {label:?}: with {remaining} items left, expected a nextToken: {}, got \
                     {body}",
                    remaining > 0
                );
                if token.is_none() {
                    break;
                }
                assert!(
                    pages.len() <= count,
                    "case {label:?}: the walk does not terminate, pages so far {pages:?}"
                );
            }
            assert_eq!(
                walked, whole,
                "case {label:?}: a paged walk must report the unpaged sequence, got {walked:?}"
            );
        }
    }
}

/// A `nextToken` resumes the WALK that minted it and no other.
///
/// Every reader of this service hands out an opaque token, and several of them encode the same
/// kind of boundary, so what makes a token this reader's is the operation and the selection
/// bound into it. Asserted over the whole table crossed with itself: each reader resumes its
/// OWN token, and refuses every other reader's with the one refusal this service models for a
/// value it cannot honour — never with a page of a different result set, and never with an
/// empty final page a client would read as "the walk is finished".
#[tokio::test]
async fn a_next_token_resumes_only_the_walk_that_minted_it() {
    let (router, _engine) = paged_emulator().await;

    // One token per reader, minted by a one-item page of its own walk: the fixture gives
    // every row more than one item to report, so each hands one out.
    let mut minted: Vec<(&'static str, String)> = Vec::new();
    for reader in readers() {
        let body = read(&router, &reader, "minting a token", Some(1), None).await;
        let token = body["nextToken"].as_str().unwrap_or_else(|| {
            panic!(
                "case {:?}: a one-item page of a longer sequence must hand out a nextToken, got \
                 {body}",
                reader.operation
            )
        });
        minted.push((reader.operation, token.to_string()));
    }

    for reader in readers() {
        let whole = page_of(&read(&router, &reader, "the unpaged read", None, None).await, &reader);
        assert!(
            whole.len() >= 2,
            "case {:?}: this fixture must hold at least two items for a token to resume at one, \
             got {whole:?}",
            reader.operation
        );
        let own = minted
            .iter()
            .find(|(operation, _)| *operation == reader.operation)
            .map(|(_, token)| token.clone())
            .expect("every reader of the table minted a token");

        // Its OWN token resumes its own walk, at the item after the page that minted it.
        let label = format!("{} resuming its own token", reader.operation);
        let page = page_of(
            &read(&router, &reader, &label, Some(1), Some(&own)).await,
            &reader,
        );
        assert_eq!(
            page,
            whole[1..2].to_vec(),
            "case {label:?}: expected the page {:?}, got {page:?}",
            &whole[1..2]
        );

        // Every OTHER reader's token names a boundary of a walk this reader is not.
        for (minted_by, token) in &minted {
            if *minted_by == reader.operation {
                continue;
            }
            let label = format!("{} resuming a token {minted_by} minted", reader.operation);
            let mut request = reader.request.clone();
            request["nextToken"] = json!(token);
            let (status, body) = call(&router, reader.operation, request.clone()).await;
            assert_eq!(
                status, 400,
                "case {label:?}: expected {request} to be refused with 400, got {status}: {body}"
            );
            assert_eq!(
                error_code(&body),
                "InvalidParameterValueException",
                "case {label:?}: expected InvalidParameterValueException, got {body}"
            );
        }
    }
}

/// The member each recommendation reader narrows its selection with, from that operation's
/// input in this service's Smithy model (`GetAutoScalingGroupRecommendations$autoScalingGroupArns`,
/// `GetEBSVolumeRecommendations$volumeArns`, `GetEC2InstanceRecommendations$instanceArns`,
/// `GetECSServiceRecommendations$serviceArns`, `GetLambdaFunctionRecommendations$functionArns`).
/// A reader absent from this list narrows its selection by no resource ARN at all.
const RESOURCE_SELECTION_MEMBERS: &[(&str, &str)] = &[
    (
        "GetAutoScalingGroupRecommendations",
        "autoScalingGroupArns",
    ),
    ("GetEBSVolumeRecommendations", "volumeArns"),
    ("GetEC2InstanceRecommendations", "instanceArns"),
    ("GetECSServiceRecommendations", "serviceArns"),
    ("GetLambdaFunctionRecommendations", "functionArns"),
];

/// A `nextToken` resumes the SELECTION it was minted for and no other: a token of a reader's
/// own walk, handed back with a different set of resources selected, is refused rather than
/// resolved against a sequence it names no position in. The rows differ in how the selection
/// changed — the same resources named explicitly, and a narrower set — because what makes the
/// token foreign is the selection it was minted for, not how much of it survives.
#[tokio::test]
async fn a_next_token_does_not_resume_a_different_selection() {
    let (router, _engine) = paged_emulator().await;

    for reader in readers() {
        let Some((_, member)) = RESOURCE_SELECTION_MEMBERS
            .iter()
            .find(|(operation, _)| *operation == reader.operation)
        else {
            continue;
        };
        let whole = page_of(&read(&router, &reader, "the unpaged read", None, None).await, &reader);
        assert!(
            whole.len() >= 2,
            "case {:?}: this fixture must hold at least two items for a token to resume at one, \
             got {whole:?}",
            reader.operation
        );
        let body = read(&router, &reader, "minting a token", Some(1), None).await;
        let token = body["nextToken"]
            .as_str()
            .unwrap_or_else(|| {
                panic!(
                    "case {:?}: a one-item page of a longer sequence must hand out a nextToken, \
                     got {body}",
                    reader.operation
                )
            })
            .to_string();

        for (case, selected) in [
            ("the same resources named explicitly", whole.clone()),
            ("one resource of the sequence", whole[..1].to_vec()),
        ] {
            let label = format!("{} resuming its token with {case}", reader.operation);
            let mut request = reader.request.clone();
            request["nextToken"] = json!(token);
            request[*member] = json!(selected);
            let (status, body) = call(&router, reader.operation, request.clone()).await;
            assert_eq!(
                status, 400,
                "case {label:?}: expected {request} to be refused with 400, got {status}: {body}"
            );
            assert_eq!(
                error_code(&body),
                "InvalidParameterValueException",
                "case {label:?}: expected InvalidParameterValueException, got {body}"
            );
        }
    }
}

/// The preference reader pages a SELECTED set of stored records too, and its selection is
/// `resourceType` plus the optional `scope` rather than a list of resource ARNs — so the same
/// rule holds there: a token minted for one selection of records is refused by another, never
/// applied to it. The control row is the selection the token WAS minted for, which resumes.
#[tokio::test]
async fn a_preference_token_resumes_only_the_records_it_was_minted_for() {
    let (router, _engine) = paged_emulator().await;
    let operation = "GetRecommendationPreferences";
    let selection = json!({"resourceType": "Ec2Instance"});

    let mut request = selection.clone();
    request["maxResults"] = json!(1);
    let (status, body) = call(&router, operation, request).await;
    assert_eq!(
        status, 200,
        "minting a token: expected 200, got {status}: {body}"
    );
    let token = body["nextToken"]
        .as_str()
        .unwrap_or_else(|| panic!("minting a token: expected a nextToken, got {body}"))
        .to_string();

    for (case, mut request) in [
        (
            "another resource type",
            json!({"resourceType": RESOURCE_TYPE_EBS_VOLUME}),
        ),
        (
            "a scope narrowing the records",
            json!({
                "resourceType": "Ec2Instance",
                "scope": {"name": SCOPE_ACCOUNT_ID, "value": ACCOUNT},
            }),
        ),
    ] {
        request["nextToken"] = json!(token);
        let (status, body) = call(&router, operation, request.clone()).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected {request} to be refused with 400, got {status}: {body}"
        );
        assert_eq!(
            error_code(&body),
            "InvalidParameterValueException",
            "case {case:?}: expected InvalidParameterValueException, got {body}"
        );
    }

    // The selection the token WAS minted for resumes the walk it stopped.
    let mut request = selection.clone();
    request["maxResults"] = json!(1);
    request["nextToken"] = json!(token);
    let (status, body) = call(&router, operation, request).await;
    assert_eq!(
        status, 200,
        "the selection the token was minted for: expected 200, got {status}: {body}"
    );
    assert_eq!(
        body["recommendationPreferencesDetails"]
            .as_array()
            .map(Vec::len),
        Some(1),
        "the selection the token was minted for: expected the next record, got {body}"
    );
}

/// One page of `reader`'s response, as the identities of the items it carries.
fn page_of(body: &Value, reader: &PagedReader) -> Vec<String> {
    body[reader.items]
        .as_array()
        .unwrap_or_else(|| {
            panic!(
                "case {:?}: expected a list of {}, got {body}",
                reader.operation, reader.items
            )
        })
        .iter()
        .map(|item| {
            let mut found = item;
            for member in reader.identity {
                found = &found[*member];
            }
            found
                .as_str()
                .unwrap_or_else(|| {
                    panic!(
                        "case {:?}: expected every item to carry {:?}, got {item}",
                        reader.operation, reader.identity
                    )
                })
                .to_string()
        })
        .collect()
}

/// Read one page from `reader`, asserting the call was accepted.
async fn read(
    router: &MultiServiceRouter,
    reader: &PagedReader,
    case: &str,
    max_results: Option<usize>,
    token: Option<&str>,
) -> Value {
    let mut request = reader.request.clone();
    if let Some(max_results) = max_results {
        request["maxResults"] = json!(max_results);
    }
    if let Some(token) = token {
        request["nextToken"] = json!(token);
    }
    let (status, body) = call(router, reader.operation, request.clone()).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected 200 from {request}, got {status}: {body}"
    );
    body
}

/// An emulator every reader in [`readers`] has at least two items to report from: the
/// account is enrolled, the organization holds member accounts, preference records are
/// written at four distinct scopes, export jobs exist for three resource types, two analysed
/// databases are recorded, the account's own Lambda functions are held with the utilization
/// published for them, and two Amazon ECS services have the analysis inputs a recommendation
/// is computed from.
async fn paged_emulator() -> (MultiServiceRouter, Arc<engine::InMemoryEngine>) {
    let (router, engine) = emulator_with_analysed_services_and_engine();
    register_analysed_functions(&router, &engine).await;
    opt_in(&router).await;
    // The analysis an enrollment starts is the gate every recommendation reader of this
    // service applies, so the fixture makes it due by moving the stored enrollment instant
    // back ([`make_analysis_due`]) rather than by waiting for the clock to reach it.
    make_analysis_due(&engine);
    record_organization(&engine);
    record_analysed_databases(&engine);
    write_preference_records(&router).await;
    create_bucket(&router, BUCKET).await;
    create_export_jobs(&router).await;
    analyse_ecs_services(&router, &engine).await;
    (router, engine)
}

/// Record member accounts for the organization, the way AWS Organizations would: no operation
/// of THIS API creates one, and `GetEnrollmentStatusesForOrganization` reports one row per
/// member account the store holds.
fn record_organization(engine: &Arc<engine::InMemoryEngine>) {
    let state = service_state(&**engine).expect("the engine bears this service's state");
    let mut guard = state.lock();
    for account_id in ["200000000001", "200000000002", "200000000003"] {
        guard
            .organization_members
            .insert(MemberAccountEnrollment::new(account_id));
    }
}

/// Record two analysed Amazon Aurora or RDS databases into the findings catalog, the way
/// Compute Optimizer's own analysis of an observed workload would: a page size is only
/// observable over a sequence of more than one.
fn record_analysed_databases(engine: &Arc<engine::InMemoryEngine>) {
    let state = service_state(&**engine).expect("the engine bears this service's state");
    let mut guard = state.lock();
    for identifier in ["db:ledger-primary", "db:ledger-replica"] {
        guard.analysed_resources.push(AnalysedResource {
            account_id: ::engine::emulator_account_id().to_string(),
            resource_type: SOURCE_TYPE_RDS_DB_INSTANCE.to_string(),
            // The catalog records a resource's REGION-FREE identity; the ARN reported for it
            // names the region of the endpoint that reports it.
            arn_service: "rds".to_string(),
            arn_resource: identifier.to_string(),
            // An `RDSInstanceFinding` value with nothing to change, so the row needs no
            // recommendation option of its own.
            finding: "Optimized".to_string(),
            finding_reason_codes: Vec::new(),
            current_performance_risk: "VeryLow".to_string(),
            current_monthly_cost: 120.0,
            recommended_monthly_cost: 120.0,
            inferred_workload_types: Vec::new(),
            rds_database: None,
        });
    }
}

/// Write one preference record at each of four distinct scopes, which is what
/// `GetRecommendationPreferences` reports one row per. The value written is the one the
/// store already presents, so no other reader's recommendation changes.
async fn write_preference_records(router: &MultiServiceRouter) {
    let scopes = [
        json!({"name": SCOPE_ACCOUNT_ID, "value": ACCOUNT}),
        json!({"name": SCOPE_ORGANIZATION, "value": "ALL_ACCOUNTS"}),
        json!({"name": SCOPE_RESOURCE_ARN, "value": instance_arn("i-0ab100000000000a1")}),
        json!({"name": SCOPE_RESOURCE_ARN, "value": instance_arn("i-0ab100000000000a2")}),
    ];
    for scope in scopes {
        put_preference(
            router,
            &json!({
                "resourceType": "Ec2Instance",
                "scope": scope,
                "enhancedInfrastructureMetrics": "Inactive",
            }),
        )
        .await;
    }
}

/// Create one export job per resource type, which is what `DescribeRecommendationExportJobs`
/// reports. The resource types differ because only one job per resource type may be in
/// progress in a Region at a time.
async fn create_export_jobs(router: &MultiServiceRouter) {
    for operation in [
        "ExportIdleRecommendations",
        "ExportEC2InstanceRecommendations",
        "ExportEBSVolumeRecommendations",
    ] {
        let (status, body) = call(
            router,
            operation,
            json!({"s3DestinationConfig": {"bucket": BUCKET}}),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {operation:?}: expected the export to be accepted, got {status}: {body}"
        );
    }
}

/// Give two Amazon ECS services the inputs a recommendation is computed from: the service the
/// ECS store holds, and the `AWS/ECS` utilization published for it.
async fn analyse_ecs_services(router: &MultiServiceRouter, engine: &Arc<engine::InMemoryEngine>) {
    let observed_at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("the clock is after the epoch")
        .as_secs_f64()
        - 3_600.0;
    ecs_ok(router, "CreateCluster", json!({"clusterName": CLUSTER})).await;
    for service in ["alpha", "bravo"] {
        let family = format!("{service}-family");
        ecs_ok(
            router,
            "RegisterTaskDefinition",
            json!({
                "family": family,
                "networkMode": "awsvpc",
                "requiresCompatibilities": ["FARGATE"],
                "cpu": "1024",
                "memory": "2048",
                "containerDefinitions": [{
                    "name": "web",
                    "image": "public.ecr.aws/nginx/nginx:latest",
                    "cpu": 128,
                    "memory": 256,
                    "memoryReservation": 192,
                }],
            }),
        )
        .await;
        ecs_ok(
            router,
            "CreateService",
            json!({
                "cluster": CLUSTER,
                "serviceName": service,
                "taskDefinition": family,
                "desiredCount": 1,
                "launchType": "FARGATE",
                // The revision above is in the `awsvpc` network mode, whose tasks each get
                // their own elastic network interface, so the launch says which subnets and
                // security groups that interface takes.
                "networkConfiguration": {
                    "awsvpcConfiguration": {
                        "subnets": ["subnet-0123456789abcdef0"],
                        "securityGroups": ["sg-0123456789abcdef0"],
                    }
                },
            }),
        )
        .await;
        // `AWS/ECS` is a namespace AWS reserves for the service that owns the metric, so the
        // utilization analysed here is seeded the way ECS writes it.
        for metric_name in ["CPUUtilization", "MemoryUtilization"] {
            seed_cloudwatch_series(
                engine,
                "AWS/ECS",
                metric_name,
                &[("ClusterName", CLUSTER), ("ServiceName", service)],
                Some("Percent"),
                observed_at,
                &[20.0, 30.0],
            );
        }
    }
}

/// One Amazon ECS call that must be accepted.
async fn ecs_ok(router: &MultiServiceRouter, operation: &str, body: Value) {
    let (status, parsed) = call_ecs(router, operation, body).await;
    assert_eq!(
        status, 200,
        "case {operation:?}: expected 200, got {status}: {parsed}"
    );
}
