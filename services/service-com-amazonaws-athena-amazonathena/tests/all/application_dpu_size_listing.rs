// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for `ListApplicationDPUSizes`: the application runtimes Athena
//! publishes and the data processing unit sizes each one supports, driven over the wire
//! (awsJson1_1, `x-amz-target: AmazonAthena.<Operation>`) through the same router the
//! emulator boots, so every assertion is about what an SDK client observes.
//!
//! THE RULE UNDER TEST: the listing reports the service's catalog of application runtimes —
//! one entry per runtime Athena runs applications on, each naming that runtime and the DPU
//! sizes a unit of its compute may be asked for — in pages of at most `MaxResults` entries.
//! The catalog belongs to the service, not to the account: no request adds to it or takes
//! from it, so two listings report the same entries, and every size it publishes is one a
//! session of that runtime can be started with while every size outside it is refused. The
//! catalog holds fewer entries than the smallest page the model admits, so no page of it is
//! truncated: the listing mints no `NextToken`, and a value a caller sends names no position
//! to continue from, which is answered with the catalog rather than refused.
//!
//! Each rule is asserted over a TABLE rather than one request: the rows differ along the
//! page size asked for (both published bounds of the modelled range, one past each of them,
//! a size at the catalog and one past it, and no size at all), the form of a continuation
//! token a caller sends back, and the DPU size a session start names.
//!
//! The runtime id and the sizes asserted below are the model's own, read with
//! `smithy_query(query="operation",operation="ListApplicationDPUSizes")`,
//! `smithy_query(query="shape",shape="ApplicationDPUSizes,EngineConfiguration")` and
//! `smithy_query(query="doc",shape="StartSessionRequest",member="NotebookVersion")`.

use serde_json::{json, Value};
use service_com_amazonaws_athena_amazonathena::state::{
    ATHENA_NOTEBOOK_RUNTIME_ID, MAX_RUNTIME_DPU_SIZE, MIN_RUNTIME_DPU_SIZE, PYSPARK_ENGINE_VERSION,
};
use service_common::page_tokens::AUTHENTICATOR_BYTES;
use smithy_server_runtime::MultiServiceRouter;

use crate::harness::{call, error_code, router};

/// The bounds of the modelled `MaxApplicationDPUSizesCount` range (1..100). The maximum is
/// also the page a request naming no size gets.
const SMALLEST_PAGE: i64 = 1;
const LARGEST_PAGE: i64 = 100;

/// The bounds of the `@length` the model carries on
/// `ApplicationDPUSizes$ApplicationRuntimeId` (`NameString`, 1..128).
const SHORTEST_RUNTIME_ID: usize = 1;
const LONGEST_RUNTIME_ID: usize = 128;

/// The longest value the modelled `Token` (@length 1..1024) admits, which a caller may send
/// as a `NextToken`.
const LONGEST_TOKEN: usize = 1024;

/// The Spark enabled workgroup a session that names a DPU size runs in.
const WORK_GROUP: &str = "application-dpu-sizes";

/// The entries a `ListApplicationDPUSizes` answer carries.
fn entries(listed: &Value) -> Vec<Value> {
    listed["ApplicationDPUSizes"]
        .as_array()
        .cloned()
        .unwrap_or_else(|| panic!("a listing must carry ApplicationDPUSizes, got {listed:?}"))
}

/// The `ApplicationRuntimeId` of one entry.
fn runtime_id(entry: &Value) -> String {
    entry["ApplicationRuntimeId"]
        .as_str()
        .unwrap_or_else(|| panic!("every entry names its application runtime, got {entry:?}"))
        .to_string()
}

/// The `SupportedDPUSizes` of one entry.
fn sizes(entry: &Value) -> Vec<i64> {
    entry["SupportedDPUSizes"]
        .as_array()
        .unwrap_or_else(|| panic!("every entry names the sizes it supports, got {entry:?}"))
        .iter()
        .map(|size| {
            size.as_i64()
                .unwrap_or_else(|| panic!("a supported DPU size is an integer, got {entry:?}"))
        })
        .collect()
}

/// The runtimes a listing reported, in the order it reported them.
fn runtimes(listed: &Value) -> Vec<String> {
    entries(listed).iter().map(runtime_id).collect()
}

/// The `NextToken` a page handed back, if it handed one back.
fn next_token(listed: &Value) -> Option<String> {
    listed["NextToken"].as_str().map(str::to_string)
}

/// One `ListApplicationDPUSizes` call that must be accepted.
async fn list(router: &MultiServiceRouter, request: &Value) -> Value {
    let (status, listed) = call(router, "ListApplicationDPUSizes", request).await;
    assert_eq!(
        status, 200,
        "ListApplicationDPUSizes({request}) must answer 200, got {status} with {listed:?}"
    );
    listed
}

/// Create the Spark enabled workgroup a session runs in, with the result configuration a
/// session's working directory is taken from.
async fn given_spark_work_group(router: &MultiServiceRouter, name: &str) {
    let (status, created) = call(
        router,
        "CreateWorkGroup",
        &json!({
            "Name": name,
            "Configuration": {
                "EngineVersion": { "SelectedEngineVersion": PYSPARK_ENGINE_VERSION },
                "ExecutionRole": "arn:aws:iam::000000000000:role/AthenaSparkExecution",
                "ResultConfiguration": { "OutputLocation": "s3://athena-results/sessions/" },
            },
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "the Spark workgroup {name} must be created, got {status} with {created:?}"
    );
}

/// The whole catalog in one call: every application runtime, each with the DPU sizes it
/// supports, and nothing a caller has to page for.
#[tokio::test]
async fn the_listing_publishes_every_application_runtime_with_its_supported_dpu_sizes() {
    let router = router();
    let listed = list(&router, &json!({})).await;

    let published = runtimes(&listed);
    assert!(
        !published.is_empty(),
        "the catalog names at least the runtime Athena runs notebooks on, got {listed:?}"
    );
    assert_eq!(
        next_token(&listed),
        None,
        "a listing that reported everything hands back no token, got {listed:?}"
    );

    // The runtime the model names: "The only valid notebook version is `Athena notebook
    // version 1`" (`StartSessionRequest$NotebookVersion`), which is also the example
    // `ListApplicationDPUSizes` gives for an application runtime.
    assert_eq!(
        published,
        vec![ATHENA_NOTEBOOK_RUNTIME_ID.to_string()],
        "expected the catalog of application runtimes, got {published:?}"
    );

    for entry in entries(&listed) {
        let case = runtime_id(&entry);
        assert!(
            (SHORTEST_RUNTIME_ID..=LONGEST_RUNTIME_ID).contains(&case.chars().count()),
            "case {case:?}: expected a runtime id of the modelled length \
             ({SHORTEST_RUNTIME_ID}..{LONGEST_RUNTIME_ID}), got {} characters",
            case.chars().count()
        );

        // Every published size is one the model admits for a unit of a runtime's compute:
        // the `@range` on `CoordinatorDpuSize` and `DefaultExecutorDpuSize`.
        let supported = sizes(&entry);
        assert!(
            !supported.is_empty(),
            "case {case:?}: a runtime supports at least one DPU size, got {entry:?}"
        );
        let mut ordered = supported.clone();
        ordered.sort_unstable();
        ordered.dedup();
        assert_eq!(
            ordered, supported,
            "case {case:?}: expected the supported sizes in ascending order without repeats, \
             got {supported:?}"
        );
        for size in &supported {
            assert!(
                (i64::from(MIN_RUNTIME_DPU_SIZE)..=i64::from(MAX_RUNTIME_DPU_SIZE))
                    .contains(size),
                "case {case:?}: expected a size the model admits \
                 ({MIN_RUNTIME_DPU_SIZE}..{MAX_RUNTIME_DPU_SIZE}), got {size} among \
                 {supported:?}"
            );
        }
    }

    // A READ, of a catalog no request owns: a second listing sees exactly what the first one
    // did, and so does one made after the account has been written to.
    let again = list(&router, &json!({})).await;
    assert_eq!(
        again, listed,
        "two listings must publish the same runtimes, got {again:?} after {listed:?}"
    );
    crate::harness::create_work_group(&router, "application-dpu-suite").await;
    let after_a_write = list(&router, &json!({})).await;
    assert_eq!(
        after_a_write, listed,
        "the catalog is the service's own, so a write to the account leaves it alone, got \
         {after_a_write:?} after {listed:?}"
    );
}

/// A paged walk reports every published runtime exactly once, in pages of at most the size
/// asked for, and hands back a token exactly while runtimes remain behind the page.
#[tokio::test]
async fn a_paged_listing_walks_every_application_runtime_exactly_once() {
    let router = router();
    let expected: Vec<String> = runtimes(&list(&router, &json!({})).await);
    let total = expected.len();

    // Each row is a page size: both published bounds of `MaxApplicationDPUSizesCount`, a
    // size at the catalog and one past it, and no size at all — the page a request naming
    // none gets.
    let catalog = i64::try_from(total).expect("the catalog fits in an i64");
    let sizes: Vec<Option<i64>> = [SMALLEST_PAGE, catalog, catalog + 1, LARGEST_PAGE]
        .into_iter()
        .map(Some)
        .chain([None])
        .collect();
    for case in sizes {
        let mut walked: Vec<String> = Vec::new();
        let mut token: Option<String> = None;
        let mut pages = 0_usize;
        loop {
            let mut request = json!({});
            if let Some(size) = case {
                request["MaxResults"] = json!(size);
            }
            if let Some(token) = token.as_deref() {
                request["NextToken"] = json!(token);
            }
            let listed = list(&router, &request).await;
            pages += 1;
            assert!(
                pages <= total + 1,
                "case {case:?}: a token walk must terminate, walked {walked:?}"
            );

            let page = runtimes(&listed);
            let page_size = usize::try_from(case.unwrap_or(LARGEST_PAGE)).unwrap_or(0);
            let remaining = total - walked.len();
            assert_eq!(
                page.len(),
                page_size.min(remaining),
                "case {case:?}: expected a page of {} after {walked:?}, got {page:?}",
                page_size.min(remaining)
            );
            walked.extend(page);

            token = next_token(&listed);
            let more = walked.len() < total;
            assert_eq!(
                token.is_some(),
                more,
                "case {case:?}: expected a NextToken to be present={more} after {walked:?}, got \
                 {listed:?}"
            );
            if token.is_none() {
                break;
            }
        }

        assert_eq!(
            walked, expected,
            "case {case:?}: expected the walk to report every runtime once, in catalog order: \
             {expected:?}, got {walked:?}"
        );
        let page_size = usize::try_from(case.unwrap_or(LARGEST_PAGE)).unwrap_or(0);
        assert_eq!(
            pages,
            total.div_ceil(page_size),
            "case {case:?}: expected the walk to take {} pages, took {pages}",
            total.div_ceil(page_size)
        );
    }

    // One step past each published bound of the range is outside it, so the request is
    // refused rather than answered with a page.
    for case in [SMALLEST_PAGE - 1, LARGEST_PAGE + 1] {
        let (status, refused) = call(
            &router,
            "ListApplicationDPUSizes",
            &json!({ "MaxResults": case }),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: a page size outside the modelled range is refused, got {status} \
             with {refused:?}"
        );
        assert_eq!(
            error_code(&refused),
            "InvalidRequestException",
            "case {case:?}: expected InvalidRequestException, got {refused:?}"
        );
    }
}

/// A `NextToken` is "a token generated by the Athena service that specifies where to
/// continue pagination if a previous request was truncated": this listing truncates no page,
/// so it mints no token, and a value a caller sends names no position to continue from — the
/// catalog is answered rather than the request refused.
///
/// This is deliberately NOT the refusal the account-state listings of this service state for
/// a token they did not mint (`tests/all/engine_version_listing.rs` and the workgroup,
/// catalog, session and named-query suites assert that refusal): those page over registries
/// a request writes, so a token names a position one of their pages really had. This listing
/// has only ever one page, and AWS serves the call — the recorded unit for it
/// (`athena.list-application-dpu-sizes-malformed-token-rejected`) passes only when the call
/// is answered, and failed while the emulator refused it with either of Athena's two
/// token-refusal sentences.
#[tokio::test]
async fn a_next_token_this_listing_never_minted_names_no_position() {
    let account = router();
    let whole = list(&account, &json!({})).await;

    // A token THIS store minted for a DIFFERENT listing: the account's workgroups, which
    // resume at a position in another sequence entirely.
    let work_groups_token = {
        for index in 0..2 {
            crate::harness::create_work_group(&account, &format!("token-case-{index}")).await;
        }
        let (status, listed) = call(&account, "ListWorkGroups", &json!({ "MaxResults": 1 })).await;
        assert_eq!(
            status, 200,
            "the workgroup listing must be readable, got {status} with {listed:?}"
        );
        next_token(&listed).expect("a page of one of three workgroups hands back a token")
    };

    // (case, the token sent)
    for (case, token) in [
        (
            "a value that is not a token at all",
            "made-up-token".to_string(),
        ),
        ("an empty-looking hex value", "00".to_string()),
        (
            "the shape of a minted token with an authenticator nobody computed",
            ::hex::encode(vec![0_u8; AUTHENTICATOR_BYTES + 16]),
        ),
        (
            "a token this store minted for another listing",
            work_groups_token,
        ),
        (
            "the longest value the modelled Token length admits",
            "t".repeat(LONGEST_TOKEN),
        ),
    ] {
        let listed = list(
            &account,
            &json!({ "MaxResults": SMALLEST_PAGE, "NextToken": token }),
        )
        .await;
        assert_eq!(
            runtimes(&listed),
            runtimes(&whole),
            "case {case:?}: expected the catalog, got {listed:?}"
        );
        assert_eq!(
            next_token(&listed),
            None,
            "case {case:?}: a listing that reported everything hands back no token, got \
             {listed:?}"
        );
    }

    // Nothing those calls carried changed what the next caller sees.
    let after = list(&account, &json!({})).await;
    assert_eq!(
        after, whole,
        "expected the whole catalog afterwards, got {after:?} after {whole:?}"
    );
}

/// Every size the listing publishes is one a session of that runtime can be started with,
/// and every size outside the published set is refused — read back through `GetSession`, so
/// the claim is about persisted state and not about the start call's own answer.
#[tokio::test]
async fn every_published_dpu_size_is_one_a_session_can_be_started_with() {
    let router = router();
    given_spark_work_group(&router, WORK_GROUP).await;

    let published: Vec<i64> = entries(&list(&router, &json!({})).await)
        .iter()
        .flat_map(sizes)
        .collect();

    for case in &published {
        let request = json!({
            "WorkGroup": WORK_GROUP,
            "EngineConfiguration": {
                "CoordinatorDpuSize": case,
                "DefaultExecutorDpuSize": case,
            },
        });
        let (status, started) = call(&router, "StartSession", &request).await;
        assert_eq!(
            status, 200,
            "case {case:?}: a start naming a published DPU size must be accepted, got {status} \
             with {started:?}"
        );
        let session_id = started["SessionId"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: a start answers with a SessionId, got {started:?}"))
            .to_string();

        let (status, found) = call(&router, "GetSession", &json!({ "SessionId": session_id })).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the started session must read back, got {status} with {found:?}"
        );
        let configured = &found["EngineConfiguration"];
        assert_eq!(
            configured["CoordinatorDpuSize"].as_i64(),
            Some(*case),
            "case {case:?}: expected the session to report the coordinator size it was started \
             with, got {found:?}"
        );
        assert_eq!(
            configured["DefaultExecutorDpuSize"].as_i64(),
            Some(*case),
            "case {case:?}: expected the session to report the executor size it was started \
             with, got {found:?}"
        );
    }

    // One step outside the published set at each end: a size no runtime supports starts no
    // session.
    let smallest = *published.iter().min().expect("the catalog publishes a size");
    let largest = *published.iter().max().expect("the catalog publishes a size");
    for case in [smallest - 1, largest + 1] {
        let request = json!({
            "WorkGroup": WORK_GROUP,
            "EngineConfiguration": { "CoordinatorDpuSize": case },
        });
        let (status, refused) = call(&router, "StartSession", &request).await;
        assert_eq!(
            status, 400,
            "case {case:?}: a size no runtime publishes must be refused, got {status} with \
             {refused:?}"
        );
        assert_eq!(
            error_code(&refused),
            "InvalidRequestException",
            "case {case:?}: expected InvalidRequestException, got {refused:?}"
        );
    }
}
