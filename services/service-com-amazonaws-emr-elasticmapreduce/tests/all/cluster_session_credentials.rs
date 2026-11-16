// SPDX-License-Identifier: CC0-1.0

//! The session credentials a cluster with fine-grained access control issues, end to end over
//! the wire.
//!
//! THE RULE: `GetClusterSessionCredentials` "provides temporary, HTTP basic credentials that
//! are associated with a given runtime IAM role and used by a cluster with fine-grained access
//! control activated" (its own Smithy doc), and a cluster gets that activation from the runtime
//! roles or the IAM Identity Center integration its SECURITY CONFIGURATION enables. So the
//! answer follows from the document the launch named: a cluster whose configuration enables
//! one of them is issued a username and password, and a cluster whose configuration enables
//! neither — including a cluster naming no security configuration at all — is refused with the
//! one sentence Amazon EMR words that class in.
//!
//! The rows span what the rule generalises over: WHICH activation the document carries (runtime
//! roles, IAM Identity Center, both), whether the flag is present and false rather than absent,
//! whether the document carries other sections only, and whether the cluster names a
//! configuration at all.
//!
//! The pair itself is held to what it is FOR: the username identifies the session, so it is
//! stable for one runtime role on one cluster and distinct for another role or another cluster;
//! the password is temporary, so two calls never hand back the same one.
//!
//! Every call drives the real HTTP path through the router's `dispatch`, and every security
//! configuration is created through `CreateSecurityConfiguration` itself — so what decides the
//! answer is the document this suite stored, never anything handed to the handler.

use std::collections::BTreeSet;
use std::time::Duration;

use serde_json::json;
use service_com_amazonaws_emr_elasticmapreduce::state::{
    runtime_roles_not_enabled_message, SESSION_CREDENTIALS_LIFETIME,
};

use super::job_flow_lifecycle::{at, kept_alive_request, Emr};

/// The runtime roles a session is asked for: two roles of this account, which differ in
/// nothing but their name.
const ROLE_A: &str = "arn:aws:iam::000000000000:role/EMRRuntimeRoleA";
const ROLE_B: &str = "arn:aws:iam::000000000000:role/EMRRuntimeRoleB";

/// The room an expiry assertion leaves for however long the launch and the calls before it
/// took, so a loaded host cannot fail a case about the LIFETIME of the credentials.
const SLACK: Duration = Duration::from_secs(600);

/// One row: the security configuration a cluster is launched with, and whether such a cluster
/// issues session credentials.
struct Case {
    label: &'static str,
    /// The document the launch's `SecurityConfiguration` names, or `None` for a launch that
    /// names none at all.
    document: Option<&'static str>,
    /// Whether a cluster launched with it has fine-grained access control activated.
    issues: bool,
}

fn cases() -> Vec<Case> {
    vec![
        Case {
            label: "runtime roles with in-transit encryption",
            document: Some(
                r#"{"AuthorizationConfiguration":{"IAMConfiguration":
                   {"EnableApplicationScopedIAMRole":true}},
                   "EncryptionConfiguration":{"EnableInTransitEncryption":true,
                   "EnableAtRestEncryption":false,
                   "InTransitEncryptionConfiguration":{"TLSCertificateConfiguration":
                   {"CertificateProviderType":"PEM","S3Object":"s3://certs/tls-certs.zip"}}}}"#,
            ),
            issues: true,
        },
        Case {
            label: "runtime roles alone",
            document: Some(
                r#"{"AuthorizationConfiguration":{"IAMConfiguration":
                   {"EnableApplicationScopedIAMRole":true}}}"#,
            ),
            issues: true,
        },
        Case {
            label: "IAM Identity Center alone",
            document: Some(
                r#"{"AuthenticationConfiguration":{"IdentityCenterConfiguration":
                   {"EnableIdentityCenter":true,
                   "IdentityCenterInstanceARN":"arn:aws:sso:::instance/ssoins-0000000000000000"}}}"#,
            ),
            issues: true,
        },
        Case {
            label: "runtime roles and IAM Identity Center together",
            document: Some(
                r#"{"AuthorizationConfiguration":{"IAMConfiguration":
                   {"EnableApplicationScopedIAMRole":true}},
                   "AuthenticationConfiguration":{"IdentityCenterConfiguration":
                   {"EnableIdentityCenter":true}}}"#,
            ),
            issues: true,
        },
        Case {
            label: "runtime roles present and switched off",
            document: Some(
                r#"{"AuthorizationConfiguration":{"IAMConfiguration":
                   {"EnableApplicationScopedIAMRole":false}}}"#,
            ),
            issues: false,
        },
        Case {
            label: "IAM Identity Center present and switched off",
            document: Some(
                r#"{"AuthenticationConfiguration":{"IdentityCenterConfiguration":
                   {"EnableIdentityCenter":false}}}"#,
            ),
            issues: false,
        },
        Case {
            label: "an encryption-only configuration",
            document: Some(
                r#"{"EncryptionConfiguration":{"EnableInTransitEncryption":false,
                   "EnableAtRestEncryption":false}}"#,
            ),
            issues: false,
        },
        Case {
            label: "a launch naming no security configuration",
            document: None,
            issues: false,
        },
    ]
}

/// Launch one row's cluster, creating its security configuration first when it has one.
async fn launched(emr: &Emr, case: &Case, ordinal: usize) -> String {
    let mut request = kept_alive_request(case.label);
    if let Some(document) = case.document {
        let name = format!("session-credentials-{ordinal}");
        emr.ok(
            "CreateSecurityConfiguration",
            json!({ "Name": name, "SecurityConfiguration": document }),
        )
        .await;
        request["SecurityConfiguration"] = json!(name);
    }
    emr.run_job_flow(request).await
}

/// The instant the cluster `cluster_id` was CREATED, as the service itself reports it.
///
/// The reference every expiry below is judged against is the SERVICE's own clock rather than
/// this suite's: the credentials expire after the cluster that issued them was launched, and
/// inside the lifetime this service publishes.
async fn created_at(emr: &Emr, cluster_id: &str) -> f64 {
    let cluster = emr.describe_cluster(cluster_id).await;
    at(&cluster, &["Status", "Timeline", "CreationDateTime"])
        .as_f64()
        .unwrap_or_else(|| {
            panic!("DescribeCluster reports when the cluster was created, got {cluster}")
        })
}

/// The `(username, password)` one call issued, which must be a non-empty pair.
async fn issued(
    emr: &Emr,
    case: &str,
    cluster_id: &str,
    created_at: f64,
    role: Option<&str>,
) -> (String, String) {
    let mut body = json!({ "ClusterId": cluster_id });
    if let Some(role) = role {
        body["ExecutionRoleArn"] = json!(role);
    }
    let (status, payload) = emr.call("GetClusterSessionCredentials", body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: a cluster with fine-grained access control activated issues \
         credentials, got {status} with {payload}"
    );
    let pair = at(&payload, &["Credentials", "UsernamePassword"]);
    let username = pair["Username"].as_str().unwrap_or_default().to_string();
    let password = pair["Password"].as_str().unwrap_or_default().to_string();
    assert!(
        !username.is_empty() && !password.is_empty(),
        "case {case:?}: expected a non-empty username and password, got {payload}"
    );
    // "The time when the credentials that are returned by the GetClusterSessionCredentials
    // API expire." A temporary credential expires after the call that issued it and inside the
    // lifetime this service publishes.
    let expires_at = at(&payload, &["ExpiresAt"]).as_f64().unwrap_or_default();
    let latest = created_at + SESSION_CREDENTIALS_LIFETIME.as_secs_f64() + SLACK.as_secs_f64();
    assert!(
        expires_at > created_at && expires_at <= latest,
        "case {case:?}: expected the credentials to expire after {created_at} and by {latest}, \
         got {payload}"
    );
    (username, password)
}

/// Every row: a cluster whose security configuration activates fine-grained access control
/// issues the pair, and one whose configuration activates neither is refused with Amazon EMR's
/// own sentence for that class.
#[tokio::test]
async fn fine_grained_access_control_decides_whether_credentials_are_issued() {
    let emr = Emr::new();
    let cases = cases();
    let mut clusters: Vec<String> = Vec::new();
    for (ordinal, case) in cases.iter().enumerate() {
        clusters.push(launched(&emr, case, ordinal).await);
    }

    for (case, cluster_id) in cases.iter().zip(&clusters) {
        let label = case.label;
        if case.issues {
            let created = created_at(&emr, cluster_id).await;
            issued(&emr, label, cluster_id, created, Some(ROLE_A)).await;
            continue;
        }
        let (status, payload) = emr
            .call(
                "GetClusterSessionCredentials",
                json!({ "ClusterId": cluster_id, "ExecutionRoleArn": ROLE_A }),
            )
            .await;
        assert_eq!(
            status, 400,
            "case {label:?}: a cluster with neither activation is refused, got {status} \
             with {payload}"
        );
        let code = at(&payload, &["__type"]).as_str().unwrap_or_default();
        assert!(
            code.contains("InvalidRequestException"),
            "case {label:?}: expected InvalidRequestException, got {code:?} in {payload}"
        );
        let message = at(&payload, &["Message"])
            .as_str()
            .or_else(|| at(&payload, &["message"]).as_str())
            .unwrap_or_default();
        assert_eq!(
            message,
            runtime_roles_not_enabled_message(),
            "case {label:?}: expected Amazon EMR's own sentence, got {payload}"
        );
    }
}

/// The username identifies the SESSION: one runtime role on one cluster keeps it across calls,
/// and another role, another cluster or a session with no runtime role at all gets another.
/// The password is temporary, so no two calls hand back the same one.
#[tokio::test]
async fn the_username_is_scoped_to_the_cluster_and_the_runtime_role() {
    let emr = Emr::new();
    let fine_grained = cases()
        .into_iter()
        .find(|case| case.issues)
        .expect("one row activates fine-grained access control");
    let first = launched(&emr, &fine_grained, 100).await;
    let second = launched(&emr, &fine_grained, 101).await;
    let first_created = created_at(&emr, &first).await;
    let second_created = created_at(&emr, &second).await;

    // One role, asked for twice on the same cluster: the same session, a fresh password.
    let (username_a, password_a) = issued(&emr, "role A", &first, first_created, Some(ROLE_A)).await;
    let (username_a_again, password_a_again) =
        issued(&emr, "role A again", &first, first_created, Some(ROLE_A)).await;
    assert_eq!(
        username_a, username_a_again,
        "asking again for one role on one cluster identifies the same session, got \
         {username_a:?} and {username_a_again:?}"
    );
    assert_ne!(
        password_a, password_a_again,
        "the credentials are temporary, so each call issues a password of its own, got \
         {password_a:?} twice"
    );

    // Every other session: another role, another cluster, and a session with no role named.
    let rows: [(&str, &String, f64, Option<&str>); 3] = [
        ("another role", &first, first_created, Some(ROLE_B)),
        ("another cluster", &second, second_created, Some(ROLE_A)),
        ("no runtime role named", &first, first_created, None),
    ];
    let mut seen: BTreeSet<String> = BTreeSet::new();
    seen.insert(username_a.clone());
    for (case, cluster_id, created, role) in rows {
        let (username, _) = issued(&emr, case, cluster_id, created, role).await;
        assert!(
            seen.insert(username.clone()),
            "case {case:?}: expected a username of its own, got {username:?} which another \
             session already carries"
        );
    }
}

/// Two calls on one cluster never hand back the same expiry twice over: the pair is temporary,
/// so the expiry each call reports moves with the call rather than being fixed to the cluster.
#[tokio::test]
async fn the_issued_credentials_are_temporary() {
    let emr = Emr::new();
    let fine_grained = cases()
        .into_iter()
        .find(|case| case.issues)
        .expect("one row activates fine-grained access control");
    let cluster_id = launched(&emr, &fine_grained, 200).await;
    let created = created_at(&emr, &cluster_id).await;
    // Both calls are judged against the service's own clock inside `issued`: each expiry is
    // after the cluster was launched and inside the published lifetime.
    let (_, first_password) = issued(&emr, "the first call", &cluster_id, created, Some(ROLE_A)).await;
    let (_, second_password) =
        issued(&emr, "the second call", &cluster_id, created, Some(ROLE_B)).await;
    assert_ne!(
        first_password, second_password,
        "two sessions of one cluster are authenticated by passwords of their own, got \
         {first_password:?} twice"
    );
}
