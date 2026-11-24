use eframe::egui::{self, Color32, Pos2, Rect, Sense, Stroke, Vec2};
use vapor_console_core::ConsoleLevel;

/// Paint the shared console's severity mark using vector primitives.
pub fn console_mark(ui: &mut egui::Ui, level: ConsoleLevel) {
    let size = Vec2::splat(13.0);
    let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
    let painter = ui.painter();
    let color = match level {
        ConsoleLevel::Trace | ConsoleLevel::Debug => ui.visuals().weak_text_color(),
        ConsoleLevel::Info | ConsoleLevel::System => ui.visuals().text_color(),
        ConsoleLevel::Success => egui::Color32::LIGHT_GREEN,
        ConsoleLevel::Warn => egui::Color32::YELLOW,
        ConsoleLevel::Error => egui::Color32::LIGHT_RED,
        ConsoleLevel::Input => egui::Color32::from_rgb(220, 180, 255),
    };
    let center = rect.center();
    let stroke = Stroke::new(1.4, color);
    match level {
        ConsoleLevel::Success => {
            painter.circle_stroke(center, 5.0, stroke);
            painter.line_segment(
                [
                    Pos2::new(rect.left() + 3.5, center.y),
                    Pos2::new(center.x - 0.5, rect.bottom() - 3.5),
                ],
                stroke,
            );
            painter.line_segment(
                [
                    Pos2::new(center.x - 0.5, rect.bottom() - 3.5),
                    Pos2::new(rect.right() - 2.5, rect.top() + 3.0),
                ],
                stroke,
            );
        }
        ConsoleLevel::Warn => {
            let triangle = [
                Pos2::new(center.x, rect.top() + 1.2),
                Pos2::new(rect.right() - 1.0, rect.bottom() - 1.0),
                Pos2::new(rect.left() + 1.0, rect.bottom() - 1.0),
            ];
            painter.add(egui::Shape::closed_line(triangle.to_vec(), stroke));
            painter.line_segment(
                [
                    Pos2::new(center.x, center.y - 2.0),
                    Pos2::new(center.x, center.y + 1.3),
                ],
                stroke,
            );
            painter.circle_filled(Pos2::new(center.x, center.y + 3.4), 0.65, color);
        }
        ConsoleLevel::Error => {
            painter.circle_stroke(center, 5.0, stroke);
            painter.line_segment(
                [
                    Pos2::new(center.x - 2.0, center.y - 2.0),
                    Pos2::new(center.x + 2.0, center.y + 2.0),
                ],
                stroke,
            );
            painter.line_segment(
                [
                    Pos2::new(center.x + 2.0, center.y - 2.0),
                    Pos2::new(center.x - 2.0, center.y + 2.0),
                ],
                stroke,
            );
        }
        ConsoleLevel::Input => {
            painter.rect_stroke(
                Rect::from_center_size(center, Vec2::new(10.0, 8.0)),
                2.0,
                stroke,
                egui::StrokeKind::Inside,
            );
            painter.line_segment(
                [
                    Pos2::new(center.x - 2.2, center.y - 1.6),
                    Pos2::new(center.x - 0.4, center.y),
                ],
                stroke,
            );
            painter.line_segment(
                [
                    Pos2::new(center.x - 0.4, center.y),
                    Pos2::new(center.x - 2.2, center.y + 1.6),
                ],
                stroke,
            );
            painter.line_segment(
                [
                    Pos2::new(center.x + 0.3, center.y + 1.6),
                    Pos2::new(center.x + 2.2, center.y + 1.6),
                ],
                stroke,
            );
        }
        ConsoleLevel::Trace | ConsoleLevel::Debug | ConsoleLevel::Info | ConsoleLevel::System => {
            painter.circle_stroke(center, 4.5, stroke);
            painter.circle_filled(center, 1.5, color);
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdeIcon {
    File,
    Folder,
    Search,
    Run,
    Close,
    Check,
    Warning,
}

pub fn draw_icon(ui: &mut egui::Ui, icon: IdeIcon, size: f32) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(size), Sense::hover());
    let color = if response.hovered() {
        ui.visuals().strong_text_color()
    } else {
        ui.visuals().weak_text_color()
    };
    paint_icon(ui.painter(), rect.shrink(2.0), icon, color);
    response
}

fn paint_icon(painter: &egui::Painter, rect: Rect, icon: IdeIcon, color: Color32) {
    let stroke = Stroke::new(1.4, color);
    let point = |x: f32, y: f32| {
        Pos2::new(
            rect.left() + rect.width() * x,
            rect.top() + rect.height() * y,
        )
    };
    let lines: &[(f32, f32, f32, f32)] = match icon {
        IdeIcon::File => &[
            (0.22, 0.08, 0.62, 0.08),
            (0.62, 0.08, 0.80, 0.27),
            (0.80, 0.27, 0.80, 0.92),
            (0.80, 0.92, 0.22, 0.92),
            (0.22, 0.92, 0.22, 0.08),
            (0.62, 0.08, 0.62, 0.28),
            (0.62, 0.28, 0.80, 0.28),
        ],
        IdeIcon::Folder => &[
            (0.08, 0.27, 0.42, 0.27),
            (0.42, 0.27, 0.52, 0.42),
            (0.52, 0.42, 0.92, 0.42),
            (0.92, 0.42, 0.82, 0.88),
            (0.82, 0.88, 0.08, 0.88),
            (0.08, 0.88, 0.08, 0.27),
        ],
        IdeIcon::Search => &[(0.64, 0.64, 0.94, 0.94)],
        IdeIcon::Run => &[
            (0.28, 0.12, 0.82, 0.50),
            (0.82, 0.50, 0.28, 0.88),
            (0.28, 0.88, 0.28, 0.12),
        ],
        IdeIcon::Close => &[(0.2, 0.2, 0.8, 0.8), (0.8, 0.2, 0.2, 0.8)],
        IdeIcon::Check => &[(0.1, 0.54, 0.40, 0.84), (0.40, 0.84, 0.92, 0.16)],
        IdeIcon::Warning => &[
            (0.50, 0.08, 0.94, 0.88),
            (0.94, 0.88, 0.06, 0.88),
            (0.06, 0.88, 0.50, 0.08),
            (0.50, 0.34, 0.50, 0.59),
        ],
    };
    for &(x1, y1, x2, y2) in lines {
        painter.line_segment([point(x1, y1), point(x2, y2)], stroke);
    }
    if icon == IdeIcon::Search {
        painter.circle_stroke(point(0.40, 0.40), rect.width() * 0.27, stroke);
    }
    if icon == IdeIcon::Warning {
        painter.circle_filled(point(0.50, 0.73), rect.width() * 0.035, color);
    }
}
