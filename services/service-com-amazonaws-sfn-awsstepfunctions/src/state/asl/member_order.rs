// SPDX-License-Identifier: CC0-1.0

//! The ORDER a definition writes its members in, which is the order the problems found
//! in them are reported in.
//!
//! A definition is read as a [`Value`], whose object is a `BTreeMap`: the parse keeps
//! every member and its value, but not the ORDER the submitted document wrote them in,
//! so a walk that iterates one object visits its members sorted by name. The service
//! reports what it found in a definition in the order the offending members appear in
//! the document the caller submitted — the subjects of an `InvalidDefinition` message
//! and the diagnostic list of a validation alike — so the walk is handed that order
//! here and visits each object through it ([`MemberOrder::members`],
//! [`MemberOrder::names`], [`MemberOrder::paths`]).
//!
//! The order is read off the SAME maintained parser the definition itself is read with:
//! the document is deserialized once more, through a seed that records where each
//! member was written rather than building a second copy of it
//! ([`serde::de::DeserializeSeed`]), so nothing here scans or parses JSON text of its
//! own. A member is recorded under the location the walk reports it at
//! ([`nested_at`], [`indexed_at`], [`echoed`]), which is what lets a walk holding a
//! location ask for the order of what is written there.
//!
//! What it costs is bounded like every other walk over a caller-sized document (the
//! modelled `Definition` is 1 MiB): one pass over the text, each recorded location
//! clipped to [`MAX_LOCATION_CHARS`] as it is built, and at most
//! [`MAX_RANKED_MEMBERS`] of them recorded. A member the table does not carry — one
//! past that ceiling, or one whose location was clipped onto another's — keeps the
//! parse's own order, so a definition larger than the table is reported in the order it
//! always was rather than in none.
//!
//! The scan follows the document's nesting, which is a caller's: how deep it may follow
//! is the reader's own ceiling on nested structures, the same one that bounds the parse
//! of the definition itself, so a document nested past it stops the scan with the parse
//! fault the walk reports rather than with this thread's stack. Nothing here reads a
//! value of the document, and a document that does not parse is read as far as the
//! fault: the scan is total over whatever text arrives.

use std::collections::HashMap;
use std::fmt;

use serde::de::{self, DeserializeSeed, Deserializer, IgnoredAny, MapAccess, SeqAccess, Visitor};
use serde::Deserialize;

use super::*;

/// How many members of one definition the order table carries.
///
/// An emulator bound, not an AWS limit: a definition is caller-sized and every member
/// of it would be recorded under a location of up to [`MAX_LOCATION_CHARS`], so the
/// table carries a bounded multiple of the definition's own ceiling rather than a
/// product of its member count and its nesting. It is far more members than the
/// diagnostics one walk reports ([`MAX_DIAGNOSTICS`]) can be about.
const MAX_RANKED_MEMBERS: usize = 4096;

/// Where each member of a definition is written in it, by the location the walk
/// reports that member at.
///
/// Empty until a walk is handed the definition's text to read the order off
/// ([`Diagnosis::ordered_by`]): a lookup in an empty table finds nothing, which leaves
/// every member where the parse put it.
#[derive(Debug, Clone, Default)]
pub(super) struct MemberOrder {
    /// The location of a member, and how many members were written before it.
    ranks: HashMap<String, usize>,
}

impl MemberOrder {
    /// No order at all: every member keeps the order the parse gives it.
    pub(super) fn none() -> Self {
        Self::default()
    }

    /// The order `definition` writes its members in, read as a whole document.
    pub(super) fn of(definition: &str) -> Self {
        Self::within("", definition)
    }

    /// The same, for a document the walk reads at `prefix` rather than at the root.
    pub(super) fn within(prefix: &str, definition: &str) -> Self {
        let mut ranks = Ranks {
            ranks: HashMap::new(),
            written: 0,
            room: MAX_RANKED_MEMBERS,
        };
        let mut reader = serde_json::Deserializer::from_str(definition);
        // A document that does not parse is reported as the parse fault it is by the
        // walk itself, so what was read of it before the fault is all the order this
        // carries.
        let _ = Scan {
            path: prefix.to_string(),
            record: false,
            into: &mut ranks,
        }
        .deserialize(&mut reader);
        Self { ranks: ranks.ranks }
    }

    /// How many members were written before the one at `path`.
    ///
    /// A location the table does not carry is ordered with the nearest ENCLOSING one it
    /// does: a field reported inside a value the walk located by something other than a
    /// member name belongs where the member that holds it is written. A location with no
    /// ranked ancestor at all takes the last place of all, which leaves it where the
    /// parse put it.
    fn rank(&self, path: &str) -> usize {
        if let Some(rank) = self.ranks.get(path) {
            return *rank;
        }
        let mut enclosing = path;
        while let Some(cut) = enclosing.rfind(['/', '[']) {
            enclosing = &enclosing[..cut];
            if let Some(rank) = self.ranks.get(enclosing) {
                return *rank;
            }
        }
        usize::MAX
    }

    /// The members of the object at `prefix`, in the order the definition writes them.
    ///
    /// A member the table does not carry follows the ones it does, keeping the parse's
    /// own order among them: the sort is stable, so an object no part of which was
    /// recorded is visited exactly as it was before.
    pub(super) fn members<'a>(
        &self,
        prefix: &str,
        members: &'a Map<String, Value>,
    ) -> Vec<(&'a String, &'a Value)> {
        let mut ordered: Vec<(&'a String, &'a Value)> = members.iter().collect();
        ordered.sort_by_cached_key(|(member, _)| self.rank(&member_at(prefix, member)));
        ordered
    }

    /// The same, for a walk that reads only the NAMES of those members.
    pub(super) fn names<'a>(&self, prefix: &str, members: &'a Map<String, Value>) -> Vec<&'a String> {
        self.members(prefix, members)
            .into_iter()
            .map(|(member, _)| member)
            .collect()
    }

    /// Put `found` — what a walk found at paths RELATIVE to `prefix` — in the order the
    /// definition writes those paths.
    ///
    /// The paths are the ones the walk builds through [`nested_at`] from the object at
    /// `prefix`, so they resolve in this table exactly as a member's own location does.
    pub(super) fn paths<T>(&self, prefix: &str, found: &mut [(String, T)]) {
        found.sort_by_cached_key(|(path, _)| self.rank(&nested_at(prefix, path)));
    }
}

/// The location `member` of the object at `prefix` is reported at, which is what it is
/// ranked under: the one every diagnostic about that member states ([`at`]).
fn member_at(prefix: &str, member: &str) -> String {
    nested_at(prefix, format_args!("/{}", echoed(member)))
}

/// The table as it is being filled: what has been recorded, how many members were
/// written before the next one, and how much room is left.
struct Ranks {
    ranks: HashMap<String, usize>,
    written: usize,
    room: usize,
}

impl Ranks {
    /// Record that the member `member` of the object at `path` is the next thing
    /// written, and answer the location of its own value.
    fn record(&mut self, path: &str, member: &str) -> String {
        self.record_at(member_at(path, member))
    }

    /// Record that whatever the location `at` names is the next thing written, and
    /// answer that location.
    ///
    /// The FIRST writing of a location wins: two members whose locations were clipped
    /// onto one another are one entry, and the earlier of them is the one it holds.
    fn record_at(&mut self, at: String) -> String {
        self.written += 1;
        let rank = self.written;
        self.ranks.entry(at.clone()).or_insert(rank);
        self.room = self.room.saturating_sub(1);
        at
    }

    /// Whether the table's room is spent, which is where the scan stops reading.
    fn full(&self) -> bool {
        self.room == 0
    }
}

/// One node of the document being scanned: where it is, whether that place is one to
/// record (an element of an array records itself, because nothing above it named it),
/// and the table its members are recorded in.
///
/// It is a SEED rather than a value: deserializing it records the order of what it
/// reads and keeps nothing else, so the scan allocates the locations it records and no
/// copy of the document.
struct Scan<'a> {
    path: String,
    record: bool,
    into: &'a mut Ranks,
}

impl<'de> DeserializeSeed<'de> for Scan<'_> {
    type Value = ();

    fn deserialize<D>(mut self, reader: D) -> Result<(), D::Error>
    where
        D: Deserializer<'de>,
    {
        if self.into.full() {
            IgnoredAny::deserialize(reader)?;
            return Ok(());
        }
        if self.record {
            self.path = self.into.record_at(self.path);
        }
        reader.deserialize_any(self)
    }
}

impl<'de> Visitor<'de> for Scan<'_> {
    type Value = ();

    fn expecting(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        out.write_str("any JSON value")
    }

    /// An object: each member is recorded where it is written, then read for the
    /// members of its own.
    fn visit_map<A>(self, mut members: A) -> Result<(), A::Error>
    where
        A: MapAccess<'de>,
    {
        let Scan { path, into, .. } = self;
        while let Some(member) = members.next_key::<String>()? {
            if into.full() {
                members.next_value::<IgnoredAny>()?;
                continue;
            }
            let at = into.record(&path, &member);
            members.next_value_seed(Scan {
                path: at,
                record: false,
                into: &mut *into,
            })?;
        }
        Ok(())
    }

    /// An array: each element records its OWN indexed location — which is where a
    /// diagnostic about something inside it is reported — and is then read for the
    /// members nested in it. Only an element that is really there records one, because
    /// the seed records where it is as it is read.
    fn visit_seq<A>(self, mut elements: A) -> Result<(), A::Error>
    where
        A: SeqAccess<'de>,
    {
        let Scan { path, into, .. } = self;
        let mut index = 0;
        while elements
            .next_element_seed(Scan {
                path: indexed_at(&path, index),
                record: true,
                into: &mut *into,
            })?
            .is_some()
        {
            index += 1;
        }
        Ok(())
    }

    // A scalar carries no member, so it is read and nothing is recorded for it. Every
    // JSON scalar is answered, so no value of a definition ends the scan early.
    fn visit_unit<E: de::Error>(self) -> Result<(), E> {
        Ok(())
    }

    fn visit_bool<E: de::Error>(self, _value: bool) -> Result<(), E> {
        Ok(())
    }

    fn visit_i64<E: de::Error>(self, _value: i64) -> Result<(), E> {
        Ok(())
    }

    fn visit_u64<E: de::Error>(self, _value: u64) -> Result<(), E> {
        Ok(())
    }

    fn visit_i128<E: de::Error>(self, _value: i128) -> Result<(), E> {
        Ok(())
    }

    fn visit_u128<E: de::Error>(self, _value: u128) -> Result<(), E> {
        Ok(())
    }

    fn visit_f64<E: de::Error>(self, _value: f64) -> Result<(), E> {
        Ok(())
    }

    fn visit_str<E: de::Error>(self, _value: &str) -> Result<(), E> {
        Ok(())
    }

    fn visit_none<E: de::Error>(self) -> Result<(), E> {
        Ok(())
    }

    fn visit_some<D>(self, reader: D) -> Result<(), D::Error>
    where
        D: Deserializer<'de>,
    {
        self.deserialize(reader)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The order is the document's own, whatever the member names sort as: a table
    /// whose rows differ along where the members sit (the machine, a state, a template
    /// under one, an element of an array) and whether their names are written in
    /// sorted order, reversed, or mixed.
    #[test]
    fn members_are_ordered_as_the_document_writes_them() {
        // (case, the document, the object asked about as the walk locates it, the
        // RFC 6901 pointer that reads that object out of the parsed document, and the
        // order its members are written in)
        let rows: [(&str, &str, &str, &str, &[&str]); 6] = [
            (
                "members written in reverse of their sorted order",
                r#"{"z":1,"a":2,"m":3}"#,
                "",
                "",
                &["z", "a", "m"],
            ),
            (
                "members already written in sorted order",
                r#"{"a":1,"m":2,"z":3}"#,
                "",
                "",
                &["a", "m", "z"],
            ),
            (
                "a template under a state",
                r#"{"StartAt":"T","States":{"T":{"Output":{"error":1,"cause":2}}}}"#,
                "/States/T/Output",
                "/States/T/Output",
                &["error", "cause"],
            ),
            (
                "the states of a machine",
                r#"{"StartAt":"Second","States":{"Second":{},"First":{}}}"#,
                "/States",
                "/States",
                &["Second", "First"],
            ),
            (
                // The walk writes an array element's index onto the member that holds
                // the array, so the location is not the pointer that reads it.
                "an object inside an array element",
                r#"{"Catch":[{"Next":"x","ErrorEquals":["e"]}]}"#,
                "/Catch[0]",
                "/Catch/0",
                &["Next", "ErrorEquals"],
            ),
            (
                "a value that is not an object at all",
                r#"[1,2,3]"#,
                "",
                "",
                &[],
            ),
        ];

        for (case, document, object, pointer, expected) in rows {
            let order = MemberOrder::of(document);
            let parsed: Value = serde_json::from_str(document).expect("the row is JSON");
            let members = parsed
                .pointer(pointer)
                .and_then(Value::as_object)
                .cloned()
                .unwrap_or_default();
            let actual: Vec<&String> = order.names(object, &members);
            let actual: Vec<&str> = actual.into_iter().map(String::as_str).collect();
            assert_eq!(
                actual, expected,
                "case {case:?}: expected the members in the order {expected:?}, got {actual:?}"
            );
        }
    }

    /// A document the table has no room for keeps the parse's own order, and a document
    /// that stops parsing keeps the order of what was read before the fault.
    #[test]
    fn a_document_the_table_cannot_hold_keeps_the_order_it_had() {
        let members: Vec<String> = (0..MAX_RANKED_MEMBERS + 8)
            .map(|index| format!("\"m{index:05}\":{index}"))
            .collect();
        let document = format!("{{{}}}", members.join(","));
        let order = MemberOrder::of(&document);
        let parsed: Value = serde_json::from_str(&document).expect("the document is JSON");
        let object = parsed.as_object().expect("an object");
        let ordered = order.names("", object);
        assert_eq!(
            ordered.len(),
            object.len(),
            "every member is visited once, got {}",
            ordered.len()
        );
        // The first members are the ones the table has room for, in the order the
        // document writes them, which here is also their sorted order.
        assert_eq!(
            ordered.first().map(|member| member.as_str()),
            Some("m00000"),
            "expected the first member written, got {:?}",
            ordered.first()
        );

        let truncated = MemberOrder::of(r#"{"z":1,"a":2,"#);
        let parsed: Value =
            serde_json::from_str(r#"{"z":1,"a":2}"#).expect("the repaired document is JSON");
        let object = parsed.as_object().expect("an object");
        let ordered: Vec<&str> = truncated
            .names("", object)
            .into_iter()
            .map(String::as_str)
            .collect();
        assert_eq!(
            ordered,
            vec!["z", "a"],
            "expected what was read before the fault to keep its order, got {ordered:?}"
        );
    }

    /// A document nested deeper than the reader follows is read no further: the scan
    /// stops where the parse of the definition itself stops, and what was recorded
    /// before that keeps its order.
    #[test]
    fn a_document_nested_past_the_readers_ceiling_is_read_no_further() {
        let depth = 1_024;
        let document = format!("{}1{}", "{\"a\":".repeat(depth), "}".repeat(depth));
        let order = MemberOrder::of(&document);
        assert_eq!(
            order.rank("/a"),
            1,
            "expected the outermost member to be the first one written, got {}",
            order.rank("/a")
        );
    }
}
