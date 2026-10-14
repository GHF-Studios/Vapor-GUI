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
        play: bool,
    ) -> Box<dyn eframe::App>;
}

pub fn run_product(
    product: impl GuiProduct + 'static,
    launch: GuiLaunch,
    play: bool,
) -> eframe::Result<()> {
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
        Box::new(move |context| Ok(product.create(context, launch, play))),
    )
}
