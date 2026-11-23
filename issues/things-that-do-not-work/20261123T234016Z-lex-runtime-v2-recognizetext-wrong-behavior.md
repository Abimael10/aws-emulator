Issue-Severity: MEDIUM
Issue-Gap-Class: wrong-behavior
Issue-Commit: 144bcacdff36cb6d5f74daf1c12856f62206ef70

# A slot carrying only `shape` is reported as a non-null object although the dialog treats it as unfilled, so `Intent$slots` loses the documented `null`

_Filed: 2026-11-23T23:40:16Z_
_Operation: RecognizeText | Class: wrong-behavior_

> _Note: this issue was identified against the code base at commit `144bcacdff36cb6d5f74daf1c12856f62206ef70`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`Intent$slots` is modelled so that an unfilled slot of the recognized intent is reported as JSON `null`; this crate hand-writes the `Intent` codec specifically to be able to write that `null` (see the module doc of `intent_types.rs`). The predicate that decides it, `is_unfilled`, requires FOUR members to be absent — `value`, `values`, `subSlots` and `shape` — while its own doc sentence names only the first three, and while the dialog's own "is this slot filled?" predicate, `nlu::has_value`, looks only at `value`/`values`.

The two predicates therefore disagree for a model-valid input a caller can send: a slot entry that carries only `shape` (e.g. `{"shape": "Scalar"}` — `Slot` has no required members and `Shape` admits `Scalar`). `nlu::has_value` says that slot has no value, so the same turn answers `dialogAction.type = ElicitSlot` for it and plays its prompt; but `declared_slots` keeps the caller's entry verbatim (`filled.get(&slot.name).cloned()`) and `is_unfilled` then returns false because `shape` is present, so the serializer writes `{"shape":"Scalar"}` instead of `null`. One response thus says "this slot is still being elicited" and "this slot is not null" at the same time, which is exactly the distinction the crate says the hand-written codec exists to preserve.

The same entry is persisted, so a later `GetSession` reports it the same way, and `PutSession` reaches the identical path through `completed_session_state` -> `declared_slots`.

## Evidence

Smithy, `smithy_query(query="doc", shape="Intent", member="slots")`: "A map of all of the slots for the intent. The name of the slot maps to the value of the slot. If a slot has not been filled, the value is null."
Smithy, `smithy_query(query="shape", shape="Slot,Shape")`: `Slot` declares `value`, `shape`, `values`, `subSlots` with NO required member, and `Shape` = ["Scalar", "List", "Composite"] — so `{"shape":"Scalar"}` with no value is valid input.

The crate states the rule it then breaks — src/ops/types/intent_types.rs:9-12: "a slot the intent declares and the conversation has no value for would travel as an empty object `{}` — and a client could not tell \"declared and unfilled\" from \"filled with nothing\", which is exactly the distinction the documented `null` draws."

The predicate's doc names three members, the code tests four — src/ops/types/intent_types.rs:159-170:
  "/// Has this slot no value of any kind — neither the single `value` a `Scalar`
   /// slot holds, nor the `values` of a `List` one, nor the `subSlots` of a
   /// composite?"
  `pub fn is_unfilled(slot: &Slot) -> bool { slot.value.is_none() && slot.values.is_none() && slot.sub_slots.is_none() && slot.shape.is_none() }`

The sibling predicate disagrees — src/state/nlu.rs:470-477:
  "/// Does the session hold a value for that slot? A slot the map names without a
   /// value is not filled: \"if a slot has not been filled, the value is null\"
   /// (`Intent$slots`)."
  `fn has_value(slots: &HashMap<String, Slot>, name: &str) -> bool { slots.get(name).is_some_and(|slot| slot.value.is_some() || slot.values.is_some()) }`

The caller's entry is kept rather than normalised — src/state/nlu.rs:432: `let value = filled.get(&slot.name).cloned().unwrap_or_else(unfilled);`
And the write — src/ops/types/intent_types.rs:184-188: `if is_unfilled(slot) { entry.write_null(&Self::M_SLOTS)? } else { entry.write_struct(&Self::M_SLOTS, slot)? }`

Test coverage: tests/all/declared_slot_map.rs and tests/all/always_reported_members.rs exercise the declared-slot map only for slots that are absent from the request or carry a `value`; no test sends a slot entry carrying only `shape`, so nothing pins the current behaviour.

## Steps to Reproduce

1. Build a bot locale through the Lex Models V2 control plane with one intent (e.g. OrderFlowers) that declares a REQUIRED, prompted slot (e.g. FlowerType), as tests/all/harness.rs `built_bot` does.
2. POST /bots/{botId}/botAliases/{botAliasId}/botLocales/en_US/sessions/s1/text with body {"text": "<an utterance that names the intent>", "sessionState": {"intent": {"name": "OrderFlowers", "slots": {"FlowerType": {"shape": "Scalar"}}}}}.
3. Observe the 200 response: `sessionState.dialogAction` is {"type":"ElicitSlot","slotToElicit":"FlowerType"} and the slot's prompt is in `messages` — i.e. `nlu::has_value` treated FlowerType as unfilled.
4. Observe in the SAME response that `sessionState.intent.slots.FlowerType` is the object {"shape":"Scalar"} rather than `null`, although `Intent$slots` says "If a slot has not been filled, the value is null".
5. GET the same session path: `GetSession` reports the same non-null entry, so the state persisted carries it too.
