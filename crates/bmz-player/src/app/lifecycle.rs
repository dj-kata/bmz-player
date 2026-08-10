use super::*;

impl ApplicationHandler<AppUserEvent> for WinitApp {
    fn new_events(&mut self, event_loop: &ActiveEventLoop, cause: StartCause) {
        match cause {
            StartCause::Init => {
                tracing::info!("winit app init");
                self.ensure_window(event_loop);
            }
            StartCause::ResumeTimeReached { start, requested_resume } => {
                let actual_wake_at = Instant::now();
                let effective_frame_limit = self.current_frame_limit();
                self.frame.record_wait_wake(
                    start,
                    requested_resume,
                    actual_wake_at,
                    effective_frame_limit,
                );
                // `WaitUntil` の deadline 到達時だけ描画を要求する。待機中に届いた
                // keyboard/device/user event は redraw を発生させず、その場で処理できる。
                self.request_redraw();
            }
            StartCause::WaitCancelled { .. } | StartCause::Poll => {}
        }
    }

    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        tracing::info!("winit app resumed");
        self.ensure_window(event_loop);
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        window_id: WindowId,
        event: WindowEvent,
    ) {
        if self.window.as_ref().map(|window| window.id()) != Some(window_id) {
            return;
        }

        // すべてのウィンドウイベントを egui へ供給する。RedrawRequested など
        // egui が関知しないイベントは egui_winit 側で無視される。
        let practice_overlay = self
            .play
            .practice_session
            .as_ref()
            .is_some_and(|practice| practice.phase == PracticePhase::Config);
        let egui_consumed = match (self.window.clone(), self.ui.egui.as_mut()) {
            (Some(window), Some(egui)) => egui.on_window_event(&window, &event, practice_overlay),
            _ => false,
        };

        match event {
            WindowEvent::CloseRequested => {
                self.save_configs_for_exit(self.active_hispeed(), "game exit");
                event_loop.exit();
            }
            WindowEvent::KeyboardInput { event, .. } => {
                // F1 で egui メニューを開閉する。
                if event.physical_key == PhysicalKey::Code(KeyCode::F1)
                    && event.state == ElementState::Pressed
                    && !event.repeat
                {
                    if let Some(egui) = self.ui.egui.as_mut() {
                        egui.toggle();
                    }
                    return;
                }
                if event.physical_key == PhysicalKey::Code(KeyCode::F12)
                    && event.state == ElementState::Pressed
                    && !event.repeat
                {
                    self.request_manual_screenshot();
                    return;
                }
                // egui がフォーカスを持つ間はゲーム入力へ伝播させない。
                if egui_consumed {
                    return;
                }
                self.route_keyboard_input(&event);
            }
            WindowEvent::MouseWheel { delta, .. } => {
                self.ui.last_cursor_action_at = Instant::now();
                if !self.ui.cursor_visible {
                    if let Some(window) = &self.window {
                        window.set_cursor_visible(true);
                    }
                    self.ui.cursor_visible = true;
                }
                if egui_consumed {
                    return;
                }
                self.route_mouse_wheel(delta);
            }
            WindowEvent::CursorMoved { position, .. } => {
                self.select.last_cursor_position = Some(position);
                self.ui.last_cursor_action_at = Instant::now();
                if !self.ui.cursor_visible {
                    if let Some(window) = &self.window {
                        window.set_cursor_visible(true);
                    }
                    self.ui.cursor_visible = true;
                }
                if !egui_consumed {
                    self.route_select_slider_drag();
                }
            }
            WindowEvent::MouseInput { state, button, .. } => {
                self.ui.last_cursor_action_at = Instant::now();
                if !self.ui.cursor_visible {
                    if let Some(window) = &self.window {
                        window.set_cursor_visible(true);
                    }
                    self.ui.cursor_visible = true;
                }
                if egui_consumed {
                    return;
                }
                self.route_mouse_input(state, button);
            }
            WindowEvent::Ime(ime) => {
                if egui_consumed {
                    return;
                }
                self.route_ime_event(&ime);
            }
            WindowEvent::Resized(size) => {
                self.renderer
                    .resize_surface(SurfaceSize { width: size.width, height: size.height });
                // 検索モード中はリサイズに合わせて IME 候補ウィンドウ位置を再計算する。
                self.update_search_ime_cursor_area();
            }
            WindowEvent::Focused(focused) => {
                let event_focused = focused;
                let native_focused = self.window.as_ref().is_some_and(|window| window.has_focus());
                let previous_effective_focused = self.ui.focused;
                let focus_update = resolve_window_focus_update(
                    previous_effective_focused,
                    event_focused,
                    native_focused,
                    cfg!(target_os = "macos"),
                );
                if event_focused != native_focused {
                    tracing::warn!(
                        event_focused,
                        native_focused,
                        previous_effective_focused,
                        effective_focused = focus_update.effective_focused,
                        "window focus state mismatch"
                    );
                }
                if previous_effective_focused != focus_update.effective_focused {
                    let previous_effective_frame_limit = self.current_frame_limit();
                    self.ui.focused = focus_update.effective_focused;
                    let effective_frame_limit = self.current_frame_limit();
                    tracing::info!(
                        previous_effective_focused,
                        effective_focused = focus_update.effective_focused,
                        previous_effective_frame_limit,
                        effective_frame_limit,
                        "effective window focus and frame limit changed"
                    );
                }
                if focus_update.focus_lost {
                    let releases = self.input.handle_focus_lost();
                    for event in releases.raw_keyboard {
                        self.route_play_device_input(event);
                    }
                    for event in releases.window_keyboard {
                        self.route_play_device_input(event);
                    }
                    self.sync_select_holds_from_pressed_controls();
                    self.clear_select_hold();
                    self.reset_select_analog_scroll();
                    self.reset_play_analog_scroll();
                    self.clear_result_ir_scroll_input();
                    self.clear_play_control_holds();
                }
            }
            WindowEvent::RedrawRequested => {
                let limit_start = Instant::now();
                if !self.begin_scheduled_frame(event_loop) {
                    return;
                }
                let pacing_timings = self.frame.current_pacing_timings();
                let limit_us = instant_elapsed_us_u64(limit_start);
                let redraw_started_at = Instant::now();
                let scene_before = self.current_scene_kind();
                let pending_skin_before = self.has_pending_skin_reload();
                let render_probe_before = self.skin.pending_skin_render_probe.is_some();
                let cursor_start = Instant::now();
                if self.ui.cursor_visible
                    && self.ui.last_cursor_action_at.elapsed() >= Duration::from_secs(2)
                {
                    if let Some(window) = &self.window {
                        window.set_cursor_visible(false);
                    }
                    self.ui.cursor_visible = false;
                }
                let cursor_us = instant_elapsed_us_u64(cursor_start);
                // Worker completion should be applied before intentional frame pacing sleep;
                // otherwise reload latency includes the frame limiter wait.
                let drain_start = Instant::now();
                let skin_drain_stats = self.drain_pending_skins();
                let drain_us = instant_elapsed_us_u64(drain_start);
                let input_start = Instant::now();
                self.poll_gamepad_events();
                self.advance_select_hold_move();
                self.advance_result_ir_scroll_hold();
                self.advance_select_analog_scroll();
                self.advance_result_ir_analog_scroll();
                let input_us = instant_elapsed_us_u64(input_start);
                let background_start = Instant::now();
                self.poll_chart_bga_texture_load();
                self.poll_play_preload();
                self.refresh_play_target_from_source();
                self.poll_select_maintenance();
                let background_us = instant_elapsed_us_u64(background_start);
                let transition_start = Instant::now();
                self.advance_decide_transition();
                self.advance_play_ending();
                self.advance_result_exit();
                let transition_us = instant_elapsed_us_u64(transition_start);
                let egui_start = Instant::now();
                self.run_egui_frame();
                let egui_us = instant_elapsed_us_u64(egui_start);
                if !self.first_frame_startup_completed {
                    self.ensure_audio_output();
                }
                let advance_active_play_start = Instant::now();
                self.advance_active_play();
                let advance_active_play_us = instant_elapsed_us_u64(advance_active_play_start);
                self.log_input_diagnostics();
                let scene_start = Instant::now();
                let scene_profile = self.render_current_scene();
                let scene_us = instant_elapsed_us_u64(scene_start);
                let post_scene_start = Instant::now();
                if !self.first_frame_startup_completed {
                    self.first_frame_startup_completed = true;
                    self.start_deferred_boot();
                    self.sync_select_maintenance_gate();
                    if self.current_scene_kind() == AppSceneKind::Result {
                        self.ensure_result_skin_ready(self.current_result_skin_slot());
                    }
                    // render_current_scene() が既に last_scene_kind を更新済み。
                    // None に戻すと次フレームの start_scene_timers_before_snapshot が
                    // result_scene_started_at を再初期化し、動画 decode 時計が巻き戻って
                    // clocked decode thread が古い loop_base で待ち続けることがある。
                }
                self.advance_draining_audio();
                if let Some(runtime) = &self.audio.audio_runtime {
                    // chart sample bank を保持する source の破棄は、CPAL callback
                    // ではなく app thread 側で回収する。
                    runtime.reap_retired_sources();
                }
                self.log_audio_diagnostics();
                let post_scene_us = instant_elapsed_us_u64(post_scene_start);
                let total_us = instant_elapsed_us_u64(redraw_started_at);
                if let Some(sample) = scene_profile {
                    self.frame.record_profile(
                        sample,
                        AppLoopFrameTimings {
                            total_redraw_us: total_us,
                            input_us,
                            background_us,
                            transition_us,
                            egui_us,
                            advance_active_play_us,
                            post_scene_us,
                            pacing: pacing_timings,
                        },
                    );
                }
                let pending_skin_after = self.has_pending_skin_reload();
                if skin_drain_stats.received_count > 0
                    || render_probe_before
                    || (pending_skin_before
                        && total_us >= duration_us_u64(SKIN_RELOAD_REDRAW_PROFILE_THRESHOLD))
                {
                    tracing::debug!(
                        scene_before = ?scene_before,
                        scene_after = ?self.current_scene_kind(),
                        pending_before = pending_skin_before,
                        pending_after = pending_skin_after,
                        render_probe_before,
                        received_uploads = skin_drain_stats.received_count,
                        applied_uploads = skin_drain_stats.applied_count,
                        max_upload_wait_us = skin_drain_stats.max_upload_wait_us,
                        total_us,
                        cursor_us,
                        drain_us,
                        limit_us,
                        input_us,
                        background_us,
                        transition_us,
                        egui_us,
                        scene_us,
                        post_scene_us,
                        "skin reload redraw timings"
                    );
                }
                if self.should_exit_via_select_hold() {
                    tracing::info!("escape held for 2s on select screen; exiting app");
                    self.save_configs_for_exit(self.active_hispeed(), "select exit hold");
                    event_loop.exit();
                    return;
                }
                self.handle_smoke_exit_after_redraw(event_loop);
            }
            _ => {}
        }
    }

    fn device_event(
        &mut self,
        _event_loop: &ActiveEventLoop,
        _device_id: winit::event::DeviceId,
        event: DeviceEvent,
    ) {
        if let DeviceEvent::Key(raw) = event {
            self.route_raw_keyboard_gameplay_input(raw.physical_key, raw.state);
        }
    }

    fn user_event(&mut self, _event_loop: &ActiveEventLoop, event: AppUserEvent) {
        match event {
            AppUserEvent::SkinUploadReady { sent_at } => {
                let event_received_at = Instant::now();
                let pending_before = self.has_pending_skin_reload();
                let drain_start = Instant::now();
                let skin_drain_stats = self.drain_pending_skins();
                let drain_us = instant_elapsed_us_u64(drain_start);
                self.request_redraw();
                tracing::debug!(
                    event_delay_us = instant_duration_us_u64(sent_at, event_received_at),
                    pending_before,
                    pending_after = self.has_pending_skin_reload(),
                    received_uploads = skin_drain_stats.received_count,
                    applied_uploads = skin_drain_stats.applied_count,
                    max_upload_wait_us = skin_drain_stats.max_upload_wait_us,
                    drain_us,
                    "skin upload ready event timings"
                );
            }
            AppUserEvent::TableFetchReady => {
                self.poll_select_maintenance();
                self.request_redraw();
            }
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        if std::mem::take(&mut self.ui.device_events_reconfigure_pending) {
            self.configure_device_events(event_loop);
        }
        let pending_before = self.has_pending_skin_reload();
        if pending_before {
            let drain_start = Instant::now();
            let skin_drain_stats = self.drain_pending_skins();
            let drain_us = instant_elapsed_us_u64(drain_start);
            if skin_drain_stats.received_count > 0 {
                self.request_redraw();
            }
            if skin_drain_stats.received_count > 0
                || drain_us >= duration_us_u64(SKIN_RELOAD_REDRAW_PROFILE_THRESHOLD)
            {
                tracing::debug!(
                    pending_before,
                    pending_after = self.has_pending_skin_reload(),
                    received_uploads = skin_drain_stats.received_count,
                    applied_uploads = skin_drain_stats.applied_count,
                    max_upload_wait_us = skin_drain_stats.max_upload_wait_us,
                    drain_us,
                    "skin reload about_to_wait timings"
                );
            }
        }
        if self.shutdown_requested.load(Ordering::SeqCst) {
            tracing::info!("Ctrl-C received; exiting cleanly");
            event_loop.exit();
            return;
        }
        self.schedule_next_frame(event_loop);
    }

    fn exiting(&mut self, _event_loop: &ActiveEventLoop) {
        if let Some(handle) = self.integrations.discord_presence.take() {
            handle.shutdown();
        }
        self.flush_pending_screenshots("app exit");
        self.save_configs_for_exit(self.active_hispeed(), "game exit");
        if self.release_audio_for_process_exit() {
            std::process::exit(0);
        }
        // Linux の winit/wgpu backend では Window より後に Surface を drop すると
        // native 側で落ちることがあるため、Window を保持したまま GPU 資源を解放する。
        self.ui.egui = None;
        if let Ok(mut cache) = self.skin.skin_pipeline.gpu_texture_cache.lock() {
            cache.clear();
        }
        self.renderer.detach_surface();
    }
}

impl WinitApp {
    fn release_audio_for_process_exit(&mut self) -> bool {
        if self.audio.audio_runtime.as_ref().is_some_and(AudioRuntime::uses_pulseaudio_host) {
            // cpal 0.18 の PulseAudio backend は stream Drop 時に pulseaudio crate の
            // reactor 切断と stream delete が重なり、終了時に native 側で abort する
            // 環境がある。プロセス終了直前だけ handle を残し、通常の drop cascade
            // に戻らずプロセスを終了する。
            if let Some(audio) = self.audio.draining_audio.take() {
                std::mem::forget(audio);
            }
            if let Some(active_play) = self.play.active_play.take() {
                std::mem::forget(active_play);
            }
            if let Some(system_audio) = self.audio.system_audio.take() {
                std::mem::forget(system_audio);
            }
            if let Some(runtime) = self.audio.audio_runtime.take() {
                std::mem::forget(runtime);
            }
            tracing::debug!("exiting process directly after PulseAudio output workaround");
            return true;
        }

        // プロセス終了前に音声出力を確実に Drop し、ASIO の停止・後処理を走らせる。
        self.audio.draining_audio = None;
        self.play.active_play = None;
        self.audio.system_audio = None;
        self.audio.audio_runtime = None;
        false
    }
}
