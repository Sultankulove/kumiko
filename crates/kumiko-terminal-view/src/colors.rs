//! Terminal colours: the 16 ANSI colours of the theme, the xterm 256-colour cube and grey
//! ramp, true colour as is.

use alacritty_terminal::vte::ansi::{Color, NamedColor, Rgb};

/// The theme's colours for a terminal.
#[derive(Debug, Clone, PartialEq)]
pub struct Palette {
    pub foreground: Rgb,
    pub background: Rgb,
    pub cursor: Rgb,
    pub selection: Rgb,
    /// Black, red, green, yellow, blue, magenta, cyan, white, then the bright ones.
    pub ansi: [Rgb; 16],
}

const fn rgb(hex: u32) -> Rgb {
    Rgb {
        r: (hex >> 16) as u8,
        g: (hex >> 8) as u8,
        b: hex as u8,
    }
}

impl Default for Palette {
    /// Close to IntelliJ's terminal in Darcula.
    fn default() -> Self {
        Self {
            foreground: rgb(0xbbbbbb),
            background: rgb(0x2b2b2b),
            cursor: rgb(0xbbbbbb),
            selection: rgb(0x214283),
            ansi: [
                rgb(0x000000),
                rgb(0xf0524f),
                rgb(0x5c962c),
                rgb(0xa68a0d),
                rgb(0x3993d4),
                rgb(0xa771bf),
                rgb(0x00a3a3),
                rgb(0x808080),
                rgb(0x595959),
                rgb(0xff4050),
                rgb(0x4fc414),
                rgb(0xe5bf00),
                rgb(0x1fb0ff),
                rgb(0xed7eed),
                rgb(0x00e5e5),
                rgb(0xffffff),
            ],
        }
    }
}

impl Palette {
    /// The colour of an indexed entry: 0–15 from the theme, then the 6×6×6 cube, then greys.
    pub fn indexed(&self, index: u8) -> Rgb {
        match index {
            0..=15 => self.ansi[index as usize],
            16..=231 => {
                let i = index - 16;
                let level = |v: u8| if v == 0 { 0 } else { 55 + v * 40 };
                Rgb {
                    r: level(i / 36),
                    g: level((i / 6) % 6),
                    b: level(i % 6),
                }
            }
            232..=255 => {
                let v = 8 + (index - 232) * 10;
                Rgb { r: v, g: v, b: v }
            }
        }
    }

    pub fn resolve(&self, color: Color) -> Rgb {
        match color {
            Color::Spec(rgb) => rgb,
            Color::Indexed(i) => self.indexed(i),
            Color::Named(named) => self.named(named),
        }
    }

    fn named(&self, named: NamedColor) -> Rgb {
        match named {
            NamedColor::Foreground | NamedColor::BrightForeground => self.foreground,
            NamedColor::Background => self.background,
            NamedColor::Cursor => self.cursor,
            NamedColor::DimForeground => dim(self.foreground),
            other => {
                let index = other as usize;
                if index < 16 {
                    self.ansi[index]
                } else if (NamedColor::DimBlack as usize..=NamedColor::DimWhite as usize)
                    .contains(&index)
                {
                    dim(self.ansi[index - NamedColor::DimBlack as usize])
                } else {
                    self.foreground
                }
            }
        }
    }
}

fn dim(c: Rgb) -> Rgb {
    Rgb {
        r: (c.r as u16 * 2 / 3) as u8,
        g: (c.g as u16 * 2 / 3) as u8,
        b: (c.b as u16 * 2 / 3) as u8,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn indexed_colours() {
        let p = Palette::default();
        assert_eq!(p.indexed(1), p.ansi[1]);
        assert_eq!(p.indexed(16), Rgb { r: 0, g: 0, b: 0 });
        assert_eq!(
            p.indexed(231),
            Rgb {
                r: 255,
                g: 255,
                b: 255
            }
        );
        assert_eq!(p.indexed(232), Rgb { r: 8, g: 8, b: 8 });
        assert_eq!(p.resolve(Color::Named(NamedColor::Red)), p.ansi[1]);
        assert_eq!(p.resolve(Color::Named(NamedColor::BrightBlue)), p.ansi[12]);
        assert_eq!(
            p.resolve(Color::Spec(Rgb { r: 1, g: 2, b: 3 })),
            Rgb { r: 1, g: 2, b: 3 }
        );
    }
}
