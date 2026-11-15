//! Structured recovery of an Agent entered through a WSL child of a Shell pane.

use serde::{Deserialize, Serialize};

use crate::ai_agents::AgentKind;
use crate::display::side_panel::{PathQuote, drop_text_for_paths};

mod command;
#[cfg(all(test, windows))]
mod native_tests;
#[cfg(test)]
mod tests;

/// Only validated, literal launch arguments are replayable. An unsupported
/// wrapper still records the guest location, but cannot fall back to the host.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WslAgentLaunch {
    pub launcher: String,
    pub distribution: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
    pub cwd: String,
    #[serde(default)]
    pub options: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    command: Option<AgentCommand>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct AgentCommand {
    program: String,
    args: Vec<String>,
    wrapper: CommandWrapper,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum CommandWrapper {
    Direct,
    LoginShell,
}

impl WslAgentLaunch {
    pub(crate) fn literal_invocation(value: &str) -> Option<Vec<String>> {
        let owner_command = value.split_once('\n').filter(|(owner, _)| owner.starts_with("pwsh:"));
        let value = owner_command.map_or(value, |(_, command)| command).trim_start();
        let powershell = owner_command.is_some() || value.starts_with("& ");
        let value = value.strip_prefix("& ").map_or(value, str::trim_start);
        let args = if powershell {
            command::powershell_literal_words(value)?
        } else {
            command::literal_words(value)?
        };
        crate::shell_detect::is_wsl_launcher(args.first()?).then_some(args)
    }

    /// A literal launch request is only intent, never proof of a live Agent.
    pub(crate) fn requested_agent(argv: &[String]) -> Option<AgentKind> {
        if !crate::shell_detect::is_wsl_launcher(argv.first()?) {
            return None;
        }
        let args = &argv[1..];
        let at = args.iter().position(|arg| matches!(arg.as_str(), "-e" | "--exec" | "--"))?;
        if !valid_options(&args[..at]) {
            return None;
        }
        [AgentKind::Claude, AgentKind::Codex, AgentKind::Hermes]
            .into_iter()
            .find(|kind| command::parse(&args[at + 1..], *kind).is_some())
    }

    pub(crate) fn is_replayable(&self) -> bool {
        self.command.is_some()
    }

    pub(crate) fn unavailable(distribution: &str, user: Option<&str>, cwd: &str) -> Self {
        let mut options = vec!["-d".into(), distribution.into()];
        if let Some(user) = user {
            options.extend(["-u".into(), user.into()]);
        }
        Self {
            launcher: "wsl.exe".into(),
            distribution: distribution.into(),
            user: user.map(str::to_owned),
            cwd: cwd.into(),
            options,
            command: None,
        }
    }

    /// Called only after both the Windows launcher and its guest foreground
    /// Agent have been verified for the same Shell submission.
    pub(crate) fn capture(
        launcher: &str,
        args: &[String],
        distribution: &str,
        user: Option<&str>,
        cwd: &str,
        kind: AgentKind,
    ) -> Self {
        let mut launch = Self::unavailable(distribution, user, cwd);
        if !crate::shell_detect::is_wsl_launcher(launcher) {
            return launch;
        }
        let Some(at) = args.iter().position(|arg| matches!(arg.as_str(), "-e" | "--exec" | "--"))
        else {
            return launch;
        };
        let options = &args[..at];
        if !valid_options(options) {
            return launch;
        }
        let Some(scope) = crate::shell_detect::wsl_explicit_scope(launcher, options) else {
            return launch;
        };
        if scope.0 != distribution || scope.1.as_deref() != user {
            return launch;
        }
        launch.launcher = launcher.into();
        launch.options = options.to_vec();
        launch.command = command::parse(&args[at + 1..], kind);
        launch
    }

    pub(crate) fn requested_session_id(&self, kind: AgentKind) -> Option<String> {
        command::requested_session_id(self.command.as_ref()?, kind)
    }

    pub(crate) fn resume_command(
        &self,
        kind: AgentKind,
        session_id: &str,
        quote: PathQuote,
    ) -> Option<String> {
        kind.resume_command(session_id)?;
        let command = self.command.as_ref()?;
        let args = command::resumed_args(command, kind, session_id)?;
        self.command_with_args(command, args, quote)
    }

    pub(crate) fn codex_selector_command(&self, quote: PathQuote) -> Option<String> {
        let command = self.command.as_ref()?;
        let args = command::selector_args(command)?;
        self.command_with_args(command, args, quote)
    }

    fn command_with_args(
        &self,
        command: &AgentCommand,
        args: Vec<String>,
        quote: PathQuote,
    ) -> Option<String> {
        if !valid_value(&self.launcher)
            || !crate::shell_detect::is_wsl_launcher(&self.launcher)
            || !valid_options(&self.options)
            || !valid_value(&self.distribution)
            || self.distribution.len() > 256
            || self.user.as_deref().is_some_and(|user| !valid_value(user) || user.len() > 256)
            || !self.cwd.starts_with('/')
            || !valid_value(&self.cwd)
            || !crate::shell_detect::wsl_accepts_startup_arg(&self.cwd)
        {
            return None;
        }
        let scope = crate::shell_detect::wsl_explicit_scope(&self.launcher, &self.options)?;
        if scope.0 != self.distribution || scope.1 != self.user {
            return None;
        }
        let mut guest = vec![command.program.clone()];
        guest.extend(args);
        let mut options =
            crate::shell_detect::wsl_args_in_host_directory(&self.launcher, &self.options)?;
        // These are literal arguments in a command typed into the outer Shell,
        // not the prequoted startup values used by Command::args at PTY spawn.
        options.splice(0..0, ["--cd".into(), self.cwd.clone()]);
        options.push("-e".into());
        match command.wrapper {
            CommandWrapper::Direct => options.extend(guest),
            CommandWrapper::LoginShell => {
                let inner = quoted(&guest, PathQuote::Posix)?;
                let inner = quoted(&[inner], PathQuote::Posix)?;
                options.extend([
                    "sh".into(),
                    "-lc".into(),
                    format!("exec ${{SHELL:-sh}} -lic {inner}"),
                ]);
            },
        }
        let launcher = if matches!(quote, PathQuote::Posix)
            && self.launcher.as_bytes().get(1) == Some(&b':')
        {
            "wsl.exe".to_owned()
        } else {
            self.launcher.clone()
        };
        let mut outer = vec![launcher];
        outer.extend(options);
        let text = quoted(&outer, quote)?;
        Some(if matches!(quote, PathQuote::PowerShell) { format!("& {text}") } else { text })
    }
}

fn valid_value(value: &str) -> bool {
    !value.is_empty() && value.len() <= 4096 && !value.chars().any(char::is_control)
}

fn valid_options(options: &[String]) -> bool {
    if options.len() > 24 || !options.iter().all(|option| valid_value(option)) {
        return false;
    }
    let mut at = usize::from(options.first().is_some_and(|option| option == "~"));
    while let Some(option) = options.get(at) {
        let (flag, inline) = option
            .split_once('=')
            .map_or((option.as_str(), None), |(flag, value)| (flag, Some(value)));
        if !matches!(flag, "-d" | "--distribution" | "-u" | "--user" | "--cd" | "--shell-type")
            || inline.is_some_and(str::is_empty)
        {
            return false;
        }
        at += 1;
        if inline.is_none() {
            if options.get(at).is_none_or(|value| value.starts_with('-')) {
                return false;
            }
            at += 1;
        }
    }
    true
}

fn quoted(args: &[String], quote: PathQuote) -> Option<String> {
    drop_text_for_paths(args, quote).map(|text| text.trim_end().to_owned())
}
