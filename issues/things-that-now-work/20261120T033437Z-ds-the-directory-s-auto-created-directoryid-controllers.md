Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ds.simple-ad-small-lifecycle-to-active-then-deleted
Issue-Shape-Id: com.amazonaws.directoryservice#DirectoryService_20150416
Issue-Key: ds.simple-ad-small-lifecycle-to-active-then-deleted|run#67|AssertionError|assert permissions, (
Issue-Commit: 4f996ebba5f78af5b839bd456729e9857e1eb1d7

# ds: the directory's auto-created <directoryId>_controllers security group is minted with no ingress rules, so the domain controllers are unreachable and IpPermissions is empty

> _Note: this issue was identified against the code base at commit `4f996ebba5f78af5b839bd456729e9857e1eb1d7`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateMicrosoftAD

## Description
One net-new root cause.

Cause 1 of 1: Directory Service mints the domain-controller security group but never populates its inbound rules.
Failing check id: ds.simple-ad-small-lifecycle-to-active-then-deleted

AWS: when CreateMicrosoftAD (and CreateDirectory for Simple AD) provisions a directory into the caller's VPC, Directory Service creates, alongside the two domain-controller ENIs, a security group in that VPC named `<directoryId>_controllers` with the description `AWS created security group for <directoryId> directory controllers`, attaches it to those ENIs, and POPULATES it with inbound (ingress) rules admitting the Active Directory protocol traffic the controllers serve, each rule scoped to the directory's own VPC CIDR rather than to 0.0.0.0/0. The Smithy model says nothing about the rules — DirectoryVpcSettingsDescription.SecurityGroupId is documented only as "The domain controller security group identifier for the directory" — so this is recorded/documented behavior: the AWS Directory Service documentation for what AWS Managed Microsoft AD creates lists the inbound set (DNS 53 TCP/UDP, Kerberos 88 TCP/UDP and 464 TCP/UDP, NTP 123 UDP, RPC endpoint mapper 135 TCP, NetBIOS 137-138 UDP / 139 TCP, LDAP 389 TCP/UDP, SMB 445 TCP/UDP, LDAPS 636 TCP, global catalog 3268-3269 TCP, AD Web Services 9389 TCP, the dynamic RPC range, and ICMP), with all outbound traffic allowed. The invariant the recording asserts is the general one: DescribeSecurityGroups on the directory's SecurityGroupId returns a group whose IpPermissions is NON-EMPTY, and every IpRanges entry across those rules carries the directory VPC's own CIDR (the CIDR the caller passed to CreateVpc), never a wider range.

Emulator: the group is created with the right GroupName, Description, VpcId and the expected allow-all egress rule, and the two controller ENIs are correctly placed in it, but `IpPermissions` comes back as the empty list `[]`. Replay output for the group the emulator minted:
  {'GroupId': 'sg-...', 'GroupName': 'd-<id>_controllers', 'Description': 'AWS created security group for d-<id> directory controllers', 'VpcId': 'vpc-...', 'IpPermissions': [], 'IpPermissionsEgress': [{'IpProtocol': '-1', 'IpRanges': [{'CidrIp': '0.0.0.0/0'}], ...}]}
The group id, directory id, VPC id and account id above are per-run values that fill the template; the empty `IpPermissions` and the present allow-all egress are the behavior. The check fails at `assert permissions` with "the domain controllers must be reachable, so the group must carry ingress rules". Because ingress is empty, the downstream assertion that every ingress IpRanges CIDR equals the VPC CIDR is never even reached, so the scoping half of the behavior is also unverified in the emulator.

Why: a client that relies on AWS's behavior gets two things wrong. (1) Reachability reasoning: an empty-ingress group means nothing can reach the controllers, so any code or test that verifies "my workload's subnet can talk to the directory on LDAP/Kerberos/DNS" by reading the directory's security group concludes the directory is unusable, while on AWS it concludes the opposite. (2) Rule-management and audit flows: tooling that reads the AWS-authored baseline and then adds, removes or diffs rules (CIDR-scope audits, drift detection, least-privilege checks, AuthorizeSecurityGroupIngress/RevokeSecurityGroupIngress against the AWS-created rules) sees no baseline at all — a revoke of an AWS-authored rule has nothing to revoke, and a scope audit that should flag an over-broad rule trivially passes on an empty set. Both failures are silent: the call succeeds and the SecurityGroupId is present and plausible.

Fix: the directory-provisioning side effect of the CreateMicrosoftAD / CreateDirectory handler in the directoryservice service crate — the same code path that already mints the `<directoryId>_controllers` group, its description and the controller ENIs, and that records VpcSettings.SecurityGroupId. It must additionally author the inbound rules into that group's EC2 state (so DescribeSecurityGroups on the shared EC2 state returns them), each scoped to the CIDR of the VPC named in VpcSettings.VpcId rather than 0.0.0.0/0. Egress is already correct and should not change.

Traces: ds.simple-ad-small-lifecycle-to-active-then-deleted (asserts non-empty IpPermissions on the directory's SecurityGroupId, then that every ingress IpRanges CIDR is the directory VPC's CIDR).

## Evidence
Check ds.simple-ad-small-lifecycle-to-active-then-deleted, assertion at check line 390:

  permissions = group.get("IpPermissions") or []
  assert permissions, (
      f"the domain controllers must be reachable, so the group must carry ingress rules: {group}"
  )

Expected (AWS): DescribeSecurityGroups(GroupIds=[DescribeDirectories(...).VpcSettings.SecurityGroupId]) returns one group in the directory's VPC named <directoryId>_controllers whose IpPermissions is non-empty (the AD protocol inbound set AWS authors), and every IpRanges entry in those rules carries the directory VPC's own CIDR.

Actual (emulator): the group exists and is otherwise correct, but IpPermissions == []:
  AssertionError: the domain controllers must be reachable, so the group must carry ingress rules: {'GroupId': 'sg-000000000000000bd', 'IpPermissionsEgress': [{'IpProtocol': '-1', 'UserIdGroupPairs': [], 'IpRanges': [{'CidrIp': '0.0.0.0/0'}], 'Ipv6Ranges': [], 'PrefixListIds': []}], 'Tags': [], 'VpcId': 'vpc-000000000000000a1', 'SecurityGroupArn': 'arn:aws:ec2:us-west-2:000000000000:security-group/sg-000000000000000bd', 'OwnerId': '000000000000', 'GroupName': 'd-003ba5fe5a_controllers', 'Description': 'AWS created security group for d-003ba5fe5a directory controllers', 'IpPermissions': []}

Everything earlier in the check passed: the directory reached Active, VpcSettings echoed the VPC/subnets/AZs, SecurityGroupId was minted, both ENIs were created with the AWS description and placed in the group, and DnsIpAddrs matched the ENIs' private addresses. The sole divergence is the missing ingress rule set. The sg-/vpc-/d- ids and the account/region in the ARN are per-run values, not requirements.

Smithy (ds model com-amazonaws-directoryservice-directoryservice-20150416): DirectoryVpcSettingsDescription.SecurityGroupId — "The domain controller security group identifier for the directory." The model constrains nothing about rule contents, hence "recorded/documented behavior only" for the rule set itself; the non-empty-and-VPC-scoped invariant is what the recording asserts.

## Steps to Reproduce
1. ec2.create_vpc(CidrBlock="10.241.0.0/16") and create two subnets in two distinct availability zones of that VPC.
2. ds.create_microsoft_ad(Name="<unique>.example.com", ShortName="SIM2REAL", Password="<strong>", Edition="Standard", Description="...", VpcSettings={"VpcId": vpc_id, "SubnetIds": subnet_ids}) and poll ds.describe_directories(DirectoryIds=[directory_id]) until Stage == "Active".
3. Read security_group_id = DirectoryDescriptions[0]["VpcSettings"]["SecurityGroupId"].
4. ec2.describe_security_groups(GroupIds=[security_group_id]).
5. Observe SecurityGroups[0]["IpPermissions"] == [] (only the allow-all egress rule is present). On AWS it is a non-empty inbound rule set for the AD protocol ports, every IpRanges entry scoped to 10.241.0.0/16 — the VPC CIDR from step 1.

Replay: run_behavioral_suite and re-run check ds.simple-ad-small-lifecycle-to-active-then-deleted.
