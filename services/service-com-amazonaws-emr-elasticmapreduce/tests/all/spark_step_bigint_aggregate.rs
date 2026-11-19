// SPDX-License-Identifier: CC0-1.0

//! A step that submits a Spark application aggregating a SPAN of bigints, end to end over the
//! wire, with Amazon S3 mounted beside Amazon EMR on one engine (the harness
//! `tests/all/spark_step_application.rs` owns).
//!
//! THE RULE: an aggregate a submitted application asks for is COMPUTED over the dataset it
//! names, and `spark.range` names a dataset of a bigint column — so `sum(id)` over it answers
//! the exact total of that span, of type `bigint`, however many values the span holds and
//! however many partitions it is spread over. A total that a 64-bit float cannot hold exactly
//! is still exact, because the integers it is made of are: that is what makes the sum of a
//! bigint column worth asking for.
//!
//! The rows span what the rule generalises over: how large the span is (a handful of values,
//! and spans of a million and of two hundred million, whose totals are far past the integers a
//! float64 holds exactly), where it starts, the step between its values, how many partitions it
//! is spread over, and a span that holds nothing at all. Every expected total is this suite's
//! OWN arithmetic — the closed form of the progression, computed here in 128-bit integers — so
//! nothing below is read back from what the emulator answered.
//!
//! Each application also asks for the same total with its values read as doubles
//! (`sum(cast(id as double))`), which is the comparison a client makes to see what the exact
//! total costs in a float: the double is the nearest one to the true total, so for a total past
//! the exactly-representable integers it differs from it.

use serde_json::json;

use crate::spark_step_application::{
    drained, emulator, launch, object, put_object, s3, BUCKET,
};

/// The application every row submits: it aggregates the span its arguments name and writes
/// what it came to — the exact total, the type that total has, how many values the span held
/// and the same total read as a double — as one line in Amazon S3.
const APPLICATION: &str = r#"
import sys
from pyspark.sql import SparkSession

out, start, end, step, parts = sys.argv[1:6]
spark = SparkSession.builder.appName("bigint-aggregate").getOrCreate()
span = spark.range(int(start), int(end), int(step), int(parts))
exact = span.selectExpr("sum(id) as total")
approx = span.selectExpr("sum(cast(id as double)) as total")
unaliased = span.selectExpr("sum(cast(id as double))")
report = "%s|%s|%s|%r|%r" % (
    exact.collect()[0]["total"],
    exact.schema[0].dataType.simpleString(),
    span.count(),
    approx.collect()[0]["total"],
    unaliased.collect()[0][0],
)
spark.sparkContext.parallelize([report], 1).saveAsTextFile(out)
spark.stop()
"#;

/// One row: the span an application is asked to aggregate.
struct Row {
    case: &'static str,
    start: i128,
    end: i128,
    step: i128,
    partitions: u32,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a handful of values",
            start: 1,
            end: 11,
            step: 1,
            partitions: 2,
        },
        Row {
            case: "a million values, whose total a float64 still holds exactly",
            start: 1,
            end: 1_000_001,
            step: 1,
            partitions: 4,
        },
        Row {
            case: "two hundred million values, whose total is past float64's exact integers",
            start: 1,
            end: 200_000_002,
            step: 1,
            partitions: 4,
        },
        Row {
            case: "a span that starts far from zero, over one partition",
            start: 9_000_000_000_000_000,
            end: 9_000_000_000_001_000,
            step: 1,
            partitions: 1,
        },
        Row {
            case: "a span with a step between its values",
            start: 10,
            end: 1_000_010,
            step: 7,
            partitions: 3,
        },
        Row {
            case: "a span that holds nothing at all",
            start: 5,
            end: 5,
            step: 1,
            partitions: 2,
        },
    ]
}

impl Row {
    /// How many values this span holds, by the same arithmetic `range` counts them with.
    fn count(&self) -> i128 {
        if self.step <= 0 || self.end <= self.start {
            return 0;
        }
        (self.end - self.start + self.step - 1) / self.step
    }

    /// The exact total of this span: the closed form of an arithmetic progression, computed
    /// here in 128-bit integers so the expectation is this suite's own arithmetic.
    fn total(&self) -> Option<i128> {
        let count = self.count();
        if count == 0 {
            return None;
        }
        let last = self.start + (count - 1) * self.step;
        Some((self.start + last) * count / 2)
    }

    /// The key the application writes its line under.
    fn output(&self, ordinal: usize) -> String {
        format!("out/span-{ordinal}")
    }
}

/// The fields of the line the application wrote.
struct Report {
    total: String,
    kind: String,
    count: String,
    approx: String,
    /// The same double total, asked for by an expression that names no alias of its own: the
    /// `as` inside `cast(... as ...)` is the cast's, not an alias marker.
    unaliased: String,
}

fn read_report(case: &str, line: &str) -> Report {
    let fields: Vec<&str> = line.trim().split('|').collect();
    assert_eq!(
        fields.len(),
        5,
        "case {case:?}: expected five fields in the written line, got {line:?}"
    );
    Report {
        total: fields[0].to_string(),
        kind: fields[1].to_string(),
        count: fields[2].to_string(),
        approx: fields[3].to_string(),
        unaliased: fields[4].to_string(),
    }
}

/// The integers a 64-bit float holds exactly reach 2^53; past that a float cannot tell one
/// integer from its neighbour.
const FLOAT64_EXACT_INTEGERS: i128 = 1i128 << 53;

#[tokio::test]
async fn a_bigint_span_is_aggregated_exactly_however_many_values_it_holds() {
    let router = emulator();
    let rows = rows();

    let (status, answered) = s3(&router, "PUT", &format!("/{BUCKET}"), &[]).await;
    assert!(
        (200..300).contains(&status),
        "CreateBucket must succeed, got {status} {}",
        String::from_utf8_lossy(&answered)
    );
    put_object(&router, "the application", "aggregate.py", APPLICATION.as_bytes()).await;

    // One cluster per row, all launched before any is waited on, each kept alive so its step's
    // own end is what is read rather than the cluster's shutdown.
    let mut launched: Vec<String> = Vec::new();
    for (ordinal, row) in rows.iter().enumerate() {
        let step = json!({
            "Name": row.case,
            "ActionOnFailure": "CONTINUE",
            "HadoopJarStep": {
                "Jar": "command-runner.jar",
                "Args": [
                    "spark-submit",
                    format!("s3://{BUCKET}/aggregate.py"),
                    format!("s3://{BUCKET}/{}", row.output(ordinal)),
                    row.start.to_string(),
                    row.end.to_string(),
                    row.step.to_string(),
                    row.partitions.to_string(),
                ],
            },
        });
        launched.push(launch(&router, row.case, json!([step]), true).await);
    }

    for ((ordinal, row), cluster_id) in rows.iter().enumerate().zip(&launched) {
        let case = row.case;
        let steps = drained(case, &router, cluster_id).await;
        let step = steps
            .first()
            .unwrap_or_else(|| panic!("case {case:?}: expected the submitted step back"));
        assert_eq!(
            step["Status"]["State"].as_str(),
            Some("COMPLETED"),
            "case {case:?}: an application that aggregated its span completes its step, \
             got {step}"
        );

        let key = format!("{}/part-00000", row.output(ordinal));
        let written = object(&router, &key)
            .await
            .unwrap_or_else(|| panic!("case {case:?}: expected the application's own output"));
        let report = read_report(case, &written);

        let expected = row.total();
        let expected_text = match expected {
            Some(total) => total.to_string(),
            // Every aggregate of no rows is null, which is what the application reports.
            None => "None".to_string(),
        };
        assert_eq!(
            report.total, expected_text,
            "case {case:?}: expected the exact total {expected_text}, got {:?}",
            report.total
        );
        assert_eq!(
            report.count,
            row.count().to_string(),
            "case {case:?}: expected {} values in the span, got {:?}",
            row.count(),
            report.count
        );
        assert_eq!(
            report.kind, "bigint",
            "case {case:?}: the total of a bigint column is a bigint, got {:?}",
            report.kind
        );

        assert_eq!(
            report.unaliased, report.approx,
            "case {case:?}: an expression that names no alias answers the same total, got \
             {:?} and {:?}",
            report.unaliased, report.approx
        );

        let Some(total) = expected else {
            continue;
        };
        let approx: f64 = report
            .approx
            .parse()
            .unwrap_or_else(|_| panic!("case {case:?}: expected a double, got {:?}", report.approx));
        assert_eq!(
            approx, total as f64,
            "case {case:?}: the double total is the nearest one to {total}, got {approx}"
        );
        if total > FLOAT64_EXACT_INTEGERS {
            assert_ne!(
                approx as i128, total,
                "case {case:?}: a total past {FLOAT64_EXACT_INTEGERS} cannot be held exactly \
                 by a double, so the two must differ"
            );
        }
    }
}
