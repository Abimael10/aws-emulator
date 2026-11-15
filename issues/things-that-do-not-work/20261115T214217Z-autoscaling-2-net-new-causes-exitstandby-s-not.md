Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: autoscaling.instance-scoped-ops-reject-foreign-instances autoscaling.op-contract-exit-standby
Issue-Shape-Id: com.amazonaws.autoscaling#AutoScaling_2011_01_01
Issue-Key: autoscaling.instance-scoped-ops-reject-foreign-instances|run#30|AssertionError|assert error.get("Message") == not_in_standby, (
Issue-Key: autoscaling.op-contract-exit-standby|run#26|AssertionError|assert message == expected_message, f"expected message {expected_message!r}, got {message!r}"
Issue-Commit: 4c0c5b4e226e70c69e19bf0a05778a3d402e5899

# autoscaling: 2 net-new causes — ExitStandby's not-in-Standby refusal is re-worded, and group member instance ids are minted from a dense counter so a fabricated id collides with a live member

> _Note: this issue was identified against the code base at commit `4c0c5b4e226e70c69e19bf0a05778a3d402e5899`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ExitStandby

## Description
Two net-new causes, both on the autoscaling service, both reached through ExitStandby. The autoscaling Smithy model (services/service-com-amazonaws-autoscaling-autoscaling-2011-01-01/smithy-model/com-amazonaws-autoscaling-autoscaling-2011-01-01.json) declares only ResourceContentionFault for ExitStandby, so the argument refusal class (query-protocol Sender/ValidationError, HTTP 400) is grounded in recorded behavior only; the error CODE matched in both checks and only the message/refusal class diverges.

Cause 1 of 2 — ExitStandby's refusal for a group member that is not in Standby carries emulator-authored wording.
AWS: recorded behavior only (Smithy: ExitStandby input AutoScalingGroupName required XmlStringMaxLen255, InstanceIds list; modeled errors: ResourceContentionFault only — the argument refusal is the protocol-common ValidationError). Naming an instance that IS a member of the group but is not in the Standby lifecycle state is refused HTTP 400 with Error.Code 'ValidationError' and Message 'The instance <instance-id> is not in Standby.' The fixed text is 'The instance ' / ' is not in Standby.'; the only variable part is the instance id taken from the request.
Emulator: HTTP 400 ValidationError with 'Instance <instance-id> is in <lifecycle-state> state, not in Standby state, and cannot be moved.' — a different sentence, with a different subject form ('Instance' rather than 'The instance') and an extra variable part (the lifecycle state the emulator holds) that AWS's sentence does not carry.
Why: a client that branches on AWS's refusal text (or an operator reading it) cannot recognise the 'not in Standby' refusal class here; the two sentences share no fixed text, so any string match fails and the condition is misattributed.
Fix: the standby refusal wording — the StandbyRefusal::WrongState arm of StandbyRefusal::message, which both ExitStandby and EnterStandby refuse through.
Traces: autoscaling.instance-scoped-ops-reject-foreign-instances (ExitStandby on an InService member of the named group), and the same wording is what autoscaling.op-contract-exit-standby received.

Cause 2 of 2 — an Auto Scaling group's member instance ids are minted from a dense counter starting at 1, so an id a client picks as 'this instance never existed' resolves to a live member and ExitStandby answers the wrong refusal class.
AWS: recorded behavior only. An EC2 instance id is 'i-' plus 17 hex characters allocated unpredictably, so an id a caller fabricates names no instance of the account. ExitStandby naming an id that is not a member of the group it names is refused HTTP 400 ValidationError with Message 'The instance <instance-id> is not part of Auto Scaling group <group-name>.' — fixed text 'The instance ' / ' is not part of Auto Scaling group ' / '.', with the instance id and the group name both taken from the request. That is a DIFFERENT refusal class from Cause 1 and takes precedence over it: membership is resolved before lifecycle state.
Emulator: the instances an Auto Scaling capacity change launches are given ids from a dense, zero-padded sequence — the first group member of the store is i-00000000000000001, the second i-00000000000000002 (both observed, in two independent checks). A request naming a 'nonexistent' id in that low range therefore finds a live InService member, and the call is refused with the Cause 1 lifecycle sentence ('Instance i-00000000000000001 is in InService state, not in Standby state, and cannot be moved.') instead of the not-part-of-group sentence. The crate already holds the correct text (state.rs the_instance_not_in_group_message / NOT_PART_OF_GROUP_CLAUSE) and the StandbyRefusal::NotInGroup arm that uses it; that arm is simply never reached, because the id is a real member.
Why: a client cannot distinguish 'unknown instance' from 'wrong state', so negative-path handling and any test that fabricates an instance id to prove a non-member is rejected gets the wrong error. The predictable, dense id space is itself a divergence: ids are guessable across runs and collide with ids a caller invents, and nothing that AWS allocates lands in that range.
Fix: the EC2 instance-id allocator the Auto Scaling launch path uses — the launch in src/state/ec2_instances.rs writes into the account's EC2 store, so the id minting in that store (not ExitStandby) is the component; ids must be drawn from the full 17-hex-character space rather than a counter from 1.
Traces: autoscaling.op-contract-exit-standby (fabricated id i-00000000000000001 collides with the group's own first member); the sibling ids i-00000000000000001 / i-00000000000000002 are also visible in autoscaling.instance-scoped-ops-reject-foreign-instances.

Related but not covering: open issues 20261111T102954Z-autoscaling-policy-and-instance-operations-miss-or-re.md (SetInstanceProtection / TerminateInstanceInAutoScalingGroup membership wording) and 20261110T100847Z-autoscaling-refusal-fidelity-wrong-codes-and-emulator-authored.md own other autoscaling refusal text; neither carries ExitStandby's not-in-Standby sentence or the instance-id allocation, and the replay reported no owner for either check.

## Evidence
Replay of simparity-behavioral-aws-v1 against the emulator built from this checkout. Both checks are listed under '### autoscaling'; neither was marked ALREADY FILED or OWNED by.

Cause 1 — autoscaling.instance-scoped-ops-reject-foreign-instances [mismatch]:
  check_autoscaling_instance_scoped_ops_reject_foreign_instances.py line 346:
  AssertionError: ExitStandby on an InService instance: expected message 'The instance i-00000000000000002 is not in Standby.', got 'Instance i-00000000000000002 is in InService state, not in Standby state, and cannot be moved.'
  (Error.Code was asserted ValidationError first and matched; only the message diverged. The same check's EnterStandby and SetInstanceProtection non-member assertions passed, so 'The instance ... is not part of Auto Scaling group ...' is already produced on those paths.)

Cause 2 — autoscaling.op-contract-exit-standby [mismatch]:
  check_autoscaling_op_contract_exit_standby.py line 79:
  AssertionError: expected message 'The instance i-00000000000000001 is not part of Auto Scaling group sim2real-behav-exitstandby-<suffix>.', got 'Instance i-00000000000000001 is in InService state, not in Standby state, and cannot be moved.'
  The check fabricates the id i-00000000000000001 as an instance that was never part of the group; the emulator's reply states that very id is an InService member of the group, which is only reachable through the WrongState arm of src/state/standby.rs (it requires group.instances to contain the id). The group had been scaled to DesiredCapacity 2 by the preceding successful ExitStandby, so the colliding member is one the emulator itself launched.
  Note the instance-id values above (i-00000000000000001, i-00000000000000002) are the emulator's allocation, not per-run noise: two independent checks saw the first and second ids of the store.

## Steps to Reproduce
1. Cause 1: create an Auto Scaling group from a launch template with MinSize 1 / MaxSize 1 / DesiredCapacity 1 and wait for its instance to reach LifecycleState InService. Call ExitStandby(AutoScalingGroupName=<that group>, InstanceIds=[<that instance>]). Expect HTTP 400 ValidationError 'The instance <instance-id> is not in Standby.'; the emulator answers 'Instance <instance-id> is in InService state, not in Standby state, and cannot be moved.'
2. Cause 2: create a group with DesiredCapacity 1, let the instance reach InService, EnterStandby then ExitStandby it (capacity rises to 2), then call ExitStandby(AutoScalingGroupName=<that group>, InstanceIds=['i-00000000000000001']) with an id that was never returned by DescribeAutoScalingGroups for that group. Expect HTTP 400 ValidationError 'The instance i-00000000000000001 is not part of Auto Scaling group <group-name>.'; the emulator instead reports that id as an InService member, because its own launched members are numbered from i-00000000000000001 upwards.
3. Confirm the allocation independently: create two groups each with DesiredCapacity 1 and read Instances[].InstanceId from DescribeAutoScalingGroups — the ids are i-00000000000000001 and i-00000000000000002 rather than unpredictable 17-hex-character ids.
