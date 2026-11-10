use eframe::egui::{self, Color32, RichText};
use std::{
    sync::mpsc::{self, Receiver},
    thread,
};
use vapor_core::{
    VaporInstallation,
    storage::{self, CleanupCandidate, CleanupCost, CleanupResult, StorageInventory},
};

enum WorkerResult {
    Inventory(Result<StorageInventory, String>),
    Removed(Result<CleanupResult, String>),
}

pub struct StorageApp {
    inventory: Option<StorageInventory>,
    selected: Option<String>,
    confirmation: String,
    search: String,
    message: Option<(bool, String)>,
    worker: Option<Receiver<WorkerResult>>,
    loading: bool,
}

impl Default for StorageApp {
    fn default() -> Self {
        let mut app = Self {
            inventory: None,
            selected: None,
            confirmation: String::new(),
            search: String::new(),
            message: None,
            worker: None,
            loading: false,
        };
        app.refresh();
        app
    }
}

impl StorageApp {
    fn refresh(&mut self) {
        let (sender, receiver) = mpsc::channel();
        self.worker = Some(receiver);
        self.loading = true;
        self.message = None;
        thread::spawn(move || {
            let result = VaporInstallation::discover()
                .map_err(|error| error.to_string())
                .and_then(|installation| storage::inspect(&installation));
            let _ = sender.send(WorkerResult::Inventory(result));
        });
    }

    fn remove(&mut self, candidate: &CleanupCandidate) {
        let Some(inventory) = self.inventory.as_ref() else {
            return;
        };
        let home = inventory.home.clone();
        let id = candidate.id.clone();
        let bytes = candidate.bytes;
        let confirmation = self.confirmation.clone();
        let (sender, receiver) = mpsc::channel();
        self.worker = Some(receiver);
        self.loading = true;
        self.message = None;
        thread::spawn(move || {
            let result = VaporInstallation::from_explicit_root(home)
                .map_err(|error| error.to_string())
                .and_then(|installation| storage::remove(&installation, &id, bytes, &confirmation));
            let _ = sender.send(WorkerResult::Removed(result));
        });
    }

    fn poll_worker(&mut self) {
        let result = self
            .worker
            .as_ref()
            .and_then(|worker| worker.try_recv().ok());
        let Some(result) = result else { return };
        self.worker = None;
        self.loading = false;
        match result {
            WorkerResult::Inventory(Ok(inventory)) => {
                self.inventory = Some(inventory);
                self.selected = None;
                self.confirmation.clear();
            }
            WorkerResult::Inventory(Err(error)) => self.message = Some((false, error)),
            WorkerResult::Removed(Ok(result)) => {
                let message = format!(
                    "Removed {} · {} logical bytes. Actual disk space reclaimed may differ.",
                    result.relative_path,
                    format_bytes(result.measured_bytes_before_delete)
                );
                self.selected = None;
                self.confirmation.clear();
                self.refresh();
                self.message = Some((true, message));
            }
            WorkerResult::Removed(Err(error)) => self.message = Some((false, error)),
        }
    }
}

impl eframe::App for StorageApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.poll_worker();
        ui.spacing_mut().item_spacing = egui::vec2(8.0, 6.0);
        ui.horizontal(|ui| {
            ui.heading("Vapor Storage");
            if self.loading {
                ui.spinner();
                ui.weak("Scanning…");
            }
            if ui.button("Refresh").clicked() && !self.loading {
                self.refresh();
            }
        });
        ui.weak("Review the canonical Vapor home. Source files, content, tools, and state stay protected.");

        if let Some((success, message)) = &self.message {
            let color = if *success {
                Color32::LIGHT_GREEN
            } else {
                Color32::LIGHT_RED
            };
            ui.colored_label(color, message);
        }
        let Some(inventory) = self.inventory.clone() else {
            if !self.loading {
                ui.label("No inventory available. Refresh to try again.");
            }
            return;
        };

        ui.horizontal(|ui| {
            ui.label(
                RichText::new(format_bytes(inventory.total_bytes))
                    .strong()
                    .size(20.0),
            );
            ui.label("logical size in the canonical Vapor home");
            if inventory.scan_incomplete {
                ui.colored_label(
                    Color32::YELLOW,
                    "Some items could not be measured; totals may be low.",
                );
            }
        });
        ui.small(inventory.home.display().to_string());
        ui.separator();

        ui.strong("Cleanup candidates");
        ui.weak("Each row is one generated-data folder. Close active builds first; removing build output makes the next build recompile.");
        ui.horizontal(|ui| {
            ui.label("Find");
            ui.add(
                egui::TextEdit::singleline(&mut self.search)
                    .desired_width(300.0)
                    .hint_text("Name, category, or folder"),
            );
            if !self.search.is_empty() && ui.small_button("Clear").clicked() {
                self.search.clear();
            }
        });
        let query = self.search.trim().to_ascii_lowercase();
        egui::ScrollArea::vertical()
            .id_salt("storage-candidates")
            .show(ui, |ui| {
                egui::Grid::new("storage-candidate-grid")
                    .striped(true)
                    .num_columns(5)
                    .show(ui, |ui| {
                        ui.strong("Item / location");
                        ui.strong("Category");
                        ui.strong("Space");
                        ui.strong("Impact");
                        ui.strong("Action");
                        ui.end_row();
                        for candidate in inventory.candidates.iter().filter(|candidate| {
                            query.is_empty()
                                || candidate.label.to_ascii_lowercase().contains(&query)
                                || candidate.category.to_ascii_lowercase().contains(&query)
                                || candidate
                                    .relative_path
                                    .to_ascii_lowercase()
                                    .contains(&query)
                        }) {
                            let selected = self.selected.as_deref() == Some(candidate.id.as_str());
                            ui.vertical(|ui| {
                                if ui.selectable_label(selected, &candidate.label).clicked() {
                                    self.selected = Some(candidate.id.clone());
                                    self.confirmation.clear();
                                }
                                ui.small(RichText::new(candidate.relative_path.as_str()).weak());
                                ui.small(
                                    RichText::new(format!("ID: {}", candidate.id))
                                        .monospace()
                                        .weak(),
                                );
                            });
                            ui.label(&candidate.category);
                            ui.label(format_bytes(candidate.bytes));
                            let (tag, color) = match candidate.cost {
                                CleanupCost::Rebuild => ("SLOWER REBUILD", Color32::YELLOW),
                                CleanupCost::Redownload => ("RE-DOWNLOAD", Color32::LIGHT_BLUE),
                                CleanupCost::Recreate => ("RECREATE OUTPUT", Color32::YELLOW),
                            };
                            ui.colored_label(color, tag);
                            if ui.button("Review").clicked() {
                                self.selected = Some(candidate.id.clone());
                                self.confirmation.clear();
                            }
                            ui.end_row();
                        }
                    });
            });

        let selected = self
            .selected
            .as_deref()
            .and_then(|id| {
                inventory
                    .candidates
                    .iter()
                    .find(|candidate| candidate.id == id)
            })
            .cloned();
        if let Some(candidate) = selected {
            ui.separator();
            ui.strong(format!("Review: {}", candidate.relative_path));
            ui.label(format!(
                "{} · {} logical size",
                candidate.category,
                format_bytes(candidate.bytes)
            ));
            ui.label(&candidate.consequence);
            ui.horizontal(|ui| {
                let expected = format!("DELETE {}", candidate.id);
                ui.label(RichText::new(&expected).monospace());
                ui.add(
                    egui::TextEdit::singleline(&mut self.confirmation)
                        .desired_width(300.0)
                        .hint_text("Type the phrase shown"),
                );
                let enabled =
                    !self.loading && self.confirmation == format!("DELETE {}", candidate.id);
                if ui
                    .add_enabled(enabled, egui::Button::new("Delete selected data"))
                    .clicked()
                {
                    self.remove(&candidate);
                }
                if ui.button("Cancel").clicked() {
                    self.selected = None;
                    self.confirmation.clear();
                }
            });
        }

        ui.separator();
        egui::CollapsingHeader::new("Protected and retained folders")
            .default_open(false)
            .show(ui, |ui| {
                egui::Grid::new("storage-protected-grid")
                    .striped(true)
                    .num_columns(3)
                    .show(ui, |ui| {
                        ui.strong("Folder");
                        ui.strong("Space");
                        ui.strong("Why it stays");
                        ui.end_row();
                        for area in inventory.areas.iter().filter(|area| {
                            query.is_empty()
                                || area.name.to_ascii_lowercase().contains(&query)
                                || area.relative_path.to_ascii_lowercase().contains(&query)
                                || area.explanation.to_ascii_lowercase().contains(&query)
                        }) {
                            ui.label(&area.relative_path);
                            ui.label(format_bytes(area.bytes));
                            let color = match area.importance {
                                vapor_core::storage::StorageImportance::Essential => {
                                    Color32::LIGHT_GREEN
                                }
                                vapor_core::storage::StorageImportance::Mixed => Color32::YELLOW,
                                vapor_core::storage::StorageImportance::Unclassified => {
                                    Color32::LIGHT_RED
                                }
                            };
                            ui.colored_label(color, &area.explanation);
                            ui.end_row();
                        }
                    });
            });
    }
}

fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut amount = bytes as f64;
    let mut unit = 0;
    while amount >= 1024.0 && unit < UNITS.len() - 1 {
        amount /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{amount:.2} {}", UNITS[unit])
    }
}
