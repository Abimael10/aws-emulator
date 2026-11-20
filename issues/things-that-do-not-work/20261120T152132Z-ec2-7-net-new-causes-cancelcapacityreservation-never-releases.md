Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ec2.capres-cancel-with-a-running-instance-releases-it-without-stopping-the-guest ec2.cpuopts-core-count-outside-valid-cores-rejected ec2.cpuopts-partial-cpu-options-rejected ec2.cpuopts-guest-topology-differs-from-the-type-default ec2.op-contract-deleteClientVpnEndpoint ec2.op-contract-describe-ipam-resource-discoveries ec2.op-contract-revoke-client-vpn-ingress
Issue-Shape-Id: com.amazonaws.ec2#AmazonEC2
Issue-Key: ec2.capres-cancel-with-a-running-instance-releases-it-without-stopping-the-guest|run#29|AssertionError|assert released["AvailableInstanceCount"] == total, (
Issue-Key: ec2.cpuopts-core-count-outside-valid-cores-rejected|run#20|AssertionError|assert rejected.response.get("Error", {}).get("Message", "") == (
Issue-Key: ec2.cpuopts-partial-cpu-options-rejected|run#16|AssertionError|assert _code(rejected) == "InvalidParameterValue", (
Issue-Key: ec2.cpuopts-guest-topology-differs-from-the-type-default|_guest_topology#24|AssertionError|assert not missing, (
Issue-Key: ec2.op-contract-deleteClientVpnEndpoint|_client_error#18|AssertionError|raise AssertionError(f"expected the call to be rejected, but it succeeded and returned {result!r}")
Issue-Key: ec2.op-contract-describe-ipam-resource-discoveries|run#11|AssertionError|assert message == (
Issue-Key: ec2.op-contract-revoke-client-vpn-ingress|run#21|AssertionError|assert err.get("Message") == "Specified rule does not exist or cannot be deleted", (
Issue-Commit: a14965d6060559849ea419b74faa25e7836c4558

# ec2: 7 net-new causes — CancelCapacityReservation never releases the seats it held, DeleteClientVpnEndpoint is idempotent instead of NotFound, four refusal classifications/messages are emulator-authored, and the guest has no lscpu

> _Note: this issue was identified against the code base at commit `a14965d6060559849ea419b74faa25e7836c4558`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CancelCapacityReservation

## Description
Seven independent net-new root causes found by this behavioral sweep, all in the ec2 service. None is covered by an open issue (several open ec2 issues cover neighbouring operations but none carries these replay keys).

=== Cause 1 of 7: CancelCapacityReservation leaves AvailableInstanceCount at 0 after the reservation is cancelled ===
AWS: CancelCapacityReservation on a reservation that currently holds a running instance is accepted (Return=true); the reservation then settles in state 'cancelled' and holds nothing. Smithy (ec2): CapacityReservation.AvailableInstanceCount is documented as "The remaining capacity. Indicates the number of instances that can be launched in the Capacity Reservation." — so once cancelled, AvailableInstanceCount equals TotalInstanceCount, CapacityAllocations is empty, and GetCapacityReservationUsage reports InstanceUsages = [] with State 'cancelled', while the instance that had been consuming the reservation keeps running and is attributed to nobody. The exact post-cancel values are recorded behavior only (CancelCapacityReservation models no errors).
Emulator: CancelCapacityReservation returns Return=true and DescribeCapacityReservations does reach State 'cancelled', but AvailableInstanceCount stays 0 against TotalInstanceCount 1 — the seat the running instance occupied is never released.
Why: a client cancels a reservation precisely to stop holding capacity. Reading the cancelled reservation back, it sees zero remaining capacity forever, so capacity accounting, dashboards and schedulers that re-provision after a cancel conclude the pool is still full.
Fix: CancelCapacityReservation's handler and the capacity-reservation state — the cancel transition must drop every allocation and recompute AvailableInstanceCount from TotalInstanceCount.
Traces: ec2.capres-cancel-with-a-running-instance-releases-it-without-stopping-the-guest.

=== Cause 2 of 7: RunInstances' refusal message for an unsupported CpuOptions.CoreCount is emulator-authored ===
AWS: a CoreCount the instance type does not publish in VCpuInfo.ValidCores is refused with InvalidParameterValue and the message "A value of <CoreCount> for CoreCount is not a valid value for the <InstanceType> instance type. Valid values are <ValidCores>." The fixed text is "A value of ", " for CoreCount is not a valid value for the ", " instance type. Valid values are ", "."; the variable parts are the rejected CoreCount and the InstanceType taken from the request and the type's ValidCores as DescribeInstanceTypes publishes them. Recorded behavior only (EC2 models no errors on RunInstances).
Emulator: the code is correct (InvalidParameterValue) but the message is its own: 'Value (2) for parameter cpuOptions.coreCount is invalid. Valid values are 1' — it names the wire member path instead of the API member CoreCount, never names the instance type, and drops the trailing period.
Why: clients and operators match on this sentence (and the instance type inside it) to tell an unsupported-topology refusal from a generic parameter refusal; the emulator's wording breaks that and hides which type refused the value.
Fix: the RunInstances CpuOptions validation's message construction.
Traces: ec2.cpuopts-core-count-outside-valid-cores-rejected.

=== Cause 3 of 7: RunInstances refuses a partial CpuOptions with the wrong error code and the model's doc sentence as the message ===
AWS: CpuOptions carrying only ThreadsPerCore (no CoreCount) is refused with InvalidParameterValue and the message "When specifying CpuOptions you must specify both CoreCount and ThreadsPerCore." — entirely fixed text, no request values. Smithy (ec2) states the rule itself: CpuOptionsRequest's doc is "The CPU options for the instance. Both the core count and threads per core must be specified in the request."
Emulator: the refusal is classified InvalidParameterCombination and the message is 'Both the core count and threads per core must be specified in the request' — the model's documentation sentence has been shipped as the wire message, under a different error class.
Why: clients branch on Error.Code; validation and retry logic keyed on InvalidParameterValue does not fire for InvalidParameterCombination, and the message never names the two members a caller must supply.
Fix: the RunInstances CpuOptions validation — classify as InvalidParameterValue and emit AWS's sentence.
Traces: ec2.cpuopts-partial-cpu-options-rejected.

=== Cause 4 of 7: the launched guest exposes no lscpu, so the CPU topology cannot be read inside the instance ===
AWS: an Amazon Linux 2023 guest reports its CPU topology through both the kernel and util-linux: /sys/devices/system/cpu/online and /sys/devices/system/cpu/cpu0/topology/thread_siblings_list are populated, and `lscpu` prints the "CPU(s):", "Thread(s) per core:" and "Core(s) per socket:" lines that reflect the CpuOptions the instance launched with (on m5.large, ThreadsPerCore=1 halves the visible vCPU count). Recorded behavior only — this is guest-side and nothing in the model describes it.
Emulator: an aws:RunShellScript invocation on the instance succeeds (Status Success, ResponseCode 0) and does report the procfs/sysfs views, but every lscpu-derived value comes back empty: stdout was 'GUEST-NPROC 1\nGUEST-ONLINE 0\nGUEST-PROCESSORS 1\nGUEST-SIBLINGS 0\nGUEST-LSCPU-CPUS \nGUEST-LSCPU-TPC \nGUEST-LSCPU-CPS \n'. `lscpu` produces no output in the guest.
Why: a client that verifies the topology it asked for using the guest's standard tooling (lscpu is the documented way) gets nothing, and any in-guest software that sizes itself from lscpu mis-sizes.
Fix: the guest image / userland the EC2 data plane boots must carry util-linux's lscpu and it must report the topology the instance's CpuOptions describe. The open issue 20261115T003554Z-ec2-guest-hardware-is-not-presented-instance-store.md covers neighbouring guest-hardware gaps but does not carry this check.
Traces: ec2.cpuopts-guest-topology-differs-from-the-type-default.

=== Cause 5 of 7: DeleteClientVpnEndpoint succeeds a second time instead of refusing the deleted endpoint ===
AWS: DeleteClientVpnEndpoint on an endpoint id that no longer exists (already deleted, or well-formed but never created) is refused with InvalidClientVpnEndpointId.NotFound and the message "Endpoint <ClientVpnEndpointId> does not exist" — fixed text "Endpoint " and " does not exist", with the id taken from the request. Recorded behavior only (DeleteClientVpnEndpoint models no errors).
Emulator: after a successful delete has settled, repeating DeleteClientVpnEndpoint with the same id answers HTTP 200 with {'Status': {'Code': 'deleted'}} instead of refusing it.
Why: this is over-permissive. Clients use the NotFound code to tell an already-torn-down endpoint from a live one (idempotent cleanup paths catch exactly InvalidClientVpnEndpointId.NotFound), and a stale or mistyped endpoint id now reads as a successful delete of something that was never there.
Fix: DeleteClientVpnEndpoint's handler — resolve the id against the client-VPN state and refuse an absent one before answering.
Traces: ec2.op-contract-deleteClientVpnEndpoint.

=== Cause 6 of 7: DescribeIpamResourceDiscoveries' NotFound message is emulator-authored ===
AWS: filtering by a well-formed but nonexistent IpamResourceDiscoveryIds entry is refused (it does not narrow to an empty page) with InvalidIpamResourceDiscoveryId.NotFound and the message "The IPAM resource discovery (<id>) does not exist." — fixed text "The IPAM resource discovery (" and ") does not exist.", with the id taken from the request. Recorded behavior only (DescribeIpamResourceDiscoveries models no errors).
Emulator: the code is correct but the message is "The ipam-resource-discovery ID 'ipam-res-disco-0000000000000000' does not exist" — it substitutes the resource-type token for AWS's prose name, wraps the id in single quotes instead of parentheses, and drops the trailing period.
Why: message text is part of the contract operators and log-matching tooling rely on; a divergent sentence breaks it even though the code matches.
Fix: the DescribeIpamResourceDiscoveries handler's not-found refusal message.
Traces: ec2.op-contract-describe-ipam-resource-discoveries.

=== Cause 7 of 7: RevokeClientVpnIngress' authorization-rule NotFound message is emulator-authored ===
AWS: revoking an authorization rule for a TargetNetworkCidr that holds no rule is refused with InvalidClientVpnEndpointAuthorizationRuleNotFound and the message "Specified rule does not exist or cannot be deleted" — the whole sentence is fixed text; AWS deliberately names neither the CIDR nor the endpoint. Recorded behavior only (RevokeClientVpnIngress models no errors).
Emulator: the code is correct but the message is 'There is no authorization rule for 10.223.0.0/24 in Client VPN endpoint cvpn-endpoint-000000000000000a1' — emulator-authored prose that interpolates the request's CIDR and the endpoint id into a message AWS keeps constant.
Why: same contract break as cause 6, and here the emulator also leaks request/state values into a message whose text is invariant on AWS, so no client can match it.
Fix: the RevokeClientVpnIngress handler's not-found refusal message.
Traces: ec2.op-contract-revoke-client-vpn-ingress.

## Evidence
Cause 1 — ec2.capres-cancel-with-a-running-instance-releases-it-without-stopping-the-guest:
  expected: a cancelled reservation releases every seat — AvailableInstanceCount == TotalInstanceCount, CapacityAllocations empty, GetCapacityReservationUsage.InstanceUsages == [].
  actual:   AssertionError: "a cancelled reservation must release every seat it held: AvailableInstanceCount must be back to TotalInstanceCount 1, got 0" (State had already reached 'cancelled').

Cause 2 — ec2.cpuopts-core-count-outside-valid-cores-rejected:
  expected: InvalidParameterValue, message "A value of <CoreCount> for CoreCount is not a valid value for the <InstanceType> instance type. Valid values are <ValidCores>."
  actual:   InvalidParameterValue, message 'Value (2) for parameter cpuOptions.coreCount is invalid. Valid values are 1'.

Cause 3 — ec2.cpuopts-partial-cpu-options-rejected:
  expected: InvalidParameterValue, message "When specifying CpuOptions you must specify both CoreCount and ThreadsPerCore."
  actual:   'InvalidParameterCombination': 'Both the core count and threads per core must be specified in the request'.

Cause 4 — ec2.cpuopts-guest-topology-differs-from-the-type-default (m5.large launched with ThreadsPerCore=1):
  expected: the guest reports LSCPU-CPUS, LSCPU-TPC and LSCPU-CPS from `lscpu`.
  actual:   AssertionError: "the guest did not report ['LSCPU-CPS', 'LSCPU-CPUS', 'LSCPU-TPC']; its stdout was 'GUEST-NPROC 1\nGUEST-ONLINE 0\nGUEST-PROCESSORS 1\nGUEST-SIBLINGS 0\nGUEST-LSCPU-CPUS \nGUEST-LSCPU-TPC \nGUEST-LSCPU-CPS \n'" — the invocation itself was Success/ResponseCode 0, so only lscpu is missing.

Cause 5 — ec2.op-contract-deleteClientVpnEndpoint:
  expected: InvalidClientVpnEndpointId.NotFound, message "Endpoint <id> does not exist".
  actual:   HTTP 200 and {'Status': {'Code': 'deleted'}} — AssertionError: "expected the call to be rejected, but it succeeded and returned {'Status': {'Code': 'deleted'}, ...'HTTPStatusCode': 200...}".

Cause 6 — ec2.op-contract-describe-ipam-resource-discoveries:
  expected: InvalidIpamResourceDiscoveryId.NotFound, message "The IPAM resource discovery (<id>) does not exist."
  actual:   same code, message "The ipam-resource-discovery ID 'ipam-res-disco-0000000000000000' does not exist".

Cause 7 — ec2.op-contract-revoke-client-vpn-ingress:
  expected: InvalidClientVpnEndpointAuthorizationRuleNotFound, message "Specified rule does not exist or cannot be deleted".
  actual:   same code, message 'There is no authorization rule for 10.223.0.0/24 in Client VPN endpoint cvpn-endpoint-000000000000000a1'.

## Steps to Reproduce
Replay the behavioral suite and re-run exactly these checks:
1. ec2.capres-cancel-with-a-running-instance-releases-it-without-stopping-the-guest — CreateCapacityReservation (t3.micro, Linux/UNIX, InstanceCount=1), RunInstances into it, CancelCapacityReservation, then DescribeCapacityReservations and GetCapacityReservationUsage once State == 'cancelled'.
2. ec2.cpuopts-core-count-outside-valid-cores-rejected — RunInstances on a type with CpuOptions.CoreCount outside DescribeInstanceTypes' VCpuInfo.ValidCores; read Error.Code/Message.
3. ec2.cpuopts-partial-cpu-options-rejected — RunInstances on m5.large with CpuOptions={'ThreadsPerCore': 1} and no CoreCount; read Error.Code/Message.
4. ec2.cpuopts-guest-topology-differs-from-the-type-default — launch m5.large with CpuOptions={'CoreCount': 1, 'ThreadsPerCore': 1}, then aws:RunShellScript `LC_ALL=C lscpu | grep '^CPU(s):'` (and the Thread(s)-per-core / Core(s)-per-socket lines) in the guest.
5. ec2.op-contract-deleteClientVpnEndpoint — CreateClientVpnEndpoint, DeleteClientVpnEndpoint, wait for the endpoint to be deleted/gone, then DeleteClientVpnEndpoint with the same id again.
6. ec2.op-contract-describe-ipam-resource-discoveries — DescribeIpamResourceDiscoveries(IpamResourceDiscoveryIds=['ipam-res-disco-0000000000000000']).
7. ec2.op-contract-revoke-client-vpn-ingress — on a client-VPN endpoint, RevokeClientVpnIngress(TargetNetworkCidr='10.223.0.0/24', RevokeAllGroups=True) for a CIDR that was never authorized.
