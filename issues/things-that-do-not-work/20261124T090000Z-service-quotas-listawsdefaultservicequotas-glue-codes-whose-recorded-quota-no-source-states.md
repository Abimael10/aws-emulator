Issue-Source: fix-residue
Issue-Trace: service-quotas-catalog-list-aws-default-service-quotas-glue
Issue-Step: task#1
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: service-quotas-catalog-list-aws-default-service-quotas-glue
Issue-Shape-Id: com.amazonaws.servicequotas#ServiceQuotasV20190624
Issue-Key: service-quotas|ListAWSDefaultServiceQuotas|2xx->2xx|$.Quotas[QuotaCode="L-2E9FB93F"]
Issue-Key: service-quotas|ListAWSDefaultServiceQuotas|2xx->2xx|$.Quotas[QuotaCode="L-337244C9"]
Issue-Key: service-quotas|ListAWSDefaultServiceQuotas|2xx->2xx|$.Quotas[QuotaCode="L-071ABE08"].QuotaName
Issue-Related: 20261121T120000Z-service-quotas-listawsdefaultservicequotas-residual-misbound-glue-rows.md
Issue-Related: 20261121T171035Z-service-quotas-listawsdefaultservicequotas-glue-rows-still-publish-another.md
Issue-Related: 20261123T024600Z-service-quotas-listawsdefaultservicequotas-rows-misbound-across-events-fms-fsx-gamelift.md

# ListAWSDefaultServiceQuotas: the `glue` codes whose quota no source in this checkout states

## Operation
ListAWSDefaultServiceQuotas

## Description
RESIDUE of the fix for
`20261121T120000Z-service-quotas-listawsdefaultservicequotas-residual-misbound-glue-rows.md`
(now under `issues/things-that-now-work/`). That report listed the `glue` rows still bound to
the wrong `QuotaCode`; the fix bound every one of them whose recorded content this checkout
states — `L-DBA56E2F` the account's development-endpoint plane ('Max development endpoint per
account', 25.0), `L-F1AAB534` its integration plane ('Number of integrations', 40.0),
`L-87F5991C` its data-quality-ruleset plane ('Max data quality rulesets per account',
1000000.0) and `L-785C1F39` the Data Catalog's global tag plane ('Global max tags',
Description 'Catalog: global max tags') — and left `L-83192DBF`, whose data-quality-ruleset
plane is published under `L-87F5991C`, carrying no row. What is left is the codes for which
NOTHING available in this checkout states which quota the recording publishes under them.

THE RULE (unchanged). For a given `ServiceCode`, the row published under a `QuotaCode`
carries the `QuotaName`, `Value`, `Unit`, `Adjustable`, `GlobalQuota`, `Period`,
`UsageMetric`, `QuotaContext` and `Description` of the quota the service publishes UNDER
THAT CODE — the text as the service words it, never a paraphrase and never a neighbouring
quota's — and a quota whose own code no source states is published under no code at all.
Smithy fixes the row SHAPE (`ServiceQuota`: `QuotaCode` @length 1..128, `Value` @range
0..10000000000, `Unit`, `Adjustable`, `GlobalQuota`, `UsageMetric`, `Period`,
`QuotaContext`, `Description` @length 0..350); the per-code CONTENT is recorded behaviour,
so the recorded `ListAWSDefaultServiceQuotas` response of `glue` is the reference.

WHY THIS IS BLOCKED, AND WHAT A FIXER NEEDS. These rows cannot be authored from anything in
this checkout:

- the replay reports this cluster as a body divergence and withholds the recorded values, so
  it names the diverging `QuotaCode`s but not the rows under them;
- every related report quotes AWS's values only for the rows already bound (which is how
  those were bound at all); none quotes a value for the codes below;
- AWS's published `glue` quota table states quota names, values and sentences, but never
  which opaque `L-` code a quota is published under — and the code is exactly what these
  keys are about.

Closing this needs the recorded rows themselves: the `QuotaName`, `Value`, `Unit`,
`Adjustable`, `Period`, `UsageMetric`, `QuotaContext` and `Description` the recording
publishes under each code below. Authoring a plausible sentence or code pairing instead is
the defect itself, and was rejected at review on an earlier attempt.

## Evidence
Replay of trace set simparity-wire-aws-v1 after the fix: the fixture still diverges on body
content only (expected 200, actual 200), and the divergence set shrank from 64 member keys to
53 — the four rebound rows and the removed duplicate match the recording. The keys this
report carries, verbatim:

- `$.Quotas[QuotaCode="L-2E9FB93F"]` and `$.Quotas[QuotaCode="L-337244C9"]`: whole-row
  divergences. The recording publishes a row under each; no source available here names
  either quota, so the catalog publishes neither (they are named in glue's `gap_codes` in
  `services/service-com-amazonaws-servicequotas-servicequotasv20190624/tests/all/aws_minted_catalog_coverage.rs`,
  beside `L-5E4153CA` and `L-83192DBF`, and counted by its `GAP_CODE_COUNT` tripwire).
- `$.Quotas[QuotaCode="L-071ABE08"].QuotaName` (its `Description` and `Value` diverge too):
  the catalog publishes the per-database table plane there ('Tables per database', 200000.0,
  'The maximum number of tables per database.'), the plane AWS's published `glue` quota table
  states, under the code this catalog has carried it on; the recording publishes some other
  quota of the service under that code and no source here says which.

The same holds, with no replay key of their own in this report, for the remaining `glue`
codes the catalog cannot attribute: L-4071B0E3, L-64A88F0A, L-76EC689B, L-7DD7C33A and the
catalog-API request-rate codes L-94D025B7, L-A457CBAB (whose quota the recording NAMES — the
orchestration plane of an account's queued job runs — but whose value and sentence it does
not state here), L-ACAC12B6, L-AD871090, L-B8497671, L-C3952F6C, L-CB69DFA0, L-D987EC31,
L-EAC30D24, L-EEC98450, L-F545C262, L-F574AED9 and L-F953935E. Each keeps the plane or the
call rate AWS's published `glue` quota table states, under the code this catalog has carried
it on.

The rows the fixes DID correct, and must stay corrected, are pinned in
`services/service-com-amazonaws-servicequotas-servicequotasv20190624/tests/all/glue_default_rows_under_their_published_codes.rs`,
`tests/all/glue_account_planes_under_their_published_codes.rs`,
`tests/all/glue_capacity_planes_under_their_published_codes.rs` and
`tests/all/recorded_row_facts_under_their_quota_codes.rs`; the shortfall against AWS's
47-row catalog is pinned in `tests/all/aws_minted_catalog_coverage.rs` (`quota_count`,
`gap_codes`, `GAP_CODE_COUNT`).

## Steps to Reproduce
1. Boot the emulator from this checkout and replay trace set simparity-wire-aws-v1
   (fixture service-quotas-catalog-list-aws-default-service-quotas-glue, step task#1).
2. POST the service-quotas endpoint with target
   `ServiceQuotasV20190624.ListAWSDefaultServiceQuotas` and body `{"ServiceCode": "glue"}`.
3. Index `$.Quotas` by `QuotaCode` and compare each row against the recorded response for the
   SAME `QuotaCode`.
4. The `ServiceCode` is clean when every returned row's `QuotaName`, `Description`, `Value`,
   `Unit`, `Adjustable`, `GlobalQuota`, `Period`, `UsageMetric` and `QuotaContext` match the
   recorded row for the same `QuotaCode`, and no recorded `QuotaCode` is absent — in
   particular rows exist for `L-2E9FB93F` and `L-337244C9`.
