// SPDX-License-Identifier: CC0-1.0

//! The form a configuration revision's document is STORED in.
//!
//! RULE: Amazon MQ does not keep the bytes a request sent for a revision — it parses
//! the engine configuration document and stores its own serialization of it, which is
//! what every later read of the revision reports. The model's own evidence that the
//! service rewrites rather than echoes the document is `UpdateConfiguration`'s
//! `Warnings` member, "the first 20 warnings about the configuration elements or
//! attributes that were sanitized"
//! (`smithy_query(query="doc", shape="UpdateConfigurationResponse", member="Warnings")`).
//!
//! The one difference between a submitted and a stored XML document this service
//! serves is the spelling of an element with no content: the stored serialization
//! writes it self-closed (`<plugins/>`), whatever pair of tags the request spelled it
//! with. Everything else the request sent — the prolog, the element names, their
//! attributes as written, the text and comments they carry, and the whitespace between
//! them — is the document's own content and is kept.
//!
//! A document that is not XML at all is a RabbitMQ Cuttlefish configuration (`Data` is
//! "the base64-encoded XML configuration" for ActiveMQ and "the base64-encoded
//! Cuttlefish configuration" for RabbitMQ), which has no element to serialize: it is
//! stored as it was sent.
//!
//! The parsing and the re-serialization are the `quick-xml` crate's; this module only
//! decides which elements have no content. One pass over the document's events, with no
//! recursion on its nesting and an answer no larger than the document itself, is all
//! this costs: the work a request can ask for here is bounded by the document the
//! request already carried, and a document this module cannot parse is answered `None`
//! rather than by failing the call.

/// The `Data` a revision STORES for the document a request sent (both base64, as the
/// member documents).
///
/// The document is canonicalized when it is XML and the canonical serialization
/// differs from what was sent; anything else — a document that is not valid base64,
/// not UTF-8 text, not XML, or already canonical — is stored exactly as it arrived.
pub fn stored_configuration_data(data: &str) -> String {
    let Ok(bytes) = ::aws_smithy_types::base64::decode(data) else {
        return data.to_string();
    };
    let Ok(document) = String::from_utf8(bytes) else {
        return data.to_string();
    };
    match canonical_xml_document(&document) {
        Ok(canonical) if canonical != document => {
            ::aws_smithy_types::base64::encode(canonical)
        }
        _ => data.to_string(),
    }
}

/// The canonical serialization of an XML configuration document: the same document
/// with every content-free element written self-closed.
///
/// `Err(offset)` when the text is not a single-rooted XML document — a Cuttlefish
/// configuration, or markup this service cannot parse — carrying the byte offset the
/// walk stopped at. One walk answers both questions this module's callers have: the
/// form the document is STORED in ([`stored_configuration_data`]), and whether it is a
/// document of this engine at all ([`super::configuration_document_syntax_error`]), so
/// the two cannot drift into accepting a document the other would rewrite.
pub fn canonical_xml_document(document: &str) -> Result<String, usize> {
    use ::quick_xml::events::Event;

    let mut reader = ::quick_xml::Reader::from_str(document);
    // A configuration document is XML, so an element that never closes means the text
    // is not a document this service can serialize — it is kept as sent instead.
    reader.config_mut().check_end_names = true;
    let mut writer = ::quick_xml::Writer::new(Vec::new());

    // How deep in the element tree the reader is, and how many elements were opened at
    // the top: a canonical document has exactly one root, and carries nothing but
    // whitespace outside it.
    let mut depth = 0usize;
    let mut roots = 0usize;
    // An element whose start tag was read but whose content is not known yet, together
    // with the whitespace read since: if its end tag comes next the element has no
    // content and is written self-closed, otherwise both are written as they came.
    let mut pending: Option<::quick_xml::events::BytesStart<'_>> = None;
    let mut pending_space: Vec<::quick_xml::events::BytesText<'_>> = Vec::new();

    loop {
        // Where the event about to be read begins, so a walk that stops names the place
        // in the document it stopped at rather than the end of the text.
        let position = usize::try_from(reader.buffer_position()).unwrap_or(usize::MAX);
        let event = reader.read_event().map_err(|_| position)?;
        // Whitespace between a start tag and its own end tag is not content: it is the
        // indentation of an element the request spelled as a pair of tags, and the
        // canonical serialization of that element carries no content at all.
        if let Some(start) = pending.take() {
            if let Event::Text(text) = &event {
                if is_whitespace(text.as_ref()) {
                    pending_space.push(text.clone());
                    pending = Some(start);
                    continue;
                }
            }
            if matches!(event, Event::End(_)) {
                // A content-free element: one self-closed tag replaces the pair, and
                // the whitespace that stood between them goes with them.
                writer.write_event(Event::Empty(start)).map_err(|_| position)?;
                pending_space.clear();
                depth = depth.checked_sub(1).ok_or(position)?;
                continue;
            }
            writer.write_event(Event::Start(start)).map_err(|_| position)?;
            for space in pending_space.drain(..) {
                writer.write_event(Event::Text(space)).map_err(|_| position)?;
            }
        }

        match event {
            Event::Eof => break,
            Event::Start(start) => {
                if depth == 0 {
                    roots = roots.saturating_add(1);
                }
                depth = depth.saturating_add(1);
                pending = Some(start);
            }
            Event::Empty(empty) => {
                if depth == 0 {
                    roots = roots.saturating_add(1);
                }
                writer.write_event(Event::Empty(empty)).map_err(|_| position)?;
            }
            Event::End(end) => {
                depth = depth.checked_sub(1).ok_or(position)?;
                writer.write_event(Event::End(end)).map_err(|_| position)?;
            }
            Event::Text(text) => {
                // Text outside the root element is not part of an XML document, so a
                // Cuttlefish configuration — all text, no element — is not one.
                if depth == 0 && !is_whitespace(text.as_ref()) {
                    return Err(position);
                }
                writer.write_event(Event::Text(text)).map_err(|_| position)?;
            }
            other => writer.write_event(other).map_err(|_| position)?,
        }
    }

    // The document ran out without being one: an element that never closed, or no single
    // root element. The offset names the last byte carrying content rather than the
    // empty line after it.
    let ran_out = document.trim_end().len();
    if depth != 0 || roots != 1 {
        return Err(ran_out);
    }
    String::from_utf8(writer.into_inner()).map_err(|_| ran_out)
}

/// The document the revision a CREATE makes carries: the engine's own default settings
/// for `engine_type`.
///
/// `CreateConfiguration` carries no document member — "Amazon MQ uses the default
/// configuration (the engine type and version)" — so the first revision of every
/// configuration holds the settings the ENGINE ships with, in the format that engine's
/// `Data` is documented in: a Cuttlefish configuration for RabbitMQ, a `broker.xml` for
/// ActiveMQ. The document is an engine-level constant, the same for every configuration
/// of that engine, and never a value a request supplied.
///
/// The RabbitMQ default is the one Amazon MQ publishes for that engine: the management
/// header setting and the delivery acknowledgement timeout, each with the comment the
/// engine's own default file carries. No recorded answer covers the ActiveMQ default, so
/// the document below is this service's, holding the settings Amazon MQ's default
/// `broker.xml` is made of; it is already in the form a revision stores
/// ([`stored_configuration_data`]), so a client that reads it and writes it back gets it
/// stored unchanged.
pub fn default_configuration_document(engine_type: &str) -> &'static str {
    if engine_type.eq_ignore_ascii_case(super::ENGINE_TYPE_RABBITMQ) {
        DEFAULT_RABBITMQ_DOCUMENT
    } else {
        DEFAULT_ACTIVEMQ_DOCUMENT
    }
}

/// The engine default of [`default_configuration_document`], base64 as `Data` is
/// documented — the form a revision record holds its document in.
pub fn default_configuration_data(engine_type: &str) -> String {
    ::aws_smithy_types::base64::encode(default_configuration_document(engine_type))
}

/// RabbitMQ's default Cuttlefish configuration.
const DEFAULT_RABBITMQ_DOCUMENT: &str = "\
# Management header is set to true to enable Content-Type-Options, Frame-Options, and HSTS headers by default
secure.management.http.headers.enabled = true
# Default RabbitMQ delivery acknowledgement timeout is 30 minutes
consumer_timeout = 1800000
";

/// ActiveMQ's default `broker.xml`.
const DEFAULT_ACTIVEMQ_DOCUMENT: &str = "\
<?xml version=\"1.0\" encoding=\"UTF-8\"?>
<broker xmlns=\"http://activemq.apache.org/schema/core\">
  <destinationPolicy>
    <policyMap>
      <policyEntries>
        <policyEntry topic=\"&gt;\">
          <pendingMessageLimitStrategy>
            <constantPendingMessageLimitStrategy limit=\"1000\"/>
          </pendingMessageLimitStrategy>
        </policyEntry>
      </policyEntries>
    </policyMap>
  </destinationPolicy>
  <plugins>
    <forcePersistencyModeBrokerPlugin persistenceFlag=\"true\"/>
    <statisticsBrokerPlugin/>
    <timeStampingBrokerPlugin ttlCeiling=\"86400000\" zeroExpirationOverride=\"86400000\"/>
  </plugins>
</broker>
";

/// Whether a text node carries nothing but XML whitespace, which is what the
/// indentation between a pair of tags is made of.
fn is_whitespace(text: &[u8]) -> bool {
    !text.is_empty() && text.iter().all(|byte| byte.is_ascii_whitespace())
}
