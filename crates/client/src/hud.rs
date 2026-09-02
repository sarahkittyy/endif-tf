//! In-game overlay in the TF2 HUD style: crosshair, team-coloured score with the match clock,
//! rocket counter, height meter with the airshot line, focus/connection hints, the kill feed and
//! the round banner.

use crate::game::{MouseCaptured, PendingFx, RenderStates};
use crate::net::{LocalHandle, MatchKind, PlayerNames};
use crate::theme::{self, Theme};
use crate::{AppState, GameEntity};
use bevy::ecs::system::ParamSet;
use bevy::prelude::*;
use endif_sim::{MGE_ENDIF_AIRSHOT_HEIGHT, SimEvent};

#[derive(Resource, Default)]
pub struct NetStatus {
    pub text: String,
    pub ping_ms: u32,
    pub frames_ahead: i32,
}

#[derive(Component)]
struct ScoreMe;
#[derive(Component)]
struct ScoreThem;
/// Name under a score box: the player at this GGRS handle.
#[derive(Component)]
struct NameOf(usize);
#[derive(Component)]
struct TimerText;
#[derive(Component)]
struct ClipText;
#[derive(Component)]
struct ReserveText;
#[derive(Component)]
struct StatusText;
#[derive(Component)]
struct HeightText;
#[derive(Component)]
struct HeightBar;
#[derive(Component)]
struct SpeedText;
#[derive(Component)]
struct FlashText {
    until: f64,
}
/// The kill feed column (top right); kills are appended as children, newest at the bottom.
#[derive(Component)]
struct KillFeed;
#[derive(Component)]
struct KillFeedEntry {
    born: f64,
}
/// Base alpha of a kill feed element; the fade-out scales it toward zero.
#[derive(Component)]
struct Fades(f32);

/// How long a kill feed line stays fully visible, then how long it takes to fade away.
const KILL_HOLD_SECS: f64 = 2.0;
const KILL_FADE_SECS: f64 = 0.5;
const KILL_MAX_LINES: usize = 5;

pub struct HudPlugin;

impl Plugin for HudPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<NetStatus>()
            .add_systems(OnEnter(AppState::InGame), setup_hud)
            .add_systems(Update, (update_hud, kill_flashes, kill_feed).run_if(in_state(AppState::InGame)));
    }
}

/// A full-width invisible strip used to centre its children horizontally.
fn centred_strip(top: Val) -> Node {
    Node {
        position_type: PositionType::Absolute,
        left: Val::Px(0.0),
        right: Val::Px(0.0),
        top,
        flex_direction: FlexDirection::Column,
        justify_content: JustifyContent::FlexStart,
        align_items: AlignItems::Center,
        row_gap: Val::Px(6.0),
        ..default()
    }
}

fn score_box(theme: &Theme, color: Color, marker: impl Component, handle: usize) -> impl Bundle {
    (
        Node {
            min_width: Val::Px(64.0),
            height: Val::Px(64.0),
            padding: UiRect::horizontal(Val::Px(6.0)),
            flex_direction: FlexDirection::Column,
            justify_content: JustifyContent::Center,
            align_items: AlignItems::Center,
            border: UiRect::all(Val::Px(2.0)),
            border_radius: BorderRadius::all(Val::Px(6.0)),
            ..default()
        },
        BackgroundColor(color),
        BorderColor::all(theme::TAN_LIGHT),
        children![
            (marker, theme.heading("0", 36.0, theme::TAN_LIGHT)),
            (NameOf(handle), theme.label("", 11.0, theme::TAN_LIGHT), TextLayout { linebreak: bevy::text::LineBreak::NoWrap, ..default() }),
        ],
    )
}

fn setup_hud(mut commands: Commands, theme: Res<Theme>, local: Res<LocalHandle>) {
    let (me, them) = (local.0, 1 - local.0);
    // Crosshair: a small cross with a dark outline.
    for (w, h) in [(16.0, 2.0), (2.0, 16.0)] {
        commands.spawn((
            GameEntity,
            Node {
                position_type: PositionType::Absolute,
                left: Val::Percent(50.0),
                top: Val::Percent(50.0),
                width: Val::Px(w),
                height: Val::Px(h),
                margin: UiRect::new(Val::Px(-w / 2.0), Val::Px(0.0), Val::Px(-h / 2.0), Val::Px(0.0)),
                ..default()
            },
            BackgroundColor(Color::srgb(0.3, 1.0, 0.3)),
            Outline { width: Val::Px(1.0), offset: Val::Px(0.0), color: Color::srgba(0.0, 0.0, 0.0, 0.7) },
        ));
    }

    // Top centre: RED (you) vs BLU (them) score with the frag limit between, match clock below.
    commands.spawn((GameEntity, centred_strip(Val::Px(12.0)))).with_children(|p| {
        p.spawn(theme::panel(Node {
            flex_direction: FlexDirection::Row,
            align_items: AlignItems::Center,
            column_gap: Val::Px(10.0),
            padding: UiRect::axes(Val::Px(10.0), Val::Px(6.0)),
            ..default()
        }))
        .with_children(|r| {
            r.spawn(score_box(&theme, theme::RED_TEAM, ScoreMe, me));
            r.spawn(Node {
                flex_direction: FlexDirection::Column,
                align_items: AlignItems::Center,
                row_gap: Val::Px(2.0),
                ..default()
            })
            .with_children(|c| {
                c.spawn(theme.soldier_icon(28.0));
                c.spawn(theme.heading_flat("FIRST TO 5", 12.0, theme::OFF_WHITE));
            });
            r.spawn(score_box(&theme, theme::BLU_TEAM, ScoreThem, them));
        });
        // Match clock, like the TF2 round timer.
        p.spawn(theme::panel(Node {
            padding: UiRect::axes(Val::Px(18.0), Val::Px(2.0)),
            justify_content: JustifyContent::Center,
            align_items: AlignItems::Center,
            min_width: Val::Px(96.0),
            ..default()
        }))
        .with_children(|t| {
            t.spawn((TimerText, theme.heading("0:00", 24.0, theme::TAN_LIGHT)));
        });
    });

    // Bottom right: rocket counter (clip / reserve) like the TF2 ammo panel.
    commands
        .spawn((
            GameEntity,
            theme::panel(Node {
                position_type: PositionType::Absolute,
                bottom: Val::Px(24.0),
                right: Val::Px(28.0),
                flex_direction: FlexDirection::Row,
                align_items: AlignItems::FlexEnd,
                column_gap: Val::Px(8.0),
                padding: UiRect::new(Val::Px(18.0), Val::Px(18.0), Val::Px(4.0), Val::Px(8.0)),
                ..default()
            }),
        ))
        .with_children(|p| {
            p.spawn((ClipText, theme.heading("4", 64.0, theme::TAN_LIGHT)));
            p.spawn(Node { flex_direction: FlexDirection::Column, align_items: AlignItems::FlexStart, margin: UiRect::bottom(Val::Px(12.0)), ..default() })
                .with_children(|c| {
                    c.spawn((ReserveText, theme.heading("∞", 30.0, theme::OFF_WHITE)));
                    c.spawn(theme.label("ROCKETS", 11.0, theme::OFF_WHITE));
                });
        });

    // Bottom left: height above the floor with the airshot line as a bar.
    commands
        .spawn((
            GameEntity,
            theme::panel(Node {
                position_type: PositionType::Absolute,
                bottom: Val::Px(24.0),
                left: Val::Px(28.0),
                flex_direction: FlexDirection::Column,
                align_items: AlignItems::FlexStart,
                padding: UiRect::new(Val::Px(16.0), Val::Px(16.0), Val::Px(6.0), Val::Px(10.0)),
                row_gap: Val::Px(2.0),
                min_width: Val::Px(200.0),
                ..default()
            }),
        ))
        .with_children(|p| {
            p.spawn(Node { flex_direction: FlexDirection::Row, align_items: AlignItems::FlexEnd, column_gap: Val::Px(8.0), ..default() })
                .with_children(|r| {
                    r.spawn((HeightText, theme.heading("0", 40.0, theme::TAN_LIGHT)));
                    r.spawn((theme.label("HEIGHT", 11.0, theme::OFF_WHITE), Node { margin: UiRect::bottom(Val::Px(9.0)), ..default() }));
                });
            // Bar: fills up to the red line, then turns red.
            p.spawn(theme::inset(Node { width: Val::Px(180.0), height: Val::Px(12.0), ..default() })).with_children(|bar| {
                bar.spawn((
                    HeightBar,
                    Node {
                        width: Val::Percent(0.0),
                        height: Val::Percent(100.0),
                        border_radius: BorderRadius::all(Val::Px(3.0)),
                        ..default()
                    },
                    BackgroundColor(theme::YELLOW),
                ));
            });
            p.spawn((SpeedText, theme.label("", 13.0, theme::OFF_WHITE)));
        });

    // Top left: focus hint + connection warnings.
    commands
        .spawn((
            GameEntity,
            theme::panel(Node {
                position_type: PositionType::Absolute,
                top: Val::Px(12.0),
                left: Val::Px(12.0),
                padding: UiRect::axes(Val::Px(10.0), Val::Px(6.0)),
                ..default()
            }),
        ))
        .with_children(|p| {
            p.spawn((StatusText, theme.label("click to focus - Esc for menu", 13.0, theme::OFF_WHITE)));
        });

    // Top right: kill feed. Lines are spawned by `kill_feed` as they happen.
    commands.spawn((
        GameEntity,
        KillFeed,
        Node {
            position_type: PositionType::Absolute,
            top: Val::Px(12.0),
            right: Val::Px(12.0),
            flex_direction: FlexDirection::Column,
            align_items: AlignItems::FlexEnd,
            row_gap: Val::Px(4.0),
            ..default()
        },
    ));

    // Centre banner (round result).
    commands.spawn((GameEntity, centred_strip(Val::Percent(26.0)))).with_children(|p| {
        p.spawn((
            FlashText { until: 0.0 },
            theme.heading("", 56.0, theme::YELLOW),
            TextLayout { justify: Justify::Center, ..default() },
        ));
    });
}

/// A HUD label marked `T`. `Without<NameOf>` keeps these queries disjoint from the name tags
/// (`name_texts` below), which also write `Text`; otherwise Bevy rejects the system (B0001).
type Hud<T> = (With<T>, Without<NameOf>);

#[allow(clippy::type_complexity)]
fn update_hud(
    states: Option<Res<RenderStates>>,
    local: Res<LocalHandle>,
    status: Res<NetStatus>,
    kind: Option<Res<MatchKind>>,
    names: Res<PlayerNames>,
    captured: Res<MouseCaptured>,
    arena: Res<crate::game::ArenaRes>,
    mut texts: ParamSet<(
        Query<&mut Text, Hud<ScoreMe>>,
        Query<&mut Text, Hud<ScoreThem>>,
        Query<(&mut Text, &mut TextColor, &mut TextFont), Hud<ClipText>>,
        Query<&mut Text, Hud<ReserveText>>,
        Query<&mut Text, Hud<StatusText>>,
        Query<(&mut Text, &mut TextColor), Hud<HeightText>>,
        Query<&mut Text, Hud<SpeedText>>,
        Query<&mut Text, Hud<TimerText>>,
    )>,
    mut bar: Query<(&mut Node, &mut BackgroundColor), With<HeightBar>>,
    mut name_texts: Query<(&NameOf, &mut Text)>,
) {
    let Some(states) = states else { return };
    for (who, mut t) in &mut name_texts {
        let want = names.short(who.0);
        if t.0 != want {
            t.0 = want;
        }
    }
    let me = local.0;
    let them = 1 - me;
    let s = &states.cur;
    let p = &s.players[me];

    if let Ok(mut t) = texts.p0().single_mut() {
        t.0 = s.players[me].score.to_string();
    }
    if let Ok(mut t) = texts.p1().single_mut() {
        t.0 = s.players[them].score.to_string();
    }
    if let Ok((mut t, mut color, mut font)) = texts.p2().single_mut() {
        if p.alive {
            t.0 = p.clip.to_string();
            color.0 = if p.clip == 0 { theme::DEATH_RED } else { theme::TAN_LIGHT };
            font.font_size = FontSize::Px(64.0);
        } else {
            t.0 = "RESPAWNING".to_string();
            color.0 = theme::LIGHT_RED;
            font.font_size = FontSize::Px(26.0);
        }
    }
    if let Ok(mut t) = texts.p3().single_mut() {
        t.0 = if p.alive { "∞".to_string() } else { String::new() };
    }

    let h = p.origin.z - arena.0.floor_z();
    let speed = p.velocity.length_2d();
    let above = h >= MGE_ENDIF_AIRSHOT_HEIGHT;
    if let Ok((mut t, mut color)) = texts.p5().single_mut() {
        t.0 = format!("{h:.0}");
        color.0 = if above { theme::DEATH_RED } else { theme::TAN_LIGHT };
    }
    if let Ok(mut t) = texts.p6().single_mut() {
        t.0 = format!("SPEED {speed:.0} U/S");
    }
    if let Ok((mut node, mut color)) = bar.single_mut() {
        // The bar spans 0 .. 1.5x the airshot height so "above the line" still has room to grow.
        let frac = (h / (MGE_ENDIF_AIRSHOT_HEIGHT * 1.5)).clamp(0.0, 1.0);
        node.width = Val::Percent(frac * 100.0);
        color.0 = if above { theme::DEATH_RED } else { theme::YELLOW };
    }

    if let Ok(mut t) = texts.p4().single_mut() {
        let focus = if captured.0 { "Tab to unfocus" } else { "click to focus" };
        let net = match kind.as_deref() {
            // Only warnings are shown ("connection interrupted", "desync ..."), never "connected".
            Some(MatchKind::Room { .. } | MatchKind::Quick(_)) if status.text != "connected" && !status.text.is_empty() => {
                format!("\nping {} ms - {}", status.ping_ms, status.text)
            }
            Some(MatchKind::Room { .. } | MatchKind::Quick(_)) => format!("\nping {} ms", status.ping_ms),
            _ => String::new(),
        };
        t.0 = format!("{focus} - Esc for menu{net}");
    }
    if let Ok(mut t) = texts.p7().single_mut() {
        let secs = s.curtime().max(0.0) as u32;
        t.0 = format!("{}:{:02}", secs / 60, secs % 60);
    }
}

fn kill_flashes(
    fx: Res<PendingFx>,
    local: Res<LocalHandle>,
    time: Res<Time<Real>>,
    mut q: Query<(&mut Text, &mut TextColor, &mut FlashText)>,
) {
    let Ok((mut text, mut color, mut flash)) = q.single_mut() else { return };
    let now = time.elapsed_secs_f64();
    for ev in &fx.events {
        if let SimEvent::RoundWon { winner, score } = ev {
            let won = *winner as usize == local.0;
            text.0 = if won {
                format!("VICTORY!
{} - {}", score[local.0], score[1 - local.0])
            } else {
                format!("ROUND LOST
{} - {}", score[local.0], score[1 - local.0])
            };
            color.0 = if won { theme::YELLOW } else { theme::LIGHT_RED };
            flash.until = now + 3.5;
        }
    }
    if now > flash.until && !text.0.is_empty() {
        text.0.clear();
    }
}

/// TF2-style death notices: `killer [soldier] victim  N U`, names in team colours (RED is the
/// local player, BLU the opponent). Each line holds for two seconds, fades, then is removed.
#[allow(clippy::type_complexity, clippy::too_many_arguments)]
fn kill_feed(
    mut commands: Commands,
    fx: Res<PendingFx>,
    local: Res<LocalHandle>,
    names: Res<PlayerNames>,
    theme: Res<Theme>,
    time: Res<Time<Real>>,
    feed: Query<(Entity, Option<&Children>), With<KillFeed>>,
    mut entries: Query<(Entity, &KillFeedEntry, &Fades, &Children, &mut BackgroundColor, &mut BorderColor)>,
    mut parts: Query<(&Fades, Option<&mut TextColor>, Option<&mut ImageNode>), Without<KillFeedEntry>>,
) {
    let Ok((feed, lines)) = feed.single() else { return };
    let now = time.elapsed_secs_f64();

    // Drop lines that have finished fading, and the oldest ones if the feed is getting long.
    let mut live: Vec<Entity> = lines.map(|c| c.iter().collect()).unwrap_or_default();
    live.retain(|&e| match entries.get(e) {
        Ok((_, entry, ..)) if now - entry.born > KILL_HOLD_SECS + KILL_FADE_SECS => {
            commands.entity(e).despawn();
            false
        }
        Ok(_) => true,
        Err(_) => false,
    });

    let fresh: Vec<(u8, u8, f32)> = fx
        .events
        .iter()
        .filter_map(|ev| match ev {
            SimEvent::PlayerHit { attacker, victim, airshot_kill: true, distance, .. } if attacker != victim => Some((*attacker, *victim, *distance)),
            _ => None,
        })
        .collect();
    let overflow = (live.len() + fresh.len()).saturating_sub(KILL_MAX_LINES);
    for &e in live.iter().take(overflow) {
        commands.entity(e).despawn();
    }

    for (attacker, victim, distance) in fresh {
        let entry = spawn_kill_line(&mut commands, &theme, &names, local.0, attacker as usize, victim as usize, distance, now);
        commands.entity(feed).add_child(entry);
    }

    // Fade: scale every element's base alpha once the hold time is up.
    for (_, entry, base, children, mut bg, mut border) in &mut entries {
        let age = now - entry.born;
        let a = if age <= KILL_HOLD_SECS { 1.0 } else { (1.0 - (age - KILL_HOLD_SECS) / KILL_FADE_SECS).clamp(0.0, 1.0) } as f32;
        bg.0 = bg.0.with_alpha(base.0 * a);
        *border = BorderColor::all(border.top.with_alpha(a));
        for child in children.iter() {
            if let Ok((base, text, image)) = parts.get_mut(child) {
                if let Some(mut t) = text {
                    t.0 = t.0.with_alpha(base.0 * a);
                }
                if let Some(mut i) = image {
                    i.color = i.color.with_alpha(base.0 * a);
                }
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn spawn_kill_line(commands: &mut Commands, theme: &Theme, names: &PlayerNames, local: usize, attacker: usize, victim: usize, distance: f32, now: f64) -> Entity {
    let team = |h: usize| if h == local { theme::RED_TEAM } else { theme::BLU_TEAM };
    let name = |h: usize| -> String {
        let n = names.0.get(h).map(String::as_str).unwrap_or_default();
        if n.is_empty() { "???".to_string() } else { n.chars().take(16).collect() }
    };
    // Text boxes are trimmed to the glyph height (no line spacing) so that, with the row centred,
    // names, icon and distance all sit on the same visual middle line.
    let text = |s: String, color: Color| {
        (
            Fades(1.0),
            theme.label(s, 14.0, color),
            bevy::text::LineHeight::RelativeToFont(1.0),
            TextLayout { linebreak: bevy::text::LineBreak::NoWrap, ..default() },
        )
    };
    commands
        .spawn((
            KillFeedEntry { born: now },
            Fades(theme::PANEL_BG.alpha()),
            Node {
                height: Val::Px(30.0),
                flex_direction: FlexDirection::Row,
                align_items: AlignItems::Center,
                column_gap: Val::Px(8.0),
                padding: UiRect::horizontal(Val::Px(10.0)),
                border: UiRect::all(Val::Px(2.0)),
                border_radius: BorderRadius::all(Val::Px(6.0)),
                ..default()
            },
            BackgroundColor(theme::PANEL_BG),
            BorderColor::all(theme::TAN_DARK),
            children![
                text(name(attacker), team(attacker)),
                (Fades(1.0), theme.soldier_icon(20.0)),
                text(name(victim), team(victim)),
                text(format!("{distance:.0} U"), theme::OFF_WHITE),
            ],
        ))
        .id()
}
