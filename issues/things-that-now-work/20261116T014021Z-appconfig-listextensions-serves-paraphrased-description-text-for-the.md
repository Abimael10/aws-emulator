Issue-Source: trace-sweep-divergence
Issue-Trace: appconfig-catalog-list-extensions
Issue-Step: task#1
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: appconfig-catalog-list-extensions
Issue-Shape-Id: com.amazonaws.appconfig#AmazonAppConfig
Issue-Key: appconfig|ListExtensions|2xx->2xx|$.Items[Arn="arn:aws:appconfig:us-west-2::extension/AWS.AppConfig.DeploymentNotificationsToSns/1"].Description
Issue-Key: appconfig|ListExtensions|2xx->2xx|$.Items[Arn="arn:aws:appconfig:us-west-2::extension/AWS.AppConfig.DeploymentNotificationsToSqs/1"].Description
Issue-Commit: 78a28d66acabd31f329494a20428d6e048dfa41c

# appconfig ListExtensions serves paraphrased Description text for the AWS-authored SQS and SNS deployment-notification extensions

> _Note: this issue was identified against the code base at commit `78a28d66acabd31f329494a20428d6e048dfa41c`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ListExtensions

## Description
This issue carries 1 net-new cause in appconfig.

Related open issue that does NOT cover this cluster (the replay reports owned_by empty): 20261105T012540Z-appconfig-synthesized-response-content-diverges-deployment-eventlog-entries.md filed the same catalog fault for AWS.AppConfig.FeatureFlags/1, AWS.AppConfig.JiraIntegration/1, AWS.Evidently.ProjectImporter/1 and AWS.AppConfig.DeploymentNotificationsToEventBridge/1. Those entries now match; the two deployment-notification entries below still diverge and are not bound to that issue's replay pointer.

Cause 1 of 1: the predefined extension catalog's Description for the deployment-notification extensions is the emulator's own wording rather than AWS's fixed text.
  AWS: Smithy: ListExtensions GET /extensions, http code 200, output Extensions{Items -> ExtensionSummaries of ExtensionSummary{Id, Name, VersionNumber, Arn, Description}}; ExtensionSummary$Description -> Description (@length 0..1024), documented 'Information about the extension'. The model does not enumerate the predefined catalog, so for the catalog content this is recorded behavior only: AWS ships a fixed set of AWS-authored extensions (ARNs of the form arn:aws:appconfig:<region>::extension/<Name>/<VersionNumber>) and each carries one fixed, AWS-authored Description on every ListExtensions page — there is no part of these strings taken from the request or from account state, so they must be served verbatim per extension name and version. The recording shows AWS.AppConfig.DeploymentNotificationsToSqs/1 = 'Sends messages to the configured Amazon SQS queue when a deployment of configuration data in AppConfig is started, completed, or rolled back. Can be associated to the following resources in AppConfig: Application, Environment, Configuration Profile.' and AWS.AppConfig.DeploymentNotificationsToSns/1 = 'Sends events to the configured Amazon SNS topic when a deployment of configuration data in AppConfig is started, completed, or rolled back. Can be associated to the following resources in AppConfig: Application, Environment, Configuration Profile.' Note the distinct leading clauses AWS uses per target: 'Sends messages to the configured Amazon SQS queue ...' for the queue extension and 'Sends events to the configured Amazon SNS topic ...' for the topic extension.
  Emulator: answers 200 with the right ARNs but a generified first clause on both entries — 'Sends events to an Amazon SQS queue when a deployment ...' (AWS: 'Sends messages to the configured Amazon SQS queue ...') and 'Sends events to an Amazon SNS topic when a deployment ...' (AWS: 'Sends events to the configured Amazon SNS topic ...'). The remainder of each string matches.
  Why: Description is the text a console or a client-side extension picker shows a user when choosing an extension, and tooling that asserts on or diffs the AWS-authored catalog sees different prose for entries that are supposed to be identical in every account. A caller cannot distinguish 'this emulator paraphrases AWS' from 'this account's catalog differs', and the SQS entry in particular loses the fact that AWS words it as sending messages to the configured queue.
  Fix: the appconfig predefined/AWS-authored extension catalog seed data that ListExtensions (and GetExtension) reads, in the extension state/catalog component — the two deployment-notification entries must carry AWS's verbatim Description, which is wholly fixed text keyed by extension name and version.
  Traces: appconfig-catalog-list-extensions#task#1.

## Evidence
Replay of trace set simparity-wire-aws-v1: 47 fixtures, 570 steps, 551 passed, 19 mismatched, score 96.7%. Cluster reported with stub_path empty and owned_by empty, affected_count 1.

appconfig-catalog-list-extensions#task#1, ListExtensions, expected status 200 / actual 200 (mismatch):
  $.Items[Arn="arn:aws:appconfig:us-west-2::extension/AWS.AppConfig.DeploymentNotificationsToSqs/1"].Description: value_mismatch — expected 'Sends messages to the configured Amazon SQS queue when a deployment of configuration data in AppConfig is started, completed, or rolled back. Can be associated to the following resources in AppConfig: Application, Environment, Configuration Profile.', got 'Sends events to an Amazon SQS queue when a deployment of configuration data in AppConfig is started, completed, or rolled back. Can be associated to the following resources in AppConfig: Application, Environment, Configuration Profile.'
  $.Items[Arn="arn:aws:appconfig:us-west-2::extension/AWS.AppConfig.DeploymentNotificationsToSns/1"].Description: value_mismatch — expected 'Sends events to the configured Amazon SNS topic when a deployment of configuration data in AppConfig is started, completed, or rolled back. Can be associated to the following resources in AppConfig: Application, Environment, Configuration Profile.', got 'Sends events to an Amazon SNS topic when a deployment of configuration data in AppConfig is started, completed, or rolled back. Can be associated to the following resources in AppConfig: Application, Environment, Configuration Profile.'

The ARN's partition and region segments are environment-specific and are not part of the requirement; the per-extension Description text is entirely fixed text and is.

## Steps to Reproduce
1. Call ListExtensions with no filters against a fresh emulator and read $.Items.
2. Locate the entries whose Arn ends in extension/AWS.AppConfig.DeploymentNotificationsToSqs/1 and extension/AWS.AppConfig.DeploymentNotificationsToSns/1.
3. Compare each Description with AWS's fixed catalog text: AWS begins the SQS entry 'Sends messages to the configured Amazon SQS queue when a deployment ...' and the SNS entry 'Sends events to the configured Amazon SNS topic when a deployment ...'; the emulator begins both with 'Sends events to an Amazon SQS queue ...' / 'Sends events to an Amazon SNS topic ...'.
4. Replay fixture appconfig-catalog-list-extensions, step task#1.
