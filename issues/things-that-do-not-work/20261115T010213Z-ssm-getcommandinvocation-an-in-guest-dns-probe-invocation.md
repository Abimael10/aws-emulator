Issue-Source: trace-sweep-divergence
Issue-Trace: route53resolver-dns-firewall-override-answers-in-vpc
Issue-Step: task#21
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: route53resolver-dns-firewall-override-answers-in-vpc
Issue-Shape-Id: com.amazonaws.route53resolver#Route53Resolver
Issue-Key: ssm|GetCommandInvocation|2xx->2xx|$.ResponseCode
Issue-Key: ssm|GetCommandInvocation|2xx->2xx|$.Status
Issue-Key: ssm|GetCommandInvocation|2xx->2xx|$.StatusDetails
Issue-Related: 20261114T235914Z-ssm-run-command-responses-diverge-sendcommand-and-getcommandinvocation.md
Issue-Commit: 4f3c099690bf1a1a45af47b8a28528c58ba9b1de

# ssm GetCommandInvocation: an in-guest DNS probe invocation ends Failed with ResponseCode 127 and drops modeled response members

> _Note: this issue was identified against the code base at commit `4f3c099690bf1a1a45af47b8a28528c58ba9b1de`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetCommandInvocation

## Description
2 net-new causes in the ssm Run Command read path, both exercised by the same recorded step: a Route 53 Resolver DNS Firewall scenario sends an AWS-RunShellScript command that resolves a name from inside a managed node and then polls GetCommandInvocation for the result. Both causes must be closed before this issue is submitted.

Cause 1 of 2 - the invocation never succeeds: GetCommandInvocation reports a terminal failure where AWS reports a successful execution with the guest program's output (fixture route53resolver-dns-firewall-override-answers-in-vpc, step task#21, HTTP 200 vs 200).
AWS: Smithy (com.amazonaws.ssm, awsJson1_1) models GetCommandInvocationResult.Status : CommandInvocationStatus (enum), StatusDetails : StatusDetails (@length 0..100), ResponseCode : ResponseCode (@default 0) and StandardOutputContent : StandardOutputContent (@length 0..24000). For a command whose guest command exits 0, AWS reports Status 'Success', StatusDetails 'Success', ResponseCode 0 and StandardOutputContent carrying exactly what the command wrote to stdout. The recording shows that: the DNS probe run in the guest prints one line naming the resolved CNAME target and the alias list it was asked about, terminated by a newline. The concrete host names and the probe-run suffix in the recorded line are per-run values taken from the request's probe script and the scenario's firewall configuration - the requirement is that the program the request ships runs and its stdout is returned verbatim, not that one recording's host names appear.
Emulator: the invocation reaches a terminal FAILURE instead - Status 'Failed', StatusDetails 'Failed', ResponseCode 127 and StandardOutputContent '' (empty). The replay's poll on Status hit 'Failed' (a fail_on value) after 2 attempts, so the scenario can never converge. ResponseCode 127 is the POSIX shell's 'command not found' exit status. Hypothesis: the managed node's guest execution path has no interpreter/resolver program for the probe the command ships, so the shell exits 127 before any output is produced; nothing in the model or the recording states that, and the emulator returns no StandardErrorContent to say which program was missing.
Why: the whole purpose of this read is to collect the output of work done on the node. A client that sends a command and polls for its result is told the command definitively failed with an exit status it never produced, and gets no stdout and no stderr to diagnose it. Any flow gated on the invocation succeeding (here, observing which answer a DNS Firewall rule group returns inside a VPC) terminates in a false failure rather than a wrong-but-readable answer, and a Failed invocation is not retryable by the caller.
Fix: the ssm Run Command guest-execution path (the invocation executor that runs an AWS-RunShellScript document's commands inside a managed node) and the invocation state it writes - the shipped command must actually execute and its captured stdout/stderr and real exit status must be stored on the invocation record that GetCommandInvocation projects, instead of a 127/Failed invocation with empty output.
Traces: route53resolver-dns-firewall-override-answers-in-vpc#task#21.

Cause 2 of 2 - the same response reports the wrong PluginName and omits members AWS always serializes (same fixture, same step task#21, HTTP 200 vs 200).
AWS: Smithy models GetCommandInvocationResult.PluginName : CommandPluginName (@length min 4), Comment : Comment (@length 0..100) and CloudWatchOutputConfig : CloudWatchOutputConfig (members CloudWatchLogGroupName, CloudWatchOutputEnabled @default false). PluginName is the invoked document's plugin/action name - for AWS-RunShellScript that is 'aws:runShellScript', and it is also accepted back as GetCommandInvocation's optional PluginName input (@length min 4). When the command set neither a comment nor CloudWatch output, AWS still serializes Comment as the empty string and CloudWatchOutputConfig as the object with its defaulted members (empty log-group name, CloudWatchOutputEnabled false).
Emulator: PluginName is 'runShellScriptOnLinux' - an internal platform-specific label rather than the document's plugin name - and Comment and CloudWatchOutputConfig are absent from the body (JSON null / key not serialized).
Why: callers feed PluginName back into GetCommandInvocation and key per-plugin output (S3 prefixes, CloudWatch Logs streams) by it, so a label the service will not accept on input breaks that round trip and per-plugin output lookups find nothing. Clients that model Comment/CloudWatchOutputConfig as present decode null and either fail or conclude the command was accepted with settings it did not have.
Fix: the ssm GetCommandInvocation handler/serializer - derive PluginName from the invoked document's plugin/action name, and project Comment and CloudWatchOutputConfig from the stored command record unconditionally instead of skipping None.
Traces: route53resolver-dns-firewall-override-answers-in-vpc#task#21.

Related open issues, none of which carries this replay key, so none covers these causes: 20261114T235914Z-ssm-run-command-responses-diverge-sendcommand-and-getcommandinvocation.md (the same PluginName/Comment/CloudWatchOutputConfig projection fault, but keyed to fixture ec2-instance-connect-pushed-key-served-by-instance-metadata#task#15 and to a Success invocation), 20261114T161545Z-ec2-route53-route53resolver-ssm-an-aws-runshellscript-script.md and 20261114T172248Z-ec2-s3-sqs-ssm-aws-runshellscript-invocations-that.md (guest scripts failing with rc 127 in other scenarios), 20261114T200121Z-ec2-elbv2-ssm-getcommandinvocation-reports-status-success-with.md (Success with empty output) and 20261115T000000Z-ec2-ssm-a-managed-nodes-guest-has-no-network-programs.md. The SendCommand step that precedes this one in the same fixture (task#20) is already owned by 20261114T235914Z and is not refiled here.

## Evidence
Replay of fixture route53resolver-dns-firewall-override-answers-in-vpc (trace set simparity-wire-aws-v1) against the emulator; this step is the one divergence in the run that no open issue owns (owned_by: [], stub_path: "").

Step task#21, ssm GetCommandInvocation, expected HTTP 200 / actual HTTP 200, body mismatch:
  $.Status: expected 'Success', got 'Failed'
  $.StatusDetails: expected 'Success', got 'Failed'
  $.ResponseCode: expected 0, got 127
  $.StandardOutputContent: expected the stdout of the DNS probe the command shipped - one line naming the resolved CNAME target and the alias list queried, newline-terminated (recording: "cname example.com aliases ['sim2real-probe-r53r-blocked-9f418358.example.org']\n"; the host names and probe suffix are per-run values from the request/scenario), got ''
  $.Status (poll): 'Failed' is in the step's fail_on set after 2 attempt(s) - expected 'Success', got 'Failed'
  $.PluginName: expected 'aws:runShellScript', got 'runShellScriptOnLinux'
  $.Comment: expected '' (empty string), got None
  $.CloudWatchOutputConfig: expected {'CloudWatchLogGroupName': '', 'CloudWatchOutputEnabled': False}, got None

No StandardErrorContent is returned with the 127, so the failure carries no diagnostic text at all.

Model evidence: Smithy com.amazonaws.ssm GetCommandInvocation -> GetCommandInvocationResult with members Status : CommandInvocationStatus (enum), StatusDetails : StatusDetails (@length 0..100), ResponseCode : ResponseCode (@default 0, integer), StandardOutputContent (@length 0..24000), StandardErrorContent (@length 0..8000), PluginName : CommandPluginName (@length min 4), Comment : Comment (@length 0..100), CloudWatchOutputConfig : CloudWatchOutputConfig; GetCommandInvocationRequest also accepts PluginName : CommandPluginName (@length min 4), which is why the returned label must be the document's plugin name. CloudWatchOutputConfig.CloudWatchOutputEnabled is @default false.

## Steps to Reproduce
1. Replay wire fixture route53resolver-dns-firewall-override-answers-in-vpc from trace set simparity-wire-aws-v1 against the emulator.
2. Reach step task#20: ssm SendCommand for AWS-RunShellScript against the scenario's managed node, shipping the in-guest DNS probe, without setting Comment or CloudWatch output options. Note the returned CommandId.
3. Step task#21: poll ssm GetCommandInvocation with that CommandId and the node's InstanceId until a terminal status.
4. Observe the invocation reaches Status 'Failed' / StatusDetails 'Failed' with ResponseCode 127 and an empty StandardOutputContent (and no StandardErrorContent), instead of Status/StatusDetails 'Success', ResponseCode 0 and the probe's stdout line; the step's poll fails on 'Failed' after 2 attempts.
5. In the same 200 response, observe PluginName is 'runShellScriptOnLinux' rather than the document's plugin name 'aws:runShellScript', and that Comment and CloudWatchOutputConfig are absent (null) rather than '' and {'CloudWatchLogGroupName': '', 'CloudWatchOutputEnabled': false}.
