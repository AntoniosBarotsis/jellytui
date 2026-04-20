#![allow(clippy::indexing_slicing)]

mod app;
mod network;
mod storage;

use anyhow::Result;
use app::{ActivePane, App, AppEvent, WorkerAction};
use crossterm::{
  event::{self, Event, KeyCode, KeyEventKind},
  execute,
  terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{
  Terminal,
  backend::{Backend, CrosstermBackend},
  layout::{Constraint, Direction, Layout},
  style::{Color, Modifier, Style},
  widgets::{Block, Borders, Cell, List, ListItem, Row, Table},
};
use std::{io, time::Duration};
use tokio::sync::mpsc;

#[tokio::main]
async fn main() -> Result<()> {
  let _e = dotenvy::dotenv();

  enable_raw_mode()?;
  let mut stdout = io::stdout();
  execute!(stdout, EnterAlternateScreen)?;
  let backend = CrosstermBackend::new(stdout);
  let mut terminal = Terminal::new(backend)?;

  let (action_tx, action_rx) = mpsc::unbounded_channel();
  let (event_tx, event_rx) = mpsc::unbounded_channel();
  let mut app = App::new(action_tx.clone(), event_rx);

  let _t = tokio::spawn(async move {
    network::run_network_worker(action_rx, event_tx).await;
  });

  let _ = app.action_tx.send(WorkerAction::FetchShows).ok();

  let res = run_app(&mut terminal, &mut app).await;

  disable_raw_mode()?;
  execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
  terminal.show_cursor()?;

  if let Err(err) = res {
    println!("{err:?}");
  }
  Ok(())
}

// Needs to be async
#[allow(clippy::too_many_lines, clippy::unused_async)]
async fn run_app<B: Backend>(terminal: &mut Terminal<B>, app: &mut App) -> Result<()> {
  let tick_rate = Duration::from_millis(250);

  loop {
    let _e = terminal.draw(|f| ui(f, app))?;

    if event::poll(tick_rate)?
      && let Event::Key(key) = event::read()?
    {
      let prev_show_idx = app.shows_state.selected();
      let prev_season_idx = app.seasons_state.selected();

      if key.kind == KeyEventKind::Press {
        match key.code {
          KeyCode::Char('q') | KeyCode::Esc => app.quit(),
          KeyCode::Left => app.previous_pane(),
          KeyCode::Right => app.next_pane(),
          KeyCode::Up => app.previous_item(),
          KeyCode::Down => app.next_item(),
          KeyCode::Char(' ') => {
            // Attempt to toggle the episode. If successful, tell the server
            if let Some((episode_id, set_played)) = app.toggle_current_episode_played() {
              let _ = app
                .action_tx
                .send(WorkerAction::TogglePlayed {
                  episode_id,
                  set_played,
                })
                .ok();
            }
          }
          KeyCode::Enter | KeyCode::Char('s') => {
            if let Some(episode_id) = app.get_current_episode_id() {
              let _ = app
                .action_tx
                .send(WorkerAction::PlayEpisode { episode_id })
                .ok();
            }
          }
          KeyCode::Char('r') => {
            if let Some(idx) = app.shows_state.selected()
              && let Some(show) = app.shows.get(idx)
            {
              let _ = app.fetched_shows_this_session.insert(show.id.clone());

              app.diag_msg = Some("Refreshing...".to_owned());

              let _ = app
                .action_tx
                .send(WorkerAction::FetchEpisodes {
                  show_id: show.id.clone(),
                  is_reload: true,
                })
                .ok();
            }
          }
          _ => {}
        }
      }

      // If Show changed: fetch episodes and reset seasons/episodes to top
      if app.active_pane == ActivePane::Shows && prev_show_idx != app.shows_state.selected() {
        app.seasons_state.select(Some(0));
        app.episodes_state.select(Some(0));

        if let Some(idx) = app.shows_state.selected()
          && let Some(show) = app.shows.get(idx)
          && !app.fetched_shows_this_session.contains(&show.id)
        {
          let _ = app.fetched_shows_this_session.insert(show.id.clone());

          let _ = app
            .action_tx
            .send(WorkerAction::FetchEpisodes {
              show_id: show.id.clone(),
              is_reload: false,
            })
            .ok();
        }
      }

      // If Season changed: reset episodes to top
      if app.active_pane == ActivePane::Seasons && prev_season_idx != app.seasons_state.selected() {
        app.episodes_state.select(Some(0));
      }
    }
    if let Ok(event) = app.event_rx.try_recv() {
      match event {
        AppEvent::ShowsLoaded(shows) => {
          app.shows = shows;
          app.status_msg = None;

          // Force a fetch for whichever show is currently highlighted
          if let Some(idx) = app.shows_state.selected()
            && let Some(show) = app.shows.get(idx)
            && !app.fetched_shows_this_session.contains(&show.id)
          {
            let _ = app.fetched_shows_this_session.insert(show.id.clone());

            let _ = app
              .action_tx
              .send(WorkerAction::FetchEpisodes {
                show_id: show.id.clone(),
                is_reload: false,
              })
              .ok();
          }
        }
        AppEvent::EpisodesLoaded { show_id, episodes } => {
          let _e = app.episodes_cache.insert(show_id.clone(), episodes);

          let _ = app.fetched_shows_this_session.insert(show_id);

          app.status_msg = None;
        }
        AppEvent::Error(err) => {
          app.status_msg = Some(err);
        }
        AppEvent::EpisodesReloaded => {
          app.diag_msg = None;
        }
      }
    }

    if app.should_quit {
      // SAVE TO DISK
      storage::save_cache(&app.shows, &app.episodes_cache);
      return Ok(());
    }
  }
}

fn ui(f: &mut ratatui::Frame<'_>, app: &mut App) {
  let vertical_chunks = Layout::default()
    .direction(Direction::Vertical)
    .constraints([Constraint::Min(0), Constraint::Length(1)])
    .split(f.size());

  let constraints = match app.active_pane {
    ActivePane::Shows => [
      Constraint::Percentage(50),
      Constraint::Percentage(25),
      Constraint::Percentage(25),
    ],
    ActivePane::Seasons => [
      Constraint::Percentage(25),
      Constraint::Percentage(50),
      Constraint::Percentage(25),
    ],
    ActivePane::Episodes => [
      Constraint::Percentage(20),
      Constraint::Percentage(20),
      Constraint::Percentage(60),
    ],
  };

  let chunks = Layout::default()
    .direction(Direction::Horizontal)
    .constraints(constraints)
    .split(vertical_chunks[0]);

  let highlight_style = Style::default().add_modifier(Modifier::REVERSED);

  // Shows
  let shows_items: Vec<ListItem<'_>> = app
    .shows
    .iter()
    .map(|s| ListItem::new(s.name.as_str()))
    .collect();

  let shows_list = List::new(shows_items)
    .block(get_block(" Shows ", app.active_pane == ActivePane::Shows))
    .highlight_style(highlight_style);

  f.render_stateful_widget(shows_list, chunks[0], &mut app.shows_state);

  // Seasons
  let seasons = app.get_current_seasons();
  let seasons_items: Vec<ListItem<'_>> = if seasons.is_empty() && !app.shows.is_empty() {
    vec![ListItem::new("Loading...")]
  } else {
    seasons.into_iter().map(ListItem::new).collect()
  };

  let seasons_list = List::new(seasons_items)
    .block(get_block(
      " Seasons ",
      app.active_pane == ActivePane::Seasons,
    ))
    .highlight_style(highlight_style);
  f.render_stateful_widget(seasons_list, chunks[1], &mut app.seasons_state);

  // Episodes
  // --- Pane 3: Episodes ---
  let episodes = app.get_current_episodes();

  let episodes_items: Vec<Row<'_>> = if episodes.is_empty() && !app.shows.is_empty() {
    vec![Row::new(vec!["Loading..."])]
  } else {
    episodes
      .into_iter()
      .map(|e| {
        // In src/main.rs (inside the map function for episodes_items)
        let watched_marker = if e.played {
          "[x]"
        } else if e.playback_position_ticks.unwrap_or_default() > 0 {
          "[~]" // The episode has a resume point! (Feel free to change this to [~] or [>])
        } else {
          "[ ]"
        };

        let name_col = format!("{watched_marker} {}", e.name);

        let duration_col = {
          let total_secs = e.run_time_ticks / 10_000_000;
          let mins = total_secs / 60;
          let secs = total_secs % 60;
          // Format to ensure it always takes up exactly 5 characters (e.g., "22:15" or "05:02")
          format!("{mins:02}:{secs:02}")
        };

        // Pass the two columns into a Row
        Row::new(vec![Cell::from(name_col), Cell::from(duration_col)])
      })
      .collect()
  };

  // Define the column widths:
  // - Col 1 (Name) expands to take all available minimum space
  // - Col 2 (Duration) is locked to exactly 6 characters (" 22:15")
  let column_widths = [Constraint::Min(0), Constraint::Length(6)];

  let episodes_table = Table::new(episodes_items, column_widths)
    .block(get_block(
      " Episodes ",
      app.active_pane == ActivePane::Episodes,
    ))
    .highlight_style(highlight_style);

  f.render_stateful_widget(episodes_table, chunks[2], &mut app.episodes_state);

  // Status Bar Layout
  // Split the bottom row horizontally into two 50% halves
  let status_chunks = Layout::default()
    .direction(Direction::Horizontal)
    .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
    .split(vertical_chunks[1]);

  // Left Side: Status Message
  let (status_text, status_color) = app.status_msg.as_ref().map_or_else(
    || (" Connected to Jellyfin ".to_string(), Color::Green),
    |msg| (format!(" ERROR: {msg} "), Color::Red),
  );

  let status_bar = ratatui::widgets::Paragraph::new(status_text).style(
    Style::default()
      .fg(status_color)
      .add_modifier(Modifier::BOLD),
  );

  // Render the left status bar into status_chunks[0]
  f.render_widget(status_bar, status_chunks[0]);

  // Right Side: Diagnostic Message
  // (Assuming you add a `diagnostic_msg` field to your `App` struct,
  // otherwise you can replace the text below with whatever variable you're using)
  let diag_msg = app.diag_msg.as_ref().unwrap_or(&String::new()).to_owned();

  let diag_bar = ratatui::widgets::Paragraph::new(diag_msg)
    .style(Style::default().fg(Color::DarkGray))
    .alignment(ratatui::layout::Alignment::Right); // Aligns the text to the far right

  // Render the right diagnostic bar into status_chunks[1]
  f.render_widget(diag_bar, status_chunks[1]);
}

fn get_block(title: &str, is_active: bool) -> Block<'_> {
  let border_color = if is_active {
    Color::Yellow
  } else {
    Color::DarkGray
  };
  Block::default()
    .title(title)
    .borders(Borders::ALL)
    .border_style(Style::default().fg(border_color))
}
