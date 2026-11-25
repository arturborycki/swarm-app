use warpui::assets::asset_cache::AssetSource;
use warpui::elements::{
    Align, CacheOption, ConstrainedBox, Container, CrossAxisAlignment, Element, Flex, Image,
    MainAxisAlignment, MouseStateHandle, ParentElement, Wrap,
};
use settings::Setting as _;
use warp_errors::report_if_error;
use warpui::ui_components::button::ButtonVariant;
use warpui::ui_components::components::{UiComponent, UiComponentStyles};
use warpui::ui_components::switch::SwitchStateHandle;
use warpui::{AppContext, Entity, SingletonEntity as _, TypedActionView, View, ViewContext, ViewHandle};

use super::SettingsSection;
use super::settings_page::{
    MatchData, PageType, SettingsPageEvent, SettingsPageMeta, SettingsPageViewHandle,
    SettingsWidget, build_toggle_element, render_separator, render_sub_header,
};
use crate::appearance::Appearance;
use crate::channel::ChannelState;
use crate::settings::swarm_updates::SwarmUpdateSettings;
use crate::themes::theme::ColorScheme;
use crate::workspace::WorkspaceAction;

pub struct AboutPageView {
    page: PageType<Self>,
}

impl AboutPageView {
    pub fn new(ctx: &mut ViewContext<AboutPageView>) -> Self {
        ctx.subscribe_to_model(&SwarmUpdateSettings::handle(ctx), |_, _, _, ctx| ctx.notify());
        AboutPageView {
            page: PageType::new_monolith(AboutPageWidget::default(), None, false),
        }
    }
}

/// swarm-term: the update settings shown under the version.
#[derive(Clone, Debug)]
pub enum AboutPageAction {
    SetDownloadInBackground(bool),
    SetInstallWhenIdle(bool),
}

impl Entity for AboutPageView {
    type Event = SettingsPageEvent;
}
impl TypedActionView for AboutPageView {
    type Action = AboutPageAction;

    fn handle_action(&mut self, action: &Self::Action, ctx: &mut ViewContext<Self>) {
        SwarmUpdateSettings::handle(ctx).update(ctx, |settings, ctx| match action {
            AboutPageAction::SetDownloadInBackground(on) => {
                report_if_error!(settings.download_in_background.set_value(*on, ctx));
            }
            AboutPageAction::SetInstallWhenIdle(on) => {
                report_if_error!(settings.install_when_idle.set_value(*on, ctx));
            }
        });
        ctx.notify();
    }
}

impl View for AboutPageView {
    fn ui_name() -> &'static str {
        "AboutPage"
    }

    fn render(&self, app: &AppContext) -> Box<dyn Element> {
        self.page.render(self, app)
    }
}

#[derive(Default)]
struct AboutPageWidget {
    copy_version_button_mouse_state: MouseStateHandle,
    check_for_updates: MouseStateHandle,
    download_switch: SwitchStateHandle,
    install_switch: SwitchStateHandle,
}

impl AboutPageWidget {
    /// swarm-term: Check for Updates and when Swarm downloads and installs them.
    fn render_updates(&self, appearance: &Appearance, app: &AppContext) -> Box<dyn Element> {
        let settings = SwarmUpdateSettings::as_ref(app);
        let download = *settings.download_in_background;
        let install = download && *settings.install_when_idle;
        let mut column = Flex::column();
        column.add_child(render_separator(appearance));
        column.add_child(render_sub_header(appearance, "Updates", None));
        let check = appearance
            .ui_builder()
            .button(ButtonVariant::Secondary, self.check_for_updates.clone())
            .with_text_label("Check for Updates".to_string())
            .with_style(UiComponentStyles { font_size: Some(13.), ..Default::default() })
            .build()
            .on_click(|ctx, _, _| ctx.dispatch_typed_action(WorkspaceAction::SwarmCheckForUpdates))
            .finish();
        column.add_child(Container::new(check).with_margin_bottom(12.).finish());
        let switch = appearance
            .ui_builder()
            .switch(self.download_switch.clone())
            .check(download)
            .build()
            .on_click(move |ctx, _, _| ctx.dispatch_typed_action(AboutPageAction::SetDownloadInBackground(!download)))
            .finish();
        column.add_child(build_toggle_element(
            appearance.ui_builder().span("Download updates in the background".to_string()).build().finish(),
            switch,
            appearance,
            Some(
                "Swarm checks for a new version at launch and daily. Off: it asks before downloading. On: it downloads the new version first, then offers to restart into it; quitting Swarm also installs it."
                    .into(),
            ),
        ));
        let switch = appearance
            .ui_builder()
            .switch(self.install_switch.clone())
            .check(install)
            .with_disabled(!download)
            .build()
            .on_click(move |ctx, _, _| {
                if download {
                    ctx.dispatch_typed_action(AboutPageAction::SetInstallWhenIdle(!install))
                }
            })
            .finish();
        column.add_child(build_toggle_element(
            appearance.ui_builder().span("Install updates while Swarm isn't in use".to_string()).build().finish(),
            switch,
            appearance,
            Some(
                "Once an update is downloaded and you've been in another app for a minute, Swarm installs it and reopens in the background. Your sessions keep running."
                    .into(),
            ),
        ));
        Container::new(column.finish()).with_margin_top(24.).finish()
    }
}

impl SettingsWidget for AboutPageWidget {
    type View = AboutPageView;

    fn search_terms(&self) -> &str {
        "about swarm version updates check download install automatic"
    }

    fn render(
        &self,
        _view: &AboutPageView,
        appearance: &Appearance,
        app: &AppContext,
    ) -> Box<dyn Element> {
        let theme = appearance.theme();
        let ui_builder = appearance.ui_builder();

        // swarm-term: Swarm's icon with its name instead of Warp's logo.
        let image_path = if crate::swarm::SWARM_CHROME {
            "bundled/png/swarm-icon.png"
        } else if theme.inferred_color_scheme() == ColorScheme::LightOnDark {
            "bundled/svg/warp-logo-with-light-title.svg"
        } else {
            "bundled/svg/warp-logo-with-dark-title.svg"
        };

        let version = ChannelState::app_version().unwrap_or(if crate::swarm::SWARM_CHROME {
            concat!("v", env!("CARGO_PKG_VERSION"))
        } else {
            "v#.##.###"
        });

        let version_text = ui_builder
            .span(version.to_string())
            .with_soft_wrap()
            .build()
            .with_margin_top(16.)
            .finish();

        let copy_version_icon = appearance
            .ui_builder()
            .copy_button(16., self.copy_version_button_mouse_state.clone())
            .build()
            .on_click(move |ctx, _, _| {
                ctx.dispatch_typed_action(WorkspaceAction::CopyVersion(version));
            })
            .finish();

        let version_row = Wrap::row()
            .with_main_axis_alignment(MainAxisAlignment::Center)
            .with_children([
                version_text,
                Container::new(copy_version_icon)
                    .with_margin_top(16.)
                    .with_padding_left(6.)
                    .finish(),
            ]);

        Align::new(
            Flex::column()
                .with_cross_axis_alignment(CrossAxisAlignment::Center)
                .with_child(
                    ConstrainedBox::new(
                        Image::new(
                            AssetSource::Bundled { path: image_path },
                            CacheOption::BySize,
                        )
                        .finish(),
                    )
                    .with_max_height(100.)
                    .with_max_width(350.)
                    .finish(),
                )
                .with_children(crate::swarm::SWARM_CHROME.then(|| {
                    ui_builder
                        .span(crate::swarm::APP_NAME.to_string())
                        .with_style(warpui::ui_components::components::UiComponentStyles {
                            font_size: Some(28.),
                            ..Default::default()
                        })
                        .build()
                        .with_margin_top(12.)
                        .finish()
                }))
                .with_child(version_row.finish())
                .with_child(
                    ui_builder
                        .span("Copyright 2026 Swarm")
                        .build()
                        .with_margin_top(16.)
                        .finish(),
                )
                .with_children(
                    (crate::swarm::SWARM_CHROME && cfg!(target_os = "macos"))
                        .then(|| self.render_updates(appearance, app)),
                )
                .finish(),
        )
        .finish()
    }
}

impl SettingsPageMeta for AboutPageView {
    fn section() -> SettingsSection {
        SettingsSection::About
    }

    fn should_render(&self, _ctx: &AppContext) -> bool {
        true
    }

    fn update_filter(&mut self, query: &str, ctx: &mut ViewContext<Self>) -> MatchData {
        self.page.update_filter(query, ctx)
    }

    fn scroll_to_widget(&mut self, widget_id: &'static str) {
        self.page.scroll_to_widget(widget_id)
    }

    fn clear_highlighted_widget(&mut self) {
        self.page.clear_highlighted_widget();
    }
}

impl From<ViewHandle<AboutPageView>> for SettingsPageViewHandle {
    fn from(view_handle: ViewHandle<AboutPageView>) -> Self {
        SettingsPageViewHandle::About(view_handle)
    }
}
