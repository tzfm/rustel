use super::*;

/// Print a shell auto-completion script for `rustel`, for the given shell.
pub(super) fn run_completions(shell: clap_complete::Shell) {
    use std::io::Write as _;
    let _ = std::io::stdout().write_all(completion_script(shell).as_bytes());
}

/// The completion script for a shell: clap's, with the zsh root repaired.
fn completion_script(shell: clap_complete::Shell) -> String {
    let mut script = Vec::new();
    clap_complete::generate(
        shell,
        &mut Cli::command(),
        product::COMMAND_NAME,
        &mut script,
    );
    let script = String::from_utf8_lossy(&script).into_owned();
    if shell == clap_complete::Shell::Zsh {
        zsh_commands_at_first_word(&script).unwrap_or(script)
    } else {
        script
    }
}

/// clap's zsh script gives word one to the root FILE and word two to the
/// commands, so `rustel stu<Tab>` offers files only and no command's own
/// flags are reached. Offer both at word one, branch on that word, and let
/// a word that is no command keep the root flags. `None` if clap's layout
/// is not the one this knows.
fn zsh_commands_at_first_word(script: &str) -> Option<String> {
    let name = product::COMMAND_NAME;
    let arguments = "_arguments \"${_arguments_options[@]}\" : \\\n";
    let commands = format!("\":: :_{name}_commands\" \\\n");
    let registry = format!("(( $+functions[_{name}_commands] )) ||");
    let close = "        esac\n    ;;\nesac\n}\n\n";

    let commands_at = script.find(&commands)?;
    let file_at = script[..commands_at].trim_end_matches('\n').rfind('\n')? + 1;
    if !script[file_at..commands_at].starts_with("':file -- ") {
        return None;
    }
    let root_flags = &script[script.find(arguments)? + arguments.len()..file_at];
    let close_at = script.find(&format!("{close}{registry}"))?;
    let branches = script[commands_at + commands.len()..close_at].replace("$line[2]", "$line[1]");

    let mut out = String::with_capacity(script.len() + root_flags.len() + 512);
    out.push_str(&script[..file_at]);
    out.push_str(&format!("\": :_{name}_file_or_command\" \\\n"));
    out.push_str(&branches);
    out.push_str(&format!(
        "            (*)\n{arguments}{root_flags}'*::file:_files' \\\n&& ret=0\n;;\n"
    ));
    out.push_str(close);
    out.push_str(&format!(
        "_{name}_file_or_command() {{\n    _alternative 'commands:command:_{name}_commands' 'files:file:_files'\n}}\n"
    ));
    out.push_str(&script[close_at + close.len()..]);
    Some(out)
}

/// The shell the user is in, from `$SHELL`: `/bin/zsh` is zsh.
pub(super) fn shell_from_environment() -> Option<clap_complete::Shell> {
    let shell = std::env::var_os("SHELL")?;
    let name = std::path::Path::new(&shell).file_name()?.to_str()?;
    match name {
        "bash" => Some(clap_complete::Shell::Bash),
        "zsh" => Some(clap_complete::Shell::Zsh),
        "fish" => Some(clap_complete::Shell::Fish),
        "elvish" => Some(clap_complete::Shell::Elvish),
        "pwsh" | "powershell" => Some(clap_complete::Shell::PowerShell),
        _ => None,
    }
}

/// Where a shell looks for a command's completions on its own, and the
/// rc line it needs, if any, to look there.
fn completion_home(shell: clap_complete::Shell) -> Result<(PathBuf, Option<String>), RuntimeError> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| RuntimeError::Message("no HOME to install into".into()))?;
    let xdg = |variable: &str, fallback: &str| {
        std::env::var_os(variable)
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(fallback))
    };
    let name = product::COMMAND_NAME;
    Ok(match shell {
        clap_complete::Shell::Zsh => {
            let zdot = std::env::var_os("ZDOTDIR")
                .map(PathBuf::from)
                .unwrap_or_else(|| home.clone());
            let dir = zdot.join(".zfunc");
            // The rc has to put the folder on fpath before compinit runs.
            let rc = zdot.join(".zshrc");
            let needs = std::fs::read_to_string(&rc)
                .map(|text| !text.contains(".zfunc"))
                .unwrap_or(true);
            (
                dir.join(format!("_{name}")),
                needs.then(|| format!("fpath+=(~/.zfunc)   # in {} before compinit", rc.display())),
            )
        }
        clap_complete::Shell::Bash => (
            xdg("XDG_DATA_HOME", ".local/share")
                .join("bash-completion/completions")
                .join(name),
            Some(
                "needs the bash-completion package, which loads it by itself; without it: source the file from ~/.bashrc"
                    .to_owned(),
            ),
        ),
        clap_complete::Shell::Fish => (
            xdg("XDG_CONFIG_HOME", ".config")
                .join("fish/completions")
                .join(format!("{name}.fish")),
            None,
        ),
        clap_complete::Shell::Elvish => {
            let path = xdg("XDG_CONFIG_HOME", ".config")
                .join("elvish/lib")
                .join(format!("{name}-completions.elv"));
            (path, Some(format!("use {name}-completions   # in rc.elv")))
        }
        clap_complete::Shell::PowerShell => {
            let path = xdg("XDG_CONFIG_HOME", ".config")
                .join("powershell")
                .join(format!("{name}-completions.ps1"));
            (
                path.clone(),
                Some(format!(". {}   # in $PROFILE", path.display())),
            )
        }
        other => {
            return Err(RuntimeError::Message(format!(
                "no install path known for {other}; print the script and place it yourself"
            )));
        }
    })
}

/// Write the completion script where the shell looks, and say what the
/// rc still needs - never editing the rc itself.
pub(super) fn install_completions(shell: clap_complete::Shell) -> Result<(), RuntimeError> {
    let (path, rc_line) = completion_home(shell)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, completion_script(shell))?;
    let on = style::stdout_on();
    println!(
        "{} {} for {shell}",
        style::green(on, "wrote"),
        style::bold(on, &path.display().to_string())
    );
    match rc_line {
        Some(line) => println!("then add this line and open a new shell:\n    {line}"),
        None => println!("open a new shell and it completes"),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_zsh_script_offers_commands_at_the_first_word() {
        let script = completion_script(clap_complete::Shell::Zsh);
        let name = product::COMMAND_NAME;
        assert!(script.contains(&format!("\": :_{name}_file_or_command\" \\\n")));
        assert!(script.contains(&format!("\n_{name}_file_or_command() {{\n")));
        assert!(!script.contains(&format!("\":: :_{name}_commands\"")));
        assert!(script.contains("case $line[1] in\n"));
        assert!(!script.contains("$line[2]"));
    }
}
