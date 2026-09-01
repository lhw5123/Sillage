//! Main pane: the composer, which stays on screen, and the transcript of the
//! selected session.

use gpui::prelude::FluentBuilder as _;
use gpui::{
    App, Context, Div, InteractiveElement as _, IntoElement, ParentElement as _,
    StatefulInteractiveElement as _, Styled, div, px,
};
use gpui_component::input::Input;
use gpui_component::scroll::Scrollbar;
use gpui_component::spinner::Spinner;
use gpui_component::{
    ActiveTheme as _, Disableable as _, Icon, IconName, Sizable as _,
    button::{Button, ButtonVariants as _},
    h_flex, v_flex,
};

use crate::keys::{CancelTask, HintAction, HintTarget, PickWorkspace, SubmitTask, hint_badge};
use crate::matching::highlighted;
use crate::output;
use crate::projects::format_relative_time;

use super::{Screen, Workspace};

impl Workspace {
    pub(super) fn render_main(
        &self,
        targets: &[HintTarget],
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let typed = self.command.mode.typed_hint().to_string();
        match self.screen {
            Screen::NewTask => self.render_new_task(&typed, targets, cx).into_any_element(),
            Screen::Task { project, task } => self
                .render_task(project, task, &typed, targets, cx)
                .into_any_element(),
        }
    }

    fn hinted(&self, control: impl IntoElement, label: Option<&str>, typed: &str, cx: &App) -> Div {
        div().relative().flex_shrink_0().child(control).when_some(
            label.map(str::to_string),
            |this, label| {
                let active = label.starts_with(typed);
                this.child(hint_badge(&label, active, cx))
            },
        )
    }

    /// Sends the prompt, and turns into a stop button while the agent
    /// generates so the same spot always ends up doing what the user wants.
    fn render_send_button(&self, cx: &Context<Self>) -> Button {
        if self.running() {
            return Button::new("stop")
                .danger()
                .small()
                .rounded(px(999.))
                .icon(IconName::Pause)
                .accessibility_label("Stop")
                .tooltip("Stop generating")
                .on_click(cx.listener(|this, _, window, cx| {
                    this.cancel_task(&CancelTask, window, cx);
                }));
        }
        Button::new("send")
            .primary()
            .small()
            .rounded(px(999.))
            .icon(IconName::ArrowUp)
            .accessibility_label("Send")
            .tooltip("Send")
            .disabled(self.prompt.read(cx).value().trim().is_empty())
            .on_click(cx.listener(|this, _, window, cx| {
                this.submit_task(&SubmitTask, window, cx);
            }))
    }

    /// Prompt box plus its inline controls and the workspace meta row.
    fn render_composer(
        &self,
        typed: &str,
        targets: &[HintTarget],
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let project = self.current_project();
        let agent_hint = self.hint_label(HintAction::CycleAgent, targets);
        let access_hint = self.hint_label(HintAction::ToggleAccess, targets);
        let workspace_hint = self.hint_label(HintAction::PickWorkspace, targets);
        let submit_hint = self.hint_label(HintAction::Submit, targets);
        let theme = cx.theme();
        let workspace_label = project
            .map(|project| project.name.clone())
            .unwrap_or_else(|| "Choose folder".into());
        let workspace_path = project
            .map(|project| project.path.display().to_string())
            .unwrap_or_else(|| "Select a directory to use as the workspace".into());
        let workspace_branch = project.map(|project| project.branch.clone());

        v_flex()
            .w_full()
            .max_w(px(720.))
            .mx_auto()
            .gap_2()
            .child(
                v_flex()
                    .w_full()
                    .rounded(theme.radius_lg)
                    .border_1()
                    .border_color(theme.border)
                    .bg(theme.muted.opacity(0.25))
                    .px_3()
                    .py_3()
                    .gap_2()
                    .child(Input::new(&self.prompt).appearance(false))
                    .child(
                        h_flex()
                            .items_center()
                            .gap_1()
                            .child(
                                self.hinted(
                                    Button::new("agent")
                                        .ghost()
                                        .xsmall()
                                        .icon(IconName::Bot)
                                        .label(self.agent_label())
                                        .tooltip("Switch agent")
                                        .on_click(
                                            cx.listener(|this, _, _, cx| this.cycle_agent(cx)),
                                        ),
                                    agent_hint.as_deref(),
                                    typed,
                                    cx,
                                ),
                            )
                            .child(
                                self.hinted(
                                    Button::new("access")
                                        .ghost()
                                        .xsmall()
                                        .icon(if self.full_access {
                                            IconName::CircleCheck
                                        } else {
                                            IconName::Bell
                                        })
                                        .label(if self.full_access {
                                            "Full access"
                                        } else {
                                            "Ask first"
                                        })
                                        .tooltip("Toggle how much the agent may do on its own")
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.full_access = !this.full_access;
                                            cx.notify();
                                        })),
                                    access_hint.as_deref(),
                                    typed,
                                    cx,
                                ),
                            )
                            .child(div().flex_1())
                            .child(self.hinted(
                                self.render_send_button(cx),
                                submit_hint.as_deref(),
                                typed,
                                cx,
                            )),
                    ),
            )
            .child(
                h_flex()
                    .items_center()
                    .gap_3()
                    .pl_1()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(
                        self.hinted(
                            Button::new("workspace-folder")
                                .ghost()
                                .xsmall()
                                .icon(IconName::Folder)
                                .label(workspace_label)
                                .tooltip(workspace_path)
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.pick_workspace(&PickWorkspace, window, cx);
                                })),
                            workspace_hint.as_deref(),
                            typed,
                            cx,
                        ),
                    )
                    .child(
                        h_flex()
                            .gap_1()
                            .items_center()
                            .child(Icon::new(IconName::SquareTerminal).size(px(12.)))
                            .child("Local"),
                    )
                    .when_some(workspace_branch, |this, branch| {
                        this.child(
                            h_flex()
                                .min_w_0()
                                .gap_1()
                                .items_center()
                                .child(Icon::new(IconName::Network).size(px(12.)))
                                .child(div().overflow_hidden().text_ellipsis().child(branch)),
                        )
                    }),
            )
    }

    fn render_new_task(
        &self,
        typed: &str,
        targets: &[HintTarget],
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let heading = self
            .current_project()
            .map(|project| format!("What should we build in {}?", project.name))
            .unwrap_or_else(|| "What should we build?".into());
        let theme = cx.theme();

        v_flex()
            .flex_1()
            .h_full()
            .min_w_0()
            .child(
                h_flex()
                    .px_5()
                    .py_3()
                    .gap_2()
                    .items_center()
                    .border_b_1()
                    .border_color(theme.border.opacity(0.5))
                    .child(
                        div()
                            .text_sm()
                            .text_color(theme.muted_foreground)
                            .child("New task"),
                    )
                    .child(div().flex_1())
                    .child(self.render_preview_toggle(targets, cx)),
            )
            .child(
                v_flex()
                    .flex_1()
                    .items_center()
                    .justify_center()
                    .gap_4()
                    .child(
                        Icon::new(IconName::Asterisk)
                            .size(px(20.))
                            .text_color(theme.warning),
                    )
                    .child(
                        div()
                            .text_xl()
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .text_color(theme.foreground)
                            .child(heading),
                    ),
            )
            .child(
                div()
                    .px_6()
                    .pb_5()
                    .child(self.render_composer(typed, targets, cx)),
            )
    }

    fn render_task(
        &self,
        project: usize,
        task: usize,
        typed: &str,
        targets: &[HintTarget],
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let Some(item) = self
            .projects
            .get(project)
            .and_then(|project| project.tasks.get(task))
        else {
            return v_flex()
                .flex_1()
                .items_center()
                .justify_center()
                .child("Task not found")
                .into_any_element();
        };

        let theme = cx.theme();
        let query = self.active_query(cx);
        let project_name = self.projects.get(project).map(|p| p.name.clone());
        let last_turn = item.turns.len().saturating_sub(1);
        let transcript: Vec<_> = item
            .turns
            .iter()
            .enumerate()
            .map(|(index, turn)| {
                let pending = turn.output.is_empty() && self.running() && index == last_turn;
                v_flex()
                    .w_full()
                    .gap_3()
                    .child(
                        div()
                            .w_full()
                            .rounded(theme.radius)
                            .border_1()
                            .border_color(theme.border.opacity(0.6))
                            .bg(theme.muted.opacity(0.2))
                            .px_3()
                            .py_2()
                            .text_sm()
                            .text_color(theme.foreground)
                            .child(highlighted(turn.prompt.clone(), &query, cx)),
                    )
                    .child(if pending {
                        h_flex()
                            .gap_2()
                            .items_center()
                            .text_color(theme.muted_foreground)
                            .child(Spinner::new())
                            .child("Agent is working…")
                            .into_any_element()
                    } else {
                        output::render(&turn.output, &query, cx)
                    })
                    .into_any_element()
            })
            .collect();

        v_flex()
            .flex_1()
            .h_full()
            .min_w_0()
            .child(
                v_flex()
                    .px_6()
                    .py_4()
                    .gap_1()
                    .border_b_1()
                    .border_color(theme.border.opacity(0.5))
                    .child(
                        h_flex()
                            .items_center()
                            .gap_2()
                            .child(
                                div()
                                    .text_lg()
                                    .font_weight(gpui::FontWeight::MEDIUM)
                                    .text_color(theme.foreground)
                                    .child(highlighted(item.title.clone(), &query, cx)),
                            )
                            .child(div().flex_1())
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(theme.muted_foreground)
                                    .child(format_relative_time(item.created_at)),
                            )
                            .child(self.render_preview_toggle(targets, cx)),
                    )
                    .when_some(project_name, |this, name| {
                        this.child(
                            h_flex()
                                .gap_1()
                                .items_center()
                                .child(
                                    Icon::new(IconName::Folder)
                                        .size(px(12.))
                                        .text_color(theme.muted_foreground),
                                )
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(theme.muted_foreground)
                                        .child(format!("{name} · {}", item.branch)),
                                ),
                        )
                    }),
            )
            .child(
                // The turns are direct children of the scrolled element so the
                // find bar can bring a hit's turn into view by index, and the
                // scrollbar is a sibling so it stays put while they scroll.
                div()
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .child(
                        v_flex()
                            .id("task-output")
                            .size_full()
                            .p_6()
                            .gap_6()
                            .track_scroll(&self.transcript_scroll)
                            .overflow_y_scroll()
                            .children(transcript),
                    )
                    .child(
                        div().absolute().inset_0().child(
                            Scrollbar::vertical(&self.transcript_scroll)
                                .id("task-output-scrollbar")
                                .viewport_from_layout(),
                        ),
                    ),
            )
            .child(
                div()
                    .px_6()
                    .pb_5()
                    .child(self.render_composer(typed, targets, cx)),
            )
            .into_any_element()
    }
}
