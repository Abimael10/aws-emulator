Issue-Source: trace-sweep-divergence
Issue-Trace: entityresolution-provider-services-catalog
Issue-Step: task#2
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: entityresolution-provider-services-catalog
Issue-Shape-Id: com.amazonaws.entityresolution#AWSVeniceService
Issue-Key: entityresolution|GetProviderService|2xx->2xx|$.providerEntityOutputDefinition.properties.entity.properties.RAMPID.description
Issue-Key: entityresolution|GetProviderService|2xx->2xx|$.providerEntityOutputDefinition.properties.entity.properties.RAMPID.pattern
Issue-Key: entityresolution|GetProviderService|2xx->2xx|$.providerJobConfiguration.properties.data_format.description
Issue-Key: entityresolution|GetProviderService|2xx->2xx|$.providerJobConfiguration.properties.include_source_id.description
Issue-Key: entityresolution|GetProviderService|2xx->2xx|$.providerJobConfiguration.properties.include_source_id.required
Issue-Key: entityresolution|GetProviderService|2xx->2xx|$.providerJobConfiguration.properties.prepare_job.description
Issue-Key: entityresolution|GetProviderService|2xx->2xx|$.providerJobConfiguration.properties.prepare_job.required
Issue-Commit: 28ce3fe4daf51367f42e2e94cdfa08ab90f413bb

# entityresolution GetProviderService definition documents carry synthesized property metadata instead of the catalog's own

> _Note: this issue was identified against the code base at commit `28ce3fe4daf51367f42e2e94cdfa08ab90f413bb`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetProviderService

## Description
One cause.

Cause 1 of 1: GetProviderService answers HTTP 200 with the provider service's Document members now populated (the earlier name-only-stub defect, filed as the now-resolved issue 20261103T143443Z-entityresolution-provider-service-catalog-is-incomplete-listproviderservices-omits, is gone), but the per-property metadata inside providerEntityOutputDefinition and providerJobConfiguration is LOCALLY SYNTHESIZED from the property name rather than served from the AWS-managed catalog: the descriptions are generic 'Providers <name>'-style sentences, the provider identifier's regex is a permissive catch-all instead of the provider's own prefix-anchored pattern, and the per-property `required` flag is omitted entirely.

AWS: Smithy model services/service-com-amazonaws-entityresolution-awsveniceservice/smithy-model/com-amazonaws-entityresolution-awsveniceservice.json - GetProviderService (GET /providerservices/{providerName}/{providerServiceName}, @readonly, http 200). Its output shape GetProviderServiceOutput declares providerEntityOutputDefinition as @required with target Document ('The definition of the provider entity output.') and providerJobConfiguration as an optional Document; errors AccessDeniedException(403), ResourceNotFoundException(404), ValidationException(400), ThrottlingException(429), InternalServerException(500). The model constrains the members' SHAPE only (free-form JSON documents), so the CONTENT is recorded behavior only. The recording shows these documents are JSON-Schema-shaped descriptors whose every property carries the AWS-managed catalog's own metadata, verbatim, per provider service:
  * providerEntityOutputDefinition.properties.entity.properties.<ID>.description is the provider's prose definition of its identifier - product-specific text naming the provider and its graph, not a generated sentence.
  * providerEntityOutputDefinition.properties.entity.properties.<ID>.pattern is the provider's OWN identifier regex, anchored on the set of literal prefixes that provider's ids can start with, followed by the id's allowed character class. Fixed text: the alternation of literal prefixes and the trailing character class; the particular prefixes and class belong to that catalog entry.
  * providerJobConfiguration.properties.<knob>.description is the catalog's prose for that job knob (what it does), phrased in the imperative/descriptive form the catalog uses.
  * providerJobConfiguration.properties.<knob>.required is a BOOLEAN present on each knob (false for optional knobs), telling a caller whether the knob must appear in a providerProperties payload. The key is present, not absent.

Emulator: answers 200 with the right members and the right property names, but fills the metadata itself. Replay reports, verbatim: `$.providerEntityOutputDefinition.properties.entity.properties.RAMPID.description: value_mismatch (expected "LiveRamp's universal identifier that is tied to devices in the LiveRamp Identity Graph", got 'Providers RAMPID')`; `$.providerEntityOutputDefinition.properties.entity.properties.RAMPID.pattern: value_mismatch (expected '(^XY|^Xi|^Xc|^Xm|^hY)[a-zA-Z0-9_-]*$', got '^[a-zA-Z0-9_-]+$')`; `$.providerJobConfiguration.properties.data_format.description: value_mismatch (expected 'Data format expected by Provider', got 'Providers data format')`; `$.providerJobConfiguration.properties.prepare_job.description: value_mismatch (expected 'Prepare a job before starting it', got 'Whether the providers job prepares the input data before it is resolved')`; `$.providerJobConfiguration.properties.prepare_job.required: missing (expected False, got None)`; `$.providerJobConfiguration.properties.include_source_id.description: value_mismatch (expected 'Include UNIQUE_ID in the mappings for creating a job', got 'Whether the providers output carries the source id of each input record')`; `$.providerJobConfiguration.properties.include_source_id.required: missing (expected False, got None)`. The pattern of the three description mismatches is the same in every case: a template built from the property name ('Providers <name>' / 'Whether the providers ...') replacing the catalog's text. The identifier pattern is generalized to 'any of the allowed characters, one or more' - it drops the prefix alternation and so accepts ids the provider never mints.

Why: these documents are the only machine-readable contract a client has for a provider-based workflow. A client that validates a candidate identifier against providerEntityOutputDefinition...pattern before calling CreateIdMappingWorkflow/StartIdMappingJob accepts ids AWS's pattern rejects (the emulator's regex matches any alphanumeric/underscore/hyphen string, AWS's requires one of the provider's literal id prefixes), so the client passes validation locally and is refused by the real provider - and a negative test that asserts a bad id IS rejected passes on AWS and fails on the emulator. The absent `required` key makes every job knob look unspecified: a client that reads `required` to decide which providerProperties keys it must send (or a schema validator that walks the document) either crashes on the missing key or treats optional knobs as unknown, and generated UIs/forms render no requiredness at all. The templated descriptions are what a console or SDK help surface shows the operator, so the operator is told something about the knob that the provider did not say ('prepare_job' in particular is described as a statement about input data rather than the catalog's instruction).

Fix: the entityresolution service crate's provider-service catalog seed data and the GetProviderService read path that assembles the Document members. The handler must return the stored catalog descriptor for the requested providerName/providerServiceName byte-for-byte - every property's description, pattern, length bounds and `required` flag as the catalog holds them - instead of synthesizing any field from the property's name or substituting a generic character-class regex. No per-property metadata may be generated, and no key present in the catalog descriptor (notably `required`) may be dropped on serialization.

Traces: entityresolution-provider-services-catalog#task#2 (GetProviderService on the provider service whose output identifier is RAMPID and whose job configuration carries data_format/prepare_job/include_source_id).

## Evidence
Replay of trace set simparity-wire-aws-v1 against the emulator built from this checkout: emulator_up true, bootstrap_signal false, 9 fixtures replayed, 77 steps, 72 passed, 5 mismatched, score 93.5%, 3 divergence clusters. This cluster: fixture entityresolution-provider-services-catalog, step task#2, service entityresolution, operation GetProviderService, verdict mismatch, expected_status 200, actual_status 200, stub_path empty, owned_by empty, affected_count 1.

Divergence detail verbatim:
  $.providerEntityOutputDefinition.properties.entity.properties.RAMPID.description: value_mismatch (expected "LiveRamp's universal identifier that is tied to devices in the LiveRamp Identity Graph", got 'Providers RAMPID')
  $.providerEntityOutputDefinition.properties.entity.properties.RAMPID.pattern: value_mismatch (expected '(^XY|^Xi|^Xc|^Xm|^hY)[a-zA-Z0-9_-]*$', got '^[a-zA-Z0-9_-]+$')
  $.providerJobConfiguration.properties.data_format.description: value_mismatch (expected 'Data format expected by Provider', got 'Providers data format')
  $.providerJobConfiguration.properties.prepare_job.description: value_mismatch (expected 'Prepare a job before starting it', got 'Whether the providers job prepares the input data before it is resolved')
  $.providerJobConfiguration.properties.prepare_job.required: missing (expected False, got None)
  $.providerJobConfiguration.properties.include_source_id.description: value_mismatch (expected 'Include UNIQUE_ID in the mappings for creating a job', got 'Whether the providers output carries the source id of each input record')
  $.providerJobConfiguration.properties.include_source_id.required: missing (expected False, got None)

divergence_key: service=entityresolution|GetProviderService|status=200->200|keys=$.providerEntityOutputDefinition.properties.entity.properties.RAMPID.description,$.providerEntityOutputDefinition.properties.entity.properties.RAMPID.pattern,$.providerJobConfiguration.properties.data_format.description,$.providerJobConfiguration.properties.include_source_id.description,$.providerJobConfiguration.properties.include_source_id.required,$.providerJobConfiguration.properties.prepare_job.description,$.providerJobConfiguration.properties.prepare_job.required

Which parts are the requirement: the requirement is that each property's metadata is the catalog's own and that the `required` key is present with its boolean value. 'RAMPID', the LiveRamp prose, the prefix alternation (^XY|^Xi|^Xc|^Xm|^hY) and the knob names data_format/prepare_job/include_source_id are THIS catalog entry's values - the general rule is that the served descriptor equals the stored catalog descriptor for whichever provider service is addressed, with no name-derived substitution and no omitted keys. Nothing in this cluster is a per-run value: there are no ids, timestamps, hosts or request ids among the diverging fields, so every one of them is reproducible.

Model evidence (smithy_query on services/service-com-amazonaws-entityresolution-awsveniceservice/smithy-model/com-amazonaws-entityresolution-awsveniceservice.json): GetProviderService output providerEntityOutputDefinition @required target Document, doc 'The definition of the provider entity output.'; providerJobConfiguration optional target Document; providerConfigurationDefinition optional Document; providerEndpointConfiguration @required. Document content is unconstrained by the model, hence 'recorded behavior only' for the values above.

Relation to existing issues: issue 20261103T143443Z-entityresolution-provider-service-catalog-is-incomplete-listproviderservices-omits (status FIXED) covered the earlier, coarser defect where these same Document members were served with name-only property stubs and ListProviderServices was short; it is resolved and does not cover this residual metadata-fidelity divergence, which replay reports with owned_by empty. Open entityresolution issues 20261108T141905Z (matchId format) and 20261109T154551Z (GetMatchingJob outputSourceConfig) touch other operations.

## Steps to Reproduce
1. git_clone the repo and boot/build the emulator from that checkout (bootstrap_signal was false and 72 of 77 steps passed, so entityresolution is serving).
2. Replay the wire corpus simparity-wire-aws-v1 and look at the entityresolution GetProviderService cluster (fixture entityresolution-provider-services-catalog, step task#2).
3. Equivalently by hand: GET /providerservices/{providerName}/{providerServiceName} for the LiveRamp provider service exercised by that step (aws entityresolution get-provider-service --provider-name <name> --provider-service-name <arn>). The response is 200.
4. Walk providerEntityOutputDefinition.properties.entity.properties.<ID>: its `description` is a generated 'Providers <ID>' string rather than the catalog's prose, and its `pattern` is the catch-all '^[a-zA-Z0-9_-]+$' rather than the provider's prefix-anchored regex. Verify the looseness: an identifier that does not start with one of the provider's literal prefixes matches the emulator's pattern and not AWS's.
5. Walk providerJobConfiguration.properties: each knob's `description` is a name-derived sentence rather than the catalog's text, and the `required` key is absent entirely on prepare_job and include_source_id (AWS serves `required: false`).
6. Re-replay entityresolution-provider-services-catalog#task#2 to confirm a fix.
