//! The RetroImpact window: open printer data, look at the sheets, tune the look, export.
use eframe::egui::{self, Align2, Color32, FontId, Pos2, Rect, Sense, Vec2};
use retroimpact::emu::analyze::classify;
use retroimpact::emu::charsets::CODEPAGES;
use retroimpact::emu::detect::score_printers;
use retroimpact::emu::emulate::{emulate, thousands};
use retroimpact::emu::model::{profile, short_label, Document, EmuOptions, PROFILES};
use retroimpact::emu::pageset::{Item, PageSet};
use retroimpact::emu::settings::{paper_geometry, RenderSettings, INK_SETS, LOOK_PRESETS, PAPERS, PIN_SHAPES, RIBBONS};
use retroimpact::export::{export_sheets, Colour, ExportOptions, Format};
use retroimpact::fontraster::AbRaster;
use retroimpact::render::{build_sheet, dots_key, Region, Renderer, SheetBuf};
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Mutex};

static DEMO: &[u8] = include_bytes!("../samples/demo.prn");

/// On-screen points per inch at 100% zoom.
const BASE_PPI: f32 = 96.0;
const PAGE_GAP_IN: f32 = 0.30;
const GAP_BAR: f32 = 26.0;
const MIN_ZOOM: f32 = 0.05;
const MAX_ZOOM: f32 = 8.0;

struct Loaded {
    name: String,
    raw: Arc<Vec<u8>>,
    doc: Arc<Document>,
    pages: PageSet,
    /// bumps whenever the dots change (new data / other printer)
    generation: u64,
}

enum Msg {
    Loaded { name: String, raw: Arc<Vec<u8>>, doc: Document },
}

struct PageTex {
    key: u64,
    region: Region,
    cap: (u32, u32),
    tex: wgpu::Texture,
    tid: egui::TextureId,
}

struct ExportJob {
    done: Arc<AtomicUsize>,
    total: usize,
    cancel: Arc<AtomicBool>,
    result: Arc<Mutex<Option<Result<Vec<PathBuf>, String>>>>,
}

struct ExportDlg {
    open: bool,
    opts: ExportOptions,
    jpeg: bool,
    quality: u8,
    job: Option<ExportJob>,
    message: String,
}

#[derive(Clone, Copy, PartialEq)]
enum Fit {
    None,
    Width,
    Page,
}

pub struct App {
    render_state: eframe::egui_wgpu::RenderState,
    gpu: Renderer,
    settings: RenderSettings,
    emu: EmuOptions,
    loaded: Option<Loaded>,
    busy: bool,
    tx: mpsc::Sender<Msg>,
    rx: mpsc::Receiver<Msg>,
    sheets: HashMap<usize, (u64, u64, SheetBuf)>,
    tex: HashMap<usize, PageTex>,
    zoom: f32,
    pan: Vec2,
    fit: Fit,
    canvas: Rect,
    printer_order: Vec<&'static str>,
    printer_try: usize,
    export: ExportDlg,
    status: String,
    error: String,
    recent: Vec<PathBuf>,
}

fn settings_key(s: &RenderSettings) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    s.to_kv().hash(&mut h);
    h.finish()
}

fn hash_of<T: Hash>(t: &T) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    t.hash(&mut h);
    h.finish()
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        retroimpact::emu::glyphs::set_font_raster(Box::new(AbRaster::new()));
        let rs = cc.wgpu_render_state.clone().expect("RetroImpact needs the wgpu renderer");
        let gpu = Renderer::new(rs.device.clone(), rs.queue.clone());
        let (tx, rx) = mpsc::channel();
        let mut settings = RenderSettings::default();
        let mut recent = Vec::new();
        if let Some(st) = cc.storage {
            if let Some(kv) = st.get_string("settings") {
                settings = RenderSettings::from_kv(&kv);
            }
            if let Some(r) = st.get_string("recent") {
                recent = r.lines().map(PathBuf::from).filter(|p| p.exists()).collect();
            }
        }
        let mut app = App {
            render_state: rs,
            gpu,
            settings,
            emu: EmuOptions::default(),
            loaded: None,
            busy: false,
            tx,
            rx,
            sheets: HashMap::new(),
            tex: HashMap::new(),
            zoom: 1.0,
            pan: Vec2::new(0.0, 12.0),
            fit: Fit::Width,
            canvas: Rect::from_min_size(Pos2::ZERO, Vec2::new(900.0, 700.0)),
            printer_order: Vec::new(),
            printer_try: 0,
            export: ExportDlg { open: false, opts: ExportOptions::default(), jpeg: false, quality: 90, job: None, message: String::new() },
            status: String::new(),
            error: String::new(),
            recent,
        };
        app.start_emulate(&cc.egui_ctx, "demo.prn (built in)".into(), Arc::new(DEMO.to_vec()));
        app
    }

    fn start_emulate(&mut self, ctx: &egui::Context, name: String, raw: Arc<Vec<u8>>) {
        self.busy = true;
        self.error.clear();
        let tx = self.tx.clone();
        let opts = self.emu.clone();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let doc = emulate(&raw, &opts);
            let _ = tx.send(Msg::Loaded { name, raw, doc });
            ctx.request_repaint();
        });
    }

    fn open_path(&mut self, ctx: &egui::Context, path: &Path) {
        match std::fs::read(path) {
            Ok(bytes) => {
                self.recent.retain(|p| p != path);
                self.recent.insert(0, path.to_path_buf());
                self.recent.truncate(8);
                self.emu.printer = "auto".into();
                self.printer_order = ranked_printers(&bytes);
                self.printer_try = 0;
                let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
                self.start_emulate(ctx, name, Arc::new(bytes));
            }
            Err(e) => self.error = format!("Cannot open {}: {e}", path.display()),
        }
    }

    fn pick_and_open(&mut self, ctx: &egui::Context) {
        if let Some(p) = rfd::FileDialog::new().set_title("Open printer data").pick_file() {
            self.open_path(ctx, &p);
        }
    }

    fn reemulate(&mut self, ctx: &egui::Context) {
        if let Some(l) = &self.loaded {
            let (name, raw) = (l.name.clone(), l.raw.clone());
            self.start_emulate(ctx, name, raw);
        }
    }

    fn try_next_printer(&mut self, ctx: &egui::Context) {
        if self.printer_order.is_empty() {
            return;
        }
        self.printer_try = (self.printer_try + 1) % self.printer_order.len();
        self.emu.printer = self.printer_order[self.printer_try].to_string();
        self.reemulate(ctx);
    }

    fn poll(&mut self) {
        while let Ok(m) = self.rx.try_recv() {
            match m {
                Msg::Loaded { name, raw, doc } => {
                    let infos = classify(&doc);
                    let flags = infos.into_iter().filter(|i| i.hidden_by_default()).map(|i| (i.index, i)).collect();
                    let pages = PageSet::new(doc.pages.len(), flags);
                    if self.printer_order.is_empty() {
                        self.printer_order = ranked_printers(&raw);
                    }
                    let generation = self.loaded.as_ref().map_or(0, |l| l.generation) + 1;
                    self.sheets.clear();
                    self.loaded = Some(Loaded { name, raw, doc: Arc::new(doc), pages, generation });
                    self.fit = Fit::Width;
                    self.pan = Vec2::new(0.0, 12.0);
                    self.busy = false;
                }
            }
        }
    }

    /// (width, height) of sheet `i` as displayed at `ppi` pixels per inch.
    fn page_px(&self, i: usize, ppi: f32) -> (u32, u32) {
        let l = self.loaded.as_ref().unwrap();
        let p = &l.doc.pages[i];
        let (pw, ph, ..) = paper_geometry(&self.settings.paper, p.width_in, p.height_in, l.doc.carriage_in);
        let (w, h) = ((pw * ppi as f64).round().max(1.0) as u32, (ph * ppi as f64).round().max(1.0) as u32);
        if (self.settings.rotation / 90) % 2 == 1 {
            (h, w)
        } else {
            (w, h)
        }
    }

    fn page_size_in(&self, i: usize) -> (f32, f32) {
        let (w, h) = self.page_px(i, 1000.0);
        (w as f32 / 1000.0, h as f32 / 1000.0)
    }

    fn settings_ui(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let before = self.settings.clone();
        ui.heading("Printer");
        ui.horizontal_wrapped(|ui| {
            let mut changed = false;
            if ui.selectable_label(self.emu.printer == "auto", "Auto").clicked() {
                self.emu.printer = "auto".into();
                changed = true;
            }
            for p in PROFILES.iter() {
                if ui.selectable_label(self.emu.printer == p.key, short_label(p.key)).on_hover_text(p.label).clicked() {
                    self.emu.printer = p.key.into();
                    changed = true;
                }
            }
            if changed {
                self.reemulate(ctx);
            }
        });
        if ui.button("Try next printer (F8)").clicked() {
            self.try_next_printer(ctx);
        }
        if let Some(l) = &self.loaded {
            let det = profile(&l.doc.detected).map(|p| p.label).unwrap_or(if l.doc.detected == "mixed" { "several printers (mixed stream)" } else { "?" });
            ui.small(format!("Emulating: {det}"));
        }
        egui::CollapsingHeader::new("Emulation options").show(ui, |ui| {
            let mut changed = false;
            egui::ComboBox::from_label("Code page").selected_text(self.emu.codepage.clone()).show_ui(ui, |ui| {
                changed |= ui.selectable_value(&mut self.emu.codepage, "auto".into(), "auto").changed();
                for (k, label) in CODEPAGES.iter() {
                    changed |= ui.selectable_value(&mut self.emu.codepage, (*k).into(), *label).changed();
                }
            });
            let nl = |ui: &mut egui::Ui, label: &str, v: &mut Option<bool>| {
                let txt = match v {
                    None => "auto",
                    Some(true) => "on",
                    Some(false) => "off",
                };
                egui::ComboBox::from_label(label).selected_text(txt).show_ui(ui, |ui| {
                    let mut c = ui.selectable_value(v, None, "auto").changed();
                    c |= ui.selectable_value(v, Some(true), "on").changed();
                    c |= ui.selectable_value(v, Some(false), "off").changed();
                    c
                }).inner.unwrap_or(false)
            };
            changed |= nl(ui, "CR adds LF", &mut self.emu.cr_adds_lf);
            changed |= nl(ui, "LF adds CR", &mut self.emu.lf_adds_cr);
            changed |= nl(ui, "Force NLQ", &mut self.emu.force_nlq);
            changed |= nl(ui, "Wide carriage", &mut self.emu.wide_carriage);
            changed |= ui.add(egui::DragValue::new(&mut self.emu.page_length_in).range(2.0..=24.0).speed(0.05).suffix(" in")).on_hover_text("Page length").changed();
            changed |= ui.checkbox(&mut self.emu.honor_page_length, "Honour page length commands").changed();
            changed |= ui.checkbox(&mut self.emu.skip_blank_ff, "Skip blank pages from form feeds").changed();
            changed |= ui.checkbox(&mut self.emu.esc_a_immediate, "ESC A takes effect at once").changed();
            if changed {
                self.reemulate(ctx);
            }
        });
        ui.separator();
        ui.heading("Look");
        ui.horizontal_wrapped(|ui| {
            for (i, p) in LOOK_PRESETS.iter().enumerate() {
                if ui.small_button(p.name).clicked() {
                    self.settings.clean = false;
                    self.settings.apply_look(&LOOK_PRESETS[i]);
                }
            }
        });
        ui.checkbox(&mut self.settings.clean, "Clean dots (no paper or ribbon simulation)");
        let s = &mut self.settings;
        ui.add_enabled_ui(!s.clean, |ui| {
            ui.add(egui::Slider::new(&mut s.dot_size, 0.3..=2.0).text("Dot size"));
            ui.add(egui::Slider::new(&mut s.density, 0.0..=1.0).text("Strike density"));
            ui.add(egui::Slider::new(&mut s.saturation, 0.05..=1.5).text("Ink load"));
            ui.add(egui::Slider::new(&mut s.ribbon_wear, 0.0..=1.0).text("Ribbon wear"));
            ui.add(egui::Slider::new(&mut s.misalign, 0.0..=1.0).text("Head misalignment"));
            ui.add(egui::Slider::new(&mut s.jitter, 0.0..=1.0).text("Dot jitter"));
            ui.add(egui::Slider::new(&mut s.paper_texture, 0.0..=1.0).text("Paper texture"));
            ui.add(egui::Slider::new(&mut s.impact, 0.0..=1.0).text("Impact (embossing)"));
        });
        ui.add_enabled(s.clean, egui::Slider::new(&mut s.dot_size, 0.3..=2.0).text("Dot size"));
        egui::ComboBox::from_label("Pin shape").selected_text(PIN_SHAPES.iter().find(|p| p.0 == s.pin_shape).map_or("", |p| p.1)).show_ui(ui, |ui| {
            for p in PIN_SHAPES.iter() {
                ui.selectable_value(&mut s.pin_shape, p.0.to_string(), p.1).on_hover_text(p.2);
            }
        });
        egui::ComboBox::from_label("Ribbon").selected_text(RIBBONS.iter().find(|p| p.0 == s.ribbon).map_or("", |p| p.1)).show_ui(ui, |ui| {
            for p in RIBBONS.iter() {
                ui.selectable_value(&mut s.ribbon, p.0.to_string(), p.1);
            }
        });
        egui::ComboBox::from_label("Colour inks").selected_text(INK_SETS.iter().find(|p| p.0 == s.ink_set).map_or("custom", |p| p.1)).show_ui(ui, |ui| {
            for p in INK_SETS.iter() {
                ui.selectable_value(&mut s.ink_set, p.0.to_string(), p.1);
            }
        });
        ui.checkbox(&mut s.color_ribbon, "Honour colour ribbon commands");
        egui::ComboBox::from_label("Paper").selected_text(PAPERS.iter().find(|p| p.key == s.paper).map_or("", |p| p.label)).show_ui(ui, |ui| {
            for p in PAPERS.iter() {
                ui.selectable_value(&mut s.paper, p.key.to_string(), p.label);
            }
        });
        ui.checkbox(&mut s.show_holes, "Tractor holes");
        ui.horizontal(|ui| {
            ui.label("Rotate");
            if ui.button("⟲").clicked() {
                s.rotation = (s.rotation + 270) % 360;
            }
            if ui.button("⟳").clicked() {
                s.rotation = (s.rotation + 90) % 360;
            }
            ui.label("Seed");
            ui.add(egui::DragValue::new(&mut s.seed));
        });
        s.clamp();
        if *s != before && s.rotation != before.rotation {
            self.fit = Fit::Width;
        }
    }

    fn visible_pages(&self) -> Vec<usize> {
        self.loaded.as_ref().map(|l| l.pages.visible()).unwrap_or_default()
    }

    fn canvas_ui(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let (rect, resp) = ui.allocate_exact_size(ui.available_size(), Sense::click_and_drag());
        self.canvas = rect;
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 0.0, Color32::from_gray(70));
        if self.loaded.is_none() {
            painter.text(rect.center(), Align2::CENTER_CENTER, if self.busy { "Reading printer data…" } else { "Open or drop a printer capture" }, FontId::proportional(18.0), Color32::from_gray(200));
            return;
        }
        let ppp = ctx.pixels_per_point();

        // layout in inches -> points at the current zoom
        let items = self.loaded.as_ref().unwrap().pages.items();
        let max_w_in = items.iter().filter_map(|it| if let Item::Page(i) = it { Some(self.page_size_in(*i).0) } else { None }).fold(1.0f32, f32::max);
        let first_h_in = items.iter().find_map(|it| if let Item::Page(i) = it { Some(self.page_size_in(*i).1) } else { None }).unwrap_or(11.0);
        match self.fit {
            Fit::Width => self.zoom = ((rect.width() - 40.0) / (max_w_in * BASE_PPI)).clamp(MIN_ZOOM, MAX_ZOOM),
            Fit::Page => self.zoom = ((rect.height() - 30.0) / (first_h_in * BASE_PPI)).min((rect.width() - 40.0) / (max_w_in * BASE_PPI)).clamp(MIN_ZOOM, MAX_ZOOM),
            Fit::None => {}
        }
        if self.fit != Fit::None {
            self.fit = Fit::None;
            self.pan = Vec2::new(0.0, 12.0);
        }

        // input: zoom (pinch / Ctrl+wheel), scroll, drag
        let (zoom_delta, scroll) = ui.input(|i| (i.zoom_delta(), i.smooth_scroll_delta));
        if resp.hovered() && (zoom_delta - 1.0).abs() > 1e-4 {
            let nz = (self.zoom * zoom_delta).clamp(MIN_ZOOM, MAX_ZOOM);
            let f = nz / self.zoom;
            let a = resp.hover_pos().unwrap_or(rect.center());
            let x0 = rect.center().x + self.pan.x;
            let y0 = rect.min.y + self.pan.y;
            self.pan.x = (a.x - (a.x - x0) * f) - rect.center().x;
            self.pan.y = (a.y - (a.y - y0) * f) - rect.min.y;
            self.zoom = nz;
        }
        if resp.hovered() {
            self.pan += scroll;
        }
        self.pan += resp.drag_delta();
        if resp.double_clicked() {
            self.fit = Fit::Width;
        }

        let zoom = self.zoom;
        let ppi = BASE_PPI * zoom * ppp;
        let gap = PAGE_GAP_IN * BASE_PPI * zoom;
        // vertical extents
        let mut y = rect.min.y + self.pan.y;
        let cx = rect.center().x + self.pan.x;
        struct Slot {
            item: Item,
            rect: Rect,
        }
        let mut slots: Vec<Slot> = Vec::new();
        for it in &items {
            match it {
                Item::Page(i) => {
                    let (w, h) = self.page_px(*i, ppi);
                    let size = Vec2::new(w as f32, h as f32) / ppp;
                    slots.push(Slot { item: it.clone(), rect: Rect::from_min_size(Pos2::new((cx - size.x / 2.0).round(), y.round()), size) });
                    y += size.y + gap;
                }
                Item::Gap(..) => {
                    slots.push(Slot { item: it.clone(), rect: Rect::from_min_size(Pos2::new(cx - 160.0, y), Vec2::new(320.0, GAP_BAR)) });
                    y += GAP_BAR + 6.0;
                }
            }
        }
        let total_h = y - (rect.min.y + self.pan.y);
        let min_y = (rect.height() - total_h - 12.0).min(12.0);
        self.pan.y = self.pan.y.clamp(min_y, 12.0);

        let sk = settings_key(&self.settings);
        let dk = hash_of(&dots_key(&self.settings));
        let mut reveal: Option<(usize, usize)> = None;
        for slot in &slots {
            match &slot.item {
                Item::Gap(a, b) => {
                    let r = slot.rect;
                    let hit = ui.interact(r, ui.id().with(("gap", *a)), Sense::click());
                    painter.rect_filled(r, 4.0, if hit.hovered() { Color32::from_gray(110) } else { Color32::from_gray(90) });
                    let txt = if a == b { format!("page {} hidden (click to show)", a + 1) } else { format!("pages {}–{} hidden (click to show)", a + 1, b + 1) };
                    painter.text(r.center(), Align2::CENTER_CENTER, txt, FontId::proportional(13.0), Color32::WHITE);
                    if hit.clicked() {
                        reveal = Some((*a, *b));
                    }
                }
                Item::Page(i) => {
                    let i = *i;
                    let pr = slot.rect;
                    let vis = pr.intersect(rect);
                    painter.rect_filled(pr.translate(Vec2::new(3.0, 3.0)), 0.0, Color32::from_black_alpha(70));
                    painter.rect_filled(pr, 0.0, Color32::from_gray(235));
                    if vis.width() <= 0.0 || vis.height() <= 0.0 {
                        continue;
                    }
                    // sheet on the GPU
                    let l = self.loaded.as_ref().unwrap();
                    let gen = l.generation;
                    let need_sheet = !matches!(self.sheets.get(&i), Some((k, g, _)) if *k == dk && *g == gen);
                    if need_sheet {
                        if self.sheets.len() > 6 {
                            self.sheets.clear();
                        }
                        let sheet = build_sheet(&l.doc.pages[i], &l.doc, &self.settings);
                        self.sheets.insert(i, (dk, gen, self.gpu.upload(sheet)));
                    }
                    let (_, _, sb) = self.sheets.get(&i).unwrap();
                    // the pixel rectangle we need, a little more than what is visible so small pans need no re-render
                    let (wd, hd) = self.page_px(i, ppi);
                    let pad = 160.0;
                    let need = Region {
                        x: (((vis.min.x - pr.min.x) * ppp) - pad).floor().max(0.0) as i32,
                        y: (((vis.min.y - pr.min.y) * ppp) - pad).floor().max(0.0) as i32,
                        w: 0,
                        h: 0,
                    };
                    let x1 = ((((vis.max.x - pr.min.x) * ppp) + pad).ceil() as i64).min(wd as i64) as i32;
                    let y1 = ((((vis.max.y - pr.min.y) * ppp) + pad).ceil() as i64).min(hd as i64) as i32;
                    let max_tile = self.gpu.max_tile(&sb.sheet, &self.settings, ppi);
                    let need = Region { w: ((x1 - need.x).max(1) as u32).min(max_tile), h: ((y1 - need.y).max(1) as u32).min(max_tile), ..need };
                    let key = sk ^ hash_of(&(ppi.to_bits(), i, gen));
                    let key_vis = Region {
                        x: ((vis.min.x - pr.min.x) * ppp).floor().max(0.0) as i32,
                        y: ((vis.min.y - pr.min.y) * ppp).floor().max(0.0) as i32,
                        w: (vis.width() * ppp).ceil() as u32 + 1,
                        h: (vis.height() * ppp).ceil() as u32 + 1,
                    };
                    let fresh = matches!(self.tex.get(&i), Some(t) if t.key == key && covers(&t.region, &key_vis));
                    if !fresh {
                        let cap_needed = (need.w, need.h);
                        let reuse = matches!(self.tex.get(&i), Some(t) if t.cap.0 >= cap_needed.0 && t.cap.1 >= cap_needed.1);
                        if !reuse {
                            if let Some(old) = self.tex.remove(&i) {
                                self.render_state.renderer.write().free_texture(&old.tid);
                            }
                            let cap = (cap_needed.0.max(256), cap_needed.1.max(256));
                            let tex = self.gpu.out_texture(cap.0, cap.1);
                            let view = tex.create_view(&wgpu::TextureViewDescriptor::default());
                            let tid = self.render_state.renderer.write().register_native_texture(&self.render_state.device, &view, wgpu::FilterMode::Linear);
                            self.tex.insert(i, PageTex { key: 0, region: need, cap, tex, tid });
                        }
                        let pt = self.tex.get_mut(&i).unwrap();
                        let done = self.gpu.render_into(sb, &self.settings, ppi, need, &pt.tex);
                        pt.region = done;
                        pt.key = key;
                    }
                    let pt = self.tex.get(&i).unwrap();
                    let r = pt.region;
                    let draw = Rect::from_min_size(
                        Pos2::new(pr.min.x + r.x as f32 / ppp, pr.min.y + r.y as f32 / ppp),
                        Vec2::new(r.w as f32, r.h as f32) / ppp,
                    );
                    let uv = Rect::from_min_max(Pos2::ZERO, Pos2::new(r.w as f32 / pt.cap.0 as f32, r.h as f32 / pt.cap.1 as f32));
                    painter.image(pt.tid, draw, uv, Color32::WHITE);
                    painter.text(pr.left_top() + Vec2::new(4.0, -14.0), Align2::LEFT_TOP, format!("{}", i + 1), FontId::proportional(11.0), Color32::from_gray(190));
                }
            }
        }
        // forget textures of sheets that are gone / far away
        if self.tex.len() > 12 {
            let ids: Vec<usize> = self.tex.keys().copied().collect();
            for k in ids {
                if let Some(t) = self.tex.remove(&k) {
                    self.render_state.renderer.write().free_texture(&t.tid);
                }
            }
        }
        if let Some((a, b)) = reveal {
            if let Some(l) = self.loaded.as_mut() {
                l.pages.reveal(a, b);
            }
        }
        // which sheet is in the middle of the view?
        let mid = rect.center().y;
        self.status = String::new();
        for slot in &slots {
            if let Item::Page(i) = slot.item {
                if slot.rect.min.y <= mid && slot.rect.max.y >= mid {
                    self.status = format!("page {} of {}", i + 1, self.loaded.as_ref().unwrap().doc.pages.len());
                }
            }
        }
        let _ = ctx;
    }

    fn current_page(&self) -> Option<usize> {
        let l = self.loaded.as_ref()?;
        let n = l.doc.pages.len();
        let t: Vec<usize> = l.pages.visible();
        if t.is_empty() {
            return None;
        }
        // page whose number is shown in the status line
        let num: usize = self.status.strip_prefix("page ").and_then(|s| s.split_whitespace().next()).and_then(|s| s.parse().ok()).unwrap_or(1);
        Some((num - 1).min(n.saturating_sub(1)))
    }

    fn export_ui(&mut self, ctx: &egui::Context) {
        if !self.export.open {
            return;
        }
        let mut open = true;
        let pages = self.visible_pages();
        egui::Window::new("Export").open(&mut open).collapsible(false).resizable(false).show(ctx, |ui| {
            let e = &mut self.export;
            let running = e.job.is_some();
            ui.add_enabled_ui(!running, |ui| {
                ui.horizontal(|ui| {
                    ui.label("Format");
                    ui.selectable_value(&mut e.opts.format, Format::Pdf, "PDF");
                    ui.selectable_value(&mut e.opts.format, Format::Tiff, "TIFF");
                    ui.selectable_value(&mut e.opts.format, Format::Png, "PNG");
                });
                ui.horizontal(|ui| {
                    ui.label("Resolution");
                    ui.add(egui::DragValue::new(&mut e.opts.dpi).range(72..=1200).suffix(" dpi"));
                    for d in [150, 300, 600] {
                        if ui.small_button(d.to_string()).clicked() {
                            e.opts.dpi = d;
                        }
                    }
                });
                ui.horizontal(|ui| {
                    ui.label("Colour");
                    ui.selectable_value(&mut e.opts.colour, Colour::Rgb, "Colour");
                    ui.selectable_value(&mut e.opts.colour, Colour::Gray, "Greyscale");
                    ui.selectable_value(&mut e.opts.colour, Colour::Mono, "Black & white (1-bit)");
                });
                match e.opts.format {
                    Format::Pdf => {
                        ui.checkbox(&mut e.jpeg, "JPEG compression (smaller, slightly lossy)");
                        if e.jpeg {
                            ui.add(egui::Slider::new(&mut e.quality, 40..=100).text("Quality"));
                        }
                    }
                    Format::Tiff => {
                        ui.checkbox(&mut e.opts.tiff_deflate, "Deflate compression");
                    }
                    Format::Png => {}
                }
                if e.opts.format != Format::Png {
                    ui.checkbox(&mut e.opts.single_file, "All pages in one file");
                }
                ui.label(format!("{} page(s) will be exported.", pages.len()));
            });
            if let Some(job) = &e.job {
                let done = job.done.load(Ordering::Relaxed);
                ui.add(egui::ProgressBar::new((done as f32 + 0.5) / job.total.max(1) as f32).text(format!("page {} of {}", done + 1, job.total)));
                if ui.button("Cancel").clicked() {
                    job.cancel.store(true, Ordering::Relaxed);
                }
            } else if ui.add_enabled(!pages.is_empty(), egui::Button::new("Export…")).clicked() {
                self.start_export(ctx, pages.clone());
            }
            if !self.export.message.is_empty() {
                ui.label(&self.export.message);
            }
        });
        if !open {
            self.export.open = false;
        }
        // finished?
        let finished = self.export.job.as_ref().and_then(|j| j.result.lock().unwrap().take());
        if let Some(res) = finished {
            self.export.job = None;
            self.export.message = match res {
                Ok(files) => format!("Done: {}", files.iter().map(|f| f.display().to_string()).collect::<Vec<_>>().join(", ")),
                Err(e) => format!("Export failed: {e}"),
            };
        }
    }

    fn start_export(&mut self, ctx: &egui::Context, pages: Vec<usize>) {
        let Some(l) = &self.loaded else { return };
        let mut opts = self.export.opts.clone();
        opts.pdf_jpeg = if self.export.jpeg { Some(self.export.quality) } else { None };
        let stem = Path::new(&l.name).file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| "printout".into());
        let Some(base) = rfd::FileDialog::new().set_title("Export to").set_file_name(format!("{stem}.{}", opts.format.ext())).save_file() else { return };
        let (doc, settings) = (l.doc.clone(), self.settings.clone());
        let job = ExportJob { done: Arc::new(AtomicUsize::new(0)), total: pages.len(), cancel: Arc::new(AtomicBool::new(false)), result: Arc::new(Mutex::new(None)) };
        let (done, cancel, result) = (job.done.clone(), job.cancel.clone(), job.result.clone());
        let ctx = ctx.clone();
        self.export.message.clear();
        self.export.job = Some(job);
        std::thread::spawn(move || {
            let res = (|| -> Result<Vec<PathBuf>, String> {
                let mut gpu = Renderer::headless().ok_or("no GPU available for export")?;
                let sheets: Vec<SheetBuf> = pages.iter().map(|&i| gpu.upload(build_sheet(&doc.pages[i], &doc, &settings))).collect();
                let refs: Vec<&SheetBuf> = sheets.iter().collect();
                export_sheets(&mut gpu, &refs, &settings, &opts, &base, |p, _| done.store(p, Ordering::Relaxed), || !cancel.load(Ordering::Relaxed)).map_err(|e| e.to_string())
            })();
            *result.lock().unwrap() = Some(res);
            ctx.request_repaint();
        });
    }
}

fn covers(have: &Region, need: &Region) -> bool {
    have.x <= need.x && have.y <= need.y && have.x + have.w as i32 >= need.x + need.w as i32 && have.y + have.h as i32 >= need.y + need.h as i32
}

fn ranked_printers(data: &[u8]) -> Vec<&'static str> {
    let mut s = score_printers(data).to_vec();
    s.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    s.into_iter().map(|(k, _)| k).collect()
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll();
        for f in ctx.input(|i| i.raw.dropped_files.clone()) {
            if let Some(p) = &f.path {
                self.open_path(ctx, p);
            } else if let Some(b) = &f.bytes {
                let name = if f.name.is_empty() { "dropped file".to_owned() } else { f.name.clone() };
                self.printer_order = ranked_printers(b);
                self.start_emulate(ctx, name, Arc::new(b.to_vec()));
            }
        }
        if ctx.input(|i| i.key_pressed(egui::Key::F5)) {
            self.reemulate(ctx);
        }
        if ctx.input(|i| i.key_pressed(egui::Key::F8)) {
            self.try_next_printer(ctx);
        }

        egui::TopBottomPanel::top("toolbar").show(ctx, |ui| {
            ui.horizontal(|ui| {
                if ui.button("Open…").clicked() {
                    self.pick_and_open(ctx);
                }
                if !self.recent.is_empty() {
                    ui.menu_button("Recent", |ui| {
                        for p in self.recent.clone() {
                            if ui.button(p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default()).on_hover_text(p.display().to_string()).clicked() {
                                self.open_path(ctx, &p);
                                ui.close();
                            }
                        }
                    });
                }
                if ui.button("Reload (F5)").clicked() {
                    self.reemulate(ctx);
                }
                if ui.add_enabled(self.loaded.is_some(), egui::Button::new("Export…")).clicked() {
                    self.export.open = true;
                }
                ui.separator();
                if ui.button("−").clicked() {
                    self.zoom = (self.zoom / 1.25).max(MIN_ZOOM);
                    self.fit = Fit::None;
                }
                ui.label(format!("{:.0}%", self.zoom * 100.0));
                if ui.button("+").clicked() {
                    self.zoom = (self.zoom * 1.25).min(MAX_ZOOM);
                    self.fit = Fit::None;
                }
                if ui.button("Fit width").clicked() {
                    self.fit = Fit::Width;
                }
                if ui.button("Fit page").clicked() {
                    self.fit = Fit::Page;
                }
                if ui.button("100%").clicked() {
                    self.zoom = 1.0;
                    self.fit = Fit::None;
                }
                ui.separator();
                if let Some(cur) = self.current_page() {
                    if ui.button("Hide page").on_hover_text("Fold this page away (it is not exported)").clicked() {
                        if let Some(l) = self.loaded.as_mut() {
                            l.pages.hide(&[cur]);
                        }
                    }
                    if ui.button("Delete page").clicked() {
                        if let Some(l) = self.loaded.as_mut() {
                            l.pages.delete(&[cur]);
                        }
                    }
                }
                if let Some(l) = self.loaded.as_mut() {
                    let mut hf = l.pages.hide_flagged;
                    if ui.checkbox(&mut hf, "Fold away blank / garbled pages").changed() {
                        l.pages.set_hide_flagged(hf);
                    }
                }
                if self.busy {
                    ui.spinner();
                }
            });
        });

        egui::SidePanel::left("settings").resizable(true).default_width(300.0).show(ctx, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| self.settings_ui(ui, ctx));
        });

        egui::TopBottomPanel::bottom("status").show(ctx, |ui| {
            ui.horizontal(|ui| {
                if let Some(l) = &self.loaded {
                    ui.label(format!(
                        "{} · {} bytes · {} page(s) · {} dots{}",
                        l.name,
                        thousands(l.raw.len()),
                        l.doc.pages.len(),
                        thousands(l.doc.pages.iter().map(|p| p.n_dots()).sum()),
                        if self.status.is_empty() { String::new() } else { format!(" · {}", self.status) }
                    ));
                    for n in l.doc.warnings.iter().chain(l.doc.fixes.iter()).take(1) {
                        ui.colored_label(Color32::from_rgb(230, 170, 60), n);
                    }
                }
                if !self.error.is_empty() {
                    ui.colored_label(Color32::LIGHT_RED, &self.error);
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label("drag to pan · wheel to scroll · Ctrl+wheel to zoom · drop a file to open");
                });
            });
        });

        egui::CentralPanel::default().frame(egui::Frame::new()).show(ctx, |ui| self.canvas_ui(ui, ctx));
        self.export_ui(ctx);
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        storage.set_string("settings", self.settings.to_kv());
        storage.set_string("recent", self.recent.iter().map(|p| p.display().to_string()).collect::<Vec<_>>().join("\n"));
    }
}

fn show_error(msg: &str) {
    eprintln!("{msg}");
    let _ = std::fs::write("retroimpact-error.txt", msg);
    let _ = rfd::MessageDialog::new().set_level(rfd::MessageLevel::Error).set_title("RetroImpact").set_description(msg).show();
}

pub fn run() {
    std::panic::set_hook(Box::new(|info| {
        show_error(&format!("RetroImpact crashed:\n{info}"));
    }));
    let options = eframe::NativeOptions {
        renderer: eframe::Renderer::Wgpu,
        viewport: egui::ViewportBuilder::default().with_inner_size([1360.0, 880.0]).with_title("RetroImpact"),
        ..Default::default()
    };
    let result = eframe::run_native("RetroImpact", options, Box::new(|cc| Ok(Box::new(App::new(cc)))));
    if let Err(e) = result {
        show_error(&format!("RetroImpact failed to start:\n{e}"));
    }
}
