Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: rbin.tag-resource-and-list-tags-roundtrip
Issue-Shape-Id: com.amazonaws.rbin#AmazonRecycleBin
Issue-Key: rbin.tag-resource-and-list-tags-roundtrip|run#6|KeyError|before = rbin.list_tags_for_resource(ResourceArn=rule_arn)["Tags"]
Issue-Commit: 22c0b68190b6d53f08de46118af925b6725f9997

# rbin ListTagsForResource omits the Tags member for an untagged rule instead of returning an empty list

> _Note: this issue was identified against the code base at commit `22c0b68190b6d53f08de46118af925b6725f9997`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ListTagsForResource

## Description
Cause 1 of 1: ListTagsForResource answers a retention rule that carries no tags with a response body that has NO `Tags` member at all, where AWS answers with `Tags` present and empty.

AWS: Smithy (services/service-com-amazonaws-rbin-amazonrecyclebin/smithy-model/com-amazonaws-rbin-amazonrecyclebin.json) models `ListTagsForResource` as GET /tags/{ResourceArn} -> 200 with output `ListTagsForResourceResponse.Tags` (target `TagList`, @length min 0 max 200) — "Information about the tags assigned to the retention rule". The member is not @required, but the recorded behavior of the real service is that a freshly created, untagged rule answers 200 with `Tags` present as an EMPTY LIST: the behavioral check reads `list_tags_for_resource(...)["Tags"]` and asserts it equals `[]`, which only holds if the key is serialized. A list-of-tags reader on AWS is a present collection whose length is 0..200, not an absent member. (Model states the member and its 0-minimum length; the empty-list-when-untagged rendering is recorded behavior only.)

Emulator: for a rule with no stored tag assignments the handler returns `tags: None`, so the serialized body contains no `Tags` key. `crate::state::Rule::reported_tags()` deliberately normalises an empty tag set to `None` ("NOTHING at all when it has none"), and `ListTagsForResource::handle` renders the output straight from it. Replay of check `rbin.tag-resource-and-list-tags-roundtrip` therefore raised, in the client, `KeyError: 'Tags'` at `before = rbin.list_tags_for_resource(ResourceArn=rule_arn)["Tags"]` — no HTTP-level error, status was a success; the member was simply missing.

Why: any client that indexes or iterates the modelled list — `resp["Tags"]` in botocore, `resp.tags()` / `.unwrap_or_default()` idioms in typed SDKs, a tag-diffing or reconciliation loop in Terraform/CDK-style tooling — gets a missing-key failure or a null where AWS hands back an empty collection. The "no tags yet" state is the FIRST state of every rule, so the very common path of "read current tags, compute the delta, TagResource" breaks before it ever issues a write. It also hides the rest of the roundtrip: because the first read throws, the emulator's TagResource-then-read-back fidelity was never exercised by this check at all.

Fix: the rendering of the `Tags` output member for ListTagsForResource — the operation's handler/serialization path, and the shared state helper it reads it from, so that an untagged rule renders `Tags` as an empty list rather than an unset optional. Note the same helper also feeds the `Tags` member of other rbin shapes (CreateRule/GetRule/UpdateRule); only ListTagsForResource is grounded in a failing check here, so decide those deliberately rather than by side effect. There is one unrelated open rbin issue (20261028T050716Z-rbin-getrule-maintainability.md, about an inlined pattern renderer) — it does not cover this.

Traces: rbin.tag-resource-and-list-tags-roundtrip. Code sites: services/service-com-amazonaws-rbin-amazonrecyclebin/src/ops/list_tags_for_resource.rs:178-180 (`tags: rule.reported_tags()`), services/service-com-amazonaws-rbin-amazonrecyclebin/src/state.rs:399-401 (`reported_tags`).

## Evidence
Expected (AWS, per the recording the check encodes): CreateRule on an untagged EBS_VOLUME rule, then GET /tags/<RuleArn> -> HTTP 200 with body `{"Tags": []}`; the client expression `list_tags_for_resource(ResourceArn=rule_arn)["Tags"]` yields `[]`.

Actual (emulator): GET /tags/<RuleArn> -> success status with a body that omits `Tags` entirely (handler returns `tags: None` because `Rule::reported_tags()` maps an empty tag set to `None`). The client raises:
  File "check_rbin_tag_resource_and_list_tags_roundtrip.py", line 25, in run
    before = rbin.list_tags_for_resource(ResourceArn=rule_arn)["Tags"]
  KeyError: 'Tags'

No error code or error message is involved — this is a successful response whose shape diverges: a modelled optional list member that AWS renders present-and-empty is rendered absent. The requirement is structural (member `Tags` present, value an empty JSON array) and carries no per-run values; the rule id/ARN in any one replay is a value from the request/state, not part of the requirement.

## Steps to Reproduce
1. Boot the emulator and point an AWS SDK client for `rbin` at it.
2. CreateRule(ResourceType="EBS_VOLUME", RetentionPeriod={RetentionPeriodValue: 6, RetentionPeriodUnit: "DAYS"}, Description=<any>) and keep the returned `RuleArn` (no Tags supplied, so the rule is untagged).
3. Call ListTagsForResource(ResourceArn=<that RuleArn>) and read the `Tags` member of the response.
4. Observe the member is absent (botocore: `KeyError: 'Tags'`) instead of an empty list; AWS returns `{"Tags": []}`.
5. Equivalent: replay behavioral check `rbin.tag-resource-and-list-tags-roundtrip`.
