Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudformation__rds.cfn-rds-db-instance-monitoring-and-storage-properties-invalid-interval-rejected cloudformation__rds.deploy-cfn-rds-db-parameter-group-binlog-row-image-casing-preserved cloudformation__rds.deploy-cfn-rds-db-parameter-group-invalid-parameter-value-rejected-and-rolled-back
Issue-Key: cloudformation__rds.cfn-rds-db-instance-monitoring-and-storage-properties-invalid-interval-rejected|run#7|AssertionError|assert bad_status == "ROLLBACK_COMPLETE", (
Issue-Key: cloudformation__rds.deploy-cfn-rds-db-parameter-group-binlog-row-image-casing-preserved|run#8|AssertionError|assert status == "CREATE_COMPLETE", f"stack settled at {status!r}, expected CREATE_COMPLETE"
Issue-Key: cloudformation__rds.deploy-cfn-rds-db-parameter-group-invalid-parameter-value-rejected-and-rolled-back|run#11|AssertionError|assert got == _REJECTED, f"the ParamGroup CREATE_FAILED reason was {got!r}, expected {_REJECTED!r}"
Issue-Commit: cd00ce55523bfa99c95686848d372910b65cd86b

# cloudformation__rds: 3 net-new causes — an invalid MonitoringInterval reaches CREATE_COMPLETE, an uppercase binlog_row_image value rolls the stack back, and the handler-failure reason omits AWS's SDK Attempt Count trailer

> _Note: this issue was identified against the code base at commit `cd00ce55523bfa99c95686848d372910b65cd86b`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateDBInstance

## Description
Three independent defects in the CloudFormation-to-RDS provisioning path, all grounded in failing behavioral checks under the suite heading `### cloudformation__rds`. Three causes; all three must be closed before submitting.

=== Cause 1 of 3: AWS::RDS::DBInstance accepts a MonitoringInterval outside the allowed set and the stack reaches CREATE_COMPLETE ===
AWS: Smithy `CreateDBInstanceMessage.MonitoringInterval` (IntegerOptional) — RDS accepts only the values 0, 1, 5, 10, 15, 30 and 60 (the model documents the valid-values set; the recording confirms it). A value outside that set is refused with HTTP 400 and, through the CloudFormation registry handler, the DBInstance resource row goes CREATE_FAILED with ResourceStatusReason `Resource handler returned message: "Invalid monitoring interval, please enter a value in [0, 1, 5, 10, 15, 30, 60] (Service: Rds, Status Code: 400, Request ID: <id>) (SDK Attempt Count: <n>)" (RequestToken: <uuid>, HandlerErrorCode: InvalidRequest)` and the stack settles ROLLBACK_COMPLETE with no DB instance left behind. Fixed text: `Invalid monitoring interval, please enter a value in [0, 1, 5, 10, 15, 30, 60]`; the allowed-value list is the rule itself, and the Request ID / attempt count / RequestToken are per-run values.
Emulator: the same template with MonitoringInterval outside that set is accepted. The stack settles `CREATE_COMPLETE` with rows `[('MonitoringRole','CREATE_COMPLETE'), ('DBInstance','CREATE_COMPLETE'), (<stack>,'CREATE_COMPLETE')]` — no refusal is produced at all, so there is no CREATE_FAILED reason to compare.
Why: a client relying on AWS's behavior gets a stack that deploys an instance AWS would never create. Enhanced-Monitoring validation is silently absent, so templates that are invalid in AWS pass in the emulator and the negative half of every monitoring-interval test (and any rollback-driven cleanup logic) never runs.
Fix: the MonitoringInterval validation in the CreateDBInstance handler's input validation (it must refuse before any state is created), so the CloudFormation AWS::RDS::DBInstance resource surfaces that refusal as CREATE_FAILED and the stack rolls back.
Traces: cloudformation__rds.cfn-rds-db-instance-monitoring-and-storage-properties-invalid-interval-rejected.

=== Cause 2 of 3: AWS::RDS::DBParameterGroup with Parameters {binlog_row_image: FULL} rolls the stack back instead of reaching CREATE_COMPLETE ===
AWS: Smithy `ModifyDBParameterGroupMessage.Parameters` / `Parameter.ParameterValue` carry no case constraint. RDS accepts the `binlog_row_image` value in any casing and stores it verbatim: a `AWS::RDS::DBParameterGroup` of Family `mysql8.0` declaring `Parameters: {binlog_row_image: FULL}` reaches CREATE_COMPLETE, `Ref` returns the declared DBParameterGroupName, and DescribeDBParameters with `Source=user` lists the parameter with `ParameterValue` exactly as declared (`FULL`, uppercase preserved). Case-insensitive acceptance plus case-preserving storage is the rule.
Emulator: the stack settles `ROLLBACK_COMPLETE`, i.e. the uppercase value is refused as if it were outside the allowed set, so the group is never created and nothing can be read back.
Why: a client relying on AWS's behavior cannot deploy a perfectly legal parameter group through CloudFormation; the deploy fails outright rather than returning a slightly different value. Any stack that sets an engine parameter in non-lowercase form is undeployable.
Fix: the allowed-value check in the DBParameterGroup parameter validation must compare case-insensitively while the stored value keeps the request's casing (the same defect on the direct RDS surface is tracked under the `rds` service by open issue 20261115T181413Z-rds-3-net-new-behavioral-causes-every-db.md, which does not cover this CloudFormation check).
Traces: cloudformation__rds.deploy-cfn-rds-db-parameter-group-binlog-row-image-casing-preserved.

=== Cause 3 of 3: the CloudFormation resource-handler failure reason omits AWS's trailing "(SDK Attempt Count: <n>)" ===
AWS: when an RDS registry handler fails, CloudFormation records the reason as `Resource handler returned message: "<RDS message> (Service: Rds, Status Code: <status>, Request ID: <id>) (SDK Attempt Count: <n>)" (RequestToken: <uuid>, HandlerErrorCode: <code>)`. The `(SDK Attempt Count: <n>)` group is fixed text that always follows the `(Service: …, Status Code: …, Request ID: …)` group; only `<n>` and `<id>` are per-run values. Recorded for a DBParameterGroup declaring an out-of-set `binlog_row_image` value: HandlerErrorCode `InvalidRequest`, inner RDS message `Invalid parameter value: <value> for: <parameter name> allowed values are: full,minimal,noblob` (fixed text `Invalid parameter value: `, ` for: `, ` allowed values are: ` with the value and parameter name taken from the request and the allowed set from the engine's parameter definition).
Emulator: the reason ends at `… Request ID: <id>)` — the `(SDK Attempt Count: <n>)` group is missing entirely. Observed reason: `('Invalid parameter value: fully for: binlog_row_image allowed values are: full,minimal,noblob (Service: Rds, Status Code: 400, Request ID: <id>)', 'InvalidRequest')` where AWS produces `('… Request ID: <id>) (SDK Attempt Count: <n>)', 'InvalidRequest')`.
Why: a client or test that parses the handler reason to recover the originating service call's envelope (the standard AWS SDK error-message shape) sees a truncated envelope and either fails to match or mis-reports the failure. The refusal itself is right; its reported shape is not.
Fix: the resource-handler failure-reason serializer in the CloudFormation provisioning path, which composes the `Resource handler returned message: "…" (RequestToken: …, HandlerErrorCode: …)` string.
Traces: cloudformation__rds.deploy-cfn-rds-db-parameter-group-invalid-parameter-value-rejected-and-rolled-back.

## Evidence
Cause 1 — cloudformation__rds.cfn-rds-db-instance-monitoring-and-storage-properties-invalid-interval-rejected [mismatch]:
  Expected: stack ROLLBACK_COMPLETE, DBInstance CREATE_FAILED with reason message 'Invalid monitoring interval, please enter a value in [0, 1, 5, 10, 15, 30, 60] (Service: Rds, Status Code: 400, Request ID: <id>) (SDK Attempt Count: <n>)' / HandlerErrorCode 'InvalidRequest'
  Actual:   AssertionError: invalid-interval stack settled at 'CREATE_COMPLETE': [('MonitoringRole', 'CREATE_COMPLETE'), ('DBInstance', 'CREATE_COMPLETE'), ('sim2real-cfnrds-badmonstk-8bfaaf9ae262', 'CREATE_COMPLETE')]
  (template declared MonitoringInterval = 7, which is not one of 0, 1, 5, 10, 15, 30, 60)

Cause 2 — cloudformation__rds.deploy-cfn-rds-db-parameter-group-binlog-row-image-casing-preserved [mismatch]:
  Expected: CREATE_COMPLETE, then DescribeDBParameters(Source='user') lists binlog_row_image with ParameterValue 'FULL'
  Actual:   AssertionError: stack settled at 'ROLLBACK_COMPLETE', expected CREATE_COMPLETE
  (template: AWS::RDS::DBParameterGroup, Family 'mysql8.0', Parameters {binlog_row_image: 'FULL'})

Cause 3 — cloudformation__rds.deploy-cfn-rds-db-parameter-group-invalid-parameter-value-rejected-and-rolled-back [mismatch]:
  Expected (request-id and attempt-count masked): ('Invalid parameter value: fully for: binlog_row_image allowed values are: full,minimal,noblob (Service: Rds, Status Code: 400, Request ID: <id>) (SDK Attempt Count: <n>)', 'InvalidRequest')
  Actual:   ('Invalid parameter value: fully for: binlog_row_image allowed values are: full,minimal,noblob (Service: Rds, Status Code: 400, Request ID: <id>)', 'InvalidRequest')
  Divergence: the fixed trailing group '(SDK Attempt Count: <n>)' is absent. The status, code and inner message match.

## Steps to Reproduce
Replay the three behavioral checks listed in trace_ids against the booted emulator.

Cause 1: CreateStack with a template containing an AWS::IAM::Role for Enhanced Monitoring and an AWS::RDS::DBInstance whose Properties set MonitoringInterval: 7 and MonitoringRoleArn: Fn::GetAtt [MonitoringRole, Arn]. Poll DescribeStacks until terminal. Expect ROLLBACK_COMPLETE and a DBInstance CREATE_FAILED event; the emulator returns CREATE_COMPLETE. Control: the same template with MonitoringInterval: 60 must deploy and DescribeDBInstances must echo MonitoringInterval 60 and the stack's role ARN.

Cause 2: CreateStack with a template whose only resource is AWS::RDS::DBParameterGroup { DBParameterGroupName: <name>, Family: 'mysql8.0', Description: '…', Parameters: { binlog_row_image: 'FULL' } }. Poll DescribeStacks until terminal. Expect CREATE_COMPLETE, Ref == <name>, and DescribeDBParameters(DBParameterGroupName=<name>, Source='user') listing binlog_row_image with ParameterValue 'FULL'; the emulator returns ROLLBACK_COMPLETE.

Cause 3: CreateStack with the same parameter-group template but Parameters { binlog_row_image: 'fully' }. Poll until ROLLBACK_COMPLETE, then DescribeStackEvents and take the single CREATE_FAILED row for logical id 'ParamGroup'. Parse its ResourceStatusReason with ^Resource handler returned message: "(?P<message>.*)" \(RequestToken: [0-9a-f-]+, HandlerErrorCode: (?P<code>[A-Za-z]+)\)$ and mask 'Request ID: <id>' / 'SDK Attempt Count: <n>'. The masked message must end with '(SDK Attempt Count: <n>)'; the emulator's ends at 'Request ID: <id>)'.
