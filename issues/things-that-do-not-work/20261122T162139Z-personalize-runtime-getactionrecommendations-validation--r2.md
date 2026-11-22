Issue-Severity: MEDIUM
Issue-Gap-Class: validation
Issue-Commit: 80ac8c3d781ff9dbf20b11ba57ca2512b801c7de

# The filter parser accepts a $placeholder name no filterValues key can carry, so the expression is stored and fails at inference time instead of at creation

_Filed: 2026-11-22T16:21:39Z_
_Operation: GetActionRecommendations | Class: validation_

> _Note: this issue was identified against the code base at commit `80ac8c3d781ff9dbf20b11ba57ca2512b801c7de`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`filter::parse` admits ANY non-empty word after `$` as a parameter name, including names containing `-`, `+`, `.` and `:` (every one of those characters is in `reads()`, the set a bare word is spelled with). The model, however, ties a parameter name to a `filterValues` MAP KEY — `GetActionRecommendationsRequest$filterValues`: "For each placeholder parameter in your filter expression, provide the parameter name (in matching case) as a key" — and that key's shape `FilterAttributeName` carries `@pattern ^[A-Za-z0-9_]+$`, which the boundary enforces before any handler runs (the premise the crate already relies on in `src/service.rs:93-103`, which rewords exactly that boundary pattern violation).

So an expression such as `INCLUDE ActionID WHERE Actions.CHANNEL IN ($MY-CHANNEL)` (or `$A.B`) names a parameter NO request can ever bind: `resolve` looks the name up in `filterValues` (`filter_values?.get(name)?`) and the only key that could satisfy it is one the modelled pattern refuses. The consequences are both at inference time, where this crate says they can never be:

* an INCLUDE expression: `unbound_parameter` refuses every request naming the filter with 400 `filterValues provides no value for the filter expression parameter $MY-CHANNEL, which the INCLUDE expression requires` — a refusal the caller can never satisfy, because supplying that key is itself refused at the boundary. The filter is permanently unusable on all three operations and on a promotion's own `filterArn`.
* an EXCLUDE expression: the clause is dropped for every request, so the filter is a permanent no-op.

The check belongs where the crate says every other unusable expression is caught — `parse`, which `register_filter` (and the sibling control plane's `CreateFilter`) runs when the filter is CREATED. `parse`'s own doc claims it already does this ("EVERY element of the grammar is checked here"), and it does check the subject identifier and the clause dataset names against the fixed sets the language allows; the parameter-name slot is the one operand it leaves unchecked against what a request can supply.

## Evidence

The parser accepts any non-empty name after `$` — `src/state/filter.rs:470-483`:
```
    /// One operand: `$parameter`, a quoted literal, or a bare literal.
    fn operand(&mut self) -> Result<Operand, String> {
        match self.next() {
            Some(Token::Quoted(literal)) => Ok(Operand::Literal(literal)),
            Some(Token::Word(word)) => match word.strip_prefix('$') {
                Some(name) if !name.is_empty() => Ok(Operand::Parameter(name.to_string())),
```
and a bare word may carry characters the key pattern forbids — `src/state/filter.rs:339-341`:
```
fn reads(ch: char) -> bool {
    ch.is_alphanumeric() || matches!(ch, '_' | '.' | '$' | '-' | '+' | ':')
}
```

What the model says a parameter name is — `smithy_query(query="shape", shape="FilterValues")`:
```
"FilterValues": {"type": "map", "length": {"min": 0, "max": 25},
  "key": {"target": "FilterAttributeName", "length": {"min": 0, "max": 50}, "pattern": "^[A-Za-z0-9_]+$"},
  "value": {"target": "FilterAttributeValue", "length": {"min": 0, "max": 1000}}}
```
and that the key IS the parameter name — `GetActionRecommendationsRequest$filterValues` doc, mirrored in the handler at `src/ops/get_action_recommendations.rs:61-64`:
```
//! * `filterValues` (FilterValues, len 0..25) — The values to use when filtering
//!   recommendations. For each placeholder parameter in your filter expression, provide the
//!   parameter name (in matching case) as a key and the filter value(s) as the corresponding
//!   value.
```

The binding path that can therefore never succeed — `src/state/filter.rs:963-972`:
```
fn resolve(operand: &Operand, filter_values: Option<&HashMap<String, String>>) -> Option<Vec<String>> {
    match operand {
        Operand::Literal(literal) => Some(vec![literal.clone()]),
        Operand::Parameter(name) => {
            let raw = filter_values?.get(name)?;
```
and the inference-time refusal it produces — `src/state/filter.rs:944-952`:
```
fn unbound_parameter(kind: FilterKind, name: &str) -> Result<(), String> {
    if kind == FilterKind::Include {
        return Err(format!(
            "filterValues provides no value for the filter expression parameter \
             ${name}, which the INCLUDE expression requires"
        ));
    }
    Ok(())
}
```
reached from this handler at `src/ops/get_action_recommendations.rs:248-254` (and identically `src/ops/get_personalized_ranking.rs:274-280`, `src/ops/get_recommendations.rs:327-333` and the promotion's filter at `src/ops/get_recommendations.rs:622-631`):
```
        let request_filter = guard
            .bind_filter(
                registry::present(input.filter_arn.as_deref()),
                Entity::Actions,
                input.filter_values.as_ref(),
            )
            .map_err(deny)?;
```

The crate's own rule, which this contradicts — `src/state.rs:1029-1034`:
```
/// Register a filter, validating its expression the way `CreateFilter` does.
///
/// `Err` carries the parse error, so an expression the runtime could not apply is
/// refused where the filter is created — never at inference time, and never by
/// quietly filtering nothing.
```
restated in the module header — `src/state.rs:63-66`:
```
//! * A filter's expression is validated when the filter is REGISTERED, exactly as
//!   `CreateFilter` validates it, so a stored filter is always one the runtime can
//!   apply.
```
and claimed by `parse` itself — `src/state/filter.rs:646-653`:
```
/// `Err` carries a message naming what was wrong, for the caller that registered the
/// filter: the real service validates the expression when the filter is CREATED, so a
/// stored filter is always one this evaluator can apply. EVERY element of the grammar
/// is checked here, the subject included: the slot admits the two identifiers the
/// language defines and nothing else, so a stored expression always names the entity it
/// selects.
```
while `referenced_parameters` states what those names are FOR — `src/state/filter.rs:285-289`:
```
    /// Every `$placeholder` the expression references, sorted and de-duplicated.
    ///
    /// This is what `filterValues` has to bind for an `INCLUDE` expression
    /// ("you must provide values for all parameters that are defined in the
    /// expression").
```

That the boundary really refuses such a key (so the request can never bind the parameter) is the same premise the crate acts on in `src/service.rs:93-103`, whose `clause()` rewords a `ViolationKind::Pattern` violation on `filterValues`'s key shape.

Test coverage: `tests/all/unbound_filter_values.rs` drives every form in which a request names no value for a parameter (`:188-210`, rows at `:211-480`) and `tests/all/filter_value_quoting.rs` drives the value forms, but every parameter name used in the suite is `[A-Za-z0-9_]+` — a grep for a placeholder containing `-`, `.`, `:` or `+` across `tests/` returns no match — so nothing pins what `parse` does with a name no `filterValues` key can carry.

## Steps to Reproduce

1. Build an emulator carrying this service's state (as `tests/all/action_filter_value_forms.rs` does) and register a PERSONALIZED_ACTIONS campaign with `registry::register_campaign(engine, Campaign::new_actions("arn:aws:personalize:us-east-1:000000000000:campaign/actions", "actions", actions(8)))`.
2. Register a filter whose expression names a hyphenated placeholder: `registry::register_filter(engine, "arn:aws:personalize:us-east-1:000000000000:filter/bad-param", "INCLUDE ActionID WHERE Actions.CHANNEL IN ($MY-CHANNEL)")`. It answers `Ok(true)` — `parse` accepts the name (src/state/filter.rs:473-474 with `reads` at :339-341) even though no `filterValues` key can ever equal `MY-CHANNEL`. (Over the wire the same expression reaches this seam through the sibling control plane's `CreateFilter`.)
3. POST /action-recommendations with that `campaignArn`, `userId: "u1"`, `numResults: 3`, `filterArn` of the filter above and `filterValues: {"MY-CHANNEL": "\"App\""}`: the boundary refuses the request 400 `ValidationException` because the map key violates `FilterAttributeName`'s `@pattern ^[A-Za-z0-9_]+$` (rendered by src/service.rs:93-103).
4. Retry with any key the pattern DOES admit (e.g. `MY_CHANNEL`), or with no `filterValues` at all: the handler answers 400 `InvalidInputException` `filterValues provides no value for the filter expression parameter $MY-CHANNEL, which the INCLUDE expression requires` (src/state/filter.rs:944-952 via src/ops/get_action_recommendations.rs:248-254). No request can ever make this stored filter usable.
5. Repeat steps 2-4 with `EXCLUDE ActionID WHERE Actions.CHANNEL IN ($MY-CHANNEL)`: every call is served 200 with the clause dropped, so the filter removes nothing for the rest of its life — the "quietly filtering nothing" outcome `src/state.rs:1029-1034` says a stored filter can never produce.
