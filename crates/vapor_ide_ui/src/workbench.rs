use vapor_client_protocol::{OperationPhase, OperationStatus, OperationSummary};

pub(super) const DEFAULT_TOOL_WINDOW_HEIGHT: f32 = 190.0;
pub(super) const TOOL_WINDOW_HEIGHT_RANGE: std::ops::RangeInclusive<f32> = 140.0..=300.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ToolWindow {
    Problems,
    Console,
    Tasks,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(super) enum CompactView {
    Explorer,
    #[default]
    Editor,
    Inspector,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum LayoutMode {
    Compact,
    Standard,
    Wide,
}

pub(super) fn layout_mode(width: f32) -> LayoutMode {
    if width < 760.0 {
        LayoutMode::Compact
    } else if width < 1160.0 {
        LayoutMode::Standard
    } else {
        LayoutMode::Wide
    }
}

pub(super) fn explorer_panel_metrics(layout: LayoutMode) -> (f32, f32, f32) {
    match layout {
        LayoutMode::Compact => (0.0, 0.0, 0.0),
        LayoutMode::Standard => (330.0, 280.0, 480.0),
        LayoutMode::Wide => (390.0, 330.0, 600.0),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct WorkbenchState {
    pub(super) explorer_visible: bool,
    pub(super) bottom_visible: bool,
    pub(super) inspector_visible: bool,
    pub(super) active_tool_window: ToolWindow,
    pub(super) compact_view: CompactView,
}

impl Default for WorkbenchState {
    fn default() -> Self {
        Self {
            explorer_visible: true,
            bottom_visible: true,
            inspector_visible: true,
            active_tool_window: ToolWindow::Problems,
            compact_view: CompactView::Editor,
        }
    }
}

impl WorkbenchState {
    pub(super) fn open_tool_window(&mut self, tool_window: ToolWindow) {
        self.active_tool_window = tool_window;
        self.bottom_visible = true;
    }

    pub(super) fn toggle_bottom_from_shortcut(&mut self) {
        if self.bottom_visible {
            self.bottom_visible = false;
        } else {
            self.active_tool_window = ToolWindow::Console;
            self.bottom_visible = true;
        }
    }
}

pub(super) fn visible_operation_rows(
    active: &[OperationSummary],
    history: &[OperationSummary],
) -> Vec<OperationSummary> {
    let mut seen = std::collections::HashSet::new();
    let mut rows = Vec::with_capacity(active.len() + history.len());

    for operation in active {
        if seen.insert(operation.id.as_str()) {
            rows.push(operation.clone());
        }
    }
    for operation in history.iter().rev() {
        if seen.insert(operation.id.as_str()) {
            rows.push(operation.clone());
        }
    }
    rows
}

pub(super) fn operation_title(action: &str) -> &str {
    match action {
        "workspace.check" => "Check workspace",
        "workspace.test" => "Test workspace",
        "workspace.build" => "Build workspace",
        "workspace.format" => "Format workspace",
        "workspace.run" => "Run workspace",
        _ => action,
    }
}

pub(super) fn operation_phase_label(phase: OperationPhase) -> &'static str {
    match phase {
        OperationPhase::Queued => "Queued",
        OperationPhase::Resolving => "Resolving",
        OperationPhase::Acquiring => "Acquiring",
        OperationPhase::Installing => "Installing",
        OperationPhase::Verifying => "Verifying",
        OperationPhase::Building => "Building",
        OperationPhase::Staging => "Staging",
        OperationPhase::Publishing => "Publishing",
        OperationPhase::Running => "Running",
        OperationPhase::Finalizing => "Finishing",
        OperationPhase::Complete => "Complete",
    }
}

pub(super) fn operation_status_label(status: &OperationStatus) -> &'static str {
    match status {
        OperationStatus::Running => "In progress",
        OperationStatus::Cancelling => "Stopping",
        OperationStatus::Succeeded => "Succeeded",
        OperationStatus::Failed { .. } => "Failed",
        OperationStatus::Cancelled => "Cancelled",
    }
}

#[cfg(test)]
mod tests {
    use super::{
        CompactView, DEFAULT_TOOL_WINDOW_HEIGHT, LayoutMode, TOOL_WINDOW_HEIGHT_RANGE, ToolWindow,
        WorkbenchState, explorer_panel_metrics, layout_mode, operation_phase_label,
        operation_status_label, operation_title, visible_operation_rows,
    };
    use vapor_client_protocol::{OperationPhase, OperationStatus, OperationSummary};

    #[test]
    fn workbench_starts_with_problems_and_inspector_visible() {
        let state = WorkbenchState::default();

        assert!(state.explorer_visible);
        assert!(state.bottom_visible);
        assert!(state.inspector_visible);
        assert_eq!(state.active_tool_window, ToolWindow::Problems);
        assert_eq!(state.compact_view, CompactView::Editor);
    }

    #[test]
    fn layout_adapts_from_single_pane_to_docked_workbench() {
        assert_eq!(layout_mode(640.0), LayoutMode::Compact);
        assert_eq!(layout_mode(980.0), LayoutMode::Standard);
        assert_eq!(layout_mode(1440.0), LayoutMode::Wide);
    }

    #[test]
    fn explorer_gets_more_room_on_wide_workspaces() {
        assert_eq!(
            explorer_panel_metrics(LayoutMode::Standard),
            (330.0, 280.0, 480.0)
        );
        assert_eq!(
            explorer_panel_metrics(LayoutMode::Wide),
            (390.0, 330.0, 600.0)
        );
    }

    #[test]
    fn bottom_tools_have_a_bounded_default_height() {
        assert_eq!(DEFAULT_TOOL_WINDOW_HEIGHT, 190.0);
        assert_eq!(*TOOL_WINDOW_HEIGHT_RANGE.start(), 140.0);
        assert_eq!(*TOOL_WINDOW_HEIGHT_RANGE.end(), 300.0);
    }

    #[test]
    fn explorer_visibility_is_independent_of_the_narrow_view_selection() {
        let state = WorkbenchState {
            explorer_visible: false,
            compact_view: CompactView::Inspector,
            ..WorkbenchState::default()
        };

        assert!(!state.explorer_visible);
        assert_eq!(state.compact_view, CompactView::Inspector);
    }

    #[test]
    fn opening_tool_window_restores_bottom_panel_and_selects_it() {
        let mut state = WorkbenchState {
            bottom_visible: false,
            ..WorkbenchState::default()
        };

        state.open_tool_window(ToolWindow::Console);

        assert!(state.bottom_visible);
        assert_eq!(state.active_tool_window, ToolWindow::Console);
    }

    #[test]
    fn f2_toggles_bottom_and_selects_console_when_reopening() {
        let mut state = WorkbenchState::default();

        state.toggle_bottom_from_shortcut();
        assert!(!state.bottom_visible);

        state.toggle_bottom_from_shortcut();
        assert!(state.bottom_visible);
        assert_eq!(state.active_tool_window, ToolWindow::Console);
    }

    #[test]
    fn operation_rows_put_active_first_and_deduplicate_history() {
        let active = vec![operation("active")];
        let history = vec![operation("older"), operation("active"), operation("newer")];

        let rows = visible_operation_rows(&active, &history);

        assert_eq!(
            rows.iter().map(|row| row.id.as_str()).collect::<Vec<_>>(),
            ["active", "newer", "older"]
        );
    }

    #[test]
    fn operation_labels_use_player_readable_names_and_phases() {
        assert_eq!(operation_title("workspace.test"), "Test workspace");
        assert_eq!(operation_title("custom.operation"), "custom.operation");
        assert_eq!(operation_phase_label(OperationPhase::Building), "Building");
        assert_eq!(
            operation_status_label(&OperationStatus::Failed {
                message: "failure".to_owned(),
            }),
            "Failed"
        );
    }

    fn operation(id: &str) -> OperationSummary {
        OperationSummary {
            id: id.to_owned(),
            composition_id: None,
            action: format!("workspace.{id}"),
            phase: OperationPhase::Complete,
            status: OperationStatus::Succeeded,
            progress: Some(1.0),
        }
    }
}
