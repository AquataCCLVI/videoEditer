use eframe::{App, egui};
use ffmpeg_next as ffmpeg;
use image::RgbImage;
use std::default::Default;
use std::fs;
use std::process::Command;
use std::sync::mpsc::{Receiver, channel};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[derive(Clone, Debug)]
struct SubtitleItem {
    start_ms: u64,
    end_ms: u64,
    text: String,
}

fn parse_srt_to_items(srt: &str) -> Vec<SubtitleItem> {
    let mut items = Vec::<SubtitleItem>::new();
    let mut current_start: Option<u64> = None;
    let mut current_end: Option<u64> = None;
    let mut current_text_lines: Vec<String> = Vec::new();

    let flush = |items: &mut Vec<SubtitleItem>,
                     current_start: &mut Option<u64>,
                     current_end: &mut Option<u64>,
                     current_text_lines: &mut Vec<String>| {
        if let (Some(start), Some(end)) = (*current_start, *current_end) {
            let text = current_text_lines.join("\n").trim().to_string();
            if !text.is_empty() && end > start {
                items.push(SubtitleItem {
                    start_ms: start,
                    end_ms: end,
                    text,
                });
            }
        }
        *current_start = None;
        *current_end = None;
        current_text_lines.clear();
    };

    for raw_line in srt.lines() {
        let line = raw_line.trim_end_matches(['\r', '\n']);
        let t = line.trim();
        if t.is_empty() {
            flush(
                &mut items,
                &mut current_start,
                &mut current_end,
                &mut current_text_lines,
            );
            continue;
        }

        // SRT index line (e.g., "1")
        if current_start.is_none() && current_end.is_none() && t.chars().all(|c| c.is_ascii_digit()) {
            continue;
        }

        // timecode line
        if t.contains("-->") {
            // start/end already set? flush and start new.
            if current_start.is_some() || current_end.is_some() || !current_text_lines.is_empty() {
                flush(
                    &mut items,
                    &mut current_start,
                    &mut current_end,
                    &mut current_text_lines,
                );
            }
            let mut parts = t.split("-->");
            let start_str = parts.next().unwrap_or("").trim();
            let end_str = parts.next().unwrap_or("").trim();
            if let (Some(start), Some(end)) = (parse_srt_time_ms(start_str), parse_srt_time_ms(end_str)) {
                current_start = Some(start);
                current_end = Some(end);
            }
            continue;
        }

        // text line
        if current_start.is_some() && current_end.is_some() {
            current_text_lines.push(t.to_string());
        }
    }

    // flush last
    flush(
        &mut items,
        &mut current_start,
        &mut current_end,
        &mut current_text_lines,
    );

    items
}

fn parse_srt_time_ms(s: &str) -> Option<u64> {
    // Accept: HH:MM:SS,mmm or HH:MM:SS.mmm
    // Also accept: MM:SS,mmm or MM:SS.mmm
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    let (main, frac) = if let Some(pos) = s.rfind([',', '.']) {
        let (a, b) = s.split_at(pos);
        (a, Some(&b[1..]))
    } else {
        (s, None)
    };

    let parts: Vec<&str> = main.split(':').collect();
    let (h, m, sec) = match parts.len() {
        3 => (
            parts[0].trim().parse::<u64>().ok()?,
            parts[1].trim().parse::<u64>().ok()?,
            parts[2].trim().parse::<u64>().ok()?,
        ),
        2 => (
            0,
            parts[0].trim().parse::<u64>().ok()?,
            parts[1].trim().parse::<u64>().ok()?,
        ),
        _ => return None,
    };

    let mut ms = 0u64;
    if let Some(frac) = frac {
        let digits: String = frac.chars().take_while(|c| c.is_ascii_digit()).collect();
        if !digits.is_empty() {
            // normalize to milliseconds
            let v = digits.parse::<u64>().ok()?;
            ms = match digits.len() {
                1 => v * 100,
                2 => v * 10,
                3 => v,
                _ => {
                    // more than 3 digits: truncate
                    let p = 10u64.pow((digits.len() as u32).saturating_sub(3));
                    v / p
                }
            };
        }
    }

    Some(((h * 3600 + m * 60 + sec) * 1000) + ms)
}

fn ass_time_from_ms(ms: u64) -> String {
    // ASS uses h:mm:ss.cc (centiseconds)
    let total_cs = ms / 10;
    let cs = total_cs % 100;
    let total_s = total_cs / 100;
    let s = total_s % 60;
    let total_m = total_s / 60;
    let m = total_m % 60;
    let h = total_m / 60;
    format!("{}:{:02}:{:02}.{:02}", h, m, s, cs)
}

fn escape_ass_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '\n' => out.push_str("\\N"),
            '{' => out.push_str("\\{"),
            '}' => out.push_str("\\}"),
            _ => out.push(ch),
        }
    }
    out
}

fn escape_ffmpeg_filter_path(path: &std::path::Path) -> String {
    // ffmpeg filter graph parsing is picky on Windows (drive letter ':' etc)
    // Use forward slashes and escape ':' and '\''.
    let mut s = path.to_string_lossy().replace('\\', "/");
    s = s.replace(':', "\\:");
    s = s.replace('"', "\\\"");
    s = s.replace('\'', "\\\'");
    s
}

// アップロード時に大きすぎるフレームを縮小するための最大幅・高さ
const MAX_UPLOAD_DIM: u32 = 960;

// 動画クリップをオブジェクトとして扱う
#[derive(Clone)]
struct VideoClip {
    video_path_input: String,
    size: (u32, u32),
    fps: f32,
    frames: Vec<Vec<u8>>,
}

// プレイリスト用のエントリ（フレームを保持）
#[derive(Clone)]
struct ClipEntry {
    id: usize,
    path: String,
    size: (u32, u32),
    fps: f32,
    frames: Vec<Vec<u8>>,
    color: egui::Color32,
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
            let mut out_size: Option<(u32, u32)> = None;

            for (stream, packet) in ictx.packets() {
                if stream.index() == video_stream_index {
                    decoder.send_packet(&packet)?;
                    while decoder.receive_frame(&mut decoded).is_ok() {
                        let mut rgb_frame = ffmpeg::util::frame::video::Video::empty();
                        scaler.run(&decoded, &mut rgb_frame)?;

                        // egui::ColorImage::from_rgb は w*h*3 の密なRGBバッファを要求する。
                        // ffmpeg のフレームは stride(行パディング) を持つことがあるため、
                        // 毎フレーム packed な RGB に詰め替える。
                        let w = rgb_frame.width();
                        let h = rgb_frame.height();
                        let stride = rgb_frame.stride(0);
                        let data = rgb_frame.data(0);

                        if out_size.is_none() {
                            out_size = Some((w, h));
                        }

                        // stride は通常 row_bytes 以上だが、念のため守る
                        let row_bytes = (w as usize).saturating_mul(3);
                        let stride_bytes = (stride as isize).max(0) as usize;
                        let expected_len = row_bytes.saturating_mul(h as usize);

                        if row_bytes == 0 || h == 0 {
                            continue;
                        }

                        if stride_bytes < row_bytes {
                            // 異常系: 期待より短い場合はそのまま密データとして扱える長さか確認
                            if data.len() >= expected_len {
                                frames.push(data[0..expected_len].to_vec());
                            } else {
                                // 足りないフレームはスキップ
                                continue;
                            }
                        } else {
                            // 通常系: 行ごとに詰め替え
                            if data.len() < stride_bytes.saturating_mul(h as usize) {
                                // バッファ長が足りないならスキップ
                                continue;
                            }
                            let mut packed = vec![0u8; expected_len];
                            for y in 0..(h as usize) {
                                let src_off = y.saturating_mul(stride_bytes);
                                let dst_off = y.saturating_mul(row_bytes);
                                packed[dst_off..dst_off + row_bytes]
                                    .copy_from_slice(&data[src_off..src_off + row_bytes]);
                            }
                            frames.push(packed);
                        }
                    }
                }
            }

            // デコーダ側の幅高ではなく、packed 化したRGBフレームの幅高を採用
            let (out_w, out_h) = out_size.unwrap_or((decoder.width(), decoder.height()));

            // 動画情報をコンソールに出力（デバッグ用）
            println!("Loaded video: {}", path);
            println!("Resolution: {}x{}", out_w, out_h);
            println!("FPS: {}", fps_val);
            println!("Total frames: {}", frames.len());

            Ok(VideoClip {
                video_path_input: path.to_string(),
                size: (out_w, out_h),
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
    // プレイリスト管理
    playlist: Vec<ClipEntry>,
    clip_offsets: Vec<usize>, // 各クリップ開始の累積フレーム数
    total_frames: usize,
    global_frame: usize, // 再生位置（全体のフレームインデックス）
    current_clip_idx: usize,
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
    include_audio: bool,        // エクスポート時に元動画の音声トラックを含める
    audio_offset_frames: usize, // frames[0] が元動画の何フレーム目に相当するか（音声同期用オフセット）

    // 字幕（焼き込み用）
    burn_subtitles: bool,
    subtitles_srt: String,
}

impl Default for VideoEditorApp {
    fn default() -> Self {
        ffmpeg::init().unwrap();
        Self {
            video_path_input: "C:\\test\\test.mp4".to_string(),
            texture: None,
            playing: false,
            last_update: Instant::now(),
            playlist: Vec::new(),
            clip_offsets: Vec::new(),
            total_frames: 0,
            global_frame: 0,
            current_clip_idx: 0,
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

            burn_subtitles: false,
            subtitles_srt: "".to_string(),
        }
    }
}

impl VideoEditorApp {
    // 累積オフセットを再計算
    fn rebuild_offsets(&mut self) {
        self.clip_offsets.clear();
        let mut acc = 0usize;
        for clip in &self.playlist {
            self.clip_offsets.push(acc);
            acc += clip.frames.len();
        }
        self.total_frames = acc;
        if self.global_frame > self.total_frames.saturating_sub(1) {
            self.global_frame = self.total_frames.saturating_sub(1);
        }
    }

    // グローバルフレームから (clip_idx, local_idx) を取得
    fn map_global(&self, g: usize) -> Option<(usize, usize)> {
        if self.playlist.is_empty() || self.clip_offsets.is_empty() {
            return None;
        }
        // 二分探索
        match self.clip_offsets.binary_search(&g) {
            Ok(idx) => Some((idx, 0)),
            Err(pos) => {
                if pos == 0 {
                    return None;
                }
                let clip_idx = pos - 1;
                let local = g - self.clip_offsets[clip_idx];
                Some((clip_idx, local))
            }
        }
    }

    fn current_clip(&self) -> Option<&ClipEntry> {
        self.map_global(self.global_frame)
            .and_then(|(ci, _)| self.playlist.get(ci))
    }

    fn current_clip_mut(&mut self) -> Option<&mut ClipEntry> {
        let idx = self.map_global(self.global_frame).map(|(ci, _)| ci)?;
        self.playlist.get_mut(idx)
    }

    // 指定クリップの先頭にシーク
    fn seek_clip_start(&mut self, clip_idx: usize) {
        if clip_idx < self.playlist.len() {
            if let Some(off) = self.clip_offsets.get(clip_idx) {
                self.global_frame = *off;
                self.current_clip_idx = clip_idx;
                self.timeline_dirty = true;
            }
        }
    }

    // 指定グローバルフレームにシーク
    fn seek_global(&mut self, g: usize) {
        if self.total_frames == 0 {
            return;
        }
        self.global_frame = g.min(self.total_frames.saturating_sub(1));
        if let Some((ci, _)) = self.map_global(self.global_frame) {
            self.current_clip_idx = ci;
        }
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

            // プレビュー領域を上下に分割（ラベル + 動画）
            let label_height = 25.0;
            let video_height = view_size.y - label_height;

            let label_rect =
                egui::Rect::from_min_size(rect.min, egui::vec2(rect.width(), label_height));
            let video_rect = egui::Rect::from_min_size(
                rect.min + egui::vec2(0.0, label_height),
                egui::vec2(rect.width(), video_height),
            );

            // ラベル領域
            let mut label_ui =
                ui.child_ui(label_rect, egui::Layout::left_to_right(egui::Align::Center));

            // 選択中クリップ情報（ラベル領域に描画）
            if let Some(clip) = self.current_clip() {
                let fname = std::path::Path::new(&clip.path)
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or(&clip.path);
                let secs = if clip.fps > 0.0 {
                    clip.frames.len() as f32 / clip.fps
                } else {
                    0.0
                };
                label_ui.label(format!(
                    "選択中: {}  {}x{}  {:.2}fps  {:.2}s",
                    fname, clip.size.0, clip.size.1, clip.fps, secs
                ));
            }

            // 動画表示領域
            let mut video_ui = ui.child_ui(
                video_rect,
                egui::Layout::centered_and_justified(egui::Direction::TopDown),
            );

            // 表示
            let (disp_w, disp_h) = if let Some(clip) = self.current_clip() {
                clip.size
            } else {
                (640, 360)
            };
            let xsize = disp_w as f32;
            let ysize = disp_h as f32;
            let scale = if video_rect.width() / xsize < video_rect.height() / ysize {
                video_rect.width() / xsize as f32
            } else {
                video_rect.height() / ysize as f32
            };
            let video_size = egui::vec2(xsize * scale, ysize * scale);

            if let Some(texture) = &self.texture {
                video_ui.image((texture.id(), video_size));
            } else {
                video_ui.label("動画を読み込んでね！");
            }

            // タイムライン: クリップの長さに比例したバーを描画し、クリックでシーク
            let (rect, _res) = ui.allocate_exact_size(timeline_size, egui::Sense::hover());
            ui.painter()
                .rect_filled(rect, 0.0, egui::Color32::from_rgb(200, 240, 200));

            let mut timeline_child_ui = ui.child_ui(
                rect,
                egui::Layout::centered_and_justified(egui::Direction::TopDown),
            );

            if self.total_frames > 0 {
                let total_w = timeline_size.x - 20.0;
                timeline_child_ui.horizontal(|ui| {
                    let mut clicked_global: Option<usize> = None;
                    let current = self.map_global(self.global_frame);
                    for (idx, clip) in self.playlist.iter().enumerate() {
                        let start = self.clip_offsets[idx] as f32;
                        let end = start + clip.frames.len() as f32;
                        let ratio = clip.frames.len() as f32 / self.total_frames as f32;
                        let width = (total_w * ratio).max(8.0);
                        let height = 40.0;
                        let (rect, resp) =
                            ui.allocate_exact_size(egui::vec2(width, height), egui::Sense::click());
                        ui.painter().rect_filled(rect, 4.0, clip.color);
                        // 選択中クリップを太枠で強調
                        if idx == self.current_clip_idx {
                            ui.painter().rect_stroke(
                                rect,
                                4.0,
                                egui::Stroke {
                                    width: 3.0,
                                    color: egui::Color32::YELLOW,
                                },
                            );
                        }
                        // 現在位置のプレイヘッド線（このクリップ内の場合のみ）
                        if let Some((ci, local)) = current {
                            if ci == idx && !clip.frames.is_empty() {
                                let local_ratio =
                                    (local as f32 / clip.frames.len() as f32).clamp(0.0, 1.0);
                                let x = rect.min.x + rect.width() * local_ratio;
                                let top = rect.top();
                                let bottom = rect.bottom();
                                ui.painter().line_segment(
                                    [egui::pos2(x, top), egui::pos2(x, bottom)],
                                    egui::Stroke {
                                        width: 2.0,
                                        color: egui::Color32::BLACK,
                                    },
                                );
                            }
                        }
                        if resp.clicked() {
                            // クリック位置をローカル比率に変換してシーク
                            let local_ratio = ((resp.interact_pointer_pos().unwrap().x
                                - rect.min.x)
                                / rect.width())
                            .clamp(0.0, 1.0);
                            let local_frame =
                                (clip.frames.len() as f32 * local_ratio).floor() as usize;
                            let g = self.clip_offsets[idx] + local_frame;
                            clicked_global = Some(g.min(self.total_frames.saturating_sub(1)));
                        }
                        let label =
                            format!("{}: {:.1}s", idx + 1, clip.frames.len() as f32 / clip.fps);
                        ui.painter().text(
                            rect.center(),
                            egui::Align2::CENTER_CENTER,
                            label,
                            egui::FontId::proportional(12.0),
                            egui::Color32::BLACK,
                        );
                    }
                    if let Some(g) = clicked_global {
                        self.seek_global(g);
                    }
                });
            } else {
                timeline_child_ui.label("タイムライン");
            }
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
        option_child_ui.vertical(|ui| {
            ui.horizontal(|ui| {
                ui.label("動画パス:");
                ui.text_edit_singleline(&mut self.video_path_input);
            });

            if ui.button("読み込み").clicked() {
                match VideoClip::load(&self.video_path_input) {
                    Ok(clip) => {
                        let color = egui::Color32::from_rgb(
                            (50 + (self.playlist.len() * 70 % 200)) as u8,
                            (80 + (self.playlist.len() * 90 % 150)) as u8,
                            (100 + (self.playlist.len() * 60 % 155)) as u8,
                        );
                        let entry = ClipEntry {
                            id: self.playlist.len(),
                            path: clip.video_path_input.clone(),
                            size: clip.size,
                            fps: clip.fps,
                            frames: clip.frames,
                            color,
                        };
                        self.playlist.push(entry);
                        self.rebuild_offsets();
                        self.global_frame = self.total_frames.saturating_sub(1);
                        self.current_clip_idx = self.playlist.len().saturating_sub(1);
                        self.timeline_dirty = true;
                        self.playing = false;
                        self.last_update = Instant::now();
                        self.load_status = Some("読み込み完了".to_string());
                    }
                    Err(e) => {
                        self.load_status = Some(format!("エラー: {}", e));
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
                    self.seek_global(0);
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
                    if let Some((ci, _)) = self.map_global(self.global_frame) {
                        if let Some(clip) = self.playlist.get_mut(ci) {
                            if clip.fps > 0.0 && !clip.frames.is_empty() {
                                let total_frames = clip.frames.len();
                                let mut start_f = (self.cut_start_sec * clip.fps).round() as isize;
                                let mut end_f = (self.cut_end_sec * clip.fps).round() as isize;
                                if start_f < 0 {
                                    start_f = 0;
                                }
                                if end_f <= 0 {
                                    end_f = total_frames as isize;
                                }
                                let mut s = start_f as usize;
                                let mut e = end_f as usize;
                                if s > total_frames {
                                    s = total_frames;
                                }
                                if e > total_frames {
                                    e = total_frames;
                                }
                                if s > e {
                                    std::mem::swap(&mut s, &mut e);
                                }

                                if s < e {
                                    clip.frames.drain(s..e);
                                    self.rebuild_offsets();
                                    self.seek_clip_start(ci);
                                    self.timeline_dirty = true;
                                }
                            }
                        }
                    }
                }

                if ui.button("指定範囲を抽出（その範囲だけ残す）").clicked() {
                    if let Some((ci, _)) = self.map_global(self.global_frame) {
                        if let Some(clip) = self.playlist.get_mut(ci) {
                            if clip.fps > 0.0 && !clip.frames.is_empty() {
                                let total_frames = clip.frames.len();
                                let mut start_f = (self.cut_start_sec * clip.fps).round() as isize;
                                let mut end_f = (self.cut_end_sec * clip.fps).round() as isize;
                                if start_f < 0 {
                                    start_f = 0;
                                }
                                if end_f < 0 {
                                    end_f = 0;
                                }
                                let mut s = start_f as usize;
                                let mut e = end_f as usize;
                                if s > total_frames {
                                    s = total_frames;
                                }
                                if e > total_frames {
                                    e = total_frames;
                                }
                                if s > e {
                                    std::mem::swap(&mut s, &mut e);
                                }

                                if s < e {
                                    let new_frames: Vec<Vec<u8>> = clip.frames[s..e].to_vec();
                                    clip.frames = new_frames;
                                    self.rebuild_offsets();
                                    self.seek_clip_start(ci);
                                    self.timeline_dirty = true;
                                }
                            }
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

            ui.separator();
            ui.horizontal(|ui| {
                ui.checkbox(&mut self.burn_subtitles, "字幕を焼き込む");
            });
            if self.burn_subtitles {
                ui.label("字幕（SRT形式）を貼り付け:");
                ui.add(
                    egui::TextEdit::multiline(&mut self.subtitles_srt)
                        .desired_rows(8)
                        .hint_text("例:\n1\n00:00:01,000 --> 00:00:03,000\nこんにちは\n\n2\n00:00:04,000 --> 00:00:06,000\n字幕だよ"),
                );
            }

            if ui.button("書き出し").clicked() {
                // set up channel and spawn thread so we can receive status messages
                let (tx, rx) = channel::<String>();
                self.export_status = Some("エクスポート開始".to_string());
                self.export_rx = Some(rx);
                let out = self.output_path.clone();
                let include_audio = self.include_audio;
                let playlist = self.playlist.clone();
                let burn_subtitles = self.burn_subtitles;
                let subtitles_srt = self.subtitles_srt.clone();

                thread::spawn(move || {
                    if playlist.is_empty() {
                        let _ = tx.send("プレイリストが空です".to_string());
                        return;
                    }

                    let out_path_abs = if std::path::Path::new(&out).is_absolute() {
                        std::path::PathBuf::from(&out)
                    } else {
                        std::env::current_dir()
                            .unwrap_or_else(|_| std::path::PathBuf::from("."))
                            .join(&out)
                    };

                    let mut args: Vec<String> = Vec::new();
                    args.push("-y".to_string());

                    // クリップをそれぞれ入力として渡す
                    for clip in &playlist {
                        args.push("-i".to_string());
                        args.push(clip.path.clone());
                    }

                    // filter_complex を構築
                    let n = playlist.len();
                    let mut filter = String::new();
                    for i in 0..n {
                        filter.push_str(&format!("[{}:v][{}:a]", i, i));
                    }
                    if include_audio {
                        filter.push_str(&format!("concat=n={}:v=1:a=1[outv][outa]", n));
                    } else {
                        // 音声なし
                        for i in 0..n {
                            filter.push_str(&format!("[{}:v]", i));
                        }
                        filter.push_str(&format!("concat=n={}:v=1:a=0[outv]", n));
                    }

                    // 字幕を焼き込む場合は outv に subtitles フィルタを接続
                    let mut map_video = "[outv]".to_string();
                    let mut tmp_ass_path: Option<std::path::PathBuf> = None;
                    if burn_subtitles {
                        let items = parse_srt_to_items(&subtitles_srt);
                        if !items.is_empty() {
                            let (w, h) = playlist
                                .get(0)
                                .map(|c| c.size)
                                .unwrap_or((1280, 720));

                            let mut ass = String::new();
                            ass.push_str("[Script Info]\n");
                            ass.push_str("ScriptType: v4.00+\n");
                            ass.push_str(&format!("PlayResX: {}\n", w.max(1)));
                            ass.push_str(&format!("PlayResY: {}\n", h.max(1)));
                            ass.push_str("ScaledBorderAndShadow: yes\n");
                            ass.push_str("\n[V4+ Styles]\n");
                            ass.push_str("Format: Name, Fontname, Fontsize, PrimaryColour, SecondaryColour, OutlineColour, BackColour, Bold, Italic, Underline, StrikeOut, ScaleX, ScaleY, Spacing, Angle, BorderStyle, Outline, Shadow, Alignment, MarginL, MarginR, MarginV, Encoding\n");
                            ass.push_str("Style: Default,keifont,48,&H00FFFFFF,&H000000FF,&H00000000,&H64000000,0,0,0,0,100,100,0,0,1,2,0,2,40,40,40,1\n");
                            ass.push_str("\n[Events]\n");
                            ass.push_str("Format: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\n");
                            for it in items {
                                let start = ass_time_from_ms(it.start_ms);
                                let end = ass_time_from_ms(it.end_ms);
                                let text = escape_ass_text(&it.text);
                                ass.push_str(&format!(
                                    "Dialogue: 0,{},{},Default,,0,0,0,,{}\n",
                                    start, end, text
                                ));
                            }

                            let now = SystemTime::now()
                                .duration_since(UNIX_EPOCH)
                                .unwrap_or_else(|_| Duration::from_secs(0))
                                .as_millis();
                            let ass_path = std::env::temp_dir().join(format!("videoEditer_subs_{}.ass", now));
                            if fs::write(&ass_path, ass).is_ok() {
                                let fonts_dir = std::env::current_dir()
                                    .unwrap_or_else(|_| std::path::PathBuf::from("."))
                                    .join("fonts");

                                let ass_esc = escape_ffmpeg_filter_path(&ass_path);
                                let fonts_esc = escape_ffmpeg_filter_path(&fonts_dir);
                                filter.push_str(&format!(
                                    ";[outv]subtitles='{}':fontsdir='{}'[vsub]",
                                    ass_esc, fonts_esc
                                ));
                                map_video = "[vsub]".to_string();
                                tmp_ass_path = Some(ass_path);
                            } else {
                                let _ = tx.send("字幕ファイル生成に失敗（字幕なしで書き出します）".to_string());
                            }
                        } else {
                            let _ = tx.send("字幕が空 or 解析できません（字幕なしで書き出します）".to_string());
                        }
                    }

                    args.push("-filter_complex".to_string());
                    args.push(filter);
                    args.push("-map".to_string());
                    args.push(map_video);
                    if include_audio {
                        args.push("-map".to_string());
                        args.push("[outa]".to_string());
                    }
                    args.push("-c:v".to_string());
                    args.push("libx264".to_string());
                    if include_audio {
                        args.push("-c:a".to_string());
                        args.push("aac".to_string());
                    }
                    args.push(out_path_abs.to_string_lossy().to_string());

                    let _ = tx.send("ffmpeg concat で書き出し中...".to_string());

                    let output = Command::new("ffmpeg")
                        .args(&args)
                        .stderr(std::process::Stdio::piped())
                        .output();

                    match output {
                        Ok(outp) => {
                            let stderr = String::from_utf8_lossy(&outp.stderr).to_string();
                            if outp.status.success() {
                                let _ = tx.send(format!("出力成功: {}", out_path_abs.display()));
                            } else {
                                let _ = tx.send(format!(
                                    "出力失敗: status={} stderr={}",
                                    outp.status, stderr
                                ));
                            }
                        }
                        Err(e) => {
                            let _ = tx.send(format!("ffmpeg起動失敗: {}", e));
                        }
                    }

                    // 一時字幕ファイルを掃除
                    if let Some(p) = tmp_ass_path {
                        let _ = fs::remove_file(p);
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

        // 読み込み進捗のポーリング（現在は同期ロードのため未使用）

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
                            if s.starts_with("Export succeeded")
                                || s.starts_with("No frames to export")
                            {
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

        if self.playing && self.total_frames > 0 {
            if let Some((ci, local_idx)) = self.map_global(self.global_frame) {
                if let Some(clip) = self.playlist.get(ci) {
                    let now = Instant::now();
                    let frame_duration = Duration::from_secs_f32(1.0 / clip.fps.max(1.0));
                    if now.duration_since(self.last_update) >= frame_duration {
                        self.last_update = now;

                        if local_idx < clip.frames.len() {
                            let frame = &clip.frames[local_idx];
                            let src_w = clip.size.0;
                            let src_h = clip.size.1;
                            let mut dst_w = src_w;
                            let mut dst_h = src_h;
                            if src_w > MAX_UPLOAD_DIM || src_h > MAX_UPLOAD_DIM {
                                let scale = (MAX_UPLOAD_DIM as f32 / src_w as f32)
                                    .min(MAX_UPLOAD_DIM as f32 / src_h as f32);
                                dst_w = (src_w as f32 * scale).max(1.0) as u32;
                                dst_h = (src_h as f32 * scale).max(1.0) as u32;
                            }

                            let upload_buffer_vec: Option<Vec<u8>> = if dst_w != src_w
                                || dst_h != src_h
                            {
                                let mut out = vec![0u8; (dst_w as usize) * (dst_h as usize) * 3];
                                for y in 0..dst_h {
                                    for x in 0..dst_w {
                                        let src_x =
                                            ((x as f32) * (src_w as f32 / dst_w as f32)) as u32;
                                        let src_y =
                                            ((y as f32) * (src_h as f32 / dst_h as f32)) as u32;
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
                        }

                        // 次フレームへ
                        if self.global_frame + 1 < self.total_frames {
                            self.global_frame += 1;
                        } else {
                            self.playing = false;
                        }
                    }
                }
            }
            ctx.request_repaint();
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
