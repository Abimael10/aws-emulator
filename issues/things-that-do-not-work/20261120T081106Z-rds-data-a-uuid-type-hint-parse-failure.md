Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: rds-data.rds-data-type-hint-is-required-for-engine-typed-parameters
Issue-Shape-Id: com.amazonaws.rdsdata#RdsDataService
Issue-Key: rds-data.rds-data-type-hint-is-required-for-engine-typed-parameters|_refused#4|AssertionError|assert _message(exc) == message, (
Issue-Commit: 8dd5be77b3d35a914e458e1c3403e40fd701f0e0

# rds-data: a UUID type-hint parse failure carries the emulator's 'Parse Error for UUID' sentence instead of AWS's 'Cannot parse UUID parameter: "<value>"'

> _Note: this issue was identified against the code base at commit `8dd5be77b3d35a914e458e1c3403e40fd701f0e0`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ExecuteStatement

## Description
1 net-new root cause in the Data API's typeHint parameter coercion.

Cause 1 of 1: the UUID type-hint parse failure is refused with an emulator-authored message.
AWS: ExecuteStatement with a parameter carrying typeHint=UUID whose stringValue cannot be parsed as a UUID is refused by the Data API itself, before the engine sees the statement, with HTTP 400 / DatabaseErrorException and the message 'Cannot parse UUID parameter: "<value>"'. The fixed text of that message is `Cannot parse UUID parameter: ` followed by the offending value from the request, in double quotes; it carries no `ERROR:` prefix and no `SQLState:` suffix, which is what distinguishes this Data-API-authored refusal class from an engine-authored one. Smithy: ExecuteStatement/BatchExecuteStatement both model DatabaseErrorException (client, httpError 400) and SqlParameter.typeHint (see API_SqlParameter); the model fixes the code and status, and the message text is recorded behavior only — the recording shows that sentence with the request's value interpolated.
Emulator: the code (DatabaseErrorException) and the status (400) match, but the message is the fixed sentence 'Parse Error for UUID: input contains invalid characters' — a different sentence, which drops the offending value entirely. This looks like the 'Parse Error for Date: input contains invalid characters' shape (the AWS wording for the DATE hint class, filed and fixed as 20261109T135215Z-rds-rds-data-rds-data-3-net-new-causes-date-type.md) generalised over the other hints; for UUID, AWS words it the other way round.
Why: the message is the only discriminator inside this refusal class — the code and status are shared with every engine-raised error. A client that matches on the message to tell 'this parameter value is not a UUID' from an engine type-mismatch matches nothing, and it never receives the offending value AWS echoes back, so it cannot name the bad parameter value to its user.
Why (scope): the same check also grades the JSON-hint unparseable leg ('invalid input syntax for type json: key must be a string at line 1 column 2') and the batched UUID leg ('Cannot parse UUID parameter: "not-a-uuid"' from BatchExecuteStatement); the check aborts on the UUID leg above before reaching them, so the fix must restore the Data-API-authored wording for every hint it emits and for the batch path too, not just the single-statement UUID leg.
Fix: the typeHint parameter coercion / refusal-message authoring in the rds-data ExecuteStatement (and BatchExecuteStatement) parameter-binding path, which authors the parse-failure sentence for each typeHint.
Traces: rds-data.rds-data-type-hint-is-required-for-engine-typed-parameters.

## Evidence
Cause 1: ExecuteStatement 'INSERT INTO <table> (id, u) VALUES (2, :p)' with parameters=[{name:'p', value:{stringValue:'nope-not-a-uuid'}, typeHint:'UUID'}] against a uuid column.
Expected (recorded AWS): HTTP 400, DatabaseErrorException, message 'Cannot parse UUID parameter: "nope-not-a-uuid"' — fixed text 'Cannot parse UUID parameter: ' plus the request's own stringValue in double quotes; no 'ERROR:' prefix, no 'SQLState:' part.
Actual (emulator): HTTP 400, DatabaseErrorException, message 'Parse Error for UUID: input contains invalid characters'.
The check's own report: AssertionError: a UUID hint on a value that cannot be parsed as one must carry the message 'Cannot parse UUID parameter: "nope-not-a-uuid"'; got 'Parse Error for UUID: input contains invalid characters'.
The code and status asserts passed before this one, so only the message diverged.

## Steps to Reproduce
1. Provision an Aurora Serverless v2 PostgreSQL cluster with the Data API (HTTP endpoint) enabled and a secret for its admin user.
2. ExecuteStatement: CREATE TABLE sim2real_behav_type_hints (id INT PRIMARY KEY, u uuid, j jsonb, i integer, ts timestamp, tstz timestamptz, d date, tm time, n numeric).
3. ExecuteStatement sql="INSERT INTO sim2real_behav_type_hints (id, u) VALUES (2, :p)", parameters=[{"name":"p","value":{"stringValue":"nope-not-a-uuid"},"typeHint":"UUID"}].
4. Read Error.Code, the HTTP status and Error.Message: the code is DatabaseErrorException and the status is 400 as AWS answers, but the message is 'Parse Error for UUID: input contains invalid characters' instead of 'Cannot parse UUID parameter: "nope-not-a-uuid"'.
5. Also exercise, after the fix, the JSON-hint unparseable leg (stringValue '{bad json}', typeHint JSON -> 'invalid input syntax for type json: key must be a string at line 1 column 2') and the same UUID hint through BatchExecuteStatement (stringValue 'not-a-uuid' -> 'Cannot parse UUID parameter: "not-a-uuid"'), which this check reaches once the leg above passes.
