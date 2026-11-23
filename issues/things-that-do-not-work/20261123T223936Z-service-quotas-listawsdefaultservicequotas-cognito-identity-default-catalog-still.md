Issue-Source: trace-sweep-divergence
Issue-Trace: service-quotas-catalog-list-aws-default-service-quotas-cognito-identity
Issue-Step: task#1
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: service-quotas-catalog-list-aws-default-service-quotas-cognito-identity
Issue-Shape-Id: com.amazonaws.servicequotas#ServiceQuotasV20190624
Issue-Key: service-quotas|ListAWSDefaultServiceQuotas|2xx->2xx|$.Quotas[QuotaCode="L-14403AD5"].Description
Issue-Key: service-quotas|ListAWSDefaultServiceQuotas|2xx->2xx|$.Quotas[QuotaCode="L-14403AD5"].QuotaName
Issue-Related: 20261111T131231Z-service-quotas-listawsdefaultservicequotas-default-catalog-incomplete-for-cognito-identity-cognito.md
Issue-Commit: 135485142286501fe8192681e8d56b16575ee550

# service-quotas ListAWSDefaultServiceQuotas cognito-identity default catalog still omits five rows and misbinds L-14403AD5

> _Note: this issue was identified against the code base at commit `135485142286501fe8192681e8d56b16575ee550`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ListAWSDefaultServiceQuotas

## Description
ListAWSDefaultServiceQuotas for ServiceCode 'cognito-identity' (Amazon Cognito Federated Identities) answers 200 with a Quotas list that is still not the AWS default-quota set for that service code: five AWS rows are absent entirely, and the one row that has since been added (L-14403AD5) is published under AWS's quota code but with a QuotaName and Description that are not AWS's pair for that quota.

Cause 1 of 1 - ServiceCode 'cognito-identity' default-quota rows absent or misbound.

AWS: Smithy (services/service-com-amazonaws-servicequotas-servicequotasv20190624/smithy-model/com-amazonaws-servicequotas-servicequotasv20190624.json): ListAWSDefaultServiceQuotas { ServiceCode (required, @length 1..63, @pattern ^[a-zA-Z][a-zA-Z0-9-]{1,63}$), NextToken (@length 0..2048), MaxResults (@range 1..100) } -> ListAWSDefaultServiceQuotasResponse { Quotas: list<ServiceQuota>, NextToken }, @paginated(items: Quotas); errors AccessDeniedException(403), IllegalArgumentException(400), InvalidPaginationTokenException(400), NoSuchResourceException(404), ServiceException(500), TooManyRequestsException(429). ServiceQuota { ServiceCode, ServiceName, QuotaArn, QuotaCode (@length 1..128, @pattern ^[a-zA-Z][a-zA-Z0-9-]{1,128}$), QuotaName, Value (double @range 0..10000000000), Unit, Adjustable (boolean, default false), GlobalQuota (boolean, default false), UsageMetric: MetricInfo, Period: QuotaPeriod, Description (@length 0..350) }. The model does NOT enumerate quota codes, names, values or text, so for the catalog's CONTENT this is recorded behavior only and the recording is the requirement: for a given ServiceCode, AWS returns one ServiceQuota per default quota of that service, keyed by AWS's own quota code of the form L-XXXXXXXX, each carrying AWS's QuotaName, Description, Unit, Value, Adjustable and GlobalQuota, with QuotaArn following the template arn:aws:servicequotas:<region>::<serviceCode>/<quotaCode> (the region in the recorded ARN is the recording environment's and is not itself a requirement; the template and the per-row pairing are). For cognito-identity the recording includes, among others, L-EAF710D2 (QuotaName 'List API call results', Description 'The maximum number of results from a list or lookup API call.', Adjustable false, GlobalQuota false, Unit 'None', Value 60.0), L-8692CE1C ('Identity pools per account', 'The maximum number of identity pools per account.', Adjustable true, Unit 'None', Value 1000.0), L-1F1BC060 ('Role-based access control rules', 'The maximum number of rules for role-based access control (RBAC)', Adjustable false, Unit 'None', Value 25.0), L-B08F14B9 ('Login provider name size', 'The maximum size of a login provider name in bytes.', Adjustable false, Unit 'Bytes', Value 2048.0), L-29CA570C ('The maximum size of an identity pool name in bytes.', Unit 'Bytes'), and L-14403AD5 with its own distinct QuotaName/Description pair. ServiceName on every row is 'Amazon Cognito Federated Identities'. Numeric limits, names and descriptions above are the recorded AWS catalog content for that quota code, not values of a single run; the region segment of the ARN is the only per-environment part.

Emulator: replay of this fixture step returns 200 with the Quotas list present and correctly shaped, but $.Quotas[QuotaCode="L-EAF710D2"], $.Quotas[QuotaCode="L-8692CE1C"], $.Quotas[QuotaCode="L-1F1BC060"], $.Quotas[QuotaCode="L-B08F14B9"] and $.Quotas[QuotaCode="L-29CA570C"] are each reported 'missing (expected {...}, got None)'. L-14403AD5 is now emitted, but diverges on two members, $.Quotas[QuotaCode="L-14403AD5"].QuotaName and $.Quotas[QuotaCode="L-14403AD5"].Description: the row carries a name and description that are not AWS's pair for that quota code (the catalog seed has bound another quota's text to it). No status or error-code divergence: expected_status 200, actual_status 200.

Why: a client enumerating Amazon Cognito Federated Identities limits receives a successful 200 that silently under-reports the service. Five quota codes it needs to pass to GetAWSDefaultServiceQuota / GetServiceQuota / RequestServiceQuotaIncrease do not exist in the response at all, so increase requests and limit dashboards for identity pools per account, RBAC rules, login-provider name size, identity-pool name size and list-API page size cannot be built. For L-14403AD5 the failure is worse than absence: the code resolves, so the client renders and reasons about a quota whose displayed name and description belong to a different limit, and any UI or policy keyed on that text is wrong with no error to detect it.

Fix: the service-quotas default-quota catalog seed data that the ListAWSDefaultServiceQuotas handler reads - specifically its cognito-identity rows. The operation's wire framing, serializer and pagination replay clean (status 200, Quotas list shape correct), so this is state/seed content, not the handler or serializer. The five absent rows must be added with AWS's QuotaName, Description, Unit, Value, Adjustable, GlobalQuota, ServiceName and ARN template, and L-14403AD5 must be re-bound to its own AWS QuotaName and Description rather than another quota's text; Description must be AWS's exact string for the quota, not a paraphrase.

Related open issue, which does NOT cover this cluster: 20261111T131231Z-service-quotas-listawsdefaultservicequotas-default-catalog-incomplete-for-cognito-identity-cognito.md names the same fixture step but is keyed on L-14403AD5 as a WHOLE-row omission ($.Quotas[QuotaCode="L-14403AD5"]). That row is now present and the divergence has moved to its .QuotaName and .Description members, so the replay reports this cluster with owned_by EMPTY and closing that issue does not close it.

Traces: service-quotas-catalog-list-aws-default-service-quotas-cognito-identity#task#1.

## Evidence
Replay of trace set simparity-wire-aws-v1 against the emulator built from this checkout: emulator_up true, bootstrap_signal false, 249 fixtures replayed, 270 steps total, 49 passed, 221 mismatched, 0 blocked, 0 errored, score_pct 18.1, 111 divergence clusters.

This cluster: fixture_id service-quotas-catalog-list-aws-default-service-quotas-cognito-identity, step_key task#1, service service-quotas, operation ListAWSDefaultServiceQuotas, verdict 'mismatch', expected_status 200, actual_status 200, stub_path empty, owned_by empty, affected_count 1.

divergence_key (verbatim): service=service-quotas|ListAWSDefaultServiceQuotas|status=200->200|keys=$.Quotas[QuotaCode="L-14403AD5"].Description,$.Quotas[QuotaCode="L-14403AD5"].QuotaName,$.Quotas[QuotaCode="L-1F1BC060"],$.Quotas[QuotaCode="L-29CA570C"],$.Quotas[QuotaCode="L-8692CE1C"],$.Quotas[QuotaCode="L-B08F14B9"],$.Quotas[QuotaCode="L-EAF710D2"]

Expected vs actual, from the reported detail (verbatim excerpts):
- $.Quotas[QuotaCode="L-EAF710D2"]: missing (expected {'Adjustable': False, 'Description': 'The maximum number of results from a list or lookup API call.', 'GlobalQuota': False, 'QuotaArn': 'arn:aws:servicequotas:us-west-2::cognito-identity/L-EAF710D2', 'QuotaCode': 'L-EAF710D2', 'QuotaName': 'List API call results', 'ServiceCode': 'cognito-identity', 'ServiceName': 'Amazon Cognito Federated Identities', 'Unit': 'None', 'Value': 60.0}, got None)
- $.Quotas[QuotaCode="L-8692CE1C"]: missing (expected {'Adjustable': True, 'Description': 'The maximum number of identity pools per account.', 'GlobalQuota': False, 'QuotaArn': 'arn:aws:servicequotas:us-west-2::cognito-identity/L-8692CE1C', 'QuotaCode': 'L-8692CE1C', 'QuotaName': 'Identity pools per account', 'ServiceCode': 'cognito-identity', 'ServiceName': 'Amazon Cognito Federated Identities', 'Unit': 'None', 'Value': 1000.0}, got None)
- $.Quotas[QuotaCode="L-1F1BC060"]: missing (expected {'Adjustable': False, 'Description': 'The maximum number of rules for role-based access control (RBAC)', 'GlobalQuota': False, 'QuotaArn': 'arn:aws:servicequotas:us-west-2::cognito-identity/L-1F1BC060', 'QuotaCode': 'L-1F1BC060', 'QuotaName': 'Role-based access control rules', 'ServiceCode': 'cognito-identity', 'ServiceName': 'Amazon Cognito Federated Identities', 'Unit': 'None', 'Value': 25.0}, got None)
- $.Quotas[QuotaCode="L-B08F14B9"]: missing (expected {'Adjustable': False, 'Description': 'The maximum size of a login provider name in bytes.', 'GlobalQuota': False, 'QuotaArn': 'arn:aws:servicequotas:us-west-2::cognito-identity/L-B08F14B9', 'QuotaCode': 'L-B08F14B9', 'QuotaName': 'Login provider name size', 'ServiceCode': 'cognito-identity', 'ServiceName': 'Amazon Cognito Federated Identities', 'Unit': 'Bytes', 'Value': 2048.0}, got None)
- $.Quotas[QuotaCode="L-29CA570C"]: missing (expected {'Adjustable': False, 'Description': 'The maximum size of an identity pool name in bytes.', 'GlobalQuota': False, 'QuotaArn': 'arn:aws:servicequotas:us-west-2::cognito-identity/L-29CA570C', ...}, got None)
- $.Quotas[QuotaCode="L-14403AD5"].QuotaName and $.Quotas[QuotaCode="L-14403AD5"].Description: present but not the recorded AWS pair for that quota code (member-level divergence, which is why this cluster's key set no longer matches the open issue keyed on the whole L-14403AD5 row).

Fixed text vs variable parts: the per-row QuotaName, Description, Unit, Value, Adjustable, GlobalQuota and ServiceName above are the AWS catalog content for those quota codes and are the requirement. The only per-environment part of a row is the <region> segment of QuotaArn (arn:aws:servicequotas:<region>::cognito-identity/<quotaCode>); 'us-west-2' in the recorded ARNs is the recording environment's region, not a requirement. No request id, timestamp or generated id participates in this comparison.

## Steps to Reproduce
1. Build and boot the emulator from this checkout.
2. Replay wire fixture service-quotas-catalog-list-aws-default-service-quotas-cognito-identity from trace set simparity-wire-aws-v1, step task#1 - i.e. call service-quotas ListAWSDefaultServiceQuotas with ServiceCode 'cognito-identity' (no NextToken, no MaxResults) against the emulator's data listener.
3. The emulator answers 200. Index the returned Quotas list by QuotaCode and look up L-EAF710D2, L-8692CE1C, L-1F1BC060, L-B08F14B9 and L-29CA570C: each is absent (got None) where the recorded AWS response has a full ServiceQuota entry.
4. Look up L-14403AD5: the row is returned, but its QuotaName and Description are not AWS's pair for that quota code.
5. Compare the whole list against the recorded response for that fixture step: the divergence is confined to the body, status is 200 on both sides.
