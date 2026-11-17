Issue-Source: trace-sweep-divergence
Issue-Trace: ec2-instance-explicit-defaults-read-back-by-attribute-and-credit-specification
Issue-Step: task#3
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: ec2-instance-explicit-defaults-read-back-by-attribute-and-credit-specification ec2-instance-tag-change-and-remove-last-then-terminated ec2-vfd-log-format-field-name-refusals ec2-vfd-s3-destination-mistakes-then-recovery
Issue-Shape-Id: com.amazonaws.ec2#AmazonEC2
Issue-Key: ec2|RunInstances|2xx->4xx|$.RunInstancesResponse
Issue-Key: ec2|DescribeFlowLogs|2xx->2xx|$.DescribeFlowLogsResponse.flowLogSet
Issue-Related: 20261029T235636Z-ec2-runinstances-refuses-the-recorded-elb-classic-setup.md
Issue-Commit: 81e21ca007588719de802b4dd67d83498ccb7823

# ec2 RunInstances refuses a launch AWS accepts and a refused CreateFlowLogs still persists a flow log

> _Note: this issue was identified against the code base at commit `81e21ca007588719de802b4dd67d83498ccb7823`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
RunInstances

## Description
2 causes, one shared fault: the emulator's accept/refuse decision disagrees with AWS - one valid request is refused, and one request AWS refuses leaves state behind.

Cause 1 of 2: RunInstances refuses a valid launch with HTTP 400
AWS: Smithy: RunInstances returns a Reservation; the recorded response is HTTP 200 with `RunInstancesResponse` carrying reservationId, ownerId, and one instancesSet item in instanceState {code 0, name 'pending'} for a t3.micro launched into the trace's subnet with a Name tag, an explicit default block-device/credit-specification request shape, and a default security group. The request names only members the model accepts, so AWS accepts it.
Emulator: HTTP 400 with no `RunInstancesResponse` body at all - the launch is refused outright.
Why: nothing downstream of a launch can be exercised: no instance exists, so attribute read-back, credit-specification reads, tagging and termination all fail. Any client that launches an instance with explicit defaults gets a hard 400 where AWS returns a pending instance. This cluster blocks two separate fixtures.
Fix: the RunInstances handler's request validation in the EC2 crate - stop rejecting the explicit-default launch shape, and return the Reservation projection with instanceState 'pending'.
Traces: ec2-instance-explicit-defaults-read-back-by-attribute-and-credit-specification#task#3 (same cluster also at ec2-instance-tag-change-and-remove-last-then-terminated#task#3)

Cause 2 of 2: a CreateFlowLogs request AWS refuses still leaves a flow log in state
AWS: recorded behavior - the fixture's CreateFlowLogs carries an invalid log-format field name (and, in the second affected fixture, a bad S3 destination); AWS refuses the request and creates nothing, so the subsequent DescribeFlowLogs is HTTP 200 with an empty `flowLogSet`.
Emulator: HTTP 200, but `flowLogSet` contains an item for the refused request - flowLogStatus 'ACTIVE', deliverLogsStatus 'SUCCESS', the rejected logFormat, and the rejected S3 logDestination.
Why: a refusal that is supposed to be atomic is not: the client is told the request failed, yet the VPC now has an ACTIVE flow log it never successfully created, delivering (or claiming to deliver) to a destination that was rejected. A retry with a corrected request then collides with a flow log that should not exist.
Fix: the CreateFlowLogs handler in the EC2 crate - validate the log format and destination before mutating state, and do not record a flow log for a refused request.
Traces: ec2-vfd-log-format-field-name-refusals#task#8 (same cluster also at ec2-vfd-s3-destination-mistakes-then-recovery#task#9)

Related open issues that do NOT own these steps (the replay reports no owner): 20261117T065015Z-ec2-vpc-flow-logs-refusals-and-describeflowlogs-output.md and 20261116T213556Z-ec2-s3-ec2-createflowlogs-the-invalidparameter-refusal-for-a-request.md.

## Evidence
Replay of trace set simparity-wire-aws-v1 (service ec2), verdict=mismatch:
- RunInstances, fixture ec2-instance-explicit-defaults-read-back-by-attribute-and-credit-specification step task#3 (cluster also covers ec2-instance-tag-change-and-remove-last-then-terminated#task#3): status: expected 200, got 400; $.RunInstancesResponse: missing (expected a Reservation with reservationId, ownerId '000000000000' and instancesSet.item {instanceState {code '0', name 'pending'}, instanceType 't3.micro', subnetId/vpcId of the trace's VPC, groupSet item groupName 'default', rootDeviceType 'ebs', ...}, got None)
- DescribeFlowLogs, fixture ec2-vfd-log-format-field-name-refusals step task#8 (cluster also covers ec2-vfd-s3-destination-mistakes-then-recovery#task#9): expected_status 200 / actual_status 200; $.DescribeFlowLogsResponse.flowLogSet: type_mismatch (expected '', got {'item': {'flowLogId': 'fl-...', 'flowLogStatus': 'ACTIVE', 'deliverLogsStatus': 'SUCCESS', 'resourceId': 'vpc-...', 'logDestinationType': 's3', 'logDestination': 'arn:aws:s3:::<bucket>/bad/', 'logFormat': '${srcaddr},${dstaddr}', ...}})
(Ids, bucket name and timestamps above are this recording's values, not part of the requirement.)

## Steps to Reproduce
Cause 1:
1. CreateVpc + CreateSubnet, then RunInstances with ImageId of a public AMI, InstanceType=t3.micro, MinCount/MaxCount=1, the subnet, a Name tag and the explicit-default block-device/credit-specification request shape used by the fixture.
2. The emulator answers HTTP 400 with no RunInstancesResponse; AWS answers HTTP 200 with the reservation and the instance in 'pending'.
Cause 2:
1. CreateFlowLogs for the VPC with a LogFormat naming a field AWS does not define (and/or an S3 LogDestination AWS rejects) -> the request is refused.
2. DescribeFlowLogs for that VPC -> the emulator lists an ACTIVE flow log carrying the rejected log format/destination; AWS returns an empty flowLogSet.
Or replay the fixture steps listed under Traces.
