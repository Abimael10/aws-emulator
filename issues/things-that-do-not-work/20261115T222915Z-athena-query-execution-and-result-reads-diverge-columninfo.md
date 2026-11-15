Issue-Source: trace-sweep-divergence
Issue-Trace: athena-cats-traj-query-execution-polling-reads
Issue-Step: task#5
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: athena-cats-traj-query-execution-polling-reads athena-kfi-traj-athena-query-context-mistakes athena-cats-traj-table-before-database-and-ddl-retries athena-kfi-traj-athena-reads-the-iceberg-table
Issue-Shape-Id: com.amazonaws.athena#AmazonAthena
Issue-Key: athena|GetQueryResults|2xx->2xx|$.ResultSet.ColumnInfos[*].CaseSensitive
Issue-Key: athena|GetQueryResults|2xx->2xx|$.ResultSet.ColumnInfos[*].Precision
Issue-Key: athena|GetQueryResults|2xx->2xx|$.ResultSet.ColumnInfos[*].Type
Issue-Key: athena|GetQueryExecution|2xx->2xx|$.QueryExecutionDetail.Status.StateChangeReason
Issue-Key: athena|GetQueryExecution|2xx->2xx|$.QueryExecutionDetail.Stats.ProcessedBytes
Issue-Key: athena|GetQueryExecution|2xx->2xx|$.QueryExecutionDetail.Stats.ProcessedInputRows
Issue-Key: athena|GetQueryExecution|2xx->2xx|$.QueryExecutionDetail.Stats.ProcessedRows
Issue-Key: athena|GetQueryResults|2xx->2xx|$.ResultSet.ResultRows[*].Data[*]
Issue-Related: 20261108T041854Z-athena-getqueryresults-omits-columninfo-catalog-schema-table-updatecount.md
Issue-Related: 20261111T064644Z-athena-query-submission-and-ddl-execution-ignore-s3.md
Issue-Commit: c2a51bbc379e00e6c722f42efee8d77cde344066

# Athena query execution and result reads diverge: ColumnInfo string typing, Hive already-exists wording, and Iceberg scan statistics and row count (4 causes)

> _Note: this issue was identified against the code base at commit `c2a51bbc379e00e6c722f42efee8d77cde344066`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetQueryResults

## Description
Four net-new root causes in the Athena query-execution read path (GetQueryResults / GetQueryExecution). All four are read-back divergences on queries the emulator itself ran: the query reaches a terminal state, but the metadata, the failure text, the statistics and the row data it reports are not what Athena reports. Each cause must be closed separately; there are 4 of them.

=== Cause 1 of 4: GetQueryResults describes a Hive STRING column as a 2147483647-precision case-sensitive varchar ===
AWS: Smithy com.amazonaws.athena#ColumnInfo declares Type (required String), Precision (Integer, @default 0, documented as meaningful only "For DECIMAL data types"), Scale (@default 0) and CaseSensitive (Boolean, @default false). For a column whose catalog type is Hive `string`, the recorded Athena response reports ColumnInfo.Type = "string", Precision = 0 and CaseSensitive = false, in both ResultSet.ResultSetMetadata.ColumnInfo[*] and the ResultSet.ColumnInfos[*] projection. The requirement is the general rule: the column's catalog/engine type name is carried through verbatim (`string` stays `string`), Precision stays at its modelled default of 0 for a non-DECIMAL column, and CaseSensitive is the modelled default false unless the column really is case-sensitive — not a per-recording value.
Emulator: for the same column it answers Type = "varchar", Precision = 2147483647 and CaseSensitive = true (same three keys wrong in both ColumnInfo and ColumnInfos). Status is 200 in both, so only the metadata diverges.
Why: a client that drives a JDBC/ODBC-style result cursor or generates DDL from result metadata sees a different SQL type than Athena reports (`varchar` vs `string`), a bogus declared precision of 2147483647 for a type that has no precision, and a case-sensitivity flag inverted from AWS's default — so type mapping, column sizing and case-insensitive comparisons all come out wrong against the emulator while they are right against AWS.
Fix: the GetQueryResults serializer / the type-mapping layer that builds ColumnInfo from the engine's column descriptors — it must emit the catalog type name and the modelled defaults (Precision 0, Scale 0, CaseSensitive false) rather than hard-coded varchar/max-precision/true.
Note: this cluster also reports `$.Output` missing (expected ""). That member's omission is already owned by the open issue 20261108T041854Z-athena-getqueryresults-omits-columninfo-catalog-schema-table-updatecount.md; the ColumnInfo type/precision/case-sensitivity divergence above is NOT covered by it (its clusters cover CatalogName/SchemaName/TableName/UpdateCount), so it is filed here.
Traces: athena-cats-traj-query-execution-polling-reads#task#5 (representative), athena-kfi-traj-athena-query-context-mistakes#task#12.

=== Cause 2 of 4: a CREATE DATABASE that collides reports a Java exception wrapper instead of Athena's Hive DDL failure text ===
AWS: recorded behavior only (Smithy models QueryExecutionStatus.StateChangeReason and AthenaError.ErrorMessage as free-form String, with no text constraint). For a CREATE DATABASE whose database already exists, GetQueryExecution returns 200 with State FAILED and:
  - QueryExecution.Status.AthenaError.ErrorMessage = "Database <databaseName> already exists." — fixed text "Database " + " already exists." with the requested database name, including the terminating period.
  - QueryExecution.Status.StateChangeReason and QueryExecutionDetail.Status.StateChangeReason = "FAILED: Execution Error, return code 1 from org.apache.hadoop.hive.ql.exec.DDLTask. Database <databaseName> already exists" — fixed prefix "FAILED: Execution Error, return code 1 from org.apache.hadoop.hive.ql.exec.DDLTask. " followed by the bare "Database <databaseName> already exists" sentence with NO exception-class wrapper and NO trailing period. The only variable part of either message is the database name taken from the submitted statement.
Emulator: ErrorMessage = "Database <databaseName> already exists" (trailing period dropped) and both StateChangeReason values wrap the sentence in a Java exception rendering: "... DDLTask. AlreadyExistsException(message:Database <databaseName> already exists)".
Why: clients and runbooks classify Athena DDL failures by matching the DDLTask reason text; the extra "AlreadyExistsException(message:...)" wrapper and the missing period mean an already-exists collision is not recognised as such, so idempotent create-if-absent logic and retry/alert rules that pass on AWS fail on the emulator.
Fix: the StartQueryExecution DDL execution path's failure-text construction (the code that fills QueryExecutionStatus.StateChangeReason and AthenaError.ErrorMessage for a DDL conflict) — it must emit Athena's bare Hive sentence, not the underlying exception's toString.
Related but not covering: 20261111T064644Z-athena-query-submission-and-ddl-execution-ignore-s3.md owns the DROP DATABASE "does not exist" wording cluster; the already-exists CREATE DATABASE cluster here is unowned.
Traces: athena-cats-traj-table-before-database-and-ddl-retries#task#14 (representative), athena-cats-traj-table-before-database-and-ddl-retries#task#17.

=== Cause 3 of 4: an Iceberg-table read reports non-zero scan statistics where Athena reports zero ===
AWS: Smithy QueryExecutionStatistics.DataScannedInBytes is "The number of bytes in the data that was queried". For the recorded query over the Iceberg table, GetQueryExecution returns 200 with QueryExecution.Statistics.DataScannedInBytes = 0 and the detail projection QueryExecutionDetail.Stats.ProcessedBytes = 0, ProcessedRows = 0, ProcessedInputRows = 0 — i.e. the statistics reflect what the engine actually had to read for that query, which for this plan is nothing. Hypothesis: Athena answers the aggregate from Iceberg table metadata/manifest counts and therefore scans no data rows.
Emulator: DataScannedInBytes = 746, ProcessedBytes = 746, ProcessedRows = 1, ProcessedInputRows = 1 — it reports bytes and rows for a query AWS accounts as a zero-scan read.
Why: a client that uses DataScannedInBytes for cost attribution, for a BytesScannedCutoffPerQuery budget, or to assert that an Iceberg aggregate was answered from metadata reads fabricated non-zero work, so cost/efficiency assertions that hold on AWS fail against the emulator.
Fix: the query-execution statistics accounting in the StartQueryExecution/GetQueryExecution state (the component that populates QueryExecutionStatistics and the QueryExecutionDetail.Stats projection) — statistics must come from what the plan actually scanned rather than from the rows the emulator happened to materialise.
Traces: athena-kfi-traj-athena-reads-the-iceberg-table#task#10.

=== Cause 4 of 4: the same Iceberg-table read returns a row count of 1 where Athena returns 0 ===
AWS: recorded behavior only. GetQueryResults for that execution returns 200 whose data row carries the value "0" in the first column — in both ResultSet.Rows[1].Data[0].VarCharValue and the ResultSet.ResultRows[1].Data[0] projection (row 0 being the header row). The requirement is that the value is the count the table's committed Iceberg snapshot actually holds at that point in the recorded sequence, not a constant.
Emulator: returns "1" in both places — it counts a row the table does not hold at that point in the sequence.
Why: the result of the query is wrong, which is the one thing a query client cannot work around: assertions, downstream arithmetic and emptiness checks on the Iceberg table all take the opposite branch against the emulator.
Fix: the Iceberg/table-read path in the Athena data plane that decides which written rows are visible to a query (snapshot/commit visibility in the query execution state), not the serializer — the serializer faithfully reports the wrong row set it is given.
Traces: athena-kfi-traj-athena-reads-the-iceberg-table#task#11.

## Evidence
Replay of trace set simparity-wire-aws-v1 (37 fixtures, 394 steps, 299 passed, score 75.9%). The four clusters below carry no stub_path and no owned_by, i.e. no filed issue covers them.

Cause 1 — GetQueryResults, athena-cats-traj-query-execution-polling-reads#task#5 (also athena-kfi-traj-athena-query-context-mistakes#task#12), expected 200 / actual 200:
  $.ResultSet.ResultSetMetadata.ColumnInfo[0].Type: expected 'string', got 'varchar'
  $.ResultSet.ResultSetMetadata.ColumnInfo[0].Precision: expected 0, got 2147483647
  $.ResultSet.ResultSetMetadata.ColumnInfo[0].CaseSensitive: expected False, got True
  $.ResultSet.ColumnInfos[0].Type / .Precision / .CaseSensitive: same three divergences
  ($.Output: missing, expected '' — already owned by 20261108T041854Z-athena-getqueryresults-omits-columninfo-catalog-schema-table-updatecount.md)

Cause 2 — GetQueryExecution, athena-cats-traj-table-before-database-and-ddl-retries#task#14 (also #task#17), expected 200 / actual 200:
  $.QueryExecution.Status.AthenaError.ErrorMessage: expected 'Database <db> already exists.', got 'Database <db> already exists'
  $.QueryExecution.Status.StateChangeReason: expected 'FAILED: Execution Error, return code 1 from org.apache.hadoop.hive.ql.exec.DDLTask. Database <db> already exists', got 'FAILED: Execution Error, return code 1 from org.apache.hadoop.hive.ql.exec.DDLTask. AlreadyExistsException(message:Database <db> already exists)'
  $.QueryExecutionDetail.Status.StateChangeReason: same divergence
  (<db> is the database name from the submitted CREATE DATABASE statement; the recorded run used sim2real_probe_cats_0392935c)

Cause 3 — GetQueryExecution, athena-kfi-traj-athena-reads-the-iceberg-table#task#10, expected 200 / actual 200:
  $.QueryExecution.Statistics.DataScannedInBytes: expected 0, got 746
  $.QueryExecutionDetail.Stats.ProcessedBytes: expected 0, got 746
  $.QueryExecutionDetail.Stats.ProcessedRows: expected 0, got 1
  $.QueryExecutionDetail.Stats.ProcessedInputRows: expected 0, got 1

Cause 4 — GetQueryResults, athena-kfi-traj-athena-reads-the-iceberg-table#task#11, expected 200 / actual 200:
  $.ResultSet.Rows[1].Data[0].VarCharValue: expected '0', got '1'
  $.ResultSet.ResultRows[1].Data[0]: expected '0', got '1'

Smithy consulted (services/service-com-amazonaws-athena-amazonathena/smithy-model/com-amazonaws-athena-amazonathena.json): ColumnInfo.Type (required String), ColumnInfo.Precision (Integer @default 0, DECIMAL-only per doc), ColumnInfo.CaseSensitive (Boolean @default false); QueryExecutionStatistics.DataScannedInBytes (Long); QueryExecutionStatus.StateChangeReason (String) and AthenaError.ErrorMessage (String), both unconstrained free text.

## Steps to Reproduce
1. git clone the repo and boot the emulator built from it; replay trace set simparity-wire-aws-v1.
2. Cause 1: replay fixture athena-cats-traj-query-execution-polling-reads and stop at step task#5 — submit the fixture's SELECT over a catalog table with a Hive `string` column via StartQueryExecution, poll GetQueryExecution to SUCCEEDED, then call GetQueryResults and inspect ResultSet.ResultSetMetadata.ColumnInfo[0] (and the ResultSet.ColumnInfos[0] projection): Type/Precision/CaseSensitive come back varchar/2147483647/true instead of string/0/false. Same at athena-kfi-traj-athena-query-context-mistakes#task#12.
3. Cause 2: replay fixture athena-cats-traj-table-before-database-and-ddl-retries to step task#14 — StartQueryExecution a CREATE DATABASE for a database that already exists, then GetQueryExecution on the returned QueryExecutionId: Status.AthenaError.ErrorMessage lacks the trailing period and both StateChangeReason fields carry the 'AlreadyExistsException(message:...)' wrapper. Step task#17 repeats it.
4. Cause 3: replay fixture athena-kfi-traj-athena-reads-the-iceberg-table to step task#10 — GetQueryExecution for the Iceberg-table query reports DataScannedInBytes 746 and Stats.ProcessedRows/ProcessedInputRows 1 where AWS reports 0 for all of them.
5. Cause 4: continue the same fixture to step task#11 — GetQueryResults for that execution returns '1' in Rows[1].Data[0].VarCharValue (and ResultRows[1].Data[0]) where AWS returns '0'.
6. Re-replay all four fixture#step bindings after a fix; all four causes must pass before closing.
