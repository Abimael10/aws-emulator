Issue-Source: trace-sweep-divergence
Issue-Trace: cloudwatchomni-omni-dashboard-update-and-validation
Issue-Step: task#6
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: cloudwatchomni-omni-dashboard-update-and-validation cloudwatchomni-view-update-and-conflict-errors
Issue-Shape-Id: com.amazonaws.cloudwatchomni#CloudWatchOmniFrontend
Issue-Key: cloudwatchomni|UpdateOmniDashboard|4xx->4xx|$.message
Issue-Key: cloudwatchomni|UpdateView|4xx->4xx|$.message
Issue-Commit: 13e968f2d712b86cbfec63dd795b2bc21b7f9b4c

# cloudwatchomni UpdateOmniDashboard and UpdateView return non-AWS error messages

> _Note: this issue was identified against the code base at commit `13e968f2d712b86cbfec63dd795b2bc21b7f9b4c`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
UpdateOmniDashboard

## Description
Replay of the wire corpus simparity-wire-aws-v1 shows the cloudwatchomni update operations (UpdateOmniDashboard, UpdateView) answering with the right HTTP status and the right error code but with the emulator's own prose in the `message` member. The shared fault is that these handlers build their error messages from a generic local template instead of the per-operation AWS text, and that the dashboard-body schema validator does not report every violation it must. The error message is a requirement, so each of these is a defect. This bundle has 3 causes; each must be closed before submitting.

=== Cause 1 of 3: UpdateOmniDashboard body validation omits the additionalProperties violation (fixture cloudwatchomni-omni-dashboard-update-and-validation, step task#6) ===
AWS: Smithy com.amazonaws.cloudwatchomni#CloudWatchOmniFrontend, UpdateOmniDashboard (input spaceId required pattern ^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$, dashboardId required @length 36..36 same pattern, body @length 1..1048576) models ValidationException with @httpError 400. The dashboard body's JSON schema is not in the model, so the schema rules are recorded behavior only. Recorded behavior: a body whose panel object is missing required keys AND carries an unknown key is refused 400 ValidationException reporting EVERY schema violation, joined by "; " in schema order: "panels[0].type: does not satisfy the 'required' constraint of the dashboard body schema; panels[0].layout: does not satisfy the 'required' constraint of the dashboard body schema; panels[0]: does not satisfy the 'additionalProperties' constraint of the dashboard body schema". The fixed text of each clause is "<pointer>: does not satisfy the '<constraint>' constraint of the dashboard body schema"; the JSON pointer (panels[0], panels[0].type) and the constraint name are derived from the submitted body and the schema, not literals.
Emulator: status 400 and code ValidationException agree, but the message carries only the two 'required' clauses and drops the 'additionalProperties' clause entirely: "panels[0].type: does not satisfy the 'required' constraint of the dashboard body schema; panels[0].layout: does not satisfy the 'required' constraint of the dashboard body schema". Note the emulator DOES emit an additionalProperties clause at the document root on CreateOmniDashboard (recorded elsewhere in this corpus as "panels: does not satisfy the 'required' constraint ...; $: does not satisfy the 'additionalProperties' constraint of the dashboard body schema"), so the gap is specifically that the panel object is not validated with additionalProperties: false.
Why: a client relying on AWS's behavior reads the violation list to repair its dashboard JSON. Against the emulator an unknown or misspelled panel key passes validation silently, so an author iterating locally removes the reported 'required' problems, gets an accepted body, and is then refused by real AWS for a key it was never told about; tooling that annotates JSON pointers from the message loses that annotation.
Fix: the UpdateOmniDashboard handler's dashboard-body schema validation (shared with CreateOmniDashboard) — enforce the panel object's additionalProperties rule and append its violation to the same "; "-joined list in schema order.
Related but NOT covering this step: open behavioral issue 20261116T034449Z-cloudwatchomni-createspace-seeds-no-service-managed-access-profile.md describes the same wording gap as its cause 2, but it carries no replay key for this wire step, so this step is not re-replayed against it; closing one should close both. Open issue 20261111T022337Z-cloudwatchomni-updateomnidashboard-stub.md is stale for this: UpdateOmniDashboard now answers and validates.

=== Cause 2 of 3: UpdateOmniDashboard dashboard-not-found message is the emulator's generic template (fixture cloudwatchomni-omni-dashboard-update-and-validation, step task#8) ===
AWS: Smithy UpdateOmniDashboard models ResourceNotFoundException with @httpError 404 (required member `message`, doc "The specified resource does not exist."). The model does not fix the text, so the wording is recorded behavior only: updating a dashboardId that does not exist is refused 404 ResourceNotFoundException with message template "Dashboard not found: <dashboardId>" — fixed text "Dashboard not found: " with a single variable, the dashboardId taken verbatim from the request, and NO space id and NO trailing period.
Emulator: status 404 and code ResourceNotFoundException agree; the message is "Dashboard '<dashboardId>' does not exist in space '<spaceId>'." — wrong fixed text, the id quoted rather than appended after a colon, and an extra "in space '<spaceId>'" clause plus a trailing period that AWS does not emit. (The recorded ids in this step are per-run values and are not part of the requirement.)
Why: the message is the only discriminator a caller has between a missing dashboard and any other 404 on the operation, so callers that match AWS's "Dashboard not found: <id>" prefix never match, surface a generic failure, and lose the id they parse out of the text; the emulator additionally discloses the space id in an error where AWS names only the dashboard.
Fix: the UpdateOmniDashboard handler's not-found error construction — emit the dashboard-specific template rather than the service-wide "<Resource> '<id>' does not exist in space '<spaceId>'." formatter.
Related but NOT covering this step: open issue 20261109T172919Z-cloudwatchomni-resourcenotfoundexception-messages-do-not-match-aws-wording.md files the same generic-template fault for GetDomain, CreateSpace, GetAccessGrant, GetView and GetAlert only; it carries no key for UpdateOmniDashboard, so this step would not be re-replayed against it.

=== Cause 3 of 3: UpdateView view-not-found message is the emulator's generic template (fixture cloudwatchomni-view-update-and-conflict-errors, step task#6) ===
AWS: Smithy UpdateView (input name required, target ViewName @length 6..256 pattern ^view\\.[a-z0-9][a-z0-9_-]{0,250}$) models ResourceNotFoundException with @httpError 404. Wording is recorded behavior only: updating a view that does not exist is refused 404 ResourceNotFoundException with message template "View not found: <name>" — fixed text "View not found: ", single variable the view name taken verbatim from the request, no quoting and no trailing period.
Emulator: status 404 and code ResourceNotFoundException agree; the message is "View '<name>' does not exist." — wrong fixed text, the name quoted, and a trailing period AWS does not emit.
Why: as cause 2 — a caller matching AWS's "View not found: " prefix to decide whether to create the view instead of updating it never matches and treats the refusal as an unclassified 404, so create-or-update logic stalls.
Fix: the UpdateView handler's not-found error construction — use the same view-not-found template GetView must use, from one shared formatter.
Related but NOT covering this step: 20261109T172919Z-cloudwatchomni-resourcenotfoundexception-messages-do-not-match-aws-wording.md covers GetView's copy of this template, and 20261111T063558Z-cloudwatchomni-error-messages-diverge-from-aws-wording-for.md covers DeleteView's; neither carries a key for UpdateView. Open issue 20261111T022337Z-cloudwatchomni-updateview-stub.md is stale: UpdateView now answers.

Traces: cloudwatchomni-omni-dashboard-update-and-validation#task#6 (cause 1), cloudwatchomni-omni-dashboard-update-and-validation#task#8 (cause 2), cloudwatchomni-view-update-and-conflict-errors#task#6 (cause 3).

## Evidence
Corpus simparity-wire-aws-v1, emulator up (/__admin/health 200), 120 steps replayed, score 32.5%.

Cause 1 — UpdateOmniDashboard, fixture cloudwatchomni-omni-dashboard-update-and-validation step task#6, status 400 expected and 400 actual, code ValidationException both sides:
  expected $.message: "panels[0].type: does not satisfy the 'required' constraint of the dashboard body schema; panels[0].layout: does not satisfy the 'required' constraint of the dashboard body schema; panels[0]: does not satisfy the 'additionalProperties' constraint of the dashboard body schema"
  actual   $.message: "panels[0].type: does not satisfy the 'required' constraint of the dashboard body schema; panels[0].layout: does not satisfy the 'required' constraint of the dashboard body schema"
  The third clause (the panel object's additionalProperties violation) is missing. Per-clause fixed text: "<pointer>: does not satisfy the '<constraint>' constraint of the dashboard body schema"; pointer and constraint name are derived from the submitted body.

Cause 2 — UpdateOmniDashboard, fixture cloudwatchomni-omni-dashboard-update-and-validation step task#8, status 404 both sides:
  expected $.message: 'Dashboard not found: 00000000-0000-4000-8000-000000000000'
  actual   $.message: "Dashboard '00000000-0000-4000-8000-000000000000' does not exist in space '08efb4f1-d637-4c5b-9702-067e5b347e0e'."
  Fixed text is 'Dashboard not found: '; the dashboard id is the request variable. The space id, the quoting and the trailing period in the actual message are not part of AWS's template; the literal ids shown are per-run values.

Cause 3 — UpdateView, fixture cloudwatchomni-view-update-and-conflict-errors step task#6, status 404 both sides:
  expected $.message: 'View not found: view.sim2realprobecwomissingb28fbec6'
  actual   $.message: "View 'view.sim2realprobecwomissingb28fbec6' does not exist."
  Fixed text is 'View not found: '; the view name is the request variable (the recorded name is a per-run value).

Smithy (services/service-com-amazonaws-cloudwatchomni-cloudwatchomnifrontend/smithy-model/com-amazonaws-cloudwatchomni-cloudwatchomnifrontend.json): UpdateOmniDashboard errors include ValidationException @httpError 400 and ResourceNotFoundException @httpError 404; body @length 1..1048576. UpdateView input name target ViewName @length 6..256 pattern ^view\\.[a-z0-9][a-z0-9_-]{0,250}$, errors include ResourceNotFoundException @httpError 404. ResourceNotFoundException has a required `message` member; the model does not fix the message text, so the templates above are recorded behavior only.

## Steps to Reproduce
1. Clone the repo and boot the emulator built from that checkout.
2. Replay the wire corpus simparity-wire-aws-v1 (replay_wire_corpus).
3. Cause 1: inspect fixture cloudwatchomni-omni-dashboard-update-and-validation step task#6 — create a domain, a space and a dashboard, then call UpdateOmniDashboard(spaceId, dashboardId, body='{"panels": [{"x": 1}]}'). Observe 400 ValidationException whose message lists only the two 'required' violations and omits the clause "panels[0]: does not satisfy the 'additionalProperties' constraint of the dashboard body schema" for the unknown panel key.
4. Cause 2: inspect step task#8 of the same fixture — call UpdateOmniDashboard with a well-formed body and a dashboardId that does not exist in the space. Observe 404 ResourceNotFoundException with "Dashboard '<dashboardId>' does not exist in space '<spaceId>'." instead of "Dashboard not found: <dashboardId>".
5. Cause 3: inspect fixture cloudwatchomni-view-update-and-conflict-errors step task#6 — call UpdateView(name='view.<nonexistent>', definition=<valid SELECT>). Observe 404 ResourceNotFoundException with "View '<name>' does not exist." instead of "View not found: <name>".
6. All three reproduce for any request in the same state; only the ids and names in the message vary.
