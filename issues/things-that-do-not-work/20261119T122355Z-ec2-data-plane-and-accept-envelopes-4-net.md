Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ec2.capres-cancel-with-a-running-instance-releases-it-without-stopping-the-guest ec2.capres-launch-of-a-mismatched-instance-type-into-a-targeted-reservation-rejected ec2.dpeer-bidirectional-traffic-needs-the-peer-route-on-both-sides ec2.dpeer-overlapping-cidr-peering-fails-and-never-carries-traffic ec2.dpeer-route-is-blackhole-until-the-peering-is-accepted ec2.capres-targeted-reservation-consumed-by-a-guest-that-really-boots ec2.op-contract-search-transit-gateway-multicast-groups
Issue-Shape-Id: com.amazonaws.ec2#AmazonEC2
Issue-Key: ec2.capres-cancel-with-a-running-instance-releases-it-without-stopping-the-guest|run#23|ValueError|assert int(before["CPUS"]) == _MICRO_VCPUS, (
Issue-Key: ec2.capres-launch-of-a-mismatched-instance-type-into-a-targeted-reservation-rejected|run#34|ValueError|assert int(facts["CPUS"]) == _MICRO_VCPUS, (
Issue-Key: ec2.dpeer-bidirectional-traffic-needs-the-peer-route-on-both-sides|_json_line#5|JSONDecodeError|return json.loads(lines[-1])
Issue-Key: ec2.dpeer-overlapping-cidr-peering-fails-and-never-carries-traffic|_json_line#5|JSONDecodeError|return json.loads(lines[-1])
Issue-Key: ec2.dpeer-route-is-blackhole-until-the-peering-is-accepted|_json_line#6|JSONDecodeError|return json.loads(lines[-1])
Issue-Key: ec2.capres-targeted-reservation-consumed-by-a-guest-that-really-boots|run#15|AssertionError|assert "CapacityReservationId" not in inst, (
Issue-Key: ec2.op-contract-search-transit-gateway-multicast-groups|_poll_until#19|Timeout|time.sleep(interval_s)
Issue-Commit: 20b40f42cd78b95666b130bae2475dd4b1dad646

# EC2 data plane and accept envelopes: 4 net-new causes — in-guest Run Command probes return no usable output, peering probes print no JSON, RunInstances leaks CapacityReservationId, multicast-group flow never settles

> _Note: this issue was identified against the code base at commit `20b40f42cd78b95666b130bae2475dd4b1dad646`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
RunInstances

## Description
Four net-new root causes grounded in failing behavioral checks that the replay report marks neither ALREADY FILED nor OWNED. Open issues cover neighbouring checks of the same families but not these replay keys: 20261115T003554Z-ec2-guests-observe-nothing-in-guest-commands-exit.md (guest command output) and 20261105T224104Z-ec2-10-net-new-behavioral-root-causes-peering.md (VPC peering).

Cause 1 of 4: an in-guest hardware probe run through Run Command returns empty values for commands that always print something.
AWS: a launched, managed Linux instance executes the shell the command document sends and returns its stdout verbatim in GetCommandInvocation.StandardOutputContent with ResponseCode 0. `nproc` prints the instance type's vCPU count (2 for a t3.micro/t4g.micro-class burstable), `cat /proc/sys/kernel/random/boot_id` prints a UUID, `awk '/MemTotal/{print $2}' /proc/meminfo` prints a positive integer, and the IMDS reads (instance-id, instance-type, placement/availability-zone) echo the instance's own identity. Recorded behavior only — these are properties of a booted guest, not of the API model.
Emulator: the invocation completes but the probe's markers are EMPTY — the checks parsed `GUEST-CPUS` as '' and failed with `ValueError: invalid literal for int() with base 10: ''`. The guest produced no value for `nproc` (and the same probe's other `/proc` reads), so nothing in the guest actually executed the script.
Why: every capability that is observed from inside the instance is unverifiable — a client cannot confirm the machine it was given has the vCPUs, memory, boot identity or placement the control plane claims, and any automation that reads a value out of a Run Command's stdout gets an empty string instead of a failure.
Fix: the guest execution path for Run Command invocations in the emulator's EC2 instance data plane (the component that boots the guest and executes/collects the command's stdout), not the control-plane handler.
Traces: ec2.capres-cancel-with-a-running-instance-releases-it-without-stopping-the-guest, ec2.capres-launch-of-a-mismatched-instance-type-into-a-targeted-reservation-rejected

Cause 2 of 4: VPC-peering data-plane probes never see the JSON line the probe script prints, so no peered traffic can be observed.
AWS: a guest in VPC A with an accepted peering connection and a route to VPC B's CIDR reaches a listener on the peer's private address; the probe script runs `curl` against the peer and prints a single JSON object on its last stdout line (HTTP code plus body), and the absence/presence of a route, an accepted peering, or an overlapping CIDR decides whether that JSON reports success or a timeout. Recorded behavior only — this is guest-observable connectivity, not a modelled response.
Emulator: the invocation's stdout has content but its last line is not the JSON the script prints — `json.decoder.JSONDecodeError: Expecting value: line 1 column 1 (char 0)` in all three checks. The guest never ran the probe script as written, so blackhole-until-accepted, bidirectional-route and overlapping-CIDR behaviour cannot be observed at all.
Why: a client cannot tell whether peering actually carries packets; a peering misconfiguration (missing return route, unaccepted request, overlapping CIDRs) looks identical to a working one from the API, and all three negative semantics silently pass as unobservable.
Fix: the same guest execution/stdout collection path in the EC2 instance data plane, plus the VPC peering data path it exercises.
Traces: ec2.dpeer-bidirectional-traffic-needs-the-peer-route-on-both-sides, ec2.dpeer-overlapping-cidr-peering-fails-and-never-carries-traffic, ec2.dpeer-route-is-blackhole-until-the-peering-is-accepted

Cause 3 of 4: the RunInstances accept envelope already claims a Capacity Reservation is consumed.
AWS: consumption of a Capacity Reservation is an effect observed AFTER the accept — the Instance structure in the RunInstances response does not carry `CapacityReservationId`; it appears later on DescribeInstances once the instance is placed into the reservation, and the reservation's own AvailableInstanceCount/UsedInstanceCount move then. Smithy: Instance DOES model `CapacityReservationId` (xmlName capacityReservationId) and `CapacityReservationSpecification`, so the member is legal on the shape — what diverges is populating it in the launch response. Recorded behavior only.
Emulator: the Instance in the RunInstances response for a launch targeting a reservation includes `CapacityReservationId`.
Why: a client that treats the presence of CapacityReservationId in the accept as proof the reservation was consumed skips the read-back that real AWS requires, and bookkeeping derived from the accept disagrees with the reservation's counters.
Fix: the RunInstances response serializer in the ec2 run_instances handler (omit CapacityReservationId from the launch envelope; publish it from DescribeInstances once placement is recorded).
Traces: ec2.capres-targeted-reservation-consumed-by-a-guest-that-really-boots

Cause 4 of 4: the SearchTransitGatewayMulticastGroups flow never settles, so the check is killed at its 60 s budget.
AWS: a transit gateway reaches `available`, a VPC attachment reaches `available`, a multicast domain reaches `available`, a subnet association reaches `associated`, and a registered network interface then shows up as a static group member for its group IP through SearchTransitGatewayMulticastGroups; an unknown multicast domain id draws a NotFound refusal. Recorded behavior only.
Emulator: the check never reaches any of its assertions — the replay reports `Check timed out after 60.0s`, while sibling op-contract checks that poll far longer budgets complete. Hypothesis: one of the resources in this chain never leaves its creating/pending state (or a poll never satisfies), so the check spends its whole budget polling.
Why: a client building transit gateway multicast cannot observe a group member at all, and a resource stuck short of `available` stalls every caller that waits on it.
Fix: the transit gateway multicast state machine in the ec2 service state (domain/association state progression) and the SearchTransitGatewayMulticastGroups handler.
Traces: ec2.op-contract-search-transit-gateway-multicast-groups

## Evidence
All quotes are from the behavioral replay's failing-check details.

Cause 1 — ec2.capres-cancel-with-a-running-instance-releases-it-without-stopping-the-guest and ec2.capres-launch-of-a-mismatched-instance-type-into-a-targeted-reservation-rejected: `assert int(before["CPUS"]) == _MICRO_VCPUS` → `ValueError: invalid literal for int() with base 10: ''`. The probe the guest was sent prints `echo "GUEST-CPUS $(nproc)"`, `echo "GUEST-BOOTID $(cat /proc/sys/kernel/random/boot_id)"`, `echo "GUEST-MEMKB $(awk '/MemTotal/{print $2}' /proc/meminfo)"` and three IMDS reads; the harness collected the invocation but every marker value was empty. Expected: CPUS = the type's vCPU count (2), BOOTID a UUID, MEMKB a positive integer.

Cause 2 — ec2.dpeer-bidirectional-traffic-needs-the-peer-route-on-both-sides, ec2.dpeer-overlapping-cidr-peering-fails-and-never-carries-traffic, ec2.dpeer-route-is-blackhole-until-the-peering-is-accepted: all three fail identically with `json.decoder.JSONDecodeError: Expecting value: line 1 column 1 (char 0)` from the helper that reads the LAST non-blank stdout line of the invocation (`_json_line`) after the probe script runs `curl -s -m 8 -o /tmp/body -w '%{http_code}' http://<peer-ip>:<port>/nonce`. Expected: that last line is a JSON object describing the HTTP code and body.

Cause 3 — ec2.capres-targeted-reservation-consumed-by-a-guest-that-really-boots: AssertionError "the RunInstances accept envelope must not claim the reservation is consumed — consumption is an effect observed afterwards, not part of the accept — got {... 'CapacityReservationId': ...}" for a RunInstances with CapacityReservationSpecification.CapacityReservationTarget.CapacityReservationId set. Expected: no CapacityReservationId member in the RunInstances Instance; the id appears on DescribeInstances and the reservation's AvailableInstanceCount drops to 0 afterwards.

Cause 4 — ec2.op-contract-search-transit-gateway-multicast-groups: `Error: Check timed out after 60.0s`, with no assertion reached. The check's own polls allow 300 s (transit gateway available), 600 s (VPC attachment available), 120 s (multicast domain available), 180 s (subnet association associated) and 90 s (member visible via SearchTransitGatewayMulticastGroups).

## Steps to Reproduce
Replay the behavioral suite and re-run exactly these checks:
1. ec2.capres-cancel-with-a-running-instance-releases-it-without-stopping-the-guest / ec2.capres-launch-of-a-mismatched-instance-type-into-a-targeted-reservation-rejected — launch a burstable instance into a Capacity Reservation, then send the in-guest probe via Run Command and read GetCommandInvocation.StandardOutputContent; every GUEST-* marker (CPUS, MEMKB, BOOTID, IID, TYPE, AZ) must carry the guest's real value.
2. ec2.dpeer-route-is-blackhole-until-the-peering-is-accepted / ec2.dpeer-bidirectional-traffic-needs-the-peer-route-on-both-sides / ec2.dpeer-overlapping-cidr-peering-fails-and-never-carries-traffic — build two VPCs, peer them, run the curl probe from a guest in A against a listener in B; the invocation's last stdout line must be the JSON object the script prints, and must report unreachable before the peering is accepted or without the return route, and reachable once both routes exist.
3. ec2.capres-targeted-reservation-consumed-by-a-guest-that-really-boots — RunInstances with CapacityReservationSpecification targeting a reservation id; the returned Instance must NOT contain CapacityReservationId, while DescribeInstances afterwards must, and the reservation's AvailableInstanceCount must reach 0.
4. ec2.op-contract-search-transit-gateway-multicast-groups — create a transit gateway with multicast support, a VPC attachment, a multicast domain, associate the subnet, register an ENI as a static group member, then SearchTransitGatewayMulticastGroups; each resource must reach available/associated and the member must be listed, and an unknown domain id must draw a NotFound refusal.
