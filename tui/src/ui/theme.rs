//! Colours and glyphs.
//!
//! The Linux text console has 16 colours, only 8 of them as backgrounds,
//! and a 256-glyph font (Lat15-Fixed16 on the ThinkPad): the full block and
//! two shades, box drawing, arrows and bullets, but no eighth or half
//! blocks and no braille. Every other terminal gets the app's theme colours
//! and the finer blocks.

use ratatui::style::Color;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Colors {
    /// The 16 named colours, as the Linux console has them.
    Console,
    /// The xterm 256-colour palette, nearest match.
    Indexed,
    Rgb,
}

#[derive(Clone, Copy)]
pub struct Palette {
    /// Behind everything. None leaves the terminal's own background.
    pub plane: Option<Color>,
    pub surface: Option<Color>,
    pub ink: Color,
    pub ink2: Color,
    pub muted: Color,
    pub grid: Color,
    pub s1: Color,
    pub s2: Color,
    /// Background for a line drawn over series 1's filled area.
    pub s1_under: Color,
    pub good: Color,
    pub warning: Color,
    pub critical: Color,
}

pub struct Look {
    pub p: Palette,
    /// Only glyphs the console font has.
    pub console_glyphs: bool,
}

impl Look {
    pub fn new(theme: &str, colors: Colors, console_glyphs: bool) -> Look {
        Look { p: palette(theme, colors), console_glyphs }
    }

    /// Partial cells for a bar growing upwards, smallest first.
    pub fn rising(&self) -> &'static [&'static str] {
        if self.console_glyphs {
            &["░", "▒"]
        } else {
            &["▁", "▂", "▃", "▄", "▅", "▆", "▇"]
        }
    }

    /// Partial cells for a bar growing rightwards, smallest first.
    pub fn widening(&self) -> &'static [&'static str] {
        if self.console_glyphs {
            &[]
        } else {
            &["▏", "▎", "▍", "▌", "▋", "▊", "▉"]
        }
    }
}

// plane, surface, ink, ink-2, ink-muted, grid, series-1, series-2, good,
// warning, critical: the tokens of each theme in static/style.css. The glass
// themes' translucent surfaces are flattened onto their plane.
const THEMES: [(&str, [u32; 11]); 12] = [
    ("midnight", [0x0d0d0d, 0x1a1a19, 0xffffff, 0xc3c2b7, 0x898781, 0x2c2c2a, 0x3987e5, 0xd95926, 0x0ca30c, 0xfab219, 0xd03b3b]),
    ("ice", [0x04070d, 0x0a1018, 0xeaf4ff, 0xa8c2da, 0x6d8399, 0x14202c, 0x3987e5, 0x12a594, 0x12b981, 0xf5a524, 0xef4c4c]),
    ("neon", [0x07070b, 0x101018, 0xf2f5ff, 0xa9b2cc, 0x6f7794, 0x1d1d2b, 0x19a3c9, 0xcf3d80, 0x1fd6a2, 0xffcc33, 0xff456f]),
    ("amber", [0x060402, 0x100c06, 0xfdf3e0, 0xcbb894, 0x8b7c60, 0x2a2113, 0xbd8400, 0x0e9488, 0x12a06a, 0xe8a317, 0xe05a3c]),
    ("synthwave", [0x0b0518, 0x150a28, 0xf6eaff, 0xc4a9e0, 0x8b74a8, 0x241338, 0x8a5cf6, 0xe0457b, 0x28d6a4, 0xffc857, 0xff4f81]),
    ("matrix", [0x030503, 0x080d08, 0xdcffe4, 0x8fce9f, 0x5c8768, 0x12200f, 0x2ea043, 0x8a5cf6, 0x35d15a, 0xcbd422, 0xe0483f]),
    ("ember", [0x070302, 0x120806, 0xfff0e8, 0xd6ab99, 0x96705f, 0x241009, 0xd9531e, 0x2f9e8f, 0x2fae8f, 0xf0a73a, 0xf05a33]),
    ("nord", [0x242933, 0x2e3440, 0xeceff4, 0xd8dee9, 0x8f9bb0, 0x3b4252, 0x3987e5, 0xd95926, 0xa3be8c, 0xebcb8b, 0xbf616a]),
    ("glass", [0x050912, 0x0f1724, 0xeef5ff, 0xaebfd4, 0x74869c, 0x1b2534, 0x3987e5, 0x12a594, 0x19c58e, 0xf7b02c, 0xf25555]),
    ("vista", [0x03181a, 0x0a2628, 0xe8fbfa, 0xa2cfcd, 0x6b9694, 0x143234, 0x1b9da5, 0xc98500, 0x2fbf8a, 0xdfdd43, 0xe8624a]),
    ("win7", [0x04182f, 0x0b2440, 0xf0faff, 0xaed2ec, 0x7098b8, 0x16304a, 0x158ce7, 0xc98500, 0x1fbf90, 0xf2b32c, 0xef5a5a]),
    ("snowleopard", [0x040310, 0x141026, 0xf4f0fa, 0xc6bcd6, 0x8a7ea0, 0x221d33, 0x3987e5, 0xcf3d80, 0x2ec4a0, 0xe8b13a, 0xe8557f]),
];

fn palette(theme: &str, colors: Colors) -> Palette {
    if colors == Colors::Console {
        // Colour 3 is brown on the console, which is the orange of the
        // second series; blue 4 is dark enough to sit under a line.
        return Palette {
            plane: None,
            surface: None,
            ink: Color::White,
            ink2: Color::Gray,
            muted: Color::DarkGray,
            grid: Color::DarkGray,
            s1: Color::LightBlue,
            s2: Color::Yellow,
            s1_under: Color::Blue,
            good: Color::LightGreen,
            warning: Color::LightYellow,
            critical: Color::LightRed,
        };
    }
    // An unknown name falls back to ice, as the app does.
    let tokens = THEMES
        .iter()
        .find(|(name, _)| *name == theme)
        .unwrap_or(&THEMES[1])
        .1;
    let c = |rgb: u32| match colors {
        Colors::Indexed => Color::Indexed(xterm256(rgb)),
        _ => Color::Rgb((rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8),
    };
    Palette {
        plane: Some(c(tokens[0])),
        surface: Some(c(tokens[1])),
        ink: c(tokens[2]),
        ink2: c(tokens[3]),
        muted: c(tokens[4]),
        grid: c(tokens[5]),
        s1: c(tokens[6]),
        s2: c(tokens[7]),
        s1_under: c(tokens[6]),
        good: c(tokens[8]),
        warning: c(tokens[9]),
        critical: c(tokens[10]),
    }
}

/// Nearest colour in the xterm 256 palette: the 6x6x6 cube or the grey ramp.
fn xterm256(rgb: u32) -> u8 {
    let (r, g, b) = ((rgb >> 16) & 255, (rgb >> 8) & 255, rgb & 255);
    const STEPS: [u32; 6] = [0, 95, 135, 175, 215, 255];
    let level = |v: u32| if v < 48 { 0 } else if v < 115 { 1 } else { ((v - 35) / 40) as usize };
    let dist = |(x, y, z): (u32, u32, u32)| {
        let d = |a: u32, b: u32| (a as i64 - b as i64).pow(2);
        d(x, r) + d(y, g) + d(z, b)
    };
    let (ri, gi, bi) = (level(r), level(g), level(b));
    let cube = (STEPS[ri], STEPS[gi], STEPS[bi]);
    let grey_i = ((r + g + b) / 3).saturating_sub(8).min(238) / 10;
    let grey = 8 + grey_i * 10;
    if dist((grey, grey, grey)) < dist(cube) {
        232 + grey_i as u8
    } else {
        (16 + 36 * ri + 6 * gi + bi) as u8
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nearest_256() {
        assert_eq!(xterm256(0x000000), 16);
        assert_eq!(xterm256(0xffffff), 231);
        assert_eq!(xterm256(0x0d0d0d), 232);
        assert_eq!(xterm256(0xff0000), 196);
    }
}
