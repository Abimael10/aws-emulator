Issue-Severity: LOW
Issue-Gap-Class: maintainability
Issue-Commit: 3fa8c42c1df735de25d8beda9af18b43438c14a4

# The clause-assembly rule is implemented twice in service/validation.rs: `above_ceiling` hand-writes the clause `Clause::stated` already builds, and skips the module's own rule 9 path

_Filed: 2026-11-26T07:46:57Z_
_Operation: ListAccountActivities | Class: maintainability_

> _Note: this issue was identified against the code base at commit `3fa8c42c1df735de25d8beda9af18b43438c14a4`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`src/service/validation.rs` states ONE clause form for every refusal of this service (rule 1 + rule 2: an envelope over clauses of the shape `<subject> at '<path>' failed to satisfy constraint: <requirement>`), and rule 7 says a bound the SERVICE enforces for itself "is stated in the same envelope and the same requirement sentence" as a modelled one. That assembly is implemented by `Clause::stated` + `worded` + `envelope`, reached through the single renderer `message`.

`above_ceiling` — the only path a service-enforced bound takes, reached from `crate::state::max_results_refusal`, which `ListAccountActivities` is the one operation to call — re-implements that same assembly inline instead: it hand-writes the `format!("Value at '{path}' {CLAUSE_MARKER}{requirement}")` string that `Clause::stated` already produces for `Quoted::Counted` (whose `subject()` is exactly `"Value"`), and it hand-writes a third copy of the "fall back to the framework's own `Violation::message()`" branch that `one_clause` and `message` each already carry.

So the one clause form lives in two places and the copies can drift: a change to `Clause::stated`'s layout (the subject spelling, the quoting, the `at '...'` framing) reaches every modelled refusal of this service and silently does NOT reach the service-enforced bound, even though rule 7 says the two must read alike. The same bypass means `above_ceiling`'s sentence never passes through `message`, so rule 9's bound — which the module header says holds "on every path through this module" — is not applied on this one.

There is no behavioural change to make: `Quoted::Counted.subject()` is the literal `"Value"` the hand-written `format!` already starts with, and `above_ceiling` already calls the same `envelope`, so routing it through the existing clause writer produces a byte-identical message. Severity LOW: no response changes.

## Evidence

The ONE clause form, `services/service-com-amazonaws-freetier-awsfreetierservice/src/service/validation.rs:304-310`:
```rust
    fn stated(&self, quoted: &Quoted) -> String {
        format!(
            "{} at '{}' {CLAUSE_MARKER}{}",
            quoted.subject(),
            self.path,
            self.requirement
        )
    }
```
with `src/service/validation.rs:337-343`:
```rust
    fn subject(&self) -> String {
        match self {
            Self::Null => "Value null".to_string(),
            Self::Counted => "Value".to_string(),
            Self::Value(value) => format!("Value '{value}'"),
        }
    }
```

The SECOND copy of that assembly, `src/service/validation.rs:445-461`:
```rust
pub(crate) fn above_ceiling(path: &str, ceiling: i32) -> String {
    let kind = ViolationKind::MaxValue(ceiling.to_string());
    // A class the framework words without the clause marker keeps its whole message,
    // which still states the bound — the same fallback [`framework_requirement`]'s callers
    // make, written out because there is no framework wording to fall back TO here.
    let requirement = framework_requirement(&kind).unwrap_or_else(|| {
        Violation {
            path: String::new(),
            kind,
            value: None,
            sensitive: false,
        }
        .message()
    });
    let clause = format!("Value at '{path}' {CLAUSE_MARKER}{requirement}");
    envelope(std::slice::from_ref(&clause)).unwrap_or(clause)
}
```
(the `.unwrap_or_else(.. .message())` fallback is the third copy of the one at `src/service/validation.rs:556-558`, `fn one_clause`, and at `src/service/validation.rs:528-529`, `message(..).or_else(|| ::aws_smithy_schema_server::envelope(&at_member))`.)

The crate's own rules that say the two must be one, `src/service/validation.rs:17` (rule 1):
```
//! 1. the envelope is the AWS one, `<N> validation error(s) detected: ` over one clause
//!    per failing (member, requirement), joined with `; ` ([`envelope`]);
```
`src/service/validation.rs:36-38` (rule 7):
```
//! 7. a bound the SERVICE enforces for itself — one the model does not declare, so the
//!    constraint walk never sees it — is stated in the same envelope and the same
//!    requirement sentence, and quotes NO value ([`above_ceiling`]);
```
and `src/service/validation.rs:56-58` (rule 9), the bound `above_ceiling` does not route through:
```
//!    ([`LARGEST_MESSAGE_BYTES`], [`held_within_the_message_maximum`]) — on every path
//!    through this module, so the bound cannot hold for the boundary walk's refusal and not
//!    for a handler's own.
```
while `message` — the renderer that applies rule 9 — is documented as "THE ONE renderer every refusal of this service reaches" at `src/service/validation.rs:171-174`.

The single caller chain, `src/state.rs:631-634`:
```rust
pub fn max_results_refusal(ceiling: i32, max_results: i32) -> Option<String> {
    (max_results > ceiling)
        .then(|| crate::service::validation::above_ceiling(MAX_RESULTS_MEMBER, ceiling))
}
```
reached only from `src/ops/list_account_activities.rs:169-174`.

Test coverage that must stay unchanged (and does, since the rendering is byte-identical): `tests/all/activity_plane_refusals.rs:112-115` pins the exact sentence
```rust
    let expected = format!(
        "1 validation error detected: Value at 'maxResults' failed to satisfy constraint: \
         Member must have value less than or equal to {ACTIVITIES_MAX_RESULTS_CEILING}"
    );
```
over 15 refused rows (`tests/all/activity_plane_refusals.rs:131-150`), and `tests/all/validation_message_rules.rs:590-601` plus `tests/all/refused_request_media_type.rs:191` drive the same path on the wire.

## Steps to Reproduce

1. Read `src/service/validation.rs:304-310` and `:337-343`: the clause form of every refusal of this service is assembled in `Clause::stated`, whose `Quoted::Counted` subject is the literal "Value".
2. Read `src/service/validation.rs:459`: `above_ceiling` hand-writes `format!("Value at '{path}' {CLAUSE_MARKER}{requirement}")` — the same subject, the same `at '...'` framing and the same `CLAUSE_MARKER` — instead of building a `Clause` and wording it through the existing writer, so one clause form exists twice in this crate.
3. Read `src/service/validation.rs:36-38` (rule 7): a service-enforced bound is to be "stated in the same envelope and the same requirement sentence" as a modelled one — i.e. by the same clause writer the second copy bypasses.
4. Read `src/service/validation.rs:56-58` (rule 9) and `:171-186`: the message maximum is applied inside `message`, documented as holding "on every path through this module" and as "THE ONE renderer every refusal of this service reaches" — yet `above_ceiling` builds and returns its sentence without ever calling `message`.
5. Confirm the third copy of the fallback: `src/service/validation.rs:450-458` re-writes the `framework_requirement(..).unwrap_or_else(|| Violation{..}.message())` branch that already exists at `src/service/validation.rs:556-558` and `:528-529`.
6. Fold `above_ceiling` onto the existing clause writer (a `Clause { path, requirement, quoted: Quoted::Counted }` worded through `worded`/`envelope`, i.e. the rule-9 path) and re-run `tests/all/activity_plane_refusals.rs`, `tests/all/validation_message_rules.rs` and `tests/all/refused_request_media_type.rs`: every message and every assertion stays byte-identical, because `Quoted::Counted.subject()` is the same "Value" the hand-written format already emits.
