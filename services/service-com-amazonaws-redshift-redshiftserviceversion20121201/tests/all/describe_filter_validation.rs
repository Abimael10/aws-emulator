// SPDX-License-Identifier: CC0-1.0

//! What the `Describe*` read path does with the filters and identifiers a request
//! supplies, end to end over the awsQuery wire through the same router the
//! production emulator builds.
//!
//! The rules asserted here, each over a TABLE of inputs rather than one recorded
//! request:
//!
//! * a member documented as having to name an existing catalog entry is RESOLVED
//!   against that catalog, and a value outside it is refused on the parameter
//!   combination; a member documented as a filter PREDICATE selects, so a value
//!   that matches nothing is served with an empty collection;
//! * an identifier an operation resolves is judged on the identifier GRAMMAR
//!   before any lookup, so a value that breaks the grammar is a parameter fault
//!   (400) while a well-formed one that names nothing is the operation's own
//!   not-found fault (404);
//! * the published `MaxRecords` window refuses only below its MINIMUM — a request
//!   for a bigger page than the window names is served, clamped to it;
//! * a value outside a member's published domain is refused with that member's own
//!   sentence, and a BLANK value of a member whose domain admits blank selects
//!   everything, exactly as an omitted member does;
//! * an event read may not reach back past the published retention window;
//! * a read scoped by a caller-supplied `StartTime`/`EndTime` interval refuses an
//!   interval whose end precedes its beginning, whichever read it is.
//!
//! Every refusal is checked on its STATUS, its wire error code and its whole
//! message: a client branches on the code and an operator reads the sentence, so
//! both are part of the behaviour. The expected sentences are written out here
//! rather than taken from the handler's own helpers, so wording that drifts fails
//! this suite.

use std::time::{Duration, SystemTime};

use aws_smithy_types::date_time::Format;
use aws_smithy_types::DateTime;

use service_com_amazonaws_redshift_redshiftserviceversion20121201::state;

use crate::cluster_lifecycle::{all_text, Answer, Emulator};

/// One day, as the event-retention cases measure a period in.
const DAY: Duration = Duration::from_secs(24 * 60 * 60);

/// The fixed sentence Amazon Redshift refuses an identifier that breaks the
/// identifier grammar with, for the parameter name the validator reports.
///
/// Recorded behaviour: the whole sentence is fixed text and the offending value is
/// not echoed, so the only variable part is the parameter name.
fn invalid_identifier_message(parameter: &str) -> String {
    format!(
        "The parameter {parameter} is not a valid identifier. Identifiers must begin with a \
         letter; must contain only ASCII letters, digits, and hyphens; and must not end with a \
         hyphen or contain two consecutive hyphens."
    )
}

/// Fail unless `answer` was served with status 200.
fn assert_served(case: &str, answer: &Answer) {
    assert_eq!(
        answer.status, 200,
        "case {case}: expected status 200, got {} / {:?} with body {}",
        answer.status,
        answer.error_code(),
        answer.body
    );
}

/// Fail unless `answer` is the refusal `status` / `code` / `message` names.
fn assert_refused(case: &str, answer: &Answer, status: u16, code: &str, message: &str) {
    assert_eq!(
        (answer.status, answer.error_code().as_deref()),
        (status, Some(code)),
        "case {case}: expected {status} / {code}, got {} / {:?} with body {}",
        answer.status,
        answer.error_code(),
        answer.body
    );
    assert_eq!(
        answer.text("Message").as_deref(),
        Some(message),
        "case {case}: expected the message {message:?}, got {:?} with body {}",
        answer.text("Message"),
        answer.body
    );
}

/// `at` in the ISO 8601 form the time members are documented to take
/// (`Example: 2009-07-08T18:00Z`).
fn iso8601(at: SystemTime) -> String {
    DateTime::from(at)
        .fmt(Format::DateTime)
        .expect("an instant renders as ISO 8601")
}

/// A name of exactly `len` characters that satisfies every other part of the
/// cluster-identifier grammar: a leading letter, then alphanumerics.
fn name_of_length(len: usize) -> String {
    std::iter::once('c')
        .chain(std::iter::repeat_n('x', len - 1))
        .collect()
}

/// `DescribeOrderableClusterOptions` resolves its `ClusterVersion` against the
/// engine versions this service offers — "Constraints: Must be one of the version
/// returned from DescribeClusterVersions"
/// (`smithy_query(query="doc", shape="DescribeOrderableClusterOptionsMessage",
/// member="ClusterVersion")`) — so a version outside that catalog is refused
/// rather than selecting nothing, while `NodeType`, which carries no such
/// constraint, stays a filter.
#[tokio::test]
async fn an_unoffered_cluster_version_is_refused_and_an_offered_one_selects() {
    let emulator = Emulator::new();

    // The catalog itself, read through the operation the constraint points at, so
    // the rows below are "in it" or "outside it" by the service's own answer.
    let catalog = emulator.call("DescribeClusterVersions", &[]).await;
    assert_served("the engine-version catalog", &catalog);
    let offered = all_text(&catalog.body, "ClusterVersion");
    assert!(
        !offered.is_empty(),
        "case the engine-version catalog: expected at least one offered version, got body {}",
        catalog.body
    );

    // Rows differ along HOW the version misses the catalog: a higher major, a
    // truncated spelling, a longer one, a lower version and one that is not a
    // version at all.
    let outside = [
        "2.0",
        "1",
        "1.0.1",
        "0.9",
        "latest",
    ];
    for version in outside {
        assert!(
            !offered.iter().any(|entry| entry == version),
            "case {version}: the suite's row must name a version the catalog does not offer, \
             which {offered:?} does"
        );
        let answer = emulator
            .call(
                "DescribeOrderableClusterOptions",
                &[("ClusterVersion", version)],
            )
            .await;
        assert_refused(
            version,
            &answer,
            400,
            "InvalidParameterCombination",
            &format!("Cannot find Cluster version {version}"),
        );
    }

    // Every version the catalog DOES offer selects the options carrying it, and a
    // node type nobody offers still selects nothing rather than refusing.
    for version in &offered {
        let answer = emulator
            .call(
                "DescribeOrderableClusterOptions",
                &[("ClusterVersion", version)],
            )
            .await;
        assert_served(version, &answer);
        let versions = all_text(&answer.body, "ClusterVersion");
        assert!(
            !versions.is_empty() && versions.iter().all(|entry| entry == version),
            "case {version}: expected every option to carry it, got {versions:?}"
        );
    }
    let unoffered_node_type = emulator
        .call(
            "DescribeOrderableClusterOptions",
            &[("NodeType", "zz.enormous")],
        )
        .await;
    assert_served("a node type nobody offers", &unoffered_node_type);
    assert!(
        all_text(&unoffered_node_type.body, "NodeType").is_empty(),
        "case a node type nobody offers: expected no option at all, got body {}",
        unoffered_node_type.body
    );
}

/// One row of the source-pairing table: the case name, the request's source
/// parameters, and the sentence the pairing rule refuses it with.
type PairingRow = (&'static str, Vec<(&'static str, &'static str)>, &'static str);

/// `SourceIdentifier` and `SourceType` scope an event read TOGETHER — each
/// member's own documented constraint — and the refusal carries Amazon Redshift's
/// wording for the pairing, which names neither value.
#[tokio::test]
async fn the_two_event_source_members_are_refused_apart() {
    let emulator = Emulator::new();

    // Rows differ along WHICH half is missing, and along the spelling of the half
    // that is present: the sentence names no value, so the same one comes back for
    // every row of a direction.
    let rows: [PairingRow; 5] = [
        (
            "an identifier with no source type",
            vec![("SourceIdentifier", "events-pair-cluster")],
            "Cannot specify source identifier without source type",
        ),
        (
            "another identifier with no source type",
            vec![("SourceIdentifier", "a-very-different-name")],
            "Cannot specify source identifier without source type",
        ),
        (
            "a cluster source type with no identifier",
            vec![("SourceType", "cluster")],
            "Cannot specify source type without source identifier",
        ),
        (
            "a snapshot source type with no identifier",
            vec![("SourceType", "cluster-snapshot")],
            "Cannot specify source type without source identifier",
        ),
        (
            "a parameter-group source type with no identifier",
            vec![("SourceType", "cluster-parameter-group")],
            "Cannot specify source type without source identifier",
        ),
    ];
    for (case, params, message) in rows {
        let answer = emulator.call("DescribeEvents", &params).await;
        assert_refused(case, &answer, 400, "InvalidParameterCombination", message);
    }

    // Both halves together, and neither of them, are served: the refusals above
    // are the pairing rule rather than a blanket refusal.
    let created = emulator.create_minimal("events-pair-cluster").await;
    assert_served("seed events-pair-cluster", &created);
    for (case, params) in [
        ("neither member", Vec::new()),
        (
            "both members",
            vec![
                ("SourceType", "cluster"),
                ("SourceIdentifier", "events-pair-cluster"),
            ],
        ),
    ] {
        let answer = emulator.call("DescribeEvents", &params).await;
        assert_served(case, &answer);
    }
}

/// An event read may not reach back past the published retention window: the log
/// keeps nothing from before it, so a `StartTime` outside it is refused on the
/// request alone.
#[tokio::test]
async fn an_event_read_cannot_reach_back_past_the_retention_window() {
    let emulator = Emulator::new();
    let retention = state::EVENT_RETENTION;
    let minute = Duration::from_secs(60);
    let now = SystemTime::now();

    // The one sentence AWS answers this error class with: entirely fixed text,
    // whose single number is the published retention window itself.
    let expected = format!(
        "Events occurring more than {} days in the past are not available.",
        retention.as_secs() / DAY.as_secs()
    );
    let retention_minutes = retention.as_secs() / 60;

    // Rows differ along HOW far outside the window the period begins and along
    // HOW the request names that beginning: as an instant (`StartTime`) or as the
    // minutes before the request to reach back (`Duration`).
    let refused: Vec<(&str, Vec<(&str, String)>)> = vec![
        (
            "a StartTime a minute past the window",
            vec![("StartTime", iso8601(now - retention - minute))],
        ),
        (
            "a StartTime a day past the window",
            vec![("StartTime", iso8601(now - retention - DAY))],
        ),
        (
            "a StartTime a year ago",
            vec![("StartTime", iso8601(now - 365 * DAY))],
        ),
        (
            "a StartTime at the epoch",
            vec![("StartTime", iso8601(SystemTime::UNIX_EPOCH))],
        ),
        (
            "a Duration one minute past the window",
            vec![("Duration", (retention_minutes + 1).to_string())],
        ),
        (
            "a Duration reaching back a month",
            vec![("Duration", ((30 * DAY).as_secs() / 60).to_string())],
        ),
        (
            "a StartTime past the window with an EndTime inside it",
            vec![
                ("StartTime", iso8601(now - 30 * DAY)),
                ("EndTime", iso8601(now)),
            ],
        ),
        (
            "an EndTime a minute past the window",
            vec![("EndTime", iso8601(now - retention - minute))],
        ),
        (
            "an EndTime a month ago",
            vec![("EndTime", iso8601(now - 30 * DAY))],
        ),
    ];
    for (case, params) in refused {
        let borrowed: Vec<(&str, &str)> = params
            .iter()
            .map(|(name, value)| (*name, value.as_str()))
            .collect();
        let answer = emulator.call("DescribeEvents", &borrowed).await;
        assert_refused(case, &answer, 400, "InvalidParameterCombination", &expected);
    }

    // Inside the window the read is served, from the instant of the request back
    // to the far edge of the window itself.
    for (case, params) in [
        ("no time member at all", Vec::new()),
        (
            "a StartTime a minute ago",
            vec![("StartTime".to_string(), iso8601(now - minute))],
        ),
        (
            "a StartTime a day ago",
            vec![("StartTime".to_string(), iso8601(now - DAY))],
        ),
        (
            "a StartTime well inside the window",
            vec![(
                "StartTime".to_string(),
                iso8601(now - retention + 6 * DAY),
            )],
        ),
        (
            "a StartTime in the future",
            vec![("StartTime".to_string(), iso8601(now + DAY))],
        ),
        (
            "an EndTime an hour ago",
            vec![(
                "EndTime".to_string(),
                iso8601(now - Duration::from_secs(3600)),
            )],
        ),
        (
            "an EndTime well inside the window",
            vec![("EndTime".to_string(), iso8601(now - retention + 6 * DAY))],
        ),
        (
            "a Duration of one minute",
            vec![("Duration".to_string(), "1".to_string())],
        ),
        (
            "a Duration reaching back the whole window",
            vec![("Duration".to_string(), retention_minutes.to_string())],
        ),
    ] {
        let borrowed: Vec<(&str, &str)> = params
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_str()))
            .collect();
        let answer = emulator.call("DescribeEvents", &borrowed).await;
        assert_served(case, &answer);
    }
}

/// An identifier an operation RESOLVES is judged on the identifier grammar before
/// any lookup: one that breaks the grammar could never name a cluster, so it is a
/// parameter fault, while a well-formed one that names nothing is the operation's
/// own not-found fault.
#[tokio::test]
async fn an_identifier_outside_the_grammar_is_a_parameter_fault_not_a_missing_cluster() {
    let emulator = Emulator::new();
    let present = emulator.create_minimal("grammar-present").await;
    assert_served("seed grammar-present", &present);

    // Rows differ along WHICH part of the grammar the value breaks: the leading
    // letter, the alphabet, the trailing hyphen and the doubled hyphen — the four
    // constraints the sentence states.
    let malformed = [
        ("an underscore and capitals", "Sim2real_Probe_Bad"),
        ("an underscore alone", "has_underscore"),
        ("a leading digit", "9leading-digit"),
        ("a leading hyphen", "-leading-hyphen"),
        ("a trailing hyphen", "trailing-hyphen-"),
        ("two consecutive hyphens", "double--hyphen"),
        ("a dot", "dot.name"),
        ("a space", "with space"),
        ("a non-ASCII letter", "café-cluster"),
    ];
    // The parameter each operation's validator names the offending member by.
    let operations = [
        ("DescribeClusters", "DBInstanceIdentifier"),
        ("DescribeLoggingStatus", "ClusterIdentifier"),
    ];
    // The documented maximum length of a cluster identifier: "Must contain from 1
    // to 63 alphanumeric characters or hyphens"
    // (`smithy_query(query="doc", shape="CreateClusterMessage",
    // member="ClusterIdentifier")`).
    let max_len = state::CLUSTER_IDENTIFIER_RULE.max_len;
    for (action, parameter) in operations {
        let expected = invalid_identifier_message(parameter);
        for (why, identifier) in malformed {
            let case = format!("{action} with {why}");
            let answer = emulator
                .call(action, &[("ClusterIdentifier", identifier)])
                .await;
            assert_refused(&case, &answer, 400, "InvalidParameterValue", &expected);
        }

        // Length is judged FIRST and has its own sentence, so a value that is
        // both too long and outside the alphabet is refused for its length. The
        // rows are the published maximum (admitted, so the bound itself is not a
        // refusal), one past it, far past it, and one past it that also breaks
        // the alphabet.
        let too_long = format!(
            "{action} refuses a name longer than the published maximum of {max_len}"
        );
        let expected_long = format!(
            "The parameter {parameter} is not a valid identifier because it is longer than \
             {max_len} characters."
        );
        for (why, identifier) in [
            ("one character past the maximum", name_of_length(max_len + 1)),
            ("far past the maximum", name_of_length(4 * max_len)),
            (
                "past the maximum and outside the alphabet",
                format!("{}_tail", name_of_length(max_len)),
            ),
        ] {
            let case = format!("{too_long}: {why}");
            let answer = emulator
                .call(action, &[("ClusterIdentifier", &identifier)])
                .await;
            assert_refused(&case, &answer, 400, "InvalidParameterValue", &expected_long);
        }

        // A WELL-FORMED name that names no cluster is the lookup's own fault, and
        // the rows span the spelling, the case and the length the grammar admits.
        for (why, identifier) in [
            ("a plain absent name", "no-such-cluster".to_string()),
            ("a one-letter absent name", "c".to_string()),
            ("an absent name in mixed case", "No-Such-Cluster".to_string()),
            ("an absent name of digits after a letter", "c123456".to_string()),
            ("a hyphenated absent name", "a-b-c-d".to_string()),
            ("an absent name of the greatest length", name_of_length(max_len)),
        ] {
            let case = format!("{action} with {why}");
            let answer = emulator
                .call(action, &[("ClusterIdentifier", &identifier)])
                .await;
            assert_eq!(
                (answer.status, answer.error_code().as_deref()),
                (404, Some("ClusterNotFound")),
                "case {case}: expected 404 / ClusterNotFound, got {} / {:?} with body {}",
                answer.status,
                answer.error_code(),
                answer.body
            );
        }

        // The cluster that IS there is still served through the same operation, so
        // the refusals above are the grammar and not a blanket refusal.
        let served = emulator
            .call(action, &[("ClusterIdentifier", "grammar-present")])
            .await;
        assert_served(&format!("{action} with the stored cluster"), &served);
    }
}

/// The published `MaxRecords` window refuses only below its MINIMUM: a request for
/// a bigger page than the window names is served, clamped to the window's
/// maximum, and the refusal carries the window's two bounds without echoing the
/// value the request sent.
#[tokio::test]
async fn the_max_records_window_refuses_only_below_its_minimum() {
    let emulator = Emulator::new();
    let created = emulator.create_minimal("max-records-cluster").await;
    assert_served("seed max-records-cluster", &created);

    // One row per paginated read and the window IT publishes: most of this
    // service's `Describe*` operations publish 20..100
    // (`smithy_query(query="doc", shape="DescribeClustersMessage",
    // member="MaxRecords")` — "Constraints: minimum 20, maximum 100"), while
    // `DescribeTags` publishes its own ("The MaxRecords parameter can be a range
    // from 10 to 50 results to return in a request").
    let windows: [(&str, i32, i32); 5] = [
        (
            "DescribeClusters",
            state::MAX_RECORDS_MIN,
            state::MAX_RECORDS_MAX,
        ),
        (
            "DescribeClusterSnapshots",
            state::MAX_RECORDS_MIN,
            state::MAX_RECORDS_MAX,
        ),
        (
            "DescribeEvents",
            state::MAX_RECORDS_MIN,
            state::MAX_RECORDS_MAX,
        ),
        (
            "DescribeClusterVersions",
            state::MAX_RECORDS_MIN,
            state::MAX_RECORDS_MAX,
        ),
        (
            "DescribeTags",
            state::TAG_MAX_RECORDS_WINDOW.min,
            state::TAG_MAX_RECORDS_WINDOW.max,
        ),
    ];

    for (action, min, max) in windows {
        let expected = format!("Invalid value for max records. Must be between {min} and {max}");
        // Below the minimum: the published floor itself less one, a page of one,
        // none at all and a negative page.
        for value in [min - 1, 1, 0, -1] {
            let case = format!("{action} with MaxRecords {value}");
            let answer = emulator
                .call(action, &[("MaxRecords", &value.to_string())])
                .await;
            assert_refused(&case, &answer, 400, "InvalidParameterValue", &expected);
        }
        // The minimum, the maximum, and values past the maximum: all served, the
        // last clamped to the window rather than refused.
        for value in [min, min + 1, max, max + 1, 10 * max, 1_000_000] {
            let case = format!("{action} with MaxRecords {value}");
            let answer = emulator
                .call(action, &[("MaxRecords", &value.to_string())])
                .await;
            assert_served(&case, &answer);
        }
    }
}

/// A `Marker` this service did not issue is not a refusal: it names a position
/// past the end of the sequence, so the read is SERVED with the empty page that
/// position holds — a pagination loop handed a cursor the service cannot place
/// terminates instead of aborting on a call the service answers.
#[tokio::test]
async fn a_cursor_the_service_cannot_place_is_served_as_an_empty_last_page() {
    let emulator = Emulator::new();
    let created = emulator.create_minimal("cursor-cluster").await;
    assert_served("seed cursor-cluster", &created);

    // Rows differ along the SHAPE of the cursor: a word, a bare offset, this
    // service's own cursor shape with the wrong tag, and an offset far past the
    // collection.
    let cursors = ["not-a-real-marker", "7", "0.deadbeef", "999999.deadbeef"];
    // One row per paginated read, so the rule is the shared cursor's rather than
    // one operation's: a collection with records in it, and catalogs without.
    let reads = [
        ("DescribeClusters", "ClusterIdentifier"),
        ("DescribeClusterSnapshots", "SnapshotIdentifier"),
        ("DescribeClusterVersions", "ClusterVersion"),
        ("DescribeEvents", "EventId"),
        ("DescribeTags", "Key"),
    ];
    for (action, record_member) in reads {
        for cursor in cursors {
            let case = format!("{action} resuming from {cursor:?}");
            let answer = emulator.call(action, &[("Marker", cursor)]).await;
            assert_served(&case, &answer);
            assert_eq!(
                answer.text("Marker"),
                None,
                "case {case}: nothing follows the end of the sequence, so expected no cursor, \
                 got {:?}",
                answer.text("Marker")
            );
            assert!(
                all_text(&answer.body, record_member).is_empty(),
                "case {case}: expected the empty page that position holds, got {:?}",
                all_text(&answer.body, record_member)
            );
        }
    }

    // The same reads WITHOUT a cursor still carry their records, so the empty
    // pages above are the cursor's position and not a broken read.
    let unpaged = emulator.call("DescribeClusters", &[]).await;
    assert_served("the whole collection", &unpaged);
    assert_eq!(
        all_text(&unpaged.body, "ClusterIdentifier"),
        vec!["cursor-cluster".to_string()],
        "case the whole collection: expected the seeded cluster, got {:?}",
        all_text(&unpaged.body, "ClusterIdentifier")
    );
}

/// A `SnapshotType` outside the published domain is refused with the member's own
/// sentence, while a BLANK one names no type at all and so selects every
/// snapshot, exactly as an omitted member does.
#[tokio::test]
async fn the_snapshot_type_domain_admits_blank_and_refuses_anything_else() {
    let emulator = Emulator::new();
    let created = emulator.create_minimal("snapshot-type-cluster").await;
    assert_served("seed snapshot-type-cluster", &created);
    for snapshot in ["type-snap-1", "type-snap-2"] {
        let answer = emulator
            .call(
                "CreateClusterSnapshot",
                &[
                    ("SnapshotIdentifier", snapshot),
                    ("ClusterIdentifier", "snapshot-type-cluster"),
                ],
            )
            .await;
        assert_served(&format!("seed {snapshot}"), &answer);
    }

    // Rows differ along HOW the value misses the domain: a truncation, a case
    // difference, a plural, a neighbouring word and a value from another member's
    // vocabulary.
    for value in ["auto", "Manual", "manuals", "hourly", "cluster-snapshot"] {
        let answer = emulator
            .call("DescribeClusterSnapshots", &[("SnapshotType", value)])
            .await;
        assert_refused(
            value,
            &answer,
            400,
            "InvalidParameterValue",
            &format!("Invalid snapshot type '{value}'.  Must be blank or one of: manual, automated."),
        );
    }

    // Every snapshot the read reports when nothing narrows it, which is what a
    // blank type must select too.
    let unfiltered = emulator.call("DescribeClusterSnapshots", &[]).await;
    assert_served("no snapshot type at all", &unfiltered);
    let mut every = all_text(&unfiltered.body, "SnapshotIdentifier");
    every.sort();
    assert_eq!(
        every,
        vec!["type-snap-1".to_string(), "type-snap-2".to_string()],
        "case no snapshot type at all: expected both seeded snapshots, got {every:?}"
    );

    for (case, value) in [("an empty type", ""), ("a blank type", " ")] {
        let answer = emulator
            .call("DescribeClusterSnapshots", &[("SnapshotType", value)])
            .await;
        assert_served(case, &answer);
        let mut listed = all_text(&answer.body, "SnapshotIdentifier");
        listed.sort();
        assert_eq!(
            listed, every,
            "case {case}: expected the same snapshots an omitted type selects ({every:?}), got \
             {listed:?}"
        );
    }

    // The two values the domain names each select their own snapshots, so the
    // member still filters.
    for (case, value, expected) in [
        (
            "the manual type",
            "manual",
            vec!["type-snap-1".to_string(), "type-snap-2".to_string()],
        ),
        ("the automated type", "automated", Vec::new()),
    ] {
        let answer = emulator
            .call("DescribeClusterSnapshots", &[("SnapshotType", value)])
            .await;
        assert_served(case, &answer);
        let mut listed = all_text(&answer.body, "SnapshotIdentifier");
        listed.sort();
        assert_eq!(
            listed, expected,
            "case {case}: expected {expected:?}, got {listed:?}"
        );
    }
}

/// `DescribeClusterSnapshots` FILTERS on the `ClusterIdentifier` it is given —
/// "the identifier of the cluster which generated the requested snapshots" — so an
/// identifier that matches no snapshot is served with an empty collection instead
/// of being resolved as a cluster this account must hold.
#[tokio::test]
async fn a_snapshot_read_filters_by_cluster_identifier_instead_of_resolving_it() {
    let emulator = Emulator::new();
    for cluster in ["snap-filter-a", "snap-filter-b", "snap-filter-gone"] {
        let answer = emulator.create_minimal(cluster).await;
        assert_served(&format!("seed {cluster}"), &answer);
    }
    for (snapshot, cluster) in [
        ("filter-snap-a1", "snap-filter-a"),
        ("filter-snap-a2", "snap-filter-a"),
        ("filter-snap-gone", "snap-filter-gone"),
    ] {
        let answer = emulator
            .call(
                "CreateClusterSnapshot",
                &[
                    ("SnapshotIdentifier", snapshot),
                    ("ClusterIdentifier", cluster),
                ],
            )
            .await;
        assert_served(&format!("seed {snapshot}"), &answer);
    }
    // A cluster that is GONE still has the snapshots it generated, so its
    // identifier selects them while naming no cluster the account holds.
    let deleted = emulator
        .call(
            "DeleteCluster",
            &[
                ("ClusterIdentifier", "snap-filter-gone"),
                ("SkipFinalClusterSnapshot", "true"),
            ],
        )
        .await;
    assert_served("delete snap-filter-gone", &deleted);

    // Rows differ along WHAT the identifier matches: a cluster with snapshots, a
    // cluster with none, a deleted cluster whose snapshots remain, a name nothing
    // ever created, one differing only in case, and one that is not even a legal
    // identifier — none of which is an error, because the member is a predicate.
    let rows: [(&str, &str, Vec<&str>); 6] = [
        (
            "a cluster with snapshots",
            "snap-filter-a",
            vec!["filter-snap-a1", "filter-snap-a2"],
        ),
        ("a cluster with no snapshot", "snap-filter-b", Vec::new()),
        (
            "a deleted cluster whose snapshots remain",
            "snap-filter-gone",
            vec!["filter-snap-gone"],
        ),
        ("a name nothing ever created", "nowhere-cluster", Vec::new()),
        (
            "a stored name in another case",
            "SNAP-FILTER-A",
            Vec::new(),
        ),
        (
            "a name no cluster could ever carry",
            "Not_A_Cluster",
            Vec::new(),
        ),
    ];
    for (case, cluster_identifier, expected) in rows {
        let answer = emulator
            .call(
                "DescribeClusterSnapshots",
                &[("ClusterIdentifier", cluster_identifier)],
            )
            .await;
        assert_served(case, &answer);
        let mut listed = all_text(&answer.body, "SnapshotIdentifier");
        listed.sort();
        let mut expected: Vec<String> = expected.iter().map(|id| (*id).to_string()).collect();
        expected.sort();
        assert_eq!(
            listed, expected,
            "case {case}: expected the snapshots {expected:?}, got {listed:?}"
        );
    }

    // `SnapshotIdentifier` is NOT a predicate: it names one snapshot, and naming
    // none this service holds is that resource's own modelled fault, which is what
    // makes the empty collections above a filter rather than a swallowed refusal.
    let missing = emulator
        .call(
            "DescribeClusterSnapshots",
            &[("SnapshotIdentifier", "nowhere-snapshot")],
        )
        .await;
    assert_eq!(
        (missing.status, missing.error_code().as_deref()),
        (404, Some("ClusterSnapshotNotFound")),
        "case a snapshot identifier nothing created: expected 404 / ClusterSnapshotNotFound, got \
         {} / {:?} with body {}",
        missing.status,
        missing.error_code(),
        missing.body
    );
}

/// The reads of this service whose input carries the `StartTime`/`EndTime` pair
/// that names the interval they report over, which is the whole class the rule
/// below holds over.
///
/// From the model: `DescribeEventsMessage`, `DescribeClusterSnapshotsMessage` and
/// `DescribeScheduledActionsMessage` are the input shapes carrying both members
/// (`smithy_query(query="operation",
/// operation="DescribeEvents,DescribeClusterSnapshots,DescribeScheduledActions")`).
/// `CreateScheduledAction` / `ModifyScheduledAction` carry the pair too, but there
/// it is the action's own activation window — a value stored and reported back,
/// not the interval of a read — so they are not of this class.
const INTERVAL_SCOPED_READS: [&str; 3] = [
    "DescribeEvents",
    "DescribeClusterSnapshots",
    "DescribeScheduledActions",
];

/// A read scoped by a caller-supplied time interval refuses, on the request
/// alone, an interval whose end precedes its beginning: such an interval holds no
/// instant, so there is nothing to report over and nothing the caller could have
/// meant by it.
///
/// The sentence is entirely fixed text — it names neither member and echoes
/// neither instant — and the refusal is the query-protocol parameter-combination
/// one, because each end is an acceptable instant on its own and only the pair is
/// impossible.
#[tokio::test]
async fn a_read_whose_interval_ends_before_it_begins_is_refused() {
    let emulator = Emulator::new();
    let now = SystemTime::now();
    let second = Duration::from_secs(1);
    let minute = Duration::from_secs(60);
    let half_hour = Duration::from_secs(30 * 60);

    // The one sentence AWS answers this error class with.
    let expected = "End time must not precede start time";

    // Rows differ along HOW FAR the end precedes the beginning (a second, a
    // minute, half an hour), where the interval sits relative to the request's
    // own instant (behind it, straddling it, ahead of it), and which ends the
    // request named at all — the two degenerate cases (an interval of zero width,
    // and one end alone) being the boundary the refusal stops at. Every instant
    // stays inside the published event-retention window, so no row can pass by
    // being refused for reaching too far back.
    let rows: Vec<(&str, Option<SystemTime>, Option<SystemTime>, bool)> = vec![
        ("neither end named", None, None, false),
        ("the beginning alone", Some(now - half_hour), None, false),
        ("the end alone", None, Some(now - half_hour), false),
        (
            "an interval of zero width",
            Some(now - minute),
            Some(now - minute),
            false,
        ),
        (
            "an interval half an hour wide",
            Some(now - half_hour),
            Some(now),
            false,
        ),
        (
            "an interval a minute wide, ahead of the request",
            Some(now + minute),
            Some(now + 2 * minute),
            false,
        ),
        (
            "an end one second before its start",
            Some(now - minute),
            Some(now - minute - second),
            true,
        ),
        (
            "an end one minute before its start",
            Some(now - minute),
            Some(now - 2 * minute),
            true,
        ),
        (
            "an end half an hour before its start",
            Some(now),
            Some(now - half_hour),
            true,
        ),
        (
            "an inverted interval entirely ahead of the request",
            Some(now + half_hour),
            Some(now + minute),
            true,
        ),
    ];

    for action in INTERVAL_SCOPED_READS {
        for (row, start, end, refused) in &rows {
            let mut params: Vec<(&str, String)> = Vec::new();
            if let Some(at) = start {
                params.push(("StartTime", iso8601(*at)));
            }
            if let Some(at) = end {
                params.push(("EndTime", iso8601(*at)));
            }
            let borrowed: Vec<(&str, &str)> = params
                .iter()
                .map(|(name, value)| (*name, value.as_str()))
                .collect();
            let answer = emulator.call(action, &borrowed).await;
            let case = format!("{action} with {row}");
            if *refused {
                assert_refused(&case, &answer, 400, "InvalidParameterCombination", expected);
            } else {
                assert_served(&case, &answer);
            }
        }
    }
}
