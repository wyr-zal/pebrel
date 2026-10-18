//! Per-PTY WSL Hook authentication and lifetime.

use std::io::{self, Seek as _, Write as _};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use nebula_terminal::tty;
use serde::{Deserialize, Serialize};

use crate::ai_agents::AgentKind;
use crate::ai_hook::lifecycle::CodexTerminalEvidence;

#[cfg(test)]
mod live_tests;
mod pipe;

pub(crate) const PIPE_ENV: &str = "PEBREL_WSL_HOOK_PIPE";
pub(crate) const TOKEN_ENV: &str = "PEBREL_WSL_HOOK_TOKEN";
const MAX_WIRE: usize = 64 * 1024 + 128;

#[derive(Clone, Deserialize, Serialize, PartialEq, Eq)]
struct Anchor {
    pid: u32,
    start: String,
    uid: u32,
    tty: String,
    session: String,
}

#[derive(Deserialize)]
struct Evidence {
    anchor: Anchor,
    argv: Vec<[String; 3]>,
    #[serde(default)]
    terminal: Option<CodexTerminalEvidence>,
}

#[derive(Clone)]
struct Scope {
    token: String,
    pane: String,
    instance: String,
    distro: String,
    user: Option<String>,
    anchor: Option<Anchor>,
}

impl Scope {
    fn accept(
        &mut self,
        wire: &[u8],
        probe: impl FnOnce(&str, Option<&Anchor>) -> Option<Evidence>,
    ) -> Option<Vec<u8>> {
        if wire.len() > MAX_WIRE {
            return None;
        }
        let end = wire.iter().position(|byte| *byte == b'\n')?;
        let expected = format!("pebrel-wsl-hook/1 token={}", self.token);
        if wire[..end] != *expected.as_bytes() {
            return None;
        }
        let envelope = &wire[end + 1..];
        let header_end = envelope.iter().position(|byte| *byte == b'\n')?;
        if header_end > 512 {
            return None;
        }
        let header = std::str::from_utf8(&envelope[..header_end]).ok()?;
        let mut fields = header.split_whitespace();
        if fields.next()? != "nebula-hook/1" {
            return None;
        }
        let source = fields.next()?.strip_prefix("source=")?;
        if !matches!(source, "claude" | "codex") {
            return None;
        }
        let owner = fields.next()?.strip_prefix("process=")?;
        let (pid, epoch) = owner.split_once(':')?;
        if pid.parse::<u32>().ok()? == 0
            || epoch.is_empty()
            || epoch.len() > 32
            || !epoch.bytes().all(|byte| byte.is_ascii_digit())
        {
            return None;
        }
        if let Some(mode) = fields.next() {
            if source != "codex" || !matches!(mode, "codex_hooks=full" | "codex_hooks=turns") {
                return None;
            }
        }
        if fields.next().is_some() {
            return None;
        }
        let evidence = probe(owner, self.anchor.as_ref())?;
        self.accept_owner(source, &evidence)?;
        if !serde_json::from_slice::<serde_json::Value>(&envelope[header_end + 1..])
            .ok()?
            .is_object()
        {
            return None;
        }
        self.anchor = Some(evidence.anchor);
        Some(envelope.to_vec())
    }

    fn accept_owner(&self, source: &str, evidence: &Evidence) -> Option<()> {
        let parse =
            |args: &[String; 3]| AgentKind::parse_process_argv(&args[0], &args[1], Some(&args[2]));
        // The npm CLI keeps a Node launcher directly above its native child.
        // Skip only that adjacent launcher; any outer Agent remains nested.
        let launcher = |index: usize, args: &[String; 3]| {
            source == "codex"
                && index == 0
                && matches!(args[1].rsplit(['/', '\\']).next(), Some("node" | "nodejs"))
                && matches!(args[2].rsplit(['/', '\\']).next(), Some("codex" | "codex.js"))
        };
        if evidence.argv.is_empty()
            || evidence.argv.len() > 48
            || parse(&evidence.argv[0]) != AgentKind::parse(source)
            || evidence.argv[1..]
                .iter()
                .enumerate()
                .any(|(index, args)| parse(args).is_some() && !launcher(index, args))
            || self.anchor.as_ref().is_some_and(|anchor| anchor != &evidence.anchor)
        {
            return None;
        }
        Some(())
    }

    fn probe(
        &self,
        owner: &str,
        anchor: Option<&Anchor>,
        stop: &pipe::Signal,
        deadline: Instant,
    ) -> Option<Evidence> {
        let request = serde_json::json!({"pane":self.pane,"instance":self.instance,"token":self.token,
            "user":self.user,"owner":owner,"anchor":anchor});
        self.probe_request(request, stop, deadline)
    }

    fn probe_request(
        &self,
        request: serde_json::Value,
        stop: &pipe::Signal,
        deadline: Instant,
    ) -> Option<Evidence> {
        let quoted = serde_json::to_string(&request.to_string()).ok()?;
        let mut input = tempfile::tempfile().ok()?;
        write!(
            input,
            "{}\nprint(json.dumps(verify(json.loads({quoted}))))\n",
            include_str!("../../../res/hooks/wsl_owner.py")
        )
        .ok()?;
        input.rewind().ok()?;
        let mut command = self.owner_command(&crate::platform::shell::wsl_executable()?);
        command.stdin(Stdio::from(input));
        let output = crate::platform::process_output::read_cancellable_with_stdin(
            command,
            deadline.saturating_duration_since(Instant::now()),
            32 * 1024,
            &|| stop.is_set(),
        )
        .ok()?;
        serde_json::from_slice(&output).ok()
    }

    fn owner_command(&self, wsl: &str) -> Command {
        // Root only reads kernel evidence in the frozen distro. Ignore user
        // Python paths/startup files; never resolve its interpreter via PATH.
        let mut command = Command::new(wsl);
        command.args([
            "--distribution",
            &self.distro,
            "--user",
            "root",
            "--exec",
            "/usr/bin/python3",
            "-I",
            "-S",
            "-",
        ]);
        crate::platform::process::hidden_command(&mut command);
        command
    }
}

/// Owned by the PTY session, including while its view moves between windows.
pub(crate) struct Channel {
    stop: pipe::Signal,
    generation: String,
    authenticated: Arc<Mutex<Option<Scope>>>,
}

pub(crate) struct FailureProbe {
    scope: Scope,
    stop: pipe::Signal,
    pub(crate) generation: String,
}

impl FailureProbe {
    pub(crate) fn run(
        &self,
        target: &crate::ai_hook::lifecycle::CodexFailureTarget,
    ) -> Option<CodexTerminalEvidence> {
        if self.stop.is_set() {
            return None;
        }
        let request = serde_json::json!({"pane":self.scope.pane,"instance":self.scope.instance,
            "token":self.scope.token,"user":self.scope.user,"owner":target.process,
            "anchor":self.scope.anchor,"terminal":{"session":target.session,"turn":target.turn}});
        let evidence = self.scope.probe_request(
            request,
            &self.stop,
            Instant::now() + Duration::from_secs(2),
        )?;
        self.scope.accept_owner("codex", &evidence)?;
        if self.stop.is_set() {
            return None;
        }
        evidence.terminal
    }
}

pub(super) fn prepare(options: &mut tty::Options, enabled: bool) -> io::Result<()> {
    // Even a failed setup marks this as an OOB pane: the bridge must never
    // silently fall back to OSC when its dedicated channel is unavailable.
    options.env.insert(PIPE_ENV.into(), String::new());
    options.env.insert(TOKEN_ENV.into(), String::new());
    let wslenv = options.env.entry("WSLENV".into()).or_default();
    let mut entries: Vec<_> = wslenv
        .split(':')
        .filter(|entry| {
            !entry.is_empty() && !matches!(entry.split('/').next(), Some(PIPE_ENV | TOKEN_ENV))
        })
        .map(str::to_owned)
        .collect();
    entries.extend([format!("{PIPE_ENV}/u"), format!("{TOKEN_ENV}/u")]);
    *wslenv = entries.join(":");
    if !enabled {
        return Ok(());
    }
    let fresh = || crate::ai_hook::remote::new_token().map_err(io::Error::other);
    let token = fresh()?;
    let nonce = fresh()?;
    let pane = options.env.get(crate::ai_hook::PANE_ENV).ok_or(io::ErrorKind::InvalidInput)?;
    options.env.insert(
        PIPE_ENV.into(),
        format!(r"\\.\pipe\pebrel-wsl-hook-{}-{pane}-{nonce}", std::process::id()),
    );
    options.env.insert(TOKEN_ENV.into(), token);
    Ok(())
}

impl Channel {
    pub(crate) fn start(
        options: &tty::Options,
        sink: impl Fn(String, Vec<u8>) -> bool + Send + 'static,
    ) -> io::Result<Option<Self>> {
        let Some(name) = options.env.get(PIPE_ENV).filter(|name| !name.is_empty()) else {
            return Ok(None);
        };
        let token = options
            .env
            .get(TOKEN_ENV)
            .filter(|value| value.len() == 32)
            .ok_or(io::ErrorKind::InvalidInput)?;
        let shell = options.shell.as_ref().ok_or(io::ErrorKind::InvalidInput)?;
        let distro = crate::shell_detect::wsl_launch_distro(shell.program(), shell.args())
            .ok_or(io::ErrorKind::InvalidInput)?;
        let scope = Scope {
            token: token.clone(),
            pane: options
                .env
                .get(crate::ai_hook::PANE_ENV)
                .cloned()
                .ok_or(io::ErrorKind::InvalidInput)?,
            instance: options
                .env
                .get(crate::agent_env::PROCESS_ENV)
                .cloned()
                .ok_or(io::ErrorKind::InvalidInput)?,
            distro: distro.into(),
            user: crate::shell_detect::wsl_launch_user(shell.program(), shell.args())
                .map(str::to_owned),
            anchor: None,
        };
        let endpoint = pipe::Endpoint::bind(name)?;
        let stop = pipe::Signal::new()?;
        let worker_stop = stop.clone();
        let generation = name.clone();
        let worker_generation = generation.clone();
        let authenticated = Arc::new(Mutex::new(None));
        let worker_authenticated = authenticated.clone();
        std::thread::Builder::new().name("pebrel-wsl-hook".into()).spawn(move || {
            serve(endpoint, scope, worker_stop, &worker_generation, worker_authenticated, sink);
        })?;
        Ok(Some(Self { stop, generation, authenticated }))
    }

    pub(crate) fn failure_probe(&self) -> Option<FailureProbe> {
        let scope = self.authenticated.lock().ok()?.clone()?;
        self.accepts(&self.generation).then(|| FailureProbe {
            scope,
            stop: self.stop.clone(),
            generation: self.generation.clone(),
        })
    }

    pub(crate) fn cancel(&self) {
        self.stop.set();
    }

    pub(crate) fn accepts(&self, generation: &str) -> bool {
        !self.stop.is_set() && generation == self.generation
    }

    pub(crate) fn diagnostics(&self) -> (bool, bool) {
        (!self.stop.is_set(), self.authenticated.lock().is_ok_and(|scope| scope.is_some()))
    }
}

impl Drop for Channel {
    fn drop(&mut self) {
        self.cancel();
    }
}

fn serve(
    endpoint: pipe::Endpoint,
    mut scope: Scope,
    stop: pipe::Signal,
    generation: &str,
    authenticated: Arc<Mutex<Option<Scope>>>,
    sink: impl Fn(String, Vec<u8>) -> bool,
) {
    while !stop.is_set() {
        if let Err(error) = endpoint.connect(&stop) {
            endpoint.disconnect();
            if !stop.is_set() && pipe::peer_disconnected(&error) {
                continue;
            }
            break;
        }
        let deadline = Instant::now() + Duration::from_millis(1700);
        let received = endpoint.receive(&stop, deadline, MAX_WIRE).ok().and_then(|wire| {
            let evidence_scope = scope.clone();
            scope
                .accept(&wire, |owner, anchor| evidence_scope.probe(owner, anchor, &stop, deadline))
        });
        let accepted = if let Some(envelope) = received.filter(|_| !stop.is_set()) {
            if let Ok(mut current) = authenticated.lock() {
                *current = Some(scope.clone());
            }
            sink(generation.to_owned(), envelope)
        } else {
            false
        };
        let _ = endpoint.acknowledge(accepted, &stop, deadline);
        endpoint.disconnect();
    }
    stop.set();
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn scope() -> Scope {
        Scope {
            token: "a".repeat(32),
            pane: "17".into(),
            instance: "900".into(),
            distro: "fixture".into(),
            user: None,
            anchor: None,
        }
    }

    fn wire(token: &str, owner: &str, source: &str) -> Vec<u8> {
        format!("pebrel-wsl-hook/1 token={token}\nnebula-hook/1 source={source} process={owner}\n{{\"hook_event_name\":\"Stop\",\"session_id\":\"s\",\"bridge_sequence\":1}}").into_bytes()
    }

    fn evidence() -> Evidence {
        serde_json::from_value(json!({"anchor":{"pid":10,"start":"100","uid":1000,"tty":"42","session":"10"},"argv":[["claude","claude",""]]})).unwrap()
    }

    #[test]
    fn wsl_hook_codex_terminal_response_preserves_explicit_kinds_and_missing_evidence() {
        use crate::ai_hook::lifecycle::CodexTerminalEvidence;
        let base = json!({"anchor":{"pid":10,"start":"100","uid":1000,"tty":"42","session":"10"},"argv":[["codex","codex",""]]});
        for (terminal, expected) in [
            (json!({"kind":"finished"}), CodexTerminalEvidence::Finished {}),
            (json!({"kind":"cancelled"}), CodexTerminalEvidence::Cancelled {}),
            (
                json!({"kind":"failed","error":"failure"}),
                CodexTerminalEvidence::Failed { error: "failure".into() },
            ),
        ] {
            let mut value = base.clone();
            value["terminal"] = terminal;
            let parsed: Evidence = serde_json::from_value(value).unwrap();
            assert_eq!(parsed.terminal, Some(expected));
        }
        for terminal in [None, Some(serde_json::Value::Null)] {
            let mut value = base.clone();
            if let Some(terminal) = terminal {
                value["terminal"] = terminal;
            }
            let parsed: Evidence = serde_json::from_value(value).unwrap();
            assert_eq!(parsed.terminal, None);
        }
    }

    #[test]
    fn wsl_hook_codex_terminal_response_rejects_unknown_or_malformed_outcomes() {
        use crate::ai_hook::lifecycle::CodexTerminalEvidence;
        for value in [
            json!({}),
            json!({"kind":"done"}),
            json!({"kind":"failed"}),
            json!({"kind":"failed","error":null}),
            json!({"kind":"finished","error":"not an idle fact"}),
            json!({"kind":"cancelled","error":"not an idle fact"}),
        ] {
            assert!(serde_json::from_value::<CodexTerminalEvidence>(value).is_err());
        }
    }

    #[test]
    fn wsl_hook_owner_probe_uses_fixed_distro_and_isolated_system_python() {
        let command = scope().owner_command("wsl.exe");
        let args: Vec<_> = command.get_args().map(|arg| arg.to_str().unwrap()).collect();
        assert_eq!(
            args,
            [
                "--distribution",
                "fixture",
                "--user",
                "root",
                "--exec",
                "/usr/bin/python3",
                "-I",
                "-S",
                "-"
            ]
        );
    }

    #[test]
    fn wsl_hook_abandoned_connection_does_not_retire_the_pane_channel() {
        use std::io::{Read as _, Write as _};
        let name = format!(
            r"\\.\pipe\pebrel-wsl-hook-abandoned-{}",
            crate::ai_hook::remote::new_token().unwrap()
        );
        let endpoint = pipe::Endpoint::bind(&name).unwrap();
        drop(std::fs::OpenOptions::new().read(true).write(true).open(&name).unwrap());
        let stop = pipe::Signal::new().unwrap();
        let worker_stop = stop.clone();
        let worker = std::thread::spawn(move || {
            serve(
                endpoint,
                scope(),
                worker_stop,
                "generation",
                Arc::new(Mutex::new(None)),
                |_, _| panic!("invalid input must not reach UI"),
            )
        });
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut client = loop {
            match std::fs::OpenOptions::new().read(true).write(true).open(&name) {
                Ok(client) => break client,
                Err(_) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(5)),
                Err(error) => {
                    stop.set();
                    worker.join().unwrap();
                    panic!("channel retired after an abandoned client: {error}");
                },
            }
        };
        client.write_all(&((MAX_WIRE + 1) as u32).to_le_bytes()).unwrap();
        let mut acknowledgement = [1];
        let received = client.read_exact(&mut acknowledgement);
        let _ = client.write_all(&[2]);
        stop.set();
        worker.join().unwrap();
        assert!(received.is_ok());
        assert_eq!(acknowledgement, [0]);
    }

    #[test]
    fn wsl_hook_codex_node_launcher_is_not_a_nested_provider() {
        let mut scope = scope();
        let mut launched = evidence();
        launched.argv = vec![
            ["codex".into(), "/opt/codex/vendor/bin/codex".into(), "".into()],
            ["MainThread".into(), "node".into(), "/opt/node/bin/codex".into()],
            ["bash".into(), "/bin/bash".into(), "".into()],
        ];
        let input = wire(&"a".repeat(32), "20:200", "codex");
        assert!(
            scope
                .accept(&input, |_, _| Some(Evidence {
                    anchor: launched.anchor.clone(),
                    argv: launched.argv.clone(),
                    terminal: None
                }))
                .is_some()
        );
        launched.argv.insert(2, ["codex".into(), "codex".into(), "".into()]);
        assert!(
            scope
                .accept(&input, |_, _| Some(Evidence {
                    anchor: launched.anchor.clone(),
                    argv: launched.argv.clone(),
                    terminal: None
                }))
                .is_none(),
            "a real outer Codex remains nested"
        );
        launched.argv.remove(2);
        launched.argv[1] = ["claude".into(), "claude".into(), "".into()];
        assert!(scope.accept(&input, |_, _| Some(launched)).is_none());
    }

    #[test]
    fn wsl_hook_authentication_precedes_parsing_and_rejects_foreign_tokens() {
        let mut scope = scope();
        assert!(
            scope
                .accept(&wire(&"b".repeat(32), "20:200", "claude"), |_, _| panic!(
                    "unauthenticated probe"
                ))
                .is_none()
        );
        let bytes = scope
            .accept(&wire(&"a".repeat(32), "20:200", "claude"), |_, _| Some(evidence()))
            .unwrap();
        let event = crate::ai_hook::parse_remote_envelope(&bytes, Some(17)).unwrap();
        assert_eq!(event.pane, Some(17));
        assert_eq!(event.remote_process.as_deref(), Some("20:200"));
        assert!(event.client_pid.is_none() && event.agent_pid.is_none());
    }

    #[test]
    fn wsl_hook_rejects_unknown_owner_nested_agents_and_replaced_pty() {
        let mut scope = scope();
        assert!(
            scope
                .accept(&wire(&"a".repeat(32), "bad", "claude"), |_, _| panic!("invalid owner"))
                .is_none()
        );
        assert!(scope.accept(&wire(&"a".repeat(32), "20:200", "claude"), |_, _| None).is_none());
        assert!(
            scope
                .accept(&wire(&"a".repeat(32), "20:200", "codex"), |_, _| Some(evidence()))
                .is_none()
        );
        let mut nested = evidence();
        nested.argv.push(["codex".into(), "codex".into(), "".into()]);
        assert!(
            scope.accept(&wire(&"a".repeat(32), "20:200", "claude"), |_, _| Some(nested)).is_none()
        );
        assert!(
            scope
                .accept(&wire(&"a".repeat(32), "20:200", "claude"), |_, _| Some(evidence()))
                .is_some()
        );
        let mut replaced = evidence();
        replaced.anchor.start = "101".into();
        assert!(
            scope
                .accept(&wire(&"a".repeat(32), "20:200", "claude"), |_, _| Some(replaced))
                .is_none()
        );
    }
}
