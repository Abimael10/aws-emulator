// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for `DeleteObjects`, S3's multi-object delete.
//!
//! Every assertion drives a real HTTP request through the router — the harness is the one
//! [`super::bucket_lifecycle`], [`super::object_and_tagging_roundtrip`],
//! [`super::object_storage`] and [`super::object_versioning`] already own — so what is
//! pinned is the wire behaviour (status, the result document, what the store holds
//! afterwards) rather than any handler's internals.
//!
//! The rule under test, asserted over a SET of payloads rather than one request:
//!
//! > One `DeleteObjects` performs the delete of each `Object` its `Delete` payload names —
//! > the version a `VersionId` names, or the simple delete that lays a delete marker on a
//! > bucket carrying a versioning state — and reports the result of each in the response:
//! > a key that held nothing is reported deleted too — a key's delete is idempotent — while
//! > an identifier that pins a `VersionId` the bucket holds no version of is that key's own
//! > `Errors` entry while the bucket still holds OTHER versions of that key, and is confirmed
//! > deleted once the bucket holds no version of the key at all, whether that token is of the
//! > published shape or no version id's shape at all, and `Quiet` leaves the successes out.
//!
//! Its sources (this operation's own documentation): "For each key, Amazon S3 performs a
//! delete operation and returns the result of that delete, success or failure, in the
//! response. If the object specified in the request isn't found, Amazon S3 confirms the
//! deletion by returning the result as deleted"; "The request can contain a list of up to
//! 1,000 keys that you want to delete"; "In quiet mode the response includes only keys
//! where the delete operation encountered an error"; and, for the marker a simple delete
//! lays, "If bucket versioning is enabled, the operation inserts a delete marker, which
//! becomes the current version of the object" (`DeleteObject`).

use std::collections::BTreeMap;

use quick_xml::events::Event;
use quick_xml::Reader;
use service_com_amazonaws_s3_amazons3::state::{NULL_VERSION_ID, VERSION_ID_CHARS};
use smithy_server_runtime::MultiServiceRouter;

use super::bucket_lifecycle::{router, send, Answer};
use super::bucket_tagging::unescaped;
use super::object_and_tagging_roundtrip::{bucket, get_object, put_object, text};
use super::object_storage::{list_objects_v2, listing_of};
use super::object_versioning::{list_object_versions, set_versioning, version_listing_of};

/// One identifier of a `Delete` payload: the key, and the version id when the case names
/// one.
type Identifier<'a> = (&'a str, Option<&'a str>);

/// The `Delete` payload a `DeleteObjects` request carries, from the identifiers and the
/// quiet flag a case names.
fn delete_payload(identifiers: &[Identifier], quiet: bool) -> String {
    delete_payload_quiet_spelled(identifiers, quiet.then_some("true"))
}

/// The same payload, with the `Quiet` element carrying exactly the text the case gives — and
/// no `Quiet` element at all for `None`.
///
/// The one builder both forms go through, so a case that spells the element itself and one that
/// states the flag cannot disagree about the rest of the document.
fn delete_payload_quiet_spelled(identifiers: &[Identifier], quiet: Option<&str>) -> String {
    let mut payload =
        String::from("<Delete xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\">");
    for (key, version_id) in identifiers {
        payload.push_str("<Object><Key>");
        payload.push_str(key);
        payload.push_str("</Key>");
        if let Some(version_id) = version_id {
            payload.push_str("<VersionId>");
            payload.push_str(version_id);
            payload.push_str("</VersionId>");
        }
        payload.push_str("</Object>");
    }
    if let Some(quiet) = quiet {
        payload.push_str("<Quiet>");
        payload.push_str(quiet);
        payload.push_str("</Quiet>");
    }
    payload.push_str("</Delete>");
    payload
}

/// `POST /<bucket>?delete` — `DeleteObjects`, with the payload the case carries.
async fn delete_objects(
    router: &MultiServiceRouter,
    name: &str,
    identifiers: &[Identifier<'_>],
    quiet: bool,
) -> Answer {
    send(
        router,
        "POST",
        &format!("/{name}?delete"),
        &[],
        &delete_payload(identifiers, quiet),
    )
    .await
}

/// One `DeleteObjects` response, as a client's XML reader sees it: one map per `Deleted`
/// and per `Error` entry, each in document order.
#[derive(Debug, Default)]
struct DeleteResult {
    /// One map per `Deleted` entry, keyed by element name.
    deleted: Vec<BTreeMap<String, String>>,
    /// One map per `Error` entry, keyed by element name.
    errors: Vec<BTreeMap<String, String>>,
}

impl DeleteResult {
    /// Every `Deleted` entry's key, in response order.
    fn deleted_keys(&self) -> Vec<String> {
        self.deleted
            .iter()
            .map(|entry| entry.get("Key").cloned().unwrap_or_default())
            .collect()
    }

    /// Every `Error` entry as `(key, code)`, in response order.
    fn error_codes(&self) -> Vec<(String, String)> {
        self.errors
            .iter()
            .map(|entry| {
                (
                    entry.get("Key").cloned().unwrap_or_default(),
                    entry.get("Code").cloned().unwrap_or_default(),
                )
            })
            .collect()
    }

    /// The `Deleted` entry for `key`, or `None` when the response reports none.
    fn deleted_entry(&self, key: &str) -> Option<&BTreeMap<String, String>> {
        self.deleted
            .iter()
            .find(|entry| entry.get("Key").map(String::as_str) == Some(key))
    }
}

/// Read a delete result with an XML reader, the same accumulate-per-element walk
/// [`super::object_versioning::version_listing_of`] does for a version listing.
fn delete_result_of(answer: &Answer) -> DeleteResult {
    let body = String::from_utf8_lossy(&answer.body).into_owned();
    let mut reader = Reader::from_str(&body);

    let mut result = DeleteResult::default();
    let mut element = String::new();
    let mut current = String::new();
    let mut entry: Option<(bool, BTreeMap<String, String>)> = None;
    loop {
        match reader
            .read_event()
            .unwrap_or_else(|error| panic!("the result body must be well-formed XML: {error}"))
        {
            Event::Start(start) => {
                element = String::from_utf8_lossy(start.name().as_ref()).into_owned();
                current.clear();
                match element.as_str() {
                    "Deleted" => entry = Some((true, BTreeMap::new())),
                    "Error" => entry = Some((false, BTreeMap::new())),
                    _ => {}
                }
            }
            Event::Text(text) => {
                let decoded = text
                    .xml10_content()
                    .unwrap_or_else(|error| panic!("the result body must decode: {error}"));
                current.push_str(&unescaped(&decoded));
            }
            Event::End(end) => {
                let closing = String::from_utf8_lossy(end.name().as_ref()).into_owned();
                match closing.as_str() {
                    "Deleted" | "Error" => {
                        if let Some((is_deleted, done)) = entry.take() {
                            if is_deleted {
                                result.deleted.push(done);
                            } else {
                                result.errors.push(done);
                            }
                        }
                    }
                    name if name == element => {
                        let value = std::mem::take(&mut current);
                        if let Some((_, entry)) = entry.as_mut() {
                            entry.insert(name.to_string(), value);
                        }
                    }
                    _ => {}
                }
                element.clear();
                current.clear();
            }
            Event::Eof => break,
            _ => {}
        }
    }
    result
}

/// Write `keys` into the bucket, handing back the version id each write minted (the empty
/// string when the bucket minted none).
async fn write_keys(
    router: &MultiServiceRouter,
    name: &str,
    keys: &[&str],
) -> BTreeMap<String, String> {
    let mut minted = BTreeMap::new();
    for key in keys {
        let written = put_object(router, name, key, format!("body of {key}").as_bytes(), &[]).await;
        assert_eq!(
            written.status,
            200,
            "the write of {key:?} must be accepted, got {} with body {:?}",
            written.status,
            text(&written)
        );
        minted.insert(
            (*key).to_string(),
            written.header("x-amz-version-id").unwrap_or_default(),
        );
    }
    minted
}

#[tokio::test]
async fn a_batch_delete_reports_the_result_of_every_identifier_it_names() {
    let keys: &[&str] = &["one", "agent/two.txt", "agent/nested/three.txt"];

    // (case, the keys the payload names, quiet). The rows differ in how many identifiers
    // one payload carries, in the shape of the keys, in whether a named key is even
    // stored, and in the response mode.
    let rows: &[(&str, &[&str], bool)] = &[
        ("one stored key", &["one"], false),
        ("every stored key at once", keys, false),
        (
            "a key that is not stored is still reported deleted",
            &["one", "never-written"],
            false,
        ),
        ("only keys that are not stored", &["absent/a", "absent/b"], false),
        ("quiet mode reports no success", keys, true),
        ("the same key twice", &["one", "one"], false),
    ];

    for (case, named, quiet) in rows {
        // A fresh store per row, so one row's deletes cannot decide another's.
        let router = router();
        let name = "batch-delete";
        bucket(&router, name).await;
        write_keys(&router, name, keys).await;

        let answer = delete_objects(&router, name, &identifiers(named), *quiet).await;
        assert_eq!(
            answer.status,
            200,
            "case {case:?}: a multi-object delete answers 200, got {} with body {:?}",
            answer.status,
            text(&answer)
        );
        let result = delete_result_of(&answer);
        assert!(
            result.errors.is_empty(),
            "case {case:?}: no identifier can fail here, got {:?}",
            result.error_codes()
        );
        let expected: Vec<String> = if *quiet {
            Vec::new()
        } else {
            named.iter().map(|key| (*key).to_string()).collect()
        };
        assert_eq!(
            result.deleted_keys(),
            expected,
            "case {case:?}: expected one Deleted entry per identifier in request order, got \
             {:?}",
            result.deleted_keys()
        );

        // Whatever the response mode, every key the payload named is gone from the
        // listing, and every key it did not name is still there.
        let left = listing_of(&list_objects_v2(&router, name, "").await).keys();
        for key in keys {
            let still_there = !named.contains(key);
            assert_eq!(
                left.contains(&(*key).to_string()),
                still_there,
                "case {case:?}: key {key:?} must {} be listed, got {left:?}",
                if still_there { "still" } else { "no longer" }
            );
        }
    }
}

#[tokio::test]
async fn an_identifier_that_pins_an_unresolvable_version_is_one_error_and_takes_nothing_away() {
    // RULE: an identifier that pins a `VersionId` resolves to exactly the version of that
    // name, so one the bucket holds no version of resolves to none: that identifier is the
    // key's own `Errors` entry — `NoSuchVersion` "indicates that the version ID specified in
    // the request does not match an existing version" — reported exactly once, never in
    // `Deleted`, and the key's stored versions are left where they are.
    //
    // The rows differ along the FORM of the unresolvable token: one of the published shape
    // that this store never minted, the real version id with its last character changed (the
    // nearest miss a client's own bookkeeping produces), a token at the shape's own minimum
    // width, the literal `null` version id on a bucket that has no null version, and a token
    // that is no version id at all. Each rides in one batch beside a LIVE version of the same
    // key, so the two sides of the one response are graded together.
    let key = "versioned.bin";
    let other = "kept.bin";
    // (case, how the unresolvable token is built from the live version id this run minted —
    // `None` for a token that stands on its own).
    let rows: [(&str, Option<String>); 5] = [
        (
            "a token of the published shape that nothing minted",
            Some("a".repeat(36)),
        ),
        (
            "a token at the shape's minimum width",
            Some("b".repeat(VERSION_ID_CHARS)),
        ),
        (
            "the literal null version on a versioned bucket",
            Some("null".to_string()),
        ),
        (
            "a token that is no version id at all",
            Some("not-a-version".to_string()),
        ),
        ("the live version id with its last character changed", None),
    ];
    for (case, stated) in rows {
        let router = router();
        let name = "batch-delete-unresolvable";
        bucket(&router, name).await;
        set_versioning(&router, name, "Enabled").await;
        // Two writes of the key, so the batch can take one version away and leave the other
        // readable, and a second key whose version is never named.
        let first = write_keys(&router, name, &[key]).await[key].clone();
        let doomed = write_keys(&router, name, &[key]).await[key].clone();
        let kept = write_keys(&router, name, &[other]).await[other].clone();
        // The nearest miss a client's own bookkeeping produces is built from the id this run
        // minted, since that id is a per-run value.
        let bogus = match stated {
            Some(stated) => stated,
            None => {
                let mut flipped: Vec<char> = doomed.chars().collect();
                let last = flipped.len() - 1;
                flipped[last] = if flipped[last] == 'a' { 'b' } else { 'a' };
                flipped.into_iter().collect::<String>()
            }
        };
        assert_ne!(
            bogus, doomed,
            "case {case:?}: the unresolvable token must not name the live version"
        );

        let answer = delete_objects(
            &router,
            name,
            &[(key, Some(&doomed)), (key, Some(&bogus))],
            false,
        )
        .await;
        assert_eq!(
            answer.status,
            200,
            "case {case:?}: the batch itself answers 200, got {} with body {:?}",
            answer.status,
            text(&answer)
        );
        let result = delete_result_of(&answer);

        // The live version is the one success, and the unresolvable identifier is on the
        // error side exactly once, echoing the pair the request sent.
        let deleted_pairs: Vec<(String, String)> = result
            .deleted
            .iter()
            .map(|entry| {
                (
                    entry.get("Key").cloned().unwrap_or_default(),
                    entry.get("VersionId").cloned().unwrap_or_default(),
                )
            })
            .collect();
        assert_eq!(
            deleted_pairs,
            vec![(key.to_string(), doomed.clone())],
            "case {case:?}: only the live version is reported deleted, got {deleted_pairs:?} \
             in body {:?}",
            text(&answer)
        );
        let errored: Vec<(String, String, String, String)> = result
            .errors
            .iter()
            .map(|entry| {
                let field = |name: &str| entry.get(name).cloned().unwrap_or_default();
                (
                    field("Key"),
                    field("VersionId"),
                    field("Code"),
                    field("Message"),
                )
            })
            .collect();
        assert_eq!(
            errored,
            vec![(
                key.to_string(),
                bogus.clone(),
                "NoSuchVersion".to_string(),
                "The specified version does not exist.".to_string()
            )],
            "case {case:?}: the unresolvable pair is reported once on the error side, got \
             {errored:?} in body {:?}",
            text(&answer)
        );

        // And nothing else moved: the key's other version and the untouched key are both
        // still readable.
        for (held_key, version_id) in [(key, &first), (other, &kept)] {
            let read = get_object(
                &router,
                name,
                held_key,
                &format!("?versionId={version_id}"),
                &[],
            )
            .await;
            assert_eq!(
                read.status,
                200,
                "case {case:?}: the version {version_id:?} of {held_key:?} must still be \
                 readable, got {} with body {:?}",
                read.status,
                text(&read)
            );
        }
    }
}

#[tokio::test]
async fn a_batch_delete_drains_a_versioned_bucket_and_reports_each_marker() {
    let router = router();
    let name = "batch-delete-versions";
    bucket(&router, name).await;
    set_versioning(&router, name, "Enabled").await;
    let keys: &[&str] = &["one", "agent/two.txt"];
    let minted = write_keys(&router, name, keys).await;

    // Without a version id each identifier is the simple delete, so each one lays a delete
    // marker and reports the marker it created.
    let laid = delete_objects(&router, name, &identifiers(keys), false).await;
    assert_eq!(
        laid.status,
        200,
        "the simple batch delete answers 200, got {} with body {:?}",
        laid.status,
        text(&laid)
    );
    let result = delete_result_of(&laid);
    assert_eq!(
        result.deleted_keys(),
        keys.iter().map(|key| (*key).to_string()).collect::<Vec<_>>(),
        "every key is reported deleted, got {:?}",
        result.deleted_keys()
    );
    let mut marker_ids: Vec<(String, String)> = Vec::new();
    for key in keys {
        let entry = result
            .deleted_entry(key)
            .unwrap_or_else(|| panic!("case {key:?}: the key must be reported deleted"));
        assert_eq!(
            entry.get("DeleteMarker").map(String::as_str),
            Some("true"),
            "case {key:?}: the delete reports that it created a delete marker, got {entry:?}"
        );
        let marker_id = entry
            .get("DeleteMarkerVersionId")
            .cloned()
            .unwrap_or_default();
        assert!(
            !marker_id.is_empty() && marker_id != minted[*key],
            "case {key:?}: the marker carries a minted id of its own, got {marker_id:?}"
        );
        marker_ids.push(((*key).to_string(), marker_id));
    }

    // Nothing was taken away: each key's version is still stored behind its marker.
    let listing = version_listing_of(&list_object_versions(&router, name, "").await);
    assert_eq!(
        listing.entries().len(),
        4,
        "two versions and two markers are stored, got {:?}",
        listing.entries()
    );
    for key in keys {
        let read = get_object(
            &router,
            name,
            key,
            &format!("?versionId={}", minted[*key]),
            &[],
        )
        .await;
        assert_eq!(
            read.status,
            200,
            "case {key:?}: the version behind the marker is still readable, got {} with body \
             {:?}",
            read.status,
            text(&read)
        );
    }

    // Naming every version AND every marker empties the bucket in one request. Each
    // identifier performs exactly what a `DeleteObject` for it would and reports what it
    // resolved to, judged against what the bucket holds WHEN that identifier runs: a
    // `VersionId` the bucket holds no version of is that identifier's own failure while the
    // key still holds versions, and is confirmed once the batch has emptied the key — whether
    // the token is of the published shape or no version id's shape at all (the single-object
    // path refuses the ARGUMENT for the latter, see `version_id_classification`).
    let shaped = "a".repeat(VERSION_ID_CHARS);
    let shapeless = "not-a-version".to_string();
    let mut named: Vec<(String, Option<String>)> = Vec::new();
    // Both unresolvable tokens BEFORE anything is drained, while the key holds its version
    // and its marker.
    named.push(("one".to_string(), Some(shaped.clone())));
    named.push(("one".to_string(), Some(shapeless.clone())));
    for key in keys {
        named.push(((*key).to_string(), Some(minted[*key].clone())));
    }
    for (key, marker_id) in &marker_ids {
        named.push((key.clone(), Some(marker_id.clone())));
    }
    // And again AFTER, when the key the batch just emptied holds no version at all.
    named.push(("one".to_string(), Some(shaped.clone())));
    named.push(("one".to_string(), Some(shapeless.clone())));
    let borrowed: Vec<Identifier> = named
        .iter()
        .map(|(key, version_id)| (key.as_str(), version_id.as_deref()))
        .collect();

    let drained = delete_objects(&router, name, &borrowed, false).await;
    assert_eq!(
        drained.status,
        200,
        "the versioned batch delete answers 200, got {} with body {:?}",
        drained.status,
        text(&drained)
    );
    let result = delete_result_of(&drained);
    assert_eq!(
        result.deleted_keys().len(),
        6,
        "every version and marker named is reported deleted, beside the two tokens the batch \
         had already emptied their key of, got {:?}",
        result.deleted_keys()
    );
    assert_eq!(
        result
            .deleted
            .iter()
            .filter_map(|entry| entry.get("VersionId").cloned())
            .filter(|version_id| *version_id == shaped || *version_id == shapeless)
            .count(),
        2,
        "a token naming no stored version confirms a delete only once its key holds no \
         version, got {:?}",
        result.deleted
    );
    assert_eq!(
        result.error_codes(),
        vec![
            ("one".to_string(), "NoSuchVersion".to_string()),
            ("one".to_string(), "NoSuchVersion".to_string())
        ],
        "each identifier naming no stored version of a key that still holds versions is a \
         failure of its own, got {:?}",
        result.error_codes()
    );
    let emptied = version_listing_of(&list_object_versions(&router, name, "").await);
    assert!(
        emptied.entries().is_empty(),
        "the bucket holds no version afterwards, got {:?}",
        emptied.entries()
    );
}

#[tokio::test]
async fn a_batch_delete_outside_what_it_can_serve_is_refused_not_guessed() {
    let router = router();
    let name = "batch-delete-refusals";
    bucket(&router, name).await;
    write_keys(&router, name, &["one"]).await;

    // A payload that names no Object at all, and one past the published ceiling of 1,000,
    // are both outside the schema S3 publishes for `Delete`: each is REFUSED with 400, not
    // answered as a success that deleted nothing, and each carries `MalformedXML` — the
    // code S3 answers a document that does not validate against its published schema.
    // The empty one is refused by the boundary validator before the handler runs
    // (`Delete$Objects` is a required member) and the over-long one by the handler, so one
    // code for one logical failure is what says the two cannot drift apart.
    let too_many: Vec<String> = (0..1_001).map(|index| format!("key-{index}")).collect();
    let too_many: Vec<Identifier> = too_many.iter().map(|key| (key.as_str(), None)).collect();
    for (case, identifiers, code) in [
        ("no Object element at all", Vec::new(), "MalformedXML"),
        (
            "one more than the 1,000-key ceiling",
            too_many,
            "MalformedXML",
        ),
    ] {
        let answer = delete_objects(&router, name, &identifiers, false).await;
        assert_eq!(
            answer.status,
            400,
            "case {case:?}: expected HTTP 400, got {} with body {:?}",
            answer.status,
            text(&answer)
        );
        assert_eq!(
            answer.error_code(),
            code,
            "case {case:?}: expected {code}, got {:?}",
            answer.error_code()
        );
    }

    // A ceiling-sized payload IS served, so the refusal above is the bound and not a
    // smaller one of this emulator's own.
    let at_the_ceiling: Vec<String> = (0..1_000).map(|index| format!("key-{index}")).collect();
    let at_the_ceiling: Vec<Identifier> = at_the_ceiling
        .iter()
        .map(|key| (key.as_str(), None))
        .collect();
    let answer = delete_objects(&router, name, &at_the_ceiling, true).await;
    assert_eq!(
        answer.status,
        200,
        "case \"exactly the 1,000-key ceiling\": expected HTTP 200, got {} with body {:?}",
        answer.status,
        text(&answer)
    );

    // The whole request is addressed to one bucket, so a bucket this emulator does not
    // hold refuses it entirely rather than reporting a per-key error.
    let absent = delete_objects(
        &router,
        "bucket-that-was-never-created",
        &[("one", None)],
        false,
    )
    .await;
    assert_eq!(
        absent.status,
        404,
        "case \"a bucket that does not exist\": expected HTTP 404, got {} with body {:?}",
        absent.status,
        text(&absent)
    );
    assert_eq!(
        absent.error_code(),
        "NoSuchBucket",
        "case \"a bucket that does not exist\": expected NoSuchBucket, got {:?}",
        absent.error_code()
    );

    // An MFA token is something this emulator cannot judge — it holds no authentication
    // device — so a request that carries one keeps the scaffold's 501 instead of being
    // answered as a success that ignored it.
    let with_mfa = send(
        &router,
        "POST",
        &format!("/{name}?delete"),
        &[("x-amz-mfa", "arn:aws:iam::123456789012:mfa/device 123456")],
        &delete_payload(&[("one", None)], false),
    )
    .await;
    assert_eq!(
        with_mfa.status,
        501,
        "case \"an MFA token\": expected HTTP 501, got {} with body {:?}",
        with_mfa.status,
        text(&with_mfa)
    );

    // Neither refusal changed anything.
    assert_eq!(
        listing_of(&list_objects_v2(&router, name, "").await).keys(),
        vec!["one".to_string()],
        "a refused batch delete keeps every object"
    );
}

/// The identifiers of a payload that names keys and no version ids.
fn identifiers<'a>(keys: &'a [&'a str]) -> Vec<Identifier<'a>> {
    keys.iter().map(|key| (*key, None)).collect()
}

// ---------------------------------------------------------------------------------------
// Which SIDE of the result a version-pinned identifier that resolves to no version lands on.
// ---------------------------------------------------------------------------------------

#[tokio::test]
async fn a_version_pinned_identifier_is_confirmed_only_where_the_key_holds_no_version() {
    // RULE: an identifier that pins a `VersionId` the bucket holds no version of is reported
    // by what the bucket holds of its KEY — the version chain such an identifier addresses.
    // While the key still holds versions, S3 "indicates that the version ID specified in the
    // request does not match an existing version" (`NoSuchVersion`), and the versions it
    // holds are left alone. Once the bucket holds no version of the key at all the key is not
    // there, so the delete is confirmed — "if the object specified in the request isn't
    // found, Amazon S3 confirms the deletion by returning the result as deleted" — which is
    // what makes a repeated delete of one version idempotent.
    //
    // The rows differ along WHAT the bucket holds of the named key and along the form of the
    // token: a key whose only version this run's own delete already took away (the repeated
    // delete), a key nothing was ever written at, a key holding a live version, a key holding
    // a version behind a delete marker, and a key holding nothing but a delete marker. The
    // token varies with them: one of the published shape that nothing minted, a real-S3-shaped
    // id of another width and alphabet, a token that is no version id at all, and the id this
    // run really minted and deleted (a per-run value, so it is taken from the response rather
    // than written out).
    #[derive(Debug)]
    enum Holding {
        /// The key's only version is deleted by a batch of this test before the row runs.
        VersionDeletedAgain,
        /// Nothing was ever written at the key.
        NeverWritten,
        /// The key holds the version this run wrote.
        LiveVersion,
        /// The key holds its version behind a delete marker a simple delete laid.
        VersionBehindMarker,
        /// The key holds nothing but a delete marker: the version behind it is removed.
        MarkerOnly,
    }

    // (case, what the key holds, the token the identifier pins — `None` for the id this run
    // minted for that key, whether the entry must be confirmed).
    let rows: [(&str, Holding, Option<String>, bool); 7] = [
        (
            "the same version deleted twice",
            Holding::VersionDeletedAgain,
            None,
            true,
        ),
        (
            "a key nothing was written at, with a shaped token",
            Holding::NeverWritten,
            Some("e3b0c44298fc1c149afbf4c8996fb924".to_string()),
            true,
        ),
        (
            "a key nothing was written at, with a real-S3-shaped id",
            Holding::NeverWritten,
            // The published example of an issued version id, which is what a client holding
            // one against the real service hands back.
            Some("3HL4kqtJlcpXroDTDmJ_RGrmHJ1PyFCY".to_string()),
            true,
        ),
        (
            "a key nothing was written at, with a token that is no version id",
            Holding::NeverWritten,
            Some("not-a-version".to_string()),
            true,
        ),
        (
            "a live version beside a token nothing minted",
            Holding::LiveVersion,
            Some("f".repeat(VERSION_ID_CHARS)),
            false,
        ),
        (
            "a version behind a delete marker",
            Holding::VersionBehindMarker,
            Some("not-a-version".to_string()),
            false,
        ),
        (
            "nothing but a delete marker",
            Holding::MarkerOnly,
            Some("e3b0c44298fc1c149afbf4c8996fb924".to_string()),
            false,
        ),
    ];

    for (case, holding, token, confirmed) in rows {
        // A fresh store per row, so one row's deletes cannot decide another's.
        let router = router();
        let name = "batch-delete-absent-version";
        bucket(&router, name).await;
        set_versioning(&router, name, "Enabled").await;
        let key = "svc-file1.txt";
        let minted = write_keys(&router, name, &[key]).await[key].clone();
        assert!(
            !minted.is_empty(),
            "case {case:?}: the setup write must mint a version to name, got {minted:?}"
        );

        // Bring the key to what this row holds of it.
        match holding {
            Holding::VersionDeletedAgain => {
                let first = delete_objects(&router, name, &[(key, Some(&minted))], false).await;
                let result = delete_result_of(&first);
                assert_eq!(
                    (first.status, result.deleted_keys(), result.error_codes()),
                    (200, vec![key.to_string()], Vec::new()),
                    "case {case:?}: the first delete of the version must be reported deleted, \
                     got {} with body {:?}",
                    first.status,
                    text(&first)
                );
            }
            Holding::NeverWritten => {
                // The version the setup wrote goes away with the key it was written at, so
                // the row's key really holds nothing.
                let drained = delete_objects(&router, name, &[(key, Some(&minted))], false).await;
                assert_eq!(
                    drained.status,
                    200,
                    "case {case:?}: the setup version must be removed, got {} with body {:?}",
                    drained.status,
                    text(&drained)
                );
            }
            Holding::LiveVersion => {}
            Holding::VersionBehindMarker => {
                let marked = delete_objects(&router, name, &[(key, None)], false).await;
                assert_eq!(
                    marked.status,
                    200,
                    "case {case:?}: the simple delete must lay a marker, got {} with body {:?}",
                    marked.status,
                    text(&marked)
                );
            }
            Holding::MarkerOnly => {
                let marked = delete_objects(&router, name, &[(key, None)], false).await;
                let marker_id = delete_result_of(&marked)
                    .deleted_entry(key)
                    .and_then(|entry| entry.get("DeleteMarkerVersionId").cloned())
                    .unwrap_or_else(|| {
                        panic!(
                            "case {case:?}: the simple delete must report the marker it laid, \
                             got body {:?}",
                            text(&marked)
                        )
                    });
                let purged = delete_objects(&router, name, &[(key, Some(&minted))], false).await;
                assert_eq!(
                    purged.status,
                    200,
                    "case {case:?}: the version behind the marker must be removed, got {} with \
                     body {:?}",
                    purged.status,
                    text(&purged)
                );
                assert!(
                    !marker_id.is_empty(),
                    "case {case:?}: the marker must carry an id of its own"
                );
            }
        }

        // What the key holds now, which is what the rule is keyed on.
        let held_before =
            version_listing_of(&list_object_versions(&router, name, "").await).entries();

        let pinned = token.unwrap_or_else(|| minted.clone());
        let answer = delete_objects(&router, name, &[(key, Some(&pinned))], false).await;
        assert_eq!(
            answer.status,
            200,
            "case {case:?}: the batch itself answers 200, got {} with body {:?}",
            answer.status,
            text(&answer)
        );
        let result = delete_result_of(&answer);
        let reported: Vec<(String, String)> = result
            .deleted
            .iter()
            .map(|entry| {
                (
                    entry.get("Key").cloned().unwrap_or_default(),
                    entry.get("VersionId").cloned().unwrap_or_default(),
                )
            })
            .collect();
        let errored: Vec<(String, String, String, String)> = result
            .errors
            .iter()
            .map(|entry| {
                let field = |name: &str| entry.get(name).cloned().unwrap_or_default();
                (
                    field("Key"),
                    field("VersionId"),
                    field("Code"),
                    field("Message"),
                )
            })
            .collect();
        match confirmed {
            true => assert_eq!(
                (reported.clone(), errored.clone()),
                (vec![(key.to_string(), pinned.clone())], Vec::new()),
                "case {case:?}: a key the bucket holds no version of confirms the delete, \
                 echoing the pair it named, got {reported:?}/{errored:?} in body {:?}",
                text(&answer)
            ),
            false => assert_eq!(
                (reported.clone(), errored.clone()),
                (
                    Vec::new(),
                    vec![(
                        key.to_string(),
                        pinned.clone(),
                        "NoSuchVersion".to_string(),
                        "The specified version does not exist.".to_string()
                    )]
                ),
                "case {case:?}: a key the bucket holds versions of reports the unresolved \
                 version as that key's own failure, got {reported:?}/{errored:?} in body {:?}",
                text(&answer)
            ),
        }

        // Either way the identifier took nothing away: what the bucket held before the call
        // is what it holds after it.
        let held_after =
            version_listing_of(&list_object_versions(&router, name, "").await).entries();
        assert_eq!(
            held_after, held_before,
            "case {case:?}: an identifier that resolved to no version must leave every stored \
             version where it was, got {held_after:?}"
        );
    }
}

#[tokio::test]
async fn a_mixed_batch_reports_each_identifier_on_its_own_side() {
    // The two sides of ONE response, over identifiers of DIFFERENT keys: a live version is
    // deleted and confirmed, an unresolvable token on a key that still holds a version is
    // that key's own `NoSuchVersion` failure, and an unresolvable token on a key the bucket
    // holds no version of is confirmed. Every identifier is reported exactly once and the
    // two lists are disjoint.
    let router = router();
    let name = "batch-delete-mixed";
    bucket(&router, name).await;
    set_versioning(&router, name, "Enabled").await;
    let doomed = write_keys(&router, name, &["svc-file1.txt"]).await["svc-file1.txt"].clone();
    let kept = write_keys(&router, name, &["svc-file2.txt"]).await["svc-file2.txt"].clone();
    // A real-S3-shaped id, which is what a client's stale bookkeeping hands back: longer than
    // this store mints and drawn from another alphabet.
    let stale = "3sL4kqtJlcpXroDTDmJ_rmSpXd3dIbrHY_MTRCxf3vjVBH40Nr8X8gdRQBpUMLUo";
    assert_ne!(stale, kept, "the stale token must name no stored version");

    let answer = delete_objects(
        &router,
        name,
        &[
            ("svc-file1.txt", Some(&doomed)),
            ("svc-file2.txt", Some(stale)),
            ("svc-file3.txt", Some(stale)),
        ],
        false,
    )
    .await;
    assert_eq!(
        answer.status,
        200,
        "the mixed batch answers 200, got {} with body {:?}",
        answer.status,
        text(&answer)
    );
    let result = delete_result_of(&answer);
    let reported: Vec<(String, String)> = result
        .deleted
        .iter()
        .map(|entry| {
            (
                entry.get("Key").cloned().unwrap_or_default(),
                entry.get("VersionId").cloned().unwrap_or_default(),
            )
        })
        .collect();
    assert_eq!(
        reported,
        vec![
            ("svc-file1.txt".to_string(), doomed.clone()),
            ("svc-file3.txt".to_string(), stale.to_string()),
        ],
        "case \"a mixed batch\": the live version and the identifier of a key holding nothing \
         are confirmed, got {reported:?} in body {:?}",
        text(&answer)
    );
    assert_eq!(
        result.error_codes(),
        vec![("svc-file2.txt".to_string(), "NoSuchVersion".to_string())],
        "case \"a mixed batch\": only the key that still holds versions reports a failure, got \
         {:?} in body {:?}",
        result.error_codes(),
        text(&answer)
    );

    // The refused identifier took nothing away: the version it could not name is still there.
    let read = get_object(
        &router,
        name,
        "svc-file2.txt",
        &format!("?versionId={kept}"),
        &[],
    )
    .await;
    assert_eq!(
        read.status,
        200,
        "case \"a mixed batch\": the version the stale token missed must still be readable, got \
         {} with body {:?}",
        read.status,
        text(&read)
    );
}

#[tokio::test]
async fn a_versioned_batch_delete_is_served_whatever_its_quiet_element_spells() {
    // `Delete$Quiet` turns a MODE on — "element to enable quiet mode for the request. When you
    // add this element, you must set its value to `true`" — so what its text decides is the
    // response mode and never whether the document validates: a batch naming a version the
    // bucket holds is carried out and reported per identifier in every row.
    //
    // (case, the text the `Quiet` element carries — `None` for no element at all —, whether the
    // mode is on). The rows differ along the text the element carries: each of the two words,
    // each of the two numeric spellings the published lexical space admits, surrounding
    // whitespace, the word in another case and in upper case (the case is not part of what the
    // text states), a number outside the space, a word that is no boolean
    // in any reading, no text at all, and no element at all.
    let rows: &[(&str, Option<&str>, bool)] = &[
        ("the word true", Some("true"), true),
        ("the word false", Some("false"), false),
        ("the numeric spelling of true", Some("1"), true),
        ("the numeric spelling of false", Some("0"), false),
        ("a word with surrounding whitespace", Some(" true "), true),
        ("a word in another case", Some("True"), true),
        ("a word in upper case", Some("TRUE"), true),
        ("a number outside the space", Some("2"), false),
        ("a word that is no boolean at all", Some("yes"), false),
        ("no text at all", Some(""), false),
        ("no Quiet element at all", None, false),
    ];
    for (case, spelled, quiet) in rows {
        // A fresh store per row, so one row's delete cannot decide another's.
        let router = router();
        let name = "batch-delete-quiet-versioned";
        bucket(&router, name).await;
        set_versioning(&router, name, "Enabled").await;
        let key = "svc-file1.txt";
        let minted = write_keys(&router, name, &[key]).await;
        let version_id = minted[key].clone();
        assert!(
            !version_id.is_empty(),
            "case {case:?}: the setup write must mint a version to name, got {minted:?}"
        );

        let answer = send(
            &router,
            "POST",
            &format!("/{name}?delete"),
            &[],
            &delete_payload_quiet_spelled(&[(key, Some(&version_id))], *spelled),
        )
        .await;
        assert_eq!(
            answer.status,
            200,
            "case {case:?}: the batch names one resolvable version, so it is carried out and \
             reported per identifier, got {} with body {:?}",
            answer.status,
            text(&answer)
        );
        let result = delete_result_of(&answer);
        assert!(
            result.errors.is_empty(),
            "case {case:?}: a resolvable version is no per-identifier failure, got {:?}",
            result.error_codes()
        );
        // "In quiet mode the response includes only keys where the delete operation
        // encountered an error"; verbose mode reports the identifier the request named, which
        // for a version-scoped delete is the key and that version id.
        match quiet {
            true => assert!(
                result.deleted.is_empty(),
                "case {case:?}: quiet mode reports no success, got {:?}",
                result.deleted_keys()
            ),
            false => {
                let entry = result.deleted_entry(key).unwrap_or_else(|| {
                    panic!(
                        "case {case:?}: the key must be reported deleted, got {:?}",
                        result.deleted_keys()
                    )
                });
                assert_eq!(
                    (
                        entry.get("VersionId").cloned().unwrap_or_default(),
                        entry.get("DeleteMarker").cloned()
                    ),
                    (version_id.clone(), None),
                    "case {case:?}: the entry must echo the version the identifier named and \
                     report no marker, got {entry:?}"
                );
            }
        }

        // And whichever mode the answer was in, the version is gone.
        let listing = version_listing_of(&list_object_versions(&router, name, "").await);
        assert!(
            listing.entries().is_empty(),
            "case {case:?}: the version {version_id:?} must be gone, the listing still reports \
             {:?}",
            listing.entries()
        );
    }
}

/// How the `VersionId` an identifier pins, which names no version of the key it is pinned
/// on, is built for a row of
/// [`an_unresolvable_version_pin_is_one_error_per_entry_whatever_the_batch_around_it`].
#[derive(Debug)]
enum Unresolvable {
    /// A version id this run really minted — for ANOTHER key of the same bucket. The nearest
    /// miss a client's own bookkeeping produces, and a token this store itself issued, so
    /// nothing but the PAIRING makes it name no version.
    OtherKeysVersion,
    /// A token of the shape this store mints that it never issued.
    Shaped,
}

#[tokio::test]
async fn an_unresolvable_version_pin_is_one_error_per_entry_whatever_the_batch_around_it() {
    // RULE: a `VersionId` resolves against the versions stored for the KEY it is pinned on, so
    // an identifier whose pair names no stored version is that key's own `Errors` entry —
    // `NoSuchVersion` "indicates that the version ID specified in the request does not match an
    // existing version", with the key and the version id the request sent echoed back — once
    // per identifier the payload carries, never in `Deleted`, and the versions the bucket holds
    // are left where they are. The response mode decides only whether the SUCCESSES are
    // reported: "in quiet mode the response includes only keys where the delete operation
    // encountered an error".
    //
    // The rows differ along what makes the pair unresolvable (a real version id of another key
    // of the same bucket, a token of the minted shape nothing issued), along the bucket's
    // versioning state (`Enabled`, `Suspended`, never set at all) and along the response mode —
    // and each row rides in ONE batch beside a resolvable version of another key and a second
    // copy of the same unresolvable identifier, so the accounting over the whole payload is
    // graded with the classification.
    //
    // (case, the bucket's versioning status — `None` for a bucket whose state is never set —,
    // how the unresolvable token is built, quiet).
    let rows: [(&str, Option<&str>, Unresolvable, bool); 5] = [
        (
            "an id minted for another key of the same bucket",
            Some("Enabled"),
            Unresolvable::OtherKeysVersion,
            false,
        ),
        (
            "an id minted for another key, reported in quiet mode",
            Some("Enabled"),
            Unresolvable::OtherKeysVersion,
            true,
        ),
        (
            "a token of the minted shape nothing issued",
            Some("Enabled"),
            Unresolvable::Shaped,
            false,
        ),
        (
            "a token nothing issued, on a bucket whose versioning is suspended",
            Some("Suspended"),
            Unresolvable::Shaped,
            false,
        ),
        (
            "a token nothing issued, on a bucket whose versioning was never set",
            None,
            Unresolvable::Shaped,
            false,
        ),
    ];
    for (case, status, form, quiet) in rows {
        // A fresh store per row, so one row's deletes cannot decide another's.
        let router = router();
        let name = "batch-delete-pinned-pairs";
        bucket(&router, name).await;
        if let Some(status) = status {
            set_versioning(&router, name, status).await;
        }
        // The key the unresolvable identifier is pinned on, the key whose own version the same
        // batch really removes, and the key whose minted id one row borrows.
        let pinned = "pinned.bin";
        let doomed = "doomed.bin";
        let lender = "lender.bin";
        let minted = write_keys(&router, name, &[pinned, doomed, lender]).await;
        // A bucket that carries no versioning state mints no ids at all, and the version it
        // holds is the `null` version every reader names it by.
        let version_of = |key: &str| {
            let id = minted[key].clone();
            if id.is_empty() {
                "null".to_string()
            } else {
                id
            }
        };
        let doomed_id = version_of(doomed);
        let bogus = match form {
            Unresolvable::OtherKeysVersion => version_of(lender),
            // The width this store mints, which is also the width the rows above pin.
            Unresolvable::Shaped => "c".repeat(VERSION_ID_CHARS),
        };
        assert_ne!(
            bogus,
            version_of(pinned),
            "case {case:?}: the token must name no version of the key it is pinned on"
        );

        // One payload: the unresolvable identifier, a resolvable one of another key, and the
        // unresolvable identifier a second time.
        let identifiers: Vec<Identifier> = vec![
            (pinned, Some(bogus.as_str())),
            (doomed, Some(doomed_id.as_str())),
            (pinned, Some(bogus.as_str())),
        ];
        let answer = delete_objects(&router, name, &identifiers, quiet).await;
        assert_eq!(
            answer.status,
            200,
            "case {case:?}: the batch itself answers 200, got {} with body {:?}",
            answer.status,
            text(&answer)
        );
        let result = delete_result_of(&answer);
        let deleted_pairs: Vec<(String, String)> = result
            .deleted
            .iter()
            .map(|entry| {
                (
                    entry.get("Key").cloned().unwrap_or_default(),
                    entry.get("VersionId").cloned().unwrap_or_default(),
                )
            })
            .collect();
        let errored: Vec<(String, String, String, String)> = result
            .errors
            .iter()
            .map(|entry| {
                let field = |name: &str| entry.get(name).cloned().unwrap_or_default();
                (
                    field("Key"),
                    field("VersionId"),
                    field("Code"),
                    field("Message"),
                )
            })
            .collect();

        // The unresolvable pair is on the error side once per identifier that named it, under
        // the absent-version code and that code's own fixed sentence, with the pair echoed.
        let refused = (
            pinned.to_string(),
            bogus.clone(),
            "NoSuchVersion".to_string(),
            "The specified version does not exist.".to_string(),
        );
        assert_eq!(
            errored,
            vec![refused.clone(), refused],
            "case {case:?}: each identifier naming no stored version of its key is reported \
             once on the error side, got {errored:?} in body {:?}",
            text(&answer)
        );
        // And nowhere on the success side, whatever the response mode: quiet leaves the
        // successes out, verbose reports the one version the batch really removed.
        assert_eq!(
            deleted_pairs,
            if quiet {
                Vec::new()
            } else {
                vec![(doomed.to_string(), doomed_id.clone())]
            },
            "case {case:?}: only the resolvable version is reported deleted, and only in \
             verbose mode, got {deleted_pairs:?} in body {:?}",
            text(&answer)
        );
        // Every identifier the payload carried is accounted for exactly once in verbose mode,
        // across the two lists together, and the two lists are disjoint.
        if !quiet {
            assert_eq!(
                deleted_pairs.len() + errored.len(),
                identifiers.len(),
                "case {case:?}: every identifier is reported exactly once, got \
                 {deleted_pairs:?}/{errored:?}"
            );
            let overlap: Vec<&(String, String)> = deleted_pairs
                .iter()
                .filter(|pair| errored.iter().any(|(key, id, _, _)| (key, id) == (&pair.0, &pair.1)))
                .collect();
            assert!(
                overlap.is_empty(),
                "case {case:?}: the two lists must be disjoint, got {overlap:?}"
            );
        }

        // The refused identifier took nothing away: the key it was pinned on still answers a
        // read, as does the key whose id one row borrowed — while the version the batch really
        // named is gone from the listing.
        for still_there in [pinned, lender] {
            let read = get_object(&router, name, still_there, "", &[]).await;
            assert_eq!(
                (read.status, text(&read)),
                (200, format!("body of {still_there}")),
                "case {case:?}: {still_there:?} must still be readable, got {} with body {:?}",
                read.status,
                text(&read)
            );
        }
        let held: Vec<(String, String)> =
            version_listing_of(&list_object_versions(&router, name, "").await).entries();
        assert!(
            !held.contains(&(doomed.to_string(), doomed_id.clone())),
            "case {case:?}: the version the batch removed must be gone, the listing still \
             reports {held:?}"
        );
        assert!(
            held.iter().any(|(key, _)| key == pinned),
            "case {case:?}: the key the refused identifier named must keep its version, the \
             listing reports {held:?}"
        );
    }
}

// ---------------------------------------------------------------------------------------
// The RESOLVABLE side of the partition: WHICH stored version a version-pinned identifier
// reaches, and the accounting over the whole payload around it.
// ---------------------------------------------------------------------------------------

/// What kind of STORED version the resolvable identifier of a mixed batch pins.
///
/// The rows of [`a_mixed_batch_deletes_every_version_its_identifiers_resolve`] span the kinds a
/// bucket holds at all: a key's chain is "the current version followed by the noncurrent ones,
/// delete markers among them", and a version is minted ("all objects added to the bucket
/// receive a unique version ID") or is the `null` version ([`NULL_VERSION_ID`]) — "all objects
/// added to the bucket receive the version ID null" while the status is `Suspended`, and
/// "objects stored in your bucket before you set the versioning state have a version ID of null"
/// (`PutBucketVersioning`'s own documentation).
#[derive(Debug)]
enum Stored {
    /// The key's current object version, minted by a write into a versioning-enabled bucket.
    CurrentVersion,
    /// A noncurrent object version, kept behind a newer write of the same key.
    NoncurrentVersion,
    /// The delete marker a simple delete of the key laid.
    Marker,
    /// The `null` version of a write made while the bucket's versioning is `Suspended`.
    NullAfterSuspension,
    /// The `null` version of a write made before the bucket carried a versioning state at all.
    NullFromBeforeVersioning,
}

/// Write `key` and hand back the version id a request names that write's version by: the id the
/// write minted, or [`NULL_VERSION_ID`] — the id a request names the version of a write made
/// while the bucket was not versioning-enabled by — when the bucket minted none.
async fn written_version(router: &MultiServiceRouter, name: &str, key: &str) -> String {
    let minted = write_keys(router, name, &[key]).await[key].clone();
    if minted.is_empty() {
        NULL_VERSION_ID.to_string()
    } else {
        minted
    }
}

#[tokio::test]
async fn a_mixed_batch_deletes_every_version_its_identifiers_resolve() {
    // RULE: `DeleteObjects` is evaluated per identifier, and each one performs exactly what a
    // `DeleteObject` for its pair would: an identifier pinning a `VersionId` that names a
    // version the bucket HOLDS of that key removes exactly that version and is reported under
    // `Deleted`, echoing the `Key` and `VersionId` it named — with `DeleteMarker` true and
    // `DeleteMarkerVersionId` naming it when the version removed was itself a delete marker
    // ("indicates whether the specified object version that was permanently deleted was (true)
    // or was not (false) a delete marker before deletion", `DeletedObject$DeleteMarker`) —
    // while only an identifier whose `VersionId` resolves to NO version of a key the bucket
    // still holds versions of is an `Errors` entry, under `NoSuchVersion` and that code's own
    // fixed sentence. The two lists are disjoint and together report every requested pair
    // exactly once, and nothing but the versions the resolvable identifiers named leaves the
    // store.
    //
    // The rows differ along the KIND of stored version the resolvable identifier pins (every
    // kind a bucket holds: a current version, a noncurrent one behind a newer write, a delete
    // marker, the `null` version of a suspended bucket and the `null` version of a write made
    // before versioning was ever set), along the FORM of the token the unresolvable identifier
    // carries (one of the shape this store mints that nothing issued, the published example of
    // a real S3 version id, a token that is no version id at all) and along the ORDER the two
    // ride in the payload.
    let target = "svc-file1.txt";
    let partner = "svc-file2.txt";

    // (case, the kind the resolvable identifier pins, the unresolvable token, whether the
    // resolvable identifier comes first).
    let rows: Vec<(&str, Stored, String, bool)> = vec![
        (
            "the current version, resolvable identifier first",
            Stored::CurrentVersion,
            "f".repeat(VERSION_ID_CHARS),
            true,
        ),
        (
            "a noncurrent version behind a newer write, resolvable identifier last",
            Stored::NoncurrentVersion,
            // The published example of an issued version id, which is the shape a client's
            // stale bookkeeping hands back: another width and another alphabet.
            "3HL4kqtJlcpXroDTDmJ_RGrmHJ1PyFCY".to_string(),
            false,
        ),
        (
            "the delete marker a simple delete laid",
            Stored::Marker,
            "not-a-version".to_string(),
            true,
        ),
        (
            "the null version of a suspended bucket, resolvable identifier last",
            Stored::NullAfterSuspension,
            "e3b0c44298fc1c149afbf4c8996fb924".to_string(),
            false,
        ),
        (
            "the null version of a write made before versioning was set",
            Stored::NullFromBeforeVersioning,
            "f".repeat(VERSION_ID_CHARS),
            true,
        ),
    ];

    for (case, stored, stale, resolvable_first) in &rows {
        // A fresh store per row, so one row's deletes cannot decide another's.
        let router = router();
        let name = "batch-delete-partition";
        bucket(&router, name).await;

        // Each row brings the bucket to the state its kind needs, and reports the version the
        // resolvable identifier pins, whether removing it removes a delete marker, and every
        // version the bucket must still hold afterwards. The identifier that must FAIL is
        // pinned on a key holding a version of its own, which is the one state in which an
        // unresolvable token is that key's `NoSuchVersion` rather than a confirmed delete.
        let (pinned, removes_marker, mut survivors): (String, bool, Vec<(String, String)>) =
            match stored {
                Stored::CurrentVersion => {
                    set_versioning(&router, name, "Enabled").await;
                    let kept = written_version(&router, name, partner).await;
                    let pinned = written_version(&router, name, target).await;
                    (pinned, false, vec![(partner.to_string(), kept)])
                }
                Stored::NoncurrentVersion => {
                    set_versioning(&router, name, "Enabled").await;
                    let kept = written_version(&router, name, partner).await;
                    let older = written_version(&router, name, target).await;
                    let newer = written_version(&router, name, target).await;
                    assert_ne!(
                        older, newer,
                        "case {case:?}: two writes of one key must mint two version ids"
                    );
                    (
                        older,
                        false,
                        vec![
                            (partner.to_string(), kept),
                            (target.to_string(), newer),
                        ],
                    )
                }
                Stored::Marker => {
                    set_versioning(&router, name, "Enabled").await;
                    let kept = written_version(&router, name, partner).await;
                    let behind = written_version(&router, name, target).await;
                    let marked = delete_objects(&router, name, &[(target, None)], false).await;
                    let marker = delete_result_of(&marked)
                        .deleted_entry(target)
                        .and_then(|entry| entry.get("DeleteMarkerVersionId").cloned())
                        .unwrap_or_else(|| {
                            panic!(
                                "case {case:?}: the simple delete must report the marker it \
                                 laid, got body {:?}",
                                text(&marked)
                            )
                        });
                    (
                        marker,
                        true,
                        vec![
                            (partner.to_string(), kept),
                            (target.to_string(), behind),
                        ],
                    )
                }
                Stored::NullAfterSuspension => {
                    set_versioning(&router, name, "Enabled").await;
                    let kept = written_version(&router, name, partner).await;
                    set_versioning(&router, name, "Suspended").await;
                    let pinned = written_version(&router, name, target).await;
                    assert_eq!(
                        pinned, NULL_VERSION_ID,
                        "case {case:?}: a write into a suspended bucket is the null version, \
                         got {pinned:?}"
                    );
                    (pinned, false, vec![(partner.to_string(), kept)])
                }
                Stored::NullFromBeforeVersioning => {
                    // Written while the bucket carries no versioning state at all, so the
                    // version it stores has no id of its own; enabling versioning afterwards
                    // keeps it, under the id a request names it by.
                    write_keys(&router, name, &[target]).await;
                    set_versioning(&router, name, "Enabled").await;
                    let kept = written_version(&router, name, partner).await;
                    (
                        NULL_VERSION_ID.to_string(),
                        false,
                        vec![(partner.to_string(), kept)],
                    )
                }
            };
        assert!(
            !survivors.iter().any(|(_, id)| id == stale),
            "case {case:?}: the unresolvable token must name no stored version, the bucket \
             holds {survivors:?}"
        );

        let requested: Vec<Identifier> = if *resolvable_first {
            vec![
                (target, Some(pinned.as_str())),
                (partner, Some(stale.as_str())),
            ]
        } else {
            vec![
                (partner, Some(stale.as_str())),
                (target, Some(pinned.as_str())),
            ]
        };
        let answer = delete_objects(&router, name, &requested, false).await;
        assert_eq!(
            answer.status,
            200,
            "case {case:?}: the mixed batch answers 200, got {} with body {:?}",
            answer.status,
            text(&answer)
        );

        let result = delete_result_of(&answer);
        let confirmed: Vec<(String, String)> = result
            .deleted
            .iter()
            .map(|entry| {
                (
                    entry.get("Key").cloned().unwrap_or_default(),
                    entry.get("VersionId").cloned().unwrap_or_default(),
                )
            })
            .collect();
        assert_eq!(
            confirmed,
            vec![(target.to_string(), pinned.clone())],
            "case {case:?}: the identifier whose version the bucket holds is reported deleted \
             with its own pair echoed, got {confirmed:?} in body {:?}",
            text(&answer)
        );
        let errored: Vec<(String, String, String, String)> = result
            .errors
            .iter()
            .map(|entry| {
                let field = |name: &str| entry.get(name).cloned().unwrap_or_default();
                (
                    field("Key"),
                    field("VersionId"),
                    field("Code"),
                    field("Message"),
                )
            })
            .collect();
        assert_eq!(
            errored,
            vec![(
                partner.to_string(),
                stale.clone(),
                "NoSuchVersion".to_string(),
                "The specified version does not exist.".to_string()
            )],
            "case {case:?}: only the identifier that resolved to no version fails, under the \
             absent-version code and its fixed sentence, got {errored:?} in body {:?}",
            text(&answer)
        );

        // A version that WAS a delete marker says so, and names the marker the identifier took
        // away; every other kind reports neither member.
        let entry = result
            .deleted_entry(target)
            .unwrap_or_else(|| panic!("case {case:?}: the pair must be reported deleted"));
        let marker_members = (
            entry.get("DeleteMarker").map(String::as_str),
            entry.get("DeleteMarkerVersionId").map(String::as_str),
        );
        assert_eq!(
            marker_members,
            if removes_marker {
                (Some("true"), Some(pinned.as_str()))
            } else {
                (None, None)
            },
            "case {case:?}: the delete-marker members of the entry must report what the \
             identifier removed, got {entry:?}"
        );

        // Every requested pair is reported exactly once over the two lists together, so the
        // partition covers the payload and the lists are disjoint.
        let mut reported: Vec<(String, String)> = confirmed.clone();
        reported.extend(
            errored
                .iter()
                .map(|(key, version_id, _, _)| (key.clone(), version_id.clone())),
        );
        reported.sort();
        let mut asked: Vec<(String, String)> = requested
            .iter()
            .map(|(key, version_id)| {
                ((*key).to_string(), version_id.unwrap_or_default().to_string())
            })
            .collect();
        asked.sort();
        assert_eq!(
            reported, asked,
            "case {case:?}: every requested pair is reported exactly once across the two \
             lists, got {reported:?} in body {:?}",
            text(&answer)
        );

        // What the store holds afterwards: the version the resolvable identifier named is
        // gone, and every other version the bucket held is still where it was.
        let mut held: Vec<(String, String)> =
            version_listing_of(&list_object_versions(&router, name, "").await).entries();
        held.sort();
        survivors.sort();
        assert_eq!(
            held, survivors,
            "case {case:?}: only the version the resolvable identifier named may leave the \
             store, got {held:?}"
        );
        // And a reader agrees: the version is no longer addressable, while the version the
        // refused identifier was pinned beside still answers a read.
        let gone = get_object(&router, name, target, &format!("?versionId={pinned}"), &[]).await;
        assert_eq!(
            gone.status,
            404,
            "case {case:?}: the deleted version must no longer be readable, got {} with body \
             {:?}",
            gone.status,
            text(&gone)
        );
        let left = get_object(&router, name, partner, "", &[]).await;
        assert_eq!(
            (left.status, text(&left)),
            (200, format!("body of {partner}")),
            "case {case:?}: the key the refused identifier named must still be readable, got \
             {} with body {:?}",
            left.status,
            text(&left)
        );
    }
}
