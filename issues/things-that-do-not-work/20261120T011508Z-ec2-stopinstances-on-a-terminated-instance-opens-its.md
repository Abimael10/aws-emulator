Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ec2.stop-start-transitions-are-monotone-and-reads-do-not-advance-them
Issue-Shape-Id: com.amazonaws.ec2#AmazonEC2
Issue-Key: ec2.stop-start-transitions-are-monotone-and-reads-do-not-advance-them|_legs#3|AssertionError|assert err.get("Message") == expected, (
Issue-Commit: 16caeccf34fd30fafc2febabf9312168f85b2a49

# EC2 StopInstances on a terminated instance opens its IncorrectInstanceState message with "The instance" instead of AWS's "This instance"

> _Note: this issue was identified against the code base at commit `16caeccf34fd30fafc2febabf9312168f85b2a49`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
StopInstances

## Description
One net-new root cause in the EC2 emulator, grounded in the one failing behavioral check listed under `### ec2` that the replay report marks neither ALREADY FILED nor OWNED. The HTTP status (400) and the error code (`IncorrectInstanceState`) are correct; the refusal MESSAGE's fixed text diverges from the sentence AWS words this refusal with for StopInstances.

Named-but-not-covering issues: 20261027T192643Z-ec2-startinstances-on-a-terminated-instance-returns-an.md (fixed) corrected the SAME message class for StartInstances and is where the shared sentence now comes from, but it names StartInstances only and does not carry this check's replay key — this is not a reopen of it: the start wording it established is a DIFFERENT sentence from the stop wording, and the code and status here are already right. 20261115T003554Z-ec2-control-plane-runinstances-discards-cpuoptions-the-instance.md and 20261116T091224Z-cloudwatch-ec2-2-net-new-causes-monitorinstances-on.md each carry an IncorrectInstanceState message defect on OTHER operations (instance-attribute modification, MonitorInstances) and neither lists this check.

Cause 1 of 1: StopInstances' IncorrectInstanceState sentence for an instance that cannot be stopped begins "The instance" instead of "This instance".

AWS: recorded behavior only — EC2 speaks ec2Query and the Smithy model declares NO error shapes for this operation (smithy_query on services/service-com-amazonaws-ec2-amazonec2/smithy-model/com-amazonaws-ec2-amazonec2.json: StopInstances input InstanceIds (InstanceIdStringList, required), Hibernate, SkipOsShutdown, DryRun, Force; output StoppingInstances; errors: []), so the code and the message are observable only from recorded responses. The recording shows StopInstances naming an instance whose state is `terminated` refused with HTTP 400, error code `IncorrectInstanceState`, and the message "This instance '<instance-id>' is not in a state from which it can be stopped." The FIXED text of that template is `This instance '` + `' is not in a state from which it can be stopped.` (single trailing full stop); the ONLY variable part is the instance id, taken verbatim from the request's InstanceIds member. AWS does not name the state the instance is actually in. Note that this is a different opening from the sibling start refusal, which AWS words "The instance '<id>' is not in a state from which it can be started." (the sentence 20261027T192643Z established): the two sentences are not one template with a swapped verb.

Emulator: same HTTP 400 and the same code `IncorrectInstanceState`, but the message reads "The instance '<instance-id>' is not in a state from which it can be stopped." — AWS's start-refusal opening ("The instance") applied to the stop refusal, where AWS writes "This instance". Everything else in the sentence, including the trailing full stop and the request-derived instance id, matches.

Why: the message is the only human- and machine-readable statement of why the lifecycle call was refused. A client relying on AWS's behavior — a retry wrapper, a harness, a runbook or a log matcher that compares or regexes the IncorrectInstanceState message against AWS's stop sentence — does not recognise the refusal, and anything written against the emulator comes to depend on a sentence AWS never produces for StopInstances. Because the emulator serves the start wording for both, a consumer also cannot tell the two refusals apart by their fixed text the way AWS lets it.

Fix: the stop-refusal wording in the StopInstances handler's state check and in the shared message builder it calls — `Scaffolded::incorrect_instance_state(instance_id, action)` formats one sentence ("The instance '{id}' is not in a state from which it can be {action}.") for start, stop, reboot and monitor, so the stop path needs its own fixed text with the instance id from the request as the only interpolated value. File and line citations are under Traces.

Traces: ec2.stop-start-transitions-are-monotone-and-reads-do-not-advance-them.

## Evidence
Check ec2.stop-start-transitions-are-monotone-and-reads-do-not-advance-them (verdict: mismatch), suite simparity-behavioral-aws-v1.

AssertionError at check_ec2_stop_start_transitions_are_monotone_and_reads_do_not_advance_them.py line 167 (in _legs, reached from run line 54):
  assert err.get("Message") == expected, (
  AssertionError: stopping a TERMINATED instance must be refused with "This instance 'i-00000000000000001' is not in a state from which it can be stopped.", got "The instance 'i-00000000000000001' is not in a state from which it can be stopped."

Expected (AWS): HTTP 400, Error.Code = IncorrectInstanceState, Error.Message = "This instance '<instance-id>' is not in a state from which it can be stopped."
Actual (emulator): HTTP 400, Error.Code = IncorrectInstanceState, Error.Message = "The instance '<instance-id>' is not in a state from which it can be stopped."

Message anatomy — fixed text: "This instance '" ... "' is not in a state from which it can be stopped." ; variable: the instance id, copied from the request's InstanceIds. The literal id i-00000000000000001 in the lines above is this run's value filling the template and is NOT part of the requirement. The divergence is confined to the two opening words.

Everything the check asserts before this point passed: StopInstances' PreviousState running / CurrentState stopping with codes 16/64, the observed running->stopping->stopped walk, StateReason Client.UserInitiatedShutdown with "Client.UserInitiatedShutdown: User initiated shutdown", the canonical "User initiated (YYYY-MM-DD HH:MM:SS GMT)" StateTransitionReason, five identical freeze-detector DescribeInstances reads, DescribeInstanceStatus(IncludeAllInstances=True), the start walk stopped->pending->running with the private IPv4 retained, the terminate walk running->shutting-down->terminated, and the error CODE IncorrectInstanceState on the final stop. Only the message's fixed text diverges.

Source of the sentence: services/service-com-amazonaws-ec2-amazonec2/src/ops/stop_instances.rs line 214 calls `Scaffolded::incorrect_instance_state(instance_id, "stopped")`; that builder is services/service-com-amazonaws-ec2-amazonec2/src/scaffolded.rs lines 438-444 and formats `The instance '{instance_id}' is not in a state from which it can be {action}.` for every lifecycle operation.

Smithy: StopInstances errors = [] (no modeled error shapes), hence "recorded behavior only" for the status, code and message.

## Steps to Reproduce
1. git_clone(), then run_behavioral_suite().
2. Replay check ec2.stop-start-transitions-are-monotone-and-reads-do-not-advance-them (listed under `### ec2`).
3. Or by hand against the emulator: CreateVpc 10.79.0.0/16, CreateSubnet 10.79.1.0/24, RunInstances (t3.micro, the catalog AL2023 x86_64 AMI, that subnet); wait for state `running`.
4. StopInstances, wait for `stopped`; StartInstances, wait for `running`; TerminateInstances, wait for `terminated`.
5. Call StopInstances again on the terminated instance id.
6. Observe HTTP 400 / IncorrectInstanceState with "The instance '<id>' is not in a state from which it can be stopped."; AWS answers HTTP 400 / IncorrectInstanceState with "This instance '<id>' is not in a state from which it can be stopped." (instance id from the request, no state clause, trailing full stop).
