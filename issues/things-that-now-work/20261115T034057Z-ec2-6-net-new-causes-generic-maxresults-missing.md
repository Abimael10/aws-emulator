Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ec2.describe-managed-prefix-lists-contract ec2.describe-network-insights-paths-contract ec2.op-contract-describe-byoip-cidrs-duplicate ec2.op-contract-modify-reserved-instances-required-params-rejected ec2.ec2-delete-vpc-removes-own-and-default-resource-tags ec2.dasgrefresh-instance-refresh-replaces-every-guest-with-the-new-launch-template-version
Issue-Shape-Id: com.amazonaws.ec2#AmazonEC2
Issue-Key: ec2.describe-managed-prefix-lists-contract|run#12|AssertionError|assert message == expected_message, f"expected {expected_message!r}, got {message!r}"
Issue-Key: ec2.describe-network-insights-paths-contract|run#10|AssertionError|assert message == expected, f"expected message {expected!r}, got {message!r}"
Issue-Key: ec2|DescribeByoipCidrs|InvalidMaxResults
Issue-Key: ec2.op-contract-modify-reserved-instances-required-params-rejected|run#6|AssertionError|assert message == "TargetReservedInstancesConfigurationSet", message
Issue-Key: ec2.ec2-delete-vpc-removes-own-and-default-resource-tags|run#24|AssertionError|assert len(rule_tags) == 1 and rule_tags[0]["Value"] == tag_value, (
Issue-Key: ec2.dasgrefresh-instance-refresh-replaces-every-guest-with-the-new-launch-template-version|run#66|AssertionError|assert over.get("Message") == over_message, (
Issue-Commit: 2bc939698b9a959721d42e691c2ca676c39ad8ba

# ec2: 6 net-new causes — generic MaxResults/missing-parameter refusal prose, DeleteVpc wipes orphaned security-group-rule tags, and SetDesiredCapacity message appends the group name

> _Note: this issue was identified against the code base at commit `2bc939698b9a959721d42e691c2ca676c39ad8ba`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

> _Causes 1-4 are FIXED and their replay keys pass; this report carries only those keys. Causes 5 and 6 are NOT: they are re-filed, with the blocker they are on, as `issues/things-that-do-not-work/20261115T034057Z-ec2-2-of-the-6-causes-remain-open-guests-observe-nothing.md`, which carries their five replay keys so they are not recorded as working._

## Operation
DescribeManagedPrefixLists

## Description
Six net-new root causes under the `ec2` behavioral service. Each is grounded in the failing checks named under it.

=== Cause 1 of 6: every over-maximum MaxResults refusal carries one emulator-authored sentence instead of the per-operation message AWS words ===
AWS: each EC2 read declares its own MaxResults bound and words its own refusal. Smithy (com-amazonaws-ec2-amazonec2): DescribeManagedPrefixListsRequest.MaxResults -> PrefixListMaxResults @range min 1 max 100; DescribeNetworkInsightsPathsRequest.MaxResults -> NetworkInsightsMaxResults @range min 1 max 100; DescribeByoipCidrsRequest.MaxResults -> DescribeByoipCidrsMaxResults @range min 1 max 100 (and @required). The recorded refusals are HTTP 400 and the messages are operation-specific: DescribeManagedPrefixLists answers fixed text `The requested max results (` + the rejected value + `) is larger than limit (100).`; DescribeNetworkInsightsPaths answers the wholly fixed text `MaxResults must be between 1 and 100`; DescribeByoipCidrs answers the wholly fixed text `The MaxResults parameter cannot be larger than 100.` under error code InvalidMaxResults. Only the rejected value is a request-supplied variable; the limit 100 is the modelled bound, and the rest of each sentence is fixed text.
Emulator: all three answer one shared sentence, `Value ( <n> ) for parameter maxResults is invalid. Expecting a value smaller than or equal to 100.`, where `<n>` is the rejected value. The bound is enforced, but the wording belongs to no AWS operation.
Why: a client that matches on the refusal text — log scrapers, retry/back-off rules that distinguish a page-size refusal from other 400s, and test suites asserting AWS's wording — does not recognise the refusal, and three operations that AWS words differently become indistinguishable. The open issue 20261029T175114Z-ec2-client-error-classification-wrong-error-codes-for.md records out-of-range MaxResults among its causes but does not carry these three checks' keys.
Fix: the MaxResults validation of each of these three read handlers (or the shared page-size validator they call) must render that operation's own message rather than one generic template.
Traces: ec2.describe-managed-prefix-lists-contract, ec2.describe-network-insights-paths-contract, ec2.op-contract-describe-byoip-cidrs-duplicate.

=== Cause 2 of 6: ModifyReservedInstances' missing-required-parameter refusal is emulator prose instead of AWS's parameter-name message ===
AWS: Smithy ModifyReservedInstancesRequest declares ReservedInstancesIds and TargetConfigurations as @required (TargetConfigurations' wire name is the ReservedInstancesConfigurationSet parameter group). A request that omits TargetConfigurations is refused HTTP 400 and the recorded message is exactly the wire parameter name `TargetReservedInstancesConfigurationSet` — the whole message is fixed text, with no sentence around it.
Emulator: answers the message `The request must include the TargetConfigurations parameter. Add the required parameter and retry the request.` — an emulator-authored sentence naming the SDK member rather than the wire parameter.
Why: a client or SDK that reads the message to learn WHICH parameter the query protocol rejected gets a different token than AWS emits, so parameter-name-driven error handling and fixtures of EC2 query missing-parameter refusals do not match.
Fix: the required-member validation for ModifyReservedInstances must emit the wire parameter name as the message, the way the EC2 query protocol's missing-parameter refusal does.
Traces: ec2.op-contract-modify-reserved-instances-required-params-rejected.

=== Cause 3 of 6: DeleteVpc also destroys the tags of security-group rules it orphans ===
AWS: recorded behavior only. Deleting a VPC removes the VPC's own resources and their tags, but a tag written on a security-group rule that the delete orphans survives the VPC delete — DescribeTags/the rule's tag set still returns that tag after the VPC is gone.
Emulator: after DeleteVpc the tag set of the orphaned security-group rule is empty: `expected the orphaned security-group-rule tag on sgr-<id> to survive the VPC delete (real-AWS behaviour), got []`.
Why: a client that uses tags on security-group rules for ownership, cost attribution or cleanup bookkeeping loses that record on a VPC delete, so a reconciliation pass sees untagged rules and cannot tell which were its own.
Fix: the DeleteVpc cascade in the ec2 service state must not remove tags of security-group rules it does not itself delete.
Traces: ec2.ec2-delete-vpc-removes-own-and-default-resource-tags.

=== Cause 4 of 6: the Auto Scaling above-max SetDesiredCapacity refusal appends the group name AWS leaves out ===
AWS: SetDesiredCapacity with a DesiredCapacity above the group's MaxSize is refused with error code ValidationError and the message `New SetDesiredCapacity value <desired> is above max value <max> for the AutoScalingGroup.` — the fixed text ends at the full stop after `AutoScalingGroup`; the only variable parts are the rejected desired capacity and the group's MaxSize, both taken from the request and the stored group.
Emulator: answers `{'Type': 'Sender', 'Code': 'ValidationError', 'Message': 'New SetDesiredCapacity value <desired> is above max value <max> for the AutoScalingGroup: <group-name>'}` — the full stop is replaced by a colon and the group name is appended.
Why: a client matching the documented refusal text (or comparing refusals across groups) sees a message that varies with the group name and never equals AWS's, so the refusal is not recognised. The open issue 20261113T163227Z-autoscaling-cloudformation-ec2-setdesiredcapacity-s-above-max-refusal-appends-group-name.md describes the same wording under the autoscaling__cloudformation__ec2 heading but does not carry this check's key.
Fix: the SetDesiredCapacity bounds validation's refusal renderer must terminate the sentence after `AutoScalingGroup.` and not interpolate the group name.
Traces: ec2.dasgrefresh-instance-refresh-replaces-every-guest-with-the-new-launch-template-version.

=== Cause 5 of 6: in-guest commands return no output, so nothing about a running guest can be observed === (RE-FILED, NOT FIXED: `issues/things-that-do-not-work/20261115T034057Z-ec2-2-of-the-6-causes-remain-open-guests-observe-nothing.md` carries this cause and its replay keys)
AWS: recorded behavior only. A launched instance runs a real guest; a command executed in it (here `uptime`, and `chronyc waitsync` / `chronyc sources` against the link-local Amazon Time Sync service at 169.254.169.123) writes its result to stdout, and that stdout is what the caller reads back.
Emulator: every such command yields an empty string. `uptime` returns `''`, so the check comparing uptime before and after a refused terminate fails with `ValueError: could not convert string to float: ''`; `chronyc waitsync` returns `''`, reported as "`chronyc waitsync` did not report success, so the guest never synchronised to any time source: ''". No non-empty guest stdout is produced on any of these paths.
Why: a client cannot verify anything that is only observable inside the instance — that a refused TerminateInstances left the guest running and ticking, or that the guest's clock is driven by the link-local time source and agrees with the service's own Date header. Every in-guest assertion silently has nothing to assert on. The open issue 20261115T003554Z-ec2-guests-observe-nothing-in-guest-commands-exit.md records the same symptom for other checks but does not carry these keys.
Fix: the guest execution path of the ec2 service's data plane must run the command in the instance and return its real stdout, and must present the link-local time source the guest's NTP client synchronises against.
Traces: ec2.shutdown-api-termination-disabled-rejects-terminate-but-still-permits-stop, ec2.timesync-chrony-in-the-guest-locks-onto-the-link-local-amazon-time-sync-source, ec2.timesync-guest-clock-agrees-with-the-aws-api-date-header-inside-two-seconds.

=== Cause 6 of 6: a guest-initiated shutdown never moves the instance per InstanceInitiatedShutdownBehavior === (RE-FILED, NOT FIXED: same report as cause 5)
AWS: recorded behavior only. When the guest halts itself, the instance follows the InstanceInitiatedShutdownBehavior attribute: with `stop` it settles in `stopped` and can be started again; with `terminate` it settles in `terminated`. DescribeInstances reflects that final state within a client's polling budget.
Emulator: neither transition is ever produced — both checks are killed with `Check timed out after 60.0s` without reaching a verdict, so the instance stays in its pre-shutdown state and the attribute has no effect.
Why: a client relying on guest-initiated shutdown (an instance that powers itself off at the end of a job, the standard pattern for self-terminating workers) sees an instance that never leaves `running`, so neither the stop-and-restart nor the self-terminate lifecycle can be exercised or relied on.
Fix: the ec2 service's instance lifecycle must act on a guest-initiated shutdown and apply the stored InstanceInitiatedShutdownBehavior to reach `stopped` or `terminated`.
Traces: ec2.shutdown-in-guest-halt-with-stop-behavior-leaves-it-stopped-and-restartable, ec2.shutdown-in-guest-halt-with-terminate-behavior-terminates-the-instance.

## Evidence
Cause 1 — ec2.describe-managed-prefix-lists-contract: `AssertionError: expected 'The requested max results (1000) is larger than limit (100).', got 'Value ( 1000 ) for parameter maxResults is invalid. Expecting a value smaller than or equal to 100.'`. ec2.describe-network-insights-paths-contract: `AssertionError: expected message 'MaxResults must be between 1 and 100', got 'Value ( 1000 ) for parameter maxResults is invalid. Expecting a value smaller than or equal to 100.'`. ec2.op-contract-describe-byoip-cidrs-duplicate: `AssertionError: unexpected InvalidMaxResults message: 'Value ( 101 ) for parameter maxResults is invalid. Expecting a value smaller than or equal to 100.'` (expected `The MaxResults parameter cannot be larger than 100.`). The rejected values 1000/101 are request-supplied; 100 is the modelled bound.

Cause 2 — ec2.op-contract-modify-reserved-instances-required-params-rejected: expected message `TargetReservedInstancesConfigurationSet`, got `The request must include the TargetConfigurations parameter. Add the required parameter and retry the request.`

Cause 3 — ec2.ec2-delete-vpc-removes-own-and-default-resource-tags: `AssertionError: expected the orphaned security-group-rule tag on sgr-<id> to survive the VPC delete (real-AWS behaviour), got []` (the rule id is a per-run value).

Cause 4 — ec2.dasgrefresh-...: expected `New SetDesiredCapacity value 5 is above max value 4 for the AutoScalingGroup.`, got `...for the AutoScalingGroup: <group-name>` with Code ValidationError, Type Sender. The 5/4 are the request's desired capacity and the group's MaxSize; the group name is a per-run value and is not part of AWS's message at all.

Cause 5 — ec2.shutdown-api-termination-disabled-...: `ValueError: could not convert string to float: ''` at `assert float(after_refusal["uptime"]) > float(before["uptime"])`. ec2.timesync-chrony-...: "`chronyc waitsync` did not report success, so the guest never synchronised to any time source: ''". ec2.timesync-guest-clock-...: same empty WAITSYNC output.

Cause 6 — ec2.shutdown-in-guest-halt-with-stop-behavior-... and ec2.shutdown-in-guest-halt-with-terminate-behavior-...: `Error: Check timed out after 60.0s`, no assertion reached.

## Steps to Reproduce
1. Clone the repo and run the behavioral suite so the emulator is serving.
2. Cause 1: call DescribeManagedPrefixLists(MaxResults=1000), DescribeNetworkInsightsPaths(MaxResults=1000) and DescribeByoipCidrs(MaxResults=101) and read each refusal's message — all three return the same generic sentence instead of their own.
3. Cause 2: call ModifyReservedInstances with ReservedInstancesIds set and TargetConfigurations omitted and read the refusal message.
4. Cause 3: create a VPC, a security group in it with a rule, tag that security-group rule, delete the VPC, then read the rule's tags — the tag is gone.
5. Cause 4: create an Auto Scaling group with MaxSize=4 and call SetDesiredCapacity(DesiredCapacity=5); read Error.Message.
6. Cause 5: launch an instance and run `uptime` and `chronyc waitsync` in the guest — stdout is empty both times.
7. Cause 6: launch an instance with InstanceInitiatedShutdownBehavior=stop (then =terminate), halt it from inside the guest and poll DescribeInstances — it never reaches `stopped` / `terminated`.
