//! The canvas the scene effects draw on: sprites placed by cell, shaded
//! discs and rings, and the frame they add up to.

use super::*;

/// One frame of a scene effect: the sprites to paint, by cell, and an
/// optional glow.
pub(super) struct SceneFrame {
    pub(super) sprites: std::collections::HashMap<(u16, u16), SpaceSprite>,
    /// A light in the scene that warms the code near it: where, and how
    /// far it reaches.
    pub(super) glow: Option<(f32, f32, f32)>,
}

#[derive(Clone, Copy)]
pub(super) struct SpaceSprite {
    pub(super) glyph: &'static str,
    pub(super) colour: Color,
    pub(super) bold: bool,
}

/// A body in the sky: an ellipse of cells, lit from the sun's side.
pub(super) struct SpaceBody {
    pub(super) x: f32,
    pub(super) y: f32,
    /// Half-width in cells and half-height in rows.
    pub(super) rx: f32,
    pub(super) ry: f32,
    pub(super) colour: Color,
    /// Alternate rows a shade darker: a gas giant's bands.
    pub(super) banded: bool,
    /// Drawn bright, the way the near side of an orbit is.
    pub(super) near: bool,
}

/// The sky being drawn: where each cell's sprite goes, and the palette the
/// shading works in.
pub(super) struct SceneCanvas {
    pub(super) area: Rect,
    /// Where the light comes from, for shading a body.
    pub(super) light: (f32, f32),
    pub(super) background: Color,
    pub(super) foreground: Color,
    pub(super) sprites: std::collections::HashMap<(u16, u16), SpaceSprite>,
}

impl SceneCanvas {
    pub(super) fn put(&mut self, x: f32, y: f32, sprite: SpaceSprite, over: bool) {
        let (cx, cy) = (x.round(), y.round());
        if cx < 0.0
            || cy < 0.0
            || cx >= f32::from(self.area.width)
            || cy >= f32::from(self.area.height)
        {
            return;
        }
        let key = (self.area.x + cx as u16, self.area.y + cy as u16);
        if over {
            self.sprites.insert(key, sprite);
        } else {
            self.sprites.entry(key).or_insert(sprite);
        }
    }

    /// A shaded disc: the side facing the sun in solid block, the far side
    /// dithered down to the background, so a flat ellipse of cells reads as
    /// a sphere with a light on it.
    pub(super) fn disc(&mut self, body: &SpaceBody, over: bool) {
        let (lx, ly) = (self.light.0 - body.x, self.light.1 - body.y);
        // Light comes from the sun, and a little from the viewer, so the
        // near side of a body is never wholly dark.
        let length = (lx * lx * 0.25 + ly * ly).sqrt().max(0.001);
        let (lx, ly, lz) = (lx * 0.5 / length, ly / length, 0.55);
        let (x0, x1) = ((body.x - body.rx).floor(), (body.x + body.rx).ceil());
        let (y0, y1) = ((body.y - body.ry).floor(), (body.y + body.ry).ceil());
        let mut y = y0;
        while y <= y1 {
            let mut x = x0;
            while x <= x1 {
                let (nx, ny) = ((x - body.x) / body.rx, (y - body.y) / body.ry);
                let inside = 1.0 - nx * nx - ny * ny;
                if inside >= 0.0 {
                    let nz = inside.sqrt();
                    let mut light = (nx * lx + ny * ly + nz * lz).clamp(0.0, 1.0);
                    if !body.near {
                        light *= 0.55;
                    }
                    let mut colour = body.colour;
                    if body.banded && (y as i32).rem_euclid(2) == 0 {
                        colour = blend_rgb(colour, self.background, 0.35);
                    }
                    let (glyph, colour) = if light > 0.72 {
                        ("█", blend_rgb(colour, self.foreground, 0.3))
                    } else if light > 0.48 {
                        ("█", colour)
                    } else if light > 0.3 {
                        ("▓", colour)
                    } else if light > 0.15 {
                        ("▒", blend_rgb(colour, self.background, 0.25))
                    } else {
                        ("░", blend_rgb(colour, self.background, 0.45))
                    };
                    self.put(
                        x,
                        y,
                        SpaceSprite {
                            glyph,
                            colour,
                            bold: body.near && light > 0.48,
                        },
                        over,
                    );
                }
                x += 1.0;
            }
            y += 1.0;
        }
    }

    /// A ring around a body: the half behind it under it, the half in front
    /// over it.
    pub(super) fn ring(&mut self, body: &SpaceBody, front: bool, colour: Color) {
        use std::f32::consts::TAU;
        let (rx, ry) = (body.rx * 1.9, body.ry * 0.55);
        let ring = SpaceSprite {
            glyph: "─",
            colour,
            bold: false,
        };
        // Sampled densely enough that rounding to cells leaves no gaps.
        let dots = ((rx * 5.0) as usize).max(24);
        for dot in 0..dots {
            let a = dot as f32 / dots as f32 * TAU;
            if (a.sin() >= 0.0) == front {
                self.put(body.x + rx * a.cos(), body.y + ry * a.sin(), ring, true);
            }
        }
    }
}
