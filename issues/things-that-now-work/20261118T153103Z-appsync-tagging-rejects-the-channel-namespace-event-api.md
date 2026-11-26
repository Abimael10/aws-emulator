Issue-Source: trace-sweep-divergence
Issue-Trace: channel-namespace-tagging
Issue-Step: task#3
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: channel-namespace-tagging
Issue-Shape-Id: com.amazonaws.appsync#AWSDeepdishControlPlaneService
Issue-Key: appsync|TagResource|4xx->4xx|$.message
Issue-Key: appsync|TagResource|2xx->4xx|$
Issue-Key: appsync|TagResource|2xx->4xx|status
Issue-Key: appsync|ListTagsForResource|2xx->4xx|$.tags
Issue-Key: appsync|ListTagsForResource|2xx->4xx|status
Issue-Key: appsync|UntagResource|2xx->4xx|$
Issue-Key: appsync|UntagResource|2xx->4xx|status
Issue-Key: appsync|ListTagsForResource|4xx->4xx|$
Issue-Key: appsync|ListTagsForResource|4xx->4xx|$.message
Issue-Key: appsync|ListTagsForResource|4xx->4xx|$headers.x-amzn-errortype
Issue-Key: appsync|ListTagsForResource|4xx->4xx|status
Issue-Commit: c9344e2779494e078eb181c7552bbea5ec625bd0

# AppSync tagging rejects the channel-namespace/Event-API resource ARN as "Invalid input resource arn", so the whole tag surface is unusable (5 causes)

> _Note: this issue was identified against the code base at commit `c9344e2779494e078eb181c7552bbea5ec625bd0`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
TagResource

## Description
Every AppSync tagging call made against the resource ARN used by the channel-namespace tagging flow is refused by the emulator with HTTP 400 BadRequestException and the message "Invalid input resource arn", whatever the operation and whatever the request body. The recording shows AWS accepting the very same ARN on the very same requests (it validates the tag keys, stores the tags, lists them back, removes them, and after the resource is gone reports it as not found), so the emulator is failing to parse/resolve that resource ARN form at all and short-circuits before any tagging logic runs. Smithy (appsync): TagResource/UntagResource/ListTagsForResource each take resourceArn as a required httpLabel and model BadRequestException (400) and NotFoundException (404); the model's ResourceArn pattern is ^arn:aws:appsync:[A-Za-z0-9_/.-]{0,63}:\d{12}:apis/[0-9A-Za-z_-]{26}$ with @length 70-75, which the recorded, accepted request shows is narrower than what the service really admits — so the pattern cannot be the grounds for a refusal.

Cause 1 of 5 — TagResource with a reserved tag key. AWS: a tag key beginning with the reserved prefix "aws:" (case-insensitive) is refused with 400 BadRequestException and the message "Invalid tag key. Customers cannot set tag keys starting with aws: (case insensitive)". That message is entirely fixed text — it names no request value. Emulator: 400 with message "Invalid input resource arn", i.e. it never reaches tag-key validation. Why: a client probing which tag keys are reserved is told its ARN is malformed, so it cannot distinguish a reserved-prefix rejection from an addressing error and retries the same doomed call. Fix: the TagResource handler's input validation — resolve the resource ARN first, then apply the reserved-prefix check on each key. Traces: channel-namespace-tagging#task#3.

Cause 2 of 5 — TagResource with valid tags. AWS: recorded behavior only — the call succeeds, answering HTTP 204 with an empty body ({}). Emulator: 400 BadRequestException {"__type": "BadRequestException", "message": "Invalid input resource arn"}. Why: tags can never be attached to this resource, so nothing downstream that keys off tags (cost allocation, tag-based discovery, ListTagsForResource) can work. Fix: the TagResource handler plus the tag store that associates a tag map with the resolved resource. Traces: channel-namespace-tagging#task#4.

Cause 3 of 5 — ListTagsForResource on a tagged resource. AWS: 200 with Smithy ListTagsForResourceResponse$tags holding exactly the tag map previously stored for that resource (in the recording, the two keys set by the preceding TagResource). Emulator: 400 BadRequestException "Invalid input resource arn" and no tags member. Why: a client cannot read back tags it just set, so tag-driven reconciliation loops see the resource as untagged forever. Fix: the ListTagsForResource handler's ARN resolution and its read of the tag store. Traces: channel-namespace-tagging#task#5 (same cluster: #task#7).

Cause 4 of 5 — UntagResource. AWS: recorded behavior only — removing tag keys succeeds with HTTP 204 and an empty body ({}). Emulator: 400 BadRequestException "Invalid input resource arn". Why: tags cannot be removed, so a cleanup path that must untag before deleting a resource fails hard. Fix: the UntagResource handler's ARN resolution and tag-store delete. Traces: channel-namespace-tagging#task#6.

Cause 5 of 5 — ListTagsForResource after the resource is deleted. AWS: 404 NotFoundException with message "Resource was not found" and the x-amzn-errortype header naming NotFoundException; the message is fixed text with no request-derived parts. Emulator: 400 BadRequestException "Invalid input resource arn" (x-amzn-errortype: BadRequestException), and it keeps answering 400 for the whole poll window, so a teardown waiting for the resource to disappear never observes the terminal 404. Why: the error class is wrong (client addressing error instead of not-found), so deletion-confirmation polling cannot terminate and cleanup code treats a successful delete as a failure. Fix: the ListTagsForResource handler's resource lookup — distinguish an unparseable ARN (BadRequestException) from a well-formed ARN naming a resource that does not exist (NotFoundException "Resource was not found").

Related open issue, which this does NOT duplicate: 20261027T222245Z-appsync-tagging-operations-are-not-gated-by-identity.md reports that tagging calls which DO succeed are not IAM-gated; here the calls never succeed at all because the ARN is refused.

## Evidence
Replayed fixture channel-namespace-tagging (service appsync), trace set simparity-wire-aws-v1:
- task#3 TagResource (reserved aws: tag key): expected 400 message 'Invalid tag key. Customers cannot set tag keys starting with aws: (case insensitive)'; got 400 message 'Invalid input resource arn'.
- task#4 TagResource (valid tags): expected status 204 body {}; got status 400 body {'__type': 'BadRequestException', 'message': 'Invalid input resource arn'}.
- task#5 / task#7 ListTagsForResource: expected status 200 with $.tags = {'Env': 'staging', 'Team': 'checkout'}; got status 400 and $.tags missing.
- task#6 UntagResource: expected status 204 body {}; got status 400 body {'__type': 'BadRequestException', 'message': 'Invalid input resource arn'}.
- teardown#10 ListTagsForResource after delete: expected status 404, $.message 'Resource was not found', x-amzn-errortype NotFoundException; got status 400, $.message 'Invalid input resource arn', x-amzn-errortype 'BadRequestException' — status 400 not in [404] after 15 poll attempts.
All five steps report stub_path = "" and owned_by = [] (no stub and no owning open issue).

## Steps to Reproduce
1. Replay wire fixture channel-namespace-tagging against the emulator (trace set simparity-wire-aws-v1).
2. Observe step task#3: POST /v1/tags/{resourceArn} with a tag key starting with 'aws:' answers 400 'Invalid input resource arn' instead of the reserved-tag-key refusal.
3. Observe steps task#4 (TagResource), task#5/task#7 (ListTagsForResource), task#6 (UntagResource): each answers 400 'Invalid input resource arn' instead of 204/200.
4. Observe teardown#10: after the resource is deleted, ListTagsForResource keeps answering 400 BadRequestException instead of 404 NotFoundException 'Resource was not found', so the poll for the terminal state exhausts its 15 attempts.
