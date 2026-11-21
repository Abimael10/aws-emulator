Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ec2.security-group-reference-appears-with-active-peering-and-clears-on-revoke ec2.ipam-provision-beyond-parent-space-accepted-then-rejected-as-failed-provision ec2.ipam-child-pool-sourced-across-scopes-rejected ec2.ipam-provision-pool-cidr-without-cidr-or-netmask-rejected ec2.ipam-vpc-missing-pool-required-resource-tag-rejected ec2.dasgrefresh-instance-refresh-replaces-every-guest-with-the-new-launch-template-version ec2.ec2-attach-volume-to-a-device-name-already-in-use-rejected ec2.nvme-instance-store-guest-device-path-rejected-as-an-ebs-attachment-point ec2.nvme-instance-store-namespace-is-nvme-while-the-xen-ebs-root-stays-xvd ec2.nvme-instance-store-serial-to-device-mapping-and-data-survive-a-reboot ec2.shutdown-api-termination-disabled-rejects-terminate-but-still-permits-stop ec2.shutdown-in-guest-halt-with-stop-behavior-leaves-it-stopped-and-restartable ec2.timesync-guest-clock-agrees-with-the-aws-api-date-header-inside-two-seconds ec2.cpucredit-unlimited-sustains-guest-throughput-on-surplus-credits ec2.secip-hot-attached-second-eni-appears-in-the-guest-as-a-new-interface-with-its-own-address ec2.timesync-chrony-in-the-guest-locks-onto-the-link-local-amazon-time-sync-source
Issue-Shape-Id: com.amazonaws.ec2#AmazonEC2
Issue-Key: ec2|AuthorizeSecurityGroupIngress|InvalidGroup.NotFound
Issue-Key: ec2|ProvisionIpamPoolCidr|InvalidParameterValue
Issue-Key: ec2.ipam-child-pool-sourced-across-scopes-rejected|run#9|AssertionError|assert msg == want_msg, f"expected {want_msg!r} for a cross-scope source, got {msg!r}"
Issue-Key: ec2.ipam-provision-pool-cidr-without-cidr-or-netmask-rejected|run#6|AssertionError|assert msg == _NO_SIZE_MSG, f"expected {_NO_SIZE_MSG!r} for a sizeless provision, got {msg!r}"
Issue-Key: ec2.ipam-vpc-missing-pool-required-resource-tag-rejected|run#9|AssertionError|assert missing_msg == _MISSING_TAG_MSG, (
Issue-Key: ec2.dasgrefresh-instance-refresh-replaces-every-guest-with-the-new-launch-template-version|_guest_generations#23|AssertionError|assert inv["Status"] == "Success", (
Issue-Key: ec2.ec2-attach-volume-to-a-device-name-already-in-use-rejected|_run_guest#24|AssertionError|assert inv["Status"] == "Success", (
Issue-Key: ec2.nvme-instance-store-guest-device-path-rejected-as-an-ebs-attachment-point|_run_guest#15|AssertionError|assert inv["Status"] == "Success", (
Issue-Key: ec2.nvme-instance-store-namespace-is-nvme-while-the-xen-ebs-root-stays-xvd|_run_guest#18|AssertionError|assert inv["Status"] == "Success", (
Issue-Key: ec2.nvme-instance-store-serial-to-device-mapping-and-data-survive-a-reboot|_run_guest#20|AssertionError|assert inv["Status"] == "Success", (
Issue-Key: ec2.shutdown-api-termination-disabled-rejects-terminate-but-still-permits-stop|_guest#14|AssertionError|assert inv["Status"] == "Success", (
Issue-Key: ec2.shutdown-in-guest-halt-with-stop-behavior-leaves-it-stopped-and-restartable|_guest#16|AssertionError|assert inv["Status"] == "Success", (
Issue-Key: ec2.timesync-guest-clock-agrees-with-the-aws-api-date-header-inside-two-seconds|run#20|AssertionError|assert inv["StandardErrorContent"] == "", (
Issue-Key: ec2.cpucredit-unlimited-sustains-guest-throughput-on-surplus-credits|run#27|ValueError|w1, w2 = int(seen["w1"]), int(seen["w2"])
Issue-Key: ec2.secip-hot-attached-second-eni-appears-in-the-guest-as-a-new-interface-with-its-own-address|_run_guest#26|AssertionError|assert inv["Status"] == "Success", (
Issue-Key: ec2.timesync-chrony-in-the-guest-locks-onto-the-link-local-amazon-time-sync-source|run#18|AssertionError|assert inv["StandardErrorContent"] == "", (
Issue-Commit: ef5208a2316f6e603f33dcb5e1ac1bc1a8b9d2da

# ec2: 8 net-new causes — AuthorizeSecurityGroupIngress refuses a peer-VPC group reference across an active peering connection, ProvisionIpamPoolCidr refuses an out-of-parent CIDR synchronously instead of settling failed-provision, three IPAM/VPC refusal messages are emulator-authored, and the guest userland has no coreutils, no shell functions and no chrony

> _Note: this issue was identified against the code base at commit `ef5208a2316f6e603f33dcb5e1ac1bc1a8b9d2da`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
AuthorizeSecurityGroupIngress

## Description
Eight net-new root causes under the suite's `### ec2` heading. Five are EC2 control-plane defects (one functional refusal, one wrong synchronous-vs-asynchronous refusal model, three emulator-authored refusal messages); three are in the in-guest execution environment the emulator presents behind SSM Run Command on an EC2 instance. The guest-side causes belong to the same family as the already-open 20261121T165349Z-ec2-ssm-3-net-new-causes-the-al2023.md (missing `aws`/`cloud-init`/`seq`+`break`), 20261115T003554Z-ec2-guests-observe-nothing-in-guest-commands-exit.md and 20261115T000000Z-ec2-ssm-a-managed-nodes-guest-has-no-network-programs.md, but none of those covers the programs named below and the suite marked none of these checks OWNED by them — they are named only for context. In every guest case the SSM control plane answers and the invocation reaches a terminal status, so the script IS dispatched and per-line stderr IS returned; what is missing is the userland.

Cause 1 of 8: AuthorizeSecurityGroupIngress refuses a UserIdGroupPair naming a security group in a peer VPC even though the VPC peering connection is active.
AWS: Smithy (com.amazonaws.ec2): AuthorizeSecurityGroupIngress input carries IpPermissions -> IpPermission.UserIdGroupPairs (UserIdGroupPair with GroupId, VpcId, VpcPeeringConnectionId, PeeringStatus), and output carries Return (boolean) plus SecurityGroupRuleSet; DescribeSecurityGroupReferences exists precisely so that a group referenced from another VPC over a peering connection can be read back, and UserIdGroupPair models VpcPeeringConnectionId/PeeringStatus for exactly this case. A rule in VPC A referencing a group in VPC B is therefore accepted once the peering connection between A and B is `active`, and DescribeSecurityGroupReferences on the referenced group then names the referencing VPC and the peering connection; after RevokeSecurityGroupIngress the reference set is empty again.
Emulator: the AuthorizeSecurityGroupIngress call is refused with HTTP 400, error code `InvalidGroup.NotFound` and message `You have specified two resources that belong to different networks.` The fixed text is that whole sentence; it carries no variable part. The emulator applies a flat same-VPC test and never consults the peering connection, and it classifies the refusal as a not-found of the group rather than as a parameter problem.
Why: cross-VPC security-group referencing over peering is the standard way to express "only the peer's application tier may reach me" without hard-coding CIDRs. A client gets a hard 400 for a configuration AWS accepts, so the rule can never be created, DescribeSecurityGroupReferences can never return anything, and any estate built on peered security-group references cannot be provisioned at all.
Fix: the AuthorizeSecurityGroupIngress handler's validation of UserIdGroupPairs — it must resolve the referenced group across VPCs and admit it when an active VPC peering connection joins the two VPCs (and record the reference so DescribeSecurityGroupReferences reports it, clearing on revoke).
Traces: ec2.security-group-reference-appears-with-active-peering-and-clears-on-revoke.

Cause 2 of 8: ProvisionIpamPoolCidr refuses a CIDR the parent pool cannot supply synchronously, instead of accepting it and settling the provision as failed.
AWS: Smithy (com.amazonaws.ec2): ProvisionIpamPoolCidr takes IpamPoolId (required), Cidr, NetmaskLength and returns IpamPoolCidr; IpamPoolCidr.State is the enum pending-provision | provisioned | failed-provision | pending-deprovision | deprovisioned | failed-deprovision | pending-import | failed-import, and IpamPoolCidr.FailureReason is a structure with Code (enum cidr-not-available | limit-exceeded) and Message, documented as "Details related to why an IPAM pool CIDR failed to be provisioned". The modelled `failed-provision` state and FailureReason exist because provisioning is asynchronous: a request the parent cannot satisfy returns HTTP 200 with State `pending-provision` and then settles at `failed-provision` carrying a FailureReason.Message, and the parent pool keeps no allocation for it. Recorded behavior: both a NetmaskLength=7 request against a 10.0.0.0/8 parent and an explicit Cidr outside the parent's space are accepted and settle failed-provision, and a later in-range request still provisions normally.
Emulator: the call is refused synchronously with HTTP 400, error code `InvalidParameterValue` and message `Invalid value '192.168.0.0/16' for cidr. It is not within a CIDR provisioned to the source IPAM pool 'ipam-pool-...'` — the fixed text is `Invalid value '<cidr>' for cidr. It is not within a CIDR provisioned to the source IPAM pool '<pool-id>'`, with the CIDR taken from the request and the pool id from the pool's source; the whole message is emulator-authored and AWS emits no error here at all.
Why: the asynchronous provisioning contract is inverted. A client that submits a provision and polls DescribeIpamPoolCidrs for `failed-provision` plus FailureReason — the only way AWS reports this condition — instead gets a terminal ClientError on the submit, so the failed-provision and FailureReason code paths are unreachable and any workflow that distinguishes "rejected request" from "accepted request that failed" is broken.
Fix: the ProvisionIpamPoolCidr handler and the IPAM pool-CIDR state machine — accept the request, record the CIDR in `pending-provision`, and have the pool's provisioning transition settle it to `failed-provision` with a FailureReason rather than validating parent containment at the request boundary.
Traces: ec2.ipam-provision-beyond-parent-space-accepted-then-rejected-as-failed-provision.

Cause 3 of 8: the CreateIpamPool cross-scope-source refusal message is emulator-authored.
AWS: Smithy (com.amazonaws.ec2): CreateIpamPool takes IpamScopeId (required), SourceIpamPoolId and AddressFamily (required). A SourceIpamPoolId naming a pool that lives in a different scope of the same IPAM is refused with error code `InvalidParameterCombination` and the message `The sourcePoolId <source-pool-id> has a scope <source-scope-id> different from input scope <requested-scope-id>.` — fixed text `The sourcePoolId ` / ` has a scope ` / ` different from input scope ` / `.`, with the three ids taken from the request and from the source pool's own scope.
Emulator: the error code is correct (`InvalidParameterCombination`) but the message is `The source IPAM pool '<source-pool-id>' is not in the IPAM scope '<requested-scope-id>'` — different wording, quoting style and punctuation, and it omits the source pool's actual scope id that AWS names.
Why: a client that keys on the refusal text (log triage, retry classification, user-facing surfacing) sees a message that is not AWS's, and the diagnostic value of being told which scope the source pool is actually in is lost.
Fix: the CreateIpamPool handler's source-pool/scope validation message.
Traces: ec2.ipam-child-pool-sourced-across-scopes-rejected.

Cause 4 of 8: the ProvisionIpamPoolCidr sizeless-request refusal message is emulator-authored.
AWS: Smithy (com.amazonaws.ec2): on ProvisionIpamPoolCidr both Cidr and NetmaskLength are optional members, so exactly one must be supplied; a request with neither is refused with the fixed message `The request must contain either a CIDR or Netmask Length.` (no variable part).
Emulator: the message is `The request cannot contain empty CIDR and Netmask Length. Exactly one of them should be provided.` — two sentences of emulator-authored text in place of AWS's one.
Why: same as cause 3 — the refusal text is a requirement for clients that match on it, and this one has no variable part at all, so the divergence is purely invented wording.
Fix: the ProvisionIpamPoolCidr handler's input validation message.
Traces: ec2.ipam-provision-pool-cidr-without-cidr-or-netmask-rejected.

Cause 5 of 8: the CreateVpc refusal for an IPAM pool's AllocationResourceTags is emulator-authored and leaks state into the message.
AWS: Smithy (com.amazonaws.ec2): CreateVpc takes Ipv4IpamPoolId / Ipv4NetmaskLength and TagSpecifications; CreateIpamPool takes AllocationResourceTags (RequestIpamResourceTagList), which gate which resources may draw a CIDR from the pool. A CreateVpc from such a pool whose tags do not satisfy the rule — whether the required key is absent or present with the wrong value — is refused with error code `InvalidParameterValue` and the single fixed message `The resource is missing one or more of the resource tags required by the IPAM pool.` AWS uses that same text for both the missing-key and wrong-value cases and names neither the tag nor the pool.
Emulator: the error code is correct (`InvalidParameterValue`) but the message is `The resource does not have the tag <key>=<value>, which pool <ipam-pool-id> requires of the resources that use CIDRs from it.` — entirely emulator-authored, and it interpolates the pool's required tag key/value and the pool id, values AWS deliberately does not disclose here.
Why: a client matching AWS's fixed sentence never matches, and the emulator's message implies a per-tag diagnostic contract that AWS does not provide, so code written against the emulator breaks on AWS.
Fix: the CreateVpc handler's IPAM allocation-resource-tag validation message (one constant message for both the missing-key and mismatched-value cases).
Traces: ec2.ipam-vpc-missing-pool-required-resource-tag-rejected.

Cause 6 of 8: the guest userland has no coreutils — `seq`, `awk`, `sed`, `sync`, `date` and `curl` are all "command not found", so in-guest scripts exit 127.
AWS: the Amazon Linux 2023 AMI resolved through /aws/service/ami-amazon-linux-latest/al2023-ami-kernel-default-x86_64 ships coreutils, gawk, sed and curl on PATH, and `AWS-RunShellScript` runs the submitted lines as root in a POSIX shell with that PATH. Smithy (com.amazonaws.ssm): GetCommandInvocation output carries Status (CommandInvocationStatus), ResponseCode (integer), StandardOutputContent (@length 0..24000) and StandardErrorContent (@length 0..8000); a script whose lines all succeed returns Status `Success`, ResponseCode 0 and a zero-length StandardErrorContent.
Emulator: GetCommandInvocation returns Status `Failed` with ResponseCode 127 and StandardErrorContent made of the shell's `<shell>: <name>: command not found` lines — observed as `sh: seq: command not found`, `sh: awk: command not found`, `sh: sed: command not found`, `sh: sync: command not found`, `sh: date: command not found`, `sh: curl: command not found`. The fixed text is the shell's `sh: ` / `: command not found` form; the variable part is the program name taken from the submitted script. The correct behavior is that no such line is produced at all.
Why: every guest-observable behaviour these scenarios exist to prove — block-device inventory, NVMe namespace/serial mapping, writing a sentinel and flushing it before a stop, instance-refresh generation reads — is unreachable, and a client cannot tell "the instance has no such device" from "the emulator has no `sed`". Any customer bootstrap script that uses ordinary coreutils fails on the emulator with a shell error instead of running.
Fix: the guest userland/image behind the EC2 instance data plane and the PATH the `AWS-RunShellScript` plugin executes with — not the EC2 or SSM API handlers, which report the terminal status faithfully.
Traces: ec2.dasgrefresh-instance-refresh-replaces-every-guest-with-the-new-launch-template-version (stderr `sh: seq: command not found\nsh: break: command not found\n`), ec2.ec2-attach-volume-to-a-device-name-already-in-use-rejected (rc=127, `sh: seq`/`sh: awk`), ec2.nvme-instance-store-guest-device-path-rejected-as-an-ebs-attachment-point, ec2.nvme-instance-store-namespace-is-nvme-while-the-xen-ebs-root-stays-xvd, ec2.nvme-instance-store-serial-to-device-mapping-and-data-survive-a-reboot (all three rc=127, `sh: sed: command not found`), ec2.shutdown-api-termination-disabled-rejects-terminate-but-still-permits-stop and ec2.shutdown-in-guest-halt-with-stop-behavior-leaves-it-stopped-and-restartable (`sh: sync: command not found`), ec2.timesync-guest-clock-agrees-with-the-aws-api-date-header-inside-two-seconds (`sh: date`, `sh: awk`, `sh: curl`).

Cause 7 of 8: the guest shell does not implement user-defined shell functions, so a defined function is "command not found" and the variables it was to set stay unset.
AWS: POSIX requires a shell to support function definitions (`name() { ...; }`) and to invoke them as commands, including inside command substitution; `AWS-RunShellScript` on Amazon Linux 2023 runs the submitted body through such a shell. Smithy (com.amazonaws.ssm): a script whose lines all succeed yields GetCommandInvocation Status `Success`, ResponseCode 0 and empty StandardErrorContent.
Emulator: a script that defines `burn() { ... }` or `addrs() { ... }` and then calls it gets `sh: burn: command not found` / `sh: addrs: command not found`, and the assignments fed from those calls come back empty — observed as a report line with `w1=` and `w2=` empty while `nproc=$(nproc)` in the same line resolved, and as `sh: PIF: parameter not set` after `set -- $(addrs <mac>); PIF=$1`. Fixed text is again the shell's `sh: <name>: command not found` and `sh: <name>: parameter not set`, with the function/variable name taken from the script; the correct behavior is that the function runs and its stdout is substituted.
Why: command substitution over a helper function is the ordinary way a Run Command script measures or collects anything, so the emulator silently produces empty values while still returning a terminal status. A client reading those values gets '' where AWS gives a measurement, and callers that `int()` the result crash rather than observing a divergence.
Fix: the guest shell behind the `AWS-RunShellScript` plugin — execute the script body as one POSIX shell program (function definitions, loops and builtins intact) instead of dispatching lines as individual commands.
Traces: ec2.cpucredit-unlimited-sustains-guest-throughput-on-surplus-credits (ValueError: invalid literal for int() with base 10: '' — `w1`/`w2` empty from `$(burn N)`), ec2.secip-hot-attached-second-eni-appears-in-the-guest-as-a-new-interface-with-its-own-address (rc=1, `sh: addrs: command not found`, `sh: md: command not found`, `sh: PIF: parameter not set`).

Cause 8 of 8: the guest has no chrony, so the Amazon Time Sync source cannot be observed.
AWS: Amazon Linux 2023 ships chrony, configured against the link-local Amazon Time Sync Service at 169.254.169.123, and `chronyc` is on PATH in the guest; `chronyc sources`/`tracking` report that source as the one the guest is locked onto. recorded behavior only (no Smithy member describes guest state): the recording shows `chronyc` answering with the link-local source selected and the guest clock agreeing with the AWS API `Date` header.
Emulator: the in-guest probe's StandardErrorContent carries repeated `sh: chronyc: command not found` lines (fixed text `sh: <name>: command not found`, with `chronyc` the program name from the script), so no time source is observable at all; AWS produces an empty StandardErrorContent here.
Why: a client cannot verify that an instance is synchronised against the Amazon Time Sync Service — the prerequisite for request signing, certificate validity and any timestamp-ordered workload — and clock-drift diagnostics built on chrony are unreachable.
Fix: the guest userland/image behind the EC2 instance data plane: provide chrony (with `chronyc`) bound to the link-local 169.254.169.123 time source.
Traces: ec2.timesync-chrony-in-the-guest-locks-onto-the-link-local-amazon-time-sync-source, ec2.timesync-guest-clock-agrees-with-the-aws-api-date-header-inside-two-seconds.

Handler files for the control-plane causes live under services/service-com-amazonaws-ec2-amazonec2/src/ops/ (authorize_security_group_ingress.rs, provision_ipam_pool_cidr.rs, create_ipam_pool.rs, create_vpc.rs).

## Evidence
Suite: simparity-behavioral-aws-v1 — 1444/2191 passed (65.9%). Service heading `### ec2`: 939/1368 (69%), 428 failed. The 16 checks below are the ones the report did NOT mark "ALREADY FILED" or "OWNED by" under that heading.

Cause 1 — ec2.security-group-reference-appears-with-active-peering-and-clears-on-revoke [error]:
  Expected: AuthorizeSecurityGroupIngress(GroupId=<group in VPC A>, IpPermissions=[{tcp 443, UserIdGroupPairs:[{GroupId:<group in VPC B>}]}]) succeeds (Return=true) while the A<->B peering connection is `active`; DescribeSecurityGroupReferences(GroupId=[<group in VPC B>]) then names VPC A and the peering connection, and reads [] again after the revoke.
  Actual:   botocore.exceptions.ClientError: An error occurred (InvalidGroup.NotFound) when calling the AuthorizeSecurityGroupIngress operation: You have specified two resources that belong to different networks.

Cause 2 — ec2.ipam-provision-beyond-parent-space-accepted-then-rejected-as-failed-provision [error]:
  Expected: ProvisionIpamPoolCidr(IpamPoolId=<child of a 10.0.0.0/8 pool>, Cidr='192.168.0.0/16') returns HTTP 200 with IpamPoolCidr.State 'pending-provision', settling to 'failed-provision' with a non-empty FailureReason.Message, and the parent pool holds no allocation for it; a NetmaskLength=16 request afterwards still reaches 'provisioned'.
  Actual:   botocore.exceptions.ClientError: An error occurred (InvalidParameterValue) when calling the ProvisionIpamPoolCidr operation: Invalid value '192.168.0.0/16' for cidr. It is not within a CIDR provisioned to the source IPAM pool 'ipam-pool-000000000000000a7'

Cause 3 — ec2.ipam-child-pool-sourced-across-scopes-rejected [mismatch]:
  Expected (code matched, message asserted): InvalidParameterCombination / 'The sourcePoolId ipam-pool-...a7 has a scope ipam-scope-...a6 different from input scope ipam-scope-...a9.'
  Actual:   "The source IPAM pool 'ipam-pool-000000000000000a7' is not in the IPAM scope 'ipam-scope-000000000000000a9'"

Cause 4 — ec2.ipam-provision-pool-cidr-without-cidr-or-netmask-rejected [mismatch]:
  Expected: 'The request must contain either a CIDR or Netmask Length.'
  Actual:   'The request cannot contain empty CIDR and Netmask Length. Exactly one of them should be provided.'

Cause 5 — ec2.ipam-vpc-missing-pool-required-resource-tag-rejected [mismatch]:
  Expected (code InvalidParameterValue matched): 'The resource is missing one or more of the resource tags required by the IPAM pool.' for BOTH the untagged VPC and the env=prod VPC.
  Actual:   'The resource does not have the tag env=dev, which pool ipam-pool-000000000000000a9 requires of the resources that use CIDRs from it.'

Cause 6 — in-guest coreutils absent, GetCommandInvocation Status 'Failed', ResponseCode 127, empty stdout:
  ec2.dasgrefresh-instance-refresh-replaces-every-guest-with-the-new-launch-template-version: stderr='sh: seq: command not found\nsh: break: command not found\n'
  ec2.ec2-attach-volume-to-a-device-name-already-in-use-rejected: rc=127 stdout='' stderr='sh: seq: command not found\nsh: awk: command not found\n'
  ec2.nvme-instance-store-guest-device-path-rejected-as-an-ebs-attachment-point: rc=127 stdout='' stderr='sh: sed: command not found\n'
  ec2.nvme-instance-store-namespace-is-nvme-while-the-xen-ebs-root-stays-xvd: rc=127 stdout='' stderr='sh: sed: command not found\n'
  ec2.nvme-instance-store-serial-to-device-mapping-and-data-survive-a-reboot: rc=127 stdout='' stderr='sh: sed: command not found\n'
  ec2.shutdown-api-termination-disabled-rejects-terminate-but-still-permits-stop: stderr='sh: sync: command not found\n'
  ec2.shutdown-in-guest-halt-with-stop-behavior-leaves-it-stopped-and-restartable: stderr='sh: sync: command not found\n'
  ec2.timesync-guest-clock-agrees-with-the-aws-api-date-header-inside-two-seconds: stderr contains 'sh: seq: command not found', 'sh: awk: command not found', 'sh: date: command not found', 'sh: curl: command not found'
  Expected in every case: Status 'Success', ResponseCode 0, StandardErrorContent ''.

Cause 7 — user-defined shell functions not executed:
  ec2.cpucredit-unlimited-sustains-guest-throughput-on-surplus-credits: the guest report line parsed with w1='' and w2='' while nproc resolved to '2' -> ValueError: invalid literal for int() with base 10: ''. The script defines `burn() { ... echo "$n"; }` and reads `W1=$(burn 20)`.
  ec2.secip-hot-attached-second-eni-appears-in-the-guest-as-a-new-interface-with-its-own-address: rc=1 stdout='' stderr='sh: addrs: command not found\nsh: addrs: command not found\nsh: md: command not found\nsh: PIF: parameter not set\n'. The script defines `addrs() { ... }` and does `set -- $(addrs <mac>); PIF=$1`.
  Expected in both: the function runs, its stdout is substituted, Status 'Success' with empty StandardErrorContent.

Cause 8 — chrony absent:
  ec2.timesync-chrony-in-the-guest-locks-onto-the-link-local-amazon-time-sync-source: AssertionError: the guest script wrote to stderr: 'sh: chronyc: command not found\nsh: seq: command not found\nsh: chronyc: command not found\nsh: awk: command not found\nsh: chronyc: command not found\nsh: chronyc: command not found\n'
  ec2.timesync-guest-clock-agrees-with-the-aws-api-date-header-inside-two-seconds: same, with 'sh: chronyc: command not found' repeated.
  Expected: StandardErrorContent '' and `chronyc` reporting the 169.254.169.123 link-local Amazon Time Sync source as selected.

Message-wording note: in every "command not found" line the fixed text is the shell's own `sh: ` ... `: command not found`; the variable part is the program name taken from the submitted script. The requirement is that no such line is produced.

## Steps to Reproduce
Cause 1: CreateVpc 10.214.0.0/16 (requester) and 10.215.0.0/16 (accepter); wait vpc_available. CreateSecurityGroup in the accepter VPC (referenced) and one in the requester VPC (referencing). CreateVpcPeeringConnection(VpcId=requester, PeerVpcId=accepter), AcceptVpcPeeringConnection, poll DescribeVpcPeeringConnections until Status.Code == 'active'. Then AuthorizeSecurityGroupIngress(GroupId=<referencing>, IpPermissions=[{IpProtocol:'tcp', FromPort:443, ToPort:443, UserIdGroupPairs:[{GroupId:<referenced>}]}]). Observe InvalidGroup.NotFound 'You have specified two resources that belong to different networks.'; AWS accepts, and DescribeSecurityGroupReferences(GroupId=[<referenced>]) then lists the requester VPC and the peering connection id (and [] after RevokeSecurityGroupIngress of the same permission).

Causes 2-4: CreateIpam in the Region, read its private scope, CreateIpamPool(IpamScopeId=<scope>, AddressFamily='ipv4', Locale=<region>) as the top pool and ProvisionIpamPoolCidr(Cidr='10.0.0.0/8') on it; CreateIpamPool(SourceIpamPoolId=<top>) as a child.
  2: ProvisionIpamPoolCidr(IpamPoolId=<child>, Cidr='192.168.0.0/16') and ProvisionIpamPoolCidr(IpamPoolId=<child>, NetmaskLength=7). Observe the synchronous InvalidParameterValue; AWS returns 200 with State 'pending-provision' which settles to 'failed-provision' with FailureReason.Message, leaves no allocation on the parent, and still provisions a later NetmaskLength=16 request.
  3: CreateIpamScope(IpamId=<ipam>) for a second scope, then CreateIpamPool(IpamScopeId=<second scope>, SourceIpamPoolId=<top pool in the first scope>, Locale=<region>, AddressFamily='ipv4'). Compare the InvalidParameterCombination message against 'The sourcePoolId <src> has a scope <src-scope> different from input scope <req-scope>.'
  4: ProvisionIpamPoolCidr(IpamPoolId=<child>) with neither Cidr nor NetmaskLength. Compare the message against 'The request must contain either a CIDR or Netmask Length.'

Cause 5: CreateIpamPool(SourceIpamPoolId=<top>, AllocationResourceTags=[{Key:'env',Value:'dev'}]) and ProvisionIpamPoolCidr(NetmaskLength=16) on it. Then CreateVpc(Ipv4IpamPoolId=<child>, Ipv4NetmaskLength=24) with no tags, and again with TagSpecifications=[{ResourceType:'vpc', Tags:[{Key:'env',Value:'prod'}]}]. Both must be InvalidParameterValue with the single message 'The resource is missing one or more of the resource tags required by the IPAM pool.'; observe the emulator's per-tag message instead. A CreateVpc tagged env=dev must succeed and appear as an allocation in the pool.

Causes 6-8 (common setup): create a VPC with an internet gateway, a default route and a public subnet with MapPublicIpOnLaunch; create an IAM role trusted by ec2.amazonaws.com with arn:aws:iam::aws:policy/AmazonSSMManagedInstanceCore and an instance profile holding it; resolve ImageId from SSM parameter /aws/service/ami-amazon-linux-latest/al2023-ami-kernel-default-x86_64; RunInstances a t3.micro into the subnet with that profile; poll DescribeInstances to 'running' and DescribeInstanceInformation to PingStatus 'Online'.
  6: ssm.send_command(DocumentName='AWS-RunShellScript', Parameters={'commands':['sed -n 1p /proc/mounts', 'seq 1 3', "awk '{print $1}' /proc/uptime", 'sync', 'date +%s', 'curl -s -o /dev/null http://169.254.169.254/latest/meta-data/']}) and poll GetCommandInvocation. Observe Status 'Failed', ResponseCode 127 and 'sh: <name>: command not found' per line; AWS returns Status 'Success', ResponseCode 0, StandardErrorContent ''.
  7: send_command with commands=['f() { echo 42; }', 'V=$(f)', 'echo "MARK v=$V"']. Observe 'sh: f: command not found' and MARK v= empty; AWS prints 'MARK v=42' with empty stderr.
  8: send_command with commands=['chronyc -n sources', 'chronyc tracking']. Observe 'sh: chronyc: command not found'; AWS lists the 169.254.169.123 link-local Amazon Time Sync source as the selected one with empty stderr.
