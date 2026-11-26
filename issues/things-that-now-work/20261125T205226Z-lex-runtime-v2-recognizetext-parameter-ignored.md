Issue-Severity: MEDIUM
Issue-Gap-Class: parameter-ignored
Issue-Commit: 9d94d8e1cd3a917b1659a893a3ba20b52a695ddd

# A turn discards the caller's dialogAction.slotElicitationStyle (and subSlotToElicit), although the sibling PutSession door stores and reports them

_Filed: 2026-11-25T20:52:26Z_
_Operation: RecognizeText | Class: parameter-ignored_

> _Note: this issue was identified against the code base at commit `9d94d8e1cd3a917b1659a893a3ba20b52a695ddd`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

Both turn operations merge a caller-supplied `sessionState` into the stored session (`take_turn` -> `open_turn` -> `merge_session_state`), which stores the supplied `dialogAction` structure whole — `slotElicitationStyle` and `subSlotToElicit` included. The second half of the same turn then overwrites that member unconditionally with a DialogAction built from scratch inside `nlu`, and every DialogAction `nlu` builds hard-codes `slot_elicitation_style: None` and `sub_slot_to_elicit: None`. The two members the caller sent are therefore gone before the response is serialized: the turn's own `sessionState` reports them absent, and a later `GetSession` on the same session reports them absent too. `grep` over the whole crate shows `slot_elicitation_style` is never assigned anything but `None` in production code, and no test mentions the member at all — so a value a caller sends on a turn can never be observed again.

The crate is not missing the ability to carry the member: the SIBLING write door keeps it. `PutSession` stores the seeded dialog state through `Session::replace_session_state`, which copies the supplied state with `..supplied`, so a `PutSession` carrying `dialogAction: {"type": "ElicitSlot", "slotToElicit": "Flavor", "slotElicitationStyle": "SpellByLetter"}` is read back by `GetSession` WITH the style. One session field, two doors, two answers for the same documented member — and the crate even carries the member's modelled enum (`STYLE_TYPE_VALUES`) without ever producing one of its values.

The comment above the overwrite justifies replacing the dialog action's *decision* members ("A `Delegate` a request carried is an instruction ... so what the turn reports is the decision, not the instruction") — it says nothing about the elicitation style, which is not a decision the bot makes about the next step but a presentation setting the client asked for on the slot being elicited. Even on the branch where the turn's own decision IS `ElicitSlot` for the very slot the caller was styling, the style is dropped.

## Evidence

Smithy — `smithy_query(query="doc", shape="DialogAction", member="slotElicitationStyle")`:
  "Configures the slot to use spell-by-letter or spell-by-word style. When you use a style on a slot, users can spell out their input to make it clear to your bot. Spell by letter - \"b\" \"o\" \"b\" / Spell by word - \"b as in boy\" ..."
(and `DialogAction$subSlotToElicit`: "The name of the constituent sub slot of the composite slot specified in slotToElicit that should be elicited from the user.")

The turn path STORES the supplied dialog action — src/state.rs:711-713 (`merge_session_state`, reached from `open_turn` at src/state.rs:397-399, reached from `take_turn` at src/state.rs:486-490, called by src/ops/recognize_text.rs:280-289 and src/ops/recognize_utterance.rs:495-504):
  711: `if let Some(dialog_action) = supplied.dialog_action {`
  712: `    self.session_state.dialog_action = Some(dialog_action);`
  713: `}`

...and then throws it away — src/state.rs:424-429 (`record_turn`):
  424: `// "The next action that the bot should take in its interaction with the`
  428: `// reports is the decision, not the instruction.`
  429: `session.session_state.dialog_action = Some(plan.dialog_action);`
where every DialogAction the plan can carry hard-codes both members to `None` — src/state/nlu.rs:313-318:
  `DialogAction {`
  `    r#type: DIALOG_ACTION_ELICIT_SLOT.to_string(),`
  `    slot_to_elicit: Some(unfilled.name.clone()),`
  `    slot_elicitation_style: None,`
  `    sub_slot_to_elicit: None,`
  `},`
and src/state/nlu.rs:446-452 (`close()`): `slot_elicitation_style: None,` / `sub_slot_to_elicit: None,`.

The SIBLING door keeps the same member — src/state.rs:691-696 (`replace_session_state`, called by src/ops/put_session.rs:353 `session.replace_session_state(input.session_state, now, &locale);`):
  691: `let (supplied, interpretations) = locale.completed_session_state(supplied);`
  692: `self.session_state = SessionState {`
  693: `    active_contexts: None,`
  694: `    originating_request_id: None,`
  695: `    ..supplied`
  696: `};`
(`..supplied` carries `dialog_action` verbatim, style and sub-slot included, and src/ops/get_session.rs:205 reports it through `session.visible_session_state(now)`, which does not touch `dialog_action`.)

The member is never produced anywhere in this crate — `grep -rn 'slot_elicitation_style\|sub_slot_to_elicit' src/` returns only the struct definition (src/ops/types/shared_types.rs:143-144) and four `None` literals (src/state/nlu.rs:316,317,450,451; src/state/bot_definition.rs:216-217 and src/state/headers.rs:352-353 are a unit test and a doc fixture). The modelled enum the member targets is carried but unused — src/ops/types/shared_types.rs:229: `pub const STYLE_TYPE_VALUES: &[&str] = &["Default", "SpellByLetter", "SpellByWord"];`

Test coverage: NOTHING pins the drop — the same grep shows no `tests/` file mentions `slotElicitationStyle` or `subSlotToElicit`. The suites that drive dialog actions end to end (tests/all/session_lifecycle.rs:124 `a_session_is_created_read_advanced_and_deleted`, tests/all/bot_locale_dialog.rs, tests/all/dialog_turn_reporting.rs) only ever send `type`/`slotToElicit`.

## Steps to Reproduce

1. Build a bot locale through the Lex Models V2 control plane mounted on the same engine (as tests/all/harness.rs `built_bot` does), with an intent declaring a prompted required slot (say `Flavor`).
2. POST /bots/{botId}/botAliases/{botAliasId}/botLocales/en_US/sessions/style-1/text with body {"text": "hello", "sessionState": {"dialogAction": {"type": "ElicitSlot", "slotToElicit": "Flavor", "slotElicitationStyle": "SpellByLetter"}, "intent": {"name": "<the declared intent>"}}}.
3. Observe HTTP 200 whose `sessionState.dialogAction` carries `type`/`slotToElicit` but NO `slotElicitationStyle` — the member the request sent is gone (src/state/nlu.rs:316 writes `None` over it at src/state.rs:429).
4. GET the same session path: `sessionState.dialogAction` still has no `slotElicitationStyle`, so the value is unobservable anywhere after the turn.
5. Contrast the sibling door: POST /bots/{botId}/botAliases/{botAliasId}/botLocales/en_US/sessions/style-2 (PutSession) with {"sessionState": {"dialogAction": {"type": "ElicitSlot", "slotToElicit": "Flavor", "slotElicitationStyle": "SpellByLetter"}, "intent": {"name": "<the declared intent>"}}}, then GET that session — `sessionState.dialogAction.slotElicitationStyle` IS reported, because `replace_session_state` keeps the supplied dialog action (src/state.rs:695 `..supplied`).
6. Repeat step 2 through /utterance with the same structure in the gzip-then-base64 `x-amz-lex-session-state` header: same loss, since both encodings share `take_turn`.
