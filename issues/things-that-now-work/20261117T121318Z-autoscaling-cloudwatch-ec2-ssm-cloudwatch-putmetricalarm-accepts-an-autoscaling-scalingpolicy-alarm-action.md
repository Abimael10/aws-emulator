Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: autoscaling__cloudwatch__ec2__ssm.autoscaling-action-arn-structure-and-region-validated-before-delivery
Issue-Key: autoscaling__cloudwatch__ec2__ssm.autoscaling-action-arn-structure-and-region-validated-before-delivery|_reject#28|AssertionError|raise AssertionError(f"expected the call to be rejected, but it succeeded and returned {result!r}")
Issue-Commit: e00df5b4e39e0b77a91b9413050affc17449d7b8

# cloudwatch: PutMetricAlarm accepts an autoscaling scalingPolicy alarm action whose relative id is malformed, and one naming a foreign region, instead of refusing with ValidationError

> _Note: this issue was identified against the code base at commit `e00df5b4e39e0b77a91b9413050affc17449d7b8`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
PutMetricAlarm

## Description
Cause 1 of 1: PutMetricAlarm performs no structural or region validation on an 'arn:aws:autoscaling:...:scalingPolicy:...' AlarmActions entry.
AWS: Smithy (cloudwatch 2010-08-01) models PutMetricAlarm$AlarmActions as a list of ResourceName (@length 1..1024) and declares only LimitExceededFault, so this refusal is recorded behavior only: AWS validates every action ARN's vendor + relative id and its region BEFORE storing the alarm and answers HTTP 400 with Error.Code ValidationError, creating no alarm. For the autoscaling vendor the message template is 'The vendor "autoscaling" cannot be used with relative id <relative-id>. Allowed relative IDs must be in the formatscalingPolicy:policy-id:autoScalingGroupName/group-friendly-name:policyName/policy-friendly-name' — the two sentences are fixed text (including AWS's missing space before 'scalingPolicy') and <relative-id> is the ARN's own relative-id portion taken from the request. For an otherwise well-formed policy ARN whose region segment is not the alarm's own region the message is 'Invalid region <action-region> specified. Only <alarm-region> is supported.' — fixed text with the action's region and the endpoint's region as variable parts. A well-formed policy ARN for the caller's own region is accepted and round-trips verbatim through DescribeAlarms.
Emulator: both malformed inputs are ACCEPTED — HTTP 200, content-type application/x-amz-json-1.0, empty body — so an alarm is created carrying an action ARN that can never be delivered to. Because the check stops at the first unexpected success, the foreign-region refusal in the same validation block is presumed broken the same way.
Why: the alarm is stored with an undeliverable action. The failure surfaces much later, at state-transition time, as a silently dropped scaling action rather than at write time where AWS reports it; infrastructure code that relies on the refusal to catch a truncated or cross-region policy ARN deploys a group that never scales. A sibling defect for EC2-vendor action ARNs is filed separately under cloudwatch__ec2.
Fix: PutMetricAlarm's action-ARN validation (the vendor/relative-id grammar per vendor and the region check), ahead of any alarm state write.
Traces: autoscaling__cloudwatch__ec2__ssm.autoscaling-action-arn-structure-and-region-validated-before-delivery.

## Evidence
Expected: PutMetricAlarm with AlarmActions=['arn:aws:autoscaling:<region>:<account>:scalingPolicy:<policy-id>:autoScalingGroupName/<group>'] (the policyName segment removed) refused HTTP 400 ValidationError 'The vendor "autoscaling" cannot be used with relative id scalingPolicy:<policy-id>:autoScalingGroupName/<group>. Allowed relative IDs must be in the formatscalingPolicy:policy-id:autoScalingGroupName/group-friendly-name:policyName/policy-friendly-name', with no alarm left behind.
Actual: 'AssertionError: expected the call to be rejected, but it succeeded and returned {"ResponseMetadata": {"HTTPStatusCode": 200, "HTTPHeaders": {"content-type": "application/x-amz-json-1.0", "content-length": "2", ...}}}'. The positive control (the group's own well-formed policy ARN) was accepted and round-tripped correctly.

## Steps to Reproduce
1. Create a VPC/subnet/launch template and an ASG with MinSize=MaxSize=DesiredCapacity=0; PutScalingPolicy(SimpleScaling) and keep the returned PolicyARN.
2. Positive control: PutMetricAlarm(Namespace='AWS/EC2', MetricName='CPUUtilization', Statistic='Average', Period=60, EvaluationPeriods=1, Threshold=80.0, ComparisonOperator='GreaterThanThreshold', AlarmActions=[PolicyARN]) — accepted, and DescribeAlarms returns the ARN verbatim.
3. Repeat with AlarmActions=[the same ARN truncated after 'autoScalingGroupName/<group>'] — expect 400 ValidationError with the vendor/relative-id message; the emulator returns 200.
4. Repeat with the full PolicyARN rewritten to a different region — expect 400 ValidationError 'Invalid region <foreign> specified. Only <region> is supported.'; confirm DescribeAlarms shows no alarm for either rejected name.
