//! The samples tab's details of the sound under the cursor. The file is
//! read for its facts on a thread of its own, one read at a time, because
//! a folder can sit on a slow disk or a network share.

use std::sync::mpsc::{Receiver, TryRecvError};

use rustel_runtime::samples::SampleFacts;

use super::*;
use crate::reference::{SampleDetails, Tab};

/// How long a read may take before its file is shown with no facts and the
/// next file's read may start. A share that stopped answering holds its
/// own thread, not the details of every file after it.
const READ_PATIENCE: Duration = Duration::from_secs(5);

/// The one read out: which file it reads, when it started, and where its
/// answer comes.
pub(super) struct DetailsRead {
    url: String,
    started: Instant,
    answer: Receiver<SampleFacts>,
    /// The file changed while this read was out: its answer may describe
    /// the file as it was, so the details are read again after it.
    superseded: bool,
}

impl App {
    /// Each turn on the samples tab: the file under the cursor. A new file
    /// goes in the panel with no facts, and stale facts are read again, one
    /// read at a time. An answer lands only while its file is still the one
    /// under the cursor; the cursor moved on otherwise, and the next read
    /// starts.
    pub(super) fn poll_sample_details(&mut self) {
        if let Some(read) = &self.details_read {
            let answer = match read.answer.try_recv() {
                Ok(facts) => Some(facts),
                // A read that failed or took too long says nothing more
                // than a file with no facts would.
                Err(TryRecvError::Disconnected) => Some(SampleFacts::default()),
                Err(TryRecvError::Empty) if read.started.elapsed() >= READ_PATIENCE => {
                    Some(SampleFacts::default())
                }
                Err(TryRecvError::Empty) => None,
            };
            if let Some(facts) = answer
                && let Some(read) = self.details_read.take()
                && let Some(details) = self
                    .reference_panel
                    .as_mut()
                    .and_then(|panel| panel.sample_details.as_mut())
                    .filter(|details| details.url == read.url)
            {
                details.facts = Some(facts);
                details.stale = read.superseded;
                self.dirty_frame = true;
            }
        }
        let Some(panel) = self
            .reference_panel
            .as_mut()
            .filter(|panel| panel.tab == Tab::Samples)
        else {
            return;
        };
        let library = self.worker.library();
        let url = panel.details_target().and_then(|(name, variant)| {
            library
                .as_ref()
                .and_then(|library| library.file_location(&name, variant))
        });
        if url.as_deref()
            != panel
                .sample_details
                .as_ref()
                .map(|details| details.url.as_str())
        {
            panel.sample_details = url.map(SampleDetails::unread);
            self.dirty_frame = true;
        }
        if self.details_read.is_some() {
            return;
        }
        let (Some(library), Some(details)) = (library, panel.sample_details.as_ref()) else {
            return;
        };
        if !details.stale {
            return;
        }
        let url = details.url.clone();
        let (sender, answer) = std::sync::mpsc::channel();
        let reading = url.clone();
        let started = std::thread::Builder::new()
            .name("sample-details".into())
            .spawn(move || {
                let _ = sender.send(library.sample_facts(&reading));
            });
        if started.is_ok() {
            self.details_read = Some(DetailsRead {
                url,
                started: Instant::now(),
                answer,
                superseded: false,
            });
        }
    }

    /// The selected file may have changed on disk: a remote file arrives in
    /// the cache when it loads, and a trim rewrites a file in place. Its
    /// facts are read again; those on show stay until the read answers.
    pub(super) fn refresh_sample_details(&mut self, remote_only: bool) {
        let Some(details) = self
            .reference_panel
            .as_mut()
            .and_then(|panel| panel.sample_details.as_mut())
            .filter(|details| {
                !remote_only || details.facts.as_ref().is_some_and(|facts| facts.remote)
            })
        else {
            return;
        };
        details.stale = true;
        if let Some(read) = self
            .details_read
            .as_mut()
            .filter(|read| read.url == details.url)
        {
            read.superseded = true;
        }
    }
}
