// SPDX-License-Identifier: CC0-1.0

//! The MQTT VERSIONS the broker serves, and the ONE reading of a packet both of them frame.
//!
//! AWS IoT Core's message broker serves MQTT 3.1.1 and MQTT 5 at the same endpoint, and a
//! client says which of them it speaks in the variable header of its own CONNECT. What the
//! broker DOES with a packet is the same either way — a CONNECT opens or resumes a session,
//! a SUBSCRIBE records topic filters on it, a PUBLISH is handed to the sessions subscribed
//! to its topic — so the session is served once ([`super::serve_session`]) over the
//! version-neutral reading in this module, and only the CODEC differs:
//!
//! * MQTT 5 carries a publication's own message PROPERTIES (payload format, content type,
//!   response topic, correlation data, user properties, message expiry), which 3.1.1 has no
//!   field for at all, so a message forwarded to a 3.1.1 subscriber carries its topic,
//!   payload and QoS alone.
//! * each version spells its CONNACK and SUBACK outcomes with its own codes, and a code of
//!   the other version is unreadable to a client — which is why the version is settled
//!   before anything is written back.
//!
//! The packets themselves are read and written by the maintained codec ([`::mqttbytes`],
//! whose `v5` feature is what this module adds): this module decides WHICH of a message's
//! facts each version can carry, never how a packet is laid out.

use ::bytes::{Bytes, BytesMut};
use ::mqttbytes::{v4, v5, QoS};

use crate::iot_mqtt_broker::DirectMessage;

use super::{qos_number, qos_of, MAX_GRANTED_QOS};

/// The protocol level an MQTT 3.1.1 CONNECT announces: 4 (MQTT 3.1.1 section 3.1.2.2, "the
/// 8 bit unsigned value that represents the revision level of the protocol used by the
/// Client ... The value of the Protocol Level field for the version 3.1.1 of the protocol
/// is 4").
pub const PROTOCOL_LEVEL_V311: u8 = 4;

/// The protocol level an MQTT 5 CONNECT announces: 5 (MQTT 5 section 3.1.2.2, "The value of
/// the Protocol Version field for version 5.0 of the protocol is 5").
pub const PROTOCOL_LEVEL_V5: u8 = 5;

/// How many bytes of a connection are read looking for the protocol level before the
/// connection is given up on.
///
/// The level sits a fixed distance into the CONNECT: the packet type, the remaining-length
/// varint (at most four bytes), the two-byte length of the protocol name and the name
/// itself (`MQTT`, and `MQIsdp` for the longest name any revision used). This is room for
/// all of it, so a connection whose opening bytes carry no level in that span is not a
/// CONNECT of any MQTT revision.
pub const MAX_PROTOCOL_LEVEL_OFFSET: usize = 16;

/// Which version of MQTT one connection speaks — the two AWS IoT Core's message broker
/// serves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Version {
    /// MQTT 3.1.1.
    V311,
    /// MQTT 5.
    V5,
}

impl Version {
    /// The version a CONNECT's protocol level names, or `None` for a level this broker
    /// serves no protocol for.
    #[must_use]
    pub const fn of_protocol_level(level: u8) -> Option<Self> {
        match level {
            PROTOCOL_LEVEL_V311 => Some(Self::V311),
            PROTOCOL_LEVEL_V5 => Some(Self::V5),
            _ => None,
        }
    }

    /// Whether this version carries a publication's MQTT5 message properties.
    #[must_use]
    pub const fn carries_message_properties(self) -> bool {
        matches!(self, Self::V5)
    }
}

/// The protocol level the bytes of a CONNECT announce, or `None` while fewer bytes than the
/// level's own position have arrived — or when they are no CONNECT this reading can place.
///
/// The position is the packet's own layout (MQTT 3.1.1 and MQTT 5, section 3.1.2): the
/// fixed header's type byte, then its remaining-length varint, then the protocol NAME as a
/// length-prefixed string, then the one level byte. It is read here rather than by framing
/// the packet first, because framing it needs the codec of the version this answers.
#[must_use]
pub fn announced_protocol_level(bytes: &[u8]) -> Option<u8> {
    // Past the fixed header's type byte, the remaining length is a varint of at most four
    // bytes, the last of which has its continuation bit clear.
    let mut index = 1;
    loop {
        let byte = *bytes.get(index)?;
        index += 1;
        if byte & 0b1000_0000 == 0 {
            break;
        }
        if index > 4 {
            return None;
        }
    }
    let length = u16::from_be_bytes([*bytes.get(index)?, *bytes.get(index + 1)?]);
    index += 2 + usize::from(length);
    bytes.get(index).copied()
}

/// What one CONNECT asks for, read from whichever version framed it.
#[derive(Debug, Clone, Default)]
pub struct Connect {
    /// The client id the session is keyed by.
    pub client_id: String,
    /// Whether the client asked for a session with nothing carried over — MQTT 3.1.1's
    /// Clean Session flag, which MQTT 5 section 3.1.2.4 carries as Clean Start.
    pub clean_session: bool,
    /// The keep-alive interval the CONNECT asks for, in SECONDS.
    pub keep_alive_seconds: i32,
    /// The Session Expiry Interval an MQTT 5 CONNECT configured, in SECONDS; 0 for a 3.1.1
    /// client, whose protocol carries no such property.
    pub session_expiry_seconds: i64,
    /// The client's Last Will and Testament, as this CONNECT configured one: the
    /// publication the broker is to make for the client when its session ends without the
    /// client's own DISCONNECT. `None` for a CONNECT that configured none.
    ///
    /// BOXED, like the message of a [`ClientPacket::Publish`]: a message is the largest
    /// thing a packet carries, so the reading of a packet stays small whichever packet it
    /// is.
    pub last_will: Option<Box<DirectMessage>>,
}

/// One topic filter a SUBSCRIBE asks for.
#[derive(Debug, Clone)]
pub struct Filter {
    pub path: String,
    /// The maximum QoS the client asks to be delivered at.
    pub qos: i32,
}

/// One packet from a client, as the broker reads it whatever version framed it.
#[derive(Debug)]
pub enum ClientPacket {
    Connect(Connect),
    /// A publication, with the packet identifier its acknowledgement answers. The message
    /// travels BOXED: it is the largest thing any packet carries, so every other reading of
    /// a packet stays small.
    Publish {
        message: Box<DirectMessage>,
        packet_id: u16,
    },
    /// The client acknowledged a delivery made at QoS 1.
    PubAck {
        packet_id: u16,
    },
    Subscribe {
        packet_id: u16,
        filters: Vec<Filter>,
    },
    Unsubscribe {
        packet_id: u16,
        filters: Vec<String>,
    },
    PingReq,
    Disconnect,
    /// A packet only a SERVER sends, or one of an exchange this broker does not run (QoS
    /// 2): a client that sends one is not speaking the protocol this endpoint serves.
    Unserved,
}

/// Framing one packet off the bytes read so far.
#[derive(Debug)]
pub enum Framing {
    /// One packet, taken off the buffer.
    Packet(ClientPacket),
    /// Fewer bytes than one whole packet have arrived.
    Incomplete,
    /// No packet this codec can read — including one past the size the caller allows.
    Unreadable,
}

/// Take one packet off `buffer` with `version`'s codec, reading at most `max_bytes` of
/// packet.
pub fn frame(version: Version, buffer: &mut BytesMut, max_bytes: usize) -> Framing {
    match version {
        Version::V311 => match v4::read(buffer, max_bytes) {
            Ok(packet) => Framing::Packet(of_v4(packet)),
            Err(::mqttbytes::Error::InsufficientBytes(_needed)) => Framing::Incomplete,
            Err(_unreadable) => Framing::Unreadable,
        },
        Version::V5 => match v5::read(buffer, max_bytes) {
            Ok(packet) => Framing::Packet(of_v5(packet)),
            Err(::mqttbytes::Error::InsufficientBytes(_needed)) => Framing::Incomplete,
            Err(_unreadable) => Framing::Unreadable,
        },
    }
}

/// One MQTT 3.1.1 packet, as the broker reads it.
fn of_v4(packet: v4::Packet) -> ClientPacket {
    match packet {
        v4::Packet::Connect(connect) => ClientPacket::Connect(Connect {
            // A 3.1.1 will carries its topic, payload, QoS and RETAIN flag: the protocol
            // has no field for a message property.
            last_will: connect.last_will.map(|will| {
                Box::new(DirectMessage {
                    topic: will.topic,
                    payload: will.message.to_vec(),
                    qos: qos_number(will.qos),
                    retain: will.retain,
                    ..DirectMessage::default()
                })
            }),
            client_id: connect.client_id,
            clean_session: connect.clean_session,
            keep_alive_seconds: i32::from(connect.keep_alive),
            session_expiry_seconds: 0,
        }),
        v4::Packet::Publish(publish) => ClientPacket::Publish {
            packet_id: publish.pkid,
            // A 3.1.1 publication carries its topic, payload, QoS and RETAIN flag and
            // nothing else: the protocol has no field for a message property.
            message: Box::new(DirectMessage {
                topic: publish.topic,
                payload: publish.payload.to_vec(),
                qos: qos_number(publish.qos),
                retain: publish.retain,
                ..DirectMessage::default()
            }),
        },
        v4::Packet::PubAck(acknowledged) => ClientPacket::PubAck {
            packet_id: acknowledged.pkid,
        },
        v4::Packet::Subscribe(subscribe) => ClientPacket::Subscribe {
            packet_id: subscribe.pkid,
            filters: subscribe
                .filters
                .into_iter()
                .map(|filter| Filter {
                    path: filter.path,
                    qos: qos_number(filter.qos),
                })
                .collect(),
        },
        v4::Packet::Unsubscribe(unsubscribe) => ClientPacket::Unsubscribe {
            packet_id: unsubscribe.pkid,
            filters: unsubscribe.topics,
        },
        v4::Packet::PingReq => ClientPacket::PingReq,
        v4::Packet::Disconnect => ClientPacket::Disconnect,
        _server_or_qos2 => ClientPacket::Unserved,
    }
}

/// One MQTT 5 packet, as the broker reads it — with the message properties a publication
/// carries, which is what makes them forwardable to another MQTT 5 subscriber.
fn of_v5(packet: v5::Packet) -> ClientPacket {
    match packet {
        v5::Packet::Connect(connect) => ClientPacket::Connect(Connect {
            // The Session Expiry Interval is a property of the CONNECT, and a CONNECT
            // that carries no property section configures none.
            session_expiry_seconds: connect
                .properties
                .as_ref()
                .and_then(|properties| properties.session_expiry_interval)
                .map_or(0, i64::from),
            // An MQTT 5 will carries the message properties of its own property section,
            // which is what makes them forwardable to another MQTT 5 subscriber when the
            // broker publishes the will.
            last_will: connect.last_will.map(|will| {
                let properties = will.properties;
                Box::new(DirectMessage {
                    topic: will.topic,
                    payload: will.message.to_vec(),
                    qos: qos_number(will.qos),
                    retain: will.retain,
                    content_type: properties
                        .as_ref()
                        .and_then(|properties| properties.content_type.clone()),
                    response_topic: properties
                        .as_ref()
                        .and_then(|properties| properties.response_topic.clone()),
                    user_properties: properties
                        .as_ref()
                        .and_then(|properties| {
                            user_properties_document(&properties.user_properties)
                        }),
                    payload_format_indicator: properties
                        .as_ref()
                        .and_then(|properties| properties.payload_format_indicator)
                        .map(payload_format_token),
                    correlation_data: properties
                        .as_ref()
                        .and_then(|properties| properties.correlation_data.as_ref())
                        .map(|data| encode_correlation_data(data)),
                    message_expiry_seconds: properties
                        .as_ref()
                        .and_then(|properties| properties.message_expiry_interval),
                    ..DirectMessage::default()
                })
            }),
            client_id: connect.client_id,
            clean_session: connect.clean_session,
            keep_alive_seconds: i32::from(connect.keep_alive),
        }),
        v5::Packet::Publish(publish) => {
            let properties = publish.properties;
            ClientPacket::Publish {
                packet_id: publish.pkid,
                message: Box::new(DirectMessage {
                    topic: publish.topic,
                    payload: publish.payload.to_vec(),
                    qos: qos_number(publish.qos),
                    retain: publish.retain,
                    content_type: properties
                        .as_ref()
                        .and_then(|properties| properties.content_type.clone()),
                    response_topic: properties
                        .as_ref()
                        .and_then(|properties| properties.response_topic.clone()),
                    user_properties: properties
                        .as_ref()
                        .and_then(|properties| {
                            user_properties_document(&properties.user_properties)
                        }),
                    payload_format_indicator: properties
                        .as_ref()
                        .and_then(|properties| properties.payload_format_indicator)
                        .map(payload_format_token),
                    correlation_data: properties
                        .as_ref()
                        .and_then(|properties| properties.correlation_data.as_ref())
                        .map(|data| encode_correlation_data(data)),
                    message_expiry_seconds: properties
                        .as_ref()
                        .and_then(|properties| properties.message_expiry_interval),
                    ..DirectMessage::default()
                }),
            }
        }
        v5::Packet::PubAck(acknowledged) => ClientPacket::PubAck {
            packet_id: acknowledged.pkid,
        },
        v5::Packet::Subscribe(subscribe) => ClientPacket::Subscribe {
            packet_id: subscribe.pkid,
            filters: subscribe
                .filters
                .into_iter()
                .map(|filter| Filter {
                    path: filter.path,
                    qos: qos_number(filter.qos),
                })
                .collect(),
        },
        v5::Packet::Unsubscribe(unsubscribe) => ClientPacket::Unsubscribe {
            packet_id: unsubscribe.pkid,
            filters: unsubscribe.filters,
        },
        v5::Packet::PingReq => ClientPacket::PingReq,
        v5::Packet::Disconnect(_reason) => ClientPacket::Disconnect,
        _server_or_qos2 => ClientPacket::Unserved,
    }
}

/// How a CONNECT is answered.
#[derive(Debug, Clone, Copy)]
pub enum Connack {
    /// The session is open, and `session_present` says whether it continues a stored one.
    Accepted { session_present: bool },
    /// The protocol level the CONNECT announced is one this broker serves no protocol for.
    UnsupportedProtocolVersion,
    /// The client id is not one a session can be keyed by.
    ClientIdNotValid,
    /// The broker is holding as many sessions as it will.
    ServerUnavailable,
}

/// Render the CONNACK that answers a CONNECT, in `version`'s own codes.
pub fn render_connack(
    version: Version,
    buffer: &mut BytesMut,
    outcome: Connack,
) -> Result<usize, ::mqttbytes::Error> {
    let session_present = matches!(
        outcome,
        Connack::Accepted {
            session_present: true
        }
    );
    match version {
        Version::V311 => {
            let code = match outcome {
                Connack::Accepted { .. } => v4::ConnectReturnCode::Success,
                Connack::UnsupportedProtocolVersion => {
                    v4::ConnectReturnCode::RefusedProtocolVersion
                }
                Connack::ClientIdNotValid => v4::ConnectReturnCode::BadClientId,
                Connack::ServerUnavailable => v4::ConnectReturnCode::ServiceUnavailable,
            };
            v4::ConnAck::new(code, session_present).write(buffer)
        }
        Version::V5 => {
            let code = match outcome {
                Connack::Accepted { .. } => v5::ConnectReturnCode::Success,
                Connack::UnsupportedProtocolVersion => {
                    v5::ConnectReturnCode::UnsupportedProtocolVersion
                }
                Connack::ClientIdNotValid => v5::ConnectReturnCode::ClientIdentifierNotValid,
                Connack::ServerUnavailable => v5::ConnectReturnCode::ServerUnavailable,
            };
            let mut connack = v5::ConnAck::new(code, session_present);
            // MQTT 5 gives every CONNACK a property section, zero-length where the server
            // sets no property (section 3.2.2.3), and a client reading one cannot do
            // without its length.
            connack.properties = Some(v5::ConnAckProperties {
                session_expiry_interval: None,
                receive_max: None,
                max_qos: None,
                retain_available: None,
                max_packet_size: None,
                assigned_client_identifier: None,
                topic_alias_max: None,
                reason_string: None,
                user_properties: Vec::new(),
                wildcard_subscription_available: None,
                subscription_identifiers_available: None,
                shared_subscription_available: None,
                server_keep_alive: None,
                response_information: None,
                server_reference: None,
                authentication_method: None,
                authentication_data: None,
            });
            connack.write(buffer)
        }
    }
}

/// What a SUBACK says about one of the filters its SUBSCRIBE asked for.
#[derive(Debug, Clone, Copy)]
pub enum Grant {
    /// The subscription is granted, at this QoS — never above [`MAX_GRANTED_QOS`].
    Qos(i32),
    /// The filter is no topic filter this broker will subscribe.
    TopicFilterInvalid,
    /// The session already subscribes to as many filters as it may.
    QuotaExceeded,
}

/// Render the SUBACK that answers a SUBSCRIBE, in `version`'s own reason codes.
pub fn render_suback(
    version: Version,
    buffer: &mut BytesMut,
    packet_id: u16,
    granted: &[Grant],
) -> Result<usize, ::mqttbytes::Error> {
    match version {
        Version::V311 => {
            // MQTT 3.1.1 section 3.9.3 has ONE failure code, so both refusals are spelled
            // with it; the per-filter granted QoS is the rest of the code set.
            let codes = granted
                .iter()
                .map(|grant| match grant {
                    Grant::Qos(qos) => v4::SubscribeReasonCode::Success(qos_of(*qos)),
                    Grant::TopicFilterInvalid | Grant::QuotaExceeded => {
                        v4::SubscribeReasonCode::Failure
                    }
                })
                .collect();
            v4::SubAck::new(packet_id, codes).write(buffer)
        }
        Version::V5 => {
            // MQTT 5 section 3.9.3 names the reason: the filter, or the quota.
            let codes = granted
                .iter()
                .map(|grant| match grant {
                    Grant::Qos(qos) if *qos >= MAX_GRANTED_QOS => {
                        v5::SubscribeReasonCode::QoS1
                    }
                    Grant::Qos(_at_most_once) => v5::SubscribeReasonCode::QoS0,
                    Grant::TopicFilterInvalid => v5::SubscribeReasonCode::TopicFilterInvalid,
                    Grant::QuotaExceeded => v5::SubscribeReasonCode::QuotaExceeded,
                })
                .collect();
            v5::SubAck::new(packet_id, codes).write(buffer)
        }
    }
}

/// Render the PUBACK that acknowledges a client's publication made at QoS 1.
pub fn render_puback(
    version: Version,
    buffer: &mut BytesMut,
    packet_id: u16,
) -> Result<usize, ::mqttbytes::Error> {
    match version {
        Version::V311 => v4::PubAck::new(packet_id).write(buffer),
        Version::V5 => v5::PubAck::new(packet_id).write(buffer),
    }
}

/// Render the UNSUBACK that answers an UNSUBSCRIBE of `filters` filters: every filter it
/// named is removed from the session, so each is answered as removed.
pub fn render_unsuback(
    version: Version,
    buffer: &mut BytesMut,
    packet_id: u16,
    filters: usize,
) -> Result<usize, ::mqttbytes::Error> {
    match version {
        Version::V311 => v4::UnsubAck::new(packet_id).write(buffer),
        Version::V5 => {
            // MQTT 5 section 3.11.3 answers one reason code per filter the UNSUBSCRIBE
            // named, and a client reading the packet expects exactly that many.
            let mut unsuback = v5::UnsubAck::new(packet_id);
            unsuback.reasons = vec![v5::UnsubAckReason::Success; filters.max(1)];
            unsuback.write(buffer)
        }
    }
}

/// Render the PINGRESP that answers a client's PINGREQ.
pub fn render_pingresp(
    version: Version,
    buffer: &mut BytesMut,
) -> Result<usize, ::mqttbytes::Error> {
    match version {
        Version::V311 => v4::PingResp.write(buffer),
        Version::V5 => v5::PingResp.write(buffer),
    }
}

/// Render one delivery as a PUBLISH to a client of `version`, at the QoS the delivery was
/// made at, carrying the RETAIN flag the delivery carries, and with the message properties
/// that version can carry.
///
/// `retain` is the RETAIN flag of THIS delivery, which is a fact about the delivery and not
/// about the message: MQTT 3.1.1 section 3.3.1.3 sets it on the PUBLISH that answers a new
/// subscription with what the topic retained, and clears it on a message forwarded by
/// ordinary publishing, so a subscriber can tell the two apart.
///
/// `now_millis` is what the forwarded message expiry is computed against — see
/// [`DirectMessage::forwarded_message_expiry`]. A message carrying no property at all is
/// written with no property section, so a publication made without properties arrives
/// without any.
pub fn render_publish(
    version: Version,
    buffer: &mut BytesMut,
    message: &DirectMessage,
    packet_id: u16,
    retain: bool,
    now_millis: i64,
) -> Result<usize, ::mqttbytes::Error> {
    let qos = qos_of(message.qos.clamp(0, MAX_GRANTED_QOS));
    match version {
        Version::V311 => {
            let mut publish = v4::Publish::new(&message.topic, qos, message.payload.clone());
            publish.retain = retain;
            if qos != QoS::AtMostOnce {
                publish.pkid = packet_id;
            }
            publish.write(buffer)
        }
        Version::V5 => {
            let mut publish = v5::Publish::new(&message.topic, qos, message.payload.clone());
            publish.retain = retain;
            if qos != QoS::AtMostOnce {
                publish.pkid = packet_id;
            }
            // MQTT 5 gives every PUBLISH a property section, zero-length where the message
            // carries no property (section 3.3.2.3) — so a publication made with none
            // arrives with none, and a client reading one still finds its length.
            publish.properties = Some(message_properties(message, now_millis));
            publish.write(buffer)
        }
    }
}

/// The MQTT 5 property section one message is forwarded with: every property it carries,
/// and nothing where it carries none.
fn message_properties(message: &DirectMessage, now_millis: i64) -> v5::PublishProperties {
    v5::PublishProperties {
        payload_format_indicator: message
            .payload_format_indicator
            .as_deref()
            .and_then(payload_format_indicator),
        message_expiry_interval: message.forwarded_message_expiry(now_millis),
        content_type: message.content_type.clone(),
        response_topic: message.response_topic.clone(),
        correlation_data: message
            .correlation_data
            .as_deref()
            .map(decode_correlation_data),
        user_properties: message
            .user_properties
            .as_deref()
            .map(user_property_pairs)
            .unwrap_or_default(),
        // A topic alias replaces a topic name on a connection that negotiated one, and a
        // subscription identifier belongs to a subscription made with one; this broker
        // negotiates neither, so a forwarded message carries neither.
        topic_alias: None,
        subscription_identifiers: Vec::new(),
    }
}

/// The `payloadFormatIndicator` token one MQTT 5 payload format indicator names.
///
/// The tokens are the data plane's own modelled enum (`PayloadFormatIndicator`:
/// `UNSPECIFIED_BYTES` and `UTF8_DATA`), and the values are MQTT 5 section 3.3.2.3.2: 0 for
/// unspecified bytes, 1 for a UTF-8 encoded payload. A value the protocol does not define
/// names no format, so it is carried as unspecified bytes.
#[must_use]
pub fn payload_format_token(indicator: u8) -> String {
    if indicator == UTF8_DATA_INDICATOR {
        UTF8_DATA.to_string()
    } else {
        UNSPECIFIED_BYTES.to_string()
    }
}

/// The MQTT 5 payload format indicator a `payloadFormatIndicator` token names, or `None`
/// for a token that names no format.
#[must_use]
pub fn payload_format_indicator(token: &str) -> Option<u8> {
    match token {
        UTF8_DATA => Some(UTF8_DATA_INDICATOR),
        UNSPECIFIED_BYTES => Some(UNSPECIFIED_BYTES_INDICATOR),
        _unmodelled => None,
    }
}

/// The two `PayloadFormatIndicator` tokens the IoT data plane models, and the MQTT 5
/// payload format indicator each names (section 3.3.2.3.2).
const UNSPECIFIED_BYTES: &str = "UNSPECIFIED_BYTES";
/// See [`UNSPECIFIED_BYTES`].
const UTF8_DATA: &str = "UTF8_DATA";
/// See [`UNSPECIFIED_BYTES`].
const UNSPECIFIED_BYTES_INDICATOR: u8 = 0;
/// See [`UNSPECIFIED_BYTES`].
const UTF8_DATA_INDICATOR: u8 = 1;

/// How many user properties one message carries, at most.
///
/// CHOSEN as a process bound, not a protocol limit: a publisher decides how many name and
/// value pairs it sends, and a message is held for every subscribed session, so without a
/// ceiling one publication would size what the broker holds. The AWS IoT Core message
/// broker quota "Maximum number of user properties" is 50, which is the bound.
const MAX_USER_PROPERTIES: usize = 50;

/// The `userProperties` document a message's user properties are carried as: "A JSON string
/// that contains an array of JSON objects ... `[{"deviceName": "alpha"}, {"deviceCnt":
/// "45"}]`" (`PublishRequest$userProperties`). `None` when the message carries none, so
/// nothing is read back for a publication that set no property.
#[must_use]
pub fn user_properties_document(properties: &[(String, String)]) -> Option<String> {
    if properties.is_empty() {
        return None;
    }
    let entries: Vec<::serde_json::Value> = properties
        .iter()
        .take(MAX_USER_PROPERTIES)
        .map(|(name, value)| {
            let mut entry = ::serde_json::Map::new();
            entry.insert(name.clone(), ::serde_json::Value::String(value.clone()));
            ::serde_json::Value::Object(entry)
        })
        .collect();
    ::serde_json::to_string(&entries).ok()
}

/// The user properties a `userProperties` document spells, in the order it wrote them: one
/// name and value pair per entry of the array, and nothing at all for a document that is no
/// array of objects of strings.
#[must_use]
pub fn user_property_pairs(document: &str) -> Vec<(String, String)> {
    let Ok(::serde_json::Value::Array(entries)) = ::serde_json::from_str(document) else {
        return Vec::new();
    };
    let mut pairs = Vec::new();
    for entry in entries {
        let ::serde_json::Value::Object(entry) = entry else {
            continue;
        };
        for (name, value) in entry {
            if pairs.len() >= MAX_USER_PROPERTIES {
                return pairs;
            }
            // A value that is not a string is no user property value: MQTT 5 section
            // 3.3.2.3.7 carries both halves of a pair as UTF-8 strings.
            if let ::serde_json::Value::String(value) = value {
                pairs.push((name, value));
            }
        }
    }
    pairs
}

/// The correlation data a message holds, as the bytes the MQTT wire carries: the data is
/// kept base64-encoded (the form `PublishRequest$correlationData` carries it in), and what
/// a subscriber receives is the DECODED bytes. Data that is not base64 at all is carried as
/// the bytes of the text itself, which is what a publisher that sent raw text sent.
#[must_use]
pub fn decode_correlation_data(data: &str) -> Bytes {
    use ::base64::Engine as _;
    match ::base64::engine::general_purpose::STANDARD.decode(data) {
        Ok(decoded) => Bytes::from(decoded),
        Err(_not_base64) => Bytes::from(data.as_bytes().to_vec()),
    }
}

/// The base64 form of the correlation data a client published, which is how the data plane
/// carries it — see [`decode_correlation_data`].
#[must_use]
pub fn encode_correlation_data(data: &[u8]) -> String {
    use ::base64::Engine as _;
    ::base64::engine::general_purpose::STANDARD.encode(data)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One packet the broker writes, as a case names it: what it is, and the render that
    /// lays it out.
    type WrittenPacket<'a> =
        (&'a str, &'a dyn Fn(&mut BytesMut) -> Result<usize, ::mqttbytes::Error>);

    /// One row of the user-property cases: what it is, the pairs, and the document they
    /// are carried as.
    type UserPropertyRow = (
        &'static str,
        &'static [(&'static str, &'static str)],
        Option<&'static str>,
    );

    /// RULE: the protocol level sits at a fixed position in the CONNECT's own variable
    /// header, so the version a connection speaks is read off the packet before any codec
    /// frames it.
    ///
    /// The rows span what the reading generalises over: both levels this broker serves, a
    /// level it does not, a protocol NAME of another length, a remaining length that needs
    /// a second varint byte, and a prefix too short to carry a level at all.
    #[test]
    fn the_announced_protocol_level_is_read_off_a_connect() {
        let rows: &[(&str, &[u8], Option<u8>)] = &[
            (
                "MQTT 3.1.1",
                &[0x10, 0x10, 0x00, 0x04, b'M', b'Q', b'T', b'T', 0x04],
                Some(PROTOCOL_LEVEL_V311),
            ),
            (
                "MQTT 5",
                &[0x10, 0x10, 0x00, 0x04, b'M', b'Q', b'T', b'T', 0x05],
                Some(PROTOCOL_LEVEL_V5),
            ),
            (
                "a level this broker serves no protocol for",
                &[0x10, 0x10, 0x00, 0x04, b'M', b'Q', b'T', b'T', 0x03],
                Some(3),
            ),
            (
                "the longer protocol name of MQTT 3.1",
                &[
                    0x10, 0x12, 0x00, 0x06, b'M', b'Q', b'I', b's', b'd', b'p', 0x03,
                ],
                Some(3),
            ),
            (
                "a remaining length spanning two varint bytes",
                &[0x10, 0x80, 0x02, 0x00, 0x04, b'M', b'Q', b'T', b'T', 0x05],
                Some(PROTOCOL_LEVEL_V5),
            ),
            (
                "fewer bytes than the level's own position",
                &[0x10, 0x10, 0x00, 0x04, b'M', b'Q'],
                None,
            ),
            ("one byte", &[0x10], None),
        ];
        for (case, bytes, expected) in rows {
            let read = announced_protocol_level(bytes);
            assert_eq!(
                read, *expected,
                "case {case:?}: expected {expected:?}, got {read:?}"
            );
            let version = read.and_then(Version::of_protocol_level);
            let serves = matches!(read, Some(PROTOCOL_LEVEL_V311 | PROTOCOL_LEVEL_V5));
            assert_eq!(
                version.is_some(),
                serves,
                "case {case:?}: expected a served version only for levels 4 and 5, got \
                 {version:?}"
            );
        }
    }

    /// RULE: each version's own codec frames that version's CONNECT, and the CONNACK that
    /// answers it is written in codes a client of THAT version reads — so a session opens
    /// for a client of either version AWS IoT Core's broker serves.
    #[test]
    fn a_connect_of_each_served_version_is_framed_and_answered_in_its_own_codes() {
        // MQTT 3.1.1.
        let mut buffer = BytesMut::new();
        let mut connect = v4::Connect::new("probe-v4");
        connect.clean_session = false;
        connect.keep_alive = 45;
        connect.write(&mut buffer).expect("the CONNECT renders");
        assert_eq!(
            announced_protocol_level(&buffer),
            Some(PROTOCOL_LEVEL_V311),
            "case \"MQTT 3.1.1\": expected level 4 announced, got the packet {:?}",
            &buffer[..]
        );
        let framed = frame(Version::V311, &mut buffer, 4096);
        let Framing::Packet(ClientPacket::Connect(read)) = framed else {
            panic!("case \"MQTT 3.1.1\": expected a CONNECT, got {framed:?}");
        };
        assert_eq!(
            (read.client_id.as_str(), read.clean_session, read.keep_alive_seconds),
            ("probe-v4", false, 45),
            "case \"MQTT 3.1.1\": expected what the CONNECT asked for, got {read:?}"
        );
        let mut answered = BytesMut::new();
        render_connack(
            Version::V311,
            &mut answered,
            Connack::Accepted {
                session_present: true,
            },
        )
        .expect("the CONNACK renders");
        let read = v4::read(&mut answered, 4096).expect("a 3.1.1 client reads it");
        let v4::Packet::ConnAck(connack) = read else {
            panic!("case \"MQTT 3.1.1\": expected a CONNACK, got {read:?}");
        };
        assert_eq!(
            (connack.code, connack.session_present),
            (v4::ConnectReturnCode::Success, true),
            "case \"MQTT 3.1.1\": expected an accepted CONNACK, got {connack:?}"
        );

        // MQTT 5, with the session expiry its own protocol carries.
        let mut buffer = BytesMut::new();
        let mut connect = v5::Connect::new("probe-v5");
        connect.clean_session = true;
        connect.keep_alive = 15;
        let properties = v5::ConnectProperties {
            session_expiry_interval: Some(120),
            receive_maximum: None,
            max_packet_size: None,
            topic_alias_max: None,
            request_response_info: None,
            request_problem_info: None,
            user_properties: Vec::new(),
            authentication_method: None,
            authentication_data: None,
        };
        connect.properties = Some(properties);
        connect.write(&mut buffer).expect("the CONNECT renders");
        assert_eq!(
            announced_protocol_level(&buffer),
            Some(PROTOCOL_LEVEL_V5),
            "case \"MQTT 5\": expected level 5 announced, got the packet {:?}",
            &buffer[..]
        );
        let framed = frame(Version::V5, &mut buffer, 4096);
        let Framing::Packet(ClientPacket::Connect(read)) = framed else {
            panic!("case \"MQTT 5\": expected a CONNECT, got {framed:?}");
        };
        assert_eq!(
            (
                read.client_id.as_str(),
                read.clean_session,
                read.keep_alive_seconds,
                read.session_expiry_seconds
            ),
            ("probe-v5", true, 15, 120),
            "case \"MQTT 5\": expected what the CONNECT asked for, got {read:?}"
        );
        let mut answered = BytesMut::new();
        render_connack(
            Version::V5,
            &mut answered,
            Connack::Accepted {
                session_present: false,
            },
        )
        .expect("the CONNACK renders");
        let read = v5::read(&mut answered, 4096).expect("an MQTT 5 client reads it");
        let v5::Packet::ConnAck(connack) = read else {
            panic!("case \"MQTT 5\": expected a CONNACK, got {read:?}");
        };
        assert_eq!(
            (connack.code, connack.session_present),
            (v5::ConnectReturnCode::Success, false),
            "case \"MQTT 5\": expected an accepted CONNACK, got {connack:?}"
        );
    }

    /// RULE: a CONNECT's Last Will and Testament and a PUBLISH's RETAIN flag are read off
    /// the packet, in both versions the broker serves — the will is what the broker
    /// publishes for a session that ended without a DISCONNECT, and the RETAIN flag is what
    /// makes a publication its topic's retained message.
    #[test]
    fn a_connects_will_and_a_publishs_retain_flag_are_read_off_the_packet() {
        // MQTT 3.1.1: a will with its own topic, payload, QoS and RETAIN flag.
        let mut buffer = BytesMut::new();
        let mut connect = v4::Connect::new("will-v4");
        connect.last_will = Some(v4::LastWill::new(
            "devices/will-v4/status",
            b"offline".to_vec(),
            QoS::AtLeastOnce,
            true,
        ));
        connect.write(&mut buffer).expect("the CONNECT renders");
        let framed = frame(Version::V311, &mut buffer, 4096);
        let Framing::Packet(ClientPacket::Connect(read)) = framed else {
            panic!("case \"MQTT 3.1.1 will\": expected a CONNECT, got {framed:?}");
        };
        let will = read
            .last_will
            .clone()
            .unwrap_or_else(|| panic!("case \"MQTT 3.1.1 will\": expected the will, got {read:?}"));
        assert_eq!(
            (
                will.topic.as_str(),
                will.payload.as_slice(),
                will.qos,
                will.retain
            ),
            ("devices/will-v4/status", &b"offline"[..], 1, true),
            "case \"MQTT 3.1.1 will\": expected what the CONNECT configured, got {will:?}"
        );

        // MQTT 5: the same will, carried with its own (empty) property section.
        let mut buffer = BytesMut::new();
        let mut connect = v5::Connect::new("will-v5");
        connect.last_will = Some(v5::LastWill::new(
            "devices/will-v5/status",
            b"gone".to_vec(),
            QoS::AtMostOnce,
            false,
        ));
        connect.write(&mut buffer).expect("the CONNECT renders");
        let framed = frame(Version::V5, &mut buffer, 4096);
        let Framing::Packet(ClientPacket::Connect(read)) = framed else {
            panic!("case \"MQTT 5 will\": expected a CONNECT, got {framed:?}");
        };
        let will = read
            .last_will
            .clone()
            .unwrap_or_else(|| panic!("case \"MQTT 5 will\": expected the will, got {read:?}"));
        assert_eq!(
            (
                will.topic.as_str(),
                will.payload.as_slice(),
                will.qos,
                will.retain
            ),
            ("devices/will-v5/status", &b"gone"[..], 0, false),
            "case \"MQTT 5 will\": expected what the CONNECT configured, got {will:?}"
        );

        // A CONNECT that configured no will carries none, in either version.
        for (case, version, rendered) in [
            ("MQTT 3.1.1, no will", Version::V311, {
                let mut buffer = BytesMut::new();
                v4::Connect::new("bare-v4")
                    .write(&mut buffer)
                    .expect("the CONNECT renders");
                buffer
            }),
            ("MQTT 5, no will", Version::V5, {
                let mut buffer = BytesMut::new();
                v5::Connect::new("bare-v5")
                    .write(&mut buffer)
                    .expect("the CONNECT renders");
                buffer
            }),
        ] {
            let mut buffer = rendered;
            let framed = frame(version, &mut buffer, 4096);
            let Framing::Packet(ClientPacket::Connect(read)) = framed else {
                panic!("case {case:?}: expected a CONNECT, got {framed:?}");
            };
            assert!(
                read.last_will.is_none(),
                "case {case:?}: expected no will, got {:?}",
                read.last_will
            );
        }

        // The RETAIN flag of a publication, in both versions.
        for (case, version, rendered) in [
            ("MQTT 3.1.1 retained publish", Version::V311, {
                let mut buffer = BytesMut::new();
                let mut publish =
                    v4::Publish::new("devices/sensor-1/config", QoS::AtLeastOnce, b"keep".to_vec());
                publish.retain = true;
                publish.pkid = 3;
                publish.write(&mut buffer).expect("the PUBLISH renders");
                buffer
            }),
            ("MQTT 5 retained publish", Version::V5, {
                let mut buffer = BytesMut::new();
                let mut publish =
                    v5::Publish::new("devices/sensor-1/config", QoS::AtLeastOnce, b"keep".to_vec());
                publish.retain = true;
                publish.pkid = 4;
                publish.write(&mut buffer).expect("the PUBLISH renders");
                buffer
            }),
        ] {
            let mut buffer = rendered;
            let framed = frame(version, &mut buffer, 4096);
            let Framing::Packet(ClientPacket::Publish { message, .. }) = framed else {
                panic!("case {case:?}: expected a PUBLISH, got {framed:?}");
            };
            assert!(
                message.retain,
                "case {case:?}: expected the RETAIN flag read off the packet, got {message:?}"
            );
        }
    }

    /// RULE: every packet the broker writes to a client is one that client's own codec
    /// reads back — the acknowledgements and the forwarded publication alike, in both
    /// versions. A packet a client cannot read is a session that hangs rather than fails.
    ///
    /// The rows span what the writing generalises over: a SUBACK granting a QoS and a
    /// SUBACK refusing a filter for each of its reasons, the acknowledgements of a
    /// publication and an unsubscribe, the keep-alive answer, and a forwarded publication
    /// with every property set and with none.
    #[test]
    fn every_packet_the_broker_writes_is_read_back_by_that_versions_codec() {
        let loaded = DirectMessage {
            topic: "devices/sensor-1/telemetry".to_string(),
            payload: b"{\"t\":21}".to_vec(),
            qos: 1,
            content_type: Some("application/json".to_string()),
            response_topic: Some("devices/sensor-1/reply".to_string()),
            user_properties: Some(r#"[{"a":"alpha"}]"#.to_string()),
            payload_format_indicator: Some(UTF8_DATA.to_string()),
            correlation_data: Some(encode_correlation_data(b"corr-1")),
            message_expiry_seconds: Some(300),
            delivered_at_millis: 1_700_000_000_000,
            retain: false,
        };
        let bare = DirectMessage {
            topic: "devices/sensor-2/telemetry".to_string(),
            payload: b"plain".to_vec(),
            ..DirectMessage::default()
        };
        for version in [Version::V311, Version::V5] {
            let granted = [
                Grant::Qos(0),
                Grant::Qos(1),
                Grant::TopicFilterInvalid,
                Grant::QuotaExceeded,
            ];
            let packets: &[WrittenPacket<'_>] = &[
                ("a SUBACK", &|buffer: &mut BytesMut| {
                    render_suback(version, buffer, 7, &granted)
                }),
                ("a PUBACK", &|buffer: &mut BytesMut| {
                    render_puback(version, buffer, 8)
                }),
                ("an UNSUBACK", &|buffer: &mut BytesMut| {
                    render_unsuback(version, buffer, 9, 2)
                }),
                ("a PINGRESP", &|buffer: &mut BytesMut| {
                    render_pingresp(version, buffer)
                }),
                ("a publication carrying every property", &|buffer: &mut BytesMut| {
                    render_publish(version, buffer, &loaded, 10, false, loaded.delivered_at_millis)
                }),
                ("a publication carrying none", &|buffer: &mut BytesMut| {
                    render_publish(version, buffer, &bare, 0, false, 1_700_000_000_000)
                }),
                ("a retained publication answering a subscription", &|buffer: &mut BytesMut| {
                    render_publish(version, buffer, &loaded, 11, true, loaded.delivered_at_millis)
                }),
            ];
            for (case, render) in packets {
                let case = format!("{version:?}: {case}");
                let mut buffer = BytesMut::new();
                render(&mut buffer)
                    .unwrap_or_else(|error| panic!("case {case:?}: the packet renders: {error}"));
                let written = buffer.len();
                let read = match version {
                    Version::V311 => v4::read(&mut buffer, 4096).map(|_packet| ()),
                    Version::V5 => v5::read(&mut buffer, 4096).map(|_packet| ()),
                };
                read.unwrap_or_else(|error| {
                    panic!("case {case:?}: the client reads the {written} bytes written: {error}")
                });
                assert!(
                    buffer.is_empty(),
                    "case {case:?}: expected the whole packet consumed, {} bytes are left",
                    buffer.len()
                );
            }
        }
    }

    /// RULE: a forwarded MQTT 5 publication carries the message's own properties, with the
    /// Message Expiry Interval reduced by the time the message waited in the broker.
    #[test]
    fn a_forwarded_publication_carries_the_messages_properties() {
        let published_at = 1_700_000_000_000;
        let message = DirectMessage {
            topic: "devices/sensor-1/telemetry".to_string(),
            payload: b"{\"t\":21}".to_vec(),
            qos: 1,
            content_type: Some("application/json".to_string()),
            response_topic: Some("devices/sensor-1/reply".to_string()),
            user_properties: Some(r#"[{"a":"alpha"},{"b":"beta"}]"#.to_string()),
            payload_format_indicator: Some(UTF8_DATA.to_string()),
            correlation_data: Some(encode_correlation_data(&[0x00, 0xff])),
            message_expiry_seconds: Some(300),
            delivered_at_millis: published_at,
            retain: false,
        };
        // The rows span the wait the message had before it was forwarded: none, part of the
        // interval, and longer than the interval, which spends it.
        let rows: &[(&str, i64, u32)] = &[
            ("forwarded at once", published_at, 300),
            ("forwarded after a 30 second wait", published_at + 30_000, 270),
            ("forwarded past the interval", published_at + 400_000, 0),
        ];
        for (case, now_millis, expected) in rows {
            let mut buffer = BytesMut::new();
            render_publish(Version::V5, &mut buffer, &message, 4, false, *now_millis)
                .expect("the publication renders");
            let read = v5::read(&mut buffer, 4096).expect("an MQTT 5 client reads it");
            let v5::Packet::Publish(publish) = read else {
                panic!("case {case:?}: expected a PUBLISH, got {read:?}");
            };
            let properties = publish
                .properties
                .unwrap_or_else(|| panic!("case {case:?}: expected a property section"));
            assert_eq!(
                properties.message_expiry_interval,
                Some(*expected),
                "case {case:?}: expected {expected} seconds left, got {:?}",
                properties.message_expiry_interval
            );
            assert_eq!(
                (
                    properties.content_type.as_deref(),
                    properties.response_topic.as_deref(),
                    properties.payload_format_indicator,
                    properties.correlation_data.as_deref(),
                    properties.user_properties.len()
                ),
                (
                    Some("application/json"),
                    Some("devices/sensor-1/reply"),
                    Some(UTF8_DATA_INDICATOR),
                    Some(&[0x00u8, 0xff][..]),
                    2
                ),
                "case {case:?}: expected the message's own properties, got {properties:?}"
            );
        }

        // A message carrying no property is forwarded with an EMPTY property section: the
        // section is part of every MQTT 5 PUBLISH, and a client reading a section with
        // nothing in it reads no property — which is what a publication made without any
        // must arrive as.
        let bare = DirectMessage {
            topic: "devices/sensor-2/telemetry".to_string(),
            payload: b"plain".to_vec(),
            ..DirectMessage::default()
        };
        let mut buffer = BytesMut::new();
        render_publish(Version::V5, &mut buffer, &bare, 0, false, published_at)
            .expect("the publication renders");
        let read = v5::read(&mut buffer, 4096).expect("an MQTT 5 client reads it");
        let v5::Packet::Publish(publish) = read else {
            panic!("case \"no property at all\": expected a PUBLISH, got {read:?}");
        };
        assert!(
            publish.properties.is_none(),
            "case \"no property at all\": expected no property read back, got {:?}",
            publish.properties
        );
    }

    /// RULE: a message's user properties round-trip through the `userProperties` document
    /// the data plane carries them in — an array of one-entry JSON objects — and a message
    /// that carries none has no document at all.
    ///
    /// The rows span what the rule generalises over: no properties, one, several (whose
    /// ORDER is kept), a repeated name, a value that is not a string, and a document that
    /// is no array of objects.
    #[test]
    fn user_properties_round_trip_through_their_document() {
        let rows: &[UserPropertyRow] = &[
            ("none", &[], None),
            (
                "one",
                &[("deviceName", "alpha")],
                Some(r#"[{"deviceName":"alpha"}]"#),
            ),
            (
                "several, in the order they were written",
                &[("b", "beta"), ("a", "alpha")],
                Some(r#"[{"b":"beta"},{"a":"alpha"}]"#),
            ),
            (
                "the same name twice",
                &[("tag", "one"), ("tag", "two")],
                Some(r#"[{"tag":"one"},{"tag":"two"}]"#),
            ),
        ];
        for (case, pairs, expected) in rows {
            let owned: Vec<(String, String)> = pairs
                .iter()
                .map(|(name, value)| ((*name).to_string(), (*value).to_string()))
                .collect();
            let document = user_properties_document(&owned);
            assert_eq!(
                document.as_deref(),
                *expected,
                "case {case:?}: expected {expected:?}, got {document:?}"
            );
            let read = document.as_deref().map(user_property_pairs).unwrap_or_default();
            assert_eq!(
                read, owned,
                "case {case:?}: expected the pairs back, got {read:?}"
            );
        }

        // What is NOT a pair of strings is no user property, and what is no array of
        // objects spells none.
        let unreadable: &[(&str, &str)] = &[
            ("a value that is not a string", r#"[{"count":45}]"#),
            ("an array of scalars", r#"["alpha"]"#),
            ("an object", r#"{"deviceName":"alpha"}"#),
            ("not JSON at all", "deviceName=alpha"),
        ];
        for (case, document) in unreadable {
            let read = user_property_pairs(document);
            assert!(
                read.is_empty(),
                "case {case:?}: expected no user property, got {read:?}"
            );
        }
    }

    /// RULE: correlation data is carried base64-encoded and reaches the wire as the decoded
    /// bytes, so what a subscriber receives is the bytes the publisher identified its
    /// request with.
    ///
    /// The rows span what the rule generalises over: empty data, text, bytes that are not
    /// UTF-8 at all, and a string that is no base64 at all.
    #[test]
    fn correlation_data_reaches_the_wire_as_the_decoded_bytes() {
        let rows: &[(&str, &[u8])] = &[
            ("empty", b""),
            ("text", b"correlation-7"),
            ("bytes that are no UTF-8", &[0x00, 0xff, 0x10, 0x80]),
        ];
        for (case, raw) in rows {
            let encoded = encode_correlation_data(raw);
            let decoded = decode_correlation_data(&encoded);
            assert_eq!(
                decoded.as_ref(),
                *raw,
                "case {case:?}: expected the published bytes, got {decoded:?}"
            );
        }
        let text = decode_correlation_data("not base64!!");
        assert_eq!(
            text.as_ref(),
            b"not base64!!",
            "case \"no base64 at all\": expected the text's own bytes, got {text:?}"
        );
    }

    /// RULE: a payload format indicator round-trips through the data plane's own modelled
    /// token — MQTT 5 section 3.3.2.3.2's 0 and 1 — and a token the model does not carry
    /// names no format.
    #[test]
    fn a_payload_format_indicator_round_trips_through_its_token() {
        let rows: &[(&str, u8, &str)] = &[
            ("unspecified bytes", UNSPECIFIED_BYTES_INDICATOR, UNSPECIFIED_BYTES),
            ("UTF-8 data", UTF8_DATA_INDICATOR, UTF8_DATA),
        ];
        for (case, indicator, token) in rows {
            let read = payload_format_token(*indicator);
            assert_eq!(
                read, *token,
                "case {case:?}: expected {token:?}, got {read:?}"
            );
            let back = payload_format_indicator(&read);
            assert_eq!(
                back,
                Some(*indicator),
                "case {case:?}: expected {indicator} back, got {back:?}"
            );
        }
        let unmodelled = payload_format_indicator("TEXT");
        assert!(
            unmodelled.is_none(),
            "case \"a token the model does not carry\": expected no format, got {unmodelled:?}"
        );
    }
}
