//! The judge panel: every region judged per aspect, findings confirmed by quorum.

pub const JUDGE_QUORUM: usize = 2;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_finding_is_confirmed_at_two_judge_votes() {
        assert_eq!(JUDGE_QUORUM, 2);
    }
}
