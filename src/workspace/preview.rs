//! Resizable workspace preview for files, command output, and web pages.

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PreviewTab {
    File,
    Terminal,
    Browser,
}

#[derive(Debug, Clone)]
struct WorkspaceFile {
    absolute: PathBuf,
    relative: String,
    artifact: bool,
}

pub(super) struct PreviewPane {
    workspace: Option<PathBuf>,
    tab: PreviewTab,
    files: Vec<WorkspaceFile>,
    selected_file: Option<PathBuf>,
    file_content: SharedString,
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
            tab: PreviewTab::File,
            files: Vec::new(),
            selected_file: None,
            file_content: "Choose a file from the workspace.".into(),
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

    pub(super) fn refresh(&mut self, cx: &mut Context<Self>) {
        self.files = self
            .workspace
            .as_deref()
            .map(workspace_files)
            .unwrap_or_default();
        if let Some(path) = self.selected_file.clone() {
            self.read_file(path);
        }
        cx.notify();
    }

    fn select_tab(&mut self, tab: PreviewTab, cx: &mut Context<Self>) {
        self.tab = tab;
        self.webview.update(cx, |webview, _| {
            if tab == PreviewTab::Browser {
                webview.show();
            } else {
                webview.hide();
            }
        });
        cx.notify();
    }

    fn read_file(&mut self, path: PathBuf) {
        self.selected_file = Some(path.clone());
        self.file_content = read_preview(&path).into();
    }

    fn open_file(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        self.read_file(path);
        self.select_tab(PreviewTab::File, cx);
    }

    fn preview_in_browser(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        let url = format!("file://{}", path.to_string_lossy());
        self.load_url(&url, cx);
        self.address_input.update(cx, |input, cx| {
            input.set_value(url, window, cx);
        });
        self.select_tab(PreviewTab::Browser, cx);
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

    fn render_tab(
        &self,
        id: &'static str,
        label: &'static str,
        icon: IconName,
        tab: PreviewTab,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        Button::new(id)
            .ghost()
            .xsmall()
            .icon(icon)
            .label(label)
            .when(self.tab == tab, |button| button.primary())
            .on_click(cx.listener(move |this, _, _, cx| this.select_tab(tab, cx)))
    }

    fn render_file_tab(&self, cx: &mut Context<Self>) -> impl IntoElement {
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
                                        .tooltip("Open in Browser preview")
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
        cx: &mut Context<Self>,
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

    fn render_terminal_tab(&self, cx: &mut Context<Self>) -> impl IntoElement {
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

    fn render_browser_tab(&self, cx: &mut Context<Self>) -> impl IntoElement {
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
}

impl Render for PreviewPane {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.webview.update(cx, |webview, _| {
            if self.tab == PreviewTab::Browser {
                webview.show();
            } else {
                webview.hide();
            }
        });
        let theme = cx.theme();
        v_flex()
            .size_full()
            .min_w_0()
            .border_l_1()
            .border_color(theme.border)
            .bg(theme.background)
            .child(
                h_flex()
                    .h(px(38.))
                    .px_2()
                    .gap_1()
                    .items_center()
                    .border_b_1()
                    .border_color(theme.border)
                    .child(self.render_tab(
                        "preview-file",
                        "File",
                        IconName::File,
                        PreviewTab::File,
                        cx,
                    ))
                    .child(self.render_tab(
                        "preview-terminal",
                        "Terminal",
                        IconName::SquareTerminal,
                        PreviewTab::Terminal,
                        cx,
                    ))
                    .child(self.render_tab(
                        "preview-browser",
                        "Browser",
                        IconName::Globe,
                        PreviewTab::Browser,
                        cx,
                    ))
                    .child(div().flex_1())
                    .child(
                        Button::new("refresh-preview")
                            .ghost()
                            .xsmall()
                            .label("Refresh")
                            .tooltip("Refresh workspace files")
                            .on_click(cx.listener(|this, _, _, cx| this.refresh(cx))),
                    ),
            )
            .child(div().flex_1().min_h_0().child(match self.tab {
                PreviewTab::File => self.render_file_tab(cx).into_any_element(),
                PreviewTab::Terminal => self.render_terminal_tab(cx).into_any_element(),
                PreviewTab::Browser => self.render_browser_tab(cx).into_any_element(),
            }))
    }
}

fn normalize_url(value: &str) -> String {
    if value.is_empty() || value.contains("://") || value.starts_with("file:") {
        value.to_string()
    } else {
        format!("http://{value}")
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
}
