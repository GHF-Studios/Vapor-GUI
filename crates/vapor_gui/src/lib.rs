use clap::{Parser, ValueEnum};
use std::path::PathBuf;
use vapor_gui_core::{GuiLaunch, GuiMode, GuiProduct, run_product};

#[derive(Debug, Parser)]
#[command(
    name = "vapor-gui",
    about = "Vapor Launcher, Studio, IDE, and Installer"
)]
struct Args {
    #[arg(long, value_enum)]
    mode: Option<ModeArg>,
    #[arg(long)]
    workspace: Option<PathBuf>,
    #[arg(long)]
    app_root: Option<PathBuf>,
    #[arg(long)]
    first_launch: bool,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum ModeArg {
    Studio,
    Launcher,
    Play,
    Chooser,
    Installer,
    Ide,
}

impl From<ModeArg> for GuiMode {
    fn from(value: ModeArg) -> Self {
        match value {
            ModeArg::Studio => Self::Studio,
            ModeArg::Launcher => Self::Launcher,
            ModeArg::Play => Self::Play,
            ModeArg::Chooser => Self::Chooser,
            ModeArg::Installer => Self::Installer,
            ModeArg::Ide => Self::Ide,
        }
    }
}

pub fn run(default_mode: GuiMode) -> eframe::Result<()> {
    let args = Args::parse();
    let mode = args.mode.map(GuiMode::from).unwrap_or(default_mode);
    let launch = GuiLaunch {
        workspace: args.workspace,
        app_root: args.app_root,
        first_launch: args.first_launch,
    };
    match mode {
        GuiMode::Chooser => run_product(ChooserProduct, launch),
        GuiMode::Installer => run_product(InstallerProduct, launch),
        GuiMode::Ide => run_product(IdeProduct, launch),
        GuiMode::Launcher | GuiMode::Play => run_product(LauncherProduct, launch),
        GuiMode::Studio => run_product(StudioProduct, launch),
    }
}

struct StudioProduct;

impl GuiProduct for StudioProduct {
    fn title(&self) -> &'static str {
        "Vapor Studio"
    }

    fn create(
        &self,
        context: &eframe::CreationContext<'_>,
        launch: GuiLaunch,
    ) -> Box<dyn eframe::App> {
        Box::new(vapor_studio_ui::StudioApp::new(context, launch.workspace))
    }
}

struct LauncherProduct;

impl GuiProduct for LauncherProduct {
    fn title(&self) -> &'static str {
        "Vapor Launcher"
    }

    fn window_size(&self) -> [f32; 2] {
        [600.0, 420.0]
    }

    fn minimum_window_size(&self) -> [f32; 2] {
        [520.0, 360.0]
    }

    fn create(
        &self,
        context: &eframe::CreationContext<'_>,
        launch: GuiLaunch,
    ) -> Box<dyn eframe::App> {
        Box::new(vapor_launcher_ui::LauncherApp::new(context, launch))
    }
}

struct InstallerProduct;

struct IdeProduct;

struct ChooserProduct;

impl GuiProduct for ChooserProduct {
    fn title(&self) -> &'static str {
        "Vapor"
    }

    fn window_size(&self) -> [f32; 2] {
        [560.0, 430.0]
    }

    fn minimum_window_size(&self) -> [f32; 2] {
        [500.0, 370.0]
    }

    fn create(
        &self,
        _context: &eframe::CreationContext<'_>,
        _launch: GuiLaunch,
    ) -> Box<dyn eframe::App> {
        Box::new(ChooserApp::default())
    }
}

#[derive(Default)]
struct ChooserApp {
    message: Option<String>,
    close_requested: bool,
}

impl ChooserApp {
    fn open(&mut self, mode: &str, label: &str) {
        let result = (|| {
            let executable = std::env::current_exe()
                .map_err(|error| format!("could not find the Vapor app folder: {error}"))?;
            let entrypoint = executable
                .parent()
                .ok_or_else(|| "could not find the Vapor app folder".to_owned())?
                .join(format!("vapor-entrypoint{}", std::env::consts::EXE_SUFFIX));
            if !entrypoint.is_file() {
                return Err(format!("{label} is unavailable in this installation."));
            }
            vapor_core::managed_process::background_command(entrypoint)
                .arg(mode)
                .spawn()
                .map(|_| ())
                .map_err(|_| format!("Could not open {label}. Try repairing Vapor."))
        })();
        match result {
            Ok(()) => {
                self.message = None;
                self.close_requested = true;
            }
            Err(error) => self.message = Some(error),
        }
    }
}

impl eframe::App for ChooserApp {
    fn ui(&mut self, ui: &mut eframe::egui::Ui, _frame: &mut eframe::Frame) {
        ui.spacing_mut().item_spacing = eframe::egui::vec2(7.0, 4.0);
        ui.vertical_centered(|ui| {
            ui.add_space(8.0);
            ui.heading("Welcome to Vapor");
            ui.label("Choose an app to continue.");
            ui.add_space(12.0);
        });

        eframe::egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.group(|ui| {
                    ui.label(eframe::egui::RichText::new("APPS").strong());
                    ui.add_space(4.0);
                    for (title, description, mode) in [
                        (
                            "Vapor Launcher",
                            "Choose a composition and play.",
                            "launcher",
                        ),
                        (
                            "Vapor Studio",
                            "Discover content and create compositions.",
                            "studio",
                        ),
                        ("Vapor IDE", "Work on Rust projects and code.", "ide"),
                    ] {
                        self.product_row(ui, title, description, mode);
                    }
                });

                ui.add_space(8.0);
                ui.group(|ui| {
                    ui.label(eframe::egui::RichText::new("SETUP & ADVANCED TOOLS").strong());
                    ui.add_space(4.0);
                    self.product_row(
                        ui,
                        "Vapor Installer",
                        "Set up, repair, or change this installation.",
                        "installer",
                    );
                    self.product_row(
                        ui,
                        "Vapor Shell",
                        "Open the command line for advanced work.",
                        "shell",
                    );
                });
            });

        if let Some(message) = &self.message {
            ui.add_space(8.0);
            ui.colored_label(eframe::egui::Color32::LIGHT_RED, message);
        }
        if self.close_requested {
            ui.ctx()
                .send_viewport_cmd(eframe::egui::ViewportCommand::Close);
        }
    }
}

impl ChooserApp {
    fn product_row(
        &mut self,
        ui: &mut eframe::egui::Ui,
        title: &str,
        description: &str,
        mode: &str,
    ) {
        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                ui.strong(title);
                ui.label(description);
            });
            ui.with_layout(
                eframe::egui::Layout::right_to_left(eframe::egui::Align::Center),
                |ui| {
                    if ui.button("Open").clicked() {
                        self.open(mode, title);
                    }
                },
            );
        });
        ui.add_space(4.0);
    }
}

impl GuiProduct for IdeProduct {
    fn title(&self) -> &'static str {
        "Vapor IDE"
    }

    fn window_size(&self) -> [f32; 2] {
        [1480.0, 940.0]
    }

    fn minimum_window_size(&self) -> [f32; 2] {
        [860.0, 560.0]
    }

    fn create(
        &self,
        context: &eframe::CreationContext<'_>,
        launch: GuiLaunch,
    ) -> Box<dyn eframe::App> {
        Box::new(vapor_ide_ui::IdeApp::new(context, launch))
    }
}

impl GuiProduct for InstallerProduct {
    fn title(&self) -> &'static str {
        "Vapor Installer"
    }

    fn create(
        &self,
        _context: &eframe::CreationContext<'_>,
        launch: GuiLaunch,
    ) -> Box<dyn eframe::App> {
        Box::new(vapor_installer_ui::InstallerApp::new(
            launch.app_root,
            launch.first_launch,
        ))
    }
}
