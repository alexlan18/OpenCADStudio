//! The AI assistant chat panel: settings, transcript and composer.

use crate::app::assistant::{AssistantMsg, AssistantPanel, Effort, Entry, Provider};
use crate::app::Message;
use crate::t;
use crate::ui::dock::PanelId;
use crate::ui::style::common::muted_style;
use crate::ui::style::form::{button_style, field_style};
use iced::widget::{
    button, checkbox, column, container, image, markdown, mouse_area, pick_list, row, scrollable,
    text, text_editor, text_input, tooltip, Space,
};
use iced::{Background, Border, Element, Fill, Length, Theme};

fn msg(m: AssistantMsg) -> Message {
    Message::Assistant(m)
}

fn card(theme: &Theme) -> container::Style {
    let palette = theme.palette();
    container::Style {
        background: Some(Background::Color(palette.background.weak.color)),
        border: Border {
            color: palette.background.neutral.color,
            width: 1.0,
            radius: 6.0.into(),
        },
        ..Default::default()
    }
}

fn user_bubble(theme: &Theme) -> container::Style {
    let pair = theme.palette().primary.weak;
    container::Style {
        background: Some(Background::Color(pair.color)),
        text_color: Some(pair.text),
        border: Border {
            radius: 8.0.into(),
            ..Default::default()
        },
        ..Default::default()
    }
}

fn error_card(theme: &Theme) -> container::Style {
    let pair = theme.palette().danger.weak;
    container::Style {
        background: Some(Background::Color(pair.color)),
        text_color: Some(pair.text),
        border: Border {
            color: theme.palette().danger.base.color,
            width: 1.0,
            radius: 6.0.into(),
        },
        ..Default::default()
    }
}

fn label<'a>(s: impl Into<String>) -> Element<'a, Message> {
    text(s.into()).size(11).style(muted_style).into()
}

fn settings_view<'a>(panel: &'a AssistantPanel) -> Element<'a, Message> {
    let s = &panel.settings;
    let names: Vec<String> = s.profiles.iter().map(|p| p.name.clone()).collect();
    let none_label = t!("None").into_owned();

    // Which profile chats, which one looks at captures.
    let chat = pick_list(Some(s.active.clone()), names.clone(), |n: &String| n.clone())
        .on_select(|n| msg(AssistantMsg::SelectChat(n)))
        .text_size(12)
        .padding([4, 8])
        .width(Fill);
    let mut vision_options = vec![none_label.clone()];
    vision_options.extend(names.iter().cloned());
    let vision_selected = if s.vision_model.is_empty() { none_label.clone() } else { s.vision_model.clone() };
    let vision = pick_list(Some(vision_selected), vision_options, |n: &String| n.clone())
        .on_select(move |n| msg(AssistantMsg::SelectVision(if n == none_label { String::new() } else { n })))
        .text_size(12)
        .padding([4, 8])
        .width(Fill);

    // The profile under edit.
    let editing_name = s
        .profile(&panel.editing)
        .map(|p| p.name.clone())
        .unwrap_or_else(|| s.active.clone());
    let editing = pick_list(Some(editing_name.clone()), names, |n: &String| n.clone())
        .on_select(|n| msg(AssistantMsg::SelectEditing(n)))
        .text_size(12)
        .padding([4, 8])
        .width(Fill);
    let add = bar_button(
        crate::ui::icons::themed_secondary(crate::ui::icons::PLUS, 14.0),
        t!("Add model").into_owned(),
        AssistantMsg::AddProfile,
    );
    let remove: Element<'a, Message> = if s.profiles.len() > 1 {
        bar_button(
            crate::ui::icons::themed_secondary(crate::ui::icons::TRASH, 14.0),
            t!("Remove model").into_owned(),
            AssistantMsg::RemoveProfile,
        )
    } else {
        crate::ui::icons::themed_disabled(crate::ui::icons::TRASH, 14.0)
    };

    let mut col = column![
        label(t!("Chat model")),
        chat,
        label(t!("Vision model")),
        vision,
        text(t!("Describes captures when the chat model cannot see images")).size(10).style(muted_style),
        label(t!("Profile")),
        row![editing, add, remove].spacing(4).align_y(iced::Center),
    ]
    .spacing(3);

    let Some(p) = s.profile(&editing_name) else {
        return container(col).padding(8).width(Fill).style(card).into();
    };
    let name = text_input("", &p.name)
        .on_input(|v| msg(AssistantMsg::ProfileName(v)))
        .size(12)
        .padding([4, 6])
        .style(field_style);
    let provider = pick_list(Some(p.provider), Provider::ALL, |p: &Provider| p.label().to_string())
        .on_select(|p| msg(AssistantMsg::Provider(p)))
        .text_size(12)
        .padding([4, 8])
        .width(Fill);
    let base_url = text_input(p.provider.default_base_url(), &p.base_url)
        .on_input(|v| msg(AssistantMsg::BaseUrl(v)))
        .size(12)
        .padding([4, 6])
        .style(field_style);
    let model_placeholder = if p.provider.default_model().is_empty() {
        t!("Model name").into_owned()
    } else {
        p.provider.default_model().to_string()
    };
    let model = text_input(&model_placeholder, &p.model)
        .on_input(|v| msg(AssistantMsg::Model(v)))
        .size(12)
        .padding([4, 6])
        .style(field_style);
    let key_hint = crate::tf!("Empty uses the {} environment variable", p.provider.env_key());
    let api_key = text_input(&key_hint, &p.api_key)
        .secure(true)
        .on_input(|v| msg(AssistantMsg::ApiKey(v)))
        .size(12)
        .padding([4, 6])
        .style(field_style);
    col = col
        .push(label(t!("Name")))
        .push(name)
        .push(label(t!("Provider")))
        .push(provider)
        .push(label(t!("Base URL")))
        .push(base_url)
        .push(label(t!("Model")))
        .push(model)
        .push(label(t!("API key")))
        .push(api_key);
    if p.provider == Provider::Anthropic {
        let effort = pick_list(Some(p.effort), Effort::ALL, |e: &Effort| e.label().to_string())
            .on_select(|e| msg(AssistantMsg::Effort(e)))
            .text_size(12)
            .padding([4, 8])
            .width(Fill);
        col = col.push(label(t!("Effort"))).push(effort);
    }
    col = col.push(
        checkbox(p.vision)
            .label(t!("Understands images").into_owned())
            .on_toggle(|on| msg(AssistantMsg::ProfileVision(on)))
            .size(14)
            .text_size(12),
    );
    col = col.push(
        text(t!("Settings are saved with your preferences; the API key is stored in plain text."))
            .size(10)
            .style(muted_style),
    );
    container(col).padding(8).width(Fill).style(card).into()
}

fn tool_entry<'a>(
    index: usize,
    summary: &'a str,
    input: &'a str,
    result: Option<&'a crate::app::assistant::ToolResultView>,
    expanded: bool,
) -> Element<'a, Message> {
    let status: Element<'a, Message> = match result {
        None => text("…").size(12).style(muted_style).into(),
        Some(r) if r.ok => crate::ui::icons::themed_success(crate::ui::icons::CHECK, 12.0),
        Some(_) => crate::ui::icons::themed_warning(crate::ui::icons::CLOSE, 12.0),
    };
    let chevron: Element<'a, Message> = if expanded {
        crate::ui::icons::themed_arrow_down(10.0)
    } else {
        crate::ui::icons::themed_arrow_right(10.0)
    };
    let header = row![chevron, status, text(summary).size(11).width(Fill)]
        .spacing(6)
        .align_y(iced::Center);
    let header = mouse_area(container(header).padding([4, 6]).width(Fill))
        .on_press(msg(AssistantMsg::ToggleEntry(index)))
        .interaction(iced::mouse::Interaction::Pointer);
    let mut body = column![header].spacing(4);
    if expanded {
        body = body.push(label(t!("Arguments")));
        body = body.push(
            container(text(input).size(10).font(iced::Font::MONOSPACE))
                .padding(6)
                .width(Fill),
        );
        if let Some(r) = result {
            body = body.push(label(if r.ok { t!("Result") } else { t!("Failed") }));
            body = body.push(
                container(text(r.text.as_str()).size(10).font(iced::Font::MONOSPACE))
                    .padding(6)
                    .width(Fill),
            );
        }
    }
    if let Some(handle) = result.and_then(|r| r.image.as_ref()) {
        body = body.push(
            container(image(handle.clone()).width(Fill).content_fit(iced::ContentFit::Contain))
                .padding(4)
                .width(Fill),
        );
    }
    container(body).width(Fill).style(card).into()
}

fn transcript<'a>(panel: &'a AssistantPanel, theme: &Theme) -> Element<'a, Message> {
    let mut col = column![].spacing(8).width(Fill).padding([4, 6]);
    if panel.entries.is_empty() {
        col = col.push(
            text(t!("Describe what to draw, change, measure or check. The assistant works on the open drawing through the same tools the MCP server exposes."))
                .size(12)
                .style(muted_style),
        );
    }
    let md_settings = markdown::Settings::with_text_size(12, theme);
    for (index, entry) in panel.entries.iter().enumerate() {
        let element: Element<'a, Message> = match entry {
            Entry::User(text_body) => row![
                Space::new().width(Length::FillPortion(1)),
                container(text(text_body.as_str()).size(12))
                    .padding([6, 10])
                    .width(Length::FillPortion(5))
                    .style(user_bubble),
            ]
            .into(),
            Entry::Assistant { markdown: content, .. } => container(
                markdown::view(content.items(), md_settings)
                    .map(|url| Message::OpenUrl(url.to_string())),
            )
            .width(Fill)
            .into(),
            Entry::Tool {
                summary,
                input,
                result,
                expanded,
                ..
            } => tool_entry(index, summary, input, result.as_ref(), *expanded),
            Entry::Error(error) => container(text(error.as_str()).size(11))
                .padding([6, 8])
                .width(Fill)
                .style(error_card)
                .into(),
        };
        col = col.push(element);
    }
    if panel.running {
        col = col.push(text(t!("Working…")).size(11).style(muted_style));
    }
    scrollable(col)
        .id(panel.scroll_id.clone())
        .height(Fill)
        .width(Fill)
        .into()
}

fn composer<'a>(panel: &'a AssistantPanel) -> Element<'a, Message> {
    let editor = text_editor(&panel.input)
        .on_action(|action| msg(AssistantMsg::Input(action)))
        .placeholder(t!("Ask the assistant… Enter sends, Shift+Enter adds a line").into_owned())
        .size(12)
        .padding([6, 8])
        .height(Length::Fixed(84.0))
        .key_binding(|press| {
            use iced::keyboard::key::Named;
            use iced::keyboard::Key;
            let plain_enter = matches!(press.key.as_ref(), Key::Named(Named::Enter))
                && matches!(press.status, text_editor::Status::Focused { .. })
                && !press.modifiers.shift();
            if plain_enter {
                Some(text_editor::Binding::Custom(msg(AssistantMsg::Send)))
            } else {
                text_editor::Binding::from_key_press(press)
            }
        })
        .style(|theme: &Theme, status| {
            let palette = theme.palette();
            let border_color = match status {
                text_editor::Status::Focused { .. } => palette.primary.base.color,
                _ => palette.background.neutral.color,
            };
            text_editor::Style {
                background: Background::Color(palette.background.base.color),
                border: Border {
                    color: border_color,
                    width: 1.0,
                    radius: 4.0.into(),
                },
                placeholder: palette.background.base.text.scale_alpha(0.5),
                value: palette.background.base.text,
                selection: palette.primary.weak.color,
            }
        });
    let action: Element<'a, Message> = if panel.running {
        button(text(t!("Stop")).size(12))
            .on_press(msg(AssistantMsg::Stop))
            .padding([5, 12])
            .style(button_style(false))
            .into()
    } else {
        let can_send = !panel.input.text().trim().is_empty();
        let mut send = button(text(t!("Send")).size(12))
            .padding([5, 12])
            .style(button_style(true));
        if can_send {
            send = send.on_press(msg(AssistantMsg::Send));
        }
        send.into()
    };
    let usage = if panel.usage.input_tokens + panel.usage.output_tokens > 0 {
        crate::tf!(
            "Tokens: {} in / {} out",
            panel.usage.input_tokens,
            panel.usage.output_tokens
        )
        .into_owned()
    } else {
        String::new()
    };
    column![
        editor,
        row![
            text(usage).size(10).style(muted_style).width(Fill),
            action
        ]
        .spacing(6)
        .align_y(iced::Center),
    ]
    .spacing(4)
    .into()
}

fn bar_button<'a>(icon: Element<'a, Message>, tip: String, m: AssistantMsg) -> Element<'a, Message> {
    let b = button(icon).style(button::text).padding([3, 5]).on_press(msg(m));
    tooltip(b, text(tip).size(10), tooltip::Position::Bottom)
        .gap(4)
        .into()
}

pub fn view<'a>(
    panel: &'a AssistantPanel,
    width: f32,
    auto_collapse: bool,
    theme: &Theme,
) -> Element<'a, Message> {
    let title_bar = crate::ui::dock::title_bar(
        PanelId::Assistant,
        t!("AI Assistant").into_owned(),
        auto_collapse,
    );
    let chat = panel.settings.active();
    let model = chat.effective_model();
    let model_label = if model.is_empty() {
        t!("No model set").into_owned()
    } else if chat.name.trim().is_empty() || chat.name == model {
        model
    } else {
        format!("{} · {}", chat.name, model)
    };
    let toolbar = row![
        text(model_label).size(10).style(muted_style).width(Fill),
        bar_button(
            crate::ui::icons::themed_secondary(crate::ui::icons::DOC_NEW, 14.0),
            t!("New chat").into_owned(),
            AssistantMsg::NewChat,
        ),
        bar_button(
            if panel.settings_open {
                crate::ui::icons::themed_primary(crate::ui::icons::GEAR, 14.0)
            } else {
                crate::ui::icons::themed_secondary(crate::ui::icons::GEAR, 14.0)
            },
            t!("Settings").into_owned(),
            AssistantMsg::ToggleSettings,
        ),
    ]
    .spacing(2)
    .align_y(iced::Center);
    let mut body = column![title_bar, toolbar].spacing(4).height(Fill);
    if panel.settings_open {
        body = body.push(settings_view(panel));
    }
    body = body.push(transcript(panel, theme));
    body = body.push(composer(panel));
    crate::ui::dock::frame(body, width)
}
