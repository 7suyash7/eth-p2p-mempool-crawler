use anyhow::Result;
use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event as CrosstermEvent, KeyCode},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use num_format::{Locale, ToFormattedString};
use ratatui::{
    Frame, Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style, Stylize},
    text::{Line, Span},
    widgets::{Block, Borders, Cell, List, ListItem, Paragraph, Row, Table, TableState},
};
use reth::revm::revm::primitives::U256;
use reth_primitives::TxType;
use std::{
    collections::VecDeque,
    io::{self},
    time::{Duration, Instant},
};
use tokio::sync::mpsc::UnboundedReceiver;
use tracing::debug;

use crate::{
    analysis::TxAnalysisResult,
    types::{PeerInfo, UiUpdate},
};

#[derive(Debug)]
struct AppState {
    peers: Vec<PeerInfo>,
    recent_txs: VecDeque<TxAnalysisResult>,
    table_state: TableState,
    total_txs_seen: u64,
    legacy_tx_count: u64,
    eip1559_tx_count: u64,
    eip2930_tx_count: u64,
    eip4844_tx_count: u64,
    eip_7702_tx_count: u64,
    last_update: Instant,
    should_quit: bool,
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            peers: Vec::new(),
            recent_txs: VecDeque::with_capacity(MAX_RECENT_TXS),
            table_state: TableState::default(),
            total_txs_seen: 0,
            legacy_tx_count: 0,
            eip1559_tx_count: 0,
            eip2930_tx_count: 0,
            eip4844_tx_count: 0,
            eip_7702_tx_count: 0,
            last_update: Instant::now(),
            should_quit: false,
        }
    }
}

const MAX_RECENT_TXS: usize = 50;

pub async fn run_ui(mut ui_rx: UnboundedReceiver<UiUpdate>) -> Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let original_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |panic_info| {
        let mut stdout = io::stdout();
        _ = execute!(stdout, LeaveAlternateScreen, DisableMouseCapture);
        _ = disable_raw_mode();
        original_hook(panic_info);
    }));

    let mut app_state = AppState::default();
    app_state.last_update = Instant::now();

    loop {
        terminal.draw(|f| draw_frame(f, &mut app_state))?; // Pass mutable state

        if event::poll(Duration::from_millis(50))? {
            if let CrosstermEvent::Key(key) = event::read()? {
                match key.code {
                    KeyCode::Char('q') => app_state.should_quit = true,
                    KeyCode::Down => {
                        let i = match app_state.table_state.selected() {
                            Some(i) => {
                                if i >= app_state.recent_txs.len().saturating_sub(1) {
                                    0
                                } else {
                                    i + 1
                                }
                            }
                            None => 0,
                        };
                        if !app_state.recent_txs.is_empty() {
                            app_state.table_state.select(Some(i));
                        }
                    }
                    KeyCode::Up => {
                        let i = match app_state.table_state.selected() {
                            Some(i) => {
                                if i == 0 {
                                    app_state.recent_txs.len().saturating_sub(1)
                                } else {
                                    i - 1
                                }
                            }
                            None => 0.max(app_state.recent_txs.len().saturating_sub(1)),
                        };
                        if !app_state.recent_txs.is_empty() {
                            app_state.table_state.select(Some(i));
                        }
                    }
                    _ => {}
                }
            }
        }

        while let Ok(update) = ui_rx.try_recv() {
            match update {
                UiUpdate::PeerUpdate(data) => {
                    debug!(target: "crawler::ui::receiver", "UI Received PeerUpdate: {} peers", data.connected_peers.len());
                    app_state.peers = data.connected_peers;
                }
                UiUpdate::NewTx(boxed_analysis_result) => {
                    let analysis_result = *boxed_analysis_result;
                    debug!(target: "crawler::ui::receiver", "UI Received NewTx: {}", analysis_result.hash);

                    app_state.total_txs_seen += 1;
                    match analysis_result.tx_type {
                        TxType::Legacy => app_state.legacy_tx_count += 1,
                        TxType::Eip1559 => app_state.eip1559_tx_count += 1,
                        TxType::Eip2930 => app_state.eip2930_tx_count += 1,
                        TxType::Eip4844 => app_state.eip4844_tx_count += 1,
                        TxType::Eip7702 => app_state.eip_7702_tx_count += 1,
                    }

                    app_state.recent_txs.push_front(analysis_result);
                    if app_state.recent_txs.len() > MAX_RECENT_TXS {
                        app_state.recent_txs.pop_back();
                    }
                    if app_state.table_state.selected().is_none()
                        && !app_state.recent_txs.is_empty()
                    {
                        app_state.table_state.select(Some(0));
                    }
                }
                UiUpdate::Shutdown => {
                    debug!(target: "crawler::ui::receiver", "UI Received Shutdown");
                    app_state.should_quit = true;
                }
            }
            app_state.last_update = Instant::now();
        }

        if app_state.should_quit {
            break;
        }
    }

    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;

    Ok(())
}

fn draw_frame(f: &mut Frame, app_state: &mut AppState) {
    let main_chunks = Layout::default()
        .direction(Direction::Vertical)
        .margin(1)
        .constraints(
            [
                Constraint::Length(10),
                Constraint::Length(5),
                Constraint::Min(0),
            ]
            .as_ref(),
        )
        .split(f.area());

    // --- 1. Stats Panel ---
    let stats_content = vec![
        Line::from(Span::styled("Total Txs Seen:", Style::default().bold())),
        Line::from(format!(
            "  {}",
            app_state.total_txs_seen.to_formatted_string(&Locale::en)
        )),
        Line::from(Span::styled("Breakdown:", Style::default().bold())),
        Line::from(format!(
            "  Legacy:   {}",
            app_state.legacy_tx_count.to_formatted_string(&Locale::en)
        )),
        Line::from(format!(
            "  EIP-1559: {}",
            app_state.eip1559_tx_count.to_formatted_string(&Locale::en)
        )),
        Line::from(format!(
            "  EIP-2930: {}",
            app_state.eip2930_tx_count.to_formatted_string(&Locale::en)
        )),
        Line::from(format!(
            "  EIP-4844: {}",
            app_state.eip4844_tx_count.to_formatted_string(&Locale::en)
        )),
        Line::from(format!(
            "  EIP-7702: {}",
            app_state.eip_7702_tx_count.to_formatted_string(&Locale::en)
        )),
    ];
    let stats_paragraph = Paragraph::new(stats_content)
        .block(Block::default().title("📊 Stats").borders(Borders::ALL));
    f.render_widget(stats_paragraph, main_chunks[0]);

    // --- 2. Peer Panel ---
    let peer_items: Vec<ListItem> = app_state
        .peers
        .iter()
        .map(|p| {
            let peer_id_short = format!("{:#}", p.id).chars().take(12).collect::<String>() + "...";
            let client_short = p.client_version.chars().take(40).collect::<String>();
            ListItem::new(format!("{} ({})", peer_id_short, client_short))
        })
        .collect();

    let peers_list = List::new(peer_items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(format!("🔗 Peers ({})", app_state.peers.len())),
        )
        .highlight_style(
            Style::default()
                .add_modifier(Modifier::BOLD)
                .bg(Color::DarkGray),
        )
        .highlight_symbol(">> ");

    f.render_widget(peers_list, main_chunks[1]);

    // --- 3. Transaction Table ---
    let header_cells = [
        "Hash",
        "Type",
        "Sender",
        "Receiver",
        "Value (Gwei)",
        "Prio(G)",
        "MaxFee(G)",
    ]
    .iter()
    .map(|h| Cell::from(*h).style(Style::default().fg(Color::Yellow).bold()));
    let header = Row::new(header_cells)
        .style(Style::default().bg(Color::DarkGray))
        .height(1)
        .bottom_margin(1);

    let rows = app_state.recent_txs.iter().map(|tx| {
        let hash_short = format!("{:#x}", tx.hash)
            .chars()
            .take(10)
            .collect::<String>()
            + "...";
        let sender = tx.sender.map_or_else(
            || "N/A".to_string(),
            |a| format!("{:#x}", a).chars().take(10).collect::<String>() + "...",
        );
        let receiver = tx.receiver.map_or_else(
            || "Create".to_string(),
            |a| format!("{:#x}", a).chars().take(10).collect::<String>() + "...",
        );

        let gwei_divisor_u256 = U256::from(1_000_000_000);
        let value_gwei = tx.value / gwei_divisor_u256;
        let value_gwei_str = value_gwei.to_string();

        let gwei_divisor_u128 = 1_000_000_000u128;
        let gas_price_gwei_str = tx
            .gas_price_or_max_fee
            .map(|p_wei| (p_wei / gwei_divisor_u128).to_formatted_string(&Locale::en))
            .unwrap_or_else(|| "N/A".to_string());
        let gas_prio_gwei_str = tx
            .max_priority_fee
            .map(|p_wei| (p_wei / gwei_divisor_u128).to_formatted_string(&Locale::en))
            .unwrap_or_else(|| "-".to_string());

        Row::new(vec![
            Cell::from(hash_short),
            Cell::from(format!("{:?}", tx.tx_type)),
            Cell::from(sender),
            Cell::from(receiver),
            Cell::from(value_gwei_str),
            Cell::from(gas_prio_gwei_str),
            Cell::from(gas_price_gwei_str),
        ])
    });

    let table = Table::new(
        rows,
        &[
            Constraint::Percentage(12),
            Constraint::Percentage(8),
            Constraint::Percentage(15),
            Constraint::Percentage(15),
            Constraint::Percentage(15),
            Constraint::Percentage(15),
            Constraint::Percentage(20),
        ],
    )
    .header(header)
    .block(
        Block::default()
            .borders(Borders::ALL)
            .title("📈 Recent Transactions"),
    )
    .row_highlight_style(Style::default().add_modifier(Modifier::REVERSED))
    .highlight_symbol(">> ");

    f.render_stateful_widget(table, main_chunks[2], &mut app_state.table_state);
}
