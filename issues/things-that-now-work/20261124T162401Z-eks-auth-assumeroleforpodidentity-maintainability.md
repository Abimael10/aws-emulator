Issue-Severity: LOW
Issue-Gap-Class: maintainability
Issue-Commit: 9d6dbefbeeb79c5752051e10ea3d5ba278a8ab35

# Delete the inlined `unanchored` pattern renderer in favour of service-common's

_Filed: 2026-11-24T16:24:01Z_
_Operation: AssumeRoleForPodIdentity | Class: maintainability_

> _Note: this issue was identified against the code base at commit `9d6dbefbeeb79c5752051e10ea3d5ba278a8ab35`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`src/service.rs` carries its own private copy of the "print a `@pattern` without the `^`/`$` anchor pair" rule (`fn unanchored` plus its helper `fn ends_with_escape`), through which every constraint refusal of this service's one operation is rendered (`stated()` -> `ViolationKind::Pattern(unanchored(pattern))`). The workspace crate `service-common` — already a path dependency of this crate in Cargo.toml and therefore in Cargo.lock — owns that exact rendering as `validation::unanchored`, and the local copy is byte-for-byte the shared one, down to the identical private `ends_with_escape`. The file's own TODO says so, and the shared module says the inlined copies are to be deleted in favour of it and that nothing new should inline the rule again.

Two copies of one rendering rule can drift (the same duplication, after it HAD drifted, is already filed for another crate as issues/things-that-do-not-work/20261028T050716Z-rbin-getrule-maintainability.md). Because the bodies are identical, switching `stated()` to call `::service_common::validation::unanchored` and deleting both local functions leaves every rendered message byte-identical and every existing test unchanged.

## Evidence

The inlined copy, with its own TODO — services/service-com-amazonaws-eksauth-eksauthfrontend/src/service.rs:144-160:
```
/// TODO: duplicate of `services/common/src/validation.rs::unanchored`; delete when that
/// module's migration lands.
fn unanchored(regex: &str) -> String {
    let Some(body) = regex.strip_prefix('^') else {
        return regex.to_string();
    };
    match body.strip_suffix('$') {
        Some(inner) if !ends_with_escape(inner) => inner.to_string(),
        _ => regex.to_string(),
    }
}
...
fn ends_with_escape(text: &str) -> bool {
    text.chars().rev().take_while(|c| *c == '\\').count() % 2 == 1
}
```
The shared implementation it duplicates — services/common/src/validation.rs:1737-1745:
```
pub fn unanchored(regex: &str) -> String {
    let Some(body) = regex.strip_prefix('^') else {
        return regex.to_string();
    };
    match body.strip_suffix('$') {
        Some(inner) if !ends_with_escape(inner) => inner.to_string(),
        _ => regex.to_string(),
    }
}
```
and its identical helper — services/common/src/validation.rs:2360-2362:
```
fn ends_with_escape(text: &str) -> bool {
    text.chars().rev().take_while(|c| *c == '\\').count() % 2 == 1
}
```
The shared module's own instruction — services/common/src/validation.rs:1695-1698:
```
/// of a validation message that are AWS's rather than any one service's. ~26 service crates
/// carry their own inlined copy (`grep -rln 'fn unanchored' services/*/src`), each pinned by
/// that service's own recordings; those copies are to be DELETED in favour of this one by the
/// change that owns that migration, and nothing new should inline the rule again.
```
The dependency is already declared — services/service-com-amazonaws-eksauth-eksauthfrontend/Cargo.toml:16:
```
service-common = { path = "../common" }
```
The only call site — src/service.rs:109: `ViolationKind::Pattern(pattern) => ViolationKind::Pattern(unanchored(pattern)),`.
Tests that cover the rendering and must stay unchanged — tests/all/rejection_errors.rs:771-772 (`.contains(&format!("regular expression pattern: {pattern}"))`, "the clause must state the unanchored pattern") and tests/all/rejection_errors.rs:822 (`Member must satisfy regular expression pattern: [A-Za-z0-9-_=]+\.[A-Za-z0-9-_=]+\.[A-Za-z0-9-_=]+`).

## Steps to Reproduce

1. Open services/service-com-amazonaws-eksauth-eksauthfrontend/src/service.rs:144-160 and read `fn unanchored` and `fn ends_with_escape`, together with the TODO above them.
2. Open services/common/src/validation.rs:1737-1745 and :2360-2362 and compare: the bodies are identical, and the module note at :1695-1698 states the inlined copies are to be deleted in favour of this one.
3. Confirm the crate already depends on it: services/service-com-amazonaws-eksauth-eksauthfrontend/Cargo.toml:16.
4. Replace the call at src/service.rs:109 with `::service_common::validation::unanchored(pattern)`, delete both local functions, and run `cargo test -p service-com-amazonaws-eksauth-eksauthfrontend`: tests/all/rejection_errors.rs (including the pattern-clause assertions at :771 and :822) passes unchanged, since the two implementations are the same code.
