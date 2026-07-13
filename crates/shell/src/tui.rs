//! Live book view: depth ladder, last trades, and a rolling latency
//! readout while the agent simulation hammers the engine in real time.
//!
//! `tessera tui --seed 42` — press `q` to quit.

use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;
use std::collections::VecDeque;
use std::io;
use std::time::{Duration, Instant};
use tessera_core::Side;
use tessera_sim::runner::Sim;
use tessera_sim::SimConfig;

const LADDER_ROWS: usize = 12;
const MAX_SAMPLES: usize = 50_000;

pub fn run(sc: SimConfig) -> io::Result<()> {
    let mut sim = Sim::new(sc);
    let mut terminal = ratatui::init();
    let mut samples: VecDeque<u64> = VecDeque::with_capacity(MAX_SAMPLES);
    let mut total_events = 0u64;
    let started = Instant::now();

    let result = loop {
        // Drive the market for ~15ms per frame; sample per-event latency
        // per tick (Instant around the whole tick, divided by its events —
        // display-grade, not benchmark-grade).
        let frame_start = Instant::now();
        while frame_start.elapsed() < Duration::from_millis(15) {
            let t0 = Instant::now();
            let n = sim.tick(None);
            if n > 0 {
                let per_event = t0.elapsed().as_nanos() as u64 / n;
                for _ in 0..n.min(32) {
                    if samples.len() == MAX_SAMPLES {
                        samples.pop_front();
                    }
                    samples.push_back(per_event);
                }
                total_events += n;
            }
        }

        terminal.draw(|f| draw(f, &sim, &samples, total_events, started))?;

        if event::poll(Duration::ZERO)? {
            if let Event::Key(k) = event::read()? {
                if k.kind == KeyEventKind::Press
                    && matches!(k.code, KeyCode::Char('q') | KeyCode::Esc)
                {
                    break Ok(());
                }
            }
        }
    };
    ratatui::restore();
    result
}

fn draw(f: &mut Frame, sim: &Sim, samples: &VecDeque<u64>, total_events: u64, started: Instant) {
    let [header, body, footer] = Layout::vertical([
        Constraint::Length(4),
        Constraint::Min(8),
        Constraint::Length(6),
    ])
    .areas(f.area());

    // ---- Header: throughput + microstructure ----
    let elapsed = started.elapsed().as_secs_f64().max(1e-9);
    let s = &sim.stats;
    let header_text = vec![
        Line::from(vec![
            Span::styled(
                " TESSERA ",
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(format!(
                "  {total_events} events  |  {:.2}M events/s  |  live orders {}  |  q quits",
                total_events as f64 / elapsed / 1e6,
                sim.book.live_count(),
            )),
        ]),
        Line::from(format!(
            " cancel rate {:>5.1}%   order-to-trade {:>5.1}:1   fills {}   rejects {}",
            100.0 * s.cancel_rate(),
            s.order_to_trade(),
            s.fills,
            s.rejects,
        )),
    ];
    f.render_widget(
        Paragraph::new(header_text).block(Block::default().borders(Borders::ALL)),
        header,
    );

    // ---- Body: bid and ask ladders ----
    let [bid_area, ask_area] =
        Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)]).areas(body);
    render_ladder(f, bid_area, sim, Side::Bid);
    render_ladder(f, ask_area, sim, Side::Ask);

    // ---- Footer: latency + last trades ----
    let [lat_area, trades_area] =
        Layout::horizontal([Constraint::Percentage(55), Constraint::Percentage(45)]).areas(footer);

    let mut sorted: Vec<u64> = samples.iter().copied().collect();
    sorted.sort_unstable();
    let pct = |q: f64| -> u64 {
        if sorted.is_empty() {
            0
        } else {
            sorted[((sorted.len() - 1) as f64 * q) as usize]
        }
    };
    let lat = Paragraph::new(vec![
        Line::from(format!(
            " p50 {:>6} ns   p99 {:>6} ns   p99.9 {:>7} ns",
            pct(0.50),
            pct(0.99),
            pct(0.999)
        )),
        Line::from(Span::styled(
            " per-event estimate over a rolling window (display-grade)",
            Style::default().fg(Color::DarkGray),
        )),
    ])
    .block(Block::default().borders(Borders::ALL).title(" latency "));
    f.render_widget(lat, lat_area);

    let trades: Vec<Line> = sim
        .recent_trades
        .iter()
        .rev()
        .take(4)
        .map(|(p, q)| Line::from(format!(" {q:>6} @ {p}")))
        .collect();
    f.render_widget(
        Paragraph::new(trades).block(
            Block::default()
                .borders(Borders::ALL)
                .title(" last trades "),
        ),
        trades_area,
    );
}

fn render_ladder(f: &mut Frame, area: Rect, sim: &Sim, side: Side) {
    let levels = sim.book.depth(side, LADDER_ROWS);
    let max_qty = levels.iter().map(|l| l.1).max().unwrap_or(1).max(1);
    let bar_width = (area.width.saturating_sub(24)) as u128;
    let color = match side {
        Side::Bid => Color::Green,
        Side::Ask => Color::Red,
    };
    let lines: Vec<Line> = levels
        .iter()
        .map(|(price, qty, count)| {
            let w = ((qty * bar_width) / max_qty) as usize;
            Line::from(vec![
                Span::raw(format!(" {:>7} {:>8} ", price.0, qty)),
                Span::styled("█".repeat(w), Style::default().fg(color)),
                Span::styled(format!(" {count}"), Style::default().fg(Color::DarkGray)),
            ])
        })
        .collect();
    let title = match side {
        Side::Bid => " bids (best first) ",
        Side::Ask => " asks (best first) ",
    };
    f.render_widget(
        Paragraph::new(lines).block(Block::default().borders(Borders::ALL).title(title)),
        area,
    );
}
