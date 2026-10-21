//! Shared panel treatment for visually separating adjacent work areas.

use eframe::egui;

/// A compact, titled surface for one coherent area of a GUI page.
pub struct Panel<'a> {
    title: egui::WidgetText,
    description: Option<egui::WidgetText>,
    contents: Box<dyn FnOnce(&mut egui::Ui) + 'a>,
}

impl<'a> Panel<'a> {
    pub fn new(
        title: impl Into<egui::WidgetText>,
        contents: impl FnOnce(&mut egui::Ui) + 'a,
    ) -> Self {
        Self {
            title: title.into(),
            description: None,
            contents: Box::new(contents),
        }
    }

    pub fn description(mut self, description: impl Into<egui::WidgetText>) -> Self {
        self.description = Some(description.into());
        self
    }

    pub fn show(self, ui: &mut egui::Ui) {
        let hover_description = self.description.clone();
        let response = egui::Frame::group(ui.style())
            .fill(ui.visuals().faint_bg_color)
            .inner_margin(egui::Margin::symmetric(8, 6))
            .show(ui, |ui| {
                ui.vertical(|ui| {
                    ui.label(self.title.strong());
                    if let Some(description) = self.description {
                        ui.label(description.small().weak());
                    }
                    ui.add_space(3.0);
                    (self.contents)(ui);
                });
            });
        if response.response.hovered() {
            ui.painter().rect_stroke(
                response.response.rect,
                egui::CornerRadius::same(6),
                ui.visuals().widgets.hovered.bg_stroke,
                egui::StrokeKind::Inside,
            );
        }
        if let Some(description) = hover_description {
            response.response.on_hover_text(description);
        }
    }
}
