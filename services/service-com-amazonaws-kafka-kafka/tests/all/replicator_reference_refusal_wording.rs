// SPDX-License-Identifier: CC0-1.0

//! HOW the replicator operations word a reference they resolve no replicator for, driven end
//! to end over the same router the emulator serves from (the shared `kafka_harness`).
//!
//! RULE: a replicator reference that resolves to nothing is always HTTP 404
//! `NotFoundException`, with the input member at fault in `invalidParameter` and nothing of
//! the request in the message — and WHICH of the service's two forms the message takes
//! follows what the ARN could have denoted at all. An ARN carrying the identity Amazon MSK
//! ISSUES a replicator under — a `ReplicatorName` the API accepts, then the identifier the
//! service assigned it (`<name>/<uuid>-<ordinal>`) — names a replicator the caller may yet
//! create, so it earns the service's one fixed sentence. An ARN carrying any other identity
//! is a value this service could never have held under: it is reported as a fault of the
//! parameter that carries it, in the `Invalid parameter <member>: ` framing the replicator
//! APIs put before that same sentence.
//!
//! The rows differ along what the rule generalises over: the NAME segment (names the modelled
//! `@pattern` admits — alphanumeric, hyphenated, digits-only, the longest one its `@length`
//! allows — and names it does not: an underscore, a dot, a leading hyphen, an empty one) and
//! the IDENTIFIER segment (the `<uuid>-<ordinal>` form the service mints, against a bare
//! word, a UUID with no ordinal, a non-numeric ordinal and a truncated UUID) — crossed with
//! the Region the ARN names, which this endpoint does not serve for any of them, and with
//! both replicator operations that resolve an ARN on their own path.
//!
//! A replicator this endpoint HELD and released is the other reference reported as the
//! parameter's fault; that half is driven in
//! `crate::resource_resolution_refusals::a_released_replicator_is_told_apart_from_one_that_never_existed`,
//! which creates one and deletes it, so nothing here restates it.

use crate::kafka_harness::{pct, Emulator, Reply, ACCOUNT, REGION};

/// The whole message Amazon MSK answers a resource reference it resolves nothing for with
/// (the apostrophe is U+2019, as the service writes it). Written out rather than imported, so
/// an assertion cannot agree with a constant that is wrong.
const RESOURCE_NOT_FOUND: &str = "The requested resource doesn\u{2019}t exist.";

/// The wire name of the member every replicator operation carries its target's ARN in, which
/// is the parameter a refusal names.
const REPLICATOR_ARN_MEMBER: &str = "replicatorArn";

/// The identifier form Amazon MSK assigns a replicator: a UUID, then the ordinal of the
/// resource within the registry.
const ISSUED_IDENTIFIER: &str = "0a1b2c3d-4e5f-4a7b-8c9d-0e1f2a3b4c5d-7";

/// A replicator ARN of `region` naming `name` and `identifier`, which is the form every row
/// below varies one segment of.
fn replicator_arn(region: &str, name: &str, identifier: &str) -> String {
    format!("arn:aws:kafka:{region}:{ACCOUNT}:replicator/{name}/{identifier}")
}

/// Which of the two forms a row's reference earns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Worded {
    /// The service's one fixed sentence: a replicator the caller may yet create.
    AsAbsent,
    /// The same sentence behind the `Invalid parameter <member>: ` framing: a value this
    /// service could never have held a replicator under.
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

/// One row: the case, the ARN the request carries and the form the refusal must take.
struct Row {
    case: String,
    arn: String,
    worded: Worded,
}

/// The rows: every identity form, in this endpoint's Region and in another.
fn rows() -> Vec<Row> {
    // The longest name `ReplicatorName`'s `@length(min: 1, max: 128)` allows, built from the
    // characters its `@pattern ^[0-9A-Za-z][0-9A-Za-z-]{0,}$` admits.
    let longest_name = format!("r{}", "a".repeat(127));
    // (case, the name segment, the identifier segment, the form expected)
    let identities: Vec<(String, String, String, Worded)> = vec![
        (
            "a name and an identifier of the forms the service issues".to_string(),
            "replicator-never-minted".to_string(),
            ISSUED_IDENTIFIER.to_string(),
            Worded::AsAbsent,
        ),
        (
            "a name of digits alone".to_string(),
            "20261124".to_string(),
            ISSUED_IDENTIFIER.to_string(),
            Worded::AsAbsent,
        ),
        (
            "the longest name the model allows".to_string(),
            longest_name.clone(),
            ISSUED_IDENTIFIER.to_string(),
            Worded::AsAbsent,
        ),
        (
            "an identifier whose ordinal counts further".to_string(),
            "replicator-ordinal".to_string(),
            "0a1b2c3d-4e5f-4a7b-8c9d-0e1f2a3b4c5d-128".to_string(),
            Worded::AsAbsent,
        ),
        (
            "a name one character past the model's length".to_string(),
            format!("{longest_name}a"),
            ISSUED_IDENTIFIER.to_string(),
            Worded::AsTheParametersFault,
        ),
        (
            "a name carrying an underscore".to_string(),
            "replicator_never_minted".to_string(),
            ISSUED_IDENTIFIER.to_string(),
            Worded::AsTheParametersFault,
        ),
        (
            "a name carrying a dot".to_string(),
            "replicator.never.minted".to_string(),
            ISSUED_IDENTIFIER.to_string(),
            Worded::AsTheParametersFault,
        ),
        (
            "a name starting with a hyphen".to_string(),
            "-replicator".to_string(),
            ISSUED_IDENTIFIER.to_string(),
            Worded::AsTheParametersFault,
        ),
        (
            "an identifier that is a bare word".to_string(),
            "replicator-bare-id".to_string(),
            "ghost".to_string(),
            Worded::AsTheParametersFault,
        ),
        (
            "an identifier that is a UUID with no ordinal".to_string(),
            "replicator-no-ordinal".to_string(),
            "0a1b2c3d-4e5f-4a7b-8c9d-0e1f2a3b4c5d".to_string(),
            Worded::AsTheParametersFault,
        ),
        (
            "an identifier whose ordinal is not a number".to_string(),
            "replicator-word-ordinal".to_string(),
            "0a1b2c3d-4e5f-4a7b-8c9d-0e1f2a3b4c5d-last".to_string(),
            Worded::AsTheParametersFault,
        ),
        (
            "an identifier whose UUID is truncated".to_string(),
            "replicator-short-uuid".to_string(),
            "0a1b2c3d-4e5f-4a7b-8c9d-0e1f2a3b4c5-7".to_string(),
            Worded::AsTheParametersFault,
        ),
    ];

    let mut rows: Vec<Row> = Vec::new();
    // The Region the ARN names is not part of this reading: this endpoint serves one Region's
    // registry, so a replicator of another Region's is simply not here either way.
    for region in [REGION, "eu-central-1"] {
        for (case, name, identifier, worded) in &identities {
            rows.push(Row {
                case: format!("{case}, in {region}"),
                arn: replicator_arn(region, name, identifier),
                worded: *worded,
            });
        }
    }
    rows
}

#[tokio::test]
async fn a_replicator_reference_is_worded_by_the_identity_the_arn_carries() {
    let emulator = Emulator::new();
    for row in rows() {
        for (operation, method) in [("DescribeReplicator", "GET"), ("DeleteReplicator", "DELETE")] {
            let case = format!("{operation}: {}", row.case);
            let reply = emulator
                .call(
                    method,
                    &format!("/replication/v1/replicators/{}", pct(&row.arn)),
                    None,
                )
                .await;
            assert_not_found(&reply, &case, &row.arn, row.worded);
        }
    }
}
