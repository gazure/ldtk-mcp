//! Pure-Rust software rasterizer that renders an LDtk level to a PNG.
//!
//! Closes the perceive→act→verify loop: after editing, an agent can render the level and
//! actually see the result. Layers are drawn bottom-to-top — IntGrid cells as their value
//! colors, tile/auto layers as real pixels sampled from the decoded tileset images, and
//! entities as their tile sprite or a colored box. Tilesets that can't be decoded
//! (`.aseprite`, embedded) render as a magenta placeholder so a render never fails outright.

// The blit/draw helpers are inherently many-parameter (rect geometry + flip + opacity); a context
// struct would obscure more than it clarifies for this internal module.
#![allow(clippy::too_many_arguments)]

use std::{collections::HashMap, path::Path};

use anyhow::{anyhow, Result};
use serde_json::Value;

use crate::{
    fields::hex_to_int,
    project::{LevelRef, Project},
};

/// Magenta placeholder for tiles whose tileset image is unavailable.
const PLACEHOLDER: [u8; 4] = [0xFF, 0x00, 0xFF, 0xFF];
/// LDtk's default level background, used when a level has no `__bgColor`.
const DEFAULT_BG: [u8; 4] = [0x69, 0x6A, 0x79, 0xFF];

/// Options controlling render output size and layer selection.
pub struct RenderOpts {
    /// Explicit output scale (pixels out per source pixel). Overrides `max_px` when set.
    pub scale: Option<f64>,
    /// Cap for the longest output edge in pixels when `scale` is not given.
    pub max_px: i64,
    /// If set, only layers whose `__identifier` is listed are drawn.
    pub layers: Option<Vec<String>>,
}

impl Default for RenderOpts {
    fn default() -> Self {
        Self {
            scale: None,
            max_px: 1024,
            layers: None,
        }
    }
}

/// Result of a render: the encoded PNG plus metadata for the caller's text note.
pub struct RenderOutput {
    pub png: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub scale: f64,
    pub warnings: Vec<String>,
}

/// A decoded RGBA8 image (row-major, 4 bytes per pixel).
struct DecodedImage {
    w: u32,
    h: u32,
    rgba: Vec<u8>,
}

/// An RGBA8 framebuffer with simple alpha-compositing primitives.
struct Canvas {
    w: u32,
    h: u32,
    px: Vec<u8>,
}

impl Canvas {
    fn new(w: u32, h: u32, bg: [u8; 4]) -> Self {
        let mut px = vec![0u8; (w as usize) * (h as usize) * 4];
        for chunk in px.chunks_exact_mut(4) {
            chunk.copy_from_slice(&bg);
        }
        Self { w, h, px }
    }

    /// Alpha-composite `c` over the pixel at `(x, y)` (out-of-bounds is a no-op).
    fn blend(&mut self, x: i64, y: i64, c: [u8; 4]) {
        if x < 0 || y < 0 || x >= self.w as i64 || y >= self.h as i64 {
            return;
        }
        let a = c[3] as u32;
        if a == 0 {
            return;
        }
        let i = ((y as u32 * self.w + x as u32) * 4) as usize;
        if a == 255 {
            self.px[i..i + 4].copy_from_slice(&c);
            return;
        }
        let ia = 255 - a;
        for (k, &cc) in c.iter().enumerate().take(3) {
            self.px[i + k] = ((cc as u32 * a + self.px[i + k] as u32 * ia) / 255) as u8;
        }
        self.px[i + 3] = (a + self.px[i + 3] as u32 * ia / 255).min(255) as u8;
    }

    fn fill_rect(&mut self, x: i64, y: i64, w: i64, h: i64, c: [u8; 4]) {
        for yy in y..y + h {
            for xx in x..x + w {
                self.blend(xx, yy, c);
            }
        }
    }

    /// 1px rectangle outline.
    fn stroke_rect(&mut self, x: i64, y: i64, w: i64, h: i64, c: [u8; 4]) {
        if w <= 0 || h <= 0 {
            return;
        }
        for xx in x..x + w {
            self.blend(xx, y, c);
            self.blend(xx, y + h - 1, c);
        }
        for yy in y..y + h {
            self.blend(x, yy, c);
            self.blend(x + w - 1, yy, c);
        }
    }
}

/// Lazily decodes and caches tileset images by uid; `None` memoizes "unavailable".
struct TilesetCache<'a> {
    p: &'a Project,
    images: HashMap<i64, Option<DecodedImage>>,
    warnings: Vec<String>,
}

impl<'a> TilesetCache<'a> {
    fn new(p: &'a Project) -> Self {
        Self {
            p,
            images: HashMap::new(),
            warnings: Vec::new(),
        }
    }

    fn get(&mut self, uid: i64) -> Option<&DecodedImage> {
        if !self.images.contains_key(&uid) {
            let path = self.p.tileset_rel_path(uid).map(|rel| self.p.resolve_rel_path(&rel));
            let decoded = path.as_deref().and_then(|p| decode_png(p).ok());
            if decoded.is_none() {
                self.warnings
                    .push(format!("tileset {uid}: image unavailable; drawn as placeholder"));
            }
            self.images.insert(uid, decoded);
        }
        self.images.get(&uid).and_then(Option::as_ref)
    }
}

fn hex_rgba(hex: &str) -> [u8; 4] {
    match hex_to_int(hex) {
        Some(v) => [
            ((v >> 16) & 0xFF) as u8,
            ((v >> 8) & 0xFF) as u8,
            (v & 0xFF) as u8,
            0xFF,
        ],
        None => [0, 0, 0, 0xFF],
    }
}

/// Decode a PNG to RGBA8, normalizing palette/grayscale/RGB/16-bit sources.
fn decode_png(path: &Path) -> Result<DecodedImage> {
    let file = std::fs::File::open(path)?;
    let mut decoder = png::Decoder::new(file);
    decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = decoder.read_info()?;
    let mut buf = vec![0u8; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buf)?;
    buf.truncate(info.buffer_size());
    let (w, h) = (info.width, info.height);
    let rgba = match info.color_type {
        png::ColorType::Rgba => buf,
        png::ColorType::Rgb => widen(&buf, 3, false),
        png::ColorType::GrayscaleAlpha => widen(&buf, 2, true),
        png::ColorType::Grayscale => widen(&buf, 1, true),
        other => return Err(anyhow!("unsupported PNG color type {other:?}")),
    };
    Ok(DecodedImage { w, h, rgba })
}

/// Expand non-RGBA pixel data to RGBA8. `step` is source bytes/pixel; `gray` replicates the
/// single luminance channel across RGB.
fn widen(buf: &[u8], step: usize, gray: bool) -> Vec<u8> {
    let mut out = Vec::with_capacity(buf.len() / step * 4);
    for px in buf.chunks_exact(step) {
        if gray {
            let g = px[0];
            let a = if step == 2 { px[1] } else { 0xFF };
            out.extend_from_slice(&[g, g, g, a]);
        } else {
            out.extend_from_slice(&[px[0], px[1], px[2], 0xFF]);
        }
    }
    out
}

/// Nearest-neighbor scaled blit of a source rect into a dest rect, honoring flip bits
/// (`flip`: bit0=X, bit1=Y) and a multiplicative `opacity`.
#[allow(clippy::too_many_arguments)]
fn blit(
    dst: &mut Canvas,
    src: &DecodedImage,
    sx: i64,
    sy: i64,
    sw: i64,
    sh: i64,
    dx: i64,
    dy: i64,
    dw: i64,
    dh: i64,
    flip: i64,
    opacity: f64,
) {
    if sw <= 0 || sh <= 0 || dw <= 0 || dh <= 0 {
        return;
    }
    let flip_x = flip & 1 != 0;
    let flip_y = flip & 2 != 0;
    for oy in 0..dh {
        for ox in 0..dw {
            let mut u = ox * sw / dw;
            let mut v = oy * sh / dh;
            if flip_x {
                u = sw - 1 - u;
            }
            if flip_y {
                v = sh - 1 - v;
            }
            let (spx, spy) = (sx + u, sy + v);
            if spx < 0 || spy < 0 || spx >= src.w as i64 || spy >= src.h as i64 {
                continue;
            }
            let si = ((spy as u32 * src.w + spx as u32) * 4) as usize;
            let c = [
                src.rgba[si],
                src.rgba[si + 1],
                src.rgba[si + 2],
                (src.rgba[si + 3] as f64 * opacity) as u8,
            ];
            dst.blend(dx + ox, dy + oy, c);
        }
    }
}

fn arr_i64(v: &Value, key: &str, idx: usize) -> Option<i64> {
    v.get(key)
        .and_then(Value::as_array)
        .and_then(|a| a.get(idx))
        .and_then(Value::as_i64)
}

fn draw_intgrid(canvas: &mut Canvas, p: &Project, li: &Value, id: &str, grid: i64, ox: i64, oy: i64, opacity: f64) {
    let cw = li.get("__cWid").and_then(Value::as_i64).unwrap_or(0);
    let Some(csv) = li.get("intGridCsv").and_then(Value::as_array) else {
        return;
    };
    let mut color_of: HashMap<i64, [u8; 4]> = HashMap::new();
    for d in p.intgrid_value_defs(id) {
        if let Some(v) = d.get("value").and_then(Value::as_i64) {
            let c = d
                .get("color")
                .and_then(Value::as_str)
                .map(hex_rgba)
                .unwrap_or([0xFF, 0xFF, 0xFF, 0xFF]);
            color_of.insert(v, c);
        }
    }
    for (i, cell) in csv.iter().enumerate() {
        let v = cell.as_i64().unwrap_or(0);
        if v == 0 || cw == 0 {
            continue;
        }
        let cx = (i as i64) % cw;
        let cy = (i as i64) / cw;
        let mut c = *color_of.get(&v).unwrap_or(&[0xFF, 0xFF, 0xFF, 0xFF]);
        c[3] = (c[3] as f64 * opacity) as u8;
        canvas.fill_rect(cx * grid + ox, cy * grid + oy, grid, grid, c);
    }
}

fn draw_tiles(
    canvas: &mut Canvas,
    cache: &mut TilesetCache,
    p: &Project,
    li: &Value,
    grid: i64,
    ox: i64,
    oy: i64,
    opacity: f64,
) {
    let uid = li
        .get("overrideTilesetUid")
        .and_then(Value::as_i64)
        .or_else(|| li.get("__tilesetDefUid").and_then(Value::as_i64));
    // Source tile size comes from the tileset def; fall back to the layer grid size.
    let src_size = uid
        .and_then(|u| p.tileset_def(u))
        .map(|g| g.tile_grid_size)
        .unwrap_or(grid);
    for key in ["gridTiles", "autoLayerTiles"] {
        let Some(tiles) = li.get(key).and_then(Value::as_array) else {
            continue;
        };
        for t in tiles {
            let dx = arr_i64(t, "px", 0).unwrap_or(0) + ox;
            let dy = arr_i64(t, "px", 1).unwrap_or(0) + oy;
            let f = t.get("f").and_then(Value::as_i64).unwrap_or(0);
            let a = t.get("a").and_then(Value::as_f64).unwrap_or(1.0) * opacity;
            match uid.and_then(|u| cache.get(u)) {
                Some(img) => {
                    let sx = arr_i64(t, "src", 0).unwrap_or(0);
                    let sy = arr_i64(t, "src", 1).unwrap_or(0);
                    blit(canvas, img, sx, sy, src_size, src_size, dx, dy, grid, grid, f, a);
                }
                None => {
                    let mut c = PLACEHOLDER;
                    c[3] = (255.0 * a) as u8;
                    canvas.fill_rect(dx, dy, grid, grid, c);
                }
            }
        }
    }
}

fn draw_entities(canvas: &mut Canvas, cache: &mut TilesetCache, li: &Value, ox: i64, oy: i64, opacity: f64) {
    let Some(ents) = li.get("entityInstances").and_then(Value::as_array) else {
        return;
    };
    for e in ents {
        let x = arr_i64(e, "px", 0).unwrap_or(0) + ox;
        let y = arr_i64(e, "px", 1).unwrap_or(0) + oy;
        let w = e.get("width").and_then(Value::as_i64).unwrap_or(16);
        let h = e.get("height").and_then(Value::as_i64).unwrap_or(16);

        let tile = e.get("__tile").filter(|t| !t.is_null());
        if let Some(tile) = tile {
            let ts = tile.get("tilesetUid").and_then(Value::as_i64);
            if let Some(img) = ts.and_then(|u| cache.get(u)) {
                let sx = tile.get("x").and_then(Value::as_i64).unwrap_or(0);
                let sy = tile.get("y").and_then(Value::as_i64).unwrap_or(0);
                let sw = tile.get("w").and_then(Value::as_i64).unwrap_or(w);
                let sh = tile.get("h").and_then(Value::as_i64).unwrap_or(h);
                blit(canvas, img, sx, sy, sw, sh, x, y, w, h, 0, opacity);
                continue;
            }
        }
        // Fallback: translucent box in the entity's smart color with a solid outline.
        let color = e
            .get("__smartColor")
            .and_then(Value::as_str)
            .map(hex_rgba)
            .unwrap_or([0x94, 0xD9, 0xB3, 0xFF]);
        let mut fill = color;
        fill[3] = (90.0 * opacity) as u8;
        canvas.fill_rect(x, y, w, h, fill);
        let mut border = color;
        border[3] = (255.0 * opacity) as u8;
        canvas.stroke_rect(x, y, w, h, border);
    }
}

/// Compute output dimensions and scale: explicit `scale` wins; otherwise fit the longest edge
/// to `max_px` (integer upscaling for crisp pixels, fractional downscaling for oversized levels).
fn output_dims(w: u32, h: u32, opts: &RenderOpts) -> (u32, u32, f64) {
    let longest = w.max(h) as f64;
    let scale = match opts.scale {
        Some(s) if s > 0.0 => s,
        _ => {
            let max = opts.max_px.max(1) as f64;
            if longest <= max {
                (max / longest).floor().max(1.0)
            } else {
                max / longest
            }
        }
    };
    let ow = ((w as f64 * scale).round() as u32).max(1);
    let oh = ((h as f64 * scale).round() as u32).max(1);
    (ow, oh, scale)
}

fn resample(src: &Canvas, ow: u32, oh: u32) -> Canvas {
    let mut out = Canvas {
        w: ow,
        h: oh,
        px: vec![0u8; (ow as usize) * (oh as usize) * 4],
    };
    for oy in 0..oh {
        let sy = (oy as u64 * src.h as u64 / oh as u64) as u32;
        for ox in 0..ow {
            let sx = (ox as u64 * src.w as u64 / ow as u64) as u32;
            let si = ((sy * src.w + sx) * 4) as usize;
            let di = ((oy * ow + ox) * 4) as usize;
            out.px[di..di + 4].copy_from_slice(&src.px[si..si + 4]);
        }
    }
    out
}

fn encode_png(c: &Canvas) -> Result<Vec<u8>> {
    let mut buf = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut buf, c.w, c.h);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        let mut writer = enc.write_header()?;
        writer.write_image_data(&c.px)?;
    }
    Ok(buf)
}

/// Render the level at `r` to a PNG.
pub fn render(p: &Project, r: LevelRef, opts: &RenderOpts) -> Result<RenderOutput> {
    let level = p.level_ref(r).ok_or_else(|| anyhow!("level not found"))?;
    let px_wid = level.get("pxWid").and_then(Value::as_i64).unwrap_or(0).max(1);
    let px_hei = level.get("pxHei").and_then(Value::as_i64).unwrap_or(0).max(1);
    let bg = level
        .get("__bgColor")
        .and_then(Value::as_str)
        .map(hex_rgba)
        .unwrap_or(DEFAULT_BG);

    let mut canvas = Canvas::new(px_wid as u32, px_hei as u32, bg);
    let mut cache = TilesetCache::new(p);

    if let Some(layers) = level.get("layerInstances").and_then(Value::as_array) {
        // layerInstances are stored top-first; draw in reverse so upper layers composite last.
        for li in layers.iter().rev() {
            let id = li.get("__identifier").and_then(Value::as_str).unwrap_or("");
            if let Some(filter) = &opts.layers {
                if !filter.iter().any(|f| f == id) {
                    continue;
                }
            }
            if !li.get("visible").and_then(Value::as_bool).unwrap_or(true) {
                continue;
            }
            let kind = li.get("__type").and_then(Value::as_str).unwrap_or("");
            let opacity = li.get("__opacity").and_then(Value::as_f64).unwrap_or(1.0);
            let ox = li.get("__pxTotalOffsetX").and_then(Value::as_i64).unwrap_or(0);
            let oy = li.get("__pxTotalOffsetY").and_then(Value::as_i64).unwrap_or(0);
            let grid = li.get("__gridSize").and_then(Value::as_i64).unwrap_or(16).max(1);
            match kind {
                "IntGrid" => draw_intgrid(&mut canvas, p, li, id, grid, ox, oy, opacity),
                "Tiles" | "AutoLayer" => draw_tiles(&mut canvas, &mut cache, p, li, grid, ox, oy, opacity),
                "Entities" => draw_entities(&mut canvas, &mut cache, li, ox, oy, opacity),
                _ => {}
            }
        }
    }

    let (ow, oh, scale) = output_dims(canvas.w, canvas.h, opts);
    let final_canvas = if ow == canvas.w && oh == canvas.h {
        canvas
    } else {
        resample(&canvas, ow, oh)
    };
    let png = encode_png(&final_canvas)?;
    Ok(RenderOutput {
        png,
        width: ow,
        height: oh,
        scale,
        warnings: cache.warnings,
    })
}

/// Render a level addressed by identifier/iid/uid.
pub fn render_level(p: &Project, key: &str, opts: &RenderOpts) -> Result<RenderOutput> {
    let r = p.find_level(key).ok_or_else(|| anyhow!("level '{key}' not found"))?;
    render(p, r, opts)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn decode(bytes: &[u8]) -> (u32, u32, Vec<u8>) {
        let mut reader = png::Decoder::new(bytes).read_info().unwrap();
        let mut buf = vec![0u8; reader.output_buffer_size()];
        let info = reader.next_frame(&mut buf).unwrap();
        buf.truncate(info.buffer_size());
        assert_eq!(info.color_type, png::ColorType::Rgba);
        (info.width, info.height, buf)
    }

    fn px(buf: &[u8], w: u32, x: u32, y: u32) -> [u8; 4] {
        let i = ((y * w + x) * 4) as usize;
        [buf[i], buf[i + 1], buf[i + 2], buf[i + 3]]
    }

    #[test]
    fn hex_rgba_parses_and_defaults() {
        assert_eq!(hex_rgba("#FF8000"), [0xFF, 0x80, 0x00, 0xFF]);
        assert_eq!(hex_rgba("zzz"), [0, 0, 0, 0xFF]);
    }

    #[test]
    fn blit_nearest_and_flip() {
        // 2x2 source: TL=red, TR=green, BL=blue, BR=white.
        let src = DecodedImage {
            w: 2,
            h: 2,
            rgba: vec![
                255, 0, 0, 255, 0, 255, 0, 255, // row 0
                0, 0, 255, 255, 255, 255, 255, 255, // row 1
            ],
        };
        let mut c = Canvas::new(2, 2, [0, 0, 0, 255]);
        blit(&mut c, &src, 0, 0, 2, 2, 0, 0, 2, 2, 0, 1.0);
        assert_eq!(px(&c.px, 2, 0, 0), [255, 0, 0, 255], "no-flip TL");
        assert_eq!(px(&c.px, 2, 1, 0), [0, 255, 0, 255], "no-flip TR");

        // flip X: TL should now show the source's TR (green).
        let mut fx = Canvas::new(2, 2, [0, 0, 0, 255]);
        blit(&mut fx, &src, 0, 0, 2, 2, 0, 0, 2, 2, 1, 1.0);
        assert_eq!(px(&fx.px, 2, 0, 0), [0, 255, 0, 255], "flipX TL");

        // flip Y: TL should show the source's BL (blue).
        let mut fy = Canvas::new(2, 2, [0, 0, 0, 255]);
        blit(&mut fy, &src, 0, 0, 2, 2, 0, 0, 2, 2, 2, 1.0);
        assert_eq!(px(&fy.px, 2, 0, 0), [0, 0, 255, 255], "flipY TL");
    }

    #[test]
    fn blend_alpha_composites() {
        let mut c = Canvas::new(1, 1, [0, 0, 0, 255]);
        c.blend(0, 0, [255, 255, 255, 128]);
        let got = px(&c.px, 1, 0, 0);
        // ~50% white over black.
        assert!((120..=135).contains(&(got[0] as i32)), "{got:?}");
    }

    #[test]
    fn render_intgrid_produces_value_colors() {
        // 2x2 IntGrid, gridSize 16 -> 32x32 level. Cell (0,0)=1 (red), rest 0.
        let root = json!({
            "defs": { "layers": [
                { "identifier": "Walls", "__type": "IntGrid",
                  "intGridValues": [{ "value": 1, "identifier": "wall", "color": "#FF0000" }] }
            ] },
            "levels": [{
                "identifier": "L", "uid": 1, "pxWid": 32, "pxHei": 32, "__bgColor": "#000000",
                "layerInstances": [{
                    "__identifier": "Walls", "__type": "IntGrid", "__cWid": 2, "__cHei": 2,
                    "__gridSize": 16, "__opacity": 1.0, "visible": true,
                    "intGridCsv": [1, 0, 0, 0],
                }],
            }],
        });
        let p = Project::from_root_for_test(root);
        let out = render_level(
            &p,
            "L",
            &RenderOpts {
                scale: Some(1.0),
                max_px: 1024,
                layers: None,
            },
        )
        .unwrap();
        let (w, h, buf) = decode(&out.png);
        assert_eq!((w, h), (32, 32));
        assert_eq!(px(&buf, w, 0, 0), [255, 0, 0, 255], "wall cell red");
        assert_eq!(px(&buf, w, 20, 20), [0, 0, 0, 255], "empty cell bg");
        assert!(out.warnings.is_empty());
    }

    #[test]
    fn render_scales_up_to_max_px() {
        let root = json!({
            "defs": { "layers": [] },
            "levels": [{
                "identifier": "L", "uid": 1, "pxWid": 64, "pxHei": 32, "__bgColor": "#112233",
                "layerInstances": [],
            }],
        });
        let p = Project::from_root_for_test(root);
        // Longest edge 64, max_px 256 -> integer scale 4 -> 256x128.
        let out = render_level(
            &p,
            "L",
            &RenderOpts {
                scale: None,
                max_px: 256,
                layers: None,
            },
        )
        .unwrap();
        assert_eq!((out.width, out.height), (256, 128));
        assert_eq!(out.scale, 4.0);
        let (w, h, buf) = decode(&out.png);
        assert_eq!((w, h), (256, 128));
        assert_eq!(px(&buf, w, 10, 10), [0x11, 0x22, 0x33, 255]);
    }

    #[test]
    fn missing_tileset_yields_placeholder_and_warning() {
        let root = json!({
            "defs": { "layers": [], "tilesets": [
                { "uid": 9, "identifier": "Gone", "relPath": "does_not_exist.png",
                  "tileGridSize": 16, "__cWid": 4 }
            ] },
            "levels": [{
                "identifier": "L", "uid": 1, "pxWid": 16, "pxHei": 16, "__bgColor": "#000000",
                "layerInstances": [{
                    "__identifier": "Tiles", "__type": "Tiles", "__cWid": 1, "__cHei": 1,
                    "__gridSize": 16, "__opacity": 1.0, "visible": true, "__tilesetDefUid": 9,
                    "gridTiles": [{ "px": [0, 0], "src": [0, 0], "f": 0, "t": 0, "a": 1.0 }],
                }],
            }],
        });
        let p = Project::from_root_for_test(root);
        let out = render_level(
            &p,
            "L",
            &RenderOpts {
                scale: Some(1.0),
                max_px: 1024,
                layers: None,
            },
        )
        .unwrap();
        let (w, _h, buf) = decode(&out.png);
        assert_eq!(px(&buf, w, 0, 0), PLACEHOLDER, "placeholder magenta");
        assert_eq!(out.warnings.len(), 1, "{:?}", out.warnings);
    }
}
