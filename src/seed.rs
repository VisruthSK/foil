// Persisted RNG contract: XOR the recorded suite seed with fixed ASCII domains.
// Changing either domain or this operation requires a foil version change.
const SCHEDULE: u64 = u64::from_be_bytes(*b"schedule");
const POSTERIOR: u64 = u64::from_be_bytes(*b"post_rng");

pub(crate) const fn schedule(seed: u64) -> u64 {
    seed ^ SCHEDULE
}

pub(crate) const fn posterior(seed: u64) -> u64 {
    seed ^ POSTERIOR
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn domain_seeds_are_pinned() {
        assert_eq!(schedule(42), 0x7363_6865_6475_6c4f);
        assert_eq!(posterior(42), 0x706f_7374_5f72_6e4d);
        assert_ne!(schedule(42), posterior(42));
    }
}
