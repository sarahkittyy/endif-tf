//! Team Fortress 2 look and feel: the `clientscheme.res` palette, the TF2 Build / TF2 Secondary
//! fonts and the widget builders shared by the menus and the HUD.

use bevy::prelude::*;
use bevy::ui::UiSystems;
use bevy::ui::widget::NodeImageMode;
use bevy::window::PrimaryWindow;

// Colours from `tf/resource/clientscheme.res` (the names are Valve's).
pub const TAN_LIGHT: Color = Color::srgb_u8(235, 226, 202); // TanLight
pub const TAN_DARK: Color = Color::srgb_u8(117, 107, 94); // TanDark
pub const TAN_DARKER: Color = Color::srgb_u8(46, 43, 42); // TanDarker
pub const DARK_BROWN: Color = Color::srgb_u8(41, 37, 38); // DarkBrown
pub const OFF_WHITE: Color = Color::srgb_u8(200, 187, 161); // HudOffWhite
pub const ORANGE: Color = Color::srgb_u8(207, 115, 54); // the logo orange
pub const LIGHT_RED: Color = Color::srgb_u8(200, 80, 60); // LightRed
pub const RED_TEAM: Color = Color::srgb_u8(180, 92, 77); // HUDRedTeamSolid
pub const BLU_TEAM: Color = Color::srgb_u8(104, 124, 155); // HUDBlueTeamSolid
pub const YELLOW: Color = Color::srgb_u8(240, 207, 78); // HudProgressBarActive
pub const DEATH_RED: Color = Color::srgb_u8(240, 30, 30); // HudProgressBarActiveLow
pub const PANEL_BG: Color = Color::srgba(0.18, 0.169, 0.165, 0.94); // TanDarker, slightly see-through
pub const INSET_BG: Color = Color::srgba(0.0, 0.0, 0.0, 0.45);
pub const OVERLAY_BG: Color = Color::srgba(0.0, 0.0, 0.0, 0.62);

// Econ-style buttons: tan with dark lettering, orange when hovered, red while pressed.
pub const BTN: Color = TAN_LIGHT;
pub const BTN_HOVER: Color = ORANGE;
pub const BTN_ACTIVE: Color = LIGHT_RED;
pub const BTN_TEXT: Color = TAN_DARKER;

/// Fonts and images used by every screen.
#[derive(Resource, Clone)]
pub struct Theme {
    /// TF2 Build: headings, buttons and big HUD numbers.
    pub build: Handle<Font>,
    /// TF2 Secondary: body text.
    pub secondary: Handle<Font>,
    /// Soldier class icon.
    pub soldier: Handle<Image>,
    /// Title screen background.
    pub menu_bg: Handle<Image>,
    /// The bookmark tab on the title panel's edge, for the page being shown (it opens into the
    /// panel) and for the others (`tools/tf2/ui_assets.py`).
    pub tab_on: Handle<Image>,
    pub tab_off: Handle<Image>,
    /// The TF2 logo glyph (the menu tab) and the winged UGC trophy (the leaderboard tab).
    pub tf2_logo: Handle<Image>,
    pub trophy: Handle<Image>,
}

impl FromWorld for Theme {
    fn from_world(world: &mut World) -> Self {
        let assets = world.resource::<AssetServer>();
        Theme {
            build: assets.load("fonts/TF2Build.ttf"),
            secondary: assets.load("fonts/TF2Secondary.ttf"),
            soldier: assets.load("ui/soldier.png"),
            menu_bg: assets.load("ui/menu_bg.png"),
            tab_on: assets.load("ui/tab_on.png"),
            tab_off: assets.load("ui/tab_off.png"),
            tf2_logo: assets.load("ui/tf2_logo.png"),
            trophy: assets.load("ui/trophy.png"),
        }
    }
}

pub struct ThemePlugin;

impl Plugin for ThemePlugin {
    fn build(&self, app: &mut App) {
        // Right before layout, so a backdrop spawned this frame (a screen rebuilt by `menu.rs`) is
        // sized before it is first drawn; in `Update` it could run ahead of the rebuild, and the
        // unsized image would leave the window dark for a frame whenever the tabs switch.
        app.init_resource::<Theme>().add_systems(PostUpdate, fit_menu_backdrop.before(UiSystems::Layout));
    }
}

fn shadow() -> TextShadow {
    TextShadow { offset: Vec2::new(2.0, 3.0), color: Color::srgba(0.0, 0.0, 0.0, 0.6) }
}

impl Theme {
    pub fn build_font(&self, size: f32) -> TextFont {
        TextFont { font: self.build.clone().into(), font_size: FontSize::Px(size), ..default() }
    }

    pub fn body_font(&self, size: f32) -> TextFont {
        TextFont { font: self.secondary.clone().into(), font_size: FontSize::Px(size), ..default() }
    }

    /// TF2 Build text with a drop shadow (titles, HUD numbers, banners).
    pub fn heading(&self, s: impl Into<String>, size: f32, color: Color) -> impl Bundle {
        (Text::new(s), self.build_font(size), TextColor(color), shadow())
    }

    /// TF2 Build text without a shadow.
    pub fn heading_flat(&self, s: impl Into<String>, size: f32, color: Color) -> impl Bundle {
        (Text::new(s), self.build_font(size), TextColor(color))
    }

    /// TF2 Secondary body text.
    pub fn label(&self, s: impl Into<String>, size: f32, color: Color) -> impl Bundle {
        (Text::new(s), self.body_font(size), TextColor(color))
    }

    /// Econ-style button. The label is upper-cased like every TF2 menu button.
    pub fn button(&self, label: &str, action: impl Component, width: f32, height: f32, font: f32) -> impl Bundle {
        (
            Button,
            action,
            Node {
                width: Val::Px(width),
                height: Val::Px(height),
                justify_content: JustifyContent::Center,
                align_items: AlignItems::Center,
                margin: UiRect::all(Val::Px(4.0)),
                border_radius: BorderRadius::all(Val::Px(5.0)),
                ..default()
            },
            BackgroundColor(BTN),
            BoxShadow::new(Color::srgba(0.0, 0.0, 0.0, 0.5), Val::Px(0.0), Val::Px(3.0), Val::Px(0.0), Val::Px(4.0)),
            children![(Text::new(label.to_uppercase()), self.build_font(font), TextColor(BTN_TEXT))],
        )
    }

    /// The soldier class icon at a given size.
    pub fn soldier_icon(&self, size: f32) -> impl Bundle {
        (
            ImageNode::new(self.soldier.clone()),
            Node { width: Val::Px(size), height: Val::Px(size), flex_shrink: 0.0, ..default() },
        )
    }

    /// Any square icon at a given size, tinted (white leaves it as is).
    pub fn icon(image: Handle<Image>, size: f32, tint: Color) -> impl Bundle {
        (
            ImageNode { image, color: tint, ..default() },
            Node { width: Val::Px(size), height: Val::Px(size), flex_shrink: 0.0, ..default() },
        )
    }
}

/// A `TFFatLineBorder` panel: dark brown, rounded, thin tan border, soft shadow.
pub fn panel(mut node: Node) -> impl Bundle {
    node.border = UiRect::all(Val::Px(2.0));
    node.border_radius = BorderRadius::all(Val::Px(8.0));
    (
        node,
        BackgroundColor(PANEL_BG),
        BorderColor::all(TAN_DARK),
        BoxShadow::new(Color::srgba(0.0, 0.0, 0.0, 0.45), Val::Px(0.0), Val::Px(4.0), Val::Px(0.0), Val::Px(10.0)),
    )
}

/// A dark inset box (code entry, ammo counter background).
pub fn inset(mut node: Node) -> impl Bundle {
    node.border = UiRect::all(Val::Px(2.0));
    node.border_radius = BorderRadius::all(Val::Px(5.0));
    (node, BackgroundColor(INSET_BG), BorderColor::all(TAN_DARKER))
}

/// Native size of `ui/menu_bg.png`. The backdrop is always shown at this aspect ratio: it is
/// scaled uniformly to cover its parent and the overflow is cropped, never stretched.
const MENU_BG_SIZE: Vec2 = Vec2::new(1920.0, 1080.0);

/// Marks the title background node so `fit_menu_backdrop` can size it.
#[derive(Component)]
pub struct MenuBackdrop;

impl Theme {
    /// Full-screen title background image, slightly darkened so the panels read on top of it.
    /// Spawn as the first child of a full-screen root that has `Overflow::clip()`; it is positioned
    /// absolutely so it never affects the layout of the root's other children.
    pub fn menu_backdrop(&self) -> impl Bundle {
        (
            MenuBackdrop,
            ImageNode {
                image: self.menu_bg.clone(),
                image_mode: NodeImageMode::Stretch,
                color: Color::srgb(0.78, 0.78, 0.78),
                ..default()
            },
            Node { position_type: PositionType::Absolute, ..default() },
        )
    }
}

/// Keeps every `MenuBackdrop` at its native aspect ratio while covering its parent: the image is
/// scaled by the larger of the two axis ratios and centred, so the excess along the other axis is
/// cropped by the parent's clip. Runs every frame (before layout) so window resizes are picked up.
fn fit_menu_backdrop(
    mut backdrops: Query<(&ChildOf, &mut Node), With<MenuBackdrop>>,
    parents: Query<&ComputedNode>,
    window: Query<&Window, With<PrimaryWindow>>,
) {
    for (child_of, mut node) in &mut backdrops {
        let Ok(parent) = parents.get(child_of.parent()) else { continue };
        let mut avail = parent.size * parent.inverse_scale_factor;
        if avail.x <= 0.0 || avail.y <= 0.0 {
            // Freshly (re)built screen: the parent has not been laid out yet. It is always the
            // full-screen root, so size from the window rather than drawing the image at its
            // native size for a frame.
            let Ok(win) = window.single() else { continue };
            avail = Vec2::new(win.width(), win.height());
            if avail.x <= 0.0 || avail.y <= 0.0 {
                continue;
            }
        }
        let scale = (avail.x / MENU_BG_SIZE.x).max(avail.y / MENU_BG_SIZE.y);
        let size = MENU_BG_SIZE * scale;
        let offset = (avail - size) * 0.5;
        let (w, h, l, t) = (Val::Px(size.x), Val::Px(size.y), Val::Px(offset.x), Val::Px(offset.y));
        // Only write on change: a mutated `Node` re-runs layout, which we don't want every frame.
        if node.width != w || node.height != h || node.left != l || node.top != t {
            node.width = w;
            node.height = h;
            node.left = l;
            node.top = t;
        }
    }
}

/// Room codes are shown one glyph at a time: `A B C _ _ _`.
pub fn code_display(code: &str, len: usize) -> String {
    let mut out = String::new();
    for i in 0..len {
        if i > 0 {
            out.push(' ');
        }
        out.push(code.as_bytes().get(i).map(|b| *b as char).unwrap_or('_'));
    }
    out
}
