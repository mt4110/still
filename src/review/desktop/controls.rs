use super::egui;

pub(super) const HEIGHT: f32 = 36.0;
pub(super) const PADDING_X: f32 = 12.0;

/// Center the visible glyphs, including fallback glyphs and descenders.
/// A galley's line box can contain asymmetric leading even at the same point size.
#[cfg(target_os = "macos")]
pub(super) fn paint_label(
    ui: &egui::Ui,
    rect: egui::Rect,
    label: &str,
    align: egui::Align,
    color: egui::Color32,
) {
    if rect.width() <= 0.0 || rect.height() <= 0.0 {
        return;
    }
    let mut job = egui::text::LayoutJob::simple_singleline(
        label.to_owned(),
        egui::TextStyle::Button.resolve(ui.style()),
        color,
    );
    job.wrap.max_width = rect.width();
    job.wrap.max_rows = 1;
    job.wrap.break_anywhere = true;
    let galley = ui.painter().layout_job(job);
    let ink = galley.mesh_bounds;
    if !ink.is_finite() {
        return;
    }
    let x = if align == egui::Align::Min {
        rect.left() - ink.left()
    } else {
        rect.center().x - ink.center().x
    };
    let position = egui::pos2(x, rect.center().y - ink.center().y);
    ui.painter()
        .with_clip_rect(ui.clip_rect().intersect(rect))
        .galley(position, galley, color);
}

#[cfg(target_os = "macos")]
pub(super) fn button(
    ui: &mut egui::Ui,
    label: &str,
    primary: bool,
    enabled: bool,
) -> egui::Response {
    let mut button =
        egui::Button::new(egui::RichText::new(label).color(egui::Color32::TRANSPARENT))
            .min_size(egui::vec2(0.0, HEIGHT))
            .truncate();
    if primary {
        button = button.fill(super::ACCENT);
    }
    let response = ui.add_enabled(enabled, button);
    let color = if primary {
        egui::Color32::WHITE
    } else {
        ui.style().interact(&response).text_color()
    };
    let color = if enabled && ui.is_enabled() {
        color
    } else {
        color.gamma_multiply(ui.visuals().disabled_alpha())
    };
    paint_label(
        ui,
        response.rect.shrink2(egui::vec2(PADDING_X, 0.0)),
        label,
        egui::Align::Center,
        color,
    );
    response.on_hover_text(label)
}

#[cfg(target_os = "macos")]
pub(super) fn menu(
    ui: &mut egui::Ui,
    label: &str,
    contents: impl FnOnce(&mut egui::Ui),
) -> egui::Response {
    let response = button(ui, label, false, true);
    egui::Popup::menu(&response).show(|ui| {
        ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Extend);
        contents(ui);
    });
    response
}

#[cfg(target_os = "macos")]
pub(super) fn language(ui: &mut egui::Ui, japanese: &mut bool) -> egui::Response {
    let label = if *japanese { "日本語" } else { "English" };
    let label_width = ui
        .painter()
        .layout_no_wrap(
            label.into(),
            egui::TextStyle::Button.resolve(ui.style()),
            ui.visuals().text_color(),
        )
        .size()
        .x;
    let width = label_width + PADDING_X * 2.0 + ui.spacing().icon_width + ui.spacing().icon_spacing;
    let response = egui::ComboBox::from_id_salt("language")
        .width(width)
        .selected_text(egui::RichText::new(label).color(egui::Color32::TRANSPARENT))
        .show_ui(ui, |ui| {
            if ui.selectable_value(japanese, true, "日本語").clicked() {
                ui.close();
            }
            if ui.selectable_value(japanese, false, "English").clicked() {
                ui.close();
            }
        })
        .response;
    let mut text_rect = response.rect;
    text_rect.min.x += PADDING_X;
    text_rect.max.x -= PADDING_X + ui.spacing().icon_width + ui.spacing().icon_spacing;
    paint_label(
        ui,
        text_rect,
        label,
        egui::Align::Min,
        ui.style().interact(&response).text_color(),
    );
    response
}

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

    #[cfg(target_os = "macos")]
    #[test]
    fn long_workspace_names_stay_inside_their_button_hit_area() {
        let ctx = egui::Context::default();
        super::super::configure_ui(&ctx);
        let label = format!("{} を調べる", "long-workspace-name-".repeat(8));
        let mut rect = egui::Rect::NOTHING;
        let output = ctx.run_ui(egui::RawInput::default(), |ui| {
            ui.set_max_width(180.0);
            rect = button(ui, &label, true, true).rect;
        });
        assert!(rect.width() <= 180.0);
        let mut visible = 0;
        for shape in output.shapes {
            if let egui::epaint::Shape::Text(text) = shape.shape {
                if text.galley.text() == label
                    && text
                        .galley
                        .rows
                        .iter()
                        .any(|row| row.visuals.mesh.vertices.iter().any(|v| v.color.a() > 0))
                {
                    visible += 1;
                    assert!(text.galley.elided);
                    assert!(rect.contains_rect(shape.clip_rect));
                    assert!(rect.contains_rect(
                        text.galley
                            .mesh_bounds
                            .translate(text.pos.to_vec2())
                            .intersect(shape.clip_rect)
                    ));
                }
            }
        }
        assert_eq!(visible, 1, "one visible, truncated label must remain");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn mixed_script_controls_center_visible_text_at_standard_and_retina_scales() {
        for scale in [1.0, 2.0] {
            let ctx = egui::Context::default();
            super::super::configure_ui(&ctx);
            ctx.set_pixels_per_point(scale);
            let mut controls = Vec::new();
            let mut japanese = true;
            let output = ctx.run_ui(egui::RawInput::default(), |ui| {
                for label in ["still を調べる", "_workspace を調べる", "アプリと SDK"] {
                    let response = button(ui, label, true, true);
                    controls.push((label, response.rect, true));
                }
                let response = menu(ui, "調査先とプロファイル", |_| {});
                controls.push(("調査先とプロファイル", response.rect, true));
                let response = language(ui, &mut japanese);
                controls.push(("日本語", response.rect, false));
            });
            for (label, rect, centered) in controls {
                let text = output
                    .shapes
                    .iter()
                    .find_map(|shape| match &shape.shape {
                        egui::epaint::Shape::Text(text)
                            if text.galley.text() == label
                                && text.galley.rows.iter().any(|row| {
                                    row.visuals.mesh.vertices.iter().any(|v| v.color.a() > 0)
                                }) =>
                        {
                            Some(text)
                        }
                        _ => None,
                    })
                    .expect("the label must be visibly painted");
                let ink = text.galley.mesh_bounds.translate(text.pos.to_vec2());
                assert!(
                    (ink.center().y - rect.center().y).abs() <= 0.5,
                    "{label} at {scale}x: {ink:?} inside {rect:?}"
                );
                if centered {
                    assert!(
                        (ink.center().x - rect.center().x).abs() <= 0.5,
                        "unequal horizontal padding for {label}"
                    );
                } else {
                    assert!((ink.left() - rect.left() - PADDING_X).abs() <= 0.5);
                }
                assert!(
                    rect.contains_rect(ink),
                    "text must remain inside its control"
                );
            }
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn japanese_and_latin_ascenders_have_balanced_visible_heights() {
        let ctx = egui::Context::default();
        super::super::configure_ui(&ctx);
        ctx.set_pixels_per_point(2.0);
        let _ = ctx.run_ui(egui::RawInput::default(), |ui| {
            let font = egui::TextStyle::Button.resolve(ui.style());
            let height = |label: &str| {
                ui.painter()
                    .layout_no_wrap(label.into(), font.clone(), egui::Color32::BLACK)
                    .mesh_bounds
                    .height()
            };
            let japanese = height("アプリと調べる");
            for label in ["SDK", "still", "workspace"] {
                let latin = height(label);
                assert!(
                    japanese.max(latin) / japanese.min(latin) <= 1.2,
                    "{label}: Japanese {japanese}, Latin {latin}"
                );
            }
        });
    }

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
