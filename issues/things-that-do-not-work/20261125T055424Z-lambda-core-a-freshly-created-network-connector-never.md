Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: lambda-core.network-connector-create-nonexistent-subnet-rejected
Issue-Shape-Id: com.amazonaws.lambdacore#LambdaCoreApiService
Issue-Key: lambda-core.network-connector-create-nonexistent-subnet-rejected|_poll_until#4|AssertionError|raise AssertionError(f"condition not met within {timeout_s}s (last={last!r})")
Issue-Commit: 24df4b5bb34709ed7c873e3f6d0f18f4590f38dd

# lambda-core: a freshly created network connector never reaches ACTIVE inside a client's poll window — PENDING→ACTIVE is held behind a fixed 3 s wall-clock span and materialised only lazily on a later read

> _Note: this issue was identified against the code base at commit `24df4b5bb34709ed7c873e3f6d0f18f4590f38dd`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetNetworkConnector

## Description
One net-new root cause, grounded in the one surviving failing behavioral check for lambda-core.

Cause 1 of 1: the connector that CreateNetworkConnector mints stays PENDING for a fixed span of wall clock and is only flipped to ACTIVE by whichever later call happens to report the record, so a client that follows the documented poll loop does not observe ACTIVE on any probe taken inside its window and the whole check dies before it can exercise the behaviour it is about (the synchronous rejection of a non-existent subnet).

AWS: Smithy CreateNetworkConnector (POST /2026-04-04/network-connectors, success code 202, idempotent; output members Arn, Name, Id, VersionArn, Version, Configuration, OperatorRole, State -> NetworkConnectorState) is documented asynchronous: the connector "starts in PENDING state while ENIs are provisioned in your VPC" and a client is told to "use GetNetworkConnector to poll the connector state until it reaches ACTIVE". Smithy GetNetworkConnector (GET /2026-04-04/network-connectors/{Identifier}, success code 200, readonly) reports State, StateReason, StateReasonCode. The requirement under test is the state VALUE a poll of GetNetworkConnector converges on — ACTIVE, from NetworkConnectorState [PENDING, ACTIVE, INACTIVE, FAILED, DELETING, DELETE_FAILED] — not any message; no documented duration states how long provisioning takes, so the duration itself is recorded behavior only. The ARNs, connector id, name, role ARN and LastModified in the replay output are per-run environment values and are not part of the requirement.

Emulator: the create answers with State=PENDING, StateReason="Initial creation", and the PENDING->ACTIVE change is NOT applied at creation time. It is stored on the record as a due instant (services/.../src/state.rs: WORK_COMPLETES_AFTER = Duration::from_secs(3), settles_at(), PendingTransition/PendingChange::State, ConnectorVersion::settle / NetworkConnector::settle) and materialised only by the first call that reports the record AFTER that instant. Every GetNetworkConnector issued inside the span therefore answers 200 with State=PENDING; the replay's last observed response is exactly that (State 'PENDING', StateReason 'Initial creation'), and the check's positive control fails with "condition not met within 1.0s" having never seen ACTIVE or FAILED. Because the emulator charges a client real wall clock per lifecycle step rather than converging on its first read, the suite's remaining time budget had already been consumed by earlier waits, which is what shrank this check's own poll window — the non-convergence and the budget it burns are the same defect.

Why: a client that implements AWS's documented "poll GetNetworkConnector until ACTIVE" loop is never told anything wrong, it simply never arrives: every probe inside the span reports a connector still provisioning. Everything downstream of an ACTIVE connector (UpdateNetworkConnector, which the model says requires ACTIVE; attachment of compute; the suite's negative-class checks that need a warmed positive control first) is unreachable or forced to pay a full poll interval per step, so workflows and tests that complete against AWS stall or time out against the emulator.

Fix: this belongs in the network-connector state/lifecycle layer of the lambda-core service (the create handler's state mutation and the settle path that reports it), not in a validator or serializer. The provisioning outcome must be settled so that the FIRST read following the create already reports the terminal state (make the transition due at creation time, or settle on read), while the transient state stays observable in the create call's own 202 answer where the model exposes it. Note the open issue 20261103T161149Z-lambda-core-lambda-microvms-vpc-egress-data-plane.md describes the same mechanism for the two cross-service lambda-core + lambda-microvms data-plane checks (and cites the then-500 ms constant, since raised to 3 s); replay reports THIS check as unowned, so this cause must be closed for the lambda-core control-plane check too.

Traces: lambda-core.network-connector-create-nonexistent-subnet-rejected.

## Evidence
Behavioral suite simparity-behavioral-aws-v1 replayed against the emulator built from this checkout: 27/33 passed (81.8%), bootstrap_signal false; per-service lambda-core 26/28 (93%), 2 failed (the other lambda-core failure is OWNED by 20261022T193451Z-ec2-createsubnet-stub.md and skipped).

[mismatch] lambda-core.network-connector-create-nonexistent-subnet-rejected
  File "check_lambda_core_network_connector_create_nonexistent_subnet_rejected.py", line 104, in _active_control -> line 148, in _poll_until
  AssertionError: condition not met within 1.0s (last={... 'State': 'PENDING', 'StateReason': 'Initial creation', 'Version': 1, 'Configuration': {'VpcEgressConfiguration': {'SubnetIds': ['subnet-...'], 'SecurityGroupIds': ['sg-...'], 'NetworkProtocol': 'IPv4', 'AssociatedComputeResourceTypes': ['MicroVm']}}, ...})

Expected (AWS): GetNetworkConnector on a connector created with a real subnet and a warmed operator role converges to State='ACTIVE' (or 'FAILED'); the check then proceeds to assert that CreateNetworkConnector with a well-formed but non-existent subnet is refused synchronously with InvalidParameterValueException and the message "Error occurred while DescribeSubnets. EC2 Error Code: InvalidSubnetID.NotFound. EC2 Error Message: The subnet ID '<the SubnetId from the request>' does not exist" (fixed text as quoted; the quoted subnet id is the value taken from the request), and that no connector was persisted.
Actual (emulator): every GetNetworkConnector inside the create's settle span answers HTTP 200 with State='PENDING'; no probe in the check's window observes a terminal state, so the poll fails and the rejection assertions are never reached. No operation was refused — nothing returned a non-2xx — so this is a lifecycle-convergence defect, not a status/shape divergence.

Grounded in this checkout's own code: services/service-com-amazonaws-lambdacore-lambdacoreapiservice/src/state.rs lines 150-185 (WORK_COMPLETES_AFTER = Duration::from_secs(3), settles_at()), lines 212-226 (PendingTransition.due), lines 248-265 (ConnectorVersion::settle — applies the owed state only when now >= due, on whichever call reports the record).

## Steps to Reproduce
1. git_clone(), then run_behavioral_suite(); lambda-core.network-connector-create-nonexistent-subnet-rejected reports "condition not met within 1.0s" with the last GetNetworkConnector response showing State='PENDING'.
2. By hand against the running emulator: create a VPC, a subnet in an available AZ and a security group (ec2), and an IAM role with the lambda trust policy.
3. POST /2026-04-04/network-connectors (CreateNetworkConnector) with Configuration.VpcEgressConfiguration naming that subnet and group and OperatorRole set to the role ARN. Observe the answer carries State='PENDING'.
4. Immediately GET /2026-04-04/network-connectors/{Id} (GetNetworkConnector) — observe 200 with State='PENDING', and that it keeps reporting PENDING on every read taken within ~3 s of the create; ACTIVE appears only on a read taken after that span. AWS's documented poll-until-ACTIVE loop must converge on its first read; here it misses and the client pays its whole poll interval (or, on a short budget as in the check, never converges at all).
5. Once the control connector is ACTIVE, CreateNetworkConnector with SubnetIds=['subnet-' + 17 hex chars that do not exist] to reach the assertions the poll failure currently hides: AWS refuses synchronously with InvalidParameterValueException naming the missing subnet and persists no connector (ListNetworkConnectors must not show the name).
