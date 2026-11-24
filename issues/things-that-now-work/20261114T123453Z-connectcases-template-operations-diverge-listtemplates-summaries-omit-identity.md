Issue-Source: trace-sweep-divergence
Issue-Trace: connectcases-template-lifecycle
Issue-Step: task#6
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: connectcases-template-lifecycle connectcases-template-and-case-lifecycle
Issue-Shape-Id: com.amazonaws.connectcases#AmazonConnectCases
Issue-Key: connectcases|ListTemplates|2xx->2xx|$.templates[name="sim2real-probe-cc-template-0eed9195"].arn
Issue-Key: connectcases|ListTemplates|2xx->2xx|$.templates[name="sim2real-probe-cc-template-0eed9195"].id
Issue-Key: connectcases|ListTemplates|2xx->2xx|$.templates[name="sim2real-probe-cc-template-0eed9195"].tagPropagationConfigurations
Issue-Key: connectcases|CreateCase|4xx->4xx|$.message
Issue-Commit: 0e799ac55357b8146906bd333c1ff0235c6296d4

# connectcases template operations diverge: ListTemplates summaries omit identity members and CreateCase required-field refusal is mis-worded

> _Note: this issue was identified against the code base at commit `0e799ac55357b8146906bd333c1ff0235c6296d4`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ListTemplates

## Description
Two net-new wire divergences on the connectcases template surface. Both are template-scoped responses that drop or reword information AWS supplies, so a client that lists templates and then acts on one is misled. THIS ISSUE HAS 2 CAUSES; both must be fixed before it is closed.

=== Cause 1 of 2: ListTemplates summaries carry no template identifier, no ARN and no tagPropagationConfigurations ===
AWS: Smithy (connectcases) ListTemplates output member `templates` targets TemplateSummaryList of TemplateSummary. TemplateSummary models `templateId` (@required, @length min 1 max 500), `templateArn` (@required, @length min 1 max 500), `name` (@required), `status` (@required) and `tagPropagationConfigurations` (TagPropagationConfigurationList, @length max 1). So every element of the list carries that template's identifier and its ARN, of the general form arn:aws:cases:<region>:<account-id>:domain/<domainId>/template/<templateId> — the domain and template identifiers come from the request and from state, never a constant. The recording also shows a template with no tag-propagation configuration serializing `tagPropagationConfigurations` as an empty list rather than omitting the member.
Emulator: answers 200 with the right element count and the right `name`/`status`, but the summary for the just-created template has the identifier member absent (None), the ARN member absent (None) and `tagPropagationConfigurations` absent (None).
Why: list-then-use is broken. The only way a client discovers a template id is by listing; with neither id nor ARN in the summary it cannot go on to GetTemplate, UpdateTemplate, DeleteTemplate or CreateCase for a template it just enumerated, and an SDK that enforces the @required members on TemplateSummary fails deserialization of the whole page instead of returning it.
Fix: the ListTemplates handler's TemplateSummary projection/serializer in the connectcases service crate — project the stored template's identifier and ARN into each summary, and serialize an empty list for tagPropagationConfigurations instead of omitting the member.
Related-but-not-covering: several open issues describe this same symptom (e.g. 20261113T153114Z-connectcases-listtemplates-list-then-use-broken-template-summary-carries, 20261112T221816Z-connectcases-listtemplates-omits-identity-keys-and-null-tagpropagationconfigurations, 20261109T223410Z-connectcases-listtemplates-summaries-omit-the-template-id-arn). The replay reports this cluster as unowned (no open issue carries its replay key), so it is filed here with the fixture binding attached.
Traces: connectcases-template-lifecycle#task#6 (ListTemplates).

=== Cause 2 of 2: CreateCase refuses a case missing the template's required fields with a generic structural message ===
AWS: Smithy (connectcases) CreateCase input has `domainId` (@httpLabel, @required), `templateId` (@required) and `fields` (FieldValueList, @required, @length max 220); the modeled refusal is ValidationException (HTTP 400, member `message` @required). When the submitted field values do not cover the fields the chosen template requires, the recorded 400 ValidationException message is "Field(s) title are required": the fixed text is "Field(s) " ... " are required", and the variable part is the comma-separated list of the template's required field names that the request did not supply — in this recording the single name `title`, taken from that template's requiredFields, not a literal.
Emulator: answers 400 but with message "1 validation error detected: Value null at 'fields' failed to satisfy constraint: Member must not be null" — a structural member-presence complaint about the input shape instead of the template-required-fields rule.
Why: a client cannot tell which template fields it failed to populate, so it cannot repair and retry the CreateCase; and any caller matching on AWS's refusal wording treats this as an unrelated malformed-request error.
Fix: CreateCase validation in the connectcases service crate's CreateCase handler — resolve the referenced template, diff its requiredFields against the supplied field values, and raise ValidationException with AWS's "Field(s) <missing names> are required" wording, keeping the enumerated-constraint form only for genuine input-shape violations.
Related-but-not-covering: 20261109T181134Z-connectcases-validationexception-bodies-use-an-enumerated-constraint-template covers the enumerated-constraint wording on other operations and does not carry this step's replay key.
Traces: connectcases-template-and-case-lifecycle#task#13 (CreateCase).

## Evidence
Cause 1 — fixture connectcases-template-lifecycle, step task#6, ListTemplates, expected 200 / actual 200:
  $.templates[name="<created template>"].arn: missing (expected the template ARN arn:aws:cases:<region>:<account>:domain/<domainId>/template/<templateId>, got None)
  $.templates[name="<created template>"].id: missing (expected the template identifier, got None)
  $.templates[name="<created template>"].tagPropagationConfigurations: missing (expected [], got None)

Cause 2 — fixture connectcases-template-and-case-lifecycle, step task#13, CreateCase, expected 400 / actual 400:
  $.message: value_mismatch
    expected: 'Field(s) title are required'   (fixed text "Field(s) " + missing required field names + " are required")
    actual:   "1 validation error detected: Value null at 'fields' failed to satisfy constraint: Member must not be null"

Model citations: Smithy ListTemplates -> TemplateSummaryList/TemplateSummary {templateId @required, templateArn @required, name @required, status @required, tagPropagationConfigurations @length max 1}; Smithy CreateCase {domainId @required @httpLabel, templateId @required, fields @required @length max 220} errors ValidationException (400).

## Steps to Reproduce
1. Replay the wire corpus against the emulator (replay_wire_corpus) for trace set simparity-wire-aws-v1.
2. Cause 1: fixture connectcases-template-lifecycle, step task#6 — CreateDomain, CreateTemplate, then POST /domains/{domainId}/templates-list. Inspect the element whose `name` is the created template: its identifier, ARN and tagPropagationConfigurations are absent, where AWS returns the template id, its ARN and an empty list.
3. Cause 2: fixture connectcases-template-and-case-lifecycle, step task#13 — create a domain and a template whose requiredFields include a field (here `title`), then POST /domains/{domainId}/cases without supplying values for those required fields. The emulator answers 400 with "1 validation error detected: Value null at 'fields' failed to satisfy constraint: Member must not be null"; AWS answers 400 ValidationException "Field(s) title are required".
