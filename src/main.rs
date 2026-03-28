use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use nerv::agent::EffortLevel;
use nerv::core::*;
use nerv::{nerv_dir};
use nerv::interactive::event_loop::InteractiveMode;
use nerv::interactive::footer::FooterComponent;
use nerv::interactive::layout::AppLayout;
use nerv::interactive::statusbar::StatusBar;
use nerv::tui::components::editor::Editor;
use nerv::tui::*;

/// Global cancel flag for print mode — SIGINT sets this instead of killing the process.
static PRINT_CANCEL: OnceLock<Arc<AtomicBool>> = OnceLock::new();

extern "C" fn handle_sigint_print(_: libc::c_int) {
    if let Some(cancel) = PRINT_CANCEL.get() {
        cancel.store(true, Ordering::Relaxed);
    }
}

// ---------------------------------------------------------------------------
// CLI argument model
// ---------------------------------------------------------------------------

enum Cmd {
    /// Interactive TUI session (default)
    Interactive {
        model: Option<String>,
        resume: ResumeOpt,
        log_level: Option<String>,
    },
    /// Interactive TUI session inside a fresh git worktree
    Wt {
        branch: String,
        model: Option<String>,
        log_level: Option<String>,
    },
    /// Headless: read prompt from stdin, stream JSON to stdout
    Print {
        model: Option<String>,
        max_turns: u32,
        verbose: bool,
    },
    /// Open session picker (no id) or load a specific session, then drop into TUI
    Resume { id: Option<String> },
    /// Pure-chat mode: no tools, no project context, plain conversational assistant
    Talk {
        model: Option<String>,
        log_level: Option<String>,
    },
    // --- one-shot subcommands ---
    Models,
    Export { id: String },
    Add { rest: Vec<String> },
    Load { rest: Vec<String> },
    Unload,
    Codemap { rest: Vec<String> },
    Symbols { rest: Vec<String> },
    Version,
}

enum ResumeOpt {
    None,
    Picker,
    Session(String),
}

fn print_top_help() {
    println!("nerv — coding agent for the terminal");
    println!();
    println!("Usage: nerv [options]");
    println!("       nerv <command> [args]");
    println!();
    println!("Options:");
    println!("  --model <name>     Select model");
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
fn parse_args() -> Cmd {
    use lexopt::prelude::*;

    let mut parser = lexopt::Parser::from_env();

    // Peek at the first positional to route subcommands.
    // lexopt doesn't have lookahead, so we collect args into a command.
    let mut model: Option<String> = None;
    let resume = ResumeOpt::None;
    let mut log_level: Option<String> = None;
    let _wt: Option<String> = None;

    // First token determines which branch we're in.
    let first = match parser.next() {
        Ok(Some(arg)) => arg,
        Ok(None) => {
            // No args — plain interactive mode.
            return Cmd::Interactive { model: None, resume: ResumeOpt::None, log_level: None };
        }
        Err(e) => {
            eprintln!("error: {e}. Try: nerv --help");
            std::process::exit(1);
        }
    };

    match first {
        // ── top-level flags (interactive mode) ──────────────────────────────
        Short('h') | Long("help") => {
            print_top_help();
            std::process::exit(0);
        }
        Long("version") => return Cmd::Version,
        Long("model") => {
            model = Some(parser.value().unwrap_or_else(|_| {
                eprintln!("--model requires a value");
                std::process::exit(1);
            }).string().unwrap());
        }
        Long("log-level") => {
            log_level = Some(parser.value().unwrap_or_else(|_| {
                eprintln!("--log-level requires a value");
                std::process::exit(1);
            }).string().unwrap());
        }
        // ── subcommands ──────────────────────────────────────────────────────
        Value(v) if v == "talk" => {
            // talk [-h] [--model M] [--log-level L]
            let mut talk_model: Option<String> = None;
            let mut talk_log: Option<String> = None;
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
                        println!("  -h, --help          Show this help");
                        std::process::exit(0);
                    }
                    Ok(Some(Long("model"))) => {
                        talk_model = Some(parser.value().unwrap_or_else(|_| {
                            eprintln!("--model requires a value");
                            std::process::exit(1);
                        }).string().unwrap());
                    }
                    Ok(Some(Long("log-level"))) => {
                        talk_log = Some(parser.value().unwrap_or_else(|_| {
                            eprintln!("--log-level requires a value");
                            std::process::exit(1);
                        }).string().unwrap());
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
            return Cmd::Talk { model: talk_model, log_level: talk_log };
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
                Ok(Some(Value(id))) => return Cmd::Resume { id: Some(id.string().unwrap()) },
                Ok(None) => return Cmd::Resume { id: None },
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
                        println!("Headless mode: read prompt from stdin, run agent, output JSON to stdout.");
                        println!();
                        println!("Options:");
                        println!("  --model <name>      Model to use (e.g. opus, sonnet)");
                        println!("  --max-turns <n>     Max agent turns (default: 20)");
                        println!("  --verbose           Stream tool progress to stderr");
                        println!("  -h, --help          Show this help");
                        std::process::exit(0);
                    }
                    Ok(Some(Long("model"))) => {
                        p_model = Some(parser.value().unwrap_or_else(|_| {
                            eprintln!("--model requires a value");
                            std::process::exit(1);
                        }).string().unwrap());
                    }
                    Ok(Some(Long("max-turns"))) => {
                        let s = parser.value().unwrap_or_else(|_| {
                            eprintln!("--max-turns requires a value");
                            std::process::exit(1);
                        }).string().unwrap();
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
            return Cmd::Print { model: p_model, max_turns, verbose };
        }
        Value(v) if v == "wt" => {
            // wt [-h] <branch> [--model M] [--log-level L]
            let mut wt_model: Option<String> = None;
            let mut wt_log: Option<String> = None;
            let mut branch: Option<String> = None;
            loop {
                match parser.next() {
                    Ok(None) => break,
                    Ok(Some(Short('h') | Long("help"))) => {
                        println!("Usage: nerv wt <branch> [options]");
                        println!();
                        println!("Start an interactive session in a fresh git worktree on <branch>.");
                        println!("The worktree is created under ~/.nerv/worktrees/ and checked out");
                        println!("to a new branch. Merged and cleaned up when the session ends.");
                        println!();
                        println!("Arguments:");
                        println!("  branch   Name of the new git branch to create");
                        println!();
                        println!("Options:");
                        println!("  --model <name>      Model to use");
                        println!("  --log-level <lvl>   Log level (debug, info, warn, error)");
                        println!("  -h, --help          Show this help");
                        std::process::exit(0);
                    }
                    Ok(Some(Long("model"))) => {
                        wt_model = Some(parser.value().unwrap_or_else(|_| {
                            eprintln!("--model requires a value");
                            std::process::exit(1);
                        }).string().unwrap());
                    }
                    Ok(Some(Long("log-level"))) => {
                        wt_log = Some(parser.value().unwrap_or_else(|_| {
                            eprintln!("--log-level requires a value");
                            std::process::exit(1);
                        }).string().unwrap());
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
            return Cmd::Wt { branch, model: wt_model, log_level: wt_log };
        }
        Value(v) if v == "models" => {
            if matches!(parser.next(), Ok(Some(Short('h') | Long("help")))) {
                println!("Usage: nerv models");
                println!();
                println!("List all configured models and their online status.");
                std::process::exit(0);
            }
            return Cmd::Models;
        }
        Value(v) if v == "export" => {
            match parser.next() {
                Ok(Some(Short('h') | Long("help"))) => {
                    println!("Usage: nerv export <session-id>");
                    println!();
                    println!("Export a session to HTML and JSONL in ~/.nerv/exports/.");
                    std::process::exit(0);
                }
                Ok(Some(Value(id))) => return Cmd::Export { id: id.string().unwrap() },
                _ => {
                    eprintln!("Usage: nerv export <session-id>");
                    std::process::exit(1);
                }
            }
        }
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
            return Cmd::Add { rest: remaining_strings(parser) };
        }
        Value(v) if v == "load" => {
            if matches!(parser.clone().next(), Ok(Some(Short('h') | Long("help")))) {
                println!("Usage: nerv load [alias]");
                println!();
                println!("Start llama-server for a local GGUF model.");
                println!("Replaces this process via exec — Ctrl+C to stop.");
                std::process::exit(0);
            }
            return Cmd::Load { rest: remaining_strings(parser) };
        }
        Value(v) if v == "unload" => {
            if matches!(parser.next(), Ok(Some(Short('h') | Long("help")))) {
                println!("Usage: nerv unload");
                println!();
                println!("Stop the running llama-server (Ctrl+C the process directly).");
                std::process::exit(0);
            }
            return Cmd::Unload;
        }
        Value(v) if v == "codemap" => {
            if matches!(parser.clone().next(), Ok(Some(Short('h') | Long("help")))) {
                println!("Usage: nerv codemap <query> [path] [--kind <kind>] [--depth full|signatures]");
                println!();
                println!("Show symbol implementations matching a query.");
                println!();
                println!("Options:");
                println!("  --kind <kind>          Filter by symbol kind");
                println!("  --depth full|signatures  Output verbosity (default: full)");
                std::process::exit(0);
            }
            return Cmd::Codemap { rest: remaining_strings(parser) };
        }
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
            return Cmd::Symbols { rest: remaining_strings(parser) };
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

    // Remaining args for interactive mode (--model / --log-level / --wt may follow).
    loop {
        match parser.next() {
            Ok(None) => break,
            Ok(Some(Long("model"))) => {
                model = Some(parser.value().unwrap_or_else(|_| {
                    eprintln!("--model requires a value");
                    std::process::exit(1);
                }).string().unwrap());
            }
            Ok(Some(Long("log-level"))) => {
                log_level = Some(parser.value().unwrap_or_else(|_| {
                    eprintln!("--log-level requires a value");
                    std::process::exit(1);
                }).string().unwrap());
            }
            Ok(Some(Short('h') | Long("help"))) => {
                print_top_help();
                std::process::exit(0);
            }
            Ok(Some(Long("version"))) => {
                println!("nerv {}", env!("CARGO_PKG_VERSION"));
                std::process::exit(0);
            }
            Ok(Some(arg)) => {
                eprintln!("unexpected argument. Try: nerv --help");
                eprintln!("  {}", arg.unexpected());
                std::process::exit(1);
            }
            Err(e) => {
                eprintln!("error: {e}. Try: nerv --help");
                std::process::exit(1);
            }
        }
    }

    Cmd::Interactive { model, resume, log_level }
}

/// Drain remaining positional values from a lexopt parser into a Vec<String>.
fn remaining_strings(mut parser: lexopt::Parser) -> Vec<String> {
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
enum RepoGateResult {
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
/// - Git repo, never seen before  →  print prompt, read single keypress: [c]ontinue / [t]alk
/// - Git repo, already in the DB  →  return Continue immediately
///
/// Uses raw terminal I/O directly so we don't need to spin up the full TUI.
fn repo_gate(cwd: &std::path::Path, nerv_dir: &std::path::Path) -> RepoGateResult {
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
        format!("previously unknown repository: {dir}\r\n  \x1b[2m[c]\x1b[0mcontinue  \x1b[2m[t]\x1b[0mtalk\r\n")
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

fn main() {
    let cmd = parse_args();

    match cmd {
        Cmd::Version => {
            println!("nerv {}", env!("CARGO_PKG_VERSION"));
            return;
        }
        Cmd::Print { model, max_turns, verbose } => {
            print_mode(model.as_deref(), max_turns, verbose);
            return;
        }
        Cmd::Models => {
            let nerv_dir = nerv_dir();
            std::fs::create_dir_all(nerv_dir).ok();
            handle_subcommand("models", &[], nerv_dir);
            return;
        }
        Cmd::Export { id } => {
            let nerv_dir = nerv_dir();
            std::fs::create_dir_all(nerv_dir).ok();
            handle_subcommand("export", &[id], nerv_dir);
            return;
        }
        Cmd::Add { rest } => {
            let nerv_dir = nerv_dir();
            std::fs::create_dir_all(nerv_dir).ok();
            handle_subcommand("add", &rest, nerv_dir);
            return;
        }
        Cmd::Load { rest } => {
            let nerv_dir = nerv_dir();
            std::fs::create_dir_all(nerv_dir).ok();
            handle_subcommand("load", &rest, nerv_dir);
            return;
        }
        Cmd::Unload => {
            let nerv_dir = nerv_dir();
            std::fs::create_dir_all(nerv_dir).ok();
            handle_subcommand("unload", &[], nerv_dir);
            return;
        }
        Cmd::Codemap { rest } => {
            let nerv_dir = nerv_dir();
            std::fs::create_dir_all(nerv_dir).ok();
            handle_subcommand("codemap", &rest, nerv_dir);
            return;
        }
        Cmd::Symbols { rest } => {
            let nerv_dir = nerv_dir();
            std::fs::create_dir_all(nerv_dir).ok();
            handle_subcommand("symbols", &rest, nerv_dir);
            return;
        }
        Cmd::Resume { .. } | Cmd::Interactive { .. } | Cmd::Wt { .. } | Cmd::Talk { .. } => {
            // Fall through to TUI startup below.
        }
    }

    // ── TUI startup ─────────────────────────────────────────────────────────

    // Re-destructure for the TUI-launching variants.
    let (opt_model, resume_opt, log_level_opt, wt_opt, mut talk_mode) = match cmd {
        Cmd::Interactive { model, resume, log_level } => (model, resume, log_level, None, false),
        Cmd::Wt { branch, model, log_level } => (model, ResumeOpt::None, log_level, Some(branch), false),
        Cmd::Resume { id } => {
            let resume = match id {
                Some(id) => ResumeOpt::Session(id),
                None    => ResumeOpt::Picker,
            };
            (None, resume, None, None, false)
        }
        Cmd::Talk { model, log_level } => (model, ResumeOpt::None, log_level, None, true),
        _ => unreachable!(),
    };

    let nerv_dir = nerv_dir();
    std::fs::create_dir_all(nerv_dir).ok();

    nerv::log::init(&nerv_dir.join("debug.log"));
    if let Some(ref lvl) = log_level_opt {
        if let Ok(level) = lvl.parse() {
            nerv::log::set_level(level);
        }
    } else if let Ok(level) = std::env::var("NERV_LOG").unwrap_or_default().parse() {
        nerv::log::set_level(level);
    }
    nerv::log::info("nerv starting");

    let mut cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let mut worktree_path: Option<PathBuf> = None;

    if let Some(branch) = wt_opt {
        let repo_root = nerv::find_repo_root(&cwd).unwrap_or_else(|| {
            eprintln!("--wt requires a git repository");
            std::process::exit(1);
        });
        let prefix = &nerv::session::types::gen_session_id()[..8];
        match nerv::worktree::create_worktree(&repo_root, &nerv_dir, &branch, prefix) {
            Ok(wt) => {
                cwd = wt.clone();
                worktree_path = Some(wt);
            }
            Err(e) => {
                eprintln!("Failed to create worktree: {}", e);
                std::process::exit(1);
            }
        }
    }

    // ── Repository gate ──────────────────────────────────────────────────────
    // In normal (non-talk) mode, check whether we know this repository before
    // spending time on indexing and bootstrap.  Two cases:
    //
    //   1. Not a git repo at all  →  [e]xit  or  [t]alk
    //   2. Git repo but never seen before  →  [c]ontinue  or  [t]alk
    //
    // Known repos, explicit talk mode, and explicit resume commands skip the gate.
    let is_resume = matches!(resume_opt, ResumeOpt::Session(_) | ResumeOpt::Picker);
    if !talk_mode && !is_resume {
        match repo_gate(&cwd, nerv_dir) {
            RepoGateResult::Continue => {}
            RepoGateResult::Talk => {
                talk_mode = true;
            }
            RepoGateResult::Exit => {
                std::process::exit(0);
            }
        }
    }

    let b = nerv::bootstrap::bootstrap(
        &cwd,
        &nerv_dir,
        nerv::bootstrap::BootstrapOptions {
            memory: true,
            permissions: true,
            talk_mode,
        },
    );
    let config = b.config;
    let model_registry = b.model_registry;
    let cancel_flag = b.cancel_flag;
    let skills = b.resources.skills.clone();

    // Token cost breakdown for startup display
    let loaded_files: Vec<(String, usize)> = b
        .resources
        .context_files
        .iter()
        .map(|cf| {
            let tokens = nerv::compaction::count_tokens(&cf.content);
            (cf.path.display().to_string(), tokens)
        })
        .collect();
    let system_prompt_tokens = b
        .resources
        .system_prompt
        .as_ref()
        .map(|sp| nerv::compaction::count_tokens(sp));
    let memory_tokens = b
        .resources
        .memory
        .as_ref()
        .map(|m| nerv::compaction::count_tokens(m))
        .filter(|&t| t > 0);
    let append_prompt_tokens: Vec<usize> = b
        .resources
        .append_prompts
        .iter()
        .map(|ap| nerv::compaction::count_tokens(ap))
        .collect();
    let base_prompt_tokens =
        nerv::compaction::count_tokens(nerv::core::system_prompt::DEFAULT_SYSTEM_PROMPT);
    let tools_tokens = {
        let tools = b.session.tool_registry.active_tools();
        let mut tool_text = String::new();
        for t in &tools {
            tool_text.push_str(t.name());
            tool_text.push(' ');
            tool_text.push_str(t.description());
            tool_text.push_str(&t.parameters_schema().to_string());
        }
        nerv::compaction::count_tokens(&tool_text)
    };

    let mut session = b.session;
    if let Some(ref wt) = worktree_path {
        session.set_worktree(wt.clone());
    }

    // Channels (crossbeam)
    let (cmd_tx, cmd_rx) = crossbeam_channel::bounded::<SessionCommand>(32);
    let (event_tx, event_rx) = crossbeam_channel::bounded::<AgentSessionEvent>(256);

    // Capture initial state before session is moved to its thread
    let initial_thinking_level = session.agent.state.thinking_level;
    let initial_effort_level = session.agent.state.effort_level;

    // Session thread
    let evt_tx = event_tx.clone();
    std::thread::spawn(move || session_task(cmd_rx, evt_tx, session));

    // Build layout + TUI
    let terminal = ProcessTerminal::new();
    let mut tui = TUI::new(Box::new(terminal));

    let cwd_str = cwd.to_string_lossy().to_string();
    let mut footer = FooterComponent::new(&cwd_str);
    if let Some(m) = model_registry.default_model(&config) {
        footer.set_model(m);
    }
    footer.set_thinking(initial_thinking_level);
    footer.set_effort(initial_effort_level);

    let mut layout = AppLayout::new(Editor::new(), StatusBar::new(), footer);
    tui.fixed_bottom = nerv::interactive::layout::BASE_FIXED_BOTTOM; // editor + statusbar + footer — never flushed to scrollback

    let dim = nerv::interactive::theme::DIM;
    if !talk_mode {
        let load = |chat: &mut nerv::interactive::chat_writer::ChatWriter, name: &str, tok: usize| {
            chat.push_styled(dim, &format!("› Loading: {} ({} tok)", name, tok));
        };
        load(&mut layout.chat, "base prompt", base_prompt_tokens);
        load(&mut layout.chat, "tools", tools_tokens);
        for (path, tokens) in &loaded_files {
            load(&mut layout.chat, path, *tokens);
        }
        if let Some(tok) = system_prompt_tokens {
            load(&mut layout.chat, "system-prompt.md", tok);
        }
        if let Some(tok) = memory_tokens {
            load(&mut layout.chat, "memory.md", tok);
        }
        for (i, tok) in append_prompt_tokens.iter().enumerate() {
            load(&mut layout.chat, &format!("append prompt {}", i + 1), *tok);
        }
    }

    layout.chat.push_styled(
        nerv::interactive::theme::WARN,
        if talk_mode {
            "Talk mode. No tools or project context."
        } else {
            "NERV console ready. Awaiting your command."
        },
    );

    // Emit config warnings (unknown model ids, etc.)
    for warning in &b.config_warnings {
        layout.chat.push_styled(nerv::interactive::theme::WARN, &format!("⚠  {}", warning));
    }

    // Warn if required external tools are missing.
    for tool in &["rg", "fd"] {
        if std::process::Command::new(tool).arg("--version").output().is_err() {
            layout.chat.push_styled(
                nerv::interactive::theme::WARN,
                &format!("⚠  `{}` not found — install it for full functionality.", tool),
            );
        }
    }

    tui.terminal_mut().start();
    tui.request_render(true); // initial render
    tui.maybe_render(&layout);

    let repo_root_path = nerv::find_repo_root(&cwd);
    let repo_id = repo_root_path.as_deref().and_then(nerv::repo_fingerprint);
    let repo_root = repo_root_path.map(|p| p.to_string_lossy().to_string());
    let mut interactive = InteractiveMode::new(
        cmd_tx,
        model_registry.clone(),
        model_registry.default_model(&config).cloned(),
        initial_thinking_level,
        initial_effort_level,
        skills,
        repo_root,
        repo_id,
    );

    layout
        .editor
        .set_completions(interactive.slash_completions());

    // Set default model on the agent via command
    if let Some(m) = model_registry.default_model(&config) {
        let _ = interactive.cmd_tx().try_send(SessionCommand::SetModel {
            provider: m.provider_name.clone(),
            model_id: m.id.clone(),
        });
    }

    // Apply --model flag (interactive mode)
    if let Some(ref name) = opt_model {
        let found = if let Some((p, m)) = name.split_once('/') {
            model_registry.get_model(p, m)
        } else {
            model_registry.find_model(name)
        };
        if let Some(m) = found {
            let _ = interactive.cmd_tx().send(SessionCommand::SetModel {
                provider: m.provider_name.clone(),
                model_id: m.id.clone(),
            });
        } else {
            eprintln!("Unknown model: {}", name);
            std::process::exit(1);
        }
    }

    // Handle resume subcommand / flag
    match resume_opt {
        ResumeOpt::Session(id) => {
            let _ = interactive
                .cmd_tx()
                .send(SessionCommand::LoadSession { id });
        }
        ResumeOpt::Picker => {
            let _ = interactive.cmd_tx().send(SessionCommand::ListSessions {
                repo_root: interactive.repo_root(),
                repo_id: interactive.repo_id(),
            });
        }
        ResumeOpt::None => {}
    }

    // Health check custom providers (non-blocking, retries until online)
    for provider_cfg in &config.custom_providers {
        let name = provider_cfg.name.clone();
        let url = format!("{}/models", provider_cfg.base_url);
        let tx = event_tx.clone();
        std::thread::spawn(move || {
            let agent = ureq::Agent::config_builder()
                .timeout_global(Some(std::time::Duration::from_secs(2)))
                .build()
                .new_agent();
            loop {
                let online = agent.get(&url).call().is_ok();
                let _ = tx.send(AgentSessionEvent::ProviderHealth {
                    provider: name.clone(),
                    online,
                });
                if online {
                    break;
                }
                std::thread::sleep(Duration::from_secs(3));
            }
        });
    }

    // Stdin reader thread — uses poll() so it can be paused for $EDITOR
    let stdin_paused = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let stdin_paused2 = stdin_paused.clone();
    let (stdin_tx, stdin_rx) = crossbeam_channel::bounded::<Vec<u8>>(64);
    std::thread::spawn(move || {
        use std::io::Read;
        let mut buf = [0u8; 1024];
        let mut pfd = libc::pollfd {
            fd: libc::STDIN_FILENO,
            events: libc::POLLIN,
            revents: 0,
        };
        loop {
            // When paused, spin-wait with a sleep instead of reading stdin
            if stdin_paused2.load(std::sync::atomic::Ordering::Relaxed) {
                std::thread::sleep(std::time::Duration::from_millis(50));
                continue;
            }
            // Poll with 100ms timeout so we can check the pause flag
            let ready = unsafe { libc::poll(&mut pfd, 1, 100) };
            if ready <= 0 {
                continue; // timeout or error — recheck flags
            }
            match std::io::stdin().read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    if stdin_tx.send(buf[..n].to_vec()).is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    });

    // Register signals
    let sigint_flag = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let _ = signal_hook::flag::register(signal_hook::consts::SIGINT, sigint_flag.clone());
    let sigwinch_flag = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let _ = signal_hook::flag::register(signal_hook::consts::SIGWINCH, sigwinch_flag.clone());

    let mut stdin_buf = StdinBuffer::new();
    let tick_interval = Duration::from_millis(100);

    let mut should_quit = false;

    // Main event loop — polling with crossbeam select + timeout
    loop {
        // Check SIGINT — second ^C while already cancelling force-quits
        if sigint_flag.swap(false, std::sync::atomic::Ordering::Relaxed) {
            if cancel_flag.load(std::sync::atomic::Ordering::Relaxed) || !interactive.is_streaming {
                tui.terminal_mut().stop();
                break;
            }
            cancel_flag.store(true, std::sync::atomic::Ordering::Relaxed);
        }

        if should_quit {
            break;
        }

        // Poll all sources with a short timeout
        crossbeam_channel::select! {
            recv(stdin_rx) -> msg => {
                let Ok(bytes) = msg else { break };

                let events = stdin_buf.process(&bytes);
                for event in events {
                    match event {
                        StdinEvent::Sequence(ref seq) => {
                            // Permission prompt input (y/n)
                            if interactive.pending_permission.is_some() {
                                let approved = if seq == b"y" || seq == b"Y" {
                                    Some(true)
                                } else if seq == b"n" || seq == b"N"
                                    || keys::matches_key(seq, "escape")
                                    || keys::matches_key(seq, "ctrl+c")
                                    || keys::matches_key(seq, "enter")
                                {
                                    Some(false)
                                } else {
                                    None
                                };
                                if let Some(approved) = approved {
                                    if let Some(tx) = interactive.pending_permission.take() {
                                        let _ = tx.send(approved);
                                    }
                                    interactive.pending_permission_details = None;
                                    interactive.status_message = None;
                                    interactive.status_is_error = false;
                                    let label = if approved { "allowed" } else { "denied" };
                                    let style = if approved {
                                        nerv::interactive::theme::SUCCESS
                                    } else {
                                        nerv::interactive::theme::ERROR
                                    };
                                    layout.chat.push_styled(style, &format!("  → {}", label));
                                    tui.request_render(false); tui.maybe_render(&layout);
                                }
                                continue;
                            }

                            if keys::matches_key(seq, "ctrl+c") {
                                if cancel_flag.load(std::sync::atomic::Ordering::Relaxed) || !interactive.is_streaming {
                                    tui.terminal_mut().stop();
                                    should_quit = true; break;
                                }
                                cancel_flag.store(true, std::sync::atomic::Ordering::Relaxed);
                                interactive.handle_abort();
                                layout.statusbar.cancel_streaming();
                                tui.request_render(false); tui.maybe_render(&layout);
                                continue;
                            }
                            if keys::matches_key(seq, "escape") || keys::matches_key(seq, "ctrl+d") {
                                cancel_flag.store(true, std::sync::atomic::Ordering::Relaxed);
                                tui.terminal_mut().stop();
                                should_quit = true; break;
                            }
                            if keys::matches_key(seq, "ctrl+z") {
                                tui.suspend();
                                unsafe { libc::raise(libc::SIGSTOP) };
                                tui.resume(); tui.maybe_render(&layout); continue;
                            }
                            if keys::matches_key(seq, "shift+tab") {
                                let enabled = interactive.toggle_plan_mode();
                                let label = if enabled { "Plan mode on" } else { "Plan mode off" };
                                push_status(&mut layout, label, false);
                                interactive.refresh_footer(&mut layout.footer);
                                tui.request_render(false); tui.maybe_render(&layout); continue;
                            }
                            if keys::matches_key(seq, "ctrl+s") {
                                if interactive.session_id.is_some() {
                                    let _ = interactive.cmd_tx().try_send(SessionCommand::GetTree);
                                } else {
                                    push_status(&mut layout, "No active session.", false);
                                    tui.request_render(false); tui.maybe_render(&layout);
                                }
                                continue;
                            }
                            if keys::matches_key(seq, "ctrl+t") {
                                let next = interactive.cycle_thinking();
                                let _ = interactive.cmd_tx().try_send(SessionCommand::SetThinkingLevel { level: next });
                                interactive.refresh_footer(&mut layout.footer);
                                tui.request_render(false); tui.maybe_render(&layout); continue;
                            }
                            if keys::matches_key(seq, "ctrl+e") {
                                let next = interactive.cycle_effort();
                                let _ = interactive.cmd_tx().try_send(SessionCommand::SetEffortLevel { level: next });
                                let label = match next {
                                    None => "Effort: off".into(),
                                    Some(e) => format!("Effort: {}", match e {
                                        EffortLevel::Low => "low",
                                        EffortLevel::Medium => "medium",
                                        EffortLevel::High => "high",
                                        EffortLevel::Max => "max",
                                    }),
                                };
                                push_status(&mut layout, &label, false);
                                interactive.refresh_footer(&mut layout.footer);
                                tui.request_render(false); tui.maybe_render(&layout); continue;
                            }
                            if keys::matches_key(seq, "ctrl+g") {
                                stdin_paused.store(true, std::sync::atomic::Ordering::SeqCst);
                                std::thread::sleep(std::time::Duration::from_millis(60));
                                tui.terminal_mut().stop();
                                layout.editor.open_in_external_editor();
                                tui.terminal_mut().restart();
                                stdin_paused.store(false, std::sync::atomic::Ordering::SeqCst);
                                tui.request_render(true); tui.maybe_render(&layout); continue;
                            }
                            if keys::matches_key(seq, "shift+enter") || keys::matches_key(seq, "ctrl+enter") || keys::matches_key(seq, "newline") {
                                layout.editor.handle_input(b"\n");
                                tui.request_render(false); continue;
                            }
                            if keys::matches_key(seq, "enter") {
                                let text = layout.editor.take_text();
                                if !text.is_empty() {
                                    let req = interactive.handle_submit(text);
                                    if let Some(req) = req {
                                        launch_picker(req, &mut interactive, &mut layout, &stdin_paused);
                                        tui.request_render(true); tui.maybe_render(&layout); continue;
                                    }
                                    if interactive.quit_requested { tui.terminal_mut().stop(); should_quit = true; break; }
                                    interactive.refresh_footer(&mut layout.footer);
                                    if let Some(msg) = interactive.status_message.take() {
                                        push_status(&mut layout, &msg, interactive.status_is_error);
                                    }
                                    layout.statusbar.set_queue(&interactive.pending_messages, interactive.editing_queue_idx);
                                    layout.statusbar.render_queue(tui.width());
                                    tui.fixed_bottom = nerv::interactive::layout::BASE_FIXED_BOTTOM + layout.statusbar.queue_line_count();
                                }
                                tui.request_render(false); continue;
                            }
                            // Queue navigation (while streaming)
                            if keys::matches_key(seq, "up") && interactive.is_streaming && !interactive.pending_messages.is_empty() {
                                if let Some(idx) = interactive.editing_queue_idx {
                                    let current = layout.editor.text().to_string();
                                    if !current.is_empty() { interactive.pending_messages[idx] = current; }
                                }
                                if let Some(text) = interactive.edit_queue_up() {
                                    layout.editor.set_text(&text);
                                    layout.statusbar.set_queue(&interactive.pending_messages, interactive.editing_queue_idx);
                                    layout.statusbar.render_queue(tui.width());
                                    tui.fixed_bottom = nerv::interactive::layout::BASE_FIXED_BOTTOM + layout.statusbar.queue_line_count();
                                    tui.request_render(false); tui.maybe_render(&layout); continue;
                                }
                            }
                            // Up-arrow when not streaming but there are queued messages: dequeue last into editor
                            if keys::matches_key(seq, "up") && !interactive.is_streaming
                                && !interactive.pending_messages.is_empty()
                            {
                                let msg = interactive.pending_messages.pop().unwrap();
                                layout.editor.set_text(&msg);
                                layout.statusbar.set_queue(&interactive.pending_messages, None);
                                layout.statusbar.render_queue(tui.width());
                                tui.fixed_bottom = nerv::interactive::layout::BASE_FIXED_BOTTOM + layout.statusbar.queue_line_count();
                                tui.request_render(false); tui.maybe_render(&layout); continue;
                            }
                            if keys::matches_key(seq, "down") && interactive.editing_queue_idx.is_some() {
                                if let Some(idx) = interactive.editing_queue_idx {
                                    let current = layout.editor.text().to_string();
                                    if !current.is_empty() { interactive.pending_messages[idx] = current; }
                                }
                                if let Some(text) = interactive.edit_queue_down() {
                                    layout.editor.set_text(&text);
                                    layout.statusbar.set_queue(&interactive.pending_messages, interactive.editing_queue_idx);
                                    layout.statusbar.render_queue(tui.width());
                                    tui.fixed_bottom = nerv::interactive::layout::BASE_FIXED_BOTTOM + layout.statusbar.queue_line_count();
                                    tui.request_render(false); tui.maybe_render(&layout); continue;
                                }
                            }
                            if keys::matches_key(seq, "backspace") && interactive.editing_queue_idx.is_some()
                                && layout.editor.is_empty()
                            {
                                interactive.remove_editing_queue_item();
                                layout.editor.clear();
                                layout.statusbar.set_queue(&interactive.pending_messages, interactive.editing_queue_idx);
                                layout.statusbar.render_queue(tui.width());
                                tui.fixed_bottom = nerv::interactive::layout::BASE_FIXED_BOTTOM + layout.statusbar.queue_line_count();
                                tui.request_render(false); tui.maybe_render(&layout); continue;
                            }
                            // History navigation: up/down when not streaming and editor is empty
                            if keys::matches_key(seq, "up") && !interactive.is_streaming
                                && layout.editor.is_empty()
                            {
                                let current = layout.editor.text();
                                if let Some(text) = interactive.history_up(&current) {
                                    layout.editor.set_text(&text);
                                    tui.request_render(false); tui.maybe_render(&layout); continue;
                                }
                            }
                            if keys::matches_key(seq, "down") && !interactive.is_streaming
                                && interactive.history_index.is_some()
                                && let Some(text) = interactive.history_down()
                            {
                                layout.editor.set_text(&text);
                                tui.request_render(false); tui.maybe_render(&layout); continue;
                            }

                            layout.editor.handle_input(seq);
                        }
                        StdinEvent::Paste(text) => {
                            layout.editor.insert_paste(&text);
                        }
                    }
                }
                tui.request_render(false); tui.maybe_render(&layout);
            }
            recv(event_rx) -> msg => {
                let Ok(event) = msg else { break };
                // Clear cancel flag when streaming ends so next ^C works normally
                if matches!(event, nerv::core::AgentSessionEvent::Agent(nerv::agent::types::AgentEvent::AgentEnd { .. })) {
                    cancel_flag.store(false, std::sync::atomic::Ordering::Relaxed);
                }
                process_event(event, &mut interactive, &mut layout, &mut tui, &stdin_paused);
                tui.maybe_render(&layout);
            }
            default(tick_interval) => {
                // SIGWINCH — terminal resized
                if sigwinch_flag.swap(false, std::sync::atomic::Ordering::Relaxed) {
                    tui.request_render(true); // full redraw on resize
                }
                if interactive.is_streaming {
                    layout.statusbar.tick();
                    tui.request_render(false);
                }
                if interactive.is_compacting {
                    layout.footer.tick();
                    tui.request_render(false);
                }
                tui.maybe_render(&layout);
            }
        }
    }

    // Grace period for session flush
    std::thread::sleep(Duration::from_millis(200));

    if let Some(id) = &interactive.session_id {
        let short = if id.len() > 8 { &id[..8] } else { id };
        // Print below the shell prompt line — println scrolls the terminal
        println!("To resume this session: nerv resume {}", short);
    }
}

fn process_event(
    event: nerv::core::AgentSessionEvent,
    interactive: &mut InteractiveMode,
    layout: &mut AppLayout,
    tui: &mut TUI,
    stdin_paused: &Arc<std::sync::atomic::AtomicBool>,
) {
    if let Some(req) = interactive.handle_event(event, layout, tui) {
        launch_picker(req, interactive, layout, stdin_paused);
        tui.request_render(true);
        return;
    }
    if let Some(msg) = interactive.status_message.take() {
        push_status(layout, &msg, interactive.status_is_error);
        tui.request_render(false);
    }
}

/// Pause the stdin reader thread, run the fullscreen picker, then resume.
/// Acts on the result (load session, switch branch) immediately.
fn launch_picker(
    req: nerv::interactive::event_loop::PickerRequest,
    interactive: &mut InteractiveMode,
    layout: &mut AppLayout,
    stdin_paused: &Arc<std::sync::atomic::AtomicBool>,
) {
    use nerv::interactive::fullscreen_picker::run_fullscreen_picker;
    use nerv::interactive::event_loop::PickerRequest;
    use nerv::interactive::tree_selector::TreeSelection;

    // Pause the stdin reader so the picker owns stdin bytes exclusively.
    // Wait longer than the poll(100ms) timeout so the thread quiesces.
    stdin_paused.store(true, std::sync::atomic::Ordering::SeqCst);
    std::thread::sleep(std::time::Duration::from_millis(150));

    enum PickResult {
        Session(String),
        Tree(TreeSelection),
        Model(String),
        None,
    }

    let result = match req {
        PickerRequest::SessionPicker { sessions, repo_root } => {
            let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
            let repo_dir = nerv::repo_data_dir(&cwd);
            let search_fn: Box<dyn Fn(&str) -> Vec<nerv::session::manager::SearchResult>> = {
                Box::new(move |q: &str| {
                    let mgr = nerv::session::manager::SessionManager::new(&repo_dir);
                    mgr.search_sessions(q)
                })
            };
            let mut picker = nerv::interactive::session_picker::SessionPicker::new(
                sessions, search_fn, repo_root,
            );
            run_fullscreen_picker(&mut picker)
                .map(PickResult::Session)
                .unwrap_or(PickResult::None)
        }
        PickerRequest::TreeSelector { tree, current_leaf } => {
            let mut selector = nerv::interactive::tree_selector::TreeSelector::new(
                tree, current_leaf,
            );
            if run_fullscreen_picker(&mut selector).is_some() {
                selector.selected_node()
                    .map(PickResult::Tree)
                    .unwrap_or(PickResult::None)
            } else {
                PickResult::None
            }
        }
        PickerRequest::ModelPicker => {
            let models = interactive.model_registry().available_models()
                .into_iter().cloned().collect::<Vec<_>>();
            let current = interactive.model_name().to_owned();
            let mut picker = nerv::interactive::model_picker::ModelPicker::new(models, current);
            run_fullscreen_picker(&mut picker)
                .map(PickResult::Model)
                .unwrap_or(PickResult::None)
        }
    };

    stdin_paused.store(false, std::sync::atomic::Ordering::SeqCst);

    match result {
        PickResult::Session(id) => {
            let _ = interactive.cmd_tx().send(nerv::core::SessionCommand::LoadSession { id });
        }
        PickResult::Tree(sel) => {
            if sel.is_user {
                // User message selected: set leaf to parent so next prompt branches from there.
                // Place the message text in the editor for re-submission.
                let _ = interactive.cmd_tx().send(nerv::core::SessionCommand::SwitchBranch {
                    entry_id: sel.entry_id,
                    use_parent: !sel.is_root,
                    reset_leaf: sel.is_root,
                });
                if !sel.raw_text.is_empty() {
                    layout.editor.set_text(&sel.raw_text);
                }
            } else {
                // Non-user: set leaf directly to selected node, editor stays empty.
                let _ = interactive.cmd_tx().send(nerv::core::SessionCommand::SwitchBranch {
                    entry_id: sel.entry_id,
                    use_parent: false,
                    reset_leaf: false,
                });
            }
        }
        PickResult::Model(token) => {
            // token is "provider_name/model_id" encoded by ModelPicker
            if let Some((provider, model_id)) = token.split_once('/') {
                let _ = interactive.cmd_tx().send(nerv::core::SessionCommand::SetModel {
                    provider: provider.to_string(),
                    model_id: model_id.to_string(),
                });
            }
        }
        PickResult::None => {}
    }
}

fn push_status(layout: &mut AppLayout, msg: &str, is_error: bool) {
    let style = if is_error {
        nerv::interactive::theme::ERROR
    } else {
        nerv::interactive::theme::MUTED
    };
    layout.chat.push_styled(style, msg);
}

fn list_all_models() {
    let nerv_dir = nerv_dir();
    let config = nerv::core::NervConfig::load(nerv_dir);
    let mut auth = nerv::core::auth::AuthStorage::load(nerv_dir);
    let registry = nerv::core::model_registry::ModelRegistry::new(&config, &mut auth);

    let all = registry.all_models();

    if all.is_empty() {
        println!("No models configured. Run `nerv --login` or set ANTHROPIC_API_KEY.");
        return;
    }

    // Collect unique providers with a registered Arc<dyn Provider> and healthcheck each.
    // This is intentionally blocking — `nerv models` / `nerv --list-models` is a CLI command.
    use std::collections::HashMap;
    let mut provider_health: HashMap<String, bool> = HashMap::new();
    for m in &all {
        if provider_health.contains_key(&m.provider_name) {
            continue;
        }
        let online = if let Some(p) = registry.provider_registry.get(&m.provider_name) {
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
        println!(
            "    {} {:<30} ctx:{}  {}",
            marker, m.id, m.context_window, m.name
        );
    }
    println!();
}

fn handle_subcommand(cmd: &str, args: &[String], nerv_dir: &Path) {
    use nerv::core::local_models::*;

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

                    println!(
                        "Hardware: {:.0}GB RAM, {} cores",
                        sysctl_mem_gb(),
                        sysctl_cores(),
                    );
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
                if idx >= 1 && idx <= models.len() {
                    Some(models[idx - 1].clone())
                } else {
                    None
                }
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
                Err(e) => { eprintln!("HTML export failed: {}", e); std::process::exit(1); }
            }
            match nerv::export::export_session_jsonl(session_id, &jsonl_path, nerv_dir) {
                Ok(path) => println!("Exported to {}", path),
                Err(e) => { eprintln!("JSONL export failed: {}", e); std::process::exit(1); }
            }
        }
        "codemap" => {
            if args.is_empty() {
                eprintln!("Usage: nerv codemap <query> [path] [--kind <kind>] [--depth full|signatures]");
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
                    "--kind" => { kind_str = args.get(i + 1).map(|s| s.as_str()); i += 2; }
                    "--file" => { file_str = args.get(i + 1).map(|s| s.as_str()); i += 2; }
                    "--depth" => { depth_str = args.get(i + 1).map(|s| s.as_str()); i += 2; }
                    s if !s.starts_with('-') && file_str.is_none() => {
                        file_str = Some(s);
                        i += 1;
                    }
                    _ => { i += 1; }
                }
            }

            let kind = kind_str.and_then(nerv::index::codemap::parse_kind);
            let depth = depth_str
                .map(nerv::index::codemap::parse_depth)
                .unwrap_or(nerv::index::codemap::Depth::Full);
            let file_path = file_str.map(|f| {
                if f.starts_with('/') { PathBuf::from(f) } else { cwd.join(f) }
            });

            let mut index = nerv::index::SymbolIndex::new();
            index.force_index_dir(&cwd);

            let params = nerv::index::codemap::CodemapParams {
                query,
                kind,
                file: file_path.as_deref(),
                depth,
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
                    "--kind" => { kind_str = args.get(i + 1).map(|s| s.as_str()); i += 2; }
                    "--file" => { file_str = args.get(i + 1).map(|s| s.as_str()); i += 2; }
                    "--refs" => { want_refs = true; i += 1; }
                    s if !s.starts_with('-') && file_str.is_none() => {
                        file_str = Some(s);
                        i += 1;
                    }
                    _ => { i += 1; }
                }
            }

            let kind = kind_str.and_then(nerv::index::codemap::parse_kind);
            let file_path = file_str.map(|f| {
                if f.starts_with('/') { PathBuf::from(f) } else { cwd.join(f) }
            });

            let mut index = nerv::index::SymbolIndex::new();
            index.force_index_dir(&cwd);

            let results = index.search(query, kind, file_path.as_deref());
            if results.is_empty() {
                println!("No definitions found");
            } else {
                for sym in &results {
                    let rel = sym.file.strip_prefix(&cwd).unwrap_or(&sym.file).display();
                    let parent_suffix = sym.parent.as_ref()
                        .map(|p| format!("  ({})", p))
                        .unwrap_or_default();
                    println!(
                        "  {}:{:<4}  {} {}{}",
                        rel, sym.line, sym.kind.label(), sym.signature, parent_suffix,
                    );
                }
                println!("\n{} definitions", results.len());
            }

            if want_refs {
                let output = std::process::Command::new("rg")
                    .args(["--no-heading", "--line-number", "--color=never", "--word-regexp", query])
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
        _ => {
            eprintln!("Unknown command: {}. Try nerv --help", cmd);
            std::process::exit(1);
        }
    }
}

/// Headless print mode: read prompt from stdin, run agent, output JSON.
/// No TUI, no sessions, no memory, no permissions.
fn format_args_brief(args: &serde_json::Value) -> String {
    let obj = match args.as_object() {
        Some(o) if !o.is_empty() => o,
        _ => return String::new(),
    };
    let mut parts = Vec::new();
    let mut total_len = 0;
    for (k, v) in obj {
        let v_str = match v {
            serde_json::Value::String(s) if s.len() <= 60 => s.clone(),
            serde_json::Value::String(s) => {
                format!("{}…", &s[..s.floor_char_boundary(57)])
            }
            serde_json::Value::Bool(b) => b.to_string(),
            serde_json::Value::Number(n) => n.to_string(),
            _ => continue,
        };
        let part = format!("{}={}", k, v_str);
        total_len += part.len() + 2;
        if total_len > 120 {
            parts.push("…".into());
            break;
        }
        parts.push(part);
    }
    parts.join(", ")
}

fn print_mode(model_arg: Option<&str>, max_turns: u32, verbose: bool) {
    use std::io::Read;

    let nerv_dir = nerv_dir();
    std::fs::create_dir_all(nerv_dir).ok();

    nerv::log::init(&nerv_dir.join("debug.log"));
    if let Ok(level) = std::env::var("NERV_LOG").unwrap_or_default().parse() {
        nerv::log::set_level(level);
    }

    // Read prompt from stdin
    let mut prompt = String::new();
    std::io::stdin().read_to_string(&mut prompt).unwrap_or(0);
    let prompt = prompt.trim().to_string();
    if prompt.is_empty() {
        let err = serde_json::json!({"error": "no prompt provided on stdin"});
        println!("{}", err);
        std::process::exit(1);
    }

    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let b = nerv::bootstrap::bootstrap(
        &cwd,
        &nerv_dir,
        nerv::bootstrap::BootstrapOptions {
            memory: false,
            permissions: false,
            talk_mode: false,
        },
    );
    for warning in &b.config_warnings {
        eprintln!("warning: {}", warning);
    }
    let mut agent = b.session.agent;

    // Select model
    let model = if let Some(name) = model_arg {
        nerv::bootstrap::resolve_model(&b.model_registry, name)
    } else {
        b.model_registry.default_model(&b.config).cloned()
    };
    if let Some(ref m) = model {
        agent.state.model = Some(m.clone());
        eprintln!("model: {}/{}", m.provider_name, m.id);
    } else {
        let err = serde_json::json!({"error": "no model configured (use --model or set default)"});
        println!("{}", err);
        std::process::exit(1);
    }

    // Build system prompt
    agent.state.tools = b.session.tool_registry.active_tools();
    let tool_names: Vec<&str> = agent.state.tools.iter().map(|t| t.name()).collect();
    let snippets = b.session.tool_registry.prompt_snippets();
    let guidelines = b.session.tool_registry.prompt_guidelines();
    let model_id = model.as_ref().map(|m| m.id.as_str());
    agent.state.system_prompt = nerv::core::system_prompt::build_system_prompt_for_model(
        &cwd, &b.resources, &tool_names, &snippets, &guidelines, model_id,
    );

    // Collect metrics via the event callback (Mutex for Sync — no contention in practice)
    use std::sync::Mutex;

    struct Metrics {
        turns: u32,
        tool_calls: Vec<serde_json::Value>,
        tokens_in: u32,
        tokens_out: u32,
        tokens_cache_read: u32,
        cost: nerv::agent::types::Cost,
        current_tool: Option<(String, std::time::Instant)>,
        last_usage: Option<nerv::agent::types::Usage>,
        usages: Vec<nerv::agent::types::Usage>,
        verbose: bool,
        in_text: bool,
    }

    let metrics = Mutex::new(Metrics {
        turns: 0,
        tool_calls: Vec::new(),
        tokens_in: 0,
        tokens_out: 0,
        tokens_cache_read: 0,
        cost: nerv::agent::types::Cost::default(),
        current_tool: None,
        last_usage: None,
        usages: Vec::new(),
        verbose,
        in_text: false,
    });

    let model_ref = model.clone();
    let start = std::time::Instant::now();

    // Build prompt messages
    let user_msg = nerv::agent::types::AgentMessage::User {
        content: vec![nerv::agent::types::ContentItem::Text { text: prompt }],
        timestamp: nerv::now_millis(),
    };

    let cancel = agent.cancel.clone();

    // Graceful SIGINT: set cancel flag instead of dying, so JSON output flushes.
    PRINT_CANCEL.set(cancel.clone()).ok();
    unsafe { libc::signal(libc::SIGINT, handle_sigint_print as *const () as libc::sighandler_t); }

    let new_messages = agent.prompt(vec![user_msg], &|event| {
        use nerv::agent::types::{AgentEvent, StreamDelta};
        let mut m = metrics.lock().unwrap();
        match &event {
            AgentEvent::TurnStart => {
                m.turns += 1;
                if m.turns > max_turns {
                    cancel.store(true, std::sync::atomic::Ordering::Relaxed);
                }
                if m.verbose {
                    eprintln!("\n── turn {} {}", m.turns, "─".repeat(40));
                }
            }
            AgentEvent::MessageUpdate { delta } if m.verbose => {
                if let StreamDelta::Text(s) = delta {
                    m.in_text = true;
                    eprint!("{}", s);
                }
            }
            AgentEvent::ToolExecutionStart { name, args, .. } => {
                if m.in_text {
                    eprintln!();
                    m.in_text = false;
                }
                if m.verbose {
                    let brief = format_args_brief(args);
                    if brief.is_empty() {
                        eprint!("  {}() ... ", name);
                    } else {
                        eprint!("  {}({}) ... ", name, brief);
                    }
                } else {
                    eprint!("  turn {} › {} ... ", m.turns, name);
                }
                m.current_tool = Some((name.clone(), std::time::Instant::now()));
            }
            AgentEvent::ToolExecutionEnd { result, .. } => {
                if let Some((name, start)) = m.current_tool.take() {
                    let ms = start.elapsed().as_millis();
                    let status = if result.is_error { "err" } else { "ok" };
                    if m.verbose {
                        let summary = result.display.as_ref().map(|s| s.as_str()).unwrap_or_else(|| {
                            result.content.lines().next().unwrap_or("")
                        });
                        if summary.is_empty() || summary.len() > 120 {
                            eprintln!("{} ({}ms)", status, ms);
                        } else {
                            eprintln!("{} ({}ms) — {}", status, ms, summary);
                        }
                    } else {
                        eprintln!("{} ({}ms)", status, ms);
                    }
                    m.tool_calls.push(serde_json::json!({
                        "name": name,
                        "duration_ms": ms as u64,
                        "is_error": result.is_error,
                    }));
                }
            }
            AgentEvent::MessageEnd { message } => {
                if m.in_text {
                    eprintln!();
                    m.in_text = false;
                }
                if let Some(ref usage) = message.usage {
                    if usage.input > m.tokens_in {
                        m.tokens_in = usage.input;
                    }
                    m.tokens_out += usage.output;
                    if usage.cache_read > m.tokens_cache_read {
                        m.tokens_cache_read = usage.cache_read;
                    }
                    if let Some(ref model) = model_ref {
                        m.cost.add_usage(usage, &model.pricing);
                    }
                    m.last_usage = Some(usage.clone());
                    m.usages.push(usage.clone());
                }
            }
            AgentEvent::Retrying { attempt, wait_secs, reason } if m.verbose => {
                eprintln!("  retry {} ({}s): {}", attempt, wait_secs, reason);
            }
            _ => {}
        }
    }, None);

    let wall_time = start.elapsed();
    let m = metrics.into_inner().unwrap();

    // Extract final assistant text
    let final_text: String = new_messages
        .iter()
        .filter_map(|msg| {
            if let nerv::agent::types::AgentMessage::Assistant(a) = msg {
                let text = a.text_content();
                if text.is_empty() { None } else { Some(text) }
            } else {
                None
            }
        })
        .last()
        .unwrap_or_default();

    // Build message trace for debugging
    let mut usage_idx = 0;
    let trace: Vec<serde_json::Value> = new_messages
        .iter()
        .filter_map(|msg| match msg {
            nerv::agent::types::AgentMessage::User { content, .. } => {
                let text: String = content
                    .iter()
                    .filter_map(|c| match c {
                        nerv::agent::types::ContentItem::Text { text } => Some(text.as_str()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("");
                Some(serde_json::json!({"role": "user", "text": text}))
            }
            nerv::agent::types::AgentMessage::Assistant(a) => {
                let text = a.text_content();
                let tools: Vec<serde_json::Value> = a
                    .content
                    .iter()
                    .filter_map(|b| match b {
                        nerv::agent::types::ContentBlock::ToolCall { name, arguments, .. } => {
                            Some(serde_json::json!({"tool": name, "args": arguments}))
                        }
                        _ => None,
                    })
                    .collect();
                let mut entry = serde_json::json!({"role": "assistant"});
                if !text.is_empty() {
                    entry["text"] = serde_json::Value::String(text);
                }
                if !tools.is_empty() {
                    entry["tool_calls"] = serde_json::Value::Array(tools);
                }
                entry["stop_reason"] = serde_json::Value::String(format!("{:?}", a.stop_reason));
                if let Some(usage) = m.usages.get(usage_idx) {
                    entry["usage"] = serde_json::json!({
                        "input": usage.input,
                        "output": usage.output,
                        "cache_read": usage.cache_read,
                    });
                    usage_idx += 1;
                }
                Some(entry)
            }
            nerv::agent::types::AgentMessage::ToolResult {
                content, is_error, ..
            } => {
                let text: String = content
                    .iter()
                    .filter_map(|c| match c {
                        nerv::agent::types::ContentItem::Text { text } => Some(text.as_str()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("");
                // Truncate long tool results in the trace
                let text = if text.len() > 500 {
                    format!("{}...[truncated {}b]", &text[..text.floor_char_boundary(500)], text.len())
                } else {
                    text
                };
                Some(serde_json::json!({"role": "tool_result", "text": text, "is_error": is_error}))
            }
            _ => None,
        })
        .collect();

    let output = serde_json::json!({
        "success": !new_messages.iter().any(|msg| matches!(msg,
            nerv::agent::types::AgentMessage::Assistant(a) if a.stop_reason.is_error()
        )),
        "final_text": final_text,
        "trace": trace,
        "metrics": {
            "turns": m.turns,
            "tool_calls": m.tool_calls,
            "tokens_in": m.tokens_in,
            "tokens_out": m.tokens_out,
            "tokens_cache_read": m.tokens_cache_read,
            "cost": (m.cost.total * 10000.0).round() / 10000.0,
            "wall_time_ms": wall_time.as_millis() as u64,
        }
    });

    println!("{}", serde_json::to_string_pretty(&output).unwrap());
}
