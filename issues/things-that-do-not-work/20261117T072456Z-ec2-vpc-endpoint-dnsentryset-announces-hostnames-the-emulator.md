Issue-Source: trace-sweep-divergence
Issue-Trace: ec2-ssm-interface-endpoint-private-dns-converges-to-available-and-deletes-to-not-found
Issue-Step: task#5
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: ec2-ssm-interface-endpoint-private-dns-converges-to-available-and-deletes-to-not-found
Issue-Shape-Id: com.amazonaws.ec2#AmazonEC2
Issue-Key: ec2|CreateVpcEndpoint|2xx->2xx|$.CreateVpcEndpointResponse.vpcEndpoint.dnsEntrySet.item[*].dnsName
Issue-Key: ec2|DescribeVpcEndpoints|2xx->2xx|$.DescribeVpcEndpointsResponse.vpcEndpointSet.item.dnsEntrySet.item[*].dnsName
Issue-Commit: 0e5c96f71ec439fc5dd27026803182d983843d39

# ec2 VPC endpoint dnsEntrySet announces hostnames the emulator never serves

> _Note: this issue was identified against the code base at commit `0e5c96f71ec439fc5dd27026803182d983843d39`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateVpcEndpoint

## Description
Two net-new wire causes in the EC2 emulator's interface-VPC-endpoint DNS projection. Both must be closed before this issue is submitted. The replay verdict for both is host_not_own: the dnsName values the emulator publishes are not addresses the emulator can be reached on, and two of the four entries match the recording byte-for-byte yet still fail, so the fault is the host that is announced, not the name's spelling.

Cause 1 of 2: CreateVpcEndpoint publishes DnsEntries pointing at public AWS hosts the emulator does not serve.
AWS: Smithy (com.amazonaws.ec2) CreateVpcEndpoint -> VpcEndpoint$DnsEntries : list of DnsEntry{DnsName (xmlName dnsName), HostedZoneId} -- 'Describes a DNS entry' / 'The DNS name'. For an Interface endpoint with private DNS enabled AWS returns one regional endpoint-specific name of the form <vpce-id>-<hash>.<service>.<region>.vpce.amazonaws.com, one zonal endpoint-specific name per subnet the endpoint is placed in (<vpce-id>-<hash>-<az>.<service>.<region>.vpce.amazonaws.com) and one entry per private DNS name the endpoint service advertises (for com.amazonaws.<region>.ssm: <service>.<region>.amazonaws.com and the dual-stack <service>.<region>.api.aws). The contract of those names is that they resolve, for the caller, to the endpoint that serves the service -- the request a client sends to a DnsEntry name must arrive at the API that answered the CreateVpcEndpoint call. The vpce id and the 8-character hash are minted per endpoint and are not a requirement (recorded behavior only for their spelling).
Emulator: the endpoint record is built with AWS's real public hostnames copied verbatim. The replayed CreateVpcEndpoint answers 200 with dnsEntrySet item[0] 'vpce-<id>-<hash>.ssm.us-west-2.vpce.amazonaws.com', item[1] the matching '-us-west-2a' zonal name, item[2] 'ssm.us-west-2.amazonaws.com' and item[3] 'ssm.us-west-2.api.aws'. Items [2] and [3] are identical to the recording and are still reported as host_not_own, because none of the four hosts is one the emulator answers on -- the emulator is reached on its own data listener, and it announces an address other than the one it is reached on.
Why: a client that follows the endpoint it just created -- the whole point of DnsEntries -- resolves a name that either leaves the sandbox for real AWS or does not resolve at all. Private-DNS wiring automation (Route 53 records, hosts entries, SDK endpoint overrides fed from DnsEntries[].DnsName) is configured against an unreachable host, and the failure surfaces far from this call as a connect/DNS error rather than as a bad response.
Fix: the DnsEntries construction in the CreateVpcEndpoint handler (and the endpoint record it writes into state) must announce a host the emulator serves while keeping AWS's name shape and entry composition, the same way the emulator's other endpoint-bearing responses are rewritten to its own address.
Traces: ec2-ssm-interface-endpoint-private-dns-converges-to-available-and-deletes-to-not-found#task#5.

Cause 2 of 2: DescribeVpcEndpoints reads the same record back and re-announces the non-served hosts.
AWS: Smithy (com.amazonaws.ec2) DescribeVpcEndpoints -> VpcEndpoint$DnsEntries, same DnsEntry shape and same resolve-to-the-serving-endpoint contract; the read is a projection of the endpoint created earlier, so every DnsName a client discovers by describing the endpoint must be reachable.
Emulator: the describe projection answers 200 and repeats the four hostnames from Cause 1 ('vpce-<id>-<hash>.ssm.us-west-2.vpce.amazonaws.com', its '-us-west-2a' zonal form, 'ssm.us-west-2.amazonaws.com', 'ssm.us-west-2.api.aws'), all flagged host_not_own; the last two are byte-identical to the recording. The per-endpoint hash differs from the recording's ('y3gvsl32' vs the emulator's value) and that difference is NOT the requirement -- it is a per-run generated value.
Why: discovery is the common path -- clients that did not create the endpoint learn its names only from DescribeVpcEndpoints. Fixing only the create path would leave every re-read serving unreachable hosts, so the read path needs the same rewrite.
Fix: the DescribeVpcEndpoints serializer/projection (or, better, the one shared endpoint-state field both paths read) must emit the emulator-served host.

Related open issue, which does NOT cover these clusters: 20261117T035019Z-ec2-4-net-new-causes-interface-endpoint-dnsentries.md (behavioral) reports that the endpoint's DnsEntries carry a private name the service catalogue does not advertise. That is about entry COMPOSITION; this issue is about the HOST announced in each entry, and the replay reports these two wire clusters as unowned.

## Evidence
Cause 1 — fixture ec2-ssm-interface-endpoint-private-dns-converges-to-available-and-deletes-to-not-found, step task#5, CreateVpcEndpoint, expected status 200, actual status 200, verdict mismatch (host_not_own):
  $.CreateVpcEndpointResponse.vpcEndpoint.dnsEntrySet.item[0].dnsName: host_not_own (expected 'vpce-00971b36facfcd488-y3gvsl32.ssm.us-west-2.vpce.amazonaws.com', got 'vpce-00000000000000099-da2wntq4.ssm.us-west-2.vpce.amazonaws.com')
  $.CreateVpcEndpointResponse.vpcEndpoint.dnsEntrySet.item[1].dnsName: host_not_own (expected '<shaped-not-asserted>', got 'vpce-00000000000000099-da2wntq4-us-west-2a.ssm.us-west-2.vpce.amazonaws.com')
  $.CreateVpcEndpointResponse.vpcEndpoint.dnsEntrySet.item[2].dnsName: host_not_own (expected 'ssm.us-west-2.amazonaws.com', got 'ssm.us-west-2.amazonaws.com')
  $.CreateVpcEndpointResponse.vpcEndpoint.dnsEntrySet.item[3].dnsName: host_not_own (expected 'ssm.us-west-2.api.aws', got 'ssm.us-west-2.api.aws')
  Expected: four entries whose dnsName values are hosts the emulator serves, in AWS's name shape (one regional endpoint-specific name, one zonal name per subnet, one per advertised private DNS name).
  Actual: four public AWS hosts the emulator does not answer on. Items [2]/[3] expected == got, so the divergence is host ownership alone; the differing vpce id and hash in items [0]/[1] are per-run generated values and are not part of the requirement.

Cause 2 — same fixture, step task#6, DescribeVpcEndpoints, expected status 200, actual status 200, verdict mismatch (host_not_own):
  $.DescribeVpcEndpointsResponse.vpcEndpointSet.item.dnsEntrySet.item[0].dnsName: host_not_own (expected 'vpce-00000000000000099-y3gvsl32.ssm.us-west-2.vpce.amazonaws.com', got 'vpce-00000000000000099-da2wntq4.ssm.us-west-2.vpce.amazonaws.com')
  $.DescribeVpcEndpointsResponse.vpcEndpointSet.item.dnsEntrySet.item[1].dnsName: host_not_own (expected 'vpce-00000000000000099-y3gvsl32-us-west-2a.ssm.us-west-2.vpce.amazonaws.com', got 'vpce-00000000000000099-da2wntq4-us-west-2a.ssm.us-west-2.vpce.amazonaws.com')
  $.DescribeVpcEndpointsResponse.vpcEndpointSet.item.dnsEntrySet.item[2].dnsName: host_not_own (expected 'ssm.us-west-2.amazonaws.com', got 'ssm.us-west-2.amazonaws.com')
  $.DescribeVpcEndpointsResponse.vpcEndpointSet.item.dnsEntrySet.item[3].dnsName: host_not_own (expected 'ssm.us-west-2.api.aws', got 'ssm.us-west-2.api.aws')
  Expected: the same reachable hosts on the read path.
  Actual: the stored public AWS hostnames are re-served unchanged.

## Steps to Reproduce
1. Replay wire fixture 'ec2-ssm-interface-endpoint-private-dns-converges-to-available-and-deletes-to-not-found' (trace set simparity-wire-aws-v1) against the emulator.
2. Step task#5 — CreateVpcEndpoint(VpcEndpointType='Interface', ServiceName='com.amazonaws.us-west-2.ssm', VpcId=<the fixture's VPC>, SubnetIds=[<one subnet in us-west-2a>], SecurityGroupIds=[<sg>], PrivateDnsEnabled defaulting to true). Read $.CreateVpcEndpointResponse.vpcEndpoint.dnsEntrySet.item[*].dnsName and check whether each announced host is one the emulator's data listener answers on.
3. Step task#6 — poll DescribeVpcEndpoints(VpcEndpointIds=[<the new endpoint>]) until state 'available' and read $.DescribeVpcEndpointsResponse.vpcEndpointSet.item.dnsEntrySet.item[*].dnsName; the same non-served hosts come back.
4. Both steps are reported by replay_wire_corpus as verdict 'mismatch' with reason host_not_own on every dnsEntrySet entry, including the two entries whose text matches the recording exactly.
