
use eframe::{App, egui};
use ffmpeg_next as ffmpeg;
use image::RgbImage;
use std::default::Default;
use std::fs;
use std::process::Command;
use std::io::Write;
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
    texture: Option<egui::TextureHandle>, // 動画表示用テクスチャ
    playing: bool,
    last_update: Instant, // 最後にフレームを更新した時間
    size: (u32, u32),
    fps: f32,
    frames: Vec<Vec<u8>>, // 動画フレーム
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
    // 音声関連設定
    include_audio: bool, // エクスポート時に元動画の音声トラックを含める
    audio_offset_frames: usize,   // frames[0] が元動画の何フレーム目に相当するか（音声同期用オフセット）
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
            include_audio: true,
            audio_offset_frames: 0,
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

        // export_rx のポーリングは update() 側で行う（再描画要求を出せるように）
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
                        // reset audio offset when loading a new clip
                        self.audio_offset_frames = 0;
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
                            // if we removed from the head, advance audio offset accordingly
                            if s == 0 {
                                self.audio_offset_frames = self
                                    .audio_offset_frames
                                    .saturating_add(e - s);
                            }
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
                            // update audio offset because we discard frames before s
                            self.audio_offset_frames = self
                                .audio_offset_frames
                                .saturating_add(s);
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

            ui.horizontal(|ui| {
                ui.checkbox(&mut self.include_audio, "元動画の音声を含める");
            });

            if ui.button("書き出し").clicked() {
                // set up channel and spawn thread so we can receive status messages
                let (tx, rx) = channel::<String>();
                self.export_status = Some("Export started".to_string());
                self.export_rx = Some(rx);

                let out = self.output_path.clone();
                let frames = self.frames.clone();
                let include_audio = self.include_audio;
                let audio_source = self
                    .current_clip
                    .as_ref()
                    .map(|c| c.video_path_input.clone());
                let (w, h) = self.size;
                let fps = self.fps;
                let audio_offset_frames = self.audio_offset_frames;

                thread::spawn(move || {
                    if frames.is_empty() {
                        let _ = tx.send("No frames to export".to_string());
                        return;
                    }

                        // Stream frames directly to ffmpeg stdin (rawvideo) to avoid PNG save overhead
                        let _ = tx.send("Starting ffmpeg (stdin rawvideo)".to_string());

                        // decide absolute output path
                        let out_path_abs = if std::path::Path::new(&out).is_absolute() {
                            std::path::PathBuf::from(&out)
                        } else {
                            std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from(".")).join(&out)
                        };

                        // build ffmpeg args for rawvideo on stdin
                        let mut args: Vec<String> = Vec::new();
                        args.push("-y".to_string());
                        args.push("-f".to_string());
                        args.push("rawvideo".to_string());
                        args.push("-pix_fmt".to_string());
                        args.push("rgb24".to_string());
                        args.push("-s".to_string());
                        args.push(format!("{}x{}", w, h));
                        args.push("-r".to_string());
                        args.push(format!("{}", fps));
                        args.push("-i".to_string());
                        args.push("pipe:0".to_string());

                        // if including audio and we have a source path, add it as a second input
                        if include_audio {
                            if let Some(audio_path) = audio_source {
                                // calculate audio start time from audio_offset_frames
                                // audio_offset_frames is number of frames removed from original head
                                let audio_start_sec = (audio_offset_frames as f64) / (fps as f64);
                                if audio_start_sec > 0.0 {
                                    args.push("-ss".to_string());
                                    args.push(format!("{:.3}", audio_start_sec));
                                }

                                // limit audio input length to video duration so audio won't overshoot
                                let duration_sec = if fps > 0.0 {
                                    (frames.len() as f64) / (fps as f64)
                                } else {
                                    0.0
                                };
                                if duration_sec > 0.0 {
                                    args.push("-t".to_string());
                                    args.push(format!("{:.3}", duration_sec));
                                }

                                args.push("-i".to_string());
                                args.push(audio_path.clone());
                                // map video from first input and audio from second input
                                args.push("-map".to_string());
                                args.push("0:v:0".to_string());
                                args.push("-map".to_string());
                                args.push("1:a:0".to_string());
                            }
                        }

                        args.push("-c:v".to_string());
                        args.push("libx264".to_string());
                        args.push("-pix_fmt".to_string());
                        args.push("yuv420p".to_string());

                        if include_audio {
                            args.push("-c:a".to_string());
                            args.push("aac".to_string());
                            args.push("-b:a".to_string());
                            args.push("192k".to_string());
                            args.push("-shortest".to_string());
                        }

                        args.push(out_path_abs.to_string_lossy().to_string());

                        let mut child = match Command::new("ffmpeg").args(&args).stdin(std::process::Stdio::piped()).stderr(std::process::Stdio::piped()).spawn() {
                            Ok(c) => c,
                            Err(e) => {
                                let _ = tx.send(format!("failed to spawn ffmpeg: {}", e));
                                return;
                            }
                        };

                        // write frames to ffmpeg stdin
                        if let Some(mut stdin) = child.stdin.take() {
                            let total = frames.len();
                            for (i, fr) in frames.iter().enumerate() {
                                
                                let _ = tx.send(format!("Writing frames: {}/{}", i, total));
                                
                                if let Err(e) = stdin.write_all(fr) {
                                    let _ = tx.send(format!("failed to write frame {}: {}", i, e));
                                    break;
                                }
                            }
                            // close stdin to signal EOF
                            drop(stdin);

                            // wait for ffmpeg to finish and capture stderr
                            match child.wait_with_output() {
                                Ok(output) => {
                                    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
                                    if output.status.success() {
                                        let _ = tx.send(format!("Export succeeded: {}", out_path_abs.display()));
                                    } else {
                                        let _ = tx.send(format!("ffmpeg failed: status={} stderr={}", output.status, stderr));
                                    }
                                }
                                Err(e) => {
                                    let _ = tx.send(format!("failed waiting for ffmpeg: {}", e));
                                }
                            }
                        } else {
                            let _ = tx.send("failed to open ffmpeg stdin".to_string());
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

        // export スレッドからのメッセージをここで受け取り、受信があれば再描画要求を出す
        if let Some(rx) = &self.export_rx {
            let mut any = false;
            // drain available messages
            loop {
                match rx.try_recv() {
                    Ok(msg) => {
                        self.export_status = Some(msg.clone());
                        any = true;
                        if let Some(s) = &self.export_status {
                            if s.starts_with("Export succeeded") || s.starts_with("No frames to export") {
                                // 完了メッセージが来たらチャネルを外す
                                self.export_rx = None;
                                break;
                            }
                        }
                    }
                    Err(std::sync::mpsc::TryRecvError::Empty) => break,
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                        // 切断されたらチャネル解放
                        self.export_rx = None;
                        break;
                    }
                }
            }
            if any {
                ctx.request_repaint();
            }
        }


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

        // 再生中はイベントが来ない環境でも定期的に update() を呼ぶよう要求する
        if self.playing {
            ctx.request_repaint_after(Duration::from_millis(16));
        }
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
