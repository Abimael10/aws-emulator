// SPDX-License-Identifier: CC0-1.0

use super::{
    echo_of, is_elision_of, list_of_element, named_byte_count, quoted_echo, quoted_echo_up_to,
    echo_held_to, rendered_blob, rendered_declared_structure, rendered_entries, rendered_map,
    rendered_map_in_key_order, rendered_record, rendered_sequence, rendered_structure,
    unanchored, unanchored_grouping_alternation,
    unanchored_keeping_inner_anchors, unanchored_unless_published_as_declared, Composite,
    RenderedChild, Submitted, SubmittedEcho,
    SubmittedValues, MAX_RECORDED_BYTES, MAX_RECORDED_DEPTH, MAX_RECORDED_LEAVES,
    MAX_RECORDED_RENDERED_BYTES, MAX_RECORDED_RENDERINGS, MAX_RENDERED_CHILDREN,
    MAX_RESTORED_ECHO, MAX_RESTORED_TOTAL, UNSET_MEMBER,
};
use super::whole_value_expression;
use aws_smithy_schema_server::constraints::{Violation, ViolationKind};
use std::collections::HashMap;

/// RULE: a `@pattern` is quoted as the model's expression minus the leading `^` and the
/// trailing `$` the model writes around the service's own full-match expression — the
/// pair dropped positionally, and only when both ends carry one.
#[test]
fn a_pattern_loses_the_anchor_pair_the_model_wraps_it_in() {
    // (case, the modelled expression, the expression a clause quotes)
    let rows: [(&str, &str, &str); 11] = [
        ("a wrapped character class", r"^[a-z]+$", r"[a-z]+"),
        ("a wrapped group", r"^([\p{L}\p{N}_.:/=+\-@]*)$", r"([\p{L}\p{N}_.:/=+\-@]*)"),
        // The recorded case: the pair is dropped even though the expression
        // alternates at the top level (`NetworkConnectorName`, lambdacore).
        (
            "a wrapped top-level alternation",
            r"^(arn:aws:lambda:[a-z-]+-\d:\d{12}:network-connector:[a-z0-9-]+)|[a-zA-Z0-9_-]{1,64}$",
            r"(arn:aws:lambda:[a-z-]+-\d:\d{12}:network-connector:[a-z0-9-]+)|[a-zA-Z0-9_-]{1,64}",
        ),
        ("anchored at the start only", r"^arn:aws.*", r"^arn:aws.*"),
        ("anchored at the end only", r"[0-9]{12}$", r"[0-9]{12}$"),
        ("no anchor at all", r"\d{3}", r"\d{3}"),
        // Anchored inside its branches rather than at its ends: no outer pair.
        ("anchors inside the branches", r"(^a$)|(^b$)", r"(^a$)|(^b$)"),
        // The same two branches WITHOUT the parentheses carry the pair at the ends,
        // so the pair is what is dropped — the rule's faithful output even though the
        // result describes another language. See this function's doc comment.
        ("branches sharing the outer pair", r"^a$|^b$", r"a$|^b"),
        ("one branch anchored at each end", r"^a|b$", r"a|b"),
        // An escaped trailing `$` is a literal dollar, not the end anchor.
        ("an escaped trailing dollar", r"^costs\$", r"^costs\$"),
        // An escaped backslash before the `$` leaves the `$` an anchor.
        ("an escaped backslash then the anchor", r"^[a-z]\\$", r"[a-z]\\"),
    ];
    for (case, modelled, expected) in rows {
        let actual = unanchored(modelled);
        assert_eq!(
            actual, expected,
            "case {case:?}: expected {modelled:?} to be quoted as {expected:?}, got \
                 {actual:?}"
        );
    }
}

/// RULE: the collection is COMPLETE only while every string-rendered leaf was
/// recorded; any leaf one of the stated bounds drops marks it incomplete, which is what
/// stops [`super::submitted_lists`] from offering a collection with a hole in it.
#[test]
fn a_leaf_a_bound_dropped_marks_the_collection_incomplete() {
    // (case, the leaf values written in order, whether the walk stayed complete)
    let rows: [(&str, Vec<String>, bool); 5] = [
        ("nothing written at all", Vec::new(), true),
        (
            "two small leaves",
            vec!["first".to_string(), "second".to_string()],
            true,
        ),
        (
            "a leaf at the per-value ceiling",
            vec!["x".repeat(MAX_RESTORED_ECHO)],
            true,
        ),
        (
            "a leaf one byte past the per-value ceiling",
            vec!["x".repeat(MAX_RESTORED_ECHO + 1)],
            false,
        ),
        (
            "leaves that exhaust the byte budget",
            (0..=MAX_RECORDED_BYTES / MAX_RESTORED_ECHO)
                .map(|_| "y".repeat(MAX_RESTORED_ECHO))
                .collect(),
            false,
        ),
    ];

    for (case, values, expected) in rows {
        let mut walk = SubmittedValues::default();
        walk.path.push("member".to_string());
        for value in &values {
            walk.record(Some(value));
        }
        assert_eq!(
            !walk.skipped, expected,
            "case {case:?}: expected complete == {expected}, got {} (recorded {} leaves)",
            !walk.skipped,
            walk.leaves.len()
        );
    }
}

/// RULE: reaching the leaf cap also marks the collection incomplete — the walk stops
/// there, so a leaf beyond it is a leaf that was dropped — and the cap is counted over
/// every leaf VISITED, so a request whose values are all dropped for their size still
/// stops the walk.
#[test]
fn the_leaf_cap_marks_the_collection_incomplete() {
    let mut walk = SubmittedValues::default();
    walk.path.push("member".to_string());
    assert!(
        !walk.done() && !walk.skipped,
        "case an empty walk: expected it to be complete and not done"
    );
    for index in 0..MAX_RECORDED_LEAVES {
        walk.visited += 1;
        walk.record(Some(&index.to_string()));
    }
    assert_eq!(
        walk.leaves.len(),
        MAX_RECORDED_LEAVES,
        "case a full walk: expected the cap to be reached, got {} leaves",
        walk.leaves.len()
    );
    assert!(
        walk.done() && walk.skipped,
        "case a full walk: expected it to be done and incomplete"
    );

    // Nothing is recorded for a value past the echo bound, so a walk whose stop
    // condition counted RECORDED leaves would never stop on a request made of them.
    let mut dropping = SubmittedValues::default();
    dropping.path.push("member".to_string());
    let oversized = "x".repeat(MAX_RESTORED_ECHO + 1);
    for _ in 0..MAX_RECORDED_LEAVES {
        dropping.visited += 1;
        dropping.record(Some(&oversized));
    }
    assert!(
        dropping.leaves.is_empty(),
        "case a walk of oversized values: expected nothing recorded, got {} leaves",
        dropping.leaves.len()
    );
    assert!(
        dropping.done() && dropping.skipped,
        "case a walk of oversized values: expected it to be done and incomplete"
    );
}

/// RULE: the traversal's DEPTH is bounded by a stated constant just as its leaf count
/// is — a value nested past the ceiling is not descended into and marks the collection
/// incomplete — so a value built only of composites, which spends no leaf budget at
/// all, cannot set how deep the walk goes.
#[test]
fn the_depth_ceiling_bounds_a_value_built_only_of_composites() {
    use aws_smithy_schema::serde::{SerdeError, ShapeSerializer};
    use aws_smithy_schema::{Schema, ShapeId, ShapeType};

    static ELEMENT: Schema = Schema::new_member(
        ShapeId::from_static("test#Nested$member", "test", "member"),
        ShapeType::String,
        "member",
        0,
    );
    static LIST: Schema = Schema::new_member(
        ShapeId::from_static("test#Nested$values", "test", "values"),
        ShapeType::List,
        "values",
        0,
    );

    /// `depth` lists nested one inside the other, holding one string at the bottom.
    fn nest(
        serializer: &mut dyn ShapeSerializer,
        depth: usize,
    ) -> Result<(), SerdeError> {
        if depth == 0 {
            return serializer.write_string(&ELEMENT, "innermost");
        }
        let deeper = move |inner: &mut dyn ShapeSerializer| nest(inner, depth - 1);
        serializer.write_list(&LIST, &deeper)
    }

    // (case, how deep the value nests, whether the walk reaches the bottom of it)
    let rows: [(&str, usize, bool); 5] = [
        ("one list", 1, true),
        ("a handful of lists", 8, true),
        ("nested exactly to the ceiling", MAX_RECORDED_DEPTH, true),
        ("one list past the ceiling", MAX_RECORDED_DEPTH + 1, false),
        ("nested far past the ceiling", 16 * MAX_RECORDED_DEPTH, false),
    ];
    for (case, depth, reached) in rows {
        let mut walk = SubmittedValues::default();
        nest(&mut walk, depth).unwrap_or_else(|e| panic!("case {case:?}: the walk failed: {e}"));
        assert!(
            walk.collections.is_empty(),
            "case {case:?}: expected every frame closed, got {} open",
            walk.collections.len()
        );
        assert_eq!(
            !walk.leaves.is_empty(),
            reached,
            "case {case:?}: expected the innermost value recorded == {reached}, got {} leaves",
            walk.leaves.len()
        );
        assert_eq!(
            walk.skipped, !reached,
            "case {case:?}: expected incomplete == {}, got {}",
            !reached, walk.skipped
        );
    }
}

/// RULE: a clause quotes the value itself up to the ceiling one clause may carry, and
/// past that a character-boundary-safe prefix plus the marker naming the whole value's
/// byte count — so the quoting is bounded by a constant rather than by the request, and
/// what it renders is an elision of the value the walk itself would recognise.
#[test]
fn a_quoted_echo_is_the_value_itself_up_to_one_ceiling() {
    // One row: the case, the value, and whether the ceiling leaves it whole.
    let rows: [(&str, String, bool); 5] = [
        ("a short value", "bob".to_string(), true),
        (
            "a value at the ceiling",
            "x".repeat(MAX_RESTORED_ECHO),
            true,
        ),
        (
            "a value one byte over the ceiling",
            "x".repeat(MAX_RESTORED_ECHO + 1),
            false,
        ),
        (
            "a value far over the ceiling",
            "x".repeat(64 * MAX_RESTORED_ECHO),
            false,
        ),
        (
            // Two bytes per character: the prefix must not split one.
            "a multi-byte value over the ceiling",
            "é".repeat(MAX_RESTORED_ECHO),
            false,
        ),
    ];
    for (case, value, whole) in rows {
        let quoted = quoted_echo(&value);
        if whole {
            assert_eq!(
                quoted, value,
                "case {case:?}: expected the value itself, got {} bytes",
                quoted.len()
            );
        } else {
            assert!(
                quoted.len() <= MAX_RESTORED_ECHO + 32,
                "case {case:?}: expected a clause bounded by the ceiling, got {} bytes",
                quoted.len()
            );
            assert!(
                quoted.ends_with(&format!("... [{} bytes]", value.len())),
                "case {case:?}: expected the marker naming {} bytes, got the tail {:?}",
                value.len(),
                &quoted[quoted.len().saturating_sub(32)..]
            );
            assert!(
                is_elision_of(&quoted, &value),
                "case {case:?}: the walk must recognise the rendering as this value's \
                     elision"
            );
        }
    }
}

/// RULE: the same rendering holds at a ceiling the SERVICE states, in either direction:
/// a value within it is quoted whole and one past it keeps a character-boundary-safe
/// prefix plus the marker naming the whole value's byte count.
#[test]
fn a_stated_ceiling_quotes_the_value_itself_up_to_that_ceiling() {
    // Rows differ along the ceiling (far below the default, the default itself, and far
    // above it), the value's size relative to it, and its encoding.
    let rows: [(&str, usize, String, bool); 6] = [
        ("a short value under a small ceiling", 768, "bob".to_string(), true),
        ("a value at a small ceiling", 768, "x".repeat(768), true),
        (
            "a value one byte over a small ceiling",
            768,
            "x".repeat(769),
            false,
        ),
        (
            "a value the default would have quoted whole",
            768,
            "x".repeat(MAX_RESTORED_ECHO),
            false,
        ),
        (
            // Three bytes per character, and 768 is a multiple of three only by
            // accident of the repeat count: the prefix must not split a character.
            "a multi-byte value over a small ceiling",
            1000,
            "€".repeat(500),
            false,
        ),
        (
            "a value under a ceiling above the default",
            64 * 1024,
            "x".repeat(MAX_RESTORED_ECHO + 1),
            true,
        ),
    ];
    for (case, ceiling, value, whole) in rows {
        let quoted = quoted_echo_up_to(&value, ceiling);
        if whole {
            assert_eq!(
                quoted, value,
                "case {case:?}: expected the value itself, got {} bytes",
                quoted.len()
            );
            continue;
        }
        let marker = format!("... [{} bytes]", value.len());
        let Some(prefix) = quoted.strip_suffix(marker.as_str()) else {
            panic!(
                "case {case:?}: expected the marker naming {} bytes, got {} quoted bytes",
                value.len(),
                quoted.len()
            );
        };
        assert!(
            value.starts_with(prefix) && prefix.len() <= ceiling,
            "case {case:?}: expected a prefix of the value no longer than {ceiling} \
                 bytes, got {} bytes",
            prefix.len()
        );
        assert!(
            is_elision_of(&quoted, &value),
            "case {case:?}: the walk must recognise the rendering as this value's elision"
        );
    }
}

/// RULE: an echo held to a smaller bound keeps the byte count it names — the submitted
/// value's size, not the rendering's — and shortens only the prefix, so a clause held
/// twice still reports how much was sent and still quotes an elision of the same value.
#[test]
fn an_echo_held_again_keeps_the_count_it_names() {
    let submitted = "v".repeat(5000);
    let once = quoted_echo_up_to(&submitted, 768).into_owned();
    // Rows differ along what the echo IS (the value itself, an elision of a longer one),
    // the bound it is held to, and the encoding.
    let rows: [(&str, String, usize); 5] = [
        ("the value itself, within the bound", "bob".to_string(), 64),
        ("the value itself, over the bound", "v".repeat(700), 200),
        ("an elision, within the bound", once.clone(), 2048),
        ("an elision, over the bound", once.clone(), 200),
        (
            "an elision of multibyte characters, over the bound",
            quoted_echo_up_to(&"€".repeat(2000), 768).into_owned(),
            200,
        ),
    ];
    for (case, echo, bound) in rows {
        let held = echo_held_to(&echo, bound);
        assert!(
            held.len() <= bound,
            "case {case:?}: expected at most {bound} bytes, got {}",
            held.len()
        );
        if echo.len() <= bound {
            assert_eq!(
                held, echo,
                "case {case:?}: expected the echo unchanged, got {} bytes",
                held.len()
            );
            continue;
        }
        let named = named_byte_count(&echo)
            .map_or(echo.len(), |at| echo[at + "... [".len()..].trim_end_matches(" bytes]").parse().expect("a byte count"));
        assert!(
            held.ends_with(&format!("... [{named} bytes]")),
            "case {case:?}: expected the count {named} kept, got the tail {:?}",
            &held[held.len().saturating_sub(32)..]
        );
        assert!(
            echo.starts_with(held.split("... [").next().unwrap_or_default()),
            "case {case:?}: expected the kept prefix to be a prefix of the echo"
        );
    }

    // Held to the submitted value's own elision bound, the count still names the value
    // the caller sent rather than the text the first rendering produced.
    let twice = echo_held_to(&once, 200);
    assert!(
        twice.ends_with(&format!("... [{} bytes]", submitted.len())),
        "case \"a doubly held echo\": expected the submitted {} bytes named, got {twice:?}",
        submitted.len()
    );
    assert!(
        is_elision_of(&twice, &submitted),
        "case \"a doubly held echo\": the walk must still recognise it as this value's \
             elision"
    );
}

/// RULE: an echo elided with a marker of the service's own keeps at most the bound in
/// bytes, ends on a character boundary, and ends with that marker — and a value inside
/// the bound is borrowed back unchanged, whatever the marker.
#[test]
fn an_echo_elided_with_a_marker_is_bounded_and_character_safe() {
    const MARKER: &str = " ...";
    // Rows differ along the value's length relative to the bound, its encoding, and the
    // bound itself.
    let rows: [(&str, String, usize); 5] = [
        ("a value well inside the bound", "bob".to_string(), 64),
        ("a value exactly at the bound", "v".repeat(64), 64),
        ("a value one byte past the bound", "v".repeat(65), 64),
        ("a value many times the bound", "v".repeat(5000), 64),
        (
            "a multibyte value whose boundary is inside the bound",
            "€".repeat(100),
            64,
        ),
    ];
    for (case, value, bound) in rows {
        let shown = super::echo_elided_with(&value, bound, MARKER);
        if value.len() <= bound {
            assert_eq!(
                shown, value,
                "case {case:?}: expected the value unchanged, got {shown:?}"
            );
            continue;
        }
        let prefix = shown
            .strip_suffix(MARKER)
            .unwrap_or_else(|| panic!("case {case:?}: expected the marker, got {shown:?}"));
        assert!(
            prefix.len() <= bound && value.starts_with(prefix),
            "case {case:?}: expected a bounded prefix of the value, got {prefix:?}"
        );
        assert!(
            value.is_char_boundary(prefix.len()),
            "case {case:?}: expected the prefix to end on a character boundary, got {} \
                 bytes",
            prefix.len()
        );
    }
}

/// RULE: a stored echo is the submitted value's elision when it is that value, or a
/// proper prefix of it followed by the marker naming the value's byte count.
#[test]
fn an_elision_is_recognised_by_its_shape_not_by_a_cap() {
    let long = "t".repeat(300);
    let prefix: String = long.chars().take(256).collect();
    let rows: [(&str, String, bool); 6] = [
        ("the value itself", long.clone(), true),
        (
            "prefix plus the marker",
            format!("{prefix}... [300 bytes]"),
            true,
        ),
        (
            "prefix plus the marker, a shorter cap",
            format!("{}... [300 bytes]", "t".repeat(16)),
            true,
        ),
        (
            "the marker naming another byte count",
            format!("{prefix}... [301 bytes]"),
            false,
        ),
        (
            "a prefix of another value",
            format!("{}... [300 bytes]", "x".repeat(16)),
            false,
        ),
        ("an unrelated rendering", "300 items".to_string(), false),
    ];
    for (case, stored, expected) in rows {
        let actual = is_elision_of(&stored, &long);
        assert_eq!(
            actual, expected,
            "case {case:?}: expected is_elision_of == {expected}, got {actual}"
        );
    }
}

/// RULE: a leaf is a scalar list element exactly when its path ends in a 1-based
/// position followed by the collection step, and the list it belongs to is that path
/// without those two segments.
#[test]
fn a_list_element_path_names_its_list() {
    let rows: [(&str, &str, Option<&str>); 7] = [
        ("a top-level list element", "tags.1.member", Some("tags")),
        (
            "a nested list element",
            "config.rules.12.member",
            Some("config.rules"),
        ),
        (
            "an element of a list inside a list",
            "outer.2.member.inner.3.member",
            Some("outer.2.member.inner"),
        ),
        ("a struct member", "trajectoryId", None),
        ("a nested struct member", "tags.1.member.value", None),
        ("a map entry value", "attributes.someKey", None),
        ("a map key spelled like the step", "attributes.member", None),
    ];
    for (case, leaf_path, expected) in rows {
        let actual = list_of_element(leaf_path);
        assert_eq!(
            actual, expected,
            "case {case:?}: expected {expected:?}, got {actual:?}"
        );
    }
}

/// RULE: the single index pass answers a clause's path with the leaf recorded there,
/// and a `@uniqueItems` clause — reported at the LIST's path — with the first element
/// value that repeated in walk order.
#[test]
fn the_index_answers_each_violation_class() {
    let leaves: Vec<(String, String)> = [
        ("trajectoryId", "the-id"),
        ("tags.1.member", "a"),
        ("tags.2.member", "b"),
        ("tags.3.member", "a"),
        ("attributes.someKey", "entry-value"),
    ]
    .into_iter()
    .map(|(leaf_path, value)| (leaf_path.to_string(), value.to_string()))
    .collect();
    let index = Submitted::of(&leaves);

    let rows: [(&str, &str, ViolationKind, Option<&str>); 5] = [
        (
            "a member clause",
            "trajectoryId",
            ViolationKind::MaxLength(256),
            Some("the-id"),
        ),
        (
            "a list-element clause",
            "tags.2.member",
            ViolationKind::MaxLength(1),
            Some("b"),
        ),
        (
            "a map-entry clause",
            "attributes.someKey",
            ViolationKind::Pattern("^x$".to_string()),
            Some("entry-value"),
        ),
        (
            "a uniqueItems clause at the list",
            "tags",
            ViolationKind::UniqueItems,
            Some("a"),
        ),
        (
            "a clause on a member no leaf was recorded for",
            "clientToken",
            ViolationKind::MinLength(1),
            None,
        ),
    ];
    for (case, leaf_path, kind, expected) in rows {
        let violation = Violation {
            path: leaf_path.to_string(),
            kind,
            value: Some("whatever".to_string()),
            sensitive: false,
        };
        let actual = index.candidate(&violation);
        assert_eq!(
            actual, expected,
            "case {case:?}: expected {expected:?}, got {actual:?}"
        );
    }
}

/// RULE: a clause on a LIST — whose stored echo is the framework's `<n> items`
/// summary — is answered with the list rendered from exactly the `n` elements
/// recorded under it, and with nothing when the two do not agree.
#[test]
fn a_collection_clause_is_answered_with_the_list_as_submitted() {
    let leaves: Vec<(String, String)> = [
        ("resourceIds.1.member", "table/a"),
        ("resourceIds.2.member", "table/b"),
        ("resourceIds.3.member", "table/c"),
        ("policyNames.1.member", "only"),
        ("serviceNamespace", "dynamodb"),
    ]
    .into_iter()
    .map(|(leaf_path, value)| (leaf_path.to_string(), value.to_string()))
    .collect();
    let index = Submitted::of(&leaves);

    // (case, the clause's path, the echo the framework stored, the restored echo)
    let rows: [(&str, &str, &str, Option<&str>); 6] = [
        (
            "a list of three",
            "resourceIds",
            "3 items",
            Some("[table/a, table/b, table/c]"),
        ),
        (
            "a list of one, which the framework still renders in the plural",
            "policyNames",
            "1 items",
            Some("[only]"),
        ),
        (
            "a summary counting more elements than were recorded",
            "resourceIds",
            "51 items",
            None,
        ),
        ("a list no element was recorded for", "tagKeys", "2 items", None),
        (
            "a clause on the list whose echo is not a collection summary",
            "resourceIds",
            "table/a",
            None,
        ),
        (
            "a scalar member, whose echo is never a collection summary",
            "serviceNamespace",
            "dynamodb",
            None,
        ),
    ];
    for (case, path, stored, expected) in rows {
        let violation = Violation {
            path: path.to_string(),
            kind: ViolationKind::MaxLength(50),
            value: Some(stored.to_string()),
            sensitive: false,
        };
        let actual = index.collection_candidate(&violation, stored);
        assert_eq!(
            actual.as_deref(),
            expected,
            "case {case:?}: expected {expected:?}, got {actual:?}"
        );
    }
}

/// RULE: the stated bounds are finite and ordered — no single echo may exceed the
/// per-message budget, and nothing is collected that could never be restored.
#[test]
fn the_stated_bounds_are_coherent() {
    const COHERENT: () = {
        assert!(MAX_RESTORED_ECHO <= MAX_RESTORED_TOTAL);
        assert!(MAX_RECORDED_BYTES <= MAX_RESTORED_TOTAL);
        assert!(MAX_RECORDED_LEAVES > 0 && MAX_RECORDED_BYTES > 0);
        assert!(MAX_RECORDED_RENDERINGS > 0 && MAX_RECORDED_RENDERED_BYTES > 0);
        assert!(MAX_RENDERED_CHILDREN > 0);
    };
    let () = COHERENT;
}

/// RULE: a composite renders from its children — a list as `[<element>, …]`, a
/// structure as `<ShapeName>(<member>=<value>, …)` over the members that were
/// present — and renders as nothing at all when a child has no text rendering.
#[test]
fn a_composite_renders_from_the_children_it_was_given() {
    let child = |name: Option<&str>, rendering: Option<&str>| RenderedChild {
        name: name.map(str::to_string),
        declared_by: name.map(|_| "Thing".to_string()),
        key: None,
        rendering: rendering.map(str::to_string),
    };
    let sequences: [(&str, Vec<RenderedChild>, Option<&str>); 4] = [
        ("no elements", vec![], Some("[]")),
        ("one element", vec![child(None, Some("a"))], Some("[a]")),
        (
            "three elements keep their order",
            vec![
                child(None, Some("a")),
                child(None, Some("b")),
                child(None, Some("c")),
            ],
            Some("[a, b, c]"),
        ),
        (
            "an element with no text rendering",
            vec![child(None, Some("a")), child(None, None)],
            None,
        ),
    ];
    for (case, children, expected) in sequences {
        let actual = rendered_sequence(&children, MAX_RESTORED_ECHO);
        assert_eq!(
            actual.as_deref(),
            expected,
            "case {case:?}: expected {expected:?}, got {actual:?}"
        );
    }

    let structures: [(&str, Vec<RenderedChild>, Option<&str>); 5] = [
        ("no members present", vec![], Some("Thing()")),
        (
            "one member present",
            vec![child(Some("name"), Some("n"))],
            Some("Thing(name=n)"),
        ),
        (
            "two members in the order they were written",
            vec![child(Some("name"), Some("n")), child(Some("size"), Some("2"))],
            Some("Thing(name=n, size=2)"),
        ),
        (
            "a member with no text rendering suppresses the whole structure",
            vec![child(Some("name"), Some("n")), child(Some("body"), None)],
            None,
        ),
        (
            "a child with no member name at all",
            vec![child(None, Some("n"))],
            None,
        ),
    ];
    for (case, children, expected) in structures {
        let actual = rendered_structure("Thing", &children, MAX_RESTORED_ECHO);
        assert_eq!(
            actual.as_deref(),
            expected,
            "case {case:?}: expected {expected:?}, got {actual:?}"
        );
    }
}

/// RULE: a BLOB renders as the buffer the validating front end read it into — its length
/// three ways and nothing of its bytes — so a structure carrying one, and the list that
/// structure belongs to, render rather than stating no value at all.
#[test]
fn a_blob_renders_as_the_buffer_it_was_read_into() {
    // (case, the blob's length, the rendering a clause states it with). The rows differ
    // along the length: none, one byte, and a mebibyte.
    let rows: [(&str, usize, &str); 3] = [
        ("an empty blob", 0, "java.nio.HeapByteBuffer[pos=0 lim=0 cap=0]"),
        ("one byte", 1, "java.nio.HeapByteBuffer[pos=0 lim=1 cap=1]"),
        (
            "a mebibyte",
            1024 * 1024,
            "java.nio.HeapByteBuffer[pos=0 lim=1048576 cap=1048576]",
        ),
    ];
    for (case, bytes, expected) in rows {
        let actual = rendered_blob(bytes);
        assert_eq!(
            actual, expected,
            "case {case:?}: expected {expected:?}, got {actual:?}"
        );
    }

    // A structure carrying a blob renders over every member its shape declares, the
    // unset ones as `null` — and so does the list of such structures.
    let entry = RenderedChild {
        name: Some("data".to_string()),
        declared_by: Some("Entry".to_string()),
        key: None,
        rendering: Some(rendered_blob(1)),
    };
    let declared = ["data".to_string(), "key".to_string()];
    let rendered = rendered_declared_structure("Entry", &declared, &[entry], MAX_RESTORED_ECHO);
    assert_eq!(
        rendered.as_deref(),
        Some("Entry(data=java.nio.HeapByteBuffer[pos=0 lim=1 cap=1], key=null)"),
        "case a structure carrying a blob: expected it rendered, got {rendered:?}"
    );
    let element = RenderedChild {
        name: None,
        declared_by: None,
        key: None,
        rendering: rendered,
    };
    let list = rendered_sequence(&[element], MAX_RESTORED_ECHO);
    assert_eq!(
        list.as_deref(),
        Some("[Entry(data=java.nio.HeapByteBuffer[pos=0 lim=1 cap=1], key=null)]"),
        "case a list of structures carrying blobs: expected it rendered, got {list:?}"
    );
}

/// RULE: the renderers a HANDLER states its own clause with produce the same forms the
/// walk's own composites do — a structure as `<ShapeName>(<member>=<value>, …)` over
/// the members given, a map as `{<key>=<value>, …}` over its entries in KEY order — and
/// a map larger than a clause quotes renders as nothing, so one request cannot size the
/// message it is refused with.
#[test]
fn the_handler_renderers_state_the_walk_s_own_forms() {
    /// One structure row: the members given and the rendering they state.
    struct Record {
        case: &'static str,
        members: Vec<(&'static str, &'static str)>,
        expected: &'static str,
    }

    // The rows differ along how many members there are and whether one of them is the
    // unset spelling.
    let records = [
        Record {
            case: "no members",
            members: vec![],
            expected: "Thing()",
        },
        Record {
            case: "one member",
            members: vec![("name", "n")],
            expected: "Thing(name=n)",
        },
        Record {
            case: "two members in the order given",
            members: vec![("name", "n"), ("size", "2")],
            expected: "Thing(name=n, size=2)",
        },
        Record {
            case: "a member the request left unset",
            members: vec![("name", UNSET_MEMBER), ("size", "2")],
            expected: "Thing(name=null, size=2)",
        },
    ];
    for Record {
        case,
        members,
        expected,
    } in records
    {
        let actual = rendered_record("Thing", &members);
        assert_eq!(
            actual, expected,
            "case {case:?}: expected {expected:?}, got {actual:?}"
        );
    }

    /// One map row: the entries given, in the order they arrive, and the rendering.
    struct Map {
        case: &'static str,
        entries: Vec<(&'static str, String)>,
        expected: Option<&'static str>,
    }

    // The rows differ along the entry count and the order the entries arrive in, which
    // the rendering is independent of, and the last is past what a clause quotes.
    let entry = |key: &'static str, value: &str| (key, value.to_string());
    let maps = [
        Map {
            case: "no entries",
            entries: vec![],
            expected: Some("{}"),
        },
        Map {
            case: "one entry",
            entries: vec![entry("b", "2")],
            expected: Some("{b=2}"),
        },
        Map {
            case: "two entries already in key order",
            entries: vec![entry("a", "1"), entry("b", "2")],
            expected: Some("{a=1, b=2}"),
        },
        Map {
            case: "the same two entries the other way round",
            entries: vec![entry("b", "2"), entry("a", "1")],
            expected: Some("{a=1, b=2}"),
        },
        Map {
            case: "a map larger than a clause quotes",
            entries: vec![entry("a", &"x".repeat(MAX_RESTORED_ECHO))],
            expected: None,
        },
    ];
    for Map {
        case,
        entries,
        expected,
    } in maps
    {
        let actual = rendered_map_in_key_order(&entries);
        assert_eq!(
            actual.as_deref(),
            expected,
            "case {case:?}: expected {expected:?}, got {actual:?}"
        );
    }
}

/// RULE: in the DECLARED form a structure renders over every member its shape
/// declares, in declaration order, with one the request left unset stated as `null`;
/// a member that WAS written and has no text rendering still suppresses the whole
/// structure.
#[test]
fn a_declared_structure_names_every_member_its_shape_declares() {
    let child = |name: &str, rendering: Option<&str>| RenderedChild {
        name: Some(name.to_string()),
        declared_by: Some("Thing".to_string()),
        key: None,
        rendering: rendering.map(str::to_string),
    };
    let declared = |names: &[&str]| -> Vec<String> {
        names.iter().map(|name| (*name).to_string()).collect()
    };

    // (case, the members the shape declares, the children the request wrote, the
    // rendering). The rows differ along which declared members were set, how many
    // were not, and where the unset ones sit in the declaration order.
    struct Row {
        case: &'static str,
        declared: Vec<String>,
        children: Vec<RenderedChild>,
        expected: Option<&'static str>,
    }

    let rows = [
        Row {
            case: "a shape declaring nothing",
            declared: declared(&[]),
            children: vec![],
            expected: Some("Thing()"),
        },
        Row {
            case: "every declared member set",
            declared: declared(&["name", "size"]),
            children: vec![child("name", Some("n")), child("size", Some("2"))],
            expected: Some("Thing(name=n, size=2)"),
        },
        Row {
            case: "a trailing member left unset",
            declared: declared(&["name", "size"]),
            children: vec![child("name", Some("n"))],
            expected: Some("Thing(name=n, size=null)"),
        },
        Row {
            case: "a leading member left unset",
            declared: declared(&["name", "size"]),
            children: vec![child("size", Some("2"))],
            expected: Some("Thing(name=null, size=2)"),
        },
        Row {
            case: "every declared member left unset",
            declared: declared(&["name", "size", "body"]),
            children: vec![],
            expected: Some("Thing(name=null, size=null, body=null)"),
        },
        Row {
            case: "a member written with no text rendering",
            declared: declared(&["name", "body"]),
            children: vec![child("name", Some("n")), child("body", None)],
            expected: None,
        },
    ];
    for row in rows {
        let actual = rendered_declared_structure(
            "Thing",
            &row.declared,
            &row.children,
            MAX_RESTORED_ECHO,
        );
        assert_eq!(
            actual.as_deref(),
            row.expected,
            "case {:?}: expected {:?}, got {actual:?}",
            row.case,
            row.expected
        );
    }
}

/// RULE: what a composite may hold is bounded by BYTES as well as by child count, and
/// a rendering past [`MAX_RESTORED_ECHO`] is refused before it is built — so neither
/// the text this walk retains nor the text it allocates is sized by the request.
#[test]
fn a_composite_is_bounded_by_the_bytes_its_children_hold() {
    let child = |rendering: &str| RenderedChild {
        name: Some("name".to_string()),
        declared_by: Some("Thing".to_string()),
        key: None,
        rendering: Some(rendering.to_string()),
    };

    // One child at the bound is held; the next one tips the composite over it, and a
    // truncated composite renders as nothing rather than as a part of the value.
    let mut composite = Composite::default();
    composite.take(
        child(&"a".repeat(MAX_RESTORED_ECHO - "name".len())),
        MAX_RENDERED_CHILDREN,
        MAX_RESTORED_ECHO,
    );
    assert!(
        !composite.truncated && composite.children.len() == 1,
        "case a child at the byte bound: expected it held, got {composite:?}"
    );
    composite.take(child("b"), MAX_RENDERED_CHILDREN, MAX_RESTORED_ECHO);
    assert!(
        composite.truncated && composite.children.len() == 1,
        "case a child past the byte bound: expected the composite truncated with the \
             child dropped, got {composite:?}"
    );

    // A child count past the cap is the other bound, unchanged.
    let mut counted = Composite::default();
    for _ in 0..=MAX_RENDERED_CHILDREN {
        counted.take(child("a"), MAX_RENDERED_CHILDREN, MAX_RESTORED_ECHO);
    }
    assert!(
        counted.truncated && counted.children.len() == MAX_RENDERED_CHILDREN,
        "case more children than the cap: expected the composite truncated at the cap, \
             got {} children",
        counted.children.len()
    );

    // A caller that states a LARGER child ceiling — a service whose model declares a
    // collection longer than the default count — gets those children held.
    let stated_children = 2 * MAX_RENDERED_CHILDREN;
    let mut stated = Composite::default();
    for _ in 0..stated_children {
        stated.take(child("a"), stated_children, MAX_RESTORED_ECHO * 64);
    }
    assert!(
        !stated.truncated && stated.children.len() == stated_children,
        "case a stated child ceiling: expected every child held, got {} of \
             {stated_children}",
        stated.children.len()
    );

    // And a rendering that would exceed the bound is refused by both renderers, which
    // measure it before joining anything.
    let oversized = vec![
        child(&"a".repeat(MAX_RESTORED_ECHO)),
        child(&"b".repeat(MAX_RESTORED_ECHO)),
    ];
    let sequence = rendered_sequence(&oversized, MAX_RESTORED_ECHO);
    assert_eq!(
        sequence, None,
        "case an oversized list: expected no rendering, got {sequence:?}"
    );
    let structure = rendered_structure("Thing", &oversized, MAX_RESTORED_ECHO);
    assert_eq!(
        structure, None,
        "case an oversized structure: expected no rendering, got {structure:?}"
    );
    let declared = rendered_declared_structure(
        "Thing",
        &["name".to_string()],
        &[child(&"a".repeat(MAX_RESTORED_ECHO))],
        MAX_RESTORED_ECHO,
    );
    assert_eq!(
        declared, None,
        "case an oversized declared structure: expected no rendering, got {declared:?}"
    );

    // A caller that states its OWN composite ceiling is held to THAT one: the same
    // children the module's own bound refuses render whole under a ceiling above them,
    // and the bound still bites one byte past whatever was stated.
    let stated = 4 * MAX_RESTORED_ECHO;
    let under_stated = rendered_sequence(&oversized, stated);
    assert!(
        under_stated.is_some_and(|rendered| rendered.len() > MAX_RESTORED_ECHO),
        "case a list past the default bound under a stated ceiling: expected the whole \
             list rendered"
    );
    let mut held = Composite::default();
    held.take(
        child(&"a".repeat(MAX_RESTORED_ECHO)),
        MAX_RENDERED_CHILDREN,
        stated,
    );
    held.take(
        child(&"b".repeat(MAX_RESTORED_ECHO)),
        MAX_RENDERED_CHILDREN,
        stated,
    );
    assert!(
        !held.truncated && held.children.len() == 2,
        "case two children inside a stated ceiling: expected both held, got {held:?}"
    );
    let tight = rendered_sequence(&oversized, 1);
    assert_eq!(
        tight, None,
        "case a stated ceiling below the rendering: expected no rendering, got {tight:?}"
    );
}

/// RULE: a clause's echo is `Absent` for a member the request never sent, `Rendered`
/// for a position this walk rendered, and `Unrendered` for a value that was present
/// with no text rendering, or past the stated bounds.
#[test]
fn an_echo_distinguishes_absent_from_present_but_unrenderable() {
    let too_long = "x".repeat(MAX_RESTORED_ECHO + 1);
    let renderings: Vec<(String, Option<String>)> = [
        ("name", Some("a-name")),
        ("body", None),
        ("tags", Some("[a, b]")),
    ]
    .into_iter()
    .map(|(position, rendering)| {
        (
            position.to_string(),
            rendering.map(|text: &str| text.to_string()),
        )
    })
    .chain(std::iter::once((
        "huge".to_string(),
        Some(too_long.clone()),
    )))
    .collect();
    let mut by_path: HashMap<&str, Option<&str>> = HashMap::new();
    for (position, rendering) in &renderings {
        by_path.insert(position.as_str(), rendering.as_deref());
    }

    let rows: [(&str, &str, ViolationKind, Option<&str>, SubmittedEcho); 6] = [
        (
            "an absent member",
            "name",
            ViolationKind::MissingRequiredMember,
            None,
            SubmittedEcho::Absent,
        ),
        (
            "a rendered string",
            "name",
            ViolationKind::MaxLength(1),
            Some("a-name"),
            SubmittedEcho::Rendered("a-name".to_string()),
        ),
        (
            "a list rendered from its elements",
            "tags",
            ViolationKind::MaxLength(1),
            Some("2 items"),
            SubmittedEcho::Rendered("[a, b]".to_string()),
        ),
        (
            "a value with no text rendering",
            "body",
            ViolationKind::MaxLength(1),
            Some("4000 bytes"),
            SubmittedEcho::Unrendered,
        ),
        (
            "a position this walk recorded nothing for",
            "elsewhere",
            ViolationKind::MaxLength(1),
            Some("whatever"),
            SubmittedEcho::Unrendered,
        ),
        (
            "a rendering past the stated bound",
            "huge",
            ViolationKind::MaxLength(1),
            Some("whatever"),
            SubmittedEcho::Unrendered,
        ),
    ];
    for (case, path, kind, value, expected) in rows {
        let violation = Violation {
            path: path.to_string(),
            kind,
            value: value.map(str::to_string),
            sensitive: false,
        };
        let mut budget = MAX_RESTORED_TOTAL;
        let actual = echo_of(&violation, &by_path, &mut budget, MAX_RESTORED_ECHO);
        assert_eq!(
            actual, expected,
            "case {case:?}: expected {expected:?}, got {actual:?}"
        );
    }
}

/// RULE: a MAP renders as the entries it holds — `{<key>=<value>, …}` in KEY order —
/// for the caller that states that form, and as nothing for the caller that does not;
/// an entry whose value has no text rendering, and a child that is no map entry at
/// all, suppress the whole rendering rather than state a map missing an entry.
///
/// The rows span what the rule generalises over: how many entries the map holds, which
/// order they arrived in, and whether every entry can be stated.
#[test]
fn a_map_renders_as_its_entries_in_key_order() {
    let entry = |key: Option<&str>, rendering: Option<&str>| RenderedChild {
        name: Some("tags".to_string()),
        declared_by: None,
        key: key.map(str::to_string),
        rendering: rendering.map(str::to_string),
    };

    let rows: [(&str, Vec<RenderedChild>, Option<&str>); 6] = [
        ("no entries", vec![], Some("{}")),
        (
            "one entry",
            vec![entry(Some("a"), Some("1"))],
            Some("{a=1}"),
        ),
        (
            "entries that arrived in key order",
            vec![
                entry(Some("a"), Some("1")),
                entry(Some("b"), Some("2")),
                entry(Some("c"), Some("3")),
            ],
            Some("{a=1, b=2, c=3}"),
        ),
        (
            "entries that arrived in another order",
            vec![
                entry(Some("c"), Some("3")),
                entry(Some("a"), Some("1")),
                entry(Some("b"), Some("2")),
            ],
            Some("{a=1, b=2, c=3}"),
        ),
        (
            "an entry whose value has no text rendering",
            vec![entry(Some("a"), Some("1")), entry(Some("b"), None)],
            None,
        ),
        (
            "a child that is no map entry",
            vec![entry(None, Some("1"))],
            None,
        ),
    ];

    for (case, children, expected) in rows {
        let actual = rendered_entries(&children, MAX_RESTORED_ECHO);
        assert_eq!(
            actual.as_deref(),
            expected,
            "case {case:?}: expected {expected:?}, got {actual:?}"
        );
    }

    // The entries a caller renders by hand go through the same renderer, so a clause a
    // handler words and one restored from the walk state one map the same way.
    let stated = rendered_map(&[("a", "1"), ("b", "2")]);
    assert_eq!(
        stated, "{a=1, b=2}",
        "case \"stated by a caller\": expected the same form, got {stated}"
    );
}

/// RULE: the two renderings drop the SAME pair ([`super::unanchored`]'s positional
/// rule, which the test above pins) and differ in exactly one thing —
/// [`super::unanchored_grouping_alternation`] leaves a top-level ALTERNATION grouped,
/// so the expression it quotes still matches what the model's anchored text matched,
/// while [`super::unanchored`] quotes it bare. Nothing else is grouped: an alternation
/// already inside a group is not wrapped again, and an expression whose pair was not
/// dropped is stated verbatim by both.
///
/// The rows are the model patterns this rule meets in the tree plus the regex
/// constructs that can hide an anchor or an alternation — an escape, a character class,
/// a group — and the half-anchored forms neither rendering may touch.
#[test]
fn only_the_grouping_rendering_wraps_a_top_level_alternation() {
    // (case, the model's pattern, what `unanchored` states, what
    // `unanchored_grouping_alternation` states)
    let rows: [(&str, &str, &str, &str); 15] = [
        ("a plain whole-value pattern", "^[0-9]{12}$", "[0-9]{12}", "[0-9]{12}"),
        (
            "an alternation already inside a group",
            "^(a|b)$",
            "(a|b)",
            "(a|b)",
        ),
        // The shape of this service family's `SessionIdentifier`: an ARN branch or a
        // bare uuid branch, sharing one outer pair.
        (
            "a top-level alternation of two groups",
            "^(arn:[^:]+:x)|([a-f0-9]{8})$",
            "(arn:[^:]+:x)|([a-f0-9]{8})",
            "((arn:[^:]+:x)|([a-f0-9]{8}))",
        ),
        (
            "a bare top-level alternation",
            "^a|b$",
            "a|b",
            "(a|b)",
        ),
        // Branches that carry an anchor of their own while SHARING the outer pair: the
        // pair is dropped positionally (see the test above), and the group this
        // rendering adds around what is left changes nothing about what it matches.
        (
            "branches anchored individually, sharing the outer pair",
            "^a$|^b$",
            "a$|^b",
            "(a$|^b)",
        ),
        // Branches parenthesised, so there is no outer pair to drop and nothing to
        // group.
        (
            "branches anchored individually inside their own groups",
            "(^a$)|(^b$)",
            "(^a$)|(^b$)",
            "(^a$)|(^b$)",
        ),
        ("anchored at the start only", "^abc", "^abc", "^abc"),
        ("anchored at the end only", "abc$", "abc$", "abc$"),
        ("no anchors at all", "[0-9]+", "[0-9]+", "[0-9]+"),
        (
            "a trailing escaped dollar, which is a literal",
            r"^cost\$$",
            r"cost\$",
            r"cost\$",
        ),
        (
            "an expression that IS a literal dollar",
            r"^\$$",
            r"\$",
            r"\$",
        ),
        (
            "a pipe inside a character class, which is a literal",
            "^[a|b]{2}$",
            "[a|b]{2}",
            "[a|b]{2}",
        ),
        (
            "a caret inside a negated class, which is not an anchor",
            "^[^:]+$",
            "[^:]+",
            "[^:]+",
        ),
        // A `]` in the first position of a class is the literal bracket, so the `|`
        // after it is still inside the class and alternates nothing.
        (
            "a literal bracket opening a class, with a pipe inside it",
            "^[]|]a$",
            "[]|]a",
            "[]|]a",
        ),
        (
            "an escaped bracket inside a class, with a pipe inside it",
            r"^[\]|]a$",
            r"[\]|]a",
            r"[\]|]a",
        ),
    ];

    for (case, pattern, plain, grouped) in rows {
        let actual = unanchored(pattern);
        assert_eq!(
            actual, plain,
            "case {case:?}: expected unanchored {plain:?}, got {actual:?}"
        );
        let actual = unanchored_grouping_alternation(pattern);
        assert_eq!(
            actual, grouped,
            "case {case:?}: expected unanchored_grouping_alternation {grouped:?}, \
                 got {actual:?}"
        );
    }
}

/// RULE: [`super::unanchored_keeping_inner_anchors`] drops the same pair the two
/// renderings above drop — whatever the expression's top level does, which is what the
/// recorded lambdacore refusal quoted in [`super::unanchored`] states — and declines the
/// drop for one case: an expression carrying an anchor of its OWN, which is a whole-value
/// expression before any wrapping and so is quoted as the model declares it.
///
/// The rows are the forms a published `@pattern` takes in the tree: a plain whole-value
/// expression, the recorded top-level alternation that carries no anchor of its own, the
/// alternations whose branches anchor themselves (sharing one pair, parenthesised, and
/// with an empty first branch), the half-anchored forms no rendering may touch, and the
/// regex constructs that can hide an anchor — an escape, a negated class, a class whose
/// first member is a literal bracket.
#[test]
fn the_inner_anchor_rendering_declines_the_drop_for_a_self_anchored_expression() {
    // (case, the model's pattern, what `unanchored_keeping_inner_anchors` states)
    let rows: [(&str, &str, &str); 14] = [
        ("a plain whole-value pattern", "^[0-9]{12}$", "[0-9]{12}"),
        // The recorded form: a top-level alternation carrying no anchor of its own, so
        // the pair is the model's wrapping and is dropped, exactly as `unanchored`
        // states it (`com.amazonaws.lambdacore#NetworkConnectorName`).
        (
            "a recorded top-level alternation of a group and a bare branch",
            "^(arn:[^:]+:network-connector:[^:]+)|[a-zA-Z0-9_-]{1,64}$",
            "(arn:[^:]+:network-connector:[^:]+)|[a-zA-Z0-9_-]{1,64}",
        ),
        ("a bare top-level alternation", "^a|b$", "a|b"),
        ("an alternation already inside a group", "^(a|b)$", "(a|b)"),
        // Branches that anchor themselves while sharing the outer pair: dropping it
        // would state `a$|^b`, which requires nothing of either branch.
        (
            "branches anchored individually, sharing the outer pair",
            "^a$|^b$",
            "^a$|^b$",
        ),
        // The shape of an "empty or an ARN" member: the first branch IS the anchored
        // empty string, the only value it accepts.
        (
            "an anchored empty branch beside an anchored ARN branch",
            r"^$|^arn:aws:logs:r:1:log-group:g:\*$",
            r"^$|^arn:aws:logs:r:1:log-group:g:\*$",
        ),
        (
            "branches anchored inside their own groups, wrapped in a further pair",
            "^(^a$)|(^b$)$",
            "^(^a$)|(^b$)$",
        ),
        (
            "branches anchored inside their own groups, with no outer pair",
            "(^a$)|(^b$)",
            "(^a$)|(^b$)",
        ),
        ("anchored at the start only", "^abc", "^abc"),
        ("anchored at the end only", "abc$", "abc$"),
        ("no anchors at all", "[0-9]+", "[0-9]+"),
        (
            "a trailing escaped dollar, which is a literal and no anchor",
            r"^cost\$$",
            r"cost\$",
        ),
        (
            "an escaped caret inside the expression, which is a literal",
            r"^a\^b$",
            r"a\^b",
        ),
        (
            "a caret negating a class, and a literal bracket opening another",
            "^[^:]+[]|]$",
            "[^:]+[]|]",
        ),
    ];

    for (case, pattern, stated) in rows {
        let actual = unanchored_keeping_inner_anchors(pattern);
        assert_eq!(
            actual, stated,
            "case {case:?}: expected unanchored_keeping_inner_anchors {stated:?}, got \
                 {actual:?}"
        );
    }
}

/// RULE: [`super::unanchored_unless_published_as_declared`] quotes an expression the
/// service's own list names exactly as the model writes it, and defers to
/// [`super::unanchored_keeping_inner_anchors`] for every other expression — so the list
/// decides nothing but its own rows.
///
/// The rows span what the rule generalises over: a listed expression and an unlisted one
/// over the same shape of text, a listed expression that rendering would have left alone
/// anyway, an expression that only LOOKS like a listed one (the same text without its
/// pair), and the empty list, which is every service that publishes no expression as
/// declared.
#[test]
fn a_published_expression_is_quoted_as_the_model_declares_it() {
    let published: &[&str] = &[r"^aos-[a-zA-Z0-9]*$", "^a$|^b$"];
    // (case, the list, the model's pattern, what the rendering states)
    let rows: [(&str, &[&str], &str, &str); 6] = [
        (
            "an expression the list names",
            published,
            r"^aos-[a-zA-Z0-9]*$",
            r"^aos-[a-zA-Z0-9]*$",
        ),
        (
            "an expression of the same shape the list does not name",
            published,
            r"^vpce-[a-zA-Z0-9]*$",
            r"vpce-[a-zA-Z0-9]*",
        ),
        (
            "a listed expression the other rendering would keep anyway",
            published,
            "^a$|^b$",
            "^a$|^b$",
        ),
        (
            "the listed expression without its pair, which is a different expression",
            published,
            r"aos-[a-zA-Z0-9]*",
            r"aos-[a-zA-Z0-9]*",
        ),
        (
            "no expression published as declared",
            &[],
            r"^aos-[a-zA-Z0-9]*$",
            r"aos-[a-zA-Z0-9]*",
        ),
        (
            "no expression published as declared, over a self-anchored alternation",
            &[],
            "^a$|^b$",
            "^a$|^b$",
        ),
    ];

    for (case, list, pattern, stated) in rows {
        let actual = unanchored_unless_published_as_declared(pattern, list);
        assert_eq!(
            actual, stated,
            "case {case:?}: expected {stated:?}, got {actual:?}"
        );
    }
}

/// RULE: [`super::unanchored_at_the_close`] states the expression as the model declares
/// it, less the `$` that closes it — so the opening `^` and every anchor the expression
/// writes around its own branches stay, an expression that closes with no anchor is
/// quoted verbatim, and a trailing escaped dollar is a literal rather than an anchor.
///
/// The rows span the forms the two ends take (both anchored, start only, end only,
/// neither), the alternations whose branches carry anchors of their own, the lookahead
/// form the `CustomID` family of `iotsitewise` publishes, and the escape that hides an
/// anchor.
#[test]
fn the_closing_anchor_rendering_drops_only_the_expressions_last_anchor() {
    // (case, the model's pattern, what `unanchored_at_the_close` states)
    let rows: [(&str, &str, &str); 8] = [
        ("a plain whole-value pattern", "^[0-9]{12}$", "^[0-9]{12}"),
        ("anchored at the start only", "^abc", "^abc"),
        ("anchored at the end only", "abc$", "abc"),
        ("no anchors at all", "[0-9]+", "[0-9]+"),
        ("branches anchored individually", "^a$|^b$", "^a$|^b"),
        (
            "branches anchored inside their own groups",
            "(^a$)|(^b$)",
            "(^a$)|(^b$)",
        ),
        // The published form of a reference that is a UUID or an `externalId:` alias:
        // a lookahead, two anchored branches, and the closing anchor this drops.
        (
            "a two-branch reference pattern opening with a negative lookahead",
            "^(?!0{4})[0-9a-f]{4}$|^externalId:[a-zA-Z0-9_]+$",
            "^(?!0{4})[0-9a-f]{4}$|^externalId:[a-zA-Z0-9_]+",
        ),
        (
            "a trailing escaped dollar, which is a literal and no anchor",
            r"^cost\$",
            r"^cost\$",
        ),
    ];

    for (case, pattern, stated) in rows {
        let actual = super::unanchored_at_the_close(pattern);
        assert_eq!(
            actual, stated,
            "case {case:?}: expected unanchored_at_the_close {stated:?}, got {actual:?}"
        );
    }
}

/// RULE: [`super::unanchored_at_the_open`] states the expression as the model declares
/// it, less the `^` that opens it — so the closing `$` and every anchor the expression
/// writes around its own branches stay, and an expression that opens with no anchor is
/// quoted verbatim.
///
/// The rows span the forms the two ends take (both anchored, start only, end only,
/// neither), the alternations whose branches carry anchors of their own, an expression
/// whose body opens with an escape, and the one the Network Firewall pagination token
/// publishes.
#[test]
fn the_opening_anchor_rendering_drops_only_the_expressions_first_anchor() {
    // (case, the model's pattern, what `unanchored_at_the_open` states)
    let rows: [(&str, &str, &str); 8] = [
        ("a plain whole-value pattern", "^[0-9]{12}$", "[0-9]{12}$"),
        ("anchored at the start only", "^abc", "abc"),
        ("anchored at the end only", "abc$", "abc$"),
        ("no anchors at all", "[0-9]+", "[0-9]+"),
        ("branches anchored individually", "^a$|^b$", "a$|^b$"),
        (
            "branches anchored inside their own groups",
            "(^a$)|(^b$)",
            "(^a$)|(^b$)",
        ),
        // The pagination token of `com.amazonaws.networkfirewall#PaginationToken`,
        // whose refusals quote the expression without its opening anchor.
        (
            "a pagination token's character class",
            r"^[0-9A-Za-z:\/+=]+$",
            r"[0-9A-Za-z:\/+=]+$",
        ),
        (
            "a body opening with an escape",
            r"^\d{4}-\d{2}$",
            r"\d{4}-\d{2}$",
        ),
    ];

    for (case, pattern, stated) in rows {
        let actual = super::unanchored_at_the_open(pattern);
        assert_eq!(
            actual, stated,
            "case {case:?}: expected unanchored_at_the_open {stated:?}, got {actual:?}"
        );
    }
}

/// RULE: [`super::unanchored_at_the_open_where_published`] drops that anchor for the
/// expressions a service names and quotes every other as the model declares it.
///
/// The rows span a named expression, an expression the list does not name, a named one
/// that carries no opening anchor to drop, and the empty list.
#[test]
fn only_a_published_expression_loses_its_opening_anchor() {
    let published = [r"^[0-9A-Za-z:\/+=]+$", "abc"];
    // (case, the model's pattern, the list, what is stated)
    let rows: [(&str, &str, &[&str], &str); 4] = [
        (
            "a published expression",
            r"^[0-9A-Za-z:\/+=]+$",
            &published,
            r"[0-9A-Za-z:\/+=]+$",
        ),
        (
            "an expression the list does not name",
            "^vpc-[0-9a-f]+$",
            &published,
            "^vpc-[0-9a-f]+$",
        ),
        ("a published expression with no anchor", "abc", &published, "abc"),
        (
            "nothing published at all",
            r"^[0-9A-Za-z:\/+=]+$",
            &[],
            r"^[0-9A-Za-z:\/+=]+$",
        ),
    ];

    for (case, pattern, published, stated) in rows {
        let actual = super::unanchored_at_the_open_where_published(pattern, published);
        assert_eq!(
            actual, stated,
            "case {case:?}: expected {stated:?}, got {actual:?}"
        );
    }
}

/// RULE: [`super::anchors_its_own_ends`] reports whether EVERY alternative an expression
/// offers at its top level — read through a group that wraps the whole expression —
/// pins both of its own ends with a `^` and an unescaped `$`.
///
/// The rows span the forms such an expression takes (branches anchored inside one
/// wrapping group, anchored without one, a single anchored branch, an anchored empty
/// branch), the forms that pin only one end or neither, an anchor that pins no end at
/// all, and the constructs that can hide a `|` or an anchor: a group, a character class,
/// a negated class, a class whose first member is a literal bracket, and an escape.
#[test]
fn an_expression_that_pins_both_of_its_own_ends_is_reported_as_such() {
    // (case, the expression, whether it anchors its own ends)
    let rows: [(&str, &str, bool); 14] = [
        // The `DeploymentStrategyId` shape: an alternation of whole-value branches,
        // kept together by one wrapping group.
        (
            "branches anchored inside one wrapping group",
            r"(^[a-z0-9]{4,7}$|^AppConfig\.[A-Za-z0-9]{9,40}$)",
            true,
        ),
        ("branches anchored with no wrapping group", r"^a$|^b$", true),
        ("branches anchored inside their own groups", r"(^a$)|(^b$)", true),
        ("one branch, anchored at both ends", r"^[a-z]+$", true),
        ("an anchored empty branch beside another", r"^$|^arn:aws.*$", true),
        ("a branch anchored at the start only", r"^a$|^b", false),
        ("anchored at the start only", r"^arn:aws.*", false),
        ("anchored at the end only", r"[0-9]{12}$", false),
        ("an interior dollar that pins neither end", r"a$b", false),
        ("no anchor at all", r"[a-zA-Z0-9_-]{1,64}", false),
        ("a wrapping group around an unanchored branch", r"((arn):[a-z]+)", false),
        ("two groups, neither wrapping the whole", r"(^a$)(^b$)", false),
        ("a caret that negates a class", r"[^0-9]", false),
        ("a trailing escaped dollar", r"^cost\$", false),
    ];

    for (case, expression, anchors) in rows {
        let actual = super::anchors_its_own_ends(expression);
        assert_eq!(
            actual, anchors,
            "case {case:?}: expected anchors_its_own_ends {anchors}, got {actual}"
        );
    }
}

/// RULE: [`super::whole_value_expression`] states the expression the WHOLE value must
/// match, deciding the two ends independently — an anchored end loses its anchor, an
/// open end is spelled with the unconstrained run it admits — while an expression that
/// anchors only its own branches is quoted as declared, and a top-level alternation
/// left bare by dropping BOTH anchors is kept grouped.
///
/// The rows span every combination of the two ends (both anchored, neither, start only,
/// end only), the alternation forms the Bedrock family's published patterns take
/// (branches sharing one pair, branches anchored individually, branches anchored inside
/// their own groups), an expression that already opens an end with the run, and the
/// regex constructs that can hide an anchor or an alternation — an escape, a negated
/// class, a class whose first member is a literal bracket.
#[test]
fn the_whole_value_rendering_decides_the_two_ends_independently() {
    // (case, the model's pattern, what `whole_value_expression` states)
    let rows: [(&str, &str, &str); 16] = [
        ("a plain whole-value pattern", "^[0-9]{12}$", "[0-9]{12}"),
        // The recorded `SessionId` form: one branch, both ends anchored.
        (
            "a recorded single-branch label pattern",
            "^[0-9a-zA-Z._:-]+$",
            "[0-9a-zA-Z._:-]+",
        ),
        // The recorded `FlowIdentifier` form: two groups sharing one pair, neither
        // branch anchored, so the leftover alternation is grouped.
        (
            "a top-level alternation of two unanchored groups",
            "^(arn:[^:]+:flow/[0-9a-zA-Z]{10})|([0-9a-zA-Z]{10})$",
            "((arn:[^:]+:flow/[0-9a-zA-Z]{10})|([0-9a-zA-Z]{10}))",
        ),
        ("a bare top-level alternation", "^a|b$", "(a|b)"),
        // The recorded `FlowExecutionIdentifier` form: the two ends are the first and
        // last branch's own anchors, and the interior ones stay where they are.
        (
            "branches anchored individually, sharing the outer pair",
            "^[a-zA-Z0-9-]{1,36}$|^(arn:[^:]+:execution/x)$",
            "[a-zA-Z0-9-]{1,36}$|^(arn:[^:]+:execution/x)",
        ),
        // The recorded `ModelIdentifier` form: the start is open, the end anchored.
        (
            "branches in their own groups with the anchors outside them",
            "(^a)$|(^b)$",
            ".*(^a)$|(^b)",
        ),
        (
            "branches anchored inside their own groups",
            "(^a$)|(^b$)",
            "(^a$)|(^b$)",
        ),
        (
            "one branch anchored inside its own group",
            "(^arn:[^:]+:session/x$)",
            "(^arn:[^:]+:session/x$)",
        ),
        ("anchored at the start only", "^abc", "abc.*"),
        ("anchored at the end only", "abc$", ".*abc"),
        ("no anchors at all", "[0-9]+", ".*[0-9]+.*"),
        (
            "an expression that already opens its tail with the run",
            "^abc.*",
            "abc.*",
        ),
        (
            "a trailing escaped dollar, which is a literal and no anchor",
            r"^cost\$",
            r"cost\$.*",
        ),
        (
            "a caret inside a negated class, which is not an anchor",
            "^[^:]+$",
            "[^:]+",
        ),
        (
            "a pipe inside a character class, which is a literal",
            "^[a|b]{2}$",
            "[a|b]{2}",
        ),
        (
            "a literal bracket opening a class, with a pipe inside it",
            "^[]|]a$",
            "[]|]a",
        ),
    ];

    for (case, pattern, stated) in rows {
        let actual = whole_value_expression(pattern);
        assert_eq!(
            actual, stated,
            "case {case:?}: expected whole_value_expression {stated:?}, got {actual:?}"
        );
    }
}
