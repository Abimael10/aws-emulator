Issue-Source: trace-sweep-divergence
Issue-Trace: taxsettings-tax-exemption-workflow
Issue-Step: task#1
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: taxsettings-tax-exemption-workflow
Issue-Shape-Id: com.amazonaws.taxsettings#TaxSettings
Issue-Key: tax|GetTaxExemptionTypes|2xx->2xx|$.taxExemptionTypes[*]
Issue-Commit: c1f1f84a519926b88e66095e1ac1efcd420caa22

# tax: GetTaxExemptionTypes publishes only 2 of the 23 exemption types AWS reports

> _Note: this issue was identified against the code base at commit `c1f1f84a519926b88e66095e1ac1efcd420caa22`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetTaxExemptionTypes

## Description
One cause.

Cause 1 of 1: GetTaxExemptionTypes answers HTTP 200 with a `taxExemptionTypes` list of only TWO entries, so 21 of the 23 exemption types the operation publishes are absent from the body. The two it does carry line up with the first two published entries and compare equal; every index from `$.taxExemptionTypes[2]` through `$.taxExemptionTypes[22]` is simply missing.

AWS: Smithy (com.amazonaws.taxsettings) GetTaxExemptionTypes — POST /GetTaxExemptionTypes, empty input shape GetTaxExemptionTypesRequest, output GetTaxExemptionTypesResponse with the single member `taxExemptionTypes` -> list TaxExemptionTypes of structure TaxExemptionType {displayName (@length 0..50, @pattern ^[\s\S]*$), description (@length 1..200, @pattern ^[\s\S]*$), applicableJurisdictions -> list Authorities of Authority {country (required, @length 2..2, @pattern ^[a-zA-Z]+$), state (@length 1..50)}}; modeled errors ValidationException (httpError 400), ResourceNotFoundException (404), InternalServerException (500); operation is @readonly and documented as "Get supported tax exemption types". The CONTENT of the published collection is recorded behavior only — the model constrains the shape, not the rows. The recording shows AWS answering HTTP 200 with a `taxExemptionTypes` list of 23 entries (indices 0..22), each entry carrying `displayName`, `description` (the same catalog token in both members) and its own per-state `applicableJurisdictions` list of {country:"US", state:<two-letter state>} authorities, with a different set of states per type. For example index 2 is the entry whose `displayName` and `description` are both `Charitable Organization`, applicable in its own US state set (AL, CO, CT, DC, FL, ID, IL, IN, MD, MI, MN, MO, MS, NE, NJ, NV, NY, OH, OK, PA, RI, SC, SD, TN, TX, UT, VT, WI, WV, WY), and index 3 is a further entry with a wider US state set. The requirement is the published collection in full, not these particular rows' spelling in one run: the list must contain every exemption type the service supports, each with the jurisdictions published for it.

Emulator: `GetTaxExemptionTypes::handle` returns `crate::state::tax_exemption_type_catalog()`, which maps over the hard-coded table `TAX_EXEMPTION_TYPE_CATALOG`. That table has exactly two rows — `("501C", "501C", CHARITABLE_ORGANIZATION_JURISDICTIONS)` and `("Agriculture Production", "Agriculture Production", AGRICULTURE_PRODUCTION_JURISDICTIONS)` — and its own doc comment states that any category whose published token or per-state list is "not available" is deliberately ABSENT. The replayed step therefore answers 200 (status matches) with a two-element list, and the comparison reports `$.taxExemptionTypes[2]` .. `$.taxExemptionTypes[22]`: missing (expected the entry, got None). No error, no header difference — purely an under-populated body.

Why: this is the only API by which a client discovers which exemption types it may claim and where each one applies. A client relying on AWS's behavior enumerates the response to populate a picker, to validate an `exemptionType` before calling PutTaxExemption, or to decide which jurisdictions a type is grantable in; against the emulator 21 of 23 types do not exist, so legitimate types are reported unsupported, a UI offers two choices instead of twenty-three, and the jurisdiction check for any absent type falls back to "recorded as submitted" instead of the published per-state authority list. Conformance and catalog-completeness tests pass against the emulator and fail against AWS; code that treats this list as the authoritative set of exemption types silently excludes most of it.

Fix: the taxsettings service crate's state catalog that backs this operation — `crate::state::TAX_EXEMPTION_TYPE_CATALOG` and its per-type jurisdiction tables, which `crate::state::tax_exemption_type_catalog()` projects into the modeled TaxExemptionType list for the GetTaxExemptionTypes handler. The catalog must carry every exemption type the service publishes, each with its own `applicableJurisdictions` authority list, in the published `displayName` order, rather than only the subset whose rows were transcribed; the handler/serializer itself is correct (status, shape and member mapping all compare equal for the rows that exist), so the gap is the data table, not the response path. Note that `find_exemption_type` / `check_exemption_authority` read the same table, so completing it also restores the jurisdiction validation for the missing types.

Traces: taxsettings-tax-exemption-workflow#task#1 (divergence_key service=tax|GetTaxExemptionTypes|status=200->200|keys=$.taxExemptionTypes[2]..$.taxExemptionTypes[22]). Related open issue 20261112T023149Z-tax-puttaxinheritance-accepts-a-non-management-account-opt-in.md covers a different tax operation and does not cover this failure. The previously RESOLVED issues 20261023T110604Z-taxsettings-gettaxexemptiontypes-stub.md, 20261024T200859Z-taxsettings-gettaxexemptiontypes-wrong-behavior.md and 20261103T181220Z-tax-gettaxexemptiontypes-publishes-a-7-row-invented-catalog-instead.md touched this operation — the invented 7-row catalog was replaced by a 2-row transcribed one, so the body is no longer invented but is now incomplete; a resolved issue covers nothing, hence this filing.

## Evidence
Fixture taxsettings-tax-exemption-workflow, step task#1 (affected: taxsettings-tax-exemption-workflow#task#1, affected_count 1), operation GetTaxExemptionTypes, verdict mismatch.

Expected (recorded AWS response): HTTP 200, body {"taxExemptionTypes": [ ... 23 entries, indices 0..22 ... ]}. Each entry is {displayName, description, applicableJurisdictions:[{country, state}, ...]}. Index 2 = {"displayName": "Charitable Organization", "description": "Charitable Organization", "applicableJurisdictions": [{"country":"US","state":"AL"},{"country":"US","state":"CO"},{"country":"US","state":"CT"},{"country":"US","state":"DC"},{"country":"US","state":"FL"},{"country":"US","state":"ID"},{"country":"US","state":"IL"},{"country":"US","state":"IN"},{"country":"US","state":"MD"},{"country":"US","state":"MI"},{"country":"US","state":"MN"},{"country":"US","state":"MO"},{"country":"US","state":"MS"},{"country":"US","state":"NE"},{"country":"US","state":"NJ"},{"country":"US","state":"NV"},{"country":"US","state":"NY"},{"country":"US","state":"OH"},{"country":"US","state":"OK"},{"country":"US","state":"PA"},{"country":"US","state":"RI"},{"country":"US","state":"SC"},{"country":"US","state":"SD"},{"country":"US","state":"TN"},{"country":"US","state":"TX"},{"country":"US","state":"UT"},{"country":"US","state":"VT"},{"country":"US","state":"WI"},{"country":"US","state":"WV"},{"country":"US","state":"WY"}]}; index 3 is a further entry with a wider US state set (AL, AR, AZ, CA, CO, CT, DC, FL, GA, HI, IA, ID, IL, IN, KS, KY, LA, MA, MD, ME, MI, ...).

Actual (emulator): HTTP 200, body {"taxExemptionTypes": [ <2 entries> ]} — only indices 0 and 1 are present; both compare equal.

Replay detail, verbatim (head):
  $.taxExemptionTypes[2]: missing (expected {'applicableJurisdictions': [{'country': 'US', 'state': 'AL'}, ... , {'country': 'US', 'state': 'WY'}], 'description': 'Charitable Organization', 'displayName': 'Charitable Organization'}, got None)
  $.taxExemptionTypes[3]: missing (expected {'applicableJurisdictions': [{'country': 'US', 'state': 'AL'}, {'country': 'US', 'state': 'AR'}, {'country': 'US', 'state': 'AZ'}, ...], ...}, got None)

Full missing-key set from divergence_key: $.taxExemptionTypes[2], [3], [4], [5], [6], [7], [8], [9], [10], [11], [12], [13], [14], [15], [16], [17], [18], [19], [20], [21], [22] — 21 absent entries.

Source in the checkout: services/service-com-amazonaws-taxsettings-taxsettings/src/ops/get_tax_exemption_types.rs:150-152 returns crate::state::tax_exemption_type_catalog(); services/service-com-amazonaws-taxsettings-taxsettings/src/state.rs:4279-4286 defines TAX_EXEMPTION_TYPE_CATALOG with exactly two rows ("501C", "Agriculture Production"), with per-type jurisdiction tables at state.rs:4291 and state.rs:4336, projected at state.rs:4387-4392.

Status, error code and message: no error is involved — expected 200, actual 200. The divergence is confined to the body's `taxExemptionTypes` list membership.

## Steps to Reproduce
1. Clone this checkout and boot the emulator from it.
2. Replay the wire corpus (trace set simparity-wire-aws-v1), fixture taxsettings-tax-exemption-workflow, step task#1: POST /GetTaxExemptionTypes with an empty JSON body ({}).
3. Observe the emulator answers HTTP 200 with a taxExemptionTypes list containing only 2 entries ("501C" and "Agriculture Production").
4. AWS answers HTTP 200 with 23 entries (indices 0..22), each with its own displayName/description token and its own per-state applicableJurisdictions list; index 2 is "Charitable Organization".
5. The replay reports $.taxExemptionTypes[2] .. $.taxExemptionTypes[22] as missing.
