Issue-Source: trace-sweep-divergence
Issue-Trace: connectcases-template-lifecycle
Issue-Step: task#6
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: connectcases-template-lifecycle
Issue-Shape-Id: com.amazonaws.connectcases#AmazonConnectCases
Issue-Key: connectcases|ListTemplates|2xx->2xx|$.templates[name="sim2real-probe-cc-template-48ac6b48"].arn
Issue-Key: connectcases|ListTemplates|2xx->2xx|$.templates[name="sim2real-probe-cc-template-48ac6b48"].id
Issue-Key: connectcases|ListTemplates|2xx->2xx|$.templates[name="sim2real-probe-cc-template-48ac6b48"].tagPropagationConfigurations
Issue-Commit: 44bbf79358888366d10de6b934e7daf7fcf849e8

# connectcases ListTemplates summaries omit template identity and tagPropagationConfigurations

> _Note: this issue was identified against the code base at commit `44bbf79358888366d10de6b934e7daf7fcf849e8`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ListTemplates

## Description
ONE cause.

=== Cause 1 of 1: ListTemplates template summaries carry no identifier, no ARN and no tagPropagationConfigurations ===

AWS: Smithy (com.amazonaws.connectcases#AmazonConnectCases) ListTemplates — POST /domains/{domainId}/templates-list, HTTP 200 — output member `templates` targets TemplateSummaryList (@length max 100, @required) of TemplateSummary. TemplateSummary models four @required members — the template identifier (`templateId`, @length min 1 max 500), the template ARN (`templateArn`, @length min 1 max 500), `name` (@required, @length 1..100, pattern ^.*[\S]$) and `status` (@required) — plus `tagPropagationConfigurations` (TagPropagationConfigurationList, @length max 1). So every element AWS returns identifies the template it summarizes and gives its ARN, whose general form is arn:aws:cases:<region>:<account-id>:domain/<domainId>/template/<templateId>: region, account, domain id and template id all come from the request and from stored state, never from a constant. The recording asserts these two members under the wire keys `id` and `arn` on each summary, and shows a template with no tag-propagation configuration serializing `tagPropagationConfigurations` as an empty list ([]) rather than omitting the member.

Emulator: answers 200 with the correct number of elements and the correct `name`, but on the summary for the just-created template the identifier member is absent (None), the ARN member is absent (None) and `tagPropagationConfigurations` is absent (None).

Why: list-then-use is broken for templates. Listing is the only way a client discovers template identifiers, so with neither id nor ARN in the summary it cannot go on to GetTemplate, UpdateTemplate, DeleteTemplate, or CreateCase against a template it just enumerated, and it has no ARN to tag or to reference in a policy. An SDK that enforces TemplateSummary's @required members fails deserialization of the entire page instead of returning it, so the call appears to fail rather than return incomplete data. A client reading `tagPropagationConfigurations` gets null where AWS gives an empty list, so a straightforward iteration over the list raises instead of iterating zero times.

Fix: the connectcases ListTemplates handler's TemplateSummary projection/serializer in the connectcases service crate — project the stored template's identifier and its ARN into each summary element, and serialize an empty list for tagPropagationConfigurations when the template has none instead of omitting the member.

Related-but-not-covering: open issue 20261114T123453Z-connectcases-template-operations-diverge-listtemplates-summaries-omit-identity describes the same symptom (its Issue-Key names a different run's generated template name), as do 20261113T153114Z-connectcases-listtemplates-list-then-use-broken-template-summary-carries, 20261112T221816Z-connectcases-listtemplates-omits-identity-keys-and-null-tagpropagationconfigurations and 20261109T223410Z-connectcases-listtemplates-summaries-omit-the-template-id-arn. The replay reports this cluster as UNOWNED — no open issue carries its replay key — so it is filed here with the fixture binding attached.

Traces: connectcases-template-lifecycle#task#6 (ListTemplates).

## Evidence
Fixture connectcases-template-lifecycle, step task#6, operation ListTemplates, verdict mismatch, expected status 200 / actual status 200:
  $.templates[name="<the created template>"].arn: missing — expected the template ARN of the general form arn:aws:cases:<region>:<account-id>:domain/<domainId>/template/<templateId> (domain id and template id taken from the request and from state), got None
  $.templates[name="<the created template>"].id: missing — expected the template's identifier as stored/returned by CreateTemplate, got None
  $.templates[name="<the created template>"].tagPropagationConfigurations: missing — expected [] (an empty list for a template with no tag-propagation configuration), got None

Status and body shape otherwise matched: 200, right element count, right `name`.

Model citations: Smithy ListTemplates {input domainId @httpLabel @required @length 1..500; output templates @required -> TemplateSummaryList @length max 100}, http POST /domains/{domainId}/templates-list code 200; TemplateSummary {templateId @required @length 1..500, templateArn @required @length 1..500, name @required @length 1..100 pattern ^.*[\S]$, status @required, tagPropagationConfigurations -> TagPropagationConfigurationList @length max 1}.

## Steps to Reproduce
1. Boot the emulator from this checkout and replay the wire corpus (replay_wire_corpus) for trace set simparity-wire-aws-v1.
2. Fixture connectcases-template-lifecycle, step task#6: CreateDomain, then CreateTemplate in that domain, then POST /domains/{domainId}/templates-list (ListTemplates).
3. Inspect the element of `templates` whose `name` equals the template just created: its identifier member and its ARN member are absent (None) and `tagPropagationConfigurations` is absent, where AWS returns the template id, the template ARN arn:aws:cases:<region>:<account-id>:domain/<domainId>/template/<templateId>, and an empty list.
4. Confirm the consequence: take the id/ARN from the listing and call GetTemplate or CreateCase — there is no id to take, so the follow-up call cannot be made.
