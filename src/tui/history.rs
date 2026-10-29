use std::collections::{VecDeque, vec_deque};

use crate::model::Snapshot;

#[derive(Debug)]
pub(super) struct SnapshotHistory {
    capacity: usize,
    snapshots: VecDeque<Snapshot>,
}

impl SnapshotHistory {
    pub(super) fn new(capacity: usize) -> Self {
        Self {
            capacity,
            snapshots: VecDeque::new(),
        }
    }

    pub(super) fn push(&mut self, snapshot: Snapshot) {
        if let Some(latest) = self.latest() {
            if latest.sequence == snapshot.sequence {
                return;
            }
            if latest.sequence > snapshot.sequence {
                self.snapshots.clear();
            }
        }
        self.snapshots.push_back(snapshot);
        while self.snapshots.len() > self.capacity {
            self.snapshots.pop_front();
        }
    }

    pub(super) fn latest(&self) -> Option<&Snapshot> {
        self.snapshots.back()
    }

    pub(super) fn iter(&self) -> vec_deque::Iter<'_, Snapshot> {
        self.snapshots.iter()
    }

    pub(super) fn len(&self) -> usize {
        self.snapshots.len()
    }

    pub(super) fn capacity(&self) -> usize {
        self.capacity
    }
}

#[cfg(test)]
mod tests {
    use chrono::Utc;

    use super::*;

    #[test]
    fn test_retains_only_configured_snapshot_capacity() {
        // Arrange
        let mut history = SnapshotHistory::new(2);

        // Act
        history.push(snapshot(1));
        history.push(snapshot(2));
        history.push(snapshot(3));

        // Assert
        assert_eq!(history.len(), 2);
        assert_eq!(history.iter().next().map(|item| item.sequence), Some(2));
    }

    #[test]
    fn test_ignores_duplicate_snapshot_sequence() {
        // Arrange
        let mut history = SnapshotHistory::new(2);
        history.push(snapshot(1));

        // Act
        history.push(snapshot(1));

        // Assert
        assert_eq!(history.len(), 1);
    }

    #[test]
    fn test_resets_history_when_sequence_moves_backwards() {
        // Arrange
        let mut history = SnapshotHistory::new(3);
        history.push(snapshot(8));
        history.push(snapshot(9));

        // Act
        history.push(snapshot(1));

        // Assert
        assert_eq!(history.len(), 1);
        assert_eq!(history.latest().map(|item| item.sequence), Some(1));
    }

    fn snapshot(sequence: u64) -> Snapshot {
        Snapshot {
            schema_version: 1,
            sequence,
            generated_at: Utc::now(),
            observations: Vec::new(),
        }
    }
}
