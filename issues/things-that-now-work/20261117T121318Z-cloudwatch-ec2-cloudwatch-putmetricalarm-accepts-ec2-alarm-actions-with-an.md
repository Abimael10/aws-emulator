Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudwatch__ec2.ec2-action-arn-vendor-relative-id-and-region-validated-ahead-of-dimension-check
Issue-Key: cloudwatch__ec2.ec2-action-arn-vendor-relative-id-and-region-validated-ahead-of-dimension-check|_reject#15|AssertionError|raise AssertionError(f"expected the call to be rejected, but it succeeded and returned {result!r}")
Issue-Commit: e00df5b4e39e0b77a91b9413050affc17449d7b8

# cloudwatch: PutMetricAlarm accepts EC2 alarm actions with an unknown vendor relative id or a foreign region instead of refusing them with ValidationError

> _Note: this issue was identified against the code base at commit `e00df5b4e39e0b77a91b9413050affc17449d7b8`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
PutMetricAlarm

## Description
Cause 1 of 1: PutMetricAlarm does not validate EC2-vendor action ARNs ('arn:aws:automate:...' and the SWF action form) against the allowed relative-id set, nor against the alarm's own region.
AWS: Smithy (cloudwatch 2010-08-01) models PutMetricAlarm$AlarmActions as a list of ResourceName (@length 1..1024) and declares only LimitExceededFault, so these refusals are recorded behavior only: AWS checks each action ARN's vendor and relative id, and its region, BEFORE storing the alarm, answering HTTP 400 Error.Code ValidationError and creating nothing. Three message templates, all with fixed text and request-supplied variable parts:
  - swf vendor, unknown relative id: 'The vendor "swf" can not be used with relative id <relative-id>. Allowed relative IDs are action/actions/AWS_EC2.InstanceId.Stop/1.0, action/actions/AWS_EC2.InstanceId.Terminate/1.0, action/actions/AWS_EC2.InstanceId.Recover/1.0, action/actions/AWS_EC2.InstanceId.Reboot/1.0' (note AWS's 'can not');
  - automate vendor, unknown relative id: 'The vendor "automate" cannot be used with relative id <relative-id>. Allowed relative IDs are ec2:stop, ec2:terminate, ec2:recover, ec2:reboot';
  - any vendor, foreign region: 'Invalid region <action-region> specified. Only <alarm-region> is supported.'
The well-formed SWF-form action 'arn:aws:swf:<region>:<account>:action/actions/AWS_EC2.InstanceId.Stop/1.0' is accepted and round-trips verbatim through DescribeAlarms. This validation happens AHEAD of the InstanceId-dimension check, so it fires even on a metric that already carries an InstanceId dimension.
Emulator: the malformed SWF-form action is ACCEPTED — HTTP 200, content-type application/x-amz-json-1.0, empty body — and the alarm is created. The two sibling refusals in the same block (unknown automate relative id, foreign region) are presumed broken the same way; the check stops at the first unexpected success.
Why: the alarm is stored with an action AWS would never have admitted, so the configured stop/terminate/recover/reboot never happens and the operator discovers it only when the alarm fires and nothing recovers the instance. A typo in the action verb is accepted silently instead of being reported at write time. The same missing validation for autoscaling-vendor actions is filed separately under autoscaling__cloudwatch__ec2__ssm.
Fix: PutMetricAlarm's action-ARN validation (per-vendor relative-id allow-lists plus the region check), evaluated before the dimension check and before any state write.
Traces: cloudwatch__ec2.ec2-action-arn-vendor-relative-id-and-region-validated-ahead-of-dimension-check.

## Evidence
Expected: PutMetricAlarm(Namespace='AWS/EC2', MetricName='CPUUtilization', Statistic='Average', Period=300, EvaluationPeriods=1, Threshold=10.0, ComparisonOperator='LessThanThreshold', Dimensions=[{InstanceId: <id>}], AlarmActions=['arn:aws:swf:<region>:<account>:action/actions/AWS_EC2.InstanceId.Pause/1.0']) refused HTTP 400 ValidationError 'The vendor "swf" can not be used with relative id action/actions/AWS_EC2.InstanceId.Pause/1.0. Allowed relative IDs are action/actions/AWS_EC2.InstanceId.Stop/1.0, action/actions/AWS_EC2.InstanceId.Terminate/1.0, action/actions/AWS_EC2.InstanceId.Recover/1.0, action/actions/AWS_EC2.InstanceId.Reboot/1.0', leaving no alarm.
Actual: 'AssertionError: expected the call to be rejected, but it succeeded and returned {"ResponseMetadata": {"HTTPStatusCode": 200, "HTTPHeaders": {"content-type": "application/x-amz-json-1.0", "content-length": "2", ...}}}'. The positive control (the .Stop/1.0 SWF-form action) was accepted and round-tripped verbatim.

## Steps to Reproduce
1. Positive control: PutMetricAlarm with Dimensions=[{Name:'InstanceId', Value:'i-feedfacefeedfacef'}] and AlarmActions=['arn:aws:swf:<region>:<account>:action/actions/AWS_EC2.InstanceId.Stop/1.0'] — accepted; DescribeAlarms returns the action verbatim.
2. Same call with AlarmActions=['arn:aws:swf:<region>:<account>:action/actions/AWS_EC2.InstanceId.Pause/1.0'] — expect 400 ValidationError with the swf vendor message; the emulator returns 200.
3. Same call with AlarmActions=['arn:aws:automate:<region>:ec2:pause'] — expect 400 ValidationError with the automate vendor message.
4. Same call with AlarmActions=['arn:aws:automate:<other-region>:ec2:stop'] — expect 400 ValidationError 'Invalid region <other-region> specified. Only <region> is supported.'
5. After each refusal, DescribeAlarms(AlarmNames=[the name]) must be empty.
