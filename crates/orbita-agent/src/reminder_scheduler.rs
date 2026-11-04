//! Reminder delivery loop.

use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use tracing::{error, info};

use crate::db::Store;
use crate::notify::Notifier;

const BATCH: usize = 20;
const MAX_ATTEMPTS: u32 = 3;
const RETRY_DELAY_SECONDS: i64 = 30;

pub async fn run(store: Arc<Store>, notifier: Arc<dyn Notifier>, interval: Duration) {
    loop {
        if let Err(error) = tick(&store, notifier.as_ref(), Utc::now()).await {
            error!(%error, "scheduler tick failed");
        }
        tokio::time::sleep(interval).await;
    }
}

/// Deliver all currently due reminders. Public for tests with a fake notifier.
pub async fn tick(
    store: &Store,
    notifier: &dyn Notifier,
    now: DateTime<Utc>,
) -> Result<usize, String> {
    let due = store.due_tasks(now, BATCH)?;
    let mut delivered = 0;
    for task in due {
        if !store.claim_task(&task.id)? {
            continue;
        }
        // Native timers fire on the station itself; the SQLite row is a mirror
        // for audit and cancellation. It must never be pushed as a reminder.
        if task.kind == "native_timer" {
            store.finish_task(&task.id, "native")?;
            store.audit("task.native_done", &format!("task={}", task.id))?;
            continue;
        }
        let Some(device_id) = task.device_id.as_deref() else {
            store.finish_task(&task.id, "failed")?;
            store.audit(
                "reminder.failed",
                &format!("task={} reason=no_device", task.id),
            )?;
            continue;
        };
        match notifier.push(device_id, &task.text).await {
            Ok(()) => {
                store.finish_task(&task.id, "sent")?;
                store.audit("reminder.sent", &format!("task={}", task.id))?;
                delivered += 1;
                info!(task = %task.id, %device_id, "reminder delivered");
            }
            Err(error) => {
                let attempts = task.attempts + 1;
                if attempts < MAX_ATTEMPTS {
                    store.retry_task(
                        &task.id,
                        now + chrono::Duration::seconds(RETRY_DELAY_SECONDS),
                        attempts,
                    )?;
                } else {
                    store.finish_task(&task.id, "failed")?;
                }
                store.audit(
                    "reminder.failed",
                    &format!("task={} attempts={attempts} error={error}", task.id),
                )?;
            }
        }
    }
    Ok(delivered)
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
    };

    use async_trait::async_trait;
    use chrono::Duration as ChronoDuration;

    use super::*;

    #[derive(Default)]
    struct FakeNotifier {
        pushed: Mutex<Vec<(String, String)>>,
        fail: AtomicBool,
    }

    #[async_trait]
    impl Notifier for FakeNotifier {
        async fn push(&self, device_id: &str, text: &str) -> Result<(), String> {
            if self.fail.load(Ordering::SeqCst) {
                return Err("offline".to_owned());
            }
            self.pushed
                .lock()
                .unwrap()
                .push((device_id.to_owned(), text.to_owned()));
            Ok(())
        }
    }

    fn now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-11-03T09:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    #[tokio::test]
    async fn native_timer_mirror_is_not_pushed() {
        let store = Store::open(":memory:").unwrap();
        let notifier = FakeNotifier::default();
        let id = store
            .insert_task(
                "native_timer",
                "таймер на 2 минуты",
                Some("duid"),
                now() - ChronoDuration::seconds(1),
            )
            .unwrap();
        assert_eq!(tick(&store, &notifier, now()).await.unwrap(), 0);
        assert!(notifier.pushed.lock().unwrap().is_empty());
        assert!(store.due_tasks(now(), 10).unwrap().is_empty());
        let _ = id;
    }

    #[tokio::test]
    async fn due_reminder_is_delivered() {
        let store = Store::open(":memory:").unwrap();
        let notifier = FakeNotifier::default();
        let id = store
            .insert_task(
                "reminder",
                "чай",
                Some("duid"),
                now() - ChronoDuration::seconds(1),
            )
            .unwrap();
        assert_eq!(tick(&store, &notifier, now()).await.unwrap(), 1);
        assert_eq!(
            notifier.pushed.lock().unwrap().as_slice(),
            &[("duid".to_owned(), "чай".to_owned())]
        );
        assert!(store.due_tasks(now(), 10).unwrap().is_empty());
        assert!(!store.claim_task(&id).unwrap());
    }

    #[tokio::test]
    async fn future_reminder_is_not_delivered() {
        let store = Store::open(":memory:").unwrap();
        let notifier = FakeNotifier::default();
        store
            .insert_task(
                "reminder",
                "чай",
                Some("duid"),
                now() + ChronoDuration::minutes(5),
            )
            .unwrap();
        assert_eq!(tick(&store, &notifier, now()).await.unwrap(), 0);
        assert!(notifier.pushed.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn failure_is_retried_then_marked_failed() {
        let store = Store::open(":memory:").unwrap();
        let notifier = FakeNotifier::default();
        notifier.fail.store(true, Ordering::SeqCst);
        let id = store
            .insert_task(
                "reminder",
                "чай",
                Some("duid"),
                now() - ChronoDuration::seconds(1),
            )
            .unwrap();

        assert_eq!(tick(&store, &notifier, now()).await.unwrap(), 0);
        assert_eq!(store.due_tasks(now(), 10).unwrap().len(), 0);
        assert_eq!(
            store
                .due_tasks(now() + ChronoDuration::seconds(31), 10)
                .unwrap()
                .len(),
            1
        );

        // Third attempt (attempts already 1 from the retry above).
        let later = now() + ChronoDuration::seconds(31);
        tick(&store, &notifier, later).await.unwrap();
        let later = later + ChronoDuration::seconds(31);
        tick(&store, &notifier, later).await.unwrap();
        assert!(
            store
                .due_tasks(later + ChronoDuration::minutes(5), 10)
                .unwrap()
                .is_empty()
        );
        let _ = id;
    }
}
