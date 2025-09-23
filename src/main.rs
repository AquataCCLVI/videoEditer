use eframe::{App, egui};
use serde::{Deserialize, Serialize};
use serde_json;
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

#[derive(Debug, Deserialize, Serialize)]
struct FFProbeResult {
    streams: Vec<Stream>,
}

#[derive(Debug, Deserialize, Serialize)]
struct Stream {
    width: Option<u32>,
    height: Option<u32>,
    r_frame_rate: Option<String>,
    nb_frames: Option<String>,
}

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
    infomation: Arc<Mutex<Option<FFProbeResult>>>,
    start_frame: String,
    end_frame: String,
}

impl VideoEditorApp {
    fn new() -> Self {
        Self {
            frames: Arc::new(Mutex::new(Vec::new())),
            color_cache: Arc::new(Mutex::new(HashMap::new())),
            textures: HashMap::new(),
            current_frame: 0,
            last_frame_time: Instant::now(),
            frame_interval: Duration::from_millis(1000 / 30),
            video_path_input: String::new(),
            is_loading: Arc::new(Mutex::new(false)),
            total_frames: Arc::new(Mutex::new(0)),
            cache_radius: 10, // 先読みフレーム数増やす
            infomation: Arc::new(Mutex::new(None)),
            start_frame: String::new(),
            end_frame: String::new(),
        }
    }

    fn load_video(&mut self, path: PathBuf) {
        let is_loading = self.is_loading.clone();
        let frames = self.frames.clone();
        let color_cache = self.color_cache.clone();
        let total_frames_clone = self.total_frames.clone();
        let infomation_clone = self.infomation.clone();

        *is_loading.lock().unwrap() = true;

        thread::spawn(move || {
            let out_dir = "frames";
            let info_dir = "video_info";
            let _ = fs::remove_dir_all(out_dir);
            let _ = fs::remove_dir_all(info_dir);
            let _ = fs::create_dir_all(out_dir);
            let _ = fs::create_dir_all(info_dir);

            let output_pattern = format!("{}/frame_%03d.jpeg", out_dir);
            let status = std::process::Command::new("ffmpeg")
                .args([
                    "-i",
                    path.to_str().unwrap(),
                    "-vf",
                    "fps=30,scale=640:-1",
                    &output_pattern,
                ])
                .status()
                .expect("ffmpeg 実行失敗");

            if !status.success() {
                eprintln!("ffmpeg 実行に失敗しました");
                *is_loading.lock().unwrap() = false;
                return;
            }

            // ffprobe で動画情報を取得
            // スレッド内なのでselfは使えない
            if let Some(video_info) = Self::probe_video(&path) {
                // JSONファイルとして保存
                let info_path = format!("{}/info.json", info_dir);
                if let Ok(json_str) = serde_json::to_string_pretty(&video_info) {
                    let _ = fs::write(&info_path, json_str);
                }
                let mut info_lock = infomation_clone.lock().unwrap();
                *info_lock = Some(video_info);
                println!("動画情報を表示します: {:?}", *info_lock);
            } else {
                eprintln!("動画情報の取得に失敗しました");
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
        });
    }

    fn probe_video(path: &PathBuf) -> Option<FFProbeResult> {
        let output = std::process::Command::new("ffprobe")
            .args([
                "-v",
                "quiet",
                "-print_format",
                "json",
                "-show_format",
                "-show_streams",
                path.to_str().unwrap(),
            ])
            .output()
            .ok()?;

        if !output.status.success() {
            eprintln!("ffprobe 実行に失敗");
            return None;
        }

        let json_str = String::from_utf8_lossy(&output.stdout);
        serde_json::from_str(&json_str).ok()
    }

    // ffmpegで動画をトリミングする
    fn trim_video(&self, input_path: PathBuf, start: usize, end: usize) -> Result<(), String> {
        let frames = self.frames.clone();
        let color_cache = self.color_cache.clone();
        let total_frames = self.total_frames.clone();
        let is_loading = self.is_loading.clone();
        let infomation_clone = self.infomation.clone();
        let info_dir = "video_info";

        *is_loading.lock().unwrap() = true;

        thread::spawn(move || {
            if start >= end {
                eprintln!("開始フレームは終了フレームより小さくなければなりません");
                *is_loading.lock().unwrap() = false;
                return;
            }

            let out_dir = "frames";
            let _ = std::fs::remove_dir_all(out_dir);
            let _ = std::fs::create_dir_all(out_dir);

            let output_pattern = format!("{}/frame_%03d.jpeg", out_dir);

            // ffprobe で動画情報を取得
            // スレッド内なのでselfは使えない
            if let Some(video_info) = Self::probe_video(&input_path) {
                // JSONファイルとして保存
                let info_path = format!("{}/info.json", info_dir);
                if let Ok(json_str) = serde_json::to_string_pretty(&video_info) {
                    let _ = fs::write(&info_path, json_str);
                }
                let mut info_lock = infomation_clone.lock().unwrap();
                *info_lock = Some(video_info);
                println!("動画情報を表示します: {:?}", *info_lock);
            } else {
                eprintln!("動画情報の取得に失敗しました");
            }

            // ffmpegでフレーム抽出
            let status = std::process::Command::new("ffmpeg")
                .args([
                    "-i",
                    input_path.to_str().unwrap(),
                    "-vf",
                    &format!("select=between(n\\,{start}\\,{end}),scale=640:-1"),
                    "-vsync",
                    "0",
                    &output_pattern,
                ])
                .status()
                .expect("ffmpeg 実行失敗");

            if !status.success() {
                eprintln!("ffmpeg 実行に失敗しました");
                *is_loading.lock().unwrap() = false;
                return;
            }

            // フレームを読み込んでキャッシュに登録
            let mut frame_indices = Vec::new();
            let mut frame_index = 1;
            loop {
                let frame_path = format!("{}/frame_{:03}.jpeg", out_dir, frame_index);
                if !PathBuf::from(&frame_path).exists() {
                    break;
                }

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
            *total_frames.lock().unwrap() = frame_index - 1;

            *is_loading.lock().unwrap() = false;
            println!("トリミング完了: {start} ~ {end} フレームを読み込みました");
        });

        Ok(())
    }

    fn export_mp4(&self, output_path: PathBuf, fps: usize) -> Result<(), String> {
        let out_dir = "frames";
        if !PathBuf::from(out_dir).exists() {
            return Err("フレームディレクトリが存在しません".to_string());
        }

        let status = std::process::Command::new("ffmpeg")
            .args([
                "-y",
                "-framerate",
                &fps.to_string(),
                "-i",
                &format!("{}/frame_%03d.jpeg", out_dir),
                "-vf",
                "scale=trunc(iw/2)*2:trunc(ih/2)*2",
                "-c:v",
                "libx264",
                "-pix_fmt",
                "yuv420p",
                output_path.to_str().unwrap(),
            ])
            .status()
            .map_err(|e| format!("ffmpeg 実行失敗: {}", e))?;

        if !status.success() {
            return Err("ffmpeg 実行に失敗しました".to_string());
        }

        Ok(())


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

            ui.label("動画情報:");
            if let Some(info) = &*self.infomation.lock().unwrap() {
                // info.streamsの一周目は動画情報、2周目は音声情報

                for (i, stream) in info.streams.iter().enumerate() {
                    if i == 0 {
                        ui.label("【動画ストリーム】");
                    } else if i == 1 {
                        ui.label("【音声ストリーム】");
                    } else {
                        ui.label(&format!("【その他のストリーム {}】", i));
                    }
                    if let (Some(width), Some(height)) = (stream.width, stream.height) {
                        ui.label(format!("解像度: {}x{}", width, height));
                    }
                    if let Some(rate) = &stream.r_frame_rate {
                        ui.label(format!(
                            "フレームレート: {}",
                            self.parse_fps(rate).unwrap_or(0.0)
                        ));
                    }
                    if let Some(nb) = &stream.nb_frames {
                        ui.label(format!("総フレーム数: {}", nb));
                    }
                }
            } else {
                ui.label("動画情報はありません");
            }

            ui.horizontal(|ui| {
                ui.label("動画始点:");
                ui.text_edit_singleline(&mut self.start_frame);
            });

            ui.horizontal(|ui| {
                ui.label("動画終点:");
                ui.text_edit_singleline(&mut self.end_frame);
            });

            if ui.button("動画をトリミングする").clicked() {
                let path = PathBuf::from(self.video_path_input.clone());
                if path.exists() {
                    let start = self.start_frame.clone().parse().unwrap_or(0);
                    let end = self.end_frame.clone().parse().unwrap_or(0);
                    match self.trim_video(path, start, end) {
                        Ok(out_path) => println!("トリミング完了: {:?}", out_path),
                        Err(e) => eprintln!("{e}"),
                    }
                } else {
                    eprintln!("指定されたパスが存在しません: {:?}", path);
                }
            }

            ui.horizontal(|ui| {
                ui.label("動画を出力させる:");
                if ui.button("出力").clicked() {
                    // export_mp4関数を呼び出す
                    let output_path = PathBuf::from("output.mp4");
                    let fps = 30; // 固定値
                    if let Err(e) = self.export_mp4(output_path, fps) {
                        eprintln!("動画の出力に失敗しました: {}", e);
                    } else {
                        println!("動画を出力しました: output.mp4");
                    }
                }
            });
        });
    }

    // 分数の文字列をパースする関数
    fn parse_fps(&self, s: &str) -> Option<f64> {
        let parts: Vec<&str> = s.split('/').collect();
        if parts.len() == 2 {
            if let (Ok(num), Ok(den)) = (parts[0].parse::<f64>(), parts[1].parse::<f64>()) {
                if den != 0.0 {
                    return Some(num / den);
                }
            }
        }
        None
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
