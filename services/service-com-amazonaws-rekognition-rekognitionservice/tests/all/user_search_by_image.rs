// SPDX-License-Identifier: CC0-1.0

//! `SearchUsersByImage` end to end over the wire (`awsJson1_1`, `POST /` with
//! `X-Amz-Target: RekognitionService.SearchUsersByImage`), dispatched through the same
//! router the production emulator mounts.
//!
//! THE RULE UNDER TEST: `SearchUsersByImage` measures the faces of the submitted image,
//! drops the ones the `QualityFilter`'s bar removes, searches with the LARGEST of the rest
//! and reports, out of the users of the collection the request named and no other, every
//! user whose measured match confidence — the resemblance between the searched face and the
//! faces that user holds — reaches `UserMatchThreshold` (80 by default), highest similarity
//! first, capped by `MaxUsers`, together with what the detector measured for the face the
//! search used, every face it did not use with the reason it was not used, and the
//! collection's own face model version. An image the search is left no face by is SERVED
//! with an empty match list and no searched face; a collection the store does not hold is
//! refused with HTTP 400 `ResourceNotFoundException`; and the search itself writes nothing.
//!
//! Every image here is a PORTRAIT drawn by the test through the fixtures of the detection
//! suite, every stored face got into its collection through `IndexFaces` and every user
//! through `CreateUser` plus `AssociateFaces`, so each assertion compares the service's
//! answer against the pictures the test drew and the users it enrolled. The tables span
//! what the rule generalises over: how the submitted picture differs from the one that was
//! enrolled (the same pixels, another scale, another exposure, another encoder, another
//! face), how it is submitted (inline, from an S3 object), how many faces a user holds, how
//! many faces the image carries, the threshold across its published range and past both of
//! its bounds, the cap across its range and past both of its bounds, each `QualityFilter`
//! level, and which collection is named.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The router harness, the dispatch helper and the reduced reply come from the
// collection-lifecycle suite, and the drawn portraits and the object-store harness from the
// detection suite, rather than being built a second time here.
use crate::face_collections::{bytes_image, call, harness, Reply};
use crate::face_detection::{
    blurred, encode, harness_with_object_store, number, s3, DrawnFace, Portrait, BUCKET,
};

/// The threshold `SearchUsersByImage` applies when the request names none: "specifies the
/// minimum confidence in the UserID match to return. Default value is 80"
/// (`smithy_query(query="doc",shape="SearchUsersByImageRequest",member="UserMatchThreshold")`).
const DEFAULT_USER_MATCH_THRESHOLD: f64 = 80.0;

/// `UserMatchThreshold` is `Percent`, `range 0..100`, and `MaxUsers` is `MaxUserResults`,
/// `range 1..500` (`smithy_query(query="operation",operation="SearchUsersByImage")`).
const PERCENT_MIN: f64 = 0.0;
const PERCENT_MAX: f64 = 100.0;
const MAX_USERS_MIN: i64 = 1;
const MAX_USERS_MAX: i64 = 500;

/// How near 100 the confidence of one face measured against itself must be: the enrolled
/// face and the submitted one were measured off the same pixels, so their correlation is one
/// but for the rounding of the arithmetic that computes it.
const SAME_PIXELS_CONFIDENCE: f64 = 99.5;

/// The face model version a collection created now is associated with, which every answer
/// about that collection reports.
const LATEST_FACE_MODEL_VERSION: &str = "7.0";

/// The `QualityFilter` levels, ordered by how high a bar each sets: `NONE` filters nothing
/// and `HIGH` the most (`smithy_query(query="doc",shape="SearchUsersByImageRequest",
/// member="QualityFilter")`: "filtered faces aren't searched for in the collection … the
/// default value is NONE"). `AUTO`, where "Amazon Rekognition chooses the quality bar", is
/// not in the order: nothing documents where its bar sits.
const QUALITY_LEVELS: [&str; 4] = ["NONE", "LOW", "MEDIUM", "HIGH"];

/// The `UnsearchedFaceReason` values this operation reports
/// (`smithy_query(query="shape",shape="UnsearchedFaceReason")`: FACE_NOT_LARGEST |
/// EXCEEDS_MAX_FACES | EXTREME_POSE | LOW_BRIGHTNESS | LOW_SHARPNESS | LOW_CONFIDENCE |
/// SMALL_BOUNDING_BOX | LOW_FACE_QUALITY).
const FACE_NOT_LARGEST: &str = "FACE_NOT_LARGEST";
const LOW_SHARPNESS: &str = "LOW_SHARPNESS";

/// The `UserStatus` values (`smithy_query(query="shape",shape="UserStatus")`: ACTIVE |
/// UPDATING | CREATING | CREATED). A user a face has been associated with carries one of the
/// first two — `UPDATING` while the association is still being applied, `ACTIVE` once it is
/// complete (`smithy_query(query="doc",shape="AssociateFaces")`) — so both are accepted
/// wherever a matched user's status is asserted, and `CREATED` is not.
const USER_STATUS_UPDATING: &str = "UPDATING";
const USER_STATUS_ACTIVE: &str = "ACTIVE";

/// The `Message` a search of a collection the store does not hold is refused with: the
/// wording the operations on the CONTENTS of a collection answer that class with, the
/// request's own id between two pieces of fixed text.
fn collection_does_not_exist(collection_id: &str) -> String {
    format!("The collection id: {collection_id} does not exist")
}

/// The upright portrait most cases enrol and search with.
fn upright() -> Portrait {
    Portrait::one_face()
}

/// The same face, tilted: a picture that differs from [`upright`] in what the resemblance
/// measurement reads and in nothing else.
fn tilted() -> Portrait {
    Portrait::one_face().with_faces(vec![DrawnFace::new(160.0, 150.0, 56.0).rolled(12.0)])
}

/// A third face, smaller and tilted the other way: the least alike of the three.
fn other() -> Portrait {
    Portrait::one_face().with_faces(vec![DrawnFace::new(160.0, 150.0, 30.0).rolled(-9.0)])
}

async fn create_collection(router: &MultiServiceRouter, collection_id: &str) {
    let reply = call(
        router,
        "CreateCollection",
        json!({ "CollectionId": collection_id }),
    )
    .await;
    assert_eq!(
        reply.status, 200,
        "the collection {collection_id} must be created, got {:?}",
        reply.body
    );
}

/// Index `image` into `collection_id` and answer the `FaceId` of every face stored from it.
async fn index(router: &MultiServiceRouter, collection_id: &str, image: &[u8]) -> Vec<String> {
    let reply = call(
        router,
        "IndexFaces",
        json!({ "CollectionId": collection_id, "Image": bytes_image(image) }),
    )
    .await;
    assert_eq!(
        reply.status, 200,
        "indexing into {collection_id} must succeed, got {:?}",
        reply.body
    );
    reply
        .list("FaceRecords")
        .iter()
        .map(|record| {
            record
                .get("Face")
                .and_then(|face| face.get("FaceId"))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string()
        })
        .collect()
}

/// A user of `collection_id`, created and holding nothing yet.
async fn create_user(router: &MultiServiceRouter, collection_id: &str, user_id: &str) {
    let reply = call(
        router,
        "CreateUser",
        json!({ "CollectionId": collection_id, "UserId": user_id }),
    )
    .await;
    assert_eq!(
        reply.status, 200,
        "the user {user_id} must be created in {collection_id}, got {:?}",
        reply.body
    );
}

/// Associate `face_ids` with `user_id`, at the floor `threshold` when one is given, and fail
/// the test unless every one of them joined the user.
async fn associate(
    router: &MultiServiceRouter,
    collection_id: &str,
    user_id: &str,
    face_ids: &[String],
    threshold: Option<f64>,
) {
    let mut body = json!({
        "CollectionId": collection_id,
        "UserId": user_id,
        "FaceIds": face_ids,
    });
    if let Some(threshold) = threshold {
        body["UserMatchThreshold"] = json!(threshold);
    }
    let reply = call(router, "AssociateFaces", body).await;
    assert_eq!(
        reply.status, 200,
        "associating {face_ids:?} with {user_id} must succeed, got {:?}",
        reply.body
    );
    assert_eq!(
        reply.list("AssociatedFaces").len(),
        face_ids.len(),
        "every face named must join {user_id}, got {:?}",
        reply.body
    );
}

/// Enrol `images` as the user `user_id` of `collection_id`: every face of every picture is
/// indexed and associated with that one user. The association floor is the bottom of the
/// range, so a user may hold faces that do not resemble each other — which is what lets a
/// user holding two faces be matched by a picture of either.
async fn enrol(
    router: &MultiServiceRouter,
    collection_id: &str,
    user_id: &str,
    images: &[Vec<u8>],
) -> Vec<String> {
    create_user(router, collection_id, user_id).await;
    let mut held: Vec<String> = Vec::new();
    for image in images {
        let faces = index(router, collection_id, image).await;
        assert!(
            !faces.is_empty(),
            "a picture enrolled as {user_id} must carry a face"
        );
        associate(router, collection_id, user_id, &faces, Some(PERCENT_MIN)).await;
        held.extend(faces);
    }
    held
}

/// One search of `collection_id` by an image already in the `Image` form the request carries.
async fn search_image(
    router: &MultiServiceRouter,
    collection_id: &str,
    image: Value,
    extra: Value,
) -> Reply {
    let mut body = extra;
    let members = body
        .as_object_mut()
        .expect("the extra members are an object");
    members.insert("CollectionId".to_string(), json!(collection_id));
    members.insert("Image".to_string(), image);
    call(router, "SearchUsersByImage", body).await
}

/// One search by an image submitted inline.
async fn search(
    router: &MultiServiceRouter,
    collection_id: &str,
    image: &[u8],
    extra: Value,
) -> Reply {
    search_image(router, collection_id, bytes_image(image), extra).await
}

/// The matched users of a served search, with what every answer must carry already checked.
fn served(case: &str, reply: &Reply) -> Vec<Value> {
    assert_eq!(
        reply.status, 200,
        "case {case:?}: the search must be served, got {} with {:?}",
        reply.status, reply.body
    );
    assert_eq!(
        reply.string("FaceModelVersion").as_deref(),
        Some(LATEST_FACE_MODEL_VERSION),
        "case {case:?}: the answer carries the collection's own face model version, got {:?}",
        reply.body
    );
    let matches = reply.list("UserMatches");
    for (position, entry) in matches.iter().enumerate() {
        let similarity = number(entry, &["Similarity"]);
        assert!(
            (PERCENT_MIN..=PERCENT_MAX).contains(&similarity),
            "case {case:?}: match {position} reports a Percent similarity, got {similarity}"
        );
        let status = entry
            .get("User")
            .and_then(|user| user.get("UserStatus"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        assert!(
            [USER_STATUS_UPDATING, USER_STATUS_ACTIVE].contains(&status.as_str()),
            "case {case:?}: match {position} names a user holding a face, whose status is \
             UPDATING or ACTIVE, got {status:?} in {entry:?}"
        );
    }
    matches
}

/// The searched face of a served answer: its `FaceDetail`, with the two members the
/// operation documents it by already checked.
fn searched_face(case: &str, reply: &Reply) -> Value {
    let detail = reply
        .field("SearchedFace")
        .and_then(|face| face.get("FaceDetail"))
        .cloned();
    let detail = detail.unwrap_or_else(|| {
        panic!("case {case:?}: the face the search used must be reported, got {:?}", reply.body)
    });
    assert!(
        detail.get("BoundingBox").is_some(),
        "case {case:?}: the box measured for the searched face must be reported, got {detail:?}"
    );
    let confidence = number(&detail, &["Confidence"]);
    assert!(
        (PERCENT_MIN..=PERCENT_MAX).contains(&confidence),
        "case {case:?}: the searched face's confidence is a Percent, got {confidence}"
    );
    detail
}

/// The `(UserId, Similarity)` pairs of a served answer, in the order they were reported.
fn matched(matches: &[Value]) -> Vec<(String, f64)> {
    matches
        .iter()
        .map(|entry| {
            (
                entry
                    .get("User")
                    .and_then(|user| user.get("UserId"))
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                number(entry, &["Similarity"]),
            )
        })
        .collect()
}

/// The `(Reasons, BoundingBox)` of every face the search did not use, in the reported order.
fn unsearched(reply: &Reply) -> Vec<(Vec<String>, Value)> {
    reply
        .list("UnsearchedFaces")
        .iter()
        .map(|entry| {
            let reasons = entry
                .get("Reasons")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default()
                .iter()
                .map(|reason| reason.as_str().unwrap_or_default().to_string())
                .collect();
            let box_of = entry
                .get("FaceDetails")
                .and_then(|detail| detail.get("BoundingBox"))
                .cloned()
                .unwrap_or(Value::Null);
            (reasons, box_of)
        })
        .collect()
}

/// A refusal, by status, wire code and message.
fn assert_refused(case: &str, reply: &Reply, status: u16, code: &str, message: &str) {
    assert_eq!(
        reply.status, status,
        "case {case:?}: expected HTTP {status}, got {} with {:?}",
        reply.status, reply.body
    );
    assert_eq!(
        reply.error_code(),
        code,
        "case {case:?}: expected __type {code}, got {:?}",
        reply.body
    );
    assert_eq!(
        reply.string("Message").as_deref(),
        Some(message),
        "case {case:?}: expected Message {message:?}, got {:?}",
        reply.body
    );
}

/// The user enrolled from one picture is matched by a later picture of that same face,
/// whatever changed between the two that the resemblance measurement divides out — the scale
/// of the face in its frame, the exposure, the encoder — and the same pixels match it at the
/// top of the scale. The user is reported with the status it carries, and the face the search
/// used is reported as the detector measured it.
#[tokio::test]
async fn a_picture_of_an_enrolled_face_matches_the_user_holding_it() {
    let router = harness();
    create_collection(&router, "enrolled").await;
    let held = enrol(&router, "enrolled", "alex", &[upright().png()]).await;
    assert_eq!(held.len(), 1, "the user must hold the one drawn face");

    let cases: Vec<(&str, Vec<u8>, f64)> = vec![
        ("the same pixels again", upright().png(), SAME_PIXELS_CONFIDENCE),
        (
            // Twice the eye span in a frame twice as wide: the same face, twice as large in
            // the image.
            "the same face at twice the scale",
            Portrait::one_face()
                .with_size(640, 640)
                .with_faces(vec![DrawnFace::new(320.0, 300.0, 112.0)])
                .png(),
            DEFAULT_USER_MATCH_THRESHOLD,
        ),
        (
            "the same face at another exposure",
            Portrait::one_face().with_tones(95, 135, 55).png(),
            DEFAULT_USER_MATCH_THRESHOLD,
        ),
        (
            "the same face through a lossy encoder",
            encode(&upright().render(), image::ImageFormat::Jpeg),
            DEFAULT_USER_MATCH_THRESHOLD,
        ),
    ];
    for (case, submitted, floor) in cases {
        let reply = search(&router, "enrolled", &submitted, json!({})).await;
        let matches = served(case, &reply);
        searched_face(case, &reply);
        let found = matched(&matches);
        assert_eq!(
            found.len(),
            1,
            "case {case:?}: the collection holds one user, which this picture of its face \
             must match at the default threshold, got {found:?}"
        );
        assert_eq!(
            found[0].0, "alex",
            "case {case:?}: the match must name the enrolled user, got {found:?}"
        );
        assert!(
            found[0].1 >= floor,
            "case {case:?}: expected a confidence of at least {floor}, got {}",
            found[0].1
        );
    }
}

/// A user is identified by EVERY face associated with it, so it is matched as closely as its
/// nearest face: a user holding two faces is matched by a picture of either, at the
/// confidence of the nearer one.
#[tokio::test]
async fn a_user_is_matched_as_closely_as_the_nearest_face_it_holds() {
    let router = harness();
    create_collection(&router, "two-faced").await;
    let held = enrol(
        &router,
        "two-faced",
        "alex",
        &[upright().png(), other().png()],
    )
    .await;
    assert_eq!(held.len(), 2, "the user must hold both drawn faces");
    // A second user holding only the middle face, so the answers below are not about one
    // user alone.
    enrol(&router, "two-faced", "blake", &[tilted().png()]).await;

    for (case, submitted) in [
        ("a picture of the first face it holds", upright().png()),
        ("a picture of the second face it holds", other().png()),
    ] {
        let reply = search(
            &router,
            "two-faced",
            &submitted,
            json!({ "UserMatchThreshold": PERCENT_MIN }),
        )
        .await;
        let found = matched(&served(case, &reply));
        let alex = found
            .iter()
            .find(|(user_id, _)| user_id == "alex")
            .unwrap_or_else(|| panic!("case {case:?}: the user holding the face must be matched, got {found:?}"));
        assert!(
            alex.1 >= SAME_PIXELS_CONFIDENCE,
            "case {case:?}: the user holds the face in this very picture, so its confidence \
             must reach {SAME_PIXELS_CONFIDENCE}, got {}",
            alex.1
        );
        let blake = found
            .iter()
            .find(|(user_id, _)| user_id == "blake")
            .map(|(_, confidence)| *confidence);
        assert!(
            blake.is_some_and(|confidence| confidence < alex.1),
            "case {case:?}: the user holding another face must rank below, got {found:?}"
        );
    }
}

/// "The operation returns an array of UserIDs that match the face in the supplied image,
/// ordered by similarity score with the highest similarity first", and `MaxUsers` is the
/// "maximum number of UserIDs to return" — so the cap keeps the strongest of that order.
#[tokio::test]
async fn matched_users_are_ranked_and_capped() {
    let router = harness();
    create_collection(&router, "ranked").await;
    for (user_id, picture) in [
        ("alex", upright()),
        ("blake", tilted()),
        ("casey", other()),
    ] {
        enrol(&router, "ranked", user_id, &[picture.png()]).await;
    }

    let searched = upright().png();
    let case = "three enrolled users, searched by the first one's own picture";
    let found = matched(&served(
        case,
        &search(
            &router,
            "ranked",
            &searched,
            json!({ "UserMatchThreshold": PERCENT_MIN }),
        )
        .await,
    ));
    assert_eq!(
        found.len(),
        3,
        "case {case:?}: every user holding a face is measured against the searched face, \
         got {found:?}"
    );
    assert_eq!(
        found[0].0, "alex",
        "case {case:?}: the user holding the searched face's own picture ranks first, got \
         {found:?}"
    );
    for pair in found.windows(2) {
        assert!(
            pair[0].1 >= pair[1].1,
            "case {case:?}: the matches are ordered highest similarity first, got {found:?}"
        );
    }

    for wanted in 1..=found.len() {
        let case = format!("MaxUsers {wanted} of {} matches", found.len());
        let capped = matched(&served(
            &case,
            &search(
                &router,
                "ranked",
                &searched,
                json!({ "UserMatchThreshold": PERCENT_MIN, "MaxUsers": wanted }),
            )
            .await,
        ));
        assert_eq!(
            capped,
            found[..wanted].to_vec(),
            "case {case:?}: the cap keeps the first {wanted} of the ranked matches"
        );
    }

    // The top of the published range asks for more users than this collection has, so it
    // reports every match and no more.
    let case = format!("MaxUsers at the top of the range ({MAX_USERS_MAX})");
    assert_eq!(
        matched(&served(
            &case,
            &search(
                &router,
                "ranked",
                &searched,
                json!({ "UserMatchThreshold": PERCENT_MIN, "MaxUsers": MAX_USERS_MAX }),
            )
            .await,
        )),
        found,
        "case {case:?}: a cap above the number of matches keeps them all"
    );

    // One past each published bound: the modelled range refuses it before the handler runs.
    for (case, cap) in [
        ("below the bottom of the MaxUsers range", MAX_USERS_MIN - 1),
        ("above the top of the MaxUsers range", MAX_USERS_MAX + 1),
    ] {
        let reply = search(&router, "ranked", &searched, json!({ "MaxUsers": cap })).await;
        assert_eq!(
            reply.status, 400,
            "case {case:?}: a cap outside MaxUsers is refused, got {} with {:?}",
            reply.status, reply.body
        );
        assert_eq!(
            reply.error_code(),
            "ValidationException",
            "case {case:?}: the modelled range refuses it at the boundary, got {:?}",
            reply.body
        );
    }
}

/// `UserMatchThreshold` is the only thing that decides which measured users are reported: no
/// reported match is below it, raising it never adds one, and a threshold set between two
/// users' confidences reports the one and not the other.
#[tokio::test]
async fn the_user_match_threshold_decides_which_users_are_reported() {
    let router = harness();
    create_collection(&router, "thresholds").await;
    enrol(&router, "thresholds", "alex", &[upright().png()]).await;
    enrol(&router, "thresholds", "blake", &[other().png()]).await;
    let searched = upright().png();

    let thresholds: Vec<(String, Option<f64>)> = vec![
        ("the bottom of the range".to_string(), Some(PERCENT_MIN)),
        ("a quarter".to_string(), Some(25.0)),
        ("no threshold at all".to_string(), None),
        ("the top of the range".to_string(), Some(PERCENT_MAX)),
    ];
    let mut previous: Option<(String, usize)> = None;
    for (case, threshold) in thresholds {
        let mut extra = json!({});
        if let Some(threshold) = threshold {
            extra["UserMatchThreshold"] = json!(threshold);
        }
        let found = matched(&served(
            &case,
            &search(&router, "thresholds", &searched, extra).await,
        ));
        let floor = threshold.unwrap_or(DEFAULT_USER_MATCH_THRESHOLD);
        for (user_id, confidence) in &found {
            assert!(
                *confidence >= floor,
                "case {case:?}: {user_id} is reported below the threshold {floor}, at \
                 {confidence}"
            );
        }
        if let Some((lower_case, lower_count)) = previous.replace((case.clone(), found.len())) {
            assert!(
                found.len() <= lower_count,
                "case {case:?}: raising the threshold from {lower_case:?} cannot add a match, \
                 got {} after {lower_count}",
                found.len()
            );
        }
    }

    // A threshold strictly between the two users' confidences reports the stronger alone.
    let all = matched(&served(
        "every user, to read their confidences",
        &search(
            &router,
            "thresholds",
            &searched,
            json!({ "UserMatchThreshold": PERCENT_MIN }),
        )
        .await,
    ));
    assert_eq!(all.len(), 2, "both enrolled users must be measured, got {all:?}");
    let between = (all[0].1 + all[1].1) / 2.0;
    let case = format!("a threshold of {between} between the two confidences");
    let narrowed = matched(&served(
        &case,
        &search(
            &router,
            "thresholds",
            &searched,
            json!({ "UserMatchThreshold": between }),
        )
        .await,
    ));
    assert_eq!(
        narrowed,
        all[..1].to_vec(),
        "case {case:?}: only the user above the threshold is reported, got {narrowed:?}"
    );

    // One past each published bound: the modelled range refuses it before the handler runs.
    for (case, value) in [
        ("below the bottom of the Percent range", PERCENT_MIN - 0.5),
        ("above the top of the Percent range", PERCENT_MAX + 0.5),
    ] {
        let reply = search(
            &router,
            "thresholds",
            &searched,
            json!({ "UserMatchThreshold": value }),
        )
        .await;
        assert_eq!(
            reply.status, 400,
            "case {case:?}: a threshold outside Percent is refused, got {} with {:?}",
            reply.status, reply.body
        );
        assert_eq!(
            reply.error_code(),
            "ValidationException",
            "case {case:?}: the modelled range refuses it at the boundary, got {:?}",
            reply.body
        );
    }
}

/// The users measured are the ones of the collection the request named: a user that holds no
/// face is matched by nothing, since there is nothing to measure it by, and a user of another
/// collection is reported by neither search.
#[tokio::test]
async fn only_the_named_collections_users_holding_a_face_are_measured() {
    let router = harness();
    create_collection(&router, "left").await;
    create_collection(&router, "right").await;
    let searched = upright().png();
    enrol(&router, "left", "alex", std::slice::from_ref(&searched)).await;
    // The same picture enrolled in the other collection, under another id: a search of one
    // collection must report neither it nor the holder.
    enrol(&router, "right", "blake", std::slice::from_ref(&searched)).await;
    // A user of the searched collection that holds no face at all.
    create_user(&router, "left", "casey").await;

    for (case, collection_id, expected) in [
        ("the collection holding alex", "left", "alex"),
        ("the collection holding blake", "right", "blake"),
    ] {
        let found = matched(&served(
            case,
            &search(
                &router,
                collection_id,
                &searched,
                json!({ "UserMatchThreshold": PERCENT_MIN }),
            )
            .await,
        ));
        assert_eq!(
            found.iter().map(|(id, _)| id.clone()).collect::<Vec<_>>(),
            vec![expected.to_string()],
            "case {case:?}: only the users of {collection_id} that hold a face are reported, \
             got {found:?}"
        );
    }
}

/// "It first detects the largest face in the image": which face of a crowded image the search
/// used is reported as the box and confidence measured for it — the largest one `DetectFaces`
/// measures in that same image — and "information about faces detected in the supplied image,
/// but not used for the search, is returned in an array of UnsearchedFace objects".
#[tokio::test]
async fn the_largest_face_is_searched_and_the_others_are_reported_unsearched() {
    let router = harness();
    create_collection(&router, "crowded").await;
    enrol(&router, "crowded", "alex", &[upright().png()]).await;

    // One face drawn at nearly twice the eye span of the other, and the smaller one tilted,
    // so which of them the search used shows in the answer.
    let crowd = Portrait::one_face()
        .with_size(480, 320)
        .with_faces(vec![
            DrawnFace::new(150.0, 170.0, 72.0),
            DrawnFace::new(390.0, 120.0, 38.0).rolled(12.0),
        ])
        .png();
    let detected = call(
        &router,
        "DetectFaces",
        json!({ "Image": bytes_image(&crowd) }),
    )
    .await;
    let faces = detected.list("FaceDetails");
    assert_eq!(
        faces.len(),
        2,
        "the crowded portrait must carry both drawn faces, got {:?}",
        detected.body
    );
    // `DetectFaces` reports the faces largest box first, so the first of them is the one a
    // search of the same image must use.
    let boxes: Vec<Value> = faces
        .iter()
        .map(|face| face.get("BoundingBox").cloned().unwrap_or(Value::Null))
        .collect();

    let case = "a two-face image searched at the default quality bar";
    let reply = search(
        &router,
        "crowded",
        &crowd,
        json!({ "UserMatchThreshold": PERCENT_MIN }),
    )
    .await;
    served(case, &reply);
    let detail = searched_face(case, &reply);
    assert_eq!(
        detail.get("BoundingBox"),
        boxes.first(),
        "case {case:?}: the search uses the largest face of the image, got {detail:?}"
    );
    assert_eq!(
        unsearched(&reply),
        vec![(vec![FACE_NOT_LARGEST.to_string()], boxes[1].clone())],
        "case {case:?}: the face the search did not use is reported with the reason it was \
         not the largest"
    );
}

/// "Filtered faces aren't searched for in the collection": the bar removes a face before the
/// largest is picked, so a harder level never adds a match, and an image whose every face it
/// removes reports each of them as a face left out for the quality it was measured at, with
/// an empty match list and no searched face.
#[tokio::test]
async fn the_quality_filter_only_narrows_what_is_searched() {
    let router = harness();
    create_collection(&router, "quality").await;
    let sharp = upright();
    enrol(&router, "quality", "alex", &[sharp.png()]).await;
    // The same picture, softened more and more: blur takes the contrast out of the edges the
    // sharpness measurement reads without moving the face.
    let softened: Vec<(usize, Vec<u8>)> = [0, 2, 5]
        .into_iter()
        .map(|passes| {
            (
                passes,
                encode(&blurred(&sharp.render(), passes), image::ImageFormat::Png),
            )
        })
        .collect();
    for (passes, submitted) in &softened {
        let mut previous: Option<(String, usize)> = None;
        for level in QUALITY_LEVELS {
            let case = format!("{passes} blur passes at QualityFilter {level}");
            let reply = search(
                &router,
                "quality",
                submitted,
                json!({ "QualityFilter": level, "UserMatchThreshold": PERCENT_MIN }),
            )
            .await;
            let found = matched(&served(&case, &reply));
            let left_out = unsearched(&reply);
            if found.is_empty() {
                // The bar removed every face of the image: the answer carries no searched
                // face, and each face it removed is reported for the quality measured for it.
                assert!(
                    reply.field("SearchedFace").is_none(),
                    "case {case:?}: an image the bar left no face in reports no searched \
                     face, got {:?}",
                    reply.body
                );
                for (reasons, _) in &left_out {
                    assert_eq!(
                        reasons,
                        &vec![LOW_SHARPNESS.to_string()],
                        "case {case:?}: a face the bar removed is reported for its measured \
                         sharpness, got {left_out:?}"
                    );
                }
            } else {
                searched_face(&case, &reply);
            }
            if let Some((lower_case, lower_count)) = previous.replace((case.clone(), found.len()))
            {
                assert!(
                    found.len() <= lower_count,
                    "case {case:?}: raising the quality bar from {lower_case:?} cannot add a \
                     match, got {} after {lower_count}",
                    found.len()
                );
            }
        }
    }
}

/// "If no valid face is detected in the image, the response will contain an empty UserMatches
/// list and no SearchedFace object": such a search is SERVED, not refused, however the image
/// reaches the service.
#[tokio::test]
async fn an_image_carrying_no_face_is_served_with_no_match() {
    let router = harness_with_object_store();
    assert_eq!(
        s3(&router, &format!("/{BUCKET}"), &[]).await,
        200,
        "the image bucket must be created first"
    );
    create_collection(&router, "faceless").await;
    enrol(&router, "faceless", "alex", &[upright().png()]).await;

    let faceless = Portrait::blank(240, 240, 200).png();
    assert_eq!(
        s3(&router, &format!("/{BUCKET}/faceless.png"), &faceless).await,
        200,
        "the faceless object must be stored"
    );
    for (case, image) in [
        ("the image inline", bytes_image(&faceless)),
        (
            "the image from S3",
            json!({ "S3Object": { "Bucket": BUCKET, "Name": "faceless.png" } }),
        ),
    ] {
        let reply = search_image(
            &router,
            "faceless",
            image,
            json!({ "UserMatchThreshold": PERCENT_MIN }),
        )
        .await;
        let found = matched(&served(case, &reply));
        assert!(
            found.is_empty(),
            "case {case:?}: an image with no face matches no user, got {found:?}"
        );
        assert!(
            reply.field("SearchedFace").is_none(),
            "case {case:?}: no searched face is reported, got {:?}",
            reply.body
        );
        assert!(
            unsearched(&reply).is_empty(),
            "case {case:?}: the detector found no face, so none is reported as unsearched, \
             got {:?}",
            reply.body
        );
    }
}

/// An image read out of the emulator's own Amazon S3 is searched exactly like the same bytes
/// submitted inline, and an image the service cannot read is refused for the reason it could
/// not be read.
#[tokio::test]
async fn an_s3_sourced_image_searches_like_inline_bytes() {
    let router = harness_with_object_store();
    assert_eq!(
        s3(&router, &format!("/{BUCKET}"), &[]).await,
        200,
        "the image bucket must be created first"
    );
    create_collection(&router, "s3-sourced").await;
    let searched = upright().png();
    enrol(&router, "s3-sourced", "alex", std::slice::from_ref(&searched)).await;
    assert_eq!(
        s3(&router, &format!("/{BUCKET}/searched.png"), &searched).await,
        200,
        "the searched object must be stored"
    );

    let inline = matched(&served(
        "the image inline",
        &search(&router, "s3-sourced", &searched, json!({})).await,
    ));
    let staged = matched(&served(
        "the image from S3",
        &search_image(
            &router,
            "s3-sourced",
            json!({ "S3Object": { "Bucket": BUCKET, "Name": "searched.png" } }),
            json!({}),
        )
        .await,
    ));
    assert_eq!(
        inline, staged,
        "one image answers the same whichever way it was submitted, got {inline:?} and \
         {staged:?}"
    );

    for (case, image, code, message) in [
        (
            "an object no bucket of the store holds",
            json!({ "S3Object": { "Bucket": "absent-bucket", "Name": "absent.png" } }),
            "InvalidS3ObjectException",
            "Unable to get object metadata from S3. Check object key, region and/or access \
             permissions.",
        ),
        (
            "bytes that are not an image at all",
            bytes_image(b"not an image"),
            "InvalidImageFormatException",
            "Request has invalid image format",
        ),
        (
            "an image naming neither bytes nor an object",
            json!({}),
            "InvalidParameterException",
            "Requested image should either contain bytes or s3 object.",
        ),
    ] {
        assert_refused(
            case,
            &search_image(&router, "s3-sourced", image, json!({})).await,
            400,
            code,
            message,
        );
    }
}

/// A collection the store does not hold is refused as not found, and a served search writes
/// nothing: the users and the faces of the collection read the same after it as before, and
/// the same search answers the same way again.
#[tokio::test]
async fn an_unknown_collection_is_refused_and_a_search_stores_nothing() {
    let router = harness();
    create_collection(&router, "reads-only").await;
    let searched = upright().png();
    let held = enrol(
        &router,
        "reads-only",
        "alex",
        std::slice::from_ref(&searched),
    )
    .await;

    for case in ["never-created", "reads-only-x", "reads.only"] {
        assert_refused(
            case,
            &search(&router, case, &searched, json!({})).await,
            400,
            "ResourceNotFoundException",
            &collection_does_not_exist(case),
        );
    }

    let users_before = call(
        &router,
        "ListUsers",
        json!({ "CollectionId": "reads-only" }),
    )
    .await
    .list("Users");
    let faces_before = call(
        &router,
        "ListFaces",
        json!({ "CollectionId": "reads-only" }),
    )
    .await
    .list("Faces");

    let first = matched(&served(
        "the first search",
        &search(&router, "reads-only", &searched, json!({})).await,
    ));
    let second = matched(&served(
        "the same search again",
        &search(&router, "reads-only", &searched, json!({})).await,
    ));
    assert_eq!(
        first, second,
        "a search changes nothing, so the same search answers the same way, got {first:?} \
         then {second:?}"
    );

    let faces_after = call(
        &router,
        "ListFaces",
        json!({ "CollectionId": "reads-only" }),
    )
    .await
    .list("Faces");
    assert_eq!(
        faces_after, faces_before,
        "the faces of the collection are untouched by a search"
    );
    let users_after = call(
        &router,
        "ListUsers",
        json!({ "CollectionId": "reads-only" }),
    )
    .await
    .list("Users");
    assert_eq!(
        users_after.len(),
        users_before.len(),
        "a search neither adds nor removes a user, got {users_after:?} after {users_before:?}"
    );
    for user in &users_after {
        let status = user
            .get("UserStatus")
            .and_then(Value::as_str)
            .unwrap_or_default();
        assert!(
            [USER_STATUS_UPDATING, USER_STATUS_ACTIVE].contains(&status),
            "a search moves no user's status; a user holding a face is UPDATING or ACTIVE, \
             got {status:?}"
        );
    }
    assert_eq!(
        held.len(),
        faces_after.len(),
        "every face enrolled is still in the collection, got {faces_after:?}"
    );
}
