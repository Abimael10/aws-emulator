// SPDX-License-Identifier: CC0-1.0

//! `RecognizeCelebrities` and `GetCelebrityInfo` end to end over the wire (`awsJson1_1`,
//! `POST /` with `X-Amz-Target: RekognitionService.<Operation>`), dispatched through the same
//! [`MultiServiceRouter`] the production emulator mounts.
//!
//! THE RULE UNDER TEST: `RecognizeCelebrities` PARTITIONS the faces measured in the submitted
//! image — "it lists the recognized celebrities in the CelebrityFaces array and any
//! unrecognized faces in the UnrecognizedFaces array" — so every face the detector measures is
//! reported on EXACTLY ONE side of it and none is left out. A face that resolves to an entry of
//! the celebrity catalogue this service holds is reported on the CelebrityFaces side as a
//! `Celebrity`, carrying that entry's `Id`, `Name`, `Urls` and `KnownGender`, the
//! `MatchConfidence` measured for it and, as its `Face`, the `ComparedFace` the detector
//! measured; it is NOT repeated among the unrecognized faces, and one entry is reported at most
//! once per answer. Every other measured face is reported on the UnrecognizedFaces side. The
//! `Id` reported is the key of the documented follow-on lookup — "use the Celebrity ID property
//! as a unique identifier for the celebrity … you will need the ID to identify the celebrity in
//! a call to the GetCelebrityInfo operation" — so `GetCelebrityInfo` resolves it to the same
//! name, links and known gender, and refuses an identifier the catalogue does not hold. Either
//! way a reported face carries the members the detector measured (a box, a confidence,
//! landmarks, a pose and a quality) and none a classifier would have had to judge; an image
//! carrying no face is answered with both lists empty rather than refused; the orientation
//! correction the member documents as always null is absent; an image this service cannot read
//! at all is refused with the modelled error for why; and nothing is persisted, so a later
//! caller sees exactly what it would have seen without the call.
//!
//! Every portrait here is DRAWN from known tones and distances by the fixtures of the
//! detection suite, so each assertion compares the service's answer against the picture the
//! test drew rather than against a recorded answer. The tables span what the rule generalises
//! over: how many faces an image carries (none, one, two of different sizes, three), where in
//! the frame they sit, their size, tilt and tone, the frame's size, the encoded format, how the
//! image is submitted (inline, from an S3 object), and how an image fails to be readable at
//! all.

use image::ImageFormat;
use serde_json::{json, Value};
// The floor a measured resemblance has to clear to BE a catalogue entry's celebrity, read
// from the service rather than restated here.
use service_com_amazonaws_rekognition_rekognitionservice::state::celebrity_catalogue::MINIMUM_RESEMBLANCE;
use smithy_server_runtime::MultiServiceRouter;

// The router harness, the dispatch helper and the reduced reply come from the
// collection-lifecycle suite, and the drawn portraits and the object-store harness from the
// detection suite, rather than being built a second time here.
use crate::face_collections::{bytes_image, call, harness, Reply};
use crate::face_detection::{
    encode, harness_with_object_store, number, s3, DrawnFace, Portrait, BUCKET,
};

/// Every member of a `ComparedFace` a classifier would have had to judge, which is nothing
/// this emulator runs (`smithy_query(query="shape",shape="ComparedFace")` lists the shape's
/// members; these two are the judged ones).
const UNJUDGED_MEMBERS: [&str; 2] = ["Emotions", "Smile"];

/// The members of a reported face the detector MEASURES, which are the members a
/// `ComparedFace` and a `FaceDetail` both carry
/// (`smithy_query(query="shape",shape="ComparedFace,FaceDetail")`).
const MEASURED_MEMBERS: [&str; 5] = ["BoundingBox", "Confidence", "Landmarks", "Pose", "Quality"];

/// Every value a `KnownGender.Type` may carry: "the known gender identity can be Male,
/// Female, Nonbinary, or Unlisted" (`smithy_query(query="shape",shape="KnownGenderType")`).
const KNOWN_GENDERS: [&str; 4] = ["Male", "Female", "Nonbinary", "Unlisted"];

/// One `RecognizeCelebrities` call on an image already in the `Image` form a request
/// carries.
async fn recognize(router: &MultiServiceRouter, image: Value) -> Reply {
    call(router, "RecognizeCelebrities", json!({ "Image": image })).await
}

/// One `GetCelebrityInfo` call on a celebrity identifier.
async fn celebrity_info(router: &MultiServiceRouter, id: &str) -> Reply {
    call(router, "GetCelebrityInfo", json!({ "Id": id })).await
}

/// One `DetectFaces` call: the faces the detector reports for the same pixels, through the
/// operation whose whole job is reporting them.
async fn detect(router: &MultiServiceRouter, image: Value) -> Reply {
    call(router, "DetectFaces", json!({ "Image": image })).await
}

/// Both sides of one served partition.
struct Partition {
    celebrities: Vec<Value>,
    unrecognized: Vec<Value>,
}

impl Partition {
    /// Every face this answer reported, whichever side it was reported on: a celebrity's own
    /// `Face` and an unrecognized face are both the `ComparedFace` the detector measured.
    fn faces(&self) -> Vec<Value> {
        let mut faces: Vec<Value> = self
            .celebrities
            .iter()
            .map(|celebrity| {
                celebrity
                    .get("Face")
                    .cloned()
                    .expect("a recognized celebrity carries the face it was measured as")
            })
            .collect();
        faces.extend(self.unrecognized.iter().cloned());
        faces
    }
}

/// The partition of a served answer, with what every answer must and must not carry already
/// checked: both lists present, no orientation correction, every reported face carrying the
/// measured members and none of the judged ones, and every celebrity carrying the identity its
/// catalogue entry holds, at most once per answer.
fn served(case: &str, reply: &Reply) -> Partition {
    assert_eq!(
        reply.status, 200,
        "case {case:?}: the recognition must be served, got {} with {:?}",
        reply.status, reply.body
    );
    // "Any returned values for this field included in an API response will always be NULL."
    assert!(
        reply.field("OrientationCorrection").is_none(),
        "case {case:?}: the orientation correction is always null, got {:?}",
        reply.body
    );
    for side in ["CelebrityFaces", "UnrecognizedFaces"] {
        assert!(
            reply.field(side).is_some(),
            "case {case:?}: the answer carries the {side} side of the partition, got {:?}",
            reply.body
        );
    }
    let partition = Partition {
        celebrities: reply.list("CelebrityFaces"),
        unrecognized: reply.list("UnrecognizedFaces"),
    };

    for face in &partition.faces() {
        for member in MEASURED_MEMBERS {
            assert!(
                face.get(member).is_some_and(|value| !value.is_null()),
                "case {case:?}: a reported face carries the measured {member}, got {face:?}"
            );
        }
        for member in UNJUDGED_MEMBERS {
            assert!(
                face.get(member).is_none_or(Value::is_null),
                "case {case:?}: no classifier ran, so {member} must be absent, got {face:?}"
            );
        }
        let confidence = number(face, &["Confidence"]);
        assert!(
            (0.0..=100.0).contains(&confidence),
            "case {case:?}: a confidence is a percentage, got {confidence}"
        );
    }

    let mut reported_ids: Vec<String> = Vec::new();
    for celebrity in &partition.celebrities {
        let id = celebrity
            .get("Id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        // `Id` is a `RekognitionUniqueId`, `pattern ^[0-9A-Za-z]*$`
        // (`smithy_query(query="shape",shape="RekognitionUniqueId")`), and it has to name an
        // entry, so it is never the empty string the pattern would also admit.
        assert!(
            !id.is_empty() && id.chars().all(|character| character.is_ascii_alphanumeric()),
            "case {case:?}: a celebrity's Id is a non-empty RekognitionUniqueId, got {id:?}"
        );
        assert!(
            !reported_ids.contains(&id),
            "case {case:?}: one entry is one person, so {id:?} is reported once, got {:?}",
            partition.celebrities
        );
        reported_ids.push(id);
        assert!(
            celebrity
                .get("Name")
                .and_then(Value::as_str)
                .is_some_and(|name| !name.is_empty()),
            "case {case:?}: a recognized celebrity carries its Name, got {celebrity:?}"
        );
        let urls = celebrity
            .get("Urls")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        assert!(
            !urls.is_empty()
                && urls
                    .iter()
                    .all(|url| url.as_str().is_some_and(|url| !url.is_empty())),
            "case {case:?}: a recognized celebrity carries the URLs its entry holds, \
             got {celebrity:?}"
        );
        let gender = celebrity
            .get("KnownGender")
            .and_then(|known| known.get("Type"))
            .and_then(Value::as_str)
            .unwrap_or_default();
        assert!(
            KNOWN_GENDERS.contains(&gender),
            "case {case:?}: a KnownGender.Type is one of {KNOWN_GENDERS:?}, got {gender:?}"
        );
        // `MatchConfidence` is a `Percent`, `range 0..100`, and it is the measured
        // resemblance that made this face THIS celebrity, so it clears the floor the
        // catalogue identifies above.
        let confidence = number(celebrity, &["MatchConfidence"]);
        assert!(
            confidence >= f64::from(MINIMUM_RESEMBLANCE) && confidence <= 100.0,
            "case {case:?}: a MatchConfidence is a measured percentage at or above \
             {MINIMUM_RESEMBLANCE}, got {confidence}"
        );
        assert!(
            celebrity.get("Face").is_some_and(|face| !face.is_null()),
            "case {case:?}: a recognized celebrity carries the face it was measured as, \
             got {celebrity:?}"
        );
    }
    partition
}

/// Whether the reported box of `face` holds the point the test drew a face's eyes at, in
/// an image of `size`. The box carries fractions of the image's own width and height.
fn box_holds(face: &Value, size: (u32, u32), eyes: (f32, f32)) -> bool {
    let left = number(face, &["BoundingBox", "Left"]);
    let top = number(face, &["BoundingBox", "Top"]);
    let width = number(face, &["BoundingBox", "Width"]);
    let height = number(face, &["BoundingBox", "Height"]);
    let x = f64::from(eyes.0) / f64::from(size.0);
    let y = f64::from(eyes.1) / f64::from(size.1);
    (left..=left + width).contains(&x) && (top..=top + height).contains(&y)
}

/// One row of the portrait table: the picture, the eye centres the test drew on it, and how
/// many faces it carries.
struct PortraitCase {
    case: &'static str,
    portrait: Portrait,
    eyes: Vec<(f32, f32)>,
}

/// The portraits every table below draws from: none, one, two or three faces, at several
/// positions, sizes, tilts, tones and frame sizes. Each row's face count is what the
/// detection suite pins for the same picture.
fn portrait_cases() -> Vec<PortraitCase> {
    vec![
        PortraitCase {
            case: "a flat light image with no face at all",
            portrait: Portrait::blank(320, 320, 235),
            eyes: Vec::new(),
        },
        PortraitCase {
            case: "a flat dark image with no face at all",
            portrait: Portrait::blank(240, 240, 12),
            eyes: Vec::new(),
        },
        PortraitCase {
            case: "one centred face",
            portrait: Portrait::one_face(),
            eyes: vec![(160.0, 150.0)],
        },
        PortraitCase {
            case: "two faces of different sizes",
            portrait: Portrait::one_face().with_size(480, 320).with_faces(vec![
                DrawnFace::new(130.0, 150.0, 56.0),
                DrawnFace::new(350.0, 140.0, 36.0),
            ]),
            eyes: vec![(130.0, 150.0), (350.0, 140.0)],
        },
        PortraitCase {
            case: "three faces across a wide frame",
            portrait: Portrait::one_face().with_size(640, 480).with_faces(vec![
                DrawnFace::new(150.0, 140.0, 56.0),
                DrawnFace::new(470.0, 140.0, 56.0),
                DrawnFace::new(310.0, 360.0, 56.0),
            ]),
            eyes: vec![(150.0, 140.0), (470.0, 140.0), (310.0, 360.0)],
        },
        PortraitCase {
            case: "a face tilted clockwise",
            portrait: Portrait::one_face()
                .with_faces(vec![DrawnFace::new(160.0, 150.0, 56.0).rolled(10.0)]),
            eyes: vec![(160.0, 150.0)],
        },
        PortraitCase {
            case: "a face tilted the other way",
            portrait: Portrait::one_face()
                .with_faces(vec![DrawnFace::new(160.0, 150.0, 56.0).rolled(-10.0)]),
            eyes: vec![(160.0, 150.0)],
        },
        PortraitCase {
            case: "a small face in a large frame",
            portrait: Portrait::one_face()
                .with_size(640, 480)
                .with_faces(vec![DrawnFace::new(300.0, 220.0, 56.0)]),
            eyes: vec![(300.0, 220.0)],
        },
        PortraitCase {
            case: "a close-up face larger than the frame",
            portrait: Portrait::one_face()
                .with_faces(vec![DrawnFace::new(160.0, 140.0, 110.0)]),
            eyes: vec![(160.0, 140.0)],
        },
        PortraitCase {
            case: "bright skin on a dark background",
            portrait: Portrait::one_face().with_tones(60, 220, 20),
            eyes: vec![(160.0, 150.0)],
        },
        PortraitCase {
            case: "a dim portrait",
            portrait: Portrait::one_face().with_tones(20, 90, 10),
            eyes: vec![(160.0, 150.0)],
        },
    ]
}

/// The partition covers every face the image carries, each on exactly ONE side of it: the
/// drawn faces are reported once each, and the two sides together are what the detector
/// measured.
#[tokio::test]
async fn every_face_the_image_carries_is_reported_on_exactly_one_side_of_the_partition() {
    let router = harness();
    for PortraitCase {
        case,
        portrait,
        eyes,
    } in portrait_cases()
    {
        let partition = served(case, &recognize(&router, bytes_image(&portrait.png())).await);
        let faces = partition.faces();
        assert_eq!(
            faces.len(),
            eyes.len(),
            "case {case:?}: the {} drawn face(s) must each be reported once, got {} celebrity \
             face(s) and {} unrecognized face(s)",
            eyes.len(),
            partition.celebrities.len(),
            partition.unrecognized.len()
        );
        let size = (portrait.width, portrait.height);
        for drawn in &eyes {
            let holding = faces
                .iter()
                .filter(|face| box_holds(face, size, *drawn))
                .count();
            assert_eq!(
                holding, 1,
                "case {case:?}: the face drawn at {drawn:?} is reported on exactly one side \
                 of the partition, got {holding} report(s) in {faces:?}"
            );
        }
    }
}

/// A face that resolves to the catalogue is reported as a CELEBRITY, and the identifier it is
/// reported under is the one `GetCelebrityInfo` resolves — to the same name, links and known
/// gender, by a read that changes nothing.
#[tokio::test]
async fn a_recognized_celebrity_resolves_through_get_celebrity_info() {
    let router = harness();
    let mut recognized = 0_usize;
    for PortraitCase {
        case,
        portrait,
        eyes,
    } in portrait_cases()
    {
        let partition = served(case, &recognize(&router, bytes_image(&portrait.png())).await);
        // A celebrity is one of the faces this answer measured, never an extra one, and the
        // faces that resolved to no entry are on the other side of the partition.
        assert!(
            partition.celebrities.len() <= eyes.len(),
            "case {case:?}: a celebrity is one of the {} measured face(s), got {} celebrity \
             face(s) and {} unrecognized face(s)",
            eyes.len(),
            partition.celebrities.len(),
            partition.unrecognized.len()
        );
        for celebrity in &partition.celebrities {
            recognized += 1;
            let id = celebrity
                .get("Id")
                .and_then(Value::as_str)
                .expect("a recognized celebrity carries its Id");
            let looked_up = celebrity_info(&router, id).await;
            assert_eq!(
                looked_up.status, 200,
                "case {case:?}: the Id {id:?} the recognition handed out must resolve, got {} \
                 with {:?}",
                looked_up.status, looked_up.body
            );
            for member in ["Name", "Urls", "KnownGender"] {
                assert_eq!(
                    looked_up.field(member),
                    celebrity.get(member),
                    "case {case:?}: the lookup of {id:?} reports the same {member} as the \
                     recognition, got {:?} against {celebrity:?}",
                    looked_up.body
                );
            }
            // The lookup is a READ: asking twice answers the same, and nothing about the
            // catalogue moved under it.
            let again = celebrity_info(&router, id).await;
            assert_eq!(
                again.body, looked_up.body,
                "case {case:?}: two lookups of {id:?} answer identically, got {:?} against {:?}",
                again.body, looked_up.body
            );
            // An identifier is a value, not a name to be matched loosely: one that differs
            // from this entry's only in case names no entry.
            let other_case = id.to_ascii_lowercase();
            if other_case != id {
                let miss = celebrity_info(&router, &other_case).await;
                assert_eq!(
                    miss.status, 400,
                    "case {case:?}: {other_case:?} is not the entry {id:?}, got {} with {:?}",
                    miss.status, miss.body
                );
            }
        }
    }
    // The point of the partition's celebrity side: a face CAN be reported as a celebrity, so
    // "did we recognize anyone?" is answerable and the documented lookup chain is reachable.
    assert!(
        recognized > 0,
        "the portraits of the table must include a face reported as a celebrity, got none"
    );
    // And a plain, well-lit portrait of ONE face is one of them: its single measured face
    // resolves to a catalogue entry rather than being left unrecognized.
    let plain = served(
        "one centred face",
        &recognize(&router, bytes_image(&Portrait::one_face().png())).await,
    );
    assert_eq!(
        plain.celebrities.len(),
        1,
        "case {:?}: the one measured face resolves to a catalogue entry, got {} celebrity \
         face(s) and {} unrecognized face(s)",
        "one centred face",
        plain.celebrities.len(),
        plain.unrecognized.len()
    );
}

/// An identifier the catalogue does not hold names no celebrity: the modelled
/// `ResourceNotFoundException`, never an invented entry and never an empty 200.
#[tokio::test]
async fn an_identifier_the_catalogue_does_not_hold_is_refused_as_not_found() {
    let router = harness();
    // Each row is a well-formed `RekognitionUniqueId` — `pattern ^[0-9A-Za-z]*$` — that no
    // entry of the catalogue is held under: a short one, a long one, and digits only.
    let absent = ["Q", "0", "99999999", "zzzzzzzzzzzzzzzz"];
    for id in absent {
        let reply = celebrity_info(&router, id).await;
        assert_eq!(
            reply.status, 400,
            "case {id:?}: expected HTTP 400, got {} with {:?}",
            reply.status, reply.body
        );
        for named in [reply.error_code(), reply.string("Code").unwrap_or_default()] {
            assert_eq!(
                named, "ResourceNotFoundException",
                "case {id:?}: expected ResourceNotFoundException, got {:?}",
                reply.body
            );
        }
        assert_eq!(
            reply.string("Message").as_deref(),
            Some("No Celebrity found with passed Id"),
            "case {id:?}: the refusal carries the service's wording for the class, got {:?}",
            reply.body
        );
        assert!(
            reply.field("Name").is_none() && reply.field("Urls").is_none(),
            "case {id:?}: a refusal reports no celebrity, got {:?}",
            reply.body
        );
    }
}

/// A reported face is the face the DETECTOR measured, member for member, whichever side of
/// the partition it was reported on: the same pixels answered through `DetectFaces` report the
/// same boxes, confidences, landmarks, poses and qualities. The table spans the frame's size
/// and the encoded format, which the rule generalises over.
#[tokio::test]
async fn a_reported_face_is_the_face_the_detector_measured() {
    let router = harness();
    // One drawn face per side and format, at the smallest eye span the detection suite pins
    // as found at that scale.
    let scales: [(&str, u32, f32); 3] = [
        ("160 px", 160, 28.0),
        ("320 px", 320, 56.0),
        ("640 px", 640, 112.0),
    ];
    let formats: [(&str, ImageFormat); 2] = [("PNG", ImageFormat::Png), ("JPEG", ImageFormat::Jpeg)];
    for (scale, side, eye_span) in scales {
        for (format_name, format) in formats {
            let case = format!("{scale}, {format_name}");
            let centre = f32::from(u16::try_from(side).expect("the test's sides are small"));
            let portrait = Portrait::one_face()
                .with_size(side, side)
                .with_faces(vec![DrawnFace::new(centre / 2.0, centre * 0.47, eye_span)]);
            let bytes = encode(&portrait.render(), format);

            let detected = detect(&router, bytes_image(&bytes)).await;
            assert_eq!(
                detected.status, 200,
                "case {case:?}: detecting must succeed, got {:?}",
                detected.body
            );
            let details = detected.list("FaceDetails");
            let partition = served(&case, &recognize(&router, bytes_image(&bytes)).await);
            let mut faces = partition.faces();
            assert_eq!(
                faces.len(),
                details.len(),
                "case {case:?}: the partition covers the {} detected face(s), got {faces:?}",
                details.len()
            );
            assert_eq!(
                faces.len(),
                1,
                "case {case:?}: one drawn face must be reported once, got {faces:?}"
            );
            for detail in &details {
                let found = faces.iter().position(|face| {
                    MEASURED_MEMBERS
                        .iter()
                        .all(|member| face.get(member) == detail.get(member))
                });
                let Some(found) = found else {
                    panic!(
                        "case {case:?}: the detected face {detail:?} must be reported with \
                         every member the detector measured, got {faces:?}"
                    );
                };
                faces.remove(found);
            }
        }
    }
}

/// One image answers the same whichever way it was submitted: inline bytes and the object
/// this emulator's Amazon S3 holds at the named location are read alike.
#[tokio::test]
async fn an_s3_sourced_image_is_recognized_like_inline_bytes() {
    let router = harness_with_object_store();
    assert_eq!(
        s3(&router, &format!("/{BUCKET}"), &[]).await,
        200,
        "the image bucket must be created first"
    );
    for PortraitCase {
        case,
        portrait,
        eyes,
    } in portrait_cases()
    {
        for key in ["portrait.png", "nested/prefix/portrait.png"] {
            let bytes = portrait.png();
            assert_eq!(
                s3(&router, &format!("/{BUCKET}/{key}"), &bytes).await,
                200,
                "case {case:?}: the image must be staged at {key}"
            );
            let staged = recognize(
                &router,
                json!({ "S3Object": { "Bucket": BUCKET, "Name": key } }),
            )
            .await;
            let inline = recognize(&router, bytes_image(&bytes)).await;
            let staged_faces = served(case, &staged).faces();
            assert_eq!(
                staged_faces.len(),
                eyes.len(),
                "case {case:?}: the staged image carries {} face(s), got {staged_faces:?}",
                eyes.len()
            );
            assert_eq!(
                staged.body, inline.body,
                "case {case:?}: the object at {key} must answer exactly as the same bytes \
                 submitted inline, got {:?} against {:?}",
                staged.body, inline.body
            );
        }
    }
}

/// The call is a READ of the submitted pixels and of the catalogue: it persists nothing, so a
/// second identical call answers identically and every stored resource is exactly as it was.
#[tokio::test]
async fn recognizing_celebrities_changes_nothing_a_later_caller_sees() {
    let router = harness();
    let collection = "celebrity-recognition";
    let created = call(
        &router,
        "CreateCollection",
        json!({ "CollectionId": collection }),
    )
    .await;
    assert_eq!(
        created.status, 200,
        "the collection must be created, got {:?}",
        created.body
    );
    let portrait = Portrait::one_face();
    let bytes = portrait.png();
    let indexed = call(
        &router,
        "IndexFaces",
        json!({ "CollectionId": collection, "Image": bytes_image(&bytes) }),
    )
    .await;
    assert_eq!(
        indexed.status, 200,
        "the face must be indexed, got {:?}",
        indexed.body
    );
    let before = call(
        &router,
        "DescribeCollection",
        json!({ "CollectionId": collection }),
    )
    .await;

    let first = recognize(&router, bytes_image(&bytes)).await;
    let second = recognize(&router, bytes_image(&bytes)).await;
    assert_eq!(
        served("first call", &first).faces().len(),
        1,
        "the face is reported"
    );
    assert_eq!(
        first.body, second.body,
        "case {:?}: two identical calls answer identically, got {:?} against {:?}",
        "repeated call", first.body, second.body
    );

    let after = call(
        &router,
        "DescribeCollection",
        json!({ "CollectionId": collection }),
    )
    .await;
    assert_eq!(
        before.body, after.body,
        "case {:?}: the stored collection is untouched by the call, got {:?} against {:?}",
        "described collection", after.body, before.body
    );
    let listed = call(&router, "ListCollections", json!({})).await;
    assert_eq!(
        listed.list("CollectionIds"),
        vec![Value::from(collection)],
        "case {:?}: no resource is created by the call, got {:?}",
        "listed collections",
        listed.body
    );
}

/// An image this service cannot read at all is refused with the modelled error for WHY,
/// never with an empty answer: a 200 carrying no face is reserved for an image that was
/// read and carried none.
#[tokio::test]
async fn an_unreadable_image_is_refused_with_the_modelled_error_for_why() {
    let router = harness();
    // Each row: what the request identifies as its image, and the modelled answer for it.
    // The messages are the service's own fixed wording for each class.
    let cases: Vec<(&str, Value, &str, &str)> = vec![
        (
            "bytes that are not an image at all",
            bytes_image(b"not-an-image-at-all"),
            "InvalidImageFormatException",
            "Request has invalid image format",
        ),
        (
            // The service reads PNG and JPEG — "the image must be either a PNG or JPEG
            // formatted file" — and the format comes from the bytes' own magic numbers, so
            // a GIF signature is content in a format it does not read.
            "bytes in an image format this service does not read",
            bytes_image(b"GIF89a\x01\x00\x01\x00\x80\x00\x00"),
            "InvalidImageFormatException",
            "Request has invalid image format",
        ),
        (
            "an image identifying no content at all",
            json!({}),
            "InvalidParameterException",
            "Requested image should either contain bytes or s3 object.",
        ),
        (
            "an S3 object the store does not serve",
            json!({ "S3Object": { "Bucket": "no-such-bucket", "Name": "no-such-key" } }),
            "InvalidS3ObjectException",
            "Unable to get object metadata from S3. Check object key, region and/or access \
             permissions.",
        ),
        (
            "an S3 object with no key",
            json!({ "S3Object": { "Bucket": "no-such-bucket" } }),
            "InvalidS3ObjectException",
            "Unable to get object metadata from S3. Check object key, region and/or access \
             permissions.",
        ),
    ];

    for (case, image, expected_code, expected_message) in cases {
        let reply = recognize(&router, image).await;
        assert_eq!(
            reply.status, 400,
            "case {case:?}: expected HTTP 400, got {} with {:?}",
            reply.status, reply.body
        );
        assert_eq!(
            reply.error_code(),
            expected_code,
            "case {case:?}: expected __type {expected_code}, got {:?}",
            reply.body
        );
        assert_eq!(
            reply.string("Code").as_deref(),
            Some(expected_code),
            "case {case:?}: expected Code {expected_code}, got {:?}",
            reply.body
        );
        assert_eq!(
            reply.string("Message").as_deref(),
            Some(expected_message),
            "case {case:?}: expected Message {expected_message:?}, got {:?}",
            reply.body
        );
        assert!(
            reply.field("UnrecognizedFaces").is_none() && reply.field("CelebrityFaces").is_none(),
            "case {case:?}: a refusal carries no face list, got {:?}",
            reply.body
        );
    }
}
