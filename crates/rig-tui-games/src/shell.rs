//! Focus, placement and the clock belong to the panel, never to a game board.

use std::time::{Duration, Instant};

use bevy_ecs::prelude::*;
use rig_harness::commands::CommandArgs;
use rig_harness::main_agent::ShownAgent;
use rig_harness::prelude::{KeepAwake, Notice};
use rig_tui::crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers};
use rig_tui::ratatui::layout::{Constraint, Rect};
use rig_tui::ratatui::style::{Modifier, Style};
use rig_tui::ratatui::text::Line;
use rig_tui::ratatui::widgets::{Paragraph, Widget, Wrap};
use rig_tui::{
    InputAction, InputContext, KeyFocus, KeyInput, KeyScope, PanelCanvas, PendingInput, Placement,
    RequestRedraw, TerminalFocus, TerminalSize, Theme, TuiPanel,
};

use crate::{Game, GamesSettings, Kind, Location};

const FRAME: Duration = Duration::from_millis(50);

#[derive(Event)]
pub(super) struct Open {
    pub(super) args: CommandArgs,
    pub(super) kind: Kind,
}

#[derive(Component)]
pub(super) struct Session {
    kind: Kind,
    game: Game,
    owner: Entity,
    previous_focus: Option<Entity>,
    location: Location,
    paused: bool,
    suspended: bool,
    playable: bool,
    last_tick: Instant,
}

pub(super) fn open(
    event: On<Open>,
    mut commands: Commands,
    mut sessions: Query<(Entity, &mut Session, Has<TuiPanel>)>,
    mut focus: ResMut<KeyFocus>,
    settings: Res<GamesSettings>,
    screen: Res<TerminalSize>,
    mut notices: MessageWriter<Notice>,
) {
    // A non-terminal front has no terminal dimensions and must not capture keys.
    if screen.0.is_empty() {
        notices.write(Notice::error(
            event.args.agent,
            "Games need the terminal frontend.",
        ));
        return;
    }
    let location = match event.args.args.as_str() {
        "" => None,
        "popup" => Some(Location::Popup),
        "right" => Some(Location::Right),
        _ => {
            notices.write(Notice::error(
                event.args.agent,
                "Use the game command alone, or with popup or right.",
            ));
            return;
        }
    };
    let now = Instant::now();
    let mut previous_focus = focus.0;
    for (entity, mut session, visible) in &mut sessions {
        if visible {
            if focus.0 == Some(entity) {
                previous_focus = session.previous_focus;
            }
            if session.kind != event.kind {
                session.paused = true;
                session.suspended = true;
                commands.entity(entity).remove::<(TuiPanel, KeepAwake)>();
            }
        }
    }
    if let Some((entity, mut session, _)) = sessions
        .iter_mut()
        .find(|(_, session, _)| session.kind == event.kind)
    {
        session.owner = event.args.agent;
        session.previous_focus = previous_focus;
        session.location = location.unwrap_or(session.location);
        session.paused = false;
        session.suspended = true;
        session.playable = false;
        session.last_tick = now;
        commands
            .entity(entity)
            .insert(TuiPanel::new(placement(&session, screen.0)));
        focus.0 = Some(entity);
        return;
    }
    // At most one retained session per game: board values live until the app
    // exits. Hidden sessions have no panel or wake interval and do no work.
    let session = Session {
        kind: event.kind,
        game: event.kind.start(),
        owner: event.args.agent,
        previous_focus,
        location: location.unwrap_or(settings.placement),
        paused: false,
        suspended: true,
        playable: false,
        last_tick: now,
    };
    let panel = TuiPanel::new(placement(&session, screen.0));
    focus.0 = Some(commands.spawn((Name::new("tui:game"), session, panel)).id());
}

fn dimensions(kind: Kind) -> (u16, u16) {
    let (width, height) = kind.size();
    (width.saturating_add(2).max(34), height.saturating_add(4))
}

fn placement(session: &Session, screen: Rect) -> Placement {
    let (width, height) = dimensions(session.kind);
    // Keep at least forty columns for chat. An undersized side panel becomes
    // an overlay, rather than silently disappearing with focus still captured.
    if session.location == Location::Right
        && screen.width >= width.saturating_add(40)
        && screen.height >= height.saturating_add(5)
    {
        Placement::Right(Constraint::Length(width))
    } else {
        Placement::Over {
            width: Constraint::Length(width),
            height: Constraint::Length(height),
        }
    }
}

fn modal(context: &InputContext) -> bool {
    context.picking().is_some() || context.answering().is_some() || context.completion().is_some()
}

pub(super) fn input(
    mut pending: ResMut<PendingInput>,
    context: Res<InputContext>,
    mut sessions: Query<(Entity, &mut Session), With<TuiPanel>>,
    mut focus: ResMut<KeyFocus>,
    panels: Query<(), With<TuiPanel>>,
    mut commands: Commands,
    mut redraw: MessageWriter<RequestRedraw>,
) {
    let Ok((entity, mut session)) = sessions.single_mut() else {
        return;
    };
    if focus.0 != Some(entity) || context.scope() != KeyScope::Panel(entity) || modal(&context) {
        return;
    }
    let mut hidden = false;
    pending.0.retain(|input| {
        // Do not route the remainder of this input batch into the editor after
        // hiding; the next physical key is handled with the restored context.
        if hidden {
            return !matches!(
                input,
                KeyInput::Event(Event::Key(_) | Event::Paste(_)) | KeyInput::Action(_)
            );
        }
        let key = match input {
            KeyInput::Event(Event::Key(key)) => key,
            KeyInput::Event(Event::Paste(_) | Event::Mouse(_)) => return false,
            KeyInput::Action(InputAction::Stop) => return true,
            KeyInput::Action(_) => return false,
            _ => return true,
        };
        // Keep Rig's interrupt available while a game is open.
        if key.code == KeyCode::Char('c') && key.modifiers == KeyModifiers::CONTROL {
            return true;
        }
        if key.kind == KeyEventKind::Release {
            return false;
        }
        if key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER)
        {
            return false;
        }
        match key.code {
            KeyCode::Esc => {
                focus.0 = session
                    .previous_focus
                    .filter(|previous| panels.contains(*previous));
                session.paused = true;
                session.suspended = true;
                commands.entity(entity).remove::<(TuiPanel, KeepAwake)>();
                hidden = true;
            }
            KeyCode::Char('p' | 'P') if key.kind == KeyEventKind::Press => {
                session.paused = !session.paused;
                session.last_tick = Instant::now();
            }
            KeyCode::Char('r' | 'R') if key.kind == KeyEventKind::Press => {
                session.game = session.kind.start();
                session.paused = false;
                session.last_tick = Instant::now();
            }
            _ if session.playable && !session.paused && !session.suspended => {
                session.game.key(key.code);
            }
            _ => {}
        }
        redraw.write(RequestRedraw);
        false
    });
}

pub(super) fn prepare(
    mut sessions: Query<(Entity, &mut Session, &mut TuiPanel)>,
    context: Res<InputContext>,
    mut focus: ResMut<KeyFocus>,
    terminal_focus: Res<TerminalFocus>,
    shown: Res<ShownAgent>,
    screen: Res<TerminalSize>,
    panels: Query<(), With<TuiPanel>>,
    mut commands: Commands,
    mut redraw: MessageWriter<RequestRedraw>,
) {
    let now = Instant::now();
    for (entity, mut session, mut panel) in &mut sessions {
        if shown.0 != Some(session.owner) {
            if focus.0 == Some(entity) {
                focus.0 = session
                    .previous_focus
                    .filter(|previous| panels.contains(*previous));
            }
            session.paused = true;
            session.suspended = true;
            commands.entity(entity).remove::<(TuiPanel, KeepAwake)>();
            continue;
        }
        panel.set_if_neq(TuiPanel::new(placement(&session, screen.0)));
        let suspended =
            focus.0 != Some(entity) || terminal_focus.0 == Some(false) || modal(&context);
        let elapsed = now
            .duration_since(session.last_tick)
            .min(Duration::from_millis(100));
        session.last_tick = now;
        if session.suspended != suspended {
            session.suspended = suspended;
            redraw.write(RequestRedraw);
        }
        if session.kind.realtime()
            && !session.paused
            && !suspended
            && session.playable
            && !session.game.is_over()
        {
            session.game.tick(elapsed);
            redraw.write(RequestRedraw);
        }
    }
}

pub(super) fn draw(
    mut sessions: Query<(&mut Session, &mut PanelCanvas), With<TuiPanel>>,
    theme: Res<Theme>,
) {
    for (mut session, mut canvas) in &mut sessions {
        let area = canvas.area();
        let buffer = canvas.bypass_change_detection().buffer_mut();
        let title = format!("{}  /  pocket arcade", session.kind.title());
        let block = theme.raised(title);
        let inner = block.inner(area);
        block.render(area, buffer);
        let (width, height) = session.kind.size();
        let body = Rect::new(
            inner.x,
            inner.y,
            inner.width,
            inner.height.saturating_sub(2),
        );
        session.playable = body.width >= width && body.height >= height;
        if session.playable {
            let game_area = Rect::new(body.x + (body.width - width) / 2, body.y, width, height);
            session.game.draw(game_area, buffer, &theme);
            if session.paused || session.suspended {
                let label = if session.paused {
                    " PAUSED - p to resume "
                } else {
                    " PAUSED "
                };
                Paragraph::new(label)
                    .alignment(rig_tui::ratatui::layout::Alignment::Center)
                    .style(theme.selection().add_modifier(Modifier::BOLD))
                    .render(
                        Rect::new(body.x, body.y + body.height / 2, body.width, 1),
                        buffer,
                    );
            }
        } else {
            let (width, height) = dimensions(session.kind);
            Paragraph::new(format!(
                "Resize to {width}x{height} to play.\nEsc hides and pauses."
            ))
            .wrap(Wrap { trim: true })
            .render(body, buffer);
        }
        if inner.height >= 2 {
            let controls = Rect::new(inner.x, inner.bottom() - 2, inner.width, 1);
            let common = Rect::new(inner.x, inner.bottom() - 1, inner.width, 1);
            Line::from(session.kind.help()).render(controls, buffer);
            Line::styled(
                "p pause  r new game  Esc hide",
                Style::default().add_modifier(Modifier::DIM),
            )
            .render(common, buffer);
        }
    }
}

pub(super) fn fit(mut sessions: Query<(&Session, &PanelCanvas, &mut TuiPanel)>) {
    for (session, canvas, mut panel) in &mut sessions {
        let (width, height) = dimensions(session.kind);
        // Other panels and a multiline composer can leave less than the
        // terminal-size estimate. Rig re-lays out Fit changes before Draw.
        if matches!(panel.placement, Placement::Right(_))
            && (canvas.area().width < width || canvas.area().height < height)
        {
            panel.placement = Placement::Over {
                width: Constraint::Length(width),
                height: Constraint::Length(height),
            };
        }
    }
}

pub(super) fn clock(
    sessions: Query<(Entity, &Session, Option<&KeepAwake>), With<TuiPanel>>,
    mut commands: Commands,
) {
    for (entity, session, awake) in &sessions {
        let running = session.kind.realtime()
            && session.playable
            && !session.paused
            && !session.suspended
            && !session.game.is_over();
        match (running, awake) {
            (true, None) => {
                commands.entity(entity).insert(KeepAwake(FRAME));
            }
            (false, Some(_)) => {
                commands.entity(entity).remove::<KeepAwake>();
            }
            _ => {}
        }
    }
}
