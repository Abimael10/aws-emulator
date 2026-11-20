Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ec2.assign-private-nat-gateway-address-in-subnets-reserved-range-rejected ec2.idvdoc-document-reminted-and-resigned-after-a-stop-start ec2.idvdoc-foreign-signature-rejected-as-proof-of-this-document ec2.idvdoc-signature-verifies-against-the-aws-public-certificate
Issue-Shape-Id: com.amazonaws.ec2#AmazonEC2
Issue-Key: ec2.assign-private-nat-gateway-address-in-subnets-reserved-range-rejected|run#14|AssertionError|assert code == "InvalidParameter", f"expected InvalidParameter, got {code!r}: {message!r}"
Issue-Key: ec2.idvdoc-document-reminted-and-resigned-after-a-stop-start|_read_identity_from_guest#14|AssertionError|assert inv["Status"] == "Success", (
Issue-Key: ec2.idvdoc-foreign-signature-rejected-as-proof-of-this-document|_guest_material#16|AssertionError|assert inv["Status"] == "Success", (
Issue-Key: ec2.idvdoc-signature-verifies-against-the-aws-public-certificate|_guest_material#18|AssertionError|assert inv["Status"] == "Success", (
Issue-Commit: a3c668f6bb53d78c1d1232fe39d40072881572f4

# ec2: 2 net-new causes — AssignPrivateNatGatewayAddress misclassifies a reserved-address refusal, and the instance identity document cannot be read from the guest (3 idvdoc checks)

> _Note: this issue was identified against the code base at commit `a3c668f6bb53d78c1d1232fe39d40072881572f4`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
AssignPrivateNatGatewayAddress

## Description
Two net-new root causes in the ec2 emulator, both grounded in failing behavioral checks that the replay report did NOT mark as already filed or owned.

Cause 1 of 2: AssignPrivateNatGatewayAddress refuses an address in the subnet's AWS-reserved low range with the wrong error code and an emulator-authored message.
Failing check id: ec2.assign-private-nat-gateway-address-in-subnets-reserved-range-rejected
  AWS:      Smithy (com-amazonaws-ec2-amazonec2): AssignPrivateNatGatewayAddress(NatGatewayId required, PrivateIpAddresses: IpList, PrivateIpAddressCount @range 1..31); the operation models no named errors, so the refusal is the ec2 query protocol's generic error envelope — recorded behavior only for code and text. The recorded AWS behavior for one of the four addresses AWS reserves at the bottom of every subnet (network address + 1 .. + 3, plus the network address itself) is HTTP 400 with Error.Code `InvalidParameter` and Error.Message `The provided private Ip addresses are in subnet's reserved address range [<address>]`. Fixed text: "The provided private Ip addresses are in subnet's reserved address range [" … "]"; the only variable part is the list of refused addresses taken verbatim from the request's PrivateIpAddresses. Nothing is added to the NAT gateway (DescribeNatGateways still shows only the primary address), and an ordinary free address in the same subnet is accepted immediately afterwards.
  Emulator: answers Error.Code `InvalidParameterValue` with the message `Invalid value '<ip>' for privateIpAddress. It is not an assignable address of the subnet '<subnet-id>' ('<cidr>')` — a different error class and a message whose template (an "Invalid value ... for privateIpAddress" / "not an assignable address of the subnet" form naming the subnet id and its CIDR) AWS never emits for this condition.
  Why:      A client that branches on the refusal class cannot distinguish "you asked for a reserved address" from "you asked for a value that is not an address of this subnet at all": retry/choose-another-address logic keyed on `InvalidParameter` plus the reserved-range wording never fires, and operators and log scrapers see a refusal reason AWS does not produce.
  Fix:      The AssignPrivateNatGatewayAddress handler's request validation / refusal mapping for the ec2 service: the subnet-reserved-range check must be its own refusal, classified `InvalidParameter`, with AWS's reserved-address-range message carrying the requested address(es), and ordered ahead of the generic "not an assignable address of the subnet" path (which stays for addresses genuinely outside the subnet).
  Traces:   ec2.assign-private-nat-gateway-address-in-subnets-reserved-range-rejected (mismatch).

Cause 2 of 2: the instance identity document and its signature are unreadable from a running guest — the aws:RunShellScript invocation that reads IMDS ends Status 'Failed' with empty output, so no idvdoc scenario reaches a verdict.
Failing check ids: ec2.idvdoc-document-reminted-and-resigned-after-a-stop-start, ec2.idvdoc-foreign-signature-rejected-as-proof-of-this-document, ec2.idvdoc-signature-verifies-against-the-aws-public-certificate
  AWS:      recorded behavior only (this is the EC2 guest/IMDS data plane, not a modeled control-plane shape). A launched instance that has come Online as an SSM managed node runs an aws:RunShellScript document in the guest; the invocation reaches a terminal Status "Success" with ResponseCode 0 and StandardOutputContent carrying the script's stdout. The script reads http://169.254.169.254/latest/dynamic/instance-identity/document and .../signature (after taking an IMDSv2 token from PUT /latest/api/token), so the guest observes: a JSON identity document whose instanceId/accountId/region/imageId/instanceType/availabilityZone/architecture/version describe the instance and are invariant across a stop/start, a pendingTime that is reminted on each start, and a base64 RSA signature that verifies against the AWS-published ec2.amazonaws.com public certificate for the Region — and does NOT verify a document other than the one it was minted for.
  Emulator: all three invocations end terminal Status 'Failed' with an empty StandardErrorContent (and no stdout), e.g. "the guest read on i-00000000000000001 was 'Failed': 'Failed' stderr=''". Nothing about the identity document is observable: no document, no pendingTime remint, no signature.
  Why:      Any client that authenticates an instance to itself or to a peer by presenting the identity document and verifying its signature against the AWS certificate (the documented EC2 procedure) gets nothing to present and nothing to verify; a workload that gates on the document's invariant fields across a reboot, or that rejects a foreign signature, cannot run at all against the emulator.
  Fix:      The EC2 instance data plane: the guest runtime's IMDS service must serve /latest/dynamic/instance-identity/document and /signature (minting the document at each start and signing it with the identity key whose published certificate verifies it), and the in-guest command execution path that SSM Run Command reports on must actually execute the script and surface its stdout/exit code instead of a bare Failed. This is the same family as the open issues 20261115T003554Z-ec2-guests-observe-nothing-in-guest-commands-exit.md and 20261113T123420Z-ec2-data-plane-a-launched-guest-never-runs.md — neither owns these three checks (the report left them unattributed to any issue), so they are filed here and those issues are named for the fixer's benefit.
  Traces:   ec2.idvdoc-signature-verifies-against-the-aws-public-certificate, ec2.idvdoc-foreign-signature-rejected-as-proof-of-this-document, ec2.idvdoc-document-reminted-and-resigned-after-a-stop-start (all mismatch, all failing on the same assertion: the guest invocation was 'Failed' with empty stderr).

## Evidence
Cause 1 — expected vs actual (behavioral check assertion):
  expected: code 'InvalidParameter', message "The provided private Ip addresses are in subnet's reserved address range [<reserved address>]"
  actual:   code 'InvalidParameterValue', message "Invalid value '10.219.0.1' for privateIpAddress. It is not an assignable address of the subnet 'subnet-000000000000000b9' ('10.199.0.0/25'-style subnet CIDR)"
  (the request asked for network_address+1 of the NAT gateway's subnet; the concrete IP, subnet id and CIDR are per-run values, the code and the message template are the requirement)
  AssertionError: expected InvalidParameter, got 'InvalidParameterValue': "Invalid value '10.219.0.1' for privateIpAddress. It is not an assignable address of the subnet 'subnet-000000000000000b9' ('10.219.0.0/25')"

Cause 2 — expected vs actual:
  expected: GetCommandInvocation Status == 'Success' (ResponseCode 0) with StandardOutputContent carrying the guest's IMDS reads of /latest/dynamic/instance-identity/document and /latest/dynamic/instance-identity/signature
  actual:   Status 'Failed', empty stderr, no stdout:
    AssertionError: the guest read on i-00000000000000001 was 'Failed': 'Failed' stderr=''        (idvdoc-document-reminted-and-resigned-after-a-stop-start)
    AssertionError: the guest command on i-00000000000000001 was 'Failed': 'Failed' stderr=''     (idvdoc-foreign-signature-rejected-as-proof-of-this-document)
    AssertionError: the guest command on i-00000000000000001 was 'Failed': 'Failed' stderr=''     (idvdoc-signature-verifies-against-the-aws-public-certificate)
  Consequence: the checks never get to compare invariant document fields across a stop/start, to re-verify the reminted signature, or to reject a foreign signature.

## Steps to Reproduce
Cause 1:
1. ec2 CreateVpc 10.219.0.0/24, CreateSubnet 10.219.0.0/25 in it.
2. CreateNatGateway(SubnetId=<subnet>, ConnectivityType='private'); wait State=available.
3. AssignPrivateNatGatewayAddress(NatGatewayId=<nat>, PrivateIpAddresses=['10.219.0.1'])  (= subnet network address + 1, inside AWS's reserved low range).
4. Observe the error: emulator answers InvalidParameterValue / "Invalid value ... not an assignable address of the subnet ..."; AWS answers InvalidParameter / "The provided private Ip addresses are in subnet's reserved address range [10.219.0.1]".
5. DescribeNatGateways must still show only the primary address, and assigning an ordinary free host address of the same subnet must then succeed.

Cause 2:
1. RunInstances a t3.micro from the AL2023 SSM AMI parameter with an instance profile carrying AmazonSSMManagedInstanceCore; wait until it is an Online SSM managed node.
2. ssm SendCommand(DocumentName='AWS-RunShellScript') with a script that takes an IMDSv2 token (PUT /latest/api/token) and curls /latest/dynamic/instance-identity/document and /latest/dynamic/instance-identity/signature.
3. Poll GetCommandInvocation to a terminal status: the emulator reports Status 'Failed' with empty StandardOutputContent/StandardErrorContent instead of Success with the document and signature on stdout.
4. Re-run after a Stop/Start to see the document reminted (new pendingTime, invariant instanceId/region/imageId/...) and the new signature verifying against the AWS ec2.amazonaws.com public certificate, and a signature from another instance failing to verify — none of which is observable today.
