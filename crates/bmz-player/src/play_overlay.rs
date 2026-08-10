use std::collections::{HashMap, HashSet, VecDeque, hash_map::Entry};
use std::net::SocketAddr;
use std::time::Instant;

use bmz_core::input::ScratchDirection;
use bmz_core::lane::{KeyMode, LANE_COUNT, Lane};
use bmz_gameplay::input::backend::{DeviceId, PhysicalControl};
use bmz_render::scene::AppSceneSnapshot;
use futures_util::SinkExt;
use serde::Serialize;
use tokio::net::TcpListener;
use tokio::sync::watch;
use tokio::task::JoinHandle;
use tokio_tungstenite::accept_async;
use tokio_tungstenite::tungstenite::Message;

use crate::config::play_input::lane_binding_for_key_mode;
use crate::config::profile_config::{
    PlayAnalysisConfig, PlayAnalysisControllerModeConfig, PlayAnalysisReleaseDisplayModeConfig,
    ProfileInputConfig,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlayOverlayServerConfig {
    pub enabled: bool,
    pub port: u16,
}

impl From<&PlayAnalysisConfig> for PlayOverlayServerConfig {
    fn from(config: &PlayAnalysisConfig) -> Self {
        Self { enabled: config.websocket_enabled, port: config.websocket_port }
    }
}

#[derive(Debug)]
pub struct PlayOverlayController {
    applied: PlayOverlayServerConfig,
    tx: watch::Sender<String>,
    task: Option<JoinHandle<()>>,
}

impl Default for PlayOverlayController {
    fn default() -> Self {
        let (tx, _rx) = watch::channel(String::new());
        Self { applied: PlayOverlayServerConfig { enabled: false, port: 0 }, tx, task: None }
    }
}

impl PlayOverlayController {
    pub fn apply_config(&mut self, config: PlayOverlayServerConfig) {
        if self.applied == config {
            return;
        }
        self.stop();
        self.applied = config;
        if !config.enabled {
            tracing::info!("play overlay WebSocket disabled");
            return;
        }
        let rx = self.tx.subscribe();
        self.task = Some(tokio::spawn(async move {
            run_websocket_server(config.port, rx).await;
        }));
        tracing::info!(port = config.port, "play overlay WebSocket enabled");
    }

    pub fn publish(&self, payload: &PlayOverlayPayload) {
        if !self.applied.enabled {
            return;
        }
        match serde_json::to_string(payload) {
            Ok(message) => {
                let _ = self.tx.send(message);
            }
            Err(error) => {
                tracing::warn!(%error, "failed to encode play overlay payload");
            }
        }
    }

    fn stop(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

impl Drop for PlayOverlayController {
    fn drop(&mut self) {
        self.stop();
    }
}

async fn run_websocket_server(port: u16, rx: watch::Receiver<String>) {
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let listener = match TcpListener::bind(addr).await {
        Ok(listener) => listener,
        Err(error) => {
            tracing::warn!(%error, port, "failed to bind play overlay WebSocket");
            return;
        }
    };
    loop {
        let (stream, peer) = match listener.accept().await {
            Ok(accepted) => accepted,
            Err(error) => {
                tracing::warn!(%error, "failed to accept play overlay WebSocket client");
                continue;
            }
        };
        let client_rx = rx.clone();
        tokio::spawn(async move {
            if let Err(error) = serve_websocket_client(stream, client_rx).await {
                tracing::debug!(%error, %peer, "play overlay WebSocket client disconnected");
            }
        });
    }
}

async fn serve_websocket_client(
    stream: tokio::net::TcpStream,
    mut rx: watch::Receiver<String>,
) -> anyhow::Result<()> {
    let mut socket = accept_async(stream).await?;
    let initial = rx.borrow().clone();
    if !initial.is_empty() {
        socket.send(Message::Text(initial)).await?;
    }
    while rx.changed().await.is_ok() {
        let message = rx.borrow().clone();
        if message.is_empty() {
            continue;
        }
        socket.send(Message::Text(message)).await?;
    }
    Ok(())
}

#[derive(Debug, Clone, Default)]
pub struct PlayOverlayState {
    counted_pressed_inputs: HashMap<PhysicalInputKey, Lane>,
    pressed_since: HashMap<PhysicalInputKey, PressSampleStart>,
    release_samples: VecDeque<ReleaseSample>,
    last_release_by_lane: [Option<f32>; LANE_COUNT],
    total_lane_presses: [u64; LANE_COUNT],
}

impl PlayOverlayState {
    pub fn build_payload(
        &mut self,
        config: &PlayAnalysisConfig,
        input_config: &ProfileInputConfig,
        scene: &AppSceneSnapshot,
        pressed_play_inputs: &[(DeviceId, PhysicalControl)],
    ) -> PlayOverlayPayload {
        let active = self.observe_inputs(config, input_config, pressed_play_inputs);
        PlayOverlayPayload {
            version: 1,
            generated_at_ms: unix_time_ms(),
            enabled: config.websocket_enabled,
            controller_mode: controller_mode_label(config.controller_mode).to_string(),
            release_display_mode: release_display_mode_label(config.release_display_mode)
                .to_string(),
            summary: PlayOverlaySummary::from_scene(scene),
            lanes: overlay_lanes(config.controller_mode)
                .iter()
                .map(|&lane| PlayOverlayLanePayload {
                    id: lane_id(lane).to_string(),
                    label: lane_label(config.controller_mode, lane).to_string(),
                    pressed: active.lanes.contains(&lane),
                    scratch_up: active.scratch_up.contains(&lane),
                    scratch_down: active.scratch_down.contains(&lane),
                    release_ms: self.last_release_by_lane[lane.index()],
                    total_presses: self.total_lane_presses[lane.index()],
                })
                .collect(),
            lane_pattern: lane_pattern_from_scene(scene),
        }
    }

    fn observe_inputs(
        &mut self,
        config: &PlayAnalysisConfig,
        input_config: &ProfileInputConfig,
        pressed_play_inputs: &[(DeviceId, PhysicalControl)],
    ) -> ActiveOverlayInputs {
        let now = Instant::now();
        let active =
            resolve_active_inputs(input_config, config.controller_mode, pressed_play_inputs);
        let pressed = pressed_play_inputs
            .iter()
            .map(|(device, control)| PhysicalInputKey { device: *device, control: control.clone() })
            .collect::<HashSet<_>>();
        for key in &pressed {
            let Some(resolved) = active.by_input.get(key).copied() else {
                continue;
            };
            if let Some(lane) = resolved.lane
                && let Entry::Vacant(entry) = self.counted_pressed_inputs.entry(key.clone())
            {
                entry.insert(lane);
                self.total_lane_presses[lane.index()] =
                    self.total_lane_presses[lane.index()].saturating_add(1);
            }
            let Some(lane) =
                resolved.lane.filter(|lane| release_average_lane(config.controller_mode, *lane))
            else {
                continue;
            };
            if let Entry::Vacant(entry) = self.pressed_since.entry(key.clone()) {
                entry.insert(PressSampleStart { started_at: now, lane });
            }
        }
        let released = self
            .pressed_since
            .keys()
            .filter(|key| !pressed.contains(*key))
            .cloned()
            .collect::<Vec<_>>();
        self.counted_pressed_inputs.retain(|key, _| pressed.contains(key));
        let mut updated_release_lanes = Vec::new();
        for key in released {
            if let Some(started) = self.pressed_since.remove(&key) {
                let held_ms = now.duration_since(started.started_at).as_secs_f32() * 1000.0;
                if held_ms <= config.release_ignore_threshold_ms.max(1) as f32 {
                    self.release_samples.push_back(ReleaseSample {
                        released_at: now,
                        lane: started.lane,
                        held_ms,
                    });
                    updated_release_lanes.push(started.lane);
                }
            }
        }
        let window = config.release_window_ms.max(100) as f32;
        while self.release_samples.front().is_some_and(|sample| {
            now.duration_since(sample.released_at).as_secs_f32() * 1000.0 > window
        }) {
            self.release_samples.pop_front();
        }
        for lane in updated_release_lanes {
            self.last_release_by_lane[lane.index()] = self.lane_average_ms(lane);
        }
        active
    }

    fn lane_average_ms(&self, lane: Lane) -> Option<f32> {
        let mut sum = 0.0;
        let mut count = 0_u32;
        for sample in self.release_samples.iter().filter(|sample| sample.lane == lane) {
            sum += sample.held_ms;
            count += 1;
        }
        (count > 0).then_some(sum / count as f32)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct PhysicalInputKey {
    device: DeviceId,
    control: PhysicalControl,
}

#[derive(Debug, Clone, Copy)]
struct PressSampleStart {
    started_at: Instant,
    lane: Lane,
}

#[derive(Debug, Clone, Copy)]
struct ReleaseSample {
    released_at: Instant,
    lane: Lane,
    held_ms: f32,
}

#[derive(Debug, Clone, Serialize)]
pub struct PlayOverlayPayload {
    pub version: u32,
    pub generated_at_ms: u128,
    pub enabled: bool,
    pub controller_mode: String,
    pub release_display_mode: String,
    pub summary: PlayOverlaySummary,
    pub lanes: Vec<PlayOverlayLanePayload>,
    pub lane_pattern: Vec<u8>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PlayOverlaySummary {
    pub scene: &'static str,
    pub title: String,
    pub notes: u32,
    pub play_count: u32,
    pub ex_score: Option<u32>,
    pub score_rate: Option<f32>,
}

impl PlayOverlaySummary {
    fn from_scene(scene: &AppSceneSnapshot) -> Self {
        match scene {
            AppSceneSnapshot::Select(snapshot) => {
                let row = snapshot
                    .rows
                    .iter()
                    .find(|row| row.index == snapshot.selected_index)
                    .or_else(|| snapshot.rows.first());
                let (title, notes, play_count, ex_score) = row
                    .map(|row| (row.title.clone(), row.total_notes, row.play_count, row.ex_score))
                    .unwrap_or_else(|| (String::new(), 0, 0, None));
                Self {
                    scene: "Select",
                    title,
                    notes,
                    play_count,
                    ex_score,
                    score_rate: score_rate(ex_score, notes),
                }
            }
            AppSceneSnapshot::Decide(snapshot) => Self {
                scene: "Decide",
                title: snapshot.title.clone(),
                notes: snapshot.total_notes,
                play_count: 0,
                ex_score: Some(snapshot.ex_score),
                score_rate: score_rate(Some(snapshot.ex_score), snapshot.total_notes),
            },
            AppSceneSnapshot::Play(snapshot) => Self {
                scene: "Play",
                title: snapshot.title.clone(),
                notes: snapshot.total_notes,
                play_count: 0,
                ex_score: Some(snapshot.ex_score),
                score_rate: score_rate(Some(snapshot.ex_score), snapshot.total_notes),
            },
            AppSceneSnapshot::Result(snapshot) => Self {
                scene: "Result",
                title: snapshot.title.clone(),
                notes: snapshot.total_notes,
                play_count: 0,
                ex_score: Some(snapshot.ex_score),
                score_rate: Some(snapshot.ex_score_rate),
            },
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct PlayOverlayLanePayload {
    pub id: String,
    pub label: String,
    pub pressed: bool,
    pub scratch_up: bool,
    pub scratch_down: bool,
    pub release_ms: Option<f32>,
    pub total_presses: u64,
}

#[derive(Debug, Clone, Copy, Default)]
struct ResolvedInput {
    lane: Option<Lane>,
}

#[derive(Debug, Default)]
struct ActiveOverlayInputs {
    lanes: HashSet<Lane>,
    scratch_up: HashSet<Lane>,
    scratch_down: HashSet<Lane>,
    by_input: HashMap<PhysicalInputKey, ResolvedInput>,
}

fn resolve_active_inputs(
    input_config: &ProfileInputConfig,
    mode: PlayAnalysisControllerModeConfig,
    pressed_play_inputs: &[(DeviceId, PhysicalControl)],
) -> ActiveOverlayInputs {
    let mut active = ActiveOverlayInputs::default();
    for &key_mode in controller_binding_key_modes(mode) {
        let Ok(binding) = lane_binding_for_key_mode(input_config, key_mode) else {
            continue;
        };
        for (device, control) in pressed_play_inputs {
            if let Some(resolved) = binding.resolve_entry(*device, control) {
                let Some(display_lane) = controller_display_lane(mode, resolved.lane) else {
                    continue;
                };
                active.lanes.insert(display_lane);
                if resolved.scratch_direction == Some(ScratchDirection::Up) {
                    active.scratch_up.insert(display_lane);
                }
                if resolved.scratch_direction == Some(ScratchDirection::Down) {
                    active.scratch_down.insert(display_lane);
                }
                active.by_input.insert(
                    PhysicalInputKey { device: *device, control: control.clone() },
                    ResolvedInput { lane: Some(display_lane) },
                );
            }
        }
    }
    active
}

fn controller_binding_key_modes(mode: PlayAnalysisControllerModeConfig) -> &'static [KeyMode] {
    match mode {
        PlayAnalysisControllerModeConfig::Key7P1 | PlayAnalysisControllerModeConfig::Key7P2 => {
            &[KeyMode::K7, KeyMode::K14]
        }
        PlayAnalysisControllerModeConfig::Key14 => &[KeyMode::K14],
    }
}

fn controller_display_lane(mode: PlayAnalysisControllerModeConfig, lane: Lane) -> Option<Lane> {
    match mode {
        PlayAnalysisControllerModeConfig::Key7P1 | PlayAnalysisControllerModeConfig::Key7P2 => {
            Some(match lane {
                Lane::Scratch | Lane::Scratch2 => Lane::Scratch,
                Lane::Key1 | Lane::Key8 => Lane::Key1,
                Lane::Key2 | Lane::Key9 => Lane::Key2,
                Lane::Key3 | Lane::Key10 => Lane::Key3,
                Lane::Key4 | Lane::Key11 => Lane::Key4,
                Lane::Key5 | Lane::Key12 => Lane::Key5,
                Lane::Key6 | Lane::Key13 => Lane::Key6,
                Lane::Key7 | Lane::Key14 => Lane::Key7,
            })
        }
        PlayAnalysisControllerModeConfig::Key14 => {
            overlay_lanes(mode).contains(&lane).then_some(lane)
        }
    }
}

fn release_average_lane(mode: PlayAnalysisControllerModeConfig, lane: Lane) -> bool {
    match mode {
        PlayAnalysisControllerModeConfig::Key7P1 | PlayAnalysisControllerModeConfig::Key7P2 => {
            matches!(
                lane,
                Lane::Key1
                    | Lane::Key2
                    | Lane::Key3
                    | Lane::Key4
                    | Lane::Key5
                    | Lane::Key6
                    | Lane::Key7
            )
        }
        PlayAnalysisControllerModeConfig::Key14 => {
            matches!(
                lane,
                Lane::Key1
                    | Lane::Key2
                    | Lane::Key3
                    | Lane::Key4
                    | Lane::Key5
                    | Lane::Key6
                    | Lane::Key7
                    | Lane::Key8
                    | Lane::Key9
                    | Lane::Key10
                    | Lane::Key11
                    | Lane::Key12
                    | Lane::Key13
                    | Lane::Key14
            )
        }
    }
}

fn overlay_lanes(mode: PlayAnalysisControllerModeConfig) -> &'static [Lane] {
    match mode {
        PlayAnalysisControllerModeConfig::Key7P1 | PlayAnalysisControllerModeConfig::Key7P2 => &[
            Lane::Scratch,
            Lane::Key1,
            Lane::Key2,
            Lane::Key3,
            Lane::Key4,
            Lane::Key5,
            Lane::Key6,
            Lane::Key7,
        ],
        PlayAnalysisControllerModeConfig::Key14 => &[
            Lane::Scratch,
            Lane::Key1,
            Lane::Key2,
            Lane::Key3,
            Lane::Key4,
            Lane::Key5,
            Lane::Key6,
            Lane::Key7,
            Lane::Scratch2,
            Lane::Key8,
            Lane::Key9,
            Lane::Key10,
            Lane::Key11,
            Lane::Key12,
            Lane::Key13,
            Lane::Key14,
        ],
    }
}

fn lane_id(lane: Lane) -> &'static str {
    match lane {
        Lane::Scratch => "scratch",
        Lane::Key1 => "key1",
        Lane::Key2 => "key2",
        Lane::Key3 => "key3",
        Lane::Key4 => "key4",
        Lane::Key5 => "key5",
        Lane::Key6 => "key6",
        Lane::Key7 => "key7",
        Lane::Scratch2 => "scratch2",
        Lane::Key8 => "key8",
        Lane::Key9 => "key9",
        Lane::Key10 => "key10",
        Lane::Key11 => "key11",
        Lane::Key12 => "key12",
        Lane::Key13 => "key13",
        Lane::Key14 => "key14",
    }
}

fn lane_label(mode: PlayAnalysisControllerModeConfig, lane: Lane) -> &'static str {
    match (mode, lane) {
        (_, Lane::Scratch) => "SC",
        (_, Lane::Scratch2) => "SC2",
        (
            PlayAnalysisControllerModeConfig::Key7P1 | PlayAnalysisControllerModeConfig::Key7P2,
            Lane::Key1,
        ) => "1",
        (
            PlayAnalysisControllerModeConfig::Key7P1 | PlayAnalysisControllerModeConfig::Key7P2,
            Lane::Key2,
        ) => "2",
        (
            PlayAnalysisControllerModeConfig::Key7P1 | PlayAnalysisControllerModeConfig::Key7P2,
            Lane::Key3,
        ) => "3",
        (
            PlayAnalysisControllerModeConfig::Key7P1 | PlayAnalysisControllerModeConfig::Key7P2,
            Lane::Key4,
        ) => "4",
        (
            PlayAnalysisControllerModeConfig::Key7P1 | PlayAnalysisControllerModeConfig::Key7P2,
            Lane::Key5,
        ) => "5",
        (
            PlayAnalysisControllerModeConfig::Key7P1 | PlayAnalysisControllerModeConfig::Key7P2,
            Lane::Key6,
        ) => "6",
        (
            PlayAnalysisControllerModeConfig::Key7P1 | PlayAnalysisControllerModeConfig::Key7P2,
            Lane::Key7,
        ) => "7",
        _ => lane_id(lane).trim_start_matches("key"),
    }
}

fn controller_mode_label(mode: PlayAnalysisControllerModeConfig) -> &'static str {
    match mode {
        PlayAnalysisControllerModeConfig::Key7P1 => "7K (1P)",
        PlayAnalysisControllerModeConfig::Key7P2 => "7K (2P)",
        PlayAnalysisControllerModeConfig::Key14 => "14K",
    }
}

fn release_display_mode_label(mode: PlayAnalysisReleaseDisplayModeConfig) -> &'static str {
    match mode {
        PlayAnalysisReleaseDisplayModeConfig::ReleaseOnly => "release",
        PlayAnalysisReleaseDisplayModeConfig::ReleaseAndNotes => "release-and-count",
        PlayAnalysisReleaseDisplayModeConfig::NotesOnly => "count",
    }
}

fn lane_pattern_from_scene(scene: &AppSceneSnapshot) -> Vec<u8> {
    match scene {
        AppSceneSnapshot::Select(snapshot) => snapshot.lane_shuffle_pattern.clone(),
        AppSceneSnapshot::Decide(snapshot) | AppSceneSnapshot::Play(snapshot) => {
            snapshot.lane_shuffle_pattern.clone()
        }
        AppSceneSnapshot::Result(snapshot) => snapshot.lane_shuffle_pattern.clone(),
    }
}

fn score_rate(ex_score: Option<u32>, total_notes: u32) -> Option<f32> {
    let ex_score = ex_score?;
    (total_notes > 0).then_some(ex_score as f32 / (total_notes as f32 * 2.0))
}

fn unix_time_ms() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or_default()
}
