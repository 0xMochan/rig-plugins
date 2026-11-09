//! Mouse-mode ownership, including Rig's temporary terminal handoffs.

use bevy_app::{App, AppExit, Last, OnAppExitSystems, PostStartup};
use bevy_ecs::prelude::*;
use rig_harness::prelude::launcher::Refused;
use rig_tui::{RunInTerminal, TerminalExit, TerminalWrite, TuiTestScreen};

// Normal button/wheel reporting, with SGR coordinates. Unlike crossterm's
// EnableMouseCapture, this does not request every pointer movement.
pub(crate) const ENABLE: &[u8] = b"\x1b[?1000h\x1b[?1006h";
pub(crate) const DISABLE: &[u8] = b"\x1b[?1006l\x1b[?1000l";

pub(crate) fn install(app: &mut App, terminal_front: bool) {
    app.insert_resource(TerminalFront(terminal_front))
        .add_systems(PostStartup, open)
        .add_observer(suspend)
        // Rig writes TerminalWrite after this set, including the last frame.
        // Its RunInTerminal handoff/re-entry finishes in PostUpdate.
        .add_systems(Last, reconcile.in_set(OnAppExitSystems));
}

#[derive(Resource)]
struct TerminalFront(bool);

enum Destination {
    Test,
    #[cfg(unix)]
    Terminal(std::fs::File),
}

#[derive(Resource)]
struct MouseReporting {
    destination: Destination,
    enabled: bool,
    armed: bool,
    exiting: bool,
}

impl MouseReporting {
    fn new(destination: Destination) -> Self {
        Self {
            destination,
            enabled: false,
            armed: false,
            exiting: false,
        }
    }

    fn disable_now(&mut self) {
        #[cfg(unix)]
        if let Destination::Terminal(tty) = &mut self.destination {
            use std::io::Write;
            if let Err(error) = tty.write_all(DISABLE).and_then(|()| tty.flush()) {
                eprintln!("rig-tui-scroll: could not release mouse reporting: {error}");
            }
        }
    }
}

impl Drop for MouseReporting {
    fn drop(&mut self) {
        // The private TUI writer may already be gone or an unwind may have
        // skipped Last. This file owns the cleanup independently of stdout.
        if self.armed {
            self.disable_now();
        }
    }
}

fn open(
    front: Res<TerminalFront>,
    test: Option<Res<TuiTestScreen>>,
    refused: Option<Res<Refused>>,
    mut commands: Commands,
) {
    // A test screen always wins, even in a process with a controlling tty.
    if test.is_some() {
        commands.insert_resource(MouseReporting::new(Destination::Test));
        return;
    }
    if !front.0 || refused.is_some() {
        return;
    }
    #[cfg(unix)]
    {
        use std::io::IsTerminal;
        if !std::io::stdin().is_terminal() {
            return;
        }
        match std::fs::OpenOptions::new().write(true).open("/dev/tty") {
            Ok(tty) => {
                commands.insert_resource(MouseReporting::new(Destination::Terminal(tty)));
            }
            Err(error) => eprintln!("rig-tui-scroll: could not open the terminal: {error}"),
        }
    }
}

fn suspend(
    _: On<Add<RunInTerminal>>,
    reporting: Option<ResMut<MouseReporting>>,
    mut writes: MessageWriter<TerminalWrite>,
) {
    let Some(mut reporting) = reporting else {
        return;
    };
    if !reporting.armed {
        return;
    }
    // TerminalWrite is flushed only in Last, after an editor has already
    // run. This one immediate /dev/tty write releases its input before Rig
    // hands it the terminal; it never changes or wraps the child command.
    reporting.disable_now();
    reporting.enabled = false;
    // Supersede an enable already queued earlier in this same frame.
    writes.write(TerminalWrite(DISABLE.to_vec()));
}

fn reconcile(
    reporting: Option<ResMut<MouseReporting>>,
    runs: Query<(), (With<RunInTerminal>, Without<TerminalExit>)>,
    mut exits: MessageReader<AppExit>,
    mut writes: MessageWriter<TerminalWrite>,
) {
    let Some(mut reporting) = reporting else {
        return;
    };
    reporting.exiting |= exits.read().next().is_some();
    let enable = !reporting.exiting && runs.is_empty();
    if reporting.enabled == enable {
        return;
    }
    let bytes = if enable { ENABLE } else { DISABLE };
    writes.write(TerminalWrite(bytes.to_vec()));
    reporting.enabled = enable;
    reporting.armed |= enable;
}
