use eframe::egui;
use std::{
    collections::HashSet,
    sync::mpsc::{self, Receiver, TryRecvError},
    thread,
    time::{Duration, Instant},
};
use vapor_client::{Client, ClientHostConfig, service_sibling_path};
use vapor_client_protocol::{
    CatalogAction, CatalogEntry, CatalogItemAction, CatalogItemRole, CatalogLocationKind,
    ClientAction, ClientEvent, Composition, CompositionAction, ContentKind, InstallationState,
    LibraryAction, OperationStatus, ProtocolFrame, WorkshopAction, WorkshopItemSnapshot,
    WorkshopSnapshot,
};

const WORKSHOP_RESULTS_PER_PAGE: u32 = 50;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum Page {
    #[default]
    Play,
    Library,
    Workshop,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum WorkshopView {
    #[default]
    Discover,
    Subscriptions,
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

#[derive(Debug, Clone)]
struct WorkshopItemRow {
    published_file_id: String,
    title: String,
    description: String,
    tags: Vec<String>,
    subscribed: bool,
    downloading: bool,
    download_progress: Option<(u64, u64)>,
    needs_update: bool,
    local: Option<CatalogEntry>,
}

fn workshop_item_rows(
    snapshot: &WorkshopSnapshot,
    results: Option<&vapor_client_protocol::WorkshopSearchSnapshot>,
    catalog: &[CatalogEntry],
) -> Vec<WorkshopItemRow> {
    let local_for = |id: &str| {
        catalog
            .iter()
            .find(|entry| entry.published_file_id.as_deref() == Some(id))
            .cloned()
    };
    let mut rows: Vec<_> = if results.is_none() {
        snapshot
            .subscriptions
            .iter()
            .map(|item| WorkshopItemRow {
                published_file_id: item.published_file_id.clone(),
                title: item.title.clone(),
                description: item.description.clone(),
                tags: Vec::new(),
                subscribed: true,
                downloading: item.downloading,
                download_progress: item.download_progress,
                needs_update: item.needs_update,
                local: local_for(&item.published_file_id),
            })
            .collect()
    } else {
        Vec::new()
    };

    if let Some(results) = results {
        for item in &results.items {
            if let Some(row) = rows
                .iter_mut()
                .find(|row| row.published_file_id == item.published_file_id)
            {
                row.title = item.title.clone();
                row.description = item.description.clone();
                row.tags = item.tags.clone();
                row.local = row
                    .local
                    .clone()
                    .or_else(|| local_for(&item.published_file_id));
            } else {
                let subscription = snapshot
                    .subscriptions
                    .iter()
                    .find(|subscription| subscription.published_file_id == item.published_file_id);
                let mut row = workshop_item_row(
                    item,
                    subscription.is_some(),
                    local_for(&item.published_file_id),
                );
                if let Some(subscription) = subscription {
                    row.downloading = subscription.downloading;
                    row.download_progress = subscription.download_progress;
                    row.needs_update = subscription.needs_update;
                }
                rows.push(row);
            }
        }
    }
    rows
}

fn visible_workshop_rows<'a>(
    rows: &'a [WorkshopItemRow],
    results: &vapor_client_protocol::WorkshopSearchSnapshot,
    offset: usize,
    page_size: usize,
) -> Vec<&'a WorkshopItemRow> {
    let visible_ids: HashSet<_> = results
        .items
        .iter()
        .skip(offset)
        .take(page_size)
        .map(|item| item.published_file_id.as_str())
        .collect();
    rows.iter()
        .filter(|row| visible_ids.contains(row.published_file_id.as_str()))
        .collect()
}

fn workshop_item_row(
    item: &WorkshopItemSnapshot,
    subscribed: bool,
    local: Option<CatalogEntry>,
) -> WorkshopItemRow {
    WorkshopItemRow {
        published_file_id: item.published_file_id.clone(),
        title: item.title.clone(),
        description: item.description.clone(),
        tags: item.tags.clone(),
        subscribed,
        downloading: false,
        download_progress: None,
        needs_update: false,
        local,
    }
}

fn composition_uses_content<'a>(
    compositions: impl IntoIterator<Item = &'a Composition>,
    content_id: &str,
) -> bool {
    compositions.into_iter().any(|composition| {
        composition
            .root_package
            .as_ref()
            .is_some_and(|root| root.id == content_id)
            || composition
                .resolved_graph
                .as_ref()
                .is_some_and(|graph| graph.nodes.iter().any(|node| node.content.id == content_id))
    })
}

fn catalog_entry_has_cached_copy(entry: &CatalogEntry) -> bool {
    entry
        .locations
        .iter()
        .any(|location| location.kind == CatalogLocationKind::Cache)
}

fn can_remove_cached_copy(entry: &CatalogEntry, compositions: &[Composition]) -> bool {
    catalog_entry_has_cached_copy(entry) && !composition_uses_content(compositions, &entry.id)
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
    workshop_view: WorkshopView,
    subscriptions_query: String,
    subscriptions_offset: usize,
    selected_workshop_items: HashSet<String>,
    workshop_page_size: usize,
    workshop_result_offset: usize,
    status: String,
}

impl LauncherApp {
    pub fn new(creation: &eframe::CreationContext<'_>, _launch: vapor_gui_core::GuiLaunch) -> Self {
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
            workshop_view: WorkshopView::Discover,
            subscriptions_query: String::new(),
            subscriptions_offset: 0,
            selected_workshop_items: HashSet::new(),
            workshop_page_size: creation
                .storage
                .and_then(|storage| storage.get_string("workshop_page_size"))
                .and_then(|value| value.parse::<usize>().ok())
                .filter(|size| [10, 20, 50].contains(size))
                .unwrap_or(20),
            workshop_result_offset: 0,
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
        if page == 1 {
            self.workshop_result_offset = 0;
            self.selected_workshop_items.clear();
        }
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
                vapor_gui_core::panel::Panel::new(&composition.name, |ui| {
                    ui.horizontal_wrapped(|ui| {
                        ui.vertical(|ui| {
                            if active {
                                ui.label(egui::RichText::new("Selected").small().weak());
                            }
                        });
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
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
                                self.send(ClientAction::Composition(CompositionAction::Select {
                                    composition_id: composition.id.clone(),
                                }));
                            }
                        });
                    });
                })
                .description(composition_summary(composition))
                .show(ui);
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
            vapor_gui_core::panel::Panel::new("Your Library", |ui| {
                ui.label("No player content found. Discover more in Workshop.");
            })
            .description("Content available on this device")
            .show(ui);
        }
        egui::ScrollArea::vertical().show(ui, |ui| {
            for entry in entries {
                let is_installed = entry.locations.iter().any(|location| {
                    location.enabled && matches!(location.kind, CatalogLocationKind::Installed)
                });
                let is_downloaded = catalog_entry_has_cached_copy(&entry);
                let is_unambiguous = snapshot
                    .catalog
                    .iter()
                    .filter(|candidate| candidate.id == entry.id)
                    .count()
                    == 1;
                let can_auto_install = vapor_client_protocol::can_auto_install_library_item(
                    &entry.kind,
                    entry.published_file_id.is_some(),
                    is_downloaded,
                    is_unambiguous,
                );
                let auto_install = snapshot
                    .durable_state
                    .library_policies
                    .iter()
                    .any(|policy| policy.content_id.as_str() == entry.id && policy.auto_install);
                let add_to_play = create_composition_action(&entry);
                let subscribed = snapshot.workshop.subscriptions.iter().any(|item| {
                    entry.published_file_id.as_deref() == Some(item.published_file_id.as_str())
                });
                let subscription_id = subscribed
                    .then(|| entry.published_file_id.clone())
                    .flatten();
                let used_by: Vec<_> = snapshot
                    .durable_state
                    .compositions
                    .compositions
                    .iter()
                    .filter(|composition| {
                        composition_uses_content(std::iter::once(*composition), &entry.id)
                    })
                    .map(|composition| composition.name.as_str())
                    .collect();
                vapor_gui_core::panel::Panel::new(
                    entry.name.as_deref().unwrap_or(&entry.id),
                    |ui| {
                    ui.horizontal(|ui| {
                        ui.vertical(|ui| {
                            ui.label(format!(
                                "{} · {}",
                                friendly_kind(&entry.kind),
                                entry.version.as_deref().unwrap_or("version unknown")
                            ));
                            ui.horizontal_wrapped(|ui| {
                                if subscribed {
                                    ui.label("Subscribed");
                                }
                                if is_installed {
                                    ui.label(egui::RichText::new("Installed here").strong());
                                } else if is_downloaded {
                                    ui.label(egui::RichText::new("Available here").strong());
                                }
                                if !used_by.is_empty() {
                                    ui.weak(format!("Used by {}", used_by.join(", ")));
                                }
                                if snapshot.workshop.subscriptions.iter().any(|item| {
                                    entry.published_file_id.as_deref() == Some(item.published_file_id.as_str())
                                        && item.needs_update
                                }) {
                                    ui.colored_label(egui::Color32::YELLOW, "Update available");
                                }
                            });
                        });
                        ui.horizontal_wrapped(|ui| {
                            if let Some(action) = add_to_play.clone()
                                && ui.button("Add to Play").clicked()
                            {
                                self.send(action);
                                self.page = Page::Play;
                            } else if !is_installed && !is_downloaded && !subscribed {
                                if entry.published_file_id.is_some()
                                    && ui.button("Find in Workshop").clicked()
                                {
                                    self.workshop_query =
                                        entry.name.as_deref().unwrap_or(&entry.id).to_owned();
                                    self.page = Page::Workshop;
                                    self.workshop_view = WorkshopView::Discover;
                                    self.workshop_kind = None;
                                    self.search_workshop(1);
                                } else if entry.published_file_id.is_none() {
                                    ui.label("Not installed");
                                }
                            }
                            let can_remove = can_remove_cached_copy(
                                &entry,
                                &snapshot.durable_state.compositions.compositions,
                            );
                            if (subscribed && subscription_id.is_some())
                                || can_remove
                                || can_auto_install
                            {
                                ui.menu_button("⋯", |ui| {
                                    if can_auto_install {
                                        let mut enabled = auto_install;
                                        if ui
                                            .checkbox(&mut enabled, "Auto-Install")
                                            .on_hover_text(
                                                "Follow compatible Workshop updates and remove the local copy when no composition uses it.",
                                            )
                                            .changed()
                                        {
                                            self.send(ClientAction::Library(
                                                LibraryAction::SetAutoInstall {
                                                    content_id: entry.id.clone(),
                                                    enabled,
                                                },
                                            ));
                                        }
                                    }
                                    if subscribed
                                        && let Some(item_id) = subscription_id.clone()
                                        && ui.button("Unsubscribe").clicked()
                                    {
                                        self.send(ClientAction::Workshop(
                                            WorkshopAction::Unsubscribe {
                                                item_ids: vec![item_id],
                                            },
                                        ));
                                        ui.close();
                                    }
                                    if can_remove && ui.button("Remove from this device").clicked() {
                                        self.send(ClientAction::Library(LibraryAction::Uninstall {
                                            selector: entry.id.clone(),
                                        }));
                                        ui.close();
                                    }
                                })
                                .response
                                .on_hover_text("More actions");
                            }
                        });
                    });
                    },
                )
                .show(ui);
                ui.add_space(6.0);
            }
        });
    }

    fn workshop_page(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            if ui
                .selectable_label(self.workshop_view == WorkshopView::Discover, "Discover")
                .clicked()
            {
                self.workshop_view = WorkshopView::Discover;
                self.selected_workshop_items.clear();
            }
            if ui
                .selectable_label(
                    self.workshop_view == WorkshopView::Subscriptions,
                    "Subscriptions",
                )
                .clicked()
            {
                self.workshop_view = WorkshopView::Subscriptions;
                self.subscriptions_offset = 0;
                self.selected_workshop_items.clear();
            }
            ui.weak("One item identity, viewed by discovery or account subscription.");
        });
        if self.workshop_view == WorkshopView::Discover {
            vapor_gui_core::panel::Panel::new("Discover", |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut self.workshop_query)
                            .hint_text("Search Workshop")
                            .desired_width(ui.available_width().min(320.0)),
                    );
                    let mut kind_changed = false;
                    egui::ComboBox::from_label("Content type")
                        .selected_text(
                            self.workshop_kind
                                .map(|kind| friendly_kind(kind.as_str()))
                                .unwrap_or("All types"),
                        )
                        .show_ui(ui, |ui| {
                            kind_changed |= ui
                                .selectable_value(&mut self.workshop_kind, None, "All types")
                                .changed();
                            for kind in ContentKind::ALL
                                .into_iter()
                                .filter(|kind| *kind != ContentKind::Crate)
                            {
                                kind_changed |= ui
                                    .selectable_value(
                                        &mut self.workshop_kind,
                                        Some(kind),
                                        friendly_kind(kind.as_str()),
                                    )
                                    .changed();
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
                        || (kind_changed && !busy)
                    {
                        self.search_workshop(1);
                    }
                });
            })
            .description("Find games and add-ons. Type and tag filters apply to the next search.")
            .show(ui);
        }
        let Some(snapshot) = self
            .service
            .as_ref()
            .map(|service| service.latest_snapshot().workshop.clone())
        else {
            ui.label("Workshop will be available when Vapor is ready.");
            return;
        };
        match self.workshop_view {
            WorkshopView::Discover => workshop_page_contents(self, ui, snapshot),
            WorkshopView::Subscriptions => workshop_subscriptions_contents(self, ui, snapshot),
        }
    }
}

fn workshop_page_contents(app: &mut LauncherApp, ui: &mut egui::Ui, snapshot: WorkshopSnapshot) {
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
    let matching_search = search.as_ref().filter(|results| {
        workshop_search_matches_controls(results, &app.workshop_query, app.workshop_kind)
    });
    let rows = matching_search
        .map(|results| workshop_item_rows(&snapshot, Some(results), &catalog))
        .unwrap_or_default();
    let page_count = matching_search
        .as_ref()
        .map(|results| workshop_page_count(results.total_results))
        .unwrap_or(1);

    if let Some(results) = &matching_search {
        let choices = [10, 20, 50];
        if !choices.contains(&app.workshop_page_size) {
            app.workshop_page_size = 20;
        }
        let local_page_count = results.items.len().div_ceil(app.workshop_page_size).max(1);
        ui.horizontal(|ui| {
            ui.weak(format!(
                "Steam page {} of {} · results page {} of {} · {} total",
                results.page,
                page_count,
                app.workshop_result_offset / app.workshop_page_size + 1,
                local_page_count,
                results.total_results
            ));
            egui::ComboBox::from_label("Rows shown")
                .selected_text(app.workshop_page_size.to_string())
                .show_ui(ui, |ui| {
                    for size in choices {
                        if ui
                            .selectable_value(&mut app.workshop_page_size, size, size.to_string())
                            .changed()
                        {
                            app.workshop_result_offset = 0;
                            app.selected_workshop_items.clear();
                        }
                    }
                });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .add_enabled(
                        !busy && results.page < page_count,
                        egui::Button::new("Next →"),
                    )
                    .clicked()
                {
                    app.workshop_result_offset = 0;
                    app.selected_workshop_items.clear();
                    app.send(ClientAction::Workshop(WorkshopAction::Search {
                        query: results.query.clone(),
                        page: results.page + 1,
                        content_kind: results.content_kind,
                        required_tags: results.required_tags.clone(),
                    }));
                }
                if ui
                    .add_enabled(!busy && results.page > 1, egui::Button::new("← Previous"))
                    .clicked()
                {
                    app.workshop_result_offset = 0;
                    app.selected_workshop_items.clear();
                    app.send(ClientAction::Workshop(WorkshopAction::Search {
                        query: results.query.clone(),
                        page: results.page - 1,
                        content_kind: results.content_kind,
                        required_tags: results.required_tags.clone(),
                    }));
                }
            });
        });
        if local_page_count > 1 {
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(
                        app.workshop_result_offset > 0,
                        egui::Button::new("← Previous results"),
                    )
                    .clicked()
                {
                    app.workshop_result_offset = app
                        .workshop_result_offset
                        .saturating_sub(app.workshop_page_size);
                    app.selected_workshop_items.clear();
                }
                if ui
                    .add_enabled(
                        app.workshop_result_offset + app.workshop_page_size < results.items.len(),
                        egui::Button::new("More results →"),
                    )
                    .clicked()
                {
                    app.workshop_result_offset += app.workshop_page_size;
                    app.selected_workshop_items.clear();
                }
                ui.weak(format!(
                    "{} items returned in this Steam page",
                    results.items.len()
                ));
            });
        }
    } else if search.is_some() {
        ui.colored_label(
            egui::Color32::YELLOW,
            "Search settings changed. Run Search to refresh these results.",
        );
    }

    let mut visible_rows = matching_search
        .map(|results| {
            visible_workshop_rows(
                &rows,
                results,
                app.workshop_result_offset,
                app.workshop_page_size,
            )
        })
        .unwrap_or_default();
    visible_rows.sort_by_key(|row| row.title.to_lowercase());
    let mut selectable: Vec<_> = visible_rows
        .iter()
        .map(|row| row.published_file_id.clone())
        .collect();
    selectable.sort();
    selectable.dedup();
    ui.horizontal(|ui| {
        if ui.small_button("Select visible").clicked() {
            app.selected_workshop_items
                .extend(selectable.iter().cloned());
        }
        if ui.small_button("Clear selection").clicked() {
            app.selected_workshop_items.clear();
        }
        let selected: Vec<_> = app.selected_workshop_items.iter().cloned().collect();
        let subscribed: Vec<_> = visible_rows
            .iter()
            .filter(|row| selected.contains(&row.published_file_id) && row.subscribed)
            .map(|row| row.published_file_id.clone())
            .collect();
        let unsubscribed: Vec<_> = visible_rows
            .iter()
            .filter(|row| selected.contains(&row.published_file_id) && !row.subscribed)
            .map(|row| row.published_file_id.clone())
            .collect();
        if ui
            .add_enabled(
                !busy && !unsubscribed.is_empty(),
                egui::Button::new("Subscribe selected"),
            )
            .clicked()
        {
            app.send(ClientAction::Workshop(WorkshopAction::Subscribe {
                item_ids: unsubscribed,
            }));
            app.selected_workshop_items.clear();
        }
        if ui
            .add_enabled(
                !busy && !subscribed.is_empty(),
                egui::Button::new("Unsubscribe selected"),
            )
            .clicked()
        {
            app.send(ClientAction::Workshop(WorkshopAction::Unsubscribe {
                item_ids: subscribed,
            }));
            app.selected_workshop_items.clear();
        }
        ui.weak(format!("{} selected", app.selected_workshop_items.len()));
    });

    let has_subscriptions = rows.iter().any(|row| row.subscribed);
    if !snapshot.subscriptions_loaded {
        ui.label("Loading your subscriptions…");
    } else if !has_subscriptions && matching_search.is_none() {
        ui.label("Your Workshop items and new discoveries will appear here.");
    }
    for row in visible_rows {
        vapor_gui_core::panel::Panel::new(&row.title, |ui| {
            ui.horizontal(|ui| {
                let mut selected = app.selected_workshop_items.contains(&row.published_file_id);
                if ui.checkbox(&mut selected, "").changed() {
                    if selected {
                        app.selected_workshop_items
                            .insert(row.published_file_id.clone());
                    } else {
                        app.selected_workshop_items.remove(&row.published_file_id);
                    }
                }
                ui.vertical(|ui| {
                    ui.weak(
                        workshop_item_kind(&row.tags)
                            .map(|kind| friendly_kind(kind.as_str()))
                            .unwrap_or("Community content"),
                    );
                    if !row.description.is_empty() || !row.tags.is_empty() {
                        egui::CollapsingHeader::new("Details")
                            .id_salt(("workshop-details", &row.published_file_id))
                            .show(ui, |ui| {
                                if !row.description.is_empty() {
                                    ui.label(egui::RichText::new(&row.description).small());
                                }
                                if !row.tags.is_empty() {
                                    ui.horizontal_wrapped(|ui| {
                                        for tag in &row.tags {
                                            ui.label(egui::RichText::new(tag).small().weak());
                                        }
                                    });
                                }
                            });
                    }
                    ui.horizontal_wrapped(|ui| {
                        if row.subscribed {
                            ui.label(egui::RichText::new("Subscribed").small());
                        }
                        if row.local.is_some() {
                            ui.label(egui::RichText::new("In Library").small().strong());
                        }
                        if row.needs_update {
                            ui.colored_label(egui::Color32::YELLOW, "Update available");
                        }
                        if row.downloading {
                            if let Some((done, total)) =
                                row.download_progress.filter(|(_, total)| *total > 0)
                            {
                                ui.add(
                                    egui::ProgressBar::new(done as f32 / total as f32)
                                        .desired_width(120.0),
                                );
                            } else {
                                ui.weak("Downloading…");
                            }
                        }
                    });
                });
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if let Some(entry) = row.local.as_ref()
                        && let Some(action) = create_composition_action(entry)
                        && ui.button("Add to Play").clicked()
                    {
                        app.send(action);
                        app.page = Page::Play;
                    }
                    if row.subscribed
                        && row.local.is_none()
                        && !row.downloading
                        && !busy
                        && ui.button("Download").clicked()
                    {
                        app.send(ClientAction::Workshop(WorkshopAction::Acquire {
                            item_ids: vec![row.published_file_id.clone()],
                        }));
                    }
                    if !row.subscribed && !busy && ui.button("Subscribe").clicked() {
                        app.send(ClientAction::Workshop(WorkshopAction::Subscribe {
                            item_ids: vec![row.published_file_id.clone()],
                        }));
                    }
                    if row.subscribed && !busy {
                        ui.menu_button("⋯", |ui| {
                            if ui.button("Unsubscribe").clicked() {
                                app.send(ClientAction::Workshop(WorkshopAction::Unsubscribe {
                                    item_ids: vec![row.published_file_id.clone()],
                                }));
                                ui.close();
                            }
                        })
                        .response
                        .on_hover_text("More actions");
                    }
                });
            });
        })
        .show(ui);
    }
}

fn workshop_subscriptions_contents(
    app: &mut LauncherApp,
    ui: &mut egui::Ui,
    snapshot: WorkshopSnapshot,
) {
    let (busy, catalog, compositions) = app
        .service
        .as_ref()
        .map(|service| {
            let snapshot = service.latest_snapshot();
            (
                snapshot.active_operation.is_some(),
                snapshot.catalog.clone(),
                snapshot.durable_state.compositions.compositions.clone(),
            )
        })
        .unwrap_or_else(|| (false, Vec::new(), Vec::new()));
    let mut rows = workshop_item_rows(&snapshot, None, &catalog);
    let query = app.subscriptions_query.trim().to_lowercase();
    rows.retain(|row| {
        query.is_empty()
            || row.title.to_lowercase().contains(&query)
            || row.published_file_id.contains(&query)
            || row
                .tags
                .iter()
                .any(|tag| tag.to_lowercase().contains(&query))
    });
    rows.sort_by_key(|row| row.title.to_lowercase());

    vapor_gui_core::panel::Panel::new("Your Subscriptions", |ui| {
        ui.horizontal_wrapped(|ui| {
            let filter = ui.add(
                egui::TextEdit::singleline(&mut app.subscriptions_query)
                    .hint_text("Filter your subscriptions")
                    .desired_width(ui.available_width().min(300.0)),
            );
            if filter.changed() {
                app.subscriptions_offset = 0;
                app.selected_workshop_items.clear();
            }
            egui::ComboBox::from_label("Rows shown")
                .selected_text(app.workshop_page_size.to_string())
                .show_ui(ui, |ui| {
                    for size in [10, 20, 50] {
                        if ui
                            .selectable_value(&mut app.workshop_page_size, size, size.to_string())
                            .changed()
                        {
                            app.subscriptions_offset = 0;
                            app.selected_workshop_items.clear();
                        }
                    }
                });
            ui.weak(format!("{} matching items", rows.len()));
        });
        ui.weak("Subscriptions belong to your Steam account. Availability in the Library is specific to this device.");
        if !snapshot.subscriptions_loaded {
            ui.label("Loading your Workshop subscriptions…");
            return;
        }
        if rows.is_empty() {
            ui.label(if query.is_empty() {
                "You have no Workshop subscriptions yet. Discover content to get started."
            } else {
                "No subscriptions match this filter."
            });
            return;
        }
        let page_count = rows.len().div_ceil(app.workshop_page_size).max(1);
        app.subscriptions_offset = app
            .subscriptions_offset
            .min((page_count - 1) * app.workshop_page_size);
        ui.horizontal(|ui| {
            ui.weak(format!(
                "Page {} of {}",
                app.subscriptions_offset / app.workshop_page_size + 1,
                page_count
            ));
            if ui
                .add_enabled(
                    app.subscriptions_offset > 0,
                    egui::Button::new("← Previous"),
                )
                .clicked()
            {
                app.subscriptions_offset = app
                    .subscriptions_offset
                    .saturating_sub(app.workshop_page_size);
                app.selected_workshop_items.clear();
            }
            if ui
                .add_enabled(
                    app.subscriptions_offset + app.workshop_page_size < rows.len(),
                    egui::Button::new("Next →"),
                )
                .clicked()
            {
                app.subscriptions_offset += app.workshop_page_size;
                app.selected_workshop_items.clear();
            }
        });
        let visible: Vec<_> = rows
            .iter()
            .skip(app.subscriptions_offset)
            .take(app.workshop_page_size)
            .collect();
        ui.horizontal_wrapped(|ui| {
            if ui.small_button("Select visible").clicked() {
                app.selected_workshop_items.extend(
                    visible
                        .iter()
                        .map(|row| row.published_file_id.clone()),
                );
            }
            if ui.small_button("Clear selection").clicked() {
                app.selected_workshop_items.clear();
            }
            let selected: Vec<_> = visible
                .iter()
                .filter(|row| app.selected_workshop_items.contains(&row.published_file_id))
                .map(|row| row.published_file_id.clone())
                .collect();
            if ui
                .add_enabled(!busy && !selected.is_empty(), egui::Button::new("Unsubscribe selected"))
                .clicked()
            {
                app.send(ClientAction::Workshop(WorkshopAction::Unsubscribe {
                    item_ids: selected,
                }));
                app.selected_workshop_items.clear();
            }
            ui.weak(format!("{} selected", app.selected_workshop_items.len()));
        });
        for row in visible {
            vapor_gui_core::panel::Panel::new(&row.title, |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.weak(workshop_item_kind(&row.tags)
                        .map(|kind| friendly_kind(kind.as_str()))
                        .unwrap_or("Community content"));
                    if row.local.is_some() {
                        ui.label(egui::RichText::new("In Library here").strong());
                    } else if row.downloading {
                        ui.label("Downloading to this device");
                    } else {
                        ui.weak("Not downloaded here");
                    }
                    if row.needs_update {
                        ui.colored_label(egui::Color32::YELLOW, "Update available");
                    }
                    if row.downloading
                        && let Some((done, total)) =
                            row.download_progress.filter(|(_, total)| *total > 0)
                    {
                        ui.add(
                            egui::ProgressBar::new(done as f32 / total as f32)
                                .desired_width(130.0),
                        );
                    }
                });
                ui.horizontal_wrapped(|ui| {
                    let mut selected = app.selected_workshop_items.contains(&row.published_file_id);
                    if ui.checkbox(&mut selected, "Select").changed() {
                        if selected {
                            app.selected_workshop_items.insert(row.published_file_id.clone());
                        } else {
                            app.selected_workshop_items.remove(&row.published_file_id);
                        }
                    }
                    if !row.downloading && row.local.is_none() && !busy
                        && ui.button("Download here").clicked()
                    {
                        app.send(ClientAction::Workshop(WorkshopAction::Acquire {
                            item_ids: vec![row.published_file_id.clone()],
                        }));
                    }
                    if let Some(entry) = row.local.as_ref()
                        && let Some(action) = create_composition_action(entry)
                        && ui.button("Add to Play").clicked()
                    {
                        app.send(action);
                        app.page = Page::Play;
                    }
                    let can_remove = row.local.as_ref().is_some_and(|entry| {
                        can_remove_cached_copy(entry, &compositions)
                    });
                    if !busy {
                        ui.menu_button("⋯", |ui| {
                            if can_remove
                                && let Some(entry) = row.local.as_ref()
                                && ui.button("Remove from this device").clicked()
                            {
                                app.send(ClientAction::Library(LibraryAction::Uninstall {
                                    selector: entry.id.clone(),
                                }));
                                ui.close();
                            }
                            if ui.button("Unsubscribe").clicked() {
                                app.send(ClientAction::Workshop(WorkshopAction::Unsubscribe {
                                    item_ids: vec![row.published_file_id.clone()],
                                }));
                                ui.close();
                            }
                        })
                        .response
                        .on_hover_text("More actions");
                    }
                });
            })
            .show(ui);
        }
    })
    .description("Manage your Steam account subscriptions and this device's local availability")
    .show(ui);
}

fn workshop_search_action(query: &str, page: u32, kind: Option<ContentKind>) -> ClientAction {
    ClientAction::Workshop(WorkshopAction::Search {
        query: query.trim().to_owned(),
        page: page.max(1),
        content_kind: kind,
        required_tags: Vec::new(),
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
    search.query == query.trim() && search.content_kind == kind
}

#[cfg(test)]
mod lifecycle_tests {
    use super::*;

    fn workshop_item(id: &str, title: &str) -> WorkshopItemSnapshot {
        WorkshopItemSnapshot {
            published_file_id: id.to_owned(),
            title: title.to_owned(),
            description: String::new(),
            tags: vec!["game".to_owned()],
            preview_url: None,
            updated_at_unix: 1,
        }
    }

    #[test]
    fn discover_subpage_contains_only_rows_in_its_visible_range() {
        let subscription = |id: &str| vapor_client_protocol::WorkshopSubscriptionSnapshot {
            published_file_id: id.to_owned(),
            title: format!("Item {id}"),
            description: String::new(),
            preview_url: None,
            updated_at_unix: 1,
            installed: true,
            downloading: false,
            download_progress: None,
            needs_update: false,
        };
        let snapshot = WorkshopSnapshot {
            subscriptions: vec![subscription("1"), subscription("2"), subscription("3")],
            ..Default::default()
        };
        let search = vapor_client_protocol::WorkshopSearchSnapshot {
            query: String::new(),
            page: 1,
            total_results: 3,
            revision: 1,
            content_kind: None,
            required_tags: Vec::new(),
            items: vec![
                workshop_item("1", "One"),
                workshop_item("2", "Two"),
                workshop_item("3", "Three"),
            ],
        };
        let rows = workshop_item_rows(&snapshot, Some(&search), &[]);

        let visible = visible_workshop_rows(&rows, &search, 1, 1);

        assert_eq!(
            visible
                .iter()
                .map(|row| row.published_file_id.as_str())
                .collect::<Vec<_>>(),
            ["2"]
        );
    }

    #[test]
    fn workshop_rows_merge_subscription_and_discovery_by_published_identity() {
        let snapshot = WorkshopSnapshot {
            subscriptions: vec![vapor_client_protocol::WorkshopSubscriptionSnapshot {
                published_file_id: "123".to_owned(),
                title: "Subscribed title".to_owned(),
                description: String::new(),
                preview_url: None,
                updated_at_unix: 1,
                installed: true,
                downloading: false,
                download_progress: None,
                needs_update: false,
            }],
            ..Default::default()
        };
        let search = vapor_client_protocol::WorkshopSearchSnapshot {
            query: String::new(),
            page: 1,
            total_results: 1,
            revision: 1,
            content_kind: None,
            required_tags: Vec::new(),
            items: vec![workshop_item("123", "Current title")],
        };

        let rows = workshop_item_rows(&snapshot, Some(&search), &[]);

        assert_eq!(rows.len(), 1);
        assert!(rows[0].subscribed);
        assert_eq!(rows[0].title, "Current title");
        assert_eq!(rows[0].published_file_id, "123");
    }

    #[test]
    fn workshop_search_kind_is_typed_and_user_tags_stay_separate() {
        let action = workshop_search_action("planet", 2, Some(ContentKind::Game));
        assert_eq!(
            action,
            ClientAction::Workshop(WorkshopAction::Search {
                query: "planet".to_owned(),
                page: 2,
                content_kind: Some(ContentKind::Game),
                required_tags: Vec::new(),
            })
        );
    }

    #[test]
    fn local_copy_removal_is_available_only_for_unused_cached_content() {
        let entry = CatalogEntry {
            id: "ghf/game/addon".to_owned(),
            name: Some("Addon".to_owned()),
            version: Some("0.5.0".to_owned()),
            kind: "mod".to_owned(),
            published_file_id: Some("123".to_owned()),
            dependencies: Vec::new(),
            conflicts: Vec::new(),
            locations: vec![vapor_client_protocol::CatalogLocation {
                kind: CatalogLocationKind::Cache,
                root: "/device/cache/addon".to_owned(),
                enabled: true,
                fingerprint: None,
                install_source: None,
            }],
            role: CatalogItemRole::RuntimeContent,
            actions: Vec::new(),
        };

        assert!(can_remove_cached_copy(&entry, &[]));
        let mut composition = Composition::default_composition();
        composition.root_package = Some(vapor_client_protocol::ContentReference {
            id: entry.id.clone(),
            version: entry.version.clone(),
        });
        assert!(!can_remove_cached_copy(&entry, &[composition]));
    }
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

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        storage.set_string("workshop_page_size", self.workshop_page_size.to_string());
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
                content_kind: Some(ContentKind::Game),
                required_tags: vec![],
            })
        );
        assert_eq!(
            workshop_search_action("", 2, None),
            ClientAction::Workshop(WorkshopAction::Search {
                query: String::new(),
                page: 2,
                content_kind: None,
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
            content_kind: Some(ContentKind::Game),
            required_tags: vec![],
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
