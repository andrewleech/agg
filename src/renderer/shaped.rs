use crate::renderer::{color_to_rgb, text_attrs, Renderer, Settings};
use crate::theme::Theme;
use imgref::ImgVec;
use log::debug;
use rgb::{FromSlice, RGBA8};
use std::collections::HashMap;

type CharVariant = (char, bool, bool);
type FontKey = (String, bool, bool);

enum GlyphEntry {
    /// Vector outline glyph -- rendered via fill_path with transform.
    Path {
        path: tiny_skia::Path,
        units_per_em: u16,
    },
    /// Pre-rendered color glyph (bitmap emoji, CBDT/sbix PNG bitmaps).
    Bitmap {
        pixmap: tiny_skia::Pixmap,
        x: i16,
        y: i16,
        pixels_per_em: u16,
    },
}

#[derive(Clone)]
struct FontInfo {
    face_id: fontdb::ID,
    face_index: u32,
    units_per_em: u16,
}

struct GlyphPathBuilder(tiny_skia::PathBuilder);

impl ttf_parser::OutlineBuilder for GlyphPathBuilder {
    fn move_to(&mut self, x: f32, y: f32) {
        self.0.move_to(x, y);
    }

    fn line_to(&mut self, x: f32, y: f32) {
        self.0.line_to(x, y);
    }

    fn quad_to(&mut self, x1: f32, y1: f32, x: f32, y: f32) {
        self.0.quad_to(x1, y1, x, y);
    }

    fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32) {
        self.0.cubic_to(x1, y1, x2, y2, x, y);
    }

    fn close(&mut self) {
        self.0.close();
    }
}

pub struct ShapedRenderer {
    font_families: Vec<String>,
    theme: Theme,
    pixel_width: usize,
    pixel_height: usize,
    font_size: f64,
    col_width: f64,
    row_height: f64,
    font_db: fontdb::Database,
    font_cache: HashMap<FontKey, Option<FontInfo>>,
    glyph_cache: HashMap<CharVariant, Option<GlyphEntry>>,
    pixmap: tiny_skia::Pixmap,
    scratch_pixmap: Option<tiny_skia::Pixmap>,
}

impl ShapedRenderer {
    pub fn new(settings: Settings) -> Self {
        let font_size = settings.font_size as f64;

        // Measure col_width by shaping '/' with the default font face.
        let col_width = {
            let mut width = font_size * 0.5; // fallback
            if let Some((face_id, face_index)) = Self::query_font_db(
                &settings.font_db,
                &settings.font_families,
                false,
                false,
            ) {
                if let Some(w) = Self::measure_advance(
                    &settings.font_db,
                    face_id,
                    face_index,
                    '/',
                    font_size,
                ) {
                    width = w;
                }
            }
            width
        };

        let row_height = font_size * settings.line_height;
        let (cols, rows) = settings.terminal_size;
        let pixel_width = ((cols + 2) as f64 * col_width).round() as usize;
        let pixel_height = ((rows + 1) as f64 * row_height).round() as usize;

        let pixmap = tiny_skia::Pixmap::new(pixel_width as u32, pixel_height as u32).unwrap();

        Self {
            font_db: settings.font_db,
            font_families: settings.font_families,
            theme: settings.theme,
            pixel_width,
            pixel_height,
            font_size,
            col_width,
            row_height,
            font_cache: HashMap::new(),
            glyph_cache: HashMap::new(),
            pixmap,
            scratch_pixmap: None,
        }
    }

    /// Query fontdb for a single face matching the given families/style.
    /// Returns (face_id, face_index).
    fn query_font_db(
        db: &fontdb::Database,
        families: &[String],
        bold: bool,
        italic: bool,
    ) -> Option<(fontdb::ID, u32)> {
        debug!(
            "looking up font for families={:?}, bold={}, italic={}",
            families, bold, italic
        );

        let weight = if bold {
            fontdb::Weight::BOLD
        } else {
            fontdb::Weight::NORMAL
        };
        let style = if italic {
            fontdb::Style::Italic
        } else {
            fontdb::Style::Normal
        };

        let families_db: Vec<fontdb::Family> = families
            .iter()
            .map(|n| fontdb::Family::Name(n.as_str()))
            .collect();

        let query = fontdb::Query {
            families: &families_db,
            weight,
            stretch: fontdb::Stretch::Normal,
            style,
        };

        db.query(&query).map(|face_id| {
            let face_index = db.face(face_id).unwrap().index;
            (face_id, face_index)
        })
    }

    /// Measure the horizontal advance of a character in pixels using rustybuzz shaping.
    fn measure_advance(
        db: &fontdb::Database,
        face_id: fontdb::ID,
        face_index: u32,
        ch: char,
        font_size: f64,
    ) -> Option<f64> {
        db.with_face_data(face_id, |font_data, _| {
            let rb_face = rustybuzz::Face::from_slice(font_data, face_index)?;
            let units_per_em = rb_face.units_per_em();

            let mut buf = rustybuzz::UnicodeBuffer::new();
            buf.push_str(&ch.to_string());
            let output = rustybuzz::shape(&rb_face, &[], buf);

            let infos = output.glyph_infos();
            let positions = output.glyph_positions();
            if infos.is_empty() {
                return None;
            }

            let x_advance = positions[0].x_advance as f64;
            Some(x_advance * font_size / units_per_em as f64)
        })
        .flatten()
    }

    /// Look up or populate the font cache entry for (family, bold, italic).
    fn get_font_info(&mut self, name: &str, bold: bool, italic: bool) -> Option<FontInfo> {
        let key = (name.to_owned(), bold, italic);

        if let Some(cached) = self.font_cache.get(&key) {
            return cached.clone();
        }

        let info = Self::query_font_db(&self.font_db, &[name.to_owned()], bold, italic)
            .and_then(|(face_id, face_index)| {
                // Get units_per_em via ttf_parser which returns u16 directly.
                let units_per_em = self.font_db.with_face_data(face_id, |font_data, _| {
                    let face = ttf_parser::Face::parse(font_data, face_index).ok()?;
                    Some(face.units_per_em())
                })??;

                Some(FontInfo {
                    face_id,
                    face_index,
                    units_per_em,
                })
            });

        self.font_cache.insert(key, info.clone());
        info
    }

    fn ensure_glyph(&mut self, ch: char, bold: bool, italic: bool) {
        let key = (ch, bold, italic);

        if self.glyph_cache.contains_key(&key) {
            return;
        }

        // Try requested style across all families.
        if let Some(entry) = self.resolve_glyph(ch, bold, italic) {
            self.glyph_cache.insert(key, Some(entry));
            return;
        }

        // Fall back to normal style if a styled variant was requested.
        if bold || italic {
            if let Some(entry) = self.resolve_glyph(ch, false, false) {
                self.glyph_cache.insert(key, Some(entry));
                return;
            }
        }

        self.glyph_cache.insert(key, None);
    }

    /// Try each font family in order, returning the first GlyphEntry found.
    fn resolve_glyph(&mut self, ch: char, bold: bool, italic: bool) -> Option<GlyphEntry> {
        for i in 0..self.font_families.len() {
            let family = self.font_families[i].clone();
            if let Some(entry) = self.try_family_glyph(&family, ch, bold, italic) {
                return Some(entry);
            }
        }
        None
    }

    /// Attempt to produce a GlyphEntry for `ch` from one specific font family.
    fn try_family_glyph(
        &mut self,
        family: &str,
        ch: char,
        bold: bool,
        italic: bool,
    ) -> Option<GlyphEntry> {
        let info = self.get_font_info(family, bold, italic)?;

        // Shape the character to get the correct glyph ID (handles GSUB substitutions).
        let glyph_id_u32 = self.font_db.with_face_data(info.face_id, |font_data, _| {
            let rb_face = rustybuzz::Face::from_slice(font_data, info.face_index)?;
            let mut buf = rustybuzz::UnicodeBuffer::new();
            buf.push_str(&ch.to_string());
            let output = rustybuzz::shape(&rb_face, &[], buf);
            let infos = output.glyph_infos();
            if infos.is_empty() || infos[0].glyph_id == 0 {
                return None;
            }
            Some(infos[0].glyph_id)
        })??;

        let glyph_id = match u16::try_from(glyph_id_u32) {
            Ok(id) => ttf_parser::GlyphId(id),
            Err(_) => {
                debug!("glyph ID {} for '{}' exceeds u16 range, skipping", glyph_id_u32, ch);
                return None;
            }
        };

        // Try outline glyph first, then bitmap.
        if let Some(entry) = self.try_outline(&info, glyph_id) {
            return Some(entry);
        }
        if let Some(entry) = self.try_bitmap(&info, glyph_id) {
            return Some(entry);
        }

        None
    }

    fn try_outline(&self, info: &FontInfo, glyph_id: ttf_parser::GlyphId) -> Option<GlyphEntry> {
        self.font_db
            .with_face_data(info.face_id, |font_data, _| {
                let face = ttf_parser::Face::parse(font_data, info.face_index).ok()?;
                let mut builder = GlyphPathBuilder(tiny_skia::PathBuilder::new());
                face.outline_glyph(glyph_id, &mut builder)?;
                let path = builder.0.finish()?;
                Some(GlyphEntry::Path {
                    path,
                    units_per_em: info.units_per_em,
                })
            })
            .flatten()
    }

    fn try_bitmap(&self, info: &FontInfo, glyph_id: ttf_parser::GlyphId) -> Option<GlyphEntry> {
        // Use font_size rounded to u16 as the target pixels_per_em for strike selection.
        let pixels_per_em = self.font_size.round() as u16;

        self.font_db
            .with_face_data(info.face_id, |font_data, _| {
                let face = ttf_parser::Face::parse(font_data, info.face_index).ok()?;
                let img = face.glyph_raster_image(glyph_id, pixels_per_em)?;

                // Only handle PNG format; skip mono/grey bitmaps.
                if img.format != ttf_parser::RasterImageFormat::PNG {
                    return None;
                }

                let pixmap = tiny_skia::Pixmap::decode_png(img.data).ok()?;
                Some(GlyphEntry::Bitmap {
                    pixmap,
                    x: img.x,
                    y: img.y,
                    pixels_per_em: img.pixels_per_em,
                })
            })
            .flatten()
    }
}

impl Renderer for ShapedRenderer {
    fn render(&mut self, lines: Vec<avt::Line>, cursor: Option<(usize, usize)>) -> ImgVec<RGBA8> {
        let bg = self.theme.background.alpha(255);
        self.pixmap
            .fill(tiny_skia::Color::from_rgba8(bg.r, bg.g, bg.b, bg.a));

        let margin_l = self.col_width;
        let margin_t = (self.row_height / 2.0).round();

        for (row, line) in lines.iter().enumerate() {
            let y_t = margin_t + row as f64 * self.row_height;
            let y_b = margin_t + (row + 1) as f64 * self.row_height;
            let mut col = 0usize;

            for cell in line.cells() {
                let ch = cell.char();
                let x_l = margin_l + col as f64 * self.col_width;
                let x_r = margin_l + (col + cell.width()) as f64 * self.col_width;
                let attrs = text_attrs(cell.pen(), &cursor, col, row, &self.theme);

                // Background rect.
                if let Some(c) = attrs.background {
                    let c = color_to_rgb(&c, &self.theme);
                    let bg_color = tiny_skia::Color::from_rgba8(c.r, c.g, c.b, 255);
                    if let Some(rect) = tiny_skia::Rect::from_ltrb(
                        x_l as f32,
                        y_t as f32,
                        x_r as f32,
                        y_b as f32,
                    ) {
                        let paint = tiny_skia::Paint {
                            shader: tiny_skia::Shader::SolidColor(bg_color),
                            anti_alias: false,
                            ..Default::default()
                        };
                        self.pixmap.fill_rect(
                            rect,
                            &paint,
                            tiny_skia::Transform::identity(),
                            None,
                        );
                    }
                }

                let fg = color_to_rgb(
                    &attrs
                        .foreground
                        .unwrap_or(avt::Color::RGB(self.theme.foreground)),
                    &self.theme,
                )
                .alpha(255);

                // Underline: 1 px at baseline + small offset.
                if attrs.underline {
                    let ul_y = margin_t + row as f64 * self.row_height + self.font_size * 1.2;
                    if let Some(rect) = tiny_skia::Rect::from_ltrb(
                        x_l as f32,
                        ul_y as f32,
                        x_r as f32,
                        (ul_y + 1.0) as f32,
                    ) {
                        let color = tiny_skia::Color::from_rgba8(fg.r, fg.g, fg.b, fg.a);
                        let paint = tiny_skia::Paint {
                            shader: tiny_skia::Shader::SolidColor(color),
                            anti_alias: false,
                            ..Default::default()
                        };
                        self.pixmap.fill_rect(
                            rect,
                            &paint,
                            tiny_skia::Transform::identity(),
                            None,
                        );
                    }
                }

                if ch == ' ' {
                    col += cell.width();
                    continue;
                }

                self.ensure_glyph(ch, attrs.bold, attrs.italic);

                // Borrow glyph_cache immutably; clone Path to avoid conflicting borrows with
                // &mut self.pixmap. tiny_skia::Path clone is cheap (typically ~50 points).
                let glyph = match self.glyph_cache.get(&(ch, attrs.bold, attrs.italic)) {
                    Some(Some(GlyphEntry::Path { path, units_per_em })) => {
                        Some(GlyphEntry::Path {
                            path: path.clone(),
                            units_per_em: *units_per_em,
                        })
                    }
                    Some(Some(GlyphEntry::Bitmap { pixmap, x, y, pixels_per_em })) => {
                        let bw = pixmap.width();
                        let bh = pixmap.height();
                        // Scale from bitmap's ppem to our font size.
                        let scale = self.font_size as f32 / *pixels_per_em as f32;
                        let scaled_w = ((bw as f32) * scale).round() as u32;
                        let scaled_h = ((bh as f32) * scale).round() as u32;
                        // Position using the bitmap's x/y offsets, scaled to our size.
                        let offset_x = (*x as f32 * scale).round() as i32;
                        let offset_y = (*y as f32 * scale).round() as i32;
                        let pos_x = x_l as i32 + offset_x;
                        // y offset from ttf-parser is Y-up, negate for screen coords.
                        let pos_y = y_t as i32 - offset_y;
                        let opacity = if attrs.faint { 0.5 } else { 1.0 };

                        // Reuse or allocate scratch pixmap for scaling.
                        let need_w = scaled_w.max(1);
                        let need_h = scaled_h.max(1);
                        let needs_realloc = match &self.scratch_pixmap {
                            Some(sp) => sp.width() < need_w || sp.height() < need_h,
                            None => true,
                        };
                        if needs_realloc {
                            self.scratch_pixmap = tiny_skia::Pixmap::new(need_w, need_h);
                        }
                        if let Some(ref mut scratch) = self.scratch_pixmap {
                            scratch.fill(tiny_skia::Color::TRANSPARENT);
                            scratch.draw_pixmap(
                                0,
                                0,
                                pixmap.as_ref(),
                                &tiny_skia::PixmapPaint {
                                    opacity: 1.0, // don't apply opacity here
                                    blend_mode: tiny_skia::BlendMode::SourceOver,
                                    quality: tiny_skia::FilterQuality::Bilinear,
                                },
                                tiny_skia::Transform::from_scale(scale, scale),
                                None,
                            );

                            self.pixmap.draw_pixmap(
                                pos_x,
                                pos_y,
                                scratch.as_ref(),
                                &tiny_skia::PixmapPaint {
                                    opacity, // apply opacity only on final blit
                                    blend_mode: tiny_skia::BlendMode::SourceOver,
                                    quality: tiny_skia::FilterQuality::Nearest,
                                },
                                tiny_skia::Transform::identity(),
                                None,
                            );
                        }

                        col += cell.width();
                        continue;
                    }
                    _ => None,
                };

                if let Some(GlyphEntry::Path { path, units_per_em }) = glyph {
                    let scale = self.font_size / units_per_em as f64;
                    let baseline_x = x_l as f32;
                    // Baseline sits one em below the top of the cell. This leaves
                    // (row_height - font_size) pixels for descenders below the baseline.
                    let baseline_y = (y_t + self.font_size).round() as f32;

                    let transform =
                        tiny_skia::Transform::from_scale(scale as f32, -(scale as f32))
                            .post_translate(baseline_x, baseline_y);

                    let alpha = if attrs.faint { 128u8 } else { 255u8 };
                    let color = tiny_skia::Color::from_rgba8(fg.r, fg.g, fg.b, alpha);
                    let paint = tiny_skia::Paint {
                        shader: tiny_skia::Shader::SolidColor(color),
                        anti_alias: true,
                        ..Default::default()
                    };

                    self.pixmap.fill_path(
                        &path,
                        &paint,
                        tiny_skia::FillRule::Winding,
                        transform,
                        None,
                    );
                }

                col += cell.width();
            }
        }

        let data = self.pixmap.data().as_rgba().to_vec();
        ImgVec::new(data, self.pixel_width, self.pixel_height)
    }

    fn pixel_size(&self) -> (usize, usize) {
        (self.pixel_width, self.pixel_height)
    }
}
