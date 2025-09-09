use eframe::{App, egui};
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

struct VideoEditorApp {
    frames: Arc<Mutex<Vec<usize>>>, // frame index のみ保持
    color_cache: Arc<Mutex<HashMap<usize, egui::ColorImage>>>, // 先読み ColorImage
    textures: HashMap<usize, egui::TextureHandle>, // GUIスレッド用 Texture
    current_frame: usize,
    last_frame_time: Instant,
    frame_interval: Duration,
    video_path_input: String,
    is_loading: Arc<Mutex<bool>>,
    total_frames: Arc<Mutex<usize>>,
    cache_radius: usize,
}

impl VideoEditorApp {
    fn new() -> Self {
        Self {
            frames: Arc::new(Mutex::new(Vec::new())),
            color_cache: Arc::new(Mutex::new(HashMap::new())),
            textures: HashMap::new(),
            current_frame: 0,
            last_frame_time: Instant::now(),
            frame_interval: Duration::from_millis(1000 / 60),
            video_path_input: String::new(),
            is_loading: Arc::new(Mutex::new(false)),
            total_frames: Arc::new(Mutex::new(0)),
            cache_radius: 10, // 先読みフレーム数増やす
        }
    }

    fn load_video(&mut self, path: PathBuf) {
        let is_loading = self.is_loading.clone();
        let frames = self.frames.clone();
        let color_cache = self.color_cache.clone();
        let total_frames_clone = self.total_frames.clone();

        *is_loading.lock().unwrap() = true;

        thread::spawn(move || {
            let out_dir = "frames";
            let _ = fs::remove_dir_all(out_dir);
            let _ = fs::create_dir_all(out_dir);

            let output_pattern = format!("{}/frame_%03d.jpeg", out_dir);
            let status = std::process::Command::new("ffmpeg")
                .args([
                    "-i",
                    path.to_str().unwrap(),
                    "-vf",
                    "fps=60,scale=640:-1",
                    &output_pattern,
                ])
                .status()
                .expect("ffmpeg 実行失敗");

            if !status.success() {
                eprintln!("ffmpeg 実行に失敗しました");
                *is_loading.lock().unwrap() = false;
                return;
            }

            // 出力ファイル数を数えて frames に登録
            let mut frame_index = 1;
            let mut frame_indices = Vec::new();
            loop {
                let frame_path = format!("{}/frame_{:03}.jpeg", out_dir, frame_index);
                if !PathBuf::from(&frame_path).exists() {
                    break;
                }
                // 先読み ColorImage に読み込み
                if let Ok(img) = image::open(&frame_path) {
                    let rgba = img.to_rgba8();
                    let size = [rgba.width() as usize, rgba.height() as usize];
                    let pixels = rgba.into_vec();
                    let color_img = egui::ColorImage::from_rgba_unmultiplied(size, &pixels);
                    color_cache.lock().unwrap().insert(frame_index, color_img);
                }

                frame_indices.push(frame_index);
                frame_index += 1;
            }

            *frames.lock().unwrap() = frame_indices;
            *total_frames_clone.lock().unwrap() = frame_index - 1;
            *is_loading.lock().unwrap() = false;
            println!("動画の読み込みが完了しました。総フレーム数: {}", frame_index - 1);
        });
    }

    fn get_texture(&mut self, ui: &egui::Ui, frame_index: usize) -> Option<&egui::TextureHandle> {
        if !self.textures.contains_key(&frame_index) {
            let color_cache = self.color_cache.lock().unwrap();
            if let Some(color_img) = color_cache.get(&frame_index) {
                let tex = ui.ctx().load_texture(
                    format!("video_frame_{}", frame_index),
                    color_img.clone(),
                    egui::TextureOptions::default(),
                );
                self.textures.insert(frame_index, tex);
            }
        }

        // 古いキャッシュを削除
        let min_keep = self.current_frame.saturating_sub(self.cache_radius);
        let total_frames = *self.total_frames.lock().unwrap();
        let max_keep = (self.current_frame + self.cache_radius).min(total_frames);
        self.textures.retain(|&k, _| k >= min_keep && k <= max_keep);

        self.textures.get(&frame_index)
    }

    fn draw_left_column(
        &mut self,
        ui: &mut egui::Ui,
        view_size: egui::Vec2,
        timeline_size: egui::Vec2,
    ) {
        ui.vertical(|ui| {
            let (rect, _) = ui.allocate_exact_size(view_size, egui::Sense::hover());
            ui.painter()
                .rect_filled(rect, 0.0, egui::Color32::from_rgb(240, 200, 200));

            let mut view_child_ui = ui.child_ui(
                rect,
                egui::Layout::centered_and_justified(egui::Direction::TopDown),
            );

            if self.last_frame_time.elapsed() >= self.frame_interval {
                let total_frames = *self.total_frames.lock().unwrap();
                if total_frames > 0 {
                    self.current_frame = (self.current_frame + 1) % total_frames;
                }
                self.last_frame_time = Instant::now();
            }

            if let Some(tex) = self.get_texture(&view_child_ui, self.current_frame) {
                view_child_ui.image(tex);
            } else {
                view_child_ui.label("読み込み中…");
            }

            let (rect, _) = ui.allocate_exact_size(timeline_size, egui::Sense::hover());
            ui.painter()
                .rect_filled(rect, 0.0, egui::Color32::from_rgb(200, 240, 200));

            let mut timeline_child_ui = ui.child_ui(
                rect,
                egui::Layout::centered_and_justified(egui::Direction::TopDown),
            );
            self.draw_timeline(&mut timeline_child_ui, timeline_size);
        });
    }

    fn draw_right_column(&mut self, ui: &mut egui::Ui, option_size: egui::Vec2) {
        let (rect, _) = ui.allocate_exact_size(option_size, egui::Sense::hover());
        ui.painter()
            .rect_filled(rect, 0.0, egui::Color32::from_rgb(200, 200, 240));

        let mut option_child_ui = ui.child_ui(rect, egui::Layout::top_down(egui::Align::LEFT));

        option_child_ui.vertical(|ui| {
            ui.horizontal(|ui| {
                ui.label("動画パス:");
                ui.text_edit_singleline(&mut self.video_path_input);
            });

            if ui.button("読み込み").clicked() {
                let path = PathBuf::from(self.video_path_input.clone());
                if path.exists() {
                    self.load_video(path);
                } else {
                    eprintln!("指定されたパスが存在しません: {:?}", path);
                }
            }
        });
    }

    fn draw_timeline(&mut self, ui: &mut egui::Ui, timeline_size: egui::Vec2) {
        let frame_indices = self.frames.lock().unwrap();
        if frame_indices.is_empty() {
            ui.label("タイムラインはまだありません");
            return;
        }

        ui.allocate_ui_with_layout(
            timeline_size,
            egui::Layout::left_to_right(egui::Align::TOP),
            |ui| {
                egui::ScrollArea::horizontal().show(ui, |ui| {
                    let step = 60; // サムネイル間隔
                    for &i in frame_indices.iter().step_by(step) {
                        // 先にサムネイル用 Texture を作っておく
                        let tex = if let Some(tex) = self.textures.get(&i) {
                            tex.clone()
                        } else {
                            // GUIスレッドで ColorImage から生成
                            let color_img =
                                self.color_cache.lock().unwrap().get(&i).unwrap().clone();
                            let tex = ui.ctx().load_texture(
                                format!("thumb_{}", i),
                                color_img,
                                egui::TextureOptions::default(),
                            );
                            self.textures.insert(i, tex.clone());
                            tex
                        };

                        if ui
                            .add(egui::ImageButton::new((tex.id(), egui::vec2(80.0, 45.0))))
                            .clicked()
                        {
                            self.current_frame = i;
                        }
                    }
                });
            },
        );
    }
}

impl App for VideoEditorApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // 日本語フォント設定
        use egui::FontData;
        use egui::FontDefinitions;
        use egui::FontFamily::Proportional;

        let mut fonts = FontDefinitions::default();
        fonts.font_data.insert(
            "keifont".to_owned(),
            FontData::from_static(include_bytes!("../fonts/keifont.ttf")),
        );
        fonts
            .families
            .get_mut(&Proportional)
            .unwrap()
            .insert(0, "keifont".to_owned());
        ctx.set_fonts(fonts);

        ctx.request_repaint();

        egui::CentralPanel::default().show(ctx, |ui| {
            ui.spacing_mut().item_spacing = egui::vec2(0.0, 0.0);

            let view_size = egui::vec2(860.0, 400.0);
            let timeline_size = egui::vec2(860.0, 300.0);
            let option_size = egui::vec2(400.0, 700.0);

            ui.horizontal(|ui| {
                self.draw_left_column(ui, view_size, timeline_size);
                self.draw_right_column(ui, option_size);
            });
        });
    }
}

fn main() -> Result<(), eframe::Error> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1280.0, 720.0])
            .with_resizable(false),
        ..eframe::NativeOptions::default()
    };

    let app = VideoEditorApp::new();
    eframe::run_native("RustVideoEditor", options, Box::new(|_cc| Box::new(app)))
}
