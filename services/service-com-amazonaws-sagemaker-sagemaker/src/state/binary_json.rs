// SPDX-License-Identifier: CC0-1.0

//! The BINARY form of a JSON document, decoded into the SAME document its text form parses to.
//!
//! # Why this reading exists
//!
//! A serialized gradient-boosted model ([`super::boosted_model`]) is one document in two
//! interchangeable encodings: the text JSON a `.json` artifact carries, and the universal
//! binary JSON a `.ubj` one carries — the encoding the boosting library writes by default. An
//! endpoint deployed with either artifact has to be served from the same parameters, so both
//! encodings are decoded here into one `::serde_json::Value` and read ONCE, by the one reading
//! of the model's own members.
//!
//! The text side is the maintained JSON parser. The binary side is decoded here because this
//! registry holds no maintained decoder for it: the one universal-binary-JSON crate that
//! carries a serde implementation only WRITES the encoding, and the one that reads it exposes
//! its byte markers alone, behind a dependency chain this project does not take. So this is the
//! deliberate exception, and it is written as an untrusted-input reader:
//!
//! * the input is bounded before a byte is read ([`MAX_BINARY_JSON_BYTES`]);
//! * the WHOLE document is bounded, not only each container: a document may produce at most one
//!   value per input byte, and never more than [`MAX_BINARY_JSON_VALUES`] of them, so the
//!   element counts of nested containers cannot multiply into an allocation the input size does
//!   not pay for;
//! * nesting is bounded ([`MAX_BINARY_JSON_DEPTH`]) and the bound is checked BEFORE descending,
//!   so a document of any depth costs at most that many frames;
//! * every container's element count is bounded ([`MAX_BINARY_JSON_ITEMS`]), is never allocated
//!   against before its elements are read, and may not exceed the bytes left to read — and the
//!   optimized form admits only element types that CONSUME input, so a declared count can never
//!   stand for more values than the remaining bytes could carry;
//! * anything the encoding does not admit — an unknown marker, a truncated value, a length
//!   that is not a whole number, bytes left over after the document — is NO document at all
//!   rather than a partial one.
//!
//! # The encoding
//!
//! Each value starts with a one-byte marker: `Z` nothing, `T`/`F` the two truth values, `i`/`U`
//! an 8-bit integer, `I` a 16-bit one, `l` a 32-bit one, `L` a 64-bit one, `d`/`D` a 32- or
//! 64-bit floating-point number, `C` one character, `S` a string, `H` a number carried as text,
//! `[` an array and `{` an object. Every number is big-endian. A string, and an object's key,
//! is an integer length followed by that many bytes of text. An array or an object may declare
//! the TYPE of every element (`$`) and how many elements it holds (`#`), which is the optimized
//! form a list of numbers is written in; where it declares neither, it ends at its own closing
//! marker.

// Nothing of the parent module is needed here: this reading is over bytes alone. It lives
// beside the model reading that uses it ([`super::boosted_model`]) because that is the only
// artifact encoding this service decodes.

/// The most bytes one binary document may carry. The same ceiling the inflated side of a model
/// archive is bounded by ([`super::MAX_INFLATED_ARCHIVE_BYTES`]), stated for this reading too so
/// it holds whatever hands it bytes.
pub const MAX_BINARY_JSON_BYTES: usize = 4 * 1024 * 1024;

/// The most levels of nesting one binary document may carry.
pub const MAX_BINARY_JSON_DEPTH: usize = 64;

/// The most elements one array or object of a binary document may carry.
pub const MAX_BINARY_JSON_ITEMS: usize = 1 << 21;

/// The most VALUES one whole document may produce, however its containers are nested.
///
/// The budget a document is read against is the smaller of this and its own length in bytes: in
/// the plain forms every value costs at least one byte, so one value per input byte is the
/// ceiling the encoding itself implies, and holding the whole document to it is what stops the
/// element counts of nested containers from multiplying into memory the input never paid for.
pub const MAX_BINARY_JSON_VALUES: usize = 1 << 20;

/// The document the binary JSON `bytes` carry, or `None` for bytes that are not one.
///
/// Bytes left over after the document are a refusal: a document is the WHOLE of what was
/// handed over, so trailing bytes mean this is some other encoding that happens to start like
/// one.
#[must_use]
pub fn binary_json_document(bytes: &[u8]) -> Option<::serde_json::Value> {
    if bytes.is_empty() || bytes.len() > MAX_BINARY_JSON_BYTES {
        return None;
    }
    let mut reader = BinaryJson {
        bytes,
        at: 0,
        remaining: bytes.len().min(MAX_BINARY_JSON_VALUES),
    };
    let document = reader.value(0)?;
    reader.skip_nothings();
    (reader.at == bytes.len()).then_some(document)
}

/// The markers the encoding gives its value types.
mod marker {
    pub const NOTHING: u8 = b'Z';
    pub const NO_OP: u8 = b'N';
    pub const TRUE: u8 = b'T';
    pub const FALSE: u8 = b'F';
    pub const INT8: u8 = b'i';
    pub const UINT8: u8 = b'U';
    pub const INT16: u8 = b'I';
    pub const INT32: u8 = b'l';
    pub const INT64: u8 = b'L';
    pub const FLOAT32: u8 = b'd';
    pub const FLOAT64: u8 = b'D';
    pub const HIGH_PRECISION: u8 = b'H';
    pub const CHAR: u8 = b'C';
    pub const STRING: u8 = b'S';
    pub const ARRAY_OPEN: u8 = b'[';
    pub const ARRAY_CLOSE: u8 = b']';
    pub const OBJECT_OPEN: u8 = b'{';
    pub const OBJECT_CLOSE: u8 = b'}';
    /// What declares the TYPE of every element of a container.
    pub const ELEMENT_TYPE: u8 = b'$';
    /// What declares HOW MANY elements a container holds.
    pub const ELEMENT_COUNT: u8 = b'#';
}

/// A position in the bytes being decoded, and what is left of the document's value budget.
struct BinaryJson<'bytes> {
    bytes: &'bytes [u8],
    at: usize,
    /// How many more values this document may produce (see [`MAX_BINARY_JSON_VALUES`]).
    remaining: usize,
}

impl BinaryJson<'_> {
    /// The next byte, consumed.
    fn byte(&mut self) -> Option<u8> {
        let byte = self.bytes.get(self.at).copied()?;
        self.at += 1;
        Some(byte)
    }

    /// The next byte, left where it is.
    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.at).copied()
    }

    /// The next `count` bytes, consumed.
    fn take(&mut self, count: usize) -> Option<&[u8]> {
        let end = self.at.checked_add(count)?;
        let read = self.bytes.get(self.at..end)?;
        self.at = end;
        Some(read)
    }

    /// Step over the no-op markers a container may carry between its elements, which stand for
    /// no value at all.
    fn skip_nothings(&mut self) {
        while self.peek() == Some(marker::NO_OP) {
            self.at += 1;
        }
    }

    /// Charge one value to the document's budget, or answer `None` once it is spent — which is
    /// the bound on the WHOLE document, not on any one container.
    fn charge(&mut self) -> Option<()> {
        self.remaining = self.remaining.checked_sub(1)?;
        Some(())
    }

    /// The value at this position, at nesting level `depth`.
    fn value(&mut self, depth: usize) -> Option<::serde_json::Value> {
        self.skip_nothings();
        let marker = self.byte()?;
        self.typed_value(marker, depth)
    }

    /// The value of type `marker` at this position — the marker having been read already, which
    /// is what lets a container declare the type of its elements once.
    fn typed_value(&mut self, marker: u8, depth: usize) -> Option<::serde_json::Value> {
        // Every value the document produces passes here, so this is where the document-wide
        // budget is spent.
        self.charge()?;
        match marker {
            marker::NOTHING => Some(::serde_json::Value::Null),
            marker::TRUE => Some(::serde_json::Value::Bool(true)),
            marker::FALSE => Some(::serde_json::Value::Bool(false)),
            marker::INT8 => Some(integer(i64::from(self.take(1)?[0] as i8))),
            marker::UINT8 => Some(integer(i64::from(self.take(1)?[0]))),
            marker::INT16 => {
                let read: [u8; 2] = self.take(2)?.try_into().ok()?;
                Some(integer(i64::from(i16::from_be_bytes(read))))
            }
            marker::INT32 => {
                let read: [u8; 4] = self.take(4)?.try_into().ok()?;
                Some(integer(i64::from(i32::from_be_bytes(read))))
            }
            marker::INT64 => {
                let read: [u8; 8] = self.take(8)?.try_into().ok()?;
                Some(integer(i64::from_be_bytes(read)))
            }
            marker::FLOAT32 => {
                let read: [u8; 4] = self.take(4)?.try_into().ok()?;
                Some(decimal(f64::from(f32::from_be_bytes(read))))
            }
            marker::FLOAT64 => {
                let read: [u8; 8] = self.take(8)?.try_into().ok()?;
                Some(decimal(f64::from_be_bytes(read)))
            }
            // A character is one byte of text, and a number carried as text stays text: the
            // reading that wants a number out of a member takes it from either form.
            marker::CHAR => Some(::serde_json::Value::String(
                ::std::str::from_utf8(self.take(1)?).ok()?.to_string(),
            )),
            marker::STRING | marker::HIGH_PRECISION => {
                Some(::serde_json::Value::String(self.text()?))
            }
            marker::ARRAY_OPEN => self.array(depth),
            marker::OBJECT_OPEN => self.object(depth),
            // Every other byte is no value this encoding declares, so these bytes are no
            // document.
            _ => None,
        }
    }

    /// The text at this position: an integer length, then that many bytes of it.
    fn text(&mut self) -> Option<String> {
        let length = self.length()?;
        Some(::std::str::from_utf8(self.take(length)?).ok()?.to_string())
    }

    /// A whole number at this position, as a length or a count: only the integer types carry
    /// one, and only a non-negative value inside the element bound is one.
    fn length(&mut self) -> Option<usize> {
        let marker = self.byte()?;
        let read = match marker {
            marker::INT8 => i64::from(self.take(1)?[0] as i8),
            marker::UINT8 => i64::from(self.take(1)?[0]),
            marker::INT16 => i64::from(i16::from_be_bytes(self.take(2)?.try_into().ok()?)),
            marker::INT32 => i64::from(i32::from_be_bytes(self.take(4)?.try_into().ok()?)),
            marker::INT64 => i64::from_be_bytes(self.take(8)?.try_into().ok()?),
            _ => return None,
        };
        let read = usize::try_from(read).ok()?;
        (read <= MAX_BINARY_JSON_ITEMS.max(MAX_BINARY_JSON_BYTES)).then_some(read)
    }

    /// What a container declares about its elements: the type every element is of, and how many
    /// of them there are.
    ///
    /// An element type that consumes NO input ([`consumes_no_input`]) is refused here: with it,
    /// a declared count would stand for values the remaining bytes never carried. For the same
    /// reason a count may not exceed the bytes left to read, since every element of a counted
    /// container costs at least one of them.
    fn declaration(&mut self) -> Option<(Option<u8>, Option<usize>)> {
        let element_type = if self.peek() == Some(marker::ELEMENT_TYPE) {
            self.at += 1;
            Some(self.byte()?)
        } else {
            None
        };
        if element_type.is_some_and(consumes_no_input) {
            return None;
        }
        let count = if self.peek() == Some(marker::ELEMENT_COUNT) {
            self.at += 1;
            Some(self.length()?)
        } else {
            None
        };
        // A container that declares the type of its elements has to declare how many there
        // are: with no closing marker to look for, nothing else says where it ends.
        if element_type.is_some() && count.is_none() {
            return None;
        }
        if count.is_some_and(|count| count > MAX_BINARY_JSON_ITEMS) {
            return None;
        }
        let left = self.bytes.len().saturating_sub(self.at);
        if count.is_some_and(|count| count > left) {
            return None;
        }
        Some((element_type, count))
    }

    /// The array at this position, its opening marker having been read.
    fn array(&mut self, depth: usize) -> Option<::serde_json::Value> {
        // The bound is checked BEFORE descending, so the frames one document costs are bounded
        // by the constant whatever it declares.
        if depth >= MAX_BINARY_JSON_DEPTH {
            return None;
        }
        let (element_type, count) = self.declaration()?;
        let mut items = Vec::new();
        match count {
            Some(count) => {
                for _ in 0..count {
                    items.push(match element_type {
                        Some(element_type) => self.typed_value(element_type, depth + 1)?,
                        None => self.value(depth + 1)?,
                    });
                }
            }
            None => loop {
                self.skip_nothings();
                if self.peek()? == marker::ARRAY_CLOSE {
                    self.at += 1;
                    break;
                }
                if items.len() >= MAX_BINARY_JSON_ITEMS {
                    return None;
                }
                items.push(self.value(depth + 1)?);
            },
        }
        Some(::serde_json::Value::Array(items))
    }

    /// The object at this position, its opening marker having been read.
    fn object(&mut self, depth: usize) -> Option<::serde_json::Value> {
        if depth >= MAX_BINARY_JSON_DEPTH {
            return None;
        }
        let (element_type, count) = self.declaration()?;
        let mut members = ::serde_json::Map::new();
        match count {
            Some(count) => {
                for _ in 0..count {
                    // A member's name is text with no marker in front of it: the length comes
                    // first.
                    let name = self.text()?;
                    let value = match element_type {
                        Some(element_type) => self.typed_value(element_type, depth + 1)?,
                        None => self.value(depth + 1)?,
                    };
                    members.insert(name, value);
                }
            }
            None => loop {
                self.skip_nothings();
                if self.peek()? == marker::OBJECT_CLOSE {
                    self.at += 1;
                    break;
                }
                if members.len() >= MAX_BINARY_JSON_ITEMS {
                    return None;
                }
                let name = self.text()?;
                let value = self.value(depth + 1)?;
                members.insert(name, value);
            },
        }
        Some(::serde_json::Value::Object(members))
    }
}

/// Whether a value of type `marker` is carried by its marker ALONE, with no bytes after it:
/// the two truth values, nothing at all, and the no-op. A container may not declare one of
/// these as the type of its elements (see [`BinaryJson::declaration`]).
fn consumes_no_input(marker: u8) -> bool {
    matches!(
        marker,
        marker::NOTHING | marker::NO_OP | marker::TRUE | marker::FALSE
    )
}

/// A whole number as the document carries it.
fn integer(value: i64) -> ::serde_json::Value {
    ::serde_json::Value::Number(::serde_json::Number::from(value))
}

/// A floating-point number as the document carries it; one that is not finite is no JSON number
/// at all, so it reads as nothing rather than as a value it is not.
fn decimal(value: f64) -> ::serde_json::Value {
    match ::serde_json::Number::from_f64(value) {
        Some(number) => ::serde_json::Value::Number(number),
        None => ::serde_json::Value::Null,
    }
}

#[cfg(test)]
mod tests {
    use super::{binary_json_document, MAX_BINARY_JSON_DEPTH};

    /// One member of a binary object: the name's length, the name, then the value's bytes.
    fn member(name: &str, value: &[u8]) -> Vec<u8> {
        let mut bytes = vec![b'i', name.len() as u8];
        bytes.extend_from_slice(name.as_bytes());
        bytes.extend_from_slice(value);
        bytes
    }

    /// A string value.
    fn text(value: &str) -> Vec<u8> {
        let mut bytes = vec![b'S', b'i', value.len() as u8];
        bytes.extend_from_slice(value.as_bytes());
        bytes
    }

    /// An array of 32-bit floating-point numbers in the OPTIMIZED form: the element type and
    /// the count declared once, then the values.
    fn float_array(values: &[f32]) -> Vec<u8> {
        let mut bytes = vec![b'[', b'$', b'd', b'#', b'i', values.len() as u8];
        for value in values {
            bytes.extend_from_slice(&value.to_be_bytes());
        }
        bytes
    }

    /// RULE: the binary encoding decodes to the same document its text form parses to.
    ///
    /// The rows span the encoding's own value types: the two truth values, nothing at all, each
    /// integer width, both floating-point widths, text, a number carried as text, a character,
    /// an array that ends at its closing marker, an optimized array of numbers, and nesting.
    #[test]
    fn the_binary_encoding_decodes_to_the_document_it_carries() {
        // (case, the bytes, the document they carry)
        let rows: Vec<(&str, Vec<u8>, ::serde_json::Value)> = vec![
            ("nothing at all", vec![b'Z'], ::serde_json::json!(null)),
            ("the true value", vec![b'T'], ::serde_json::json!(true)),
            ("the false value", vec![b'F'], ::serde_json::json!(false)),
            ("an 8-bit integer", vec![b'i', 0xfb], ::serde_json::json!(-5)),
            ("an unsigned 8-bit integer", vec![b'U', 0xfb], ::serde_json::json!(251)),
            (
                "a 16-bit integer",
                vec![b'I', 0x01, 0x00],
                ::serde_json::json!(256),
            ),
            (
                "a 32-bit integer",
                vec![b'l', 0x00, 0x01, 0x00, 0x00],
                ::serde_json::json!(65_536),
            ),
            (
                "a 64-bit integer",
                vec![b'L', 0, 0, 0, 0, 0, 0, 0, 7],
                ::serde_json::json!(7),
            ),
            (
                "a 32-bit floating-point number",
                vec![b'd', 0x3f, 0x00, 0x00, 0x00],
                ::serde_json::json!(0.5),
            ),
            (
                "a 64-bit floating-point number",
                vec![b'D', 0x3f, 0xe0, 0, 0, 0, 0, 0, 0],
                ::serde_json::json!(0.5),
            ),
            ("a string", text("binary:logistic"), ::serde_json::json!("binary:logistic")),
            (
                "a number carried as text",
                vec![b'H', b'i', 4, b'5', b'E', b'-', b'1'],
                ::serde_json::json!("5E-1"),
            ),
            ("a character", vec![b'C', b'x'], ::serde_json::json!("x")),
            (
                "an array that ends at its closing marker",
                vec![b'[', b'i', 1, b'i', 2, b']'],
                ::serde_json::json!([1, 2]),
            ),
            (
                "an optimized array of numbers",
                float_array(&[0.5, -2.0]),
                ::serde_json::json!([0.5, -2.0]),
            ),
            (
                "an object of several members",
                {
                    let mut bytes = vec![b'{'];
                    bytes.extend(member("name", &text("gbtree")));
                    bytes.extend(member("weights", &float_array(&[1.5, 0.0])));
                    bytes.push(b'}');
                    bytes
                },
                ::serde_json::json!({ "name": "gbtree", "weights": [1.5, 0.0] }),
            ),
            (
                "an object declaring how many members it holds",
                {
                    let mut bytes = vec![b'{', b'#', b'i', 1];
                    bytes.extend(member("name", &text("gblinear")));
                    bytes
                },
                ::serde_json::json!({ "name": "gblinear" }),
            ),
            (
                "nested objects and arrays",
                {
                    let mut inner = vec![b'{'];
                    inner.extend(member("trees", &{
                        let mut trees = vec![b'['];
                        trees.extend({
                            let mut tree = vec![b'{'];
                            tree.extend(member("base_weights", &float_array(&[1.0])));
                            tree.push(b'}');
                            tree
                        });
                        trees.push(b']');
                        trees
                    }));
                    inner.push(b'}');
                    let mut bytes = vec![b'{'];
                    bytes.extend(member("model", &inner));
                    bytes.push(b'}');
                    bytes
                },
                ::serde_json::json!({ "model": { "trees": [{ "base_weights": [1.0] }] } }),
            ),
        ];
        for (case, bytes, expected) in rows {
            let read = binary_json_document(&bytes);
            assert_eq!(
                read.as_ref(),
                Some(&expected),
                "case {case:?}: expected {expected}, got {read:?}"
            );
        }
    }

    /// RULE: bytes the encoding does not admit are NO document — never a partial one.
    ///
    /// The rows span how bytes fall short: empty, an unknown marker, a value cut off mid-way, a
    /// container that never closes, a declared count the bytes do not carry, a length that is
    /// not a whole number, text that is not valid, bytes left over after the document, and
    /// nesting past the bound.
    #[test]
    fn bytes_the_encoding_does_not_admit_are_no_document() {
        let past_the_depth_bound: Vec<u8> = ::std::iter::repeat_n(b'[', MAX_BINARY_JSON_DEPTH + 2)
            .chain(::std::iter::repeat_n(b']', MAX_BINARY_JSON_DEPTH + 2))
            .collect();
        let rows: Vec<(&str, Vec<u8>)> = vec![
            ("no bytes at all", Vec::new()),
            ("an unknown marker", vec![b'Q']),
            ("a number cut off mid-way", vec![b'l', 0x00, 0x01]),
            ("a container that never closes", vec![b'[', b'i', 1]),
            ("a declared count the bytes do not carry", vec![b'[', b'$', b'd', b'#', b'i', 4]),
            ("a count declared with no element type", vec![b'[', b'$', b'd']),
            ("a length that is not a whole number", vec![b'S', b'd', 0, 0, 0, 0]),
            ("text that is not valid", vec![b'S', b'i', 2, 0xff, 0xfe]),
            ("bytes left over after the document", vec![b'T', b'T']),
            ("an object with no value for its member", vec![b'{', b'i', 1, b'a', b'}']),
            ("nesting past the bound", past_the_depth_bound),
            (
                // The amplification the document-wide budget exists for: a nine-byte header
                // declaring the largest count of a value type that carries no bytes of its own.
                "a declared count of values that carry no bytes",
                vec![b'[', b'$', b'Z', b'#', b'l', 0x00, 0x20, 0x00, 0x00],
            ),
            (
                "the same with a truth value as the element type",
                vec![b'[', b'$', b'T', b'#', b'l', 0x00, 0x20, 0x00, 0x00],
            ),
            (
                "a declared count past the bytes that could carry it",
                vec![b'[', b'$', b'd', b'#', b'l', 0x00, 0x20, 0x00, 0x00],
            ),
            (
                // Many such headers packed into one document: the budget is the document's, so
                // the total is bounded however the counts are spread over containers.
                "an outer array packed with counted containers",
                {
                    let mut bytes = vec![b'['];
                    for _ in 0..512 {
                        bytes.extend_from_slice(&[b'[', b'$', b'i', b'#', b'I', 0x7f, 0xff]);
                    }
                    bytes.push(b']');
                    bytes
                },
            ),
            (
                "a document of more values than its bytes could carry",
                {
                    // One counted array per byte pair, each declaring more elements than the
                    // document has bytes left.
                    let mut bytes = vec![b'{', b'i', 1, b'a', b'[', b'#', b'I', 0x10, 0x00];
                    bytes.push(b'}');
                    bytes
                },
            ),
            (
                "a payload of some other encoding",
                b"\x1f\x8b\x08\x00not an encoded document".to_vec(),
            ),
        ];
        for (case, bytes) in rows {
            let read = binary_json_document(&bytes);
            assert!(
                read.is_none(),
                "case {case:?}: bytes the encoding does not admit must be no document, got \
                 {read:?}"
            );
        }
    }

    /// RULE: a document at the nesting bound is still decoded, so the bound is the depth it
    /// states and not one less.
    #[test]
    fn a_document_at_the_nesting_bound_is_decoded() {
        let depth = MAX_BINARY_JSON_DEPTH;
        let bytes: Vec<u8> = ::std::iter::repeat_n(b'[', depth)
            .chain([b'i', 1])
            .chain(::std::iter::repeat_n(b']', depth))
            .collect();
        let read = binary_json_document(&bytes);
        assert!(
            read.is_some(),
            "case \"at the bound\": a document nested {depth} deep is decoded, got {read:?}"
        );
    }

    /// RULE: the values a document may produce are bounded by its own length in bytes, so a
    /// document that really carries its values is read and one that only declares them is not.
    ///
    /// The rows differ along how many values the bytes carry against how many they declare.
    #[test]
    fn a_document_produces_at_most_one_value_per_input_byte() {
        // (case, the bytes, whether they are a document)
        let rows: Vec<(&str, Vec<u8>, bool)> = vec![
            (
                "a counted array whose elements are all there",
                vec![b'[', b'$', b'i', b'#', b'i', 3, 1, 2, 3],
                true,
            ),
            (
                "a counted array of one more element than the bytes carry",
                vec![b'[', b'$', b'i', b'#', b'i', 9, 1, 2, 3],
                false,
            ),
            (
                "an array that ends at its marker, every element present",
                vec![b'[', b'i', 1, b'i', 2, b']'],
                true,
            ),
        ];
        for (case, bytes, expected) in rows {
            let read = binary_json_document(&bytes);
            assert_eq!(
                read.is_some(),
                expected,
                "case {case:?}: expected a document={expected}, got {read:?}"
            );
        }
    }

    /// RULE: a document larger than the input bound is not read at all.
    #[test]
    fn a_document_past_the_input_bound_is_not_read() {
        let oversized = vec![b'T'; super::MAX_BINARY_JSON_BYTES + 1];
        assert!(
            binary_json_document(&oversized).is_none(),
            "case \"past the input bound\": the input bound refuses the bytes before they are read"
        );
    }
}
