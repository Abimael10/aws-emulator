// SPDX-License-Identifier: CC0-1.0

//! What two characters leave between them when a PROPORTIONAL face sets them side by side,
//! and what a word space adds to that: the widths [`super::Separators`] measures a line's
//! own gaps against.
//!
//! A proportional face sets every character in the width of its own ink plus the SIDE
//! BEARINGS its letterform leaves inside the advance it is set on, so the gap two
//! characters leave when they stand side by side is a property of THOSE TWO characters: a
//! `T` and a `J` set together all but touch, because the crossbar of the one and the hook
//! of the other reach to the edges of their advances, while a `1` inks a third of the
//! tabular advance its face sets every figure on and leaves a gap inside its own cell as
//! wide as a space's. Measuring every gap of a line against ONE width therefore reads the
//! first pair's bearings as a space and the second pair's space as a bearing, whichever
//! width is chosen — so what a gap is measured against here is the pair of characters the
//! two marks were NAMED as, and a word space is the gap they leave PAST that.
//!
//! The bearings are the outline face's own, the proportional family of the recogniser's
//! bank ([`super::glyphs`]), measured once at a large raster and held in CAPITAL HEIGHTS —
//! the frame a line of ink is measured in ([`super::glyphs::Line`]) — so they describe a
//! page set at any size. Another sans-serif face leaves bearings a shade different from
//! these, which is why what a space has to cover is a part of the face's own space advance
//! rather than the whole of it ([`SPACE_SHARE`]), and why what the line's own gaps stand
//! wider than their letterforms by is read off the line itself ([`Letterforms`]).

use std::sync::OnceLock;

/// The raster the bearings are measured at, in pixels: large enough that a bearing of a
/// few hundredths of an em is several pixels of it, so the ratios held here are exact to
/// well under the pixel a page is read on.
const REFERENCE_RASTER: f32 = 96.0;

/// The first character the table holds: the printable ASCII range the bank names marks from.
const FIRST: u8 = 0x20;

/// One past the last character the table holds.
const LAST: u8 = 0x7f;

/// How many characters that is.
const SPAN: usize = (LAST - FIRST) as usize;

/// The side bearings of one character, in capital heights.
#[derive(Debug, Clone, Copy)]
struct Bearings {
    /// The blank its letterform leaves to the LEFT of its ink inside its own advance.
    left: f32,
    /// The blank it leaves to the RIGHT of its ink.
    right: f32,
}

/// How the reference face sets the printable ASCII characters: what each leaves either side
/// of its own ink, and the advance it sets a word space on.
struct Set {
    bearings: [Option<Bearings>; SPAN],
    space: f32,
}

/// The reference face's own setting, measured once.
///
/// A face this emulator cannot parse leaves the table empty, and a line is then separated by
/// its own gaps alone ([`super::Separators::between`]) — the reading a face with no metrics
/// to measure against has.
fn set() -> &'static Set {
    static SET: OnceLock<Set> = OnceLock::new();
    SET.get_or_init(|| {
        let mut bearings: [Option<Bearings>; SPAN] = [None; SPAN];
        let mut space = 0.0f32;
        let settings = fontdue::FontSettings {
            scale: REFERENCE_RASTER,
            ..fontdue::FontSettings::default()
        };
        if let Ok(font) = fontdue::Font::from_bytes(ttf_noto_sans::REGULAR, settings) {
            // The height of the face's own capitals, which every width here is held in.
            let cap = font.metrics('H', REFERENCE_RASTER).height as f32;
            if cap > 0.0 {
                for code in FIRST..LAST {
                    let metrics = font.metrics(char::from(code), REFERENCE_RASTER);
                    // A character this face draws no ink for — the space itself — has no
                    // bearings of its own: it is all advance.
                    if metrics.width == 0 {
                        continue;
                    }
                    let left = metrics.xmin as f32;
                    let right = metrics.advance_width - (left + metrics.width as f32);
                    bearings[usize::from(code - FIRST)] = Some(Bearings {
                        left: left / cap,
                        right: right / cap,
                    });
                }
                space = font.metrics(' ', REFERENCE_RASTER).advance_width / cap;
            }
        }
        Set { bearings, space }
    })
}

/// What the reference face leaves either side of `ch`'s own ink, in capital heights, or
/// `None` for a character it draws no ink for and for one outside printable ASCII.
fn bearings(ch: char) -> Option<Bearings> {
    let code = u8::try_from(u32::from(ch)).ok()?;
    if !(FIRST..LAST).contains(&code) {
        return None;
    }
    set().bearings[usize::from(code - FIRST)]
}

/// The advance a word SPACE is set on, in capital heights, or `None` where the reference
/// face could not be measured at all.
fn space() -> Option<f32> {
    let space = set().space;
    (space > 0.0).then_some(space)
}

/// How much of the face's own space advance a gap must cover PAST what the two characters
/// either side of it leave when they stand side by side for a word SPACE to be between
/// them.
///
/// Part of it rather than the whole: a page is set in whatever face it is set in, and
/// another sans-serif face's bearings and space advance stand a little away from the
/// reference face's, and the floor that tells ink from ground takes a column off the edge of
/// every stroke. So the boundary sits between the gap a pair leaves set together and the gap
/// it leaves with a space between them rather than at either end of that range.
const SPACE_SHARE: f32 = 0.45;

/// How much of a capital height a line's own marks may have lost off their letterforms'
/// ink, in capital heights: the anti-aliased edge of a stroke that fell under the floor
/// telling ink from ground, which widens EVERY gap of the line by the same column or two.
const MAX_INK_EROSION: f32 = 0.15;

/// Which of a line's own gaps measure what its marks lost off their letterforms: a low
/// quantile of them, since most neighbouring pairs of a line of text stand INSIDE a word.
const EROSION_QUANTILE: f32 = 0.25;

/// How much wider than what its two letterforms leave the gap between `before` and `after`
/// is, in pixels, where the two marks were named as `before_ch` and `after_ch` on a line
/// whose capitals are `cap` pixels tall.
///
/// This is what a word space measures: nothing, give or take the ink the line lost, for two
/// characters the face set side by side, and the space's own advance where one stands
/// between them.
fn excess(
    before: &super::GlyphBox,
    before_ch: char,
    after: &super::GlyphBox,
    after_ch: char,
    cap: f32,
) -> Option<f32> {
    let adjacent = adjacent(before_ch, after_ch)? * cap;
    let gap = after.x0.saturating_sub(before.x1 + 1) as f32;
    Some(gap - adjacent)
}

/// How the line whose marks were named as these characters separates its words: what each
/// of its gaps has to cover past the letterforms around it.
///
/// Measured off the line itself, like every other width here: what a line's marks lost off
/// their own ink is read off its own gaps, since the floor that tells ink from ground takes
/// the same column or two off every stroke of one page.
#[derive(Debug, Clone, Copy)]
pub(super) struct Letterforms {
    /// The height of this line's capitals, in pixels.
    cap: f32,
    /// What a word space adds to a gap of this line, in pixels.
    space: f32,
    /// What the gaps of this line stand wider than their letterforms by, in pixels.
    allowance: f32,
}

impl Letterforms {
    /// How the line these `marks` are of separates its words, or `None` where the reference
    /// face could not be measured at all.
    pub(super) fn of(marks: &[super::Settled], line: super::glyphs::Line) -> Option<Self> {
        let space = space()? * line.cap;
        let mut excesses: Vec<f32> = Vec::new();
        let mut previous: Option<(super::GlyphBox, char)> = None;
        for mark in marks {
            let Some(named) = mark.named else {
                continue;
            };
            if let Some((before, before_ch)) = previous {
                if let Some(excess) = excess(&before, before_ch, &mark.glyph, named.ch, line.cap)
                {
                    excesses.push(excess);
                }
            }
            previous = Some((mark.glyph, named.ch));
        }
        let allowance = super::quantile(&excesses, EROSION_QUANTILE)
            .unwrap_or(0.0)
            .clamp(0.0, MAX_INK_EROSION * line.cap);
        Some(Self {
            cap: line.cap,
            space,
            allowance,
        })
    }

    /// Whether a word space stands between `before` and `after`, the marks named as
    /// `before_ch` and `after_ch`: `None` where those two letterforms leave nothing to
    /// measure the gap against, so the line's own gaps have to settle it.
    pub(super) fn between(
        &self,
        before: &super::GlyphBox,
        before_ch: char,
        after: &super::GlyphBox,
        after_ch: char,
    ) -> Option<super::Separation> {
        let excess = excess(before, before_ch, after, after_ch, self.cap)?;
        Some(
            if excess >= self.allowance + SPACE_SHARE * self.space {
                super::Separation::Word
            } else {
                super::Separation::None
            },
        )
    }
}

/// The gap the two characters `before` and `after` leave between their ink when the face
/// sets them SIDE BY SIDE, in capital heights: the one's right bearing and the other's
/// left.
///
/// `None` where either character has no bearings of its own, which is where a line falls
/// back to measuring its gaps against each other.
fn adjacent(before: char, after: char) -> Option<f32> {
    Some(bearings(before)?.right + bearings(after)?.left)
}
