// SPDX-License-Identifier: CC0-1.0

//! A CloudFront control-plane read of a distribution identifier, on the ASSEMBLED emulator.
//!
//! RULE: a request whose credential scope names CloudFront and whose path is a modelled
//! control-plane route is served by that modelled operation on the emulator as it is assembled
//! — every service mounted, as `aws_emulator_router::build_router` assembles the one the binary
//! serves — and never by another plane of the same service or by another service that happens
//! to claim the host. So `GET /2020-05-31/distribution/{Id}/config` answers the operation's own
//! modelled refusal: 404 `NoSuchDistribution`, under the sentence that tells an identifier
//! CloudFront cannot READ ("Id format is invalid", for a value written outside the alphabet
//! CloudFront writes an entity identifier in) from a well-formed identifier the account holds
//! no distribution under ("The specified distribution does not exist."), and never the front
//! door's unknown-action refusal.
//!
//! The rows span what the rule generalises over: the two refusal classes, values written
//! outside the identifier alphabet in several ways, the values this account's own distribution
//! carries (its published domain name and its ARN, each submitted the way a client puts a path
//! label on the wire) and — because the data plane of this same service claims a request by its
//! HOST — whether the account already holds a distribution whose edge domain the emulator
//! published when the read arrives.
//!
//! This lives here rather than in the CloudFront crate because what it pins is the ASSEMBLED
//! service set: the crate's own suites mount CloudFront alone and cannot see another mounted
//! service, or another plane, claiming the path.

use std::sync::Arc;

use aws_emulator_router::build_router;
use smithy_server_runtime::MultiServiceRouter;

use crate::cloudfront_oac_s3_origin_edge::{
    call, create_bucket, create_distribution, create_origin_access_control, API_VERSION,
    CLOUDFRONT_HOST,
};

/// The AWS-managed cache policy `Managed-CachingDisabled`, named on the distribution's default
/// behaviour so the create needs nothing else configured.
const CACHING_DISABLED: &str = "4135ea2d-6df8-44a3-9df3-4b5a84be39ad";

/// The bucket the created distribution's origin names.
const BUCKET: &str = "assembled-identifier-refusal-origin";

/// The code every refusal below carries.
const NO_SUCH_DISTRIBUTION: &str = "NoSuchDistribution";

/// The sentence a well-formed identifier the account holds no distribution under earns.
const ABSENT: &str = "The specified distribution does not exist.";

/// The sentence an identifier written outside the alphabet CloudFront writes an entity
/// identifier in earns, naming the model's own member.
const UNREADABLE: &str = "Id format is invalid";

/// `GET /2020-05-31/distribution/{id}/config` on the assembled emulator, addressed to the
/// CloudFront control-plane host and carrying CloudFront's signing name in its credential
/// scope — which is what routes it.
async fn read_config(router: &MultiServiceRouter, id: &str) -> (u16, String) {
    let answered = call(
        router,
        "cloudfront",
        CLOUDFRONT_HOST,
        "GET",
        &format!("/{API_VERSION}/distribution/{id}/config"),
        &[],
        bytes::Bytes::new(),
    )
    .await;
    (answered.status, answered.text())
}

/// The `<Code>` and `<Message>` of a restXml error envelope, as this suite reads them: the
/// envelope's own elements, so an answer carrying neither reads as empty strings.
fn refusal(body: &str) -> (String, String) {
    let element = |name: &str| {
        let open = format!("<{name}>");
        let close = format!("</{name}>");
        body.split_once(&open)
            .and_then(|(_, rest)| rest.split_once(&close))
            .map(|(value, _)| value.to_string())
            .unwrap_or_default()
    };
    (element("Code"), element("Message"))
}

/// Every identifier this suite reads, with the sentence it earns.
fn rows() -> Vec<(&'static str, &'static str, &'static str)> {
    vec![
        (
            "an identifier of the alphabet the account holds no distribution under",
            "E1ABCDEFGHIJKL",
            ABSENT,
        ),
        (
            "a UUID, the namespace the policy kinds are keyed by",
            "00000000-0000-0000-0000-000000000000",
            UNREADABLE,
        ),
        (
            "an identifier of the alphabet in lower case",
            "e1abcdefghijkl",
            UNREADABLE,
        ),
        ("a bare word", "not-an-identifier", UNREADABLE),
    ]
}

#[tokio::test]
async fn a_distribution_config_read_answers_its_own_refusal_on_the_assembled_emulator() {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = build_router(Arc::clone(&engine));
    router.set_request_authentication(false);

    for (case, id, expected) in rows() {
        let (status, body) = read_config(&router, id).await;
        let (code, message) = refusal(&body);
        assert_eq!(
            (status, code.as_str()),
            (404, NO_SUCH_DISTRIBUTION),
            "case {case:?}: expected the operation's own 404 {NO_SUCH_DISTRIBUTION}, got \
             {status} with {body:?}"
        );
        assert_eq!(
            message, expected,
            "case {case:?}: expected the sentence {expected:?}, got {message:?}"
        );
    }
}

#[tokio::test]
async fn the_read_keeps_its_refusal_once_the_account_holds_a_published_distribution() {
    // The counter-case to the rule's second half: this service's own data plane claims a
    // request by its HOST, and a created distribution publishes an edge domain, so the read is
    // driven again with one in the store — the control-plane host still routes to the modelled
    // operation.
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = build_router(Arc::clone(&engine));
    router.set_request_authentication(false);

    let case = "a published distribution beside the read";
    create_bucket(&router, case, BUCKET).await;
    let control = create_origin_access_control(&router, case, "oac-assembled-refusal", "always")
        .await;
    let (held, domain, arn) = create_distribution(
        &router,
        case,
        "assembled-identifier-refusal",
        BUCKET,
        &control,
        CACHING_DISABLED,
    )
    .await;

    for (case, id, expected) in rows() {
        let (status, body) = read_config(&router, id).await;
        let (code, message) = refusal(&body);
        assert_eq!(
            (status, code.as_str()),
            (404, NO_SUCH_DISTRIBUTION),
            "case {case:?}: expected the operation's own 404 {NO_SUCH_DISTRIBUTION}, got \
             {status} with {body:?}"
        );
        assert_eq!(
            message, expected,
            "case {case:?}: expected the sentence {expected:?}, got {message:?}"
        );
    }

    // The same rule over the values a client is most likely to paste in place of an id: the
    // ones this account's own distribution carries. Each is written outside the identifier
    // alphabet, so each earns the unreadable-identifier sentence — and each is submitted the
    // way a client puts a path label on the wire, with the characters a path segment does not
    // carry percent-encoded, which is how a value spanning several segments still reaches the
    // operation as ONE identifier.
    for (case, id) in [
        (
            "the distribution's own published domain name",
            domain.replace('/', "%2F"),
        ),
        ("the distribution's own ARN", arn.replace('/', "%2F")),
    ] {
        let (status, body) = read_config(&router, &id).await;
        let (code, message) = refusal(&body);
        assert_eq!(
            (status, code.as_str(), message.as_str()),
            (404, NO_SUCH_DISTRIBUTION, UNREADABLE),
            "case {case:?} ({id:?}): expected the unreadable-identifier refusal, got {status} \
             with {body:?}"
        );
    }

    // And the identifier the distribution IS held under is still served by the same route.
    let (status, body) = read_config(&router, &held).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the held distribution's configuration, got {status} with \
         {body:?}"
    );
}
