Issue-Source: trace-sweep-divergence
Issue-Trace: bcm-recommended-actions-error-invalid-filter-key
Issue-Step: task#1
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: bcm-recommended-actions-error-invalid-filter-key bcm-recommended-actions-error-invalid-match-option
Issue-Shape-Id: com.amazonaws.bcmrecommendedactions#AWSBillingAndCostManagementRecommendedActions
Issue-Key: bcm-recommended-actions|ListRecommendedActions|4xx->4xx|$.fieldList[name="filter.actions.1.member.key"].message
Issue-Key: bcm-recommended-actions|ListRecommendedActions|4xx->4xx|$.fieldList[name="filter.actions.1.member.matchOption"].message
Issue-Commit: 7098827f7d7522d4a6107e35de6129ed6df1e0dd

# ListRecommendedActions echoes the rejected enum value instead of redacting it in ValidationException fieldList messages

> _Note: this issue was identified against the code base at commit `7098827f7d7522d4a6107e35de6129ed6df1e0dd`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ListRecommendedActions

## Description
When an ActionFilter member carries a value outside its modelled enum, the emulator returns the right HTTP status (400), the right error shape (ValidationException with reason/fieldList) and the right fieldList entry name, but it builds the per-field `message` by interpolating the caller-supplied value verbatim in single quotes. AWS does not disclose the rejected value in this message: it emits the fixed token `[redacted]` in that position and discloses only the permitted enum value set. Two causes, one per enum-typed member of ActionFilter, bundled because they share one service, one operation and one message-construction defect.

Cause 1 of 2: filter.actions.N.member.key — the rejected FilterName value is echoed instead of redacted.
  AWS: Smithy: ListRecommendedActions input `filter` -> RequestFilter.actions -> ActionFilterList -> ActionFilter.key is @required, targets enum FilterName with values [FEATURE, SEVERITY, TYPE]; the modelled failure is ValidationException (errorKind client, httpError 400, awsQueryError code BCMRecommendedActionsValidation) whose members are message, reason (ValidationExceptionReason: unknownOperation|cannotParse|fieldValidationFailed|other) and fieldList (ValidationExceptionField{name, message}, documented as "Provides the name of the field that failed validation" / "Provides a message explaining why the field failed validation"). The model does NOT specify the message text, so the wording is recorded behavior only: the recorded 400 response carries a fieldList entry whose `name` is the JSON path of the offending member (`filter.actions.1.member.key`) and whose `message` is `Value [redacted] failed to satisfy constraint: Member must satisfy enum value set: [FEATURE, TYPE, SEVERITY]`. Fixed text of that message: the `Value ` prefix, the literal token `[redacted]` standing in for the rejected value, and the ` failed to satisfy constraint: Member must satisfy enum value set: [` / `]` frame. The only variable part is the permitted enum value set, taken from the targeted enum shape; the rejected value itself is deliberately NOT interpolated. The `name` is likewise variable, derived from the request's own structure (the `actions` index of the offending element).
  Emulator: same status 400 and same fieldList entry name, but `message` is `Value 'NOT_A_REAL_FILTER_KEY' failed to satisfy constraint: Member must satisfy enum value set: [FEATURE, TYPE, SEVERITY]` — the request's own rejected string, single-quoted, in the slot where AWS puts `[redacted]`. (That literal is this recording's input value, not part of the requirement; the defect is that ANY caller-supplied value appears there at all.)
  Why: the per-field message is the part of a ValidationException clients surface to humans and log. A client relying on AWS's behavior gets a message whose rejected-value slot is a constant and therefore safe to log and safe to assert on; against the emulator the slot contains arbitrary caller input, so (a) exact-match assertions on the validation message fail, (b) the emulator reflects unvalidated request data straight back into an error body and into logs that AWS deliberately keeps free of it, and (c) any test that pins this message passes on the emulator and fails on AWS or vice versa.
  Fix: the ListRecommendedActions input-validation step that constructs ValidationException.fieldList — the enum-constraint branch must emit the fixed `[redacted]` token in place of the offending value, and must keep deriving only the enum value set from the model. Code: services/service-com-amazonaws-bcmrecommendedactions-awsbillingandcostmanagementrecommendedactions/src/ — the operation's validation/error-construction path.
  Traces: bcm-recommended-actions-error-invalid-filter-key#task#1.

Cause 2 of 2: filter.actions.N.member.matchOption — the rejected MatchOption value is echoed instead of redacted.
  AWS: Smithy: ActionFilter.matchOption is @required and targets enum MatchOption with values [EQUALS, NOT_EQUALS] (documented "Use EQUALS to include matching results or NOT_EQUALS to exclude matching results"); same modelled ValidationException (400). Message text is recorded behavior only: the recorded fieldList entry has `name` = `filter.actions.1.member.matchOption` and `message` = `Value [redacted] failed to satisfy constraint: Member must satisfy enum value set: [NOT_EQUALS, EQUALS]`. Same fixed/variable split as cause 1: `Value `, `[redacted]`, ` failed to satisfy constraint: Member must satisfy enum value set: [...]` are fixed; the enum value set is taken from the MatchOption shape and the field name from the request's structure.
  Emulator: status 400 and the fieldList name are correct, but `message` is `Value 'NOT_A_REAL_MATCH_OPTION' failed to satisfy constraint: Member must satisfy enum value set: [NOT_EQUALS, EQUALS]` — again the caller-supplied value in the redacted slot.
  Why: identical client impact to cause 1, on the sibling member — which is what shows this is one shared construction defect rather than a per-member typo: every enum-constrained member of this input will leak its rejected value.
  Fix: same component as cause 1; fixing the shared enum-constraint message builder closes both members (and any other enum member of this input).
  Traces: bcm-recommended-actions-error-invalid-match-option#task#1.

Related but NOT covering this: the open issue 20261029T214023Z-bcm-recommended-actions-listrecommendedactions-never-authorizes-the-caller-so-a-principal.md is about this operation never evaluating IAM authorization (AccessDeniedException unreached). It is a different root cause in a different component and does not touch ValidationException message construction.

## Evidence
Wire replay of trace set simparity-wire-aws-v1 against the emulator booted from this checkout: 6 steps, 4 passed, 2 mismatched (score 66.7%), emulator_up=true, bootstrap_signal=false, no blocked/errored steps and no stub_path or owned_by on either cluster.

Cause 1 — fixture bcm-recommended-actions-error-invalid-filter-key, step task#1 [verdict: mismatch], service bcm-recommended-actions, operation ListRecommendedActions:
  Expected status 400, actual status 400 — status, error shape and fieldList entry name all agree. Diverging key: $.fieldList[name="filter.actions.1.member.key"].message (value_mismatch).
  Expected message: 'Value [redacted] failed to satisfy constraint: Member must satisfy enum value set: [FEATURE, TYPE, SEVERITY]'
  Actual   message: "Value 'NOT_A_REAL_FILTER_KEY' failed to satisfy constraint: Member must satisfy enum value set: [FEATURE, TYPE, SEVERITY]"
  Fixed text: "Value " + "[redacted]" + " failed to satisfy constraint: Member must satisfy enum value set: [" ... "]". Variable, from the model: the enum value set (FilterName = FEATURE, SEVERITY, TYPE). Variable, from the request: the fieldList entry's `name` (the path of the offending element, here index 1 of `actions`). NOT variable, and this is the defect: the rejected value slot — AWS holds the constant `[redacted]` there; the emulator substitutes the request's value ('NOT_A_REAL_FILTER_KEY' in this recording).
  divergence_key: service=bcm-recommended-actions|ListRecommendedActions|status=400->400|keys=$.fieldList[name="filter.actions.1.member.key"].message

Cause 2 — fixture bcm-recommended-actions-error-invalid-match-option, step task#1 [verdict: mismatch], same service and operation:
  Expected status 400, actual status 400. Diverging key: $.fieldList[name="filter.actions.1.member.matchOption"].message (value_mismatch).
  Expected message: 'Value [redacted] failed to satisfy constraint: Member must satisfy enum value set: [NOT_EQUALS, EQUALS]'
  Actual   message: "Value 'NOT_A_REAL_MATCH_OPTION' failed to satisfy constraint: Member must satisfy enum value set: [NOT_EQUALS, EQUALS]"
  Same fixed/variable split; the enum value set (MatchOption = EQUALS, NOT_EQUALS) is rendered identically by both sides, so the message differs ONLY in the redacted-value slot.
  divergence_key: service=bcm-recommended-actions|ListRecommendedActions|status=400->400|keys=$.fieldList[name="filter.actions.1.member.matchOption"].message

Model corroboration (smithy_query on services/service-com-amazonaws-bcmrecommendedactions-awsbillingandcostmanagementrecommendedactions/smithy-model/com-amazonaws-bcmrecommendedactions-awsbillingandcostmanagementrecommendedactions.json): ActionFilter.key -> FilterName @enum [FEATURE, SEVERITY, TYPE] @required; ActionFilter.matchOption -> MatchOption @enum [EQUALS, NOT_EQUALS] @required; ListRecommendedActions errors include ValidationException (client, 400) with required members message and reason plus optional fieldList of ValidationExceptionField{name, message}. Neither enum nor either member is marked @sensitive, so the redaction is a property of this service's validation-message construction, not of a shape trait — hence "recorded behavior only" for the wording.

## Steps to Reproduce
1. Clone this checkout and boot the emulator from it (the replay harness reports /__admin/health 200 and an accepting data listener).
2. Replay the wire corpus simparity-wire-aws-v1. Steps bcm-recommended-actions-error-invalid-filter-key#task#1 and bcm-recommended-actions-error-invalid-match-option#task#1 both report verdict=mismatch at expected_status 400 / actual_status 400.
3. Cause 1 by hand: call ListRecommendedActions with filter.actions = [ {key: <any string not in [FEATURE, SEVERITY, TYPE]>, matchOption: "EQUALS", values: ["x"]} ]. The emulator answers 400 ValidationException whose fieldList entry for name "filter.actions.1.member.key" has message "Value '<the string you sent>' failed to satisfy constraint: Member must satisfy enum value set: [FEATURE, TYPE, SEVERITY]". AWS answers the same status and the same field name with "Value [redacted] failed to satisfy constraint: Member must satisfy enum value set: [FEATURE, TYPE, SEVERITY]".
4. Cause 2 by hand: call ListRecommendedActions with filter.actions = [ {key: "FEATURE", matchOption: <any string not in [EQUALS, NOT_EQUALS]>, values: ["x"]} ]. The emulator's fieldList entry for name "filter.actions.1.member.matchOption" carries the string you sent, single-quoted; AWS carries the literal [redacted].
5. Vary the value you send in either call: the emulator's message changes with it, which is exactly the behavior that must not happen — the slot is a constant on AWS.
6. After fixing, re-replay BOTH fixtures above; cause 2 is not exercised by cause 1's fixture.
