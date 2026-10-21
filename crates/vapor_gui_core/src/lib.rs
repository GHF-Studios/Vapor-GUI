use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GuiMode {
    Studio,
    Play,
    Installer,
}

#[derive(Debug, Clone, Default)]
pub struct GuiLaunch {
    pub workspace: Option<PathBuf>,
    pub app_root: Option<PathBuf>,
}

pub trait GuiProduct {
    fn title(&self) -> &'static str;
    fn create(
        &self,
        context: &eframe::CreationContext<'_>,
        launch: GuiLaunch,
    ) -> Box<dyn eframe::App>;
}

pub fn run_product(product: impl GuiProduct + 'static, launch: GuiLaunch) -> eframe::Result<()> {
    let title = product.title();
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title(title)
            .with_inner_size([1440.0, 900.0])
            .with_min_inner_size([980.0, 640.0]),
        ..Default::default()
    };
    eframe::run_native(
        title,
        options,
        Box::new(move |context| {
            // All Vapor GUI products start from the same theme. Individual
            // products may refine the shared dark palette, but the host OS
            // theme must not make Installer and Studio look like different apps.
            context
                .egui_ctx
                .set_theme(eframe::egui::ThemePreference::Dark);
            context.egui_ctx.set_visuals(eframe::egui::Visuals::dark());
            Ok(product.create(context, launch))
        }),
    )
}
