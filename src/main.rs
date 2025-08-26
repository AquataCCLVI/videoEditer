use eframe::{App, egui};
use ez_ffmpeg::FfmpegContext;
use std::fs;
use std::path::PathBuf;
use std::time::{Duration, Instant};

struct VideoEditorApp {
    frames: Vec<egui::ColorImage>,
    current_frame: usize,
    last_frame_time: Instant,
    frame_interval: Duration,
}

impl VideoEditorApp {
    fn new(video_path: &str) -> anyhow::Result<Self> {
        let mut frames = Vec::new();

        //連番ファイルを作成
        let out_dir = "frames";
        let _ = fs::create_dir_all(out_dir);

        // ffmpegに実行させる（失敗してもpanicしない）
        let ctx = FfmpegContext::builder()
            .input(video_path)
            .filter_desc("fps=30,scale=680:-1")
            .output(format!("{}/frame_%03d.jpeg", out_dir))
            .build()?;

        if let Err(e) = ctx.start().and_then(|c| c.wait()) {
            eprintln!("ffmpeg実行エラー: {e}");
        }

        // フレーム画像を順番に読み込む
        let mut frame_index = 1;
        loop {
            let path = PathBuf::from(format!("{}/frame_{:03}.jpeg", out_dir, frame_index));
            if !path.exists() {
                break; // 存在しなければ終了
            }

            match image::open(&path) {
                Ok(img) => {
                    let rgba = img.to_rgba8();
                    let size = [rgba.width() as usize, rgba.height() as usize];
                    let pixels = rgba.into_vec();
                    let color_img = egui::ColorImage::from_rgba_unmultiplied(size, &pixels);
                    frames.push(color_img);
                }
                Err(e) => eprintln!("フレーム読み込み失敗 {path:?}: {e}"),
            }

            frame_index += 1;
        }

        if frames.is_empty() {
            eprintln!("⚠️ フレームが読み込めませんでした: {video_path}");
        }

        Ok(Self {
            frames,
            current_frame: 0,
            last_frame_time: Instant::now(),
            frame_interval: Duration::from_millis(1000 / 30),
        })
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
            if !self.frames.is_empty() {
                // 経過時間でフレームを進める
                if self.last_frame_time.elapsed() >= self.frame_interval {
                    self.current_frame = (self.current_frame + 1) % self.frames.len();
                    self.last_frame_time = Instant::now();
                }

                //最終フレームまで行ったら最初に戻す
                if self.current_frame == self.frames.len() - 1 {
                    self.current_frame = 0;
                }

                let tex = view_child_ui.ctx().load_texture(
                    "video_frame",
                    self.frames[self.current_frame].clone(),
                    egui::TextureOptions::default(),
                );
                view_child_ui.image(&tex);
            } else {
                view_child_ui.label("動画フレームを読み込み中...");
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

        let mut option_child_ui = ui.child_ui(
            rect,
            egui::Layout::centered_and_justified(egui::Direction::TopDown),
        );
        option_child_ui.label("オプション");
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

    // new() が Result を返すように
    let app = VideoEditorApp::new("/home/aquata/code/videoEditer/assets/sample.mp4")
        .unwrap_or_else(|e| {
            eprintln!("アプリ初期化に失敗しました: {e}");
            VideoEditorApp {
                frames: Vec::new(),
                current_frame: 0,
                last_frame_time: Instant::now(),
                frame_interval: Duration::from_millis(1000 / 60),
            }
        });

    eframe::run_native("RustVideoEditor", options, Box::new(|_cc| Box::new(app)))
}
