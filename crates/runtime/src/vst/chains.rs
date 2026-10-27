//! The plugin chains of a score, each with its number for the audio engine
//! and the host, and the plugin copies their notes need.

use std::cmp::Reverse;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use rustel_audio::{InsertChains, InsertKey, InsertParam, InsertSlot, InsertTurn, InsertValue};
use rustel_vst3::WantedCopy;

use super::lanes::ScoreLanes;
use super::*;

/// The lane of a chain owner.
#[derive(Clone, Debug, Hash, PartialEq, Eq)]
enum OwnerLane {
    /// The number of a lane of the lane table: see [`LaneTable`]. A note of
    /// a labeled statement carries the number as the tag `#n`. A note with
    /// no tag of a score with no label has the lane of the empty label.
    Lane(u64),
    /// The tag `#n` of a note of a text whose label rewrite wrote places:
    /// the place of the statement among the labeled statements. See
    /// [`ScoreLanes::places`].
    Place(u64),
    /// Each other tag: the run-time name of a lane with no statement.
    Name(String),
    /// A note with no tag of a score with labels, of a direct pattern or of
    /// a render: the orbit of the note.
    NoLane(u8),
}

impl OwnerLane {
    /// The lane of a note. `lanes` are the lanes of the text of the note,
    /// and `None` for a note of a render.
    fn of(value: &serde_json::Value, lanes: Option<&ScoreLanes>) -> Self {
        let tag = value
            .get("__rustel_lane")
            .and_then(serde_json::Value::as_str);
        let Some(tag) = tag else {
            let mut lanes = lanes.into_iter().flat_map(ScoreLanes::lanes);
            return match lanes.find(|(_, label, ..)| label.is_empty()) {
                Some((number, ..)) => Self::Lane(number),
                None => Self::NoLane(orbit(value)),
            };
        };
        match tag.strip_prefix('#').and_then(|number| number.parse().ok()) {
            Some(place) if lanes.is_some_and(ScoreLanes::places) => Self::Place(place),
            Some(number) => Self::Lane(number),
            None => Self::Name(tag.to_owned()),
        }
    }

    /// True for a lane the score names at run time: a name, or a place.
    /// See [`ChainRegistry::grown`].
    fn run_time(&self) -> bool {
        matches!(self, Self::Name(_) | Self::Place(_))
    }

    /// The number of a lane of the lane table.
    fn number(&self) -> Option<u64> {
        match self {
            Self::Lane(number) => Some(*number),
            _ => None,
        }
    }
}

/// The orbit of a note, as the voice reads it: 1 with no `orbit`.
fn orbit(value: &serde_json::Value) -> u8 {
    let orbit = value.get("orbit").and_then(serde_json::Value::as_f64);
    let last = (rustel_audio::MAX_ORBITS - 1) as f64;
    orbit.unwrap_or(1.0).clamp(0.0, last) as u8
}

/// A chain owns its copies independently of the values of its notes. Only a
/// note with no lane has its orbit in its owner: see [`OwnerLane::NoLane`].
#[derive(Clone, Debug, Hash, PartialEq, Eq)]
struct ChainOwner {
    lane: OwnerLane,
    plugins: [Option<PluginOwner>; rustel_audio::EFFECT_CHAIN + 1],
}

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
struct PluginOwner {
    binding: Option<String>,
    name: String,
    preset: Option<String>,
}

impl PluginOwner {
    fn from_value(value: &serde_json::Value) -> Option<Self> {
        Some(Self {
            binding: value
                .get("__rustel_binding")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned),
            name: value.get("name")?.as_str()?.to_owned(),
            preset: preset_name(value),
        })
    }

    /// True for a call of the same plugin with the same preset: one copy
    /// serves the two calls.
    fn same_plugin(&self, other: &Self) -> bool {
        self.same_name(other) && self.preset == other.preset
    }

    /// True for a call of the same plugin, with any preset.
    fn same_name(&self, other: &Self) -> bool {
        canonical(&self.name) == canonical(&other.name)
    }
}

fn preset_name(value: &serde_json::Value) -> Option<String> {
    match value.get("preset")? {
        serde_json::Value::String(name) => Some(name.clone()),
        serde_json::Value::Number(number) => Some(number.to_string()),
        _ => None,
    }
}

impl ChainOwner {
    /// The owner of the chain of a note with a plugin. `lanes` are the lanes
    /// of the text of the note: see [`OwnerLane::of`].
    fn from_value(value: &serde_json::Value, lanes: Option<&ScoreLanes>) -> Option<Self> {
        let mut plugins = std::array::from_fn(|_| None);
        if let Some(effects) = value.get("vst").and_then(serde_json::Value::as_array) {
            for (slot, plugin) in plugins
                .iter_mut()
                .take(rustel_audio::EFFECT_CHAIN)
                .zip(effects)
            {
                *slot = PluginOwner::from_value(plugin);
            }
        }
        plugins[rustel_audio::EFFECT_CHAIN] = value.get("vsti").and_then(PluginOwner::from_value);
        plugins.iter().any(Option::is_some).then(|| Self {
            lane: OwnerLane::of(value, lanes),
            plugins,
        })
    }

    /// True when `later` is this chain with zero or more effects at its
    /// end. Each copy of this chain then serves `later` on its stage: see
    /// [`seat`].
    fn grows_to(&self, later: &Self) -> bool {
        let mut stages = self.plugins.iter().zip(&later.plugins).enumerate();
        stages.all(|(stage, pair)| match pair {
            (Some(mine), Some(theirs)) => mine.same_plugin(theirs),
            (None, None) => true,
            (None, Some(_)) => stage < rustel_audio::EFFECT_CHAIN,
            (Some(_), None) => false,
        })
    }

    /// How well this chain fits `later`, an owner of the same lane with a
    /// new text, or `None` with no plugin in common. More is better: the
    /// same instrument and preset, the same instrument, then the effects
    /// whose copies the chain keeps, in any order: see [`seat`].
    fn fit(&self, later: &Self) -> Option<(bool, bool, usize)> {
        if self.lane != later.lane {
            return None;
        }
        let theirs = || later.plugins.iter().flatten();
        let shared = |mine: &PluginOwner| theirs().any(|theirs| mine.same_name(theirs));
        if !self.plugins.iter().flatten().any(shared) {
            return None;
        }
        let instrument = rustel_audio::EFFECT_CHAIN;
        let (same, named) = match (&self.plugins[instrument], &later.plugins[instrument]) {
            (Some(mine), Some(theirs)) => (mine.same_plugin(theirs), mine.same_name(theirs)),
            _ => (false, false),
        };
        // A copy serves one call of its plugin and preset.
        let mut copies: Vec<&PluginOwner> = self.plugins[..instrument].iter().flatten().collect();
        let effects = later.plugins[..instrument].iter().flatten();
        let kept = effects.filter(|theirs| {
            let copy = copies.iter().position(|mine| mine.same_plugin(theirs));
            copy.map(|copy| copies.swap_remove(copy)).is_some()
        });
        Some((same, named, kept.count()))
    }

    /// The plugin calls of the owner on the chain `chain`, each with the
    /// slot of its copy: see [`slot_of`].
    fn copies(
        &self,
        chain: u64,
        stages: [u8; rustel_audio::EFFECT_CHAIN],
    ) -> impl Iterator<Item = (InsertSlot, &PluginOwner)> {
        let calls = self.plugins.iter().enumerate();
        calls.filter_map(move |(place, plugin)| {
            let plugin = plugin.as_ref()?;
            Some((slot_of(chain, stages, place), plugin))
        })
    }

    /// The first plugin call of the owner: see [`call_order`].
    fn call(&self) -> (&str, u64) {
        let plugins = self.plugins.iter().flatten();
        let bindings = plugins.filter_map(|plugin| plugin.binding.as_deref());
        bindings.map(call_order).min().unwrap_or(("", u64::MAX))
    }
}

/// The order of a plugin call among the calls of a score. The binding of a
/// call is the path of its scope, `p` and the number of the call in the
/// scope.
fn call_order(binding: &str) -> (&str, u64) {
    match binding.rsplit_once('p') {
        Some((scope, number)) => (scope, number.parse().unwrap_or(u64::MAX)),
        None => (binding, u64::MAX),
    }
}

/// A chain that left the score text has this much play before it ends: see
/// [`Chains::tick`]. In this time the new score takes each chain it still
/// has.
const CHAIN_GRACE: Duration = Duration::from_secs(5);
/// A chain with a plugin the body of its lane does not name ends after this
/// much play with no note: a chain of a lane with no statement, of no lane,
/// or with a plugin from a variable or a helper. See [`Chains::tick`].
const CHAIN_UNSEEN_LIMIT: Duration = Duration::from_secs(600);

/// A chain of a registry. Each time is a time of the clock of the chains:
/// see [`Chains::tick`].
#[derive(Clone, PartialEq)]
struct Chain {
    id: u64,
    /// The stage of each effect call of the owner, in call order: see
    /// [`seat`].
    stages: [u8; rustel_audio::EFFECT_CHAIN],
    /// The count of all uses at the last use of this chain: the order.
    used: u64,
    /// The score change at the last use.
    epoch: u64,
    /// The time of the last use.
    seen: Duration,
    /// The time the lane of the chain left the score in play: see
    /// [`ChainRegistry::score_changed`].
    left: Option<Duration>,
    /// The time the body of the lane lost the plugin of each call of the
    /// owner, until the body names it again or a note uses the chain: see
    /// [`ChainRegistry::score_changed`].
    lost: [Option<Duration>; rustel_audio::EFFECT_CHAIN + 1],
    /// The keys the chain lost: the copy of each stage where an owner took
    /// the chain with another plugin or preset, or with no call, with the
    /// time of the take. See [`ChainRegistry::take`]. The host keeps such a
    /// copy [`CHAIN_GRACE`] more: see [`Chains::wanted`].
    dropped: Vec<(WantedCopy, Duration)>,
}

impl Chain {
    /// The start of the grace of the chain, or `None` for a chain with no
    /// grace. The chain ends [`CHAIN_GRACE`] later. Its lane left the score,
    /// or the body of its lane lost its instrument or the plugin of each
    /// call. `owner` is the owner of the chain.
    fn grace(&self, owner: &ChainOwner) -> Option<Duration> {
        let calls = owner.plugins.iter().zip(self.lost);
        let mut marks = calls.filter_map(|(plugin, lost)| plugin.as_ref().map(|_| lost));
        // The last loss, when each call with a plugin lost it.
        let last = marks.try_fold(Duration::ZERO, |last, lost| Some(last.max(lost?)));
        let instrument = self.lost[rustel_audio::EFFECT_CHAIN];
        let starts = [self.left, instrument, last];
        starts.into_iter().flatten().min()
    }
}

/// A new chain number. One count serves each registry of the process, so
/// the host never has one number for two chains.
fn new_chain() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(rustel_audio::MAX_ORBITS as u64);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

/// The slot of the plugin call at `place` of an owner of the chain `chain`:
/// an effect on its stage of `stages`, or the instrument after the effects.
/// The call at `place` has a name.
fn slot_of(chain: u64, stages: [u8; rustel_audio::EFFECT_CHAIN], place: usize) -> InsertSlot {
    match stages.get(place) {
        Some(stage) => rustel_audio::effect_slot(chain, usize::from(*stage)),
        None => rustel_audio::instrument_slot(chain),
    }
}

/// The stage of each effect call of `owner` on a chain, in call order, and
/// [`NO_STAGE`](rustel_audio::NO_STAGE) for a call with no name. `units` are
/// the effect calls of the last owner of the chain, each with its stage, in
/// call order: none for a new chain. `dropped` are the keys the chain lost
/// in the last [`CHAIN_GRACE`]: see [`Chain::dropped`]. The engine runs the
/// copy of such a key on its stage until the next note of the chain. A call
/// sits on the stage of a unit of its plugin or on no unit, and a unit holds
/// one call. These rules pick the seat, each rule on a tie of the rule
/// before:
///
/// ```text
///   1. the most calls on a unit with their plugin and preset: each such
///      stage keeps its copy
///   2. the most calls on a unit of their plugin: a new preset keeps the
///      stage
///   3. the fewest pairs of calls on units in the reverse order: the stages
///      keep their order, and only calls in a new order change it
///   4. the first units: the k-th call of a plugin takes the k-th unit
/// ```
///
/// A call on no unit takes the free stage of a dropped key with its plugin
/// and preset, so an undo keeps the copy. Else it takes the lowest free
/// stage that no unit and no dropped key holds. So the stage of an effect
/// that leaves keeps its place for the fade, also through a second edit
/// before the next note. With no such stage, the call takes the lowest free
/// stage that no unit holds: the engine empties it at the next note. With
/// none, the call takes the lowest free stage. A new chain has the stages 0,
/// 1, 2 and 3 in call order, and effects in a new order keep their stages.
/// The engine takes the order of the stages from the note.
fn seat(
    units: &[(&PluginOwner, u8)],
    dropped: &[CopyName<'_>],
    owner: &ChainOwner,
) -> [u8; rustel_audio::EFFECT_CHAIN] {
    const CALLS: usize = rustel_audio::EFFECT_CHAIN;
    let calls = &owner.plugins[..CALLS];
    // Each number below base^CALLS is one choice. Its digit k in this base
    // is the place in `units` of the unit of call k, or `none`.
    let (none, base) = (units.len(), units.len() + 1);
    let choices = (0..base.pow(CALLS as u32)).map(|number| -> [usize; CALLS] {
        std::array::from_fn(|call| number / base.pow(call as u32) % base)
    });
    let fits = |choice: &[usize; CALLS]| {
        let mut picks = calls.iter().zip(choice).enumerate();
        picks.all(|(place, (call, unit))| {
            let named = |call: &PluginOwner| units[*unit].0.same_name(call);
            *unit == none || (!choice[..place].contains(unit) && call.as_ref().is_some_and(named))
        })
    };
    let rank = |choice: &[usize; CALLS]| {
        let seated = || calls.iter().zip(choice).filter(|(_, unit)| **unit < none);
        let copies = seated().filter(|(call, unit)| {
            let copy = |call: &PluginOwner| units[**unit].0.same_plugin(call);
            call.as_ref().is_some_and(copy)
        });
        // The pairs of seated calls on units in the reverse order.
        let reversed = (0..CALLS).flat_map(|place| {
            let later = choice[place + 1..].iter();
            later.filter(move |unit| **unit < choice[place] && choice[place] < none)
        });
        let (copies, seated, reversed) = (copies.count(), seated().count(), reversed.count());
        (copies, seated, Reverse(reversed), Reverse(*choice))
    };
    let best = choices.filter(fits).max_by_key(rank);
    let choice = best.expect("the choice of no unit");
    let mut stages = [rustel_audio::NO_STAGE; CALLS];
    let mut free = [true; CALLS];
    for (stage, unit) in stages.iter_mut().zip(choice) {
        if let Some((_, at)) = units.get(unit) {
            (*stage, free[usize::from(*at)]) = (*at, false);
        }
    }
    let unit = |stage: &usize| units.iter().any(|(_, at)| usize::from(*at) == *stage);
    let key = |stage: &usize| {
        dropped
            .iter()
            .any(|(slot, ..)| usize::from(slot.stage) == *stage)
    };
    for (call, stage) in calls.iter().zip(&mut stages) {
        if let Some(call) = call
            && *stage == rustel_audio::NO_STAGE
        {
            // A dropped key with the plugin and preset of the call.
            let copy = |at: &usize| {
                dropped.iter().any(|(slot, name, preset)| {
                    usize::from(slot.stage) == *at
                        && canonical(name) == canonical(&call.name)
                        && *preset == call.preset.as_deref()
                })
            };
            let open = (0..CALLS).filter(|at| free[*at]);
            let lowest = open
                .min_by_key(|at| (!copy(at), unit(at) || key(at), unit(at), *at))
                .expect("a stage for each call");
            (*stage, free[lowest]) = (lowest as u8, false);
        }
    }
    stages
}

/// A plugin copy by name, as the chains have it: the slot, the plugin and
/// the preset.
type CopyName<'a> = (InsertSlot, &'a str, Option<&'a str>);

/// The copy of a plugin with a preset at a slot, by name, as the host keeps
/// it: see [`Host::keep`].
fn wanted_copy((slot, name, preset): CopyName<'_>) -> WantedCopy {
    WantedCopy {
        slot,
        name: name.to_owned(),
        preset: preset.map(str::to_owned),
    }
}

/// The chains of a score, each with its number for the audio engine and
/// the host.
///
/// An edit of a score often gives a chain a new owner text and leaves the
/// chain as it was: a plugin call above moves the call number, a new preset
/// changes one copy, and a `.p("$")` call above moves the name of a lane
/// with no statement. A new copy of each plugin for such an edit costs
/// memory and load time, and the old copy plays on for a time. So the notes
/// of a batch take their chains in one assignment, and an owner with a new
/// text takes a chain that the score did not use since its last change: see
/// [`Self::assign`].
///
/// A chain of a lane that left the text ends after [`CHAIN_GRACE`] of play,
/// and so does a chain whose lane body lost its plugins: see
/// [`Self::settle`].
///
/// Each session has a registry for its score in play, and the plan of a
/// session is a registry too: see [`Chains`]. Each render to a file has a
/// registry of its own: see [`render_chains`].
#[derive(Clone, PartialEq)]
struct ChainRegistry {
    owners: HashMap<ChainOwner, Chain>,
    accessed: u64,
    /// The count of score changes.
    epoch: u64,
    /// The plugin names in the body of each lane of the score, by lane
    /// number. A lane that left keeps the names of its last body while a
    /// chain has the lane: see [`Self::leave`].
    bodies: HashMap<u64, Vec<String>>,
}

/// What one assignment gives: see [`ChainRegistry::assign`].
struct Assignment {
    /// The seat of each owner, in the order of the owners. `None` for an
    /// owner with no room.
    seats: Vec<Option<Seat>>,
    /// The chains that left the registry to make room. The host ends such a
    /// chain when no registry of the session has it: see [`keep_now`].
    ended: Vec<u64>,
    /// The lane of the newest chain, when the registry has more chains than
    /// room and no chain ends to make room. The list of the output leaves
    /// that chain out.
    cut: Option<OwnerLane>,
}

impl ChainRegistry {
    fn new() -> Self {
        Self {
            owners: HashMap::new(),
            accessed: 0,
            epoch: 0,
            bodies: HashMap::new(),
        }
    }

    /// A new score plays: `lanes` are the lanes of its text. A chain of a
    /// lane that left the text ends [`CHAIN_GRACE`] from now, and a chain
    /// of a lane that came back stays. A chain whose lane body lost its
    /// instrument, or the plugin of each call, ends [`CHAIN_GRACE`] from
    /// now too: see [`Chain::grace`]. The chain stays when the body names
    /// the plugin again, or when a note uses the chain: such a note takes
    /// the plugin from a variable or a helper. The chains of the score
    /// before stay for the new score to take.
    fn score_changed(&mut self, lanes: &ScoreLanes, now: Duration) {
        self.epoch += 1;
        self.leave(lanes, now);
    }

    /// Marks each chain of a lane with no statement in `lanes` with the time
    /// the lane left, and clears the mark of each chain of a lane in
    /// `lanes`. Marks each call of a chain whose plugin the body of the
    /// lane named before and does not name now, and clears the mark of each
    /// call whose plugin the body names.
    fn leave(&mut self, lanes: &ScoreLanes, now: Duration) {
        let bodies: HashMap<u64, &[String]> = lanes
            .lanes()
            .map(|(number, .., plugins)| (number, plugins))
            .collect();
        for (owner, chain) in &mut self.owners {
            let Some(number) = owner.lane.number() else {
                continue;
            };
            let Some(names) = bodies.get(&number) else {
                chain.left = chain.left.or(Some(now));
                continue;
            };
            chain.left = None;
            let before = self.bodies.get(&number);
            for (plugin, lost) in owner.plugins.iter().zip(&mut chain.lost) {
                let Some(plugin) = plugin else {
                    continue;
                };
                let name = canonical(&plugin.name);
                if names.contains(&name) {
                    *lost = None;
                } else if before.is_some_and(|before| before.contains(&name)) {
                    lost.get_or_insert(now);
                }
            }
        }
        // A lane that comes back compares its body with the body it left
        // with.
        let chained: HashSet<u64> = self
            .owners
            .keys()
            .filter_map(|owner| owner.lane.number())
            .collect();
        self.bodies.retain(|number, _| chained.contains(number));
        let bodies = bodies
            .into_iter()
            .map(|(number, names)| (number, names.to_vec()));
        self.bodies.extend(bodies);
    }

    /// Gives each owner of a batch of notes its chain, in one assignment.
    /// `owners` are the owners of the notes, each one time, in the order of
    /// their first note, with true when that note plays. `room` is the most
    /// chains the registry holds. An owner takes the chain of the first step
    /// that gives one:
    ///
    /// ```text
    ///   1. its own chain
    ///   2. for an owner of a run-time lane: an unused chain of a run-time
    ///      lane that grows to it, see Self::grown
    ///   3. an unused chain of its lane with a plugin in common, see
    ///      Self::pair
    ///   4. a new chain, while the registry has room
    /// ```
    ///
    /// An unused chain has no note since the last score change. An owner
    /// with its own chain keeps the stages of the chain, and an owner that
    /// takes the chain of another owner sits on its units: see [`seat`].
    /// With no room, `spare` gives the chain that ends to make room. An
    /// owner gets no chain when `spare` gives none. With more chains than
    /// room and no chain that ends, the owner of each newest chain above
    /// the room gets no chain too: the list of the output leaves that chain
    /// out.
    fn assign(
        &mut self,
        owners: &[(ChainOwner, bool)],
        room: usize,
        now: Duration,
        spare: fn(&Self) -> Option<ChainOwner>,
    ) -> Assignment {
        let touched = owners.iter().map(|(owner, _)| self.touch(owner, now));
        let mut seats: Vec<Option<Seat>> = touched.collect();
        for ((owner, sounds), seat) in owners.iter().zip(&mut seats) {
            if seat.is_none()
                && owner.lane.run_time()
                && let Some(old) = self.grown(owner, *sounds)
            {
                *seat = Some(self.take(&old, owner, now));
            }
        }
        self.pair(owners, &mut seats, now);
        let (mut ended, mut cut) = (Vec::new(), None);
        // A Csound chain that sounds now takes a place of the room.
        while self.owners.len() > room {
            let Some(old) = spare(self) else {
                // The list of the output leaves out the newest chains above
                // the room. Their notes play with no plugin, so they ask the
                // output for no bus.
                let mut ids: Vec<u64> = self.owners.values().map(|chain| chain.id).collect();
                ids.sort_unstable();
                let over = &ids[room..];
                for seat in &mut seats {
                    if seat.is_some_and(|seat| over.contains(&seat.chain)) {
                        *seat = None;
                    }
                }
                let newest = self.owners.iter().max_by_key(|(_, chain)| chain.id);
                cut = newest.map(|(owner, _)| owner.lane.clone());
                break;
            };
            ended.extend(self.owners.remove(&old).map(|gone| gone.id));
        }
        for ((owner, _), seat) in owners.iter().zip(&mut seats) {
            if seat.is_some() {
                continue;
            }
            if self.owners.len() >= room {
                let Some(old) = spare(self) else {
                    continue;
                };
                ended.extend(self.owners.remove(&old).map(|gone| gone.id));
            }
            *seat = Some(self.insert(owner, now));
        }
        Assignment { seats, ended, cut }
    }

    /// The seat of `owner` on its chain, in use now: its use, its score
    /// change and its time. A use ends the mark of each plugin the body of
    /// the lane lost: the owner has a plugin at each stage of the chain.
    /// `None` when the registry has no chain of the owner.
    fn touch(&mut self, owner: &ChainOwner, now: Duration) -> Option<Seat> {
        let chain = self.owners.get_mut(owner)?;
        self.accessed += 1;
        (chain.used, chain.epoch) = (self.accessed, self.epoch);
        (chain.seen, chain.lost) = (now, Default::default());
        Some(Seat {
            chain: chain.id,
            stages: chain.stages,
        })
    }

    /// Gives `owner` the chain of `old`, in use now. Each effect of `owner`
    /// sits on a stage of the chain: see [`seat`]. The copy of each stage
    /// and of the instrument where `owner` has another plugin or preset, or
    /// no call, goes to the keys the chain lost: see [`Chain::dropped`].
    fn take(&mut self, old: &ChainOwner, owner: &ChainOwner, now: Duration) -> Seat {
        let mut chain = self.owners.remove(old).expect("a chain of the registry");
        let effects = old.plugins.iter().zip(chain.stages);
        let units: Vec<(&PluginOwner, u8)> = effects
            .filter_map(|(plugin, stage)| Some((plugin.as_ref()?, stage)))
            .collect();
        let young = chain
            .dropped
            .iter()
            .filter(|(_, at)| now.saturating_sub(*at) < CHAIN_GRACE);
        let dropped: Vec<CopyName<'_>> = young
            .map(|(copy, _)| (copy.slot, copy.name.as_str(), copy.preset.as_deref()))
            .collect();
        let stages = seat(&units, &dropped, owner);
        let kept: Vec<(InsertSlot, &PluginOwner)> = owner.copies(chain.id, stages).collect();
        for (slot, mine) in old.copies(chain.id, chain.stages) {
            if !kept
                .iter()
                .any(|(at, theirs)| *at == slot && mine.same_plugin(theirs))
            {
                let copy = wanted_copy((slot, mine.name.as_str(), mine.preset.as_deref()));
                chain.dropped.push((copy, now));
            }
        }
        chain.stages = stages;
        self.owners.insert(owner.clone(), chain);
        self.touch(owner, now).expect("the chain of the owner")
    }

    /// Gives `owner` a new chain, in use now. Its effects sit on the stages
    /// 0, 1, 2 and 3 in call order: see [`seat`].
    fn insert(&mut self, owner: &ChainOwner, now: Duration) -> Seat {
        let chain = Chain {
            id: new_chain(),
            stages: seat(&[], &[], owner),
            used: 0,
            epoch: self.epoch,
            seen: now,
            left: None,
            lost: Default::default(),
            dropped: Vec::new(),
        };
        self.owners.insert(owner.clone(), chain);
        self.touch(owner, now).expect("the new chain")
    }

    /// The owner of the unused chain of a run-time lane that grows to
    /// `owner`, an owner of a run-time lane: see [`ChainOwner::grows_to`].
    /// A lane above moves the run-time name of a lane, and a call above
    /// moves each call number after it, so such a chain is often the same
    /// chain with a new owner. The chain with the fewest plugins to add goes
    /// first, then a chain of the lane of the owner, then the lowest number.
    /// A note of a lane that does not play, with `sounds` false, takes only
    /// a chain of its own lane.
    fn grown(&self, owner: &ChainOwner, sounds: bool) -> Option<ChainOwner> {
        let plugins = |owner: &ChainOwner| owner.plugins.iter().flatten().count();
        let unused = self.owners.iter().filter(|(old, chain)| {
            let lane = old.lane.run_time() && (sounds || old.lane == owner.lane);
            chain.epoch < self.epoch && lane && old.grows_to(owner)
        });
        let order = |(old, chain): &(&ChainOwner, &Chain)| {
            (
                plugins(owner) - plugins(old),
                old.lane != owner.lane,
                chain.id,
            )
        };
        unused.min_by_key(order).map(|(old, _)| old.clone())
    }

    /// Gives each owner with no chain yet an unused chain of its lane with a
    /// plugin in common: see [`ChainOwner::fit`]. The best pair goes first,
    /// and each owner and each chain pairs one time. Pairs that fit as well
    /// go in call order: the k-th owner takes the k-th chain. An owner with
    /// fewer effects takes the chain too: see [`Self::take`].
    fn pair(&mut self, owners: &[(ChainOwner, bool)], seats: &mut [Option<Seat>], now: Duration) {
        let unused = self
            .owners
            .iter()
            .filter(|(_, chain)| chain.epoch < self.epoch);
        let unused: Vec<(ChainOwner, u64)> =
            unused.map(|(old, chain)| (old.clone(), chain.id)).collect();
        let mut pairs = Vec::new();
        for (at, (owner, _)) in owners.iter().enumerate() {
            if seats[at].is_some() {
                continue;
            }
            for (place, (old, id)) in unused.iter().enumerate() {
                if let Some((same, named, effects)) = old.fit(owner) {
                    let fit = (Reverse(same), Reverse(named), Reverse(effects));
                    pairs.push(((fit, owner.call(), old.call(), *id), at, place));
                }
            }
        }
        pairs.sort_by_key(|(order, ..)| *order);
        let (mut asked, mut paired) = (vec![false; owners.len()], vec![false; unused.len()]);
        for (_, at, place) in pairs {
            if asked[at] || paired[place] {
                continue;
            }
            (asked[at], paired[place]) = (true, true);
            seats[at] = Some(self.take(&unused[place].0, &owners[at].0, now));
        }
    }

    /// The owner of the unused chain in grace with the oldest time, then
    /// the lowest number: see [`Chain::grace`].
    fn in_grace(&self) -> Option<ChainOwner> {
        let unused = self
            .owners
            .iter()
            .filter(|(_, chain)| chain.epoch < self.epoch);
        let grace = unused.filter_map(|(owner, chain)| {
            let order = (chain.grace(owner)?, chain.id);
            Some((order, owner))
        });
        grace
            .min_by_key(|(order, _)| *order)
            .map(|(_, owner)| owner.clone())
    }

    /// The owner of the unused chain with the oldest use: see
    /// [`render_chains`].
    fn oldest_unused(&self) -> Option<ChainOwner> {
        let unused = self
            .owners
            .iter()
            .filter(|(_, chain)| chain.epoch < self.epoch);
        unused
            .min_by_key(|(_, chain)| chain.used)
            .map(|(owner, _)| owner.clone())
    }

    /// Takes out the chains that left the score, after their time: see
    /// [`Self::due`]. Gives their numbers.
    fn settle(&mut self, now: Duration) -> Vec<u64> {
        let gone = self.due(now);
        self.end(&gone, now);
        gone
    }

    /// The chains whose time passed at `now`. A chain ends [`CHAIN_GRACE`]
    /// after the start of its grace: see [`Chain::grace`]. A chain with a
    /// plugin the body of its lane does not name ends [`CHAIN_UNSEEN_LIMIT`]
    /// after its last note, when no note used it since the last score
    /// change: see [`Self::unnamed`].
    fn due(&self, now: Duration) -> Vec<u64> {
        let past = |at: Option<Duration>, time: Duration| {
            at.is_some_and(|at| now.saturating_sub(at) >= time)
        };
        let due = self.owners.iter().filter(|(owner, chain)| {
            let unseen = chain.epoch < self.epoch
                && past(Some(chain.seen), CHAIN_UNSEEN_LIMIT)
                && self.unnamed(owner, chain);
            unseen || past(chain.grace(owner), CHAIN_GRACE)
        });
        due.map(|(_, chain)| chain.id).collect()
    }

    /// True when the chain has a plugin the body of its lane does not name:
    /// each plugin of a chain of a run-time lane or of no lane, and a plugin
    /// a lane takes from a variable or a helper. A plugin the body lost
    /// counts in [`Chain::grace`] instead, until a note uses the chain.
    fn unnamed(&self, owner: &ChainOwner, chain: &Chain) -> bool {
        let Some(number) = owner.lane.number() else {
            return true;
        };
        let names = self.bodies.get(&number).map_or(&[][..], Vec::as_slice);
        let calls = owner.plugins.iter().zip(&chain.lost);
        let mut kept =
            calls.filter_map(|(plugin, lost)| plugin.as_ref().filter(|_| lost.is_none()));
        kept.any(|plugin| !names.contains(&canonical(&plugin.name)))
    }

    /// Takes out the chains with a number in `gone`, and each key a chain
    /// lost [`CHAIN_GRACE`] before `now` or earlier: see [`Chain::dropped`].
    fn end(&mut self, gone: &[u64], now: Duration) {
        self.owners.retain(|_, chain| !gone.contains(&chain.id));
        for chain in self.owners.values_mut() {
            chain
                .dropped
                .retain(|(_, at)| now.saturating_sub(*at) < CHAIN_GRACE);
        }
    }

    /// The chains the score has, the last used first.
    fn named(&self) -> Vec<u64> {
        let mut chains: Vec<&Chain> = self.owners.values().collect();
        chains.sort_by_key(|chain| Reverse(chain.used));
        chains.truncate(rustel_audio::MAX_INSERT_BUSES);
        chains.iter().map(|chain| chain.id).collect()
    }

    /// True when a chain here has the number `id`.
    fn has(&self, id: u64) -> bool {
        self.owners.values().any(|chain| chain.id == id)
    }

    /// Brings a plan up to date with `playing`, the registry of the score
    /// in play. The plan gets each chain that score made after the plan, as
    /// a chain the planned score did not use. A chain of the plan with the
    /// same owner gives way: an engine holds the copies of the chain in
    /// play. Gives each chain that gave way and that only the plan had.
    fn catch_up(&mut self, playing: &Self) -> Vec<u64> {
        let known: HashSet<u64> = self.owners.values().map(|chain| chain.id).collect();
        let before = self.epoch.saturating_sub(1);
        let mut ended = Vec::new();
        for (owner, chain) in &playing.owners {
            if known.contains(&chain.id) {
                continue;
            }
            let chain = Chain {
                epoch: chain.epoch.min(before),
                ..chain.clone()
            };
            let gave_way = self.owners.insert(owner.clone(), chain);
            let only_here = gave_way.filter(|chain| !playing.has(chain.id));
            ended.extend(only_here.map(|chain| chain.id));
        }
        self.accessed = self.accessed.max(playing.accessed);
        ended
    }
}

/// The parameter values the sliders of one score link: see [`link_sliders`].
const MAX_SLIDER_LINKS: usize = 1024;

/// A slider that is the value of a plugin key: the parameter of the copy of
/// `key` in `slot` that the slider moves. See [`link_sliders`].
struct SliderLink {
    slider: String,
    slot: InsertSlot,
    key: InsertKey,
    /// The key as the note writes it, and the number of its parameter.
    param_key: String,
    param: u32,
}

/// The chains of the scores of one session.
///
/// The session plans the chains of a score text before the text goes in
/// play: see [`plan`]. The plan is a registry of its own, so a text that
/// does not go in play changes no chain of the score in play. The names
/// source of the live output of the session reads the same chains: see
/// `Session::attach_plugins`. The time of the chains is the play of that
/// output: see [`Self::tick`]. The plugin host keeps the copies the chains
/// want: see [`keep_now`]. The values source of the output reads the
/// values the sliders of the score move: see [`slider_moved`].
pub(crate) struct Chains {
    /// The chains of the score in play.
    playing: ChainRegistry,
    /// The last text the session planned that is not in play, with the
    /// registry as the commit of that text leaves it.
    plan: Option<(String, ChainRegistry)>,
    /// The plugin lines of the chains since the last take: see
    /// [`take_problems`].
    problems: Vec<String>,
    /// The time of the chains: see [`Self::tick`].
    clock: Duration,
    /// The time of the live output at the last turn of its producer, in
    /// seconds.
    ticked: Option<f64>,
    /// The number of the chains for the host: see [`Host::keep`].
    caller: u64,
    /// The copies the chains want and the chains they name, as the host
    /// has them from the last call of [`keep_now`].
    kept: (Vec<WantedCopy>, Vec<u64>),
    /// The copies an engine holds that the chains no longer want, as the
    /// last call of [`Host::keep`] gave them.
    gone: Vec<(InsertSlot, InsertKey)>,
    /// The counts of the live output at its last turn: see
    /// [`named_chains`].
    turn: InsertTurn,
    /// The counts of the turn at the last take of the lines of the output:
    /// see [`take_output_lines`].
    told: InsertTurn,
    /// The sliders that are the value of a plugin key in the notes of the
    /// score in play: see [`link_sliders`].
    links: Vec<SliderLink>,
    /// True while `links` has a link. The session reads it with no lock
    /// before a move: see [`slider_links`].
    linked: Arc<AtomicBool>,
    /// The values the sliders moved that the live output did not take yet:
    /// see [`slider_moved`].
    waiting: Vec<InsertValue>,
}

impl Chains {
    pub(crate) fn new() -> Self {
        // One count serves each session of the process, so a second
        // session unnames no chain of the first.
        static CALLERS: AtomicU64 = AtomicU64::new(1);
        Self {
            playing: ChainRegistry::new(),
            plan: None,
            problems: Vec::new(),
            clock: Duration::ZERO,
            ticked: None,
            caller: CALLERS.fetch_add(1, Ordering::Relaxed),
            kept: (Vec::new(), Vec::new()),
            gone: Vec::new(),
            turn: InsertTurn::default(),
            told: InsertTurn::default(),
            links: Vec::new(),
            linked: Arc::default(),
            waiting: Vec::new(),
        }
    }

    /// A turn of the live output, with the counts of the output. A count
    /// below the count of the turn before is the count of a new output, so
    /// the lines tell that count from 0.
    fn count(&mut self, turn: InsertTurn) {
        if turn.refusals < self.turn.refusals {
            self.told.refusals = 0;
        }
        if turn.late_jobs < self.turn.late_jobs {
            self.told.late_jobs = 0;
        }
        self.turn = turn;
    }

    /// A turn of the producer of the live output, at the time `now` of the
    /// output in seconds. The clock of the chains goes on by the time since
    /// the turn before, 1 s at most, and by nothing when `now` goes back. So
    /// a Stop, a held start or a pause of the producer ends no chain. A
    /// session with no chain counts no turn.
    fn tick(&mut self, now: f64) {
        let plan = self.plan.iter().map(|(_, planned)| planned);
        let mut registries = std::iter::once(&self.playing).chain(plan);
        if !now.is_finite() || registries.all(|registry| registry.owners.is_empty()) {
            return;
        }
        if let Some(ticked) = self.ticked
            && now > ticked
        {
            self.clock += Duration::from_secs_f64((now - ticked).min(1.0));
        }
        self.ticked = Some(now);
    }

    /// Plans the chains of a score text that is not in play. `lanes` are
    /// the lanes of the text. Gives the registry of the plan: a copy of the
    /// registry of the score in play, with the score change of the text.
    /// Each note of the text takes the number of its chain there. A session
    /// with a plan starts from that plan, so a new lane keeps its number from
    /// one try of an edit to the next. Gives also the chains the plan no
    /// longer has: see [`ChainRegistry::catch_up`].
    fn plan(
        &mut self,
        source: &str,
        lanes: &ScoreLanes,
        now: Duration,
    ) -> (&mut ChainRegistry, Vec<u64>) {
        let (mut planned, ended) = match self.plan.take() {
            Some((_, mut last)) => {
                let ended = last.catch_up(&self.playing);
                (last, ended)
            }
            None => (self.playing.clone(), Vec::new()),
        };
        planned.score_changed(lanes, now);
        let (_, planned) = self.plan.insert((source.to_owned(), planned));
        (planned, ended)
    }

    /// A new score plays: see [`score_changed`]. `lanes` are the lanes of
    /// its text. Gives the chains of the plan that end with this change.
    /// The links of the sliders go, and the notes of the new score link
    /// their sliders again: see [`link_sliders`]. A value a slider moved
    /// still goes to the output.
    fn score_changed(
        &mut self,
        source: Option<&str>,
        lanes: &ScoreLanes,
        now: Duration,
    ) -> Vec<u64> {
        self.links.clear();
        self.linked.store(false, Ordering::Relaxed);
        match self.plan.take_if(|(text, _)| Some(text.as_str()) == source) {
            // The plan made the score change of this text. A chain the
            // score in play made after the plan leaves with its lane too,
            // and loses each plugin the lane body in play named and this
            // text does not. Each mark the plan made takes the time of
            // this commit, as a mark the score in play makes now: a lane
            // that left, a lost plugin and a dropped key. A mark the score
            // in play has keeps its time. A chain keeps its last use in
            // play too.
            Some((_, mut planned)) => {
                let ended = planned.catch_up(&self.playing);
                planned.bodies = std::mem::take(&mut self.playing.bodies);
                planned.leave(lanes, now);
                let restart = |mark: Option<Duration>, kept: Option<Duration>| {
                    mark.map(|_| kept.unwrap_or(now))
                };
                for chain in planned.owners.values_mut() {
                    let played = self.playing.owners.values().find(|old| old.id == chain.id);
                    chain.left = restart(chain.left, played.and_then(|old| old.left));
                    for (place, lost) in chain.lost.iter_mut().enumerate() {
                        *lost = restart(*lost, played.and_then(|old| old.lost[place]));
                    }
                    if let Some(old) = played {
                        chain.seen = chain.seen.max(old.seen);
                    }
                    for key in &mut chain.dropped {
                        if !played.is_some_and(|old| old.dropped.contains(key)) {
                            key.1 = now;
                        }
                    }
                }
                self.playing = planned;
                ended
            }
            None => {
                self.playing.score_changed(lanes, now);
                Vec::new()
            }
        }
    }

    /// Drops the plan. Gives the chains only the plan made.
    fn forget(&mut self) -> Vec<u64> {
        let planned = self.plan.take().map(|(_, planned)| planned);
        let chains = planned.iter().flat_map(|planned| planned.owners.values());
        let made = chains.filter(|chain| !self.playing.has(chain.id));
        made.map(|chain| chain.id).collect()
    }

    /// Takes out the chains that left the score in play, after their time:
    /// see [`ChainRegistry::settle`]. The chains settle while the session
    /// has a plan too. A chain of the score in play and of the plan ends
    /// when its time passed in both, so a chain the plan uses stays, and so
    /// does a chain the score in play plays. A chain only one of them has
    /// ends after its time there. A chain that ends leaves both: the commit
    /// of a plan brings back no chain whose copies the host ended.
    fn settle(&mut self, now: Duration) -> Vec<u64> {
        let Some((_, planned)) = &mut self.plan else {
            return self.playing.settle(now);
        };
        let ends = planned.due(now);
        let played = self.playing.due(now).into_iter();
        let mut gone: Vec<u64> = played
            .filter(|id| ends.contains(id) || !planned.has(*id))
            .collect();
        gone.extend(ends.iter().filter(|id| !self.playing.has(**id)));
        self.playing.end(&gone, now);
        planned.end(&gone, now);
        gone
    }

    /// The plugin copies the chains want now, each one time and in order,
    /// and the chains of the score in play and of the plan: what the host
    /// keeps for the session. An owner wants the copy of each stage it sits
    /// on, until [`CHAIN_GRACE`] after the body of its lane lost the plugin:
    /// see [`Chain::lost`]. A chain wants the copy of a key it lost
    /// [`CHAIN_GRACE`] more: see [`Chain::dropped`]. Of the keys a slot lost
    /// in that time, the slot wants the oldest and the newest. So a run of
    /// preset changes keeps the copy in use, the copy before and the copy of
    /// the preset the run started from: 3 copies of the slot at most. Its
    /// undo builds nothing.
    fn wanted(&self) -> (Vec<CopyName<'_>>, Vec<u64>) {
        let young = |at: Duration| self.clock.saturating_sub(at) < CHAIN_GRACE;
        let plan = self.plan.iter().map(|(_, planned)| planned);
        let registries = std::iter::once(&self.playing).chain(plan);
        let chains: Vec<(&ChainOwner, &Chain)> = registries.flat_map(|one| &one.owners).collect();
        let mut copies = Vec::new();
        for (owner, chain) in &chains {
            let calls = owner.plugins.iter().zip(chain.lost).enumerate();
            for (place, (plugin, lost)) in calls {
                if let Some(plugin) = plugin
                    && lost.is_none_or(young)
                {
                    let slot = slot_of(chain.id, chain.stages, place);
                    copies.push((slot, plugin.name.as_str(), plugin.preset.as_deref()));
                }
            }
            // The keys of each slot in the order they left. The sort keeps
            // the order of the list for keys of one time.
            let mut dropped: Vec<&(WantedCopy, Duration)> =
                chain.dropped.iter().filter(|(_, at)| young(*at)).collect();
            dropped.sort_by_key(|(copy, at)| (copy.slot, *at));
            for (place, (copy, _)) in dropped.iter().enumerate() {
                let first = place == 0 || dropped[place - 1].0.slot != copy.slot;
                let last = dropped
                    .get(place + 1)
                    .is_none_or(|(next, _)| next.slot != copy.slot);
                if first || last {
                    copies.push((copy.slot, copy.name.as_str(), copy.preset.as_deref()));
                }
            }
        }
        copies.sort_unstable();
        copies.dedup();
        let mut named: Vec<u64> = chains.iter().map(|(_, chain)| chain.id).collect();
        named.sort_unstable();
        named.dedup();
        (copies, named)
    }

    /// The plugin names of the chains of the score in play and of the
    /// plan, as the notes write them, each one time. A chain in its grace
    /// does not count: see [`Chain::grace`].
    fn plugin_names(&self) -> Vec<String> {
        let plan = self.plan.iter().map(|(_, planned)| planned);
        let registries = std::iter::once(&self.playing).chain(plan);
        let chains = registries.flat_map(|registry| &registry.owners);
        let kept = chains.filter(|(owner, chain)| chain.grace(owner).is_none());
        let plugins = kept.flat_map(|(owner, _)| owner.plugins.iter().flatten());
        let mut names: Vec<String> = plugins.map(|plugin| plugin.name.clone()).collect();
        names.sort_unstable();
        names.dedup();
        names
    }

    /// Links the slider `slider` to the parameter with the key `param_key`
    /// of the copy in `slot`. `wanted` is what a note asks of that copy, and
    /// `param_of` gives the number of the parameter: see [`link_sliders`].
    fn link(
        &mut self,
        slider: &str,
        slot: InsertSlot,
        wanted: &InsertControls,
        param_key: &str,
        param_of: impl FnOnce() -> Option<u32>,
    ) {
        let key = wanted.key;
        let known = self
            .links
            .iter()
            .find(|link| (link.slot, link.key) == (slot, key) && link.param_key == param_key);
        let Some(param) = known.map(|link| link.param).or_else(param_of) else {
            return;
        };
        // A value the note dropped makes no link.
        if !wanted.params().iter().any(|value| value.id == param) {
            return;
        }
        let present = self.links.iter().any(|link| {
            link.slider == slider && (link.slot, link.key, link.param) == (slot, key, param)
        });
        if !present && self.links.len() < MAX_SLIDER_LINKS {
            self.links.push(SliderLink {
                slider: slider.to_owned(),
                slot,
                key,
                param_key: param_key.to_owned(),
                param,
            });
            self.linked.store(true, Ordering::Relaxed);
        }
    }
}

/// The host ends the chains of a session that ends: each chain the last
/// call of [`Host::keep`] named, so each chain with a copy the session
/// wanted. Their copies end, and a copy an engine holds ends when it comes
/// home. The host keeps no copy for a session that is gone.
impl Drop for Chains {
    fn drop(&mut self) {
        let (copies, named) = &self.kept;
        if (!copies.is_empty() || !named.is_empty())
            && let Some(host) = started()
        {
            host.keep(self.caller, Vec::new(), Vec::new(), named.clone());
        }
    }
}

/// Takes the lock of the chains of a session. A panic of a thread with the
/// lock does not end the plugins: the next caller reads the chains as that
/// thread left them.
fn locked(chains: &Mutex<Chains>) -> std::sync::MutexGuard<'_, Chains> {
    chains.lock().unwrap_or_else(|error| error.into_inner())
}

/// The chain of a note with a plugin, and the stage of each of its effect
/// calls: see [`seat`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Seat {
    pub(crate) chain: u64,
    stages: [u8; rustel_audio::EFFECT_CHAIN],
}

impl Seat {
    /// Puts the controls of a converted note on the seat: the chain, and
    /// the stage of each effect. The engine then runs the effects of the
    /// note in call order on their stages.
    fn place(self, controls: &mut rustel_audio::OscillatorControls) {
        controls.insert_chain = Some(self.chain);
        for (effect, stage) in controls.effects.iter_mut().zip(self.stages) {
            if let Some(effect) = effect {
                effect.set_stage(stage);
            }
        }
    }
}

/// The chain of a note: see [`assign`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum NoteChain {
    /// The note has no plugin.
    None,
    /// The seat of the note on its chain.
    Seat(Seat),
    /// The output has no room for the chain of the note.
    Refused,
}

impl NoteChain {
    /// Gives the controls of a converted note its chain. A note with no room
    /// plays with no plugin: it loses its effects. A note with an instrument
    /// loses it too, with gain 0 and no gain binding, so no oscillator
    /// sounds in place of the instrument.
    pub(crate) fn apply(self, controls: &mut rustel_audio::OscillatorControls, gain: &mut f32) {
        match self {
            Self::None => {}
            Self::Seat(seat) => seat.place(controls),
            Self::Refused => {
                controls.effects = [None; rustel_audio::EFFECT_CHAIN];
                if controls.instrument.take().is_some() {
                    *gain = 0.0;
                    controls.live_controls[0] = 0;
                }
            }
        }
    }

    /// Gives the event of a converted note of a render its chain: see
    /// [`Self::apply`].
    pub(crate) fn number(self, mut event: rustel_audio::OnsetEvent) -> rustel_audio::OnsetEvent {
        self.apply(&mut event.controls, &mut event.gain);
        event
    }
}

/// The owners of a batch of notes: see [`ChainRegistry::assign`].
struct Batch {
    /// Each owner one time, in the order of its first note, with true when
    /// that note plays.
    owners: Vec<(ChainOwner, bool)>,
    /// The owner of each note, as its place in `owners`.
    notes: Vec<Option<usize>>,
    /// The orbits of the Csound notes with no plugin.
    csound: Vec<u64>,
}

impl Batch {
    /// The owners of `notes`, each with false for a note of a lane that does
    /// not play. The notes that play come first. `lanes` are the lanes of
    /// the text of the notes: see [`OwnerLane::of`].
    fn of<'a>(
        notes: impl IntoIterator<Item = (&'a crate::ValueJson, bool)>,
        lanes: Option<&ScoreLanes>,
    ) -> Self {
        let mut batch = Self {
            owners: Vec::new(),
            notes: Vec::new(),
            csound: Vec::new(),
        };
        let mut places: HashMap<ChainOwner, usize> = HashMap::new();
        for (value, sounds) in notes {
            let crate::ValueJson::Raw(value) = value else {
                batch.notes.push(None);
                continue;
            };
            let Some(owner) = ChainOwner::from_value(value, lanes) else {
                // A Csound note with no plugin plays on the chain of its
                // orbit.
                if value.get("csound").is_some() || value.get("csoundm").is_some() {
                    batch.csound.push(orbit(value).into());
                }
                batch.notes.push(None);
                continue;
            };
            let place = *places.entry(owner).or_insert_with_key(|owner| {
                batch.owners.push((owner.clone(), sounds));
                batch.owners.len() - 1
            });
            batch.notes.push(Some(place));
        }
        batch
    }

    /// The room for the plugin chains: the chains of the output, less the
    /// Csound chains of the session and of this batch. `csound` are the
    /// orbits of the Csound chains of the session, and none for a render.
    fn room(&self, mut csound: Vec<u64>) -> usize {
        csound.extend(&self.csound);
        csound.sort_unstable();
        csound.dedup();
        rustel_audio::MAX_INSERT_BUSES.saturating_sub(csound.len())
    }

    /// The chain of each note. `seats` has the seat of each owner, or
    /// `None` for an owner with no room. Each note of an owner, also a note
    /// of a lane that does not play, sits on the seat of its owner.
    fn chains(&self, seats: &[Option<Seat>]) -> Vec<NoteChain> {
        let chain = |note: &Option<usize>| match note {
            None => NoteChain::None,
            Some(place) => seats[*place].map_or(NoteChain::Refused, NoteChain::Seat),
        };
        self.notes.iter().map(chain).collect()
    }
}

/// The plugin line of a lane with no room for its chain: its label and its
/// line, or its run-time name. `lanes` are the lanes of the text.
fn no_room(lane: &OwnerLane, lanes: &ScoreLanes) -> String {
    let statement = |(_, label, line, _): (u64, &str, usize, &[String])| match label {
        "" => format!("the lane at line {line}"),
        label => format!("the lane '{label}' at line {line}"),
    };
    let mut statements = lanes.lanes();
    let lane = match lane {
        OwnerLane::Lane(number) => statements.find(|(at, ..)| at == number).map(statement),
        OwnerLane::Place(place) => {
            let mut labeled = statements.filter(|(_, label, ..)| !label.is_empty());
            labeled.nth(*place as usize).map(statement)
        }
        OwnerLane::Name(name) => Some(format!("the lane '{name}'")),
        OwnerLane::NoLane(_) => None,
    };
    let lane = lane.unwrap_or_else(|| "a note with no lane".to_owned());
    let most = rustel_audio::MAX_INSERT_BUSES;
    format!("{lane} plays with no plugin: {most} chains play at most")
}

/// Gives each note of a window of the score in play of a session its
/// chain, in one assignment: see [`ChainRegistry::assign`]. `values` are
/// the notes the voice took, in the order of the window. `csound` has the
/// Csound chains of the session: each Csound orbit takes a place of the
/// room. A note with no room plays with no plugin, and its lane gets one
/// plugin line: see [`take_problems`]. A window with no plugin note asks
/// the chains nothing. With `live`, the session has plugins on a live
/// output: the host then hears the copies the chains want, before the
/// output asks for a copy. See [`keep_now`].
pub(crate) fn assign<'a>(
    chains: &Mutex<Chains>,
    lanes: &LaneTable,
    csound: &crate::csound::State,
    values: impl IntoIterator<Item = &'a crate::ValueJson>,
    live: bool,
) -> Vec<NoteChain> {
    let lanes = lanes.playing();
    let batch = Batch::of(values.into_iter().map(|value| (value, true)), Some(lanes));
    if batch.owners.is_empty() {
        return batch.chains(&[]);
    }
    let room = batch.room(csound.orbit_chains());
    let mut chains = locked(chains);
    let now = chains.clock;
    let assignment = chains
        .playing
        .assign(&batch.owners, room, now, ChainRegistry::in_grace);
    let owners = batch.owners.iter().zip(&assignment.seats);
    let refused = owners.filter_map(|((owner, _), seat)| seat.is_none().then_some(&owner.lane));
    for lane in refused.chain(&assignment.cut) {
        let line = no_room(lane, lanes);
        if !chains.problems.contains(&line) {
            chains.problems.push(line);
        }
    }
    if live {
        keep_now(&mut chains, assignment.ended);
    }
    batch.chains(&assignment.seats)
}

/// The chain of each note of a render to a file, in the order of `onsets`:
/// the notes of the render, in the order of their frames. A render has
/// chains of its own, so its notes take no chain of the score on the live
/// output. Each run of notes of one generation takes its chains in one
/// assignment: see [`ChainRegistry::assign`]. A new generation is a score
/// change, so an owner with a new text takes the chain of its lane that the
/// generation before used. Each Csound orbit of a run takes a place of the
/// room: see [`Batch::room`]. With no room, the chain with the oldest use
/// that the generation does not use ends. A note with no room plays with no
/// plugin, and gives no line.
pub(crate) fn render_chains(onsets: &[&crate::OnsetEventJson]) -> Vec<NoteChain> {
    let (mut registry, start) = (ChainRegistry::new(), Duration::ZERO);
    let mut chains = Vec::with_capacity(onsets.len());
    for run in onsets.chunk_by(|one, other| one.generation == other.generation) {
        registry.epoch += 1;
        let batch = Batch::of(run.iter().map(|onset| (&onset.value, true)), None);
        let room = batch.room(Vec::new());
        let assignment = registry.assign(&batch.owners, room, start, ChainRegistry::oldest_unused);
        chains.extend(batch.chains(&assignment.seats));
    }
    chains
}

/// A new score plays in a session: `source` is its text, or `None` for a
/// direct pattern. `lanes` has the lanes of the new score in play. The
/// chains of the score before stay for the new score to take, and a chain
/// of a lane that left ends after its time: see
/// [`ChainRegistry::score_changed`]. When the session planned this text,
/// the chains of the plan are the chains of the score, so each note has the
/// number the plan gave: see [`plan`]. A plan of a different text stays.
///
/// `live` is false for a session with no live output: see
/// `Session::attach_plugins`. Such a session counts no time, so each chain
/// ends at once where it would end after its time, and the host hears
/// nothing of it: no copy of such a chain exists. A session with a live
/// output tells the host the copies its chains want: see [`keep_now`].
pub(crate) fn score_changed(
    chains: &Mutex<Chains>,
    lanes: &LaneTable,
    source: Option<&str>,
    live: bool,
) {
    let mut chains = locked(chains);
    let now = chains.clock;
    let ended = chains.score_changed(source, lanes.playing(), now);
    if live {
        keep_now(&mut chains, ended);
    } else {
        chains.settle(Duration::MAX);
    }
}

/// Forgets each lane that left the score in play and that no chain of the
/// session has: see [`LaneTable::forget_away`]. A chain of the plan counts
/// too. The commit of a score calls this after [`score_changed`].
pub(crate) fn forget_lanes_away(chains: &Mutex<Chains>, lanes: &mut LaneTable) {
    let chained: HashSet<u64> = {
        let chains = locked(chains);
        let plan = chains.plan.iter().map(|(_, planned)| planned);
        let registries = std::iter::once(&chains.playing).chain(plan);
        let owners = registries.flat_map(|registry| registry.owners.keys());
        owners.filter_map(|owner| owner.lane.number()).collect()
    };
    lanes.forget_away(|lane| chained.contains(&lane));
}

/// Plans the chains of the score text `source` before the text goes in
/// play. `haps` are notes of the text, and `shadow` are notes of its lanes
/// that do not play. `lanes` has the lanes of the text, and `csound` the
/// Csound chains of the session. Gives the plugin copies the notes need,
/// each for the chain its note has when the text is in play: see
/// [`wants_of`].
/// The plan changes no chain of the score in play, and the commit of the
/// text takes the plan: see [`score_changed`]. A new plan starts from the
/// plan before. A new lane then keeps its chain, and so its copies, from one
/// try of an edit to the next.
///
/// The chains settle while the session has a plan: see [`Chains::settle`].
/// The session ends each plan: with the commit of its text, with a new
/// plan, or with [`forget_plan`]. The host hears the copies the chains of
/// the plan want, before the caller asks for them: see [`keep_now`].
// Only the session of a live output plans: see `Session::reload_live`.
#[cfg_attr(not(feature = "device-audio"), allow(dead_code))]
pub(crate) fn plan(
    chains: &Mutex<Chains>,
    lanes: &LaneTable,
    csound: &crate::csound::State,
    source: &str,
    haps: &[crate::HapJson],
    shadow: &[crate::HapJson],
) -> Vec<Want> {
    let lanes = lanes.candidate(source).unwrap_or(lanes.playing());
    let batch = Batch::of(notes(haps, shadow), Some(lanes));
    let room = batch.room(csound.orbit_chains());
    let mut chains = locked(chains);
    let now = chains.clock;
    let (planned, mut ended) = chains.plan(source, lanes, now);
    let assignment = planned.assign(&batch.owners, room, now, ChainRegistry::in_grace);
    ended.extend(assignment.ended);
    keep_now(&mut chains, ended);
    drop(chains);
    wants_of(haps, shadow, &batch.chains(&assignment.seats))
}

/// Drops the plan of a session: its text does not go in play. The host
/// ends the copies of the chains only the plan made, and the copies only
/// the plan wanted. A session with no plan changes nothing.
// Only the session of a live output plans: see `Session::reload_live`.
#[cfg_attr(not(feature = "device-audio"), allow(dead_code))]
pub(crate) fn forget_plan(chains: &Mutex<Chains>) {
    let mut chains = locked(chains);
    if chains.plan.is_some() {
        let made = chains.forget();
        keep_now(&mut chains, made);
    }
}

/// Tells the host the plugin copies the chains of a session want and the
/// chains they name, when these changed since the last call or a chain
/// ended: see [`Host::keep`] and [`Chains::wanted`]. `ended` has the
/// chains that ended in a registry of the session. The host hears only the
/// chains that no registry has now: a chain that one registry ends to make
/// room stays while the other registry has it, as in [`Chains::settle`].
/// The host ends each copy no chain wants, and builds no copy a live chain
/// does not want. The host gives the copies an engine holds that no chain
/// wants: see [`named_chains`]. Only a session with plugins on a live
/// output calls this: no copy of the chains of another session exists.
fn keep_now(chains: &mut Chains, mut ended: Vec<u64>) {
    let (copies, named) = chains.wanted();
    ended.retain(|chain| !named.contains(chain));
    let (kept, kept_named) = &chains.kept;
    let as_kept = kept
        .iter()
        .map(|copy| (copy.slot, copy.name.as_str(), copy.preset.as_deref()));
    if ended.is_empty() && as_kept.eq(copies.iter().copied()) && *kept_named == named {
        return;
    }
    // With no host, no copy exists. The next call tells the host.
    let Some(host) = started() else {
        return;
    };
    let copies: Vec<WantedCopy> = copies.into_iter().map(wanted_copy).collect();
    chains.gone = host.keep(chains.caller, copies.clone(), named.clone(), ended);
    chains.kept = (copies, named);
}

/// The chains the playing score has, for the audio output: see
/// `LiveScalarDevice::set_named_insert_chains`. `turn` has the counts of
/// the output: see [`take_output_lines`]. A chain that left the score is not
/// in the list after its time: see [`Chains::settle`]. The host then hears
/// the copies the chains want, before the output asks for a copy: see
/// [`keep_now`]. Gives also each copy an engine holds that the chains no
/// longer want, [`CHAIN_GRACE`] after a chain lost its key: see
/// [`Chains::wanted`]. The output gives such a copy back.
// Only the names source of a live output asks: see `Session::attach_plugins`.
#[cfg_attr(not(feature = "device-audio"), allow(dead_code))]
pub(crate) fn named_chains(chains: &Mutex<Chains>, turn: InsertTurn) -> InsertChains {
    let mut chains = locked(chains);
    chains.count(turn);
    let now = chains.clock;
    let left = chains.settle(now);
    keep_now(&mut chains, left);
    InsertChains {
        named: chains.playing.named(),
        gone: chains.gone.clone(),
    }
}

/// The plugin names of the chains of a session, for the unload of the
/// plugins no score uses: see [`Chains::plugin_names`]. The clock of the
/// chains stands while the output is stopped, so a Stop ends no chain.
pub(crate) fn plugin_names(chains: &Mutex<Chains>) -> Vec<String> {
    locked(chains).plugin_names()
}

/// A turn of the producer of the live output of a session, at the time
/// `now` of the output in seconds: see [`Chains::tick`].
// Only the producer of a live output turns: see `Session::plugin_turn`.
#[cfg_attr(not(feature = "device-audio"), allow(dead_code))]
pub(crate) fn tick(chains: &Mutex<Chains>, now: f64) {
    locked(chains).tick(now);
}

/// The plugin lines of the chains of a session since the last call: each
/// lane with no room for its chain. See [`assign`].
pub(crate) fn take_problems(chains: &Mutex<Chains>) -> Vec<String> {
    std::mem::take(&mut locked(chains).problems)
}

/// The plugin lines of the live output of a session since the last call:
/// one line when the output had no bus for a chain, and one when a plugin
/// worker ran late. See [`named_chains`].
pub(crate) fn take_output_lines(chains: &Mutex<Chains>) -> Vec<String> {
    let mut chains = locked(chains);
    let turn = chains.turn;
    let told = std::mem::replace(&mut chains.told, turn);
    let most = rustel_audio::MAX_INSERT_BUSES;
    let refused = (turn.refusals > told.refusals)
        .then(|| format!("the output had no room for a plugin chain: {most} chains play at most"));
    let late = (turn.late_jobs > told.late_jobs)
        .then(|| "a plugin worker ran late, and the audio waited for it".to_owned());
    refused.into_iter().chain(late).collect()
}

/// Links each slider that is the value of a plugin key of a note to the
/// parameter of the copy that plays the note: see [`slider_moved`]. `value`
/// is a note of a live window of the score in play, and `controls` are its
/// controls on its chain. A plugin object of the note names such a slider
/// under `__rustel_sliders`, by key. `param_of` gives the number of a
/// parameter from the plugin name and the key: see
/// [`rustel_vst3::Plugin::param`]. A link the chains have asks for no
/// number. A value the note dropped makes no link. The chains keep
/// [`MAX_SLIDER_LINKS`] links at most, until the next score change.
pub(crate) fn link_sliders(
    chains: &Mutex<Chains>,
    value: &serde_json::Value,
    controls: &rustel_audio::OscillatorControls,
    param_of: impl Fn(&str, &str) -> Option<u32>,
) {
    let Some(chain) = controls.insert_chain else {
        return;
    };
    // Each plugin object of the note, with the slot of its copy and what the
    // note asks of the copy.
    let effects = value.get("vst").and_then(serde_json::Value::as_array);
    let effects = effects.into_iter().flatten().zip(&controls.effects);
    let effects = effects.enumerate().filter_map(|(place, (plugin, wanted))| {
        let wanted = wanted.as_ref()?;
        let slot = rustel_audio::effect_slot(chain, wanted.stage(place));
        Some((plugin, slot, wanted))
    });
    let instrument = value.get("vsti").zip(controls.instrument.as_ref());
    let instrument =
        instrument.map(|(plugin, wanted)| (plugin, rustel_audio::instrument_slot(chain), wanted));
    let mut linked = effects
        .chain(instrument)
        .filter_map(|(plugin, slot, wanted)| {
            let sliders = plugin.get("__rustel_sliders")?.as_object()?;
            let name = plugin.get("name")?.as_str()?;
            Some((name, sliders, slot, wanted))
        })
        .peekable();
    if linked.peek().is_none() {
        return;
    }
    let mut chains = locked(chains);
    for (name, sliders, slot, wanted) in linked {
        for (param_key, slider) in sliders {
            if let Some(slider) = slider.as_str() {
                chains.link(slider, slot, wanted, param_key, || {
                    param_of(name, param_key)
                });
            }
        }
    }
}

/// A slider of the score in play of a session moved to `value`, at the
/// generation `generation` of the score. Each parameter the slider links
/// gets one value for the live output, in place of the value of a move
/// before: see [`link_sliders`] and [`take_slider_values`]. A slider with
/// no link moves no parameter value.
pub(crate) fn slider_moved(chains: &Mutex<Chains>, slider: &str, value: f64, generation: u64) {
    let mut chains = locked(chains);
    let Chains { links, waiting, .. } = &mut *chains;
    for link in links.iter().filter(|link| link.slider == slider) {
        let moved = InsertValue {
            slot: link.slot,
            key: link.key,
            param: InsertParam {
                id: link.param,
                value: value as f32,
            },
            generation,
        };
        let same = |kept: &&mut InsertValue| {
            (kept.slot, kept.key, kept.param.id) == (moved.slot, moved.key, moved.param.id)
        };
        match waiting.iter_mut().find(same) {
            Some(kept) => *kept = moved,
            None => waiting.push(moved),
        }
    }
}

/// The flag of the chains of a session that is true while a slider of the
/// score in play links a parameter value: see [`link_sliders`]. The session
/// reads it with no lock, so a move of a score with no link takes no lock.
pub(crate) fn slider_links(chains: &Mutex<Chains>) -> Arc<AtomicBool> {
    Arc::clone(&locked(chains).linked)
}

/// The values the sliders of a session moved, in the order of the moves,
/// `room` at most: see [`slider_moved`]. The rest wait for the next call.
// Only the values source of a live output asks: see `Session::attach_plugins`.
#[cfg_attr(not(feature = "device-audio"), allow(dead_code))]
pub(crate) fn take_slider_values(chains: &Mutex<Chains>, room: usize) -> Vec<InsertValue> {
    let mut chains = locked(chains);
    let room = room.min(chains.waiting.len());
    chains.waiting.drain(..room).collect()
}

/// One plugin copy the notes of a score need: the slot of the copy, and the
/// plugin and the preset by name.
pub(crate) struct Want {
    instrument: bool,
    slot: InsertSlot,
    name: String,
    preset: Option<String>,
    /// False for a copy of a lane that does not play now: a muted lane, or
    /// a lane a solo drops. The copy loads for the end of the mute or of
    /// the solo, and nothing waits for it.
    sounds: bool,
}

impl Want {
    /// The name of the plugin, as the score writes it.
    #[cfg(feature = "device-audio")]
    pub(crate) fn name(&self) -> &str {
        &self.name
    }

    /// True for a copy of a lane that plays now.
    #[cfg(feature = "device-audio")]
    pub(crate) fn sounds(&self) -> bool {
        self.sounds
    }
}

/// The plugin copies the notes of the score in play of a session need: see
/// [`wants_of`]. `haps` are notes that play, and `shadow` are notes of the
/// lanes that do not play. The notes take their chains in one assignment,
/// as in [`assign`]. A note with no room gives no copy, and no line. The
/// host hears the copies the chains want, before the caller asks for them:
/// see [`keep_now`]. Only a session with plugins on a live output asks.
pub(crate) fn wants(
    chains: &Mutex<Chains>,
    lanes: &LaneTable,
    csound: &crate::csound::State,
    haps: &[crate::HapJson],
    shadow: &[crate::HapJson],
) -> Vec<Want> {
    let batch = Batch::of(notes(haps, shadow), Some(lanes.playing()));
    if batch.owners.is_empty() {
        return Vec::new();
    }
    let room = batch.room(csound.orbit_chains());
    let mut chains = locked(chains);
    let now = chains.clock;
    let assignment = chains
        .playing
        .assign(&batch.owners, room, now, ChainRegistry::in_grace);
    keep_now(&mut chains, assignment.ended);
    drop(chains);
    wants_of(haps, shadow, &batch.chains(&assignment.seats))
}

/// The notes of `haps`, then the notes of `shadow`, each with true for a
/// note of `haps`.
fn notes<'a>(
    haps: &'a [crate::HapJson],
    shadow: &'a [crate::HapJson],
) -> impl Iterator<Item = (&'a crate::ValueJson, bool)> {
    let haps = haps.iter().map(|hap| (&hap.value, true));
    haps.chain(shadow.iter().map(|hap| (&hap.value, false)))
}

/// The plugin copies the notes of the haps need, each one time, in the
/// order of the notes. The copies of `shadow`, the notes of the lanes that
/// do not play, come after the others. `chains` has the chain of each note
/// of `haps`, then of `shadow`. A note with no chain needs no copy. An
/// effect call needs the copy of its stage, and a call with no stage needs
/// none: see [`seat`].
fn wants_of(haps: &[crate::HapJson], shadow: &[crate::HapJson], chains: &[NoteChain]) -> Vec<Want> {
    let mut wants: Vec<Want> = Vec::new();
    for ((value, sounds), chain) in notes(haps, shadow).zip(chains) {
        let (crate::ValueJson::Raw(value), NoteChain::Seat(seat)) = (value, chain) else {
            continue;
        };
        let Some(object) = value.as_object() else {
            continue;
        };
        let instrument = object.get("vsti").map(|plugin| {
            let slot = rustel_audio::instrument_slot(seat.chain);
            (true, slot, plugin)
        });
        // The effects are a list, in chain order.
        let effects = object.get("vst").and_then(serde_json::Value::as_array);
        let effects = effects.into_iter().flatten().zip(seat.stages);
        let effects = effects
            .filter(|(_, stage)| *stage != rustel_audio::NO_STAGE)
            .map(|(plugin, stage)| {
                let slot = rustel_audio::effect_slot(seat.chain, usize::from(stage));
                (false, slot, plugin)
            });
        for (instrument, slot, plugin) in effects.chain(instrument) {
            let Some(name) = plugin.get("name").and_then(serde_json::Value::as_str) else {
                continue;
            };
            let preset = preset_name(plugin);
            let known = wants
                .iter()
                .any(|want| (want.slot, want.name.as_str(), &want.preset) == (slot, name, &preset));
            if !known {
                wants.push(Want {
                    instrument,
                    slot,
                    name: name.to_owned(),
                    preset,
                    sounds,
                });
            }
        }
    }
    wants
}

/// Starts the load of each plugin in `names` that is not loaded, with no
/// wait, and takes the name out of `names`. The names are the plugin names
/// of the plugin calls an evaluation ran: see
/// [`rustel_jsruntime::ScoreEffects::plugins`]. A name the host cannot
/// serve does nothing here: the note reports the reason. While the host
/// reads the plugin folders, the plugin list is not complete: see
/// [`scanning`]. A name the host does not know then stays in `names`, and
/// a later call starts its load.
pub(crate) fn load(names: &mut Vec<String>) {
    let host = host();
    names.retain(|name| {
        // The read comes first: a read that ends before the answer keeps
        // the name for one more call and loses no load.
        let scanning = host.scanning();
        matches!(host.resolve(name, false), Resolved::Pending) && scanning
    });
}

/// Makes the host build each copy in `wants` that is not ready, with no
/// wait, at the sample rate of the output. The builds run in the order of
/// the list, so a caller gives the copies of the lanes that sound first.
/// Gives the copies of lanes that play and that are not ready: a plugin in
/// its load, a copy in its build, or a copy an engine gave back on its way
/// home, which needs no build. Gives also the line of each copy the
/// host cannot make, with its reason, as the note of the copy or the host
/// says it. A copy with a build in the line gets no second build, so a
/// caller looks as often as it needs.
pub(crate) fn ensure(wants: &[Want], sample_rate: u32) -> (Vec<&Want>, Vec<String>) {
    let (mut pending, mut lines) = (Vec::new(), Vec::new());
    for want in wants {
        let Want {
            instrument,
            slot,
            name,
            preset,
            sounds,
        } = want;
        let (host, preset) = (host(), preset.as_deref());
        match host.prepared(name, preset, *instrument, sample_rate, *slot) {
            rustel_vst3::Prepared::Ready(_) => {}
            // The copy comes home from an engine, and needs no build.
            rustel_vst3::Prepared::Returning => {
                if *sounds {
                    pending.push(want);
                }
            }
            rustel_vst3::Prepared::Unavailable(reason) => {
                let call = if *instrument { "vsti" } else { "vst" };
                lines.push(format!("{call} skipped: {reason}"));
            }
            // The build starts now, or behind the load of the plugin.
            rustel_vst3::Prepared::Unbuilt(_) | rustel_vst3::Prepared::Pending => {
                host.prepare(name, preset, *instrument, sample_rate, *slot);
                if *sounds {
                    pending.push(want);
                }
            }
        }
    }
    (pending, lines)
}

/// The copies on their way home that the chains of a session want, for a
/// live output at the rate `sample_rate`: see [`Host::returning`]. The call
/// starts the retune of each such copy that waits at another rate. Chains
/// that want no copy ask the host nothing.
pub(crate) fn returning(chains: &Mutex<Chains>, sample_rate: u32) -> usize {
    let chains = locked(chains);
    if chains.kept.0.is_empty() {
        return 0;
    }
    started().map_or(0, |host| host.returning(chains.caller, sample_rate))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vst::lanes::tests::play;
    use crate::vst::tests::HOST_CALLS;

    /// A chain owner: the lane tag, the effects and the instrument, each
    /// plugin as its call number and its name. An empty tag is a note with
    /// no tag.
    fn owner(lane: &str, effects: &[(u32, &str)], instrument: Option<(u32, &str)>) -> ChainOwner {
        let plugin = |(call, name): &(u32, &str)| serde_json::json!({"name": name, "__rustel_binding": format!("p{call}")});
        let mut value = serde_json::json!({});
        if !lane.is_empty() {
            value["__rustel_lane"] = lane.into();
        }
        if !effects.is_empty() {
            value["vst"] = effects.iter().map(plugin).collect();
        }
        if let Some(instrument) = &instrument {
            value["vsti"] = plugin(instrument);
        }
        ChainOwner::from_value(&value, None).unwrap()
    }

    /// `owner` with the preset `preset` for its plugin at `stage`.
    fn with_preset(mut owner: ChainOwner, stage: usize, preset: &str) -> ChainOwner {
        let plugin = owner.plugins[stage].as_mut().expect("a plugin");
        plugin.preset = Some(preset.to_owned());
        owner
    }

    /// The seat of each owner, as one batch of notes that play.
    fn seats_of(registry: &mut ChainRegistry, owners: &[ChainOwner], now: Duration) -> Vec<Seat> {
        let owners: Vec<(ChainOwner, bool)> =
            owners.iter().map(|owner| (owner.clone(), true)).collect();
        let room = rustel_audio::MAX_INSERT_BUSES;
        let assignment = registry.assign(&owners, room, now, ChainRegistry::in_grace);
        let seats = assignment.seats.into_iter();
        seats.map(|seat| seat.expect("a chain")).collect()
    }

    /// The chain of each owner, as one batch of notes that play.
    fn chains_of(registry: &mut ChainRegistry, owners: &[ChainOwner], now: Duration) -> Vec<u64> {
        let seats = seats_of(registry, owners, now).into_iter();
        seats.map(|seat| seat.chain).collect()
    }

    /// The chain of one owner, as a batch of one note that plays.
    fn chain_of(registry: &mut ChainRegistry, owner: ChainOwner, now: Duration) -> u64 {
        chains_of(registry, &[owner], now)[0]
    }

    /// The stages of a seat: `seated`, then no stage.
    fn stages(seated: &[u8]) -> [u8; rustel_audio::EFFECT_CHAIN] {
        let mut stages = [rustel_audio::NO_STAGE; rustel_audio::EFFECT_CHAIN];
        stages[..seated.len()].copy_from_slice(seated);
        stages
    }

    /// Puts `score` in play: its lanes, then the score change of the
    /// registry. Gives the lane number of each labeled statement.
    fn change(
        lanes: &mut LaneTable,
        registry: &mut ChainRegistry,
        score: &str,
        now: Duration,
    ) -> Vec<u64> {
        let numbers = play(lanes, score);
        registry.score_changed(lanes.playing(), now);
        numbers
    }

    /// The copies the chains of `registry` want at `now`, as the host gets
    /// them: see [`Chains::wanted`].
    fn wanted(registry: &ChainRegistry, now: Duration) -> Vec<WantedCopy> {
        let mut chains = Chains::new();
        (chains.playing, chains.clock) = (registry.clone(), now);
        let (copies, _) = chains.wanted();
        copies.into_iter().map(wanted_copy).collect()
    }

    /// Gives the host the copies and the chains `chains` want now, as a
    /// call of [`keep_now`] with a host does. Gives the calls of this thread
    /// to the host so far.
    fn kept(chains: &Mutex<Chains>) -> usize {
        let mut chains = locked(chains);
        let (copies, named) = chains.wanted();
        let copies = copies.into_iter().map(wanted_copy).collect();
        chains.kept = (copies, named);
        HOST_CALLS.get()
    }

    /// The numbers of the notes of `chains` with a chain.
    fn numbers(chains: &[NoteChain]) -> Vec<u64> {
        let number = |chain: &NoteChain| match chain {
            NoteChain::Seat(seat) => Some(seat.chain),
            _ => None,
        };
        chains.iter().filter_map(number).collect()
    }

    /// A plugin host that serves each plugin call of a note at once.
    struct Plugins;

    impl rustel_voice::SampleLookup for Plugins {
        fn resolve(&self, s: &str, n: f64, midi: f64) -> rustel_voice::SampleResolution {
            rustel_voice::SampleLookup::resolve(&rustel_voice::BundledOnly, s, n, midi)
        }
        fn insert(
            &self,
            _: &rustel_voice::PluginRequest<'_>,
        ) -> Result<Option<InsertControls>, String> {
            Ok(Some(
                InsertControls::new(rustel_audio::InsertKey::default()),
            ))
        }
    }

    /// A hap of the first cycle with the value of a note.
    fn hap(value: &serde_json::Value) -> crate::HapJson {
        let cycle = crate::SpanJson {
            begin: "0".into(),
            end: "1".into(),
        };
        crate::HapJson {
            whole: Some(cycle.clone()),
            part: cycle,
            value: crate::ValueJson::Raw(value.clone()),
            context: Vec::new(),
            has_onset: true,
            show: String::new(),
        }
    }

    /// A note at the first frame, with the value `value`, of the generation
    /// `generation`.
    fn onset(value: &serde_json::Value, generation: u64) -> crate::OnsetEventJson {
        crate::OnsetEventJson {
            onset_id: 1,
            generation,
            whole_begin: "0/1".into(),
            duration_secs: 0.25,
            target_time: 0.0,
            live_controls: [0; 4],
            ui_visuals: 0,
            value: crate::ValueJson::Raw(value.clone()),
            value_show: String::new(),
            log_line: None,
        }
    }

    /// A chain owner is its lane and its plugin calls: the values of a note
    /// change no owner. Only a note with no lane has its orbit in its owner.
    /// The lane of a note comes from its tag and from the lanes of its text.
    #[test]
    fn plugin_ownership_separates_patterns_and_survives_control_edits() {
        let mut registry = ChainRegistry::new();
        let now = Duration::ZERO;
        let value = serde_json::json!({
            "__rustel_lane": "$0",
            "orbit": 1,
            "vst": [{"name": "Delay", "__rustel_binding": "p1", "mix": 0.2}]
        });
        let owner = |value: &serde_json::Value| ChainOwner::from_value(value, None).unwrap();
        let first = chain_of(&mut registry, owner(&value), now);
        let mut edited = value.clone();
        edited["orbit"] = 2.into();
        edited["vst"][0]["mix"] = 0.8.into();
        assert_eq!(chain_of(&mut registry, owner(&edited), now), first);
        // In one score, each lane, call and preset has a chain of its own.
        edited["__rustel_lane"] = "$1".into();
        assert_ne!(chain_of(&mut registry, owner(&edited), now), first);
        edited = value.clone();
        edited["vst"][0]["__rustel_binding"] = "p2".into();
        assert_ne!(chain_of(&mut registry, owner(&edited), now), first);
        edited = value.clone();
        edited["vst"][0]["preset"] = "Long".into();
        assert_ne!(chain_of(&mut registry, owner(&edited), now), first);
        assert!(ChainOwner::from_value(&serde_json::json!({"s": "sine"}), None).is_none());

        // `#n` is a lane number, or a place in a text whose label rewrite
        // wrote places. A note with no tag has the lane of the empty label
        // in a score with no label, and no lane with its orbit in a score
        // with labels, in a direct pattern and in a render. Each other tag
        // is a run-time name.
        let lane = |tag: Option<&str>, lanes: Option<&ScoreLanes>| {
            let mut value = serde_json::json!({"orbit": 3});
            if let Some(tag) = tag {
                value["__rustel_lane"] = tag.into();
            }
            OwnerLane::of(&value, lanes)
        };
        let (mut bare, mut labeled) = (LaneTable::default(), LaneTable::default());
        assert!(play(&mut bare, "note(\"c\").vsti(\"serum\")").is_empty());
        let score = "lead: note(\"c\").vsti(\"serum\")";
        assert_eq!(play(&mut labeled, score), [0]);
        let (bare, numbered) = (Some(bare.playing()), Some(labeled.playing()));
        assert_eq!(lane(None, bare), OwnerLane::Lane(0));
        assert_eq!(lane(None, numbered), OwnerLane::NoLane(3));
        assert_eq!(lane(None, None), OwnerLane::NoLane(3));
        assert_eq!(lane(Some("#0"), numbered), OwnerLane::Lane(0));
        assert_eq!(lane(Some("#0"), None), OwnerLane::Lane(0));
        assert_eq!(lane(Some("$0"), numbered), OwnerLane::Name("$0".into()));
        let mut placed = LaneTable::default();
        placed.read(score);
        placed.wrote_places();
        placed.install(score);
        assert_eq!(
            lane(Some("#0"), Some(placed.playing())),
            OwnerLane::Place(0)
        );
        let mut direct = LaneTable::default();
        play(&mut direct, "note(\"c\").vsti(\"serum\")");
        direct.install_pattern();
        assert_eq!(lane(None, Some(direct.playing())), OwnerLane::NoLane(3));

        // The plan of a text reads the lanes of that text: a lane keeps its
        // chain when its label goes.
        let (chains, csound) = (Mutex::new(Chains::new()), crate::csound::State::new(false));
        let note = |tag: Option<&str>| {
            let mut value =
                serde_json::json!({"s": "bd", "vst": [{"name": "ott", "__rustel_binding": "p0"}]});
            if let Some(tag) = tag {
                value["__rustel_lane"] = tag.into();
            }
            value
        };
        let mut lanes = LaneTable::default();
        let lead = r#"lead: s("bd").vst("ott")"#;
        play(&mut lanes, lead);
        score_changed(&chains, &lanes, Some(lead), true);
        let tagged = crate::ValueJson::Raw(note(Some("#0")));
        let playing = numbers(&assign(&chains, &lanes, &csound, [&tagged], false));
        let bare = r#"s("bd").vst("ott")"#;
        assert!(lanes.read(bare).is_empty());
        let needed = plan(&chains, &lanes, &csound, bare, &[hap(&note(None))], &[]);
        assert_eq!(needed[0].slot.chain, playing[0]);
    }

    /// The output runs 64 chains. The owner of a 65th chain gets no chain,
    /// with one line that names the label and the line of its lane, and
    /// its note plays with no plugin. A Csound note with no plugin plays on
    /// the chain of its orbit, so each Csound orbit takes a place, also in
    /// the first window after a commit. A chain that ends makes room, and
    /// its number does not come again. A chain that one registry ends to
    /// make room stays in the host while the other registry has it.
    #[test]
    fn plugin_registry_bounds_metadata_without_reusing_live_ids() {
        let line = |lane: usize| format!("l{lane}: note(\"c\").vsti(\"serum\").vst(\"ott\")\n");
        let score: String = (0..65).map(line).collect();
        let mut lanes = LaneTable::default();
        play(&mut lanes, &score);
        let note = |lane: usize| {
            serde_json::json!({
                "__rustel_lane": format!("#{lane}"),
                "s": "sine",
                "note": 48,
                "vsti": {"name": "serum", "__rustel_binding": format!("p{}", 2 * lane)},
                "vst": [{"name": "ott", "__rustel_binding": format!("p{}", 2 * lane + 1)}],
            })
        };
        let notes: Vec<crate::ValueJson> = (0..65)
            .map(|lane| crate::ValueJson::Raw(note(lane)))
            .collect();
        let no_csound = crate::csound::State::new(false);
        let chains = Mutex::new(Chains::new());
        let numbered = assign(&chains, &lanes, &no_csound, &notes, false);
        let distinct: HashSet<u64> = numbers(&numbered).into_iter().collect();
        assert_eq!(distinct.len(), rustel_audio::MAX_INSERT_BUSES);
        assert_eq!(numbered[64], NoteChain::Refused);
        // The next window gives the same line, and the lines keep it one
        // time. The copies of the notes leave out that lane.
        assert_eq!(assign(&chains, &lanes, &no_csound, &notes, false), numbered);
        let refused = "the lane 'l64' at line 65 plays with no plugin: 64 chains play at most";
        assert_eq!(take_problems(&chains), [refused]);
        let haps: Vec<crate::HapJson> = (0..65).map(|lane| hap(&note(lane))).collect();
        let needed = wants(&chains, &lanes, &no_csound, &haps, &[]);
        assert_eq!(needed.len(), 2 * rustel_audio::MAX_INSERT_BUSES);
        // A lane by its place, by its run-time name, and no lane.
        let view = lanes.playing();
        let said = |lane: OwnerLane| {
            no_room(&lane, view).replace(" plays with no plugin: 64 chains play at most", "")
        };
        assert_eq!(said(OwnerLane::Place(2)), "the lane 'l2' at line 3");
        assert_eq!(said(OwnerLane::Name("$0".into())), "the lane '$0'");
        assert_eq!(said(OwnerLane::NoLane(1)), "a note with no lane");
        let mut bare = LaneTable::default();
        play(&mut bare, "\nnote(\"c\").vsti(\"serum\")");
        assert_eq!(
            no_room(&OwnerLane::Lane(0), bare.playing()),
            "the lane at line 2 plays with no plugin: 64 chains play at most"
        );

        // The note of the 65th lane converts with its plugins. The chains of
        // a session window then give it none: no chain, no effect, no
        // instrument, gain 0 and no gain binding.
        let convert = |lane: usize| {
            let bound = crate::OnsetEventJson {
                live_controls: [7, 0, 0, 0],
                ..onset(&note(lane), 1)
            };
            let event = crate::render::live_audio_event(&bound, 48_000, 0.5, &Plugins, |_| None);
            event.expect("the note")
        };
        let mut events: Vec<rustel_audio::AudioEvent> = (0..65).map(convert).collect();
        let controls = &events[64].controls;
        let plugins = (controls.instrument.is_some(), controls.effects[0].is_some());
        assert_eq!((plugins, controls.live_controls[0]), ((true, true), 7));
        assert!(events[64].gain > 0.0);
        let values: Vec<&crate::ValueJson> = notes.iter().collect();
        let session = crate::Session::new().expect("session");
        session.chain_live_events(&mut events, &values);
        assert!(
            events[..64]
                .iter()
                .all(|event| event.controls.insert_chain.is_some())
        );
        let event = &events[64];
        assert_eq!(event.controls.insert_chain, None);
        assert!(event.controls.instrument.is_none());
        assert!(event.controls.effects.iter().all(Option::is_none));
        assert_eq!((event.gain, event.controls.live_controls[0]), (0.0, 0));

        // One Csound orbit leaves room for 63 plugin chains, also when its
        // first note comes in the window of the plugin notes, before the
        // Csound chains of the session have it.
        let csound = |orbit: u64| {
            crate::ValueJson::Raw(
                serde_json::json!({"s": "sine", "csound": "probe", "orbit": orbit}),
            )
        };
        let chains = Mutex::new(Chains::new());
        let window: Vec<crate::ValueJson> = std::iter::once(csound(2))
            .chain(notes[..64].iter().cloned())
            .collect();
        let numbered = assign(&chains, &lanes, &no_csound, &window, false);
        assert_eq!(numbered[0], NoteChain::None);
        assert_eq!(numbers(&numbered).len(), 63);
        assert_eq!(numbered[64], NoteChain::Refused);
        // The Csound chains of the session count too.
        let recorded = crate::csound::State::new(false);
        let score_event = InsertControls::new(rustel_audio::InsertKey::default()).with_score(1);
        recorded.record_chain(&rustel_audio::OscillatorControls {
            orbit: 2,
            instrument: Some(score_event),
            ..Default::default()
        });
        let chains = Mutex::new(Chains::new());
        let numbered = assign(&chains, &lanes, &recorded, &notes[..64], false);
        assert_eq!(numbers(&numbered).len(), 63);
        // A Csound orbit of the batch and of the session is one place.
        let chains = Mutex::new(Chains::new());
        let numbered = assign(&chains, &lanes, &recorded, &window, false);
        assert_eq!(numbers(&numbered).len(), 63);
        // A Csound note with a plugin effect has the chain of its owner, so
        // it takes one place.
        let chains = Mutex::new(Chains::new());
        let mut wet = note(63);
        wet["csound"] = "probe".into();
        let window: Vec<crate::ValueJson> = notes[..63]
            .iter()
            .cloned()
            .chain([crate::ValueJson::Raw(wet)])
            .collect();
        let numbered = assign(&chains, &lanes, &no_csound, &window, false);
        assert_eq!(numbers(&numbered).len(), 64);
        // The Csound chains of the session then take the chain of the note.
        // That chain is no orbit, so the next window gives the same chains
        // and no line.
        let recorded = crate::csound::State::new(false);
        recorded.record_chain(&rustel_audio::OscillatorControls {
            insert_chain: Some(numbers(&numbered)[63]),
            instrument: Some(score_event),
            ..Default::default()
        });
        assert_eq!(assign(&chains, &lanes, &recorded, &window, false), numbered);
        assert!(take_problems(&chains).is_empty());

        // A Csound orbit that sounds first when the room is full ends the
        // chain in grace with the oldest time: the lane `l0` left 2 s before
        // the lane `l1`.
        let chains = Mutex::new(Chains::new());
        let full = numbers(&assign(&chains, &lanes, &no_csound, &notes[..64], false));
        let fewer: String = (1..65).map(line).collect();
        play(&mut lanes, &fewer);
        score_changed(&chains, &lanes, Some(fewer.as_str()), true);
        locked(&chains).clock += Duration::from_secs(2);
        let fewest: String = (2..65).map(line).collect();
        play(&mut lanes, &fewest);
        score_changed(&chains, &lanes, Some(fewest.as_str()), true);
        let window: Vec<crate::ValueJson> =
            notes[2..64].iter().cloned().chain([csound(2)]).collect();
        assert_eq!(
            numbers(&assign(&chains, &lanes, &no_csound, &window, false)),
            full[2..]
        );
        assert!(!locked(&chains).playing.has(full[0]));
        assert!(locked(&chains).playing.has(full[1]));
        assert!(take_problems(&chains).is_empty());
        // The lane `l1` comes back before the end of its time: it keeps its
        // chain.
        play(&mut lanes, &fewer);
        score_changed(&chains, &lanes, Some(fewer.as_str()), true);
        // With no chain in grace, the list of the output leaves out the
        // newest chain, with one line. Its note plays with no plugin, so it
        // asks the output for no bus.
        let window: Vec<crate::ValueJson> = notes[1..64]
            .iter()
            .cloned()
            .chain([csound(2), csound(3)])
            .collect();
        let numbered = assign(&chains, &lanes, &no_csound, &window, false);
        assert_eq!(numbers(&numbered), full[1..63]);
        assert_eq!(numbered[62], NoteChain::Refused);
        let cut = "the lane 'l63' at line 63 plays with no plugin: 64 chains play at most";
        assert_eq!(take_problems(&chains), [cut]);
        // A third Csound orbit leaves out the 2 newest chains.
        let window: Vec<crate::ValueJson> = notes[1..64]
            .iter()
            .cloned()
            .chain([csound(2), csound(3), csound(4)])
            .collect();
        assert_eq!(
            numbers(&assign(&chains, &lanes, &no_csound, &window, false)),
            full[1..62]
        );
        // The lane `l0` comes back: its chain has a new number.
        play(&mut lanes, &score);
        score_changed(&chains, &lanes, Some(score.as_str()), true);
        let back = numbers(&assign(&chains, &lanes, &no_csound, &notes[..1], false));
        assert!(back[0] > *full.iter().max().expect("a chain"));

        // A chain in grace that a note of the batch uses does not end: a
        // direct pattern has no lanes, and its notes keep the tag of `l0`,
        // the lane that left first. The lane `l2` left 2 s later, before
        // `l1` and the others: its chain ends, the unused chain in grace
        // with the oldest time, and not the chain of `l1` with its lower
        // number.
        let mut registry = ChainRegistry::new();
        let start = Duration::ZERO;
        let owner = |lane: usize| ChainOwner::from_value(&note(lane), None).expect("an owner");
        let owners: Vec<(ChainOwner, bool)> = (0..64).map(|lane| (owner(lane), true)).collect();
        let room = rustel_audio::MAX_INSERT_BUSES;
        let full = registry
            .assign(&owners, room, start, ChainRegistry::in_grace)
            .seats;
        play(&mut lanes, &fewer);
        registry.score_changed(lanes.playing(), start);
        let gap: String = (1..65).filter(|lane| *lane != 2).map(line).collect();
        play(&mut lanes, &gap);
        registry.score_changed(lanes.playing(), start + Duration::from_secs(2));
        lanes.install_pattern();
        registry.score_changed(lanes.playing(), start + CHAIN_GRACE);
        registry.assign(
            &owners[..1],
            room - 1,
            start + CHAIN_GRACE,
            ChainRegistry::in_grace,
        );
        assert!(registry.has(full[0].expect("a chain").chain));
        assert!(registry.has(full[1].expect("a chain").chain));
        assert!(!registry.has(full[2].expect("a chain").chain));
        assert_eq!(registry.owners.len(), room - 1);

        // A window of a session gives the line as a plugin line.
        let mut session = crate::Session::new().expect("session");
        session.set_direct_diagnostic_logging(false);
        let score: String = (0..65)
            .map(|lane| format!("l{lane}: s(\"sine\").vst(\"ott\")\n"))
            .collect();
        session.evaluate(&score).expect("65 lanes");
        session.schedule_audio_at(0.0, 48_000).expect("events");
        let lines = session.take_diagnostics().into_iter();
        let lines = lines.filter(|line| line.kind == crate::PLUGIN_DIAGNOSTIC);
        let lines: Vec<String> = lines.map(|line| line.message).collect();
        let refused = lines
            .iter()
            .filter(|line| line.ends_with("plays with no plugin: 64 chains play at most"));
        assert_eq!(refused.count(), 1, "{lines:?}");

        // The plan of an edit that drops `l0` and adds a lane ends the chain
        // of `l0` in the plan to make room. The score in play keeps it, so
        // the host hears no end, also from a second plan. An undo then takes
        // the chain again.
        let in_play: String = (0..64).map(line).collect();
        let rest: String = (1..64).map(line).collect();
        {
            let (mut lanes, chains) = (LaneTable::default(), Mutex::new(Chains::new()));
            play(&mut lanes, &in_play);
            score_changed(&chains, &lanes, Some(in_play.as_str()), true);
            let full = numbers(&assign(&chains, &lanes, &no_csound, &notes[..64], true));
            let edit = format!("{rest}l64: note(\"e\").vsti(\"tone\")\n");
            let read = lanes.read(&edit);
            let tone = serde_json::json!({
                "__rustel_lane": format!("#{}", read[63]),
                "s": "sine",
                "vsti": {"name": "tone", "__rustel_binding": "p126"},
            });
            let mut haps: Vec<crate::HapJson> = (1..64).map(|lane| hap(&note(lane))).collect();
            haps.push(hap(&tone));
            plan(&chains, &lanes, &no_csound, &edit, &haps, &[]);
            let held = |chains: &Mutex<Chains>| {
                let chains = locked(chains);
                let (_, planned) = chains.plan.as_ref().expect("the plan");
                (chains.playing.has(full[0]), planned.has(full[0]))
            };
            assert_eq!(held(&chains), (true, false));
            let calls = kept(&chains);
            plan(&chains, &lanes, &no_csound, &edit, &haps, &[]);
            assert_eq!(HOST_CALLS.get(), calls, "a chain in play");
            lanes.read(&in_play);
            let haps: Vec<crate::HapJson> = (0..64).map(|lane| hap(&note(lane))).collect();
            let undo = plan(&chains, &lanes, &no_csound, &in_play, &haps, &[]);
            assert_eq!(undo[0].slot.chain, full[0]);
        }
        // A Csound orbit ends the chain of `l0` in the score in play to make
        // room, after `l0` left. The plan of the text with `l0` keeps the
        // chain, and the host hears no end.
        {
            let (mut lanes, chains) = (LaneTable::default(), Mutex::new(Chains::new()));
            play(&mut lanes, &in_play);
            score_changed(&chains, &lanes, Some(in_play.as_str()), true);
            let full = numbers(&assign(&chains, &lanes, &no_csound, &notes[..64], true));
            play(&mut lanes, &rest);
            score_changed(&chains, &lanes, Some(rest.as_str()), true);
            lanes.read(&in_play);
            let haps: Vec<crate::HapJson> = (0..64).map(|lane| hap(&note(lane))).collect();
            let back = plan(&chains, &lanes, &no_csound, &in_play, &haps, &[]);
            assert_eq!(back[0].slot.chain, full[0]);
            let calls = kept(&chains);
            let window: Vec<crate::ValueJson> =
                notes[1..64].iter().cloned().chain([csound(2)]).collect();
            assign(&chains, &lanes, &no_csound, &window, true);
            assert_eq!(HOST_CALLS.get(), calls, "a chain of the plan");
            let chains = locked(&chains);
            let (_, planned) = chains.plan.as_ref().expect("the plan");
            assert!(!chains.playing.has(full[0]) && planned.has(full[0]));
        }
    }

    /// An edit that leaves a plugin call as it is keeps the chain of the
    /// call, and so its copies: a lane above, a call above, a new preset.
    /// The host keeps the copy of the preset before for a time. The notes
    /// of a batch take their chains in one assignment, so 2 arms of a stack
    /// keep their chains when a new arm asks first. A lane with a
    /// different instrument gets a new chain, and the chain of the instrument
    /// it lost ends after its time. A lane with fewer effects keeps its
    /// chain, and so does a lane that drops its instrument. Each effect call
    /// sits on a stage of its chain, and keeps the stage through an edit. A
    /// lane with no statement takes a chain of a run-time lane that grows to
    /// it. The plan of an edit gives its chains before the edit is in play.
    #[test]
    fn an_edit_keeps_the_chains_the_new_score_still_has() {
        let mut lanes = LaneTable::default();
        let mut registry = ChainRegistry::new();
        let start = Duration::ZERO;
        let later = start + CHAIN_GRACE;
        let score = concat!(
            "lead: note(\"c\").vsti(\"serum\")\n",
            "bass: note(\"c1\").vsti(\"zyklop\").vst(\"ott\")\n",
            "fx: s(\"bd\").vst(\"ott\").vst(\"verb\")",
        );
        assert_eq!(change(&mut lanes, &mut registry, score, start), [0, 1, 2]);
        // The owners of the 3 lanes, after `above` plugin calls above them.
        let owners = |above: u32| {
            [
                owner("#0", &[], Some((above, "serum"))),
                owner("#1", &[(above + 2, "ott")], Some((above + 1, "zyklop"))),
                owner("#2", &[(above + 3, "ott"), (above + 4, "verb")], None),
            ]
        };
        let seats = seats_of(&mut registry, &owners(0), start);
        let chains: Vec<u64> = seats.iter().map(|seat| seat.chain).collect();
        let (serum, zyklop, fx) = (chains[0], chains[1], chains[2]);
        assert!(serum != zyklop && zyklop != fx);
        assert_eq!(registry.named(), [fx, zyklop, serum]);
        // A new chain sits the effects on its stages in call order.
        assert_eq!(seats[2].stages, stages(&[0, 1]));

        // A lane above with no plugin keeps each call number. A plugin call
        // above moves each call number after it: each lane keeps its chain.
        let above = format!("pad: s(\"hh\")\n{score}");
        assert_eq!(
            change(&mut lanes, &mut registry, &above, start),
            [3, 0, 1, 2]
        );
        assert_eq!(chains_of(&mut registry, &owners(0), start), chains);
        let call = format!("pad: s(\"hh\").vst(\"eq\")\n{score}");
        assert_eq!(
            change(&mut lanes, &mut registry, &call, start),
            [3, 0, 1, 2]
        );
        let pad = owner("#3", &[(0, "eq")], None);
        let mut moved = vec![pad];
        moved.extend(owners(1));
        let after = chains_of(&mut registry, &moved, start);
        assert_eq!(after[1..], chains);
        assert!(!chains.contains(&after[0]));

        // A new preset keeps the chain: one copy changes. The host keeps the
        // copy of the preset before CHAIN_GRACE more.
        let dark = call.replace("vsti(\"serum\")", "vsti(\"serum\", { preset: \"Dark\" })");
        assert_eq!(
            change(&mut lanes, &mut registry, &dark, start),
            [3, 0, 1, 2]
        );
        let instrument = rustel_audio::EFFECT_CHAIN;
        let preset = with_preset(owners(1)[0].clone(), instrument, "Dark");
        assert_eq!(chain_of(&mut registry, preset, start), serum);
        let presets = |registry: &ChainRegistry, now| {
            let copies = wanted(registry, now).into_iter();
            let copies = copies.filter(|copy| copy.slot == rustel_audio::instrument_slot(serum));
            copies.map(|copy| copy.preset).collect::<Vec<_>>()
        };
        let dark_preset = Some("Dark".to_owned());
        assert_eq!(presets(&registry, start), [None, dark_preset.clone()]);
        assert_eq!(presets(&registry, later), [dark_preset]);
        // Of a run of 4 presets in CHAIN_GRACE, the slot keeps the copy in
        // use, the copy before and the copy of the preset the run started
        // from, and not the copy between them.
        let second = Duration::from_secs(1);
        for (at, name) in [(1, "Soft"), (2, "Hard")] {
            let picked = with_preset(owners(1)[0].clone(), instrument, name);
            registry.score_changed(lanes.playing(), start + second * at);
            assert_eq!(chain_of(&mut registry, picked, start + second * at), serum);
        }
        let [hard, soft] = ["Hard", "Soft"].map(|name| Some(name.to_owned()));
        assert_eq!(presets(&registry, start + second * 2), [None, hard, soft]);

        // 2 arms of a stack with one plugin keep their chains when a third
        // arm asks first.
        let arms = |count: usize| {
            let arm = |at: usize| format!("note(\"{}\").vsti(\"tone\")", ["c", "e", "g"][at]);
            let arms: Vec<String> = (0..count).map(arm).collect();
            format!("{dark}\nkeys: stack({})", arms.join(", "))
        };
        assert_eq!(
            change(&mut lanes, &mut registry, &arms(2), start),
            [3, 0, 1, 2, 4]
        );
        let arm = |call: u32| owner("#4", &[], Some((call, "tone")));
        let pair = chains_of(&mut registry, &[arm(6), arm(7)], start);
        assert_eq!(
            change(&mut lanes, &mut registry, &arms(3), start),
            [3, 0, 1, 2, 4]
        );
        let three = chains_of(&mut registry, &[arm(8), arm(6), arm(7)], start);
        assert_eq!(three[1..], pair);
        assert!(!pair.contains(&three[0]));
        // 2 arms with the plugin in 2 presets change places: each keeps the
        // chain of its preset, also against the call order.
        let soft = |call: u32| with_preset(arm(call), instrument, "Soft");
        let hard = |call: u32| with_preset(arm(call), instrument, "Hard");
        registry.score_changed(lanes.playing(), start);
        let presets = chains_of(&mut registry, &[soft(9), hard(10)], start);
        registry.score_changed(lanes.playing(), start);
        assert_eq!(
            chains_of(&mut registry, &[hard(11), soft(12)], start),
            [presets[1], presets[0]]
        );
        // Arms that fit as well pair in call order: the k-th arm takes the
        // k-th chain.
        registry.score_changed(lanes.playing(), start);
        let twins = chains_of(&mut registry, &[arm(15), arm(16)], start);
        registry.score_changed(lanes.playing(), start);
        assert_eq!(chains_of(&mut registry, &[arm(17), arm(18)], start), twins);
        // An arm takes the chain that keeps the most copies of its effects,
        // also in a new order.
        registry.score_changed(lanes.playing(), start);
        let effects = [
            owner("#4", &[(19, "verb"), (20, "eq")], None),
            owner("#4", &[(21, "ott"), (22, "verb")], None),
        ];
        let made = chains_of(&mut registry, &effects, start);
        registry.score_changed(lanes.playing(), start);
        let swapped = owner("#4", &[(23, "verb"), (24, "ott")], None);
        let swapped = seats_of(&mut registry, &[swapped], start)[0];
        assert_eq!((swapped.chain, swapped.stages), (made[1], stages(&[1, 0])));

        // A lane with a different instrument gets a new chain.
        let vital = arms(3).replace("vsti(\"serum\", { preset: \"Dark\" })", "vsti(\"vital\")");
        assert_eq!(
            change(&mut lanes, &mut registry, &vital, start),
            [3, 0, 1, 2, 4]
        );
        let other = chain_of(&mut registry, owner("#0", &[], Some((1, "vital"))), start);
        assert!(![serum, zyklop, fx].contains(&other));

        // A lane with one effect less keeps its chain, so the instrument
        // keeps its copy. The host keeps the copy of the effect CHAIN_GRACE
        // more. The chain of the instrument the lead lost ends after its
        // time.
        let dry = vital.replace(".vsti(\"zyklop\").vst(\"ott\")", ".vsti(\"zyklop\")");
        assert_eq!(
            change(&mut lanes, &mut registry, &dry, start),
            [3, 0, 1, 2, 4]
        );
        let bass = owner("#1", &[], Some((2, "zyklop")));
        let bass = seats_of(&mut registry, &[bass], start)[0];
        assert_eq!((bass.chain, bass.stages), (zyklop, stages(&[])));
        // The effect copies of a chain, by stage.
        let effects = |registry: &ChainRegistry, now, chain: u64| {
            let copies = wanted(registry, now).into_iter();
            let copies = copies.filter(|copy| copy.slot.chain == chain);
            let copies = copies.filter(|copy| usize::from(copy.slot.stage) < instrument);
            let copies = copies.map(|copy| (copy.slot.stage, copy.name, copy.preset));
            copies.collect::<Vec<_>>()
        };
        let copy = |stage: u8, name: &str, preset: Option<&str>| {
            (stage, name.to_owned(), preset.map(str::to_owned))
        };
        assert_eq!(effects(&registry, start, zyklop), [copy(0, "ott", None)]);
        assert!(effects(&registry, later, zyklop).is_empty());
        assert_eq!(registry.settle(later), [serum]);

        // Each effect call of fx sits on a stage of the chain. A new effect
        // takes the lowest stage no effect of the chain and no key it dropped
        // holds: at the end, before the last, first. The other effects keep
        // their stages, and the engine takes the order from the note. A
        // removed effect leaves its stage. A new preset keeps the stage, also
        // when another effect leaves. 2 calls of one plugin keep their stages
        // in call order when a call above moves the call numbers. A new
        // preset of one of 2 calls of a plugin keeps its stage, and so does
        // its undo. 2 calls of one plugin keep their order with the other
        // effects. The host keeps the copy a stage lost CHAIN_GRACE more.
        let fx_of = |calls: &[(u32, &str)]| owner("#2", calls, None);
        let hard = |owner: ChainOwner, place: usize| with_preset(owner, place, "Hard");
        let seated = |registry: &mut ChainRegistry, owner: ChainOwner, now| {
            registry.score_changed(lanes.playing(), now);
            let seat = seats_of(registry, &[owner], now)[0];
            assert_eq!(seat.chain, fx);
            seat.stages
        };
        let at = |times: u32| later + CHAIN_GRACE * times;
        let end = fx_of(&[(30, "ott"), (31, "verb"), (32, "chorus")]);
        assert_eq!(seated(&mut registry, end, at(0)), stages(&[0, 1, 2]));
        let last = fx_of(&[(30, "ott"), (31, "chorus"), (32, "verb")]);
        assert_eq!(seated(&mut registry, last, at(0)), stages(&[0, 2, 1]));
        let first = fx_of(&[(30, "chorus"), (31, "ott"), (32, "verb")]);
        assert_eq!(
            seated(&mut registry, first.clone(), at(0)),
            stages(&[2, 0, 1])
        );
        // The notes of the owner keep its stages, and the host keeps the
        // copy of each stage.
        let again = seats_of(&mut registry, &[first], at(0))[0];
        assert_eq!(again.stages, stages(&[2, 0, 1]));
        let copies = [
            copy(0, "ott", None),
            copy(1, "verb", None),
            copy(2, "chorus", None),
        ];
        assert_eq!(effects(&registry, at(0), fx), copies);
        let removed = fx_of(&[(30, "ott"), (31, "verb")]);
        assert_eq!(
            seated(&mut registry, removed.clone(), at(1)),
            stages(&[0, 1])
        );
        assert_eq!(effects(&registry, at(1), fx), copies);
        assert_eq!(effects(&registry, at(2), fx), copies[..2]);
        assert_eq!(
            seated(&mut registry, hard(removed, 0), at(2)),
            stages(&[0, 1])
        );
        let presets = [copy(0, "ott", None), copy(0, "ott", Some("Hard"))];
        assert_eq!(effects(&registry, at(2), fx)[..2], presets);
        let twice = fx_of(&[(30, "ott"), (31, "ott")]);
        assert_eq!(seated(&mut registry, twice, at(3)), stages(&[0, 2]));
        let moved = fx_of(&[(31, "ott"), (32, "ott")]);
        assert_eq!(seated(&mut registry, moved, at(3)), stages(&[0, 2]));
        let hard_first = hard(fx_of(&[(31, "ott"), (32, "ott")]), 0);
        assert_eq!(seated(&mut registry, hard_first, at(3)), stages(&[0, 2]));
        let undo = fx_of(&[(31, "ott"), (32, "ott")]);
        assert_eq!(seated(&mut registry, undo, at(3)), stages(&[0, 2]));
        let once = fx_of(&[(32, "ott")]);
        assert_eq!(seated(&mut registry, once, at(4)), stages(&[0]));
        let left = [copy(0, "ott", None), copy(2, "ott", None)];
        assert_eq!(effects(&registry, at(4), fx), left);
        let chorus = fx_of(&[(30, "chorus"), (31, "ott")]);
        assert_eq!(seated(&mut registry, chorus, at(5)), stages(&[1, 0]));
        let chorus = hard(fx_of(&[(30, "chorus")]), 0);
        assert_eq!(seated(&mut registry, chorus, at(5)), stages(&[1]));
        let twice = fx_of(&[(30, "chorus"), (31, "chorus")]);
        assert_eq!(seated(&mut registry, twice, at(5)), stages(&[1, 2]));
        let moved = fx_of(&[(31, "chorus"), (32, "chorus")]);
        assert_eq!(seated(&mut registry, moved, at(5)), stages(&[1, 2]));
        // 2 calls of one plugin in 2 presets each keep their stage, also in a
        // new order.
        let presets = hard(fx_of(&[(31, "chorus"), (32, "chorus")]), 0);
        assert_eq!(seated(&mut registry, presets, at(5)), stages(&[1, 2]));
        let swapped = hard(fx_of(&[(31, "chorus"), (32, "chorus")]), 1);
        assert_eq!(seated(&mut registry, swapped, at(5)), stages(&[2, 1]));
        // A new effect takes a stage no effect of the chain holds, so the
        // effect that leaves fades out in its place. 2 calls of one plugin
        // keep their order with the other effects.
        let added = hard(fx_of(&[(31, "chorus"), (32, "verb")]), 0);
        assert_eq!(seated(&mut registry, added, at(5)), stages(&[1, 3]));
        let around = hard(fx_of(&[(31, "chorus"), (32, "verb"), (33, "chorus")]), 0);
        let around = hard(around, 2);
        assert_eq!(seated(&mut registry, around, at(5)), stages(&[1, 3, 0]));
        let kept_order = hard(fx_of(&[(32, "verb"), (33, "chorus")]), 1);
        assert_eq!(seated(&mut registry, kept_order, at(5)), stages(&[3, 0]));
        // A lane that drops its instrument and keeps an effect keeps its
        // chain, and the effect keeps its stage. The host keeps the copy of
        // the instrument CHAIN_GRACE more.
        let toned = owner("#2", &[(31, "verb")], Some((30, "tone")));
        assert_eq!(seated(&mut registry, toned, at(6)), stages(&[3]));
        let plain = fx_of(&[(30, "verb")]);
        assert_eq!(seated(&mut registry, plain, at(6)), stages(&[3]));
        let instruments = |registry: &ChainRegistry, now| {
            let copies = wanted(registry, now).into_iter();
            let copies = copies.filter(|copy| copy.slot == rustel_audio::instrument_slot(fx));
            copies.map(|copy| copy.name).collect::<Vec<_>>()
        };
        assert_eq!(instruments(&registry, at(6)), ["tone"]);
        assert!(instruments(&registry, at(7)).is_empty());
        // Each stage of a chain with 4 effects holds an effect. A new
        // effect then takes the lowest free stage: the stage of the effect
        // that leaves. The host keeps the copy of that effect CHAIN_GRACE
        // more.
        let four = fx_of(&[(30, "ott"), (31, "chorus"), (32, "eq"), (33, "verb")]);
        assert_eq!(seated(&mut registry, four, at(7)), stages(&[0, 1, 2, 3]));
        let replaced = fx_of(&[(30, "ott"), (31, "delay"), (32, "chorus"), (33, "eq")]);
        assert_eq!(
            seated(&mut registry, replaced, at(7)),
            stages(&[0, 3, 1, 2])
        );
        let copies = [
            copy(0, "ott", None),
            copy(1, "chorus", None),
            copy(2, "eq", None),
            copy(3, "delay", None),
            copy(3, "verb", None),
        ];
        assert_eq!(effects(&registry, at(7), fx), copies);
        assert_eq!(effects(&registry, at(8), fx), copies[..4]);
        // With 3 effects and a dropped key on the fourth stage, a new effect
        // takes the stage of the key before the stage of an effect that
        // leaves: the engine empties it at the next note.
        let three = fx_of(&[(30, "ott"), (31, "chorus"), (32, "eq")]);
        assert_eq!(seated(&mut registry, three, at(8)), stages(&[0, 1, 2]));
        let flanger = fx_of(&[(30, "chorus"), (31, "eq"), (32, "flanger")]);
        assert_eq!(seated(&mut registry, flanger, at(8)), stages(&[1, 2, 3]));
        // One plugin in place of another takes a stage of its own. Its undo
        // in CHAIN_GRACE puts the plugin back on the stage of its dropped
        // key, so the copy of that stage plays again.
        let swapped = fx_of(&[(30, "chorus"), (31, "phaser"), (32, "flanger")]);
        assert_eq!(seated(&mut registry, swapped, at(8)), stages(&[1, 0, 3]));
        let undone = fx_of(&[(30, "chorus"), (31, "eq"), (32, "flanger")]);
        assert_eq!(seated(&mut registry, undone, at(8)), stages(&[1, 2, 3]));
        assert!(effects(&registry, at(8), fx).contains(&copy(2, "eq", None)));

        // A lane with no statement: a `$` lane above moves the name of each
        // lane after it, and a call above moves each call number. An owner
        // takes the chain of a run-time lane that grows to it. It does not
        // take the chain of its new name, with the same effect and another
        // instrument. So no copy builds.
        let mut registry = ChainRegistry::new();
        let none = ScoreLanes::default();
        registry.score_changed(&none, start);
        let named = |lane: u32, above: u32| {
            [
                owner(
                    &format!("${lane}"),
                    &[(above + 1, "ott")],
                    Some((above, "serum")),
                ),
                owner(
                    &format!("${}", lane + 1),
                    &[(above + 3, "ott")],
                    Some((above + 2, "zyklop")),
                ),
            ]
        };
        let lead = chains_of(&mut registry, &named(0, 0), start);
        let built = wanted(&registry, start);
        registry.score_changed(&none, start);
        assert_eq!(chains_of(&mut registry, &named(1, 0), start), lead);
        assert_eq!(wanted(&registry, start), built);
        registry.score_changed(&none, start);
        let mut eq = vec![owner("$0", &[(0, "eq")], None)];
        eq.extend(named(1, 1));
        let after = chains_of(&mut registry, &eq, start);
        assert_eq!(after[1..], lead);
        assert!(!lead.contains(&after[0]));
        let copies = wanted(&registry, start).into_iter();
        let copies: Vec<WantedCopy> = copies.filter(|copy| copy.slot.chain != after[0]).collect();
        assert_eq!(copies, built);
        // A new lane whose note asks first takes no chain of a lane that
        // plays.
        registry.score_changed(&none, start);
        let new = owner("$3", &[(6, "ott")], Some((5, "serum")));
        let first = chains_of(&mut registry, &[new, named(1, 1)[0].clone()], start);
        assert_eq!(first[1], lead[0]);
        assert!(!lead.contains(&first[0]) && first[0] != after[0]);
        // Of an unused chain with the plugins of the owner and an unused
        // shorter chain that grows to it, the owner takes the first one.
        registry.score_changed(&none, start);
        let short = owner("$4", &[(5, "chorus")], None);
        let wide = owner("$5", &[(5, "chorus"), (6, "delay")], None);
        let made = chains_of(&mut registry, &[short, wide], start);
        registry.score_changed(&none, start);
        let grown = owner("$6", &[(7, "chorus"), (8, "delay")], None);
        assert_eq!(chain_of(&mut registry, grown, start), made[1]);
        // With no unused chain of its plugins, the owner takes the shorter
        // chain.
        registry.score_changed(&none, start);
        let longer = owner("$7", &[(9, "chorus"), (10, "flanger")], None);
        assert_eq!(chain_of(&mut registry, longer, start), made[0]);
        // An owner of a run-time lane sits on the stages of the chain it
        // takes: a lane with one effect less, then a new name with a new
        // effect at the end.
        registry.score_changed(&none, start);
        let wet = owner("$13", &[(11, "ringmod"), (12, "gate")], None);
        let wet = chain_of(&mut registry, wet, start);
        registry.score_changed(&none, start);
        let dry = seats_of(&mut registry, &[owner("$13", &[(12, "gate")], None)], start)[0];
        assert_eq!((dry.chain, dry.stages), (wet, stages(&[1])));
        registry.score_changed(&none, start);
        let renamed = owner("$14", &[(13, "gate"), (14, "ringmod")], None);
        let renamed = seats_of(&mut registry, &[renamed], start)[0];
        assert_eq!((renamed.chain, renamed.stages), (wet, stages(&[1, 0])));
        // A new effect takes no stage of an effect the edit before removed,
        // also with no note between: the engine runs that effect until the
        // next note, so it fades out in its place.
        registry.score_changed(&none, start);
        let gate = seats_of(&mut registry, &[owner("$14", &[(13, "gate")], None)], start)[0];
        assert_eq!((gate.chain, gate.stages), (wet, stages(&[1])));
        registry.score_changed(&none, start);
        let delay = owner("$14", &[(13, "gate"), (15, "delay")], None);
        let delay = seats_of(&mut registry, &[delay], start)[0];
        assert_eq!((delay.chain, delay.stages), (wet, stages(&[1, 2])));
        // A place takes a chain as a run-time name does.
        registry.score_changed(&none, start);
        let place = |place: u64, owner: ChainOwner| ChainOwner {
            lane: OwnerLane::Place(place),
            ..owner
        };
        let placed = chain_of(
            &mut registry,
            place(0, owner("", &[(8, "verb")], None)),
            start,
        );
        registry.score_changed(&none, start);
        let moved = place(1, owner("", &[(9, "verb")], None));
        assert_eq!(chain_of(&mut registry, moved, start), placed);
        // An owner of a lane of the table takes no chain of another lane,
        // also with the same plugins.
        registry.score_changed(&none, start);
        let numbered = chain_of(&mut registry, owner("#7", &[(10, "verb")], None), start);
        assert_ne!(numbered, placed);
        registry.score_changed(&none, start);
        let other = chain_of(&mut registry, owner("#8", &[(10, "verb")], None), start);
        assert!(other != numbered && other != placed);
        // An owner of a run-time lane takes no chain of a lane of the table.
        let table = chain_of(&mut registry, owner("#9", &[(14, "phaser")], None), start);
        registry.score_changed(&none, start);
        let loose = chain_of(&mut registry, owner("$12", &[(14, "phaser")], None), start);
        assert_ne!(loose, table);

        // The plan of a text that is not in play changes no chain of the
        // score in play. The text has one more lane. Its next try has one
        // more call above that lane: the 2 plans give the lane one number.
        let mut chains = Chains::new();
        chains.playing = registry;
        let before = chains.playing.clone();
        let kept = named(1, 1)[0].clone();
        let harp = |lane: u32, call: u32| owner(&format!("${lane}"), &[], Some((call, "harp")));
        let (planned, _) = chains.plan("a lane more", &none, start);
        let tried = chains_of(planned, &[kept.clone(), harp(9, 11)], start);
        assert_eq!(tried[0], lead[0]);
        assert!(!before.has(tried[1]));
        let moved = harp(10, 12);
        let (planned, ended) = chains.plan("a call more", &none, start);
        assert_eq!(
            chains_of(planned, &[kept.clone(), moved.clone()], start),
            tried
        );
        assert!(ended.is_empty() && chains.playing == before);

        // The score in play makes a chain for the new lane while the
        // session has the plan. An engine holds the copies of that chain:
        // the plan takes it, and the chain the plan made ends.
        let sounding = chain_of(&mut chains.playing, moved.clone(), start);
        let (planned, ended) = chains.plan("a call more", &none, start);
        assert_eq!(chain_of(planned, moved.clone(), start), sounding);
        assert_eq!(ended, [tried[1]]);

        // A different text goes in play: the plan stays. The text of the
        // plan goes in play: each note has the number the plan gave, and a
        // chain the score in play made after the plan stays.
        let late = owner("$11", &[(13, "flanger")], None);
        let made = chain_of(&mut chains.playing, late.clone(), start);
        assert!(
            chains
                .score_changed(Some("s('bd')"), &none, start)
                .is_empty()
        );
        assert!(chains.plan.is_some());
        assert!(
            chains
                .score_changed(Some("a call more"), &none, start)
                .is_empty()
        );
        assert!(chains.plan.is_none());
        assert_eq!(chain_of(&mut chains.playing, moved, start), sounding);
        assert_eq!(chain_of(&mut chains.playing, kept, start), lead[0]);
        assert_eq!(chain_of(&mut chains.playing, late, start), made);
    }

    /// A chain that is new in the text takes no chain of a lane with a
    /// rest. A lane with a rest keeps its chain through each edit, also a
    /// long time. The chain of a lane that leaves the text ends after its
    /// time, and a lane that comes back before then keeps its chain. A
    /// chain of a run-time lane, or of no lane, ends after a long time with
    /// no note. A direct pattern has no lanes: each lane leaves, and its
    /// notes take no chain of a lane. A session with no live output ends the
    /// chain of a lane that leaves at once.
    #[test]
    fn a_new_chain_takes_no_chain_of_a_lane_with_a_rest() {
        use rustel_core::Value;
        let mut lanes = LaneTable::default();
        let mut registry = ChainRegistry::new();
        let start = Duration::ZERO;
        let long = start + CHAIN_GRACE + CHAIN_UNSEEN_LIMIT;
        let score = |count: usize| {
            let lane =
                |at: usize| format!("$: note(\"{}\").vsti(\"serum\")\n", ["c", "e", "g"][at]);
            (0..count).map(lane).collect::<String>()
        };
        let serum = |lane: u64| owner(&format!("#{lane}"), &[], Some((lane as u32, "serum")));
        assert_eq!(change(&mut lanes, &mut registry, &score(2), start), [0, 1]);
        let first = chain_of(&mut registry, serum(0), start);
        let rest = chain_of(&mut registry, serum(1), start);

        assert_eq!(
            change(&mut lanes, &mut registry, &score(3), start),
            [0, 1, 2]
        );
        assert_eq!(chain_of(&mut registry, serum(0), start), first);
        let new = chain_of(&mut registry, serum(2), start);
        assert!(new != first && new != rest);
        // The lane with the rest plays again a long time after.
        assert!(registry.settle(long).is_empty());
        assert_eq!(chain_of(&mut registry, serum(1), long), rest);

        // A lane that left the text and came back before the end of its
        // time keeps its chain.
        assert_eq!(change(&mut lanes, &mut registry, &score(2), start), [0, 1]);
        assert_eq!(
            change(&mut lanes, &mut registry, &score(3), start),
            [0, 1, 2]
        );
        assert!(registry.settle(start + CHAIN_GRACE).is_empty());
        assert_eq!(registry.owners.len(), 3);
        // The chain of a lane that left the text ends after its time.
        assert_eq!(change(&mut lanes, &mut registry, &score(2), start), [0, 1]);
        assert!(registry.settle(start).is_empty());
        assert_eq!(registry.settle(start + CHAIN_GRACE), [new]);

        // A chain of a run-time lane with no note since the score change
        // ends after a long time.
        let mut registry = ChainRegistry::new();
        let none = ScoreLanes::default();
        registry.score_changed(&none, start);
        let shown = chain_of(&mut registry, owner("$0", &[], Some((0, "serum"))), start);
        let hidden = chain_of(&mut registry, owner("$1", &[], Some((1, "serum"))), start);
        registry.score_changed(&none, start);
        assert_eq!(
            chain_of(&mut registry, owner("$0", &[], Some((0, "serum"))), start),
            shown
        );
        assert!(registry.settle(start + CHAIN_GRACE).is_empty());
        assert_eq!(registry.settle(long), [hidden]);

        // A direct pattern has no lanes: each lane leaves. A chain of a note
        // with no lane ends after a long time with no note.
        let mut lanes = LaneTable::default();
        let mut registry = ChainRegistry::new();
        let ott = r#"$: s("bd").vst("ott")"#;
        assert_eq!(change(&mut lanes, &mut registry, ott, start), [0]);
        let laned = chain_of(&mut registry, owner("#0", &[(0, "ott")], None), start);
        let loose = chain_of(&mut registry, owner("", &[(1, "ott")], None), start);
        lanes.install_pattern();
        registry.score_changed(lanes.playing(), start);
        assert_eq!(registry.settle(start + CHAIN_GRACE), [laned]);
        assert_eq!(registry.settle(long), [loose]);
        // The notes of a direct pattern of a session take no chain of the
        // lane of the score before. A session with no live output ends the
        // chain of that lane at once: the score that comes back takes a new
        // chain.
        let mut session = crate::Session::new().expect("session");
        let chain = |session: &mut crate::Session| {
            let events = session.schedule_audio_at(0.0, 48_000).expect("events");
            events[0].controls.insert_chain.expect("a chain")
        };
        let score = r#"s("sine").vst("ott")"#;
        session.evaluate(score).expect("score");
        let before = chain(&mut session);
        let ott = Value::object(vec![("name".into(), Value::Str("ott".into()))]);
        let note = vec![
            ("s".into(), Value::Str("sine".into())),
            ("vst".into(), Value::List(vec![ott])),
        ];
        session
            .set_pattern(rustel_core::pure(Value::object(note)))
            .expect("a direct pattern");
        session.restart_transport_at(0.0);
        let direct = chain(&mut session);
        assert_ne!(direct, before);
        session.evaluate(score).expect("the score back");
        session.restart_transport_at(0.0);
        let back = chain(&mut session);
        assert!(back != before && back != direct);
    }

    /// A chain with a new preset keeps its number. A chain ends after its
    /// time when its lane leaves the text, or when the body of its lane
    /// loses its instrument or the plugin of each stage. A chain with a
    /// plugin the body does not name ends a long time after its last note.
    #[test]
    fn a_chain_that_left_the_text_goes_after_its_time() {
        let mut lanes = LaneTable::default();
        let mut registry = ChainRegistry::new();
        let start = Duration::ZERO;
        let later = start + CHAIN_GRACE;
        let preset = |name: &str| {
            let lead = format!("lead: note(\"c\").vsti(\"serum\", {{ preset: \"{name}\" }})");
            let owner = owner("#0", &[], Some((0, "serum")));
            (lead, with_preset(owner, rustel_audio::EFFECT_CHAIN, name))
        };
        let (dark, dark_owner) = preset("Dark");
        let (soft, soft_owner) = preset("Soft");
        assert_eq!(change(&mut lanes, &mut registry, &dark, start), [0]);
        let before = chain_of(&mut registry, dark_owner, start);
        assert!(registry.settle(later).is_empty());
        assert_eq!(change(&mut lanes, &mut registry, &soft, start), [0]);
        assert_eq!(chain_of(&mut registry, soft_owner, start), before);

        // The clock counts the turns of the producer, 1 s for one turn at
        // most: a pause of 700 s counts 1 s. A time that goes back counts
        // nothing, and so does a time that is not a number. A session with
        // no chain counts no turn.
        let mut chains = Chains::new();
        chains.tick(0.0);
        chains.tick(2.0);
        assert_eq!(chains.clock, Duration::ZERO, "no chain");
        chains.playing = registry;
        for now in [10.0, 10.5, f64::NAN, 710.5, 700.0, 700.25] {
            chains.tick(now);
        }
        assert_eq!(chains.clock, Duration::from_millis(1750));
        // The lane leaves the text: its chain ends after 5 s of turns.
        let bass = "bass: note(\"c1\").vsti(\"zyklop\")";
        assert_eq!(play(&mut lanes, bass), [1]);
        chains.score_changed(Some(bass), lanes.playing(), chains.clock);
        for turn in 1..=5 {
            assert!(chains.settle(chains.clock).is_empty(), "turn {turn}");
            chains.tick(700.25 + f64::from(turn));
        }
        assert_eq!(chains.settle(chains.clock), [before]);

        // An edit takes the instrument call of lead and bass, and one effect
        // call of keys. It puts the only effect call of fx in a comment
        // inside the lane. The pad takes its plugin from a variable. Such
        // chains are in grace from the same time, so an output with no room
        // ends the chain of lead first: it has the lowest number. The chain
        // of keys is not in grace. The chains of lead and fx end after their
        // time: the notes of fx play with no plugin, so no note uses its
        // chain. Bass names its plugin again in that time and keeps its
        // chain. Keys rests and keeps its chain.
        let mut lanes = LaneTable::default();
        let mut registry = ChainRegistry::new();
        let score = concat!(
            "lead: note(\"c\").vsti(\"serum\").vst(\"eq\")\n",
            "bass: note(\"c1\").vsti(\"zyklop\")\n",
            "fx: s(\"bd\").vst(\"ott\")\n",
            "keys: note(\"e\").vsti(\"tone\").vst(\"ott\")\n",
            "pad: s(\"hh\").vst(\"chorus\")",
        );
        let lanes_of = [0, 1, 2, 3, 4];
        assert_eq!(change(&mut lanes, &mut registry, score, start), lanes_of);
        let pad = owner("#4", &[(6, "chorus")], None);
        let owners = [
            owner("#0", &[(1, "eq")], Some((0, "serum"))),
            owner("#1", &[], Some((2, "zyklop"))),
            owner("#2", &[(3, "ott")], None),
            owner("#3", &[(5, "ott")], Some((4, "tone"))),
            pad.clone(),
        ];
        let played = chains_of(&mut registry, &owners, start);
        let edit = concat!(
            "const chorus = \"chorus\"\n",
            "lead: note(\"c\").vsti(\"vital\").vst(\"eq\")\n",
            "bass: note(\"c1\").vsti(\"dexed\")\n",
            "fx: s(\"bd\")\n  // .vst(\"ott\")\n  .gain(1)\n",
            "keys: note(\"e\").vsti(\"tone\")\n",
            "pad: s(\"hh\").vst(chorus)",
        );
        assert_eq!(change(&mut lanes, &mut registry, edit, start), lanes_of);
        let spare = registry.in_grace().expect("a chain in grace");
        assert_eq!(spare, owners[0]);
        // A note of the pad uses its chain.
        let note = start + Duration::from_secs(1);
        assert_eq!(chain_of(&mut registry, pad, note), played[4]);
        let almost = start + Duration::from_secs(4);
        assert!(registry.settle(almost).is_empty());
        let back = edit.replace("dexed", "zyklop");
        assert_eq!(change(&mut lanes, &mut registry, &back, almost), lanes_of);
        // The host keeps the copy of the effect keys lost for that time, and
        // the copy of the pad while its notes use it.
        let names = |registry: &ChainRegistry, now, chain: u64| {
            let copies = wanted(registry, now).into_iter();
            let copies = copies.filter(|copy| copy.slot.chain == chain);
            copies.map(|copy| copy.name).collect::<Vec<_>>()
        };
        assert_eq!(names(&registry, almost, played[3]), ["ott", "tone"]);
        let mut gone = registry.settle(later);
        gone.sort_unstable();
        assert_eq!(gone, [played[0], played[2]]);
        assert_eq!(names(&registry, later, played[3]), ["tone"]);
        assert_eq!(names(&registry, later, played[4]), ["chorus"]);
        // Bass leaves the text, and comes back with a new instrument: its
        // body lost the plugin of its chain, which ends after its time.
        let without = back.replace("bass: note(\"c1\").vsti(\"zyklop\")\n", "");
        let four = change(&mut lanes, &mut registry, &without, later);
        assert_eq!(four, [0, 2, 3, 4]);
        let returned = later + Duration::from_secs(1);
        assert_eq!(change(&mut lanes, &mut registry, edit, returned), lanes_of);
        let ends = returned + CHAIN_GRACE;
        assert!(registry.settle(ends - Duration::from_millis(1)).is_empty());
        assert_eq!(registry.settle(ends), [played[1]]);
        // The pad has no note after the edit: its chain ends 600 s after its
        // last note.
        let unseen = note + CHAIN_UNSEEN_LIMIT;
        assert!(
            registry
                .settle(unseen - Duration::from_millis(1))
                .is_empty()
        );
        assert_eq!(registry.settle(unseen), [played[4]]);
        assert_eq!(registry.owners.len(), 1, "keys");

        // The chains settle while the session has a plan. Lead and bass
        // leave the score in play. A plan of a text with bass and pad uses
        // the chain of bass, and makes a chain for pad. The next plan, of a
        // text with bass alone, uses the chain of bass too: it stays. The
        // chain of lead ends after its time, and leaves the plan too. The
        // chain only the last plan made ends after its time, and so does a
        // chain the score in play made after the plans. The session forgets
        // the plan, and the chain of bass ends.
        let mut lanes = LaneTable::default();
        let mut chains = Chains::new();
        let both = "lead: note(\"c\").vsti(\"serum\")\nbass: note(\"c1\").vsti(\"zyklop\")";
        assert_eq!(play(&mut lanes, both), [0, 1]);
        chains.score_changed(Some(both), lanes.playing(), start);
        let bass = owner("#1", &[], Some((1, "zyklop")));
        let owners = [owner("#0", &[], Some((0, "serum"))), bass.clone()];
        let played = chains_of(&mut chains.playing, &owners, start);
        let kick = "kick: s(\"bd\").vst(\"ott\")";
        assert_eq!(play(&mut lanes, kick), [2]);
        chains.score_changed(Some(kick), lanes.playing(), start);
        let first = "bass: note(\"c1\").vsti(\"zyklop\")\npad: note(\"e\").vsti(\"tone\")";
        assert_eq!(lanes.read(first), [1, 3]);
        let read = lanes.candidate(first).expect("the read");
        let (planned, _) = chains.plan(first, read, start);
        let owners = [bass.clone(), owner("#3", &[], Some((2, "tone")))];
        let made = chains_of(planned, &owners, start);
        assert_eq!(made[0], played[1]);
        // The plugins of the chains in their grace do not count for the
        // unload of the plugins: the chains of lead and bass in play. The
        // chains of the plan count.
        assert_eq!(chains.plugin_names(), ["tone", "zyklop"]);
        let second = "bass: note(\"c1\").vsti(\"zyklop\")";
        assert_eq!(lanes.read(second), [1]);
        let read = lanes.candidate(second).expect("the read");
        let (planned, _) = chains.plan(second, read, start);
        assert_eq!(chain_of(planned, bass, start), played[1]);
        let late = chain_of(&mut chains.playing, owner("#2", &[(3, "ott")], None), start);
        let hat = "hat: note(\"c e g\").gain(0.5)";
        assert_eq!(play(&mut lanes, hat), [4]);
        chains.score_changed(Some(hat), lanes.playing(), start);
        let mut gone = chains.settle(later);
        gone.sort_unstable();
        assert_eq!(gone, [played[0], made[1], late]);
        let (_, planned) = chains.plan.as_ref().expect("the plan");
        assert!(planned.has(played[1]) && planned.owners.len() == 1);
        assert!(chains.forget().is_empty());
        assert_eq!(chains.settle(later), [played[1]]);

        // A plan of a text with a new preset for lead and a new lane: the
        // host keeps the copies of the score in play and of the plan, so the
        // copies of both presets of lead. The commit of the plan 10 s later
        // keeps the copy of the preset before CHAIN_GRACE more.
        let (dark, dark_owner) = preset("Dark");
        let (soft, soft_owner) = preset("Soft");
        let mut lanes = LaneTable::default();
        let mut chains = Chains::new();
        assert_eq!(play(&mut lanes, &dark), [0]);
        chains.score_changed(Some(dark.as_str()), lanes.playing(), start);
        let lead = chain_of(&mut chains.playing, dark_owner, start);
        let text = format!("{soft}\npad: note(\"e\").vsti(\"tone\")");
        assert_eq!(lanes.read(&text), [0, 1]);
        let read = lanes.candidate(&text).expect("the read");
        let (planned, _) = chains.plan(&text, read, start);
        let owners = [soft_owner, owner("#1", &[], Some((1, "tone")))];
        let made = chains_of(planned, &owners, start);
        assert_eq!(made[0], lead);
        // The chain of lead in play and in the plan gives its plugin one
        // time.
        assert_eq!(chains.plugin_names(), ["serum", "tone"]);
        let presets = |chains: &Chains| {
            let (copies, named) = chains.wanted();
            let copies = copies.into_iter().filter(|(slot, ..)| slot.chain == lead);
            let presets = copies.map(|(.., preset)| preset.map(str::to_owned));
            (presets.collect::<Vec<_>>(), named)
        };
        let both_presets = vec![Some("Dark".to_owned()), Some("Soft".to_owned())];
        chains.clock = start + 2 * CHAIN_GRACE;
        assert_eq!(presets(&chains), (both_presets.clone(), made.clone()));
        lanes.install(&text);
        chains.score_changed(Some(text.as_str()), lanes.playing(), chains.clock);
        assert_eq!(presets(&chains).0, both_presets);
        // The commit of the next plan in that time keeps the time of the key
        // the score in play lost.
        chains.clock += CHAIN_GRACE / 2;
        let next = format!("{text}\nhat: s(\"hh\")");
        assert_eq!(lanes.read(&next), [0, 1, 2]);
        let read = lanes.candidate(&next).expect("the read");
        chains.plan(&next, read, chains.clock);
        lanes.install(&next);
        chains.score_changed(Some(next.as_str()), lanes.playing(), chains.clock);
        assert_eq!(presets(&chains).0, both_presets);
        chains.clock += CHAIN_GRACE / 2;
        assert_eq!(presets(&chains).0, [Some("Soft".to_owned())]);
        chains.settle(chains.clock);
        let mut registered = chains.playing.owners.values();
        assert!(
            registered.all(|chain| chain.dropped.is_empty()),
            "a lost key"
        );

        // A session with no live output counts no turn: the chain of a lane
        // that left ends at once, and the host hears nothing of it. With a
        // live output the chain has its time.
        let csound = crate::csound::State::new(false);
        let note = |lane: &str, call: u32, name: &str| {
            serde_json::json!({
                "__rustel_lane": lane,
                "vsti": {"name": name, "__rustel_binding": format!("p{call}")},
            })
        };
        let serum = crate::ValueJson::Raw(note("#0", 0, "serum"));
        for live in [true, false] {
            let calls = HOST_CALLS.get();
            let (mut lanes, chains) = (LaneTable::default(), Mutex::new(Chains::new()));
            play(&mut lanes, both);
            score_changed(&chains, &lanes, Some(both), live);
            let chain = numbers(&assign(&chains, &lanes, &csound, [&serum], live))[0];
            play(&mut lanes, kick);
            score_changed(&chains, &lanes, Some(kick), live);
            assert_eq!(locked(&chains).playing.has(chain), live);
            assert_eq!(HOST_CALLS.get() > calls, live, "a call of the host");
        }

        // The host of a live output hears each change of the copies the
        // chains want: an assignment, a plan, a plan that goes, a look, a
        // commit and a chain that ends. With no change it hears nothing. A
        // session the host keeps copies for tells the host when it ends. The
        // door of the copies on their way home asks the host only for chains
        // that want a copy.
        let (mut lanes, chains) = (LaneTable::default(), Mutex::new(Chains::new()));
        play(&mut lanes, both);
        score_changed(&chains, &lanes, Some(both), true);
        let calls = kept(&chains);
        assert_eq!(returning(&chains, 48_000), 0);
        assert_eq!(HOST_CALLS.get(), calls, "a door with no copy");
        assign(&chains, &lanes, &csound, [&serum], true);
        assert_eq!(HOST_CALLS.get(), calls + 1, "an assignment");
        let calls = kept(&chains);
        returning(&chains, 48_000);
        assert_eq!(HOST_CALLS.get(), calls + 1, "a door with a copy");
        let calls = kept(&chains);
        assign(&chains, &lanes, &csound, [&serum], true);
        score_changed(&chains, &lanes, Some(both), true);
        forget_plan(&chains);
        assert_eq!(HOST_CALLS.get(), calls, "no change");
        keep_now(&mut locked(&chains), vec![new_chain()]);
        assert_eq!(HOST_CALLS.get(), calls + 1, "a chain that ended");
        let text = format!("{both}\npad: note(\"e\").vsti(\"tone\")");
        assert_eq!(lanes.read(&text), [0, 1, 2]);
        let pad = note("#2", 2, "tone");
        let calls = kept(&chains);
        plan(&chains, &lanes, &csound, &text, &[hap(&pad)], &[]);
        assert_eq!(HOST_CALLS.get(), calls + 1, "a plan");
        let calls = kept(&chains);
        forget_plan(&chains);
        assert_eq!(HOST_CALLS.get(), calls + 1, "a plan that goes");
        let calls = kept(&chains);
        wants(
            &chains,
            &lanes,
            &csound,
            &[hap(&note("#1", 1, "zyklop"))],
            &[],
        );
        assert_eq!(HOST_CALLS.get(), calls + 1, "a look");
        // The score in play makes a chain for the owner of the new lane of
        // the plan: at the commit, the chain the plan made ends.
        let needed = plan(&chains, &lanes, &csound, &text, &[hap(&pad)], &[]);
        let made = needed[0].slot.chain;
        let pad = owner("#2", &[], Some((2, "tone")));
        chain_of(&mut locked(&chains).playing, pad, start);
        let calls = kept(&chains);
        lanes.install(&text);
        let ended = locked(&chains).score_changed(Some(text.as_str()), lanes.playing(), start);
        assert_eq!(ended, [made]);
        keep_now(&mut locked(&chains), ended);
        assert_eq!(HOST_CALLS.get(), calls + 1, "a commit");
        // The lanes leave the text. Their chains keep their copies for their
        // time, and then end.
        play(&mut lanes, kick);
        let calls = kept(&chains);
        score_changed(&chains, &lanes, Some(kick), true);
        assert_eq!(HOST_CALLS.get(), calls, "a lane that left");
        locked(&chains).clock += CHAIN_GRACE;
        assert!(
            named_chains(&chains, InsertTurn::default())
                .named
                .is_empty()
        );
        assert_eq!(HOST_CALLS.get(), calls + 1, "a chain that ends");
        for any in [true, false] {
            let chains = Mutex::new(Chains::new());
            if any {
                assign(&chains, &lanes, &csound, [&serum], false);
            }
            let calls = kept(&chains);
            drop(chains);
            assert_eq!(HOST_CALLS.get(), calls + usize::from(any), "an end");
        }

        // The score in play makes the chains of pad and keys after the plan
        // of a text with no pad, and with a new instrument on keys. The
        // commit of that text marks both chains: they end after their time. A
        // chain of a lane the planned text drops ends CHAIN_GRACE after the
        // commit, also when the commit comes long after the plan. Lead leaves
        // the score in play 1 s before the commit: its chain ends CHAIN_GRACE
        // after it left, not after the commit. After the plan, a note in play
        // uses the chain of a lane with no statement: that chain ends
        // CHAIN_UNSEEN_LIMIT after the note, not after the plan.
        let mut lanes = LaneTable::default();
        let score = concat!(
            "kick: s(\"bd\")\n",
            "pad: note(\"c\").vsti(\"tone\")\n",
            "keys: note(\"e\").vsti(\"tone\")\n",
            "bass: note(\"c1\").vsti(\"zyklop\")\n",
            "lead: note(\"g\").vsti(\"serum\")",
        );
        assert_eq!(play(&mut lanes, score), [0, 1, 2, 3, 4]);
        let mut chains = Chains::new();
        chains.playing.score_changed(lanes.playing(), start);
        let bass = chain_of(
            &mut chains.playing,
            owner("#3", &[], Some((2, "zyklop"))),
            start,
        );
        let lead = chain_of(
            &mut chains.playing,
            owner("#4", &[], Some((3, "serum"))),
            start,
        );
        let loose = owner("$0", &[(4, "ott")], None);
        let unnamed = chain_of(&mut chains.playing, loose.clone(), start);
        let text = "kick: s(\"bd\")\nkeys: note(\"e\").vsti(\"vital\")";
        assert_eq!(lanes.read(text), [0, 2]);
        chains.plan(text, lanes.candidate(text).expect("the read"), start);
        let owners = [
            owner("#1", &[], Some((0, "tone"))),
            owner("#2", &[], Some((1, "tone"))),
        ];
        let mut made = chains_of(&mut chains.playing, &owners, start);
        made.push(bass);
        made.sort_unstable();
        let commit = start + 2 * CHAIN_GRACE;
        let left = commit - Duration::from_secs(1);
        let without = score.replace("\nlead: note(\"g\").vsti(\"serum\")", "");
        assert_eq!(play(&mut lanes, &without), [0, 1, 2, 3]);
        chains.playing.score_changed(lanes.playing(), left);
        assert_eq!(chain_of(&mut chains.playing, loose, left), unnamed);
        assert_eq!(lanes.read(text), [0, 2]);
        lanes.install(text);
        let ended = chains.score_changed(Some(text), lanes.playing(), commit);
        assert!(ended.is_empty());
        assert!(chains.settle(commit).is_empty());
        assert_eq!(chains.settle(left + CHAIN_GRACE), [lead]);
        let mut gone = chains.settle(commit + CHAIN_GRACE);
        gone.sort_unstable();
        assert_eq!(gone, made);
        assert!(chains.settle(start + CHAIN_UNSEEN_LIMIT).is_empty());
        assert_eq!(chains.settle(left + CHAIN_UNSEEN_LIMIT), [unnamed]);
    }

    /// A lane keeps its number through an edit. A muted lane plays no note.
    /// A new muted lane changes no chain of a lane that plays, also when a
    /// note of the muted lane asks first. A note of a lane that does not
    /// play takes only a chain of its own lane. A lane that left keeps its
    /// number while a chain of the session has it.
    #[test]
    fn a_muted_lane_takes_no_chain_from_a_lane_that_plays() {
        let mut lanes = LaneTable::default();
        let mut registry = ChainRegistry::new();
        let start = Duration::ZERO;
        let lane = |lane: u64, call: u32| owner(&format!("#{lane}"), &[], Some((call, "serum")));
        let silent = |registry: &mut ChainRegistry, owner: ChainOwner| {
            let room = rustel_audio::MAX_INSERT_BUSES;
            let assignment =
                registry.assign(&[(owner, false)], room, start, ChainRegistry::in_grace);
            assignment.seats[0].expect("a chain").chain
        };
        let lead = r#"$: note("c2").vsti("serum")"#;
        assert_eq!(change(&mut lanes, &mut registry, lead, start), [0]);
        let chain = chain_of(&mut registry, lane(0, 0), start);

        // A new muted lane above with the same plugin: the lane that plays
        // keeps its number and its chain. Only its call number moves. A note
        // of the muted lane asks first, while the lane that plays rests: it
        // takes no chain of that lane.
        let muted = format!("_$: note(\"e2\").vsti(\"serum\")\n{lead}");
        assert_eq!(change(&mut lanes, &mut registry, &muted, start), [1, 0]);
        let hidden = silent(&mut registry, lane(1, 0));
        assert_ne!(hidden, chain);
        assert_eq!(chain_of(&mut registry, lane(0, 1), start), chain);
        assert!(registry.settle(start + CHAIN_GRACE).is_empty());

        // The end of the mute: a mute mark changes no lane number. The lane
        // keeps the chain of its mute.
        let open = muted.replacen("_$", "$", 1);
        assert_eq!(change(&mut lanes, &mut registry, &open, start), [1, 0]);
        assert_eq!(chain_of(&mut registry, lane(1, 0), start), hidden);
        assert_eq!(chain_of(&mut registry, lane(0, 1), start), chain);

        // The first lane has its mute again, and one more lane with the
        // plugin comes: the new lane takes no chain of the muted lane.
        let more = format!("{muted}\n$: note(\"g2\").vsti(\"serum\")");
        assert_eq!(change(&mut lanes, &mut registry, &more, start), [1, 0, 2]);
        assert_eq!(chain_of(&mut registry, lane(0, 1), start), chain);
        let third = chain_of(&mut registry, lane(2, 2), start);
        assert!(third != chain && third != hidden);
        assert!(registry.settle(start + CHAIN_GRACE).is_empty());

        // A run-time lane that does not play takes no chain of another
        // run-time lane. A lane that plays takes it.
        let mut names = ChainRegistry::new();
        let none = ScoreLanes::default();
        names.score_changed(&none, start);
        let named = chain_of(&mut names, owner("$0", &[], Some((0, "serum"))), start);
        names.score_changed(&none, start);
        assert_ne!(
            silent(&mut names, owner("$1", &[], Some((0, "serum")))),
            named
        );
        assert_eq!(
            chain_of(&mut names, owner("$2", &[], Some((0, "serum"))), start),
            named
        );

        // 3 lanes leave the text. The commit keeps the lanes 1 and 2 away
        // for their chains in play, and the lane 3 for its chain in the
        // plan. With no plan, the lane 3 has no chain and goes.
        let four = format!(
            "{}\n$: note(\"a2\").vsti(\"serum\")",
            more.replacen("_$", "$", 1)
        );
        assert_eq!(play(&mut lanes, &four), [1, 0, 2, 3]);
        let mut planned = registry.clone();
        chain_of(&mut planned, lane(3, 3), start);
        let mut chains = Chains::new();
        (chains.playing, chains.plan) = (registry, Some((four.clone(), planned)));
        let chains = Mutex::new(chains);
        for in_plan in [true, false] {
            assert_eq!(play(&mut lanes, lead), [0]);
            if !in_plan {
                locked(&chains).forget();
            }
            forget_lanes_away(&chains, &mut lanes);
            let back = if in_plan { 3 } else { 4 };
            assert_eq!(play(&mut lanes, &four), [1, 0, 2, back]);
        }

        // 2 lanes with equal text keep their order below a new lane, and an
        // edit of the first one leaves the number of the second one.
        let mut lanes = LaneTable::default();
        let (hat, twin) = (r#"$: s("hh")"#, r#"$: s("bd").vst("ott")"#);
        assert_eq!(play(&mut lanes, &format!("{twin}\n{twin}")), [0, 1]);
        assert_eq!(
            play(&mut lanes, &format!("{hat}\n{twin}\n{twin}")),
            [2, 0, 1]
        );
        let score = format!("{hat}\n$: s(\"bd*2\").vst(\"ott\")\n{twin}");
        assert_eq!(play(&mut lanes, &score), [2, 0, 1]);
        // An edit of a lane and a new lane with its plugin above, in one
        // evaluation: the edited lane keeps its number.
        let score = format!(
            "$: s(\"cp\").vst(\"ott\")\n{}",
            score.replace("bd*2", "bd*4")
        );
        assert_eq!(play(&mut lanes, &score), [3, 2, 0, 1]);
        // A new label, then a mute mark on it.
        let score = score.replace(r#"$: s("bd*4")"#, r#"kick: s("bd*4")"#);
        assert_eq!(play(&mut lanes, &score), [3, 2, 0, 1]);
        assert_eq!(
            play(&mut lanes, &score.replace("kick:", "_kick:")),
            [3, 2, 0, 1]
        );
        // A score with no label is one lane, and keeps it with a label.
        let mut lanes = LaneTable::default();
        let bare = r#"note("c2").vsti("serum")"#;
        assert!(play(&mut lanes, bare).is_empty());
        assert_eq!(play(&mut lanes, &format!("score: {bare}")), [0]);
    }

    /// A chain whose effect changes keeps its number: the new effect takes
    /// a stage that no effect of the chain holds, and the host keeps the
    /// copy of the old one for a time. A chain of a run-time lane ends after
    /// a long time with no note.
    #[test]
    fn a_chain_the_text_does_not_show_goes_with_its_call() {
        let mut registry = ChainRegistry::new();
        let start = Duration::ZERO;
        let none = ScoreLanes::default();
        registry.score_changed(&none, start);
        let before = chain_of(
            &mut registry,
            owner("$0", &[(1, "ott")], Some((0, "serum"))),
            start,
        );
        registry.score_changed(&none, start);
        let after = chain_of(
            &mut registry,
            owner("$0", &[(1, "verb")], Some((0, "serum"))),
            start,
        );
        assert_eq!(after, before);
        // The effect copies of the chain, each with its stage.
        let effects = |now| {
            let copies = wanted(&registry, now).into_iter();
            let copies = copies.filter(|copy| copy.slot != rustel_audio::instrument_slot(after));
            let copies = copies.map(|copy| (copy.slot.stage, copy.name));
            copies.collect::<Vec<_>>()
        };
        let (ott, verb) = ((0, "ott".to_owned()), (1, "verb".to_owned()));
        assert_eq!(effects(start), [ott, verb.clone()]);
        assert_eq!(effects(start + CHAIN_GRACE), [verb]);
        let chain = registry.owners.values().next().expect("the chain");
        assert_eq!(chain.dropped.len(), 1, "the effect before");
        assert!(registry.settle(start + CHAIN_GRACE).is_empty());
        assert_eq!(registry.named(), [after]);
        registry.score_changed(&none, start);
        assert_eq!(registry.settle(start + CHAIN_UNSEEN_LIMIT), [after]);
    }

    /// The score of a session with no live output is not the score that
    /// plays: an export in Studio changes no chain of the live output, and
    /// only its render calls the plugin host. Each session has chains of its
    /// own. A slider of the export moves no parameter value.
    #[test]
    fn a_session_with_no_live_output_leaves_the_live_chains() {
        let calls = HOST_CALLS.get();
        let score =
            r#"$: s("bd").vst("a plugin of the export", { mix: sliderWithID('mix', 0.5) })"#;
        // The chain of the first note, as the conversion of a live note
        // gives the number.
        let chain = |session: &mut crate::Session| {
            let events = session.schedule_audio_at(0.0, 48_000).expect("events");
            events[0].controls.insert_chain.expect("a chain")
        };
        let mut playing = crate::Session::new().expect("session");
        playing.evaluate(score).expect("score");
        let mut export = crate::Session::new().expect("session");
        export.evaluate(score).expect("score");
        assert_eq!(export.active_source(), Some(score));
        // The 2 sessions share no registry: the same note has 2 chains.
        assert_ne!(chain(&mut export), chain(&mut playing));
        // A count of the plugins of the export calls no host.
        let ceiling = Duration::from_millis(50);
        assert_eq!(export.plugins_pending(0.0, 2.0, ceiling), 0);
        // A slider on a plugin key of the export moves with no host call and
        // no waiting value: its notes link no slider, and a move records
        // nothing, also for a link.
        let note = serde_json::json!({
            "s": "bd",
            "vst": [{"name": "a plugin of the export", "__rustel_sliders": {"mix": "mix"}}],
        });
        let event =
            crate::render::live_audio_event(&onset(&note, 1), 48_000, 0.5, &Plugins, |_| None);
        let mut events = [event.expect("the note")];
        export.chain_live_events(&mut events, &[&crate::ValueJson::Raw(note.clone())]);
        let mut controls = events[0].controls.clone();
        let effect = controls.effects[0].as_mut().expect("the effect");
        assert!(effect.push(InsertParam { id: 3, value: 0.5 }));
        link_sliders(export.chains_for_test(), &note, &controls, |_, _| Some(3));
        assert!(export.set_slider_value("mix", 0.25).expect("a move"));
        assert!(locked(export.chains_for_test()).waiting.is_empty());
        // An edit of the export waits for no plugin, and a launch prepares
        // none.
        #[cfg(feature = "device-audio")]
        {
            let edit = format!("{score}\n$: s(\"hh\").vst(\"a plugin of the edit\")");
            let stopped = std::sync::atomic::AtomicBool::new(false);
            let reload = export.reload_live(&edit, false, &stopped, || 0.5);
            assert!(matches!(reload, Ok(crate::LiveReload::Installed(_))));
            let prepared = export.prepare_reload(score, false, 0.0, &stopped, || 0.6);
            assert!(!prepared.expect("prepare"));
        }
        assert_eq!(HOST_CALLS.get(), calls, "the export called the host");
        // The chains of each session have a number of their own for the
        // host, so a second session unnames no chain of the first.
        assert_ne!(Chains::new().caller, Chains::new().caller);
    }

    /// A render has chains of its own. Its notes of one generation take
    /// their chains in one assignment, and an owner change at a new
    /// generation keeps the chain and the stage of each effect. The event
    /// of a note has its chain, and each effect its stage. With no room, a
    /// render ends the chain with the oldest use that the generation does
    /// not use. Each Csound orbit of a generation takes a place of the
    /// room. The plan of a text has chains of its own too. The plan gives
    /// the plugin copies its notes need, each on the stage of its call.
    #[test]
    fn a_render_has_chains_of_its_own() {
        let value = serde_json::json!({
            "__rustel_lane": "#0",
            "vst": [{"name": "Render Only", "__rustel_binding": "p0"}]
        });
        let mut moved = value.clone();
        moved["vst"][0]["__rustel_binding"] = "p1".into();
        let notes = [
            onset(&value, 1),
            onset(&value, 1),
            onset(&serde_json::Value::Null, 1),
            onset(&moved, 2),
        ];
        let chains = render_chains(&notes.iter().collect::<Vec<_>>());
        let render = numbers(&chains)[0];
        let seat = NoteChain::Seat(Seat {
            chain: render,
            stages: stages(&[0]),
        });
        assert_eq!(chains, [seat, seat, NoteChain::None, seat]);
        // The effects of the event of a render sit on their stages: the
        // effects in a new order keep their stages at a new generation.
        let effects = |calls: [&str; 2], generation: u64| {
            let call = |at: usize| {
                let binding = format!("p{generation}{at}");
                serde_json::json!({"name": calls[at], "__rustel_binding": binding})
            };
            let value = serde_json::json!({
                "s": "sine",
                "__rustel_lane": "#0",
                "vst": [call(0), call(1)],
            });
            onset(&value, generation)
        };
        let notes = [
            effects(["Render Only", "Delay"], 1),
            effects(["Delay", "Render Only"], 2),
        ];
        let events = crate::render::scalar_events(&notes, 48_000, 0.5, &Plugins).expect("events");
        // `set_stage` leaves an effect on `stage` as it was.
        let on = |effect: Option<InsertControls>, stage: u8| {
            let mut seated = effect.expect("an effect");
            seated.set_stage(stage);
            effect == Some(seated)
        };
        let (first, second) = (&events[0].controls, &events[1].controls);
        assert!(first.insert_chain.is_some() && second.insert_chain == first.insert_chain);
        assert!(on(first.effects[0], 0) && on(first.effects[1], 1));
        assert!(on(second.effects[0], 1) && on(second.effects[1], 0));
        // A session has a different number for the same note.
        let live = Mutex::new(Chains::new());
        let (lanes, csound) = (LaneTable::default(), crate::csound::State::new(false));
        let raw = crate::ValueJson::Raw(value.clone());
        let playing = numbers(&assign(&live, &lanes, &csound, [&raw], false))[0];
        assert_ne!(playing, render);

        // A render with 64 chains ends the chain with the oldest use that a
        // new generation does not use. A generation that uses each chain
        // leaves no room for one more.
        let lane = |lane: usize, generation: u64| {
            let value = serde_json::json!({
                "__rustel_lane": format!("#{lane}"),
                "vst": [{"name": "Render Only", "__rustel_binding": "p0"}]
            });
            onset(&value, generation)
        };
        let mut notes: Vec<crate::OnsetEventJson> = (0..64).map(|at| lane(at, 1)).collect();
        notes.extend([lane(64, 2), lane(0, 3)]);
        notes.extend((0..65).map(|at| lane(at + 100, 4)));
        let chains = render_chains(&notes.iter().collect::<Vec<_>>());
        let first = numbers(&chains[..64]);
        let (new, again) = (numbers(&chains[64..65]), numbers(&chains[65..66]));
        assert!(!first.contains(&new[0]) && !first.contains(&again[0]));
        assert_ne!(again[0], first[0], "the chain of the lane 0 ended");
        assert_eq!(chains[66 + 64], NoteChain::Refused);
        // Each Csound orbit of a generation takes a place of the room.
        let csound_note = serde_json::json!({"s": "sine", "csound": "probe", "orbit": 2});
        let mut notes = vec![onset(&csound_note, 1)];
        notes.extend((0..64).map(|at| lane(at, 1)));
        let chains = render_chains(&notes.iter().collect::<Vec<_>>());
        assert_eq!(chains[0], NoteChain::None);
        assert_eq!(numbers(&chains).len(), 63);
        assert_eq!(chains[64], NoteChain::Refused);

        // The plan of a text gives each copy its notes need, one time. A
        // note of the score in play keeps its chain. A new note takes a
        // number of the plan, and makes no chain in the score in play. The
        // copies of a lane that does not play come last, and do not sound.
        let mut new = value.clone();
        new["__rustel_lane"] = "plan".into();
        new["vsti"] = serde_json::json!({"name": "Tone", "__rustel_binding": "p1", "preset": 2});
        let mut muted = value.clone();
        muted["__rustel_lane"] = "muted".into();
        let haps = [hap(&value), hap(&new), hap(&new)];
        let needed = plan(
            &live,
            &lanes,
            &csound,
            "a text",
            &haps,
            &[hap(&muted), hap(&value)],
        );
        let slots: Vec<InsertSlot> = needed.iter().map(|want| want.slot).collect();
        let (made, silent) = (slots[1].chain, slots[3].chain);
        let (effect, tone) = (InsertSlot::effect(made, 0), InsertSlot::instrument(made));
        let muted = InsertSlot::effect(silent, 0);
        assert_eq!(slots, [InsertSlot::effect(playing, 0), effect, tone, muted]);
        assert!(silent != made && silent != playing);
        let sounds: Vec<bool> = needed.iter().map(|want| want.sounds).collect();
        assert_eq!(sounds, [true, true, true, false]);
        let tone = &needed[2];
        assert!(tone.instrument && !needed[1].instrument);
        assert_eq!(tone.name, "Tone");
        assert_eq!(tone.preset.as_deref(), Some("2"));
        assert!(!locked(&live).playing.has(made));
        forget_plan(&live);
        assert!(locked(&live).plan.is_none());
        // Each copy of a note sits on the stage of its call. A call with no
        // name has no stage, and needs no copy.
        let calls =
            serde_json::json!({"vst": [{"name": "eq"}, {"preset": "Hard"}, {"name": "ott"}]});
        let seat = NoteChain::Seat(Seat {
            chain: made,
            stages: [1, rustel_audio::NO_STAGE, 0, rustel_audio::NO_STAGE],
        });
        let needed = wants_of(&[hap(&calls)], &[], &[seat]);
        let slots: Vec<InsertSlot> = needed.iter().map(|want| want.slot).collect();
        assert_eq!(
            slots,
            [InsertSlot::effect(made, 1), InsertSlot::effect(made, 0)]
        );
    }

    /// A slider that is the value of a plugin key links to the parameter of
    /// the copy that plays the note: an effect on its stage, and the
    /// instrument. A value the note dropped, a key the plugin does not have
    /// and a note with no chain link nothing. A link the chains have asks
    /// for no number, and a score links 1024 values at most. A move gives
    /// one value for each link, in place of the value of the move before,
    /// and the output takes the values in the order of the moves. A score
    /// change clears the links and keeps the values. A move then gives no
    /// value until a note of the new score links the slider again. The flag
    /// of the links is true while the chains have a link. A session records
    /// a move while its live output plays, and a move with no link records
    /// nothing. A move with no link and a note with no slider take no lock
    /// of the chains.
    #[test]
    fn a_slider_moves_the_values_its_notes_link() {
        let chain = 100;
        // `level` on the depth of ott and on the gain, the mix and the shape
        // of eq. `tone` on the cutoff of the instrument. The note dropped the
        // mix of eq, and eq has no shape.
        let note = serde_json::json!({
            "vst": [
                {"name": "ott", "__rustel_sliders": {"depth": "level"}},
                {"name": "eq", "__rustel_sliders": {"gain": "level", "mix": "level", "shape": "level"}},
            ],
            "vsti": {"name": "synth", "__rustel_sliders": {"cutoff": "tone"}},
        });
        // The controls of a copy of `plugin` on `stage` with the parameters
        // `ids`.
        let copy = |plugin: u32, stage: u8, ids: &[u32]| {
            let mut controls = InsertControls::new(InsertKey { plugin, preset: 0 });
            for id in ids {
                assert!(controls.push(InsertParam {
                    id: *id,
                    value: 0.5
                }));
            }
            controls.set_stage(stage);
            controls
        };
        let mut controls = rustel_audio::OscillatorControls {
            insert_chain: Some(chain),
            instrument: Some(copy(3, rustel_audio::NO_STAGE, &[6])),
            ..Default::default()
        };
        controls.effects[0] = Some(copy(1, 1, &[3]));
        controls.effects[1] = Some(copy(2, 0, &[4]));
        let asked = std::cell::Cell::new(0);
        let param_of = |name: &str, key: &str| {
            asked.set(asked.get() + 1);
            match (name, key) {
                ("ott", "depth") => Some(3),
                ("eq", "gain") => Some(4),
                ("eq", "mix") => Some(5),
                ("synth", "cutoff") => Some(6),
                _ => None,
            }
        };
        let chains = Mutex::new(Chains::new());
        let linked = slider_links(&chains);
        assert!(!linked.load(Ordering::Relaxed));
        link_sliders(&chains, &note, &controls, param_of);
        assert!(linked.load(Ordering::Relaxed));
        assert_eq!(asked.get(), 5);
        // The next note asks only for the keys with no link, and links
        // nothing again.
        link_sliders(&chains, &note, &controls, param_of);
        assert_eq!(asked.get(), 7);
        assert_eq!(locked(&chains).links.len(), 3);
        let unchained = rustel_audio::OscillatorControls {
            insert_chain: None,
            ..controls.clone()
        };
        link_sliders(&chains, &note, &unchained, param_of);
        assert_eq!(asked.get(), 7, "a note with no chain");

        let (ott, eq) = (InsertSlot::effect(chain, 1), InsertSlot::effect(chain, 0));
        let synth = InsertSlot::instrument(chain);
        let moved = |slot, plugin, id, value, generation| InsertValue {
            slot,
            key: InsertKey { plugin, preset: 0 },
            param: InsertParam { id, value },
            generation,
        };
        slider_moved(&chains, "level", 0.25, 7);
        slider_moved(&chains, "tone", 0.75, 7);
        slider_moved(&chains, "level", 0.5, 8);
        slider_moved(&chains, "no link", 0.5, 8);
        let values = [
            moved(ott, 1, 3, 0.5, 8),
            moved(eq, 2, 4, 0.5, 8),
            moved(synth, 3, 6, 0.75, 7),
        ];
        assert_eq!(take_slider_values(&chains, 1), values[..1]);
        assert_eq!(take_slider_values(&chains, 64), values[1..]);
        assert!(take_slider_values(&chains, 64).is_empty());

        // A score change clears the links, also the commit of a plan. A
        // value that waits stays. A move gives no value until a note of the
        // new score links the slider again.
        let (lanes, csound) = (LaneTable::default(), crate::csound::State::new(false));
        slider_moved(&chains, "level", 1.0, 9);
        score_changed(&chains, &lanes, None, false);
        assert!(!linked.load(Ordering::Relaxed));
        slider_moved(&chains, "level", 0.0, 10);
        let values = [moved(ott, 1, 3, 1.0, 9), moved(eq, 2, 4, 1.0, 9)];
        assert_eq!(take_slider_values(&chains, 64), values);
        link_sliders(&chains, &note, &controls, param_of);
        slider_moved(&chains, "level", 0.5, 10);
        let values = [moved(ott, 1, 3, 0.5, 10), moved(eq, 2, 4, 0.5, 10)];
        assert_eq!(take_slider_values(&chains, 64), values);
        plan(&chains, &lanes, &csound, "a text", &[], &[]);
        score_changed(&chains, &lanes, Some("a text"), false);
        assert!(!linked.load(Ordering::Relaxed));
        slider_moved(&chains, "level", 0.0, 11);
        assert!(take_slider_values(&chains, 64).is_empty());

        // A score links 1024 values at most.
        for at in 0..=MAX_SLIDER_LINKS {
            let one = serde_json::json!({
                "vst": [{"name": "ott", "__rustel_sliders": {"depth": format!("s{at}")}}],
            });
            link_sliders(&chains, &one, &controls, param_of);
        }
        assert_eq!(locked(&chains).links.len(), MAX_SLIDER_LINKS);

        // A session records a move while its live output plays, at the
        // generation of its score, and the values source of the output takes
        // it. A move with no link and a stopped transport record nothing. A
        // move with no link and a note with no slider take no lock of the
        // chains: both return while another thread holds the lock.
        #[cfg(feature = "device-audio")]
        {
            let mut session = crate::Session::new().expect("session");
            let score = "note('c3').vst('ott', { depth: sliderWithID('level', 0.5) })";
            session.evaluate(score).expect("score");
            let generation = session.generation();
            let output = rustel_audio::device::ManualLiveOutput::new(48_000, generation);
            let output = output.expect("output");
            session.attach_plugins(output.device());
            let chains = session.chains_for_test();
            let bare = serde_json::json!({"vst": [{"name": "ott"}], "vsti": {"name": "synth"}});
            let (held, hold) = std::sync::mpsc::channel();
            let (done, end) = std::sync::mpsc::channel::<()>();
            let (known, waited) = std::thread::scope(|scope| {
                let holder = scope.spawn(move || {
                    let _lock = locked(chains);
                    held.send(()).expect("the lock");
                    end.recv_timeout(Duration::from_secs(10)).is_err()
                });
                hold.recv().expect("the lock");
                let known = session.set_slider_value("level", 0.1);
                link_sliders(chains, &bare, &controls, param_of);
                let _ = done.send(());
                (known, holder.join().expect("the holder"))
            });
            assert!(known.expect("a move"));
            assert!(!waited, "a call waited for the lock of the chains");
            assert!(locked(chains).waiting.is_empty(), "no link");
            link_sliders(chains, &note, &controls, param_of);
            assert!(session.set_slider_value("level", 0.25).expect("a move"));
            let values = [
                moved(ott, 1, 3, 0.25, generation),
                moved(eq, 2, 4, 0.25, generation),
            ];
            assert_eq!(locked(chains).waiting, values);
            rustel_audio::LiveReverbBatch::install_inserts(output.device(), &[]);
            assert!(locked(chains).waiting.is_empty(), "a turn");
            session.transport().stop();
            assert!(session.set_slider_value("level", 0.5).expect("a move"));
            assert!(locked(chains).waiting.is_empty(), "a Stop");
        }
    }
}
