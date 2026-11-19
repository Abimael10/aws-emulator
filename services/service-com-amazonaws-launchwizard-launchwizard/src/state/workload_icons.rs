// SPDX-License-Identifier: CC0-1.0

//! The icon the workload catalogue publishes for one entry: where it is addressed, and
//! the image served at that address.
//!
//! `WorkloadData$iconUrl` is "the URL of a workload icon" — an absolute URL the client
//! that read the catalogue entry fetches NEXT. An address a reporting operation hands
//! out is only usable if it leads back to the endpoint the client is already talking to:
//! so the icon of a catalogue entry is addressed on this emulator's own base URL, at a
//! path built from the entry being reported ([`icon_path`]), SigV4 query-signed through
//! the emulator's own signer ([`icon_url`]) — and this emulator serves the image there
//! (`crate::service::workload_icon`, over [`workload_icon`]).
//!
//! The catalogue therefore stores no absolute URL for an icon at all: the host is
//! whatever this emulator is reached on at the moment of the read, and the per-entry part
//! is the entry's own `workloadName`, so an entry added to [`super::seeded_workloads`]
//! cannot be seeded with the icon of another and no entry can address a host this
//! emulator does not answer on.
//!
//! An address is PRE-SIGNED, and it also carries an AUTHENTICATOR this service's store
//! minted for exactly that entry ([`ComAmazonawsLaunchwizardLaunchwizardState::icon_token`],
//! over the key the store drew from the system's randomness). A pre-signed fetch carries no
//! caller identity of its own, so the authenticator is what stands in for the read that
//! handed the address out: the bytes served are selected by a value only this store could
//! have produced, never by path text alone, and an address assembled by a caller — or
//! minted by another store, or for another entry — resolves to nothing.

use std::time::Duration;

use ::smithy_server_runtime::{http, SignedUrlRequest, UrlSigner};

/// The signing name this service's requests are signed under, which an address on this
/// emulator's base URL is signed with so the router resolves it back to this service
/// (the model's `sigv4Name`/`endpointPrefix`, `smithy_query(query="service_info")`).
pub const SIGNING_NAME: &str = "launchwizard";

/// The media type the icon is served as, and the format the shared renderer writes: a PNG,
/// which is what the `.png` address names
/// (`service_common::record_identicon::IDENTICON_MEDIA_TYPE`).
pub const ICON_MEDIA_TYPE: &str = ::service_common::record_identicon::IDENTICON_MEDIA_TYPE;

/// The first path segment of every icon address: the catalogue collection the entry whose
/// icon it is belongs to.
const ICON_PATH_ROOT: &str = "workloads";

/// The query field an icon address carries its AUTHENTICATOR in (see the module docs).
pub const ICON_TOKEN_FIELD: &str = "iconToken";

/// The last path segment of every icon address. The entry's own name is the segment
/// between the two, so one entry's icon cannot be addressed as another's.
const ICON_FILE_NAME: &str = "icon.png";

/// How long the signed address of an icon stays valid.
///
/// The service documents no validity for a workload icon, so this one is the emulator's
/// CHOICE: long enough for a client that read a catalogue page to fetch every icon on it
/// (and to re-render the page it cached), short enough that an address copied out of one
/// response does not stay fetchable indefinitely.
const ICON_URL_VALIDITY: Duration = Duration::from_secs(60 * 60);

/// The path this emulator serves the icon of the catalogue entry `workload_name` at,
/// decoded, as the signer takes it.
///
/// The ONE statement of that layout: [`icon_url`] signs this path and
/// [`workload_in_icon_path`] reads it back, so an address handed out and an address served
/// cannot drift apart. The name is placed in the path as written, which is a round trip
/// because a catalogue key is a `WorkloadName` ([`is_workload_name`]) and so is one path
/// segment carrying no character a URL encodes.
pub fn icon_path(workload_name: &str) -> String {
    format!("/{ICON_PATH_ROOT}/{workload_name}/{ICON_FILE_NAME}")
}

/// The `workloadName` whose icon `segments` addresses — the decoded path segments of a
/// request, in order — or `None` when the path is not an icon address at all.
///
/// The name segment has to have the shape of a `WorkloadName` ([`is_workload_name`]): it is
/// the only form [`icon_path`] can have written there, so anything else is a path this
/// service never handed out, refused before any store is reached.
pub fn workload_in_icon_path(segments: &[String]) -> Option<&str> {
    match segments {
        [root, workload_name, file]
            if root == ICON_PATH_ROOT
                && file == ICON_FILE_NAME
                && is_workload_name(workload_name) =>
        {
            Some(workload_name)
        }
        _ => None,
    }
}

/// Whether `label` has the shape of a `WorkloadName`: len 1..100, `^[A-Za-z][a-zA-Z0-9-_]*$`
/// (`smithy_query(query="shape", shape="WorkloadName")`).
///
/// Every character that admits is one a URL path carries as written, so a name of this shape
/// is one path segment that survives being signed into an address and read back out of the
/// request that fetches it.
fn is_workload_name(label: &str) -> bool {
    (1..=100).contains(&label.len())
        && label.starts_with(|c: char| c.is_ascii_alphabetic())
        && label
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// `WorkloadData$iconUrl` for the catalogue entry `workload_name`: the signed address of
/// [`icon_path`] on this emulator's base URL.
///
/// Signed per read rather than stored, because it is a TIME-LIMITED address
/// ([`ICON_URL_VALIDITY`]) of the emulator that is serving it: the host is the one this
/// emulator is reached on when the read happens, which the store cannot know, and the
/// window has to start at the read rather than at whenever the catalogue was seeded. The
/// icon the address leads to is the entry's own either way ([`workload_icon`]).
///
/// `icon_token` is the authenticator this store minted for this entry
/// ([`ComAmazonawsLaunchwizardLaunchwizardState::icon_token`]), which the address carries so
/// the fetch of it is served from the entry the read was for.
///
/// `None` when the address cannot be signed, which leaves the optional member absent
/// rather than reporting a URL that leads nowhere.
pub fn icon_url(signer: &UrlSigner, workload_name: &str, icon_token: &str) -> Option<String> {
    let path = icon_path(workload_name);
    let request = SignedUrlRequest::new(http::Method::GET, SIGNING_NAME, &path)
        .query(ICON_TOKEN_FIELD, icon_token);
    signer
        .sign(request, ICON_URL_VALIDITY)
        .ok()
        .map(|url| url.as_str().to_string())
}

impl super::ComAmazonawsLaunchwizardLaunchwizardState {
    /// The authenticator the address of the catalogue entry `workload_name` carries: this
    /// store's own tag over WHICH entry the address names, rendered as the hex the address
    /// spells it in.
    ///
    /// Computed under the secret this store drew from the system's randomness when it was
    /// created and never reveals
    /// (`service_common::page_tokens::PageTokenKey`, the one place this workspace
    /// authenticates a value a store hands out and takes back). So an address is a value
    /// only this store could have produced.
    pub fn icon_token(&self, workload_name: &str) -> String {
        ::hex::encode(self.icon_address_key.mint(workload_name))
    }
}

/// The icon image the address of the catalogue entry `workload_name` leads to, or `None`
/// when this emulator serves no image at that address, which is what the serving route
/// answers "nothing here" for.
///
/// `token` is the authenticator the address carries, checked against the one this store
/// minted for exactly this entry BEFORE any catalogue is read: a pre-signed fetch carries no
/// caller identity of its own, so the authenticator is what stands in for the read that
/// handed the address out. An address this store did not mint, one minted for another entry,
/// and a name the catalogue does not hold are all answered the same way, so a refusal tells
/// a caller nothing about which it was.
///
/// Read through the catalogue the reporting operations read, so an address is served an
/// image exactly while `GetWorkload` reports the entry it belongs to, and the image is
/// rendered from the STORED entry's name rather than from the requested text.
///
/// The image itself is the one every service that publishes a picture of a record it holds no
/// uploaded bytes for serves (`service_common::record_identicon`), derived from the entry's
/// own name: the catalogue needs no image file beside it, two entries cannot share an icon,
/// and nothing about the image depends on the request or the clock.
pub fn workload_icon<Eng: Send + Sync + 'static>(
    engine: &Eng,
    workload_name: &str,
    token: &str,
) -> Option<Vec<u8>> {
    let state = super::service_state(engine)?;
    let store = state.lock();
    let minted = store
        .icon_address_key
        .cursor_of(&::hex::decode(token).ok()?)?;
    if minted != workload_name {
        return None;
    }
    let workload = store.resolve_workload(workload_name).ok()?;
    ::service_common::record_identicon::identicon_png(&workload.workload_name)
}
