Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: appintegrations.tag-resource-on-nonexistent-data-integration-rejected
Issue-Shape-Id: com.amazonaws.appintegrations#AmazonAppIntegrationService
Issue-Key: appintegrations|TagResource|ResourceNotFoundException
Issue-Commit: 0685ecf4ce56006659cf4e34833f0a1282c52a8c

# appintegrations: TagResource/UntagResource not-found message is the service's resource-typed template instead of AWS's generic "Could not find resource with Arn: <arn>."

> _Note: this issue was identified against the code base at commit `0685ecf4ce56006659cf4e34833f0a1282c52a8c`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
TagResource

## Description
ONE net-new root cause on the AppIntegrations emulator's tag-API refusal path. The service is bootstrapped and these operations are implemented and answering, so this is a fidelity defect, not a bootstrap gap. It is NOT covered by the open issue 20261029T223053Z-appintegrations-refusal-fidelity-createapplication-duplicate-name-namespace-answers.md (that issue carries CreateApplication duplicate-name, CreateDataIntegrationAssociation ExecutionConfiguration and UpdateDataIntegrationAssociation not-found; it says nothing about the tagging APIs and does not carry this check's replay key).

Cause 1 of 1 — TagResource (and UntagResource, which shares the same helper) refuses an ARN that names no existing resource with a resource-typed message AWS does not produce.
AWS: Smithy TagResource (POST /tags/{resourceArn}; resourceArn -> Arn @httpLabel @required @length 1..2048 @pattern ^arn:aws:...) and UntagResource (DELETE /tags/{resourceArn}) both model ResourceNotFoundException (HTTP 404, documented "The specified resource was not found."); the model does not state the message text, so the wording is recorded behavior only. The recording shows the tagging APIs use a GENERIC, resource-type-agnostic sentence: HTTP 404, error code ResourceNotFoundException, message "Could not find resource with Arn: <resourceArn>." — fixed text "Could not find resource with Arn: " plus the trailing period, with the single variable part being the resourceArn exactly as the request sent it (the ARN string, whatever resource type it names). AWS does NOT name the resource type and does NOT re-case the word "Arn" per type. Note this deliberately differs from the resource-scoped "Could not find <Resource> with identifier: <id>." family this service uses on Get/Update/Delete paths: the tag APIs take an arbitrary ARN and do not resolve a type before refusing.
Emulator: status 404 and code ResourceNotFoundException are correct, but the message is "Could not find DataIntegration with ARN: <resourceArn>." — it both inserts the resource type inferred from the ARN and spells the token "ARN" instead of "Arn". Produced by crate::state::tagged_resource_not_found, called from the tags_of_mut not-found branch of the TagResource handler (services/service-com-amazonaws-appintegrations-amazonappintegrationservice/src/ops/tag_resource.rs:160-164) and from the matching branch of UntagResource.
Why: the error message is part of the contract. A client, operator or test that matches on AWS's sentence (or parses the ARN out of it) sees text AWS cannot produce; worse, the emulator's wording asserts a resource TYPE for an ARN that resolves to nothing, so a caller passing an ARN of any other AppIntegrations type (or a malformed/foreign one) is told about a "DataIntegration" it never mentioned, which misdirects diagnosis.
Fix: the refusal-message construction for the tagging operations — the shared helper crate::state::tagged_resource_not_found (src/state.rs) must render AWS's generic sentence with the request's resourceArn interpolated verbatim and no resource type, and both the TagResource and UntagResource not-found branches must keep 404/ResourceNotFoundException while persisting nothing.
Traces: appintegrations.tag-resource-on-nonexistent-data-integration-rejected. Code: src/ops/tag_resource.rs:160-164 (not-found branch), src/ops/untag_resource.rs (same branch), src/state.rs (tagged_resource_not_found).

Scope note (not filed here): the same check's third leg expects ListTagsForResource on the same missing ARN to answer code NotFoundException with the message "Resource was not found" — a DIFFERENT, undocumented error class from the tag/untag pair. The check never reaches that leg because it fails on the first assertion, and ListTagsForResource is an unimplemented scaffold stub already filed for this service, so no claim is made about it here; it should be re-checked once that stub lands.

## Evidence
Behavioral suite simparity-behavioral-aws-v1 replayed against the emulator built from this checkout: appintegrations 19/23 passed (83%), 4 failed. Three of the four are marked ALREADY FILED / OWNED by existing issues (ListEventIntegrationAssociations scaffold stub; CreateDataIntegrationAssociation and UpdateDataIntegrationAssociation refusal messages) and are excluded. The remaining failure is the cause above.

Cause 1 — appintegrations.tag-resource-on-nonexistent-data-integration-rejected [mismatch]:
  request:  tag_resource(resourceArn='arn:aws:app-integrations:<region>:<account>:data-integration/<random uuid>', tags={'owner':'ops'})
  expected: ('ResourceNotFoundException', 'Could not find resource with Arn: arn:aws:app-integrations:us-west-2:000000000000:data-integration/743cf1f8-5900-44bb-9d1e-cd4d1813dad3.')
  got:      ('ResourceNotFoundException', 'Could not find DataIntegration with ARN: arn:aws:app-integrations:us-west-2:000000000000:data-integration/743cf1f8-5900-44bb-9d1e-cd4d1813dad3.')
  failure:  AssertionError at check_appintegrations_tag_resource_on_nonexistent_data_integration_rejected.py:47 (expect_error on the FIRST leg, so the untag and list legs were never reached).
  The region, account and uuid in that one run are per-run values filling the template; the requirement is the fixed text 'Could not find resource with Arn: ' + the request's resourceArn + '.' with no resource type named.
  HTTP status (404) and error code (ResourceNotFoundException) already match; only the message diverges.

Model evidence: smithy_query(query='operation', operation='TagResource,UntagResource,ListTagsForResource') and smithy_query(query='doc', shape='ResourceNotFoundException') on services/service-com-amazonaws-appintegrations-amazonappintegrationservice/smithy-model/com-amazonaws-appintegrations-amazonappintegrationservice.json — TagResource/UntagResource/ListTagsForResource each model ResourceNotFoundException (404), InvalidRequestException (400), ThrottlingException (429), InternalServiceError (500); no message text is modeled.

Emulator source: services/service-com-amazonaws-appintegrations-amazonappintegrationservice/src/ops/tag_resource.rs:160-164 raises ResourceNotFoundException with crate::state::tagged_resource_not_found(&input.resource_arn); the module doc at lines 15-20 states the intent to word it "like every other 404 of this service", which is exactly the divergence — AWS's tagging APIs do not use the per-resource family template.

## Steps to Reproduce
Boot the emulator and point a botocore appintegrations client at it.

1. Build an ARN for a resource that does not exist, e.g. missing_arn = 'arn:aws:app-integrations:<region>:<account>:data-integration/' + str(uuid.uuid4()).
2. Call tag_resource(resourceArn=missing_arn, tags={'owner': 'ops'}). Expect HTTP 404, code ResourceNotFoundException and message exactly 'Could not find resource with Arn: <missing_arn>.'; observe the message 'Could not find DataIntegration with ARN: <missing_arn>.'
3. Call untag_resource(resourceArn=missing_arn, tagKeys=['owner']). Expect the same 404/ResourceNotFoundException and the same generic message with the same ARN interpolated.
4. Positive control that must keep working: create a KMS key and an S3 bucket, create_data_integration(Name=<unique>, KmsKey=<key arn>, SourceURI='s3://<bucket>', Tags={'owner':'ops'}), then tag_resource / list_tags_for_resource / untag_resource against the returned Arn and confirm the tag set reads back as {'owner':'ops','cost-center':'eng'} then {'owner':'ops'}.
5. Re-run run_behavioral_suite and confirm appintegrations.tag-resource-on-nonexistent-data-integration-rejected passes (its ListTagsForResource leg additionally depends on the already-filed ListTagsForResource scaffold stub).
