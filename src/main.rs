#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")] // no console window in release

use eframe::egui::{
    self, Align2, Color32, ColorImage, FontId, Key, KeyboardShortcut, Modifiers, Pos2, Rect,
    TextureHandle, TextureOptions, Vec2, ViewportCommand, WindowLevel,
};
use pdfium_render::prelude::*;
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

const PAGE_GAP: f32 = 10.0;
const MARGIN: f32 = 10.0;
const MAX_RENDERS_PER_FRAME: usize = 2; // keeps scrolling responsive on heavy pages
const MAX_TEXTURE_PX: usize = 8192;
const MIN_ZOOM: f32 = 0.25;
const MAX_ZOOM: f32 = 6.0;
const ARROW_SCROLL_STEP: f32 = 60.0; // points scrolled per Up/Down press (repeats while held)

#[cfg(windows)]
const OPEN_WITH_ADDED: &str = "Added. Right-click a PDF → Open with → deruPDF.\n\
     To make it the default, pick deruPDF there and choose \"Always\".";
#[cfg(windows)]
const OPEN_WITH_REMOVED: &str = "Removed deruPDF from \"Open with\".";

const BG_MAIN: Color32 = Color32::from_rgb(0x00, 0x00, 0x00);

/// Black-based dark theme so the toolbars and widgets match the black page background.
fn apply_theme(ctx: &egui::Context) {
    let g = Color32::from_gray;
    let stroke = |width: f32, color: Color32| egui::Stroke::new(width, color);
    let mut v = egui::Visuals::dark();

    v.panel_fill = g(10);
    v.window_fill = g(14);
    v.window_stroke = stroke(1.0, g(36));
    v.faint_bg_color = g(14);
    v.extreme_bg_color = g(20); // text edit background
    v.code_bg_color = g(20);

    let w = &mut v.widgets;
    w.noninteractive.bg_fill = g(10);
    w.noninteractive.weak_bg_fill = g(10);
    w.noninteractive.bg_stroke = stroke(1.0, g(32)); // separators, panel borders
    w.noninteractive.fg_stroke = stroke(1.0, g(175));

    w.inactive.bg_fill = g(26);
    w.inactive.weak_bg_fill = g(26);
    w.inactive.fg_stroke = stroke(1.0, g(200));

    w.hovered.bg_fill = g(40);
    w.hovered.weak_bg_fill = g(40);
    w.hovered.bg_stroke = stroke(1.0, g(80));
    w.hovered.fg_stroke = stroke(1.5, g(235));

    w.active.bg_fill = g(55);
    w.active.weak_bg_fill = g(55);
    w.active.bg_stroke = stroke(1.0, g(110));
    w.active.fg_stroke = stroke(2.0, Color32::WHITE);

    w.open.bg_fill = g(20);
    w.open.weak_bg_fill = g(20);
    w.open.bg_stroke = stroke(1.0, g(50));

    ctx.set_visuals(v);
}

fn main() -> eframe::Result {
    let initial_file = std::env::args_os().nth(1).map(PathBuf::from);

    // `deruPDF.exe --register` / `--unregister`: add or remove the "Open with" entry and exit
    #[cfg(windows)]
    if let Some(flag) = initial_file.as_deref().and_then(|p| p.to_str()) {
        let result = match flag {
            "--register" => Some(open_with::register().map(|()| OPEN_WITH_ADDED)),
            "--unregister" => Some(open_with::unregister().map(|()| OPEN_WITH_REMOVED)),
            _ => None,
        };
        if let Some(result) = result {
            let (level, text) = match result {
                Ok(msg) => (rfd::MessageLevel::Info, msg.to_string()),
                Err(e) => (rfd::MessageLevel::Error, e),
            };
            rfd::MessageDialog::new()
                .set_title("deruPDF")
                .set_description(text)
                .set_level(level)
                .show();
            return Ok(());
        }
    }

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("deruPDF")
            .with_inner_size([520.0, 760.0])
            .with_min_inner_size([260.0, 200.0])
            .with_window_level(WindowLevel::AlwaysOnTop)
            .with_drag_and_drop(true)
            .with_icon(
                eframe::icon_data::from_png_bytes(include_bytes!("../assets/icon.png"))
                    .expect("bundled icon is a valid png"),
            ),
        ..Default::default()
    };

    eframe::run_native(
        "deruPDF",
        options,
        Box::new(move |cc| Ok(Box::new(App::new(cc, initial_file)))),
    )
}

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

struct Doc {
    pdf: PdfDocument<'static>,
    /// page sizes in PDF points (1/72 inch)
    sizes: Vec<Vec2>,
    max_width: f32,
}

struct PageTexture {
    tex: TextureHandle,
    px_width: usize,
}

struct Hit {
    page: usize,
    rects: Vec<Rect>,
}

struct App {
    pdfium: Option<&'static Pdfium>,
    doc: Option<Doc>,
    error: Option<String>,
    textures: HashMap<usize, PageTexture>,

    pinned: bool,

    /// 1.0 = page width fits the window
    zoom: f32,
    last_zoom_change: Instant,

    // find bar
    find_open: bool,
    focus_find: bool,
    query: String,
    searched_query: String,
    hits: Vec<Hit>,
    current_hit: Option<usize>,
    scroll_to_hit: bool,

    // scrolling
    current_page: usize,
    last_offset_y: f32,
    last_total_h: f32,
    last_view_h: f32,
    pending_scroll_y: Option<f32>,
    pending_center_frac: Option<f32>,
    pending_page: Option<usize>,

    // "Open with" registration, shown on the start screen
    #[cfg(windows)]
    open_with_registered: bool,
    #[cfg(windows)]
    open_with_msg: Option<String>,
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>, initial_file: Option<PathBuf>) -> Self {
        // handle Ctrl +/-/0 themselves (zoom the page, not the UI)
        cc.egui_ctx.options_mut(|o| o.zoom_with_keyboard = false);
        apply_theme(&cc.egui_ctx);
        #[cfg(windows)]
        load_system_font(&cc.egui_ctx);

        let (pdfium, error) = match load_pdfium() {
            // pdfium lives for the whole program, leaking it gives documents a static lifetime
            Ok(p) => (Some(&*Box::leak(Box::new(p))), None),
            Err(e) => (None, Some(e)),
        };

        let mut app = Self {
            pdfium,
            doc: None,
            error,
            textures: HashMap::new(),
            pinned: true,
            zoom: 1.0,
            last_zoom_change: Instant::now(),
            find_open: false,
            focus_find: false,
            query: String::new(),
            searched_query: String::new(),
            hits: Vec::new(),
            current_hit: None,
            scroll_to_hit: false,
            current_page: 0,
            last_offset_y: 0.0,
            last_total_h: 1.0,
            last_view_h: 1.0,
            pending_scroll_y: None,
            pending_center_frac: None,
            pending_page: None,
            #[cfg(windows)]
            open_with_registered: open_with::is_registered(),
            #[cfg(windows)]
            open_with_msg: None,
        };

        if let Some(path) = initial_file {
            app.open(&cc.egui_ctx, path);
        }
        app
    }

    fn open(&mut self, ctx: &egui::Context, path: PathBuf) {
        let Some(pdfium) = self.pdfium else { return };

        match pdfium.load_pdf_from_file(&path, None) {
            Ok(pdf) => {
                let count = pdf.pages().len() as usize;
                let sizes: Vec<Vec2> = (0..count)
                    .map(|i| {
                        pdf.pages()
                            .get(i as _)
                            .map(|p| Vec2::new(p.width().value, p.height().value))
                            .unwrap_or(Vec2::new(612.0, 792.0))
                    })
                    .collect();
                let max_width = sizes.iter().map(|s| s.x).fold(1.0, f32::max);

                self.doc = Some(Doc {
                    pdf,
                    sizes,
                    max_width,
                });
                self.error = None;
                self.textures.clear();
                self.hits.clear();
                self.current_hit = None;
                self.searched_query.clear();
                self.current_page = 0;
                self.pending_scroll_y = Some(0.0);

                let name = path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                ctx.send_viewport_cmd(ViewportCommand::Title(format!("{name} — deruPDF")));
            }
            Err(e) => {
                self.error = Some(format!("Couldn't open {}:\n{e:?}", path.display()));
            }
        }
    }

    fn open_dialog(&mut self, ctx: &egui::Context) {
        if let Some(path) = rfd::FileDialog::new()
            .add_filter("PDF", &["pdf"])
            .pick_file()
        {
            self.open(ctx, path);
        }
    }

    fn set_pinned(&mut self, ctx: &egui::Context, pinned: bool) {
        self.pinned = pinned;
        let level = if pinned {
            WindowLevel::AlwaysOnTop
        } else {
            WindowLevel::Normal
        };
        ctx.send_viewport_cmd(ViewportCommand::WindowLevel(level));
    }

    fn set_zoom(&mut self, zoom: f32) {
        let zoom = zoom.clamp(MIN_ZOOM, MAX_ZOOM);
        if (zoom - self.zoom).abs() < 1e-4 {
            return;
        }
        // keep whatever is in the middle of the window in the middle after zooming
        let center = self.last_offset_y + self.last_view_h / 2.0;
        self.pending_center_frac = Some(center / self.last_total_h.max(1.0));
        self.zoom = zoom;
        self.last_zoom_change = Instant::now();
    }

    fn run_search(&mut self) {
        self.hits.clear();
        self.current_hit = None;
        self.searched_query = self.query.clone();

        let Some(doc) = &self.doc else { return };
        let query = self.query.trim();
        if query.is_empty() {
            return;
        }

        let options = PdfSearchOptions::new(); // case insensitive, partial words
        'pages: for (i, size) in doc.sizes.iter().enumerate() {
            let Ok(page) = doc.pdf.pages().get(i as _) else {
                continue;
            };
            let Ok(text) = page.text() else { continue };
            let Ok(search) = text.search(query, &options) else {
                continue;
            };
            while let Some(segments) = search.find_next() {
                let rects: Vec<Rect> = segments
                    .iter()
                    .map(|seg| {
                        let b = seg.bounds();
                        // pdf origin is bottom-left, flip to top left
                        Rect::from_min_max(
                            Pos2::new(b.left().value, size.y - b.top().value),
                            Pos2::new(b.right().value, size.y - b.bottom().value),
                        )
                    })
                    .collect();
                if !rects.is_empty() {
                    self.hits.push(Hit { page: i, rects });
                }
                if self.hits.len() >= 20_000 {
                    break 'pages;
                }
            }
        }

        if !self.hits.is_empty() {
            // start from the first match at or after the page currently seen
            let start = self
                .hits
                .iter()
                .position(|h| h.page >= self.current_page)
                .unwrap_or(0);
            self.current_hit = Some(start);
            self.scroll_to_hit = true;
        }
    }

    fn step_hit(&mut self, dir: i32) {
        if self.hits.is_empty() {
            return;
        }
        let n = self.hits.len() as i32;
        let cur = self.current_hit.map(|c| c as i32).unwrap_or(-1);
        self.current_hit = Some((cur + dir).rem_euclid(n) as usize);
        self.scroll_to_hit = true;
    }

    fn find_next(&mut self, dir: i32) {
        if self.query != self.searched_query {
            self.run_search();
        } else {
            self.step_hit(dir);
        }
    }

    fn close_find(&mut self) {
        self.find_open = false;
        self.hits.clear();
        self.current_hit = None;
        self.searched_query.clear();
    }

    fn handle_input(&mut self, ctx: &egui::Context) {
        // files dropped onto the window
        let dropped = ctx.input(|i| i.raw.dropped_files.iter().find_map(|f| f.path.clone()));
        if let Some(path) = dropped {
            self.open(ctx, path);
        }

        let cmd = |key: Key| KeyboardShortcut::new(Modifiers::COMMAND, key);
        let (open, find, pin, zoom_in, zoom_in2, zoom_out, zoom_reset) = ctx.input_mut(|i| {
            (
                i.consume_shortcut(&cmd(Key::O)),
                i.consume_shortcut(&cmd(Key::F)),
                i.consume_shortcut(&cmd(Key::T)),
                i.consume_shortcut(&cmd(Key::Plus)),
                i.consume_shortcut(&cmd(Key::Equals)),
                i.consume_shortcut(&cmd(Key::Minus)),
                i.consume_shortcut(&cmd(Key::Num0)),
            )
        });

        if open {
            self.open_dialog(ctx);
        }
        if find {
            self.find_open = true;
            self.focus_find = true;
        }
        if pin {
            self.set_pinned(ctx, !self.pinned);
        }
        if zoom_in || zoom_in2 {
            self.set_zoom(self.zoom * 1.15);
        }
        if zoom_out {
            self.set_zoom(self.zoom / 1.15);
        }
        if zoom_reset {
            self.set_zoom(1.0);
        }

        // ctrl + mouse wheel / trackpad pinch
        let zoom_delta = ctx.input(|i| i.zoom_delta());
        if (zoom_delta - 1.0).abs() > 1e-4 {
            self.set_zoom(self.zoom * zoom_delta);
        }

        let (f3, shift, esc) = ctx.input(|i| {
            (
                i.key_pressed(Key::F3),
                i.modifiers.shift,
                i.key_pressed(Key::Escape),
            )
        });
        if f3 {
            self.find_next(if shift { -1 } else { 1 });
        }
        if esc && self.find_open {
            self.close_find();
        }

        // page navigation (only when not typing in the find box)
        if !ctx.wants_keyboard_input() {
            if let Some(doc) = &self.doc {
                let last = doc.sizes.len().saturating_sub(1);
                let (pg_down, pg_up, home, end, up, down) = ctx.input(|i| {
                    (
                        i.key_pressed(Key::PageDown) || i.key_pressed(Key::ArrowRight),
                        i.key_pressed(Key::PageUp) || i.key_pressed(Key::ArrowLeft),
                        i.key_pressed(Key::Home),
                        i.key_pressed(Key::End),
                        i.key_pressed(Key::ArrowUp),
                        i.key_pressed(Key::ArrowDown),
                    )
                });
                if pg_down {
                    self.pending_page = Some((self.current_page + 1).min(last));
                }
                if pg_up {
                    self.pending_page = Some(self.current_page.saturating_sub(1));
                }
                if up || down {
                    let step = if down { ARROW_SCROLL_STEP } else { -ARROW_SCROLL_STEP };
                    let max = (self.last_total_h - self.last_view_h).max(0.0);
                    self.pending_scroll_y = Some((self.last_offset_y + step).clamp(0.0, max));
                }
                if home {
                    self.pending_page = Some(0);
                }
                if end {
                    self.pending_page = Some(last);
                }
            }
        }
    }

    /// button that adds or removes deruPDF from Explorer's "Open with" menu
    #[cfg(windows)]
    fn open_with_ui(&mut self, ui: &mut egui::Ui) {
        ui.add_space(16.0);
        let (label, hover) = if self.open_with_registered {
            ("Remove from \"Open with\"", "Takes deruPDF out of the right-click menu for PDFs")
        } else {
            ("Add to \"Open with\"", "Lists deruPDF when you right-click a PDF → Open with")
        };
        if ui.button(label).on_hover_text(hover).clicked() {
            let result = if self.open_with_registered {
                open_with::unregister().map(|()| OPEN_WITH_REMOVED)
            } else {
                open_with::register().map(|()| OPEN_WITH_ADDED)
            };
            self.open_with_msg = Some(match result {
                Ok(msg) => msg.to_string(),
                Err(e) => e,
            });
            self.open_with_registered = open_with::is_registered();
        }
        if let Some(msg) = &self.open_with_msg {
            ui.add_space(6.0);
            ui.weak(msg);
        }
    }

    fn toolbar(&mut self, ctx: &egui::Context) {
        let mut want_open = false;
        egui::TopBottomPanel::top("toolbar").show(ctx, |ui| {
            ui.horizontal(|ui| {
                if ui.button("Open").on_hover_text("Ctrl+O").clicked() {
                    want_open = true;
                }

                let mut pinned = self.pinned;
                if ui
                    .toggle_value(&mut pinned, "Pin on top")
                    .on_hover_text("Ctrl+T")
                    .changed()
                {
                    self.set_pinned(ctx, pinned);
                }

                ui.separator();
                if ui.button("-").on_hover_text("Ctrl+-").clicked() {
                    self.set_zoom(self.zoom / 1.15);
                }
                ui.label(format!("{:.0}%", self.zoom * 100.0));
                if ui.button("+").on_hover_text("Ctrl++").clicked() {
                    self.set_zoom(self.zoom * 1.15);
                }
                if ui.button("Fit").on_hover_text("Ctrl+0").clicked() {
                    self.set_zoom(1.0);
                }

                if let Some(doc) = &self.doc {
                    ui.separator();
                    ui.label(format!("{} / {}", self.current_page + 1, doc.sizes.len()));
                }

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("Find").on_hover_text("Ctrl+F").clicked() {
                        self.find_open = true;
                        self.focus_find = true;
                    }
                });
            });
        });
        if want_open {
            self.open_dialog(ctx);
        }
    }

    fn find_bar(&mut self, ctx: &egui::Context) {
        if !self.find_open {
            return;
        }
        egui::TopBottomPanel::top("find").show(ctx, |ui| {
            ui.horizontal(|ui| {
                let resp = ui.add(
                    egui::TextEdit::singleline(&mut self.query)
                        .hint_text("Find in document")
                        .desired_width(200.0),
                );
                if self.focus_find {
                    resp.request_focus();
                    self.focus_find = false;
                }

                let enter = resp.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
                if enter {
                    let shift = ui.input(|i| i.modifiers.shift);
                    self.find_next(if shift { -1 } else { 1 });
                    resp.request_focus(); // keep typing / pressing enter
                }

                if ui.button("Prev").on_hover_text("Shift+Enter").clicked() {
                    self.find_next(-1);
                }
                if ui.button("Next").on_hover_text("Enter").clicked() {
                    self.find_next(1);
                }

                if !self.searched_query.is_empty() {
                    if self.hits.is_empty() {
                        ui.colored_label(Color32::from_rgb(230, 110, 110), "No matches");
                    } else {
                        let cur = self.current_hit.map(|c| c + 1).unwrap_or(0);
                        ui.label(format!("{cur} / {}", self.hits.len()));
                    }
                }

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("Close").on_hover_text("Esc").clicked() {
                        self.close_find();
                    }
                });
            });
        });
    }

    fn pages(&mut self, ui: &mut egui::Ui) {
        let Some(doc) = self.doc.as_ref() else { return };

        let avail = ui.available_size();
        let (scale, rects, content) = layout(&doc.sizes, doc.max_width, avail.x, self.zoom);

        let mut area = egui::ScrollArea::both().auto_shrink([false, false]);

        if let Some(frac) = self.pending_center_frac.take() {
            area = area.vertical_scroll_offset((frac * content.y - avail.y / 2.0).max(0.0));
        }
        if let Some(y) = self.pending_scroll_y.take() {
            area = area.vertical_scroll_offset(y.max(0.0));
        }
        if let Some(p) = self.pending_page.take() {
            if let Some(r) = rects.get(p) {
                area = area.vertical_scroll_offset((r.min.y - PAGE_GAP / 2.0).max(0.0));
            }
        }
        if self.scroll_to_hit {
            self.scroll_to_hit = false;
            if let Some(hit) = self.current_hit.and_then(|i| self.hits.get(i)) {
                let page = rects[hit.page];
                let first = hit.rects[0];
                let y = page.min.y + first.min.y * scale - avail.y * 0.3;
                area = area.vertical_scroll_offset(y.max(0.0));
                if content.x > avail.x {
                    let x = page.min.x + first.min.x * scale - avail.x * 0.3;
                    area = area.horizontal_scroll_offset(x.max(0.0));
                }
            }
        }

        let ctx = ui.ctx().clone();
        let ppp = ctx.pixels_per_point();
        let zoom_settled = self.last_zoom_change.elapsed() > Duration::from_millis(200);
        let textures = &mut self.textures;
        let hits = &self.hits;
        let current_hit = self.current_hit;
        let mut renders = 0;
        let mut more_to_render = false;

        let output = area.show_viewport(ui, |ui, viewport| {
            let origin = ui.max_rect().min;
            ui.set_min_size(content);
            let painter = ui.painter();

            // render one extra screen above and below so scrolling doesnt show blanks
            let prefetch = viewport.expand2(Vec2::new(0.0, viewport.height()));
            let mut first_near = usize::MAX;
            let mut last_near = 0;
            let mut current = 0;

            for (i, r) in rects.iter().enumerate() {
                if r.min.y <= viewport.center().y {
                    current = i;
                }
                if !r.intersects(prefetch) {
                    continue;
                }
                first_near = first_near.min(i);
                last_near = i;

                // (re)render at the exact on screen pixel width for crisp text
                let target_px = ((r.width() * ppp).round() as usize).clamp(16, MAX_TEXTURE_PX);
                let needs_render = match textures.get(&i) {
                    None => true,
                    // while zooming, stretch the old bitmap, rerender once zoom settles
                    Some(t) => {
                        zoom_settled
                            && (t.px_width as f32 - target_px as f32).abs() / target_px as f32
                                > 0.02
                    }
                };
                if needs_render {
                    if renders < MAX_RENDERS_PER_FRAME {
                        if let Some(tex) = render_page(&ctx, doc, i, target_px) {
                            textures.insert(
                                i,
                                PageTexture {
                                    tex,
                                    px_width: target_px,
                                },
                            );
                        }
                        renders += 1;
                    } else {
                        more_to_render = true;
                    }
                }

                if !r.intersects(viewport) {
                    continue;
                }

                let screen = r.translate(origin.to_vec2());
                painter.rect_filled(screen.expand(1.0), 0.0, Color32::from_gray(45));
                painter.rect_filled(screen, 0.0, Color32::WHITE);
                match textures.get(&i) {
                    Some(t) => {
                        painter.image(
                            t.tex.id(),
                            screen,
                            Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                            Color32::WHITE,
                        );
                    }
                    None => {
                        painter.text(
                            screen.center(),
                            Align2::CENTER_CENTER,
                            format!("Page {}", i + 1),
                            FontId::proportional(14.0),
                            Color32::GRAY,
                        );
                    }
                }

                // search highlights
                for (hi, hit) in hits.iter().enumerate().filter(|(_, h)| h.page == i) {
                    let color = if Some(hi) == current_hit {
                        Color32::from_rgba_unmultiplied(255, 120, 0, 130)
                    } else {
                        Color32::from_rgba_unmultiplied(255, 220, 0, 90)
                    };
                    for hr in &hit.rects {
                        let rr = Rect::from_min_max(
                            screen.min + hr.min.to_vec2() * scale,
                            screen.min + hr.max.to_vec2() * scale,
                        );
                        painter.rect_filled(rr.expand(1.0), 2.0, color);
                    }
                }
            }
            (first_near, last_near, current)
        });

        let (first_near, last_near, current) = output.inner;
        if first_near != usize::MAX {
            // free gpu memory for pages far from view
            textures.retain(|&k, _| k + 3 >= first_near && k <= last_near + 3);
        }
        self.current_page = current;
        self.last_offset_y = output.state.offset.y;
        self.last_total_h = content.y;
        self.last_view_h = avail.y;

        if more_to_render {
            ctx.request_repaint();
        } else if !zoom_settled {
            ctx.request_repaint_after(Duration::from_millis(220));
        }
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.handle_input(ctx);
        self.toolbar(ctx);
        self.find_bar(ctx);

        egui::CentralPanel::default()
            .frame(egui::Frame::default().fill(BG_MAIN))
            .show(ctx, |ui| {
                if let Some(err) = &self.error {
                    ui.centered_and_justified(|ui| {
                        ui.colored_label(Color32::from_rgb(240, 130, 130), err);
                    });
                } else if self.doc.is_none() {
                    ui.add_space((ui.available_height() / 2.0 - 40.0).max(0.0));
                    ui.vertical_centered(|ui| {
                        ui.label("Drop a PDF here, or press Ctrl+O");
                        #[cfg(windows)]
                        self.open_with_ui(ui);
                    });
                } else {
                    self.pages(ui);
                }
            });
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// lays pages out in a vertical strip. returns (points to screen scale, page rects, content size)
fn layout(sizes: &[Vec2], max_width: f32, avail_w: f32, zoom: f32) -> (f32, Vec<Rect>, Vec2) {
    let scale = ((avail_w - 2.0 * MARGIN).max(80.0) * zoom) / max_width.max(1.0);
    let content_w = (max_width * scale + 2.0 * MARGIN).max(avail_w);
    let mut y = MARGIN;
    let rects = sizes
        .iter()
        .map(|s| {
            let size = *s * scale;
            let r = Rect::from_min_size(Pos2::new((content_w - size.x) / 2.0, y), size);
            y += size.y + PAGE_GAP;
            r
        })
        .collect();
    (scale, rects, Vec2::new(content_w, y - PAGE_GAP + MARGIN))
}

fn render_page(
    ctx: &egui::Context,
    doc: &Doc,
    index: usize,
    px_width: usize,
) -> Option<TextureHandle> {
    let page = doc.pdf.pages().get(index as _).ok()?;
    let config = PdfRenderConfig::new()
        .set_target_width(px_width as _)
        .render_form_data(true)
        .render_annotations(true);
    let bitmap = page.render_with_config(&config).ok()?;
    let (w, h) = (bitmap.width() as usize, bitmap.height() as usize);
    let rgba = bitmap.as_rgba_bytes();
    if w == 0 || h == 0 || rgba.len() != w * h * 4 {
        return None;
    }
    let image = ColorImage::from_rgba_unmultiplied([w, h], &rgba);
    Some(ctx.load_texture(format!("page-{index}"), image, TextureOptions::LINEAR))
}

/// finds the pdfium library: next to the executable, then the working directory, then the system
fn load_pdfium() -> Result<Pdfium, String> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    if let Some(dir) = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()))
    {
        dirs.push(dir);
    }
    dirs.push(PathBuf::from("./"));

    for dir in &dirs {
        if let Ok(bindings) =
            Pdfium::bind_to_library(Pdfium::pdfium_platform_library_name_at_path(dir))
        {
            return Ok(Pdfium::new(bindings));
        }
    }
    Pdfium::bind_to_system_library()
        .map(Pdfium::new)
        .map_err(|e| {
            format!(
                "Couldn't load the PDFium library ({e:?}).\n\n\
                 Put pdfium.dll (Windows), libpdfium.so (Linux) or libpdfium.dylib (macOS) \
                 next to the deruPDF executable.\n\
                 Download: github.com/bblanchon/pdfium-binaries/releases"
            )
        })
}

/// Windows builds don't bundle egui's fonts (saves ~1.4 MB); use a font every Windows install has.
#[cfg(windows)]
fn load_system_font(ctx: &egui::Context) {
    let windir = std::env::var("WINDIR").unwrap_or_else(|_| "C:/Windows".into());
    let Some(bytes) = ["segoeui.ttf", "arial.ttf", "tahoma.ttf"]
        .iter()
        .find_map(|name| std::fs::read(format!("{windir}/Fonts/{name}")).ok())
    else {
        return;
    };
    let mut fonts = egui::FontDefinitions::empty();
    fonts
        .font_data
        .insert("ui".into(), std::sync::Arc::new(egui::FontData::from_owned(bytes)));
    for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
        fonts.families.entry(family).or_default().push("ui".into());
    }
    ctx.set_fonts(fonts);
}

// ---------------------------------------------------------------------------
// "Open with" registration (Windows)
// ---------------------------------------------------------------------------

/// Lists deruPDF in Explorer's right-click "Open with" menu for .pdf files.
/// Everything lives under HKEY_CURRENT_USER, so no admin rights are needed.
/// Windows doesn't let apps make themselves the default; the user picks deruPDF once
/// in the "Open with" dialog and ticks "Always".
#[cfg(windows)]
mod open_with {
    use std::ffi::OsStr;
    use std::os::windows::ffi::OsStrExt;
    use std::ptr::{null, null_mut};
    use windows_sys::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS};
    use windows_sys::Win32::System::Registry::{
        RegCloseKey, RegCreateKeyExW, RegDeleteKeyValueW, RegDeleteTreeW, RegGetValueW,
        RegSetValueExW, HKEY, HKEY_CURRENT_USER, KEY_SET_VALUE, REG_OPTION_NON_VOLATILE, REG_SZ,
        RRF_RT_REG_SZ,
    };
    use windows_sys::Win32::UI::Shell::{SHChangeNotify, SHCNE_ASSOCCHANGED, SHCNF_IDLIST};

    const PROG_ID: &str = "deruPDF.pdf";
    const CLASSES: &str = r"Software\Classes";

    fn wide(s: impl AsRef<OsStr>) -> Vec<u16> {
        s.as_ref().encode_wide().chain(Some(0)).collect()
    }

    /// (full command line Explorer should run, the exe's file name)
    fn exe() -> Result<(String, String), String> {
        let path = std::env::current_exe().map_err(|e| format!("Couldn't find deruPDF.exe: {e}"))?;
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .ok_or("Couldn't find deruPDF.exe")?;
        Ok((format!("\"{}\"", path.display()), name))
    }

    fn app_key(exe_name: &str) -> String {
        format!(r"{CLASSES}\Applications\{exe_name}")
    }

    /// writes a string value, creating the key if needed. `name: None` is the key's default value.
    fn set(key: &str, name: Option<&str>, value: &str) -> Result<(), String> {
        let name = name.map(wide);
        let data = wide(value);
        unsafe {
            let mut hkey: HKEY = null_mut();
            let err = RegCreateKeyExW(
                HKEY_CURRENT_USER,
                wide(key).as_ptr(),
                0,
                null(),
                REG_OPTION_NON_VOLATILE,
                KEY_SET_VALUE,
                null(),
                &mut hkey,
                null_mut(),
            );
            if err != ERROR_SUCCESS {
                return Err(format!(r"Couldn't write HKCU\{key} (error {err})"));
            }
            let err = RegSetValueExW(
                hkey,
                name.as_ref().map_or(null(), |n| n.as_ptr()),
                0,
                REG_SZ,
                data.as_ptr().cast(),
                (data.len() * 2) as u32,
            );
            RegCloseKey(hkey);
            if err != ERROR_SUCCESS {
                return Err(format!(r"Couldn't write HKCU\{key} (error {err})"));
            }
        }
        Ok(())
    }

    /// reads a key's default string value
    fn get_default(key: &str) -> Option<String> {
        let key = wide(key);
        let mut buf = vec![0u16; 1024];
        let mut bytes = (buf.len() * 2) as u32;
        let err = unsafe {
            RegGetValueW(
                HKEY_CURRENT_USER,
                key.as_ptr(),
                null(),
                RRF_RT_REG_SZ,
                null_mut(),
                buf.as_mut_ptr().cast(),
                &mut bytes,
            )
        };
        if err != ERROR_SUCCESS {
            return None;
        }
        let len = (bytes as usize / 2).saturating_sub(1); // drop the trailing nul
        Some(String::from_utf16_lossy(&buf[..len]))
    }

    fn ok_or_missing(err: u32, what: &str) -> Result<(), String> {
        if err == ERROR_SUCCESS || err == ERROR_FILE_NOT_FOUND {
            Ok(())
        } else {
            Err(format!(r"Couldn't remove HKCU\{what} (error {err})"))
        }
    }

    /// tells Explorer the file associations changed, so menus refresh without a sign-out
    fn notify_shell() {
        unsafe { SHChangeNotify(SHCNE_ASSOCCHANGED as i32, SHCNF_IDLIST, null(), null()) };
    }

    /// true when the "Open with" entry exists and points at this exe (false after the exe moved)
    pub fn is_registered() -> bool {
        let Ok((exe, name)) = exe() else { return false };
        get_default(&format!(r"{}\shell\open\command", app_key(&name)))
            .is_some_and(|cmd| cmd == format!("{exe} \"%1\""))
    }

    pub fn register() -> Result<(), String> {
        let (exe, name) = exe()?;
        let command = format!("{exe} \"%1\"");
        let icon = format!("{exe},0");
        let app = app_key(&name);
        let prog = format!(r"{CLASSES}\{PROG_ID}");

        // the application itself, listed in "Open with" for the types it supports
        set(&app, Some("FriendlyAppName"), "deruPDF")?;
        set(&format!(r"{app}\DefaultIcon"), None, &icon)?;
        set(&format!(r"{app}\SupportedTypes"), Some(".pdf"), "")?;
        set(&format!(r"{app}\shell\open\command"), None, &command)?;

        // a ProgID, so .pdf files show deruPDF's icon once it's chosen as the default
        set(&prog, None, "PDF Document")?;
        set(&prog, Some("FriendlyTypeName"), "PDF Document")?;
        set(&format!(r"{prog}\DefaultIcon"), None, &icon)?;
        set(&format!(r"{prog}\shell\open\command"), None, &command)?;
        set(&format!(r"{CLASSES}\.pdf\OpenWithProgids"), Some(PROG_ID), "")?;

        notify_shell();
        Ok(())
    }

    pub fn unregister() -> Result<(), String> {
        let (_, name) = exe()?;
        let app = app_key(&name);
        let prog = format!(r"{CLASSES}\{PROG_ID}");
        let progids = format!(r"{CLASSES}\.pdf\OpenWithProgids");
        unsafe {
            ok_or_missing(RegDeleteTreeW(HKEY_CURRENT_USER, wide(&app).as_ptr()), &app)?;
            ok_or_missing(RegDeleteTreeW(HKEY_CURRENT_USER, wide(&prog).as_ptr()), &prog)?;
            ok_or_missing(
                RegDeleteKeyValueW(
                    HKEY_CURRENT_USER,
                    wide(&progids).as_ptr(),
                    wide(PROG_ID).as_ptr(),
                ),
                &progids,
            )?;
        }
        notify_shell();
        Ok(())
    }
}
