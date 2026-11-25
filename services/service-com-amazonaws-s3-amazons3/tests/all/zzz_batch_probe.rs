// SPDX-License-Identifier: CC0-1.0

//! The PROBE a client makes with the TEXT of a request document's element, end to end — the
//! batch delete that carries a boolean and an object key, and the configuration write that
//! carries four booleans.
//!
//! Every assertion drives a real HTTP request through the router (the harness is the one
//! [`super::bucket_lifecycle`], [`super::object_and_tagging_roundtrip`] and
//! [`super::bucket_public_access_block`] already own), so what is pinned is the wire
//! behaviour — status, the result document, what the bucket reports afterwards — rather than
//! any codec's internals.
//!
//! The three rules under test, each asserted over a SET of spellings rather than one request:
//!
//! > A boolean element of a request document is read in the lexical space the schema S3
//! > publishes for it gives a boolean: `true`, `false`, `1` and `0`, with surrounding whitespace
//! > allowed — so a document spelling one the NUMERIC way states the same value as the word, and
//! > text in neither spelling states no boolean at all. Where the element states a VALUE the
//! > service keeps beyond whether it is on, such text is refused.
//!
//! > A boolean element whose published rule is that ADDING it with the value `true` turns a
//! > SWITCH on states only whether that switch is on: text that states no `true` — which is read
//! > in ANY case, the spelling such a member's own documentation uses — leaves it off, the
//! > reading the element's own absence has, and never makes the document one that does not
//! > validate, so the request is carried out and answered whatever that element spells. The
//! > quiet mode of a batch delete is one such switch, and so is each of the four settings of a
//! > Block Public Access configuration.
//!
//! > The text of an element that carries an OBJECT KEY is the key itself, whitespace and all:
//! > the key a batch delete names is the key it takes away, and the key a tag set states is
//! > the key it stores, whether that key is composed of whitespace, begins or ends with it, or
//! > carries it inside.
//!
//! Their sources: a request document is validated against the published schema, where a boolean
//! element is an `xsd:boolean`, whose lexical space is exactly `{true, false, 1, 0}` (W3C XML
//! Schema Part 2: Datatypes, §3.2.2.1) — the "0" and "1" being the numeric spellings of the
//! other two, and `service_com_amazonaws_s3_amazons3::state::DOCUMENT_BOOLEAN_SPELLINGS` the one
//! place this service states that space; `Delete$Quiet` is "element to enable quiet mode for the
//! request. When you add this element, you must set its value to `true`"
//! (`smithy_query(query="doc",shape="Delete",member="Quiet")`), switching the two modes
//! `DeleteObjects` documents ("by default, the operation uses verbose mode in which the response
//! includes the result of deletion of each key in your request. In quiet mode the response
//! includes only keys where the delete operation encountered an error"); and `DeleteObjects`
//! names a whitespace key in its own documentation of
//! `Delete$Objects` ("an object that's composed entirely of whitespace characters is not
//! supported by the `DeleteObjects` API operation" for a DIRECTORY bucket, which is the bucket
//! kind that does not support it). The documents driven here are the two that carry a
//! boolean — `Delete$Quiet` on `DeleteObjects`, which turns the response's quiet mode on, and
//! the four `PublicAccessBlockConfiguration` settings on `PutPublicAccessBlock`, each of which
//! is published as "setting this element to TRUE causes the following behavior"
//! (`smithy_query(query="shape",shape="PublicAccessBlockConfiguration")`) — each read back
//! through the operation that reports it.

use std::collections::BTreeSet;

use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_s3_amazons3::service::wire::{
    XML_BOOLEAN_DOCUMENT_ELEMENTS, XML_MODE_SWITCH_DOCUMENT_ELEMENTS,
    XML_OBJECT_KEY_DOCUMENT_ELEMENTS,
};
use service_com_amazonaws_s3_amazons3::state::{
    document_boolean, document_switch_is_on, DOCUMENT_BOOLEAN_SPELLINGS,
};

use super::bucket_lifecycle::{router, send, Answer};
use super::bucket_public_access_block::{get_public_access_block, reported_flags};
use super::bucket_tagging::tag_set_of;
use super::object_and_tagging_roundtrip::{bucket, put_object, text};
use super::object_storage::{list_objects_v2, listing_of};
use super::object_tagging::{get_object_tagging, put_object_tagging};

/// The XML namespace botocore puts on an S3 request payload.
const XMLNS: &str = "http://s3.amazonaws.com/doc/2006-03-01/";

/// How a case spells the `Quiet` element of its `Delete` document: the element absent, an empty
/// `<Quiet/>`, or the element carrying exactly this text.
#[derive(Debug, Clone, Copy)]
enum Spelled {
    Absent,
    SelfClosing,
    Text(&'static str),
}

impl Spelled {
    /// The element as the document carries it.
    fn element(self) -> String {
        match self {
            Self::Absent => String::new(),
            Self::SelfClosing => "<Quiet/>".to_string(),
            Self::Text(text) => format!("<Quiet>{text}</Quiet>"),
        }
    }
}

/// `POST /<bucket>?delete` with one identifier and the `Quiet` element spelled exactly as
/// `quiet` gives it.
async fn delete_objects(
    router: &MultiServiceRouter,
    name: &str,
    key: &str,
    quiet: Spelled,
) -> Answer {
    let payload = format!(
        "<Delete xmlns=\"{XMLNS}\"><Object><Key>{key}</Key></Object>{}</Delete>",
        quiet.element()
    );
    send(router, "POST", &format!("/{name}?delete"), &[], &payload).await
}

/// `PUT /<bucket>?publicAccessBlock` with every setting spelled exactly as `spelling` gives
/// it.
async fn put_public_access_block_spelled(
    router: &MultiServiceRouter,
    name: &str,
    spelling: &str,
) -> Answer {
    let mut payload = format!("<PublicAccessBlockConfiguration xmlns=\"{XMLNS}\">");
    for setting in SETTINGS {
        payload.push_str(&format!("<{setting}>{spelling}</{setting}>"));
    }
    payload.push_str("</PublicAccessBlockConfiguration>");
    send(
        router,
        "PUT",
        &format!("/{name}?publicAccessBlock"),
        &[],
        &payload,
    )
    .await
}

/// Every member `PublicAccessBlockConfiguration` publishes, in the model's own order
/// (`smithy_query(query="shape",shape="PublicAccessBlockConfiguration")`), each targeting the
/// boolean `Setting`.
const SETTINGS: [&str; 4] = [
    "BlockPublicAcls",
    "IgnorePublicAcls",
    "BlockPublicPolicy",
    "RestrictPublicBuckets",
];

/// One row of the spelling table: the case, the text the element carries, the boolean the
/// published lexical space reads it as — `None` for text in neither spelling, which states no
/// boolean at all — and whether a SWITCH that text was written for is left ON.
type SpellingRow = (&'static str, &'static str, Option<bool>, bool);

/// Every spelling the published lexical space admits, beside text outside it.
///
/// The rows differ along what the rules generalise over: which of the two words, which of the
/// two numeric spellings, and the surrounding whitespace the space allows — plus, for the rows
/// that state no boolean in that space, the word in another case and in upper case (which a
/// SWITCH still reads as true, the case not being part of what the text states), a number
/// outside the space, text that is no boolean in any reading and no text at all. A row stating
/// no true leaves a switch off — the reading the element's own absence has — and a row in
/// neither spelling of the space is refused where the element states a value the service keeps
/// beyond whether it is on.
///
/// Every row is also checked against the one place this service states that space and that
/// switch reading ([`the_boolean_spellings_are_the_space_the_service_reads`]), so the table
/// cannot drift from either.
const SPELLINGS: [SpellingRow; 11] = [
    ("the word true", "true", Some(true), true),
    ("the word false", "false", Some(false), false),
    ("the numeric spelling of true", "1", Some(true), true),
    ("the numeric spelling of false", "0", Some(false), false),
    ("a word with surrounding whitespace", " true ", Some(true), true),
    (
        "a numeric spelling with surrounding whitespace",
        " 0 ",
        Some(false),
        false,
    ),
    ("the word true in another case", "True", None, true),
    ("the word true in upper case", "TRUE", None, true),
    ("a number outside the space", "2", None, false),
    ("a word that is not one of the two", "yes", None, false),
    ("no text at all", "", None, false),
];

/// This service's Smithy model's shapes, parsed.
///
/// The one reading of the model in this suite, so the two catalogs below are derived from the
/// same source.
fn model_shapes() -> serde_json::Map<String, serde_json::Value> {
    let model_path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/smithy-model/com-amazonaws-s3-amazons3.json"
    );
    let model: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(model_path).expect("the service's Smithy model must be readable"),
    )
    .expect("the Smithy model must parse as JSON");
    model["shapes"]
        .as_object()
        .expect("the model must carry a shapes object")
        .clone()
}

/// The shapes every request DOCUMENT of this service is rooted at: the structured
/// `@httpPayload` members of the operation inputs. A member bound to a header, a query
/// parameter or a URI label is not in a document at all, and a scalar payload IS the body
/// rather than a document.
fn request_document_shapes(shapes: &serde_json::Map<String, serde_json::Value>) -> Vec<String> {
    let mut rooted: Vec<String> = Vec::new();
    for (_, shape) in shapes.iter().filter(|(_, shape)| shape["type"] == "operation") {
        let Some(input) = shape["input"]["target"].as_str() else {
            continue;
        };
        let Some(members) = shapes.get(input).and_then(|shape| shape["members"].as_object()) else {
            continue;
        };
        for (_, member) in members {
            if member["traits"].get("smithy.api#httpPayload").is_none() {
                continue;
            }
            let Some(target) = member["target"].as_str() else {
                continue;
            };
            let kind = shapes
                .get(target)
                .and_then(|shape| shape["type"].as_str())
                .unwrap_or_default();
            if matches!(kind, "structure" | "union" | "list" | "map") {
                rooted.push(target.to_string());
            }
        }
    }
    rooted
}

/// Every element a request document carries a member of `kind` in — walked from the shapes
/// those documents are rooted at and following each member's target, under the element name
/// restXml writes each with (its `@xmlName`, else the member's own name).
///
/// `kind` is read as the member's TARGET: its shape `type` for [`element_type`] and its shape
/// id for a named shape, which is what tells an object key from any other string.
fn document_elements(by_target: &dyn Fn(&str, &serde_json::Value) -> bool) -> BTreeSet<String> {
    let shapes = model_shapes();
    let mut pending: Vec<String> = request_document_shapes(&shapes);
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut found: BTreeSet<String> = BTreeSet::new();
    while let Some(shape_id) = pending.pop() {
        if !seen.insert(shape_id.clone()) {
            continue;
        }
        let Some(shape) = shapes.get(&shape_id) else {
            continue;
        };
        // A list or a map carries its element shape rather than members.
        for target in [
            shape["member"]["target"].as_str(),
            shape["value"]["target"].as_str(),
        ]
        .into_iter()
        .flatten()
        {
            pending.push(target.to_string());
        }
        let Some(members) = shape["members"].as_object() else {
            continue;
        };
        for (name, member) in members {
            let Some(target) = member["target"].as_str() else {
                continue;
            };
            let targeted = shapes.get(target).cloned().unwrap_or_default();
            if by_target(target, &targeted) {
                found.insert(
                    member["traits"]["smithy.api#xmlName"]
                        .as_str()
                        .unwrap_or(name)
                        .to_string(),
                );
                continue;
            }
            pending.push(target.to_string());
        }
    }
    found
}

#[test]
fn the_boolean_spellings_are_the_space_the_service_reads() {
    // Every row of the table states what the one place this service states that space reads
    // the same text as, and what a SWITCH written with that text is left in, so the
    // end-to-end rows above drive those two readings rather than second copies of them.
    for (case, spelled, states, on) in SPELLINGS {
        assert_eq!(
            document_boolean(spelled),
            states,
            "case {case:?}: the published space must read {spelled:?} as {states:?}, got {:?}",
            document_boolean(spelled)
        );
        assert_eq!(
            document_switch_is_on(spelled),
            on,
            "case {case:?}: a switch written {spelled:?} must be {}, got {}",
            match on {
                true => "on",
                false => "off",
            },
            document_switch_is_on(spelled)
        );
    }

    // And every spelling that space admits is a row of the table, so each one is driven
    // end-to-end rather than only declared.
    let driven: BTreeSet<&str> = SPELLINGS
        .iter()
        .map(|(_, spelled, _, _)| spelled.trim())
        .collect();
    for (spelling, states) in DOCUMENT_BOOLEAN_SPELLINGS {
        assert!(
            driven.contains(spelling),
            "case {spelling:?}: the spelling states {states} and must be driven end to end, \
             the table drives {driven:?}"
        );
    }
}

#[test]
fn the_boolean_element_catalog_matches_the_model() {    // Every element a request document of this service binds a BOOLEAN member from, read from
    // the model itself rather than from a second hand-written list.
    let derived = document_elements(&|_, targeted| targeted["type"] == "boolean");
    let declared: BTreeSet<String> = XML_BOOLEAN_DOCUMENT_ELEMENTS
        .iter()
        .map(|element| (*element).to_string())
        .collect();
    assert_eq!(
        declared, derived,
        "XML_BOOLEAN_DOCUMENT_ELEMENTS must name exactly the elements a request document of \
         this service binds a boolean from; declared {declared:?}, the model binds {derived:?}"
    );
}

#[test]
fn the_object_key_element_catalog_matches_the_model() {
    // And every element such a document carries an OBJECT KEY in: a member whose target is
    // the `ObjectKey` shape, which is the shape every member carrying a key targets.
    let derived = document_elements(&|target, _| target == OBJECT_KEY_SHAPE);
    let declared: BTreeSet<String> = XML_OBJECT_KEY_DOCUMENT_ELEMENTS
        .iter()
        .map(|element| (*element).to_string())
        .collect();
    assert_eq!(
        declared, derived,
        "XML_OBJECT_KEY_DOCUMENT_ELEMENTS must name exactly the elements a request document of \
         this service carries an object key in; declared {declared:?}, the model carries \
         {derived:?}"
    );
}

/// The model shape an object KEY is: the one every member carrying a key targets.
const OBJECT_KEY_SHAPE: &str = "com.amazonaws.s3#ObjectKey";

#[tokio::test]
async fn a_batch_delete_addresses_the_key_its_document_spells_whitespace_and_all() {
    let router = router();
    let name = "batch-delete-keys";
    bucket(&router, name).await;

    // (case, the key, the path it is written at). The rows differ along WHERE the whitespace
    // of a key falls — a key that is nothing but whitespace, one that begins or ends with it,
    // one that carries it inside, and one with none at all — because that is what the
    // trimming a codec does to an element's text generalises over. The whitespace-only key is
    // the one `DeleteObjects` names in its own documentation ("an object that's composed
    // entirely of whitespace characters"), and every one of them is a key S3 stores.
    let rows: [(&str, &str, &str); 6] = [
        ("one space", " ", "%20"),
        ("two spaces", "  ", "%20%20"),
        ("a tab", "\t", "%09"),
        ("a leading space", " leading.csv", "%20leading.csv"),
        ("a trailing space", "trailing.csv ", "trailing.csv%20"),
        ("whitespace inside", "two words.csv", "two%20words.csv"),
    ];
    for (case, key, path) in rows {
        let written = put_object(&router, name, path, b"a,b\n", &[]).await;
        assert_eq!(
            written.status,
            200,
            "case {case:?}: the setup write of {key:?} must be accepted, got {} with body {:?}",
            written.status,
            text(&written)
        );
    }
    let listed = listing_of(&list_objects_v2(&router, name, "").await).keys();
    for (case, key, _) in rows {
        assert!(
            listed.contains(&key.to_string()),
            "case {case:?}: the bucket must hold the key {key:?}, got {listed:?}"
        );
    }

    // Each key is then deleted by a batch that spells it in the document verbatim: the entry
    // the result reports names that key, and the key — and only that key — is gone.
    let mut left: Vec<String> = rows.iter().map(|(_, key, _)| (*key).to_string()).collect();
    for (case, key, _) in rows {
        let payload =
            format!("<Delete xmlns=\"{XMLNS}\"><Object><Key>{key}</Key></Object></Delete>");
        let answer = send(&router, "POST", &format!("/{name}?delete"), &[], &payload).await;
        assert_eq!(
            answer.status,
            200,
            "case {case:?}: a batch naming {key:?} is served, got {} with body {:?}",
            answer.status,
            text(&answer)
        );
        let body = String::from_utf8_lossy(&answer.body).into_owned();
        assert!(
            body.contains(&format!("<Deleted><Key>{key}</Key></Deleted>")),
            "case {case:?}: the result must report the key as the document spelled it, got \
             {body:?}"
        );

        left.retain(|held| held != key);
        let mut expected = left.clone();
        expected.sort();
        let after = listing_of(&list_objects_v2(&router, name, "").await).keys();
        assert_eq!(
            after, expected,
            "case {case:?}: the delete must take {key:?} alone away, got {after:?}"
        );
    }
}

#[tokio::test]
async fn a_batch_delete_is_served_whatever_its_quiet_element_spells() {
    // The element turns a MODE on, so what its text decides is the mode and never whether the
    // request validates: every row is served, and the mode is on only where the text states
    // `true`, in any case. The rows differ along the TEXT (each of the two
    // words, each of the two numeric spellings, surrounding whitespace, the word in another
    // case and in upper case, a number outside the space, a word that is no boolean at all, no
    // text at all) and along the FORM the element takes (carrying text, self-closing, absent
    // altogether).
    let rows = SPELLINGS
        .iter()
        .map(|(case, spelled, _, on)| (*case, Spelled::Text(spelled), *on))
        .chain([
            ("a self-closing element", Spelled::SelfClosing, false),
            ("no Quiet element at all", Spelled::Absent, false),
        ]);
    for (case, spelled, quiet) in rows {
        // A fresh store per row, so one row's delete cannot decide another's.
        let router = router();
        let name = "batch-delete-quiet";
        bucket(&router, name).await;
        let key = "data/one.csv";
        let written = put_object(&router, name, key, b"a,b\n", &[]).await;
        assert_eq!(
            written.status,
            200,
            "case {case:?}: the setup write must be accepted, got {} with body {:?}",
            written.status,
            text(&written)
        );

        let answer = delete_objects(&router, name, key, spelled).await;
        assert_eq!(
            answer.status,
            200,
            "case {case:?}: the batch names one key, so it is carried out and reported per key, \
             got {} with body {:?}",
            answer.status,
            text(&answer)
        );
        // "In quiet mode the response includes only keys where the delete operation
        // encountered an error" (this operation's own documentation), so the mode the element
        // puts the request in is what decides whether the deleted key is reported.
        let body = String::from_utf8_lossy(&answer.body).into_owned();
        assert_eq!(
            body.contains(&format!("<Deleted><Key>{key}</Key></Deleted>")),
            !quiet,
            "case {case:?}: expected quiet mode {quiet}, so the result must {} report the \
             deleted key, got {body:?}",
            match quiet {
                true => "not",
                false => "",
            }
        );

        // And whichever mode the answer was in, the delete itself happened: the bucket holds
        // the key no longer.
        let left = listing_of(&list_objects_v2(&router, name, "").await).keys();
        assert!(
            left.is_empty(),
            "case {case:?}: the delete must take {key:?} away, the bucket still holds {left:?}"
        );
    }
}

#[test]
fn every_mode_switch_element_is_one_the_model_binds_a_boolean_from() {
    // The fold is keyed on the element name, so each element it may touch must be one a request
    // document really binds a BOOLEAN member from — which is the catalog
    // `the_boolean_element_catalog_matches_the_model` above derives from the model itself.
    let booleans: BTreeSet<&str> = XML_BOOLEAN_DOCUMENT_ELEMENTS.iter().copied().collect();
    for element in XML_MODE_SWITCH_DOCUMENT_ELEMENTS {
        assert!(
            booleans.contains(element),
            "case {element:?}: a mode-switch element must be one the model binds a boolean \
             from, which are {booleans:?}"
        );
    }
}

#[tokio::test]
async fn a_configuration_write_reads_every_setting_in_the_published_boolean_space() {
    for (case, spelled, _, on) in SPELLINGS {
        // A fresh store per row: every bucket is created with all four switches on, so a
        // row's own write is the only thing that can have changed them.
        let router = router();
        let name = "public-access-block-spelling";
        bucket(&router, name).await;

        let answer = put_public_access_block_spelled(&router, name, spelled).await;
        assert_eq!(
            answer.status,
            200,
            "case {case:?}: each of the four settings states only whether its switch is on, so \
             the document is served whatever its text spells, got {} with body {:?}",
            answer.status,
            text(&answer)
        );

        // And the state each setting is in afterwards is the one its text stated: on only
        // where that text states the boolean true, and off for text that states no true at
        // all — the reading the element's own absence has. Read back through the operation
        // that reports the stored configuration.
        let setting = on;
        let read = get_public_access_block(&router, name, &[]).await;
        assert_eq!(
            read.status,
            200,
            "case {case:?}: the stored configuration must read back, got {} with body {:?}",
            read.status,
            text(&read)
        );
        assert_eq!(
            reported_flags(&read),
            [Some(setting); 4],
            "case {case:?}: expected every setting to report {setting}, got {:?}",
            reported_flags(&read)
        );
    }
}

#[tokio::test]
async fn a_tag_set_keeps_the_whitespace_the_key_its_document_spells_carries() {
    // `Tag$Key` is the OTHER member the model targets at `ObjectKey` — that shape documents
    // it as "name of the object key" — and it travels in an element of the same name, so the
    // text of a tag key is kept exactly as the document spelled it too. Whitespace is a
    // character a tag key may carry ("letters, whitespace, and numbers representable in
    // UTF-8", the published tag restrictions), so a key stated with it is a key with it.
    let router = router();
    let name = "tag-key-whitespace";
    bucket(&router, name).await;
    let key = "data/one.csv";
    put_object(&router, name, key, b"a,b\n", &[]).await;

    // The rows differ along where the whitespace of a tag key falls, as the delete rows above
    // do for an object key.
    let rows: [(&str, &str); 4] = [
        ("a leading space", " leading"),
        ("a trailing space", "trailing "),
        ("whitespace inside", "two words"),
        ("no whitespace at all", "plain"),
    ];
    for (case, tag_key) in rows {
        let written = put_object_tagging(
            &router,
            name,
            key,
            &[(tag_key.to_string(), "value".to_string())],
        )
        .await;
        assert_eq!(
            written.status,
            200,
            "case {case:?}: the tag set must be stored, got {} with body {:?}",
            written.status,
            text(&written)
        );
        let read = get_object_tagging(&router, name, key, "").await;
        assert_eq!(
            tag_set_of(&read),
            vec![(tag_key.to_string(), "value".to_string())],
            "case {case:?}: the stored key must be the one the document spelled, got {:?}",
            tag_set_of(&read)
        );
    }
}
