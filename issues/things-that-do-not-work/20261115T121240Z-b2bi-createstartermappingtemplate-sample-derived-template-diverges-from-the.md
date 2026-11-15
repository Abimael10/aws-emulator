Issue-Source: trace-sweep-divergence
Issue-Trace: b2bi-mapping-template-and-test-mapping
Issue-Step: task#1
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: b2bi-mapping-template-and-test-mapping
Issue-Shape-Id: com.amazonaws.b2bi#B2BI
Issue-Key: b2bi|CreateStarterMappingTemplate|2xx->2xx|$.mappingTemplate
Issue-Commit: e21d6e6f6ee0013c77eca32e21a13d15f059c07a

# b2bi CreateStarterMappingTemplate: sample-derived template diverges from the starter template AWS returns

> _Note: this issue was identified against the code base at commit `e21d6e6f6ee0013c77eca32e21a13d15f059c07a`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateStarterMappingTemplate

## Description
One cause, affecting both CreateStarterMappingTemplate steps of the fixture (task#1 and task#2, one divergence cluster, keys=$.mappingTemplate, status 200 -> 200).

Cause 1 of 1: when the request names an `outputSampleLocation`, the emulator answers with the SAMPLE's parsed document instead of the starter template AWS generates for the requested X12 transaction set and version.

AWS: Smithy: CreateStarterMappingTemplate (POST /createmappingstarttemplate, awsJson RPC, success 200), input `templateDetails` (required, union TemplateDetails -> X12Details{transactionSet, version}) + `mappingType` (required, enum MappingType) + `outputSampleLocation` (S3Location, documented as "Specify the location of the sample EDI file that is used to generate the mapping template"); output `mappingTemplate` (required, String). The model says nothing further about the template's content, so for the content this is recorded behavior only. The recording shows the template AWS returns is one X12 envelope tree (`interchanges[0].functional_groups[0].transactions[0].segments[]`) in which:
  * the ENVELOPE elements carry values taken from the named sample, keyed `<SEGMENT>_<position>_<ElementName>` — e.g. `ISA_01_AuthorizationQualifier`, `ISA_05_SenderQualifier`, `ISA_13_InterchangeControlNumber`, `GS_01_FunctionalIdentifierCode` .. `GS_08_Version`, `ST_01_TransactionSetIdentifierCode`, `ST_02_TransactionSetControlNumber` (the particular codes, ids, dates, times and control numbers in the recording are that sample's values, not a requirement);
  * each segment INSIDE the transaction is an object holding EVERY element position of that segment's X12 table, keyed `<SEGMENT>_<position>` (BEG_01 .. BEG_12, CUR_01 .. ), and the value of each such element is the single bare word `PLACEHOLDER` — it does NOT carry the sample's value for that field, and the segment object carries no reader-internal members and no loop wrappers.

Emulator: on the `outputSampleLocation` path the handler reads the sample and returns `sample_derived_document(parse_x12(sample))` — the X12 reader's own parsed document. That differs from the recorded template in two ways at once: (a) every transaction-set business field is rendered as `PLACEHOLDER: <the sample's value for that field>` (`SAMPLE_PLACEHOLDER_PREFIX = "PLACEHOLDER: "`), where AWS writes the bare word `PLACEHOLDER`; and (b) the body structure is the sample's parsed structure — only the element positions the sample happened to populate, plus the reader's own members (per-segment `segment_id` markers, detail-table loop wrappers such as `PO1-8000_loop`, and the envelope trailers SE/GE/IEA the reader read) — where AWS enumerates the requested transaction set's table positions and writes no reader artifacts. Replay reports `$.mappingTemplate: value_mismatch` on both steps of the fixture (the response status, 200, and every other member match).

Why: `mappingTemplate` is this operation's only output and is the document a caller edits into a JSONata mapping (or an XSLT stylesheet) and then hands to CreateTransformer/TestMapping. A client that relies on AWS's behavior gets a template whose placeholders are not the agreed sentinel word (so its own "replace every PLACEHOLDER" step leaves the sample's data embedded in the mapping and leaks sample content into generated EDI), that is missing the element positions the transaction set defines but the sample did not populate, and that contains structural members the mapping language is not meant to address. Diff-based or golden-file consumers of the starter template fail outright.

Fix: the sample-driven branch of the CreateStarterMappingTemplate handler — the template builder, not the X12 reader: build the same table-driven starter document the no-sample branch builds for the requested transactionSet/version, and use the sample only to fill the envelope element values; the placeholder value must be the bare sentinel, with no sample value appended, and no reader-internal members (segment ids, loop wrappers, envelope trailers) may reach the rendered template.

Traces: services/service-com-amazonaws-b2bi-b2bi/src/ops/create_starter_mapping_template.rs — `SAMPLE_PLACEHOLDER_PREFIX` (line 1113), `sample_derived_document` (line 1143), and the `Some(sample)` arm of `handle` (lines 1358-1393). Failing fixture steps: b2bi-mapping-template-and-test-mapping#task#1 and #task#2. (The two open b2bi issues — ListTransformers epoch timestamps, and the CloudFormation AWS::B2BI::* resource types — are unrelated and do not cover this.)

## Evidence
Fixture b2bi-mapping-template-and-test-mapping, step task#1, operation CreateStarterMappingTemplate, verdict mismatch, expected_status 200, actual_status 200, divergence_key `service=b2bi|CreateStarterMappingTemplate|status=200->200|keys=$.mappingTemplate`, affected: task#1, task#2 (affected_count 2).

Replay detail: `$.mappingTemplate: value_mismatch`.

Expected (recorded) template, shape only — envelope elements hold the sample's values, body elements hold the bare sentinel:
  {
    "interchanges" : [ {
      "ISA_01_AuthorizationQualifier" : "00", ... "ISA_15_TestIndicator" : "P",
      "functional_groups" : [ {
        "GS_01_FunctionalIdentifierCode" : "PO", ... "GS_08_Version" : "004010",
        "transactions" : [ {
          "ST_01_TransactionSetIdentifierCode" : "850",
          "ST_02_TransactionSetControlNumber" : "000001",
          "segments" : [ { "BEG_01" : "PLACEHOLDER", ... "BEG_12" : "PLACEHOLDER" },
                          { "CUR_01" : "PLACEHOLDER", ... } ] } ] } ] } ] }
(the concrete ISA/GS/ST values above are that sample's data and are not themselves the requirement; the requirement is that they come from the named sample and that every transaction-set element is the bare word PLACEHOLDER over the full element list of the segment's X12 table.)

Actual: the emulator's sample-derived rendering, in which transaction-set fields read `PLACEHOLDER: <sample value>` and the body is the X12 reader's parsed structure (populated positions only, plus `segment_id` members, `<SEG>-<n>_loop` wrappers and the SE/GE/IEA trailers) rather than the transaction set's table positions.

## Steps to Reproduce
1. Replay fixture b2bi-mapping-template-and-test-mapping against the emulator (replay_wire_corpus).
2. Step task#1 POSTs CreateStarterMappingTemplate with templateDetails.x12 {transactionSet X12_850, version VERSION_4010}, a mappingType, and an outputSampleLocation naming a sample X12 850 interchange in the emulator's S3.
3. Compare $.mappingTemplate in the 200 response with the recorded template: the recorded one carries bare "PLACEHOLDER" values over the transaction set's full element list, the emulator's carries "PLACEHOLDER: <sample value>" over the sample's parsed structure.
4. Step task#2 (same operation, other mappingType) diverges on the same member for the same reason.
