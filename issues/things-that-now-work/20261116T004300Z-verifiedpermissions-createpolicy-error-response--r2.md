Issue-Severity: MEDIUM
Issue-Gap-Class: error-response
Issue-Commit: b3a770a3be6c5f0f7c75c209666b17acc27919a9

# CreatePolicy raises the STRICT-store-has-no-schema refusal before parsing the statement, the opposite order its three sibling Cedar writers use

_Filed: 2026-11-16T00:43:00Z_
_Operation: CreatePolicy | Class: error-response_

> _Note: this issue was identified against the code base at commit `b3a770a3be6c5f0f7c75c209666b17acc27919a9`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

Four handlers in this crate submit caller-written Cedar to a policy store and must apply the same two checks: (a) does the submitted statement PARSE, and (b) does the store's validation mode leave anything to validate against (`PolicyStore::schema_to_validate_against`, which errors for a STRICT store holding no schema and blames `/policyStoreId`).

Three of the four — `UpdatePolicy`, `CreatePolicyTemplate`, `UpdatePolicyTemplate` — run the PARSE first and only then ask the store. `CreatePolicy` alone asks the store first, so the submitted statement is never parsed at all when the store is STRICT and holds no schema.

Consequence: for one and the same pair of faults — a STRICT store with no schema AND a statement that does not parse — two sibling operations blame different request members in `ValidationException$fieldList`:

* `CreatePolicy` answers `path = /policyStoreId`, reason "its validation mode is STRICT and it contains no schema, so there is nothing to validate a policy against";
* `UpdatePolicy` on the same store with the same (unparseable) `definition.static.statement` answers `path = /definition/static/statement`, reason `Failed to validate policy body`.

A client that keys on `fieldList` is therefore told to fix the policy store on a create and to fix the statement on an update, for identical input. The crate already treats exactly this class of divergence as a defect — the fixed issue `verifiedpermissions-createpolicy-error-response` is "CreatePolicy checks the store's quota before the duplicate-name conflict, against the ordering CreatePolicyTemplate states for the same pair of checks" — and this is a second, distinct pair of checks ordered inconsistently in the same handler.

## Evidence

CreatePolicy asks the store BEFORE any parse:

`services/service-com-amazonaws-verifiedpermissions-verifiedpermissions/src/ops/create_policy.rs:327-333`
```
        // What the store's validation settings say this policy is checked
        // against — `None` when the store validates nothing, and the refusal a
        // STRICT store with no schema raises. The rule lives on the store, so
        // every write handler that validates Cedar asks it the same way.
        let schema_json = store
            .schema_to_validate_against(crate::state::ValidatedContent::Policy)
            .map_err(invalid)?;
```
and only after that, at `create_policy.rs:341-349`:
```
            match &input.definition {
                PolicyDefinition::Static(def) => {
                    if let Some(schema_json) = schema_json {
                        crate::state::cedar::validate_static_policy(schema_json, &def.statement)
                            .map_err(refused)?;
                    }
                    let scope = crate::state::cedar::parse_static_policy(&def.statement)
                        .map_err(invalid)?;
```

Its three siblings parse FIRST:

`src/ops/update_policy.rs:295-296`
```
                let scope = crate::state::cedar::parse_static_policy(&def.statement)
                    .map_err(invalid)?;
```
then `src/ops/update_policy.rs:341-343`
```
                if let Some(schema_json) = store
                    .schema_to_validate_against(crate::state::ValidatedContent::Policy)
                    .map_err(invalid)?
```

`src/ops/create_policy_template.rs:255-261`
```
        // The statement is Cedar TEMPLATE text, so the real Cedar template parser
        // decides whether it is a template at all. One it refuses is refused here,
        // BEFORE the name-conflict and quota checks: a malformed request is a 400
        // whatever else the store already holds. ...
        crate::state::cedar::parse_policy_template(&input.statement).map_err(invalid)?;
```
then `src/ops/create_policy_template.rs:268-270`
```
        if let Some(schema_json) = store
            .schema_to_validate_against(crate::state::ValidatedContent::PolicyTemplate)
            .map_err(invalid)?
```

`src/ops/update_policy_template.rs:248`
```
        let replacement = crate::state::cedar::template_scope(&input.statement).map_err(invalid)?;
```
then `src/ops/update_policy_template.rs:267-269`
```
        if let Some(schema_json) = store
            .schema_to_validate_against(crate::state::ValidatedContent::PolicyTemplate)
            .map_err(invalid)?
```

The refusal CreatePolicy returns, and the member it blames, come from `src/state.rs:1341-1351`:
```
        match self.schema.as_ref() {
            None => Err(crate::ops::InvalidInput::new(
                crate::ops::paths::POLICY_STORE_ID,
                format!(
                    "its validation mode is STRICT and it contains no schema, so there is \
                     nothing to validate {} against",
                    content.noun()
                ),
            )),
```
and the one UpdatePolicy returns from `src/state/cedar.rs:414-421`:
```
pub const POLICY_BODY_REFUSAL: &str = "Failed to validate policy body";
...
fn policy_body_refusal() -> InvalidInput {
    InvalidInput::new(paths::STATIC_STATEMENT, POLICY_BODY_REFUSAL)
}
```

Test coverage of the two refusals in isolation (neither test exercises the two faults together, so neither pins the order): `tests/all/policy_validation.rs`, `tests/all/policy_body_parse_refusal.rs`, `tests/all/validation_exception_contents.rs`, `tests/all/schema_lifecycle.rs`.

## Steps to Reproduce

1. CreatePolicyStore with `{"validationSettings": {"mode": "STRICT"}}` and put no schema in it (so the store is STRICT and holds no schema).
2. Call CreatePolicy on that store with `definition.static.statement` set to text Cedar cannot parse, e.g. `"this is not cedar"`.
3. Observe the 400 ValidationException: `fieldList[0].path` is `/policyStoreId` and the reason is the STRICT/no-schema sentence — the statement was never parsed.
4. Now PutSchema a valid schema, CreatePolicy a valid static policy, then PutSchema `{}` to delete the schema again (the store is STRICT and holds no schema once more).
5. Call UpdatePolicy on that policy with the same unparseable `definition.static.statement`.
6. Observe the 400 ValidationException: `fieldList[0].path` is `/definition/static/statement` with `Failed to validate policy body` — the opposite member is blamed for the identical pair of faults. CreatePolicyTemplate and UpdatePolicyTemplate behave like UpdatePolicy, leaving CreatePolicy the only writer with the inverted order.
