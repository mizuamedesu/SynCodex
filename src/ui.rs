use crate::{
    rollout::Message,
    sessions::{self, Catalog, Session},
};
use anyhow::{Result, ensure};
use crossterm::{
    cursor::Show,
    event::{
        self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
        Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind,
    },
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{
    Frame, Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, BorderType, Borders, List, ListItem, ListState, Padding, Paragraph, Wrap},
};
use ratatui_textarea::{CursorMove, TextArea, WrapMode};
use serde_json::Value;
use std::{
    io::{self, IsTerminal},
    path::Path,
    sync::Once,
};

const BG: Color = Color::Rgb(15, 19, 27);
const FG: Color = Color::Rgb(224, 230, 240);
const MUTED: Color = Color::Rgb(147, 160, 179);
const BORDER: Color = Color::Rgb(55, 68, 86);
const ACCENT: Color = Color::Rgb(111, 208, 224);
const SELECTED: Color = Color::Rgb(29, 51, 72);
const WARNING: Color = Color::Rgb(247, 190, 109);

struct Screen {
    terminal: Terminal<CrosstermBackend<io::Stderr>>,
}

fn restore() {
    let _ = execute!(
        io::stderr(),
        DisableBracketedPaste,
        DisableMouseCapture,
        LeaveAlternateScreen,
        Show
    );
    let _ = disable_raw_mode();
}

impl Screen {
    fn open() -> Result<Self> {
        ensure!(
            io::stdin().is_terminal() && io::stderr().is_terminal(),
            "Interactive editing requires a terminal. Use rewind THREAD_ID --message N --text-file PATH"
        );
        static HOOK: Once = Once::new();
        HOOK.call_once(|| {
            let previous = std::panic::take_hook();
            std::panic::set_hook(Box::new(move |info| {
                restore();
                previous(info);
            }));
        });
        enable_raw_mode()?;
        let result = (|| {
            execute!(
                io::stderr(),
                EnterAlternateScreen,
                EnableBracketedPaste,
                EnableMouseCapture
            )?;
            let mut terminal = Terminal::new(CrosstermBackend::new(io::stderr()))?;
            terminal.hide_cursor()?;
            Ok(Self { terminal })
        })();
        if result.is_err() {
            restore();
        }
        result
    }

    fn repaint(&mut self) {
        // After draw, the next buffer is empty. Clear the previous frame too so
        // a step change fully repaints without requesting a terminal cursor report.
        self.terminal.swap_buffers();
    }
}

impl Drop for Screen {
    fn drop(&mut self) {
        let _ = self.terminal.show_cursor();
        restore();
    }
}

fn key_event(event: &Event) -> Option<KeyEvent> {
    match event {
        Event::Key(key) if key.kind != KeyEventKind::Release => Some(*key),
        _ => None,
    }
}

fn ctrl(key: KeyEvent, ch: char) -> bool {
    key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char(ch)
}

fn clean(text: &str) -> String {
    text.chars()
        .map(|ch| {
            if ch.is_control() && ch != '\n' && ch != '\t' {
                ' '
            } else {
                ch
            }
        })
        .collect()
}

fn panel(title: impl Into<Line<'static>>, active: bool) -> Block<'static> {
    Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(if active { ACCENT } else { BORDER }))
        .title_style(Style::default().fg(if active { ACCENT } else { MUTED }))
        .title(title)
        .padding(Padding::horizontal(1))
}

fn shell(frame: &mut Frame, step: usize, title: &str, help: &str, status: &str) -> Rect {
    frame.render_widget(
        Block::default().style(Style::default().bg(BG).fg(FG)),
        frame.area(),
    );
    let areas = Layout::vertical([
        Constraint::Length(3),
        Constraint::Min(1),
        Constraint::Length(4),
    ])
    .split(frame.area());
    let mut steps = vec![];
    for (index, label) in ["1 会話", "2 発言", "3 編集", "4 保存"].iter().enumerate() {
        if index > 0 {
            steps.push(Span::styled("  →  ", Style::default().fg(BORDER)));
        }
        steps.push(Span::styled(
            *label,
            Style::default().fg(if index == step { ACCENT } else { MUTED }),
        ));
    }
    let header = Text::from(vec![
        Line::from(vec![
            Span::styled(
                " SynCodex  ",
                Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
            ),
            Span::raw(title.to_owned()),
        ]),
        Line::from(steps),
    ]);
    frame.render_widget(
        Paragraph::new(header).block(
            Block::default()
                .borders(Borders::BOTTOM)
                .border_style(Style::default().fg(BORDER)),
        ),
        areas[0],
    );
    frame.render_widget(
        Paragraph::new(Text::from(vec![
            Line::styled(help.to_owned(), Style::default().fg(ACCENT)),
            Line::styled(status.to_owned(), Style::default().fg(MUTED)),
        ]))
        .wrap(Wrap { trim: false }),
        areas[2],
    );
    areas[1]
}

fn split_panels(area: Rect) -> (Rect, Rect) {
    let parts = if area.width >= 92 {
        Layout::horizontal([Constraint::Percentage(42), Constraint::Percentage(58)]).split(area)
    } else {
        Layout::vertical([Constraint::Percentage(45), Constraint::Percentage(55)]).split(area)
    };
    (parts[0], parts[1])
}

fn preview(frame: &mut Frame, area: Rect, title: String, text: String, scroll: &mut u16) {
    let block = panel(title, false);
    let inner = block.inner(area);
    let paragraph = Paragraph::new(text).wrap(Wrap { trim: false }).block(block);
    let max = paragraph
        .line_count(inner.width)
        .saturating_sub(inner.height as usize);
    *scroll = (*scroll as usize).min(max).min(u16::MAX as usize) as u16;
    frame.render_widget(paragraph.scroll((*scroll, 0)), area);
}

fn move_selection(state: &mut ListState, count: usize, direction: isize) {
    if count > 0 {
        let current = state.selected().unwrap_or(0);
        state.select(Some(
            current.saturating_add_signed(direction).min(count - 1),
        ));
    }
}

fn navigate(key: KeyEvent, state: &mut ListState, count: usize, scroll: &mut u16) -> bool {
    match key.code {
        KeyCode::Up if !key.modifiers.contains(KeyModifiers::CONTROL) => {
            move_selection(state, count, -1)
        }
        KeyCode::Down if !key.modifiers.contains(KeyModifiers::CONTROL) => {
            move_selection(state, count, 1)
        }
        KeyCode::Home => state.select((count > 0).then_some(0)),
        KeyCode::End => state.select(count.checked_sub(1)),
        KeyCode::PageUp => {
            *scroll = scroll.saturating_sub(8);
            return true;
        }
        KeyCode::PageDown => {
            *scroll = scroll.saturating_add(8);
            return true;
        }
        KeyCode::Up => {
            *scroll = scroll.saturating_sub(1);
            return true;
        }
        KeyCode::Down => {
            *scroll = scroll.saturating_add(1);
            return true;
        }
        _ => return false,
    }
    *scroll = 0;
    true
}

fn mouse_navigation(
    event: &Event,
    list: Rect,
    state: &mut ListState,
    count: usize,
    scroll: &mut u16,
) {
    if let Event::Mouse(mouse) = event {
        let in_list = list.contains((mouse.column, mouse.row).into());
        match mouse.kind {
            MouseEventKind::ScrollUp if in_list => {
                move_selection(state, count, -1);
                *scroll = 0;
            }
            MouseEventKind::ScrollDown if in_list => {
                move_selection(state, count, 1);
                *scroll = 0;
            }
            MouseEventKind::ScrollUp => *scroll = scroll.saturating_sub(3),
            MouseEventKind::ScrollDown => *scroll = scroll.saturating_add(3),
            MouseEventKind::Down(MouseButton::Left)
                if in_list && count > 0 && mouse.row > list.y =>
            {
                let row = mouse.row - list.y - 1;
                let index = state.offset() + row as usize / 3;
                state.select(Some(index.min(count - 1)));
                *scroll = 0;
            }
            _ => {}
        }
    }
}

fn session_title(session: &Session) -> String {
    let text = if session.name.trim().is_empty() {
        &session.preview
    } else {
        &session.name
    };
    if session.kind() == "承認レビュー" {
        "承認レビュー (自動生成)".into()
    } else if text.trim().is_empty() {
        format!("{} の会話", project(session))
    } else {
        sessions::snippet(text.trim(), 180)
    }
}

fn project(session: &Session) -> String {
    Path::new(&session.cwd)
        .file_name()
        .and_then(|v| v.to_str())
        .unwrap_or("フォルダー未設定")
        .to_owned()
}

fn when(session: &Session) -> String {
    chrono::DateTime::parse_from_rfc3339(&session.updated_at)
        .map(|v| {
            v.with_timezone(&chrono::Local)
                .format("%m/%d %H:%M")
                .to_string()
        })
        .unwrap_or_default()
}

pub fn pick_session(
    catalog: &Catalog,
    archived: bool,
    include_internal: bool,
    search: Option<&str>,
) -> Result<Option<String>> {
    let mut screen = Screen::open()?;
    let mut archived = archived;
    let mut include_internal = include_internal;
    let mut query = search.unwrap_or_default().to_owned();
    let mut state = ListState::default().with_selected(Some(0));
    let mut scroll = 0;
    let mut notice = String::new();
    loop {
        let matching = catalog.filtered(archived, Some(&query));
        let hidden = matching.iter().filter(|s| s.internal()).count();
        let sessions: Vec<_> = matching
            .into_iter()
            .filter(|s| include_internal || !s.internal())
            .collect();
        state.select(if sessions.is_empty() {
            None
        } else {
            Some(state.selected().unwrap_or(0).min(sessions.len() - 1))
        });
        let mut list_area = Rect::default();
        screen.terminal.draw(|frame| {
            let status = if !notice.is_empty() { notice.clone() } else {
                format!("{} 件の会話 · {} · アーカイブ {}", sessions.len(),
                    if include_internal { "内部会話を表示".into() } else { format!("内部会話 {hidden} 件を非表示") },
                    if archived { "表示" } else { "非表示" })
            };
            let main = shell(frame, 0, "セッションを選択", "↑↓ 選択  Enter 発言を見る  Esc 終了  F2 内部会話  F3 アーカイブ", &status);
            let areas = Layout::vertical([Constraint::Length(3), Constraint::Min(1)]).split(main);
            frame.render_widget(Paragraph::new(if query.is_empty() { "文字入力で検索：会話名・フォルダー・本文" } else { &query })
                .style(Style::default().fg(if query.is_empty() { MUTED } else { FG }))
                .block(panel(" 検索  (Ctrl+U でクリア) ", true)), areas[0]);
            let (left, right) = split_panels(areas[1]);
            list_area = left;
            let items: Vec<_> = sessions.iter().map(|s| ListItem::new(vec![
                Line::styled(session_title(s), Style::default().add_modifier(Modifier::BOLD)),
                Line::styled(format!("{}  {}  {}{}", project(s), when(s),
                    if s.in_use { "使用中" } else { "編集可" },
                    if s.internal() { format!(" · {}", s.kind()) } else if s.archived { " · アーカイブ".into() } else { String::new() }), Style::default().fg(MUTED)),
                Line::raw(""),
            ])).collect();
            frame.render_stateful_widget(List::new(items).block(panel(" 会話 ", true))
                .highlight_style(Style::default().bg(SELECTED)).highlight_symbol("▸ "), left, &mut state);
            if let Some(s) = state.selected().and_then(|i| sessions.get(i)) {
                let text = format!("{}\n\nフォルダー\n{}\n\n最終更新  {}\n種類      {}\n状態      {}\n\nはじめの依頼\n{}\n\nEnter で発言一覧を見る → 編集する発言を選ぶ → 本文を編集 → 保存して終了\n\n会話 ID\n{}",
                    session_title(s), clean(&s.cwd), when(s), s.kind(), if s.in_use { "使用中" } else { "編集できます" },
                    if s.preview.is_empty() { "発言一覧で内容を確認できます".into() } else { clean(&s.preview) }, s.id);
                preview(frame, right, " 会話の内容 / PgUp・PgDn でスクロール ".into(), text, &mut scroll);
            } else {
                preview(frame, right, " 会話の内容 ".into(), "一致する会話がありません。\n検索を消すか、F2 / F3 で表示対象を広げてください。".into(), &mut scroll);
            }
        })?;
        let event = event::read()?;
        mouse_navigation(&event, list_area, &mut state, sessions.len(), &mut scroll);
        let Some(key) = key_event(&event) else {
            if let Event::Paste(text) = event {
                query.push_str(&text.replace(['\r', '\n'], " "));
                state.select(Some(0));
                scroll = 0;
            }
            continue;
        };
        if key.code == KeyCode::Esc || ctrl(key, 'c') {
            return Ok(None);
        }
        if navigate(key, &mut state, sessions.len(), &mut scroll) {
            notice.clear();
            continue;
        }
        if key.code == KeyCode::Enter {
            if let Some(s) = state.selected().and_then(|i| sessions.get(i)) {
                let busy_child = catalog
                    .descendants(&s.id)
                    .iter()
                    .any(|id| catalog.sessions[id].in_use);
                if s.in_use {
                    notice = "この会話は使用中です。対象の会話を閉じると編集できます。".into();
                } else if busy_child {
                    notice = "この会話を参照する分岐が使用中です。その分岐を閉じると編集できます。"
                        .into();
                } else {
                    return Ok(Some(s.id.clone()));
                }
            }
        } else {
            notice.clear();
            match key.code {
                KeyCode::F(2) => include_internal = !include_internal,
                KeyCode::F(3) => archived = !archived,
                KeyCode::Backspace => {
                    query.pop();
                }
                _ if ctrl(key, 'u') => query.clear(),
                KeyCode::Char(ch)
                    if !key
                        .modifiers
                        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
                {
                    query.push(ch)
                }
                _ => continue,
            }
            state.select(Some(0));
            scroll = 0;
        }
    }
}

fn review_prompt(text: &str) -> bool {
    text.trim_start().starts_with(
        "The following is the Codex agent history added since your last approval assessment.",
    )
}

fn decision(text: &str) -> Option<Value> {
    serde_json::from_str::<Value>(text).ok().filter(|v| {
        v["risk_level"].is_string() && v["outcome"].is_string() && v["rationale"].is_string()
    })
}

fn outcome(value: &Value) -> &str {
    match value["outcome"].as_str().unwrap_or_default() {
        "allow" => "許可",
        "deny" => "拒否",
        other => other,
    }
}

fn summary(message: &Message) -> String {
    if review_prompt(&message.text) {
        "承認レビューの入力 · 追加された実行履歴".into()
    } else if let Some(value) = decision(&message.text) {
        format!(
            "{} · {}",
            outcome(&value),
            value["rationale"].as_str().unwrap_or_default()
        )
    } else {
        sessions::snippet(message.text.trim(), 180)
    }
}

fn message_preview(message: &Message) -> String {
    if let Some(value) = decision(&message.text) {
        format!(
            "判定  {}\nリスク  {}\nユーザーの承認  {}\n\n理由\n{}",
            outcome(&value),
            level(value["risk_level"].as_str().unwrap_or_default()),
            level(value["user_authorization"].as_str().unwrap_or("未設定")),
            clean(value["rationale"].as_str().unwrap_or_default())
        )
    } else {
        clean(&message.text)
    }
}

fn level(value: &str) -> &str {
    match value {
        "low" => "低",
        "medium" => "中",
        "high" => "高",
        "critical" => "重大",
        "none" => "なし",
        other => other,
    }
}

pub fn role_label(role: &str) -> &str {
    if role == "user" { "User" } else { "Assistant" }
}

pub fn pick_message(
    session: &Session,
    messages: &[Message],
    selected: usize,
) -> Result<Option<usize>> {
    let mut screen = Screen::open()?;
    let mut query = String::new();
    let mut state = ListState::default().with_selected(Some(selected));
    let mut scroll = 0;
    loop {
        let search = query.to_lowercase();
        let visible: Vec<_> = messages
            .iter()
            .enumerate()
            .filter(|(_, m)| {
                search.is_empty()
                    || m.text.to_lowercase().contains(&search)
                    || m.role.to_lowercase().contains(&search)
                    || m.index.to_string() == search
            })
            .collect();
        state.select(if visible.is_empty() {
            None
        } else {
            Some(state.selected().unwrap_or(0).min(visible.len() - 1))
        });
        let mut list_area = Rect::default();
        screen.terminal.draw(|frame| {
            let selected = state.selected().and_then(|i| visible.get(i));
            let status = selected
                .map(|(_, m)| {
                    format!(
                        "{} · #{} {} を編集すると、後の {} 件を削除します",
                        sessions::snippet(&session_title(session), 30),
                        m.index,
                        role_label(&m.role),
                        messages.len() - m.index
                    )
                })
                .unwrap_or_else(|| "一致する発言がありません".into());
            let main = shell(
                frame,
                1,
                "発言を選択",
                "↑↓ 選択  Enter 本文を編集  PgUp/PgDn 全文を読む  Esc 会話一覧へ",
                &status,
            );
            let areas = Layout::vertical([Constraint::Length(3), Constraint::Min(1)]).split(main);
            frame.render_widget(
                Paragraph::new(if query.is_empty() {
                    "文字入力で発言を検索"
                } else {
                    &query
                })
                .style(Style::default().fg(if query.is_empty() { MUTED } else { FG }))
                .block(panel(" 検索  (Ctrl+U でクリア) ", true)),
                areas[0],
            );
            let (left, right) = split_panels(areas[1]);
            list_area = left;
            let items: Vec<_> = visible
                .iter()
                .map(|(_, m)| {
                    ListItem::new(vec![
                        Line::from(vec![
                            Span::styled(format!("#{}  ", m.index), Style::default().fg(MUTED)),
                            Span::styled(
                                if m.role == "user" {
                                    "User / あなた"
                                } else {
                                    "Assistant"
                                },
                                Style::default()
                                    .fg(if m.role == "user" { ACCENT } else { WARNING })
                                    .add_modifier(Modifier::BOLD),
                            ),
                        ]),
                        Line::raw(summary(m)),
                        Line::raw(""),
                    ])
                })
                .collect();
            frame.render_stateful_widget(
                List::new(items)
                    .block(panel(
                        format!(" 発言  {} / {} 件 ", visible.len(), messages.len()),
                        true,
                    ))
                    .highlight_style(Style::default().bg(SELECTED))
                    .highlight_symbol("▸ "),
                left,
                &mut state,
            );
            if let Some((_, m)) = selected {
                preview(
                    frame,
                    right,
                    format!(" #{} {} · 全文 ", m.index, role_label(&m.role)),
                    message_preview(m),
                    &mut scroll,
                );
            } else {
                preview(
                    frame,
                    right,
                    " 発言の全文 ".into(),
                    "検索を消すと全発言を表示します。".into(),
                    &mut scroll,
                );
            }
        })?;
        let event = event::read()?;
        mouse_navigation(&event, list_area, &mut state, visible.len(), &mut scroll);
        let Some(key) = key_event(&event) else {
            if let Event::Paste(text) = event {
                query.push_str(&text.replace(['\r', '\n'], " "));
                state.select(Some(0));
                scroll = 0;
            }
            continue;
        };
        if key.code == KeyCode::Esc || ctrl(key, 'c') {
            return Ok(None);
        }
        if navigate(key, &mut state, visible.len(), &mut scroll) {
            continue;
        }
        if key.code == KeyCode::Enter {
            if let Some((index, _)) = state.selected().and_then(|i| visible.get(i)) {
                return Ok(Some(*index));
            }
        } else {
            match key.code {
                KeyCode::Backspace => {
                    query.pop();
                }
                _ if ctrl(key, 'u') => query.clear(),
                KeyCode::Char(ch)
                    if !key
                        .modifiers
                        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
                {
                    query.push(ch)
                }
                _ => continue,
            }
            state.select(Some(0));
            scroll = 0;
        }
    }
}

pub fn edit_message(
    session: &Session,
    message: &Message,
    total: usize,
    forks: usize,
    dry_run: bool,
) -> Result<Option<String>> {
    let mut screen = Screen::open()?;
    // split preserves a trailing newline, including when the text is saved unchanged.
    let mut editor = TextArea::new(message.text.split('\n').map(str::to_owned).collect());
    editor.set_style(Style::default().bg(BG).fg(FG));
    editor.set_cursor_line_style(Style::default());
    editor.set_cursor_style(Style::default().bg(ACCENT).fg(BG));
    editor.set_selection_style(Style::default().bg(SELECTED));
    editor.set_wrap_mode(WrapMode::Glyph);
    editor.set_max_histories(1000);
    editor.move_cursor(CursorMove::Top);
    let mut confirm = false;
    let mut notice = String::new();
    let mut scroll = 0;
    let mut discard = false;
    loop {
        screen.terminal.draw(|frame| {
            if discard {
                let main = shell(
                    frame,
                    2,
                    "編集内容を破棄しますか",
                    "Enter 破棄して発言一覧へ  Esc 編集を続ける",
                    "まだ履歴には反映していません",
                );
                frame.render_widget(
                    Paragraph::new("編集した内容を破棄して、発言一覧に戻ります。")
                        .block(panel(" 編集内容の破棄 ", true))
                        .wrap(Wrap { trim: false }),
                    main,
                );
            } else if confirm {
                let main = shell(
                    frame,
                    3,
                    "変更を確認",
                    if dry_run {
                        "Enter 検証する  Esc 編集に戻る  PgUp/PgDn スクロール"
                    } else {
                        "Enter この内容で巻き戻す  Esc 編集に戻る  PgUp/PgDn スクロール"
                    },
                    if dry_run {
                        "dry-run：履歴は変更しません"
                    } else {
                        "反映前に元の履歴を自動でバックアップします"
                    },
                );
                let areas =
                    Layout::vertical([Constraint::Length(4), Constraint::Min(1)]).split(main);
                frame.render_widget(
                    Paragraph::new(vec![
                        Line::styled(
                            format!(
                                "{} · #{} {} を更新",
                                sessions::snippet(&session_title(session), 30),
                                message.index,
                                role_label(&message.role)
                            ),
                            Style::default().add_modifier(Modifier::BOLD),
                        ),
                        Line::from(vec![
                            Span::styled(
                                format!("残す {} 件", message.index),
                                Style::default().fg(ACCENT),
                            ),
                            Span::raw("    "),
                            Span::styled(
                                format!("この後の {} 件を削除", total - message.index),
                                Style::default().fg(WARNING),
                            ),
                        ]),
                        Line::styled(
                            format!(
                                "後続のツール履歴・要約も削除{}",
                                if forks > 0 {
                                    format!(" / 分岐 {forks} 件の内容は保全")
                                } else {
                                    String::new()
                                }
                            ),
                            Style::default().fg(MUTED),
                        ),
                    ])
                    .wrap(Wrap { trim: false }),
                    areas[0],
                );
                let (left, right) = split_panels(areas[1]);
                let mut before_scroll = scroll;
                let mut after_scroll = scroll;
                preview(
                    frame,
                    left,
                    " 変更前 ".into(),
                    clean(&message.text),
                    &mut before_scroll,
                );
                preview(
                    frame,
                    right,
                    " 変更後 · ここまでを残す ".into(),
                    clean(&editor.lines().join("\n")),
                    &mut after_scroll,
                );
                scroll = before_scroll.max(after_scroll);
            } else {
                let status = if notice.is_empty() {
                    format!(
                        "#{} {} · この後の {} 件を削除予定 · 保存前に変更を確認できます",
                        message.index,
                        role_label(&message.role),
                        total - message.index
                    )
                } else {
                    notice.clone()
                };
                let main = shell(
                    frame,
                    2,
                    "本文を編集",
                    "そのまま文字入力  Ctrl+S 確認へ  Ctrl+Z 元に戻す  Ctrl+A 全選択  Esc 戻る",
                    &status,
                );
                editor.set_block(panel(
                    format!(
                        " #{} {} · 日本語 / 複数行 / 貼り付け可 ",
                        message.index,
                        role_label(&message.role)
                    ),
                    true,
                ));
                frame.render_widget(&editor, main);
            }
        })?;
        let event = event::read()?;
        let Some(key) = key_event(&event) else {
            if !confirm && !discard {
                if let Event::Paste(text) = event {
                    editor.insert_str(text.replace("\r\n", "\n").replace('\r', "\n"));
                } else {
                    editor.input(event);
                }
            }
            continue;
        };
        if discard {
            if key.code == KeyCode::Enter || ctrl(key, 'c') {
                return Ok(None);
            }
            if key.code == KeyCode::Esc {
                discard = false;
                screen.repaint();
            }
        } else if confirm {
            match key.code {
                KeyCode::Enter => return Ok(Some(editor.lines().join("\n"))),
                KeyCode::Esc => {
                    confirm = false;
                    scroll = 0;
                    screen.repaint();
                }
                KeyCode::PageUp => scroll = scroll.saturating_sub(8),
                KeyCode::PageDown => scroll = scroll.saturating_add(8),
                _ if ctrl(key, 'c') => return Ok(None),
                _ => {}
            }
        } else if key.code == KeyCode::Esc || ctrl(key, 'c') {
            if editor.lines().join("\n") == message.text {
                return Ok(None);
            }
            discard = true;
            screen.repaint();
        } else if ctrl(key, 's') {
            if editor.lines().join("\n").trim().is_empty() {
                notice = "本文が空です。文字を入力してから Ctrl+S を押してください。".into();
            } else {
                confirm = true;
                notice.clear();
                screen.repaint();
            }
        } else if ctrl(key, 'a') {
            editor.select_all();
        } else if ctrl(key, 'z') {
            editor.undo();
        } else if ctrl(key, 'y') {
            editor.redo();
        } else {
            notice.clear();
            editor.input(key);
        }
    }
}
