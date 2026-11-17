Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: rds.iam-db-auth-token-minted-for-another-user-is-refused rds.pg-dialect-numeric-timestamptz-jsonb-arrays-returning-recursive-cte rds.postgres-idle-in-transaction-session-timeout-terminates-session rds.deep-rds-1-rds-engine-truth-mysql-constraint-lock-and-charset-violations-rejected rds.deep-rds-sql-truth-iam-token-past-expiry-rejected rds.mysql-iam-db-auth-token-binding-and-validity-enforced rds.postgres-create-extension-restricted-by-allowed-extensions rds.rds-kill-ends-session-with-lost-connection-error rds.deep-rds-1-rds-engine-truth-aurora-postgres-constraint-and-lock-violations-rejected
Issue-Shape-Id: com.amazonaws.rds#AmazonRDSv19
Issue-Key: rds.iam-db-auth-token-minted-for-another-user-is-refused|_sql_rows#20|UndefinedTable|cur.execute(statement, params)
Issue-Key: rds.pg-dialect-numeric-timestamptz-jsonb-arrays-returning-recursive-cte|_rows#7|UndefinedTable|cur = conn.execute(sql, params)
Issue-Key: rds.postgres-idle-in-transaction-session-timeout-terminates-session|_rows#21|SyntaxError|cur = conn.execute(sql, params)
Issue-Key: rds.deep-rds-1-rds-engine-truth-mysql-constraint-lock-and-charset-violations-rejected|_refusal#6|AssertionError|raise AssertionError(f"the engine ACCEPTED a statement it must refuse: {statement!r} {params!r}")
Issue-Key: rds.deep-rds-sql-truth-iam-token-past-expiry-rejected|run#15|AssertionError|assert refused == ("refused", _REFUSED_AUTH), (
Issue-Key: rds.mysql-iam-db-auth-token-binding-and-validity-enforced|_my_refusal#11|AssertionError|raise AssertionError(f"expected the login to be refused, but it was admitted: {own!r}")
Issue-Key: rds.postgres-create-extension-restricted-by-allowed-extensions|_pg_error#21|AssertionError|raise AssertionError(f"expected the engine to refuse {sql!r}, but it succeeded")
Issue-Key: rds.rds-kill-ends-session-with-lost-connection-error|_wait_for_processlist_state#7|AssertionError|raise AssertionError(
Issue-Key: rds.deep-rds-1-rds-engine-truth-aurora-postgres-constraint-and-lock-violations-rejected|_poll_until#6|Timeout|time.sleep(interval_s)
Issue-Commit: 7f5dd6cdee8fc233932a8e3b3b251e9cc2539379

# rds: 7 net-new behavioral causes in the emulated SQL data plane — the PostgreSQL dialect rejects ANY(), INTERVAL literals and EXTRACT(... FROM ...), MySQL stores a 4-byte emoji in a non-utf8mb4 column, both IAM-auth token contracts are wrong, rds.allowed_extensions is unenforced, SHOW PROCESSLIST reports no State, and the aurora-postgresql engine-truth flow never converges

> _Note: this issue was identified against the code base at commit `7f5dd6cdee8fc233932a8e3b3b251e9cc2539379`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ExecuteStatement

## Description
Seven net-new root causes in the data plane the RDS endpoints serve, all found by replaying the behavioral suite against the emulator built from this checkout. They are independent and each must be closed on its own.

Cause 1 of 7: the emulated PostgreSQL engine does not implement core PostgreSQL expression syntax, and leaks SQLite-shaped errors when it meets it.
AWS: an RDS/Aurora PostgreSQL endpoint speaks PostgreSQL. 'x = ANY(<array parameter>)' is the array comparison form every driver uses to bind a list; 'interval '1 month'' is an INTERVAL literal; 'EXTRACT(EPOCH FROM (now() - state_change))' is the EXTRACT(field FROM source) form. Reference: PostgreSQL functions-comparisons, datatype-datetime and functions-datetime. All three are valid in every supported PostgreSQL major version and must execute, not be refused. Recorded behavior only for the exact wire text, since the Smithy model does not describe the SQL dialect.
Emulator: each is refused, with a message that is a SQLite diagnostic surfaced through the PostgreSQL error channel: 'SELECT rolname FROM pg_roles WHERE rolname = ANY(%s)' -> psycopg UndefinedTable 'no such function: ANY'; "SELECT '2024-01-31 00:00:00+00'::timestamptz + interval '1 month'" -> psycopg UndefinedTable 'no such column: interval'; 'SELECT state, EXTRACT(EPOCH FROM (now() - state_change)) FROM pg_stat_activity WHERE pid = %s' -> psycopg SyntaxError 'syntax error at or near "FROM"'. The SQLSTATE class is wrong too: a missing function or an unparsable expression is not UndefinedTable (42P01).
Why: ordinary, driver-generated SQL fails against the endpoint. Binding a list of ids, doing date arithmetic, or reading an elapsed time out of pg_stat_activity are all everyday operations; an application ported onto the emulator sees them fail with a message naming a storage engine PostgreSQL does not have, and cannot even classify the failure by SQLSTATE.
Fix: the emulated PostgreSQL dialect layer (the SQL parser/planner behind the postgres and aurora-postgresql listeners) — implement ANY(array), INTERVAL literals and EXTRACT(field FROM source), and map the backing engine's errors onto PostgreSQL SQLSTATEs and wording rather than passing SQLite text through.
Traces: rds.iam-db-auth-token-minted-for-another-user-is-refused (ANY), rds.pg-dialect-numeric-timestamptz-jsonb-arrays-returning-recursive-cte (INTERVAL), rds.postgres-idle-in-transaction-session-timeout-terminates-session (EXTRACT ... FROM).

Cause 2 of 7: the emulated MySQL engine stores a 4-byte character in a column whose character set cannot hold it instead of refusing the INSERT.
AWS: on a MySQL/MariaDB RDS endpoint, inserting a character outside the BMP (U+1F600 GRINNING FACE, 4 bytes in UTF-8) into a column whose charset is utf8/latin1 rather than utf8mb4 is refused — MySQL answers error 1366 'Incorrect string value: '<hex bytes>' for column '<column>' at row <n>' (and the statement has no effect). Only a utf8mb4 column accepts it. Recorded behavior only for the exact text; the refusal itself is the documented MySQL charset contract.
Emulator: the INSERT is ACCEPTED — the replay reports 'the engine ACCEPTED a statement it must refuse: INSERT INTO <table> (id, body) VALUES (2, %s)' with the emoji bound. No error, and the row lands.
Why: an application that relies on the engine to reject un-encodable text silently stores data its declared charset cannot represent; on real RDS the same write is rejected, so the emulator hides a class of data-corruption bug instead of reproducing AWS's refusal.
Fix: the emulated MySQL engine's column character-set enforcement on INSERT/UPDATE — reject values that cannot be encoded in the target column's charset with MySQL error 1366 naming the column and the offending bytes.
Traces: rds.deep-rds-1-rds-engine-truth-mysql-constraint-lock-and-charset-violations-rejected.

Cause 3 of 7: an expired IAM DB-auth token is refused as a wrong password instead of by PAM.
AWS: IAM database authentication on PostgreSQL is enforced through the rds_iam role and PAM. A token whose X-Amz-Date + X-Amz-Expires window has passed is rejected at connect time by the PAM module, and the server's FATAL line is 'PAM authentication failed for user "<user>"' — the fixed text is 'PAM authentication failed', the variable part is the login role from the connection request. A plain wrong password produces the different 'password authentication failed for user "<user>"' line, and the two are distinguishable on purpose.
Emulator: the expired token IS refused (good), but with 'FATAL:  password authentication failed for user "<role>"' — the password-authentication wording, not the PAM wording.
Why: a client cannot tell 'your IAM token has expired, mint a new one and retry' from 'these credentials are wrong, stop retrying'. Token-refresh logic keyed on the PAM text never fires, so a long-lived connection pool retries with the stale token or gives up on a recoverable condition.
Fix: the IAM-auth path of the emulated PostgreSQL listener — when the login presents an rds_iam token that fails validation (expired, wrong signature, wrong user), emit PostgreSQL's PAM failure line rather than the password-authentication line.
Traces: rds.deep-rds-sql-truth-iam-token-past-expiry-rejected.

Cause 4 of 7: on MySQL, a tampered IAM DB-auth token is ADMITTED.
AWS: an RDS auth token is a presigned, signed string; its signature binds it to the DB user, the host, the port and its validity window. A token that has been altered — or minted for a different user — does not validate, and the login is refused (MySQL error 1045 Access denied for user). It is never admitted.
Emulator: the login with a tampered token SUCCEEDS: the session opens and CURRENT_USER() reports the role the token named ('<role>@%'), i.e. the token is accepted without its signature being checked.
Why: this is an authentication bypass in the emulated data plane. Any check or application that relies on token binding to prove rds-db:connect authorization gets a false positive, and a negative test for a forged token passes when it must fail — exactly inverting the security property under test.
Fix: the IAM-auth path of the emulated MySQL listener — validate the presented token's signature, user, host/port and expiry before admitting the session, and refuse with MySQL 1045 otherwise.
Traces: rds.mysql-iam-db-auth-token-binding-and-validity-enforced.

Cause 5 of 7: the rds.allowed_extensions parameter is not enforced — CREATE EXTENSION of an excluded extension succeeds.
AWS: on RDS for PostgreSQL the DB parameter rds.allowed_extensions lists the extensions that may be installed; CREATE EXTENSION for an extension NOT in that list is refused by the engine with SQLSTATE 22023 and the message 'Extension "<name>" is not supported' plus the detail naming rds.allowed_extensions. With rds.allowed_extensions set to 'pgcrypto,vector', CREATE EXTENSION "hstore" must be refused. Fixed text: 'Extension "' ... '" is not supported'; variable part: the extension name from the statement.
Emulator: 'CREATE EXTENSION "hstore"' SUCCEEDS — the replay reports 'expected the engine to refuse 'CREATE EXTENSION "hstore"', but it succeeded'. The parameter is accepted and stored but has no effect on the engine.
Why: the parameter is advertised as the guardrail that keeps un-vetted extensions out of a managed instance. An emulator that ignores it lets a deployment install anything, so a configuration intended to be restrictive is validated as working when on real RDS the same deploy is refused.
Fix: the emulated PostgreSQL engine's CREATE EXTENSION path — consult the instance's effective rds.allowed_extensions parameter value and refuse an extension not listed, with PostgreSQL SQLSTATE 22023 and RDS's wording.
Traces: rds.postgres-create-extension-restricted-by-allowed-extensions.

Cause 6 of 7: SHOW PROCESSLIST reports an empty State for every session, so a sleeping session is never observable.
AWS: on a MySQL endpoint, SHOW [FULL] PROCESSLIST (and information_schema.PROCESSLIST) reports per-connection Id, User, Host, db, Command, Time, State and Info. A session executing SLEEP() shows Command 'Query' with State 'User sleep'; an idle one shows Command 'Sleep'. The State column is how a client identifies what another connection is doing — and it is the only way to pick the connection id to pass to KILL.
Emulator: every row's State comes back as the empty string: the replay polls the victim connection's PROCESSLIST state and reports "connection 2 never reached PROCESSLIST state 'User sleep'; last [('',)]".
Why: an operator or a tool that finds the blocking/sleeping session before issuing KILL (or CALL mysql.rds_kill) can never identify it, so the whole kill-a-session workflow is unreachable and any monitoring built on PROCESSLIST.State sees nothing.
Fix: the emulated MySQL engine's PROCESSLIST projection — populate State (and Command/Time/Info) from the session's actual execution state instead of emitting an empty string.
Traces: rds.rds-kill-ends-session-with-lost-connection-error.

Cause 7 of 7: the aurora-postgresql cluster+writer engine-truth flow never converges, so its constraint and lock refusals are never reached.
AWS: an aurora-postgresql cluster with a writer instance becomes available and then serves SQL on the cluster's writer endpoint, where a UNIQUE/CHECK violation is refused with the PostgreSQL SQLSTATE for that class (23505/23514) and a lock conflict with 55P03 under NOWAIT. Reaching those refusals requires the cluster and its writer to become usable first.
Emulator: the check spends its whole per-check budget before reaching any SQL assertion — the replay output is only 'Check timed out after 60.0s'. No refusal and no SQL result is observed, so the engine-truth contract is unmeasurable on aurora-postgresql.
Why: every client of an Aurora PostgreSQL cluster waits for the cluster+writer to be usable before it can do anything; a flow that never converges makes the whole engine unusable rather than merely wrong.
Fix: the aurora-postgresql cluster/writer bring-up — the cluster status transitions and the writer-endpoint listener that must accept a session once the cluster reports available. NOTE: open issue 20261108T215646Z-rds-the-aurora-postgresql-cluster-writer-engine-truth.md reports the same non-convergence for a different (180 s) budget and does not carry this check's replay key; the two should be closed together.
Traces: rds.deep-rds-1-rds-engine-truth-aurora-postgres-constraint-and-lock-violations-rejected.

## Evidence
Behavioral suite replayed against the emulator built from this checkout: 417/676 checks passed (61.7%), 196 mismatched, 63 errored. The failures below are the ones the replay reported neither ALREADY FILED (blocked by an unimplemented stub) nor OWNED by an open issue.

Cause 1 — rds.iam-db-auth-token-minted-for-another-user-is-refused:
  statement: "SELECT rolname FROM pg_roles WHERE rolname = ANY(%s) ORDER BY rolname" with a list parameter
  psycopg.errors.UndefinedTable: no such function: ANY
Cause 1 — rds.pg-dialect-numeric-timestamptz-jsonb-arrays-returning-recursive-cte:
  statement: "SELECT '2024-01-31 00:00:00+00'::timestamptz + interval '1 month'"
  psycopg.errors.UndefinedTable: no such column: interval
  (expected one row: 2024-02-29 00:00:00+00 — one month added to Jan 31 of a leap year clamps to Feb 29)
Cause 1 — rds.postgres-idle-in-transaction-session-timeout-terminates-session:
  statement: "SELECT state, EXTRACT(EPOCH FROM (now() - state_change)) FROM pg_stat_activity WHERE pid = %s"
  psycopg.errors.SyntaxError: syntax error at or near "FROM"
  'no such function' / 'no such column' are SQLite diagnostics, surfaced verbatim through the PostgreSQL error channel and classified as UndefinedTable (42P01).

Cause 2 — rds.deep-rds-1-rds-engine-truth-mysql-constraint-lock-and-charset-violations-rejected:
  AssertionError: the engine ACCEPTED a statement it must refuse: 'INSERT INTO <table> (id, body) VALUES (2, %s)' (U+1F600,)
  Expected: MySQL error 1366 'Incorrect string value: ...' for a 4-byte character in a non-utf8mb4 column, and no row written.

Cause 3 — rds.deep-rds-sql-truth-iam-token-past-expiry-rejected: the login WAS refused, but with the wrong text.
  expected refusal line: 'PAM authentication failed' (fixed text) for the login role
  got: 'connection failed: ... FATAL:  password authentication failed for user "<iam role>"'
  (host/port in the observed string are this environment's and are not part of the requirement)

Cause 4 — rds.mysql-iam-db-auth-token-binding-and-validity-enforced:
  AssertionError: expected the login to be refused, but it was admitted: [('<iam role>@%',)]
  i.e. a tampered token opened a session and CURRENT_USER() returned the role it named; expected MySQL 1045 Access denied.

Cause 5 — rds.postgres-create-extension-restricted-by-allowed-extensions (rds.allowed_extensions = 'pgcrypto,vector'):
  AssertionError: expected the engine to refuse 'CREATE EXTENSION "hstore"', but it succeeded
  Expected: SQLSTATE 22023, message 'Extension "hstore" is not supported'.

Cause 6 — rds.rds-kill-ends-session-with-lost-connection-error:
  AssertionError: connection 2 never reached PROCESSLIST state 'User sleep'; last [('',)]
  i.e. SHOW PROCESSLIST returned a row whose State member is the empty string for a session running SLEEP().

Cause 7 — rds.deep-rds-1-rds-engine-truth-aurora-postgres-constraint-and-lock-violations-rejected:
  'Check timed out after 60.0s' — no SQL assertion was reached, so none of the 23505/23514/55P03 refusals were exercised.

## Steps to Reproduce
1. Build and boot the emulator from this checkout.
2. Replay the behavioral suite (simparity-behavioral-aws-v1).
3. Cause 1: create a postgres (or aurora-postgresql) instance and, on its endpoint, run each of: "SELECT rolname FROM pg_roles WHERE rolname = ANY($1)" with an array parameter; "SELECT '2024-01-31 00:00:00+00'::timestamptz + interval '1 month'"; "SELECT state, EXTRACT(EPOCH FROM (now() - state_change)) FROM pg_stat_activity WHERE pid = $1". Each is refused with a SQLite-worded error instead of executing. Checks: rds.iam-db-auth-token-minted-for-another-user-is-refused, rds.pg-dialect-numeric-timestamptz-jsonb-arrays-returning-recursive-cte, rds.postgres-idle-in-transaction-session-timeout-terminates-session.
4. Cause 2: run rds.deep-rds-1-rds-engine-truth-mysql-constraint-lock-and-charset-violations-rejected — create a table whose text column is not utf8mb4 and INSERT U+1F600; the insert is accepted instead of refused with MySQL 1366.
5. Cause 3: run rds.deep-rds-sql-truth-iam-token-past-expiry-rejected — mint an auth token, backdate X-Amz-Date so X-Amz-Date+X-Amz-Expires is 15 minutes in the past, connect as the rds_iam role; the FATAL line says 'password authentication failed' instead of 'PAM authentication failed'.
6. Cause 4: run rds.mysql-iam-db-auth-token-binding-and-validity-enforced — tamper with a minted token and connect to the MySQL endpoint as the role it names; the session is admitted instead of refused with 1045.
7. Cause 5: run rds.postgres-create-extension-restricted-by-allowed-extensions — set rds.allowed_extensions to 'pgcrypto,vector' on the instance's parameter group, then CREATE EXTENSION "hstore"; it succeeds instead of being refused with 22023.
8. Cause 6: run rds.rds-kill-ends-session-with-lost-connection-error — open a second MySQL session running SELECT SLEEP(...), then SHOW PROCESSLIST from an admin session; the victim's State is the empty string instead of 'User sleep'.
9. Cause 7: run rds.deep-rds-1-rds-engine-truth-aurora-postgres-constraint-and-lock-violations-rejected — the aurora-postgresql cluster+writer never becomes usable within the per-check budget and no SQL assertion is reached.
