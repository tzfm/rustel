//! Auditioning things from the reference panel: chords, scales and tunings
//! played as quick runs, single sample sounds with their waveform, and snippets
//! (including the generator tab's) played under or instead of the score. Also
//! covers preview volume, the loading, count-in and progress captions on the
//! shelf, stopping a preview, and the helpers that label a bare score and add
//! the "// preview" banner so a previewed snippet plays alongside it.

use super::*;

/// The voice a chord is previewed on, and what stands in for it before
/// its bank has downloaded.
const CHORD_PREVIEW_SOUND: &str = "piano";
const CHORD_PREVIEW_FALLBACK: &str = "triangle";

/// The octave a previewed chord is played in, matching the panel's own.
const CHORD_PREVIEW_OCTAVE: i32 = 3;

/// A scale is previewed from C when a list names no tonic.
const SCALE_PREVIEW_TONIC: &str = "C";

/// A run never lights more notes than the engine will play.
const MAX_PREVIEW_RUN_NOTES: usize = 24;

/// One note every 150 ms - 400 to the minute. Quick enough to hear the
/// shape of a scale in one breath, slow enough that a piano's attack is a
/// note and not a click; each note rings on over the next, the way a hand
/// does not lift off the keys between them.
const SCALE_PREVIEW_STEP_SECS: f64 = 60.0 / 400.0;

/// How long the chord's keys stay in the "sounding" colour after Space.
/// The notes are fired together and decay on their own; this is only the
/// picture.
const CHORD_PREVIEW_GLOW: Duration = Duration::from_millis(1800);

/// How long after a preview starts Esc still means "stop it": longer than
/// any sample loop worth previewing, shorter than a forgotten key.
pub(super) const PREVIEW_STOP_WINDOW: Duration = Duration::from_secs(20);

/// A snippet auditioned under the sounding score.
///
/// One engine plays one score, so a preview is not a second engine: it is
/// the score with the snippet's voices appended, installed the way a save
/// is. What that buys is exactly what a preview wants - the set's tempo,
/// the set's clock, the same cutover on the same cycle line - and what it
/// costs is remembering the score to put back.
#[cfg(feature = "hydra")]
#[derive(Clone, Debug)]
pub(super) struct SnippetPreview {
    /// The snippet as the shelf shows it: what a second press stops.
    pub(super) code: String,
    /// The row it was taken from, so the word for what it is doing stays
    /// on that row rather than following the cursor to another.
    pub(super) row: Option<super::super::reference::SnippetLine>,
    /// Each played line as `(offset in the block, offset in `code`)`.
    pub(super) lines: Vec<(usize, usize)>,
    /// The revision of the source installed for it: what the engine
    /// answers with once the preview is the thing sounding.
    pub(super) revision: String,
    /// The exact composite handed to the engine, shared with its request.
    /// Readiness belongs to this source, not the score being edited or
    /// whichever snippet the shelf cursor has moved to since.
    pub(super) source: Arc<str>,
    /// A source-based estimate, not a query of the runtime-selected voices.
    pub(super) readiness: Option<Readiness>,
    /// Where its first byte sits in what the engine was handed, so the
    /// marks coming back can be told from the score's.
    pub(super) offset: usize,
    /// The score to put back, and the revision it was evaluated at. Absent
    /// when the studio was silent and the snippet is playing alone.
    pub(super) restore: Option<(Arc<str>, Revision)>,
    pub(super) scene: SceneId,
    /// That the count has seen this preview's own clock: the first
    /// snapshot whose `source_revision` is the preview's. The count and
    /// the strip ride the snapshot's clock, and a snapshot sampled before
    /// the install's cutover still carries the outgoing one - counting on
    /// it counts the wrong bar, and the install's rebased zero arriving
    /// afterwards reads as the clock jumping backward. Only from the first
    /// snapshot that names this revision is the cycle the preview's.
    pub(super) clock_owned: std::cell::Cell<bool>,
    /// Where the count-in's watch-keeping stands: the cycle line it is
    /// counting to, once it has one. A count read straight off the
    /// running clock re-arms at the very line it just counted to, so
    /// whenever the preview's first mark goes unmapped it would tally the
    /// next bar and the one after - the countdown the ear misses and
    /// hears start again. Capturing the line on the first look, and
    /// landing when the clock reaches it, ends the wait once and for all.
    pub(super) count_in_target: std::cell::Cell<Option<f64>>,
    /// That the wait is over for good: the line was crossed (or is here),
    /// so no count again for this preview. Dies with it, so the next one
    /// counts afresh.
    pub(super) count_in_landed: std::cell::Cell<bool>,
}

#[cfg(feature = "hydra")]
impl SnippetPreview {
    fn is_generator(&self) -> bool {
        matches!(
            self.row,
            Some(super::super::reference::SnippetLine::Generator(_))
        )
    }

    /// Where a mark from the played block falls in the snippet the shelf
    /// shows. The block is the snippet with whole lines taken out, so a
    /// column is a column; only the line moves.
    pub(super) fn mark_in_snippet(&self, mark: &SourceMark) -> Option<SourceMark> {
        let at = mark.from.checked_sub(self.offset)?;
        let length = mark.to.saturating_sub(mark.from);
        let (block_line, code_line) = *self
            .lines
            .iter()
            .rev()
            .find(|(block_at, _)| *block_at <= at)?;
        let from = code_line + (at - block_line);
        (from < self.code.len()).then(|| SourceMark {
            from,
            to: (from + length).min(self.code.len()),
            ..*mark
        })
    }
}

/// Step the preview volume by decibels, like a fader: below the floor is
/// true silence, and a step up from silence starts at the floor.
pub(super) fn nudge_preview_gain(gain: f32, delta_db: f32) -> f32 {
    let was = if gain <= 0.0 {
        super::super::reference::PREVIEW_FLOOR_DB
    } else {
        rustel_audio::linear_to_db(gain)
    };
    let mut db = was + delta_db;
    // The unity detent the master fader has: stepping past 0 dB lands on
    // it exactly, so the default is always one keystroke away.
    if db.signum() != was.signum() && db.abs() < delta_db.abs() {
        db = 0.0;
    }
    if db <= super::super::reference::PREVIEW_FLOOR_DB {
        0.0
    } else {
        rustel_audio::db_to_linear(db.min(super::super::reference::PREVIEW_CEIL_DB))
    }
}

impl App {
    #[cfg(feature = "hydra")]
    pub(super) fn queue_generator_preview(&mut self) {
        self.generator_feedback_since = Some(Instant::now());
        self.generator_preview_pending = true;
        self.flush_generator_preview(Instant::now());
        self.dirty_frame = true;
    }

    #[cfg(feature = "hydra")]
    pub(super) fn flush_generator_preview(&mut self, now: Instant) {
        if !self.generator_preview_pending {
            return;
        }
        let Some(panel) = self
            .reference_panel
            .as_ref()
            .filter(|panel| panel.tab == Tab::Generator)
        else {
            self.generator_preview_pending = false;
            return;
        };
        // Leading edge is immediate; during a hold, keep hearing the latest
        // value every 50 ms. A trailing update is never lost on key release.
        if self
            .generator_last_preview
            .is_some_and(|last| now.saturating_duration_since(last) < Duration::from_millis(50))
        {
            return;
        }
        let Some(code) = panel.selected_snippet_code().map(|code| code.into_owned()) else {
            return;
        };
        self.generator_preview_pending = false;
        self.generator_last_preview = Some(now);
        self.preview_snippet_score_inner(&code, true, false);
    }

    /// Ask for the sounds the snippet under the cursor names, before it is
    /// asked to play.
    ///
    /// Resolving a name is what starts its download, so a row looked at for
    /// a second is a row whose sounds are already on their way. Without it
    /// the first press on a snippet full of soundfont instruments waits on
    /// the fetch, which reads as a preview that took two seconds to start -
    /// and the same snippet is instant the second time, which reads as a
    /// fault rather than as a download.
    ///
    /// Only a row the cursor rests on: resolving also decodes, and a held
    /// arrow through the Examples must not decode every font of every row
    /// it passes.
    #[cfg(feature = "hydra")]
    pub(super) fn warm_selected_snippet(&mut self) {
        let Some(code) = self
            .reference_panel
            .as_ref()
            .filter(|panel| panel.tab.is_snippets())
            .and_then(|panel| panel.selected_snippet_code())
        else {
            self.snippet_under_cursor = None;
            return;
        };
        if self.warmed_snippet.as_deref() == Some(code.as_ref()) {
            // Back on the row already asked for: whatever row the cursor
            // was resting on meanwhile starts its wait afresh next time.
            self.snippet_under_cursor = None;
            return;
        }
        let now = Instant::now();
        match &self.snippet_under_cursor {
            Some((resting, since)) if resting.as_str() == code.as_ref() => {
                if now.saturating_duration_since(*since) < SNIPPET_WARM_DWELL {
                    return;
                }
            }
            _ => {
                self.snippet_under_cursor = Some((code.into_owned(), now));
                return;
            }
        }
        let Some(library) = self.worker.library() else {
            return;
        };
        // The count is not wanted; asking is.
        let _ = Readiness::of(&code, &library);
        self.warmed_snippet = Some(code.into_owned());
    }

    /// A preview can keep sounding while another scene or shelf row is
    /// selected. Follow its own source until it is replaced or stopped,
    /// including failures that recover after a later loader retry.
    #[cfg(feature = "hydra")]
    pub(super) fn refresh_preview_readiness(&mut self) {
        let Some(preview) = self.snippet_preview.as_mut() else {
            return;
        };
        let Some(library) = self.worker.library() else {
            return;
        };
        let readiness = Readiness::of(&preview.source, &library);
        self.dirty_frame |= preview.readiness.as_ref() != Some(&readiness);
        preview.readiness = Some(readiness);
        // Keep footer retirement on its diagnostic lapse: this source
        // estimate cannot prove runtime-selected variants finished loading.
    }

    /// Play a chord: every note of it at once, on a voice the browser
    /// always has. `piano` if its bank has arrived, the triangle wave
    /// otherwise - a preview that waits for a download is not a preview.
    pub(super) fn preview_chord(&mut self, chord: &str) {
        let notes = rustel_core::voicings::chord_notes(chord, None, CHORD_PREVIEW_OCTAVE)
            .unwrap_or_default()
            .iter()
            .map(|midi| *midi as f32)
            .collect::<Vec<_>>();
        if notes.is_empty() {
            self.status = format!("{chord} has no voicing to play");
            self.dirty_frame = true;
            return;
        }
        let sound = self
            .worker
            .library()
            .filter(|library| library.knows_sound(CHORD_PREVIEW_SOUND))
            .map(|_| CHORD_PREVIEW_SOUND)
            .unwrap_or(CHORD_PREVIEW_FALLBACK);
        if self
            .worker
            .try_audition_notes(&notes, sound, self.preview_gain)
        {
            self.preview_armed = Some((chord.to_owned(), Instant::now()));
            self.preview_lit_until = Some(Instant::now() + CHORD_PREVIEW_GLOW);
            self.status = format!("preview {chord}");
        } else {
            self.status = format!("the engine is busy; {chord} was not previewed");
        }
        self.dirty_frame = true;
    }

    /// The note of a run the ear is on now: how far the clock has walked
    /// since the preview started. A chord's voicing and a run's steps are
    /// lit only for as long as they sound; `None` when nothing is running
    /// or the run has finished.
    pub(super) fn sounding_preview_note(&self) -> Option<usize> {
        let (sounding, started) = self.preview_armed.as_ref()?;
        let panel = self.reference_panel.as_ref()?;
        // Only the row that is actually playing lights up: a preview of
        // something else, or of a sample, animates nothing.
        if let Some(scale) = panel.selected_scale() {
            if scale != *sounding {
                return None;
            }
            let notes = panel
                .selected_scale_notes()
                .len()
                .min(MAX_PREVIEW_RUN_NOTES);
            if notes == 0 {
                return None;
            }
            let until = SCALE_PREVIEW_STEP_SECS * notes as f64;
            (started.elapsed().as_secs_f64() < until).then_some(usize::MAX)
        } else if panel.selected_chord().as_deref() == Some(sounding.as_str())
            && started.elapsed() <= CHORD_PREVIEW_GLOW
        {
            // Every note of a chord sounds at once. `usize::MAX` recolours
            // the whole voicing, matching the ear.
            Some(usize::MAX)
        } else {
            None
        }
    }

    /// Play a scale: its notes one after another, quickly enough that the
    /// shape of it is one phrase and not a sequence of separate notes.
    pub(super) fn preview_scale(&mut self, scale: &str) {
        // A scale from the browser is written with its tonic; one from a
        // word list is not, and starts from C.
        let written = if scale.contains(':') {
            scale.to_owned()
        } else {
            format!("{SCALE_PREVIEW_TONIC}:{scale}")
        };
        let run = super::super::reference::scale_notes(&written)
            .iter()
            .map(|midi| *midi as f32)
            .collect::<Vec<_>>();
        if run.is_empty() {
            self.status = format!("{scale} has no notes to play");
            self.dirty_frame = true;
            return;
        }
        let sound = self
            .worker
            .library()
            .filter(|library| library.knows_sound(CHORD_PREVIEW_SOUND))
            .map(|_| CHORD_PREVIEW_SOUND)
            .unwrap_or(CHORD_PREVIEW_FALLBACK);
        if self
            .worker
            .try_audition_run(&run, sound, self.preview_gain, SCALE_PREVIEW_STEP_SECS)
        {
            self.preview_armed = Some((scale.to_owned(), Instant::now()));
            self.preview_lit_until = Some(
                Instant::now()
                    + Duration::from_secs_f64(
                        SCALE_PREVIEW_STEP_SECS * run.len().min(MAX_PREVIEW_RUN_NOTES) as f64,
                    ),
            );
            self.status = format!("preview {scale}");
        } else {
            self.status = format!("the engine is busy; {scale} was not previewed");
        }
        self.dirty_frame = true;
    }

    /// Play a tuning as a rising run. A tuning names no notes, so this is
    /// the only way to tell one from another without reading a table of
    /// ratios - which is the reason the list is worth opening at all.
    pub(super) fn preview_tuning(&mut self, tuning: &str) {
        let run = super::super::reference::tuning_notes(tuning)
            .iter()
            .map(|midi| *midi as f32)
            .collect::<Vec<_>>();
        if run.is_empty() {
            self.status = format!("{tuning} has no notes to play");
            self.dirty_frame = true;
            return;
        }
        let sound = self
            .worker
            .library()
            .filter(|library| library.knows_sound(CHORD_PREVIEW_SOUND))
            .map(|_| CHORD_PREVIEW_SOUND)
            .unwrap_or(CHORD_PREVIEW_FALLBACK);
        if self
            .worker
            .try_audition_run(&run, sound, self.preview_gain, SCALE_PREVIEW_STEP_SECS)
        {
            self.preview_armed = Some((tuning.to_owned(), Instant::now()));
            self.status = format!("preview {tuning}");
        } else {
            self.status = format!("the engine is busy; {tuning} was not previewed");
        }
        self.dirty_frame = true;
    }

    /// Hear one sound once, outside the score.
    pub(super) fn preview_sound(&mut self, sound: &str) {
        if self.worker.try_audition(sound, self.preview_gain) {
            self.preview_armed = Some((sound.to_owned(), Instant::now()));
            self.preview_shape = self
                .worker
                .library()
                .and_then(|library| library.sound_shape(sound));
            self.status = format!("preview {sound}");
        } else {
            self.status = format!("the engine is busy; {sound} was not previewed");
        }
        self.dirty_frame = true;
    }

    pub(super) fn show_loaded_preview_shape(
        &mut self,
        sound: String,
        shape: (std::sync::Arc<[u8]>, f64),
    ) {
        self.preview_shape = Some(shape);
        // A first-time preview begins only after its sample downloads and
        // the engine retries it. The click's old timestamp can already be
        // beyond a short sample's duration by then, hiding the waveform on
        // its first frame. Start the picture when the decoded shape arrives;
        // cached previews take the immediate path in `preview_sound`.
        self.preview_armed = Some((sound, Instant::now()));
        self.dirty_frame = true;
    }

    /// A sound heard for the first time is being decoded at the moment it
    /// is asked for, so its shape is not there yet. The readiness poll asks
    /// again while the preview is still sounding.
    pub(super) fn poll_preview_shape(&mut self) {
        if self.preview_shape.is_none()
            && let Some((sound, started)) = self.preview_armed.clone()
            && started.elapsed() <= PREVIEW_STOP_WINDOW
            && let Some(library) = self.worker.library()
            && let Some(shape) = library.sound_shape(&sound)
        {
            self.show_loaded_preview_shape(sound, shape);
        }
    }

    /// Play a snippet under the sounding score: the set's tempo, the set's
    /// clock, one cycle line to land on. A second press on the same snippet
    /// stops it and puts the score back the way it was.
    ///
    /// Silent, the snippet plays on its own, tempo and all - there is no
    /// score to borrow one from, and starting the set to hear a shelf entry
    /// is not what was asked for.
    #[cfg(feature = "hydra")]
    pub(super) fn preview_snippet_score(&mut self, code: &str) {
        self.generator_preview_pending = false;
        let generator = self
            .reference_panel
            .as_ref()
            .is_some_and(|panel| panel.tab == Tab::Generator);
        self.preview_snippet_score_inner(code, generator, true);
    }

    #[cfg(feature = "hydra")]
    fn preview_snippet_score_inner(&mut self, code: &str, generator: bool, toggle: bool) {
        if self
            .snippet_preview
            .as_ref()
            .is_some_and(|preview| preview.code == code)
        {
            if toggle {
                self.stop_snippet_preview();
            }
            return;
        }
        let playing = self.is_playing() && !self.is_stopping() && !self.stop_requested;
        // One preview at a time. With one already sounding, the score this
        // is measured against is the one under that - what is installed
        // now is the score with the last snippet on the end of it, and
        // building on that would leave the two playing together.
        let replacing = self.snippet_preview.is_some();
        let under = match self.snippet_preview.take() {
            // The set it was playing under may have been stopped since.
            // Putting the score back to hang a new snippet on it would
            // start music nobody asked to hear again.
            Some(sounding) => sounding.restore.filter(|_| playing),
            None => playing
                .then(|| self.evaluated_source.clone())
                .flatten()
                .map(|score| {
                    // The revision the marks are followed through. Absent, the
                    // score simply gets none: the preview still plays.
                    let revision = self
                        .visual_revision
                        .or_else(|| self.audible_editor().map(Editor::revision))
                        .unwrap_or_else(|| self.editor().revision());
                    (score, revision)
                }),
        };
        // A preview never sets the tempo. Under a set it plays along, on
        // the set's clock and its cycle line, so what you hear is how the
        // snippet will sit in the music. In silence it plays at whatever
        // the studio is at - 120 until something says otherwise - so two
        // snippets heard one after the other can be compared rather than
        // each arriving at a tempo of its own. The `setcps` stays in the
        // text on the shelf, so taking the snippet still brings the tempo
        // it was written at.
        let (block, lines) = if generator {
            self.generator_preview_epoch += 1;
            self.generator_feedback_since = Some(Instant::now());
            generator_preview_block(code, self.generator_preview_epoch)
        } else {
            preview_block(code)
        };
        if block.trim().is_empty() {
            self.toast("nothing to preview");
            return;
        }
        let scene = under
            .as_ref()
            .and(self.audible_scene)
            .unwrap_or_else(|| self.scenes.current().id);
        let (source, offset) = match &under {
            Some((score, _)) => {
                let mut text = labelled_score_text(score);
                if !text.ends_with('\n') {
                    text.push('\n');
                }
                text.push_str(SNIPPET_PREVIEW_BANNER);
                let offset = text.len();
                text.push_str(&block);
                (text, offset)
            }
            // Alone, at the tempo the snippet says it was written for, or
            // 120 when it says nothing: whatever the studio was last set
            // to is not the tempo of the thing being auditioned. The
            // snippet's own text still sets nothing - the comment is read
            // here - so taking it into a set cannot move the set.
            None => {
                let tempo = if generator {
                    self.snapshot
                        .as_ref()
                        .map(|snapshot| snapshot.cps)
                        .filter(|cps| cps.is_finite() && *cps > 0.0)
                        .unwrap_or(0.5)
                } else {
                    named_tempo(code).unwrap_or(0.5)
                };
                let mut text = if generator {
                    format!("setcps({tempo:.6})\n")
                } else {
                    format!("setcps({tempo:.4})\n")
                };
                let offset = text.len();
                text.push_str(&block);
                (text, offset)
            }
        };
        let revision = under
            .as_ref()
            .map(|(_, revision)| *revision)
            .or_else(|| self.scenes.get(scene).map(|scene| scene.editor.revision()))
            .unwrap_or_default();
        // Examples keep their immediate audition behavior. Generated edits
        // join a playing set at the next cycle; auditioning alone has no
        // external cycle to wait for.
        let alone = under.is_none();
        self.next_launch = if generator && !alone {
            Launch::Quantised { unit_cycles: 1.0 }
        } else {
            Launch::Now
        };
        self.snippet_heard = false;
        let row = self.reference_panel.as_ref().and_then(|panel| {
            if panel.tab == Tab::Generator {
                Some(super::super::reference::SnippetLine::Generator(
                    super::super::ideas::Row::Generate(panel.generator.direction()),
                ))
            } else {
                panel.snippet_lines().get(panel.snippet_selected).copied()
            }
        });
        let source = Arc::<str>::from(source);
        self.snippet_preview = Some(SnippetPreview {
            code: code.to_owned(),
            row,
            lines,
            revision: source_revision(&source),
            source: Arc::clone(&source),
            readiness: None,
            offset,
            restore: under,
            scene,
            clock_owned: std::cell::Cell::new(false),
            count_in_target: std::cell::Cell::new(None),
            count_in_landed: std::cell::Cell::new(false),
        });
        self.refresh_preview_readiness();
        // Installing the next one does not silence the last: a reload
        // lets sounding voices ring, which is right for an edit and wrong
        // here - a four-second break would play on under the snippet that
        // replaced it.
        //
        // The question is not whether the transport is running, since a
        // snippet playing on its own is the transport running. It is
        // whether anything but this preview is meant to be heard: with no
        // score under it, everything sounding is the snippet being
        // replaced, and it goes. Under a set the score's voices are in the
        // same breath and cutting them would tear a hole in the
        // performance. Generated voices carry an ownership token, so they
        // are replaced at the new preview's onset without cutting the set.
        if alone {
            if replacing && !generator && !self.worker.try_cut_sounding() {
                self.log.push(
                    LogLevel::Warn,
                    "preview",
                    "the engine was busy; the last snippet may ring on",
                );
            }
            // And it begins at its beginning, in the same breath the
            // pattern lands. A pattern keeps its place in the cycle
            // whenever it is installed, which is what keeps a live edit in
            // time; on its own that is the wrong rule, since the first
            // thing heard is whatever falls where the clock already
            // stands - silence, for a snippet whose first event is most of
            // a cycle away. A restart sent before the install cannot say
            // which install it is for: the producer keeps querying under
            // the old generation while the pattern is evaluated, and the
            // install then finds the cursor already past cycle zero - its
            // first cycles are skipped, and a one-hit pattern at the
            // downbeat never sounds at all. Whether a producer turn slips
            // in during the wait is a race, which is why some previews
            // play and some stay silent. A flag on the install itself is
            // atomic: the worker lowers the cursor to the fresh zero in
            // the same step the pattern takes, so the whole snippet is
            // queried whatever the engine is doing. Under a set the clock
            // is the set's and is not touched.
            self.next_rewind = !generator || !replacing || !playing;
        }
        self.queue_evaluation_for(scene, revision, source, false);
        self.status = if alone {
            "previewing the snippet".into()
        } else {
            "previewing the snippet under the score".into()
        };
        self.dirty_frame = true;
    }

    /// Where the previewed snippet has got to, for the shelf to say.
    ///
    /// A preview is not instant: the source goes to the engine, the launch
    /// waits for a cycle line, and the sounds it names may still be coming
    /// down. Each of those waits says the same plain word - `loading...` -
    /// because to the reader they are one wait: the key was pressed and
    /// the sound has not come yet. The count-in is the one wait that says
    /// something else, its bare number, because the music is close enough
    /// to count into. The flag means the source is installed and its
    /// literal sound references report ready; dynamic variants and pitch
    /// zones can still need other assets.
    #[cfg(feature = "hydra")]
    pub(super) fn snippet_preview_note(&self) -> Option<(String, bool)> {
        self.snippet_preview_note_at(Instant::now())
    }

    #[cfg(feature = "hydra")]
    pub(super) fn snippet_preview_note_at(&self, now: Instant) -> Option<(String, bool)> {
        let note = self.raw_snippet_preview_note();
        if note.as_ref().is_some_and(|(text, _)| text == "loading...")
            && self.generator_feedback_debouncing(now)
        {
            return None;
        }
        note
    }

    #[cfg(feature = "hydra")]
    fn generator_feedback_debouncing(&self, now: Instant) -> bool {
        self.snippet_preview
            .as_ref()
            .is_some_and(SnippetPreview::is_generator)
            && self.generator_feedback_since.is_some_and(|since| {
                now.saturating_duration_since(since) < Duration::from_millis(250)
            })
    }

    #[cfg(feature = "hydra")]
    fn raw_snippet_preview_note(&self) -> Option<(String, bool)> {
        let preview = self.snippet_preview.as_ref()?;
        if self.pending_evaluation.is_some() || !self.inflight.is_empty() {
            return Some(("loading...".to_owned(), false));
        }
        if self
            .snapshot
            .as_ref()
            .and_then(|snapshot| snapshot.launch.as_ref())
            .is_some()
        {
            return Some(("loading...".to_owned(), false));
        }
        // Installation starts the score while its assets can still be
        // arriving. Only this preview's sounds determine the caption:
        // a diagnostic from the previous score must not label the new one.
        if let Some(readiness) = &preview.readiness {
            if readiness.failed > 0 {
                let noun = if readiness.failed == 1 {
                    "sound"
                } else {
                    "sounds"
                };
                return Some((format!("{} {noun} failed to load", readiness.failed), false));
            }
            if readiness.loading > 0 {
                return Some(("loading...".to_owned(), false));
            }
        }
        // Installation plus the source estimate does not depend on marks:
        // highlights can be off, and the first event can be a cycle away.
        // The count knows on its own whose clock the snapshot carries, so
        // a cutover the layout has not announced yet still counts - on
        // the snapshot's revision, never the outgoing score's bar.
        if let Some(count) = self.preview_count_in() {
            return Some((count, false));
        }
        if self.installed_revision.as_deref() == Some(preview.revision.as_str()) {
            if preview
                .readiness
                .as_ref()
                .is_none_or(|readiness| readiness.unknown > 0)
            {
                return Some(("sounds unchecked".to_owned(), false));
            }
            // Sounding, there is nothing left to say in words: the
            // playhead strip beside the row carries where the bar stands,
            // and a word over it was room a narrow shelf wants back.
            return None;
        }
        None
    }

    /// The beats still to go before the preview's first downbeat, said as
    /// a bare number: `4`, then 3, 2, 1. `None` says there is no count to
    /// show.
    ///
    /// A preview is not heard the instant its install lands. Playing
    /// alone, the fresh cycle zero rides a continuity margin ahead of now.
    /// Under a score, the pattern joins wherever the cycle stands, so its
    /// first event can be most of a bar out. The count runs to the running
    /// clock's next cycle line. A mark landing ends it early, because the
    /// sound is audible. Past the line the preview is in the music, and
    /// its pattern is not a wait.
    ///
    /// The count is latched on the preview:
    ///
    /// ```text
    ///  other clock --(1)--> counting --(2)--> landed
    ///  returns None         "4" .. "1"        returns None
    /// ```
    ///
    /// (1) The first snapshot whose `source_revision` names this preview
    /// owns the clock. An earlier snapshot still carries the outgoing
    /// score's revision and cycle, and a count on it counts the wrong bar.
    /// The layout's `installed_revision` runs ahead of the cutover and is
    /// not trusted here.
    ///
    /// (2) The first counting look captures the next whole cycle as the
    /// target, and the count lands when the clock reaches it, between polls
    /// or on one. A target derived again on every look moves to the next
    /// line when the clock reaches the current one, so whenever the
    /// preview's first mark goes unmapped the count starts again. The
    /// latch dies with the preview, so the next one counts afresh.
    #[cfg(feature = "hydra")]
    pub(super) fn preview_count_in(&self) -> Option<String> {
        let preview = self.snippet_preview.as_ref()?;
        // Solo generator edits install immediately on their running clock;
        // that clock moving does not mean there is a score to count into.
        if preview.is_generator() && preview.restore.is_none() {
            return None;
        }
        if self.snippet_heard || preview.count_in_landed.get() {
            return None;
        }
        let snapshot = self.snapshot.as_ref()?;
        if !snapshot.playing || snapshot.stopping || !snapshot.cycle.is_finite() {
            return None;
        }
        if snapshot.source_revision.as_deref() != Some(preview.revision.as_str()) {
            // Not this preview's clock yet: the install has not cut over.
            // Nothing is said - the wait is still real, and counting the
            // outgoing score's bar would be counting to the wrong line.
            return None;
        }
        preview.clock_owned.set(true);
        // Already standing on the line (or a hair past it): a fresh
        // install's zero can land between two polls, and a count that
        // started at 4 for a downbeat already here reads as broken. The
        // epsilon is in cycles, the same order the beat math below uses.
        const ON_LINE: f64 = 0.0125;
        if snapshot.cycle >= -ON_LINE && snapshot.cycle.fract() <= ON_LINE {
            preview.count_in_landed.set(true);
            return None;
        }
        // The line this count is counting to: the next whole cycle. A
        // clock not yet at its anchor (a fresh start's preroll) sits
        // below zero, and `ceil` takes it to zero - the downbeat itself.
        let target = preview
            .count_in_target
            .get()
            .unwrap_or_else(|| snapshot.cycle.ceil());
        if snapshot.cycle >= target {
            // The line is here - possibly it passed between two polls.
            // What was counted stays counted: past it the preview is in
            // the music, and however the pattern falls from here is the
            // pattern's, not a wait.
            preview.count_in_landed.set(true);
            return None;
        }
        preview.count_in_target.set(Some(target));
        // Beats to the line is what remains of the cycle, times four. The
        // count is said as its bare number.
        let beats_left = (target - snapshot.cycle) * 4.0;
        Some((beats_left.ceil() as u64).to_string())
    }

    /// How far through its bar the previewed snippet's clock stands, as a
    /// fraction 0..1 - the playhead a strip of cells on the shelf row can
    /// ride. The clock is the snapshot's own, which is what the score and
    /// the preview both ride: wherever the strip stands is where the
    /// pattern is in the music, whether or not a sound has yet been heard
    /// from it. That is the point of it - "playing" beside silence could
    /// be anything; a strip already halfway across says the cycle is, and
    /// the ear can go looking for what it missed.
    ///
    /// `None` until the snapshot itself names this preview's revision -
    /// the layout's `installed_revision` runs ahead of the install's
    /// cutover, and a cycle read off the outgoing clock beside a strip
    /// saying otherwise would be a lie twice over - and while the
    /// transport is stopped or stopping.
    #[cfg(feature = "hydra")]
    pub(super) fn snippet_preview_progress(&self) -> Option<f32> {
        self.snippet_preview_progress_at(Instant::now())
    }

    #[cfg(feature = "hydra")]
    pub(super) fn snippet_preview_progress_at(&self, now: Instant) -> Option<f32> {
        // Keep the empty rail steady until edits settle, even if each new
        // revision becomes ready between key repeats. Audio is not delayed.
        if self.generator_feedback_debouncing(now) {
            return None;
        }
        let preview = self.snippet_preview.as_ref()?;
        let snapshot = self.snapshot.as_ref()?;
        if snapshot.source_revision.as_deref() != Some(preview.revision.as_str()) {
            return None;
        }
        if !snapshot.playing || snapshot.stopping || !snapshot.cycle.is_finite() {
            return None;
        }
        Some(snapshot.cycle.rem_euclid(1.0) as f32)
    }

    /// Take the snippet back out: the score alone again, or silence if that
    /// is what there was before it.
    #[cfg(feature = "hydra")]
    pub(super) fn stop_snippet_preview(&mut self) -> bool {
        self.generator_preview_pending = false;
        let Some(preview) = self.snippet_preview.take() else {
            return false;
        };
        match preview.restore {
            Some((score, revision)) => {
                self.queue_evaluation_for(preview.scene, revision, score, false);
                self.status = "preview stopped; the score plays on".into();
            }
            None => {
                self.stop_requested = true;
                self.worker.request_stop();
                self.status = "preview stopped".into();
            }
        }
        self.dirty_frame = true;
        true
    }

    /// Silence whatever the browser was auditioning, without a word about
    /// it.
    ///
    /// An update is the set speaking. A sample still ringing from the
    /// browser underneath it is a sound nobody asked for, and the status
    /// line belongs to the update rather than to the preview it ended.
    pub(super) fn silence_preview(&mut self) {
        if self.preview_armed.take().is_some() {
            let _ = self.worker.try_stop_audition();
        }
        self.preview_lit_until = None;
        self.preview_shape = None;
    }

    /// Silence the preview if one may still be sounding. Returns whether
    /// there was one to stop.
    pub(super) fn stop_preview(&mut self) -> bool {
        #[cfg(feature = "hydra")]
        let snippet = self.stop_snippet_preview();
        #[cfg(not(feature = "hydra"))]
        let snippet = false;
        let Some((_, started)) = self.preview_armed.take() else {
            return snippet;
        };
        self.preview_lit_until = None;
        self.preview_shape = None;
        // A preview that started long ago has ended on its own; nothing to
        // stop, and the key that asked keeps its ordinary meaning.
        if started.elapsed() > PREVIEW_STOP_WINDOW {
            return snippet;
        }
        if self.worker.try_stop_audition() {
            self.status = "preview stopped".into();
            self.dirty_frame = true;
        }
        true
    }
}

/// What separates the score from a snippet playing under it. A comment,
/// so the engine reads it as nothing at all.
#[cfg(feature = "hydra")]
pub(super) const SNIPPET_PREVIEW_BANNER: &str = "// preview\n";

/// The tempo a generated snippet names, in cycles a second.
///
/// A composed track opens with `// 140 bpm, dubstep`: the tempo it was
/// written for, said rather than set, so that taking it into a sounding
/// set cannot drag the set to it. Auditioned on its own there is no set
/// to respect, and the number in the comment is what the music was meant
/// to go at - drum and bass heard at 120 is not drum and bass.
///
/// Four beats to the cycle, the convention `setcpm(bpm/4)` assumes.
#[cfg(feature = "hydra")]
pub(super) fn named_tempo(code: &str) -> Option<f64> {
    let line = code.lines().next()?.trim_start().strip_prefix("//")?;
    let (number, rest) = line.trim_start().split_once(' ')?;
    if !rest.trim_start().starts_with("bpm") {
        return None;
    }
    let bpm = number.parse::<f64>().ok()?;
    (40.0..=300.0).contains(&bpm).then_some(bpm / 4.0 / 60.0)
}

/// A snippet ready to play, and where each of its lines came from.
///
/// The tempo lines come out: a preview plays along with the studio rather
/// than telling it what tempo to be. So what plays is not line-for-line
/// what the shelf shows, and the pairs are `(offset in the block, offset
/// in the snippet)`, one a line - which is what turns a mark coming back
/// from the engine into a highlight on the row the reader is looking at.
#[cfg(feature = "hydra")]
pub(super) fn preview_block(code: &str) -> (String, Vec<(usize, usize)>) {
    snippet_preview_block(code, None)
}

#[cfg(feature = "hydra")]
pub(super) fn generator_preview_block(code: &str, epoch: u64) -> (String, Vec<(usize, usize)>) {
    snippet_preview_block(code, Some(epoch))
}

#[cfg(feature = "hydra")]
fn snippet_preview_block(code: &str, epoch: Option<u64>) -> (String, Vec<(usize, usize)>) {
    let mut block = String::new();
    let mut lines = Vec::new();
    let mut at = 0;
    let ownership =
        epoch.map(|epoch| format!("  .fmap(v => ({{...v, __rustelPreview: {epoch}}}))\n"));
    let mut lane = false;
    for line in code.lines() {
        let start = at;
        at += line.len() + 1;
        let trimmed = line.trim_start();
        if trimmed.starts_with("setcps(")
            || trimmed.starts_with("setcpm(")
            || trimmed.starts_with("cps(")
        {
            continue;
        }
        if trimmed.starts_with("$:") {
            if lane && let Some(tag) = &ownership {
                block.push_str(tag);
            }
            lane = true;
        }
        lines.push((block.len(), start));
        block.push_str(line);
        block.push('\n');
    }
    if lane && let Some(tag) = &ownership {
        block.push_str(tag);
    }
    while block.ends_with('\n') {
        block.pop();
    }
    (block, lines)
}

/// A score's own text, labelled so a snippet can be previewed under it.
///
/// Block evaluation plays labelled blocks: the moment a source carries one
/// label, a bare expression elsewhere is never played (upstream's rule, and
/// the runtime's). A preview installs the score and the snippet as one
/// source, and an example is always labelled: the catalogue hands it over
/// as `$:` lines. An unlabelled score would therefore fall silent the
/// moment anything is auditioned over it. Scores written as lanes are
/// already playable blocks and are left exactly as they are; a score
/// without one has its own block labelled instead.
#[cfg(feature = "hydra")]
pub(super) fn labelled_score_text(text: &str) -> String {
    if has_label_statement(text) {
        return text.to_owned();
    }
    let Some(index) = playable_statement_start(text) else {
        return text.to_owned();
    };
    let mut labelled = String::with_capacity(text.len() + LABELLED_SCORE_PREFIX.len());
    labelled.push_str(&text[..index]);
    labelled.push_str(LABELLED_SCORE_PREFIX);
    labelled.push_str(&text[index..]);
    labelled
}

/// The label a bare score's block is given for the preview composite.
///
/// Named, not `$:`: the preview may sit under a set that already uses the
/// anonymous form, and two blocks cannot share one key.
#[cfg(feature = "hydra")]
pub(super) const LABELLED_SCORE_PREFIX: &str = "score: ";

/// Whether any line opens a block label at the top level.
///
/// A label is an identifier and a colon before anything else on its line,
/// which is what keeps `s("bd:4")` - a colon inside a call - out of it.
#[cfg(feature = "hydra")]
fn has_label_statement(text: &str) -> bool {
    let mut depth = 0i32;
    for line in text.lines() {
        let trimmed = line.trim_start();
        if depth == 0 && !trimmed.starts_with("//") {
            let ident = trimmed
                .find(|c: char| !(c.is_alphanumeric() || c == '_' || c == '$'))
                .unwrap_or(trimmed.len());
            if ident > 0 && trimmed[ident..].trim_start().starts_with(':') {
                return true;
            }
        }
        depth += line_bracket_delta(line);
    }
    false
}

/// The byte offset of the last top-level statement that starts a pattern.
///
/// Scanning from the end finds the expression a bare score ends on: the
/// chain's own first line, never its `.method()` continuations, and never a
/// declaration, which returns nothing to play.
#[cfg(feature = "hydra")]
fn playable_statement_start(text: &str) -> Option<usize> {
    let mut depth = 0i32;
    let mut at = 0;
    let mut candidate = None;
    for line in text.split_inclusive('\n') {
        let trimmed = line.trim_start();
        if depth == 0
            && !trimmed.starts_with("//")
            && !trimmed.is_empty()
            && !line.starts_with([' ', '\t'])
        {
            let head_end = trimmed
                .find(|c: char| !c.is_ascii_alphanumeric())
                .unwrap_or(trimmed.len());
            let head = &trimmed[..head_end];
            if !matches!(
                head,
                "const"
                    | "let"
                    | "var"
                    | "function"
                    | "class"
                    | "import"
                    | "export"
                    | "return"
                    | "throw"
                    | "}"
            ) {
                candidate = Some(at);
            }
        }
        depth += line_bracket_delta(line);
        at += line.len();
    }
    candidate
}

/// Brackets opened minus brackets closed on one line, strings and line
/// comments skipped so a colon or brace inside mini notation cannot tip the
/// nesting over.
#[cfg(feature = "hydra")]
fn line_bracket_delta(line: &str) -> i32 {
    let mut delta = 0;
    let mut in_string: Option<char> = None;
    let mut chars = line.char_indices().peekable();
    while let Some((index, c)) = chars.next() {
        match in_string {
            Some(quote) => {
                if c == '\\' {
                    chars.next();
                } else if c == quote {
                    in_string = None;
                }
            }
            None => match c {
                '"' | '\'' | '`' => in_string = Some(c),
                '/' if line[index..].starts_with("//") => break,
                '(' | '[' | '{' => delta += 1,
                ')' | ']' | '}' => delta -= 1,
                _ => {}
            },
        }
    }
    delta
}

#[cfg(test)]
mod tests {
    #[cfg(feature = "hydra")]
    mod generator {
        //! Tests for generator.

        use crate::app::previews::{SNIPPET_PREVIEW_BANNER, generator_preview_block};
        use crate::app::*;
        use crate::ideas::{Direction, Generator, Row};

        fn open_generator(app: &mut App) {
            app.toggle_reference_browse();
            let panel = app.reference_panel.as_mut().unwrap();
            panel.select_tab(Tab::Generator);
            panel.generator = Generator::seeded(42);
            panel.generator.set(1, 50);
            panel.snippet_selected = panel
                .generator
                .rows()
                .iter()
                .position(|row| *row == Row::Control(1))
                .unwrap(); // Tone
            app.focus_panel(PanelKind::Reference);
        }

        fn sent(app: &App) -> &PendingEvaluation {
            app.pending_evaluation
                .as_ref()
                .or_else(|| app.inflight.values().last())
                .expect("a queued preview")
        }

        #[test]
        fn stopped_generator_previews_immediately_at_last_tempo() {
            let directory = tempfile::tempdir().unwrap();
            let mut app = crate::app::tests::support::app_over(directory.path());
            open_generator(&mut app);
            app.snapshot = Some(StudioSnapshot {
                cps: 0.75,
                ..Default::default()
            });
            app.queue_generator_preview();
            assert_eq!(sent(&app).launch, Launch::Now);
            assert!(sent(&app).rewind);
            assert!(sent(&app).source.starts_with("setcps(0.750000)"));
            assert_eq!(
                app.snippet_preview.as_ref().unwrap().row,
                Some(crate::reference::SnippetLine::Generator(Row::Generate(
                    Direction::Glass
                )))
            );
        }

        #[test]
        fn generator_under_a_score_waits_for_the_next_cycle_without_rewinding() {
            let directory = tempfile::tempdir().unwrap();
            let mut app = crate::app::tests::support::app_over(directory.path());
            open_generator(&mut app);
            let score: Arc<str> = Arc::from("$: s(\"bd*4\")");
            app.snapshot = Some(StudioSnapshot {
                playing: true,
                cps: 0.7,
                ..Default::default()
            });
            app.evaluated_source = Some(score.clone());
            app.audible_scene = Some(app.scenes.current().id);
            app.queue_generator_preview();
            assert_eq!(sent(&app).launch, Launch::Quantised { unit_cycles: 1.0 });
            assert!(!sent(&app).rewind);
            assert!(sent(&app).source.contains("bd*4"));
            assert!(!sent(&app).source.contains("setcps("));
            assert_eq!(
                app.snippet_preview
                    .as_ref()
                    .unwrap()
                    .restore
                    .as_ref()
                    .unwrap()
                    .0,
                score
            );
            install_generator_preview(&mut app, 0.25);
            assert_eq!(app.preview_count_in(), Some("3".into()));
            assert_eq!(app.snippet_preview_note(), Some(("3".into(), false)));
        }

        fn install_generator_preview(app: &mut App, cycle: f64) {
            app.pending_evaluation = None;
            app.inflight.clear();
            let preview = app.snippet_preview.as_mut().unwrap();
            preview.readiness = Some(Readiness::default());
            app.installed_revision = Some(preview.revision.clone());
            app.evaluated_source = Some(preview.source.clone());
            app.snapshot = Some(StudioSnapshot {
                playing: true,
                cps: 0.5,
                cycle,
                source_revision: Some(preview.revision.clone()),
                ..Default::default()
            });
        }

        #[test]
        fn generator_alone_never_counts_into_its_own_running_cycle() {
            let directory = tempfile::tempdir().unwrap();
            let mut app = crate::app::tests::support::app_over(directory.path());
            open_generator(&mut app);
            for (index, cycle) in [0.25, 1.62, 12.5].into_iter().enumerate() {
                app.reference_panel.as_mut().unwrap().generator.fresh();
                app.generator_last_preview = None;
                app.queue_generator_preview();
                assert_eq!(sent(&app).launch, Launch::Now);
                assert_eq!(sent(&app).rewind, index == 0);
                assert!(app.snippet_preview.as_ref().unwrap().restore.is_none());
                install_generator_preview(&mut app, cycle);
                // A sparse pattern or disabled highlights need not report a mark.
                assert!(!app.snippet_heard);
                assert!(app.preview_count_in().is_none());
                let settled = app.generator_feedback_since.unwrap() + Duration::from_millis(250);
                assert!(app.snippet_preview_note_at(settled).is_none());
                assert_eq!(
                    app.snippet_preview_progress_at(settled),
                    Some(cycle.rem_euclid(1.0) as f32)
                );
            }
        }

        #[test]
        fn holding_an_arrow_updates_code_and_coalesces_audio_without_restarting_the_loop() {
            let directory = tempfile::tempdir().unwrap();
            let mut app = crate::app::tests::support::app_over(directory.path());
            open_generator(&mut app);
            app.queue_generator_preview();
            app.snapshot = Some(StudioSnapshot {
                playing: true,
                cps: 0.5,
                ..Default::default()
            });
            app.pending_evaluation = None;
            app.inflight.clear();
            let initial = app.reference_panel.as_ref().unwrap().generator.code.clone();
            let before = app.next_request_id;
            // Pin the throttle deadline ahead of this input burst to make timing deterministic.
            app.generator_last_preview = Some(Instant::now() + Duration::from_secs(1));
            for _ in 0..5 {
                app.handle_terminal_event(Event::Key(KeyEvent::new_with_kind(
                    KeyCode::Right,
                    KeyModifiers::NONE,
                    KeyEventKind::Repeat,
                )))
                .unwrap();
            }
            let panel = app.reference_panel.as_ref().unwrap();
            assert_eq!(panel.generator.controls[1], 55);
            assert_ne!(panel.generator.code, initial);
            assert_eq!(
                app.next_request_id, before,
                "repeat should coalesce evaluations"
            );
            assert!(app.generator_preview_pending);
            app.flush_generator_preview(Instant::now() + Duration::from_secs(2));
            assert_eq!(app.next_request_id, before + 1);
            assert_eq!(
                sent(&app).launch,
                Launch::Now,
                "an audition alone needs no cycle wait"
            );
            assert!(
                !sent(&app).rewind,
                "a held control must not repeatedly restart the phrase"
            );
            assert!(!app.generator_preview_pending);
        }

        #[test]
        fn stop_cancels_the_deferred_generator_update() {
            let directory = tempfile::tempdir().unwrap();
            let mut app = crate::app::tests::support::app_over(directory.path());
            open_generator(&mut app);
            app.generator_last_preview = Some(Instant::now() + Duration::from_secs(1));
            app.queue_generator_preview();
            assert!(app.generator_preview_pending);
            app.handle_editor_effect(EditorEffect::Stop).unwrap();
            let before = app.next_request_id;
            app.flush_generator_preview(Instant::now() + Duration::from_secs(2));
            assert_eq!(app.next_request_id, before);
            assert!(!app.generator_preview_pending);
        }

        #[test]
        fn generator_mouse_drag_uses_the_visible_rail_and_reaches_both_ends() {
            let directory = tempfile::tempdir().unwrap();
            let mut app = crate::app::tests::support::app_over(directory.path());
            open_generator(&mut app);
            app.frame = Rect::new(0, 0, 120, 40);
            app.regions = view::regions(app.frame, false, 1, true, true, None, None);
            let inner = inner_area(app.regions.reference);
            let panel = app.reference_panel.as_ref().unwrap();
            let geometry = panel.geometry(inner);
            let row = panel
                .generator
                .rows()
                .iter()
                .position(|row| *row == Row::Control(1))
                .unwrap();
            let y = geometry.list.y + (row - geometry.first_row) as u16;
            let rail = crate::reference::generator_rail(Rect::new(
                geometry.list.x,
                y,
                geometry.list.width,
                1,
            ));
            assert!(rail.width >= 3);
            assert_eq!(
                panel.generator_control_at(inner, rail.x, y),
                Some((1, rail))
            );
            app.click_reference(
                MouseEvent {
                    kind: MouseEventKind::Down(MouseButton::Left),
                    column: rail.x,
                    row: y,
                    modifiers: KeyModifiers::NONE,
                },
                rail.x,
                y,
            )
            .unwrap();
            assert_eq!(
                app.pointer,
                Some(Pointer::GeneratorControl { index: 1, rail })
            );
            assert_eq!(
                app.reference_panel.as_ref().unwrap().generator.controls[1],
                0
            );
            app.handle_terminal_event(Event::Mouse(MouseEvent {
                kind: MouseEventKind::Drag(MouseButton::Left),
                column: rail.right() - 1,
                row: y + 2,
                modifiers: KeyModifiers::NONE,
            }))
            .unwrap();
            assert_eq!(
                app.reference_panel.as_ref().unwrap().generator.controls[1],
                100
            );
        }

        #[test]
        fn switching_tabs_preserves_the_examples_tree_and_selection() {
            let reference = crate::reference::Reference::load_all();
            let mut panel = crate::reference::ReferencePanel::browse(&reference);
            panel.select_tab(Tab::Examples);
            panel.snippet_open.insert((0, 0));
            panel.snippet_selected = 2;
            let example = panel.selected_snippet_code().unwrap().into_owned();
            panel.select_tab(Tab::Generator);
            panel.snippet_selected = 5;
            panel.generator.fresh();
            panel.select_tab(Tab::Examples);
            assert_eq!(panel.snippet_selected, 2);
            assert_eq!(panel.selected_snippet_code().unwrap(), example);
            panel.select_tab(Tab::Generator);
            assert_eq!(panel.snippet_selected, 5);
        }

        #[test]
        fn generator_code_copies_in_both_formats_without_editing_the_score() {
            let directory = tempfile::tempdir().unwrap();
            let mut app = crate::app::tests::support::app_over(directory.path());
            open_generator(&mut app);
            let before = app.editor().source();
            for _ in 0..2 {
                let shown = app
                    .reference_panel
                    .as_ref()
                    .unwrap()
                    .selected_snippet_code()
                    .unwrap()
                    .into_owned();
                app.handle_reference_key(KeyCode::Char('c'), false, false)
                    .unwrap();
                assert_eq!(app.clipboard.get_text().unwrap(), shown);
                app.handle_reference_key(KeyCode::Char('s'), false, false)
                    .unwrap();
            }
            assert_eq!(app.editor().source(), before);
        }

        #[test]
        fn closing_and_reopening_the_browser_retains_each_directions_last_result() {
            let directory = tempfile::tempdir().unwrap();
            let mut app = crate::app::tests::support::app_over(directory.path());
            open_generator(&mut app);
            let glass = app.reference_panel.as_ref().unwrap().generator.clone();
            let panel = app.reference_panel.as_mut().unwrap();
            panel.generator.choose(Direction::Acid);
            panel.generator.fresh();
            panel.generator.set(2, 81);
            panel.snippet_selected = 6;
            let acid = panel.generator.code.clone();
            let controls = panel.generator.controls;
            app.toggle_reference_browse();
            assert!(app.reference_panel.is_none());
            app.toggle_reference_browse();
            let panel = app.reference_panel.as_mut().unwrap();
            panel.select_tab(Tab::Generator);
            assert_eq!(panel.snippet_selected, 6);
            assert_eq!(panel.generator.code, acid);
            assert_eq!(panel.generator.controls, controls);
            panel.generator.choose(Direction::Glass);
            assert_eq!(panel.generator.code, glass.code);
            assert_eq!(panel.generator.controls, glass.controls);
            panel.collapse();
            panel.generator_activate();
            assert!(panel.generator.open);
            assert_eq!(panel.generator.code, glass.code);
        }

        #[test]
        fn generate_copy_similar_and_space_use_the_examples_actions() {
            let directory = tempfile::tempdir().unwrap();
            let mut app = crate::app::tests::support::app_over(directory.path());
            open_generator(&mut app);
            let initial = app.reference_panel.as_ref().unwrap().generator.clone();
            app.reference_panel.as_mut().unwrap().snippet_selected = 1; // Generate
            app.handle_reference_key(KeyCode::Enter, false, false)
                .unwrap();
            let generated = app.reference_panel.as_ref().unwrap().generator.clone();
            assert_ne!(initial.controls, generated.controls);
            assert_ne!(initial.code, generated.code);
            assert!(app.snippet_preview.is_some());
            app.reference_panel.as_mut().unwrap().snippet_selected = 3; // Copy
            let shown = app
                .reference_panel
                .as_ref()
                .unwrap()
                .selected_snippet_code()
                .unwrap()
                .into_owned();
            app.handle_reference_key(KeyCode::Enter, false, false)
                .unwrap();
            assert_eq!(app.clipboard.get_text().unwrap(), shown);
            assert_eq!(app.reference_panel.as_ref().unwrap().generator, generated);
            app.handle_reference_key(KeyCode::Char(' '), false, false)
                .unwrap();
            assert!(
                app.snippet_preview.is_none(),
                "Space stops the currently shown idea"
            );
            app.handle_reference_key(KeyCode::Char(' '), false, false)
                .unwrap();
            assert!(app.snippet_preview.is_some(), "Space plays it again");
            app.handle_reference_key(KeyCode::Char('v'), false, false)
                .unwrap();
            let variation = &app.reference_panel.as_ref().unwrap().generator;
            assert_ne!(variation.code, generated.code);
            assert_eq!(variation.controls, generated.controls);
        }

        #[test]
        fn preview_tagging_covers_every_lane_and_preserves_source_locations() {
            for direction in Direction::ALL {
                let mut generator = Generator::seeded(2);
                if direction != generator.direction() {
                    generator.choose(direction);
                }
                for code in [
                    crate::examples::stacked(&generator.code),
                    crate::examples::laned(&generator.code),
                ] {
                    let (block, lines) = generator_preview_block(&code, 42);
                    assert_eq!(
                        block.matches("__rustelPreview").count(),
                        code.matches("$:").count()
                    );
                    for (target, source) in lines {
                        assert_eq!(
                            block[target..].lines().next(),
                            code[source..].lines().next()
                        );
                    }
                    let mut session = rustel_runtime::Session::new().unwrap();
                    session.evaluate(&block).unwrap();
                    let events = session
                        .query(
                            rustel_fraction::Fraction::from(0),
                            rustel_fraction::Fraction::from(2),
                        )
                        .unwrap();
                    assert!(!events.is_empty());
                    for event in events {
                        assert_eq!(
                            event.value.get("__rustelPreview").cloned(),
                            Some(rustel_core::Value::F64(42.0))
                        );
                    }
                }
            }
        }

        #[test]
        fn new_generated_previews_replace_ownership_without_accumulating_layers() {
            let directory = tempfile::tempdir().unwrap();
            let mut app = crate::app::tests::support::app_over(directory.path());
            open_generator(&mut app);
            app.snapshot = Some(StudioSnapshot {
                playing: true,
                ..Default::default()
            });
            let score: Arc<str> = Arc::from("$: s(\"sine\").release(8)");
            app.evaluated_source = Some(score.clone());
            app.queue_generator_preview();
            let first_epoch = app.generator_preview_epoch;
            app.reference_panel.as_mut().unwrap().generator.fresh();
            app.generator_last_preview = None;
            app.queue_generator_preview();
            assert_eq!(app.generator_preview_epoch, first_epoch + 1);
            let source = &sent(&app).source;
            assert_eq!(source.matches(score.as_ref()).count(), 1);
            assert_eq!(source.matches(SNIPPET_PREVIEW_BANNER).count(), 1);
            assert!(
                !source
                    .split(SNIPPET_PREVIEW_BANNER)
                    .next()
                    .unwrap()
                    .contains("__rustelPreview")
            );
            assert_eq!(sent(&app).launch, Launch::Quantised { unit_cycles: 1.0 });
            assert!(!sent(&app).rewind);
        }

        #[test]
        fn generator_code_pane_is_thirty_percent_taller_without_changing_examples() {
            let reference = crate::reference::Reference::load_all();
            let mut panel = crate::reference::ReferencePanel::browse(&reference);
            for height in [20, 30, 40, 60] {
                let inner = Rect::new(1, 1, 60, height);
                panel.select_tab(Tab::Examples);
                let examples = panel.snippet_layout(inner);
                assert_eq!(
                    examples.code,
                    crate::reference::snippet_layout(inner, false).code
                );
                panel.select_tab(Tab::Generator);
                let generator = panel.snippet_layout(inner);
                assert!(generator.code.height * 10 >= examples.code.height * 13);
                assert_eq!(generator.scope + 1, generator.footer);
                assert!(generator.list.bottom() <= generator.code.y);
                assert!(generator.code.bottom() <= generator.scope);
            }
        }

        #[test]
        fn generator_loading_feedback_waits_for_a_real_delay_and_resets_for_new_edits() {
            let directory = tempfile::tempdir().unwrap();
            let mut app = crate::app::tests::support::app_over(directory.path());
            open_generator(&mut app);
            app.queue_generator_preview();
            let start = Instant::now();
            app.generator_feedback_since = Some(start);
            assert!(
                app.snippet_preview_note_at(start + Duration::from_millis(249))
                    .is_none()
            );
            assert_eq!(
                app.snippet_preview_note_at(start + Duration::from_millis(250)),
                Some(("loading...".into(), false))
            );
            for _ in 0..8 {
                app.reference_panel.as_mut().unwrap().generator.fresh();
                app.generator_last_preview = None;
                app.queue_generator_preview();
                let since = app.generator_feedback_since.unwrap();
                assert!(
                    app.snippet_preview_note_at(since + Duration::from_millis(40))
                        .is_none()
                );
            }
            app.pending_evaluation = None;
            app.inflight.clear();
            app.snippet_preview.as_mut().unwrap().readiness = Some(Readiness::default());
            assert!(app.snippet_preview_note_at(start).is_none());
            assert!(app.generator_feedback_since.is_some());
        }

        #[test]
        fn generator_progress_waits_for_keyrepeat_to_settle_even_when_each_edit_loads_fast() {
            let directory = tempfile::tempdir().unwrap();
            let mut app = crate::app::tests::support::app_over(directory.path());
            open_generator(&mut app);
            app.queue_generator_preview();
            install_generator_preview(&mut app, 0.5);
            let settled = app.generator_feedback_since.unwrap() + Duration::from_millis(250);
            assert_eq!(app.snippet_preview_progress_at(settled), Some(0.5));

            for _ in 0..8 {
                // A repeat must debounce even before its coalesced audio update is
                // sent, while the snapshot still names the previous ready revision.
                app.generator_last_preview = Some(Instant::now() + Duration::from_secs(1));
                app.handle_terminal_event(Event::Key(KeyEvent::new_with_kind(
                    KeyCode::Right,
                    KeyModifiers::NONE,
                    KeyEventKind::Repeat,
                )))
                .unwrap();
                assert!(app.generator_preview_pending);
                let edited = app.generator_feedback_since.unwrap();
                assert!(app.snippet_preview_progress_at(edited).is_none());
                app.flush_generator_preview(Instant::now() + Duration::from_secs(2));
                assert_eq!(sent(&app).launch, Launch::Now);
                assert!(!sent(&app).rewind);
                let since = app.generator_feedback_since.unwrap();
                assert!(app.snippet_preview_note_at(since).is_none());
                install_generator_preview(&mut app, 0.625);
                // Ready before the debounce expires: clearing the loading caption
                // must not clear the progress debounce and flash a filled rail.
                assert!(
                    app.snippet_preview_note_at(since + Duration::from_millis(40))
                        .is_none()
                );
                assert!(
                    app.snippet_preview_progress_at(since + Duration::from_millis(249))
                        .is_none()
                );
                assert_eq!(app.generator_feedback_since, Some(since));
            }
            let since = app.generator_feedback_since.unwrap();
            assert_eq!(
                app.snippet_preview_progress_at(since + Duration::from_millis(250)),
                Some(0.625)
            );
            app.snippet_preview
                .as_mut()
                .unwrap()
                .readiness
                .as_mut()
                .unwrap()
                .failed = 1;
            assert_eq!(
                app.snippet_preview_note_at(since),
                Some(("1 sound failed to load".into(), false)),
                "errors are never debounced"
            );
        }

        #[test]
        fn generator_shortcuts_keep_the_selected_control_or_direction() {
            for (open, row) in [
                (true, Row::Control(1)),
                (true, Row::Copy),
                (false, Row::Direction(Direction::ALL[1])),
            ] {
                let directory = tempfile::tempdir().unwrap();
                let mut app = crate::app::tests::support::app_over(directory.path());
                open_generator(&mut app);
                let panel = app.reference_panel.as_mut().unwrap();
                panel.generator.open = open;
                panel.snippet_selected = panel
                    .generator
                    .rows()
                    .iter()
                    .position(|r| *r == row)
                    .unwrap();
                let score = app.editor().source();
                for shortcut in ['g', 'v', 'G', 'V'] {
                    let before = app.reference_panel.as_ref().unwrap().generator.code.clone();
                    crate::app::tests::support::press(
                        &mut app,
                        KeyCode::Char(shortcut),
                        KeyModifiers::NONE,
                    );
                    let panel = app.reference_panel.as_ref().unwrap();
                    assert_eq!(panel.generator_row(), Some(row), "{shortcut}");
                    assert_ne!(panel.generator.code, before, "{shortcut} still generates");
                    assert!(panel.generator.open);
                }
                if let Row::Control(index) = row {
                    let value = app.reference_panel.as_ref().unwrap().generator.controls[index];
                    crate::app::tests::support::press(&mut app, KeyCode::Right, KeyModifiers::NONE);
                    assert_eq!(
                        app.reference_panel.as_ref().unwrap().generator.controls[index],
                        (value + 1).min(100),
                        "arrows still adjust the selected slider"
                    );
                }
                assert_eq!(app.editor().source(), score);
            }
        }

        #[test]
        fn generate_and_similar_shortcuts_and_arrows_browse_separate_histories() {
            let directory = tempfile::tempdir().unwrap();
            let mut app = crate::app::tests::support::app_over(directory.path());
            open_generator(&mut app);
            app.reference_panel.as_mut().unwrap().snippet_selected = 1;
            app.handle_reference_key(KeyCode::Left, false, false)
                .unwrap();
            assert!(
                app.reference_panel.as_ref().unwrap().generator.open,
                "Left at the history boundary must not collapse"
            );
            let first = app.reference_panel.as_ref().unwrap().generator.code.clone();
            app.handle_reference_key(KeyCode::Char('g'), false, false)
                .unwrap();
            let generated = app.reference_panel.as_ref().unwrap().generator.code.clone();
            assert_ne!(first, generated);
            app.handle_reference_key(KeyCode::Left, false, false)
                .unwrap();
            assert_eq!(app.reference_panel.as_ref().unwrap().generator.code, first);
            app.handle_reference_key(KeyCode::Right, false, false)
                .unwrap();
            assert_eq!(
                app.reference_panel.as_ref().unwrap().generator.code,
                generated
            );
            app.handle_reference_key(KeyCode::Char('v'), false, false)
                .unwrap();
            let variant = app.reference_panel.as_ref().unwrap().generator.code.clone();
            assert_ne!(variant, generated);
            assert_eq!(app.reference_panel.as_ref().unwrap().snippet_selected, 1);
            app.reference_panel.as_mut().unwrap().snippet_selected = 2;
            app.handle_reference_key(KeyCode::Left, false, false)
                .unwrap();
            assert_eq!(
                app.reference_panel.as_ref().unwrap().generator.code,
                generated
            );
            app.handle_reference_key(KeyCode::Right, false, false)
                .unwrap();
            assert_eq!(
                app.reference_panel.as_ref().unwrap().generator.code,
                variant
            );
            app.handle_reference_key(KeyCode::Char('c'), false, false)
                .unwrap();
            assert_eq!(
                app.clipboard.get_text().unwrap(),
                app.reference_panel
                    .as_ref()
                    .unwrap()
                    .selected_snippet_code()
                    .unwrap()
            );
            app.frame = Rect::new(0, 0, 120, 40);
            app.regions = view::regions(app.frame, false, 1, true, true, None, None);
            let inner = inner_area(app.regions.reference);
            let geometry = app.reference_panel.as_ref().unwrap().geometry(inner);
            let x = geometry.list.x + 2;
            let y = geometry.list.y + 2 - geometry.first_row as u16;
            app.click_reference(
                MouseEvent {
                    kind: MouseEventKind::Down(MouseButton::Left),
                    column: x,
                    row: y,
                    modifiers: KeyModifiers::NONE,
                },
                x,
                y,
            )
            .unwrap();
            assert_eq!(
                app.reference_panel.as_ref().unwrap().generator.code,
                generated
            );
        }

        #[test]
        fn generator_forward_arrows_explore_at_the_end_and_enter_always_creates() {
            use crate::ideas::Action;
            for (selected, action) in [(1, Action::Generate), (2, Action::Similar)] {
                let directory = tempfile::tempdir().unwrap();
                let mut app = crate::app::tests::support::app_over(directory.path());
                open_generator(&mut app);
                app.reference_panel.as_mut().unwrap().snippet_selected = selected;
                let score = app.editor().source().to_owned();
                let first = app.reference_panel.as_ref().unwrap().generator.code.clone();
                let first_controls = app.reference_panel.as_ref().unwrap().generator.controls;
                app.handle_reference_key(KeyCode::Right, false, false)
                    .unwrap();
                let next = app.reference_panel.as_ref().unwrap().generator.code.clone();
                assert_ne!(next, first, "Right at the end must explore {action:?}");
                app.handle_reference_key(KeyCode::Left, false, false)
                    .unwrap();
                let panel = app.reference_panel.as_ref().unwrap();
                assert_eq!(panel.generator.code, first);
                assert_eq!(panel.generator.controls, first_controls);
                app.handle_reference_key(KeyCode::Right, false, false)
                    .unwrap();
                assert_eq!(app.reference_panel.as_ref().unwrap().generator.code, next);
                app.handle_reference_key(KeyCode::Enter, false, false)
                    .unwrap();
                let entered = app.reference_panel.as_ref().unwrap().generator.code.clone();
                assert_ne!(entered, next, "Enter must create a new {action:?} result");
                app.handle_reference_key(KeyCode::Left, false, false)
                    .unwrap();
                assert_eq!(app.reference_panel.as_ref().unwrap().generator.code, next);
                app.handle_reference_key(KeyCode::Right, false, false)
                    .unwrap();
                assert_eq!(
                    app.reference_panel.as_ref().unwrap().generator.code,
                    entered
                );

                app.frame = Rect::new(0, 0, 120, 40);
                app.regions = view::regions(app.frame, false, 1, true, true, None, None);
                let inner = inner_area(app.regions.reference);
                let panel = app.reference_panel.as_ref().unwrap();
                let geometry = panel.geometry(inner);
                let y = geometry.list.y + selected as u16 - geometry.first_row as u16;
                let x = (geometry.list.x..geometry.list.right())
                    .find(|&x| {
                        panel.generator_history_at(inner, x, y) == Some((action, true, selected))
                    })
                    .expect("the forward action is clickable even at the history end");
                app.click_reference(
                    MouseEvent {
                        kind: MouseEventKind::Down(MouseButton::Left),
                        column: x,
                        row: y,
                        modifiers: KeyModifiers::NONE,
                    },
                    x,
                    y,
                )
                .unwrap();
                assert_ne!(
                    app.reference_panel.as_ref().unwrap().generator.code,
                    entered
                );
                assert_eq!(app.editor().source(), score);
                assert!(app.reference_panel.as_ref().unwrap().generator.open);
            }
        }

        #[test]
        fn generator_code_scrolls_by_keyboard_wheel_and_thumb_without_moving_the_tree() {
            let directory = tempfile::tempdir().unwrap();
            let mut app = crate::app::tests::support::app_over(directory.path());
            open_generator(&mut app);
            app.frame = Rect::new(0, 0, 120, 40);
            app.regions = view::regions(app.frame, false, 1, true, true, None, None);
            let inner = inner_area(app.regions.reference);
            let selected = app.reference_panel.as_ref().unwrap().snippet_selected;
            let score = app.editor().source();
            app.handle_reference_key(KeyCode::PageDown, false, false)
                .unwrap();
            let panel = app.reference_panel.as_ref().unwrap();
            let down = panel.snippet_code_scroll.get();
            assert!(down > 0);
            let rows = panel.snippet_layout(inner).code_rows();
            assert!(
                panel
                    .snippet_code_lines(rows.height as usize, rows.width as usize)
                    .iter()
                    .all(|line| unicode_width::UnicodeWidthStr::width(line.as_str())
                        <= usize::from(rows.width))
            );
            assert!(app.scroll_reference(rows.x, rows.y, 1.0));
            assert!(
                app.reference_panel
                    .as_ref()
                    .unwrap()
                    .snippet_code_scroll
                    .get()
                    < down
            );
            app.handle_reference_key(KeyCode::PageUp, false, false)
                .unwrap();
            let (rail, _, length, last) = app
                .reference_panel
                .as_ref()
                .unwrap()
                .snippet_scrollbar(inner)
                .unwrap();
            assert_eq!(
                app.reference_panel
                    .as_ref()
                    .unwrap()
                    .snippet_code_scroll
                    .get(),
                0
            );
            let press = MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: rail.x,
                row: rail.y,
                modifiers: KeyModifiers::NONE,
            };
            app.click_reference(press, rail.x, rail.y).unwrap();
            app.handle_terminal_event(Event::Mouse(MouseEvent {
                kind: MouseEventKind::Drag(MouseButton::Left),
                row: rail.bottom(),
                ..press
            }))
            .unwrap();
            let panel = app.reference_panel.as_ref().unwrap();
            assert_eq!(panel.snippet_code_scroll.get(), last);
            let (_, top, _, _) = panel.snippet_scrollbar(inner).unwrap();
            assert_eq!(
                top + length,
                rail.height,
                "at the text end the thumb reaches the rail end"
            );
            assert_eq!(panel.snippet_selected, selected);
            assert_eq!(app.editor().source(), score);
        }

        /// The snippet code's scrollbar owns its drag and its release. The score's
        /// editor never saw the press, so it must see neither: a score drag whose
        /// release was never reported (let go outside the window) would otherwise
        /// be pulled across to the reference column, moving the score's selection
        /// and scrolling the score sideways under a drag that was only ever the
        /// scrollbar's.
        #[test]
        fn the_snippet_scrollbar_drag_never_reaches_the_score() {
            let directory = tempfile::tempdir().unwrap();
            let mut app = crate::app::tests::support::app_over(directory.path());
            crate::app::tests::support::type_text(
                &mut app,
                "$: s(\"bd hh sd hh bd hh sd hh bd hh sd hh bd hh\")",
            );
            app.editor_mut().set_wrap(false);
            open_generator(&mut app);
            let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(120, 40))
                .expect("test terminal");
            let mut draw = |app: &mut App| {
                terminal
                    .draw(|frame| app.paint_into(frame, Instant::now()).expect("frame paints"))
                    .expect("draw");
            };
            draw(&mut app);

            // A drag in the score whose release never arrives.
            let score = app.focused_map().expect("the score is drawn").area();
            let press = MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: score.x + 2,
                row: score.y,
                modifiers: KeyModifiers::NONE,
            };
            app.handle_terminal_event(Event::Mouse(press)).unwrap();
            app.handle_terminal_event(Event::Mouse(MouseEvent {
                kind: MouseEventKind::Drag(MouseButton::Left),
                column: score.x + 6,
                ..press
            }))
            .unwrap();
            draw(&mut app);
            let selection = app.editor().primary_selection();
            let viewport = app.editor().viewport();

            let inner = inner_area(app.regions.reference);
            let (rail, _, _, last) = app
                .reference_panel
                .as_ref()
                .unwrap()
                .snippet_scrollbar(inner)
                .expect("the snippet code overflows its rows");
            assert!(
                rail.x >= score.x + score.width,
                "the scrollbar is right of the score"
            );
            let grab = MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: rail.x,
                row: rail.y,
                modifiers: KeyModifiers::NONE,
            };
            app.handle_terminal_event(Event::Mouse(grab)).unwrap();
            for row in rail.y + 1..=rail.bottom() {
                app.handle_terminal_event(Event::Mouse(MouseEvent {
                    kind: MouseEventKind::Drag(MouseButton::Left),
                    row,
                    ..grab
                }))
                .unwrap();
            }
            app.handle_terminal_event(Event::Mouse(MouseEvent {
                kind: MouseEventKind::Up(MouseButton::Left),
                row: rail.bottom(),
                ..grab
            }))
            .unwrap();

            assert_eq!(
                app.reference_panel
                    .as_ref()
                    .unwrap()
                    .snippet_code_scroll
                    .get(),
                last,
                "the drag moved the snippet code"
            );
            assert_eq!(
                app.editor().primary_selection(),
                selection,
                "the score's selection did not follow the scrollbar"
            );
            assert_eq!(
                app.editor().viewport(),
                viewport,
                "the score did not scroll under the scrollbar's drag"
            );
        }

        #[test]
        fn generator_draws_the_existing_slider_style_and_advertises_its_actions() {
            use crate::reference::{Reference, ReferencePanel, ReferenceView, generator_rail};
            use ratatui::{buffer::Buffer, style::Color, widgets::Widget};
            let reference = Reference::load_all();
            let mut panel = ReferencePanel::browse(&reference);
            panel.select_tab(Tab::Generator);
            panel.snippet_selected = 5;
            let theme = Theme::built_in_default();
            let keybinds = crate::keybinds::Keybinds::default();
            for (width, progress, symbols_supported, note, has_history) in [
                (38, None, false, None, false),
                (60, Some(0.5), true, None, true),
                (38, Some(0.5), false, Some("loading..."), true),
                (60, Some(0.5), true, Some("3"), false),
            ] {
                panel.generator = Generator::seeded(42);
                if has_history {
                    panel.generator.fresh();
                    panel.generator.similar();
                }
                let _symbols = crate::terminal::ForceSymbolsForTest::set(symbols_supported);
                let area = Rect::new(0, 0, width, 30);
                let mut buffer = Buffer::empty(area);
                ReferenceView {
                    reference: &reference,
                    panel: &panel,
                    theme: &theme,
                    keybinds: &keybinds,
                    focused: true,
                    vim: false,
                    anchored: false,
                    pulse: None,
                    sounding_note: None,
                    picture: false,
                    refused: None,
                    loading: None,
                    caching: 0,
                    importing: 0,
                    library_loading: false,
                    playing: None,
                    preview_note: note.map(|note| {
                        (
                            Some(crate::reference::SnippetLine::Generator(Row::Generate(
                                Direction::Glass,
                            ))),
                            note.to_owned(),
                            false,
                        )
                    }),
                    preview_row: progress.map(|_| {
                        Some(crate::reference::SnippetLine::Generator(Row::Generate(
                            Direction::Glass,
                        )))
                    }),
                    preview_progress: progress,
                }
                .render(area, &mut buffer);
                let inner = inner_area(area);
                let layout = panel.snippet_layout(inner);
                let y = layout.list.y + 5;
                let rail = generator_rail(Rect::new(layout.list.x, y, layout.list.width, 1));
                let symbols: Vec<_> = (rail.x..rail.right())
                    .map(|x| buffer[(x, y)].symbol())
                    .collect();
                assert_eq!(symbols.iter().filter(|&&s| s == "█").count(), 1);
                assert!(symbols.iter().all(|&s| s == "─" || s == "█"));
                let generate_row: String = (layout.list.x..layout.list.right())
                    .map(|x| buffer[(x, layout.list.y + 1)].symbol())
                    .collect();
                assert!(generate_row.contains(if symbols_supported {
                    "⟳  Generate"
                } else {
                    "*  Generate"
                }));
                if let Some(note) = note {
                    assert!(generate_row.ends_with(note));
                    assert!(
                        !generate_row.contains(['▓', '░']),
                        "a status replaces the progress slot"
                    );
                    assert_eq!(
                        buffer[(layout.list.right() - 1, layout.list.y + 1)].fg,
                        theme.warn
                    );
                } else {
                    assert!(generate_row.ends_with(if progress.is_some() {
                        "▓▓▓▓░░░░"
                    } else {
                        "░░░░░░░░"
                    }));
                }
                for offset in 1..=3 {
                    let row: String = (layout.list.x..layout.list.right())
                        .map(|x| buffer[(x, layout.list.y + offset)].symbol())
                        .collect();
                    assert!(
                        !["[g]", "[v]", "[c]"].iter().any(|hint| row.contains(hint)),
                        "action shortcuts belong in the footer: {row}"
                    );
                    assert_eq!(
                        buffer[(layout.list.x + 4, layout.list.y + offset)].fg,
                        theme.accent
                    );
                }
                for (action, offset, forward_column) in [
                    (crate::ideas::Action::Generate, 1, 16),
                    (crate::ideas::Action::Similar, 2, 12),
                ] {
                    let y = layout.list.y + offset;
                    let back = (layout.list.x + 2, y);
                    let forward = (layout.list.x + forward_column, y);
                    assert_eq!(
                        buffer[back].symbol(),
                        if !has_history {
                            " "
                        } else if symbols_supported {
                            "◀"
                        } else {
                            "<"
                        }
                    );
                    assert_eq!(
                        buffer[forward].symbol(),
                        if symbols_supported { "▶" } else { ">" },
                        "forward stays available to generate at the history end"
                    );
                    assert_eq!(
                        panel.generator_history_at(inner, back.0, back.1),
                        Some((action, false, usize::from(offset)))
                    );
                    assert_eq!(
                        panel.generator_history_at(inner, forward.0, forward.1),
                        Some((action, true, usize::from(offset)))
                    );
                    assert_eq!(panel.generator_history_at(inner, forward.0 + 1, y), None);
                }
                let actions_footer: String = (inner.x..inner.right())
                    .map(|x| buffer[(x, layout.scope)].symbol())
                    .collect();
                for hint in ["g Generate", "v Similar", "c Copy"] {
                    assert!(actions_footer.contains(hint), "{actions_footer}");
                }
                let text: String = buffer.content.iter().map(|cell| cell.symbol()).collect();
                for hint in [
                    "Glassy pulse",
                    "Space play",
                    "Generate",
                    "Copy",
                    "PgUp/Dn",
                    "Controls",
                    "Drums",
                    "Delay",
                ] {
                    assert!(text.contains(hint), "missing {hint} at width {width}");
                }
                // Optional visual QA export; normal tests do not write artifacts.
                if let Some(directory) = std::env::var_os("RUSTEL_GENERATOR_UI_EXPORT") {
                    let directory = std::path::PathBuf::from(directory);
                    std::fs::create_dir_all(&directory).unwrap();
                    let rgb = |color: Color| match color {
                        Color::Rgb(r, g, b) => [r, g, b],
                        _ => [0, 0, 0],
                    };
                    let frame = serde_json::json!({
                        "width": buffer.area.width,
                        "height": buffer.area.height,
                        "cells": buffer.content.iter().map(|cell| serde_json::json!({
                            "text": cell.symbol(), "fg": rgb(cell.fg), "bg": rgb(cell.bg),
                        })).collect::<Vec<_>>()
                    });
                    let symbols = if symbols_supported {
                        "symbols"
                    } else {
                        "ascii"
                    };
                    let status = note.unwrap_or(if progress.is_some() {
                        "playing"
                    } else {
                        "idle"
                    });
                    let path = directory.join(format!("generator-{width}-{symbols}-{status}.json"));
                    std::fs::write(path, frame.to_string()).unwrap();
                }
            }
        }

        #[test]
        fn every_generator_opens_from_the_tree_and_exposes_its_own_controls() {
            let directory = tempfile::tempdir().unwrap();
            let mut app = crate::app::tests::support::app_over(directory.path());
            open_generator(&mut app);
            app.frame = Rect::new(0, 0, 100, 28);
            app.regions = view::regions(app.frame, false, 1, true, true, None, None);
            let inner = inner_area(app.regions.reference);
            for direction in Direction::ALL {
                let panel = app.reference_panel.as_mut().unwrap();
                panel.snippet_selected = panel
                    .generator
                    .rows()
                    .iter()
                    .position(|row| *row == Row::Direction(direction))
                    .unwrap();
                app.handle_reference_key(KeyCode::Right, false, false)
                    .unwrap();
                let panel = app.reference_panel.as_ref().unwrap();
                assert_eq!(panel.generator.direction(), direction);
                assert!(panel.generator.open);
                for &index in direction.controls() {
                    let panel = app.reference_panel.as_mut().unwrap();
                    panel.snippet_selected = panel
                        .generator
                        .rows()
                        .iter()
                        .position(|row| *row == Row::Control(index))
                        .unwrap();
                    let geometry = panel.geometry(inner);
                    let y = geometry.list.y + (panel.snippet_selected - geometry.first_row) as u16;
                    let rail = crate::reference::generator_rail(Rect::new(
                        geometry.list.x,
                        y,
                        geometry.list.width,
                        1,
                    ));
                    assert!(
                        y < geometry.list.bottom(),
                        "{direction:?}: control {index} is scrolled into view"
                    );
                    assert_eq!(
                        panel.generator_control_at(inner, rail.x, y),
                        Some((index, rail))
                    );
                    app.handle_reference_key(KeyCode::Home, false, false)
                        .unwrap();
                    assert_eq!(
                        app.reference_panel.as_ref().unwrap().generator.controls[index],
                        0
                    );
                    app.handle_reference_key(KeyCode::End, false, false)
                        .unwrap();
                    assert_eq!(
                        app.reference_panel.as_ref().unwrap().generator.controls[index],
                        100
                    );
                }
            }
        }

        /// Only a row the cursor rests on has its sounds asked for: asking decodes
        /// them - a whole soundfont for a `gm_` instrument - and an arrow held
        /// through the rows must not decode every one it passes.
        #[test]
        fn a_snippet_is_warmed_only_once_the_cursor_rests_on_it() {
            let directory = tempfile::tempdir().unwrap();
            let mut app = crate::app::tests::support::app_over(directory.path());
            open_generator(&mut app);

            app.warm_selected_snippet();
            let (resting, _) = app
                .snippet_under_cursor
                .clone()
                .expect("the row under the cursor is noted");
            assert_eq!(app.warmed_snippet, None, "not on arrival");
            app.warm_selected_snippet();
            assert_eq!(app.warmed_snippet, None, "not before the dwell");

            // Rested long enough: asked for, once.
            app.snippet_under_cursor = Some((resting.clone(), Instant::now() - SNIPPET_WARM_DWELL));
            app.warm_selected_snippet();
            assert!(app.worker.library().is_some(), "the studio has a library");
            assert_eq!(app.warmed_snippet.as_deref(), Some(resting.as_str()));

            // Another row starts its own wait.
            app.snippet_under_cursor = Some(("another row".into(), Instant::now()));
            app.warmed_snippet = None;
            app.warm_selected_snippet();
            assert_eq!(app.warmed_snippet, None, "a new row waits again");
            assert_eq!(
                app.snippet_under_cursor
                    .as_ref()
                    .map(|(code, _)| code.as_str()),
                Some(resting.as_str()),
                "the wait follows the row actually selected"
            );
        }
    }

    #[cfg(feature = "hydra")]
    mod preview_readiness {
        //! Tests for preview readiness.

        use crate::app::tests::support::app_over;
        use crate::app::*;

        fn queue_preview(app: &mut App, code: &str) {
            // Hold the request on the UI side, then deliver its install below.
            // The engine must not fetch the real GM bank behind the test library.
            app.prebake_queue.push_back(PrebakeRequest {
                request_id: u64::MAX,
                scope: PrebakeScope::Global,
                source: Arc::from(""),
                reinstall_after: false,
            });
            app.preview_snippet_score(code);
            app.prebake_queue.clear();
            assert!(app.pending_evaluation.is_some());
            assert!(app.inflight.is_empty());
        }

        fn install_preview(app: &mut App) {
            let preview = app.snippet_preview.as_ref().expect("preview queued");
            app.installed_revision = Some(preview.revision.clone());
            app.evaluated_source = Some(Arc::clone(&preview.source));
            app.audible_scene = Some(preview.scene);
            app.pending_evaluation = None;
            app.inflight.clear();
            app.snapshot = Some(StudioSnapshot {
                playing: true,
                ..Default::default()
            });
        }

        #[test]
        fn installed_generated_preview_waits_for_its_samples_then_becomes_ready() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            let library = Arc::new(
                rustel_runtime::samples::SampleLibrary::with_loading_sample_for_test("gm_clavinet"),
            );
            app.worker.set_library(Arc::clone(&library));
            let scene = app.scenes.current().id;
            app.readiness.insert(scene, Readiness::default());

            queue_preview(&mut app, "$: note('c3 e3').s('gm_clavinet')");
            assert_eq!(
                app.snippet_preview_note(),
                Some(("loading...".into(), false))
            );
            install_preview(&mut app);
            assert!(app.is_playing(), "the transport has started");
            assert_eq!(
                app.snippet_preview_note(),
                Some(("loading...".into(), false)),
                "installing a source does not make its pending samples audible"
            );
            assert_eq!(app.go_state(), GoState::Loading { ready: 0, total: 1 });

            // A long download stays loading until this source's asset state
            // actually changes.
            app.poll_readiness();
            assert_eq!(
                app.snippet_preview_note(),
                Some(("loading...".into(), false))
            );
            app.dirty_frame = false;
            library.finish_loading_sample_for_test();
            app.poll_readiness();
            assert!(app.dirty_frame, "completion schedules the new caption");
            assert_eq!(app.go_state(), GoState::Ready);
            // Sounding says nothing in words: the strip beside the row carries it.
            assert!(app.snippet_preview_note().is_none());
        }

        #[test]
        fn failed_preview_and_its_replacement_keep_separate_readiness() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            let library = Arc::new(
                rustel_runtime::samples::SampleLibrary::with_loading_sample_for_test("gm_epiano2"),
            );
            app.worker.set_library(Arc::clone(&library));
            let scene = app.scenes.current().id;
            app.readiness.insert(scene, Readiness::default());

            queue_preview(&mut app, "$: note('c3').s('gm_epiano2')");
            install_preview(&mut app);
            library.fail_loading_sample_for_test();
            app.poll_readiness();
            assert_eq!(app.go_state(), GoState::Failed { failed: 1 });
            assert_eq!(
                app.snippet_preview_note(),
                Some(("1 sound failed to load".into(), false))
            );

            // The editor can have unrelated problems while a valid generated
            // preview plays. Neither those nor an old loading report own its UI.
            app.lint.insert(
                scene,
                crate::lint::LintResult {
                    scene,
                    revision: app.editor().revision(),
                    input_channels: None,
                    diagnostics: vec![rustel_runtime::lint::Diagnostic {
                        level: rustel_runtime::lint::Level::Syntax,
                        message: "unfinished editor source".into(),
                        from: 0,
                        to: 1,
                    }],
                },
            );
            queue_preview(&mut app, "$: note('c4').s('sine')");
            install_preview(&mut app);
            app.poll_readiness();
            assert_eq!(app.go_state(), GoState::Ready);
            // Sounding says nothing in words: the strip beside the row carries it.
            assert!(app.snippet_preview_note().is_none());

            assert!(app.stop_snippet_preview());
            assert!(app.snippet_preview_note().is_none());
            assert_eq!(
                app.go_state(),
                GoState::Problems { count: 1 },
                "stopping the preview restores the edited score's verdict"
            );
        }

        #[test]
        fn preview_readiness_follows_the_installed_composite_not_the_edited_score() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            let library = Arc::new(
                rustel_runtime::samples::SampleLibrary::with_loading_sample_for_test("gm_epiano2"),
            );
            app.worker.set_library(Arc::clone(&library));
            let score = Arc::<str>::from("$: note('c3').s('gm_epiano2')");
            app.audible_scene = Some(app.scenes.current().id);
            app.installed_revision = Some(source_revision(&score));
            app.evaluated_source = Some(score);
            app.snapshot = Some(StudioSnapshot {
                playing: true,
                ..Default::default()
            });
            app.scenes.current_mut().editor = Editor::new("$: s('saw')").expect("editor");
            app.refresh_readiness(app.scenes.current().id);
            assert_eq!(app.go_state(), GoState::Ready);

            queue_preview(&mut app, "$: note('g4').s('sine')");
            let sent = app
                .pending_evaluation
                .as_ref()
                .or_else(|| app.inflight.values().next())
                .expect("preview request");
            let preview = app.snippet_preview.as_ref().expect("preview");
            assert!(Arc::ptr_eq(&preview.source, &sent.source));
            assert!(preview.source.contains("gm_epiano2"));
            assert!(!preview.source.contains("saw"));
            install_preview(&mut app);
            assert_eq!(app.go_state(), GoState::Loading { ready: 1, total: 2 });
            assert_eq!(
                app.snippet_preview_note(),
                Some(("loading...".into(), false))
            );

            library.finish_loading_sample_for_test();
            app.poll_readiness();
            assert_eq!(app.go_state(), GoState::Ready);
            // Sounding says nothing in words: the strip beside the row carries it.
            assert!(app.snippet_preview_note().is_none());
        }

        #[test]
        fn an_uninstalled_or_stopped_preview_does_not_override_the_editor_header() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            app.worker
                .set_library(Arc::new(rustel_runtime::samples::SampleLibrary::empty()));
            let scene = app.scenes.current().id;
            app.readiness.insert(
                scene,
                Readiness {
                    failed: 1,
                    ..Default::default()
                },
            );
            queue_preview(&mut app, "$: s('sine')");
            assert_eq!(app.go_state(), GoState::Failed { failed: 1 });
            install_preview(&mut app);
            assert_eq!(app.go_state(), GoState::Ready);

            let preview_hash = app.installed_revision.clone();
            app.installed_revision = Some(source_revision("$: s('saw')"));
            assert_eq!(app.go_state(), GoState::Failed { failed: 1 });
            app.installed_revision = preview_hash;
            app.stop_requested = true;
            assert_eq!(app.go_state(), GoState::Failed { failed: 1 });
            app.stop_requested = false;
            app.snapshot = None;
            assert_eq!(app.go_state(), GoState::Failed { failed: 1 });
        }

        #[test]
        fn unknown_preview_sounds_are_unchecked_without_changing_the_editor_verdict() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            app.worker
                .set_library(Arc::new(rustel_runtime::samples::SampleLibrary::empty()));
            let scene = app.scenes.current().id;
            app.scenes.current_mut().editor =
                Editor::new("$: s('not_a_registered_sound')").expect("editor");
            app.refresh_readiness(scene);
            assert_eq!(app.readiness[&scene].unknown, 1);
            assert_eq!(
                app.go_state(),
                GoState::Ready,
                "the existing editor policy leaves unknown names to lint"
            );

            queue_preview(&mut app, "$: s('not_a_registered_preview_sound')");
            install_preview(&mut app);
            assert_eq!(
                app.snippet_preview
                    .as_ref()
                    .unwrap()
                    .readiness
                    .as_ref()
                    .unwrap()
                    .unknown,
                1
            );
            assert_eq!(app.go_state(), GoState::Unchecked);
            assert_eq!(
                app.snippet_preview_note(),
                Some(("sounds unchecked".into(), false))
            );

            // A library handle may also be absent before the engine exposes it.
            // Lack of evidence must not become a successful sample check either.
            app.snippet_preview.as_mut().unwrap().readiness = None;
            assert_eq!(app.go_state(), GoState::Unchecked);
            assert_eq!(
                app.snippet_preview_note(),
                Some(("sounds unchecked".into(), false))
            );
            assert!(app.stop_snippet_preview());
            assert_eq!(app.go_state(), GoState::Ready);
        }
    }

    mod previews {
        //! Tests for `app/previews.rs`.

        use crate::app::previews::*;
        use crate::app::tests::support::*;
        use crate::editor::MemoryClipboard;

        #[cfg(feature = "hydra")]
        /// A score nobody labelled gets one, or a preview over it silences it.
        #[test]
        fn a_bare_score_gets_a_label() {
            assert_eq!(
                labelled_score_text("s(\"bd*8\").rlpf(sine)"),
                "score: s(\"bd*8\").rlpf(sine)"
            );
        }

        #[cfg(feature = "hydra")]
        /// A multi-line chain is labelled on the line the expression starts on,
        /// never on a `.method()` continuation.
        #[test]
        fn a_chain_is_labelled_at_its_head() {
            let text = "s(\"bd*8\")\n  .rlpf(sine.slow(4))\n  .gain(.8)\n";
            let labelled = labelled_score_text(text);
            assert_eq!(
                labelled,
                "score: s(\"bd*8\")\n  .rlpf(sine.slow(4))\n  .gain(.8)\n"
            );
        }

        #[cfg(feature = "hydra")]
        /// Declarations stay unlabelled: they return nothing to play, and the
        /// expression after them is the pattern.
        #[test]
        fn the_last_expression_is_labelled_not_a_declaration() {
            let text = "const volume = slider(1)\ns(\"bd\").gain(volume)\n";
            assert_eq!(
                labelled_score_text(text),
                "const volume = slider(1)\nscore: s(\"bd\").gain(volume)\n"
            );
        }

        #[cfg(feature = "hydra")]
        /// A score already written as lanes is left alone - labelling one of
        /// its blocks would fight the `$:` keys the set plays under.
        #[test]
        fn a_laned_score_is_untouched() {
            let text = "$: s(\"bd*4\")\n$bass: n(\"1\").s(\"supersaw\")\n";
            assert_eq!(labelled_score_text(text), text);
            // The corpus's own spelling: a label name that ends in `$`.
            let alt = "_$OPENHAT: s(\"<- oh>*8\")\n";
            assert_eq!(labelled_score_text(alt), alt);
        }

        #[cfg(feature = "hydra")]
        /// A colon inside a call is not a label, or every `s("bd:4")` would be
        /// read as a track and the score left silent instead.
        #[test]
        fn a_colon_inside_a_call_is_not_a_label() {
            let text = "s(\"bd:4*4\").bank(\"tr909\")\n";
            assert_eq!(labelled_score_text(text), format!("score: {text}"));
        }

        #[cfg(feature = "hydra")]
        /// The point of the label: the runtime plays BOTH the score and the
        /// snippet once the two texts are joined the way a preview joins them.
        #[test]
        fn the_composite_plays_the_score_and_the_snippet() {
            use rustel_fraction::Fraction;
            use rustel_runtime::{Session, SessionConfig};

            let score = "s(\"bd*2\")";
            let composite = format!(
                "setcps(0.5)\n{}\n// preview\n$: s(\"hh*2\")\n",
                labelled_score_text(score)
            );
            let mut session = Session::with_config(SessionConfig::default()).expect("session");
            session.evaluate(&composite).expect("composite evaluates");
            let haps = session
                .query(Fraction::new(0, 1), Fraction::new(1, 1))
                .expect("query");
            let sounds: Vec<String> = haps
                .iter()
                .filter_map(|hap| {
                    let value = hap.value.as_object()?;
                    value.get("s").map(|s| s.to_string())
                })
                .collect();
            assert!(
                sounds.iter().any(|s| s.contains("bd")),
                "the score's own block fell silent: {sounds:?}"
            );
            assert!(
                sounds.iter().any(|s| s.contains("hh")),
                "the snippet never played: {sounds:?}"
            );
        }

        #[cfg(feature = "hydra")]
        /// Without the label the same composite loses the score. This keeps the
        /// labelling helper from being removed as dead code.
        #[test]
        fn an_unlabelled_composite_loses_the_score() {
            use rustel_fraction::Fraction;
            use rustel_runtime::{Session, SessionConfig};

            let composite = "setcps(0.5)\ns(\"bd*2\")\n// preview\n$: s(\"hh*2\")\n";
            let mut session = Session::with_config(SessionConfig::default()).expect("session");
            session.evaluate(composite).expect("composite evaluates");
            let haps = session
                .query(Fraction::new(0, 1), Fraction::new(1, 1))
                .expect("query");
            let has_bd = haps.iter().any(|hap| {
                hap.value
                    .as_object()
                    .and_then(|value| value.get("s"))
                    .is_some_and(|s| s.to_string().contains("bd"))
            });
            assert!(
                !has_bd,
                "if bare scores survive a labelled sibling the label is no longer needed"
            );
        }

        #[cfg(feature = "hydra")]
        /// The prefix the composite relies on is the one the tests spell out.
        #[test]
        fn the_prefix_is_a_plain_named_label() {
            assert_eq!(LABELLED_SCORE_PREFIX, "score: ");
        }

        /// The preview fader moves on the keyboard, and the row shows the keys.
        #[test]
        fn the_preview_fader_moves_on_the_keyboard_and_says_so() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            app.reference_panel = Some(crate::reference::ReferencePanel::samples(
                &app.reference,
                Vec::new(),
            ));
            app.focus = Focus::Panel(PanelKind::Reference);

            let start = app.preview_gain;
            press(&mut app, KeyCode::Up, KeyModifiers::ALT);
            let raised = app.preview_gain;
            assert!(
                raised > start,
                "the up arrow raised it: {start} -> {raised}"
            );
            press(&mut app, KeyCode::Down, KeyModifiers::ALT);
            assert!(
                (app.preview_gain - start).abs() < 1e-6,
                "and the down arrow put it back: {}",
                app.preview_gain
            );

            // The older spelling still answers, for the layouts it suits.
            press(&mut app, KeyCode::Char('+'), KeyModifiers::ALT);
            assert!(app.preview_gain > start, "the plus key still raises it");
        }

        /// Alt+A says whether walking the browser's lists of things to hear
        /// plays each row the cursor lands on, and the answer is kept for
        /// the next session.
        #[test]
        fn alt_a_previews_each_row_you_walk_and_is_remembered() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            app.reference_panel = Some(crate::reference::ReferencePanel::browse(&app.reference));
            app.reference_panel
                .as_mut()
                .expect("the browse list")
                .select_tab(crate::reference::Tab::Chords);
            app.focus = Focus::Panel(PanelKind::Reference);

            // Off to start: the cursor walks quietly.
            press(&mut app, KeyCode::Down, KeyModifiers::NONE);
            assert!(app.preview_armed.is_none(), "nothing was previewed");
            // Alt+A turns it on, and the choice is kept.
            press(&mut app, KeyCode::Char('a'), KeyModifiers::ALT);
            assert!(app.ui_settings.auto_preview);
            assert_eq!(app.prefs.auto_preview, Some(true));
            assert!(app.status.contains("auto-play on"), "{}", app.status);
            press(&mut app, KeyCode::Down, KeyModifiers::NONE);
            assert!(app.status.contains("C7"), "{}", app.status);
            // Alt+A again: the choice turns off, and is kept turned off.
            press(&mut app, KeyCode::Char('a'), KeyModifiers::ALT);
            assert!(!app.ui_settings.auto_preview);
            assert_eq!(app.prefs.auto_preview, Some(false));
        }

        /// A terminal that composes Option chords into text sends å for
        /// Option+A. The recovery answers on every tab that auditions, and
        /// leaves the character to the search box everywhere else.
        #[test]
        fn an_option_composed_alt_a_answers_on_every_tab_that_auditions() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            app.keybinds.set_reach(crate::keybinds::Reach {
                enhanced: false,
                terminal: "iTerm2".into(),
            });
            app.reference_panel = Some(crate::reference::ReferencePanel::browse(&app.reference));
            app.reference_panel
                .as_mut()
                .expect("the browse list")
                .select_tab(crate::reference::Tab::Chords);
            app.focus = Focus::Panel(PanelKind::Reference);

            press(&mut app, KeyCode::Char('å'), KeyModifiers::NONE);
            assert!(app.ui_settings.auto_preview, "{}", app.status);
            assert!(
                app.reference_panel
                    .as_ref()
                    .expect("the browse list")
                    .chord_query
                    .is_empty(),
                "the character did not type into the search"
            );

            app.reference_panel
                .as_mut()
                .expect("the browse list")
                .select_tab(crate::reference::Tab::Reference);
            press(&mut app, KeyCode::Char('å'), KeyModifiers::NONE);
            assert!(
                app.ui_settings.auto_preview,
                "a tab with nothing to preview kept the character for its text"
            );
            assert_eq!(
                app.reference_panel.as_ref().expect("the browse list").query,
                "å"
            );
        }

        /// A scale is a thing to hear: → and Space play it, one note after
        /// another.
        #[test]
        fn a_scale_list_plays_what_is_under_the_cursor() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            app.editor_mut()
                .dispatch(
                    Command::PasteText("$: n(\"0 2 4\").scale(\"major".into()),
                    HistoryMoment(0),
                    &mut MemoryClipboard::default(),
                )
                .expect("seed");
            app.toggle_reference_browse();
            let panel = app.reference_panel.as_ref().expect("panel open");
            assert_eq!(panel.tab, crate::reference::Tab::Scales);
            // A scale row is that scale on C: what the ear expects to hear,
            // and what a score writes.
            let action = panel.preview();
            assert!(
                matches!(&action, PanelAction::PreviewScale(name) if name == "C:major"),
                "{action:?}"
            );
        }

        /// A tuning is a set of ratios with an unreadable name, so the row is
        /// chosen by ear: the list plays it the way the scales list does.
        #[test]
        fn a_tuning_row_plays_what_is_under_the_cursor() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            app.editor_mut()
                .dispatch(
                    Command::PasteText("$: n(\"0 .. 7\").xen(\"slend".into()),
                    HistoryMoment(0),
                    &mut MemoryClipboard::default(),
                )
                .expect("seed");
            app.toggle_reference_browse();
            let panel = app.reference_panel.as_ref().expect("panel open");
            let action = panel.preview();
            assert!(
                matches!(&action, PanelAction::PreviewTuning(name) if name == "slendro"),
                "{action:?}"
            );
            // Every curated tuning resolves, and its degrees are fractional
            // MIDI rather than semitones - the reason a name cannot stand in
            // for the sound.
            let names = crate::reference::tuning_vocabulary();
            assert!(names.len() > 12, "a list worth opening");
            for name in &names {
                let notes = crate::reference::tuning_notes(name);
                assert!(!notes.is_empty(), "{name} resolved to nothing");
                assert!(
                    notes.windows(2).all(|pair| pair[1] > pair[0]),
                    "{name} must rise"
                );
            }
            let slendro = crate::reference::tuning_notes("slendro");
            assert!(
                slendro
                    .iter()
                    .any(|midi| (midi - midi.round()).abs() > 0.05),
                "a tuning that landed on semitones would need no preview: {slendro:?}"
            );
        }

        /// Stepping the preview volume through 0 dB lands on it exactly: the
        /// default must always be reachable (± steps used to give +1.7/−1.3).
        #[test]
        fn the_preview_volume_steps_land_exactly_on_unity() {
            let below = rustel_audio::db_to_linear(-1.3);
            let stepped = nudge_preview_gain(below, 3.0);
            assert_eq!(stepped, 1.0, "crossing zero snaps to unity");
            let above = nudge_preview_gain(stepped, 3.0);
            assert!(above > 1.0, "from unity the step moves on");
            assert_eq!(nudge_preview_gain(above, -3.0), 1.0);
        }

        /// A transport stop also stops the sample that the browser auditions,
        /// and the bar that draws its progress.
        #[test]
        fn a_stop_stops_the_preview_and_the_bar_drawing_it() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            app.preview_armed = Some(("bd:0".into(), Instant::now()));
            app.preview_shape = Some((std::sync::Arc::from(&[200u8, 40][..]), 30.0));

            app.handle_editor_effect(EditorEffect::Stop).expect("stop");

            assert_eq!(app.preview_armed, None, "the preview is let go");
            assert!(
                app.preview_shape.is_none(),
                "and the shape the bar was filling"
            );
            assert!(
                app.status
                    .starts_with("stopping - letting the tail ring out"),
                "the first press drains: {}",
                app.status
            );
            assert!(app.status.contains("again cuts it"));

            // With the transport at rest the audition was the only voice,
            // and the first press stopped it: a second press has nothing
            // left to cut, so the word that already said the stop stands.
            // The graceful-then-immediate pair belongs to the transport,
            // whose tail a second press really does cut.
            app.handle_editor_effect(EditorEffect::Stop)
                .expect("second stop");
            assert!(
                app.status
                    .starts_with("stopping - letting the tail ring out"),
                "nothing sounds, so the standing word stays: {}",
                app.status
            );
            // Those two are the whole of what the browser draws the bar from -
            // the shape and the clock that started it - so with neither there
            // is no progress left to draw however long the clock runs.
        }

        /// An update silences whatever the browser was auditioning. A sample
        /// still ringing under the set's own first beat is a sound nobody
        /// asked for, and the status line belongs to the update.
        #[test]
        fn an_update_stops_a_sounding_sample_preview() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            app.scenes.current_mut().editor = Editor::new("$: s(\"sine\")").expect("editor");
            app.preview_armed = Some(("bd:0".into(), Instant::now()));
            app.preview_shape = Some((std::sync::Arc::from(&[200u8, 40][..]), 0.5));

            app.dispatch_editor(Command::Evaluate).expect("update");

            assert_eq!(app.preview_armed, None, "the preview is let go");
            assert!(app.preview_shape.is_none(), "and its shape with it");
            assert!(
                !app.status.contains("preview"),
                "the update keeps the status line: {}",
                app.status
            );
        }

        /// Every wait before the sound says the same plain word, and a
        /// sounding preview says nothing in words: the playhead strip beside
        /// the row carries where the bar stands.
        #[cfg(feature = "hydra")]
        #[test]
        fn a_preview_says_what_it_is_waiting_for() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            assert!(app.snippet_preview_note().is_none(), "nothing to preview");

            app.preview_snippet_score("$: s(\"hh*8\")");
            let (note, playing) = app.snippet_preview_note().expect("a note");
            assert_eq!(note, "loading...");
            assert!(!playing);

            // The engine has it; now it waits for the cycle line.
            app.pending_evaluation = None;
            app.inflight.clear();
            let mut snapshot = sounding(true, false);
            snapshot.launch = Some(crate::engine::LaunchInfo {
                boundary_cycle: 8.0,
                cycles_left: 0.4,
                seconds_left: 0.8,
            });
            app.snapshot = Some(snapshot);
            let (note, _) = app.snippet_preview_note().expect("a note");
            assert_eq!(note, "loading...");

            // Landed, and the sounds it names are still coming down.
            app.snapshot = Some(sounding(true, false));
            app.snippet_preview.as_mut().expect("preview").readiness = Some(Readiness {
                loading: 1,
                ..Default::default()
            });
            let (note, playing) = app.snippet_preview_note().expect("a note");
            assert_eq!(note, "loading...");
            assert!(!playing);

            // Nothing to say: the gap between installed and sounding is a
            // fraction of a second, and a word for it reads as a fault.
            app.snippet_preview.as_mut().expect("preview").readiness = Some(Readiness::default());
            assert!(app.snippet_preview_note().is_none());

            // The engine says the preview's own source is what is installed:
            // it is playing - and says nothing, because the playhead strip
            // beside the row carries where the bar stands.
            app.installed_revision = app
                .snippet_preview
                .as_ref()
                .map(|preview| preview.revision.clone());
            assert!(app.snippet_preview_note().is_none());
        }

        /// An installed preview counts the bar a musician would, instead of
        /// saying "playing" over the silence before its first downbeat. The
        /// count steps down to the running clock's next line, ends on it, and
        /// ends early the moment a mark says the sound is audible.
        #[cfg(feature = "hydra")]
        #[test]
        fn an_installed_preview_counts_in_to_its_first_downbeat() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            app.preview_snippet_score("$: s(\"bd*4\")");
            app.pending_evaluation = None;
            app.inflight.clear();
            // Installed, sounds ready, and the clock mid-cycle: three beats of
            // the bar are still to go before the line the preview joins on.
            let mut snapshot = sounding(true, false);
            snapshot.cycle = 0.25;
            snapshot.source_revision = app
                .snippet_preview
                .as_ref()
                .map(|preview| preview.revision.clone());
            app.snapshot = Some(snapshot.clone());
            app.snippet_preview.as_mut().expect("preview").readiness = Some(Readiness::default());
            app.installed_revision = app
                .snippet_preview
                .as_ref()
                .map(|preview| preview.revision.clone());

            let (note, playing) = app.snippet_preview_note().expect("a note");
            assert_eq!(note, "3");
            assert!(!playing, "the count is not yet the music");

            // Half a bar later the count has followed the clock down.
            snapshot.cycle = 0.62;
            app.snapshot = Some(snapshot.clone());
            let (note, _) = app.snippet_preview_note().expect("a note");
            assert_eq!(note, "2");

            // On the line the wait is over: the preview is in the music, and
            // the strip says where - no word over it.
            snapshot.cycle = 1.0;
            app.snapshot = Some(snapshot.clone());
            assert!(app.snippet_preview_note().is_none());

            // A mark landing before the line ends the count early: whatever is
            // being waited for has arrived.
            snapshot.cycle = 0.4;
            app.snapshot = Some(snapshot);
            app.snippet_heard = true;
            assert!(app.snippet_preview_note().is_none());
        }

        /// The playhead strip follows the clock the preview sounds on. Its
        /// position is a fraction of the bar and matches the pattern's
        /// position, also when a bar is half drawn under silence.
        #[cfg(feature = "hydra")]
        #[test]
        fn a_playing_preview_rides_the_clock_its_pattern_is_on() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            app.preview_snippet_score("$: s(\"bd*4\")");
            app.pending_evaluation = None;
            app.inflight.clear();
            // The clock is the outgoing score's: the snapshot does not name
            // this preview's revision, and no bar position for a pattern that
            // is not riding this clock.
            let mut snapshot = sounding(true, false);
            snapshot.cycle = 0.5;
            app.snapshot = Some(snapshot.clone());
            assert!(app.snippet_preview_progress().is_none());

            // The snapshot names the preview's revision: the clock is its
            // own, and the fraction is the cycle's, wrapped to the bar.
            snapshot.source_revision = app
                .snippet_preview
                .as_ref()
                .map(|preview| preview.revision.clone());
            app.installed_revision = app
                .snippet_preview
                .as_ref()
                .map(|preview| preview.revision.clone());
            app.snapshot = Some(snapshot.clone());
            let progress = app.snippet_preview_progress().expect("a position");
            assert!((progress - 0.5).abs() < 1e-6, "half a bar in: {progress}");

            // Two and a half bars in is still half a bar through the strip.
            snapshot.cycle = 2.5;
            app.snapshot = Some(snapshot.clone());
            let progress = app.snippet_preview_progress().expect("a position");
            assert!((progress - 0.5).abs() < 1e-6, "wrapped: {progress}");

            // A clock that has not reached its fresh anchor reads negative;
            // rem_euclid keeps it a fraction of the bar.
            snapshot.cycle = -0.75;
            app.snapshot = Some(snapshot.clone());
            let progress = app.snippet_preview_progress().expect("a position");
            assert!((progress - 0.25).abs() < 1e-6, "preroll: {progress}");

            // Stopped, there is no pattern anywhere to point at.
            snapshot.cycle = 0.5;
            snapshot.playing = false;
            app.snapshot = Some(snapshot);
            assert!(app.snippet_preview_progress().is_none());
        }

        /// A count ends at the first line the clock crosses and does not start
        /// again on later bars. The next preview counts afresh.
        #[cfg(feature = "hydra")]
        #[test]
        fn a_count_that_misses_its_mark_does_not_start_over() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            app.preview_snippet_score("$: s(\"bd*4\")");
            app.pending_evaluation = None;
            app.inflight.clear();
            app.installed_revision = app
                .snippet_preview
                .as_ref()
                .map(|preview| preview.revision.clone());
            app.snippet_preview.as_mut().expect("preview").readiness = Some(Readiness::default());
            let mut snapshot = sounding(true, false);
            snapshot.source_revision = app
                .snippet_preview
                .as_ref()
                .map(|preview| preview.revision.clone());

            // Counting down to the line, mark still unmapped.
            snapshot.cycle = 0.9;
            app.snapshot = Some(snapshot.clone());
            let (note, _) = app.snippet_preview_note().expect("a note");
            assert_eq!(note, "1");

            // The clock strides past the line between polls. The wait is
            // over - and stays over, bar after bar: the strip rides, no
            // word comes back.
            snapshot.cycle = 1.72;
            app.snapshot = Some(snapshot.clone());
            assert!(app.snippet_preview_note().is_none());
            snapshot.cycle = 2.5;
            app.snapshot = Some(snapshot.clone());
            assert!(
                app.snippet_preview_note().is_none(),
                "no second count for this preview"
            );
            snapshot.cycle = 3.25;
            app.snapshot = Some(snapshot.clone());
            assert!(app.snippet_preview_note().is_none(), "no third either");

            // Backwards to a fresh preroll (a restart re-anchors the clock)
            // does not resurrect the count either: it had its line.
            snapshot.cycle = -0.8;
            app.snapshot = Some(snapshot.clone());
            assert!(app.snippet_preview_note().is_none());

            // A new preview is a new wait, and counts afresh.
            app.preview_snippet_score("$: s(\"hh*2\")");
            app.pending_evaluation = None;
            app.inflight.clear();
            app.installed_revision = app
                .snippet_preview
                .as_ref()
                .map(|preview| preview.revision.clone());
            app.snippet_heard = false;
            app.snippet_preview.as_mut().expect("preview").readiness = Some(Readiness::default());
            snapshot.source_revision = app
                .snippet_preview
                .as_ref()
                .map(|preview| preview.revision.clone());
            app.snapshot = Some(snapshot);
            let (note, _) = app.snippet_preview_note().expect("a note");
            assert_eq!(note, "4");
        }

        /// The count waits for a snapshot that names the preview's revision. A
        /// snapshot from the outgoing score still carries the old cycle, and
        /// the count ignores it.
        #[cfg(feature = "hydra")]
        #[test]
        fn a_count_waits_for_the_clock_its_preview_is_on() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            app.preview_snippet_score("$: s(\"bd*4\")");
            app.pending_evaluation = None;
            app.inflight.clear();
            // The layout says installed - but the snapshot is still sampled on
            // the outgoing clock, late in its bar.
            app.installed_revision = app
                .snippet_preview
                .as_ref()
                .map(|preview| preview.revision.clone());
            app.snippet_preview.as_mut().expect("preview").readiness = Some(Readiness::default());
            let mut snapshot = sounding(true, false);
            snapshot.cycle = 12.5;
            app.snapshot = Some(snapshot.clone());
            assert!(
                app.snippet_preview_note().is_none(),
                "the outgoing clock's bar is not this preview's to count"
            );

            // Cutover: the snapshot now names the preview's revision, and the
            // clock stands mid-bar on the rebased timeline.
            snapshot.source_revision = app
                .snippet_preview
                .as_ref()
                .map(|preview| preview.revision.clone());
            snapshot.cycle = 0.3;
            app.snapshot = Some(snapshot.clone());
            let (note, _) = app.snippet_preview_note().expect("a note");
            assert_eq!(note, "3", "the count starts here, on the preview's clock");
        }

        /// The first snapshot on a preview's own clock can already stand on or
        /// past its downbeat. No count opens for a line that is already here.
        #[cfg(feature = "hydra")]
        #[test]
        fn a_count_does_not_open_for_a_downbeat_already_here() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            app.preview_snippet_score("$: s(\"bd*4\")");
            app.pending_evaluation = None;
            app.inflight.clear();
            app.installed_revision = app
                .snippet_preview
                .as_ref()
                .map(|preview| preview.revision.clone());
            app.snippet_preview.as_mut().expect("preview").readiness = Some(Readiness::default());
            let mut snapshot = sounding(true, false);
            snapshot.source_revision = app
                .snippet_preview
                .as_ref()
                .map(|preview| preview.revision.clone());

            // A hair past zero: the line is here.
            snapshot.cycle = 0.005;
            app.snapshot = Some(snapshot.clone());
            assert!(app.snippet_preview_note().is_none());

            // A new preview, and the first sample lands mid-preroll below
            // zero: the count says how far is left to the downbeat, and no
            // earlier whole line steals it (the target is captured, not any
            // integer the clock crosses).
            app.preview_snippet_score("$: s(\"hh*2\")");
            app.pending_evaluation = None;
            app.inflight.clear();
            app.installed_revision = app
                .snippet_preview
                .as_ref()
                .map(|preview| preview.revision.clone());
            app.snippet_heard = false;
            app.snippet_preview.as_mut().expect("preview").readiness = Some(Readiness::default());
            let mut snapshot = sounding(true, false);
            snapshot.source_revision = app
                .snippet_preview
                .as_ref()
                .map(|preview| preview.revision.clone());
            snapshot.cycle = -0.5;
            app.snapshot = Some(snapshot.clone());
            let (note, _) = app.snippet_preview_note().expect("a note");
            assert_eq!(note, "2", "half a bar of preroll to the zero");

            // Crossing -0.5 -> -0.2 is not the downbeat: the count holds.
            snapshot.cycle = -0.2;
            app.snapshot = Some(snapshot.clone());
            let (note, _) = app.snippet_preview_note().expect("a note");
            assert_eq!(note, "1");
        }

        /// A second preview replaces the first: the score never ends up with
        /// two snippets on the end of it.
        #[cfg(feature = "hydra")]
        #[test]
        fn previewing_another_snippet_takes_the_first_one_back_out() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            let scene = app.scenes.current().id;
            let source = Arc::<str>::from("$: s(\"bd\")");

            app.snapshot = Some(sounding(true, false));
            app.installed_revision = Some(source_revision(&source));
            app.audible_scene = Some(scene);
            app.visual_revision = Some(app.editor().revision());
            app.evaluated_source = Some(Arc::clone(&source));

            app.preview_snippet_score("$: s(\"hh*8\")");
            // The engine answers: what is installed now is the score with the
            // first snippet on the end of it.
            let installed = app
                .pending_evaluation
                .as_ref()
                .or_else(|| app.inflight.values().next())
                .expect("the first preview")
                .source
                .clone();
            app.evaluated_source = Some(installed);
            app.pending_evaluation = None;
            app.inflight.clear();

            app.preview_snippet_score("$: s(\"cp*4\")");
            let sent = app
                .pending_evaluation
                .as_ref()
                .or_else(|| app.inflight.values().next())
                .expect("the second preview");
            assert!(sent.source.contains("cp*4"), "{}", sent.source);
            assert!(
                !sent.source.contains("hh*8"),
                "the first snippet kept playing under the second: {}",
                sent.source
            );
            assert_eq!(
                sent.launch,
                Launch::Now,
                "the snippet it replaced was left playing until a cycle line"
            );
        }

        /// A preview alone plays the snippet from its beginning. The install
        /// itself carries the from-zero flag, and the flag does not stay set
        /// for the next evaluation.
        #[cfg(feature = "hydra")]
        #[test]
        fn a_preview_alone_begins_at_its_beginning_on_the_install_itself() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());

            // Alone, the install carries the from-zero flag: the snippet is
            // queried from its first cycle, not from wherever the cursor
            // already ran.
            app.preview_snippet_score("$: s(\"cp\")");
            let sent = app
                .pending_evaluation
                .as_ref()
                .or_else(|| app.inflight.values().next())
                .expect("the preview was queued");
            assert!(
                sent.rewind,
                "alone, the snippet installs from its own cycle zero"
            );
            app.pending_evaluation = None;
            app.inflight.clear();

            // The flag is spent on the install it rode in on: it must not sit
            // waiting to rewind whatever is evaluated next.
            assert!(!app.next_rewind, "the flag went with the preview");
        }

        /// Under a playing score, a previewed snippet joins the cycle where it
        /// stands. The install carries no from-zero flag.
        #[cfg(feature = "hydra")]
        #[test]
        fn a_preview_under_a_score_keeps_the_set_s_place_in_the_cycle() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            let scene = app.scenes.current().id;
            let base = Arc::<str>::from("$: s(\"bd*4\")");
            app.snapshot = Some(sounding(true, false));
            app.installed_revision = Some(source_revision(&base));
            app.audible_scene = Some(scene);
            app.evaluated_source = Some(Arc::clone(&base));

            app.preview_snippet_score("$: s(\"hh*4\")");
            let sent = app
                .pending_evaluation
                .as_ref()
                .or_else(|| app.inflight.values().next())
                .expect("the under-score preview was queued");
            assert!(
                !sent.rewind,
                "under a score, the snippet keeps the set's place in the cycle"
            );
            assert!(!app.next_rewind, "and no flag lingers after it either");
        }

        /// Stopping the set forgets what a preview was playing under, so
        /// previewing again plays the snippet alone rather than starting the
        /// music back up under it.
        #[cfg(feature = "hydra")]
        #[test]
        fn a_preview_after_a_stop_plays_alone() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            let scene = app.scenes.current().id;
            let source = Arc::<str>::from("$: s(\"bd\")");

            app.snapshot = Some(sounding(true, false));
            app.installed_revision = Some(source_revision(&source));
            app.audible_scene = Some(scene);
            app.visual_revision = Some(app.editor().revision());
            app.evaluated_source = Some(Arc::clone(&source));

            app.preview_snippet_score("$: s(\"hh*8\")");
            let installed = app
                .pending_evaluation
                .as_ref()
                .or_else(|| app.inflight.values().next())
                .expect("the preview")
                .source
                .clone();
            assert!(installed.starts_with(&*source), "{installed}");
            app.evaluated_source = Some(installed);
            app.pending_evaluation = None;
            app.inflight.clear();

            // The set is stopped: the engine says so, and the studio forgets
            // both the snapshot and what the preview was playing under.
            app.handle_control(StudioControlEvent::Stopped(Box::new(
                crate::engine::StudioStop {
                    acknowledged: true,
                    report: Default::default(),
                },
            )));
            assert!(
                app.snippet_preview.is_none(),
                "the preview survived the stop"
            );

            app.preview_snippet_score("$: s(\"cp*4\")");
            let sent = app
                .pending_evaluation
                .as_ref()
                .or_else(|| app.inflight.values().next())
                .expect("the second preview");
            assert!(sent.source.contains("cp*4"), "{}", sent.source);
            assert!(
                !sent.source.contains("bd"),
                "previewing after a stop started the score again: {}",
                sent.source
            );
        }

        /// A stop that the engine has not confirmed yet still counts as
        /// stopped: a preview in that window plays alone.
        #[cfg(feature = "hydra")]
        #[test]
        fn a_preview_between_the_stop_and_its_answer_still_plays_alone() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            let scene = app.scenes.current().id;
            let source = Arc::<str>::from("$: s(\"bd\")");

            app.snapshot = Some(sounding(true, false));
            app.installed_revision = Some(source_revision(&source));
            app.audible_scene = Some(scene);
            app.visual_revision = Some(app.editor().revision());
            app.evaluated_source = Some(Arc::clone(&source));
            app.preview_snippet_score("$: s(\"hh*8\")");
            app.evaluated_source = Some(
                app.pending_evaluation
                    .as_ref()
                    .or_else(|| app.inflight.values().next())
                    .expect("the preview")
                    .source
                    .clone(),
            );
            app.pending_evaluation = None;
            app.inflight.clear();

            // Stop, and press play on another snippet before the engine has
            // had a chance to answer: the snapshot still says playing.
            app.handle_editor_effect(EditorEffect::Stop).expect("stop");
            assert!(
                app.snapshot
                    .as_ref()
                    .is_some_and(|snapshot| snapshot.playing)
            );
            app.pending_evaluation = None;
            app.inflight.clear();

            app.preview_snippet_score("$: s(\"cp*4\")");
            let sent = app
                .pending_evaluation
                .as_ref()
                .or_else(|| app.inflight.values().next())
                .expect("the preview after the stop");
            assert!(sent.source.contains("cp*4"), "{}", sent.source);
            assert!(
                !sent.source.contains("bd") && !sent.source.contains("hh*8"),
                "the stopped set came back under the preview: {}",
                sent.source
            );
        }

        /// Previewed with nothing playing, a snippet plays alone at 120
        /// rather than at its own tempo or at whatever the studio was last
        /// set to, and stopping it stops the transport rather than installing
        /// a score nobody asked to hear.
        #[cfg(feature = "hydra")]
        #[test]
        fn a_snippet_previewed_in_silence_plays_alone_at_a_hundred_and_twenty() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            let snippet = "setcps(0.5) // 120 bpm, house\n$: s(\"hh*8\")";

            app.preview_snippet_score(snippet);

            let sent = app
                .pending_evaluation
                .as_ref()
                .or_else(|| app.inflight.values().next())
                .expect("nothing was installed for the preview");
            assert_eq!(&*sent.source, "setcps(0.5000)\n$: s(\"hh*8\")");
            assert_eq!(
                app.snippet_preview
                    .as_ref()
                    .map(|preview| &sent.source[preview.offset..]),
                Some("$: s(\"hh*8\")"),
                "the offset did not skip the audition tempo"
            );
            assert!(app.stop_snippet_preview());
            assert!(app.snippet_preview.is_none());
        }

        /// A composed track auditioned on its own plays at the tempo it names
        /// in its comment, not at the studio's default: drum and bass heard at
        /// 120 is not drum and bass.
        #[cfg(feature = "hydra")]
        #[test]
        fn a_track_previewed_alone_plays_at_the_tempo_it_names() {
            assert_eq!(
                named_tempo("// 174 bpm, drum and bass\n$: s(\"bd\")"),
                Some(0.725)
            );
            assert_eq!(named_tempo("// 78 bpm, lo-fi beats"), Some(0.325));
            // Anything that is not a tempo line, and anything absurd, is not
            // a tempo: the studio's own default stands.
            assert_eq!(named_tempo("$: s(\"bd\")"), None);
            assert_eq!(named_tempo("// a comment"), None);
            assert_eq!(named_tempo("// 9000 bpm, nope"), None);

            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            app.preview_snippet_score("// 174 bpm, drum and bass\n$: s(\"bd*4\")");
            let sent = app
                .pending_evaluation
                .as_ref()
                .or_else(|| app.inflight.values().next())
                .expect("the preview");
            assert!(sent.source.starts_with("setcps(0.7250)"), "{}", sent.source);
        }

        /// A mark from the played block lands on the snippet the shelf shows,
        /// even though the tempo line was not played.
        #[cfg(feature = "hydra")]
        #[test]
        fn a_preview_mark_lands_on_the_line_the_shelf_shows() {
            let code = "setcps(0.5)\n$: s(\"bd\")\n$: s(\"hh*8\")";
            let (block, lines) = preview_block(code);
            assert_eq!(block, "$: s(\"bd\")\n$: s(\"hh*8\")");
            let preview = SnippetPreview {
                code: code.to_owned(),
                row: None,
                revision: String::new(),
                source: Arc::from(block.as_str()),
                readiness: None,
                lines,
                offset: 100,
                restore: None,
                scene: SceneId(0),
                clock_owned: std::cell::Cell::new(false),
                count_in_target: std::cell::Cell::new(None),
                count_in_landed: std::cell::Cell::new(false),
            };
            // The second played line, four bytes in: `s("hh*8")`.
            let at = block.find("s(\"hh*8\")").expect("the line");
            let mark = SourceMark {
                from: 100 + at,
                to: 100 + at + 9,
                color: ratatui::style::Color::Reset,
                onset_id: 1,
                strength: 1.0,
            };
            let mapped = preview.mark_in_snippet(&mark).expect("a mark on the shelf");
            assert_eq!(&code[mapped.from..mapped.to], "s(\"hh*8\")");
        }

        /// Esc means "stop the preview" only while one may still be sounding;
        /// a preview that ended long ago leaves the key its ordinary meaning.
        #[test]
        fn stopping_a_preview_only_claims_the_key_while_one_may_sound() {
            let directory = tempfile::tempdir().expect("tempdir");
            let mut app = app_over(directory.path());
            assert!(!app.stop_preview(), "nothing armed");
            app.preview_armed = Some(("bd:0".into(), Instant::now()));
            assert!(app.stop_preview(), "a fresh preview is stopped");
            assert_eq!(app.preview_armed, None, "and forgotten");
            app.preview_armed = Some((
                "bd:0".into(),
                Instant::now() - PREVIEW_STOP_WINDOW - Duration::from_secs(1),
            ));
            assert!(!app.stop_preview(), "a long-gone preview is not");
        }

        #[test]
        fn a_waveform_that_arrives_after_loading_starts_when_the_sound_does() {
            let directory = tempfile::tempdir().expect("tempdir");
            let mut app = app_over(directory.path());
            app.preview_armed = Some(("piano:0".into(), Instant::now() - Duration::from_secs(4)));
            app.preview_shape = None;

            app.show_loaded_preview_shape(
                "piano:0".into(),
                (std::sync::Arc::from(&[12u8, 240, 40][..]), 0.25),
            );

            let (sound, started) = app.preview_armed.as_ref().expect("preview remains armed");
            assert_eq!(sound, "piano:0");
            assert!(
                started.elapsed() < Duration::from_millis(100),
                "the download wait must not count as waveform playback"
            );
            let (_, duration) = app.preview_shape.as_ref().expect("shape is shown");
            assert_eq!(*duration, 0.25);
            assert!(app.dirty_frame, "the arrival asks for an immediate repaint");
        }

        /// Space on the row that is sounding stops it; on any other row it
        /// plays that row.
        #[test]
        fn space_toggles_the_sounding_preview() {
            check_space_toggles_the_sounding_preview(false);
        }

        #[test]
        fn piano_mode_keeps_space_preview_play_stop_usable() {
            check_space_toggles_the_sounding_preview(true);
        }

        fn check_space_toggles_the_sounding_preview(piano: bool) {
            use rustel_runtime::samples::{SoundEntry, SoundOrigin};
            let directory = tempfile::tempdir().expect("tempdir");
            let mut app = app_over(directory.path());
            let sounds = vec![
                SoundEntry {
                    name: "bd".into(),
                    variants: 2,
                    variant_names: Vec::new(),
                    origin: SoundOrigin::Default,

                    category: rustel_runtime::samples::SoundCategory::Other,
                    location: None,
                    import: None,
                },
                SoundEntry {
                    name: "hh".into(),
                    variants: 1,
                    variant_names: Vec::new(),
                    origin: SoundOrigin::Default,

                    category: rustel_runtime::samples::SoundCategory::Other,
                    location: None,
                    import: None,
                },
            ];
            app.reference_panel = Some(ReferencePanel::samples(&app.reference, sounds));
            app.focus = Focus::Panel(PanelKind::Reference);
            let source = app.editor().source();
            // Test key routing without queuing piano output preparation ahead
            // of the preview commands on the worker's two-slot queue.
            app.piano.open = piano;
            // Nothing sounding: Space previews the selected row.
            press(&mut app, KeyCode::Char(' '), KeyModifiers::NONE);
            assert_eq!(
                app.preview_armed.as_ref().map(|(sound, _)| sound.as_str()),
                Some("bd:0")
            );
            // Holding/releasing Space must not immediately stop its preview.
            for kind in [KeyEventKind::Repeat, KeyEventKind::Release] {
                app.handle_terminal_event(Event::Key(KeyEvent::new_with_kind(
                    KeyCode::Char(' '),
                    KeyModifiers::NONE,
                    kind,
                )))
                .unwrap();
                assert!(app.preview_armed.is_some());
            }
            // The same row again: that is a stop.
            press(&mut app, KeyCode::Char(' '), KeyModifiers::NONE);
            assert_eq!(app.preview_armed, None, "stopped");
            assert!(app.status.contains("stopped"), "{}", app.status);
            // Another row while one sounds: plays the other (the choke group
            // replaces it), no stop.
            app.preview_armed = Some(("bd:0".into(), Instant::now()));
            if let Some(panel) = app.reference_panel.as_mut() {
                panel.move_by(1);
            }
            press(&mut app, KeyCode::Char(' '), KeyModifiers::NONE);
            // Whether the worker's bounded command queue took the preview or
            // reported itself busy, the other row was asked for and nothing
            // was stopped.
            assert!(app.status.contains("hh:0"), "{}", app.status);
            assert!(!app.status.contains("stopped"), "{}", app.status);
            assert_eq!(app.piano.open, piano);
            assert_eq!(app.editor().source(), source);
        }
    }
}
