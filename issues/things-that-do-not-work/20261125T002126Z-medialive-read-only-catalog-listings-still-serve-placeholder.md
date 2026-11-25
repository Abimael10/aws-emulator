Issue-Source: trace-sweep-divergence
Issue-Trace: medialive-catalog-list-cloud-watch-alarm-templates
Issue-Step: task#1
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: medialive-catalog-list-cloud-watch-alarm-templates medialive-catalog-list-cloud-watch-alarm-template-groups medialive-catalog-list-offerings
Issue-Shape-Id: com.amazonaws.medialive#MediaLive
Issue-Key: medialive|ListCloudWatchAlarmTemplates|2xx->2xx|$.cloudWatchAlarmTemplates[arn="arn:aws:medialive:us-west-2::cloudwatch-alarm-template:aws-2000030"]
Issue-Key: medialive|ListCloudWatchAlarmTemplates|2xx->2xx|$.cloudWatchAlarmTemplates[arn="arn:aws:medialive:us-west-2::cloudwatch-alarm-template:aws-2000031"]
Issue-Key: medialive|ListCloudWatchAlarmTemplates|2xx->2xx|$.cloudWatchAlarmTemplates[arn="arn:aws:medialive:us-west-2::cloudwatch-alarm-template:aws-2000032"]
Issue-Key: medialive|ListCloudWatchAlarmTemplates|2xx->2xx|$.cloudWatchAlarmTemplates[arn="arn:aws:medialive:us-west-2::cloudwatch-alarm-template:aws-2000033"]
Issue-Key: medialive|ListCloudWatchAlarmTemplates|2xx->2xx|$.cloudWatchAlarmTemplates[arn="arn:aws:medialive:us-west-2::cloudwatch-alarm-template:aws-2000034"]
Issue-Key: medialive|ListCloudWatchAlarmTemplates|2xx->2xx|$.cloudWatchAlarmTemplates[arn="arn:aws:medialive:us-west-2::cloudwatch-alarm-template:aws-2000035"]
Issue-Key: medialive|ListCloudWatchAlarmTemplates|2xx->2xx|$.cloudWatchAlarmTemplates[arn="arn:aws:medialive:us-west-2::cloudwatch-alarm-template:aws-2000036"]
Issue-Key: medialive|ListCloudWatchAlarmTemplates|2xx->2xx|$.cloudWatchAlarmTemplates[arn="arn:aws:medialive:us-west-2::cloudwatch-alarm-template:aws-2000037"]
Issue-Key: medialive|ListCloudWatchAlarmTemplateGroups|2xx->2xx|$.cloudWatchAlarmTemplateGroups[arn="arn:aws:medialive:us-west-2::cloudwatch-alarm-template-group:aws-1160891"].createdAt
Issue-Key: medialive|ListCloudWatchAlarmTemplateGroups|2xx->2xx|$.cloudWatchAlarmTemplateGroups[arn="arn:aws:medialive:us-west-2::cloudwatch-alarm-template-group:aws-1484347"].createdAt
Issue-Key: medialive|ListCloudWatchAlarmTemplateGroups|2xx->2xx|$.cloudWatchAlarmTemplateGroups[arn="arn:aws:medialive:us-west-2::cloudwatch-alarm-template-group:aws-2520023"].createdAt
Issue-Key: medialive|ListCloudWatchAlarmTemplateGroups|2xx->2xx|$.cloudWatchAlarmTemplateGroups[arn="arn:aws:medialive:us-west-2::cloudwatch-alarm-template-group:aws-2587288"].createdAt
Issue-Key: medialive|ListCloudWatchAlarmTemplateGroups|2xx->2xx|$.cloudWatchAlarmTemplateGroups[arn="arn:aws:medialive:us-west-2::cloudwatch-alarm-template-group:aws-3414886"].createdAt
Issue-Key: medialive|ListCloudWatchAlarmTemplateGroups|2xx->2xx|$.cloudWatchAlarmTemplateGroups[arn="arn:aws:medialive:us-west-2::cloudwatch-alarm-template-group:aws-3840949"].createdAt
Issue-Key: medialive|ListCloudWatchAlarmTemplateGroups|2xx->2xx|$.cloudWatchAlarmTemplateGroups[arn="arn:aws:medialive:us-west-2::cloudwatch-alarm-template-group:aws-3840949"].templateCount
Issue-Key: medialive|ListCloudWatchAlarmTemplateGroups|2xx->2xx|$.cloudWatchAlarmTemplateGroups[arn="arn:aws:medialive:us-west-2::cloudwatch-alarm-template-group:aws-4237617"].createdAt
Issue-Key: medialive|ListCloudWatchAlarmTemplateGroups|2xx->2xx|$.cloudWatchAlarmTemplateGroups[arn="arn:aws:medialive:us-west-2::cloudwatch-alarm-template-group:aws-4237617"].templateCount
Issue-Key: medialive|ListCloudWatchAlarmTemplateGroups|2xx->2xx|$.cloudWatchAlarmTemplateGroups[arn="arn:aws:medialive:us-west-2::cloudwatch-alarm-template-group:aws-4336130"].createdAt
Issue-Key: medialive|ListCloudWatchAlarmTemplateGroups|2xx->2xx|$.cloudWatchAlarmTemplateGroups[arn="arn:aws:medialive:us-west-2::cloudwatch-alarm-template-group:aws-4336130"].templateCount
Issue-Key: medialive|ListCloudWatchAlarmTemplateGroups|2xx->2xx|$.cloudWatchAlarmTemplateGroups[arn="arn:aws:medialive:us-west-2::cloudwatch-alarm-template-group:aws-5128237"].createdAt
Issue-Key: medialive|ListCloudWatchAlarmTemplateGroups|2xx->2xx|$.cloudWatchAlarmTemplateGroups[arn="arn:aws:medialive:us-west-2::cloudwatch-alarm-template-group:aws-5128237"].templateCount
Issue-Key: medialive|ListCloudWatchAlarmTemplateGroups|2xx->2xx|$.cloudWatchAlarmTemplateGroups[arn="arn:aws:medialive:us-west-2::cloudwatch-alarm-template-group:aws-5775786"].createdAt
Issue-Key: medialive|ListCloudWatchAlarmTemplateGroups|2xx->2xx|$.cloudWatchAlarmTemplateGroups[arn="arn:aws:medialive:us-west-2::cloudwatch-alarm-template-group:aws-5775786"].templateCount
Issue-Related: 20261117T224057Z-medialive-aws-managed-catalogs-are-seeded-with-synthetic.md
Issue-Commit: 75f577a9b4ae922afffed072560bc3cac4bdc4f1

# medialive read-only catalog listings still serve placeholder data: alarm templates absent, group counts and timestamps synthetic

> _Note: this issue was identified against the code base at commit `75f577a9b4ae922afffed072560bc3cac4bdc4f1`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ListCloudWatchAlarmTemplates

## Description
Three divergence clusters, one shared fault: the AWS-managed, read-only catalogs MediaLive serves (CloudWatch alarm templates, alarm template groups, reserved offerings) are populated with generated placeholder rows and constant timestamps instead of the catalog AWS returns. 3 causes must be closed.

Related open issue: 20261117T224057Z-medialive-aws-managed-catalogs-are-seeded-with-synthetic describes the same family, but its recorded replay keys name the earlier synthetic ARNs (aws-1000001...) and the replay reports these three clusters as unowned, so they are filed here. The alarm template GROUP identities are now correct (real aws-1160891, aws-1484347, ... ids are present); what remains diverging is listed below.

Cause 1 of 3: ListCloudWatchAlarmTemplates returns none of the AWS-managed templates
AWS: Smithy: ListCloudWatchAlarmTemplates returns a list of CloudWatchAlarmTemplateSummary, whose required members are arn, id, groupId, name, targetResourceType, comparisonOperator, evaluationPeriods, metricName, period, statistic, treatMissingData, createdAt and threshold; CloudWatchAlarmTemplateGroupSummary.Id (__stringMin7Max11PatternAws097) documents that 'AWS provided template groups have ids that start with aws-'. The recording returns the region's AWS-managed template catalog, each template bound to a real group id, with its own createdAt and its documented description text (e.g. a ChannelInputErrorSeconds template in group aws-1484347 and an InputLossSeconds template in group aws-5775786).
Emulator: answers 200, but every AWS-managed template in the recording is absent from the response ('missing ... got None' for each), and the list instead contains sequentially generated entries (aws-2000001 ... aws-2000037).
Why: a client that discovers an AWS-managed alarm template by id - to create an alarm from it, or to bind it to a group - finds nothing, and anything it does find is an id that does not exist in AWS.
Fix: the seeded alarm-template catalog in services/service-com-amazonaws-medialive-medialive/src/state.rs together with the ListCloudWatchAlarmTemplates handler in src/ops - seed the real AWS-managed catalog (ids, group ids, metric/alarm fields, createdAt, descriptions) rather than generating rows.
Traces: medialive-catalog-list-cloud-watch-alarm-templates#task#1.

Cause 2 of 3: ListCloudWatchAlarmTemplateGroups reports a constant createdAt and an undercounted templateCount
AWS: Smithy: CloudWatchAlarmTemplateGroupSummary.CreatedAt is a REQUIRED __timestampIso8601 (date-time) and TemplateCount a REQUIRED __integer documented as 'The number of templates in a group.' So each group carries its own creation timestamp, and templateCount equals the number of templates that group actually holds.
Emulator: answers 200 with every group's createdAt set to one shared literal (the same value for all groups, where the recording gives each group a distinct timestamp), and templateCount lower than the number of templates in the group (recorded 5 vs 4, 8 vs 2, 5 vs 3 across groups).
Why: templateCount is the number a client uses to page or to decide a group is non-empty, so an undercount hides templates; one shared createdAt makes every group look created at the same instant and breaks any ordering or age-based selection.
Fix: the seeded group catalog in src/state.rs plus the ListCloudWatchAlarmTemplateGroups projection in src/ops - carry a per-group creation timestamp and compute templateCount from the templates actually present in the group rather than from a hardcoded number.
Traces: medialive-catalog-list-cloud-watch-alarm-template-groups#task#1.

Cause 3 of 3: ListOfferings returns generated offerings instead of the region's reserved-offering catalog
AWS: Smithy: ListOfferings returns Offering entries with arn, offeringId, offeringDescription, resourceSpecification {resourceType, codec, resolution, maximumBitrate, channelClass}, region, offeringType, duration, durationUnits, usagePrice, fixedPrice and currencyCode. The recording returns the real reserved-offering catalog for the region, each offering carrying its own offeringId, human-readable description, resource specification and price.
Emulator: answers 200, but none of the recorded offerings are present ('missing ... got None' for each); the list instead holds offerings with sequential synthetic ids (10000000, 10000001, ...).
Why: PurchaseOffering takes an offeringId obtained from this list, so a client that lists and then purchases uses an id AWS does not have, and any price/term logic reads fabricated values.
Fix: the seeded offering catalog in src/state.rs plus the ListOfferings handler in src/ops - seed the real offering catalog (ids, descriptions, resourceSpecification, pricing, duration) instead of generating sequential rows.
Traces: medialive-catalog-list-offerings#task#1.

## Evidence
Replay of simparity-wire-aws-v1 against the emulator built from this checkout, 3 mismatch clusters, all expected 200 / actual 200:
- ListCloudWatchAlarmTemplates (medialive-catalog-list-cloud-watch-alarm-templates#task#1): '$.cloudWatchAlarmTemplates[arn="arn:aws:medialive:us-west-2::cloudwatch-alarm-template:aws-0253256"]: missing (expected {...id aws-0253256, groupId aws-1484347, metricName ChannelInputErrorSeconds, period 60, statistic Maximum, treatMissingData missing...}, got None)' and the same for aws-0563153 and every other AWS-managed template; the emulator's own entries are aws-2000001..aws-2000037.
- ListCloudWatchAlarmTemplateGroups (medialive-catalog-list-cloud-watch-alarm-template-groups#task#1): 'createdAt: value_mismatch (expected 2024-04-09T16:15:25.342Z, got 2024-01-01T00:00:00Z)' for aws-1160891 and every other group (all actuals identical), plus 'templateCount: value_mismatch (expected 5, got 4)' for aws-3840949, '(expected 8, got 2)' for aws-4237617, '(expected 5, got 3)' for aws-4336130.
- ListOfferings (medialive-catalog-list-offerings#task#1): '$.offerings[arn="arn:aws:medialive:us-west-2::offering:304736645"]: missing (expected {offeringId 304736645, resourceSpecification {INPUT/AVC/UHD/MAX_10_MBPS/SINGLE_PIPELINE}, NO_UPFRONT, 12 MONTHS, usagePrice/fixedPrice/currencyCode}, got None)' and the same for every catalog offering; the emulator's entries are offering:10000000 upward.
The recorded ids, timestamps and prices above are the recordings' values, cited as evidence of the shape and provenance of the catalog, not as literals to hardcode: the requirement is that these read-only catalogs be the AWS catalog and that templateCount be derived from the templates present.

## Steps to Reproduce
1. Replay fixture medialive-catalog-list-cloud-watch-alarm-templates step task#1 (ListCloudWatchAlarmTemplates) and compare the returned template ids/groupIds against the recording: no aws-managed template from the recording is present.
2. Replay fixture medialive-catalog-list-cloud-watch-alarm-template-groups step task#1 (ListCloudWatchAlarmTemplateGroups) and compare createdAt (identical for all groups) and templateCount (lower than recorded) per group.
3. Replay fixture medialive-catalog-list-offerings step task#1 (ListOfferings) and compare offeringIds: the emulator returns sequential synthetic ids and none of the catalog offerings.
