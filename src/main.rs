// Hide the console window on Windows release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod escp;
mod font_data;

use eframe::egui;
use egui::{Color32, Pos2, Rect, Vec2};

static DEMO: &[u8] = include_bytes!("../samples/demo.prn");

/// On-screen pixels (egui points) per inch at 100% zoom.
const BASE_PPI: f32 = 96.0;
/// Gap between pages, in inches.
const PAGE_GAP_IN: f32 = 0.35;
const MIN_ZOOM: f32 = 0.05;
const MAX_ZOOM: f32 = 40.0;

#[derive(Clone, Copy)]
enum ViewCmd {
    FitWidth,
    Actual,
}

struct App {
    name: String,
    raw: Vec<u8>,
    doc: escp::Document,
    opts: escp::Options,

    zoom: f32,
    /// Offset of the document origin from the top-left of the canvas, in points.
    pan: Vec2,
    canvas: Rect,
    pending: Option<ViewCmd>,

    dot_diameter_in: f32,
    status: String,
}

impl Default for App {
    fn default() -> Self {
        let mut app = Self {
            name: String::new(),
            raw: Vec::new(),
            doc: escp::Document::default(),
            opts: escp::Options::default(),
            zoom: 1.0,
            pan: Vec2::new(24.0, 24.0),
            canvas: Rect::from_min_size(Pos2::ZERO, Vec2::new(800.0, 600.0)),
            pending: None,
            dot_diameter_in: 0.014,
            status: String::new(),
        };
        app.load("demo.prn (built in)".to_owned(), DEMO.to_vec());
        app
    }
}

impl App {
    fn load(&mut self, name: String, bytes: Vec<u8>) {
        self.name = name;
        self.raw = bytes;
        self.reparse();
        self.pending = Some(ViewCmd::FitWidth);
    }

    fn reparse(&mut self) {
        self.doc = escp::interpret(&self.raw, &self.opts);
    }

    fn open_path(&mut self, path: &std::path::Path) {
        match std::fs::read(path) {
            Ok(bytes) => {
                let name = path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| path.display().to_string());
                self.status.clear();
                self.load(name, bytes);
            }
            Err(e) => self.status = format!("Could not read {}: {e}", path.display()),
        }
    }

    fn ppi(&self) -> f32 {
        BASE_PPI * self.zoom
    }

    /// Zoom by `factor`, keeping the document point under `anchor` fixed.
    fn zoom_at(&mut self, anchor: Pos2, factor: f32) {
        let old_ppi = self.ppi();
        let new_zoom = (self.zoom * factor).clamp(MIN_ZOOM, MAX_ZOOM);
        if (new_zoom - self.zoom).abs() < f32::EPSILON {
            return;
        }
        let rel = anchor - self.canvas.min;
        let world = (rel - self.pan) / old_ppi; // inches
        self.zoom = new_zoom;
        self.pan = rel - world * self.ppi();
    }

    fn apply_view_cmd(&mut self, cmd: ViewCmd) {
        match cmd {
            ViewCmd::FitWidth => {
                let margin = 24.0;
                let avail = (self.canvas.width() - 2.0 * margin).max(50.0);
                self.zoom = (avail / (escp::PAGE_W_IN * BASE_PPI)).clamp(MIN_ZOOM, MAX_ZOOM);
                self.pan = Vec2::new(margin, margin);
            }
            ViewCmd::Actual => {
                let anchor = self.canvas.center();
                self.zoom_at(anchor, 1.0 / self.zoom);
            }
        }
    }

    fn toolbar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_wrapped(|ui| {
            if ui.button("Open…").clicked() {
                if let Some(path) = rfd::FileDialog::new()
                    .set_title("Open raw printer data")
                    .pick_file()
                {
                    self.open_path(&path);
                }
            }
            if ui.button("Demo").clicked() {
                self.status.clear();
                self.load("demo.prn (built in)".to_owned(), DEMO.to_vec());
            }
            ui.separator();

            let center = self.canvas.center();
            if ui.button("−").on_hover_text("Zoom out").clicked() {
                self.zoom_at(center, 1.0 / 1.25);
            }
            ui.label(format!("{:.0}%", self.zoom * 100.0));
            if ui.button("+").on_hover_text("Zoom in").clicked() {
                self.zoom_at(center, 1.25);
            }
            if ui.button("100%").clicked() {
                self.pending = Some(ViewCmd::Actual);
            }
            if ui.button("Fit width").clicked() {
                self.pending = Some(ViewCmd::FitWidth);
            }
            ui.separator();

            ui.label("Dot size");
            ui.add(
                egui::Slider::new(&mut self.dot_diameter_in, 0.006..=0.024)
                    .custom_formatter(|v, _| format!("{:.0} mil", v * 1000.0))
                    .show_value(true),
            );
            ui.separator();

            if ui
                .checkbox(&mut self.opts.auto_cr_on_lf, "LF implies CR")
                .changed()
            {
                self.reparse();
            }
        });
    }

    fn canvas_ui(&mut self, ui: &mut egui::Ui) {
        let (resp, painter) =
            ui.allocate_painter(ui.available_size(), egui::Sense::click_and_drag());
        let rect = resp.rect;
        self.canvas = rect;

        if let Some(cmd) = self.pending.take() {
            self.apply_view_cmd(cmd);
        }

        // Interaction: drag = pan, wheel = scroll, ctrl+wheel / pinch = zoom.
        if resp.dragged() {
            self.pan += resp.drag_delta();
        }
        if resp.hovered() {
            let (zoom_delta, scroll) = ui.input(|i| (i.zoom_delta(), i.smooth_scroll_delta));
            if (zoom_delta - 1.0).abs() > 1e-4 {
                if let Some(p) = ui.input(|i| i.pointer.hover_pos()) {
                    self.zoom_at(p, zoom_delta);
                }
            } else {
                self.pan += scroll;
            }
        }

        let ppi = self.ppi();
        let origin = rect.min + self.pan;
        let painter = painter.with_clip_rect(rect);
        painter.rect_filled(rect, 0.0, Color32::from_gray(52));

        let paper = Color32::from_rgb(250, 246, 232);
        let shadow = Color32::from_black_alpha(90);
        let ink = Color32::from_rgba_unmultiplied(22, 20, 30, 235);

        let radius = (self.dot_diameter_in * 0.5 * ppi).max(0.4);
        let draw_circles = radius >= 1.5;
        let cull = rect.expand(radius + 1.0);

        let page_w = escp::PAGE_W_IN * ppi;
        let page_h = escp::PAGE_H_IN * ppi;

        for (pi, page) in self.doc.pages.iter().enumerate() {
            let top = pi as f32 * (escp::PAGE_H_IN + PAGE_GAP_IN) * ppi;
            let page_rect =
                Rect::from_min_size(origin + Vec2::new(0.0, top), Vec2::new(page_w, page_h));
            if !page_rect.intersects(rect) {
                continue;
            }
            painter.rect_filled(page_rect.translate(Vec2::new(4.0, 4.0)), 0.0, shadow);
            painter.rect_filled(page_rect, 0.0, paper);

            let sx = ppi / escp::X_PER_INCH;
            let sy = ppi / escp::Y_PER_INCH;
            let mut mesh = egui::Mesh::default();
            for d in &page.dots {
                let c = Pos2::new(page_rect.min.x + d.x as f32 * sx, page_rect.min.y + d.y as f32 * sy);
                if !cull.contains(c) {
                    continue;
                }
                if draw_circles {
                    painter.circle_filled(c, radius, ink);
                } else {
                    // Zoomed out: one cheap quad per dot, batched into a single mesh.
                    mesh.add_colored_rect(Rect::from_center_size(c, Vec2::splat(radius * 2.0)), ink);
                }
            }
            if !mesh.is_empty() {
                painter.add(egui::Shape::mesh(mesh));
            }
        }
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Drag & drop a file onto the window.
        let dropped = ctx.input(|i| i.raw.dropped_files.clone());
        if let Some(file) = dropped.into_iter().next() {
            if let Some(path) = &file.path {
                self.open_path(path);
            } else if let Some(bytes) = &file.bytes {
                let name = if file.name.is_empty() { "dropped file".to_owned() } else { file.name.clone() };
                self.load(name, bytes.to_vec());
            }
        }

        egui::TopBottomPanel::top("toolbar").show(ctx, |ui| self.toolbar(ui));

        egui::TopBottomPanel::bottom("status").show(ctx, |ui| {
            ui.horizontal(|ui| {
                if self.status.is_empty() {
                    ui.label(format!(
                        "{} — {} bytes, {} page(s), {} dots",
                        self.name,
                        self.raw.len(),
                        self.doc.pages.len(),
                        self.doc.dot_count()
                    ));
                } else {
                    ui.colored_label(Color32::LIGHT_RED, &self.status);
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label("drag to pan · wheel to scroll · Ctrl+wheel to zoom · drop a file to open");
                });
            });
        });

        egui::CentralPanel::default()
            .frame(egui::Frame::NONE)
            .show(ctx, |ui| self.canvas_ui(ui));
    }
}

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        renderer: eframe::Renderer::Wgpu,
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1100.0, 800.0])
            .with_title("RustyDotMatrix"),
        ..Default::default()
    };
    eframe::run_native(
        "RustyDotMatrix",
        options,
        Box::new(|_cc| Ok(Box::new(App::default()))),
    )
}
