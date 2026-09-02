//! Single-line text fields for the account forms and the anonymous name box. Bevy UI has no text
//! input of its own, so this is a small one: click to focus, type, Backspace, Tab / Shift+Tab to
//! move between the fields on screen, Enter submits the form (Esc is left to the menu, which goes
//! back a screen). Password fields show `*`s, with an eye button that reveals the text while it is
//! held. Values live in the `Form` resource so they survive the screen being rebuilt.

use crate::theme::{self, Theme};
use bevy::ecs::relationship::RelatedSpawnerCommands;
use bevy::input::keyboard::{Key, KeyboardInput};
use bevy::platform::collections::HashMap;
use bevy::prelude::*;
use bevy::text::LineBreak;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Field {
    /// Username or e-mail, on the login screen.
    Login,
    Email,
    Username,
    NewUsername,
    CurrentPassword,
    Password,
    /// Confirmation of `Password`.
    Password2,
    /// The 6 digit e-mail code.
    Code,
    /// The name used while not logged in.
    AnonName,
    /// Opponent filter above the profile's match history.
    HistorySearch,
}

impl Field {
    /// Tab order.
    pub const ALL: [Field; 10] = [
        Field::Login,
        Field::Email,
        Field::Username,
        Field::NewUsername,
        Field::CurrentPassword,
        Field::Password,
        Field::Password2,
        Field::Code,
        Field::AnonName,
        Field::HistorySearch,
    ];

    pub fn masked(self) -> bool {
        matches!(self, Field::CurrentPassword | Field::Password | Field::Password2)
    }

    pub fn max_len(self) -> usize {
        match self {
            Field::Code => 6,
            Field::AnonName => crate::account::NAME_MAX,
            Field::Username | Field::NewUsername | Field::HistorySearch => 20,
            Field::Login | Field::Email => 254,
            _ => 128,
        }
    }

    /// Characters the field accepts.
    fn accepts(self, c: char) -> bool {
        match self {
            Field::Code => c.is_ascii_digit(),
            Field::Login | Field::Email => !c.is_whitespace() && !c.is_control(),
            Field::Username | Field::NewUsername => c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'),
            _ => !c.is_control(),
        }
    }
}

#[derive(Component)]
pub struct TextField {
    pub field: Field,
    pub placeholder: String,
}

/// The text child of a field.
#[derive(Component)]
struct FieldText(Field);

/// The eye button of a password field: hold to see the text.
#[derive(Component)]
struct RevealButton(Field);

/// The outline of the eye (a child of `RevealButton`); the pupil is its child.
#[derive(Component)]
struct EyeShape;

#[derive(Resource, Default)]
pub struct Form {
    values: HashMap<Field, String>,
    pub focus: Option<Field>,
    submitted: bool,
    /// The password field whose eye button is held down.
    revealed: Option<Field>,
}

impl Form {
    pub fn get(&self, f: Field) -> &str {
        self.values.get(&f).map(String::as_str).unwrap_or_default()
    }

    pub fn set(&mut self, f: Field, value: impl Into<String>) {
        self.values.insert(f, value.into());
    }

    pub fn clear(&mut self, f: Field) {
        self.values.remove(&f);
    }

    /// Drops every password field (after a login attempt, when leaving a form).
    pub fn clear_secrets(&mut self) {
        for f in [Field::CurrentPassword, Field::Password, Field::Password2] {
            self.values.remove(&f);
        }
    }

    /// True once, after Enter was pressed in a field.
    pub fn take_submit(&mut self) -> bool {
        std::mem::take(&mut self.submitted)
    }
}

pub struct TextFieldPlugin;

impl Plugin for TextFieldPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Form>().add_systems(Update, (field_click, field_keys, reveal_hold, field_display).chain());
    }
}

/// Spawns a field: an inset box showing the value (or its placeholder) that focuses on click.
pub fn spawn_field(c: &mut RelatedSpawnerCommands<ChildOf>, theme: &Theme, form: &Form, field: Field, placeholder: &str, width: f32) {
    let focused = form.focus == Some(field);
    let (text, color) = display(form.get(field), field, focused, placeholder, form.revealed == Some(field));
    // Password fields keep room on the right for the eye button.
    let right_pad = if field.masked() { EYE_W + 4.0 } else { 10.0 };
    let node = Node {
        width: Val::Px(width),
        height: Val::Px(38.0),
        padding: UiRect::new(Val::Px(10.0), Val::Px(right_pad), Val::Px(0.0), Val::Px(0.0)),
        align_items: AlignItems::Center,
        overflow: Overflow::clip(),
        margin: UiRect::vertical(Val::Px(3.0)),
        border: UiRect::all(Val::Px(2.0)),
        border_radius: BorderRadius::all(Val::Px(5.0)),
        ..default()
    };
    let border = BorderColor::all(if focused { theme::YELLOW } else { theme::TAN_DARKER });
    c.spawn((Button, TextField { field, placeholder: placeholder.to_string() }, node, BackgroundColor(theme::INSET_BG), border)).with_children(|b| {
        b.spawn((FieldText(field), theme.label(text, 16.0, color), TextLayout { linebreak: LineBreak::NoWrap, ..default() }));
        if field.masked() {
            spawn_eye(b, field);
        }
    });
}

/// Width of the eye button inside a password field.
const EYE_W: f32 = 30.0;

const EYE_IDLE: Color = theme::TAN_DARK;
const EYE_HOT: Color = theme::TAN_LIGHT;

/// The eye button: a pill outline with a round pupil, filling the right edge of the field.
fn spawn_eye(b: &mut RelatedSpawnerCommands<ChildOf>, field: Field) {
    b.spawn((
        Button,
        RevealButton(field),
        Node {
            position_type: PositionType::Absolute,
            right: Val::Px(0.0),
            top: Val::Px(0.0),
            bottom: Val::Px(0.0),
            width: Val::Px(EYE_W),
            justify_content: JustifyContent::Center,
            align_items: AlignItems::Center,
            ..default()
        },
        BackgroundColor(Color::NONE),
    ))
    .with_children(|e| {
        e.spawn((
            EyeShape,
            Node {
                width: Val::Px(20.0),
                height: Val::Px(12.0),
                border: UiRect::all(Val::Px(2.0)),
                border_radius: BorderRadius::all(Val::Percent(50.0)),
                justify_content: JustifyContent::Center,
                align_items: AlignItems::Center,
                ..default()
            },
            BorderColor::all(EYE_IDLE),
        ))
        .with_children(|pupil| {
            pupil.spawn((Node { width: Val::Px(5.0), height: Val::Px(5.0), border_radius: BorderRadius::all(Val::Percent(50.0)), ..default() }, BackgroundColor(EYE_IDLE)));
        });
    });
}

/// What a field shows: the placeholder while it is empty (focused or not), otherwise the value,
/// masked for passwords unless the eye is held, with a caret while focused.
fn display(value: &str, field: Field, focused: bool, placeholder: &str, revealed: bool) -> (String, Color) {
    if value.is_empty() {
        return (placeholder.to_string(), theme::TAN_DARK);
    }
    let mut shown = if field.masked() && !revealed { "*".repeat(value.chars().count()) } else { value.to_string() };
    if focused {
        shown.push('|');
    }
    (shown, theme::TAN_LIGHT)
}

fn field_click(q: Query<(&Interaction, &TextField), Changed<Interaction>>, mut form: ResMut<Form>) {
    for (interaction, field) in &q {
        if *interaction == Interaction::Pressed && form.focus != Some(field.field) {
            form.focus = Some(field.field);
        }
    }
}

fn field_keys(
    mut keys: MessageReader<KeyboardInput>,
    key_state: Res<ButtonInput<KeyCode>>,
    mut form: ResMut<Form>,
    fields: Query<&TextField>,
) {
    let Some(focus) = form.focus else {
        keys.clear();
        return;
    };
    // The fields on screen, in tab order.
    let present: Vec<Field> = Field::ALL.into_iter().filter(|f| fields.iter().any(|t| t.field == *f)).collect();
    if !present.contains(&focus) {
        // The screen changed under the focus.
        form.focus = None;
        keys.clear();
        return;
    }
    let modifier = key_state.any_pressed([KeyCode::ControlLeft, KeyCode::ControlRight, KeyCode::SuperLeft, KeyCode::SuperRight, KeyCode::AltLeft, KeyCode::AltRight]);
    let shift = key_state.any_pressed([KeyCode::ShiftLeft, KeyCode::ShiftRight]);
    for ev in keys.read() {
        if !ev.state.is_pressed() {
            continue;
        }
        match &ev.logical_key {
            Key::Character(_) | Key::Space if modifier => {}
            Key::Character(c) => push(&mut form, focus, c.as_str()),
            Key::Space => push(&mut form, focus, " "),
            Key::Backspace => {
                if let Some(v) = form.values.get_mut(&focus) {
                    v.pop();
                }
            }
            Key::Tab => {
                let i = present.iter().position(|f| *f == focus).unwrap_or(0);
                let n = present.len();
                let next = if shift { (i + n - 1) % n } else { (i + 1) % n };
                form.focus = Some(present[next]);
            }
            Key::Enter => form.submitted = true,
            // The anonymous name box lives on the title screen, where Esc has nothing else to do;
            // in a form Esc goes back a screen (handled by the menu) and takes the focus with it.
            Key::Escape if focus == Field::AnonName => form.focus = None,
            _ => {}
        }
    }
}

/// Appends typed or pasted text to a field, keeping only what the field accepts.
pub fn push(form: &mut Form, field: Field, text: &str) {
    let value = form.values.entry(field).or_default();
    for c in text.chars().filter(|c| field.accepts(*c)) {
        if value.chars().count() >= field.max_len() {
            break;
        }
        value.push(c);
    }
}

/// Reveals a password while its eye button is held, and lights the eye up under the pointer.
fn reveal_hold(
    buttons: Query<(&Interaction, &RevealButton, &Children), Changed<Interaction>>,
    eyes: Query<&RevealButton>,
    mut form: ResMut<Form>,
    mut shapes: Query<(&mut BorderColor, &Children), With<EyeShape>>,
    mut pupils: Query<&mut BackgroundColor>,
) {
    // The screen was rebuilt while the eye was held: it never reports a release.
    if let Some(f) = form.revealed
        && !eyes.iter().any(|e| e.0 == f)
    {
        form.revealed = None;
    }
    for (interaction, reveal, children) in &buttons {
        match interaction {
            Interaction::Pressed => form.revealed = Some(reveal.0),
            _ if form.revealed == Some(reveal.0) => form.revealed = None,
            _ => {}
        }
        let color = if *interaction == Interaction::None { EYE_IDLE } else { EYE_HOT };
        for child in children.iter() {
            let Ok((mut border, parts)) = shapes.get_mut(child) else { continue };
            *border = BorderColor::all(color);
            for part in parts.iter() {
                if let Ok(mut bg) = pupils.get_mut(part) {
                    *bg = BackgroundColor(color);
                }
            }
        }
    }
}

fn field_display(
    form: Res<Form>,
    mut boxes: Query<(&TextField, &mut BorderColor)>,
    mut texts: Query<(&FieldText, &mut Text, &mut TextColor)>,
) {
    let placeholders: HashMap<Field, String> = boxes.iter().map(|(t, _)| (t.field, t.placeholder.clone())).collect();
    for (field, mut border) in &mut boxes {
        let want = BorderColor::all(if form.focus == Some(field.field) { theme::YELLOW } else { theme::TAN_DARKER });
        if *border != want {
            *border = want;
        }
    }
    for (field, mut text, mut color) in &mut texts {
        let placeholder = placeholders.get(&field.0).map(String::as_str).unwrap_or_default();
        let (shown, c) = display(form.get(field.0), field.0, form.focus == Some(field.0), placeholder, form.revealed == Some(field.0));
        if text.0 != shown {
            text.0 = shown;
        }
        if color.0 != c {
            color.0 = c;
        }
    }
}
