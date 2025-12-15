
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
    // タイムライン用サムネイルキャッシュ (frame_index, texture)
    timeline_thumbs: Vec<(usize, egui::TextureHandle)>,
    // サムネイル再生成フラグ
    timeline_dirty: bool,
    // 切り取り用の秒数指定
    cut_start_sec: f32,
    cut_end_sec: f32,
    // 出力先パス
    output_path: String,
    // エクスポートステータスと受信チャネル
    export_status: Option<String>,
    export_rx: Option<Receiver<String>>,
    // 動画読み込みステータスと受信チャネル
    load_status: Option<String>,
    load_rx: Option<Receiver<String>>,
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
            timeline_thumbs: Vec::new(),
            timeline_dirty: false,
            cut_start_sec: 0.0,
            cut_end_sec: 0.0,
            output_path: "output.mp4".to_string(),
            export_status: None,
            export_rx: None,
            load_status: None,
            load_rx: None,
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
            //timeline_child_ui.label("タイムライン");

            // タイムラインに数秒ごとのサムネイルを並べる
            let ctx = ui.ctx();
            // 間隔（秒） -- 必要ならUIで変更可能にする
            let interval_sec = 1.0_f32;
            if !self.frames.is_empty() {
                // 再生成が必要なら作る
                if self.timeline_dirty {
                    // 既存テクスチャを drop して再生成
                    self.timeline_thumbs.clear();
                    let total = self.frames.len();
                    let step = ((self.fps * interval_sec).round() as usize).max(1);
                    // サムネイルサイズ（小さめ）
                    let thumb_w = 160u32;
                    let thumb_h = 90u32;
                    for idx in (0..total).step_by(step) {
                        let frame = &self.frames[idx];
                        // 簡易ダウンサンプリング nearest
                        let src_w = self.size.0;
                        let src_h = self.size.1;
                        let mut out = vec![0u8; (thumb_w as usize) * (thumb_h as usize) * 3];
                        for y in 0..thumb_h {
                            for x in 0..thumb_w {
                                let src_x = ((x as f32) * (src_w as f32 / thumb_w as f32)) as u32;
                                let src_y = ((y as f32) * (src_h as f32 / thumb_h as f32)) as u32;
                                let src_idx = ((src_y * src_w + src_x) as usize) * 3;
                                let dst_idx = ((y * thumb_w + x) as usize) * 3;
                                out[dst_idx..dst_idx + 3].copy_from_slice(&frame[src_idx..src_idx + 3]);
                            }
                        }
                        let color_image = egui::ColorImage::from_rgb([thumb_w as usize, thumb_h as usize], &out);
                        let tex = ctx.load_texture(&format!("thumb_{}", idx), color_image, egui::TextureOptions::LINEAR);
                        self.timeline_thumbs.push((idx, tex));
                    }
                    self.timeline_dirty = false;
                }

                // 描画: 横スクロール可能にする
                let mut scroll = egui::containers::ScrollArea::horizontal();
                scroll.show(&mut timeline_child_ui, |ui| {
                    ui.horizontal(|ui| {
                        for (idx, tex) in &self.timeline_thumbs {
                            let size = egui::vec2(160.0, 90.0);
                            // 画像をボタンにしてクリックでそのフレームへ移動
                            if ui.add(egui::ImageButton::new((tex.id(), size))).clicked() {
                                self.current_frame = *idx;
                            }
                        }
                    });
                });
            }
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
                // バックグラウンドスレッドで読み込み、進捗をチャネルで送信
                let (tx, rx) = channel::<String>();
                self.load_status = Some("読み込み開始".to_string());
                self.load_rx = Some(rx);

                let path = self.video_path_input.clone();
                thread::spawn(move || {
                    let _ = tx.send("動画ファイルを開いています...".to_string());
                    
                    match ffmpeg::format::input(&path) {
                        Ok(mut ictx) => {
                            let _ = tx.send("ストリーム情報を解析中...".to_string());
                            
                            let input = match ictx.streams().best(ffmpeg::media::Type::Video) {
                                Some(s) => s,
                                None => {
                                    let _ = tx.send("エラー: 映像ストリームが見つかりません".to_string());
                                    return;
                                }
                            };
                            
                            let video_stream_index = input.index();
                            let fps = input.avg_frame_rate();
                            let fps_val = if fps.1 != 0 {
                                fps.0 as f32 / fps.1 as f32
                            } else {
                                60.0
                            };

                            let context_decoder = match ffmpeg::codec::context::Context::from_parameters(input.parameters()) {
                                Ok(c) => c,
                                Err(e) => {
                                    let _ = tx.send(format!("エラー: デコーダ初期化失敗 {}", e));
                                    return;
                                }
                            };
                            
                            let mut decoder = match context_decoder.decoder().video() {
                                Ok(d) => d,
                                Err(e) => {
                                    let _ = tx.send(format!("エラー: デコーダ取得失敗 {}", e));
                                    return;
                                }
                            };

                            let mut scaler = match ffmpeg::software::scaling::context::Context::get(
                                decoder.format(),
                                decoder.width(),
                                decoder.height(),
                                ffmpeg::format::Pixel::RGB24,
                                decoder.width(),
                                decoder.height(),
                                ffmpeg::software::scaling::flag::Flags::BILINEAR,
                            ) {
                                Ok(s) => s,
                                Err(e) => {
                                    let _ = tx.send(format!("エラー: スケーラー初期化失敗 {}", e));
                                    return;
                                }
                            };

                            let _ = tx.send(format!("フレームをデコード中 ({}x{}, {:.2}fps)...", decoder.width(), decoder.height(), fps_val));

                            let mut frames: Vec<Vec<u8>> = Vec::new();
                            let mut decoded = ffmpeg::util::frame::video::Video::empty();
                            let mut frame_count = 0;

                            for (stream, packet) in ictx.packets() {
                                if stream.index() == video_stream_index {
                                    if decoder.send_packet(&packet).is_ok() {
                                        while decoder.receive_frame(&mut decoded).is_ok() {
                                            let mut rgb_frame = ffmpeg::util::frame::video::Video::empty();
                                            if scaler.run(&decoded, &mut rgb_frame).is_ok() {
                                                frames.push(rgb_frame.data(0).to_vec());
                                                frame_count += 1;
                                                
                                                // 100フレームごとに進捗を送信
                                                if frame_count % 100 == 0 {
                                                    let _ = tx.send(format!("{}フレーム読み込み完了", frame_count));
                                                }
                                            }
                                        }
                                    }
                                }
                            }

                            let _ = tx.send(format!("読み込み完了: {}フレーム", frames.len()));

                            // 完了データをシリアライズして送信（JSON形式で送る）
                            // ここでは簡易的にフォーマット文字列で送信
                            let clip = VideoClip {
                                video_path_input: path.clone(),
                                size: (decoder.width(), decoder.height()),
                                fps: fps_val,
                                frames,
                            };
                            
                            // クリップデータはチャネル経由では送れないので、完了メッセージのみ送信
                            // 実際のデータ転送は後で考慮（今回は Arc<Mutex> か別の方法が必要）
                            let _ = tx.send(format!("LOAD_COMPLETE|{}|{}|{}|{}", 
                                clip.video_path_input, 
                                clip.size.0, 
                                clip.size.1, 
                                clip.fps
                            ));
                            
                            // フレームデータはここでは送れないので、代替案として
                            // once_cell や Arc<Mutex<Option<VideoClip>>> を使う必要がある
                            // 今回は簡易実装として、完了後に再度同期ロードする形にする
                        }
                        Err(e) => {
                            let _ = tx.send(format!("エラー: ファイルを開けません {}", e));
                        }
                    }
                });
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
                        // 負の数は0に補正
                        if start_f < 0 { start_f = 0; }
                        // 負の数、または0は最後までに補正
                        if end_f <= 0 { end_f = total_frames as isize; }
                        let mut s = start_f as usize;
                        let mut e = end_f as usize;
                        if s > total_frames { s = total_frames; }
                        if e > total_frames { e = total_frames; }
                        if s > e { std::mem::swap(&mut s, &mut e); }

                        if s < e {
                            // 指定範囲を削除
                            self.frames.drain(s..e);
                            // 先頭から削った場合は audio_offset_frames を調整
                            if s == 0 {
                                self.audio_offset_frames = self
                                    .audio_offset_frames
                                    .saturating_add(e - s);
                            }
                            // current_frame が範囲内にある場合は s に移動
                            self.current_frame = s.min(self.frames.len());
                            // サムネイル再生成フラグ
                            self.timeline_dirty = true;
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
                            // 先頭から抽出した場合は audio_offset_frames を調整
                            self.audio_offset_frames = self
                                .audio_offset_frames
                                .saturating_add(s);
                            self.frames = new_frames;
                            self.current_frame = 0;
                            // サムネイル再生成フラグ
                            self.timeline_dirty = true;
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
                self.export_status = Some("エクスポート開始".to_string());
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
                        let _ = tx.send("フレームがありません！".to_string());
                        return;
                    }

                        // Stream frames directly to ffmpeg stdin (rawvideo) to avoid PNG save overhead
                        let _ = tx.send("Starting ffmpeg (stdin rawvideo)".to_string());

                        // output path absolute
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

                        // audio input
                        if include_audio {
                            if let Some(audio_path) = audio_source {
                                // 元動画から音声トラックを取得
                                // オフセット分を考慮して開始位置を調整
                                let audio_start_sec = (audio_offset_frames as f64) / (fps as f64);
                                if audio_start_sec > 0.0 {
                                    args.push("-ss".to_string());
                                    args.push(format!("{:.3}", audio_start_sec));
                                }

                                // 動画の長さに合わせて音声も切り取る
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
                                // マッピング
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
                                let _ = tx.send(format!("ffmepg起動失敗: {}", e));
                                return;
                            }
                        };

                        // ffmpeg の stdin にフレームを書き込む
                        if let Some(mut stdin) = child.stdin.take() {
                            let total = frames.len();
                            for (i, fr) in frames.iter().enumerate() {
                                
                                let _ = tx.send(format!("フレームを書き込み中: {}/{}", i, total));
                                
                                if let Err(e) = stdin.write_all(fr) {
                                    let _ = tx.send(format!("書き込みに失敗しました {}: {}", i, e));
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
                                        let _ = tx.send(format!("出力成功: {}", out_path_abs.display()));
                                    } else {
                                        let _ = tx.send(format!("出力失敗: status={} stderr={}", output.status, stderr));
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
            if let Some(status) = &self.load_status {
                ui.separator();
                ui.label(format!("読み込み: {}", status));
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

        // 読み込みスレッドからのメッセージを受信
        let mut load_complete = false;
        if let Some(rx) = &self.load_rx {
            let mut any = false;
            loop {
                match rx.try_recv() {
                    Ok(msg) => {
                        // LOAD_COMPLETE メッセージが来たら同期的に再ロード
                        if msg.starts_with("LOAD_COMPLETE|") {
                            self.load_status = Some("フレームデータを取り込み中...".to_string());
                            // 同期的にVideoClip::loadを再度呼ぶ（改善余地あり）
                            match VideoClip::load(&self.video_path_input) {
                                Ok(clip) => {
                                    self.size = clip.size;
                                    self.fps = clip.fps;
                                    self.frames = clip.frames;
                                    self.timeline_dirty = true;
                                    self.current_frame = 0;
                                    self.current_clip = Some(VideoClip {
                                        video_path_input: clip.video_path_input,
                                        size: clip.size,
                                        fps: clip.fps,
                                        frames: Vec::new(),
                                    });
                                    self.audio_offset_frames = 0;
                                    self.playing = false;
                                    self.last_update = Instant::now();
                                    self.load_status = Some("読み込み完了".to_string());
                                    load_complete = true;
                                }
                                Err(e) => {
                                    self.load_status = Some(format!("エラー: {}", e));
                                    load_complete = true;
                                }
                            }
                        } else {
                            self.load_status = Some(msg.clone());
                        }
                        any = true;
                    }
                    Err(std::sync::mpsc::TryRecvError::Empty) => break,
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                        load_complete = true;
                        break;
                    }
                }
            }
            if any {
                ctx.request_repaint();
            }
        }
        if load_complete {
            self.load_rx = None;
        }

        // export スレッドからのメッセージをここで受け取り、受信があれば再描画要求を出す
        if let Some(rx) = &self.export_rx {
            let mut any = false;
            // チャネルにメッセージがある限り受信する
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
