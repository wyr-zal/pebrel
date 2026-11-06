use super::NEBULA_PROMPT_PS1;
use crate::osc_cwd::{CwdSniffer, OscEvent};
use base64::Engine as _;
use std::os::windows::process::CommandExt as _;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

#[test]
fn powershell_reports_absolute_cwd_while_displaying_home_as_tilde() {
    assert!(NEBULA_PROMPT_PS1.contains("$cwd = (Get-Location).Path"));
    assert!(NEBULA_PROMPT_PS1.contains("$loc = $cwd"));
    assert!(NEBULA_PROMPT_PS1.contains("$loc = '~' + $loc.Substring($hp.Length)"));
    assert!(NEBULA_PROMPT_PS1.contains("NEBULA|$reportedCwd|$branch"));
    assert!(
        !NEBULA_PROMPT_PS1.contains("NEBULA|$loc|$branch"),
        "the display-only tilde path must not leak into cwd consumers"
    );
}

fn run_case(program: &str, prelude: &str, checks: &str, expected_checks: usize) {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let directory = std::env::temp_dir().join(format!(
        "pebrel-cwd-test-{}-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
    ));
    std::fs::create_dir(&directory).unwrap();
    let script = directory.join("case.ps1");
    let mut contents = vec![0xEF, 0xBB, 0xBF];
    contents.extend_from_slice(
        format!(
            "{}\n{prelude}\n$global:ProbeBootstrap = {{\n{NEBULA_PROMPT_PS1}\n}}\n\
             . $global:ProbeBootstrap\n{checks}\n",
            PRELUDE
        )
        .as_bytes(),
    );
    std::fs::write(&script, contents).unwrap();
    let output = Command::new(program)
        .creation_flags(windows_sys::Win32::System::Threading::CREATE_NO_WINDOW)
        .args(["-NoLogo", "-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-File"])
        .arg(script)
        .output()
        .expect("the requested PowerShell must run; unavailable is not a passing test");
    assert!(
        output.status.success(),
        "{program}: {}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let mut sniffer = CwdSniffer::default();
    let mut latest = String::new();
    let mut last_title_cwd = String::new();
    let mut checked = 0;
    for line in String::from_utf8(output.stdout).unwrap().split_inclusive('\n') {
        for (_, event) in sniffer.feed(line.as_bytes()) {
            if let OscEvent::Cwd(cwd) = event {
                latest = cwd;
            }
        }
        if let Some((_, title)) = line.rsplit_once("\x1b]2;NEBULA|") {
            last_title_cwd = title.split('|').next().unwrap().to_owned();
        }
        if let Some(encoded) = line.trim_end().strip_prefix("EXPECTED_CWD=") {
            let expected = base64::engine::general_purpose::STANDARD.decode(encoded).unwrap();
            let expected = String::from_utf8(expected).unwrap().replace('\\', "/");
            assert_eq!(latest, expected, "cwd must already be current at Agent entry");
            assert!(std::path::Path::new(&latest).is_absolute());
            checked += 1;
        }
        if line.trim_end() == "EXPECT_SAME_PROMPT_CWD" {
            assert_eq!(latest, last_title_cwd, "prompt must not report a second directory change");
            checked += 1;
        }
    }
    assert_eq!(checked, expected_checks, "every fake Agent entry must be checked");
}

const PRELUDE: &str = r#"
$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = [Text.UTF8Encoding]::new($false)
$env:PEBREL_CONFIG_DIR = Join-Path $PSScriptRoot 'config'
$env:NEBULA_CONFIG_DIR = $env:PEBREL_CONFIG_DIR
function global:Clear-Host {}
function global:Set-PSReadLineOption {}
function global:Set-PSReadLineKeyHandler {}
function global:Get-PSReadLineKeyHandler { @() }
function global:git { $global:LASTEXITCODE = 0; 'fixture-branch' }
function global:PSConsoleHostReadLine { $global:ProbeLine }
function global:pebrel_cwd_agent {
    $path = $ExecutionContext.SessionState.Path.CurrentFileSystemLocation.ProviderPath
    [Console]::WriteLine("`nEXPECTED_CWD=" + [Convert]::ToBase64String([Text.Encoding]::UTF8.GetBytes($path)))
}
$a = Join-Path $PSScriptRoot 'A'
$b = Join-Path $PSScriptRoot "B team's #% project 空格"
$c = Join-Path $PSScriptRoot 'C'
foreach ($path in @($a, $b, $c)) { [void][IO.Directory]::CreateDirectory($path) }
Set-Location -LiteralPath $a
"#;

#[test]
fn powershell_cwd_updates_before_agent_entry_in_the_same_line() {
    run_case(
        "pwsh",
        "",
        r#"
$quotedB = $b.Replace("'", "''")
$global:TargetDirectory = $b
New-PSDrive -Name PebrelTestCwd -PSProvider FileSystem -Root $b | Out-Null
$commands = @(
    "Set-Location -LiteralPath '$quotedB'; pebrel_cwd_agent",
    'Set-Location -LiteralPath $global:TargetDirectory; pebrel_cwd_agent',
    "cd '$quotedB'; pebrel_cwd_agent",
    "Push-Location -LiteralPath '$quotedB'; pebrel_cwd_agent; Pop-Location; pebrel_cwd_agent",
    "Set-Location -LiteralPath '$quotedB'; Set-Location -LiteralPath '$c'; pebrel_cwd_agent",
    "Set-Location -LiteralPath '$quotedB\missing' -ErrorAction SilentlyContinue; pebrel_cwd_agent",
    'Set-Location HKCU:; pebrel_cwd_agent',
    'Set-Location PebrelTestCwd:\; pebrel_cwd_agent',
    "Set-Location -LiteralPath '$([IO.Path]::GetPathRoot($a))'; pebrel_cwd_agent"
)
foreach ($command in $commands) {
    Set-Location -LiteralPath $a
    $global:ProbeLine = $command
    $accepted = PSConsoleHostReadLine
    . ([scriptblock]::Create($accepted))
}
Set-Location -LiteralPath $a
$global:LASTEXITCODE = 73
$output = Set-Location -LiteralPath $b -PassThru
if ($output -isnot [System.Management.Automation.PathInfo] -or $LASTEXITCODE -ne 73) {
    throw 'The callback changed Set-Location output or native exit status'
}
pebrel_cwd_agent
"#,
        11,
    );
}

#[test]
fn powershell_cwd_callback_preserves_previous_handlers_and_reload() {
    run_case(
        "pwsh",
        r#"
$global:PreviousLocationCalls = 0
$ExecutionContext.SessionState.InvokeCommand.LocationChangedAction = {
    param($sender, $eventArgs)
    $global:PreviousLocationCalls++
}
"#,
        r#"
$installed = $ExecutionContext.SessionState.InvokeCommand.LocationChangedAction
1..3 | ForEach-Object { . $global:ProbeBootstrap }
if (-not [object]::ReferenceEquals($installed, $ExecutionContext.SessionState.InvokeCommand.LocationChangedAction)) {
    throw 'Reload installed a second callback'
}
Set-Location -LiteralPath $b
if ($PreviousLocationCalls -ne 1) { throw 'Previous callback was lost or invoked twice' }
pebrel_cwd_agent
$ExecutionContext.SessionState.InvokeCommand.LocationChangedAction = {
    param($sender, $eventArgs)
    if ($eventArgs.NewPath.ProviderPath -eq $a) { Set-Location -LiteralPath $c }
}
. $global:ProbeBootstrap
Set-Location -LiteralPath $a
if ((Get-Location).ProviderPath -ne $c) { throw 'User callback location change was lost' }
pebrel_cwd_agent
$ExecutionContext.SessionState.InvokeCommand.LocationChangedAction = {
    param($sender, $eventArgs)
    throw 'previous-location-failure'
}
. $global:ProbeBootstrap
try { Set-Location -LiteralPath $b; throw 'Expected the previous callback error' }
catch { if ($_.Exception.Message -notlike '*previous-location-failure*') { throw } }
pebrel_cwd_agent
"#,
        3,
    );
}

#[test]
fn windows_powershell_without_location_hook_still_loads_and_reports_at_prompt() {
    run_case(
        "powershell",
        "",
        r#"
if ($ExecutionContext.SessionState.InvokeCommand.PSObject.Properties['LocationChangedAction']) {
    throw 'This compatibility case requires Windows PowerShell 5.1'
}
Set-Location -LiteralPath $b
$rendered = prompt
if (-not $rendered.Contains("NEBULA|$($b.Replace('\', '/'))|")) { throw 'Prompt did not report the current directory' }
[Console]::Write($rendered)
if ($global:PebrelLocationChangedAction) { throw 'Unsupported callback was installed' }
"#,
        0,
    );
}

#[test]
fn powershell_cwd_uri_preserves_unc_roots_and_special_characters() {
    run_case(
        "pwsh",
        "",
        r#"
foreach ($path in @('C:\', '\\server\share\项目 #% test', "C:\team's\trailing ")) {
    $location = [pscustomobject]@{
        Provider = [pscustomobject]@{ Name = 'FileSystem' }
        ProviderPath = $path
    }
    Send-PebrelWorkingDirectory $location
    [Console]::WriteLine("`nEXPECTED_CWD=" + [Convert]::ToBase64String([Text.Encoding]::UTF8.GetBytes($path)))
}
"#,
        3,
    );
}

#[test]
fn powershell_cwd_change_and_following_prompt_use_the_same_path() {
    run_case(
        "pwsh",
        "",
        r#"
Set-Location -LiteralPath $b
pebrel_cwd_agent
[Console]::Write((prompt))
[Console]::WriteLine("`nEXPECT_SAME_PROMPT_CWD")
"#,
        2,
    );
}
