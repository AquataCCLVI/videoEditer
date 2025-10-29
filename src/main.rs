use ::egui::epaint::textures;
use anyhow::Result;
use eframe::{App, egui};
use egui::TextureHandle;
use ffmpeg::{format, frame, media, software, util};
use ffmpeg_next as ffmpeg;
use image::RgbImage;
use std::collections::VecDeque;
use std::default::Default;
use std::time::{Duration, Instant};
use wgpu::util::DeviceExt;

//GPUプレビュー用構造体
struct GpuPreview {
    device: wgpu::Device,
    queue: wgpu::Queue,
    surface_texture: wgpu::Texture,
    texture_id: egui::TextureId,
}

impl GpuPreview {
    fn new() -> Self {
        let instance = wgpu::Instance::default();
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: None,
            force_fallback_adapter: false,
        }))
        .unwrap();

        let (device, queue) =
            pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default(), None))
                .unwrap();

        // ここではダミーテクスチャを生成（あとで動画フレームに差し替える）
        let size = wgpu::Extent3d {
            width: 640,
            height: 360,
            depth_or_array_layers: 1,
        };
        let surface_texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("preview_texture"),
            size,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });

        Self {
            device,
            queue,
            surface_texture,
            texture_id: egui::TextureId::User(0), // あとで登録予定
        }
    }

    fn upload_frame(&mut self, frame_data: &[u8], size: (u32, u32)) {
        let texture_extent = wgpu::Extent3d {
            width: size.0,
            height: size.1,
            depth_or_array_layers: 1,
        };

        self.queue.write_texture(
            wgpu::ImageCopyTexture {
                texture: &self.surface_texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            frame_data,
            wgpu::ImageDataLayout {
                offset: 0,
                bytes_per_row: Some(4 * size.0),
                rows_per_image: Some(size.1),
            },
            texture_extent,
        );
    }
}
struct VideoEditorApp {
    video_path_input: String,
    texture: Option<egui::TextureHandle>,
    playing: bool,
    last_update: Instant,
    frame_rgb: Option<Vec<u8>>,
    size: (u32, u32),
    fps: f32,
    renderer: Option<GpuPreview>,
}

impl Default for VideoEditorApp {
    fn default() -> Self {
        ffmpeg::init().unwrap();
        Self {
            video_path_input: String::new(),
            texture: None,
            playing: false,
            last_update: Instant::now(),
            frame_rgb: None,
            size: (1920, 1080),
            fps: 60.0,
            renderer: None,
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

        let ctx = ui.ctx().clone();

        ui.vertical(|ui| {
            let (rect, _res) = ui.allocate_exact_size(view_size, egui::Sense::hover());
            ui.painter()
                .rect_filled(rect, 0.0, egui::Color32::from_rgb(240, 200, 200));

            let mut view_child_ui = ui.child_ui(
                rect,
                egui::Layout::centered_and_justified(egui::Direction::TopDown),
            );

            // 表示

            // プレビュー画面用
            if self.renderer.is_none() {
                self.renderer = Some(GpuPreview::new());
            }

            // 動画サイズに合わせて表示サイズを調整
            let xsize = self.size.0 as f32;
            let ysize = self.size.1 as f32;
            let scale = if view_size.x / xsize < view_size.y / ysize {
                view_size.x / xsize as f32
            } else {
                view_size.y / ysize as f32
            };
            let video_size = egui::vec2(xsize * scale, ysize * scale);

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
                if let Ok((frame, (w, h), fps)) = self.extract_first_frame(&self.video_path_input) {
                    self.size = (w, h);
                    self.fps = fps;
                    self.frame_rgb = Some(frame.clone());

                    //egui用の画像形式に変換
                    let color_image = egui::ColorImage::from_rgb([w as usize, h as usize], &frame);

                    //eguiのコンテキストにテクスチャとして登録
                    let tex_handle = ui.ctx().load_texture(
                        "video_preview0",
                        color_image,
                        egui::TextureOptions::LINEAR,
                    );

                    //テクスチャハンドルを保存
                    self.texture = Some(tex_handle);
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
        });
    }

    fn extract_first_frame(
        &self,
        path: &str,
    ) -> std::result::Result<(Vec<u8>, (u32, u32), f32), ffmpeg::Error> {
        ffmpeg::format::input(&path).and_then(|mut ictx| {
            let input = ictx.streams().best(ffmpeg::media::Type::Video).unwrap();
            let video_stream_index = input.index();
            let fps = input.avg_frame_rate();
            let fps_val = if fps.1 != 0 {
                fps.0 as f32 / fps.1 as f32
            } else {
                30.0
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


            let mut decoded = ffmpeg::util::frame::video::Video::empty();
            for (stream, packet) in ictx.packets() {
                if stream.index() == video_stream_index {
                    decoder.send_packet(&packet)?;
                    if decoder.receive_frame(&mut decoded).is_ok() {
                        let mut rgb_frame = ffmpeg::util::frame::video::Video::empty();
                        scaler.run(&decoded, &mut rgb_frame)?;
                        let data = rgb_frame.data(0).to_vec();
                        return Ok((data, (rgb_frame.width(), rgb_frame.height()), fps_val));
                    }
                }
            }
            Err(ffmpeg::Error::StreamNotFound)
        })
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

    let app = VideoEditorApp::default();
    eframe::run_native("RustVideoEditor", options, Box::new(|_cc| Box::new(app)))
}
