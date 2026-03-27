use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use nerv::core::*;
use nerv::home_dir;
use nerv::interactive::event_loop::InteractiveMode;
use nerv::interactive::footer::FooterComponent;
use nerv::interactive::layout::AppLayout;
use nerv::interactive::statusbar::StatusBar;
use nerv::tui::components::editor::Editor;
use nerv::tui::*;

fn main() {
    let args: Vec<String> = std::env::args().collect();

    // Subcommands (run before TUI setup)
    if let Some(cmd) = args.get(1).map(|s| s.as_str()) {
        match cmd {
            "-h" | "--help" => {
                println!("nerv — coding agent for the terminal");
                println!();
                println!("Usage: nerv [options]");
                println!("       nerv <command> [args]");
                println!();
                println!("Options:");
                println!("  --resume           Open session picker immediately");
                println!("  --resume <id>      Resume a specific session");
                println!("  --print            Headless mode: read prompt from stdin, output JSON");
                println!("  --max-turns <n>    Max agent turns in print mode (default 20)");
                println!("  --model <name>     Select model (e.g. opus, sonnet, haiku)");
                println!("  --json             JSON output in print mode (default)");
                println!("  --list-models      List all configured models");
                println!("  --log-level <lvl>  Set log level (debug, info, warn, error)");
                println!("  --export-html <id> Export session to HTML (optional: output path)");
                println!("  -h, --help         Show this help");
                println!("  --version          Show version");
                println!();
                println!("Environment:");
                println!("  NERV_LOG=<level>   Set log level (default: warn)");
                println!();
                println!("Commands:");
                println!("  add <repo> <quant>   Download GGUF from HuggingFace");
                println!("  load [alias]         Run llama-server for a model");
                println!("  models               List configured local models");
                println!("  unload               Kill running llama-server");
                return;
            }
            "--version" => {
                println!("nerv {}", env!("CARGO_PKG_VERSION"));
                return;
            }
            "--print" => {
                print_mode(&args);
                return;
            }
            "--list-models" => {
                list_all_models();
                return;
            }
            "add" | "load" | "models" | "unload" => {
                let nerv_dir = home_dir()
                    .unwrap_or_else(|| PathBuf::from("."))
                    .join(".nerv");
                std::fs::create_dir_all(&nerv_dir).ok();
                handle_subcommand(cmd, &args[2..], &nerv_dir);
                return;
            }
            _ => {}
        }
    }
    if args.iter().any(|a| a == "--version") {
        println!("nerv {}", env!("CARGO_PKG_VERSION"));
        return;
    }

    // Reject unknown flags early
    {
        let known = [
            "--resume", "--model", "--log-level", "--version", "--export-html",
        ];
        let mut i = 1;
        while i < args.len() {
            let arg = &args[i];
            if arg.starts_with('-') && !known.contains(&arg.as_str()) {
                eprintln!("Unknown option: {}. Try nerv --help", arg);
                std::process::exit(1);
            }
            // Skip the value for flags that take one
            if matches!(arg.as_str(), "--resume" | "--model" | "--log-level" | "--export-html") {
                i += 1;
            }
            i += 1;
        }
    }

    let nerv_dir = home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".nerv");
    std::fs::create_dir_all(&nerv_dir).ok();

    nerv::log::init(&nerv_dir.join("debug.log"));
    if let Some(pos) = args.iter().position(|a| a == "--log-level") {
        if let Ok(level) = args.get(pos + 1).map(|s| s.parse()).unwrap_or(Err(())) {
            nerv::log::set_level(level);
        }
    } else if let Ok(level) = std::env::var("NERV_LOG").unwrap_or_default().parse() {
        nerv::log::set_level(level);
    }
    nerv::log::info("nerv starting");

    if let Some(pos) = args.iter().position(|a| a == "--export-html") {
        let session_id = args.get(pos + 1).unwrap_or_else(|| {
            eprintln!("Usage: nerv --export-html <session-id> [output.html]");
            std::process::exit(1);
        });
        let exports_dir = nerv_dir.join("exports");
        let out_path = args
            .get(pos + 2)
            .filter(|a| !a.starts_with('-'))
            .map(PathBuf::from)
            .unwrap_or_else(|| exports_dir.join(format!("{}.html", session_id)));
        // Create exports directory if it doesn't exist
        if let Err(e) = std::fs::create_dir_all(out_path.parent().unwrap_or(&exports_dir)) {
            eprintln!("Failed to create exports directory: {}", e);
            std::process::exit(1);
        }
        match nerv::export::export_session_html(session_id, &out_path, &nerv_dir) {
            Ok(path) => {
                println!("Exported to {}", path);
                return;
            }
            Err(e) => {
                eprintln!("Export failed: {}", e);
                std::process::exit(1);
            }
        }
    }

    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));

    let b = nerv::bootstrap::bootstrap(
        &cwd,
        &nerv_dir,
        nerv::bootstrap::BootstrapOptions {
            memory: true,
            permissions: true,
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

    let session = b.session;

    // Channels (crossbeam)
    let (cmd_tx, cmd_rx) = crossbeam_channel::bounded::<SessionCommand>(32);
    let (event_tx, event_rx) = crossbeam_channel::bounded::<AgentSessionEvent>(256);

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

    let mut layout = AppLayout::new(Editor::new(), StatusBar::new(), footer);
    tui.fixed_bottom = 8; // editor + statusbar + footer — never flushed to scrollback

    let dim = nerv::interactive::theme::DIM;
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

    layout.chat.push_styled(
        nerv::interactive::theme::WARN,
        "NERV console ready. Awaiting your command.",
    );

    tui.terminal_mut().start();
    tui.request_render(true); // initial render
    tui.maybe_render(&layout);

    let repo_root = nerv::find_repo_root(&cwd).map(|p| p.to_string_lossy().to_string());
    let mut interactive = InteractiveMode::new(
        cmd_tx,
        model_registry.clone(),
        model_registry.default_model(&config).cloned(),
        skills,
        repo_root,
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

    // Handle --model CLI flag (interactive mode)
    if let Some(pos) = args.iter().position(|a| a == "--model") {
        if let Some(name) = args.get(pos + 1) {
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
    }

    // Handle --resume CLI flag
    let resume_pos = args.iter().position(|a| a == "--resume");
    if let Some(pos) = resume_pos {
        if let Some(id) = args.get(pos + 1).filter(|s| !s.starts_with('-')) {
            // --resume <id> — load directly
            let _ = interactive
                .cmd_tx()
                .send(SessionCommand::LoadSession { id: id.clone() });
        } else {
            // --resume — open picker
            let _ = interactive.cmd_tx().send(SessionCommand::ListSessions {
                repo_root: interactive.repo_root(),
            });
        }
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
                            // Tree selector input
                            if interactive.tree_selector.is_some() {
                                if keys::matches_key(seq, "up") {
                                    interactive.tree_selector.as_mut().unwrap().move_up();
                                } else if keys::matches_key(seq, "down") {
                                    interactive.tree_selector.as_mut().unwrap().move_down();
                                } else if keys::matches_key(seq, "enter") {
                                    if let Some(id) = interactive.tree_selector.as_ref().unwrap().selected_entry_id().map(|s| s.to_string()) {
                                        pop_tree(&mut interactive, &mut layout);
                                        let _ = interactive.cmd_tx().send(nerv::core::SessionCommand::SwitchBranch { entry_id: id });
                                    }
                                } else if keys::matches_key(seq, "escape") || keys::matches_key(seq, "ctrl+c") {
                                    pop_tree(&mut interactive, &mut layout);
                                }
                                render_tree(&mut interactive, &mut layout);
                                tui.request_render(false); tui.maybe_render(&layout);
                                continue;
                            }

                            // Session picker input
                            if interactive.session_picker.is_some() {
                                if keys::matches_key(seq, "up") {
                                    interactive.session_picker.as_mut().unwrap().move_up();
                                } else if keys::matches_key(seq, "down") {
                                    interactive.session_picker.as_mut().unwrap().move_down();
                                } else if keys::matches_key(seq, "enter") {
                                    if let Some(id) = interactive.session_picker.as_ref().unwrap().selected_id().map(|s| s.to_string()) {
                                        pop_picker(&mut interactive, &mut layout);
                                        let _ = interactive.cmd_tx().send(nerv::core::SessionCommand::LoadSession { id });
                                    }
                                } else if keys::matches_key(seq, "escape") || keys::matches_key(seq, "ctrl+c") {
                                    pop_picker(&mut interactive, &mut layout);
                                } else if keys::matches_key(seq, "backspace") {
                                    let picker = interactive.session_picker.as_mut().unwrap();
                                    picker.pop_char();
                                    if picker.has_query() {
                                        let q = picker.query.clone();
                                        let _ = interactive.cmd_tx().send(nerv::core::SessionCommand::SearchSessions { query: q });
                                    }
                                } else if keys::matches_key(seq, "ctrl+u") {
                                    interactive.session_picker.as_mut().unwrap().clear_query();
                                } else if seq.len() >= 1 && seq[0] >= 0x20 && seq[0] != 0x7F && !seq.starts_with(b"\x1b") {
                                    // Printable character (including multi-byte UTF-8)
                                    if let Ok(ch_str) = std::str::from_utf8(seq) {
                                        let picker = interactive.session_picker.as_mut().unwrap();
                                        for ch in ch_str.chars() {
                                            picker.push_char(ch);
                                        }
                                        let q = picker.query.clone();
                                        let _ = interactive.cmd_tx().send(nerv::core::SessionCommand::SearchSessions { query: q });
                                    }
                                }
                                // Re-render picker if still active
                                render_picker(&mut interactive, &mut layout);
                                tui.request_render(false); tui.maybe_render(&layout);
                                continue;
                            }

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
                            if keys::matches_key(seq, "ctrl+t") {
                                let next = interactive.cycle_thinking();
                                let _ = interactive.cmd_tx().try_send(SessionCommand::SetThinkingLevel { level: next });
                                layout.footer.set_thinking(next);
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
                            if keys::matches_key(seq, "shift+enter") || keys::matches_key(seq, "ctrl+enter") {
                                layout.editor.handle_input(b"\n");
                                tui.request_render(false); continue;
                            }
                            if keys::matches_key(seq, "enter") {
                                let text = layout.editor.take_text();
                                if !text.is_empty() {
                                    interactive.handle_submit(text);
                                    if interactive.quit_requested { tui.terminal_mut().stop(); should_quit = true; break; }
                                    layout.footer.set_thinking(interactive.current_thinking());
                                    if let Some(m) = interactive.current_model() { layout.footer.set_model(m); }
                                    if let Some(msg) = interactive.status_message.take() {
                                        push_status(&mut layout, &msg, interactive.status_is_error);
                                    }
                                    layout.statusbar.set_queue(&interactive.pending_messages, interactive.editing_queue_idx);
                                }
                                tui.request_render(false); continue;
                            }
                            // Queue navigation
                            if keys::matches_key(seq, "up") && interactive.is_streaming && !interactive.pending_messages.is_empty() {
                                if let Some(idx) = interactive.editing_queue_idx {
                                    let current = layout.editor.text().to_string();
                                    if !current.is_empty() { interactive.pending_messages[idx] = current; }
                                }
                                if let Some(text) = interactive.edit_queue_up() {
                                    layout.editor.set_text(&text); tui.request_render(false); tui.maybe_render(&layout); continue;
                                }
                            }
                            if keys::matches_key(seq, "down") && interactive.editing_queue_idx.is_some() {
                                if let Some(idx) = interactive.editing_queue_idx {
                                    let current = layout.editor.text().to_string();
                                    if !current.is_empty() { interactive.pending_messages[idx] = current; }
                                }
                                if let Some(text) = interactive.edit_queue_down() {
                                    layout.editor.set_text(&text); tui.request_render(false); tui.maybe_render(&layout); continue;
                                }
                            }
                            if keys::matches_key(seq, "backspace") && interactive.editing_queue_idx.is_some()
                                && layout.editor.is_empty()
                            {
                                interactive.remove_editing_queue_item();
                                layout.editor.clear();
                                layout.statusbar.set_queue(&interactive.pending_messages, interactive.editing_queue_idx);
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
                process_event(event, &mut interactive, &mut layout, &mut tui);
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
                tui.maybe_render(&layout);
            }
        }
    }

    // Grace period for session flush
    std::thread::sleep(Duration::from_millis(200));

    if let Some(id) = &interactive.session_id {
        let short = if id.len() > 8 { &id[..8] } else { id };
        // Print below the shell prompt line — println scrolls the terminal
        println!("To resume this session: nerv --resume {}", short);
    }
}

fn process_event(
    event: nerv::core::AgentSessionEvent,
    interactive: &mut InteractiveMode,
    layout: &mut AppLayout,
    tui: &mut TUI,
) {
    interactive.handle_event(event, layout, tui);
    if let Some(msg) = interactive.status_message.take() {
        push_status(layout, &msg, interactive.status_is_error);
        tui.request_render(false);
    }
    if interactive.session_picker.is_some() {
        render_picker(interactive, layout);
        tui.request_render(false);
    }
    if interactive.tree_selector.is_some() {
        render_tree(interactive, layout);
        tui.request_render(false);
    }
}

fn pop_picker(interactive: &mut InteractiveMode, layout: &mut AppLayout) {
    interactive.session_picker = None;
    layout.chat.clear_picker();
}

fn pop_tree(interactive: &mut InteractiveMode, layout: &mut AppLayout) {
    interactive.tree_selector = None;
    layout.chat.clear_picker();
}

fn render_picker(interactive: &mut InteractiveMode, layout: &mut AppLayout) {
    let Some(ref picker) = interactive.session_picker else {
        return;
    };
    let repo = interactive.repo_root();
    let lines = picker.render_lines(repo.as_deref());
    layout.chat.set_picker(lines);
}

fn render_tree(interactive: &mut InteractiveMode, layout: &mut AppLayout) {
    let Some(ref selector) = interactive.tree_selector else {
        return;
    };
    let lines = selector.render_lines();
    layout.chat.set_picker(lines);
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
    let nerv_dir = home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".nerv");
    let config = nerv::core::NervConfig::load(&nerv_dir);
    let mut auth = nerv::core::auth::AuthStorage::load(&nerv_dir);
    let registry = nerv::core::model_registry::ModelRegistry::new(&config, &mut auth);

    let available = registry.available_models();
    let all = registry.all_models();

    if all.is_empty() {
        println!("No models configured. Run `nerv --login` or set ANTHROPIC_API_KEY.");
        return;
    }

    let mut last_provider = String::new();
    for m in &all {
        if m.provider_name != last_provider {
            println!("\n  [{}]", m.provider_name);
            last_provider = m.provider_name.clone();
        }
        let online = available.iter().any(|a| a.id == m.id);
        let marker = if online { "●" } else { "○" };
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
        _ => {
            eprintln!("Unknown command: {}", cmd);
            std::process::exit(1);
        }
    }
}

/// Headless print mode: read prompt from stdin, run agent, output JSON.
/// No TUI, no sessions, no memory, no permissions.
fn print_mode(args: &[String]) {
    use std::io::Read;

    let nerv_dir = home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".nerv");
    std::fs::create_dir_all(&nerv_dir).ok();

    nerv::log::init(&nerv_dir.join("debug.log"));
    if let Ok(level) = std::env::var("NERV_LOG").unwrap_or_default().parse() {
        nerv::log::set_level(level);
    }

    let max_turns: u32 = args
        .iter()
        .position(|a| a == "--max-turns")
        .and_then(|i| args.get(i + 1))
        .and_then(|s| s.parse().ok())
        .unwrap_or(20);

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
        },
    );
    let mut agent = b.session.agent;

    // Select model
    let model_arg = args
        .iter()
        .position(|a| a == "--model")
        .and_then(|i| args.get(i + 1));
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

    // Collect metrics via the event callback (using RefCell since Fn closure)
    use std::cell::RefCell;

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
    }

    let metrics = RefCell::new(Metrics {
        turns: 0,
        tool_calls: Vec::new(),
        tokens_in: 0,
        tokens_out: 0,
        tokens_cache_read: 0,
        cost: nerv::agent::types::Cost::default(),
        current_tool: None,
        last_usage: None,
        usages: Vec::new(),
    });

    let model_ref = model.clone();
    let start = std::time::Instant::now();

    // Build prompt messages
    let user_msg = nerv::agent::types::AgentMessage::User {
        content: vec![nerv::agent::types::ContentItem::Text { text: prompt }],
        timestamp: nerv::now_millis(),
    };

    let cancel = agent.cancel.clone();

    let new_messages = agent.prompt(vec![user_msg], &|event| {
        use nerv::agent::types::AgentEvent;
        let mut m = metrics.borrow_mut();
        match &event {
            AgentEvent::TurnStart => {
                m.turns += 1;
                if m.turns > max_turns {
                    cancel.store(true, std::sync::atomic::Ordering::Relaxed);
                }
            }
            AgentEvent::ToolExecutionStart { name, .. } => {
                eprint!("  turn {} › {} ... ", m.turns, name);
                m.current_tool = Some((name.clone(), std::time::Instant::now()));
            }
            AgentEvent::ToolExecutionEnd { result, .. } => {
                if let Some((name, start)) = m.current_tool.take() {
                    let ms = start.elapsed().as_millis();
                    let status = if result.is_error { "err" } else { "ok" };
                    eprintln!("{} ({}ms)", status, ms);
                    m.tool_calls.push(serde_json::json!({
                        "name": name,
                        "duration_ms": ms as u64,
                        "is_error": result.is_error,
                    }));
                }
            }
            AgentEvent::MessageEnd { message } => {
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
            _ => {}
        }
    });

    let wall_time = start.elapsed();
    let m = metrics.into_inner();

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
                    format!("{}...[truncated {}b]", &text[..500], text.len())
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
