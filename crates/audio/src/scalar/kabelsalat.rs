use super::*;

pub(crate) fn kabel_controls<K>(controls: &OscillatorControls<K>) -> impl Iterator<Item = &K> {
    controls.kabel.iter().chain(
        controls
            .fx_stages
            .iter()
            .flatten()
            .filter_map(|stage| stage.kabel.as_ref()),
    )
}

pub(crate) fn kabel_graph_bytes(graph: &rustel_kabel::Graph) -> usize {
    std::mem::size_of::<rustel_kabel::Graph>() + graph.heap_bytes()
}

impl KabelState {
    pub(super) fn process(
        &mut self,
        input: f32,
        quantum_secs: f64,
        sample_rate: f64,
    ) -> (f32, f32) {
        if quantum_secs < self.start_secs || quantum_secs >= self.end_secs {
            return (0.0, 0.0);
        }
        // Worklets receive the left input channel and expose two outputs.
        // Their time starts at the first quantum that passes the start gate.
        let gate = if quantum_secs > self.gate_end_secs {
            0.0
        } else {
            1.0
        };
        let out = self.graph.process([f64::from(input); 2], gate, self.time);
        self.time += 1.0 / sample_rate;
        (out[0] as f32, out[1] as f32)
    }
}

/// Whether a pooled graph can run `program` at `sample_rate`. A graph keeps
/// the rate it was built for, so a graph from before a rate change is not
/// reused.
fn kabel_fits(
    graph: &rustel_kabel::Graph,
    program: &rustel_kabel::Program,
    sample_rate: f64,
) -> bool {
    graph.can_reuse(program) && graph.sample_rate() == sample_rate
}

#[allow(clippy::vec_box)]
pub(super) fn lease_kabel(
    pool: &mut Vec<Box<rustel_kabel::Graph>>,
    programs: &[Box<crate::assets::PreparedKabelProgram>],
    controls: CompactKabelControls,
    sample_rate: f64,
) -> KabelState {
    let program =
        find_kabel_program(programs, controls.program).expect("admitted program is installed");
    let index = pool
        .iter()
        .enumerate()
        .filter(|(_, graph)| kabel_fits(graph, program, sample_rate))
        .min_by_key(|(_, graph)| graph.heap_bytes())
        .map(|(index, _)| index)
        .expect("KabelSalat graph admission reserves every stage");
    let mut graph = pool.swap_remove(index);
    let restarted = graph.restart(program);
    debug_assert!(restarted);
    KabelState {
        graph,
        start_secs: controls.start_secs,
        gate_end_secs: controls.gate_end_secs,
        end_secs: controls.end_secs,
        time: 0.0,
    }
}

fn find_kabel_program(
    programs: &[Box<crate::assets::PreparedKabelProgram>],
    id: rustel_kabel::ProgramId,
) -> Option<&rustel_kabel::Program> {
    programs
        .binary_search_by_key(&id, |entry| entry.program.id())
        .ok()
        .map(|index| &programs[index].program)
}

pub(super) fn kabel_available(
    pool: &[Box<rustel_kabel::Graph>],
    programs: &[Box<crate::assets::PreparedKabelProgram>],
    controls: &OscillatorControls<CompactKabelControls>,
    sample_rate: f64,
) -> bool {
    let mut used = [false; MAX_KABEL_GRAPHS];
    for controls in kabel_controls(controls) {
        let Some(program) = find_kabel_program(programs, controls.program) else {
            return false;
        };
        let index = pool
            .iter()
            .enumerate()
            .filter(|(index, graph)| !used[*index] && kabel_fits(graph, program, sample_rate))
            .min_by_key(|(_, graph)| graph.heap_bytes())
            .map(|(index, _)| index);
        let Some(index) = index else {
            return false;
        };
        used[index] = true;
    }
    true
}

impl ScalarBackend {
    /// Installs a processor prepared off the callback, returning refused storage
    /// to the caller for destruction on the producer thread.
    pub fn install_kabel(
        &mut self,
        graph: Box<rustel_kabel::Graph>,
    ) -> Option<Box<rustel_kabel::Graph>> {
        let bytes = kabel_graph_bytes(&graph);
        if self.kabel_count == MAX_KABEL_GRAPHS
            || bytes > MAX_KABEL_BYTES.saturating_sub(self.kabel_bytes)
        {
            return Some(graph);
        }
        // Pre-init installs are a producer operation. Init reserves the same
        // full return capacity before live callback delivery starts.
        if self.kabel_pool.capacity() < MAX_KABEL_GRAPHS {
            self.kabel_pool
                .reserve(MAX_KABEL_GRAPHS - self.kabel_pool.len());
        }
        self.kabel_bytes += bytes;
        self.kabel_count += 1;
        self.kabel_pool.push(graph);
        None
    }

    pub(crate) fn has_only_idle_kabel(&self) -> bool {
        self.kabel_count != 0 && self.kabel_pool.len() == self.kabel_count
    }

    /// Returns storage only when no active or queued graph can need it.
    /// Call after protecting all external queues and scanning pending onsets.
    pub(crate) fn retire_idle_kabel(
        &mut self,
        max_returns: usize,
        mut retire: impl FnMut(Box<rustel_kabel::Graph>),
    ) {
        if self.kabel_queued || !self.has_only_idle_kabel() {
            return;
        }
        for _ in 0..max_returns.min(self.kabel_pool.len()) {
            let graph = self.kabel_pool.pop().expect("idle processor");
            self.kabel_bytes -= kabel_graph_bytes(&graph);
            self.kabel_count -= 1;
            retire(graph);
        }
    }

    pub fn kabel_resident_bytes(&self) -> usize {
        self.kabel_bytes
    }

    /// Onsets refused because their complete worklet chain was not prepared.
    pub fn missing_kabel_events(&self) -> u64 {
        self.missing_kabel_events
    }

    #[cfg(test)]
    pub(crate) fn with_kabel_program_bank_for_test(capacity: usize) -> Self {
        Self {
            kabel_programs: Vec::with_capacity(capacity),
            kabel_program_protected: vec![false; capacity],
            ..Self::default()
        }
    }

    /// The immutable bank retains code while compact onsets wait for activation.
    /// Returned boxes must be destroyed by the producer, never by the callback.
    pub(crate) fn install_kabel_program(
        &mut self,
        mut entry: Box<crate::assets::PreparedKabelProgram>,
        mut retire: impl FnMut(Box<crate::assets::PreparedKabelProgram>),
    ) -> Option<Box<crate::assets::PreparedKabelProgram>> {
        let index = match self
            .kabel_programs
            .binary_search_by_key(&entry.program.id(), |old| old.program.id())
        {
            Ok(index) => {
                entry.until_frame = entry
                    .until_frame
                    .max(self.kabel_programs[index].until_frame);
                retire(std::mem::replace(&mut self.kabel_programs[index], entry));
                return None;
            }
            Err(index) => index,
        };
        if self.kabel_programs.len() == self.kabel_programs.capacity() {
            return Some(entry);
        }
        self.kabel_programs.insert(index, entry);
        None
    }

    pub(crate) fn kabel_program_resident_bytes(&self) -> usize {
        self.kabel_programs.len() * std::mem::size_of::<crate::assets::PreparedKabelProgram>()
    }

    pub(crate) fn has_expired_kabel_programs(&self) -> bool {
        self.kabel_programs
            .iter()
            .any(|entry| entry.until_frame <= self.frame)
    }

    pub(crate) fn begin_kabel_program_retirement(&mut self) {
        self.kabel_program_protected.fill(false);
        self.kabel_queued = false;
    }

    pub(crate) fn protect_kabel_program(&mut self, id: rustel_kabel::ProgramId) {
        self.kabel_queued = true;
        if let Ok(index) = self
            .kabel_programs
            .binary_search_by_key(&id, |entry| entry.program.id())
        {
            self.kabel_program_protected[index] = true;
        }
    }

    pub(crate) fn retire_kabel_programs(
        &mut self,
        max_returns: usize,
        mut retire: impl FnMut(Box<crate::assets::PreparedKabelProgram>),
    ) {
        for event in self
            .pending
            .slots
            .iter()
            .filter_map(|slot| slot.event.as_ref())
        {
            for controls in kabel_controls(&event.controls) {
                self.kabel_queued = true;
                if let Ok(index) = self
                    .kabel_programs
                    .binary_search_by_key(&controls.program, |entry| entry.program.id())
                {
                    self.kabel_program_protected[index] = true;
                }
            }
        }
        let mut index = self.kabel_programs.len();
        let mut returned = 0;
        // Remove backwards to preserve sorted lookup and protection indices.
        while index > 0 && returned < max_returns {
            index -= 1;
            if self.kabel_programs[index].until_frame <= self.frame
                && !self.kabel_program_protected[index]
            {
                retire(self.kabel_programs.remove(index));
                returned += 1;
            }
        }
    }

    fn prepare_kabel_code(&mut self, controls: &OscillatorControls) -> Result<(), String> {
        self.kabel_program_protected
            .resize(crate::assets::MAX_KABEL_PROGRAMS, false);
        if self.kabel_programs.capacity() < crate::assets::MAX_KABEL_PROGRAMS {
            self.kabel_programs
                .reserve(crate::assets::MAX_KABEL_PROGRAMS - self.kabel_programs.len());
        }
        for controls in kabel_controls(controls) {
            let until_frame =
                (controls.end_secs.max(0.0) * f64::from(self.sample_rate)).ceil() as u64;
            let until_frame = until_frame.saturating_add(256);
            if let Some(entry) = self
                .kabel_programs
                .iter_mut()
                .find(|entry| entry.program.id() == controls.program.id())
            {
                entry.until_frame = entry.until_frame.max(until_frame);
                continue;
            }
            let entry = Box::new(crate::assets::PreparedKabelProgram {
                program: *controls.program,
                until_frame,
            });
            if self.install_kabel_program(entry, drop).is_some() {
                return Err(
                    "KabelSalat immutable program storage exceeds the 4096-program limit".into(),
                );
            }
        }
        Ok(())
    }

    /// Prepares independent state for every worklet in a voice. Producer only:
    /// graph construction allocates. Repeated calls reuse compatible idle state.
    pub fn prepare_kabel(&mut self, controls: &OscillatorControls) -> Result<(), String> {
        self.prepare_kabel_code(controls)?;
        let programs: Vec<_> = kabel_controls(controls)
            .map(|kabel| kabel.program.as_ref())
            .collect();
        self.prepare_kabel_programs(&programs)
    }

    fn prepare_kabel_programs(
        &mut self,
        programs: &[&rustel_kabel::Program],
    ) -> Result<(), String> {
        let rate = f64::from(self.sample_rate);
        let mut used = [false; MAX_KABEL_GRAPHS];
        let mut protected = [false; MAX_KABEL_GRAPHS];
        // Previously admitted future events retain their capacity even when
        // this admission concerns a disjoint interval.
        for event in self
            .pending
            .slots
            .iter()
            .filter_map(|slot| slot.event.as_ref())
        {
            for controls in kabel_controls(&event.controls) {
                let Some(program) = self
                    .kabel_programs
                    .iter()
                    .find(|entry| entry.program.id() == controls.program)
                    .map(|entry| &entry.program)
                else {
                    protected.fill(true);
                    continue;
                };
                if let Some(index) = self
                    .kabel_pool
                    .iter()
                    .enumerate()
                    .filter(|(index, graph)| !protected[*index] && kabel_fits(graph, program, rate))
                    .min_by_key(|(_, graph)| graph.heap_bytes())
                    .map(|(index, _)| index)
                {
                    protected[index] = true;
                }
            }
        }
        let mut programs = programs.to_vec();
        programs.sort_unstable_by_key(|program| std::cmp::Reverse(program.delay_count()));
        for program in programs {
            let index = self
                .kabel_pool
                .iter()
                .enumerate()
                .filter(|(index, graph)| !used[*index] && kabel_fits(graph, program, rate))
                .min_by_key(|(_, graph)| graph.heap_bytes())
                .map(|(index, _)| index);
            let index = match index {
                Some(index) => index,
                None => {
                    let graph = Box::new(rustel_kabel::Graph::new(
                        program,
                        f64::from(self.sample_rate),
                    )?);
                    let bytes = kabel_graph_bytes(&graph);
                    while self.kabel_count == MAX_KABEL_GRAPHS
                        || bytes > MAX_KABEL_BYTES.saturating_sub(self.kabel_bytes)
                    {
                        let Some(index) = (0..self.kabel_pool.len())
                            .find(|&index| !used[index] && !protected[index])
                        else {
                            return Err(
                                "KabelSalat graph storage exceeds the 128-graph or 64 MiB limit"
                                    .into(),
                            );
                        };
                        let last = self.kabel_pool.len() - 1;
                        let old = self.kabel_pool.swap_remove(index);
                        used.swap(index, last);
                        protected.swap(index, last);
                        used[last] = false;
                        protected[last] = false;
                        self.kabel_bytes -= kabel_graph_bytes(&old);
                        self.kabel_count -= 1;
                        // Offline admission owns destruction on its caller.
                        drop(old);
                    }
                    if self.install_kabel(graph).is_some() {
                        return Err(
                            "KabelSalat graph storage exceeds the 128-graph or 64 MiB limit".into(),
                        );
                    }
                    self.kabel_pool.len() - 1
                }
            };
            used[index] = true;
        }
        Ok(())
    }

    /// Offline admission prepares the peak overlap, rather than retaining one
    /// processor for every event in a long score. The extra quantum covers
    /// callback retirement after a worklet's final quantum.
    pub fn prepare_kabel_event(&mut self, event: &OnsetEvent) -> Result<(), String> {
        if kabel_controls(&event.controls).next().is_none() {
            return Ok(());
        }
        self.prepare_kabel_code(&event.controls)?;
        let compact = event.clone().map_kabel(CompactKabelControls::from);
        let sr = f64::from(self.sample_rate);
        let interval = |event: &OnsetEvent<CompactKabelControls>| {
            let begin = event.onset_frame as f64 / sr;
            let end = kabel_controls(&event.controls)
                .map(|kabel| kabel.end_secs)
                .fold(
                    begin + f64::from(event.duration_secs + event.controls.envelope.release_secs),
                    f64::max,
                );
            (begin, (end * sr / 128.0).ceil() * 128.0 / sr + 128.0 / sr)
        };
        let (begin, end) = interval(&compact);
        let mut times = vec![begin];
        for slot in &self.pending.slots {
            if let Some(pending) = &slot.event {
                let (start, _) = interval(pending);
                if start >= begin && start < end {
                    times.push(start);
                }
            }
        }
        times.sort_unstable_by(f64::total_cmp);
        times.dedup();
        for time in times {
            let mut programs: Vec<rustel_kabel::Program> = kabel_controls(&event.controls)
                .map(|kabel| *kabel.program)
                .collect();
            for slot in &self.pending.slots {
                if let Some(pending) = &slot.event {
                    let (start, stop) = interval(pending);
                    if start <= time && time < stop {
                        programs.extend(kabel_controls(&pending.controls).filter_map(|kabel| {
                            find_kabel_program(&self.kabel_programs, kabel.program).copied()
                        }));
                    }
                }
            }
            self.prepare_kabel_programs(&programs.iter().collect::<Vec<_>>())?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    mod kabel_capacity_tests {
        use super::*;

        fn program(delays: usize) -> rustel_kabel::Program {
            let mut nodes = vec![
                r#"{"type":"n","value":0.25}"#.to_owned(),
                r#"{"type":"n","value":0.001}"#.to_owned(),
            ];
            for _ in 0..delays {
                nodes.push(r#"{"type":"delay","inputs":[0,1]}"#.into());
            }
            nodes.push(format!(
                r#"{{"type":"output","inputs":[{}]}}"#,
                if delays == 0 { 0 } else { nodes.len() - 1 }
            ));
            rustel_kabel::Program::from_json(&format!(r#"{{"nodes":[{}]}}"#, nodes.join(",")))
                .unwrap()
        }

        fn full_idle_pool() -> ScalarBackend {
            let mut backend = ScalarBackend {
                sample_rate: 1000,
                ..ScalarBackend::default()
            };
            let code = program(0);
            for _ in 0..MAX_KABEL_GRAPHS {
                assert!(
                    backend
                        .install_kabel(Box::new(rustel_kabel::Graph::new(&code, 1000.0).unwrap()))
                        .is_none()
                );
            }
            backend
        }

        #[test]
        fn offline_idle_capacity_recovers_for_larger_graphs() {
            let mut backend = full_idle_pool();
            let delay = program(1);
            backend
                .prepare_kabel_programs(&[&delay])
                .expect("idle capacity is replaceable");
            assert_eq!(backend.kabel_count, MAX_KABEL_GRAPHS);
            assert!(
                backend
                    .kabel_pool
                    .iter()
                    .any(|graph| graph.can_reuse(&delay))
            );
        }

        #[test]
        fn offline_eviction_preserves_used_slots_when_pool_moves() {
            let mut backend = full_idle_pool();
            let two = program(2);
            let one = program(1);
            backend.prepare_kabel_programs(&[&two]).unwrap();
            backend.prepare_kabel_programs(&[&two, &one, &two]).unwrap();
            for code in [&two, &two, &one] {
                let index = backend
                    .kabel_pool
                    .iter()
                    .position(|graph| graph.can_reuse(code))
                    .expect("independent state for every stage");
                let graph = backend.kabel_pool.swap_remove(index);
                assert!(graph.can_reuse(code));
            }
        }

        #[test]
        fn offline_eviction_keeps_previously_admitted_future_capacity() {
            let mut backend = full_idle_pool();
            let plain = program(0);
            let mut future = OnsetEvent::new(100_000, 440.0, 0.5, 1.0);
            future.controls.kabel = Some(crate::KabelControls {
                program: plain.into(),
                start_secs: 100.0,
                gate_end_secs: 101.0,
                end_secs: 101.0,
            });
            backend.prepare_kabel_code(&future.controls).unwrap();
            for _ in 0..MAX_KABEL_GRAPHS {
                backend
                    .pending
                    .push(future.clone().map_kabel(CompactKabelControls::from));
            }
            let delay = program(1);
            assert!(backend.prepare_kabel_programs(&[&delay]).is_err());
            assert_eq!(backend.kabel_pool.len(), MAX_KABEL_GRAPHS);
            assert!(
                backend
                    .kabel_pool
                    .iter()
                    .all(|graph| graph.can_reuse(&plain))
            );
        }
    }
}
