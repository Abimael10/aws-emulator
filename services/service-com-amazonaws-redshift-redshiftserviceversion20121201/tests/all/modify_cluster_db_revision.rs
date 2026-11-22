// SPDX-License-Identifier: CC0-1.0

//! `ModifyClusterDbRevision`: the cluster modification whose answer is decided
//! before any revision work, end to end over the awsQuery wire.
//!
//! Every case drives a real HTTP request through the same `MultiServiceRouter`
//! the production router builds (the harness is shared with
//! [`crate::cluster_lifecycle`]), so each assertion covers the whole walk:
//! form-urlencoded deserialize -> handler -> XML serialize.
//!
//! One rule is under test, and each row is one member of the class it
//! generalises over rather than one recorded request: `ModifyClusterDbRevision`
//! resolves the CLUSTER first — before the revision the request names is looked
//! at at all — so a cluster this account does not hold is `ClusterNotFound`
//! (404) and a cluster the service is already working on is
//! `InvalidClusterState` (400); then the revision decides, in two DISJOINT
//! classes: the revision the cluster already runs is `ClusterOnLatestRevision`
//! (400, "nothing to do") and every other revision the cluster is not offered is
//! `InvalidParameterValue` (400, "read the next reachable target"). Each refusal
//! leaves the cluster exactly as it was.
//!
//! The window the `InvalidClusterState` refusal needs is opened by the work that
//! makes a serving cluster busy: a `ModifyCluster` that changes the cluster's
//! SHAPE is a resize, so the cluster reports work in progress from that call's
//! own answer until the span it takes has passed — which the last suite below
//! asserts over a table of shape changes.
//!
//! The retired AQUA modification has its own suite,
//! [`crate::aqua_configuration`].

use service_com_amazonaws_redshift_redshiftserviceversion20121201::state;

use crate::cluster_lifecycle::{all_text, Answer, Emulator};

/// The Region the shared harness signs every request for, which is the Region
/// the ARNs this suite names are built from.
const REGION: &str = "us-west-2";

/// Create one cluster and wait for it to be serving, by POLLING the read.
async fn available_cluster(emulator: &Emulator, identifier: &str) {
    let created = emulator.create_minimal(identifier).await;
    assert_eq!(
        created.status, 200,
        "case seed cluster {identifier}: expected 200, got {} with body {}",
        created.status, created.body
    );
    emulator
        .poll_until(
            &format!("seed cluster {identifier}"),
            "DescribeClusters",
            &[("ClusterIdentifier", identifier)],
            |answer| answer.text("ClusterStatus").as_deref() == Some(state::STATUS_AVAILABLE),
        )
        .await;
}

/// `ModifyClusterDbRevision` for one cluster and revision target.
async fn modify_revision(emulator: &Emulator, cluster: &str, target: &str) -> Answer {
    emulator
        .call(
            "ModifyClusterDbRevision",
            &[
                ("ClusterIdentifier", cluster),
                ("RevisionTarget", target),
            ],
        )
        .await
}

/// `DescribeClusterDbRevisions` for one cluster.
async fn describe_revisions(emulator: &Emulator, cluster: &str) -> Answer {
    emulator
        .call(
            "DescribeClusterDbRevisions",
            &[("ClusterIdentifier", cluster)],
        )
        .await
}

/// The revision the cluster RUNS, read back through the operation whose answer
/// the request's `RevisionTarget` is taken from.
async fn current_revision(emulator: &Emulator, cluster: &str) -> String {
    let published = describe_revisions(emulator, cluster).await;
    assert_eq!(
        published.status, 200,
        "case read the revision of {cluster}: expected 200, got {} with body {}",
        published.status, published.body
    );
    published
        .text("CurrentDatabaseRevision")
        .unwrap_or_else(|| panic!("the read reports the revision {cluster} runs: {}", published.body))
}

#[tokio::test]
async fn a_revision_change_resolves_the_cluster_before_the_revision() {
    let emulator = Emulator::new();
    available_cluster(&emulator, "revision-present").await;

    // The class: identifiers this account holds no cluster under, crossed with
    // the revision target asked for — the target cannot change the answer,
    // because the cluster is resolved first.
    let longest: String = std::iter::once('c')
        .chain(std::iter::repeat_n(
            'x',
            state::CLUSTER_IDENTIFIER_RULE.max_len - 1,
        ))
        .collect();
    let rows: Vec<(&str, String, &str)> = vec![
        (
            "a plain identifier nothing created",
            "no-revision-target".to_string(),
            "bogus-revision",
        ),
        (
            "the shortest identifier the documented rule admits",
            "c".to_string(),
            "bogus-revision",
        ),
        (
            "the longest identifier the documented rule admits",
            longest,
            "bogus-revision",
        ),
        (
            "an identifier one character off a stored one",
            "revision-presen".to_string(),
            // A revision target of the shape this service's own revision
            // numbers take, so the refusal is about the cluster and not the
            // target's spelling.
            "1.0.12345",
        ),
        (
            "an identifier in mixed case",
            "Revision-Present".to_string(),
            "",
        ),
    ];

    for (case, identifier, target) in rows {
        let refused = modify_revision(&emulator, &identifier, target).await;
        assert_eq!(
            (refused.status, refused.error_code().as_deref()),
            (404, Some("ClusterNotFound")),
            "case {case:?}: expected 404 ClusterNotFound, got {} / {:?} with body {}",
            refused.status,
            refused.error_code(),
            refused.body
        );
        // The identifier the request named is interpolated verbatim into the
        // sentence this operation carries.
        let expected = format!("Cluster {identifier} not found.");
        assert_eq!(
            refused.text("Message").as_deref(),
            Some(expected.as_str()),
            "case {case:?}: expected the message {expected:?}, got {:?}",
            refused.text("Message")
        );
    }
}

/// One row of the latest-revision table: the case name, the cluster identifier,
/// and the cluster-shaping parameters the create carries beyond the required
/// ones.
type ShapeCase = (
    &'static str,
    &'static str,
    &'static [(&'static str, &'static str)],
);

/// The rows differ along the cluster's shape, because the revision rules are
/// about the revisions PUBLISHED for a cluster rather than about its
/// configuration.
const SHAPES: &[ShapeCase] = &[
    (
        "a single-node cluster",
        "revision-single",
        &[("NodeType", "ra3.xlplus"), ("ClusterType", "single-node")],
    ),
    (
        "a two-node cluster of another node type",
        "revision-dc2-pair",
        &[
            ("NodeType", "dc2.large"),
            ("ClusterType", "multi-node"),
            ("NumberOfNodes", "2"),
        ],
    ),
];

/// Create the cluster one `SHAPES` row describes and wait for it to serve.
async fn available_shaped_cluster(
    emulator: &Emulator,
    case: &str,
    identifier: &str,
    shape: &[(&str, &str)],
) {
    let mut params: Vec<(&str, &str)> = vec![
        ("ClusterIdentifier", identifier),
        ("MasterUsername", "admin"),
    ];
    params.extend_from_slice(shape);
    let created = emulator.call("CreateCluster", &params).await;
    assert_eq!(
        created.status, 200,
        "case {case}: expected the seed create to answer 200, got {} with body {}",
        created.status, created.body
    );
    emulator
        .poll_until(
            case,
            "DescribeClusters",
            &[("ClusterIdentifier", identifier)],
            |answer| answer.text("ClusterStatus").as_deref() == Some(state::STATUS_AVAILABLE),
        )
        .await;
}

#[tokio::test]
async fn the_revision_a_cluster_already_runs_is_nothing_to_move_it_onto() {
    let emulator = Emulator::new();

    for (case, identifier, shape) in SHAPES {
        available_shaped_cluster(&emulator, case, identifier, shape).await;

        // The revisions a cluster can be moved onto are the ones its own
        // `RevisionTargets` publishes, and a read of them reports none.
        let published = describe_revisions(&emulator, identifier).await;
        assert!(
            all_text(&published.body, "DatabaseRevision").is_empty(),
            "case {case}: expected the cluster to publish no later revision, got body {}",
            published.body
        );
        let current = published
            .text("CurrentDatabaseRevision")
            .expect("the read reports the revision the cluster runs");

        // Asking for exactly that revision asks for no move at all, which is
        // the fault the model words for this operation.
        let refused = modify_revision(&emulator, identifier, &current).await;
        assert_eq!(
            (refused.status, refused.error_code().as_deref()),
            (400, Some("ClusterOnLatestRevision")),
            "case {case} with target {current:?}: expected 400 ClusterOnLatestRevision, got \
             {} / {:?} with body {}",
            refused.status,
            refused.error_code(),
            refused.body
        );
        assert_eq!(
            refused.text("Message").as_deref(),
            Some(state::CLUSTER_ALREADY_ON_REQUESTED_REVISION),
            "case {case} with target {current:?}: expected the skipped-modification sentence, \
             got {:?}",
            refused.text("Message")
        );

        // And the refused call changed nothing: the cluster still runs the
        // revision it ran, is still serving, and still reports its revisions.
        let after = emulator
            .call("DescribeClusters", &[("ClusterIdentifier", identifier)])
            .await;
        assert_eq!(
            (
                after.text("ClusterRevisionNumber"),
                after.text("ClusterStatus")
            ),
            (
                Some(current.clone()),
                Some(state::STATUS_AVAILABLE.to_string())
            ),
            "case {case}: expected the cluster untouched, got body {}",
            after.body
        );
        assert_eq!(
            current_revision(&emulator, identifier).await,
            current,
            "case {case}: expected the read to keep reporting the same revision"
        );
    }
}

#[tokio::test]
async fn a_revision_the_cluster_is_not_offered_is_a_parameter_it_cannot_honour() {
    let emulator = Emulator::new();
    let identifier = "revision-unreachable";
    available_cluster(&emulator, identifier).await;
    let current = current_revision(&emulator, identifier).await;

    // The class: well-formed revision ids the cluster does not offer. The rows
    // differ along the FORM of the id — a sibling of the revision the cluster
    // runs (`Cluster$ClusterRevisionNumber` is "the specific revision number of
    // the database in the cluster", which this service reports as a six-digit
    // number), that revision with a digit appended, a dotted version string, a
    // word, a long numeric id, and the empty value the model admits (`length`
    // min 0) — because none of them is a target the cluster publishes, and that
    // is the only thing that decides the answer.
    let sibling = {
        let value: u32 = current
            .parse()
            .expect("the reported revision is a six-digit number");
        let next = if value >= 999_999 { 100_000 } else { value + 1 };
        next.to_string()
    };
    let appended = format!("{current}9");
    let rows: Vec<(&str, String)> = vec![
        ("a six-digit revision one past the one the cluster runs", sibling),
        ("the revision the cluster runs with a digit appended", appended),
        ("a dotted version string", "1.0.99999".to_string()),
        ("a plain word", "bogus-revision".to_string()),
        ("a long numeric id", "20261111163827".to_string()),
        ("the empty value the model admits", String::new()),
    ];

    for (case, target) in rows {
        // A target that IS the revision the cluster runs belongs to the other
        // class, so a row that collides with it would be asserting the wrong
        // rule.
        assert_ne!(
            target, current,
            "case {case:?}: the row must name a revision other than {current:?}"
        );
        let refused = modify_revision(&emulator, identifier, &target).await;
        assert_eq!(
            (refused.status, refused.error_code().as_deref()),
            (400, Some("InvalidParameterValue")),
            "case {case:?} with target {target:?}: expected 400 InvalidParameterValue, got {} / \
             {:?} with body {}",
            refused.status,
            refused.error_code(),
            refused.body
        );
        assert_eq!(
            refused.text("Message").as_deref(),
            Some(state::REVISION_TARGET_NEEDS_INTERMEDIATE_UPGRADE),
            "case {case:?} with target {target:?}: expected the intermediate-upgrade sentence, \
             got {:?}",
            refused.text("Message")
        );

        // The refusal left the cluster where it was: same revision, still
        // serving, and still mutable — tagging it goes on working.
        assert_eq!(
            current_revision(&emulator, identifier).await,
            current,
            "case {case:?}: expected the cluster's revision unchanged by the refusal"
        );
        let tagged = emulator
            .call(
                "CreateTags",
                &[
                    (
                        "ResourceName",
                        state::cluster_arn(identifier, REGION).as_str(),
                    ),
                    ("Tags.Tag.1.Key", "stage"),
                    ("Tags.Tag.1.Value", case),
                ],
            )
            .await;
        assert_eq!(
            tagged.status, 200,
            "case {case:?}: expected the cluster to stay mutable after the refusal, got {} with \
             body {}",
            tagged.status, tagged.body
        );
    }
}

/// One row of the busy-window table: the case name, the cluster identifier, the
/// shape the cluster is created with, and the `ModifyCluster` members whose
/// change is a resize.
struct BusyRow {
    case: &'static str,
    identifier: &'static str,
    created: &'static [(&'static str, &'static str)],
    resize: &'static [(&'static str, &'static str)],
}

/// The rows differ along WHICH part of the cluster's shape the resize changes —
/// the cluster type together with the node count, the node count alone, the node
/// type alone — because the rule is about a shape change, not about one member.
const BUSY_ROWS: &[BusyRow] = &[
    BusyRow {
        case: "a single-node cluster growing to two nodes",
        identifier: "revision-busy-grown",
        created: &[("NodeType", "ra3.xlplus"), ("ClusterType", "single-node")],
        resize: &[
            ("ClusterType", "multi-node"),
            ("NodeType", "ra3.xlplus"),
            ("NumberOfNodes", "2"),
        ],
    },
    BusyRow {
        case: "a multi-node cluster changing its node count only",
        identifier: "revision-busy-shrunk",
        created: &[
            ("NodeType", "ra3.xlplus"),
            ("ClusterType", "multi-node"),
            ("NumberOfNodes", "4"),
        ],
        resize: &[("NumberOfNodes", "2")],
    },
    BusyRow {
        case: "a multi-node cluster changing its node type only",
        identifier: "revision-busy-retyped",
        created: &[
            ("NodeType", "ra3.xlplus"),
            ("ClusterType", "multi-node"),
            ("NumberOfNodes", "2"),
        ],
        resize: &[("NodeType", "ra3.4xlarge")],
    },
];

#[tokio::test]
async fn a_resize_makes_the_cluster_busy_until_its_span_has_passed() {
    let emulator = Emulator::new();
    // The clock is STOPPED for the whole table, so the window a resize opens
    // stays open however long the host takes over the calls inside it, and it
    // closes only where a case advances the clock.
    emulator.stop_clock();

    for row in BUSY_ROWS {
        let case = row.case;
        let identifier = row.identifier;
        let mut params: Vec<(&str, &str)> = vec![
            ("ClusterIdentifier", identifier),
            ("MasterUsername", "admin"),
        ];
        params.extend_from_slice(row.created);
        let created = emulator.call("CreateCluster", &params).await;
        assert_eq!(
            created.status, 200,
            "case {case:?}: expected the seed create to answer 200, got {} with body {}",
            created.status, created.body
        );
        // Cross the provisioning span so the cluster is SERVING before the
        // resize: the window under test is the resize's own, not the create's.
        emulator.advance_clock(state::PROVISIONING_SPAN);
        emulator
            .poll_until(
                case,
                "DescribeClusters",
                &[("ClusterIdentifier", identifier)],
                |answer| answer.text("ClusterStatus").as_deref() == Some(state::STATUS_AVAILABLE),
            )
            .await;
        let revision = current_revision(&emulator, identifier).await;

        let mut params: Vec<(&str, &str)> = vec![("ClusterIdentifier", identifier)];
        params.extend_from_slice(row.resize);
        let resized = emulator.call("ModifyCluster", &params).await;
        assert_eq!(
            resized.status, 200,
            "case {case:?}: expected HTTP 200 from the resizing ModifyCluster, got {} with body \
             {}",
            resized.status, resized.body
        );
        // The call that STARTS the resize reports the work in progress: a
        // caller that polls for it to finish must not be told it is finished
        // before it started.
        let busy = resized.text("ClusterStatus");
        assert_eq!(
            busy.as_deref(),
            Some(state::STATUS_MODIFYING),
            "case {case:?}: expected the resize's own answer to report work in progress, got \
             {busy:?} in body {}",
            resized.body
        );
        let described = emulator
            .call("DescribeClusters", &[("ClusterIdentifier", identifier)])
            .await;
        assert_eq!(
            described.text("ClusterStatus").as_deref(),
            Some(state::STATUS_MODIFYING),
            "case {case:?}: expected the read to keep reporting work in progress, got body {}",
            described.body
        );

        // So a second mutation of the same cluster is refused while the first
        // is applying, and changes nothing.
        let refused = modify_revision(&emulator, identifier, &revision).await;
        assert_eq!(
            (refused.status, refused.error_code().as_deref()),
            (400, Some("InvalidClusterState")),
            "case {case:?}: expected 400 InvalidClusterState during the resize, got {} / {:?} \
             with body {}",
            refused.status,
            refused.error_code(),
            refused.body
        );
        assert_eq!(
            refused.text("Message").as_deref(),
            Some(state::DB_REVISION_MODIFY_REQUIRES_AVAILABLE_CLUSTER),
            "case {case:?}: expected the another-operation-is-running sentence, got {:?}",
            refused.text("Message")
        );
        assert_eq!(
            current_revision(&emulator, identifier).await,
            revision,
            "case {case:?}: expected the racing call to leave the cluster's revision alone"
        );

        // Once the resize's span has passed the cluster is serving again and
        // takes a further modification, which the read then reports.
        emulator.advance_clock(state::PROVISIONING_SPAN);
        emulator
            .poll_until(
                case,
                "DescribeClusters",
                &[("ClusterIdentifier", identifier)],
                |answer| answer.text("ClusterStatus").as_deref() == Some(state::STATUS_AVAILABLE),
            )
            .await;
        let modified = emulator
            .call(
                "ModifyCluster",
                &[
                    ("ClusterIdentifier", identifier),
                    ("ManualSnapshotRetentionPeriod", "3"),
                ],
            )
            .await;
        assert_eq!(
            modified.status, 200,
            "case {case:?}: expected the settled cluster to take a further modify, got {} with \
             body {}",
            modified.status, modified.body
        );
        let after = emulator
            .call("DescribeClusters", &[("ClusterIdentifier", identifier)])
            .await;
        assert_eq!(
            (
                after.text("ManualSnapshotRetentionPeriod").as_deref(),
                after.text("ClusterStatus").as_deref(),
            ),
            (Some("3"), Some(state::STATUS_AVAILABLE)),
            "case {case:?}: expected the further modify to be visible on a serving cluster, got \
             body {}",
            after.body
        );
    }
}
