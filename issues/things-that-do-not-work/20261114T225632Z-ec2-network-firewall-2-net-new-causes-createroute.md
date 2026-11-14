Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ec2__network-firewall.nfwr-routed-firewall-endpoint-refusals-rejected ec2__network-firewall.nfwr-stateful-domain-rule-blocks-host-header-allows-other
Issue-Key: ec2__network-firewall.nfwr-routed-firewall-endpoint-refusals-rejected|_error#12|AssertionError|raise AssertionError("the call succeeded; real AWS rejects it, so there is no error code")
Issue-Key: ec2__network-firewall.nfwr-stateful-domain-rule-blocks-host-header-allows-other|_poll_for_tag#69|Timeout|time.sleep(interval_s)
Issue-Commit: 190e7b20abd2a451df846a0932a8de897636f1f9

# ec2__network-firewall: 2 net-new causes — CreateRoute accepts a VPC-endpoint id supplied as GatewayId instead of refusing it, and the stateful domain-rule data-plane scenario never reaches a verdict

> _Note: this issue was identified against the code base at commit `190e7b20abd2a451df846a0932a8de897636f1f9`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateRoute

## Description
Two net-new root causes in the ec2__network-firewall surface. Both must be closed before this issue is submitted.

Cause 1 of 2: EC2 CreateRoute accepts a Network Firewall endpoint id passed in the GatewayId member instead of refusing it as an unknown gateway.
Failing check: ec2__network-firewall.nfwr-routed-firewall-endpoint-refusals-rejected
AWS: Smithy com.amazonaws.ec2#CreateRoute (services/service-com-amazonaws-ec2-amazonec2/smithy-model/com-amazonaws-ec2-amazonec2.json) takes RouteTableId (required) plus exactly one target member; GatewayId targets the shape RouteGatewayId (internet gateway / virtual private gateway ids), while VpcEndpointId is a separate member targeting VpcEndpointId. CreateRouteResult carries only `return`; the operation models no errors (errors: []), so the refusal is recorded behavior only: a Gateway Load Balancer endpoint id (vpce-…, the id Network Firewall reports at DescribeFirewall FirewallStatus.SyncStates.<az>.Attachment.EndpointId) is NOT a RouteGatewayId, and naming it in GatewayId is refused with HTTP 400, Error.Code InvalidGatewayID.NotFound and the message "The gateway ID '<gatewayId>' does not exist" — fixed text `The gateway ID '` … `' does not exist`, the quoted value taken verbatim from the request's GatewayId member (never a value fixed by the message). The refused call has no effect: the route table still holds only its local route for the VPC CIDR.
Emulator: the call is ACCEPTED. No error is raised at all, so the check's refusal probe fails with "the call succeeded; real AWS rejects it, so there is no error code". A vpce- endpoint id is therefore admitted through the GatewayId target path, and (per the check's follow-up assertion) a route the request should never have created can be installed in the table.
Why: a client that mis-wires a firewall endpoint as GatewayId (a very common mistake, since the endpoint must be routed via VpcEndpointId) is told the route was created. Its traffic is never steered through the firewall endpoint, and nothing in the client's view distinguishes a correctly routed firewall from a silently broken one; code that branches on InvalidGatewayID.NotFound to retry with VpcEndpointId never runs. It also leaves a route table whose contents diverge from AWS for every later read and every dependency check (DeleteFirewall-while-routed, DeleteSubnet DependencyViolation).
Fix: the EC2 CreateRoute handler's target resolution/validation — the GatewayId member must be resolved only against gateway state (internet gateway / virtual private gateway / vgw-like RouteGatewayId values) and must answer 400 InvalidGatewayID.NotFound with AWS's message when the supplied id is not such a gateway (a vpce- VPC-endpoint id included), leaving the route table unchanged.
Related open issue that does NOT cover this: 20261104T085432Z-ec2-network-firewall-createroute-refuses-a-route-whose.md describes the opposite leg of the same check (a VALID VpcEndpointId route being refused with InvalidGatewayID.NotFound). The report did not mark this check as owned, and this cause is the missing refusal on the GatewayId member.

Cause 2 of 2: the Network Firewall stateful domain-rule data-plane scenario never reaches a verdict — the check is cut off by the replay budget with no result.
Failing check: ec2__network-firewall.nfwr-stateful-domain-rule-blocks-host-header-allows-other
AWS: recorded behavior only. The scenario provisions a VPC with firewall/workload/target subnets, an interface VPC endpoint for the EC2 API, a STATEFUL rule group whose RulesString drops HTTP requests whose http.host content matches one domain, a firewall policy referencing it, and a firewall on the firewall subnet; it then routes inter-subnet traffic at the firewall endpoint and launches two instances whose user-data runs a listener and a probe. On AWS the workload instance tags itself (sim2real:ready=1, sim2real:blocked_host=BLOCKED, sim2real:allowed_host=ALLOWED), i.e. the request naming the blocked domain is dropped by the stateful rule while the request naming the other domain passes, and DescribeInstances shows those tags within the check's 300 s poll.
Emulator: the check produces no verdict at all — replay reports "Check timed out after 60.0s". The scenario's observable end state (the workload instance's sim2real:* tags) never appears, so domain-content filtering through the firewall endpoint is never demonstrated. Hypothesis (neither the model nor the replay output states which call stalls): the terminal poll is _poll_for_tag on the workload instance, and the emulator's launched guests do not execute their user-data payload, so the tags are never written; the earlier waits (vpc_available, VPC-endpoint available, firewall READY, instance_running) are the other candidates. Related open issue that does NOT cover this (different service heading): 20261113T123420Z-ec2-data-plane-a-launched-guest-never-runs.md.
Why: every client behaviour that depends on a firewall actually inspecting L7 content — i.e. the whole reason to deploy a stateful domain rule — is unobservable against the emulator. A user validating a Network Firewall deployment gets a control plane that reports READY and routed while the data plane enforces nothing, and any test suite that asserts a blocked domain is blocked hangs instead of failing or passing.
Fix: the Network Firewall data-plane/endpoint enforcement path together with the EC2 instance-guest execution the scenario observes it through: a firewall endpoint named as a route target must apply the policy's stateful rule groups to packets crossing it, and a launched instance must run its user-data so the scenario's self-tagging converges inside the poll window.

Traces: ec2__network-firewall.nfwr-routed-firewall-endpoint-refusals-rejected (cause 1), ec2__network-firewall.nfwr-stateful-domain-rule-blocks-host-header-allows-other (cause 2).

## Evidence
Replay of suite simparity-behavioral-aws-v1 (emulator serving; ec2__network-firewall 5/10 passed, 5 failed — three of those five are OWNED by existing issues and are excluded here).

Cause 1 — [mismatch] ec2__network-firewall.nfwr-routed-firewall-endpoint-refusals-rejected
Failing call (check line 36):
  code, message = _error(lambda: ec2.create_route(RouteTableId=dmz_rt, DestinationCidrBlock="0.0.0.0/0", GatewayId=ep))
where ep = describe_firewall(FirewallArn=arn)["FirewallStatus"]["SyncStates"][<az>]["Attachment"]["EndpointId"] (a vpce- id for a firewall the emulator itself reported READY) and dmz_rt is a route table in that firewall's VPC.
Expected: HTTP 400, Error.Code=InvalidGatewayID.NotFound, Error.Message="The gateway ID '<ep>' does not exist" (fixed text `The gateway ID '`…`' does not exist`; the quoted id is the request's GatewayId), and the route table unchanged — DescribeRouteTables still shows only [(<vpc cidr>, "local")].
Actual: the call SUCCEEDED; the harness raised "AssertionError: the call succeeded; real AWS rejects it, so there is no error code" from its _error() helper, i.e. no error code, no message, HTTP 2xx.
Model consulted: smithy_query query=operation operation=CreateRoute — input members GatewayId (target RouteGatewayId) and VpcEndpointId (target VpcEndpointId) are distinct targets; output CreateRouteResult.Return; errors: [].

Cause 2 — [mismatch] ec2__network-firewall.nfwr-stateful-domain-rule-blocks-host-header-allows-other
Expected: the check completes and DescribeInstances on the workload instance reports tags sim2real:ready=1, sim2real:blocked_host=BLOCKED, sim2real:allowed_host=ALLOWED (the blocked domain dropped by the stateful http.host rule, the other domain allowed).
Actual: "Check timed out after 60.0s" — no assertion is ever reached, so no tag values are produced at all. The check's terminal wait is _poll_for_tag(ec2, workload_id, "sim2real:ready", timeout_s=300), which raises "instance never reported tag sim2real:ready within 300s" on AWS-like non-convergence; here the replay budget expires first.

## Steps to Reproduce
1. git_clone the repo and boot the emulator built from it.
2. Run the behavioral suite (simparity-behavioral-aws-v1).
3. Cause 1 — replay ec2__network-firewall.nfwr-routed-firewall-endpoint-refusals-rejected, or by hand against the emulator: create a VPC (10.2.0.0/16) with two subnets in two AZs; create a Network Firewall policy and a firewall on those subnets; poll DescribeFirewall until FirewallStatus.Status is READY and read FirewallStatus.SyncStates.<az>.Attachment.EndpointId (a vpce- id); create a route table in that VPC; call ec2 CreateRoute with RouteTableId=<that table>, DestinationCidrBlock=0.0.0.0/0 and GatewayId=<that vpce- id>. It answers 2xx; AWS answers 400 InvalidGatewayID.NotFound "The gateway ID '<vpce-id>' does not exist" and leaves the table holding only its local route.
4. Cause 2 — replay ec2__network-firewall.nfwr-stateful-domain-rule-blocks-host-header-allows-other. It never reaches an assertion (times out). By hand: build the VPC/subnets, an interface VPC endpoint for com.amazonaws.<region>.ec2, a STATEFUL rule group with RulesString 'drop http any any -> any any (http.host; content:"blocked.sim2real.internal"; msg:"block domain"; sid:1; rev:1;)', a policy referencing it, a firewall on the firewall subnet; route the workload and target subnets at the firewall endpoint; launch the target and workload instances with the check's user-data; then poll DescribeInstances for the workload instance's sim2real:ready tag. No sim2real:* tag ever appears.
