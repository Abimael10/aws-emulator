Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ec2.ec2mr-pip-assign-private-ip-held-by-another-interface-rejected ec2.ec2mr-pip-unassigning-a-secondary-ip-frees-room-under-the-instance-type-limit ec2.secip-secondary-private-ip-is-reachable-from-a-peer-only-when-the-eni-holds-it ec2.secip-assigning-more-private-ips-than-the-instance-type-permits-rejected
Issue-Shape-Id: com.amazonaws.ec2#AmazonEC2
Issue-Key: ec2.ec2mr-pip-assign-private-ip-held-by-another-interface-rejected|run#16|AssertionError|assert err == want, (
Issue-Key: ec2.ec2mr-pip-unassigning-a-secondary-ip-frees-room-under-the-instance-type-limit|_error#17|AssertionError|raise AssertionError("the call succeeded; real AWS refuses it")
Issue-Key: ec2.secip-secondary-private-ip-is-reachable-from-a-peer-only-when-the-eni-holds-it|_run_guest#21|AssertionError|assert inv["Status"] == "Success", (
Issue-Key: ec2.secip-assigning-more-private-ips-than-the-instance-type-permits-rejected|_poll#28|Timeout|time.sleep(interval_s)
Issue-Commit: 68169c86c7603a2902f84035dfc94fd02b30ddfc

# EC2 private-IP inventory: 4 net-new causes — AssignPrivateIpAddresses mis-classifies a cross-interface move, ignores the instance type's Ipv4AddressesPerInterface limit, and a launched guest presents no NIC matching its primary ENI's MacAddress

> _Note: this issue was identified against the code base at commit `68169c86c7603a2902f84035dfc94fd02b30ddfc`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
AssignPrivateIpAddresses

## Description
Four net-new root causes in EC2's secondary-private-IPv4 surface, all in the `ec2` service. None of the four failing checks is marked ALREADY FILED or OWNED by an open issue in this replay. Number of causes to close before submitting: 4.

=== Cause 1 of 4: AssignPrivateIpAddresses refuses a cross-interface move with the wrong error code and message ===
Failing check ids: ec2.ec2mr-pip-assign-private-ip-held-by-another-interface-rejected

AWS: AssignPrivateIpAddresses takes an explicit `PrivateIpAddresses` list plus an `AllowReassignment` flag (Smithy: AssignPrivateIpAddresses input members `NetworkInterfaceId` (required), `PrivateIpAddresses` -> PrivateIpAddressStringList, `SecondaryPrivateIpAddressCount`, `AllowReassignment`; the ec2Query protocol declares no modeled error shapes for this operation, so the code and message are recorded behavior only). The recording shows that when the requested address is the PRIMARY address of another network interface in the same subnet — an address that can never be moved, with or without AllowReassignment — AWS refuses with HTTP 400, error code `InvalidParameterValue`, message `[<address>] assigned, but move is not allowed.`  Fixed text of that message: the bracketed list, then the literal ` assigned, but move is not allowed.` Variable part: `<address>` is the address (or comma-separated addresses) taken verbatim from the request's `PrivateIpAddresses` member. This refusal class is distinct from the taken-but-movable case, which AWS reports as `InvalidIPAddress.InUse`.

Emulator: answers HTTP 400 `InvalidIPAddress.InUse` with message `Address <address> is in use.` — i.e. it collapses "this address belongs to another interface as its primary and is therefore immovable" into the generic "address already in use" class. Replay detail: `moving '10.61.1.4' from eni-...09a to eni-...09b answered ('InvalidIPAddress.InUse', 'Address 10.61.1.4 is in use.'), expected ('InvalidParameterValue', '[10.61.1.4] assigned, but move is not allowed.')`. (The address and the two interface ids are per-run values that fill the template; they are not part of the requirement.) The emulator's own handler doc in services/service-com-amazonaws-ec2-amazonec2/src/ops/assign_private_ip_addresses.rs already states that AllowReassignment admits only a SECONDARY address of another interface and that an interface's primary address cannot be moved away, so the distinction is understood but is not represented on the wire.

Why: a client that branches on the error code cannot tell a permanently-immovable address (retrying with AllowReassignment=true is pointless) from a movable one (retry with AllowReassignment=true succeeds). IP-address-management tooling and CNI plugins that key on `InvalidIPAddress.InUse` will loop or escalate on a request AWS classifies as a plain parameter error.

Fix: the refusal classification in the AssignPrivateIpAddresses handler's validation of the `PrivateIpAddresses` member — the branch that decides between the immovable-primary case and the in-use-secondary case.

=== Cause 2 of 4: AssignPrivateIpAddresses does not enforce the instance type's Ipv4AddressesPerInterface limit ===
Failing check ids: ec2.ec2mr-pip-unassigning-a-secondary-ip-frees-room-under-the-instance-type-limit

AWS: the number of private IPv4 addresses one network interface may hold is the `NetworkInfo.Ipv4AddressesPerInterface` value DescribeInstanceTypes reports for the instance type the interface is attached to, counting the primary address. An assignment (by explicit address or by `SecondaryPrivateIpAddressCount`) that would take the interface past that number is refused with HTTP 400, error code `PrivateIpAddressLimitExceeded`, message `Number of private addresses will exceed limit.` — that message is entirely fixed text, with no variable part. The limit is counted against the addresses the interface holds at the time of the call, so unassigning one secondary address frees exactly one slot and the next assignment succeeds. (Smithy: no modeled errors on this ec2Query operation; the code/message are recorded behavior only. The limit itself is a modeled value: DescribeInstanceTypes -> InstanceTypeInfo$NetworkInfo$Ipv4AddressesPerInterface.)

Emulator: an interface already holding exactly `Ipv4AddressesPerInterface` addresses accepts one more — the call returns 200 with a granted address instead of refusing. Replay detail: `AssertionError: the call succeeded; real AWS refuses it`, raised where the check expects `('PrivateIpAddressLimitExceeded', 'Number of private addresses will exceed limit.')` after filling the interface of a running instance to the limit the emulator's own DescribeInstanceTypes declared for that type. The handler doc claims this refusal exists, so the defect is that the ceiling it compares against is not the attached instance type's Ipv4AddressesPerInterface.

Why: a client relying on AWS's ceiling gets an over-permissive emulator: capacity planning and address-exhaustion handling are never exercised, an interface accumulates more addresses than any real instance type permits, and code paths that treat `PrivateIpAddressLimitExceeded` as the signal to attach another ENI never run. The accepted-but-impossible addresses then diverge from what the guest can ever configure.

Fix: the limit check in the AssignPrivateIpAddresses handler's validation — it must resolve the attached instance's type and compare the resulting address count against that type's Ipv4AddressesPerInterface, and must count the addresses the interface holds now (so an UnassignPrivateIpAddresses frees exactly one slot).

=== Cause 3 of 4: a launched guest presents no network interface whose MAC is the MacAddress the control plane reports for its primary ENI ===
Failing check ids: ec2.secip-secondary-private-ip-is-reachable-from-a-peer-only-when-the-eni-holds-it

AWS: DescribeNetworkInterfaces reports `NetworkInterface$MacAddress` for the interface attached to a running instance, and that is the MAC the guest kernel presents for the corresponding device — so a guest can resolve its own interface by scanning /sys/class/net/*/address for that value, and the same MAC appears under IMDS at network/interfaces/macs/. Recorded behavior only for the guest-side presentation; the model side is DescribeNetworkInterfaces -> NetworkInterface$MacAddress.

Emulator: the in-guest script finds no device whose address matches the ENI's reported MacAddress; it prints `SIM2REAL-CFG iface=none` and exits 1, so the SSM invocation ends `Failed` with rc=1. Replay detail: `AssertionError: the target configures both addresses on its primary interface: the guest script was 'Failed' ('Failed'); rc=1 stdout='SIM2REAL-CFG iface=none\n' stderr=''`. Because the guest cannot name its own interface, nothing downstream of it (configuring a granted secondary address in the kernel, reading local-ipv4s back from IMDS, reaching that address from a peer) can be exercised.

Why: any client that correlates control-plane ENI identity with guest network state — bootstrap scripts that bind a service to a specific ENI, multi-ENI routing setups, CNI/ENI-per-pod agents, IMDS-driven address discovery — breaks: the MAC the API advertises names no device inside the instance, so the two views of the same interface cannot be joined.

Fix: the data plane that materialises a launched instance's guest NIC from its primary ENI record — the guest presentation of the network-interface attachment must use the same MacAddress the EC2 state holds and DescribeNetworkInterfaces serializes. Note: the open issues 20261115T003554Z-ec2-guest-hardware-is-not-presented-instance-store.md and 20261115T003554Z-ec2-guests-observe-nothing-in-guest-commands-exit.md look adjacent (guest-side presentation) but neither carries this check's replay key, so this cause is filed here; a fixer should check whether a single guest-presentation change closes all three.

=== Cause 4 of 4: the Ipv4AddressesPerInterface refusal check on a running m5.large never completes ===
Failing check ids: ec2.secip-assigning-more-private-ips-than-the-instance-type-permits-rejected

AWS: the same requirement as Cause 2, exercised end to end on a running instance — filling the primary ENI of an m5.large to the `Ipv4AddressesPerInterface` value DescribeInstanceTypes declares succeeds and is visible in the guest's IMDS under network/interfaces/macs/<mac>/local-ipv4s, and one more address is refused with HTTP 400 `PrivateIpAddressLimitExceeded` / `Number of private addresses will exceed limit.` with no address added in the control plane or in the guest.

Emulator: the check never reaches that assertion — the replay records only `Error: Check timed out after 60.0s`, with no emulator response captured, while the check's own internal waits (instance reaching `running`, SSM coming online) are budgeted at 600 s. Hypothesis: the launch-to-reachable path the check depends on (instance reaching `running` and its managed node coming online) does not converge quickly enough for any assertion to run; the recorded evidence states only the timeout, so the mechanism is not established by this trace.

Why: the guest-visible half of the per-interface IPv4 limit is never verified, so even after Cause 2 is fixed in the control plane a client reading its addresses from IMDS has no coverage that the two agree.

Fix: whichever component Causes 2 and 3 land in — re-replay this check after those two are fixed; if it still times out with no response, the remaining work is in the launch/managed-node convergence path, not in AssignPrivateIpAddresses itself.

## Evidence
Replay report, service heading `### ec2` (all four checks are listed under it; none carries an ALREADY FILED or OWNED-by marker).

Cause 1 — ec2.ec2mr-pip-assign-private-ip-held-by-another-interface-rejected [mismatch]:
  Expected (AWS): HTTP 400, ('InvalidParameterValue', '[<address>] assigned, but move is not allowed.')
  Actual (emulator): HTTP 400, ('InvalidIPAddress.InUse', 'Address <address> is in use.')
  Verbatim: "moving '10.61.1.4' from eni-0000000000000009a to eni-0000000000000009b answered ('InvalidIPAddress.InUse', 'Address 10.61.1.4 is in use.'), expected ('InvalidParameterValue', '[10.61.1.4] assigned, but move is not allowed.')"
  Positive controls in the same check pass/are unreached: re-assigning an address the interface already holds is a no-op, and an unowned in-CIDR address is accepted.

Cause 2 — ec2.ec2mr-pip-unassigning-a-secondary-ip-frees-room-under-the-instance-type-limit [mismatch]:
  Setup: t3.micro; limit read from DescribeInstanceTypes NetworkInfo.Ipv4AddressesPerInterface; interface filled with limit-1 secondaries so it holds exactly `limit` addresses.
  Expected (AWS): HTTP 400, ('PrivateIpAddressLimitExceeded', 'Number of private addresses will exceed limit.')
  Actual (emulator): HTTP 200 — the assignment succeeded. Verbatim: "AssertionError: the call succeeded; real AWS refuses it" at `ec2.assign_private_ip_addresses(NetworkInterfaceId=nic, SecondaryPrivateIpAddressCount=1)`.

Cause 3 — ec2.secip-secondary-private-ip-is-reachable-from-a-peer-only-when-the-eni-holds-it [mismatch]:
  Guest script resolves its device by matching /sys/class/net/*/address against the MacAddress from DescribeNetworkInterfaces for the instance's primary ENI.
  Expected (AWS): a device matches; the script prints `SIM2REAL-CFG iface=<dev>` and exits 0.
  Actual (emulator): no device matches. Verbatim: "AssertionError: the target configures both addresses on its primary interface: the guest script was 'Failed' ('Failed'); rc=1 stdout='SIM2REAL-CFG iface=none\n' stderr=''".

Cause 4 — ec2.secip-assigning-more-private-ips-than-the-instance-type-permits-rejected [mismatch]:
  Expected (AWS): fill m5.large's primary ENI to Ipv4AddressesPerInterface (succeeds, visible in IMDS local-ipv4s), then one more refused ('PrivateIpAddressLimitExceeded', 'Number of private addresses will exceed limit.') with no address added.
  Actual (emulator): no response recorded — "Error: Check timed out after 60.0s", while the check's own instance-running and SSM-online waits allow 600 s.

Model citations used: Smithy (services/service-com-amazonaws-ec2-amazonec2/smithy-model/com-amazonaws-ec2-amazonec2.json) — AssignPrivateIpAddresses input {NetworkInterfaceId required, PrivateIpAddresses: PrivateIpAddressStringList, SecondaryPrivateIpAddressCount, AllowReassignment, Ipv4Prefixes, Ipv4PrefixCount}, output {NetworkInterfaceId, AssignedPrivateIpAddresses, AssignedIpv4Prefixes}, errors: [] (ec2Query — untyped errors, so codes/messages are recorded behavior only); UnassignPrivateIpAddresses input {NetworkInterfaceId required, PrivateIpAddresses, Ipv4Prefixes}, output Unit, errors: []; DescribeInstanceTypes -> InstanceTypeInfo$NetworkInfo$Ipv4AddressesPerInterface supplies the per-interface ceiling. Emulator file consulted for Causes 1-2: services/service-com-amazonaws-ec2-amazonec2/src/ops/assign_private_ip_addresses.rs lines 18-39 (its documented refusal list already names both InvalidIPAddress.InUse-vs-immovable-primary and PrivateIpAddressLimitExceeded).

## Steps to Reproduce
All against the emulator's EC2 endpoint; re-replay the four check ids in trace_ids.

Cause 1:
1. CreateVpc 10.61.0.0/16, CreateSubnet 10.61.1.0/28, create a security group.
2. CreateNetworkInterface in that subnet -> interface A; note A's PrivateIpAddress (its primary) = P.
3. CreateNetworkInterface in the same subnet with SecondaryPrivateIpAddressCount=1 -> interface B (holds 2 addresses, neither equal to P).
4. AssignPrivateIpAddresses(NetworkInterfaceId=B, PrivateIpAddresses=[P]) with AllowReassignment unset.
   Expect HTTP 400 InvalidParameterValue with message "[P] assigned, but move is not allowed."; observe HTTP 400 InvalidIPAddress.InUse "Address P is in use.".
5. Controls that must keep working: AssignPrivateIpAddresses(B, [an address B already holds]) is a 200 no-op; AssignPrivateIpAddresses(B, [a free in-CIDR address]) is accepted and appears on B.

Cause 2:
1. DescribeInstanceTypes(InstanceTypes=['t3.micro']) -> L = NetworkInfo.Ipv4AddressesPerInterface.
2. RunInstances a t3.micro into a subnet; wait for `running`; take the primary ENI id N (it holds only its primary).
3. AssignPrivateIpAddresses(N, SecondaryPrivateIpAddressCount=L-1) -> N now holds exactly L addresses (confirm with DescribeNetworkInterfaces).
4. AssignPrivateIpAddresses(N, SecondaryPrivateIpAddressCount=1).
   Expect HTTP 400 PrivateIpAddressLimitExceeded "Number of private addresses will exceed limit." and N unchanged; observe HTTP 200 with a granted address.
5. UnassignPrivateIpAddresses(N, PrivateIpAddresses=[one secondary]) then AssignPrivateIpAddresses(N, SecondaryPrivateIpAddressCount=1) must succeed once, and the call after that must be refused again.

Cause 3:
1. RunInstances an m5.large with an SSM-capable instance profile into a public subnet; wait for `running` and for the managed node to come online.
2. DescribeNetworkInterfaces for the instance's primary ENI -> MacAddress M.
3. SSM SendCommand AWS-RunShellScript on the instance: `for d in /sys/class/net/*; do [ "$(cat $d/address)" = "M" ] && echo ${d##*/}; done`.
   Expect one device name; observe no match (the check's equivalent script prints `iface=none` and exits 1, so the invocation ends Failed with rc=1).

Cause 4:
1. Re-replay ec2.secip-assigning-more-private-ips-than-the-instance-type-permits-rejected after Causes 2 and 3 are fixed.
   It currently produces no emulator response at all — "Check timed out after 60.0s" — before any assertion runs.
