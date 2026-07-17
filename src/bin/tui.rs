#[path = "../hrm.rs"]
mod hrm;

use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend, layout::{Constraint, Direction, Layout, Rect, Alignment}, style::{Color, Modifier, Style},
    text::{Line, Span}, widgets::{Block, Borders, Clear, Paragraph, Sparkline, Wrap}, Terminal,
};
use std::io;
use std::time::{Duration, Instant};
use tokio::net::TcpStream;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::mpsc;

fn centered_rect(percent_x: u16, percent_y: u16, r: Rect) -> Rect {
    let popup_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage((100 - percent_y) / 2), Constraint::Percentage(percent_y), Constraint::Percentage((100 - percent_y) / 2)].as_ref())
        .split(r);

    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage((100 - percent_x) / 2), Constraint::Percentage(percent_x), Constraint::Percentage((100 - percent_x) / 2)].as_ref())
        .split(popup_layout[1])[1]
}

#[tokio::main]
async fn main() -> Result<(), io::Error> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let (tx, mut rx) = mpsc::channel::<hrm::DaemonPayload>(100);

    tokio::spawn(async move {
        loop {
            if let Ok(stream) = TcpStream::connect("127.0.0.1:8080").await {
                let mut reader = BufReader::new(stream);
                let mut line = String::new();
                while let Ok(bytes_read) = reader.read_line(&mut line).await {
                    if bytes_read == 0 { break; } 
                    if let Ok(data) = serde_json::from_str::<hrm::DaemonPayload>(&line) {
                        let _ = tx.send(data).await;
                    }
                    line.clear();
                }
            }
            tokio::time::sleep(Duration::from_secs(2)).await; 
        }
    });

    let mut latest_payload: Option<hrm::DaemonPayload> = None;
    let mut rmssd_history: Vec<u64> = vec![];
    let mut stress_history: Vec<u64> = vec![];
    let mut show_manual = false;

    let mut session_start = Instant::now();
    let mut last_packet_time = Instant::now();

    loop {
        terminal.draw(|f| {
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .margin(1)
                .constraints([Constraint::Length(3), Constraint::Min(20), Constraint::Length(3)].as_ref())
                .split(f.size());

            let is_connected = latest_payload.is_some() && last_packet_time.elapsed().as_secs() < 5;

            let d_state = if let Some(ref p) = latest_payload { 
                if is_connected {
                    p.daemon_state.clone()
                } else {
                    "OFFLINE".into()
                }
            } else { 
                "OFFLINE".into() 
            };

            let state_color = match d_state.as_str() {
                "RUNNING" => Color::Green,
                "PAUSED" => Color::Yellow,
                _ => Color::DarkGray,
            };

            let elapsed_secs = if is_connected && d_state == "RUNNING" {
                session_start.elapsed().as_secs()
            } else {
                0 
            };

            let timer_str = format!("{:02}:{:02}", elapsed_secs / 60, elapsed_secs % 60);
            let timer_color = if elapsed_secs >= 300 { Color::Green } else { Color::Yellow };

            let header = Paragraph::new(Line::from(vec![
                Span::styled(" GIBSON TUI v2.3  //  READ-ONLY UPLINK ", Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)),
                Span::styled(format!("  |  DAEMON: {} ", d_state), Style::default().fg(state_color).add_modifier(Modifier::RAPID_BLINK)),
                Span::styled(format!("  |  SESSION: {} ", timer_str), Style::default().fg(timer_color)),
                Span::styled("  |  [M] MANUAL ", Style::default().fg(Color::Yellow)),
            ])).block(Block::default().borders(Borders::ALL).border_style(Style::default().fg(Color::Green)));
            f.render_widget(header, chunks[0]);

            if is_connected {
                if let Some(ref payload) = latest_payload {
                    if let Some(ref data) = payload.telemetry {
                        let dashboard_chunks = Layout::default()
                            .direction(Direction::Horizontal)
                            .constraints([Constraint::Percentage(50), Constraint::Percentage(50)].as_ref())
                            .split(chunks[1]);

                        let left_panel = Layout::default()
                            .direction(Direction::Vertical)
                            .constraints([Constraint::Min(10), Constraint::Length(6)].as_ref())
                            .split(dashboard_chunks[0]);

                        let right_panel = Layout::default()
                            .direction(Direction::Vertical)
                            .constraints([Constraint::Min(10), Constraint::Length(6)].as_ref())
                            .split(dashboard_chunks[1]);

                        let anomaly_color = if data.anomalies > 0 { Color::Red } else { Color::DarkGray };
                        let anomaly_text = if data.anomalies > 0 { format!("{} DETECTED", data.anomalies) } else { "CLEAR".to_string() };

                        let op_lines = vec![
                            Line::from(vec![Span::styled(format!("HEART RATE:       {:03} BPM", data.bpm), Style::default().fg(Color::White).add_modifier(Modifier::BOLD))]),
                            Line::from(vec![Span::styled(format!("HR ZONE:          {}", data.hr_zone), Style::default().fg(Color::Cyan))]),
                            Line::from(""),
                            Line::from(vec![Span::styled(format!("RMSSD (FLEX):     {:05.1} ms -> {}", data.operative.rmssd, data.operative.rmssd_status), Style::default().fg(Color::Yellow))]),
                            Line::from(vec![Span::styled(format!("pNN50:            {:05.1} %  -> {}", data.operative.pnn50, data.operative.pnn50_status), Style::default().fg(Color::Yellow))]),
                            Line::from(""),
                            Line::from(vec![
                                Span::styled("ANOMALIES (ECT):  ", Style::default().fg(anomaly_color)),
                                Span::styled(anomaly_text, Style::default().fg(anomaly_color).add_modifier(Modifier::BOLD)),
                            ]),
                        ];
                        let op_block = Paragraph::new(op_lines).block(Block::default().title(" OPERATIVE (1 MIN) ").borders(Borders::ALL).border_style(Style::default().fg(Color::LightBlue)));
                        f.render_widget(op_block, left_panel[0]);

                        let rmssd_view = &rmssd_history[rmssd_history.len().saturating_sub(50)..];
                        let rmssd_spark = Sparkline::default().block(Block::default().title(" RMSSD TREND ").borders(Borders::ALL).border_style(Style::default().fg(Color::DarkGray))).data(rmssd_view).style(Style::default().fg(Color::Yellow));
                        f.render_widget(rmssd_spark, left_panel[1]);

                        let base_lines = vec![
                            Line::from(vec![Span::styled(format!("STRESS INDEX:     {:06.1} -> {}", data.baseline.baevsky_index, data.baseline.stress_status), Style::default().fg(Color::LightRed))]),
                            Line::from(""),
                            Line::from(vec![Span::styled(format!("SDNN (RESIDUAL):  {:05.1} ms -> {}", data.baseline.sdnn, data.baseline.sdnn_status), Style::default().fg(Color::Yellow))]),
                            Line::from(vec![Span::styled(format!("CV (STABILITY):   {:05.1} %  -> {}", data.baseline.cv, data.baseline.cv_status), Style::default().fg(Color::Yellow))]),
                        ];
                        let base_block = Paragraph::new(base_lines).block(Block::default().title(" BASELINE (5 MIN TARGET) ").borders(Borders::ALL).border_style(Style::default().fg(Color::Green)));
                        f.render_widget(base_block, right_panel[0]);

                        let stress_view = &stress_history[stress_history.len().saturating_sub(50)..];
                        let stress_spark = Sparkline::default().block(Block::default().title(" STRESS TREND ").borders(Borders::ALL).border_style(Style::default().fg(Color::DarkGray))).data(stress_view).style(Style::default().fg(Color::LightRed));
                        f.render_widget(stress_spark, right_panel[1]);
                    } else {
                        let wait_block = Paragraph::new(format!("SYS_MSG: {}", payload.sys_msg)).alignment(Alignment::Center).block(Block::default().borders(Borders::ALL).border_style(Style::default().fg(Color::DarkGray)));
                        f.render_widget(wait_block, chunks[1]);
                    }
                }
            } else {
                let wait_block = Paragraph::new("AWAITING TCP TELEMETRY FROM DAEMON...").alignment(Alignment::Center).block(Block::default().borders(Borders::ALL).border_style(Style::default().fg(Color::DarkGray)));
                f.render_widget(wait_block, chunks[1]);
            }

            let footer = Paragraph::new("[Q]uit | TCP :8080 | CONTROL VIA PORT :8081").style(Style::default().fg(Color::Green)).block(Block::default().borders(Borders::ALL));
            f.render_widget(footer, chunks[2]);

            if show_manual {
                let popup_area = centered_rect(80, 80, f.size());
                f.render_widget(Clear, popup_area);
                let manual_text = vec![
                    Line::from(Span::styled("УПРАВЛЕНИЕ ДЕМОНОМ (ТЕРМИНАЛ: nc 127.0.0.1 8081):", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD))),
                    Line::from("  PAUSE | RESUME | RESET | PURGE | LOGS | SHUTDOWN"),
                    Line::from(""),
                    Line::from(Span::styled("RMSSD (Оперативная гибкость):", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD))),
                    Line::from("  < 20 ms   : CRITICAL (Симпатика перегружена. Жесткий стресс или боль)"),
                    Line::from("  20-30 ms  : WARNING (Фоновое напряжение. Боевая готовность)"),
                    Line::from("  30-50 ms  : OPTIMAL (Здоровый баланс. Адекватная реакция)"),
                    Line::from("  > 50 ms   : RELAXED (Доминирует парасимпатика. Расслабление/сон)"),
                    Line::from(""),
                    Line::from(Span::styled("ИНДЕКС БАЕВСКОГО (Цена адаптации):", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD))),
                    Line::from("  > 200     : OVERFATIGUE (Опасный износ. Сжигание резервов вхолостую)"),
                    Line::from("  150-200   : HIGH STRESS (Работа на пределе возможностей)"),
                    Line::from("  50-150    : NORMAL ADAPT (Штатная рабочая нагрузка активного дня)"),
                    Line::from("  0-50      : RELAXED (Зона полного физиологического покоя)"),
                    Line::from(""),
                    Line::from(Span::styled("SDNN (Остаточный ресурс / Батарейка):", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD))),
                    Line::from("  < 30 ms   : RIGID (ЦНС истощена, требуется сон)"),
                    Line::from("  30-50 ms  : NORMAL (Стандартный рабочий диапазон)"),
                    Line::from("  > 50 ms   : HIGH (Готовность к тяжелым задачам)"),
                    Line::from(""),
                    Line::from(Span::styled("CV (Стабильность ритма):", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD))),
                    Line::from("  < 2 %     : RIGID (Пульс зажат стрессом как метроном)"),
                    Line::from("  2-6 %     : NORMAL (Здоровая нестабильность ритма)"),
                    Line::from("  > 6 %     : CHAOTIC (Возможны сбои или аномалии)"),
                ];
                let popup_block = Paragraph::new(manual_text).block(Block::default().title(" MATRIX MANUAL ").borders(Borders::ALL).border_style(Style::default().fg(Color::Cyan))).wrap(Wrap { trim: true });
                f.render_widget(popup_block, popup_area);
            }
        })?;

        if let Ok(payload) = rx.try_recv() {
            if last_packet_time.elapsed().as_secs() >= 5 {
                session_start = Instant::now();
            }
            last_packet_time = Instant::now();

            if let Some(ref data) = payload.telemetry {
                rmssd_history.push(data.operative.rmssd as u64);
                if rmssd_history.len() > 100 { rmssd_history.remove(0); } 
                stress_history.push(data.baseline.baevsky_index as u64);
                if stress_history.len() > 100 { stress_history.remove(0); }
            }
            latest_payload = Some(payload);
        }

        if event::poll(Duration::from_millis(50))? {
            if let Event::Key(key) = event::read()? {
                match key.code {
                    KeyCode::Char('q') | KeyCode::Esc => break,
                    KeyCode::Char('m') | KeyCode::Char('M') | KeyCode::Char('ь') | KeyCode::Char('Ь') => { show_manual = !show_manual; }
                    _ => {}
                }
            }
        }
    }

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen, DisableMouseCapture)?;
    terminal.show_cursor()?;
    Ok(())
}