// SPDX-License-Identifier: CC0-1.0

//! How this service reads the value of a BOOLEAN request parameter: as the FLAG it is, at
//! every depth of the parameter list.
//!
//! EC2 reads such a parameter's value without regard to ASCII case: the recorded traffic has it
//! SERVING a request whose flag spells its value in another case — answering the dry-run
//! verdict for a `DryRun` spelled that way — rather than refusing the body as unreadable, and
//! serving one whose value spells no boolean at all with the flag left unset. WHERE in the
//! parameter list the value sits is no part of that: `AssociatePublicIpAddress` on a launch
//! template's network interface, `Enabled` on its monitoring, `DeleteOnTermination` on a block
//! device and `Value` on an attribute are the same boolean written at a deeper position, so a
//! request that writes one of those that way is served exactly as one that writes a parameter
//! of the request itself that way, and the parameters a handler would have refused it for are
//! reached.
//!
//! The reading is applied where the model's TYPE of a position is known rather than where a
//! request key can be matched, which is what makes it reach every depth: a key naming a
//! position INSIDE a member resolves to no member of the input shape — a member schema carries
//! its own name and kind rather than the shape it targets, so the shape nested under it is not
//! reachable from the schema being read. [`Flags`] therefore wraps the reader the framework
//! would have read the request with and answers every BOOLEAN position itself, leaving every
//! other position to that reader untouched: a member that merely CARRIES text reading like a
//! boolean (a tag value, a filter value, a description) is never touched, because nothing here
//! looks at a position the model does not declare a boolean.
//!
//! Only the VALUE is read this way, and the reading is the FLAG's: a value that spells `true`
//! in any ASCII case sets the boolean, and every other value — another spelling of `false`, a
//! digit, a word, an empty value — leaves it unset. A position carrying no scalar at all (a
//! boolean a request wrote as a structure or a list) is left to the framework's own reading of
//! it.

use ::aws_smithy_schema::serde::{SerdeError, ShapeDeserializer};
use ::aws_smithy_schema::Schema;
use ::aws_smithy_types::{BigDecimal, BigInteger, Blob, DateTime, Document};

/// The one spelling that SETS a boolean, as the wire writes it: a value equal to it without
/// regard to ASCII case carries `true`, and every other value carries `false`.
const WIRE_TRUE: &str = "true";

/// The position a wrapped reading delegates to: the deserializer the framework would have read
/// this position of the request with.
///
/// Two of them, so ONE reading covers both the request's root — where this service hands the
/// framework's walk a boxed reader — and every position inside it, where the walk hands the
/// read a borrowed one.
pub trait Position {
    /// The framework's own reader for this position.
    fn reader(&mut self) -> &mut dyn ShapeDeserializer;

    /// The same reader, for the two readings that ask a question of the position rather than
    /// taking a value out of it.
    fn asked(&self) -> &dyn ShapeDeserializer;
}

impl Position for Box<dyn ShapeDeserializer + '_> {
    fn reader(&mut self) -> &mut dyn ShapeDeserializer {
        &mut **self
    }

    fn asked(&self) -> &dyn ShapeDeserializer {
        &**self
    }
}

impl Position for &mut (dyn ShapeDeserializer + '_) {
    fn reader(&mut self) -> &mut dyn ShapeDeserializer {
        &mut **self
    }

    fn asked(&self) -> &dyn ShapeDeserializer {
        &**self
    }
}

/// The reader this service reads an ec2Query request with: the framework's own at every
/// position but a BOOLEAN one, whose value is read without regard to ASCII case.
///
/// Every container reading hands the consumer another [`Flags`], so the rule reaches a boolean
/// however deep the request writes it; every other reading is the framework's unchanged.
pub struct Flags<P> {
    at: P,
}

impl<P: Position> Flags<P> {
    /// Read the position `at` this way.
    pub fn over(at: P) -> Self {
        Self { at }
    }
}

impl<P: Position> ShapeDeserializer for Flags<P> {
    fn read_struct(
        &mut self,
        schema: &Schema,
        consumer: &mut dyn FnMut(&Schema, &mut dyn ShapeDeserializer) -> Result<(), SerdeError>,
    ) -> Result<(), SerdeError> {
        self.at
            .reader()
            .read_struct(schema, &mut |member, inner| {
                consumer(member, &mut Flags::over(inner))
            })
    }

    fn read_list(
        &mut self,
        schema: &Schema,
        consumer: &mut dyn FnMut(&mut dyn ShapeDeserializer) -> Result<(), SerdeError>,
    ) -> Result<(), SerdeError> {
        self.at
            .reader()
            .read_list(schema, &mut |inner| consumer(&mut Flags::over(inner)))
    }

    fn read_map(
        &mut self,
        schema: &Schema,
        consumer: &mut dyn FnMut(String, &mut dyn ShapeDeserializer) -> Result<(), SerdeError>,
    ) -> Result<(), SerdeError> {
        self.at.reader().read_map(schema, &mut |key, inner| {
            consumer(key, &mut Flags::over(inner))
        })
    }

    /// The one reading that is this service's own: a value spelling [`WIRE_TRUE`] in any ASCII
    /// case sets the boolean and every other value leaves it unset — the one reading of a
    /// boolean parameter this service makes, whatever depth the request writes it at.
    ///
    /// A position carrying no scalar at all — a boolean a request wrote as a structure or a
    /// list — is no value in another case, so the framework's own reading of it is kept,
    /// whatever that answers.
    fn read_boolean(&mut self, schema: &Schema) -> Result<bool, SerdeError> {
        let reader = self.at.reader();
        match reader.read_string(schema) {
            Ok(value) => Ok(value.eq_ignore_ascii_case(WIRE_TRUE)),
            Err(_) => reader.read_boolean(schema),
        }
    }

    fn read_byte(&mut self, schema: &Schema) -> Result<i8, SerdeError> {
        self.at.reader().read_byte(schema)
    }

    fn read_short(&mut self, schema: &Schema) -> Result<i16, SerdeError> {
        self.at.reader().read_short(schema)
    }

    fn read_integer(&mut self, schema: &Schema) -> Result<i32, SerdeError> {
        self.at.reader().read_integer(schema)
    }

    fn read_long(&mut self, schema: &Schema) -> Result<i64, SerdeError> {
        self.at.reader().read_long(schema)
    }

    fn read_float(&mut self, schema: &Schema) -> Result<f32, SerdeError> {
        self.at.reader().read_float(schema)
    }

    fn read_double(&mut self, schema: &Schema) -> Result<f64, SerdeError> {
        self.at.reader().read_double(schema)
    }

    fn read_big_integer(&mut self, schema: &Schema) -> Result<BigInteger, SerdeError> {
        self.at.reader().read_big_integer(schema)
    }

    fn read_big_decimal(&mut self, schema: &Schema) -> Result<BigDecimal, SerdeError> {
        self.at.reader().read_big_decimal(schema)
    }

    fn read_string(&mut self, schema: &Schema) -> Result<String, SerdeError> {
        self.at.reader().read_string(schema)
    }

    fn read_blob(&mut self, schema: &Schema) -> Result<Blob, SerdeError> {
        self.at.reader().read_blob(schema)
    }

    fn read_timestamp(&mut self, schema: &Schema) -> Result<DateTime, SerdeError> {
        self.at.reader().read_timestamp(schema)
    }

    fn read_document(&mut self, schema: &Schema) -> Result<Document, SerdeError> {
        self.at.reader().read_document(schema)
    }

    fn is_null(&self) -> bool {
        self.at.asked().is_null()
    }

    fn read_null(&mut self) -> Result<(), SerdeError> {
        self.at.reader().read_null()
    }

    fn container_size(&self) -> Option<usize> {
        self.at.asked().container_size()
    }
}
