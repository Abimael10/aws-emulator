Issue-Source: trace-sweep-divergence
Issue-Trace: connectcases-domain-lifecycle-and-tags
Issue-Step: task#4
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: connectcases-domain-lifecycle-and-tags connectcases-template-lifecycle
Issue-Shape-Id: com.amazonaws.connectcases#AmazonConnectCases
Issue-Key: connectcases|TagResource|2xx->2xx|status
Issue-Key: connectcases|ListTemplates|2xx->2xx|$.templates[name="sim2real-probe-cc-template-ff1f818c"].arn
Issue-Key: connectcases|ListTemplates|2xx->2xx|$.templates[name="sim2real-probe-cc-template-ff1f818c"].id
Issue-Key: connectcases|ListTemplates|2xx->2xx|$.templates[name="sim2real-probe-cc-template-ff1f818c"].tagPropagationConfigurations
Issue-Commit: a5d6d303d04dbfc1d5a5028d751136cba595b88a

# connectcases response serialization diverges: TagResource success status and ListTemplates summary identity members

> _Note: this issue was identified against the code base at commit `a5d6d303d04dbfc1d5a5028d751136cba595b88a`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
TagResource

## Description
Two net-new wire divergences on the connectcases surface. Both are response-serialization faults: the emulator runs the right business logic but renders the HTTP response differently from AWS, so a client reading the status line or the listed element gets the wrong answer. THIS ISSUE HAS 2 CAUSES; both must be fixed before it is closed.

=== Cause 1 of 2: TagResource answers HTTP 200 with a JSON body where AWS answers 204 No Content ===
AWS: Smithy (connectcases) TagResource -> input {arn (Arn, @httpLabel, @required, @length min 1 max 500), tags (Tags, @required)}, output target smithy.api#Unit, @http POST /tags/{arn}, @idempotent; errors AccessDeniedException (403), InternalServerException (500), ResourceNotFoundException (404), ThrottlingException (429), ValidationException (400). The model's http code is reported as DEFAULTED (codeDefaulted: true), so the model does not pin the success status; the recorded behavior does. Recorded behavior: a successful TagResource on an existing resource ARN answers HTTP 204 with no content, which is the shape of a Unit-output tagging call on this service. There is no error code or message involved: the requirement is the status line of the success response.
Emulator: answers HTTP 200 for the same successful tag write (the tags themselves are merged correctly; only the status diverges).
Why: a client that keys off the status for a no-content response is misled. SDKs and middlewares that branch on 204-vs-200 (and anything asserting "no body" semantics for an empty-output operation) see a 200 carrying a serialized empty structure, so conformance tests, HTTP caches and generated clients that treat 2xx shapes distinctly disagree with the real service; an emulator user calibrating retry/assertion logic against 204 writes code that breaks on AWS or vice versa.
Fix: the TagResource operation's response status in the connectcases service crate's TagResource handler module — the operation's declared success status (and the output schema's http code) must be this operation's modeled/recorded 204 rather than the protocol default 200. Note the handler's own comment states that dropping the http code makes "a modelled 204 answer 200", which is exactly the observed behavior.
Related-but-not-covering: the open issue 20261022T144510Z-connectcases-tagresource-stub.md asks only that TagResource be implemented (it was a 501 scaffold); the handler is now implemented and the replay reports no stub for it, and that issue carries no wire replay key for this step, so it does not cover this status divergence.
Traces: connectcases-domain-lifecycle-and-tags#task#4 (TagResource); handler: services/service-com-amazonaws-connectcases-amazonconnectcases/src/ops/tag_resource.rs lines 79-94 (output SCHEMA with_http, code None) and line 159 (SUCCESS_STATUS = 200u16).

=== Cause 2 of 2: ListTemplates summaries omit the template identifier, its ARN and tagPropagationConfigurations ===
AWS: Smithy (connectcases) ListTemplates -> @http POST /domains/{domainId}/templates-list, output ListTemplatesResponse {templates (TemplateSummaryList, @required, @length max 100), nextToken}. TemplateSummary models templateId (@required, @length min 1 max 500), templateArn (@required, @length min 1 max 500), name (@required, @length 1..100, @pattern ^.*[\S]$), status (@required) and tagPropagationConfigurations (TagPropagationConfigurationList, @length max 1). So every listed element carries that template's identifier and its ARN, of the general form arn:aws:cases:<region>:<account-id>:domain/<domainId>/template/<templateId>, with the domain id and template id taken from the request and from state — never constants. Recorded behavior additionally shows a template with no tag-propagation configuration serializing tagPropagationConfigurations as an EMPTY LIST rather than omitting the member.
Emulator: answers 200 with the correct element count and correct name/status, but for the just-created template the identifier member is absent (None), the ARN member is absent (None) and tagPropagationConfigurations is absent (None).
Why: list-then-use is broken. Listing is the only way a client discovers a template it did not just create, and without an id or ARN in the summary it cannot go on to GetTemplate, UpdateTemplate, DeleteTemplate or CreateCase against an enumerated template; an SDK that enforces the @required members of TemplateSummary fails deserialization of the entire page rather than returning it.
Fix: the ListTemplates handler's TemplateSummary projection/serializer in the connectcases service crate — project the stored template's identifier and ARN into each summary element, and serialize an empty list for tagPropagationConfigurations instead of omitting the member.
Related-but-not-covering: many open issues describe this same symptom (e.g. 20261114T123453Z-connectcases-template-operations-diverge-listtemplates-summaries-omit-identity.md, 20261113T153114Z-connectcases-listtemplates-list-then-use-broken-template-summary-carries.md, 20261112T221816Z-connectcases-listtemplates-omits-identity-keys-and-null-tagpropagationconfigurations.md, 20261109T223410Z-connectcases-listtemplates-summaries-omit-the-template-id-arn.md). The replay reports this cluster as UNOWNED — no open issue carries its replay key for this fixture step — so it is filed here with the fixture binding attached.
Traces: connectcases-template-lifecycle#task#6 (ListTemplates).

## Evidence
Cause 1 — fixture connectcases-domain-lifecycle-and-tags, step task#4, TagResource:
  expected status 204, actual status 200
  (the tag write itself succeeds; only the success status line diverges. 204 carries no body, 200 carries a serialized empty output.)
  Model citation: Smithy TagResource {arn @httpLabel @required @length 1..500, tags @required} -> smithy.api#Unit, @http POST /tags/{arn} (code defaulted, so the 204 is recorded behavior), @idempotent.
  Code citation: services/service-com-amazonaws-connectcases-amazonconnectcases/src/ops/tag_resource.rs:82-91 (Schema::new_struct(...).with_http(HttpTrait::new("POST", "/tags/{arn}", None))) and :159 (const SUCCESS_STATUS: u16 = 200u16).

Cause 2 — fixture connectcases-template-lifecycle, step task#6, ListTemplates, expected 200 / actual 200:
  $.templates[name="<the created template>"].arn: missing (expected that template's ARN, of the form arn:aws:cases:<region>:<account-id>:domain/<domainId>/template/<templateId>, got None)
  $.templates[name="<the created template>"].id: missing (expected that template's identifier, got None)
  $.templates[name="<the created template>"].tagPropagationConfigurations: missing (expected [], got None)
  Model citation: Smithy ListTemplates -> ListTemplatesResponse {templates @required @length max 100} of TemplateSummary {templateId @required @length 1..500, templateArn @required @length 1..500, name @required, status @required, tagPropagationConfigurations @length max 1}.

## Steps to Reproduce
1. Boot the emulator from this checkout and replay the wire corpus (replay_wire_corpus) for trace set simparity-wire-aws-v1.
2. Cause 1: fixture connectcases-domain-lifecycle-and-tags, step task#4 — CreateDomain, then POST /tags/{domainArn} with a tag map. The emulator answers HTTP 200 with a serialized empty body; AWS answers HTTP 204 with no content. Verify the tags are still readable afterwards (they are — only the status diverges).
3. Cause 2: fixture connectcases-template-lifecycle, step task#6 — CreateDomain, CreateTemplate, then POST /domains/{domainId}/templates-list. Inspect the element of `templates` whose `name` is the created template: its identifier member, its ARN member and tagPropagationConfigurations are all absent, where AWS returns the template id, the template ARN and an empty list.
