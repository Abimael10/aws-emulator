// SPDX-License-Identifier: CC0-1.0

//! What this service answers a request that MINTS a cluster record under a `name`
//! member of its body — `CreateCluster` and `RegisterCluster` — whose parameters it
//! refuses, driven over the wire through the same `MultiServiceRouter` the production
//! binary mounts.
//!
//! Two rules, each asserted over a TABLE whose rows differ along what the rule
//! generalises over:
//!
//! * a member this API requires and the request omitted is refused with the sentence
//!   Amazon EKS publishes for THAT member — the general one for a member it states none
//!   of its own for, and that member's own sentence where it states one;
//! * a refusal of such a request's own parameters carries, beside the sentence, the
//!   `clusterName` the request asked for ("The Amazon EKS cluster associated with the
//!   exception", `smithy_query(query="shape", shape="InvalidParameterException")`), so a
//!   caller that submitted several registrations can attribute the failure — and names
//!   none when the request named none.
//!
//! The third rule these refusals obey — the sentence a value outside the set an enum
//! member is judged by earns, which names that member by its path from the request root
//! — is asserted where each such member already has its table: `connected_clusters` for
//! a registration's `connectorConfig.provider`, and `cluster_request_validation` for a
//! cluster's `accessConfig.authenticationMode` and its control-plane log types.
//!
//! The sentences are written out here, as the contract a client reads, rather than read
//! back out of the module that states them. The wire harness (`Answer`, `emulator`,
//! `call`) is the shared one in `tests/all/wire.rs`; the `CreateCluster` request builder
//! is `cluster_lifecycle`'s and the registration request builder `connected_clusters`'.

use crate::cluster_lifecycle::{create, describe, minimal_request, REGION};
use crate::connected_clusters::register;
use crate::wire::{call, emulator, error_code, Answer};

/// The error this API states a bad parameter with (`httpError` 400 on
/// `InvalidParameterException`, which both operations declare).
const INVALID_PARAMETER: &str = "InvalidParameterException";

/// The smallest request `RegisterCluster` accepts, built by the suite that owns the
/// registration request (`connected_clusters`): its two required members
/// (`smithy_query(query="operation", operation="RegisterCluster")` reports `name` and
/// `connectorConfig` required). The provider is immaterial to every row here, which is
/// refused for the name or for an omitted member.
fn minimal_registration(name: &str) -> serde_json::Value {
    crate::connected_clusters::minimal_registration(name, "OTHER")
}

/// The same body with one member removed, which is how a row states "the request
/// omitted this member".
fn without(body: &serde_json::Value, member: &str) -> serde_json::Value {
    let mut body = body.clone();
    body.as_object_mut()
        .expect("a request body is an object")
        .remove(member);
    body
}

/// The sentence a cluster-name member outside the cluster-name grammar earns, as the
/// service prints it.
fn invalid_characters(member: &str) -> String {
    format!(
        "The parameter {member} contains invalid characters. It should conform to the \
         regular expression \"^[0-9A-Za-z][A-Za-z0-9-_]*\"In other words, it should start \
         with a number or letter, and can contain numbers, letters, dashes, and \
         underscores."
    )
}

/// The `clusterName` member a refusal carries, or `None` when it names no cluster.
fn cluster_name_of(answer: &Answer) -> Option<&str> {
    answer.body["clusterName"].as_str()
}

/// Assert `answer` is a 400 `InvalidParameterException` carrying `message`.
fn assert_refused_with(case: &str, answer: &Answer, message: &str) {
    assert_eq!(
        answer.status, 400,
        "case {case:?}: expected HTTP 400, got {answer:?}"
    );
    assert_eq!(
        error_code(answer),
        INVALID_PARAMETER,
        "case {case:?}: expected {INVALID_PARAMETER}, got {answer:?}"
    );
    assert_eq!(
        answer.message(),
        message,
        "case {case:?}: expected the sentence this API states, got {:?}",
        answer.message()
    );
}

// ---------------------------------------------------------------------------
// RULE: a member this API requires and the request omitted is refused with the
// sentence Amazon EKS publishes for THAT member — the general one, or that
// member's own where it states one.
// ---------------------------------------------------------------------------

/// The rows differ along which member was omitted, which operation it was omitted
/// from, and whether that member has a sentence of its own: `connectorConfig`'s calls
/// it a field and ends without a full stop, `roleArn`'s and `resourcesVpcConfig`'s name
/// the parameter in their own words, and `name` earns the general template on both
/// operations.
#[tokio::test]
async fn an_omitted_required_member_earns_that_members_own_sentence() {
    let router = emulator();

    // (case, the URI, the body, the sentence)
    let rows: Vec<(&str, &str, serde_json::Value, String)> = vec![
        (
            "a registration without its connector configuration",
            "/cluster-registrations",
            without(&minimal_registration("omitted-connector-config"), "connectorConfig"),
            "Field connectorConfig is required".to_string(),
        ),
        (
            "a registration without a name",
            "/cluster-registrations",
            without(&minimal_registration("unused"), "name"),
            "The parameter name is required.".to_string(),
        ),
        (
            "a create without a name",
            "/clusters",
            without(&minimal_request("unused"), "name"),
            "The parameter name is required.".to_string(),
        ),
        (
            "a create without its cluster role",
            "/clusters",
            without(&minimal_request("omitted-role-arn"), "roleArn"),
            "The roleArn parameter is required".to_string(),
        ),
        (
            "a create without its VPC configuration",
            "/clusters",
            without(&minimal_request("omitted-vpc-config"), "resourcesVpcConfig"),
            "Subnet Id is required".to_string(),
        ),
    ];

    for (case, uri, body, message) in rows {
        let answer = call(&router, REGION, "POST", uri, &body.to_string()).await;
        assert_refused_with(case, &answer, &message);
        // The sentence this API states for an omitted member, and never the validation
        // framework's constrained-leaf report of it.
        assert!(
            !answer.message().contains("failed to satisfy constraint"),
            "case {case:?}: expected this API's own sentence, got {answer:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// RULE: a refusal of a parameter of a request that names its cluster in the
// BODY carries that `clusterName` beside the sentence — and names none when the
// request named none.
// ---------------------------------------------------------------------------

/// The rows differ along HOW the submitted name breaks the cluster-name grammar
/// (`ClusterName`'s `@pattern ^[0-9A-Za-z][A-Za-z0-9\-_]*$`) and over both operations
/// that take the name in their body, so the echo is asserted for the whole class and
/// not for one rejected spelling.
#[tokio::test]
async fn a_refused_parameter_names_the_cluster_the_request_asked_for() {
    let router = emulator();

    // Names outside the grammar: each breaks it a different way — a leading character
    // the grammar's first class excludes, and a character its tail class excludes.
    let refused_names = [
        ("a leading hyphen", "-probe-registration"),
        ("a leading underscore", "_probe-registration"),
        ("a leading period", ".probe-registration"),
        ("a space inside the name", "probe registration"),
        ("a period inside the name", "probe.registration"),
        ("a colon inside the name", "probe:registration"),
    ];

    for (case, name) in refused_names {
        let registration = register(&router, &minimal_registration(name)).await;
        assert_refused_with(
            &format!("a registration with {case}"),
            &registration,
            &invalid_characters("name"),
        );
        assert_eq!(
            cluster_name_of(&registration),
            Some(name),
            "case {case:?}: a registration's refusal must echo the submitted name, got \
             {registration:?}"
        );

        let created = create(&router, &minimal_request(name)).await;
        assert_refused_with(
            &format!("a create with {case}"),
            &created,
            &invalid_characters("name"),
        );
        assert_eq!(
            cluster_name_of(&created),
            Some(name),
            "case {case:?}: a create's refusal must echo the submitted name, got {created:?}"
        );

        // And nothing a refused call asked for was stored: the parameter is judged
        // before any record is written. A read addressed to such a name is refused for
        // the name itself — no cluster of this API can carry it — so what is asserted
        // here is that no record answers it.
        let described = describe(&router, name).await;
        assert_ne!(
            described.status, 200,
            "case {case:?}: expected no cluster to have been stored, got {described:?}"
        );
    }

    // A name longer than the published maximum is the same class, reported with the
    // length sentence, and still echoes what was submitted.
    let too_long = "a".repeat(101);
    let registration = register(&router, &minimal_registration(&too_long)).await;
    assert_refused_with(
        "a registration whose name is past the maximum",
        &registration,
        "The parameter name should not be greater than 100 characters.",
    );
    assert_eq!(
        cluster_name_of(&registration),
        Some(too_long.as_str()),
        "case \"a name past the maximum\": expected the submitted name echoed, got \
         {registration:?}"
    );

    // The other side of the rule: a request that named no cluster names none in its
    // refusal, on both operations.
    for (case, uri, body) in [
        (
            "a registration that named no cluster",
            "/cluster-registrations",
            without(&minimal_registration("unused"), "name"),
        ),
        (
            "a create that named no cluster",
            "/clusters",
            without(&minimal_request("unused"), "name"),
        ),
    ] {
        let answer = call(&router, REGION, "POST", uri, &body.to_string()).await;
        assert_refused_with(case, &answer, "The parameter name is required.");
        assert_eq!(
            cluster_name_of(&answer),
            None,
            "case {case:?}: a request that named no cluster names none in its refusal, got \
             {answer:?}"
        );
    }
}
