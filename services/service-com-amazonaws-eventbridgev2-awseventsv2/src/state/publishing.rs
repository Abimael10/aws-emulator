// SPDX-License-Identifier: CC0-1.0

//! What this service's two publish operations SHARE: the per-entry refusal
//! vocabulary, how an entry's content type is read, how a raw entry's bytes are
//! presented to a subscriber, and the key an entry is deduplicated under.
//!
//! `PutEvents` and `PutRawEvents` append to the same log and deduplicate
//! against the same per-bus index ([`super::EventBus::deduplicated_events`]), so
//! each of these readings is written ONCE here rather than once per operation:
//! two copies could not drift apart and still agree on one index.

use super::RawPayload;

/// The `ErrorCode` an entry refused for the VALUE of one of its members
/// carries. `ErrorCode` is documented as "Error code identifying why a publish
/// batch entry failed", and this is the code the service identifies a failed
/// value constraint by.
///
/// The per-entry codes are free-form strings in the model (no enum enumerates
/// them), so the code of this class is named once here rather than written at
/// each refusal of either publish operation, which is what keeps the two
/// reporting one class the same way.
pub const VALIDATION_ERROR: &str = "ValidationError";

/// The deduplication key of an entry the caller deduplicates with its own
/// token: the token itself, in its own namespace so no caller-chosen value can
/// be mistaken for a content key.
pub fn token_deduplication_key(deduplication_id: &str) -> String {
    format!("id:{deduplication_id}")
}

/// A framed SHA-256 digest of an event's content — "a hash of the event
/// content", which is what `DeduplicationType`'s documentation says
/// `CONTENT_BASED` deduplication keys on.
///
/// Fixed-width whatever the size of the event, so the index holding these keys
/// costs one 32-byte digest per deduplicated event rather than a second copy of
/// the payload, and a lookup is a compare of two short strings. The index is
/// pruned with the log, so it stays bounded by
/// [`super::MAX_STORED_EVENTS_PER_BUS`].
///
/// Each field is absorbed length-prefixed and an absent one as a distinct
/// marker, so no two different events can frame to the same bytes.
#[derive(Default)]
pub struct ContentDigest {
    hasher: ::sha2::Sha256,
}

impl ContentDigest {
    /// A digest nothing has been absorbed into yet.
    pub fn new() -> Self {
        Self::default()
    }

    /// Absorb one optional text field.
    pub fn absorb_text(&mut self, field: Option<&str>) {
        self.absorb_bytes(field.map(str::as_bytes));
    }

    /// Absorb one optional byte field.
    pub fn absorb_bytes(&mut self, field: Option<&[u8]>) {
        use ::sha2::Digest;

        match field {
            None => self.hasher.update([0u8]),
            Some(value) => {
                self.hasher.update([1u8]);
                self.absorb_count(value.len());
                self.hasher.update(value);
            }
        }
    }

    /// Absorb how many members a collection holds, so its SIZE is part of the
    /// digest and not only the fields it contributes.
    pub fn absorb_count(&mut self, count: usize) {
        use ::sha2::Digest;

        self.hasher
            .update(u64::try_from(count).unwrap_or(u64::MAX).to_be_bytes());
    }

    /// The deduplication key this digest names, in its own namespace.
    pub fn key(self) -> String {
        use ::sha2::Digest;

        format!("sha256:{}", ::hex::encode(self.hasher.finalize()))
    }
}

/// The subtype of `content_type` — what follows the `/`, with any parameters
/// (`; charset=utf-8`) dropped and the case folded, since a media type's type
/// and subtype are case-insensitive.
fn subtype(content_type: &str) -> String {
    content_type
        .split(';')
        .next()
        .unwrap_or_default()
        .split_once('/')
        .map_or("", |(_, subtype)| subtype)
        .trim()
        .to_ascii_lowercase()
}

/// The subtype of a content type that declares JSON text directly.
const JSON_SUBTYPE: &str = "json";

/// The structured-syntax suffix (RFC 6839) a content type declares JSON text
/// with — `application/cloudevents+json`, the example
/// `PutRawEventsSystemMetadata$ContentType`'s own documentation gives.
const JSON_SUFFIX: &str = "+json";

/// Whether `content_type` declares JSON text: its subtype is `json`, or carries
/// the `+json` structured-syntax suffix.
pub fn is_json_content_type(content_type: &str) -> bool {
    let subtype = subtype(content_type);
    subtype == JSON_SUBTYPE || subtype.ends_with(JSON_SUFFIX)
}

/// The open formats a request's `SchemaRegistryConfiguration` is for, as each
/// appears in a content type's subtype.
///
/// From that shape's own documentation: "Schema registry settings used to
/// encode open-format (Avro/Protobuf) events". Matched as a TOKEN of the
/// subtype, because each format is written under several registered names
/// (`application/avro`, `application/vnd.apache.avro+binary`,
/// `application/x-protobuf`, `application/vnd.google.protobuf`), every one of
/// which names the format as one of the subtype's own words — while a subtype
/// that merely CONTAINS the word (`application/vnd.avrolike+json`) names
/// another format entirely.
const OPEN_FORMATS: [&str; 2] = ["avro", "protobuf"];

/// The separators a media type's subtype is built from: a facet tree
/// (`vnd.apache.avro`), a prefix (`x-protobuf`) and a structured-syntax suffix
/// (`+binary`).
const SUBTYPE_SEPARATORS: [char; 3] = ['.', '+', '-'];

/// Whether `content_type` declares one of the open formats a schema registry
/// encodes — the entries `SchemaRegistryConfiguration` is "required for", and
/// which cannot be published without one.
///
/// A content type that declares JSON TEXT is never one of them, whichever
/// format its other tokens name (`application/avro+json` is a JSON document
/// the publisher has already encoded), because such a payload holds no
/// registry-held schema to read.
pub fn is_open_format_content_type(content_type: &str) -> bool {
    if is_json_content_type(content_type) {
        return false;
    }
    subtype(content_type)
        .split(SUBTYPE_SEPARATORS)
        .any(|token| OPEN_FORMATS.contains(&token))
}

/// Whether `data` is ONE JSON document: UTF-8 text that parses as a document
/// and nothing beside it.
///
/// What an entry declaring a JSON content type is held to before it is stored,
/// so bytes that are not the form their own `ContentType` declares are refused
/// in that entry's position rather than admitted to the log and later delivered
/// as a document nobody can read.
pub fn is_json_document(data: &[u8]) -> bool {
    json_document(data).is_some()
}

/// `data` as the one JSON document it spells, or `None` when it is not one.
///
/// The ONE reading of a published payload as a document, so the structured
/// publish's `Detail` and the raw publish's `Data` cannot come to disagree about
/// what a JSON payload is.
pub fn json_document(data: &[u8]) -> Option<::serde_json::Value> {
    ::serde_json::from_str(json_utf8(data)?).ok()
}

/// `data` as the text of the one JSON document it spells, or `None` when it is
/// not one — the payload AS SENT, for a delivery that hands the target the
/// bytes untouched.
fn json_text(data: &[u8]) -> Option<&str> {
    let text = json_utf8(data)?;
    ::serde_json::from_str::<::serde_json::Value>(text).ok()?;
    Some(text)
}

/// `data` as the text it is, or `None` for bytes that are no UTF-8 text.
fn json_utf8(data: &[u8]) -> Option<&str> {
    ::std::str::from_utf8(data).ok()
}

impl RawPayload {
    /// This payload as a subscriber READS it: the `DATA` a filter of that scope
    /// is matched against, and the payload a delivered event carries.
    ///
    /// The bytes of an entry whose `ContentType` declares JSON are the document
    /// they spell, because a `DATA`-scoped filter clause names members INSIDE
    /// the payload. Any other bytes are "the event data as a base64-encoded
    /// blob … Supports binary formats" (`PutRawEventsRequestEntry$Data`) —
    /// binary this service does not interpret — so they are read as the base64
    /// text of the bytes as sent, which is a value both a filter and a target
    /// can read.
    pub fn data_value(&self) -> ::serde_json::Value {
        if is_json_content_type(&self.content_type) {
            if let Some(document) = json_document(&self.data) {
                return document;
            }
        }
        ::serde_json::Value::String(self.base64_data())
    }

    /// The text a target is handed for this payload when the subscriber asks
    /// for no transformation: the payload AS SENT for data whose content type
    /// declares text this service reads, and the base64 of the bytes as sent
    /// for data it does not — the same rendering [`Self::data_value`] reads.
    pub fn untransformed_text(&self) -> String {
        if is_json_content_type(&self.content_type) {
            if let Some(text) = json_text(&self.data) {
                return text.to_string();
            }
        }
        self.base64_data()
    }

    /// The publisher's own `Metadata` as a document: the `METADATA` a filter of
    /// that scope is matched against, and what a delivered event carries under
    /// that member. Every value is the text the publisher sent, because
    /// `EventMetadataValue` is a string.
    pub fn metadata_document(&self) -> ::serde_json::Value {
        ::serde_json::Value::Object(
            self.metadata
                .iter()
                .map(|(name, value)| {
                    (
                        name.clone(),
                        ::serde_json::Value::String(value.clone()),
                    )
                })
                .collect(),
        )
    }

    /// The bytes as sent, base64 encoded — the form `Data`'s own documentation
    /// names them in.
    fn base64_data(&self) -> String {
        ::aws_smithy_types::base64::encode(&self.data)
    }
}

#[cfg(test)]
mod tests {
    use super::{is_json_content_type, is_json_document, is_open_format_content_type};

    /// A content type declares JSON when its subtype is `json` or carries the
    /// `+json` structured-syntax suffix, whatever parameters follow it and
    /// whatever case it is written in.
    #[test]
    fn a_json_content_type_is_read_from_its_subtype() {
        // (case, the content type, whether it declares JSON)
        let rows: [(&str, &str, bool); 10] = [
            ("the JSON media type", "application/json", true),
            // The example `ContentType`'s own documentation gives.
            (
                "a structured-syntax suffix",
                "application/cloudevents+json",
                true,
            ),
            ("a suffix on a vendor tree", "application/vnd.api+json", true),
            ("parameters after the subtype", "application/json; charset=utf-8", true),
            ("upper case", "APPLICATION/JSON", true),
            ("binary", "application/octet-stream", false),
            ("xml", "application/xml", false),
            // `json` must be the whole subtype or the suffix, not a substring.
            ("a subtype merely containing json", "application/jsonish", false),
            ("avro", "application/avro", false),
            ("no subtype at all", "application", false),
        ];
        for (case, content_type, expected) in rows {
            let read = is_json_content_type(content_type);
            assert_eq!(
                read, expected,
                "case {case:?}: expected is_json_content_type({content_type:?}) to be \
                 {expected}, got {read}"
            );
        }
    }

    /// A content type declares an open format when one of its subtype's own
    /// words names one, and never when it declares JSON text instead.
    #[test]
    fn an_open_format_content_type_is_one_a_schema_registry_encodes() {
        // (case, the content type, whether it is an open format)
        let rows: [(&str, &str, bool); 11] = [
            ("avro", "application/avro", true),
            (
                "the vendor avro name",
                "application/vnd.apache.avro+binary",
                true,
            ),
            ("protobuf", "application/protobuf", true),
            ("the x- protobuf name", "application/x-protobuf", true),
            (
                "the vendor protobuf name",
                "application/vnd.google.protobuf",
                true,
            ),
            (
                "avro with parameters",
                "application/avro; charset=binary",
                true,
            ),
            // The format must be one of the subtype's own words.
            (
                "a subtype merely containing avro",
                "application/vnd.avrolike+json",
                false,
            ),
            // JSON text the publisher has already encoded needs no registry.
            ("the avro JSON encoding", "application/avro+json", false),
            ("json", "application/json", false),
            ("binary", "application/octet-stream", false),
            ("xml", "application/xml", false),
        ];
        for (case, content_type, expected) in rows {
            let read = is_open_format_content_type(content_type);
            assert_eq!(
                read, expected,
                "case {case:?}: expected is_open_format_content_type({content_type:?}) to be \
                 {expected}, got {read}"
            );
        }
    }

    /// One JSON document is UTF-8 text that parses whole; anything else is not
    /// one, bytes that are no text included.
    #[test]
    fn one_json_document_is_utf8_text_that_parses_whole() {
        // (case, the bytes, whether they are one JSON document)
        let rows: [(&str, &[u8], bool); 9] = [
            ("an object", br#"{"a":1}"#, true),
            ("an empty object", b"{}", true),
            ("an array", b"[1,2,3]", true),
            ("a scalar", b"42", true),
            ("surrounding whitespace", b" {\n\"a\": 1\n}\n", true),
            ("a truncated object", br#"{"a":1"#, false),
            ("two documents", b"{} {}", false),
            ("no bytes at all", b"", false),
            ("bytes that are no UTF-8 text", &[0xFF, 0xFE, 0x00], false),
        ];
        for (case, data, expected) in rows {
            let read = is_json_document(data);
            assert_eq!(
                read, expected,
                "case {case:?}: expected is_json_document({data:?}) to be {expected}, got {read}"
            );
        }
    }
}
