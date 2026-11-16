// SPDX-License-Identifier: CC0-1.0

//! WHICH ERROR CLASS this service refuses a mutation in when the request names an id it
//! holds nothing under, and the wording that goes WITH that class, end to end.
//!
//! Every call goes through the production path: the service is mounted in the real
//! `MultiServiceRouter` by the harness [`crate::virtual_cluster_job_run_loop::Emr`] and
//! driven with restJson1 HTTP requests, so routing, deserialization, constraint
//! validation, the handler and serialization all run exactly as they do for an AWS SDK
//! client.
//!
//! The rule under test: a refusal's error code and its message are ONE pair, so the two
//! always agree.
//!
//! 1. A resource this service reports MISSING is carried by its not-found error
//!    (`ResourceNotFoundException`, HTTP 400) and worded from the identifier the request
//!    named the resource by — `<Resource type> <identifier> doesn't exist.` — including
//!    where the operation's own modelled `errors` list omits that shape. `CancelJobRun` is
//!    such an operation.
//! 2. A call refused as INVALID instead — which is what every delete of this service
//!    answers an id it holds nothing under — is carried by `ValidationException`
//!    (HTTP 400) and worded as a SHORT FIXED sentence carrying no identifier at all.
//!
//! The tables span what the rule generalises over: which resource the request names
//! (a virtual cluster, a managed endpoint, a job template, a security configuration, a job
//! run), which published form the requested id takes (both length bounds of
//! `ResourceIdString`, an all-digit value, a plausible minted one), and — for a job run —
//! HOW it is absent (an id nothing holds, an id under a cluster that is not there either,
//! and a real run's id offered under another cluster).
//!
//! It also pins the rule's BOUNDARY both ways: the READ of the very same id keeps the
//! identifier-bearing wording of the not-found class, and a resource that EXISTS but
//! cannot take the call is refused for what it is rather than as one that is absent.

use crate::job_run_outcome::poll_until_terminal;
use crate::virtual_cluster_job_run_loop::{text, Answer, Emr};
use crate::virtual_cluster_scheduler_status::run_id;

/// The class a call refused as INVALID is carried by: the client error every operation of
/// this service models (smithy_query: digest — `common_errors`).
const INVALID_REQUEST: &str = "ValidationException";

/// The class a resource reported MISSING is carried by (smithy_query: shape
/// `ResourceNotFoundException`, a client error at HTTP 400).
const NOT_FOUND: &str = "ResourceNotFoundException";

/// The identifier-bearing wording of the not-found class, written once: the resource type
/// as this service spells it and the identifier the request carried.
fn not_found_wording(resource_type: &str, identifier: &str) -> String {
    format!("{resource_type} {identifier} doesn't exist.")
}

/// One delete aimed at an id the service holds nothing under, with the read of the SAME id
/// beside it.
struct AbsentDelete {
    case: String,
    /// The id the refusal must not name.
    identifier: String,
    delete_path: String,
    /// `GET` of the same resource, which reports it missing instead.
    describe_path: String,
    /// The fixed sentence the `ValidationException` must carry.
    fixed_sentence: &'static str,
    /// How the read path spells this resource's type.
    resource_type: &'static str,
}

/// The sentences this service words a delete's refusal with, one per resource whose delete
/// refuses the call as invalid.
///
/// Hand-written because a sentence is not derivable from the model: each is the wording the
/// recorded refusal of that path carries. `Template does not exist` and `Security
/// configuration not found` are the rows this issue did not cite — note the lower-case
/// spelling of the second, which is NOT the read path's `Security Configuration`, which is
/// why one sentence cannot serve both classes.
const ABSENT_VIRTUAL_CLUSTER: &str = "VirtualCluster does not exist";
const ABSENT_MANAGED_ENDPOINT: &str = "Endpoint does not exist";
const ABSENT_JOB_TEMPLATE: &str = "Template does not exist";
const ABSENT_SECURITY_CONFIGURATION: &str = "Security configuration not found";

/// How this service spells each resource type in the not-found class's wording (the model's
/// own shapes `VirtualCluster`, `JobRun` and `Endpoint`; `Job Template` and `Security
/// Configuration` are the spellings that class's recorded refusals show).
const VIRTUAL_CLUSTER: &str = "Virtual cluster";
const JOB_RUN: &str = "Job run";
const ENDPOINT: &str = "Endpoint";
const JOB_TEMPLATE: &str = "Job Template";
const SECURITY_CONFIGURATION: &str = "Security Configuration";

/// Identifiers no resource of this service holds, spanning the forms a `ResourceIdString`
/// takes: both published length bounds (len 1..64, `^[0-9a-z]+$` — smithy_query: shape
/// `ResourceIdString`), an all-digit value and a plausible minted one.
fn absent_ids() -> Vec<(&'static str, String)> {
    vec![
        ("the shortest id the model allows", "a".to_string()),
        ("the longest id the model allows", "z".repeat(64)),
        ("an all-digit id", "0123456789".to_string()),
        ("a plausible minted id", "neverminted7".to_string()),
    ]
}

/// Every delete of this service, aimed at `absent`: the virtual cluster addressed by the
/// path's only label, the managed endpoint under the `host` cluster that really exists, the
/// same endpoint id under a cluster that is not there either, the job template and the
/// security configuration.
fn absent_deletes(absent: &str, label: &str, host: &str) -> Vec<AbsentDelete> {
    vec![
        AbsentDelete {
            case: format!("DeleteVirtualCluster, {label}"),
            identifier: absent.to_string(),
            delete_path: format!("/virtualclusters/{absent}"),
            describe_path: format!("/virtualclusters/{absent}"),
            fixed_sentence: ABSENT_VIRTUAL_CLUSTER,
            resource_type: VIRTUAL_CLUSTER,
        },
        AbsentDelete {
            case: format!("DeleteManagedEndpoint under a stored cluster, {label}"),
            identifier: absent.to_string(),
            delete_path: format!("/virtualclusters/{host}/endpoints/{absent}"),
            describe_path: format!("/virtualclusters/{host}/endpoints/{absent}"),
            fixed_sentence: ABSENT_MANAGED_ENDPOINT,
            resource_type: ENDPOINT,
        },
        AbsentDelete {
            case: format!("DeleteManagedEndpoint under an absent cluster, {label}"),
            identifier: absent.to_string(),
            delete_path: format!("/virtualclusters/{absent}/endpoints/{absent}"),
            describe_path: format!("/virtualclusters/{absent}/endpoints/{absent}"),
            fixed_sentence: ABSENT_MANAGED_ENDPOINT,
            resource_type: ENDPOINT,
        },
        AbsentDelete {
            case: format!("DeleteJobTemplate, {label}"),
            identifier: absent.to_string(),
            delete_path: format!("/jobtemplates/{absent}"),
            describe_path: format!("/jobtemplates/{absent}"),
            fixed_sentence: ABSENT_JOB_TEMPLATE,
            resource_type: JOB_TEMPLATE,
        },
        AbsentDelete {
            case: format!("DeleteSecurityConfiguration, {label}"),
            identifier: absent.to_string(),
            delete_path: format!("/securityconfigurations/{absent}"),
            describe_path: format!("/securityconfigurations/{absent}"),
            fixed_sentence: ABSENT_SECURITY_CONFIGURATION,
            resource_type: SECURITY_CONFIGURATION,
        },
    ]
}

/// RULE 2, with RULE 1 beside it on the same id: the delete answers the fixed sentence of
/// the invalid-request class with no identifier in it, and the read of that very id answers
/// the identifier-bearing wording of the not-found class.
#[tokio::test]
async fn every_delete_refuses_an_absent_resource_with_one_fixed_sentence() {
    let emr = Emr::new();
    // A cluster that really exists, so an endpoint row is refused for its endpoint rather
    // than for the cluster it is addressed under.
    let host = emr.cluster("class-host", "tok-class-host").await;

    for (label, absent) in absent_ids() {
        for row in absent_deletes(&absent, label, &host) {
            let case = row.case.as_str();

            let refused: Answer = emr.send("DELETE", &row.delete_path, None).await;
            refused.refused(case, 400, INVALID_REQUEST);
            let message = text(&refused.body, "/message", case);
            // Equality against ONE sentence for every id the rows send is what pins it as
            // fixed text: a wording with a variable part could not be equal to it for all
            // of them.
            assert_eq!(
                message, row.fixed_sentence,
                "case {case:?}: expected message {:?}, got {message:?}",
                row.fixed_sentence
            );
            // The other half of "fixed text", on every id long enough for the check to
            // mean anything: the sentence does not carry the requested id. (A one-character
            // id is a substring of almost any sentence, so equality to the constant above
            // is what pins those rows.)
            assert!(
                row.identifier.len() < 4 || !message.contains(&row.identifier),
                "case {case:?}: expected a sentence naming nothing of the request, got \
                 {message:?}"
            );

            let read = emr.send("GET", &row.describe_path, None).await;
            read.refused(case, 400, NOT_FOUND);
            let reported = text(&read.body, "/message", case);
            let expected = not_found_wording(row.resource_type, &row.identifier);
            assert_eq!(
                reported, expected,
                "case {case:?}: expected the read to keep the wording {expected:?}, got \
                 {reported:?}"
            );
        }
    }

    // None of the refusals touched the store: the host cluster still reads back.
    let case = "the host cluster after the refusals";
    let described = emr.describe_cluster(&host).await;
    assert_eq!(
        text(described.ok(case), "/virtualCluster/id", case),
        host,
        "case {case:?}: the refused deletes must have left the host cluster readable"
    );
}

/// One cancellation aimed at a run the addressed cluster holds nothing under.
struct AbsentRun {
    case: String,
    /// The run id the path carries, which the refusal reports.
    identifier: String,
    path: String,
}

/// RULE 1 on the operation whose modelled `errors` list omits the not-found shape:
/// `CancelJobRun` reports a run it holds nothing under as the missing resource it is, in
/// the not-found class and its identifier-bearing wording.
#[tokio::test]
async fn a_cancellation_of_an_absent_run_is_reported_as_a_missing_resource() {
    let emr = Emr::new();
    let host = emr.cluster("cancel-host", "tok-cancel-host").await;
    let elsewhere = emr.cluster("cancel-other", "tok-cancel-other").await;
    // A run that really exists under `host`, so the row that offers it under `elsewhere`
    // is refused for the cluster it is offered under and not for the id being unknown.
    let stored = run_id(&emr, "a run of the host cluster", &host, "tok-cancel-run").await;

    let mut rows: Vec<AbsentRun> = Vec::new();
    for (label, absent) in absent_ids() {
        rows.push(AbsentRun {
            case: format!("CancelJobRun under a stored cluster, {label}"),
            identifier: absent.clone(),
            path: format!("/virtualclusters/{host}/jobruns/{absent}"),
        });
        rows.push(AbsentRun {
            case: format!("CancelJobRun under an absent cluster, {label}"),
            identifier: absent.clone(),
            path: format!("/virtualclusters/{absent}/jobruns/{absent}"),
        });
    }
    rows.push(AbsentRun {
        case: "CancelJobRun of a stored run under another cluster".to_string(),
        identifier: stored.clone(),
        path: format!("/virtualclusters/{elsewhere}/jobruns/{stored}"),
    });

    for row in &rows {
        let case = row.case.as_str();
        let refused: Answer = emr.send("DELETE", &row.path, None).await;
        refused.refused(case, 400, NOT_FOUND);
        let message = text(&refused.body, "/message", case);
        let expected = not_found_wording(JOB_RUN, &row.identifier);
        assert_eq!(
            message, expected,
            "case {case:?}: expected message {expected:?}, got {message:?}"
        );
    }

    // The run the rows never addressed under its own cluster is still there to cancel, so
    // the refusals above neither consumed nor moved it.
    let case = "the stored run after the refusals";
    let cancelled = emr
        .send(
            "DELETE",
            &format!("/virtualclusters/{host}/jobruns/{stored}"),
            None,
        )
        .await;
    assert_eq!(
        text(cancelled.ok(case), "/id", case),
        stored,
        "case {case:?}: the stored run must still be cancellable"
    );
}

/// The rule's BOUNDARY: a resource that EXISTS but cannot take the call is refused for what
/// it is, in neither the not-found class's wording nor the delete class's fixed sentence.
///
/// Two rows, one per class: a job run that has finished (so `CancelJobRun` has nothing left
/// to cancel) and a virtual cluster whose teardown has already started.
#[tokio::test]
async fn a_resource_that_exists_is_not_refused_as_an_absent_one() {
    let emr = Emr::new();
    let cluster = emr.cluster("boundary-host", "tok-boundary-host").await;

    let case = "cancelling a run that has already finished";
    let run = run_id(&emr, case, &cluster, "tok-boundary-run").await;
    poll_until_terminal(&emr, case, &cluster, &run).await;
    let refused = emr
        .send(
            "DELETE",
            &format!("/virtualclusters/{cluster}/jobruns/{run}"),
            None,
        )
        .await;
    // A run that is there is not a run that is missing, so this is the invalid-call class
    // and never the not-found one.
    refused.refused(case, 400, INVALID_REQUEST);
    let message = text(&refused.body, "/message", case);
    assert_ne!(
        message,
        not_found_wording(JOB_RUN, &run),
        "case {case:?}: a run that exists must not be reported as one that does not"
    );
    assert!(
        message.contains(&run),
        "case {case:?}: the refusal must name the run it is about, got {message:?}"
    );

    let case = "deleting a cluster whose teardown has started";
    emr.delete_cluster(&cluster)
        .await
        .ok("starting the teardown");
    let refused = emr.delete_cluster(&cluster).await;
    refused.refused(case, 400, INVALID_REQUEST);
    let message = text(&refused.body, "/message", case);
    assert_ne!(
        message, ABSENT_VIRTUAL_CLUSTER,
        "case {case:?}: a cluster that exists must not be refused as an absent one"
    );

    // And the cluster really is still there, so the refusal was about its state.
    let described = emr.describe_cluster(&cluster).await;
    assert_eq!(
        text(described.ok(case), "/virtualCluster/id", case),
        cluster,
        "case {case:?}: the refused delete must have removed nothing"
    );
}
