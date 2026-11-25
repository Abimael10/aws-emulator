Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: resourcegroupstaggingapi__sns.get-resources-rejects-resource-arn-list-combined-with-resource-type-filters resourcegroupstaggingapi__sns.get-resources-tag-filter-matches-an-sns-topic-by-value-not-a-different-one
Issue-Key: resourcegroupstaggingapi__sns.get-resources-rejects-resource-arn-list-combined-with-resource-type-filters|run#9|AssertionError|assert message == "ResourceTypeFilters is not allowed when providing a ResourceARNList.", (
Issue-Key: resourcegroupstaggingapi__sns.get-resources-tag-filter-matches-an-sns-topic-by-value-not-a-different-one|_get_resources#14|Timeout|time.sleep(poll_s)
Issue-Commit: 6df92bd208b59385c9370bd54f69012d5d2b8998

# resourcegroupstaggingapi__sns: 2 net-new causes — GetResources' ResourceARNList+ResourceTypeFilters refusal reuses the generic TagFilters sentence, and TagFilters never matches an SNS topic tagged at CreateTopic

> _Note: this issue was identified against the code base at commit `6df92bd208b59385c9370bd54f69012d5d2b8998`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetResources

## Description
TWO net-new root causes, grounded in both failing checks of the suite's `resourcegroupstaggingapi__sns` heading; the replay report marks neither ALREADY FILED nor OWNED.

Cause 1 of 2: the message GetResources serves when ResourceARNList is combined with ResourceTypeFilters.
Failing check id: resourcegroupstaggingapi__sns.get-resources-rejects-resource-arn-list-combined-with-resource-type-filters
AWS: Smithy (services/service-com-amazonaws-resourcegroupstaggingapi-resourcegroupstaggingapi-20170126/smithy-model/com-amazonaws-resourcegroupstaggingapi-resourcegroupstaggingapi-20170126.json) documents GetResourcesInput.ResourceARNList as mutually exclusive with ResourceTypeFilters, with TagFilters and with the pagination parameters, each producing an Invalid Parameter exception; GetResources models InvalidParameterException (client, HTTP 400). AWS does not serve one generic sentence for all of those combinations: for ResourceARNList + ResourceTypeFilters the message is `ResourceTypeFilters is not allowed when providing a ResourceARNList.` — 100% FIXED text, including the trailing period and the two PascalCase member names; nothing is templated from the request. (The TagFilters combination has its own sentence, "Filters is not allowed when providing a resource list.") Recorded behavior for the exact sentences; the status/code are the modelled ones.
Emulator: answers the correct HTTP 400 / InvalidParameterException but serves the TagFilters sentence for the ResourceTypeFilters combination: `Filters is not allowed when providing a resource list.`
Why: the message is the only part of the refusal that tells a caller WHICH prohibited combination it sent. Collapsing both combinations onto one sentence breaks text matching against AWS and misdirects a caller that combined ResourceTypeFilters (it is told about "Filters" and "a resource list" generically), so message-based branching and golden-output tests that pass against AWS fail here.
Fix: GetResources' mutual-exclusion validation in the resourcegroupstaggingapi handler — choose the per-parameter sentence (ResourceTypeFilters vs TagFilters vs pagination) instead of emitting one sentence for every combination. NOTE for the fixer: open issue 20261111T065905Z-resourcegroupstaggingapi-getresources-resourcearnlist-tagfilters-refusal-message-is-emulator-authored.md (a wire-corpus issue, named for context only; it does not cover this behavioral check) pins the TagFilters sentence "Filters is not allowed when providing a resource list." — that sentence must be KEPT for the TagFilters combination while the ResourceTypeFilters combination gets its own.

Cause 2 of 2: GetResources with TagFilters never returns an SNS topic that was tagged through SNS's own tag plane.
Failing check id: resourcegroupstaggingapi__sns.get-resources-tag-filter-matches-an-sns-topic-by-value-not-a-different-one
AWS: tags written with sns:CreateTopic(Tags=[...]) (or sns:TagResource) are visible to the tag inventory, so GetResources(TagFilters=[{Key: <key>, Values: [<value>]}]) returns a ResourceTagMappingList containing the topic's ARN; the same query with a value the topic never carried does NOT return it, and after sns:UntagResource removes the key the topic stops matching. Recorded behavior only (the model documents TagFilters semantics, not which services index tags).
Emulator: the matching query returns nothing. The check pages GetResources(TagFilters=[{Key, Values:[value]}]) in a loop until any ARN appears and was killed at the suite's 60 s cap, i.e. the ResourceTagMappingList stayed EMPTY for the whole budget for a topic created moments earlier with that exact tag.
Why: GetResources is the cross-service tag inventory clients use for cost allocation, drift detection and bulk tagging. A resource that is tagged but never indexed is invisible to every inventory sweep, so an SNS topic can neither be discovered nor re-tagged through the tagging API, and a tag-governance check reports a false "nothing tagged" instead of a finding.
Fix: the tag-inventory index GetResources reads in the resourcegroupstaggingapi service crate (and/or the SNS tag writer that must publish into it) — SNS topic tags written via CreateTopic/TagResource must be discoverable by TagFilters, and must stop matching once UntagResource removes the key. Related OPEN issues of the SAME class for other services, named for context only and not covering this check: 20261107T173512Z-dynamodb-resourcegroupstaggingapi-getresources-returns-no-match-for-a.md, 20261114T013533Z-resourcegroupstaggingapi-getresources-never-returns-tagged-ec2-resources-1.md, 20261117T140330Z-resourcegroupstaggingapi-ssm-getresources-never-returns-ssm-parameters-or.md.

Traces: the two check ids above; one per cause.

## Evidence
Cause 1 — resourcegroupstaggingapi__sns.get-resources-rejects-resource-arn-list-combined-with-resource-type-filters
  AssertionError: unexpected message for the combined-parameters refusal: 'Filters is not allowed when providing a resource list.'
  Expected (AWS): Code InvalidParameterException (which matched) and Message exactly "ResourceTypeFilters is not allowed when providing a ResourceARNList." — entirely fixed text.
  Actual (emulator): Message "Filters is not allowed when providing a resource list." (the sentence AWS uses for the ResourceARNList+TagFilters combination).
  The check's preceding leg — GetResources(ResourceARNList=[<topic arn>]) alone returning the topic — passed, and it passes again after the refusal.
  Model evidence: GetResourcesInput.ResourceARNList documentation: "You can't specify both this parameter and the ResourceTypeFilters parameter in the same request. If you do, you get an Invalid Parameter exception. You can't specify both this parameter and the TagFilters parameter ... You can't specify both this parameter and any of the pagination parameters ..."; GetResources models InvalidParameterException (client, HTTP 400).

Cause 2 — resourcegroupstaggingapi__sns.get-resources-tag-filter-matches-an-sns-topic-by-value-not-a-different-one
  Error: Check timed out after 60.0s — killed inside the poll `while True: arns = {m["ResourceARN"] for page in paginate(TagFilters=[{Key: <key>, Values: [<value>]}]) ...}; if arns: return` for a topic created with Tags=[{Key: <key>, Value: <value>}] immediately before.
  Expected (AWS): the first query returns a ResourceTagMappingList containing <topic arn>; a query for a different value does not return it; after sns:UntagResource(TagKeys=[<key>]) the topic no longer matches.
  Actual (emulator): the matching query returns an empty ResourceTagMappingList for the entire budget, so the topic is never discoverable by its tag.

## Steps to Reproduce
Cause 1 — resourcegroupstaggingapi__sns.get-resources-rejects-resource-arn-list-combined-with-resource-type-filters
1. sns:CreateTopic(Name=<name>, Tags=[{Key:"sim2real", Value:"true"}]) -> <topic arn>.
2. resourcegroupstaggingapi:GetResources(ResourceARNList=[<topic arn>]) — returns the topic.
3. GetResources(ResourceARNList=[<topic arn>], ResourceTypeFilters=["sns:topic"]). EXPECT HTTP 400 InvalidParameterException with Message "ResourceTypeFilters is not allowed when providing a ResourceARNList."; OBSERVE the same status/code with Message "Filters is not allowed when providing a resource list."

Cause 2 — resourcegroupstaggingapi__sns.get-resources-tag-filter-matches-an-sns-topic-by-value-not-a-different-one
1. sns:CreateTopic(Name=<name>, Tags=[{Key:<unique key>, Value:<unique value>}]) -> <topic arn>.
2. resourcegroupstaggingapi:GetResources(TagFilters=[{Key:<unique key>, Values:[<unique value>]}]), paging all results. EXPECT ResourceTagMappingList to contain <topic arn>; OBSERVE an empty list (polled to the 60 s cap).
3. GetResources(TagFilters=[{Key:<unique key>, Values:[<a value never applied>]}]) must NOT return the topic.
4. sns:UntagResource(ResourceArn=<topic arn>, TagKeys=[<unique key>]); the query from step 2 must stop returning the topic.
Replay: resourcegroupstaggingapi__sns.get-resources-rejects-resource-arn-list-combined-with-resource-type-filters, resourcegroupstaggingapi__sns.get-resources-tag-filter-matches-an-sns-topic-by-value-not-a-different-one
