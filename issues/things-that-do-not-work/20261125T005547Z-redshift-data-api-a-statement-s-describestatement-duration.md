Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: redshift.data-api-wait-time-seconds-returns-a-terminal-status-without-a-poll
Issue-Key: redshift.data-api-wait-time-seconds-returns-a-terminal-status-without-a-poll|run#27|AssertionError|assert duration_s >= _SLOW_FLOOR_S, (
Issue-Commit: 601df947af41928830de48079c97009550b26bdc

# redshift Data API: a statement's DescribeStatement.Duration does not reflect the work it did — a 6.4e9-pair cross self-join over an 80,000-row table FINISHES in 0.12 s, so WaitTimeSeconds blocking semantics are unobservable

> _Note: this issue was identified against the code base at commit `601df947af41928830de48079c97009550b26bdc`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeStatement

## Description
Replay of simparity-behavioral-aws-v1 against the provisioned cluster's Data API engine (redshift-data ExecuteStatement/DescribeStatement on an emulated Redshift cluster). ONE net-new cause. The suite marked this check neither ALREADY FILED nor OWNED. Context, NOT a reopen: fixed issue 20261122T064140Z-redshift-data-api-sql-engine-2-net-new.md filed the same check when the self-join statement never left STARTED with Duration 0; the statement now does reach FINISHED (HTTP 200, no refusal anywhere), so that fix landed and this is the next, distinct divergence the same check now reaches 284 lines later (check line 384 instead of line 100).

=== Cause 1 of 1: DescribeStatement.Duration is not the time the statement ran — an analytic statement whose evaluation requires ~6.4e9 row pairs is reported as having run for 0.12 s ===
AWS: Smithy (com-amazonaws-redshiftdata-redshiftdata): DescribeStatement output member `Duration` is `Long` (default 0) and is documented as "The amount of time in nanoseconds that the statement ran."; ExecuteStatement/DescribeStatement `WaitTimeSeconds` is `@range(min: 1, max: 30)`. Beyond that the SQL execution cost is recorded behavior only: on a real single-node cluster an unindexable comma (Cartesian) self-join of a one-column 80,000-row table — `SELECT count(*) FROM <t> a, <t> b WHERE a.n + b.n = -1`, i.e. 80,000 x 80,000 = 6.4e9 row pairs with an arithmetic predicate that gets no usable hash key — runs for seconds, and DescribeStatement reports that elapsed execution time in nanoseconds. That real, multi-second cost is what makes the three WaitTimeSeconds arms observable at all: a submit with no WaitTimeSeconds must return before the query completes and carry neither `Status` nor `HasResultSet`; a wait SHORTER than the statement must actually block and then return a non-terminal Status (SUBMITTED/PICKED/STARTED); a wait LONGER than the statement must return a terminal Status in the submit response.
Emulator: the statement is accepted and FINISHES, but DescribeStatement reports `Duration` = 120,000,000 ns (0.12 s) for that join, after the check has already verified from the emulator's own `SELECT count(*)` that the joined table holds exactly 80,000 rows. No engine can evaluate 6.4e9 row pairs in 120 ms, so the emulator is either not evaluating the comma join as a Cartesian product of the two aliases, or `Duration` is a synthesized value rather than measured execution time. Hypothesis: one of those two; the replay output distinguishes only the observable, that `Duration` is orders of magnitude below the statement's true cost. Because every submission on this engine returns in milliseconds, the WaitTimeSeconds contract above cannot be exercised at all: there is no statement slow enough for a 1..30 s wait to time out against, so a target that accepts `WaitTimeSeconds` and ignores it is indistinguishable from one that honors it.
Why: a client that reads `Duration` to report or budget query time (console-style timings, cost attribution, slow-query alerting, deciding whether to poll or to pass WaitTimeSeconds) gets a number unrelated to the work performed; and if the comma join is not expanded to a Cartesian product, every `count(*)`/aggregate over such a join silently returns the wrong cardinality rather than an error, which is worse than a refusal because nothing signals it.
Fix: the cluster SQL engine's join planner/executor (a comma-separated FROM with two aliases of the same relation must materialize the full cross product before the WHERE predicate is applied) and the Data API statement record's timing field — `Duration` must be the statement's measured execution elapsed time in nanoseconds, taken from the executor, not derived from a constant or from request-handling time.
Traces: redshift.data-api-wait-time-seconds-returns-a-terminal-status-without-a-poll (AssertionError at check line 384, the premise assertion that establishes the widener's cost from the target's own reported Duration before the three WaitTimeSeconds arms run).

## Evidence
Suite: simparity-behavioral-aws-v1 — total 225/264 (85.2%); redshift 180/197 (91%), 17 failed (16 of them OWNED by open issues and skipped).

[mismatch] redshift.data-api-wait-time-seconds-returns-a-terminal-status-without-a-poll
check_redshift_data_api_wait_time_seconds_returns_a_terminal_status_without_a_poll.py:384

Setup that succeeded against the emulator (so the premise is established from the emulator's own answers, not assumed):
  CREATE TABLE <seed> (n INT); INSERT INTO <seed> VALUES (0)..(9)
  CREATE TABLE <filler> (n INT);
  INSERT INTO <filler> SELECT a.n*10000 + b.n*1000 + c.n*100 + d.n*10 + e.n FROM <seed> a, <seed> b, <seed> c, <seed> d, <seed> e WHERE ... < 80000
  SELECT count(*) FROM <filler>  ->  80000   (asserted, PASSED)

The timed statement:
  SELECT count(*) FROM <filler> a, <filler> b WHERE a.n + b.n = -1     (80,000 x 80,000 = 6.4e9 row pairs)
  ExecuteStatement -> HTTP 200; DescribeStatement -> Status 'FINISHED'

  expected: Duration >= 3,000,000,000 ns (>= 3.0 s), i.e. a nanosecond count equal to the time the statement actually ran
  actual:   Duration = 120,000,000 ns (0.12 s)

  AssertionError: the widener query must take at least 3.0s on this cluster for the WaitTimeSeconds floor below to test anything; DescribeStatement reported 0.12s. RAISE _WIDENER_ROWS (currently 80000) rather than weakening the assertion

Consequently none of the three WaitTimeSeconds arms the check goes on to assert were reached:
  arm 1 (no WaitTimeSeconds): response must omit `Status` and `HasResultSet` and return before the query completes
  arm 2 (WaitTimeSeconds=2 on a multi-second statement): must block >= 1.0 s and return a Status in (SUBMITTED, PICKED, STARTED)
  arm 3 (WaitTimeSeconds=30 on a fast statement): must return a terminal Status in the submit response

Per-run values above (cluster id, statement id, generated table names, the exact 0.12 s reading) are environment-specific; the requirements are that DescribeStatement.Duration is the statement's measured nanosecond execution time and that a comma self-join is evaluated as a full Cartesian product.

## Steps to Reproduce
1. Clone the repo and boot the emulator. CreateCluster a single-node Redshift cluster (DBName devdb, admin user adminuser) and wait for ClusterStatus 'available'.
2. Via redshift-data ExecuteStatement on that cluster: CREATE TABLE seed (n INT); INSERT INTO seed VALUES (0),(1),...,(9); CREATE TABLE filler (n INT); INSERT INTO filler SELECT a.n*10000+b.n*1000+c.n*100+d.n*10+e.n FROM seed a, seed b, seed c, seed d, seed e WHERE a.n*10000+b.n*1000+c.n*100+d.n*10+e.n < 80000. Confirm SELECT count(*) FROM filler returns 80000.
3. ExecuteStatement 'SELECT count(*) FROM filler a, filler b WHERE a.n + b.n = -1', poll DescribeStatement until FINISHED, and read Duration -> expect a nanosecond count corresponding to seconds of execution (>= 3e9 ns on the cheapest single-node class); observe 120,000,000 ns (0.12 s).
4. Also read the statement's result: SELECT count(*) over the cross product must be computed over 6.4e9 row pairs (the predicate makes the count 0, but the plan must be the full cross product) — verify the planner expands the comma join rather than pairing the aliases row-wise.
5. Then exercise the WaitTimeSeconds arms against that slow statement: submit with no WaitTimeSeconds (response must carry neither Status nor HasResultSet and return before completion), submit with WaitTimeSeconds=2 (must block ~2 s and return a non-terminal Status), and submit a trivial statement with WaitTimeSeconds=30 (must return a terminal Status inline).
6. Re-replay redshift.data-api-wait-time-seconds-returns-a-terminal-status-without-a-poll to confirm the fix.
