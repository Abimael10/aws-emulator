Issue-Source: trace-sweep-divergence
Issue-Trace: connectcases-template-lifecycle
Issue-Step: task#6
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: connectcases-template-lifecycle
Issue-Shape-Id: com.amazonaws.connectcases#AmazonConnectCases
Issue-Key: connectcases|ListTemplates|2xx->2xx|$.templates[name="sim2real-probe-cc-template-e47ec1ca"].arn
Issue-Key: connectcases|ListTemplates|2xx->2xx|$.templates[name="sim2real-probe-cc-template-e47ec1ca"].id
Issue-Key: connectcases|ListTemplates|2xx->2xx|$.templates[name="sim2real-probe-cc-template-e47ec1ca"].tagPropagationConfigurations
Issue-Commit: 7818cb79416136e769ff64d8074c2c80c5ca81b2

# connectcases TemplateSummary serializer omits templateId templateArn and tagPropagationConfigurations on list pages

> _Note: this issue was identified against the code base at commit `7818cb79416136e769ff64d8074c2c80c5ca81b2`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ListTemplates

## Description
ONE net-new wire divergence on the connectcases ListTemplates surface. THIS ISSUE HAS 1 CAUSE.

=== Cause 1 of 1: ListTemplates page elements carry no template identifier, no template ARN and no tagPropagationConfigurations ===
AWS: Smithy (com.amazonaws.connectcases) ListTemplates -> POST /domains/{domainId}/templates-list, output member `templates` targets TemplateSummaryList (@length max 100, @required) of TemplateSummary. TemplateSummary models `templateId` (TemplateId, @required, @length min 1 max 500, "The unique identifier for the template"), `templateArn` (TemplateArn, @required, @length min 1 max 500, "The Amazon Resource Name (ARN) of the template"), `name` (@required), `status` (@required) and `tagPropagationConfigurations` (TagPropagationConfigurationList, @length max 1). So EVERY element of a returned page carries that template's identifier and its ARN; the ARN has the general form arn:aws:cases:<region>:<account-id>:domain/<domainId>/template/<templateId>, where region, account, domainId and templateId all come from the request and from the emulator's own state and are never constants. The recording additionally shows that a template with no tag-propagation configuration serializes `tagPropagationConfigurations` as an EMPTY LIST, not as an omitted/null member (recorded behavior only for the empty-vs-absent choice; the member itself is modeled).
Emulator: answers 200 with the right number of elements and the correct `name` and `status`, but for the just-created template the identifier member is absent (None), the ARN member is absent (None) and `tagPropagationConfigurations` is absent (None).
Why: list-then-use is broken. Listing is the only way a client discovers a template id, so with neither `templateId` nor `templateArn` in the summary it cannot go on to GetTemplate, UpdateTemplate, DeleteTemplate or CreateCase for a template it just enumerated; and an SDK that enforces the @required members of TemplateSummary fails deserialization of the entire page rather than returning the templates it did receive. A client that reads `tagPropagationConfigurations` as a list also hits a null where AWS always gives it an iterable.
Fix: the connectcases service crate's ListTemplates handler and its TemplateSummary projection/serializer — project the stored template's identifier and its ARN into every summary element, and serialize an empty list for `tagPropagationConfigurations` instead of omitting the member.
Related-but-not-covering: many open issues describe this same symptom (20261114T123453Z-connectcases-template-operations-diverge-listtemplates-summaries-omit-identity, 20261113T153114Z-connectcases-listtemplates-list-then-use-broken-template-summary-carries, 20261113T133144Z-connectcases-listtemplates-response-omits-per-template-identifier-arn-and, 20261112T221816Z-connectcases-listtemplates-omits-identity-keys-and-null-tagpropagationconfigurations, 20261112T073215Z-connectcases-templatesummary-projection-never-emits-identity-members-for, 20261111T051856Z, 20261110T194709Z, 20261110T113333Z, 20261109T223410Z, and 20261114T191411Z-connectcases-response-serialization-diverges-tagresource-success-status-and). The replay reports this cluster as UNOWNED — no open issue carries its replay key, because the earlier keys are pinned to a previous run's generated template name — so it is re-filed here with the live fixture/step binding attached.
Traces: connectcases-template-lifecycle#task#6 (ListTemplates).

## Evidence
Cause 1 — fixture connectcases-template-lifecycle, step task#6, service connectcases, operation ListTemplates, expected status 200 / actual status 200 (body divergence only):
  $.templates[name="<the created template>"].arn: missing (expected the template ARN, general form arn:aws:cases:<region>:<account-id>:domain/<domainId>/template/<templateId>; got None)
  $.templates[name="<the created template>"].id: missing (expected the template identifier from state; got None)
  $.templates[name="<the created template>"].tagPropagationConfigurations: missing (expected [] — an empty list for a template with no tag propagation configured; got None)

The name, region, account, domain id and template id in the recorded line are per-run values that fill the template; the requirement is that the identifier member, the ARN member and an empty tagPropagationConfigurations list are present on every page element.

Model citations: Smithy ListTemplates {input domainId @required @httpLabel @length 1..500; output templates @required -> TemplateSummaryList @length max 100}; TemplateSummary {templateId @required @length 1..500, templateArn @required @length 1..500, name @required, status @required, tagPropagationConfigurations -> TagPropagationConfigurationList @length max 1}; http POST /domains/{domainId}/templates-list, code 200.

## Steps to Reproduce
1. Replay the wire corpus against the emulator (replay_wire_corpus) for trace set simparity-wire-aws-v1.
2. Cause 1: fixture connectcases-template-lifecycle, step task#6 — CreateDomain, then CreateTemplate, then POST /domains/{domainId}/templates-list (ListTemplates).
3. Inspect the page element whose `name` matches the template just created: its template identifier and template ARN members are absent (None) and tagPropagationConfigurations is absent (None), where AWS returns the template id, the full template ARN and an empty list.
