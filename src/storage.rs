use crate::app::{Episode, Show};
use directories::ProjectDirs;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

#[derive(Serialize, Deserialize)]
pub(crate) struct AppCache {
  pub shows: Vec<Show>,
  pub episodes_cache: HashMap<String, Vec<Episode>>,
}

fn get_cache_path() -> Option<PathBuf> {
  // ~/.local/share/jellytui/state.json
  // ~/AppData/Roaming/jellytui/jellytui/data
  if let Some(proj_dirs) = ProjectDirs::from("com", "jellytui", "jellytui") {
    let dir = proj_dirs.data_dir();

    if fs::create_dir_all(dir).is_ok() {
      return Some(dir.join("state.json"));
    }
  }
  None
}

pub(crate) fn load_cache() -> Option<AppCache> {
  let path = get_cache_path()?;
  if path.exists() {
    let data = fs::read_to_string(path).ok()?;
    let cache: AppCache = serde_json::from_str(&data).ok()?;
    return Some(cache);
  }
  None
}

pub(crate) fn save_cache(shows: &[Show], episodes_cache: &HashMap<String, Vec<Episode>>) {
  if let Some(path) = get_cache_path() {
    let cache = AppCache {
      shows: shows.to_vec(),
      episodes_cache: episodes_cache.clone(),
    };
    if let Ok(json) = serde_json::to_string_pretty(&cache) {
      let _e = fs::write(path, json);
    }
  }
}
