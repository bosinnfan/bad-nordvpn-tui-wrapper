// nordtui - Terminal User Interface for NordVPN
// Copyright (C) 2026 bosinnfan
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE. See the
// GNU General Public License for more details.


//! nordtui — Interactive TUI menu that wraps the official NordVPN CLI.
//!
//! This program does NOT manage the VPN at the network level: it only
//! builds, runs (via `std::process::Command`) and displays the output of
//! the `nordvpn` binary already installed on the system.
//!
//! The list of countries and cities is not hardcoded: it's fetched live by
//! running `nordvpn countries` and `nordvpn cities <country>` and parsing
//! their output, so it always reflects what the CLI itself knows.

use std::{
    collections::HashSet,
    io::{self, Stdout},
    panic,
    process::Command,
    time::Duration,
};

use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap},
    Frame, Terminal,
};

mod palette {
    use ratatui::style::Color;

    pub const BAR_BG: Color = Color::DarkGray;
    pub const BAR_FG: Color = Color::White;
    pub const BORDER: Color = Color::DarkGray;
    pub const TEXT: Color = Color::Gray;
    pub const TEXT_BRIGHT: Color = Color::White;
    pub const TEXT_DIM: Color = Color::DarkGray;
    pub const HILITE_BG: Color = Color::Gray;
    pub const HILITE_FG: Color = Color::Black;
}

#[derive(PartialEq, Eq, Clone, Copy)]
enum Screen {
    MainMenu,
    CountryMenu,
    CityMenu,
}

struct CommandResult {
    title: String,
    stdout: String,
    stderr: String,
    success: bool,
}

impl CommandResult {
    fn empty() -> Self {
        Self {
            title: "No commands executed yet".to_string(),
            stdout: String::new(),
            stderr: String::new(),
            success: true,
        }
    }
}

const MAIN_MENU_ITEMS: &[&str] = &[
    "Connect to recommended location",
    "Connect to a specific country / city",
    "View current status",
    "Disconnect",
    "Kill Switch: enable",
    "Kill Switch: disable",
    "Threat Protection: enable",
    "Threat Protection: disable",
    "Quit",
];

const CONNECT_WITHOUT_CITY: &str = "— Connect directly to the country (no specific city) —";

/// Turns the raw output of `nordvpn countries` / `nordvpn cities` into a
/// clean, deduplicated list of tokens, preserving order of appearance.
/// The NordVPN CLI separates multi-word names with an underscore
/// (e.g. "United_States"), so splitting on commas/whitespace is safe.
fn parse_items(raw: &str) -> Vec<String> {
    let mut seen = HashSet::new();
    raw.split(|c: char| c == ',' || c.is_whitespace())
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .filter(|s| seen.insert(s.clone()))
        .collect()
}

/// Turns "United_States" into "United States" for display only;
/// the actual argument passed to the CLI keeps the original "_".
fn pretty(raw: &str) -> String {
    raw.replace('_', " ")
}

struct App {
    screen: Screen,
    main_state: ListState,
    country_state: ListState,
    city_state: ListState,

    countries: Vec<String>,
    countries_loaded: bool,

    cities: Vec<String>,
    selected_country: Option<String>,

    last_result: CommandResult,
    should_quit: bool,
}

impl App {
    fn new() -> Self {
        let mut main_state = ListState::default();
        main_state.select(Some(0));

        Self {
            screen: Screen::MainMenu,
            main_state,
            country_state: ListState::default(),
            city_state: ListState::default(),
            countries: Vec::new(),
            countries_loaded: false,
            cities: Vec::new(),
            selected_country: None,
            last_result: CommandResult::empty(),
            should_quit: false,
        }
    }

    fn current_len(&self) -> usize {
        match self.screen {
            Screen::MainMenu => MAIN_MENU_ITEMS.len(),
            Screen::CountryMenu => self.countries.len(),
            // +1 for the "connect without city" item at the top of the list.
            Screen::CityMenu => self.cities.len() + 1,
        }
    }

    fn current_state_mut(&mut self) -> &mut ListState {
        match self.screen {
            Screen::MainMenu => &mut self.main_state,
            Screen::CountryMenu => &mut self.country_state,
            Screen::CityMenu => &mut self.city_state,
        }
    }

    fn next(&mut self) {
        let len = self.current_len();
        if len == 0 {
            return;
        }
        let state = self.current_state_mut();
        let i = state.selected().unwrap_or(0);
        state.select(Some((i + 1) % len));
    }

    fn previous(&mut self) {
        let len = self.current_len();
        if len == 0 {
            return;
        }
        let state = self.current_state_mut();
        let i = state.selected().unwrap_or(0);
        state.select(Some((i + len - 1) % len));
    }

    fn run_nordvpn(&mut self, title: &str, args: &[&str]) {
        let output = Command::new("nordvpn").args(args).output();

        self.last_result = match output {
            Ok(out) => CommandResult {
                title: format!("{title}  →  $ nordvpn {}", args.join(" ")),
                stdout: String::from_utf8_lossy(&out.stdout).to_string(),
                stderr: String::from_utf8_lossy(&out.stderr).to_string(),
                success: out.status.success(),
            },
            Err(e) => CommandResult {
                title: format!("Failed to run ({title})"),
                stdout: String::new(),
                stderr: format!(
                    "Could not run the 'nordvpn' binary. Is it installed and in PATH?\nDetail: {e}"
                ),
                success: false,
            },
        };
    }

    fn fetch_countries(&mut self) {
        self.run_nordvpn("Query countries", &["countries"]);
        self.countries = parse_items(&self.last_result.stdout);
        self.countries_loaded = true;
        self.country_state
            .select(if self.countries.is_empty() { None } else { Some(0) });
    }

    fn fetch_cities(&mut self, country_arg: &str, country_label: &str) {
        self.run_nordvpn(
            &format!("Query cities for {country_label}"),
            &["cities", country_arg],
        );
        self.cities = parse_items(&self.last_result.stdout);
        // Default-select the "connect without city" item (index 0).
        self.city_state.select(Some(0));
    }

    /// Handles the Enter key for the active screen/selection. Takes the
    /// terminal because entering countries/cities draws a "loading" notice
    /// before blocking on the call to `nordvpn`.
    fn on_enter(&mut self, terminal: &mut Terminal<CrosstermBackend<Stdout>>) -> io::Result<()> {
        match self.screen {
            Screen::MainMenu => {
                let idx = self.main_state.selected().unwrap_or(0);
                match idx {
                    0 => self.run_nordvpn("Connect (recommended)", &["connect"]),
                    1 => {
                        if !self.countries_loaded {
                            show_loading(terminal, "Querying countries (nordvpn countries)...")?;
                            self.fetch_countries();
                        }
                        if !self.countries.is_empty() {
                            self.screen = Screen::CountryMenu;
                        }
                    }
                    2 => self.run_nordvpn("View status", &["status"]),
                    3 => self.run_nordvpn("Disconnect", &["disconnect"]),
                    4 => self.run_nordvpn("Kill Switch on", &["set", "killswitch", "on"]),
                    5 => self.run_nordvpn("Kill Switch off", &["set", "killswitch", "off"]),
                    6 => self.run_nordvpn(
                        "Threat Protection on",
                        &["set", "threatprotectionlite", "on"],
                    ),
                    7 => self.run_nordvpn(
                        "Threat Protection off",
                        &["set", "threatprotectionlite", "off"],
                    ),
                    8 => self.should_quit = true,
                    _ => {}
                }
            }
            Screen::CountryMenu => {
                let idx = self.country_state.selected().unwrap_or(0);
                if let Some(country_arg) = self.countries.get(idx).cloned() {
                    let label = pretty(&country_arg);
                    show_loading(terminal, &format!("Querying cities for {label}..."))?;
                    self.selected_country = Some(country_arg.clone());
                    self.fetch_cities(&country_arg, &label);
                    self.screen = Screen::CityMenu;
                }
            }
            Screen::CityMenu => {
                let idx = self.city_state.selected().unwrap_or(0);
                let Some(country_arg) = self.selected_country.clone() else {
                    self.screen = Screen::MainMenu;
                    return Ok(());
                };
                let country_label = pretty(&country_arg);

                if idx == 0 {
                    self.run_nordvpn(
                        &format!("Connect to {country_label}"),
                        &["connect", &country_arg],
                    );
                } else if let Some(city_arg) = self.cities.get(idx - 1).cloned() {
                    let city_label = pretty(&city_arg);
                    self.run_nordvpn(
                        &format!("Connect to {city_label}, {country_label}"),
                        &["connect", &country_arg, &city_arg],
                    );
                }
                self.screen = Screen::MainMenu;
            }
        }
        Ok(())
    }

    fn on_refresh(&mut self, terminal: &mut Terminal<CrosstermBackend<Stdout>>) -> io::Result<()> {
        match self.screen {
            Screen::CountryMenu => {
                show_loading(terminal, "Refreshing country list...")?;
                self.fetch_countries();
            }
            Screen::CityMenu => {
                if let Some(country_arg) = self.selected_country.clone() {
                    let label = pretty(&country_arg);
                    show_loading(terminal, &format!("Refreshing cities for {label}..."))?;
                    self.fetch_cities(&country_arg, &label);
                }
            }
            Screen::MainMenu => {}
        }
        Ok(())
    }

    fn on_back(&mut self) {
        self.screen = match self.screen {
            Screen::CityMenu => Screen::CountryMenu,
            Screen::CountryMenu => Screen::MainMenu,
            Screen::MainMenu => Screen::MainMenu,
        };
    }
}

fn main() -> io::Result<()> {
    // Make sure the terminal is restored no matter what happens, panic included.
    install_panic_hook();

    let mut terminal = setup_terminal()?;
    let app_result = run_app(&mut terminal);
    restore_terminal(&mut terminal)?;

    if let Err(err) = app_result {
        eprintln!("Error in nordtui: {err}");
    }

    Ok(())
}

fn setup_terminal() -> io::Result<Terminal<CrosstermBackend<Stdout>>> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    Terminal::new(backend)
}

fn restore_terminal(terminal: &mut Terminal<CrosstermBackend<Stdout>>) -> io::Result<()> {
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;
    Ok(())
}

/// Installs a panic hook that restores the terminal before printing the
/// panic, so the user's shell isn't left in raw/alternate-screen mode.
fn install_panic_hook() {
    let original_hook = panic::take_hook();
    panic::set_hook(Box::new(move |panic_info| {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen, DisableMouseCapture);
        original_hook(panic_info);
    }));
}

fn show_loading(terminal: &mut Terminal<CrosstermBackend<Stdout>>, message: &str) -> io::Result<()> {
    terminal.draw(|f| {
        let area = f.size();
        f.render_widget(Clear, area);
        let block = Paragraph::new(message)
            .style(Style::default().fg(palette::TEXT_BRIGHT))
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(palette::BORDER))
                    .title(" Loading "),
            );
        f.render_widget(block, area);
    })?;
    Ok(())
}

fn run_app(terminal: &mut Terminal<CrosstermBackend<Stdout>>) -> io::Result<()> {
    let mut app = App::new();

    loop {
        terminal.draw(|f| ui(f, &app))?;

        // Non-blocking poll: keeps the UI responsive and leaves room for
        // future periodic refreshes without freezing on input.
        if event::poll(Duration::from_millis(100))? {
            if let Event::Key(key) = event::read()? {
                // Some platforms emit both Press and Release events; only
                // act on the actual key press.
                if key.kind != KeyEventKind::Press {
                    continue;
                }

                match key.code {
                    KeyCode::Char('q') => {
                        if app.screen == Screen::MainMenu {
                            break;
                        } else {
                            app.on_back();
                        }
                    }
                    KeyCode::Char('c')
                        if key
                            .modifiers
                            .contains(crossterm::event::KeyModifiers::CONTROL) =>
                    {
                        break;
                    }
                    KeyCode::Esc => app.on_back(),
                    KeyCode::Down | KeyCode::Char('j') => app.next(),
                    KeyCode::Up | KeyCode::Char('k') => app.previous(),
                    KeyCode::Enter => app.on_enter(terminal)?,
                    KeyCode::Char('r') => app.on_refresh(terminal)?,
                    _ => {}
                }
            }
        }

        if app.should_quit {
            break;
        }
    }

    Ok(())
}

fn ui(f: &mut Frame, app: &App) {
    let size = f.size();

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // title
            Constraint::Min(8),    // body: menu + output panel
            Constraint::Length(3), // key hints
        ])
        .split(size);

    draw_title(f, chunks[0], app);
    draw_body(f, chunks[1], app);
    draw_help(f, chunks[2], app);
}

fn draw_title(f: &mut Frame, area: Rect, app: &App) {
    let subtitle = match app.screen {
        Screen::MainMenu => "main menu".to_string(),
        Screen::CountryMenu => "choose country".to_string(),
        Screen::CityMenu => {
            let label = app
                .selected_country
                .as_deref()
                .map(pretty)
                .unwrap_or_default();
            format!("choose city — {label}")
        }
    };

    let title = Paragraph::new(Line::from(vec![
        Span::styled(
            "  nordtui ",
            Style::default()
                .fg(palette::BAR_FG)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("— {subtitle}"),
            Style::default().fg(palette::BAR_FG),
        ),
    ]))
    .style(Style::default().bg(palette::BAR_BG))
    .block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(palette::BORDER)),
    );
    f.render_widget(title, area);
}

fn draw_body(f: &mut Frame, area: Rect, app: &App) {
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(40), Constraint::Percentage(60)])
        .split(area);

    match app.screen {
        Screen::MainMenu => draw_list(
            f,
            cols[0],
            " Menu ",
            MAIN_MENU_ITEMS.iter().map(|s| s.to_string()),
            &app.main_state,
        ),
        Screen::CountryMenu => draw_list(
            f,
            cols[0],
            " Countries (Enter: cities · r: reload · Esc: back) ",
            app.countries.iter().map(|c| pretty(c)),
            &app.country_state,
        ),
        Screen::CityMenu => {
            let items = std::iter::once(CONNECT_WITHOUT_CITY.to_string())
                .chain(app.cities.iter().map(|c| pretty(c)));
            draw_list(
                f,
                cols[0],
                " Cities (Enter: connect · r: reload · Esc: back) ",
                items,
                &app.city_state,
            )
        }
    }

    draw_output_panel(f, cols[1], app);
}

fn draw_list(
    f: &mut Frame,
    area: Rect,
    title: &str,
    entries: impl Iterator<Item = String>,
    state: &ListState,
) {
    let items: Vec<ListItem> = entries
        .map(|s| ListItem::new(Line::from(s)).style(Style::default().fg(palette::TEXT)))
        .collect();

    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(palette::BORDER))
                .title(title),
        )
        .highlight_style(
            Style::default()
                .bg(palette::HILITE_BG)
                .fg(palette::HILITE_FG)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("➤ ");

    let mut state = state.clone();
    f.render_stateful_widget(list, area, &mut state);
}

fn draw_output_panel(f: &mut Frame, area: Rect, app: &App) {
    let result = &app.last_result;

    let mut lines: Vec<Line> = Vec::new();

    let status_span = if result.success {
        Span::styled(
            "[OK] ",
            Style::default()
                .fg(palette::TEXT_BRIGHT)
                .add_modifier(Modifier::BOLD),
        )
    } else {
        Span::styled(
            "[ERROR] ",
            Style::default()
                .fg(palette::TEXT_BRIGHT)
                .bg(palette::TEXT_DIM)
                .add_modifier(Modifier::BOLD),
        )
    };
    lines.push(Line::from(vec![
        status_span,
        Span::styled(
            result.title.clone(),
            Style::default()
                .fg(palette::TEXT_BRIGHT)
                .add_modifier(Modifier::BOLD),
        ),
    ]));
    lines.push(Line::from(""));

    if !result.stdout.trim().is_empty() {
        lines.push(Line::from(Span::styled(
            "stdout:",
            Style::default()
                .fg(palette::TEXT_DIM)
                .add_modifier(Modifier::ITALIC),
        )));
        for line in result.stdout.lines() {
            lines.push(Line::from(Span::styled(
                line.to_string(),
                Style::default().fg(palette::TEXT),
            )));
        }
        lines.push(Line::from(""));
    }

    if !result.stderr.trim().is_empty() {
        lines.push(Line::from(Span::styled(
            "stderr:",
            Style::default()
                .fg(palette::TEXT_DIM)
                .add_modifier(Modifier::ITALIC),
        )));
        for line in result.stderr.lines() {
            lines.push(Line::from(Span::styled(
                line.to_string(),
                Style::default()
                    .fg(palette::TEXT_BRIGHT)
                    .add_modifier(Modifier::BOLD),
            )));
        }
    }

    if result.stdout.trim().is_empty() && result.stderr.trim().is_empty() {
        lines.push(Line::from(Span::styled(
            "(no output)",
            Style::default().fg(palette::TEXT_DIM),
        )));
    }

    let paragraph = Paragraph::new(lines)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(palette::BORDER))
                .title(" Command Result "),
        )
        .wrap(Wrap { trim: false });

    f.render_widget(paragraph, area);
}

fn draw_help(f: &mut Frame, area: Rect, app: &App) {
    let text = match app.screen {
        Screen::MainMenu => "↑/↓ or j/k: move  |  Enter: select  |  q: quit  |  Ctrl+C: force quit",
        Screen::CountryMenu => {
            "↑/↓ or j/k: move  |  Enter: view cities  |  r: reload  |  Esc/q: back  |  Ctrl+C: force quit"
        }
        Screen::CityMenu => {
            "↑/↓ or j/k: move  |  Enter: connect  |  r: reload  |  Esc/q: back  |  Ctrl+C: force quit"
        }
    };
    let help = Paragraph::new(text)
        .style(Style::default().fg(palette::TEXT_DIM))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(palette::BORDER)),
        );
    f.render_widget(help, area);
}
