// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for what the aggregated metadata document reports BELOW the series
//! level — the DICOM metadata of each SOP instance a DICOM import read, and what is
//! computed from the pixel data each of its frames holds.
//!
//! RULE 1: every attribute an imported instance carries is reported at the level it
//! describes, under its DICOM keyword, and typed as its value REPRESENTATION writes it: an
//! integer VR as a JSON number, a binary VR as base64 text, and every text VR — the
//! integer strings among them — as the data set wrote it. The attributes of the patient,
//! the study and the series are reported at those levels (the Series Number as the data set
//! wrote it, not as the modelled integer member parses it) and every other attribute of the
//! instance at the instance level, beside the value representation of each
//! (`DICOMVRs`), the transfer syntax the stored pixel data is served in
//! (`StoredTransferSyntaxUID`) and, for an instance holding pixel data, the checksum
//! algorithm those frames are summed with (`ChecksumType`).
//!
//! RULE 4: the instance level carries those members and no other — and the checksum type it
//! names is the algorithm the frame checksums beside it were REALLY computed with, so a
//! client can verify a frame's checksum from the document alone.
//! `the_instance_level_carries_the_members_the_document_has_and_names_its_checksum_type`
//! is that rule's test.
//!
//! RULE 2: what is reported ABOUT an image frame is computed from the pixel data that frame
//! holds — its size in bytes, the checksum of it at the resolution the instance declares,
//! and the smallest and largest SAMPLE in it, which is stated only when the stored bytes
//! are the raster the instance's own image pixel description declares.
//!
//! RULE 3: the two study-level counts are DICOM attributes of the study, so
//! `NumberOfStudyRelatedSeries` is the one the imported files declared — the modelled
//! default of its member when they declared none, whatever the import happened to index —
//! while `NumberOfStudyRelatedInstances` falls back to the number of instances the service
//! indexed for the study, which is a count it holds.
//!
//! The suite is a TABLE whose rows differ along what the rules generalise over: the
//! transfer syntax the file is encoded in (explicit and implicit VR), how the data set
//! spells its integer strings (one digit, two digits, leading zeros), the sample width and
//! signedness of the pixel data (8-bit unsigned, 16-bit unsigned, 16-bit signed), whether
//! the bytes stored are the raster the instance declares, whether the instance carries
//! pixel data at all, and whether the file declares the study counts. Every case drives
//! real HTTP through the production front door, so what is asserted is the wire behaviour a
//! `medical-imaging` SDK client sees.
//!
//! ## Why the image set is seeded from the import's own reading
//!
//! Each case writes a real DICOM Part 10 file, reads it with the crate's own import reader
//! (`read_part10`) and groups it with the crate's own `imported_image_sets` — the two
//! functions `StartDICOMImportJob` itself uses — and then seeds the image set those
//! functions describe through the crate's public `state` module, exactly as the sibling
//! `crate::import_job_image_set_delivery` suite does. Mounting Amazon S3 beside this
//! service is `aws_emulator_router`'s `medical_imaging_dicom_import` suite's job; what is
//! asserted here is what a client READS back over the wire.

use std::collections::BTreeMap;
use std::time::{Duration, Instant, SystemTime};

use serde_json::{json, Value};

use service_com_amazonaws_medicalimaging_ahigatewayservice::state::{
    attribute_bytes_quota_message, image_set_arn, imported_image_sets,
    instance_attributes_quota_message, read_part10, service_state, ImageSet, ImageSetState,
    ImageSetWorkflowStatus, ImportedInstance, IMAGE_SET_COPY_DELAY, IMAGE_SET_UPDATE_DELAY,
    MAX_IMAGE_SET_ATTRIBUTE_BYTES, MAX_IMAGE_SET_INSTANCE_ATTRIBUTES,
};

use crate::datastore_lifecycle::Emulator;
use crate::image_set_copy::{copy_image_set, get_image_set, information};
use crate::image_set_copy_subsets::selector;
use crate::image_set_metadata::{document, get_image_set_metadata};
use crate::image_set_metadata_updates::{at, dicom_updates, update_metadata};

/// How long a poll waits for a change the CLOCK owes before giving up. Generous on
/// purpose: it bounds a hung emulator rather than the spans this service promises.
const POLL_DEADLINE: Duration = Duration::from_secs(120);

/// How long a poll waits between looks.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// Explicit VR Little Endian — the encoding most DICOM files on disk carry, and the one
/// [`part10_file`] writes a data set explicitly for.
pub(crate) const EXPLICIT_VR_LE: &str = "1.2.840.10008.1.2.1";

/// Implicit VR Little Endian — the DICOM default encoding, in which an element carries no
/// value representation of its own and a reader takes it from the data dictionary.
const IMPLICIT_VR_LE: &str = "1.2.840.10008.1.2";

/// The SOP class every file below declares: Secondary Capture Image Storage.
pub(crate) const SOP_CLASS: &str = "1.2.840.10008.5.1.4.1.1.7";

/// The implementation class UID the files below declare, which the file meta group of every
/// Part 10 file carries.
const IMPLEMENTATION_CLASS: &str = "1.2.826.0.1.3680043.8.498.1";

/// The transfer syntax the aggregated metadata document must report the STORED pixel data
/// as being in: the UID whose `contentType` `GetImageFrame` answers for a frame this
/// service holds, so the document and the frame read agree.
const STORED_TRANSFER_SYNTAX: &str = "1.2.840.10008.1.2.4.202";

/// The checksum algorithm the document must name beside a frame's checksums: the one the
/// service really sums a frame's pixel data with, which this suite verifies by recomputing
/// a reported checksum with it.
const CHECKSUM_TYPE: &str = "CRC32";

/// The members the aggregated metadata document has at the INSTANCE level, which RULE 4
/// holds an instance object to: the instance's DICOM attributes, the value representation
/// of each, the transfer syntax its stored pixel data is served in, its image frames, and
/// the checksum type of that pixel data.
///
/// The last two belong to an instance that HOLDS pixel data: an instance holding none has
/// no frames to report and no checksum to name a type for.
const INSTANCE_LEVEL_MEMBERS: [&str; 5] = [
    "ChecksumType",
    "DICOM",
    "DICOMVRs",
    "ImageFrames",
    "StoredTransferSyntaxUID",
];

/// The instance-level members only an instance holding pixel data carries.
const PIXEL_DATA_MEMBERS: [&str; 2] = ["ChecksumType", "ImageFrames"];

/// The patient, study and series identifiers every file below carries, so one case's
/// content cannot be read as another's.
const PATIENT_ID: &str = "PID-INSTANCE";
const STUDY_UID: &str = "1.2.826.0.1.3680043.9.7133.11.1";
const SERIES_UID: &str = "1.2.826.0.1.3680043.9.7133.11.1.1";
const SOP_UID: &str = "1.2.826.0.1.3680043.9.7133.11.1.1.1";

/// A SECOND SOP instance of the same series, for the cases that copy or update one
/// instance of an image set holding two.
const OTHER_SOP_UID: &str = "1.2.826.0.1.3680043.9.7133.11.1.1.2";

/// `value`, padded to the even length DICOM writes every data element value at.
///
/// Shared with the sibling suites that write Part 10 files: `crate::dicom_part10_reading`
/// builds its own files from these, so one writer of a DICOM data element serves both.
pub(crate) fn padded(value: &[u8], pad: u8) -> Vec<u8> {
    let mut bytes = value.to_vec();
    if bytes.len() % 2 == 1 {
        bytes.push(pad);
    }
    bytes
}

/// A text value as the data set WRITES it: padded to an even length with a space, which is
/// how DICOM pads an integer string.
fn written(value: &str) -> String {
    String::from_utf8(padded(value.as_bytes(), b' ')).expect("ASCII text stays UTF-8")
}

/// One data element in EXPLICIT VR little endian: the tag, the value representation, then
/// the value's length — 32-bit for the value representations DICOM gives the long form,
/// 16-bit for the rest (DICOM PS3.5 section 7.1.2).
pub(crate) fn explicit(group: u16, element: u16, vr: &[u8; 2], value: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&group.to_le_bytes());
    out.extend_from_slice(&element.to_le_bytes());
    out.extend_from_slice(vr);
    if matches!(vr, b"OB" | b"OW" | b"OF" | b"SQ" | b"UT" | b"UN") {
        out.extend_from_slice(&[0, 0]);
        out.extend_from_slice(&u32::try_from(value.len()).expect("a short value").to_le_bytes());
    } else {
        out.extend_from_slice(&u16::try_from(value.len()).expect("a short value").to_le_bytes());
    }
    out.extend_from_slice(value);
    out
}

/// One data element in IMPLICIT VR little endian: the tag and a 32-bit length, with the
/// value representation left to the data dictionary.
fn implicit(group: u16, element: u16, value: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&group.to_le_bytes());
    out.extend_from_slice(&element.to_le_bytes());
    out.extend_from_slice(&u32::try_from(value.len()).expect("a short value").to_le_bytes());
    out.extend_from_slice(value);
    out
}

/// One attribute of a data set: its tag, the value representation the explicit encoding
/// writes, and the value as DICOM pads it.
#[derive(Clone)]
pub(crate) struct Attribute {
    pub(crate) group: u16,
    pub(crate) element: u16,
    pub(crate) vr: &'static [u8; 2],
    pub(crate) value: Vec<u8>,
}

/// A text attribute, padded with a space as DICOM pads text.
pub(crate) fn text(group: u16, element: u16, vr: &'static [u8; 2], value: &str) -> Attribute {
    Attribute {
        group,
        element,
        vr,
        value: padded(value.as_bytes(), b' '),
    }
}

/// A UID attribute, padded with a NUL as DICOM pads a UI value.
pub(crate) fn uid(group: u16, element: u16, value: &str) -> Attribute {
    Attribute {
        group,
        element,
        vr: b"UI",
        value: padded(value.as_bytes(), 0),
    }
}

/// An unsigned-short attribute, written little endian.
fn unsigned_short(group: u16, element: u16, value: u16) -> Attribute {
    Attribute {
        group,
        element,
        vr: b"US",
        value: value.to_le_bytes().to_vec(),
    }
}

/// One row of the table: the file one case imports, and what the document must report for
/// it.
#[derive(Clone)]
struct Case {
    case: &'static str,
    /// The transfer syntax the data set is encoded in.
    transfer_syntax: &'static str,
    /// The Series Number and Instance Number as the data set SPELLS them (integer
    /// strings), which is how the document must report them.
    series_number: &'static str,
    instance_number: &'static str,
    /// The two study counts the file declares, when it declares them.
    declared_study_counts: Option<(&'static str, &'static str)>,
    /// The two counts the document must report.
    reported_study_counts: (i64, i64),
    rows: u16,
    columns: u16,
    bits_allocated: u16,
    bits_stored: u16,
    high_bit: u16,
    /// `1` for two's-complement signed samples, `0` for unsigned ones.
    pixel_representation: u16,
    /// The pixel data the instance carries, `None` for an instance carrying none.
    pixel_data: Option<&'static [u8]>,
    /// The smallest and largest sample the pixel data carries, or `None` for pixel data
    /// that is not the raster the attributes above declare — where there is no sample to
    /// measure.
    sample_range: Option<(i64, i64)>,
    /// How many bytes of Image Comments (an instance-level attribute DICOM gives a long
    /// text) the data set carries, when it carries that attribute at all — what the bound
    /// on the BYTES of one attribute is driven with.
    comment_bytes: Option<usize>,
}

/// A `Case` with no Image Comments attribute, which is every row but the ones driving the
/// bound on one attribute's bytes.
impl Case {
    fn with_comment(mut self, bytes: usize) -> Self {
        self.comment_bytes = Some(bytes);
        self
    }
}

impl Case {
    /// The DICOM Part 10 file this case imports: the 128-byte preamble, the `DICM` magic
    /// code, the file meta group (explicit VR little endian, as DICOM PS3.10 requires) and
    /// the data set in the transfer syntax that group declares.
    ///
    /// Answers the bytes and the length of the meta group's elements, which is the value
    /// the group length attribute carries and so the number the document must report for
    /// it.
    fn part10(&self, sop_uid: &str) -> (Vec<u8>, u32) {
        let mut attributes = vec![
            uid(0x0008, 0x0016, SOP_CLASS),
            uid(0x0008, 0x0018, sop_uid),
            text(0x0008, 0x0060, b"CS", "OT"),
            text(0x0010, 0x0020, b"LO", PATIENT_ID),
            uid(0x0020, 0x000D, STUDY_UID),
            uid(0x0020, 0x000E, SERIES_UID),
            text(0x0020, 0x0011, b"IS", self.series_number),
            text(0x0020, 0x0013, b"IS", self.instance_number),
        ];
        if let Some((series, instances)) = self.declared_study_counts {
            attributes.push(text(0x0020, 0x1206, b"IS", series));
            attributes.push(text(0x0020, 0x1208, b"IS", instances));
        }
        // (0020,4000) Image Comments, a long text DICOM gives the instance level — the one
        // attribute a row can make as large as it likes. Written here so the data set stays
        // in the ascending tag order DICOM writes it in.
        if let Some(bytes) = self.comment_bytes {
            attributes.push(Attribute {
                group: 0x0020,
                element: 0x4000,
                vr: b"LT",
                value: padded(&vec![b'C'; bytes], b' '),
            });
        }
        attributes.extend([
            unsigned_short(0x0028, 0x0002, 1),
            text(0x0028, 0x0004, b"CS", "MONOCHROME2"),
            unsigned_short(0x0028, 0x0010, self.rows),
            unsigned_short(0x0028, 0x0011, self.columns),
            unsigned_short(0x0028, 0x0100, self.bits_allocated),
            unsigned_short(0x0028, 0x0101, self.bits_stored),
            unsigned_short(0x0028, 0x0102, self.high_bit),
            unsigned_short(0x0028, 0x0103, self.pixel_representation),
        ]);
        if let Some(pixels) = self.pixel_data {
            attributes.push(Attribute {
                group: 0x7FE0,
                element: 0x0010,
                vr: b"OB",
                value: padded(pixels, 0),
            });
        }
        part10_file(
            sop_uid,
            self.transfer_syntax,
            Some(IMPLEMENTATION_CLASS),
            &attributes,
        )
    }
}

/// The DICOM Part 10 FILE carrying `attributes`: the 128-byte preamble, the `DICM` magic
/// code, the file meta group (always explicit VR little endian, as DICOM PS3.10 requires)
/// and the data set in the transfer syntax that group declares — explicit VR little endian
/// written explicitly, every other transfer syntax written implicitly.
///
/// Answers the bytes and the length of the meta group's elements, which is the value the
/// group length attribute carries and so the number the document must report for it.
///
/// The one writer of a Part 10 file in this test binary: `crate::dicom_part10_reading`
/// builds its own files through it, so the two suites cannot drift on what such a file
/// looks like. `implementation_class` is the `(0002,0012)` element, for a file that declares
/// one, and the attributes are written in the order given — which DICOM wants ascending by
/// tag.
pub(crate) fn part10_file(
    sop_uid: &str,
    transfer_syntax: &str,
    implementation_class: Option<&str>,
    attributes: &[Attribute],
) -> (Vec<u8>, u32) {
    let mut meta = Vec::new();
    meta.extend(explicit(0x0002, 0x0001, b"OB", &[0x00, 0x01]));
    meta.extend(explicit(
        0x0002,
        0x0002,
        b"UI",
        &padded(SOP_CLASS.as_bytes(), 0),
    ));
    meta.extend(explicit(0x0002, 0x0003, b"UI", &padded(sop_uid.as_bytes(), 0)));
    meta.extend(explicit(
        0x0002,
        0x0010,
        b"UI",
        &padded(transfer_syntax.as_bytes(), 0),
    ));
    if let Some(implementation_class) = implementation_class {
        meta.extend(explicit(
            0x0002,
            0x0012,
            b"UI",
            &padded(implementation_class.as_bytes(), 0),
        ));
    }

    let explicit_dataset = transfer_syntax == EXPLICIT_VR_LE;
    let mut dataset = Vec::new();
    for attribute in attributes {
        if explicit_dataset {
            dataset.extend(explicit(
                attribute.group,
                attribute.element,
                attribute.vr,
                &attribute.value,
            ));
        } else {
            dataset.extend(implicit(attribute.group, attribute.element, &attribute.value));
        }
    }

    let mut file = vec![0_u8; 128];
    file.extend_from_slice(b"DICM");
    let group_length = u32::try_from(meta.len()).expect("a short meta group");
    file.extend(explicit(0x0002, 0x0000, b"UL", &group_length.to_le_bytes()));
    file.extend(meta);
    file.extend(dataset);
    (file, group_length)
}

/// 16-bit unsigned samples, little endian: 1, 256, 255, 2571, 2, 3, 4 and 5.
const PIXELS_16_UNSIGNED: &[u8] = &[
    0x01, 0x00, 0x00, 0x01, 0xff, 0x00, 0x0b, 0x0a, 0x02, 0x00, 0x03, 0x00, 0x04, 0x00, 0x05, 0x00,
];

/// 8-bit unsigned samples: 16, 32, 48 and 240.
const PIXELS_8_UNSIGNED: &[u8] = &[0x10, 0x20, 0x30, 0xf0];

/// 16-bit TWO'S-COMPLEMENT samples, little endian: -1024 and 1000.
const PIXELS_16_SIGNED: &[u8] = &[0x00, 0xfc, 0xe8, 0x03];

/// Four bytes where the attributes declare a 4x4 raster of 16-bit samples, i.e. pixel data
/// that is NOT the raster the instance describes.
const PIXELS_SHORT_OF_THE_RASTER: &[u8] = &[0x07, 0x00, 0x09, 0x00];

/// The rows, in the order the suite drives them.
fn cases() -> Vec<Case> {
    vec![
        Case {
            case: "explicit VR, 16-bit unsigned samples, the file declaring the study counts",
            transfer_syntax: EXPLICIT_VR_LE,
            series_number: "3",
            instance_number: "1",
            declared_study_counts: Some(("7", "19")),
            reported_study_counts: (7, 19),
            rows: 2,
            columns: 4,
            bits_allocated: 16,
            bits_stored: 16,
            high_bit: 15,
            pixel_representation: 0,
            pixel_data: Some(PIXELS_16_UNSIGNED),
            sample_range: Some((1, 2571)),
            comment_bytes: None,
        },
        Case {
            case: "implicit VR, 8-bit samples, integer strings with leading zeros and no \
                   declared study counts",
            transfer_syntax: IMPLICIT_VR_LE,
            series_number: "012",
            instance_number: "007",
            declared_study_counts: None,
            // The series count stays the modelled default of its member — one series was
            // indexed and the file declared no count — while the instance count is the one
            // instance the service indexed for the study.
            reported_study_counts: (0, 1),
            rows: 2,
            columns: 2,
            bits_allocated: 8,
            bits_stored: 8,
            high_bit: 7,
            pixel_representation: 0,
            pixel_data: Some(PIXELS_8_UNSIGNED),
            sample_range: Some((16, 240)),
            comment_bytes: None,
        },
        Case {
            case: "explicit VR, 16-bit signed samples",
            transfer_syntax: EXPLICIT_VR_LE,
            series_number: "11",
            instance_number: "2",
            declared_study_counts: Some(("1", "1")),
            reported_study_counts: (1, 1),
            rows: 1,
            columns: 2,
            bits_allocated: 16,
            bits_stored: 16,
            high_bit: 15,
            pixel_representation: 1,
            pixel_data: Some(PIXELS_16_SIGNED),
            sample_range: Some((-1024, 1000)),
            comment_bytes: None,
        },
        Case {
            case: "pixel data that is not the raster the instance declares",
            transfer_syntax: EXPLICIT_VR_LE,
            series_number: "4",
            instance_number: "3",
            declared_study_counts: None,
            reported_study_counts: (0, 1),
            rows: 4,
            columns: 4,
            bits_allocated: 16,
            bits_stored: 12,
            high_bit: 11,
            pixel_representation: 0,
            pixel_data: Some(PIXELS_SHORT_OF_THE_RASTER),
            // Nothing to measure: the bytes held are not the samples the attributes
            // describe.
            sample_range: None,
            comment_bytes: None,
        },
        Case {
            case: "an instance carrying no pixel data at all",
            transfer_syntax: EXPLICIT_VR_LE,
            series_number: "5",
            instance_number: "4",
            declared_study_counts: None,
            reported_study_counts: (0, 1),
            rows: 2,
            columns: 2,
            bits_allocated: 16,
            bits_stored: 16,
            high_bit: 15,
            pixel_representation: 0,
            pixel_data: None,
            sample_range: None,
            comment_bytes: None,
        },
    ]
}

/// Put the image set the imported instances describe into `datastore_id`, built by the
/// crate's own import reading and grouping, and answer its identifier together with the
/// image frame identifier of each SOP instance that carried pixel data.
///
/// The record is seeded as a COMPLETED import leaves it — `ACTIVE` / `IMPORTED`, holding
/// the attributes and frames the grouping described — so every assertion below reads what a
/// client reads after an import has landed.
fn seed_imported_image_set(
    emulator: &Emulator,
    case: &str,
    datastore_id: &str,
    files: &[Vec<u8>],
) -> (String, BTreeMap<String, String>) {
    let instances: Vec<ImportedInstance> = files
        .iter()
        .enumerate()
        .map(|(index, file)| {
            let mut instance = read_part10(file).unwrap_or_else(|reason| {
                panic!(
                    "case {case:?}: expected the written file to be read as DICOM, got \
                     {reason:?}"
                )
            });
            instance.source_key = Some(format!("in/instance-{index}.dcm"));
            instance
        })
        .collect();

    let state = service_state(emulator.engine()).expect("the medical-imaging state store");
    let mut store = state.lock();
    let mut imported = {
        let mut mint = || store.mint_id();
        imported_image_sets(&instances, &mut mint)
    };
    assert_eq!(
        imported.len(),
        1,
        "case {case:?}: expected one image set for the instances of one series, got {}",
        imported.len()
    );
    let one = imported.remove(0);
    let frames: BTreeMap<String, String> = one
        .image_frames
        .values()
        .filter_map(|frame| {
            frame
                .sop_instance_uid
                .clone()
                .map(|uid| (uid, frame.image_frame_id.clone()))
        })
        .collect();
    let datastore = store
        .datastores
        .get_mut(datastore_id)
        .expect("the created data store");
    let datastore_arn = datastore.datastore_arn.clone();
    let mut image_set = ImageSet::new(
        one.image_set_id.clone(),
        image_set_arn(&datastore_arn, &one.image_set_id),
        ImageSetState::Active,
        ImageSetWorkflowStatus::Imported,
        true,
        false,
        SystemTime::now(),
    );
    image_set.set_attributes(one.attributes);
    image_set.image_frames = one.image_frames;
    datastore
        .image_sets
        .insert(one.image_set_id.clone(), image_set);
    (one.image_set_id, frames)
}

/// The CRC32 of `bytes`, computed here with the same maintained codec the service computes
/// a frame's checksum with — so what is asserted is the bytes the checksum covers and the
/// raster it is reported at, not the arithmetic.
fn crc32(bytes: &[u8]) -> u32 {
    let mut hasher = crc32fast::Hasher::new();
    hasher.update(bytes);
    hasher.finalize()
}

/// RULES 1, 2 and 3 (above), over the table.
#[tokio::test]
async fn the_document_reports_each_imported_instance_as_its_data_set_wrote_it() {
    for (index, case) in cases().iter().enumerate() {
        let name = case.case;
        let emulator = Emulator::new();
        let datastore_id = emulator
            .active_datastore(
                &format!("instance.metadata.{index}"),
                &format!("token-instance-metadata-{index}"),
            )
            .await;
        let (file, group_length) = case.part10(SOP_UID);
        let (image_set_id, frames) =
            seed_imported_image_set(&emulator, name, &datastore_id, &[file]);

        let answer = get_image_set_metadata(&emulator, &datastore_id, &image_set_id, None).await;
        // `document` asserts the answer's own `contentType` and `contentEncoding`: the
        // media type of the document served and the compression it is carried under.
        let read = document(&answer, name);

        // RULE 3: the two study-level counts.
        for (keyword, expected) in [
            ("NumberOfStudyRelatedSeries", case.reported_study_counts.0),
            (
                "NumberOfStudyRelatedInstances",
                case.reported_study_counts.1,
            ),
        ] {
            let path = ["Study", "DICOM", keyword];
            assert_eq!(
                at(&read, &path),
                json!(expected),
                "case {name:?}: expected {keyword} {expected}, got {}",
                at(&read, &path)
            );
        }

        // RULE 1 at the series level: the Series Number as the data set wrote it, padded
        // to the even length DICOM writes an integer string at.
        let series_number = ["Study", "Series", SERIES_UID, "DICOM", "SeriesNumber"];
        assert_eq!(
            at(&read, &series_number),
            json!(written(case.series_number)),
            "case {name:?}: expected the Series Number as the data set wrote it, got {}",
            at(&read, &series_number)
        );

        // RULE 1 at the instance level: every attribute of the instance, typed as its
        // value representation writes it.
        let instance_path = ["Study", "Series", SERIES_UID, "Instances", SOP_UID];
        let dicom = at(&read, &[instance_path.as_slice(), &["DICOM"]].concat());
        for (keyword, expected) in [
            ("FileMetaInformationGroupLength", json!(group_length)),
            // A binary value is carried as base64 text: two bytes, `00 01`.
            ("FileMetaInformationVersion", json!("AAE=")),
            ("MediaStorageSOPClassUID", json!(SOP_CLASS)),
            ("MediaStorageSOPInstanceUID", json!(SOP_UID)),
            ("TransferSyntaxUID", json!(case.transfer_syntax)),
            ("ImplementationClassUID", json!(IMPLEMENTATION_CLASS)),
            ("SOPClassUID", json!(SOP_CLASS)),
            ("SOPInstanceUID", json!(SOP_UID)),
            ("InstanceNumber", json!(written(case.instance_number))),
            ("SamplesPerPixel", json!(1)),
            ("PhotometricInterpretation", json!("MONOCHROME2")),
            ("Rows", json!(case.rows)),
            ("Columns", json!(case.columns)),
            ("BitsAllocated", json!(case.bits_allocated)),
            ("BitsStored", json!(case.bits_stored)),
            ("HighBit", json!(case.high_bit)),
            ("PixelRepresentation", json!(case.pixel_representation)),
        ] {
            assert_eq!(
                dicom[keyword], expected,
                "case {name:?}: expected the instance's {keyword} to be {expected}, got {}",
                dicom[keyword]
            );
        }
        // The attributes reported at a HIGHER level are reported there and not again here.
        for keyword in ["PatientID", "StudyInstanceUID", "SeriesInstanceUID"] {
            assert!(
                dicom[keyword].is_null(),
                "case {name:?}: {keyword} belongs to a level above the instance, got {}",
                dicom[keyword]
            );
        }

        // RULE 1: the value representation of each attribute, beside the attributes.
        let vrs = at(&read, &[instance_path.as_slice(), &["DICOMVRs"]].concat());
        for (keyword, expected) in [
            ("FileMetaInformationGroupLength", "UL"),
            ("FileMetaInformationVersion", "OB"),
            ("SOPClassUID", "UI"),
            ("InstanceNumber", "IS"),
            ("PhotometricInterpretation", "CS"),
            ("Rows", "US"),
        ] {
            assert_eq!(
                vrs[keyword],
                json!(expected),
                "case {name:?}: expected the value representation of {keyword} to be \
                 {expected:?}, got {}",
                vrs[keyword]
            );
        }

        // RULE 1: the transfer syntax the STORED pixel data is served in, which is the
        // service's own and not the one the file declared.
        let stored = at(
            &read,
            &[instance_path.as_slice(), &["StoredTransferSyntaxUID"]].concat(),
        );
        assert_eq!(
            stored,
            json!(STORED_TRANSFER_SYNTAX),
            "case {name:?}: expected the stored transfer syntax {STORED_TRANSFER_SYNTAX:?}, \
             got {stored}"
        );

        // RULE 2: what is reported about the frame, when the instance holds pixel data.
        let image_frames = at(
            &read,
            &[instance_path.as_slice(), &["ImageFrames"]].concat(),
        );
        let checksum_type = at(
            &read,
            &[instance_path.as_slice(), &["ChecksumType"]].concat(),
        );
        match case.pixel_data {
            None => {
                assert!(
                    image_frames.is_null(),
                    "case {name:?}: an instance holding no pixel data reports no frames, got \
                     {image_frames}"
                );
                assert!(
                    checksum_type.is_null(),
                    "case {name:?}: an instance holding no frame checksum names no checksum \
                     type, got {checksum_type}"
                );
            }
            Some(pixels) => {
                let image_frame_id = frames
                    .get(SOP_UID)
                    .unwrap_or_else(|| panic!("case {name:?}: expected a frame for {SOP_UID:?}"));
                let mut expected = json!({
                    "ID": image_frame_id,
                    "FrameSizeInBytes": pixels.len(),
                    "PixelDataChecksumFromBaseToFullResolution": [{
                        "Width": case.columns,
                        "Height": case.rows,
                        "Checksum": crc32(pixels),
                    }],
                });
                if let Some((min, max)) = case.sample_range {
                    expected["MinPixelValue"] = json!(min);
                    expected["MaxPixelValue"] = json!(max);
                }
                assert_eq!(
                    image_frames,
                    Value::Array(vec![expected.clone()]),
                    "case {name:?}: expected the frame reported as {expected}, got {image_frames}"
                );
                assert_eq!(
                    checksum_type,
                    json!(CHECKSUM_TYPE),
                    "case {name:?}: expected the checksum type {CHECKSUM_TYPE:?}, got \
                     {checksum_type}"
                );
            }
        }
    }
}

/// The base case the two suites below import: one 16-bit unsigned instance per SOP
/// instance UID, declaring the study counts.
fn two_instance_case() -> Case {
    Case {
        case: "two instances of one series",
        transfer_syntax: EXPLICIT_VR_LE,
        series_number: "3",
        instance_number: "1",
        declared_study_counts: Some(("2", "2")),
        reported_study_counts: (2, 2),
        rows: 2,
        columns: 4,
        bits_allocated: 16,
        bits_stored: 16,
        high_bit: 15,
        pixel_representation: 0,
        pixel_data: Some(PIXELS_16_UNSIGNED),
        sample_range: Some((1, 2571)),
        comment_bytes: None,
    }
}

/// Poll `GetImageSet` until the write the CLOCK owed has landed: the image set back in
/// `ACTIVE` at `version_id`.
///
/// Waits for the CONDITION rather than for the span the write is promised in, so a loaded
/// host only makes a case slower. Panics past [`POLL_DEADLINE`], which bounds a hung
/// emulator rather than the spans this service promises
/// ([`IMAGE_SET_COPY_DELAY`], [`IMAGE_SET_UPDATE_DELAY`]).
async fn poll_until_landed(
    emulator: &Emulator,
    case: &str,
    datastore_id: &str,
    image_set_id: &str,
    version_id: &str,
) {
    let deadline = Instant::now() + POLL_DEADLINE;
    loop {
        let read = get_image_set(emulator, datastore_id, image_set_id, None).await;
        if read.status == 200
            && read.string(&["imageSetState"]) == "ACTIVE"
            && read.string(&["versionId"]) == version_id
        {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the write never landed ACTIVE at version {version_id} (the spans are \
             {IMAGE_SET_COPY_DELAY:?} for a copy and {IMAGE_SET_UPDATE_DELAY:?} for an update), \
             last answer {read:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The DICOM attributes the document reports for the SOP instance `sop_uid` of the one
/// series, or `Value::Null` when it reports that instance at all.
fn instance_dicom(read: &Value, sop_uid: &str) -> Value {
    at(
        read,
        &["Study", "Series", SERIES_UID, "Instances", sop_uid, "DICOM"],
    )
}

/// RULE 1, the half a COPY is judged by: a copy carries the DICOM metadata of the SOP
/// instances it selected and none of the ones it left behind, so the destination's document
/// reports exactly the instances the copy carried.
///
/// The rows differ along which of the source's two instances the selector names: one, the
/// other, and both.
#[tokio::test]
async fn a_subset_copy_carries_the_instance_metadata_of_the_instances_it_selected() {
    let base = two_instance_case();
    // (the case, the instances the selector names)
    let rows: [(&str, Vec<&str>); 3] = [
        ("the first instance alone", vec![SOP_UID]),
        ("the second instance alone", vec![OTHER_SOP_UID]),
        ("both instances", vec![SOP_UID, OTHER_SOP_UID]),
    ];

    for (index, (name, selected)) in rows.iter().enumerate() {
        let emulator = Emulator::new();
        let datastore_id = emulator
            .active_datastore(
                &format!("instance.copy.{index}"),
                &format!("token-instance-copy-{index}"),
            )
            .await;
        let files = vec![base.part10(SOP_UID).0, base.part10(OTHER_SOP_UID).0];
        let (source_id, _) = seed_imported_image_set(&emulator, name, &datastore_id, &files);

        let copied = copy_image_set(
            &emulator,
            &datastore_id,
            &source_id,
            information("1", Some(&selector(SERIES_UID, selected)), None),
            &[],
        )
        .await;
        assert_eq!(
            copied.status, 200,
            "case {name:?}: expected HTTP 200 from CopyImageSet, got {copied:?}"
        );
        let destination_id = copied.string(&["destinationImageSetProperties", "imageSetId"]);
        poll_until_landed(&emulator, name, &datastore_id, &destination_id, "1").await;

        let read = document(
            &get_image_set_metadata(&emulator, &datastore_id, &destination_id, None).await,
            name,
        );
        for sop_uid in [SOP_UID, OTHER_SOP_UID] {
            let reported = instance_dicom(&read, sop_uid);
            if selected.contains(&sop_uid) {
                assert_eq!(
                    reported["SOPInstanceUID"],
                    json!(sop_uid),
                    "case {name:?}: expected the metadata of the selected instance \
                     {sop_uid:?}, got {reported}"
                );
                assert_eq!(
                    reported["Rows"],
                    json!(base.rows),
                    "case {name:?}: expected the selected instance's own attributes, got \
                     {reported}"
                );
            } else {
                assert!(
                    reported.is_null(),
                    "case {name:?}: an instance the copy left behind must not be reported, got \
                     {reported} for {sop_uid:?}"
                );
            }
        }
        // The SOURCE still holds both: a copy reads its source, it does not move it.
        let source = document(
            &get_image_set_metadata(&emulator, &datastore_id, &source_id, None).await,
            name,
        );
        for sop_uid in [SOP_UID, OTHER_SOP_UID] {
            assert_eq!(
                instance_dicom(&source, sop_uid)["SOPInstanceUID"],
                json!(sop_uid),
                "case {name:?}: expected the source to still report {sop_uid:?}, got {source}"
            );
        }
    }
}

/// RULE 1, the half an UPDATE is judged by: a metadata update reaches the attributes of an
/// imported SOP instance — what it writes is reported from then on, and what it removes is
/// reported no more — because those attributes are what the document's instance level is
/// served from.
///
/// The rows differ along which attribute of an imported instance the update names: one the
/// import stored (a write over it and a removal of it) and one it did not.
#[tokio::test]
async fn an_update_reaches_the_attributes_of_an_imported_instance() {
    let base = two_instance_case();
    let emulator = Emulator::new();
    let datastore_id = emulator
        .active_datastore("instance.update", "token-instance-update")
        .await;
    let files = vec![base.part10(SOP_UID).0, base.part10(OTHER_SOP_UID).0];
    let (image_set_id, _) =
        seed_imported_image_set(&emulator, "an imported instance", &datastore_id, &files);

    let instance_path = |keyword: &str| {
        json!({
            "SchemaVersion": "1.1",
            "Study": { "Series": { SERIES_UID: { "Instances": { SOP_UID: {
                "DICOM": { keyword: "" },
            } } } } },
        })
    };
    // A write over an attribute the import stored, a write of one it did not store, and a
    // removal of a third — all addressed at the SAME imported instance.
    let mut written = instance_path("BitsStored");
    written["Study"]["Series"][SERIES_UID]["Instances"][SOP_UID]["DICOM"] = json!({
        "BitsStored": 12,
        "ImageComments": "RE-WINDOWED",
    });
    let removed = instance_path("PhotometricInterpretation");

    let updated = update_metadata(
        &emulator,
        &datastore_id,
        &image_set_id,
        "1",
        dicom_updates(Some(&written), Some(&removed)),
        &[],
    )
    .await;
    assert_eq!(
        updated.status, 200,
        "case \"an imported instance\": expected HTTP 200 from UpdateImageSetMetadata, got \
         {updated:?}"
    );
    poll_until_landed(
        &emulator,
        "an imported instance",
        &datastore_id,
        &image_set_id,
        "2",
    )
    .await;

    let read = document(
        &get_image_set_metadata(&emulator, &datastore_id, &image_set_id, None).await,
        "an imported instance",
    );
    let reported = instance_dicom(&read, SOP_UID);
    assert_eq!(
        reported["BitsStored"],
        json!(12),
        "case \"an imported instance\": expected the written value over the imported one, got \
         {reported}"
    );
    assert_eq!(
        reported["ImageComments"],
        json!("RE-WINDOWED"),
        "case \"an imported instance\": expected the attribute the update added, got {reported}"
    );
    assert!(
        reported["PhotometricInterpretation"].is_null(),
        "case \"an imported instance\": expected the removed attribute to be reported no more, \
         got {reported}"
    );
    // The attributes the update said nothing about stay, here and on the instance it did
    // not name.
    assert_eq!(
        reported["Rows"],
        json!(base.rows),
        "case \"an imported instance\": expected the untouched attributes to stay, got {reported}"
    );
    let untouched = instance_dicom(&read, OTHER_SOP_UID);
    assert_eq!(
        untouched["PhotometricInterpretation"],
        json!("MONOCHROME2"),
        "case \"an imported instance\": an update naming one instance must leave the other's \
         attributes alone, got {untouched}"
    );
}

/// RULE: what a caller may accumulate in the DICOM metadata of one SOP instance is
/// BOUNDED, by the same shape every other writer of an image set's attributes answers to:
/// an update that would leave one instance holding more attributes than this emulator keeps
/// of one is refused with the modelled quota error, and persists nothing.
///
/// The rows differ along which side of the bound the update lands on: the attributes an
/// instance may still take, and one more than that.
#[tokio::test]
async fn an_update_past_the_instance_attribute_bound_is_refused_and_writes_nothing() {
    let base = two_instance_case();
    // (the case, how many attributes past what the instance already holds the update
    // writes, whether it is admitted)
    let rows = [("up to the bound", 0_i64, true), ("one past it", 1, false)];

    for (index, (name, past, admitted)) in rows.iter().enumerate() {
        let emulator = Emulator::new();
        let datastore_id = emulator
            .active_datastore(
                &format!("instance.bound.{index}"),
                &format!("token-instance-bound-{index}"),
            )
            .await;
        let files = vec![base.part10(SOP_UID).0];
        let (image_set_id, _) = seed_imported_image_set(&emulator, name, &datastore_id, &files);

        let before = document(
            &get_image_set_metadata(&emulator, &datastore_id, &image_set_id, None).await,
            name,
        );
        let held = instance_dicom(&before, SOP_UID)
            .as_object()
            .map(serde_json::Map::len)
            .unwrap_or_default();
        // Written as DICOM spells a private attribute — its tag, in an odd group, which is
        // a group no standard attribute lives in — so a case can write as many distinct
        // attributes as the bound takes.
        let mut written = serde_json::Map::new();
        let room = MAX_IMAGE_SET_INSTANCE_ATTRIBUTES - held;
        let writing = i64::try_from(room).expect("the bound fits an i64") + past;
        for element in 0..writing {
            written.insert(format!("0009{element:04x}"), json!("PRIVATE"));
        }
        let document_written = json!({
            "SchemaVersion": "1.1",
            "Study": { "Series": { SERIES_UID: { "Instances": { SOP_UID: {
                "DICOM": Value::Object(written),
            } } } } },
        });

        let updated = update_metadata(
            &emulator,
            &datastore_id,
            &image_set_id,
            "1",
            dicom_updates(Some(&document_written), None),
            // A private attribute of one SOP instance is one of the changes the operation
            // documents the force flag for.
            &[("force", true)],
        )
        .await;
        if *admitted {
            assert_eq!(
                updated.status, 200,
                "case {name:?}: expected the update that fits the bound to be admitted, got \
                 {updated:?}"
            );
            poll_until_landed(&emulator, name, &datastore_id, &image_set_id, "2").await;
            let after = document(
                &get_image_set_metadata(&emulator, &datastore_id, &image_set_id, None).await,
                name,
            );
            let reported = instance_dicom(&after, SOP_UID)
                .as_object()
                .map(serde_json::Map::len)
                .unwrap_or_default();
            assert_eq!(
                reported, MAX_IMAGE_SET_INSTANCE_ATTRIBUTES,
                "case {name:?}: expected the instance to hold the attributes the bound takes, \
                 got {reported}"
            );
            continue;
        }
        assert_eq!(
            (updated.status, updated.error_code.as_str()),
            (402, "ServiceQuotaExceededException"),
            "case {name:?}: expected the update past the bound to be refused, got {updated:?}"
        );
        assert_eq!(
            updated.string(&["message"]),
            instance_attributes_quota_message(),
            "case {name:?}: expected the service's one wording for that bound, got {updated:?}"
        );
        // A refused update persists nothing: the instance reports exactly what the import
        // left, and the image set is still at its first version.
        let after = document(
            &get_image_set_metadata(&emulator, &datastore_id, &image_set_id, None).await,
            name,
        );
        assert_eq!(
            instance_dicom(&after, SOP_UID),
            instance_dicom(&before, SOP_UID),
            "case {name:?}: a refused update must write nothing, got {after}"
        );
        let read = get_image_set(&emulator, &datastore_id, &image_set_id, None).await;
        assert_eq!(
            read.string(&["versionId"]),
            "1",
            "case {name:?}: a refused update must mint no version, got {read:?}"
        );
    }
}

/// RULE, the other axis of that bound: what may go into ONE attribute of a SOP instance is
/// bounded by the BYTES of it. A metadata update writing a larger value is refused with the
/// modelled quota error and persists nothing, and an imported data set carrying one has that
/// attribute left out of the document rather than kept in part — nothing is ever reported
/// cut short.
///
/// The rows differ along which side of the bound the value lands on: the bytes one attribute
/// takes, and one byte past that.
#[tokio::test]
async fn an_attribute_larger_than_the_bound_is_refused_and_not_imported() {
    let base = two_instance_case();
    // (the case, how many bytes the attribute's value carries, whether it is kept)
    let rows = [
        ("up to the bound", MAX_IMAGE_SET_ATTRIBUTE_BYTES, true),
        ("one byte past it", MAX_IMAGE_SET_ATTRIBUTE_BYTES + 1, false),
    ];

    for (index, (name, bytes, kept)) in rows.iter().enumerate() {
        let emulator = Emulator::new();
        let datastore_id = emulator
            .active_datastore(
                &format!("attribute.bytes.{index}"),
                &format!("token-attribute-bytes-{index}"),
            )
            .await;
        // The IMPORT half: a data set carrying that attribute, read by the crate's own
        // import reading.
        let files = vec![base.clone().with_comment(*bytes).part10(SOP_UID).0];
        let (image_set_id, _) = seed_imported_image_set(&emulator, name, &datastore_id, &files);

        let before = document(
            &get_image_set_metadata(&emulator, &datastore_id, &image_set_id, None).await,
            name,
        );
        let imported = instance_dicom(&before, SOP_UID);
        if *kept {
            assert_eq!(
                imported["ImageComments"].as_str().map(str::len),
                Some(*bytes),
                "case {name:?}: expected the attribute the bound takes to be reported whole, \
                 got {imported}"
            );
        } else {
            assert!(
                imported["ImageComments"].is_null(),
                "case {name:?}: an attribute larger than the bound must be left out rather \
                 than kept in part, got {imported}"
            );
        }
        // Whatever the one over-large attribute did, the rest of the instance is reported.
        assert_eq!(
            imported["Rows"],
            json!(base.rows),
            "case {name:?}: expected the instance's other attributes to be reported, got \
             {imported}"
        );

        // The UPDATE half: the same value written through the operation that writes one.
        let written = json!({
            "SchemaVersion": "1.1",
            "Study": { "Series": { SERIES_UID: { "Instances": { SOP_UID: {
                "DICOM": { "ImageComments": "C".repeat(*bytes) },
            } } } } },
        });
        let updated = update_metadata(
            &emulator,
            &datastore_id,
            &image_set_id,
            "1",
            dicom_updates(Some(&written), None),
            &[],
        )
        .await;
        if *kept {
            assert_eq!(
                updated.status, 200,
                "case {name:?}: expected the write that fits the bound to be admitted, got \
                 {updated:?}"
            );
            poll_until_landed(&emulator, name, &datastore_id, &image_set_id, "2").await;
            let after = document(
                &get_image_set_metadata(&emulator, &datastore_id, &image_set_id, None).await,
                name,
            );
            assert_eq!(
                instance_dicom(&after, SOP_UID)["ImageComments"]
                    .as_str()
                    .map(str::len),
                Some(*bytes),
                "case {name:?}: expected the written attribute to be reported whole, got {after}"
            );
            continue;
        }
        assert_eq!(
            (updated.status, updated.error_code.as_str()),
            (402, "ServiceQuotaExceededException"),
            "case {name:?}: expected the write past the bound to be refused, got {updated:?}"
        );
        assert_eq!(
            updated.string(&["message"]),
            attribute_bytes_quota_message(),
            "case {name:?}: expected the service's one wording for that bound, got {updated:?}"
        );
        let after = document(
            &get_image_set_metadata(&emulator, &datastore_id, &image_set_id, None).await,
            name,
        );
        assert_eq!(
            instance_dicom(&after, SOP_UID),
            imported,
            "case {name:?}: a refused write must persist nothing, got {after}"
        );
        let read = get_image_set(&emulator, &datastore_id, &image_set_id, None).await;
        assert_eq!(
            read.string(&["versionId"]),
            "1",
            "case {name:?}: a refused write must mint no version, got {read:?}"
        );
    }
}

/// The member names `level` carries, sorted — what an instance object of the served
/// document is held to.
fn member_names(level: &Value) -> Vec<&str> {
    let mut names: Vec<&str> = level
        .as_object()
        .map(|object| object.keys().map(String::as_str).collect())
        .unwrap_or_default();
    names.sort_unstable();
    names
}

/// RULE 4 (above), over the table: whatever an import leaves in an image set, each instance
/// object of the served document carries the members the document has at that level and
/// nothing besides — and the checksum type it names is the algorithm the frame checksums
/// beside it were really computed with, which this test verifies by recomputing a reported
/// checksum with that very algorithm.
///
/// The rows differ along what the rule generalises over: how many SOP instances the image
/// set holds, whether an instance holds pixel data at all (and so has a frame to sum), the
/// transfer syntax the data set is encoded in, how wide its samples are, and whether the
/// bytes stored are the raster the instance declares.
#[tokio::test]
async fn the_instance_level_carries_the_members_the_document_has_and_names_its_checksum_type() {
    let with_pixel_data = two_instance_case();
    let mut without_pixel_data = two_instance_case();
    without_pixel_data.pixel_data = None;
    without_pixel_data.sample_range = None;
    let mut eight_bit = two_instance_case();
    eight_bit.transfer_syntax = IMPLICIT_VR_LE;
    eight_bit.rows = 2;
    eight_bit.columns = 2;
    eight_bit.bits_allocated = 8;
    eight_bit.bits_stored = 8;
    eight_bit.high_bit = 7;
    eight_bit.pixel_data = Some(PIXELS_8_UNSIGNED);
    let mut short_of_the_raster = two_instance_case();
    short_of_the_raster.rows = 4;
    short_of_the_raster.columns = 4;
    short_of_the_raster.pixel_data = Some(PIXELS_SHORT_OF_THE_RASTER);

    // (the case, each imported instance as its SOP Instance UID and the data set it carries)
    let rows: Vec<(&str, Vec<(&str, &Case)>)> = vec![
        (
            "one instance of 16-bit unsigned pixel data",
            vec![(SOP_UID, &with_pixel_data)],
        ),
        (
            "implicit VR, 8-bit pixel data",
            vec![(SOP_UID, &eight_bit)],
        ),
        (
            "pixel data that is not the raster the instance declares",
            vec![(SOP_UID, &short_of_the_raster)],
        ),
        (
            "an instance holding no pixel data",
            vec![(SOP_UID, &without_pixel_data)],
        ),
        (
            "two instances, one holding pixel data and one not",
            vec![
                (SOP_UID, &with_pixel_data),
                (OTHER_SOP_UID, &without_pixel_data),
            ],
        ),
    ];

    for (index, (name, imported)) in rows.iter().enumerate() {
        let emulator = Emulator::new();
        let datastore_id = emulator
            .active_datastore(
                &format!("instance.members.{index}"),
                &format!("token-instance-members-{index}"),
            )
            .await;
        let files: Vec<Vec<u8>> = imported
            .iter()
            .map(|(sop_uid, case)| case.part10(sop_uid).0)
            .collect();
        let (image_set_id, _) = seed_imported_image_set(&emulator, name, &datastore_id, &files);

        let read = document(
            &get_image_set_metadata(&emulator, &datastore_id, &image_set_id, None).await,
            name,
        );

        for (sop_uid, case) in imported {
            let instance = at(
                &read,
                &["Study", "Series", SERIES_UID, "Instances", sop_uid],
            );
            // An instance the image set holds no pixel data of has no frames to report and
            // no checksum to name a type for; every other member of the level is served
            // from what the import stored.
            let mut expected: Vec<&str> = INSTANCE_LEVEL_MEMBERS
                .iter()
                .copied()
                .filter(|member| {
                    !PIXEL_DATA_MEMBERS.contains(member) || case.pixel_data.is_some()
                })
                .collect();
            expected.sort_unstable();
            assert_eq!(
                member_names(&instance),
                expected,
                "case {name:?}: expected instance {sop_uid:?} to carry {expected:?} and \
                 nothing besides, got {instance}"
            );

            // What was computed about the frame is reported inside the frame's own entry:
            // the checksum of the pixel data it holds, at the raster the instance declares.
            // The type named at the instance level is the algorithm it was summed with, so
            // recomputing the checksum with that algorithm must give the reported value.
            let frames = &instance["ImageFrames"];
            let checksum_type = &instance["ChecksumType"];
            match case.pixel_data {
                None => {
                    assert!(
                        frames.is_null(),
                        "case {name:?}: instance {sop_uid:?} holds no pixel data, so it has no \
                         frames to report, got {frames}"
                    );
                    assert!(
                        checksum_type.is_null(),
                        "case {name:?}: instance {sop_uid:?} has no checksum to name a type \
                         for, got {checksum_type}"
                    );
                }
                Some(pixels) => {
                    assert_eq!(
                        checksum_type,
                        &json!(CHECKSUM_TYPE),
                        "case {name:?}: expected instance {sop_uid:?} to name the checksum \
                         type {CHECKSUM_TYPE:?}, got {checksum_type}"
                    );
                    let checksums = &frames[0]["PixelDataChecksumFromBaseToFullResolution"];
                    // `crc32` is the named algorithm, computed here over the very bytes the
                    // frame holds — so this asserts the document names the algorithm its
                    // checksums were really summed with, not merely that it names one.
                    let expected_checksums = json!([{
                        "Width": case.columns,
                        "Height": case.rows,
                        "Checksum": crc32(pixels),
                    }]);
                    assert_eq!(
                        checksums,
                        &expected_checksums,
                        "case {name:?}: expected instance {sop_uid:?} to report its frame's \
                         checksums as {expected_checksums}, summed with the type it names, \
                         got {checksums}"
                    );
                }
            }
        }
    }
}

/// RULE 4, the other half: the instance level is COMPUTED from what the image set holds, so
/// a caller writing a member of its own there cannot move what the service computes — the
/// checksum type stays the algorithm the frames were summed with — while a name the document
/// has nothing at is served from where it was written, and under no other instance.
///
/// The rows differ along the name written: the one the document already computes at that
/// level, and another name entirely.
#[tokio::test]
async fn a_caller_written_instance_member_never_moves_the_computed_checksum_type() {
    let case = "a caller-written instance member";
    let base = two_instance_case();
    let emulator = Emulator::new();
    let datastore_id = emulator
        .active_datastore("instance.written", "token-instance-written")
        .await;
    let files = vec![base.part10(SOP_UID).0, base.part10(OTHER_SOP_UID).0];
    let (image_set_id, _) = seed_imported_image_set(&emulator, case, &datastore_id, &files);

    let written = json!({
        "SchemaVersion": "1.1",
        "Study": { "Series": { SERIES_UID: { "Instances": { SOP_UID: {
            "ChecksumType": "SPOOFED",
            "PixelDataChecksumAlgorithmRevision": 7,
        } } } } },
    });
    let updated = update_metadata(
        &emulator,
        &datastore_id,
        &image_set_id,
        "1",
        dicom_updates(Some(&written), None),
        &[],
    )
    .await;
    assert_eq!(
        updated.status, 200,
        "case {case:?}: expected HTTP 200 from UpdateImageSetMetadata, got {updated:?}"
    );
    poll_until_landed(&emulator, case, &datastore_id, &image_set_id, "2").await;

    let read = document(
        &get_image_set_metadata(&emulator, &datastore_id, &image_set_id, None).await,
        case,
    );
    // (the member written, what the document must serve at it, whether the caller's value
    // is the one served)
    for (member, expected, from_the_caller) in [
        ("ChecksumType", json!(CHECKSUM_TYPE), false),
        ("PixelDataChecksumAlgorithmRevision", json!(7), true),
    ] {
        let path = ["Study", "Series", SERIES_UID, "Instances", SOP_UID, member];
        assert_eq!(
            at(&read, &path),
            expected,
            "case {case:?}: expected {member:?} to be served as {expected} (the caller's own \
             value: {from_the_caller}), got {}",
            at(&read, &path)
        );
        let elsewhere = [
            "Study",
            "Series",
            SERIES_UID,
            "Instances",
            OTHER_SOP_UID,
            member,
        ];
        if from_the_caller {
            assert!(
                at(&read, &elsewhere).is_null(),
                "case {case:?}: expected {member:?} under no instance it was not written \
                 for, got {} for {OTHER_SOP_UID:?}",
                at(&read, &elsewhere)
            );
        } else {
            // The instance the update did not name reports the computed value too: it is
            // computed per instance from the frames that instance holds.
            assert_eq!(
                at(&read, &elsewhere),
                expected,
                "case {case:?}: expected the untouched instance to report {member:?} as \
                 {expected}, got {}",
                at(&read, &elsewhere)
            );
        }
    }
}
