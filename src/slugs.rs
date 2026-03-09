use rand::seq::IndexedRandom;

const ADJECTIVES: &[&str] = &[
    "swift", "bright", "calm", "bold", "keen", "warm", "cool", "wild", "fair", "glad", "quick",
    "brave", "proud", "true", "wise",
];

const ANIMALS: &[&str] = &[
    "penguin", "falcon", "otter", "fox", "heron", "whale", "eagle", "tiger", "panda", "koala",
    "raven", "wolf", "lynx", "hawk", "crane",
];

pub fn generate_slug() -> String {
    let mut rng = rand::rng();
    let adj = ADJECTIVES.choose(&mut rng).expect("adjectives not empty");
    let animal = ANIMALS.choose(&mut rng).expect("animals not empty");
    format!("{adj}-{animal}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slug_has_adjective_dash_animal_format() {
        let slug = generate_slug();
        let parts: Vec<&str> = slug.split('-').collect();
        assert_eq!(parts.len(), 2, "slug should be adjective-animal: {slug}");
        assert!(
            ADJECTIVES.contains(&parts[0]),
            "first word should be an adjective: {slug}"
        );
        assert!(
            ANIMALS.contains(&parts[1]),
            "second word should be an animal: {slug}"
        );
    }

    #[test]
    fn slugs_are_not_always_identical() {
        let slugs: Vec<String> = (0..20).map(|_| generate_slug()).collect();
        let unique: std::collections::HashSet<&String> = slugs.iter().collect();
        assert!(unique.len() > 1, "20 slugs should not all be identical");
    }
}
