// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the refusals AWS Batch answers a request that is OUTSIDE A
//! PUBLISHED BOUND with, over the wire.
//!
//! Every operation of this model declares exactly ONE client error (`ClientException`, HTTP
//! 400), so the message is the only machine-readable detail a refusal carries: a client
//! telling an over-limit bulk request, an out-of-range page size, an over-long describe list
//! or an over-sized job definition apart has nothing else to branch on. Each test below
//! asserts a RULE over a TABLE of inputs rather than one recorded request:
//!
//! * a BULK job operation refuses a `jobs` list past the documented per-request maximum,
//!   whole, with the one sentence that states that maximum — the same sentence for all three
//!   bulk operations, since the bound they publish is the same;
//! * each paginated listing holds `maxResults` to ITS OWN window and states that window in
//!   its own words, so three listings do not share one normalised sentence and the one whose
//!   window is wider accepts the page sizes the others refuse;
//! * a `Describe*` whose identifier list is documented "up to 100 … entries" refuses a
//!   longer list BEFORE any lookup, instead of answering a successful describe of nothing;
//! * `RegisterJobDefinition` refuses a definition past the published size limit, stating the
//!   limit and the definition's own size in whole kilobytes, and registers nothing;
//! * the consumable-resource surface states the RULE a bad `operation` or a negative
//!   quantity broke, in Batch's envelope, echoing nothing of the value.

use serde_json::{json, Value};

use service_com_amazonaws_batch_awsbatchv20160810::state;

use crate::job_lifecycle::{compute_environment, container_properties, error_code, error_message,
    job_queue, Batch};
use crate::refusal_contract::enveloped;

/// Assert one call was refused with the modelled `ClientException` (HTTP 400) carrying
/// `detail` inside Batch's frontend envelope, and answer the request id it stated.
async fn refused_detail(batch: &Batch, case: &str, uri: &str, body: Value, detail: &str) -> String {
    let (status, refused) = batch.call(uri, body).await;
    assert_eq!(
        status, 400,
        "case {case}: expected HTTP 400, got {status} {refused}"
    );
    assert_eq!(
        error_code(&refused),
        "ClientException",
        "case {case}: expected the modelled ClientException, got {refused}"
    );
    let message = error_message(&refused);
    let (stated, request_id) = enveloped(case, &message);
    assert_eq!(
        stated, detail,
        "case {case}: expected the detail {detail:?}, got {message:?}"
    );
    request_id
}

/// RULE: a bulk job operation refuses a `jobs` list longer than the documented maximum
/// (`state::limits::MAX_BULK_JOBS`) whole, with the enveloped sentence that states that
/// per-request maximum — and serves a list at the maximum.
///
/// The rows are the three bulk operations of this model (`smithy_query(query="doc",
/// shape="CancelJobs")` and its `TerminateJobs`/`TerminateServiceJobs` siblings, each
/// documenting "up to 50" ids), crossed with the list lengths: one id and exactly the
/// maximum are served, one past it and twice it are refused.
#[tokio::test]
async fn every_bulk_job_operation_refuses_a_list_past_the_documented_maximum() {
    let batch = Batch::new();
    let maximum = state::limits::MAX_BULK_JOBS;
    let detail = state::bulk_jobs_bound_message();
    assert!(
        detail.contains(&maximum.to_string()),
        "case the sentence: it must state the documented maximum {maximum}, got {detail:?}"
    );

    // (the bulk operation's URI, the label used in every assertion)
    let operations: &[(&str, &str)] = &[
        ("/v1/canceljobs", "CancelJobs"),
        ("/v1/terminatejobs", "TerminateJobs"),
        ("/v1/terminateservicejobs", "TerminateServiceJobs"),
    ];
    let mut stated_ids: Vec<String> = Vec::new();
    for (uri, label) in operations {
        // Ids this service never minted, so what decides the answer is the COUNT alone.
        let ids = |count: usize| -> Vec<String> {
            (0..count)
                .map(|index| format!("00000000-0000-4000-8000-{index:012}"))
                .collect()
        };

        for count in [1, maximum] {
            let case = format!("{label} naming {count} ids");
            let body = batch
                .ok(uri, json!({"jobs": ids(count), "reason": "bounded"}))
                .await;
            assert_eq!(
                body["successful"].as_array().map(Vec::len),
                Some(count),
                "case {case}: every id a served bulk call named is reported successful, got \
                 {body}"
            );
        }

        for count in [maximum + 1, maximum * 2] {
            let case = format!("{label} naming {count} ids");
            stated_ids.push(
                refused_detail(
                    &batch,
                    &case,
                    uri,
                    json!({"jobs": ids(count), "reason": "bounded"}),
                    &detail,
                )
                .await,
            );
        }
    }

    // The envelope names the CALL being refused, so no two of those refusals state one id.
    let mut distinct = stated_ids.clone();
    distinct.sort();
    distinct.dedup();
    assert_eq!(
        distinct.len(),
        stated_ids.len(),
        "case the refused bulk calls: expected one request id per call, got {stated_ids:?}"
    );
}

/// One paginated listing: the window it accepts and the sentence it states when a request
/// misses it.
struct PageWindow {
    label: &'static str,
    uri: &'static str,
    /// The members the listing needs beside `maxResults`.
    required: Value,
    minimum: i32,
    maximum: i32,
    detail: String,
}

/// RULE: each paginated listing holds `maxResults` to its OWN window and refuses a value
/// outside it with its OWN enveloped sentence — so a listing whose window is wider serves
/// the page sizes a narrower one refuses.
///
/// The rows are the three listings whose bound wording is recorded, and each is driven at
/// every edge of its window: one below the minimum, a negative value, the minimum, the
/// maximum, and one past the maximum.
#[tokio::test]
async fn each_listing_states_its_own_page_size_window() {
    let batch = Batch::new();
    batch
        .ok(
            "/v1/createconsumableresource",
            json!({"consumableResourceName": "cr-window", "totalQuantity": 1}),
        )
        .await;

    let windows: Vec<PageWindow> = vec![
        PageWindow {
            label: "ListSchedulingPolicies",
            uri: "/v1/listschedulingpolicies",
            required: json!({}),
            minimum: state::MIN_LIST_SCHEDULING_POLICIES_RESULTS,
            maximum: state::MAX_LIST_SCHEDULING_POLICIES_RESULTS,
            detail: state::list_scheduling_policies_max_results_message(),
        },
        PageWindow {
            label: "ListConsumableResources",
            uri: "/v1/listconsumableresources",
            required: json!({}),
            minimum: state::MIN_LIST_CONSUMABLE_RESOURCES_RESULTS,
            maximum: state::MAX_LIST_CONSUMABLE_RESOURCES_RESULTS,
            detail: state::list_consumable_resources_max_results_message(),
        },
        PageWindow {
            label: "ListJobsByConsumableResource",
            uri: "/v1/listjobsbyconsumableresource",
            required: json!({"consumableResource": "cr-window"}),
            minimum: state::MIN_LIST_JOBS_BY_CONSUMABLE_RESOURCE_RESULTS,
            maximum: state::MAX_LIST_JOBS_BY_CONSUMABLE_RESOURCE_RESULTS,
            detail: state::list_jobs_by_consumable_resource_max_results_message(),
        },
    ];

    // Each sentence states its own operation's ceiling, and no two of them are one
    // sentence: that is what a caller reading the window off a refusal depends on.
    for window in &windows {
        assert!(
            window.detail.contains(&window.maximum.to_string()),
            "case {}: its sentence must state its own maximum {}, got {:?}",
            window.label,
            window.maximum,
            window.detail
        );
    }
    let mut sentences: Vec<&str> = windows.iter().map(|w| w.detail.as_str()).collect();
    sentences.sort_unstable();
    sentences.dedup();
    assert_eq!(
        sentences.len(),
        windows.len(),
        "case the recorded sentences: each listing words its own bound, got {sentences:?}"
    );

    for window in &windows {
        for requested in [window.minimum - 1, -1, 0, window.maximum + 1] {
            let case = format!("{} with maxResults {requested}", window.label);
            let mut body = window.required.clone();
            body["maxResults"] = json!(requested);
            refused_detail(&batch, &case, window.uri, body, &window.detail).await;
        }
        for requested in [window.minimum, window.maximum] {
            let case = format!("{} with maxResults {requested}", window.label);
            let mut body = window.required.clone();
            body["maxResults"] = json!(requested);
            let (status, answered) = batch.call(window.uri, body).await;
            assert_eq!(
                status, 200,
                "case {case}: a page size inside the window is served, got {status} {answered}"
            );
        }
    }

    // The widest window is wider than the others: the page sizes between their ceiling and
    // its own are SERVED there, which is what a paginator written against the service's own
    // maximum needs.
    let widest = state::MAX_LIST_JOBS_BY_CONSUMABLE_RESOURCE_RESULTS;
    for requested in [
        state::MAX_LIST_CONSUMABLE_RESOURCES_RESULTS + 1,
        state::DEFAULT_LIST_JOBS_BY_CONSUMABLE_RESOURCE_RESULTS + 1,
        widest - 1,
        widest,
    ] {
        let case = format!("ListJobsByConsumableResource with maxResults {requested}");
        let (status, answered) = batch
            .call(
                "/v1/listjobsbyconsumableresource",
                json!({"consumableResource": "cr-window", "maxResults": requested}),
            )
            .await;
        assert_eq!(
            status, 200,
            "case {case}: a page size inside this operation's own window is served, got \
             {status} {answered}"
        );
    }
}

/// RULE: a `Describe*` whose identifier list is documented "a list of up to 100 … entries"
/// refuses a longer list before any lookup, with the enveloped sentence stating the window
/// the member admits — while a list AT the maximum, a list naming nothing and an absent
/// member are all served.
///
/// The rows are the two describes of this model whose list the member's doc bounds at 100
/// (`smithy_query(query="doc", shape="DescribeComputeEnvironmentsRequest",
/// member="computeEnvironments")` and the `DescribeJobQueuesRequest$jobQueues` twin), each
/// driven at the maximum, one past it and well past it.
#[tokio::test]
async fn a_describe_refuses_an_identifier_list_past_the_documented_maximum() {
    let batch = Batch::new();
    let environment = compute_environment(&batch, "ce-bounded").await;
    job_queue(&batch, "q-bounded", &environment).await;
    let maximum = state::MAX_DESCRIBE_FILTER_ENTRIES;

    // (the describe's URI, its list member, the output member it answers in, the sentence)
    let describes: Vec<(&str, &str, &str, String)> = vec![
        (
            "/v1/describecomputeenvironments",
            "computeEnvironments",
            "computeEnvironments",
            state::compute_environments_bound_message(),
        ),
        (
            "/v1/describejobqueues",
            "jobQueues",
            "jobQueues",
            state::job_queues_bound_message(),
        ),
    ];

    for (uri, member, answered_in, detail) in describes {
        let names = |count: usize| -> Vec<String> {
            (0..count).map(|index| format!("absent-{index:04}")).collect()
        };

        // At the maximum, and below it: a list of names nothing answers to is a SUCCESSFUL
        // describe of nothing, which is exactly why a longer list must not be.
        for count in [1, maximum - 1, maximum] {
            let case = format!("{uri} naming {count} absent entries");
            let body = batch.ok(uri, json!({member: names(count)})).await;
            assert_eq!(
                body[answered_in].as_array().map(Vec::len),
                Some(0),
                "case {case}: expected an empty {answered_in}, got {body}"
            );
        }

        // Naming nothing is this member's "every one of them", not a request past a bound.
        for body in [json!({}), json!({member: Vec::<String>::new()})] {
            let case = format!("{uri} naming no entry at all ({body})");
            let answered = batch.ok(uri, body).await;
            assert_eq!(
                answered[answered_in].as_array().map(Vec::len),
                Some(1),
                "case {case}: expected the one stored resource, got {answered}"
            );
        }

        for count in [maximum + 1, maximum * 2] {
            let case = format!("{uri} naming {count} entries");
            refused_detail(&batch, &case, uri, json!({member: names(count)}), &detail).await;
        }
    }
}

/// A registration whose compact JSON body is EXACTLY `target` bytes: the padding sits in one
/// environment variable's value, so the definition's size is what varies and nothing else.
///
/// Answers the body and its own byte length, which is the length the service measures the
/// definition at (both are the compact JSON document the request amounts to).
fn sized_registration(name: &str, target: usize) -> (Value, usize) {
    let build = |padding: usize| -> Value {
        let mut properties = container_properties();
        properties["environment"] = json!([{"name": "PADDING", "value": "A".repeat(padding)}]);
        json!({
            "jobDefinitionName": name,
            "type": "container",
            "containerProperties": properties,
        })
    };
    let overhead = serde_json::to_string(&build(0))
        .expect("the registration serializes")
        .len();
    assert!(
        target >= overhead,
        "case a {target}-byte registration: the smallest one is {overhead} bytes"
    );
    let body = build(target - overhead);
    let length = serde_json::to_string(&body)
        .expect("the registration serializes")
        .len();
    assert_eq!(
        length, target,
        "case a {target}-byte registration: expected to build exactly {target} bytes"
    );
    (body, length)
}

/// RULE: `RegisterJobDefinition` refuses a definition whose serialized size reaches the
/// published limit (`state::MAX_JOB_DEFINITION_BYTES`), stating the limit and the
/// definition's own size in whole kilobytes, and registers nothing — while the same shape
/// under the limit registers and reads back.
///
/// The rows walk the size axis: well under the limit, just under it, exactly at it, and far
/// past it. The sizes are built rather than recorded, so the assertion is on the rule and
/// not on one request's length; each refused row is built at a whole number of kilobytes, so
/// the kilobytes the refusal states are the kilobytes the request carried whatever the few
/// bytes the protocol's own rendering of the shape adds to it.
#[tokio::test]
async fn register_job_definition_refuses_a_definition_past_the_published_size_limit() {
    let batch = Batch::new();
    let limit = state::MAX_JOB_DEFINITION_BYTES;

    // (case, the name to register under, the size to build, whether it is accepted)
    let rows: Vec<(&str, &str, usize, bool)> = vec![
        ("a definition well under the limit", "jd-small", 1_024, true),
        ("a definition just under the limit", "jd-edge", limit - 256, true),
        ("a definition exactly at the limit", "jd-at-limit", limit, false),
        ("a definition far past the limit", "jd-oversized", 40 * 1_024, false),
    ];

    for (case, name, target, accepted) in rows {
        let (body, bytes) = sized_registration(name, target);
        if accepted {
            let registered = batch.ok("/v1/registerjobdefinition", body).await;
            assert_eq!(
                registered["revision"].as_i64(),
                Some(1),
                "case {case}: expected revision 1, got {registered}"
            );
            let described = batch
                .ok("/v1/describejobdefinitions", json!({"jobDefinitionName": name}))
                .await;
            assert_eq!(
                described["jobDefinitions"].as_array().map(Vec::len),
                Some(1),
                "case {case}: the registered definition reads back, got {described}"
            );
            continue;
        }

        refused_detail(
            &batch,
            case,
            "/v1/registerjobdefinition",
            body,
            &state::job_definition_size_message(bytes),
        )
        .await;

        // Nothing was registered: the describe answers an empty list rather than a
        // revision 1 a later submit could run.
        let described = batch
            .ok("/v1/describejobdefinitions", json!({"jobDefinitionName": name}))
            .await;
        assert_eq!(
            described["jobDefinitions"].as_array().map(Vec::len),
            Some(0),
            "case {case}: a refused registration stores nothing, got {described}"
        );
    }
}

/// RULE: the consumable-resource surface refuses a request VALUE by stating the rule it
/// broke — the closed set an `operation` must come from, named as AWS names that member, and
/// the non-negative rule a quantity must satisfy — inside Batch's envelope, echoing nothing
/// of the value, and changes no stored total.
///
/// The rows vary HOW the value misses the rule (an unknown operation, a documented one in
/// the wrong case, an empty one; a quantity one past the bound, the smallest `Long`, a
/// negative quantity with no operation at all) and WHICH member carries the quantity (the
/// update's `quantity` and the create's `totalQuantity`).
#[tokio::test]
async fn a_consumable_resource_value_refusal_states_the_rule_it_broke() {
    let batch = Batch::new();
    batch
        .ok(
            "/v1/createconsumableresource",
            json!({"consumableResourceName": "cr-rules", "totalQuantity": 10}),
        )
        .await;

    let operation_detail = state::closed_set_detail(
        "Update operation",
        &state::CONSUMABLE_RESOURCE_UPDATE_OPERATIONS,
    );
    // (case, the URI, the request, the detail it must state, the text it must NOT echo)
    let rows: Vec<(&str, &str, Value, &str, &str)> = vec![
        (
            "an operation outside the documented set",
            "/v1/updateconsumableresource",
            json!({"consumableResource": "cr-rules", "operation": "MULTIPLY", "quantity": 5}),
            &operation_detail,
            "MULTIPLY",
        ),
        (
            "a documented operation in the wrong case",
            "/v1/updateconsumableresource",
            json!({"consumableResource": "cr-rules", "operation": "add", "quantity": 5}),
            &operation_detail,
            "add ",
        ),
        (
            "an empty operation",
            "/v1/updateconsumableresource",
            json!({"consumableResource": "cr-rules", "operation": "", "quantity": 5}),
            &operation_detail,
            "cr-rules",
        ),
        (
            "a quantity one past its lower bound",
            "/v1/updateconsumableresource",
            json!({"consumableResource": "cr-rules", "operation": "ADD", "quantity": -1}),
            state::NEGATIVE_QUANTITY,
            "-1",
        ),
        (
            "a quantity at the smallest Long",
            "/v1/updateconsumableresource",
            json!({"consumableResource": "cr-rules", "operation": "SET", "quantity": i64::MIN}),
            state::NEGATIVE_QUANTITY,
            "9223372036854775808",
        ),
        (
            "a negative quantity with no operation",
            "/v1/updateconsumableresource",
            json!({"consumableResource": "cr-rules", "quantity": -5}),
            state::NEGATIVE_QUANTITY,
            "-5",
        ),
        (
            "a create whose totalQuantity is negative",
            "/v1/createconsumableresource",
            json!({"consumableResourceName": "cr-negative", "totalQuantity": -1}),
            state::NEGATIVE_QUANTITY,
            "-1",
        ),
    ];

    for (case, uri, body, detail, not_echoed) in rows {
        refused_detail(&batch, case, uri, body, detail).await;
        assert!(
            !detail.contains(not_echoed),
            "case {case}: the refusal must not echo {not_echoed:?}, got {detail:?}"
        );
    }

    // Every refusal above left the stored total alone, and the create stored nothing.
    let described = batch
        .ok(
            "/v1/describeconsumableresource",
            json!({"consumableResource": "cr-rules"}),
        )
        .await;
    assert_eq!(
        described["totalQuantity"].as_i64(),
        Some(10),
        "case the stored resource: a refused request changes no total, got {described}"
    );
    let (status, missing) = batch
        .call(
            "/v1/describeconsumableresource",
            json!({"consumableResource": "cr-negative"}),
        )
        .await;
    assert_eq!(
        status, 400,
        "case the refused create: nothing was stored under its name, got {status} {missing}"
    );
}
