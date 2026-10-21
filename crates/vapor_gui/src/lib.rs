use clap::{Parser, ValueEnum};
use std::path::PathBuf;
use vapor_gui_core::{GuiLaunch, GuiMode, GuiProduct, run_product};

#[derive(Debug, Parser)]
#[command(name = "vapor-gui", about = "Vapor Launcher, Studio, and Installer")]
struct Args {
    #[arg(long, value_enum)]
    mode: Option<ModeArg>,
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

pub fn run(default_mode: GuiMode) -> eframe::Result<()> {
    let args = Args::parse();
    let mode = args.mode.map(GuiMode::from).unwrap_or(default_mode);
    let launch = GuiLaunch {
        workspace: args.workspace,
        app_root: args.app_root,
    };
    match mode {
        GuiMode::Installer => run_product(InstallerProduct, launch),
        GuiMode::Play => run_product(LauncherProduct, launch),
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

    fn create(
        &self,
        context: &eframe::CreationContext<'_>,
        launch: GuiLaunch,
    ) -> Box<dyn eframe::App> {
        Box::new(vapor_launcher_ui::LauncherApp::new(context, launch))
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
    ) -> Box<dyn eframe::App> {
        Box::new(vapor_installer_ui::InstallerApp::new(launch.app_root))
    }
}
