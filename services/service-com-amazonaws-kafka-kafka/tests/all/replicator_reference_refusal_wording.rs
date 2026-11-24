// SPDX-License-Identifier: CC0-1.0

//! HOW the replicator operations word a reference they resolve no replicator for, driven end
//! to end over the same router the emulator serves from (the shared `kafka_harness`).
//!
//! RULE: a replicator reference that resolves to nothing is HTTP 404 `NotFoundException`
//! whose WHOLE message is the service's one fixed sentence, with the input member at fault in
//! `invalidParameter` and nothing of the request in the text. How a well-formed replicator
//! ARN SPELLS the replicator it names — the name segment, the identifier segment, the Region
//! and account it names — is not read: a reference this endpoint holds no replicator for is
//! absent whether or not those segments resemble ones this registry mints, so the service
//! says the same thing about all of them. The one reference the replicator APIs frame as the
//! parameter's fault is a replicator this endpoint HELD and released: a resource of the
//! service's own that is gone, which earns the same sentence behind the
//! `Invalid parameter <member>: ` framing those APIs put before it.
//!
//! The rows differ along what the rule generalises over: the NAME segment (names the modelled
//! `@pattern` admits — alphanumeric, hyphenated, digits-only, the longest one its `@length`
//! allows — and names it does not: an underscore, a dot, a leading hyphen, one past the
//! modelled length) and the IDENTIFIER segment (the `<uuid>-<ordinal>` form the service mints,
//! against a bare word, a UUID with no ordinal, a non-numeric ordinal and a truncated UUID) —
//! crossed with the Region the ARN names, this endpoint's and another's, and with both
//! replicator operations that resolve an ARN on their own path.
//!
//! The second test is the boundary: one replicator this endpoint really held, released by its
//! own delete, whose ARN is of exactly the form the first test's rows take — so what tells the
//! two forms apart is the release, not how the ARN is spelled.

use serde_json::json;

use crate::kafka_harness::{pct, ClusterRequest, Emulator, Reply, ACCOUNT, REGION};

/// The whole message Amazon MSK answers a resource reference it resolves nothing for with
/// (the apostrophe is U+2019, as the service writes it). Written out rather than imported, so
/// an assertion cannot agree with a constant that is wrong.
const RESOURCE_NOT_FOUND: &str = "The requested resource doesn\u{2019}t exist.";

/// The wire name of the member every replicator operation carries its target's ARN in, which
/// is the parameter a refusal names.
const REPLICATOR_ARN_MEMBER: &str = "replicatorArn";

/// The service-execution role a `CreateReplicator` request passes.
const ROLE_ARN: &str = "arn:aws:iam::000000000000:role/msk-replicator";

/// The identifier form Amazon MSK assigns a replicator: a UUID, then the ordinal of the
/// resource within the registry.
const ISSUED_IDENTIFIER: &str = "0a1b2c3d-4e5f-4a7b-8c9d-0e1f2a3b4c5d-7";

/// A replicator ARN of `region` naming `name` and `identifier`, which is the form every row
/// below varies one segment of.
fn replicator_arn(region: &str, name: &str, identifier: &str) -> String {
    format!("arn:aws:kafka:{region}:{ACCOUNT}:replicator/{name}/{identifier}")
}

/// Which of the two forms a reference earns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Worded {
    /// The service's one fixed sentence: a replicator this endpoint holds none of.
    AsAbsent,
    /// The same sentence behind the `Invalid parameter <member>: ` framing: a replicator of
    /// this endpoint's own that is gone.
    AsTheParametersFault,
}

impl Worded {
    /// The whole message a reply must carry.
    fn message(self) -> String {
        match self {
            Self::AsAbsent => RESOURCE_NOT_FOUND.to_string(),
            Self::AsTheParametersFault => {
                format!("Invalid parameter {REPLICATOR_ARN_MEMBER}: {RESOURCE_NOT_FOUND}")
            }
        }
    }
}

/// Assert `reply` is the 404 the rule calls for: the form `worded` names, the member at fault
/// in `invalidParameter`, and nothing of the request — the ARN, the Region, the account — in
/// the message.
fn assert_not_found(reply: &Reply, case: &str, reference: &str, worded: Worded) {
    assert_eq!(
        (reply.status, reply.error_code()),
        (404, "NotFoundException"),
        "case {case}: expected 404 NotFoundException, got {} {}",
        reply.status,
        reply.body
    );
    assert_eq!(
        reply.error_message(),
        worded.message(),
        "case {case}: expected {:?}, got {:?}",
        worded.message(),
        reply.error_message()
    );
    assert_eq!(
        reply.at("/invalidParameter").as_str(),
        Some(REPLICATOR_ARN_MEMBER),
        "case {case}: expected the refusal to name {REPLICATOR_ARN_MEMBER} at fault, got {}",
        reply.body
    );
    for leaked in [reference, REGION, ACCOUNT] {
        assert!(
            !reply.error_message().contains(leaked),
            "case {case}: the message must carry no part of the request ({leaked:?}), got {:?}",
            reply.error_message()
        );
    }
}

/// Address `arn` through each replicator operation that resolves one on its own path, and
/// assert every answer takes the form `worded` names.
async fn assert_every_path(emulator: &Emulator, case: &str, arn: &str, worded: Worded) {
    for (operation, method) in [("DescribeReplicator", "GET"), ("DeleteReplicator", "DELETE")] {
        let reply = emulator
            .call(
                method,
                &format!("/replication/v1/replicators/{}", pct(arn)),
                None,
            )
            .await;
        assert_not_found(&reply, &format!("{operation}: {case}"), arn, worded);
    }
}

/// One row: the case and the ARN the request carries. Every row is a reference this endpoint
/// resolves no replicator for, so every one of them earns the same form.
struct Row {
    case: String,
    arn: String,
}

/// The rows: every identity form, in this endpoint's Region and in another.
fn rows() -> Vec<Row> {
    // The longest name `ReplicatorName`'s `@length(min: 1, max: 128)` allows, built from the
    // characters its `@pattern ^[0-9A-Za-z][0-9A-Za-z-]{0,}$` admits.
    let longest_name = format!("r{}", "a".repeat(127));
    // (case, the name segment, the identifier segment)
    let identities: Vec<(String, String, String)> = vec![
        (
            "a name and an identifier of the forms the service issues".to_string(),
            "replicator-never-minted".to_string(),
            ISSUED_IDENTIFIER.to_string(),
        ),
        (
            "a name of digits alone".to_string(),
            "20261124".to_string(),
            ISSUED_IDENTIFIER.to_string(),
        ),
        (
            "the longest name the model allows".to_string(),
            longest_name.clone(),
            ISSUED_IDENTIFIER.to_string(),
        ),
        (
            "an identifier whose ordinal counts further".to_string(),
            "replicator-ordinal".to_string(),
            "0a1b2c3d-4e5f-4a7b-8c9d-0e1f2a3b4c5d-128".to_string(),
        ),
        (
            "a name one character past the model's length".to_string(),
            format!("{longest_name}a"),
            ISSUED_IDENTIFIER.to_string(),
        ),
        (
            "a name carrying an underscore".to_string(),
            "replicator_never_minted".to_string(),
            ISSUED_IDENTIFIER.to_string(),
        ),
        (
            "a name carrying a dot".to_string(),
            "replicator.never.minted".to_string(),
            ISSUED_IDENTIFIER.to_string(),
        ),
        (
            "a name starting with a hyphen".to_string(),
            "-replicator".to_string(),
            ISSUED_IDENTIFIER.to_string(),
        ),
        (
            "an identifier that is a bare word".to_string(),
            "replicator-bare-id".to_string(),
            "ghost".to_string(),
        ),
        (
            "an identifier that is a UUID with no ordinal".to_string(),
            "replicator-no-ordinal".to_string(),
            "0a1b2c3d-4e5f-4a7b-8c9d-0e1f2a3b4c5d".to_string(),
        ),
        (
            "an identifier whose ordinal is not a number".to_string(),
            "replicator-word-ordinal".to_string(),
            "0a1b2c3d-4e5f-4a7b-8c9d-0e1f2a3b4c5d-last".to_string(),
        ),
        (
            "an identifier whose UUID is truncated".to_string(),
            "replicator-short-uuid".to_string(),
            "0a1b2c3d-4e5f-4a7b-8c9d-0e1f2a3b4c5-7".to_string(),
        ),
    ];

    let mut rows: Vec<Row> = Vec::new();
    // The Region the ARN names is not part of this reading either: this endpoint serves one
    // Region's registry, so a replicator of another Region's is simply not here.
    for region in [REGION, "eu-central-1"] {
        for (case, name, identifier) in &identities {
            rows.push(Row {
                case: format!("{case}, in {region}"),
                arn: replicator_arn(region, name, identifier),
            });
        }
    }
    rows
}

/// The rule: whatever a well-formed replicator ARN spells, a reference this endpoint resolves
/// no replicator for is the service's one fixed sentence.
#[tokio::test]
async fn a_replicator_reference_that_resolves_to_nothing_is_one_fixed_sentence() {
    let emulator = Emulator::new();
    for row in rows() {
        assert_every_path(&emulator, &row.case, &row.arn, Worded::AsAbsent).await;
    }
}

/// The boundary: a replicator this endpoint HELD and released is the one reference those APIs
/// frame as the parameter's fault — an ARN of exactly the form the rows above carry, so the
/// release is what tells the two forms apart.
#[tokio::test]
async fn a_replicator_this_endpoint_released_is_the_parameters_fault() {
    let emulator = Emulator::new();
    let one = emulator
        .create_ok(&ClusterRequest::named("wording-source"))
        .await;
    let two = emulator
        .create_ok(&ClusterRequest::named("wording-target"))
        .await;
    let created = emulator
        .call(
            "POST",
            "/replication/v1/replicators",
            Some(json!({
                "replicatorName": "wording-released",
                "serviceExecutionRoleArn": ROLE_ARN,
                "kafkaClusters": [
                    {
                        "amazonMskCluster": { "mskClusterArn": one },
                        "vpcConfig": { "subnetIds": ["subnet-0a1b2c3d", "subnet-0a1b2c3e"] },
                    },
                    {
                        "amazonMskCluster": { "mskClusterArn": two },
                        "vpcConfig": { "subnetIds": ["subnet-0a1b2c3d", "subnet-0a1b2c3e"] },
                    },
                ],
                "replicationInfoList": [{
                    "sourceKafkaClusterArn": one,
                    "targetKafkaClusterArn": two,
                    "targetCompressionType": "NONE",
                    "consumerGroupReplication": { "consumerGroupsToReplicate": [".*"] },
                    "topicReplication": { "topicsToReplicate": [".*"] },
                }],
            })),
        )
        .await;
    assert_eq!(
        created.status, 200,
        "case created: expected the replicator this suite releases to be created, got {} {}",
        created.status, created.body
    );
    let held = created
        .at("/replicatorArn")
        .as_str()
        .expect("CreateReplicator reported no replicatorArn")
        .to_string();

    let deleted = emulator
        .call(
            "DELETE",
            &format!("/replication/v1/replicators/{}", pct(&held)),
            None,
        )
        .await;
    assert_eq!(
        deleted.status, 200,
        "case released: expected DeleteReplicator to answer 200, got {} {}",
        deleted.status, deleted.body
    );

    assert_every_path(
        &emulator,
        "a replicator this endpoint held and released",
        &held,
        Worded::AsTheParametersFault,
    )
    .await;
}
