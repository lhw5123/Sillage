//! The desktop shell: one window that lists local projects, composes tasks,
//! and runs a detected coding-agent CLI against the selected workspace.
//!
//! Callers construct it with [`Workspace::new`] and install key bindings via
//! [`init`]. Screen layout, hint overlays, and agent spawning stay inside this
//! module.

mod pane;
mod preview;
mod search;
mod sidebar;

use std::path::PathBuf;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    App, AppContext as _, Context, Entity, FocusHandle, Focusable, InteractiveElement as _,
    IntoElement, KeyDownEvent, ParentElement as _, PathPromptOptions, Render, ScrollHandle,
    SharedString, Styled, Subscription, Window, div, px,
};
use gpui_component::input::{InputEvent, InputState};
use gpui_component::resizable::{h_resizable, resizable_panel};
use gpui_component::{
    ActiveTheme as _, IconName, Root, Sizable as _, TitleBar, WindowExt,
    button::{Button, ButtonVariants as _},
    h_flex, v_flex,
};

use crate::agents::{self, AgentKind, DetectedAgent, RunHandle, RunOutcome};
use crate::keys::{
    self, CancelTask, CommandState, EnterInsert, FindNext, FindPrevious, FocusSearch, HandleEscape,
    HintAction, HintTarget, Mode, PickWorkspace, ShowHints, StartNewTask, SubmitTask,
    TogglePreview, ToggleSidebar,
};
use crate::projects::{self, Project, Task, title_from_prompt};
use preview::PreviewPane;

pub fn init(cx: &mut App) {
    keys::bind_keys(cx);
}

enum Screen {
    NewTask,
    Task { project: usize, task: usize },
}

/// The agent run in flight, and where its reply belongs.
///
/// `id` makes a late reply from a stopped or superseded run easy to discard.
struct ActiveRun {
    id: u64,
    handle: RunHandle,
    project: usize,
    task: usize,
}

/// Session coordinator for one Sillage window.
///
/// Owns which project and screen are visible, the prompt/search inputs, and
/// the in-flight agent run. Sidebar and main-pane layout live in sibling files
/// so each surface hides its own chrome.
pub struct Workspace {
    focus_handle: FocusHandle,
    agents: Vec<DetectedAgent>,
    /// `None` means "Auto": first detected agent.
    agent_kind: Option<AgentKind>,
    projects: Vec<Project>,
    selected_project: usize,
    screen: Screen,
    full_access: bool,
    searching: bool,
    /// Which transcript hit the find bar is parked on, as an index into
    /// [`Self::transcript_matches`].
    find_index: usize,
    transcript_scroll: ScrollHandle,
    sidebar_collapsed: bool,
    /// Whether the right panel is showing. It starts on its chooser so the
    /// surfaces are visible without hunting for them.
    preview_open: bool,
    run: Option<ActiveRun>,
    next_run_id: u64,
    status: SharedString,
    prompt: Entity<InputState>,
    search: Entity<InputState>,
    preview: Entity<PreviewPane>,
    command: CommandState,
    _subscriptions: Vec<Subscription>,
}

impl Workspace {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let prompt = cx.new(|cx| InputState::new(window, cx).placeholder("Do anything..."));
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Search"));
        let _subscriptions = vec![
            cx.subscribe_in(
                &search,
                window,
                |this: &mut Self,
                 _: &Entity<InputState>,
                 event: &InputEvent,
                 window: &mut Window,
                 cx: &mut Context<Self>| match event {
                    // A new query restarts the walk through the transcript.
                    InputEvent::Change => {
                        this.find_index = 0;
                        cx.notify();
                    }
                    InputEvent::PressEnter { .. } => this.advance_find_or_open(window, cx),
                    _ => {}
                },
            ),
            cx.subscribe_in(
                &prompt,
                window,
                |this: &mut Self,
                 _: &Entity<InputState>,
                 event: &InputEvent,
                 window: &mut Window,
                 cx: &mut Context<Self>| match event {
                    InputEvent::Change => cx.notify(),
                    InputEvent::PressEnter { .. } => this.submit_task(&SubmitTask, window, cx),
                    _ => {}
                },
            ),
        ];

        let projects = projects::scan();
        let selected_project = 0;
        let preview = cx.new(|cx| PreviewPane::new(window, cx));
        let preview_path = projects
            .get(selected_project)
            .map(|project| project.path.clone());
        preview.update(cx, |preview, cx| {
            preview.set_workspace(preview_path, cx);
            preview.set_panel_visible(true, cx);
        });
        let agents = agents::detect();
        let status = if agents.is_empty() {
            "No coding agents detected on PATH".into()
        } else {
            let names: Vec<_> = agents
                .iter()
                .map(|agent| agent.kind.display_name())
                .collect();
            format!("Detected {}", names.join(", ")).into()
        };

        Self {
            focus_handle: cx.focus_handle(),
            agents,
            agent_kind: None,
            projects,
            selected_project,
            screen: Screen::NewTask,
            full_access: true,
            searching: false,
            find_index: 0,
            transcript_scroll: ScrollHandle::new(),
            sidebar_collapsed: false,
            preview_open: true,
            run: None,
            next_run_id: 0,
            status,
            prompt,
            search,
            preview,
            command: CommandState::new(),
            _subscriptions,
        }
    }

    fn focus_prompt(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.prompt.focus_handle(cx).focus(window, cx);
    }

    fn focus_search_field(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.search.focus_handle(cx).focus(window, cx);
    }

    fn current_project(&self) -> Option<&Project> {
        self.projects.get(self.selected_project)
    }

    fn sync_preview_workspace(&self, cx: &mut Context<Self>) {
        let path = self.current_project().map(|project| project.path.clone());
        self.preview
            .update(cx, |preview, cx| preview.set_workspace(path, cx));
    }

    fn filtered_indices(&self, cx: &App) -> Vec<usize> {
        let value = self.search.read(cx).value();
        let query = value.trim();
        self.projects
            .iter()
            .enumerate()
            .filter(|(_, project)| project.matches_query(query))
            .map(|(index, _)| index)
            .collect()
    }

    fn resolved_agent(&self) -> Option<&DetectedAgent> {
        match self.agent_kind {
            None => self.agents.first(),
            Some(kind) => self.agents.iter().find(|agent| agent.kind == kind),
        }
    }

    fn agent_label(&self) -> SharedString {
        match self.agent_kind {
            None => "Auto".into(),
            Some(kind) => kind.display_name().into(),
        }
    }

    fn cycle_agent(&mut self, cx: &mut Context<Self>) {
        if self.agents.is_empty() {
            return;
        }
        let next = match self.agent_kind {
            None => Some(self.agents[0].kind),
            Some(kind) => {
                let pos = self
                    .agents
                    .iter()
                    .position(|agent| agent.kind == kind)
                    .unwrap_or(0);
                if pos + 1 >= self.agents.len() {
                    None
                } else {
                    Some(self.agents[pos + 1].kind)
                }
            }
        };
        self.agent_kind = next;
        cx.notify();
    }

    fn select_project(&mut self, index: usize, cx: &mut Context<Self>) {
        if index >= self.projects.len() {
            return;
        }
        self.selected_project = index;
        self.screen = Screen::NewTask;
        self.sync_preview_workspace(cx);
        cx.notify();
    }

    fn start_new_task(&mut self, _: &StartNewTask, window: &mut Window, cx: &mut Context<Self>) {
        self.screen = Screen::NewTask;
        self.command.enter_insert();
        self.focus_prompt(window, cx);
        cx.notify();
    }

    fn focus_search(&mut self, _: &FocusSearch, window: &mut Window, cx: &mut Context<Self>) {
        self.searching = true;
        self.command.enter_insert();
        self.focus_search_field(window, cx);
        cx.notify();
    }

    fn find_next(&mut self, _: &FindNext, _: &mut Window, cx: &mut Context<Self>) {
        self.step_find(1, cx);
    }

    fn find_previous(&mut self, _: &FindPrevious, _: &mut Window, cx: &mut Context<Self>) {
        self.step_find(-1, cx);
    }

    fn toggle_sidebar(&mut self, _: &ToggleSidebar, _: &mut Window, cx: &mut Context<Self>) {
        self.sidebar_collapsed = !self.sidebar_collapsed;
        cx.notify();
    }

    fn toggle_preview(&mut self, _: &TogglePreview, _: &mut Window, cx: &mut Context<Self>) {
        self.preview_open = !self.preview_open;
        let open = self.preview_open;
        self.preview
            .update(cx, |preview, cx| preview.set_panel_visible(open, cx));
        if open {
            self.sync_preview_workspace(cx);
        }
        cx.notify();
    }

    fn pick_workspace(&mut self, _: &PickWorkspace, _: &mut Window, cx: &mut Context<Self>) {
        self.open_workspace_picker(cx);
    }

    fn open_workspace_picker(&mut self, cx: &mut Context<Self>) {
        let receiver = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Choose a workspace folder".into()),
        });
        cx.spawn(async move |this, cx| {
            let paths = match receiver.await {
                Ok(Ok(Some(paths))) => paths,
                Ok(Ok(None)) => return,
                Ok(Err(err)) => {
                    this.update(cx, |this, cx| {
                        this.status = format!("Could not open folder picker: {err}").into();
                        cx.notify();
                    })
                    .ok();
                    return;
                }
                Err(_) => return,
            };
            let Some(path) = paths.into_iter().next() else {
                return;
            };
            this.update(cx, |this, cx| {
                this.apply_workspace_path(path, cx);
            })
            .ok();
        })
        .detach();
    }

    fn apply_workspace_path(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        let Some(project) = Project::from_dir(path) else {
            self.status = "Choose a folder, not a file".into();
            cx.notify();
            return;
        };
        if let Some(index) = self
            .projects
            .iter()
            .position(|existing| existing.path == project.path)
        {
            self.selected_project = index;
        } else {
            self.projects.insert(0, project);
            self.selected_project = 0;
        }
        self.screen = Screen::NewTask;
        if let Some(project) = self.current_project() {
            self.status = format!("Workspace {}", project.name).into();
        }
        self.sync_preview_workspace(cx);
        cx.notify();
    }

    fn enter_insert(&mut self, _: &EnterInsert, window: &mut Window, cx: &mut Context<Self>) {
        self.command.enter_insert();
        self.focus_prompt(window, cx);
        cx.notify();
    }

    fn show_hints(&mut self, _: &ShowHints, window: &mut Window, cx: &mut Context<Self>) {
        self.command.enter_hints();
        self.focus_handle.focus(window, cx);
        cx.notify();
    }

    fn handle_escape(&mut self, _: &HandleEscape, window: &mut Window, cx: &mut Context<Self>) {
        if self.searching {
            self.close_search(window, cx);
            return;
        }
        if self.command.on_escape() {
            if matches!(self.command.mode, Mode::Command) {
                window.blur(cx);
                self.focus_handle.focus(window, cx);
            } else {
                self.focus_prompt(window, cx);
            }
            cx.notify();
        }
    }

    fn hint_targets(&self, cx: &App) -> Vec<HintTarget> {
        if !self.command.mode.is_hint() {
            return Vec::new();
        }
        let mut actions = vec![
            HintAction::NewTask,
            HintAction::Search,
            HintAction::ToggleSidebar,
            HintAction::TogglePreview,
        ];
        if !self.sidebar_collapsed {
            for index in self.filtered_indices(cx) {
                actions.push(HintAction::Project { index });
                let project = &self.projects[index];
                let visible = project.visible_task_count();
                for task in 0..visible {
                    actions.push(HintAction::Task {
                        project: index,
                        task,
                    });
                }
                if project.tasks.len() > 3 {
                    actions.push(HintAction::ExpandProject { index });
                }
            }
        }
        actions.push(HintAction::CycleAgent);
        actions.push(HintAction::ToggleAccess);
        actions.push(HintAction::PickWorkspace);
        actions.push(HintAction::Submit);
        let labels = keys::hint_labels(actions.len());
        actions
            .into_iter()
            .zip(labels)
            .map(|(action, label)| HintTarget { label, action })
            .collect()
    }

    fn hint_label(&self, action: HintAction, targets: &[HintTarget]) -> Option<String> {
        targets
            .iter()
            .find(|target| target.action == action)
            .map(|target| target.label.clone())
    }

    fn apply_hint(&mut self, action: HintAction, window: &mut Window, cx: &mut Context<Self>) {
        match action {
            HintAction::NewTask => self.start_new_task(&StartNewTask, window, cx),
            HintAction::Search => self.focus_search(&FocusSearch, window, cx),
            HintAction::ToggleSidebar => self.toggle_sidebar(&ToggleSidebar, window, cx),
            HintAction::TogglePreview => self.toggle_preview(&TogglePreview, window, cx),
            HintAction::CycleAgent => self.cycle_agent(cx),
            HintAction::ToggleAccess => {
                self.full_access = !self.full_access;
                cx.notify();
            }
            HintAction::PickWorkspace => self.pick_workspace(&PickWorkspace, window, cx),
            HintAction::Submit => {
                if self.running() {
                    self.cancel_task(&CancelTask, window, cx);
                } else {
                    self.submit_task(&SubmitTask, window, cx);
                }
            }
            HintAction::Project { index } => self.select_project(index, cx),
            HintAction::Task { project, task } => {
                self.selected_project = project;
                self.screen = Screen::Task { project, task };
                self.command.enter_insert();
                self.sync_preview_workspace(cx);
                cx.notify();
            }
            HintAction::ExpandProject { index } => {
                if let Some(project) = self.projects.get_mut(index) {
                    project.expanded = !project.expanded;
                }
                cx.notify();
            }
        }
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if !self.command.mode.is_hint() {
            return;
        }
        if event.keystroke.modifiers.modified() {
            return;
        }
        let key = event.keystroke.key.as_str();
        if key.chars().count() != 1 {
            return;
        }
        let ch = key.chars().next().unwrap();
        let targets = self.hint_targets(cx);
        if let Some(action) = self.command.type_hint(ch, &targets) {
            self.apply_hint(action, window, cx);
        }
        cx.notify();
        cx.stop_propagation();
    }

    fn submit_task(&mut self, _: &SubmitTask, window: &mut Window, cx: &mut Context<Self>) {
        if self.running() {
            self.status = "Agent is still working. Stop it to send the next message.".into();
            window.push_notification(self.status.clone(), cx);
            cx.notify();
            return;
        }
        let prompt = self.prompt.read(cx).value().trim().to_string();
        if prompt.is_empty() {
            self.status = "Write a message first".into();
            window.push_notification(self.status.clone(), cx);
            cx.notify();
            return;
        }
        let Some(agent) = self.resolved_agent().cloned() else {
            self.status = "Install Claude Code, Codex CLI, Cursor CLI, or DeepSeek Harness".into();
            window.push_notification(self.status.clone(), cx);
            cx.notify();
            return;
        };
        let Some((project_index, task_index)) = self.route_prompt(prompt) else {
            self.status = "Choose a workspace folder first".into();
            window.push_notification(self.status.clone(), cx);
            cx.notify();
            return;
        };
        let project = &self.projects[project_index];
        let cwd = project.path.clone();
        let project_name = project.name.clone();
        let agent_prompt = project.tasks[task_index].agent_prompt();
        self.selected_project = project_index;
        self.screen = Screen::Task {
            project: project_index,
            task: task_index,
        };
        let handle = RunHandle::new();
        let id = self.next_run_id;
        self.next_run_id += 1;
        self.run = Some(ActiveRun {
            id,
            handle: handle.clone(),
            project: project_index,
            task: task_index,
        });
        self.status = format!("Running {} in {project_name}", agent.kind.display_name()).into();
        let full_access = self.full_access;

        cx.spawn(async move |this, cx| {
            let run = async move { agent.run(&agent_prompt, &cwd, full_access, &handle) };
            let outcome = cx.background_spawn(run).await;
            this.update(cx, |this, cx| this.finish_run(id, outcome, cx))
                .ok();
        })
        .detach();
        self.prompt.update(cx, |input, cx| {
            input.set_value("", window, cx);
        });
        self.focus_prompt(window, cx);
        cx.notify();
    }

    fn running(&self) -> bool {
        self.run.is_some()
    }

    /// Stop the agent mid-generation and leave a note in the pending turn.
    fn cancel_task(&mut self, _: &CancelTask, _: &mut Window, cx: &mut Context<Self>) {
        let Some(run) = self.run.take() else {
            return;
        };
        run.handle.cancel();
        if let Some(turn) = self
            .projects
            .get_mut(run.project)
            .and_then(|project| project.tasks.get_mut(run.task))
            .and_then(|task| task.last_turn_mut())
            && turn.output.is_empty()
        {
            turn.output = "Stopped by you.".into();
        }
        self.status = "Stopped the agent".into();
        cx.notify();
    }

    /// Record a finished run, ignoring replies from a run already stopped or
    /// replaced.
    fn finish_run(&mut self, id: u64, outcome: RunOutcome, cx: &mut Context<Self>) {
        let Some(run) = self.run.take_if(|run| run.id == id) else {
            return;
        };
        let title = self
            .projects
            .get(run.project)
            .and_then(|project| project.tasks.get(run.task))
            .map(|task| task.title.clone())
            .unwrap_or_default();
        let (output, status) = match outcome {
            RunOutcome::Done(text) => (text, format!("Finished {title}")),
            RunOutcome::Failed(text) => (text, format!("Failed {title}")),
            RunOutcome::Stopped => ("Stopped by you.".to_string(), format!("Stopped {title}")),
        };
        if let Some(turn) = self
            .projects
            .get_mut(run.project)
            .and_then(|project| project.tasks.get_mut(run.task))
            .and_then(|task| task.last_turn_mut())
        {
            turn.output = output;
        }
        self.status = status.into();
        self.preview.update(cx, |preview, cx| preview.refresh(cx));
        cx.notify();
    }

    /// Append the prompt to the open session, or open a new one, and return
    /// where it landed.
    fn route_prompt(&mut self, prompt: String) -> Option<(usize, usize)> {
        if let Screen::Task { project, task } = self.screen
            && let Some(open) = self
                .projects
                .get_mut(project)
                .and_then(|project| project.tasks.get_mut(task))
        {
            open.push_turn(prompt);
            return Some((project, task));
        }

        let project = self.projects.get_mut(self.selected_project)?;
        let task = Task::new(
            format!("{}-{}", project.id, project.tasks.len()),
            title_from_prompt(&prompt),
            prompt,
            project.branch.clone(),
        );
        project.tasks.insert(0, task);
        Some((self.selected_project, 0))
    }

    /// Control at the top right of the main pane that opens and closes the
    /// right panel.
    pub(super) fn render_preview_toggle(
        &self,
        targets: &[HintTarget],
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let typed = self.command.mode.typed_hint().to_string();
        let hint = self.hint_label(HintAction::TogglePreview, targets);
        let icon = if self.preview_open {
            IconName::PanelRightClose
        } else {
            IconName::PanelRightOpen
        };
        let tooltip = if self.preview_open {
            "Close the right panel"
        } else {
            "Open the right panel"
        };

        div()
            .relative()
            .flex_shrink_0()
            .child(
                Button::new("toggle-preview")
                    .ghost()
                    .xsmall()
                    .icon(icon)
                    .accessibility_label("Toggle the right panel")
                    .tooltip(tooltip)
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.toggle_preview(&TogglePreview, window, cx);
                    })),
            )
            .when_some(hint, |this, label| {
                let active = label.starts_with(&typed);
                this.child(keys::hint_badge(&label, active, cx))
            })
    }

    fn render_status_bar(&self, cx: &App) -> impl IntoElement {
        let project = self.current_project();
        h_flex()
            .h(px(28.))
            .px_3()
            .gap_3()
            .border_t_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().background)
            .text_xs()
            .text_color(cx.theme().muted_foreground)
            .child(
                div()
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .text_color(cx.theme().accent_foreground)
                    .child(self.command.mode.status_label()),
            )
            .child("·")
            .child(
                project
                    .map(|project| project.name.clone())
                    .unwrap_or_else(|| "No project".into()),
            )
            .child("·")
            .child("Local")
            .child("·")
            .child(
                project
                    .map(|project| project.branch.clone())
                    .unwrap_or_default(),
            )
            .child(div().flex_1())
            .child(self.status.clone())
    }
}

impl Focusable for Workspace {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let targets = self.hint_targets(cx);
        let key_context = keys::key_context(&self.command.mode);

        v_flex()
            .id("workspace")
            .key_context(key_context)
            .track_focus(&self.focus_handle)
            .size_full()
            .bg(cx.theme().background)
            .on_action(cx.listener(Self::handle_escape))
            .on_action(cx.listener(Self::enter_insert))
            .on_action(cx.listener(Self::show_hints))
            .on_action(cx.listener(Self::submit_task))
            .on_action(cx.listener(Self::cancel_task))
            .on_action(cx.listener(Self::start_new_task))
            .on_action(cx.listener(Self::focus_search))
            .on_action(cx.listener(Self::find_next))
            .on_action(cx.listener(Self::find_previous))
            .on_action(cx.listener(Self::toggle_sidebar))
            .on_action(cx.listener(Self::toggle_preview))
            .on_action(cx.listener(Self::pick_workspace))
            .on_key_down(cx.listener(Self::on_key_down))
            .child(TitleBar::new().child("Sillage"))
            .when(self.searching, |this| {
                this.child(self.render_global_search(cx))
            })
            .child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .child(self.render_sidebar(&targets, cx))
                    .child(
                        div().flex_1().min_w_0().h_full().child(
                            h_resizable("chat-preview")
                                .child(
                                    resizable_panel()
                                        .size_range(px(320.)..gpui::Pixels::MAX)
                                        .child(self.render_main(&targets, cx)),
                                )
                                .child(
                                    resizable_panel()
                                        .visible(self.preview_open)
                                        .flex_none()
                                        .size(px(460.))
                                        .size_range(px(300.)..px(760.))
                                        .child(self.preview.clone()),
                                ),
                        ),
                    ),
            )
            .child(self.render_status_bar(cx))
            .children(Root::render_notification_layer(window, cx))
    }
}
