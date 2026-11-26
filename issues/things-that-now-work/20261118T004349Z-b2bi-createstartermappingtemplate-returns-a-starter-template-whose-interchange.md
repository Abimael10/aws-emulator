Issue-Source: trace-sweep-divergence
Issue-Trace: b2bi-mapping-template-and-test-mapping
Issue-Step: task#1
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: b2bi-mapping-template-and-test-mapping
Issue-Shape-Id: com.amazonaws.b2bi#B2BI
Issue-Key: b2bi|CreateStarterMappingTemplate|2xx->2xx|$.mappingTemplate
Issue-Commit: 8b466838d4bdc1ab2a4a37a0f7b7d85466ef4e9d

# b2bi CreateStarterMappingTemplate returns a starter template whose interchange-header element keys do not match AWS

> _Note: this issue was identified against the code base at commit `8b466838d4bdc1ab2a4a37a0f7b7d85466ef4e9d`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateStarterMappingTemplate

## Description
Cause 1 of 1: the `mappingTemplate` string CreateStarterMappingTemplate generates for an X12 850 / VERSION_4010 request (with `outputSampleLocation` naming a sample interchange) is not the document AWS generates — the two differ in the KEY the interchange control header's eleventh element is named by, and the divergence shows up on BOTH mapping types (JSONATA, step task#1, and XSLT, step task#2, are one cluster).

AWS: Smithy gives this operation input `mappingType` (MappingType enum), `templateDetails` (TemplateDetails.x12 transactionSet/version) and `outputSampleLocation` (S3Location), and output a single required `mappingTemplate` String with HTTP 200 (services/service-com-amazonaws-b2bi-b2bi/smithy-model) — the model says NOTHING about the element keys inside that string, so this is recorded behavior only. The recorded 200 response keys the interchange header's elements `<SEGMENT>_<two-digit position>_<ElementName>` with the short-form element names `ISA_01_AuthorizationQualifier`, `ISA_02_AuthorizationInformation`, `ISA_03_SecurityQualifier`, `ISA_04_SecurityInformation`, `ISA_05_SenderQualifier`, `ISA_06_SenderId`, `ISA_07_ReceiverQualifier`, `ISA_08_ReceiverId`, `ISA_09_Date`, `ISA_10_Time`, `ISA_11_StandardsId`, `ISA_12_Version`, `ISA_13_InterchangeControlNumber`, `ISA_14_AcknowledgmentRequested`, `ISA_15_TestIndicator`, then `functional_groups` (`GS_01_FunctionalIdentifierCode` .. `GS_08_Version`), `transactions` (`ST_01_TransactionSetIdentifierCode`, `ST_02_TransactionSetControlNumber`) and `segments` with every body element (`BEG_01`..`BEG_12`, `CUR_01`..`CUR_09`, ...) set to the fixed word `PLACEHOLDER`; the envelope values come from the sample the request names. Note the eleventh key: for a pre-4020 release AWS names it `StandardsId`, i.e. the same short-form rule that turns the X12 title "Interchange Sender ID" into `SenderId`.

Emulator: replay reports `$.mappingTemplate: value_mismatch` against that recorded template on a 200/200 response — the status, the error-free shape and every other key match, so the body is wrong, not the envelope of the call. Hypothesis for the attribution (the recorded template in the replay detail is the authority, and a fixer should diff the emulator's returned string against it rather than trust this one line): the generator takes ISA11's name from the X12 reader's pre-4020 constant `InterchangeControlStandardsId` and therefore emits the key `ISA_11_InterchangeControlStandardsId` where AWS emits `ISA_11_StandardsId`. That constant contradicts the short-form naming rule its own doc comment states, and because the reader and the template generator share one name table, a wrong entry there is emitted in the JSONATA document and in the XSLT stylesheet alike.

Why: a key in a starter template is the ADDRESS a caller's mapping expression uses. A client that takes the generated template, fills in its PLACEHOLDER fields and submits the result as a transformer's `mapping`/`mappingTemplate` writes an expression against a member name AWS does not use, so the interchange control standards identifier is read from or written to a member that does not exist: the mapping silently produces no value for ISA11 against real AWS, and a client that diffs the starter template it got here against the one AWS returns sees the two as different documents.

Fix: the CreateStarterMappingTemplate handler's envelope element-name table — the shared ISA element-name list the starter-template generator and this service's X12 reader both resolve per release — so the generated template, in both the JSONATA and the XSLT rendering, keys ISA11 exactly as AWS does for the requested release.

Traces: b2bi-mapping-template-and-test-mapping#task#1 (JSONATA) and b2bi-mapping-template-and-test-mapping#task#2 (XSLT), one cluster on key `$.mappingTemplate`. Code: services/service-com-amazonaws-b2bi-b2bi/src/ops/test_parsing.rs:316 (`ISA_ELEMENTS`), :342 (`ISA11_PRE_4020 = "InterchangeControlStandardsId"`), :501 (`isa_element_names`); services/service-com-amazonaws-b2bi-b2bi/src/ops/create_starter_mapping_template.rs:865 (`isa_elements`), :930, :1535 (`starter_document`), :1578 (render of both mapping types).

## Evidence
Step b2bi-mapping-template-and-test-mapping#task#1 (CreateStarterMappingTemplate, mappingType JSONATA, templateDetails.x12 transactionSet X12_850 / version VERSION_4010, with outputSampleLocation).

Expected (recorded, HTTP 200): mappingTemplate is a JSON document whose interchange header carries, in order, `ISA_01_AuthorizationQualifier` "00", `ISA_02_AuthorizationInformation` (ten spaces), `ISA_03_SecurityQualifier` "00", `ISA_04_SecurityInformation` (ten spaces), `ISA_05_SenderQualifier`, `ISA_06_SenderId`, `ISA_07_ReceiverQualifier`, `ISA_08_ReceiverId`, `ISA_09_Date`, `ISA_10_Time`, `ISA_11_StandardsId` "U", `ISA_12_Version`, `ISA_13_InterchangeControlNumber`, `ISA_14_AcknowledgmentRequested` "0", `ISA_15_TestIndicator` "P", then `functional_groups` -> `transactions` -> `segments` with every body element the word `PLACEHOLDER`. (The sender/receiver ids, dates, times and control numbers in the recording are the SAMPLE file's values, not requirements of this issue; the KEY NAMES and the `PLACEHOLDER` word are.)

Actual (emulator, HTTP 200): `$.mappingTemplate: value_mismatch` — the returned template is not that document. Divergence key `service=b2bi|CreateStarterMappingTemplate|status=200->200|keys=$.mappingTemplate`, affected steps task#1 (JSONATA) and task#2 (XSLT). Identified difference: the eleventh interchange-header key is emitted as `ISA_11_InterchangeControlStandardsId` instead of AWS's `ISA_11_StandardsId`.

## Steps to Reproduce
1. PutObject a sample X12 850 interchange declaring version 00401 in ISA12 into the emulator's S3.
2. Call b2bi CreateStarterMappingTemplate with mappingType=JSONATA, templateDetails={x12:{transactionSet:"X12_850", version:"VERSION_4010"}} and outputSampleLocation pointing at that object.
3. Compare the returned `mappingTemplate` with the recorded one: the interchange header's eleventh member is keyed `ISA_11_InterchangeControlStandardsId` where AWS keys it `ISA_11_StandardsId`.
4. Repeat with mappingType=XSLT — the same element name appears in the stylesheet.
5. Re-replay b2bi-mapping-template-and-test-mapping#task#1 and #task#2 to confirm the fix.
