// SPDX-License-Identifier: CC0-1.0

//! The METASTORE DDL a step performs, end to end over the wire: what a submitted
//! `ALTER TABLE … PARTITION` step settles in, and what the Glue Data Catalog holds afterwards.
//!
//! THE RULE every case below asserts: a `command-runner.jar` step whose statement's whole
//! effect is a change to the partitions of a table of the cluster's Hive metastore is
//! PERFORMED against that metastore — which, on a cluster whose configuration selects the
//! AWS Glue Data Catalog, is that catalog — so the partition the statement adds is afterwards
//! readable through Glue's own `GetPartition`/`GetPartitions`, the one it drops is gone for
//! every later reader, and the step settles `COMPLETED`; a statement the metastore REFUSES
//! settles `FAILED` carrying that refusal and leaves the catalog exactly as it was.
//!
//! The last case asserts the same rule over the WHOLE command line a client submits Hive work
//! in (`bash -c 'set -eu; hive -S -e "…" > /tmp/out 2>&1; aws s3 cp /tmp/out s3://…'`): the
//! client runs its statements against the metastore and the data under it, and the stages
//! around it — the redirection, the transfer — are performed too, so the object the command
//! line names holds what the statement answered.
//!
//! Every case drives the real HTTP path — `POST /` with an `awsJson1_1` body — through the
//! router's `dispatch`, against one emulator carrying Amazon EMR, the Glue Data Catalog and
//! Amazon S3 over one shared engine (`crate::glue_metastore_steps::Pairing`). Nothing reaches
//! into any service's state struct: the catalog is prepared and read back only through Glue's
//! own operations, its data written and read through Amazon S3's own, and a step only ever
//! through `AddJobFlowSteps`/`DescribeStep`.

use serde_json::{json, Value};

use crate::glue_metastore_steps::{command_step, sql_step, Pairing, GLUE_FACTORY};
use crate::job_flow_lifecycle::at;

/// The Glue database these cases register their tables in.
const DATABASE: &str = "sim_ddl_db";

/// Where the tables of these cases keep their data: the bucket, and the prefix each table's
/// own location sits under — which is also the root Hive lays a partition directory out
/// beneath.
const DATA_BUCKET: &str = "sim-ddl-data";
const DATA_ROOT: &str = "s3://sim-ddl-data";

/// One row of the ADD-PARTITION table: the statement a step submits, the state it must
/// settle in, and what the catalog must hold afterwards.
struct AddCase {
    /// What this row is about.
    case: &'static str,
    /// The table the row registers and the statement addresses.
    table: &'static str,
    /// That table's partition keys, in declaration order.
    keys: &'static [(&'static str, &'static str)],
    /// A partition registered through Glue's OWN `CreatePartition` before the step runs:
    /// the record a statement then meets as already there.
    existing: Option<&'static [&'static str]>,
    /// The SQL client of the cluster the command names.
    client: &'static str,
    /// The statement the step submits.
    statement: String,
    /// The `StepState` the step must settle in.
    state: &'static str,
    /// A fragment the step's `FailureDetails.Message` must carry, for a row that fails.
    failure: Option<&'static str>,
    /// Every partition the catalog must hold afterwards: its values and the location its
    /// data lives under.
    held: Vec<(Vec<&'static str>, String)>,
    /// Every value list the catalog must hold NO partition under afterwards.
    absent: Vec<Vec<&'static str>>,
}

/// The location a table of these cases declares.
fn table_location(table: &str) -> String {
    format!("{DATA_ROOT}/{table}/")
}

/// The directory Hive lays the partition `values` of `table` out under, with one
/// `<key>=<value>` level per partition key in key order.
fn partition_directory(table: &str, keys: &[(&str, &str)], values: &[&str]) -> String {
    let levels: Vec<String> = keys
        .iter()
        .zip(values)
        .map(|((key, _), value)| format!("{key}={value}"))
        .collect();
    format!("{DATA_ROOT}/{table}/{}", levels.join("/"))
}

/// Register `DATABASE` once, and create the bucket the tables' data and the command lines'
/// output live in.
async fn register_database(pairing: &Pairing) {
    let (status, answered) = crate::spark_step_application::s3(
        pairing.router(),
        "PUT",
        &format!("/{DATA_BUCKET}"),
        &[],
    )
    .await;
    assert!(
        (200..300).contains(&status),
        "CreateBucket must succeed, got {status} {}",
        String::from_utf8_lossy(&answered)
    );
    pairing
        .glue(
            "CreateDatabase",
            json!({ "DatabaseInput": { "Name": DATABASE } }),
        )
        .await;
}

/// Put `body` at `key` of that bucket, the way a client lays a table's data out.
async fn put_object(pairing: &Pairing, key: &str, body: &str) {
    let (status, answered) = crate::spark_step_application::s3(
        pairing.router(),
        "PUT",
        &format!("/{DATA_BUCKET}/{key}"),
        body.as_bytes(),
    )
    .await;
    assert!(
        (200..300).contains(&status),
        "PutObject {key} must succeed, got {status} {}",
        String::from_utf8_lossy(&answered)
    );
}

/// The text of the object at `key`, or `None` when the store holds none.
async fn object_text(pairing: &Pairing, key: &str) -> Option<String> {
    let (status, answered) = crate::spark_step_application::s3(
        pairing.router(),
        "GET",
        &format!("/{DATA_BUCKET}/{key}"),
        &[],
    )
    .await;
    (200..300)
        .contains(&status)
        .then(|| String::from_utf8_lossy(&answered).to_string())
}

/// Register one partitioned table of `DATABASE`, over its own location.
async fn register_table(pairing: &Pairing, table: &str, keys: &[(&str, &str)]) {
    let partition_keys: Vec<Value> = keys
        .iter()
        .map(|(name, kind)| json!({ "Name": name, "Type": kind }))
        .collect();
    pairing
        .glue(
            "CreateTable",
            json!({
                "DatabaseName": DATABASE,
                "TableInput": {
                    "Name": table,
                    "TableType": "EXTERNAL_TABLE",
                    "PartitionKeys": partition_keys,
                    "StorageDescriptor": {
                        "Location": table_location(table),
                        "Columns": [{ "Name": "amount", "Type": "bigint" }],
                    },
                },
            }),
        )
        .await;
}

/// Read one partition back through Glue's own `GetPartition`.
async fn get_partition(pairing: &Pairing, table: &str, values: &[&str]) -> (u16, Value) {
    pairing
        .call(
            "glue",
            "AWSGlue",
            "GetPartition",
            json!({
                "DatabaseName": DATABASE,
                "TableName": table,
                "PartitionValues": values,
            }),
        )
        .await
}

/// Every partition of one table, as Glue's own `GetPartitions` lists them.
async fn list_partitions(pairing: &Pairing, table: &str) -> Vec<Value> {
    let listed = pairing
        .glue(
            "GetPartitions",
            json!({ "DatabaseName": DATABASE, "TableName": table }),
        )
        .await;
    listed["Partitions"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

/// Assert, for one case, that the catalog holds exactly the partitions the row names and
/// none of the ones it says stay absent.
async fn assert_catalog(
    pairing: &Pairing,
    case: &str,
    table: &str,
    held: &[(Vec<&str>, String)],
    absent: &[Vec<&str>],
) {
    for (values, location) in held {
        let (status, partition) = get_partition(pairing, table, values).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the catalog must hold the partition {values:?}, got {status} \
             with {partition}"
        );
        let read_values: Vec<String> = at(&partition, &["Partition", "Values"])
            .as_array()
            .map(|entries| {
                entries
                    .iter()
                    .map(|entry| entry.as_str().unwrap_or_default().to_string())
                    .collect()
            })
            .unwrap_or_default();
        assert_eq!(
            read_values, *values,
            "case {case:?}: the partition is addressed by the table's partition-key order, \
             got {read_values:?} in {partition}"
        );
        let read_location = at(&partition, &["Partition", "StorageDescriptor", "Location"])
            .as_str()
            .unwrap_or_default()
            .to_string();
        assert_eq!(
            read_location, *location,
            "case {case:?}: the partition's data lives under {location}, got \
             {read_location:?} in {partition}"
        );
        // The listing of the same table agrees with the single read.
        let listed = list_partitions(pairing, table).await;
        assert!(
            listed
                .iter()
                .any(|entry| entry["Values"] == json!(values)),
            "case {case:?}: GetPartitions must list {values:?}, got {listed:?}"
        );
    }
    for values in absent {
        let (status, body) = get_partition(pairing, table, values).await;
        assert_eq!(
            status, 400,
            "case {case:?}: the catalog must hold no partition {values:?}, got {status} \
             with {body}"
        );
    }
}

/// THE RULE for a statement that ADDS partitions, over the forms such a statement takes:
/// what it adds is in the catalog afterwards, addressed in the table's partition-key order
/// and located where the statement (or Hive's own layout) puts it, and the step is
/// `COMPLETED`; a statement the metastore refuses fails the step and adds nothing.
///
/// Rows differ along: how many partition keys the table has, whether the statement gives a
/// `LOCATION` of its own, the order the spec spells the keys in, the type of the value, how
/// the table name is qualified (`<database>.<table>`, bare after a `USE`), how many
/// partitions one statement names, whether it tolerates one that is already there, which SQL
/// client the command names, whether the command arrives as the step's argument list or as
/// one whole shell command line, and which refusal the metastore answers.
#[tokio::test]
async fn an_add_partition_step_lands_the_partition_in_the_glue_catalog() {
    let pairing = Pairing::new();
    register_database(&pairing).await;
    // One table per row, so the rows are independent and run at once.
    let one_key: &[(&str, &str)] = &[("dt", "string")];
    let two_keys: &[(&str, &str)] = &[("dt", "string"), ("hr", "int")];

    let cases: Vec<AddCase> = vec![
        AddCase {
            case: "a located partition of a table partitioned by one key",
            table: "orders_located",
            keys: one_key,
            existing: None,
            client: "hive",
            statement: format!(
                "ALTER TABLE {DATABASE}.orders_located ADD PARTITION (dt='2026-01-02') \
                 LOCATION '{DATA_ROOT}/orders_located/dt=2026-01-02'"
            ),
            state: "COMPLETED",
            failure: None,
            held: vec![(
                vec!["2026-01-02"],
                format!("{DATA_ROOT}/orders_located/dt=2026-01-02"),
            )],
            absent: vec![vec!["2026-01-03"]],
        },
        AddCase {
            case: "a located partition whose LOCATION carries a trailing separator",
            table: "orders_trailing_slash",
            keys: one_key,
            existing: None,
            client: "hive",
            statement: format!(
                "ALTER TABLE {DATABASE}.orders_trailing_slash ADD PARTITION (dt='2026-01-09') \
                 LOCATION '{DATA_ROOT}/orders_trailing_slash/dt=2026-01-09/'"
            ),
            state: "COMPLETED",
            failure: None,
            // The metastore records the PATH the location denotes, which carries no trailing
            // separator, so the two spellings of one directory are recorded identically.
            held: vec![(
                vec!["2026-01-09"],
                format!("{DATA_ROOT}/orders_trailing_slash/dt=2026-01-09"),
            )],
            absent: vec![],
        },
        AddCase {
            case: "a partition with no LOCATION, which Hive lays out under the table",
            table: "orders_default_location",
            keys: one_key,
            existing: None,
            client: "spark-sql",
            statement: format!(
                "ALTER TABLE {DATABASE}.orders_default_location ADD PARTITION (dt='2026-02-09')"
            ),
            state: "COMPLETED",
            failure: None,
            held: vec![(
                vec!["2026-02-09"],
                partition_directory("orders_default_location", one_key, &["2026-02-09"]),
            )],
            absent: vec![],
        },
        AddCase {
            case: "a two-key spec spelled in the other order",
            table: "orders_two_keys",
            keys: two_keys,
            existing: None,
            client: "hive",
            statement: format!(
                "ALTER TABLE {DATABASE}.orders_two_keys ADD PARTITION (hr=7, dt='2026-03-01')"
            ),
            state: "COMPLETED",
            failure: None,
            held: vec![(
                vec!["2026-03-01", "7"],
                partition_directory("orders_two_keys", two_keys, &["2026-03-01", "7"]),
            )],
            // The values in the order the SPEC spelled them address no partition.
            absent: vec![vec!["7", "2026-03-01"]],
        },
        AddCase {
            case: "two partitions named by one statement",
            table: "orders_two_partitions",
            keys: one_key,
            existing: None,
            client: "hive",
            statement: format!(
                "ALTER TABLE {DATABASE}.orders_two_partitions ADD PARTITION (dt='2026-04-01') \
                 PARTITION (dt='2026-04-02')"
            ),
            state: "COMPLETED",
            failure: None,
            held: vec![
                (
                    vec!["2026-04-01"],
                    partition_directory("orders_two_partitions", one_key, &["2026-04-01"]),
                ),
                (
                    vec!["2026-04-02"],
                    partition_directory("orders_two_partitions", one_key, &["2026-04-02"]),
                ),
            ],
            absent: vec![vec!["2026-04-03"]],
        },
        AddCase {
            case: "a bare table name after a USE of the database",
            table: "orders_bare_name",
            keys: one_key,
            existing: None,
            client: "spark-sql",
            statement: format!(
                "USE {DATABASE}; ALTER TABLE orders_bare_name ADD PARTITION (dt='2026-05-05')"
            ),
            state: "COMPLETED",
            failure: None,
            held: vec![(
                vec!["2026-05-05"],
                partition_directory("orders_bare_name", one_key, &["2026-05-05"]),
            )],
            absent: vec![],
        },
        AddCase {
            case: "IF NOT EXISTS over a partition the catalog already holds",
            table: "orders_tolerated",
            keys: one_key,
            existing: Some(&["2026-06-06"]),
            client: "hive",
            statement: format!(
                "ALTER TABLE {DATABASE}.orders_tolerated ADD IF NOT EXISTS \
                 PARTITION (dt='2026-06-06')"
            ),
            state: "COMPLETED",
            failure: None,
            // The record the catalog's own create wrote is left exactly as it was, under the
            // location THAT write gave it.
            held: vec![(
                vec!["2026-06-06"],
                format!("{DATA_ROOT}/orders_tolerated/created-through-glue"),
            )],
            absent: vec![],
        },
        AddCase {
            case: "a partition the catalog already holds, without IF NOT EXISTS",
            table: "orders_duplicate",
            keys: one_key,
            existing: Some(&["2026-07-07"]),
            client: "hive",
            statement: format!(
                "ALTER TABLE {DATABASE}.orders_duplicate ADD PARTITION (dt='2026-07-07')"
            ),
            state: "FAILED",
            failure: Some("Partition already exists"),
            held: vec![(
                vec!["2026-07-07"],
                format!("{DATA_ROOT}/orders_duplicate/created-through-glue"),
            )],
            absent: vec![],
        },
        AddCase {
            case: "a table the catalog holds nothing under",
            table: "orders_present",
            keys: one_key,
            existing: None,
            client: "hive",
            statement: format!(
                "ALTER TABLE {DATABASE}.orders_absent ADD PARTITION (dt='2026-08-08')"
            ),
            state: "FAILED",
            failure: Some("Table or view not found"),
            held: vec![],
            absent: vec![vec!["2026-08-08"]],
        },
        AddCase {
            case: "a spec naming a column the table is not partitioned by",
            table: "orders_wrong_column",
            keys: one_key,
            existing: None,
            client: "hive",
            statement: format!(
                "ALTER TABLE {DATABASE}.orders_wrong_column ADD PARTITION (amount='3')"
            ),
            state: "FAILED",
            failure: Some("contains non-partition columns"),
            held: vec![],
            absent: vec![vec!["3"]],
        },
        AddCase {
            case: "a spec leaving one of the table's partition columns unset",
            table: "orders_incomplete",
            keys: two_keys,
            existing: None,
            client: "spark-sql",
            statement: format!(
                "ALTER TABLE {DATABASE}.orders_incomplete ADD PARTITION (dt='2026-09-09')"
            ),
            state: "FAILED",
            failure: Some("Partition spec is incorrect"),
            held: vec![],
            absent: vec![vec!["2026-09-09"]],
        },
    ];

    for case in &cases {
        register_table(&pairing, case.table, case.keys).await;
        if let Some(values) = case.existing {
            pairing
                .glue(
                    "CreatePartition",
                    json!({
                        "DatabaseName": DATABASE,
                        "TableName": case.table,
                        "PartitionInput": {
                            "Values": values,
                            "StorageDescriptor": {
                                "Location": format!(
                                    "{DATA_ROOT}/{}/created-through-glue",
                                    case.table
                                ),
                            },
                        },
                    }),
                )
                .await;
        }
    }

    // One cluster, running the rows at once: each row addresses its own table, so the
    // sequencing rule (asserted by `crate::glue_metastore_steps`) is not what is under test
    // here.
    let cluster_id = pairing
        .glue_metastore_cluster("partition ddl", 25)
        .await;
    let step_ids = pairing
        .add_steps(
            &cluster_id,
            cases
                .iter()
                .map(|case| sql_step(case.case, case.client, &case.statement))
                .collect(),
        )
        .await;

    for (case, step_id) in cases.iter().zip(&step_ids) {
        let step = pairing
            .await_terminal_step(case.case, &cluster_id, step_id)
            .await;
        let state = at(&step, &["Status", "State"]).as_str();
        assert_eq!(
            state,
            Some(case.state),
            "case {:?}: expected {}, got {state:?} in {step}",
            case.case,
            case.state
        );
        match case.failure {
            Some(fragment) => {
                let message = at(&step, &["Status", "FailureDetails", "Message"])
                    .as_str()
                    .unwrap_or_default()
                    .to_string();
                assert!(
                    message.contains(fragment),
                    "case {:?}: the failure must carry {fragment:?}, got {message:?} in {step}",
                    case.case
                );
            }
            None => assert!(
                at(&step, &["Status", "FailureDetails"]).is_null(),
                "case {:?}: a step whose statement the metastore applied details no failure, \
                 got {step}",
                case.case
            ),
        }
        assert_catalog(&pairing, case.case, case.table, &case.held, &case.absent).await;
    }
}

/// THE RULE for the whole COMMAND LINE a step submits: a `command-runner.jar` step whose
/// command line invokes the cluster's SQL client runs that client's statements against the
/// cluster's metastore and the data under it, and the stages around it — the redirection of
/// what the client printed, the transfer that carries that file to Amazon S3 — are performed
/// too, so the object the command line names exists once the step has run and holds what the
/// statement answered.
///
/// This is the form a client submits Hive work in: `bash -c 'set -eu; hive -S -e "…" >
/// /tmp/out 2>&1; aws s3 cp /tmp/out s3://…'`. Rows differ along the statement the client is
/// given — the DDL that registers the partition, a count over it, a filter on the table's own
/// column and an aggregate of it — and each uploads its own output object.
#[tokio::test]
async fn a_sql_client_command_line_runs_its_statement_and_carries_its_output_to_s3() {
    let pairing = Pairing::new();
    register_database(&pairing).await;
    let keys: &[(&str, &str)] = &[("dt", "string")];
    register_table(&pairing, "takings", keys).await;
    // The partition's own data: one `amount` per line, under the directory the ADD PARTITION
    // below registers.
    put_object(
        &pairing,
        "takings/dt=2026-01-02/part-00000",
        "5\n10\n20\n",
    )
    .await;

    // One statement at a time, in submission order: the count rows read the partition the
    // first row registered.
    let cluster_id = pairing.glue_metastore_cluster("hive command lines", 1).await;
    let cases: Vec<(&str, String, &str, &str)> = vec![
        (
            "the DDL that registers the partition",
            format!(
                "ALTER TABLE {DATABASE}.takings ADD PARTITION (dt='2026-01-02') \
                 LOCATION '{DATA_ROOT}/takings/dt=2026-01-02/'"
            ),
            "results/ddl.txt",
            "",
        ),
        (
            "a count over the registered partition",
            format!("SELECT count(*) FROM {DATABASE}.takings WHERE dt='2026-01-02'"),
            "results/count.txt",
            "3",
        ),
        (
            "a filter on the table's own column",
            format!("SELECT count(*) FROM {DATABASE}.takings WHERE amount > 5"),
            "results/filtered.txt",
            "2",
        ),
        (
            "an aggregate of the table's own column",
            format!("SELECT sum(amount) FROM {DATABASE}.takings"),
            "results/sum.txt",
            "35",
        ),
    ];

    for (case, statement, key, expected) in &cases {
        let line = format!(
            "set -eu; hive -S -e \"{statement};\" > /tmp/hive_out.txt 2>&1; \
             aws s3 cp /tmp/hive_out.txt {DATA_ROOT}/{key} --region us-east-1"
        );
        let step_ids = pairing
            .add_steps(
                &cluster_id,
                vec![command_step(case, json!(["bash", "-c", line]))],
            )
            .await;
        let step = pairing
            .await_terminal_step(case, &cluster_id, &step_ids[0])
            .await;
        assert_eq!(
            at(&step, &["Status", "State"]).as_str(),
            Some("COMPLETED"),
            "case {case:?}: the whole command line ran, so the step completed, got {step}"
        );
        let uploaded = object_text(&pairing, key).await.unwrap_or_else(|| {
            panic!("case {case:?}: the command line's transfer must have written {key}")
        });
        assert_eq!(
            uploaded.trim(),
            *expected,
            "case {case:?}: the object must hold what the statement answered, got \
             {uploaded:?}"
        );
    }

    // The partition the first row's statement registered is in the catalog, which is what the
    // counts afterwards read.
    let (status, partition) = get_partition(&pairing, "takings", &["2026-01-02"]).await;
    assert_eq!(
        status, 200,
        "the DDL the command line ran registered the partition, got {status} with {partition}"
    );
}

/// THE RULE for a statement that DROPS a partition: the record it removes is gone from the
/// catalog and the step is `COMPLETED`; a partition the catalog does not hold is the
/// compiler's own refusal unless the statement tolerates it, and nothing else of the table
/// is touched.
///
/// Rows differ along: whether the partition is there, whether the statement tolerates its
/// absence, how many keys address it, and which SQL client the command names.
#[tokio::test]
async fn a_drop_partition_step_removes_the_partition_from_the_glue_catalog() {
    let pairing = Pairing::new();
    register_database(&pairing).await;
    let one_key: &[(&str, &str)] = &[("dt", "string")];
    let two_keys: &[(&str, &str)] = &[("dt", "string"), ("hr", "int")];
    register_table(&pairing, "drops_one_key", one_key).await;
    register_table(&pairing, "drops_two_keys", two_keys).await;

    let cluster_id = pairing.glue_metastore_cluster("partition drops", 1).await;

    // The partitions are ADDED by steps of this same cluster, so what one statement wrote is
    // what the next one drops: the two statements meet in the one catalog.
    let setup = vec![
        sql_step(
            "add the one-key partitions",
            "hive",
            &format!(
                "ALTER TABLE {DATABASE}.drops_one_key ADD PARTITION (dt='2026-01-01') \
                 PARTITION (dt='2026-01-02')"
            ),
        ),
        sql_step(
            "add the two-key partition",
            "spark-sql",
            &format!("ALTER TABLE {DATABASE}.drops_two_keys ADD PARTITION (dt='2026-01-01', hr=3)"),
        ),
    ];
    for (index, step_id) in pairing
        .add_steps(&cluster_id, setup)
        .await
        .iter()
        .enumerate()
    {
        let case = format!("setup step {index}");
        let step = pairing
            .await_terminal_step(&case, &cluster_id, step_id)
            .await;
        assert_eq!(
            at(&step, &["Status", "State"]).as_str(),
            Some("COMPLETED"),
            "case {case:?}: the partitions must be added before they are dropped, got {step}"
        );
    }

    /// One row of the DROP table: what it is about, the table it addresses, the statement, the
    /// `StepState` it must settle in, a fragment its failure must carry, the partitions the
    /// catalog must still hold afterwards and the ones it must hold none of.
    type DropCase = (
        &'static str,
        &'static str,
        String,
        &'static str,
        Option<&'static str>,
        Vec<Vec<&'static str>>,
        Vec<Vec<&'static str>>,
    );

    let cases: Vec<DropCase> = vec![
        (
            "a partition the catalog holds",
            "drops_one_key",
            format!("ALTER TABLE {DATABASE}.drops_one_key DROP PARTITION (dt='2026-01-01')"),
            "COMPLETED",
            None,
            // The table's other partition is untouched.
            vec![vec!["2026-01-02"]],
            vec![vec!["2026-01-01"]],
        ),
        (
            "a partition the catalog does not hold",
            "drops_one_key",
            format!("ALTER TABLE {DATABASE}.drops_one_key DROP PARTITION (dt='2026-12-31')"),
            "FAILED",
            Some("Partition not found"),
            vec![vec!["2026-01-02"]],
            vec![vec!["2026-12-31"]],
        ),
        (
            "IF EXISTS over a partition the catalog does not hold",
            "drops_one_key",
            format!(
                "ALTER TABLE {DATABASE}.drops_one_key DROP IF EXISTS PARTITION (dt='2026-12-31')"
            ),
            "COMPLETED",
            None,
            vec![vec!["2026-01-02"]],
            vec![vec!["2026-12-31"]],
        ),
        (
            "a two-key partition, its spec spelled in the other order",
            "drops_two_keys",
            format!(
                "ALTER TABLE {DATABASE}.drops_two_keys DROP PARTITION (hr=3, dt='2026-01-01')"
            ),
            "COMPLETED",
            None,
            vec![],
            vec![vec!["2026-01-01", "3"]],
        ),
    ];

    for (case, table, statement, expected, failure, held, absent) in &cases {
        let step_ids = pairing
            .add_steps(&cluster_id, vec![sql_step(case, "hive", statement)])
            .await;
        let step = pairing
            .await_terminal_step(case, &cluster_id, &step_ids[0])
            .await;
        assert_eq!(
            at(&step, &["Status", "State"]).as_str(),
            Some(*expected),
            "case {case:?}: expected {expected}, got {step}"
        );
        if let Some(fragment) = failure {
            let message = at(&step, &["Status", "FailureDetails", "Message"])
                .as_str()
                .unwrap_or_default()
                .to_string();
            assert!(
                message.contains(fragment),
                "case {case:?}: the failure must carry {fragment:?}, got {message:?} in {step}"
            );
        }
        for values in held {
            let (status, partition) = get_partition(&pairing, table, values).await;
            assert_eq!(
                status, 200,
                "case {case:?}: the catalog must still hold {values:?}, got {status} with \
                 {partition}"
            );
        }
        for values in absent {
            let (status, body) = get_partition(&pairing, table, values).await;
            assert_eq!(
                status, 400,
                "case {case:?}: the catalog must hold no partition {values:?}, got {status} \
                 with {body}"
            );
        }
    }
}

/// A statement NOTHING PERFORMED, after an earlier statement of the same invocation already
/// changed the metastore: the change stands and the step settles FAILED saying the rest of its
/// work was not run — never a command reported as one nothing ran over a catalog that was
/// really written.
#[tokio::test]
async fn a_statement_nothing_performed_after_an_applied_change_still_fails_the_step() {
    let pairing = Pairing::new();
    register_database(&pairing).await;
    let one_key: &[(&str, &str)] = &[("dt", "string")];
    register_table(&pairing, "half_applied", one_key).await;
    // A table whose data lives in a bucket this emulator holds nothing of: a query over it is
    // performed by nothing at all.
    pairing
        .glue(
            "CreateTable",
            json!({
                "DatabaseName": DATABASE,
                "TableInput": {
                    "Name": "unreadable",
                    "StorageDescriptor": {
                        "Location": "s3://sim-ddl-no-such-bucket/unreadable/",
                        "Columns": [{ "Name": "amount", "Type": "bigint" }],
                    },
                },
            }),
        )
        .await;

    let cluster_id = pairing.glue_metastore_cluster("partly applied", 1).await;
    let line = format!(
        "hive -S -e \"ALTER TABLE {DATABASE}.half_applied ADD PARTITION (dt='2026-11-11'); \
         SELECT count(*) FROM {DATABASE}.unreadable;\" > /tmp/hive_out.txt 2>&1"
    );
    let step_ids = pairing
        .add_steps(
            &cluster_id,
            vec![command_step("partly applied", json!(["bash", "-c", line]))],
        )
        .await;
    let step = pairing
        .await_terminal_step("partly applied", &cluster_id, &step_ids[0])
        .await;
    assert_eq!(
        at(&step, &["Status", "State"]).as_str(),
        Some("FAILED"),
        "a statement nothing performed fails the step, got {step}"
    );
    let message = at(&step, &["Status", "FailureDetails", "Message"])
        .as_str()
        .unwrap_or_default()
        .to_string();
    assert!(
        message.contains("No application was started"),
        "the failure must say the work was not run, got {message:?} in {step}"
    );
    // The change the first statement applied STANDS: it happened, so the catalog holds it.
    let (status, partition) = get_partition(&pairing, "half_applied", &["2026-11-11"]).await;
    assert_eq!(
        status, 200,
        "the partition the applied statement added is still in the catalog, got {status} \
         with {partition}"
    );
}

/// THE RULE's other half: a partition statement whose metastore this emulator does not hold
///
/// is work nothing ran, so the step settles in the modelled failure state saying so and the
/// catalog is untouched — never a success for a change nothing applied.
///
/// Rows differ along WHY the statement is not performed: the cluster selects no Glue
/// metastore client at all, it selects another one, and the step runs a jar of its own rather
/// than a command.
#[tokio::test]
async fn a_partition_statement_this_emulator_cannot_perform_reports_no_success() {
    let pairing = Pairing::new();
    register_database(&pairing).await;
    let one_key: &[(&str, &str)] = &[("dt", "string")];
    register_table(&pairing, "unperformed", one_key).await;
    let statement = format!(
        "ALTER TABLE {DATABASE}.unperformed ADD PARTITION (dt='2026-10-10')"
    );

    let no_metastore = pairing
        .cluster("no metastore configuration", 25, json!([]))
        .await;
    let other_metastore = pairing
        .cluster(
            "another metastore client",
            25,
            json!([{
                "Classification": "hive-site",
                "Properties": {
                    "hive.metastore.client.factory.class":
                        "org.apache.hadoop.hive.metastore.HiveMetaStoreClient",
                },
            }]),
        )
        .await;
    let on_glue = pairing
        .cluster(
            "the glue metastore",
            25,
            json!([{
                "Classification": "hive-site",
                "Properties": { "hive.metastore.client.factory.class": GLUE_FACTORY },
            }]),
        )
        .await;

    let cases: Vec<(&str, &str, Value)> = vec![
        (
            "a cluster selecting no Glue metastore client",
            &no_metastore,
            sql_step("add on local metastore", "hive", &statement),
        ),
        (
            "a cluster selecting another metastore client",
            &other_metastore,
            sql_step("add on other metastore", "hive", &statement),
        ),
        (
            "a jar of the step's own rather than a command",
            &on_glue,
            json!({
                "Name": "own jar",
                "HadoopJarStep": {
                    "Jar": "s3://jars/ddl.jar",
                    "MainClass": "com.example.Ddl",
                    "Args": [&statement],
                },
            }),
        ),
        (
            "a command carrying no statement at all",
            &on_glue,
            command_step("state pusher", json!(["state-pusher-script"])),
        ),
    ];

    for (case, cluster_id, submitted) in &cases {
        let step_ids = pairing.add_steps(cluster_id, vec![submitted.clone()]).await;
        let step = pairing
            .await_terminal_step(case, cluster_id, &step_ids[0])
            .await;
        assert_eq!(
            at(&step, &["Status", "State"]).as_str(),
            Some("FAILED"),
            "case {case:?}: a change nothing applied is no success, got {step}"
        );
        let message = at(&step, &["Status", "FailureDetails", "Message"])
            .as_str()
            .unwrap_or_default()
            .to_string();
        assert!(
            message.contains("No application was started"),
            "case {case:?}: the failure must say no application ran, got {message:?} in {step}"
        );
        let (status, body) = get_partition(&pairing, "unperformed", &["2026-10-10"]).await;
        assert_eq!(
            status, 400,
            "case {case:?}: nothing was performed, so the catalog holds no partition, got \
             {status} with {body}"
        );
    }
}
