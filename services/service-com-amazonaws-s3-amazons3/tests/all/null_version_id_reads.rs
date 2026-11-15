// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for WHICH VERSION ID a read of an object reports.
//!
//! Every assertion drives a real HTTP request through the router (the harness is the one
//! [`super::bucket_lifecycle`], [`super::object_and_tagging_roundtrip`],
//! [`super::object_versioning`] and [`super::object_attributes`] already own), so what is
//! pinned is the wire behaviour — the `x-amz-version-id` response header and the listing
//! that has to agree with it — rather than any handler's internals.
//!
//! The rule under test, asserted over a SET of inputs rather than one request:
//!
//! > A read of an object version reports the id that version carries. A bucket that
//! > CARRIES a versioning status has a version id for every version it holds: the unique
//! > id a write minted while the status was `Enabled`, and the `null` version id for every
//! > other version it holds — the write made while versioning was suspended, and the
//! > object stored before the status was ever set. Only a bucket whose versioning state has
//! > never been set has no version ids at all, so only there does a read report none.
//!
//! Its sources: "Version ID of the object" (`GetObjectOutput$VersionId`,
//! `HeadObjectOutput$VersionId` and `GetObjectAttributesOutput$VersionId`, each bound to
//! the `x-amz-version-id` header —
//! `smithy_query(query="operation",operation="GetObject,HeadObject,GetObjectAttributes",
//! model_path="services/service-com-amazonaws-s3-amazons3/smithy-model/com-amazonaws-s3-amazons3.json")`);
//! "Enabled—Enables versioning for the objects in the bucket. All objects added to the
//! bucket receive a unique version ID. Suspended—Disables versioning for the objects in the
//! bucket. All objects added to the bucket receive the version ID null", "objects stored in
//! your bucket before you set the versioning state have a version ID of null" and "if the
//! versioning state has never been set on a bucket, it has no versioning state"
//! (`PutBucketVersioning`'s own documentation); and "If you enable versioning for a bucket,
//! Amazon S3 automatically generates a unique version ID for the object being stored.
//! Amazon S3 returns this ID in the response" (`PutObjectOutput$VersionId`), which is why a
//! write that minted none reports none while a READ of what it stored still names it.
//!
//! The minted ids are values the SERVICE generates per write, so nothing here asserts a
//! literal one: each row's expectation is either the id a write reported or the one literal
//! the API publishes for a version nothing minted an id for
//! ([`service_com_amazonaws_s3_amazons3::state::NULL_VERSION_ID`]).

use service_com_amazonaws_s3_amazons3::state::NULL_VERSION_ID;
use smithy_server_runtime::MultiServiceRouter;

use super::bucket_lifecycle::{router, Answer};
use super::object_and_tagging_roundtrip::{bucket, get_object, put_object, text};
use super::object_attributes::get_object_attributes;
use super::object_versioning::{
    head_object, list_object_versions, set_versioning, version_listing_of,
};

/// The header S3 reports the version an answer is about in.
const VERSION_ID_HEADER: &str = "x-amz-version-id";

/// The key every row writes, so the address a read carries is the same across the table and
/// only the bucket's versioning history differs.
const KEY: &str = "versioned/subject.bin";

/// The bytes of the setup write a row makes while versioning is `Enabled`.
const SETUP_BODY: &[u8] = b"written-while-enabled";

/// The bytes of the write under test, distinct from [`SETUP_BODY`] so a read answering the
/// wrong version is visible in the body as well as in the header.
const SUBJECT_BODY: &[u8] = b"the-write-under-test";

/// The `BucketVersioningStatus` value that mints a unique id per write
/// (`smithy_query(query="shape",shape="BucketVersioningStatus")` publishes the two values).
const ENABLED: &str = "Enabled";
/// See [`ENABLED`] — the status under which "all objects added to the bucket receive the
/// version ID null".
const SUSPENDED: &str = "Suspended";

/// One row of the table: the case, the bucket it stands up, the versioning statuses applied
/// BEFORE the write under test and those applied AFTER it.
///
/// The statuses are what the rule generalises over — whether the bucket carries one at all,
/// which one it carries when the write lands, and whether it changes afterwards — so the
/// expectation is DERIVED from them rather than written out per row.
struct Row {
    case: &'static str,
    bucket: &'static str,
    before: &'static [&'static str],
    after: &'static [&'static str],
}

/// The rows: every versioning history a key's current version can have.
const ROWS: &[Row] = &[
    Row {
        case: "a bucket whose versioning state was never set",
        bucket: "null-id-plain",
        before: &[],
        after: &[],
    },
    Row {
        case: "a bucket suspended without ever being enabled",
        bucket: "null-id-suspended-first",
        before: &[SUSPENDED],
        after: &[],
    },
    Row {
        case: "a bucket enabled and then suspended",
        bucket: "null-id-resuspended",
        before: &[ENABLED, SUSPENDED],
        after: &[],
    },
    Row {
        case: "a suspended write, after which versioning is enabled again",
        bucket: "null-id-reenabled",
        before: &[ENABLED, SUSPENDED],
        after: &[ENABLED],
    },
    Row {
        case: "an object stored before versioning was ever set, then enabled",
        bucket: "null-id-enabled-after",
        before: &[],
        after: &[ENABLED],
    },
    Row {
        case: "an object stored before versioning was ever set, then suspended",
        bucket: "null-id-suspended-after",
        before: &[],
        after: &[SUSPENDED],
    },
    Row {
        case: "a versioning-enabled bucket, which mints an id per write",
        bucket: "null-id-enabled",
        before: &[ENABLED],
        after: &[],
    },
];

/// Write `body` at [`KEY`] and answer with the version id the write itself reported.
async fn write(router: &MultiServiceRouter, name: &str, case: &str, body: &[u8]) -> Option<String> {
    let written = put_object(router, name, KEY, body, &[]).await;
    assert_eq!(
        written.status, 200,
        "case {case:?}: the write must be stored, got {} with body {:?}",
        written.status,
        text(&written)
    );
    written.header(VERSION_ID_HEADER)
}

/// What one read answered: the operation's name and its response.
struct Read {
    operation: &'static str,
    answer: Answer,
}

/// The three reads that report the version they resolved, driven at `query` in turn.
async fn reads(router: &MultiServiceRouter, name: &str, query: &str) -> Vec<Read> {
    vec![
        Read {
            operation: "GetObject",
            answer: get_object(router, name, KEY, query, &[]).await,
        },
        Read {
            operation: "HeadObject",
            answer: head_object(router, name, KEY, query).await,
        },
        Read {
            operation: "GetObjectAttributes",
            answer: get_object_attributes(
                router,
                name,
                KEY,
                // `?attributes` is already the first query parameter of that call, so a
                // version is named with `&` there.
                &query.replacen('?', "&", 1),
                &["ETag", "ObjectSize"],
            )
            .await,
        },
    ]
}

/// Stand `row` up: the statuses before the write, the setup write every `Enabled` stretch
/// makes, and the write under test. Answers with the ids the setup writes minted and the id
/// the write under test reported.
async fn stand_up(router: &MultiServiceRouter, row: &Row) -> (Vec<String>, Option<String>) {
    bucket(router, row.bucket).await;
    let mut minted = Vec::new();
    for status in row.before {
        set_versioning(router, row.bucket, status).await;
        if *status == ENABLED {
            // A version WITH an id of its own, so the write under test displaces a real
            // version rather than being the key's only one.
            let id = write(router, row.bucket, row.case, SETUP_BODY).await;
            minted.push(id.unwrap_or_else(|| {
                panic!("case {:?}: a write under {ENABLED} must mint a version id", row.case)
            }));
        }
    }
    let subject = write(router, row.bucket, row.case, SUBJECT_BODY).await;
    for status in row.after {
        set_versioning(router, row.bucket, status).await;
    }
    (minted, subject)
}

#[tokio::test]
async fn a_read_reports_the_version_id_the_version_it_served_carries() {
    let router = router();

    for row in ROWS {
        let case = row.case;
        let (minted, subject) = stand_up(&router, row).await;

        // What the write itself reported: an id exactly when the status in force minted one
        // ("Amazon S3 returns this ID in the response"), which is the state the reads below
        // are judged against.
        let write_was_versioned = row.before.last().is_some_and(|status| *status == ENABLED);
        assert_eq!(
            subject.is_some(),
            write_was_versioned,
            "case {case:?}: a write reports a version id exactly when the bucket's status \
             minted one, got {subject:?}"
        );

        // The id a READ of that version must report: the minted one, the `null` version id
        // for a version of a bucket carrying a status that minted none, and nothing at all
        // for a bucket with no versioning state.
        let carries_a_status = !row.before.is_empty() || !row.after.is_empty();
        let expected = match &subject {
            Some(id) => Some(id.clone()),
            None => carries_a_status.then(|| NULL_VERSION_ID.to_string()),
        };

        for Read { operation, answer } in reads(&router, row.bucket, "").await {
            assert_eq!(
                answer.status, 200,
                "case {case:?}/{operation}: expected HTTP 200, got {} with body {:?}",
                answer.status,
                text(&answer)
            );
            assert_eq!(
                answer.header(VERSION_ID_HEADER),
                expected,
                "case {case:?}/{operation}: the version served is reported as {expected:?}, \
                 got {:?}",
                answer.header(VERSION_ID_HEADER)
            );
        }

        // A GET answers the bytes of the version it named, so the header and the body are
        // about one version rather than two.
        let served = get_object(&router, row.bucket, KEY, "", &[]).await;
        assert_eq!(
            served.body.as_ref(),
            SUBJECT_BODY,
            "case {case:?}: the current version must hold the bytes of the write under test, \
             got {:?}",
            text(&served)
        );

        // The reported id ADDRESSES the same version back — what a client round-tripping a
        // read's `VersionId` into a versioned read depends on.
        if let Some(id) = &expected {
            let query = format!("?versionId={id}");
            for Read { operation, answer } in reads(&router, row.bucket, &query).await {
                assert_eq!(
                    answer.status, 200,
                    "case {case:?}/{operation}: the reported version {id:?} must address a \
                     version, got {} with body {:?}",
                    answer.status,
                    text(&answer)
                );
                assert_eq!(
                    answer.header(VERSION_ID_HEADER).as_deref(),
                    Some(id.as_str()),
                    "case {case:?}/{operation}: a read naming {id:?} must report it, got {:?}",
                    answer.header(VERSION_ID_HEADER)
                );
            }
            let addressed = get_object(&router, row.bucket, KEY, &query, &[]).await;
            assert_eq!(
                addressed.body.as_ref(),
                SUBJECT_BODY,
                "case {case:?}: the version {id:?} must hold the bytes of the write under \
                 test, got {:?}",
                text(&addressed)
            );
        }

        // And the id is one the BUCKET really holds: the listing of the key's versions
        // reports every setup version beside the one the reads named.
        let listing = version_listing_of(&list_object_versions(&router, row.bucket, "").await);
        let listed: Vec<String> = listing
            .entries()
            .into_iter()
            .filter(|(key, _)| key == KEY)
            .map(|(_, version_id)| version_id)
            .collect();
        let mut wanted = minted.clone();
        wanted.push(
            expected
                .clone()
                .unwrap_or_else(|| NULL_VERSION_ID.to_string()),
        );
        wanted.sort();
        let mut found = listed.clone();
        found.sort();
        assert_eq!(
            found, wanted,
            "case {case:?}: the version listing must hold exactly {wanted:?}, got {listed:?}"
        );
    }
}

#[tokio::test]
async fn the_null_version_of_a_suspended_bucket_is_listed_and_read_beside_the_minted_ones() {
    let router = router();
    let name = "null-id-chain";
    bucket(&router, name).await;
    set_versioning(&router, name, ENABLED).await;

    // Several writes under `Enabled`, each minting its own id, so the null version below
    // takes its place in a chain rather than being the key's only version. Three rounds,
    // because the rule is about the SET of ids the key then holds rather than one pair.
    let mut stored: Vec<(String, Vec<u8>)> = Vec::new();
    for round in 0..3u8 {
        let body = format!("revision-{round}").into_bytes();
        let id = write(&router, name, "the enabled writes", &body)
            .await
            .unwrap_or_else(|| panic!("round {round}: a write under {ENABLED} must mint an id"));
        assert!(
            !stored.iter().any(|(held, _)| *held == id),
            "round {round}: each version must carry its own id, got the already-stored {id:?}"
        );
        stored.push((id, body));
    }

    // Suspension is a state of the BUCKET: the versions it already holds keep their ids,
    // and the write made now is the `null` version.
    set_versioning(&router, name, SUSPENDED).await;
    let suspended = write(&router, name, "the suspended write", SUBJECT_BODY).await;
    assert_eq!(
        suspended, None,
        "a write while versioning is {SUSPENDED} mints no id, got {suspended:?}"
    );
    stored.push((NULL_VERSION_ID.to_string(), SUBJECT_BODY.to_vec()));

    // The listing holds every version, the null one among them.
    let listing = version_listing_of(&list_object_versions(&router, name, "").await);
    let mut listed: Vec<String> = listing
        .entries()
        .into_iter()
        .filter(|(key, _)| key == KEY)
        .map(|(_, version_id)| version_id)
        .collect();
    listed.sort();
    let mut wanted: Vec<String> = stored.iter().map(|(id, _)| id.clone()).collect();
    wanted.sort();
    assert_eq!(
        listed, wanted,
        "the listing must hold exactly the versions written, expected {wanted:?}"
    );

    // Each listed id reads back the bytes stored under it, and reports itself — the null
    // version exactly as the minted ones.
    for (id, body) in &stored {
        let query = format!("?versionId={id}");
        let read = get_object(&router, name, KEY, &query, &[]).await;
        assert_eq!(
            read.status, 200,
            "case {id:?}: the listed version must read back, got {} with body {:?}",
            read.status,
            text(&read)
        );
        assert_eq!(
            read.body.as_ref(),
            body.as_slice(),
            "case {id:?}: expected the bytes stored under that version, got {:?}",
            text(&read)
        );
        assert_eq!(
            read.header(VERSION_ID_HEADER).as_deref(),
            Some(id.as_str()),
            "case {id:?}: the read must name the version it served, got {:?}",
            read.header(VERSION_ID_HEADER)
        );
    }

    // And the key's CURRENT version is the null one: a read naming no version answers the
    // suspended write's bytes under the `null` version id.
    for Read { operation, answer } in reads(&router, name, "").await {
        assert_eq!(
            answer.status, 200,
            "case the current version/{operation}: expected HTTP 200, got {} with body {:?}",
            answer.status,
            text(&answer)
        );
        assert_eq!(
            answer.header(VERSION_ID_HEADER).as_deref(),
            Some(NULL_VERSION_ID),
            "case the current version/{operation}: the current version is the null one, got \
             {:?}",
            answer.header(VERSION_ID_HEADER)
        );
    }
}
