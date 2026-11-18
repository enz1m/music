//! Reducer controls for playlist import staleness and export custody, driven
//! through `MusicApp::update`.

use super::*;
use crate::playlist_io::{ParseReport, PlaylistFormat};
use crate::playlist_transfer::{ExportPublishError, ExportSettlement, ImportSettlement};

fn report(path: &str) -> ParseReport {
    ParseReport {
        format: PlaylistFormat::M3u8,
        paths: vec![std::path::PathBuf::from(path)],
        refused: 0,
        duplicate: 0,
    }
}

#[test]
fn stale_import_generation_creates_no_playlist() {
    let mut a = MusicApp::boot_with_demo().0;
    let playlists_before = a.library.playlists.len();

    a.import_job = Some(ImportJob {
        generation: 7,
        path: std::path::PathBuf::from("/music/new.m3u8"),
        state: JobState::InFlight,
    });

    let _ = a.update(Message::PlaylistImportSettled {
        generation: 6,
        settlement: ImportSettlement::Parsed(report("/music/a.mp3")),
    });

    assert_eq!(
        a.library.playlists.len(),
        playlists_before,
        "a stale import generation must create no playlist"
    );
    assert!(
        matches!(a.import_job.as_ref().map(|j| j.generation), Some(7)),
        "the current in-flight import must be left untouched by a stale settlement"
    );
}

#[test]
fn export_settlement_custody_retains_busy_but_not_hard_failure() {
    let mut a = MusicApp::boot_with_demo().0;

    let busy_gen = 11;
    a.export_jobs.insert(busy_gen, export_job("Mix"));
    let _ = a.update(Message::PlaylistExportSettled {
        generation: busy_gen,
        settlement: ExportSettlement::Busy,
    });
    assert_eq!(
        a.export_jobs.get(&busy_gen).map(|j| j.state),
        Some(JobState::AwaitingRetry),
        "a Busy export must retain the exact intent awaiting one coalesced retry"
    );

    let fail_gen = 12;
    a.export_jobs.insert(fail_gen, export_job("Other"));
    let _ = a.update(Message::PlaylistExportSettled {
        generation: fail_gen,
        settlement: ExportSettlement::Failed(ExportPublishError::PrePublication("disk".into())),
    });
    assert!(
        !a.export_jobs.contains_key(&fail_gen),
        "a hard pre-publication failure must clear the job, never report success"
    );

    let _ = a.update(Message::PlaylistExportSettled {
        generation: busy_gen,
        settlement: ExportSettlement::Published,
    });
    assert!(
        !a.export_jobs.contains_key(&busy_gen),
        "a durable publication clears the export job"
    );
}

fn export_job(name: &str) -> ExportJob {
    ExportJob {
        playlist_id: 0,
        display_name: name.to_string(),
        paths: vec![std::path::PathBuf::from("/music/a.mp3")],
        grant: None,
        state: JobState::InFlight,
    }
}
