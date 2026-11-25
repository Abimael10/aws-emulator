// SPDX-License-Identifier: CC0-1.0

//! FLEET PROVISIONING over the broker's reserved MQTT topics: the exchange a device that
//! holds only a claim certificate runs to obtain its own identity and register its thing.
//!
//! The rule under test: a device provisions itself on the reserved topics of the address
//! `DescribeEndpoint` publishes, and every request there is ANSWERED on that topic's own
//! `accepted` or `rejected` topic.
//!
//! * `$aws/certificates/create/json` creates a certificate for the device and answers it
//!   with the certificate, its id, the private key and a `certificateOwnershipToken`. The
//!   certificate is recorded `PENDING_ACTIVATION`: a device cannot connect with a
//!   certificate it created but has not provisioned.
//! * `$aws/provisioning-templates/{templateName}/provision/json` carries out that template
//!   for the certificate the token proves ownership of — the thing, its attributes, the
//!   policy and the certificate's own status — and answers with the thing that was
//!   registered and the template's `DeviceConfiguration`.
//! * a request that cannot be carried out is answered on the `rejected` topic with the
//!   status and the error class a caller of the equivalent API would be refused with, never
//!   left silent.
//!
//! Everything is read back through the control plane afterwards — `DescribeThing`,
//! `ListThingPrincipals`, `DescribeCertificate` — so what the exchange reports is what the
//! registries hold, not what one answer claimed.
//!
//! The rows differ along what the rule generalises over: how the template spells the
//! certificate status it asks for (AWS's own fleet-provisioning spelling `Active`, the
//! modelled `ACTIVE`, and a template that asks for none), whether the template declares a
//! policy, whether it declares a `DeviceConfiguration` and whether that section is built
//! from a supplied parameter or a defaulted one, and which parameters the request supplies.
//! The refusal rows span each part of a request that can be at fault.
//!
//! The session is a real socket to the published address, with the packets laid out by the
//! same maintained MQTT codec a device library uses; the encrypted half of that address is
//! driven by `data_endpoint_mqtt_transport.rs` and is not re-driven here. Every wait is on a
//! CONDITION (a packet the broker writes) under a generous deadline, so a loaded host
//! passes.

use std::net::Ipv4Addr;
use std::sync::Arc;
use std::time::Duration;

use bytes::BytesMut;
use serde_json::{json, Value};
use service_com_amazonaws_iot_awsiotservice::state::{
    CERTIFICATE_STATUS_ACTIVE, CERTIFICATE_STATUS_INACTIVE,
};
use service_com_amazonaws_iot_awsiotservice::EmulatedService;
// The MQTT codec the broker itself reads and writes the protocol with, re-exported by the
// crate that holds the broker: the suite speaks the wire a device library speaks rather than
// laying packets out by hand.
use service_common::iot_mqtt_broker::mqttbytes::{self, v4, QoS};
use smithy_server_runtime::{EndpointUrl, MultiServiceRouter};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

// The percent-encoding of a query value, as this crate's own test tree already exports it:
// `automod` declares every suite as a sibling module, so the endpoint discovery below is
// written with that helper rather than with a copy of it.
use crate::topic_rule_kinesis_action::form_encode;

/// The Region every request below is signed for.
const REGION: &str = "us-east-1";

/// The address this emulator's API is served at. The broker's own socket is a DIFFERENT
/// address — one port serves one wire — and which one that is, is the core's to choose, so
/// the device reads it back rather than naming it.
const SERVED_AT: &str = "https://127.0.0.1:41811";

/// How long one leg of the exchange may take before the case fails. Generous: it bounds a
/// wait on a CONDITION (a packet the broker writes), not the speed of the host.
const EXCHANGE_DEADLINE: Duration = Duration::from_secs(60);

/// The role every provisioning template below provisions under. No IAM is mounted in this
/// harness, so nothing is ANSWERED about the role and the template is stored as written —
/// which is what `provisioning_role_trust.rs` covers on its own.
const PROVISIONING_ROLE: &str = "arn:aws:iam::123456789012:role/fleet-provisioning";

/// The status a certificate a device created over this exchange is recorded in until a
/// template activates it: AWS IoT creates a fleet-provisioned certificate in
/// `PENDING_ACTIVATION`. Written out here rather than read from the crate, so the suite pins
/// the status instead of whatever the emulator happens to hold.
const PENDING_ACTIVATION: &str = "PENDING_ACTIVATION";

/// The reserved topic a device asks for its own certificate on.
const CREATE_TOPIC: &str = "$aws/certificates/create/json";

/// The reserved topic a device that generated its own key pair asks on.
const CREATE_FROM_CSR_TOPIC: &str = "$aws/certificates/create-from-csr/json";

/// The policy document a template's policy resource declares, as the template carries it: a
/// string holding an IoT policy that lets the device connect.
fn policy_document() -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Action": ["iot:Connect", "iot:Publish"],
            "Resource": "*",
        }],
    })
    .to_string()
}

/// One provisioning template body: a thing named as `thing_name_property` states and
/// carrying the `Site` parameter as an attribute, the device's certificate (with
/// `certificate_status` where one is asked for), the policy where `declares_policy` is set,
/// the `Mappings` the body declares where it declares any, and `section` as the template's
/// own `DeviceConfiguration` where it declares one.
struct Template {
    /// The `ThingName` property, as the template states it: a reference to a parameter, or a
    /// value an intrinsic function composes.
    thing_name_property: Value,
    /// The `Status` the certificate resource asks for, where it asks for one.
    certificate_status: Option<&'static str>,
    /// Whether the template declares the device's policy beside its thing.
    declares_policy: bool,
    /// The template's `Mappings` section, where it declares one.
    mappings: Option<Value>,
    /// The template's own `DeviceConfiguration` section, where it declares one.
    device_configuration: Option<Value>,
}

impl Default for Template {
    fn default() -> Self {
        Self {
            thing_name_property: json!({"Ref": "SerialNumber"}),
            certificate_status: Some("Active"),
            declares_policy: false,
            mappings: None,
            device_configuration: None,
        }
    }
}

impl Template {
    /// The body `CreateProvisioningTemplate` is given.
    fn body(&self) -> String {
        let mut resources = json!({
            "thing": {
                "Type": "AWS::IoT::Thing",
                "Properties": {
                    "ThingName": self.thing_name_property.clone(),
                    "AttributePayload": {"site": {"Ref": "Site"}},
                },
            },
            "certificate": {
                "Type": "AWS::IoT::Certificate",
                "Properties": {"CertificateId": {"Ref": "AWS::IoT::Certificate::Id"}},
            },
        });
        if let Some(status) = self.certificate_status {
            resources["certificate"]["Properties"]["Status"] = json!(status);
        }
        if self.declares_policy {
            resources["policy"] = json!({
                "Type": "AWS::IoT::Policy",
                "Properties": {"PolicyDocument": policy_document()},
            });
        }
        let mut body = json!({
            "Parameters": {
                "SerialNumber": {"Type": "String"},
                "Site": {"Type": "String", "Default": "plant-9"},
                "DeviceLocation": {"Type": "String", "Default": "Seattle"},
            },
            "Resources": resources,
        });
        if let Some(mappings) = &self.mappings {
            body["Mappings"] = mappings.clone();
        }
        if let Some(section) = &self.device_configuration {
            body["DeviceConfiguration"] = section.clone();
        }
        body.to_string()
    }
}

/// One device provisioned end to end.
struct Row {
    case: &'static str,
    /// The template the device provisions with, and the client id it connects under.
    template_name: &'static str,
    client_id: &'static str,
    /// The template itself.
    template: Template,
    /// The `parameters` the provision request supplies.
    parameters: Value,
    /// The thing the template must then have registered, and the attributes it must carry.
    thing_name: &'static str,
    attributes: Value,
    /// The `deviceConfiguration` the accepted answer must carry.
    device_configuration: Value,
    /// The status the device's certificate must be left in.
    certificate_status: &'static str,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "AWS's own spelling of the status, a policy, and a configuration built \
                   from a defaulted parameter",
            template_name: "fleet-alpha",
            client_id: "provisioning-device-alpha",
            template: Template {
                certificate_status: Some("Active"),
                declares_policy: true,
                device_configuration: Some(json!({
                    "site": {"Ref": "Site"},
                    "fleet": "north",
                })),
                ..Template::default()
            },
            parameters: json!({"SerialNumber": "SN-7"}),
            thing_name: "SN-7",
            attributes: json!({"site": "plant-9"}),
            device_configuration: json!({"site": "plant-9", "fleet": "north"}),
            certificate_status: CERTIFICATE_STATUS_ACTIVE,
        },
        Row {
            case: "the modelled spelling of the status, no policy, no configuration, every \
                   parameter supplied",
            template_name: "fleet-beta",
            client_id: "provisioning-device-beta",
            template: Template {
                certificate_status: Some(CERTIFICATE_STATUS_ACTIVE),
                ..Template::default()
            },
            parameters: json!({"SerialNumber": "SN-8", "Site": "plant-3"}),
            thing_name: "SN-8",
            attributes: json!({"site": "plant-3"}),
            device_configuration: json!({}),
            certificate_status: CERTIFICATE_STATUS_ACTIVE,
        },
        Row {
            case: "a thing name composed by Fn::Join and a configuration looked up by \
                   Fn::FindInMap, as AWS's own documented template states them",
            template_name: "fleet-epsilon",
            client_id: "provisioning-device-epsilon",
            template: Template {
                thing_name_property: json!({
                    "Fn::Join": ["", ["ThingPrefix_", {"Ref": "SerialNumber"}]],
                }),
                declares_policy: true,
                mappings: Some(json!({
                    "LocationTable": {
                        "Seattle": {"LocationUrl": "https://example.aws"},
                        "Dublin": {"LocationUrl": "https://eu.example.aws"},
                    },
                })),
                device_configuration: Some(json!({
                    "FallbackUrl": "https://www.example.com/test-site",
                    "LocationUrl": {
                        "Fn::FindInMap": [
                            "LocationTable",
                            {"Ref": "DeviceLocation"},
                            "LocationUrl",
                        ],
                    },
                })),
                ..Template::default()
            },
            parameters: json!({
                "SerialNumber": "SN-11",
                "Site": "plant-6",
                "DeviceLocation": "Dublin",
            }),
            thing_name: "ThingPrefix_SN-11",
            attributes: json!({"site": "plant-6"}),
            device_configuration: json!({
                "FallbackUrl": "https://www.example.com/test-site",
                "LocationUrl": "https://eu.example.aws",
            }),
            certificate_status: CERTIFICATE_STATUS_ACTIVE,
        },
        Row {
            case: "a template that asks for no status at all, so the certificate stays as \
                   the exchange created it",
            template_name: "fleet-gamma",
            client_id: "provisioning-device-gamma",
            template: Template {
                certificate_status: None,
                declares_policy: true,
                device_configuration: Some(json!({"serial": {"Ref": "SerialNumber"}})),
                ..Template::default()
            },
            parameters: json!({"SerialNumber": "SN-9", "Site": "plant-4"}),
            thing_name: "SN-9",
            attributes: json!({"site": "plant-4"}),
            device_configuration: json!({"serial": "SN-9"}),
            certificate_status: PENDING_ACTIVATION,
        },
        Row {
            case: "a template that leaves the certificate INACTIVE",
            template_name: "fleet-delta",
            client_id: "provisioning-device-delta",
            template: Template {
                certificate_status: Some(CERTIFICATE_STATUS_INACTIVE),
                device_configuration: Some(json!({"fleet": "south"})),
                ..Template::default()
            },
            parameters: json!({"SerialNumber": "SN-10", "Site": "plant-5"}),
            thing_name: "SN-10",
            attributes: json!({"site": "plant-5"}),
            device_configuration: json!({"fleet": "south"}),
            certificate_status: CERTIFICATE_STATUS_INACTIVE,
        },
    ]
}

/// The emulator: the IoT control plane on one engine, served at [`SERVED_AT`]. The broker
/// the reserved topics live on is in the crate below it (`service_common`), opened by the
/// call that publishes the data endpoint's address.
struct Emulator {
    router: MultiServiceRouter,
}

fn emulator() -> Emulator {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    // Signature verification is the front door's concern, not this suite's: the credential
    // scope is what routes a request here.
    router.set_request_authentication(false);
    router
        .set_url_base(EndpointUrl::parse(SERVED_AT).expect("the served base is a usable endpoint"));
    router.register_service(EmulatedService::new(engine));
    Emulator { router }
}

/// A SigV4 credential scope naming `iot` in [`REGION`] — the signal the router picks the
/// owning service by. The signature is never checked in this harness.
fn authorization() -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/iot/aws4_request, \
         SignedHeaders=host, Signature=0000"
    )
}

impl Emulator {
    /// One control-plane call through the router, as its status and its parsed body.
    async fn send(&self, method: &str, path: &str, body: Vec<u8>) -> (u16, String, Value) {
        let request = http::Request::builder()
            .method(method)
            .uri(format!("{SERVED_AT}{path}"))
            .header("host", format!("iot.{REGION}.amazonaws.com"))
            .header("authorization", authorization())
            .header("content-type", "application/json")
            .body(bytes::Bytes::from(body))
            .expect("the test request is well formed");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let rendered = String::from_utf8_lossy(response.body()).to_string();
        let parsed: Value = serde_json::from_str(&rendered).unwrap_or(Value::Null);
        (status, rendered, parsed)
    }

    /// `GET /endpoint?endpointType=iot:Data-ATS`: the address the control plane publishes for
    /// the account's data endpoint, which is also the call that OPENS the broker's socket.
    async fn discover(&self) -> String {
        let (status, rendered, parsed) = self
            .send(
                "GET",
                &format!("/endpoint?endpointType={}", form_encode("iot:Data-ATS")),
                Vec::new(),
            )
            .await;
        assert_eq!(
            status, 200,
            "setup: DescribeEndpoint answered {status} with {rendered}"
        );
        parsed
            .get("endpointAddress")
            .and_then(Value::as_str)
            .unwrap_or_else(|| panic!("setup: no endpointAddress in {rendered}"))
            .to_string()
    }

    /// `POST /provisioning-templates` — the template a device then provisions with.
    async fn create_template(&self, case: &str, template_name: &str, body: &str, enabled: bool) {
        let (status, rendered, _) = self
            .send(
                "POST",
                "/provisioning-templates",
                json!({
                    "templateName": template_name,
                    "templateBody": body,
                    "provisioningRoleArn": PROVISIONING_ROLE,
                    "enabled": enabled,
                })
                .to_string()
                .into_bytes(),
            )
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: CreateProvisioningTemplate({template_name}) answered {status} with \
             {rendered}"
        );
    }

    /// `GET /certificates/{certificateId}` — the status the registry holds the certificate in.
    async fn certificate_status(&self, case: &str, certificate_id: &str) -> String {
        let (status, rendered, parsed) = self
            .send("GET", &format!("/certificates/{certificate_id}"), Vec::new())
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: DescribeCertificate({certificate_id}) answered {status} with \
             {rendered}"
        );
        parsed["certificateDescription"]["status"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: no status in {rendered}"))
            .to_string()
    }

    /// `GET /things/{thingName}` — the thing the provisioning run registered, as its status
    /// and its parsed body.
    async fn describe_thing(&self, thing_name: &str) -> (u16, Value) {
        let (status, _, parsed) = self
            .send("GET", &format!("/things/{thing_name}"), Vec::new())
            .await;
        (status, parsed)
    }

    /// `GET /things/{thingName}/principals` — the principals the thing is reached through.
    async fn thing_principals(&self, case: &str, thing_name: &str) -> Vec<String> {
        let (status, rendered, parsed) = self
            .send("GET", &format!("/things/{thing_name}/principals"), Vec::new())
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: ListThingPrincipals({thing_name}) answered {status} with {rendered}"
        );
        parsed["principals"]
            .as_array()
            .map(|principals| {
                principals
                    .iter()
                    .filter_map(|principal| principal.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default()
    }
}

/// A DEVICE: a real socket to the published address, speaking MQTT 3.1.1 on it.
///
/// Written here rather than taken from `data_endpoint_mqtt_transport.rs`, whose device is
/// private to that suite and drives the encrypted transport and its sentinel protocol; what
/// this suite needs is a plain session that publishes a request and reads the answer.
struct Device {
    case: &'static str,
    stream: TcpStream,
    buffer: BytesMut,
    next_packet_id: u16,
}

impl Device {
    /// Dial `address`, send the CONNECT and assert the session was opened.
    async fn connect(case: &'static str, address: &str, client_id: &str) -> Self {
        let port = loopback_port(case, address);
        let dialling = TcpStream::connect((Ipv4Addr::LOCALHOST, port));
        let stream = tokio::time::timeout(EXCHANGE_DEADLINE, dialling)
            .await
            .unwrap_or_else(|_| panic!("case {case:?}: dialling {address} timed out"))
            .unwrap_or_else(|error| {
                panic!(
                    "case {case:?}: the published address {address} must accept a connection: \
                     {error}"
                )
            });
        let mut device = Self {
            case,
            stream,
            buffer: BytesMut::new(),
            next_packet_id: 1,
        };
        let mut connect = v4::Connect::new(client_id);
        connect.clean_session = true;
        connect.keep_alive = 60;
        device.write(|buffer| connect.write(buffer)).await;
        let answered = device.read_packet().await;
        let v4::Packet::ConnAck(connack) = answered else {
            panic!("case {case:?}: expected a CONNACK, got {answered:?}");
        };
        assert_eq!(
            connack.code,
            v4::ConnectReturnCode::Success,
            "case {case:?}: expected the CONNECT accepted, got {:?}",
            connack.code
        );
        device
    }

    /// Write one packet, rendered by the codec.
    async fn write<Render>(&mut self, render: Render)
    where
        Render: FnOnce(&mut BytesMut) -> Result<usize, mqttbytes::Error>,
    {
        let mut rendered = BytesMut::new();
        render(&mut rendered)
            .unwrap_or_else(|error| panic!("case {:?}: the packet renders: {error}", self.case));
        self.stream
            .write_all(&rendered)
            .await
            .unwrap_or_else(|error| panic!("case {:?}: the socket takes it: {error}", self.case));
    }

    /// Read one packet the broker wrote, waiting for it until [`EXCHANGE_DEADLINE`].
    async fn read_packet(&mut self) -> v4::Packet {
        let case = self.case;
        let read = tokio::time::timeout(EXCHANGE_DEADLINE, async {
            loop {
                match v4::read(&mut self.buffer, 256 * 1024) {
                    Ok(packet) => return Some(packet),
                    Err(mqttbytes::Error::InsufficientBytes(_)) => {}
                    Err(error) => panic!("case {case:?}: the broker wrote no packet: {error}"),
                }
                let mut chunk = [0u8; 4096];
                match self.stream.read(&mut chunk).await {
                    Ok(0) => return None,
                    Ok(read) => self.buffer.extend_from_slice(&chunk[..read]),
                    Err(error) => panic!("case {case:?}: reading the socket failed: {error}"),
                }
            }
        })
        .await
        .unwrap_or_else(|_| panic!("case {case:?}: the broker wrote nothing in time"));
        read.unwrap_or_else(|| panic!("case {case:?}: the broker closed the connection"))
    }

    /// Subscribe to `filters` at QoS 1, asserting every one was granted.
    async fn subscribe(&mut self, filters: &[String]) {
        let mut subscribe = v4::Subscribe::new(filters[0].clone(), QoS::AtLeastOnce);
        subscribe.pkid = self.take_packet_id();
        for path in &filters[1..] {
            subscribe
                .filters
                .push(v4::SubscribeFilter::new(path.clone(), QoS::AtLeastOnce));
        }
        self.write(|buffer| subscribe.write(buffer)).await;
        let answered = self.read_packet().await;
        let v4::Packet::SubAck(suback) = answered else {
            panic!("case {:?}: expected a SUBACK, got {answered:?}", self.case);
        };
        assert_eq!(
            suback.return_codes,
            vec![v4::SubscribeReasonCode::Success(QoS::AtLeastOnce); filters.len()],
            "case {:?}: expected QoS 1 granted for {filters:?}, got {:?}",
            self.case,
            suback.return_codes
        );
    }

    /// Publish one request on `topic` at QoS 0 — the device's own message to the service,
    /// which is answered on the topic's `accepted` or `rejected` topic rather than
    /// acknowledged.
    async fn publish(&mut self, topic: &str, payload: Vec<u8>) {
        let publish = v4::Publish::new(topic, QoS::AtMostOnce, payload);
        self.write(|buffer| publish.write(buffer)).await;
    }

    /// Read the next publication the broker pushed, as its topic and the JSON document it
    /// carries, acknowledging it when it was made at QoS 1.
    async fn read_answer(&mut self) -> (String, Value) {
        let packet = self.read_packet().await;
        let v4::Packet::Publish(publish) = packet else {
            panic!("case {:?}: expected an answer, got {packet:?}", self.case);
        };
        if publish.qos != QoS::AtMostOnce {
            let pkid = publish.pkid;
            self.write(|buffer| v4::PubAck::new(pkid).write(buffer))
                .await;
        }
        let body: Value = serde_json::from_slice(&publish.payload).unwrap_or_else(|error| {
            panic!(
                "case {:?}: the answer on {} must be a JSON document: {error}",
                self.case, publish.topic
            )
        });
        (publish.topic, body)
    }

    /// The packet identifier the next packet carries: non-zero and distinct across the
    /// packets in flight, which is all one has to be.
    fn take_packet_id(&mut self) -> u16 {
        let packet_id = self.next_packet_id;
        self.next_packet_id = self.next_packet_id.checked_add(1).unwrap_or(1);
        packet_id
    }
}

/// The port a published address names, checked to be on the loopback interface this
/// emulator's listeners are bound on.
fn loopback_port(case: &str, address: &str) -> u16 {
    let (host, port) = address
        .rsplit_once(':')
        .unwrap_or_else(|| panic!("case {case:?}: {address:?} names no port"));
    assert_eq!(
        host,
        Ipv4Addr::LOCALHOST.to_string(),
        "case {case:?}: expected the published address on the loopback interface, got {address:?}"
    );
    port.parse()
        .unwrap_or_else(|error| panic!("case {case:?}: {address:?} names no port: {error}"))
}

/// The thing the refusal rows' template would register, had any of their requests been
/// carried out: no registry holds it afterwards, which is what says they provisioned nothing.
const REFUSED_THING: &str = "SN-refused";

/// The topic one template's provision requests are made on.
fn provision_topic(template_name: &str) -> String {
    format!("$aws/provisioning-templates/{template_name}/provision/json")
}

/// The member one answer must carry, read with the case in the failure.
fn member<'a>(case: &str, topic: &str, body: &'a Value, name: &str) -> &'a Value {
    body.get(name).unwrap_or_else(|| {
        panic!("case {case:?}: the answer on {topic} must carry {name:?}, got {body}")
    })
}

/// A device creates its own certificate on the reserved topic, is answered with the material
/// and an ownership token, and provisions its thing with that token — and every registry the
/// template wrote reports what the answers said.
#[tokio::test]
async fn a_device_creates_its_certificate_and_provisions_its_thing_over_the_reserved_topics() {
    let emulator = emulator();
    let address = emulator.discover().await;

    for row in rows() {
        let case = row.case;
        emulator
            .create_template(case, row.template_name, &row.template.body(), true)
            .await;

        let provision = provision_topic(row.template_name);
        let mut device = Device::connect(case, &address, row.client_id).await;
        device
            .subscribe(&[
                format!("{CREATE_TOPIC}/accepted"),
                format!("{CREATE_TOPIC}/rejected"),
                format!("{provision}/accepted"),
                format!("{provision}/rejected"),
            ])
            .await;

        // The certificate the device asks for: the create request carries no member at all.
        device.publish(CREATE_TOPIC, b"{}".to_vec()).await;
        let (topic, created) = device.read_answer().await;
        assert_eq!(
            topic,
            format!("{CREATE_TOPIC}/accepted"),
            "case {case:?}: expected the create accepted, got {topic} with {created}"
        );
        let certificate_id = member(case, &topic, &created, "certificateId")
            .as_str()
            .unwrap_or_default()
            .to_string();
        let certificate_pem = member(case, &topic, &created, "certificatePem")
            .as_str()
            .unwrap_or_default()
            .to_string();
        let private_key = member(case, &topic, &created, "privateKey")
            .as_str()
            .unwrap_or_default()
            .to_string();
        let token = member(case, &topic, &created, "certificateOwnershipToken")
            .as_str()
            .unwrap_or_default()
            .to_string();
        assert!(
            certificate_id.len() == 64
                && certificate_id.chars().all(|c| c.is_ascii_hexdigit()),
            "case {case:?}: expected a certificate id of 64 hex characters, got \
             {certificate_id:?}"
        );
        assert!(
            certificate_pem.contains("-----BEGIN CERTIFICATE-----"),
            "case {case:?}: expected a PEM certificate, got {certificate_pem:?}"
        );
        assert!(
            private_key.contains("PRIVATE KEY-----"),
            "case {case:?}: expected the device's private key, got {private_key:?}"
        );
        assert!(
            !token.is_empty(),
            "case {case:?}: expected an ownership token, got {created}"
        );

        // It is not usable yet: a certificate created here is pending until a template
        // activates it.
        let pending = emulator.certificate_status(case, &certificate_id).await;
        assert_eq!(
            pending, PENDING_ACTIVATION,
            "case {case:?}: expected the new certificate {PENDING_ACTIVATION}, got {pending}"
        );

        // The template run: the token proves which certificate the device owns.
        let request = json!({
            "certificateOwnershipToken": token,
            "parameters": row.parameters,
        });
        device
            .publish(&provision, request.to_string().into_bytes())
            .await;
        let (topic, provisioned) = device.read_answer().await;
        assert_eq!(
            topic,
            format!("{provision}/accepted"),
            "case {case:?}: expected the provision accepted, got {topic} with {provisioned}"
        );
        assert_eq!(
            member(case, &topic, &provisioned, "thingName"),
            &json!(row.thing_name),
            "case {case:?}: expected the thing {:?}, got {provisioned}",
            row.thing_name
        );
        assert_eq!(
            member(case, &topic, &provisioned, "deviceConfiguration"),
            &row.device_configuration,
            "case {case:?}: expected the configuration {}, got {provisioned}",
            row.device_configuration
        );

        // What the registries now hold: the thing with the template's attributes, reached
        // through the device's own certificate, and that certificate in the status the
        // template asked for.
        let (status, described) = emulator.describe_thing(row.thing_name).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the provisioned thing to read back, got {status} with \
             {described}"
        );
        assert_eq!(
            described["attributes"], row.attributes,
            "case {case:?}: expected the attributes {}, got {described}",
            row.attributes
        );
        let principals = emulator.thing_principals(case, row.thing_name).await;
        assert!(
            principals
                .iter()
                .any(|principal| principal.ends_with(&format!("cert/{certificate_id}"))),
            "case {case:?}: expected the thing reached through its own certificate \
             {certificate_id}, got {principals:?}"
        );
        let left = emulator.certificate_status(case, &certificate_id).await;
        assert_eq!(
            left, row.certificate_status,
            "case {case:?}: expected the certificate {}, got {left}",
            row.certificate_status
        );
    }
}

/// One request that cannot be carried out.
struct Refused {
    case: &'static str,
    client_id: &'static str,
    /// The topic the request is published on.
    topic: String,
    /// The body it carries.
    payload: Vec<u8>,
    /// The `statusCode` and `errorCode` the answer must carry.
    status_code: i64,
    error_code: &'static str,
}

/// Every part of a request that can be at fault, each answered on its own topic's `rejected`
/// topic: a template the account does not hold, one it holds but has not enabled, a token
/// that proves nothing, a request naming no token, a body that is no request at all, and the
/// signing-request topic asked without a signing request.
fn refusals(token: &str) -> Vec<Refused> {
    // Every provision row supplies the parameter the template names its thing after, so a
    // row that provisioned anything would leave the thing [`REFUSED_THING`] behind.
    let with_token = json!({
        "certificateOwnershipToken": token,
        "parameters": {"SerialNumber": REFUSED_THING},
    })
    .to_string();
    vec![
        Refused {
            case: "a template the account does not hold",
            client_id: "provisioning-refusal-absent",
            topic: provision_topic("fleet-absent"),
            payload: with_token.clone().into_bytes(),
            status_code: 404,
            error_code: "ResourceNotFoundException",
        },
        Refused {
            case: "a template the account holds but has not enabled",
            client_id: "provisioning-refusal-disabled",
            topic: provision_topic("fleet-disabled"),
            payload: with_token.into_bytes(),
            status_code: 403,
            error_code: "AccessDeniedException",
        },
        Refused {
            case: "an ownership token that proves nothing",
            client_id: "provisioning-refusal-token",
            topic: provision_topic("fleet-enabled"),
            payload: json!({
                "certificateOwnershipToken": "not-a-token",
                "parameters": {"SerialNumber": REFUSED_THING},
            })
            .to_string()
            .into_bytes(),
            status_code: 404,
            error_code: "ResourceNotFoundException",
        },
        Refused {
            case: "a provision request naming no token",
            client_id: "provisioning-refusal-no-token",
            topic: provision_topic("fleet-enabled"),
            payload: json!({"parameters": {"SerialNumber": REFUSED_THING}})
                .to_string()
                .into_bytes(),
            status_code: 400,
            error_code: "InvalidRequestException",
        },
        Refused {
            case: "a body that is no request at all",
            client_id: "provisioning-refusal-body",
            topic: provision_topic("fleet-enabled"),
            payload: b"certificateOwnershipToken=t".to_vec(),
            status_code: 400,
            error_code: "InvalidRequestException",
        },
        Refused {
            case: "the signing-request topic asked without a signing request",
            client_id: "provisioning-refusal-no-csr",
            topic: CREATE_FROM_CSR_TOPIC.to_string(),
            payload: b"{}".to_vec(),
            status_code: 400,
            error_code: "InvalidRequestException",
        },
        Refused {
            case: "a signing request this service cannot read",
            client_id: "provisioning-refusal-bad-csr",
            topic: CREATE_FROM_CSR_TOPIC.to_string(),
            payload: json!({"certificateSigningRequest": "not a signing request"})
                .to_string()
                .into_bytes(),
            status_code: 400,
            error_code: "InvalidRequestException",
        },
    ]
}

/// A request the exchange cannot carry out is answered on its own `rejected` topic, carrying
/// the status and the error class a caller of the equivalent API would be refused with — and
/// it provisions nothing.
#[tokio::test]
async fn a_request_that_cannot_be_carried_out_is_refused_on_its_own_rejected_topic() {
    let emulator = emulator();
    let address = emulator.discover().await;

    // The two templates the rows address by state: one a device may provision with, and one
    // the account holds disabled. Both declare the same thing, so a row that provisioned
    // anything would leave that thing behind.
    let body = Template::default().body();
    emulator
        .create_template("the refusal rows", "fleet-enabled", &body, true)
        .await;
    emulator
        .create_template("the refusal rows", "fleet-disabled", &body, false)
        .await;

    // A real ownership token, so a row that names one is refused for the reason it is about
    // and not for the token.
    let mut owner = Device::connect("the refusal rows", &address, "provisioning-token-owner").await;
    owner
        .subscribe(&[
            format!("{CREATE_TOPIC}/accepted"),
            format!("{CREATE_TOPIC}/rejected"),
        ])
        .await;
    owner.publish(CREATE_TOPIC, b"{}".to_vec()).await;
    let (topic, created) = owner.read_answer().await;
    assert_eq!(
        topic,
        format!("{CREATE_TOPIC}/accepted"),
        "the refusal rows: expected a certificate to create, got {topic} with {created}"
    );
    let token = created["certificateOwnershipToken"]
        .as_str()
        .unwrap_or_default()
        .to_string();

    for refused in refusals(&token) {
        let case = refused.case;
        let mut device = Device::connect(case, &address, refused.client_id).await;
        device
            .subscribe(&[
                format!("{}/accepted", refused.topic),
                format!("{}/rejected", refused.topic),
            ])
            .await;
        device
            .publish(&refused.topic, refused.payload.clone())
            .await;
        let (topic, answer) = device.read_answer().await;
        assert_eq!(
            topic,
            format!("{}/rejected", refused.topic),
            "case {case:?}: expected the request refused, got {topic} with {answer}"
        );
        assert_eq!(
            member(case, &topic, &answer, "statusCode"),
            &json!(refused.status_code),
            "case {case:?}: expected statusCode {}, got {answer}",
            refused.status_code
        );
        assert_eq!(
            member(case, &topic, &answer, "errorCode"),
            &json!(refused.error_code),
            "case {case:?}: expected errorCode {:?}, got {answer}",
            refused.error_code
        );
        assert!(
            member(case, &topic, &answer, "errorMessage")
                .as_str()
                .is_some_and(|message| !message.is_empty()),
            "case {case:?}: expected the refusal to say why, got {answer}"
        );
    }

    // Nothing was provisioned by any of them: the thing the refusal rows' template declares
    // is one the registry never took.
    let (status, described) = emulator.describe_thing(REFUSED_THING).await;
    assert_eq!(
        status, 404,
        "expected no thing provisioned by a refused request, got {status} with {described}"
    );
}
