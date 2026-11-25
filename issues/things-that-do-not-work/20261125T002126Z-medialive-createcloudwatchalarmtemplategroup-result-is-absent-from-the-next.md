Issue-Source: trace-sweep-divergence
Issue-Trace: medialive-cloudwatch-alarm-template-group-lifecycle
Issue-Step: task#3
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: medialive-cloudwatch-alarm-template-group-lifecycle
Issue-Shape-Id: com.amazonaws.medialive#MediaLive
Issue-Key: medialive|ListCloudWatchAlarmTemplateGroups|2xx->2xx|$.cloudWatchAlarmTemplateGroups[name="AWS-MediaConnectArqProtocolWorkflows"].templateCount
Issue-Key: medialive|ListCloudWatchAlarmTemplateGroups|2xx->2xx|$.cloudWatchAlarmTemplateGroups[name="AWS-MediaConnectInputFailoverWorkflows"].templateCount
Issue-Key: medialive|ListCloudWatchAlarmTemplateGroups|2xx->2xx|$.cloudWatchAlarmTemplateGroups[name="AWS-MediaLiveMultiplexWorkflows"].templateCount
Issue-Key: medialive|ListCloudWatchAlarmTemplateGroups|2xx->2xx|$.cloudWatchAlarmTemplateGroups[name="AWS-MediaLiveToMediaPackageWorkflows"].templateCount
Issue-Key: medialive|ListCloudWatchAlarmTemplateGroups|2xx->2xx|$.cloudWatchAlarmTemplateGroups[name="AWS-MediaTailorAdInsertionWorkflows"].templateCount
Issue-Key: medialive|ListCloudWatchAlarmTemplateGroups|2xx->2xx|$.cloudWatchAlarmTemplateGroups[name="sim2real-probe-medialive-cwgroup-7f786eeb"]
Issue-Commit: 75f577a9b4ae922afffed072560bc3cac4bdc4f1

# medialive CreateCloudWatchAlarmTemplateGroup result is absent from the next ListCloudWatchAlarmTemplateGroups

> _Note: this issue was identified against the code base at commit `75f577a9b4ae922afffed072560bc3cac4bdc4f1`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ListCloudWatchAlarmTemplateGroups

## Description
1 cause.

Cause 1 of 1: a customer-created CloudWatch alarm template group is not listed
AWS: Smithy: CreateCloudWatchAlarmTemplateGroup creates a group and ListCloudWatchAlarmTemplateGroups returns CloudWatchAlarmTemplateGroupSummary entries whose required members are arn, id, name, createdAt and templateCount (plus optional description, modifiedAt, tags); the model documents that only AWS-provided groups have ids starting with 'aws-', so customer-created groups are ordinary members of the same listing. Recorded behavior: in the lifecycle recording the group created earlier in the fixture appears in the very next ListCloudWatchAlarmTemplateGroups response, with the name and description it was created with, templateCount 0, empty tags, and generated arn/id/createdAt/modifiedAt (which the corpus masks as '<shaped-not-asserted>' because they vary per run).
Emulator: answers 200 with only the seeded AWS-managed groups; the just-created group is entirely absent from $.cloudWatchAlarmTemplateGroups ('missing ... got None'). So the create path writes a group the list path does not read - the listing serves the seeded catalog only.
Why: every client workflow over this resource is list-driven: after creating a group a client lists to find its id, to attach alarm templates to it, or to assert idempotently that the group exists. Against this emulator the group it just created looks as though it was never created, so the workflow either re-creates it forever or fails to find a group whose create call returned success.
Fix: the CreateCloudWatchAlarmTemplateGroup handler and the ListCloudWatchAlarmTemplateGroups handler in services/service-com-amazonaws-medialive-medialive/src/ops together with the alarm-template-group store in src/state.rs - creates must be inserted into the same collection the listing reads, and the listing must return the union of customer-created groups and the seeded AWS-managed catalog.
Note: the same response also reports templateCount undercounts on the AWS-managed groups (recorded 5 vs 4, 8 vs 2, 5 vs 3, 6 vs 2, 11 vs 5). That symptom is cause 2 of the catalog issue 'medialive read-only catalog listings still serve placeholder data'; it is not a second cause here, and fixing this listing's state plumbing alone will not close it.
Traces: medialive-cloudwatch-alarm-template-group-lifecycle#task#3.

## Evidence
Replay of simparity-wire-aws-v1 against the emulator built from this checkout, ListCloudWatchAlarmTemplateGroups, expected 200 / actual 200, fixture medialive-cloudwatch-alarm-template-group-lifecycle step task#3:
'$.cloudWatchAlarmTemplateGroups[name="sim2real-probe-medialive-cwgroup-7f786eeb"]: missing (expected {arn: <shaped-not-asserted>, id: <shaped-not-asserted>, name: sim2real-probe-medialive-cwgroup-7f786eeb, templateCount: 0, createdAt: <shaped-not-asserted>, modifiedAt: <shaped-not-asserted>, description: \'initial description\', tags: {}}, got None)'
The group name above is the fixture's per-run probe name, not a requirement; the requirement is that whatever group the preceding create returned appears in the next listing. The same cluster additionally reports, on the seeded groups: 'templateCount: value_mismatch (expected 5, got 4)' (AWS-MediaTailorAdInsertionWorkflows), '(expected 8, got 2)' (AWS-MediaLiveMultiplexWorkflows), '(expected 5, got 3)' (AWS-MediaConnectArqProtocolWorkflows), '(expected 6, got 2)' (AWS-MediaConnectInputFailoverWorkflows), '(expected 11, got 5)' (AWS-MediaLiveToMediaPackageWorkflows).

## Steps to Reproduce
1. Replay fixture medialive-cloudwatch-alarm-template-group-lifecycle: CreateCloudWatchAlarmTemplateGroup with a name and description (earlier step of the fixture) and note the 2xx response.
2. Call ListCloudWatchAlarmTemplateGroups (step task#3).
3. Observe the response contains only seeded AWS-managed groups and not the group just created.
