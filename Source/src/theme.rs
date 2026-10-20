//! Light / dark theme. All colours of the UI are written for the light theme and pass through
//! `t()`; in dark mode it inverts their lightness while keeping hue and saturation, so white
//! backgrounds become near-black, dark text becomes light and the syntax colours stay readable.

use egui::Color32;
use std::sync::atomic::{AtomicBool, Ordering};

static DARK: AtomicBool = AtomicBool::new(false);

pub fn set_dark(on: bool) {
    DARK.store(on, Ordering::Relaxed);
}

pub fn is_dark() -> bool {
    DARK.load(Ordering::Relaxed)
}

/// Colour for the current theme.
pub fn t(c: Color32) -> Color32 {
    if is_dark() { dark(c) } else { c }
}

pub fn rgb(r: u8, g: u8, b: u8) -> Color32 {
    t(Color32::from_rgb(r, g, b))
}

pub fn gray(v: u8) -> Color32 {
    t(Color32::from_gray(v))
}

fn dark(c: Color32) -> Color32 {
    let [r, g, b, a] = c.to_srgba_unmultiplied();
    let (h, s, l) = rgb_to_hsl(r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0);
    // white -> 0.11, black -> 0.89; mid tones (syntax colours) are lifted a little
    let l2 = 0.11 + (1.0 - l) * 0.78;
    let s2 = if l > 0.85 { s * 0.55 } else { s.min(0.85) };
    let (r2, g2, b2) = hsl_to_rgb(h, s2, l2);
    Color32::from_rgba_unmultiplied((r2 * 255.0).round() as u8, (g2 * 255.0).round() as u8, (b2 * 255.0).round() as u8, a)
}

fn rgb_to_hsl(r: f32, g: f32, b: f32) -> (f32, f32, f32) {
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let l = (max + min) / 2.0;
    if (max - min).abs() < 1e-6 {
        return (0.0, 0.0, l);
    }
    let d = max - min;
    let s = if l > 0.5 { d / (2.0 - max - min) } else { d / (max + min) };
    let h = if max == r {
        (g - b) / d + if g < b { 6.0 } else { 0.0 }
    } else if max == g {
        (b - r) / d + 2.0
    } else {
        (r - g) / d + 4.0
    };
    (h / 6.0, s, l)
}

fn hsl_to_rgb(h: f32, s: f32, l: f32) -> (f32, f32, f32) {
    if s <= 0.0 {
        return (l, l, l);
    }
    let q = if l < 0.5 { l * (1.0 + s) } else { l + s - l * s };
    let p = 2.0 * l - q;
    let hue = |mut t: f32| {
        if t < 0.0 {
            t += 1.0;
        }
        if t > 1.0 {
            t -= 1.0;
        }
        if t < 1.0 / 6.0 {
            p + (q - p) * 6.0 * t
        } else if t < 0.5 {
            q
        } else if t < 2.0 / 3.0 {
            p + (q - p) * (2.0 / 3.0 - t) * 6.0
        } else {
            p
        }
    };
    (hue(h + 1.0 / 3.0), hue(h), hue(h - 1.0 / 3.0))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn mapping() {
        // dark() directly: the global switch would affect tests running in parallel
        let t = dark;
        let bg = t(Color32::WHITE);
        assert!(bg.r() < 40 && bg.g() < 40 && bg.b() < 40, "{bg:?}");
        let txt = t(Color32::from_rgb(40, 40, 46));
        assert!(txt.r() > 180, "{txt:?}");
        let kw = t(Color32::from_rgb(0, 84, 200));
        assert!(kw.b() > kw.r() && kw.b() > 150, "keyword stays blue: {kw:?}");
    }
}
