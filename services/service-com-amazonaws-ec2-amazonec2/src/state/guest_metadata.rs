// SPDX-License-Identifier: CC0-1.0

//! The INSTANCE METADATA SERVICE of one instance's guest, and the programs of that guest
//! which read it.
//!
//! # Why a guest needs one
//!
//! An instance's own facts are published TO its guest, at a link-local address only that
//! guest can reach, and a program inside the guest is how they are read: "this document can
//! be found by running the following command from the instance", followed by a `curl` of
//! that address, is how Amazon ECS's `RegisterContainerInstance` documents obtaining the
//! identity document an agent registers with, and the same address publishes the instance's
//! id, its image, its type, its addresses and the zone it is in. A guest with no metadata
//! service has no way to answer "which machine am I", so every agent inside it — the ECS
//! container agent, cloud-init's own scripts, anything a Run Command sends — is stuck.
//!
//! # What is real here
//!
//! Every value is read off the STORED instance ([`GuestMachine`], the one reading of what a
//! guest knows about its machine): the id, the image, the type, the architecture, the
//! addresses, the zone and the Region of that zone, the instant it was launched, and the
//! metadata options its launch chose. Nothing is echoed from a request, and nothing is
//! answered for a fact the record does not carry — a path whose value the instance has none
//! of is a `404` rather than an invented value.
//!
//! The identity document is SIGNED: [`identity_document_signature`] is the RSASSA-PKCS1-v1_5
//! signature of the document's own bytes under a 2048-bit key this process generates once
//! (the shared signer in `service_common::jws_signing`, which is also what this workspace's
//! emulated OIDC issuers sign with), base64 as the metadata service publishes it. So the
//! signature really is a signature OF that document — two instances' documents carry
//! different ones, and a document that changes changes it — rather than a constant of the
//! right shape.
//!
//! # No network, no host
//!
//! The client below is a PROGRAM OF THE GUEST, performed by the same bounded, in-process
//! shell session the guest's own boot script runs in
//! ([`super::guest_console`]): it opens no socket, resolves no name and reads no host path.
//! NOTHING HERE DIALS ANYTHING — the emulator SERVES this address to its own emulated guest,
//! which is why the address is held as its four octets ([`METADATA_ADDRESS_OCTETS`]) and the
//! only thing it is ever used for is comparing the HOST of a URL a guest program named. An
//! invocation naming any other address is one this client performs nothing of, which the
//! session reports as a command it could not run, because this guest has no network to reach
//! one over.

use ::std::sync::OnceLock;

use ::service_common::jws_signing::{SigningKey, RS256};
use ::service_common::posix_shell_session::{EnvironmentPrograms, ProgramFiles, ProgramOutcome};

use super::guest_console::{guest_mac_address, GuestMachine};
use super::{iso8601_utc, region_of_zone_name};

/// The link-local address an instance's metadata service is published at, as its four
/// octets: an address in the IPv4 link-local space (RFC 3927), which is where every
/// documented metadata read of a guest is addressed.
///
/// Held as octets rather than as text because nothing here DIALS it — this emulator serves
/// it to its own guest — and because the one use it has is the host comparison in
/// [`metadata_path`].
pub const METADATA_ADDRESS_OCTETS: [u8; 4] = [169, 254, 169, 254];

/// That address as the HOST of a URL, which is the form a guest program's URL carries it in.
#[must_use]
pub fn metadata_address() -> String {
    ::std::net::Ipv4Addr::from(METADATA_ADDRESS_OCTETS).to_string()
}

/// The program of the guest that reads it: the HTTP client every AWS document spells a
/// metadata read with.
pub const METADATA_CLIENT: &str = "curl";

/// The path the signed identity document of the instance is published at
/// (`RegisterContainerInstanceRequest$instanceIdentityDocument`).
pub const IDENTITY_DOCUMENT_PATH: &str = "/latest/dynamic/instance-identity/document";

/// The path its signature is published at
/// (`RegisterContainerInstanceRequest$instanceIdentityDocumentSignature`).
pub const IDENTITY_SIGNATURE_PATH: &str = "/latest/dynamic/instance-identity/signature";

/// The path a session token is asked for, which is the one PUT the metadata service takes
/// (IMDSv2).
pub const METADATA_TOKEN_PATH: &str = "/latest/api/token";

/// The header a PUT asking for a session token states its lifetime in, which that request
/// has to carry.
pub const METADATA_TOKEN_TTL_HEADER: &str = "x-aws-ec2-metadata-token-ttl-seconds";

/// The header a request presents a session token in.
pub const METADATA_TOKEN_HEADER: &str = "x-aws-ec2-metadata-token";

/// The prefix of every metadata path that publishes an instance FACT.
const METADATA_PREFIX: &str = "/latest/meta-data";

/// The prefix of the paths that publish this instance's identity.
const IDENTITY_PREFIX: &str = "/latest/dynamic/instance-identity";

/// `version` of the identity document this metadata service publishes: the schema version
/// AWS's own documents carry.
pub const IDENTITY_DOCUMENT_VERSION: &str = "2017-09-30";

/// The status the client exits with when it could not connect to the address at all
/// (`CURLE_COULDNT_CONNECT`), which is what a guest whose metadata endpoint is turned off
/// answers: there is nothing listening for it.
const COULD_NOT_CONNECT: i32 = 7;

/// The status it exits with for a response it reports as an error
/// (`CURLE_HTTP_RETURNED_ERROR`), which the client answers only when it was asked to fail on
/// one (`-f`): a path the metadata service publishes nothing at, and a request it will not
/// answer without a session token. Asked for nothing, the client reports a status of success
/// and writes no body, since the body of such a response is the service's own error page,
/// which this metadata service publishes none of.
const HTTP_RETURNED_ERROR: i32 = 22;

/// The status it exits with when it could not write the file it was told to write
/// (`CURLE_WRITE_ERROR`).
const WRITE_ERROR: i32 = 23;

/// How many bytes one session token is minted out of.
const TOKEN_BYTES: usize = 32;

/// The programs installed in one instance's GUEST, beyond the shell's own utilities: the
/// client that reads this machine's metadata service.
///
/// Holds the machine's own facts and the token of the session they are read in, and nothing
/// else — no store guard, no engine and no host handle is reachable from a program.
#[derive(Debug)]
pub struct GuestPrograms {
    /// The machine whose metadata these programs publish.
    machine: GuestMachine,
    /// The session token this run of the guest's metadata service issues, minted once for
    /// the session so a program that asks for one and then presents it is answered, and a
    /// program presenting anything else is not. `None` when this process could not mint one,
    /// in which case the service issues no token and answers no request that needs one.
    token: Option<String>,
}

/// The programs the guest of `machine` has installed in it, for a session that runs a
/// program of that guest.
///
/// The ONE construction of them, so the boot's own user-data program and a command something
/// later sends the guest read the same machine through the same client.
#[must_use]
pub fn guest_programs(machine: &GuestMachine) -> Box<dyn EnvironmentPrograms> {
    Box::new(GuestPrograms {
        machine: machine.clone(),
        token: minted_token(),
    })
}

/// One session token: random bytes from the OS CSPRNG, base64 as the metadata service
/// publishes a token. Nothing derives it from the machine — a token is a credential of the
/// session, not a fact of the instance — and `None` is a token that was not minted at all
/// rather than a predictable one.
fn minted_token() -> Option<String> {
    let mut bytes = [0_u8; TOKEN_BYTES];
    ::aws_lc_rs::rand::fill(&mut bytes).ok()?;
    Some(::aws_smithy_types::base64::encode(bytes))
}

impl EnvironmentPrograms for GuestPrograms {
    fn run(
        &self,
        name: &str,
        arguments: &[String],
        files: &mut dyn ProgramFiles,
    ) -> Option<ProgramOutcome> {
        match name {
            METADATA_CLIENT => self.fetch(arguments, files),
            // Every other program is one this guest does not have installed.
            _ => None,
        }
    }
}

impl GuestPrograms {
    /// Perform one invocation of the client: `None` for an invocation naming anything but
    /// this machine's metadata service, or written in a form the client does not perform.
    fn fetch(&self, arguments: &[String], files: &mut dyn ProgramFiles) -> Option<ProgramOutcome> {
        let invocation = Invocation::read(arguments)?;
        let path = metadata_path(&invocation.url)?;
        // "Indicates whether the HTTP metadata endpoint on your instances is enabled or
        // disabled" (`InstanceMetadataOptionsResponse$HttpEndpoint`): nothing answers at the
        // address on an instance whose launch turned it off.
        if !self.machine.metadata_endpoint_enabled {
            return Some(ProgramOutcome {
                status: COULD_NOT_CONNECT,
                output: Vec::new(),
            });
        }
        let Some(answered) = self.answer(&invocation, &path) else {
            // `curl` reports an HTTP error as a failure only when it was asked to
            // (`-f`/`--fail`); otherwise the request is over with a status of success and
            // nothing written, since the body of such a response is an error page this
            // metadata service publishes none of.
            return Some(ProgramOutcome {
                status: if invocation.fail { HTTP_RETURNED_ERROR } else { 0 },
                output: Vec::new(),
            });
        };
        match invocation.output.as_deref() {
            // `-o PATH` writes the answer to the guest's own storage instead of its output.
            Some(written) => {
                let body: String = answered.iter().map(|line| format!("{line}\n")).collect();
                let status = if files.write(written, &body) {
                    0
                } else {
                    WRITE_ERROR
                };
                Some(ProgramOutcome {
                    status,
                    output: Vec::new(),
                })
            }
            None => Some(ProgramOutcome {
                status: 0,
                output: answered,
            }),
        }
    }

    /// What the metadata service answers `invocation` for `path`, or `None` for a request it
    /// answers with an error: a path it publishes nothing at, a method it does not take, or
    /// a request without the session token this instance requires.
    fn answer(&self, invocation: &Invocation, path: &str) -> Option<Vec<String>> {
        if invocation.method == "PUT" {
            if path != METADATA_TOKEN_PATH {
                return None;
            }
            // A token request states how long the token is to be good for, and a PUT that
            // does not is refused.
            invocation.header(METADATA_TOKEN_TTL_HEADER)?;
            return Some(vec![self.token.clone()?]);
        }
        if invocation.method != "GET" || path == METADATA_TOKEN_PATH {
            return None;
        }
        // "If the state is `required`, you must send a session token with any instance
        // metadata retrieval requests" (`InstanceMetadataOptionsResponse$HttpTokens`).
        if self.machine.metadata_token_required
            && invocation.header(METADATA_TOKEN_HEADER) != self.token
        {
            return None;
        }
        self.published(path)
    }

    /// The value this machine's metadata service publishes at `path`, as the lines it
    /// answers with, and `None` for a path it publishes nothing at.
    ///
    /// The paths are the instance-metadata categories AWS publishes (Amazon EC2 User Guide,
    /// "Instance metadata categories"), each answered off the stored instance; a category
    /// whose value this instance has none of is published by neither the listing nor the
    /// path itself.
    fn published(&self, path: &str) -> Option<Vec<String>> {
        let machine = &self.machine;
        let one = |value: String| Some(vec![value]);
        match path {
            METADATA_PREFIX => Some(
                METADATA_CATEGORIES
                    .iter()
                    .filter(|category| {
                        self.published(&format!("{METADATA_PREFIX}/{category}"))
                            .is_some()
                    })
                    .map(|category| (*category).to_string())
                    .collect(),
            ),
            IDENTITY_PREFIX => Some(
                [IDENTITY_DOCUMENT_PATH, IDENTITY_SIGNATURE_PATH]
                    .into_iter()
                    .filter(|published| self.published(published).is_some())
                    .filter_map(|published| published.rsplit('/').next())
                    .map(str::to_string)
                    .collect(),
            ),
            "/latest/meta-data/ami-id" => machine.image_id.clone().map(|id| vec![id]),
            "/latest/meta-data/instance-id" => one(machine.instance_id.clone()),
            "/latest/meta-data/instance-type" => one(machine.instance_type.clone()),
            "/latest/meta-data/local-ipv4" => one(machine.private_ip.clone()),
            "/latest/meta-data/local-hostname" | "/latest/meta-data/hostname" => {
                one(machine.private_dns_name.clone())
            }
            "/latest/meta-data/public-ipv4" => machine.public_ip.clone().map(|ip| vec![ip]),
            "/latest/meta-data/mac" => one(guest_mac_address(machine)),
            "/latest/meta-data/placement/availability-zone" => {
                one(machine.availability_zone.clone())
            }
            "/latest/meta-data/placement/region" => {
                one(region_of_zone_name(&machine.availability_zone).to_string())
            }
            IDENTITY_DOCUMENT_PATH => one(identity_document(machine)),
            // The published signature is base64 text of the document's signature, which is
            // the form the operations that take one accept.
            IDENTITY_SIGNATURE_PATH => identity_document_signature(&identity_document(machine))
                .map(|signature| vec![signature]),
            _ => None,
        }
    }
}

/// The metadata categories the listing of [`METADATA_PREFIX`] names, in the order the
/// service lists them (alphabetically, as a directory listing is). A category this instance
/// has no value for is left out of the listing by the reader above.
const METADATA_CATEGORIES: &[&str] = &[
    "ami-id",
    "hostname",
    "instance-id",
    "instance-type",
    "local-hostname",
    "local-ipv4",
    "mac",
    "placement/availability-zone",
    "placement/region",
    "public-ipv4",
];

/// The path `url` addresses on THIS machine's metadata service, or `None` when it addresses
/// something else: another host, or a form this client does not read.
///
/// The address is read by the `http` crate's own URI type — the maintained parser this
/// workspace already carries for a request line — rather than by anything written here; a
/// query string is therefore not part of the path, and a trailing separator is trimmed off
/// because the documented read of the identity document is written with one.
fn metadata_path(url: &str) -> Option<String> {
    let parsed = ::http::Uri::try_from(url).ok()?;
    if parsed.host() != Some(metadata_address().as_str()) {
        return None;
    }
    let trimmed = parsed.path().trim_end_matches('/');
    Some(if trimmed.is_empty() {
        "/".to_string()
    } else {
        trimmed.to_string()
    })
}

/// The identity document of `machine`: the JSON document the metadata service publishes for
/// the instance, with every member read off the stored instance.
///
/// The members are the ones an AWS identity document carries (Amazon EC2 User Guide,
/// "Instance identity documents"); a member the instance has no value for is `null`, which
/// is what AWS writes for one too.
#[must_use]
pub fn identity_document(machine: &GuestMachine) -> String {
    ::serde_json::json!({
        "accountId": ::engine::emulator_account_id(),
        "architecture": machine.architecture,
        "availabilityZone": machine.availability_zone,
        "billingProducts": ::serde_json::Value::Null,
        "devpayProductCodes": ::serde_json::Value::Null,
        "imageId": machine.image_id,
        "instanceId": machine.instance_id,
        "instanceType": machine.instance_type,
        "kernelId": machine.kernel_id,
        "marketplaceProductCodes": ::serde_json::Value::Null,
        "pendingTime": iso8601_utc(machine.launch_time),
        "privateIp": machine.private_ip,
        "ramdiskId": machine.ramdisk_id,
        "region": region_of_zone_name(&machine.availability_zone),
        "version": IDENTITY_DOCUMENT_VERSION,
    })
    .to_string()
}

/// The signature the metadata service publishes for `document`: the RSASSA-PKCS1-v1_5
/// signature of its bytes under [`signing_key`], base64 — the form the published signature
/// takes, which is why the operations that accept one accept base64 text of a signature's
/// length.
///
/// `None` when this process could not produce a key or could not sign, in which case the
/// metadata service publishes no signature at all rather than bytes that are not one.
#[must_use]
pub fn identity_document_signature(document: &str) -> Option<String> {
    let signed = signing_key()?.sign(document.as_bytes())?;
    Some(::aws_smithy_types::base64::encode(&signed))
}

/// The key the identity documents of this emulator's instances are signed under, generated
/// ONCE for this process.
///
/// Its private half never leaves this module: the only thing anything can ask of it is a
/// signature ([`SigningKey::sign`]).
fn signing_key() -> Option<&'static SigningKey> {
    static KEY: OnceLock<Option<SigningKey>> = OnceLock::new();
    KEY.get_or_init(|| SigningKey::generate(RS256)).as_ref()
}

/// One invocation of the guest's metadata client, as its arguments state it.
#[derive(Debug, Default)]
struct Invocation {
    /// The method the request is made with, `GET` where the invocation names none.
    method: String,
    /// The address it is made to.
    url: String,
    /// The headers it carries, by lowercased name.
    headers: Vec<(String, String)>,
    /// The path the answer is written to instead of the program's output, for an invocation
    /// that asked for one.
    output: Option<String>,
    /// Whether the invocation asked the client to REPORT an HTTP error as its own failure
    /// (`-f`/`--fail`), which is the only thing that makes one a non-zero exit status.
    fail: bool,
}

/// The options that carry a VALUE and are read: the method, a header, and where the answer
/// is written.
const VALUED_OPTIONS: &[&str] = &["-X", "--request", "-H", "--header", "-o", "--output"];

/// The options that carry a value this client has nothing to do with — how long it would
/// wait, how often it would try again, what it would call itself — each of which is read
/// and ignored, together with its value.
const IGNORED_VALUED_OPTIONS: &[&str] = &[
    "-m",
    "--max-time",
    "--connect-timeout",
    "--retry",
    "--retry-delay",
    "--retry-max-time",
    "-A",
    "--user-agent",
    "--max-filesize",
];

/// The flags that carry no value and make the client REPORT an HTTP error as a failure of
/// its own, rather than writing the response and exiting with success.
const FAIL_FLAGS: &[&str] = &["-f", "--fail", "--fail-with-body"];

/// The flags that carry no value and change nothing about what is answered here: how much
/// the client would say about itself, whether it would follow a redirection, and whether it
/// would check a certificate.
const IGNORED_FLAGS: &[&str] = &[
    "-s",
    "--silent",
    "-S",
    "--show-error",
    "-L",
    "--location",
    "-k",
    "--insecure",
    "-g",
    "--globoff",
    "-q",
];

/// Whether `flag` is a valueless flag this client reads: one it acts on, or one it ignores.
fn is_flag(flag: &str) -> bool {
    FAIL_FLAGS.contains(&flag) || IGNORED_FLAGS.contains(&flag)
}

impl Invocation {
    /// `arguments` as one invocation, or `None` for a form this client does not perform: an
    /// option it does not read, or more than one address.
    ///
    /// An option's value is read in both spellings the client takes it in: as the next word
    /// (`-H NAME: v`) and joined to a long option (`--header=NAME: v`).
    ///
    /// Fails CLOSED, like every other command a guest's session cannot perform: a request
    /// written in a form this does not read runs nothing rather than being answered as
    /// though it had been read.
    fn read(arguments: &[String]) -> Option<Self> {
        let mut invocation = Self {
            method: String::new(),
            ..Self::default()
        };
        let mut arguments = arguments.iter();
        while let Some(argument) = arguments.next() {
            if argument == "-" || !argument.starts_with('-') {
                if !invocation.url.is_empty() {
                    return None;
                }
                invocation.url = argument.clone();
                continue;
            }
            if FAIL_FLAGS.contains(&argument.as_str()) {
                invocation.fail = true;
                continue;
            }
            if IGNORED_FLAGS.contains(&argument.as_str()) {
                continue;
            }
            // A long option's value joined to it by `=`, which is the other spelling every
            // one of these options is written in.
            let joined = argument
                .starts_with("--")
                .then(|| argument.split_once('='))
                .flatten();
            let (option, value) = match joined {
                Some((option, value)) => (option.to_string(), Some(value.to_string())),
                None => (argument.clone(), None),
            };
            if IGNORED_VALUED_OPTIONS.contains(&option.as_str()) {
                if value.is_none() {
                    arguments.next()?;
                }
                continue;
            }
            if VALUED_OPTIONS.contains(&option.as_str()) {
                let value = match value {
                    Some(value) => value,
                    None => arguments.next()?.clone(),
                };
                match option.as_str() {
                    "-X" | "--request" => invocation.method = value.to_ascii_uppercase(),
                    "-H" | "--header" => invocation.headers.push(header(&value)?),
                    _ => invocation.output = Some(value),
                }
                continue;
            }
            // Several valueless flags written as one word, as `-fsS` is.
            if !argument.starts_with("--")
                && argument.len() > 2
                && argument
                    .chars()
                    .skip(1)
                    .all(|flag| is_flag(&format!("-{flag}")))
            {
                invocation.fail |= argument
                    .chars()
                    .skip(1)
                    .any(|flag| FAIL_FLAGS.contains(&format!("-{flag}").as_str()));
                continue;
            }
            return None;
        }
        if invocation.url.is_empty() {
            return None;
        }
        if invocation.method.is_empty() {
            invocation.method = "GET".to_string();
        }
        Some(invocation)
    }

    /// The value this invocation carries for the header `name` names, and `None` when it
    /// carries no such header.
    fn header(&self, name: &str) -> Option<String> {
        self.headers
            .iter()
            .find(|(carried, _)| carried == name)
            .map(|(_, value)| value.clone())
    }
}

/// One `Name: value` header as the pair it states, or `None` for a word that is not one.
///
/// The field line is split at its colon, as HTTP defines one, and both halves are then read
/// by the `http` crate's own header types, so a name or a value that is not a header's is
/// refused by that maintained parser rather than by anything written here. The name is
/// answered lowercased, which is how that type holds one.
fn header(written: &str) -> Option<(String, String)> {
    let (name, value) = written.split_once(':')?;
    let name = ::http::HeaderName::try_from(name.trim()).ok()?;
    let value = ::http::HeaderValue::try_from(value.trim()).ok()?;
    Some((name.as_str().to_string(), value.to_str().ok()?.to_string()))
}
