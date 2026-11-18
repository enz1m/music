use super::*;

// ---------------------------------------------------------------------------
// Titlebar + window controls
// ---------------------------------------------------------------------------

/// One section command in the titlebar: an icon button inline, a labelled
/// row once folded into the `More actions` overflow.
pub struct TitleAction {
    /// Descriptor/action id, unique within the titlebar.
    pub id: &'static str,
    /// Lucide glyph.
    pub icon: &'static str,
    /// Tooltip, accessible name and overflow-row label.
    pub label: String,
    pub message: Message,
    pub enabled: bool,
    /// Lower folds first.
    pub priority: u32,
    /// Rendered as destructive once folded.
    pub destructive: bool,
    /// Stable widget id for the inline button (focus restore, tests).
    pub widget_id: Option<iced::advanced::widget::Id>,
}

impl TitleAction {
    pub fn new(
        id: &'static str,
        icon: &'static str,
        label: impl Into<String>,
        message: Message,
        priority: u32,
    ) -> Self {
        Self {
            id,
            icon,
            label: label.into(),
            message,
            enabled: true,
            priority,
            destructive: false,
            widget_id: None,
        }
    }

    pub fn enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    pub fn destructive(mut self) -> Self {
        self.destructive = true;
        self
    }

    pub fn widget_id(mut self, id: iced::advanced::widget::Id) -> Self {
        self.widget_id = Some(id);
        self
    }
}

/// Everything the content titlebar shows for the current destination.
pub struct TitlebarModel<'a> {
    pub view: View,
    /// The destination's title, published as the page heading.
    pub title: String,
    /// A leading back control `(message, accessible name)` on drilled-in
    /// pages and the Search / Up Next side trips.
    pub back: Option<(Message, String)>,
    /// The destination's own commands, before the global ones.
    pub actions: Vec<TitleAction>,
    /// The Sort menu (Songs, Albums, Artists), with the current mode.
    pub sort: Option<SortMode>,
    pub sort_menu_model: &'a MenuModel,
    pub sort_menu: &'a MenuState,
    pub now_playing_menu_model: &'a MenuModel,
    pub now_playing_menu: &'a MenuState,
    /// The search query, edited in the titlebar while searching.
    pub search_query: &'a str,
    /// The title select's destinations menu.
    pub nav_menu_model: &'a MenuModel,
    pub nav_menu: &'a MenuState,
    /// In the split layout, the browse pane's width and the Now Playing
    /// column's: the page's bar spans the first, the window's close button
    /// sits over the second.
    pub split: Option<(f32, f32)>,
    /// Whether the `More actions` overflow is open.
    pub overflow_open: bool,
}

/// The page/global command separator's height, the toolbar icons' box.
const TITLEBAR_DIVIDER_H: f32 = 28.0;

/// The stable widget id of the titlebar back control.
pub const TITLEBAR_BACK_ID: &str = "music-titlebar-back";
/// The titlebar overflow trigger's stable widget id.
pub const TITLEBAR_OVERFLOW_ID: &str = "music-titlebar-overflow";
/// The action namespace of the titlebar's section commands.
pub const TITLEBAR_ACTIONS_PREFIX: &str = "music-titlebar-actions";

/// The content titlebar: the destination's title (the page heading), its
/// commands, and the global Search, Up Next and main menu.
pub fn titlebar<'a>(model: TitlebarModel<'a>, enz_theme: &'a EnzTheme) -> Element<'a, Message> {
    use enzui_core::action_overflow::{Descriptor, Presentation, VisibilityPriority};
    use enzui_widgets::action_bar::ActionItem;
    use enzui_widgets::toolbar_icon::enz_toolbar_icon;

    let current_view = model.view;
    let mut items: Vec<ActionItem<'a, Message>> = Vec::new();
    for action in model.actions {
        let TitleAction {
            id,
            icon,
            label,
            message,
            enabled,
            priority,
            destructive,
            widget_id,
        } = action;
        let descriptor = Descriptor::new(id)
            .with_presentation(Presentation::Compact)
            .with_priority(VisibilityPriority(priority))
            .with_enabled(enabled);
        let command = enzui_core::action::Action::new(id, message)
            .with_label(label)
            .with_icon(enzui_core::Icon::named(icon))
            .with_enabled(enabled);
        let item = ActionItem::with_inline(descriptor, command, move |action, _| {
            let mut button = enz_toolbar_icon(icon, action.message().clone())
                .aria_label(action.label().to_owned())
                .tooltip(action.label().to_owned())
                .enabled(action.is_enabled());
            // Every inline command is routable: an explicit id, else one
            // derived from its action id.
            button = button.id(widget_id
                .clone()
                .unwrap_or_else(|| enzui_core::identity::derive_id(TITLEBAR_ACTIONS_PREFIX, id)));
            button.into_element()
        })
        .destructive(destructive);
        items.push(item);
    }

    if let Some(sort_mode) = model.sort {
        let sort_menu_model = model.sort_menu_model;
        let sort_menu = model.sort_menu;
        items.push(
            ActionItem::with_inline(
                Descriptor::new("music.sort").pinned(),
                enzui_core::action::Action::new(
                    "music.sort",
                    Message::SortMenu(enzui_widgets::menu_button::MenuButtonEvent::Open(
                        enzui_core::menu_model::OpenCause::Pointer,
                    )),
                )
                .with_label(sort_trigger_label(sort_mode)),
                move |_, _| sort_control(sort_mode, sort_menu_model, sort_menu, current_view),
            )
            .compact_inline(),
        );
    }

    // A hairline separates the page's commands from the global ones.
    if !items.is_empty() {
        items.push(ActionItem::new(
            Descriptor::new("music.titlebar-divider").pinned(),
            || {
                iced::widget::container(
                    enzui_widgets::toolbar_divider::enz_toolbar_divider().into_element(),
                )
                .height(Length::Fixed(TITLEBAR_DIVIDER_H))
                .into()
            },
        ));
    }

    // Search, Up Next and the main menu stay pinned: they are first-reach
    // destinations on every page.
    let on_search = current_view == View::Search;
    items.push(ActionItem::new(
        Descriptor::new("music.search").pinned(),
        move || {
            enz_toolbar_icon(
                SEARCH_ICON,
                if on_search {
                    Message::ExitModal
                } else {
                    Message::SelectView(View::Search)
                },
            )
            .active(on_search)
            .aria_label(SEARCH_PLACEHOLDER)
            .tooltip(if on_search { "Close Search" } else { "Search" })
            .id(iced::advanced::widget::Id::new(SEARCH_ICON_ID))
            .into_element()
        },
    ));
    let on_queue = current_view == View::Queue;
    items.push(ActionItem::new(
        Descriptor::new("music.queue").pinned(),
        move || {
            enz_toolbar_icon(
                QUEUE_TOGGLE_ICON,
                if on_queue {
                    Message::ExitModal
                } else {
                    Message::SelectView(View::Queue)
                },
            )
            .active(on_queue)
            .aria_label(queue_toggle_label(on_queue))
            .tooltip(queue_toggle_label(on_queue))
            .id(iced::advanced::widget::Id::new(QUEUE_TOGGLE_ID))
            .into_element()
        },
    ));
    let now_playing_menu_model = model.now_playing_menu_model;
    let now_playing_menu = model.now_playing_menu;
    items.push(
        ActionItem::with_inline(
            Descriptor::new("music.now-playing-menu").pinned(),
            enzui_core::action::Action::new(
                "music.now-playing-menu",
                Message::NowPlayingMenu(enzui_widgets::menu_button::MenuButtonEvent::Open(
                    enzui_core::menu_model::OpenCause::Pointer,
                )),
            )
            .with_label(NOW_PLAYING_MENU_LABEL),
            move |_, _| {
                enz_menu_button(
                    "",
                    now_playing_menu_model,
                    now_playing_menu,
                    Message::NowPlayingMenu,
                )
                .menu_id(now_playing_menu_trigger_id())
                .kind(enzui_widgets::menu_button::MenuButtonKind::Toolbar)
                // The app menu is a kebab, as in the other enzUI apps.
                .trigger_icon("ellipsis-vertical")
                .tooltip(MAIN_MENU_TOOLTIP)
                .aria_label(NOW_PLAYING_MENU_LABEL)
                .into()
            },
        )
        .compact_inline(),
    );

    let mut bar = window_title_bar(enz_theme);
    let searching = current_view == View::Search;
    if searching {
        // Search happens in the titlebar: its field takes the bar, the page's
        // commands step aside, and the field's own X closes it.
        bar = bar
            .push(titlebar_search(model.search_query, enz_theme))
            .fluid_content(160.0);
    } else if let Some((message, label)) = model.back {
        // A drilled-in page keeps the destination select, with the way back
        // before it.
        bar = bar.leading(
            iced::widget::row![
                enz_toolbar_icon("navigation.back", message)
                    .aria_label(label.clone())
                    .tooltip(label)
                    .id(iced::advanced::widget::Id::new(TITLEBAR_BACK_ID))
                    .into_element(),
                nav_select(model.title, model.nav_menu_model, model.nav_menu, enz_theme,),
            ]
            .spacing(enz_theme.spacing.xs())
            .align_y(iced::Alignment::Center),
        );
    } else {
        // A destination's title is the select that switches destinations,
        // in place of a sidebar.
        bar = bar.leading(nav_select(
            model.title,
            model.nav_menu_model,
            model.nav_menu,
            enz_theme,
        ));
    }
    if searching {
        items.clear();
    }
    let bar = bar
        .actions(items, Message::TitlebarOverflow(!model.overflow_open))
        .overflow_open(model.overflow_open)
        .action_dismiss(Message::TitlebarOverflow(false))
        .action_menu_id_prefix(TITLEBAR_ACTIONS_PREFIX)
        .action_overflow_trigger_id(iced::advanced::widget::Id::new(TITLEBAR_OVERFLOW_ID));
    // The window's close button sits on its own band at the trailing edge,
    // as far from the side as from the top, so it follows the window's
    // rounded corner. In the split layout the page's bar spans the browse
    // pane and the close band ends the Now Playing column's strip.
    // Drawn only where a titlebar would draw controls: not under a
    // compositor's own frame, nor fullscreen.
    let close: Element<'a, Message> = if window_title_bar(enz_theme)
        .show_close(true)
        .renders_controls()
    {
        close_band()
    } else {
        iced::widget::Space::new().width(0).into()
    };
    match model.split {
        Some((browse_w, now_playing_w)) => iced::widget::row![
            iced::widget::container(bar.into_element()).width(browse_w),
            iced::widget::row![
                // The rest of the column's strip still drags the window.
                window_title_bar(enz_theme)
                    .id("music-app-window-now-playing")
                    .into_element(),
                close,
            ]
            .width(now_playing_w),
        ]
        .into(),
        None => iced::widget::row![bar.into_element(), close].into(),
    }
}

/// The close button's band: the titlebar's height, the button as far from
/// the window's side as from its top (`trailing_inset_for_square_corner`),
/// so it follows the window's rounded corner.
fn close_band<'a>() -> Element<'a, Message> {
    let control = enzui_widgets::window_controls::DEFAULT_CONTROL_SIZE;
    let inset =
        enzui_widgets::window_controls::trailing_inset_for_square_corner(TITLEBAR_HEIGHT, control);
    iced::widget::container(
        enzui_widgets::window_controls::enz_window_controls(Message::Window(
            enzui_widgets::title_bar_gesture::WindowCommand::Close,
        ))
        .id("music-app-window-controls")
        .into_element(),
    )
    .height(Length::Fixed(TITLEBAR_HEIGHT))
    // The shell already insets the titlebar slot by that much from the
    // window's side, so the band adds none on the trailing side.
    .padding(iced::Padding::new(0.0).left(inset))
    .center_y(Length::Fixed(TITLEBAR_HEIGHT))
    .into()
}

/// The titlebar's search field, with an X inside its trailing edge that
/// closes search and brings the page's commands back.
fn titlebar_search<'a>(query: &'a str, enz_theme: &'a EnzTheme) -> Element<'a, Message> {
    use enzui_widgets::toolbar_icon::enz_toolbar_icon;
    let field = enz_search_field(query, Message::SearchQueryChanged)
        .placeholder(SEARCH_PLACEHOLDER)
        .size(TextFieldSize::Md)
        .palette(enz_theme.palette.clone())
        .width(Length::Fill)
        .focus_on_mount(true)
        .id(iced::advanced::widget::Id::new(SEARCH_FIELD_ID));
    let close = enz_toolbar_icon("x", Message::ExitModal)
        .size(26.0, enz_theme.sizing.icon_sm)
        .aria_label("Close search")
        .tooltip("Close search")
        .id(iced::advanced::widget::Id::new(SEARCH_CLOSE_ID));
    iced::widget::stack![
        field,
        iced::widget::container(close)
            .width(Length::Fill)
            .height(Length::Fill)
            .align_right(Length::Fill)
            .center_y(Length::Fill)
            .padding(iced::Padding::new(0.0).right(4.0)),
    ]
    .width(Length::Fill)
    .into()
}

/// The titlebar search field's close button id.
pub const SEARCH_CLOSE_ID: &str = "music-titlebar-search-close";

/// The title select's trigger id.
pub const NAV_SELECT_ID: &str = "music-titlebar-destination";

/// The title select: the current destination's name with a chevron, opening
/// every destination (Songs, Artists, … Spotify).
fn nav_select<'a>(
    title: String,
    model: &'a MenuModel,
    state: &'a MenuState,
    enz_theme: &'a EnzTheme,
) -> Element<'a, Message> {
    use enzui_widgets::menu_button::MenuButtonKind;
    enz_menu_button(title, model, state, Message::NavMenu)
        .kind(MenuButtonKind::Flat)
        .height(32.0)
        .label_size(enz_theme.typography.section_size)
        .aria_label("Go to")
        .menu_id(iced::advanced::widget::Id::new(NAV_SELECT_ID))
        .into()
}

/// The titlebar's window-chrome contract without its content: a fixed
/// [`TITLEBAR_HEIGHT`] band, a close control, a stable window key, and every
/// gesture routed to `Message::Window`.
pub(super) fn window_title_bar<'a>(
    enz_theme: &'a EnzTheme,
) -> enzui_widgets::title_bar::EnzTitleBar<'a, Message> {
    let sp = &enz_theme.spacing;
    enz_title_bar()
        .height(TITLEBAR_HEIGHT)
        .padding_x(sp.md())
        .spacing(sp.xs())
        // The shell draws the close button, against the window's corner.
        .show_close(false)
        // Gestures are routed by this key; a shared prefix would alias windows.
        .id("music-app-window")
        .on_gesture(Message::Window)
}
