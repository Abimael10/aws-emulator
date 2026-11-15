Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: dynamodb__kinesis__lambda.check-lambda-kinesis-putrecord-without-permission
Issue-Key: dynamodb__kinesis__lambda.check-lambda-kinesis-putrecord-without-permission|run#8|AssertionError|assert denied["payload"]["ok"] is False, (
Issue-Commit: 6689cf75cb3d8c161cd0df9fd5579d36d63368c7

# dynamodb__kinesis__lambda: a Lambda function's in-function Kinesis PutRecord is served although its execution role carries no kinesis:PutRecord grant

> _Note: this issue was identified against the code base at commit `6689cf75cb3d8c161cd0df9fd5579d36d63368c7`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
PutRecord

## Description
1 net-new root cause in the dynamodb__kinesis__lambda group.

Cause 1 of 1: a Kinesis data-plane call (PutRecord) made from inside a Lambda function with the credentials Lambda injects for its execution role is served successfully even though that role's identity-based policy contains no statement allowing kinesis:PutRecord. Failing check: dynamodb__kinesis__lambda.check-lambda-kinesis-putrecord-without-permission.

Not covered by an existing issue. The open issue 20261106T142545Z-dynamodb-kinesis-lambda-2-net-new-causes-in.md is named here for the fixer's benefit: its Cause 1 is the same defect class for DynamoDB GetItem/PutItem made from inside a function, and its Cause 2 concerns the Kinesis ESM poller. Neither cause carries this check's replay key, and neither names Kinesis PutRecord, so this cause is filed here. Also named for context: 20261105T111951Z-dynamodb-kinesis-lambda-the-lambda-python-runtime-has.md (no AWS SDK in the python runtime) — in THIS run the in-function SDK did reach Kinesis (PutRecord returned a SequenceNumber), so this is an authorization defect, not an SDK-availability one.

AWS: the credentials placed in a Lambda execution environment are an STS assumed-role session of the function's execution role, and every SDK request they sign is authorized against that role's identity-based policy before the operation runs. An action no statement allows is refused by implicit deny. Smithy: kinesis PutRecord models AccessDeniedException (errorKind client, httpError 400) among its errors, alongside InvalidArgumentException / ResourceNotFoundException / ProvisionedThroughputExceededException — none of which applies to an authorization failure. So the refusal is HTTP 400 with error code AccessDeniedException, and the message AWS words for an implicit identity-based deny is, with its fixed text exactly as given and its variable parts taken from the request and the emulator's state: `User: arn:aws:sts::<account-id>:assumed-role/<execution-role-name>/<function-name> is not authorized to perform: kinesis:PutRecord on resource: <stream-arn> because no identity-based policy allows the kinesis:PutRecord action`. Fixed text: `User: `, ` is not authorized to perform: `, ` on resource: `, ` because no identity-based policy allows the `, ` action`. Variable parts: <account-id>, <execution-role-name>, <function-name> (the role session name Lambda uses), <stream-arn> (the stream the request addresses) and the action string — never the literal values of one run. A refused PutRecord must also leave no record on the stream. Once the same role is given an Allow for kinesis:PutRecord on that stream ARN, the identical call must succeed and exactly one record must land, carrying the PartitionKey from the request.

Emulator: the call is served. The handler's put_record returns 200 with a SequenceNumber, so the invocation payload is `{"ok": true, "sequence_number": "..."}` where AWS gives `{"ok": false, "code": "AccessDeniedException", "error": <the implicit-deny message above>}`. The check fails at `AssertionError: Lambda succeeded putting a Kinesis record without IAM permission`. No AccessDeniedException is produced at all, and the record lands on the stream, so the subsequent assertions (denied call leaves the shard empty; the grant-then-retry step) never get a meaningful verdict.

Why: a client cannot build or test least privilege against this emulator. Every negative IAM scenario for in-function Kinesis access passes vacuously: a role intended to be read-only writes to the stream, an over-broad or regressed policy is indistinguishable from a correct one, and handler code that catches AccessDeniedException to alarm, fall back or fail closed never takes that branch. A policy change AWS would refuse ships silently.

Fix: the request-authorization path for in-function SDK calls — the component that resolves the execution-role session from the credentials injected into the function environment and evaluates that role's identity-based policy before the Kinesis operation handler runs (the same gate the DynamoDB path misses in the issue named above), plus the Kinesis error serializer that must emit the awsJson 400 AccessDeniedException shape with the implicit-deny message template.

Traces: dynamodb__kinesis__lambda.check-lambda-kinesis-putrecord-without-permission.

## Evidence
Check dynamodb__kinesis__lambda.check-lambda-kinesis-putrecord-without-permission (verdict: mismatch).

Setup (every call succeeded): a single-shard Kinesis stream created and waited to StreamStatus ACTIVE; an IAM role trusted by lambda.amazonaws.com whose ONLY inline statement is Allow on [logs:CreateLogGroup, logs:CreateLogStream, logs:PutLogEvents] for arn:aws:logs:*:*:* — no kinesis action anywhere; a python Lambda function created with that role and env STREAM=<stream name>, whose handler calls kinesis.put_record(StreamName=STREAM, Data=b"x", PartitionKey=event["pk"]) and returns {"ok": True, "sequence_number": ...} or, on ClientError, {"ok": False, "code": e.response["Error"]["Code"], "error": str(e)}.

Expected (AWS): the invocation with pk="denied" returns ok False, code exactly "AccessDeniedException" (HTTP 400), and error the implicit-deny message `An error occurred (AccessDeniedException) when calling the PutRecord operation: User: arn:aws:sts::<account-id>:assumed-role/<role-name>/<function-name> is not authorized to perform: kinesis:PutRecord on resource: <stream-arn> because no identity-based policy allows the kinesis:PutRecord action` (fixed text as quoted; the account id, role name, function name and stream ARN are per-run values from the request and state). GetRecords over the shard then yields []. After iam.PutRolePolicy adds Allow kinesis:PutRecord on <stream-arn> to the SAME role, the identical invocation (pk="granted") returns ok True and exactly one record is readable from the shard with PartitionKey "granted".

Actual (emulator): the unauthorized invocation returned ok True with a SequenceNumber — the record was accepted and written. The check aborted at check_dynamodb__kinesis__lambda_check_lambda_kinesis_putrecord_without_permission.py line 88, `assert denied["payload"]["ok"] is False` -> `AssertionError: Lambda succeeded putting a Kinesis record without IAM permission`. No non-2xx and no AccessDeniedException was produced on the path at any point.

## Steps to Reproduce
1. Boot the emulator. Create a single-shard Kinesis stream and wait until DescribeStream reports StreamStatus ACTIVE; note its StreamARN.
2. Create an IAM role trusted by lambda.amazonaws.com whose only inline policy allows logs:CreateLogGroup / logs:CreateLogStream / logs:PutLogEvents on arn:aws:logs:*:*: — grant NO kinesis action.
3. Create a python Lambda function with that role and environment STREAM=<stream name>. Its handler calls kinesis.put_record(StreamName=STREAM, Data=b"x", PartitionKey=event.get("pk", "pk1")) and returns {"ok": True, "sequence_number": resp["SequenceNumber"]}, or {"ok": False, "code": e.response["Error"]["Code"], "error": str(e)} on ClientError.
4. Invoke the function with {"pk": "denied"}. Expected: ok False, code "AccessDeniedException" (HTTP 400) and the implicit-deny message naming the assumed-role session, kinesis:PutRecord and the stream ARN. Observed: ok True with a SequenceNumber.
5. Read the shard with GetShardIterator(TRIM_HORIZON)+GetRecords. Expected: no records. Observed: the record written by the unauthorized call is present.
6. iam.PutRolePolicy on the SAME role adding Allow kinesis:PutRecord on the stream ARN, then invoke again with {"pk": "granted"}. Expected: ok True and exactly one record on the stream with PartitionKey "granted" — on the emulator this step cannot distinguish allow from deny, since two records are now present.
Replay: dynamodb__kinesis__lambda.check-lambda-kinesis-putrecord-without-permission.
