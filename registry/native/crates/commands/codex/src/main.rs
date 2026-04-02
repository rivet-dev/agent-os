/// Codex TUI for Agent OS WasmVM.
///
/// Full terminal UI using ratatui + crossterm backend, rendering through
/// the WasmVM PTY. This is the interactive entry point — for headless
/// (scriptable) usage, see codex-exec.
///
/// Uses wasi-spawn for process spawning via host_process FFI and
/// wasi-http for HTTP/HTTPS requests via host_net TCP/TLS imports.
///
/// crossterm is patched for WASI support (see patches/crates/crossterm/):
///   - Terminal raw mode tracked in-process (host PTY handles discipline)
///   - Terminal size from COLUMNS/LINES env vars
///   - Event source reads stdin directly and parses ANSI escape sequences
///   - IsTty returns true (PTY slave FDs are terminals)

use std::io;

use ratatui::{
    backend::CrosstermBackend,
    crossterm::{
        event::{self, Event, KeyCode, KeyEvent, KeyModifiers},
        execute,
        terminal::{self, EnterAlternateScreen, LeaveAlternateScreen},
    },
    layout::{Constraint, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, Paragraph, Wrap},
    Frame, Terminal,
};

// Validate WASI stub crates compile by referencing key types
use codex_network_proxy::NetworkProxy;
use codex_otel::SessionTelemetry;

const VERSION: &str = env!("CARGO_PKG_VERSION");

fn main() {
    let args: Vec<String> = std::env::args().collect();

    // Handle --help
    if args.iter().any(|a| a == "--help" || a == "-h") {
        print_help();
        return;
    }

    // Handle --version
    if args.iter().any(|a| a == "--version" || a == "-V") {
        println!("codex {}", VERSION);
        return;
    }

    // Handle --model flag (accept but note it's stored for future use)
    let model = args
        .windows(2)
        .find(|w| w[0] == "--model")
        .map(|w| w[1].clone());

    // Built-in HTTP test subcommand (validates wasi-http integration)
    if args.get(1).map(|s| s.as_str()) == Some("--http-test") {
        return http_test(&args[2..]);
    }

    // Stub validation subcommand (validates WASI stub crates)
    if args.get(1).map(|s| s.as_str()) == Some("--stub-test") {
        return stub_test();
    }

    // Run the TUI
    if let Err(e) = run_tui(model.as_deref()) {
        eprintln!("codex: TUI error: {}", e);
        std::process::exit(1);
    }
}

/// Main TUI event loop using ratatui + crossterm.
fn run_tui(model: Option<&str>) -> io::Result<()> {
    // Set up terminal
    terminal::enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;

    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    // App state
    let mut input = String::new();
    let mut messages: Vec<String> = Vec::new();
    let mut should_quit = false;

    // Draw initial frame
    terminal.draw(|f| draw_ui(f, &input, &messages, model))?;

    // Event loop
    while !should_quit {
        match event::read()? {
            Event::Key(key) => {
                handle_key_event(key, &mut input, &mut messages, &mut should_quit);
                terminal.draw(|f| draw_ui(f, &input, &messages, model))?;
            }
            Event::Resize(_, _) => {
                terminal.draw(|f| draw_ui(f, &input, &messages, model))?;
            }
            _ => {}
        }
    }

    // Restore terminal
    terminal::disable_raw_mode()?;
    execute!(io::stdout(), LeaveAlternateScreen)?;

    Ok(())
}

/// Handle a key event, updating app state.
fn handle_key_event(
    key: KeyEvent,
    input: &mut String,
    messages: &mut Vec<String>,
    should_quit: &mut bool,
) {
    match (key.code, key.modifiers) {
        // Ctrl+C quits
        (KeyCode::Char('c'), m) if m.contains(KeyModifiers::CONTROL) => {
            *should_quit = true;
        }
        // 'q' on empty input quits
        (KeyCode::Char('q'), KeyModifiers::NONE) if input.is_empty() => {
            *should_quit = true;
        }
        // Enter submits the input
        (KeyCode::Enter, _) => {
            if !input.is_empty() {
                let prompt = input.clone();
                messages.push(format!("> {}", prompt));
                messages.push("codex: agent loop is under development".to_string());
                messages.push(format!(
                    "codex: prompt received ({} chars)",
                    prompt.len()
                ));
                input.clear();
            }
        }
        // Backspace deletes last char
        (KeyCode::Backspace, _) => {
            input.pop();
        }
        // Esc clears input
        (KeyCode::Esc, _) => {
            input.clear();
        }
        // Regular character input
        (KeyCode::Char(c), _) => {
            input.push(c);
        }
        _ => {}
    }
}

/// Draw the TUI layout.
fn draw_ui(f: &mut Frame, input: &str, messages: &[String], model: Option<&str>) {
    let area = f.area();

    let chunks = Layout::vertical([
        Constraint::Length(3),  // Header
        Constraint::Min(5),    // Messages
        Constraint::Length(3), // Input
    ])
    .split(area);

    // Header
    let model_text = model.unwrap_or("default");
    let header = Paragraph::new(Line::from(vec![
        Span::styled("Codex ", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
        Span::styled(VERSION, Style::default().fg(Color::DarkGray)),
        Span::raw("  "),
        Span::styled(
            format!("model: {}", model_text),
            Style::default().fg(Color::Yellow),
        ),
        Span::raw("  "),
        Span::styled(
            "WasmVM",
            Style::default().fg(Color::Green),
        ),
    ]))
    .block(Block::default().borders(Borders::ALL).title(" codex "));
    f.render_widget(header, chunks[0]);

    // Messages area
    let msg_lines: Vec<Line> = messages
        .iter()
        .map(|m| {
            if m.starts_with("> ") {
                Line::from(Span::styled(m.as_str(), Style::default().fg(Color::Cyan)))
            } else {
                Line::from(Span::raw(m.as_str()))
            }
        })
        .collect();

    let welcome = if messages.is_empty() {
        vec![
            Line::from(""),
            Line::from(Span::styled(
                "Welcome to Codex on WasmVM!",
                Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
            )),
            Line::from(""),
            Line::from("Type a prompt and press Enter to submit."),
            Line::from("Press 'q' (on empty input) or Ctrl+C to exit."),
        ]
    } else {
        msg_lines
    };

    let messages_widget = Paragraph::new(Text::from(welcome))
        .block(Block::default().borders(Borders::ALL).title(" messages "))
        .wrap(Wrap { trim: false });
    f.render_widget(messages_widget, chunks[1]);

    // Input area
    let input_widget = Paragraph::new(Line::from(vec![
        Span::styled("❯ ", Style::default().fg(Color::Green)),
        Span::raw(input),
    ]))
    .block(Block::default().borders(Borders::ALL).title(" input "));
    f.render_widget(input_widget, chunks[2]);
}

fn print_help() {
    println!("codex {} — interactive Codex TUI for Agent OS WasmVM", VERSION);
    println!();
    println!("USAGE:");
    println!("    codex [OPTIONS]");
    println!();
    println!("OPTIONS:");
    println!("    -h, --help         Print this help message");
    println!("    -V, --version      Print version information");
    println!("    --model MODEL      Select model for completions");
    println!("    --http-test URL    Test HTTP client via host_net");
    println!("    --stub-test        Validate WASI stub crates");
    println!();
    println!("DESCRIPTION:");
    println!("    Interactive TUI for the Codex agent, rendered through");
    println!("    the WasmVM PTY using ratatui + crossterm backend.");
    println!("    For headless mode, use codex-exec instead.");
}

fn stub_test() {
    let proxy = NetworkProxy;
    let mut env = std::collections::HashMap::new();
    proxy.apply_to_env(&mut env);
    println!("network-proxy: NetworkProxy is zero-size, apply_to_env is no-op");

    let telemetry = SessionTelemetry::new();
    telemetry.counter("test.counter", 1, &[]);
    telemetry.histogram("test.histogram", 42, &[]);
    println!("otel: SessionTelemetry metrics are no-ops");

    let global = codex_otel::metrics::global();
    assert!(global.is_none(), "global metrics should be None on WASI");
    println!("otel: global() returns None (no exporter on WASI)");

    println!("stub-test: all stubs validated successfully");
}

fn http_test(args: &[String]) {
    if args.is_empty() {
        eprintln!("usage: codex --http-test <url>");
        std::process::exit(1);
    }

    let url = &args[0];
    match wasi_http::get(url) {
        Ok(resp) => {
            println!("status: {}", resp.status);
            match resp.text() {
                Ok(body) => println!("body: {}", body),
                Err(e) => eprintln!("body decode error: {}", e),
            }
        }
        Err(e) => {
            eprintln!("http error: {}", e);
            std::process::exit(1);
        }
    }
}
