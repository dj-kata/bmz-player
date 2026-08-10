use std::collections::{BTreeMap, HashMap, HashSet, VecDeque, hash_map::Entry};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use bmz_core::clear::ClearType;
use bmz_core::input::ScratchDirection;
use bmz_core::lane::{KeyMode, Lane};
use bmz_gameplay::input::backend::{DeviceId, PhysicalControl};
use bmz_render::scene::{AppSceneSnapshot, ResultSnapshot};
use bmz_render::snapshot::RenderSnapshot;

use crate::config::play_input::lane_binding_for_key_mode;
use crate::config::profile_config::{
    PlayAnalysisConfig, PlayAnalysisControllerModeConfig, ProfileInputConfig,
};
use crate::storage::difficulty_table_db::DifficultyTableRecord;

use super::*;

#[derive(Debug)]
pub(super) struct PlayAnalysisPanelState {
    started_at: Instant,
    started_at_unix: i64,
    daily_press_day: i64,
    daily_lane_presses: [u64; bmz_core::lane::LANE_COUNT],
    selected_history_id: Option<i64>,
    counted_pressed_inputs: HashMap<PhysicalInputKey, Lane>,
    pressed_since: HashMap<PhysicalInputKey, PressSampleStart>,
    release_samples: VecDeque<ReleaseSample>,
    last_release_by_lane: [Option<f32>; bmz_core::lane::LANE_COUNT],
    tweet_status: String,
}

impl Default for PlayAnalysisPanelState {
    fn default() -> Self {
        Self {
            started_at: Instant::now(),
            started_at_unix: unix_now(),
            daily_press_day: local_day_key(),
            daily_lane_presses: [0; bmz_core::lane::LANE_COUNT],
            selected_history_id: None,
            counted_pressed_inputs: HashMap::new(),
            pressed_since: HashMap::new(),
            release_samples: VecDeque::new(),
            last_release_by_lane: [None; bmz_core::lane::LANE_COUNT],
            tweet_status: String::new(),
        }
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
    lane: Option<Lane>,
}

#[derive(Debug, Clone, Copy)]
struct ReleaseSample {
    released_at: Instant,
    lane: Option<Lane>,
    held_ms: f32,
}

#[derive(Debug, Clone)]
struct HistoryDisplayRow {
    local_day: String,
    entry: crate::storage::score_db::ScoreHistoryEntry,
    title: String,
    difficulty: String,
}

#[derive(Debug, Default)]
pub(super) struct PlayAnalysisPanelActions {
    pub(super) save_profile: bool,
}

pub(super) struct PlayAnalysisPanelContext<'a> {
    pub(super) scene: &'a AppSceneSnapshot,
    pub(super) score_db: &'a crate::storage::score_db::ScoreDatabase,
    pub(super) library_db: &'a crate::storage::library_db::LibraryDatabase,
    pub(super) difficulty_tables: &'a [DifficultyTableRecord],
    pub(super) input_config: &'a ProfileInputConfig,
    pub(super) connected_gamepads: &'a [crate::input::gamepad::ConnectedGamepad],
    pub(super) pressed_controls: &'a [String],
    pub(super) pressed_play_inputs: &'a [(DeviceId, PhysicalControl)],
}

pub(super) fn build_play_analysis_panel(
    ctx: &egui::Context,
    visible: &mut bool,
    state: &mut PlayAnalysisPanelState,
    config: &mut PlayAnalysisConfig,
    panel: PlayAnalysisPanelContext<'_>,
    _text: Localizer,
) -> PlayAnalysisPanelActions {
    if !*visible {
        return PlayAnalysisPanelActions::default();
    }

    state.observe_releases(config, panel.input_config, panel.pressed_play_inputs);
    let mut actions = PlayAnalysisPanelActions::default();

    play_analysis_window(ctx, visible).show(ctx, |ui| {
        let max_height = ui.available_rect_before_wrap().height().max(64.0);
        egui::ScrollArea::vertical().max_height(max_height).show(ui, |ui| {
            if config.compact_mode {
                actions.save_profile |= build_compact_view(ui, state, config, &panel);
                return;
            }

            if ui.checkbox(&mut config.compact_mode, "コンパクトモード").changed() {
                actions.save_profile = true;
            }
            if ui.checkbox(&mut config.open_on_startup, "起動時に自動で開く").changed() {
                actions.save_profile = true;
            }

            egui::CollapsingHeader::new("現在の状態")
                .default_open(true)
                .show(ui, |ui| build_current_view(ui, panel.scene));
            egui::CollapsingHeader::new("成果ツイート").default_open(true).show(ui, |ui| {
                actions.save_profile |= build_tweet_view(
                    ui,
                    state,
                    config,
                    panel.score_db,
                    panel.library_db,
                    panel.difficulty_tables,
                );
            });
            egui::CollapsingHeader::new("ノーツ数").default_open(true).show(ui, |ui| {
                build_notes_view(ui, state, config, panel.score_db);
            });
            egui::CollapsingHeader::new("コントローラ").default_open(true).show(ui, |ui| {
                actions.save_profile |= build_controller_view(
                    ui,
                    state,
                    config,
                    panel.input_config,
                    panel.connected_gamepads,
                    panel.pressed_controls,
                    panel.pressed_play_inputs,
                );
            });
            egui::CollapsingHeader::new("プレー履歴")
                .default_open(true)
                .show(ui, |ui| build_history_view(ui, state, panel.score_db, panel.library_db));
        });
    });

    actions
}

fn build_compact_view(
    ui: &mut egui::Ui,
    state: &mut PlayAnalysisPanelState,
    config: &mut PlayAnalysisConfig,
    panel: &PlayAnalysisPanelContext<'_>,
) -> bool {
    build_today_notes_compact(ui, panel.score_db);
    ui.separator();
    build_current_lane_pattern(ui, panel.scene);
    ui.separator();
    let active = resolve_active_inputs(
        panel.input_config,
        config.controller_mode,
        panel.pressed_play_inputs,
    );
    build_controller_layout(ui, state, config, &active);
    ui.separator();
    let mut changed = false;
    if ui.button("フルモードに戻る").clicked() {
        config.compact_mode = false;
        changed = true;
    }
    changed
}

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs().min(i64::MAX as u64) as i64)
        .unwrap_or_default()
}

fn local_day_key() -> i64 {
    unix_now() / 86_400
}

fn play_analysis_window<'open>(
    ctx: &egui::Context,
    visible: &'open mut bool,
) -> egui::Window<'open> {
    let constrain = ctx.content_rect().shrink(PANEL_VIEWPORT_MARGIN);
    let chrome = panel_window_chrome(ctx);
    let (default_inner, max_inner, clamped_default_pos) =
        clamp_panel_layout(constrain, chrome, 520.0, 760.0, egui::pos2(96.0, 48.0));
    let window_id = egui::Id::new("bmz_play_analysis_v3");
    let pos = ctx
        .memory(|memory| memory.area_rect(window_id))
        .map(|rect| constrain_window_rect_to_area(rect, constrain).min)
        .unwrap_or(clamped_default_pos);
    egui::Window::new("プレー分析 (F7)")
        .id(window_id)
        .open(visible)
        .resizable(true)
        .constrain_to(constrain)
        .current_pos(pos)
        .default_size(default_inner)
        .max_size(max_inner)
        .min_size([240.0, 120.0])
}

fn build_notes_view(
    ui: &mut egui::Ui,
    state: &PlayAnalysisPanelState,
    config: &PlayAnalysisConfig,
    score_db: &crate::storage::score_db::ScoreDatabase,
) {
    let today = score_db.note_count_today().ok();
    let months = score_db.monthly_note_counts(12).unwrap_or_default();
    let years = score_db.yearly_note_counts(8).unwrap_or_default();

    ui.horizontal_wrapped(|ui| {
        note_aggregate_block(ui, "今日", |ui| match today {
            Some(row) => note_aggregate_line(ui, &row),
            None => {
                ui.label("読み込み失敗");
            }
        });
        note_aggregate_block(ui, "月次", |ui| {
            for row in &months {
                note_aggregate_line(ui, row);
            }
        });
        note_aggregate_block(ui, "年次", |ui| {
            for row in &years {
                note_aggregate_line(ui, row);
            }
        });
    });

    ui.separator();
    ui.heading("レーンごとの入力数");
    ui.horizontal_wrapped(|ui| {
        for &lane in note_count_display_lanes(config.controller_mode) {
            let count = state.daily_lane_presses[lane.index()];
            let label = lane_count_label(config.controller_mode, lane);
            ui.label(format!("{label}: {count}"));
        }
    });
    ui.small("このウィンドウを開いている間に今日押した回数です。押下開始時に1回だけ加算します。");
}

fn note_aggregate_line(ui: &mut egui::Ui, row: &crate::storage::score_db::NoteCountAggregate) {
    ui.label(format!("{}  {:>8} notes  {} plays", row.label, row.total_notes, row.play_count));
}

fn build_today_notes_compact(
    ui: &mut egui::Ui,
    score_db: &crate::storage::score_db::ScoreDatabase,
) {
    ui.heading("本日のノーツ数");
    match score_db.note_count_today() {
        Ok(row) => {
            ui.horizontal_wrapped(|ui| {
                ui.label(egui::RichText::new(format_u64(row.total_notes)).strong().size(20.0));
                ui.label("notes");
                ui.label(format!("{} plays", row.play_count));
            });
        }
        Err(_) => {
            ui.label("読み込み失敗");
        }
    }
}

fn note_aggregate_block(ui: &mut egui::Ui, title: &str, add_contents: impl FnOnce(&mut egui::Ui)) {
    ui.vertical(|ui| {
        ui.heading(title);
        add_contents(ui);
    });
}

fn build_tweet_view(
    ui: &mut egui::Ui,
    state: &mut PlayAnalysisPanelState,
    config: &mut PlayAnalysisConfig,
    score_db: &crate::storage::score_db::ScoreDatabase,
    library_db: &crate::storage::library_db::LibraryDatabase,
    difficulty_tables: &[DifficultyTableRecord],
) -> bool {
    let mut changed = false;
    if config.tweet_table_sources.is_empty() && !difficulty_tables.is_empty() {
        config.tweet_table_sources =
            difficulty_tables.iter().map(|table| table.source_url.clone()).collect();
        changed = true;
    }

    let tweet = build_achievement_tweet(state, config, score_db, library_db, difficulty_tables);
    if ui.button("ツイート画面を開く").clicked() {
        let url = format!("https://twitter.com/intent/tweet?text={}", percent_encode_query(&tweet));
        state.tweet_status = match webbrowser::open(&url) {
            Ok(_) => "ブラウザでツイート画面を開きました。".to_string(),
            Err(error) => format!("ブラウザを開けませんでした: {error}"),
        };
    }
    if !state.tweet_status.is_empty() {
        ui.small(&state.tweet_status);
    }

    egui::CollapsingHeader::new("集計対象の難易度表").default_open(false).show(ui, |ui| {
        if difficulty_tables.is_empty() {
            ui.small("読み込まれている難易度表がありません。");
        } else {
            for table in difficulty_tables {
                let label = if table.name.trim().is_empty() {
                    table.source_url.as_str()
                } else {
                    table.name.as_str()
                };
                let mut enabled = config.tweet_table_sources.contains(&table.source_url);
                if ui.checkbox(&mut enabled, label).changed() {
                    if enabled {
                        config.tweet_table_sources.push(table.source_url.clone());
                    } else {
                        config.tweet_table_sources.retain(|source| source != &table.source_url);
                    }
                    changed = true;
                }
            }
        }
    });

    ui.separator();
    ui.label("ツイート文字列");
    let mut preview = tweet;
    let preview_width = ui.available_width().clamp(220.0, 440.0);
    ui.add(
        egui::TextEdit::multiline(&mut preview)
            .desired_rows(8)
            .desired_width(preview_width)
            .interactive(false),
    );
    changed
}

fn build_achievement_tweet(
    state: &PlayAnalysisPanelState,
    config: &PlayAnalysisConfig,
    score_db: &crate::storage::score_db::ScoreDatabase,
    library_db: &crate::storage::library_db::LibraryDatabase,
    difficulty_tables: &[DifficultyTableRecord],
) -> String {
    let history = score_db.local_history_since(state.started_at_unix).unwrap_or_default();
    let plays = history.len() as u64;
    let notes = history.iter().map(|entry| u64::from(entry.total_notes)).sum::<u64>();
    let ex_score = history.iter().map(|entry| u64::from(entry.ex_score)).sum::<u64>();
    let rate = if notes > 0 { ex_score as f64 * 100.0 / (notes as f64 * 2.0) } else { 0.0 };
    let uptime = state.started_at.elapsed();
    let pace =
        if uptime.as_secs() > 0 { notes as f64 * 3600.0 / uptime.as_secs_f64() } else { 0.0 };

    let mut lines = vec![format!("plays:{plays}, notes: {}, {:.2}%", format_u64(notes), rate)];
    if let Some(month) =
        score_db.monthly_note_counts(1).ok().and_then(|rows| rows.into_iter().next())
    {
        lines.push(format!(
            "({}: {})",
            month.label.replace('-', "/"),
            format_u64(month.total_notes)
        ));
    }
    lines.push(format!(
        "uptime: {}, pace: {}notes/h",
        format_duration_hms(uptime.as_secs()),
        format_u64(pace.round().max(0.0) as u64)
    ));
    lines.extend(lamp_update_lines(&history, config, library_db, difficulty_tables));
    lines.push("#bmz_player".to_string());
    lines.join("\n")
}

fn lamp_update_lines(
    history: &[crate::storage::score_db::ScoreHistoryEntry],
    config: &PlayAnalysisConfig,
    library_db: &crate::storage::library_db::LibraryDatabase,
    difficulty_tables: &[DifficultyTableRecord],
) -> Vec<String> {
    let enabled = config.tweet_table_sources.iter().cloned().collect::<HashSet<_>>();
    let table_rank = difficulty_tables
        .iter()
        .enumerate()
        .map(|(index, table)| (table.source_url.as_str(), index))
        .collect::<HashMap<_, _>>();
    let level_rank = difficulty_tables
        .iter()
        .map(|table| {
            let ranks = table
                .level_order
                .iter()
                .enumerate()
                .map(|(index, level)| (level.as_str(), index))
                .collect::<HashMap<_, _>>();
            (table.source_url.as_str(), ranks)
        })
        .collect::<HashMap<_, _>>();
    let mut counts = BTreeMap::<(usize, usize, String), BTreeMap<u8, (&'static str, u32)>>::new();

    for entry in history {
        let old_rank = entry
            .previous_best
            .as_ref()
            .map(|best| ClearType::rank_from_label(&best.clear_type))
            .unwrap_or(0);
        let new_rank = ClearType::rank_from_label(&entry.clear_type);
        if new_rank <= old_rank {
            continue;
        }
        let Some(clear_label) = ClearType::from_label(&entry.clear_type).map(clear_tweet_label)
        else {
            continue;
        };
        let sha256 = crate::storage::common::hash_to_hex(&entry.chart_sha256);
        let Ok(table_entries) =
            library_db.list_difficulty_table_entries_by_sha256s(&[sha256.as_str()])
        else {
            continue;
        };
        let mut seen = HashSet::<(String, String)>::new();
        for table_entry in table_entries {
            if !enabled.contains(&table_entry.source_url) {
                continue;
            }
            if !seen.insert((table_entry.source_url.clone(), table_entry.level.clone())) {
                continue;
            }
            let source_rank =
                table_rank.get(table_entry.source_url.as_str()).copied().unwrap_or(usize::MAX);
            let level_rank = level_rank
                .get(table_entry.source_url.as_str())
                .and_then(|levels| levels.get(table_entry.level.as_str()))
                .copied()
                .unwrap_or(usize::MAX);
            let label = format!("{}{}", table_entry.table_symbol, table_entry.level);
            let entry = counts.entry((source_rank, level_rank, label)).or_default();
            entry.entry(new_rank).and_modify(|(_, count)| *count += 1).or_insert((clear_label, 1));
        }
    }

    counts
        .into_iter()
        .map(|((_, _, level), lamps)| {
            let body = lamps
                .into_iter()
                .rev()
                .map(|(_, (lamp, count))| format!("{lamp}+{count}"))
                .collect::<Vec<_>>()
                .join(", ");
            format!("{level}: {body},")
        })
        .collect()
}

fn clear_tweet_label(clear: ClearType) -> &'static str {
    match clear {
        ClearType::NoPlay => "NP",
        ClearType::Failed => "F",
        ClearType::AssistEasy => "A",
        ClearType::LightAssistEasy => "LA",
        ClearType::Easy => "E",
        ClearType::Normal => "C",
        ClearType::Hard => "H",
        ClearType::ExHard => "EXH",
        ClearType::FullCombo => "FC",
        ClearType::Perfect => "P",
        ClearType::Max => "MAX",
    }
}

fn format_u64(value: u64) -> String {
    let text = value.to_string();
    let mut out = String::with_capacity(text.len() + text.len() / 3);
    for (index, ch) in text.chars().rev().enumerate() {
        if index > 0 && index % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    out.chars().rev().collect()
}

fn format_duration_hms(seconds: u64) -> String {
    let hours = seconds / 3600;
    let minutes = (seconds % 3600) / 60;
    let seconds = seconds % 60;
    format!("{hours}:{minutes:02}:{seconds:02}")
}

fn percent_encode_query(value: &str) -> String {
    let mut out = String::new();
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char);
            }
            b' ' => out.push_str("%20"),
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

fn build_current_view(ui: &mut egui::Ui, scene: &AppSceneSnapshot) {
    match scene {
        AppSceneSnapshot::Select(snapshot) => {
            let row = snapshot
                .rows
                .iter()
                .find(|row| row.index == snapshot.selected_index)
                .or_else(|| snapshot.rows.first());
            if let Some(row) = row {
                ui.heading(&row.title);
                let key_mode = row.chart_key_mode.unwrap_or(KeyMode::K7);
                two_col(ui, "難易度", difficulty_label(&row.table_text_secondary, &row.play_level));
                two_col(ui, "ベストランプ", row.clear_type.clone());
                two_col(ui, "スコア", row.ex_score.map_or("-".to_string(), |v| v.to_string()));
                two_col(ui, "ミスカウント", row.bp.map_or("-".to_string(), |v| v.to_string()));
                two_col(
                    ui,
                    "オプション",
                    play_arrange_options_label(
                        key_mode,
                        &snapshot.arrange,
                        &snapshot.arrange_2p,
                        None,
                    ),
                );
                lane_pattern_row(ui, key_mode, &snapshot.lane_shuffle_pattern);
            } else {
                ui.label("選択中の曲がありません。");
            }
        }
        AppSceneSnapshot::Decide(snapshot) | AppSceneSnapshot::Play(snapshot) => {
            build_render_snapshot_current(ui, snapshot);
        }
        AppSceneSnapshot::Result(snapshot) => build_result_snapshot_current(ui, snapshot),
    }
}

fn build_current_lane_pattern(ui: &mut egui::Ui, scene: &AppSceneSnapshot) {
    match scene {
        AppSceneSnapshot::Select(snapshot) => {
            let row = snapshot
                .rows
                .iter()
                .find(|row| row.index == snapshot.selected_index)
                .or_else(|| snapshot.rows.first());
            if let Some(row) = row {
                let key_mode = row.chart_key_mode.unwrap_or(KeyMode::K7);
                lane_pattern_row(ui, key_mode, &snapshot.lane_shuffle_pattern);
            } else {
                ui.label("レーン配置: -");
            }
        }
        AppSceneSnapshot::Decide(snapshot) | AppSceneSnapshot::Play(snapshot) => {
            lane_pattern_row(ui, snapshot.key_mode, &snapshot.lane_shuffle_pattern);
        }
        AppSceneSnapshot::Result(snapshot) => {
            lane_pattern_row(ui, snapshot.key_mode, &snapshot.lane_shuffle_pattern);
        }
    }
}

fn build_render_snapshot_current(ui: &mut egui::Ui, snapshot: &RenderSnapshot) {
    ui.heading(&snapshot.title);
    two_col(ui, "難易度", difficulty_label(&snapshot.table_text_secondary, &snapshot.play_level));
    two_col(ui, "スコア", snapshot.ex_score.to_string());
    two_col(
        ui,
        "ミスカウント",
        (snapshot.judge_counts.bad + snapshot.judge_counts.poor).to_string(),
    );
    two_col(
        ui,
        "オプション",
        play_arrange_options_label(
            snapshot.key_mode,
            &snapshot.arrange,
            &snapshot.arrange_2p,
            None,
        ),
    );
    lane_pattern_row(ui, snapshot.key_mode, &snapshot.lane_shuffle_pattern);
}

fn build_result_snapshot_current(ui: &mut egui::Ui, snapshot: &ResultSnapshot) {
    ui.heading(&snapshot.title);
    two_col(ui, "難易度", difficulty_label(&snapshot.table_text_secondary, &snapshot.play_level));
    two_col(
        ui,
        "ベストランプ",
        snapshot.best_clear_type.map_or("-".to_string(), |v| format!("{v:?}")),
    );
    two_col(ui, "スコア", snapshot.ex_score.to_string());
    two_col(ui, "ミスカウント", snapshot.bp.to_string());
    two_col(
        ui,
        "オプション",
        play_arrange_options_label(
            snapshot.key_mode,
            &snapshot.arrange,
            &snapshot.arrange_2p,
            Some(&snapshot.double_option),
        ),
    );
    lane_pattern_row(ui, snapshot.key_mode, &snapshot.lane_shuffle_pattern);
}

fn build_history_view(
    ui: &mut egui::Ui,
    state: &mut PlayAnalysisPanelState,
    score_db: &crate::storage::score_db::ScoreDatabase,
    library_db: &crate::storage::library_db::LibraryDatabase,
) {
    let rows = load_history_rows(score_db, library_db);
    ui.columns(2, |columns| {
        egui::ScrollArea::vertical().show(&mut columns[0], |ui| {
            let mut by_day = BTreeMap::<String, Vec<&HistoryDisplayRow>>::new();
            for row in &rows {
                by_day.entry(row.local_day.clone()).or_default().push(row);
            }
            for (day, day_rows) in by_day.iter().rev() {
                egui::CollapsingHeader::new(day).default_open(true).show(ui, |ui| {
                    for row in day_rows {
                        let selected = state.selected_history_id == Some(row.entry.id);
                        let response = ui.selectable_label(selected, history_row_label(row));
                        if response.clicked() {
                            state.selected_history_id = Some(row.entry.id);
                        }
                    }
                });
            }
        });

        columns[1].heading("リザルト詳細");
        let selected = state
            .selected_history_id
            .and_then(|id| rows.iter().find(|row| row.entry.id == id))
            .or_else(|| rows.first());
        if let Some(row) = selected {
            build_history_detail(&mut columns[1], row);
        } else {
            columns[1].label("履歴がありません。");
        }
    });
}

fn load_history_rows(
    score_db: &crate::storage::score_db::ScoreDatabase,
    library_db: &crate::storage::library_db::LibraryDatabase,
) -> Vec<HistoryDisplayRow> {
    let Ok(entries) = score_db.recent_history_by_local_day(200, 0) else {
        return Vec::new();
    };
    entries
        .into_iter()
        .map(|day_entry| {
            let entry = day_entry.entry;
            let chart = library_db
                .list_charts_by_sha256(entry.chart_sha256)
                .ok()
                .and_then(|charts| charts.into_iter().next());
            let table = crate::storage::common::hash_to_hex(&entry.chart_sha256);
            let difficulty = library_db
                .list_difficulty_table_entries_by_sha256s(&[table.as_str()])
                .ok()
                .and_then(|entries| entries.into_iter().next())
                .map(|entry| format!("{}{}", entry.table_symbol, entry.level))
                .or_else(|| chart.as_ref().map(|chart| chart.play_level.clone()))
                .unwrap_or_default();
            let title =
                chart.map(|chart| chart.title).unwrap_or_else(|| table.chars().take(12).collect());
            HistoryDisplayRow { local_day: day_entry.local_day, entry, title, difficulty }
        })
        .collect()
}

fn history_row_label(row: &HistoryDisplayRow) -> egui::WidgetText {
    let rate = score_rate(row.entry.ex_score, row.entry.total_notes);
    let text = format!(
        "{:<5}  ●  {}  {} ({:.2}%)  BP{}",
        row.difficulty, row.title, row.entry.ex_score, rate, row.entry.bp
    );
    egui::RichText::new(text).color(clear_color(&row.entry.clear_type)).into()
}

fn build_history_detail(ui: &mut egui::Ui, row: &HistoryDisplayRow) {
    ui.heading(&row.title);
    two_col(ui, "難易度", row.difficulty.clone());
    two_col(ui, "ランプ", row.entry.clear_type.clone());
    two_col(
        ui,
        "スコア",
        format!(
            "{} ({:.2}%)",
            row.entry.ex_score,
            score_rate(row.entry.ex_score, row.entry.total_notes)
        ),
    );
    two_col(ui, "ミスカウント", row.entry.bp.to_string());
    two_col(ui, "コンボ", row.entry.max_combo.to_string());
    two_col(ui, "CB", row.entry.cb.to_string());
    two_col(ui, "ノーツ", row.entry.total_notes.to_string());
    two_col(ui, "ゲージ", row.entry.gauge_value.map_or("-".to_string(), |v| format!("{v:.2}%")));
    two_col(ui, "入力", format!("{:?}", row.entry.device_type));
    two_col(ui, "保存元", row.entry.source_kind.as_str().to_string());
    if let Some(previous) = &row.entry.previous_best {
        ui.separator();
        ui.label(format!(
            "前回ベスト: {} / EX {} / BP {} / combo {}",
            previous.clear_type, previous.ex_score, previous.bp, previous.max_combo
        ));
    }
}

fn build_controller_view(
    ui: &mut egui::Ui,
    state: &mut PlayAnalysisPanelState,
    config: &mut PlayAnalysisConfig,
    input_config: &ProfileInputConfig,
    connected_gamepads: &[crate::input::gamepad::ConnectedGamepad],
    pressed_controls: &[String],
    pressed_play_inputs: &[(DeviceId, PhysicalControl)],
) -> bool {
    let mut changed = false;
    if config.release_ok_threshold_ms > config.release_ng_threshold_ms {
        config.release_ng_threshold_ms = config.release_ok_threshold_ms;
        changed = true;
    }
    ui.horizontal_wrapped(|ui| {
        ui.label("LN無視しきい値");
        changed |= ui
            .add(
                egui::DragValue::new(&mut config.release_ignore_threshold_ms)
                    .range(0..=5000)
                    .suffix(" ms"),
            )
            .changed();
        ui.label("計算期間");
        changed |= ui
            .add(
                egui::DragValue::new(&mut config.release_window_ms)
                    .range(100..=60000)
                    .suffix(" ms"),
            )
            .changed();
        ui.label("release-OK");
        let ok_changed = ui
            .add(
                egui::DragValue::new(&mut config.release_ok_threshold_ms)
                    .range(0..=5000)
                    .suffix(" ms"),
            )
            .changed();
        ui.label("release-NG");
        let ng_changed = ui
            .add(
                egui::DragValue::new(&mut config.release_ng_threshold_ms)
                    .range(0..=5000)
                    .suffix(" ms"),
            )
            .changed();
        if ok_changed && config.release_ok_threshold_ms > config.release_ng_threshold_ms {
            config.release_ok_threshold_ms = config.release_ng_threshold_ms;
        }
        if ng_changed && config.release_ng_threshold_ms < config.release_ok_threshold_ms {
            config.release_ng_threshold_ms = config.release_ok_threshold_ms;
        }
        changed |= ok_changed || ng_changed;
        egui::ComboBox::from_label("モード")
            .selected_text(controller_mode_label(config.controller_mode))
            .show_ui(ui, |ui| {
                changed |= ui
                    .selectable_value(
                        &mut config.controller_mode,
                        PlayAnalysisControllerModeConfig::Key7P1,
                        "7K (1P)",
                    )
                    .changed();
                changed |= ui
                    .selectable_value(
                        &mut config.controller_mode,
                        PlayAnalysisControllerModeConfig::Key7P2,
                        "7K (2P)",
                    )
                    .changed();
                changed |= ui
                    .selectable_value(
                        &mut config.controller_mode,
                        PlayAnalysisControllerModeConfig::Key14,
                        "14K",
                    )
                    .changed();
            });
    });
    ui.label(format!("平均リリース時間: {}", state.average_release_label()));

    if connected_gamepads.is_empty() {
        ui.small("接続中のゲームパッドはありません。キーボード入力も押下状態の観測対象です。");
    } else {
        for gamepad in connected_gamepads {
            ui.small(format!("{} ({})", gamepad.name, gamepad.stable_id));
        }
    }

    let active = resolve_active_inputs(input_config, config.controller_mode, pressed_play_inputs);
    ui.separator();
    build_controller_layout(ui, state, config, &active);

    if !pressed_controls.is_empty() {
        ui.separator();
        ui.label(format!("押下中: {}", pressed_controls.join(", ")));
    }
    changed
}

impl PlayAnalysisPanelState {
    fn observe_releases(
        &mut self,
        config: &PlayAnalysisConfig,
        input_config: &ProfileInputConfig,
        pressed_play_inputs: &[(DeviceId, PhysicalControl)],
    ) {
        let now = Instant::now();
        let day = local_day_key();
        if self.daily_press_day != day {
            self.daily_press_day = day;
            self.daily_lane_presses = [0; bmz_core::lane::LANE_COUNT];
            self.counted_pressed_inputs.clear();
        }
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
                self.daily_lane_presses[lane.index()] =
                    self.daily_lane_presses[lane.index()].saturating_add(1);
            }
            let Some(lane) =
                resolved.lane.filter(|lane| release_average_lane(config.controller_mode, *lane))
            else {
                continue;
            };
            if let Entry::Vacant(entry) = self.pressed_since.entry(key.clone()) {
                entry.insert(PressSampleStart { started_at: now, lane: Some(lane) });
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
                    if let Some(lane) = started.lane {
                        updated_release_lanes.push(lane);
                    }
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
    }

    fn average_release_label(&self) -> String {
        if self.release_samples.is_empty() {
            return "-".to_string();
        }
        let sum = self.release_samples.iter().map(|sample| sample.held_ms).sum::<f32>();
        format!(
            "{:.1} ms ({} samples)",
            sum / self.release_samples.len() as f32,
            self.release_samples.len()
        )
    }

    fn lane_release_display_ms(&self, lane: Lane) -> Option<f32> {
        self.last_release_by_lane[lane.index()]
    }

    fn lane_average_ms(&self, lane: Lane) -> Option<f32> {
        let samples = self
            .release_samples
            .iter()
            .filter(|sample| sample.lane == Some(lane))
            .map(|sample| sample.held_ms)
            .collect::<Vec<_>>();
        if samples.is_empty() {
            return None;
        }
        let sum = samples.iter().sum::<f32>();
        Some(sum / samples.len() as f32)
    }
}

#[derive(Debug, Clone, Copy, Default)]
struct ResolvedInput {
    lane: Option<Lane>,
}

#[derive(Debug, Default)]
struct ActiveControllerInputs {
    lanes: HashSet<Lane>,
    scratch_up: HashSet<Lane>,
    scratch_down: HashSet<Lane>,
    by_input: HashMap<PhysicalInputKey, ResolvedInput>,
}

fn resolve_active_inputs(
    input_config: &ProfileInputConfig,
    mode: PlayAnalysisControllerModeConfig,
    pressed_play_inputs: &[(DeviceId, PhysicalControl)],
) -> ActiveControllerInputs {
    let mut active = ActiveControllerInputs::default();
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

fn build_controller_layout(
    ui: &mut egui::Ui,
    state: &PlayAnalysisPanelState,
    config: &PlayAnalysisConfig,
    active: &ActiveControllerInputs,
) {
    match config.controller_mode {
        PlayAnalysisControllerModeConfig::Key7P1 => {
            build_seven_key_controller(
                ui,
                state,
                config,
                Lane::Scratch,
                key_lanes_1p(),
                ScratchPlacement::Left,
                active,
            );
        }
        PlayAnalysisControllerModeConfig::Key7P2 => {
            build_seven_key_controller(
                ui,
                state,
                config,
                Lane::Scratch,
                key_lanes_1p(),
                ScratchPlacement::Right,
                active,
            );
        }
        PlayAnalysisControllerModeConfig::Key14 => {
            ui.horizontal_top(|ui| {
                build_seven_key_controller(
                    ui,
                    state,
                    config,
                    Lane::Scratch,
                    key_lanes_1p(),
                    ScratchPlacement::Left,
                    active,
                );
                ui.separator();
                build_seven_key_controller(
                    ui,
                    state,
                    config,
                    Lane::Scratch2,
                    key_lanes_2p(),
                    ScratchPlacement::Right,
                    active,
                );
            });
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ScratchPlacement {
    Left,
    Right,
}

fn build_seven_key_controller(
    ui: &mut egui::Ui,
    state: &PlayAnalysisPanelState,
    config: &PlayAnalysisConfig,
    scratch: Lane,
    keys: [Lane; 7],
    scratch_placement: ScratchPlacement,
    active: &ActiveControllerInputs,
) {
    ui.horizontal(|ui| {
        if scratch_placement == ScratchPlacement::Left {
            build_scratch_button(ui, scratch, active);
        }
        build_key_button_grid(ui, state, config, keys, active);
        if scratch_placement == ScratchPlacement::Right {
            build_scratch_button(ui, scratch, active);
        }
    });
}

fn build_scratch_button(ui: &mut egui::Ui, scratch: Lane, active: &ActiveControllerInputs) {
    let (rect, response) = ui.allocate_exact_size(egui::vec2(82.0, 96.0), egui::Sense::hover());
    let painter = ui.painter_at(rect);
    let up = active.scratch_up.contains(&scratch);
    let down = active.scratch_down.contains(&scratch);
    let center = egui::pos2(rect.center().x, rect.top() + 48.0);
    let radius = 34.0;
    let inactive = egui::Color32::from_gray(38);
    let active_color = egui::Color32::from_rgb(230, 46, 58);
    let rim = egui::Color32::from_gray(175);

    painter.circle_filled(center, radius, egui::Color32::from_gray(20));
    paint_scratch_half(
        &painter,
        center,
        radius - 2.0,
        true,
        if up { active_color } else { inactive },
    );
    paint_scratch_half(
        &painter,
        center,
        radius - 2.0,
        false,
        if down { active_color } else { inactive },
    );
    painter.circle_stroke(center, radius, egui::Stroke::new(3.0_f32, rim));
    painter.line_segment(
        [egui::pos2(center.x - radius, center.y), egui::pos2(center.x + radius, center.y)],
        egui::Stroke::new(2.0_f32, egui::Color32::from_gray(80)),
    );
    painter.line_segment(
        [
            egui::pos2(center.x - 16.0, center.y + 18.0),
            egui::pos2(center.x + 18.0, center.y - 20.0),
        ],
        egui::Stroke::new(5.0_f32, egui::Color32::from_gray(230)),
    );
    painter.text(
        egui::pos2(center.x, rect.bottom() - 11.0),
        egui::Align2::CENTER_CENTER,
        "SCR",
        egui::FontId::proportional(12.0),
        egui::Color32::from_gray(170),
    );
    response.on_hover_text("Scratch");
}

fn paint_scratch_half(
    painter: &egui::Painter,
    center: egui::Pos2,
    radius: f32,
    upper: bool,
    color: egui::Color32,
) {
    let (start, end) = if upper {
        (std::f32::consts::PI, std::f32::consts::TAU)
    } else {
        (0.0, std::f32::consts::PI)
    };
    let mut points = Vec::with_capacity(18);
    for step in 0..=16 {
        let angle = start + (end - start) * step as f32 / 16.0;
        points.push(egui::pos2(center.x + angle.cos() * radius, center.y + angle.sin() * radius));
    }
    painter.add(egui::Shape::convex_polygon(points, color, egui::Stroke::NONE));
}

fn build_key_button_grid(
    ui: &mut egui::Ui,
    state: &PlayAnalysisPanelState,
    config: &PlayAnalysisConfig,
    keys: [Lane; 7],
    active: &ActiveControllerInputs,
) {
    ui.vertical(|ui| {
        ui.horizontal(|ui| {
            ui.add_space(28.0);
            for lane in [keys[1], keys[3], keys[5]] {
                build_controller_key_button(ui, state, config, lane, active);
            }
        });
        ui.horizontal(|ui| {
            for lane in [keys[0], keys[2], keys[4], keys[6]] {
                build_controller_key_button(ui, state, config, lane, active);
            }
        });
    });
}

fn release_color(config: &PlayAnalysisConfig, release_ms: Option<f32>) -> egui::Color32 {
    let Some(release_ms) = release_ms else {
        return egui::Color32::from_gray(110);
    };
    if release_ms <= config.release_ok_threshold_ms as f32 {
        egui::Color32::from_rgb(40, 190, 95)
    } else if release_ms <= config.release_ng_threshold_ms as f32 {
        egui::Color32::from_rgb(235, 200, 60)
    } else {
        egui::Color32::from_rgb(230, 70, 70)
    }
}

fn release_stroke(config: &PlayAnalysisConfig, release_ms: Option<f32>) -> egui::Stroke {
    let width = if release_ms.is_some() { 4.0_f32 } else { 1.0_f32 };
    egui::Stroke::new(width, release_color(config, release_ms))
}

fn release_key_label(release_ms: Option<f32>) -> String {
    match release_ms {
        Some(value) => {
            let value = value.round().clamp(0.0, 999.0) as u16;
            format!("{value:>3}")
        }
        None => "---".to_string(),
    }
}

fn build_controller_key_button(
    ui: &mut egui::Ui,
    state: &PlayAnalysisPanelState,
    config: &PlayAnalysisConfig,
    lane: Lane,
    active: &ActiveControllerInputs,
) {
    let number = display_key_number(KeyMode::K14, lane).unwrap_or(0);
    let release_ms = state.lane_release_display_ms(lane);
    let fill = if active.lanes.contains(&lane) {
        egui::Color32::from_rgb(130, 220, 255)
    } else if release_ms.is_none() {
        egui::Color32::from_gray(55)
    } else if matches!(number, 2 | 4 | 6) {
        egui::Color32::from_rgb(0, 60, 150)
    } else {
        egui::Color32::from_rgb(235, 238, 244)
    };
    let value_color = release_color(config, release_ms);
    let text_color = if release_ms.is_some() { value_color } else { egui::Color32::from_gray(150) };
    let stroke = release_stroke(config, release_ms);
    ui.add_sized(
        [52.0, 48.0],
        egui::Button::new(
            egui::RichText::new(release_key_label(release_ms))
                .monospace()
                .size(17.0)
                .color(text_color),
        )
        .fill(fill)
        .stroke(stroke),
    );
}

fn key_lanes_1p() -> [Lane; 7] {
    [Lane::Key1, Lane::Key2, Lane::Key3, Lane::Key4, Lane::Key5, Lane::Key6, Lane::Key7]
}

fn key_lanes_2p() -> [Lane; 7] {
    [Lane::Key8, Lane::Key9, Lane::Key10, Lane::Key11, Lane::Key12, Lane::Key13, Lane::Key14]
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
            controller_mode_includes_lane(mode, lane).then_some(lane)
        }
    }
}

fn controller_mode_includes_lane(mode: PlayAnalysisControllerModeConfig, lane: Lane) -> bool {
    match mode {
        PlayAnalysisControllerModeConfig::Key7P1 => {
            matches!(
                lane,
                Lane::Scratch
                    | Lane::Key1
                    | Lane::Key2
                    | Lane::Key3
                    | Lane::Key4
                    | Lane::Key5
                    | Lane::Key6
                    | Lane::Key7
            )
        }
        PlayAnalysisControllerModeConfig::Key7P2 => {
            matches!(
                lane,
                Lane::Scratch2
                    | Lane::Key8
                    | Lane::Key9
                    | Lane::Key10
                    | Lane::Key11
                    | Lane::Key12
                    | Lane::Key13
                    | Lane::Key14
            )
        }
        PlayAnalysisControllerModeConfig::Key14 => matches!(
            lane,
            Lane::Scratch
                | Lane::Key1
                | Lane::Key2
                | Lane::Key3
                | Lane::Key4
                | Lane::Key5
                | Lane::Key6
                | Lane::Key7
                | Lane::Scratch2
                | Lane::Key8
                | Lane::Key9
                | Lane::Key10
                | Lane::Key11
                | Lane::Key12
                | Lane::Key13
                | Lane::Key14
        ),
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
        PlayAnalysisControllerModeConfig::Key14 => matches!(
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
        ),
    }
}

fn controller_mode_label(mode: PlayAnalysisControllerModeConfig) -> &'static str {
    match mode {
        PlayAnalysisControllerModeConfig::Key7P1 => "7K (1P)",
        PlayAnalysisControllerModeConfig::Key7P2 => "7K (2P)",
        PlayAnalysisControllerModeConfig::Key14 => "14K",
    }
}

fn note_count_display_lanes(mode: PlayAnalysisControllerModeConfig) -> &'static [Lane] {
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

fn lane_count_label(mode: PlayAnalysisControllerModeConfig, lane: Lane) -> String {
    match (mode, lane) {
        (PlayAnalysisControllerModeConfig::Key14, Lane::Scratch) => "1P SCR".to_string(),
        (_, Lane::Scratch) => "SCR".to_string(),
        (PlayAnalysisControllerModeConfig::Key14, Lane::Scratch2) => "2P SCR".to_string(),
        (_, Lane::Scratch2) => "SCR".to_string(),
        (PlayAnalysisControllerModeConfig::Key14, lane)
            if matches!(
                lane,
                Lane::Key1
                    | Lane::Key2
                    | Lane::Key3
                    | Lane::Key4
                    | Lane::Key5
                    | Lane::Key6
                    | Lane::Key7
            ) =>
        {
            format!("1P {}", display_key_number(KeyMode::K14, lane).unwrap_or(0))
        }
        (PlayAnalysisControllerModeConfig::Key14, lane)
            if matches!(
                lane,
                Lane::Key8
                    | Lane::Key9
                    | Lane::Key10
                    | Lane::Key11
                    | Lane::Key12
                    | Lane::Key13
                    | Lane::Key14
            ) =>
        {
            format!("2P {}", display_key_number(KeyMode::K14, lane).unwrap_or(0))
        }
        (_, lane) => display_key_number(KeyMode::K14, lane)
            .map_or_else(|| "-".to_string(), |number| number.to_string()),
    }
}

fn two_col(ui: &mut egui::Ui, label: &str, value: String) {
    ui.horizontal(|ui| {
        ui.add_sized(
            [96.0, 20.0],
            egui::Label::new(egui::RichText::new(label).color(egui::Color32::from_gray(150))),
        );
        ui.label(egui::RichText::new(value).strong().color(egui::Color32::from_rgb(235, 238, 244)));
    });
}

fn lane_pattern_row(ui: &mut egui::Ui, key_mode: KeyMode, pattern: &[u8]) {
    let label = egui::RichText::new("レーン配置").color(egui::Color32::from_gray(150));
    if ui.available_width() < 430.0 {
        ui.label(label);
        build_lane_pattern_view(ui, key_mode, pattern);
    } else {
        ui.horizontal(|ui| {
            ui.add_sized([96.0, 20.0], egui::Label::new(label));
            build_lane_pattern_view(ui, key_mode, pattern);
        });
    }
}

fn build_lane_pattern_view(ui: &mut egui::Ui, key_mode: KeyMode, pattern: &[u8]) {
    let key_lanes = key_mode
        .active_lanes()
        .iter()
        .copied()
        .filter(|lane| !matches!(lane, Lane::Scratch | Lane::Scratch2))
        .collect::<Vec<_>>();
    let lane_count = key_lanes.len().max(1) as f32;
    let button_width =
        ((ui.available_width() - (lane_count - 1.0) * 4.0) / lane_count).clamp(24.0, 42.0);
    let button_height = if button_width < 34.0 { 52.0 } else { 64.0 };
    let text_size = if button_width < 34.0 { 16.0 } else { 20.0 };
    ui.horizontal(|ui| {
        for (index, lane) in key_lanes.iter().copied().enumerate() {
            ui.push_id(("play_analysis_lane_pattern", index), |ui| {
                let label = lane_pattern_display_label(key_mode, pattern, lane);
                let is_blue = lane_pattern_source_key_number(key_mode, pattern, lane)
                    .is_some_and(|number| matches!(number, 2 | 4 | 6));
                ui.add_sized(
                    [button_width, button_height],
                    lane_pattern_button(label, is_blue, text_size),
                )
                .on_hover_text(format!("display lane {}", index + 1));
            });
        }
    });
}

fn lane_pattern_button(label: String, is_blue: bool, text_size: f32) -> egui::Button<'static> {
    let fill = if is_blue {
        egui::Color32::from_rgb(0, 60, 150)
    } else {
        egui::Color32::from_rgb(235, 238, 244)
    };
    let text_color = if is_blue { egui::Color32::WHITE } else { egui::Color32::from_gray(35) };
    egui::Button::new(egui::RichText::new(label).size(text_size).color(text_color)).fill(fill)
}

fn lane_pattern_display_label(key_mode: KeyMode, pattern: &[u8], display_lane: Lane) -> String {
    lane_pattern_source_key_number(key_mode, pattern, display_lane)
        .map_or_else(|| "-".to_string(), |number| number.to_string())
}

fn lane_pattern_source_key_number(
    key_mode: KeyMode,
    pattern: &[u8],
    display_lane: Lane,
) -> Option<u8> {
    let source = pattern
        .get(display_lane.index())
        .copied()
        .and_then(|lane| Lane::ALL.get(usize::from(lane)).copied())
        .unwrap_or(display_lane);
    display_key_number(key_mode, source)
}

fn display_key_number(key_mode: KeyMode, lane: Lane) -> Option<u8> {
    match lane {
        Lane::Key1 => Some(1),
        Lane::Key2 => Some(2),
        Lane::Key3 => Some(3),
        Lane::Key4 => Some(4),
        Lane::Key5 => Some(5),
        Lane::Key6 => Some(6),
        Lane::Key7 => Some(7),
        Lane::Key8 if key_mode == KeyMode::K9 => Some(8),
        Lane::Key9 if key_mode == KeyMode::K9 => Some(9),
        Lane::Key8 => Some(1),
        Lane::Key9 => Some(2),
        Lane::Key10 => Some(3),
        Lane::Key11 => Some(4),
        Lane::Key12 => Some(5),
        Lane::Key13 => Some(6),
        Lane::Key14 => Some(7),
        Lane::Scratch | Lane::Scratch2 => None,
    }
}

fn play_arrange_options_label(
    key_mode: KeyMode,
    arrange: &str,
    arrange_2p: &str,
    double_option: Option<&str>,
) -> String {
    match key_mode {
        KeyMode::K10 | KeyMode::K14 => {
            let double = double_option.filter(|value| !value.is_empty() && *value != "OFF");
            match double {
                Some(double) => format!("1P {arrange} / 2P {arrange_2p} / {double}"),
                None => format!("1P {arrange} / 2P {arrange_2p}"),
            }
        }
        _ => arrange.to_string(),
    }
}

fn difficulty_label(table_level: &str, play_level: &str) -> String {
    if table_level.trim().is_empty() { play_level.to_string() } else { table_level.to_string() }
}

fn score_rate(ex_score: u32, total_notes: u32) -> f32 {
    if total_notes == 0 { 0.0 } else { ex_score as f32 * 100.0 / (total_notes as f32 * 2.0) }
}

fn clear_color(clear_type: &str) -> egui::Color32 {
    match clear_type {
        "Failed" => egui::Color32::from_rgb(120, 120, 120),
        "AssistEasy" | "LightAssistEasy" => egui::Color32::from_rgb(255, 120, 180),
        "Easy" => egui::Color32::from_rgb(90, 220, 120),
        "Normal" => egui::Color32::from_rgb(90, 170, 255),
        "Hard" => egui::Color32::from_rgb(255, 110, 90),
        "ExHard" => egui::Color32::from_rgb(255, 210, 80),
        "FullCombo" | "Perfect" | "Max" => egui::Color32::from_rgb(120, 240, 255),
        _ => egui::Color32::from_rgb(180, 180, 180),
    }
}
