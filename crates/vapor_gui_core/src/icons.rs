use eframe::egui::{self, Pos2, Rect, Stroke, Vec2};
use vapor_console_core::ConsoleLevel;

/// Paint a small severity mark from vector primitives, without font glyphs.
pub fn console_mark(ui: &mut egui::Ui, level: ConsoleLevel) {
    let size = Vec2::splat(13.0);
    let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
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
