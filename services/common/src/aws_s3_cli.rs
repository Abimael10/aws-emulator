// SPDX-License-Identifier: CC0-1.0

//! The AWS CLI's `aws s3` TRANSFERS, as a program a caller-supplied command line can ask
//! for: what one command line asks of the CLI, and the calls that carry the file it names.
//!
//! # The rule this module implements
//!
//! Several emulated services run a command line a client supplied — a container's `command`,
//! a cluster step's arguments, a build's commands — and such a line reaches Amazon S3 through
//! the AWS CLI: "`aws s3 cp` copies a local file or S3 object to another location locally or
//! in S3", `mv` "moves" one and `rm` "deletes an S3 object" (the `aws s3` command reference),
//! while the `aws s3api` group "gives access to the Amazon S3 API" one operation at a time.
//! So a command line that carries a file between the machine and a bucket has that file
//! carried, and the status the command exits with is the status of the call: "0 -- Command was
//! successful", "1 -- Limited errors … at least one or more s3 transfers failed" and "254 --
//! … the service returned an error" (AWS CLI return codes).
//!
//! That is ONE rule, so it lives here once rather than per service: two services running the
//! same command line must not carry a different file or answer a different status. What each
//! service keeps is its own — the SESSION the line runs in, the FILES a local path names, and
//! WHICH credential the calls are signed with.
//!
//! This module holds the two halves nothing about a caller makes different:
//!
//! * what one command line ASKS for ([`requested`] for the file-oriented commands,
//!   [`requested_api`] for the low-level ones) — the command group, the command, and the ends
//!   of the call, each read as the CLI reads it. Every other command group, every other
//!   command of these groups, and every FORM of these commands this program does not perform
//!   (a parameter they do not take, a prefix, the wrong number of operands) is answered
//!   `None`, which the caller's session reports as a command it could not run rather than as a
//!   call;
//! * the CALLS that carry it ([`get_object_text`], [`put_object_text`], [`copy_object`],
//!   [`delete_object`], and the low-level [`put_object_api`], [`get_object_api`],
//!   [`head_object_api`]), made with the in-process Amazon S3 client the caller hands over
//!   ([`crate::s3_client`]) — so the object a command wrote is the object a later `GetObject`
//!   of that bucket reads — and the CLI's own account of a call the store refused
//!   ([`store_answered`]).
//!
//! Nothing here opens a socket, resolves a name, starts a process or reads a file of the
//! host: the client is the caller's in-process hop and a local path is a key of the caller's
//! own tree.
//!
//! The copy that predates this module — `service-com-amazonaws-emr-elasticmapreduce`'s
//! `state::step_command_transfers`, which performs the same three commands for a cluster
//! step's command line — is to be MIGRATED onto it, in the change that owns that service.
//! Nothing new should restate either half.
//!
//! # Bounds
//!
//! How many bytes one transfer carries is the CALLER's bound, passed to each call: the bytes
//! are a client's object or a client's file, so what one transfer takes into this process is
//! bounded by the session that asked for it rather than by the object's own size. A download
//! is read RANGED, so nothing larger is ever buffered, and a transfer carries TEXT only,
//! because the trees these sessions hold hold text.

use ::aws_smithy_types::error::metadata::ProvideErrorMetadata;

/// The name a command line invokes the AWS CLI by.
pub const CLI_PROGRAM: &str = "aws";

/// The CLI's command GROUP this program answers for: `aws s3`, "high-level S3 commands".
pub const S3_COMMAND_GROUP: &str = "s3";

/// The commands of that group this program performs, as the CLI spells them: `cp` ("copies a
/// local file or S3 object to another location locally or in S3"), `mv` ("moves a local file
/// or S3 object to another location locally or in S3") and `rm` ("deletes an S3 object").
/// SOURCE: the `aws s3` command reference. Every other command of the group — and every
/// other group — is one this program performs nothing of.
pub const PERFORMED_S3_COMMANDS: [&str; 3] = ["cp", "mv", "rm"];

/// How a command line addresses an object in Amazon S3: `s3://<bucket>/<key>`, the form the
/// CLI's S3 commands take.
const S3_SCHEME_PREFIX: &str = "s3://";

/// The operand the CLI's `cp` takes for the command's own standard input or standard output:
/// "Uploading a local file stream to Amazon S3 … `aws s3 cp - s3://bucket/stream.txt`" and
/// "Downloading an Amazon S3 object as a local file stream … `aws s3 cp s3://bucket/stream.txt
/// -`" (the `aws s3 cp` command reference). It is an OPERAND rather than an option, so a
/// command line carrying it is one this program performs; `mv` and `rm` take no such operand.
pub const STREAM_OPERAND: &str = "-";

/// The status a command answers when its transfer was made: "0 -- Command was successful.
/// There were no errors thrown by either the CLI or by the service the request was made to."
/// (AWS CLI return codes.)
pub const TRANSFER_SUCCEEDED: i32 = 0;

/// The status a command answers when its transfer was not made: "1 -- Limited errors … For
/// `s3` commands, this means at least one or more s3 transfers failed." (AWS CLI return
/// codes.)
pub const TRANSFER_FAILED: i32 = 1;

/// What the CLI reports for a command it has no credential to sign with: "Unable to locate
/// credentials" (AWS CLI). It is the detail of the transfer's own failure line, so a command
/// run where no credential is held exits [`TRANSFER_FAILED`] rather than making a call it has
/// none for.
pub const NO_CREDENTIAL_LOCATED: &str = "Unable to locate credentials";

/// ONE object of the store, as a command line addressed it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjectAddress {
    /// The bucket the address names.
    pub bucket: String,
    /// The key under it.
    pub key: String,
}

/// One end of a transfer: a path of the session's own tree, an object of the store, or the
/// command's own standard input or standard output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Location {
    /// A path the command line named, which is a key of the session's own tree.
    Local(String),
    /// An `s3://<bucket>/<key>` address.
    Object(ObjectAddress),
    /// The CLI's [`STREAM_OPERAND`]: what the stage before this command wrote, when it is the
    /// SOURCE, and what this command writes, when it is the destination.
    Stream,
}

impl Location {
    /// `written` as the CLI reads it: `-` is the command's own stream, an `s3://` address is
    /// an object and everything else is a local path. `None` for an `s3://` address that names
    /// no ONE object.
    #[must_use]
    pub fn of(written: &str) -> Option<Self> {
        if written == STREAM_OPERAND {
            return Some(Self::Stream);
        }
        if written.starts_with(S3_SCHEME_PREFIX) {
            return Some(Self::Object(addressed_object(written)?));
        }
        (!written.is_empty()).then(|| Self::Local(written.to_string()))
    }

    /// This end as the command line wrote it, which is how the CLI names it in its own report.
    #[must_use]
    pub fn shown(&self) -> String {
        match self {
            Self::Local(path) => path.clone(),
            Self::Object(object) => object_shown(object),
            Self::Stream => STREAM_OPERAND.to_string(),
        }
    }
}

/// The object `written` addresses, read through this workspace's ONE Amazon S3 URI parse
/// ([`crate::s3_uri::object_address`]).
///
/// `None` when `written` is not the address of one object: a path that is not an S3 address at
/// all, an address with no bucket or no key, or a key naming a PREFIX rather than an object —
/// which these commands take only with an option, and an option is a form this program
/// performs nothing of.
fn addressed_object(written: &str) -> Option<ObjectAddress> {
    if !written.starts_with(S3_SCHEME_PREFIX) {
        return None;
    }
    let (bucket, key) = crate::s3_uri::object_address(written)?;
    (!key.ends_with('/')).then_some(ObjectAddress { bucket, key })
}

/// `object` as a command line writes it.
#[must_use]
pub fn object_shown(object: &ObjectAddress) -> String {
    format!("{S3_SCHEME_PREFIX}{}/{}", object.bucket, object.key)
}

/// One NAMED parameter of a command line, as the CLI reads both forms it takes: `--name value`
/// and `--name=value` ("you can specify the parameter value with a space or an equals sign", the
/// AWS CLI's command-structure reference). `None` for a word that names no parameter at all, and
/// for a `--name` with no value after it.
///
/// It is HERE rather than per command group so one emulated CLI reads a parameter's value one
/// way whichever group the command belongs to ([`requested_api`] and
/// [`crate::aws_sns_cli::requested_publish`] both read it through this).
pub fn parameter_of<'a, I: Iterator<Item = &'a str>>(
    word: &'a str,
    rest: &mut I,
) -> Option<(&'a str, String)> {
    let parameter = word.strip_prefix("--")?;
    match parameter.split_once('=') {
        Some((parameter, value)) => Some((parameter, value.to_string())),
        // `--name value`: the value is the next word.
        None => Some((parameter, rest.next()?.to_string())),
    }
}

/// What one `aws s3` command line asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Requested {
    /// `cp` and `mv`: the file at one end, carried to the other — and, for `mv`, taken away
    /// from the first.
    Carry {
        /// The end the file is carried FROM.
        from: Location,
        /// The end it is carried TO.
        to: Location,
        /// Set for `mv`, which "moves" rather than copies.
        taking_source_away: bool,
    },
    /// `rm`: the object deleted.
    Remove {
        /// The object the command names.
        target: ObjectAddress,
    },
}

impl Requested {
    /// The word the CLI reports this transfer under: what it did with the file — `move` for a
    /// `mv`, and otherwise the direction the `cp` carried it in.
    #[must_use]
    pub fn reported_word(&self) -> &'static str {
        match self {
            Self::Remove { .. } => "delete",
            Self::Carry {
                taking_source_away: true,
                ..
            } => "move",
            Self::Carry { from, to, .. } => match (from, to) {
                (Location::Object(_), Location::Object(_)) => "copy",
                (Location::Object(_), _) => "download",
                _ => "upload",
            },
        }
    }

    /// The two ends as the command line wrote them, which is how the CLI names the transfer in
    /// its own report: `<from> to <to>` for a carry, and the object alone for a removal.
    #[must_use]
    pub fn shown(&self) -> String {
        match self {
            Self::Remove { target } => object_shown(target),
            Self::Carry { from, to, .. } => format!("{} to {}", from.shown(), to.shown()),
        }
    }
}

/// What the command line `arguments` — the words AFTER the program name — asks of the CLI, or
/// `None` for every invocation this program performs nothing of.
///
/// It performs `aws s3 cp|mv|rm` over the operands those commands take, and nothing else: an
/// invocation naming another command group or another command of this one, carrying an OPTION
/// (every option of these commands — a recursive transfer, another Region, a filter — changes
/// what the command does, so a line carrying one is not this invocation), carrying the wrong
/// number of operands, naming no object at all, or asking for a transfer with no end in the
/// object store is one this program answers nothing for.
///
/// [`STREAM_OPERAND`] is an operand rather than an option, and `cp` is the one command that
/// takes it: a `mv` or an `rm` written with it is an invocation of theirs that does not exist.
#[must_use]
pub fn requested(arguments: &[String]) -> Option<Requested> {
    let mut words = arguments.iter();
    if words.next()?.as_str() != S3_COMMAND_GROUP {
        return None;
    }
    let command = words.next()?.as_str();
    if !PERFORMED_S3_COMMANDS.contains(&command) {
        return None;
    }
    let operands: Vec<&String> = words.collect();
    if operands
        .iter()
        .any(|operand| operand.starts_with('-') && operand.as_str() != STREAM_OPERAND)
    {
        return None;
    }
    if command == "rm" {
        let [target] = operands[..] else {
            return None;
        };
        return Some(Requested::Remove {
            target: addressed_object(target)?,
        });
    }
    // `cp` and `mv` take the two ends, at least one of which is in the object store: a
    // transfer between two local paths reaches no store, so it is not a transfer this program
    // performs.
    let [from, to] = operands[..] else {
        return None;
    };
    let from = Location::of(from)?;
    let to = Location::of(to)?;
    let reaches_the_store =
        matches!(from, Location::Object(_)) || matches!(to, Location::Object(_));
    if !reaches_the_store {
        return None;
    }
    let taking_source_away = command == "mv";
    if taking_source_away && (matches!(from, Location::Stream) || matches!(to, Location::Stream))
    {
        return None;
    }
    Some(Requested::Carry {
        from,
        to,
        taking_source_away,
    })
}

/// Whether the invocation `arguments` READS the command's own standard input: an upload whose
/// source is [`STREAM_OPERAND`], which is the one form of these commands that does.
///
/// A session asks this before it TAKES its standard input, so a command that does not read one
/// leaves the stream for whoever does.
#[must_use]
pub fn reads_standard_input(arguments: &[String]) -> bool {
    matches!(
        requested(arguments),
        Some(Requested::Carry {
            from: Location::Stream,
            ..
        })
    )
}

/// What a call the object store refused reports, as the CLI words a service's refusal: "An
/// error occurred (<code>) when calling the <operation> operation: <message>".
pub fn store_answered<E: ProvideErrorMetadata>(operation: &str, error: &E) -> String {
    format!(
        "An error occurred ({}) when calling the {operation} operation: {}",
        error.code().unwrap_or("Unknown"),
        error.message().unwrap_or_default()
    )
}

/// The CLI's LOW-LEVEL command group for the object store: `aws s3api`, which "gives access to
/// the Amazon S3 API" one operation at a time, as against the file-oriented commands of
/// [`S3_COMMAND_GROUP`].
pub const S3API_COMMAND_GROUP: &str = "s3api";

/// The commands of that group this program performs, as the CLI spells them — the object
/// operations a command line carries out on its own: `put-object`, `get-object`, `head-object`
/// and `delete-object`. SOURCE: the `aws s3api` command reference. Every other command of the
/// group is one this program performs nothing of.
pub const PERFORMED_S3API_COMMANDS: [&str; 4] =
    ["put-object", "get-object", "head-object", "delete-object"];

/// The status an `s3api` command answers when the service refused its request: "254 -- The
/// command was successfully parsed and a request was made to the specified service but the
/// service returned an error" (AWS CLI return codes). It is the status a shell reads, so a
/// program written `if aws s3api put-object …; then … else … fi` takes its other branch.
pub const API_SERVICE_ERROR: i32 = 254;

/// The status an `s3api` command answers when the CLI itself could not carry it out — no
/// credential to sign with, a local file it was pointed at that is not there: "255 -- General
/// error" (AWS CLI return codes).
pub const API_COMMAND_ERROR: i32 = 255;

/// What ONE `aws s3api` command line asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApiOperation {
    /// `put-object --bucket B --key K [--body FILE] [--content-type T]`: the object written,
    /// with the local file whose bytes it carries — a `put-object` naming no body writes an
    /// empty object, which is what that invocation does.
    PutObject {
        /// The object the command writes.
        target: ObjectAddress,
        /// The local path whose bytes it writes, and `None` for an invocation naming none.
        body: Option<String>,
        /// `--content-type`, the media type the object is stored under.
        content_type: Option<String>,
    },
    /// `get-object --bucket B --key K OUTFILE`: the object read, and the local path its bytes
    /// are written to — an operand of the command rather than an option.
    GetObject {
        /// The object the command reads.
        source: ObjectAddress,
        /// The local path its bytes are written to.
        outfile: String,
    },
    /// `head-object --bucket B --key K`: the object's own metadata, with no bytes read.
    HeadObject {
        /// The object the command asks about.
        target: ObjectAddress,
    },
    /// `delete-object --bucket B --key K`: the object deleted.
    DeleteObject {
        /// The object the command deletes.
        target: ObjectAddress,
    },
}

impl ApiOperation {
    /// The API operation this command calls, as the CLI names it in its account of a refusal
    /// ([`store_answered`]).
    #[must_use]
    pub fn api_name(&self) -> &'static str {
        match self {
            Self::PutObject { .. } => "PutObject",
            Self::GetObject { .. } => "GetObject",
            Self::HeadObject { .. } => "HeadObject",
            Self::DeleteObject { .. } => "DeleteObject",
        }
    }
}

/// One `aws s3api` invocation: what it asks for, and the Region it asks it in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiCall {
    /// The operation the command line names.
    pub operation: ApiOperation,
    /// `--region`, the Region the call is addressed to — "the region to use", which overrides
    /// the one the environment holds — and `None` for a command line naming none, which is
    /// addressed to the Region its environment is in.
    pub region: Option<String>,
}

/// What the command line `arguments` — the words AFTER the program name — asks of the CLI's
/// low-level object commands, or `None` for every invocation this program performs nothing of.
///
/// It performs the four commands of [`PERFORMED_S3API_COMMANDS`] over the parameters those
/// commands take: `--bucket` and `--key`, which address the object, `--body` and
/// `--content-type` on a write, the one OPERAND `get-object` takes (the file its bytes are
/// written to), and `--region` — each in both forms the CLI takes a parameter's value in
/// ([`parameter_of`]). Every other command of the group, every other parameter (each
/// changes what the command does, so a line carrying one is not this invocation), a line
/// missing the parameters its command needs, and one carrying an operand its command does not
/// take, is answered nothing for.
#[must_use]
pub fn requested_api(arguments: &[String]) -> Option<ApiCall> {
    let mut words = arguments.iter().map(String::as_str);
    if words.next()? != S3API_COMMAND_GROUP {
        return None;
    }
    let command = words.next()?;
    if !PERFORMED_S3API_COMMANDS.contains(&command) {
        return None;
    }
    let (mut bucket, mut key, mut body, mut content_type, mut region) = (None, None, None, None, None);
    let mut operands: Vec<String> = Vec::new();
    while let Some(word) = words.next() {
        if !word.starts_with("--") {
            operands.push(word.to_string());
            continue;
        }
        // Both forms the CLI takes a parameter's value in, read the one way this workspace
        // reads them ([`parameter_of`]).
        let (parameter, value) = parameter_of(word, &mut words)?;
        let held = match parameter {
            "bucket" => &mut bucket,
            "key" => &mut key,
            "body" => &mut body,
            "content-type" => &mut content_type,
            "region" => &mut region,
            // Every other parameter changes what the command does, so a line carrying one is
            // not an invocation this program performs.
            _ => return None,
        };
        // A parameter given twice is not an invocation either.
        if held.is_some() {
            return None;
        }
        *held = Some(value);
    }
    let target = ObjectAddress {
        bucket: bucket.filter(|bucket| !bucket.is_empty())?,
        key: key.filter(|key| !key.is_empty())?,
    };
    let operation = match command {
        "put-object" => {
            if !operands.is_empty() {
                return None;
            }
            ApiOperation::PutObject {
                target,
                body,
                content_type,
            }
        }
        "get-object" => {
            if body.is_some() || content_type.is_some() {
                return None;
            }
            let [outfile] = &operands[..] else {
                return None;
            };
            ApiOperation::GetObject {
                source: target,
                outfile: outfile.clone(),
            }
        }
        _ => {
            if !operands.is_empty() || body.is_some() || content_type.is_some() {
                return None;
            }
            if command == "head-object" {
                ApiOperation::HeadObject { target }
            } else {
                ApiOperation::DeleteObject { target }
            }
        }
    };
    Some(ApiCall { operation, region })
}

/// Write `contents` as the object at `address` through the low-level command, and answer what
/// the CLI prints for it: the response document, as the members the service answered.
pub async fn put_object_api(
    client: &::aws_sdk_s3::Client,
    address: &ObjectAddress,
    contents: &str,
    content_type: Option<&str>,
    max_bytes: usize,
) -> Result<String, String> {
    if contents.len() > max_bytes {
        return Err(format!(
            "The file is past the {max_bytes} bytes one transfer carries."
        ));
    }
    let mut request = client
        .put_object()
        .bucket(&address.bucket)
        .key(&address.key)
        .body(::aws_sdk_s3::primitives::ByteStream::from(
            contents.as_bytes().to_vec(),
        ));
    if let Some(content_type) = content_type {
        request = request.content_type(content_type);
    }
    let written = request
        .send()
        .await
        .map_err(|error| store_answered("PutObject", &error))?;
    Ok(printed_document(&[("ETag", written.e_tag().map(ToString::to_string))]))
}

/// Read the object at `address` through the low-level command: its bytes, and what the CLI
/// prints for the call — the metadata members the service answered.
pub async fn get_object_api(
    client: &::aws_sdk_s3::Client,
    address: &ObjectAddress,
    max_bytes: usize,
) -> Result<(String, String), String> {
    let read = client
        .get_object()
        .bucket(&address.bucket)
        .key(&address.key)
        // RANGED, so an object past the bound is never buffered whole.
        .range(format!("bytes=0-{max_bytes}"))
        .send()
        .await
        .map_err(|error| store_answered("GetObject", &error))?;
    let document = printed_document(&[
        ("ContentLength", read.content_length().map(|length| length.to_string())),
        ("ContentType", read.content_type().map(ToString::to_string)),
        ("ETag", read.e_tag().map(ToString::to_string)),
    ]);
    let bytes = read
        .body
        .collect()
        .await
        .map_err(|error| format!("The object did not arrive in full: {error}"))?
        .into_bytes();
    if bytes.len() > max_bytes {
        return Err(format!(
            "The object is past the {max_bytes} bytes one transfer carries."
        ));
    }
    let text = String::from_utf8(bytes.to_vec())
        .map_err(|_| "The object is not text, so no file was written.".to_string())?;
    Ok((text, document))
}

/// Ask about the object at `address` without reading it, and answer what the CLI prints: the
/// metadata members the service answered.
pub async fn head_object_api(
    client: &::aws_sdk_s3::Client,
    address: &ObjectAddress,
) -> Result<String, String> {
    let read = client
        .head_object()
        .bucket(&address.bucket)
        .key(&address.key)
        .send()
        .await
        .map_err(|error| store_answered("HeadObject", &error))?;
    Ok(printed_document(&[
        ("ContentLength", read.content_length().map(|length| length.to_string())),
        ("ContentType", read.content_type().map(ToString::to_string)),
        ("ETag", read.e_tag().map(ToString::to_string)),
    ]))
}

/// What the CLI prints for one call: the response document, carrying the members the service
/// ANSWERED and no others — a member it answered nothing for is absent, never filled in — as
/// the pretty-printed JSON the CLI's default output format is.
///
/// The format is the CLI's rather than any one service's, so the sibling command groups print
/// what their calls answered through this one spelling of it ([`crate::aws_sns_cli`]).
#[must_use]
pub fn printed_document(members: &[(&str, Option<String>)]) -> String {
    let answered: ::serde_json::Map<String, ::serde_json::Value> = members
        .iter()
        .filter_map(|(name, value)| {
            value
                .clone()
                .map(|value| ((*name).to_string(), ::serde_json::Value::String(value)))
        })
        .collect();
    if answered.is_empty() {
        return String::new();
    }
    ::serde_json::to_string_pretty(&::serde_json::Value::Object(answered)).unwrap_or_default()
}

/// What the CLI prints for one call in its `text` format: the values the service ANSWERED, in
/// the order the response states them, separated by tabs — "the output is formatted as multiple
/// lines of tab-separated string values" (AWS CLI, controlling command output) — where a member
/// it answered nothing for is absent rather than filled in.
///
/// Beside [`printed_document`] for the same reason: the format is the CLI's rather than any one
/// service's, so every command group that prints an answer prints it through these two.
#[must_use]
pub fn printed_values(members: &[(&str, Option<String>)]) -> String {
    members
        .iter()
        .filter_map(|(_, value)| value.clone())
        .collect::<Vec<String>>()
        .join("\t")
}

/// The object at `address`, as the text a transfer carries — read whole and bounded by
/// `max_bytes`, which is the caller's bound on what one transfer takes into this process.
///
/// `Err` carries what the CLI would report about the transfer: the store's own refusal, an
/// object past that bound, or one that is not text.
pub async fn get_object_text(
    client: &::aws_sdk_s3::Client,
    address: &ObjectAddress,
    max_bytes: usize,
) -> Result<String, String> {
    let read = client
        .get_object()
        .bucket(&address.bucket)
        .key(&address.key)
        // RANGED, so an object past the bound is never buffered whole: the one byte past it is
        // what shows it is.
        .range(format!("bytes=0-{max_bytes}"))
        .send()
        .await
        .map_err(|error| store_answered("GetObject", &error))?;
    let bytes = read
        .body
        .collect()
        .await
        .map_err(|error| format!("The object did not arrive in full: {error}"))?
        .into_bytes();
    if bytes.len() > max_bytes {
        return Err(format!(
            "The object is past the {max_bytes} bytes one transfer carries."
        ));
    }
    String::from_utf8(bytes.to_vec())
        .map_err(|_| "The object is not text, so no file was written.".to_string())
}

/// Write `contents` as the object at `address`, up to the caller's `max_bytes`.
pub async fn put_object_text(
    client: &::aws_sdk_s3::Client,
    address: &ObjectAddress,
    contents: &str,
    max_bytes: usize,
) -> Result<(), String> {
    if contents.len() > max_bytes {
        return Err(format!(
            "The file is past the {max_bytes} bytes one transfer carries."
        ));
    }
    client
        .put_object()
        .bucket(&address.bucket)
        .key(&address.key)
        .body(::aws_sdk_s3::primitives::ByteStream::from(
            contents.as_bytes().to_vec(),
        ))
        .send()
        .await
        .map(|_written| ())
        .map_err(|error| store_answered("PutObject", &error))
}

/// Copy the object at `source` to `destination`, in the store itself — the one transfer whose
/// bytes never enter this process.
pub async fn copy_object(
    client: &::aws_sdk_s3::Client,
    source: &ObjectAddress,
    destination: &ObjectAddress,
) -> Result<(), String> {
    client
        .copy_object()
        .bucket(&destination.bucket)
        .key(&destination.key)
        .copy_source(format!("{}/{}", source.bucket, source.key))
        .send()
        .await
        .map(|_copied| ())
        .map_err(|error| store_answered("CopyObject", &error))
}

/// Delete the object at `address`.
pub async fn delete_object(
    client: &::aws_sdk_s3::Client,
    address: &ObjectAddress,
) -> Result<(), String> {
    client
        .delete_object()
        .bucket(&address.bucket)
        .key(&address.key)
        .send()
        .await
        .map(|_deleted| ())
        .map_err(|error| store_answered("DeleteObject", &error))
}

#[cfg(test)]
mod tests {
    use super::{
        object_shown, requested, requested_api, ApiCall, ApiOperation, Location, ObjectAddress,
        Requested,
    };

    /// The words of a command line after the program name, as a session hands them to a
    /// program.
    fn words(line: &str) -> Vec<String> {
        line.split_whitespace().map(ToString::to_string).collect()
    }

    /// RULE: `aws s3 cp|mv|rm` over the operands those commands take is what this program
    /// performs, and every other invocation is one it answers nothing for — so the session
    /// reports it as a command it could not run rather than as a transfer.
    #[test]
    fn the_invocations_this_program_performs() {
        let object = |bucket: &str, key: &str| ObjectAddress {
            bucket: bucket.to_string(),
            key: key.to_string(),
        };
        // (case, the arguments after `aws`, what it asks for)
        let rows: [(&str, &str, Option<Requested>); 17] = [
            (
                "an upload",
                "s3 cp /tmp/out.txt s3://reports/output/out.txt",
                Some(Requested::Carry {
                    from: Location::Local("/tmp/out.txt".to_string()),
                    to: Location::Object(object("reports", "output/out.txt")),
                    taking_source_away: false,
                }),
            ),
            (
                "a download",
                "s3 cp s3://reports/input/in.txt /tmp/in.txt",
                Some(Requested::Carry {
                    from: Location::Object(object("reports", "input/in.txt")),
                    to: Location::Local("/tmp/in.txt".to_string()),
                    taking_source_away: false,
                }),
            ),
            (
                "a copy between two objects",
                "s3 cp s3://reports/a s3://archive/b",
                Some(Requested::Carry {
                    from: Location::Object(object("reports", "a")),
                    to: Location::Object(object("archive", "b")),
                    taking_source_away: false,
                }),
            ),
            (
                "a move",
                "s3 mv /tmp/out.txt s3://reports/out.txt",
                Some(Requested::Carry {
                    from: Location::Local("/tmp/out.txt".to_string()),
                    to: Location::Object(object("reports", "out.txt")),
                    taking_source_away: true,
                }),
            ),
            (
                "a removal",
                "s3 rm s3://reports/out.txt",
                Some(Requested::Remove {
                    target: object("reports", "out.txt"),
                }),
            ),
            ("another command of the group", "s3 sync /tmp s3://reports", None),
            ("another command group", "ec2 describe-instances", None),
            (
                "an option, which changes what the command does",
                "s3 cp --recursive /tmp s3://reports/tree",
                None,
            ),
            ("a transfer between two local paths", "s3 cp /tmp/one /tmp/two", None),
            ("an address naming no object", "s3 cp /tmp/one s3://reports", None),
            ("an address naming a prefix", "s3 cp /tmp/one s3://reports/tree/", None),
            ("too few operands", "s3 cp /tmp/one", None),
            (
                "a Hadoop filesystem scheme, which these commands do not take",
                "s3 cp s3a://reports/a /tmp/one",
                None,
            ),
            (
                "an upload from the command's own standard input",
                "s3 cp - s3://reports/stream.txt",
                Some(Requested::Carry {
                    from: Location::Stream,
                    to: Location::Object(object("reports", "stream.txt")),
                    taking_source_away: false,
                }),
            ),
            (
                "a download to the command's own standard output",
                "s3 cp s3://reports/stream.txt -",
                Some(Requested::Carry {
                    from: Location::Object(object("reports", "stream.txt")),
                    to: Location::Stream,
                    taking_source_away: false,
                }),
            ),
            (
                "a transfer between the two streams, which reaches no store",
                "s3 cp - -",
                None,
            ),
            (
                "a move written with the stream operand, which `mv` does not take",
                "s3 mv - s3://reports/stream.txt",
                None,
            ),
        ];
        for (case, line, expected) in rows {
            let asked = requested(&words(line));
            assert_eq!(
                asked, expected,
                "case {case:?}: expected {expected:?}, got {asked:?}"
            );
        }
    }

    /// RULE: `aws s3api put-object|get-object|head-object|delete-object` over the parameters
    /// those commands take is what this program performs of the low-level group, and every
    /// other invocation is one it answers nothing for.
    ///
    /// The rows differ along the command, which parameters the line carries (the address
    /// alone, a body, a media type, another Region), the operand `get-object` takes, and the
    /// forms that are not an invocation of these commands at all.
    #[test]
    fn the_low_level_invocations_this_program_performs() {
        let object = |bucket: &str, key: &str| ObjectAddress {
            bucket: bucket.to_string(),
            key: key.to_string(),
        };
        // (case, the arguments after `aws`, what it asks for)
        let rows: [(&str, &str, Option<ApiCall>); 13] = [
            (
                "a write of a local file",
                "s3api put-object --bucket reports --key out.txt --body /tmp/out.txt",
                Some(ApiCall {
                    operation: ApiOperation::PutObject {
                        target: object("reports", "out.txt"),
                        body: Some("/tmp/out.txt".to_string()),
                        content_type: None,
                    },
                    region: None,
                }),
            ),
            (
                "a write naming no body, which writes an empty object",
                "s3api put-object --bucket reports --key marker",
                Some(ApiCall {
                    operation: ApiOperation::PutObject {
                        target: object("reports", "marker"),
                        body: None,
                        content_type: None,
                    },
                    region: None,
                }),
            ),
            (
                "a write stating the object's media type, in another Region",
                "s3api put-object --bucket reports --key out.json --body /tmp/out.json \
                 --content-type application/json --region eu-west-1",
                Some(ApiCall {
                    operation: ApiOperation::PutObject {
                        target: object("reports", "out.json"),
                        body: Some("/tmp/out.json".to_string()),
                        content_type: Some("application/json".to_string()),
                    },
                    region: Some("eu-west-1".to_string()),
                }),
            ),
            (
                "a read into the file the command names",
                "s3api get-object --bucket reports --key in.txt /tmp/in.txt",
                Some(ApiCall {
                    operation: ApiOperation::GetObject {
                        source: object("reports", "in.txt"),
                        outfile: "/tmp/in.txt".to_string(),
                    },
                    region: None,
                }),
            ),
            (
                "a question about the object alone",
                "s3api head-object --bucket reports --key in.txt",
                Some(ApiCall {
                    operation: ApiOperation::HeadObject {
                        target: object("reports", "in.txt"),
                    },
                    region: None,
                }),
            ),
            (
                "a removal",
                "s3api delete-object --bucket reports --key out.txt",
                Some(ApiCall {
                    operation: ApiOperation::DeleteObject {
                        target: object("reports", "out.txt"),
                    },
                    region: None,
                }),
            ),
            (
                "another command of the group",
                "s3api list-objects-v2 --bucket reports",
                None,
            ),
            (
                "a parameter these commands do not take",
                "s3api put-object --bucket reports --key out.txt --acl public-read",
                None,
            ),
            ("a line naming no key", "s3api put-object --bucket reports", None),
            (
                "a parameter with no value after it",
                "s3api head-object --bucket reports --key",
                None,
            ),
            (
                "the values written with equals signs, the CLI's other form",
                "s3api head-object --bucket=reports --key=in.txt",
                Some(ApiCall {
                    operation: ApiOperation::HeadObject {
                        target: object("reports", "in.txt"),
                    },
                    region: None,
                }),
            ),
            (
                "a read naming no file to write",
                "s3api get-object --bucket reports --key in.txt",
                None,
            ),
            (
                "a write carrying an operand it does not take",
                "s3api put-object --bucket reports --key out.txt /tmp/out.txt",
                None,
            ),
        ];
        for (case, line, expected) in rows {
            let asked = requested_api(&words(line));
            assert_eq!(
                asked, expected,
                "case {case:?}: expected {expected:?}, got {asked:?}"
            );
            assert!(
                requested(&words(line)).is_none(),
                "case {case:?}: the file-oriented reading answers nothing for an s3api line"
            );
        }
        let put = requested_api(&words(
            "s3api put-object --bucket reports --key out.txt --body /tmp/out.txt",
        ))
        .expect("the line names a call");
        assert_eq!(
            put.operation.api_name(),
            "PutObject",
            "the operation a refusal is reported under is the API's own name"
        );
    }

    /// RULE: the CLI reports a transfer under what it DID with the file, naming the two ends
    /// as the command line wrote them.
    #[test]
    fn the_word_and_the_ends_a_transfer_is_reported_under() {
        // (case, the arguments after `aws`, the word, the ends)
        let rows: [(&str, &str, &str, &str); 5] = [
            (
                "an upload",
                "s3 cp /tmp/out.txt s3://reports/out.txt",
                "upload",
                "/tmp/out.txt to s3://reports/out.txt",
            ),
            (
                "a download",
                "s3 cp s3://reports/in.txt /tmp/in.txt",
                "download",
                "s3://reports/in.txt to /tmp/in.txt",
            ),
            (
                "a copy",
                "s3 cp s3://reports/a s3://archive/b",
                "copy",
                "s3://reports/a to s3://archive/b",
            ),
            (
                "a move",
                "s3 mv /tmp/out.txt s3://reports/out.txt",
                "move",
                "/tmp/out.txt to s3://reports/out.txt",
            ),
            (
                "a removal",
                "s3 rm s3://reports/out.txt",
                "delete",
                "s3://reports/out.txt",
            ),
        ];
        for (case, line, word, shown) in rows {
            let asked = requested(&words(line)).expect("the line names a transfer");
            assert_eq!(
                asked.reported_word(),
                word,
                "case {case:?}: expected the word {word:?}, got {:?}",
                asked.reported_word()
            );
            assert_eq!(
                asked.shown(),
                shown,
                "case {case:?}: expected the ends {shown:?}, got {:?}",
                asked.shown()
            );
        }
        let address = ObjectAddress {
            bucket: "reports".to_string(),
            key: "a/b.txt".to_string(),
        };
        assert_eq!(
            object_shown(&address),
            "s3://reports/a/b.txt",
            "an address is shown as the command line writes it"
        );
    }
}
