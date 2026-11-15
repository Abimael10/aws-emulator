// SPDX-License-Identifier: CC0-1.0

//! What a client can DO with an image set a DICOM import left behind: the rest of the
//! image-set surface, reached over HTTP and nothing else.
//!
//! RULE: an image set a completed import delivered is an ORDINARY image set of its data
//! store — held at version `1`, `ACTIVE` / `IMPORTED` — so the whole image-set surface
//! reaches it: `ListImageSetVersions` reports the one version it was imported at, and
//! `CopyImageSet` admits it as a SOURCE, landing a destination that carries the series'
//! DICOM attributes and serves the same image frames under the identifiers they were
//! imported under, while the source keeps the one version the import gave it.
//!
//! The sibling `crate::import_job_image_set_delivery` suite pins WHEN an import's delivery
//! becomes observable and what the readers report of it. What these rows pin is that the
//! image set it delivered is a first-class resource for the rest of the surface — the step
//! the client journey `CreateDatastore` -> `StartDICOMImportJob` -> `GetDICOMImportJob` ->
//! `CopyImageSet` ends on, which has no other source of an image set, since an import is
//! the only call that brings one into existence.
//!
//! The rows differ along what the rule generalises over: how many instances the import
//! read, how they spread over series and studies (one series, two series of one study, two
//! studies), and how many of them carry pixel data (all, some, none).
//!
//! Reading a real Amazon S3 prefix is `aws_emulator_router`'s `medical_imaging_dicom_import`
//! suite's case — S3 is mounted beside this service only there. Here the image sets an
//! import is to materialise are put in flight through the same seam the sibling suite uses
//! (the crate's own `imported_image_sets`), and every assertion reads the wire. Nothing
//! sleeps for a span: the import's due instants are moved, and a landing the clock owes is
//! polled for.

use std::time::{Duration, SystemTime};

use serde_json::json;

use service_com_amazonaws_medicalimaging_ahigatewayservice::state::ImportedInstance;

use crate::datastore_lifecycle::Emulator;
use crate::image_frames::get_image_frame;
use crate::image_set_copy::{copy_image_set, information};
use crate::image_set_metadata::{document, get_image_set_metadata};
use crate::image_set_metadata_updates::at;
use crate::image_set_version_listing::{
    list_versions, listed_versions, poll_until_landed, properties,
};
use crate::import_job_image_set_delivery::{
    instance, job_status, make_import_due, pixels, put_import_in_flight, start_import, InFlight,
    Instance,
};

/// How far out an import's due instants are pushed before a case moves them, so the window
/// the import is in flight in lasts the whole of that case instead of being raced.
const HELD: Duration = Duration::from_secs(3600);

/// One image set an import's completion is to deliver: the series it describes, the
/// attributes its files carried, and the SOP instances of it that carried pixel data.
struct DeliveredSeries {
    study_instance_uid: &'static str,
    series_instance_uid: &'static str,
    patient_name: &'static str,
    modality: &'static str,
    /// The SOP Instance UIDs that carry pixel data, each of which serves one image frame.
    frame_instances: Vec<&'static str>,
}

/// One row: the instances the import read, and the image sets its completion delivers, in
/// the order their study and series identifiers sort in — which is the order
/// `imported_image_sets` mints them in.
struct JourneyCase {
    case: &'static str,
    instances: Vec<Instance>,
    delivered: Vec<DeliveredSeries>,
}

/// The rows span: one instance of one series; two instances of one series, one of them
/// carrying no pixel data; two series of ONE study; two studies in one import; and a series
/// whose only instance carries no pixel data at all (an image set that is copyable while
/// serving no frame).
fn journey_cases() -> Vec<JourneyCase> {
    vec![
        JourneyCase {
            case: "one instance of one series",
            instances: vec![instance(
                "1.2.826.0.1.3680043.9.7201.1",
                "1.2.826.0.1.3680043.9.7201.1.1",
                "1.2.826.0.1.3680043.9.7201.1.1.1",
                "PARK^SOO",
                "CT",
                Some(pixels(11, 16)),
            )],
            delivered: vec![DeliveredSeries {
                study_instance_uid: "1.2.826.0.1.3680043.9.7201.1",
                series_instance_uid: "1.2.826.0.1.3680043.9.7201.1.1",
                patient_name: "PARK^SOO",
                modality: "CT",
                frame_instances: vec!["1.2.826.0.1.3680043.9.7201.1.1.1"],
            }],
        },
        JourneyCase {
            case: "two instances of one series, one of them carrying no pixel data",
            instances: vec![
                instance(
                    "1.2.826.0.1.3680043.9.7201.2",
                    "1.2.826.0.1.3680043.9.7201.2.1",
                    "1.2.826.0.1.3680043.9.7201.2.1.1",
                    "OKONKWO^ADA",
                    "MR",
                    Some(pixels(12, 32)),
                ),
                instance(
                    "1.2.826.0.1.3680043.9.7201.2",
                    "1.2.826.0.1.3680043.9.7201.2.1",
                    "1.2.826.0.1.3680043.9.7201.2.1.2",
                    "OKONKWO^ADA",
                    "MR",
                    None,
                ),
            ],
            delivered: vec![DeliveredSeries {
                study_instance_uid: "1.2.826.0.1.3680043.9.7201.2",
                series_instance_uid: "1.2.826.0.1.3680043.9.7201.2.1",
                patient_name: "OKONKWO^ADA",
                modality: "MR",
                frame_instances: vec!["1.2.826.0.1.3680043.9.7201.2.1.1"],
            }],
        },
        JourneyCase {
            case: "two series of one study",
            instances: vec![
                instance(
                    "1.2.826.0.1.3680043.9.7201.3",
                    "1.2.826.0.1.3680043.9.7201.3.1",
                    "1.2.826.0.1.3680043.9.7201.3.1.1",
                    "SILVA^ANA",
                    "MR",
                    Some(pixels(13, 24)),
                ),
                instance(
                    "1.2.826.0.1.3680043.9.7201.3",
                    "1.2.826.0.1.3680043.9.7201.3.2",
                    "1.2.826.0.1.3680043.9.7201.3.2.1",
                    "SILVA^ANA",
                    "US",
                    Some(pixels(14, 64)),
                ),
            ],
            delivered: vec![
                DeliveredSeries {
                    study_instance_uid: "1.2.826.0.1.3680043.9.7201.3",
                    series_instance_uid: "1.2.826.0.1.3680043.9.7201.3.1",
                    patient_name: "SILVA^ANA",
                    modality: "MR",
                    frame_instances: vec!["1.2.826.0.1.3680043.9.7201.3.1.1"],
                },
                DeliveredSeries {
                    study_instance_uid: "1.2.826.0.1.3680043.9.7201.3",
                    series_instance_uid: "1.2.826.0.1.3680043.9.7201.3.2",
                    patient_name: "SILVA^ANA",
                    modality: "US",
                    frame_instances: vec!["1.2.826.0.1.3680043.9.7201.3.2.1"],
                },
            ],
        },
        JourneyCase {
            case: "two studies in one import",
            instances: vec![
                instance(
                    "1.2.826.0.1.3680043.9.7201.4",
                    "1.2.826.0.1.3680043.9.7201.4.1",
                    "1.2.826.0.1.3680043.9.7201.4.1.1",
                    "NAKAMURA^REN",
                    "CT",
                    Some(pixels(15, 8)),
                ),
                instance(
                    "1.2.826.0.1.3680043.9.7201.5",
                    "1.2.826.0.1.3680043.9.7201.5.1",
                    "1.2.826.0.1.3680043.9.7201.5.1.1",
                    "HASSAN^NOUR",
                    "PT",
                    Some(pixels(16, 48)),
                ),
            ],
            delivered: vec![
                DeliveredSeries {
                    study_instance_uid: "1.2.826.0.1.3680043.9.7201.4",
                    series_instance_uid: "1.2.826.0.1.3680043.9.7201.4.1",
                    patient_name: "NAKAMURA^REN",
                    modality: "CT",
                    frame_instances: vec!["1.2.826.0.1.3680043.9.7201.4.1.1"],
                },
                DeliveredSeries {
                    study_instance_uid: "1.2.826.0.1.3680043.9.7201.5",
                    series_instance_uid: "1.2.826.0.1.3680043.9.7201.5.1",
                    patient_name: "HASSAN^NOUR",
                    modality: "PT",
                    frame_instances: vec!["1.2.826.0.1.3680043.9.7201.5.1.1"],
                },
            ],
        },
        JourneyCase {
            case: "a series whose only instance carries no pixel data",
            instances: vec![instance(
                "1.2.826.0.1.3680043.9.7201.6",
                "1.2.826.0.1.3680043.9.7201.6.1",
                "1.2.826.0.1.3680043.9.7201.6.1.1",
                "BERGER^LENA",
                "NM",
                None,
            )],
            delivered: vec![DeliveredSeries {
                study_instance_uid: "1.2.826.0.1.3680043.9.7201.6",
                series_instance_uid: "1.2.826.0.1.3680043.9.7201.6.1",
                patient_name: "BERGER^LENA",
                modality: "NM",
                frame_instances: Vec::new(),
            }],
        },
    ]
}

/// Submit one row's import over the production front door, put the image sets it read in
/// flight, and let the CLOCK complete it — answering the image sets its completion
/// delivered, in the order `imported_image_sets` minted them.
async fn completed_import(
    emulator: &Emulator,
    datastore_id: &str,
    case: &JourneyCase,
    client_token: &str,
) -> Vec<InFlight> {
    let name = case.case;
    let job_id = start_import(emulator, name, datastore_id, client_token).await;
    let read: Vec<ImportedInstance> = case.instances.iter().map(Instance::imported).collect();
    let in_flight = put_import_in_flight(
        emulator,
        datastore_id,
        &job_id,
        &read,
        SystemTime::now() + HELD,
    );
    assert_eq!(
        in_flight.len(),
        case.delivered.len(),
        "case {name:?}: expected one image set per DICOM series the import read, got {}",
        in_flight.len()
    );
    // The clock, not a reader, completes the import and delivers what it read.
    make_import_due(emulator, name, datastore_id, &job_id);
    assert_eq!(
        job_status(emulator, name, datastore_id, &job_id).await,
        "COMPLETED",
        "case {name:?}: expected the job COMPLETED once its completion fell due"
    );
    in_flight
}

/// RULE (above): an image set an import delivered is held at version 1, which is the one
/// version `ListImageSetVersions` reports for it, `ACTIVE` / `IMPORTED`.
#[tokio::test]
async fn an_imported_image_set_lists_the_one_version_it_was_imported_at() {
    let emulator = Emulator::new();
    let datastore_id = emulator
        .active_datastore("import.versions.store", "import-versions")
        .await;
    for (row, case) in journey_cases().into_iter().enumerate() {
        let name = case.case;
        let in_flight =
            completed_import(&emulator, &datastore_id, &case, &format!("versions-{row}")).await;
        for (series, one) in case.delivered.iter().zip(&in_flight) {
            let series_uid = series.series_instance_uid;
            let listed = list_versions(&emulator, &datastore_id, &one.image_set_id, "").await;
            assert_eq!(
                listed.status, 200,
                "case {name:?} series {series_uid:?}: expected HTTP 200 from \
                 ListImageSetVersions, got {listed:?}"
            );
            assert_eq!(
                listed_versions(&listed),
                vec!["1".to_string()],
                "case {name:?} series {series_uid:?}: expected the one version an import leaves, \
                 got {listed:?}"
            );
            let entries = properties(&listed);
            let entry = entries.first().cloned().unwrap_or(json!(null));
            assert_eq!(
                (
                    entry["imageSetId"].clone(),
                    entry["imageSetState"].clone(),
                    // `ImageSetProperties` spells this member with a leading capital
                    // (`smithy_query(query="shape",shape="ImageSetProperties")`), unlike
                    // the `imageSetWorkflowStatus` of a `GetImageSet` answer.
                    entry["ImageSetWorkflowStatus"].clone(),
                ),
                (
                    json!(one.image_set_id),
                    json!("ACTIVE"),
                    json!("IMPORTED"),
                ),
                "case {name:?} series {series_uid:?}: expected the listed version to be the \
                 imported image set, ACTIVE / IMPORTED, got {entry}"
            );
        }
    }
}

/// RULE (above): an image set an import delivered is an admissible SOURCE for a copy — the
/// destination carries the series' DICOM attributes and serves the same image frames — and
/// the copy leaves the source at the one version the import gave it.
#[tokio::test]
async fn an_imported_image_set_is_an_admissible_copy_source() {
    let emulator = Emulator::new();
    let datastore_id = emulator
        .active_datastore("import.copies.store", "import-copies")
        .await;
    for (row, case) in journey_cases().into_iter().enumerate() {
        let name = case.case;
        let in_flight =
            completed_import(&emulator, &datastore_id, &case, &format!("copies-{row}")).await;
        for (series, one) in case.delivered.iter().zip(&in_flight) {
            let series_uid = series.series_instance_uid;
            let copied = copy_image_set(
                &emulator,
                &datastore_id,
                &one.image_set_id,
                // Version 1 is the one an import leaves, and a copy is admitted only from
                // the source's CURRENT latest version.
                information("1", None, None),
                &[],
            )
            .await;
            assert_eq!(
                copied.status, 200,
                "case {name:?} series {series_uid:?}: expected HTTP 200 from CopyImageSet of an \
                 image set the import delivered, got {copied:?}"
            );
            let destination_id = copied.string(&["destinationImageSetProperties", "imageSetId"]);
            assert_ne!(
                destination_id, one.image_set_id,
                "case {name:?} series {series_uid:?}: a copy naming no destination mints a NEW \
                 image set, got {copied:?}"
            );

            // The clock lands the copy: the destination at its first version, and the
            // source back out of its read-only lock still at the ONE version the import
            // gave it — a copy versions its destination, never its source. Both report
            // `COPIED`, the status of what last happened to them. Polled for with the
            // crate's one poll of that condition.
            let waiting_on = format!("{name} / series {series_uid}");
            poll_until_landed(
                &emulator,
                &waiting_on,
                &datastore_id,
                &destination_id,
                "1",
                Some("COPIED"),
            )
            .await;
            poll_until_landed(
                &emulator,
                &waiting_on,
                &datastore_id,
                &one.image_set_id,
                "1",
                Some("COPIED"),
            )
            .await;

            // The attributes of the patient, the study and the series travel with the copy.
            let destination = document(
                &get_image_set_metadata(&emulator, &datastore_id, &destination_id, None).await,
                name,
            );
            for (path, value) in [
                (
                    vec!["Patient", "DICOM", "PatientName"],
                    json!(series.patient_name),
                ),
                (
                    vec!["Study", "DICOM", "StudyInstanceUID"],
                    json!(series.study_instance_uid),
                ),
                (
                    vec!["Study", "Series", series_uid, "DICOM", "SeriesInstanceUID"],
                    json!(series_uid),
                ),
                (
                    vec!["Study", "Series", series_uid, "DICOM", "Modality"],
                    json!(series.modality),
                ),
            ] {
                assert_eq!(
                    at(&destination, &path),
                    value,
                    "case {name:?} series {series_uid:?}: expected the copy to carry {path:?} as \
                     {value}, got {}",
                    at(&destination, &path)
                );
            }

            // And so does the pixel data of every instance that carried any, byte for byte,
            // under the image frame identifier it was imported under.
            for sop_instance_uid in &series.frame_instances {
                let (image_frame_id, pixel_data) =
                    one.frames.get(*sop_instance_uid).unwrap_or_else(|| {
                        panic!(
                            "case {name:?} series {series_uid:?}: expected an imported frame for \
                             {sop_instance_uid:?}, got {:?}",
                            one.frames.keys().collect::<Vec<&String>>()
                        )
                    });
                let served =
                    get_image_frame(&emulator, &datastore_id, &destination_id, image_frame_id)
                        .await;
                assert_eq!(
                    served.status, 200,
                    "case {name:?} series {series_uid:?}: expected HTTP 200 from GetImageFrame on \
                     the copy, got {served:?}"
                );
                assert_eq!(
                    served.body_bytes, *pixel_data,
                    "case {name:?} series {series_uid:?}: expected the imported pixel data of \
                     {sop_instance_uid:?} back from the copy, got {} byte(s)",
                    served.body_bytes.len()
                );
            }
        }
    }
}
