Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: lambda-core.network-connector-create-nonexistent-subnet-rejected
Issue-Shape-Id: com.amazonaws.lambdacore#LambdaCoreApiService
Issue-Key: lambda-core.network-connector-create-nonexistent-subnet-rejected|_poll_until#4|AssertionError|raise AssertionError(f"condition not met within {timeout_s}s (last={last!r})")
Issue-Commit: a4424715f3a5bc425be43bc4e35aa4a3867c8573

# lambda-core network-connector provisioning still never converges: the first GetNetworkConnector after a successful create reports PENDING, so no connector is ever observed ACTIVE

> _Note: this issue was identified against the code base at commit `a4424715f3a5bc425be43bc4e35aa4a3867c8573`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetNetworkConnector

## Description
One net-new root cause on the lambda-core network-connector lifecycle (com.amazonaws.lambdacore#LambdaCoreApiService).

The resolved issue 20261103T161149Z-lambda-core-a-created-network-connector-is-never.md claimed this exact behaviour fixed (it carries the same replay key, lambda-core.network-connector-create-nonexistent-subnet-rejected|_poll_until#4|AssertionError), yet the check fails again with the identical assertion and the identical last response. The call is NOT refused (every response is HTTP 200), so this is re-filed as a still-open state-convergence defect rather than a refusal regression; the fix landed as documentation defending the wall-clock span, not as convergence.

Cause 1 of 1 — the asynchronous PENDING->ACTIVE provisioning of a network connector is owed to a fixed wall-clock span and materialised only by a later read, so the FIRST GetNetworkConnector a client issues after a successful CreateNetworkConnector still reports State=PENDING and a poll loop that gets only one probe inside its budget never observes a terminal state at all.

AWS: Smithy — CreateNetworkConnector, POST /2026-04-04/network-connectors, success code 202, output member State targets the NetworkConnectorState enum (ACTIVE | DELETE_FAILED | DELETING | FAILED | INACTIVE | PENDING); GetNetworkConnector, GET /2026-04-04/network-connectors/{Identifier}, code 200, @readonly, reports State plus StateReason/StateReasonCode. The operation's own documentation states the contract: "This operation is asynchronous. The network connector starts in PENDING state while ENIs are provisioned in your VPC ... Use GetNetworkConnector to poll the connector state until it reaches ACTIVE." The requirement is CONVERGENCE of the State member to a terminal value (ACTIVE, or FAILED with a StateReason) for a client that polls GetNetworkConnector on its own schedule; PENDING is a transient the create response already exposes. No message is under test here — the diverging datum is the State enum value.

Emulator: CreateNetworkConnector answers State=PENDING / StateReason="Initial creation" (correct), and GetNetworkConnector, polled immediately afterwards, answers HTTP 200 with State=PENDING again. In the replay the wait for State in (ACTIVE, FAILED) expired with the connector still PENDING; the last response carried State='PENDING', StateReason='Initial creation', Version=1 and no StateReasonCode. The transition is stored as a PendingTransition due at now + LIFECYCLE_TRANSITION (a fixed 500 ms span) and applied lazily by whichever later call reads the record, so no read taken promptly after the create can report the terminal state, and a client whose remaining budget allows a single probe never sees anything but PENDING.

Why: every documented use of a connector is gated on "poll until ACTIVE" — attaching it to a MicroVM or a WebFunction, updating it, and the whole VPC-egress data plane. Against the emulator that loop does not terminate within a realistic client budget, so a client following AWS's own instruction hangs or gives up and the connector-dependent half of the service is unreachable. Here it also means the behaviour the check exists to exercise (CreateNetworkConnector refusing a non-existent subnet with InvalidParameterValueException and the message "Error occurred while DescribeSubnets. EC2 Error Code: InvalidSubnetID.NotFound. EC2 Error Message: The subnet ID '<subnetId>' does not exist" — fixed text, with only <subnetId> taken from the request) is never reached, and neither is the assertion that a synchronously-refused create persists no connector in ListNetworkConnectors.

Fix: lambda-core's connector state/lifecycle layer (not a serializer or a validator). The provisioning settlement must be due such that the first GetNetworkConnector following a successful create already reports the terminal state — make it due at creation time, or drive it from a read/step counter rather than elapsed wall clock — leaving PENDING observable in CreateNetworkConnector's own 202 response, which is where the model exposes the transient. The update (InProgress->Successful) and deletion (DELETING->removal) transitions share the same constant and must settle on the same terms, so that the ResourceConflictException arms that depend on an observably PENDING connector keep their window through the create response rather than through a wall-clock gate.

Traces: lambda-core.network-connector-create-nonexistent-subnet-rejected. Code citations for the fixer: services/service-com-amazonaws-lambdacore-lambdacoreapiservice/src/state.rs line 159 (LIFECYCLE_TRANSITION = Duration::from_millis(500)) with the doc comment at lines 131-158 that argues for the non-zero span, lines 161-176 (PendingChange / PendingTransition) and lines 198-209 (ConnectorVersion::settle, which applies the change only when now >= pending.due); services/service-com-amazonaws-lambdacore-lambdacoreapiservice/src/ops/create_network_connector.rs (the pending transition written on create). Related open issue 20261103T161149Z-lambda-core-lambda-microvms-vpc-egress-data-plane.md describes the same mechanism for the cross-service lambda-core + lambda-microvms data-plane checks and names the lambda-microvms spans; it carries no key for this lambda-core-only check, so it does not cover this one.

## Evidence
Behavioral suite simparity-behavioral-aws-v1 replayed against the emulator built from this checkout: 26/33 passed, bootstrap_signal false (the emulator answered every call in this check). Per-service: lambda-core 26/28 (93%), 2 failed — the other lambda-core failure is OWNED by an existing open issue.

[mismatch] lambda-core.network-connector-create-nonexistent-subnet-rejected:
  AssertionError: condition not met within 1.0s (last={'ResponseMetadata': {'HTTPStatusCode': 200, 'HTTPHeaders': {'content-type': 'application/json', ...}}, 'Arn': 'arn:aws:lambda:us-west-2:000000000000:network-connector:nc-0000000000000001', 'Name': 'sim2real-lnc-<id>-ctl0', 'Id': 'nc-0000000000000001', 'Version': 1, 'Configuration': {'VpcEgressConfiguration': {'SubnetIds': ['subnet-<id>'], 'SecurityGroupIds': ['sg-<id>'], 'NetworkProtocol': 'IPv4', 'AssociatedComputeResourceTypes': ['MicroVm']}}, 'OperatorRole': 'arn:aws:iam::000000000000:role/sim2real-lnc-<id>', 'State': 'PENDING', 'StateReason': 'Initial creation', 'LastModified': <timestamp>})
  raised from the wait `lambda resp: resp['State'] in ('ACTIVE', 'FAILED')` over lc.get_network_connector(Identifier=<connectorId>) in _active_control/_poll_until.

Expected (AWS): the poll observes a terminal State — 'ACTIVE' ("poll the connector state until it reaches ACTIVE"), or 'FAILED' with a StateReason.
Actual (emulator): HTTP 200 with State='PENDING' on every read inside the client's budget; no call was refused and no error was returned, so this is a state-convergence divergence, not a wire-shape or status one.

The ARN, account id, generated connector id, subnet/security-group ids, name suffix and timestamp above are per-run values and are not part of the requirement. The requirement is the State member reaching a terminal value on the first read after the create.

This is the same failure the resolved issue 20261103T161149Z-lambda-core-a-created-network-connector-is-never.md recorded (same check, same assertion, same PENDING/'Initial creation' last response); state.rs still defines LIFECYCLE_TRANSITION as a 500 ms wall-clock span settled lazily on read.

## Steps to Reproduce
1. git_clone, then run_behavioral_suite() and look at check lambda-core.network-connector-create-nonexistent-subnet-rejected (listed under ### lambda-core).
2. Or by hand against the running emulator: create a VPC, a subnet in an available AZ and a security group in EC2, and an IAM role trusted by lambda.amazonaws.com with the operator policy attached.
3. POST /2026-04-04/network-connectors with Name=<unique>, Configuration={"VpcEgressConfiguration":{"SubnetIds":[<subnetId>],"SecurityGroupIds":[<sgId>],"NetworkProtocol":"IPv4","AssociatedComputeResourceTypes":["MicroVm"]}}, OperatorRole=<roleArn>. Observe 202 with State=PENDING (correct).
4. Immediately GET /2026-04-04/network-connectors/<Id> ONCE, as a poll loop's first iteration does: observe HTTP 200 with State=PENDING. AWS's contract is that this poll converges to ACTIVE; the emulator flips the state only on a read taken more than 500 ms of wall clock after the create, so a client with a single probe inside its budget never observes a terminal state and everything gated on ACTIVE is unreachable.
5. Downstream, never reached today: POST /2026-04-04/network-connectors with a well-formed but non-existent SubnetIds entry ("subnet-" + 17 hex chars) and confirm it is refused 400 InvalidParameterValueException with "Error occurred while DescribeSubnets. EC2 Error Code: InvalidSubnetID.NotFound. EC2 Error Message: The subnet ID '<subnetId>' does not exist", and that ListNetworkConnectors(MaxItems=100) contains no connector with that Name.
