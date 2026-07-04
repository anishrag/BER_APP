//! Paintbrush ANNOTATION over EXTERNAL photos (Front/Right/Left/Back): a
//! centred takeover window showing the stored photo large; dragging paints red
//! strokes over it; Save BURNS the strokes into the image (rasterised at image
//! resolution) and replaces the stored photo through the controller's normal
//! photo-store path (`AnnotateExternalPhoto`); Cancel discards.
//!
//! This is the ONE module holding the raster/composite code (it's view-coupled
//! — strokes are captured through the display transform — so it lives in
//! `src/ui` per the boundary rules): the [`PhotoAnnotator`] session state + its
//! [`render`] window, and the pure helpers [`burn_strokes`] / [`encode_jpeg`]
//! that the tests cover without egui.

use crate::ui::pages::events::UiEvent;
use crate::ui::pages::sketch::SketchEvent;
use ber_model::ber::common::ExternalSide;

/// Brush colour burned into the photo (and used for the live preview).
const BRUSH_RGB: [u8; 3] = [230, 30, 30];
/// On-screen brush width in points — "a few px", deliberately unscaled by any
/// zoom; the burn radius is this mapped through the display→image scale.
const BRUSH_SCREEN_PT: f32 = 4.0;
/// JPEG quality the annotated image is re-encoded at. The controller stores
/// the bytes verbatim (no second encode), so this is the stored quality.
const SAVE_JPEG_QUALITY: u8 = 85;

/// One open annotation session: the decoded stored image (the burn target),
/// its lazily-uploaded display texture, and the strokes painted so far (points
/// in IMAGE-pixel coordinates, so window resizes don't shift them).
pub struct PhotoAnnotator {
    side: ExternalSide,
    img: image::RgbImage,
    texture: Option<egui::TextureHandle>,
    strokes: Vec<Vec<(f32, f32)>>,
    /// Image pixels per screen point at the last drawn size — maps the screen
    /// brush width to the burn radius at save time.
    px_per_point: f32,
}

impl PhotoAnnotator {
    /// Open a session on `side`'s stored photo at `path`. `None` when the file
    /// can't be decoded (e.g. a desktop-picked format outside the app's
    /// jpeg/png decoders) — the caller then simply doesn't open the takeover.
    pub fn open(side: ExternalSide, path: &str) -> Option<Self> {
        let img = image::ImageReader::open(path)
            .and_then(|r| r.with_guessed_format())
            .ok()?
            .decode()
            .ok()?
            .to_rgb8();
        Some(Self {
            side,
            img,
            texture: None,
            strokes: Vec::new(),
            px_per_point: 1.0,
        })
    }
}

/// Draw the annotation takeover while a session is open (no-op otherwise).
/// Save emits [`SketchEvent::AnnotateExternalPhoto`] with the burned-in JPEG
/// bytes; Save and Cancel both close the session by clearing `slot`.
pub fn render<F: FnMut(UiEvent)>(
    ctx: &egui::Context,
    canvas_rect: egui::Rect,
    slot: &mut Option<PhotoAnnotator>,
    on_event: &mut F,
) {
    let Some(a) = slot.as_mut() else {
        return;
    };
    let mut close = false;
    egui::Area::new(egui::Id::new("photo_annotate_takeover"))
        .order(egui::Order::Foreground)
        .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
        .show(ctx, |ui| {
            egui::Frame::popup(ui.style()).show(ui, |ui| {
                ui.label(
                    egui::RichText::new(format!("Annotate — {} photo", a.side.label()))
                        .strong()
                        .size(15.0),
                );
                ui.label(
                    egui::RichText::new("drag to paint")
                        .size(12.0)
                        .weak()
                        .italics(),
                );

                // Fit the image into the canvas (leave room for the header +
                // button row), preserving aspect ratio.
                let (img_w, img_h) = (a.img.width() as f32, a.img.height() as f32);
                let avail =
                    (canvas_rect.size() - egui::vec2(48.0, 120.0)).max(egui::vec2(120.0, 120.0));
                let scale = (avail.x / img_w).min(avail.y / img_h);
                let disp = egui::vec2(img_w * scale, img_h * scale);
                a.px_per_point = img_w / disp.x;

                let (rect, resp) = ui.allocate_exact_size(disp, egui::Sense::drag());
                let texture = a.texture.get_or_insert_with(|| {
                    let color = egui::ColorImage::from_rgb(
                        [a.img.width() as usize, a.img.height() as usize],
                        a.img.as_raw(),
                    );
                    ctx.load_texture("photo_annotate", color, egui::TextureOptions::LINEAR)
                });
                let painter = ui.painter_at(rect);
                painter.image(
                    texture.id(),
                    rect,
                    egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                    egui::Color32::WHITE,
                );

                // Paint: strokes are captured in IMAGE coords immediately.
                let to_img = |pos: egui::Pos2| {
                    (
                        ((pos.x - rect.left()) / disp.x * img_w).clamp(0.0, img_w - 1.0),
                        ((pos.y - rect.top()) / disp.y * img_h).clamp(0.0, img_h - 1.0),
                    )
                };
                if resp.drag_started() {
                    a.strokes.push(Vec::new());
                }
                if resp.dragged() || resp.drag_stopped() {
                    if let (Some(pos), Some(stroke)) =
                        (resp.interact_pointer_pos(), a.strokes.last_mut())
                    {
                        stroke.push(to_img(pos));
                    }
                }

                // Live preview of the strokes, mapped image→screen.
                let to_screen = |&(x, y): &(f32, f32)| {
                    egui::pos2(
                        rect.left() + x / img_w * disp.x,
                        rect.top() + y / img_h * disp.y,
                    )
                };
                let color = egui::Color32::from_rgb(BRUSH_RGB[0], BRUSH_RGB[1], BRUSH_RGB[2]);
                for stroke in &a.strokes {
                    match stroke.as_slice() {
                        [] => {}
                        [p] => {
                            painter.circle_filled(to_screen(p), BRUSH_SCREEN_PT * 0.5, color);
                        }
                        pts => {
                            for w in pts.windows(2) {
                                painter.line_segment(
                                    [to_screen(&w[0]), to_screen(&w[1])],
                                    egui::Stroke::new(BRUSH_SCREEN_PT, color),
                                );
                            }
                        }
                    }
                }

                ui.separator();
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(!a.strokes.is_empty(), egui::Button::new("↩ Undo stroke"))
                        .clicked()
                    {
                        a.strokes.pop();
                    }
                    if ui
                        .add_enabled(!a.strokes.is_empty(), egui::Button::new("Clear"))
                        .clicked()
                    {
                        a.strokes.clear();
                    }
                    ui.separator();
                    if ui.button(egui::RichText::new("💾 Save").strong()).clicked() {
                        // Burn at image resolution: the screen brush width
                        // mapped through the display scale.
                        let radius = (BRUSH_SCREEN_PT * 0.5 * a.px_per_point).max(1.5);
                        let mut burned = a.img.clone();
                        burn_strokes(&mut burned, &a.strokes, radius, BRUSH_RGB);
                        if let Some(jpeg) = encode_jpeg(&burned, SAVE_JPEG_QUALITY) {
                            on_event(UiEvent::Sketch(SketchEvent::AnnotateExternalPhoto {
                                side: a.side,
                                jpeg,
                            }));
                        }
                        close = true;
                    }
                    if ui.button("Cancel").clicked() {
                        close = true;
                    }
                });
            });
        });
    if close {
        *slot = None;
    }
}

/// Rasterise stroke polylines onto `img` (points in image-pixel coords): each
/// segment is stamped with filled discs of `radius` px at sub-radius steps —
/// a solid round-capped line without an anti-aliasing dependency. Pure (no
/// egui), so the composite behaviour is unit-testable.
pub fn burn_strokes(
    img: &mut image::RgbImage,
    strokes: &[Vec<(f32, f32)>],
    radius: f32,
    rgb: [u8; 3],
) {
    let radius = radius.max(1.0);
    for stroke in strokes {
        match stroke.as_slice() {
            [] => {}
            [p] => stamp_disc(img, *p, radius, rgb),
            pts => {
                for w in pts.windows(2) {
                    stamp_segment(img, w[0], w[1], radius, rgb);
                }
            }
        }
    }
}

/// Stamp discs along the segment `a`→`b` at half-radius steps (dense enough
/// that consecutive discs overlap into a solid stroke).
fn stamp_segment(
    img: &mut image::RgbImage,
    a: (f32, f32),
    b: (f32, f32),
    radius: f32,
    rgb: [u8; 3],
) {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let len = (dx * dx + dy * dy).sqrt();
    let steps = (len / (radius * 0.5)).ceil().max(1.0) as u32;
    for i in 0..=steps {
        let t = i as f32 / steps as f32;
        stamp_disc(img, (a.0 + dx * t, a.1 + dy * t), radius, rgb);
    }
}

/// Fill the disc of `radius` px centred at `c` (clamped to the image bounds).
fn stamp_disc(img: &mut image::RgbImage, c: (f32, f32), radius: f32, rgb: [u8; 3]) {
    let (w, h) = img.dimensions();
    let x0 = (c.0 - radius).floor().max(0.0) as u32;
    let y0 = (c.1 - radius).floor().max(0.0) as u32;
    let x1 = ((c.0 + radius).ceil() as u32).min(w.saturating_sub(1));
    let y1 = ((c.1 + radius).ceil() as u32).min(h.saturating_sub(1));
    let r2 = radius * radius;
    for y in y0..=y1.min(h - 1) {
        for x in x0..=x1.min(w - 1) {
            let (fx, fy) = (x as f32 - c.0, y as f32 - c.1);
            if fx * fx + fy * fy <= r2 {
                img.put_pixel(x, y, image::Rgb(rgb));
            }
        }
    }
}

/// Encode `img` as a JPEG at `quality` (1..=100). `None` on encoder failure.
pub fn encode_jpeg(img: &image::RgbImage, quality: u8) -> Option<Vec<u8>> {
    use image::ImageEncoder;
    let mut buf = Vec::new();
    let encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut buf, quality);
    encoder
        .write_image(
            img.as_raw(),
            img.width(),
            img.height(),
            image::ExtendedColorType::Rgb8,
        )
        .ok()?;
    Some(buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Burning a stroke onto a known bitmap changes the pixels under the
    /// stroke, leaves pixels away from it untouched, preserves dimensions, and
    /// the re-encoded JPEG stays decodable at the same size.
    #[test]
    fn burn_strokes_composites_onto_bitmap() {
        let mut img = image::RgbImage::from_pixel(32, 32, image::Rgb([255, 255, 255]));
        burn_strokes(
            &mut img,
            &[vec![(4.0, 16.0), (28.0, 16.0)]],
            2.0,
            [230, 30, 30],
        );
        assert_eq!(img.dimensions(), (32, 32), "dimensions preserved");
        // A pixel on the stroke line is brush-coloured…
        assert_eq!(*img.get_pixel(16, 16), image::Rgb([230, 30, 30]));
        // …and one well away from it is untouched.
        assert_eq!(*img.get_pixel(2, 2), image::Rgb([255, 255, 255]));
        // The burned image round-trips through the save encoding.
        let jpeg = encode_jpeg(&img, 85).expect("encodes");
        let decoded = image::load_from_memory(&jpeg).expect("decodable jpeg");
        assert_eq!(decoded.width(), 32);
        assert_eq!(decoded.height(), 32);
        // The stroke survives the (lossy) round trip: still clearly red.
        let px = decoded.to_rgb8().get_pixel(16, 16).0;
        assert!(
            px[0] > 150 && px[1] < 120 && px[2] < 120,
            "stroke pixel stays red after jpeg round trip: {px:?}"
        );
    }

    /// A single-point stroke stamps a dot (the tap case).
    #[test]
    fn burn_strokes_single_point_stamps_dot() {
        let mut img = image::RgbImage::from_pixel(16, 16, image::Rgb([0, 0, 0]));
        burn_strokes(&mut img, &[vec![(8.0, 8.0)]], 1.5, [230, 30, 30]);
        assert_eq!(*img.get_pixel(8, 8), image::Rgb([230, 30, 30]));
        assert_eq!(*img.get_pixel(0, 0), image::Rgb([0, 0, 0]));
    }
}
