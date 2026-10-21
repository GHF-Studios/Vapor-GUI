use eframe::egui;
use std::{
    sync::mpsc::{self, Receiver, TryRecvError},
    thread,
    time::{Duration, Instant},
};
use vapor_client::{Client, ClientHostConfig, service_sibling_path};
use vapor_client_protocol::{
    CatalogAction, CatalogEntry, CatalogItemAction, CatalogItemRole, CatalogLocationKind,
    ClientAction, ClientEvent, Composition, CompositionAction, ContentKind, InstallationState,
    OperationStatus, ProtocolFrame, WorkshopAction, WorkshopSnapshot,
};

const WORKSHOP_RESULTS_PER_PAGE: u32 = 50;

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

fn create_composition_action(entry: &CatalogEntry) -> Option<ClientAction> {
    if entry.role != CatalogItemRole::PlayableComposition
        || !entry
            .actions
            .contains(&CatalogItemAction::CreateComposition)
    {
        return None;
    }
    Some(ClientAction::Composition(
        CompositionAction::CreateFromContent {
            name: entry
                .name
                .clone()
                .unwrap_or_else(|| "New composition".to_owned()),
            content_id: entry.id.clone(),
            content_version: entry.version.clone()?,
        },
    ))
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

    fn search_workshop(&mut self, page: u32) {
        self.send(workshop_search_action(
            &self.workshop_query,
            page,
            self.workshop_kind,
        ));
    }

    fn play_page(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.heading("Play");
            ui.weak("Choose a game to continue.");
        });
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
            ui.label("No games are installed yet.");
            if ui.button("Find a game in Workshop").clicked() {
                self.page = Page::Workshop;
                self.workshop_kind = Some(ContentKind::Game);
                self.workshop_query.clear();
                self.search_workshop(1);
            }
            return;
        }
        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut self.query)
                    .hint_text("Find a game")
                    .desired_width(ui.available_width()),
            );
        });
        let query = self.query.trim().to_lowercase();
        let mut filtered: Vec<_> = compositions
            .iter()
            .filter(|item| query.is_empty() || item.name.to_lowercase().contains(&query))
            .collect();
        filtered.sort_by_key(|composition| {
            snapshot.active_composition_id.as_deref() != Some(composition.id.as_str())
        });
        ui.add_space(4.0);
        egui::ScrollArea::vertical().show(ui, |ui| {
            for composition in filtered {
                let active =
                    snapshot.active_composition_id.as_deref() == Some(composition.id.as_str());
                let (intent, action) = composition_action(composition);
                egui::Frame::group(ui.style())
                    .inner_margin(egui::Margin::symmetric(9, 6))
                    .show(ui, |ui| {
                        ui.horizontal_wrapped(|ui| {
                            ui.vertical(|ui| {
                                ui.strong(&composition.name);
                                ui.weak(composition_summary(composition));
                                if active {
                                    ui.label(egui::RichText::new("Selected").small().weak());
                                }
                            });
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if ui.button(intent_label(intent)).clicked() {
                                        if let Some(action) = action.clone() {
                                            self.send(action);
                                        } else {
                                            self.page = Page::Workshop;
                                            self.workshop_query.clear();
                                            self.workshop_kind = None;
                                            self.search_workshop(1);
                                        }
                                    }
                                    if !active && ui.small_button("Select").clicked() {
                                        self.send(ClientAction::Composition(
                                            CompositionAction::Select {
                                                composition_id: composition.id.clone(),
                                            },
                                        ));
                                    }
                                },
                            );
                        });
                    });
            }
        });
    }

    fn library_page(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.heading("Library");
            ui.weak("Games and add-ons on this device.");
        });
        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut self.query)
                    .hint_text("Find content")
                    .desired_width(ui.available_width()),
            );
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
                let is_downloaded = entry
                    .locations
                    .iter()
                    .any(|location| location.kind == CatalogLocationKind::Cache);
                let add_to_play = create_composition_action(&entry);
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
                            if let Some(action) = add_to_play.clone() {
                                if ui.button("Add to Play").clicked() {
                                    self.send(action);
                                    self.page = Page::Play;
                                }
                            } else if is_installed {
                                ui.label("Available");
                            } else if is_downloaded {
                                ui.label("Downloaded");
                            } else if entry.published_file_id.is_some() {
                                if ui.button("Find in Workshop").clicked() {
                                    self.workshop_query =
                                        entry.name.as_deref().unwrap_or(&entry.id).to_owned();
                                    self.page = Page::Workshop;
                                    self.workshop_kind = None;
                                    self.search_workshop(1);
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
        ui.horizontal(|ui| {
            ui.heading("Workshop");
            ui.weak("Find games and add-ons.");
        });
        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut self.workshop_query)
                    .hint_text("Search Workshop")
                    .desired_width(ui.available_width().min(260.0)),
            );
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
            let busy = self
                .service
                .as_ref()
                .and_then(|service| service.latest_snapshot().active_operation.as_ref())
                .is_some();
            if ui
                .add_enabled(!busy, egui::Button::new("Search Workshop"))
                .clicked()
            {
                self.search_workshop(1);
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
    let (busy, search, catalog) = app
        .service
        .as_ref()
        .map(|service| {
            let snapshot = service.latest_snapshot();
            (
                snapshot.active_operation.is_some(),
                snapshot.workshop.search.clone(),
                snapshot.catalog.clone(),
            )
        })
        .unwrap_or_else(|| (false, None, Vec::new()));
    ui.separator();
    ui.heading("Subscriptions");
    if !snapshot.subscriptions_loaded {
        ui.label("Loading your subscriptions…");
    } else if snapshot.subscriptions.is_empty() {
        ui.label("You have no subscriptions yet.");
    }
    for item in &snapshot.subscriptions {
        let playable = catalog.iter().find(|entry| {
            entry.published_file_id.as_deref() == Some(item.published_file_id.as_str())
        });
        let add_to_play = playable.and_then(create_composition_action);
        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                ui.label(&item.title);
                if item.downloading {
                    match item.download_progress {
                        Some((downloaded, total)) if total > 0 => {
                            ui.add(
                                egui::ProgressBar::new(downloaded as f32 / total as f32)
                                    .desired_width(140.0)
                                    .show_percentage(),
                            );
                        }
                        _ => {
                            ui.weak("Downloading…");
                        }
                    }
                }
            });
            ui.label(if item.downloading {
                "Downloading…"
            } else if item.needs_update {
                "Update available"
            } else if item.installed {
                "Ready"
            } else {
                "Subscribed"
            });
            if !busy && !item.installed && !item.downloading && ui.button("Download").clicked() {
                app.send(ClientAction::Workshop(WorkshopAction::Acquire {
                    item_ids: vec![item.published_file_id.clone()],
                }));
            }
            if !busy
                && item.installed
                && let Some(action) = add_to_play.clone()
                && ui.button("Add to Play").clicked()
            {
                app.send(action);
                app.page = Page::Play;
            }
            if !busy && ui.button("Unsubscribe").clicked() {
                app.send(ClientAction::Workshop(WorkshopAction::Unsubscribe {
                    item_ids: vec![item.published_file_id.clone()],
                }));
            }
        });
    }
    ui.separator();
    ui.heading("Search results");
    if let Some(results) = search {
        let controls_match_results =
            workshop_search_matches_controls(&results, &app.workshop_query, app.workshop_kind);
        let page_count = workshop_page_count(results.total_results);
        ui.horizontal(|ui| {
            ui.weak(format!(
                "{} · page {} of {} · {} items",
                if results.query.is_empty() {
                    "All Workshop items"
                } else {
                    "Search results"
                },
                results.page,
                page_count,
                results.total_results
            ));
            if !controls_match_results {
                ui.colored_label(egui::Color32::YELLOW, "Filters changed · Search to apply");
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .add_enabled(
                        !busy && controls_match_results && results.page < page_count,
                        egui::Button::new("Next →"),
                    )
                    .clicked()
                {
                    app.send(ClientAction::Workshop(WorkshopAction::Search {
                        query: results.query.clone(),
                        page: results.page + 1,
                        required_tags: results.required_tags.clone(),
                    }));
                }
                if ui
                    .add_enabled(
                        !busy && controls_match_results && results.page > 1,
                        egui::Button::new("← Previous"),
                    )
                    .clicked()
                {
                    app.send(ClientAction::Workshop(WorkshopAction::Search {
                        query: results.query.clone(),
                        page: results.page - 1,
                        required_tags: results.required_tags.clone(),
                    }));
                }
            });
        });
        if results.items.is_empty() {
            ui.label("No matching items.");
        }
        for item in results.items {
            let subscribed = subscription_ids.contains(&item.published_file_id);
            let playable = catalog.iter().find(|entry| {
                entry.published_file_id.as_deref() == Some(item.published_file_id.as_str())
            });
            let add_to_play = playable.and_then(create_composition_action);
            ui.horizontal(|ui| {
                ui.vertical(|ui| {
                    ui.label(egui::RichText::new(&item.title).strong());
                    ui.label(
                        workshop_item_kind(&item.tags)
                            .map(|kind| friendly_kind(kind.as_str()))
                            .unwrap_or("Community content"),
                    );
                });
                if !busy && !subscribed && ui.button("Subscribe").clicked() {
                    app.send(ClientAction::Workshop(WorkshopAction::Subscribe {
                        item_ids: vec![item.published_file_id],
                    }));
                }
                if !busy
                    && let Some(action) = add_to_play
                    && ui.button("Add to Play").clicked()
                {
                    app.send(action);
                    app.page = Page::Play;
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

fn workshop_search_action(query: &str, page: u32, kind: Option<ContentKind>) -> ClientAction {
    ClientAction::Workshop(WorkshopAction::Search {
        query: query.trim().to_owned(),
        page: page.max(1),
        required_tags: kind
            .map(|kind| vec![kind.as_str().to_owned()])
            .unwrap_or_default(),
    })
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

fn workshop_search_matches_controls(
    search: &vapor_client_protocol::WorkshopSearchSnapshot,
    query: &str,
    kind: Option<ContentKind>,
) -> bool {
    let selected_tags = kind
        .map(|kind| vec![kind.as_str().to_owned()])
        .unwrap_or_default();
    search.query == query.trim() && search.required_tags == selected_tags
}

fn workshop_page_count(total_results: u32) -> u32 {
    total_results.div_ceil(WORKSHOP_RESULTS_PER_PAGE).max(1)
}

impl eframe::App for LauncherApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        if !self.connect_attempted {
            self.begin_connect();
        }
        self.poll_connection();
        self.poll_service();
        ui.spacing_mut().item_spacing = egui::vec2(8.0, 6.0);
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
    fn launcher_only_offers_add_to_play_for_service_eligible_packagepacks() {
        let mut entry = CatalogEntry {
            id: "examples/wheel".into(),
            name: Some("Wheel Salvage Demo".into()),
            version: Some("0.1.1".into()),
            kind: "packagepack".into(),
            published_file_id: Some("3823734148".into()),
            dependencies: vec![],
            conflicts: vec![],
            locations: vec![],
            role: CatalogItemRole::PlayableComposition,
            actions: vec![CatalogItemAction::CreateComposition],
        };
        assert_eq!(
            create_composition_action(&entry),
            Some(ClientAction::Composition(
                CompositionAction::CreateFromContent {
                    name: "Wheel Salvage Demo".into(),
                    content_id: "examples/wheel".into(),
                    content_version: "0.1.1".into(),
                }
            ))
        );

        entry.actions.clear();
        assert_eq!(create_composition_action(&entry), None);
        entry.actions.push(CatalogItemAction::CreateComposition);
        entry.version = None;
        assert_eq!(create_composition_action(&entry), None);
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

    #[test]
    fn launcher_workshop_search_uses_service_page_numbering_and_selected_type() {
        assert_eq!(
            workshop_search_action("  moon  ", 0, Some(ContentKind::Game)),
            ClientAction::Workshop(WorkshopAction::Search {
                query: "moon".into(),
                page: 1,
                required_tags: vec!["game".into()],
            })
        );
        assert_eq!(
            workshop_search_action("", 2, None),
            ClientAction::Workshop(WorkshopAction::Search {
                query: String::new(),
                page: 2,
                required_tags: vec![],
            })
        );
    }

    #[test]
    fn launcher_workshop_page_count_accounts_for_partial_final_page() {
        assert_eq!(workshop_page_count(0), 1);
        assert_eq!(workshop_page_count(50), 1);
        assert_eq!(workshop_page_count(51), 2);
        assert_eq!(workshop_page_count(100), 2);
        assert_eq!(workshop_page_count(101), 3);
    }

    #[test]
    fn workshop_paging_does_not_restore_unsubmitted_filters() {
        let search = vapor_client_protocol::WorkshopSearchSnapshot {
            revision: 1,
            query: "moon".into(),
            page: 1,
            total_results: 73,
            required_tags: vec!["game".into()],
            items: vec![],
        };
        assert!(workshop_search_matches_controls(
            &search,
            " moon ",
            Some(ContentKind::Game)
        ));
        assert!(!workshop_search_matches_controls(
            &search,
            "planet",
            Some(ContentKind::Game)
        ));
        assert!(!workshop_search_matches_controls(&search, "moon", None));
    }
}
