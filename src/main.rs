use eframe::{App, egui};
use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

struct VideoEditorApp {
    frames: Arc<Mutex<Vec<egui::ColorImage>>>,
    current_frame: usize,
    last_frame_time: Instant,
    frame_interval: Duration,
    video_path_input: String,
    is_loading: Arc<Mutex<bool>>,
}

impl VideoEditorApp {
    fn new() -> Self {
        Self {
            frames: Arc::new(Mutex::new(Vec::new())),
            current_frame: 0,
            last_frame_time: Instant::now(),
            frame_interval: Duration::from_millis(1000 / 30),
            video_path_input: String::new(),
            is_loading: Arc::new(Mutex::new(false)),
        }
    }

    fn load_video(&mut self, path: PathBuf) {
        let frames = self.frames.clone();
        let is_loading = self.is_loading.clone();

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
                    "fps=30,scale=320:-1",
                    &output_pattern,
                ])
                .status()
                .expect("ffmpeg 実行失敗");

            if !status.success() {
                eprintln!("ffmpeg 実行に失敗しました");
                *is_loading.lock().unwrap() = false;
                return;
            }

            let mut frame_index = 1;
            loop {
                let path = PathBuf::from(format!("{}/frame_{:03}.jpeg", out_dir, frame_index));
                if !path.exists() {
                    break;
                }

                if let Ok(img) = image::open(&path) {
                    let rgba = img.to_rgba8();
                    let size = [rgba.width() as usize, rgba.height() as usize];
                    let pixels = rgba.into_vec();
                    let color_img = egui::ColorImage::from_rgba_unmultiplied(size, &pixels);

                    frames.lock().unwrap().push(color_img);
                }
                frame_index += 1;
            }

            println!("{} フレーム読み込み完了", frames.lock().unwrap().len());
            *is_loading.lock().unwrap() = false;
        });
    }

    fn draw_left_column(
        &mut self,
        ui: &mut egui::Ui,
        view_size: egui::Vec2,
        timeline_size: egui::Vec2,
    ) {
        // 左カラムの描画

        ui.vertical(|ui| {
            let (rect, _res) = ui.allocate_exact_size(view_size, egui::Sense::hover());
            ui.painter()
                .rect_filled(rect, 0.0, egui::Color32::from_rgb(240, 200, 200));

            let mut view_child_ui = ui.child_ui(
                rect,
                egui::Layout::centered_and_justified(egui::Direction::TopDown),
            );
            let frames = self.frames.lock().unwrap();
            if !frames.is_empty() {
                if self.last_frame_time.elapsed() >= self.frame_interval {
                    self.current_frame = (self.current_frame + 1) % frames.len();
                    self.last_frame_time = Instant::now();
                }

                let tex = view_child_ui.ctx().load_texture(
                    "video_frame",
                    frames[self.current_frame].clone(),
                    egui::TextureOptions::default(),
                );
                view_child_ui.image(&tex);
            } else {
                if *self.is_loading.lock().unwrap() {
                    view_child_ui.label("動画読み込み中…");
                } else {
                    view_child_ui.label("動画を選択してください");
                }
            }
            //view_child_ui.label("プレビュー画面");

            let (rect, _res) = ui.allocate_exact_size(timeline_size, egui::Sense::hover());
            ui.painter()
                .rect_filled(rect, 0.0, egui::Color32::from_rgb(200, 240, 200));

            let mut timeline_child_ui = ui.child_ui(
                rect,
                egui::Layout::centered_and_justified(egui::Direction::TopDown),
            );
            timeline_child_ui.label("タイムライン");
        });
    }

    fn draw_right_column(&mut self, ui: &mut egui::Ui, option_size: egui::Vec2) {
        // 右カラムの描画
        let (rect, _res) = ui.allocate_exact_size(option_size, egui::Sense::hover());
        ui.painter()
            .rect_filled(rect, 0.0, egui::Color32::from_rgb(200, 200, 240));

        let mut option_child_ui = ui.child_ui(rect, egui::Layout::top_down(egui::Align::LEFT));

        //option_child_ui.label("オプション");

        option_child_ui.vertical(|ui| {
            ui.horizontal(|ui| {
                ui.label("動画パス:");
                ui.text_edit_singleline(&mut self.video_path_input);
            });

            if ui.button("読み込み").clicked() {
                println!("動画パス: {}", self.video_path_input);
                let path = PathBuf::from(self.video_path_input.clone());
                if path.exists() {
                    self.load_video(path);
                } else {
                    eprintln!("指定されたパスが存在しません: {:?}", path);
                }
            }
        });
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

        //メイン画面構成
        egui::CentralPanel::default().show(ctx, |ui| {
            //UIを詰める
            ui.spacing_mut().item_spacing = egui::vec2(0.0, 0.0);
            //画面サイズを指定→eguiで使える形式に変換
            let view_size = [860.0, 400.0];
            let view_size = egui::vec2(view_size[0], view_size[1]);

            let timeline_size = [860.0, 300.0];
            let timeline_size = egui::vec2(timeline_size[0], timeline_size[1]);

            let option_size = [400.0, 700.0];
            let option_size = egui::vec2(option_size[0], option_size[1]);

            ui.horizontal(|ui| {
                //右左のカラムを関数で分ける
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
