Issue-Severity: MEDIUM
Issue-Gap-Class: wrong-behavior
Issue-Commit: 3b2c489229ee1293122b93552c056649e886f841

# A templateLinked CreatePolicy is refused 400 in a STRICT store with no schema, although the rule the handler applies is modelled for static policies and policy templates only

_Filed: 2026-11-15T16:57:39Z_
_Operation: CreatePolicy | Class: wrong-behavior_

> _Note: this issue was identified against the code base at commit `3b2c489229ee1293122b93552c056649e886f841`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`CreatePolicy` runs the "STRICT store holds no schema" refusal for BOTH arms of `PolicyDefinition`, because `store.schema_to_validate_against(...)` is called with `?` BEFORE the `match &input.definition` that selects the arm (`src/ops/create_policy.rs:331-333`). A `templateLinked` definition therefore answers HTTP 400 `ValidationException` naming `/policyStoreId` in a STRICT store that holds no schema, even though such a request submits no Cedar text at all — it carries a `policyTemplateId` and up to two `EntityIdentifier`s, every one of them already-stored or model-bounded.

The modelled rule enumerates exactly which submissions that state rejects: static policies and policy templates. A template-linked policy is neither; it instantiates a template whose text this store already accepted. With a schema present the handler validates the LINKED policy (`link_template(&template.statement, &link, schema_json)`, `src/ops/create_policy.rs:382-387`), and `link_template` accepts `schema_json: Option<&str>` and simply skips validation when it is `None` (`src/state/cedar.rs:683`), so the `None` path is already implemented and reachable — the handler just never reaches it, because the `?` on line 333 refuses first.

The state is reachable two ways: create a store with `mode: OFF`, create a template, `UpdatePolicyStore` to `STRICT` (the handler assigns the mode with no schema requirement, `src/ops/update_policy_store.rs:181`); or create a STRICT store, `PutSchema`, create a template, then `PutSchema` with `{}` to delete the schema (`src/state.rs:1298-1301`). In both, every later template-linked `CreatePolicy` is refused.

## Evidence

MODEL (`smithy_query(query="doc", shape="ValidationSettings", member="mode")`), verbatim: "If <code>Mode=STRICT</code> and the policy store doesn't contain a schema, Verified Permissions rejects all static policies and policy templates because there is no schema to validate against." and "To submit a static policy or policy template without a schema, you must turn off validation." — the enumeration is `static policies` and `policy templates`; a template-linked policy is neither.

HANDLER, `src/ops/create_policy.rs:327-333` — the refusal runs before the arm is selected, so it applies to `templateLinked` too:

        // What the store's validation settings say this policy is checked
        // against — `None` when the store validates nothing, and the refusal a
        // STRICT store with no schema raises. The rule lives on the store, so
        // every write handler that validates Cedar asks it the same way.
        let schema_json = store
            .schema_to_validate_against(crate::state::ValidatedContent::Policy)
            .map_err(invalid)?;

The refusal it propagates, `src/state.rs:1341-1349`:

        match self.schema.as_ref() {
            None => Err(crate::ops::InvalidInput::new(
                crate::ops::paths::POLICY_STORE_ID,
                format!(
                    "its validation mode is STRICT and it contains no schema, so there is \
                     nothing to validate {} against",

THE CRATE'S OWN SCOPING OF THAT RULE, `src/state.rs:526-527` — the value the handler passes is documented as a STATIC policy's statement:

        /// A static policy's statement: `CreatePolicy` and `UpdatePolicy`.
        Policy,

AND the same handler, five lines above the refusal, states that a `templateLinked` definition carries no caller Cedar text at all (`src/ops/create_policy.rs:316-319`):

        // the parser or the validator. Only a `static` definition carries
        // caller-sized Cedar text — a `templateLinked` one carries a template
        // reference and two entity identifiers, each already bounded by the model
        // — so only it has a size to check.

AND the suite that pins this rule scopes it the same way, `tests/all/store_validation_settings.rs:169-175`:

    /// Answer 3: a `STRICT` store holding no schema refuses EVERY write that submits
    /// Cedar content, with the modelled `ValidationException` naming
    /// `/policyStoreId` ...
    /// The rows span the four write operations the rule covers, both kinds of content
    /// (a policy and a policy template), ...

TEST COVERAGE: the six rows of `a_strict_store_with_no_schema_refuses_every_cedar_write` (`tests/all/store_validation_settings.rs:204-255`) are `CreatePolicy` with `definition.static`, `CreatePolicyTemplate`, `UpdatePolicy` with `definition.static` and `UpdatePolicyTemplate` only — no row submits a `definition.templateLinked`, so nothing in the suite pins the behaviour described here, and `tests/all/template_linked_policies.rs:712-755` switches a store to STRICT only after its schema is in place.

## Steps to Reproduce

1. CreatePolicyStore(validationSettings={mode: "OFF"}).
2. CreatePolicyTemplate(policyStoreId=<store>, statement='permit(principal == ?principal, action, resource);') — succeeds, the store validates nothing.
3. UpdatePolicyStore(policyStoreId=<store>, validationSettings={mode: "STRICT"}) — succeeds; the store is now STRICT and holds no schema.
4. CreatePolicy(policyStoreId=<store>, definition={templateLinked: {policyTemplateId: <template>, principal: {entityType: "PhotoApp::User", entityId: "alice"}}}).
5. Observe HTTP 400 ValidationException with fieldList[0].path = "/policyStoreId" and reason "its validation mode is STRICT and it contains no schema, so there is nothing to validate a policy against", although the request submits no static policy and no policy template — the two submissions the modelled rule says that state rejects.
6. Repeat via the other route: CreatePolicyStore(mode STRICT) -> PutSchema(<schema>) -> CreatePolicyTemplate -> PutSchema(definition={cedarJson: "{}"}) -> CreatePolicy(templateLinked) — same 400.
