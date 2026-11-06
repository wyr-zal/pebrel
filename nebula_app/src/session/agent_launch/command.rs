//! Accept the literal argv and login-shell form used by Magpie's resume copy.

use super::{AgentCommand, AgentKind, CommandWrapper, valid_value};

pub(super) fn parse(argv: &[String], kind: AgentKind) -> Option<AgentCommand> {
    if !matches!(kind, AgentKind::Claude | AgentKind::Codex) {
        return None;
    }
    let (wrapper, words) = if argv.len() == 3 && argv[0] == "sh" && argv[1] == "-lc" {
        let script = argv[2].strip_prefix("exec ${SHELL:-sh} -lic ")?;
        let outer = literal_words(script)?;
        if outer.len() != 1 {
            return None;
        }
        (CommandWrapper::LoginShell, literal_words(&outer[0])?)
    } else {
        (CommandWrapper::Direct, argv.to_vec())
    };
    let (program, args) = words.split_first()?;
    if AgentKind::parse(program) != Some(kind)
        || words.len() > 32
        || !words.iter().all(|word| valid_value(word))
    {
        return None;
    }
    let command = AgentCommand { program: program.clone(), args: args.to_vec(), wrapper };
    prepare(&command, kind)?;
    Some(command)
}

pub(super) fn requested_session_id(command: &AgentCommand, kind: AgentKind) -> Option<String> {
    if kind == AgentKind::Claude && command.args.iter().any(|arg| arg == "--fork-session") {
        return None;
    }
    let id = prepare(command, kind)?.1?;
    crate::session::valid_session_uuid(&id).then_some(id)
}

pub(super) fn resumed_args(
    command: &AgentCommand,
    kind: AgentKind,
    id: &str,
) -> Option<Vec<String>> {
    kind.resume_command(id)?;
    let (mut options, _, resume_at) = prepare(command, kind)?;
    let flag = match kind {
        AgentKind::Claude => "--resume",
        AgentKind::Codex => "resume",
        _ => return None,
    };
    let at = if kind == AgentKind::Codex { resume_at.unwrap_or(options.len()) } else { 0 };
    options.splice(at..at, [flag.into(), id.into()]);
    Some(options)
}

pub(super) fn selector_args(command: &AgentCommand) -> Option<Vec<String>> {
    let (mut options, _, resume_at) = prepare(command, AgentKind::Codex)?;
    options.insert(resume_at.unwrap_or(options.len()), "resume".into());
    Some(options)
}

fn prepare(
    command: &AgentCommand,
    kind: AgentKind,
) -> Option<(Vec<String>, Option<String>, Option<usize>)> {
    if AgentKind::parse(&command.program) != Some(kind)
        || !valid_value(&command.program)
        || command.args.len() > 32
        || !command.args.iter().all(|arg| valid_value(arg))
    {
        return None;
    }
    let mut id = None;
    let mut options = Vec::new();
    let mut at = 0;
    let mut codex_resume_at = None;
    while let Some(arg) = command.args.get(at) {
        let (flag, inline) =
            arg.split_once('=').map_or((arg.as_str(), None), |(key, value)| (key, Some(value)));
        at += 1;
        if kind == AgentKind::Codex
            && flag == "resume"
            && inline.is_none()
            && codex_resume_at.is_none()
        {
            codex_resume_at = Some(options.len());
            if let Some(value) = command.args.get(at).filter(|arg| !arg.starts_with('-')) {
                remember_id(&mut id, kind, value)?;
                at += 1;
            }
            continue;
        }
        let identity =
            kind == AgentKind::Claude && matches!(flag, "--resume" | "-r" | "--session-id");
        if identity {
            let value = inline.or_else(|| {
                command.args.get(at).filter(|arg| !arg.starts_with('-')).map(String::as_str)
            });
            let Some(value) = value else {
                if flag == "--session-id" {
                    return None;
                }
                continue;
            };
            remember_id(&mut id, kind, value)?;
            at += usize::from(inline.is_none());
            continue;
        }
        let selection = match kind {
            AgentKind::Claude => matches!(flag, "--continue" | "-c" | "--fork-session"),
            AgentKind::Codex => matches!(flag, "--last" | "--all" | "--include-non-interactive"),
            _ => return None,
        };
        if selection {
            if inline.is_some() {
                return None;
            }
            continue;
        }
        let (takes_value, allowed) = allowed_option(kind, flag);
        if !allowed || inline.is_some() && !takes_value {
            return None;
        }
        let replaces_directory = kind == AgentKind::Codex && matches!(flag, "--cd" | "-C");
        if !replaces_directory {
            options.push(arg.clone());
        }
        if takes_value && inline.is_none() {
            let value = command.args.get(at)?;
            if !replaces_directory {
                options.push(value.clone());
            }
            at += 1;
        }
    }
    Some((options, id, codex_resume_at))
}

fn remember_id(current: &mut Option<String>, kind: AgentKind, value: &str) -> Option<()> {
    kind.resume_command(value)?;
    if current.as_deref().is_some_and(|previous| previous != value) {
        return None;
    }
    *current = Some(value.into());
    Some(())
}

fn allowed_option(kind: AgentKind, flag: &str) -> (bool, bool) {
    let valued = match kind {
        AgentKind::Claude => matches!(
            flag,
            "--model"
                | "--settings"
                | "--setting-sources"
                | "--permission-mode"
                | "--allowedTools"
                | "--disallowedTools"
                | "--add-dir"
                | "--mcp-config"
                | "--system-prompt"
                | "--append-system-prompt"
                | "--agents"
                | "--agent"
                | "--fallback-model"
                | "--debug-file"
                | "--plugin-dir"
        ),
        AgentKind::Codex => matches!(
            flag,
            "--model"
                | "-m"
                | "--profile"
                | "-p"
                | "--config"
                | "-c"
                | "--sandbox"
                | "-s"
                | "--ask-for-approval"
                | "-a"
                | "--cd"
                | "-C"
                | "--add-dir"
                | "--enable"
                | "--disable"
                | "--local-provider"
        ),
        _ => false,
    };
    let boolean = match kind {
        AgentKind::Claude => matches!(
            flag,
            "--dangerously-skip-permissions"
                | "--allow-dangerously-skip-permissions"
                | "--verbose"
                | "--disable-slash-commands"
                | "--strict-mcp-config"
        ),
        AgentKind::Codex => matches!(
            flag,
            "--full-auto"
                | "--dangerously-bypass-approvals-and-sandbox"
                | "--yolo"
                | "--approve-for-me"
                | "--not-so-yolo"
                | "--dangerously-bypass-hook-trust"
                | "--skip-git-repo-check"
                | "--no-alt-screen"
                | "--search"
                | "--oss"
                | "--no-daemon"
                | "--strict-config"
        ),
        _ => false,
    };
    (valued, valued || boolean)
}

/// Decode literal POSIX words only. Expansion, pipelines and additional Shell
/// commands are unsupported rather than being serialized for later execution.
pub(super) fn literal_words(text: &str) -> Option<Vec<String>> {
    if text.len() > 64 * 1024 || text.chars().any(char::is_control) {
        return None;
    }
    let mut words = Vec::new();
    let mut word = String::new();
    let mut quote = None;
    let mut started = false;
    let mut chars = text.chars();
    while let Some(ch) = chars.next() {
        match (quote, ch) {
            (Some('\''), '\'') | (Some('"'), '"') => quote = None,
            (Some('\''), ch) => word.push(ch),
            (Some('"'), '$' | '`') => return None,
            (Some('"'), '\\') => {
                let next = chars.next()?;
                if !matches!(next, '$' | '`' | '"' | '\\') {
                    word.push('\\');
                }
                word.push(next);
            },
            (Some('"'), ch) => word.push(ch),
            (None, '\'' | '"') => {
                quote = Some(ch);
                started = true;
            },
            (None, '\\') => {
                word.push(chars.next()?);
                started = true;
            },
            (None, ch) if ch.is_whitespace() => {
                if started {
                    words.push(std::mem::take(&mut word));
                    started = false;
                }
            },
            (
                None,
                '$' | '`' | ';' | '|' | '&' | '<' | '>' | '(' | ')' | '#' | '*' | '?' | '[' | ']'
                | '{' | '}' | '~',
            ) => return None,
            (None, ch) => {
                word.push(ch);
                started = true;
            },
            _ => return None,
        }
    }
    if quote.is_some() {
        return None;
    }
    if started {
        words.push(word);
    }
    (words.len() <= 32).then_some(words)
}
