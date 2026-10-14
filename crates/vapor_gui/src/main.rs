use clap::{Parser, ValueEnum};
use eframe::egui;
use std::{path::PathBuf, process::Command, sync::mpsc, thread};
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
        Box::new(InstallerApp::new(launch.app_root))
    }
}

struct InstallerApp {
    app_root: PathBuf,
    status: String,
    receiver: Option<mpsc::Receiver<Result<String, String>>>,
}

impl InstallerApp {
    fn new(app_root: Option<PathBuf>) -> Self {
        Self {
            app_root: app_root
                .or_else(|| std::env::var_os("VAPOR_HOME").map(PathBuf::from))
                .or_else(|| std::env::current_dir().ok())
                .unwrap_or_default(),
            status: "Ready to inspect or maintain this Vapor installation.".to_owned(),
            receiver: None,
        }
    }

    fn run_installer(&mut self, operation: &'static str) {
        let Some(executable) = std::env::current_exe()
            .ok()
            .and_then(|path| path.parent().map(|parent| parent.join(installer_name())))
        else {
            self.status = "Could not locate the installer recovery executable.".to_owned();
            return;
        };
        if !executable.is_file() {
            self.status = format!("Installer executable is missing: {}", executable.display());
            return;
        }
        let app_root = self.app_root.clone();
        let (sender, receiver) = mpsc::channel();
        self.receiver = Some(receiver);
        self.status = format!("Running installer {operation}…");
        thread::spawn(move || {
            let result = Command::new(executable)
                .arg(operation)
                .arg("--app-root")
                .arg(app_root)
                .output()
                .map_err(|error| error.to_string())
                .and_then(|output| {
                    let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
                    text.push_str(&String::from_utf8_lossy(&output.stderr));
                    if output.status.success() {
                        Ok(text)
                    } else {
                        Err(if text.is_empty() {
                            format!("Installer exited with {}", output.status)
                        } else {
                            text
                        })
                    }
                });
            let _ = sender.send(result);
        });
    }
}

impl eframe::App for InstallerApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        if let Some(result) = self.receiver.as_ref().and_then(|receiver| receiver.try_recv().ok()) {
            self.receiver = None;
            self.status = result.unwrap_or_else(|error| error);
        }
        egui::CentralPanel::default().show_inside(ui, |ui| {
            ui.heading("Vapor Installer");
            ui.label("Manage the installed Vapor app root.");
            ui.horizontal(|ui| {
                if ui.add_enabled(self.receiver.is_none(), egui::Button::new("Check status")).clicked() {
                    self.run_installer("status");
                }
                if ui.add_enabled(self.receiver.is_none(), egui::Button::new("Install or repair")).clicked() {
                    self.run_installer("install");
                }
                if ui.add_enabled(self.receiver.is_none(), egui::Button::new("Uninstall")).clicked() {
                    self.run_installer("uninstall");
                }
            });
            ui.separator();
            ui.label(format!("App root: {}", self.app_root.display()));
            ui.label(&self.status);
        });
        if self.receiver.is_some() {
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(100));
        }
    }
}

#[cfg(windows)]
fn installer_name() -> &'static str {
    "vapor-installer.exe"
}

#[cfg(not(windows))]
fn installer_name() -> &'static str {
    "vapor-installer"
}
