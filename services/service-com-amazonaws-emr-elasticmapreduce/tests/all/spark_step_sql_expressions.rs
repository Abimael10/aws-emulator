// SPDX-License-Identifier: CC0-1.0

//! A submitted Spark application's SQL EXPRESSIONS are read by the SQL expression grammar,
//! and the cluster's own `spark-defaults` configuration is what its engine runs with — end to
//! end over the wire, with Amazon S3 mounted beside Amazon EMR on one engine (the harness
//! `tests/all/spark_step_application.rs` owns).
//!
//! THE RULE, in two halves:
//!
//! * `DataFrame.selectExpr` takes SQL expressions, so every expression of that grammar — a
//!   literal, a reference to a column, a call of a function the standard catalog holds, a
//!   cast, an operator over two expressions, a conditional, a test — is read as the column
//!   expression it names and COMPUTED over the dataset, and only a name no catalog holds or a
//!   form outside the grammar is the analysis failure Spark raises.
//! * a property the cluster's `spark-defaults` configuration classification sets is the
//!   property the application's own configuration reads back, and
//!   `spark.sql.shuffle.partitions` — "the number of partitions to use when shuffling data
//!   for joins or aggregations" — is therefore what a grouped aggregate answers over.
//!
//! The rows of the first half span what that grammar generalises over: which kind of
//! expression is written (a call, a cast, arithmetic, comparison, logic, a conditional, a
//! null test, a membership test, a pattern match, an operator spelled as a function), how
//! deeply calls nest, whether an expression is an aggregate or a projection, and the three
//! ways one can fail to resolve. A further table spans the BOUNDS the reading is held to:
//! text that is malformed, truncated, longer than the reading takes, holding more tokens than
//! it takes, nested deeper than it goes, and a pattern of many wildcards — each of which must
//! answer the reading's own analysis failure rather than cost this process the expression. The
//! rows of the second half span the value the configuration sets, including a cluster that
//! sets none.
//!
//! Every expected value is this suite's OWN arithmetic over the three rows the application
//! builds, so nothing below is read back from what the emulator answered.

use serde_json::{json, Value};

use crate::spark_step_application::{
    drained, emr_ok, emulator, object, put_object, s3, BUCKET,
};

/// The roles a launch names, as the harness names them.
const INSTANCE_PROFILE: &str = "EMR_EC2_DefaultRole";
const SERVICE_ROLE: &str = "EMR_DefaultRole";

/// One expression the application is asked for, and what it must answer for the three rows
/// of that dataset — the values of the projected column, run together with `|`, or the
/// failure class and sentence a form that cannot be resolved raises.
struct Expression {
    case: &'static str,
    expression: &'static str,
    answers: &'static str,
}

/// One BOUND the reading of an expression is held to: the expression is built in the
/// application itself, because an expression past a bound is longer than this file would
/// carry, and what the reading must answer is the sentence that bound raises.
struct Bound {
    case: &'static str,
    /// Python source that evaluates to the expression's own text.
    builds: &'static str,
    /// What the failure the reading raises must carry.
    answers: &'static str,
}

/// The bounds, each with the malformed, oversized or over-nested expression that reaches it.
///
/// Every row is an expression a caller could write, and none of them may cost this process
/// more than the bound allows or reach the interpreter's own recursion limit: what each must
/// answer is the reading's own analysis failure.
fn bounds() -> Vec<Bound> {
    vec![
        Bound {
            case: "an expression that ends inside a quoted value",
            builds: "\"concat('a\"",
            answers: "ends inside a quoted value",
        },
        Bound {
            case: "a call that is never closed",
            builds: "\"concat('a', id\"",
            answers: "expected )",
        },
        Bound {
            case: "a character that is no part of the grammar",
            builds: "\"id ^ 2\"",
            answers: "is not part of it",
        },
        Bound {
            case: "an expression longer than the reading takes",
            builds: "\"id + \" * 1200 + \"id\"",
            answers: "too long to read",
        },
        Bound {
            case: "an expression inside that length holding more tokens than it takes",
            builds: "\"id+\" * 600 + \"id\"",
            answers: "holds too many tokens",
        },
        Bound {
            case: "calls nested deeper than the reading goes",
            builds: "\"upper(\" * 40 + \"region\" + \")\" * 40",
            answers: "nests too deeply",
        },
        Bound {
            case: "brackets nested deeper than the reading goes",
            builds: "\"(\" * 60 + \"id\" + \")\" * 60",
            answers: "nests too deeply",
        },
        Bound {
            case: "a chain of negations, which recurses on the text as nesting does",
            builds: "\"not \" * 200 + \"region is null\"",
            answers: "nests too deeply",
        },
        Bound {
            case: "a chain of prefix minuses, for the same reason",
            builds: "\"- \" * 200 + \"id\"",
            answers: "nests too deeply",
        },
        Bound {
            case: "a pattern of many wildcards over text that does not match it",
            builds: "\"region like '\" + \"%x\" * 200 + \"'\"",
            answers: "False|False|None",
        },
    ]
}

/// The dataset every expression is computed over: `(id, region, amount)`.
///
/// `region` is absent in the last row, so every rule's answer for a null operand is observed
/// beside its answer for a value.
const ROWS: &str = "[(1, \"eu\", 10.0), (2, \"us\", 20.5), (3, None, 30.0)]";

/// The expressions, each with the answer this suite computes for it.
///
/// The functions named are members of the standard catalog `pyspark.sql.functions` publishes
/// in its own `__all__` (`concat`, `upper`, `length`, `coalesce`, `sum`, `count`); `coalesce`
/// and `length` are members this suite picked that the report behind it never cited.
fn expressions() -> Vec<Expression> {
    vec![
        Expression {
            case: "a function of a literal and a cast column",
            expression: "concat('pp-', cast(id as string))",
            answers: "pp-1|pp-2|pp-3",
        },
        Expression {
            case: "arithmetic over a column",
            expression: "id * 10 + 1",
            answers: "11|21|31",
        },
        Expression {
            case: "the remainder operator, which a partition key is built with",
            expression: "id % 2",
            answers: "1|0|1",
        },
        Expression {
            case: "a comparison",
            expression: "amount > 15",
            answers: "False|True|True",
        },
        Expression {
            case: "logic over two comparisons",
            expression: "amount > 15 and id < 3",
            answers: "False|True|False",
        },
        Expression {
            case: "a conditional",
            expression: "case when amount > 15 then 'big' else 'small' end",
            answers: "small|big|big",
        },
        Expression {
            case: "a null test",
            expression: "region is null",
            answers: "False|False|True",
        },
        Expression {
            case: "a membership test",
            expression: "id in (1, 3)",
            answers: "True|False|True",
        },
        Expression {
            case: "a pattern match",
            expression: "region like 'e%'",
            answers: "True|False|None",
        },
        Expression {
            case: "a text function over a column",
            expression: "upper(region)",
            answers: "EU|US|None",
        },
        Expression {
            case: "the concatenation operator, which is that function spelled as one",
            expression: "region || '-x'",
            answers: "eu-x|us-x|None",
        },
        Expression {
            case: "a catalog function this suite picked rather than the report behind it",
            expression: "coalesce(region, 'none')",
            answers: "eu|us|none",
        },
        Expression {
            case: "one call nested inside another",
            expression: "length(concat(region, region))",
            answers: "4|4|None",
        },
        Expression {
            case: "an aggregate over a column, which answers one row for the dataset",
            expression: "sum(amount)",
            answers: "60.5",
        },
        Expression {
            case: "the aggregate of every row",
            expression: "count(*)",
            answers: "3",
        },
        Expression {
            case: "a function no catalog holds",
            expression: "no_such_function(id)",
            answers: "AnalysisException: Undefined function: no_such_function",
        },
        Expression {
            case: "a cast to a type no catalog holds",
            expression: "cast(id as geography)",
            answers: "AnalysisException: cannot resolve the data type geography",
        },
        Expression {
            case: "a column the dataset has none of",
            expression: "missing + 1",
            answers: "AnalysisException: cannot resolve the column missing",
        },
    ]
}

/// The application the expression rows are computed by: it asks for each expression in turn
/// and writes one line per row — the case, and what the projection answered or the failure it
/// raised. The expressions are the ones this suite named, rendered into the program, so there
/// is ONE statement of them.
fn expression_application(held: &[Expression], bounds: &[Bound]) -> String {
    let mut cases = String::new();
    for expression in held {
        cases.push_str(&format!(
            "    ({}, {}),\n",
            Value::String(expression.case.to_string()),
            Value::String(expression.expression.to_string()),
        ));
    }
    // A bound's expression is BUILT in the program: one past a length or a nesting bound is
    // longer than this suite would carry as a literal.
    for bound in bounds {
        cases.push_str(&format!(
            "    ({}, {}),\n",
            Value::String(bound.case.to_string()),
            bound.builds,
        ));
    }
    format!(
        r#"
import sys
from pyspark.sql import SparkSession

CASES = [
{cases}]

spark = SparkSession.builder.appName("expressions").getOrCreate()
frame = spark.createDataFrame({ROWS}, ["id", "region", "amount"])
lines = []
for case, expression in CASES:
    try:
        answered = "|".join(
            "%s" % (row[0],) for row in frame.selectExpr(expression).collect()
        )
    except Exception as raised:
        answered = "%s: %s" % (type(raised).__name__, raised)
    lines.append("%s\t%s" % (case, answered))
spark.sparkContext.parallelize(["\n".join(lines)], 1).saveAsTextFile(sys.argv[1])
spark.stop()
"#
    )
}

/// The application the shuffle rows are computed by: it reports the property its own
/// configuration carries, how many partitions the grouped aggregate answers over, and the
/// aggregate itself.
const SHUFFLE_APPLICATION: &str = r#"
import sys
from pyspark.sql import SparkSession

spark = SparkSession.builder.appName("shuffle").getOrCreate()
configured = spark.conf.get("spark.sql.shuffle.partitions")
frame = spark.range(0, 12).selectExpr("id % 3 as k", "id as v")
rolled = frame.groupBy("k").sum("v")
report = "%s;%s;%s" % (
    configured,
    rolled.partitions(),
    ",".join(
        "%s=%s" % (row["k"], row["sum(v)"]) for row in rolled.orderBy("k").collect()
    ),
)
spark.sparkContext.parallelize([report], 1).saveAsTextFile(sys.argv[1])
spark.stop()
"#;

/// One shuffle row: what the cluster's configuration sets the property to, and what the
/// application must then read back and group over.
struct Shuffle {
    case: &'static str,
    /// The value the `spark-defaults` classification sets, or `None` for a launch that
    /// carries no configuration at all.
    configured: Option<&'static str>,
    /// How many partitions the grouped aggregate answers over.
    partitions: u32,
}

/// The sums of `0..12` by the remainder of each value modulo three, computed here so the
/// expectation is this suite's own arithmetic.
fn grouped_sums() -> String {
    let mut held: Vec<String> = Vec::new();
    for key in 0..3u64 {
        let total: u64 = (0..12u64).filter(|value| value % 3 == key).sum();
        held.push(format!("{key}={total}"));
    }
    held.join(",")
}

fn shuffles() -> Vec<Shuffle> {
    vec![
        Shuffle {
            case: "a configuration that sets the shuffle width",
            configured: Some("7"),
            partitions: 7,
        },
        Shuffle {
            case: "another width, so the count is the configuration's own",
            configured: Some("3"),
            partitions: 3,
        },
        Shuffle {
            case: "a cluster whose configuration sets no width",
            configured: None,
            partitions: 1,
        },
    ]
}

/// A kept-alive cluster carrying `configurations`, with one step submitting `application`.
async fn launch_with(
    router: &smithy_server_runtime::MultiServiceRouter,
    name: &str,
    configurations: Value,
    key: &str,
    output: &str,
) -> String {
    let launched = emr_ok(
        router,
        "RunJobFlow",
        json!({
            "Name": name,
            "ReleaseLabel": "emr-6.15.0",
            "JobFlowRole": INSTANCE_PROFILE,
            "ServiceRole": SERVICE_ROLE,
            "Applications": [{ "Name": "Spark" }],
            "Configurations": configurations,
            "Steps": [{
                "Name": name,
                "ActionOnFailure": "CONTINUE",
                "HadoopJarStep": {
                    "Jar": "command-runner.jar",
                    "Args": [
                        "spark-submit",
                        format!("s3://{BUCKET}/{key}"),
                        format!("s3://{BUCKET}/{output}"),
                    ],
                },
            }],
            "Instances": {
                "InstanceCount": 1,
                "MasterInstanceType": "m5.xlarge",
                "KeepJobFlowAliveWhenNoSteps": true,
            },
        }),
    )
    .await;
    launched["JobFlowId"]
        .as_str()
        .expect("RunJobFlow answers a JobFlowId")
        .to_string()
}

/// The one line the step's application wrote, which must be there: a step reported COMPLETED
/// for work that wrote nothing would be no evidence at all.
async fn written(
    case: &str,
    router: &smithy_server_runtime::MultiServiceRouter,
    cluster_id: &str,
    output: &str,
) -> String {
    let steps = drained(case, router, cluster_id).await;
    let step = steps
        .first()
        .unwrap_or_else(|| panic!("case {case:?}: expected the submitted step back"));
    assert_eq!(
        step["Status"]["State"].as_str(),
        Some("COMPLETED"),
        "case {case:?}: an application that ran to its end completes its step, got {step}"
    );
    object(router, &format!("{output}/part-00000"))
        .await
        .unwrap_or_else(|| panic!("case {case:?}: expected the application's own output"))
}

/// Every expression of the grammar is computed, and only a name no catalog holds or a form
/// outside it raises the analysis failure.
#[tokio::test]
async fn a_submitted_application_computes_every_sql_expression_of_the_grammar() {
    let router = emulator();
    let (status, answered) = s3(&router, "PUT", &format!("/{BUCKET}"), &[]).await;
    assert!(
        (200..300).contains(&status),
        "CreateBucket must succeed, got {status} {}",
        String::from_utf8_lossy(&answered)
    );
    let held = expressions();
    let bounded = bounds();
    put_object(
        &router,
        "the application",
        "jobs/expressions.py",
        expression_application(&held, &bounded).as_bytes(),
    )
    .await;

    let output = "out/expressions";
    let cluster_id = launch_with(
        &router,
        "sql expressions",
        json!([]),
        "jobs/expressions.py",
        output,
    )
    .await;
    let report = written("sql expressions", &router, &cluster_id, output).await;

    let answered: Vec<&str> = report.trim_end_matches('\n').split('\n').collect();
    assert_eq!(
        answered.len(),
        held.len() + bounded.len(),
        "expected one line per expression, got {report:?}"
    );
    for (expression, line) in held.iter().zip(&answered) {
        let case = expression.case;
        let (named, value) = line
            .split_once('\t')
            .unwrap_or_else(|| panic!("case {case:?}: expected a tab-separated line, got {line:?}"));
        assert_eq!(
            named, case,
            "case {case:?}: the lines must answer in the order asked, got {named:?}"
        );
        assert_eq!(
            value, expression.answers,
            "case {case:?}: expression {:?} must answer {:?}, got {value:?}",
            expression.expression, expression.answers
        );
    }
    // The bounds: the reading answers its own analysis failure rather than spending this
    // process on the expression or reaching the interpreter's own recursion limit. The
    // failure names the expression, which is too long to compare whole, so what is asserted
    // is the sentence the bound raises.
    for (bound, line) in bounded.iter().zip(answered.iter().skip(held.len())) {
        let case = bound.case;
        let (named, value) = line
            .split_once('\t')
            .unwrap_or_else(|| panic!("case {case:?}: expected a tab-separated line, got {line:?}"));
        assert_eq!(
            named, case,
            "case {case:?}: the lines must answer in the order asked, got {named:?}"
        );
        assert!(
            value.contains(bound.answers),
            "case {case:?}: expected {:?} in the answer, got {value:?}",
            bound.answers
        );
    }
}

/// The property the cluster's own configuration sets is what the application reads back, and
/// what its grouped aggregate is then spread over.
#[tokio::test]
async fn a_spark_defaults_property_reaches_the_application_and_governs_its_shuffle() {
    let router = emulator();
    let (status, _) = s3(&router, "PUT", &format!("/{BUCKET}"), &[]).await;
    assert!((200..300).contains(&status), "CreateBucket must succeed");
    put_object(
        &router,
        "the application",
        "jobs/shuffle.py",
        SHUFFLE_APPLICATION.as_bytes(),
    )
    .await;

    let rows = shuffles();
    let sums = grouped_sums();
    // Every cluster is launched before any is waited on, so the rows run beside each other.
    let mut launched: Vec<(usize, String)> = Vec::new();
    for (ordinal, row) in rows.iter().enumerate() {
        let configurations = match row.configured {
            Some(value) => json!([{
                "Classification": "spark-defaults",
                "Properties": { "spark.sql.shuffle.partitions": value },
            }]),
            None => json!([]),
        };
        launched.push((
            ordinal,
            launch_with(
                &router,
                row.case,
                configurations,
                "jobs/shuffle.py",
                &format!("out/shuffle-{ordinal}"),
            )
            .await,
        ));
    }

    for ((ordinal, cluster_id), row) in launched.iter().zip(&rows) {
        let case = row.case;
        let report = written(case, &router, cluster_id, &format!("out/shuffle-{ordinal}")).await;
        let expected = format!(
            "{};{};{sums}",
            row.configured.unwrap_or("None"),
            row.partitions
        );
        assert_eq!(
            report.trim(),
            expected,
            "case {case:?}: expected {expected:?}, got {:?}",
            report.trim()
        );
    }
}
