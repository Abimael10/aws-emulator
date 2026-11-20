Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: acm__cloudformation__ec2.deploy-ec2-client-vpn-endpoint-family-round-trip-and-delete
Issue-Key: acm__cloudformation__ec2.deploy-ec2-client-vpn-endpoint-family-round-trip-and-delete|run#18|AssertionError|assert got == want, f"endpoint reads {got!r}, expected {want!r}"
Issue-Commit: 7a896f613c88ac172234cb0dc0f4b83c6ec3b6e7

# EC2 DescribeClientVpnEndpoints omits ConnectionLogOptions.IncludeAuthorizationPolicyContext, so a Client VPN endpoint's connection-log options read back incomplete

> _Note: this issue was identified against the code base at commit `7a896f613c88ac172234cb0dc0f4b83c6ec3b6e7`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeClientVpnEndpoints

## Description
One net-new root cause in the EC2 Client VPN read path, surfaced by a CloudFormation-deployed AWS::EC2::ClientVpnEndpoint that is read back through EC2.

Cause 1 of 1: DescribeClientVpnEndpoints serializes ConnectionLogOptions without the IncludeAuthorizationPolicyContext member.

AWS: Smithy (services/service-com-amazonaws-ec2-amazonec2) — ClientVpnEndpoint.ConnectionLogOptions targets com.amazonaws.ec2#ConnectionLogResponseOptions (xmlName connectionLogOptions, ec2QueryName ConnectionLogOptions), whose members are Enabled (Boolean), CloudwatchLogGroup, CloudwatchLogStream and IncludeAuthorizationPolicyContext (Boolean, "Specifies whether the authorization policy evaluation context is included in the connection logs for the Client VPN endpoint"). Recorded behavior: for an endpoint created with ConnectionLogOptions {Enabled: false} and no authorization-policy-context setting, DescribeClientVpnEndpoints reports the response options with BOTH booleans materialized — Enabled false AND IncludeAuthorizationPolicyContext false — i.e. the unset flag is reported as its effective value (false), not left absent. CloudwatchLogGroup/CloudwatchLogStream stay absent because logging is disabled. HTTP 200; this is a representation divergence, not a refusal, so no error code is involved. Everything else the check reads on the same endpoint matches: VpcId, ClientCidrBlock (10.199.0.0/22 here, from the template's ClientCidrBlock), ServerCertificateArn, AuthenticationOptions [{Type: certificate-authentication, MutualAuthentication: {ClientRootCertificateChain: <the imported ACM certificate ARN>}}], the endpoint's own tags, and Status.Code "pending-associate".

Emulator: the same read returns ConnectionLogOptions as {'Enabled': False} only — the IncludeAuthorizationPolicyContext member is never emitted, whether the create request set it or not. (The compared value shown is the whole endpoint projection; the single differing element is the connection-log options structure.)

Why: a client that reads endpoint.ConnectionLogOptions['IncludeAuthorizationPolicyContext'] to decide whether authorization-policy context is being logged gets a KeyError / nil instead of the boolean AWS always supplies. Round-trip tooling (Terraform/CDK drift detection, CloudFormation-vs-EC2 reconciliation, config exporters) sees the field as missing rather than false, so it either crashes or reports phantom drift and tries to "re-apply" the same setting. Any equality comparison of a described endpoint against the expected shape fails, exactly as this check's does.

Fix: the DescribeClientVpnEndpoints response serializer / ClientVpnEndpoint projection in the ec2 service crate — the ConnectionLogResponseOptions structure must carry IncludeAuthorizationPolicyContext, defaulted from the Client VPN endpoint state written by CreateClientVpnEndpoint (and by the CloudFormation AWS::EC2::ClientVpnEndpoint resource provider) and updatable through ModifyClientVpnEndpoint; an unset flag is stored and reported as false, not omitted.

Traces: acm__cloudformation__ec2.deploy-ec2-client-vpn-endpoint-family-round-trip-and-delete.

Note on already-open issues: three open EC2 Client VPN issues exist (20261022T235108Z-ec2-the-whole-ec2-client-vpn-and-enclave-certificate.md, 20261114T033033Z-ec2-vpn-and-client-vpn-refusal-paths-10.md, 20261114T091241Z-ec2-10-net-new-causes-client-vpn-capacity.md). All three concern refusal paths / NotImplemented stubs on ApplySecurityGroupsToClientVpnTargetNetwork, ModifyClientVpnEndpoint, DescribeClientVpnRoutes and friends; none of them covers the ConnectionLogOptions representation on a successful DescribeClientVpnEndpoints, and the suite did not mark this check as owned by any of them.

## Evidence
Check acm__cloudformation__ec2.deploy-ec2-client-vpn-endpoint-family-round-trip-and-delete, [mismatch]:

AssertionError: endpoint reads ('vpc-...', '10.199.0.0/22', 'arn:aws:acm:<region>:<account>:certificate/<id>', [{'Type': 'certificate-authentication', 'MutualAuthentication': {'ClientRootCertificateChain': 'arn:aws:acm:<region>:<account>:certificate/<id>'}}], {'Enabled': False}, {'sim2real-cvpn': '<stack-name>'}, 'pending-associate'), expected (..., {'Enabled': False, 'IncludeAuthorizationPolicyContext': False}, ...)

Expected (AWS): ConnectionLogOptions == {'Enabled': False, 'IncludeAuthorizationPolicyContext': False}
Actual (emulator): ConnectionLogOptions == {'Enabled': False}

Every other element of the tuple matched; the sole divergence is the missing IncludeAuthorizationPolicyContext boolean. Values such as the vpc id, certificate ARN, stack name and tag value are per-run and are not part of the requirement; the requirement is that the member is present with its effective boolean value.

## Steps to Reproduce
1. acm.ImportCertificate a self-signed CA cert/key pair; keep the returned CertificateArn.
2. cloudformation.CreateStack with a template containing an AWS::EC2::VPC (CidrBlock 10.96.0.0/16) and an AWS::EC2::ClientVpnEndpoint whose Properties are ClientCidrBlock 10.199.0.0/22, ServerCertificateArn = that ARN, AuthenticationOptions [{Type: certificate-authentication, MutualAuthentication: {ClientRootCertificateChainArn: that ARN}}], ConnectionLogOptions {Enabled: false}, VpcId {Ref: Vpc}, and a TagSpecifications entry for resource type client-vpn-endpoint. Wait for CREATE_COMPLETE.
3. ec2.DescribeClientVpnEndpoints filtered on the endpoint's tag (or by the stack output's endpoint id).
4. Inspect ClientVpnEndpoints[0]['ConnectionLogOptions']: AWS returns {'Enabled': False, 'IncludeAuthorizationPolicyContext': False}; the emulator returns {'Enabled': False}.
5. Replay the trace id listed in trace_ids to confirm the fix.
