//! All menus: the title screen (main menu and leaderboard, switched by the bookmark tabs on the
//! panel's edge), the account screens (log in, create account, verification, forgot / reset
//! password, profile), the matchmaking queues (quick play, competitive), "waiting for opponent",
//! the in-game pause overlay (Esc) and the settings screen. Screens are described by the
//! `UiScreen` resource; the UI is rebuilt whenever it changes. Styling comes from `theme` (TF2
//! fonts, palette and panels).

use crate::AppState;
use crate::account::{
    Account, Ending, HistoryEntry, LeaderboardEntry, QueueKind, RankedResult, Rating, Stats,
};
use crate::config::{ClientConfig, ROOM_CODE_LEN, code_from_text, normalize_room_code};
use crate::loading::StartupDone;
use crate::net::{
    LOBBY_TIMEOUT_MINUTES, MatchKind, NetCommand, RoomConnection, RoomFailure, SignalingStatus,
};
use crate::settings::{Action, Axis, Binding, Settings, Slider};
use crate::textfield::{Field, Form, spawn_field};
use crate::theme::{self, Theme, code_display};
use bevy::camera::ClearColorConfig;
use bevy::clipboard::{Clipboard, ClipboardRead};
use bevy::ecs::relationship::RelatedSpawnerCommands;
use bevy::ecs::system::SystemParam;
use bevy::input::keyboard::{Key, KeyboardInput};
use bevy::input::mouse::{MouseButtonInput, MouseScrollUnit, MouseWheel};
use bevy::prelude::*;
use bevy::text::LineBreak;
use bevy::ui::UiGlobalTransform;
use bevy::ui::widget::NodeImageMode;
use bevy::window::PrimaryWindow;
use endif_sim::Weapon;

/// Which screen is showing.
#[derive(Resource, Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum UiScreen {
    #[default]
    Hidden,
    Main,
    /// The title screen's second tab: the top players by rating.
    Leaderboard,
    Pause,
    Settings {
        /// Opened from the pause menu (true) or the main menu (false).
        from_game: bool,
    },
    Login,
    Register,
    /// Waiting for the e-mail code after registering.
    Verify,
    Forgot,
    /// Waiting for the e-mail code after "forgot password".
    Reset,
    Profile,
    ChangeUsername,
    ChangePassword,
    /// In a matchmaking queue (which one: `Account::queue`).
    Queue,
    /// The desktop builds (reachable from the web build's title screen).
    Download,
}

impl UiScreen {
    /// True while an in-game overlay is open (mouse released, game input ignored).
    pub fn blocks_game_input(self) -> bool {
        matches!(
            self,
            UiScreen::Pause | UiScreen::Settings { from_game: true }
        )
    }

    /// The button Enter presses on this screen, if it is a form.
    fn submit_action(self) -> Option<UiAction> {
        Some(match self {
            UiScreen::Login => UiAction::SubmitLogin,
            UiScreen::Register => UiAction::SubmitRegister,
            UiScreen::Verify => UiAction::SubmitVerify,
            UiScreen::Forgot => UiAction::SubmitForgot,
            UiScreen::Reset => UiAction::SubmitReset,
            UiScreen::ChangeUsername => UiAction::SubmitUsername,
            UiScreen::ChangePassword => UiAction::SubmitPassword,
            _ => return None,
        })
    }
}

/// The screen the menu opens on next time it is entered (a finished ranked match opens the profile).
#[derive(Resource, Default)]
pub struct ReturnScreen(pub Option<UiScreen>);

/// Set when the current screen must be rebuilt (settings changed, binding captured, ...).
#[derive(Resource, Default)]
pub struct UiRefresh(pub bool);

/// The main menu's panel, whose size the other title tabs copy.
#[derive(Component)]
struct TitlePanel;

/// Size of the main menu's panel (logical pixels), as last laid out. The leaderboard panel is
/// built to the same size, so switching tabs moves nothing: the frame stays put and only its
/// contents change.
#[derive(Resource, Default)]
struct TitlePanelSize(Option<Vec2>);

/// The leaderboard's table box, whose height says how many players fit on a page.
#[derive(Component)]
struct LeaderboardTable;

/// Players that fit on a leaderboard page, from the table box as last laid out; none until the
/// tab has been shown.
#[derive(Resource, Default)]
struct LeaderboardRows(Option<u32>);

/// What the profile's match history shows: competitive matches only, or every round.
#[derive(Resource, Default)]
struct HistoryFilter {
    comp_only: bool,
}

/// Waiting for the next key/mouse press to bind this action.
#[derive(Resource, Default)]
pub struct Listening(pub Option<Action>);

/// A value box is open for typing: which slider, and the text typed so far.
#[derive(Resource, Default)]
struct Editing(Option<(Slider, String)>);

#[derive(Component)]
struct UiRoot;

#[derive(Component)]
struct MenuCamera;

/// A panel that scrolls (mouse wheel) when its contents are taller than the window.
#[derive(Component)]
struct ScrollPane;

#[derive(Component, Clone, Copy, PartialEq, Debug)]
enum UiAction {
    Practice,
    CreateRoom,
    JoinRoom,
    /// Join the quick play queue (anyone).
    QuickPlay,
    /// Join the competitive queue (accounts only).
    Competitive,
    CancelQueue,
    OpenSettings,
    Back,
    Resume,
    Leave,
    InvertY,
    /// Unlock / relock the X and Y sensitivities.
    SeparateSensitivity,
    /// Web only: go fullscreen on the click that starts play.
    #[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
    Fullscreen,
    /// Let the game pick the input delay from the connection.
    AdaptiveDelay,
    /// Preferred rocket launcher: stock or The Original (takes effect at the next spawn).
    Launcher,
    /// Dismiss the "room is full" error: back to the menu with the code cleared.
    ErrorOk,
    /// Dismiss the result popup of a finished ranked match.
    ResultOk,
    /// Paste a room code / invite link from the clipboard into the code field.
    Paste,
    EditValue(Slider),
    Bind(Action),
    ResetDefaults,
    /// The little arrow in the corner of a form window (same as `Back`, drawn subtly).
    BackArrow,
    OpenLogin,
    SubmitLogin,
    OpenRegister,
    SubmitRegister,
    SubmitVerify,
    /// Another code for the verify / reset screens.
    Resend,
    OpenForgot,
    SubmitForgot,
    SubmitReset,
    OpenProfile,
    Logout,
    OpenChangeUsername,
    SubmitUsername,
    OpenChangePassword,
    SubmitPassword,
    /// Only the web build spawns the button (desktop has nothing to download).
    #[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
    OpenDownload,
    /// Fetch a desktop build (opens `/download/<platform>` in a new tab).
    Download(Platform),
    /// This build is out of date: reload the page (web) or hand off to the updater (desktop).
    Update,
    /// One of the bookmark tabs on the title panel's edge.
    OpenTab(Tab),
    /// Another page of the leaderboard (from 1).
    LeaderboardPage(u32),
    /// The COMP / ALL switch above the match history.
    HistoryFilter,
}

impl UiAction {
    /// How the button colours itself: tan econ button, a subtle hover tint, or its own colours.
    fn style(self) -> ButtonStyle {
        match self {
            UiAction::EditValue(_) | UiAction::OpenProfile | UiAction::BackArrow => {
                ButtonStyle::Subtle
            }
            UiAction::InvertY
            | UiAction::SeparateSensitivity
            | UiAction::Fullscreen
            | UiAction::AdaptiveDelay
            | UiAction::Launcher
            | UiAction::HistoryFilter
            | UiAction::OpenTab(_) => ButtonStyle::Custom,
            _ => ButtonStyle::Plain,
        }
    }
}

/// A desktop build on the download screen. nginx serves each from `/download/<name>` next to the
/// page (`deploy/nginx/endif.tf.conf`); the packages come from `deploy/package-desktop.sh`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Platform {
    Windows,
    Linux,
}

impl Platform {
    const ALL: [Platform; 2] = [Platform::Windows, Platform::Linux];

    fn label(self) -> &'static str {
        match self {
            Platform::Windows => "Windows",
            Platform::Linux => "Linux",
        }
    }

    /// Relative to the page, so a staging deployment offers its own builds.
    fn url(self) -> String {
        let name = match self {
            Platform::Windows => "windows",
            Platform::Linux => "linux",
        };
        format!("/download/{name}")
    }
}

/// The bookmark tabs hanging off the right edge of the title screen's panel, top to bottom. The
/// tab of the page being shown has no border against the panel: the panel opens into it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Tab {
    Menu,
    Leaderboard,
}

impl Tab {
    const ALL: [Tab; 2] = [Tab::Menu, Tab::Leaderboard];

    fn screen(self) -> UiScreen {
        match self {
            Tab::Menu => UiScreen::Main,
            Tab::Leaderboard => UiScreen::Leaderboard,
        }
    }

    /// What the tab says when hovered.
    fn name(self) -> &'static str {
        match self {
            Tab::Menu => "menu",
            Tab::Leaderboard => "leaderboard",
        }
    }
}

/// A tab's icon: its tint at rest and while the tab is hovered.
#[derive(Component)]
struct TabIcon {
    rest: Color,
    hover: Color,
}

/// A bookmark tab. `off`: not the current page, so it can be opened (and lights up under the
/// mouse); the current one only shows its name.
#[derive(Component)]
struct TabNode {
    off: bool,
}

/// Tab geometry, logical pixels. The images are drawn to these proportions (`tools/tf2/ui_assets.py`).
const TAB_W: f32 = 46.0;
const TAB_H: f32 = 64.0;
const TAB_GAP: f32 = 3.0;
/// Distance from the panel's top to the first tab.
const TAB_TOP: f32 = 18.0;
const TAB_ICON: f32 = 28.0;
/// The panel's border (`theme::panel`), which the selected tab lies over.
const PANEL_BORDER: f32 = theme::PANEL_BORDER;
/// Brightening of a tab's artwork under the mouse.
const TAB_HOVER_TINT: Color = Color::srgb(1.25, 1.25, 1.25);

#[derive(Clone, Copy, PartialEq, Eq)]
enum ButtonStyle {
    Plain,
    Subtle,
    Custom,
}

/// A button that shows a tooltip instead of acting (online play while the server is unreachable).
#[derive(Component)]
struct Disabled;

#[derive(Component)]
struct Tooltip;

const BTN_DISABLED: Color = Color::srgb_u8(120, 112, 100);

#[derive(Component)]
struct CodeField;

#[derive(Component)]
struct ConnectingText;

/// The "searching..." line of the queue screen.
#[derive(Component)]
struct QueueText;

/// The "N playing, M in queue" line under it.
#[derive(Component)]
struct QueueSizeText;

/// The "(N players)" label inside the main menu's "Quick play" / "Competitive" button.
#[derive(Component)]
struct CountText(QueueKind);

/// The "N players online" line under the logo.
#[derive(Component)]
struct OnlineText;

/// Slider parts.
#[derive(Component)]
struct SliderTrack(Slider);
#[derive(Component)]
struct SliderFill(Slider);
#[derive(Component)]
struct SliderKnob(Slider);
/// The text inside a value box.
#[derive(Component)]
struct ValueText(Slider);

#[derive(Resource, Default)]
struct TypedCode(String);

/// A clipboard read (Ctrl+V or the paste button) that has not been applied yet; on the web the
/// read is asynchronous. Pasted text goes to the focused text field, or else the room code.
#[derive(Resource, Default)]
struct PendingPaste(Option<ClipboardRead>);

const BUTTON_W: f32 = 320.0;
const BUTTON_H: f32 = 44.0;
const ROW_W: f32 = 660.0;
const SLIDER_W: f32 = 260.0;
const FIELD_W: f32 = 320.0;
/// Logical pixels scrolled per mouse-wheel notch.
const WHEEL_LINE_PX: f32 = 48.0;
/// Space kept between a panel and the window edges, so a scrolling panel never touches them.
const SCREEN_MARGIN: f32 = 16.0;

pub struct MenuPlugin;

impl Plugin for MenuPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<TypedCode>()
            .init_resource::<UiScreen>()
            .init_resource::<UiRefresh>()
            .init_resource::<Listening>()
            .init_resource::<Editing>()
            .init_resource::<PendingPaste>()
            .init_resource::<ReturnScreen>()
            .init_resource::<TitlePanelSize>()
            .init_resource::<LeaderboardRows>()
            .init_resource::<HistoryFilter>()
            .add_systems(OnEnter(AppState::Menu), enter_menu)
            .add_systems(OnExit(AppState::Menu), leave_menu)
            .add_systems(OnEnter(AppState::InGame), |mut s: ResMut<UiScreen>| {
                *s = UiScreen::Hidden
            })
            .add_systems(
                OnExit(AppState::InGame),
                |mut s: ResMut<UiScreen>, mut l: ResMut<Listening>| {
                    *s = UiScreen::Hidden;
                    l.0 = None;
                },
            )
            .add_systems(
                Update,
                (
                    edit_value_keys,
                    escape_key,
                    capture_binding,
                    ui_buttons,
                    tab_hover,
                    form_submit,
                    slider_drag,
                    paste_shortcut.run_if(in_state(AppState::Menu)),
                    type_code.run_if(in_state(AppState::Menu)),
                    apply_paste.run_if(in_state(AppState::Menu)),
                    auto_join
                        .run_if(in_state(AppState::Menu).and_then(resource_exists::<StartupDone>)),
                    history_search,
                    rebuild_ui,
                    wheel_scroll,
                    sync_settings_widgets,
                    disabled_tooltips,
                    queue_status,
                    activity_counts,
                    resend_timer,
                )
                    .chain(),
            )
            // Reads last frame's layout, so it needs no place in the chain (which is full anyway).
            .add_systems(
                Update,
                (measure_title_panel, leaderboard_fetch)
                    .chain()
                    .run_if(in_state(AppState::Menu)),
            )
            .add_systems(OnEnter(AppState::Connecting), setup_connecting)
            .add_systems(OnExit(AppState::Connecting), despawn_ui)
            .add_systems(
                Update,
                (connecting_phase, connecting_screen, connecting_keys)
                    .run_if(in_state(AppState::Connecting)),
            );
    }
}

fn menu_camera() -> impl Bundle {
    (
        Camera2d,
        Camera {
            clear_color: ClearColorConfig::Custom(theme::DARK_BROWN),
            ..default()
        },
        MenuCamera,
    )
}

fn enter_menu(
    mut commands: Commands,
    mut screen: ResMut<UiScreen>,
    mut ret: ResMut<ReturnScreen>,
    cam: Query<Entity, With<Camera>>,
) {
    if cam.is_empty() {
        commands.spawn(menu_camera());
    }
    *screen = ret.0.take().unwrap_or(UiScreen::Main);
}

fn leave_menu(
    mut commands: Commands,
    cams: Query<Entity, With<MenuCamera>>,
    roots: Query<Entity, With<UiRoot>>,
    mut form: ResMut<Form>,
) {
    form.focus = None;
    for e in cams.iter().chain(roots.iter()) {
        commands.entity(e).despawn();
    }
}

fn despawn_ui(
    mut commands: Commands,
    q: Query<Entity, With<UiRoot>>,
    cams: Query<Entity, With<MenuCamera>>,
) {
    for e in q.iter().chain(cams.iter()) {
        commands.entity(e).despawn();
    }
}

// ------------------------------------------------------------------------------------ widgets

fn button(theme: &Theme, label: &str, action: UiAction) -> impl Bundle {
    theme.button(label, action, BUTTON_W, BUTTON_H, 18.0)
}

fn small_button(theme: &Theme, label: &str, action: UiAction) -> impl Bundle {
    theme.button(label, action, 64.0, 32.0, 13.0)
}

/// A button in a row of two or three under a form.
fn form_button(theme: &Theme, label: &str, action: UiAction) -> impl Bundle {
    theme.button(label, action, 158.0, 40.0, 14.0)
}

/// Full-screen root. The main menu gets the title background; in-game overlays darken the view.
fn screen_root(commands: &mut Commands, theme: &Theme, translucent: bool) -> Entity {
    let node = Node {
        width: Val::Percent(100.0),
        height: Val::Percent(100.0),
        flex_direction: FlexDirection::Column,
        justify_content: JustifyContent::Center,
        align_items: AlignItems::Center,
        row_gap: Val::Px(6.0),
        padding: UiRect::all(Val::Px(SCREEN_MARGIN)),
        // The backdrop overflows the window along one axis; crop it rather than scroll.
        overflow: Overflow::clip(),
        ..default()
    };
    let mut e = commands.spawn((UiRoot, node, GlobalZIndex(10)));
    if translucent {
        e.insert(BackgroundColor(theme::OVERLAY_BG));
    } else {
        // First child so it draws beneath the panels; absolute, so it doesn't join the flex column.
        e.with_child(theme.menu_backdrop());
    }
    e.id()
}

/// A panel column holding a screen's controls.
fn panel_column(padding: f32) -> impl Bundle {
    theme::panel(Node {
        flex_direction: FlexDirection::Column,
        align_items: AlignItems::Center,
        padding: UiRect::all(Val::Px(padding)),
        row_gap: Val::Px(4.0),
        ..default()
    })
}

/// A `panel_column` that is never taller than the window: past that its contents scroll. Clipping at
/// the content box keeps the padding clear so scrolled rows never touch the rounded border.
fn scrolling_panel_column(padding: f32, scroll: Vec2) -> impl Bundle {
    (
        theme::panel(Node {
            flex_direction: FlexDirection::Column,
            align_items: AlignItems::Center,
            padding: UiRect::all(Val::Px(padding)),
            row_gap: Val::Px(4.0),
            max_height: Val::Percent(100.0),
            overflow: Overflow::scroll_y(),
            overflow_clip_margin: OverflowClipMargin::content_box(),
            ..default()
        }),
        ScrollPosition(scroll),
        ScrollPane,
    )
}

fn section(theme: &Theme, title: &str) -> impl Bundle {
    (
        theme.heading_flat(title.to_uppercase(), 20.0, theme::ORANGE),
        Node {
            width: Val::Px(ROW_W),
            margin: UiRect::new(Val::Px(0.0), Val::Px(0.0), Val::Px(10.0), Val::Px(2.0)),
            ..default()
        },
    )
}

fn no_wrap() -> TextLayout {
    TextLayout {
        linebreak: LineBreak::NoWrap,
        ..default()
    }
}

/// A settings row: label on the left, controls right-aligned.
fn row(
    p: &mut RelatedSpawnerCommands<ChildOf>,
    theme: &Theme,
    label: &str,
    f: impl FnOnce(&mut RelatedSpawnerCommands<ChildOf>),
) {
    p.spawn(Node {
        width: Val::Px(ROW_W),
        min_height: Val::Px(40.0),
        align_items: AlignItems::Center,
        justify_content: JustifyContent::SpaceBetween,
        ..default()
    })
    .with_children(|r| {
        r.spawn((theme.label(label, 17.0, theme::TAN_LIGHT), no_wrap()));
        r.spawn(Node {
            flex_direction: FlexDirection::Row,
            align_items: AlignItems::Center,
            column_gap: Val::Px(12.0),
            ..default()
        })
        .with_children(f);
    });
}

/// Slider + value box for one setting. The slider is dragged; the value box is clicked to type an
/// exact number.
fn slider_controls(
    c: &mut RelatedSpawnerCommands<ChildOf>,
    theme: &Theme,
    s: &Settings,
    slider: Slider,
    width: f32,
) {
    let axis = slider;
    let frac = slider.fraction(s);
    // Track (the clickable area is taller than the bar so it is easy to grab).
    c.spawn((
        Button,
        SliderTrack(axis),
        Node {
            width: Val::Px(width),
            height: Val::Px(26.0),
            ..default()
        },
    ))
    .with_children(|t| {
        t.spawn(theme::inset(Node {
            position_type: PositionType::Absolute,
            left: Val::Px(0.0),
            right: Val::Px(0.0),
            top: Val::Px(8.0),
            height: Val::Px(10.0),
            ..default()
        }))
        .with_children(|bar| {
            bar.spawn((
                SliderFill(axis),
                Node {
                    position_type: PositionType::Absolute,
                    left: Val::Px(0.0),
                    top: Val::Px(0.0),
                    bottom: Val::Px(0.0),
                    width: Val::Percent(frac * 100.0),
                    border_radius: BorderRadius::all(Val::Px(3.0)),
                    ..default()
                },
                BackgroundColor(theme::YELLOW),
            ));
        });
        t.spawn((
            SliderKnob(axis),
            Node {
                position_type: PositionType::Absolute,
                left: Val::Percent(frac * 100.0),
                top: Val::Px(1.0),
                width: Val::Px(16.0),
                height: Val::Px(24.0),
                margin: UiRect::left(Val::Px(-8.0)),
                border: UiRect::all(Val::Px(2.0)),
                border_radius: BorderRadius::all(Val::Px(4.0)),
                ..default()
            },
            BackgroundColor(theme::TAN_LIGHT),
            BorderColor::all(theme::TAN_DARKER),
            BoxShadow::new(
                Color::srgba(0.0, 0.0, 0.0, 0.5),
                Val::Px(0.0),
                Val::Px(2.0),
                Val::Px(0.0),
                Val::Px(3.0),
            ),
        ));
    });
    // Value box.
    c.spawn((
        Button,
        UiAction::EditValue(axis),
        theme::inset(Node {
            width: Val::Px(84.0),
            height: Val::Px(34.0),
            justify_content: JustifyContent::Center,
            align_items: AlignItems::Center,
            ..default()
        }),
    ))
    .with_children(|b| {
        // "8 (133 ms)" is too long for the box at the usual size.
        let size = if slider == Slider::InputDelay {
            12.0
        } else {
            16.0
        };
        b.spawn((
            ValueText(axis),
            theme.heading_flat(slider.display(s), size, theme::YELLOW),
            no_wrap(),
        ));
    });
}

fn toggle_cell(theme: &Theme, text: &str, active: bool) -> impl Bundle {
    (
        Node {
            flex_grow: 1.0,
            height: Val::Percent(100.0),
            justify_content: JustifyContent::Center,
            align_items: AlignItems::Center,
            border_radius: BorderRadius::all(Val::Px(14.0)),
            ..default()
        },
        BackgroundColor(if active { theme::ORANGE } else { Color::NONE }),
        children![(
            theme.heading_flat(
                text.to_string(),
                13.0,
                if active {
                    theme::TAN_DARKER
                } else {
                    theme::OFF_WHITE
                }
            ),
            no_wrap()
        )],
    )
}

/// Two-way switch; the active half is lit. `first` picks the left label.
fn switch(theme: &Theme, labels: [&str; 2], first: bool, action: UiAction) -> impl Bundle {
    (
        Button,
        action,
        Node {
            width: Val::Px(112.0),
            height: Val::Px(32.0),
            flex_direction: FlexDirection::Row,
            padding: UiRect::all(Val::Px(2.0)),
            border: UiRect::all(Val::Px(2.0)),
            border_radius: BorderRadius::all(Val::Px(16.0)),
            ..default()
        },
        BackgroundColor(theme::INSET_BG),
        BorderColor::all(theme::TAN_DARKER),
        children![
            toggle_cell(theme, labels[0], first),
            toggle_cell(theme, labels[1], !first)
        ],
    )
}

/// YES / NO switch.
fn toggle(theme: &Theme, on: bool, action: UiAction) -> impl Bundle {
    switch(theme, ["YES", "NO"], on, action)
}

/// On-screen size of a launcher picture in the launcher switch (`ui/launcher_*.png` are drawn at
/// twice this, see `tools/tf2/ui_assets.py`).
const LAUNCHER_ICON_W: f32 = 64.0;
const LAUNCHER_ICON_H: f32 = 40.0;

fn icon_cell(image: Handle<Image>, active: bool) -> impl Bundle {
    (
        Node {
            width: Val::Px(LAUNCHER_ICON_W + 12.0),
            height: Val::Percent(100.0),
            justify_content: JustifyContent::Center,
            align_items: AlignItems::Center,
            border_radius: BorderRadius::all(Val::Px(3.0)),
            ..default()
        },
        BackgroundColor(if active { theme::ORANGE } else { Color::NONE }),
        children![(
            ImageNode {
                image,
                color: if active {
                    Color::WHITE
                } else {
                    Color::srgba(1.0, 1.0, 1.0, 0.4)
                },
                ..default()
            },
            Node {
                width: Val::Px(LAUNCHER_ICON_W),
                height: Val::Px(LAUNCHER_ICON_H),
                ..default()
            },
        )],
    )
}

/// Two-way switch between two pictures, square-cornered where `switch` is a pill; the active
/// side is lit and the other dimmed. `first` picks the left picture.
fn icon_switch(images: [Handle<Image>; 2], first: bool, action: UiAction) -> impl Bundle {
    let [left, right] = images;
    (
        Button,
        action,
        Node {
            height: Val::Px(LAUNCHER_ICON_H + 16.0),
            flex_direction: FlexDirection::Row,
            padding: UiRect::all(Val::Px(2.0)),
            border: UiRect::all(Val::Px(2.0)),
            border_radius: BorderRadius::all(Val::Px(5.0)),
            ..default()
        },
        BackgroundColor(theme::INSET_BG),
        BorderColor::all(theme::TAN_DARKER),
        children![icon_cell(left, first), icon_cell(right, !first)],
    )
}

// ------------------------------------------------------------------------------------ screens

/// The last of `rebuild_ui`'s inputs, bundled: a system takes at most sixteen parameters.
#[derive(SystemParam)]
struct RebuildExtras<'w> {
    cfg: Res<'w, ClientConfig>,
    panel_size: Res<'w, TitlePanelSize>,
    history: Res<'w, HistoryFilter>,
}

/// Rebuilds the profile while the opponent search is typed in, so the list follows the text.
fn history_search(
    screen: Res<UiScreen>,
    form: Res<Form>,
    mut refresh: ResMut<UiRefresh>,
    mut last: Local<String>,
) {
    let query = form.get(Field::HistorySearch);
    if *last != query {
        *last = query.to_string();
        if *screen == UiScreen::Profile {
            refresh.0 = true;
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn rebuild_ui(
    mut commands: Commands,
    theme: Res<Theme>,
    screen: Res<UiScreen>,
    status: Res<SignalingStatus>,
    mut refresh: ResMut<UiRefresh>,
    settings: Res<Settings>,
    listening: Res<Listening>,
    mut editing: ResMut<Editing>,
    typed: Res<TypedCode>,
    account: Res<Account>,
    mut form: ResMut<Form>,
    roots: Query<Entity, With<UiRoot>>,
    panes: Query<&ScrollPosition, With<ScrollPane>>,
    state: Res<State<AppState>>,
    time: Res<Time<Real>>,
    extra: RebuildExtras,
) {
    if !screen.is_changed() && !refresh.0 {
        return;
    }
    let now = time.elapsed_secs_f64();
    refresh.0 = false;
    // A refresh (binding captured, toggle flipped, ...) keeps the scroll offset; a new screen starts at the top.
    let scroll = if screen.is_changed() {
        Vec2::ZERO
    } else {
        panes.iter().next().map(|p| p.0).unwrap_or_default()
    };
    // The connecting screen is built by `setup_connecting` and must not be torn down here.
    if *state.get() == AppState::Connecting {
        return;
    }
    if screen.is_changed() {
        editing.0 = None;
        // Forms open with their first field focused; the anonymous name box does not steal keys.
        form.focus = match *screen {
            UiScreen::Login => Some(Field::Login),
            UiScreen::Register | UiScreen::Forgot => Some(Field::Email),
            UiScreen::Verify | UiScreen::Reset => Some(Field::Code),
            UiScreen::ChangeUsername => Some(Field::NewUsername),
            UiScreen::ChangePassword => Some(Field::CurrentPassword),
            _ => None,
        };
        if matches!(*screen, UiScreen::Main | UiScreen::Leaderboard) {
            form.set(Field::AnonName, account.anon_name.clone());
        }
    }
    for e in &roots {
        commands.entity(e).despawn();
    }
    match *screen {
        UiScreen::Hidden => {}
        UiScreen::Main => spawn_main(
            &mut commands,
            &theme,
            &typed,
            &settings,
            &status,
            &account,
            &form,
        ),
        UiScreen::Leaderboard => {
            spawn_leaderboard(&mut commands, &theme, &account, &form, extra.panel_size.0)
        }
        UiScreen::Pause => spawn_pause(&mut commands, &theme),
        UiScreen::Settings { from_game } => spawn_settings(
            &mut commands,
            &theme,
            &settings,
            listening.0,
            from_game,
            scroll,
        ),
        UiScreen::Login => spawn_login(&mut commands, &theme, &account, &form),
        UiScreen::Register => spawn_register(&mut commands, &theme, &account, &form),
        UiScreen::Verify => spawn_verify(&mut commands, &theme, &account, &form, now),
        UiScreen::Forgot => spawn_forgot(&mut commands, &theme, &account, &form),
        UiScreen::Reset => spawn_reset(&mut commands, &theme, &account, &form, now),
        UiScreen::Profile => spawn_profile(
            &mut commands,
            &theme,
            &account,
            &form,
            &extra.history,
            scroll,
        ),
        UiScreen::ChangeUsername => spawn_change_username(&mut commands, &theme, &account, &form),
        UiScreen::ChangePassword => spawn_change_password(&mut commands, &theme, &account, &form),
        UiScreen::Queue => spawn_queue(&mut commands, &theme, &account, &extra.cfg),
        UiScreen::Download => spawn_download(&mut commands, &theme),
    }
}

/// Top of the main panel while the server runs a newer build: what happens and the button that
/// does it. The desktop packages go up a few minutes after the server, so until the site's package
/// is the server's build the button waits. Only a protocol change greys out online play; a build
/// that changed nothing the server checks keeps playing meanwhile.
fn update_banner(c: &mut RelatedSpawnerCommands<ChildOf>, theme: &Theme, status: &SignalingStatus) {
    let web = cfg!(target_arch = "wasm32");
    let (hint, label, waiting) = if web {
        ("the page reloads to fetch it.", "Reload", false)
    } else if status.package_ready() {
        (
            "the update downloads and restarts the game.",
            "Update now",
            false,
        )
    } else {
        ("waiting for new version to deploy...", "Update now", true)
    };
    c.spawn(theme.heading_flat("A NEW VERSION IS AVAILABLE", 16.0, theme::LIGHT_RED));
    c.spawn((
        theme.label(hint, 12.0, theme::OFF_WHITE),
        Node {
            margin: UiRect::bottom(Val::Px(4.0)),
            ..default()
        },
    ));
    if waiting {
        online_button(
            c,
            theme,
            label,
            UiAction::Update,
            Some("the desktop package is not published yet; try again in a few minutes."),
        );
    } else {
        c.spawn(button(theme, label, UiAction::Update));
    }
    if !web && !status.is_outdated() {
        c.spawn((
            theme.label(
                "matches still work on this build until then.",
                12.0,
                theme::OFF_WHITE,
            ),
            Node {
                margin: UiRect::top(Val::Px(4.0)),
                ..default()
            },
        ));
    }
    c.spawn((
        theme.label("", 4.0, theme::OFF_WHITE),
        Node {
            margin: UiRect::bottom(Val::Px(6.0)),
            ..default()
        },
    ));
}

/// A tan button, greyed out with a hover tooltip when `disabled`. Returns the button entity.
fn online_button(
    c: &mut RelatedSpawnerCommands<ChildOf>,
    theme: &Theme,
    label: &str,
    action: UiAction,
    disabled: Option<&str>,
) -> Entity {
    let mut e = c.spawn(button(theme, label, action));
    let Some(tooltip_text) = disabled else {
        return e.id();
    };
    e.insert((Disabled, BackgroundColor(BTN_DISABLED)))
        .with_children(|b| {
            b.spawn((
                Tooltip,
                Visibility::Hidden,
                GlobalZIndex(10),
                theme::panel(Node {
                    position_type: PositionType::Absolute,
                    top: Val::Percent(100.0),
                    left: Val::Px(0.0),
                    padding: UiRect::axes(Val::Px(10.0), Val::Px(6.0)),
                    margin: UiRect::top(Val::Px(4.0)),
                    ..default()
                }),
                children![(theme.label(tooltip_text, 13.0, theme::LIGHT_RED), no_wrap())],
            ));
        });
    e.id()
}

/// "(N players)" inside a queue button: everyone in a game of that kind or waiting for one.
/// Nothing while the count is unknown.
fn count_label(stats: Option<Stats>, kind: QueueKind) -> String {
    let Some(s) = stats else { return String::new() };
    let n = match kind {
        QueueKind::Competitive => s.competitive.total(),
        QueueKind::Quick => s.quick.total(),
    };
    format!("({n} players)")
}

/// "N players online" under the logo: every connected client, whatever it is doing. Nothing while
/// the count is unknown.
fn online_label(stats: Option<Stats>) -> String {
    match stats {
        Some(Stats { online: 1, .. }) => "1 player online".to_string(),
        Some(s) => format!("{} players online", s.online),
        None => String::new(),
    }
}

/// The small "(N players)" text inside a queue button, right after its label.
fn count_child(
    c: &mut RelatedSpawnerCommands<ChildOf>,
    button: Entity,
    theme: &Theme,
    stats: Option<Stats>,
    kind: QueueKind,
) {
    c.commands().entity(button).with_child((
        CountText(kind),
        theme.heading_flat(count_label(stats, kind), 12.0, theme::BTN_TEXT),
        no_wrap(),
        Node {
            margin: UiRect::left(Val::Px(8.0)),
            ..default()
        },
    ));
}

/// Top-right corner of the title screen: who you are. Logged in: the class icon and the account
/// name (click for the profile). Logged out: the log in button and the anonymous name box.
fn spawn_identity(
    p: &mut RelatedSpawnerCommands<ChildOf>,
    theme: &Theme,
    account: &Account,
    form: &Form,
) {
    p.spawn(Node {
        position_type: PositionType::Absolute,
        top: Val::Px(14.0),
        right: Val::Px(16.0),
        flex_direction: FlexDirection::Column,
        align_items: AlignItems::FlexEnd,
        row_gap: Val::Px(4.0),
        ..default()
    })
    .with_children(|c| {
        if let Some(user) = &account.user {
            c.spawn((
                Button,
                UiAction::OpenProfile,
                Node {
                    flex_direction: FlexDirection::Row,
                    align_items: AlignItems::Center,
                    column_gap: Val::Px(8.0),
                    padding: UiRect::axes(Val::Px(10.0), Val::Px(6.0)),
                    border_radius: BorderRadius::all(Val::Px(6.0)),
                    ..default()
                },
                BackgroundColor(Color::NONE),
            ))
            .with_children(|b| {
                b.spawn(theme.soldier_icon(26.0));
                b.spawn((
                    theme.heading(user.username.clone(), 22.0, theme::TAN_LIGHT),
                    no_wrap(),
                ));
            });
            c.spawn((
                theme.label(format!("{} ELO", user.elo), 13.0, theme::OFF_WHITE),
                Node {
                    margin: UiRect::right(Val::Px(10.0)),
                    ..default()
                },
            ));
        } else {
            c.spawn(theme.button("[ log in ]", UiAction::OpenLogin, 170.0, 38.0, 15.0));
            spawn_field(c, theme, form, Field::AnonName, "name", 200.0);
            c.spawn((
                theme.label("playing as", 12.0, theme::OFF_WHITE),
                Node {
                    margin: UiRect::right(Val::Px(4.0)),
                    ..default()
                },
            ));
        }
    });
}

/// Height kept free for the error line between the logo and the panel, whether or not there is
/// one, so a message appearing does not move the panel.
const ERROR_SLOT_H: f32 = 22.0;

/// The title screen, the same on each of its tabs: the backdrop, the corner furniture (build
/// identity, identity corner, the download button on the web) and, in a column, the logo with
/// the online count under it and the error slot. Returns that column; the caller adds its panel.
///
/// The column sits between two spacers that share the free height, which centres it while it
/// fits. In a window too short for it the spacers are squeezed to nothing and the root's
/// `JustifyContent::FlexEnd` keeps the column's bottom in view: the panel stays whole and the
/// logo is what gets cut off at the top. (Auto margins would be the usual way to centre, but
/// taffy 0.10 hands them the free space and then shifts the item to the end as well.)
fn title_screen(commands: &mut Commands, theme: &Theme, account: &Account, form: &Form) -> Entity {
    let root = commands
        .spawn((
            UiRoot,
            Node {
                width: Val::Percent(100.0),
                height: Val::Percent(100.0),
                flex_direction: FlexDirection::Column,
                justify_content: JustifyContent::FlexEnd,
                align_items: AlignItems::Center,
                padding: UiRect::all(Val::Px(SCREEN_MARGIN)),
                overflow: Overflow::clip(),
                ..default()
            },
            GlobalZIndex(10),
        ))
        .id();
    commands.entity(root).with_children(|p| {
        // First child so it draws beneath everything; absolute, so it joins no layout.
        p.spawn(theme.menu_backdrop());
        // Build and protocol identity in the corner, so two people can tell at a glance whether
        // they match.
        p.spawn((
            theme.label(
                format!(
                    "build {} ({})",
                    endif_sim::BUILD_ID,
                    endif_sim::protocol_id()
                ),
                11.0,
                theme::TAN_DARK,
            ),
            Node {
                position_type: PositionType::Absolute,
                right: Val::Px(10.0),
                bottom: Val::Px(8.0),
                ..default()
            },
        ));
        // Web only: the desktop builds are served next to the page (`/download/<platform>`).
        #[cfg(target_arch = "wasm32")]
        p.spawn(Node {
            position_type: PositionType::Absolute,
            left: Val::Px(6.0),
            bottom: Val::Px(4.0),
            ..default()
        })
        .with_children(|w| {
            w.spawn(theme.button(
                "download the desktop app",
                UiAction::OpenDownload,
                220.0,
                34.0,
                12.0,
            ));
        });
        spawn_identity(p, theme, account, form);
    });
    let spacer = || {
        (
            Node {
                flex_grow: 1.0,
                flex_shrink: 1.0,
                flex_basis: Val::Px(0.0),
                min_height: Val::Px(0.0),
                ..default()
            },
            ChildOf(root),
        )
    };
    commands.spawn(spacer());
    let column = commands
        .spawn((
            Node {
                flex_direction: FlexDirection::Column,
                align_items: AlignItems::Center,
                row_gap: Val::Px(6.0),
                // Never squeezed: it overflows (at the top) rather than crushing its contents.
                flex_shrink: 0.0,
                ..default()
            },
            ChildOf(root),
        ))
        .id();
    commands.spawn(spacer());
    commands.entity(column).with_children(|p| {
        // The logo, with the number of connected clients tucked under its right-hand end.
        p.spawn(Node {
            flex_direction: FlexDirection::Column,
            align_items: AlignItems::FlexEnd,
            margin: UiRect::bottom(Val::Px(8.0)),
            ..default()
        })
        .with_children(|l| {
            l.spawn(theme.heading("endif.tf", 96.0, theme::ORANGE));
            // The logo's line box leaves a lot of air under the glyphs; pull the line up into it.
            l.spawn((
                OnlineText,
                theme.label(online_label(account.stats), 13.0, theme::OFF_WHITE),
                no_wrap(),
                Node {
                    margin: UiRect {
                        right: Val::Px(4.0),
                        top: Val::Px(-14.0),
                        ..default()
                    },
                    ..default()
                },
            ));
        });
        p.spawn(Node {
            min_height: Val::Px(ERROR_SLOT_H),
            justify_content: JustifyContent::Center,
            align_items: AlignItems::Center,
            ..default()
        })
        .with_children(|s| {
            s.spawn((
                theme.label(
                    account.error.clone().unwrap_or_default(),
                    14.0,
                    theme::LIGHT_RED,
                ),
                no_wrap(),
            ));
        });
    });
    column
}
/// The bookmark tabs on the right edge of a title panel, `selected` being the page on show. They
/// are absolute children of the panel, so the panel stays centred and the tabs jut out of it.
/// Insets resolve inside the border: at `left: 100%` a tab starts on the inner edge of the
/// panel's right border, so the selected one lies over that border and the panel opens into
/// it; the others step past it and keep the border between them and the panel.
///
/// The panel's own right border is left out and redrawn in two pieces, above and below the
/// selected tab: the tab is as see-through as the panel, and the opaque border under it would
/// show through as a faint line. The panel's background stops at the border, so what is under
/// the tab in the gap is the backdrop, the same as under the rest of it.
fn spawn_tabs(c: &mut RelatedSpawnerCommands<ChildOf>, theme: &Theme, selected: Tab) {
    let panel = c.target_entity();
    c.commands().entity(panel).insert(BorderColor {
        right: Color::NONE,
        ..BorderColor::all(theme::TAN_DARK)
    });
    let sel = Tab::ALL.iter().position(|&t| t == selected).unwrap_or(0) as f32;
    let gap_top = TAB_TOP + sel * (TAB_H + TAB_GAP);
    // A piece's border thins to nothing over its last `PANEL_BORDER` rows, where its bare end
    // is the nearer edge; each runs that far past the gap, under the tab's own opaque border.
    // Widths keep the far edge out of that reckoning and fit the corner's curve.
    let piece = |top: Val, bottom: Val, height: Val, radius: BorderRadius, border: UiRect| {
        (
            Node {
                position_type: PositionType::Absolute,
                right: Val::Px(-PANEL_BORDER),
                width: Val::Px(2.0 * theme::PANEL_RADIUS),
                top,
                bottom,
                height,
                border,
                border_radius: radius,
                ..default()
            },
            BorderColor {
                right: theme::TAN_DARK,
                ..BorderColor::all(Color::NONE)
            },
        )
    };
    let b = Val::Px(PANEL_BORDER);
    c.spawn(piece(
        Val::Px(-PANEL_BORDER),
        Val::Auto,
        Val::Px(PANEL_BORDER + gap_top + PANEL_BORDER),
        BorderRadius::top_right(Val::Px(theme::PANEL_RADIUS)),
        UiRect {
            top: b,
            right: b,
            ..UiRect::ZERO
        },
    ));
    c.spawn(piece(
        Val::Px(gap_top + TAB_H - PANEL_BORDER),
        Val::Px(-PANEL_BORDER),
        Val::Auto,
        BorderRadius::bottom_right(Val::Px(theme::PANEL_RADIUS)),
        UiRect {
            bottom: b,
            right: b,
            ..UiRect::ZERO
        },
    ));
    for (i, tab) in Tab::ALL.into_iter().enumerate() {
        let on = tab == selected;
        let (image, icon, rest, hover) = match tab {
            Tab::Menu => (
                theme.tf2_logo.clone(),
                TAB_ICON,
                if on {
                    theme::TAN_LIGHT
                } else {
                    theme::TAN_DARK
                },
                theme::TAN_LIGHT,
            ),
            Tab::Leaderboard => (
                theme.trophy.clone(),
                TAB_ICON + 4.0,
                if on {
                    Color::WHITE
                } else {
                    Color::srgb(0.6, 0.6, 0.6)
                },
                Color::WHITE,
            ),
        };
        let mut e = c.spawn((
            TabNode { off: !on },
            // Hover tracking; the page on show is not a button (nothing to open, no sounds).
            Interaction::default(),
            ImageNode {
                image: if on {
                    theme.tab_on.clone()
                } else {
                    theme.tab_off.clone()
                },
                image_mode: NodeImageMode::Stretch,
                ..default()
            },
            Node {
                position_type: PositionType::Absolute,
                left: Val::Percent(100.0),
                top: Val::Px(TAB_TOP + i as f32 * (TAB_H + TAB_GAP)),
                // Both end the same distance out: the selected one also covers the border.
                width: Val::Px(if on { TAB_W + PANEL_BORDER } else { TAB_W }),
                height: Val::Px(TAB_H),
                margin: UiRect::left(Val::Px(if on { 0.0 } else { PANEL_BORDER })),
                justify_content: JustifyContent::Center,
                align_items: AlignItems::Center,
                ..default()
            },
        ));
        if !on {
            // `ui_buttons` wants a `BackgroundColor`; the tab's look is its image, so it stays clear.
            e.insert((Button, UiAction::OpenTab(tab), BackgroundColor(Color::NONE)));
        }
        e.with_children(|t| {
            t.spawn((TabIcon { rest, hover }, Theme::icon(image, icon, rest)));
            // The tab's name, to its right while hovered.
            t.spawn((
                Tooltip,
                Visibility::Hidden,
                GlobalZIndex(10),
                Node {
                    position_type: PositionType::Absolute,
                    left: Val::Percent(100.0),
                    top: Val::Px(0.0),
                    bottom: Val::Px(0.0),
                    margin: UiRect::left(Val::Px(8.0)),
                    align_items: AlignItems::Center,
                    ..default()
                },
            ))
            .with_children(|w| {
                w.spawn((
                    theme::panel(Node {
                        padding: UiRect::axes(Val::Px(10.0), Val::Px(6.0)),
                        ..default()
                    }),
                    children![(theme.label(tab.name(), 13.0, theme::OFF_WHITE), no_wrap())],
                ));
            });
        });
    }
}

/// Shows a tab's name while the mouse is over it, and brightens an unselected tab and its icon.
/// The tab's artwork sits on the entity with the `Interaction`, the icon and the name on its
/// children; `TabNode` keeps the queries apart.
fn tab_hover(
    mut tabs: Query<(&Interaction, &Children, &mut ImageNode, &TabNode), Changed<Interaction>>,
    mut icons: Query<(&TabIcon, &mut ImageNode), Without<TabNode>>,
    mut tips: Query<&mut Visibility, With<Tooltip>>,
) {
    for (interaction, children, mut art, tab) in &mut tabs {
        let hovered = *interaction != Interaction::None;
        if tab.off {
            art.color = if hovered {
                TAB_HOVER_TINT
            } else {
                Color::WHITE
            };
        }
        for child in children.iter() {
            if let Ok((icon, mut node)) = icons.get_mut(child)
                && tab.off
            {
                node.color = if hovered { icon.hover } else { icon.rest };
            }
            if let Ok(mut vis) = tips.get_mut(child) {
                *vis = if hovered {
                    Visibility::Visible
                } else {
                    Visibility::Hidden
                };
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn spawn_main(
    commands: &mut Commands,
    theme: &Theme,
    typed: &TypedCode,
    s: &Settings,
    status: &SignalingStatus,
    account: &Account,
    form: &Form,
) {
    let offline = if status.is_outdated() {
        Some(if cfg!(target_arch = "wasm32") {
            "this build is out of date: reload the page (Ctrl+Shift+R)."
        } else {
            "this build is out of date: update the client."
        })
    } else if status.is_down() {
        Some("cannot connect to matchmaking servers.")
    } else {
        None
    };
    let ranked_offline =
        offline.or((!account.logged_in()).then_some("log in to play matchmaking."));
    let column = title_screen(commands, theme, account, form);
    commands.entity(column).with_children(|p| {
        p.spawn((panel_column(18.0), TitlePanel))
            .with_children(|c| {
                spawn_tabs(c, theme, Tab::Menu);
                if status.update_available() {
                    update_banner(c, theme, status);
                }
                let quick = online_button(c, theme, "Quick play", UiAction::QuickPlay, offline);
                count_child(c, quick, theme, account.stats, QueueKind::Quick);
                let competitive = online_button(
                    c,
                    theme,
                    "Competitive",
                    UiAction::Competitive,
                    ranked_offline,
                );
                count_child(c, competitive, theme, account.stats, QueueKind::Competitive);
                c.spawn(button(theme, "Practice (offline)", UiAction::Practice));
                online_button(
                    c,
                    theme,
                    "Create private room",
                    UiAction::CreateRoom,
                    offline,
                );

                c.spawn((
                    theme.heading_flat("JOIN A ROOM", 20.0, theme::ORANGE),
                    Node {
                        margin: UiRect::new(
                            Val::Px(0.0),
                            Val::Px(0.0),
                            Val::Px(10.0),
                            Val::Px(2.0),
                        ),
                        ..default()
                    },
                ));
                c.spawn(theme.label("type the six letter code", 13.0, theme::OFF_WHITE));
                c.spawn(Node {
                    width: Val::Px(BUTTON_W - 8.0),
                    flex_direction: FlexDirection::Row,
                    align_items: AlignItems::Center,
                    column_gap: Val::Px(6.0),
                    margin: UiRect::vertical(Val::Px(4.0)),
                    ..default()
                })
                .with_children(|r| {
                    r.spawn(theme::inset(Node {
                        flex_grow: 1.0,
                        height: Val::Px(54.0),
                        justify_content: JustifyContent::Center,
                        align_items: AlignItems::Center,
                        ..default()
                    }))
                    .with_children(|b| {
                        // Six of the widest glyph (`W W W W W W`) must still fit on one line: the
                        // box is ~238 px wide, and wrapping would break the code across two rows.
                        b.spawn((
                            CodeField,
                            theme.heading_flat(
                                code_display(&typed.0, ROOM_CODE_LEN),
                                30.0,
                                theme::YELLOW,
                            ),
                            no_wrap(),
                        ));
                    });
                    r.spawn(small_button(theme, "paste", UiAction::Paste));
                });
                if status.is_outdated() {
                    c.spawn((
                        theme.label(
                            if cfg!(target_arch = "wasm32") {
                                "a newer build is available: reload the page"
                            } else {
                                "a newer build is available: update the client"
                            },
                            13.0,
                            theme::LIGHT_RED,
                        ),
                        Node {
                            margin: UiRect::top(Val::Px(8.0)),
                            ..default()
                        },
                    ));
                }
                online_button(c, theme, "Join room", UiAction::JoinRoom, offline);
                c.spawn(button(theme, "Settings", UiAction::OpenSettings));

                // Master volume, always within reach from the title screen. The slider's track has
                // its own headroom above the bar, so the label sits right on top of it.
                c.spawn((
                    theme.heading_flat("VOLUME", 16.0, theme::TAN_LIGHT),
                    Node {
                        margin: UiRect::new(
                            Val::Px(0.0),
                            Val::Px(0.0),
                            Val::Px(10.0),
                            Val::Px(-6.0),
                        ),
                        ..default()
                    },
                ));
                c.spawn(Node {
                    width: Val::Px(BUTTON_W),
                    flex_direction: FlexDirection::Row,
                    align_items: AlignItems::Center,
                    justify_content: JustifyContent::SpaceBetween,
                    ..default()
                })
                .with_children(|r| {
                    slider_controls(r, theme, s, Slider::Volume, BUTTON_W - 84.0 - 12.0)
                });
            });
    });
    if let Some(result) = &account.result {
        spawn_result_popup(commands, theme, result);
    }
}

/// Leaderboard column widths: place, rating, record, win rate. Each is just wide enough for its
/// widest realistic contents ("#99", "9999", "120 - 95", "100%" and the headings) so that the
/// name, which takes what is left (~146 px), has room for a 20-character username.
const LB_COLS: [f32; 4] = [28.0, 32.0, 46.0, 32.0];
const LB_GAP: f32 = 4.0;
/// Horizontal padding inside the table box.
const LB_PAD_X: f32 = 8.0;

/// Height of a leaderboard row (and of the column headings).
const LB_ROW_H: f32 = 26.0;
/// Vertical padding inside the table box.
const LB_PAD_Y: f32 = 6.0;
/// Most players ever asked for on one page (the server caps it there too).
const LB_ROWS_MAX: u32 = 50;

/// A row of the leaderboard table, as wide as the panel's contents.
fn lb_row() -> Node {
    Node {
        width: Val::Percent(100.0),
        min_height: Val::Px(LB_ROW_H),
        flex_direction: FlexDirection::Row,
        align_items: AlignItems::Center,
        column_gap: Val::Px(LB_GAP),
        ..default()
    }
}

/// One line of the leaderboard: place, name, rating, record and win rate. `me` lights up the
/// viewer's own line.
fn leaderboard_row(
    c: &mut RelatedSpawnerCommands<ChildOf>,
    theme: &Theme,
    e: &LeaderboardEntry,
    me: bool,
) {
    let place = if e.rank == 1 {
        theme::YELLOW
    } else {
        theme::TAN_LIGHT
    };
    let name = if me { theme::YELLOW } else { theme::OFF_WHITE };
    c.spawn(lb_row()).with_children(|r| {
        cell(
            r,
            LB_COLS[0],
            theme.heading_flat(format!("#{}", e.rank), 14.0, place),
            JustifyContent::FlexStart,
        );
        // `min_width: 0` lets a long name shrink (and clip) rather than shove the columns after it.
        r.spawn(Node {
            flex_grow: 1.0,
            flex_basis: Val::Px(0.0),
            min_width: Val::Px(0.0),
            overflow: Overflow::clip(),
            ..default()
        })
        .with_children(|x| {
            x.spawn((theme.label(e.username.clone(), 14.0, name), no_wrap()));
        });
        cell(
            r,
            LB_COLS[1],
            theme.heading_flat(e.elo.to_string(), 15.0, theme::TAN_LIGHT),
            JustifyContent::FlexEnd,
        );
        // Wins - losses, each in its colour.
        r.spawn(Node {
            width: Val::Px(LB_COLS[2]),
            justify_content: JustifyContent::FlexEnd,
            column_gap: Val::Px(3.0),
            flex_shrink: 0.0,
            ..default()
        })
        .with_children(|x| {
            x.spawn((
                theme.heading_flat(e.wins.to_string(), 13.0, theme::YELLOW),
                no_wrap(),
            ));
            x.spawn((theme.heading_flat("-", 13.0, theme::TAN_DARK), no_wrap()));
            x.spawn((
                theme.heading_flat(e.losses.to_string(), 13.0, theme::LIGHT_RED),
                no_wrap(),
            ));
        });
        cell(
            r,
            LB_COLS[3],
            theme.label(format!("{:.0}%", e.win_rate()), 13.0, theme::OFF_WHITE),
            JustifyContent::FlexEnd,
        );
    });
}

/// Records the main menu panel's size (`TitlePanelSize`) and, on the leaderboard tab, how many
/// rows its table box has room for (`LeaderboardRows`), once they have been laid out.
fn measure_title_panel(
    panels: Query<&ComputedNode, With<TitlePanel>>,
    tables: Query<&ComputedNode, With<LeaderboardTable>>,
    mut size: ResMut<TitlePanelSize>,
    mut rows: ResMut<LeaderboardRows>,
) {
    if let Some(node) = panels.iter().next()
        && node.size.x > 0.0
        && node.size.y > 0.0
    {
        let want = node.size * node.inverse_scale_factor;
        if size.0 != Some(want) {
            size.0 = Some(want);
        }
    }
    if let Some(node) = tables.iter().next()
        && node.size.y > 0.0
    {
        let inner = node.size.y * node.inverse_scale_factor - 2.0 * LB_PAD_Y - 4.0;
        let want = ((inner / LB_ROW_H).floor() as u32).clamp(1, LB_ROWS_MAX);
        if rows.0 != Some(want) {
            rows.0 = Some(want);
        }
    }
}

/// Fetches the first page once the leaderboard tab has been laid out and its row count is known
/// (opening the tab cannot ask before then). A failure leaves its message and does not retry.
fn leaderboard_fetch(
    mut account: ResMut<Account>,
    screen: Res<UiScreen>,
    rows: Res<LeaderboardRows>,
    cfg: Res<ClientConfig>,
) {
    let Some(per) = rows.0 else { return };
    if *screen == UiScreen::Leaderboard
        && account.leaderboard.is_none()
        && account.error.is_none()
        && !account.loading_leaderboard()
    {
        account.fetch_leaderboard(&cfg, 1, per);
    }
}

/// The title screen's leaderboard tab: the top players by rating, in a panel the size of the
/// main menu's (`size`), so the frame does not move when the tabs switch.
fn spawn_leaderboard(
    commands: &mut Commands,
    theme: &Theme,
    account: &Account,
    form: &Form,
    size: Option<Vec2>,
) {
    let me = account.user.as_ref().map(|u| u.username.as_str());
    let (width, height) = match size {
        Some(s) => (Val::Px(s.x), Val::Px(s.y)),
        // Never measured (cannot happen: the main menu comes first): the main panel's usual width.
        None => (Val::Px(BUTTON_W + 48.0), Val::Auto),
    };
    let column = title_screen(commands, theme, account, form);
    commands.entity(column).with_children(|p| {
        p.spawn(theme::panel(Node {
            width,
            height,
            // The main panel cannot shrink below its contents; this one must not shrink below
            // its copied height either, or the two would sit differently in a short window.
            flex_shrink: 0.0,
            flex_direction: FlexDirection::Column,
            align_items: AlignItems::Center,
            padding: UiRect::all(Val::Px(18.0)),
            row_gap: Val::Px(4.0),
            ..default()
        }))
        .with_children(|c| {
            spawn_tabs(c, theme, Tab::Leaderboard);
            c.spawn((
                theme.heading("LEADERBOARD", 30.0, theme::TAN_LIGHT),
                Node {
                    margin: UiRect::bottom(Val::Px(6.0)),
                    ..default()
                },
            ));
            // Column headings, lined up with the rows below (the box has a 2 px border and `LB_PAD_X`
            // of side padding).
            c.spawn(Node {
                width: Val::Percent(100.0),
                padding: UiRect::horizontal(Val::Px(LB_PAD_X + 2.0)),
                ..default()
            })
            .with_children(|w| {
                w.spawn(lb_row()).with_children(|r| {
                    let head = |r: &mut RelatedSpawnerCommands<ChildOf>,
                                w: f32,
                                s: &str,
                                j: JustifyContent| {
                        cell(r, w, theme.heading_flat(s, 11.0, theme::TAN_DARK), j)
                    };
                    head(r, LB_COLS[0], "#", JustifyContent::FlexStart);
                    r.spawn(Node {
                        flex_grow: 1.0,
                        flex_basis: Val::Px(0.0),
                        min_width: Val::Px(0.0),
                        ..default()
                    })
                    .with_children(|x| {
                        x.spawn((
                            theme.heading_flat("PLAYER", 11.0, theme::TAN_DARK),
                            no_wrap(),
                        ));
                    });
                    head(r, LB_COLS[1], "ELO", JustifyContent::FlexEnd);
                    head(r, LB_COLS[2], "W - L", JustifyContent::FlexEnd);
                    head(r, LB_COLS[3], "WIN%", JustifyContent::FlexEnd);
                });
            });
            // The table fills the rest of the panel; its height decides the players to a page.
            c.spawn((
                LeaderboardTable,
                theme::inset(Node {
                    width: Val::Percent(100.0),
                    flex_grow: 1.0,
                    min_height: Val::Px(LB_ROW_H + 2.0 * LB_PAD_Y + 4.0),
                    flex_direction: FlexDirection::Column,
                    align_items: AlignItems::Center,
                    padding: UiRect::axes(Val::Px(LB_PAD_X), Val::Px(LB_PAD_Y)),
                    overflow: Overflow::clip(),
                    ..default()
                }),
            ))
            .with_children(|list| {
                let note = |list: &mut RelatedSpawnerCommands<ChildOf>, s: &str| {
                    list.spawn((
                        theme.label(s, 14.0, theme::TAN_DARK),
                        Node {
                            margin: UiRect::vertical(Val::Px(12.0)),
                            ..default()
                        },
                    ));
                };
                match &account.leaderboard {
                    Some(lb) if lb.players.is_empty() => {
                        note(list, "nobody has played a competitive match yet")
                    }
                    Some(lb) => {
                        for e in &lb.players {
                            leaderboard_row(list, theme, e, me == Some(e.username.as_str()));
                        }
                    }
                    // A failure is on the error line above the panel.
                    None if account.error.is_some() => note(list, ""),
                    None => note(list, "loading..."),
                }
            });
            // Page buttons: previous, "page N of M", next. The ends are greyed out.
            let (page, pages) = account
                .leaderboard
                .as_ref()
                .map(|lb| (lb.page, lb.pages))
                .unwrap_or((1, 1));
            c.spawn(Node {
                flex_direction: FlexDirection::Row,
                align_items: AlignItems::Center,
                column_gap: Val::Px(10.0),
                margin: UiRect::top(Val::Px(4.0)),
                ..default()
            })
            .with_children(|r| {
                let mut prev = r.spawn(small_button(
                    theme,
                    "<",
                    UiAction::LeaderboardPage(page.saturating_sub(1).max(1)),
                ));
                if page <= 1 {
                    prev.insert((Disabled, BackgroundColor(BTN_DISABLED)));
                }
                r.spawn((
                    theme.label(format!("page {page} of {pages}"), 13.0, theme::OFF_WHITE),
                    no_wrap(),
                    Node {
                        min_width: Val::Px(90.0),
                        justify_content: JustifyContent::Center,
                        ..default()
                    },
                ));
                let mut next = r.spawn(small_button(
                    theme,
                    ">",
                    UiAction::LeaderboardPage((page + 1).min(pages)),
                ));
                if page >= pages {
                    next.insert((Disabled, BackgroundColor(BTN_DISABLED)));
                }
            });
        });
    });
}

/// The popup over the main menu after a ranked match: how it ended and what it did to the rating.
fn spawn_result_popup(commands: &mut Commands, theme: &Theme, result: &RankedResult) {
    let (title, color) = match result.ending {
        Ending::Won => ("YOU WON!", theme::YELLOW),
        Ending::OpponentLeft => ("OPPONENT LEFT! YOU WON!", theme::YELLOW),
        Ending::Lost => ("YOU LOST!", theme::LIGHT_RED),
        Ending::WeLeft => ("YOU LEFT! YOU LOST!", theme::LIGHT_RED),
    };
    let note = match result.rating {
        Rating::Pending => "updating rating...",
        Rating::Settled(_) => "",
        Rating::Void => "match voided: the two results did not agree, so ratings are unchanged",
        Rating::Unconfirmed => "rating not confirmed yet; check your profile in a moment",
    };
    let root = screen_root(commands, theme, true);
    commands
        .entity(root)
        .insert(GlobalZIndex(20))
        .with_children(|p| {
            p.spawn(panel_column(26.0)).with_children(|c| {
                c.spawn((
                    theme.heading(title, 30.0, color),
                    Node {
                        margin: UiRect::bottom(Val::Px(8.0)),
                        ..default()
                    },
                ));
                if let Rating::Settled(d) = result.rating {
                    c.spawn((
                        theme.heading_flat(format!("{d:+} ELO"), 34.0, color),
                        Node {
                            margin: UiRect::bottom(Val::Px(16.0)),
                            ..default()
                        },
                    ));
                } else {
                    c.spawn((
                        theme.label(note, 14.0, theme::OFF_WHITE),
                        Node {
                            margin: UiRect::bottom(Val::Px(16.0)),
                            max_width: Val::Px(440.0),
                            ..default()
                        },
                    ));
                }
                c.spawn(theme.button("OK", UiAction::ResultOk, 160.0, BUTTON_H, 18.0));
            });
        });
}

fn spawn_pause(commands: &mut Commands, theme: &Theme) {
    let root = screen_root(commands, theme, true);
    commands.entity(root).with_children(|p| {
        p.spawn(panel_column(22.0)).with_children(|c| {
            c.spawn(theme.heading("PAUSED", 44.0, theme::TAN_LIGHT));
            c.spawn((
                theme.label("the match keeps running", 13.0, theme::OFF_WHITE),
                Node {
                    margin: UiRect::bottom(Val::Px(12.0)),
                    ..default()
                },
            ));
            c.spawn(button(theme, "Resume", UiAction::Resume));
            c.spawn(button(theme, "Settings", UiAction::OpenSettings));
            c.spawn(button(theme, "Leave match", UiAction::Leave));
        });
    });
}

/// Whether any action sits on a Ctrl key, the one the browser pairs with W to close the tab.
fn binds_ctrl(s: &Settings) -> bool {
    Action::ALL.iter().any(|a| {
        matches!(
            s.bindings.get(*a),
            Binding::Key(KeyCode::ControlLeft | KeyCode::ControlRight)
        )
    })
}

/// Explains what the fullscreen toggle buys in this browser (web only).
fn fullscreen_note(s: &Settings) -> Option<&'static str> {
    if !cfg!(target_arch = "wasm32") {
        return None;
    }
    Some(if crate::webclip::can_lock_keyboard() {
        if s.fullscreen {
            "in fullscreen Ctrl+W and friends stay in the game; hold Esc to leave it"
        } else {
            "in fullscreen this browser can keep Ctrl+W and friends in the game"
        }
    } else {
        "this browser cannot keep Ctrl+W from closing the tab, fullscreen or not"
    })
}

/// Warns when something is bound to Ctrl in a browser that will act on Ctrl+W (web only). The
/// beforeunload prompt catches the close, but the match is still interrupted by a dialog.
fn ctrl_note(s: &Settings) -> Option<&'static str> {
    if !cfg!(target_arch = "wasm32") || !binds_ctrl(s) {
        return None;
    }
    if crate::webclip::can_lock_keyboard() {
        (!s.fullscreen).then_some("Ctrl+W tries to close the tab outside fullscreen: turn on fullscreen on play, or use Shift")
    } else {
        Some(
            "Ctrl+W tries to close the tab in this browser (a leave-page prompt catches it): Shift is safer",
        )
    }
}

fn spawn_settings(
    commands: &mut Commands,
    theme: &Theme,
    s: &Settings,
    listening: Option<Action>,
    from_game: bool,
    scroll: Vec2,
) {
    let root = screen_root(commands, theme, from_game);
    commands.entity(root).with_children(|p| {
        p.spawn(scrolling_panel_column(22.0, scroll))
            .with_children(|c| {
                c.spawn((
                    theme.heading("SETTINGS", 40.0, theme::TAN_LIGHT),
                    Node {
                        margin: UiRect::bottom(Val::Px(6.0)),
                        ..default()
                    },
                ));

                c.spawn(section(theme, "mouse"));
                if s.separate_sensitivity {
                    row(c, theme, "Sensitivity X", |b| {
                        slider_controls(b, theme, s, Slider::Sens(Axis::X), SLIDER_W)
                    });
                    row(c, theme, "Sensitivity Y", |b| {
                        slider_controls(b, theme, s, Slider::Sens(Axis::Y), SLIDER_W)
                    });
                } else {
                    row(c, theme, "Sensitivity", |b| {
                        slider_controls(b, theme, s, Slider::Sens(Axis::X), SLIDER_W)
                    });
                }
                row(c, theme, "Separate X / Y sensitivity", |b| {
                    b.spawn(toggle(
                        theme,
                        s.separate_sensitivity,
                        UiAction::SeparateSensitivity,
                    ));
                });
                row(c, theme, "Invert Y look", |b| {
                    b.spawn(toggle(theme, s.invert_y, UiAction::InvertY));
                });
                c.spawn((
                    theme.label(
                        format!("{:.3} deg per count at TF2 m_yaw 0.022", s.yaw_per_count()),
                        12.0,
                        theme::TAN_DARK,
                    ),
                    Node {
                        width: Val::Px(ROW_W),
                        margin: UiRect::bottom(Val::Px(6.0)),
                        ..default()
                    },
                ));

                c.spawn(section(theme, "loadout"));
                row(c, theme, "Preferred rocket launcher", |b| {
                    b.spawn(icon_switch(
                        [
                            theme.launcher_stock.clone(),
                            theme.launcher_original.clone(),
                        ],
                        s.weapon == Weapon::Stock,
                        UiAction::Launcher,
                    ));
                });

                c.spawn(section(theme, "audio"));
                row(c, theme, "Volume", |b| {
                    slider_controls(b, theme, s, Slider::Volume, SLIDER_W)
                });

                c.spawn(section(theme, "network"));
                row(c, theme, "Adaptive input delay", |b| {
                    b.spawn(toggle(theme, s.adaptive_delay, UiAction::AdaptiveDelay));
                });
                if !s.adaptive_delay {
                    row(c, theme, "Input delay", |b| {
                        slider_controls(b, theme, s, Slider::InputDelay, SLIDER_W)
                    });
                }
                if cfg!(target_arch = "wasm32") {
                    c.spawn(section(theme, "video"));
                    row(c, theme, "Fullscreen on play", |b| {
                        b.spawn(toggle(theme, s.fullscreen, UiAction::Fullscreen));
                    });
                    if let Some(note) = fullscreen_note(s) {
                        c.spawn((
                            theme.label(note, 12.0, theme::TAN_DARK),
                            Node {
                                width: Val::Px(ROW_W),
                                margin: UiRect::bottom(Val::Px(6.0)),
                                ..default()
                            },
                        ));
                    }
                }

                c.spawn(section(theme, "keys"));
                for a in Action::ALL {
                    let value = if listening == Some(a) {
                        "PRESS A KEY".to_string()
                    } else {
                        s.bindings.get(a).label()
                    };
                    row(c, theme, a.label(), |b| {
                        b.spawn((theme.heading_flat(value, 16.0, theme::YELLOW), no_wrap()));
                        let mut e = b.spawn(small_button(theme, "bind", UiAction::Bind(a)));
                        if listening == Some(a) {
                            e.insert(BackgroundColor(theme::BTN_ACTIVE));
                        }
                    });
                    if a == Action::Crouch
                        && let Some(note) = ctrl_note(s)
                    {
                        c.spawn((
                            theme.label(note, 12.0, theme::TAN_DARK),
                            Node {
                                width: Val::Px(ROW_W),
                                margin: UiRect::bottom(Val::Px(6.0)),
                                ..default()
                            },
                        ));
                    }
                }

                c.spawn(Node {
                    flex_direction: FlexDirection::Row,
                    margin: UiRect::top(Val::Px(14.0)),
                    ..default()
                })
                .with_children(|b| {
                    b.spawn(theme.button(
                        "Reset defaults",
                        UiAction::ResetDefaults,
                        200.0,
                        BUTTON_H,
                        16.0,
                    ));
                    b.spawn(theme.button("Back", UiAction::Back, 200.0, BUTTON_H, 16.0));
                });
                c.spawn(theme.label("Esc goes back", 12.0, theme::TAN_DARK));
            });
    });
}

// ------------------------------------------------------------------------------------ account screens

/// A titled panel on the title background with a back arrow in its corner and the account status
/// line under the fields.
fn form_screen(
    commands: &mut Commands,
    theme: &Theme,
    account: &Account,
    title: &str,
    build: impl FnOnce(&mut RelatedSpawnerCommands<ChildOf>),
) {
    let root = screen_root(commands, theme, false);
    commands.entity(root).with_children(|p| {
        p.spawn(panel_column(24.0)).with_children(|c| {
            back_arrow(c, theme);
            // Side margins keep a wide title clear of the arrow.
            c.spawn((
                theme.heading(title, 34.0, theme::TAN_LIGHT),
                Node {
                    margin: UiRect::new(Val::Px(36.0), Val::Px(36.0), Val::Px(0.0), Val::Px(8.0)),
                    ..default()
                },
            ));
            build(c);
            status_line(c, theme, account);
        });
    });
}

/// The small "<" in the top-left corner of a form window; Esc does the same.
fn back_arrow(c: &mut RelatedSpawnerCommands<ChildOf>, theme: &Theme) {
    c.spawn((
        Button,
        UiAction::BackArrow,
        Node {
            position_type: PositionType::Absolute,
            top: Val::Px(8.0),
            left: Val::Px(8.0),
            width: Val::Px(32.0),
            height: Val::Px(32.0),
            justify_content: JustifyContent::Center,
            align_items: AlignItems::Center,
            border_radius: BorderRadius::all(Val::Px(6.0)),
            ..default()
        },
        BackgroundColor(Color::NONE),
        children![(theme.heading_flat("<", 24.0, theme::OFF_WHITE), no_wrap())],
    ));
}

/// "working..." / the last error / a notice, with a fixed height so the layout does not jump.
fn status_line(c: &mut RelatedSpawnerCommands<ChildOf>, theme: &Theme, account: &Account) {
    let (text, color) = if account.busy() {
        ("working...".to_string(), theme::TAN_LIGHT)
    } else if let Some(e) = &account.error {
        (e.clone(), theme::LIGHT_RED)
    } else if let Some(n) = &account.notice {
        (n.clone(), theme::OFF_WHITE)
    } else {
        (String::new(), theme::OFF_WHITE)
    };
    c.spawn(Node {
        min_height: Val::Px(24.0),
        max_width: Val::Px(FIELD_W + 160.0),
        justify_content: JustifyContent::Center,
        margin: UiRect::vertical(Val::Px(4.0)),
        ..default()
    })
    .with_children(|b| {
        b.spawn((
            theme.label(text, 14.0, color),
            TextLayout {
                justify: Justify::Center,
                ..default()
            },
        ));
    });
}

fn buttons_row(
    c: &mut RelatedSpawnerCommands<ChildOf>,
    build: impl FnOnce(&mut RelatedSpawnerCommands<ChildOf>),
) {
    c.spawn(Node {
        flex_direction: FlexDirection::Row,
        margin: UiRect::top(Val::Px(8.0)),
        ..default()
    })
    .with_children(build);
}

fn spawn_login(commands: &mut Commands, theme: &Theme, account: &Account, form: &Form) {
    form_screen(commands, theme, account, "LOG IN", |c| {
        spawn_field(c, theme, form, Field::Login, "username or email", FIELD_W);
        spawn_field(c, theme, form, Field::Password, "password", FIELD_W);
        buttons_row(c, |b| {
            b.spawn(form_button(theme, "sign in", UiAction::SubmitLogin));
            b.spawn(form_button(theme, "create account", UiAction::OpenRegister));
            b.spawn(form_button(theme, "forgot password", UiAction::OpenForgot));
        });
    });
}

fn spawn_register(commands: &mut Commands, theme: &Theme, account: &Account, form: &Form) {
    form_screen(commands, theme, account, "CREATE ACCOUNT", |c| {
        spawn_field(c, theme, form, Field::Email, "e-mail", FIELD_W);
        spawn_field(
            c,
            theme,
            form,
            Field::Username,
            "username (3-20: letters, digits, _ - .)",
            FIELD_W,
        );
        spawn_field(
            c,
            theme,
            form,
            Field::Password,
            "password (8+ characters)",
            FIELD_W,
        );
        spawn_field(c, theme, form, Field::Password2, "password again", FIELD_W);
        buttons_row(c, |b| {
            b.spawn(form_button(
                theme,
                "create account",
                UiAction::SubmitRegister,
            ));
        });
    });
}

fn spawn_verify(commands: &mut Commands, theme: &Theme, account: &Account, form: &Form, now: f64) {
    form_screen(commands, theme, account, "CHECK YOUR E-MAIL", |c| {
        c.spawn((
            theme.label(
                format!("we sent a 6 digit code to {}", account.pending_email),
                14.0,
                theme::OFF_WHITE,
            ),
            Node {
                margin: UiRect::bottom(Val::Px(6.0)),
                ..default()
            },
        ));
        spawn_field(c, theme, form, Field::Code, "code", FIELD_W);
        buttons_row(c, |b| {
            b.spawn(form_button(theme, "verify", UiAction::SubmitVerify));
            resend_button(b, theme, account.can_resend(now));
        });
    });
}

/// "resend code", greyed out for a few seconds after a code went out.
fn resend_button(b: &mut RelatedSpawnerCommands<ChildOf>, theme: &Theme, ready: bool) {
    let mut e = b.spawn(form_button(theme, "resend code", UiAction::Resend));
    if !ready {
        e.insert((Disabled, BackgroundColor(BTN_DISABLED)));
    }
}

fn spawn_forgot(commands: &mut Commands, theme: &Theme, account: &Account, form: &Form) {
    form_screen(commands, theme, account, "FORGOT PASSWORD", |c| {
        c.spawn((
            theme.label(
                "we will mail you a code to set a new password",
                14.0,
                theme::OFF_WHITE,
            ),
            Node {
                margin: UiRect::bottom(Val::Px(6.0)),
                ..default()
            },
        ));
        spawn_field(c, theme, form, Field::Email, "e-mail", FIELD_W);
        buttons_row(c, |b| {
            b.spawn(form_button(theme, "send code", UiAction::SubmitForgot));
        });
    });
}

fn spawn_reset(commands: &mut Commands, theme: &Theme, account: &Account, form: &Form, now: f64) {
    form_screen(commands, theme, account, "RESET PASSWORD", |c| {
        c.spawn((
            theme.label(
                format!(
                    "if {} has an account, a code is on its way",
                    account.pending_email
                ),
                14.0,
                theme::OFF_WHITE,
            ),
            Node {
                margin: UiRect::bottom(Val::Px(6.0)),
                ..default()
            },
        ));
        spawn_field(c, theme, form, Field::Code, "code", FIELD_W);
        spawn_field(
            c,
            theme,
            form,
            Field::Password,
            "new password (8+ characters)",
            FIELD_W,
        );
        spawn_field(
            c,
            theme,
            form,
            Field::Password2,
            "new password again",
            FIELD_W,
        );
        buttons_row(c, |b| {
            b.spawn(form_button(theme, "set password", UiAction::SubmitReset));
            resend_button(b, theme, account.can_resend(now));
        });
    });
}

/// Rebuilds the verify / reset screen when the "resend code" wait runs out (or a request lands),
/// so the button greys in and out without the screen polling every frame.
fn resend_timer(
    account: Res<Account>,
    screen: Res<UiScreen>,
    time: Res<Time<Real>>,
    mut refresh: ResMut<UiRefresh>,
    mut was_ready: Local<Option<bool>>,
) {
    if !matches!(*screen, UiScreen::Verify | UiScreen::Reset) {
        *was_ready = None;
        return;
    }
    let ready = account.can_resend(time.elapsed_secs_f64());
    if *was_ready != Some(ready) {
        if was_ready.is_some() {
            refresh.0 = true;
        }
        *was_ready = Some(ready);
    }
}

fn spawn_change_username(commands: &mut Commands, theme: &Theme, account: &Account, form: &Form) {
    form_screen(commands, theme, account, "CHANGE USERNAME", |c| {
        spawn_field(c, theme, form, Field::NewUsername, "new username", FIELD_W);
        buttons_row(c, |b| {
            b.spawn(form_button(theme, "save", UiAction::SubmitUsername));
        });
    });
}

fn spawn_change_password(commands: &mut Commands, theme: &Theme, account: &Account, form: &Form) {
    form_screen(commands, theme, account, "CHANGE PASSWORD", |c| {
        spawn_field(
            c,
            theme,
            form,
            Field::CurrentPassword,
            "current password",
            FIELD_W,
        );
        spawn_field(
            c,
            theme,
            form,
            Field::Password,
            "new password (8+ characters)",
            FIELD_W,
        );
        spawn_field(
            c,
            theme,
            form,
            Field::Password2,
            "new password again",
            FIELD_W,
        );
        buttons_row(c, |b| {
            b.spawn(form_button(theme, "save", UiAction::SubmitPassword));
        });
    });
}

/// `YYYY-MM-DD` of a unix timestamp (civil-from-days, Howard Hinnant's algorithm).
fn ymd(ts: i64) -> String {
    let z = ts.div_euclid(86_400) + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}")
}

/// A fixed-width cell of a history row.
fn cell(
    r: &mut RelatedSpawnerCommands<ChildOf>,
    w: f32,
    bundle: impl Bundle,
    justify: JustifyContent,
) {
    r.spawn(Node {
        width: Val::Px(w),
        justify_content: justify,
        flex_shrink: 0.0,
        ..default()
    })
    .with_children(|x| {
        x.spawn((bundle, no_wrap()));
    });
}

/// Fuzzy name match: every character of `query` appears in `name`, in order, ignoring case.
fn fuzzy(name: &str, query: &str) -> bool {
    let mut chars = name.chars().flat_map(char::to_lowercase);
    query
        .chars()
        .flat_map(char::to_lowercase)
        .all(|q| chars.any(|c| c == q))
}

/// One line of the match history.
fn history_row(c: &mut RelatedSpawnerCommands<ChildOf>, theme: &Theme, m: &HistoryEntry) {
    let (result, color) = if m.won {
        ("WIN", theme::YELLOW)
    } else {
        ("LOSS", theme::LIGHT_RED)
    };
    c.spawn(Node {
        width: Val::Px(ROW_W - 40.0),
        min_height: Val::Px(30.0),
        flex_direction: FlexDirection::Row,
        align_items: AlignItems::Center,
        column_gap: Val::Px(10.0),
        ..default()
    })
    .with_children(|r| {
        cell(
            r,
            84.0,
            theme.label(ymd(m.played_at), 13.0, theme::TAN_DARK),
            JustifyContent::FlexStart,
        );
        cell(
            r,
            48.0,
            theme.heading_flat(result, 14.0, color),
            JustifyContent::FlexStart,
        );
        cell(
            r,
            58.0,
            theme.heading_flat(
                format!("{} - {}", m.my_score, m.their_score),
                15.0,
                theme::TAN_LIGHT,
            ),
            JustifyContent::Center,
        );
        let vs = match m.their_elo {
            Some(elo) => format!("vs {} ({elo})", m.opponent),
            None => format!("vs {}", m.opponent),
        };
        r.spawn(Node {
            flex_grow: 1.0,
            overflow: Overflow::clip(),
            ..default()
        })
        .with_children(|x| {
            x.spawn((theme.label(vs, 14.0, theme::OFF_WHITE), no_wrap()));
        });
        // Casual rounds carry no rating: the two rating cells say so instead.
        match (m.ranked, m.my_elo, m.delta) {
            (true, Some(elo), Some(delta)) => {
                cell(
                    r,
                    58.0,
                    theme.heading_flat(format!("{delta:+}"), 15.0, color),
                    JustifyContent::FlexEnd,
                );
                cell(
                    r,
                    96.0,
                    theme.label(format!("{elo} → {}", elo + delta), 12.0, theme::TAN_DARK),
                    JustifyContent::FlexEnd,
                );
            }
            _ => {
                cell(
                    r,
                    58.0,
                    theme.label("", 15.0, theme::TAN_DARK),
                    JustifyContent::FlexEnd,
                );
                cell(
                    r,
                    96.0,
                    theme.label("casual", 12.0, theme::TAN_DARK),
                    JustifyContent::FlexEnd,
                );
            }
        }
    });
}

fn spawn_profile(
    commands: &mut Commands,
    theme: &Theme,
    account: &Account,
    form: &Form,
    filter: &HistoryFilter,
    scroll: Vec2,
) {
    let root = screen_root(commands, theme, false);
    let query = form.get(Field::HistorySearch).trim().to_string();
    let user = account
        .profile
        .as_ref()
        .map(|p| &p.user)
        .or(account.user.as_ref())
        .cloned()
        .unwrap_or_default();
    let games = user.wins + user.losses;
    let rate = if games > 0 {
        format!("{:.0}% win rate", 100.0 * user.wins as f32 / games as f32)
    } else {
        "no ranked games yet".to_string()
    };
    let rank = account.profile.as_ref().and_then(|p| p.rank);
    commands.entity(root).with_children(|p| {
        p.spawn(panel_column(24.0)).with_children(|c| {
            back_arrow(c, theme);
            c.spawn(Node {
                flex_direction: FlexDirection::Row,
                align_items: AlignItems::Center,
                column_gap: Val::Px(12.0),
                ..default()
            })
            .with_children(|r| {
                r.spawn(theme.soldier_icon(40.0));
                r.spawn((
                    theme.heading(user.username.clone(), 40.0, theme::TAN_LIGHT),
                    no_wrap(),
                ));
                // The place on the leaderboard hangs off the right of the name, which stays centred.
                if let Some(rank) = rank {
                    r.spawn(Node {
                        position_type: PositionType::Absolute,
                        left: Val::Percent(100.0),
                        top: Val::Px(0.0),
                        bottom: Val::Px(0.0),
                        margin: UiRect::left(Val::Px(14.0)),
                        align_items: AlignItems::Center,
                        ..default()
                    })
                    .with_children(|x| {
                        x.spawn((
                            theme.heading_flat(format!("(#{rank})"), 22.0, theme::YELLOW),
                            no_wrap(),
                        ));
                    });
                }
            });
            c.spawn(Node {
                flex_direction: FlexDirection::Row,
                align_items: AlignItems::FlexEnd,
                column_gap: Val::Px(24.0),
                margin: UiRect::vertical(Val::Px(8.0)),
                ..default()
            })
            .with_children(|r| {
                r.spawn((
                    theme.heading_flat(format!("{} ELO", user.elo), 26.0, theme::YELLOW),
                    no_wrap(),
                ));
                r.spawn((
                    theme.heading_flat(
                        format!("{}W - {}L", user.wins, user.losses),
                        20.0,
                        theme::TAN_LIGHT,
                    ),
                    no_wrap(),
                ));
                r.spawn((theme.label(rate, 15.0, theme::OFF_WHITE), no_wrap()));
            });

            // The section title, with the opponent search and the COMP / ALL switch on its right.
            c.spawn(Node {
                width: Val::Px(ROW_W),
                align_items: AlignItems::Center,
                justify_content: JustifyContent::SpaceBetween,
                margin: UiRect::new(Val::Px(0.0), Val::Px(0.0), Val::Px(10.0), Val::Px(2.0)),
                ..default()
            })
            .with_children(|r| {
                r.spawn((
                    theme.heading_flat("MATCH HISTORY", 20.0, theme::ORANGE),
                    no_wrap(),
                ));
                r.spawn(Node {
                    flex_direction: FlexDirection::Row,
                    align_items: AlignItems::Center,
                    column_gap: Val::Px(12.0),
                    ..default()
                })
                .with_children(|x| {
                    spawn_field(
                        x,
                        theme,
                        form,
                        Field::HistorySearch,
                        "search opponent",
                        200.0,
                    );
                    x.spawn(switch(
                        theme,
                        ["COMP", "ALL"],
                        filter.comp_only,
                        UiAction::HistoryFilter,
                    ));
                });
            });
            c.spawn((
                theme::inset(Node {
                    width: Val::Px(ROW_W),
                    max_height: Val::Px(300.0),
                    min_height: Val::Px(60.0),
                    flex_direction: FlexDirection::Column,
                    align_items: AlignItems::Center,
                    padding: UiRect::axes(Val::Px(16.0), Val::Px(8.0)),
                    overflow: Overflow::scroll_y(),
                    overflow_clip_margin: OverflowClipMargin::content_box(),
                    ..default()
                }),
                ScrollPosition(scroll),
                ScrollPane,
            ))
            .with_children(|list| match account.profile.as_ref() {
                Some(profile) if profile.matches.is_empty() => {
                    list.spawn((
                        theme.label(
                            "no matches yet: play a competitive match, or a round of quick play",
                            14.0,
                            theme::TAN_DARK,
                        ),
                        Node {
                            margin: UiRect::vertical(Val::Px(12.0)),
                            ..default()
                        },
                    ));
                }
                Some(profile) => {
                    let shown: Vec<&HistoryEntry> = profile
                        .matches
                        .iter()
                        .filter(|m| !filter.comp_only || m.ranked)
                        .filter(|m| fuzzy(&m.opponent, &query))
                        .collect();
                    if shown.is_empty() {
                        let msg = if query.is_empty() {
                            "no competitive matches yet".to_string()
                        } else {
                            format!("no matches against \"{query}\"")
                        };
                        list.spawn((
                            theme.label(msg, 14.0, theme::TAN_DARK),
                            Node {
                                margin: UiRect::vertical(Val::Px(12.0)),
                                ..default()
                            },
                        ));
                    }
                    for m in shown {
                        history_row(list, theme, m);
                    }
                }
                None => {
                    list.spawn((
                        theme.label(
                            if account.busy() { "loading..." } else { "" },
                            14.0,
                            theme::TAN_DARK,
                        ),
                        Node {
                            margin: UiRect::vertical(Val::Px(12.0)),
                            ..default()
                        },
                    ));
                }
            });
            c.spawn(theme.label("scroll with the mouse wheel", 11.0, theme::TAN_DARK));

            status_line(c, theme, account);
            buttons_row(c, |b| {
                b.spawn(form_button(
                    theme,
                    "change username",
                    UiAction::OpenChangeUsername,
                ));
                b.spawn(form_button(
                    theme,
                    "change password",
                    UiAction::OpenChangePassword,
                ));
                b.spawn(form_button(theme, "log out", UiAction::Logout));
            });
        });
    });
}

fn spawn_queue(commands: &mut Commands, theme: &Theme, account: &Account, cfg: &ClientConfig) {
    let kind = account
        .queue
        .as_ref()
        .map(|q| q.kind)
        .unwrap_or(QueueKind::Competitive);
    let root = screen_root(commands, theme, false);
    commands.entity(root).with_children(|p| {
        p.spawn(panel_column(26.0)).with_children(|c| {
            let title = match kind {
                QueueKind::Competitive => "COMPETITIVE",
                QueueKind::Quick => "QUICK PLAY",
            };
            c.spawn(theme.heading_flat(title, 22.0, theme::ORANGE));
            c.spawn(Node {
                flex_direction: FlexDirection::Row,
                align_items: AlignItems::Center,
                column_gap: Val::Px(10.0),
                margin: UiRect::top(Val::Px(12.0)),
                ..default()
            })
            .with_children(|r| {
                r.spawn(theme.soldier_icon(32.0));
                r.spawn((
                    QueueText,
                    theme.heading_flat("SEARCHING FOR A GAME...", 18.0, theme::TAN_LIGHT),
                    no_wrap(),
                ));
            });
            c.spawn((
                QueueSizeText,
                theme.label(queue_size_label(account), 13.0, theme::TAN_LIGHT),
                Node {
                    margin: UiRect::bottom(Val::Px(8.0)),
                    ..default()
                },
            ));
            let identity = match kind {
                QueueKind::Competitive => format!(
                    "playing as {} ({} ELO)",
                    account.display_name(),
                    account.user.as_ref().map(|u| u.elo).unwrap_or(0)
                ),
                QueueKind::Quick => format!("playing as {}", account.display_name()),
            };
            c.spawn((
                theme.label(identity, 13.0, theme::OFF_WHITE),
                Node {
                    margin: UiRect::bottom(Val::Px(8.0)),
                    ..default()
                },
            ));
            if kind == QueueKind::Quick {
                // An invite: whoever opens the link lands in this queue too (`?qp`, see `auto_join`).
                c.spawn((
                    theme.label(
                        "invite a friend: anyone who opens this link joins the quick play queue",
                        13.0,
                        theme::OFF_WHITE,
                    ),
                    Node {
                        margin: UiRect::top(Val::Px(4.0)),
                        ..default()
                    },
                ));
                crate::copylink::spawn_link_box(c, theme, cfg.quick_play_link());
            }
            c.spawn(theme.button("Cancel", UiAction::CancelQueue, 200.0, BUTTON_H, 16.0));
            c.spawn(theme.label("Esc to cancel", 12.0, theme::TAN_DARK));
        });
    });
}

/// The desktop builds, one button per platform. Each opens `/download/<platform>` in a new tab
/// (the page's `endifOpen` helper), which nginx answers with the packaged build as an attachment.
fn spawn_download(commands: &mut Commands, theme: &Theme) {
    let root = screen_root(commands, theme, false);
    commands.entity(root).with_children(|p| {
        p.spawn(panel_column(24.0)).with_children(|c| {
            back_arrow(c, theme);
            c.spawn((
                theme.heading("DOWNLOAD", 34.0, theme::TAN_LIGHT),
                Node {
                    margin: UiRect::new(Val::Px(36.0), Val::Px(36.0), Val::Px(0.0), Val::Px(4.0)),
                    ..default()
                },
            ));
            for platform in Platform::ALL {
                c.spawn(button(
                    theme,
                    platform.label(),
                    UiAction::Download(platform),
                ));
            }
        });
    });
}

/// "N playing, M in queue" (us included), or nothing until the server has said.
fn queue_size_label(account: &Account) -> String {
    match &account.queue {
        Some(q) if q.waiting > 0 => format!("{} playing, {} in queue", q.playing, q.waiting),
        _ => String::new(),
    }
}

/// Keeps the queue screen's status lines ticking.
fn queue_status(
    account: Res<Account>,
    time: Res<Time<Real>>,
    mut text: Query<&mut Text, (With<QueueText>, Without<QueueSizeText>)>,
    mut size: Query<&mut Text, (With<QueueSizeText>, Without<QueueText>)>,
) {
    if let Ok(mut t) = text.single_mut() {
        let want = match &account.queue {
            Some(q) => {
                let secs = (time.elapsed_secs_f64() - q.since).max(0.0) as u32;
                let pos = if q.position > 1 {
                    format!(" ({} ahead)", q.position - 1)
                } else {
                    String::new()
                };
                format!(
                    "SEARCHING FOR A GAME... {}:{:02}{pos}",
                    secs / 60,
                    secs % 60
                )
            }
            None => "SEARCHING FOR A GAME...".to_string(),
        };
        if t.0 != want {
            t.0 = want;
        }
    }
    if let Ok(mut t) = size.single_mut() {
        let want = queue_size_label(&account);
        if t.0 != want {
            t.0 = want;
        }
    }
}

/// Keeps the main menu's counts (in the queue buttons and under the logo) current without
/// rebuilding the screen.
fn activity_counts(
    account: Res<Account>,
    mut counts: Query<(&CountText, &mut Text), Without<OnlineText>>,
    mut online: Query<&mut Text, With<OnlineText>>,
) {
    if !account.is_changed() {
        return;
    }
    for (kind, mut t) in &mut counts {
        let want = count_label(account.stats, kind.0);
        if t.0 != want {
            t.0 = want;
        }
    }
    if let Ok(mut t) = online.single_mut() {
        let want = online_label(account.stats);
        if t.0 != want {
            t.0 = want;
        }
    }
}

// ------------------------------------------------------------------------------------ behaviour

fn commit_edit(editing: &mut Editing, settings: &mut Settings) {
    if let Some((slider, text)) = editing.0.take()
        && let Some(v) = slider.parse(&text)
    {
        slider.set(settings, v);
        settings.save();
    }
}

/// Everything a menu action may touch.
#[derive(SystemParam)]
struct MenuCtx<'w, 's> {
    commands: Commands<'w, 's>,
    typed: ResMut<'w, TypedCode>,
    next: ResMut<'w, NextState<AppState>>,
    cmds: MessageWriter<'w, NetCommand>,
    screen: ResMut<'w, UiScreen>,
    settings: ResMut<'w, Settings>,
    refresh: ResMut<'w, UiRefresh>,
    listening: ResMut<'w, Listening>,
    editing: ResMut<'w, Editing>,
    state: Res<'w, State<AppState>>,
    clipboard: ResMut<'w, Clipboard>,
    paste: ResMut<'w, PendingPaste>,
    account: ResMut<'w, Account>,
    form: ResMut<'w, Form>,
    cfg: Res<'w, ClientConfig>,
    time: Res<'w, Time<Real>>,
    lb_rows: Res<'w, LeaderboardRows>,
    history: ResMut<'w, HistoryFilter>,
    /// Both only read by the desktop update path.
    #[cfg_attr(target_arch = "wasm32", allow(dead_code))]
    status: Res<'w, SignalingStatus>,
    #[cfg_attr(target_arch = "wasm32", allow(dead_code))]
    exit: MessageWriter<'w, AppExit>,
}

fn ui_buttons(
    mut q: Query<
        (
            &Interaction,
            &UiAction,
            &mut BackgroundColor,
            Option<&Disabled>,
        ),
        Changed<Interaction>,
    >,
    mut ctx: MenuCtx,
) {
    for (interaction, action, mut bg, disabled) in &mut q {
        if disabled.is_some() {
            *bg = BackgroundColor(BTN_DISABLED);
            continue;
        }
        let style = action.style();
        match interaction {
            Interaction::Pressed => {
                if style == ButtonStyle::Plain {
                    *bg = BackgroundColor(theme::BTN_ACTIVE);
                }
                perform(*action, &mut ctx);
            }
            Interaction::Hovered => match style {
                ButtonStyle::Plain => *bg = BackgroundColor(theme::BTN_HOVER),
                ButtonStyle::Subtle => *bg = BackgroundColor(Color::srgba(1.0, 1.0, 1.0, 0.08)),
                ButtonStyle::Custom => {}
            },
            Interaction::None => match (style, action) {
                (ButtonStyle::Plain, _) => *bg = BackgroundColor(theme::BTN),
                (ButtonStyle::Subtle, UiAction::EditValue(_)) => {
                    *bg = BackgroundColor(theme::INSET_BG)
                }
                (ButtonStyle::Subtle, _) => *bg = BackgroundColor(Color::NONE),
                (ButtonStyle::Custom, _) => {}
            },
        }
    }
}

/// Enter in a form field presses that form's main button.
fn form_submit(mut ctx: MenuCtx) {
    if !ctx.form.take_submit() {
        return;
    }
    match ctx.screen.submit_action() {
        Some(action) => perform(action, &mut ctx),
        // The anonymous name box: Enter just finishes editing.
        None => ctx.form.focus = None,
    }
}

fn perform(action: UiAction, ctx: &mut MenuCtx) {
    let in_game = *ctx.state.get() == AppState::InGame;
    // Any other click finishes a value edit first.
    if !matches!(action, UiAction::EditValue(_)) && ctx.editing.0.is_some() {
        commit_edit(&mut ctx.editing, &mut ctx.settings);
    }
    // ... and takes the keyboard away from a text field.
    ctx.form.focus = None;
    match action {
        UiAction::Practice => {
            ctx.cmds.write(NetCommand::Practice);
        }
        UiAction::CreateRoom => {
            ctx.cmds.write(NetCommand::CreateRoom);
        }
        UiAction::JoinRoom => {
            if ctx.typed.0.len() == ROOM_CODE_LEN {
                ctx.cmds.write(NetCommand::JoinRoom(ctx.typed.0.clone()));
            }
        }
        UiAction::QuickPlay => {
            let now = ctx.time.elapsed_secs_f64();
            ctx.account.join_queue(&ctx.cfg, now, QueueKind::Quick);
            *ctx.screen = UiScreen::Queue;
        }
        UiAction::Competitive => {
            if ctx.account.logged_in() {
                let now = ctx.time.elapsed_secs_f64();
                ctx.account
                    .join_queue(&ctx.cfg, now, QueueKind::Competitive);
                *ctx.screen = UiScreen::Queue;
            }
        }
        UiAction::CancelQueue => {
            ctx.account.leave_queue(&ctx.cfg);
            *ctx.screen = UiScreen::Main;
        }
        UiAction::OpenSettings => *ctx.screen = UiScreen::Settings { from_game: in_game },
        UiAction::Back | UiAction::BackArrow => {
            ctx.listening.0 = None;
            ctx.account.error = None;
            *ctx.screen = back_target(*ctx.screen, in_game);
        }
        UiAction::Resume => *ctx.screen = UiScreen::Hidden,
        UiAction::Leave => {
            ctx.cmds.write(NetCommand::Leave);
        }
        UiAction::ErrorOk => leave_room_error(&mut ctx.commands, &mut ctx.typed, &mut ctx.next),
        UiAction::ResultOk => {
            ctx.account.dismiss_result();
            ctx.refresh.0 = true;
        }
        UiAction::Paste => ctx.paste.0 = crate::webclip::request_paste(&mut ctx.clipboard),
        UiAction::HistoryFilter => {
            ctx.history.comp_only = !ctx.history.comp_only;
            ctx.refresh.0 = true;
        }
        UiAction::InvertY => {
            ctx.settings.invert_y = !ctx.settings.invert_y;
            ctx.settings.save();
            ctx.refresh.0 = true;
        }
        UiAction::Fullscreen => {
            ctx.settings.fullscreen = !ctx.settings.fullscreen;
            ctx.settings.save();
            ctx.refresh.0 = true;
        }
        UiAction::AdaptiveDelay => {
            commit_edit(&mut ctx.editing, &mut ctx.settings);
            ctx.settings.adaptive_delay = !ctx.settings.adaptive_delay;
            ctx.settings.save();
            ctx.refresh.0 = true;
        }
        UiAction::Launcher => {
            ctx.settings.weapon = match ctx.settings.weapon {
                Weapon::Stock => Weapon::Original,
                Weapon::Original => Weapon::Stock,
            };
            ctx.settings.save();
            ctx.refresh.0 = true;
        }
        UiAction::SeparateSensitivity => {
            commit_edit(&mut ctx.editing, &mut ctx.settings);
            let separate = !ctx.settings.separate_sensitivity;
            ctx.settings.set_separate_sensitivity(separate);
            ctx.settings.save();
            ctx.refresh.0 = true;
        }
        UiAction::EditValue(slider) => {
            if ctx.editing.0.as_ref().map(|(a, _)| *a) != Some(slider) {
                commit_edit(&mut ctx.editing, &mut ctx.settings);
                ctx.editing.0 = Some((slider, slider.edit_text(&ctx.settings)));
            }
        }
        UiAction::Bind(a) => {
            ctx.listening.0 = Some(a);
            ctx.refresh.0 = true;
        }
        UiAction::ResetDefaults => {
            *ctx.settings = Settings::default();
            ctx.settings.save();
            ctx.listening.0 = None;
            ctx.editing.0 = None;
            ctx.refresh.0 = true;
        }
        UiAction::OpenLogin => {
            ctx.account.error = None;
            ctx.account.notice = None;
            ctx.form.clear_secrets();
            *ctx.screen = UiScreen::Login;
        }
        UiAction::OpenRegister => {
            ctx.account.error = None;
            *ctx.screen = UiScreen::Register;
        }
        UiAction::OpenForgot => {
            ctx.account.error = None;
            *ctx.screen = UiScreen::Forgot;
        }
        UiAction::Resend => {
            if !ctx.account.can_resend(ctx.time.elapsed_secs_f64()) {
                return;
            }
            match *ctx.screen {
                UiScreen::Verify => ctx.account.resend(&ctx.cfg),
                UiScreen::Reset => {
                    let email = ctx.account.pending_email.clone();
                    ctx.account.forgot(&ctx.cfg, &email);
                }
                _ => {}
            }
            ctx.refresh.0 = true;
        }
        UiAction::OpenProfile => {
            ctx.account.error = None;
            ctx.account.notice = None;
            ctx.account.profile = None;
            ctx.form.clear(Field::HistorySearch);
            let name = ctx.account.display_name();
            ctx.account.fetch_profile(&ctx.cfg, &name);
            *ctx.screen = UiScreen::Profile;
        }
        UiAction::Logout => {
            ctx.account.logout();
            ctx.account.notice = None;
            *ctx.screen = UiScreen::Main;
        }
        UiAction::OpenChangeUsername => {
            ctx.account.error = None;
            ctx.account.notice = None;
            ctx.form.clear(Field::NewUsername);
            *ctx.screen = UiScreen::ChangeUsername;
        }
        UiAction::OpenChangePassword => {
            ctx.account.error = None;
            ctx.account.notice = None;
            ctx.form.clear_secrets();
            *ctx.screen = UiScreen::ChangePassword;
        }
        UiAction::OpenTab(tab) => {
            ctx.account.error = None;
            // Fresh standings, on the page that was open last. The first time the tab has not
            // been laid out yet, so how many fit is unknown: `leaderboard_fetch` asks then.
            if tab == Tab::Leaderboard
                && let Some(per) = ctx.lb_rows.0
            {
                let page = ctx
                    .account
                    .leaderboard
                    .as_ref()
                    .map(|lb| lb.page)
                    .unwrap_or(1);
                ctx.account.fetch_leaderboard(&ctx.cfg, page, per);
            }
            *ctx.screen = tab.screen();
        }
        UiAction::LeaderboardPage(page) => {
            let per = ctx.lb_rows.0.unwrap_or(10);
            ctx.account.fetch_leaderboard(&ctx.cfg, page, per);
        }
        UiAction::OpenDownload => *ctx.screen = UiScreen::Download,
        UiAction::Download(platform) => crate::webclip::open_url(&platform.url()),
        UiAction::Update => {
            #[cfg(target_arch = "wasm32")]
            {
                crate::webclip::reload_for_update(None);
            }
            #[cfg(not(target_arch = "wasm32"))]
            {
                match crate::update::launch_updater(
                    &ctx.cfg,
                    ctx.status.server_build.as_deref().unwrap_or(""),
                ) {
                    Ok(()) => {
                        info!("updater started; quitting");
                        ctx.exit.write(AppExit::Success);
                    }
                    Err(e) => {
                        warn!("{e}");
                        ctx.account.error = Some(e);
                        ctx.refresh.0 = true;
                    }
                }
            }
        }
        UiAction::SubmitLogin
        | UiAction::SubmitRegister
        | UiAction::SubmitVerify
        | UiAction::SubmitForgot
        | UiAction::SubmitReset
        | UiAction::SubmitUsername
        | UiAction::SubmitPassword => submit_form(action, ctx),
    }
}

/// Validates a form locally, then sends it. Errors show on the status line.
fn submit_form(action: UiAction, ctx: &mut MenuCtx) {
    if ctx.account.busy() {
        return;
    }
    ctx.refresh.0 = true;
    fn get(ctx: &MenuCtx, f: Field) -> String {
        ctx.form.get(f).trim().to_string()
    }
    fn raw(ctx: &MenuCtx, f: Field) -> String {
        ctx.form.get(f).to_string()
    }
    fn fail(ctx: &mut MenuCtx, msg: &str) {
        ctx.account.error = Some(msg.to_string());
    }
    fn password_pair(ctx: &mut MenuCtx) -> Option<String> {
        let (p1, p2) = (raw(ctx, Field::Password), raw(ctx, Field::Password2));
        if p1.chars().count() < 8 {
            fail(ctx, "the password needs at least 8 characters");
            return None;
        }
        if p1 != p2 {
            fail(ctx, "the passwords do not match");
            return None;
        }
        Some(p1)
    }
    match action {
        UiAction::SubmitLogin => {
            let (u, p) = (get(ctx, Field::Login), raw(ctx, Field::Password));
            if u.is_empty() || p.is_empty() {
                return fail(ctx, "enter your username and password");
            }
            ctx.account.login(&ctx.cfg, &u, &p);
        }
        UiAction::SubmitRegister => {
            let (e, u) = (get(ctx, Field::Email), get(ctx, Field::Username));
            if !e.contains('@') {
                return fail(ctx, "enter your e-mail address");
            }
            if u.chars().count() < 3 {
                return fail(ctx, "the username needs at least 3 characters");
            }
            let Some(p) = password_pair(ctx) else { return };
            ctx.account.register(&ctx.cfg, &e, &u, &p);
        }
        UiAction::SubmitVerify => {
            let code = get(ctx, Field::Code);
            if code.len() != 6 {
                return fail(ctx, "the code has 6 digits");
            }
            ctx.account.verify(&ctx.cfg, &code);
        }
        UiAction::SubmitForgot => {
            let e = get(ctx, Field::Email);
            if !e.contains('@') {
                return fail(ctx, "enter your e-mail address");
            }
            ctx.account.forgot(&ctx.cfg, &e);
        }
        UiAction::SubmitReset => {
            let code = get(ctx, Field::Code);
            if code.len() != 6 {
                return fail(ctx, "the code has 6 digits");
            }
            let Some(p) = password_pair(ctx) else { return };
            ctx.account.reset(&ctx.cfg, &code, &p);
        }
        UiAction::SubmitUsername => {
            let u = get(ctx, Field::NewUsername);
            if u.chars().count() < 3 {
                return fail(ctx, "the username needs at least 3 characters");
            }
            ctx.account.change_username(&ctx.cfg, &u);
        }
        UiAction::SubmitPassword => {
            let current = raw(ctx, Field::CurrentPassword);
            if current.is_empty() {
                return fail(ctx, "enter your current password");
            }
            let Some(p) = password_pair(ctx) else { return };
            ctx.account.change_password(&ctx.cfg, &current, &p);
        }
        _ => {}
    }
}

/// Where "back" / Esc leads from a screen.
fn back_target(screen: UiScreen, in_game: bool) -> UiScreen {
    match screen {
        UiScreen::Settings { from_game: true } => UiScreen::Pause,
        UiScreen::Settings { from_game: false } => UiScreen::Main,
        UiScreen::Verify => UiScreen::Register,
        UiScreen::Reset => UiScreen::Forgot,
        UiScreen::Register | UiScreen::Forgot => UiScreen::Login,
        UiScreen::ChangeUsername | UiScreen::ChangePassword => UiScreen::Profile,
        UiScreen::Login
        | UiScreen::Profile
        | UiScreen::Queue
        | UiScreen::Download
        | UiScreen::Leaderboard
        | UiScreen::Main => UiScreen::Main,
        UiScreen::Pause | UiScreen::Hidden => {
            if in_game {
                UiScreen::Pause
            } else {
                UiScreen::Main
            }
        }
    }
}

/// Dragging a sensitivity slider. The value is applied live; it is saved when the button is let go.
fn slider_drag(
    mouse: Res<ButtonInput<MouseButton>>,
    window: Query<&Window, With<PrimaryWindow>>,
    tracks: Query<(
        Entity,
        &SliderTrack,
        &Interaction,
        &ComputedNode,
        &UiGlobalTransform,
    )>,
    mut settings: ResMut<Settings>,
    mut editing: ResMut<Editing>,
    mut active: Local<Option<Entity>>,
) {
    if mouse.just_pressed(MouseButton::Left) {
        *active = tracks
            .iter()
            .find(|(_, _, i, _, _)| **i == Interaction::Pressed)
            .map(|(e, ..)| e);
        if active.is_some() && editing.0.is_some() {
            commit_edit(&mut editing, &mut settings);
        }
    }
    if !mouse.pressed(MouseButton::Left) {
        if active.take().is_some() {
            settings.save();
        }
        return;
    }
    let Some(e) = *active else { return };
    let (Ok((_, track, _, node, transform)), Ok(window)) = (tracks.get(e), window.single()) else {
        return;
    };
    let Some(cursor) = window.physical_cursor_position() else {
        return;
    };
    let Some(p) = node.normalize_point(*transform, cursor) else {
        return;
    };
    let frac = (p.x + 0.5).clamp(0.0, 1.0);
    let (lo, hi) = track.0.range();
    let value = lo + frac * (hi - lo);
    if (value - track.0.value(&settings)).abs() >= 0.0025 * (hi - lo) {
        track.0.set(&mut settings, value);
    }
}

/// Mouse wheel scrolls a `ScrollPane`. Layout clamps the offset it applies but never writes the
/// clamped value back, so clamp here too or scrolling past the end would build up a dead zone.
fn wheel_scroll(
    mut wheel: MessageReader<MouseWheel>,
    mut panes: Query<(&mut ScrollPosition, &ComputedNode), With<ScrollPane>>,
) {
    let dy: f32 = wheel
        .read()
        .map(|w| match w.unit {
            MouseScrollUnit::Line => w.y * WHEEL_LINE_PX,
            MouseScrollUnit::Pixel => w.y,
        })
        .sum();
    if dy == 0.0 {
        return;
    }
    for (mut pos, node) in &mut panes {
        // `ComputedNode` sizes are physical pixels; `ScrollPosition` is logical.
        let max = (node.content_size - node.size + node.scrollbar_size)
            .max(Vec2::ZERO)
            .y
            * node.inverse_scale_factor;
        pos.y = (pos.y - dy).clamp(0.0, max);
    }
}

/// Keeps the slider fill/knob and the value boxes in step with the settings and the edit buffer.
#[allow(clippy::type_complexity)]
fn sync_settings_widgets(
    settings: Res<Settings>,
    editing: Res<Editing>,
    mut fills: Query<(&SliderFill, &mut Node), Without<SliderKnob>>,
    mut knobs: Query<(&SliderKnob, &mut Node), Without<SliderFill>>,
    mut values: Query<(&ValueText, &mut Text, &mut TextColor)>,
) {
    for (fill, mut node) in &mut fills {
        node.width = Val::Percent(fill.0.fraction(&settings) * 100.0);
    }
    for (knob, mut node) in &mut knobs {
        node.left = Val::Percent(knob.0.fraction(&settings) * 100.0);
    }
    for (value, mut text, mut color) in &mut values {
        match &editing.0 {
            Some((axis, buf)) if *axis == value.0 => {
                text.0 = format!("{buf}_");
                color.0 = theme::TAN_LIGHT;
            }
            _ => {
                text.0 = value.0.display(&settings);
                color.0 = theme::YELLOW;
            }
        }
    }
}

/// Typing into an open sensitivity value box. Enter applies, Esc cancels.
fn edit_value_keys(
    mut keys: MessageReader<KeyboardInput>,
    mut editing: ResMut<Editing>,
    mut settings: ResMut<Settings>,
) {
    if editing.0.is_none() {
        keys.clear();
        return;
    }
    for ev in keys.read() {
        if !ev.state.is_pressed() {
            continue;
        }
        match &ev.logical_key {
            Key::Character(c) => {
                let c = c.as_str();
                if let Some((_, buf)) = editing.0.as_mut()
                    && buf.len() < 6
                    && c.chars().all(|ch| ch.is_ascii_digit() || ch == '.')
                {
                    buf.push_str(c);
                }
            }
            Key::Backspace => {
                if let Some((_, buf)) = editing.0.as_mut() {
                    buf.pop();
                }
            }
            Key::Enter => commit_edit(&mut editing, &mut settings),
            Key::Escape => editing.0 = None,
            _ => {}
        }
    }
}

/// While a bind button is active, the next key or mouse press becomes the binding.
/// Runs before `ui_buttons`, so the click that activated the bind button is never captured.
fn capture_binding(
    mut keys: MessageReader<KeyboardInput>,
    mut mouse: MessageReader<MouseButtonInput>,
    mut listening: ResMut<Listening>,
    mut settings: ResMut<Settings>,
    mut refresh: ResMut<UiRefresh>,
) {
    let Some(action) = listening.0 else {
        // Keep the readers drained so stale presses don't bind later.
        keys.clear();
        mouse.clear();
        return;
    };
    let mut chosen: Option<Binding> = None;
    let mut cancel = false;
    for ev in keys.read() {
        if !ev.state.is_pressed() {
            continue;
        }
        if ev.key_code == KeyCode::Escape {
            cancel = true;
        } else {
            chosen = Some(Binding::Key(ev.key_code));
        }
    }
    for ev in mouse.read() {
        if ev.state.is_pressed() && chosen.is_none() && !cancel {
            chosen = Some(Binding::Mouse(ev.button));
        }
    }
    if cancel {
        listening.0 = None;
        refresh.0 = true;
    } else if let Some(b) = chosen {
        settings.bindings.set(action, b);
        settings.save();
        listening.0 = None;
        refresh.0 = true;
    }
}

/// Esc: in game toggles the pause overlay; elsewhere goes back a screen.
#[allow(clippy::too_many_arguments)]
fn escape_key(
    keys: Res<ButtonInput<KeyCode>>,
    mut screen: ResMut<UiScreen>,
    state: Res<State<AppState>>,
    listening: Res<Listening>,
    editing: Res<Editing>,
    mut account: ResMut<Account>,
    cfg: Res<ClientConfig>,
    mut refresh: ResMut<UiRefresh>,
) {
    // A value box swallows Esc (it just closed, or is still open). A focused text field does not:
    // Esc goes back from a form even while typing in it.
    if !keys.just_pressed(KeyCode::Escape)
        || listening.0.is_some()
        || editing.0.is_some()
        || editing.is_changed()
    {
        return;
    }
    // The result popup goes first.
    if *screen == UiScreen::Main && account.result.is_some() {
        account.dismiss_result();
        refresh.0 = true;
        return;
    }
    let in_game = *state.get() == AppState::InGame;
    let next = match *screen {
        UiScreen::Hidden if in_game => UiScreen::Pause,
        UiScreen::Pause => UiScreen::Hidden,
        UiScreen::Main | UiScreen::Hidden => *screen,
        other => back_target(other, in_game),
    };
    if next != *screen {
        if *screen == UiScreen::Queue {
            account.leave_queue(&cfg);
        }
        account.error = None;
        *screen = next;
    }
}

/// Ctrl+V / Cmd+V starts a clipboard read on any menu screen (desktop; on the web the page keeps
/// the key and the browser's paste event feeds `apply_paste` instead).
fn paste_shortcut(
    key_state: Res<ButtonInput<KeyCode>>,
    mut clipboard: ResMut<Clipboard>,
    mut paste: ResMut<PendingPaste>,
) {
    let modifier = key_state.any_pressed([
        KeyCode::ControlLeft,
        KeyCode::ControlRight,
        KeyCode::SuperLeft,
        KeyCode::SuperRight,
    ]);
    if modifier && key_state.just_pressed(KeyCode::KeyV) {
        paste.0 = crate::webclip::request_paste(&mut clipboard);
    }
}

fn type_code(
    mut keys: MessageReader<KeyboardInput>,
    key_state: Res<ButtonInput<KeyCode>>,
    mut typed: ResMut<TypedCode>,
    mut field: Query<&mut Text, With<CodeField>>,
    mut cmds: MessageWriter<NetCommand>,
    screen: Res<UiScreen>,
    editing: Res<Editing>,
    form: Res<Form>,
) {
    // A value box (the volume slider's) or the name box takes the keys while it is open.
    if *screen != UiScreen::Main || editing.0.is_some() || form.focus.is_some() {
        keys.clear();
        return;
    }
    // Shortcuts must not type their letter into the code.
    let modifier = key_state.any_pressed([
        KeyCode::ControlLeft,
        KeyCode::ControlRight,
        KeyCode::SuperLeft,
        KeyCode::SuperRight,
    ]);
    let mut changed = false;
    for ev in keys.read() {
        if !ev.state.is_pressed() {
            continue;
        }
        match &ev.logical_key {
            Key::Character(_) if modifier => {}
            Key::Character(c) => {
                let add = normalize_room_code(c.as_str());
                if !add.is_empty() && typed.0.len() < ROOM_CODE_LEN {
                    typed.0.push_str(&add);
                    typed.0.truncate(ROOM_CODE_LEN);
                    changed = true;
                }
            }
            Key::Backspace => {
                typed.0.pop();
                changed = true;
            }
            Key::Enter => {
                if typed.0.len() == ROOM_CODE_LEN {
                    cmds.write(NetCommand::JoinRoom(typed.0.clone()));
                }
            }
            _ => {}
        }
    }
    if changed && let Ok(mut t) = field.single_mut() {
        t.0 = code_display(&typed.0, ROOM_CODE_LEN);
    }
}

/// Applies a finished clipboard read: into the focused text field, or else the room code field
/// (a bare code or an invite link).
fn apply_paste(
    mut paste: ResMut<PendingPaste>,
    mut typed: ResMut<TypedCode>,
    mut field: Query<&mut Text, With<CodeField>>,
    mut form: ResMut<Form>,
) {
    // Web: text handed over by the page's paste handlers.
    let result = match crate::webclip::take_pasted() {
        Some(text) => Ok(text),
        None => {
            let Some(read) = paste.0.as_mut() else { return };
            let Some(result) = read.poll_result() else {
                return;
            };
            paste.0 = None;
            result
        }
    };
    match result {
        Ok(text) => {
            if let Some(focus) = form.focus {
                crate::textfield::push(&mut form, focus, text.trim());
                return;
            }
            let code = code_from_text(&text);
            if code.is_empty() {
                warn!("clipboard holds no room code");
                return;
            }
            typed.0 = code;
            if let Ok(mut t) = field.single_mut() {
                t.0 = code_display(&typed.0, ROOM_CODE_LEN);
            }
        }
        Err(e) => warn!("could not read the clipboard: {e:?}"),
    }
}

/// Joins when the app was launched with a room code (`?room=` or `--room`) or a quick play invite
/// (`?qp` or `--quick`). Waits for the web loading screen to be gone (`StartupDone`) so the match
/// cannot start, and the countdown run, under it while the assets and the render warm-up are
/// still in progress.
fn auto_join(
    mut cfg: ResMut<ClientConfig>,
    mut cmds: MessageWriter<NetCommand>,
    mut account: ResMut<Account>,
    mut screen: ResMut<UiScreen>,
    time: Res<Time<Real>>,
) {
    if let Some(code) = cfg.initial_room.take()
        && code.len() == ROOM_CODE_LEN
    {
        cmds.write(NetCommand::JoinRoom(code));
        // The link has done its job; a reload must not rejoin a room that may be long gone.
        crate::config::forget_join_in_url();
    } else if cfg.initial_quick {
        cfg.initial_quick = false;
        account.join_queue(&cfg, time.elapsed_secs_f64(), QueueKind::Quick);
        *screen = UiScreen::Queue;
        crate::config::forget_join_in_url();
    }
    if cfg.auto_practice {
        cfg.auto_practice = false;
        cmds.write(NetCommand::Practice);
    }
}

fn setup_connecting(
    mut commands: Commands,
    theme: Res<Theme>,
    room: Option<Res<RoomConnection>>,
    kind: Option<Res<MatchKind>>,
    cfg: Res<ClientConfig>,
    account: Res<Account>,
    mut screen: ResMut<UiScreen>,
    cams: Query<Entity, With<MenuCamera>>,
) {
    *screen = UiScreen::Hidden;
    let code = room.map(|r| r.code.clone()).unwrap_or_default();
    if cams.is_empty() {
        commands.spawn(menu_camera());
    }
    let root = screen_root(&mut commands, &theme, false);
    commands.entity(root).with_children(|p| {
        p.spawn(panel_column(26.0)).with_children(|c| {
            // "me VS them", with or without ratings.
            let versus = |c: &mut RelatedSpawnerCommands<ChildOf>, me: String, them: String| {
                c.spawn(Node {
                    flex_direction: FlexDirection::Row,
                    align_items: AlignItems::Center,
                    column_gap: Val::Px(18.0),
                    margin: UiRect::vertical(Val::Px(10.0)),
                    ..default()
                })
                .with_children(|r| {
                    r.spawn((theme.heading(me, 30.0, theme::RED_TEAM), no_wrap()));
                    r.spawn(theme.heading_flat("VS", 18.0, theme::OFF_WHITE));
                    r.spawn((theme.heading(them, 30.0, theme::BLU_TEAM), no_wrap()));
                });
            };
            match kind.as_deref() {
                Some(MatchKind::Ranked(info)) => {
                    c.spawn(theme.heading_flat("RANKED MATCH", 22.0, theme::ORANGE));
                    let me = account
                        .user
                        .as_ref()
                        .map(|u| (u.username.clone(), u.elo))
                        .unwrap_or((account.display_name(), 0));
                    versus(
                        c,
                        format!("{} ({})", me.0, me.1),
                        format!("{} ({})", info.opponent, info.opponent_elo),
                    );
                    c.spawn((
                        theme.label("ft5; leaving early ffs", 13.0, theme::OFF_WHITE),
                        Node {
                            margin: UiRect::bottom(Val::Px(8.0)),
                            ..default()
                        },
                    ));
                }
                Some(MatchKind::Quick(info)) => {
                    c.spawn(theme.heading_flat("QUICK PLAY", 22.0, theme::ORANGE));
                    versus(c, account.display_name(), info.opponent.clone());
                    c.spawn((
                        theme.label("unranked", 13.0, theme::OFF_WHITE),
                        Node {
                            margin: UiRect::bottom(Val::Px(8.0)),
                            ..default()
                        },
                    ));
                }
                _ => {
                    c.spawn(theme.heading_flat("PRIVATE ROOM", 22.0, theme::ORANGE));
                    c.spawn(theme::inset(Node {
                        padding: UiRect::axes(Val::Px(26.0), Val::Px(6.0)),
                        margin: UiRect::vertical(Val::Px(8.0)),
                        ..default()
                    }))
                    .with_children(|b| {
                        b.spawn((
                            theme.heading(code_display(&code, ROOM_CODE_LEN), 64.0, theme::YELLOW),
                            no_wrap(),
                        ));
                    });
                    // `join_link` is a link on the web and the bare code on desktop.
                    let what = if cfg!(target_arch = "wasm32") {
                        "link"
                    } else {
                        "code"
                    };
                    c.spawn(theme.label(
                        format!("share this {what} to invite someone:"),
                        14.0,
                        theme::OFF_WHITE,
                    ));
                    crate::copylink::spawn_link_box(c, &theme, cfg.join_link(&code));
                }
            }
            c.spawn((
                theme.label(
                    format!("matchmaking via {}", cfg.signaling_url),
                    11.0,
                    theme::TAN_DARK,
                ),
                Node {
                    margin: UiRect::bottom(Val::Px(8.0)),
                    ..default()
                },
            ));
            c.spawn(Node {
                flex_direction: FlexDirection::Row,
                align_items: AlignItems::Center,
                column_gap: Val::Px(10.0),
                ..default()
            })
            .with_children(|r| {
                r.spawn(theme.soldier_icon(32.0));
                r.spawn((
                    ConnectingText,
                    theme.heading_flat("CONNECTING TO MATCHMAKING...", 18.0, theme::TAN_LIGHT),
                ));
            });
            c.spawn((
                theme.label("Esc to cancel", 12.0, theme::TAN_DARK),
                Node {
                    margin: UiRect::top(Val::Px(12.0)),
                    ..default()
                },
            ));
        });
    });
}

/// Lobby status line: reaching the matchmaking server first, then waiting for the other player
/// (the WebRTC handshake itself is not observable through matchbox; it ends in the match starting).
fn connecting_phase(
    room: Option<Res<RoomConnection>>,
    mut text: Query<&mut Text, With<ConnectingText>>,
) {
    let Some(room) = room else { return };
    let want = if matches!(room.failure, Some(RoomFailure::Checking)) {
        "CHECKING THE ROOM...".to_string()
    } else if let Some(state) = room.peer_status() {
        // Web: the browser's ICE state while the direct connection to the opponent comes up.
        format!("OPPONENT FOUND, CONNECTING... ({state})")
    } else if room.opponent_found() {
        // Desktop, or a browser whose handshake has not started: the opponent is in the room.
        "OPPONENT FOUND, CONNECTING...".to_string()
    } else if room.connected {
        "WAITING FOR THE OPPONENT...".to_string()
    } else {
        "CONNECTING TO MATCHMAKING...".to_string()
    };
    if let Ok(mut t) = text.single_mut()
        && t.0 != want
    {
        t.0 = want;
    }
}

/// Once the room reports an error (full, or the server refused us) the waiting panel is replaced
/// by a short error box with an OK button.
fn connecting_screen(
    mut commands: Commands,
    theme: Res<Theme>,
    room: Option<Res<RoomConnection>>,
    roots: Query<Entity, With<UiRoot>>,
    mut shown: Local<bool>,
) {
    let Some(room) = room else { return };
    // `Checking` is still the waiting panel (with its own status line); the box comes after.
    let Some(failure) = room
        .failure
        .as_ref()
        .filter(|f| !matches!(f, RoomFailure::Checking))
    else {
        *shown = false;
        return;
    };
    if *shown {
        return;
    }
    *shown = true;
    warn!("room {} ended: {failure:?}", room.code);
    for e in &roots {
        commands.entity(e).despawn();
    }
    let (title, hint) = match failure {
        RoomFailure::Full => (
            "ERROR: ROOM IS FULL.",
            "two players are already in this room.",
        ),
        RoomFailure::Refused => (
            "ERROR: COULD NOT JOIN THE ROOM.",
            "the matchmaking server refused the connection. wait a moment and try again.",
        ),
        RoomFailure::Unreachable => (
            "ERROR: CANNOT REACH THE MATCHMAKING SERVER.",
            "try again once the server is back.",
        ),
        RoomFailure::Outdated => (
            "ERROR: THIS BUILD IS OUT OF DATE.",
            if cfg!(target_arch = "wasm32") {
                "reload the page (Ctrl+Shift+R) to get the current version."
            } else {
                "update the client to the current version."
            },
        ),
        RoomFailure::Timeout => (
            "NOBODY JOINED.",
            "the room was closed after waiting for an opponent. Create a new one when you are both ready.",
        ),
        RoomFailure::PeerUnreachable => (
            "ERROR: COULD NOT CONNECT TO THE OPPONENT.",
            "the opponent was found but no connection between you came up.",
        ),
        RoomFailure::Checking => unreachable!(),
    };
    let hint = if matches!(failure, RoomFailure::Timeout) {
        format!(
            "the room was closed after {LOBBY_TIMEOUT_MINUTES} minutes without an opponent. Create a new one when you are both ready."
        )
    } else {
        hint.to_string()
    };
    let root = screen_root(&mut commands, &theme, false);
    commands.entity(root).with_children(|p| {
        p.spawn(panel_column(26.0)).with_children(|c| {
            c.spawn((
                theme.heading(title, 26.0, theme::LIGHT_RED),
                Node {
                    margin: UiRect::bottom(Val::Px(6.0)),
                    ..default()
                },
            ));
            c.spawn((
                theme.label(hint, 13.0, theme::OFF_WHITE),
                Node {
                    margin: UiRect::bottom(Val::Px(12.0)),
                    max_width: Val::Px(520.0),
                    ..default()
                },
            ));
            c.spawn(theme.button("OK", UiAction::ErrorOk, 160.0, BUTTON_H, 18.0));
        });
    });
}

/// Shows / hides the tooltip of a greyed-out button as the mouse moves over it.
fn disabled_tooltips(
    buttons: Query<(&Interaction, &Children), (With<Disabled>, Changed<Interaction>)>,
    mut tips: Query<&mut Visibility, With<Tooltip>>,
) {
    for (interaction, children) in &buttons {
        for child in children.iter() {
            if let Ok(mut vis) = tips.get_mut(child) {
                *vis = if *interaction == Interaction::None {
                    Visibility::Hidden
                } else {
                    Visibility::Visible
                };
            }
        }
    }
}

fn leave_room_error(
    commands: &mut Commands,
    typed: &mut TypedCode,
    next: &mut NextState<AppState>,
) {
    typed.0.clear();
    commands.remove_resource::<RoomConnection>();
    next.set(AppState::Menu);
}

/// Esc cancels waiting; on the error box Enter/Space/Esc all act as OK.
fn connecting_keys(
    keys: Res<ButtonInput<KeyCode>>,
    room: Option<Res<RoomConnection>>,
    mut commands: Commands,
    mut typed: ResMut<TypedCode>,
    mut next: ResMut<NextState<AppState>>,
) {
    let errored = room.is_some_and(|r| {
        r.failure
            .as_ref()
            .is_some_and(|f| !matches!(f, RoomFailure::Checking))
    });
    let ok = errored && (keys.just_pressed(KeyCode::Enter) || keys.just_pressed(KeyCode::Space));
    if keys.just_pressed(KeyCode::Escape) || ok {
        if errored {
            typed.0.clear();
        }
        commands.remove_resource::<RoomConnection>();
        next.set(AppState::Menu);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates_come_out_civil() {
        assert_eq!(ymd(0), "1970-01-01");
        assert_eq!(ymd(951_782_400), "2000-02-29");
        assert_eq!(ymd(1_788_480_000), "2026-09-04");
    }
}
