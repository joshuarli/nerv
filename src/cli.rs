use std::path::{Path, PathBuf};

use nerv::agent::EffortLevel;

pub enum Cmd {
    /// Interactive TUI session (default)
    Interactive {
        model: Option<String>,
        resume: ResumeOpt,
        log_level: Option<String>,
        prompt: Option<String>,
        thinking: bool,
        effort: Option<EffortLevel>,
        plan_mode: bool,
    },
    /// Interactive TUI session inside a fresh git worktree
    Wt {
        branch: String,
        model: Option<String>,
        log_level: Option<String>,
        prompt: Option<String>,
        thinking: bool,
        effort: Option<EffortLevel>,
        plan_mode: bool,
    },
    /// Headless: read prompt from stdin, stream JSON to stdout
    Print {
        model: Option<String>,
        max_turns: u32,
        verbose: bool,
    },
    /// Open session picker (no id) or load a specific session, then drop into
    /// TUI
    Resume {
        id: Option<String>,
    },
    /// Pure-chat mode: no tools, no project context, plain conversational
    /// assistant
    Talk {
        model: Option<String>,
        log_level: Option<String>,
        prompt: Option<String>,
        thinking: bool,
        effort: Option<EffortLevel>,
    },
    /// One-shot subcommands
    Models,
    Export {
        id: String,
    },
    Add {
        rest: Vec<String>,
    },
    Load {
        rest: Vec<String>,
    },
    Unload,
    Codemap {
        rest: Vec<String>,
    },
    Symbols {
        rest: Vec<String>,
    },
    Version,
    BenchStartup,
}

pub enum ResumeOpt {
    None,
    Picker,
    Session(String),
}

pub fn print_top_help() {
    println!("nerv — coding agent for the terminal");
    println!();
    println!("Usage: nerv [options]");
    println!("       nerv <command> [args]");
    println!();
    println!("Options:");
    println!("  --model <name>     Select model");
    println!("  --prompt <text>    Send an initial prompt automatically");
    println!("  --plan-mode        Enable plan mode from the first turn");
    println!("  --log-level <lvl>  Set log level (debug, info, warn, error)");
    println!("  -h, --help         Show this help");
    println!("  --version          Show version");
    println!();
    println!("Commands:");
    println!("  talk [--model M]               Pure-chat mode: no tools, no project context");
    println!("  resume [id]                    Open session picker, or resume a specific session");
    println!("  print [--model M] [--max-turns N] [--verbose]");
    println!("                                 Headless mode: read prompt from stdin, output JSON");
    println!("  wt <branch> [--model M]        Start session in a fresh git worktree on <branch>");
    println!("  models                         List all configured models and their status");
    println!("  export <id>                    Export session (HTML + JSONL) to ~/.nerv/exports/");
    println!("  add <hf-repo> <quant>          Download GGUF model from HuggingFace");
    println!("  load [alias]                   Start llama-server for a local model");
    println!("  unload                         Stop running llama-server");
    println!("  codemap <query> [path]         Show symbol implementations matching query");
    println!("  symbols <query> [path]         List symbol definitions matching query");
    println!();
    println!("Environment:");
    println!("  NERV_LOG=<level>   Set log level (default: warn)");
}

/// Parse CLI args with lexopt. Returns the resolved Cmd or exits on error.
pub fn parse_args() -> Cmd {
    use lexopt::prelude::*;

    let mut parser = lexopt::Parser::from_env();

    // Peek at the first positional to route subcommands.
    // lexopt doesn't have lookahead, so we collect args into a command.
    let mut model: Option<String> = None;
    let mut log_level: Option<String> = None;
    let _wt: Option<String> = None;

    // Read leading flags before the subcommand name, then route on the first
    // positional value (or return Interactive if there are no positional args).
    let mut prompt: Option<String> = None;
    let mut thinking = false;
    let mut effort: Option<EffortLevel> = None;
    let mut plan_mode = false;

    let first = loop {
        match parser.next() {
            Ok(None) => {
                return Cmd::Interactive {
                    model,
                    resume: ResumeOpt::None,
                    log_level,
                    prompt,
                    thinking,
                    effort,
                    plan_mode,
                };
            }
            Err(e) => {
                eprintln!("error: {e}. Try: nerv --help");
                std::process::exit(1);
            }
            Ok(Some(Short('h') | Long("help"))) => {
                print_top_help();
                std::process::exit(0);
            }
            Ok(Some(Long("version"))) => return Cmd::Version,
            Ok(Some(Long("model"))) => {
                model = Some(
                    parser
                        .value()
                        .unwrap_or_else(|_| {
                            eprintln!("--model requires a value");
                            std::process::exit(1);
                        })
                        .string()
                        .unwrap(),
                );
            }
            Ok(Some(Long("log-level"))) => {
                log_level = Some(
                    parser
                        .value()
                        .unwrap_or_else(|_| {
                            eprintln!("--log-level requires a value");
                            std::process::exit(1);
                        })
                        .string()
                        .unwrap(),
                );
            }
            Ok(Some(Long("prompt"))) => {
                prompt = Some(
                    parser
                        .value()
                        .unwrap_or_else(|_| {
                            eprintln!("--prompt requires a value");
                            std::process::exit(1);
                        })
                        .string()
                        .unwrap(),
                );
            }
            Ok(Some(Long("thinking"))) => thinking = true,
            Ok(Some(Long("plan-mode"))) => plan_mode = true,
            Ok(Some(Long("effort"))) => {
                effort = Some(parse_effort_level(
                    &parser
                        .value()
                        .unwrap_or_else(|_| {
                            eprintln!("--effort requires a value");
                            std::process::exit(1);
                        })
                        .string()
                        .unwrap(),
                ));
            }
            Ok(Some(arg)) => break arg,
        }
    };

    match first {
        // ── subcommands ──────────────────────────────────────────────────────
        Value(v) if v == "talk" => {
            // talk [-h] [--model M] [--log-level L] [--prompt P] [--thinking] [--effort E]
            let mut talk_model: Option<String> = None;
            let mut talk_log: Option<String> = None;
            let mut talk_prompt: Option<String> = None;
            let mut talk_thinking = false;
            let mut talk_effort: Option<EffortLevel> = None;
            loop {
                match parser.next() {
                    Ok(None) => break,
                    Ok(Some(Short('h') | Long("help"))) => {
                        println!("Usage: nerv talk [options]");
                        println!();
                        println!("Pure-chat mode: no tools, no project context, no memory.");
                        println!("Opens the TUI as a plain conversational assistant.");
                        println!();
                        println!("Options:");
                        println!("  --model <name>      Model to use");
                        println!("  --log-level <lvl>   Log level (debug, info, warn, error)");
                        println!("  --prompt <text>     Send an initial prompt automatically");
                        println!("  --thinking          Enable extended thinking");
                        println!("  --effort <level>    Thinking effort: off|low|medium|high|max");
                        println!("  -h, --help          Show this help");
                        std::process::exit(0);
                    }
                    Ok(Some(Long("model"))) => {
                        talk_model = Some(
                            parser
                                .value()
                                .unwrap_or_else(|_| {
                                    eprintln!("--model requires a value");
                                    std::process::exit(1);
                                })
                                .string()
                                .unwrap(),
                        );
                    }
                    Ok(Some(Long("log-level"))) => {
                        talk_log = Some(
                            parser
                                .value()
                                .unwrap_or_else(|_| {
                                    eprintln!("--log-level requires a value");
                                    std::process::exit(1);
                                })
                                .string()
                                .unwrap(),
                        );
                    }
                    Ok(Some(Long("prompt"))) => {
                        talk_prompt = Some(
                            parser
                                .value()
                                .unwrap_or_else(|_| {
                                    eprintln!("--prompt requires a value");
                                    std::process::exit(1);
                                })
                                .string()
                                .unwrap(),
                        );
                    }
                    Ok(Some(Long("thinking"))) => talk_thinking = true,
                    Ok(Some(Long("effort"))) => {
                        talk_effort = Some(parse_effort_level(
                            &parser
                                .value()
                                .unwrap_or_else(|_| {
                                    eprintln!("--effort requires a value");
                                    std::process::exit(1);
                                })
                                .string()
                                .unwrap(),
                        ));
                    }
                    Ok(Some(arg)) => {
                        eprintln!("nerv talk: unexpected argument. Try: nerv talk --help");
                        eprintln!("  {}", arg.unexpected());
                        std::process::exit(1);
                    }
                    Err(e) => {
                        eprintln!("nerv talk: {e}. Try: nerv talk --help");
                        std::process::exit(1);
                    }
                }
            }
            Cmd::Talk {
                model: talk_model,
                log_level: talk_log,
                prompt: talk_prompt,
                thinking: talk_thinking,
                effort: talk_effort,
            }
        }
        Value(v) if v == "resume" => {
            // resume [-h] [id]
            match parser.next() {
                Ok(Some(Short('h') | Long("help"))) => {
                    println!("Usage: nerv resume [id]");
                    println!();
                    println!("Open the session picker, or resume a specific session by id prefix.");
                    println!();
                    println!("Arguments:");
                    println!("  id   Session id (or prefix) to resume directly");
                    std::process::exit(0);
                }
                Ok(Some(Value(id))) => Cmd::Resume { id: Some(id.string().unwrap()) },
                Ok(None) => Cmd::Resume { id: None },
                Ok(Some(arg)) => {
                    eprintln!("nerv resume: unexpected argument '{}'", arg.unexpected());
                    std::process::exit(1);
                }
                Err(e) => {
                    eprintln!("nerv resume: {e}");
                    std::process::exit(1);
                }
            }
        }
        Value(v) if v == "print" => {
            // print [-h] [--model M] [--max-turns N] [--verbose]
            let mut p_model: Option<String> = None;
            let mut max_turns: u32 = 20;
            let mut verbose = false;
            loop {
                match parser.next() {
                    Ok(None) => break,
                    Ok(Some(Short('h') | Long("help"))) => {
                        println!("Usage: nerv print [options]");
                        println!();
                        println!(
                            "Headless mode: read prompt from stdin, run agent, output JSON to stdout."
                        );
                        println!();
                        println!("Options:");
                        println!("  --model <name>      Model to use (e.g. opus, sonnet)");
                        println!("  --max-turns <n>     Max agent turns (default: 20)");
                        println!("  --verbose           Stream tool progress to stderr");
                        println!("  -h, --help          Show this help");
                        std::process::exit(0);
                    }
                    Ok(Some(Long("model"))) => {
                        p_model = Some(
                            parser
                                .value()
                                .unwrap_or_else(|_| {
                                    eprintln!("--model requires a value");
                                    std::process::exit(1);
                                })
                                .string()
                                .unwrap(),
                        );
                    }
                    Ok(Some(Long("max-turns"))) => {
                        let s = parser
                            .value()
                            .unwrap_or_else(|_| {
                                eprintln!("--max-turns requires a value");
                                std::process::exit(1);
                            })
                            .string()
                            .unwrap();
                        max_turns = s.parse().unwrap_or_else(|_| {
                            eprintln!("--max-turns must be a number");
                            std::process::exit(1);
                        });
                    }
                    Ok(Some(Long("verbose"))) => verbose = true,
                    Ok(Some(arg)) => {
                        eprintln!("nerv print: unexpected argument. Try: nerv print --help");
                        eprintln!("  {}", arg.unexpected());
                        std::process::exit(1);
                    }
                    Err(e) => {
                        eprintln!("nerv print: {e}. Try: nerv print --help");
                        std::process::exit(1);
                    }
                }
            }
            Cmd::Print { model: p_model, max_turns, verbose }
        }
        Value(v) if v == "wt" => {
            // wt [-h] <branch> [--model M] [--log-level L] [--prompt P] [--thinking]
            // [--effort E]
            let mut wt_model: Option<String> = None;
            let mut wt_log: Option<String> = None;
            let mut wt_prompt: Option<String> = None;
            let mut wt_thinking = false;
            let mut wt_effort: Option<EffortLevel> = None;
            let mut wt_plan_mode = false;
            let mut branch: Option<String> = None;
            loop {
                match parser.next() {
                    Ok(None) => break,
                    Ok(Some(Short('h') | Long("help"))) => {
                        println!("Usage: nerv wt <branch> [options]");
                        println!();
                        println!(
                            "Start an interactive session in a fresh git worktree on <branch>."
                        );
                        println!(
                            "The worktree is created under ~/.nerv/worktrees/ and checked out"
                        );
                        println!("to a new branch. Merged and cleaned up when the session ends.");
                        println!();
                        println!("Arguments:");
                        println!("  branch   Name of the new git branch to create");
                        println!();
                        println!("Options:");
                        println!("  --model <name>      Model to use");
                        println!("  --log-level <lvl>   Log level (debug, info, warn, error)");
                        println!("  --prompt <text>     Send an initial prompt automatically");
                        println!("  --thinking          Enable extended thinking");
                        println!("  --effort <level>    Thinking effort: off|low|medium|high|max");
                        println!("  -h, --help          Show this help");
                        std::process::exit(0);
                    }
                    Ok(Some(Long("model"))) => {
                        wt_model = Some(
                            parser
                                .value()
                                .unwrap_or_else(|_| {
                                    eprintln!("--model requires a value");
                                    std::process::exit(1);
                                })
                                .string()
                                .unwrap(),
                        );
                    }
                    Ok(Some(Long("log-level"))) => {
                        wt_log = Some(
                            parser
                                .value()
                                .unwrap_or_else(|_| {
                                    eprintln!("--log-level requires a value");
                                    std::process::exit(1);
                                })
                                .string()
                                .unwrap(),
                        );
                    }
                    Ok(Some(Long("prompt"))) => {
                        wt_prompt = Some(
                            parser
                                .value()
                                .unwrap_or_else(|_| {
                                    eprintln!("--prompt requires a value");
                                    std::process::exit(1);
                                })
                                .string()
                                .unwrap(),
                        );
                    }
                    Ok(Some(Long("thinking"))) => wt_thinking = true,
                    Ok(Some(Long("plan-mode"))) => wt_plan_mode = true,
                    Ok(Some(Long("effort"))) => {
                        wt_effort = Some(parse_effort_level(
                            &parser
                                .value()
                                .unwrap_or_else(|_| {
                                    eprintln!("--effort requires a value");
                                    std::process::exit(1);
                                })
                                .string()
                                .unwrap(),
                        ));
                    }
                    Ok(Some(Value(v))) if branch.is_none() => {
                        branch = Some(v.string().unwrap());
                    }
                    Ok(Some(arg)) => {
                        eprintln!("nerv wt: unexpected argument. Try: nerv wt --help");
                        eprintln!("  {}", arg.unexpected());
                        std::process::exit(1);
                    }
                    Err(e) => {
                        eprintln!("nerv wt: {e}. Try: nerv wt --help");
                        std::process::exit(1);
                    }
                }
            }
            let branch = branch.unwrap_or_else(|| {
                eprintln!("nerv wt: branch name required. Try: nerv wt --help");
                std::process::exit(1);
            });
            Cmd::Wt {
                branch,
                model: wt_model,
                log_level: wt_log,
                prompt: wt_prompt,
                thinking: wt_thinking,
                effort: wt_effort,
                plan_mode: wt_plan_mode,
            }
        }
        Value(v) if v == "models" => {
            if matches!(parser.next(), Ok(Some(Short('h') | Long("help")))) {
                println!("Usage: nerv models");
                println!();
                println!("List all configured models and their online status.");
                std::process::exit(0);
            }
            Cmd::Models
        }
        Value(v) if v == "export" => match parser.next() {
            Ok(Some(Short('h') | Long("help"))) => {
                println!("Usage: nerv export <session-id>");
                println!();
                println!("Export a session to HTML and JSONL in ~/.nerv/exports/.");
                std::process::exit(0);
            }
            Ok(Some(Value(id))) => Cmd::Export { id: id.string().unwrap() },
            _ => {
                eprintln!("Usage: nerv export <session-id>");
                std::process::exit(1);
            }
        },
        Value(v) if v == "add" => {
            if matches!(parser.clone().next(), Ok(Some(Short('h') | Long("help")))) {
                println!("Usage: nerv add <hf-repo> <quant>");
                println!();
                println!("Download a GGUF model from HuggingFace and register it.");
                println!();
                println!("Arguments:");
                println!("  hf-repo   HuggingFace repo (e.g. Org/Model-GGUF)");
                println!("  quant     Quantization level (e.g. Q4_K_M)");
                std::process::exit(0);
            }
            Cmd::Add { rest: remaining_strings(parser) }
        }
        Value(v) if v == "load" => {
            if matches!(parser.clone().next(), Ok(Some(Short('h') | Long("help")))) {
                println!("Usage: nerv load [alias]");
                println!();
                println!("Start llama-server for a local GGUF model.");
                println!("Replaces this process via exec — Ctrl+C to stop.");
                std::process::exit(0);
            }
            Cmd::Load { rest: remaining_strings(parser) }
        }
        Value(v) if v == "unload" => {
            if matches!(parser.next(), Ok(Some(Short('h') | Long("help")))) {
                println!("Usage: nerv unload");
                println!();
                println!("Stop the running llama-server (Ctrl+C the process directly).");
                std::process::exit(0);
            }
            Cmd::Unload
        }
        Value(v) if v == "codemap" => {
            if matches!(parser.clone().next(), Ok(Some(Short('h') | Long("help")))) {
                println!(
                    "Usage: nerv codemap <query> [path] [--kind <kind>] [--depth full|signatures]"
                );
                println!();
                println!("Show symbol implementations matching a query.");
                println!();
                println!("Options:");
                println!("  --kind <kind>          Filter by symbol kind");
                println!("  --depth full|signatures  Output verbosity (default: full)");
                std::process::exit(0);
            }
            Cmd::Codemap { rest: remaining_strings(parser) }
        }
        Value(v) if v == "bench-startup" => Cmd::BenchStartup,
        Value(v) if v == "symbols" => {
            if matches!(parser.clone().next(), Ok(Some(Short('h') | Long("help")))) {
                println!("Usage: nerv symbols <query> [path] [--kind <kind>] [--refs]");
                println!();
                println!("List symbol definitions matching a query.");
                println!();
                println!("Options:");
                println!("  --kind <kind>   Filter by symbol kind");
                println!("  --refs          Also show call sites via ripgrep");
                std::process::exit(0);
            }
            Cmd::Symbols { rest: remaining_strings(parser) }
        }
        Value(v) => {
            eprintln!("unknown command '{}'. Try: nerv --help", v.to_string_lossy());
            std::process::exit(1);
        }
        arg => {
            eprintln!("unexpected argument. Try: nerv --help");
            eprintln!("  {}", arg.unexpected());
            std::process::exit(1);
        }
    }
}

/// Parse an --effort flag value; exits on unrecognised input.
pub fn parse_effort_level(s: &str) -> EffortLevel {
    match s {
        "low" => EffortLevel::Low,
        "medium" => EffortLevel::Medium,
        "high" => EffortLevel::High,
        "max" => EffortLevel::Max,
        other => {
            eprintln!("--effort: unknown level '{other}'. Choose: low, medium, high, max");
            std::process::exit(1);
        }
    }
}

/// Drain remaining positional values from a lexopt parser into a Vec<String>.
pub fn remaining_strings(mut parser: lexopt::Parser) -> Vec<String> {
    use lexopt::prelude::*;
    let mut out = Vec::new();
    loop {
        match parser.next() {
            Ok(Some(lexopt::Arg::Value(v))) => out.push(v.string().unwrap_or_default()),
            Ok(Some(lexopt::Arg::Long(l))) => out.push(format!("--{}", l)),
            Ok(Some(lexopt::Arg::Short(s))) => out.push(format!("-{}", s)),
            Ok(None) => break,
            Err(_) => break,
        }
    }
    out
}

/// Outcome of the pre-TUI repository gate check.
pub enum RepoGateResult {
    /// Proceed into normal TUI mode.
    Continue,
    /// Switch to talk mode (no tools / context).
    Talk,
    /// Exit cleanly.
    Exit,
}

/// Check whether the current directory is a known git repository.
///
/// - Not a git repo  →  print prompt, read single keypress: [e]xit / [t]alk
/// - Git repo, never seen before  →  print prompt, read single keypress:
///   [c]ontinue / [t]alk
/// - Git repo, already in the DB  →  return Continue immediately
///
/// Uses raw terminal I/O directly so we don't need to spin up the full TUI.
pub fn repo_gate(cwd: &std::path::Path, nerv_dir: &std::path::Path) -> RepoGateResult {
    use std::io::Write;

    let repo_root = nerv::find_repo_root(cwd);

    // Determine which case we're in: 0 = no repo, 1 = unknown repo.
    let is_no_repo: bool;

    match &repo_root {
        None => {
            is_no_repo = true;
        }
        Some(root) => {
            match nerv::repo_fingerprint(root) {
                // git found .git but couldn't fingerprint — treat as no-repo
                None => {
                    is_no_repo = true;
                }
                Some(ref fpr) => {
                    let repo_dir = nerv_dir.join("repos").join(fpr);
                    let _ = std::fs::create_dir_all(&repo_dir);
                    let sm = nerv::session::SessionManager::new(&repo_dir);
                    if sm.has_sessions_for_repo(fpr) {
                        // Known repo — proceed silently.
                        return RepoGateResult::Continue;
                    }
                    is_no_repo = false;
                }
            }
        }
    }

    // We need to prompt the user.  Put stdin into raw mode, print the message,
    // read one byte, restore terminal, then act on the key.
    let dir = cwd.display();
    let prompt = if is_no_repo {
        format!("repository not found: {dir}\r\n  \x1b[2m[e]\x1b[0mexit  \x1b[2m[t]\x1b[0mtalk\r\n")
    } else {
        format!(
            "previously unknown repository: {dir}\r\n  \x1b[2m[c]\x1b[0mcontinue  \x1b[2m[t]\x1b[0mtalk\r\n"
        )
    };

    // Enter raw mode, write prompt, read one byte, restore.
    let key = unsafe {
        use std::mem::MaybeUninit;

        let stdin_fd = libc::STDIN_FILENO;
        let mut orig = MaybeUninit::<libc::termios>::uninit();
        let has_tty = libc::tcgetattr(stdin_fd, orig.as_mut_ptr()) == 0;

        if has_tty {
            let orig = orig.assume_init();
            let mut raw = orig;
            libc::cfmakeraw(&mut raw);
            libc::tcsetattr(stdin_fd, libc::TCSANOW, &raw);

            // Write directly to stdout.
            let _ = std::io::stdout().write_all(prompt.as_bytes());
            let _ = std::io::stdout().flush();

            let mut buf = [0u8; 1];
            let n = libc::read(stdin_fd, buf.as_mut_ptr() as *mut libc::c_void, 1);

            libc::tcsetattr(stdin_fd, libc::TCSAFLUSH, &orig);

            // Print newline after key so subsequent output starts fresh.
            let _ = std::io::stdout().write_all(b"\r\n");
            let _ = std::io::stdout().flush();

            if n == 1 { buf[0] } else { b'e' }
        } else {
            // Non-interactive stdin: auto-continue for unknown repos, exit for no-repo.
            if is_no_repo { b'e' } else { b'c' }
        }
    };

    match key {
        b't' | b'T' => RepoGateResult::Talk,
        b'c' | b'C' | b'\r' | b'\n' if !is_no_repo => RepoGateResult::Continue,
        _ => RepoGateResult::Exit,
    }
}

pub fn list_all_models() {
    let nerv_dir = nerv::nerv_dir();
    let config = nerv::core::NervConfig::load(nerv_dir);
    let registry = nerv::core::model_registry::ModelRegistry::new(&config, nerv_dir);

    let all = registry.all_models();

    if all.is_empty() {
        println!("No models configured. Run `nerv --login` or set ANTHROPIC_API_KEY.");
        return;
    }

    // Collect unique providers with a registered Arc<dyn Provider> and healthcheck
    // each. This is intentionally blocking — `nerv models` / `nerv
    // --list-models` is a CLI command.
    use std::collections::HashMap;
    let mut provider_health: HashMap<String, bool> = HashMap::new();
    for m in &all {
        if provider_health.contains_key(&m.provider_name) {
            continue;
        }
        let online =
            if let Some(p) = registry.provider_registry.read().unwrap().get(&m.provider_name) {
                p.healthcheck()
            } else {
                false // provider not registered (no auth)
            };
        provider_health.insert(m.provider_name.clone(), online);
    }

    // Emit config warnings for unknown model ids
    let known_ids: Vec<&str> = all.iter().map(|m| m.id.as_str()).collect();
    for warning in config.validate_model_ids(&known_ids) {
        eprintln!("⚠  {}", warning);
    }

    use nerv::interactive::theme;
    let mut last_provider = String::new();
    for m in &all {
        if m.provider_name != last_provider {
            println!("\n  [{}]", m.provider_name);
            last_provider = m.provider_name.clone();
        }
        let online = *provider_health.get(&m.provider_name).unwrap_or(&false);
        let marker = if online {
            format!("{}●{}", theme::SUCCESS, theme::RESET)
        } else {
            format!("{}○{}", theme::FOOTER_DIM, theme::RESET)
        };
        println!("    {} {:<30} ctx:{}  {}", marker, m.id, m.context_window, m.name);
    }
    println!();
}

pub fn handle_subcommand(cmd: &str, args: &[String], nerv_dir: &Path) {
    use nerv::core::local_models::*;
    if nerv::git_bin().is_none() {
        eprintln!("error: `git` not found in $PATH — nerv requires git");
        std::process::exit(1);
    }

    match cmd {
        "models" => {
            list_all_models();
            let models = load_models(nerv_dir);
            if !models.is_empty() {
                println!("  [local gguf]");
                for m in &models {
                    use nerv::interactive::theme;
                    let status = if is_healthy(m.port) {
                        format!("{}●{}", theme::SUCCESS, theme::RESET)
                    } else {
                        format!("{}○{}", theme::FOOTER_DIM, theme::RESET)
                    };
                    println!(
                        "    {} {:<20} ctx:{:<6} gpu:{:<3} port:{}  {}",
                        status, m.alias, m.context_length, m.gpu_layers, m.port, m.path,
                    );
                }
                println!();
            }
        }
        "add" => {
            if args.len() < 2 {
                eprintln!("Usage: nerv add <hf-repo> <quant>");
                eprintln!(
                    "Example: nerv add Jackrong/Qwen3.5-9B-Claude-4.6-Opus-Reasoning-Distilled-v2-GGUF Q4_K_M"
                );
                std::process::exit(1);
            }
            let hf_repo = &args[0];
            let quant = &args[1];

            let cache_dir = nerv_dir.join("models");
            match download_gguf(hf_repo, quant, &cache_dir) {
                Ok(local_path) => {
                    let mut models = load_models(nerv_dir);

                    // Derive alias from repo name + quant
                    let base_alias = hf_repo
                        .rsplit('/')
                        .next()
                        .unwrap_or(hf_repo)
                        .to_lowercase()
                        .replace("-gguf", "")
                        .chars()
                        .take(30)
                        .collect::<String>();

                    // Append quant to alias for uniqueness
                    let alias = format!("{}-{}", base_alias, quant.to_lowercase());

                    if models.iter().any(|m| m.alias == alias) {
                        println!("Model '{}' already in models.json", alias);
                        return;
                    }

                    // Auto-detect hardware and compute defaults
                    let mut model = recommended_defaults(&local_path);
                    model.alias = alias.clone();
                    model.hf_repo = Some(hf_repo.to_string());

                    println!("Hardware: {:.0}GB RAM, {} cores", sysctl_mem_gb(), sysctl_cores(),);
                    println!(
                        "Defaults: ctx:{} gpu:{} batch:{} threads:{}",
                        model.context_length,
                        model.gpu_layers,
                        model
                            .extra_args
                            .iter()
                            .position(|a| a == "-b")
                            .and_then(|i| model.extra_args.get(i + 1))
                            .map(|s| s.as_str())
                            .unwrap_or("?"),
                        model
                            .extra_args
                            .iter()
                            .position(|a| a == "-t")
                            .and_then(|i| model.extra_args.get(i + 1))
                            .map(|s| s.as_str())
                            .unwrap_or("?"),
                    );

                    models.push(model);

                    if let Err(e) = save_models(nerv_dir, &models) {
                        eprintln!("Failed to save models.json: {}", e);
                    } else {
                        println!("Added '{}' to ~/.nerv/models.json", alias);
                        println!("Run: nerv load {}", alias);
                    }
                }
                Err(e) => {
                    eprintln!("Download failed: {}", e);
                    std::process::exit(1);
                }
            }
        }
        "load" => {
            let models = load_models(nerv_dir);
            if models.is_empty() {
                eprintln!("No models configured. Use `nerv add <hf-repo> [quant]` first.");
                std::process::exit(1);
            }

            let model = if let Some(alias) = args.first() {
                models.iter().find(|m| m.alias == *alias).cloned()
            } else if models.len() == 1 {
                Some(models[0].clone())
            } else {
                println!("Available models:");
                for (i, m) in models.iter().enumerate() {
                    println!("  [{}] {}", i + 1, m.alias);
                }
                print!("Select: ");
                let _ = std::io::Write::flush(&mut std::io::stdout());
                let mut input = String::new();
                let _ = std::io::stdin().read_line(&mut input);
                let idx: usize = input.trim().parse().unwrap_or(0);
                if idx >= 1 && idx <= models.len() { Some(models[idx - 1].clone()) } else { None }
            };

            let Some(model) = model else {
                eprintln!("Model not found");
                std::process::exit(1);
            };

            if !model.resolved_path().exists() {
                eprintln!("Model file not found: {}", model.resolved_path().display());
                std::process::exit(1);
            }

            let server = find_llama_server().unwrap_or_else(|| {
                eprintln!("llama-server not found on PATH. Install: brew install llama.cpp");
                std::process::exit(1);
            });

            let server_args = model.server_args();
            eprintln!("  {} {}", server.display(), server_args.join(" "));

            // exec — replaces this process with llama-server
            use std::ffi::CString;
            let c_prog = CString::new(server.to_string_lossy().as_bytes()).unwrap();
            let mut c_args: Vec<CString> = vec![c_prog.clone()];
            for a in &server_args {
                c_args.push(CString::new(a.as_bytes()).unwrap());
            }
            let c_ptrs: Vec<*const libc::c_char> = c_args
                .iter()
                .map(|a| a.as_ptr())
                .chain(std::iter::once(std::ptr::null()))
                .collect();
            unsafe { libc::execvp(c_prog.as_ptr(), c_ptrs.as_ptr()) };
            eprintln!("exec failed");
            std::process::exit(1);
        }
        "unload" => {
            println!("With exec mode, just Ctrl+C the llama-server process.");
        }
        "export" => {
            let session_id = args.first().unwrap_or_else(|| {
                eprintln!("Usage: nerv export <session-id>");
                std::process::exit(1);
            });
            let exports_dir = nerv_dir.join("exports");
            if let Err(e) = std::fs::create_dir_all(&exports_dir) {
                eprintln!("Failed to create exports directory: {}", e);
                std::process::exit(1);
            }
            let html_path = exports_dir.join(format!("{}.html", session_id));
            let jsonl_path = exports_dir.join(format!("{}.jsonl", session_id));
            match nerv::export::export_session_html(session_id, &html_path, nerv_dir) {
                Ok(path) => println!("Exported to {}", path),
                Err(e) => {
                    eprintln!("HTML export failed: {}", e);
                    std::process::exit(1);
                }
            }
            match nerv::export::export_session_jsonl(session_id, &jsonl_path, nerv_dir) {
                Ok(path) => println!("Exported to {}", path),
                Err(e) => {
                    eprintln!("JSONL export failed: {}", e);
                    std::process::exit(1);
                }
            }
        }
        "codemap" => {
            if args.is_empty() {
                eprintln!(
                    "Usage: nerv codemap <query> [path] [--kind <kind>] [--depth full|signatures]"
                );
                std::process::exit(1);
            }
            let query = &args[0];
            let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));

            // Parse optional flags and positional path arg
            let mut kind_str = None;
            let mut file_str = None;
            let mut depth_str = None;
            let mut i = 1;
            while i < args.len() {
                match args[i].as_str() {
                    "--kind" => {
                        kind_str = args.get(i + 1).map(|s| s.as_str());
                        i += 2;
                    }
                    "--file" => {
                        file_str = args.get(i + 1).map(|s| s.as_str());
                        i += 2;
                    }
                    "--depth" => {
                        depth_str = args.get(i + 1).map(|s| s.as_str());
                        i += 2;
                    }
                    s if !s.starts_with('-') && file_str.is_none() => {
                        file_str = Some(s);
                        i += 1;
                    }
                    _ => {
                        i += 1;
                    }
                }
            }

            let kind = kind_str.and_then(nerv::index::codemap::parse_kind);
            let depth = depth_str
                .map(nerv::index::codemap::parse_depth)
                .unwrap_or(nerv::index::codemap::Depth::Full);
            let file_path =
                file_str.map(|f| if f.starts_with('/') { PathBuf::from(f) } else { cwd.join(f) });

            let mut index = nerv::index::SymbolIndex::new();
            index.force_index_dir(&cwd);

            let params = nerv::index::codemap::CodemapParams {
                query,
                kind,
                file: file_path.as_deref(),
                depth,
                match_mode: nerv::index::codemap::MatchMode::Substring,
                from: None,
            };
            println!("{}", nerv::index::codemap::codemap(&index, &cwd, &params));
        }
        "symbols" => {
            if args.is_empty() {
                eprintln!("Usage: nerv symbols <query> [path] [--kind <kind>] [--refs]");
                std::process::exit(1);
            }
            let query = &args[0];
            let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));

            let mut kind_str = None;
            let mut file_str = None;
            let mut want_refs = false;
            let mut i = 1;
            while i < args.len() {
                match args[i].as_str() {
                    "--kind" => {
                        kind_str = args.get(i + 1).map(|s| s.as_str());
                        i += 2;
                    }
                    "--file" => {
                        file_str = args.get(i + 1).map(|s| s.as_str());
                        i += 2;
                    }
                    "--refs" => {
                        want_refs = true;
                        i += 1;
                    }
                    s if !s.starts_with('-') && file_str.is_none() => {
                        file_str = Some(s);
                        i += 1;
                    }
                    _ => {
                        i += 1;
                    }
                }
            }

            let kind = kind_str.and_then(nerv::index::codemap::parse_kind);
            let file_path =
                file_str.map(|f| if f.starts_with('/') { PathBuf::from(f) } else { cwd.join(f) });

            let mut index = nerv::index::SymbolIndex::new();
            index.force_index_dir(&cwd);

            let results = index.search(query, kind, file_path.as_deref());
            if results.is_empty() {
                println!("No definitions found");
            } else {
                for sym in &results {
                    let rel = sym.file.strip_prefix(&cwd).unwrap_or(&sym.file).display();
                    let parent_suffix =
                        sym.parent.as_ref().map(|p| format!("  ({})", p)).unwrap_or_default();
                    println!(
                        "  {}:{:<4}  {} {}{}",
                        rel,
                        sym.line,
                        sym.kind.label(),
                        sym.signature,
                        parent_suffix,
                    );
                }
                println!("\n{} definitions", results.len());
            }

            if want_refs {
                if let Some(rg) = nerv::rg() {
                    let output = std::process::Command::new(rg)
                        .args([
                            "--no-heading",
                            "--line-number",
                            "--color=never",
                            "--word-regexp",
                            query,
                        ])
                        .current_dir(&cwd)
                        .output();
                    if let Ok(o) = output {
                        let refs = String::from_utf8_lossy(&o.stdout);
                        if !refs.is_empty() {
                            println!("\nREFERENCES:");
                            for line in refs.lines().take(50) {
                                println!("  {}", line);
                            }
                        }
                    }
                }
            }
        }
        _ => {
            eprintln!("Unknown command: {}. Try nerv --help", cmd);
            std::process::exit(1);
        }
    }
}
