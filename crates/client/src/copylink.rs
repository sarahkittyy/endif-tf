//! The invite link in the private-room lobby: click to copy it, or click-drag to select part of
//! it like ordinary text (Ctrl+C copies the selection, double click selects everything).
//!
//! Bevy UI text is not selectable by itself, so the selection is drawn as a highlight node behind
//! the glyphs using the text layout's glyph positions.

use crate::theme::{self, Theme};
use bevy::clipboard::Clipboard;
use bevy::ecs::relationship::RelatedSpawnerCommands;
use bevy::prelude::*;
use bevy::text::{LineBreak, TextLayoutInfo};
use bevy::ui::UiGlobalTransform;
use bevy::window::{CursorIcon, PrimaryWindow, SystemCursorIcon};

/// The clickable box around the link.
#[derive(Component)]
struct LinkBox;

/// The link text itself.
#[derive(Component)]
struct LinkText(String);

/// Translucent bar drawn behind the selected glyphs.
#[derive(Component)]
struct SelectionHighlight;

/// "click to copy" / "copied!" line under the link.
#[derive(Component)]
struct CopyHint;

#[derive(Resource, Default)]
struct LinkSelection {
    /// Glyph index where the drag started, if a selection exists.
    anchor: Option<usize>,
    focus: usize,
    dragging: bool,
    press_pos: Vec2,
    last_click: f64,
    /// When the "copied!" hint should revert.
    copied_until: f64,
}

pub struct CopyLinkPlugin;

impl Plugin for CopyLinkPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<LinkSelection>().add_systems(Update, (link_input, link_visuals).chain());
    }
}

/// Spawns the link box: inset panel, highlight, text and the hint line.
pub fn spawn_link_box(c: &mut RelatedSpawnerCommands<ChildOf>, theme: &Theme, link: String) {
    c.spawn((
        LinkBox,
        Interaction::default(),
        theme::inset(Node {
            padding: UiRect::axes(Val::Px(14.0), Val::Px(8.0)),
            margin: UiRect::bottom(Val::Px(4.0)),
            justify_content: JustifyContent::Center,
            align_items: AlignItems::Center,
            ..default()
        }),
    ))
    .with_children(|b| {
        b.spawn((
            SelectionHighlight,
            Visibility::Hidden,
            Node { position_type: PositionType::Absolute, left: Val::Px(0.0), width: Val::Px(0.0), top: Val::Px(6.0), bottom: Val::Px(6.0), ..default() },
            BackgroundColor(Color::srgba(0.94, 0.81, 0.31, 0.35)),
        ));
        b.spawn((
            LinkText(link.clone()),
            theme.label(link, 18.0, theme::TAN_LIGHT),
            TextLayout { linebreak: LineBreak::NoWrap, ..default() },
        ));
    });
    c.spawn((
        CopyHint,
        theme.label("click to copy, or drag to select", 12.0, theme::TAN_DARK),
        Node { margin: UiRect::bottom(Val::Px(14.0)), ..default() },
    ));
}

/// Glyph index nearest to a cursor position (physical pixels within the text node); glyph
/// boundaries are at the centres of the glyph quads, so this returns 0..=glyph count.
fn glyph_index_at(layout: &TextLayoutInfo, local_x: f32) -> usize {
    let mut idx = 0;
    for g in &layout.glyphs {
        if local_x > g.position.x {
            idx += 1;
        }
    }
    idx.min(layout.glyphs.len())
}

/// Left/right edges (logical pixels, relative to the text node's left edge) of a glyph range.
fn glyph_span(layout: &TextLayoutInfo, from: usize, to: usize) -> Option<(f32, f32)> {
    let (a, b) = (from.min(to), from.max(to));
    if a == b || layout.glyphs.is_empty() {
        return None;
    }
    let first = layout.glyphs.get(a)?;
    let last = layout.glyphs.get(b - 1)?;
    let left = first.position.x - first.atlas_info.rect.width() * 0.5;
    let right = last.position.x + last.atlas_info.rect.width() * 0.5;
    let s = layout.scale_factor.max(0.01);
    Some((left / s, right / s))
}

#[allow(clippy::too_many_arguments)]
fn link_input(
    mouse: Res<ButtonInput<MouseButton>>,
    keys: Res<ButtonInput<KeyCode>>,
    time: Res<Time<Real>>,
    window: Query<&Window, With<PrimaryWindow>>,
    boxes: Query<&Interaction, With<LinkBox>>,
    texts: Query<(&LinkText, &TextLayoutInfo, &ComputedNode, &UiGlobalTransform)>,
    mut sel: ResMut<LinkSelection>,
    mut clipboard: ResMut<Clipboard>,
) {
    let Ok((link, layout, node, transform)) = texts.single() else {
        sel.anchor = None;
        sel.dragging = false;
        return;
    };
    let hovered = boxes.iter().any(|i| *i != Interaction::None);
    let now = time.elapsed_secs_f64();
    let cursor = window.single().ok().and_then(|w| w.physical_cursor_position());
    // Cursor position in physical pixels from the text node's left edge.
    let local_x = cursor.and_then(|c| node.normalize_point(*transform, c)).map(|p| (p.x + 0.5) * node.size().x);
    let n = layout.glyphs.len();

    if mouse.just_pressed(MouseButton::Left) {
        if hovered {
            let idx = local_x.map(|x| glyph_index_at(layout, x)).unwrap_or(0);
            let double = now - sel.last_click < 0.35;
            sel.last_click = now;
            sel.press_pos = cursor.unwrap_or_default();
            if double {
                sel.anchor = Some(0);
                sel.focus = n;
                sel.dragging = false;
            } else {
                sel.anchor = Some(idx);
                sel.focus = idx;
                sel.dragging = true;
            }
        } else {
            sel.anchor = None;
            sel.dragging = false;
        }
    }
    if sel.dragging && mouse.pressed(MouseButton::Left) {
        if let Some(x) = local_x {
            sel.focus = glyph_index_at(layout, x);
        }
    }
    if sel.dragging && mouse.just_released(MouseButton::Left) {
        sel.dragging = false;
        let moved = cursor.map(|c| c.distance(sel.press_pos)).unwrap_or(0.0);
        if moved < 4.0 || sel.anchor == Some(sel.focus) {
            // A plain click: copy the whole link.
            sel.anchor = None;
            if crate::webclip::copy(&mut clipboard, &link.0) {
                sel.copied_until = now + 1.5;
            }
        }
    }
    // Ctrl+C (or Cmd+C) copies the selected part.
    let modifier = keys.any_pressed([KeyCode::ControlLeft, KeyCode::ControlRight, KeyCode::SuperLeft, KeyCode::SuperRight]);
    if modifier && keys.just_pressed(KeyCode::KeyC)
        && let Some(a) = sel.anchor
        && a != sel.focus
    {
        // Glyph i is the i-th non-whitespace character (whitespace produces no glyph).
        let chars: Vec<char> = link.0.chars().filter(|c| !c.is_whitespace()).collect();
        let (lo, hi) = (a.min(sel.focus).min(chars.len()), a.max(sel.focus).min(chars.len()));
        let text: String = chars[lo..hi].iter().collect();
        if !text.is_empty() && crate::webclip::copy(&mut clipboard, &text) {
            sel.copied_until = now + 1.5;
        }
    }
}

fn link_visuals(
    mut commands: Commands,
    sel: Res<LinkSelection>,
    time: Res<Time<Real>>,
    window: Query<Entity, With<PrimaryWindow>>,
    boxes: Query<&Interaction, With<LinkBox>>,
    texts: Query<(&TextLayoutInfo, &ComputedNode, &UiGlobalTransform), With<LinkText>>,
    parents: Query<&ComputedNode, Without<LinkText>>,
    text_parent: Query<&ChildOf, With<LinkText>>,
    mut highlight: Query<(&mut Node, &mut Visibility), With<SelectionHighlight>>,
    mut hint: Query<(&mut Text, &mut TextColor), With<CopyHint>>,
    mut cursor_set: Local<bool>,
) {
    let Ok((layout, node, transform)) = texts.single() else {
        if *cursor_set && let Ok(w) = window.single() {
            commands.entity(w).insert(CursorIcon::System(SystemCursorIcon::Default));
            *cursor_set = false;
        }
        return;
    };
    // Text cursor while over the link.
    let hovered = boxes.iter().any(|i| *i != Interaction::None);
    if hovered != *cursor_set && let Ok(w) = window.single() {
        let icon = if hovered { SystemCursorIcon::Text } else { SystemCursorIcon::Default };
        commands.entity(w).insert(CursorIcon::System(icon));
        *cursor_set = hovered;
    }
    // Selection highlight, positioned relative to the box the text sits in.
    if let Ok((mut hl, mut vis)) = highlight.single_mut() {
        let span = sel.anchor.and_then(|a| glyph_span(layout, a, sel.focus));
        match span {
            Some((left, right)) => {
                // Text node offset inside its parent box (logical px).
                let offset = text_parent
                    .single()
                    .ok()
                    .and_then(|p| parents.get(p.parent()).ok())
                    .map(|parent| {
                        let text_left = transform.translation.x - node.size().x * 0.5;
                        let parent_left = transform.translation.x - parent.size().x * 0.5;
                        (text_left - parent_left) * node.inverse_scale_factor()
                    })
                    .unwrap_or(0.0);
                hl.left = Val::Px(offset + left);
                hl.width = Val::Px(right - left);
                *vis = Visibility::Visible;
            }
            None => *vis = Visibility::Hidden,
        }
    }
    if let Ok((mut text, mut color)) = hint.single_mut() {
        let copied = time.elapsed_secs_f64() < sel.copied_until;
        let want = if copied { "copied!" } else { "click to copy, or drag to select" };
        if text.0 != want {
            text.0 = want.to_string();
            color.0 = if copied { theme::YELLOW } else { theme::TAN_DARK };
        }
    }
}
