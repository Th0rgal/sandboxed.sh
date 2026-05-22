use serde::Serialize;
use std::collections::{HashMap, HashSet, VecDeque};
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct QueueKey {
    pub channel: String,
    pub session_id: String,
    pub mission_id: Option<Uuid>,
    pub priority: u8,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct QueueMetrics {
    pub pending_items: usize,
    pub active_sessions: usize,
}

#[derive(Debug, Default)]
pub struct PalomaQueue<T> {
    queues: HashMap<QueueKey, VecDeque<T>>,
    max_per_key: usize,
}

impl<T> PalomaQueue<T> {
    pub fn new(max_per_key: usize) -> Self {
        Self {
            queues: HashMap::new(),
            max_per_key,
        }
    }

    pub fn push(&mut self, key: QueueKey, item: T) -> Result<(), T> {
        let queue = self.queues.entry(key).or_default();
        if queue.len() >= self.max_per_key {
            return Err(item);
        }
        queue.push_back(item);
        Ok(())
    }

    pub fn pop(&mut self, key: &QueueKey) -> Option<T> {
        let item = self.queues.get_mut(key)?.pop_front();
        if self.queues.get(key).is_some_and(VecDeque::is_empty) {
            self.queues.remove(key);
        }
        item
    }

    pub fn cleanup_idle(&mut self) {
        self.queues.retain(|_, queue| !queue.is_empty());
    }

    pub fn metrics(&self) -> QueueMetrics {
        QueueMetrics {
            pending_items: self.queues.values().map(VecDeque::len).sum(),
            active_sessions: self.queues.len(),
        }
    }

    pub fn metrics_for_channels(&self, channel_ids: &HashSet<String>) -> QueueMetrics {
        let mut pending_items = 0;
        let mut active_sessions = 0;
        for (key, queue) in &self.queues {
            if channel_ids.contains(&key.channel) {
                pending_items += queue.len();
                active_sessions += 1;
            }
        }
        QueueMetrics {
            pending_items,
            active_sessions,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serializes_per_key_without_merging_sessions() {
        let mut queue = PalomaQueue::new(8);
        let key_a = QueueKey {
            channel: "telegram".to_string(),
            session_id: "chat-a".to_string(),
            mission_id: None,
            priority: 5,
        };
        let key_b = QueueKey {
            session_id: "chat-b".to_string(),
            ..key_a.clone()
        };
        queue.push(key_a.clone(), 1).unwrap();
        queue.push(key_a.clone(), 2).unwrap();
        queue.push(key_b.clone(), 9).unwrap();
        assert_eq!(queue.pop(&key_a), Some(1));
        assert_eq!(queue.pop(&key_b), Some(9));
        assert_eq!(queue.pop(&key_a), Some(2));
    }

    #[test]
    fn bounds_queue_per_key_and_cleans_idle_sessions() {
        let mut queue = PalomaQueue::new(1);
        let key = QueueKey {
            channel: "telegram".to_string(),
            session_id: "chat-a".to_string(),
            mission_id: None,
            priority: 5,
        };

        assert!(queue.push(key.clone(), "first").is_ok());
        assert_eq!(queue.push(key.clone(), "overflow"), Err("overflow"));
        assert_eq!(
            queue.metrics(),
            QueueMetrics {
                pending_items: 1,
                active_sessions: 1,
            }
        );

        assert_eq!(queue.pop(&key), Some("first"));
        queue.cleanup_idle();
        assert_eq!(
            queue.metrics(),
            QueueMetrics {
                pending_items: 0,
                active_sessions: 0,
            }
        );
    }

    #[test]
    fn metrics_can_be_scoped_to_known_channels() {
        let mut queue = PalomaQueue::new(8);
        let channel_a = Uuid::new_v4().to_string();
        let channel_b = Uuid::new_v4().to_string();
        queue
            .push(
                QueueKey {
                    channel: channel_a.clone(),
                    session_id: "chat-a".to_string(),
                    mission_id: None,
                    priority: 0,
                },
                1,
            )
            .unwrap();
        queue
            .push(
                QueueKey {
                    channel: channel_b,
                    session_id: "chat-b".to_string(),
                    mission_id: None,
                    priority: 0,
                },
                2,
            )
            .unwrap();

        let channel_ids = HashSet::from([channel_a]);
        assert_eq!(
            queue.metrics_for_channels(&channel_ids),
            QueueMetrics {
                pending_items: 1,
                active_sessions: 1,
            }
        );
    }
}
