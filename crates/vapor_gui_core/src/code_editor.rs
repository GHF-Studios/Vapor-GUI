use std::{collections::BTreeMap, ops::Range, path::Path, sync::OnceLock};

use eframe::egui::{self, Color32, FontId, TextFormat, TextStyle, text::LayoutJob};
use egui_code_editor::{ColorTheme, Syntax, Token, TokenType};

pub fn language_for_path(path: &Path) -> Option<&'static str> {
    syntax_for_path(path).map(Syntax::language)
}

pub fn show(
    ui: &mut egui::Ui,
    path: &Path,
    source: &mut String,
    size: egui::Vec2,
) -> egui::Response {
    show_with_cursor(ui, path, source, size).response
}

pub struct CodeEditorOutput {
    pub response: egui::Response,
    pub cursor: Option<egui::text::CCursorRange>,
}

pub fn show_with_cursor(
    ui: &mut egui::Ui,
    path: &Path,
    source: &mut String,
    size: egui::Vec2,
) -> CodeEditorOutput {
    let Some(syntax) = syntax_for_path(path) else {
        let output = ui
            .allocate_ui_with_layout(size, egui::Layout::top_down(egui::Align::Min), |ui| {
                egui::TextEdit::multiline(source)
                    .font(TextStyle::Monospace)
                    .desired_width(f32::INFINITY)
                    .desired_rows(desired_rows(ui))
                    .id(egui::Id::new(("vapor-code-editor", path)))
                    .show(ui)
            })
            .inner;
        return CodeEditorOutput {
            response: output.response.response,
            cursor: output.cursor_range,
        };
    };

    let font_size = TextStyle::Monospace.resolve(ui.style()).size;
    let mut layouter = move |ui: &egui::Ui, text: &dyn egui::TextBuffer, wrap_width: f32| {
        let mut job =
            highlighted_layout(text.as_str(), syntax, ui.visuals().text_color(), font_size);
        job.wrap.max_width = wrap_width;
        ui.fonts_mut(|fonts| fonts.layout_job(job))
    };
    let output = ui
        .allocate_ui_with_layout(size, egui::Layout::top_down(egui::Align::Min), |ui| {
            egui::TextEdit::multiline(source)
                .font(TextStyle::Monospace)
                .desired_width(f32::INFINITY)
                .desired_rows(desired_rows(ui))
                .id(egui::Id::new(("vapor-code-editor", path)))
                .layouter(&mut layouter)
                .show(ui)
        })
        .inner;
    CodeEditorOutput {
        response: output.response.response,
        cursor: output.cursor_range,
    }
}

/// Move the editor selection to a UTF-8 byte range and focus that editor.
pub fn select_range(ctx: &egui::Context, path: &Path, source: &str, range: Range<usize>) {
    let id = egui::Id::new(("vapor-code-editor", path));
    let mut state = egui::widgets::text_edit::TextEditState::load(ctx, id).unwrap_or_default();
    if let Some((start, end)) = byte_range_to_char_range(source, range) {
        state
            .cursor
            .set_char_range(Some(egui::text::CCursorRange::two(
                egui::text::CCursor::new(start),
                egui::text::CCursor::new(end),
            )));
    }
    state.store(ctx, id);
    ctx.memory_mut(|memory| memory.request_focus(id));
}

fn desired_rows(ui: &egui::Ui) -> usize {
    let line_height = ui.text_style_height(&TextStyle::Monospace).max(1.0);
    (ui.available_height() / line_height).floor().max(1.0) as usize
}

fn byte_range_to_char_range(source: &str, range: Range<usize>) -> Option<(usize, usize)> {
    (range.start <= range.end
        && range.end <= source.len()
        && source.is_char_boundary(range.start)
        && source.is_char_boundary(range.end))
    .then(|| {
        (
            source[..range.start].chars().count(),
            source[..range.end].chars().count(),
        )
    })
}

fn syntax_for_path(path: &Path) -> Option<&'static Syntax> {
    static SYNTAXES: OnceLock<BTreeMap<&'static str, Syntax>> = OnceLock::new();
    let extension = path.extension()?.to_str()?.to_ascii_lowercase();
    let syntaxes = SYNTAXES.get_or_init(|| {
        BTreeMap::from([
            ("rs", Syntax::rust()),
            (
                "rhai",
                Syntax::new("Rhai")
                    .with_comment("//")
                    .with_comment_multiline(["/*", "*/"])
                    .with_keywords([
                        "as", "break", "catch", "const", "continue", "else", "export", "fn", "for",
                        "if", "import", "in", "let", "loop", "private", "return", "throw", "try",
                        "while",
                    ])
                    .with_types([
                        "Array", "Blob", "bool", "char", "Dynamic", "FnPtr", "float", "int", "Map",
                        "string", "Unit",
                    ])
                    .with_special(["false", "this", "true"]),
            ),
            (
                "toml",
                Syntax::new("TOML")
                    .with_comment("#")
                    .with_special(["false", "true"]),
            ),
            ("py", Syntax::python()),
            ("pyw", Syntax::python()),
            ("sh", Syntax::shell()),
            ("bash", Syntax::shell()),
            ("zsh", Syntax::shell()),
            ("lua", Syntax::lua()),
            ("sql", Syntax::sql()),
            ("s", Syntax::asm()),
            ("asm", Syntax::asm()),
        ])
    });
    syntaxes.get(extension.as_str())
}

fn highlighted_layout(
    source: &str,
    syntax: &Syntax,
    foreground: Color32,
    font_size: f32,
) -> LayoutJob {
    let mut job = LayoutJob::default();
    let mut lexer = Token::default();
    let theme = ColorTheme::GRUVBOX;
    for token in lexer.tokens(syntax, source) {
        let color = match token.ty() {
            TokenType::Whitespace(_) | TokenType::Unknown => foreground,
            token_type => parse_rgb(theme.type_color_str(token_type)).unwrap_or(foreground),
        };
        job.append(
            token.buffer(),
            0.0,
            TextFormat {
                font_id: FontId::monospace(font_size),
                color,
                ..Default::default()
            },
        );
    }
    job
}

fn parse_rgb(hex: &str) -> Option<Color32> {
    let hex = hex.strip_prefix('#').unwrap_or(hex);
    if hex.len() != 6 {
        return None;
    }
    let channel = |start| u8::from_str_radix(&hex[start..start + 2], 16).ok();
    Some(Color32::from_rgb(channel(0)?, channel(2)?, channel(4)?))
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use egui_code_editor::{Syntax, Token, TokenType};

    use super::{byte_range_to_char_range, highlighted_layout, language_for_path, syntax_for_path};

    #[test]
    fn selects_language_syntax_from_supported_source_extensions() {
        for (path, expected) in [
            ("src/main.rs", "Rust"),
            ("scripts/startup.rhai", "Rhai"),
            ("Cargo.toml", "TOML"),
            ("tools/build.sh", "Shell"),
            ("scripts/check.py", "Python"),
        ] {
            assert_eq!(
                syntax_for_path(Path::new(path))
                    .as_ref()
                    .map(|syntax| syntax.language()),
                Some(expected),
                "unexpected syntax for {path}"
            );
        }
    }

    #[test]
    fn rhai_files_use_rhai_keywords_and_literals() {
        let syntax = syntax_for_path(Path::new("scripts/startup.rhai")).expect("Rhai syntax");
        let tokens = Token::default().tokens(syntax, "let enabled = true; // script");
        assert!(
            tokens
                .iter()
                .any(|token| { token.ty() == TokenType::Keyword && token.buffer() == "let" })
        );
        assert!(
            tokens
                .iter()
                .any(|token| { token.ty() == TokenType::Special && token.buffer() == "true" })
        );
        assert_eq!(
            language_for_path(Path::new("scripts/startup.rhai")),
            Some("Rhai")
        );
    }

    #[test]
    fn leaves_unsupported_text_files_unhighlighted() {
        assert!(syntax_for_path(Path::new("README.md")).is_none());
        assert!(syntax_for_path(Path::new("assets/logo.svg")).is_none());
    }

    #[test]
    fn uses_language_tokens_to_color_code_instead_of_plain_text() {
        let tokens = Token::default().tokens(&Syntax::rust(), "fn main() { // run\n}");
        assert!(
            tokens
                .iter()
                .any(|token| { token.ty() == TokenType::Keyword && token.buffer() == "fn" })
        );
        assert!(tokens.iter().any(|token| {
            token.ty() == TokenType::Comment(false) && token.buffer().contains("run")
        }));

        let job = highlighted_layout(
            "fn main() {}",
            &Syntax::rust(),
            eframe::egui::Color32::WHITE,
            14.0,
        );
        assert!(
            job.sections
                .iter()
                .any(|section| { section.format.color != eframe::egui::Color32::WHITE })
        );
    }

    #[test]
    fn converts_valid_utf8_byte_ranges_to_editor_character_indices() {
        assert_eq!(byte_range_to_char_range("aλbc", 1..3), Some((1, 2)));
        assert_eq!(byte_range_to_char_range("aλbc", 2..3), None);
        assert_eq!(
            byte_range_to_char_range("aλbc", std::ops::Range { start: 3, end: 2 }),
            None
        );
    }
}
