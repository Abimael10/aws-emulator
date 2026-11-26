Issue-Source: trace-sweep-divergence
Issue-Trace: b2bi-mapping-template-and-test-mapping
Issue-Step: task#1
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: b2bi-mapping-template-and-test-mapping
Issue-Shape-Id: com.amazonaws.b2bi#B2BI
Issue-Key: b2bi|CreateStarterMappingTemplate|2xx->2xx|$.mappingTemplate
Issue-Commit: 9c31bf6152f020b6d0683107981107b2a4f16ac3

# b2bi CreateStarterMappingTemplate injects sample values into body-segment placeholders

> _Note: this issue was identified against the code base at commit `9c31bf6152f020b6d0683107981107b2a4f16ac3`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateStarterMappingTemplate

## Description
Cause 1 of 1: when CreateStarterMappingTemplate is called WITH `outputSampleLocation`, the emulator returns a `mappingTemplate` whose body-segment element values are not the ones AWS returns. AWS fills the X12 ENVELOPE levels (ISA / GS / ST) from the named sample but leaves every element of every BODY segment of the transaction set (BEG, CUR, ... for an 850) as the bare placeholder token; the emulator additionally rewrites each body element the sample carries a value at into `PLACEHOLDER: <sample value>`.

AWS: Smithy model `CreateStarterMappingTemplate` (POST /createmappingstarttemplate, awsJson RPC, SUCCESS 200) — input members `mappingType` (required, enum `MappingType`), `templateDetails` (required, union `TemplateDetails` with the single `x12` member), `outputSampleLocation` (`S3Location`); output member `mappingTemplate` (required, `String`); modelled errors `ValidationException` (400), `ResourceNotFoundException` (404), `AccessDeniedException` (403), `InternalServerException` (500). The model constrains only the shape of `mappingTemplate`, not its content, so the content rule here is recorded behavior only. The recorded 200 response for a request naming an X12 850 sample shows: envelope elements carrying the sample's own values (`ISA_01_AuthorizationQualifier` = the sample's qualifier, `ISA_05_SenderQualifier`/`ISA_06_SenderId`, `ISA_09_Date`/`ISA_10_Time`, `ISA_13_InterchangeControlNumber`, `GS_01_FunctionalIdentifierCode` .. `GS_08_Version`, `ST_01_TransactionSetIdentifierCode`, `ST_02_TransactionSetControlNumber`), and every element of the transaction's `segments` list (`BEG_01` .. `BEG_12`, `CUR_01` .. ) carrying the single fixed word `PLACEHOLDER` with nothing appended — including the BEG positions the sample demonstrably fills, since BEG is mandatory in an 850. The fixed text of a body placeholder is exactly `PLACEHOLDER`; it carries no value from the request, from the sample object or from state. (The concrete sender/receiver ids, dates and control numbers in the recording are the named sample's values, not part of the requirement: the requirement is 'envelope element = the value the sample holds at that position, body element = the bare placeholder'.)

Emulator: for the same request the emulator returns 200 with a `mappingTemplate` whose body segments diverge at `$.mappingTemplate`: `fill_body_from_sample` -> `fill_elements_from_sample` replaces a body element the sample holds a non-empty value at with `sampled_placeholder(value)` = the constant `SAMPLE_PLACEHOLDER_PREFIX` (`"PLACEHOLDER: "`) concatenated with the sample's value, so `BEG_01` reads `PLACEHOLDER: <sample BEG01>` instead of `PLACEHOLDER`. The envelope levels (filled by `fill_from_sample` over `ENVELOPE_LEVELS`) agree with AWS; only the body differs. Both the JSONATA and the XSLT rendering of the document are affected, which is why the cluster covers two steps of the one fixture (task#1 and task#2).

Why: the generated template is the document a caller edits into a real mapping. A body field whose value is `PLACEHOLDER: 00` rather than `PLACEHOLDER` is no longer the token AWS's own tooling and every caller greps for to find the fields it must fill, and a caller that renders the template unchanged emits the prefixed string as EDI element data. A client that diffs or templates against AWS's output, or that counts/locates placeholder fields, misreads every body element of the generated template.

Fix: the sample-fill path of the CreateStarterMappingTemplate handler — a sample must fill only the envelope levels, and a body-segment element must keep the bare placeholder regardless of what the sample holds at that position. Remove the sample-derived prefixing of body elements from the handler's template generation (see Traces for the file).

Traces: fixture b2bi-mapping-template-and-test-mapping, steps task#1 and task#2 (both $.mappingTemplate value_mismatch, expected 200 / actual 200). Code: services/service-com-amazonaws-b2bi-b2bi/src/ops/create_starter_mapping_template.rs — `SAMPLE_PLACEHOLDER_PREFIX` / `sampled_placeholder` (lines 269-274), `fill_body_from_sample` (line 1508), `fill_body_segment_from_sample` (line 1525), `fill_elements_from_sample` (line 1570).

Not covered by the other open b2bi issues: 20261125T125531Z-b2bi-createtransformer-mints-transformerid-with-prefix-t-instead.md (CreateTransformer id prefix) and 20261109T123922Z-b2bi-cloudformation-aws-b2bi-profile-transformer-capability-partnership.md (CloudFormation resource types).

## Evidence
Replay of fixture `b2bi-mapping-template-and-test-mapping`, step `task#1` (cluster also covers `task#2`):
- verdict: mismatch; expected_status 200, actual_status 200; diverging key `$.mappingTemplate`.
- detail: `$.mappingTemplate: value_mismatch`.
- Expected (recorded AWS 200 body, JSONATA rendering): `interchanges[0]` carries `ISA_01_AuthorizationQualifier` .. `ISA_15_TestIndicator` filled from the named sample, `functional_groups[0]` carries `GS_01_FunctionalIdentifierCode` .. `GS_08_Version` filled from the sample, `transactions[0]` carries `ST_01_TransactionSetIdentifierCode` and `ST_02_TransactionSetControlNumber`, and `segments` carries `{"BEG_01": "PLACEHOLDER", ... "BEG_12": "PLACEHOLDER"}`, `{"CUR_01": "PLACEHOLDER", ...}` — every body element the bare word `PLACEHOLDER`.
- Actual: same envelope, but body elements the sample fills are emitted as `PLACEHOLDER: <sample value>` (constant `SAMPLE_PLACEHOLDER_PREFIX = "PLACEHOLDER: "` + the sample's element value), so `$.mappingTemplate` is not byte-equal to AWS's template.
- Fixed vs variable text: `PLACEHOLDER` is fixed text with no variable part; the envelope element values are variable and come from the sample object named by `outputSampleLocation`.

## Steps to Reproduce
1. Boot the emulator and replay fixture `b2bi-mapping-template-and-test-mapping`.
2. Step `task#1` (and `task#2`) call b2bi `CreateStarterMappingTemplate` with `templateDetails.x12.transactionSet`/`version` set and an `outputSampleLocation` pointing at an X12 850 sample object served by the emulator's own S3.
3. Both answer 200. Compare the returned `mappingTemplate` with the recorded one: the ISA/GS/ST envelope members match, but each element of the objects in `transactions[0].segments` that the sample carries a value for (e.g. `BEG_01`) reads `PLACEHOLDER: <value>` instead of the recorded bare `PLACEHOLDER`.
