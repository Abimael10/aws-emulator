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
Issue-Key: service-quotas|ListAWSDefaultServiceQuotas|2xx->2xx|$.Quotas[QuotaCode="L-096DBB95"].QuotaName

# ListAWSDefaultServiceQuotas still misbinds the `glue` rows no recorded value is quoted for

## Operation
ListAWSDefaultServiceQuotas

## Description
RESIDUE of the fix for
`20261115T133417Z-service-quotas-glue-listawsdefaultservicequotas-default-rows-carry-another-quota-s.md`
(now under `issues/things-that-now-work/`). That report quoted the recorded QuotaName, Value
and Description for six codes of the `glue` default-quota catalog; each of those is now
published under the code the recording binds it to (`L-04CEE988` the machine-learning
transform count, `L-E15CE20A` the concurrent task runs of one transform, `L-4256D6D2` the
connection plane, `L-2C3F5401` the per-table partition plane, `L-F1653A6D` the trigger
plane, `L-3890A802` the column-statistics sentence), and each quota those codes used to
carry is published under no code at all. What is left is the rows the report lists WITHOUT
quoting their recorded values.

THE RULE (unchanged, and the one the fix applied to the quoted rows). For a given
`ServiceCode`, the row published under a `QuotaCode` carries the `QuotaName`, `Value`,
`Unit`, `Adjustable`, `GlobalQuota`, `Period`, `UsageMetric`, `QuotaContext` and
`Description` of the quota the service publishes UNDER THAT CODE — the text as the service
words it, never a paraphrase and never a neighbouring quota's — and a quota whose own code
no source states is published under no code at all. Smithy fixes the row SHAPE
(`ServiceQuota`: `QuotaCode` @length 1..128, `Value` @range 0..10000000000, `Unit`,
`Adjustable`, `GlobalQuota`, `UsageMetric`, `Period`, `QuotaContext`, `Description`
@length 0..350); the per-code CONTENT is recorded behaviour, so the recorded
`ListAWSDefaultServiceQuotas` response of `glue` is the reference.

WHAT A FIXER NEEDS, AND WHY THIS IS FILED SEPARATELY. The remaining rows cannot be authored
from anything in this checkout: the replay tool reports this cluster as a body divergence
without disclosing the recorded values, and the report quotes AWS's values only for the rows
already fixed. Closing this needs the recorded rows themselves (the `QuotaName`, `Value`,
`Unit`, `Adjustable`, `Period`, `UsageMetric`, `QuotaContext` and `Description` each of the
codes below publishes). Authoring a plausible sentence or code pairing instead is the defect
itself.

## Evidence
Replay of trace set simparity-wire-aws-v1 after the fix: the fixture still diverges on body
content only (expected 200, actual 200), and the divergence list — which used to open at
`L-04CEE988` — now opens at `L-071ABE08`, so the six rebound rows match the recording member
for member. The codes still reported:

- carrying another quota's `QuotaName`, `Value` or `Description`: L-071ABE08, L-096DBB95,
  L-4071B0E3, L-64A88F0A, L-76EC689B, L-785C1F39, L-7DD7C33A, L-83192DBF, and the
  catalog-API request-rate codes L-83A59AA6, L-87F5991C, L-94D025B7, L-A457CBAB, L-ACAC12B6,
  L-AD871090, L-B8497671, L-C3952F6C, L-CB69DFA0, L-D987EC31, L-DBA56E2F, L-EAC30D24,
  L-EEC98450, L-F1AAB534, L-F545C262, L-F574AED9, L-F953935E, L-FB210BD9, L-FEBBFA7A. Each
  keeps the plane or the call rate AWS's published `glue` quota table states, under the code
  this catalog has carried it on, because no source available here states which quota the
  recording publishes under those codes.
- published under no row at all: `L-2E9FB93F` and `L-337244C9` (the recording publishes a row
  for each, and names neither quota in anything available here), beside `L-5E4153CA`, whose
  machine-learning-transform plane the recording binds to `L-04CEE988`.
- the quotas now published under no code, whose own `QuotaCode` no source states: the account
  database plane, the workflow plane, the development-endpoint plane, the account-wide
  machine-learning-transform plane, and the `GetTable` and `StartCrawler` call rates.

The rows the fix DID correct, and must stay corrected, are pinned in
`services/service-com-amazonaws-servicequotas-servicequotasv20190624/tests/all/glue_default_rows_under_their_published_codes.rs`
and in `tests/all/recorded_row_facts_under_their_quota_codes.rs`; the shortfall against AWS's
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
   recorded row for the same `QuotaCode`, and no recorded `QuotaCode` is absent.
