use super::*;
use crate::ui::{ScrollingWindowTitle, TitlebarProgress};
use adw::gtk;

#[test]
fn cancelling_updates_restores_controls_and_ignores_late_results() {
    gtk::init().expect("GTK must initialize for the updater UI test");
    let app = Application::builder()
        .application_id("io.github.noobping.listenmoe.UpdaterTest")
        .build();
    let ui = UpdateUi {
        win_title: ScrollingWindowTitle::new("Artist", "Song"),
        normal_title: Rc::new(RefCell::new(("Artist".into(), "Song".into()))),
        playback_playing: Rc::new(Cell::new(false)),
        update_active: Rc::new(Cell::new(false)),
        update_title_override: Rc::new(Cell::new(false)),
        play_button: gtk::Button::new(),
        pause_button: gtk::Button::new(),
        titlebar_progress: TitlebarProgress::for_test(),
    };
    let controller = UpdaterController::new(&app, ui);
    let release = SelectedRelease {
        version: "99.0.0".parse().unwrap(),
        asset: ReleaseAsset {
            name: "listenmoe.msi".into(),
            browser_download_url: "https://example.invalid/listenmoe.msi".into(),
            size: 100,
            sha256_digest: None,
        },
    };
    let download = DownloadedUpdate {
        path: PathBuf::from("unused-test-installer.msi"),
        size: release.asset.size,
    };

    for mode in [CheckMode::Automatic, CheckMode::Manual] {
        for playing in [false, true] {
            let cancel = Arc::new(AtomicBool::new(false));
            let states = [
                UpdateState::Checking { mode, run_id: 1 },
                UpdateState::Downloading {
                    mode,
                    run_id: 1,
                    release: release.clone(),
                    download: download.clone(),
                    cancel: cancel.clone(),
                    downloaded: 25,
                },
                UpdateState::Ready {
                    release: release.clone(),
                    download: download.clone(),
                },
            ];
            for state in states {
                let downloading = matches!(state, UpdateState::Downloading { .. });
                *controller.inner.state.borrow_mut() = state;
                controller.inner.ui.playback_playing.set(playing);
                controller.show_update_ui();
                controller.cancel_update();

                assert!(matches!(
                    *controller.inner.state.borrow(),
                    UpdateState::Idle
                ));
                assert!(!controller.inner.ui.update_active.get());
                assert_eq!(controller.inner.ui.play_button.is_visible(), !playing);
                assert_eq!(controller.inner.ui.pause_button.is_visible(), playing);
                assert!(controller.inner.check_action.is_enabled());
                assert!(!controller.inner.cancel_action.is_enabled());
                if downloading {
                    assert!(cancel.load(Ordering::Relaxed));
                }

                let generation = controller.inner.status_generation.get();
                controller.cancel_update();
                for message in [
                    WorkerMessage::CheckFinished {
                        run_id: 1,
                        result: Ok(Some(release.clone())),
                    },
                    WorkerMessage::DownloadProgress {
                        run_id: 1,
                        downloaded: 75,
                    },
                    WorkerMessage::DownloadReady {
                        run_id: 1,
                        download: download.clone(),
                    },
                    WorkerMessage::DownloadCancelled { run_id: 1 },
                    WorkerMessage::DownloadFailed {
                        run_id: 1,
                        error: "late failure".into(),
                    },
                ] {
                    controller.handle_worker_message(message);
                    assert!(matches!(
                        *controller.inner.state.borrow(),
                        UpdateState::Idle
                    ));
                    assert_eq!(controller.inner.status_generation.get(), generation);
                    assert!(!controller.inner.ui.update_active.get());
                }
            }
        }
    }

    *controller.inner.state.borrow_mut() = UpdateState::Installing;
    let generation = controller.inner.status_generation.get();
    controller.cancel_update();
    assert!(matches!(
        *controller.inner.state.borrow(),
        UpdateState::Installing
    ));
    assert_eq!(controller.inner.status_generation.get(), generation);

    // Drain the temporary status timers on the same GTK thread before it exits.
    let context = glib::MainContext::default();
    let done = Rc::new(Cell::new(false));
    let done_for_timeout = done.clone();
    glib::timeout_add_local_once(Duration::from_millis(TEMPORARY_STATUS_MS + 50), move || {
        done_for_timeout.set(true);
    });
    while !done.get() {
        context.iteration(true);
    }
}
