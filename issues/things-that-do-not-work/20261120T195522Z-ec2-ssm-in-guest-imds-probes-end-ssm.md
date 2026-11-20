Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ec2__ssm.guest-imdsv1-read-rejected-once-http-tokens-is-required ec2__ssm.imds-token-ttl-above-21600-seconds-rejected-while-the-maximum-is-served
Issue-Key: ec2__ssm.guest-imdsv1-read-rejected-once-http-tokens-is-required|run#14|AssertionError|assert before["Status"] == "Success", (
Issue-Key: ec2__ssm.imds-token-ttl-above-21600-seconds-rejected-while-the-maximum-is-served|run#16|AssertionError|assert inv["Status"] == "Success" and inv["ResponseCode"] == 0, (
Issue-Commit: a3c668f6bb53d78c1d1232fe39d40072881572f4

# ec2__ssm: in-guest IMDS probes end SSM Status 'Failed' with empty output, so neither the HttpTokens=required IMDSv1 refusal nor the token TTL ceiling is observable

> _Note: this issue was identified against the code base at commit `a3c668f6bb53d78c1d1232fe39d40072881572f4`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetCommandInvocation

## Description
One net-new root cause, grounded in two failing behavioral checks the replay report did NOT mark as already filed or owned.

Cause 1 of 1: an aws:RunShellScript invocation that probes the instance metadata service from inside a running, Online managed node ends terminal Status 'Failed' with ResponseCode 1 and empty stdout/stderr, so the IMDS behaviour the checks are about is never exercised.
Failing check ids: ec2__ssm.guest-imdsv1-read-rejected-once-http-tokens-is-required, ec2__ssm.imds-token-ttl-above-21600-seconds-rejected-while-the-maximum-is-served
  AWS:      recorded behavior only for the guest/IMDS data plane (ssm GetCommandInvocation models Status, ResponseCode, StandardOutputContent and StandardErrorContent; the IMDS HTTP surface itself is not in any Smithy model). Against real AWS: SendCommand of AWS-RunShellScript on an Online managed node reaches Status "Success" with ResponseCode 0 and StandardOutputContent equal to the script's stdout. The probed IMDS behaviour is: with the instance's MetadataOptions HttpTokens=optional, a plain GET http://169.254.169.254/latest/meta-data/instance-id answers 200 with the instance id; after ModifyInstanceMetadataOptions sets HttpTokens=required, the same token-less GET is refused with HTTP 401 while a GET carrying a token from PUT /latest/api/token still answers 200. For the token itself, PUT /latest/api/token with X-aws-ec2-metadata-token-ttl-seconds = 21600 (the maximum) answers 200 and returns a token, while a TTL above 21600 is refused with HTTP 400 and no token issued.
  Emulator: both probes end terminal Status 'Failed' with an empty StandardErrorContent and no stdout — "the pre-flip probe was 'Failed': 'Failed' stderr=''" and "the guest probe ended 'Failed' rc=1: stderr=''". The failure happens on the FIRST, pre-condition probe (before HttpTokens is flipped / before any TTL is tried), so neither the 401-vs-200 token enforcement nor the TTL ceiling is ever reached.
  Why:      A client that hardens instances by requiring IMDSv2 cannot verify the hardening from the instance, and a workload that fetches an IMDSv2 token (every modern SDK on EC2 does) sees a failed command with no diagnostics instead of either the token or an HTTP status it can act on. Because the invocation reports Failed with empty stderr, there is also nothing in the response to tell a caller why.
  Fix:      The in-guest command execution path that SSM Run Command reports on (the invocation must actually run the shell script and surface its stdout/stderr and exit code) plus the instance runtime's IMDS service, which must serve /latest/meta-data/* and PUT /latest/api/token and enforce MetadataOptions (HttpTokens=required ⇒ 401 for a token-less read) and the 21600-second token TTL ceiling (⇒ 400 above it). Related open issues in the same family that do NOT own these two checks: 20261115T000000Z-ec2-ssm-a-managed-nodes-guest-has-no-network-programs.md and 20261119T000635Z-ec2-ssm-an-aws-runshellscript-invocation-s-standardoutputcontent.md — named here so the fixer can land one coherent guest-execution fix.
  Traces:   ec2__ssm.guest-imdsv1-read-rejected-once-http-tokens-is-required, ec2__ssm.imds-token-ttl-above-21600-seconds-rejected-while-the-maximum-is-served (both mismatch).

## Evidence
expected: GetCommandInvocation Status == 'Success' and ResponseCode == 0, StandardOutputContent carrying the guest probe's output (the instance id for a token-less GET while HttpTokens=optional; the HTTP status codes for the token-less GET once HttpTokens=required; the token for a TTL of 21600 and a 400 for a TTL above it)
actual:
  AssertionError: the pre-flip probe was 'Failed': 'Failed' stderr=''                 (ec2__ssm.guest-imdsv1-read-rejected-once-http-tokens-is-required)
  AssertionError: the guest probe ended 'Failed' rc=1: stderr=''                      (ec2__ssm.imds-token-ttl-above-21600-seconds-rejected-while-the-maximum-is-served)
Both failures occur on the first probe, before the condition under test is applied; the instance id in the message (i-0000...) and the concrete run are per-run values, the Status/ResponseCode/empty-output divergence is the defect.

## Steps to Reproduce
1. RunInstances a t3.micro from the AL2023 SSM AMI parameter with an instance profile carrying AmazonSSMManagedInstanceCore, in a subnet with egress; wait until DescribeInstanceInformation reports it Online.
2. ssm SendCommand(DocumentName='AWS-RunShellScript') with a script that curls http://169.254.169.254/latest/meta-data/instance-id without a token and prints the HTTP status and body.
3. Poll GetCommandInvocation to a terminal status: the emulator reports Status 'Failed', ResponseCode 1, StandardOutputContent and StandardErrorContent empty, instead of Success/0 with the instance id.
4. (Never reached today) ModifyInstanceMetadataOptions HttpTokens=required and re-probe: the token-less GET must answer 401 while a GET with a token from PUT /latest/api/token answers 200.
5. (Never reached today) PUT /latest/api/token with X-aws-ec2-metadata-token-ttl-seconds=21600 must answer 200 with a token, and a TTL above 21600 must answer 400 with no token.
