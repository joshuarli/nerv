use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

mod cli;

use nerv::agent::EffortLevel;
use crate::cli::{Cmd, RepoGateResult, ResumeOpt, handle_subcommand, parse_args, repo_gate};
use nerv::core::*;
use nerv::interactive::event_loop::InteractiveMode;
use nerv::interactive::footer::FooterComponent;
use nerv::interactive::layout::AppLayout;
use nerv::interactive::statusbar::StatusBar;
use nerv::nerv_dir;
use nerv::tui::components::editor::Editor;
use nerv::tui::*;

/// Render a frame and notify ChatWriter of how many lines have been flushed to
/// terminal scrollback, so it can free heap memory for blocks it will never
/// need to diff-render again.
macro_rules! render_frame {
    ($tui:expr, $layout:expr) => {{
        $tui.maybe_render(&$layout, $layout.fixed_bottom_lines());
    }};
}

/// Global cancel flag for print mode — SIGINT sets this instead of killing the
/// process.
static PRINT_CANCEL: OnceLock<Arc<AtomicBool>> = OnceLock::new();

extern "C" fn handle_sigint_print(_: libc::c_int) {
    if let Some(cancel) = PRINT_CANCEL.get() {
        cancel.store(true, Ordering::Relaxed);
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
    let (
        opt_model,
        resume_opt,
        log_level_opt,
        wt_opt,
        mut talk_mode,
        opt_prompt,
        opt_thinking,
        opt_effort,
    ) = match cmd {
        Cmd::Interactive { model, resume, log_level, prompt, thinking, effort } => {
            (model, resume, log_level, None, false, prompt, thinking, effort)
        }
        Cmd::Wt { branch, model, log_level, prompt, thinking, effort } => {
            (model, ResumeOpt::None, log_level, Some(branch), false, prompt, thinking, effort)
        }
        Cmd::Resume { id } => {
            let resume = match id {
                Some(id) => ResumeOpt::Session(id),
                None => ResumeOpt::Picker,
            };
            (None, resume, None, None, false, None, false, None)
        }
        Cmd::Talk { model, log_level, prompt, thinking, effort } => {
            (model, ResumeOpt::None, log_level, None, true, prompt, thinking, effort)
        }
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
        match nerv::worktree::create_worktree(&repo_root, nerv_dir, &branch, prefix) {
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
        nerv_dir,
        nerv::bootstrap::BootstrapOptions { memory: true, permissions: true, talk_mode },
    );
    let config = b.config;
    let model_registry = b.model_registry;
    let skills = b.resources.skills.clone();
    let cancel_flag = b.cancel_flag;

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
    let system_prompt_tokens =
        b.resources.system_prompt.as_ref().map(|sp| nerv::compaction::count_tokens(sp));
    let memory_tokens =
        b.resources.memory.as_ref().map(|m| nerv::compaction::count_tokens(m)).filter(|&t| t > 0);
    let append_prompt_tokens: Vec<usize> =
        b.resources.append_prompts.iter().map(|ap| nerv::compaction::count_tokens(ap)).collect();
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
    let initial_tools = session.tool_registry.active_tools();
    if let Some(ref wt) = worktree_path {
        session.set_worktree(wt.clone());
    }

    // Channels (crossbeam)
    let (cmd_tx, cmd_rx) = crossbeam_channel::bounded::<SessionCommand>(32);
    let (event_tx, event_rx) = crossbeam_channel::bounded::<AgentSessionEvent>(64);

    // Capture initial state before session is moved to its thread
    let initial_thinking_level = session.agent.state.thinking_level;
    let initial_effort_level = session.agent.state.effort_level;
    // Clone compact_threshold_arc so the main thread can write it directly for
    // immediate effect without waiting for SetCompactThreshold through cmd_tx.
    let compact_threshold_arc = session.compaction.threshold_pct.clone();
    // Clone the provider_registry Arc so the main thread can make /btw overlay
    // calls.
    let provider_registry = session.agent.provider_registry.clone();
    // cancel_flag was cloned from session.agent.cancel in bootstrap — same Arc, no
    // re-clone needed.

    // Session thread
    let evt_tx = event_tx.clone();
    std::thread::Builder::new()
        .name("nerv-session".into())
        .stack_size(4 * 1024 * 1024)
        .spawn(move || session_task(cmd_rx, evt_tx, session))
        .expect("failed to spawn session thread");

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

    let dim = nerv::interactive::theme::DIM;
    if !talk_mode {
        let load =
            |chat: &mut nerv::interactive::chat_writer::ChatWriter, name: &str, tok: usize| {
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
    render_frame!(tui, layout);

    let repo_root_path = nerv::find_repo_root(&cwd);
    let repo_id = repo_root_path.as_deref().and_then(nerv::repo_fingerprint);
    let repo_root = repo_root_path.map(|p| p.to_string_lossy().to_string());
    let mut interactive = InteractiveMode::new(
        cmd_tx,
        model_registry.clone(),
        provider_registry,
        initial_tools,
        model_registry.default_model(&config).cloned(),
        initial_thinking_level,
        initial_effort_level,
        config.clone(),
    );
    interactive.set_repo_root(repo_root);
    interactive.set_repo_id(repo_id);
    interactive.set_skills(skills);
    interactive.cancel_flag = cancel_flag.clone();
    interactive.midturn_inject = b.midturn_inject;
    interactive.compact_threshold_arc = compact_threshold_arc;

    layout.editor.set_completions(interactive.slash_completions());

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
            let _ = interactive.cmd_tx().send(SessionCommand::LoadSession { id });
        }
        ResumeOpt::Picker => {
            let _ = interactive.cmd_tx().send(SessionCommand::ListSessions {
                repo_root: interactive.repo_root(),
                repo_id: interactive.repo_id(),
            });
        }
        ResumeOpt::None => {}
    }

    // Apply --thinking / --effort flags
    if opt_thinking {
        let _ = interactive
            .cmd_tx()
            .try_send(SessionCommand::SetThinkingLevel { level: nerv::agent::ThinkingLevel::On });
    }
    if let Some(effort) = opt_effort {
        let _ =
            interactive.cmd_tx().try_send(SessionCommand::SetEffortLevel { level: Some(effort) });
    }

    // Apply --prompt: send as the initial user message once the TUI is up
    if let Some(text) = opt_prompt {
        let _ = interactive.cmd_tx().send(SessionCommand::Prompt { text });
    }

    // Health check custom providers (non-blocking, retries until online)
    for provider_cfg in &config.custom_providers {
        let name = provider_cfg.name.clone();
        let url = format!("{}/models", provider_cfg.base_url);
        let tx = event_tx.clone();
        std::thread::Builder::new()
            .name("nerv-provider-health".into())
            .stack_size(64 * 1024)
            .spawn(move || {
                let agent = ureq::Agent::config_builder()
                    .timeout_global(Some(std::time::Duration::from_secs(2)))
                    .build()
                    .new_agent();
                loop {
                    let online = agent.get(&url).call().is_ok();
                    let _ = tx
                        .send(AgentSessionEvent::ProviderHealth { provider: name.clone(), online });
                    if online {
                        break;
                    }
                    std::thread::sleep(Duration::from_secs(3));
                }
            })
            .expect("failed to spawn provider health thread");
    }

    // Stdin reader thread — uses poll() so it can be paused for $EDITOR
    let stdin_paused = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let stdin_paused2 = stdin_paused.clone();
    let (stdin_tx, stdin_rx) = crossbeam_channel::bounded::<Vec<u8>>(64);
    std::thread::Builder::new()
        .name("nerv-stdin".into())
        .stack_size(64 * 1024)
        .spawn(move || {
            use std::io::Read;
            let mut buf = [0u8; 1024];
            let mut pfd = libc::pollfd { fd: libc::STDIN_FILENO, events: libc::POLLIN, revents: 0 };
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
        })
        .expect("failed to spawn stdin reader thread");

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
            interactive.handle_abort();
            layout.statusbar.cancel_streaming();
            tui.request_render(false);
            render_frame!(tui, layout);
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
                            // Permission prompt input (y/n/a)
                            if interactive.pending_permission.is_some() {
                                // 'a' / 'A' — allow the entire directory containing the path
                                if seq == b"a" || seq == b"A" {
                                    // Extract path from pending details and find its parent dir.
                                    if let Some((tool, ref args)) = interactive.pending_permission_details.clone()
                                        && let Some(path_str) = nerv::core::permissions::path_for_args(&tool, args)
                                    {
                                        let path = std::path::PathBuf::from(&path_str);
                                        let dir = if path.is_dir() {
                                            path.clone()
                                        } else {
                                            path.parent().map(|p| p.to_path_buf()).unwrap_or(path)
                                        };
                                        interactive.allowed_dirs.push(dir.clone());
                                        layout.chat.push_styled(
                                            nerv::interactive::theme::SUCCESS,
                                            &format!("  → allowed dir: {}", dir.display()),
                                        );
                                    }
                                    if let Some(tx) = interactive.pending_permission.take() {
                                        let _ = tx.send(true);
                                    }
                                    interactive.pending_permission_details = None;
                                    interactive.status_message = None;
                                    interactive.status_is_error = false;
                                    tui.request_render(false); render_frame!(tui, layout);
                                    continue;
                                }

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
                                    tui.request_render(false); render_frame!(tui, layout);
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
                                tui.request_render(false); render_frame!(tui, layout);
                                continue;
                            }
                            // Dismiss the inline /btw panel on Esc or Enter.
                            if layout.btw_panel.is_some()
                                && (keys::matches_key(seq, "escape") || keys::matches_key(seq, "enter"))
                            {
                                if let Some(panel) = layout.btw_panel.take() {
                                    panel.cancel();
                                }
                                tui.request_render(false); render_frame!(tui, layout);
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
                                tui.resume(); layout.chat.reset_eviction(); render_frame!(tui, layout); continue;
                            }
                            if keys::matches_key(seq, "shift+tab") {
                                let enabled = interactive.toggle_plan_mode();
                                let label = if enabled { "Plan mode on" } else { "Plan mode off" };
                                push_status(&mut layout, label, false);
                                interactive.refresh_footer(&mut layout.footer);
                                tui.request_render(false); render_frame!(tui, layout); continue;
                            }
                            if keys::matches_key(seq, "ctrl+s") {
                                if interactive.session_id.is_some() {
                                    let _ = interactive.cmd_tx().try_send(SessionCommand::GetTree);
                                } else {
                                    push_status(&mut layout, "No active session.", false);
                                    tui.request_render(false); render_frame!(tui, layout);
                                }
                                continue;
                            }
                            if keys::matches_key(seq, "ctrl+t") {
                                let next = interactive.cycle_thinking();
                                let _ = interactive.cmd_tx().try_send(SessionCommand::SetThinkingLevel { level: next });
                                interactive.refresh_footer(&mut layout.footer);
                                tui.request_render(false); render_frame!(tui, layout); continue;
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
                                tui.request_render(false); render_frame!(tui, layout); continue;
                            }
                            if keys::matches_key(seq, "ctrl+g") {
                                stdin_paused.store(true, std::sync::atomic::Ordering::SeqCst);
                                std::thread::sleep(std::time::Duration::from_millis(60));
                                tui.terminal_mut().stop();
                                layout.editor.open_in_external_editor();
                                tui.terminal_mut().restart();
                                stdin_paused.store(false, std::sync::atomic::Ordering::SeqCst);
                                tui.request_render(true); render_frame!(tui, layout); continue;
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
                                        // ToggleHud is lightweight — handle inline without pausing stdin.
                                        if matches!(req, nerv::interactive::event_loop::PickerRequest::ToggleHud) {
                                            let on = layout.footer.toggle_hud();
                                            interactive.status_message = Some(if on { "HUD on".into() } else { "HUD off".into() });
                                            interactive.refresh_footer(&mut layout.footer);
                                            if let Some(msg) = interactive.status_message.take() {
                                                push_status(&mut layout, &msg, interactive.status_is_error);
                                            }
                                            tui.request_render(false); continue;
                                        }
                                        launch_picker(req, &mut interactive, &mut layout, &stdin_paused);
                                        tui.request_render(true); render_frame!(tui, layout); continue;
                                    }
                                    if interactive.quit_requested { tui.terminal_mut().stop(); should_quit = true; break; }
                                    interactive.refresh_footer(&mut layout.footer);
                                    if let Some(msg) = interactive.status_message.take() {
                                        push_status(&mut layout, &msg, interactive.status_is_error);
                                    }
                                    layout.statusbar.set_queue(&interactive.pending_messages, interactive.editing_queue_idx);
                                    layout.statusbar.render_queue(tui.width());
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
                                    tui.request_render(false); render_frame!(tui, layout); continue;
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
                                tui.request_render(false); render_frame!(tui, layout); continue;
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
                                    tui.request_render(false); render_frame!(tui, layout); continue;
                                }
                            }
                            if keys::matches_key(seq, "backspace") && interactive.editing_queue_idx.is_some()
                                && layout.editor.is_empty()
                            {
                                interactive.remove_editing_queue_item();
                                layout.editor.clear();
                                layout.statusbar.set_queue(&interactive.pending_messages, interactive.editing_queue_idx);
                                layout.statusbar.render_queue(tui.width());
                                tui.request_render(false); render_frame!(tui, layout); continue;
                            }
                            // History navigation: up/down when not streaming and cursor is on the first line
                            if keys::matches_key(seq, "up") && !interactive.is_streaming
                                && layout.editor.cursor_line() == 0
                            {
                                let current = layout.editor.text();
                                if let Some(text) = interactive.history_up(&current) {
                                    layout.editor.set_text(&text);
                                    tui.request_render(false); render_frame!(tui, layout); continue;
                                }
                            }
                            if keys::matches_key(seq, "down") && !interactive.is_streaming
                                && interactive.history_index.is_some()
                                && let Some(text) = interactive.history_down()
                            {
                                layout.editor.set_text(&text);
                                tui.request_render(false); render_frame!(tui, layout); continue;
                            }

                            layout.editor.handle_input(seq);
                        }
                        StdinEvent::Paste(text) => {
                            layout.editor.insert_paste(&text);
                        }
                    }
                }
                tui.request_render(false); render_frame!(tui, layout);
            }
            recv(event_rx) -> msg => {
                let Ok(event) = msg else { break };
                // Clear cancel flag when streaming ends so next ^C works normally
                if matches!(event, nerv::core::AgentSessionEvent::Agent(nerv::agent::types::AgentEvent::AgentEnd { .. })) {
                    cancel_flag.store(false, std::sync::atomic::Ordering::Relaxed);
                }
                process_event(event, &mut interactive, &mut layout, &mut tui, &stdin_paused);
                render_frame!(tui, layout);
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
                // Drain any new text from the inline /btw panel.
                if let Some(panel) = &mut layout.btw_panel
                    && panel.drain()
                {
                    tui.request_render(false);
                }
                render_frame!(tui, layout);
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
    use nerv::interactive::event_loop::PickerRequest;
    use nerv::interactive::fullscreen_picker::run_fullscreen_picker;
    use nerv::interactive::tree_selector::TreeSelection;

    // Pause the stdin reader so the picker owns stdin bytes exclusively.
    // Wait longer than the poll(100ms) timeout so the thread quiesces.
    stdin_paused.store(true, std::sync::atomic::Ordering::SeqCst);
    std::thread::sleep(std::time::Duration::from_millis(150));

    // ToggleHud is handled inline at the call site; this path is unreachable but
    // must be present for exhaustive pattern matching.
    if matches!(req, PickerRequest::ToggleHud) {
        stdin_paused.store(false, std::sync::atomic::Ordering::SeqCst);
        return;
    }

    // /btw inline panel: spawn background agent, attach panel to layout, return
    // immediately.
    if let PickerRequest::BtwOverlay { messages, system_prompt, tools, model, note } = req {
        let panel = nerv::interactive::btw_panel::spawn_btw(
            messages,
            system_prompt,
            tools,
            model,
            interactive.provider_registry.clone(),
            note,
        );
        layout.btw_panel = Some(panel);
        stdin_paused.store(false, std::sync::atomic::Ordering::SeqCst);
        return;
    }

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
            type SearchFn = dyn Fn(&str) -> Vec<nerv::session::manager::SearchResult>;
            let search_fn: Box<SearchFn> = {
                Box::new(move |q: &str| {
                    let mgr = nerv::session::manager::SessionManager::new(&repo_dir);
                    mgr.search_sessions(q)
                })
            };
            let mut picker = nerv::interactive::session_picker::SessionPicker::new(
                sessions, search_fn, repo_root,
            );
            run_fullscreen_picker(&mut picker).map(PickResult::Session).unwrap_or(PickResult::None)
        }
        PickerRequest::TreeSelector { tree, current_leaf } => {
            let mut selector =
                nerv::interactive::tree_selector::TreeSelector::new(tree, current_leaf);
            if run_fullscreen_picker(&mut selector).is_some() {
                selector.selected_node().map(PickResult::Tree).unwrap_or(PickResult::None)
            } else {
                PickResult::None
            }
        }
        PickerRequest::ModelPicker => {
            let models = interactive
                .model_registry()
                .available_models()
                .into_iter()
                .cloned()
                .collect::<Vec<_>>();
            let current = interactive.model_name().to_owned();
            let mut picker = nerv::interactive::model_picker::ModelPicker::new(models, current);
            run_fullscreen_picker(&mut picker).map(PickResult::Model).unwrap_or(PickResult::None)
        }
        // BtwOverlay and ToggleHud are handled above with early returns; these arms are
        // unreachable.
        PickerRequest::BtwOverlay { .. } => PickResult::None,
        PickerRequest::ToggleHud => PickResult::None,
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
    let style =
        if is_error { nerv::interactive::theme::ERROR } else { nerv::interactive::theme::MUTED };
    layout.chat.push_styled(style, msg);
}

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
        nerv_dir,
        nerv::bootstrap::BootstrapOptions { memory: false, permissions: false, talk_mode: false },
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
        agent.set_model(Some(m.clone()));
        eprintln!("model: {}/{}", m.provider_name, m.id);
    } else {
        let err = serde_json::json!({"error": "no model configured (use --model or set default)"});
        println!("{}", err);
        std::process::exit(1);
    }

    // Build system prompt
    agent.set_tools(b.session.tool_registry.active_tools());
    let tool_names: Vec<&str> = agent.state.tools.iter().map(|t| t.name()).collect();
    let snippets = b.session.tool_registry.prompt_snippets();
    let guidelines = b.session.tool_registry.prompt_guidelines();
    let model_id = model.as_ref().map(|m| m.id.as_str());
    agent.set_system_prompt(nerv::core::system_prompt::build_system_prompt_for_model(
        &cwd,
        &b.resources,
        &tool_names,
        &snippets,
        &guidelines,
        model_id,
    ));

    // Collect metrics via the event callback (Mutex for Sync — no contention in
    // practice)
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
    unsafe {
        libc::signal(libc::SIGINT, handle_sigint_print as *const () as libc::sighandler_t);
    }

    let new_messages = agent.prompt(
        vec![user_msg],
        &|event| {
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
                AgentEvent::MessageUpdate { delta: StreamDelta::Text(s), .. } if m.verbose => {
                    m.in_text = true;
                    eprint!("{}", s);
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
                            let summary = result
                                .display
                                .as_deref()
                                .unwrap_or_else(|| result.content.lines().next().unwrap_or(""));
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
        },
        None,
    );

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
        .next_back()
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
            nerv::agent::types::AgentMessage::ToolResult { content, is_error, .. } => {
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
                    format!(
                        "{}...[truncated {}b]",
                        &text[..text.floor_char_boundary(500)],
                        text.len()
                    )
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
