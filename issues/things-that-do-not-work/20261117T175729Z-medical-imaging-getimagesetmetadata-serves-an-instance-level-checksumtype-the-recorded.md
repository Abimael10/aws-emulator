Issue-Source: trace-sweep-divergence
Issue-Trace: medical-imaging-dicom-import-job-yields-searchable-image-set
Issue-Step: task#12
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: medical-imaging-dicom-import-job-yields-searchable-image-set
Issue-Shape-Id: com.amazonaws.medicalimaging#AHIGatewayService
Issue-Key: medical-imaging|GetImageSetMetadata|2xx->2xx|$.Study.Series.1.2.826.0.1.3680043.10.511.1.3.Instances.1.2.826.0.1.3680043.10.511.1.1.ChecksumType
Issue-Commit: 8ad4ab4e1e6876e6dc935046a168f0e0d43fb6bd

# GetImageSetMetadata serves an instance-level ChecksumType the recorded aggregated metadata document does not carry at that path

> _Note: this issue was identified against the code base at commit `8ad4ab4e1e6876e6dc935046a168f0e0d43fb6bd`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetImageSetMetadata

## Description
Cause 1 of 1: the SOP-instance level of the aggregated DICOM metadata document GetImageSetMetadata serves diverges from the recorded document at the key `ChecksumType`, under $.Study.Series.<SeriesInstanceUID>.Instances.<SOPInstanceUID>.

AWS: recorded behavior only. The Smithy model says nothing about the document's shape — GetImageSetMetadata's output is `imageSetMetadataBlob` (ImageSetMetadataBlob, @httpPayload, @streaming, blob, required) plus the `contentType`/`contentEncoding` headers (smithy_query operation=GetImageSetMetadata on services/service-com-amazonaws-medicalimaging-ahigatewayservice/smithy-model/com-amazonaws-medicalimaging-ahigatewayservice.json), so every key inside the blob is unmodelled and the recording is the only authority for it. The recorded AWS document for this image set does not present the same value at the instance-level `ChecksumType` path that the emulator serves there: AWS reports what it computed over stored pixel data per FRAME, under each `ImageFrames[].PixelDataChecksumFromBaseToFullResolution` entry (`Width`/`Height`/`Checksum`), and the recorded instance object does not match the emulator at the sibling `ChecksumType` key. Status and all other keys of the document agree (expected 200, actual 200; this is the only diverging key in 38 replayed steps).

Emulator: for every SOP instance that has at least one frame carrying a checksum, the handler unconditionally inserts an instance-level `"ChecksumType"` whose value is the crate's own algorithm constant `CRC32` — a value chosen by the emulator because it checksums frame bytes with crc32fast, not a value taken from imported DICOM content or from anything AWS served. Replay reports: fixture medical-imaging-dicom-import-job-yields-searchable-image-set, step task#12, verdict mismatch, detail "$.Study.Series.1.2.826.0.1.3680043.10.511.1.3.Instances.1.2.826.0.1.3680043.10.511.1.1.ChecksumType: value_mismatch" (the replay's own expected/actual rendering resolves neither side, because the JSON path it prints walks dot-separated segments and both the Series and the SOP Instance UID keys contain dots; the mismatch itself is on that key).

Why: GetImageSetMetadata's blob is the only way a client reads an image set's DICOM content, and clients parse the instance level to decide how to verify pixel data they fetch with GetImageFrame. An instance-level key whose presence or value differs from AWS makes a client either read a checksum algorithm AWS never named for that instance, or fail a strict/schema-validating parse of the document, while the same code works against real HealthImaging. Hypothesis: the emulator introduced `ChecksumType` to name the algorithm behind the frame checksums it synthesizes, and AWS instead carries that information only inside the per-frame `PixelDataChecksumFromBaseToFullResolution` entries, so the key is emulator-invented at that level.

Fix: the GetImageSetMetadata handler's document builder — the instance-level assembly that adds `ChecksumType` beside `ImageFrames`, and the algorithm constant it reads — must report at the instance level only keys the recorded document carries there, with the recorded value. Nothing about the frame-level checksum entries, the status, the content type or the content encoding needs to change.

Traces: medical-imaging-dicom-import-job-yields-searchable-image-set#task#12 (GetImageSetMetadata, mismatch, expected 200 / actual 200, diverging key $.Study.Series.1.2.826.0.1.3680043.10.511.1.3.Instances.1.2.826.0.1.3680043.10.511.1.1.ChecksumType). Code: services/service-com-amazonaws-medicalimaging-ahigatewayservice/src/ops/get_image_set_metadata.rs:343-354 (inserts "ChecksumType" when any frame is checksummed) and services/service-com-amazonaws-medicalimaging-ahigatewayservice/src/state/dicom_instance.rs:35 (PIXEL_DATA_CHECKSUM_TYPE = "CRC32"). In the series/UID path above, `Study`, `Series`, `Instances` and `ChecksumType` are fixed document keys; the two dotted UIDs are values from the imported DICOM content of the request, not part of the requirement. No currently open medical-imaging issue covers this document key: the open ones concern SearchImageSets enum refusals, StartDICOMImportJob DICOMTags validation, ListDatastores filter re-checking, CreateDatastore signing-region narrowing, the CloudFormation HealthImaging resource type and a CreateDatastore kmsKeyArn check.

## Evidence
Replay of fixture `medical-imaging-dicom-import-job-yields-searchable-image-set`, step `task#12`, operation GetImageSetMetadata (service medical-imaging):
  expected_status 200, actual_status 200 — status and headers agree.
  verdict: mismatch; divergence_key service=medical-imaging|GetImageSetMetadata|status=200->200|keys=$.Study.Series.1.2.826.0.1.3680043.10.511.1.3.Instances.1.2.826.0.1.3680043.10.511.1.1.ChecksumType
  detail: "$.Study.Series.1.2.826.0.1.3680043.10.511.1.3.Instances.1.2.826.0.1.3680043.10.511.1.1.ChecksumType: value_mismatch (expected None, got None)" — the printed expected/actual are unresolved because the path's UID segments contain dots; the diverging key is the instance-level ChecksumType.
  Expected (recorded AWS): instance object whose checksum information is the per-frame ImageFrames[].PixelDataChecksumFromBaseToFullResolution entries, and which does not match the emulator at the instance-level ChecksumType key.
  Actual (emulator): instance object additionally carrying "ChecksumType": "CRC32", emitted for every instance with at least one checksummed frame (get_image_set_metadata.rs:343-354 reading PIXEL_DATA_CHECKSUM_TYPE from state/dicom_instance.rs:35).
  All 37 other steps of the corpus passed (score 97.4%), so this is an isolated document-shape divergence, not a transport or auth problem.

## Steps to Reproduce
1. Start the emulator from this checkout and point an AWS client at it.
2. CreateDatastore, then StartDICOMImportJob with a P10 instance that declares Rows/Columns (so the frame gets a checksum entry) and wait for the job to reach COMPLETED, as fixture medical-imaging-dicom-import-job-yields-searchable-image-set does.
3. SearchImageSets / GetImageSet to obtain the minted imageSetId.
4. POST /datastore/{datastoreId}/imageSet/{imageSetId}/getImageSetMetadata (GetImageSetMetadata), gunzip the returned blob and read $.Study.Series.<SeriesInstanceUID>.Instances.<SOPInstanceUID>.
5. Observe the instance object carries "ChecksumType": "CRC32" beside ImageFrames; the recorded AWS document for the same step does not present that value at that path. Equivalent: replay fixture medical-imaging-dicom-import-job-yields-searchable-image-set and inspect step task#12.
