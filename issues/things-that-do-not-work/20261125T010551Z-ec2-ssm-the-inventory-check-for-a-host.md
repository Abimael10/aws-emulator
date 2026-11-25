Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ec2__ssm.vscan-inventory-omits-a-host-whose-profile-role-lacks-the-ssm-core-policy-until-it-is-attached
Issue-Key: ec2__ssm.vscan-inventory-omits-a-host-whose-profile-role-lacks-the-ssm-core-policy-until-it-is-attached|run#21|Timeout|_call_with_retry(lambda: ec2.stop_instances(InstanceIds=[starved]))
Issue-Commit: 07a12ce4140be4c807d532cfc03706c508ac8640

# ec2__ssm: the inventory check for a host whose instance-profile role lacks AmazonSSMManagedInstanceCore never reaches a verdict — the host never reports Online and the check is cut at its budget

> _Note: this issue was identified against the code base at commit `07a12ce4140be4c807d532cfc03706c508ac8640`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeInstanceInformation

## Description
Cause 1 of 1: the scenario that proves SSM inventory OMITS an EC2 host whose instance-profile role carries no AmazonSSMManagedInstanceCore and ADMITS it once the policy is attached makes no observable progress: the check spends its whole run budget polling and is cut off with "Check timed out after 60.0s", so none of its assertions is graded.

AWS: recorded behavior only (SSM's agent-registration timing is not expressed in the Smithy model). The recording the check is built on shows that an EC2 instance launched with an instance profile whose role has no AmazonSSMManagedInstanceCore never appears in SSM's managed-node views — DescribeInstanceInformation lists no record for that instance id, so it is absent from inventory — and that after AttachRolePolicy puts arn:aws:iam::aws:policy/AmazonSSMManagedInstanceCore on that same role, the instance's agent registers and the node reaches PingStatus Online within the check's documented 600 s registration deadline, from which point inventory includes it. The check's own comment records that deadline: "Deadline for an SSM agent on a fresh host to report Online; above 300 s, so the suite's per-check budget is raised. A host that never registers fails the check."

Emulator: the polled condition never becomes true inside the run budget. The check is terminated while waiting, and the suite records only "Check timed out after 60.0s" — no step-level diagnosis, no status, no error code and no error message, because nothing refused: the awaited managed-node state simply never arrives.

Hypothesis: the launched instance never registers as an SSM managed node at all (so the before/after differential has no "after" to observe), which is the same shape of gap as the open issues 20261115T003554Z-ec2-ssm-ssm-managed-nodes-execute-nothing-invocations-end-failed.md and 20261103T200742Z-ec2-ssm-cloudformation-deployed-ec2-ssm-estates-are-unusable-instance.md. Neither of those carries this check's replay key, so neither covers it; if the fix for them also makes this host register, re-replaying the trace id below is what proves it.

Why: a client cannot tell an authorization gap (role missing AmazonSSMManagedInstanceCore, therefore absent from inventory) from a service that registers nobody. Every remediation workflow built on "attach the core policy, then the node appears" is unobservable, and patch/inventory tooling sees a permanently empty fleet.

Fix: the SSM managed-node registration path and the state it reads from — an instance whose profile role grants ssm:UpdateInstanceInformation (via AmazonSSMManagedInstanceCore) must come to PingStatus Online and be listed by DescribeInstanceInformation/inventory, and one whose role does not must stay absent.

Traces: ec2__ssm.vscan-inventory-omits-a-host-whose-profile-role-lacks-the-ssm-core-policy-until-it-is-attached (mismatch).

## Evidence
Check: ec2__ssm.vscan-inventory-omits-a-host-whose-profile-role-lacks-the-ssm-core-policy-until-it-is-attached
Verdict: mismatch — "Error: Check timed out after 60.0s"

Expected (recorded AWS behavior): with no AmazonSSMManagedInstanceCore on the instance profile's role, DescribeInstanceInformation returns no record for the instance id and inventory omits it; after attaching arn:aws:iam::aws:policy/AmazonSSMManagedInstanceCore to that role, the node reports PingStatus 'Online' within the check's 600 s registration deadline and inventory includes it.

Actual (emulator): the poll for the node's registration never satisfies its predicate; the check is terminated when its run budget ends, before the differential is graded. The emulator produced no refusal — no HTTP status, error code or message is involved; the divergence is the absence of the managed-node record.

Check's own documented deadline (from the replayed source): _SSM_ONLINE_DEADLINE_S = 600, "A host that never registers fails the check."

## Steps to Reproduce
1. Create an IAM role with an ec2.amazonaws.com trust policy and NO AmazonSSMManagedInstanceCore attached, wrap it in an instance profile.
2. RunInstances a t3.micro from the latest AL2 AMI (/aws/service/ami-amazon-linux-latest/amzn2-ami-hvm-x86_64-gp2) with that instance profile, in a subnet that can reach SSM.
3. Poll ssm:DescribeInstanceInformation for the instance id: AWS lists nothing (the host is absent from inventory).
4. AttachRolePolicy arn:aws:iam::aws:policy/AmazonSSMManagedInstanceCore to the role and poll again for up to 600 s. Observe that in the emulator the node never reaches PingStatus Online and never appears in inventory, so the omit-then-admit differential never converges and the scenario ends only when the client's budget does.
