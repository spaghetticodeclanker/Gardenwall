//! Gardenwall desktop application (ADR-011): the trusted core in the Tauri
//! main process, with a React UI in the webview.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
#![forbid(unsafe_code)]

mod activity;
mod agent;
mod app_update;
mod attach;
mod backup;
mod binary;
mod chat_web;
mod commands;
mod diagnostics;
mod dictation;
mod documents;
#[cfg(windows)]
mod download_helper;
mod export;
mod gpu_driver;
mod handoff;
mod library;
mod lifecycle;
mod lockdown;
mod mcp;
mod memory;
mod migrate;
mod new_project;
mod parent_folders;
mod picker;
mod projects;
mod readiness;
mod research;
mod research_agent;
mod sandbox_restart;
mod skills;
mod speech;
mod speed;
mod state;
mod toolchains;
mod tuning;
mod updates;
mod vault;
mod vision;
mod web;
mod worker;

use tauri::Manager;

/// The window is drawn by Microsoft Edge WebView2. Without it Tauri panics
/// with "Could not find the webview runtime", so say plainly what's missing
/// and how to get it instead. (The installers add it during setup; this
/// catches a runtime removed later, for example by a debloat tool.)
#[cfg(windows)]
fn require_webview2() {
    if tauri::webview_version().is_ok() {
        return;
    }
    let message = "Gardenwall needs the Microsoft Edge WebView2 Runtime to draw its window, and it isn't installed on this computer (it may have been removed along with Edge).\n\nInstall the \"Evergreen\" runtime from Microsoft, then start Gardenwall again:\nhttps://developer.microsoft.com/microsoft-edge/webview2/\n\nOr in PowerShell:\nwinget install Microsoft.EdgeWebView2Runtime";
    eprintln!("{message}");
    rfd::MessageDialog::new()
        .set_level(rfd::MessageLevel::Error)
        .set_title("Gardenwall can't start")
        .set_description(message)
        .set_buttons(rfd::MessageButtons::Ok)
        .show();
    std::process::exit(1);
}

/// The data folder's name: Tauri's bundle identifier (checked by a test
/// against `tauri.conf.json`).
#[cfg_attr(not(windows), allow(dead_code))]
const DATA_FOLDER: &str = "ai.gardenwall.desktop";

/// Run by the uninstaller (release plan feature 3, F3.5) after Gardenwall
/// has closed: revoke sandbox file permissions left in the grant journals,
/// so none stay on the user's folders whatever happens to the data. Only
/// `Gardenwall.*` container permissions are ever removed.
#[cfg(windows)]
fn revoke_for_uninstall() -> i32 {
    let mut failed = false;
    for base in ["LOCALAPPDATA", "APPDATA"] {
        let Some(base) = std::env::var_os(base) else {
            continue;
        };
        let journals = std::path::Path::new(&base)
            .join(DATA_FOLDER)
            .join("sandbox-grants");
        if journals.exists() {
            match gardenwall_sandbox::reconcile_grant_journals(&journals) {
                Ok(report) => failed |= report.kept > 0,
                Err(_) => failed = true,
            }
        }
        // Toolchain grants kept between runs (ADR-022 §8a).
        let kept = std::path::Path::new(&base)
            .join(DATA_FOLDER)
            .join("sandbox-kept");
        if kept.exists() {
            failed |= !gardenwall_sandbox::revoke_all_kept(&kept);
        }
    }
    i32::from(failed)
}

fn main() {
    #[cfg(windows)]
    if std::env::args().nth(1).as_deref() == Some("--revoke-sandbox-grants") {
        std::process::exit(revoke_for_uninstall());
    }
    #[cfg(windows)]
    require_webview2();
    let app = tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        // System notifications are sent only by the core (no webview
        // permission is granted), naming a session and an event.
        .plugin(tauri_plugin_notification::init())
        // Gardenwall's own updates (ADR-017 §3.3): used by the core only to
        // verify and install; no webview permission is granted.
        .plugin(tauri_plugin_updater::Builder::new().build())
        .setup(|app| {
            // Data lives in the local (never roaming) profile; earlier
            // versions used the roaming one.
            let old = app.path().app_data_dir()?;
            let new = app.path().app_local_data_dir()?;
            #[cfg(windows)]
            let _ = gardenwall_sandbox::reconcile_grant_journals(&old.join("sandbox-grants"));
            let outcome = migrate::move_to_local(&old, &new);
            let state = state::AppState::new(outcome.root(&old, &new).to_path_buf())?;
            match &outcome {
                migrate::Outcome::Moved(_) => state.add_notice(format!(
                    "Gardenwall's data moved to {} so it is never copied by a roaming profile.",
                    new.display()
                )),
                migrate::Outcome::Conflict(why) | migrate::Outcome::Failed(why) => state
                    .add_notice(format!(
                        "Gardenwall's data is still in the roaming profile ({why})."
                    )),
                migrate::Outcome::NothingToMove => {}
            }
            let _ = state.app.set(app.handle().clone());
            // Did the previous session close normally? (F2.3)
            diagnostics::start_session(&state);
            app.manage(state);
            // Leftover sandbox permissions are cleaned up off the main
            // thread, then the toolchain folders sessions use are kept
            // granted so sessions start fast (ADR-022 §8, §8a).
            lifecycle::startup_cleanup(app.handle());
            // Update checks follow the user's schedule (ADR-017 §3.4).
            updates::spawn_scheduler(app.handle().clone());
            // A click on a system notification opens its session.
            activity::on_notification(app.handle());
            Ok(())
        })
        // Files dropped on the window: the paths come from the OS, and the
        // core reads them as images for the open composer.
        .on_window_event(|window, event| {
            // The X button: asks first only while something is running.
            if let tauri::WindowEvent::CloseRequested { api, .. } = event
                && window.label() == "main"
            {
                lifecycle::on_close_requested(window, api);
                return;
            }
            if let tauri::WindowEvent::DragDrop(tauri::DragDropEvent::Drop { paths, .. }) = event
                && window.label() == "main"
            {
                let app = window.app_handle().clone();
                let paths = paths.clone();
                tauri::async_runtime::spawn_blocking(move || {
                    commands::on_files_dropped(&app.state::<state::AppState>(), &paths);
                });
            }
        })
        .invoke_handler(tauri::generate_handler![
            commands::get_state,
            commands::set_offline,
            commands::set_favourite_model,
            commands::system_memory,
            commands::get_draft,
            commands::search_history,
            commands::set_draft,
            lockdown::set_lockdown,
            commands::install_runtime,
            commands::import_model,
            commands::acknowledge_template,
            commands::delete_model,
            commands::search_hub,
            commands::list_repo_files,
            commands::download_model,
            commands::cancel_download,
            commands::pause_download,
            picker::starter_models,
            picker::choose_model,
            picker::download_starter,
            picker::hub_ranking,
            picker::model_research_prompt,
            picker::research_repositories,
            commands::load_model,
            commands::unload_model,
            commands::set_generation,
            commands::list_conversations,
            commands::create_conversation,
            commands::get_conversation,
            commands::rename_conversation,
            commands::set_conversation_prompt,
            commands::delete_conversation,
            commands::truncate_conversation,
            commands::branch_conversation,
            commands::export_conversation,
            commands::send_message,
            commands::cancel_chat,
            commands::list_pending_approvals,
            commands::decide_approval,
            commands::get_audit_log,
            agent::agent_state,
            agent::agent_sessions,
            activity::agent_dashboard,
            activity::agent_attention,
            activity::set_showing_session,
            projects::list_projects,
            projects::create_project,
            projects::rename_project,
            projects::project_setup,
            projects::init_project_repository,
            commands::set_project_toolchains,
            sandbox_restart::agent_restart_sandbox,
            sandbox_restart::agent_install_check,
            sandbox_restart::agent_install_dismiss,
            new_project::new_project_pick,
            new_project::new_project_options,
            new_project::create_guided_project,
            readiness::project_readiness,
            readiness::project_instructions_draft,
            readiness::save_project_instructions,
            parent_folders::parent_folders_status,
            parent_folders::set_parent_folders,
            projects::add_project_folders,
            projects::remove_project_folder,
            vault::project_secrets,
            vault::move_project_secrets,
            vault::restore_project_secrets,
            vault::project_gitignore,
            vault::add_project_gitignore,
            projects::delete_project,
            agent::agent_start,
            agent::agent_end,
            agent::agent_stop,
            agent::agent_send,
            agent::agent_undo,
            agent::agent_transcripts,
            agent::agent_transcript,
            agent::agent_resume,
            agent::agent_delete_transcript,
            updates::get_updates,
            updates::check_updates,
            app_update::download_app_update,
            app_update::install_app_update,
            diagnostics::diagnostics_report,
            diagnostics::save_diagnostics,
            speech::speech_prepare,
            speech::speech_voices,
            speech::speak_piece,
            speech::set_voice_settings,
            dictation::dictation_status,
            dictation::dictation_setup,
            dictation::dictation_start,
            dictation::dictation_stop,
            dictation::dictation_partial,
            dictation::dictation_cancel,
            backup::backup_status,
            backup::create_backup,
            backup::choose_backup,
            backup::inspect_backup,
            backup::restore_backup,
            backup::cancel_restore,
            backup::restart_to_restore,
            updates::set_update_schedule,
            updates::set_active_runtime,
            updates::remove_runtime,
            updates::check_model_update,
            updates::test_runtime,
            commands::add_agent_tool_folder,
            commands::add_git_tool_folder,
            commands::remove_agent_tool_folder,
            commands::set_searxng_url,
            agent::agent_set_plan_mode,
            agent::agent_compact,
            agent::agent_stop_background,
            agent::agent_mcp_prompt,
            research::agent_start_research,
            research::agent_set_web_mode,
            research::agent_visits,
            research::chat_visits,
            research::web_status,
            research::set_web_settings,
            research::set_brave_key,
            research::web_test_connection,
            research::web_find_searxng,
            research::chat_web,
            chat_web::chat_web_state,
            chat_web::chat_set_web,
            agent::agent_clear,
            agent::agent_context,
            agent::agent_instructions,
            agent::agent_save_instructions,
            skills::list_skills,
            skills::add_skill,
            skills::remove_skill,
            mcp::list_mcp_servers,
            mcp::add_mcp_server,
            mcp::set_mcp_args,
            mcp::set_mcp_env,
            mcp::set_mcp_enabled,
            mcp::add_mcp_folder,
            mcp::remove_mcp_folder,
            mcp::remove_mcp_server,
            mcp::rename_mcp_server,
            mcp::test_mcp_server,
            mcp::accept_mcp_tools,
            attach::attach_files,
            attach::discard_attachment,
            commands::attach_pasted_image,
            commands::watch_dropped_files,
            commands::unwatch_dropped_files,
            binary::binary_settings,
            commands::set_agent_settings,
            agent::agent_steer,
            agent::agent_question,
            agent::agent_changes,
            agent::agent_revert_file,
            agent::agent_answer,
            agent::agent_run_request,
            agent::agent_run_answer,
            research_agent::agent_research_card,
            research_agent::agent_research_answer,
            agent::agent_reload,
            agent::agent_user_shell,
            agent::agent_complete_path,
            agent::agent_rename,
            agent::rename_transcript,
            agent::agent_session_info,
            agent::agent_fork_points,
            agent::agent_fork,
            handoff::handoff_draft,
            handoff::handoff_start,
            handoff::handoff_save_as,
            handoff::handoff_save_to_project,
            agent::export_transcript,
            agent::import_transcript,
            binary::set_binary_folder,
            commands::set_model_projector,
            commands::set_model_load,
            vision::find_projectors,
            commands::set_model_speculation,
            commands::set_model_expert_cache,
            commands::set_model_thinking,
            memory::list_memories,
            memory::add_memory,
            memory::pick_memory_folder,
            memory::update_memory,
            memory::delete_memory,
            memory::delete_memories,
            memory::chat_memories,
            memory::set_chat_memories,
            agent::agent_set_memories,
            commands::suggest_settings,
            commands::apply_suggested_settings,
            documents::list_collections,
            documents::create_collection,
            documents::rename_collection,
            documents::delete_collection,
            documents::collection_files,
            documents::remove_document,
            documents::document_text,
            documents::reindex_documents,
            documents::add_documents,
            documents::index_collection,
            documents::set_embedding_model,
            documents::set_chat_collection,
            documents::chat_collection,
        ])
        .build(tauri::generate_context!())
        .expect("failed to build Gardenwall");

    app.run(|handle, event| {
        if let tauri::RunEvent::Exit = event {
            // End sessions and stop the runtime cleanly, so their sandbox
            // grants are revoked (within a time limit; the rest is cleaned
            // up at the next start, in the background).
            lifecycle::shut_down(&handle.state::<state::AppState>());
        }
    });
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_data_folder_matches_the_bundle_identifier() {
        let config: serde_json::Value =
            serde_json::from_str(include_str!("../tauri.conf.json")).unwrap();
        assert_eq!(config["identifier"], super::DATA_FOLDER);
    }
}
