use adw::gtk;
use adw::gtk::gdk::gdk_pixbuf::{InterpType::Bilinear, Pixbuf};
use adw::gtk::gdk::Display;
use std::error::Error;

pub fn fetch_cover_bytes_blocking(url: &str) -> Result<Vec<u8>, Box<dyn Error + Send + Sync>> {
    let resp = reqwest::blocking::get(url)?;
    if !resp.status().is_success() {
        return Err(format!("Non-success status: {}", resp.status()).into());
    }
    let body = resp.bytes()?;
    Ok(body.to_vec())
}

pub fn install_css_provider() -> gtk::CssProvider {
    let provider = gtk::CssProvider::new();
    if let Some(display) = Display::default() {
        gtk::style_context_add_provider_for_display(
            &display,
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
    }
    provider
}

pub fn avg_rgb_from_pixbuf(pixbuf: &Pixbuf) -> (u8, u8, u8) {
    let small = pixbuf
        .scale_simple(32, 32, Bilinear)
        .unwrap_or_else(|| pixbuf.clone());

    let w = small.width() as usize;
    let h = small.height() as usize;
    let n_channels = small.n_channels() as usize;
    let rowstride = small.rowstride() as usize;
    let has_alpha = small.has_alpha();
    let pixels = unsafe { small.pixels() };

    let mut r_sum: u64 = 0;
    let mut g_sum: u64 = 0;
    let mut b_sum: u64 = 0;
    let mut count: u64 = 0;

    for y in 0..h {
        let row = &pixels[y * rowstride..(y * rowstride + w * n_channels)];
        for x in 0..w {
            let i = x * n_channels;
            let r = row[i] as u64;
            let g = row[i + 1] as u64;
            let b = row[i + 2] as u64;

            if has_alpha {
                let a = row[i + 3] as u64;
                if a < 20 {
                    continue; // ignore near-transparent
                }
            }

            r_sum += r;
            g_sum += g;
            b_sum += b;
            count += 1;
        }
    }
    if count == 0 {
        return (128, 128, 128);
    }

    (
        (r_sum / count) as u8,
        (g_sum / count) as u8,
        (b_sum / count) as u8,
    )
}

pub fn apply_color(provider: &gtk::CssProvider, tint: (u8, u8, u8), tint_is_light: bool) {
    let (r, g, b) = tint;

    // Foreground for buttons/text
    let (fr, fg, fb) = if tint_is_light {
        (0, 0, 0)
    } else {
        (255, 255, 255)
    };

    // slightly dim for backdrop
    let (br, bg, bb) = (
        (r as f32 * 0.92) as u8,
        (g as f32 * 0.92) as u8,
        (b as f32 * 0.92) as u8,
    );

    // viz bar color derived from the tint, but with contrast
    let (vr, vg, vb) = if tint_is_light {
        (
            (r as f32 * 0.25) as u8,
            (g as f32 * 0.25) as u8,
            (b as f32 * 0.25) as u8,
        )
    } else {
        (
            (255.0 - (255.0 - r as f32) * 0.25) as u8,
            (255.0 - (255.0 - g as f32) * 0.25) as u8,
            (255.0 - (255.0 - b as f32) * 0.25) as u8,
        )
    };
    let (vr, vg, vb) = boost_saturation(vr, vg, vb, 1.25);
    let css = format!(
        r#"
        .titlebar-tint {{
            background: rgb({r} {g} {b});
            color: rgb({fr} {fg} {fb});
        }}
        .titlebar-tint:backdrop {{
            background: rgb({br} {bg} {bb});
            color: rgb({fr} {fg} {fb});
        }}

        .header-viz {{
            color: rgb({vr} {vg} {vb});
        }}
        .header-viz:backdrop {{
            color: rgb({vr} {vg} {vb});
        }}

        headerbar.viz-transparent {{
            background: transparent;
            box-shadow: none;
        }}
        headerbar.viz-transparent:backdrop {{
            background: transparent;
            box-shadow: none;
        }}

        headerbar.cover-tint button {{
            color: inherit;
            background: transparent;
        }}
        headerbar.cover-tint button:hover {{
            background: rgba({fr} {fg} {fb} / 0.12);
        }}
        headerbar.cover-tint button:active {{
            background: rgba({fr} {fg} {fb} / 0.18);
        }}

        popover.cover-tint > contents {{
            background: rgb({r} {g} {b});
            color: rgb({fr} {fg} {fb});
            border-radius: 12px;
        }}
        popover.cover-tint > arrow {{
            background: rgb({r} {g} {b});
        }}
        "#
    );

    provider.load_from_data(&css);
}

pub fn apply_cover_tint_css_clear(provider: &gtk::CssProvider) {
    provider.load_from_data(
        r#"
        .titlebar-tint { background: transparent; }
        .header-viz { color: @accent_color; }
        .header-viz:backdrop { color: @accent_color; }
        headerbar.viz-transparent { background: transparent; box-shadow: none; }
        headerbar.viz-transparent:backdrop { background: transparent; box-shadow: none; }
        "#,
    );
}

pub fn is_light_color(r: u8, g: u8, b: u8) -> bool {
    let luma = 0.2126 * r as f32 + 0.7152 * g as f32 + 0.0722 * b as f32;
    luma > 130.0
}

pub fn boost_saturation(r: u8, g: u8, b: u8, amount: f32) -> (u8, u8, u8) {
    let gray = (r as f32 + g as f32 + b as f32) / 3.0;
    let boost = |c| (gray + (c as f32 - gray) * amount).clamp(0.0, 255.0) as u8;
    (boost(r), boost(g), boost(b))
}
