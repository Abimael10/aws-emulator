Issue-Severity: MEDIUM
Issue-Gap-Class: error-response
Issue-Commit: 738f456db4ce6c2a753baa11d2746fa368552910

# A nextToken this service never issued is pre-empted by the registry 403/404, breaking the handler's own "well-formedness first" rule

_Filed: 2026-11-25T02:58:28Z_
_Operation: ListDiscoverableRegistryRecords | Class: error-response_

> _Note: this issue was identified against the code base at commit `738f456db4ce6c2a753baa11d2746fa368552910`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

Inside `ListDiscoverableRegistryRecords::handle` the request's well-formedness checks are split across the authorization gate and the registry lookup. The `filters` members are validated FIRST, before authorization and before the registry is resolved, and the handler states why: whether a request is well formed must not depend on who asked or on what the store holds. The `nextToken` refusal — which this crate itself classifies as input the service "cannot parse at all (an opaque token it did not mint, say)" and answers as a 400 `ValidationException` with reason `CannotParse` — is decided LAST: after `authorization::authorize` (403) and after `owner::registry` (404/500).

Consequence: the same malformed `nextToken` gets three different answers depending on who asked and on what the store holds — 400 `ValidationException` for a registry the store holds, 404 `ResourceNotFoundException` for a registry it does not hold, and 403 `AccessDeniedException` for a caller whose policies do not allow the action. That is exactly the pre-emption the crate's own filter suite is written to prevent ("...and one the store does not hold at all, which a 404 would otherwise pre-empt"). Nothing forces the ordering: the token payload is compared against the registry id, and that id is available from the request alone via `crate::state::registry_id_from_identifier` (the same reading `resolved_registry_resource` already uses before the store is touched), so the token can be parsed and matched before the caller is authorized, exactly as the filters are. No existing test pins the current ordering: `a_next_token_this_service_did_not_issue_is_refused` only ever posts the bad token against `REGISTRY`, a registry the store holds.

## Evidence

Rule the handler states for filters — services/service-com-amazonaws-agentregistry-agentregistry/src/ops/list_discoverable_registry_records.rs:180-186:
"// A filter's `values` is as much part of a well-formed request as its name: ... Checked for every
// filter before the caller is authorized, before the registry is resolved and
// before any record is read, so whether a request is well formed never depends
// on who asked or on what the store holds."

The code that breaks it, same handler — the token is decoded only after the gate and the lookup:
- list_discoverable_registry_records.rs:224 `crate::state::authorization::authorize(` ... :230 `.map_err(ListDiscoverableRegistryRecordsErrorModeled::AccessDeniedException)?;`
- list_discoverable_registry_records.rs:232 `let registry = match crate::state::owner::registry(&*self.engine, &input.registry_id) {` ... :245-252 `Ok(None) => { return Err(...ResourceNotFoundException(crate::state::registry_not_found(&input.registry_id))...) }`
- list_discoverable_registry_records.rs:258-270 `let start_after = match input.next_token.as_deref() { Some(token) => match decode_next_token(token, &registry.registry_id) { ... None => { return Err(...ValidationException(crate::state::cannot_parse("nextToken is not a pagination token this service issued for this registry."))...) }`

The crate's own classification of that refusal — src/state.rs:158-160:
"/// The modelled `ValidationException` for a request value this service cannot
/// parse at all (an opaque token it did not mint, say)."

The rule the sibling check is tested against — tests/all/registry_read_refusals.rs:837-842 (test `a_filter_is_refused_before_the_registry_is_resolved_or_a_record_is_read`):
"/// Whether a filter is well formed is a property of the REQUEST: the same sentence
/// whatever else the read carries — the page members at their published bounds, a
/// resumption token this service never minted, either admitted form of the registry
/// identifier — and whatever the store holds for the registry named there, including a
/// registry holding no discoverable record and one the store does not hold at all, which
/// a 404 would otherwise pre-empt."
with the row that exercises an absent registry at registry_read_refusals.rs:879-883 `("a registry the store does not hold", absent_registry, json!({ "filters": offending }))` → 400.

Coverage of the token refusal is store-dependent only by accident — tests/all/discovery_read_path.rs:1341-1349 posts the bad tokens to `list_path(REGISTRY)` (a registry the store holds) and asserts 400; no case posts one to an absent registry or as an unauthorized caller.

## Steps to Reproduce

1. POST /registries/abc123def456/discoverable-records-list with body {"nextToken": "not-a-token"} against the suite's seeded router — answered 400 ValidationException (reason CannotParse), as tests/all/discovery_read_path.rs:1341 asserts.
2. POST /registries/zzz999999999/discoverable-records-list (a RegistryIdentifier the model admits that the store does not hold) with the SAME body {"nextToken": "not-a-token"} — the handler authorizes, then resolves, and answers 404 ResourceNotFoundException "Registry not found: zzz999999999" instead of the 400 the identical token got in step 1.
3. POST the same body with {"filters": [{"name": "recordType", "values": ["MCP", "AGENT"]}]} to that same absent registry — answered 400, because the filter checks run before the lookup (registry_read_refusals.rs:879-883).
4. Repeat step 1's request as the restricted caller of tests/all/discovery_read_authorization.rs (a session whose role allows nothing) against the registry the store DOES hold — the 403 from list_discoverable_registry_records.rs:224-230 pre-empts the token refusal.
