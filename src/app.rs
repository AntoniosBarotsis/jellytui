use ratatui::widgets::{ListState, TableState};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};

// UI -> Worker
#[derive(Debug)]
pub(crate) enum WorkerAction {
  FetchShows,
  FetchEpisodes {
    show_id: String,
    is_reload: bool,
  },
  TogglePlayed {
    episode_id: String,
    set_played: bool,
  },
  PlayEpisode {
    episode_id: String,
  },
}

// UI <- worker
#[derive(Debug)]
pub(crate) enum AppEvent {
  EpisodesReloaded,
  ShowsLoaded(Vec<Show>),
  EpisodesLoaded {
    show_id: String,
    episodes: Vec<Episode>,
  },
  Error(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Show {
  pub id: String,
  pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Episode {
  pub id: String,
  pub name: String,
  pub season_name: String,
  pub played: bool,
  pub run_time_ticks: u64,
  pub playback_position_ticks: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ActivePane {
  Shows,
  Seasons,
  Episodes,
}

pub(crate) struct App {
  pub active_pane: ActivePane,
  pub should_quit: bool,
  pub shows: Vec<Show>,
  pub episodes_cache: HashMap<String, Vec<Episode>>,
  pub fetched_shows_this_session: HashSet<String>,
  pub shows_state: ListState,
  pub seasons_state: ListState,
  pub episodes_state: TableState,

  pub status_msg: Option<String>,
  pub diag_msg: Option<String>,

  pub action_tx: UnboundedSender<WorkerAction>,
  pub event_rx: UnboundedReceiver<AppEvent>,
}

impl App {
  pub(crate) fn new(
    action_tx: UnboundedSender<WorkerAction>,
    event_rx: UnboundedReceiver<AppEvent>,
  ) -> Self {
    let mut app = Self {
      active_pane: ActivePane::Shows,
      should_quit: false,
      shows: Vec::new(),
      episodes_cache: HashMap::new(),
      fetched_shows_this_session: HashSet::new(),
      shows_state: ListState::default(),
      seasons_state: ListState::default(),
      episodes_state: TableState::default(),
      action_tx,
      event_rx,
      status_msg: None,
      diag_msg: None,
    };

    // Try to load from disk
    if let Some(cache) = crate::storage::load_cache() {
      app.shows = cache.shows;
      app.episodes_cache = cache.episodes_cache;
    }

    app.shows_state.select(Some(0));
    app.seasons_state.select(Some(0));
    app.episodes_state.select(Some(0));
    app
  }

  pub(crate) const fn quit(&mut self) {
    self.should_quit = true;
  }

  pub(crate) fn get_current_seasons(&self) -> Vec<String> {
    if let Some(show_idx) = self.shows_state.selected()
      && let Some(show) = self.shows.get(show_idx)
      && let Some(episodes) = self.episodes_cache.get(&show.id)
    {
      let mut seasons = Vec::new();
      for e in episodes {
        if seasons.last() != Some(&e.season_name) {
          seasons.push(e.season_name.clone());
        }
      }
      return seasons;
    }
    Vec::new()
  }

  pub(crate) fn get_current_episodes(&self) -> Vec<Episode> {
    let seasons = self.get_current_seasons();
    let selected_season = self.seasons_state.selected().and_then(|i| seasons.get(i));

    if let Some(show_idx) = self.shows_state.selected()
      && let Some(show) = self.shows.get(show_idx)
      && let Some(episodes) = self.episodes_cache.get(&show.id)
      && let Some(season_name) = selected_season
    {
      return episodes
        .iter()
        .filter(|e| &e.season_name == season_name)
        .cloned()
        .collect();
    }
    Vec::new()
  }

  // --- Navigation Logic ---
  pub(crate) const fn next_pane(&mut self) {
    match self.active_pane {
      ActivePane::Shows => self.active_pane = ActivePane::Seasons,
      ActivePane::Seasons => self.active_pane = ActivePane::Episodes,
      ActivePane::Episodes => {}
    }
  }

  pub(crate) const fn previous_pane(&mut self) {
    match self.active_pane {
      ActivePane::Shows => {}
      ActivePane::Seasons => self.active_pane = ActivePane::Shows,
      ActivePane::Episodes => self.active_pane = ActivePane::Seasons,
    }
  }

  pub(crate) fn next_item(&mut self) {
    match self.active_pane {
      ActivePane::Shows => {
        let max = self.shows.len().saturating_sub(1);
        let i = self
          .shows_state
          .selected()
          .map_or(0, |i| i.saturating_add(1).min(max));
        self.shows_state.select(Some(i));
      }
      ActivePane::Seasons => {
        let max = self.get_current_seasons().len().saturating_sub(1);
        let i = self
          .seasons_state
          .selected()
          .map_or(0, |i| i.saturating_add(1).min(max));
        self.seasons_state.select(Some(i));
      }
      ActivePane::Episodes => {
        let max = self.get_current_episodes().len().saturating_sub(1);
        let i = self
          .episodes_state
          .selected()
          .map_or(0, |i| i.saturating_add(1).min(max));
        self.episodes_state.select(Some(i));
      }
    }
  }

  pub(crate) fn previous_item(&mut self) {
    match self.active_pane {
      ActivePane::Shows => {
        let i = self.shows_state.selected().unwrap_or(0).saturating_sub(1);
        self.shows_state.select(Some(i));
      }
      ActivePane::Seasons => {
        let i = self.seasons_state.selected().unwrap_or(0).saturating_sub(1);
        self.seasons_state.select(Some(i));
      }
      ActivePane::Episodes => {
        let i = self
          .episodes_state
          .selected()
          .unwrap_or(0)
          .saturating_sub(1);
        self.episodes_state.select(Some(i));
      }
    }
  }

  pub(crate) fn toggle_current_episode_played(&mut self) -> Option<(String, bool)> {
    // Only allow toggling if we are actually focused on the Episodes pane
    if self.active_pane != ActivePane::Episodes {
      return None;
    }

    let seasons = self.get_current_seasons();
    let selected_season = self
      .seasons_state
      .selected()
      .and_then(|i| seasons.get(i).cloned());

    if let Some(show_idx) = self.shows_state.selected()
      && let Some(show) = self.shows.get(show_idx)
    {
      let show_id = show.id.clone();

      if let Some(episodes) = self.episodes_cache.get_mut(&show_id)
        && let Some(season_name) = selected_season
      {
        // We need to find the correct episode within the main un-filtered list
        // by matching the index from the filtered season list.
        let mut filtered_indices: Vec<usize> = Vec::new();
        for (idx, ep) in episodes.iter().enumerate() {
          if ep.season_name == season_name {
            filtered_indices.push(idx);
          }
        }

        if let Some(episodes_pane_idx) = self.episodes_state.selected()
          && let Some(&actual_cache_idx) = filtered_indices.get(episodes_pane_idx)
          && let Some(episode) = episodes.get_mut(actual_cache_idx)
        {
          // 1. Optimistically toggle the state locally
          episode.played = !episode.played;

          // 2. Return the data needed to send to the server
          return Some((episode.id.clone(), episode.played));
        }
      }
    }
    None
  }

  pub(crate) fn get_current_episode_id(&self) -> Option<String> {
    if self.active_pane == ActivePane::Episodes {
      let episodes = self.get_current_episodes();
      if let Some(idx) = self.episodes_state.selected()
        && let Some(ep) = episodes.get(idx)
      {
        return Some(ep.id.clone());
      }
    }
    None
  }
}
