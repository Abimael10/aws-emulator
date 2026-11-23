Issue-Source: trace-sweep-divergence
Issue-Trace: athena-cats-traj-table-before-database-and-ddl-retries
Issue-Step: task#17
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: athena-cats-traj-table-before-database-and-ddl-retries athena-kfi-traj-athena-reads-the-iceberg-table
Issue-Shape-Id: com.amazonaws.athena#AmazonAthena
Issue-Key: athena|GetQueryExecution|2xx->2xx|$.QueryExecution.Status.StateChangeReason
Issue-Key: athena|GetQueryExecution|2xx->2xx|$.QueryExecution.Statistics.DataScannedInBytes
Issue-Related: 20261115T222915Z-athena-query-execution-and-result-reads-diverge-columninfo.md
Issue-Commit: dbc160e5ba09bc1337b7dfc453c49db2296d6587

# Athena GetQueryExecution misreports terminal DDL failure text and query scan statistics (2 causes)

> _Note: this issue was identified against the code base at commit `dbc160e5ba09bc1337b7dfc453c49db2296d6587`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetQueryExecution

## Description
Two net-new root causes in the Athena GetQueryExecution read path. In both the query itself reaches a terminal state and the call answers 200, but what the response REPORTS about that execution diverges from AWS: the failure text of a DDL collision, and the scan statistics of a successful read. Each cause must be closed separately; there are 2 of them.

=== Cause 1 of 2: a CREATE TABLE that collides reports a Java exception wrapper instead of Athena's bare Hive DDL failure sentence ===
AWS: recorded behavior only — Smithy (services/service-com-amazonaws-athena-amazonathena/smithy-model/com-amazonaws-athena-amazonathena.json) models QueryExecutionStatus.StateChangeReason as an unconstrained String ("Further detail about the status of the query"), so the model states no text rule. For a CREATE TABLE whose table already exists, the recording shows GetQueryExecution answering 200 with State FAILED and StateChangeReason (in BOTH QueryExecution.Status.StateChangeReason and the QueryExecutionDetail.Status.StateChangeReason projection) = "FAILED: Execution Error, return code 1 from org.apache.hadoop.hive.ql.exec.DDLTask. AlreadyExistsException(message:Table <tableName> already exist)". Fixed text: the prefix "FAILED: Execution Error, return code 1 from org.apache.hadoop.hive.ql.exec.DDLTask. " followed by the Hive metastore exception rendering "AlreadyExistsException(message:Table " ... " already exist)", with no trailing period. The ONLY variable part is the table name taken from the submitted statement (the recorded run used the table `items`). Note the two distinguishing details of AWS's wording: the AlreadyExistsException(message:...) wrapper IS present for a table collision, and the verb is the un-inflected "already exist", not "already exists".
Emulator: for the same statement it answers "FAILED: Execution Error, return code 1 from org.apache.hadoop.hive.ql.exec.DDLTask. Table <tableName> already exists" in both fields — the Hive AlreadyExistsException(message:...) wrapper is dropped and the verb is inflected ("already exists"). Status is 200 in both, so only the reason text diverges.
Why: clients, runbooks and IaC wrappers classify Athena DDL outcomes by matching the DDLTask reason text; an already-exists collision that does not carry the AlreadyExistsException wrapper is not recognised as a benign create-if-absent collision, so idempotent table-creation logic and retry/alert rules that pass against AWS take the generic-failure branch against the emulator.
Fix: the StartQueryExecution DDL execution path's failure-text construction — the component that fills QueryExecutionStatus.StateChangeReason (and the QueryExecutionDetail.Status projection of it) when a DDL statement fails on an existing object. It must emit Athena's recorded Hive sentence for a TABLE collision rather than re-wording the underlying error.
Related but NOT covering: the open issue 20261115T222915Z-athena-query-execution-and-result-reads-diverge-columninfo.md (its Cause 2) owns the CREATE DATABASE already-exists wording, whose AWS text is the opposite shape (bare "Database <db> already exists", no exception wrapper). The replay reports this CREATE TABLE cluster as unowned, and closing the database cause does not fix it — the table wording needs the wrapper the database wording must not have.
Traces: athena-cats-traj-table-before-database-and-ddl-retries#task#17.

=== Cause 2 of 2: an Iceberg-table read reports non-zero scan statistics where Athena accounts the query as a zero-scan read ===
AWS: Smithy QueryExecutionStatistics.DataScannedInBytes is a Long, "The number of bytes in the data that was queried". For the recorded query over the Iceberg table, GetQueryExecution answers 200 with QueryExecution.Statistics.DataScannedInBytes = 0 and the detail projection QueryExecutionDetail.Stats.ProcessedBytes = 0, ProcessedRows = 0, ProcessedInputRows = 0. The requirement is the general rule: these counters report what the plan actually had to read, and for this plan that is nothing, so all four are 0 rather than a count of whatever rows the engine happened to materialise. Hypothesis: Athena answers the aggregate from the Iceberg table's manifest/snapshot metadata and therefore scans no data rows.
Emulator: DataScannedInBytes = 746, ProcessedBytes = 746, ProcessedRows = 1, ProcessedInputRows = 1 — it bills bytes and rows for a query AWS accounts as a zero-scan read. (The specific 746/1 magnitudes are this run's values; the defect is the non-zero-for-a-zero-scan-plan reporting, not the numbers.)
Why: a client that uses DataScannedInBytes for cost attribution, that sets a WorkGroup BytesScannedCutoffPerQuery budget, or that asserts an Iceberg aggregate was served from metadata, sees fabricated work — cost and efficiency assertions that hold on AWS fail against the emulator, and a cutoff can be tripped by a query that scans nothing.
Fix: the query-execution statistics accounting in the StartQueryExecution/GetQueryExecution state — the component that populates QueryExecutionStatistics and its QueryExecutionDetail.Stats projection must derive the counters from what the plan scanned, not from the rows the emulator materialised.
Related but NOT covering: the same open issue 20261115T222915Z-athena-query-execution-and-result-reads-diverge-columninfo.md describes the Stats projection for this fixture step, but its recorded keys do not cover $.QueryExecution.Statistics.DataScannedInBytes and the replay reports this cluster as unowned, so it is filed here.
Traces: athena-kfi-traj-athena-reads-the-iceberg-table#task#10.

## Evidence
Replay of trace set simparity-wire-aws-v1 against the emulator built from this checkout: 42 fixtures replayed, 433 steps, 408 passed, score 94.2%, emulator_up true, bootstrap_signal false. The two clusters below carry an EMPTY stub_path and an EMPTY owned_by, i.e. no filed issue carries their replay key.

Cause 1 — GetQueryExecution, athena-cats-traj-table-before-database-and-ddl-retries#task#17, expected 200 / actual 200:
  $.QueryExecution.Status.StateChangeReason:
    expected 'FAILED: Execution Error, return code 1 from org.apache.hadoop.hive.ql.exec.DDLTask. AlreadyExistsException(message:Table items already exist)'
    got      'FAILED: Execution Error, return code 1 from org.apache.hadoop.hive.ql.exec.DDLTask. Table items already exists'
  $.QueryExecutionDetail.Status.StateChangeReason: same divergence
  Fixed text of the expected message: 'FAILED: Execution Error, return code 1 from org.apache.hadoop.hive.ql.exec.DDLTask. AlreadyExistsException(message:Table ' + ' already exist)'. Variable part: the table name from the submitted CREATE TABLE statement (the recorded run used 'items'). No trailing period.
  divergence_key: service=athena|GetQueryExecution|status=200->200|keys=$.QueryExecution.Status.StateChangeReason,$.QueryExecutionDetail.Status.StateChangeReason

Cause 2 — GetQueryExecution, athena-kfi-traj-athena-reads-the-iceberg-table#task#10, expected 200 / actual 200:
  $.QueryExecution.Statistics.DataScannedInBytes: expected 0, got 746
  $.QueryExecutionDetail.Stats.ProcessedBytes:    expected 0, got 746
  $.QueryExecutionDetail.Stats.ProcessedRows:     expected 0, got 1
  $.QueryExecutionDetail.Stats.ProcessedInputRows: expected 0, got 1
  divergence_key: service=athena|GetQueryExecution|status=200->200|keys=$.QueryExecution.Statistics.DataScannedInBytes,$.QueryExecutionDetail.Stats.ProcessedBytes,$.QueryExecutionDetail.Stats.ProcessedInputRows,$.QueryExecutionDetail.Stats.ProcessedRows

Smithy consulted (services/service-com-amazonaws-athena-amazonathena/smithy-model/com-amazonaws-athena-amazonathena.json): QueryExecutionStatus.StateChangeReason (String, unconstrained free text — 'Further detail about the status of the query'); QueryExecutionStatistics.DataScannedInBytes (Long — 'The number of bytes in the data that was queried'), plus the sibling Processed* counters surfaced through the QueryExecutionDetail.Stats projection.

## Steps to Reproduce
1. git clone the repo and boot the emulator built from that checkout; replay trace set simparity-wire-aws-v1.
2. Cause 1: replay fixture athena-cats-traj-table-before-database-and-ddl-retries up to step task#17 — the fixture creates a table and then re-submits a CREATE TABLE for the same table via StartQueryExecution; poll GetQueryExecution on the returned QueryExecutionId until the state is terminal and read Status.StateChangeReason (and the QueryExecutionDetail.Status.StateChangeReason projection). The emulator returns '... DDLTask. Table items already exists' where AWS returns '... DDLTask. AlreadyExistsException(message:Table items already exist)'.
3. Cause 2: replay fixture athena-kfi-traj-athena-reads-the-iceberg-table up to step task#10 — GetQueryExecution for the Iceberg-table read reports Statistics.DataScannedInBytes 746 and Stats.ProcessedBytes 746 / ProcessedRows 1 / ProcessedInputRows 1, where AWS reports 0 for all four.
4. After a fix, re-replay BOTH bindings — athena-cats-traj-table-before-database-and-ddl-retries#task#17 and athena-kfi-traj-athena-reads-the-iceberg-table#task#10 — and confirm both pass before closing; this issue bundles 2 independent causes.
