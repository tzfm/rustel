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
