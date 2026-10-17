//! English letter-to-sound rules.
//!
//! The rules read one lowercase word and produce phonemes: at each letter the
//! longest matching pattern wins, so digraphs and suffixes beat single letters
//! without hand ordering. Words English spells against its own rules come
//! from the exception list first. The result is intelligible, rule-driven
//! speech in the spirit of the original engine, not a pronouncing dictionary.

use crate::lang::{Phone, Rule, apply_rules};
use crate::phoneme::Phoneme as P;

/// Whole words the letter rules would get badly wrong, with apostrophes and
/// accents already stripped. High-frequency function words live here because
/// a mistake in them is heard in every sentence.
const EXCEPTIONS: &[(&str, &[P])] = &[
    ("the", &[P::Dh, P::Ax]),
    ("a", &[P::Ax]),
    ("i", &[P::Ay]),
    ("be", &[P::B, P::Iy]),
    ("he", &[P::Hh, P::Iy]),
    ("me", &[P::M, P::Iy]),
    ("we", &[P::W, P::Iy]),
    ("she", &[P::Sh, P::Iy]),
    ("of", &[P::Ah, P::V]),
    ("to", &[P::T, P::Uw]),
    ("do", &[P::D, P::Uw]),
    ("does", &[P::D, P::Ah, P::Z]),
    ("done", &[P::D, P::Ah, P::N]),
    ("go", &[P::G, P::Ow]),
    ("so", &[P::S, P::Ow]),
    ("no", &[P::N, P::Ow]),
    ("you", &[P::Y, P::Uw]),
    ("your", &[P::Y, P::Ao, P::R]),
    ("one", &[P::W, P::Ah, P::N]),
    ("once", &[P::W, P::Ah, P::N, P::S]),
    ("two", &[P::T, P::Uw]),
    ("who", &[P::Hh, P::Uw]),
    ("whose", &[P::Hh, P::Uw, P::Z]),
    ("what", &[P::W, P::Ah, P::T]),
    ("where", &[P::W, P::Eh, P::R]),
    ("there", &[P::Dh, P::Eh, P::R]),
    ("their", &[P::Dh, P::Eh, P::R]),
    ("they", &[P::Dh, P::Ey]),
    ("them", &[P::Dh, P::Eh, P::M]),
    ("then", &[P::Dh, P::Eh, P::N]),
    ("than", &[P::Dh, P::Ae, P::N]),
    ("this", &[P::Dh, P::Ih, P::S]),
    ("that", &[P::Dh, P::Ae, P::T]),
    ("these", &[P::Dh, P::Iy, P::Z]),
    ("those", &[P::Dh, P::Ow, P::Z]),
    ("though", &[P::Dh, P::Ow]),
    ("thus", &[P::Dh, P::Ah, P::S]),
    ("is", &[P::Ih, P::Z]),
    ("as", &[P::Ae, P::Z]),
    ("his", &[P::Hh, P::Ih, P::Z]),
    ("has", &[P::Hh, P::Ae, P::Z]),
    ("was", &[P::W, P::Ah, P::Z]),
    ("are", &[P::Aa, P::R]),
    ("said", &[P::S, P::Eh, P::D]),
    ("says", &[P::S, P::Eh, P::Z]),
    ("come", &[P::K, P::Ah, P::M]),
    ("some", &[P::S, P::Ah, P::M]),
    ("gone", &[P::G, P::Ao, P::N]),
    ("love", &[P::L, P::Ah, P::V]),
    ("above", &[P::Ax, P::B, P::Ah, P::V]),
    ("give", &[P::G, P::Ih, P::V]),
    ("live", &[P::L, P::Ih, P::V]),
    ("have", &[P::Hh, P::Ae, P::V]),
    ("from", &[P::F, P::R, P::Ah, P::M]),
    ("by", &[P::B, P::Ay]),
    ("my", &[P::M, P::Ay]),
    ("why", &[P::W, P::Ay]),
    ("any", &[P::Eh, P::N, P::Iy]),
    ("many", &[P::M, P::Eh, P::N, P::Iy]),
    ("very", &[P::V, P::Eh, P::R, P::Iy]),
    ("every", &[P::Eh, P::V, P::R, P::Iy]),
    ("been", &[P::B, P::Ih, P::N]),
    ("were", &[P::W, P::Er]),
    ("could", &[P::K, P::Uh, P::D]),
    ("would", &[P::W, P::Uh, P::D]),
    ("should", &[P::Sh, P::Uh, P::D]),
    ("through", &[P::Th, P::R, P::Uw]),
    ("thought", &[P::Th, P::Ao, P::T]),
    ("brought", &[P::B, P::R, P::Ao, P::T]),
    ("bought", &[P::B, P::Ao, P::T]),
    ("rough", &[P::R, P::Ah, P::F]),
    ("tough", &[P::T, P::Ah, P::F]),
    ("enough", &[P::Ih, P::N, P::Ah, P::F]),
    ("laugh", &[P::L, P::Ae, P::F]),
    ("heart", &[P::Hh, P::Aa, P::R, P::T]),
    ("earth", &[P::Er, P::Th]),
    ("learn", &[P::L, P::Er, P::N]),
    ("early", &[P::Er, P::L, P::Iy]),
    ("bear", &[P::B, P::Eh, P::R]),
    ("great", &[P::G, P::R, P::Ey]),
    ("break", &[P::B, P::R, P::Ey]),
    ("dead", &[P::D, P::Eh, P::D]),
    ("death", &[P::D, P::Eh, P::Th]),
    ("head", &[P::Hh, P::Eh, P::D]),
    ("ready", &[P::R, P::Eh, P::D, P::Iy]),
    ("bread", &[P::B, P::R, P::Eh, P::D]),
    ("heavy", &[P::Hh, P::Eh, P::V, P::Iy]),
    ("weather", &[P::W, P::Eh, P::Dh, P::Er]),
    ("eye", &[P::Ay]),
    ("eyes", &[P::Ay, P::Z]),
    ("friend", &[P::F, P::R, P::Eh, P::N, P::D]),
    ("build", &[P::B, P::Ih, P::L, P::D]),
    ("busy", &[P::B, P::Ih, P::Z, P::Iy]),
    ("buy", &[P::B, P::Ay]),
    ("people", &[P::P, P::Iy, P::P, P::Ax, P::L]),
    ("because", &[P::B, P::Ih, P::K, P::Ah, P::Z]),
    ("woman", &[P::W, P::Uh, P::M, P::Ax, P::N]),
    ("women", &[P::W, P::Ih, P::M, P::Ih, P::N]),
    ("answer", &[P::Ae, P::N, P::S, P::Er]),
    ("island", &[P::Ay, P::L, P::Ax, P::N, P::D]),
    ("sword", &[P::S, P::Ao, P::R, P::D]),
    ("sure", &[P::Sh, P::Uh, P::R]),
    ("measure", &[P::M, P::Ey, P::Zh, P::Er]),
    ("pleasure", &[P::P, P::L, P::Ey, P::Zh, P::Er]),
    ("treasure", &[P::T, P::R, P::Ey, P::Zh, P::Er]),
    ("move", &[P::M, P::Uw, P::V]),
    ("prove", &[P::P, P::R, P::Uw, P::V]),
    ("shoe", &[P::Sh, P::Uw]),
    ("machine", &[P::M, P::Ax, P::Sh, P::Iy, P::N]),
    ("engine", &[P::Eh, P::N, P::D, P::Zh, P::Ih, P::N]),
    ("giant", &[P::D, P::Zh, P::Ay, P::Ax, P::N, P::T]),
    ("magic", &[P::M, P::Ae, P::D, P::Zh, P::Ih, P::K]),
    ("imagine", &[P::Ih, P::M, P::Ae, P::D, P::Zh, P::Ih, P::N]),
    ("gentle", &[P::D, P::Zh, P::Eh, P::N, P::T, P::Ax, P::L]),
    ("ginger", &[P::D, P::Zh, P::Ih, P::N, P::D, P::Zh, P::Er]),
    ("gym", &[P::D, P::Zh, P::Ih, P::M]),
    (
        "digital",
        &[P::D, P::Ih, P::D, P::Zh, P::Ih, P::T, P::Ax, P::L],
    ),
    ("hour", &[P::Aw, P::Er]),
    ("our", &[P::Aw, P::R]),
    ("village", &[P::V, P::Ih, P::L, P::Ih, P::D, P::Zh]),
    ("message", &[P::M, P::Eh, P::S, P::Ih, P::D, P::Zh]),
    ("damage", &[P::D, P::Ae, P::M, P::Ih, P::D, P::Zh]),
    ("image", &[P::Ih, P::M, P::Ih, P::D, P::Zh]),
    ("manage", &[P::M, P::Ae, P::N, P::Ih, P::D, P::Zh]),
    ("only", &[P::Ow, P::N, P::L, P::Iy]),
    ("other", &[P::Ah, P::Dh, P::Er]),
    ("another", &[P::Ax, P::N, P::Ah, P::Dh, P::Er]),
    ("mother", &[P::M, P::Ah, P::Dh, P::Er]),
    ("father", &[P::F, P::Aa, P::Dh, P::Er]),
    ("brother", &[P::B, P::R, P::Ah, P::Dh, P::Er]),
    ("together", &[P::T, P::Ax, P::G, P::Eh, P::Dh, P::Er]),
    ("whether", &[P::W, P::Eh, P::Dh, P::Er]),
    ("either", &[P::Iy, P::Dh, P::Er]),
    ("neither", &[P::N, P::Iy, P::Dh, P::Er]),
    ("height", &[P::Hh, P::Ay, P::T]),
    ("eight", &[P::Ey, P::T]),
    ("weigh", &[P::W, P::Ey]),
    ("weight", &[P::W, P::Ey, P::T]),
    ("water", &[P::W, P::Ao, P::T, P::Er]),
    ("minute", &[P::M, P::Ih, P::N, P::Ih, P::T]),
    ("young", &[P::Y, P::Ah, P::Ng]),
    ("july", &[P::D, P::Zh, P::Uw, P::L, P::Ay]),
    // Contractions, with the apostrophe already removed by normalization.
    ("dont", &[P::D, P::Ow, P::N, P::T]),
    ("doesnt", &[P::D, P::Ah, P::Z, P::Ax, P::N, P::T]),
    ("wont", &[P::W, P::Ow, P::N, P::T]),
    ("cant", &[P::K, P::Ae, P::N, P::T]),
    ("isnt", &[P::Ih, P::Z, P::Ax, P::N, P::T]),
    ("wasnt", &[P::W, P::Ah, P::Z, P::Ax, P::N, P::T]),
    ("werent", &[P::W, P::Er, P::Ax, P::N, P::T]),
    ("wouldnt", &[P::W, P::Uh, P::D, P::Ax, P::N, P::T]),
    ("couldnt", &[P::K, P::Uh, P::D, P::Ax, P::N, P::T]),
    ("shouldnt", &[P::Sh, P::Uh, P::D, P::Ax, P::N, P::T]),
    ("its", &[P::Ih, P::T, P::S]),
    ("im", &[P::Ay, P::M]),
    ("ive", &[P::Ay, P::V]),
    ("ill", &[P::Ay, P::L]),
    ("youre", &[P::Y, P::Ao, P::R]),
    ("theyre", &[P::Dh, P::Eh, P::R]),
    ("youll", &[P::Y, P::Uw, P::L]),
    ("theyll", &[P::Dh, P::Ey, P::L]),
];

/// Letter patterns. `#` matches a word edge. Order within the table does not
/// matter: the engine takes the longest pattern that matches.
const RULES: &[Rule] = &[
    // Suffix families the single letters cannot express.
    (Rule {
        pattern: "ation",
        phones: &[P::Ey, P::Sh, P::Ax, P::N],
    }),
    (Rule {
        pattern: "tion",
        phones: &[P::Sh, P::Ax, P::N],
    }),
    (Rule {
        pattern: "sion",
        phones: &[P::Zh, P::Ax, P::N],
    }),
    (Rule {
        pattern: "cious",
        phones: &[P::Sh, P::Ax, P::S],
    }),
    (Rule {
        pattern: "tious",
        phones: &[P::Sh, P::Ax, P::S],
    }),
    (Rule {
        pattern: "cial",
        phones: &[P::Sh, P::Ax, P::L],
    }),
    (Rule {
        pattern: "tial",
        phones: &[P::Sh, P::Ax, P::L],
    }),
    (Rule {
        pattern: "ture",
        phones: &[P::T, P::Sh, P::Er],
    }),
    (Rule {
        pattern: "asure",
        phones: &[P::Ey, P::Zh, P::Er],
    }),
    (Rule {
        pattern: "isure",
        phones: &[P::Ih, P::Zh, P::Er],
    }),
    (Rule {
        pattern: "sure",
        phones: &[P::Zh, P::Er],
    }),
    (Rule {
        pattern: "ough",
        phones: &[P::Aw],
    }),
    (Rule {
        pattern: "augh",
        phones: &[P::Ao],
    }),
    (Rule {
        pattern: "eigh",
        phones: &[P::Ey],
    }),
    (Rule {
        pattern: "ight",
        phones: &[P::Ay, P::T],
    }),
    (Rule {
        pattern: "ign#",
        phones: &[P::Ay, P::N],
    }),
    (Rule {
        pattern: "stle",
        phones: &[P::S, P::Ax, P::L],
    }),
    (Rule {
        pattern: "ften",
        phones: &[P::F, P::T, P::Ax, P::N],
    }),
    (Rule {
        pattern: "sten",
        phones: &[P::S, P::Ax, P::N],
    }),
    (Rule {
        pattern: "dge",
        phones: &[P::D, P::Zh],
    }),
    // Past tense and plural endings.
    (Rule {
        pattern: "ted#",
        phones: &[P::T, P::Ih, P::D],
    }),
    (Rule {
        pattern: "ded#",
        phones: &[P::D, P::Ih, P::D],
    }),
    (Rule {
        pattern: "ked#",
        phones: &[P::K, P::T],
    }),
    (Rule {
        pattern: "ped#",
        phones: &[P::P, P::T],
    }),
    (Rule {
        pattern: "fed#",
        phones: &[P::F, P::T],
    }),
    (Rule {
        pattern: "ced#",
        phones: &[P::S, P::T],
    }),
    (Rule {
        pattern: "ged#",
        phones: &[P::D, P::Zh, P::D],
    }),
    (Rule {
        pattern: "sed#",
        phones: &[P::S, P::T],
    }),
    (Rule {
        pattern: "xed#",
        phones: &[P::K, P::S, P::T],
    }),
    (Rule {
        pattern: "shed#",
        phones: &[P::Sh, P::T],
    }),
    (Rule {
        pattern: "ssed#",
        phones: &[P::S, P::T],
    }),
    (Rule {
        pattern: "sses#",
        phones: &[P::S, P::Ih, P::Z],
    }),
    (Rule {
        pattern: "ched#",
        phones: &[P::T, P::Sh, P::T],
    }),
    (Rule {
        pattern: "ed#",
        phones: &[P::D],
    }),
    (Rule {
        pattern: "ges#",
        phones: &[P::D, P::Zh, P::Ih, P::Z],
    }),
    (Rule {
        pattern: "ces#",
        phones: &[P::S, P::Ih, P::Z],
    }),
    (Rule {
        pattern: "ses#",
        phones: &[P::S, P::Ih, P::Z],
    }),
    (Rule {
        pattern: "zes#",
        phones: &[P::Z, P::Ih, P::Z],
    }),
    (Rule {
        pattern: "xes#",
        phones: &[P::K, P::S, P::Ih, P::Z],
    }),
    (Rule {
        pattern: "ches#",
        phones: &[P::T, P::Sh, P::Ih, P::Z],
    }),
    (Rule {
        pattern: "shes#",
        phones: &[P::Sh, P::Ih, P::Z],
    }),
    (Rule {
        pattern: "ies#",
        phones: &[P::Iy, P::Z],
    }),
    // Magic e: vowel, consonant, silent final e.
    (Rule {
        pattern: "ake",
        phones: &[P::Ey, P::K],
    }),
    (Rule {
        pattern: "ame",
        phones: &[P::Ey, P::M],
    }),
    (Rule {
        pattern: "ane",
        phones: &[P::Ey, P::N],
    }),
    (Rule {
        pattern: "ape",
        phones: &[P::Ey, P::P],
    }),
    (Rule {
        pattern: "ate",
        phones: &[P::Ey, P::T],
    }),
    (Rule {
        pattern: "ave",
        phones: &[P::Ey, P::V],
    }),
    (Rule {
        pattern: "ase",
        phones: &[P::Ey, P::S],
    }),
    (Rule {
        pattern: "aste",
        phones: &[P::Ey, P::S, P::T],
    }),
    (Rule {
        pattern: "eme",
        phones: &[P::Iy, P::M],
    }),
    (Rule {
        pattern: "ene",
        phones: &[P::Iy, P::N],
    }),
    (Rule {
        pattern: "ete",
        phones: &[P::Iy, P::T],
    }),
    (Rule {
        pattern: "eve",
        phones: &[P::Iy, P::V],
    }),
    (Rule {
        pattern: "eze",
        phones: &[P::Iy, P::Z],
    }),
    (Rule {
        pattern: "ife",
        phones: &[P::Ay, P::F],
    }),
    (Rule {
        pattern: "ike",
        phones: &[P::Ay, P::K],
    }),
    (Rule {
        pattern: "ile",
        phones: &[P::Ay, P::L],
    }),
    (Rule {
        pattern: "ime",
        phones: &[P::Ay, P::M],
    }),
    (Rule {
        pattern: "ine",
        phones: &[P::Ay, P::N],
    }),
    (Rule {
        pattern: "ite",
        phones: &[P::Ay, P::T],
    }),
    (Rule {
        pattern: "ire",
        phones: &[P::Ay, P::Er],
    }),
    (Rule {
        pattern: "ise",
        phones: &[P::Ay, P::Z],
    }),
    (Rule {
        pattern: "ive",
        phones: &[P::Ay, P::V],
    }),
    (Rule {
        pattern: "ice",
        phones: &[P::Ay, P::S],
    }),
    (Rule {
        pattern: "ype",
        phones: &[P::Ay, P::P],
    }),
    (Rule {
        pattern: "ole",
        phones: &[P::Ow, P::L],
    }),
    (Rule {
        pattern: "one",
        phones: &[P::Ow, P::N],
    }),
    (Rule {
        pattern: "ope",
        phones: &[P::Ow, P::P],
    }),
    (Rule {
        pattern: "ore",
        phones: &[P::Ao, P::R],
    }),
    (Rule {
        pattern: "ose",
        phones: &[P::Ow, P::Z],
    }),
    (Rule {
        pattern: "ote",
        phones: &[P::Ow, P::T],
    }),
    (Rule {
        pattern: "ome",
        phones: &[P::Ow, P::M],
    }),
    (Rule {
        pattern: "oze",
        phones: &[P::Ow, P::Z],
    }),
    (Rule {
        pattern: "ube",
        phones: &[P::Uw, P::B],
    }),
    (Rule {
        pattern: "ude",
        phones: &[P::Uw, P::D],
    }),
    (Rule {
        pattern: "uge",
        phones: &[P::Uw, P::D, P::Zh],
    }),
    (Rule {
        pattern: "ule",
        phones: &[P::Uw, P::L],
    }),
    (Rule {
        pattern: "ume",
        phones: &[P::Uw, P::M],
    }),
    (Rule {
        pattern: "une",
        phones: &[P::Uw, P::N],
    }),
    (Rule {
        pattern: "ure",
        phones: &[P::Y, P::Uh, P::R],
    }),
    (Rule {
        pattern: "use",
        phones: &[P::Uw, P::Z],
    }),
    (Rule {
        pattern: "ute",
        phones: &[P::Uw, P::T],
    }),
    (Rule {
        pattern: "are",
        phones: &[P::Eh, P::R],
    }),
    (Rule {
        pattern: "ere",
        phones: &[P::Iy, P::R],
    }),
    // Word-final letter behaviors.
    (Rule {
        pattern: "cle",
        phones: &[P::K, P::Ax, P::L],
    }),
    (Rule {
        pattern: "le#",
        phones: &[P::Ax, P::L],
    }),
    (Rule {
        pattern: "ge#",
        phones: &[P::D, P::Zh],
    }),
    (Rule {
        pattern: "ce#",
        phones: &[P::S],
    }),
    (Rule {
        pattern: "se#",
        phones: &[P::Z],
    }),
    (Rule {
        pattern: "re#",
        phones: &[P::Er],
    }),
    (Rule {
        pattern: "nce",
        phones: &[P::N, P::S],
    }),
    (Rule {
        pattern: "mb#",
        phones: &[P::M],
    }),
    (Rule {
        pattern: "mn#",
        phones: &[P::M],
    }),
    (Rule {
        pattern: "gn#",
        phones: &[P::N],
    }),
    (Rule {
        pattern: "y#",
        phones: &[P::Ay],
    }),
    (Rule {
        pattern: "e#",
        phones: &[],
    }),
    // r-colored and long vowel pairs.
    (Rule {
        pattern: "air",
        phones: &[P::Eh, P::R],
    }),
    (Rule {
        pattern: "are",
        phones: &[P::Eh, P::R],
    }),
    (Rule {
        pattern: "ear",
        phones: &[P::Iy, P::R],
    }),
    (Rule {
        pattern: "eer",
        phones: &[P::Iy, P::R],
    }),
    (Rule {
        pattern: "ere",
        phones: &[P::Iy, P::R],
    }),
    (Rule {
        pattern: "oor",
        phones: &[P::Ao, P::R],
    }),
    (Rule {
        pattern: "our",
        phones: &[P::Aw, P::R],
    }),
    (Rule {
        pattern: "ther",
        phones: &[P::Dh, P::Er],
    }),
    (Rule {
        pattern: "ar",
        phones: &[P::Aa, P::R],
    }),
    (Rule {
        pattern: "or",
        phones: &[P::Ao, P::R],
    }),
    (Rule {
        pattern: "er",
        phones: &[P::Er],
    }),
    (Rule {
        pattern: "ir",
        phones: &[P::Er],
    }),
    (Rule {
        pattern: "ur",
        phones: &[P::Er],
    }),
    (Rule {
        pattern: "yr",
        phones: &[P::Er],
    }),
    (Rule {
        pattern: "ai",
        phones: &[P::Ey],
    }),
    (Rule {
        pattern: "ay",
        phones: &[P::Ey],
    }),
    (Rule {
        pattern: "au",
        phones: &[P::Ao],
    }),
    (Rule {
        pattern: "aw",
        phones: &[P::Ao],
    }),
    (Rule {
        pattern: "ea",
        phones: &[P::Iy],
    }),
    (Rule {
        pattern: "ee",
        phones: &[P::Iy],
    }),
    (Rule {
        pattern: "ei",
        phones: &[P::Iy],
    }),
    (Rule {
        pattern: "eu",
        phones: &[P::Uw],
    }),
    (Rule {
        pattern: "ew",
        phones: &[P::Uw],
    }),
    (Rule {
        pattern: "ey",
        phones: &[P::Iy],
    }),
    (Rule {
        pattern: "ie",
        phones: &[P::Iy],
    }),
    (Rule {
        pattern: "oa",
        phones: &[P::Ow],
    }),
    (Rule {
        pattern: "oe",
        phones: &[P::Ow],
    }),
    (Rule {
        pattern: "oi",
        phones: &[P::Oy],
    }),
    (Rule {
        pattern: "oo",
        phones: &[P::Uw],
    }),
    (Rule {
        pattern: "ou",
        phones: &[P::Aw],
    }),
    (Rule {
        pattern: "ow",
        phones: &[P::Ow],
    }),
    (Rule {
        pattern: "oy",
        phones: &[P::Oy],
    }),
    (Rule {
        pattern: "ue",
        phones: &[P::Uw],
    }),
    (Rule {
        pattern: "ui",
        phones: &[P::Uw],
    }),
    (Rule {
        pattern: "uy",
        phones: &[P::Ay],
    }),
    // Consonant digraphs and contexts.
    (Rule {
        pattern: "sch",
        phones: &[P::S, P::K],
    }),
    (Rule {
        pattern: "tch",
        phones: &[P::T, P::Sh],
    }),
    (Rule {
        pattern: "ch",
        phones: &[P::T, P::Sh],
    }),
    (Rule {
        pattern: "ck",
        phones: &[P::K],
    }),
    (Rule {
        pattern: "ss",
        phones: &[P::S],
    }),
    (Rule {
        pattern: "ph",
        phones: &[P::F],
    }),
    (Rule {
        pattern: "qu",
        phones: &[P::K, P::W],
    }),
    (Rule {
        pattern: "th",
        phones: &[P::Th],
    }),
    (Rule {
        pattern: "wh",
        phones: &[P::W],
    }),
    (Rule {
        pattern: "gh",
        phones: &[P::G],
    }),
    (Rule {
        pattern: "sh",
        phones: &[P::Sh],
    }),
    (Rule {
        pattern: "ng",
        phones: &[P::Ng],
    }),
    (Rule {
        pattern: "nk",
        phones: &[P::Ng, P::K],
    }),
    (Rule {
        pattern: "#kn",
        phones: &[P::N],
    }),
    (Rule {
        pattern: "#wr",
        phones: &[P::R],
    }),
    (Rule {
        pattern: "#gn",
        phones: &[P::N],
    }),
    (Rule {
        pattern: "ce",
        phones: &[P::S, P::Eh],
    }),
    (Rule {
        pattern: "ci",
        phones: &[P::S, P::Ih],
    }),
    (Rule {
        pattern: "cy",
        phones: &[P::S, P::Ih],
    }),
    (Rule {
        pattern: "cy#",
        phones: &[P::S, P::Iy],
    }),
    (Rule {
        pattern: "#ya",
        phones: &[P::Y, P::Ae],
    }),
    (Rule {
        pattern: "#ye",
        phones: &[P::Y, P::Eh],
    }),
    (Rule {
        pattern: "#yo",
        phones: &[P::Y, P::Ow],
    }),
    (Rule {
        pattern: "#yu",
        phones: &[P::Y, P::Uw],
    }),
    (Rule {
        pattern: "#y",
        phones: &[P::Y],
    }),
    (Rule {
        pattern: "#a",
        phones: &[P::Ax],
    }),
    // Single letters.
    (Rule {
        pattern: "a",
        phones: &[P::Ae],
    }),
    (Rule {
        pattern: "e",
        phones: &[P::Eh],
    }),
    (Rule {
        pattern: "i",
        phones: &[P::Ih],
    }),
    (Rule {
        pattern: "o",
        phones: &[P::Aa],
    }),
    (Rule {
        pattern: "u",
        phones: &[P::Ah],
    }),
    (Rule {
        pattern: "y",
        phones: &[P::Ih],
    }),
    (Rule {
        pattern: "b",
        phones: &[P::B],
    }),
    (Rule {
        pattern: "c",
        phones: &[P::K],
    }),
    (Rule {
        pattern: "d",
        phones: &[P::D],
    }),
    (Rule {
        pattern: "f",
        phones: &[P::F],
    }),
    (Rule {
        pattern: "g",
        phones: &[P::G],
    }),
    (Rule {
        pattern: "h",
        phones: &[P::Hh],
    }),
    (Rule {
        pattern: "j",
        phones: &[P::D, P::Zh],
    }),
    (Rule {
        pattern: "k",
        phones: &[P::K],
    }),
    (Rule {
        pattern: "l",
        phones: &[P::L],
    }),
    (Rule {
        pattern: "m",
        phones: &[P::M],
    }),
    (Rule {
        pattern: "n",
        phones: &[P::N],
    }),
    (Rule {
        pattern: "p",
        phones: &[P::P],
    }),
    (Rule {
        pattern: "r",
        phones: &[P::R],
    }),
    (Rule {
        pattern: "s",
        phones: &[P::S],
    }),
    (Rule {
        pattern: "t",
        phones: &[P::T],
    }),
    (Rule {
        pattern: "v",
        phones: &[P::V],
    }),
    (Rule {
        pattern: "w",
        phones: &[P::W],
    }),
    (Rule {
        pattern: "x",
        phones: &[P::K, P::S],
    }),
    (Rule {
        pattern: "z",
        phones: &[P::Z],
    }),
];

/// One lowercase word to phonemes.
/// Words that carry no beat of their own in a sentence: articles, pronouns,
/// prepositions, conjunctions and auxiliary verbs.
const FUNCTION_WORDS: &[&str] = &[
    "a", "an", "the", "and", "or", "but", "nor", "of", "to", "in", "on", "at", "by", "for", "with",
    "from", "into", "onto", "as", "if", "so", "than", "that", "this", "these", "those", "is",
    "are", "was", "were", "be", "been", "being", "am", "do", "does", "did", "have", "has", "had",
    "will", "would", "can", "could", "shall", "should", "may", "might", "must", "i", "you", "he",
    "she", "it", "we", "they", "me", "him", "her", "us", "them", "my", "your", "his", "its", "our",
    "their",
];

/// One word's phonemes, with the first vowel of a content word stressed:
/// English stresses most words on their first syllable, and the stress
/// gives the sentence its beats.
pub(super) fn word_phones(word: &str) -> Vec<Phone> {
    let normalized = normalize(word);
    let mut phones = unstressed_phones(&normalized);
    if !FUNCTION_WORDS.contains(&normalized.as_str())
        && let Some(first) = phones
            .iter_mut()
            .find(|phone| matches!(phone, Phone::Sound(ph) if ph.is_nucleus()))
        && let Phone::Sound(ph) = *first
    {
        *first = Phone::Stressed(ph);
    }
    phones
}

fn unstressed_phones(normalized: &str) -> Vec<Phone> {
    if let Some((_, phones)) = EXCEPTIONS.iter().find(|(w, _)| *w == normalized) {
        return phones.iter().map(|p| Phone::Sound(*p)).collect();
    }
    // A trailing y is the long-i of a short word (by, cry) but the long-e of
    // a longer one (city, money). Split it off, speak the stem, append. Words
    // ending in cy keep the y for the rules, so the c softens in front of it.
    if normalized.len() > 3 && normalized.ends_with('y') && !normalized.ends_with("cy") {
        let stem: Vec<char> = normalized
            .chars()
            .take(normalized.chars().count() - 1)
            .collect();
        let mut out: Vec<Phone> = apply_rules(&stem, RULES)
            .into_iter()
            .map(|(p, _, _)| Phone::Sound(p))
            .collect();
        out.push(Phone::Sound(P::Iy));
        return out;
    }
    apply_rules(&normalized.chars().collect::<Vec<char>>(), RULES)
        .into_iter()
        .map(|(p, _, _)| Phone::Sound(p))
        .collect()
}

/// Lowercase, accents folded, apostrophes dropped.
pub(super) fn normalize(word: &str) -> String {
    let mut out = String::with_capacity(word.len());
    for c in word.to_lowercase().chars() {
        if c == '\'' || c == '\u{0301}' {
            continue;
        }
        out.push(match c {
            'á' | 'à' | 'â' | 'ä' | 'ã' | 'å' => 'a',
            'é' | 'è' | 'ê' | 'ë' => 'e',
            'í' | 'ì' | 'î' | 'ï' => 'i',
            'ó' | 'ò' | 'ô' | 'ö' | 'õ' => 'o',
            'ú' | 'ù' | 'û' | 'ü' => 'u',
            'ñ' => 'n',
            'ç' => 'c',
            c => c,
        });
    }
    out
}

pub(super) const DIGITS: [&str; 10] = [
    "zero", "one", "two", "three", "four", "five", "six", "seven", "eight", "nine",
];

const TEENS: [&str; 10] = [
    "ten",
    "eleven",
    "twelve",
    "thirteen",
    "fourteen",
    "fifteen",
    "sixteen",
    "seventeen",
    "eighteen",
    "nineteen",
];

const TENS: [&str; 8] = [
    "twenty", "thirty", "forty", "fifty", "sixty", "seventy", "eighty", "ninety",
];

/// Words for `n`. Above 999,999 the digits are named one by one, which keeps
/// the table small and any number speakable.
pub(super) fn number_words(n: i64) -> Vec<&'static str> {
    if n < 0 {
        return number_words(-n);
    }
    if n == 0 {
        return vec!["zero"];
    }
    if n > 999_999 {
        return n
            .to_string()
            .chars()
            .map(|c| DIGITS[(c as u8 - b'0') as usize])
            .collect();
    }
    let mut out = Vec::new();
    if n >= 1000 {
        under_thousand(&mut out, n / 1000);
        out.push("thousand");
    }
    under_thousand(&mut out, n % 1000);
    out
}

fn under_thousand(out: &mut Vec<&'static str>, n: i64) {
    if n == 0 {
        return;
    }
    if n >= 100 {
        out.push(DIGITS[(n / 100) as usize]);
        out.push("hundred");
    }
    let rest = n % 100;
    if rest == 0 {
        return;
    }
    if rest < 10 {
        out.push(DIGITS[rest as usize]);
    } else if rest < 20 {
        out.push(TEENS[(rest - 10) as usize]);
    } else {
        out.push(TENS[(rest / 10 - 2) as usize]);
        if rest % 10 > 0 {
            out.push(DIGITS[(rest % 10) as usize]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn phones(word: &str) -> Vec<P> {
        word_phones(word)
            .into_iter()
            .filter_map(|p| match p {
                Phone::Sound(p) | Phone::Stressed(p) => Some(p),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn reads_the_common_patterns() {
        assert_eq!(phones("chest"), vec![P::T, P::Sh, P::Eh, P::S, P::T]);
        assert_eq!(phones("light"), vec![P::L, P::Ay, P::T]);
        assert_eq!(phones("sing"), vec![P::S, P::Ih, P::Ng]);
        assert_eq!(
            phones("station"),
            vec![P::S, P::T, P::Ey, P::Sh, P::Ax, P::N]
        );
        assert_eq!(phones("city"), vec![P::S, P::Ih, P::T, P::Iy]);
        assert_eq!(phones("dont"), vec![P::D, P::Ow, P::N, P::T]);
        assert_eq!(phones("cared"), vec![P::K, P::Eh, P::R, P::D]);
        assert_eq!(phones("misses"), vec![P::M, P::Ih, P::S, P::Ih, P::Z]);
    }

    #[test]
    fn reads_numbers_as_words() {
        assert_eq!(number_words(0), vec!["zero"]);
        assert_eq!(number_words(42), vec!["forty", "two"]);
        assert_eq!(
            number_words(2145),
            vec!["two", "thousand", "one", "hundred", "forty", "five"]
        );
        assert_eq!(
            number_words(1_000_000),
            vec!["one", "zero", "zero", "zero", "zero", "zero", "zero"]
        );
    }
}
