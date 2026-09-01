//! Resizable workspace preview for web pages, shell output, files, and changes.
//!
//! The panel opens on a chooser, so what it shows is always a surface the user
//! picked. Every surface reads from the workspace selected in the sidebar.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, AppContext as _, Context, Entity, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, SharedString, StatefulInteractiveElement as _, Styled,
    Subscription, Window, div, px,
};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::scroll::ScrollableElement as _;
use gpui_component::{
    ActiveTheme as _, Icon, IconName, Sizable as _,
    button::{Button, ButtonVariants as _},
    h_flex, v_flex,
};
use gpui_wry::WebView;
use raw_window_handle::HasWindowHandle as _;

const MAX_FILES: usize = 1_000;
const MAX_PREVIEW_BYTES: u64 = 1_000_000;
/// A long diff stops here so one large change cannot stall the panel.
const MAX_DIFF_LINES: usize = 1_500;

/// One thing the panel can show.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Surface {
    Browser,
    Terminal,
    Files,
    Review,
}

/// Chooser order, which is also the order the 2x2 grid paints.
const SURFACES: [Surface; 4] = [
    Surface::Browser,
    Surface::Terminal,
    Surface::Files,
    Surface::Review,
];

impl Surface {
    fn id(self) -> &'static str {
        match self {
            Self::Browser => "surface-browser",
            Self::Terminal => "surface-terminal",
            Self::Files => "surface-files",
            Self::Review => "surface-review",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Browser => "Browser",
            Self::Terminal => "Terminal",
            Self::Files => "Files",
            Self::Review => "Review",
        }
    }

    fn description(self) -> &'static str {
        match self {
            Self::Browser => "Open a local app or URL",
            Self::Terminal => "Start a shell in this workspace",
            Self::Files => "Browse and read workspace files",
            Self::Review => "Review file changes",
        }
    }

    fn icon(self) -> IconName {
        match self {
            Self::Browser => IconName::Globe,
            Self::Terminal => IconName::SquareTerminal,
            Self::Files => IconName::Folder,
            Self::Review => IconName::File,
        }
    }
}

/// How one diff line is painted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DiffLine {
    Added,
    Removed,
    Hunk,
    Meta,
    Context,
}

#[derive(Debug, Clone)]
struct WorkspaceFile {
    absolute: PathBuf,
    relative: String,
    artifact: bool,
}

pub(super) struct PreviewPane {
    workspace: Option<PathBuf>,
    /// `None` shows the chooser rather than a surface.
    surface: Option<Surface>,
    /// Whether the workspace is showing the panel at all; a hidden panel also
    /// hides the browser, which is a native view painted over the window.
    panel_visible: bool,
    files: Vec<WorkspaceFile>,
    selected_file: Option<PathBuf>,
    file_content: SharedString,
    review: SharedString,
    terminal_input: Entity<InputState>,
    terminal_output: SharedString,
    terminal_running: bool,
    address_input: Entity<InputState>,
    webview: Entity<WebView>,
    _subscriptions: Vec<Subscription>,
}

impl PreviewPane {
    pub(super) fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let terminal_input = cx.new(|cx| InputState::new(window, cx).placeholder("Run a command…"));
        let address_input = cx.new(|cx| {
            InputState::new(window, cx)
                .default_value("http://localhost:3000")
                .placeholder("Enter a URL…")
        });
        let webview = cx.new(|cx| {
            let handle = window
                .window_handle()
                .expect("window handle is unavailable");
            let view = wry::WebViewBuilder::new()
                .with_url("about:blank")
                .build_as_child(&handle)
                .expect("failed to create browser preview");
            WebView::new(view, window, cx)
        });
        webview.update(cx, |webview, _| webview.hide());

        let _subscriptions = vec![
            cx.subscribe_in(&terminal_input, window, |this, input, event, window, cx| {
                if matches!(event, InputEvent::PressEnter { .. }) {
                    let command = input.read(cx).value().trim().to_string();
                    if !command.is_empty() {
                        this.run_command(command, window, cx);
                    }
                }
            }),
            cx.subscribe_in(&address_input, window, |this, input, event, _, cx| {
                if matches!(event, InputEvent::PressEnter { .. }) {
                    let address = normalize_url(input.read(cx).value().trim());
                    this.load_url(&address, cx);
                }
            }),
        ];

        Self {
            workspace: None,
            surface: None,
            panel_visible: false,
            files: Vec::new(),
            selected_file: None,
            file_content: "Choose a file from the workspace.".into(),
            review: "No changes since the last commit.".into(),
            terminal_input,
            terminal_output: "Commands run in the selected workspace.\n".into(),
            terminal_running: false,
            address_input,
            webview,
            _subscriptions,
        }
    }

    pub(super) fn set_workspace(&mut self, path: Option<PathBuf>, cx: &mut Context<Self>) {
        if self.workspace == path {
            self.refresh(cx);
            return;
        }
        self.workspace = path;
        self.selected_file = None;
        self.file_content = "Choose a file from the workspace.".into();
        self.terminal_output = "Commands run in the selected workspace.\n".into();
        self.refresh(cx);
    }

    /// Follow the workspace panel's own visibility so the native browser view
    /// never paints over a closed panel.
    pub(super) fn set_panel_visible(&mut self, visible: bool, cx: &mut Context<Self>) {
        self.panel_visible = visible;
        self.sync_webview(cx);
        cx.notify();
    }

    pub(super) fn refresh(&mut self, cx: &mut Context<Self>) {
        self.files = self
            .workspace
            .as_deref()
            .map(workspace_files)
            .unwrap_or_default();
        self.review = self
            .workspace
            .as_deref()
            .map(review_text)
            .unwrap_or_else(|| "Choose a workspace to review changes.".to_string())
            .into();
        if let Some(path) = self.selected_file.clone() {
            self.read_file(path);
        }
        cx.notify();
    }

    fn open_surface(&mut self, surface: Surface, cx: &mut Context<Self>) {
        self.surface = Some(surface);
        self.sync_webview(cx);
        cx.notify();
    }

    fn show_chooser(&mut self, cx: &mut Context<Self>) {
        self.surface = None;
        self.sync_webview(cx);
        cx.notify();
    }

    fn sync_webview(&self, cx: &mut Context<Self>) {
        let visible = self.panel_visible && self.surface == Some(Surface::Browser);
        self.webview.update(cx, |webview, _| {
            if visible {
                webview.show();
            } else {
                webview.hide();
            }
        });
    }

    fn read_file(&mut self, path: PathBuf) {
        self.selected_file = Some(path.clone());
        self.file_content = read_preview(&path).into();
    }

    fn open_file(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        self.read_file(path);
        self.open_surface(Surface::Files, cx);
    }

    fn preview_in_browser(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        let url = format!("file://{}", path.to_string_lossy());
        self.load_url(&url, cx);
        self.address_input.update(cx, |input, cx| {
            input.set_value(url, window, cx);
        });
        self.open_surface(Surface::Browser, cx);
    }

    fn load_url(&mut self, url: &str, cx: &mut Context<Self>) {
        if url.is_empty() {
            return;
        }
        self.webview.update(cx, |view, _| view.load_url(url));
    }

    fn run_command(&mut self, command: String, window: &mut Window, cx: &mut Context<Self>) {
        if self.terminal_running {
            return;
        }
        let Some(cwd) = self.workspace.clone() else {
            self.terminal_output = "Choose a workspace before running commands.\n".into();
            cx.notify();
            return;
        };
        self.terminal_running = true;
        let mut output = self.terminal_output.to_string();
        output.push_str(&format!("\n$ {command}\n"));
        self.terminal_output = output.into();
        self.terminal_input.update(cx, |input, cx| {
            input.set_value("", window, cx);
        });

        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    Command::new(std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".to_string()))
                        .args(["-lc", &command])
                        .current_dir(cwd)
                        .output()
                })
                .await;
            this.update(cx, |this, cx| {
                let mut text = this.terminal_output.to_string();
                match result {
                    Ok(result) => {
                        text.push_str(&String::from_utf8_lossy(&result.stdout));
                        text.push_str(&String::from_utf8_lossy(&result.stderr));
                        if !result.status.success() {
                            text.push_str(&format!("\n[exit: {}]\n", result.status));
                        }
                    }
                    Err(error) => text.push_str(&format!("Failed to run command: {error}\n")),
                }
                this.terminal_output = text.into();
                this.terminal_running = false;
                this.refresh(cx);
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    /// The 2x2 grid the panel opens on.
    fn render_chooser(&self, cx: &Context<Self>) -> impl IntoElement {
        let muted_foreground = cx.theme().muted_foreground;
        let rows: Vec<_> = SURFACES
            .chunks(2)
            .map(|pair| {
                h_flex().gap_2().children(
                    pair.iter()
                        .map(|surface| self.render_surface_card(*surface, cx)),
                )
            })
            .collect();

        v_flex()
            .size_full()
            .items_center()
            .justify_center()
            .gap_2()
            .child(div().text_sm().child("Open a surface"))
            .child(
                div()
                    .pb_2()
                    .text_xs()
                    .text_color(muted_foreground)
                    .child("Choose what to show in the right panel"),
            )
            .children(rows)
    }

    fn render_surface_card(&self, surface: Surface, cx: &Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let radius = theme.radius_lg;
        let border = theme.border;
        let muted = theme.muted;
        let muted_foreground = theme.muted_foreground;

        div()
            .id(surface.id())
            .w(px(146.))
            .p_3()
            .rounded(radius)
            .border_1()
            .border_color(border)
            .bg(muted.opacity(0.2))
            .cursor_pointer()
            .hover(move |card| card.bg(muted.opacity(0.45)))
            .on_click(cx.listener(move |this, _, _, cx| this.open_surface(surface, cx)))
            .child(
                v_flex()
                    .gap_2()
                    .child(
                        Icon::new(surface.icon())
                            .size(px(14.))
                            .text_color(muted_foreground),
                    )
                    .child(div().text_sm().child(surface.label()))
                    .child(
                        div()
                            .text_xs()
                            .text_color(muted_foreground)
                            .child(surface.description()),
                    ),
            )
    }

    fn render_files_surface(&self, cx: &Context<Self>) -> impl IntoElement {
        let artifacts: Vec<_> = self
            .files
            .iter()
            .filter(|file| file.artifact)
            .cloned()
            .collect();
        let files = self.files.clone();
        let selected = self.selected_file.clone();
        let title = selected
            .as_ref()
            .and_then(|path| {
                self.workspace
                    .as_ref()
                    .and_then(|root| path.strip_prefix(root).ok())
            })
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| "File preview".to_string());
        let can_browse = selected
            .as_ref()
            .and_then(|path| path.extension())
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| matches!(extension, "html" | "htm" | "svg"));
        let has_artifacts = !artifacts.is_empty();
        let artifact_group = self.render_file_group("AGENT ARTIFACTS", artifacts, cx);
        let file_group = self.render_file_group("FILES", files, cx);
        let border = cx.theme().border;
        let muted_foreground = cx.theme().muted_foreground;
        let mono_font = cx.theme().mono_font_family.clone();

        h_flex()
            .size_full()
            .min_w_0()
            .child(
                v_flex()
                    .w(px(190.))
                    .h_full()
                    .flex_shrink_0()
                    .border_r_1()
                    .border_color(border)
                    .child(
                        h_flex()
                            .h(px(32.))
                            .px_2()
                            .items_center()
                            .text_xs()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(muted_foreground)
                            .child("WORKSPACE"),
                    )
                    .child(
                        div()
                            .id("preview-file-list")
                            .flex_1()
                            .min_h_0()
                            .overflow_y_scrollbar()
                            .child(
                                v_flex()
                                    .pb_3()
                                    .when(has_artifacts, |this| this.child(artifact_group))
                                    .child(file_group),
                            ),
                    ),
            )
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .child(
                        h_flex()
                            .h(px(32.))
                            .px_3()
                            .gap_2()
                            .items_center()
                            .border_b_1()
                            .border_color(border)
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .text_xs()
                                    .overflow_hidden()
                                    .text_ellipsis()
                                    .child(title),
                            )
                            .when(can_browse, |this| {
                                let path = selected.clone().unwrap();
                                this.child(
                                    Button::new("preview-file-browser")
                                        .ghost()
                                        .xsmall()
                                        .icon(IconName::Globe)
                                        .tooltip("Open in Browser")
                                        .on_click(cx.listener(move |this, _, window, cx| {
                                            this.preview_in_browser(path.clone(), window, cx);
                                        })),
                                )
                            }),
                    )
                    .child(
                        div()
                            .id("file-preview-content")
                            .flex_1()
                            .min_h_0()
                            .p_3()
                            .overflow_scroll()
                            .font_family(mono_font)
                            .text_xs()
                            .line_height(px(19.))
                            .child(self.file_content.clone()),
                    ),
            )
    }

    fn render_file_group(
        &self,
        label: &'static str,
        files: Vec<WorkspaceFile>,
        cx: &Context<Self>,
    ) -> AnyElement {
        let muted = cx.theme().muted_foreground;
        let hover = cx.theme().muted;
        v_flex()
            .child(
                div()
                    .px_2()
                    .pt_2()
                    .pb_1()
                    .text_xs()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(muted)
                    .child(label),
            )
            .children(files.into_iter().map(|file| {
                let path = file.absolute;
                h_flex()
                    .id(SharedString::from(format!("preview-{}", file.relative)))
                    .px_2()
                    .py(px(3.))
                    .gap_1()
                    .items_center()
                    .cursor_pointer()
                    .hover(move |row| row.bg(hover.opacity(0.65)))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.open_file(path.clone(), cx);
                    }))
                    .child(Icon::new(IconName::File).size(px(12.)).text_color(muted))
                    .child(
                        div()
                            .min_w_0()
                            .text_xs()
                            .overflow_hidden()
                            .text_ellipsis()
                            .child(file.relative),
                    )
            }))
            .into_any_element()
    }

    fn render_review_surface(&self, cx: &Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let added = theme.green;
        let removed = theme.red;
        let hunk = theme.blue;
        let meta = theme.muted_foreground;
        let context = theme.foreground.opacity(0.85);
        let mono_font = theme.mono_font_family.clone();

        let lines: Vec<_> = self
            .review
            .lines()
            .map(|line| {
                let color = match classify_diff_line(line) {
                    DiffLine::Added => added,
                    DiffLine::Removed => removed,
                    DiffLine::Hunk => hunk,
                    DiffLine::Meta => meta,
                    DiffLine::Context => context,
                };
                let text = if line.is_empty() { " " } else { line };
                div()
                    .w_full()
                    .text_color(color)
                    .child(text.to_string())
                    .into_any_element()
            })
            .collect();

        div()
            .id("preview-review")
            .size_full()
            .p_3()
            .overflow_scroll()
            .font_family(mono_font)
            .text_xs()
            .line_height(px(18.))
            .child(v_flex().w_full().children(lines))
    }

    fn render_terminal_surface(&self, cx: &Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        v_flex()
            .size_full()
            .bg(theme.background)
            .child(
                div()
                    .id("preview-terminal-output")
                    .flex_1()
                    .min_h_0()
                    .p_3()
                    .overflow_y_scrollbar()
                    .font_family(theme.mono_font_family.clone())
                    .text_xs()
                    .line_height(px(19.))
                    .child(self.terminal_output.clone()),
            )
            .child(
                h_flex()
                    .border_t_1()
                    .border_color(theme.border)
                    .px_2()
                    .py_1()
                    .gap_2()
                    .items_center()
                    .child(div().text_color(theme.primary).child("$"))
                    .child(
                        div()
                            .flex_1()
                            .child(Input::new(&self.terminal_input).appearance(false)),
                    )
                    .when(self.terminal_running, |this| {
                        this.child(
                            div()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child("Running…"),
                        )
                    }),
            )
    }

    fn render_browser_surface(&self, cx: &Context<Self>) -> impl IntoElement {
        v_flex()
            .size_full()
            .child(
                h_flex()
                    .px_2()
                    .py_1()
                    .gap_2()
                    .items_center()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(Icon::new(IconName::Globe).size(px(14.)))
                    .child(div().flex_1().child(Input::new(&self.address_input))),
            )
            .child(div().flex_1().min_h_0().child(self.webview.clone()))
    }

    /// Surface title, the way back to the chooser, and a refresh for the
    /// surfaces that read from disk.
    fn render_header(&self, surface: Surface, cx: &Context<Self>) -> impl IntoElement {
        let border = cx.theme().border;
        let reads_disk = matches!(surface, Surface::Files | Surface::Review);

        h_flex()
            .h(px(38.))
            .px_2()
            .gap_2()
            .items_center()
            .border_b_1()
            .border_color(border)
            .child(
                Button::new("preview-surfaces")
                    .ghost()
                    .xsmall()
                    .icon(IconName::ChevronLeft)
                    .tooltip("All surfaces")
                    .on_click(cx.listener(|this, _, _, cx| this.show_chooser(cx))),
            )
            .child(div().text_sm().child(surface.label()))
            .child(div().flex_1())
            .when(reads_disk, |this| {
                this.child(
                    Button::new("preview-refresh")
                        .ghost()
                        .xsmall()
                        .label("Refresh")
                        .tooltip("Read the workspace again")
                        .on_click(cx.listener(|this, _, _, cx| this.refresh(cx))),
                )
            })
    }
}

impl Render for PreviewPane {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let border = theme.border;
        let background = theme.background;

        v_flex()
            .size_full()
            .min_w_0()
            .border_l_1()
            .border_color(border)
            .bg(background)
            .when_some(self.surface, |this, surface| {
                this.child(self.render_header(surface, cx))
                    .child(div().flex_1().min_h_0().child(match surface {
                        Surface::Browser => self.render_browser_surface(cx).into_any_element(),
                        Surface::Terminal => self.render_terminal_surface(cx).into_any_element(),
                        Surface::Files => self.render_files_surface(cx).into_any_element(),
                        Surface::Review => self.render_review_surface(cx).into_any_element(),
                    }))
            })
            .when(self.surface.is_none(), |this| {
                this.child(div().flex_1().min_h_0().child(self.render_chooser(cx)))
            })
    }
}

fn normalize_url(value: &str) -> String {
    if value.is_empty() || value.contains("://") || value.starts_with("file:") {
        value.to_string()
    } else {
        format!("http://{value}")
    }
}

fn classify_diff_line(line: &str) -> DiffLine {
    if line.starts_with("+++") || line.starts_with("---") || line.starts_with("diff --git") {
        DiffLine::Meta
    } else if line.starts_with("index ") || line.starts_with("new file") {
        DiffLine::Meta
    } else if line.starts_with("@@") {
        DiffLine::Hunk
    } else if line.starts_with('+') {
        DiffLine::Added
    } else if line.starts_with('-') {
        DiffLine::Removed
    } else {
        DiffLine::Context
    }
}

fn workspace_files(root: &Path) -> Vec<WorkspaceFile> {
    let artifacts = changed_paths(root);
    let mut paths = Vec::new();
    collect_files(root, root, &mut paths);
    paths.sort_by(|a, b| a.relative.cmp(&b.relative));
    paths
        .into_iter()
        .map(|mut file| {
            file.artifact = artifacts.contains(&file.relative);
            file
        })
        .collect()
}

fn collect_files(root: &Path, directory: &Path, files: &mut Vec<WorkspaceFile>) {
    if files.len() >= MAX_FILES {
        return;
    }
    let Ok(entries) = fs::read_dir(directory) else {
        return;
    };
    let mut entries: Vec<_> = entries.flatten().collect();
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        if files.len() >= MAX_FILES {
            break;
        }
        let path = entry.path();
        if path.is_dir() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if matches!(name.as_ref(), ".git" | "node_modules" | "target" | ".next") {
                continue;
            }
            collect_files(root, &path, files);
        } else if path.is_file() {
            let relative = path
                .strip_prefix(root)
                .unwrap_or(&path)
                .to_string_lossy()
                .into_owned();
            files.push(WorkspaceFile {
                absolute: path,
                relative,
                artifact: false,
            });
        }
    }
}

fn changed_paths(root: &Path) -> HashSet<String> {
    let Ok(output) = Command::new("git")
        .args(["status", "--porcelain=v1", "-z", "--untracked-files=all"])
        .current_dir(root)
        .output()
    else {
        return HashSet::new();
    };
    if !output.status.success() {
        return HashSet::new();
    }
    parse_changed_paths(&output.stdout)
}

fn parse_changed_paths(output: &[u8]) -> HashSet<String> {
    let entries: Vec<_> = output.split(|byte| *byte == 0).collect();
    let mut paths = HashSet::new();
    let mut index = 0;
    while index < entries.len() {
        let entry = entries[index];
        if entry.len() < 4 {
            index += 1;
            continue;
        }
        let status = &entry[..2];
        let path = String::from_utf8_lossy(&entry[3..]).into_owned();
        paths.insert(path);
        index += if status.contains(&b'R') || status.contains(&b'C') {
            2
        } else {
            1
        };
    }
    paths
}

/// Tracked changes as a diff, with untracked files listed after them since a
/// diff against HEAD cannot show a file Git has never seen.
fn review_text(root: &Path) -> String {
    let Ok(diff) = Command::new("git")
        .args(["--no-pager", "diff", "--no-color", "HEAD"])
        .current_dir(root)
        .output()
    else {
        return "Git is unavailable on this machine.".to_string();
    };
    if !diff.status.success() {
        return "This folder has no Git history to review.".to_string();
    }

    let mut text = String::from_utf8_lossy(&diff.stdout).into_owned();
    let untracked = untracked_files(root);
    if !untracked.is_empty() {
        if !text.is_empty() && !text.ends_with('\n') {
            text.push('\n');
        }
        text.push_str("\nUntracked files:\n");
        for path in untracked {
            text.push_str(&format!("+ {path}\n"));
        }
    }
    if text.trim().is_empty() {
        return "No changes since the last commit.".to_string();
    }
    truncate_diff(&text)
}

fn untracked_files(root: &Path) -> Vec<String> {
    let Ok(output) = Command::new("git")
        .args(["ls-files", "--others", "--exclude-standard", "-z"])
        .current_dir(root)
        .output()
    else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }
    output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|entry| !entry.is_empty())
        .map(|entry| String::from_utf8_lossy(entry).into_owned())
        .collect()
}

fn truncate_diff(text: &str) -> String {
    let mut out: String = text
        .lines()
        .take(MAX_DIFF_LINES)
        .collect::<Vec<_>>()
        .join("\n");
    if text.lines().count() > MAX_DIFF_LINES {
        out.push_str(&format!("\n… diff cut off at {MAX_DIFF_LINES} lines"));
    }
    out
}

fn read_preview(path: &Path) -> String {
    let Ok(metadata) = fs::metadata(path) else {
        return "The file is unavailable.".to_string();
    };
    if metadata.len() > MAX_PREVIEW_BYTES {
        return format!(
            "Preview limited to files smaller than {} MB.",
            MAX_PREVIEW_BYTES / 1_000_000
        );
    }
    let Ok(bytes) = fs::read(path) else {
        return "The file could not be read.".to_string();
    };
    if bytes.iter().take(8_192).any(|byte| *byte == 0) {
        return format!("Binary file · {} bytes", bytes.len());
    }
    String::from_utf8(bytes).unwrap_or_else(|error| {
        format!(
            "Text preview requires UTF-8. This file contains {} bytes.",
            error.as_bytes().len()
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls_gain_a_default_scheme() {
        assert_eq!(normalize_url("localhost:5173"), "http://localhost:5173");
        assert_eq!(normalize_url("https://example.com"), "https://example.com");
        assert_eq!(
            normalize_url("file:///tmp/index.html"),
            "file:///tmp/index.html"
        );
    }

    #[test]
    fn file_collection_skips_build_directories() {
        let root = std::env::temp_dir().join(format!("sillage-preview-{}", std::process::id()));
        fs::create_dir_all(root.join("src")).unwrap();
        fs::create_dir_all(root.join("target")).unwrap();
        fs::write(root.join("src/main.rs"), "fn main() {}").unwrap();
        fs::write(root.join("target/build.log"), "generated").unwrap();
        let mut files = Vec::new();
        collect_files(&root, &root, &mut files);
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].relative, "src/main.rs");
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn porcelain_paths_keep_spaces_and_rename_destination() {
        let paths = parse_changed_paths(b"M  src/file name.rs\0R  src/new.rs\0src/old.rs\0");
        assert!(paths.contains("src/file name.rs"));
        assert!(paths.contains("src/new.rs"));
        assert!(!paths.contains("src/old.rs"));
    }

    #[test]
    fn diff_headers_read_as_metadata_rather_than_edits() {
        assert_eq!(classify_diff_line("--- a/src/main.rs"), DiffLine::Meta);
        assert_eq!(classify_diff_line("+++ b/src/main.rs"), DiffLine::Meta);
        assert_eq!(classify_diff_line("@@ -1,4 +1,6 @@"), DiffLine::Hunk);
        assert_eq!(classify_diff_line("+let value = 1;"), DiffLine::Added);
        assert_eq!(classify_diff_line("-let value = 0;"), DiffLine::Removed);
        assert_eq!(classify_diff_line(" unchanged"), DiffLine::Context);
    }

    #[test]
    fn a_long_diff_is_cut_off_with_a_note() {
        let long = "+line\n".repeat(MAX_DIFF_LINES + 50);
        let out = truncate_diff(&long);
        assert_eq!(out.lines().count(), MAX_DIFF_LINES + 1);
        assert!(out.ends_with(&format!("… diff cut off at {MAX_DIFF_LINES} lines")));
    }
}
