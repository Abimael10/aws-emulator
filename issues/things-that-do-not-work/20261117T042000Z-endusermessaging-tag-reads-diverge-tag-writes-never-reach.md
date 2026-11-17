Issue-Source: trace-sweep-divergence
Issue-Trace: endusermessaging-brand-profile-tagging
Issue-Step: task#4
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: endusermessaging-brand-profile-tagging endusermessaging-notify-code-configuration-channels-and-tags
Issue-Shape-Id: com.amazonaws.endusermessaging#EndUserMessaging
Issue-Key: endusermessaging|ListTagsForResource|2xx->2xx|$.tags[key="owner"]
Issue-Key: endusermessaging|ListTagsForResource|2xx->2xx|$.tags[key="team"].value
Issue-Key: endusermessaging|ListTagsForResource|2xx->2xx|$.tags[*]
Issue-Key: endusermessaging|ListTagsForResource|2xx->2xx|$.tags[key="stage"]
Issue-Key: endusermessaging|ListTagsForResource|4xx->4xx|$.message
Issue-Key: endusermessaging|ListTagsForResource|4xx->2xx|$.message
Issue-Key: endusermessaging|ListTagsForResource|4xx->2xx|$.resourceId
Issue-Key: endusermessaging|ListTagsForResource|4xx->2xx|$.resourceType
Issue-Key: endusermessaging|ListTagsForResource|4xx->2xx|$headers.x-amzn-errortype
Issue-Key: endusermessaging|ListTagsForResource|4xx->2xx|status
Issue-Commit: d56f3e72fadd2bcc773c18998c38d59f68ab7320

# endusermessaging tag reads diverge: tag writes never reach tag state, cross-region ARN refusal re-worded, deleted resource still answers 200

> _Note: this issue was identified against the code base at commit `d56f3e72fadd2bcc773c18998c38d59f68ab7320`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ListTagsForResource

## Description
5 net-new causes in this service's tagging surface, all observed through ListTagsForResource (GET /v1/tags/{resourceArn}). The open stub issues 20261110T210354Z-endusermessaging-tagresource-stub.md and 20261115T011048Z-endusermessaging-deletenotifycodeconfiguration-stub.md, and the open issue 20261115T024310Z-endusermessaging-notify-code-configuration-projections-diverge-minted-id.md (which carries the GetNotifyCodeConfiguration side of cause 5), describe neighbouring faults; none of them carries the replay key of any step below and the replay report marks none of these clusters owned or stub-blocked, so they are filed here and named against those issues rather than described as unrelated. Causes 1-3 share one root cause in the tag WRITE path; causes 4 and 5 are the refusal wording and the not-found branch of the READ path.

Cause 1 of 5: a tag write never reaches the tag state a brand profile carries, so ListTagsForResource keeps reporting only the create-time tag set.
AWS: Smithy (com.amazonaws.endusermessaging#EndUserMessaging, restJson1): TagResource POST /v1/tags/{resourceArn} (resourceArn @required @httpLabel AmazonResourceName; tags @required TagList @length 1..50) -> 200 with an empty body, and ListTagsForResource GET /v1/tags/{resourceArn} -> 200 with tags (TagList, @length 0..50), "An array of key and value pair tags that are associated with the resource." Both are bound to the same resource ARN, so AWS's contract is that a successful TagResource is immediately visible to a subsequent ListTagsForResource: a key the resource did not carry is added, and a key it already carried takes the newly supplied value (tag keys are unique per resource).
Emulator: after the tagging sequence the fixture replays, ListTagsForResource answers 200 but reports neither the added key nor the updated value: the added tag is absent entirely ($.tags[key="owner"] missing) and the pre-existing key still reports the value it was created with instead of the value the tag write supplied ($.tags[key="team"].value: expected the updated value, got the create-time value). The read handler itself is implemented (services/service-com-amazonaws-endusermessaging-endusermessaging/src/ops/list_tags_for_resource.rs:149-183 reads BrandProfile.tags through state::tags_of), but the write path that would have changed those tags answers 501 and never mutates state, so the tag set is frozen at whatever CreateBrandProfile stored.
Why: tagging is how callers label, bill and find resources. A client that tags a brand profile and then reads its tags back - the normal write-then-read of any tagging client, of an IaC provider's refresh, or of a tag-based policy or cost-allocation check - sees its write silently lost: the emulator reports 200 for the read and a tag set that does not contain what was just written, so the client concludes the tag was never applied or, worse, acts on the stale value.
Fix: the tag write path of this service's state - TagResource's handler (src/ops/tag_resource.rs, currently returning crate::scaffolded::Scaffolded::not_implemented) must resolve the ARN through state::resolve_tagged_resource and upsert each supplied key into the resolved record's tags vector (BrandProfile.tags / NotifyCodeConfigurationRecord.tags), replacing the value of a key already present rather than appending a duplicate, so the already-implemented read in src/ops/list_tags_for_resource.rs reports it.
Traces: endusermessaging-brand-profile-tagging#task#4.

Cause 2 of 5: a tag removal never reaches the tag state either, so a key the caller untagged is still reported and a key it added is still missing.
AWS: Smithy: UntagResource DELETE /v1/tags/{resourceArn} (resourceArn @required @httpLabel; tagKeys @required TagKeyList @httpQuery "tagKeys" @length min 1), @idempotent, -> 200 with an empty body. After it succeeds, ListTagsForResource reports the resource's tags WITHOUT the removed keys; untagging a key the resource does not carry is a no-op success (the operation is @idempotent).
Emulator: ListTagsForResource answers 200 with the tag set the resource was created with: the key that was untagged is still present and the key that had been added is still absent ($.tags[0]: expected the surviving added tag, got the create-time tag). UntagResource answers 501 (src/ops/untag_resource.rs:161 returns Scaffolded::not_implemented) and makes no state change.
Why: a client that removes a tag and re-reads the resource still sees the removed tag, so cleanup, re-tagging and tag-driven access or cost grouping all operate on a tag set that does not exist on AWS; an @idempotent operation that never takes effect also breaks retry logic that assumes the second call is a no-op because the first succeeded.
Fix: UntagResource's handler must resolve the ARN the same way the read does and delete each named key from the resolved record's tags vector, leaving the remaining tags in their written order, and must succeed for a key the resource does not carry.
Traces: endusermessaging-brand-profile-tagging#task#6 (cluster also covers endusermessaging-brand-profile-tagging#task#10).

Cause 3 of 5: the same frozen tag state applies to a notify code configuration, not only to a brand profile.
AWS: Smithy: TagResource/UntagResource/ListTagsForResource address any taggable resource of this service by its ARN; a notify-code-configuration ARN (arn:<partition>:end-user-messaging:<region>:<account>:notify-code-configuration/<id>) is one of them, so a tag written against that ARN is reported by a subsequent ListTagsForResource on it.
Emulator: after a tag is written against the notify-code-configuration ARN, ListTagsForResource answers 200 with that key absent ($.tags[key="stage"] missing) while the configuration's create-time tags are reported. The read resolves the resource correctly (state::resolve_tagged_resource handles TaggedResource::NotifyCodeConfiguration), so what is missing is again the write.
Why: a client tagging a notify code configuration gets the same silently-lost write as for a brand profile, so the defect is not confined to one resource type and a fix limited to brand profiles would leave it.
Fix: the tag write path must be implemented against TaggedResource generally (both variants state::resolve_tagged_resource returns), not per resource type.
Traces: endusermessaging-notify-code-configuration-channels-and-tags#task#3.

Cause 4 of 5: the refusal for a resourceArn scoped to another Region is worded as this emulator's own sentence instead of the service's partition/region mismatch message.
AWS: Smithy: ListTagsForResource models ValidationException (HTTP 400, "A standard error for input validation failures"); resourceArn is @required @httpLabel AmazonResourceName (@length 20..256, pattern ^arn:[A-Za-z0-9_:/-]+$). The model does not state the wording, so: recorded behavior only - a well-formed ARN of this service whose partition/region fields do not match the credential scope the request was signed for is refused 400 with the message "ARN partition/region mismatch: expected partition '<expected-partition>' region '<expected-region>', got partition '<arn-partition>' region '<arn-region>'". Fixed text: "ARN partition/region mismatch: expected partition '", "' region '", "', got partition '", "' region '". Variable parts: the expected partition and Region, which come from the Region the request was made in, and the partition and Region fields parsed out of the request's own resourceArn. Status 400 and the ValidationException error code are both requirements.
Emulator: 400 ValidationException, but the message is "Invalid ARN: <resourceArn> does not name a resource in Region <request-region>, the Region this request was made in" - a different sentence that quotes the whole ARN instead of naming the expected and actual partition and Region (src/state/tags.rs:78-95, cross_region_resource_arn, reached from resolve_tagged_resource at src/state/tags.rs:128-133).
Why: the status and error code agree, so a client matching on the message - log assertions, user-facing error surfacing, or any SDK-level string match used to tell a cross-Region ARN apart from a malformed one - reads a message AWS never emits, and the two values it needs (which partition/Region was expected and which the ARN named) are not in it in parseable form.
Fix: cross_region_resource_arn in the service's tagging state module must word the refusal as the service's partition/region mismatch message, naming the expected and the ARN-supplied partition and Region, and the shared fieldList entry on /resourceArn must follow that wording.
Traces: endusermessaging-brand-profile-tagging#task#9.

Cause 5 of 5: ListTagsForResource answers 200 with tags for a notify code configuration that has been deleted, instead of ResourceNotFoundException.
AWS: Smithy: ListTagsForResource models ResourceNotFoundException (HTTP 404) - "The request references a resource that does not exist. Verify that the resource identifier is correct and try your request again" - with members message, resourceId and resourceType. An ARN of this service and Region naming a resource that no longer exists is refused 404 with message 'Resource not found - ResourceType="<resource-type>" ResourceId="<resource-id>"', resourceType the hyphenated resource segment of the ARN (here notify-code-configuration) and resourceId the identifier the ARN names; x-amzn-errortype carries the error shape. The fixed text is that template; the only variable parts are the resource type and the identifier taken from the request's ARN.
Emulator: after the configuration is deleted, ListTagsForResource on its ARN answers 200 and reports the deleted resource's tags - no message, no resourceId, no resourceType, no x-amzn-errortype. The record is still in state because the delete never removed it, so resolve_tagged_resource still resolves the ARN; the read's own not-found branch (src/ops/list_tags_for_resource.rs:159-168 -> UnreachableResource::NotFound) is therefore never reached.
Why: a client that deletes a resource and then reads its tags - cleanup verification, an IaC refresh after a destroy, or a reconciler deciding whether a resource is gone - is told the resource still exists and still carries tags, so it never converges and may re-tag or re-use an identifier AWS has released.
Fix: the delete path for notify code configurations must remove the record from state so every ARN-addressed reader, including the tagging reads, resolves it to ResourceNotFoundException; the tagging read's not-found branch and message template are already correct and must not be changed.
Traces: endusermessaging-notify-code-configuration-channels-and-tags#task#9.

## Evidence
Replayed against the emulator from this checkout; 5 clusters, none marked owned_by or stub_path by the replay report.

Cause 1 - endusermessaging-brand-profile-tagging#task#4, ListTagsForResource, expected 200 / actual 200:
  $.tags[key="owner"]: missing (expected {'key': 'owner', 'value': 'ops'}, got None)
  $.tags[key="team"].value: value_mismatch (expected 'beta', got 'alpha')
  expected: the tag set the preceding tag write left on the profile - the added key present, the pre-existing key carrying its new value.
  actual:   the create-time tag set, unchanged. (Key and value literals above are this fixture's values, not part of the requirement.)

Cause 2 - endusermessaging-brand-profile-tagging#task#6 (also #task#10), ListTagsForResource, expected 200 / actual 200:
  $.tags[0]: missing (expected {'key': 'owner', 'value': 'ops'}, got {'key': 'team', 'value': 'alpha'})
  $.tags[0]: extra (expected {'key': 'owner', 'value': 'ops'}, got {'key': 'team', 'value': 'alpha'})
  expected: only the surviving tag, the untagged key gone.
  actual:   the untagged key still present and the added key still absent.

Cause 3 - endusermessaging-notify-code-configuration-channels-and-tags#task#3, ListTagsForResource, expected 200 / actual 200:
  $.tags[key="stage"]: missing (expected {'key': 'stage', 'value': 'test'}, got None)

Cause 4 - endusermessaging-brand-profile-tagging#task#9, ListTagsForResource, expected 400 / actual 400:
  $.message: value_mismatch
  expected: "ARN partition/region mismatch: expected partition 'aws' region 'us-west-2', got partition 'aws' region 'us-east-1'"
  actual:   "Invalid ARN: arn:aws:end-user-messaging:us-east-1:<account>:brand-profile/<id> does not name a resource in Region us-west-2, the Region this request was made in"
  (The partition/Region tokens are the request's own values filling the template; the account id and the profile id in the actual message are per-run values.)

Cause 5 - endusermessaging-notify-code-configuration-channels-and-tags#task#9, ListTagsForResource, expected 404 / actual 200:
  status: expected 404, got 200
  $.message: missing (expected 'Resource not found - ResourceType="notify-code-configuration" ResourceId="<id>"', got None)
  $.resourceId: missing (expected the identifier the ARN names, got None)
  $.resourceType: missing (expected 'notify-code-configuration', got None)
  $headers.x-amzn-errortype: missing (expected the error shape, got None)
  (The configuration id in the recorded message is a per-run value filling the template.)

## Steps to Reproduce
1. POST /v1/brand-profiles (CreateBrandProfile) with tags, e.g. [{"key":"team","value":"alpha"}]; note brandProfileArn.
2. POST /v1/tags/{brandProfileArn} (TagResource) with tags [{"key":"owner","value":"ops"},{"key":"team","value":"beta"}].
3. GET /v1/tags/{brandProfileArn} (ListTagsForResource) -> emulator reports only team=alpha; AWS reports owner=ops and team=beta. (Cause 1)
4. DELETE /v1/tags/{brandProfileArn}?tagKeys=team (UntagResource), then repeat step 3 -> emulator still reports team=alpha and no owner; AWS reports owner=ops only. (Cause 2)
5. POST /v1/notify-code-configurations (CreateNotifyCodeConfiguration), tag its ARN with stage=test, then GET /v1/tags/{notifyCodeConfigurationArn} -> the stage tag is absent. (Cause 3)
6. GET /v1/tags/{arn} where the ARN's Region differs from the Region the request is signed for -> 400 ValidationException, but with the "Invalid ARN: ... does not name a resource in Region ..., the Region this request was made in" sentence instead of "ARN partition/region mismatch: expected partition '<p>' region '<r>', got partition '<p>' region '<r>'". (Cause 4)
7. Delete the notify code configuration, then GET /v1/tags/{notifyCodeConfigurationArn} -> emulator answers 200 with the deleted resource's tags; AWS answers 404 ResourceNotFoundException with message 'Resource not found - ResourceType="notify-code-configuration" ResourceId="<id>"'. (Cause 5)
Replay: fixtures endusermessaging-brand-profile-tagging (task#4, task#6, task#9) and endusermessaging-notify-code-configuration-channels-and-tags (task#3, task#9).
