use adw::gtk;
use gtk::prelude::*;
use std::cell::RefCell;
use std::rc::Rc;

#[derive(Clone)]
pub struct VizHandle {
    values: Rc<RefCell<Vec<f32>>>, // 0.0..=1.0
}

impl VizHandle {
    pub fn set_values(&self, new_vals: &[f32]) {
        let mut v = self.values.borrow_mut();
        if v.len() != new_vals.len() {
            v.resize(new_vals.len(), 0.0);
        }

        for (dst, src) in v.iter_mut().zip(new_vals.iter()) {
            *dst = src.clamp(0.0, 1.0);
        }
    }
}

/// Create a drawing area that renders N bars, and a handle to update bar values.
/// The drawing color is taken from the widget's resolved CSS `color` value.
pub fn make_bars_visualizer(n_bars: usize, height: i32) -> (gtk::DrawingArea, VizHandle) {
    let values = Rc::new(RefCell::new(vec![0.0_f32; n_bars.max(1)]));
    let handle = VizHandle {
        values: values.clone(),
    };

    let area = gtk::DrawingArea::new();
    area.set_hexpand(true);
    area.set_vexpand(true);
    area.set_content_height(height);
    area.add_css_class("header-viz");

    let area_clone = area.clone();
    area.set_draw_func(move |_, cr, w, h| {
        let w = w as f64;
        let h = h as f64;

        let (r, g, b) = widget_css_color(&area_clone.clone().upcast::<gtk::Widget>());

        // Vertical gradient: stronger at top/bottom, weaker in the center where text sits.
        let grad = gtk::cairo::LinearGradient::new(0.0, 0.0, 0.0, h);
        let edge_a = 0.18;
        let center_a = 0.04;

        grad.add_color_stop_rgba(0.0, r, g, b, edge_a);
        grad.add_color_stop_rgba(0.40, r, g, b, center_a);
        grad.add_color_stop_rgba(0.60, r, g, b, center_a);
        grad.add_color_stop_rgba(1.0, r, g, b, edge_a);

        let _ = cr.set_source(&grad);

        let vals = values.borrow();
        let n = vals.len().max(1) as f64;
        let bar_w = (w / n).max(1.0);

        for (i, v) in vals.iter().enumerate() {
            let i = i as f64;
            let x = i * bar_w;

            let bh = (*v as f64) * (h * 0.85);
            let y = h - bh;

            // Slightly wider than computed bar_w to avoid gaps at small widths.
            cr.rectangle(x, y, (bar_w * 2.0).max(1.0), bh.max(1.0));
        }

        let _ = cr.fill();
    });

    (area, handle)
}

fn widget_css_color(widget: &gtk::Widget) -> (f64, f64, f64) {
    // Read the resolved CSS "color" from this widget
    let ctx = widget.style_context();
    if let Some(c) = ctx.lookup_color("color") {
        return (c.red() as f64, c.green() as f64, c.blue() as f64);
    }

    // Fallbacks that often exist in themes
    if let Some(c) = ctx.lookup_color("theme_fg_color") {
        return (c.red() as f64, c.green() as f64, c.blue() as f64);
    }
    if let Some(c) = ctx.lookup_color("window_fg_color") {
        return (c.red() as f64, c.green() as f64, c.blue() as f64);
    }

    (1.0, 1.0, 1.0)
}
