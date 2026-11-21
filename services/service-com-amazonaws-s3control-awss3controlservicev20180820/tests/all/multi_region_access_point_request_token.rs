// SPDX-License-Identifier: CC0-1.0

//! The request token ARN an asynchronous Multi-Region Access Point call hands
//! its caller, driven end to end over HTTP.
//!
//! The rule under test: the token NAMES THE WORK it was handed out for. Its
//! trailing path segment carries the Multi-Region Access Point the request
//! submitted in its `Details`, ahead of a per-request identifier — so a client
//! holding a token can tell which access point the request belongs to — and
//! `DescribeMultiRegionAccessPointOperation` reports that same token back
//! BYTE FOR BYTE, so a poll loop recognises its own request. Two requests that
//! differ in anything they submitted get different tokens, while a
//! byte-identical retry is handed the first call's token (`ClientToken` is an
//! `@idempotencyToken`).
//!
//! The rows differ along what the rule generalises over: the asynchronous call
//! making the request (`create`, `put-policy` and `delete`, the three the model
//! binds under `/async-requests/mrap/`), the length and characters of the access
//! point name (both modelled bounds, digits and hyphens, and a name that is a
//! prefix of another row's), how many buckets the create associates, the
//! idempotency token, and the Region the call is signed for.
//!
//! Every assertion goes through the same front door a client uses: a buffered
//! `http::Request` dispatched by the multi-service router. The harness, the
//! request builder and the XML readers are the sibling suites', reused rather
//! than copied.

use std::collections::BTreeSet;

use crate::access_point_lifecycle::{
    call_in_region, xml_escape, Emulator, Reply, ACCOUNT, REGION, XMLNS,
};
use crate::multi_region_access_point_lifecycle::{
    assert_token_shape, create, describe, leaf, CreateCase,
};
use crate::storage_lens_configuration::leaves_of;

/// A second signing Region: the token names the Region its request was signed
/// for, so a row signed here must not be answered a token naming another.
const OTHER_REGION: &str = "eu-central-1";

/// The longest name the modelled `MultiRegionAccessPointName` admits (`len
/// 0..50`, pattern `^[a-z0-9][-a-z0-9]{1,48}[a-z0-9]$`), and the shortest.
const LONGEST_NAME: &str = "tokennametokennametokennametokennametokenname-50ab";
/// See [`LONGEST_NAME`].
const SHORTEST_NAME: &str = "abc";

/// The leaf path the described request reports its token in.
const DESCRIBED_TOKEN: &str = "AsyncOperation/RequestTokenARN";

/// A policy document `PutMultiRegionAccessPointPolicy` accepts, so the
/// put-policy rows submit a request the service carries out.
const POLICY: &str = r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Principal":{"AWS":"arn:aws:iam::123456789012:root"},"Action":"s3:GetObject","Resource":"*"}]}"#;

/// One asynchronous request to make: the access point it is about, how it is
/// created, and the Region it is signed for.
struct TokenCase {
    case: &'static str,
    name: &'static str,
    client_token: &'static str,
    buckets: &'static [&'static str],
    region: &'static str,
}

/// The rows every assertion below is run over. No row is merely a renaming of
/// another: each differs in the name's length or characters, in how many buckets
/// the create associates, in the idempotency token's characters, or in the
/// Region the call is signed for.
fn token_cases() -> Vec<TokenCase> {
    vec![
        TokenCase {
            case: "the shortest name the model admits",
            name: SHORTEST_NAME,
            client_token: "tok-short",
            buckets: &["token-reports"],
            region: REGION,
        },
        TokenCase {
            case: "the longest name the model admits",
            name: LONGEST_NAME,
            client_token: "tok-long",
            buckets: &["token-reports"],
            region: REGION,
        },
        TokenCase {
            case: "a name of digits and hyphens",
            name: "9-0-token-mrap-2026",
            client_token: "a+b/c=9_%",
            buckets: &["token-reports"],
            region: REGION,
        },
        TokenCase {
            case: "a name another row's name starts with",
            name: "token-mrap",
            client_token: "tok-prefix",
            buckets: &["token-reports", "token-reports-eu"],
            region: REGION,
        },
        TokenCase {
            case: "a name that starts with another row's name",
            name: "token-mrap-wide",
            client_token: "tok-wider",
            buckets: &["token-reports", "token-reports-eu", "token-reports-ap"],
            region: REGION,
        },
        TokenCase {
            case: "signed for another Region",
            name: "eu-token-mrap",
            client_token: "tok-eu",
            buckets: &["token-frankfurt"],
            region: OTHER_REGION,
        },
    ]
}

impl TokenCase {
    /// The create this row's access point is made by.
    fn create_case(&self) -> CreateCase {
        CreateCase {
            region: self.region,
            ..CreateCase::new(self.case, self.name, self.client_token, self.buckets)
        }
    }
}

/// `PutMultiRegionAccessPointPolicy` for `name`.
async fn put_policy(
    emulator: &Emulator,
    region: &str,
    name: &str,
    client_token: &str,
    document: &str,
) -> Reply {
    let body = format!(
        "<PutMultiRegionAccessPointPolicyRequest xmlns=\"{XMLNS}\">\
         <ClientToken>{}</ClientToken><Details><Name>{}</Name><Policy>{}</Policy></Details>\
         </PutMultiRegionAccessPointPolicyRequest>",
        xml_escape(client_token),
        xml_escape(name),
        xml_escape(document)
    );
    call_in_region(
        &emulator.router,
        region,
        "POST",
        "/v20180820/async-requests/mrap/put-policy",
        ACCOUNT,
        &body,
    )
    .await
}

/// `DeleteMultiRegionAccessPoint` for `name`.
async fn delete(emulator: &Emulator, region: &str, name: &str, client_token: &str) -> Reply {
    let body = format!(
        "<DeleteMultiRegionAccessPointRequest xmlns=\"{XMLNS}\">\
         <ClientToken>{}</ClientToken><Details><Name>{}</Name></Details>\
         </DeleteMultiRegionAccessPointRequest>",
        xml_escape(client_token),
        xml_escape(name)
    );
    call_in_region(
        &emulator.router,
        region,
        "POST",
        "/v20180820/async-requests/mrap/delete",
        ACCOUNT,
        &body,
    )
    .await
}

/// The token `reply` answered, which every accepted asynchronous call carries.
fn token_of(case: &str, reply: &Reply) -> String {
    assert_eq!(
        reply.status, 200,
        "case {case:?}: expected the asynchronous call to be accepted with 200, got {} with \
         body {}",
        reply.status, reply.body
    );
    reply.text("RequestTokenARN").unwrap_or_else(|| {
        panic!(
            "case {case:?}: expected a <RequestTokenARN>, got body {}",
            reply.body
        )
    })
}

/// Assert that `token` is the token of a `verb` request on `name` in `region`,
/// and that describing it answers the very same token.
async fn assert_token_names_its_work(
    emulator: &Emulator,
    case: &str,
    token: &str,
    region: &str,
    verb: &str,
    name: &str,
) {
    // The ARN's own layout, and the UUID request id after the name.
    assert_token_shape(case, token, region, verb, name);
    // The access point the request was about is readable straight off the
    // token's trailing segment — the fact a client correlates a token with.
    let trailing = token.rsplit('/').next().unwrap_or_default();
    assert!(
        trailing.starts_with(&format!("{name}-")),
        "case {case:?}: expected the trailing segment to name the access point {name:?}, got \
         {trailing:?} in {token:?}"
    );

    let described = describe(emulator, region, ACCOUNT, token).await;
    assert_eq!(
        described.status, 200,
        "case {case:?}: expected the token to resolve to its request with 200, got {} with \
         body {}",
        described.status, described.body
    );
    let echoed = leaf(&leaves_of(&described.body), DESCRIBED_TOKEN);
    assert_eq!(
        echoed.as_deref(),
        Some(token),
        "case {case:?}: expected the described request to report the token it was asked \
         about byte for byte, got {echoed:?} in body {}",
        described.body
    );
    // The echo does not drift between polls either.
    let polled = describe(emulator, region, ACCOUNT, token).await;
    assert_eq!(
        leaf(&leaves_of(&polled.body), DESCRIBED_TOKEN).as_deref(),
        Some(token),
        "case {case:?}: expected a second poll to report the same token, got body {}",
        polled.body
    );
}

#[tokio::test]
async fn every_async_request_token_names_the_access_point_and_round_trips() {
    assert_eq!(
        LONGEST_NAME.len(),
        50,
        "the longest-name row must sit on the modelled maximum, not near it"
    );
    let emulator = Emulator::start();
    let mut minted: BTreeSet<String> = BTreeSet::new();

    for case in token_cases() {
        // The create, and the token it answers.
        let created = create(&emulator, &case.create_case()).await;
        let token = token_of(case.case, &created);
        assert_token_names_its_work(
            &emulator,
            case.case,
            &token,
            case.region,
            "create",
            case.name,
        )
        .await;
        assert!(
            minted.insert(token.clone()),
            "case {:?}: expected a token no other request was answered, got {token:?}",
            case.case
        );

        // A byte-identical retry is ONE request: the same token, not a second.
        let retried = create(&emulator, &case.create_case()).await;
        assert_eq!(
            token_of(case.case, &retried),
            token,
            "case {:?}: expected a byte-identical retry to be handed the first call's token",
            case.case
        );

        // A create of the same name under another idempotency token is another
        // request, so it is answered another token — which still names the same
        // access point.
        let other = create(
            &emulator,
            &CreateCase {
                client_token: "tok-again",
                ..case.create_case()
            },
        )
        .await;
        let other_token = token_of(case.case, &other);
        assert_ne!(
            other_token, token,
            "case {:?}: expected a second request to be answered its own token",
            case.case
        );
        assert_token_names_its_work(
            &emulator,
            &format!("{}: a second create of the name", case.case),
            &other_token,
            case.region,
            "create",
            case.name,
        )
        .await;
        assert!(
            minted.insert(other_token),
            "case {:?}: expected the second token to be new as well",
            case.case
        );

        // The policy put of that access point: its own verb, the same name.
        let put = put_policy(&emulator, case.region, case.name, "tok-policy", POLICY).await;
        let put_token = token_of(case.case, &put);
        assert_token_names_its_work(
            &emulator,
            &format!("{}: the policy put", case.case),
            &put_token,
            case.region,
            "put-policy",
            case.name,
        )
        .await;
        assert!(
            minted.insert(put_token),
            "case {:?}: expected the policy put's token to be new",
            case.case
        );

        // And the delete, which is a third verb on the same access point.
        let deleted = delete(&emulator, case.region, case.name, "tok-delete").await;
        let delete_token = token_of(case.case, &deleted);
        assert_token_names_its_work(
            &emulator,
            &format!("{}: the delete", case.case),
            &delete_token,
            case.region,
            "delete",
            case.name,
        )
        .await;
        assert!(
            minted.insert(delete_token),
            "case {:?}: expected the delete's token to be new",
            case.case
        );
    }
}
