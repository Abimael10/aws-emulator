// SPDX-License-Identifier: CC0-1.0

//! WHICH subject this service names when it refuses an identifier past the bound its
//! operation states.
//!
//! The rule under test: ElastiCache states the length half of its identifier rule in the
//! words the OPERATION that answers it uses for the identifier's subject — where that subject
//! is AWS's own parameter label, `The parameter <label> is not a valid identifier because it
//! is longer than <n> characters.`, and where it is the resource named in prose, `<prose>
//! cannot be longer than <n> characters.` The only variable parts are that
//! subject and the bound, both fixed by the rule: nothing of the value that broke it is
//! echoed back, the refusal is 400 `InvalidParameterValue`, and it is answered BEFORE the
//! identifier is resolved against anything — while an identifier the bound admits reaches
//! past the rule and is judged by the resource it names.
//!
//! The tests are tables. The rows differ along what the rule generalises over — which
//! operation answers, which member carries the identifier, which bound that operation states
//! (the cluster's 50, the replication group's 40 and the wider bound its lookup states, a
//! parameter group's 255), which of the two subjects the service names that member by, and
//! how far past the bound the value is. The lead-in of the OTHER half of the rule, for a
//! value the grammar does not admit, is the subject of `refusal_wording_per_class`.
//!
//! Every assertion goes over the wire through the same router the production emulator mounts,
//! using the harness in [`crate::harness`]; nothing reaches into the service's state struct.

use service_com_amazonaws_elasticache_amazonelasticachev9::state;

use crate::harness::{params, Emulator};

/// How the service names a member in the length sentence of its identifier rule.
#[derive(Debug, Clone, Copy)]
enum Subject {
    /// AWS's own parameter label for the member, e.g. `CacheClusterIdentifier`.
    Label(&'static str),
    /// The resource identifier named in prose, e.g. `Replication Group ID`.
    Prose(&'static str),
}

impl Subject {
    /// The whole sentence an identifier past `bound` is refused with.
    fn over_length(&self, bound: usize) -> String {
        match self {
            Self::Label(label) => format!(
                "The parameter {label} is not a valid identifier because it is longer than \
                 {bound} characters."
            ),
            Self::Prose(prose) => format!("{prose} cannot be longer than {bound} characters."),
        }
    }
}

/// One row: an operation that validates an identifier, the member carrying it, the rest of
/// the request it needs to reach the identifier rule, the bound it states and the subject it
/// names.
struct IdentifierSubject {
    case: &'static str,
    action: &'static str,
    member: &'static str,
    extra: &'static [(&'static str, &'static str)],
    bound: usize,
    subject: Subject,
}

/// The axis: which operation answers, which member it is about, and which of the two subjects
/// the service names that member by. The bounds are the ones the state module publishes from
/// each member's own documentation, so no number is written twice.
fn identifier_subjects() -> Vec<IdentifierSubject> {
    vec![
        IdentifierSubject {
            case: "a cluster lookup",
            action: "DescribeCacheClusters",
            member: "CacheClusterId",
            extra: &[],
            bound: state::CLUSTER_ID_MAX_LEN,
            subject: Subject::Label("CacheClusterIdentifier"),
        },
        IdentifierSubject {
            case: "the create that would store a cluster id",
            action: "CreateCacheCluster",
            member: "CacheClusterId",
            extra: &[("Engine", "redis"), ("CacheNodeType", "cache.t3.micro")],
            bound: state::CLUSTER_ID_MAX_LEN,
            subject: Subject::Label("CacheClusterIdentifier"),
        },
        IdentifierSubject {
            case: "a snapshot listing filtered by cluster",
            action: "DescribeSnapshots",
            member: "CacheClusterId",
            extra: &[],
            bound: state::CLUSTER_ID_MAX_LEN,
            subject: Subject::Label("CacheClusterIdentifier"),
        },
        IdentifierSubject {
            case: "a parameter group delete",
            action: "DeleteCacheParameterGroup",
            member: "CacheParameterGroupName",
            extra: &[],
            bound: state::PARAMETER_GROUP_NAME_MAX_LEN,
            subject: Subject::Label("CacheParameterGroupName"),
        },
        IdentifierSubject {
            case: "a parameter group reset",
            action: "ResetCacheParameterGroup",
            member: "CacheParameterGroupName",
            extra: &[("ResetAllParameters", "true")],
            bound: state::PARAMETER_GROUP_NAME_MAX_LEN,
            subject: Subject::Label("CacheParameterGroupName"),
        },
        IdentifierSubject {
            case: "a settings change to a replication group",
            action: "ModifyReplicationGroup",
            member: "ReplicationGroupId",
            extra: &[("ApplyImmediately", "true")],
            bound: state::REPLICATION_GROUP_ID_MAX_LEN,
            subject: Subject::Label("ReplicationGroupId"),
        },
        // The contrast rows: the same resource on the operations whose own wording names it in
        // prose, so the rule is shown to be per member and per operation rather than one
        // subject for the whole service.
        IdentifierSubject {
            case: "a replication group delete",
            action: "DeleteReplicationGroup",
            member: "ReplicationGroupId",
            extra: &[],
            bound: state::REPLICATION_GROUP_ID_MAX_LEN,
            subject: Subject::Prose("Replication Group ID"),
        },
        IdentifierSubject {
            case: "a replication group lookup",
            action: "DescribeReplicationGroups",
            member: "ReplicationGroupId",
            extra: &[],
            bound: state::LOOKUP_IDENTIFIER_MAX_LEN,
            subject: Subject::Prose("Replication Group ID"),
        },
        IdentifierSubject {
            case: "a replica count decrease",
            action: "DecreaseReplicaCount",
            member: "ReplicationGroupId",
            extra: &[("ApplyImmediately", "true"), ("NewReplicaCount", "0")],
            bound: state::REPLICATION_GROUP_ID_MAX_LEN,
            subject: Subject::Prose("Replication Group ID"),
        },
    ]
}

/// A well-formed identifier of exactly `length` characters: it begins with a letter and
/// carries only letters, so the only thing that can be wrong with it is its length.
fn identifier(length: usize) -> String {
    format!("g{}", "a".repeat(length - 1))
}

#[tokio::test]
async fn an_over_long_identifier_is_refused_with_the_subject_its_member_is_named_by() {
    // Where the bounds come from: each member's own documented maximum, as this service
    // publishes it. A row that drifted from one of these would state a ceiling no create
    // honours, which is why the numbers are checked here and not only used in the table.
    assert_eq!(
        state::CLUSTER_ID_MAX_LEN, 50,
        "CacheClusterId is documented as at most 50 characters, got {}",
        state::CLUSTER_ID_MAX_LEN
    );
    assert_eq!(
        state::REPLICATION_GROUP_ID_MAX_LEN, 40,
        "ReplicationGroupId is documented as at most 40 characters, got {}",
        state::REPLICATION_GROUP_ID_MAX_LEN
    );

    for row in identifier_subjects() {
        let emulator = Emulator::new();
        let case = row.case;
        let expected = row.subject.over_length(row.bound);
        for (how, value) in [
            ("one character past the bound", identifier(row.bound + 1)),
            ("far past the bound", identifier(row.bound + 23)),
        ] {
            let mut request = params(&[(row.member, value.as_str())]);
            request.extend(params(row.extra));
            let refused = emulator.call(row.action, &request).await;
            assert_eq!(
                (refused.status, refused.error_code()),
                (400, "InvalidParameterValue"),
                "case {case:?} / {how}: expected 400 InvalidParameterValue, got {} {} {}",
                refused.status,
                refused.error_code(),
                refused.body
            );
            assert_eq!(
                refused.error_message(),
                expected,
                "case {case:?} / {how}: expected {expected:?}, got {:?}",
                refused.error_message()
            );
            // The sentence states the rule, so it carries neither the value nor its length.
            assert!(
                !refused.error_message().contains(value.as_str()),
                "case {case:?} / {how}: the rule carries no request value, got {:?}",
                refused.error_message()
            );
            assert!(
                !refused.error_message().contains(&value.len().to_string()),
                "case {case:?} / {how}: the rule states the bound, not the length, got {:?}",
                refused.error_message()
            );
        }
    }
}

#[tokio::test]
async fn an_identifier_at_the_bound_reaches_past_the_length_rule() {
    for row in identifier_subjects() {
        let emulator = Emulator::new();
        let case = row.case;
        let at_bound = identifier(row.bound);
        let mut request = params(&[(row.member, at_bound.as_str())]);
        request.extend(params(row.extra));
        let answered = emulator.call(row.action, &request).await;
        // The rule did not fire: whatever this operation answers for an identifier it admits
        // — the resource it names, or that no such resource is held — it is not the length
        // sentence.
        assert_ne!(
            answered.error_message(),
            row.subject.over_length(row.bound),
            "case {case:?} / at the bound: the length rule must not fire, got {:?}",
            answered.error_message()
        );
    }
}
