Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: resourcegroupstaggingapi__ssm.parameter-native-tags-visible-through-get-resources resourcegroupstaggingapi__ssm.document-tag-removal-excludes-only-the-untagged-sibling
Issue-Key: resourcegroupstaggingapi__ssm.parameter-native-tags-visible-through-get-resources|_poll_until#12|Timeout|time.sleep(interval_s)
Issue-Key: resourcegroupstaggingapi__ssm.document-tag-removal-excludes-only-the-untagged-sibling|_poll_until#14|Timeout|time.sleep(interval_s)
Issue-Commit: 845c80d629e2b6091bbba6d5ecc9b7e37b15e9ab

# resourcegroupstaggingapi__ssm: GetResources never returns SSM parameters or documents tagged through SSM's own tag plane

> _Note: this issue was identified against the code base at commit `845c80d629e2b6091bbba6d5ecc9b7e37b15e9ab`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetResources

## Description
Cause 1 of 1: the Resource Groups Tagging index does not include SSM resources, so a natively tagged parameter or document is never discoverable through GetResources.
AWS: recorded behavior only (the tagging index is cross-service and not described by either service's Smithy model). A tag written through SSM's own tag plane — PutParameter's Tags list, CreateDocument's Tags list, AddTagsToResource — is visible to resourcegroupstaggingapi GetResources: with ResourceTypeFilters=["ssm:parameter"] and TagFilters=[{Key,Values}] AWS answers HTTP 200 with one ResourceTagMappingList entry whose ResourceARN is the parameter's own ARN (arn:aws:ssm:<region>:<account>:parameter/<name>) and whose Tags carry that pair; with ResourceTypeFilters=["ssm:document"] and TagFilters=[{Key}] it returns every document carrying that key (arn:aws:ssm:<region>:<account>:document/<name>). SSM's RemoveTagsFromResource then drops exactly that resource from the index while a sibling carrying the same key stays.
Emulator: the predicate is never satisfied — both checks poll GetResources with those filters until the suite's 60 s budget is exhausted and are killed ("Check timed out after 60.0s"), so the tagged parameter and the two tagged documents never appear in the index at any point. The SSM-side reads in the same checks (ListTagsForResource) are not what fails.
Why: tag-driven discovery and governance — inventory sweeps, cost allocation, "find everything tagged owner=X" — silently returns nothing for SSM resources, and a removal test cannot be distinguished from an index that never populated. A client sees an empty, successful answer rather than an error, so the gap is invisible until it runs against AWS.
Fix: the tag-index integration in the resourcegroupstaggingapi service crate together with SSM's tag writers — tags written by PutParameter/CreateDocument/AddTagsToResource must register the resource's ARN and tag set under its ssm:parameter / ssm:document resource type, and RemoveTagsFromResource must de-register it.
Note: the resolved issue 20261102T083644Z-resourcegroupstaggingapi-getresources-never-returns-resources-tagged-by-their-native.md described this class generally and open issues 20261107T173512Z (dynamodb) and 20261114T013533Z (ec2) cover other services' resources; neither covers ssm:parameter / ssm:document, which are still absent from the index.
Traces: resourcegroupstaggingapi__ssm.parameter-native-tags-visible-through-get-resources, resourcegroupstaggingapi__ssm.document-tag-removal-excludes-only-the-untagged-sibling.

## Evidence
Behavioral suite, service heading `### resourcegroupstaggingapi__ssm`. Both checks end:

    Error: Check timed out after 60.0s

resourcegroupstaggingapi__ssm.parameter-native-tags-visible-through-get-resources polls, after a successful PutParameter(Name=<name>, Type="String", Tags=[{Key,Value}]) whose tag ListTagsForResource(ResourceType="Parameter") does report:

    rgt.get_resources(ResourceTypeFilters=["ssm:parameter"], TagFilters=[{"Key": key, "Values": [value]}])["ResourceTagMappingList"]

until it has exactly one entry; the expected entry's ResourceARN is arn:aws:ssm:<region>:<account>:parameter/<name>. The predicate is never met, so the poll runs until the check budget ends.

resourcegroupstaggingapi__ssm.document-tag-removal-excludes-only-the-untagged-sibling polls, after CreateDocument of two documents each with Tags=[{Key: key, ...}]:

    rgt.get_resources(ResourceTypeFilters=["ssm:document"], TagFilters=[{"Key": key}])["ResourceTagMappingList"]

for the pair {arn:...:document/<a>, arn:...:document/<b>} as its positive control, before it ever reaches RemoveTagsFromResource. That control never becomes true either.

Generated parameter/document names, tag keys and values are per-run; the requirement is that a natively tagged SSM resource appears in GetResources under its own ARN and leaves the index when its tag is removed.

## Steps to Reproduce
1. Build and boot the emulator from this checkout.
2. ssm PutParameter Name=<name>, Value=<any>, Type="String", Tags=[{"Key": "k1", "Value": "v1"}]. Confirm ssm ListTagsForResource(ResourceType="Parameter", ResourceId=<name>) reports k1=v1.
3. resourcegroupstaggingapi GetResources with ResourceTypeFilters=["ssm:parameter"] and TagFilters=[{"Key": "k1", "Values": ["v1"]}]. Observe an empty ResourceTagMappingList; AWS returns one entry whose ResourceARN is arn:aws:ssm:<region>:<account>:parameter/<name> with Tags k1=v1.
4. ssm CreateDocument twice with Tags=[{"Key": "k2", ...}], then GetResources with ResourceTypeFilters=["ssm:document"] and TagFilters=[{"Key": "k2"}]. Observe an empty list; AWS returns both document ARNs. ssm RemoveTagsFromResource on one and observe that AWS leaves only the sibling.
5. Re-replay the 2 checks listed in trace_ids.
