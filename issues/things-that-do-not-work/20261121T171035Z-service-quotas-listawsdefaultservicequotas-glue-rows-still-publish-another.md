Issue-Source: trace-sweep-divergence
Issue-Trace: service-quotas-catalog-list-aws-default-service-quotas-glue
Issue-Step: task#1
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: service-quotas-catalog-list-aws-default-service-quotas-glue
Issue-Shape-Id: com.amazonaws.servicequotas#ServiceQuotasV20190624
Issue-Key: service-quotas|ListAWSDefaultServiceQuotas|2xx->2xx|$.Quotas[QuotaCode="L-5E4153CA"]
Issue-Related: 20261113T025107Z-service-quotas-listawsdefaultservicequotas-default-rows-misbound-and-re-worded-for.md
Issue-Related: 20261121T120000Z-service-quotas-listawsdefaultservicequotas-residual-misbound-glue-rows.md
Issue-Commit: ad86636396f6a6db2377010867131d0a7e72726e

# service-quotas ListAWSDefaultServiceQuotas glue rows still publish another quota's name, value and description under the wrong QuotaCode

> _Note: this issue was identified against the code base at commit `ad86636396f6a6db2377010867131d0a7e72726e`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ListAWSDefaultServiceQuotas

## Description
ONE cause (Cause 1 of 1): for ServiceCode `glue`, `ListAWSDefaultServiceQuotas` answers 200 with a well-formed `$.Quotas` list whose rows are bound to the WRONG `QuotaCode` — the row published under a code carries a neighbouring quota's `QuotaName`, `Value` and `Description` — and two recorded codes are published under no row at all.

AWS: recorded behavior only for the catalog CONTENT; Smithy fixes just the row shape (`ListAWSDefaultServiceQuotas` ServiceCode required -> `Quotas`: ServiceQuotaListDefinition; `ServiceQuota.QuotaCode` @length 1..128, `QuotaName` 'Specifies the quota name.', `Value` QuotaValue @range 0..10000000000, `Description` @length 0..350). The rule: for a `ServiceCode`, the row published under a `QuotaCode` carries the `QuotaName`, `Value`, `Unit`, `Adjustable`, `GlobalQuota`, `Period`, `UsageMetric`, `QuotaContext` and `Description` of the quota the service publishes UNDER THAT CODE, worded as the service words it; a quota whose code no source states is published under no code at all. The recorded `glue` response is the reference — e.g. L-FEBBFA7A is 'Max partitions per account', Value 20000000, Description 'The maximum number of partitions in your account.'; L-FB210BD9 is 'Global maximum paths', Value 30000, Description 'Catalog: global maximum paths'; L-83A59AA6 is 'Total concurrent machine learning task runs for transforms per account', Value 30; L-096DBB95 is 'Max spare compute capacity consumed in data processing units (DPUs) per account.', Value 4000.

Emulator: those codes carry the per-API request-rate rows instead — L-FEBBFA7A is served as 'Rate of UpdateJob requests' Value 10 with 'The maximum number of UpdateJob requests you can make per second in this account in the current Region.', L-FB210BD9 as 'Rate of UpdatePartition requests' Value 50, L-83A59AA6 as 'Rate of BatchCreatePartition requests' Value 50, L-096DBB95 as 'Table versions per table' Value 100000 — and the same three-member (QuotaName/Description/Value) swap repeats across L-071ABE08, L-4071B0E3, L-64A88F0A, L-76EC689B, L-785C1F39, L-7DD7C33A, L-83192DBF, L-87F5991C, L-94D025B7, L-A457CBAB, L-ACAC12B6, L-AD871090, L-B8497671, L-C3952F6C, L-CB69DFA0, L-D987EC31, L-DBA56E2F, L-EAC30D24, L-EEC98450, L-F1AAB534, L-F545C262, L-F574AED9, L-F953935E. Codes L-2E9FB93F and L-337244C9 are absent from the response entirely, and L-5E4153CA is missing.

Why: `QuotaCode` is the stable key every client uses — GetServiceQuota, GetAWSDefaultServiceQuota and RequestServiceQuotaIncrease all address a quota by it. A client that reads the Glue partition ceiling from L-FEBBFA7A gets a 10-requests-per-second API rate instead of 20,000,000 partitions, so capacity planning and any increase request are aimed at the wrong quota; two codes it may hold cannot be resolved at all.

Fix: the service-quotas default-quota catalog data for `glue` in the ListAWSDefaultServiceQuotas handler's catalog (the code->row binding, not the row serializer, which is otherwise shape-correct).

RELATED OPEN ISSUE. `20261121T120000Z-service-quotas-listawsdefaultservicequotas-residual-misbound-glue-rows.md` (Issue-Source: fix-residue) describes this same `glue` cluster and notes the recorded rows are needed to close it. The replay report nevertheless reports this cluster with an EMPTY `owned_by` — no open issue carries its replay key as a trace-sweep divergence — so it is filed here with the verbatim fixture/step linkage so a fixer's re-replay is gated on it. Treat the two as the same root cause; the recorded `glue` response remains the only source for the row content.

## Evidence
Replay of trace set simparity-wire-aws-v1 against the emulator built from this checkout (249 fixtures replayed, 227 steps mismatched, score 15.9%). Cluster: fixture service-quotas-catalog-list-aws-default-service-quotas-glue, step task#1, operation ListAWSDefaultServiceQuotas, verdict mismatch, expected_status 200, actual_status 200, stub_path "", owned_by [].

Quoted divergences (expected -> actual):
- `$.Quotas[QuotaCode="L-FEBBFA7A"].QuotaName`: 'Max partitions per account' -> 'Rate of UpdateJob requests'; `.Description`: 'The maximum number of partitions in your account.' -> 'The maximum number of UpdateJob requests you can make per second in this account in the current Region.'; `.Value`: 20000000.0 -> 10.0
- `$.Quotas[QuotaCode="L-FB210BD9"].QuotaName`: 'Global maximum paths' -> 'Rate of UpdatePartition requests'; `.Description`: 'Catalog: global maximum paths' -> 'The maximum number of UpdatePartition requests you can make per second in this account in the current Region.'; `.Value`: 30000.0 -> 50.0
- `$.Quotas[QuotaCode="L-83A59AA6"].QuotaName`: 'Total concurrent machine learning task runs for transforms per account' -> 'Rate of BatchCreatePartition requests'; `.Description`: 'The total number of concurrent machine learning transform task runs for machine learning transforms for this account.' -> 'The maximum number of BatchCreatePartition requests you can make per second in this account in the current Region.'; `.Value`: 30.0 -> 50.0
- `$.Quotas[QuotaCode="L-096DBB95"].QuotaName`: 'Max spare compute capacity consumed in data processing units (DPUs) per account.' -> 'Table versions per table'; `.Description`: 'The maximum spare compute capacity in data processing units (DPUs) you can use concurrently in your account.' -> 'The maximum number of table versions per table.'; `.Value`: 4000.0 -> 100000.0
- the same QuotaName/Description/Value swap on L-071ABE08, L-4071B0E3, L-76EC689B, L-7DD7C33A, L-83192DBF, L-87F5991C, L-94D025B7, L-A457CBAB, L-ACAC12B6, L-AD871090, L-B8497671, L-CB69DFA0, L-DBA56E2F, L-EAC30D24, L-F1AAB534, L-F545C262, L-F574AED9, L-F953935E; QuotaName/Description only on L-64A88F0A, L-785C1F39, L-C3952F6C, L-D987EC31, L-EEC98450
- `$.Quotas[QuotaCode="L-2E9FB93F"]`, `$.Quotas[QuotaCode="L-337244C9"]`, `$.Quotas[QuotaCode="L-5E4153CA"]`: missing (a recorded row exists for each, got None)

Model evidence: Smithy `ListAWSDefaultServiceQuotas` input ServiceCode required @length 1..63 @pattern ^[a-zA-Z][a-zA-Z0-9-]{1,63}$, output Quotas -> ServiceQuotaListDefinition, paginated items=Quotas; `ServiceQuota.QuotaCode` @length 1..128, `Value` @range 0..10000000000, `Description` @length 0..350. The shape is satisfied; only the code-to-row binding diverges.

## Steps to Reproduce
1. Boot the emulator from this checkout and replay trace set simparity-wire-aws-v1, fixture service-quotas-catalog-list-aws-default-service-quotas-glue, step task#1 — it reports verdict mismatch with expected and actual status 200.
2. Equivalently POST the service-quotas endpoint with X-Amz-Target `ServiceQuotasV20190624.ListAWSDefaultServiceQuotas` and body {"ServiceCode": "glue"}.
3. Index `$.Quotas` by `QuotaCode` and compare each row to the recorded response row for the SAME `QuotaCode`.
4. The ServiceCode is clean when every row's QuotaName, Description, Value, Unit, Adjustable, GlobalQuota, Period, UsageMetric and QuotaContext match the recorded row under that same QuotaCode — in particular L-FEBBFA7A is the account partition ceiling and not an UpdateJob call rate — and no recorded QuotaCode (including L-2E9FB93F, L-337244C9 and L-5E4153CA) is absent.
