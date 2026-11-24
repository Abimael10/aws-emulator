Issue-Source: trace-sweep-divergence
Issue-Trace: athena-kfi-traj-athena-reads-the-iceberg-table
Issue-Step: task#11
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: athena-kfi-traj-athena-reads-the-iceberg-table
Issue-Shape-Id: com.amazonaws.athena#AmazonAthena
Issue-Key: athena|GetQueryResults|2xx->2xx|$.ResultSet.Rows[*].Data[*].VarCharValue
Issue-Related: 20261115T222915Z-athena-query-execution-and-result-reads-diverge-columninfo.md
Issue-Commit: 16df41068e9592847de76789eb93312b3e770273

# Athena GetQueryResults returns a stale row count for an Iceberg-table read

> _Note: this issue was identified against the code base at commit `16df41068e9592847de76789eb93312b3e770273`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetQueryResults

## Description
One net-new root cause in the Athena result-read path: GetQueryResults serves a data value that disagrees with the table state the recorded Athena run reports. There is 1 cause.

=== Cause 1 of 1: the aggregate read over the Iceberg table reports a row count of 1 where Athena reports 0 ===
AWS: recorded behavior only — Smithy com.amazonaws.athena#GetQueryResults returns ResultSet{Rows: RowList, ResultSetMetadata}, Row{Data: datumList}, Datum{VarCharValue: datumString}, all free-form with no constraint on the value, so the model fixes only the shape and the status (200), not the datum. The recording therefore supplies the behavioral requirement: for the aggregate query over the Iceberg table at that point in the sequence, Athena answers 200 and the first data row (Rows[1], row index 0 being the header row) carries the value "0" in its first column — in ResultSet.Rows[1].Data[0].VarCharValue and in the ResultSet.ResultRows[1].Data[0] projection alike. The requirement is the general rule, not the literal digit: the value a query returns must be computed from the rows the table's committed Iceberg snapshot actually holds when the query runs, so an aggregate over a table whose written rows are not visible in the current snapshot counts them as absent. The companion GetQueryExecution read in the same fixture states the same thing from the statistics side (DataScannedInBytes 0, Stats.ProcessedRows / ProcessedInputRows 0): AWS accounts that execution as having read no data rows at all, so a count of 0 is consistent with a read that scanned nothing.
Emulator: for the same execution it answers 200 with "1" in both ResultSet.Rows[1].Data[0].VarCharValue and ResultSet.ResultRows[1].Data[0] — it counts a row that is not visible to the query at that point in the recorded sequence. Status, result-set shape, column count and header row all match; only the datum diverges.
Why: the answer to the query is wrong, which is the one divergence a query client cannot compensate for. Every emptiness check, assertion and downstream arithmetic over the Iceberg table takes the opposite branch against the emulator than against AWS: a "table is still empty" guard passes on AWS and fails here, a count-based wait loop terminates early, and an ETL step that branches on 0-vs-nonzero rows runs the wrong arm. Because the status is 200 and the shape is valid, nothing in a client surfaces this as an error — it silently produces different results.
Fix: the Iceberg/table-read path of the Athena data plane that decides which written rows are visible to an executing query — snapshot/commit visibility in the query-execution state, not the GetQueryResults serializer, which faithfully renders whatever row set it is handed. The same visibility decision also feeds the execution's scan statistics.
Related but NOT covering: open issue 20261115T222915Z-athena-query-execution-and-result-reads-diverge-columninfo.md discusses this fixture step in prose, but the replay reports this cluster with an empty owned_by — no filed issue carries its replay key, so a fixer replaying that issue's pointer never exercises this binding. Also related: 20261111T064644Z-athena-query-submission-and-ddl-execution-ignore-s3.md (query-submission/DDL text) and 20261108T041854Z-athena-getqueryresults-omits-columninfo-catalog-schema-table-updatecount.md (omitted GetQueryResults members); neither covers the row-visibility defect.
Traces: athena-kfi-traj-athena-reads-the-iceberg-table#task#11.

## Evidence
Replay of trace set simparity-wire-aws-v1 against the emulator built from this checkout: 42 fixtures replayed, 433 steps, 375 passed, score 86.6%, bootstrap_signal false, emulator_up true (/__admin/health answered 200 and the :43125 data listener accepts). 33 divergence clusters; 32 carry a stub_path or an owned_by. The cluster below carries neither, i.e. no filed issue carries its replay key.

Cause 1 — GetQueryResults, fixture athena-kfi-traj-athena-reads-the-iceberg-table, step task#11, expected status 200 / actual status 200, verdict mismatch, divergence_key service=athena|GetQueryResults|status=200->200|keys=$.ResultSet.ResultRows[1].Data[0],$.ResultSet.Rows[1].Data[0].VarCharValue:
  $.ResultSet.Rows[1].Data[0].VarCharValue: value_mismatch (expected '0', got '1')
  $.ResultSet.ResultRows[1].Data[0]: value_mismatch (expected '0', got '1')

Corroborating (already owned, listed only as context, not filed here): the preceding GetQueryExecution read of the same execution, athena-kfi-traj-athena-reads-the-iceberg-table#task#10, reports DataScannedInBytes 746 vs expected 0 and Stats.ProcessedRows / ProcessedInputRows 1 vs expected 0 — the same invisible row being counted, seen through the statistics projection.

Message/value analysis: there is no error message here — the divergence is a result datum on a 200 response. The fixed part of the response is its structure (header row at Rows[0], one data row at Rows[1], one column), which matches; the variable part is the datum, which must be derived from the table's committed snapshot contents at query time. The literal '0' in the recording is that derivation's outcome for this sequence, not a constant to hard-code.

Smithy consulted (services/service-com-amazonaws-athena-amazonathena/smithy-model/com-amazonaws-athena-amazonathena.json): GetQueryResults input QueryExecutionId (required, @length 1..128, pattern ^\S+$), output {UpdateCount: Long, ResultSet, NextToken}; errors InternalServerException (500), InvalidRequestException (400), TooManyRequestsException (400); ResultSet{Rows: RowList, ResultSetMetadata}, Row{Data: datumList}, Datum{VarCharValue: datumString} — no constraint on the datum value, hence "recorded behavior only" for the count itself.

## Steps to Reproduce
1. git clone the repo and boot the emulator built from that checkout; replay trace set simparity-wire-aws-v1.
2. Replay fixture athena-kfi-traj-athena-reads-the-iceberg-table from its first step so the Iceberg table and its writes are set up exactly as recorded (the defect is about which of those writes are visible, so earlier steps must not be skipped).
3. Stop at step task#11: call GetQueryResults with the QueryExecutionId of the fixture's aggregate query over the Iceberg table, after the query has reached SUCCEEDED.
4. Inspect ResultSet.Rows[1].Data[0].VarCharValue and the ResultSet.ResultRows[1].Data[0] projection: the emulator answers 200 with '1' in both, where the recorded Athena response carries '0' — the count includes a row not visible in the table's committed snapshot at that point in the sequence.
5. Cross-check at step task#10: GetQueryExecution for the same execution reports DataScannedInBytes 746 and Stats.ProcessedRows/ProcessedInputRows 1 where AWS reports 0, confirming the same extra row is being read.
6. After a fix, re-replay athena-kfi-traj-athena-reads-the-iceberg-table#task#11 and confirm the datum matches the recorded snapshot-derived count before closing.
