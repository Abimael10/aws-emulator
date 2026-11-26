// SPDX-License-Identifier: CC0-1.0

//! The `AttributeValue` readers `UpdateUser` and `UpdateGroup` share.
//!
//! Both operations carry the new value of an attribute in an `AttributeOperation`'s
//! `AttributeValue`, which the model types as a `document`
//! (`smithy_query(query="shape",shape="AttributeOperation")`). A document declares no
//! constraints of its own, so the request boundary walks NOTHING about the value: the
//! type the attribute is modelled with, and the `@length` and `@pattern` of the member
//! being written, are that member's traits and have to be applied where the value is
//! decoded — here, once, for both operations and for every string attribute either one
//! writes.
//!
//! The check is the FRAMEWORK's own constraint walk over a shadow member schema carrying
//! the modelled traits ([`AttributeTextShape`]), and the refusal is worded by the same
//! renderer that words a refused modelled string member
//! ([`crate::service::validation::message`]). So a value a create of a member rejects an
//! update of that member rejects too, in the same clause, and a constraint class the
//! framework learns to check cannot drift from the one the boundary applies.

use ::std::sync::LazyLock;

use ::aws_smithy_schema::serde::{SerdeError, SerializableStruct, ShapeSerializer};
use ::aws_smithy_schema::{Schema, ShapeId, ShapeType, TraitMap};
use ::aws_smithy_schema_server::constraints::{validate, LengthTrait, PatternTrait, Violation};
use ::aws_smithy_types::Document;

/// This service's Smithy namespace, for the shadow member schemas below.
const NAMESPACE: &str = "com.amazonaws.identitystore";

/// The `@pattern` of `UserName`: letters, marks, symbols, numbers and punctuation, and NO
/// whitespace at all (`smithy_query(query="shape",shape="UserName")`).
const USER_NAME_PATTERN: &str = r"^[\p{L}\p{M}\p{S}\p{N}\p{P}]+$";

/// The `@length` of `UserName`: 1..128 characters.
const USER_NAME_LENGTH: (Option<i64>, Option<i64>) = (Some(1), Some(128));

/// The `@pattern` the two interchangeable sensitive text shapes declare —
/// `SensitiveStringType` and `GroupDisplayName`
/// (`smithy_query(query="shape",shape="SensitiveStringType,GroupDisplayName")`): the
/// `UserName` class plus tab, newline, carriage return and the three space characters the
/// model spells out literally, U+0020, U+00A0 (no-break space) and U+3000 (ideographic
/// space), which are the only space characters the model's own text contains.
///
/// The expression is the model's text CHARACTER FOR CHARACTER, because it is reported to
/// the caller as the expression the service applies: the two spaces outside ASCII are
/// written as `\u{..}` string escapes, which the compiler turns into those characters, and
/// the class names keep the single backslash the model gives them.
const SENSITIVE_TEXT_PATTERN: &str =
    "^[\\p{L}\\p{M}\\p{S}\\p{N}\\p{P}\\t\\n\\r \u{a0}\u{3000}]+$";

/// The `@length` both sensitive text shapes declare: 1..1024 characters.
const SENSITIVE_TEXT_LENGTH: (Option<i64>, Option<i64>) = (Some(1), Some(1024));

/// One shape's `@length` and `@pattern`, as the constraint walk reads them off a member.
fn constraint_traits(length: (Option<i64>, Option<i64>), pattern: &'static str) -> TraitMap {
    let mut traits = TraitMap::new();
    traits.insert(Box::new(LengthTrait::new(length.0, length.1)));
    traits.insert(Box::new(PatternTrait::new(pattern)));
    traits
}

/// `UserName`'s modelled constraints.
static USER_NAME_TRAITS: LazyLock<TraitMap> =
    LazyLock::new(|| constraint_traits(USER_NAME_LENGTH, USER_NAME_PATTERN));

/// `SensitiveStringType` / `GroupDisplayName`'s modelled constraints.
static SENSITIVE_TEXT_TRAITS: LazyLock<TraitMap> =
    LazyLock::new(|| constraint_traits(SENSITIVE_TEXT_LENGTH, SENSITIVE_TEXT_PATTERN));

/// A `UserName`-shaped attribute, as the constraint walk reads it.
static M_USER_NAME: Schema = Schema::new_member(
    ShapeId::from_static(
        "com.amazonaws.identitystore#AttributeText$UserName",
        NAMESPACE,
        "UserName",
    ),
    ShapeType::String,
    "UserName",
    0,
)
.with_traits(&USER_NAME_TRAITS);

/// A sensitive-text attribute, as the constraint walk reads it.
static M_SENSITIVE_TEXT: Schema = Schema::new_member(
    ShapeId::from_static(
        "com.amazonaws.identitystore#AttributeText$SensitiveText",
        NAMESPACE,
        "SensitiveText",
    ),
    ShapeType::String,
    "SensitiveText",
    1,
)
.with_traits(&SENSITIVE_TEXT_TRAITS);

/// The root of the shadow structure the constraint walk is driven over. Its member list is
/// empty on purpose: the walk visits what [`AttributeText`] WRITES, and a root struct
/// schema carries no member name so it contributes no path segment.
static ATTRIBUTE_TEXT_SCHEMA: Schema = Schema::new_struct(
    ShapeId::from_static(
        "com.amazonaws.identitystore#AttributeText",
        NAMESPACE,
        "AttributeText",
    ),
    ShapeType::Structure,
    &[],
);

/// One attribute value handed to the framework's constraint walk, written the way a codec
/// writes a string member so the walk runs the same checks the request boundary does for a
/// modelled string.
struct AttributeText<'a> {
    schema: &'static Schema,
    text: &'a str,
}

impl SerializableStruct for AttributeText<'_> {
    fn serialize_members(&self, s: &mut dyn ShapeSerializer) -> Result<(), SerdeError> {
        s.write_string(self.schema, self.text)
    }
}

/// The modelled string shape the attribute an `AttributeOperation` writes targets, and so
/// the constraint set its value must satisfy.
///
/// Two of them cover every string attribute of a `User` and of a `Group`
/// (`smithy_query(query="shape",shape="User,Group")`): `UserName`, which names a user, and
/// the sensitive text shape everything else targets — `SensitiveStringType` throughout
/// `User` and on `Group$Description`, and `GroupDisplayName`, which declares the same
/// `@length` and the same `@pattern` as `SensitiveStringType` and so is the same set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttributeTextShape {
    /// `UserName`: `@length(1..128)` and a pattern that admits no whitespace.
    UserName,
    /// `SensitiveStringType` / `GroupDisplayName`: `@length(1..1024)` and a pattern that
    /// admits the three space characters the model lists.
    SensitiveText,
}

impl AttributeTextShape {
    /// The member schema whose modelled constraints a value of this shape must satisfy.
    fn member_schema(self) -> &'static Schema {
        match self {
            Self::UserName => &M_USER_NAME,
            Self::SensitiveText => &M_SENSITIVE_TEXT,
        }
    }
}

/// Which JSON type a document carries, for a refusal that says what arrived.
pub fn attribute_value_kind(value: &Document) -> &'static str {
    match value {
        Document::Object(_) => "an object",
        Document::Array(_) => "an array",
        Document::Number(_) => "a number",
        Document::String(_) => "a string",
        Document::Bool(_) => "a boolean",
        Document::Null => "null",
    }
}

/// The STRING an `AttributeValue` carries, checked for its JSON TYPE alone: `None` when the
/// operation REMOVES the attribute (an absent `AttributeValue` or an explicit JSON `null`).
///
/// For an attribute whose modelled shape constrains its TEXT — every string attribute of a
/// user or a group — read it through [`attribute_text_value`] instead, which applies those
/// constraints too. This reader is for a member constrained some other way, such as an
/// enum's value set.
pub fn attribute_string_value(
    value: Option<&Document>,
    path: &str,
) -> Result<Option<String>, String> {
    match value {
        None | Some(Document::Null) => Ok(None),
        Some(Document::String(text)) => Ok(Some(text.clone())),
        Some(other) => Err(format!(
            "the AttributeValue for {path} must be a string, got {}",
            attribute_value_kind(other)
        )),
    }
}

/// The new value of a STRING attribute an `AttributeOperation` sets: `None` when the
/// operation REMOVES it, otherwise the text it carries, once that text satisfies every
/// constraint the member being written declares.
///
/// `shape` is the modelled shape of that member, so the `@length` and the `@pattern`
/// applied are the model's own and a value an update stores is one the member's own type
/// admits — the same value set a create of that member admits, where the boundary validator
/// applies the identical traits. The model gives every one of these shapes a minimum length
/// of 1, so an empty string is refused rather than quietly meaning "remove": an operation
/// that removes carries no value at all.
pub fn attribute_text_value(
    value: Option<&Document>,
    path: &str,
    shape: AttributeTextShape,
) -> Result<Option<String>, String> {
    let Some(text) = attribute_string_value(value, path)? else {
        return Ok(None);
    };
    if let Some(message) = constraint_message(&text, path, shape) {
        return Err(message);
    }
    Ok(Some(text))
}

/// Why `text` is not a value the member at `path` admits, or `None` when it is one.
///
/// Every string attribute either update writes targets a `@sensitive` shape, so each
/// violation is recorded as one whose value is WITHHELD: the clause names the attribute and
/// the constraint it failed and never echoes what was sent.
fn constraint_message(text: &str, path: &str, shape: AttributeTextShape) -> Option<String> {
    let violations: Vec<Violation> = validate(
        &AttributeText {
            schema: shape.member_schema(),
            text,
        },
        &ATTRIBUTE_TEXT_SCHEMA,
    )
    .into_iter()
    .map(|violation| Violation {
        // The walk names the shadow member it was driven over; what the caller asked to
        // change is the attribute its `AttributePath` named.
        path: path.to_string(),
        value: None,
        sensitive: true,
        ..violation
    })
    .collect();
    crate::service::validation::message(&violations)
}
