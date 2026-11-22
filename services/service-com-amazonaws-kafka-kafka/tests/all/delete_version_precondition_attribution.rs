// SPDX-License-Identifier: CC0-1.0

//! Which INPUT a cluster or replicator delete blames when it cannot read the
//! reference its version pre-condition would be checked against, driven end to
//! end over the same router the emulator serves from.
//!
//! The RULE this suite asserts, over a table rather than one recorded request:
//! the two deletes of this service take their resource's version on the OPTIONAL
//! `currentVersion` query parameter (`smithy_query(query="operation",
//! operation="DeleteCluster,DeleteReplicator")`: `CurrentVersion` is an
//! `@httpQuery` member and only the ARN `@httpLabel` is required), and each
//! surface blames the input Amazon MSK blames for a reference it cannot read:
//! the CLUSTER delete names the version pre-condition it could not check
//! (`currentVersion`) when the request quoted a version, and the member that
//! carried the reference when it quoted none; the REPLICATOR delete names the
//! path-bound ARN member whether or not a version was quoted, because the ARN is
//! what it could not read and the optional version is never examined. The
//! sentence is the surface's own either way and does not depend on the request.
//!
//! The rows span what the rule generalises over: which delete answers (the
//! cluster one and the replicator one, each with its own ARN member and its own
//! fixed sentence), what makes the reference unreadable (five forms, from "not an
//! ARN at all" to a non-ASCII value), and
//! which version the request quotes (absent, a well-formed version token, one no
//! cluster is ever minted at, a lower-cased one and a value that is not shaped
//! like a version at all) — the attribution is the surface's, not one version
//! string's. A reference that IS an ARN and names no resource is a different
//! gate on the CLUSTER delete: its action applies to a cluster, so authorization
//! answers it before the reference is read, which the same suite asserts.
//!
//! The controls hold the rest of the surface fixed: a reference the service CAN
//! read is answered on the resource (a well-formed ARN holding nothing is the
//! 404, whichever version the request quotes), a resolved cluster is answered on
//! the version it quotes — the stale-version refusal for a token the cluster is
//! not at, and the parameter refusal for a value that is not a version token at
//! all (`current_version_parameter`'s rule), both naming `currentVersion` — and
//! the same delete carrying the cluster's own version still removes it.

use serde_json::Value;

// The service's own refusal wordings and the wire names it reports parameters
// by, so the suite and the handlers cannot drift apart on what the service says.
use service_com_amazonaws_kafka_kafka::state::{
    BAD_REQUEST_MESSAGE, CLUSTER_ARN_MEMBER, CLUSTER_VERSION_NOT_CURRENT_MESSAGE,
    CURRENT_VERSION_MEMBER, REPLICATOR_ARN_MEMBER, RESOURCE_NOT_FOUND_MESSAGE,
    VALUE_NOT_VALID_MESSAGE,
};

use super::kafka_harness::{
    pct, poll_until_active, ClusterRequest, Emulator, Reply, ACCOUNT, REGION,
};

/// An id segment shaped like the ones this service mints, for the well-formed
/// ARNs the controls build.
const GHOST_ID: &str = "11111111-2222-4333-8444-555555555555-1";

/// One delete the rule covers: the route the model binds it to, the member its
/// reference travels in, the fixed sentence that surface answers a reference it
/// cannot read with, and whether that surface blames the version pre-condition
/// when the request quotes one.
struct Delete {
    name: &'static str,
    /// The path the resource's ARN is percent-encoded into, as a prefix.
    prefix: &'static str,
    arn_member: &'static str,
    message: &'static str,
    blames_quoted_version: bool,
    /// Whether this surface's target is a CLUSTER, in which case a value that IS
    /// an ARN and names no cluster is refused by authorization before the
    /// reference is read at all.
    targets_a_cluster: bool,
}

impl Delete {
    /// `DELETE <prefix><arn>` with `currentVersion` appended when the row quotes
    /// one, which is where the model puts it on both of these operations.
    fn uri(&self, arn: &str, current_version: Option<&str>) -> String {
        let base = format!("{}{}", self.prefix, pct(arn));
        match current_version {
            Some(version) => format!("{base}?{CURRENT_VERSION_MEMBER}={}", pct(version)),
            None => base,
        }
    }

    /// The input this surface blames for a reference it cannot read, given the
    /// version the request quoted.
    fn blamed(&self, current_version: Option<&str>) -> &'static str {
        match current_version {
            Some(_) if self.blames_quoted_version => CURRENT_VERSION_MEMBER,
            _ => self.arn_member,
        }
    }
}

/// The two deletes of this service whose version pre-condition is opt-in.
fn deletes() -> Vec<Delete> {
    vec![
        Delete {
            name: "DeleteCluster",
            prefix: "/v1/clusters/",
            arn_member: CLUSTER_ARN_MEMBER,
            message: BAD_REQUEST_MESSAGE,
            blames_quoted_version: true,
            targets_a_cluster: true,
        },
        Delete {
            name: "DeleteReplicator",
            prefix: "/replication/v1/replicators/",
            arn_member: REPLICATOR_ARN_MEMBER,
            message: VALUE_NOT_VALID_MESSAGE,
            blames_quoted_version: false,
            targets_a_cluster: false,
        },
    ]
}

/// Every way a submitted reference can fail to be an ARN AT ALL, which neither
/// delete can read and neither can decide an authorization question over.
fn unreadable_references() -> Vec<(&'static str, String)> {
    vec![
        ("not-an-arn-at-all", "not-a-resource-arn".to_string()),
        ("free-text-with-spaces", "not an arn at all".to_string()),
        ("truncated-arn", "arn:aws:kafka".to_string()),
        (
            "a-long-opaque-value",
            format!("not-a-valid-arn-{}", "x".repeat(200)),
        ),
        ("a-non-ascii-value", "ニセのクラスター-arn".to_string()),
    ]
}

/// Every way a submitted reference can BE a well-formed ARN naming a resource of
/// ANOTHER KIND than the delete's target — another vendor's, and another of this
/// service's. Neither delete can read one as its own target, and neither decides an
/// authorization question over it: no permission scoped to the target's kind is ever
/// matched against a resource of another. The account segment of each row is the
/// emulator's own, so no row is refused on the account it names.
fn arns_of_another_kind() -> Vec<(&'static str, String)> {
    vec![
        (
            "another-service-namespace",
            format!("arn:aws:ec2:{REGION}:{ACCOUNT}:instance/i-0a1b2c3d"),
        ),
        (
            "another-resource-kind-of-this-service",
            format!("arn:aws:kafka:{REGION}:{ACCOUNT}:configuration/a-configuration/{GHOST_ID}"),
        ),
    ]
}

/// Every way a submitted reference can BE a CLUSTER reference and still identify no
/// cluster: the resource field with a segment missing, and with one too many.
fn cluster_references_naming_no_cluster() -> Vec<(&'static str, String)> {
    vec![
        (
            "cluster-arn-missing-its-id-segment",
            format!("arn:aws:kafka:{REGION}:{ACCOUNT}:cluster/only-a-name"),
        ),
        (
            "cluster-arn-with-an-extra-segment",
            format!("arn:aws:kafka:{REGION}:{ACCOUNT}:cluster/a-cluster/{GHOST_ID}/extra"),
        ),
    ]
}

/// The versions a row quotes: `None` is a request that states no pre-condition,
/// and every `Some` is one that does — a well-formed version token, one no
/// cluster is ever minted at, a lower-cased token, and a value that is not shaped
/// like a version at all. Which of them is quoted must not change the
/// attribution, so the table carries several.
fn version_probes() -> Vec<Option<&'static str>> {
    vec![
        None,
        Some("K3AEGXETSR30VB"),
        Some("K0000000000000"),
        Some("k3aegxetsr30vb"),
        Some("not-a-version"),
    ]
}

/// Assert ONE refusal of an unreadable reference: HTTP 400 `BadRequestException`,
/// that surface's fixed sentence as the whole message, `expected` as the
/// parameter at fault, and nothing of the request in the message.
fn assert_blamed(
    reply: &Reply,
    case: &str,
    expected: &str,
    message: &str,
    submitted: &str,
    version: Option<&str>,
) {
    assert_eq!(
        (reply.status, reply.error_code()),
        (400, "BadRequestException"),
        "case {case}: expected 400 BadRequestException, got {} {}",
        reply.status,
        reply.body
    );
    assert_eq!(
        reply.at("/invalidParameter").as_str(),
        Some(expected),
        "case {case}: expected the refusal to name {expected}, got {}",
        reply.body
    );
    assert_eq!(
        reply.error_message(),
        message,
        "case {case}: expected the fixed sentence {message:?}, got {:?}",
        reply.error_message()
    );
    assert!(
        !reply.error_message().contains(submitted),
        "case {case}: the refusal must not echo the submitted reference, got {:?}",
        reply.error_message()
    );
    if let Some(version) = version {
        assert!(
            !reply.error_message().contains(version),
            "case {case}: the refusal must not echo the submitted version, got {:?}",
            reply.error_message()
        );
    }
}

/// RULE: a delete handed a reference it cannot read refuses it with HTTP 400
/// `BadRequestException` naming the input that surface blames — the version
/// pre-condition the request asked for (`currentVersion`) on the cluster delete
/// when the request quoted a version, and the member that carried the reference
/// otherwise — whatever makes the reference unreadable and whatever version is
/// quoted, with that surface's one fixed sentence either way.
#[tokio::test]
async fn an_unreadable_reference_is_blamed_on_the_input_its_surface_names() {
    let emulator = Emulator::new();

    for delete in deletes() {
        // A value that is no ARN at all, and one naming a resource of another KIND:
        // neither is a reference to the delete's own target, so each surface blames
        // the input it names, with its own fixed sentence.
        for (form, reference) in unreadable_references()
            .into_iter()
            .chain(arns_of_another_kind())
        {
            for probe in version_probes() {
                let case = format!("{}/{form}/currentVersion={probe:?}", delete.name);
                let reply = emulator
                    .call("DELETE", &delete.uri(&reference, probe), None)
                    .await;
                let expected = delete.blamed(probe);
                assert_blamed(
                    &reply,
                    &case,
                    expected,
                    delete.message,
                    &reference,
                    probe,
                );
            }
        }

        // A CLUSTER reference that identifies no cluster: on the CLUSTER delete the
        // action asks for a cluster that resolves to no resource a permission could
        // be scoped to, so authorization answers before the reference is read and
        // the attribution never arises; the replicator delete, whose target is a
        // replicator, reads it as no reference of its own kind and blames its own
        // ARN member as before.
        for (form, reference) in cluster_references_naming_no_cluster() {
            for probe in version_probes() {
                let case = format!("{}/{form}/currentVersion={probe:?}", delete.name);
                let reply = emulator
                    .call("DELETE", &delete.uri(&reference, probe), None)
                    .await;
                if delete.targets_a_cluster {
                    assert_eq!(
                        (reply.status, reply.error_code()),
                        (403, "AccessDeniedException"),
                        "case {case}: expected the authorization refusal, got {} {}",
                        reply.status,
                        reply.body
                    );
                } else {
                    assert_blamed(
                        &reply,
                        &case,
                        delete.arn_member,
                        delete.message,
                        &reference,
                        probe,
                    );
                }
            }
        }
    }
}

/// RULE: on the cluster delete the attribution follows the pre-condition — for
/// one and the same unreadable reference the only thing that changes which input
/// is named is whether the request quoted a version at all — while the
/// replicator delete names the ARN it could not read either way.
#[tokio::test]
async fn one_reference_is_blamed_by_whether_its_surface_reads_the_quoted_version() {
    let emulator = Emulator::new();

    for delete in deletes() {
        let reference = "neither-a-cluster-nor-a-replicator-arn";
        let without = emulator
            .call("DELETE", &delete.uri(reference, None), None)
            .await;
        let with = emulator
            .call(
                "DELETE",
                &delete.uri(reference, Some("K3AEGXETSR30VB")),
                None,
            )
            .await;
        assert_eq!(
            (
                without.at("/invalidParameter").as_str(),
                with.at("/invalidParameter").as_str(),
            ),
            (Some(delete.arn_member), Some(delete.blamed(Some("any")))),
            "case {}: expected {} without a version and {} with one, got {} and {}",
            delete.name,
            delete.arn_member,
            delete.blamed(Some("any")),
            without.body,
            with.body
        );
        assert_eq!(
            (without.error_message(), with.error_message()),
            (delete.message, delete.message),
            "case {}: expected the one fixed sentence either way, got {:?} and {:?}",
            delete.name,
            without.error_message(),
            with.error_message()
        );
    }
}

/// RULE: a reference the service CAN read is answered on the RESOURCE — a
/// well-formed ARN this endpoint holds nothing for is the 404 naming the member
/// that resolved to nothing, whether or not the request quoted a version: the
/// version attribution belongs to the 400 for a reference that resolves nothing
/// at all.
#[tokio::test]
async fn a_readable_reference_that_holds_nothing_is_still_answered_on_the_resource() {
    let emulator = Emulator::new();

    let absent = [
        (
            "DeleteCluster",
            "/v1/clusters/",
            format!("arn:aws:kafka:{REGION}:{ACCOUNT}:cluster/absent/{GHOST_ID}"),
            CLUSTER_ARN_MEMBER,
        ),
        (
            "DeleteReplicator",
            "/replication/v1/replicators/",
            format!("arn:aws:kafka:{REGION}:{ACCOUNT}:replicator/absent/{GHOST_ID}"),
            REPLICATOR_ARN_MEMBER,
        ),
    ];

    for (name, prefix, arn, member) in absent {
        for probe in version_probes() {
            let case = format!("{name}/currentVersion={probe:?}");
            let uri = match probe {
                Some(version) => format!(
                    "{prefix}{}?{CURRENT_VERSION_MEMBER}={}",
                    pct(&arn),
                    pct(version)
                ),
                None => format!("{prefix}{}", pct(&arn)),
            };
            let reply = emulator.call("DELETE", &uri, None).await;
            assert_eq!(
                (reply.status, reply.error_code()),
                (404, "NotFoundException"),
                "case {case}: expected 404 NotFoundException, got {} {}",
                reply.status,
                reply.body
            );
            // The not-found refusal is about the resource, so it names the member
            // that resolved to nothing rather than the version pre-condition.
            let named = reply
                .at("/invalidParameter")
                .as_str()
                .unwrap_or_default()
                .to_string();
            assert!(
                named.eq_ignore_ascii_case(member),
                "case {case}: expected the 404 to name {member}, got {}",
                reply.body
            );
        }
    }
}

/// RULE: once the reference resolves, the version pre-condition is checked
/// against the resource's own version — a cluster at another version refuses the
/// delete in the service's stale-version words, a value that is not a version
/// token at all refuses it in the service's parameter words, both still naming
/// `currentVersion`, and the same delete quoting the cluster's own version
/// removes it.
#[tokio::test]
async fn a_resolved_cluster_checks_the_quoted_version_against_its_own() {
    let emulator = Emulator::new();
    let arn = emulator
        .create_ok(&ClusterRequest::named("version-precondition"))
        .await;
    // A cluster is deletable once it has finished provisioning; the state guard
    // is `cluster_delete_state_guard`'s rule.
    let current_version = poll_until_active(&emulator, &arn, "version-precondition").await;

    // Each row is a version the cluster is NOT at, with the sentence that quote
    // earns: a token of the shape the service mints is stale, and a value that
    // is not one is the parameter refusal.
    let quotes: Vec<(String, &str)> = vec![
        ("K0000000000000".to_string(), CLUSTER_VERSION_NOT_CURRENT_MESSAGE),
        ("K1111111111111".to_string(), CLUSTER_VERSION_NOT_CURRENT_MESSAGE),
        ("not-a-version".to_string(), BAD_REQUEST_MESSAGE),
        (current_version.to_lowercase(), BAD_REQUEST_MESSAGE),
    ];
    for (stale, message) in &quotes {
        let reply = emulator
            .call(
                "DELETE",
                &format!(
                    "/v1/clusters/{}?{CURRENT_VERSION_MEMBER}={}",
                    pct(&arn),
                    pct(stale)
                ),
                None,
            )
            .await;
        assert_eq!(
            (
                reply.status,
                reply.error_code(),
                reply.error_message(),
                reply.at("/invalidParameter").as_str(),
            ),
            (
                400,
                "BadRequestException",
                *message,
                Some(CURRENT_VERSION_MEMBER),
            ),
            "case quoted={stale:?}: expected the refusal that quote earns, got {} {}",
            reply.status,
            reply.body
        );
        assert_eq!(
            emulator.describe(&arn).await.status,
            200,
            "case quoted={stale:?}: a refused delete must leave the cluster in place"
        );
    }

    // The cluster's own version satisfies the pre-condition, so the delete goes
    // through and the cluster is gone for every later reader.
    let reply = emulator
        .call(
            "DELETE",
            &format!(
                "/v1/clusters/{}?{CURRENT_VERSION_MEMBER}={}",
                pct(&arn),
                pct(&current_version)
            ),
            None,
        )
        .await;
    assert_eq!(
        (reply.status, reply.at("/state").as_str()),
        (200, Some("DELETING")),
        "expected the delete quoting the cluster's own version to be accepted, got {} {}",
        reply.status,
        reply.body
    );
    let described = emulator.describe(&arn).await;
    assert_eq!(
        (described.status, described.error_message()),
        (404, RESOURCE_NOT_FOUND_MESSAGE),
        "expected the deleted cluster to be gone, got {} {}",
        described.status,
        described.body
    );
    assert_eq!(
        described.at("/clusterInfo"),
        &Value::Null,
        "a deleted cluster must be served to nobody, got {}",
        described.body
    );
}
