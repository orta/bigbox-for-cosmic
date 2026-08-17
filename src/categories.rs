// SPDX-License-Identifier: MPL-2.0

//! Curated categories and tags for BigBox's class timetable.
//!
//! The API does expose its own `activity_groups` (Cardio, Toning, Mind and
//! Body, …), but they are both too coarse to browse by and visibly wrong in
//! places — "Beginners Hyrox" is filed under *Dance*, "Zumba Gold" under
//! *Aqua*. So this table is maintained here instead, derived from each class's
//! own description in the member API.
//!
//! Every class gets exactly one [`Category`], which is what the planning view
//! groups by, plus any number of free-form tags, which is what search matches
//! on. Tags are the goal- and body-part words a member would actually type —
//! `legs`, `strength`, `weights`, `core` — and deliberately overlap.
//!
//! Classes the club adds later won't be in the table; [`lookup`] falls back to
//! keyword matching on the name so they still land somewhere sensible.

/// The heading a class appears under in the planning view.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Category {
    StrengthWeights,
    HiitConditioning,
    Cardio,
    Dance,
    MindBody,
    Reformer,
    Aqua,
    Ems,
    Over50s,
    KidsFamily,
    Appointments,
}

impl Category {
    pub fn label(self) -> &'static str {
        match self {
            Category::StrengthWeights => "Strength & Weights",
            Category::HiitConditioning => "HIIT & Conditioning",
            Category::Cardio => "Cardio",
            Category::Dance => "Dance",
            Category::MindBody => "Mind & Body",
            Category::Reformer => "Reformer",
            Category::Aqua => "Aqua",
            Category::Ems => "EMS",
            Category::Over50s => "Over 50s",
            Category::KidsFamily => "Kids & Family",
            Category::Appointments => "Inductions & Check-ups",
        }
    }

    /// A one-line explainer shown under the heading in the planning view.
    ///
    /// Most of these are self-evident from the name; EMS and Reformer are the
    /// ones that genuinely need explaining, since both are equipment brands as
    /// much as class types and neither tells you what you'd actually be doing.
    pub fn description(self) -> &'static str {
        match self {
            Category::StrengthWeights => {
                "Barbells, dumbbells and bodyweight — building muscle and tone."
            }
            Category::HiitConditioning => {
                "Circuits and intervals: short hard efforts with brief recovery."
            }
            Category::Cardio => "Heart-rate and endurance work — bikes, steps and aerobics.",
            Category::Dance => "Choreographed routines set to music. No experience needed.",
            Category::MindBody => {
                "Yoga, Pilates, stretching and meditation — flexibility and calm."
            }
            Category::Reformer => {
                "Pilates on a sprung carriage. The springs both assist and resist, \
                 so it suits beginners and advanced alike — low impact on the joints."
            }
            Category::Aqua => {
                "In the pool. The water supports your joints while adding resistance, \
                 so it's low impact but still hard work. No swimming ability needed."
            }
            Category::Ems => {
                "Electro Muscle Stimulation, branded Electric Box here. You wear a \
                 suit that sends electrical pulses through your muscles while you \
                 exercise, contracting far more fibres than movement alone. Sessions \
                 are 20 minutes, one-to-one with a trainer, and low impact — used for \
                 strength, recovery and pain relief."
            }
            Category::Over50s => "Slower paced and lower impact, aimed at older adults.",
            Category::KidsFamily => "For children, teenagers and families training together.",
            Category::Appointments => {
                "One-to-one inductions and assessments rather than group classes — \
                 book these to get set up on the gym equipment."
            }
        }
    }

    /// Display order for the grouped view. Roughly "most people are here for
    /// this" first, with one-to-one appointments last.
    pub fn all() -> &'static [Category] {
        &[
            Category::StrengthWeights,
            Category::HiitConditioning,
            Category::Cardio,
            Category::Dance,
            Category::MindBody,
            Category::Reformer,
            Category::Aqua,
            Category::Ems,
            Category::Over50s,
            Category::KidsFamily,
            Category::Appointments,
        ]
    }

    /// Words that should surface this category in search, beyond its label.
    fn search_aliases(self) -> &'static [&'static str] {
        match self {
            Category::StrengthWeights => &["strength", "weights", "lifting", "toning"],
            Category::HiitConditioning => &["hiit", "conditioning", "circuit", "bootcamp"],
            Category::Cardio => &["cardio", "fitness"],
            Category::Dance => &["dance", "dancing"],
            Category::MindBody => &["mind", "body", "wellness", "relax"],
            Category::Reformer => &["reformer", "pilates"],
            Category::Aqua => &["aqua", "water", "pool", "swimming"],
            Category::Ems => &["ems", "electro", "personal training", "pt"],
            Category::Over50s => &["over 50s", "seniors", "mature", "gentle"],
            Category::KidsFamily => &["kids", "family", "children", "teens"],
            Category::Appointments => &["induction", "check-up", "appointment", "assessment"],
        }
    }
}

/// What we know about one class.
#[derive(Debug, Clone, Copy)]
pub struct ClassInfo {
    pub category: Category,
    pub tags: &'static [&'static str],
}

use Category::*;

/// Curated from each class's description in the member API. Names are matched
/// case-insensitively against the activity name.
#[rustfmt::skip]
static CLASSES: &[(&str, Category, &[&str])] = &[
    // --- Strength & weights ---
    ("Body Pump",            StrengthWeights, &["strength", "weights", "barbell", "toning", "full-body", "core", "legs"]),
    ("Grit Strength",        StrengthWeights, &["strength", "weights", "barbell", "hiit", "full-body"]),
    ("Body Sculpt",          StrengthWeights, &["strength", "weights", "toning", "full-body", "bodyweight", "intervals"]),
    ("Low and Tone",         StrengthWeights, &["strength", "toning", "full-body", "low-impact"]),
    ("Legs, Bums and Tums",  StrengthWeights, &["legs", "glutes", "core", "toning", "lower-body"]),
    ("Abs Blast",            StrengthWeights, &["core", "abs", "strength", "stability"]),

    // --- HIIT & conditioning ---
    ("HYROX",                HiitConditioning, &["hiit", "running", "functional", "weights", "endurance", "full-body", "advanced"]),
    ("Beginners Hyrox",      HiitConditioning, &["hiit", "running", "functional", "weights", "endurance", "beginner"]),
    ("Bootcamp",             HiitConditioning, &["hiit", "circuit", "cardio", "weights", "full-body", "functional"]),
    ("Boxing Bootcamp",      HiitConditioning, &["boxing", "martial-arts", "hiit", "circuit", "weights", "cardio", "full-body"]),
    ("Beginners Bootcamp",   HiitConditioning, &["hiit", "circuit", "cardio", "full-body", "beginner"]),
    ("HIIT and Tone",        HiitConditioning, &["hiit", "cardio", "weights", "toning", "endurance", "full-body"]),

    // --- Cardio ---
    ("Spin",                 Cardio, &["cardio", "cycling", "legs", "fat-burn", "endurance"]),
    ("PEAK SPIN",            Cardio, &["cardio", "cycling", "legs", "endurance", "advanced"]),
    ("Body Attack",          Cardio, &["cardio", "hiit", "athletic", "functional", "full-body"]),
    ("Body Combat",          Cardio, &["martial-arts", "boxing", "cardio", "hiit", "full-body", "fat-burn"]),
    ("Step Aerobics",        Cardio, &["cardio", "aerobics", "step", "legs", "coordination"]),
    ("Old Skool Aerobics",   Cardio, &["cardio", "aerobics", "coordination", "dance"]),
    ("Aerotone",             Cardio, &["cardio", "aerobics", "toning", "weights", "coordination"]),
    ("BIGBOX Bounce",        Cardio, &["cardio", "trampoline", "dance", "legs", "fat-burn", "low-impact"]),

    // --- Dance ---
    ("Zumba",                Dance, &["dance", "cardio", "latin", "fat-burn"]),
    ("Sh'Bam",               Dance, &["dance", "cardio", "fat-burn"]),
    ("Clubbercise",          Dance, &["dance", "cardio", "toning", "fat-burn"]),
    ("Soca",                 Dance, &["dance", "cardio", "endurance", "caribbean"]),

    // --- Mind & body ---
    ("Pilates",              MindBody, &["pilates", "core", "strength", "flexibility", "stability", "low-impact"]),
    ("Beginners Pilates",    MindBody, &["pilates", "core", "strength", "beginner", "low-impact"]),
    ("Clinical Pilates",     MindBody, &["pilates", "rehab", "low-impact", "gentle", "beginner"]),
    ("Pilates Mix (Clinical and Beginners)", MindBody, &["pilates", "rehab", "beginner", "low-impact"]),
    ("Circle Pilates",       MindBody, &["pilates", "core", "strength", "equipment"]),
    ("Yoga",                 MindBody, &["yoga", "flexibility", "relaxation", "breathwork"]),
    ("Beginners Yoga",       MindBody, &["yoga", "flexibility", "relaxation", "beginner", "low-impact"]),
    ("Hatha Yoga",           MindBody, &["yoga", "flexibility", "breathwork", "relaxation", "low-impact"]),
    ("Vinyasa Yoga",         MindBody, &["yoga", "flexibility", "strength", "flow"]),
    ("Body Balance",         MindBody, &["yoga", "pilates", "tai-chi", "flexibility", "strength", "relaxation", "low-impact"]),
    ("Pure Stretch",         MindBody, &["flexibility", "stretch", "mobility", "pilates", "yoga", "low-impact"]),
    ("KINISIFLOW",           MindBody, &["flexibility", "mobility", "recovery", "flow", "strength"]),
    ("Barre",                MindBody, &["barre", "ballet", "legs", "core", "toning", "flexibility", "strength"]),
    ("Tai Chi",              MindBody, &["tai-chi", "balance", "mobility", "relaxation", "low-impact", "gentle"]),
    ("Dynamic Meditation",   MindBody, &["meditation", "breathwork", "relaxation", "qigong", "low-impact"]),
    ("Sunday śavāsana",      MindBody, &["meditation", "relaxation", "yoga", "sleep", "low-impact"]),

    // --- Reformer ---
    ("Beginners Reformer",   Reformer, &["pilates", "reformer", "beginner", "core", "flexibility", "full-body", "low-impact"]),
    ("Full Body Reformer",   Reformer, &["pilates", "reformer", "full-body", "strength", "core", "flexibility"]),
    ("Advanced Reformer",    Reformer, &["pilates", "reformer", "advanced", "core", "strength", "balance"]),
    ("Clinical Reformer",    Reformer, &["pilates", "reformer", "rehab", "low-impact", "gentle"]),
    ("Ladies Only Reformer", Reformer, &["pilates", "reformer", "legs", "lower-body", "full-body", "women-only"]),
    ("Rise and Reform",      Reformer, &["pilates", "reformer", "stretch", "core", "posture", "morning"]),
    ("Reformer Taster",      Reformer, &["pilates", "reformer", "beginner"]),

    // --- Aqua ---
    ("Aqua Fit",             Aqua, &["aqua", "cardio", "toning", "low-impact", "full-body"]),
    ("Aqua Combat",          Aqua, &["aqua", "martial-arts", "toning", "cardio", "low-impact"]),
    ("Aqua Groove",          Aqua, &["aqua", "dance", "cardio", "low-impact"]),
    ("Aqua Zumba",           Aqua, &["aqua", "dance", "latin", "cardio", "low-impact"]),
    ("AQUA PILATES",         Aqua, &["aqua", "pilates", "core", "flexibility", "balance", "low-impact"]),
    ("Aqua Med",             Aqua, &["aqua", "low-impact", "gentle", "strength", "over-50s"]),

    // --- EMS ---
    ("EMS Session",          Ems, &["ems", "strength", "full-body", "recovery", "personal-training", "low-impact"]),

    // --- Over 50s ---
    ("Mature Movers",        Over50s, &["over-50s", "low-impact", "cardio", "mobility", "gentle", "beginner"]),
    ("Zumba Gold",           Over50s, &["dance", "over-50s", "low-impact", "cardio", "beginner"]),

    // --- Kids & family ---
    ("Family Bootcamp 8yr+", KidsFamily, &["family", "kids", "cardio", "hiit"]),
    ("Family Time (age 4+)", KidsFamily, &["family", "kids"]),
    ("Kids Fun session Age 5-12", KidsFamily, &["kids"]),
    ("Kids Gymnastics Age 5+", KidsFamily, &["kids", "gymnastics"]),
    ("Teen Performance Workshop Age 11-15", KidsFamily, &["teens", "kids", "sports", "agility", "speed"]),

    // --- Appointments ---
    ("BIGBOX Fit Induction", Appointments, &["induction", "beginner", "personal-training"]),
    ("Bio Circuit Induction", Appointments, &["induction", "beginner", "weights", "personal-training"]),
    ("Box12 Induction",      Appointments, &["induction", "boxing", "beginner"]),
    ("Teen Gym Induction",   Appointments, &["induction", "teens", "kids"]),
    ("BIGBOX Check-Up",      Appointments, &["assessment", "check-up", "personal-training"]),
];

/// Categorises a class by name.
///
/// Falls back to keyword matching so classes added to the timetable after this
/// table was written still group sensibly instead of vanishing into a catch-all.
pub fn lookup(name: &str) -> ClassInfo {
    let needle = name.trim().to_lowercase();

    if let Some(&(_, category, tags)) = CLASSES
        .iter()
        .find(|(class, _, _)| class.to_lowercase() == needle)
    {
        return ClassInfo { category, tags };
    }

    guess(&needle)
}

/// Keyword fallback for classes not in the table, most specific first.
fn guess(needle: &str) -> ClassInfo {
    let has = |word: &str| needle.contains(word);

    let category = if has("induction") || has("check-up") || has("taster") {
        Appointments
    } else if has("aqua") {
        Aqua
    } else if has("reformer") {
        Reformer
    } else if has("ems") {
        Ems
    } else if has("kids") || has("family") || has("teen") || has("junior") {
        KidsFamily
    } else if has("gold") || has("over 50") || has("mature") {
        Over50s
    } else if has("yoga") || has("pilates") || has("stretch") || has("meditation") || has("barre") {
        MindBody
    } else if has("zumba") || has("dance") || has("bam") {
        Dance
    } else if has("hyrox") || has("bootcamp") || has("hiit") || has("circuit") {
        HiitConditioning
    } else if has("pump") || has("strength") || has("tone") || has("sculpt") || has("abs") {
        StrengthWeights
    } else {
        Cardio
    };

    ClassInfo {
        category,
        tags: &[],
    }
}

/// Whether a class matches a free-text query like `legs`, `strength` or
/// `weights`. Matches the class name, its tags, and its category.
///
/// Multi-word queries are treated as "all words must match somewhere", so
/// `beginner pilates` narrows rather than widens.
pub fn matches(name: &str, query: &str) -> bool {
    let query = query.trim().to_lowercase();
    if query.is_empty() {
        return true;
    }

    let info = lookup(name);
    let name = name.to_lowercase();
    let category_label = info.category.label().to_lowercase();

    query.split_whitespace().all(|word| {
        name.contains(word)
            || category_label.contains(word)
            || info.tags.iter().any(|tag| tag.contains(word))
            || info
                .category
                .search_aliases()
                .iter()
                .any(|alias| alias.contains(word))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_classes_use_the_curated_table() {
        assert_eq!(lookup("Body Pump").category, StrengthWeights);
        assert_eq!(lookup("HYROX").category, HiitConditioning);
        assert_eq!(lookup("Vinyasa Yoga").category, MindBody);
        assert_eq!(lookup("EMS Session").category, Ems);
    }

    #[test]
    fn lookup_ignores_case_and_padding() {
        assert_eq!(lookup("  body pump  ").category, StrengthWeights);
        assert_eq!(lookup("AQUA PILATES").category, Aqua);
    }

    /// The club's own groups file these two wrongly; the curated table is the
    /// whole reason this module exists.
    #[test]
    fn corrects_the_api_groups_that_are_wrong() {
        assert_eq!(lookup("Beginners Hyrox").category, HiitConditioning);
        assert_eq!(lookup("Zumba Gold").category, Over50s);
    }

    #[test]
    fn searching_by_goal_finds_the_right_classes() {
        assert!(matches("Legs, Bums and Tums", "legs"));
        assert!(matches("Spin", "legs"));
        assert!(!matches("Hatha Yoga", "legs"));

        assert!(matches("Body Pump", "weights"));
        assert!(matches("Grit Strength", "weights"));
        assert!(!matches("Sunday śavāsana", "weights"));

        assert!(matches("Body Pump", "strength"));
        assert!(matches("Pilates", "strength"));
        assert!(!matches("Zumba", "strength"));
    }

    #[test]
    fn search_matches_names_and_categories_too() {
        assert!(matches("Vinyasa Yoga", "yoga"));
        assert!(matches("Aqua Fit", "pool"));
        assert!(matches("EMS Session", "ems"));
        assert!(matches("Beginners Reformer", "reformer"));
    }

    #[test]
    fn multi_word_queries_narrow_the_results() {
        assert!(matches("Beginners Pilates", "beginner pilates"));
        assert!(!matches("Advanced Reformer", "beginner pilates"));
    }

    #[test]
    fn empty_query_matches_everything() {
        assert!(matches("Anything At All", ""));
        assert!(matches("Body Pump", "   "));
    }

    #[test]
    fn unknown_classes_fall_back_to_keywords() {
        assert_eq!(lookup("Aqua Blast").category, Aqua);
        assert_eq!(lookup("Sunrise Yoga Flow").category, MindBody);
        assert_eq!(lookup("Kettlebell Strength").category, StrengthWeights);
        assert_eq!(lookup("Kids Trampolining").category, KidsFamily);
        // Nothing recognisable still lands somewhere rather than disappearing.
        assert_eq!(lookup("Mystery Class").category, Cardio);
    }

    /// A typo'd duplicate would silently shadow the earlier entry.
    #[test]
    fn table_has_no_duplicate_class_names() {
        let mut names: Vec<String> = CLASSES
            .iter()
            .map(|(name, _, _)| name.to_lowercase())
            .collect();
        names.sort();
        let count = names.len();
        names.dedup();
        assert_eq!(names.len(), count, "duplicate class name in CLASSES");
    }

    #[test]
    fn every_category_explains_itself() {
        for category in Category::all() {
            let description = category.description();
            assert!(
                description.len() > 20,
                "{category:?} needs a real description"
            );
        }
        // The acronym is the whole reason this exists — spell it out.
        assert!(
            Category::Ems
                .description()
                .contains("Electro Muscle Stimulation")
        );
    }

    #[test]
    fn every_category_is_listed_in_display_order() {
        for (_, category, _) in CLASSES {
            assert!(
                Category::all().contains(category),
                "{category:?} missing from Category::all()"
            );
        }
    }
}
