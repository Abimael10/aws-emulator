// SPDX-License-Identifier: CC0-1.0

//! End-to-end coverage of the USERS of a Rekognition collection over the wire
//! (`awsJson1_1`, `POST /` with `X-Amz-Target: RekognitionService.<Operation>`),
//! dispatched through the same router the production emulator mounts.
//!
//! Every assertion goes over HTTP and reads only the status, the wire error code and the
//! JSON body: nothing reaches into the service's state struct, so a response that is not
//! computed from persisted state cannot pass.
//!
//! Each test states a RULE and asserts it over a TABLE of inputs:
//!
//! * `CreateUser` records a user under the `UserId` the caller supplied in the
//!   collection it named, and that record is what every reader answers from: `ListUsers`
//!   reports the user with the `UserStatus` of a user nothing has been associated with
//!   yet, and `DescribeCollection` counts it. The rows span the published `UserId`
//!   shape — every character class its pattern allows, and both ends of its length
//!   range — and two collections, so a user belongs to the collection it was created in;
//! * a create is idempotent on its `ClientRequestToken`: the same token with the same
//!   request completes the create ONCE, the same token with a different request is
//!   refused as a parameter mismatch, and a `UserId` the collection already holds is a
//!   conflict however it is reached;
//! * a user is created only in a collection that exists;
//! * a `ListUsers` page is cut out of the collection's own order, `MaxResults` caps it,
//!   a `NextToken` resumes exactly where the page ended, and a token this list did not
//!   mint names no position;
//! * the store is bounded: past a collection's user capacity a create is refused with
//!   the modelled quota error and stores nothing;
//! * `AssociateFaces` decides each face the request names on its own — associating the
//!   ones it can, ignoring the ones the user already holds and reporting every other one
//!   as an unsuccessful association with the reason the model names for it — and what it
//!   associated is what `ListFaces`, `ListUsers` and `DeleteFaces` then answer from;
//! * a candidate joins a user exactly when the confidence MEASURED between it and the
//!   faces that user already holds reaches `UserMatchThreshold`, which does not apply to
//!   a user that holds no face yet;
//! * an association completes once per `ClientRequestToken`, and the same token is
//!   answered with the same response;
//! * a user is `UPDATING` until the clock owes the completion of the update its faces are
//!   under, and faces are associated only with a user and a collection that exist;
//! * a user holds at most the published number of faces: part way through a request the
//!   faces beyond that are unsuccessful, and a call to a user that is already full is
//!   refused with the modelled quota error.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};
use service_com_amazonaws_rekognition_rekognitionservice::state;

// The wire harness is the collection suite's, so a request is built one way whichever
// operation is being exercised, and the drawn portraits come from the detection suite, so
// a picture is drawn one way wherever a face has to be indexed first.
use crate::face_collections::{bytes_image, call, harness, one_face_png, Reply};
use crate::face_detection::{DrawnFace, Portrait};

/// `MaxResults` is `MaxUserResults`, `range 1..500` on `ListUsers`
/// (`smithy_query(query="operation",operation="ListUsers")`).
const MAX_USER_RESULTS_MIN: i64 = 1;
const MAX_USER_RESULTS_MAX: i64 = 500;

/// `UserMatchThreshold` is `Percent`, `range 0..100`, and `FaceIds` is `UserFaceIdList`,
/// `len 1..100` (`smithy_query(query="operation",operation="AssociateFaces")`).
const PERCENT_MIN: f64 = 0.0;
const PERCENT_MAX: f64 = 100.0;
const FACE_IDS_LEN_MIN: usize = 1;
const FACE_IDS_LEN_MAX: usize = 100;

/// The threshold `AssociateFaces` applies when the request names none: "the value ranges
/// from 0-100 and default value is 75"
/// (`smithy_query(query="doc",shape="AssociateFaces")`).
const DEFAULT_USER_MATCH_THRESHOLD: f64 = 75.0;

/// The `UnsuccessfulFaceAssociationReason` values
/// (`smithy_query(query="shape",shape="UnsuccessfulFaceAssociationReason")`).
const FACE_NOT_FOUND: &str = "FACE_NOT_FOUND";
const ASSOCIATED_TO_A_DIFFERENT_USER: &str = "ASSOCIATED_TO_A_DIFFERENT_USER";
const LOW_MATCH_CONFIDENCE: &str = "LOW_MATCH_CONFIDENCE";

/// A `FaceId` the service never minted, in the published form of one
/// (`pattern ^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$`,
/// `smithy_query(query="shape",shape="FaceId")`), so it is the handler and not the
/// boundary validator that answers for it.
const UNMINTED_FACE_ID: &str = "ffffffff-ffff-ffff-ffff-ffffffffffff";

/// The `UserId` length range, `len 1..128`
/// (`smithy_query(query="shape",shape="UserId")`).
const USER_ID_LEN_MIN: usize = 1;
const USER_ID_LEN_MAX: usize = 128;

/// The `UserStatus` a user carries before any face is associated with it. `UserStatus`
/// is ACTIVE | UPDATING | CREATING | CREATED
/// (`smithy_query(query="shape",shape="UserStatus")`), and a user a create has just
/// recorded is created.
const CREATED: &str = "CREATED";

/// The other two `UserStatus` values a user of this store carries: `UPDATING` while an
/// association of its faces is still being applied, and `ACTIVE` once "all associations or
/// disassociations of FaceID(s) for a UserID are complete"
/// (`smithy_query(query="doc",shape="AssociateFaces")`).
const UPDATING: &str = "UPDATING";
const ACTIVE: &str = "ACTIVE";

async fn create_collection(
    router: &smithy_server_runtime::MultiServiceRouter,
    collection_id: &str,
) {
    let reply = call(
        router,
        "CreateCollection",
        json!({ "CollectionId": collection_id }),
    )
    .await;
    assert_eq!(
        reply.status, 200,
        "case {collection_id}: the collection must be created, got {:?}",
        reply.body
    );
}

/// `CreateUser` in `collection_id` for `user_id`, with `token` as its
/// `ClientRequestToken` when one is given.
async fn create_user(
    router: &smithy_server_runtime::MultiServiceRouter,
    collection_id: &str,
    user_id: &str,
    token: Option<&str>,
) -> Reply {
    let mut body = json!({ "CollectionId": collection_id, "UserId": user_id });
    if let Some(token) = token {
        body["ClientRequestToken"] = json!(token);
    }
    call(router, "CreateUser", body).await
}

/// Every `(UserId, UserStatus)` `ListUsers` reports for `collection_id`, walking the
/// pages to the end.
async fn list_users(
    router: &smithy_server_runtime::MultiServiceRouter,
    collection_id: &str,
) -> Vec<(String, String)> {
    let mut users = Vec::new();
    let mut token: Option<String> = None;
    let mut pages = 0;
    loop {
        let mut body = json!({ "CollectionId": collection_id });
        if let Some(cursor) = &token {
            body["NextToken"] = json!(cursor);
        }
        let reply = call(router, "ListUsers", body).await;
        assert_eq!(
            reply.status, 200,
            "case {collection_id}: listing the users must succeed, got {:?}",
            reply.body
        );
        users.extend(reply.list("Users").iter().map(user_entry));
        token = reply.string("NextToken");
        if token.is_none() {
            return users;
        }
        // A page carries at least one user while users remain, so the walk cannot need
        // more pages than the collection may hold: a handler that minted a cursor
        // forever fails here instead of hanging the run.
        pages += 1;
        assert!(
            pages <= state::MAX_USERS_PER_COLLECTION,
            "case {collection_id}: paging must terminate, walked {} user(s) over {pages} pages",
            users.len()
        );
    }
}

/// One reported `User`, as the pair a caller branches on.
fn user_entry(user: &Value) -> (String, String) {
    (
        user.get("UserId")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        user.get("UserStatus")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
    )
}

/// The `UserCount` `DescribeCollection` reports for `collection_id`.
async fn user_count(
    router: &smithy_server_runtime::MultiServiceRouter,
    collection_id: &str,
) -> Option<i64> {
    call(
        router,
        "DescribeCollection",
        json!({ "CollectionId": collection_id }),
    )
    .await
    .field("UserCount")
    .and_then(Value::as_i64)
}

/// `count` well-formed `FaceId`s, for a request whose LIST LENGTH is what is under test
/// rather than the faces it names.
fn face_id_list(count: usize) -> Vec<String> {
    (0..count)
        .map(|index| format!("{index:08x}-0000-0000-0000-000000000000"))
        .collect()
}

/// A picture of ANOTHER face than the one [`one_face_png`] draws: a different eye span, a
/// tilt and different tones, so what the detector measures off it correlates poorly with
/// the faces indexed from that portrait. The fixtures are the detection suite's, as
/// everywhere else in this crate's tests.
fn other_face_png() -> Vec<u8> {
    Portrait::one_face()
        .with_faces(vec![DrawnFace::new(160.0, 150.0, 34.0).rolled(16.0)])
        .with_tones(60, 150, 20)
        .png()
}

/// Index `image` into `collection_id` and answer the `FaceId` minted for the one face the
/// portrait carries.
async fn index_face(
    router: &smithy_server_runtime::MultiServiceRouter,
    collection_id: &str,
    image: &[u8],
) -> String {
    let reply = call(
        router,
        "IndexFaces",
        json!({ "CollectionId": collection_id, "Image": bytes_image(image) }),
    )
    .await;
    let records = reply.list("FaceRecords");
    assert_eq!(
        records.len(),
        1,
        "case {collection_id}: the portrait carries one face, so one face must be indexed, \
         got {:?}",
        reply.body
    );
    records[0]
        .get("Face")
        .and_then(|face| face.get("FaceId"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// `AssociateFaces` in `collection_id` for `user_id` over `face_ids`, with the threshold
/// and the `ClientRequestToken` when one is given.
async fn associate_faces(
    router: &smithy_server_runtime::MultiServiceRouter,
    collection_id: &str,
    user_id: &str,
    face_ids: &[String],
    threshold: Option<f64>,
    token: Option<&str>,
) -> Reply {
    let mut body = json!({
        "CollectionId": collection_id,
        "UserId": user_id,
        "FaceIds": face_ids,
    });
    if let Some(threshold) = threshold {
        body["UserMatchThreshold"] = json!(threshold);
    }
    if let Some(token) = token {
        body["ClientRequestToken"] = json!(token);
    }
    call(router, "AssociateFaces", body).await
}

/// The `FaceId` of every face an answer reports as associated, in the order it reports
/// them.
fn associated(reply: &Reply) -> Vec<String> {
    reply
        .list("AssociatedFaces")
        .iter()
        .map(|face| {
            face.get("FaceId")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string()
        })
        .collect()
}

/// Every reported `UnsuccessfulFaceAssociation`, as the triple a caller branches on: the
/// face, the user that holds it and the reasons. The measured confidence is read on its
/// own through [`confidence_of`], since it is a measurement rather than a value to
/// compare for equality.
fn unsuccessful(reply: &Reply) -> Vec<(String, Option<String>, Vec<String>)> {
    reply
        .list("UnsuccessfulFaceAssociations")
        .iter()
        .map(|entry| {
            (
                entry
                    .get("FaceId")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                entry
                    .get("UserId")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                entry
                    .get("Reasons")
                    .and_then(Value::as_array)
                    .map(|reasons| {
                        reasons
                            .iter()
                            .map(|reason| {
                                reason.as_str().unwrap_or_default().to_string()
                            })
                            .collect()
                    })
                    .unwrap_or_default(),
            )
        })
        .collect()
}

/// The `Confidence` an answer reports for the unsuccessful association of `face_id`.
fn confidence_of(reply: &Reply, face_id: &str) -> Option<f64> {
    reply
        .list("UnsuccessfulFaceAssociations")
        .iter()
        .find(|entry| entry.get("FaceId").and_then(Value::as_str) == Some(face_id))
        .and_then(|entry| entry.get("Confidence"))
        .and_then(Value::as_f64)
}

/// Every `(FaceId, UserId)` `ListFaces` reports for `collection_id`, with the filter the
/// caller asked for.
async fn list_faces(
    router: &smithy_server_runtime::MultiServiceRouter,
    collection_id: &str,
    user_id: Option<&str>,
) -> Vec<(String, Option<String>)> {
    let mut body = json!({ "CollectionId": collection_id });
    if let Some(user_id) = user_id {
        body["UserId"] = json!(user_id);
    }
    let reply = call(router, "ListFaces", body).await;
    assert_eq!(
        reply.status, 200,
        "case {collection_id}/{user_id:?}: listing the faces must succeed, got {:?}",
        reply.body
    );
    reply
        .list("Faces")
        .iter()
        .map(|face| {
            (
                face.get("FaceId")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                face.get("UserId")
                    .and_then(Value::as_str)
                    .map(str::to_string),
            )
        })
        .collect()
}

/// A user of `collection_id` holding exactly one face indexed from `enrolled`, plus the
/// `FaceId` of a candidate face indexed from `candidate` that no user holds yet: the pair
/// every threshold case starts from, so no case is decided by what another one associated.
async fn user_holding_one_face(
    router: &smithy_server_runtime::MultiServiceRouter,
    collection_id: &str,
    user_id: &str,
    enrolled: &[u8],
    candidate: &[u8],
) -> String {
    create_user(router, collection_id, user_id, None).await;
    let first = index_face(router, collection_id, enrolled).await;
    let candidate_id = index_face(router, collection_id, candidate).await;
    let reply = associate_faces(
        router,
        collection_id,
        user_id,
        std::slice::from_ref(&first),
        None,
        None,
    )
    .await;
    assert_eq!(
        (reply.status, associated(&reply)),
        (200, vec![first]),
        "case {user_id:?}: the user's first face must be associated, got {:?}",
        reply.body
    );
    candidate_id
}

#[tokio::test]
async fn a_created_user_is_the_user_every_reader_reports() {
    let router = harness();
    create_collection(&router, "people").await;
    create_collection(&router, "others").await;

    // Rows span the published `UserId` shape rather than one example: every character
    // class `^[a-zA-Z0-9_.\-:]+$` allows, and both ends of `len 1..128`.
    let cases: Vec<String> = vec![
        "alice".to_string(),
        "Bob".to_string(),
        "user_42".to_string(),
        "first.last".to_string(),
        "team-east".to_string(),
        "ns:member".to_string(),
        "A".repeat(USER_ID_LEN_MIN),
        "u".repeat(USER_ID_LEN_MAX),
    ];

    for user_id in &cases {
        let created = create_user(&router, "people", user_id, None).await;
        assert_eq!(
            created.status, 200,
            "case {user_id:?}: creating the user must succeed, got {:?}",
            created.body
        );
        // `CreateUserResponse` models no members, so the answer is the bare success:
        // what the create recorded is read back below, never echoed here.
        assert!(
            created.field("UserId").is_none(),
            "case {user_id:?}: the create answers with no members, got {:?}",
            created.body
        );
    }

    // Every created user, and only those, is reported — each with the status of a user
    // no face has been associated with yet.
    let mut expected: Vec<(String, String)> = cases
        .iter()
        .map(|user_id| (user_id.clone(), CREATED.to_string()))
        .collect();
    expected.sort();
    let reported = list_users(&router, "people").await;
    let mut sorted = reported.clone();
    sorted.sort();
    assert_eq!(
        sorted, expected,
        "every created user must be reported with its status, got {reported:?}"
    );
    assert_eq!(
        user_count(&router, "people").await,
        Some(cases.len() as i64),
        "case people: the described user count must be the number of users created"
    );

    // A user belongs to the collection it was created in: the other collection reports
    // none of them, and the same id created there is a second, independent user.
    assert_eq!(
        list_users(&router, "others").await,
        Vec::new(),
        "case others: a collection nothing was created in holds no user"
    );
    assert_eq!(
        user_count(&router, "others").await,
        Some(0),
        "case others: a collection nothing was created in counts no user"
    );
    assert_eq!(
        create_user(&router, "others", "alice", None).await.status,
        200,
        "case others/alice: the same id in another collection is another user"
    );
    assert_eq!(
        list_users(&router, "others").await,
        vec![("alice".to_string(), CREATED.to_string())],
        "case others: the other collection reports only its own user"
    );
}

/// One row of the idempotency table: the request a LATER call makes, and the answer the
/// rule requires for it.
struct TokenCase {
    case: &'static str,
    collection_id: &'static str,
    user_id: &'static str,
    token: Option<&'static str>,
    status: u16,
    error_code: &'static str,
    message: &'static str,
}

#[tokio::test]
async fn a_create_completes_once_per_client_request_token() {
    let router = harness();
    create_collection(&router, "people").await;
    create_collection(&router, "others").await;

    // The token that commits the first request. Every row below is about how a LATER
    // request relates to it.
    let committed = "token-one";
    assert_eq!(
        create_user(&router, "people", "alice", Some(committed))
            .await
            .status,
        200,
        "the first create must succeed"
    );

    // Rows: the same token with the same request (a retry, which completes the create
    // only once); the same token with a different user, and with a different
    // collection (a parameter mismatch); a different token, and no token at all, for a
    // user the collection already holds (a conflict). Each refusal names the class and
    // the text the service words that class with.
    let cases = vec![
        TokenCase {
            case: "same token, same request",
            collection_id: "people",
            user_id: "alice",
            token: Some(committed),
            status: 200,
            error_code: "",
            message: "",
        },
        TokenCase {
            case: "same token, other user",
            collection_id: "people",
            user_id: "carol",
            token: Some(committed),
            status: 400,
            error_code: "IdempotentParameterMismatchException",
            // Spelled out, not read from the implementation's own constant: the text a
            // recorded AWS refusal of a reused token carries.
            message: "Service Unavailable",
        },
        TokenCase {
            case: "same token, other collection",
            collection_id: "others",
            user_id: "alice",
            token: Some(committed),
            status: 400,
            error_code: "IdempotentParameterMismatchException",
            message: "Service Unavailable",
        },
        TokenCase {
            case: "other token, existing user",
            collection_id: "people",
            user_id: "alice",
            token: Some("token-two"),
            status: 400,
            error_code: "ConflictException",
            message: "The user id: alice already exists",
        },
        TokenCase {
            case: "no token, existing user",
            collection_id: "people",
            user_id: "alice",
            token: None,
            status: 400,
            error_code: "ConflictException",
            message: "The user id: alice already exists",
        },
    ];

    for TokenCase {
        case,
        collection_id,
        user_id,
        token,
        status,
        error_code,
        message,
    } in cases
    {
        let reply = create_user(&router, collection_id, user_id, token).await;
        assert_eq!(
            (reply.status, reply.error_code().as_str()),
            (status, error_code),
            "case {case:?}: expected {status} {error_code:?}, got {:?}",
            reply.body
        );
        assert_eq!(
            reply.string("Message").unwrap_or_default(),
            message,
            "case {case:?}: expected the message {message:?}, got {:?}",
            reply.body
        );
        // Whatever the answer, the store still holds exactly the one user the one
        // completed create recorded: a retry creates no second user, and a refusal
        // creates none at all.
        assert_eq!(
            list_users(&router, "people").await,
            vec![("alice".to_string(), CREATED.to_string())],
            "case {case:?}: the completed create must have recorded exactly one user"
        );
        assert_eq!(
            user_count(&router, "people").await,
            Some(1),
            "case {case:?}: the described user count must still be one"
        );
    }

    // A refused request commits nothing, so the token it was refused under is still
    // free: used for a request that CAN be carried out, it carries it out.
    assert_eq!(
        create_user(&router, "people", "carol", Some("token-two"))
            .await
            .status,
        200,
        "case token-two/carol: a token a refusal never committed stays usable"
    );
    assert_eq!(
        list_users(&router, "people").await,
        vec![
            ("alice".to_string(), CREATED.to_string()),
            ("carol".to_string(), CREATED.to_string()),
        ],
        "both completed creates must be reported"
    );
}

#[tokio::test]
async fn a_user_is_created_only_in_a_collection_that_exists() {
    let router = harness();
    create_collection(&router, "people").await;

    // Rows: a collection id nothing created, and one created and then deleted. Both
    // name no collection, so neither can hold a user.
    call(
        &router,
        "CreateCollection",
        json!({ "CollectionId": "gone" }),
    )
    .await;
    call(
        &router,
        "DeleteCollection",
        json!({ "CollectionId": "gone" }),
    )
    .await;

    for collection_id in ["missing", "gone"] {
        let reply = create_user(&router, collection_id, "alice", None).await;
        assert_eq!(
            (reply.status, reply.error_code().as_str()),
            (400, "ResourceNotFoundException"),
            "case {collection_id}: a user cannot be created in a collection that does not \
             exist, got {:?}",
            reply.body
        );
        assert_eq!(
            reply.string("Message").as_deref(),
            Some(format!("The collection id: {collection_id} does not exist").as_str()),
            "case {collection_id}: the refusal names the collection the request asked for, \
             got {:?}",
            reply.body
        );
        let listed = call(
            &router,
            "ListUsers",
            json!({ "CollectionId": collection_id }),
        )
        .await;
        assert_eq!(
            (listed.status, listed.error_code().as_str()),
            (400, "ResourceNotFoundException"),
            "case {collection_id}: listing a collection that does not exist is refused the \
             same way, got {:?}",
            listed.body
        );
    }

    // The live collection is untouched by either refusal.
    assert_eq!(
        list_users(&router, "people").await,
        Vec::new(),
        "a refused create must store nothing"
    );
}

#[tokio::test]
async fn the_users_page_is_cut_out_of_the_collections_own_order() {
    let router = harness();
    create_collection(&router, "people").await;

    let created = [
        "alice", "bob", "carol", "dave", "erin", "frank", "grace",
    ];
    for user_id in created {
        create_user(&router, "people", user_id, None).await;
    }
    let order: Vec<String> = list_users(&router, "people")
        .await
        .into_iter()
        .map(|(user_id, _)| user_id)
        .collect();
    assert_eq!(
        order.len(),
        created.len(),
        "every created user must be reported once, got {order:?}"
    );

    // Rows differ along the page size: one user per page, through more than the
    // collection holds, plus both published bounds of `MaxUserResults` and "no
    // MaxResults at all".
    for max_results in [
        Some(MAX_USER_RESULTS_MIN),
        Some(2),
        Some(3),
        Some(7),
        Some(11),
        Some(MAX_USER_RESULTS_MAX),
        None,
    ] {
        let mut walked: Vec<String> = Vec::new();
        let mut token: Option<String> = None;
        let mut pages = 0;
        loop {
            let mut body = json!({ "CollectionId": "people" });
            if let Some(max) = max_results {
                body["MaxResults"] = json!(max);
            }
            if let Some(cursor) = &token {
                body["NextToken"] = json!(cursor);
            }
            let reply = call(&router, "ListUsers", body).await;
            assert_eq!(
                reply.status, 200,
                "case MaxResults={max_results:?}: page {pages} must succeed, got {:?}",
                reply.body
            );
            let page: Vec<String> = reply
                .list("Users")
                .iter()
                .map(|user| user_entry(user).0)
                .collect();
            if let Some(max) = max_results {
                assert!(
                    page.len() as i64 <= max,
                    "case MaxResults={max_results:?}: a page must not exceed the cap, got {} \
                     users",
                    page.len()
                );
            }
            walked.extend(page);
            pages += 1;
            token = reply.string("NextToken");
            assert_eq!(
                token.is_some(),
                walked.len() < order.len(),
                "case MaxResults={max_results:?}: a NextToken must come back exactly while \
                 users remain, walked {} of {}",
                walked.len(),
                order.len()
            );
            if token.is_none() {
                break;
            }
            assert!(
                pages <= order.len() + 1,
                "case MaxResults={max_results:?}: paging must terminate, walked {walked:?}"
            );
        }
        assert_eq!(
            walked, order,
            "case MaxResults={max_results:?}: the pages must walk the collection's order \
             exactly once"
        );
    }

    // A page resumes where the previous one ended whatever happens in between: a user
    // created after the cursor was minted is reported by the resumed page when it falls
    // after the position the cursor names.
    let first = call(
        &router,
        "ListUsers",
        json!({ "CollectionId": "people", "MaxResults": 3 }),
    )
    .await;
    let cursor = first
        .string("NextToken")
        .expect("a further page exists after the first three users");
    create_user(&router, "people", "zoe", None).await;
    let resumed = call(
        &router,
        "ListUsers",
        json!({ "CollectionId": "people", "NextToken": cursor }),
    )
    .await;
    let after: Vec<String> = resumed
        .list("Users")
        .iter()
        .map(|user| user_entry(user).0)
        .collect();
    assert_eq!(
        after,
        order[3..]
            .iter()
            .cloned()
            .chain(["zoe".to_string()])
            .collect::<Vec<String>>(),
        "a cursor resumes at the position it named, got {:?}",
        resumed.body
    );

    // Rows: a token that is not hex at all, hex that decodes to nothing this service
    // minted, and another list's cursor — none names a position in this list.
    let faces_cursor = state::encode_sequence_cursor(0);
    for token in ["not-a-token", "00", faces_cursor.as_str()] {
        let reply = call(
            &router,
            "ListUsers",
            json!({ "CollectionId": "people", "NextToken": token }),
        )
        .await;
        assert_eq!(
            (reply.status, reply.error_code().as_str()),
            (400, "InvalidPaginationTokenException"),
            "case {token:?}: a token this list did not mint must be refused, got {:?}",
            reply.body
        );
    }

    // One past each published bound of `MaxUserResults` is outside the model's range,
    // so it is refused rather than served.
    for max_results in [MAX_USER_RESULTS_MIN - 1, MAX_USER_RESULTS_MAX + 1] {
        let reply = call(
            &router,
            "ListUsers",
            json!({ "CollectionId": "people", "MaxResults": max_results }),
        )
        .await;
        assert_eq!(
            reply.status, 400,
            "case MaxResults={max_results}: a page size outside the modelled range must be \
             refused, got {:?}",
            reply.body
        );
    }
}

#[tokio::test]
async fn the_store_refuses_to_grow_past_its_user_capacity() {
    let router = harness();
    create_collection(&router, "full").await;

    for index in 0..state::MAX_USERS_PER_COLLECTION {
        let user_id = format!("user-{index:05}");
        assert_eq!(
            create_user(&router, "full", &user_id, None).await.status,
            200,
            "case {user_id}: creating up to the capacity must succeed"
        );
    }
    let over = create_user(&router, "full", "one-too-many", None).await;
    assert_eq!(
        (over.status, over.error_code().as_str()),
        (400, "ServiceQuotaExceededException"),
        "case users={}: creating past the capacity must be refused, got {:?}",
        state::MAX_USERS_PER_COLLECTION,
        over.body
    );
    assert_eq!(
        user_count(&router, "full").await,
        Some(state::MAX_USERS_PER_COLLECTION as i64),
        "a refused create must store nothing"
    );
}

#[tokio::test]
async fn an_association_is_decided_face_by_face_and_read_back_from_the_store() {
    let router = harness();
    create_collection(&router, "people").await;
    create_collection(&router, "others").await;
    create_user(&router, "people", "alice", None).await;
    create_user(&router, "people", "bob", None).await;

    // Rows: a face no user holds, a face THIS user already holds, a face another user of
    // the collection holds, a face that was indexed and then removed, a well-formed id the
    // service never minted, and a face that belongs to a different collection. One
    // portrait draws them all and the threshold stays at the bottom of its published
    // range, so every row is decided by which face it names and none by a measurement.
    let portrait = one_face_png(11);
    let free = index_face(&router, "people", &portrait).await;
    let held = index_face(&router, "people", &portrait).await;
    let taken = index_face(&router, "people", &portrait).await;
    let removed = index_face(&router, "people", &portrait).await;
    let elsewhere = index_face(&router, "others", &portrait).await;

    for (user_id, face_id) in [("bob", &taken), ("alice", &held)] {
        let reply = associate_faces(
            &router,
            "people",
            user_id,
            std::slice::from_ref(face_id),
            Some(PERCENT_MIN),
            None,
        )
        .await;
        assert_eq!(
            (reply.status, associated(&reply)),
            (200, vec![face_id.clone()]),
            "case {user_id}: the face must be associated, got {:?}",
            reply.body
        );
    }
    let deleted = call(
        &router,
        "DeleteFaces",
        json!({ "CollectionId": "people", "FaceIds": [removed.clone()] }),
    )
    .await;
    assert_eq!(
        deleted.list("DeletedFaces").len(),
        1,
        "the face no user holds must be removed, got {:?}",
        deleted.body
    );

    let requested = vec![
        free.clone(),
        held.clone(),
        taken.clone(),
        removed.clone(),
        UNMINTED_FACE_ID.to_string(),
        elsewhere.clone(),
    ];
    let reply = associate_faces(
        &router,
        "people",
        "alice",
        &requested,
        Some(PERCENT_MIN),
        None,
    )
    .await;
    assert_eq!(
        reply.status, 200,
        "a request naming faces of every kind is answered rather than refused, got {:?}",
        reply.body
    );
    assert_eq!(
        associated(&reply),
        vec![free.clone()],
        "case associated: only the face no user held joins the user, got {:?}",
        reply.body
    );
    assert_eq!(
        unsuccessful(&reply),
        vec![
            (
                taken.clone(),
                Some("bob".to_string()),
                vec![ASSOCIATED_TO_A_DIFFERENT_USER.to_string()],
            ),
            (removed.clone(), None, vec![FACE_NOT_FOUND.to_string()]),
            (
                UNMINTED_FACE_ID.to_string(),
                None,
                vec![FACE_NOT_FOUND.to_string()],
            ),
            (elsewhere.clone(), None, vec![FACE_NOT_FOUND.to_string()]),
        ],
        "case unsuccessful: every face that could not be associated is reported once, with \
         the reason the model names for it, got {:?}",
        reply.body
    );
    assert_eq!(
        reply.string("UserStatus").as_deref(),
        Some(UPDATING),
        "case UserStatus: a call that associated a face leaves the user updating, got {:?}",
        reply.body
    );

    // The association is recorded ON the stored face, so every reader answers from it: the
    // faces of the collection carry the user that holds them, and the `UserId` filter
    // selects exactly those.
    assert_eq!(
        list_faces(&router, "people", None).await,
        vec![
            (free.clone(), Some("alice".to_string())),
            (held.clone(), Some("alice".to_string())),
            (taken.clone(), Some("bob".to_string())),
        ],
        "case ListFaces: each surviving face reports the user that holds it"
    );
    assert_eq!(
        list_faces(&router, "people", Some("alice")).await,
        vec![
            (free.clone(), Some("alice".to_string())),
            (held.clone(), Some("alice".to_string())),
        ],
        "case ListFaces(UserId=alice): the filter reports the faces that user holds"
    );
    assert_eq!(
        list_faces(&router, "people", Some("bob")).await,
        vec![(taken.clone(), Some("bob".to_string()))],
        "case ListFaces(UserId=bob): the filter reports the faces that user holds"
    );
    assert_eq!(
        list_faces(&router, "others", Some("alice")).await,
        Vec::new(),
        "case ListFaces(others): a user of one collection holds none of another's faces"
    );

    // A user that holds a face is no longer one nothing has been associated with. Which of
    // the two remaining statuses it carries is the clock's, and the instant it changes is
    // pinned by `a_user_is_updating_until_the_clock_completes_the_association`.
    for (user_id, status) in list_users(&router, "people").await {
        assert_ne!(
            status, CREATED,
            "case {user_id}: a user holding a face is no longer reported created"
        );
    }

    // What the association persisted is also what `DeleteFaces` answers from: a face a
    // user holds survives the request that would remove it.
    let refused = call(
        &router,
        "DeleteFaces",
        json!({ "CollectionId": "people", "FaceIds": [free.clone()] }),
    )
    .await;
    assert_eq!(
        (
            refused.list("DeletedFaces").len(),
            refused
                .list("UnsuccessfulFaceDeletions")
                .first()
                .and_then(|entry| entry.get("UserId"))
                .and_then(Value::as_str),
        ),
        (0, Some("alice")),
        "case DeleteFaces: an associated face is not removed and names the user that holds \
         it, got {:?}",
        refused.body
    );
}

#[tokio::test]
async fn the_user_match_threshold_decides_which_faces_join_a_user() {
    let router = harness();
    create_collection(&router, "people").await;

    // Every case starts from the same two pictures: the portrait a user's first face is
    // indexed from, and a picture of another face offered to that user afterwards.
    let enrolled = one_face_png(7);
    let candidate = other_face_png();

    // A user that holds no face yet has nothing to match a candidate against, so even the
    // top of the published range admits its first face.
    create_user(&router, "people", "first-face", None).await;
    let first = index_face(&router, "people", &enrolled).await;
    let admitted = associate_faces(
        &router,
        "people",
        "first-face",
        std::slice::from_ref(&first),
        Some(PERCENT_MAX),
        None,
    )
    .await;
    assert_eq!(
        (admitted.status, associated(&admitted)),
        (200, vec![first]),
        "case first face: a user with no face to match against admits its first face at \
         any threshold, got {:?}",
        admitted.body
    );

    // What the service MEASURES between the two pictures, read off the refusal at the top
    // of the range where nothing below 100 can be admitted. Every row below is stated
    // relative to this measurement, so the rule is what is asserted and not one run's
    // number.
    let probe = user_holding_one_face(&router, "people", "probe", &enrolled, &candidate).await;
    let refused = associate_faces(
        &router,
        "people",
        "probe",
        std::slice::from_ref(&probe),
        Some(PERCENT_MAX),
        None,
    )
    .await;
    assert_eq!(
        (associated(&refused), unsuccessful(&refused)),
        (
            Vec::new(),
            vec![(
                probe.clone(),
                None,
                vec![LOW_MATCH_CONFIDENCE.to_string()]
            )],
        ),
        "case probe: a candidate under the threshold joins nothing and is reported with the \
         reason the model names, got {:?}",
        refused.body
    );
    assert_ne!(
        refused.string("UserStatus").as_deref(),
        Some(CREATED),
        "case probe: a user that holds a face is not reported created, got {:?}",
        refused.body
    );
    let measured =
        confidence_of(&refused, &probe).expect("the refusal reports the confidence it measured");
    assert!(
        (PERCENT_MIN..PERCENT_MAX).contains(&measured),
        "case probe: the measured user match confidence is a Percent below the top of the \
         range, got {measured}"
    );
    assert_eq!(
        list_faces(&router, "people", Some("probe")).await.len(),
        1,
        "case probe: a candidate the threshold turned away is not associated"
    );

    // Rows: the threshold at both published bounds, at the measured confidence itself and
    // just past it. A candidate joins the user exactly while the confidence measured for
    // it reaches the threshold.
    let rows: Vec<(&str, Option<f64>, bool)> = vec![
        ("the bottom of the published range", Some(PERCENT_MIN), true),
        ("the measured confidence itself", Some(measured), true),
        (
            "just past the measured confidence",
            Some((measured + 0.5).min(PERCENT_MAX)),
            false,
        ),
        ("the top of the published range", Some(PERCENT_MAX), false),
        // No threshold at all is the documented default, so the outcome follows from where
        // the measurement falls relative to it rather than from a value written here.
        (
            "no threshold at all",
            None,
            measured >= DEFAULT_USER_MATCH_THRESHOLD,
        ),
        (
            "the documented default, named",
            Some(DEFAULT_USER_MATCH_THRESHOLD),
            measured >= DEFAULT_USER_MATCH_THRESHOLD,
        ),
    ];
    for (index, (case, threshold, joins)) in rows.into_iter().enumerate() {
        let user_id = format!("case-{index}");
        let offered =
            user_holding_one_face(&router, "people", &user_id, &enrolled, &candidate).await;
        let reply = associate_faces(
            &router,
            "people",
            &user_id,
            std::slice::from_ref(&offered),
            threshold,
            None,
        )
        .await;
        assert_eq!(
            !associated(&reply).is_empty(),
            joins,
            "case {case:?}: a candidate measured at {measured} must {} a threshold of \
             {threshold:?}, got {:?}",
            if joins { "join under" } else { "be turned away by" },
            reply.body
        );
        assert_eq!(
            list_faces(&router, "people", Some(&user_id)).await.len(),
            if joins { 2 } else { 1 },
            "case {case:?}: the store must hold exactly the faces the answer reported"
        );
    }
}

#[tokio::test]
async fn an_association_completes_once_per_client_request_token() {
    let router = harness();
    create_collection(&router, "people").await;
    create_user(&router, "people", "alice", None).await;
    let portrait = one_face_png(17);
    let one = index_face(&router, "people", &portrait).await;
    let two = index_face(&router, "people", &portrait).await;

    let committed = "assoc-one";
    let first = associate_faces(
        &router,
        "people",
        "alice",
        std::slice::from_ref(&one),
        None,
        Some(committed),
    )
    .await;
    assert_eq!(
        (first.status, associated(&first)),
        (200, vec![one.clone()]),
        "the first association must be carried out, got {:?}",
        first.body
    );

    // The same token with the same request: the same response, and no second association.
    let retried = associate_faces(
        &router,
        "people",
        "alice",
        std::slice::from_ref(&one),
        None,
        Some(committed),
    )
    .await;
    assert_eq!(
        (retried.status, &retried.body),
        (200, &first.body),
        "case same token, same request: the same token must be answered with the same \
         response, got {:?}",
        retried.body
    );

    // The same token with a different request is a parameter mismatch, and it changes
    // nothing: the face it named is still held by nobody.
    let mismatched = associate_faces(
        &router,
        "people",
        "alice",
        std::slice::from_ref(&two),
        None,
        Some(committed),
    )
    .await;
    assert_eq!(
        (mismatched.status, mismatched.error_code().as_str()),
        (400, "IdempotentParameterMismatchException"),
        "case same token, other faces: a reused token with a different request must be \
         refused, got {:?}",
        mismatched.body
    );
    assert_eq!(
        mismatched.string("Message").as_deref(),
        // Spelled out rather than read from the implementation: the text a recorded AWS
        // refusal of a reused token carries.
        Some("Service Unavailable"),
        "case same token, other faces: the refusal carries the wording of its class, got \
         {:?}",
        mismatched.body
    );
    assert_eq!(
        list_faces(&router, "people", Some("alice")).await,
        vec![(one.clone(), Some("alice".to_string()))],
        "case same token, other faces: a refused request must associate nothing"
    );

    // A refused request commits nothing either, so a fresh token carries it out.
    let fresh = associate_faces(
        &router,
        "people",
        "alice",
        std::slice::from_ref(&two),
        None,
        Some("assoc-two"),
    )
    .await;
    assert_eq!(
        (fresh.status, associated(&fresh)),
        (200, vec![two.clone()]),
        "case other token: a request under a token of its own is carried out, got {:?}",
        fresh.body
    );
    assert_eq!(
        list_faces(&router, "people", Some("alice")).await,
        vec![
            (one, Some("alice".to_string())),
            (two, Some("alice".to_string())),
        ],
        "both completed associations must be read back from the store"
    );
}

/// A user is updating until the clock owes the completion of the update its faces are
/// under, and active from that instant on — the transition the answer of `AssociateFaces`
/// leaves pending. The instants are handed to the record, so the rule is decided by
/// arithmetic and nothing here waits, sleeps or reads the host's clock.
#[tokio::test]
async fn a_user_is_updating_until_the_clock_completes_the_association() {
    let span = state::face_association::UPDATE_SPAN;
    let started_at = UNIX_EPOCH + Duration::from_secs(1_767_225_600);

    let fresh = state::StoredUser::new("alice".to_string());
    for (case, now) in [
        ("the instant it was created", started_at),
        ("long after", started_at + Duration::from_secs(3_600)),
    ] {
        assert_eq!(
            fresh.status_at(now),
            CREATED,
            "case {case:?}: a user no face has been associated with is created"
        );
    }

    let mut updating = state::StoredUser::new("alice".to_string());
    updating.face_ids.push(UNMINTED_FACE_ID.to_string());
    updating.start_update(started_at);
    let cases: Vec<(&str, SystemTime, &str)> = vec![
        ("the instant the association was made", started_at, UPDATING),
        ("a nanosecond in", started_at + Duration::from_nanos(1), UPDATING),
        (
            "a nanosecond before it is due",
            started_at + span - Duration::from_nanos(1),
            UPDATING,
        ),
        ("the instant it is due", started_at + span, ACTIVE),
        (
            "long after it is due",
            started_at + span + Duration::from_secs(3_600),
            ACTIVE,
        ),
    ];
    for (case, now, expected) in cases {
        let actual = updating.status_at(now);
        assert_eq!(
            actual, expected,
            "case {case:?}: expected {expected:?}, got {actual:?}"
        );
        assert_eq!(
            updating.modeled_at(now).user_status.as_deref(),
            Some(expected),
            "case {case:?}: the reported `UserStatus` is the status at that instant"
        );
    }
    assert!(
        span > Duration::ZERO,
        "an association answers while the update is still being applied, so the span is \
         never zero"
    );
}

#[tokio::test]
async fn faces_are_associated_only_with_a_user_and_a_collection_that_exist() {
    let router = harness();
    create_collection(&router, "people").await;
    create_user(&router, "people", "alice", None).await;
    let portrait = one_face_png(19);
    let face = index_face(&router, "people", &portrait).await;

    // Rows: a collection id nothing created, one created and then deleted, and a user the
    // live collection holds no record of. Each names a resource the store does not hold,
    // and each refusal names the id the request asked for.
    call(
        &router,
        "CreateCollection",
        json!({ "CollectionId": "gone" }),
    )
    .await;
    call(
        &router,
        "DeleteCollection",
        json!({ "CollectionId": "gone" }),
    )
    .await;

    let cases: Vec<(&str, &str, &str, String)> = vec![
        (
            "a collection nothing created",
            "missing",
            "alice",
            "The collection id: missing does not exist".to_string(),
        ),
        (
            "a collection that was deleted",
            "gone",
            "alice",
            "The collection id: gone does not exist".to_string(),
        ),
        (
            "a user the collection does not hold",
            "people",
            "carol",
            "The user id: carol does not exist".to_string(),
        ),
    ];
    for (case, collection_id, user_id, message) in cases {
        let reply = associate_faces(
            &router,
            collection_id,
            user_id,
            std::slice::from_ref(&face),
            None,
            None,
        )
        .await;
        assert_eq!(
            (reply.status, reply.error_code().as_str()),
            (400, "ResourceNotFoundException"),
            "case {case:?}: the request must be refused, got {:?}",
            reply.body
        );
        assert_eq!(
            reply.string("Message").as_deref(),
            Some(message.as_str()),
            "case {case:?}: the refusal names what the request asked for, got {:?}",
            reply.body
        );
    }

    // One past each published bound of `FaceIds` is outside the model's own length range,
    // so it is refused before the handler decides anything.
    for (case, face_ids) in [
        (
            "one face short of the published minimum",
            face_id_list(FACE_IDS_LEN_MIN - 1),
        ),
        (
            "one face past the published maximum",
            face_id_list(FACE_IDS_LEN_MAX + 1),
        ),
    ] {
        let reply = associate_faces(&router, "people", "alice", &face_ids, None, None).await;
        assert_eq!(
            reply.status, 400,
            "case {case:?}: a list of {} outside the modelled length range must be refused, \
             got {:?}",
            face_ids.len(),
            reply.body
        );
    }

    // Not one of the refusals associated anything.
    assert_eq!(
        list_faces(&router, "people", None).await,
        vec![(face, None)],
        "a refused request must associate nothing"
    );
}

#[tokio::test]
async fn a_user_holds_at_most_the_published_number_of_faces() {
    let router = harness();
    create_collection(&router, "full").await;
    create_user(&router, "full", "alice", None).await;

    // One portrait, indexed once per face: filling a user is about the bound, not the
    // pictures, and every face measured off the same portrait matches the user it is
    // offered to, so no face here is turned away for its resemblance.
    let portrait = one_face_png(13);
    let capacity = state::face_association::MAX_FACES_PER_USER;
    let mut faces: Vec<String> = Vec::new();
    for _ in 0..capacity + 2 {
        faces.push(index_face(&router, "full", &portrait).await);
    }

    // Up to one short of the capacity, in a single request.
    let most = faces[..capacity - 1].to_vec();
    let reply = associate_faces(&router, "full", "alice", &most, None, None).await;
    assert_eq!(
        (reply.status, associated(&reply)),
        (200, most.clone()),
        "case up to the capacity: every face must be associated, got {:?}",
        reply.body
    );

    // The request that reaches the capacity part way through: the face there is room for
    // is associated, and the one past the bound is reported as an unsuccessful
    // association, which is what the operation documents for a user that already holds
    // every face it can. The modelled reason enum names no value for that outcome, so the
    // entry carries none.
    let over = faces[capacity - 1..=capacity].to_vec();
    let partial = associate_faces(&router, "full", "alice", &over, None, None).await;
    assert_eq!(
        (associated(&partial), unsuccessful(&partial)),
        (
            vec![faces[capacity - 1].clone()],
            vec![(faces[capacity].clone(), None, Vec::new())],
        ),
        "case the capacity reached part way: the face that fits joins and the one past the \
         bound is reported, got {:?}",
        partial.body
    );

    // A call to a user that is already full is refused outright.
    let refused = associate_faces(
        &router,
        "full",
        "alice",
        std::slice::from_ref(&faces[capacity + 1]),
        None,
        None,
    )
    .await;
    assert_eq!(
        (refused.status, refused.error_code().as_str()),
        (400, "ServiceQuotaExceededException"),
        "case faces={capacity}: associating with a full user must be refused, got {:?}",
        refused.body
    );
    assert_eq!(
        list_faces(&router, "full", Some("alice")).await.len(),
        capacity,
        "a refused call must associate nothing: the user still holds exactly its capacity"
    );
}
