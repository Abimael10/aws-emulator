// SPDX-License-Identifier: CC0-1.0

//! The two questions `AssumeRoleForPodIdentity` answers about something OTHER than the
//! form of the presented value, and what each is answered with.
//!
//! 1. THE `clusterName` LABEL IS A RESOURCE IDENTITY. A label that names no cluster is
//!    the modelled `ResourceNotFoundException` (404) — the request addresses a resource
//!    that is not there — whatever the presented token is worth, and the name is matched
//!    exactly, so a differently-cased spelling of a registered cluster is a different
//!    cluster. The question about the VALUE still comes first: a value that is not a JSON
//!    Web Token at all is refused as such whichever cluster the URI names.
//! 2. A CLAIM-LEVEL REJECTION NAMES THE CLAIM. When the issuing cluster's verification
//!    refuses a claim of a token it really issued, the refusal names that claim and the
//!    value the claim has to carry where exactly one value is admissible — the audience
//!    the model documents as always `pods.eks.amazonaws.com` — so a projected-token volume
//!    asking for the wrong `audiences:` is diagnosable from the refusal. It is still the
//!    modelled `InvalidTokenException` (400), and nothing of the `@sensitive` token is
//!    echoed back.
//!
//! Each rule is asserted over a TABLE: rule 1 over the cross product of unregistered
//! cluster names (a case-flipped registered name, names nothing registered, the published
//! length bounds) and tokens of every worth (genuine, unissued, expired, wrongly
//! addressed); rule 2 over the audience lists a Kubernetes API server can mint a token
//! for, plus the claim judged against the clock. Every row is driven over the real
//! request path through the harness `tests/all/assume_role_for_pod_identity.rs` owns.

use std::time::{Duration, SystemTime};

use service_com_amazonaws_eksauth_eksauthfrontend::state::{
    encode_jwt_segment, service_state, ServiceAccountTokenRequest, PODS_AUDIENCE,
};

use crate::assume_role_for_pod_identity::{Harness, SEEDS};

/// Two seeded clusters, each publishing its own signing key and holding its own
/// associations.
const CLUSTER: &str = SEEDS[0].cluster;
const OTHER_CLUSTER: &str = SEEDS[3].cluster;

/// What one call answered: the status, the shape restJson1 resolved, the message and the
/// raw body.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Reply {
    status: u16,
    code: String,
    message: String,
    body: String,
}

impl Reply {
    /// `<status> <shape>`, for comparing a whole verdict in one assertion.
    fn verdict(&self) -> String {
        format!("{} {}", self.status, self.code)
    }
}

/// `POST /clusters/{cluster}/assume-role-for-pod-identity` with `token`.
async fn assume(harness: &Harness, cluster: &str, token: &str) -> Reply {
    let (status, errortype, body) = harness
        .send(cluster, serde_json::json!({ "token": token }))
        .await;
    let parsed: serde_json::Value = serde_json::from_str(&body).unwrap_or(serde_json::Value::Null);
    let code = parsed["__type"]
        .as_str()
        .or(errortype.as_deref())
        .unwrap_or("<no shape>");
    let code = code
        .split(':')
        .next()
        .unwrap_or(code)
        .rsplit(['#', '/'])
        .next()
        .unwrap_or(code)
        .to_string();
    Reply {
        status,
        code,
        message: parsed["message"].as_str().unwrap_or_default().to_string(),
        body,
    }
}

/// AWS's not-found sentence for a cluster name, whose only variable part is the name the
/// request URI carried.
fn not_found_message(cluster: &str) -> String {
    format!("No cluster found for name: {cluster}.")
}

/// The sentence a claim-level refusal is worded as: the claim that failed, and the value
/// it must carry when exactly one value is admissible.
fn invalid_claim_message(claim: &str, expected: Option<&str>) -> String {
    let expected = expected
        .map(|expected| format!(" Expected {expected}."))
        .unwrap_or_default();
    format!("The token included in the request has an invalid claim value for {claim}.{expected}")
}

/// Cluster names inside the modelled `ClusterName` (1..100 characters of
/// `^[0-9A-Za-z][A-Za-z0-9\-_]*$`, from the model's own constraints on the member) that no
/// cluster in the seeded world carries — so each is a pattern-valid name a request can
/// address and nothing can resolve.
fn unregistered_names() -> Vec<(&'static str, String)> {
    vec![
        // The recorded instance: the registered name with its case flipped, which the
        // case-SENSITIVE pattern makes a different name.
        ("a registered name with its case flipped", "DEMO-CLUSTER".to_string()),
        ("the other registered name, case-flipped", "pROD2".to_string()),
        ("a name nothing registered", "no-such-cluster".to_string()),
        ("a registered name with one character more", format!("{CLUSTER}2")),
        (
            "a registered name without its last character",
            CLUSTER[..CLUSTER.len() - 1].to_string(),
        ),
        // The published bounds of the member's @length: 1 and 100 characters.
        ("the shortest name the model admits", "0".to_string()),
        ("the longest name the model admits", "c".repeat(100)),
        ("a name carrying an underscore", "cluster_0".to_string()),
    ]
}

/// RULE 1: every token, however good, against every cluster name that resolves to
/// nothing, is the modelled not-found — and the sentence names the cluster the URI
/// carried.
#[tokio::test]
async fn an_unresolvable_cluster_name_is_the_modelled_not_found() {
    let harness = Harness::seeded();
    let seed = SEEDS[0];

    // Tokens spanning what a caller can hold: the genuine token of each seeded cluster,
    // one in the form a cluster issues that nothing issued, one whose window has closed,
    // and one issued to another audience. None of them may change the answer.
    let now = SystemTime::now();
    let genuine = harness.token(&seed);
    let other_cluster_token = harness.token(&SEEDS[3]);
    let unissued = crate::rejection_errors::token_for_key(&harness.signing_key_id(CLUSTER));
    let expired = harness.token_request(
        CLUSTER,
        ServiceAccountTokenRequest::for_pod(seed.namespace, seed.service_account, seed.pod)
            .issued_at(now - Duration::from_secs(600))
            .expiring_at(now - Duration::from_secs(60)),
    );
    let wrong_audience = harness.token_request(
        CLUSTER,
        ServiceAccountTokenRequest::for_pod(seed.namespace, seed.service_account, seed.pod)
            .for_audience(&["sts.amazonaws.com"]),
    );
    let tokens: &[(&str, &str)] = &[
        ("the genuine token of the cluster", genuine.as_str()),
        ("the genuine token of another cluster", other_cluster_token.as_str()),
        ("a token in the form a cluster issues that nothing issued", unissued.as_str()),
        ("a genuine token whose window has closed", expired.as_str()),
        ("a genuine token issued to another audience", wrong_audience.as_str()),
    ];

    for (name_case, cluster) in unregistered_names() {
        for (token_case, token) in tokens {
            let reply = assume(&harness, &cluster, token).await;
            assert_eq!(
                reply.verdict(),
                "404 ResourceNotFoundException",
                "case {name_case:?} with {token_case:?}: expected the modelled not-found, \
                 got {reply:?}"
            );
            assert_eq!(
                reply.message,
                not_found_message(&cluster),
                "case {name_case:?} with {token_case:?}: expected the not-found sentence \
                 naming {cluster:?}, got {:?}",
                reply.message
            );
            // `token` is @sensitive: no part of the presented value comes back.
            assert!(
                !reply.body.contains(*token),
                "case {name_case:?} with {token_case:?}: the token reached the response {}",
                reply.body
            );
        }
    }

    // None of those refusals minted anything.
    let state = service_state(&*harness.engine).expect("state");
    assert_eq!(
        state.lock().sessions.len(),
        0,
        "a refused exchange must mint no session"
    );
}

/// The CONTROL for rule 1: the same tokens against the cluster that issued them are
/// judged on the token, so the not-found above is a verdict on the NAME and not an answer
/// this operation gives everybody.
#[tokio::test]
async fn a_resolvable_cluster_name_is_judged_on_the_token() {
    let harness = Harness::seeded();
    let seed = SEEDS[0];
    let now = SystemTime::now();

    let expired = harness.token_request(
        CLUSTER,
        ServiceAccountTokenRequest::for_pod(seed.namespace, seed.service_account, seed.pod)
            .issued_at(now - Duration::from_secs(600))
            .expiring_at(now - Duration::from_secs(60)),
    );
    let unissued = crate::rejection_errors::token_for_key(&harness.signing_key_id(CLUSTER));
    // (case, the cluster the URI names, the token, the verdict)
    let rows: Vec<(&str, &str, String, &str)> = vec![
        (
            "the genuine token at its own cluster",
            CLUSTER,
            harness.token(&seed),
            "200 <no shape>",
        ),
        (
            "the genuine token of the other cluster at its own cluster",
            OTHER_CLUSTER,
            harness.token(&SEEDS[3]),
            "200 <no shape>",
        ),
        (
            "the genuine token of one cluster at the other",
            OTHER_CLUSTER,
            harness.token(&seed),
            "400 InvalidTokenException",
        ),
        (
            "a token nothing issued at a registered cluster",
            CLUSTER,
            unissued,
            "400 InvalidTokenException",
        ),
        (
            "a token whose window has closed at its own cluster",
            CLUSTER,
            expired,
            "400 ExpiredTokenException",
        ),
    ];

    for (case, cluster, token, expected) in &rows {
        let reply = assume(&harness, cluster, token).await;
        assert_eq!(
            reply.verdict(),
            *expected,
            "case {case:?}: expected {expected}, got {reply:?}"
        );
        assert_ne!(
            reply.code, "ResourceNotFoundException",
            "case {case:?}: a cluster that resolves is never reported as not found, got \
             {reply:?}"
        );
    }
}

/// Rule 1 does not move the question about the VALUE: a presented value that is not a
/// JSON Web Token is refused as such before any state is read, so it is the modelled
/// invalid-token failure even when the URI names no cluster.
#[tokio::test]
async fn a_value_that_is_no_token_is_still_refused_on_the_value() {
    let harness = Harness::seeded();
    // Each row is three base64url segments — a value the modelled `JwtToken` pattern
    // ADMITS, so it reaches the handler — whose first segment is nevertheless not a JOSE
    // header.
    let claims = encode_jwt_segment(r#"{"sub":"system:serviceaccount:default:my-sa"}"#);
    let signature = encode_jwt_segment([7_u8; 32]);
    let not_tokens: Vec<(&str, String)> = vec![
        (
            "a header that is not base64url",
            format!("A.{claims}.{signature}"),
        ),
        (
            "a header that is not JSON",
            format!(
                "{}.{claims}.{signature}",
                encode_jwt_segment("not json at all")
            ),
        ),
        (
            "a header that is a JSON array",
            format!(
                "{}.{claims}.{signature}",
                encode_jwt_segment(r#"["alg","kid"]"#)
            ),
        ),
        (
            "a header that is a JSON object but not a JOSE header",
            format!(
                "{}.{claims}.{signature}",
                encode_jwt_segment(r#"{"typ":"JWT"}"#)
            ),
        ),
    ];

    for (name_case, cluster) in unregistered_names() {
        for (token_case, token) in &not_tokens {
            let reply = assume(&harness, &cluster, token).await;
            assert_eq!(
                reply.verdict(),
                "400 InvalidTokenException",
                "case {name_case:?} with {token_case:?}: a value that is no token is \
                 refused on the value, got {reply:?}"
            );
            assert_ne!(
                reply.code, "ResourceNotFoundException",
                "case {name_case:?} with {token_case:?}: the value is judged before the \
                 cluster, got {reply:?}"
            );
        }
    }
}

/// RULE 2: the audience a cluster issued a token to decides the exchange, and a token
/// issued to anything else is refused on the `aud` claim — naming the claim and the one
/// audience a pod identity token may carry.
#[tokio::test]
async fn a_token_issued_to_another_audience_is_refused_on_the_aud_claim() {
    let harness = Harness::seeded();
    let seed = SEEDS[0];
    let expected = invalid_claim_message("aud", Some(PODS_AUDIENCE));

    // The audience lists a Kubernetes API server can mint a projected token for, and
    // whether the pod identity exchange admits each: only a list CARRYING the pod
    // identity audience is admitted, and the comparison is exact.
    let rows: &[(&str, &[&str], bool)] = &[
        // The recorded instance.
        ("the STS audience", &["sts.amazonaws.com"], false),
        (
            "the in-cluster API server audience",
            &["https://kubernetes.default.svc"],
            false,
        ),
        ("no audience at all", &[], false),
        (
            "two audiences, neither of them the pod identity one",
            &["sts.amazonaws.com", "https://kubernetes.default.svc"],
            false,
        ),
        (
            "a near miss of the pod identity audience",
            &["pods.eks.amazonaws.com.example"],
            false,
        ),
        (
            "the pod identity audience in upper case",
            &["PODS.EKS.AMAZONAWS.COM"],
            false,
        ),
        ("the pod identity audience", &[PODS_AUDIENCE], true),
        (
            "the pod identity audience beside another",
            &[PODS_AUDIENCE, "sts.amazonaws.com"],
            true,
        ),
    ];

    for (case, audience, admitted) in rows {
        let token = harness.token_request(
            CLUSTER,
            ServiceAccountTokenRequest::for_pod(seed.namespace, seed.service_account, seed.pod)
                .for_audience(audience),
        );
        let reply = assume(&harness, CLUSTER, &token).await;
        if *admitted {
            assert_eq!(
                reply.status, 200,
                "case {case:?}: a token carrying the pod identity audience is exchanged, \
                 got {reply:?}"
            );
            continue;
        }
        assert_eq!(
            reply.verdict(),
            "400 InvalidTokenException",
            "case {case:?}: expected the modelled token refusal, got {reply:?}"
        );
        assert_eq!(
            reply.message, expected,
            "case {case:?}: expected {expected:?}, got {:?}",
            reply.message
        );
        // Nothing of the rejected token is derived from: not the token, not the audience
        // it was issued to.
        assert!(
            !reply.body.contains(&token),
            "case {case:?}: the token reached the response {}",
            reply.body
        );
        for issued_to in *audience {
            assert!(
                !reply.body.contains(issued_to),
                "case {case:?}: the rejected audience {issued_to:?} reached the response {}",
                reply.body
            );
        }

        // Repeating the call answers identically: nothing about the refusal is cached or
        // partially accepted.
        let again = assume(&harness, CLUSTER, &token).await;
        assert_eq!(
            again, reply,
            "case {case:?}: the identical call must answer identically, got {again:?}"
        );
    }
}

/// The same rule for the other claim the cluster's verification judges: a token presented
/// before the window it was issued for opened is refused on its `iat` claim, which is
/// judged against the clock and so carries no one admissible value to state.
#[tokio::test]
async fn a_token_presented_before_it_was_issued_is_refused_on_the_iat_claim() {
    let harness = Harness::seeded();
    let seed = SEEDS[0];
    let now = SystemTime::now();
    let expected = invalid_claim_message("iat", None);

    for ahead in [60_u64, 3_600, 86_400] {
        let token = harness.token_request(
            CLUSTER,
            ServiceAccountTokenRequest::for_pod(seed.namespace, seed.service_account, seed.pod)
                .issued_at(now + Duration::from_secs(ahead))
                .expiring_at(now + Duration::from_secs(ahead + 3_600)),
        );
        let reply = assume(&harness, CLUSTER, &token).await;
        assert_eq!(
            reply.verdict(),
            "400 InvalidTokenException",
            "case issued {ahead}s ahead: expected the modelled token refusal, got {reply:?}"
        );
        assert_eq!(
            reply.message, expected,
            "case issued {ahead}s ahead: expected {expected:?}, got {:?}",
            reply.message
        );
    }
}

/// The verdicts a cluster can reach on a token it can read are told apart by their
/// messages: each claim it refuses names that claim, and a value it has no record of
/// issuing reached no claim at all, so it names none.
#[tokio::test]
async fn every_cluster_side_verdict_has_its_own_message() {
    let harness = Harness::seeded();
    let seed = SEEDS[0];
    let now = SystemTime::now();

    let wrong_audience = harness.token_request(
        CLUSTER,
        ServiceAccountTokenRequest::for_pod(seed.namespace, seed.service_account, seed.pod)
            .for_audience(&["sts.amazonaws.com"]),
    );
    let not_yet_issued = harness.token_request(
        CLUSTER,
        ServiceAccountTokenRequest::for_pod(seed.namespace, seed.service_account, seed.pod)
            .issued_at(now + Duration::from_secs(300)),
    );
    let expired = harness.token_request(
        CLUSTER,
        ServiceAccountTokenRequest::for_pod(seed.namespace, seed.service_account, seed.pod)
            .issued_at(now - Duration::from_secs(600))
            .expiring_at(now - Duration::from_secs(60)),
    );
    let unissued = crate::rejection_errors::token_for_key(&harness.signing_key_id(CLUSTER));

    // (case, token, the verdict, whether the message must name a claim)
    let rows: &[(&str, &str, &str, Option<&str>)] = &[
        (
            "a token issued to another audience",
            wrong_audience.as_str(),
            "400 InvalidTokenException",
            Some("aud"),
        ),
        (
            "a token presented before it was issued",
            not_yet_issued.as_str(),
            "400 InvalidTokenException",
            Some("iat"),
        ),
        (
            "a token whose window has closed",
            expired.as_str(),
            "400 ExpiredTokenException",
            None,
        ),
        (
            "a token the cluster has no record of",
            unissued.as_str(),
            "400 InvalidTokenException",
            None,
        ),
    ];

    let mut messages = Vec::new();
    for (case, token, verdict, claim) in rows {
        let reply = assume(&harness, CLUSTER, token).await;
        assert_eq!(
            reply.verdict(),
            *verdict,
            "case {case:?}: expected {verdict}, got {reply:?}"
        );
        match claim {
            Some(claim) => assert!(
                reply.message.contains(&format!("claim value for {claim}.")),
                "case {case:?}: the refusal must name the {claim:?} claim, got {:?}",
                reply.message
            ),
            None => assert!(
                !reply.message.contains("claim value for"),
                "case {case:?}: no claim failed, so none may be named, got {:?}",
                reply.message
            ),
        }
        messages.push(reply.message);
    }

    let mut distinct = messages.clone();
    distinct.sort();
    distinct.dedup();
    assert_eq!(
        distinct.len(),
        messages.len(),
        "each verdict must be told apart by its message, got {messages:?}"
    );
}
