use eframe::egui::{self, Color32, Stroke};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IdePalette {
    pub canvas: Color32,
    pub panel: Color32,
    pub raised: Color32,
    pub editor: Color32,
    pub border: Color32,
    pub accent: Color32,
    pub accent_soft: Color32,
    pub text: Color32,
    pub muted_text: Color32,
}

pub const IDE_PALETTE: IdePalette = IdePalette {
    canvas: Color32::from_rgb(16, 18, 23),
    panel: Color32::from_rgb(23, 27, 34),
    raised: Color32::from_rgb(31, 36, 45),
    editor: Color32::from_rgb(18, 21, 27),
    border: Color32::from_rgb(49, 57, 69),
    accent: Color32::from_rgb(104, 169, 245),
    accent_soft: Color32::from_rgb(42, 70, 103),
    text: Color32::from_rgb(226, 232, 240),
    muted_text: Color32::from_rgb(151, 163, 179),
};

pub fn ide_visuals() -> egui::Visuals {
    let mut visuals = egui::Visuals::dark();
    visuals.dark_mode = true;
    visuals.panel_fill = IDE_PALETTE.canvas;
    visuals.window_fill = IDE_PALETTE.panel;
    visuals.extreme_bg_color = IDE_PALETTE.editor;
    visuals.faint_bg_color = IDE_PALETTE.panel;
    visuals.override_text_color = Some(IDE_PALETTE.text);
    visuals.weak_text_color = Some(IDE_PALETTE.muted_text);
    visuals.selection.bg_fill = IDE_PALETTE.accent;
    visuals.selection.stroke = Stroke::new(1.0, IDE_PALETTE.accent);

    visuals.widgets.noninteractive.bg_fill = IDE_PALETTE.panel;
    visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0, IDE_PALETTE.border);
    visuals.widgets.noninteractive.fg_stroke.color = IDE_PALETTE.text;

    visuals.widgets.inactive.bg_fill = IDE_PALETTE.panel;
    visuals.widgets.inactive.bg_stroke = Stroke::new(1.0, IDE_PALETTE.border);
    visuals.widgets.inactive.fg_stroke.color = IDE_PALETTE.text;

    visuals.widgets.hovered.bg_fill = IDE_PALETTE.raised;
    visuals.widgets.hovered.bg_stroke = Stroke::new(1.0, IDE_PALETTE.accent);
    visuals.widgets.hovered.fg_stroke.color = Color32::WHITE;

    visuals.widgets.active.bg_fill = IDE_PALETTE.accent_soft;
    visuals.widgets.active.bg_stroke = Stroke::new(1.0, IDE_PALETTE.accent);
    visuals.widgets.active.fg_stroke.color = Color32::WHITE;

    visuals
}

pub fn apply_ide_theme(context: &egui::Context) {
    context.set_theme(egui::ThemePreference::Dark);
    context.set_visuals(ide_visuals());
    context.style_mut_of(egui::Theme::Dark, |style| {
        style.spacing.item_spacing = egui::vec2(7.0, 5.0);
        style.spacing.button_padding = egui::vec2(8.0, 4.0);
        style.spacing.interact_size = egui::vec2(24.0, 22.0);
    });
}

pub fn tool_window_header(ui: &mut egui::Ui, title: &str, detail: Option<&str>) {
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(title).strong().size(13.0));
        if let Some(detail) = detail {
            ui.label(
                egui::RichText::new(detail)
                    .small()
                    .color(IDE_PALETTE.muted_text),
            );
        }
    });
    ui.painter().hline(
        ui.min_rect().left()..=ui.min_rect().right(),
        ui.min_rect().bottom() + 3.0,
        Stroke::new(1.0, IDE_PALETTE.border),
    );
    ui.add_space(5.0);
}

#[cfg(test)]
mod tests {
    use super::{IDE_PALETTE, apply_ide_theme, ide_visuals};

    #[test]
    fn palette_uses_graphite_surfaces_and_a_cool_blue_accent() {
        assert_eq!(
            IDE_PALETTE.canvas,
            eframe::egui::Color32::from_rgb(16, 18, 23)
        );
        assert_eq!(
            IDE_PALETTE.panel,
            eframe::egui::Color32::from_rgb(23, 27, 34)
        );
        assert_eq!(
            IDE_PALETTE.accent,
            eframe::egui::Color32::from_rgb(104, 169, 245)
        );
    }

    #[test]
    fn visuals_apply_palette_to_panels_editor_and_selection() {
        let visuals = ide_visuals();

        assert!(visuals.dark_mode);
        assert_eq!(visuals.panel_fill, IDE_PALETTE.canvas);
        assert_eq!(visuals.extreme_bg_color, IDE_PALETTE.editor);
        assert_eq!(visuals.selection.bg_fill, IDE_PALETTE.accent);
        assert_eq!(visuals.widgets.inactive.bg_fill, IDE_PALETTE.panel);
    }

    #[test]
    fn ide_theme_applies_compact_shared_spacing() {
        let context = eframe::egui::Context::default();
        apply_ide_theme(&context);

        let style = context.style_of(eframe::egui::Theme::Dark);
        assert_eq!(style.spacing.item_spacing, eframe::egui::vec2(7.0, 5.0));
        assert_eq!(style.spacing.button_padding, eframe::egui::vec2(8.0, 4.0));
        assert_eq!(style.spacing.interact_size, eframe::egui::vec2(24.0, 22.0));
    }
}
