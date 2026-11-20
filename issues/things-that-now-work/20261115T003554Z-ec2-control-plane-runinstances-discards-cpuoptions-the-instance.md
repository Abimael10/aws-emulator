Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ec2.cpuopts-guest-topology-differs-from-the-type-default ec2.modify-instance-attribute-immutable-field-rejected ec2.modify-instance-attribute-wrong-state-fails ec2.modify-instance-attribute-user-data-while-running-rejected ec2.op-contract-reset-instance-attribute ec2.network-interface-attach-detach-lifecycle ec2.op-contract-describe-ipam-resource-discoveries ec2.store-image-task-lifecycle-to-completion
Issue-Shape-Id: com.amazonaws.ec2#AmazonEC2
Issue-Key: ec2.cpuopts-guest-topology-differs-from-the-type-default|run#22|AssertionError|assert described["ht-off"]["CpuOptions"] == requested, (
Issue-Key: ec2.modify-instance-attribute-immutable-field-rejected|run#15|AssertionError|assert msg == expected, f"expected message {expected!r}, got {msg!r}"
Issue-Key: ec2.modify-instance-attribute-wrong-state-fails|run#9|AssertionError|assert msg == expected, f"expected message {expected!r}, got {msg!r}"
Issue-Key: ec2|ModifyInstanceAttribute|IncorrectInstanceState
Issue-Key: ec2.op-contract-reset-instance-attribute|run#16|AssertionError|assert message == f"The instance '{instance_id}' is not in the 'stopped' state.", (
Issue-Key: ec2|DetachNetworkInterface|OperationNotPermitted
Issue-Key: ec2.op-contract-describe-ipam-resource-discoveries|run#10|AssertionError|assert code == "InvalidIpamResourceDiscoveryId.NotFound", f"expected NotFound, got {code!r}"
Issue-Key: s3|HeadObject|404
Issue-Commit: a49cdab94fc41177458b6e53730040285f53d8d6

# EC2 control plane: RunInstances discards CpuOptions, the instance-attribute IncorrectInstanceState message is emulator prose, DetachNetworkInterface mis-cases 'networkCard', DescribeIpamResourceDiscoveries mis-classifies an absent id, and CreateStoreImageTask never writes the object

> _Note: this issue was identified against the code base at commit `a49cdab94fc41177458b6e53730040285f53d8d6`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
RunInstances

## Description
Five net-new control-plane root causes in EC2.

Cause 1 of 5: RunInstances accepts CpuOptions and then discards them — the created instance reads back the instance type's default topology.
AWS: Smithy models RunInstances input member CpuOptions of CpuOptionsRequest {CoreCount, ThreadsPerCore ("To disable multithreading for the instance, specify a value of 1. Otherwise, specify the default value of 2"), AmdSevSnp, NestedVirtualization} and the shape doc "Both the core count and threads per core must be specified in the request"; DescribeInstances returns CpuOptions {CoreCount, ThreadsPerCore} reflecting what was requested. A launch with CoreCount=C and ThreadsPerCore=1 therefore reads back CoreCount=C, ThreadsPerCore=1.
Emulator: an instance launched with CpuOptions {CoreCount: 1, ThreadsPerCore: 1} reads back {CoreCount: 1, ThreadsPerCore: 2} — the type default — so the requested value is silently replaced.
Why: ThreadsPerCore=1 is how customers halve per-core licence cost and get deterministic single-thread performance; a silently ignored request means the instance is billed and licensed on a topology the customer did not ask for, and no read-back reveals it.
Fix: the RunInstances handler's CpuOptions validation and the instance state that DescribeInstances serialises CpuOptions from.
Traces: ec2.cpuopts-guest-topology-differs-from-the-type-default.

Cause 2 of 5: the IncorrectInstanceState refusal for instance-attribute writes/resets carries emulator-authored prose instead of AWS's message.
AWS: Smithy models ModifyInstanceAttribute (InstanceId, Attribute of InstanceAttributeName, and the per-attribute members InstanceType, UserData, Kernel, Ramdisk, ...) and ResetInstanceAttribute. For a running instance AWS refuses an attribute that may only be changed while stopped with error code IncorrectInstanceState and the message exactly "The instance '<instance-id>' is not in the 'stopped' state." — the fixed text is "The instance '" ... "' is not in the 'stopped' state." and the only variable part is the instance id from the request. The message is recorded behavior (the EC2 model does not carry error shapes); the code and the member names are from the model.
Emulator: the code is right but the message is a different sentence that names the attribute: "The instance '<instance-id>' is not in a state from which it can be modified for the instanceType attribute." / "... for the userData attribute." / for ResetInstanceAttribute "... is not in a state from which it can be reset for the kernel attribute."
Why: clients and the AWS CLI/SDK error-handling code paths match on this message to tell "stop the instance first" apart from other IncorrectInstanceState cases; an emulator-authored sentence breaks that branch and any log-based alerting keyed on it.
Fix: ModifyInstanceAttribute's and ResetInstanceAttribute's validation/refusal message template.
Traces: ec2.modify-instance-attribute-immutable-field-rejected; ec2.modify-instance-attribute-wrong-state-fails; ec2.modify-instance-attribute-user-data-while-running-rejected; ec2.op-contract-reset-instance-attribute.

Cause 3 of 5: DetachNetworkInterface's refusal for the primary attachment mis-cases 'networkCard'.
AWS: Smithy models DetachNetworkInterface(AttachmentId required, Force). Detaching the device-index-0 attachment is refused with code OperationNotPermitted and the message exactly "The network interface at device index 0 and networkCard index 0 cannot be detached." — the fixed text includes the camel-cased 'networkCard'; the two indexes come from the attachment being detached (message recorded behavior only).
Emulator: the code is OperationNotPermitted but the message reads "The network interface at device index 0 and networkcard index 0 cannot be detached." — lower-case 'networkcard'.
Why: this is a character-exact message requirement; a client, test or runbook matching AWS's wording does not match the emulator's, so the primary-interface guard looks like an unknown failure.
Fix: the DetachNetworkInterface handler's refusal message for the primary attachment.
Traces: ec2.network-interface-attach-detach-lifecycle.

Cause 4 of 5: DescribeIpamResourceDiscoveries mis-classifies a well-formed but nonexistent resource-discovery id as malformed.
AWS: Smithy models DescribeIpamResourceDiscoveries(IpamResourceDiscoveryIds of ValueStringList, Filters, MaxResults range 5..1000). A syntactically valid id that does not exist is refused as a NotFound (InvalidIpamResourceDiscoveryId.NotFound); the Malformed classification is reserved for an id that does not match the ipam-res-disco- id form (recorded behavior only for the exact code string).
Emulator: the call is refused with InvalidIpamResourceDiscoveryId.Malformed for a well-formed id that simply does not exist.
Why: Malformed vs NotFound is the signal a client uses to decide "my input is wrong" vs "the resource is gone"; mis-classifying absence as malformation makes idempotent delete/reconcile loops treat a normal post-deletion read as a programming error.
Fix: the DescribeIpamResourceDiscoveries handler's id validation/lookup ordering.
Traces: ec2.op-contract-describe-ipam-resource-discoveries.

Cause 5 of 5: CreateStoreImageTask returns an ObjectKey but never writes the AMI object to the bucket, so the task never completes.
AWS: Smithy models CreateStoreImageTask(ImageId required, Bucket required, S3ObjectTags) -> CreateStoreImageTaskResult{ObjectKey}, and DescribeStoreImageTasks reports the task progressing to StoreTaskState 'Completed'. The object named by ObjectKey exists in the named bucket, so S3 HeadObject on it answers HTTP 200 (recorded behavior only for the object's existence).
Emulator: the task's ObjectKey is returned (e.g. '<image-id>.bin') but S3 HeadObject on Bucket/ObjectKey answers HTTP 404 Not Found (botocore ClientError '404' / 'Not Found') and the task never reaches Completed.
Why: store-image is an AMI backup/transfer mechanism; a client is told the store succeeded and is handed a key that resolves to nothing, so the restore path (CreateRestoreImageTask) has no object to read and the backup is silently lost.
Fix: the CreateStoreImageTask handler and the store-image task state — it must write the object into the target bucket and advance DescribeStoreImageTasks to Completed.
Traces: ec2.store-image-task-lifecycle-to-completion.

## Evidence
Cause 1 — Expected: DescribeInstances reports CpuOptions {'CoreCount': 1, 'ThreadsPerCore': 1} for an instance launched with that CpuOptions. Actual: AssertionError: DescribeInstances reports CpuOptions {'CoreCount': 1, 'ThreadsPerCore': 2} for the instance launched with {'CoreCount': 1, 'ThreadsPerCore': 1}.

Cause 2 — Expected code IncorrectInstanceState with message "The instance 'i-...' is not in the 'stopped' state." Actual: "The instance 'i-00000000000000001' is not in a state from which it can be modified for the instanceType attribute." / "... for the userData attribute." / (ResetInstanceAttribute) "The instance 'i-00000000000000001' is not in a state from which it can be reset for the kernel attribute."

Cause 3 — Expected OperationNotPermitted with message "The network interface at device index 0 and networkCard index 0 cannot be detached." Actual: AssertionError: DetachNetworkInterface on the primary attachment refusal message: 'The network interface at device index 0 and networkcard index 0 cannot be detached.'

Cause 4 — Expected a NotFound classification (InvalidIpamResourceDiscoveryId.NotFound) for a well-formed, absent id. Actual: AssertionError: expected NotFound, got 'InvalidIpamResourceDiscoveryId.Malformed'.

Cause 5 — Expected HTTP 200 from S3 HeadObject on the task's Bucket/ObjectKey and DescribeStoreImageTasks reaching StoreTaskState 'Completed'. Actual: botocore.exceptions.ClientError: An error occurred (404) when calling the HeadObject operation: Not Found.

## Steps to Reproduce
Cause 1: ec2.RunInstances(ImageId=<al2023>, InstanceType=<type with 2 threads/core, e.g. m5.large>, MinCount=1, MaxCount=1, CpuOptions={'CoreCount': 1, 'ThreadsPerCore': 1}); then ec2.DescribeInstances and read Instances[0].CpuOptions.

Cause 2: launch an instance, wait for 'running', then ec2.ModifyInstanceAttribute(InstanceId=<id>, InstanceType={'Value': 't3.small'}) and ec2.ModifyInstanceAttribute(InstanceId=<id>, UserData={'Value': <b64>}), and ec2.ResetInstanceAttribute(InstanceId=<id>, Attribute='kernel'); compare the Error.Message of each refusal with AWS's "The instance '<id>' is not in the 'stopped' state."

Cause 3: launch an instance, read its DeviceIndex-0 attachment id from DescribeInstances, then ec2.DetachNetworkInterface(AttachmentId=<that id>) and compare Error.Message character for character.

Cause 4: ec2.DescribeIpamResourceDiscoveries(IpamResourceDiscoveryIds=['ipam-res-disco-00000000000000000']) with a well-formed but nonexistent id and read Error.Code.

Cause 5: create an S3 bucket, ec2.CreateStoreImageTask(ImageId=<ami>, Bucket=<bucket>) and keep the returned ObjectKey; poll ec2.DescribeStoreImageTasks for StoreTaskState 'Completed' and call s3.HeadObject(Bucket=<bucket>, Key=<ObjectKey>).
