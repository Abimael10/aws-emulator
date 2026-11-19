Issue-Source: trace-sweep-divergence
Issue-Trace: securityhub-catalog-list-security-control-definitions
Issue-Step: task#1
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: securityhub-catalog-list-security-control-definitions
Issue-Shape-Id: com.amazonaws.securityhub#SecurityHubAPIService
Issue-Key: securityhub|ListSecurityControlDefinitions|2xx->2xx|$.SecurityControlDefinitions[*]
Issue-Key: securityhub|ListSecurityControlDefinitions|2xx->2xx|$.SecurityControlDefinitions[*].RemediationUrl
Issue-Commit: c0e625426a17c83757c061123f0383276d45d724

# securityhub ListSecurityControlDefinitions serves a truncated, text-divergent control-definition catalog

> _Note: this issue was identified against the code base at commit `c0e625426a17c83757c061123f0383276d45d724`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ListSecurityControlDefinitions

## Description
Cause 1 of 1: GET /securityControls/definitions (ListSecurityControlDefinitions) answers 200 with a SecurityControlDefinitions list that is a small fraction of the real Security Hub control catalog, and the few definitions it does return carry Description and RemediationUrl values that do not match the service's own catalog text. Fixture securityhub-catalog-list-security-control-definitions, step task#1.

AWS: Smithy (com-amazonaws-securityhub-securityhubapiservice): ListSecurityControlDefinitions, http GET /securityControls/definitions, code 200, output ListSecurityControlDefinitionsResponse with required member SecurityControlDefinitions (list) plus NextToken; paginated {inputToken: NextToken, outputToken: NextToken, items: SecurityControlDefinitions, pageSize: MaxResults}, MaxResults @range min 1 max 100; errors InternalException (500), InvalidAccessException (401), InvalidInputException (400), LimitExceededException (429). The model does not enumerate the catalog contents, so the catalog itself is recorded behavior only: the recording for this step enumerates the full definition set — 710 SecurityControlDefinition entries — each with SecurityControlId, Title, Description, RemediationUrl, SeverityRating, CurrentRegionAvailability, CustomizableProperties and Provider, and the Description is the service's full control text (for ACM.3 it ends with the sentence 'System tags, which are automatically applied and begin with aws:, are ignored.' and spells contractions with the U+2019 right single quotation mark, e.g. 'doesn\u2019t'). The catalog text, the set of control ids and each control's RemediationUrl are service-owned data, identical on every call; they are not per-run values.

Emulator: the response is 200 with the right envelope shape, but the comparator reports every expected definition from index 2 through index 709 as absent — 708 of the 710 recorded definitions are not served at all (the actual list only has entries at roughly the first two dozen index positions). Of the handful it does serve, the content still diverges: $.SecurityControlDefinitions[2] (ACM.3) comes back with the Description truncated after "...fails if the certificate isn't tagged with any key." (the final 'System tags ... are ignored.' sentence is dropped) and with ASCII apostrophes where the service uses U+2019; $.SecurityControlDefinitions[3] (APIGateway.1) likewise mismatches on Description; and RemediationUrl diverges at indices 1, 3, 4, 5, 6, 7, 8, 9, 11, 12, 13, 20, 21 and 22. No error is returned and no NextToken continuation makes the missing entries reachable — the catalog simply is not there.

Why: ListSecurityControlDefinitions is how a client discovers which controls exist before calling GetSecurityControlDefinition, UpdateSecurityControl, BatchGetSecurityControls or any standards-enablement flow. A client that enumerates the catalog against the emulator sees a few dozen controls instead of the full set, so every lookup of a control the emulator omits (anything beyond the first page positions it happens to seed) finds nothing and the client concludes the control does not exist in the region; compliance tooling that diffs enabled controls against the catalog reports hundreds of phantom removals. Clients that surface Description/RemediationUrl to a user render truncated remediation guidance and the wrong documentation link. Because the status is 200 and the required member is present, nothing signals the truncation.

Fix: the ListSecurityControlDefinitions handler's seed data — the service's control-definition catalog in the securityhub crate's state/catalog layer that the handler pages over. The catalog must contain the full definition set (all 710 recorded SecurityControlIds), each entry must carry the service's verbatim Description text (including the trailing system-tags sentence and the U+2019 contractions) and its real RemediationUrl, and the handler must page that full set through NextToken honouring MaxResults @range 1..100 rather than returning a short fixed list.

Not covered by any open issue: the open securityhub issues concern mutation read-back (20261029T180317Z-securityhub-mutations-are-not-reflected-by-later-reads.md), two unimplemented operation stubs (EnableOrganizationAdminAccount, UpdateSecurityHubConfiguration), the unreachable boolean control-parameter round trip (20261118T061118Z), and two CloudFormation resource-type issues — none of them is about the content of the ListSecurityControlDefinitions catalog.

## Evidence
Fixture securityhub-catalog-list-security-control-definitions, step task#1, operation ListSecurityControlDefinitions, verdict mismatch, expected_status 200, actual_status 200.

Expected (recorded): 710 entries under $.SecurityControlDefinitions. Example $.SecurityControlDefinitions[2]:
  {'SecurityControlId': 'ACM.3', 'Title': 'ACM certificates should be tagged', 'Description': "This control checks whether an AWS Certificate Manager (ACM) certificate has tags with the specific keys defined in the parameter requiredTagKeys. The control fails if the certificate doesn\u2019t have any tag keys or if it doesn\u2019t have all the keys specified in the parameter requiredTagKeys. If the parameter requiredTagKeys isn't provided, the control only checks for the existence of a tag key and fails if the certificate isn't tagged with any key. System tags, which are automatically applied and begin with aws:, are ignored.", 'RemediationUrl': 'https://docs.aws.amazon.com/acm/latest/userguide/tags.html', 'SeverityRating': 'LOW', 'CurrentRegionAvailability': 'AVAILABLE', 'CustomizableProperties': ['Parameters'], 'Provider': 'AWS'}

Actual (emulator) for the same control id: identical except Description = "This control checks whether an AWS Certificate Manager (ACM) certificate has tags with the specific keys defined in the parameter requiredTagKeys. The control fails if the certificate doesn't have any tag keys or if it doesn't have all the keys specified in the parameter requiredTagKeys. If the parameter requiredTagKeys isn't provided, the control only checks for the existence of a tag key and fails if the certificate isn't tagged with any key." — the trailing 'System tags, which are automatically applied and begin with aws:, are ignored.' sentence is missing and the U+2019 apostrophes are ASCII.

Diverging JSON paths reported by the replay (cluster key service=securityhub|ListSecurityControlDefinitions|status=200->200): $.SecurityControlDefinitions[2] .. $.SecurityControlDefinitions[709] reported 'missing' (708 expected definitions absent), plus member-level mismatches $.SecurityControlDefinitions[1].RemediationUrl, [3].RemediationUrl, [4].RemediationUrl, [5].RemediationUrl, [6].RemediationUrl, [7].RemediationUrl, [8].RemediationUrl, [9].RemediationUrl, [11].RemediationUrl, [12].RemediationUrl, [13].RemediationUrl, [20].RemediationUrl, [21].RemediationUrl, [22].RemediationUrl, and $.SecurityControlDefinitions[3] (APIGateway.1, 'API Gateway REST and WebSocket API execution logging should be enabled') reported missing on the same Description grounds.

Fixed vs variable text: the Description, Title, RemediationUrl, SeverityRating and Provider of a control are fixed service-owned text keyed by SecurityControlId — none of them is a per-run value. No request id, timestamp, host or generated id is involved in this divergence.

## Steps to Reproduce
1. Boot the emulator from this checkout and replay fixture securityhub-catalog-list-security-control-definitions.
2. At step task#1 the fixture issues GET /securityControls/definitions (ListSecurityControlDefinitions) with no StandardsArn/Providers filter.
3. Compare the returned SecurityControlDefinitions list with the recorded one: count the entries (recorded 710, emulator serves only the first ~24 index positions) and follow NextToken to exhaustion — the remaining definitions are never returned.
4. Locate the ACM.3 entry in the response and compare its Description with the recorded text: the emulator drops the trailing 'System tags, which are automatically applied and begin with aws:, are ignored.' sentence and uses ASCII apostrophes instead of U+2019.
5. Compare RemediationUrl on the entries at index positions 1, 3-9, 11-13 and 20-22 with the recorded values; they differ.
