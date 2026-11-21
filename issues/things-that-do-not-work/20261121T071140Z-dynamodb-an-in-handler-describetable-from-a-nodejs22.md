Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: dynamodb.awsjson-epoch-timestamp-survives-two-deserializers
Issue-Shape-Id: com.amazonaws.dynamodb#DynamoDB_20120810
Issue-Key: dynamodb.awsjson-epoch-timestamp-survives-two-deserializers|run#60|TimeoutError|raise TimeoutError("the JS consumer's role taking effect: the run budget ended first; not graded")
Issue-Commit: 9c4e35a7eb2cfc44174bf14b75d4647ba0e4b17d

# dynamodb: an in-handler DescribeTable from a nodejs22.x Lambda using the function role never succeeds, so the awsJson1_0 epoch-timestamp round trip through the JS v3 deserializer reaches no verdict

> _Note: this issue was identified against the code base at commit `9c4e35a7eb2cfc44174bf14b75d4647ba0e4b17d`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeTable

## Description
1 cause (Cause 1 of 1).

Cause 1 of 1: a `nodejs22.x` Lambda function whose execution role grants `dynamodb:DescribeTable` on the table never gets a successful DescribeTable answer from inside the handler. The handler itself runs and returns (the check asserts `FunctionError is None` before this point and that assertion passes), but the payload's `real.ok` stays false for the whole bounded 180 s retry window, so the check's grading of the timestamp round trip never happens.
Failing check: dynamodb.awsjson-epoch-timestamp-survives-two-deserializers.

AWS: Smithy (dynamodb-2012-08-10): `DescribeTable` takes `TableName` (required) and answers `Table: TableDescription`; its modeled errors are `ResourceNotFoundException` (400), `InvalidEndpointException` (421) and `InternalServerError` (500). `TableDescription.CreationDateTime` is a plain timestamp member with no `@timestampFormat` override, so under awsJson1_0 it goes on the wire as epoch seconds (a JSON number) and the JS v3 SDK deserializes it into a `Date`. Recorded behavior for the rest: within seconds of the role's propagation, a `DescribeTableCommand` issued from inside a function whose role allows `dynamodb:DescribeTable` on that table ARN is answered 200 with `Table.TableName` equal to the requested table, `Table.TableStatus == "ACTIVE"`, and `CreationDateTime` landing on the same instant botocore's `create_table` reported (within 1 ms) — never a 1970 instant. A call the role does not allow is refused 400 `AccessDeniedException` with the `User: <principal-arn> is not authorized to perform: dynamodb:DescribeTable on resource: <table-arn> because no identity-based policy allows the dynamodb:DescribeTable action` message class (fixed text apart from the principal and resource ARNs taken from the request and state); a missing table is refused `ResourceNotFoundException` with `Requested resource not found: Table: <name> not found`.

Emulator: the in-handler call never succeeds and never yields a graded refusal either. The handler returns `real.ok == false` on every one of the retries spread over the check's 180 s window (`lam.invoke` succeeds each time, `FunctionError` is absent), and the check ends with `TimeoutError: the JS consumer's role taking effect: the run budget ended first; not graded`. The same handler's loopback control arms (the identical JS deserializer reading emulator-shaped bodies from a 127.0.0.1 server inside the sandbox) are never reached, so the whole awsJson1_0 epoch-encoding contract for `CreationDateTime` goes unobserved. Hypothesis: either the execution role's `dynamodb:DescribeTable` grant never becomes effective for calls originating inside the function sandbox, or the bundled `@aws-sdk/client-dynamodb` in the Node runtime cannot reach the emulator's dynamodb endpoint from inside the sandbox; the replay output carries the handler payload only as `real.ok != true`, so it does not distinguish the two.
Open issues that look related but do not carry this check's replay key (the report lists this check with no owner), so they do not cover it: 20261111T040849Z-lambda-s3-the-lambda-node-js-runtime-s-bundled-sdk.md and 20261111T081112Z-lambda-managed-runtimes-are-stub-interpreters-10-node.md.

Why: the ordinary pattern of a Lambda function reading DynamoDB with its own execution role cannot be exercised. A client gets neither a 200 with the table description nor an attributable refusal — the call simply never comes good, so a handler retries until its timeout. Any consumer that depends on `CreationDateTime` arriving as an epoch-seconds number (so that a JS v3 client materializes a `Date` on the right instant) cannot be validated at all.

Fix: the in-sandbox AWS call path of the Lambda execution environment for the Node.js runtime — the resolution of the dynamodb endpoint from inside the sandbox and the authorization of the signed in-handler request against the function's execution role — plus the dynamodb `DescribeTable` handler/serializer that must answer it 200 with `TableDescription.CreationDateTime` as an epoch-seconds number (or refuse it with the modeled `AccessDeniedException`/`ResourceNotFoundException` message class).

Traces: failing check dynamodb.awsjson-epoch-timestamp-survives-two-deserializers (verdict `error`, detail `TimeoutError: the JS consumer's role taking effect: the run budget ended first; not graded`).

## Evidence
Behavioral suite replay, service heading `### dynamodb` (1211/1343 passed):

  #### [error] dynamodb.awsjson-epoch-timestamp-survives-two-deserializers
  Error: File "run.py", line 456, in <lambda>
  body = calls.in_phase(calls.BODY, lambda: fn(ctx))
  File "check_dynamodb_awsjson_epoch_timestamp_survives_two_deserializers.py", line 352, in run
  raise TimeoutError("the JS consumer's role taking effect: the run budget ended first; not graded")
  TimeoutError: the JS consumer's role taking effect: the run budget ended first; not graded

The check reached that raise only after:
  - `create_function(Runtime="nodejs22.x", Role=<role with dynamodb:DescribeTable on the table>)` succeeded (its bounded retry over `InvalidParameterValueException`/'The role defined for the function cannot be assumed by Lambda.' completed), and
  - `get_function_configuration` reported `State == "Active"`, and
  - every `lam.invoke(..., LogType="Tail")` in the retry loop returned with no `FunctionError` (that assertion precedes the raise), i.e. the handler ran and answered.

Expected (handler payload `real`): `{ok: true, isDate: true, tableName: <table>, tableStatus: "ACTIVE", epochMs: <within 1 ms of create_table's CreationDateTime>, iso: not starting with "1970"}` — i.e. `DescribeTable` answers the in-sandbox client and `CreationDateTime` arrives as an epoch-seconds number that the JS v3 SDK turns into the right `Date`.

Actual: `real.ok` is false on every retry across the loop's 180 s window, so the payload carries `{ok: false, name, message}` instead; the loopback control arms (`epochFloat`, `numericString`) are never asserted and the timestamp contract is never observed.

No per-run value is part of this finding: the table name, function name, role name and the recorded instant vary per run; the requirement is a 200 answer carrying the requested table with an epoch-seconds `CreationDateTime`, or one of the modeled refusals with its message class.

## Steps to Reproduce
1. `create_table` a table (hash key S) and wait for ACTIVE; note `TableDescription.CreationDateTime`.
2. Create an IAM role trusted by `lambda.amazonaws.com` with an inline policy allowing `dynamodb:DescribeTable` on `arn:aws:dynamodb:<region>:<account>:table/<table>` plus the three logs actions.
3. `create_function(Runtime="nodejs22.x", Handler="index.handler", Role=<that role>)` with an ESM handler that does `new DynamoDBClient({region}).send(new DescribeTableCommand({TableName: <table>}))` and returns `{ok: true, isDate: d instanceof Date, epochMs: d.getTime(), tableName, tableStatus}` on success or `{ok: false, name, message}` on failure. Wait for `State == "Active"`.
4. `invoke` the function with `{region, table}`.
   Expected: payload `real.ok == true`, `isDate == true`, `tableStatus == "ACTIVE"`, `epochMs` within 1 ms of the `CreationDateTime` from step 1.
   Observed: `real.ok` is false on every invoke, repeated over a 180 s retry window; the invoke itself carries no `FunctionError`.
5. Re-replay dynamodb.awsjson-epoch-timestamp-survives-two-deserializers to confirm the fix (it then also grades the loopback epoch-encoding control arms).
