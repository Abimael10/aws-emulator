// SPDX-License-Identifier: CC0-1.0

//! WHERE in a request body, as the client wrote it, the value of a member sits.
//!
//! A parse failure of the query DSL names that position — the line and the column the reader
//! had reached when it could not go on — and the position is a property of the BYTES the
//! client sent, not of the parsed document: two bodies that parse to the same query report
//! different positions when they are written out differently, and a client reading the message
//! is pointed at a place in the text it wrote. So the position is read off the source text
//! here rather than reconstructed from the parsed value.
//!
//! The reader reports the position of the TOKEN it is on, which for a member whose value it
//! could not make sense of is that value's first character — the `{` of the object it was
//! about to read.

/// WHERE the member `member` of `source` sits: the position its NAME begins at and the
/// position its VALUE begins at, both as a line and a column counted from 1, or `None` where
/// `source` names no such member.
///
/// The walk reads `source` as the JSON text it is: a member name is a STRING followed by a
/// colon, so a string carrying the same text as a VALUE — a document holding the name as one
/// of its field values — is not taken for the member.
pub(super) fn member_at(source: &str, member: &str) -> Option<Member> {
    let mut line = 1_usize;
    let mut column = 1_usize;
    let mut in_string = false;
    let mut escaped = false;
    let mut literal = String::new();
    let mut literal_at = (1_usize, 1_usize);
    // What the walk has read of the member it is looking for: its name, and then the colon
    // that makes that name a member rather than a value.
    let mut after_name: Option<(usize, usize)> = None;
    let mut name_at: Option<(usize, usize)> = None;
    for ch in source.chars() {
        if let (Some(name_at), false) = (name_at, ch.is_whitespace()) {
            return Some(Member {
                name_at,
                value_at: (line, column),
            });
        }
        if in_string {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_string = false;
                after_name = (literal == member).then_some(literal_at);
                literal.clear();
            } else {
                literal.push(ch);
            }
        } else if ch == '"' {
            in_string = true;
            after_name = None;
            literal.clear();
            literal_at = (line, column);
        } else if ch == ':' {
            name_at = after_name.take();
        } else if !ch.is_whitespace() {
            after_name = None;
        }
        if ch == '\n' {
            line = line.saturating_add(1);
            column = 1;
        } else {
            column = column.saturating_add(1);
        }
    }
    None
}

/// Where one member of a body sits in the text the client sent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Member {
    /// The position the member's NAME begins at: the quote that opens it, which is where a
    /// reader that found no place for the member had reached.
    pub(super) name_at: (usize, usize),
    /// The position the member's VALUE begins at: the first character of what the member was
    /// given, which is where a reader that was already reading that value had reached.
    pub(super) value_at: (usize, usize),
}

/// The position the VALUE of `member` begins at ([`member_at`]).
pub(super) fn member_value_at(source: &str, member: &str) -> Option<(usize, usize)> {
    Some(member_at(source, member)?.value_at)
}

/// The position the NAME of `member` begins at ([`member_at`]).
pub(super) fn member_name_at(source: &str, member: &str) -> Option<(usize, usize)> {
    Some(member_at(source, member)?.name_at)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One row of the tables below: the case, the body as sent, the member it is about, and
    /// where that member sits in the text.
    type Row = (
        &'static str,
        &'static str,
        &'static str,
        Option<(usize, usize)>,
    );

    /// RULE: the position answered is where the member's VALUE begins in the text as sent, so
    /// it moves with the text's own layout.
    #[test]
    fn a_members_value_is_located_in_the_text_as_sent() {
        // (case, the body as sent, the member, the position its value begins at)
        let rows: [Row; 6] = [
            (
                "the spacing a JSON writer puts after each colon",
                r#"{"query": {"no_such_query": {"name": "alpha"}}}"#,
                "no_such_query",
                Some((1, 29)),
            ),
            (
                "the same body written with no spacing at all",
                r#"{"query":{"no_such_query":{"name":"alpha"}}}"#,
                "no_such_query",
                Some((1, 27)),
            ),
            (
                "a body broken over lines",
                "{\n  \"query\": {\n    \"no_such_query\": {}\n  }\n}",
                "no_such_query",
                Some((3, 22)),
            ),
            (
                "a member whose value is not an object",
                r#"{"query": {"nope": 7}}"#,
                "nope",
                Some((1, 20)),
            ),
            (
                "the name carried as a VALUE rather than as a member",
                r#"{"query": {"term": {"kind": "nope"}}}"#,
                "nope",
                None,
            ),
            (
                "a member the body does not carry",
                r#"{"query": {"term": {"kind": "a"}}}"#,
                "absent",
                None,
            ),
        ];
        for (case, source, member, expected) in rows {
            let at = member_value_at(source, member);
            assert_eq!(
                at, expected,
                "case {case:?}: expected {expected:?}, got {at:?}"
            );
        }
    }

    /// RULE: the member's NAME is located at the quote that opens it, in the same text.
    #[test]
    fn a_members_name_is_located_at_the_quote_that_opens_it() {
        // (case, the body as sent, the member, the position its name begins at)
        let rows: [Row; 3] = [
            (
                "a member of a nested object",
                r#"{"query": {"bool": {"must_sometimes": [{"match_all": {}}]}}}"#,
                "must_sometimes",
                Some((1, 21)),
            ),
            (
                "the same body written with no spacing at all",
                r#"{"query":{"bool":{"must_sometimes":[]}}}"#,
                "must_sometimes",
                Some((1, 19)),
            ),
            (
                "a member the body does not carry",
                r#"{"query": {"bool": {"must": []}}}"#,
                "must_sometimes",
                None,
            ),
        ];
        for (case, source, member, expected) in rows {
            let at = member_name_at(source, member);
            assert_eq!(
                at, expected,
                "case {case:?}: expected {expected:?}, got {at:?}"
            );
        }
    }

    /// RULE: text this walk cannot find the member in answers no position — it reports where a
    /// member IS and never fails, whatever the text turns out to be.
    ///
    /// The text is what a caller sent, so the rows are the shapes a caller can send that no
    /// JSON writer produces: a body cut off mid-way, one whose keys carry escapes, one that is
    /// not JSON at all, and nothing at all.
    #[test]
    fn text_the_member_cannot_be_found_in_answers_no_position() {
        let rows: [(&str, &str, &str); 6] = [
            ("a body cut off after the member's name", r#"{"query": {"nope"#, "nope"),
            ("a body cut off at the colon", r#"{"query": {"nope":"#, "nope"),
            (
                "a key written with an escape",
                r#"{"query": {"no\u0070e": {}}}"#,
                "nope",
            ),
            (
                "a quote escaped inside a value",
                r#"{"query": {"term": {"kind": "\"nope\":"}}}"#,
                "nope",
            ),
            ("text that is not JSON at all", "nope: 1", "nope"),
            ("no text at all", "", "nope"),
        ];
        for (case, source, member) in rows {
            let at = member_value_at(source, member);
            assert!(
                at.is_none(),
                "case {case:?}: expected no position, got {at:?}"
            );
        }
    }
}
