//! Two independent Rig clients over a disposable CLI protocol fixture.
//!
//! This exercises native consent, actual owned watch/mount processes and the
//! append/remote-context path. It is not real Delta/account validation. Python
//! is a test-only fixture interpreter; no network or credentials are used.

use std::fs;
use std::os::unix::fs::PermissionsExt;

use bevy_app::TaskPoolPlugin;
use bevy_ecs::message::Messages;

use super::*;
use crate::DeltaPlugin;

struct Server {
    _scratch: tempfile::TempDir,
    root: PathBuf,
    executable: PathBuf,
}

impl Server {
    fn new() -> Self {
        let scratch = tempfile::tempdir().unwrap();
        let root = scratch.path().canonicalize().unwrap();
        let python = std::process::Command::new("python3")
            .args(["-c", "import sys; print(sys.executable)"])
            .output()
            .expect("Python 3 is required only for the disposable Delta CLI integration fixture");
        assert!(
            python.status.success(),
            "Cannot locate the fixture's Python 3 interpreter"
        );
        let python = String::from_utf8(python.stdout).unwrap();
        let source = root.join("fixture.py");
        fs::write(&source, FIXTURE).unwrap();
        let executable = root.join("delta-fixture");
        fs::write(
            &executable,
            format!(
                "#!/bin/sh\nexec {} {} \"$@\"\n",
                shell_words::quote(python.trim()),
                shell_words::quote(source.to_str().unwrap()),
            ),
        )
        .unwrap();
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
        Self {
            _scratch: scratch,
            root,
            executable,
        }
    }

    fn records(&self) -> Vec<Value> {
        self.state()["records"].as_array().unwrap().clone()
    }

    fn state(&self) -> Value {
        serde_json::from_slice(&fs::read(self.root.join("state.json")).unwrap()).unwrap()
    }

    fn commands(&self) -> Vec<Value> {
        fs::read_to_string(self.root.join("commands.jsonl"))
            .unwrap_or_default()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }
}

struct ClientApp {
    _scratch: tempfile::TempDir,
    app: App,
    original: Entity,
    original_directory: PathBuf,
    notices: VecDeque<String>,
}

impl ClientApp {
    fn new(server: &Server, id: &str, initial_text: &str) -> Self {
        let scratch = tempfile::tempdir().unwrap();
        let project = scratch.path().join("project");
        let home = scratch.path().join("rig-home");
        fs::create_dir(&project).unwrap();
        fs::create_dir(project.join(".git")).unwrap();
        fs::create_dir(&home).unwrap();
        fs::write(project.join("shared.txt"), "initial fixture content\n").unwrap();
        let project = project.canonicalize().unwrap();
        let home = home.canonicalize().unwrap();
        let settings = Settings::from_toml(&format!(
            "[delta]\nexecutable = {}\n",
            serde_json::to_string(&server.executable).unwrap(),
        ));
        let mut app = App::new();
        app.insert_resource(RigHome(Home::at(&home)))
            .insert_resource(ProjectRoot(project.clone()))
            .insert_resource(settings)
            .add_plugins((TaskPoolPlugin::default(), AgentPlugin, DeltaPlugin));
        app.finish();
        app.cleanup();
        let mut conversation = Conversation::default();
        conversation.push(Message::user(initial_text));
        let original = app
            .world_mut()
            .spawn((
                Agent,
                AgentId(id.into()),
                WorkDir(project.clone()),
                conversation,
            ))
            .id();
        app.world_mut().spawn(Answerer { agent: None });
        Self {
            _scratch: scratch,
            app,
            original,
            original_directory: project,
            notices: VecDeque::new(),
        }
    }

    fn command(&mut self, line: &str) {
        self.app
            .world_mut()
            .trigger(RunCommand::new(self.original, line));
        self.app.world_mut().flush();
    }

    /// A test frontend deliberately supplies local Person choices to unchanged
    /// native questions. Production code still requires a real native frontend.
    fn update(&mut self) {
        self.app.update();
        let questions: Vec<_> = self
            .app
            .world_mut()
            .query_filtered::<(Entity, &Ask, &Decision), (Without<Outcome>, Without<Settled>)>()
            .iter(self.app.world())
            .filter(|(_, _, decision)| !decision.is_decided())
            .map(|(entity, ask, _)| (entity, ask.clone()))
            .collect();
        for (entity, question) in questions {
            let choice = [
                "allow",
                "private",
                "unchanged",
                "org-0",
                "public",
                "share",
                "join",
                "trust",
            ]
            .into_iter()
            .find(|id| question.choices.iter().any(|choice| choice.id == *id))
            .unwrap_or_else(|| panic!("Unexpected fixture consent question: {}", question.title));
            self.app
                .world_mut()
                .get_mut::<Decision>(entity)
                .unwrap()
                .decide(Decider::Person, Answer::choice(choice).outcome());
        }
        if let Some(mut notices) = self.app.world_mut().get_resource_mut::<Messages<Notice>>() {
            for notice in notices.drain() {
                if self.notices.len() == 100 {
                    self.notices.pop_front();
                }
                self.notices.push_back(notice.text);
            }
        }
    }

    fn connected(&self, agent: Entity) -> bool {
        self.app
            .world()
            .resource::<Runtime>()
            .connections
            .get(&agent)
            .is_some_and(|connection| connection.ready() && connection.journal.pending_count() == 0)
    }

    fn joined(&mut self) -> Option<Entity> {
        self.app
            .world_mut()
            .query::<(Entity, &Binding)>()
            .iter(self.app.world())
            .find(|(_, binding)| binding.joined)
            .map(|(entity, _)| entity)
    }

    fn text(&self, agent: Entity) -> String {
        self.app
            .world()
            .get::<Conversation>(agent)
            .unwrap()
            .entries()
            .filter_map(|entry| match entry.message {
                Message::User { content } => Some(
                    content
                        .iter()
                        .filter_map(|part| match part {
                            rig_harness::rig_core::message::UserContent::Text(text) => {
                                Some(text.text.as_str())
                            }
                            _ => None,
                        })
                        .collect::<Vec<_>>()
                        .join("\n"),
                ),
                Message::Assistant(message) => Some(message.text()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn clean(&self) -> bool {
        let runtime = self.app.world().resource::<Runtime>();
        runtime.connections.is_empty()
            && runtime.flows.is_empty()
            && runtime.draining.is_empty()
            && runtime.draining_processes.is_empty()
    }

    fn diagnostics(&self) -> String {
        let runtime = self.app.world().resource::<Runtime>();
        format!(
            "notices={:?}, connections={:?}, flows={}",
            self.notices,
            runtime
                .connections
                .iter()
                .map(|(agent, connection)| (
                    *agent,
                    connection.phase(),
                    &connection.failure,
                    connection.journal.pending_count()
                ))
                .collect::<Vec<_>>(),
            runtime.flows.len()
        )
    }
}

fn until(client: &mut ClientApp, condition: impl Fn(&mut ClientApp) -> bool, label: &str) {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        client.update();
        if condition(client) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "{label}: {}",
            client.diagnostics()
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn both_until(
    a: &mut ClientApp,
    b: &mut ClientApp,
    condition: impl Fn(&mut ClientApp, &mut ClientApp) -> bool,
    label: &str,
) {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        a.update();
        b.update();
        if condition(a, b) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "{label}: A {}; B {}",
            a.diagnostics(),
            b.diagnostics()
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn native_share_join_exchange_live_context_and_files_without_exporting_old_session() {
    let server = Server::new();
    let mut sharer = ClientApp::new(&server, "fixture-share", "Source public fixture question");
    sharer.command("share");
    sharer.app.update();
    assert!(
        server.commands().is_empty(),
        "an unanswered native consent request started a CLI process"
    );
    assert!(sharer.app.world().get::<Binding>(sharer.original).is_none());
    until(
        &mut sharer,
        |client| client.connected(client.original),
        "native share did not connect",
    );
    let shared_binding = sharer
        .app
        .world()
        .get::<Binding>(sharer.original)
        .unwrap()
        .clone();
    assert!(!shared_binding.joined);
    assert_eq!(shared_binding.visibility.as_deref(), Some("private"));
    let prior_commands = server.commands().len();

    let mut joiner = ClientApp::new(
        &server,
        "fixture-old-local",
        "OLD_LOCAL_PRIVATE_PREFIX_MUST_NOT_UPLOAD",
    );
    let old_directory = joiner.original_directory.clone();
    joiner.command("delta join https://delta.dev/join/share_fixture");
    both_until(
        &mut sharer,
        &mut joiner,
        |_, joiner| joiner.joined().is_some_and(|agent| joiner.connected(agent)),
        "native join did not connect a fresh agent",
    );
    let joined = joiner.joined().unwrap();
    assert_ne!(joined, joiner.original);
    let joined_id = joiner.app.world().get::<AgentId>(joined).unwrap().clone();
    assert_ne!(joined_id.0, "fixture-old-local");
    assert!(
        joiner.app.world().get::<SpawnedBy>(joined).is_none(),
        "join must not inherit old-root authorization"
    );
    assert_eq!(
        joiner
            .app
            .world()
            .get::<WorkDir>(joiner.original)
            .unwrap()
            .0,
        old_directory
    );
    assert!(
        joiner
            .text(joiner.original)
            .contains("OLD_LOCAL_PRIVATE_PREFIX_MUST_NOT_UPLOAD")
    );
    assert!(
        !joiner
            .text(joined)
            .contains("OLD_LOCAL_PRIVATE_PREFIX_MUST_NOT_UPLOAD")
    );
    let binding = joiner.app.world().get::<Binding>(joined).unwrap().clone();
    let scope = joiner.app.world().get::<LiveWorkspace>(joined).unwrap();
    assert_eq!(scope.owner(), &joined_id);
    assert_eq!(scope.primary(), binding.checkout.as_path());
    assert!(
        !scope.allows(&joined_id, &server.root),
        "join must not grant the common parent of its mounts"
    );
    assert!(
        joiner
            .app
            .world()
            .get::<Conversation>(joined)
            .unwrap()
            .entries()
            .all(|entry| !entry.is_users_own())
    );
    let commands = server.commands();
    let join_commands = &commands[prior_commands..];
    assert!(
        join_commands
            .iter()
            .any(|command| command["args"][0] == "thread" && command["args"][1] == "mount")
    );
    assert!(
        !join_commands.iter().any(|command| {
            matches!(
                (command["args"][0].as_str(), command["args"][1].as_str()),
                (Some("worktree"), Some("attach" | "create"))
                    | (Some("thread"), Some("share" | "create"))
            )
        }),
        "joining must not attach or republish the old checkout"
    );

    fs::write(
        sharer.original_directory.join("shared.txt"),
        "edit from sharer\n",
    )
    .unwrap();
    both_until(
        &mut sharer,
        &mut joiner,
        |_, _| {
            fs::read_to_string(binding.checkout.join("shared.txt"))
                .ok()
                .as_deref()
                == Some("edit from sharer\n")
        },
        "owned source mount did not deliver the source file change",
    );
    fs::write(
        binding.checkout.join("shared.txt"),
        "edit from joined session\n",
    )
    .unwrap();
    let source_file = sharer.original_directory.join("shared.txt");
    both_until(
        &mut sharer,
        &mut joiner,
        |_, _| {
            fs::read_to_string(&source_file).ok().as_deref() == Some("edit from joined session\n")
        },
        "owned joined mount did not deliver the joined file change",
    );

    sharer
        .app
        .world_mut()
        .get_mut::<Conversation>(sharer.original)
        .unwrap()
        .push(Message::assistant("LIVE_FROM_SHARER"));
    both_until(
        &mut sharer,
        &mut joiner,
        |_, joiner| joiner.text(joined).contains("LIVE_FROM_SHARER"),
        "append/watch did not deliver source activity",
    );
    joiner
        .app
        .world_mut()
        .get_mut::<Conversation>(joined)
        .unwrap()
        .push(Message::assistant("LIVE_FROM_JOINER"));
    both_until(
        &mut sharer,
        &mut joiner,
        |sharer, _| sharer.text(sharer.original).contains("LIVE_FROM_JOINER"),
        "append/watch did not deliver joined activity",
    );
    assert!(
        sharer
            .app
            .world()
            .get::<ActiveTurn>(sharer.original)
            .is_none()
    );
    assert!(
        joiner.app.world().get::<ActiveTurn>(joined).is_none(),
        "remote context must not start coding"
    );
    assert!(
        sharer
            .app
            .world()
            .get::<Conversation>(sharer.original)
            .unwrap()
            .entries()
            .filter(|entry| format!("{:?}", entry.message).contains("LIVE_FROM_JOINER"))
            .all(|entry| entry.author.is_some_and(
                |author| matches!(&author.kind, AuthorKind::Plugin(name) if name == "delta-remote")
            ))
    );
    let recorded = serde_json::to_string(&server.records()).unwrap();
    assert!(!recorded.contains("OLD_LOCAL_PRIVATE_PREFIX_MUST_NOT_UPLOAD"));
    assert!(
        !recorded.contains("Delta remote transcript item"),
        "remote context recursively entered this producer's outbox"
    );

    disconnect(sharer.app.world_mut(), sharer.original);
    disconnect(joiner.app.world_mut(), joined);
    both_until(
        &mut sharer,
        &mut joiner,
        |a, b| a.clean() && b.clean(),
        "owned CLI cleanup did not finish",
    );
    let released = fs::read_to_string(server.root.join("released.jsonl")).unwrap();
    assert!(released.contains("worktree_mount"));
    assert!(released.contains("thread_mount"));
    assert!(
        !binding.checkout.exists(),
        "joined fixture checkouts were not released"
    );
    assert!(
        sharer.original_directory.exists(),
        "stopping an existing-checkout mount deleted the source checkout"
    );
}

const FIXTURE: &str = r#"import contextlib
import fcntl
import json
import os
from pathlib import Path
import shutil
import signal
import sys
import time

ROOT = Path(__file__).resolve().parent
STATE = ROOT / 'state.json'
THREAD = 'thread_fixture'
REFERENCE = 'history#t=fixture'
URL = 'https://delta.dev/join/share_fixture'
STOP = False


def stopped(signum, frame):
    global STOP
    STOP = True


signal.signal(signal.SIGTERM, stopped)
signal.signal(signal.SIGINT, stopped)


@contextlib.contextmanager
def locked():
    with (ROOT / 'state.lock').open('a+') as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        if STATE.exists():
            value = json.loads(STATE.read_text())
        else:
            value = {'source': None, 'attached': False, 'visibility': 'private',
                     'producers': {}, 'records': [], 'version': 0, 'file': None}
        yield value
        temporary = ROOT / 'state.next'
        temporary.write_text(json.dumps(value))
        temporary.replace(STATE)


def emit(value):
    print(json.dumps(value), flush=True)


def producer(state, name):
    return state['producers'].setdefault(name, {'next': 1, 'turn': None})


def option(args, name, default=None):
    return args[args.index(name) + 1] if name in args else default


args = sys.argv[1:]
if args and args[0] == '--json':
    args.pop(0)
with locked() as state:
    with (ROOT / 'commands.jsonl').open('a') as log:
        log.write(json.dumps({'args': args, 'cwd': os.getcwd(), 'pid': os.getpid()}) + '\n')


def sync_file(path, last, seen):
    local = path.read_text() if path.exists() else None
    with locked() as state:
        if last is None and state['file'] is None and local is not None:
            state['file'] = local
            state['version'] += 1
        elif last is not None and local is not None and local != last:
            state['file'] = local
            state['version'] += 1
        if state['file'] is not None and (state['version'] != seen or local is None):
            path.write_text(state['file'])
            local = state['file']
        seen = state['version']
    return local, seen


def mount(path, ready, kind, cleanup=None):
    path.mkdir(parents=True, exist_ok=True)
    last, seen = sync_file(path / 'shared.txt', None, -1)
    emit(ready)
    try:
        while not STOP:
            last, seen = sync_file(path / 'shared.txt', last, seen)
            time.sleep(0.02)
    finally:
        with locked() as state:
            with (ROOT / 'released.jsonl').open('a') as log:
                log.write(json.dumps({'kind': kind, 'pid': os.getpid()}) + '\n')
        if cleanup is not None:
            shutil.rmtree(cleanup, ignore_errors=True)


def visible_items(state, own):
    order = []
    items = {}
    for row in state['records']:
        if row['producer'] == own:
            continue
        update = row['record']['update']
        kind = update.get('sessionUpdate')
        if kind not in ('user_message', 'agent_message', 'agent_thought'):
            continue
        identity = row['producer'] + ':' + update['messageId']
        text = ''.join(block.get('text', '') for block in update.get('content', []) if block.get('type') == 'text')
        if identity not in items:
            order.append(identity)
        items[identity] = {'id': identity, 'kind': kind, 'after_id': None,
                           'turn_id': None, 'author': {'user_id': None, 'name': row['producer'], 'actor_id': row['producer']},
                           'created_at': None, 'open': False, 'content': {'text': text}}
    previous = None
    for identity in order:
        items[identity]['after_id'] = previous
        previous = identity
    return [items[identity] for identity in order]


if args[:2] == ['thread', 'contract']:
    emit({'operation': 'contract', 'min': 8, 'max': 9})
elif args[:2] == ['auth', 'status']:
    emit({'status': 'signed_in', 'account': {'login': 'fixture'}})
elif args[:2] == ['org', 'list']:
    emit({'organizations': [{'id': 'org_fixture', 'name': 'Fixture team', 'current': True}]})
elif args[:2] == ['thread', 'create']:
    name = option(args, '--external-id')
    with locked() as state:
        created = name not in state['producers']
        producer(state, name)
    emit({'operation': 'created', 'thread_id': THREAD, 'created': created})
elif args[:2] == ['thread', 'status']:
    name = option(args, '--external-id')
    with locked() as state:
        who = dict(producer(state, name))
        source = state['source']
        visibility = state['visibility']
        attached = state['attached']
    emit({'operation': 'status', 'thread_id': THREAD, 'url': URL, 'visibility': visibility,
          'upload': {'state': 'uploaded', 'organization_id': 'org_fixture', 'error': None},
          'access': {'state': 'granted', 'is_owner': True},
          'worktrees': {'status': 'available', 'items': [{'reference': REFERENCE, 'checkouts': [{'directory': source, 'state': 'attached'}]}] if attached else []},
          'producer': {'source': 'rig', 'external_id': name, 'next_sequence': who['next'], 'turn': who['turn']}})
elif args[:2] == ['worktree', 'list']:
    with locked() as state:
        source = state['source']
    emit({'operation': 'listed', 'worktrees': [{'reference': REFERENCE, 'directories': [{'directory': source, 'state': 'attached', 'owners': [THREAD]}]}] if source else []})
elif args[:2] == ['worktree', 'create']:
    with locked() as state:
        state['source'] = str(Path(args[2]).resolve())
    emit({'operation': 'created', 'reference': REFERENCE})
elif args[:2] == ['worktree', 'attach']:
    with locked() as state:
        state['attached'] = True
    emit({'operation': 'attached', 'reference': REFERENCE, 'thread_id': THREAD})
elif args[:2] == ['worktree', 'mount']:
    path = Path(args[3]).resolve()
    mount(path, {'operation': 'mounted', 'reference': REFERENCE, 'directory': str(path)}, 'worktree_mount')
elif args[:2] == ['thread', 'mount']:
    owner = option(args, '--owner')
    parent = ROOT / 'joined' / owner
    path = parent / 'checkout'
    path.mkdir(parents=True, exist_ok=True)
    (path / '.git').mkdir(exist_ok=True)
    mount(path, {'operation': 'mounted', 'thread_id': THREAD, 'owner': owner, 'directory': str(path),
                 'worktrees': [{'reference': REFERENCE, 'name': 'fixture', 'directory': str(path), 'state': 'mounted'}]},
          'thread_mount', parent)
elif args[:2] == ['thread', 'share']:
    with locked() as state:
        state['visibility'] = 'organization'
    emit({'operation': 'shared', 'thread_id': THREAD, 'url': URL})
elif args[:2] == ['thread', 'invite']:
    emit({'operation': 'invited', 'thread_id': THREAD, 'url': URL,
          'results': [{'email': email, 'invited': True} for email in args[3:]]})
elif args[:2] == ['thread', 'append']:
    name = option(args, '--external-id')
    for line in sys.stdin:
        record = json.loads(line)
        with locked() as state:
            who = producer(state, name)
            sequence = record['sequence']
            if sequence < who['next']:
                duplicate = True
            elif sequence == who['next']:
                duplicate = False
                state['records'].append({'producer': name, 'record': record})
                who['next'] += 1
                update = record['update']
                if update.get('sessionUpdate') == 'state_update':
                    turn_sequence = record.get('turnSequence')
                    who['turn'] = {'sequence': turn_sequence, 'status': 'running' if update.get('state') == 'running' else 'finished'}
            else:
                emit({'error': {'code': 'sequence_gap', 'message': 'fixture sequence gap'}})
                sys.exit(1)
            frontier = who['next']
        emit({'operation': 'appended', 'thread_id': THREAD, 'sequence': sequence,
              'next_sequence': frontier, 'duplicate': duplicate,
              'disposition': 'duplicate' if duplicate else 'recorded',
              'turn_sequence': record.get('turnSequence'), 'version': 'fixture.1'})
elif args[:2] == ['thread', 'watch']:
    name = option(args, '--external-id')
    with locked() as state:
        who = dict(producer(state, name))
        initial = visible_items(state, name)
    emit({'type': 'ready', 'thread_id': THREAD,
          'producer': {'source': 'rig', 'external_id': name, 'next_sequence': who['next'], 'turn': who['turn']}})
    sent = {}
    for item in initial:
        emit({'type': 'item', 'item': item})
        sent[item['id']] = item
    emit({'type': 'worktrees', 'worktrees': [{'reference': REFERENCE, 'name': 'fixture'}]})
    emit({'type': 'presence', 'users': [{'user_id': None, 'name': 'Fixture peer', 'actor_id': 'fixture-peer'}]})
    emit({'type': 'live'})
    previous_turn = who['turn']
    while not STOP:
        with locked() as state:
            items = visible_items(state, name)
            turn = dict(producer(state, name)).get('turn')
        for item in items:
            if sent.get(item['id']) != item:
                emit({'type': 'item', 'item': item})
                sent[item['id']] = item
        if turn != previous_turn and turn is not None:
            emit({'type': 'producer.turn', **turn})
            previous_turn = turn
        time.sleep(0.02)
else:
    emit({'error': {'code': 'unsupported_fixture_command', 'message': repr(args)}})
    sys.exit(2)
"#;
