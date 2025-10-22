use eframe::{App, egui};


struct VideoEditorApp {

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
                draw_left_column(ui, view_size, timeline_size);

                draw_right_column(ui, option_size);  
                
            });
        });
    }
}

fn draw_left_column(ui: &mut egui::Ui, view_size: egui::Vec2, timeline_size: egui::Vec2) {
    // 左カラムの描画

    ui.vertical(|ui| {
        let (rect, _res) = ui.allocate_exact_size(view_size, egui::Sense::hover());
        ui.painter()
            .rect_filled(rect, 0.0, egui::Color32::from_rgb(240, 200, 200));

        let mut view_child_ui = ui.child_ui(
            rect,
            egui::Layout::centered_and_justified(egui::Direction::TopDown),
        );
        view_child_ui.label("プレビュー画面");

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

fn draw_right_column(ui: &mut egui::Ui, option_size: egui::Vec2) {
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

fn main() -> Result<(), eframe::Error> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1280.0, 720.0])
            .with_resizable(false),
        ..eframe::NativeOptions::default()
    };
    eframe::run_native(
        "My GUI App",
        options,
        Box::new(|_cc| {
            Box::new(VideoEditorApp {}) as Box<dyn App>
        }),
    )
}
