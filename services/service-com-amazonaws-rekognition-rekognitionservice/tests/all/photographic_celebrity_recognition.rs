// SPDX-License-Identifier: CC0-1.0

//! `RecognizeCelebrities` and `GetCelebrityInfo` on PHOTOGRAPH-like images over the wire
//! (`awsJson1_1`, `POST /` with `X-Amz-Target: RekognitionService.<Operation>`), dispatched
//! through the same [`MultiServiceRouter`] the production emulator mounts.
//!
//! THE RULE UNDER TEST, on the input a client actually submits: a photograph, whose every
//! square of background carries grain and a gradient and whose subject carries hair,
//! eyebrows, nostrils, lips, shading and clothing.
//!
//! 1. A face is MEASURED in such a picture. What makes the region a pair of dark features
//!    stands for a face is the face itself: the region from the brow to the chin and from
//!    temple to temple is in its majority that face's own skin. The box the proportions
//!    derive around the eyes runs above the hairline and below the chin and its corners fall
//!    outside the head, so on a photograph most of THAT is hair, clothing and background —
//!    which is why what is measured is the face and not the box.
//! 2. Every measured face is then NAMED by the celebrity catalogue this service holds: it is
//!    the entry whose reference face it most resembles of those the answer has not already
//!    named, reported on the `CelebrityFaces` side as a `Celebrity` carrying that entry's
//!    `Id`, `Name`, `Urls` and `KnownGender`, the resemblance measured for it as its
//!    `MatchConfidence` and the `ComparedFace` the detector measured as its `Face`. One entry
//!    is one person, so no two faces of one answer are named as the same person, and the `Id`
//!    reported is the one `GetCelebrityInfo` resolves to the same identity — "use the
//!    Celebrity ID property as a unique identifier for the celebrity … you will need the ID
//!    to identify the celebrity in a call to the GetCelebrityInfo operation".
//!
//! A face the catalogue has no entry left for, or none it resembles at all, stays on the
//! `UnrecognizedFaces` side; a picture of nobody is answered with both lists empty; and the
//! answer is a function of the submitted pixels, so the same photograph answers identically
//! twice over.
//!
//! Every photograph here is DRAWN from known distances and tones by the fixtures of the
//! enrolment suite, so each assertion compares the service's answer against where the test
//! put the person rather than against a recorded answer. The table spans what the rule
//! generalises over: how many people the picture holds and how large each is in the frame,
//! what else it holds (grain, a tonal gradient, hair, clothing, a subject turned away), the
//! light it was taken in — including a dark room, where the frame is in the majority not
//! skin at all — the size of the frame, and the format it is encoded in.

use image::ImageFormat;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The router harness, the dispatch helper and the reduced reply come from the
// collection-lifecycle suite, the drawn photographs from the enrolment suite and the JSON
// reader from the detection suite, rather than being built a second time here.
use crate::face_collections::{bytes_image, call, harness, Reply};
use crate::face_detection::{encode, fill_ellipse, next, number};
use crate::photographic_enrolment::{Photograph, Subject};

/// Every value a `KnownGender.Type` may carry: "the known gender identity can be Male,
/// Female, Nonbinary, or Unlisted" (`smithy_query(query="shape",shape="KnownGenderType")`).
const KNOWN_GENDERS: [&str; 4] = ["Male", "Female", "Nonbinary", "Unlisted"];

/// One `RecognizeCelebrities` call on one photograph's bytes.
async fn recognize(router: &MultiServiceRouter, bytes: &[u8]) -> Reply {
    call(
        router,
        "RecognizeCelebrities",
        json!({ "Image": bytes_image(bytes) }),
    )
    .await
}

/// One `GetCelebrityInfo` call on a celebrity identifier.
async fn celebrity_info(router: &MultiServiceRouter, id: &str) -> Reply {
    call(router, "GetCelebrityInfo", json!({ "Id": id })).await
}

/// Both sides of one served partition, with the envelope already checked.
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

/// The partition of a served answer: 200, both lists present, and the orientation correction
/// the member documents as always null absent.
fn served(case: &str, reply: &Reply) -> Partition {
    assert_eq!(
        reply.status, 200,
        "case {case:?}: a readable photograph is answered, got {} with {:?}",
        reply.status, reply.body
    );
    for list in ["CelebrityFaces", "UnrecognizedFaces"] {
        assert!(
            reply.field(list).is_some(),
            "case {case:?}: {list} is always present, got {:?}",
            reply.body
        );
    }
    assert!(
        reply.field("OrientationCorrection").is_none_or(Value::is_null),
        "case {case:?}: the orientation correction is always null, got {:?}",
        reply.body
    );
    Partition {
        celebrities: reply.list("CelebrityFaces"),
        unrecognized: reply.list("UnrecognizedFaces"),
    }
}

/// Whether the reported box of `face` holds the point `(x, y)` of a photograph `size` pixels
/// across. A `BoundingBox` carries fractions of the image's own width and height.
fn box_holds(face: &Value, size: (u32, u32), (x, y): (f32, f32)) -> bool {
    let left = number(face, &["BoundingBox", "Left"]);
    let top = number(face, &["BoundingBox", "Top"]);
    let width = number(face, &["BoundingBox", "Width"]);
    let height = number(face, &["BoundingBox", "Height"]);
    let x = f64::from(x) / f64::from(size.0);
    let y = f64::from(y) / f64::from(size.1);
    x >= left && x <= left + width && y >= top && y <= top + height
}

/// One photograph the table submits, and the format it is encoded in.
struct PhotographCase {
    case: &'static str,
    photograph: Photograph,
    format: ImageFormat,
}

/// The photographs every table below submits. Each row is a picture of one or more people,
/// drawn with the grain, the gradient, the hair, the shading and the clothing a photograph
/// carries; the rows differ in the number of subjects, their size and placing, the light,
/// the grain, the frame and the encoding.
fn photograph_cases() -> Vec<PhotographCase> {
    vec![
        PhotographCase {
            case: "a head-and-shoulders portrait",
            photograph: Photograph::portrait(),
            format: ImageFormat::Jpeg,
        },
        PhotographCase {
            case: "the same portrait as a PNG",
            photograph: Photograph::portrait(),
            format: ImageFormat::Png,
        },
        PhotographCase {
            case: "a close crop",
            photograph: Photograph::portrait()
                .with_subjects(vec![Subject::new(158.0, 120.0, 96.0)]),
            format: ImageFormat::Jpeg,
        },
        PhotographCase {
            case: "a small figure in a wide frame",
            photograph: Photograph::portrait()
                .with_size(640, 400)
                .with_subjects(vec![Subject::new(430.0, 150.0, 44.0)]),
            format: ImageFormat::Jpeg,
        },
        PhotographCase {
            case: "two people at different distances",
            photograph: Photograph::portrait()
                .with_size(480, 320)
                .with_subjects(vec![
                    Subject::new(140.0, 120.0, 56.0),
                    Subject::new(360.0, 135.0, 36.0),
                ]),
            format: ImageFormat::Jpeg,
        },
        PhotographCase {
            // A dark room: the ground behind the head, the hair and the clothing are all
            // darker than the skin, so most of the box the proportions derive is not skin.
            case: "a dim room",
            photograph: Photograph::portrait().with_light(40, 70, 120),
            format: ImageFormat::Jpeg,
        },
        PhotographCase {
            case: "a bright room",
            photograph: Photograph::portrait().with_light(200, 230, 170).with_grain(4),
            format: ImageFormat::Png,
        },
        PhotographCase {
            case: "heavier grain",
            photograph: Photograph::portrait().with_grain(14),
            format: ImageFormat::Jpeg,
        },
    ]
}

/// Every person a photograph holds is measured and NAMED: the face is reported on exactly one
/// side of the partition, and the celebrity it is reported as carries a well-formed identity
/// the catalogue holds — which `GetCelebrityInfo` then resolves to that same identity.
#[tokio::test]
async fn every_person_in_a_photograph_is_named_by_the_catalogue() {
    let router = harness();
    for PhotographCase {
        case,
        photograph,
        format,
    } in photograph_cases()
    {
        let bytes = photograph.bytes(format);
        let partition = served(case, &recognize(&router, &bytes).await);
        let size = (photograph.width, photograph.height);
        let people = photograph.subjects.len();

        // The partition covers the people the picture holds, each face once.
        let faces = partition.faces();
        assert_eq!(
            faces.len(),
            people,
            "case {case:?}: the picture holds {people} person(s), so that many faces are \
             reported, got {} celebrity face(s) and {} unrecognized face(s)",
            partition.celebrities.len(),
            partition.unrecognized.len()
        );
        for subject in &photograph.subjects {
            for (name, point) in [
                ("the left eye", subject.left_eye()),
                ("the right eye", subject.right_eye()),
                ("the mouth", subject.mouth_centre()),
            ] {
                let holding = faces
                    .iter()
                    .filter(|face| box_holds(face, size, point))
                    .count();
                assert_eq!(
                    holding, 1,
                    "case {case:?}: exactly one reported face holds {name} at {point:?}, got \
                     {holding} of {faces:?}"
                );
            }
        }

        // Every measured face is a celebrity the catalogue names: the picture holds people
        // the catalogue has entries for, so none of them is left anonymous.
        assert_eq!(
            partition.celebrities.len(),
            people,
            "case {case:?}: the {people} measured face(s) are named by the catalogue, got {} \
             celebrity face(s) and {} unrecognized face(s)",
            partition.celebrities.len(),
            partition.unrecognized.len()
        );

        let mut named: Vec<String> = Vec::new();
        for celebrity in &partition.celebrities {
            let id = celebrity
                .get("Id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            // `Id` is a `RekognitionUniqueId`, `pattern ^[0-9A-Za-z]*$`, and it has to name an
            // entry, so it is not empty.
            assert!(
                !id.is_empty() && id.chars().all(|character| character.is_ascii_alphanumeric()),
                "case {case:?}: a celebrity's Id is a non-empty RekognitionUniqueId, got {id:?}"
            );
            assert!(
                !named.contains(&id),
                "case {case:?}: one entry is one person, so {id:?} is named once, got {:?}",
                partition.celebrities
            );
            named.push(id.clone());

            let name = celebrity
                .get("Name")
                .and_then(Value::as_str)
                .unwrap_or_default();
            assert!(
                !name.is_empty(),
                "case {case:?}: a recognized celebrity carries the name of the celebrity, got \
                 {celebrity:?}"
            );
            let urls = celebrity.get("Urls").and_then(Value::as_array).cloned();
            assert!(
                urls.is_some(),
                "case {case:?}: Urls is present as a list, got {celebrity:?}"
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
            // `MatchConfidence` is a `Percent`, `range 0..100`, and it is the resemblance
            // measured between the face and that entry's reference face, so it is a
            // resemblance at all.
            let confidence = number(celebrity, &["MatchConfidence"]);
            assert!(
                confidence > 0.0 && confidence <= 100.0,
                "case {case:?}: a MatchConfidence is a measured percentage above zero, got \
                 {confidence}"
            );
            // The face it was measured as, on this side of the partition only.
            let measured = celebrity.get("Face").cloned().unwrap_or(Value::Null);
            assert!(
                measured.get("BoundingBox").is_some() && measured.get("Confidence").is_some(),
                "case {case:?}: a celebrity carries the face the detector measured, got \
                 {celebrity:?}"
            );

            // The documented lookup chain closes: the Id resolves to the same identity.
            let looked_up = celebrity_info(&router, &id).await;
            assert_eq!(
                looked_up.status, 200,
                "case {case:?}: the Id {id:?} the answer handed out resolves, got {} with {:?}",
                looked_up.status, looked_up.body
            );
            assert_eq!(
                looked_up.field("Name").and_then(Value::as_str),
                Some(name),
                "case {case:?}: GetCelebrityInfo names the same celebrity for {id:?}, got {:?}",
                looked_up.body
            );
            assert_eq!(
                looked_up.field("Urls").and_then(Value::as_array).cloned(),
                urls,
                "case {case:?}: GetCelebrityInfo carries the same links for {id:?}, got {:?}",
                looked_up.body
            );
            assert_eq!(
                looked_up
                    .field("KnownGender")
                    .and_then(|known| known.get("Type"))
                    .and_then(Value::as_str),
                Some(gender),
                "case {case:?}: GetCelebrityInfo carries the same known gender for {id:?}, \
                 got {:?}",
                looked_up.body
            );
        }
    }
}

/// The seed the grain over a re-touched photograph is drawn from, so one picture is one set
/// of bytes however often the test is run.
const RETOUCH_SEED: u64 = 0x5245_4b5f_4652_494e;

/// The same subject wearing their hair in a FRINGE and a high collar, which is what makes
/// the box the proportions derive around the eyes in its majority NOT the face: hair across
/// the top of it, the garment across the bottom, the room at its corners. The face itself —
/// from the brow to the chin, from temple to temple — is still skin, and that is the region
/// the detector reads a face off.
fn with_a_fringe_and_a_high_collar(photograph: &Photograph, format: ImageFormat) -> Vec<u8> {
    let mut pixels = photograph.render();
    // The hair and the garment are darker than the skin, the way they are drawn behind the
    // head by the enrolment suite's own renderer.
    let hair = (f32::from(photograph.skin) * 0.28).round().clamp(0.0, 255.0) as u8;
    let garment = (f32::from(photograph.skin) * 0.35).round().clamp(0.0, 255.0) as u8;
    for subject in &photograph.subjects {
        let (left_x, eyes_y) = subject.left_eye();
        let (right_x, _) = subject.right_eye();
        let span = right_x - left_x;
        let eyes_x = (left_x + right_x) / 2.0;
        // The fringe: hair from above the hairline down to just above the brow, and out past
        // the sides of the head.
        fill_ellipse(
            &mut pixels,
            eyes_x,
            eyes_y - 1.0 * span,
            1.2 * span,
            0.64 * span,
            hair,
        );
        // The collar: the garment up to just below the chin, and out past the shoulders.
        fill_ellipse(
            &mut pixels,
            eyes_x,
            eyes_y + 1.95 * span,
            1.5 * span,
            0.63 * span,
            garment,
        );
    }
    // The grain of the film over what was drawn, so the hair and the garment are no flatter
    // than the rest of the picture.
    let mut state = RETOUCH_SEED;
    for pixel in pixels.pixels_mut() {
        let draw = (next(&mut state) % 7) as i32 - 3;
        pixel.0[0] = (i32::from(pixel.0[0]) + draw).clamp(0, 255) as u8;
    }
    encode(&pixels, format)
}

/// A face is measured off THE FACE, not off the box the proportions derive around its eyes:
/// a person whose hair falls across their forehead and whose collar comes up under their
/// chin is measured and named like any other, even though hair, garment and background take
/// up most of that box. The table spans the light the picture was taken in, the size of the
/// subject in the frame and the encoded format.
#[tokio::test]
async fn a_face_whose_box_is_mostly_hair_collar_and_room_is_still_measured_and_named() {
    let router = harness();
    let cases: Vec<(&str, Photograph, ImageFormat)> = vec![
        (
            "a fringe and a high collar",
            Photograph::portrait(),
            ImageFormat::Jpeg,
        ),
        (
            "the same picture as a PNG",
            Photograph::portrait(),
            ImageFormat::Png,
        ),
        (
            "a fringe and a high collar in a dim room",
            Photograph::portrait().with_light(40, 70, 120),
            ImageFormat::Jpeg,
        ),
        (
            "a fringe and a high collar in a bright room",
            Photograph::portrait().with_light(200, 230, 170).with_grain(4),
            ImageFormat::Png,
        ),
        (
            "a close crop with a fringe and a high collar",
            Photograph::portrait().with_subjects(vec![Subject::new(158.0, 130.0, 92.0)]),
            ImageFormat::Jpeg,
        ),
        (
            "a smaller subject with a fringe and a high collar",
            Photograph::portrait()
                .with_size(480, 360)
                .with_subjects(vec![Subject::new(230.0, 140.0, 48.0)]),
            ImageFormat::Jpeg,
        ),
    ];
    for (case, photograph, format) in cases {
        let bytes = with_a_fringe_and_a_high_collar(&photograph, format);
        let partition = served(case, &recognize(&router, &bytes).await);
        let size = (photograph.width, photograph.height);
        assert_eq!(
            partition.celebrities.len(),
            photograph.subjects.len(),
            "case {case:?}: the {} person(s) in the picture are named by the catalogue, got {} \
             celebrity face(s) and {} unrecognized face(s)",
            photograph.subjects.len(),
            partition.celebrities.len(),
            partition.unrecognized.len()
        );
        let faces = partition.faces();
        for subject in &photograph.subjects {
            for (name, point) in [
                ("the left eye", subject.left_eye()),
                ("the right eye", subject.right_eye()),
                ("the mouth", subject.mouth_centre()),
            ] {
                let holding = faces
                    .iter()
                    .filter(|face| box_holds(face, size, point))
                    .count();
                assert_eq!(
                    holding, 1,
                    "case {case:?}: exactly one reported face holds {name} at {point:?}, got \
                     {holding} of {faces:?}"
                );
            }
        }
    }
}

/// A photograph with nobody in it names nobody: neither the grain and the gradient of an/// empty frame nor a subject photographed from behind is a face, so both sides of the
/// partition are empty and the answer is still a 200.
#[tokio::test]
async fn a_photograph_of_nobody_names_nobody() {
    let router = harness();
    let cases: Vec<(&str, Photograph)> = vec![
        (
            "an empty room",
            Photograph::portrait().with_subjects(Vec::new()),
        ),
        (
            "an empty room in a wide frame",
            Photograph::portrait()
                .with_size(480, 320)
                .with_subjects(Vec::new()),
        ),
        (
            "one subject photographed from behind",
            Photograph::portrait()
                .with_subjects(vec![Subject::new(160.0, 128.0, 60.0).turned_away()]),
        ),
        (
            "a grainier empty frame",
            Photograph::portrait()
                .with_subjects(Vec::new())
                .with_grain(16),
        ),
    ];
    for (case, photograph) in cases {
        let bytes = photograph.bytes(ImageFormat::Jpeg);
        let partition = served(case, &recognize(&router, &bytes).await);
        assert!(
            partition.celebrities.is_empty() && partition.unrecognized.is_empty(),
            "case {case:?}: a picture of nobody reports nobody, got {} celebrity face(s) and \
             {} unrecognized face(s)",
            partition.celebrities.len(),
            partition.unrecognized.len()
        );
    }
}

/// The answer is a function of the submitted pixels and of the seeded catalogue: the same/// photograph is recognized identically twice over, and the identity handed out the second
/// time is the same person as the first.
#[tokio::test]
async fn the_same_photograph_is_recognized_identically_twice() {
    let router = harness();
    for PhotographCase {
        case,
        photograph,
        format,
    } in photograph_cases()
    {
        let bytes = photograph.bytes(format);
        let first = recognize(&router, &bytes).await;
        let second = recognize(&router, &bytes).await;
        assert_eq!(
            first.body, second.body,
            "case {case:?}: one photograph answers the same twice, got {:?} then {:?}",
            first.body, second.body
        );
    }
}
