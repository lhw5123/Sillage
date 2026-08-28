//! Sillage: a macOS ARM64 desktop shell for local coding agents.

#[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
compile_error!("Sillage currently supports macOS ARM64 only.");

mod agents;
mod keys;
mod matching;
mod output;
mod projects;
mod workspace;

use gpui::{WindowBounds, prelude::*, px, size};
use gpui_component::{ActiveTheme as _, Root, Theme, ThemeMode, TitleBar};
use gpui_component_assets::Assets;

use crate::workspace::Workspace;

fn main() {
    let application = gpui_platform::application().with_assets(Assets);

    application.run(move |cx| {
        gpui_component::init(cx);
        crate::workspace::init(cx);
        Theme::change(ThemeMode::Dark, None, cx);

        let mut window_options = TitleBar::window_options();
        window_options.window_bounds = Some(WindowBounds::centered(size(px(1280.), px(840.)), cx));
        window_options.window_min_size = Some(size(px(880.), px(560.)));

        cx.spawn(async move |cx| {
            cx.open_window(window_options, |window, cx| {
                let workspace = cx.new(|cx| Workspace::new(window, cx));
                cx.new(|cx| Root::new(workspace, window, cx).bg(cx.theme().background))
            })
            .expect("failed to open window");
        })
        .detach();

        cx.activate(true);
    });
}
