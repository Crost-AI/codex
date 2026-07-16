use pretty_assertions::assert_eq;

use super::BoundedEventQueue;

#[test]
fn drains_in_fifo_order() {
    let mut queue = BoundedEventQueue::new(4);
    assert_eq!(queue.push("a".to_string()), None);
    assert_eq!(queue.push("b".to_string()), None);
    assert_eq!(queue.drain_all(), vec!["a".to_string(), "b".to_string()]);
    assert!(queue.is_empty());
}

#[test]
fn drops_oldest_when_full() {
    let mut queue = BoundedEventQueue::new(2);
    assert_eq!(queue.push("a".to_string()), None);
    assert_eq!(queue.push("b".to_string()), None);
    assert_eq!(queue.push("c".to_string()), Some("a".to_string()));
    assert_eq!(queue.drain_all(), vec!["b".to_string(), "c".to_string()]);
}

#[test]
fn capacity_has_a_floor_of_one() {
    let mut queue = BoundedEventQueue::new(0);
    assert_eq!(queue.push("a".to_string()), None);
    assert_eq!(queue.push("b".to_string()), Some("a".to_string()));
    assert_eq!(queue.drain_all(), vec!["b".to_string()]);
}
