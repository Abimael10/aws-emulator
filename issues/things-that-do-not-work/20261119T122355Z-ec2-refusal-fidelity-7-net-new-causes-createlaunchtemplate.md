Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ec2.asgeni-launch-template-with-unknown-ssm-alias-rejected ec2.modify-private-dns-name-options-running-instance-rejected ec2.placement-partition-number-above-the-group-count-rejected ec2.placement-spread-group-rejects-more-instances-than-one-zone-can-place ec2.op-contract-modify-instance-credit-specification ec2.create-delegate-mac-volume-ownership-task-invalid-instance-rejected ec2.op-contract-modify-instance-connect-endpoint
Issue-Shape-Id: com.amazonaws.ec2#AmazonEC2
Issue-Key: ec2.asgeni-launch-template-with-unknown-ssm-alias-rejected|_error#4|AssertionError|raise AssertionError("the call succeeded; real AWS rejects it, so there is no error")
Issue-Key: ec2.modify-private-dns-name-options-running-instance-rejected|_error#10|AssertionError|raise AssertionError(f"expected the call to be rejected, but it SUCCEEDED and returned {result!r}")
Issue-Key: ec2.placement-partition-number-above-the-group-count-rejected|_expected_failure#17|AssertionError|raise AssertionError(
Issue-Key: ec2.placement-spread-group-rejects-more-instances-than-one-zone-can-place|_expected_failure#21|AssertionError|raise AssertionError(
Issue-Key: ec2.op-contract-modify-instance-credit-specification|run#15|AssertionError|assert message in expected_messages, f"expected one of {expected_messages!r}, got {message!r}"
Issue-Key: ec2.create-delegate-mac-volume-ownership-task-invalid-instance-rejected|run#16|AssertionError|assert code == "InvalidInstanceID.Malformed", (
Issue-Key: ec2.op-contract-modify-instance-connect-endpoint|run#19|AssertionError|assert msg == expected, f"expected message {expected!r}, got {msg!r}"
Issue-Commit: 20b40f42cd78b95666b130bae2475dd4b1dad646

# EC2 refusal fidelity: 7 net-new causes — CreateLaunchTemplate/ModifyPrivateDnsNameOptions/RunInstances placement accept requests AWS refuses, and 3 refusals carry the wrong code or message

> _Note: this issue was identified against the code base at commit `20b40f42cd78b95666b130bae2475dd4b1dad646`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateLaunchTemplate

## Description
Seven net-new root causes in the EC2 control plane's refusal surface, all grounded in failing behavioral checks of this suite. Four are over-permissive accepts (the emulator returns 200 and creates a resource where AWS refuses and creates nothing); three are refusals whose code or message text diverges. None of these checks is marked ALREADY FILED or OWNED in the replay report. Related open issues cover neighbouring checks but not these replay keys: 20261117T185234Z-ec2-10-net-new-refusal-classification-causes-createcapacityreservationfleet.md and 20261114T091241Z-ec2-9-net-new-causes-refusal-messages-are.md.

Cause 1 of 7: CreateLaunchTemplate accepts an unresolvable `resolve:ssm:` ImageId alias.
AWS: an ImageId given as `resolve:ssm:<parameter-path>` is resolved when the launch template is created; a path no SSM parameter exists under is refused with error code `SsmInvalidParameter` and message `The following aliases are invalid: <parameter-path>` — fixed text `The following aliases are invalid: ` followed by the alias from the request with the `resolve:ssm:` prefix stripped. The refusal creates nothing, so DescribeLaunchTemplates on that name then answers `InvalidLaunchTemplateName.NotFoundException` / `At least one of the launch templates specified in the request does not exist.` (fixed text). Smithy: CreateLaunchTemplate models no operation errors (EC2 is ec2Query and its refusals are not modelled), so the code/message pair is recorded behaviour only. The same request with a resolvable alias is accepted, and an IamInstanceProfile ARN naming no profile IS stored verbatim — the validation is specific to the SSM alias.
Emulator: the CreateLaunchTemplate call SUCCEEDS and a template is created (the check's `_error` helper raised "the call succeeded; real AWS rejects it, so there is no error").
Why: a client that mistypes or has not yet published the parameter gets a template that looks valid and only fails later at launch, and tooling that relies on the create-time refusal to fail fast silently ships a broken template.
Fix: CreateLaunchTemplate's validation (resolution of `resolve:ssm:` ImageId aliases) in the ec2 service's create_launch_template handler.
Traces: ec2.asgeni-launch-template-with-unknown-ssm-alias-rejected

Cause 2 of 7: ModifyPrivateDnsNameOptions is accepted on a running instance.
AWS: the hostname type of a RUNNING instance cannot be modified; the call is refused with `InvalidParameterValue` and the message `Cannot modify hostname-type for running instances` (entirely fixed text — no request values appear in it). The rejected call leaves PrivateDnsNameOptions.HostnameType unchanged, and the identical call succeeds once the instance is stopped. Smithy: ModifyPrivateDnsNameOptions input PrivateDnsHostnameType targets the HostnameType enum (ip-name|resource-name); output is only `Return: Boolean`; no modelled errors, so the code/message is recorded behaviour only.
Emulator: answers HTTP 200 with `{'Return': True}` for a running instance.
Why: a client reads the accept as proof the hostname type changed while the instance keeps serving the old private DNS name, and a caller that relies on the refusal to know it must stop the instance first never learns it.
Fix: ModifyPrivateDnsNameOptions' validation (instance-state guard) in the ec2 handler for that operation.
Traces: ec2.modify-private-dns-name-options-running-instance-rejected

Cause 3 of 7: RunInstances accepts a Placement.PartitionNumber above the placement group's PartitionCount.
AWS: the bound comes from the group, not the request — launching into partition `PartitionCount + 1` of a partition group is refused with `InvalidParameterValue` and the message `Value '<n>' is not a valid value for PartitionNumber. Use a value between 1 and <partition-count>.` where `<n>` is the PartitionNumber from the request and `<partition-count>` is the PartitionCount of the group named in Placement.GroupName; the refusal creates nothing (the group holds no members afterwards). A launch into the highest partition the group DOES have succeeds. Smithy: Placement.PartitionNumber is an Integer documented "The number of the partition that the instance is in. Valid only if the placement group strategy is set to partition" — no range trait, so the limit is the group's own PartitionCount and the code/message pair is recorded behaviour only.
Emulator: the launch SUCCEEDS — a reservation is returned and an instance enters `pending`.
Why: the instance is billed and placed in a partition that does not exist, so a client using partitions for fault isolation gets no isolation guarantee, and the refused-launch-creates-nothing contract is broken.
Fix: RunInstances' placement validation in the ec2 run_instances handler (compare Placement.PartitionNumber against the stored PartitionCount of the referenced placement group).
Traces: ec2.placement-partition-number-above-the-group-count-rejected

Cause 4 of 7: RunInstances accepts more instances in a spread placement group than one Availability Zone can place.
AWS: a spread placement group holds at most seven instances per Availability Zone. A RunInstances with MinCount=MaxCount=8 into one subnet of such a group is refused atomically with `InsufficientInstanceCapacity` and the message `You've reached the limit of instances in this spread placement group. A spread placement group can have up to seven instances per Availability Zone.` (entirely fixed text), and creates nothing — the group still holds no members. Reference: https://docs.aws.amazon.com/AWSEC2/latest/UserGuide/placement-strategies.html. Smithy models no errors for RunInstances, so code and message are recorded behaviour only.
Emulator: the launch SUCCEEDS — a reservation is returned and instances enter `pending`.
Why: a client gets instances the real service would never have placed, so a capacity-planning or anti-affinity assumption built on the spread ceiling is wrong, and an atomic-refusal expectation (all placed or nothing created) is violated.
Fix: RunInstances' placement validation in the ec2 run_instances handler (per-AZ spread-group member ceiling, enforced against MinCount before anything is created).
Traces: ec2.placement-spread-group-rejects-more-instances-than-one-zone-can-place

Cause 5 of 7: ModifyInstanceCreditSpecification's InvalidInstanceID.NotFound message omits the other instance ids in the batch.
AWS: when any InstanceId in the batch is unknown the WHOLE request is refused as a raised error with code `InvalidInstanceID.NotFound`, and the message names every instance id the request carried: `The instance IDs <id-list> do not exist or have been terminated. Change the IDs and try again.` — fixed text around `<id-list>`, which is the comma-separated list of the InstanceIds taken from the request (order is not significant). The refused batch leaves the real instance's CpuCredits untouched. Smithy: ModifyInstanceCreditSpecification declares no modelled errors, so the message template is recorded behaviour only.
Emulator: the code is right but the message lists only the unknown id — `The instance IDs i-00000000 do not exist or have been terminated. Change the IDs and try again.` — where the request also carried the live instance id.
Why: a client that parses the message to learn which ids its batch was refused for (the only place the per-id information appears, since this operation raises instead of reporting per-entry failures) gets an incomplete list.
Fix: ModifyInstanceCreditSpecification's refusal message construction in the ec2 handler for that operation.
Traces: ec2.op-contract-modify-instance-credit-specification

Cause 6 of 7: CreateDelegateMacVolumeOwnershipTask classifies a well-formed but unallocated instance id as NotFound instead of Malformed.
AWS: this operation refuses an instance id that is syntactically well formed but allocated to nothing with code `InvalidInstanceID.Malformed` and message `The instance ID '<instance-id>' is malformed` — fixed text with the InstanceId from the request in single quotes. The boundary discriminates on the id: a real, running, non-Mac instance draws a wholly different refusal, `InvalidInstanceType` / `The instance type is not supported for this operation`, which the emulator already answers correctly. Smithy: CreateDelegateMacVolumeOwnershipTask models no errors (input InstanceId is required, MacCredentials is required and sensitive), so the classification is recorded behaviour only.
Emulator: answers `InvalidInstanceID.NotFound` / `The instance ID 'i-0123456789abcdef0' does not exist`.
Why: the two classes mean different things to a client — Malformed says the id can never be valid for this operation and must be corrected, NotFound says it may appear later, so retry logic keyed on the code retries forever on an id that will never resolve.
Fix: CreateDelegateMacVolumeOwnershipTask's instance-id validation/classification in the ec2 handler for that operation.
Traces: ec2.create-delegate-mac-volume-ownership-task-invalid-instance-rejected

Cause 7 of 7: ModifyInstanceConnectEndpoint's not-found message is worded differently from AWS's.
AWS: an unknown InstanceConnectEndpointId is refused with code `InvalidInstanceConnectEndpointId.NotFound` and message `The instance-connect-endpoint ID '<eice-id>' does not exist` — fixed text with the InstanceConnectEndpointId from the request in single quotes. Smithy: ModifyInstanceConnectEndpoint models no errors; output is only `Return: Boolean`, so the message is recorded behaviour only. The happy path (swapping SecurityGroupIds and reading back update-complete) already matches.
Emulator: right code, wrong message — `The Instance Connect Endpoint eice-00000000000000000 does not exist. Add an existing Instance Connect Endpoint ID and retry your request.`
Why: a client or test asserting on AWS's message text (the message is part of the refusal contract) sees a different sentence and an unquoted id.
Fix: ModifyInstanceConnectEndpoint's refusal message in the ec2 handler for that operation.
Traces: ec2.op-contract-modify-instance-connect-endpoint

## Evidence
All quotes are from the behavioral replay's failing-check details.

Cause 1 — ec2.asgeni-launch-template-with-unknown-ssm-alias-rejected: expected `("SsmInvalidParameter", "The following aliases are invalid: /sim2real/absent-<...>")`; got AssertionError "the call succeeded; real AWS rejects it, so there is no error" (CreateLaunchTemplate returned 200).

Cause 2 — ec2.modify-private-dns-name-options-running-instance-rejected: expected code `InvalidParameterValue`, message `Cannot modify hostname-type for running instances`; got "expected the call to be rejected, but it SUCCEEDED and returned {'Return': True, ... 'HTTPStatusCode': 200 ...}".

Cause 3 — ec2.placement-partition-number-above-the-group-count-rejected: expected `("InvalidParameterValue", "Value '3' is not a valid value for PartitionNumber. Use a value between 1 and 2.")` for partition 3 of a 2-partition group; got "the request was expected to be refused, but it SUCCEEDED: {'ReservationId': 'r-...', ... 'StateReason': {'Code': 'pending'} ...}".

Cause 4 — ec2.placement-spread-group-rejects-more-instances-than-one-zone-can-place: expected code `InsufficientInstanceCapacity` with message "You've reached the limit of instances in this spread placement group. A spread placement group can have up to seven instances per Availability Zone." for MinCount=8 in one AZ; got "the request was expected to be refused, but it SUCCEEDED: {'ReservationId': 'r-...', ...}".

Cause 5 — ec2.op-contract-modify-instance-credit-specification: expected one of "The instance IDs <live-id>, i-00000000 do not exist or have been terminated. Change the IDs and try again." / the same two ids reversed; got "The instance IDs i-00000000 do not exist or have been terminated. Change the IDs and try again." (code InvalidInstanceID.NotFound was correct).

Cause 6 — ec2.create-delegate-mac-volume-ownership-task-invalid-instance-rejected: expected `InvalidInstanceID.Malformed` / "The instance ID 'i-0123456789abcdef0' is malformed"; got `InvalidInstanceID.NotFound` / "The instance ID 'i-0123456789abcdef0' does not exist".

Cause 7 — ec2.op-contract-modify-instance-connect-endpoint: expected message "The instance-connect-endpoint ID 'eice-00000000000000000' does not exist"; got 'The Instance Connect Endpoint eice-00000000000000000 does not exist. Add an existing Instance Connect Endpoint ID and retry your request.'

## Steps to Reproduce
Replay the behavioral suite against the emulator and re-run exactly these checks:
1. ec2.asgeni-launch-template-with-unknown-ssm-alias-rejected — CreateLaunchTemplate with LaunchTemplateData.ImageId = "resolve:ssm:/<path no parameter exists under>"; expect SsmInvalidParameter and no template created.
2. ec2.modify-private-dns-name-options-running-instance-rejected — RunInstances a t3.micro, wait running, ModifyPrivateDnsNameOptions(PrivateDnsHostnameType="resource-name"); expect InvalidParameterValue "Cannot modify hostname-type for running instances", then stop the instance and expect the same call to succeed.
3. ec2.placement-partition-number-above-the-group-count-rejected — CreatePlacementGroup(Strategy="partition", PartitionCount=2), RunInstances with Placement={GroupName, PartitionNumber: 3}; expect InvalidParameterValue with the "Use a value between 1 and 2." message and zero group members.
4. ec2.placement-spread-group-rejects-more-instances-than-one-zone-can-place — CreatePlacementGroup(Strategy="spread"), RunInstances MinCount=MaxCount=8 into one subnet with Placement={GroupName}; expect InsufficientInstanceCapacity and zero group members.
5. ec2.op-contract-modify-instance-credit-specification — ModifyInstanceCreditSpecification with two entries, one live instance id and one unknown id "i-00000000"; expect InvalidInstanceID.NotFound whose message lists BOTH ids.
6. ec2.create-delegate-mac-volume-ownership-task-invalid-instance-rejected — CreateDelegateMacVolumeOwnershipTask(InstanceId="i-0123456789abcdef0", MacCredentials=<base64>); expect InvalidInstanceID.Malformed "...is malformed".
7. ec2.op-contract-modify-instance-connect-endpoint — ModifyInstanceConnectEndpoint(InstanceConnectEndpointId="eice-00000000000000000", SecurityGroupIds=[sg]); expect the message "The instance-connect-endpoint ID 'eice-00000000000000000' does not exist".
