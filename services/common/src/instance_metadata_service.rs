// SPDX-License-Identifier: CC0-1.0

//! The INSTANCE METADATA SERVICE one emulated node publishes to the programs running inside
//! it, and the HTTP client those programs read it with.
//!
//! # Why a node needs one
//!
//! A node's own facts are published TO it, at a link-local address only that node can reach,
//! and a program inside it is how they are read: "this document can be found by running the
//! following command from the instance", followed by a `curl` of that address, is how Amazon
//! ECS's `RegisterContainerInstance` documents obtaining the identity document an agent
//! registers with, and the same address publishes the instance's id, its type, its addresses
//! and the zone it is in. A node with no metadata service has no way to answer "which machine
//! am I", so every program inside it — a container agent, a cloud-init script, an Amazon EMR
//! bootstrap action — is stuck.
//!
//! Two kinds of node in this emulator publish one: an Amazon EC2 instance's guest, and a node
//! of an Amazon EMR cluster. WHAT each publishes is its own service's fact
//! ([`PublishedInstanceMetadata`], implemented by the service that holds the node); the wire —
//! the address, the session token a request presents, the client's options and the statuses it
//! exits with — is the same for both and is therefore stated HERE rather than once per service.
//!
//! # No network, no host
//!
//! The client below is a PROGRAM OF THE NODE, performed by the same bounded, in-process shell
//! session the node's own programs run in ([`crate::posix_shell_session`]): it opens no socket,
//! resolves no name and reads no host path. NOTHING HERE DIALS ANYTHING — the emulator SERVES
//! this address to its own node — which is why the address is held as its four octets
//! ([`METADATA_ADDRESS_OCTETS`]) and the only thing it is ever used for is comparing the HOST
//! of a URL a program named. An invocation naming any other address is one this client performs
//! nothing of, which the session reports as a command it could not run, because such a node has
//! no network to reach one over.

use ::std::fmt::Debug;

use crate::posix_shell_session::{EnvironmentPrograms, ProgramFiles, ProgramOutcome};

/// The link-local address a node's metadata service is published at, as its four octets: an
/// address in the IPv4 link-local space (RFC 3927), which is where every documented metadata
/// read of a node is addressed.
///
/// Held as octets rather than as text because nothing here DIALS it — this emulator serves it
/// to its own node — and because the one use it has is the host comparison in
/// [`metadata_path`].
pub const METADATA_ADDRESS_OCTETS: [u8; 4] = [169, 254, 169, 254];

/// That address as the HOST of a URL, which is the form a program's URL carries it in.
#[must_use]
pub fn metadata_address() -> String {
    ::std::net::Ipv4Addr::from(METADATA_ADDRESS_OCTETS).to_string()
}

/// The program of the node that reads it: the HTTP client every AWS document spells a metadata
/// read with.
pub const METADATA_CLIENT: &str = "curl";

/// The path a session token is asked for, which is the one PUT the metadata service takes
/// (IMDSv2).
pub const METADATA_TOKEN_PATH: &str = "/latest/api/token";

/// The header a PUT asking for a session token states its lifetime in, which that request has
/// to carry.
pub const METADATA_TOKEN_TTL_HEADER: &str = "x-aws-ec2-metadata-token-ttl-seconds";

/// The header a request presents a session token in.
pub const METADATA_TOKEN_HEADER: &str = "x-aws-ec2-metadata-token";

/// The prefix of every metadata path that publishes a node FACT.
pub const METADATA_PREFIX: &str = "/latest/meta-data";

/// The status the client exits with when it could not connect to the address at all
/// (`CURLE_COULDNT_CONNECT`), which is what a node whose metadata endpoint is turned off
/// answers: there is nothing listening for it.
pub const COULD_NOT_CONNECT: i32 = 7;

/// The status it exits with for a response it reports as an error
/// (`CURLE_HTTP_RETURNED_ERROR`), which the client answers only when it was asked to fail on
/// one (`-f`): a path the metadata service publishes nothing at, and a request it will not
/// answer without a session token. Asked for nothing, the client reports a status of success
/// and writes no body, since the body of such a response is the service's own error page,
/// which this metadata service publishes none of.
pub const HTTP_RETURNED_ERROR: i32 = 22;

/// The status it exits with when it could not write the file it was told to write
/// (`CURLE_WRITE_ERROR`).
pub const WRITE_ERROR: i32 = 23;

/// How many bytes one session token is minted out of.
const TOKEN_BYTES: usize = 32;

/// What ONE node's metadata service publishes, which is the service holding that node's own
/// fact.
///
/// The implementation answers out of the STORED node and nothing else: no value is echoed from
/// a request, and a path whose value the node has none of is answered by nothing, which the
/// client reports as the metadata service's own `404` rather than as an invented value.
pub trait PublishedInstanceMetadata: Debug + Send + Sync {
    /// The value this node's metadata service publishes at `path`, as the lines it answers
    /// with, and `None` for a path it publishes nothing at.
    fn published(&self, path: &str) -> Option<Vec<String>>;

    /// Whether anything answers at the address at all: "indicates whether the HTTP metadata
    /// endpoint on your instances is enabled or disabled"
    /// (`InstanceMetadataOptionsResponse$HttpEndpoint`). A node whose launch turned it off
    /// answers nothing, which is [`COULD_NOT_CONNECT`].
    fn endpoint_enabled(&self) -> bool {
        true
    }

    /// Whether a read has to present the session token: "if the state is `required`, you must
    /// send a session token with any instance metadata retrieval requests"
    /// (`InstanceMetadataOptionsResponse$HttpTokens`).
    fn token_required(&self) -> bool {
        true
    }
}

/// The metadata CLIENT of one node's session: the program [`METADATA_CLIENT`] names, answering
/// out of what `published` publishes.
///
/// Holds the node's own facts and the token of the session they are read in, and nothing
/// else — no store guard, no engine and no host handle is reachable from a program.
#[derive(Debug)]
pub struct MetadataClient {
    /// What this node's metadata service publishes.
    published: Box<dyn PublishedInstanceMetadata>,
    /// The session token this run of the node's metadata service issues, minted once for the
    /// session so a program that asks for one and then presents it is answered, and a program
    /// presenting anything else is not. `None` when this process could not mint one, in which
    /// case the service issues no token and answers no request that needs one.
    token: Option<String>,
}

/// The metadata client of a node publishing `published`, for a session that runs a program of
/// that node.
#[must_use]
pub fn metadata_client(published: Box<dyn PublishedInstanceMetadata>) -> Box<dyn EnvironmentPrograms>
{
    Box::new(MetadataClient {
        published,
        token: minted_token(),
    })
}

/// One session token: random bytes from the OS CSPRNG, base64 as the metadata service publishes
/// a token. Nothing derives it from the node — a token is a credential of the session, not a
/// fact of the node — and `None` is a token that was not minted at all rather than a
/// predictable one.
fn minted_token() -> Option<String> {
    let mut bytes = [0_u8; TOKEN_BYTES];
    ::aws_lc_rs::rand::fill(&mut bytes).ok()?;
    Some(::aws_smithy_types::base64::encode(bytes))
}

impl EnvironmentPrograms for MetadataClient {
    fn run(
        &self,
        name: &str,
        arguments: &[String],
        files: &mut dyn ProgramFiles,
    ) -> Option<ProgramOutcome> {
        match name {
            METADATA_CLIENT => self.fetch(arguments, files),
            // Every other program is one this node does not have installed.
            _ => None,
        }
    }
}

impl MetadataClient {
    /// Perform one invocation of the client: `None` for an invocation naming anything but this
    /// node's metadata service, or written in a form the client does not perform.
    fn fetch(&self, arguments: &[String], files: &mut dyn ProgramFiles) -> Option<ProgramOutcome> {
        let invocation = Invocation::read(arguments)?;
        let path = metadata_path(&invocation.url)?;
        if !self.published.endpoint_enabled() {
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
            // `-o PATH` writes the answer to the node's own storage instead of its output.
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
    /// answers with an error: a path it publishes nothing at, a method it does not take, or a
    /// request without the session token this node requires.
    fn answer(&self, invocation: &Invocation, path: &str) -> Option<Vec<String>> {
        if invocation.method == "PUT" {
            if path != METADATA_TOKEN_PATH {
                return None;
            }
            // A token request states how long the token is to be good for, and a PUT that does
            // not is refused.
            invocation.header(METADATA_TOKEN_TTL_HEADER)?;
            return Some(vec![self.token.clone()?]);
        }
        if invocation.method != "GET" || path == METADATA_TOKEN_PATH {
            return None;
        }
        if self.published.token_required()
            && invocation.header(METADATA_TOKEN_HEADER) != self.token
        {
            return None;
        }
        self.published.published(path)
    }
}

/// The path `url` addresses on a node's metadata service, or `None` when it addresses something
/// else: another host, or a form this client does not read.
///
/// The address is read by the `http` crate's own URI type — the maintained parser this
/// workspace already carries for a request line — rather than by anything written here; a query
/// string is therefore not part of the path, and a trailing separator is trimmed off because
/// the documented read of the identity document is written with one.
#[must_use]
pub fn metadata_path(url: &str) -> Option<String> {
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

/// One invocation of a node's metadata client, as its arguments state it.
#[derive(Debug, Default)]
pub struct Invocation {
    /// The method the request is made with, `GET` where the invocation names none.
    pub method: String,
    /// The address it is made to.
    pub url: String,
    /// The headers it carries, by lowercased name.
    pub headers: Vec<(String, String)>,
    /// The path the answer is written to instead of the program's output, for an invocation
    /// that asked for one.
    pub output: Option<String>,
    /// Whether the invocation asked the client to REPORT an HTTP error as its own failure
    /// (`-f`/`--fail`), which is the only thing that makes one a non-zero exit status.
    pub fail: bool,
}

/// The options that carry a VALUE and are read: the method, a header, and where the answer is
/// written.
const VALUED_OPTIONS: &[&str] = &["-X", "--request", "-H", "--header", "-o", "--output"];

/// The options that carry a value this client has nothing to do with — how long it would wait,
/// how often it would try again, what it would call itself — each of which is read and ignored,
/// together with its value.
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

/// The flags that carry no value and make the client REPORT an HTTP error as a failure of its
/// own, rather than writing the response and exiting with success.
const FAIL_FLAGS: &[&str] = &["-f", "--fail", "--fail-with-body"];

/// The flags that carry no value and change nothing about what is answered here: how much the
/// client would say about itself, whether it would follow a redirection, and whether it would
/// check a certificate.
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
    /// Fails CLOSED, like every other command a node's session cannot perform: a request
    /// written in a form this does not read runs nothing rather than being answered as though
    /// it had been read.
    #[must_use]
    pub fn read(arguments: &[String]) -> Option<Self> {
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
            // A long option's value joined to it by `=`, which is the other spelling every one
            // of these options is written in.
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
    #[must_use]
    pub fn header(&self, name: &str) -> Option<String> {
        self.headers
            .iter()
            .find(|(carried, _)| carried == name)
            .map(|(_, value)| value.clone())
    }
}

/// One `Name: value` header as the pair it states, or `None` for a word that is not one.
///
/// The field line is split at its colon, as HTTP defines one, and both halves are then read by
/// the `http` crate's own header types, so a name or a value that is not a header's is refused
/// by that maintained parser rather than by anything written here. The name is answered
/// lowercased, which is how that type holds one.
fn header(written: &str) -> Option<(String, String)> {
    let (name, value) = written.split_once(':')?;
    let name = ::http::HeaderName::try_from(name.trim()).ok()?;
    let value = ::http::HeaderValue::try_from(value.trim()).ok()?;
    Some((name.as_str().to_string(), value.to_str().ok()?.to_string()))
}
