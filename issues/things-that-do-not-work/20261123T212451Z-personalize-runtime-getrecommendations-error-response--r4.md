Issue-Severity: MEDIUM
Issue-Gap-Class: error-response
Issue-Commit: 724276c7155ec07ee34cf1e38d33e217c3831042

# A filterValues value the model marks @sensitive is quoted back verbatim in the unorderable-comparison refusal, which the crate's own rule forbids

_Filed: 2026-11-23T21:24:51Z_
_Operation: GetRecommendations | Class: error-response_

> _Note: this issue was identified against the code base at commit `724276c7155ec07ee34cf1e38d33e217c3831042`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

This crate states one rule for the value a refusal may carry: it withholds it, in EVERY clause, explicitly including the case where the model marks the member `@sensitive`. `src/service.rs` applies that to every boundary refusal (`|_| true` for every member path), and `tests/all/refusal_wording.rs` pins it as an absolute for exactly the `filterValues` map value: "a sensitive value must never be quoted back".

One handler-reachable path breaks it. When a request binds a `$placeholder` of a stored filter expression to a value that names no number and the clause carries an ORDERING operator (`<`, `<=`, `>`, `>=`), `filter::bound_comparison` refuses the request through `filter::unorderable`, whose message interpolates the caller-supplied value verbatim: `filterValues binds the filter expression parameter $YEAR to the non-numeric value '<value>', which the Items.YEAR >= comparison cannot apply`. That string is lifted into `Denied::invalid` by `State::bind_filter` and answered to the client as the `InvalidInputException` (400) message by each handler's `deny`, so the value reaches the response body (and every client log of it).

The map value is `FilterAttributeValue`, which the Smithy model marks `sensitive: true` — the same shape whose boundary clause the test above requires to carry no value. So the identical member is treated two opposite ways by one service: a `filterValues` value one byte past its `@length` ceiling comes back with the value withheld, while a `filterValues` value that is merely non-numeric comes back with the value printed. The refusal does not need it: it already names the member (`filterValues`), the parameter (`$YEAR`) and the clause (`Items.YEAR >=`), which is all the existing test for this path asserts.

The same leak is reachable on all three operations (each binds `filterArn`/`filterValues` through `State::bind_filter`) and on `GetRecommendations`'s promotion filter. The other `unorderable` call site — `orderable()` at parse time — quotes a literal written into the filter's OWN expression by the control plane, not a caller's sensitive value, so only the `bind` call site is affected.

## Evidence

The rule this crate states, and applies to every member path — `src/service.rs:52-55`:
```
/// * a clause names the offending MEMBER PATH and the CONSTRAINT it broke and states NO
///   value — `Value at '<path>' failed to satisfy constraint: <requirement>` — neither the
///   bytes a present member submitted nor the `null` an absent member would otherwise be
///   stated with;
```
and `src/service.rs:69-73`:
```
fn refused(refusal: &Refusal) -> Option<Refused> {
    // `true` for EVERY member path: this service withholds the value in every clause, whatever
    // constraint the value broke and whether or not the model marks that member `@sensitive`.
    ::service_common::validation_message::aws_validation_refusal_withholding(refusal, |_| true)
}
```
The test that pins it as an absolute, for the `filterValues` map value itself — `tests/all/refusal_wording.rs:229-232` and `:283-287`:
```
    // `Context`'s and `FilterValues`'s map VALUES are the shapes the model marks
    // `@sensitive`, and a clause about one reads exactly as every other clause of this
    // service does: the member path and the requirement, and no value.
```
```
        assert!(
            !refusal.message.contains(&past_the_ceiling),
            "case {route} {member}: a sensitive value must never be quoted back, got {:?}",
            refusal.message
        );
```
(its rows include `("filterValues.GENRE", RECOMMENDATIONS, ...)` at `tests/all/refusal_wording.rs:244-253`.)

The code that quotes it back — `src/state/filter.rs:606-626`:
```
/// The refusal an ordering comparison earns when the value it would compare against
/// names no number: naming the clause, the operator and the value, plus the
/// `filterValues` parameter the value came from when it came from one.
fn unorderable(
...
    format!(
        "{source}the non-numeric value '{value}', which the {dataset}.{column} {} comparison \
         cannot apply: {} compares numbers only",
```
reached with the REQUEST's value — `src/state/filter.rs:872-886` (the operand is resolved from `filter_values`) and `src/state/filter.rs:923-934`:
```
            Predicate::Compare { op, operand } => {
                match resolve(operand, filter_values).and_then(|resolved| resolved.into_iter().next())
                {
                    Some(value) => {
                        bound_comparison(*op, &clause.dataset, &clause.column, operand, value)?
```
```
    match numeric(&value) {
        Some(number) => Ok(BoundPredicate::Order { op, value: number }),
        None => Err(unorderable(
            dataset,
            column,
            op,
            &value,
```
and carried to the client: `src/state.rs:638-651` (`filter::bind(...).map_err(Denied::invalid)`), called from `src/ops/get_recommendations.rs:330-336`:
```
        let request_filter = guard
            .bind_filter(
                registry::present(input.filter_arn.as_deref()),
                Entity::Items,
                input.filter_values.as_ref(),
            )
            .map_err(deny)?;
```
with `deny` rendering it as the modelled 400 message — `src/ops/get_recommendations.rs:671-673`:
```
fn deny(denied: Denied) -> GetRecommendationsError {
    invalid(denied.message())
}
```
The model marks that value sensitive — `smithy_query(query="shape", shape="FilterValues")`:
```
"FilterValues": {"type": "map", "length": {"min": 0, "max": 25}, "key": {"target": "FilterAttributeName", ...}, "value": {"target": "FilterAttributeValue", "length": {"min": 0, "max": 1000}, "sensitive": true}}
```
Test coverage: the only test over this refusal asserts the member and the parameter, never the value — `tests/all/comparison_operands.rs:741-749`:
```
            assert!(
                refusal.message.contains("filterValues"),
                "case {case}: the refusal must name the member it is built from, got {refusal:?}"
            );
            assert!(
                refusal.message.contains(&format!("${parameter}")),
                "case {case}: the refusal must name the parameter whose value it cannot apply, \
                 got {refusal:?}"
            );
```
so withholding the value leaves every existing assertion satisfied.

## Steps to Reproduce

1. Register an ACTIVE USER_PERSONALIZATION campaign and a filter whose expression orders a numeric column, e.g. `INCLUDE ItemID WHERE Items.YEAR >= $YEAR` (`crate::state::register_campaign` / `register_filter`, or the control plane's CreateCampaign/CreateFilter).
2. POST /recommendations with that `campaignArn`, `userId: "user-1"`, that `filterArn`, and `filterValues: {"YEAR": "\"super-secret-cohort\""}` — a value the ordering comparison cannot apply.
3. Observe HTTP 400 `InvalidInputException` whose message quotes the caller's `filterValues` value verbatim: `filterValues binds the filter expression parameter $YEAR to the non-numeric value 'super-secret-cohort', which the Items.YEAR >= comparison cannot apply: >= compares numbers only` (src/state/filter.rs:620-625).
4. Compare with the same member one byte past its `@length` ceiling (`filterValues: {"YEAR": "v".repeat(1001)}`): the boundary clause names only the member and the requirement and withholds the value, as src/service.rs:69-73 and tests/all/refusal_wording.rs:283-287 require.
5. Confirm the model marks the echoed value sensitive: `smithy_query(query="shape", shape="FilterValues")` reports the map value target `FilterAttributeValue` with `sensitive: true`.
6. Repeat on POST /personalize-ranking and POST /action-recommendations, and on a `promotions[0].filterValues` entry of /recommendations: all of them reach the same `bind` path (src/ops/get_personalized_ranking.rs:278-284, src/ops/get_action_recommendations.rs:248-254, src/ops/get_recommendations.rs:625-631).
