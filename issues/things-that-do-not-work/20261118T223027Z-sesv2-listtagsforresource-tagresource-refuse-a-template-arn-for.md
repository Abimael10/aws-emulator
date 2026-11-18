Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: sesv2.sesv2-email-template-tag-lifecycle-through-delete sesv2.sesv2-email-template-tag-overwrite-untag-empties-and-unknown-name-rejected
Issue-Shape-Id: com.amazonaws.sesv2#SimpleEmailService_v2
Issue-Key: sesv2|ListTagsForResource|NotFoundException
Issue-Commit: 03a0508b3aa0858110fb1a346ba54180a1d13415

# sesv2: ListTagsForResource/TagResource refuse a template ARN for a template that exists — 404 NotFoundException "No Template present with name: <NAME>" immediately after CreateEmailTemplate

> _Note: this issue was identified against the code base at commit `03a0508b3aa0858110fb1a346ba54180a1d13415`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ListTagsForResource

## Description
REOPENS 20261025T214624Z-sesv2-listtagsforresource-stub.md: its fix left the call refused.

Cause 1 of 1 — the email-template tag surface is unreachable through the template ARN.

AWS: Smithy ListTagsForResource input {ResourceArn (httpQuery ResourceArn, required, @length min 1)} -> output {Tags (required list)}, errors BadRequestException(400)/NotFoundException(404)/TooManyRequestsException(429); CreateEmailTemplate input {TemplateName (required), TemplateContent (required), Tags}. A template ARN of the form `arn:aws:ses:<region>:<account>:template/<TEMPLATE-NAME>` is a taggable SESv2 resource: immediately after CreateEmailTemplate with inline Tags, ListTagsForResource on that ARN answers 200 with exactly the tags supplied inline; TagResource on the ARN adds a key or overwrites an existing key in place; UntagResource removes keys and can leave the list EMPTY (200 with Tags: []); and GetEmailTemplate's TemplateContent is unaffected by either. NotFoundException is correct only for a template that does not exist, and its message is `No Template present with name: <TEMPLATE-NAME>` — fixed text `No Template present with name: ` plus one variable part, the template NAME parsed out of the resource ARN in the request, with no trailing period. That same refusal is what AWS gives after the template is deleted, and TagResource/UntagResource on a never-created template name give the identical message.

Emulator: ListTagsForResource on the template ARN is REFUSED 404 NotFoundException `No Template present with name: <TEMPLATE-NAME>` for a template that was just created successfully in the same check — the very first tag read after CreateEmailTemplate(TemplateName=<name>, TemplateContent=..., Tags=[...]) raises it. The message text itself matches AWS's not-found wording, so the defect is the lookup verdict, not the sentence: a live template is reported absent. Both template-tagging checks die on that first call, so TagResource-overwrite, UntagResource-to-empty, the content-unchanged assertions and the genuine after-delete / never-created refusals are never reached. Hypothesis (neither the model nor the recording states which): either the template-ARN branch of the tagging handlers resolves the ARN against the wrong store (or fails to parse `template/<name>` out of the ARN), or CreateEmailTemplate does not register the template in the resource table the tagging path consults, leaving the tags from the inline Tags member unrecorded.

Why: a client cannot read, add or remove tags on an email template at all — every tag call on a template ARN is a hard 404 — so tag-driven workflows (cost allocation, ownership, tag-based cleanup, infrastructure tools that reconcile tags after create) fail against a resource that demonstrably exists, and a client that treats NotFoundException as "the template is gone" deletes or recreates a live template.

Fix: the ListTagsForResource handler's ARN-to-resource resolution for the `template/<name>` resource type (and the same resolution shared by TagResource/UntagResource), together with whatever registration CreateEmailTemplate owes the tag store for its inline Tags member. The after-delete and never-created refusals must keep answering 404 with the existing message.

Traces: sesv2.sesv2-email-template-tag-lifecycle-through-delete, sesv2.sesv2-email-template-tag-overwrite-untag-empties-and-unknown-name-rejected.

## Evidence
Behavioral suite simparity-behavioral-aws-v1 (verdict: error on both checks).

sesv2.sesv2-email-template-tag-lifecycle-through-delete — after `create_email_template(TemplateName=<name>, TemplateContent={'Subject':'S','Text':'T'}, Tags=[{'Key':'env','Value':'dev'},{'Key':'team','Value':'core'}])` returned success, the next call `list_tags_for_resource(ResourceArn='arn:aws:ses:<region>:<account>:template/<name>')` raised:
`botocore.errorfactory.NotFoundException: An error occurred (NotFoundException) when calling the ListTagsForResource operation: No Template present with name: <name>`
Expected: 200 with Tags == [{'Key':'env','Value':'dev'},{'Key':'team','Value':'core'}].

sesv2.sesv2-email-template-tag-overwrite-untag-empties-and-unknown-name-rejected — after `create_email_template(..., Tags=[{'Key':'audit','Value':'original'}])` succeeded, `list_tags_for_resource(ResourceArn='arn:aws:ses:<region>:<account>:template/<name>')` raised the same:
`botocore.errorfactory.NotFoundException: An error occurred (NotFoundException) when calling the ListTagsForResource operation: No Template present with name: <name>`
Expected: 200 with Tags == [{'Key':'audit','Value':'original'}], then TagResource overwrite -> [{'Key':'audit','Value':'updated'}], then UntagResource(['audit']) -> [].

The template names shown are this run's generated names; the requirement is that the ARN of any existing template resolves.

## Steps to Reproduce
1. CreateEmailTemplate(TemplateName=T, TemplateContent={'Subject':'S','Text':'T'}, Tags=[{'Key':'env','Value':'dev'}]).
2. ListTagsForResource(ResourceArn='arn:aws:ses:<region>:<account>:template/T') — today 404 NotFoundException 'No Template present with name: T'; expect 200 with the inline tag.
3. TagResource on the same ARN with {'Key':'env','Value':'prod'} and re-read: expect the value overwritten in place, and GetEmailTemplate(TemplateName=T) content unchanged.
4. UntagResource on the same ARN with TagKeys=['env'] and re-read: expect 200 with Tags == [].
5. DeleteEmailTemplate(TemplateName=T), then ListTagsForResource on the ARN: expect 404 NotFoundException 'No Template present with name: T'.
6. TagResource and UntagResource on an ARN for a template name never created: expect the identical 404 message.
7. Re-replay the two checks in trace_ids.
