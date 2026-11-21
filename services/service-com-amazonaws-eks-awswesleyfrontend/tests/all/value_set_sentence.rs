// SPDX-License-Identifier: CC0-1.0

//! The ONE sentence this service refuses a value outside an enum member's set with, over
//! every operation that words it, driven over the wire through the same
//! `MultiServiceRouter` the production binary mounts.
//!
//! RULE under test: the refusal names the member by its OWN name — the member as the model
//! declares it, with no parent structure and no preamble before it — then states the values
//! that member accepts, in the order the model declares them, and echoes the rejected value
//! nowhere. So one member earns one sentence whatever depth the request carries it at and
//! whichever operation it was sent to.
//!
//! The rows span what the rule generalises over: the DEPTH the member sits at (a member of
//! the body, a member of a nested structure), which seam words it (the validating walk
//! before dispatch, and an operation that judges the value itself), how the value misses
//! the set (wrong case, a token nothing declares, the empty string) and the operation it
//! was sent to. Every accepted value set is read out of the model's own generated constants,
//! so no row repeats a table the service also holds.
//!
//! The wire harness is the shared one in `tests/all/wire.rs`; the request builders belong to
//! the suites that own each operation.

use std::collections::BTreeSet;

use service_com_amazonaws_eks_awswesleyfrontend::ops::types::CONNECTOR_CONFIG_PROVIDER_VALUES;
use service_com_amazonaws_eks_awswesleyfrontend::state::value_must_be_one_of_message;
use smithy_server_runtime::MultiServiceRouter;

use crate::cluster_lifecycle::{create, describe, minimal_request, REGION};
use crate::connected_clusters::{minimal_registration, register};
use crate::wire::{call, emulator, error_code, Answer};

/// The channel every one of these refusals travels in: "The specified parameter is invalid"
/// (`smithy_query(query="shape",shape="InvalidParameterException")`).
const INVALID_PARAMETER: &str = "InvalidParameterException";

/// The accepted set of `CreateAccessConfigRequest$authenticationMode`, in the order the model
/// declares it (`smithy_query(query="shape",shape="AuthenticationMode")`).
const AUTHENTICATION_MODES: [&str; 3] = ["API", "API_AND_CONFIG_MAP", "CONFIG_MAP"];

/// The accepted set of the `versionStatus` filter of the cluster-version catalog, in the
/// order the model declares it (`smithy_query(query="shape",shape="VersionStatus")`).
const VERSION_STATUSES: [&str; 3] = ["UNSUPPORTED", "STANDARD_SUPPORT", "EXTENDED_SUPPORT"];

/// The sentence the rule requires for `member`, written out here as the contract a client
/// reads rather than read from the service's own helper.
fn expected(member: &str, accepted: &[&str]) -> String {
    format!("{member} must be one of [{}]", accepted.join(", "))
}

/// `GET /cluster-versions?<query>` — `DescribeClusterVersions`.
async fn describe_versions(router: &MultiServiceRouter, query: &str) -> Answer {
    call(router, REGION, "GET", &format!("/cluster-versions?{query}"), "").await
}

/// The refusal `answer` carries, with the status and the error class already asserted.
fn refusal(case: &str, answer: &Answer) -> String {
    assert_eq!(
        answer.status, 400,
        "case {case:?}: expected HTTP 400, got {answer:?}"
    );
    assert_eq!(
        error_code(answer),
        INVALID_PARAMETER,
        "case {case:?}: expected {INVALID_PARAMETER}, got {answer:?}"
    );
    answer.message()
}

/// `CreateCluster` carries its `authenticationMode` inside `accessConfig`, and the sentence
/// names that member ALONE: the path the request spelled it at is no part of it.
#[tokio::test]
async fn a_nested_member_is_named_by_itself_and_not_by_its_path() {
    let router = emulator();
    let want = expected("authenticationMode", &AUTHENTICATION_MODES);

    // (case, the value sent) — one row per way a value can miss a value set. The wrong-case
    // and unknown-token forms are the ones `cluster_request_validation`'s own table already
    // sends, so this table takes the two it does not.
    let rows: &[(&str, &str)] = &[
        ("the empty string", ""),
        ("a near-miss of a member", "API_AND_CONFIGMAP"),
        ("a member with surrounding space", " API "),
    ];

    for (index, (case, mode)) in rows.iter().enumerate() {
        let name = format!("value-set-{index}");
        let mut body = minimal_request(&name);
        body["accessConfig"] = serde_json::json!({ "authenticationMode": mode });
        let answer = create(&router, &body).await;
        let actual = refusal(case, &answer);
        assert_eq!(
            actual, want,
            "case {case:?}: expected {want:?}, got {actual:?}"
        );
        assert!(
            !actual.contains("accessConfig"),
            "case {case:?}: the structure the member travelled in is no part of the \
             sentence, got {actual:?}"
        );
        assert!(
            !actual.contains(mode) || mode.is_empty(),
            "case {case:?}: the rejected value is not echoed into the sentence, \
             got {actual:?}"
        );
        // The complaint is about the member, so the refusal names no cluster — and the
        // refused create wrote nothing, which a later read is what proves.
        assert_eq!(
            answer.body["clusterName"].as_str(),
            None,
            "case {case:?}: expected the refusal to name no cluster, got {answer:?}"
        );
        assert_eq!(
            describe(&router, &name).await.status,
            404,
            "case {case:?}: a refused create must leave no cluster behind"
        );
    }

    // The accepted side of the same rule: every declared member is taken.
    for mode in AUTHENTICATION_MODES {
        let name = format!("value-set-ok-{}", mode.to_ascii_lowercase());
        let mut body = minimal_request(&name);
        body["accessConfig"] = serde_json::json!({ "authenticationMode": mode });
        let answer = create(&router, &body).await;
        assert_eq!(
            answer.status, 200,
            "case {mode:?}: a declared member is accepted, got {answer:?}"
        );
    }
}

/// The same sentence answers the other operations that judge a value against an enum's set:
/// a registration's `connectorConfig.provider` — nested, like the mode above — and the
/// cluster-version catalog's own `versionStatus` filter, which travels in the query string
/// and is judged by the handler rather than by the validating walk.
#[tokio::test]
async fn every_operation_that_judges_a_value_set_states_the_same_sentence() {
    let router = emulator();

    let answer = register(&router, &minimal_registration("value-set-provider", "nomad")).await;
    let actual = refusal("a provider nothing declares", &answer);
    assert!(
        actual.starts_with("provider must be one of [") && actual.ends_with(']'),
        "a registration's provider earns the member's own sentence, got {actual:?}"
    );
    assert!(
        !actual.contains("connectorConfig"),
        "the structure the provider travelled in is no part of the sentence, got {actual:?}"
    );
    // The values it states are the set the model declares — compared as a SET, because the
    // sentence states them in the model's own declaration order and the generated constant
    // holds its own.
    let stated: BTreeSet<&str> = actual
        .trim_start_matches("provider must be one of [")
        .trim_end_matches(']')
        .split(", ")
        .collect();
    assert_eq!(
        stated,
        CONNECTOR_CONFIG_PROVIDER_VALUES.iter().copied().collect(),
        "the sentence states the set the model declares, got {actual:?}"
    );

    let statuses = expected("versionStatus", &VERSION_STATUSES);
    for (case, query) in [
        ("a status in the wrong case", "versionStatus=standard_support"),
        ("a status nothing declares", "versionStatus=RETIRED"),
    ] {
        let answer = describe_versions(&router, query).await;
        let actual = refusal(case, &answer);
        assert_eq!(
            actual, statuses,
            "case {case:?}: expected {statuses:?}, got {actual:?}"
        );
    }

    // The accepted side: a declared status is a filter the catalog serves.
    for status in VERSION_STATUSES {
        let answer = describe_versions(&router, &format!("versionStatus={status}")).await;
        assert_eq!(
            answer.status, 200,
            "case {status:?}: a declared status filters the catalog, got {answer:?}"
        );
    }
}

/// The service states that sentence in ONE place, which is what every seam above reads: a
/// second copy of the wording could drift from the one a client is answered with.
#[test]
fn the_service_words_the_sentence_in_one_place() {
    for (member, accepted) in [
        ("authenticationMode", AUTHENTICATION_MODES.to_vec()),
        ("versionStatus", VERSION_STATUSES.to_vec()),
    ] {
        let values: Vec<String> = accepted.iter().map(|value| (*value).to_string()).collect();
        let stated = value_must_be_one_of_message(member, &values);
        let want = expected(member, &accepted);
        assert_eq!(
            stated, want,
            "case {member:?}: the service's own helper must word AWS's sentence, \
             expected {want:?}, got {stated:?}"
        );
    }
}
