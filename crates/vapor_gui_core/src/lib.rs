use std::path::PathBuf;

pub mod code_editor;
pub mod console;
pub mod icons;
pub mod panel;
pub mod rhai_semantics;
pub mod semantic;
pub mod workspace_tree;

pub use vapor_console_core::{
    ConsoleFilter, ConsoleHistory, ConsoleLevel, ConsoleRecord, DiagnosticContext,
    DiagnosticOrigin, DiagnosticScope, DiagnosticSeverity, DiagnosticSpan, StructuredDiagnostic,
    plain_terminal_text,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GuiMode {
    Studio,
    Launcher,
    /// Backwards-compatible name accepted by the original GUI host.
    Play,
    Chooser,
    Installer,
    Ide,
}

#[derive(Debug, Clone, Default)]
pub struct GuiLaunch {
    pub workspace: Option<PathBuf>,
    pub app_root: Option<PathBuf>,
    pub first_launch: bool,
}

pub trait GuiProduct {
    fn title(&self) -> &'static str;

    fn window_size(&self) -> [f32; 2] {
        [1440.0, 900.0]
    }

    fn minimum_window_size(&self) -> [f32; 2] {
        [980.0, 640.0]
    }

    fn create(
        &self,
        context: &eframe::CreationContext<'_>,
        launch: GuiLaunch,
    ) -> Box<dyn eframe::App>;
}

pub fn run_product(product: impl GuiProduct + 'static, launch: GuiLaunch) -> eframe::Result<()> {
    let title = product.title();
    let window_size = product.window_size();
    let minimum_window_size = product.minimum_window_size();
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title(title)
            .with_inner_size(window_size)
            .with_min_inner_size(minimum_window_size),
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
