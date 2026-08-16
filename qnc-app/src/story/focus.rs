//! Timeline focus for native Story: same ←/→ command; focus chooses the target.

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum FocusTarget {
    Playhead,
    In,
    Out,
}

#[derive(Debug, Clone)]
pub(super) struct TimelineFocus {
    pub target: FocusTarget,
}

impl Default for TimelineFocus {
    fn default() -> Self {
        Self {
            target: FocusTarget::Playhead,
        }
    }
}

impl TimelineFocus {
    pub fn clear(&mut self) {
        self.target = FocusTarget::Playhead;
    }

    pub fn is_playhead(&self) -> bool {
        matches!(self.target, FocusTarget::Playhead)
    }

    pub fn select_in(&mut self) {
        self.target = FocusTarget::In;
    }

    pub fn select_out(&mut self) {
        self.target = FocusTarget::Out;
    }

    pub fn focus_next(&mut self, chain: &[FocusTarget]) {
        if chain.is_empty() {
            self.clear();
            return;
        }
        let idx = chain.iter().position(|t| t == &self.target);
        let next = match idx {
            Some(i) => (i + 1) % chain.len(),
            None => 0,
        };
        self.target = chain[next].clone();
    }

    pub fn focus_prev(&mut self, chain: &[FocusTarget]) {
        if chain.is_empty() {
            self.clear();
            return;
        }
        let idx = chain.iter().position(|t| t == &self.target);
        let prev = match idx {
            Some(0) | None => chain.len() - 1,
            Some(i) => i - 1,
        };
        self.target = chain[prev].clone();
    }
}

/// Source editing Tab cycle: playhead → IN → OUT.
#[cfg(test)]
pub(super) fn source_focus_chain() -> Vec<FocusTarget> {
    vec![FocusTarget::Playhead, FocusTarget::In, FocusTarget::Out]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn focus_cycles_playhead_in_out() {
        let chain = source_focus_chain();
        let mut focus = TimelineFocus::default();
        focus.focus_next(&chain);
        assert_eq!(focus.target, FocusTarget::In);
        focus.focus_next(&chain);
        assert_eq!(focus.target, FocusTarget::Out);
        focus.focus_next(&chain);
        assert_eq!(focus.target, FocusTarget::Playhead);
        focus.focus_prev(&chain);
        assert_eq!(focus.target, FocusTarget::Out);
    }
}
