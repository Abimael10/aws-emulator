// SPDX-License-Identifier: CC0-1.0

//! The segment document a READER is answered with: how a stored document is read
//! and written again, and what that keeps.
//!
//! `PutTraceSegments` stores the bytes it was handed and the readers that report
//! a segment (`BatchGetTraces$Segment$Document`,
//! `ListRetrievedTraces$Span$Document`) answer with the document that was
//! submitted: every member it carried, with the value it carried, in the order
//! it carried them. Nothing a reader answers is re-authored — no member is added
//! or dropped, no value is replaced by one the service computed, and in
//! particular no time the caller recorded is replaced by the service's own clock
//! — so a client that compares the document it submitted against the document it
//! reads back finds them the same, and every latency and ordering computation
//! over a replayed or backdated segment still holds.
//!
//! A document submitted ON ITS OWN is answered as the bytes it arrived as, so
//! nothing here is reached for it. The one document a reader has to WRITE is a
//! document that ADOPTED another ([`super::assembly`]): a trace is one tree, so
//! an entity submitted as its own subsegment document is answered for inside the
//! entity it named. That one is read and written through this module.
//!
//! Member ORDER survives that round trip because this crate declares
//! `serde_json`'s `preserve_order` feature in its own manifest: with the feature
//! off a JSON object is held as a SORTED map, and a document written again would
//! name every entity's members in an order no client ever sent — exactly the
//! re-authoring the rule above forbids. The feature is declared HERE rather than
//! inherited from whatever other crate a build happens to pull in, because the
//! answers of this service depend on it.
//!
//! What a written document does NOT keep is how its text was framed: the
//! insignificant whitespace between its tokens is gone and each number is
//! written in the shortest digits that read back as the same number, so an entity
//! that adopted a child reports `1000.0` for a `1.0E3` it was submitted with.
//! That is this service's own spelling of the same value, and it is reached only
//! for the document that adopts; one that adopted nothing is answered byte for
//! byte.
//!
//! Reading and writing both walk a document's own nesting, which is bounded
//! twice over: a document nested deeper than [`super::MAX_SUBSEGMENT_DEPTH`] is
//! refused where it is accepted, and `serde_json` refuses nesting past its own
//! recursion limit before a value is built at all — so neither walk recurses on a
//! depth a caller is free to choose.

/// One stored document, read as the value a reader assembles from, or `None` for
/// text that is not JSON.
///
/// Every stored document parsed at ingest, so a stored one is always read back;
/// a reader still has the `None` branch so no answer depends on that holding.
pub fn read_document(document: &str) -> Option<serde_json::Value> {
    serde_json::from_str(document).ok()
}

/// `value` as the document text a reader is answered with, or `None` for a value
/// no JSON document can carry (a number that is not finite), which nothing read
/// from a document ever is.
pub fn written_document(value: &serde_json::Value) -> Option<String> {
    serde_json::to_string(value).ok()
}

#[cfg(test)]
mod tests {
    /// A document read and written again carries its own members, with its own
    /// values, in its own order — whatever order that is. Each row is a
    /// different shape of document.
    #[test]
    fn a_document_written_again_keeps_its_members_and_their_order() {
        let rows: [(&str, &str); 6] = [
            (
                "the member order the segment documentation shows",
                r#"{"trace_id":"1-581cf771-a006649127e371903a2de979","id":"70de5b6f19ff9a0a","start_time":1478293361.271,"end_time":1478293361.449,"name":"svc"}"#,
            ),
            (
                "the same members, named in another order",
                r#"{"name":"svc","id":"70de5b6f19ff9a0a","trace_id":"1-581cf771-a006649127e371903a2de979","start_time":1478293361.271,"end_time":1478293361.449}"#,
            ),
            (
                "an http block and an annotation, each a nested object of its own",
                r#"{"id":"1111111111111111","name":"svc","start_time":100.5,"trace_id":"1-581cf771-a006649127e371903a2de979","end_time":100.75,"http":{"request":{"url":"http://localhost/","method":"GET"},"response":{"status":200}},"annotations":{"order_id":"o-1001","retries":2,"paid":true}}"#,
            ),
            (
                "an in-progress segment, which carries no end_time",
                r#"{"id":"2222222222222222","name":"svc","trace_id":"1-581cf771-a006649127e371903a2de979","start_time":100.5,"in_progress":true}"#,
            ),
            (
                "a segment carrying subsegments, lists and a null",
                r#"{"id":"3333333333333333","name":"svc","trace_id":"1-581cf771-a006649127e371903a2de979","start_time":100.0,"end_time":101.0,"subsegments":[{"id":"4444444444444444","name":"inner","start_time":100.1,"end_time":100.2,"namespace":"remote"}],"metadata":{"notes":["a","b"],"missing":null}}"#,
            ),
            (
                "a whole second, a negative number and text needing escapes",
                "{\"id\":\"5555555555555555\",\"name\":\"svc \\\"quoted\\\"\\n\",\"trace_id\":\"1-581cf771-a006649127e371903a2de979\",\"start_time\":1793915525.0,\"end_time\":1793915527.0,\"annotations\":{\"offset\":-1.5,\"zone\":\"eu-west-1é\"}}",
            ),
        ];
        for (case, submitted) in rows {
            let written = super::read_document(submitted)
                .as_ref()
                .and_then(super::written_document)
                .expect("the document is well formed");
            assert_eq!(
                written, submitted,
                "case {case:?}: a document written again must be the document it was read from, \
                 got {written}"
            );
        }
    }

    /// What a written document does not keep: the framing of its text. A number
    /// keeps its VALUE, written in the shortest digits that read back as that
    /// value, and the whitespace between tokens is gone — while every member,
    /// its order and every other value are the document's own. Each row frames
    /// one number, or the text around it, differently.
    #[test]
    fn a_number_written_again_keeps_its_value_in_the_shortest_spelling_of_it() {
        let rows: [(&str, &str, &str); 5] = [
            (
                "a power of ten",
                r#"{"id":"1111111111111111","start_time":1.0E3,"name":"svc"}"#,
                r#"{"id":"1111111111111111","start_time":1000.0,"name":"svc"}"#,
            ),
            (
                "a trailing zero",
                r#"{"id":"1111111111111111","start_time":1.50,"name":"svc"}"#,
                r#"{"id":"1111111111111111","start_time":1.5,"name":"svc"}"#,
            ),
            (
                "an epoch second with a fraction",
                r#"{"id":"1111111111111111","start_time":1478293361.271,"name":"svc"}"#,
                r#"{"id":"1111111111111111","start_time":1478293361.271,"name":"svc"}"#,
            ),
            (
                "a count, which is no fraction at all",
                r#"{"id":"1111111111111111","http":{"response":{"status":200}},"name":"svc"}"#,
                r#"{"id":"1111111111111111","http":{"response":{"status":200}},"name":"svc"}"#,
            ),
            (
                "the spaces a client framed its document with",
                r#"{ "id": "1111111111111111", "start_time": 100.5, "name": "svc" }"#,
                r#"{"id":"1111111111111111","start_time":100.5,"name":"svc"}"#,
            ),
        ];
        for (case, submitted, expected) in rows {
            let written = super::read_document(submitted)
                .as_ref()
                .and_then(super::written_document)
                .expect("the document is well formed");
            assert_eq!(
                written, expected,
                "case {case:?}: expected {expected}, got {written}"
            );
            let again = super::read_document(&written)
                .as_ref()
                .and_then(super::written_document)
                .unwrap_or_default();
            assert_eq!(
                again, written,
                "case {case:?}: writing an answered document again must not move it, got {again}"
            );
        }
    }

    /// Text that is no document is read as none, so a reader answers the bytes
    /// it holds rather than text no client can parse.
    #[test]
    fn text_that_is_not_json_is_read_as_no_document() {
        assert!(
            super::read_document("{\"trace_id\": ").is_none(),
            "unparseable text must be read as no document"
        );
    }
}
