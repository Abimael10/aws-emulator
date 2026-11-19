// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for WHICH refusal a `NetworkInsightsPathId` that resolves to no path of
//! the account is answered with, driven over the wire the service actually speaks: `ec2Query`
//! — an `application/x-www-form-urlencoded` POST answered with XML (README.md /
//! OPERATIONS.md "Wire protocol").
//!
//! The rule under test: which of this surface's two refusals EC2 answers is a fact of the
//! VALUE and not of the operation that resolved it. A value EC2 could never have issued a
//! path id as — one outside the two published widths, one whose long form does not lead with
//! `0`, or the all-zero placeholder — is the parameter being wrong: HTTP 400
//! `InvalidParameterValue` with "NetworkInsightsPathId (<value>) is not valid". An id in a
//! form EC2 does issue names a path that is simply not there: HTTP 400
//! `InvalidNetworkInsightsPathId.NotFound` with "<id> does not exist". Both operations that
//! resolve the ONE path they are given — `StartNetworkInsightsAnalysis` and
//! `DeleteNetworkInsightsPath` — answer one value the same way, and the describes, which
//! SELECT paths with a LIST of ids at the request boundary, keep the resource's own not-found
//! code for every value.
//!
//! The table rows are distinct inputs along what the rule generalises over: the id's width
//! (both published forms), its payload, and each way a value can fall outside the form EC2
//! issues a path id in. The last test takes the id out of the service's own mint instead, so
//! the not-found half is proven on an id this emulator really issued rather than on one
//! written here: a path is created over the wire, analyzed, deleted, and then named again.

use ec2::state::{INVALID_NETWORK_INSIGHTS_PATH_ID_NOT_FOUND, INVALID_PARAMETER_VALUE};
use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{call, element, error_code, error_message};
use super::wire::{param, Param};

/// The Region every call here is signed for. None of the rules depends on it: each refusal is
/// about the value the request carried.
const REGION: &str = "us-east-1";

/// The HTTP status EC2 answers a refusal about the request with.
const REFUSAL_STATUS: u16 = 400;

/// One `action` call carrying `params`, as its HTTP status and XML body.
async fn api(router: &MultiServiceRouter, action: &str, params: &[Param]) -> (u16, String) {
    call(router, REGION, action, params).await
}

/// One `action` call that has to succeed, and the body it answered.
async fn ok(case: &str, router: &MultiServiceRouter, action: &str, params: &[Param]) -> String {
    let (status, body) = api(router, action, params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from {action}, got {status} with body {body}"
    );
    body
}

/// The form fields naming `path_id` to each operation that resolves the ONE path it is given:
/// the start of an analysis (whose `ClientToken` the model declares `@required`, distinct per
/// call so no call is answered as a replay of another) and the delete.
fn acting_calls(path_id: &str, token: &str) -> Vec<(&'static str, Vec<Param>)> {
    vec![
        (
            "StartNetworkInsightsAnalysis",
            vec![
                param("NetworkInsightsPathId", path_id),
                param("ClientToken", token),
            ],
        ),
        (
            "DeleteNetworkInsightsPath",
            vec![param("NetworkInsightsPathId", path_id)],
        ),
    ]
}

/// Assert one refusal: the status, the code and the whole sentence.
async fn assert_refusal(
    case: &str,
    router: &MultiServiceRouter,
    action: &str,
    params: &[Param],
    expected_code: &str,
    expected_message: &str,
) {
    let (status, body) = api(router, action, params).await;
    assert_eq!(
        status, REFUSAL_STATUS,
        "case {case:?}: expected HTTP {REFUSAL_STATUS} from {action}, got {status} with body \
         {body}"
    );
    let code = error_code(&body);
    assert_eq!(
        code, expected_code,
        "case {case:?}: expected the code {expected_code:?} from {action}, got {code:?} in \
         {body}"
    );
    let message = error_message(&body);
    assert_eq!(
        message, expected_message,
        "case {case:?}: expected the message {expected_message:?} from {action}, got \
         {message:?}"
    );
}

/// Path ids in a form EC2 DOES issue that the account holds no path under: both published
/// widths and three long-form payloads, so no one id is the specification.
const IN_THE_ISSUED_FORM: [(&str, &str); 5] = [
    ("the long id form", "nip-0123456789abcdef0"),
    ("another long-form payload", "nip-0fedcba987654321f"),
    (
        "a long form of nothing but zeros after the lead",
        "nip-0000000000000000f",
    ),
    ("the short id form", "nip-0a1b2c3d"),
    ("another short-form payload", "nip-fedcba98"),
];

/// Values EC2 could never have issued a path id as, each outside that form for a different
/// reason: the all-zero placeholder in either published width, a suffix of neither width, a
/// long form whose leading digit no issued id carries, an upper-case suffix, a
/// non-hexadecimal suffix, another resource kind's id, free text, the prefix alone, and a
/// value carrying the characters that are significant in the XML envelope the refusal is
/// written into.
const OUTSIDE_THE_ISSUED_FORM: [(&str, &str); 11] = [
    ("the all-zero long form", "nip-00000000000000000"),
    ("the all-zero short form", "nip-00000000"),
    ("a suffix of neither width", "nip-0000000000000000"),
    (
        "a long form whose leading digit no issued id carries",
        "nip-fffffffffffffffff",
    ),
    ("an upper-case suffix", "nip-0ABCDEF01"),
    ("a non-hexadecimal suffix", "nip-0zzzzzzz"),
    ("the analysis resource's id", "nia-0123456789abcdef0"),
    ("another resource kind's id", "vpc-0123456789abcdef0"),
    ("free text", "not-a-network-insights-path"),
    ("the prefix with no digits at all", "nip-"),
    ("XML metacharacters", "nip-<tag>&'\"end"),
];

/// An id in a form EC2 issues that names no path of the account is the path resource's own
/// not-found code, with the id followed by the fixed text — from BOTH operations that resolve
/// the one path they are given, so the sentence belongs to the id and not to one handler.
#[tokio::test]
async fn an_id_in_the_issued_form_that_names_no_path_is_the_resources_own_not_found() {
    let (router, _engine) = super::ec2_query::emulator();

    for (case, path_id) in IN_THE_ISSUED_FORM {
        for (action, params) in acting_calls(path_id, &format!("token-for-{path_id}")) {
            assert_refusal(
                &format!("{case} ({action})"),
                &router,
                action,
                &params,
                INVALID_NETWORK_INSIGHTS_PATH_ID_NOT_FOUND,
                &format!("{path_id} does not exist"),
            )
            .await;
        }
    }
}

/// A value EC2 could never have issued a path id as is the parameter being wrong, worded with
/// the member that carried it and the value — from BOTH operations that resolve the one path
/// they are given.
#[tokio::test]
async fn a_value_outside_the_issued_form_is_refused_as_a_parameter_value() {
    let (router, _engine) = super::ec2_query::emulator();

    for (case, path_id) in OUTSIDE_THE_ISSUED_FORM {
        for (action, params) in acting_calls(path_id, &format!("token-for-{case}")) {
            assert_refusal(
                &format!("{case} ({action})"),
                &router,
                action,
                &params,
                INVALID_PARAMETER_VALUE,
                &format!("NetworkInsightsPathId ({path_id}) is not valid"),
            )
            .await;
        }
    }
}

/// The DESCRIBES select paths with a LIST of ids at the request boundary, so they keep this
/// resource's own not-found code and sentence for EVERY value that resolves to no path —
/// whichever side of the issued form it falls on. That contrast is what tells a caller which
/// validator refused the call.
#[tokio::test]
async fn the_describes_keep_the_resources_not_found_code_for_every_value() {
    let (router, _engine) = super::ec2_query::emulator();

    for (case, path_id) in IN_THE_ISSUED_FORM
        .into_iter()
        .chain(OUTSIDE_THE_ISSUED_FORM)
    {
        for (action, params) in [
            (
                "DescribeNetworkInsightsPaths",
                vec![param("NetworkInsightsPathId.1", path_id)],
            ),
            (
                "DescribeNetworkInsightsAnalyses",
                vec![param("NetworkInsightsPathId", path_id)],
            ),
        ] {
            assert_refusal(
                &format!("{case} ({action})"),
                &router,
                action,
                &params,
                INVALID_NETWORK_INSIGHTS_PATH_ID_NOT_FOUND,
                &format!("{path_id} does not exist"),
            )
            .await;
        }
    }
}

/// The not-found half on an id this emulator ITSELF issued: a path is created over the wire,
/// an analysis of it is started, the path is deleted, and the very same id then names no path
/// of the account — so both operations that resolve it answer the resource's own not-found
/// code, where before the delete the start succeeded.
#[tokio::test]
async fn a_path_this_service_minted_and_deleted_is_reported_as_not_there() {
    let (router, _engine) = super::ec2_query::emulator();
    let case = "a minted path, deleted";

    let created = ok(
        case,
        &router,
        "CreateNetworkInsightsPath",
        &[
            param("ClientToken", "the-one-path"),
            param("Source", "eni-0123456789abcdef0"),
            param("Destination", "i-0123456789abcdef0"),
            param("Protocol", "tcp"),
        ],
    )
    .await;
    let path_id = element(&created, "networkInsightsPathId")
        .unwrap_or_else(|| panic!("case {case:?}: the create answered no id: {created}"));

    // While the path is held, the start resolves it and the analysis is stored.
    let started = ok(
        case,
        &router,
        "StartNetworkInsightsAnalysis",
        &[
            param("NetworkInsightsPathId", &path_id),
            param("ClientToken", "the-one-analysis"),
        ],
    )
    .await;
    assert!(
        element(&started, "networkInsightsAnalysisId").is_some(),
        "case {case:?}: the start of a held path must answer an analysis, got {started}"
    );

    let deleted = ok(
        case,
        &router,
        "DeleteNetworkInsightsPath",
        &[param("NetworkInsightsPathId", &path_id)],
    )
    .await;
    assert_eq!(
        element(&deleted, "networkInsightsPathId").as_deref(),
        Some(path_id.as_str()),
        "case {case:?}: the delete must answer the id it removed, got {deleted}"
    );

    for (action, params) in acting_calls(&path_id, "after-the-delete") {
        assert_refusal(
            &format!("{case} ({action})"),
            &router,
            action,
            &params,
            INVALID_NETWORK_INSIGHTS_PATH_ID_NOT_FOUND,
            &format!("{path_id} does not exist"),
        )
        .await;
    }
}
