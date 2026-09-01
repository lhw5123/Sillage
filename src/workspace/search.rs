//! Global search across workspaces, session labels, prompts, and agent replies,
//! and the find-in-page walk over the open session's transcript.

use gpui::prelude::FluentBuilder as _;
use gpui::{
    App, Context, InteractiveElement as _, IntoElement, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled, Window, div, px,
};
use gpui_component::input::Input;
use gpui_component::scroll::ScrollableElement as _;
use gpui_component::{
    ActiveTheme as _, Icon, IconName, Sizable as _,
    button::{Button, ButtonVariants as _},
    h_flex, v_flex,
};

use crate::matching::{contains_ignoring_case, find_ignoring_case, highlighted, match_ranges};
use crate::projects::{Project, Task};

use super::{Screen, Workspace};

/// Results cut off here so a common word cannot flood the panel.
const MAX_RESULTS: usize = 40;

/// Which part of the session produced the hit, shown as a tag on the row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MatchSource {
    Workspace,
    Session,
    Message,
    AgentOutput,
}

impl MatchSource {
    fn label(self) -> &'static str {
        match self {
            Self::Workspace => "Workspace",
            Self::Session => "Session",
            Self::Message => "Your message",
            Self::AgentOutput => "Agent output",
        }
    }
}

#[derive(Clone)]
pub(super) struct SearchResult {
    project: usize,
    task: Option<usize>,
    title: String,
    detail: String,
    source: MatchSource,
}

impl Workspace {
    pub(super) fn search_results(&self, cx: &App) -> Vec<SearchResult> {
        let query = self.search.read(cx).value();
        results_for(&self.projects, &query)
    }

    /// The query every surface highlights against: empty unless the search
    /// panel is open, so closing it clears the highlights.
    pub(super) fn active_query(&self, cx: &App) -> String {
        if !self.searching {
            return String::new();
        }
        self.search.read(cx).value().trim().to_string()
    }

    /// Turn index of every hit in the open session, in reading order, with one
    /// entry per occurrence so the find bar can count and step through them.
    pub(super) fn transcript_matches(&self, cx: &App) -> Vec<usize> {
        let query = self.active_query(cx);
        let Screen::Task { project, task } = self.screen else {
            return Vec::new();
        };
        let Some(task) = self
            .projects
            .get(project)
            .and_then(|project| project.tasks.get(task))
        else {
            return Vec::new();
        };
        transcript_match_turns(task, &query)
    }

    /// Move the find cursor by `step` hits, wrapping at both ends, and bring
    /// the hit's turn into view.
    pub(super) fn step_find(&mut self, step: isize, cx: &mut Context<Self>) {
        if !self.searching {
            return;
        }
        let matches = self.transcript_matches(cx);
        if matches.is_empty() {
            return;
        }
        let total = matches.len() as isize;
        let current = (self.find_index as isize + step).rem_euclid(total);
        self.find_index = current as usize;
        self.transcript_scroll
            .scroll_to_top_of_item(matches[self.find_index]);
        cx.notify();
    }

    /// Enter walks the transcript when the open session has hits, and otherwise
    /// opens the first result so the key always does something useful.
    pub(super) fn advance_find_or_open(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.transcript_matches(cx).is_empty() {
            self.open_first_search_result(window, cx);
        } else {
            self.step_find(1, cx);
        }
    }

    /// Current position and total for the find bar counter, 1-based.
    fn find_counter(&self, cx: &App) -> Option<(usize, usize)> {
        let total = self.transcript_matches(cx).len();
        if total == 0 {
            return None;
        }
        Some((self.find_index.min(total - 1) + 1, total))
    }

    pub(super) fn open_first_search_result(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(result) = self.search_results(cx).into_iter().next() {
            self.open_search_result(result.project, result.task, window, cx);
        }
    }

    fn open_search_result(
        &mut self,
        project: usize,
        task: Option<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if project >= self.projects.len() {
            return;
        }
        self.selected_project = project;
        self.screen = match task {
            Some(task) if task < self.projects[project].tasks.len() => {
                Screen::Task { project, task }
            }
            _ => Screen::NewTask,
        };
        self.sync_preview_workspace(cx);
        self.close_search(window, cx);
    }

    pub(super) fn close_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.searching = false;
        self.search.update(cx, |input, cx| {
            input.set_value("", window, cx);
        });
        window.blur(cx);
        self.focus_handle.focus(window, cx);
        cx.notify();
    }

    pub(super) fn render_global_search(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let query = self.search.read(cx).value().trim().to_string();
        let results = self.search_results(cx);
        let has_query = !query.is_empty();
        let result_count = results.len();
        let counter = self.find_counter(cx);
        let theme = cx.theme();

        v_flex()
            .w_full()
            .max_h(px(360.))
            .border_b_1()
            .border_color(theme.border)
            .bg(theme.background)
            .child(
                h_flex()
                    .px_4()
                    .py_2()
                    .gap_2()
                    .items_center()
                    .child(
                        Icon::new(IconName::Search)
                            .size(px(16.))
                            .text_color(theme.muted_foreground),
                    )
                    .child(
                        div()
                            .flex_1()
                            .child(Input::new(&self.search).appearance(false).cleanable(true)),
                    )
                    .when_some(counter, |this, (position, total)| {
                        this.child(
                            div()
                                .flex_shrink_0()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child(format!("{position}/{total}")),
                        )
                        .child(
                            Button::new("find-previous")
                                .ghost()
                                .xsmall()
                                .icon(IconName::ChevronUp)
                                .tooltip("Previous match")
                                .on_click(cx.listener(|this, _, _, cx| this.step_find(-1, cx))),
                        )
                        .child(
                            Button::new("find-next")
                                .ghost()
                                .xsmall()
                                .icon(IconName::ChevronDown)
                                .tooltip("Next match")
                                .on_click(cx.listener(|this, _, _, cx| this.step_find(1, cx))),
                        )
                    })
                    .child(
                        div()
                            .flex_shrink_0()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child("Esc to close"),
                    ),
            )
            .when(!has_query, |this| {
                this.child(
                    div()
                        .px_4()
                        .pb_3()
                        .text_sm()
                        .text_color(theme.muted_foreground)
                        .child("Search projects, sessions, prompts, and agent output."),
                )
            })
            .when(has_query && results.is_empty(), |this| {
                this.child(
                    div()
                        .px_4()
                        .pb_3()
                        .text_sm()
                        .text_color(theme.muted_foreground)
                        .child("No matches found."),
                )
            })
            .when(!results.is_empty(), |this| {
                this.child(
                    div()
                        .id("global-search-results")
                        .min_h_0()
                        .overflow_y_scrollbar()
                        .child(
                            v_flex()
                                .px_2()
                                .pb_2()
                                .child(
                                    div()
                                        .px_2()
                                        .py_1()
                                        .text_xs()
                                        .text_color(theme.muted_foreground)
                                        .child(format!("{result_count} results")),
                                )
                                .children(results.into_iter().enumerate().map(
                                    |(index, result)| {
                                        let query = query.clone();
                                        let project = result.project;
                                        let task = result.task;
                                        let icon = if task.is_some() {
                                            IconName::SquareTerminal
                                        } else {
                                            IconName::Folder
                                        };
                                        let source = result.source.label();
                                        h_flex()
                                            .id(SharedString::from(format!(
                                                "search-result-{index}"
                                            )))
                                            .w_full()
                                            .px_2()
                                            .py_2()
                                            .gap_2()
                                            .items_center()
                                            .rounded(theme.radius)
                                            .cursor_pointer()
                                            .hover(|row| row.bg(theme.muted.opacity(0.6)))
                                            .on_click(cx.listener(move |this, _, window, cx| {
                                                this.open_search_result(project, task, window, cx);
                                            }))
                                            .child(
                                                Icon::new(icon)
                                                    .size(px(14.))
                                                    .text_color(theme.muted_foreground),
                                            )
                                            .child(
                                                v_flex()
                                                    .min_w_0()
                                                    .child(
                                                        div()
                                                            .text_sm()
                                                            .text_color(theme.foreground)
                                                            .overflow_hidden()
                                                            .text_ellipsis()
                                                            .child(highlighted(
                                                                result.title,
                                                                &query,
                                                                cx,
                                                            )),
                                                    )
                                                    .child(
                                                        h_flex()
                                                            .min_w_0()
                                                            .gap_2()
                                                            .items_center()
                                                            .text_xs()
                                                            .text_color(theme.muted_foreground)
                                                            .child(
                                                                div()
                                                                    .flex_shrink_0()
                                                                    .px_1()
                                                                    .rounded(px(3.))
                                                                    .bg(theme.muted.opacity(0.8))
                                                                    .child(source),
                                                            )
                                                            .child(
                                                                div()
                                                                    .min_w_0()
                                                                    .overflow_hidden()
                                                                    .text_ellipsis()
                                                                    .child(highlighted(
                                                                        result.detail,
                                                                        &query,
                                                                        cx,
                                                                    )),
                                                            ),
                                                    ),
                                            )
                                    },
                                )),
                        ),
                )
            })
    }
}

fn results_for(projects: &[Project], query: &str) -> Vec<SearchResult> {
    let query = query.trim();
    if query.is_empty() {
        return Vec::new();
    }

    let mut results = Vec::new();
    for (project_index, project) in projects.iter().enumerate() {
        let path = project.path.display().to_string();
        if contains_ignoring_case(&project.name, query) || contains_ignoring_case(&path, query) {
            results.push(SearchResult {
                project: project_index,
                task: None,
                title: project.name.clone(),
                detail: path,
                source: MatchSource::Workspace,
            });
        }

        for (task_index, task) in project.tasks.iter().enumerate() {
            if let Some((source, detail)) = task_match(task, project, query) {
                results.push(SearchResult {
                    project: project_index,
                    task: Some(task_index),
                    title: task.title.clone(),
                    detail,
                    source,
                });
            }
        }

        if results.len() >= MAX_RESULTS {
            break;
        }
    }
    results.truncate(MAX_RESULTS);
    results
}

/// Turn index per hit in one transcript, in reading order: each turn's message
/// before its reply.
fn transcript_match_turns(task: &Task, query: &str) -> Vec<usize> {
    if query.is_empty() {
        return Vec::new();
    }
    let mut turns = Vec::new();
    for (index, turn) in task.turns.iter().enumerate() {
        let hits =
            match_ranges(&turn.prompt, query).len() + match_ranges(&turn.output, query).len();
        turns.extend(std::iter::repeat_n(index, hits));
    }
    turns
}

/// First hit inside one session, preferring transcript text over labels so the
/// row can show the sentence the agent actually wrote.
fn task_match(task: &Task, project: &Project, query: &str) -> Option<(MatchSource, String)> {
    for turn in &task.turns {
        if let Some(excerpt) = match_excerpt(&turn.prompt, query) {
            return Some((MatchSource::Message, excerpt));
        }
        if let Some(excerpt) = match_excerpt(&turn.output, query) {
            return Some((MatchSource::AgentOutput, excerpt));
        }
    }
    if contains_ignoring_case(&task.title, query) || contains_ignoring_case(&task.branch, query) {
        return Some((
            MatchSource::Session,
            format!("{} · {}", project.name, task.branch),
        ));
    }
    None
}

/// One-line window of `value` around its first match of `query`, or `None`
/// when `value` does not match.
///
/// The window keeps a little text ahead of the hit for context, and an agent
/// reply of any length stays readable in a single row.
fn match_excerpt(value: &str, query: &str) -> Option<String> {
    const LEAD: usize = 24;
    const WIDTH: usize = 120;

    let compact = value.split_whitespace().collect::<Vec<_>>().join(" ");
    let at = find_ignoring_case(&compact, query)?;
    let chars: Vec<char> = compact.chars().collect();
    let start = at.saturating_sub(LEAD);
    let end = (start + WIDTH).min(chars.len());

    let mut excerpt = String::new();
    if start > 0 {
        excerpt.push('…');
    }
    excerpt.extend(&chars[start..end]);
    if end < chars.len() {
        excerpt.push('…');
    }
    Some(excerpt)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::projects::Turn;
    use std::path::PathBuf;

    fn project_with(tasks: Vec<Task>) -> Project {
        Project {
            id: "id".into(),
            name: "sillage".into(),
            path: PathBuf::from("/tmp/sillage"),
            branch: "main".into(),
            tasks,
            expanded: true,
        }
    }

    fn task_with(title: &str, turns: Vec<Turn>) -> Task {
        let mut task = Task::new("t0".into(), title.into(), String::new(), "main".into());
        task.turns = turns;
        task
    }

    #[test]
    fn agent_output_matches_and_is_reported_as_such() {
        let task = task_with(
            "unrelated title",
            vec![Turn {
                prompt: "check the parser".into(),
                output: "I refactored `LaunchPlan` so cancellation reaches subprocesses.".into(),
            }],
        );
        let results = results_for(&[project_with(vec![task])], "Cancellation");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].source, MatchSource::AgentOutput);
        assert!(
            results[0]
                .detail
                .contains("cancellation reaches subprocesses")
        );
    }

    #[test]
    fn excerpt_keeps_a_deep_match_visible() {
        let long = format!("{} needle tail", "filler ".repeat(200));
        let excerpt = match_excerpt(&long, "needle").expect("long output should match");
        assert!(excerpt.contains("needle"));
        assert!(excerpt.starts_with('…'));
        assert!(!excerpt.contains('\n'));
    }

    #[test]
    fn excerpt_flattens_newlines_from_agent_output() {
        let excerpt = match_excerpt("first line\n\nsecond needle line", "needle")
            .expect("multi-line output should match");
        assert_eq!(excerpt, "first line second needle line");
    }

    #[test]
    fn transcript_walk_lists_one_entry_per_hit_in_reading_order() {
        let task = task_with(
            "title",
            vec![
                Turn {
                    prompt: "needle here".into(),
                    output: "no hit".into(),
                },
                Turn {
                    prompt: "nothing".into(),
                    output: "needle and needle again".into(),
                },
            ],
        );
        assert_eq!(transcript_match_turns(&task, "needle"), vec![0, 1, 1]);
    }

    #[test]
    fn transcript_walk_is_empty_without_a_query() {
        let task = task_with(
            "title",
            vec![Turn {
                prompt: "needle".into(),
                output: String::new(),
            }],
        );
        assert!(transcript_match_turns(&task, "").is_empty());
    }

    #[test]
    fn query_without_a_match_yields_no_results() {
        let task = task_with(
            "title",
            vec![Turn {
                prompt: "a".into(),
                output: "b".into(),
            }],
        );
        assert!(results_for(&[project_with(vec![task])], "zzz").is_empty());
    }
}
