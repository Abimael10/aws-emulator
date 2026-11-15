// SPDX-License-Identifier: CC0-1.0

//! `DescribeClusterDbRevisions` as a REPORT, end to end over the awsQuery wire.
//!
//! The RULE every case here asserts: the operation answers out of the cluster
//! records this service already holds — each cluster's own identifier, the
//! `ClusterRevisionNumber` its record carries, the instant it was put on that
//! revision and the revisions it can be moved onto — whatever lifecycle status
//! the cluster is in, so what this operation says about a cluster and what
//! `DescribeClusters` says about it cannot disagree; and because it only
//! reports, calling it neither changes those facts nor moves a cluster's
//! lifecycle on, so a transition stays owed to the clock.
//!
//! The rows differ along what the rule generalises over: the lifecycle status
//! the cluster is in when it is read (being provisioned, serving, restarting,
//! having its compute suspended, suspended), whether the read names one cluster
//! or none while the account holds several, how many times it is read, and why a
//! read is refused.
//!
//! Every case that observes a TRANSIENT status stops the emulator's clock first
//! and then moves it on by the span the transition is due after, so no assertion
//! here depends on how fast the host runs.

use service_com_amazonaws_redshift_redshiftserviceversion20121201::state;

use crate::cluster_lifecycle::{all_text, element_blocks, Answer, Emulator};

/// The admin password every cluster in this suite is created with.
const PASSWORD: &str = "TestPassw0rd123";

/// How many FRESH reads a case makes of one cluster inside a stopped-clock span,
/// to show that reading materialises nothing the clock has not already made due.
///
/// The clock is STOPPED while they are made, so the span stays open across all of
/// them however long the host takes: an implementation where a read settled the
/// cluster it reports fails on the first one, and no number of them can fail a
/// correct implementation.
const READS_INSIDE_THE_SPAN: usize = 3;

/// The revision facts the report carries for ONE cluster.
#[derive(Debug, PartialEq, Eq)]
struct Reported {
    identifier: Option<String>,
    current_revision: Option<String>,
    release_date: Option<String>,
    /// The revisions the cluster can be moved onto, by the `DatabaseRevision`
    /// each `RevisionTarget` names.
    targets: Vec<String>,
}

impl Reported {
    /// The facts one served `<ClusterDbRevision>` record carries.
    fn of(record: &str) -> Self {
        Self {
            identifier: first(record, "ClusterIdentifier"),
            current_revision: first(record, "CurrentDatabaseRevision"),
            release_date: first(record, "DatabaseRevisionReleaseDate"),
            targets: all_text(record, "DatabaseRevision"),
        }
    }
}

/// One refused-read row: the case name, the parameters the read sends, and the
/// status and error code it is refused with.
type RefusalCase<'a> = (&'a str, Vec<(&'a str, &'a str)>, u16, &'a str);

/// The text of the first element with this name in a served body or record.
fn first(xml: &str, tag: &str) -> Option<String> {
    all_text(xml, tag).into_iter().next()
}

/// `CreateCluster` for one cluster of this shape, asserting the seed was served.
async fn create(emulator: &Emulator, identifier: &str, shape: &[(&str, &str)]) {
    let mut params: Vec<(&str, &str)> = vec![
        ("ClusterIdentifier", identifier),
        ("MasterUsername", "adminuser"),
        ("MasterUserPassword", PASSWORD),
    ];
    params.extend_from_slice(shape);
    let created = emulator.call("CreateCluster", &params).await;
    assert_eq!(
        created.status, 200,
        "case seed cluster {identifier}: expected the create served, got {} with body {}",
        created.status, created.body
    );
}

/// `DescribeClusterDbRevisions` with these parameters.
async fn report(emulator: &Emulator, params: &[(&str, &str)]) -> Answer {
    emulator.call("DescribeClusterDbRevisions", params).await
}

/// The ONE record a read naming a single cluster serves.
fn sole_record(case: &str, answer: &Answer) -> Reported {
    assert_eq!(
        answer.status, 200,
        "case {case}: expected the report served, got {} with body {}",
        answer.status, answer.body
    );
    let records = element_blocks(&answer.body, "ClusterDbRevision");
    assert_eq!(
        records.len(),
        1,
        "case {case}: expected one record for the cluster asked about, got body {}",
        answer.body
    );
    Reported::of(&records[0])
}

/// Move the clock on by the span a cluster transition is due after and wait for
/// the CONDITION that the cluster reports `status`.
///
/// Nothing sleeps for the span: the due instant was recorded by the call that
/// started the transition, and the advance crosses it.
async fn settle(emulator: &Emulator, identifier: &str, status: &str) {
    emulator.advance_clock(state::PROVISIONING_SPAN);
    emulator
        .poll_until(
            &format!("cluster {identifier} settling to {status}"),
            "DescribeClusters",
            &[("ClusterIdentifier", identifier)],
            |answer| answer.text("ClusterStatus").as_deref() == Some(status),
        )
        .await;
}

/// Assert that the report says about `under_test` exactly what the cluster's own
/// record says, both when it is the cluster asked about and when the read names
/// none — and that the account's other cluster is reported beside it.
async fn assert_report_agrees(
    emulator: &Emulator,
    case: &str,
    under_test: &str,
    peer: &str,
    expected_status: &str,
) {
    let described = emulator
        .call("DescribeClusters", &[("ClusterIdentifier", under_test)])
        .await;
    assert_eq!(
        described.status, 200,
        "case {case}: expected the cluster read served, got {} with body {}",
        described.status, described.body
    );
    assert_eq!(
        described.text("ClusterStatus").as_deref(),
        Some(expected_status),
        "case {case}: expected the cluster in the status the stage puts it in, got body {}",
        described.body
    );

    // What the cluster's OWN record says, which is what the report has to say.
    // This service publishes no revision of a cluster's database later than the
    // one it runs, so there is no target to be moved onto.
    let expected = Reported {
        identifier: Some(under_test.to_string()),
        current_revision: described.text("ClusterRevisionNumber"),
        release_date: described.text("ClusterCreateTime"),
        targets: Vec::new(),
    };
    assert!(
        expected.current_revision.is_some() && expected.release_date.is_some(),
        "case {case}: expected the cluster record to carry a revision and the instant it was put \
         on it, got body {}",
        described.body
    );

    let named = report(emulator, &[("ClusterIdentifier", under_test)]).await;
    assert_eq!(
        sole_record(case, &named),
        expected,
        "case {case}: expected the record read off the cluster's own facts, got body {}",
        named.body
    );

    // Naming no cluster reports every cluster of the account, in the order the
    // account created them, and says the same about this one as the named read.
    let everything = report(emulator, &[]).await;
    assert_eq!(
        all_text(&everything.body, "ClusterIdentifier"),
        vec![peer.to_string(), under_test.to_string()],
        "case {case}: expected every cluster of the account in creation order, got body {}",
        everything.body
    );
    let mine = element_blocks(&everything.body, "ClusterDbRevision")
        .into_iter()
        .find(|record| first(record, "ClusterIdentifier").as_deref() == Some(under_test))
        .unwrap_or_else(|| {
            panic!(
                "case {case}: expected the unfiltered report to carry the cluster, got body {}",
                everything.body
            )
        });
    assert_eq!(
        Reported::of(&mine),
        expected,
        "case {case}: expected the unfiltered report to say what the named read said, got body {}",
        everything.body
    );
}

/// The report is read off the cluster record in EVERY lifecycle status a cluster
/// passes through, not only while it is serving.
#[tokio::test]
async fn the_report_agrees_with_the_cluster_record_in_every_lifecycle_status() {
    let emulator = Emulator::new();
    // Nothing this service owes can fall due until a stage moves the clock on,
    // so every transient status below stays observable however slow the host is.
    emulator.stop_clock();

    // The peer is of another shape and is held serving throughout: the report is
    // per cluster, so what the cluster under test is doing cannot change what is
    // reported about it.
    let peer = "revision-stage-peer";
    create(
        &emulator,
        peer,
        &[
            ("NodeType", "dc2.large"),
            ("ClusterType", "multi-node"),
            ("NumberOfNodes", "2"),
        ],
    )
    .await;
    settle(&emulator, peer, state::STATUS_AVAILABLE).await;

    let under_test = "revision-stage";
    create(
        &emulator,
        under_test,
        &[("NodeType", "ra3.xlplus"), ("ClusterType", "single-node")],
    )
    .await;

    // Inside the span the create started, the cluster is still being provisioned
    // — and its revision is already a fact of its record.
    assert_report_agrees(
        &emulator,
        "a cluster still being provisioned",
        under_test,
        peer,
        state::STATUS_CREATING,
    )
    .await;

    // Serving, once that span has passed.
    settle(&emulator, under_test, state::STATUS_AVAILABLE).await;
    assert_report_agrees(
        &emulator,
        "a cluster serving",
        under_test,
        peer,
        state::STATUS_AVAILABLE,
    )
    .await;

    // Restarting: a momentary outage the cluster comes back from.
    let rebooted = emulator
        .call("RebootCluster", &[("ClusterIdentifier", under_test)])
        .await;
    assert_eq!(
        rebooted.status, 200,
        "case the restart of the cluster: expected 200, got {} with body {}",
        rebooted.status, rebooted.body
    );
    assert_report_agrees(
        &emulator,
        "a cluster restarting",
        under_test,
        peer,
        state::STATUS_REBOOTING,
    )
    .await;
    settle(&emulator, under_test, state::STATUS_AVAILABLE).await;

    // Having its compute suspended, and then suspended: a cluster that is not
    // serving at all is still reported, out of the same record.
    let paused = emulator
        .call("PauseCluster", &[("ClusterIdentifier", under_test)])
        .await;
    assert_eq!(
        paused.status, 200,
        "case the suspension of the cluster: expected 200, got {} with body {}",
        paused.status, paused.body
    );
    assert_report_agrees(
        &emulator,
        "a cluster whose compute is being suspended",
        under_test,
        peer,
        state::STATUS_MODIFYING,
    )
    .await;
    settle(&emulator, under_test, state::STATUS_PAUSED).await;
    assert_report_agrees(
        &emulator,
        "a cluster whose compute is suspended",
        under_test,
        peer,
        state::STATUS_PAUSED,
    )
    .await;
}

/// Reading the report — however often, and whether it is served or refused —
/// leaves the cluster exactly as a later caller would have found it.
#[tokio::test]
async fn reading_the_report_changes_nothing_a_later_caller_sees() {
    let emulator = Emulator::new();
    emulator.stop_clock();

    let identifier = "revision-read-only";
    create(
        &emulator,
        identifier,
        &[("NodeType", "ra3.xlplus"), ("ClusterType", "single-node")],
    )
    .await;

    let first_read = report(&emulator, &[("ClusterIdentifier", identifier)]).await;
    let facts = sole_record("the first read", &first_read);

    // Fresh reads inside the span the create started: the clock is stopped, so
    // an implementation where a read settles the cluster it reports would show
    // it here, on the cluster's own status as well as in the report.
    for read in 1..=READS_INSIDE_THE_SPAN {
        let again = report(&emulator, &[("ClusterIdentifier", identifier)]).await;
        let case = format!("read {read} of {READS_INSIDE_THE_SPAN} inside the span");
        assert_eq!(
            sole_record(&case, &again),
            facts,
            "case {case}: expected the same facts as the first read, got body {}",
            again.body
        );
        let described = emulator
            .call("DescribeClusters", &[("ClusterIdentifier", identifier)])
            .await;
        assert_eq!(
            described.text("ClusterStatus").as_deref(),
            Some(state::STATUS_CREATING),
            "case {case}: expected the cluster still being provisioned, got body {}",
            described.body
        );
    }

    // A REFUSED read changes nothing either. The rows differ along why the read
    // is refused: the cluster it names, the page size it asks for, and the
    // parameters it combines.
    let below_the_window = (state::MAX_RECORDS_MIN - 1).to_string();
    let rows: Vec<RefusalCase<'_>> = vec![
        (
            "an identifier that names no cluster",
            vec![("ClusterIdentifier", "no-such-cluster")],
            404,
            "ClusterNotFound",
        ),
        (
            "a page smaller than the documented minimum",
            vec![("MaxRecords", below_the_window.as_str())],
            400,
            "InvalidParameterValue",
        ),
        (
            "an identifier and a marker together",
            vec![("ClusterIdentifier", identifier), ("Marker", "0")],
            400,
            "InvalidParameterCombination",
        ),
    ];
    for (case, params, status, code) in rows {
        let refused = report(&emulator, &params).await;
        assert_eq!(
            (refused.status, refused.error_code().as_deref()),
            (status, Some(code)),
            "case {case}: expected {status} / {code}, got {} / {:?} with body {}",
            refused.status,
            refused.error_code(),
            refused.body
        );
    }
    let after_refusals = report(&emulator, &[("ClusterIdentifier", identifier)]).await;
    assert_eq!(
        sole_record("after the refused reads", &after_refusals),
        facts,
        "case after the refused reads: expected the cluster's facts untouched, got body {}",
        after_refusals.body
    );

    // The move to serving was still OWED to the clock: it falls due when the
    // span passes, not when somebody read — and the revision facts, which are
    // the cluster's own rather than its status's, do not move with it.
    settle(&emulator, identifier, state::STATUS_AVAILABLE).await;
    let after_settling = report(&emulator, &[("ClusterIdentifier", identifier)]).await;
    assert_eq!(
        sole_record("after the cluster settled", &after_settling),
        facts,
        "case after the cluster settled: expected the same revision and release date, got body {}",
        after_settling.body
    );
}
