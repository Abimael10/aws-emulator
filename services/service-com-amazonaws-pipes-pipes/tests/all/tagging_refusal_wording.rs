// SPDX-License-Identifier: CC0-1.0

//! How a refused TAG is WORDED on the wire, and which code carries it.
//!
//! The resource-tagging layer that fronts the ARN-addressed tagging operations refuses a
//! tag with its own error code (`BadRequestException`, which no operation of this model
//! declares), and the message it carries is not one shape for every rule. Three rules,
//! each tested here over a table rather than one request:
//!
//! * a value outside a bound the MODEL publishes (`TagKey`'s `@length(min: 1, max: 128)`,
//!   `TagValue`'s `@length(min: 0, max: 256)`) is refused with the request-validation
//!   sentence — the member the value arrived on, that member's value echoed, and both
//!   published bounds — and that sentence arrives as the JSON document this error class
//!   carries, the sentence under a `message` member, so a client that parses the message
//!   gets a document and reads the sentence out of it; nothing of the sentence is
//!   re-escaped, so the apostrophes it quotes a value and a member name with arrive as
//!   apostrophes;
//! * a rule the model expresses NOWHERE (the prefix AWS reserves for its own tags, the
//!   documented tag character set) has no validation sentence: the message is the fixed
//!   documented sentence itself, stated bare — no document around it — and it interpolates
//!   nothing from the request;
//! * the two bounds of one `@length` can be owned by different layers, and then they
//!   answer different codes: a key named for removal that is past `TagKey`'s MAXIMUM is the
//!   tagging layer's `BadRequestException` carrying that document, while one that breaks
//!   its MINIMUM is the `ValidationException` the operation models, naming the member alone
//!   — because a `@httpQuery` value that arrived empty never became a parameter at all.
//!
//! The other message of this service that is a JSON document is the `ValidationException` it
//! models for its own refusals (a `ValidationErrorFields` array), which
//! [`the_modelled_validation_exception_keeps_its_document`] holds in place.
//!
//! Which tags a write accepts is `tests/all/tag_key_rules.rs`; the merge semantics are
//! `tests/all/pipe_tagging.rs`; how an unresolvable `resourceArn` is reported is
//! `tests/all/tagging_refusals.rs`. Every assertion here goes through
//! `MultiServiceRouter::dispatch`, and every refused call is checked to have changed
//! nothing.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::pipe_lifecycle::{create, describe, minimal_body, router, Reply};
use crate::pipe_tagging::{
    assert_tags_agree, create_body, pipe_arn, tag, tag_map, untag, TAG_MUTATION_STATUS,
};
use crate::tag_key_rules::{
    elided, one_tag, shown, tags_of, validation_document, CHARACTER_SET_SENTENCE,
    RESERVED_PREFIX_SENTENCE,
};

/// `TagKey`'s published bounds and `TagValue`'s. Source: the `TagKey` and `TagValue`
/// shapes of `smithy-model/com-amazonaws-pipes-pipes.json`.
const MAX_TAG_KEY_LEN: usize = 128;
const MIN_TAG_KEY_LEN: usize = 1;
const MAX_TAG_VALUE_LEN: usize = 256;
const MIN_TAG_VALUE_LEN: usize = 0;

/// The code the tagging layer refuses a broken tag rule with, and the code the operations
/// model for the refusals the service itself states.
const TAG_RULE_CODE: &str = "BadRequestException";
const MODELLED_CODE: &str = "ValidationException";

/// The member each write's request validation reports a refused tag at: `TagResource`
/// names the tags it merges `addedTags`, `UntagResource` names the keys it removes
/// `removedTagKeys`, and `CreatePipe` names them as the model does. A map VALUE outside
/// its bound is found by the boundary validator, which places a violation by its path
/// alone and so knows the member only as the model names it.
const ADDED_TAGS: &str = "addedTags";
const REMOVED_TAG_KEYS: &str = "removedTagKeys";
const MODEL_TAGS: &str = "tags";

/// The member a refused `@httpQuery` value is named by: the model's member name as a
/// validation path spells it (`tagKeys` is `UntagResource`'s `@httpQuery("tagKeys")`),
/// which is not the name its request validation reports a tag's CONTENT at.
const REMOVED_TAG_KEYS_QUERY: &str = "tagKeys";

/// The most BYTES of a refused value a validation sentence echoes before eliding it.
const MAX_ECHOED_BYTES: usize = 256;

/// The one tag every fixture pipe below starts out carrying, so a refused call can be
/// shown to have removed nothing and added nothing.
fn stored() -> Value {
    json!({"keep": "value"})
}

/// Whether `message` is a JSON document rather than the prose sentence a refusal states.
/// A constraint-validation refusal's message IS one, carrying the sentence under its
/// `message` member ([`document`]) with the apostrophes it quotes with escaped, so a client
/// unescapes those before parsing it; an unmodelled rule's message is the bare sentence,
/// which is what [`an_unmodelled_tag_rule_is_refused_with_its_fixed_sentence`] pins.
fn is_a_json_document(message: &str) -> bool {
    serde_json::from_str::<Value>(&message.replace("\\'", "'"))
        .is_ok_and(|parsed| parsed.is_object())
}

/// The message a constraint-validation refusal carries for `sentence`: the document this
/// error class words such a refusal as, with the sentence under its `message` member.
fn document(sentence: &str) -> String {
    validation_document(sentence)
}

/// The sentence a refusal's message document carries, or the message itself when it is no
/// document — what a client that unescapes the document's quotes and parses it reads.
fn carried_sentence(message: &str) -> String {
    serde_json::from_str::<Value>(&message.replace("\\'", "'"))
        .ok()
        .and_then(|parsed| parsed["message"].as_str().map(str::to_string))
        .unwrap_or_else(|| message.to_string())
}

/// Which published `@length` a row breaks, and the constraint class request validation
/// states it as: a bound on a map's KEYS, on a map's VALUES, or on a list's MEMBERS.
#[derive(Debug, Clone, Copy)]
enum Bound {
    MapKey,
    MapValue,
    ListMember,
}

impl Bound {
    /// The validation sentence for this bound, at `member`, echoing `shown` as the value
    /// of that member. The bounds come from the model, the echo from the request.
    fn sentence(self, member: &str, shown: &str) -> String {
        let (class, max, min) = match self {
            Self::MapKey => ("Map keys must satisfy", MAX_TAG_KEY_LEN, MIN_TAG_KEY_LEN),
            Self::MapValue => (
                "Map value must satisfy",
                MAX_TAG_VALUE_LEN,
                MIN_TAG_VALUE_LEN,
            ),
            Self::ListMember => ("Member must satisfy", MAX_TAG_KEY_LEN, MIN_TAG_KEY_LEN),
        };
        format!(
            "1 validation error detected: Value '{shown}' at '{member}' failed to satisfy \
             constraint: {class} constraint: [Member must have length less than or equal \
             to {max}, Member must have length greater than or equal to {min}]"
        )
    }
}

/// Which call carries one row's tags.
#[derive(Debug, Clone)]
enum Write {
    /// `TagResource` merging a map into a pipe that already exists.
    Merge(Value),
    /// `CreatePipe` carrying the same map as `Tags`.
    Create(Value),
    /// `UntagResource` naming keys for removal.
    Remove(Vec<String>),
}

/// Issue one row's write. `Merge` and `Remove` address the pipe `name`, which the caller
/// has already created; `Create` is itself the call that would create it.
async fn issue(router: &MultiServiceRouter, name: &str, write: &Write) -> Reply {
    match write {
        Write::Merge(tags) => tag(router, &pipe_arn(name), tags).await,
        Write::Create(tags) => create(router, name, create_body(Some(tags))).await,
        Write::Remove(keys) => {
            let borrowed: Vec<&str> = keys.iter().map(String::as_str).collect();
            untag(router, &pipe_arn(name), &borrowed).await
        }
    }
}

/// The fixture one row needs: for every write but `Create`, a pipe carrying [`stored`].
async fn fixture(router: &MultiServiceRouter, name: &str, case: &str, write: &Write) {
    if matches!(write, Write::Create(_)) {
        return;
    }
    let created = create(router, name, create_body(Some(&stored()))).await;
    assert_eq!(
        created.status, 200,
        "case {case:?}: the fixture pipe must be created, got {created:?}"
    );
}

/// Assert that the row's refused call left the store exactly as it was.
async fn assert_nothing_changed(
    router: &MultiServiceRouter,
    name: &str,
    case: &str,
    write: &Write,
) {
    if matches!(write, Write::Create(_)) {
        let described = describe(router, name).await;
        assert_eq!(
            (described.status, described.error_code.as_str()),
            (404, "NotFoundException"),
            "case {case:?}: a refused CreatePipe must store no pipe, got {described:?}"
        );
        return;
    }
    assert_tags_agree(router, case, name, &tag_map(&stored())).await;
}

/// RULE: a tag value outside a bound the model publishes is refused with 400
/// `BadRequestException` whose message is the JSON document this error class carries — the
/// request-validation sentence (the member the value arrived on, that member's value, and
/// both published bounds) under the document's `message` member, with nothing of the
/// sentence re-escaped — and nothing is stored.
///
/// The rows span what the rule generalises over: which bound was broken (`TagKey`'s
/// maximum and its minimum, `TagValue`'s maximum), by how much (one character past, far
/// past), which collection carries it (a map's keys, a map's values, a list's members),
/// which write carries that collection (`TagResource`, `CreatePipe`, `UntagResource`), how
/// many tags travel beside the broken one, whether the value is ASCII or unicode (the
/// bound counts characters, the echo elides bytes), and whether the echoed value carries
/// characters a JSON string must escape — which the error body's serializer quotes, so the
/// sentence a client decodes is the same prose either way.
#[tokio::test]
async fn a_bound_outside_the_model_is_refused_with_the_validation_sentence() {
    let router = router();

    let past_key = "k".repeat(MAX_TAG_KEY_LEN + 1);
    let far_past_key = "k".repeat(MAX_TAG_KEY_LEN + 72);
    let past_value = "v".repeat(MAX_TAG_VALUE_LEN + 1);
    // 129 characters, 258 bytes: `@length` counts CHARACTERS, so this is past the bound —
    // and past the bytes a list-element echo may carry, so that echo states a prefix cut
    // on a character boundary and the full byte count.
    let unicode_past_key = "é".repeat(MAX_TAG_KEY_LEN + 1);
    let unicode_echo = format!(
        "{}... [{} bytes]",
        "é".repeat(MAX_ECHOED_BYTES / "é".len()),
        unicode_past_key.len(),
    );
    // 130 characters of exactly the two a JSON string cannot hold literally. The length
    // rule is read before the character-set one, so this key is refused for its length and
    // its characters reach the echo — where the error body's own serializer, and nothing in
    // the sentence, is what quotes them.
    let quoting_past_key = "\\\"".repeat(MAX_TAG_KEY_LEN / 2 + 1);

    // (the case, the write, the member the refusal names, the bound it broke, the value
    // that member is echoed as).
    let rows: Vec<(&str, Write, &str, Bound, String)> = vec![
        (
            "merge-a-key-one-character-past-the-maximum",
            Write::Merge(one_tag(&past_key, "irrelevant")),
            ADDED_TAGS,
            Bound::MapKey,
            shown(&[(&past_key, "irrelevant")]),
        ),
        (
            "merge-a-key-far-past-the-maximum",
            Write::Merge(one_tag(&far_past_key, "v")),
            ADDED_TAGS,
            Bound::MapKey,
            shown(&[(&far_past_key, "v")]),
        ),
        (
            "merge-an-empty-key-breaking-the-minimum",
            Write::Merge(one_tag("", "prod")),
            ADDED_TAGS,
            Bound::MapKey,
            shown(&[("", "prod")]),
        ),
        (
            // What the sentence echoes is the whole MEMBER, not only the entry that broke
            // the bound.
            "merge-a-broken-key-beside-two-legal-tags",
            Write::Merge(tags_of(vec![
                ("env".to_string(), "prod".to_string()),
                (past_key.clone(), "x".to_string()),
                ("team".to_string(), "core".to_string()),
            ])),
            ADDED_TAGS,
            Bound::MapKey,
            shown(&[("env", "prod"), (&past_key, "x"), ("team", "core")]),
        ),
        (
            "merge-a-unicode-key-past-the-maximum",
            Write::Merge(one_tag(&unicode_past_key, "prod")),
            ADDED_TAGS,
            Bound::MapKey,
            shown(&[(&unicode_past_key, "prod")]),
        ),
        (
            "merge-a-key-of-characters-the-error-body-must-quote",
            Write::Merge(one_tag(&quoting_past_key, "irrelevant")),
            ADDED_TAGS,
            Bound::MapKey,
            shown(&[(&quoting_past_key, "irrelevant")]),
        ),
        (
            "merge-a-value-past-its-maximum",
            Write::Merge(one_tag("env", &past_value)),
            MODEL_TAGS,
            Bound::MapValue,
            shown(&[("env", &elided(&past_value))]),
        ),
        (
            "create-a-key-past-the-maximum",
            Write::Create(one_tag(&past_key, "irrelevant")),
            MODEL_TAGS,
            Bound::MapKey,
            shown(&[(&past_key, "irrelevant")]),
        ),
        (
            "create-a-value-past-its-maximum",
            Write::Create(one_tag("env", &past_value)),
            MODEL_TAGS,
            Bound::MapValue,
            shown(&[("env", &elided(&past_value))]),
        ),
        (
            "remove-a-key-one-character-past-the-maximum",
            Write::Remove(vec![past_key.clone()]),
            REMOVED_TAG_KEYS,
            Bound::ListMember,
            format!("[{past_key}]"),
        ),
        (
            "remove-a-key-far-past-the-maximum",
            Write::Remove(vec![far_past_key.clone()]),
            REMOVED_TAG_KEYS,
            Bound::ListMember,
            format!("[{far_past_key}]"),
        ),
        (
            "remove-a-broken-key-beside-a-stored-one",
            Write::Remove(vec!["keep".to_string(), past_key.clone()]),
            REMOVED_TAG_KEYS,
            Bound::ListMember,
            format!("[{past_key}]"),
        ),
        (
            "remove-a-unicode-key-past-the-maximum",
            Write::Remove(vec![unicode_past_key.clone()]),
            REMOVED_TAG_KEYS,
            Bound::ListMember,
            format!("[{unicode_echo}]"),
        ),
    ];

    for (index, (case, write, member, bound, echoed)) in rows.into_iter().enumerate() {
        let name = format!("tagdoc{index}");
        fixture(&router, &name, case, &write).await;

        let reply = issue(&router, &name, &write).await;
        assert_eq!(
            (reply.status, reply.error_code.as_str()),
            (400, TAG_RULE_CODE),
            "case {case:?}: expected 400 {TAG_RULE_CODE}, got {reply:?}"
        );

        let sentence = bound.sentence(member, &echoed);
        // The document is the message, byte for byte...
        assert_eq!(
            reply.message(),
            document(&sentence),
            "case {case:?}: expected the validation sentence {sentence:?} inside the \
             message document, got {reply:?}"
        );
        // ...so a client that parses the message gets a document and reads the sentence out
        // of it, and the apostrophes the sentence quotes with are not escaped again.
        assert!(
            is_a_json_document(&reply.message()),
            "case {case:?}: the message must be the document that carries the sentence, got {:?}",
            reply.message()
        );
        assert_eq!(
            carried_sentence(&reply.message()),
            sentence,
            "case {case:?}: expected the document to carry {sentence:?}, got {:?}",
            reply.message()
        );
        assert!(
            !carried_sentence(&reply.message()).contains("\\'"),
            "case {case:?}: the sentence the document carries must quote with plain \
             apostrophes, got {:?}",
            reply.message()
        );

        assert_nothing_changed(&router, &name, case, &write).await;
    }
}

/// RULE, pinned ONCE with nothing shared with the code that builds it: the whole message a
/// refused tag carries, spelled out literally — the document, the count, the echoed map, the
/// member and both bounds, and not one character more — so an error in the wording cannot be
/// mirrored in the expectation that checks it. The key's length is this one row's example;
/// the table above is what spans the class.
#[tokio::test]
async fn the_bytes_of_one_refusal_are_spelled_out_whole() {
    let router = router();
    let name = "tagbytes";
    let created = create(&router, name, create_body(Some(&stored()))).await;
    assert_eq!(
        created.status, 200,
        "the fixture pipe must be created, got {created:?}"
    );

    let key = "k".repeat(MAX_TAG_KEY_LEN + 1);
    let reply = tag(&router, &pipe_arn(name), &one_tag(&key, "irrelevant")).await;
    assert_eq!(
        (reply.status, reply.error_code.as_str()),
        (400, TAG_RULE_CODE),
        "an over-long key must be refused with 400 {TAG_RULE_CODE}, got {reply:?}"
    );
    assert_eq!(
        reply.message(),
        format!(
            "{{\"message\": \"1 validation error detected: Value \\'{{{key}=irrelevant}}\\' \
             at \\'addedTags\\' failed to satisfy constraint: Map keys must satisfy \
             constraint: [Member must have length less than or equal to 128, Member must \
             have length greater than or equal to 1]\"}}"
        ),
        "the refusal's bytes must be the document carrying the validation sentence, got {reply:?}"
    );

    assert_tags_agree(&router, "nothing-stored", name, &tag_map(&stored())).await;
}

/// RULE: the message of EVERY constraint-validation refusal of the tagging surface parses as
/// the document this error class carries, and the sentence a client reads is the one under
/// its `message` member — whichever member the value arrived on and whichever collection's
/// bound it broke.
///
/// The rows span each validated tag member of the surface: the map a `TagResource` merges
/// (its keys), the map a `CreatePipe` carries (its values), and the key list an
/// `UntagResource` names (its members). What they add to the table above is that the message
/// is READ as a document — parsed, with the sentence taken out of it — rather than compared
/// to one built the same way the service builds it.
#[tokio::test]
async fn every_constraint_validated_tag_member_carries_the_sentence_in_a_document() {
    let router = router();

    let past_key = "k".repeat(MAX_TAG_KEY_LEN + 1);
    let past_value = "v".repeat(MAX_TAG_VALUE_LEN + 1);
    let key_bounds = format!(
        "[Member must have length less than or equal to {MAX_TAG_KEY_LEN}, \
         Member must have length greater than or equal to {MIN_TAG_KEY_LEN}]"
    );

    // (the case, the write, the member the carried sentence must name, the constraint class
    // it must state, the bracketed bounds it must list).
    let rows: Vec<(&str, Write, &str, &str, String)> = vec![
        (
            "merge-a-key-past-its-bound",
            Write::Merge(one_tag(&past_key, "irrelevant")),
            ADDED_TAGS,
            "Map keys must satisfy constraint:",
            key_bounds.clone(),
        ),
        (
            "create-with-a-value-past-its-bound",
            Write::Create(one_tag("env", &past_value)),
            MODEL_TAGS,
            "Map value must satisfy constraint:",
            format!(
                "[Member must have length less than or equal to {MAX_TAG_VALUE_LEN}, \
                 Member must have length greater than or equal to {MIN_TAG_VALUE_LEN}]"
            ),
        ),
        (
            "remove-a-key-past-its-bound",
            Write::Remove(vec![past_key.clone()]),
            REMOVED_TAG_KEYS,
            "Member must satisfy constraint:",
            key_bounds.clone(),
        ),
    ];

    for (index, (case, write, member, class, bounds)) in rows.into_iter().enumerate() {
        let name = format!("tagenvelope{index}");
        fixture(&router, &name, case, &write).await;

        let reply = issue(&router, &name, &write).await;
        assert_eq!(
            (reply.status, reply.error_code.as_str()),
            (400, TAG_RULE_CODE),
            "case {case:?}: expected 400 {TAG_RULE_CODE}, got {reply:?}"
        );
        let parsed: Value =
            serde_json::from_str(&reply.message().replace("\\'", "'")).unwrap_or(Value::Null);
        let carried = parsed["message"].as_str().unwrap_or_default().to_string();
        assert!(
            parsed.is_object() && !carried.is_empty(),
            "case {case:?}: the message must parse as the document that carries the \
             sentence, got {:?}",
            reply.message()
        );
        assert!(
            carried.starts_with("1 validation error detected: Value '"),
            "case {case:?}: expected the validation sentence, got {carried:?}"
        );
        assert!(
            carried.contains(&format!("' at '{member}' failed to satisfy constraint: {class} ")),
            "case {case:?}: expected the sentence to name {member:?} and state {class:?}, \
             got {carried:?}"
        );
        assert!(
            carried.ends_with(&bounds),
            "case {case:?}: expected the sentence to end in the bounds {bounds:?}, got \
             {carried:?}"
        );

        assert_nothing_changed(&router, &name, case, &write).await;
    }
}

/// RULE: a tag rule the model expresses nowhere — the reserved key prefix, the documented
/// character set — is refused with the FIXED documented sentence itself: no validation
/// envelope, no JSON document, and nothing of the request interpolated into it.
///
/// The rows span both unmodelled rules, the reserved prefix in every casing (it is matched
/// case-insensitively) and alone, a broken tag beside a legal one, and both writes that
/// install tags, so neither the casing nor the write nor the key changes the sentence.
#[tokio::test]
async fn an_unmodelled_tag_rule_is_refused_with_its_fixed_sentence() {
    let router = router();

    // (the case, the write, the fixed sentence, the text of the request that must NOT
    // appear in it).
    let rows: Vec<(&str, Write, &str, &str)> = vec![
        (
            "merge-the-reserved-prefix-lower-case",
            Write::Merge(one_tag("aws:cloudformation:stack-name", "demo")),
            RESERVED_PREFIX_SENTENCE,
            "cloudformation",
        ),
        (
            "merge-the-reserved-prefix-upper-case",
            Write::Merge(one_tag("AWS:createdBy", "demo")),
            RESERVED_PREFIX_SENTENCE,
            "createdBy",
        ),
        (
            "merge-the-reserved-prefix-mixed-case",
            Write::Merge(one_tag("Aws:Owner", "demo")),
            RESERVED_PREFIX_SENTENCE,
            "Owner",
        ),
        (
            "merge-the-reserved-prefix-alone",
            Write::Merge(one_tag("aws:", "demo")),
            RESERVED_PREFIX_SENTENCE,
            "demo",
        ),
        (
            "merge-the-reserved-prefix-beside-a-legal-tag",
            Write::Merge(tags_of(vec![
                ("env".to_string(), "prod".to_string()),
                ("aws:owner".to_string(), "demo".to_string()),
            ])),
            RESERVED_PREFIX_SENTENCE,
            "owner",
        ),
        (
            "create-the-reserved-prefix",
            Write::Create(one_tag("aws:owner", "demo")),
            RESERVED_PREFIX_SENTENCE,
            "owner",
        ),
        (
            "merge-a-key-outside-the-character-set",
            Write::Merge(one_tag("env#1", "prod")),
            CHARACTER_SET_SENTENCE,
            "env#1",
        ),
        (
            "merge-a-value-outside-the-character-set",
            Write::Merge(one_tag("sticker", "🚀")),
            CHARACTER_SET_SENTENCE,
            "sticker",
        ),
        (
            "create-a-value-outside-the-character-set",
            Write::Create(one_tag("sticker", "🚀")),
            CHARACTER_SET_SENTENCE,
            "sticker",
        ),
    ];

    for (index, (case, write, sentence, withheld)) in rows.into_iter().enumerate() {
        let name = format!("tagfixed{index}");
        fixture(&router, &name, case, &write).await;

        let reply = issue(&router, &name, &write).await;
        assert_eq!(
            (reply.status, reply.error_code.as_str()),
            (400, TAG_RULE_CODE),
            "case {case:?}: expected 400 {TAG_RULE_CODE}, got {reply:?}"
        );
        assert_eq!(
            reply.message(),
            sentence,
            "case {case:?}: expected the fixed documented sentence, got {reply:?}"
        );
        assert!(
            !is_a_json_document(&reply.message()),
            "case {case:?}: an unmodelled rule's sentence is not wrapped in a document, \
             got {:?}",
            reply.message()
        );
        assert!(
            !reply.message().contains(withheld),
            "case {case:?}: the sentence must echo nothing of the request, but it carries \
             {withheld:?}: {:?}",
            reply.message()
        );

        assert_nothing_changed(&router, &name, case, &write).await;
    }
}

/// One row of [`the_two_tag_key_bounds_are_answered_by_different_codes_on_removal`]: the
/// case, the keys the call names, and the error code and message it must be answered with
/// — `None` when every key is inside the bound and the call is accepted.
type RemovalRow = (&'static str, Vec<String>, Option<(&'static str, String)>);

/// RULE: the two bounds of `TagKey` are not answered by one layer when a key travels as a
/// `@httpQuery` list: a key past the MAXIMUM is the tagging layer's `BadRequestException`
/// carrying the validation sentence, while a key breaking the MINIMUM never arrived as a
/// parameter at all and is the `ValidationException` the operation models, naming the
/// member alone — so an SDK branching on the error code is told which it was.
///
/// The rows sit inside the bound, on it, and past it from both ends, with the offending key
/// alone and beside a key the pipe does carry.
#[tokio::test]
async fn the_two_tag_key_bounds_are_answered_by_different_codes_on_removal() {
    let router = router();
    let name = "tagkeybounds";
    let created = create(&router, name, create_body(Some(&stored()))).await;
    assert_eq!(
        created.status, 200,
        "the fixture pipe must be created, got {created:?}"
    );

    let at_maximum = "k".repeat(MAX_TAG_KEY_LEN);
    let past_maximum = "k".repeat(MAX_TAG_KEY_LEN + 1);
    let at_minimum = "k".repeat(MIN_TAG_KEY_LEN);

    // (the case, the keys the call names, what it must be answered with — `None` when the
    // keys are inside the bound and the call is accepted).
    let rows: Vec<RemovalRow> = vec![
        ("a-key-at-the-minimum", vec![at_minimum], None),
        ("a-key-at-the-maximum", vec![at_maximum], None),
        (
            "a-key-one-character-past-the-maximum",
            vec![past_maximum.clone()],
            Some((
                TAG_RULE_CODE,
                document(&Bound::ListMember.sentence(
                    REMOVED_TAG_KEYS,
                    &format!("[{past_maximum}]"),
                )),
            )),
        ),
        (
            "an-empty-key-breaking-the-minimum",
            vec![String::new()],
            Some((MODELLED_CODE, format!("[{REMOVED_TAG_KEYS_QUERY}]"))),
        ),
        (
            "an-empty-key-beside-a-stored-one",
            vec!["keep".to_string(), String::new()],
            Some((MODELLED_CODE, format!("[{REMOVED_TAG_KEYS_QUERY}]"))),
        ),
    ];

    for (case, keys, expected) in rows {
        let borrowed: Vec<&str> = keys.iter().map(String::as_str).collect();
        let reply = untag(&router, &pipe_arn(name), &borrowed).await;
        match &expected {
            None => assert_eq!(
                reply.status, TAG_MUTATION_STATUS,
                "case {case:?}: a key inside the bound must be accepted, got {reply:?}"
            ),
            Some((code, message)) => {
                assert_eq!(
                    (reply.status, reply.error_code.as_str()),
                    (400, *code),
                    "case {case:?}: expected 400 {code}, got {reply:?}"
                );
                assert_eq!(
                    reply.message(),
                    *message,
                    "case {case:?}: expected the message that code carries, got {reply:?}"
                );
            }
        }

        // Accepted or refused, no row names a key the pipe carries — except the row that
        // pairs one with an empty key, which removes nothing because the call is refused.
        assert_tags_agree(&router, case, name, &tag_map(&stored())).await;
    }
}

/// The source and target every fixture pipe of the test below is created with, neither of
/// which is what any of its rows gets wrong.
const SOURCE: &str = "arn:aws:sqs:us-west-2:000000000000:orders";
const TARGET: &str = "arn:aws:lambda:us-west-2:000000000000:function:handler";

/// RULE, the other side of the one above: the `ValidationException` this service MODELS for
/// its own refusals words its message as this service's `ValidationErrorFields`
/// document, which is a document of its own rather than the tagging layer's `message`
/// envelope, so a client that parses that document keeps getting one.
///
/// The rows differ along what the refusal is about (a filter pattern the service cannot
/// read, in two forms, and a pipe name outside its modelled pattern) and therefore along
/// which field the document names, so what they pin is the SHAPE of the modelled message
/// rather than one field's wording — which `tests/all/pipe_configuration_rules.rs` and
/// `tests/all/front_door_refusals.rs` pin in full.
#[tokio::test]
async fn the_modelled_validation_exception_keeps_its_document() {
    // (the case, the pipe name the call addresses, the `SourceParameters` it carries, the
    // field the document must name).
    let rows: Vec<(&str, &str, Option<Value>, &str)> = vec![
        (
            "a-pattern-that-is-not-json-at-all",
            "modelleddoc0",
            Some(json!({"FilterCriteria": {"Filters": [{"Pattern": "nope"}]}})),
            "Pattern",
        ),
        (
            "a-pattern-that-is-a-truncated-document",
            "modelleddoc1",
            Some(json!({"FilterCriteria": {"Filters": [{"Pattern": "{\"body\": "}]}})),
            "Pattern",
        ),
        (
            "a-pipe-name-outside-its-pattern",
            "bad@name",
            None,
            "PipeName",
        ),
    ];

    for (case, name, source_parameters, field) in rows {
        let router = router();
        let mut body = minimal_body(SOURCE, TARGET);
        if let Some(parameters) = source_parameters {
            body.as_object_mut()
                .expect("the CreatePipe body is an object")
                .insert("SourceParameters".to_string(), parameters);
        }

        let reply = create(&router, name, body).await;
        assert_eq!(
            (reply.status, reply.error_code.as_str()),
            (400, MODELLED_CODE),
            "case {case:?}: expected 400 {MODELLED_CODE}, got {reply:?}"
        );

        let message = reply.message();
        assert!(
            is_a_json_document(&message),
            "case {case:?}: the modelled exception's message stays a JSON document, \
             got {message:?}"
        );
        let reported: Value = serde_json::from_str(&message).unwrap_or(Value::Null);
        assert_eq!(
            reported
                .get("ValidationErrorFields")
                .and_then(Value::as_array)
                .and_then(|entries| entries.first())
                .and_then(|entry| entry.get("Name"))
                .and_then(Value::as_str),
            Some(field),
            "case {case:?}: the document must name the refused field {field}, \
             got {message:?}"
        );
    }
}
