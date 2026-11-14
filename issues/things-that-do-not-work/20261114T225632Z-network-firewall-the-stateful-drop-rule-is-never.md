Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: network-firewall.stateful-rule-drops-matching-traffic-allows-other
Issue-Shape-Id: com.amazonaws.networkfirewall#NetworkFirewall_20201112
Issue-Key: network-firewall.stateful-rule-drops-matching-traffic-allows-other|_poll_for_tag#69|Timeout|time.sleep(interval_s)
Issue-Commit: 190e7b20abd2a451df846a0932a8de897636f1f9

# network-firewall: the stateful DROP rule is never enforced on the firewall endpoint's data plane — the packet-level check reaches no verdict

> _Note: this issue was identified against the code base at commit `190e7b20abd2a451df846a0932a8de897636f1f9`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateFirewall

## Description
Cause 1 of 1: a firewall the emulator reports READY and routed enforces nothing observable on packets, and the behavioral check that proves it never reaches a verdict.
Failing check: network-firewall.stateful-rule-drops-matching-traffic-allows-other

AWS: recorded behavior only (the Network Firewall Smithy model describes the control plane — CreateRuleGroup/CreateFirewallPolicy/CreateFirewall/DescribeFirewall and the SyncStates endpoint id — but says nothing about packet handling). The recorded scenario creates a STATEFUL rule group whose StatefulRules carry Action=DROP with Header {Protocol: TCP, Source: ANY, SourcePort: ANY, Direction: FORWARD, Destination: <target ip>/32, DestinationPort: "8080"}, a firewall policy whose StatelessDefaultActions/StatelessFragmentDefaultActions are aws:forward_to_sfe and which references that rule group, and a firewall on the firewall subnet; it polls DescribeFirewall to READY, reads FirewallStatus.SyncStates.<az>.Attachment.EndpointId, and points the workload and target route tables at that endpoint for each other's subnet CIDR. On AWS the workload instance's probe then observes tcp/8081 to the target as reachable and tcp/8080 to the same target as blocked, and records that by tagging itself: sim2real:port8081=ALLOWED, sim2real:port8080=BLOCKED, sim2real:ready=1, visible through DescribeInstances inside the check's 300 s poll.

Emulator: the check produces no verdict at all — replay reports "Check timed out after 60.0s". No sim2real:* tag is ever observed, so neither the DROP of tcp/8080 nor the pass of tcp/8081 through the firewall endpoint is ever demonstrated: the firewall exists in the control plane (READY, with an endpoint id in SyncStates) while the endpoint it advertises filters nothing that any client can see. Hypothesis (neither the model nor the replay output states which call stalls): the terminal wait is _poll_for_tag on the workload instance and the emulator's launched guests never execute their user-data, so the tags are never written; the earlier waits in the same check (vpc_available, VPC-endpoint `available`, firewall READY, instance_running, and the CreateRoute calls at the endpoint) are the other candidates.

Why: a client cannot distinguish a working Network Firewall deployment from an inert one. Everything the service exists for — a stateful rule actually dropping matching packets while unmatched traffic passes — is unverifiable, so any user or CI suite validating a firewall policy against the emulator either hangs or passes a deployment that would silently carry blocked traffic. Routing traffic at FirewallStatus.SyncStates.<az>.Attachment.EndpointId is the documented way to interpose the firewall; if that endpoint is not a filtering data path, the whole control-plane fidelity above it is unusable as a test target.

Fix: the Network Firewall endpoint data path and the state it is driven from — packets crossing an endpoint named as a route target must be evaluated against the attached policy's stateless default actions and its referenced stateful rule groups (DROP on a matching 5-tuple, pass otherwise) — together with the EC2 instance-guest execution the scenario observes that through (a launched instance must run its user-data so the self-tagging converges inside the poll window). Related open issues that do NOT cover this (different service headings): 20261113T123420Z-ec2-data-plane-a-launched-guest-never-runs.md and 20261023T132856Z-network-firewall-a-ready-firewall-s-syncstates-endpoints-are-not.md (the latter is about SyncStates endpoints not being registered as EC2 VPC endpoints, not about packet enforcement).

Traces: network-firewall.stateful-rule-drops-matching-traffic-allows-other.

## Evidence
Replay of suite simparity-behavioral-aws-v1 (emulator serving; network-firewall 24/25 passed, 1 failed).

[mismatch] network-firewall.stateful-rule-drops-matching-traffic-allows-other
Expected: the check completes and DescribeInstances on the workload instance reports tags sim2real:port8080=BLOCKED (the stateful DROP rule for tcp/8080 to <target ip>/32), sim2real:port8081=ALLOWED (unfiltered port to the same host) and sim2real:ready=1.
Actual: "Check timed out after 60.0s" — the check never reaches any of its three assertions, so no tag value is produced. Its terminal wait is _poll_for_tag(ec2, workload_id, "sim2real:ready", timeout_s=300) (which would otherwise raise "instance never reported tag sim2real:ready within 300s"); the preceding waits are _wait_for_vpc_endpoint_available(..., timeout_s=300), _wait_for_ready(nfw, firewall_arn, timeout_s=900) and the instance_running waiters.
Note: the same emulator run reaches READY and reads SyncStates.<az>.Attachment.EndpointId successfully in ec2__network-firewall.nfwr-routed-firewall-endpoint-refusals-rejected, so firewall provisioning itself is not the stall; the unmet observable is the packet-level verdict carried by the instance tags.

## Steps to Reproduce
1. git_clone the repo and boot the emulator built from it.
2. Run the behavioral suite (simparity-behavioral-aws-v1).
3. Replay check network-firewall.stateful-rule-drops-matching-traffic-allows-other — it ends with "Check timed out after 60.0s" and no assertion result.
4. Equivalently by hand against the emulator: create a VPC 10.99.0.0/16 with firewall (10.99.0.0/24), workload (10.99.1.0/24) and target (10.99.2.0/24) subnets in one AZ; create an interface VPC endpoint for com.amazonaws.<region>.ec2 in the workload subnet; CreateRuleGroup Type=STATEFUL Capacity=100 with a DROP StatefulRule for TCP/FORWARD to 10.99.2.10/32 port 8080; CreateFirewallPolicy with aws:forward_to_sfe stateless defaults referencing that rule group; CreateFirewall on the firewall subnet; poll DescribeFirewall to READY and read FirewallStatus.SyncStates.<az>.Attachment.EndpointId; create route tables for the workload and target subnets with routes for each other's CIDR pointing at that endpoint; launch the target instance (listeners on 8080 and 8081, private IP 10.99.2.10) and the workload instance (probe + ec2:CreateTags role) with the check's user-data; poll DescribeInstances for the workload instance's sim2real:ready tag.
5. No sim2real:port8080 / sim2real:port8081 / sim2real:ready tag ever appears, so the DROP of tcp/8080 and the pass of tcp/8081 through the firewall endpoint are never observable.
