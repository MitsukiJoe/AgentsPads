use eframe::egui::{Color32, ColorImage};
use qrcode::{EcLevel, QrCode};

pub const QR_PT: f32 = 260.0;

pub fn raster_px(scale: f32) -> u32 {
    (QR_PT * scale).round().max(1.0) as u32
}

/// Always dark-on-light with an opaque quiet zone: phone decoders handle
/// inverted codes poorly, and EC level L keeps on-screen modules large.
pub fn color_image(payload: &str, px: u32) -> Result<ColorImage, String> {
    let code = QrCode::with_error_correction_level(payload.as_bytes(), EcLevel::L)
        .map_err(|e| e.to_string())?;
    let w = px as usize;
    let n = code.width();
    if n == 0 {
        return Err("empty qr".into());
    }
    let quiet = 4usize;
    let dim = n + quiet * 2;
    let mut pixels = vec![Color32::WHITE; w * w];
    for y in 0..w {
        for x in 0..w {
            let mx = x * dim / w;
            let my = y * dim / w;
            if mx >= quiet
                && my >= quiet
                && mx < quiet + n
                && my < quiet + n
                && code[(mx - quiet, my - quiet)] == qrcode::Color::Dark
            {
                pixels[y * w + x] = Color32::BLACK;
            }
        }
    }
    Ok(ColorImage::new([w, w], pixels))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raster_follows_scale_not_display_size() {
        assert_eq!(raster_px(1.0), 260);
        assert_eq!(raster_px(2.0), 520);
        assert_eq!(raster_px(1.5), 390);
        assert_eq!(QR_PT, 260.0);
    }

    #[test]
    fn renders_black_on_opaque_white() {
        let img = color_image("{\"v\":1}", 64).unwrap();
        assert_eq!(img.size, [64, 64]);
        assert!(img.pixels.contains(&Color32::BLACK));
        assert!(img
            .pixels
            .iter()
            .all(|p| *p == Color32::BLACK || *p == Color32::WHITE));
        assert_eq!(img.pixels[0], Color32::WHITE);
        assert_eq!(img.pixels[63], Color32::WHITE);
    }

    #[test]
    fn pairing_payload_stays_low_version() {
        let payload = r#"{"v":1,"type":"agentpad","device_id":"abcdefab-cdef-4abc-8def-abcdefabcdef","ip":"192.168.100.101","port":9618,"name":"Someone's MacBook Pro","os":"macos","secret":"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"}"#;
        let code = QrCode::with_error_correction_level(payload, EcLevel::L).unwrap();
        assert!(
            code.width() <= 53,
            "version too dense: {} modules",
            code.width()
        );
    }
}
