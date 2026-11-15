Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ec2.dpeer-bidirectional-traffic-needs-the-peer-route-on-both-sides ec2.droute-longest-prefix-match-decides-between-overlapping-route-targets ec2.op-contract-modify-vpc-peering-connection-options-notfound-rejected ec2.vpc-peering-connection-lifecycle
Issue-Shape-Id: com.amazonaws.ec2#AmazonEC2
Issue-Key: ec2.dpeer-bidirectional-traffic-needs-the-peer-route-on-both-sides|_json_line#5|AssertionError|assert lines, (
Issue-Key: ec2.droute-longest-prefix-match-decides-between-overlapping-route-targets|_json_line#5|AssertionError|assert lines, (
Issue-Key: ec2.op-contract-modify-vpc-peering-connection-options-notfound-rejected|run#13|AssertionError|assert err == "InvalidVpcPeeringConnectionID.NotFound", err
Issue-Key: ec2.vpc-peering-connection-lifecycle|run#33|AssertionError|assert (code, message) == expected, (
Issue-Commit: 015bd153266295538afd2721c5b6fd3d37297a70

# ec2: 3 net-new causes — in-guest commands return no output so VPC peering/route-table data-plane behaviour is unobservable, ModifyVpcPeeringConnectionOptions answers 501 NotImplemented, and AcceptVpcPeeringConnection on a deleted connection uses emulator-authored message text

> _Note: this issue was identified against the code base at commit `015bd153266295538afd2721c5b6fd3d37297a70`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ModifyVpcPeeringConnectionOptions

## Description
Three net-new root causes under the ec2 heading, all reached from VPC-peering / route-table scenarios.

Cause 1 of 3 — a command run inside a launched guest produces no output, so the VPC router's forwarding behaviour cannot be observed.
AWS: a launched instance that is an Online SSM managed node runs an AWS-RunShellScript document and the invocation returns the script's stdout; these scenarios use that stdout (a one-line JSON record, plus curl(1) exit statuses — 28 for a dropped packet, 7 for a refused connection) to decide whether a packet crossed a peering connection or followed the longest-prefix route. (Recorded behavior only: the EC2 model says nothing about guest execution.)
Emulator: the invocation ends in status `Failed` with an EMPTY stdout and an empty stderr, so the scenario fails on its first read of the guest: "the guest reported no stdout: 'Failed' stderr=''". Nothing about the peering/route data plane is exercised. NOTE: the open issue 20261115T003554Z-ec2-guests-observe-nothing-in-guest-commands-exit.md describes the same class of symptom for other ec2 checks, but it does not carry these two checks' replay keys, so it does not cover them.
Why: every client contract that depends on packets actually following VPC routes — bidirectional peering requiring a route on both sides, and a /24 beating a /16 for the same destination — is unverifiable; a wrong route table produces the same observation as a correct one.
Fix: the guest execution/data-plane side of the launched instance (the component that runs a managed node's shell document and returns its stdout), plus the VPC routing that those scripts probe.
Traces: ec2.dpeer-bidirectional-traffic-needs-the-peer-route-on-both-sides, ec2.droute-longest-prefix-match-decides-between-overlapping-route-targets

Cause 2 of 3 — ModifyVpcPeeringConnectionOptions is an unimplemented scaffold stub and answers HTTP 501 `NotImplemented` for every request.
AWS: ModifyVpcPeeringConnectionOptions sets the peering options on one side of a peering connection — for a same-account connection it enables DNS resolution for queries from the local VPC — and returns RequesterPeeringConnectionOptions / AccepterPeeringConnectionOptions reflecting the change (Smithy: ModifyVpcPeeringConnectionOptions input VpcPeeringConnectionId required, RequesterPeeringConnectionOptions / AccepterPeeringConnectionOptions of PeeringConnectionOptionsRequest; output PeeringConnectionOptions for each side). The change is then visible on the connection: DescribeVpcPeeringConnections reports RequesterVpcInfo.PeeringOptions.AllowDnsResolutionFromRemoteVpc == true. A VpcPeeringConnectionId that names no connection is refused with error code `InvalidVpcPeeringConnectionID.NotFound` and the message `The peering ID '<VpcPeeringConnectionId>' does not exist` (recorded behavior; the ec2Query model declares no modeled error shapes for this operation). Fixed text: "The peering ID '" … "' does not exist"; the quoted id is the value from the request.
Emulator: the handler returns the scaffold's 501 `NotImplemented` for both the refusal and the positive call, so the refusal class is misreported and the option is never settable or readable.
Why: a client enabling cross-VPC DNS resolution gets a 501 instead of a result, and its not-found handling never fires because the error code it branches on never arrives.
Fix: this operation's handler — services/service-com-amazonaws-ec2-amazonec2/src/ops/modify_vpc_peering_connection_options.rs still carries the SCAFFOLDED marker and returns `Scaffolded::not_implemented`; it must resolve the connection from state, refuse an unknown id with InvalidVpcPeeringConnectionID.NotFound and the message above, and otherwise apply and return the per-side options so DescribeVpcPeeringConnections reflects them.
Traces: ec2.op-contract-modify-vpc-peering-connection-options-notfound-rejected

Cause 3 of 3 — the InvalidStateTransition message for accepting a DELETED peering connection is emulator-authored prose.
AWS: AcceptVpcPeeringConnection on a connection whose Status.Code is `deleted` is refused with error code `InvalidStateTransition` and the message `Invalid state transition for <VpcPeeringConnectionId>, attempted to transition from deleted to provisioning` (recorded behavior; the model declares no error shapes for this operation). Fixed text: "Invalid state transition for " … ", attempted to transition from " <current state> " to " <target state>; the connection id and the two state names are values from the request and the emulator's state.
Emulator: the code is right but the message is a different sentence — `Invalid state transition for pcx-…, state: deleted` — it omits the "attempted to transition from X to Y" clause entirely.
Why: the message is part of the refusal contract; a client that surfaces or matches on it (or a test fixture asserting it) diverges from AWS, and the omitted clause is the only part of the refusal that says which transition was attempted.
Fix: the AcceptVpcPeeringConnection handler's state-transition refusal — its message formatting, not the code it returns.
Traces: ec2.vpc-peering-connection-lifecycle

## Evidence
Cause 1 — Expected: the AWS-RunShellScript invocation inside the launched guest returns its stdout (a JSON line; curl exit 28 for a dropped packet, 7 for a refused connection). Actual: AssertionError "the guest reported no stdout: 'Failed' stderr=''" from both ec2.dpeer-bidirectional-traffic-needs-the-peer-route-on-both-sides and ec2.droute-longest-prefix-match-decides-between-overlapping-route-targets.

Cause 2 — Expected: modify_vpc_peering_connection_options(VpcPeeringConnectionId='pcx-<nonexistent>', RequesterPeeringConnectionOptions={'AllowDnsResolutionFromRemoteVpc': True}) raises error code 'InvalidVpcPeeringConnectionID.NotFound' with message "The peering ID '<that id>' does not exist"; and on the live active connection the call returns RequesterPeeringConnectionOptions.AllowDnsResolutionFromRemoteVpc == True, after which DescribeVpcPeeringConnections reports RequesterVpcInfo.PeeringOptions.AllowDnsResolutionFromRemoteVpc == True. Actual: AssertionError 'NotImplemented' — the operation answers HTTP 501 NotImplemented (handler still the scaffold stub).

Cause 3 — Expected: ('InvalidStateTransition', "Invalid state transition for <pcx id>, attempted to transition from deleted to provisioning"). Actual: ('InvalidStateTransition', 'Invalid state transition for <pcx id>, state: deleted').

## Steps to Reproduce
Cause 1: create two VPCs with a peering connection (and, for the route check, a /16 and a /24 covering the same destination with different targets), launch t3.micro guests with an instance profile carrying AmazonSSMManagedInstanceCore, then run AWS-RunShellScript in a guest and read StandardOutputContent — the invocation ends Failed with empty stdout and stderr instead of returning the probe's JSON line.

Cause 2: create two VPCs, CreateVpcPeeringConnection, AcceptVpcPeeringConnection, wait for Status.Code 'active'. (a) Call ModifyVpcPeeringConnectionOptions with VpcPeeringConnectionId='pcx-00000000000000000' and RequesterPeeringConnectionOptions={'AllowDnsResolutionFromRemoteVpc': True} — observe NotImplemented (501) instead of InvalidVpcPeeringConnectionID.NotFound with "The peering ID 'pcx-00000000000000000' does not exist". (b) Call it with the real connection id — observe 501 instead of the returned per-side options and the updated PeeringOptions on DescribeVpcPeeringConnections.

Cause 3: with the same active connection, DeleteVpcPeeringConnection and wait for Status.Code 'deleted', then call AcceptVpcPeeringConnection on it — the code is InvalidStateTransition but the message reads 'Invalid state transition for <pcx id>, state: deleted' instead of 'Invalid state transition for <pcx id>, attempted to transition from deleted to provisioning'.
