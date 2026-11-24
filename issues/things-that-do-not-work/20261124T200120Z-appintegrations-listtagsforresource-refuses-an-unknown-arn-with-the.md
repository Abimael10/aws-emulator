Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: appintegrations.tag-resource-on-nonexistent-data-integration-rejected
Issue-Shape-Id: com.amazonaws.appintegrations#AmazonAppIntegrationService
Issue-Key: appintegrations|ListTagsForResource|ResourceNotFoundException
Issue-Commit: 3b5632730b2042519b3a6b1d3d27bedfe6029b95

# appintegrations ListTagsForResource refuses an unknown ARN with the wrong error code and message (ResourceNotFoundException instead of NotFoundException)

> _Note: this issue was identified against the code base at commit `3b5632730b2042519b3a6b1d3d27bedfe6029b95`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ListTagsForResource

## Description
Cause 1 of 1: ListTagsForResource reuses the TagResource/UntagResource not-found refusal, so it answers the wrong error code and the wrong message for an ARN that names no taggable resource.

Failing check ids: appintegrations.tag-resource-on-nonexistent-data-integration-rejected

AWS:      For AppIntegrations, the three tagging operations do NOT share a not-found refusal. TagResource and UntagResource on an ARN that names no resource in the account refuse with error code `ResourceNotFoundException` and the message `Could not find resource with Arn: <resourceArn>.` — fixed text `Could not find resource with Arn: ` plus the `resourceArn` value taken verbatim from the request, then a trailing period. ListTagsForResource on that SAME ARN refuses with a DIFFERENT code and a message that carries no variable part at all: code `NotFoundException`, message `Resource was not found` — that string is entirely fixed text, with nothing interpolated from the request or from state and no trailing period.
            Smithy (services/service-com-amazonaws-appintegrations-amazonappintegrationservice/smithy-model/com-amazonaws-appintegrations-amazonappintegrationservice.json): ListTagsForResource declares errors InternalServiceError (500), InvalidRequestException (400), ResourceNotFoundException (404), ThrottlingException (429); `resourceArn` is a required @httpLabel on GET /tags/{resourceArn}. The model therefore does NOT declare `NotFoundException` on this operation, and the code AWS actually emits here is recorded behavior only — the recording is the authority for it, not the model. The recorded refusal's HTTP status is not asserted by the check, so the status is not part of this finding; only the code and the message are. Hypothesis: AWS serves this operation's ARN lookup through a shared tagging front end that raises its own unmodeled NotFoundException, which is why the wording and the code differ from its two siblings.

Emulator: ListTagsForResource refuses the unknown ARN with code `ResourceNotFoundException` and message `Could not find resource with Arn: <arn>.` — i.e. it emits the TagResource/UntagResource refusal. The replay observed code `ResourceNotFoundException` with the `Could not find resource with Arn: ...` wording where `NotFoundException` / `Resource was not found` was required. TagResource and UntagResource on the same ARN are correct in the emulator; only ListTagsForResource diverges, so the defect is that this one operation does not have its own distinct refusal.

Why:      A client that branches on the tagging error contract gets both halves wrong. Code-based handling is broken: `except appi.exceptions.NotFoundException` never matches, the refusal escapes as the unrelated ResourceNotFoundException, and retry/skip logic keyed on NotFoundException for a tag read falls through to a generic failure path. Message-based handling is broken in the opposite direction: the emulator interpolates an ARN into a message AWS leaves constant, so a client that matches or logs the exact text sees a variable string where AWS sends a fixed one. It also destroys the signal AWS deliberately provides — on real AWS the differing code is how a caller distinguishes 'the tag READ found no resource' from 'the tag WRITE found no resource'; under the emulator all three operations are indistinguishable.

Fix:      The ListTagsForResource handler's not-found refusal in services/service-com-amazonaws-appintegrations-amazonappintegrationservice/src/ops/list_tags_for_resource.rs. It must stop sharing the write-path refusal helper used by TagResource/UntagResource (crate::state::tagged_resource_not_found, which builds the ARN-bearing wording) and instead raise this operation's own refusal carrying the `NotFoundException` code and the constant message `Resource was not found`. Because that code is not in the operation's modeled error list, the operation's error channel / serializer must be able to carry it; leave TagResource's and UntagResource's refusals untouched, since they already match AWS.

Traces:   appintegrations.tag-resource-on-nonexistent-data-integration-rejected. The check builds `arn:aws:app-integrations:<region>:<account>:data-integration/<uuid4>` for a DataIntegration it never creates, asserts TagResource and UntagResource refuse with ResourceNotFoundException naming that ARN (both pass), then asserts ListTagsForResource on the same ARN refuses with (NotFoundException, 'Resource was not found') — this is the failing assertion. The region, account and uuid in any one run are per-run values that fill the ARN template and are not part of the requirement. The handler's refusal site is list_tags_for_resource.rs lines 162-166.

## Evidence
Check: appintegrations.tag-resource-on-nonexistent-data-integration-rejected [mismatch]

Call: appi.list_tags_for_resource(resourceArn="arn:aws:app-integrations:<region>:<account>:data-integration/<uuid4>")  (ARN of a DataIntegration that was never created)

Expected (AWS, recorded):
  Error.Code    = "NotFoundException"
  Error.Message = "Resource was not found"          <- entirely fixed text, no interpolation

Actual (emulator):
  Error.Code    = "ResourceNotFoundException"
  Error.Message = "Could not find resource with Arn: arn:aws:app-integrations:us-west-2:000000000000:data-integration/8cc61e2f-2f81-4f2b-8758-cc9a58a3585f."
                  fixed text "Could not find resource with Arn: " + the request's resourceArn + "."
                  (the region/account/uuid shown are that run's values, not requirements)

Assertion from the replay:
  AssertionError: expected ('NotFoundException', 'Resource was not found'), got ('ResourceNotFoundException', 'Could not find resource with Arn: arn:aws:app-integrations:us-west-2:000000000000:data-integration/8cc61e2f-2f81-4f2b-8758-cc9a58a3585f.')

Positive controls in the same check that PASS, isolating the defect to ListTagsForResource:
  - tag_resource(resourceArn=<missing arn>, tags={...})     -> ResourceNotFoundException, "Could not find resource with Arn: <arn>."  (correct)
  - untag_resource(resourceArn=<missing arn>, tagKeys=[...]) -> ResourceNotFoundException, "Could not find resource with Arn: <arn>."  (correct)

Emulator source responsible (services/service-com-amazonaws-appintegrations-amazonappintegrationservice/src/ops/list_tags_for_resource.rs:162-166):
  let tags = state.tags_of(&input.resource_arn).ok_or_else(|| {
      ListTagsForResourceErrorModeled::ResourceNotFoundException(ResourceNotFoundException {
          message: Some(crate::state::tagged_resource_not_found(&input.resource_arn)),
      })
  })?;

Smithy model for ListTagsForResource: errors = InternalServiceError(500), InvalidRequestException(400), ResourceNotFoundException(404), ThrottlingException(429); NotFoundException is NOT modeled on this operation, so AWS's actual code here is recorded behavior only.

## Steps to Reproduce
1. Boot the emulator and point a botocore appintegrations client at it.
2. Construct an ARN for a DataIntegration that does not exist:
     missing_arn = f"arn:aws:app-integrations:{region}:{account}:data-integration/{uuid.uuid4()}"
3. Confirm the two write-path refusals are correct (they pass today):
     appi.tag_resource(resourceArn=missing_arn, tags={"owner": "ops"})      -> ResourceNotFoundException, "Could not find resource with Arn: <missing_arn>."
     appi.untag_resource(resourceArn=missing_arn, tagKeys=["owner"])        -> ResourceNotFoundException, "Could not find resource with Arn: <missing_arn>."
4. Call the read path on the SAME ARN:
     appi.list_tags_for_resource(resourceArn=missing_arn)
5. Observe the raised ClientError carries Error.Code "ResourceNotFoundException" and the ARN-bearing message, instead of AWS's Error.Code "NotFoundException" with the constant message "Resource was not found".
6. Re-replay to confirm a fix: appintegrations.tag-resource-on-nonexistent-data-integration-rejected (its positive control also requires that ListTagsForResource still succeeds and returns the exact tag map on a real DataIntegration, both after TagResource and after UntagResource).
