// SPDX-License-Identifier: CC0-1.0

//! The CENSUS of the Hadoop invocations a cluster's nodes perform, end to end over the wire:
//! which forms of the filesystem shell and of the configuration reader a step's command line
//! really runs, and which forms the node answers for nothing of.
//!
//! THE RULE: the commands the cluster's installed Hadoop distribution ships are commands of a
//! `command-runner.jar` step's command line, and a step's end is the status its command line
//! exited with (`AddJobFlowSteps` doc). What the node provides is a FIXED catalogue: an
//! invocation of it is performed, and every other invocation — another command of the
//! distribution, another form of one of these, an option that changes what one does — is one the
//! node answers for nothing of, which makes the step report that its work was not run rather
//! than a status nothing produced.
//!
//! The rows span that boundary from both sides: each operation of the filesystem shell this
//! emulator performs (over all three filesystems a path's scheme names, and over a path that
//! exists and one that does not), and the forms it performs nothing of. The harness is
//! `node_hadoop_filesystem`'s — Amazon EMR and Amazon S3 on one engine, every object read back
//! through `GetObject` — so each row's verdict is the step's own, read through `ListSteps`.
//!
//! It sorts last among this crate's suites because it launches one cluster per row and reads
//! every one of them to a terminal state.

use serde_json::{json, Value};
use service_com_amazonaws_emr_elasticmapreduce::state::STEP_NOT_EXECUTED_REASON;

use crate::node_hadoop_filesystem::{
    create_bucket, drained, emulator, launch, launch_request, object, put_object, shell,
    step_config, BUCKET,
};

/// What a row expects of its step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Verdict {
    /// The node performed the invocation and it exited zero.
    Performed,
    /// The node performed it and the filesystem refused what it asked for, so the command
    /// exited non-zero.
    Refused,
    /// The node answers for nothing of this invocation.
    NotPerformed,
}

/// The objects this suite seeds: `(key, contents)`.
const SEEDED: [(&str, &str); 1] = [("in/seed.txt", "seeded bytes\n")];

/// Every row: the invocation (as the one command line a step submits) and the verdict it must
/// reach.
fn rows() -> Vec<(&'static str, String, Verdict)> {
    vec![
        // The forms the node performs, over each filesystem and each shape of path.
        (
            "the configuration reader",
            "hdfs getconf -confKey fs.s3.impl".to_string(),
            Verdict::Performed,
        ),
        (
            "the version reader",
            "hadoop version".to_string(),
            Verdict::Performed,
        ),
        (
            "a listing of a place in the object store",
            format!("hadoop fs -ls s3://{BUCKET}/in"),
            Verdict::Performed,
        ),
        (
            "a listing of one object",
            format!("hadoop fs -ls s3://{BUCKET}/in/seed.txt"),
            Verdict::Performed,
        ),
        (
            "a disk-usage summary of a place",
            format!("hadoop fs -du -s s3://{BUCKET}/in"),
            Verdict::Performed,
        ),
        (
            "the statistics of one object",
            format!("hadoop fs -stat '%b %F %n %o %r %y' s3://{BUCKET}/in/seed.txt"),
            Verdict::Performed,
        ),
        (
            "an existence test that holds",
            format!("hadoop fs -test -e s3://{BUCKET}/in/seed.txt"),
            Verdict::Performed,
        ),
        (
            "a quiet removal of an object the store holds nothing at",
            format!("hadoop fs -rm -f s3://{BUCKET}/in/absent.txt"),
            Verdict::Performed,
        ),
        (
            "a directory of HDFS, made and listed",
            "set -eu; hadoop fs -mkdir -p hdfs:///census/one; hdfs dfs -ls hdfs:///census"
                .to_string(),
            Verdict::Performed,
        ),
        (
            "a file moved inside HDFS and read back",
            "set -eu; printf %s moved | hadoop fs -put - hdfs:///census/from.txt; \
             hadoop fs -mv hdfs:///census/from.txt hdfs:///census/to.txt; \
             hadoop fs -cat hdfs:///census/to.txt"
                .to_string(),
            Verdict::Performed,
        ),
        (
            "a subtree of HDFS removed with everything under it",
            "set -eu; printf %s gone | hadoop fs -put - hdfs:///census/tree/deeper/one.txt; \
             hadoop fs -rm -r hdfs:///census/tree; \
             ! hadoop fs -test -e hdfs:///census/tree/deeper/one.txt; \
             ! hadoop fs -test -d hdfs:///census/tree/deeper; \
             ! hadoop fs -test -d hdfs:///census/tree"
                .to_string(),
            Verdict::Performed,
        ),
        (
            "a place of the object store made, then tested and listed",
            format!(
                "set -eu; hadoop fs -mkdir -p s3://{BUCKET}/census/place; \
                 hadoop fs -test -d s3://{BUCKET}/census/place; \
                 hadoop fs -ls s3://{BUCKET}/census/place"
            ),
            Verdict::Performed,
        ),
        (
            "the bytes a nested directory of HDFS holds",
            "set -eu; printf %s 12345 | hadoop fs -put - hdfs:///usage/one.txt; \
             printf %s 67890 | hadoop fs -put - hdfs:///usage/under/two.txt; \
             hadoop fs -du -s hdfs:///usage | cut -d' ' -f1 > /tmp/usage; \
             grep -q '^10$' /tmp/usage"
                .to_string(),
            Verdict::Performed,
        ),
        (
            "a zero-length file created on the node's own filesystem",
            "set -eu; hadoop fs -touchz file:///tmp/census; hadoop fs -test -z file:///tmp/census"
                .to_string(),
            Verdict::Performed,
        ),
        // The forms the filesystem REFUSES, which are the command's own non-zero status.
        (
            "a read of a path the filesystem holds nothing at",
            "hadoop fs -cat hdfs:///census/absent.txt".to_string(),
            Verdict::Refused,
        ),
        (
            "a write over a file the filesystem already holds",
            "set -eu; printf %s one | hadoop fs -put - hdfs:///census/kept.txt; \
             printf %s two > /tmp/two; hadoop fs -put /tmp/two hdfs:///census/kept.txt"
                .to_string(),
            Verdict::Refused,
        ),
        (
            "a removal of a path the filesystem holds nothing at",
            format!("hadoop fs -rm s3://{BUCKET}/in/absent.txt"),
            Verdict::Refused,
        ),
        (
            "a property the configuration does not carry",
            "hdfs getconf -confKey emulator.property.nothing.sets".to_string(),
            Verdict::Refused,
        ),
        // The forms the node answers for nothing of.
        (
            "a command of the distribution this emulator does not provide",
            format!("hadoop distcp s3://{BUCKET}/in/seed.txt hdfs:///census/copied.txt"),
            Verdict::NotPerformed,
        ),
        (
            "an operation of the filesystem shell it does not perform",
            format!("hadoop fs -chmod 777 s3://{BUCKET}/in/seed.txt"),
            Verdict::NotPerformed,
        ),
        (
            "an option that changes what an operation does",
            format!("hadoop fs -du -h s3://{BUCKET}/in/seed.txt"),
            Verdict::NotPerformed,
        ),
        (
            "a scheme this cluster has no filesystem for",
            "hadoop fs -cat gs://census/one.txt".to_string(),
            Verdict::NotPerformed,
        ),
        (
            "the configuration reader with no property named",
            "hdfs getconf -confKey".to_string(),
            Verdict::NotPerformed,
        ),
    ]
}

/// THE RULE over the whole census: each invocation the node provides is performed and its step
/// ends in the status it exited with, and every other invocation leaves the step reporting that
/// its work was not run.
#[tokio::test]
async fn the_invocations_the_node_performs_and_the_ones_it_does_not() {
    let router = emulator();
    create_bucket(&router).await;
    for (key, contents) in SEEDED {
        put_object(&router, key, contents.as_bytes()).await;
    }
    // Nothing is at the key the removal rows name, so each of those rows is about a path the
    // store really holds nothing at.
    assert!(
        object(&router, "in/absent.txt").await.is_none(),
        "the census starts with nothing at in/absent.txt"
    );

    let rows = rows();
    let mut launched: Vec<String> = Vec::new();
    for (label, program, _) in &rows {
        let request = launch_request(label, json!([step_config(label, shell(program))]));
        launched.push(launch(&router, label, request).await);
    }

    for ((label, program, verdict), cluster_id) in rows.iter().zip(&launched) {
        let steps = drained(label, &router, cluster_id).await;
        let step: &Value = steps
            .first()
            .unwrap_or_else(|| panic!("case {label:?}: expected the submitted step back"));
        let state = step["Status"]["State"].as_str().unwrap_or_default();
        let reason = step["Status"]["FailureDetails"]["Reason"]
            .as_str()
            .unwrap_or_default();
        let reached = match (state, reason == STEP_NOT_EXECUTED_REASON) {
            ("COMPLETED", _) => Verdict::Performed,
            ("FAILED", false) => Verdict::Refused,
            ("FAILED", true) => Verdict::NotPerformed,
            _ => panic!("case {label:?}: unexpected end for {program:?}, got {step}"),
        };
        assert_eq!(
            reached, *verdict,
            "case {label:?} ({program:?}): expected {verdict:?}, got {reached:?} from {step}"
        );
    }
}
