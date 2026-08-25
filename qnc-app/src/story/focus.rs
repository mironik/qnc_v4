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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PanelFocus {
    MediaPool,
    SegmentPanel,
    SourceTimeline,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SourceNavigationTarget {
    Start,
    MarkIn,
    MarkOut,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SourceNavigationItem {
    target: SourceNavigationTarget,
    frame: i64,
    order: i32,
}

impl SourceNavigationItem {
    fn new(target: SourceNavigationTarget, frame: i64) -> Self {
        Self {
            target,
            frame: frame.max(0),
            order: match target {
                SourceNavigationTarget::Start => 0,
                SourceNavigationTarget::MarkIn => 1,
                SourceNavigationTarget::MarkOut => 2,
            },
        }
    }
}

impl PanelFocus {
    pub fn label(self) -> &'static str {
        match self {
            PanelFocus::MediaPool => "Media pool",
            PanelFocus::SegmentPanel => "Segmenti",
            PanelFocus::SourceTimeline => "Source timeline",
        }
    }
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

    #[allow(dead_code)]
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

    #[allow(dead_code)]
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

pub(super) fn panel_focus_chain(has_segment_panel: bool) -> Vec<PanelFocus> {
    let mut chain = vec![PanelFocus::MediaPool];
    if has_segment_panel {
        chain.push(PanelFocus::SegmentPanel);
    }
    chain.push(PanelFocus::SourceTimeline);
    chain
}

pub(super) fn next_panel_focus(
    current: PanelFocus,
    has_segment_panel: bool,
    direction: i32,
) -> PanelFocus {
    let chain = panel_focus_chain(has_segment_panel);
    let idx = chain
        .iter()
        .position(|panel| *panel == current)
        .unwrap_or(0);
    if direction < 0 {
        chain
            .get(idx.checked_sub(1).unwrap_or(chain.len().saturating_sub(1)))
            .copied()
            .unwrap_or(current)
    } else {
        chain
            .get((idx + 1) % chain.len())
            .copied()
            .unwrap_or(current)
    }
}

pub(super) fn adjacent_source_navigation_target(
    focus: &TimelineFocus,
    playhead_frame: i64,
    mark_in_frame: Option<i64>,
    mark_out_frame: Option<i64>,
    direction: i32,
) -> Option<SourceNavigationTarget> {
    if direction == 0 {
        return None;
    }
    let mut items = vec![SourceNavigationItem::new(SourceNavigationTarget::Start, 0)];
    if let Some(frame) = mark_in_frame {
        items.push(SourceNavigationItem::new(
            SourceNavigationTarget::MarkIn,
            frame,
        ));
    }
    if let Some(frame) = mark_out_frame {
        items.push(SourceNavigationItem::new(
            SourceNavigationTarget::MarkOut,
            frame,
        ));
    }
    items.sort_by_key(|item| (item.frame, item.order));
    items.dedup_by(|left, right| left.target == right.target && left.frame == right.frame);

    let selected_index = match focus.target {
        FocusTarget::In => items
            .iter()
            .position(|item| item.target == SourceNavigationTarget::MarkIn),
        FocusTarget::Out => items
            .iter()
            .position(|item| item.target == SourceNavigationTarget::MarkOut),
        FocusTarget::Playhead => None,
    };
    if let Some(current_index) = selected_index {
        let next = if direction < 0 {
            current_index.checked_sub(1)?
        } else {
            current_index
                .checked_add(1)
                .filter(|next| *next < items.len())?
        };
        return items.get(next).map(|item| item.target);
    }

    let frame = playhead_frame.max(0);
    if direction < 0 {
        if frame == 0 {
            return None;
        }
        items
            .iter()
            .rfind(|item| (item.frame, item.order) < (frame, i32::MAX))
            .map(|item| item.target)
    } else {
        items
            .iter()
            .find(|item| (item.frame, item.order) > (frame, 0))
            .map(|item| item.target)
    }
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

    #[test]
    fn panel_focus_skips_missing_segment_panel() {
        assert_eq!(
            panel_focus_chain(false),
            vec![PanelFocus::MediaPool, PanelFocus::SourceTimeline]
        );
        assert_eq!(
            next_panel_focus(PanelFocus::MediaPool, false, 1),
            PanelFocus::SourceTimeline
        );
        assert_eq!(
            next_panel_focus(PanelFocus::MediaPool, true, 1),
            PanelFocus::SegmentPanel
        );
    }

    #[test]
    fn source_navigation_uses_active_focus_before_playhead_scan() {
        let mut focus = TimelineFocus::default();
        assert_eq!(
            adjacent_source_navigation_target(&focus, 5, Some(10), Some(20), 1),
            Some(SourceNavigationTarget::MarkIn)
        );
        assert_eq!(
            adjacent_source_navigation_target(&focus, 25, Some(10), Some(20), -1),
            Some(SourceNavigationTarget::MarkOut)
        );

        focus.select_in();
        assert_eq!(
            adjacent_source_navigation_target(&focus, 5, Some(10), Some(20), 1),
            Some(SourceNavigationTarget::MarkOut)
        );
        assert_eq!(
            adjacent_source_navigation_target(&focus, 5, Some(10), Some(20), -1),
            Some(SourceNavigationTarget::Start)
        );
    }
}
