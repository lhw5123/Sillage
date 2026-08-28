//! Maps Markdown segments and A2UI surfaces onto GPUI Component widgets.

use gpui::{
    AnyElement, App, FontWeight, HighlightStyle, Hsla, IntoElement as _, Overflow,
    ParentElement as _, SharedString, StyleRefinement, Styled as _, div, px, rems, rgb, rgba,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::checkbox::Checkbox;
use gpui_component::separator::Separator;
use gpui_component::tab::{Tab, TabBar};
use gpui_component::text::{TextView, TextViewStyle};
use gpui_component::{ActiveTheme as _, Disableable as _, Icon, IconName, h_flex, v_flex};
use serde_json::{Map, Value};

use super::a2ui::{
    Scope, Surface, array_at, child_ids, resolve_bool, resolve_dynamic, resolve_string,
    resolve_strings, stringify_value, template_binding,
};

/// Body copy metrics: 14px on a 23px leading reads as one comfortable column
/// of prose at the widths the task pane uses.
const BODY_SIZE: gpui::Pixels = px(14.);
const BODY_LINE_HEIGHT: gpui::Pixels = px(23.);

/// Agent prose with search hits marked, at the Markdown body's metrics so
/// turning highlights on does not reflow the column.
pub(super) fn highlighted_body(source: &str, query: &str, cx: &App) -> AnyElement {
    div()
        .w_full()
        .text_size(BODY_SIZE)
        .line_height(BODY_LINE_HEIGHT)
        .text_color(cx.theme().foreground.opacity(0.92))
        .child(crate::matching::highlighted(source.to_string(), query, cx))
        .into_any_element()
}

pub(super) fn markdown_view(id: impl Into<SharedString>, source: &str, cx: &App) -> AnyElement {
    TextView::markdown(id.into(), source.to_string())
        .selectable(true)
        .style(markdown_style(cx))
        .text_size(BODY_SIZE)
        .line_height(BODY_LINE_HEIGHT)
        .text_color(cx.theme().foreground.opacity(0.92))
        .into_any_element()
}

/// Reading style for agent prose: quiet body text, headings that step down in
/// tight increments, warm inline code, and tables with room to breathe.
fn markdown_style(cx: &App) -> TextViewStyle {
    let theme = cx.theme();
    let dark = theme.is_dark();

    let mut inline_code = HighlightStyle::default();
    inline_code.color = Some(inline_code_color(dark));
    inline_code.background_color = Some(if dark {
        rgba(0xffffff14).into()
    } else {
        rgba(0x1c17120f).into()
    });

    let mut table = StyleRefinement::default();
    table.overflow.x = Some(Overflow::Scroll);

    let mut style = TextViewStyle::default()
        .paragraph_gap(rems(0.9))
        .heading_font_size(|level, _| match level {
            1 => px(22.),
            2 => px(18.),
            3 => px(16.),
            4 => px(15.),
            _ => BODY_SIZE,
        })
        .inline_code(inline_code)
        .code_block(
            StyleRefinement::default()
                .p_4()
                .rounded(theme.radius_lg)
                .border_1()
                .border_color(theme.border)
                .line_height(px(21.)),
        )
        .table(table)
        .table_head(
            StyleRefinement::default()
                .font_weight(FontWeight::MEDIUM)
                .text_color(theme.foreground.opacity(0.75)),
        )
        .table_cell(
            StyleRefinement::default()
                .px_3()
                .py_2()
                .line_height(px(21.)),
        );

    style.heading_base_font_size = BODY_SIZE;
    style.is_dark = dark;
    style.highlight_theme = theme.highlight_theme.clone();
    style
}

/// Inline code borrows the warm tone editors use for literals, so `code`
/// spans read as code without a heavy pill around every word.
fn inline_code_color(dark: bool) -> Hsla {
    if dark {
        rgb(0xd8a27b).into()
    } else {
        rgb(0xa8552b).into()
    }
}

pub(super) fn surface_view(index: usize, surface: &Surface, cx: &App) -> AnyElement {
    let theme = cx.theme();
    v_flex()
        .w_full()
        .gap_2()
        .p_4()
        .rounded(theme.radius_lg)
        .border_1()
        .border_color(theme.border)
        .bg(theme.muted.opacity(0.2))
        .child(
            div()
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(format!("A2UI · {}", surface.id)),
        )
        .child(render_id(
            &format!("s{index}"),
            surface,
            "root",
            Scope::root(surface.data()),
            &mut Vec::new(),
            cx,
        ))
        .into_any_element()
}

fn render_id(
    prefix: &str,
    surface: &Surface,
    id: &str,
    scope: Scope<'_>,
    stack: &mut Vec<String>,
    cx: &App,
) -> AnyElement {
    if stack.iter().any(|seen| seen == id) {
        return div().into_any_element();
    }
    let Some(component) = surface.component(id) else {
        return placeholder(cx, &format!("Missing component `{id}`"));
    };
    stack.push(id.to_string());
    let element = render_component(prefix, surface, id, component, scope, stack, cx);
    stack.pop();
    element
}

fn render_component(
    prefix: &str,
    surface: &Surface,
    id: &str,
    component: &Map<String, Value>,
    scope: Scope<'_>,
    stack: &mut Vec<String>,
    cx: &App,
) -> AnyElement {
    let kind = component
        .get("component")
        .and_then(Value::as_str)
        .unwrap_or("Unknown");
    let weight = component
        .get("weight")
        .and_then(Value::as_f64)
        .unwrap_or(0.0);
    let node_id = format!("{prefix}-{id}");
    let painted = match kind {
        "Text" => render_text(&node_id, component, scope, cx),
        "Icon" => render_icon(component, scope, cx),
        "Image" => render_media("Image", component, scope, cx),
        "Video" | "AudioPlayer" => render_media(kind, component, scope, cx),
        "Row" => render_axis(true, &node_id, surface, component, scope, stack, cx),
        "Column" => render_axis(false, &node_id, surface, component, scope, stack, cx),
        "List" => render_list(&node_id, surface, component, scope, stack, cx),
        "Card" => render_card(&node_id, surface, component, scope, stack, cx),
        "Tabs" => render_tabs(&node_id, surface, component, scope, stack, cx),
        "Modal" => render_modal(&node_id, surface, component, scope, stack, cx),
        "Divider" => render_divider(component, cx),
        "Button" => render_button(&node_id, surface, component, scope, stack, cx),
        "TextField" => render_text_field(component, scope, cx),
        "CheckBox" => render_checkbox(&node_id, component, scope, cx),
        "ChoicePicker" => render_choice(&node_id, component, scope, cx),
        "Slider" => render_labeled_value("Slider", component, scope, cx),
        "DateTimeInput" => render_labeled_value("Date", component, scope, cx),
        other => placeholder(cx, &format!("Unsupported A2UI component `{other}`")),
    };
    if weight > 0.0 {
        div()
            .flex_grow(weight as f32)
            .min_w_0()
            .child(painted)
            .into_any_element()
    } else {
        painted
    }
}

fn render_text(id: &str, component: &Map<String, Value>, scope: Scope<'_>, cx: &App) -> AnyElement {
    let text = component
        .get("text")
        .map(|value| resolve_string(value, scope))
        .unwrap_or_default();
    let caption = component.get("variant").and_then(Value::as_str) == Some("caption");
    let view = markdown_view(id.to_string(), &text, cx);
    if caption {
        div()
            .text_xs()
            .text_color(cx.theme().muted_foreground)
            .child(view)
            .into_any_element()
    } else {
        view
    }
}

fn render_icon(component: &Map<String, Value>, scope: Scope<'_>, cx: &App) -> AnyElement {
    let name = match component.get("name") {
        Some(Value::String(name)) => name.clone(),
        Some(other) => stringify_value(&resolve_dynamic(other, scope)),
        None => String::new(),
    };
    Icon::new(icon_name(&name))
        .size(px(16.))
        .text_color(cx.theme().foreground)
        .into_any_element()
}

fn render_media(
    kind: &str,
    component: &Map<String, Value>,
    scope: Scope<'_>,
    cx: &App,
) -> AnyElement {
    let url = component
        .get("url")
        .map(|value| resolve_string(value, scope))
        .unwrap_or_default();
    let description = component
        .get("description")
        .map(|value| resolve_string(value, scope))
        .unwrap_or_else(|| kind.into());
    v_flex()
        .gap_1()
        .p_2()
        .rounded(cx.theme().radius)
        .border_1()
        .border_color(cx.theme().border)
        .child(
            div()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(description),
        )
        .child(
            div()
                .text_xs()
                .text_color(cx.theme().accent_foreground)
                .child(url),
        )
        .into_any_element()
}

fn render_axis(
    horizontal: bool,
    prefix: &str,
    surface: &Surface,
    component: &Map<String, Value>,
    scope: Scope<'_>,
    stack: &mut Vec<String>,
    cx: &App,
) -> AnyElement {
    let justify = component
        .get("justify")
        .and_then(Value::as_str)
        .unwrap_or("start");
    let align = component
        .get("align")
        .and_then(Value::as_str)
        .unwrap_or("stretch");
    let children = collect_children(prefix, surface, component, scope, stack, cx);
    if horizontal {
        apply_align(
            apply_justify(h_flex().gap_3().w_full().children(children), justify),
            align,
        )
        .into_any_element()
    } else {
        apply_align(
            apply_justify(v_flex().gap_3().w_full().children(children), justify),
            align,
        )
        .into_any_element()
    }
}

fn render_list(
    prefix: &str,
    surface: &Surface,
    component: &Map<String, Value>,
    scope: Scope<'_>,
    stack: &mut Vec<String>,
    cx: &App,
) -> AnyElement {
    let horizontal = component.get("direction").and_then(Value::as_str) == Some("horizontal");
    let children = collect_children(prefix, surface, component, scope, stack, cx);
    if horizontal {
        h_flex()
            .gap_3()
            .w_full()
            .children(children)
            .into_any_element()
    } else {
        v_flex()
            .gap_2()
            .w_full()
            .children(children)
            .into_any_element()
    }
}

fn render_card(
    prefix: &str,
    surface: &Surface,
    component: &Map<String, Value>,
    scope: Scope<'_>,
    stack: &mut Vec<String>,
    cx: &App,
) -> AnyElement {
    let child = component
        .get("child")
        .and_then(Value::as_str)
        .unwrap_or_default();
    v_flex()
        .w_full()
        .gap_2()
        .p_3()
        .rounded(cx.theme().radius)
        .border_1()
        .border_color(cx.theme().border)
        .bg(cx.theme().background)
        .child(render_id(prefix, surface, child, scope, stack, cx))
        .into_any_element()
}

fn render_tabs(
    prefix: &str,
    surface: &Surface,
    component: &Map<String, Value>,
    scope: Scope<'_>,
    stack: &mut Vec<String>,
    cx: &App,
) -> AnyElement {
    let tabs = component
        .get("tabs")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let titles: Vec<Tab> = tabs
        .iter()
        .filter_map(|tab| tab.get("title"))
        .map(|title| Tab::new().label(resolve_string(title, scope)))
        .collect();
    let selected_child = tabs
        .first()
        .and_then(|tab| tab.get("child"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    v_flex()
        .w_full()
        .gap_3()
        .child(
            TabBar::new(format!("{prefix}-tabs"))
                .selected_index(0)
                .children(titles),
        )
        .child(render_id(prefix, surface, selected_child, scope, stack, cx))
        .into_any_element()
}

fn render_modal(
    prefix: &str,
    surface: &Surface,
    component: &Map<String, Value>,
    scope: Scope<'_>,
    stack: &mut Vec<String>,
    cx: &App,
) -> AnyElement {
    let trigger = component
        .get("trigger")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let content = component
        .get("content")
        .and_then(Value::as_str)
        .unwrap_or_default();
    v_flex()
        .w_full()
        .gap_2()
        .child(render_id(prefix, surface, trigger, scope, stack, cx))
        .child(render_id(prefix, surface, content, scope, stack, cx))
        .into_any_element()
}

fn render_divider(component: &Map<String, Value>, _cx: &App) -> AnyElement {
    if component.get("axis").and_then(Value::as_str) == Some("vertical") {
        Separator::vertical().into_any_element()
    } else {
        Separator::horizontal().into_any_element()
    }
}

fn render_button(
    prefix: &str,
    surface: &Surface,
    component: &Map<String, Value>,
    scope: Scope<'_>,
    _stack: &mut Vec<String>,
    _cx: &App,
) -> AnyElement {
    let child = component
        .get("child")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let label = surface
        .component(child)
        .and_then(|node| node.get("text"))
        .map(|value| resolve_string(value, scope))
        .filter(|text| !text.is_empty())
        .unwrap_or_else(|| "Button".into());
    let variant = component
        .get("variant")
        .and_then(Value::as_str)
        .unwrap_or("default");
    let url = open_url(component.get("action"), scope);
    let mut button = Button::new(format!("{prefix}-btn")).label(label);
    button = match variant {
        "primary" => button.primary(),
        "borderless" => button.ghost(),
        _ => button,
    };
    if let Some(url) = url {
        button = button.on_click(move |_, _, cx| {
            cx.open_url(&url);
        });
    }
    button.into_any_element()
}

fn open_url(action: Option<&Value>, scope: Scope<'_>) -> Option<String> {
    let object = action.and_then(Value::as_object)?;
    let call = object
        .get("functionCall")
        .and_then(Value::as_object)
        .or(Some(object))?;
    if call.get("call").and_then(Value::as_str)? != "openUrl" {
        return None;
    }
    let url = call.get("args").and_then(|args| args.get("url"))?;
    let text = resolve_string(url, scope);
    if text.is_empty() { None } else { Some(text) }
}

fn render_text_field(component: &Map<String, Value>, scope: Scope<'_>, cx: &App) -> AnyElement {
    let label = component
        .get("label")
        .map(|value| resolve_string(value, scope))
        .unwrap_or_default();
    let value = component
        .get("value")
        .map(|value| resolve_string(value, scope))
        .unwrap_or_default();
    let placeholder = component
        .get("placeholder")
        .map(|value| resolve_string(value, scope))
        .unwrap_or_default();
    let shown = if value.is_empty() { placeholder } else { value };
    v_flex()
        .w_full()
        .gap_1()
        .child(
            div()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(label),
        )
        .child(
            div()
                .w_full()
                .px_3()
                .py_2()
                .rounded(cx.theme().radius)
                .border_1()
                .border_color(cx.theme().border)
                .bg(cx.theme().background)
                .text_sm()
                .child(shown),
        )
        .into_any_element()
}

fn render_checkbox(
    id: &str,
    component: &Map<String, Value>,
    scope: Scope<'_>,
    _cx: &App,
) -> AnyElement {
    let label = component
        .get("label")
        .map(|value| resolve_string(value, scope))
        .unwrap_or_default();
    let checked = component
        .get("value")
        .map(|value| resolve_bool(value, scope))
        .unwrap_or(false);
    Checkbox::new(id.to_string())
        .label(label)
        .checked(checked)
        .disabled(true)
        .into_any_element()
}

fn render_choice(
    prefix: &str,
    component: &Map<String, Value>,
    scope: Scope<'_>,
    cx: &App,
) -> AnyElement {
    let label = component
        .get("label")
        .map(|value| resolve_string(value, scope))
        .unwrap_or_default();
    let selected = component
        .get("value")
        .map(|value| resolve_strings(value, scope))
        .unwrap_or_default();
    let options = component
        .get("options")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    v_flex()
        .w_full()
        .gap_1()
        .child(
            div()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(label),
        )
        .children(options.into_iter().enumerate().map(|(index, option)| {
            let value = option
                .get("value")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            let option_label = option
                .get("label")
                .map(|value| resolve_string(value, scope))
                .unwrap_or_else(|| value.clone());
            Checkbox::new(format!("{prefix}-opt-{index}"))
                .label(option_label)
                .checked(selected.iter().any(|item| item == &value))
                .disabled(true)
        }))
        .into_any_element()
}

fn render_labeled_value(
    kind: &str,
    component: &Map<String, Value>,
    scope: Scope<'_>,
    cx: &App,
) -> AnyElement {
    let label = component
        .get("label")
        .map(|value| resolve_string(value, scope))
        .unwrap_or_else(|| kind.into());
    let value = component
        .get("value")
        .map(|value| resolve_string(value, scope))
        .unwrap_or_default();
    h_flex()
        .gap_2()
        .items_center()
        .child(
            div()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(label),
        )
        .child(div().text_sm().font_weight(FontWeight::MEDIUM).child(value))
        .into_any_element()
}

fn collect_children(
    prefix: &str,
    surface: &Surface,
    component: &Map<String, Value>,
    scope: Scope<'_>,
    stack: &mut Vec<String>,
    cx: &App,
) -> Vec<AnyElement> {
    let children = component.get("children");
    if let Some((path, template_id)) = template_binding(children) {
        return array_at(scope, &path)
            .iter()
            .enumerate()
            .map(|(index, item)| {
                render_id(
                    &format!("{prefix}-t{index}"),
                    surface,
                    &template_id,
                    scope.with_item(item),
                    stack,
                    cx,
                )
            })
            .collect();
    }
    child_ids(children)
        .into_iter()
        .map(|child| render_id(prefix, surface, &child, scope, stack, cx))
        .collect()
}

fn apply_justify<E: gpui::Styled>(element: E, justify: &str) -> E {
    match justify {
        "center" => element.justify_center(),
        "end" => element.justify_end(),
        "spaceBetween" | "spaceEvenly" => element.justify_between(),
        "spaceAround" => element.justify_around(),
        _ => element.justify_start(),
    }
}

fn apply_align<E: gpui::Styled>(element: E, align: &str) -> E {
    match align {
        "center" => element.items_center(),
        "end" => element.items_end(),
        "start" => element.items_start(),
        _ => element.items_stretch(),
    }
}

fn placeholder(cx: &App, message: &str) -> AnyElement {
    div()
        .text_xs()
        .text_color(cx.theme().muted_foreground)
        .child(message.to_string())
        .into_any_element()
}

fn icon_name(name: &str) -> IconName {
    match name {
        "accountCircle" | "person" => IconName::CircleUser,
        "add" => IconName::Plus,
        "arrowBack" => IconName::ChevronLeft,
        "arrowForward" => IconName::ChevronRight,
        "calendarToday" | "event" => IconName::Calendar,
        "check" => IconName::Check,
        "close" => IconName::Close,
        "delete" => IconName::Delete,
        "error" | "warning" => IconName::TriangleAlert,
        "favorite" => IconName::Heart,
        "favoriteOff" => IconName::HeartOff,
        "folder" => IconName::Folder,
        "help" | "info" => IconName::Info,
        "home" => IconName::LayoutDashboard,
        "mail" => IconName::Inbox,
        "menu" => IconName::Menu,
        "moreVert" => IconName::EllipsisVertical,
        "moreHoriz" => IconName::Ellipsis,
        "notifications" => IconName::Bell,
        "pause" => IconName::Pause,
        "play" => IconName::Play,
        "search" => IconName::Search,
        "settings" => IconName::Settings,
        "star" => IconName::Star,
        "starOff" => IconName::StarOff,
        "visibility" => IconName::Eye,
        "visibilityOff" => IconName::EyeOff,
        _ => IconName::Bot,
    }
}
