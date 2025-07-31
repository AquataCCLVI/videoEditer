use eframe::{App, egui};
use std::collections::HashMap;

struct MyApp {
    textures: HashMap<String, egui::TextureHandle>,
}

impl App for MyApp {
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





        

        egui::CentralPanel::default().show(ctx, |ui| {
            let img_path = "C:\\Users\\aquata256\\Downloads\\GobfYI2XsAALjVr.jpg";
            
            // 画像を読み込む
            if !self.textures.contains_key(img_path) {
                if let Ok(img) = image::open(img_path) {
                    let rgba_img = img.to_rgba8();
                    let size = [rgba_img.width() as usize, rgba_img.height() as usize];
                    let pixels = rgba_img.into_raw();
                    let color_img = egui::ColorImage::from_rgba_unmultiplied(size, &pixels);
                    let texture = ctx.load_texture("my_image", color_img, egui::TextureOptions::default());
                    self.textures.insert(img_path.to_string(), texture);
                }
            }
            
            // 画像を表示
            if let Some(texture) = self.textures.get(img_path) {
                egui::ScrollArea::both().show(ui, |ui| {
                    ui.add(egui::Image::from_texture(texture).max_width(800.0));
                });
            } else {
                ui.label("画像を読み込めませんでした");
            }
        });
    }
}

fn main() -> Result<(), eframe::Error> {
    let options = eframe::NativeOptions::default();
    eframe::run_native(
        "My GUI App",
        options,
        Box::new(|_cc| Box::new(MyApp {
            textures: HashMap::new(),
        }) as Box<dyn App>),
    )
}
