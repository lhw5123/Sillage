//! Sidebar: project sessions, search, and collapse chrome.

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, ClickEvent, Context, InteractiveElement as _, IntoElement, ParentElement as _,
    SharedString, StatefulInteractiveElement as _, Styled, Window, div, px,
};
use gpui_component::scroll::ScrollableElement as _;
use gpui_component::{
    ActiveTheme as _, Icon, IconName, Sizable as _,
    button::{Button, ButtonVariants as _},
    h_flex, v_flex,
};

use crate::keys::{FocusSearch, HintAction, HintTarget, StartNewTask, ToggleSidebar, hint_badge};
use crate::matching::highlighted;
use crate::projects::format_relative_time;

use super::{Screen, Workspace};

impl Workspace {
    pub(super) fn render_sidebar(
        &self,
        targets: &[HintTarget],
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let typed = self.command.mode.typed_hint().to_string();
        let collapsed = self.sidebar_collapsed;
        let indices = if collapsed {
            Vec::new()
        } else {
            self.filtered_indices(cx)
        };
        let new_task_hint = self.hint_label(HintAction::NewTask, targets);
        let search_hint = self.hint_label(HintAction::Search, targets);
        let empty = indices.is_empty()
            || indices
                .iter()
                .all(|index| self.projects[*index].tasks.is_empty());
        let empty_color = cx.theme().muted_foreground;
        let section_color = cx.theme().muted_foreground;

        v_flex()
            .w(if collapsed { px(48.) } else { px(260.) })
            .h_full()
            .flex_shrink_0()
            .border_r_1()
            .border_color(cx.theme().sidebar_border)
            .bg(cx.theme().sidebar)
            .child(self.render_sidebar_header(targets, &typed, cx))
            .child(
                v_flex()
                    .px_2()
                    .pb_2()
                    .gap(px(1.))
                    .child(self.sidebar_action_row(
                        "new-task",
                        IconName::Plus,
                        "New task",
                        new_task_hint.as_deref(),
                        &typed,
                        false,
                        cx,
                        cx.listener(|this, _, window, cx| {
                            this.start_new_task(&StartNewTask, window, cx);
                        }),
                    ))
                    .child(self.sidebar_action_row(
                        "search",
                        IconName::Search,
                        "Search",
                        search_hint.as_deref(),
                        &typed,
                        self.searching,
                        cx,
                        cx.listener(|this, _, window, cx| {
                            this.focus_search(&FocusSearch, window, cx);
                        }),
                    )),
            )
            .when(!collapsed, |this| {
                this.child(
                    div().px_3().pb_1().child(
                        div()
                            .text_xs()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(section_color)
                            .child("Sessions"),
                    ),
                )
                .child({
                    let mut groups = Vec::new();
                    for index in indices {
                        groups.push(self.render_project_sessions(index, targets, &typed, cx));
                    }
                    div()
                        .id("session-list")
                        .flex_1()
                        .min_h_0()
                        .px_1()
                        .overflow_y_scrollbar()
                        .child(
                            v_flex()
                                .gap_0()
                                .pb_4()
                                .children(groups)
                                .when(empty, |this| {
                                    this.child(
                                        div()
                                            .px_3()
                                            .py_6()
                                            .text_sm()
                                            .text_color(empty_color)
                                            .child("No sessions yet. Start a new task."),
                                    )
                                }),
                        )
                })
            })
    }

    fn render_sidebar_header(
        &self,
        targets: &[HintTarget],
        typed: &str,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let collapsed = self.sidebar_collapsed;
        let hint = self.hint_label(HintAction::ToggleSidebar, targets);
        let icon = if collapsed {
            IconName::PanelLeftOpen
        } else {
            IconName::PanelLeftClose
        };
        let tooltip = if collapsed {
            "Expand sidebar"
        } else {
            "Collapse sidebar"
        };

        h_flex()
            .px_2()
            .pt_2()
            .pb_1()
            .items_center()
            .when(collapsed, |this| this.justify_center())
            .child(
                div()
                    .relative()
                    .child(
                        Button::new("toggle-sidebar")
                            .ghost()
                            .xsmall()
                            .icon(icon)
                            .tooltip(tooltip)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.toggle_sidebar(&ToggleSidebar, window, cx);
                            })),
                    )
                    .when_some(hint, |this, label| {
                        let active = label.starts_with(typed);
                        this.child(hint_badge(&label, active, cx))
                    }),
            )
    }

    fn sidebar_action_row(
        &self,
        id: &'static str,
        icon: IconName,
        label: &'static str,
        hint: Option<&str>,
        typed: &str,
        active: bool,
        cx: &mut Context<Self>,
        on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> impl IntoElement {
        let theme = cx.theme();
        let collapsed = self.sidebar_collapsed;
        div()
            .id(id)
            .relative()
            .px_2()
            .py(px(3.))
            .rounded(theme.radius)
            .cursor_pointer()
            .when(active, |this| this.bg(theme.sidebar_accent))
            .hover(|this| this.bg(theme.sidebar_accent.opacity(0.65)))
            .on_click(on_click)
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .when(collapsed, |this| this.justify_center())
                    .child(
                        Icon::new(icon)
                            .size(px(14.))
                            .text_color(theme.sidebar_foreground.opacity(0.75)),
                    )
                    .when(!collapsed, |this| {
                        this.child(
                            div()
                                .text_sm()
                                .text_color(theme.sidebar_foreground)
                                .child(label),
                        )
                    }),
            )
            .when_some(hint.map(str::to_string), |this, hint| {
                let active = hint.starts_with(typed);
                this.child(hint_badge(&hint, active, cx))
            })
    }

    fn render_project_sessions(
        &self,
        index: usize,
        targets: &[HintTarget],
        typed: &str,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let project = &self.projects[index];
        if project.tasks.is_empty() {
            return div().into_any_element();
        }
        let selected = index == self.selected_project;
        let visible = project.visible_task_count();
        let project_hint = self.hint_label(HintAction::Project { index }, targets);
        let expand_hint = self.hint_label(HintAction::ExpandProject { index }, targets);
        let sidebar_foreground = cx.theme().sidebar_foreground;
        let muted_foreground = cx.theme().muted_foreground;
        let primary = cx.theme().primary;
        let project_name = project.name.clone();
        let expanded = project.expanded;
        let has_more = project.tasks.len() > 3;

        v_flex()
            .gap_0()
            .child(
                div()
                    .id(SharedString::from(format!("project-header-{index}")))
                    .relative()
                    .px_2()
                    .pt_2()
                    .pb(px(2.))
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.select_project(index, cx);
                    }))
                    .child(
                        h_flex()
                            .items_center()
                            .gap_1()
                            .child(
                                div()
                                    .text_xs()
                                    .font_weight(gpui::FontWeight::MEDIUM)
                                    .when(selected, |this| this.text_color(sidebar_foreground))
                                    .when(!selected, |this| this.text_color(muted_foreground))
                                    .child(project_name),
                            )
                            .when(selected, |this| {
                                this.child(div().size(px(4.)).rounded_full().bg(primary))
                            }),
                    )
                    .when_some(project_hint, |this, label| {
                        let active = label.starts_with(typed);
                        this.child(hint_badge(&label, active, cx))
                    }),
            )
            .children(
                (0..visible).map(|task_index| {
                    self.render_session_row(index, task_index, targets, typed, cx)
                }),
            )
            .when(has_more, |this| {
                this.child(
                    div()
                        .id(SharedString::from(format!("expand-{index}")))
                        .relative()
                        .pl_4()
                        .px_2()
                        .py_1()
                        .cursor_pointer()
                        .text_xs()
                        .text_color(muted_foreground)
                        .hover(|row| row.text_color(sidebar_foreground))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if let Some(project) = this.projects.get_mut(index) {
                                project.expanded = !project.expanded;
                            }
                            cx.notify();
                        }))
                        .child(if expanded { "Show less" } else { "Show more" })
                        .when_some(expand_hint, |this, label| {
                            let active = label.starts_with(typed);
                            this.child(hint_badge(&label, active, cx))
                        }),
                )
            })
            .into_any_element()
    }

    fn render_session_row(
        &self,
        project_index: usize,
        task_index: usize,
        targets: &[HintTarget],
        typed: &str,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let project = &self.projects[project_index];
        let task = &project.tasks[task_index];
        let hint = self.hint_label(
            HintAction::Task {
                project: project_index,
                task: task_index,
            },
            targets,
        );
        let active = matches!(
            self.screen,
            Screen::Task {
                project: p,
                task: t
            } if p == project_index && t == task_index
        );
        let theme = cx.theme();
        let time = format_relative_time(task.created_at);
        let folder = project.name.clone();
        let title = task.title.clone();
        let task_id = task.id.clone();
        let query = self.active_query(cx);

        div()
            .relative()
            .mx_1()
            .child(
                v_flex()
                    .id(SharedString::from(task_id))
                    .px_2()
                    .py(px(3.))
                    .gap(px(1.))
                    .rounded(theme.radius)
                    .cursor_pointer()
                    .when(active, |this| this.bg(theme.sidebar_accent))
                    .when(!active, |this| {
                        this.hover(|row| row.bg(theme.sidebar_accent.opacity(0.55)))
                    })
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.selected_project = project_index;
                        this.screen = Screen::Task {
                            project: project_index,
                            task: task_index,
                        };
                        this.sync_preview_workspace(cx);
                        cx.notify();
                    }))
                    .child(
                        div()
                            .w_full()
                            .text_sm()
                            .line_height(px(17.))
                            .text_color(theme.sidebar_foreground)
                            .overflow_hidden()
                            .text_ellipsis()
                            .child(highlighted(title, &query, cx)),
                    )
                    .child(
                        h_flex()
                            .w_full()
                            .gap_1()
                            .items_center()
                            .justify_between()
                            .text_xs()
                            .line_height(px(14.))
                            .text_color(theme.muted_foreground)
                            .child(
                                h_flex()
                                    .flex_1()
                                    .min_w_0()
                                    .gap_1()
                                    .items_center()
                                    .child(
                                        Icon::new(IconName::Folder)
                                            .size(px(10.))
                                            .text_color(theme.muted_foreground.opacity(0.8)),
                                    )
                                    .child(
                                        div()
                                            .min_w_0()
                                            .overflow_hidden()
                                            .text_ellipsis()
                                            .child(folder),
                                    ),
                            )
                            .child(div().flex_shrink_0().child(time)),
                    ),
            )
            .when_some(hint, |this, label| {
                let active_hint = label.starts_with(typed);
                this.child(hint_badge(&label, active_hint, cx))
            })
            .into_any_element()
    }
}
