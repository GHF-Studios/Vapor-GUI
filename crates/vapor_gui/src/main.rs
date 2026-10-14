use clap::{Parser, ValueEnum};
use eframe::egui;
use std::path::PathBuf;
use vapor_gui_core::{GuiLaunch, GuiMode, GuiProduct, run_product};

#[derive(Debug, Parser)]
#[command(name = "vapor-gui", about = "Vapor Studio and Installer")]
struct Args {
    #[arg(long, value_enum, default_value_t = ModeArg::Studio)]
    mode: ModeArg,
    #[arg(long)]
    workspace: Option<PathBuf>,
    #[arg(long)]
    app_root: Option<PathBuf>,
}
#[derive(Debug, Clone, Copy, ValueEnum)]
enum ModeArg {
    Studio,
    Play,
    Installer,
}

impl From<ModeArg> for GuiMode {
    fn from(value: ModeArg) -> Self {
        match value {
            ModeArg::Studio => Self::Studio,
            ModeArg::Play => Self::Play,
            ModeArg::Installer => Self::Installer,
        }
    }
}

fn main() -> eframe::Result<()> {
    let args = Args::parse();
    let mode = GuiMode::from(args.mode);
    let launch = GuiLaunch {
        workspace: args.workspace,
        app_root: args.app_root,
    };
    if mode == GuiMode::Installer {
        return run_product(InstallerProduct, launch, false);
    }
    run_product(StudioProduct, launch, mode == GuiMode::Play)
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
        play: bool,
    ) -> Box<dyn eframe::App> {
        Box::new(vapor_studio_ui::StudioApp::new(
            context,
            launch.workspace,
            play,
        ))
    }
}

struct InstallerProduct;

impl GuiProduct for InstallerProduct {
    fn title(&self) -> &'static str {
        "Vapor Installer"
    }

    fn create(
        &self,
        _context: &eframe::CreationContext<'_>,
        launch: GuiLaunch,
        _play: bool,
    ) -> Box<dyn eframe::App> {
        Box::new(vapor_installer_ui::InstallerApp::new(launch.app_root))
    }
}
