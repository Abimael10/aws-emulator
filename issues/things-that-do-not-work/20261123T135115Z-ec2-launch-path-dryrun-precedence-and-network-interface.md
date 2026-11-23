Issue-Source: trace-sweep-divergence
Issue-Trace: ec2-asgeni-traj-launch-template-ni-accepted-then-refused-at-launch
Issue-Step: task#9
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: ec2-asgeni-traj-launch-template-ni-accepted-then-refused-at-launch ec2-asgeni-traj-run-instances-ni-device-index-and-placement ec2-asgeni-traj-run-instances-ni-param-conflicts ec2-trajwire-deb-deb-instance-type-casing-and-absent-ami ec2-trajwire-deb-deb-security-group-name-passed-with-subnet ec2-dry-run-validation-before-commit
Issue-Shape-Id: com.amazonaws.ec2#AmazonEC2
Issue-Key: ec2|RunInstances|4xx->4xx|$.Response.Errors.Error.Message
Issue-Key: ec2|CreateCapacityReservationFleet|4xx->4xx|$.Response.Errors.Error.Code
Issue-Key: ec2|CreateCapacityReservationFleet|4xx->4xx|$.Response.Errors.Error.Message
Issue-Key: ec2|CreateCapacityReservationFleet|4xx->4xx|status
Issue-Related: 20261103T071133Z-ec2-invalid-enum-and-attribute-values-are-refused.md
Issue-Related: 20261122T230953Z-ec2-request-validation-and-refusal-paths-diverge-wrong.md
Issue-Commit: 94f7b27553b727586aac9de0add3c17de28dbaa9

# ec2 launch-path DryRun precedence and network-interface validation diverge in status, error code and message

> _Note: this issue was identified against the code base at commit `94f7b27553b727586aac9de0add3c17de28dbaa9`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
RunInstances

## Description
Five net-new causes on the EC2 launch/reserve paths. They share one fault: the emulator's own request-validation layer for NetworkInterfaces and for DryRun runs in the wrong order and emits emulator-authored error codes and prose instead of the EC2 refusals the recordings show. The EC2 Smithy model declares no modeled errors for these operations (smithy_query operation=RunInstances/CreateCapacityReservationFleet -> "errors": []), so for every code and message below the authority is "recorded behavior only" plus the modeled DryRun contract (Smithy: RunInstancesRequest.DryRun doc — "If you have the required permissions, the error response is DryRunOperation. Otherwise, it is UnauthorizedOperation").

Cause 1 of 5 — RunInstances: associatePublicIpAddress with more than one network interface (fixture ec2-asgeni-traj-launch-template-ni-accepted-then-refused-at-launch, step task#9).
AWS: recorded behavior only — HTTP 400, Code InvalidParameterCombination, Message exactly "The associatePublicIPAddress parameter cannot be specified when launching with multiple network interfaces." The whole message is fixed text; it has no variable parts.
Emulator: same status and code but its own prose, "A public IPv4 address cannot be assigned when the request specifies more than one network interface".
Why: clients and test suites that match on the refusal text (and humans reading it) see a message AWS never emits; the sentence is also not terminated the way AWS terminates it, so exact-match assertions fail.
Fix: the RunInstances handler's network-interface validation — emit AWS's fixed sentence verbatim.

Cause 2 of 5 — RunInstances: network interfaces supplied with no interface at device index 0 (fixture ec2-asgeni-traj-run-instances-ni-device-index-and-placement, step task#5).
AWS: recorded behavior only — HTTP 400, Code UnsupportedOperation, Message exactly "When specifying network interfaces, you must include a device at index 0." (all fixed text).
Emulator: HTTP 400 with Code InvalidParameterCombination and message "A launch that specifies network interfaces must specify the primary network interface at device index 0".
Why: the error CODE is part of the contract — SDK retry/branching logic keys on UnsupportedOperation, and a client that special-cases that code never sees it.
Fix: the RunInstances handler's network-interface validation — classify this refusal as UnsupportedOperation and use AWS's sentence.

Cause 3 of 5 — RunInstances: a network interface supplied with no deviceIndex, request also carrying DryRun (fixture ec2-asgeni-traj-run-instances-ni-device-index-and-placement, step task#6).
AWS: recorded behavior only — shape validation precedes the dry-run short-circuit, so the answer is HTTP 400, Code InvalidParameterValue, Message exactly "Each network interface requires a device index." (all fixed text).
Emulator: HTTP 412, Code DryRunOperation, Message "Request would have succeeded, but DryRun flag is set." — it short-circuits on DryRun before validating the interface list.
Why: a caller using DryRun as a pre-flight check is told a malformed launch would have succeeded; the same request then fails for real at commit time, which is exactly what DryRun exists to prevent.
Fix: the RunInstances handler — run input validation before the DryRun short-circuit.

Cause 4 of 5 — RunInstances: launch-template-derived interfaces whose device indexes AWS accepts, request carrying DryRun (fixture ec2-asgeni-traj-launch-template-ni-accepted-then-refused-at-launch, step task#15).
AWS: recorded behavior only — HTTP 412, Code DryRunOperation, Message exactly "Request would have succeeded, but DryRun flag is set." (all fixed text), i.e. AWS does not refuse this interface set at all.
Emulator: HTTP 400, Code InvalidParameterValue, message "Value (0) for parameter deviceIndex is invalid. Each network interface must be attached at a device index of its own" — an over-strict duplicate-index rule applied to a set AWS admits.
Why: a launch configuration that works on AWS is rejected by the emulator, so the pre-flight check fails on a valid request.
Fix: the RunInstances handler's device-index uniqueness rule — it must not reject the interface set AWS accepts; the member-name and value in the message are request-derived, the surrounding sentence is fixed text.

Cause 5 of 5 — CreateCapacityReservationFleet: DryRun request without InstanceMatchCriteria (fixture ec2-dry-run-validation-before-commit, step task#2).
AWS: Smithy: CreateCapacityReservationFleetRequest.InstanceMatchCriteria is NOT required (only TotalTargetCapacity is), and recorded behavior is HTTP 412, Code DryRunOperation, Message exactly "Request would have succeeded, but DryRun flag is set."
Emulator: HTTP 400, Code MissingParameter, Message "The following parameter is missing: instanceMatchCriteria" — it invents a required member.
Why: a valid fleet request is refused; the caller cannot create a capacity reservation fleet without sending a member AWS defaults.
Fix: CreateCapacityReservationFleet's validation — drop the invented requirement so the operation reaches the DryRun answer and, without DryRun, the create.

Related open issues that do NOT carry these replay keys: 20261122T230953Z-ec2-request-validation-and-refusal-paths-diverge-wrong.md and 20261119T122355Z-ec2-refusal-fidelity-7-net-new-causes-createlaunchtemplate.md.

## Evidence
Cause 1 — expected $.Response.Errors.Error.Message 'The associatePublicIPAddress parameter cannot be specified when launching with multiple network interfaces.'; actual 'A public IPv4 address cannot be assigned when the request specifies more than one network interface' (status 400 both). Cluster also covers ec2-asgeni-traj-launch-template-ni-accepted-then-refused-at-launch#task#13, ec2-asgeni-traj-run-instances-ni-device-index-and-placement#task#4/#task#7/#task#8, ec2-asgeni-traj-run-instances-ni-param-conflicts#task#8.
Cause 2 — expected Code 'UnsupportedOperation' / Message 'When specifying network interfaces, you must include a device at index 0.'; actual Code 'InvalidParameterCombination' / Message 'A launch that specifies network interfaces must specify the primary network interface at device index 0'. Also affects ec2-trajwire-deb-deb-instance-type-casing-and-absent-ami#task#5.
Cause 3 — expected status 412->actual... reversed: expected status 400 Code 'InvalidParameterValue' Message 'Each network interface requires a device index.'; actual status 412 Code 'DryRunOperation' Message 'Request would have succeeded, but DryRun flag is set.'. Also affects ec2-asgeni-traj-run-instances-ni-device-index-and-placement#task#10, ec2-asgeni-traj-run-instances-ni-param-conflicts#task#7, ec2-trajwire-deb-deb-instance-type-casing-and-absent-ami#task#4, ec2-trajwire-deb-deb-security-group-name-passed-with-subnet#task#5.
Cause 4 — expected status 412 Code 'DryRunOperation' Message 'Request would have succeeded, but DryRun flag is set.'; actual status 400 Code 'InvalidParameterValue' Message 'Value (0) for parameter deviceIndex is invalid. Each network interface must be attached at a device index of its own'.
Cause 5 — expected status 412 Code 'DryRunOperation' Message 'Request would have succeeded, but DryRun flag is set.'; actual status 400 Code 'MissingParameter' Message 'The following parameter is missing: instanceMatchCriteria'.

## Steps to Reproduce
Replay trace set simparity-wire-aws-v1 against the emulator and inspect: (1) ec2-asgeni-traj-launch-template-ni-accepted-then-refused-at-launch task#9 and task#15; (2) ec2-asgeni-traj-run-instances-ni-device-index-and-placement task#5 and task#6; (3) ec2-dry-run-validation-before-commit task#2. Equivalently: RunInstances with two NetworkInterfaces where one sets AssociatePublicIpAddress; RunInstances with a NetworkInterface at DeviceIndex 1 only; RunInstances with DryRun=true and a NetworkInterface carrying no DeviceIndex; RunInstances with DryRun=true from a launch template whose interfaces AWS accepts; CreateCapacityReservationFleet with DryRun=true, TotalTargetCapacity set and InstanceMatchCriteria omitted.
