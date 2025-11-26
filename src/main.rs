
use anyhow::Result;
use eframe::{App, egui};
use ffmpeg_next as ffmpeg;
use image::RgbImage;
use std::default::Default;
use std::fs;
use std::process::Command;
use std::sync::mpsc::{channel, Receiver};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

// アップロード時に大きすぎるフレームを縮小するための最大幅・高さ
const MAX_UPLOAD_DIM: u32 = 960;

// 動画クリップをオブジェクトとして扱う
struct VideoClip {
    video_path_input: String,
    size: (u32, u32),
    fps: f32,
    frames: Vec<Vec<u8>>,
}

impl VideoClip {
    fn load(path: &str) -> std::result::Result<VideoClip, ffmpeg::Error> {
        ffmpeg::format::input(&path).and_then(|mut ictx| {
            let input = ictx.streams().best(ffmpeg::media::Type::Video).unwrap();
            let video_stream_index = input.index();
            let fps = input.avg_frame_rate();
            let fps_val = if fps.1 != 0 {
                fps.0 as f32 / fps.1 as f32
            } else {
                60.0
            };

            let context_decoder =
                ffmpeg::codec::context::Context::from_parameters(input.parameters())?;
            let mut decoder = context_decoder.decoder().video()?;

            let mut scaler = ffmpeg::software::scaling::context::Context::get(
                decoder.format(),
                decoder.width(),
                decoder.height(),
                ffmpeg::format::Pixel::RGB24,
                decoder.width(),
                decoder.height(),
                ffmpeg::software::scaling::flag::Flags::BILINEAR,
            )?;

            let mut frames: Vec<Vec<u8>> = Vec::new();
            let mut decoded = ffmpeg::util::frame::video::Video::empty();

            for (stream, packet) in ictx.packets() {
                if stream.index() == video_stream_index {
                    decoder.send_packet(&packet)?;
                    while decoder.receive_frame(&mut decoded).is_ok() {
                        let mut rgb_frame = ffmpeg::util::frame::video::Video::empty();
                        scaler.run(&decoded, &mut rgb_frame)?;
                        frames.push(rgb_frame.data(0).to_vec());
                    }
                }
            }

            // 動画情報をコンソールに出力（デバッグ用）
            println!("Loaded video: {}", path);
            println!("Resolution: {}x{}", decoder.width(), decoder.height());
            println!("FPS: {}", fps_val);
            println!("Total frames: {}", frames.len());
            

            Ok(VideoClip {
                video_path_input: path.to_string(),
                size: (decoder.width(), decoder.height()),
                fps: fps_val,
                frames,
            })
        })
    }
}

struct VideoEditorApp {
    video_path_input: String,
    texture: Option<egui::TextureHandle>,
    playing: bool,
    last_update: Instant,
    size: (u32, u32),
    fps: f32,
    frames: Vec<Vec<u8>>,
    current_frame: usize,
    current_clip: Option<VideoClip>,
    // 切り取り用の秒数指定
    cut_start_sec: f32,
    cut_end_sec: f32,
    // 出力先パス
    output_path: String,
    // エクスポートステータスと受信チャネル
    export_status: Option<String>,
    export_rx: Option<Receiver<String>>,
}

impl Default for VideoEditorApp {
    fn default() -> Self {
        ffmpeg::init().unwrap();
        Self {
            video_path_input: "C:\\test\\test.mp4".to_string(),
            texture: None,
            playing: false,
            last_update: Instant::now(),
            size: (1920, 1080),
            fps: 60.0,
            frames: Vec::new(),
            current_frame: 0,
            current_clip: None,
            cut_start_sec: 0.0,
            cut_end_sec: 0.0,
            output_path: "output.mp4".to_string(),
            export_status: None,
            export_rx: None,
        }
    }
}

impl VideoEditorApp {
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

            // 表示

            // 動画サイズに合わせて表示サイズを調整
            let xsize = self.size.0 as f32;
            let ysize = self.size.1 as f32;
            let scale = if view_size.x / xsize < view_size.y / ysize {
                view_size.x / xsize as f32
            } else {
                view_size.y / ysize as f32
            };
            let video_size = egui::vec2(xsize * scale, ysize * scale);

            //読み込みが終わっていたらメッセージで伝える



            if let Some(texture) = &self.texture {
                view_child_ui.image((texture.id(), video_size));
            } else {
                view_child_ui.label("動画を読み込んでね！");
            }

            let (rect, _res) = ui.allocate_exact_size(timeline_size, egui::Sense::hover());
            ui.painter()
                .rect_filled(rect, 0.0, egui::Color32::from_rgb(200, 240, 200));

            let mut timeline_child_ui = ui.child_ui(
                rect,
                egui::Layout::centered_and_justified(egui::Direction::TopDown),
            );
            timeline_child_ui.label("タイムライン");
        });

        // 受信チャネルがある場合はメッセージをチェックしてステータス表示を更新
        if let Some(rx) = &self.export_rx {
            if let Ok(msg) = rx.try_recv() {
                self.export_status = Some(msg);
                // 成功・致命的メッセージならチャネルをクリア
                if let Some(s) = &self.export_status {
                    if s.starts_with("Export succeeded") || s.starts_with("No frames to export") {
                        self.export_rx = None;
                    }
                }
            }
        }
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
        option_child_ui.vertical(|ui| {
            ui.horizontal(|ui| {
                ui.label("動画パス:");
                ui.text_edit_singleline(&mut self.video_path_input);
            });

            if ui.button("読み込み").clicked() {
                match VideoClip::load(&self.video_path_input) {
                    Ok(clip) => {
                        // フレームの二重コピーを避けるため、frames をムーブして再生用バッファに入れる
                        self.size = clip.size;
                        self.fps = clip.fps;
                        // move frames out of clip into self.frames (Vecにムーブ)
                        self.frames = clip.frames;
                        self.current_frame = 0;
                        // current_clip にはメタデータのみ保持（フレームは self.frames にムーブ済み）
                        self.current_clip = Some(VideoClip {
                            video_path_input: clip.video_path_input,
                            size: clip.size,
                            fps: clip.fps,
                            frames: Vec::new(),
                        });
                        self.playing = false;
                        self.last_update = Instant::now();
                    }
                    Err(e) => {
                        // 読み込みエラーは現状無視（必要ならエラーハンドリング追加）
                        eprintln!("Error loading clip: {}", e);
                    }
                }
            }

            if ui
                .button(if self.playing {
                    "⏸ 停止"
                } else {
                    "▶ 再生"
                })
                .clicked()
            {
                self.playing = !self.playing;
                self.last_update = Instant::now();
            }

            ui.horizontal(|ui| {
                if ui.button("最初から").clicked() {
                    self.current_frame = 0;
                }

            });

            ui.separator();
            ui.label("秒指定で範囲切り取り:");
            ui.horizontal(|ui| {
                ui.label("開始(s):");
                // シンプルにテキスト入力/DragValue で秒数を受ける
                ui.add(egui::widgets::DragValue::new(&mut self.cut_start_sec).speed(0.1));
                ui.label("終了(s):");
                ui.add(egui::widgets::DragValue::new(&mut self.cut_end_sec).speed(0.1));
            });

            ui.horizontal(|ui| {
                if ui.button("指定範囲を切り取る").clicked() {
                    // 秒 -> フレームインデックス
                    if self.fps > 0.0 && !self.frames.is_empty() {
                        let total_frames = self.frames.len();
                        let mut start_f = (self.cut_start_sec * self.fps).round() as isize;
                        let mut end_f = (self.cut_end_sec * self.fps).round() as isize;
                        if start_f < 0 { start_f = 0; }
                        if end_f < 0 { end_f = 0; }
                        let mut s = start_f as usize;
                        let mut e = end_f as usize;
                        if s > total_frames { s = total_frames; }
                        if e > total_frames { e = total_frames; }
                        if s > e { std::mem::swap(&mut s, &mut e); }

                        if s < e {
                            // drain the range [s, e)
                            self.frames.drain(s..e);
                            // adjust current_frame to be at start s (or end)
                            self.current_frame = s.min(self.frames.len());
                        }
                    }
                }

                if ui.button("指定範囲を抽出（その範囲だけ残す）").clicked() {
                    if self.fps > 0.0 && !self.frames.is_empty() {
                        let total_frames = self.frames.len();
                        let mut start_f = (self.cut_start_sec * self.fps).round() as isize;
                        let mut end_f = (self.cut_end_sec * self.fps).round() as isize;
                        if start_f < 0 { start_f = 0; }
                        if end_f < 0 { end_f = 0; }
                        let mut s = start_f as usize;
                        let mut e = end_f as usize;
                        if s > total_frames { s = total_frames; }
                        if e > total_frames { e = total_frames; }
                        if s > e { std::mem::swap(&mut s, &mut e); }

                        if s < e {
                            let new_frames: Vec<Vec<u8>> = self.frames[s..e].to_vec();
                            self.frames = new_frames;
                            self.current_frame = 0;
                        }
                    }
                }
            });

            ui.separator();
            ui.horizontal(|ui| {
                ui.label("出力先:");
                ui.text_edit_singleline(&mut self.output_path);
            });

            if ui.button("書き出し").clicked() {
                // set up channel and spawn thread so we can receive status messages
                let (tx, rx) = channel::<String>();
                self.export_status = Some("Export started".to_string());
                self.export_rx = Some(rx);

                let out = self.output_path.clone();
                let frames = self.frames.clone();
                let (w, h) = self.size;
                let fps = self.fps;

                thread::spawn(move || {
                    if frames.is_empty() {
                        let _ = tx.send("No frames to export".to_string());
                        return;
                    }

                    let _ = tx.send("Creating temp dir".to_string());
                    // create temporary dir
                    let now_ms = SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .unwrap()
                        .as_millis();
                    let tmpdir = std::env::temp_dir().join(format!("video_export_{}", now_ms));
                    if let Err(e) = fs::create_dir_all(&tmpdir) {
                        let _ = tx.send(format!("failed to create tmpdir: {}", e));
                        return;
                    }

                    // save frames as PNG
                    let total = frames.len();
                    for (i, fr) in frames.iter().enumerate() {
                        if i % 30 == 0 {
                            let _ = tx.send(format!("Saving frames: {}/{}", i, total));
                        }
                        let img = match RgbImage::from_raw(w, h, fr.clone()) {
                            Some(i) => i,
                            None => {
                                let _ = tx.send(format!("failed to create image for frame {}", i));
                                continue;
                            }
                        };
                        let fname = tmpdir.join(format!("frame_{:06}.png", i + 1));
                        if let Err(e) = img.save(&fname) {
                            let _ = tx.send(format!("failed to save frame {}: {}", i, e));
                        }
                    }

                    let _ = tx.send("Running ffmpeg".to_string());
                    // decide absolute output path so ffmpeg writes outside tmpdir
                    let out_path_abs = if std::path::Path::new(&out).is_absolute() {
                        std::path::PathBuf::from(&out)
                    } else {
                        std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from(".")).join(&out)
                    };

                    // run ffmpeg to assemble and capture output
                    let ffmpeg_out = Command::new("ffmpeg")
                        .arg("-y")
                        .arg("-framerate")
                        .arg(format!("{}", fps))
                        .arg("-i")
                        .arg("frame_%06d.png")
                        .arg("-c:v")
                        .arg("libx264")
                        .arg("-pix_fmt")
                        .arg("yuv420p")
                        .arg(out_path_abs.to_string_lossy().as_ref())
                        .current_dir(&tmpdir)
                        .output();

                    match ffmpeg_out {
                        Ok(output) => {
                            let stderr = String::from_utf8_lossy(&output.stderr).to_string();
                            if output.status.success() {
                                let _ = tx.send(format!("Export succeeded: {}", out_path_abs.display()));
                                // cleanup temp dir
                                let _ = fs::remove_dir_all(&tmpdir);
                            } else {
                                let _ = tx.send(format!("ffmpeg failed: status={} stderr={} ", output.status, stderr));
                                // keep tmpdir for debugging
                            }
                        }
                        Err(e) => {
                            let _ = tx.send(format!("failed to run ffmpeg: {}", e));
                        }
                    }
                });
            }
            if let Some(status) = &self.export_status {
                ui.separator();
                ui.label(format!("状態: {}", status));
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


        if self.playing && !self.frames.is_empty() {
            let now = Instant::now();
            let frame_duration = Duration::from_secs_f32(1.0 / self.fps);

            if now.duration_since(self.last_update) >= frame_duration {
                self.last_update = now;

                if self.current_frame < self.frames.len() {
                    let frame = &self.frames[self.current_frame];
                    // アップロード前に表示用に縮小しておく（簡易 nearest-neighbor）
                    let src_w = self.size.0;
                    let src_h = self.size.1;
                    let mut dst_w = src_w;
                    let mut dst_h = src_h;
                    if src_w > MAX_UPLOAD_DIM || src_h > MAX_UPLOAD_DIM {
                        let scale = (MAX_UPLOAD_DIM as f32 / src_w as f32)
                            .min(MAX_UPLOAD_DIM as f32 / src_h as f32);
                        dst_w = (src_w as f32 * scale).max(1.0) as u32;
                        dst_h = (src_h as f32 * scale).max(1.0) as u32;
                    }

                    let upload_buffer_vec: Option<Vec<u8>> = if dst_w != src_w || dst_h != src_h {
                        // 簡易ダウンサンプリング（nearest） - 出力を新しい Vec に作る
                        let mut out = vec![0u8; (dst_w as usize) * (dst_h as usize) * 3];
                        for y in 0..dst_h {
                            for x in 0..dst_w {
                                let src_x = ((x as f32) * (src_w as f32 / dst_w as f32)) as u32;
                                let src_y = ((y as f32) * (src_h as f32 / dst_h as f32)) as u32;
                                let src_idx = ((src_y * src_w + src_x) as usize) * 3;
                                let dst_idx = ((y * dst_w + x) as usize) * 3;
                                out[dst_idx..dst_idx + 3]
                                    .copy_from_slice(&frame[src_idx..src_idx + 3]);
                            }
                        }
                        Some(out)
                    } else {
                        None
                    };

                    let color_image = if let Some(buf) = upload_buffer_vec {
                        egui::ColorImage::from_rgb([dst_w as usize, dst_h as usize], &buf)
                    } else {
                        egui::ColorImage::from_rgb([dst_w as usize, dst_h as usize], &frame)
                    };
                    if let Some(tex) = &mut self.texture {
                        tex.set(color_image, egui::TextureOptions::LINEAR);
                    } else {
                        self.texture = Some(ctx.load_texture(
                            "video_preview",
                            color_image,
                            egui::TextureOptions::LINEAR,
                        ));
                    }
                    self.current_frame += 1;
                }

                // フレーム終了時の処理
                ctx.request_repaint();
            }

            if self.frames.is_empty() {
                self.playing = false;
            }
        }

        //メイン画面構成
        egui::CentralPanel::default().show(ctx, |ui| {
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

    let app = VideoEditorApp::default();
    eframe::run_native("RustVideoEditor", options, Box::new(|_cc| Box::new(app)))
}
