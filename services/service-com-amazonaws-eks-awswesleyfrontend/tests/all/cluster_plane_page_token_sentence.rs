// SPDX-License-Identifier: CC0-1.0

//! What a listing of the CLUSTER-MANAGEMENT plane answers when the `nextToken` a
//! request carries is not one that listing minted, driven over the wire through the
//! same `MultiServiceRouter` the production binary mounts (`tests/all/wire.rs`).
//!
//! The rule under test: a page token such a listing cannot recognise — one nothing
//! minted, one minted for another listing, or one whose shape it cannot even read — is
//! refused as a malformed parameter (HTTP 400 `InvalidParameterException`) with Amazon
//! EKS's single page-token sentence. That sentence is ENTIRELY FIXED TEXT: it covers a
//! token the service cannot read and a token whose position is gone alike, so it states
//! neither the decoder's reason for giving up nor any value of the request, and it does
//! not change with HOW the token missed or with what else the request carries.
//!
//! The rows span the dimensions the rule generalises over, rather than the one shape a
//! client that sends a stale or garbled token happens to send:
//!
//! * the FORM of the token — not hexadecimal at all, hexadecimal too short to carry a
//!   position, odd-length hexadecimal, hexadecimal bytes nothing minted, a long value,
//!   the empty token, a token this listing really minted with one digit changed or one
//!   digit missing, and a well-formed token minted for another listing;
//! * the REST of the request it travels in — nothing else, the page size at each end of
//!   the modelled range (`maxResults` is `@range {min: 1, max: 100}`), and the
//!   projection `include` names;
//! * the LISTING addressed — one bound region-wide and two bound on a cluster.
//!
//! The boundary of the rule is asserted as a control: a token a listing DID mint
//! resumes it, and the pages it walks enumerate every name exactly once, so no refusal
//! below is an artefact of pagination being broken.

use service_com_amazonaws_eks_awswesleyfrontend::state::{
    encode_scoped_page_token, ADDONS_LISTING, INVALID_NEXT_TOKEN_MESSAGE,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::cluster_lifecycle::{create, minimal_request, REGION};
use crate::wire::{call, emulator, encode_label, error_code, Answer};

/// Amazon EKS's page-token sentence, written out here rather than only imported, so a
/// change to the service's own constant cannot keep this suite green; the constant is
/// tied to this same text once, at the end of the first test.
const EXPECTED_SENTENCE: &str = "nextToken is not valid or expired.";

/// The class and status such a refusal carries: a value the caller handed in that this
/// service cannot decode is a malformed PARAMETER, not a state-dependent refusal.
const MALFORMED_PARAMETER: &str = "InvalidParameterException";
const MALFORMED_PARAMETER_STATUS: u16 = 400;

/// `GET <path>` with `query` as its query string (empty for none).
async fn listed(router: &MultiServiceRouter, path: &str, query: &str) -> Answer {
    let uri = if query.is_empty() {
        path.to_string()
    } else {
        format!("{path}?{query}")
    };
    call(router, REGION, "GET", &uri, "").await
}

/// The query string carrying `token` as the `nextToken` member, after everything in
/// `rest`.
fn with_token(rest: &str, token: &str) -> String {
    let encoded = encode_label(token);
    if rest.is_empty() {
        format!("nextToken={encoded}")
    } else {
        format!("{rest}&nextToken={encoded}")
    }
}

/// The cluster names one page of `ListClusters` reports.
fn names(answer: &Answer) -> Vec<String> {
    answer.body["clusters"]
        .as_array()
        .map(|values| {
            values
                .iter()
                .filter_map(|value| value.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

/// Every way the one sentence is fixed text: the class, the status, the wording, and
/// that nothing of the request — the token, a member name, or the decoder's reason —
/// reaches it.
fn assert_fixed_sentence(case: &str, token: &str, answer: &Answer) {
    assert_eq!(
        (answer.status, error_code(answer).as_str()),
        (MALFORMED_PARAMETER_STATUS, MALFORMED_PARAMETER),
        "case {case:?}: a token the listing did not mint is a malformed parameter, got {answer:?}"
    );
    let reported = answer.message();
    assert_eq!(
        reported, EXPECTED_SENTENCE,
        "case {case:?}: expected Amazon EKS's page-token sentence, got {reported:?}"
    );
    assert!(
        token.is_empty() || !reported.contains(token),
        "case {case:?}: the sentence must not echo the rejected token, got {reported:?}"
    );
    // Neither a member the request carries nor the decoder's own failure mode is part
    // of this sentence: a caller cannot tell from it whether the token was unreadable
    // or merely stale, which is exactly what it covers.
    for forbidden in ["maxResults", "include", "format", "clusters"] {
        assert!(
            !reported.contains(forbidden),
            "case {case:?}: the sentence must not mention {forbidden:?}, got {reported:?}"
        );
    }
}

#[tokio::test]
async fn an_unrecognised_page_token_is_refused_in_one_fixed_sentence_whatever_its_form() {
    let router = emulator();
    // Two clusters, so the listing has a page boundary to mint a real token at — and
    // two rows below can start from a token it really issued.
    let standing = ["token-form-one", "token-form-two"];
    for name in standing {
        let created = create(&router, &minimal_request(name)).await;
        assert_eq!(
            created.status, 200,
            "setup: expected 200 from CreateCluster for {name:?}, got {created:?}"
        );
    }

    // CONTROL, and the boundary of the rule: a token this listing minted RESUMES it,
    // and the two pages together report both clusters exactly once, in order, with no
    // token left on the last one.
    let first = listed(&router, "/clusters", "maxResults=1").await;
    assert_eq!(
        first.status, 200,
        "control: expected a served first page, got {first:?}"
    );
    let minted = first.body["nextToken"]
        .as_str()
        .unwrap_or_else(|| {
            panic!("control: a one-cluster page must leave a nextToken, got {first:?}")
        })
        .to_owned();
    let second = listed(
        &router,
        "/clusters",
        &format!("maxResults=1&nextToken={minted}"),
    )
    .await;
    assert_eq!(
        second.status, 200,
        "control: a token this listing minted must resume it, got {second:?}"
    );
    let walked: Vec<String> = names(&first).into_iter().chain(names(&second)).collect();
    assert_eq!(
        walked,
        standing.map(str::to_owned).to_vec(),
        "control: the pages must enumerate every cluster once, got {walked:?}"
    );
    assert!(
        second.body["nextToken"].is_null(),
        "control: the last page must leave no token, got {second:?}"
    );

    // The FORM dimension. The last two rows start from the token above, so a token that
    // is one digit away from a real one is covered too; the last one is a token another
    // listing of this service really mints, in its own scope (the add-on listing's,
    // `state::ADDONS_LISTING`).
    let truncated = minted[..minted.len() - 1].to_owned();
    let flipped = {
        let mut token = minted.clone();
        let last = token.pop().expect("the minted token is not empty");
        token.push(if last == '0' { '1' } else { '0' });
        token
    };
    let tokens: Vec<(&str, String)> = vec![
        ("a value that is not hexadecimal", "not-a-token".to_owned()),
        (
            "a value carrying characters a query string must escape",
            "%%% not a real token %%%".to_owned(),
        ),
        ("odd-length hexadecimal", "abc".to_owned()),
        (
            "hexadecimal too short to carry a position",
            "deadbeef".to_owned(),
        ),
        (
            "hexadecimal bytes nothing minted",
            "deadbeefdeadbeefdeadbeef".to_owned(),
        ),
        ("a long value nothing minted", "9".repeat(512)),
        ("the empty token", String::new()),
        ("a minted token with one digit missing", truncated),
        ("a minted token with one digit changed", flipped),
        (
            "a well-formed token minted for another listing",
            encode_scoped_page_token(ADDONS_LISTING, "vpc-cni"),
        ),
    ];

    // The REST-OF-THE-REQUEST dimension: nothing else, each end of the modelled page
    // range, and the projection `include` names.
    let rest = [
        "",
        "maxResults=1",
        "maxResults=100",
        "include=all",
        "maxResults=1&include=all",
    ];

    for (form, token) in &tokens {
        for carried in rest {
            let case = format!(
                "{form} / alongside {}",
                if carried.is_empty() {
                    "nothing else"
                } else {
                    carried
                }
            );
            let answer = listed(&router, "/clusters", &with_token(carried, token)).await;
            assert_fixed_sentence(&case, token, &answer);
        }
    }

    assert_eq!(
        INVALID_NEXT_TOKEN_MESSAGE, EXPECTED_SENTENCE,
        "the cluster-management listings must refuse an unrecognised token in Amazon EKS's \
         page-token sentence, got {INVALID_NEXT_TOKEN_MESSAGE:?}"
    );
}

#[tokio::test]
async fn every_listing_of_this_plane_refuses_such_a_token_in_the_same_sentence() {
    const CLUSTER: &str = "token-plane-scope";
    let router = emulator();
    let created = create(&router, &minimal_request(CLUSTER)).await;
    assert_eq!(
        created.status, 200,
        "setup: expected 200 from CreateCluster, got {created:?}"
    );

    // One listing bound region-wide and two bound on a cluster, at the paths the model
    // binds them to (`smithy_query query="operation"
    // operation="ListClusters,ListFargateProfiles,ListIdentityProviderConfigs"`).
    let listings: Vec<(&str, String)> = vec![
        ("ListClusters", "/clusters".to_owned()),
        (
            "ListFargateProfiles",
            format!("/clusters/{}/fargate-profiles", encode_label(CLUSTER)),
        ),
        (
            "ListIdentityProviderConfigs",
            format!(
                "/clusters/{}/identity-provider-configs",
                encode_label(CLUSTER)
            ),
        ),
    ];

    let tokens: Vec<(&str, String)> = vec![
        ("a value that is not hexadecimal", "not-a-token".to_owned()),
        (
            "hexadecimal bytes nothing minted",
            "deadbeefdeadbeefdeadbeef".to_owned(),
        ),
        ("the empty token", String::new()),
        (
            "a well-formed token minted for another listing",
            encode_scoped_page_token(ADDONS_LISTING, "vpc-cni"),
        ),
    ];

    for (operation, path) in &listings {
        // CONTROL: the untokened listing is served, so what the rows below read is the
        // refusal of the token they carry.
        let served = listed(&router, path, "").await;
        assert_eq!(
            served.status, 200,
            "control {operation:?}: the untokened listing must be served, got {served:?}"
        );
        for (form, token) in &tokens {
            let case = format!("{operation} / {form}");
            let answer = listed(&router, path, &with_token("", token)).await;
            assert_fixed_sentence(&case, token, &answer);
        }
    }
}
