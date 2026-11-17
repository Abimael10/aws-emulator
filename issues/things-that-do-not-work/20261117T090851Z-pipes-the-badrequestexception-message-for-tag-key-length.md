Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: pipes.tag-resource-invalid-key-validation-rejected pipes.untag-resource-malformed-key-validation-rejected
Issue-Shape-Id: com.amazonaws.pipes#Pipes
Issue-Key: pipes.tag-resource-invalid-key-validation-rejected|run#7|AssertionError|assert message == expected_message, f"unexpected TagResource message {message!r}"
Issue-Key: pipes.untag-resource-malformed-key-validation-rejected|run#8|AssertionError|assert message == expected_message, f"unexpected UntagResource message {message!r}"
Issue-Commit: da4535b294faf80813d2c15d8cdb458adbb8bab6

# pipes: the BadRequestException message for tag-key length rejections on TagResource/UntagResource is served bare instead of inside AWS's {"message": ...} envelope

> _Note: this issue was identified against the code base at commit `da4535b294faf80813d2c15d8cdb458adbb8bab6`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
TagResource

## Description
Cause 1 of 1: Pipes rejects a tag key that violates the TagKey length constraint with a BadRequestException whose Error.Message is a JSON object literal — the validation sentence wrapped in a `{"message": "..."}` envelope. The emulator returns the correct error code but serves the bare validation sentence with the envelope stripped. This affects every constraint-validation rejection on the pipes tagging surface; it is observed on both TagResource (map-key constraint) and UntagResource (list-member constraint).

AWS: Smithy (services/service-com-amazonaws-pipes-pipes) models TagResource.tags -> TagMap (map, @length 1..50) whose key targets TagKey (string, @length min 1, max 128), and UntagResource.tagKeys -> TagKeyList (list, @length 0..50) whose member targets the same TagKey @length 1..128. The model lists only ValidationException/NotFoundException/InternalException for these operations; the recorded behavior shows the constraint rejection arriving as BadRequestException (HTTP 400) — recorded behavior only for the code. The recorded message is, for TagResource:
  {"message": "1 validation error detected: Value '{<badKey>=<value>}' at 'addedTags' failed to satisfy constraint: Map keys must satisfy constraint: [Member must have length less than or equal to 128, Member must have length greater than or equal to 1]"}
and for UntagResource:
  {"message": "1 validation error detected: Value '[<badKey>]' at 'removedTagKeys' failed to satisfy constraint: Member must satisfy constraint: [Member must have length less than or equal to 128, Member must have length greater than or equal to 1]"}
Fixed text: the whole `{"message": "..."}` envelope, the `1 validation error detected: Value '` / `' at '` / `' failed to satisfy constraint: ` scaffolding, the wire member names `addedTags` and `removedTagKeys`, the clause `Map keys must satisfy constraint:` (map input) or `Member must satisfy constraint:` (list input), and the bracketed bound list `[Member must have length less than or equal to 128, Member must have length greater than or equal to 1]` whose numbers are the TagKey @length bounds from the model. Variable parts taken from the request: the rendered offending value — `{<key>=<value>}` for the tag map, `[<key>]` for the key list — and the error count when more than one member is invalid.

Emulator: returns code BadRequestException, but Error.Message is the inner sentence only, with no JSON envelope: "1 validation error detected: Value '{kkk…=v}' at 'addedTags' failed to satisfy constraint: Map keys must satisfy constraint: [Member must have length less than or equal to 128, Member must have length greater than or equal to 1]" (TagResource) and "1 validation error detected: Value '[kkk…]' at 'removedTagKeys' failed to satisfy constraint: Member must satisfy constraint: [Member must have length less than or equal to 128, Member must have length greater than or equal to 1]" (UntagResource). The envelope-stripped form differs from AWS at the first character, so the two sides never compare equal. The inner sentence itself (member names, constraint clause, bounds) already matches AWS; the only divergence is the missing envelope.

Why: a client that parses the BadRequestException message as JSON to pull out the `message` field — the shape AWS actually sends for this error class — gets a JSON parse failure against the emulator, and any client matching on the exact message text, logging it, or asserting on it in a test sees a message that is not the one AWS produces. The failure mode is silent: the operation is correctly refused, so only the message text betrays it, and error-handling paths that branch on the parsed envelope take the wrong branch.

Fix: the pipes tagging constraint-validation path — where the BadRequestException for a TagKey length violation is constructed and serialized for TagResource and UntagResource. The inner validation sentence is already built correctly; the serializer must emit it inside the `{"message": "..."}` envelope AWS uses for this error class, rather than as the raw message string.

Traces: pipes.tag-resource-invalid-key-validation-rejected (TagResource, oversized map key at 'addedTags'), pipes.untag-resource-malformed-key-validation-rejected (UntagResource, oversized list member at 'removedTagKeys'). Both checks also assert the effect is absent (the malformed tag never lands, the existing tag survives) and that a well-formed tag/untag still succeeds; those legs passed — only the message assertion diverged. No existing open pipes issue covers this: 20261111T104349Z-pipes-response-envelopes-diverge-pipe-record-timestamps-carry.md touches the tagging surface but is about the 204 Content-Length header and timestamp tails, not the BadRequestException message body.

## Evidence
TagResource, key = "k"*200 (TagKey @length max 128):
  Expected (AWS, recorded): code BadRequestException, Message = {"message": "1 validation error detected: Value '{<key>=<value>}' at 'addedTags' failed to satisfy constraint: Map keys must satisfy constraint: [Member must have length less than or equal to 128, Member must have length greater than or equal to 1]"}
  Actual (emulator): code BadRequestException, Message = 1 validation error detected: Value '{<key>=<value>}' at 'addedTags' failed to satisfy constraint: Map keys must satisfy constraint: [Member must have length less than or equal to 128, Member must have length greater than or equal to 1]
  -> AssertionError: unexpected TagResource message (envelope absent)

UntagResource, tagKeys = ["k"*200]:
  Expected (AWS, recorded): code BadRequestException, Message = {"message": "1 validation error detected: Value '[<key>]' at 'removedTagKeys' failed to satisfy constraint: Member must satisfy constraint: [Member must have length less than or equal to 128, Member must have length greater than or equal to 1]"}
  Actual (emulator): code BadRequestException, Message = 1 validation error detected: Value '[<key>]' at 'removedTagKeys' failed to satisfy constraint: Member must satisfy constraint: [Member must have length less than or equal to 128, Member must have length greater than or equal to 1]
  -> AssertionError: unexpected UntagResource message (envelope absent)

The long key above is the checks' probe value (200 'k' characters); it fills the template's variable slot and is not part of the requirement. Error code and HTTP status match AWS in both cases.

## Steps to Reproduce
1. Create an SQS source queue, an SQS target queue and an IAM role trusted by pipes.amazonaws.com with sqs:ReceiveMessage/DeleteMessage/GetQueueAttributes/SendMessage.
2. CreatePipe with SourceParameters.SqsQueueParameters.BatchSize=1 and DesiredState=STOPPED; wait for CurrentState STOPPED. Keep the returned Arn.
3. TagResource(resourceArn=<pipe arn>, tags={"k"*200: "v"}). Observe the ClientError: code is BadRequestException as AWS gives, but Error.Message is the bare validation sentence rather than the {"message": "..."} envelope AWS returns.
4. TagResource(resourceArn=<pipe arn>, tags={"env": "keep-me"}), then UntagResource(resourceArn=<pipe arn>, tagKeys=["k"*200]). Same divergence with member name 'removedTagKeys' and the "Member must satisfy constraint:" clause.
5. ListTagsForResource confirms neither rejected call changed state (this part already matches AWS).
