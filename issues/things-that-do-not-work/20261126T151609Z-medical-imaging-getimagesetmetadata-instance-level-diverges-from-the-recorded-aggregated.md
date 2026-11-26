Issue-Source: trace-sweep-divergence
Issue-Trace: medical-imaging-dicom-import-job-yields-searchable-image-set
Issue-Step: task#12
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: medical-imaging-dicom-import-job-yields-searchable-image-set
Issue-Shape-Id: com.amazonaws.medicalimaging#AHIGatewayService
Issue-Key: medical-imaging|GetImageSetMetadata|2xx->2xx|$.Study.Series.1.2.826.0.1.3680043.10.511.1.3.Instances.1.2.826.0.1.3680043.10.511.1.1.ChecksumType
Issue-Commit: 9aea35f6744522f24fe9957da7e0a3d1191f628b

# GetImageSetMetadata instance level diverges from the recorded aggregated-metadata document at ChecksumType

> _Note: this issue was identified against the code base at commit `9aea35f6744522f24fe9957da7e0a3d1191f628b`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetImageSetMetadata

## Description
Cause 1 of 1: the SOP-instance level of the aggregated DICOM metadata document that GetImageSetMetadata serves does not match the recorded AWS document at the instance's `ChecksumType` member, for an image set materialised by a DICOM import job.

AWS: recorded behavior only. The operation's output is an opaque streaming blob — Smithy: GetImageSetMetadata output `imageSetMetadataBlob` (ImageSetMetadataBlob, @httpPayload, @streaming, required), `contentType`/`contentEncoding` bound to Content-Type/Content-Encoding; the model (services/service-com-amazonaws-medicalimaging-ahigatewayservice/smithy-model/com-amazonaws-medicalimaging-ahigatewayservice.json) declares nothing about the document's internal members, so the recording is the only contract. The recorded 200 response for this import-job-created image set carries, at $.Study.Series.<seriesInstanceUID>.Instances.<sopInstanceUID>, an object whose `ChecksumType` member does NOT agree with what the emulator serves at the same path. Both status lines agree (200 expected, 200 served) and every other node of the document — SchemaVersion, DatastoreID, ImageSetID, Patient/Study/Series DICOM attributes and the per-frame ImageFrames entries including PixelDataChecksumFromBaseToFullResolution — matched, so this one member is the whole divergence.

Emulator: the instance builder unconditionally adds an instance-level `ChecksumType` whose value is the hard-coded constant PIXEL_DATA_CHECKSUM_TYPE = "CRC32" whenever any frame of that instance carries at least one checksum entry (checksums are produced only when the instance declares both Columns and Rows). It is emitted from the document builder in the GetImageSetMetadata handler, beside the stored transfer syntax, and the replay reports: `$.Study.Series.1.2.826.0.1.3680043.10.511.1.3.Instances.1.2.826.0.1.3680043.10.511.1.1.ChecksumType: value_mismatch`. The replay's expected/actual rendering for that leaf is unusable ("expected None, got None") because the path segments are DICOM UIDs that themselves contain dots, so the dotted path cannot be re-resolved on either side for display — the divergence verdict on the leaf is still the harness's finding.

Hypothesis: AWS's aggregated metadata document does not report a checksum ALGORITHM at the SOP-instance level at all — the per-frame `PixelDataChecksumFromBaseToFullResolution` entries (Width/Height/Checksum) are the only checksum reporting — so the emulator is adding an instance member AWS never serves (or, if AWS does report one, it does not name CRC32). Nothing in the model or the recording states the algorithm, so the emulator's choice of "CRC32" is an invention of this crate rather than a recorded value.

Why: a client that parses the aggregated metadata document — the documented workflow is `aws medical-imaging get-image-set-metadata … studymetadata.json.gz` then gunzip and read — sees an instance attribute that does not exist in AWS output, or sees an algorithm name AWS does not use. Code that enumerates the instance object's members (schema validation, strict deserialisation into a generated model, or a diff of two metadata documents) rejects or mis-reads the emulator's document, and anything that trusts `ChecksumType` to interpret the frame checksums is told an algorithm AWS never named.

Fix: the SOP-instance node builder of the aggregated-metadata document in the GetImageSetMetadata handler (services/service-com-amazonaws-medicalimaging-ahigatewayservice/src/ops/get_image_set_metadata.rs, the instances builder that inserts "ImageFrames" and then "ChecksumType" under the `checksummed` flag) must emit the instance level exactly as the recorded document shapes it; the algorithm constant it reads lives in services/service-com-amazonaws-medicalimaging-ahigatewayservice/src/state/dicom_instance.rs (PIXEL_DATA_CHECKSUM_TYPE), which is where the name is chosen if the member is kept at all.

Note: the two open medical-imaging issues (SearchImageSets enum-value refusal wording; cloudformation AWS::HealthImaging::Datastore resource type) are unrelated and do not cover this document-shape divergence.

## Evidence
Replay of fixture `medical-imaging-dicom-import-job-yields-searchable-image-set`, step `task#12`, operation GetImageSetMetadata (service medical-imaging), verdict `mismatch`:
  expected_status 200, actual_status 200
  detail: "$.Study.Series.1.2.826.0.1.3680043.10.511.1.3.Instances.1.2.826.0.1.3680043.10.511.1.1.ChecksumType: value_mismatch (expected None, got None)"
  divergence_key: service=medical-imaging|GetImageSetMetadata|status=200->200|keys=$.Study.Series.…Instances.….ChecksumType
  affected (1): medical-imaging-dicom-import-job-yields-searchable-image-set#task#12

Expected: the decompressed aggregated metadata document's SOP-instance object agrees with the recorded AWS document at `ChecksumType` (per the recording, that member is not served the way the emulator serves it). The series UID and SOP instance UID above are values of this one recording, not part of the requirement — the requirement is the instance object's member set and values at whatever UIDs the imported study carries.

Actual: the emulator's document inserts instance member "ChecksumType" with the fixed string "CRC32" (PIXEL_DATA_CHECKSUM_TYPE) for every instance that has at least one frame checksum. Fixed text here is the member name `ChecksumType` and the literal `CRC32`; neither is taken from the request or from state, so neither is a per-run value.

All other 37 steps of the corpus passed; score 97.4%, 1 diverged step, no blocked or errored steps, no stub and no owning issue for this cluster.

## Steps to Reproduce
1. Replay fixture `medical-imaging-dicom-import-job-yields-searchable-image-set` against the emulator (the fixture creates a data store, starts a DICOM import job, lets it complete and searches the resulting image set).
2. At step `task#12` the client calls POST /datastore/{datastoreId}/imageSet/{imageSetId}/getImageSetMetadata (GetImageSetMetadata) for the image set the import produced.
3. gunzip the response payload (contentEncoding: gzip) and read the JSON document.
4. Walk to $.Study.Series.<seriesInstanceUID>.Instances.<sopInstanceUID> and compare its members with the recorded document: the emulator's object carries "ChecksumType": "CRC32", which the recorded AWS object does not match.
