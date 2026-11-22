Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: redshift.rdapi-cancelled-statement-aborts-and-second-cancel-rejected redshift.data-api-wait-time-seconds-returns-a-terminal-status-without-a-poll
Issue-Key: redshift.rdapi-cancelled-statement-aborts-and-second-cancel-rejected|run#28|AssertionError|assert control == [[{"longValue": 5}]], (
Issue-Key: redshift.data-api-wait-time-seconds-returns-a-terminal-status-without-a-poll|_wait_until#23|AssertionError|raise AssertionError(
Issue-Commit: 9f15073fe42cc4661a8a99924cff5eec339d1b59

# redshift Data API SQL engine: 2 net-new causes — stv_blocklist serves a single row so a 'SELECT ... FROM stv_blocklist LIMIT 5' insert commits 1 row, and a submitted self-join statement never reaches a terminal status (stuck STARTED with Duration 0)

> _Note: this issue was identified against the code base at commit `9f15073fe42cc4661a8a99924cff5eec339d1b59`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ExecuteStatement (redshift-data against a provisioned cluster)

## Description
Replay of simparity-behavioral-aws-v1 against the provisioned cluster's Data API engine (redshift-data ExecuteStatement/DescribeStatement on the emulated cluster). This bundle carries TWO causes; BOTH must be closed before submitting. The suite marked neither check ALREADY FILED or OWNED. Related but NOT covering them: open issues 20261029T193810Z-redshift-cluster-sql-engine-multi-statement-sql-refused.md (it filed stv_blocklist as ABSENT; the relation now resolves but is under-populated), 20261114T083409Z-redshift-cluster-sql-engine-3-net-new-causes.md and 20261116T000944Z-redshift-cluster-sql-engine-2-net-new-causes.md.

=== Cause 1 of 2: the system table stv_blocklist holds a single row, so a row-generating SELECT over it yields fewer rows than asked for ===
AWS: recorded behavior only (no Smithy surface describes SQL). On any Redshift cluster stv_blocklist carries one row per 1 MB disk block per slice, so it always holds many rows (thousands even on an empty single-node cluster); it is the idiomatic Redshift row generator, and INSERT INTO t SELECT 1 FROM stv_blocklist LIMIT 5 commits exactly five rows.
Emulator: the same INSERT commits ONE row — SELECT count(*) on the target table returned 1 where 5 was asked for — i.e. the relation resolves but is populated with a single synthetic row, so LIMIT n for n>1 silently under-delivers.
Why: stv_blocklist is used all over real Redshift SQL as a row generator and as the basis of storage/skew diagnostics; any statement whose row count comes from it produces too few rows, and the client sees a silently truncated load rather than an error.
Fix: the cluster SQL engine's system-table (STV/STL/SVV) catalog — back stv_blocklist with one row per block per slice derived from the engine's own stored tables, so its cardinality scales with the data instead of being a fixed single row.
Traces: redshift.rdapi-cancelled-statement-aborts-and-second-cancel-rejected (AssertionError at check line 96; the cancel/abort semantics the check asserts before this all passed).

=== Cause 2 of 2: an accepted statement never reaches a terminal status — it stays STARTED with Duration 0 and UpdatedAt frozen ===
AWS: recorded behavior only. Every statement ExecuteStatement accepts reaches one of the terminal statuses FINISHED/FAILED/ABORTED, and DescribeStatement then reports a positive Duration; a count over a self-join of an 80,000-row single-column table is a few seconds of work on the cheapest single-node cluster and completes well inside a 120 s budget, with UpdatedAt advancing as the statement progresses.
Emulator: the statement is accepted (HTTP 200) and then never converges: DescribeStatement reported Status 'STARTED', Duration 0 and UpdatedAt still equal to CreatedAt after 120 s of polling for SELECT count(*) FROM <filler> a, <filler> b WHERE a.n + b.n = -1 over an 80,000-row table. Nothing ever moves it to FINISHED or FAILED.
Why: a client that submits a non-trivial analytic query hangs forever — pollers never exit, WaitTimeSeconds/synchronous-submit semantics are unobservable, and nothing downstream of such a statement (results, events, cancellation) can be reached. A server that cannot finish a statement must at least fail it, not park it in a non-terminal state with a zero Duration.
Fix: the Data API statement executor and its statement-record state machine — execute the engine's join plan to completion (or fail the statement with its engine error), and advance Status/Duration/UpdatedAt on the record so the statement always reaches a terminal status.
Traces: redshift.data-api-wait-time-seconds-returns-a-terminal-status-without-a-poll (AssertionError at _wait_until, check line 100).

## Evidence
Suite: simparity-behavioral-aws-v1 — total 200/264 (75.8%); redshift 157/197 (80%), 40 failed (33 of them OWNED/ALREADY FILED and skipped).

Cause 1 — check_redshift_rdapi_cancelled_statement_aborts_and_second_cancel_rejected.py:96
  statements: CREATE TABLE <ctrl> (n INT); INSERT INTO <ctrl> SELECT 1 FROM stv_blocklist LIMIT 5; SELECT count(*) FROM <ctrl>
  expected:  [[{'longValue': 5}]]
  actual:    AssertionError: DATAPLANE_UNIMPLEMENTED: an uncancelled INSERT on the same cluster must commit its five rows; got [[{'longValue': 1}]]

Cause 2 — check_redshift_data_api_wait_time_seconds_returns_a_terminal_status_without_a_poll.py:100 (_wait_until via _wait_statement)
  statement: SELECT count(*) FROM <filler> a, <filler> b WHERE a.n + b.n = -1   (<filler> loaded with exactly 80,000 rows by the check, verified by its own count)
  expected:  a terminal Status (FINISHED) with a positive Duration inside the 120 s budget
  actual:    AssertionError: timed out after 120s waiting for statement <id> to reach a terminal status; last observed Status 'STARTED', Duration 0, ResultRows 0, ResultSize 0, with UpdatedAt equal to CreatedAt (2026-11-22T06:22:58 observed at 06:24:58), HTTP 200

(cluster ids, statement ids, generated table names and timestamps above are per-run values; the requirements are stv_blocklist's cardinality and that an accepted statement always reaches a terminal status with a positive Duration.)

## Steps to Reproduce
1. Clone the repo and boot the emulator. CreateCluster a single-node Redshift cluster (DBName devdb) and wait for 'available'.
2. Cause 1: via redshift-data ExecuteStatement run CREATE TABLE ctrl (n INT), then INSERT INTO ctrl SELECT 1 FROM stv_blocklist LIMIT 5, then SELECT count(*) FROM ctrl -> expect 5; observe 1. (SELECT count(*) FROM stv_blocklist directly shows the single row.)
3. Cause 2: create a one-column table and load it with exactly 80,000 rows (e.g. a 5-way cross join of a 0..9 seed table with a < 80000 filter); confirm the count. Then ExecuteStatement 'SELECT count(*) FROM filler a, filler b WHERE a.n + b.n = -1' and poll DescribeStatement -> expect FINISHED with a positive Duration within 120 s; observe Status 'STARTED', Duration 0 and UpdatedAt never advancing.
4. Re-replay every check id in trace_ids to confirm the fix.
