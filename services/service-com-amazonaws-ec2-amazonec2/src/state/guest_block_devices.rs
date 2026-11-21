// SPDX-License-Identifier: CC0-1.0

//! What one instance's GUEST sees of its own BLOCK DEVICES — the volumes attached to it, as
//! the kernel inside it presents them.
//!
//! # Why a guest needs this
//!
//! "After you attach an Amazon EBS volume to your instance, it is exposed as a block device"
//! (EC2 User Guide, "Make an Amazon EBS volume available for use"), and the documented first
//! step of using one is `lsblk`: a client attaches a volume and then goes INTO the guest to
//! find the device, put a filesystem on it and mount it. So an instance whose guest reports no
//! block device at all cannot be used for the thing a volume is attached for, however
//! faithfully `DescribeVolumes` reports the attachment — and nothing inside the machine can
//! tell which device carries which volume, or how large it is.
//!
//! # One derivation, two readers
//!
//! The devices here are read off the ONE record of an attachment this crate holds — the volume
//! store, which the launch that mints a root volume and `AttachVolume` alike write
//! ([`ComAmazonawsEc2Amazonec2State::instance_block_device_mappings`]) — and each one's size is
//! the size of the volume attached there. So the guest's own `lsblk` and `DescribeVolumes`
//! cannot disagree about which volumes an instance holds or how large they are, and a volume
//! whose attachment the clock has not made `attached` yet is not a device in the guest, exactly
//! as it is not one on a real instance.
//!
//! What the kernel NAMES a device is the instance type's own question: "EBS volumes are exposed
//! as NVMe block devices on instances built on the Nitro System" (EC2 User Guide, "Amazon EBS
//! and NVMe on Linux instances"), where the name the request named is not the name the guest
//! sees, while a pre-Nitro instance renames `/dev/sd*` to `/dev/xvd*`
//! ([`guest_device_node`]).
//!
//! # No disk, no host
//!
//! The program below is a PROGRAM OF THE GUEST, performed by the same bounded, in-process shell
//! session the guest's own boot script runs in ([`super::guest_console`]): it opens no device,
//! reads no host path and reports only what the stored instance and its volumes say. The
//! columns it writes are the ones this emulator really derives; an invocation asking for a
//! column nothing here holds answers as a command that could not be run rather than a value
//! nothing stands behind.

use ::service_common::posix_shell_session::{EnvironmentPrograms, ProgramFiles, ProgramOutcome};

use super::guest_console::GuestMachine;

/// The program a guest lists its block devices with.
pub const BLOCK_DEVICE_PROGRAM: &str = "lsblk";

/// The directory the kernel publishes one directory per block device under, where a guest
/// reads a device's own facts from.
pub const GUEST_BLOCK_DIRECTORY: &str = "/sys/block";

/// The status a program of this guest exits with when it did what it was asked.
const SUCCESS: i32 = 0;

/// The status `lsblk` exits with when the devices its operands named are not devices of this
/// machine: "32 none of specified devices found" (`lsblk` manual, EXIT STATUS).
const NO_SUCH_DEVICE: i32 = 32;

/// The bytes of one SECTOR, which is the unit the kernel publishes a device's size in at
/// `/sys/block/<device>/size` — "the number of 512-byte sectors", whatever the device's own
/// logical block size is.
const SECTOR_BYTES: i64 = 512;

/// The bytes of one GiB, which is the unit a volume's size is stated in
/// (`Volume$Size`: "the size of the volume, in GiBs").
const GIB_BYTES: i64 = 1024 * 1024 * 1024;

/// The block-device MAJOR number of the NVMe driver, which is the driver that presents a
/// volume attached to an instance built on the Nitro System, and the one of the Xen block
/// front-end driver, which presents a volume attached to every other instance type.
///
/// The kernel's own numbers (`Documentation/admin-guide/devices.txt`: 259 "Block Extended
/// Major", which is where NVMe namespaces are enumerated, and 202 "Xen Virtual Block Device").
const NVME_MAJOR: i64 = 259;
const XEN_BLOCK_MAJOR: i64 = 202;

/// How many MINOR numbers the Xen block front-end driver gives one device: 16 per disk, which
/// is what makes `xvda` `202:0` and `xvdb` `202:16` — the driver's own allocation, 15 of them
/// being that disk's partitions.
const XEN_MINORS_PER_DISK: i64 = 16;

/// The model `lsblk` reports for a block device that is an Amazon EBS volume: the model string
/// the NVMe device presents, which the EC2 User Guide's own `lsblk`/`nvme id-ctrl` output shows
/// as "Amazon Elastic Block Store".
const EBS_DEVICE_MODEL: &str = "Amazon Elastic Block Store";

/// What the SERIAL of such a device begins with: "the volume ID … is in the serial number
/// field" of an EBS NVMe device, written as the volume id with its `-` removed
/// ([`GuestBlockDevice::serial`]).
const VOLUME_ID_PREFIX: &str = "vol";

/// The directory the root filesystem of a guest is mounted at, which is the MOUNTPOINT the
/// device carrying it reports.
const ROOT_MOUNTPOINT: &str = "/";

/// The `TYPE` a whole device reports, as `lsblk` names it.
const DISK_TYPE: &str = "disk";

/// One block device of one instance's guest: the volume attached to it, under the name the
/// kernel inside the guest gives it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuestBlockDevice {
    /// The name the kernel gives the device — `nvme1n1`, `xvdf` — which is what `lsblk` writes
    /// and what the guest holds the device node for ([`guest_device_node`]).
    pub node: String,
    /// The device the ATTACHMENT names (`VolumeAttachment$Device`), which is the name the
    /// request asked for rather than the one the guest sees.
    pub device_name: String,
    /// The volume attached there (`VolumeAttachment$VolumeId`).
    pub volume_id: String,
    /// That volume's size in GiB (`Volume$Size`).
    pub size_gib: i32,
    /// Whether the guest's ROOT filesystem is on this device, which is what makes it the one
    /// reported as mounted at `/`.
    pub root: bool,
    /// The device's major and minor numbers, as the driver presenting it allocates them
    /// ([`NVME_MAJOR`], [`XEN_BLOCK_MAJOR`]).
    pub major_minor: (i64, i64),
}

impl GuestBlockDevice {
    /// The device's size in bytes, which is the volume's size.
    #[must_use]
    pub fn size_bytes(&self) -> i64 {
        i64::from(self.size_gib).saturating_mul(GIB_BYTES)
    }

    /// The path of the device node in the guest: `/dev/` and the name the kernel gave it.
    #[must_use]
    pub fn path(&self) -> String {
        format!("/dev/{}", self.node)
    }

    /// The SERIAL this device presents: the volume's id without its `-`, which is how the
    /// volume id reaches the guest.
    #[must_use]
    pub fn serial(&self) -> String {
        let digits = self
            .volume_id
            .strip_prefix(VOLUME_ID_PREFIX)
            .unwrap_or(&self.volume_id)
            .trim_start_matches('-');
        format!("{VOLUME_ID_PREFIX}{digits}")
    }

    /// Where this device is mounted in the guest: the root filesystem's directory for the
    /// device carrying it, and nothing for a device the guest has not mounted — an attached
    /// volume is not mounted until something in the guest mounts it.
    #[must_use]
    pub fn mountpoint(&self) -> &str {
        if self.root {
            ROOT_MOUNTPOINT
        } else {
            ""
        }
    }
}

/// The name the KERNEL inside the guest gives the volume attached at `device_name`, the
/// `index`th device of the machine.
///
/// On an instance built on the Nitro System "EBS volumes are exposed as NVMe block devices",
/// and "the device names that you specify … are renamed using NVMe device names
/// (`/dev/nvme[0-26]n1`)", in the order the devices were attached — so the name the request
/// named is not a name the guest sees at all. On every other instance type the guest sees the
/// name it asked for, with `/dev/sd*` renamed to `/dev/xvd*` ("depending on the block device
/// driver of the kernel, the device might be attached with a different name than you
/// specified").
///
/// `index` is therefore that order, which the caller reads off the attachments themselves
/// ([`ComAmazonawsEc2Amazonec2State::guest_block_devices`]).
#[must_use]
pub fn guest_device_node(device_name: &str, index: usize, nitro: bool) -> String {
    if nitro {
        return format!("nvme{index}n1");
    }
    let named = device_name.rsplit('/').next().unwrap_or(device_name);
    match named.strip_prefix("sd") {
        Some(rest) => format!("xvd{rest}"),
        None => named.to_string(),
    }
}

/// The major and minor numbers the driver presenting `node` gives it, as that driver allocates
/// them.
///
/// The Xen front-end's allocation is its own: 16 minors per disk, in the order its letters run,
/// which is what makes `xvda` `202:0` and `xvdb` `202:16`. For an NVMe namespace the kernel
/// allocates a minor of the extended block major as each device is discovered, so the minor
/// here is the device's own position among the machine's devices — DERIVED, there being no
/// device number in any EC2 API to read one off.
#[must_use]
pub fn guest_device_numbers(node: &str, index: usize, nitro: bool) -> (i64, i64) {
    if nitro {
        return (NVME_MAJOR, i64::try_from(index).unwrap_or_default());
    }
    let letters = node.trim_start_matches(|character: char| !character.is_ascii_lowercase());
    let slot = letters
        .strip_prefix("xvd")
        .or_else(|| letters.strip_prefix("sd"))
        .and_then(|rest| rest.chars().next())
        // The letter of the disk, which is what the driver allocates by; a name carrying
        // none is the machine's first disk.
        .filter(char::is_ascii_lowercase)
        .map_or(0, |letter| i64::from((letter as u8).saturating_sub(b'a')));
    (XEN_BLOCK_MAJOR, slot.saturating_mul(XEN_MINORS_PER_DISK))
}

/// Every file the kernel publishes about `devices`, as `(path, contents)`: one `size` per
/// device, in the 512-byte sectors the kernel states a device's size in.
///
/// That path is where a guest's own scripts read a device's capacity from, beside `lsblk`, so a
/// device the guest can see is one it can measure.
#[must_use]
pub fn published_block_device_files(devices: &[GuestBlockDevice]) -> Vec<(String, String)> {
    devices
        .iter()
        .map(|device| {
            (
                format!("{GUEST_BLOCK_DIRECTORY}/{}/size", device.node),
                format!("{}\n", device.size_bytes() / SECTOR_BYTES),
            )
        })
        .collect()
}

/// The programs a guest reads its own block devices with, for a session that runs a program of
/// that guest.
///
/// `None` for a machine with no block device at all: a guest with nothing attached has no
/// program that lists one either, which is the same answer a shell gives for a utility it has
/// not got.
#[must_use]
pub fn block_device_programs(machine: &GuestMachine) -> Option<Box<dyn EnvironmentPrograms>> {
    (!machine.block_devices.is_empty()).then(|| {
        Box::new(BlockDevicePrograms {
            devices: machine.block_devices.clone(),
        }) as Box<dyn EnvironmentPrograms>
    })
}

/// The block-device programs of one machine's guest.
///
/// Holds that machine's devices and nothing else — no store guard, no engine and no host
/// handle is reachable from a program.
#[derive(Debug)]
struct BlockDevicePrograms {
    /// The devices these programs report, read once when the programs were installed.
    devices: Vec<GuestBlockDevice>,
}

impl EnvironmentPrograms for BlockDevicePrograms {
    /// `lsblk` writes one row per block device of this machine.
    ///
    /// A program named by its absolute path is the same program, as a shell resolving a name on
    /// `PATH` finds. An invocation this guest performs nothing of answers `None` — a form it
    /// has no answer for — rather than a listing that would not be the answer to what was
    /// asked.
    fn run(
        &self,
        name: &str,
        arguments: &[String],
        files: &mut dyn ProgramFiles,
    ) -> Option<ProgramOutcome> {
        let _ = files;
        let name = name.rsplit('/').next().unwrap_or(name);
        if name != BLOCK_DEVICE_PROGRAM {
            return None;
        }
        let invocation = Listing::of(arguments)?;
        let selected = invocation.selected(&self.devices);
        if selected.is_empty() {
            // "none of specified devices found": the operands named nothing this machine
            // holds, so there is nothing to write about them.
            return Some(ProgramOutcome {
                status: NO_SUCH_DEVICE,
                output: Vec::new(),
            });
        }
        Some(ProgramOutcome {
            status: SUCCESS,
            output: invocation.written(&selected),
        })
    }
}

/// What one `lsblk` invocation asks for: which columns, in which form, about which devices.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Listing {
    /// The columns, in the order they are written.
    columns: Vec<Column>,
    /// `-b`/`--bytes`: "print the sizes in bytes rather than in a human-readable format".
    bytes: bool,
    /// `-n`/`--noheadings`: "do not print a header line".
    headings: bool,
    /// How the rows are laid out ([`Layout`]).
    layout: Layout,
    /// `-p`/`--paths`: "print full device paths" in the name column.
    paths: bool,
    /// The devices the operands named, empty for an invocation about every device of the
    /// machine. A device is named by its node or by its path.
    operands: Vec<String>,
}

/// How one invocation lays its rows out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Layout {
    /// The default: columns padded to the widest value in each.
    Columns,
    /// `-r`/`--raw`: "use the raw output format", one space between values and no padding.
    Raw,
    /// `-P`/`--pairs`: "produce output in the form of key=\"value\" pairs".
    Pairs,
}

/// One column `lsblk` writes, as the name an invocation selects it by.
///
/// Exactly the columns this emulator DERIVES: the device's names, its size, what it is, where
/// it is mounted and the two facts an EBS device presents about itself. A column outside this
/// table is one no invocation here writes ([`Column::of_name`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Column {
    /// `NAME`: the device's name — with its full path for `--paths`.
    Name,
    /// `KNAME`: "internal kernel device name", which is the same name without a path.
    KernelName,
    /// `PATH`: "path to the device node".
    Path,
    /// `MAJ:MIN`: "major:minor device number".
    DeviceNumber,
    /// `SIZE`: "size of the device".
    Size,
    /// `RM`: "removable device", which a volume is not.
    Removable,
    /// `RO`: "read-only device", which a volume attached here is not.
    ReadOnly,
    /// `TYPE`: "device type", which is a whole disk for every device here.
    Type,
    /// `MOUNTPOINT`: "where the device is mounted".
    Mountpoint,
    /// `MOUNTPOINTS`: the same column under the plural spelling, which is the one `lsblk`'s own
    /// default output carries — kept apart from the singular so an invocation is answered under
    /// the heading it asked for.
    Mountpoints,
    /// `MODEL`: "device identifier".
    Model,
    /// `SERIAL`: "disk serial number".
    Serial,
}

impl Column {
    /// The column `name` selects, or `None` for a column this guest writes nothing for.
    fn of_name(name: &str) -> Option<Self> {
        match name {
            "NAME" => Some(Self::Name),
            "KNAME" => Some(Self::KernelName),
            "PATH" => Some(Self::Path),
            "MAJ:MIN" => Some(Self::DeviceNumber),
            "SIZE" => Some(Self::Size),
            "RM" => Some(Self::Removable),
            "RO" => Some(Self::ReadOnly),
            "TYPE" => Some(Self::Type),
            "MOUNTPOINT" => Some(Self::Mountpoint),
            "MOUNTPOINTS" => Some(Self::Mountpoints),
            "MODEL" => Some(Self::Model),
            "SERIAL" => Some(Self::Serial),
            _ => None,
        }
    }

    /// The heading this column is written under, which is the name it is selected by.
    fn heading(self) -> &'static str {
        match self {
            Self::Name => "NAME",
            Self::KernelName => "KNAME",
            Self::Path => "PATH",
            Self::DeviceNumber => "MAJ:MIN",
            Self::Size => "SIZE",
            Self::Removable => "RM",
            Self::ReadOnly => "RO",
            Self::Type => "TYPE",
            Self::Mountpoint => "MOUNTPOINT",
            Self::Mountpoints => "MOUNTPOINTS",
            Self::Model => "MODEL",
            Self::Serial => "SERIAL",
        }
    }

    /// Whether the values of this column are written to the RIGHT of it, as `lsblk` writes the
    /// numeric ones.
    fn right_aligned(self) -> bool {
        matches!(self, Self::Size | Self::Removable | Self::ReadOnly)
    }

    /// What `device` reports in this column, under an invocation reading sizes in `bytes` and
    /// naming devices by their `paths`.
    fn value(self, device: &GuestBlockDevice, bytes: bool, paths: bool) -> String {
        match self {
            Self::Name if paths => device.path(),
            Self::Name | Self::KernelName => device.node.clone(),
            Self::Path => device.path(),
            Self::DeviceNumber => {
                let (major, minor) = device.major_minor;
                format!("{major}:{minor}")
            }
            Self::Size if bytes => device.size_bytes().to_string(),
            Self::Size => human_size(device.size_bytes()),
            // "removable device" and "read-only device": an EBS volume attached to an
            // instance is neither.
            Self::Removable | Self::ReadOnly => "0".to_string(),
            Self::Type => DISK_TYPE.to_string(),
            Self::Mountpoint | Self::Mountpoints => device.mountpoint().to_string(),
            Self::Model => EBS_DEVICE_MODEL.to_string(),
            Self::Serial => device.serial(),
        }
    }
}

/// The columns an invocation that selected none writes, which is `lsblk`'s own default output.
const DEFAULT_COLUMNS: &[Column] = &[
    Column::Name,
    Column::DeviceNumber,
    Column::Removable,
    Column::Size,
    Column::ReadOnly,
    Column::Type,
    Column::Mountpoints,
];

impl Listing {
    /// What `arguments` ask for, or `None` for an invocation this guest performs nothing of: an
    /// option it does not read, a column it holds nothing for, or `--output` naming no columns
    /// at all.
    fn of(arguments: &[String]) -> Option<Self> {
        let mut listing = Self {
            columns: Vec::new(),
            bytes: false,
            headings: true,
            layout: Layout::Columns,
            paths: false,
            operands: Vec::new(),
        };
        let mut arguments = arguments.iter();
        while let Some(argument) = arguments.next() {
            if let Some(name) = argument.strip_prefix("--") {
                let (name, inline) = match name.split_once('=') {
                    Some((name, value)) => (name, Some(value.to_string())),
                    None => (name, None),
                };
                match name {
                    "bytes" => listing.bytes = true,
                    "noheadings" => listing.headings = false,
                    "raw" => listing.layout = Layout::Raw,
                    "pairs" => listing.layout = Layout::Pairs,
                    "paths" => listing.paths = true,
                    // "do not print holder devices or slaves", "use list output format" and
                    // "also list empty devices": every device of this machine is a whole disk
                    // with no holder and none of them is empty, so each asks for the rows
                    // written anyway.
                    "nodeps" | "list" | "all" => {}
                    "output" => {
                        let selected = match inline {
                            Some(value) => value,
                            None => arguments.next()?.clone(),
                        };
                        listing.select(&selected)?;
                    }
                    _ => return None,
                }
                continue;
            }
            if let Some(letters) = argument.strip_prefix('-').filter(|rest| !rest.is_empty()) {
                let mut letters = letters.chars().peekable();
                while let Some(letter) = letters.next() {
                    match letter {
                        'b' => listing.bytes = true,
                        'n' => listing.headings = false,
                        'r' => listing.layout = Layout::Raw,
                        'P' => listing.layout = Layout::Pairs,
                        'p' => listing.paths = true,
                        'd' | 'l' | 'a' => {}
                        'o' => {
                            // `-o LIST`: the columns are the word after this one, so the
                            // letter is the last of its own word.
                            if letters.peek().is_some() {
                                return None;
                            }
                            let selected = arguments.next()?.clone();
                            listing.select(&selected)?;
                        }
                        _ => return None,
                    }
                }
                continue;
            }
            listing.operands.push(argument.clone());
        }
        if listing.columns.is_empty() {
            listing.columns = DEFAULT_COLUMNS.to_vec();
        }
        Some(listing)
    }

    /// Select the columns `list` names, as the comma-separated list `--output` takes — `None`
    /// when it names a column this guest holds nothing for, or no column at all.
    ///
    /// A LATER `--output` replaces what an earlier one selected, which is the option's own rule:
    /// the columns are the ones the last of them named.
    fn select(&mut self, list: &str) -> Option<()> {
        let named: Vec<&str> = list.split(',').filter(|name| !name.is_empty()).collect();
        if named.is_empty() {
            return None;
        }
        self.columns.clear();
        for name in named {
            self.columns.push(Column::of_name(&name.to_uppercase())?);
        }
        Some(())
    }

    /// The devices of `devices` this invocation is about: the ones its operands named, and
    /// every one of them for an invocation that named none.
    fn selected<'a>(&self, devices: &'a [GuestBlockDevice]) -> Vec<&'a GuestBlockDevice> {
        if self.operands.is_empty() {
            return devices.iter().collect();
        }
        devices
            .iter()
            .filter(|device| {
                self.operands.iter().any(|operand| {
                    let named = operand.rsplit('/').next().unwrap_or(operand);
                    named == device.node
                })
            })
            .collect()
    }

    /// The lines this invocation writes about `devices`.
    fn written(&self, devices: &[&GuestBlockDevice]) -> Vec<String> {
        let rows: Vec<Vec<String>> = devices
            .iter()
            .map(|device| {
                self.columns
                    .iter()
                    .map(|column| column.value(device, self.bytes, self.paths))
                    .collect()
            })
            .collect();
        match self.layout {
            // Each value under the key naming its column, which is what a program reading
            // one line at a time parses.
            Layout::Pairs => rows
                .iter()
                .map(|values| {
                    self.columns
                        .iter()
                        .zip(values)
                        .map(|(column, value)| format!("{}=\"{value}\"", column.heading()))
                        .collect::<Vec<String>>()
                        .join(" ")
                })
                .collect(),
            // One space between values, with no padding at all.
            Layout::Raw => {
                let mut written: Vec<String> = Vec::new();
                if self.headings {
                    written.push(
                        self.columns
                            .iter()
                            .map(|column| column.heading().to_string())
                            .collect::<Vec<String>>()
                            .join(" "),
                    );
                }
                written.extend(rows.iter().map(|values| values.join(" ")));
                written
            }
            Layout::Columns => self.in_columns(&rows),
        }
    }

    /// [`Self::written`]'s default layout: every value padded to the widest in its column —
    /// the numeric columns to the right of it — with nothing padded past the last value of a
    /// line.
    fn in_columns(&self, rows: &[Vec<String>]) -> Vec<String> {
        let widths: Vec<usize> = self
            .columns
            .iter()
            .enumerate()
            .map(|(index, column)| {
                let heading = if self.headings {
                    column.heading().chars().count()
                } else {
                    0
                };
                rows.iter()
                    .filter_map(|values| values.get(index))
                    .map(|value| value.chars().count())
                    .chain([heading])
                    .max()
                    .unwrap_or_default()
            })
            .collect();
        let laid_out = |values: &[String]| -> String {
            let mut line = String::new();
            for (index, value) in values.iter().enumerate() {
                if index > 0 {
                    line.push(' ');
                }
                let width = widths.get(index).copied().unwrap_or_default();
                let padding = width.saturating_sub(value.chars().count());
                let right = self
                    .columns
                    .get(index)
                    .is_some_and(|column| column.right_aligned());
                if right {
                    line.push_str(&" ".repeat(padding));
                    line.push_str(value);
                } else {
                    line.push_str(value);
                    line.push_str(&" ".repeat(padding));
                }
            }
            line.trim_end().to_string()
        };
        let mut written: Vec<String> = Vec::new();
        if self.headings {
            let headings: Vec<String> = self
                .columns
                .iter()
                .map(|column| column.heading().to_string())
                .collect();
            written.push(laid_out(&headings));
        }
        written.extend(rows.iter().map(|values| laid_out(values)));
        written
    }
}

/// The suffixes a size is written with, smallest first: the one-letter form `lsblk` writes a
/// size in when it was not asked for bytes.
const SIZE_SUFFIXES: &[&str] = &["B", "K", "M", "G", "T", "P", "E"];

/// `bytes` in the human-readable form `lsblk` writes a size in: the value in the largest unit
/// it is at least one of, with one decimal where that decimal is not zero — `8G`, `931.5G`,
/// `100M`.
fn human_size(bytes: i64) -> String {
    let mut value = bytes.max(0) as f64;
    let mut suffix = 0;
    while value >= 1024.0 && suffix + 1 < SIZE_SUFFIXES.len() {
        value /= 1024.0;
        suffix += 1;
    }
    let unit = SIZE_SUFFIXES.get(suffix).copied().unwrap_or_default();
    let tenths = (value * 10.0).round() / 10.0;
    if (tenths.fract() * 10.0).round() == 0.0 {
        return format!("{:.0}{unit}", tenths);
    }
    format!("{tenths:.1}{unit}")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A device of `size_gib` GiB attached at `device_name`, the `index`th of a machine of
    /// `nitro` kind.
    fn device(
        device_name: &str,
        volume_id: &str,
        size_gib: i32,
        index: usize,
        nitro: bool,
        root: bool,
    ) -> GuestBlockDevice {
        let node = guest_device_node(device_name, index, nitro);
        GuestBlockDevice {
            major_minor: guest_device_numbers(&node, index, nitro),
            node,
            device_name: device_name.to_string(),
            volume_id: volume_id.to_string(),
            size_gib,
            root,
        }
    }

    /// One row of the table below: the case, the device the attachment names, the device's
    /// position among the machine's, whether the machine presents its volumes as NVMe devices,
    /// and the node and device numbers its guest is then to see.
    type NodeRow = (&'static str, &'static str, usize, bool, &'static str, (i64, i64));

    /// RULE: the name the guest sees is the instance type's own — an NVMe namespace per
    /// attached volume on a Nitro machine, whatever name the request asked for, and the name
    /// asked for with `sd` renamed to `xvd` on every other type.
    ///
    /// The rows differ along what the rule generalises over: the kind of machine, the name the
    /// attachment asked for, how it is spelled and the device's position.
    #[test]
    fn the_node_a_guest_sees_follows_the_machines_device_model() {
        let rows: &[NodeRow] = &[
            ("a Nitro root volume", "/dev/xvda", 0, true, "nvme0n1", (259, 0)),
            ("a Nitro data volume", "/dev/sdf", 1, true, "nvme1n1", (259, 1)),
            (
                "another one, named the other way",
                "/dev/xvdg",
                2,
                true,
                "nvme2n1",
                (259, 2),
            ),
            ("a Xen root volume", "/dev/sda1", 0, false, "xvda1", (202, 0)),
            ("a Xen data volume", "/dev/sdf", 1, false, "xvdf", (202, 80)),
            ("a Xen volume already named xvd", "/dev/xvdb", 1, false, "xvdb", (202, 16)),
            ("a bare name with no directory", "sdh", 1, false, "xvdh", (202, 112)),
        ];
        for (case, device_name, index, nitro, node, numbers) in rows {
            let held = device(device_name, "vol-0123456789abcdef0", 8, *index, *nitro, false);
            assert_eq!(
                held.node, *node,
                "case {case:?}: expected the node {node:?}, got {:?}",
                held.node
            );
            assert_eq!(
                held.major_minor, *numbers,
                "case {case:?}: expected the numbers {numbers:?}, got {:?}",
                held.major_minor
            );
        }
    }

    /// RULE: every column `lsblk` writes follows from the volume attached there — its size,
    /// its id in the serial, where it is mounted — under the heading the invocation asked for,
    /// and the kernel publishes the same size in sectors.
    #[test]
    fn every_column_follows_from_the_volume_attached_there() {
        let devices = vec![
            device("/dev/xvda", "vol-0123456789abcdef0", 8, 0, true, true),
            device("/dev/sdf", "vol-00fedcba9876543210", 1, 1, true, false),
        ];
        let listing = Listing::of(&["-o".to_string(), "NAME,SIZE,SERIAL,MOUNTPOINT".to_string()])
            .expect("the invocation is one this guest performs");
        let selected = listing.selected(&devices);
        let written = listing.written(&selected);
        assert_eq!(
            written,
            vec![
                "NAME    SIZE SERIAL                MOUNTPOINT".to_string(),
                "nvme0n1   8G vol0123456789abcdef0  /".to_string(),
                "nvme1n1   1G vol00fedcba9876543210".to_string(),
            ],
            "case \"the singular mount-point spelling\": got {written:?}"
        );
        // The plural spelling is the same column under the heading that spelling carries, which
        // is the one `lsblk`'s own default output writes.
        let plural = Listing::of(&["-o".to_string(), "NAME,MOUNTPOINTS".to_string()])
            .expect("the invocation is one this guest performs");
        let selected = plural.selected(&devices);
        let written = plural.written(&selected);
        assert_eq!(
            written,
            vec![
                "NAME    MOUNTPOINTS".to_string(),
                "nvme0n1 /".to_string(),
                "nvme1n1".to_string(),
            ],
            "case \"the plural mount-point spelling\": got {written:?}"
        );
        // A later `--output` replaces what an earlier one selected, as the option does.
        let replaced = Listing::of(&[
            "-o".to_string(),
            "NAME".to_string(),
            "--output=SIZE".to_string(),
        ])
        .expect("the invocation is one this guest performs");
        let selected = replaced.selected(&devices);
        let written = replaced.written(&selected);
        assert_eq!(
            written,
            vec!["SIZE".to_string(), "  8G".to_string(), "  1G".to_string()],
            "case \"a second output list\": got {written:?}"
        );
        let published = published_block_device_files(&devices);
        assert_eq!(
            published,
            vec![
                ("/sys/block/nvme0n1/size".to_string(), "16777216\n".to_string()),
                ("/sys/block/nvme1n1/size".to_string(), "2097152\n".to_string()),
            ],
            "case \"the published sizes\": got {published:?}"
        );
    }

    /// RULE: a size is written in bytes where the invocation asked for bytes, and otherwise in
    /// the largest unit the device is at least one of.
    ///
    /// The rows differ along what the rule generalises over: the size, whether it divides the
    /// unit evenly, and which unit it reaches.
    #[test]
    fn a_size_is_written_in_the_form_the_invocation_asked_for() {
        // (case, the bytes, the human-readable form)
        let rows: &[(&str, i64, &str)] = &[
            ("one GiB", 1_073_741_824, "1G"),
            ("eight GiB", 8_589_934_592, "8G"),
            ("one hundred GiB", 107_374_182_400, "100G"),
            ("a TiB", 1_099_511_627_776, "1T"),
            ("a thousand GiB", 1_073_741_824_000, "1000G"),
            ("a round decimal", 1_610_612_736, "1.5G"),
            ("a MiB", 1_048_576, "1M"),
            ("nothing at all", 0, "0B"),
        ];
        for (case, bytes, human) in rows {
            let written = human_size(*bytes);
            assert_eq!(
                written, *human,
                "case {case:?}: expected {human:?}, got {written:?}"
            );
        }
    }

    /// RULE: an invocation this guest performs nothing of is answered as a command that could
    /// not be run, rather than with a listing that is not what was asked for.
    #[test]
    fn an_invocation_this_guest_performs_nothing_of_is_refused() {
        // (case, the arguments, whether the invocation is performed)
        let rows: &[(&str, &[&str], bool)] = &[
            ("no arguments at all", &[], true),
            ("the common bytes-only listing", &["-bdn", "-o", "NAME,SIZE"], true),
            ("the same options spelled out", &["--bytes", "--nodeps"], true),
            ("an inline output list", &["--output=NAME,SIZE"], true),
            ("a lower-case column name", &["-o", "name"], true),
            ("the pairs form", &["-P"], true),
            ("a column nothing here holds", &["-o", "FSTYPE"], false),
            ("an option nothing here reads", &["--json"], false),
            ("an unknown letter", &["-z"], false),
            ("a letter after -o in one word", &["-ob"], false),
            ("-o naming nothing", &["-o"], false),
            ("an empty output list", &["-o", ""], false),
        ];
        for (case, arguments, performed) in rows {
            let arguments: Vec<String> = arguments.iter().map(|word| (*word).to_string()).collect();
            assert_eq!(
                Listing::of(&arguments).is_some(),
                *performed,
                "case {case:?}: expected the invocation to be performed {performed}"
            );
        }
    }

    /// RULE: an operand names which devices the listing is about, and an operand naming no
    /// device of this machine is answered with the status `lsblk` answers for one.
    #[test]
    fn an_operand_names_the_devices_the_listing_is_about() {
        let devices = vec![
            device("/dev/xvda", "vol-0123456789abcdef0", 8, 0, true, true),
            device("/dev/sdf", "vol-00fedcba9876543210", 1, 1, true, false),
        ];
        let programs = block_device_programs(&machine(devices.clone()))
            .expect("a machine with devices has the programs");
        struct NoFiles;
        impl ProgramFiles for NoFiles {
            fn read(&self, _path: &str) -> Option<String> {
                None
            }
            fn write(&mut self, _path: &str, _contents: &str) -> bool {
                false
            }
            fn remove(&mut self, _path: &str) -> bool {
                false
            }
        }
        // (case, the arguments, the status, the lines)
        let rows: &[(&str, &[&str], i32, &[&str])] = &[
            (
                "the device the second volume is on",
                &["-bdn", "-o", "SIZE", "/dev/nvme1n1"],
                SUCCESS,
                &["1073741824"],
            ),
            (
                "the same device named without a directory",
                &["-bdn", "-o", "SIZE", "nvme1n1"],
                SUCCESS,
                &["1073741824"],
            ),
            (
                "every device of the machine",
                &["-bdn", "-o", "SIZE"],
                SUCCESS,
                &["8589934592", "1073741824"],
            ),
            (
                "a device this machine has not got",
                &["-bdn", "-o", "SIZE", "/dev/nvme9n1"],
                NO_SUCH_DEVICE,
                &[],
            ),
        ];
        for (case, arguments, status, written) in rows {
            let arguments: Vec<String> = arguments.iter().map(|word| (*word).to_string()).collect();
            let outcome = programs
                .run(BLOCK_DEVICE_PROGRAM, &arguments, &mut NoFiles)
                .unwrap_or_else(|| panic!("case {case:?}: the invocation is performed"));
            assert_eq!(
                outcome.status, *status,
                "case {case:?}: expected status {status}, got {}",
                outcome.status
            );
            assert_eq!(
                outcome.output, *written,
                "case {case:?}: expected {written:?}, got {:?}",
                outcome.output
            );
        }
        assert!(
            block_device_programs(&machine(Vec::new())).is_none(),
            "case \"a machine with no device at all\": it has no program that lists one"
        );
    }

    /// A machine of the shape a launch stores, holding `block_devices`.
    fn machine(block_devices: Vec<GuestBlockDevice>) -> GuestMachine {
        GuestMachine {
            instance_id: "i-0123456789abcdef0".to_string(),
            instance_type: "m5.large".to_string(),
            image_id: None,
            architecture: "x86_64".to_string(),
            availability_zone: "us-west-2a".to_string(),
            private_ip: "10.0.0.5".to_string(),
            private_dns_name: "ip-10-0-0-5.us-west-2.compute.internal".to_string(),
            public_ip: None,
            launch_time: ::std::time::SystemTime::UNIX_EPOCH,
            kernel_release: "6.1.0".to_string(),
            root_device: None,
            processor: None,
            cpu: None,
            kernel_id: None,
            ramdisk_id: None,
            metadata_endpoint_enabled: true,
            metadata_token_required: false,
            instance_profile: None,
            block_devices,
        }
    }
}
