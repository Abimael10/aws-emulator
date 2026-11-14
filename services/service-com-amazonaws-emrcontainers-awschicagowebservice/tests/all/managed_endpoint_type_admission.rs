// SPDX-License-Identifier: CC0-1.0

//! Which `EndpointType` values `CreateManagedEndpoint` provisions a gateway for.
//!
//! Every call goes through the production path: the service is mounted in the real
//! `MultiServiceRouter` by the harness [`crate::virtual_cluster_job_run_loop::Emr`] and
//! driven with restJson1 HTTP requests, so routing, deserialization, constraint validation,
//! the handler and serialization all run exactly as they do for an AWS SDK client. The
//! create, describe and list helpers are the ones [`crate::managed_endpoint_listing`]
//! drives, so this suite carries no request path of its own.
//!
//! The rule under test, asserted over a TABLE of values rather than one request: `type` is
//! modelled as a plain string (`smithy_query(query="operation",
//! operation="CreateManagedEndpoint")`: `EndpointType`, len 1..64, pattern `\S`, with no
//! enumeration), so the values it may carry are the SERVICE's own — a value the model admits
//! but the service provisions no gateway for is refused with the modelled
//! `ValidationException` (HTTP 400) carrying one fixed sentence that names the MEMBER and
//! nothing of the value, and no endpoint is created; the type the service does serve is
//! accepted and reads back out of the stored record.
//!
//! The rows differ along what the rule generalises over: how the value differs from a served
//! one (case, surrounding space, a prefix, a suffix, an unrelated word), and its length —
//! including a value at the published 64-character maximum, which the model admits.

use std::collections::BTreeSet;

use serde_json::{json, Value};

use crate::managed_endpoint_listing::{
    create_endpoint, describe_endpoint, described, endpoint_body, list_endpoints, listed_ids,
};
use crate::virtual_cluster_job_run_loop::{field, text, Emr};

/// AWS's sentence for a member whose value is outside the set the service accepts for it,
/// written here as the service emits it: FIXED text naming the member, with neither the
/// value that was rejected nor the values that would have been accepted in it.
const INVALID_TYPE: &str = "Invalid value for argument type";

/// The `EndpointType` a managed endpoint's gateway is provisioned for: the Jupyter
/// Enterprise Gateway an EMR on EKS managed endpoint is, which is the value AWS documents
/// the create with.
const SERVED_TYPE: &str = "JUPYTER_ENTERPRISE_GATEWAY";

/// The ids a cluster's endpoint listing reports, as a set.
async fn stored_ids(emr: &Emr, cluster: &str, case: &str) -> BTreeSet<String> {
    let answer = list_endpoints(emr, cluster, "").await;
    let body = answer.ok(case).clone();
    listed_ids(&body, case).into_iter().collect()
}

/// The `message` a refused call carries.
fn refusal_message(body: &Value, case: &str) -> String {
    text(body, "/message", case)
}

/// One `CreateManagedEndpoint` body carrying `endpoint_type`, differing from an accepted
/// request in exactly that member.
fn body_with_type(token: &str, endpoint_type: &str) -> Value {
    let mut body = endpoint_body("type-row", token, SERVED_TYPE);
    body.as_object_mut()
        .expect("the fixture body is a JSON object")
        .insert("type".to_string(), json!(endpoint_type));
    body
}

/// THE RULE: a `type` the service provisions no gateway for is refused, and nothing is
/// created for it.
#[tokio::test]
async fn a_managed_endpoint_type_the_service_does_not_serve_is_refused() {
    let emr = Emr::new();
    let cluster = emr.cluster("endpoint-types", "vc-ep-types").await;

    // The endpoint the cluster holds before any refusal, so the listing at the end shows
    // whether a refused create stored anything.
    let case = "the endpoint stored before the refusals";
    let created = create_endpoint(
        &emr,
        &cluster,
        endpoint_body("served", "ep-tok-served", SERVED_TYPE),
    )
    .await;
    let stored = text(created.ok(case), "/id", case);

    // Every value here is inside what the MODEL admits for the member (len 1..64, pattern
    // `\S` — it only has to carry a non-space character), so each one reaches the handler
    // and is refused by the service rather than by the constraint walk.
    let rows: Vec<(&str, String)> = vec![
        (
            "an unrelated word",
            "NOT_A_REAL_ENDPOINT_TYPE".to_string(),
        ),
        (
            "a plausible gateway that is not the served one",
            "JUPYTER_SERVER".to_string(),
        ),
        (
            "the served type in lower case",
            SERVED_TYPE.to_lowercase(),
        ),
        (
            "the served type with a leading space",
            format!(" {SERVED_TYPE}"),
        ),
        (
            "the served type with a trailing space",
            format!("{SERVED_TYPE} "),
        ),
        (
            "the served type with a suffix",
            format!("{SERVED_TYPE}_2"),
        ),
        (
            "a prefix of the served type",
            SERVED_TYPE[..SERVED_TYPE.len() - 1].to_string(),
        ),
        ("a single character", "J".to_string()),
        (
            "a value at the published length maximum",
            "T".repeat(64),
        ),
        (
            "a value carrying the characters a type never has",
            "jupyter enterprise gateway!".to_string(),
        ),
    ];

    for (index, (label, endpoint_type)) in rows.iter().enumerate() {
        let body = body_with_type(&format!("ep-tok-type-{index}"), endpoint_type);
        let answer = create_endpoint(&emr, &cluster, body).await;
        answer.refused(label, 400, "ValidationException");
        let message = refusal_message(&answer.body, label);
        assert_eq!(
            message, INVALID_TYPE,
            "case {label:?}: expected the one sentence an unaccepted argument carries, got \
             {message:?}"
        );
    }

    // No refused create stored anything: the cluster holds exactly the endpoint it held
    // before them.
    let case = "the cluster after every refusal";
    assert_eq!(
        stored_ids(&emr, &cluster, case).await,
        [stored.clone()].into_iter().collect::<BTreeSet<String>>(),
        "case {case:?}: a refused create must store no endpoint"
    );
}

/// THE RULE's other half: the type the service does serve is accepted, stored and read
/// back, and it is accepted again in a cluster where a refusal has just been answered.
///
/// The rows are the clusters a create is addressed to, so the accepted path is exercised in
/// a cluster that has only ever been refused and in one that already holds an endpoint.
#[tokio::test]
async fn the_served_managed_endpoint_type_is_accepted_and_reads_back() {
    let emr = Emr::new();
    let fresh = emr.cluster("endpoint-type-fresh", "vc-ep-type-fresh").await;
    let used = emr.cluster("endpoint-type-used", "vc-ep-type-used").await;
    create_endpoint(
        &emr,
        &used,
        endpoint_body("first", "ep-tok-type-first", SERVED_TYPE),
    )
    .await
    .ok("an endpoint created before the row");

    for (label, cluster) in [
        ("a cluster whose only create so far was refused", &fresh),
        ("a cluster that already holds an endpoint", &used),
    ] {
        // The refusal first, so the accepted create below runs on a cluster that has just
        // answered one.
        create_endpoint(
            &emr,
            cluster,
            body_with_type(&format!("ep-tok-refused-{label}"), "SOMETHING_ELSE"),
        )
        .await
        .refused(label, 400, "ValidationException");

        let created = create_endpoint(
            &emr,
            cluster,
            endpoint_body("served", &format!("ep-tok-served-{label}"), SERVED_TYPE),
        )
        .await;
        let id = text(created.ok(label), "/id", label);

        let answer = describe_endpoint(&emr, cluster, &id).await;
        let endpoint = described(answer.ok(label), label);
        assert_eq!(
            field(&endpoint, "/type", label),
            &json!(SERVED_TYPE),
            "case {label:?}: the stored endpoint must report the type it was created with: \
             {endpoint}"
        );
        assert!(
            stored_ids(&emr, cluster, label).await.contains(&id),
            "case {label:?}: the accepted create must be the endpoint the cluster lists"
        );
    }
}
