// SPDX-License-Identifier: CC0-1.0

//! The non-volatile UEFI variable store one instance holds — what `GetInstanceUefiData`
//! serves and what a register of an AMI can be given a copy of.
//!
//! "A binary representation of the UEFI variable store. ONLY NON-VOLATILE VARIABLES ARE
//! STORED. This is a base64 encoded and zlib compressed binary value"
//! (`GetInstanceUefiData`), so a store travels as the base64 of the zlib stream of its own
//! binary form, and only the variables that survive a power cycle are in it.
//!
//! A store reaches an instance one of two ways, and both are facts of state rather than of
//! the call:
//!
//! * the AMI the instance was launched from was REGISTERED with one — "when you use
//!   register-image to create an AMI, you can create an exact copy of your variable store by
//!   passing the UEFI data in the UefiData parameter" (`GetInstanceUefiData`) — so the
//!   instance comes up with that exact copy
//!   ([`ComAmazonawsEc2Amazonec2State::image_uefi_data`], the same bytes
//!   `DescribeImageAttribute` reports as the image's `uefiData`); or
//! * its own FIRMWARE wrote one as the machine came up, which is the store of an instance
//!   whose AMI was registered without one: the globally defined non-volatile variables of
//!   the UEFI specification's section 3.3 — the platform's language, its boot timeout, the
//!   boot option for the loader the machine starts and the order those options are tried in,
//!   and the secure-boot operating mode — derived from the instance's own architecture and
//!   AMI ([`uefi_firmware_variables`]).
//!
//! An instance that boots the LEGACY BIOS runs no UEFI firmware, so it holds no non-volatile
//! UEFI variables at all and there is no store to report rather than an empty one; and an
//! instance whose guest has not come up yet has none either, because the firmware writes its
//! variables as the machine boots. That is why a client polls for the store of an instance it
//! has just launched.
//!
//! ## The binary form
//!
//! The ENVELOPE is the one a client reads: the store opens with the magic
//! [`UEFI_STORE_MAGIC`] every store AWS hands out opens with, the zlib stream of the variable
//! records follows it ("this is a base64 encoded and ZLIB COMPRESSED binary value",
//! `GetInstanceUefiData`), and the whole of that is base64. What AWS writes INSIDE the zlib
//! stream is not published — the python-uefivars tool its documentation points at is what
//! reads one — so the records are this crate's own stable serialization of a variable store,
//! one per variable in the order the variables are listed:
//!
//! ```text
//! u32    attributes        the EFI_VARIABLE_* bits the variable carries
//! u32    name length       bytes of the name, UCS-2LE and NUL-terminated
//! u32    data length       bytes of the value
//! [16]   vendor GUID       EFI_GUID byte order
//! ..     name              UCS-2LE, NUL-terminated
//! ..     data              the variable's value
//! ```
//!
//! [`decode_uefi_variable_store`] reads it back, so a client — and this crate's own tests —
//! can take a store apart variable by variable instead of trusting the encoder.

use ::base64::Engine as _;

use super::{ComAmazonawsEc2Amazonec2State, InstanceRecord, instance_boots};

/// The eight bytes a UEFI variable store OPENS with, before its compressed records: the magic
/// a client checks to tell a store it can read from any other blob, which every store this
/// service hands out carries.
pub const UEFI_STORE_MAGIC: &[u8; 8] = b"AMZNUEFI";

/// `EFI_VARIABLE_NON_VOLATILE`: the bit that makes a variable one of the "only non-volatile
/// variables [that] are stored" in the store this module serves (UEFI specification, "Variable
/// Attributes").
pub const EFI_VARIABLE_NON_VOLATILE: u32 = 0x0000_0001;

/// `EFI_VARIABLE_BOOTSERVICE_ACCESS`: the variable is readable while boot services are
/// available.
pub const EFI_VARIABLE_BOOTSERVICE_ACCESS: u32 = 0x0000_0002;

/// `EFI_VARIABLE_RUNTIME_ACCESS`: the variable is readable after the operating system has
/// taken over, which is how a guest reads one.
pub const EFI_VARIABLE_RUNTIME_ACCESS: u32 = 0x0000_0004;

/// The attributes every variable of the firmware's own store carries: non-volatile, and
/// readable both before and after the operating system takes over — the attribute set the
/// UEFI specification gives each globally defined variable in [`uefi_firmware_variables`].
pub const UEFI_NON_VOLATILE_ATTRIBUTES: u32 =
    EFI_VARIABLE_NON_VOLATILE | EFI_VARIABLE_BOOTSERVICE_ACCESS | EFI_VARIABLE_RUNTIME_ACCESS;

/// `EFI_GLOBAL_VARIABLE`, the vendor GUID the UEFI specification's globally defined variables
/// live under: `8be4df61-93ca-11d2-aa0d-00e098032b8c`, in the byte order an `EFI_GUID` is
/// written in (the first three fields little-endian, the last eight bytes as they read).
pub const EFI_GLOBAL_VARIABLE_GUID: [u8; 16] = [
    0x61, 0xdf, 0xe4, 0x8b, 0xca, 0x93, 0xd2, 0x11, 0xaa, 0x0d, 0x00, 0xe0, 0x98, 0x03, 0x2b, 0x8c,
];

/// `LOAD_OPTION_ACTIVE`: the boot option is one the firmware attempts (UEFI specification,
/// `EFI_LOAD_OPTION`).
const LOAD_OPTION_ACTIVE: u32 = 0x0000_0001;

/// The device path node type of a MEDIA device path, and the sub-type of a FILE PATH node
/// inside one (UEFI specification, "File Path Media Device Path").
const MEDIA_DEVICE_PATH_TYPE: u8 = 0x04;
const MEDIA_FILE_PATH_SUBTYPE: u8 = 0x04;

/// The device path node that ENDS an entire device path (type `0x7F`, sub-type `0xFF`).
const END_DEVICE_PATH_TYPE: u8 = 0x7f;
const END_ENTIRE_DEVICE_PATH_SUBTYPE: u8 = 0xff;

/// The language the platform reports, as the RFC 4646 code `PlatformLang` carries.
const PLATFORM_LANGUAGE: &str = "en-US";

/// The boot timeout the firmware of an instance holds, in seconds: an instance has no
/// keyboard attached, so no interactive boot menu can be reached and the firmware starts the
/// first boot option it has straight away.
const BOOT_TIMEOUT_SECONDS: u16 = 0;

/// The number of the ONE boot option the firmware writes: `Boot0000`, the loader of the
/// instance's root device.
const FIRST_BOOT_OPTION: u16 = 0;

/// The description of that boot option for an instance whose AMI this store does not hold, so
/// there is no image name to call it by: the UEFI specification's own name for the loader a
/// machine starts when nothing more specific is registered.
const DEFAULT_LOADER_DESCRIPTION: &str = "EFI Default Loader";

/// `SetupMode`'s value when the platform holds NO enrolled platform key, which is the state of
/// a firmware store nothing has enrolled secure-boot keys into (UEFI specification, "Globally
/// Defined Variables": `SetupMode` is 1 while the platform is in setup mode).
const SETUP_MODE_WITHOUT_PLATFORM_KEY: u8 = 1;

/// The most bytes a variable store may hold once its records are decompressed, which is also
/// the most [`decode_uefi_variable_store`] will inflate from one: a ceiling far above any
/// store this service writes or a register of an AMI may supply (`ImageUefiDataRequest` is
/// bounded at 64,000 characters of base64), so a value whose zlib stream expands past it is no
/// store of this service's at all and is refused rather than allocated for.
pub const MAX_UEFI_STORE_BYTES: u64 = 256 * 1024;

/// One variable of a UEFI variable store: the name it is read by, the vendor GUID that
/// namespaces that name, the attribute bits it carries and its value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UefiVariable {
    /// The variable's name, as a guest reads it (`BootOrder`, `PlatformLang`, `Boot0000`).
    pub name: String,
    /// The vendor GUID the name lives under — [`EFI_GLOBAL_VARIABLE_GUID`] for a variable the
    /// UEFI specification defines globally.
    pub vendor_guid: [u8; 16],
    /// The `EFI_VARIABLE_*` bits the variable carries; every variable in a stored
    /// variable store carries [`EFI_VARIABLE_NON_VOLATILE`].
    pub attributes: u32,
    /// The variable's value, as the bytes the firmware holds.
    pub data: Vec<u8>,
}

impl ComAmazonawsEc2Amazonec2State {
    /// The non-volatile UEFI variable store the instance `instance` HOLDS, as
    /// `GetInstanceUefiData` serves it: "base64 representation of the non-volatile UEFI
    /// variable store" (`GetInstanceUefiDataResult$UefiData`), or `None` for an instance that
    /// holds no non-volatile UEFI variables at all.
    ///
    /// See the module docs for where a store comes from. Derived on every call rather than
    /// kept beside the instance, so reading one writes nothing and two reads of an instance
    /// nothing has moved answer exactly the same bytes.
    pub fn instance_uefi_data(&self, instance: &InstanceRecord) -> Option<String> {
        // An instance that boots the legacy BIOS runs no UEFI firmware, so there are no
        // non-volatile UEFI variables to report — the one reading of how an instance boots
        // ([`Self::instance_boots_uefi`]), which is also what `DescribeInstances` reports as
        // that instance's `CurrentInstanceBootMode`.
        if !self.instance_boots_uefi(instance) {
            return None;
        }
        // A store is what the firmware HOLDS, and a machine's firmware comes up with the
        // machine: an instance whose guest has not booted yet holds nothing to read. The same
        // reading of "has this machine come up" the console takes
        // ([`super::instance_console_lines`]), so a client that can read the boot on the
        // console can read the variables that boot wrote.
        if instance_boots(instance).is_empty() {
            return None;
        }
        // "When you use register-image to create an AMI, you can create an EXACT COPY of your
        // variable store by passing the UEFI data in the UefiData parameter"
        // (`GetInstanceUefiData`): an instance launched from such an AMI comes up with that
        // copy, so the bytes reported here are the image's own
        // ([`ComAmazonawsEc2Amazonec2State::image_uefi_data`], which
        // `DescribeImageAttribute` reports as `uefiData`) and the two cannot disagree.
        if let Some(registered) = instance
            .image_id
            .as_deref()
            .and_then(|image_id| self.image_uefi_data.get(image_id))
        {
            return Some(registered.clone());
        }
        Some(encode_uefi_variable_store(
            &self.instance_uefi_variables(instance),
        ))
    }

    /// The variables the instance `instance`'s own FIRMWARE holds, in the order the store
    /// lists them — empty for an instance that boots the legacy BIOS, which has no UEFI
    /// firmware at all.
    ///
    /// The store [`Self::instance_uefi_data`] serves for an instance whose AMI was registered
    /// without a variable store of its own.
    pub fn instance_uefi_variables(&self, instance: &InstanceRecord) -> Vec<UefiVariable> {
        if !self.instance_boots_uefi(instance) {
            return Vec::new();
        }
        let image_name = instance
            .image_id
            .as_deref()
            .and_then(|image_id| self.images.get(image_id))
            .and_then(|image| image.name.clone());
        uefi_firmware_variables(&instance.architecture, image_name.as_deref())
    }
}

/// The non-volatile variables the UEFI firmware of a machine of `architecture` holds, whose
/// boot option names `description`.
///
/// Every one is a variable the UEFI specification defines globally (section "Globally Defined
/// Variables") and marks non-volatile, so every one belongs in a stored variable store:
///
/// * `PlatformLangCodes` and `PlatformLang` — the languages the platform supports and the one
///   it reports in;
/// * `Timeout` — the seconds the firmware waits before starting the first boot option;
/// * `Boot0000` — the `EFI_LOAD_OPTION` of the loader the machine starts; and
/// * `BootOrder` — the order the boot options are attempted in;
/// * `SetupMode`, `AuditMode` and `DeployedMode` — the secure-boot operating mode, which is
///   setup mode for a platform that holds no enrolled platform key.
pub fn uefi_firmware_variables(architecture: &str, description: Option<&str>) -> Vec<UefiVariable> {
    let global = |name: &str, data: Vec<u8>| UefiVariable {
        name: name.to_string(),
        vendor_guid: EFI_GLOBAL_VARIABLE_GUID,
        attributes: UEFI_NON_VOLATILE_ATTRIBUTES,
        data,
    };
    let language = {
        let mut bytes = PLATFORM_LANGUAGE.as_bytes().to_vec();
        // A `PlatformLang` value is a NUL-terminated ASCII language code.
        bytes.push(0);
        bytes
    };
    vec![
        global("PlatformLangCodes", language.clone()),
        global("PlatformLang", language),
        global("Timeout", BOOT_TIMEOUT_SECONDS.to_le_bytes().to_vec()),
        global(
            &boot_option_name(FIRST_BOOT_OPTION),
            load_option(
                description.unwrap_or(DEFAULT_LOADER_DESCRIPTION),
                default_loader_path(architecture),
            ),
        ),
        // "BootOrder" is an ordered list of the boot option numbers, each a 16-bit value.
        global("BootOrder", FIRST_BOOT_OPTION.to_le_bytes().to_vec()),
        global("SetupMode", vec![SETUP_MODE_WITHOUT_PLATFORM_KEY]),
        global("AuditMode", vec![0]),
        global("DeployedMode", vec![0]),
    ]
}

/// The name of boot option `number`: `Boot####`, the four UPPERCASE hexadecimal digits of the
/// option number, as the UEFI specification spells a `Boot####` variable.
pub fn boot_option_name(number: u16) -> String {
    format!("Boot{number:04X}")
}

/// The file the firmware of a machine of `architecture` starts when nothing more specific is
/// registered, as the UEFI specification's removable-media boot behaviour names it per
/// machine type: `BOOTX64.EFI` on x86-64, `BOOTAA64.EFI` on 64-bit Arm and `BOOTIA32.EFI` on
/// 32-bit x86.
pub fn default_loader_path(architecture: &str) -> String {
    let file = match architecture {
        "arm64" | "aarch64" => "BOOTAA64.EFI",
        "i386" => "BOOTIA32.EFI",
        _ => "BOOTX64.EFI",
    };
    format!("\\EFI\\BOOT\\{file}")
}

/// An `EFI_LOAD_OPTION` for the loader at `path`, described to a user as `description`: the
/// value a `Boot####` variable carries (UEFI specification, "Load Options").
///
/// `Attributes` marks the option one the firmware attempts, the file path list is the ONE
/// media file-path node naming `path` followed by the end-of-path node, and there is no
/// optional data.
fn load_option(description: &str, path: String) -> Vec<u8> {
    let file_path_list = {
        let mut nodes = file_path_node(&path);
        nodes.extend_from_slice(&[
            END_DEVICE_PATH_TYPE,
            END_ENTIRE_DEVICE_PATH_SUBTYPE,
            // A device path node's length covers its own header.
            4,
            0,
        ]);
        nodes
    };
    let mut option = Vec::new();
    option.extend_from_slice(&LOAD_OPTION_ACTIVE.to_le_bytes());
    // The length of the file path list, in bytes, which the option carries so a reader can
    // find the optional data after it.
    let length = u16::try_from(file_path_list.len()).unwrap_or(u16::MAX);
    option.extend_from_slice(&length.to_le_bytes());
    option.extend_from_slice(&ucs2(description));
    option.extend_from_slice(&file_path_list);
    option
}

/// A MEDIA / FILE PATH device path node naming `path`: the node header (type, sub-type and
/// its own total length) followed by the path as a NUL-terminated UCS-2 string.
fn file_path_node(path: &str) -> Vec<u8> {
    let name = ucs2(path);
    let mut node = Vec::with_capacity(name.len() + 4);
    node.push(MEDIA_DEVICE_PATH_TYPE);
    node.push(MEDIA_FILE_PATH_SUBTYPE);
    let length = u16::try_from(name.len() + 4).unwrap_or(u16::MAX);
    node.extend_from_slice(&length.to_le_bytes());
    node.extend_from_slice(&name);
    node
}

/// `text` as the NUL-terminated little-endian UCS-2 string every name and description in a
/// UEFI structure is written as. A character outside the basic multilingual plane has no UCS-2
/// code unit, so it is written as the Unicode replacement character rather than split into
/// surrogates.
fn ucs2(text: &str) -> Vec<u8> {
    /// U+FFFD, the character Unicode replaces one it cannot represent with.
    const REPLACEMENT: u16 = 0xfffd;

    let mut bytes = Vec::with_capacity(text.len() * 2 + 2);
    for character in text.chars() {
        let unit = u16::try_from(u32::from(character)).unwrap_or(REPLACEMENT);
        bytes.extend_from_slice(&unit.to_le_bytes());
    }
    bytes.extend_from_slice(&0_u16.to_le_bytes());
    bytes
}

/// The variable store `variables` make up, as `GetInstanceUefiData` and
/// `DescribeImageAttribute` report one: [`UEFI_STORE_MAGIC`] and the zlib stream of the
/// records the module docs lay out, base64-encoded — "a base64 encoded and zlib compressed
/// binary value" (`GetInstanceUefiData`).
pub fn encode_uefi_variable_store(variables: &[UefiVariable]) -> String {
    let mut binary = Vec::new();
    for variable in variables {
        let name = ucs2(&variable.name);
        let length = |bytes: usize| u32::try_from(bytes).unwrap_or(u32::MAX).to_le_bytes();
        binary.extend_from_slice(&variable.attributes.to_le_bytes());
        binary.extend_from_slice(&length(name.len()));
        binary.extend_from_slice(&length(variable.data.len()));
        binary.extend_from_slice(&variable.vendor_guid);
        binary.extend_from_slice(&name);
        binary.extend_from_slice(&variable.data);
    }
    let compressed = {
        use ::std::io::Write as _;

        let mut encoder =
            ::flate2::write::ZlibEncoder::new(Vec::new(), ::flate2::Compression::default());
        // Writing to a `Vec` cannot fail, so neither of these can: the store is compressed in
        // memory. An impossible failure is loud rather than silently producing a store whose
        // records nothing can read.
        encoder
            .write_all(&binary)
            .and_then(|()| encoder.finish())
            .expect("compressing a variable store into a Vec cannot fail")
    };
    let mut store = UEFI_STORE_MAGIC.to_vec();
    store.extend_from_slice(&compressed);
    ::base64::engine::general_purpose::STANDARD.encode(store)
}

/// The variables a store `encoded` by [`encode_uefi_variable_store`] holds, or `None` for a
/// value that is not one: text outside base64, bytes that do not open with
/// [`UEFI_STORE_MAGIC`], bytes outside a zlib stream, records that inflate past
/// [`MAX_UEFI_STORE_BYTES`], or a record whose declared lengths run past the end of the store.
///
/// The inverse of the encoder, so a store can be taken apart variable by variable rather than
/// compared as opaque text: it is READER-side rather than on the serving path — a client, and
/// this crate's own suite, reads a store back through it, while `GetInstanceUefiData` only ever
/// writes one.
pub fn decode_uefi_variable_store(encoded: &str) -> Option<Vec<UefiVariable>> {
    let store = ::base64::engine::general_purpose::STANDARD
        .decode(encoded.as_bytes())
        .ok()?;
    let compressed = store.strip_prefix(UEFI_STORE_MAGIC.as_slice())?;
    let binary = {
        use ::std::io::Read as _;

        // The decompressed records are read through a ceiling, so a small value whose zlib
        // stream expands without bound cannot grow this allocation without bound: a stream
        // that fills the ceiling is one no store of this service wrote, and is refused rather
        // than read further.
        let mut decoder =
            ::flate2::read::ZlibDecoder::new(compressed).take(MAX_UEFI_STORE_BYTES);
        let mut binary = Vec::new();
        decoder.read_to_end(&mut binary).ok()?;
        if u64::try_from(binary.len()).unwrap_or(u64::MAX) >= MAX_UEFI_STORE_BYTES {
            return None;
        }
        binary
    };

    // Every record opens with its attributes, the two lengths and the vendor GUID: 4 + 4 + 4
    // + 16 bytes, the header the module docs lay out.
    const HEADER_BYTES: usize = 28;

    let mut variables = Vec::new();
    let mut rest = binary.as_slice();
    while !rest.is_empty() {
        let (header, tail) = rest.split_at_checked(HEADER_BYTES)?;
        let number = |at: usize| {
            let bytes: [u8; 4] = header.get(at..at + 4)?.try_into().ok()?;
            usize::try_from(u32::from_le_bytes(bytes)).ok()
        };
        let attributes = u32::from_le_bytes(header.get(0..4)?.try_into().ok()?);
        let name_length = number(4)?;
        let data_length = number(8)?;
        let vendor_guid: [u8; 16] = header.get(12..HEADER_BYTES)?.try_into().ok()?;
        let (name, tail) = tail.split_at_checked(name_length)?;
        let (data, tail) = tail.split_at_checked(data_length)?;
        variables.push(UefiVariable {
            name: from_ucs2(name)?,
            vendor_guid,
            attributes,
            data: data.to_vec(),
        });
        rest = tail;
    }
    Some(variables)
}

/// The text a NUL-terminated little-endian UCS-2 string holds, or `None` for bytes that are
/// not one: an odd length, a missing terminator or a code unit that is no character.
fn from_ucs2(bytes: &[u8]) -> Option<String> {
    if !bytes.len().is_multiple_of(2) {
        return None;
    }
    let units: Vec<u16> = bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| u16::from_le_bytes(*pair))
        .collect();
    let (terminator, units) = units.split_last()?;
    if *terminator != 0 {
        return None;
    }
    units
        .iter()
        .map(|unit| char::from_u32(u32::from(*unit)))
        .collect()
}
