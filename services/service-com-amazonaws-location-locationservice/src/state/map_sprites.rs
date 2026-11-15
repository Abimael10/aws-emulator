// SPDX-License-Identifier: CC0-1.0

//! The sprite sheet a map resource publishes, and the two paired files a sprite
//! read serves from it.
//!
//! The `GetMapSprites` doc states what a sheet is: "The sprite sheet is a PNG
//! image paired with a JSON document describing the offsets of individual icons
//! that will be displayed on a rendered map." Both files here are derived from
//! ONE layout of the same icon set, so the offsets the JSON document reports are
//! the rectangles the PNG beside it really draws, and `@2x` is that same layout
//! at twice the scale.
//!
//! Nothing is stored per sheet: every function below is a pure function of the
//! map resource's own catalog entry (the data provider its style is sourced
//! from) and the requested pixel ratio, so a repeat read of one map's sheet
//! answers with the same bytes without a read ever writing to state.
//!
//! The iconography is this emulator's own: the API publishes no icon list, so
//! the sheet carries one small family per icon shape a rendered map needs — a
//! place disc, a location pin, a route shield — named and indexed the way the
//! sprite-index format a map renderer reads expects (`x`, `y`, `width`,
//! `height`, `pixelRatio` per entry).

/// An RGBA pixel: the colour model every sheet is drawn and encoded in, so an
/// icon can leave the space around it transparent.
pub type Rgba = [u8; 4];

/// The member a refusal about the requested sprite file names, as the model
/// spells it on `GetMapSpritesRequest`.
pub const FILE_NAME_MEMBER: &str = "FileName";

/// The sprite files a map resource publishes, in the order the
/// `GetMapSpritesRequest$FileName` member doc lists them: the sheet itself
/// ("Use the following file names for the sprite sheet: sprites.png
/// sprites@2x.png for high pixel density displays"), then the offsets document
/// ("For the JSON document containing image offsets. Use the following file
/// names: sprites.json sprites@2x.json").
///
/// The model admits exactly those four through the pattern on that member, which
/// [`FILE_NAME_PATTERN`] carries, so this list and that pattern describe the same
/// set.
pub const SPRITE_FILE_NAMES: [&str; 4] = [
    "sprites.png",
    "sprites@2x.png",
    "sprites.json",
    "sprites@2x.json",
];

/// The pattern the model declares on `GetMapSpritesRequest$FileName`
/// (`smithy_query(query="operation",operation="GetMapSprites")`), which is what
/// the request boundary refuses a file name outside [`SPRITE_FILE_NAMES`] by.
pub const FILE_NAME_PATTERN: &str = r"^sprites(@2x)?\.(png|json)$";

/// The media type the PNG sheet is served with. The `ContentType` member doc:
/// "the sprite sheet content type is image/png".
pub const SHEET_CONTENT_TYPE: &str = "image/png";

/// The media type the offsets document is served with. That same member doc:
/// "the sprite offset JSON document is application/json".
pub const INDEX_CONTENT_TYPE: &str = "application/json";

/// The `Cache-Control` directive every sprite file is served with.
///
/// A sheet is a function of the map's style alone, so it only changes when the
/// map resource does; the freshness window is this emulator's choice — the model
/// documents the member as "The HTTP Cache-Control directive for the value" and
/// publishes no value for it.
pub const SPRITE_CACHE_CONTROL: &str = "max-age=86400";

/// Which of a sheet's two paired files a `FileName` names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpriteFileKind {
    /// The PNG sheet itself.
    Sheet,
    /// The JSON document describing the sheet's icon offsets.
    Index,
}

/// A sprite file the API publishes: which of the two paired files it is, and the
/// pixel ratio it is drawn at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpriteFile {
    pub kind: SpriteFileKind,
    /// Device pixels per style pixel: 1 for the plain file, 2 for the `@2x`
    /// file the member doc offers "for high pixel density displays".
    pub pixel_ratio: u32,
}

/// The file kind and pixel ratio a sprite file name names, or `None` when the
/// name is not one of [`SPRITE_FILE_NAMES`].
///
/// Matched against the published set exactly rather than by extension, so
/// `sprites@3x.png` and `sprites.svg` are both outside it.
pub fn sprite_file(file_name: &str) -> Option<SpriteFile> {
    let (kind, pixel_ratio) = match file_name {
        "sprites.png" => (SpriteFileKind::Sheet, 1),
        "sprites@2x.png" => (SpriteFileKind::Sheet, 2),
        "sprites.json" => (SpriteFileKind::Index, 1),
        "sprites@2x.json" => (SpriteFileKind::Index, 2),
        _ => return None,
    };
    Some(SpriteFile { kind, pixel_ratio })
}

/// The media type a sprite file is served with.
pub fn content_type(kind: SpriteFileKind) -> &'static str {
    match kind {
        SpriteFileKind::Sheet => SHEET_CONTENT_TYPE,
        SpriteFileKind::Index => INDEX_CONTENT_TYPE,
    }
}

/// What an icon is drawn as on the sheet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IconShape {
    /// A filled disc inscribed in the icon's box: a place icon.
    Disc,
    /// A pin — a disc head over a stem down to the point — for a marker.
    Pin,
    /// A filled rectangle: a route shield.
    Square,
}

/// One icon of a sheet, measured in style pixels (`pixelRatio` 1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpriteIcon {
    pub name: &'static str,
    pub width: u32,
    pub height: u32,
    pub shape: IconShape,
    pub tint: Rgba,
}

/// Where one icon sits on a laid-out sheet, in the pixel ratio the layout was
/// made at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpriteSlot {
    pub name: &'static str,
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
    pub shape: IconShape,
    pub tint: Rgba,
}

/// A laid-out sheet: every icon's rectangle, the sheet's own size, and the
/// pixel ratio all of those are measured in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpriteLayout {
    pub slots: Vec<SpriteSlot>,
    pub width: u32,
    pub height: u32,
    pub pixel_ratio: u32,
}

/// The place icons every sheet carries, drawn as discs.
const PLACE_ICONS: [&str; 4] = ["airport", "hospital", "lodging", "restaurant"];

/// The marker icons every sheet carries, drawn as pins.
const MARKER_ICONS: [&str; 2] = ["marker", "marker-selected"];

/// The route shield icons every sheet carries, drawn as rectangles.
const SHIELD_ICONS: [&str; 2] = ["shield-1", "shield-2"];

/// The side of a place icon's box, in style pixels.
const PLACE_ICON_BOX: u32 = 15;

/// The width of a marker pin's box, in style pixels.
const MARKER_WIDTH: u32 = 18;

/// The height of a marker pin's box, in style pixels. Taller than it is wide:
/// a pin's point sits below its head.
const MARKER_HEIGHT: u32 = 24;

/// The width of a route shield's box, in style pixels.
const SHIELD_WIDTH: u32 = 20;

/// The height of a route shield's box, in style pixels.
const SHIELD_HEIGHT: u32 = 12;

/// How wide a sheet may grow, in style pixels, before an icon starts a new row.
const SHEET_MAX_WIDTH: u32 = 64;

/// The three tints one sheet draws its icon families in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Palette {
    place: Rgba,
    marker: Rgba,
    shield: Rgba,
}

/// The palette a map whose style is sourced from `data_source` draws its sheet
/// in, so two maps whose styles come from different providers really are served
/// different sheets rather than the same bytes.
///
/// `data_source` is the provider stored on the map resource when it was created
/// (one of the names [`super::MAP_STYLES`] carries); a value from outside that
/// catalog cannot reach here through a map resource, and takes the Open Data
/// palette rather than failing a read.
fn palette(data_source: &str) -> Palette {
    match data_source {
        super::PROVIDER_ESRI => Palette {
            place: [0x2d, 0x6b, 0xa3, 0xff],
            marker: [0xd9, 0x4a, 0x3d, 0xff],
            shield: [0x33, 0x44, 0x55, 0xff],
        },
        super::PROVIDER_HERE => Palette {
            place: [0x00, 0x87, 0x8f, 0xff],
            marker: [0xf2, 0x8c, 0x28, 0xff],
            shield: [0x1f, 0x3b, 0x4d, 0xff],
        },
        super::PROVIDER_GRAB => Palette {
            place: [0x00, 0x8a, 0x3c, 0xff],
            marker: [0x1a, 0x7a, 0x4c, 0xff],
            shield: [0x0d, 0x3b, 0x24, 0xff],
        },
        _ => Palette {
            place: [0x5b, 0x4b, 0x8a, 0xff],
            marker: [0xc0, 0x39, 0x7a, 0xff],
            shield: [0x2a, 0x2a, 0x36, 0xff],
        },
    }
}

/// The icon set a map whose style is sourced from `data_source` publishes,
/// ordered by icon name so the rectangle a client is handed for an icon does not
/// depend on the order the families are written in above.
pub fn sheet_icons(data_source: &str) -> Vec<SpriteIcon> {
    let palette = palette(data_source);
    let mut icons = Vec::with_capacity(PLACE_ICONS.len() + MARKER_ICONS.len() + SHIELD_ICONS.len());
    for name in PLACE_ICONS {
        icons.push(SpriteIcon {
            name,
            width: PLACE_ICON_BOX,
            height: PLACE_ICON_BOX,
            shape: IconShape::Disc,
            tint: palette.place,
        });
    }
    for name in MARKER_ICONS {
        icons.push(SpriteIcon {
            name,
            width: MARKER_WIDTH,
            height: MARKER_HEIGHT,
            shape: IconShape::Pin,
            tint: palette.marker,
        });
    }
    for name in SHIELD_ICONS {
        icons.push(SpriteIcon {
            name,
            width: SHIELD_WIDTH,
            height: SHIELD_HEIGHT,
            shape: IconShape::Square,
            tint: palette.shield,
        });
    }
    icons.sort_by(|left, right| left.name.cmp(right.name));
    icons
}

/// Lay a sheet's icons out at `pixel_ratio` device pixels per style pixel.
///
/// The icons are not all one size, so they are packed in rows rather than in a
/// grid of equal cells: each icon takes the next free horizontal run of its own
/// width, an icon that would cross the sheet's width bound starts the next row,
/// and a row is as tall as its tallest icon.
///
/// Every dimension scales with `pixel_ratio`, the width bound included, so the
/// `@2x` sheet is this layout at twice the size: an icon's rectangle there is
/// exactly twice its rectangle here, which is what makes one offsets document
/// describe the sheet it is paired with at either ratio.
pub fn layout_sheet(icons: &[SpriteIcon], pixel_ratio: u32) -> SpriteLayout {
    let ratio = pixel_ratio.max(1);
    let widest = icons.iter().map(|icon| icon.width).max().unwrap_or(0);
    // A sheet is never narrower than its widest icon: an icon that cannot fit
    // any row would never be placed.
    let bound = SHEET_MAX_WIDTH.max(widest) * ratio;
    let mut slots = Vec::with_capacity(icons.len());
    let mut x = 0;
    let mut y = 0;
    let mut row_height = 0;
    let mut width = 0;
    for icon in icons {
        let icon_width = (icon.width * ratio).max(1);
        let icon_height = (icon.height * ratio).max(1);
        if x > 0 && x + icon_width > bound {
            y += row_height;
            x = 0;
            row_height = 0;
        }
        slots.push(SpriteSlot {
            name: icon.name,
            x,
            y,
            width: icon_width,
            height: icon_height,
            shape: icon.shape,
            tint: icon.tint,
        });
        x += icon_width;
        row_height = row_height.max(icon_height);
        width = width.max(x);
    }
    SpriteLayout {
        slots,
        // A sheet with no icons is still a 1x1 image rather than a zero-sized
        // one, which no PNG decoder accepts.
        width: width.max(1),
        height: (y + row_height).max(1),
        pixel_ratio: ratio,
    }
}

/// The PNG bytes of a laid-out sheet: every icon drawn in its own rectangle, on
/// a transparent background.
pub fn render_sheet_png(layout: &SpriteLayout) -> Vec<u8> {
    let mut canvas = Canvas::new(layout.width, layout.height);
    for slot in &layout.slots {
        match slot.shape {
            IconShape::Square => canvas.fill_rect(slot.x, slot.y, slot.width, slot.height, slot.tint),
            IconShape::Disc => canvas.disc(
                slot.x + slot.width / 2,
                slot.y + slot.height / 2,
                slot.width.min(slot.height) / 2,
                slot.tint,
            ),
            IconShape::Pin => {
                // A pin: a head in the upper third, and a stem down the middle
                // to the point.
                canvas.disc(
                    slot.x + slot.width / 2,
                    slot.y + slot.height / 3,
                    slot.width / 3,
                    slot.tint,
                );
                let stem = (slot.width / 4).max(1);
                canvas.fill_rect(
                    slot.x + slot.width / 2 - stem / 2,
                    slot.y + slot.height / 3,
                    stem,
                    slot.height * 2 / 3,
                    slot.tint,
                );
            }
        }
    }
    canvas.to_png()
}

/// The sheet's offsets document: one entry per icon, naming the rectangle it
/// occupies in the paired PNG and the pixel ratio that rectangle is measured in
/// — the sprite-index format a map renderer reads.
pub fn sheet_index_json(layout: &SpriteLayout) -> Vec<u8> {
    let mut index = ::serde_json::Map::new();
    for slot in &layout.slots {
        index.insert(
            slot.name.to_string(),
            ::serde_json::json!({
                "x": slot.x,
                "y": slot.y,
                "width": slot.width,
                "height": slot.height,
                "pixelRatio": layout.pixel_ratio,
            }),
        );
    }
    // Serialization of a map of numbers cannot fail.
    ::serde_json::to_vec(&::serde_json::Value::Object(index)).unwrap_or_default()
}

/// The bytes one sprite `file` of the sheet a map sourced from `data_source`
/// publishes: the PNG sheet, or the offsets document describing exactly that
/// sheet.
pub fn sprite_file_bytes(data_source: &str, file: SpriteFile) -> Vec<u8> {
    let layout = layout_sheet(&sheet_icons(data_source), file.pixel_ratio);
    match file.kind {
        SpriteFileKind::Sheet => render_sheet_png(&layout),
        SpriteFileKind::Index => sheet_index_json(&layout),
    }
}

/// An RGBA raster a map asset is drawn into, and the PNG encoder for it.
///
/// Visible to the crate rather than to this module alone: a raster map tile is
/// drawn into the same primitive (see [`super::map_tiles`]), so there is one
/// RGBA/PNG surface per crate rather than one per asset.
pub(crate) struct Canvas {
    width: u32,
    height: u32,
    pixels: Vec<u8>,
}

impl Canvas {
    /// A fully transparent raster of `width` x `height`, each at least one pixel
    /// so the encoder always has an image to write.
    pub(crate) fn new(width: u32, height: u32) -> Self {
        let width = width.max(1);
        let height = height.max(1);
        Self {
            width,
            height,
            pixels: vec![0; width as usize * height as usize * 4],
        }
    }

    /// Paint one pixel, ignoring a coordinate outside the raster.
    pub(crate) fn set(&mut self, x: u32, y: u32, colour: Rgba) {
        if x >= self.width || y >= self.height {
            return;
        }
        let offset = (y as usize * self.width as usize + x as usize) * 4;
        self.pixels[offset..offset + 4].copy_from_slice(&colour);
    }

    /// Fill the rectangle whose top-left corner is `(x, y)`.
    fn fill_rect(&mut self, x: u32, y: u32, width: u32, height: u32, colour: Rgba) {
        for row in y..y.saturating_add(height) {
            for column in x..x.saturating_add(width) {
                self.set(column, row, colour);
            }
        }
    }

    /// Fill the disc of radius `radius` centred on `(cx, cy)`.
    fn disc(&mut self, cx: u32, cy: u32, radius: u32, colour: Rgba) {
        let radius = i64::from(radius);
        let cx = i64::from(cx);
        let cy = i64::from(cy);
        for row in (cy - radius)..=(cy + radius) {
            for column in (cx - radius)..=(cx + radius) {
                let dx = column - cx;
                let dy = row - cy;
                if dx * dx + dy * dy > radius * radius {
                    continue;
                }
                if let (Ok(column), Ok(row)) = (u32::try_from(column), u32::try_from(row)) {
                    self.set(column, row, colour);
                }
            }
        }
    }

    /// The raster as 8-bit RGBA PNG bytes.
    ///
    /// Infallible: the sink is a `Vec`, the dimensions are non-zero, and the
    /// buffer holds exactly four channels per pixel, so the encoder has nothing
    /// left to refuse. An encoder that still refused would answer with no image
    /// rather than a broken one.
    pub(crate) fn to_png(&self) -> Vec<u8> {
        let mut out = Vec::new();
        {
            let mut encoder = ::png::Encoder::new(&mut out, self.width, self.height);
            encoder.set_color(::png::ColorType::Rgba);
            encoder.set_depth(::png::BitDepth::Eight);
            match encoder
                .write_header()
                .and_then(|mut writer| writer.write_image_data(&self.pixels))
            {
                Ok(()) => {}
                Err(_) => return Vec::new(),
            }
        }
        out
    }
}
