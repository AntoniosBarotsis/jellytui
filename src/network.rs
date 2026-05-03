use crate::app::{AppEvent, Episode, Show, WorkerAction};
use reqwest::Client;
use serde::Deserialize;
use std::fs::OpenOptions;
use std::io::Write;
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};

// --- Jellyfin API Deserialization Structs ---
#[derive(Deserialize)]
struct JellyfinResponse {
  #[serde(rename = "Items")]
  items: Vec<JellyfinItem>,
}

#[derive(Deserialize, Clone)]
struct JellyfinItem {
  #[serde(rename = "Id")]
  id: String,
  #[serde(rename = "Name")]
  name: String,
  #[serde(rename = "SeasonName")]
  season_name: Option<String>,
  #[serde(rename = "UserData")]
  user_data: Option<JellyfinUserData>,
  #[serde(rename = "RunTimeTicks")]
  run_time_ticks: u64,
}

#[derive(Deserialize, Clone)]
struct JellyfinUserData {
  #[serde(rename = "Played")]
  played: bool,
  #[serde(rename = "PlaybackPositionTicks")]
  playback_position_ticks: u64,
}

#[allow(clippy::too_many_lines)]
pub(crate) async fn run_network_worker(
  mut action_rx: UnboundedReceiver<WorkerAction>,
  event_tx: UnboundedSender<AppEvent>,
) {
  let client = Client::new();

  // Load from environment
  let jellyfin_url = std::env::var("JELLYFIN_URL").expect("JELLYFIN_URL must be set in .env");
  let api_key = std::env::var("JELLYFIN_API_KEY").expect("JELLYFIN_API_KEY must be set in .env");
  let user_id = std::env::var("JELLYFIN_USER_ID").expect("JELLYFIN_USER_ID must be set in .env");
  let debug_mode =
    std::env::var("DEBUG").is_ok_and(|el| el.parse::<bool>().expect("Could not parse DEBUG"));

  let auth_header = format!("MediaBrowser Token=\"{api_key}\"");

  while let Some(action) = action_rx.recv().await {
    let jellyfin_url = jellyfin_url.clone();
    let user_id = user_id.clone();

    match action {
      WorkerAction::FetchShows => {
        let url = format!("{jellyfin_url}/Users/{user_id}/Items");
        let params = [
          ("IncludeItemTypes", "Series"),
          ("Recursive", "true"),
          ("enableImages", "false"),
          ("enableUserData", "false"), // Save bandwidth, we don't need played status for the show itself
          ("SortBy", "SortName"),
        ];

        match client
          .get(&url)
          .header("Authorization", &auth_header)
          .query(&params)
          .send()
          .await
        {
          Ok(resp) if resp.status().is_success() => {
            if let Ok(data) = resp.json::<JellyfinResponse>().await {
              let shows: Vec<Show> = data
                .items
                .into_iter()
                .map(|item| Show {
                  id: item.id,
                  name: item.name,
                })
                .collect();
              let _ = event_tx.send(AppEvent::ShowsLoaded(shows)).ok();
            }
          }
          Err(e) => {
            let _ = event_tx.send(AppEvent::Error(e.to_string())).ok();
          }
          Ok(resp) => {
            let _ = event_tx
              .send(AppEvent::Error(format!("Status: {}", resp.status())))
              .ok();
          }
        }
      }
      WorkerAction::FetchEpisodes { show_id, is_reload } => {
        let url = format!("{jellyfin_url}/Shows/{show_id}/Episodes");
        // Important: We need UserId here to get the UserData (played status)
        let params = [("UserId", user_id)];

        match client
          .get(&url)
          .header("Authorization", &auth_header)
          .query(&params)
          .send()
          .await
        {
          Ok(resp) if resp.status().is_success() => {
            if let Ok(data) = resp.json::<JellyfinResponse>().await {
              let episodes: Vec<Episode> = data
                .items
                .into_iter()
                .map(|item| Episode {
                  id: item.id,
                  name: item.name,
                  season_name: item
                    .season_name
                    .unwrap_or_else(|| "Unknown Season".to_string()),
                  played: item.user_data.as_ref().is_some_and(|ud| ud.played),
                  run_time_ticks: item.run_time_ticks,
                  playback_position_ticks: item.user_data.map(|ud| ud.playback_position_ticks),
                })
                .collect();
              let _ = event_tx
                .send(AppEvent::EpisodesLoaded { show_id, episodes })
                .ok();

              if is_reload {
                let _ = event_tx.send(AppEvent::EpisodesReloaded).ok();
              }
            }
          }
          Err(e) => {
            let _ = event_tx.send(AppEvent::Error(e.to_string())).ok();
          }
          Ok(resp) => {
            let _ = event_tx
              .send(AppEvent::Error(format!("Status: {}", resp.status())))
              .ok();
          }
        }
      }
      WorkerAction::TogglePlayed {
        episode_id,
        set_played,
      } => {
        let url = format!("{jellyfin_url}/Users/{user_id}/PlayedItems/{episode_id}");

        // Jellyfin uses POST to mark played, and DELETE to mark unplayed
        let request = if set_played {
          client.post(&url)
        } else {
          client.delete(&url)
        };

        let _res = request.header("Authorization", &auth_header).send().await;

        // Note: We are ignoring the response here because we did an optimistic UI update.
        // In a hyper-robust app, you would check for an error status here and send a
        // message back to the UI to revert the checkbox if the network request failed.
      }
      WorkerAction::PlayEpisode { episode_id } => {
        let stream_url = format!("{jellyfin_url}/Items/{episode_id}/Download?api_key={api_key}");

        let auth_header = auth_header.clone();
        let client = client.clone();
        let episode_id = episode_id.clone();
        let user_id = user_id.clone();

        let _t = tokio::spawn(async move {
          let mut debug_file: Box<dyn Write + Send> = if debug_mode {
            Box::new(
              OpenOptions::new()
                .create(true)
                .append(true)
                .open("mpv_debug.log")
                .expect("Could not create debug file!"),
            )
          } else {
            Box::new(std::io::sink())
          };

          let _e = writeln!(debug_file, "\n--- STARTING NEW MPV SESSION ---");

          // 1. FETCH RESUME POINT FROM JELLYFIN
          let item_url = format!("{jellyfin_url}/Users/{user_id}/Items/{episode_id}");
          let mut start_time_sec = 0;

          if let Ok(response) = client
            .get(&item_url)
            .header("Authorization", &auth_header)
            .send()
            .await
            && let Ok(json) = response.json::<serde_json::Value>().await
          {
            // Extract ticks and convert back to seconds (1 sec = 10,000,000 ticks)
            if let Some(ticks) = json["UserData"]["PlaybackPositionTicks"].as_u64() {
              start_time_sec = ticks / 10_000_000;
              let _e = writeln!(
                debug_file,
                "Found resume point at {start_time_sec} seconds ({ticks} ticks)"
              );
            }
          }

          // 2. CONFIGURE MPV
          let mut command = Command::new("mpv");
          let _ = command
            .arg(&stream_url)
            .arg("--fs")
            .arg("--terminal=yes")
            .arg("--term-status-msg=STATUS:${=time-pos}");

          // If we found a valid resume point, pass the start flag
          if start_time_sec > 0 {
            let _ = command.arg(format!("--start={start_time_sec}"));
          }

          // Replace your .spawn().expect(...) with this:
          let mut child = match command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
          {
            Ok(c) => c,
            Err(e) => {
              let _e = writeln!(debug_file, "CRITICAL: Failed to spawn mpv: {e}");
              return; // Gracefully exit this specific task without crashing the network thread
            }
          };

          if let Some(mut stdout) = child.stdout.take() {
            let _t = tokio::spawn(async move {
              let mut buf = [0; 1024];
              while let Ok(bytes) = stdout.read(&mut buf).await {
                if bytes == 0 {
                  break;
                }
              }
            });
          }

          // Main loop watching stdout
          if let Some(stderr) = child.stderr.take() {
            let mut reader = BufReader::new(stderr);
            let mut last_reported_time = 0;

            let mut buf = Vec::<u8>::new();
            while let Ok(bytes_read) = reader.read_until(b'\r', &mut buf).await {
              let line = String::from_utf8(buf.clone()).expect("Could not parse bytes to string");
              if bytes_read == 0 {
                break;
              }

              if line.contains("STATUS:") {
                // Use `rfind` to get the LAST occurrence of STATUS in the chunk
                if let Some(idx) = line.rfind("STATUS:") {
                  let time_str_dirty = &line[idx + 7..];

                  // Split by the first carriage return and take the clean number
                  let time_str_clean = time_str_dirty.split('\r').next().unwrap_or("").trim();

                  #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
                  if let Ok(seconds) = time_str_clean.parse::<f64>().map(|s| s as u64)
                    && seconds.saturating_sub(last_reported_time) >= 15
                  {
                    last_reported_time = seconds;

                    update_watch_progress(
                      &jellyfin_url,
                      &auth_header,
                      client.clone(),
                      &episode_id,
                      &user_id,
                      &mut debug_file,
                      last_reported_time,
                    )
                    .await;
                  }
                }
              }
            }
            buf.clear();

            // update_watch_progress(
            //   jellyfin_url,
            //   auth_header,
            //   client,
            //   episode_id,
            //   user_id,
            //   &mut debug_file,
            //   last_reported_time,
            // )
            // .await;
          }

          let _e = child.wait().await;
          let _e = writeln!(debug_file, "--- MPV SESSION ENDED ---");
        });
      }
    }
  }
}

// Could not decide if i want this in a loop or not so i made it into a func i can move around xd
async fn update_watch_progress(
  jellyfin_url: &str,
  auth_header: &str,
  client: Client,
  episode_id: &str,
  user_id: &str,
  debug_file: &mut Box<dyn Write + Send + 'static>,
  last_reported_time: u64,
) {
  if last_reported_time > 0 {
    let ticks = last_reported_time * 10_000_000;

    // The proper API call for stopping playback is a DELETE request
    let stop_url =
      format!("{jellyfin_url}/Users/{user_id}/PlayingItems/{episode_id}?PositionTicks={ticks}");

    let response = client
      .delete(&stop_url)
      .header("Authorization", auth_header)
      .send()
      .await;

    match response {
      Ok(res) => {
        let _e = writeln!(
          debug_file,
          "Sent final stop tick at {} sec | Jellyfin Status: {}",
          last_reported_time,
          res.status()
        );
      }
      Err(e) => {
        let _e = writeln!(debug_file, "Failed to send stop tick: {e}");
      }
    }
  }
}
