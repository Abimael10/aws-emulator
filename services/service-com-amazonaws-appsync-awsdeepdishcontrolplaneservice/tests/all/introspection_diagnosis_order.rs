// SPDX-License-Identifier: CC0-1.0

//! The ORDER a data source introspection diagnoses a failure in: the cluster its
//! configuration names is resolved with the control plane that OWNS it before anything
//! about that cluster's Data API endpoint is reported.
//!
//! The rule under test: a `resourceArn` the RDS control plane holds no cluster for settles
//! the job `FAILED` with the service's fixed cluster-not-found sentence, whose whole text is
//! fixed — nothing of the request is interpolated into it — while a cluster that control
//! plane DOES hold keeps the diagnosis of whatever the read of it was refused with, the
//! endpoint-not-enabled refusal among them. So the two conditions a caller remedies
//! differently — an ARN to correct, against a cluster setting to turn on — are never
//! reported as each other.
//!
//! Both halves are asserted in ONE run, over clusters of the same account and Region, so
//! the ordering is pinned rather than each half separately: an ARN that resolves to nothing
//! is never diagnosed by a flag of a cluster that was never reached, and a cluster that IS
//! there is still diagnosed by its own flag.
//!
//! The rows differ along what the rule generalises over: whether the control plane holds the
//! cluster at all, whether a cluster it holds serves the Data API, and — for the ARNs that
//! name nothing — the shape of the identifier (one nothing created, one that extends the
//! identifier of the cluster whose endpoint is off, and one whose identifier differs from it
//! by a single character), so the fixed sentence is read against several different requests.
//!
//! The harness is the sibling diagnosis suite's: one emulator carrying AppSync beside the
//! RDS control plane that holds the cluster, the Data API that serves its databases, and the
//! Secrets Manager (with its KMS) the login comes from, over one shared engine.

use service_com_amazonaws_appsync_awsdeepdishcontrolplaneservice::state;

use crate::rds_introspection_failure_diagnosis::{cluster_arn, Cloud, DATABASE, REGION};

/// The identifier of the cluster this suite creates with its Data API endpoint turned OFF,
/// which is the condition the not-found diagnosis must not be confused with.
const ENDPOINT_OFF_CLUSTER: &str = "diagnosis-order-endpoint-off";

/// The identifier of the cluster this suite creates with its Data API endpoint turned ON,
/// the positive control that the resolution admits a cluster.
const SERVED_CLUSTER: &str = "diagnosis-order-served";

/// The table the served cluster holds, so the positive control reports a model.
const TABLE: &str = "orders";

/// RULE: an introspection resolves the cluster its configuration names with the control
/// plane that owns it before it reads any schema, so a `resourceArn` naming no cluster is
/// diagnosed as a cluster that was not found — never by a property of a cluster nothing
/// resolved — while a cluster that control plane holds is diagnosed by the refusal of the
/// read of it.
#[tokio::test]
async fn an_arn_naming_no_cluster_is_never_diagnosed_by_the_endpoint_of_one() {
    let cloud = Cloud::new();
    let (endpoint_off, off_secret) = cloud.create_cluster(ENDPOINT_OFF_CLUSTER, false).await;
    let (served, secret) = cloud.create_cluster(SERVED_CLUSTER, true).await;
    cloud
        .execute(
            &served,
            &secret,
            &format!("CREATE TABLE {TABLE} (id serial PRIMARY KEY)"),
        )
        .await;
    let account = engine::emulator_account_id();

    // The cluster the control plane HOLDS, whose Data API endpoint is off: the diagnosis is
    // that cluster's own, so it names the cluster that was reached and is not the
    // not-found sentence.
    let case = "a cluster the control plane holds, with its Data API endpoint off";
    let settled = cloud
        .settled_introspection(case, &endpoint_off, &off_secret, DATABASE)
        .await;
    assert_eq!(
        settled.body["introspectionStatus"].as_str(),
        Some(state::INTROSPECTION_FAILED),
        "case {case}: expected a failed introspection, got {}",
        settled.body
    );
    let endpoint_detail = settled.body["introspectionStatusDetail"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    assert_ne!(
        endpoint_detail,
        state::CLUSTER_NOT_FOUND_DETAIL,
        "case {case}: a cluster the control plane holds is not reported as absent, got \
         {endpoint_detail:?}"
    );
    assert!(
        endpoint_detail.contains(&endpoint_off),
        "case {case}: expected the diagnosis to name the cluster that was reached, got \
         {endpoint_detail:?}"
    );

    // (case, the resourceArn) — one row per way a well-formed ARN of the same account and
    // Region names no cluster at all, including identifiers built from the one whose
    // endpoint is off.
    let rows: Vec<(&str, String)> = vec![
        (
            "an identifier nothing created",
            cluster_arn(REGION, account, "diagnosis-order-never-created"),
        ),
        (
            "an identifier extending the one whose endpoint is off",
            format!("{endpoint_off}-replica"),
        ),
        (
            "an identifier differing from it in one character",
            cluster_arn(REGION, account, "diagnosis-order-endpoint-ofg"),
        ),
    ];
    for (case, resource_arn) in rows {
        let settled = cloud
            .settled_introspection(case, &resource_arn, &off_secret, DATABASE)
            .await;
        assert_eq!(
            settled.body["introspectionStatus"].as_str(),
            Some(state::INTROSPECTION_FAILED),
            "case {case}: expected a failed introspection, got {}",
            settled.body
        );
        let detail = settled.body["introspectionStatusDetail"]
            .as_str()
            .unwrap_or_default();
        assert_eq!(
            detail,
            state::CLUSTER_NOT_FOUND_DETAIL,
            "case {case}: expected {:?}, got {detail:?}",
            state::CLUSTER_NOT_FOUND_DETAIL
        );
        // The whole sentence is fixed text: nothing the request carried is interpolated
        // into it, and the condition of a cluster that was never reached is not reported.
        assert!(
            !detail.contains(&resource_arn) && !detail.contains(DATABASE),
            "case {case}: expected a sentence carrying nothing of the request, got {detail:?}"
        );
        assert_ne!(
            detail, endpoint_detail,
            "case {case}: an unresolvable ARN is not diagnosed as the cluster whose endpoint \
             is off, got {detail:?}"
        );
    }

    // The positive control: the cluster the control plane holds and whose Data API serves
    // the read is introspected, so the resolution above admits a cluster rather than
    // refusing every ARN.
    let case = "the cluster whose Data API serves the read";
    let settled = cloud
        .settled_introspection(case, &served, &secret, DATABASE)
        .await;
    assert_eq!(
        settled.body["introspectionStatus"].as_str(),
        Some(state::INTROSPECTION_SUCCESS),
        "case {case}: expected the schema to be read, got {}",
        settled.body
    );
    assert_eq!(
        settled.body["introspectionResult"]["models"][0]["name"].as_str(),
        Some(TABLE),
        "case {case}: expected the table the cluster holds, got {}",
        settled.body
    );
}
