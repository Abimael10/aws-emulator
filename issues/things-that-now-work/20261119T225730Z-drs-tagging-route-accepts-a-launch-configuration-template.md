Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: drs.tag-resource-rejects-launch-configuration-template-arn drs.tag-resource-rejected-for-launch-configuration-template-arn
Issue-Shape-Id: com.amazonaws.drs#ElasticDisasterRecoveryService
Issue-Key: drs.tag-resource-rejects-launch-configuration-template-arn|_error#7|AssertionError|raise AssertionError(f"expected the call to be rejected, but it succeeded: {result!r}")
Issue-Key: drs.tag-resource-rejected-for-launch-configuration-template-arn|_expect_rejected#6|AssertionError|raise AssertionError(f"expected the call to be refused, but it succeeded: {result!r}")
Issue-Commit: 25f334ef49f0fe78ce039b48068cddfc6b5b1616

# drs tagging route accepts a launch-configuration-template ARN (200 + tag written) instead of refusing it with BadRequestException "Invalid input resource arn"

> _Note: this issue was identified against the code base at commit `25f334ef49f0fe78ce039b48068cddfc6b5b1616`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
TagResource

## Description
One net-new root cause in the drs resource-tagging route (POST/GET/DELETE /tags/{resourceArn}).

Cause 1 of 1: the ARN-kind resolution shared by the tagging route treats a launch-configuration-template ARN as a taggable DRS resource, so TagResource (and, on the same resolution, ListTagsForResource and UntagResource) answers 200 and mutates the template's tag set, where AWS refuses the ARN as an input fault. Failing checks: drs.tag-resource-rejects-launch-configuration-template-arn, drs.tag-resource-rejected-for-launch-configuration-template-arn.

AWS: Smithy (com.amazonaws.drs) models TagResource POST /tags/{resourceArn} (input resourceArn: ARN @httpLabel @required @length(20,2048) @pattern ^arn:.{16,2044}$, tags: TagsMap @required; output Unit; @idempotent), ListTagsForResource GET /tags/{resourceArn} -> {tags: TagsMap}, UntagResource DELETE /tags/{resourceArn} (tagKeys @httpQuery). The model does NOT enumerate the resource kinds the generic tag API supports, and it declares no BadRequestException shape at all for this service, so the refusal itself is RECORDED BEHAVIOR ONLY: a launch-configuration-template ARN is not a resource type the DRS tag API supports, and all three operations refuse it with HTTP 400, error code BadRequestException, and the message "Invalid input resource arn". That message is entirely FIXED text — nothing in it is interpolated from the request or from state (no ARN, no id, no resource kind). The refusal is independent of existence: an existing template's ARN is refused exactly like an absent one, and a launch configuration template's tags are settable only at create time and readable only through DescribeLaunchConfigurationTemplates. Because the call is refused, nothing is written: the template's tag set is exactly what CreateLaunchConfigurationTemplate set. Related, already open: 20261119T114813Z-drs-tag-apis-listtagsforresource-drops-tagresource-added-keys.md reports the same ARN class being answered as 404 ResourceNotFoundException on ListTagsForResource; it does not cover TagResource ACCEPTING the ARN with 200 and writing to it, and these checks are not marked as owned by it. The open stub issue 20261110T042638Z-drs-tagresource-stub.md likewise does not apply: TagResource is implemented and answering 200 here.

Emulator: drs.tag_resource(resourceArn=<launch-configuration-template ARN>, tags={...}) returns HTTP 200 with an empty body (content-type application/json, content-length 0) and no error at all — the replay reports `AssertionError: expected the call to be rejected, but it succeeded: {'ResponseMetadata': {'HTTPStatusCode': 200, ...}}` for both checks, at the first TagResource call, so the subsequent ListTagsForResource/UntagResource refusals and the "tags unchanged" read in those checks were never reached. The accepted call also writes the supplied keys into the template's own tag set, so the state the refused attempt must leave untouched is changed.

Why: the route is overly permissive on a resource class AWS rejects. A client is told its tag landed on a launch configuration template when on real AWS the call is terminally refused (400/BadRequestException), so code that branches on BadRequestException to fall back to create-time tagging never fires, and tag-driven inventory/cost-allocation or IaC code builds state (tags on a template) that cannot exist on AWS. Worse, the accepted UntagResource on the same ARN would remove create-time tags that AWS leaves in place, so a caller's refused cleanup silently destroys state.

Fix: the tagging route's shared ARN resolution and its refusal class — `TaggedResource::in_arn` must not classify a `launch-configuration-template/...` resource part as a taggable DRS resource, and the route must refuse an unsupported resource kind BEFORE any lookup or mutation with HTTP 400, error code BadRequestException and the fixed message "Invalid input resource arn", for all three of TagResource, ListTagsForResource and UntagResource; 404/not-found stays reserved for a well-formed ARN of a SUPPORTED kind that is absent from state.

Traces: drs.tag-resource-rejects-launch-configuration-template-arn, drs.tag-resource-rejected-for-launch-configuration-template-arn. Code: services/service-com-amazonaws-drs-elasticdisasterrecoveryservice/src/state/tags.rs lines 48-52 and 58-73 (launch-configuration-template admitted as a TaggedResource kind), lines 102-106 / 137-141 (its tag set handed to the route), lines 155-169 (the route's only refusal is a 404 NotFoundException); services/service-com-amazonaws-drs-elasticdisasterrecoveryservice/src/ops/tag_resource.rs lines 190-219 (resolve, then write and answer 200).

## Evidence
Check drs.tag-resource-rejects-launch-configuration-template-arn: after create_launch_configuration_template(tags={'owner': 'sim2real-behav'}), tag_resource(resourceArn=<lct arn>, tags={'extra': 'v'}) was expected to raise BadRequestException with Message exactly "Invalid input resource arn" (HTTP 400). Observed: success — {'ResponseMetadata': {'HTTPStatusCode': 200, 'HTTPHeaders': {'content-type': 'application/json', 'content-length': '0', ...}}}; AssertionError "expected the call to be rejected, but it succeeded".

Check drs.tag-resource-rejected-for-launch-configuration-template-arn: same first call (tags={'sim2real-other': 'x'}) with the same expectation (BadRequestException / "Invalid input resource arn"); observed HTTP 200 and AssertionError "expected the call to be refused, but it succeeded". The checks' later assertions — ListTagsForResource and UntagResource on the same ARN must also be 400/BadRequestException/"Invalid input resource arn", and DescribeLaunchConfigurationTemplates must still show exactly the create-time tags — were never reached.

Model evidence: Smithy com.amazonaws.drs TagResource POST /tags/{resourceArn} -> Unit, errors AccessDeniedException(403), InternalServerException(500), ResourceNotFoundException(404), ThrottlingException(429), ValidationException(400); no BadRequestException shape exists in the model (smithy_query shape=BadRequestException -> not found), hence "recorded behavior only" for the 400/BadRequestException refusal.

## Steps to Reproduce
1. Initialize the drs account state (requires:drs-initialized).
2. created = drs.create_launch_configuration_template(tags={'owner': 'x'}); take arn = created['launchConfigurationTemplate']['arn'] and id = ...['launchConfigurationTemplateID'].
3. drs.tag_resource(resourceArn=arn, tags={'extra': 'v'}) — expect ClientError with Code 'BadRequestException', Message 'Invalid input resource arn' (HTTP 400); observe HTTP 200 with an empty body.
4. drs.list_tags_for_resource(resourceArn=arn) and drs.untag_resource(resourceArn=arn, tagKeys=['owner']) — expect the same 400/BadRequestException/'Invalid input resource arn' refusal from both.
5. drs.describe_launch_configuration_templates(launchConfigurationTemplateIDs=[id])['items'][0]['tags'] — expect exactly the create-time tags ({'owner': 'x'}), unchanged by the refused attempts.
