Issue-Source: trace-sweep-divergence
Issue-Trace: dynamodb-ssbs-traj-status-set-membership-filter
Issue-Step: task#6
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: dynamodb-ssbs-traj-status-set-membership-filter scan-filter-function-operand-count-and-type-rejected dynamodb-ddbkds-traj-table-arn-instead-of-name
Issue-Shape-Id: com.amazonaws.dynamodb#DynamoDB_20120810
Issue-Key: dynamodb|Scan|2xx->4xx|$.Count
Issue-Key: dynamodb|Scan|2xx->4xx|$.Items
Issue-Key: dynamodb|Scan|2xx->4xx|$.ScannedCount
Issue-Key: dynamodb|Scan|2xx->4xx|status
Issue-Key: dynamodb|DescribeKinesisStreamingDestination|4xx->4xx|$.message
Issue-Commit: 2986c932db5644b5e53751894a3faacc2d59c288

# dynamodb: 2 net-new causes — a valid Scan set-membership FilterExpression is refused, and the table-ARN resource-type refusal is reworded

> _Note: this issue was identified against the code base at commit `2986c932db5644b5e53751894a3faacc2d59c288`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
Scan

## Description
Two net-new divergences on dynamodb, both on how an addressed/filtered read is admitted: one valid request is refused outright, one refusal carries emulator-authored wording. 2 causes; both must be closed.

Cause 1 of 2: Scan with a set-membership FilterExpression is rejected with HTTP 400 instead of scanning (fixture dynamodb-ssbs-traj-status-set-membership-filter, step task#6).
AWS: Smithy: Scan.FilterExpression targets ConditionExpression (no constraints on the expression text); Scan output Items/Count/ScannedCount (Count and ScannedCount default 0); Scan's modeled errors are InternalServerError, InvalidEndpointException, ProvisionedThroughputExceededException, RequestLimitExceeded, ResourceNotFoundException and ThrottlingException — a well-formed FilterExpression is not one of them. Recorded behavior only for the expression grammar: the recording shows AWS answering HTTP 200 for a Scan whose FilterExpression tests one attribute for membership in a list of values, returning every item of the table that satisfies it (in the recording all scanned items match, so Count == ScannedCount == the table's item count and Items carries the full set). A set-membership test over a document path and a list of ExpressionAttributeValues is a supported DynamoDB condition-expression operand form, so it must be parsed and evaluated, not refused.
Emulator: answers HTTP 400 to the same Scan, so the response body carries no Items, no Count and no ScannedCount at all (replay records them as expected 5 / 5 / the five items, got None). The replay detail records only the status and the absent output members, so the refusal's error code and message text are not in the divergence record — but any 400 here is wrong: the request is valid and the only correct answer is a 200 scan result.
Why: a client that filters a Scan by set membership (the natural translation of a SQL-style IN over a status/enum attribute, and what the AWS SDK document-mapper emits for it) gets a ValidationException-shaped failure where AWS returns rows. Pagination and retry logic built on Count/ScannedCount/LastEvaluatedKey never runs, so the client concludes the data is unqueryable rather than retrying, and a second, structurally identical probe in the same family (scan-filter-function-operand-count-and-type-rejected#task#14) fails the same way.
Fix: Scan's FilterExpression handling — the condition-expression parser/evaluator shared by Scan and Query must accept the set-membership operand form (path tested against a parenthesised list of expression-attribute values) and evaluate it per item, instead of failing the parse and turning it into a 400; the handler must then serialize Items/Count/ScannedCount as usual.
Traces: dynamodb-ssbs-traj-status-set-membership-filter#task#6 (representative), scan-filter-function-operand-count-and-type-rejected#task#14.

Cause 2 of 2: the refusal for a TableName that is an ARN of the wrong resource type is emulator-authored (fixture dynamodb-ddbkds-traj-table-arn-instead-of-name, step task#6).
AWS: Smithy: DescribeKinesisStreamingDestination.TableName is required and targets TableArn (@length min 1 max 1024), so an ARN is an accepted shape for the member and the resource type inside it is checked by the service, not by a member-constraint. Recorded behavior only for the message: AWS answers HTTP 400 with a ValidationException whose message is "1 validation error detected: Invalid resource type in '<table-arn>'". Fixed text: "1 validation error detected: Invalid resource type in '" + the ARN + "'". Variable part: the ARN exactly as supplied in the request's TableName — nothing else (no account id, region or table name from the recording is part of the template).
Emulator: same status 400, but the message is the generic constraint-violation template: "1 validation error detected: Value '<table-arn>' at 'tableName' failed to satisfy constraint: Invalid resource type". It wraps the ARN in the "Value ... at '<member>' failed to satisfy constraint: ..." frame AWS uses for member-constraint violations and relegates the real diagnosis to the constraint slot.
Why: a client or test that matches on AWS's "Invalid resource type in '<arn>'" wording to tell "you passed an ARN for the wrong resource" apart from a member-pattern/length rejection sees a different sentence and mis-classifies the failure; the emulator's text also asserts a constraint on tableName that the model does not define.
Fix: the ARN-resolution path for operations whose TableName targets TableArn (here DescribeKinesisStreamingDestination's handler/validation) must emit the resource-type diagnosis in AWS's own frame rather than routing it through the generic member-constraint message builder.
Traces: dynamodb-ddbkds-traj-table-arn-instead-of-name#task#6.

Neither cause is covered by an open issue. The open dynamodb message-wording issues (20261110T232401Z-dynamodb-validation-and-not-found-refusal-messages-are.md, 20261110T232401Z-dynamodb-expression-diagnostics-4-net-new-causes-operand.md, 20261103T042714Z-dynamodb-data-plane-limits-are-unenforced-or-wrongly.md) and the open Scan issues (20261115T115757Z-dynamodb-scan-parallel-scan-segments-and-index-consumedcapacity.md, 20261112T161524Z-dynamodb-3-net-new-causes-scan-s-limit.md) carry neither of these replay keys — the replay reports both clusters as unowned.

## Evidence
Replay of simparity-wire-aws-v1 against the emulator (emulator_up=true, 4399 steps, 3991 passed, score 90.7%).

Cause 1 — divergence_key service=dynamodb|Scan|status=200->400|keys=$.Count,$.Items,$.ScannedCount,status; fixture dynamodb-ssbs-traj-status-set-membership-filter step task#6; expected_status 200, actual_status 400.
  expected: HTTP 200 with $.Count = 5, $.ScannedCount = 5 and $.Items = the five scanned items (status values suspended/inactive/active/active/pending).
  actual:   HTTP 400; $.Count, $.ScannedCount and $.Items all absent (None).
  cluster members (affected_count 2): dynamodb-ssbs-traj-status-set-membership-filter#task#6, scan-filter-function-operand-count-and-type-rejected#task#14.

Cause 2 — divergence_key service=dynamodb|DescribeKinesisStreamingDestination|status=400->400|keys=$.message; fixture dynamodb-ddbkds-traj-table-arn-instead-of-name step task#6; expected_status 400, actual_status 400 (only $.message diverges).
  expected $.message: "1 validation error detected: Invalid resource type in 'arn:aws:dynamodb:<region>:<account>:Table/<name>'"
  actual   $.message: "1 validation error detected: Value 'arn:aws:dynamodb:<region>:<account>:Table/<name>' at 'tableName' failed to satisfy constraint: Invalid resource type"
  (the recording's concrete ARN was arn:aws:dynamodb:us-west-2:000000000000:Table/sim2real-probe-ddbkds-arn-4d70957e-a; the region, account, and generated table name are per-run values, not part of the required text.)
  cluster members (affected_count 1): dynamodb-ddbkds-traj-table-arn-instead-of-name#task#6.

Model evidence: smithy_query(operation="Scan,DescribeKinesisStreamingDestination") on services/service-com-amazonaws-dynamodb-dynamodb-20120810 — Scan.FilterExpression -> ConditionExpression, output Items/Count/ScannedCount, errors list contains no expression-validation error; DescribeKinesisStreamingDestination.TableName required -> TableArn @length 1..1024 with no pattern.

## Steps to Reproduce
1. Clone the repo and boot the emulator built from it; replay the wire corpus (replay_wire_corpus) for trace set simparity-wire-aws-v1.
2. Cause 1: replay fixture dynamodb-ssbs-traj-status-set-membership-filter step task#6 — a Scan over the seeded 5-item table whose FilterExpression tests the `status` attribute for membership in a list of ExpressionAttributeValues. The emulator answers HTTP 400 with no Items/Count/ScannedCount; AWS answers HTTP 200 with Count=5, ScannedCount=5 and all five items. The same failure reproduces on scan-filter-function-operand-count-and-type-rejected step task#14.
3. Cause 2: replay fixture dynamodb-ddbkds-traj-table-arn-instead-of-name step task#6 — call DescribeKinesisStreamingDestination with TableName set to an ARN whose resource segment is `Table/<name>` (wrong resource type). Both AWS and the emulator return HTTP 400 ValidationException, but compare $.message: the emulator emits the "Value '<arn>' at 'tableName' failed to satisfy constraint: Invalid resource type" frame instead of AWS's "1 validation error detected: Invalid resource type in '<arn>'".
