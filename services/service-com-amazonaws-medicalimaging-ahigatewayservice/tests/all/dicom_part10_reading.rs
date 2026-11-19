// SPDX-License-Identifier: CC0-1.0

//! What this service READS out of an object a DICOM import was pointed at, and how what it
//! read becomes image sets.
//!
//! RULE 1: an object is turned into an image set only when it really is a readable DICOM
//! Part 10 file — the 128-byte preamble and the `DICM` magic code of a file, or the magic
//! code of a stream, followed by a file meta group and a dataset the DICOM library can
//! decode. Anything else describes no image set and is passed over, so an import reports
//! nothing for it.
//!
//! RULE 2: the instances read become ONE image set per DICOM series of a study, holding
//! that series' attributes and one image frame per instance that carried pixel data; an
//! instance the file declared no study or series for belongs to no series and describes no
//! image set. The two study-level counts are ATTRIBUTES the instances carried, like every
//! other attribute of an image set — nothing about the import is counted into them.
//!
//! RULE 3: a value read out of a file is stored in a modelled `DICOMTags` member only when
//! it satisfies every constraint that member's shape declares — the kind it holds, its
//! `@length`, its `@pattern`, its `@range`. A file carrying a value its member cannot hold
//! describes no instance, and the reason the import reports for that object names the DICOM
//! attribute and the requirement its value failed, in the same words an update document
//! naming that attribute is refused with. The file's bytes are no more walked by the
//! boundary validator than an update document's blob is, so without that check a search
//! summary and a served metadata document would report values the model forbids.
//!
//! These are the readings themselves, over the crate's own public
//! `read_part10` / `imported_image_sets`, so the rules are covered where the bytes are.
//! `aws_emulator_router`'s `medical_imaging_dicom_import` suite drives the whole import end
//! to end over real Amazon S3 objects — including the malformed FILES (one truncated, one
//! declaring a value length far past what it carries) that only a written Part 10 file can
//! express.
//!
//! The rows differ along what each rule generalises over: for rule 1, HOW the bytes fail to
//! be a Part 10 file (empty, another format, a preamble with no magic code, a magic code
//! with nothing after it, a meta group that does not decode); for rule 2, how the instances
//! are spread over studies and series, how many of them carry pixel data, and how many of
//! them belong to no series at all; for rule 3, which constraint class the value fails
//! (maximum length, pattern, lower bound, upper bound), which member carries the attribute
//! and at which level of the DICOM hierarchy it sits, whether that member holds text or a
//! number, and which side of a bound the value sits on — each member's extreme LEGAL value
//! is read back off the image set, and a value past the bound is refused.

use serde_json::{json, Value};

use service_com_amazonaws_medicalimaging_ahigatewayservice::ops::types::DICOMTags;
use service_com_amazonaws_medicalimaging_ahigatewayservice::state::import_job_output::{
    job_output_objects, JOB_OUTPUT_FAILURE_KEY,
};
use service_com_amazonaws_medicalimaging_ahigatewayservice::state::{
    imported_image_sets, read_part10, ImageSetInstance, ImportPrefixRead, ImportedInstance,
    ModelledAttribute, PixelDescription, PrefixAnswer, ScannedObject,
};

/// The DICOM Part 10 magic code, which a file carries at byte 128 and a stream at byte 0.
const MAGIC: &[u8; 4] = b"DICM";

/// RULE 1: bytes that are not a readable DICOM Part 10 file describe no instance.
#[test]
fn bytes_that_are_not_a_readable_part10_file_are_passed_over() {
    // (case, the bytes)
    let rows: Vec<(&str, Vec<u8>)> = vec![
        ("empty", Vec::new()),
        (
            "another format entirely",
            br#"{"note":"not a DICOM file"}"#.to_vec(),
        ),
        ("a preamble with no magic code", vec![0_u8; 200]),
        ("the magic code and nothing after it", {
            let mut bytes = vec![0_u8; 128];
            bytes.extend_from_slice(MAGIC);
            bytes
        }),
        ("a stream whose magic code is all it has", MAGIC.to_vec()),
        ("a magic code followed by bytes that decode as no element", {
            let mut bytes = vec![0_u8; 128];
            bytes.extend_from_slice(MAGIC);
            bytes.extend_from_slice(&[0xff_u8; 64]);
            bytes
        }),
        ("the magic code one byte out of place", {
            let mut bytes = vec![0_u8; 127];
            bytes.extend_from_slice(MAGIC);
            bytes.extend_from_slice(&[0_u8; 64]);
            bytes
        }),
    ];

    for (case, bytes) in rows {
        let read = read_part10(&bytes);
        assert!(
            read.is_err(),
            "case {case:?}: expected the bytes to describe no instance, got {read:?}"
        );
    }
}

/// The study every instance below belongs to unless a row says otherwise.
const STUDY: &str = "1.2.826.0.1.3680043.9.7133.8.1";

/// One instance read out of an import, built directly so this suite covers the GROUPING
/// rule over instances rather than over files.
///
/// `study_counts` is the pair of study-level counts the instance's file DECLARED, which is
/// where an image set's own pair is read from.
fn instance(
    study: Option<&str>,
    series: Option<&str>,
    sop: &str,
    pixel_data: Option<Vec<u8>>,
    study_counts: (i32, i32),
) -> ImportedInstance {
    ImportedInstance {
        // The key of the object the instance came out of, which the grouping carries into
        // the image set it builds so a job's output manifest can name the input file.
        source_key: Some(format!("in/{sop}.dcm")),
        study_instance_uid: study.map(str::to_string),
        series_instance_uid: series.map(str::to_string),
        sop_instance_uid: Some(sop.to_string()),
        tags: DICOMTags {
            dicom_study_instance_uid: study.map(str::to_string),
            dicom_series_instance_uid: series.map(str::to_string),
            dicom_patient_id: Some("PID-GROUPED".to_string()),
            dicom_number_of_study_related_series: study_counts.0,
            dicom_number_of_study_related_instances: study_counts.1,
            ..DICOMTags::default()
        },
        // No data set wrote this instance: the grouping rule is about the instances, not
        // about the text a file spelled their attributes with.
        series_number_text: None,
        metadata: ImageSetInstance::default(),
        pixel_description: PixelDescription::default(),
        pixel_data,
    }
}

/// One row of the grouping table.
struct GroupingCase {
    case: &'static str,
    instances: Vec<ImportedInstance>,
    /// The series each image set must describe, in the order the identifiers sort in.
    series: Vec<&'static str>,
    /// How many frames each of those image sets must hold.
    frames: Vec<usize>,
    /// The two study-level counts every image set of the study must report — the pair its
    /// instances declared, and the modelled default of each member when they declared
    /// none.
    study_series: i32,
    study_instances: i32,
}

/// RULE 2: the instances read become one image set per series of a study, carrying that
/// series' frames and the study-level counts its instances declared; an instance belonging
/// to no series describes none.
#[test]
fn instances_become_one_image_set_per_series_of_a_study() {
    let pixels = vec![0x01_u8, 0x02, 0x03, 0x04];
    let other_pixels = vec![0x10_u8, 0x20, 0x30, 0x40];
    // The pair of study-level counts a row's files declare, which has nothing to do with
    // how many series or instances the row itself carries.
    let declared = (7_i32, 19_i32);
    let undeclared = (0_i32, 0_i32);
    let rows = vec![
        GroupingCase {
            case: "one instance of one series, declaring the study's counts",
            instances: vec![instance(
                Some(STUDY),
                Some("1.1"),
                "1.1.1",
                Some(pixels.clone()),
                declared,
            )],
            series: vec!["1.1"],
            frames: vec![1],
            study_series: declared.0,
            study_instances: declared.1,
        },
        GroupingCase {
            case: "two instances of one series, one of them carrying no pixel data and \
                   only one of them declaring the counts",
            instances: vec![
                instance(
                    Some(STUDY),
                    Some("1.1"),
                    "1.1.1",
                    Some(pixels.clone()),
                    declared,
                ),
                instance(Some(STUDY), Some("1.1"), "1.1.2", None, undeclared),
            ],
            series: vec!["1.1"],
            frames: vec![1],
            // An attribute one instance of the series records and another does not is
            // still the series'.
            study_series: declared.0,
            study_instances: declared.1,
        },
        GroupingCase {
            case: "two series of one study, whose files declare no counts",
            instances: vec![
                instance(
                    Some(STUDY),
                    Some("1.1"),
                    "1.1.1",
                    Some(pixels.clone()),
                    undeclared,
                ),
                instance(
                    Some(STUDY),
                    Some("1.2"),
                    "1.2.1",
                    Some(other_pixels.clone()),
                    undeclared,
                ),
            ],
            series: vec!["1.1", "1.2"],
            frames: vec![1, 1],
            // Two series were indexed, and neither image set reports two: the series
            // count is a study attribute, so the modelled default of its member stands
            // when the files declare none. The instance count the service can state from
            // what it indexed — one instance per series here, two for the study.
            study_series: 0,
            study_instances: 2,
        },
        GroupingCase {
            case: "instances with no series and with no study describe nothing",
            instances: vec![
                instance(Some(STUDY), None, "1.1.1", Some(pixels.clone()), declared),
                instance(None, Some("1.1"), "1.1.2", Some(pixels.clone()), declared),
                instance(None, None, "1.1.3", None, declared),
            ],
            series: vec![],
            frames: vec![],
            study_series: 0,
            study_instances: 0,
        },
        GroupingCase {
            case: "one placed series beside instances that belong to none",
            instances: vec![
                instance(Some(STUDY), None, "1.1.1", Some(pixels.clone()), declared),
                instance(
                    Some(STUDY),
                    Some("1.3"),
                    "1.3.1",
                    Some(pixels.clone()),
                    declared,
                ),
                instance(Some(STUDY), Some("1.3"), "1.3.2", Some(other_pixels), declared),
                instance(None, Some("1.4"), "1.4.1", Some(pixels), declared),
            ],
            series: vec!["1.3"],
            frames: vec![2],
            study_series: declared.0,
            study_instances: declared.1,
        },
    ];

    for row in &rows {
        let case = row.case;
        // The identifiers come from a minter of the test's own, so the rule is asserted
        // over what was MINTED per image set and per frame rather than over ids the
        // service happened to produce.
        let mut next = 0_u32;
        let mut minted = 0_usize;
        let mut mint = || {
            next += 1;
            minted += 1;
            format!("{next:032x}")
        };
        let imported = imported_image_sets(&row.instances, &mut mint);

        assert_eq!(
            imported.len(),
            row.series.len(),
            "case {case:?}: expected {} image set(s), got {imported:?}",
            row.series.len()
        );
        for (index, image_set) in imported.iter().enumerate() {
            let tags = &image_set.attributes.dicom_tags;
            assert_eq!(
                tags.dicom_series_instance_uid.as_deref(),
                Some(row.series[index]),
                "case {case:?}: expected the image sets in series order, got {imported:?}"
            );
            assert_eq!(
                tags.dicom_study_instance_uid.as_deref(),
                Some(STUDY),
                "case {case:?}: expected the image set to describe the study, got {image_set:?}"
            );
            assert_eq!(
                tags.dicom_number_of_study_related_series, row.study_series,
                "case {case:?}: expected the study's series count, got {image_set:?}"
            );
            assert_eq!(
                tags.dicom_number_of_study_related_instances, row.study_instances,
                "case {case:?}: expected the study's instance count, got {image_set:?}"
            );
            assert_eq!(
                image_set.image_frames.len(),
                row.frames[index],
                "case {case:?}: expected {} frame(s) for series {}, got {image_set:?}",
                row.frames[index],
                row.series[index]
            );
            // Every frame is reached by the identifier it is stored under, and carries the
            // instance its pixel data came from.
            for (image_frame_id, frame) in &image_set.image_frames {
                assert_eq!(
                    &frame.image_frame_id, image_frame_id,
                    "case {case:?}: a frame must be stored under its own identifier, got \
                     {frame:?}"
                );
                assert!(
                    frame.sop_instance_uid.is_some(),
                    "case {case:?}: a frame must carry the instance it came from, got {frame:?}"
                );
                assert!(
                    !frame.pixel_data.is_empty(),
                    "case {case:?}: a frame must carry the pixel data it was read from, got \
                     {frame:?}"
                );
                // An imported frame declares no stored transfer syntax: the file's own
                // encoding describes the file, not the frame this service stored.
                assert!(
                    frame.transfer_syntax_uid.is_none(),
                    "case {case:?}: an imported frame must declare no stored transfer syntax, \
                     got {frame:?}"
                );
            }
        }

        // One identifier per image set and one per frame, and not one more: nothing is
        // minted for an instance that describes no image set.
        let expected_minted = row.series.len() + row.frames.iter().sum::<usize>();
        assert_eq!(
            minted, expected_minted,
            "case {case:?}: expected {expected_minted} minted identifier(s), got {minted}"
        );
    }
}

// ---------------------------------------------------------------------------
// RULE 3: what a modelled member may hold, over real DICOM Part 10 files
// ---------------------------------------------------------------------------

// The writer of a DICOM Part 10 file the sibling `crate::imported_instance_metadata` suite
// already has — the data elements, the file meta group and the framing around them — rather
// than a second copy of it. What this suite adds is which ATTRIBUTES each file carries.
use crate::imported_instance_metadata::{
    part10_file, text, uid, Attribute, EXPLICIT_VR_LE, SOP_CLASS,
};

/// The SOP instance every file below is.
const SOP_UID: &str = "1.2.826.0.1.3680043.9.7133.8.1.1.1";

/// The series every file below belongs to unless its own row writes another.
const SERIES: &str = "1.2.826.0.1.3680043.9.7133.8.1.1";

/// The DICOM Part 10 file `attributes` describe, written in the ascending tag order DICOM
/// writes a data set in.
fn part10(attributes: &[Attribute]) -> Vec<u8> {
    let mut ordered = attributes.to_vec();
    ordered.sort_by_key(|attribute| (attribute.group, attribute.element));
    // The file declares no implementation class: what every row here is about is the data
    // set's own attributes.
    part10_file(SOP_UID, EXPLICIT_VR_LE, None, &ordered).0
}

/// The attributes every file of the table below carries, each a value the member it is
/// destined for admits — so the only value a case can be refused for is the one its own row
/// writes.
fn conforming_attributes() -> Vec<Attribute> {
    vec![
        uid(0x0008, 0x0016, SOP_CLASS),
        uid(0x0008, 0x0018, SOP_UID),
        text(0x0008, 0x0020, b"DA", "20240115"),
        text(0x0008, 0x0030, b"TM", "101500"),
        text(0x0008, 0x0050, b"SH", "ACC-1"),
        text(0x0008, 0x0060, b"CS", "CT"),
        text(0x0008, 0x1030, b"LO", "CHEST"),
        text(0x0010, 0x0010, b"PN", "DOE^JANE"),
        text(0x0010, 0x0020, b"LO", "PID-1"),
        text(0x0010, 0x0030, b"DA", "19800101"),
        text(0x0010, 0x0040, b"CS", "F"),
        text(0x0018, 0x0015, b"CS", "CHEST"),
        uid(0x0020, 0x000D, STUDY),
        uid(0x0020, 0x000E, SERIES),
        text(0x0020, 0x0010, b"SH", "STUDY-1"),
        text(0x0020, 0x0011, b"IS", "3"),
        text(0x0020, 0x1206, b"IS", "7"),
        text(0x0020, 0x1208, b"IS", "19"),
    ]
}

/// `filler` repeated `length` times — the value a case writes at a member's `@length`
/// maximum, or one character past it.
fn filled(length: usize, filler: char) -> String {
    filler.to_string().repeat(length)
}

/// The requirement a value longer than a member's `@length` maximum fails, worded as this
/// service words every constraint requirement.
fn over_length(max: usize) -> String {
    format!("Member must have length less than or equal to {max}")
}

/// The requirement a value outside the `@pattern` both UID members declare fails: the
/// expression without the `^` / `$` anchors, which is how this service advertises a pattern.
fn outside_pattern() -> String {
    "Member must satisfy regular expression pattern: [0-9.]+".to_string()
}

/// The requirement a number below a member's `@range` minimum fails.
fn under_value(min: i64) -> String {
    format!("Member must have value greater than or equal to {min}")
}

/// The requirement a number above a member's `@range` maximum fails.
fn over_value(max: i64) -> String {
    format!("Member must have value less than or equal to {max}")
}

/// One value a member cannot hold, as a data set writes it, and the requirement it fails.
struct Unholdable {
    case: &'static str,
    written: String,
    requirement: String,
}

/// One modelled attribute as a DICOM data set carries it: the tag it is written under, the
/// member it is destined for, the extreme value that member admits, and the values it
/// cannot hold.
struct MemberRow {
    case: &'static str,
    attribute: ModelledAttribute,
    group: u16,
    element: u16,
    vr: &'static [u8; 2],
    /// A value at the bound of what the member admits, as the data set writes it.
    legal: String,
    /// What the member must HOLD once that value has been read.
    held: Value,
    unholdable: Vec<Unholdable>,
}

/// A row for a member holding TEXT whose value representation takes free text, so a value
/// at its `@length` maximum — and one character past it — is a value a data set can write.
fn text_row(
    case: &'static str,
    attribute: ModelledAttribute,
    group: u16,
    element: u16,
    vr: &'static [u8; 2],
    max_length: usize,
    filler: char,
) -> MemberRow {
    MemberRow {
        case,
        attribute,
        group,
        element,
        vr,
        legal: filled(max_length, filler),
        held: json!(filled(max_length, filler)),
        unholdable: vec![Unholdable {
            case: "one character past the length maximum",
            written: filled(max_length + 1, filler),
            requirement: over_length(max_length),
        }],
    }
}

/// Every modelled member the table drives, with the constraints the model declares on it —
/// each bound, pattern and range below is read off
/// `smithy_query(query="shape",shape="DICOMTags")`.
fn member_rows() -> Vec<MemberRow> {
    let mut rows = vec![
        text_row(
            "the patient identifier (0010,0020), len 0..512",
            ModelledAttribute::PatientId,
            0x0010,
            0x0020,
            b"LO",
            512,
            'P',
        ),
        text_row(
            "the patient name (0010,0010), len 0..512",
            ModelledAttribute::PatientName,
            0x0010,
            0x0010,
            b"PN",
            512,
            'N',
        ),
        text_row(
            "the patient sex (0010,0040), len 0..16",
            ModelledAttribute::PatientSex,
            0x0010,
            0x0040,
            b"CS",
            16,
            'S',
        ),
        text_row(
            "the study identifier (0020,0010), len 0..512",
            ModelledAttribute::StudyId,
            0x0020,
            0x0010,
            b"SH",
            512,
            'I',
        ),
        text_row(
            "the study description (0008,1030), len 0..512",
            ModelledAttribute::StudyDescription,
            0x0008,
            0x1030,
            b"LO",
            512,
            'D',
        ),
        text_row(
            "the accession number (0008,0050), len 0..512",
            ModelledAttribute::AccessionNumber,
            0x0008,
            0x0050,
            b"SH",
            512,
            'A',
        ),
        text_row(
            "the series modality (0008,0060), len 0..32",
            ModelledAttribute::SeriesModality,
            0x0008,
            0x0060,
            b"CS",
            32,
            'M',
        ),
        text_row(
            "the series body part (0018,0015), len 0..128",
            ModelledAttribute::SeriesBodyPart,
            0x0018,
            0x0015,
            b"CS",
            128,
            'B',
        ),
    ];
    // The two UID members carry a `@pattern` on top of their length, and a UID a file
    // happens to write is exactly where a value outside it comes from.
    rows.push(MemberRow {
        case: "the study instance UID (0020,000D), len 0..512, pattern ^[0-9.]+$",
        attribute: ModelledAttribute::StudyInstanceUid,
        group: 0x0020,
        element: 0x000D,
        vr: b"UI",
        legal: filled(512, '9'),
        held: json!(filled(512, '9')),
        unholdable: vec![
            Unholdable {
                case: "one character past the length maximum",
                written: filled(513, '9'),
                requirement: over_length(512),
            },
            Unholdable {
                case: "a UID with a letter in it",
                written: "1.2.3.ABC".to_string(),
                requirement: outside_pattern(),
            },
        ],
    });
    rows.push(MemberRow {
        case: "the series instance UID (0020,000E), len 0..512, pattern ^[0-9.]+$",
        attribute: ModelledAttribute::SeriesInstanceUid,
        group: 0x0020,
        element: 0x000E,
        vr: b"UI",
        legal: filled(512, '8'),
        held: json!(filled(512, '8')),
        unholdable: vec![
            Unholdable {
                case: "one character past the length maximum",
                written: filled(513, '8'),
                requirement: over_length(512),
            },
            Unholdable {
                case: "a UID that is a name rather than a number",
                written: "SERIES-ONE".to_string(),
                requirement: outside_pattern(),
            },
        ],
    });
    // The two study counts are the members holding a NUMBER whose `@range` a data set can
    // write past: an Integer String is signed and carries more digits than a million.
    for (case, attribute, element) in [
        (
            "the number of study related series (0020,1206), range 0..1000000",
            ModelledAttribute::StudyRelatedSeries,
            0x1206_u16,
        ),
        (
            "the number of study related instances (0020,1208), range 0..1000000",
            ModelledAttribute::StudyRelatedInstances,
            0x1208_u16,
        ),
    ] {
        rows.push(MemberRow {
            case,
            attribute,
            group: 0x0020,
            element,
            vr: b"IS",
            legal: "1000000".to_string(),
            held: json!(1_000_000),
            unholdable: vec![
                Unholdable {
                    case: "one below the range minimum",
                    written: "-1".to_string(),
                    requirement: under_value(0),
                },
                Unholdable {
                    case: "one above the range maximum",
                    written: "1000001".to_string(),
                    requirement: over_value(1_000_000),
                },
            ],
        });
    }
    // The series number's `@range` is the whole of the 32-bit integer the model types it at,
    // so its upper bound is read back here and no written value lies outside it: an Integer
    // String past the 32-bit member is not a number the member parses at all.
    rows.push(MemberRow {
        case: "the series number (0020,0011), range -2147483648..2147483647",
        attribute: ModelledAttribute::SeriesNumber,
        group: 0x0020,
        element: 0x0011,
        vr: b"IS",
        legal: i32::MAX.to_string(),
        held: json!(i32::MAX),
        unholdable: Vec::new(),
    });
    // The three members holding a DATE or a TIME: a data set writes a date in eight
    // characters and a time in six, so a value at their `@length` bound is a MULTI-VALUED
    // element — the one form in which such an attribute runs past what its member holds.
    for (case, attribute, group, element, vr, one, legal_count, unholdable_count, max) in [
        (
            "the patient birth date (0010,0030), len 0..36",
            ModelledAttribute::PatientBirthDate,
            0x0010_u16,
            0x0030_u16,
            b"DA",
            "19800101",
            4_usize,
            5_usize,
            36_usize,
        ),
        (
            "the study date (0008,0020), len 0..36",
            ModelledAttribute::StudyDate,
            0x0008,
            0x0020,
            b"DA",
            "20240115",
            4,
            5,
            36,
        ),
        (
            "the study time (0008,0030), len 0..56",
            ModelledAttribute::StudyTime,
            0x0008,
            0x0030,
            b"TM",
            "101500",
            8,
            9,
            56,
        ),
    ] {
        let repeated = |count: usize| vec![one; count].join("\\");
        rows.push(MemberRow {
            case,
            attribute,
            group,
            element,
            vr,
            legal: repeated(legal_count),
            held: json!(repeated(legal_count)),
            unholdable: vec![Unholdable {
                case: "a multi-valued element past the length maximum",
                written: repeated(unholdable_count),
                requirement: over_length(max),
            }],
        });
    }
    rows
}

/// One attribute as a data set writes it: a UID padded with a NUL, any other text padded
/// with a space.
fn attribute_written(
    group: u16,
    element: u16,
    vr: &'static [u8; 2],
    written: &str,
) -> Attribute {
    if vr == b"UI" {
        uid(group, element, written)
    } else {
        text(group, element, vr, written)
    }
}

/// The data set `conforming_attributes` describes, with the attribute at `group`/`element`
/// written as `written` instead.
fn written_as(group: u16, element: u16, vr: &'static [u8; 2], written: &str) -> Vec<Attribute> {
    let mut attributes = conforming_attributes();
    attributes.retain(|attribute| (attribute.group, attribute.element) != (group, element));
    attributes.push(attribute_written(group, element, vr, written));
    attributes
}

/// How the import states an object whose data set carries a value one of the modelled
/// members cannot hold: the attribute's DICOM keyword and the requirement its value failed.
fn expected_reason(keyword: &str, requirement: &str) -> String {
    format!("The object's {keyword} is not a value an image set can hold: {requirement}")
}

/// RULE 3: a readable Part 10 file carrying a value a modelled member cannot hold describes
/// no instance, and the reason names the attribute and the requirement.
#[test]
fn a_file_whose_value_a_modelled_member_cannot_hold_describes_no_instance() {
    for row in member_rows() {
        let keyword = row.attribute.keyword();
        for unholdable in &row.unholdable {
            let case = format!("{}: {}", row.case, unholdable.case);
            let file = part10(&written_as(
                row.group,
                row.element,
                row.vr,
                &unholdable.written,
            ));

            let read = read_part10(&file);
            let reason = match &read {
                Err(reason) => reason.clone(),
                Ok(instance) => panic!(
                    "case {case:?}: expected the file to describe no instance, got \
                     {:?}",
                    instance.tags
                ),
            };
            assert_eq!(
                reason,
                expected_reason(keyword, &unholdable.requirement),
                "case {case:?}: expected the refusal to name the attribute and the \
                 requirement its value failed, got {reason:?}"
            );
            // The refused value is never quoted back: the members are `@sensitive`, so the
            // reason states the requirement and not the patient detail that failed it.
            assert!(
                !reason.contains(&unholdable.written),
                "case {case:?}: expected the reason to withhold the value it refused, got \
                 {reason:?}"
            );
        }
    }
}

/// RULE 3, from the other side: the extreme value each member ADMITS is read off the file
/// and held by that member, so the check turns away what the model forbids and nothing else.
#[test]
fn a_file_whose_values_every_member_admits_is_read_and_its_image_set_holds_them() {
    let rows = member_rows();
    // One file carrying every member's extreme legal value at once, so what the image set
    // holds for each member is what that one read put in it.
    let mut attributes = conforming_attributes();
    for row in &rows {
        attributes.retain(|attribute| {
            (attribute.group, attribute.element) != (row.group, row.element)
        });
        attributes.push(attribute_written(row.group, row.element, row.vr, &row.legal));
    }
    let case = "every modelled member at the bound of its constraints";

    let mut instance = read_part10(&part10(&attributes)).unwrap_or_else(|reason| {
        panic!("case {case:?}: expected the file to be read, got {reason:?}")
    });
    instance.source_key = Some("in/at-the-bounds.dcm".to_string());

    for row in &rows {
        assert_eq!(
            row.attribute.stored(&instance.tags),
            Some(row.held.clone()),
            "case {case:?}: expected the read to hold {:?} for {}, got {:?}",
            row.held,
            row.attribute.keyword(),
            row.attribute.stored(&instance.tags)
        );
    }

    // And the image set the instance describes holds them: the attributes a search summary
    // and the served metadata document report are these.
    let mut next = 0_u32;
    let mut mint = || {
        next += 1;
        format!("{next:032x}")
    };
    let imported = imported_image_sets(&[instance], &mut mint);
    assert_eq!(
        imported.len(),
        1,
        "case {case:?}: expected one image set for the one series read, got {imported:?}"
    );
    let tags = &imported[0].attributes.dicom_tags;
    for row in &rows {
        assert_eq!(
            row.attribute.stored(tags),
            Some(row.held.clone()),
            "case {case:?}: expected the image set to hold {:?} for {}, got {:?}",
            row.held,
            row.attribute.keyword(),
            row.attribute.stored(tags)
        );
    }
}

/// RULE 3, at the end of the chain: the object a value no member can hold was read out of
/// is reported as that object's own failure in the job's manifest, with the reason the read
/// gave — and it contributes no image set.
#[test]
fn an_object_a_modelled_member_turned_away_is_reported_as_that_objects_failure() {
    let case = "a study instance UID that is not a DICOM UID";
    let key = "in/out-of-constraint.dcm";
    let file = part10(&written_as(0x0020, 0x000D, b"UI", "1.2.3.ABC"));
    let reason = read_part10(&file)
        .err()
        .unwrap_or_else(|| panic!("case {case:?}: expected the file to describe no instance"));

    // The read as the import records it: the object was scanned, carried no instance, and
    // the reason is the one the reading gave.
    let read = ImportPrefixRead {
        answer: PrefixAnswer::Listed,
        objects: vec![ScannedObject {
            key: key.to_string(),
            instance: None,
            skipped: Some(reason.clone()),
        }],
    };
    let objects = job_output_objects("1234567890abcdef1234567890abcdef", "job-1", &read, &[], None);
    let failures = objects
        .iter()
        .find(|(name, _, _)| name.ends_with(JOB_OUTPUT_FAILURE_KEY))
        .map(|(_, _, body)| body.clone())
        .unwrap_or_else(|| panic!("case {case:?}: expected a failure manifest, got {objects:?}"));

    let records: Vec<Value> = failures
        .lines()
        .map(|line| serde_json::from_str(line).expect("a JSON record per line"))
        .collect();
    assert_eq!(
        records,
        vec![json!({ "inputFile": key, "message": reason })],
        "case {case:?}: expected the failure manifest to name the object and the reason, got \
         {failures:?}"
    );
}
