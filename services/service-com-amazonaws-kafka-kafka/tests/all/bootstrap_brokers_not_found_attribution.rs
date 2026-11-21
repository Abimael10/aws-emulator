// SPDX-License-Identifier: CC0-1.0

//! `GetBootstrapBrokers` ATTRIBUTES a not-found refusal to the input that failed to
//! resolve.
//!
//! RULE: a cluster reference this endpoint holds no cluster for is refused with the
//! service's not-found class — HTTP 404 `NotFoundException`, whose shape carries
//! `InvalidParameter` ("The parameter that caused the error") beside `Message`
//! (`smithy_query(query="shape", shape="com.amazonaws.kafka#NotFoundException")`) — and the
//! refusal NAMES the input member that carried the reference in `invalidParameter`, by the
//! name the wire carries it under (`clusterArn`), with the service's one fixed sentence for
//! a resource it does not hold as the whole message. So a client can attribute the refusal
//! to a specific argument instead of reading a null, whether or not the operation's own
//! `errors` list declares the shape — `GetBootstrapBrokers`'s does not, and the service
//! answers it regardless.
//!
//! The rows differ along what the rule generalises over: WHY the reference resolves to
//! nothing (a name never created, a cluster of another region, a stored cluster's name
//! under an id it does not carry), the FORM of the ARN's id segment (both cluster-type
//! segments this service mints, `-<ordinal>` and `-s2`), and the shape of the cluster name
//! (short, long, and one carrying the kind of text a composed message would echo).
//!
//! The control is the other side of the rule: a cluster this endpoint DOES hold is served
//! at the same URI, so the rows are refused for the reference they carry rather than by a
//! reader that refuses everything.

use crate::kafka_harness::{ClusterRequest, Emulator, ACCOUNT, REGION};

/// What MSK answers an operation for a reference it holds no resource for: fixed text end
/// to end, with nothing of the request in it. The apostrophe is U+2019, as the service
/// writes it. Written out here rather than imported, so the assertion cannot agree with a
/// constant that is wrong.
const RESOURCE_NOT_FOUND: &str = "The requested resource doesn\u{2019}t exist.";

/// The member `GetBootstrapBrokers` carries its cluster reference in, as the wire names it.
const CLUSTER_ARN_MEMBER: &str = "clusterArn";

/// A cluster ARN of the caller's own account in `region`, naming `name` under the id
/// segment `id` — a reference that is well formed and that no create of this suite issued.
fn cluster_arn(region: &str, name: &str, id: &str) -> String {
    format!("arn:aws:kafka:{region}:{ACCOUNT}:cluster/{name}/{id}")
}

/// RULE: `GetBootstrapBrokers` refuses a well-formed cluster reference this endpoint
/// resolves to nothing with 404 `NotFoundException` naming `clusterArn` in
/// `invalidParameter` and the service's fixed sentence as the whole message.
#[tokio::test]
async fn an_unresolvable_cluster_reference_names_the_member_that_carried_it() {
    let emulator = Emulator::new();

    // One cluster this endpoint really holds, so the control below has something to be
    // served and the rows are not refused by an empty registry.
    let held = emulator
        .create_ok(&ClusterRequest::named("attribution-held"))
        .await;

    // `<uuid>-<ordinal>` and `<uuid>-s2` are the two id segments this service mints (a
    // provisioned cluster's ordinal and the serverless marker); both are well-formed
    // references, so both are refused the same way.
    let provisioned_id = "11111111-2222-4333-8444-555555555555-1";
    let serverless_id = "11111111-2222-4333-8444-555555555555-s2";
    let long_name = "a-cluster-name-long-enough-to-exceed-any-alias-truncation-this-service-does";

    let rows = [
        (
            "a name no create asked for",
            cluster_arn(REGION, "attribution-absent", provisioned_id),
        ),
        (
            "the same name under the serverless id segment",
            cluster_arn(REGION, "attribution-absent", serverless_id),
        ),
        (
            "a cluster of another region",
            cluster_arn("eu-central-1", "attribution-held", provisioned_id),
        ),
        (
            "the stored cluster's name under an id it does not carry",
            cluster_arn(REGION, "attribution-held", provisioned_id),
        ),
        (
            "a name a composed message would quote",
            cluster_arn(REGION, "a-name-a-message-would-quote", serverless_id),
        ),
        ("a long name", cluster_arn(REGION, long_name, provisioned_id)),
    ];

    for (case, arn) in &rows {
        let reply = emulator.bootstrap_brokers(arn).await;
        assert_eq!(
            (reply.status, reply.error_code()),
            (404, "NotFoundException"),
            "case {case:?}: expected 404 NotFoundException, got {} {}",
            reply.status,
            reply.body
        );
        assert_eq!(
            reply.at("/invalidParameter").as_str(),
            Some(CLUSTER_ARN_MEMBER),
            "case {case:?}: expected the refusal to name {CLUSTER_ARN_MEMBER}, got {}",
            reply.body
        );
        assert_eq!(
            reply.error_message(),
            RESOURCE_NOT_FOUND,
            "case {case:?}: expected the whole fixed sentence, got {:?}",
            reply.error_message()
        );
        // The attribution names the MEMBER, never the value: no part of the reference
        // reaches either field.
        for leaked in [arn.as_str(), REGION, "attribution"] {
            assert!(
                !reply.error_message().contains(leaked),
                "case {case:?}: the refusal must not state {leaked:?}, got {:?}",
                reply.error_message()
            );
            assert!(
                !reply
                    .at("/invalidParameter")
                    .as_str()
                    .unwrap_or_default()
                    .contains(leaked),
                "case {case:?}: the named parameter must not carry {leaked:?}, got {}",
                reply.body
            );
        }
    }

    // The control: the reference this endpoint DOES hold is served at the same URI, and
    // answers an address rather than a refusal. Waited for by LOOKING AGAIN until the
    // cluster publishes one, which is the transition its own record owes.
    let published = emulator
        .published_bootstrap(&held, "bootstrapBrokerString", "the held cluster")
        .await;
    assert!(
        !published.is_empty(),
        "the held cluster must publish an address, got {published:?}"
    );
}
