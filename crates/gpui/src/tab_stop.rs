use std::fmt::Debug;

use collections::FxHashMap;

use crate::{FocusHandle, FocusId};

/// Represents a collection of focus handles using the tab-index APIs.
///
/// Registration appends nodes; tab order is sorted only when focus traverses it.
#[derive(Debug, Default)]
pub(crate) struct TabStopMap {
    current_path: TabStopPath,
    pub(crate) insertion_history: Vec<TabStopOperation>,
    /// Position of each focus handle's most recent insertion in `nodes`.
    by_id: FxHashMap<FocusId, usize>,
    nodes: Vec<TabStopNode>,
    sorted: bool,
}

#[derive(Debug, Clone)]
pub enum TabStopOperation {
    Insert(FocusHandle),
    Group(TabIndex),
    GroupEnd,
}

impl TabStopOperation {
    fn focus_handle(&self) -> Option<&FocusHandle> {
        match self {
            TabStopOperation::Insert(focus_handle) => Some(focus_handle),
            _ => None,
        }
    }
}

type TabIndex = isize;

#[derive(Debug, Default, PartialEq, Eq, Clone, Ord, PartialOrd)]
struct TabStopPath(smallvec::SmallVec<[TabIndex; 6]>);

#[derive(Debug, Eq, PartialEq)]
struct TabStopNode {
    /// Ancestor groups' tab indices, followed by the focus handle's own tab index.
    path: TabStopPath,
    /// Index into `insertion_history`, also used to break ties in tab order.
    node_insertion_index: usize,

    /// Whether this node is a tab stop
    tab_stop: bool,
}

impl Ord for TabStopNode {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.path
            .cmp(&other.path)
            .then(self.node_insertion_index.cmp(&other.node_insertion_index))
    }
}

impl PartialOrd for TabStopNode {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(&other))
    }
}

impl TabStopMap {
    pub fn insert(&mut self, focus_handle: &FocusHandle, tab_stop_override: Option<bool>) {
        // Store the effective frame-local policy for cached subtree replay, without
        // mutating the owner's handle or the shared FocusMap.
        let mut registered_handle = focus_handle.clone();
        registered_handle.tab_stop = tab_stop_override.unwrap_or(focus_handle.tab_stop);
        let tab_stop = registered_handle.tab_stop;
        self.insertion_history
            .push(TabStopOperation::Insert(registered_handle));
        let mut path = self.current_path.clone();
        path.0.push(focus_handle.tab_index);
        self.by_id.insert(focus_handle.id, self.nodes.len());
        self.sorted = false;
        self.nodes.push(TabStopNode {
            node_insertion_index: self.insertion_history.len() - 1,
            tab_stop,
            path,
        });
    }

    pub fn begin_group(&mut self, tab_index: isize) {
        self.insertion_history
            .push(TabStopOperation::Group(tab_index));
        self.current_path.0.push(tab_index);
    }

    pub fn end_group(&mut self) {
        self.insertion_history.push(TabStopOperation::GroupEnd);
        self.current_path.0.pop();
    }

    pub fn clear(&mut self) {
        self.current_path.0.clear();
        self.insertion_history.clear();
        self.by_id.clear();
        self.nodes.clear();
        self.sorted = false;
    }

    pub fn next(&mut self, focused_id: Option<&FocusId>) -> Option<FocusHandle> {
        self.ensure_sorted();
        let start = focused_id
            .and_then(|id| self.by_id.get(id))
            .map_or(0, |position| position + 1);
        let node = self.nodes[start..]
            .iter()
            .chain(&self.nodes[..start])
            .find(|node| node.tab_stop)?;
        self.focus_handle_for_order(node)
    }

    pub fn prev(&mut self, focused_id: Option<&FocusId>) -> Option<FocusHandle> {
        self.ensure_sorted();
        let end = focused_id
            .and_then(|id| self.by_id.get(id).copied())
            .unwrap_or(self.nodes.len());
        let node = self.nodes[..end]
            .iter()
            .rev()
            .chain(self.nodes[end..].iter().rev())
            .find(|node| node.tab_stop)?;
        self.focus_handle_for_order(node)
    }

    fn ensure_sorted(&mut self) {
        if self.sorted {
            return;
        }
        // Unique insertion indices break ties, so unstable sorting needs no scratch allocation.
        self.nodes.sort_unstable();
        self.by_id.clear();
        for (position, node) in self.nodes.iter().enumerate() {
            let focus_handle = self.insertion_history[node.node_insertion_index]
                .focus_handle()
                .expect("tab stop node must correspond to an insertion");
            let latest = self.by_id.entry(focus_handle.id).or_insert(position);
            if self.nodes[*latest].node_insertion_index < node.node_insertion_index {
                *latest = position;
            }
        }
        self.sorted = true;
    }

    pub fn replay(&mut self, nodes: &[TabStopOperation]) {
        for node in nodes {
            match node {
                TabStopOperation::Insert(focus_handle) => self.insert(focus_handle, None),
                TabStopOperation::Group(tab_index) => self.begin_group(*tab_index),
                TabStopOperation::GroupEnd => self.end_group(),
            }
        }
    }

    pub fn paint_index(&self) -> usize {
        self.insertion_history.len()
    }

    pub(crate) fn tab_stop_count(&self) -> usize {
        self.by_id
            .values()
            .filter(|index| self.nodes[**index].tab_stop)
            .count()
    }

    fn focus_handle_for_order(&self, order: &TabStopNode) -> Option<FocusHandle> {
        let handle = self.insertion_history[order.node_insertion_index].focus_handle();
        debug_assert!(
            handle.is_some(),
            "The order node did not correspond to an element, this is a GPUI bug"
        );
        handle.cloned()
    }
}

#[cfg(test)]
mod tests {
    use itertools::Itertools as _;

    use crate::{FocusHandle, FocusId, FocusMap, TabStopMap};
    use std::sync::Arc;

    #[test]
    fn test_tab_handles() {
        let focus_map = Arc::new(FocusMap::default());
        let mut tab_index_map = TabStopMap::default();

        let focus_handles = [
            FocusHandle::new(&focus_map).tab_stop(true).tab_index(0),
            FocusHandle::new(&focus_map).tab_stop(true).tab_index(1),
            FocusHandle::new(&focus_map).tab_stop(true).tab_index(1),
            FocusHandle::new(&focus_map),
            FocusHandle::new(&focus_map).tab_index(2),
            FocusHandle::new(&focus_map).tab_stop(true).tab_index(0),
            FocusHandle::new(&focus_map).tab_stop(true).tab_index(2),
        ];

        for handle in focus_handles.iter() {
            tab_index_map.insert(handle, None);
        }
        let expected = [
            focus_handles[0].clone(),
            focus_handles[5].clone(),
            focus_handles[1].clone(),
            focus_handles[2].clone(),
            focus_handles[6].clone(),
        ];

        let mut prev = None;
        let mut found = vec![];
        for _ in 0..expected.len() {
            let handle = tab_index_map.next(prev.as_ref()).unwrap();
            prev = Some(handle.id);
            found.push(handle.id);
        }

        assert_eq!(
            found,
            expected.iter().map(|handle| handle.id).collect::<Vec<_>>()
        );

        // Select first tab index if no handle is currently focused.
        assert_eq!(tab_index_map.next(None), Some(expected[0].clone()));
        // Select last tab index if no handle is currently focused.
        assert_eq!(tab_index_map.prev(None), expected.last().cloned(),);

        assert_eq!(
            tab_index_map.next(Some(&expected[0].id)),
            Some(expected[1].clone())
        );
        assert_eq!(
            tab_index_map.next(Some(&expected[1].id)),
            Some(expected[2].clone())
        );
        assert_eq!(
            tab_index_map.next(Some(&expected[2].id)),
            Some(expected[3].clone())
        );
        assert_eq!(
            tab_index_map.next(Some(&expected[3].id)),
            Some(expected[4].clone())
        );
        assert_eq!(
            tab_index_map.next(Some(&expected[4].id)),
            Some(expected[0].clone())
        );

        // prev
        assert_eq!(tab_index_map.prev(None), Some(expected[4].clone()));
        assert_eq!(
            tab_index_map.prev(Some(&expected[0].id)),
            Some(expected[4].clone())
        );
        assert_eq!(
            tab_index_map.prev(Some(&expected[1].id)),
            Some(expected[0].clone())
        );
        assert_eq!(
            tab_index_map.prev(Some(&expected[2].id)),
            Some(expected[1].clone())
        );
        assert_eq!(
            tab_index_map.prev(Some(&expected[3].id)),
            Some(expected[2].clone())
        );
        assert_eq!(
            tab_index_map.prev(Some(&expected[4].id)),
            Some(expected[3].clone())
        );
    }

    #[test]
    fn test_tab_non_stop_filtering() {
        let focus_map = Arc::new(FocusMap::default());
        let mut tab_index_map = TabStopMap::default();

        // Check that we can query next from a non-stop tab
        let tab_non_stop_1 = FocusHandle::new(&focus_map).tab_stop(false).tab_index(1);
        let tab_stop_2 = FocusHandle::new(&focus_map).tab_stop(true).tab_index(2);
        tab_index_map.insert(&tab_non_stop_1, None);
        tab_index_map.insert(&tab_stop_2, None);
        let result = tab_index_map.next(Some(&tab_non_stop_1.id)).unwrap();
        assert_eq!(result.id, tab_stop_2.id);

        // Check that we skip over non-stop tabs
        let tab_stop_0 = FocusHandle::new(&focus_map).tab_stop(true).tab_index(0);
        let tab_non_stop_0 = FocusHandle::new(&focus_map).tab_stop(false).tab_index(0);
        tab_index_map.insert(&tab_stop_0, None);
        tab_index_map.insert(&tab_non_stop_0, None);
        let result = tab_index_map.next(Some(&tab_stop_0.id)).unwrap();
        assert_eq!(result.id, tab_stop_2.id);
    }

    #[test]
    fn test_tab_stop_override_is_frame_local_and_replayed() {
        let focus_map = Arc::new(FocusMap::default());
        for original_tab_stop in [true, false] {
            let before = FocusHandle::new(&focus_map).tab_stop(true).tab_index(0);
            let item = FocusHandle::new(&focus_map)
                .tab_stop(original_tab_stop)
                .tab_index(1);
            let after = FocusHandle::new(&focus_map).tab_stop(true).tab_index(2);
            for enabled in [false, true, false] {
                let mut frame = TabStopMap::default();
                frame.insert(&before, None);
                frame.insert(&item, (!enabled).then_some(false));
                frame.insert(&after, None);
                let expected = if enabled && original_tab_stop {
                    item.id
                } else {
                    after.id
                };
                assert_eq!(frame.next(Some(&before.id)).unwrap().id, expected);
                let mut replayed = TabStopMap::default();
                replayed.replay(&frame.insertion_history);
                assert_eq!(replayed.next(Some(&before.id)).unwrap().id, expected);
                assert_eq!(item.tab_stop, original_tab_stop);
                assert_eq!(
                    item.downgrade().upgrade().unwrap().tab_stop,
                    original_tab_stop
                );
            }
        }
    }

    #[must_use]
    struct TabStopMapTest {
        tab_map: TabStopMap,
        focus_map: Arc<FocusMap>,
        expected: Vec<(usize, FocusId)>,
    }

    impl TabStopMapTest {
        #[must_use]
        fn new() -> Self {
            Self {
                tab_map: TabStopMap::default(),
                focus_map: Arc::new(FocusMap::default()),
                expected: Vec::default(),
            }
        }

        #[must_use]
        fn tab_non_stop(mut self, index: isize) -> Self {
            let handle = FocusHandle::new(&self.focus_map)
                .tab_stop(false)
                .tab_index(index);
            self.tab_map.insert(&handle, None);
            self
        }

        #[must_use]
        fn tab_stop(mut self, index: isize, expected: usize) -> Self {
            let handle = FocusHandle::new(&self.focus_map)
                .tab_stop(true)
                .tab_index(index);
            self.tab_map.insert(&handle, None);
            self.expected.push((expected, handle.id));
            self.expected.sort_by_key(|(expected, _)| *expected);
            self
        }

        #[must_use]
        fn tab_group(mut self, tab_index: isize, children: impl FnOnce(Self) -> Self) -> Self {
            self.tab_map.begin_group(tab_index);
            self = children(self);
            self.tab_map.end_group();
            self
        }

        fn traverse_tab_map(
            &mut self,
            traverse: impl Fn(&mut TabStopMap, Option<&FocusId>) -> Option<FocusHandle>,
        ) -> Vec<FocusId> {
            let mut last_focus_id = None;
            let mut found = vec![];
            for _ in 0..self.expected.len() {
                let handle = traverse(&mut self.tab_map, last_focus_id.as_ref()).unwrap();
                last_focus_id = Some(handle.id);
                found.push(handle.id);
            }
            found
        }

        fn assert(mut self) {
            let mut expected = self.expected.iter().map(|(_, id)| *id).collect_vec();

            // Check next order
            let forward_found = self.traverse_tab_map(|tab_map, prev| tab_map.next(prev));
            assert_eq!(forward_found, expected);

            // Test overflow. Last to first
            assert_eq!(
                self.tab_map
                    .next(forward_found.last())
                    .map(|handle| handle.id),
                expected.first().cloned()
            );

            // Check previous order
            let reversed_found = self.traverse_tab_map(|tab_map, prev| tab_map.prev(prev));
            expected.reverse();
            assert_eq!(reversed_found, expected);

            // Test overflow. First to last
            assert_eq!(
                self.tab_map
                    .prev(reversed_found.last())
                    .map(|handle| handle.id),
                expected.first().cloned(),
            );
        }
    }

    #[test]
    fn test_insert_after_sorting() {
        let focus_map = Arc::new(FocusMap::default());
        let mut tab_index_map = TabStopMap::default();
        let second = FocusHandle::new(&focus_map).tab_stop(true).tab_index(1);
        let first = FocusHandle::new(&focus_map).tab_stop(true).tab_index(0);

        tab_index_map.insert(&second, None);
        assert_eq!(tab_index_map.next(None), Some(second.clone()));

        tab_index_map.insert(&first, None);
        assert_eq!(tab_index_map.next(None), Some(first.clone()));
        assert_eq!(tab_index_map.next(Some(&first.id)), Some(second.clone()));
        assert_eq!(tab_index_map.prev(Some(&first.id)), Some(second));
    }

    #[test]
    fn test_latest_duplicate_insertion_survives_sorting_and_replay() {
        let focus_map = Arc::new(FocusMap::default());
        let item = FocusHandle::new(&focus_map).tab_stop(true).tab_index(0);
        let before = FocusHandle::new(&focus_map).tab_stop(true).tab_index(1);
        let after = FocusHandle::new(&focus_map).tab_stop(true).tab_index(3);
        let mut frame = TabStopMap::default();
        // The latest insertion sorts before the original, so sorted position cannot
        // be used as a proxy for insertion order when rebuilding the ID index.
        frame.begin_group(2);
        frame.insert(&item, None);
        frame.end_group();
        frame.insert(&before, None);
        frame.insert(&after, None);
        frame.insert(&item, Some(false));
        assert_eq!(frame.tab_stop_count(), 2);
        assert_eq!(frame.next(Some(&item.id)), Some(before.clone()));
        assert_eq!(frame.prev(Some(&item.id)), Some(after.clone()));
        assert_eq!(frame.tab_stop_count(), 2);
        assert!(item.tab_stop);

        let mut replayed = TabStopMap::default();
        replayed.replay(&frame.insertion_history);
        assert_eq!(replayed.tab_stop_count(), 2);
        assert_eq!(replayed.next(Some(&item.id)), Some(before));
        assert_eq!(replayed.prev(Some(&item.id)), Some(after));
        assert_eq!(replayed.tab_stop_count(), 2);
    }

    #[test]
    fn test_clear_resets_traversal() {
        let focus_map = Arc::new(FocusMap::default());
        let item = FocusHandle::new(&focus_map).tab_stop(true);
        let mut frame = TabStopMap::default();
        frame.begin_group(7);
        frame.insert(&item, None);
        assert_eq!(frame.next(None), Some(item.clone()));
        frame.clear();
        assert_eq!(frame.paint_index(), 0);
        assert_eq!(frame.tab_stop_count(), 0);
        assert_eq!(frame.next(None), None);
        assert_eq!(frame.prev(Some(&item.id)), None);
        frame.insert(&item, Some(false));
        assert_eq!(frame.next(None), None);
        assert_eq!(frame.prev(None), None);
        frame.clear();
        frame.insert(&item, None);
        assert!(frame.current_path.0.is_empty());
        assert_eq!(frame.next(Some(&item.id)), Some(item.clone()));
        assert_eq!(frame.prev(Some(&item.id)), Some(item));
    }

    #[test]
    fn test_with_disabled_tab_stop() {
        TabStopMapTest::new()
            .tab_stop(0, 0)
            .tab_non_stop(1)
            .tab_stop(2, 1)
            .tab_stop(3, 2)
            .assert();
    }

    #[test]
    fn test_with_multiple_disabled_tab_stops() {
        TabStopMapTest::new()
            .tab_non_stop(0)
            .tab_stop(1, 0)
            .tab_non_stop(3)
            .tab_stop(3, 1)
            .tab_non_stop(4)
            .assert();
    }

    #[test]
    fn test_tab_group_functionality() {
        TabStopMapTest::new()
            .tab_stop(0, 0)
            .tab_stop(0, 1)
            .tab_group(2, |t| t.tab_stop(0, 2).tab_stop(1, 3))
            .tab_stop(3, 4)
            .tab_stop(4, 5)
            .assert()
    }

    #[test]
    fn test_sibling_groups() {
        TabStopMapTest::new()
            .tab_stop(0, 0)
            .tab_stop(1, 1)
            .tab_group(2, |test| test.tab_stop(0, 2).tab_stop(1, 3))
            .tab_stop(3, 4)
            .tab_stop(4, 5)
            .tab_group(6, |test| test.tab_stop(0, 6).tab_stop(1, 7))
            .tab_stop(7, 8)
            .tab_stop(8, 9)
            .assert();
    }

    #[test]
    fn test_nested_group() {
        TabStopMapTest::new()
            .tab_stop(0, 0)
            .tab_stop(1, 1)
            .tab_group(2, |t| {
                t.tab_group(0, |t| t.tab_stop(0, 2).tab_stop(1, 3))
                    .tab_stop(1, 4)
            })
            .tab_stop(3, 5)
            .tab_stop(4, 6)
            .assert();
    }

    #[test]
    fn test_sibling_nested_groups() {
        TabStopMapTest::new()
            .tab_stop(0, 0)
            .tab_stop(1, 1)
            .tab_group(2, |builder| {
                builder
                    .tab_stop(0, 2)
                    .tab_stop(2, 5)
                    .tab_group(1, |builder| builder.tab_stop(0, 3).tab_stop(1, 4))
                    .tab_group(3, |builder| builder.tab_stop(0, 6).tab_stop(1, 7))
            })
            .tab_stop(3, 8)
            .tab_stop(4, 9)
            .assert();
    }

    #[test]
    fn test_sibling_nested_groups_out_of_order() {
        TabStopMapTest::new()
            .tab_stop(9, 9)
            .tab_stop(8, 8)
            .tab_group(7, |builder| {
                builder
                    .tab_stop(0, 2)
                    .tab_stop(2, 5)
                    .tab_group(3, |builder| builder.tab_stop(1, 7).tab_stop(0, 6))
                    .tab_group(1, |builder| builder.tab_stop(0, 3).tab_stop(1, 4))
            })
            .tab_stop(3, 0)
            .tab_stop(4, 1)
            .assert();
    }
}
