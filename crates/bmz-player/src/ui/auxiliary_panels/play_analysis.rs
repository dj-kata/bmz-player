use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use bmz_core::clear::ClearType;
use bmz_core::input::ScratchDirection;
use bmz_core::lane::{KeyMode, Lane};
use bmz_gameplay::input::backend::{DeviceId, PhysicalControl};
use bmz_render::scene::{AppSceneSnapshot, ResultSnapshot};
use bmz_render::snapshot::RenderSnapshot;

use crate::config::play::lane_from_config;
use crate::config::play_input::resolve_play_bindings;
use crate::config::profile_config::{
    PlayAnalysisConfig, PlayAnalysisControllerModeConfig, ProfileInputConfig,
};
use crate::storage::difficulty_table_db::DifficultyTableRecord;

use super::*;

#[derive(Debug)]
pub(super) struct PlayAnalysisPanelState {
    started_at: Instant,
    started_at_unix: i64,
    selected_history_id: Option<i64>,
    pressed_since: HashMap<PhysicalInputKey, PressSampleStart>,
    release_samples: VecDeque<ReleaseSample>,
    tweet_status: String,
}

impl Default for PlayAnalysisPanelState {
    fn default() -> Self {
        Self {
            started_at: Instant::now(),
            started_at_unix: unix_now(),
            selected_history_id: None,
            pressed_since: HashMap::new(),
            release_samples: VecDeque::new(),
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

    localized_sized_panel_window(
        "bmz_play_analysis",
        "プレー分析 (F7)".to_string(),
        ctx,
        visible,
        520.0,
        760.0,
        egui::pos2(96.0, 48.0),
    )
    .show(ctx, |ui| {
        scrollable_window_content(ui, |ui| {
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
            egui::CollapsingHeader::new("ノーツ数")
                .default_open(true)
                .show(ui, |ui| build_notes_view(ui, panel.scene, panel.score_db));
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

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs().min(i64::MAX as u64) as i64)
        .unwrap_or_default()
}

fn build_notes_view(
    ui: &mut egui::Ui,
    scene: &AppSceneSnapshot,
    score_db: &crate::storage::score_db::ScoreDatabase,
) {
    let today = score_db.note_count_today().ok();
    let months = score_db.monthly_note_counts(12).unwrap_or_default();
    let years = score_db.yearly_note_counts(8).unwrap_or_default();

    ui.columns(3, |columns| {
        columns[0].heading("今日");
        match today {
            Some(row) => note_aggregate_line(&mut columns[0], &row),
            None => {
                columns[0].label("読み込み失敗");
            }
        }

        columns[1].heading("月次");
        for row in &months {
            note_aggregate_line(&mut columns[1], row);
        }

        columns[2].heading("年次");
        for row in &years {
            note_aggregate_line(&mut columns[2], row);
        }
    });

    ui.separator();
    ui.heading("レーンごとのノーツ数");
    match current_lane_counts(scene) {
        Some(counts) => {
            ui.horizontal_wrapped(|ui| {
                for (index, count) in counts.iter().enumerate() {
                    ui.label(format!("L{}: {}", index + 1, count));
                }
            });
        }
        None => {
            ui.label("選択中またはプレイ中の譜面から取得できるときに表示します。");
        }
    }
    ui.small("履歴全体のレーン別累計は、プレイ結果にレーン内訳を保存する追加スキーマが必要です。");
}

fn note_aggregate_line(ui: &mut egui::Ui, row: &crate::storage::score_db::NoteCountAggregate) {
    ui.label(format!("{}  {:>8} notes  {} plays", row.label, row.total_notes, row.play_count));
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

    ui.label("集計対象の難易度表");
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

    ui.separator();
    ui.label("ツイート文字列");
    let mut preview = tweet;
    ui.add(
        egui::TextEdit::multiline(&mut preview)
            .desired_rows(8)
            .desired_width(f32::INFINITY)
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
    ui.horizontal(|ui| {
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
            let Some(lane) =
                resolved.lane.filter(|lane| release_average_lane(config.controller_mode, *lane))
            else {
                continue;
            };
            self.pressed_since
                .entry(key.clone())
                .or_insert(PressSampleStart { started_at: now, lane: Some(lane) });
        }
        let released = self
            .pressed_since
            .keys()
            .filter(|key| !pressed.contains(*key))
            .cloned()
            .collect::<Vec<_>>();
        for key in released {
            if let Some(started) = self.pressed_since.remove(&key) {
                let held_ms = now.duration_since(started.started_at).as_secs_f32() * 1000.0;
                if held_ms <= config.release_ignore_threshold_ms.max(1) as f32 {
                    self.release_samples.push_back(ReleaseSample {
                        released_at: now,
                        lane: started.lane,
                        held_ms,
                    });
                }
            }
        }
        let window = config.release_window_ms.max(100) as f32;
        while self.release_samples.front().is_some_and(|sample| {
            now.duration_since(sample.released_at).as_secs_f32() * 1000.0 > window
        }) {
            self.release_samples.pop_front();
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

    fn lane_average_label(&self, lane: Lane) -> String {
        match self.lane_average_ms(lane) {
            Some(value) => format!("{value:.0}ms"),
            None => "-".to_string(),
        }
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
    let key_mode = controller_mode_key_mode(mode);
    let Ok(bindings) = resolve_play_bindings(input_config, key_mode) else {
        return ActiveControllerInputs::default();
    };
    let entries = bindings
        .iter()
        .filter_map(|entry| {
            let lane = entry.lane.map(lane_from_config)?;
            if !controller_mode_includes_lane(mode, lane) {
                return None;
            }
            Some((entry.control.clone(), lane, entry.scratch.map(scratch_direction_from_config)))
        })
        .collect::<Vec<_>>();
    let mut active = ActiveControllerInputs::default();
    for (device, control) in pressed_play_inputs {
        if let Some((_, lane, scratch_direction)) = entries
            .iter()
            .find(|(candidate, _, _)| physical_control_matches_name(control, candidate))
        {
            active.lanes.insert(*lane);
            if *scratch_direction == Some(ScratchDirection::Up) {
                active.scratch_up.insert(*lane);
            }
            if *scratch_direction == Some(ScratchDirection::Down) {
                active.scratch_down.insert(*lane);
            }
            active.by_input.insert(
                PhysicalInputKey { device: *device, control: control.clone() },
                ResolvedInput { lane: Some(*lane) },
            );
        }
    }
    active
}

fn physical_control_matches_name(control: &PhysicalControl, name: &str) -> bool {
    match control {
        PhysicalControl::KeyboardKey(control) | PhysicalControl::GamepadButton(control) => {
            control == name
        }
        PhysicalControl::HidButton(button) => name == format!("Button{button}"),
    }
}

fn scratch_direction_from_config(
    value: crate::config::profile_config::ScratchDirectionConfig,
) -> ScratchDirection {
    match value {
        crate::config::profile_config::ScratchDirectionConfig::Up => ScratchDirection::Up,
        crate::config::profile_config::ScratchDirectionConfig::Down => ScratchDirection::Down,
    }
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
                Lane::Scratch2,
                key_lanes_2p(),
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
    ui.vertical(|ui| {
        let up = active.scratch_up.contains(&scratch);
        let down = active.scratch_down.contains(&scratch);
        let fill = if up || down {
            egui::Color32::from_rgb(120, 30, 36)
        } else {
            egui::Color32::from_gray(32)
        };
        let label =
            format!("SCR\n{} {}", if up { "UP" } else { "up" }, if down { "DN" } else { "dn" });
        ui.add_sized([82.0, 96.0], egui::Button::new(label).fill(fill));
    });
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

fn release_stroke(config: &PlayAnalysisConfig, average_ms: Option<f32>) -> egui::Stroke {
    let Some(average_ms) = average_ms else {
        return egui::Stroke::new(1.0_f32, egui::Color32::from_gray(60));
    };
    let color = if average_ms <= config.release_ok_threshold_ms as f32 {
        egui::Color32::from_rgb(40, 190, 95)
    } else if average_ms <= config.release_ng_threshold_ms as f32 {
        egui::Color32::from_rgb(235, 200, 60)
    } else {
        egui::Color32::from_rgb(230, 70, 70)
    };
    egui::Stroke::new(4.0_f32, color)
}

fn build_controller_key_button(
    ui: &mut egui::Ui,
    state: &PlayAnalysisPanelState,
    config: &PlayAnalysisConfig,
    lane: Lane,
    active: &ActiveControllerInputs,
) {
    let number = display_key_number(KeyMode::K14, lane).unwrap_or(0);
    let fill = if active.lanes.contains(&lane) {
        egui::Color32::from_rgb(130, 220, 255)
    } else if matches!(number, 2 | 4 | 6) {
        egui::Color32::from_rgb(0, 60, 150)
    } else {
        egui::Color32::from_rgb(235, 238, 244)
    };
    let text_color = if matches!(number, 2 | 4 | 6) && !active.lanes.contains(&lane) {
        egui::Color32::WHITE
    } else {
        egui::Color32::from_gray(25)
    };
    let label = format!("{}\n{}", number, state.lane_average_label(lane));
    let stroke = release_stroke(config, state.lane_average_ms(lane));
    ui.add_sized(
        [52.0, 48.0],
        egui::Button::new(egui::RichText::new(label).color(text_color)).fill(fill).stroke(stroke),
    );
}

fn key_lanes_1p() -> [Lane; 7] {
    [Lane::Key1, Lane::Key2, Lane::Key3, Lane::Key4, Lane::Key5, Lane::Key6, Lane::Key7]
}

fn key_lanes_2p() -> [Lane; 7] {
    [Lane::Key8, Lane::Key9, Lane::Key10, Lane::Key11, Lane::Key12, Lane::Key13, Lane::Key14]
}

fn controller_mode_key_mode(mode: PlayAnalysisControllerModeConfig) -> KeyMode {
    match mode {
        PlayAnalysisControllerModeConfig::Key7P1 | PlayAnalysisControllerModeConfig::Key7P2 => {
            KeyMode::K7
        }
        PlayAnalysisControllerModeConfig::Key14 => KeyMode::K14,
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
        PlayAnalysisControllerModeConfig::Key7P1 => {
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
        PlayAnalysisControllerModeConfig::Key7P2 => {
            matches!(
                lane,
                Lane::Key8
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

fn current_lane_counts(scene: &AppSceneSnapshot) -> Option<[usize; bmz_core::lane::LANE_COUNT]> {
    let snapshot = match scene {
        AppSceneSnapshot::Decide(snapshot) | AppSceneSnapshot::Play(snapshot) => snapshot,
        _ => return None,
    };
    let mut counts = [0; bmz_core::lane::LANE_COUNT];
    for (lane, count) in counts.iter_mut().enumerate() {
        *count = snapshot.visible_notes[lane].len() + snapshot.visible_mines[lane].len();
    }
    Some(counts)
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
    ui.horizontal(|ui| {
        ui.add_sized(
            [96.0, 20.0],
            egui::Label::new(
                egui::RichText::new("レーン配置").color(egui::Color32::from_gray(150)),
            ),
        );
        build_lane_pattern_view(ui, key_mode, pattern);
    });
}

fn build_lane_pattern_view(ui: &mut egui::Ui, key_mode: KeyMode, pattern: &[u8]) {
    let key_lanes = key_mode
        .active_lanes()
        .iter()
        .copied()
        .filter(|lane| !matches!(lane, Lane::Scratch | Lane::Scratch2))
        .collect::<Vec<_>>();
    ui.horizontal(|ui| {
        for (index, lane) in key_lanes.iter().copied().enumerate() {
            ui.push_id(("play_analysis_lane_pattern", index), |ui| {
                let label = lane_pattern_display_label(key_mode, pattern, lane);
                let is_blue = display_key_number(key_mode, lane)
                    .is_some_and(|number| matches!(number, 2 | 4 | 6));
                ui.add_sized([42.0, 64.0], lane_pattern_button(label, is_blue))
                    .on_hover_text(format!("display lane {}", index + 1));
            });
        }
    });
}

fn lane_pattern_button(label: String, is_blue: bool) -> egui::Button<'static> {
    let fill = if is_blue {
        egui::Color32::from_rgb(0, 60, 150)
    } else {
        egui::Color32::from_rgb(235, 238, 244)
    };
    let text_color = if is_blue { egui::Color32::WHITE } else { egui::Color32::from_gray(35) };
    egui::Button::new(egui::RichText::new(label).size(20.0).color(text_color)).fill(fill)
}

fn lane_pattern_display_label(key_mode: KeyMode, pattern: &[u8], display_lane: Lane) -> String {
    let source = pattern
        .get(display_lane.index())
        .copied()
        .and_then(|lane| Lane::ALL.get(usize::from(lane)).copied())
        .unwrap_or(display_lane);
    display_key_number(key_mode, source)
        .map_or_else(|| "-".to_string(), |number| number.to_string())
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
