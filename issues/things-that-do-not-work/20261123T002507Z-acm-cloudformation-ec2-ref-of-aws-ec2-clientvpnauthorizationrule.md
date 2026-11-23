Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: acm__cloudformation__ec2.deploy-ec2-client-vpn-endpoint-family-round-trip-and-delete
Issue-Key: acm__cloudformation__ec2.deploy-ec2-client-vpn-endpoint-family-round-trip-and-delete|run#22|AssertionError|assert out["AuthRuleRef"] == want_ref, f"AuthorizationRule Ref {out['AuthRuleRef']!r}"
Issue-Commit: 5b88798ccff0ec7f765809a75ef2c7dfb1d7aad0

# acm__cloudformation__ec2: Ref of AWS::EC2::ClientVpnAuthorizationRule is the raw two-field string instead of the base64-encoded endpoint|cidr|group physical id

> _Note: this issue was identified against the code base at commit `5b88798ccff0ec7f765809a75ef2c7dfb1d7aad0`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateStack

## Description
Cause 1 of 1: the CloudFormation resource handler for AWS::EC2::ClientVpnAuthorizationRule builds the wrong physical resource id, so `{"Ref": "<AuthRule>"}` (and the stack Output carrying it) resolves to a value no AWS client can decode.

AWS: recorded behavior only — CloudFormation resource-type physical ids are not described by any Smithy model, and the EC2 model has no shape for them (the rule itself is only ever read back through DescribeClientVpnAuthorizationRules, whose AuthorizationRule carries ClientVpnEndpointId/DestinationCidr/GroupId/AccessAll). The recording shows the rule's physical id — what Ref returns — is the base64 encoding of the three-field, pipe-delimited triple `<ClientVpnEndpointId>|<TargetNetworkCidr>|<AccessGroupId>`, with the third field present but EMPTY when the rule was created with AuthorizeAllGroups (no AccessGroupId). Fixed text: the two `|` delimiters and the base64 envelope. Variable parts, all taken from the request and the emulator's own state: the endpoint id the rule's ClientVpnEndpointId resolved to, the rule's TargetNetworkCidr, and its AccessGroupId (empty for AuthorizeAllGroups).

Emulator: Ref resolves to the triple's first two fields, un-encoded and with only one delimiter — `<ClientVpnEndpointId>|<TargetNetworkCidr>` — i.e. neither base64-encoded nor carrying the trailing `|` for the absent access group. In this run the stack Output read `'cvpn-endpoint-000000000000000c1|10.96.1.0/24'` where base64 of `cvpn-endpoint-000000000000000c1|10.96.1.0/24|` was required (the endpoint id and the CIDR are this run's values filling the template, not part of the requirement). Everything before the Ref assertion passed: the stack reached CREATE_COMPLETE and the endpoint and VPC read back correctly through EC2, so this is purely the authorization rule's id representation.

Why: a client that round-trips the Ref gets a broken identity. Base64-decoding it to recover the (endpoint, target CIDR, access group) triple — the normal way to resolve this resource's id, e.g. for a resource import, a cross-stack export consumed by another template, or an operator script mapping a stack resource back to a DescribeClientVpnAuthorizationRules row — fails outright, and the missing third field makes the id ambiguous between an all-groups rule and a rule scoped to a group. Two rules on one endpoint for the same CIDR but different groups would also collide on the same id.

Fix: the CloudFormation resource handler for AWS::EC2::ClientVpnAuthorizationRule — the physical-resource-id construction it records at create and returns for Ref. It must join endpoint id, target network CIDR and access group id (empty string when AuthorizeAllGroups is set) with `|` and base64-encode the result. Note the earlier fixed issue 20261109T123922Z-acm-cloudformation-ec2-aws-ec2-clientvpnendpoint-clientvpnauthorizationrule-are registered the two resource types and is not this defect: the type is now recognized and provisions, and this call is not refused — only the id it hands back is wrong, which that issue never covered.

Traces: acm__cloudformation__ec2.deploy-ec2-client-vpn-endpoint-family-round-trip-and-delete.

## Evidence
Check: acm__cloudformation__ec2.deploy-ec2-client-vpn-endpoint-family-round-trip-and-delete [mismatch]

Assertion that diverged (after CREATE_COMPLETE and a successful EC2 read-back of the endpoint and VPC):

    # The rule's Ref is CloudFormation's id: base64 of endpoint|cidr|group (no group here).
    want_ref = base64.b64encode(f"{ep_id}|{_AUTH_CIDR}|".encode()).decode()
    assert out["AuthRuleRef"] == want_ref, f"AuthorizationRule Ref {out['AuthRuleRef']!r}"

Expected: the stack Output AuthRuleRef equals base64("<ClientVpnEndpointId>|<TargetNetworkCidr>|") — three pipe-delimited fields, the access-group field empty because the rule was created with AuthorizeAllGroups: true, the whole string base64-encoded.

Actual: AssertionError: AuthorizationRule Ref 'cvpn-endpoint-000000000000000c1|10.96.1.0/24'
  - not base64-encoded (the literal pipe-delimited string is returned)
  - only two fields: the trailing '|' for the absent AccessGroupId is missing

(The endpoint id and 10.96.1.0/24 are this run's generated/templated values; the requirement is the three-field base64 shape, with those fields taken from the request and the emulator's state.)

## Steps to Reproduce
1. ACM ImportCertificate a self-signed certificate; keep its CertificateArn.
2. CreateStack with a template containing:
   - AWS::EC2::VPC (any CIDR),
   - AWS::EC2::ClientVpnEndpoint with ClientCidrBlock, ServerCertificateArn = that ARN, AuthenticationOptions type certificate-authentication, ConnectionLogOptions {Enabled: false}, VpcId {Ref: Vpc},
   - AWS::EC2::ClientVpnAuthorizationRule with ClientVpnEndpointId {Ref: Endpoint}, TargetNetworkCidr e.g. 10.96.1.0/24, AuthorizeAllGroups: true,
   and an Output AuthRuleRef = {"Value": {"Ref": "AuthRule"}}.
3. Wait for CREATE_COMPLETE and read DescribeStacks Outputs.
4. Observe AuthRuleRef is the raw string '<endpoint-id>|10.96.1.0/24' instead of base64('<endpoint-id>|10.96.1.0/24|'); base64-decoding it fails.
5. Re-replay acm__cloudformation__ec2.deploy-ec2-client-vpn-endpoint-family-round-trip-and-delete.
