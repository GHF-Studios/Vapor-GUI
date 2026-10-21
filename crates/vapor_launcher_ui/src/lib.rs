use eframe::egui;
use std::{
    sync::mpsc::{self, Receiver, TryRecvError},
    thread,
    time::{Duration, Instant},
};
use vapor_client::{Client, ClientHostConfig, service_sibling_path};
use vapor_client_protocol::{
    CatalogAction, CatalogItemRole, CatalogLocationKind, ClientAction, ClientEvent, Composition,
    CompositionAction, ContentKind, InstallationState, OperationStatus, ProtocolFrame,
    WorkshopAction, WorkshopSnapshot,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum Page {
    #[default]
    Play,
    Library,
    Workshop,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CompositionIntent {
    FindGame,
    Launch,
    Install,
    Verify,
    Update,
    Repair,
}

fn composition_intent(composition: &Composition) -> CompositionIntent {
    if composition.root_package.is_none() {
        return CompositionIntent::FindGame;
    }
    match &composition.installation {
        InstallationState::Installed { .. } => CompositionIntent::Launch,
        InstallationState::NotInstalled => CompositionIntent::Install,
        InstallationState::Unknown => CompositionIntent::Verify,
        InstallationState::OutOfDate { .. } => CompositionIntent::Update,
        InstallationState::Partial { .. } | InstallationState::NeedsRepair { .. } => {
            CompositionIntent::Repair
        }
    }
}

fn composition_action(composition: &Composition) -> (CompositionIntent, Option<ClientAction>) {
    let intent = composition_intent(composition);
    let action = match intent {
        CompositionIntent::FindGame => None,
        CompositionIntent::Launch => Some(CompositionAction::Launch {
            composition_id: composition.id.clone(),
        }),
        CompositionIntent::Install => Some(CompositionAction::Install {
            composition_id: composition.id.clone(),
        }),
        CompositionIntent::Verify => Some(CompositionAction::Verify {
            composition_id: composition.id.clone(),
        }),
        CompositionIntent::Update => Some(CompositionAction::Update {
            composition_id: composition.id.clone(),
        }),
        CompositionIntent::Repair => Some(CompositionAction::Repair {
            composition_id: composition.id.clone(),
        }),
    };
    (intent, action.map(ClientAction::Composition))
}

pub struct LauncherApp {
    page: Page,
    service: Option<Client>,
    connecting: Option<Receiver<Result<Client, String>>>,
    connect_attempted: bool,
    reconnect_delay: Duration,
    next_connect_attempt: Instant,
    service_error: Option<String>,
    query: String,
    workshop_query: String,
    workshop_kind: Option<ContentKind>,
    status: String,
}

impl LauncherApp {
    pub fn new(
        _creation: &eframe::CreationContext<'_>,
        _launch: vapor_gui_core::GuiLaunch,
    ) -> Self {
        Self {
            page: Page::Play,
            service: None,
            connecting: None,
            connect_attempted: false,
            reconnect_delay: Duration::from_secs(1),
            next_connect_attempt: Instant::now(),
            service_error: None,
            query: String::new(),
            workshop_query: String::new(),
            workshop_kind: None,
            status: "Connecting to Vapor…".to_owned(),
        }
    }

    fn begin_connect(&mut self) {
        if self.service.is_some()
            || self.connecting.is_some()
            || self.connect_attempted
            || Instant::now() < self.next_connect_attempt
        {
            return;
        }
        self.connect_attempted = true;
        self.next_connect_attempt = Instant::now() + self.reconnect_delay;
        self.reconnect_delay = (self.reconnect_delay * 2).min(Duration::from_secs(30));
        let (sender, receiver) = mpsc::channel();
        self.connecting = Some(receiver);
        self.service_error = None;
        self.status = "Connecting to Vapor…".to_owned();
        let attempt = thread::Builder::new()
            .name("vapor-launcher-service-connect".to_owned())
            .spawn(move || {
                let result = connect_service().map_err(|error| error.to_string());
                let _ = sender.send(result);
            });
        if let Err(error) = attempt {
            self.connecting = None;
            self.connect_attempted = false;
            self.service_error = Some(format!("Could not start Vapor: {error}"));
        }
    }

    fn poll_connection(&mut self) {
        let result = self.connecting.as_ref().map(Receiver::try_recv);
        match result {
            Some(Ok(Ok(mut client))) => {
                self.connecting = None;
                self.reconnect_delay = Duration::from_secs(1);
                self.next_connect_attempt = Instant::now() + self.reconnect_delay;
                let _ = client.send(ClientAction::Catalog(CatalogAction::Refresh), None);
                let _ = client.send(
                    ClientAction::Workshop(WorkshopAction::LoadSubscriptions),
                    None,
                );
                self.service = Some(client);
                self.status = "Ready".to_owned();
            }
            Some(Ok(Err(error))) => {
                self.connecting = None;
                self.connect_attempted = false;
                self.service_error = Some(error);
                self.status = "Vapor could not be reached.".to_owned();
            }
            Some(Err(TryRecvError::Disconnected)) => {
                self.connecting = None;
                self.connect_attempted = false;
                self.service_error = Some("Vapor could not finish starting.".to_owned());
                self.status = "Vapor could not be reached.".to_owned();
            }
            Some(Err(TryRecvError::Empty)) | None => {}
        }
    }

    fn poll_service(&mut self) {
        let Some(service) = self.service.as_mut() else {
            return;
        };
        let mut disconnected = false;
        loop {
            match service.try_recv() {
                Ok(Some(ProtocolFrame::Event(ClientEvent::OperationChanged(event)))) => {
                    self.status = event.message.unwrap_or_else(|| match event.status {
                        OperationStatus::Running => "Working…".to_owned(),
                        OperationStatus::Cancelling => "Stopping…".to_owned(),
                        OperationStatus::Succeeded => "Done".to_owned(),
                        OperationStatus::Failed { message } => message,
                        OperationStatus::Cancelled => "Cancelled".to_owned(),
                    });
                }
                Ok(Some(ProtocolFrame::Response(response))) => {
                    if let Some(error) = response.error {
                        self.status = error.message;
                    }
                }
                Ok(Some(_)) => {}
                Ok(None) => break,
                Err(error) => {
                    self.service_error = Some(error.to_string());
                    self.status = "Vapor connection was lost.".to_owned();
                    disconnected = true;
                    break;
                }
            }
        }
        if disconnected {
            self.service = None;
            self.connect_attempted = false;
            self.next_connect_attempt = Instant::now() + self.reconnect_delay;
        }
    }

    fn send(&mut self, action: ClientAction) {
        let Some(service) = self.service.as_mut() else {
            return;
        };
        match service.send(action, None) {
            Ok(_) => self.status = "Request sent to Vapor…".to_owned(),
            Err(error) => {
                self.service_error = Some(error.to_string());
                self.status = "Vapor connection was lost.".to_owned();
            }
        }
    }

    fn play_page(&mut self, ui: &mut egui::Ui) {
        ui.heading("Your games");
        ui.label("Choose a composition, then install, repair, update, or play it.");
        ui.add_space(12.0);
        let Some(snapshot) = self
            .service
            .as_ref()
            .map(|service| service.latest_snapshot().clone())
        else {
            ui.label("Your compositions will appear here when Vapor is ready.");
            return;
        };
        let compositions = &snapshot.durable_state.compositions.compositions;
        if compositions.is_empty() {
            ui.label("No compositions yet. Browse Workshop or create one in Vapor Studio.");
            return;
        }
        ui.horizontal(|ui| {
            ui.label("Search");
            ui.text_edit_singleline(&mut self.query);
        });
        let query = self.query.trim().to_lowercase();
        let filtered: Vec<_> = compositions
            .iter()
            .filter(|item| query.is_empty() || item.name.to_lowercase().contains(&query))
            .cloned()
            .collect();
        ui.add_space(8.0);
        egui::ScrollArea::vertical().show(ui, |ui| {
            for composition in filtered {
                let active = snapshot.active_composition_id.as_deref() == Some(&composition.id);
                let (intent, action) = composition_action(&composition);
                egui::Frame::group(ui.style()).show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.vertical(|ui| {
                            ui.heading(&composition.name);
                            if active {
                                ui.label("Selected composition");
                            }
                            ui.label(composition_summary(&composition));
                        });
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.button(intent_label(intent)).clicked() {
                                if let Some(action) = action.clone() {
                                    self.send(action);
                                } else {
                                    self.page = Page::Workshop;
                                    self.send(ClientAction::Workshop(WorkshopAction::Search {
                                        query: String::new(),
                                        page: 0,
                                        required_tags: Vec::new(),
                                    }));
                                }
                            }
                            if !active && ui.button("Select").clicked() {
                                self.send(ClientAction::Composition(CompositionAction::Select {
                                    composition_id: composition.id.clone(),
                                }));
                            }
                        });
                    });
                });
                ui.add_space(6.0);
            }
        });
    }

    fn library_page(&mut self, ui: &mut egui::Ui) {
        ui.heading("Library");
        ui.label("Installed and downloaded content available to your compositions.");
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            ui.label("Search");
            ui.text_edit_singleline(&mut self.query);
        });
        let Some(snapshot) = self
            .service
            .as_ref()
            .map(|service| service.latest_snapshot().clone())
        else {
            ui.label("Your Library will appear here when Vapor is ready.");
            return;
        };
        if !snapshot.catalog_status.is_current() {
            ui.label(
                snapshot
                    .catalog_status
                    .error
                    .as_deref()
                    .unwrap_or("Refreshing Library…"),
            );
            return;
        }
        let query = self.query.trim().to_lowercase();
        let entries: Vec<_> = snapshot
            .catalog
            .iter()
            .filter(|entry| {
                matches!(
                    entry.role,
                    CatalogItemRole::PlayableComposition | CatalogItemRole::RuntimeContent
                ) && (query.is_empty()
                    || entry.id.to_lowercase().contains(&query)
                    || entry
                        .name
                        .as_deref()
                        .unwrap_or_default()
                        .to_lowercase()
                        .contains(&query)
                    || entry.kind.to_lowercase().contains(&query))
            })
            .cloned()
            .collect();
        if entries.is_empty() {
            ui.label("No player content found. Discover more in Workshop.");
        }
        egui::ScrollArea::vertical().show(ui, |ui| {
            for entry in entries {
                let is_installed = entry.locations.iter().any(|location| {
                    location.enabled && matches!(location.kind, CatalogLocationKind::Installed)
                });
                egui::Frame::group(ui.style()).show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.vertical(|ui| {
                            ui.heading(entry.name.as_deref().unwrap_or(&entry.id));
                            ui.label(format!(
                                "{} · {}",
                                friendly_kind(&entry.kind),
                                entry.version.as_deref().unwrap_or("version unknown")
                            ));
                        });
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if is_installed {
                                ui.label("Available");
                            } else if entry.published_file_id.is_some() {
                                if ui.button("Find in Workshop").clicked() {
                                    self.workshop_query =
                                        entry.name.as_deref().unwrap_or(&entry.id).to_owned();
                                    self.page = Page::Workshop;
                                    self.send(ClientAction::Workshop(WorkshopAction::Search {
                                        query: self.workshop_query.clone(),
                                        page: 0,
                                        required_tags: Vec::new(),
                                    }));
                                }
                            } else {
                                ui.label("Not installed");
                            }
                        });
                    });
                });
                ui.add_space(6.0);
            }
        });
    }

    fn workshop_page(&mut self, ui: &mut egui::Ui) {
        ui.heading("Workshop");
        ui.label("Find community content and manage your subscriptions.");
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            ui.label("Search");
            ui.text_edit_singleline(&mut self.workshop_query);
            egui::ComboBox::from_label("Content type")
                .selected_text(
                    self.workshop_kind
                        .map(|kind| friendly_kind(kind.as_str()))
                        .unwrap_or("All types"),
                )
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.workshop_kind, None, "All types");
                    for kind in ContentKind::ALL
                        .into_iter()
                        .filter(|kind| *kind != ContentKind::Crate)
                    {
                        ui.selectable_value(
                            &mut self.workshop_kind,
                            Some(kind),
                            friendly_kind(kind.as_str()),
                        );
                    }
                });
            if ui.button("Search Workshop").clicked() {
                let required_tags = self
                    .workshop_kind
                    .map(|kind| vec![kind.as_str().to_owned()])
                    .unwrap_or_default();
                self.send(ClientAction::Workshop(WorkshopAction::Search {
                    query: self.workshop_query.clone(),
                    page: 0,
                    required_tags,
                }));
            }
        });
        let Some(snapshot) = self
            .service
            .as_ref()
            .map(|service| service.latest_snapshot().workshop.clone())
        else {
            ui.label("Workshop will be available when Vapor is ready.");
            return;
        };
        workshop_page_contents(self, ui, snapshot);
    }
}

fn workshop_page_contents(app: &mut LauncherApp, ui: &mut egui::Ui, snapshot: WorkshopSnapshot) {
    let subscription_ids = snapshot_subscription_ids(&snapshot);
    ui.separator();
    ui.heading("Subscriptions");
    if !snapshot.subscriptions_loaded {
        ui.label("Loading your subscriptions…");
    } else if snapshot.subscriptions.is_empty() {
        ui.label("You have no subscriptions yet.");
    }
    for item in &snapshot.subscriptions {
        ui.horizontal(|ui| {
            ui.label(&item.title);
            ui.label(if item.downloading {
                "Downloading…"
            } else if item.needs_update {
                "Update available"
            } else if item.installed {
                "Ready"
            } else {
                "Subscribed"
            });
            if !item.installed && !item.downloading && ui.button("Download").clicked() {
                app.send(ClientAction::Workshop(WorkshopAction::Acquire {
                    item_ids: vec![item.published_file_id.clone()],
                }));
            }
            if ui.button("Unsubscribe").clicked() {
                app.send(ClientAction::Workshop(WorkshopAction::Unsubscribe {
                    item_ids: vec![item.published_file_id.clone()],
                }));
            }
        });
    }
    ui.separator();
    ui.heading("Search results");
    if let Some(results) = snapshot.search {
        if results.items.is_empty() {
            ui.label("No matching items.");
        }
        for item in results.items {
            let subscribed = subscription_ids.contains(&item.published_file_id);
            ui.horizontal(|ui| {
                ui.vertical(|ui| {
                    ui.label(egui::RichText::new(&item.title).strong());
                    ui.label(
                        workshop_item_kind(&item.tags)
                            .map(|kind| friendly_kind(kind.as_str()))
                            .unwrap_or("Community content"),
                    );
                });
                if !subscribed && ui.button("Subscribe").clicked() {
                    app.send(ClientAction::Workshop(WorkshopAction::Subscribe {
                        item_ids: vec![item.published_file_id],
                    }));
                }
            });
        }
    } else {
        ui.label("Search to discover available games and content.");
    }
}

fn snapshot_subscription_ids(snapshot: &WorkshopSnapshot) -> Vec<String> {
    snapshot
        .subscriptions
        .iter()
        .map(|item| item.published_file_id.clone())
        .collect()
}

fn connect_service() -> Result<Client, vapor_client::ClientError> {
    let executable = std::env::current_exe()
        .map_err(|error| vapor_client::ClientError::Io(error.to_string()))?;
    let config = ClientHostConfig {
        application_executable: executable.clone(),
        workshop_app_id: None,
        workspace_root: None,
        active_composition_id: None,
        fixture_mode: false,
        legacy_storage: Default::default(),
        state_directory: None,
    };
    Client::connect_or_start(&service_sibling_path(&executable), config)
}

fn intent_label(intent: CompositionIntent) -> &'static str {
    match intent {
        CompositionIntent::FindGame => "Find a game",
        CompositionIntent::Launch => "Play",
        CompositionIntent::Install => "Install",
        CompositionIntent::Verify => "Check setup",
        CompositionIntent::Update => "Update",
        CompositionIntent::Repair => "Repair",
    }
}

fn composition_summary(composition: &Composition) -> String {
    if composition.root_package.is_none() {
        return "Choose a game setup to get started".to_owned();
    }
    match &composition.installation {
        InstallationState::Installed { .. } => "Ready to play".to_owned(),
        InstallationState::NotInstalled => "Not installed".to_owned(),
        InstallationState::Unknown => "Setup needs checking".to_owned(),
        InstallationState::OutOfDate { .. } => "An update is available".to_owned(),
        InstallationState::Partial { detail } | InstallationState::NeedsRepair { detail, .. } => {
            detail.clone()
        }
    }
}

fn friendly_kind(kind: &str) -> &str {
    match kind {
        "packagepack" => "Playable composition",
        "enginepack" => "Engine composition",
        "gamepack" => "Game composition",
        "modpack" => "Add-on collection",
        "engine" => "Engine",
        "game" => "Game",
        "game-mod" => "Game add-on",
        "engine-mod" => "Engine add-on",
        "extension-mod" => "Extension",
        "crate" => "Developer library",
        _ => "Content",
    }
}

fn workshop_item_kind(tags: &[String]) -> Option<ContentKind> {
    tags.iter().find_map(|tag| tag.parse::<ContentKind>().ok())
}

impl eframe::App for LauncherApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        if !self.connect_attempted {
            self.begin_connect();
        }
        self.poll_connection();
        self.poll_service();
        ui.vertical(|ui| {
            ui.horizontal(|ui| {
                ui.heading("Vapor Launcher");
                ui.separator();
                for (page, label) in [
                    (Page::Play, "Play"),
                    (Page::Library, "Library"),
                    (Page::Workshop, "Workshop"),
                ] {
                    if ui.selectable_label(self.page == page, label).clicked() {
                        self.page = page;
                    }
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("Refresh").clicked() {
                        self.send(ClientAction::Catalog(CatalogAction::Refresh));
                        self.send(ClientAction::Workshop(WorkshopAction::LoadSubscriptions));
                    }
                });
            });
            ui.separator();
            egui::ScrollArea::vertical().show(ui, |ui| match self.page {
                Page::Play => self.play_page(ui),
                Page::Library => self.library_page(ui),
                Page::Workshop => self.workshop_page(ui),
            });
            ui.separator();
            ui.horizontal_wrapped(|ui| {
                ui.label(&self.status);
                if let Some(error) = &self.service_error {
                    ui.colored_label(egui::Color32::LIGHT_RED, error);
                    if ui.button("Reconnect").clicked() {
                        self.service = None;
                        self.connect_attempted = false;
                        self.next_connect_attempt = Instant::now();
                        self.begin_connect();
                    }
                }
            });
        });
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_millis(100));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn player_primary_action_tracks_service_installation_state() {
        let mut composition = Composition::default_composition();
        assert_eq!(
            composition_intent(&composition),
            CompositionIntent::FindGame
        );
        composition.root_package = Some(vapor_client_protocol::ContentReference {
            id: "studio/test".into(),
            version: None,
        });
        assert_eq!(composition_intent(&composition), CompositionIntent::Verify);
        composition.installation = InstallationState::NotInstalled;
        assert_eq!(composition_intent(&composition), CompositionIntent::Install);
        assert_eq!(
            composition_intent(&Composition {
                installation: InstallationState::Installed {
                    artifacts: vec![],
                    graph_fingerprint: None
                },
                ..composition.clone()
            }),
            CompositionIntent::Launch
        );
        assert_eq!(
            composition_intent(&Composition {
                installation: InstallationState::Partial {
                    detail: "partial".into()
                },
                ..composition
            }),
            CompositionIntent::Repair
        );
    }

    #[test]
    fn player_workshop_filters_use_canonical_types_and_hide_rust_crates() {
        let player_kinds: Vec<_> = ContentKind::ALL
            .into_iter()
            .filter(|kind| *kind != ContentKind::Crate)
            .collect();

        assert_eq!(player_kinds.len(), ContentKind::ALL.len() - 1);
        assert!(player_kinds.contains(&ContentKind::Packagepack));
        assert!(player_kinds.contains(&ContentKind::Game));
        assert!(player_kinds.contains(&ContentKind::EngineMod));
        assert!(!player_kinds.contains(&ContentKind::Crate));
        assert_eq!(ContentKind::GameMod.as_str(), "game-mod");
    }

    #[test]
    fn workshop_results_expose_the_typed_content_kind_without_raw_tags() {
        let tags = vec!["game-mod".to_owned(), "survival".to_owned()];
        assert_eq!(workshop_item_kind(&tags), Some(ContentKind::GameMod));
        assert_eq!(friendly_kind("game-mod"), "Game add-on");
        assert_eq!(workshop_item_kind(&["survival".to_owned()]), None);
    }
}
