Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ec2.flow-log-fleet-and-gateway-error-semantics ec2.route-table-routes-read-back-with-origin-and-state-beside-the-local-routes
Issue-Shape-Id: com.amazonaws.ec2#AmazonEC2
Issue-Key: ec2.flow-log-fleet-and-gateway-error-semantics|run#4|AssertionError|assert not problems, "unexpected flow-log/gateway error semantics:\n" + "\n".join(problems)
Issue-Key: ec2.route-table-routes-read-back-with-origin-and-state-beside-the-local-routes|run#28|AssertionError|assert (code, msg) == (
Issue-Commit: f7526cbca79906f89a28c5d197946d676b42fe8d

# ec2: 2 net-new refusal-fidelity causes — CreateCarrierGateway's malformed-VPC code is InvalidVpcID.Malformed instead of InvalidVpcId.Malformed, and a second IPv6 default route is refused RouteAlreadyExists instead of OperationNotPermitted

> _Note: this issue was identified against the code base at commit `f7526cbca79906f89a28c5d197946d676b42fe8d`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateRoute

## Description
TWO net-new root causes on EC2 refusal paths. A fixer must close BOTH before submitting; the failing check id for each cause is named under its Traces line. Both are control-plane refusals the emulator already issues at the right HTTP status — what diverges is the error CODE (and, for Cause 2, the message as well), which is the machine-readable part clients branch on.

Smithy note for both causes: the EC2 model (services/service-com-amazonaws-ec2-amazonec2/smithy-model/com-amazonaws-ec2-amazonec2.json, ec2Query) declares `errors: []` on CreateCarrierGateway and on CreateRoute — EC2 carries no modelled error shapes, so every code and sentence below is recorded behavior only, taken from the recorded response the check asserts against.

Cause 1 of 2 — CreateCarrierGateway spells the malformed-VPC-id code with a capitalised 'ID'.
AWS: HTTP 400 with Error.Code exactly 'InvalidVpcId.Malformed' — lower-case 'd' in 'Id' — and Error.Message "The vpc ID '<vpc-id>' is malformed", where the quoted id is the VpcId from the request and the rest is fixed text. EC2 is deliberately inconsistent about this token across operations: the same family uses 'InvalidVpcID.NotFound' with a capital 'ID' elsewhere, and the behavioral check calls this out in a source comment ('Note the lower-case "Id" here, unlike InvalidVpcID.NotFound elsewhere'), so the casing is per-operation and must be reproduced per operation rather than normalised.
Emulator: the right status and the right message, but Error.Code 'InvalidVpcID.Malformed' — capital 'ID'.
Why: error codes are matched exactly. botocore/boto3 `except ec2.exceptions...`/`exc.response['Error']['Code'] == 'InvalidVpcId.Malformed'`, SDK retry and error-classification tables, CloudFormation/CDK resource handlers and Terraform's `awserr.Code` comparisons all string-compare the code; a one-character difference turns a classified client error into an unrecognised one, so callers that would have reported a malformed id instead fall through to generic failure or retry a non-retryable request.
Fix: the CreateCarrierGateway handler's VpcId validation must emit the code 'InvalidVpcId.Malformed'; the code string belongs in that operation's validation/refusal construction, and any shared malformed-VPC-id helper must not impose one casing on all operations because EC2's own casing differs per operation.
Traces: ec2.flow-log-fleet-and-gateway-error-semantics.

Cause 2 of 2 — CreateRoute treats a second IPv6 default route as a duplicate destination instead of refusing the conflicting next-hop class.
AWS: with an existing ::/0 route whose next hop is an internet gateway in the route table, a CreateRoute for the SAME destination ::/0 naming an EgressOnlyInternetGatewayId is refused HTTP 400 with Error.Code 'OperationNotPermitted' and Error.Message 'There are existing ipv6 routes with next hop as internet Gateway.' — the whole sentence is fixed text, with no identifier substituted (note AWS's own capitalisation of 'Gateway' and the lower-case 'ipv6'). The table is left unchanged, and once the blocking ::/0 internet-gateway route is deleted the identical CreateRoute succeeds and reads back with Origin 'CreateRoute' and State 'active'. This is a next-hop-class rule, not a duplicate-destination rule: AWS refuses mixing an egress-only gateway with an internet-gateway IPv6 route rather than reporting the destination as taken.
Emulator: HTTP 400 with Error.Code 'RouteAlreadyExists' and Error.Message 'The route identified by ::/0 already exists.' — the duplicate-destination refusal, which is the response AWS gives for a repeated destination with the same next-hop class, not for this conflict.
Why: the two refusals mean different things and callers act on them differently. 'RouteAlreadyExists' tells a client the destination is already programmed — a converging CloudFormation/CDK/Terraform apply treats that as benign and moves on, or deletes and recreates the wrong route — whereas 'OperationNotPermitted' tells it the egress-only gateway can never coexist with the IPv6 internet-gateway route and the internet-gateway route must be removed first. A client relying on AWS's behaviour therefore takes the wrong remediation, and the message names a destination AWS does not name instead of the conflicting next hop class.
Fix: the CreateRoute handler's conflict detection must separate the two cases before it reports a duplicate: when the requested destination already has a route whose next hop is an internet gateway and the request names an EgressOnlyInternetGatewayId, refuse with OperationNotPermitted and AWS's sentence; only an actual same-class duplicate destination keeps RouteAlreadyExists. This belongs in that operation's validation against the route table's current routes, not in the serializer, and the refused call must leave the table's routes untouched.
Traces: ec2.route-table-routes-read-back-with-origin-and-state-beside-the-local-routes.

## Evidence
Behavioral replay of suite simparity-behavioral-aws-v1 against this checkout (emulator serving; all setup calls in both checks succeeded).

Cause 1 — [mismatch] ec2.flow-log-fleet-and-gateway-error-semantics. The check batches refusal probes and collects every mismatch; exactly one was collected:

  create_carrier_gateway: expected ('InvalidVpcId.Malformed', "The vpc ID 'vpc-0123456789abcdef0' is malformed"),
                          got      ('InvalidVpcID.Malformed', "The vpc ID 'vpc-0123456789abcdef0' is malformed")

The message already matches; only the code's casing diverges ('Id' vs 'ID'). The probe uses a well-formed-but-absent/malformed VpcId; the literal 'vpc-0123456789abcdef0' is the check's fixture value and fills the message's variable slot — the requirement is the fixed text "The vpc ID '<VpcId from the request>' is malformed" plus the code 'InvalidVpcId.Malformed'. Every other probe in this check (flow logs, fleets, the remaining gateway probes) passed, so this is the only cause in it.

Cause 2 — [mismatch] ec2.route-table-routes-read-back-with-origin-and-state-beside-the-local-routes. AssertionError raised at the check's
  assert (code, msg) == ("OperationNotPermitted",
                         "There are existing ipv6 routes with next hop as internet Gateway.")
with:

  a second ::/0 route answered ('RouteAlreadyExists', 'The route identified by ::/0 already exists.')

Sequence actually executed and accepted before the failure: CreateRouteTable in a fresh VPC; CreateRoute(0.0.0.0/0, GatewayId=<igw>) -> Return true; CreateRoute(::/0, GatewayId=<igw>) -> Return true; the table then reads back its local routes plus both gateway routes with Origin 'CreateRoute' and State 'active' (that assertion passed). The failing call is CreateRoute(RouteTableId=<rtb>, DestinationIpv6CidrBlock='::/0', EgressOnlyInternetGatewayId=<eigw>).

Message anatomy: 'There are existing ipv6 routes with next hop as internet Gateway.' is entirely fixed text — no route-table id, destination or gateway id is substituted. The emulator's 'The route identified by ::/0 already exists.' carries both the wrong code and a destination AWS does not echo for this refusal.

Not reached because the check stops here, and therefore not claimed as separate causes: the assertions that the refused CreateRoute leaves the table's routes unchanged, that deleting the ::/0 internet-gateway route then lets the identical egress-only CreateRoute succeed, and that the restored ::/0 route reads back with EgressOnlyInternetGatewayId, Origin 'CreateRoute' and State 'active'.

Smithy grounding: smithy_query query=operation on services/service-com-amazonaws-ec2-amazonec2/smithy-model/com-amazonaws-ec2-amazonec2.json reports CreateCarrierGateway errors [] (input VpcId required) and CreateRoute errors [] (input RouteTableId required, with DestinationIpv6CidrBlock, GatewayId and EgressOnlyInternetGatewayId all optional members), so both codes and sentences are recorded behavior only.

## Steps to Reproduce
1) Build and boot the emulator from this checkout and replay suite simparity-behavioral-aws-v1, checks: ec2.flow-log-fleet-and-gateway-error-semantics, ec2.route-table-routes-read-back-with-origin-and-state-beside-the-local-routes.
2) Cause 1: ec2 CreateCarrierGateway(VpcId='vpc-0123456789abcdef0') — a well-formed-looking but malformed/absent VPC id. Expect HTTP 400 with Error.Code exactly 'InvalidVpcId.Malformed' (lower-case 'd' in 'Id') and Error.Message "The vpc ID 'vpc-0123456789abcdef0' is malformed". Observed code: 'InvalidVpcID.Malformed'. Confirm the neighbouring spellings are not changed by the fix: CreateFlowLogs with the same absent VPC id must still answer 'InvalidVpcId.NotFound' / 'Unknown resource <vpc-id>', and operations that use 'InvalidVpcID.NotFound' must keep the capital 'ID'.
3) Cause 2: ec2 CreateVpc + CreateInternetGateway + AttachInternetGateway + CreateEgressOnlyInternetGateway + CreateRouteTable.
4) ec2 CreateRoute(RouteTableId=<rtb>, DestinationCidrBlock='0.0.0.0/0', GatewayId=<igw>) and CreateRoute(RouteTableId=<rtb>, DestinationIpv6CidrBlock='::/0', GatewayId=<igw>); both return Return=true and DescribeRouteTables shows both with Origin 'CreateRoute', State 'active'.
5) ec2 CreateRoute(RouteTableId=<rtb>, DestinationIpv6CidrBlock='::/0', EgressOnlyInternetGatewayId=<eigw>). Expect HTTP 400, Error.Code 'OperationNotPermitted', Error.Message 'There are existing ipv6 routes with next hop as internet Gateway.' Observed: ('RouteAlreadyExists', 'The route identified by ::/0 already exists.').
6) Then confirm the rest of the contract: DescribeRouteTables must be byte-identical to step 4's reading; ec2 DeleteRoute(RouteTableId=<rtb>, DestinationIpv6CidrBlock='::/0') followed by the identical CreateRoute from step 5 must return Return=true, and the table must then show ::/0 -> EgressOnlyInternetGatewayId=<eigw> with Origin 'CreateRoute' and State 'active'.
