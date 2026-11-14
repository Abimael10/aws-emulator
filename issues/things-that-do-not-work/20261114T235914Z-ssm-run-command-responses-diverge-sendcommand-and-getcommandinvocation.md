Issue-Source: trace-sweep-divergence
Issue-Trace: ec2-instance-connect-pushed-key-served-by-instance-metadata
Issue-Step: task#14
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: ec2-instance-connect-pushed-key-served-by-instance-metadata
Issue-Shape-Id: com.amazonaws.ec2instanceconnect#AWSEC2InstanceConnectService
Issue-Key: ssm|SendCommand|2xx->2xx|$.Command.AlarmConfiguration
Issue-Key: ssm|SendCommand|2xx->2xx|$.Command.ClientName
Issue-Key: ssm|SendCommand|2xx->2xx|$.Command.ClientSourceId
Issue-Key: ssm|SendCommand|2xx->2xx|$.Command.CloudWatchOutputConfig
Issue-Key: ssm|SendCommand|2xx->2xx|$.Command.Comment
Issue-Key: ssm|SendCommand|2xx->2xx|$.Command.ExpiresAfter
Issue-Key: ssm|SendCommand|2xx->2xx|$.Command.HasCancelCommandSignature
Issue-Key: ssm|SendCommand|2xx->2xx|$.Command.HasSendCommandSignature
Issue-Key: ssm|SendCommand|2xx->2xx|$.Command.Interactive
Issue-Key: ssm|SendCommand|2xx->2xx|$.Command.NotificationConfig
Issue-Key: ssm|SendCommand|2xx->2xx|$.Command.OutputS3BucketName
Issue-Key: ssm|SendCommand|2xx->2xx|$.Command.OutputS3KeyPrefix
Issue-Key: ssm|SendCommand|2xx->2xx|$.Command.OutputS3Region
Issue-Key: ssm|SendCommand|2xx->2xx|$.Command.RequestedDateTime
Issue-Key: ssm|SendCommand|2xx->2xx|$.Command.ServiceRole
Issue-Key: ssm|SendCommand|2xx->2xx|$.Command.Targets
Issue-Key: ssm|SendCommand|2xx->2xx|$.Command.TriggeredAlarms
Issue-Key: ssm|GetCommandInvocation|2xx->2xx|$.CloudWatchOutputConfig
Issue-Key: ssm|GetCommandInvocation|2xx->2xx|$.Comment
Issue-Key: ssm|GetCommandInvocation|2xx->2xx|$.PluginName
Issue-Key: ssm|GetCommandInvocation|2xx->2xx|$.StandardOutputContent
Issue-Commit: a49cdab94fc41177458b6e53730040285f53d8d6

# ssm Run Command responses diverge: SendCommand and GetCommandInvocation drop modeled members, PluginName is wrong, and a pushed Instance Connect key is invisible in the guest

> _Note: this issue was identified against the code base at commit `a49cdab94fc41177458b6e53730040285f53d8d6`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
SendCommand

## Description
3 net-new causes in the ssm Run Command surface, all exercised by the same recorded scenario (an EC2 Instance Connect public key is pushed to an instance and then read back from inside the guest with an AWS-RunShellScript command). Each cause must be closed before this issue is submitted.

Cause 1 of 3 — SendCommand's Command projection omits members AWS always serializes, and its timestamps are not in AWS's shape (fixture ec2-instance-connect-pushed-key-served-by-instance-metadata, step task#14, HTTP 200 vs 200).
AWS: Smithy (com.amazonaws.ssm, awsJson1_1) models SendCommand -> SendCommandResult.Command : Command, whose members include Comment, Targets, OutputS3Region, OutputS3BucketName, OutputS3KeyPrefix, ServiceRole, NotificationConfig (NotificationArn/NotificationEvents/NotificationType), CloudWatchOutputConfig (CloudWatchLogGroupName/CloudWatchOutputEnabled, @default false), AlarmConfiguration (Alarms/IgnorePollAlarmFailure, @default false), TriggeredAlarms, plus the timestamps ExpiresAfter and RequestedDateTime (target DateTime). The recording shows AWS populating every one of these in the accepted-command echo even when the request did not set them: the scalars as empty strings (Comment, ClientName, ClientSourceId, OutputS3BucketName, OutputS3KeyPrefix, ServiceRole), OutputS3Region as the request's region, the booleans as false (HasSendCommandSignature, HasCancelCommandSignature, Interactive), the lists as [], and the config structures as fully-populated objects with their defaulted members. ClientName, ClientSourceId, HasSendCommandSignature, HasCancelCommandSignature and Interactive are recorded behavior only — they are not in the Smithy Command shape, but AWS emits them on the wire and a recorded client sees them.
Emulator: every one of those members is absent from the Command object (JSON null / key not serialized): AlarmConfiguration, ClientName, ClientSourceId, CloudWatchOutputConfig, Comment, HasCancelCommandSignature, HasSendCommandSignature, Interactive, NotificationConfig, OutputS3BucketName, OutputS3KeyPrefix, OutputS3Region, ServiceRole, Targets, TriggeredAlarms. The two timestamp members are serialized but fail the recorded shape assertion: ExpiresAfter and RequestedDateTime come back as full-precision epoch-second doubles with seven fractional digits. Hypothesis: AWS's awsJson1_1 epoch-second timestamps carry at most millisecond precision, so a seven-decimal double is outside the recorded shape. The absolute instants themselves are per-run values and are not a requirement.
Why: a client that reads the accepted command's echo to confirm what it submitted — CloudWatch output settings, notification config, S3 output location, max concurrency targets, the alarm configuration — gets null for all of it and either crashes on a non-optional field or concludes the command was accepted with different settings than it asked for. SDKs that model Command.AlarmConfiguration.Alarms as required see a structure that cannot be decoded.
Fix: the ssm SendCommand handler and the Command serializer — the stored command record must carry the defaulted/empty values for all of the above and the serializer must emit them unconditionally (and emit DateTime members with AWS's epoch-second precision) rather than skipping None.
Traces: ec2-instance-connect-pushed-key-served-by-instance-metadata#task#14.

Cause 2 of 3 — GetCommandInvocation reports the wrong PluginName and omits Comment and CloudWatchOutputConfig (same fixture, step task#15, HTTP 200 vs 200).
AWS: Smithy models GetCommandInvocationResult.PluginName : CommandPluginName (@length min 4), Comment : Comment and CloudWatchOutputConfig : CloudWatchOutputConfig. The plugin name is the document's plugin/action name, which for AWS-RunShellScript is the document's step action identifier 'aws:runShellScript'; the recording shows exactly that. As in cause 1, Comment is present as the empty string and CloudWatchOutputConfig as the object with its defaulted members when the command set neither.
Emulator: PluginName is 'runShellScriptOnLinux' — an internal/platform-specific label, not the document's plugin name — and Comment and CloudWatchOutputConfig are absent (null).
Why: callers pass PluginName back into GetCommandInvocation (and key per-plugin output in S3/CloudWatch Logs by it); a name the service will not accept on input breaks that round trip, and tooling that selects output by plugin finds nothing. Omitted Comment/CloudWatchOutputConfig again break clients that expect the members to be present.
Fix: the ssm GetCommandInvocation handler/serializer — derive PluginName from the invoked document's plugin/action name and project Comment and CloudWatchOutputConfig from the stored command record instead of skipping them.
Traces: ec2-instance-connect-pushed-key-served-by-instance-metadata#task#15 (this step also carries cause 3).

Cause 3 of 3 — a public key pushed with EC2 Instance Connect is never visible to a command running in the guest, so the invocation's StandardOutputContent reports no matching key (same fixture, step task#15, HTTP 200 vs 200).
AWS: recorded behavior only. The scenario pushes an ed25519 public key to the instance (EC2 Instance Connect) and then runs an AWS-RunShellScript command that looks the pushed key up from inside the guest; AWS returns Status Success with StandardOutputContent containing the exact public key that was pushed, one 'ssh-ed25519 AAAA...' line terminated by a newline. The key material itself is a per-run value supplied by the request — the requirement is that whatever key the preceding push supplied is what the in-guest lookup prints.
Emulator: StandardOutputContent is the fixed sentinel 'no-matching-key-listed\n' — the pushed key is nowhere observable from the guest, so the command runs but finds nothing.
Why: the whole point of Instance Connect is that a pushed key becomes usable on the instance for its 60-second window. A client (or test) that pushes a key and then verifies it landed concludes the push silently did nothing, and any flow that gates an SSH connection on that verification can never proceed.
Fix: the emulator's Instance Connect push must write the key into the state the guest-side Run Command execution reads (the instance's authorized-key/metadata-served key state), and the AWS-RunShellScript execution path must read that state rather than returning a hard-coded sentinel.
Traces: ec2-instance-connect-pushed-key-served-by-instance-metadata#task#15.

Related open issues (none of which carries these replay keys, so none covers these causes): 20261114T200121Z-ec2-elbv2-ssm-getcommandinvocation-reports-status-success-with.md (GetCommandInvocation Success with empty StandardOutputContent in a different, guest-script scenario) and 20261115T000000Z-ec2-ssm-a-managed-nodes-guests-has-no-network-programs (guest program availability). Here the invocation does run and does return output — the output and the response projection are what diverge.

## Evidence
Replay of fixture ec2-instance-connect-pushed-key-served-by-instance-metadata (trace set simparity-wire-aws-v1), 2 mismatched steps of 28, score 92.9%.

Step task#14, ssm SendCommand, expected 200 / actual 200, body mismatch:
  $.Command.AlarmConfiguration: expected {'Alarms': [], 'IgnorePollAlarmFailure': False}, got None
  $.Command.ClientName: expected '', got None
  $.Command.ClientSourceId: expected '', got None
  $.Command.CloudWatchOutputConfig: expected {'CloudWatchLogGroupName': '', 'CloudWatchOutputEnabled': False}, got None
  $.Command.Comment: expected '', got None
  $.Command.HasCancelCommandSignature: expected False, got None
  $.Command.HasSendCommandSignature: expected False, got None
  $.Command.Interactive: expected False, got None
  $.Command.NotificationConfig: expected {'NotificationArn': '', 'NotificationEvents': [], 'NotificationType': ''}, got None
  $.Command.OutputS3BucketName: expected '', got None
  $.Command.OutputS3KeyPrefix: expected '', got None
  $.Command.OutputS3Region: expected the request's region (recording: 'us-west-2'), got None
  $.Command.ServiceRole: expected '', got None
  $.Command.Targets: expected [], got None
  $.Command.TriggeredAlarms: expected [], got None
  $.Command.ExpiresAfter: shape assertion failed, got 1794700684.9756951 (7-decimal epoch double)
  $.Command.RequestedDateTime: shape assertion failed, got 1794700624.9756951 (7-decimal epoch double)

Step task#15, ssm GetCommandInvocation, expected 200 / actual 200, body mismatch:
  $.PluginName: expected 'aws:runShellScript', got 'runShellScriptOnLinux'
  $.Comment: expected '', got None
  $.CloudWatchOutputConfig: expected {'CloudWatchLogGroupName': '', 'CloudWatchOutputEnabled': False}, got None
  $.StandardOutputContent: expected the public key pushed earlier in the scenario, as one 'ssh-ed25519 <base64>' line + '\n' (recording: 'ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIG8+5971kKuGYPUmkwgr2zRLeCjZpXnL3XeUccJRib+v\n'), got 'no-matching-key-listed\n'

Model evidence: Smithy com.amazonaws.ssm — SendCommand output Command : Command with members Comment, Targets, OutputS3Region, OutputS3BucketName, OutputS3KeyPrefix, ServiceRole, NotificationConfig, CloudWatchOutputConfig, AlarmConfiguration (Alarms required), TriggeredAlarms, ExpiresAfter/RequestedDateTime : DateTime (timestamp); CloudWatchOutputConfig.CloudWatchOutputEnabled @default false; AlarmConfiguration.IgnorePollAlarmFailure @default false; GetCommandInvocation output PluginName : CommandPluginName (@length min 4), Comment, CloudWatchOutputConfig, StandardOutputContent (@length 0..24000). Command has no ClientName/ClientSourceId/Interactive/Has*Signature members — those are recorded behavior only.

## Steps to Reproduce
1. Replay wire fixture ec2-instance-connect-pushed-key-served-by-instance-metadata from trace set simparity-wire-aws-v1 against the emulator.
2. Step task#14: call ssm SendCommand for AWS-RunShellScript against the scenario's instance without setting Comment, Targets, output-S3, notification, CloudWatch-output or alarm options. Inspect the 200 response's Command object: AlarmConfiguration, CloudWatchOutputConfig, NotificationConfig, Comment, ServiceRole, OutputS3Region/BucketName/KeyPrefix, Targets, TriggeredAlarms, ClientName, ClientSourceId, Interactive, HasSendCommandSignature and HasCancelCommandSignature are all null/absent, and ExpiresAfter/RequestedDateTime are 7-decimal epoch doubles.
3. Step task#15: call ssm GetCommandInvocation with that CommandId and InstanceId. PluginName is 'runShellScriptOnLinux' instead of the document's plugin name 'aws:runShellScript'; Comment and CloudWatchOutputConfig are null.
4. Same response: the scenario earlier pushed an ed25519 public key via EC2 Instance Connect, and the command reads it back inside the guest. StandardOutputContent is 'no-matching-key-listed\n' instead of the pushed 'ssh-ed25519 ...' line.
