use super::egui;

pub(super) const HEIGHT: f32 = 36.0;
pub(super) const PADDING_X: f32 = 12.0;

#[cfg(target_os = "macos")]
pub(super) fn cell<R>(
    ui: &mut egui::Ui,
    width: f32,
    height: f32,
    align: egui::Align,
    content: impl FnOnce(&mut egui::Ui) -> R,
) -> R {
    let layout = if align == egui::Align::Max {
        egui::Layout::right_to_left(egui::Align::Center)
    } else {
        egui::Layout::left_to_right(egui::Align::Center)
    };
    ui.allocate_ui_with_layout(egui::vec2(width, height), layout, |ui| {
        ui.set_width(width);
        content(ui)
    })
    .inner
}

pub(super) fn field<'a>(ui: &egui::Ui, value: &'a mut String) -> egui::TextEdit<'a> {
    let font = egui::TextStyle::Button.resolve(ui.style());
    let line_height = ui.fonts_mut(|fonts| fonts.row_height(&font));
    let vertical = (HEIGHT - line_height).max(0.0).round() as i8;
    egui::TextEdit::singleline(value)
        .font(egui::TextStyle::Button)
        .margin(egui::Margin {
            left: PADDING_X as i8,
            right: PADDING_X as i8,
            top: vertical / 2,
            bottom: vertical - vertical / 2,
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn controls_share_the_same_height_and_inset() {
        let ctx = egui::Context::default();
        super::super::configure_ui(&ctx);
        let mut value = String::new();
        let _ = ctx.run_ui(egui::RawInput::default(), |ui| {
            ui.horizontal(|ui| {
                let plain = ui.button("Refresh").rect.height();
                let icon = super::super::icons::button(ui, super::super::icons::Icon::Help, "Help")
                    .rect
                    .height();
                let input = ui.add(field(ui, &mut value)).rect.height();
                assert!((plain - HEIGHT).abs() < 0.1, "button height {plain}");
                assert!((icon - HEIGHT).abs() < 0.1, "icon height {icon}");
                assert!((input - HEIGHT).abs() <= 0.5, "input height {input}");
            });
        });
    }
}
