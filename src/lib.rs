use std::{
    collections::VecDeque,
    sync::{Arc, Condvar, Mutex},
};

/// An async sender that does not block if the channel queue is full.
pub struct Sender<T> {
    shared: Arc<SharedState<T>>,
}

impl<T> Clone for Sender<T> {
    fn clone(&self) -> Self {
        let mut inner = self.shared.inner.lock().unwrap();
        inner.senders += 1;
        drop(inner);

        Sender {
            shared: Arc::clone(&self.shared),
        }
    }
}

impl<T> Drop for Sender<T> {
    fn drop(&mut self) {
        let mut inner = self.shared.inner.lock().unwrap();
        inner.senders -= 1;
        let was_last = inner.senders == 0;
        drop(inner);
        // no more senders - we should wake up any receiver that's possibly waiting
        if was_last {
            self.shared.available.notify_one();
        }
    }
}
impl<T> Sender<T> {
    fn send(&self, value: T) {
        let mut inner = self.shared.inner.lock().unwrap();
        inner.queue.push_back(value);

        // give up the mutex guard
        // notify one of the sleeping receiver threads to wake up
        drop(inner);
        self.shared.available.notify_one();
    }
}

pub struct Receiver<T> {
    shared: Arc<SharedState<T>>,
    buffer: VecDeque<T>,
}

impl<T> Receiver<T> {
    // If there isn't anything in the channel, block and wait.
    fn recv(&mut self) -> Option<T> {
        // no need to acquire the lock if there are still items in the buffer
        if let Some(t) = self.buffer.pop_front() {
            return Some(t);
        }
        // We `unwrap` here because the mutex guard could have been "poisoned"
        // if the previous holder of the lock panicked. We ignore that here.
        let mut inner = self.shared.inner.lock().unwrap();
        // This is not a spin loop
        loop {
            match inner.queue.pop_front() {
                Some(t) => {
                    // since we have the lock right now, take as many items as you can from the channel queue
                    // use `std::mem::swap` instead of `std::mem::take` so we can reuse the 2 existing buffers instead of allocating a new one every time.
                    if !inner.queue.is_empty() {
                        std::mem::swap(&mut self.buffer, &mut inner.queue);
                    }
                    return Some(t);
                }
                None if inner.senders == 0 => return None,
                None => {
                    // A sender will notify this thread to wake up with the queue's mutex guard.
                    // `Condvar::wait` gives up the lock just before the thread goes to sleep.
                    // It is also susceptible to spurious wakeups (i.e. the OS wakes up the thread for some reason).
                    // This is why we wrap this whole thing in a loop.
                    inner = self.shared.available.wait(inner).unwrap();
                }
            }
        }
    }
}

struct SharedState<T> {
    inner: Mutex<Inner<T>>,
    available: Condvar,
}

/// We back the channel with a `VecDeque`, since it helps us avoid shifting content with every pop.
/// Alternatively, we could also use a linked list, so we could shift pointers and avoid array resizing.
/// If we wanted to avoid locking entirely, we could use an atomic blocked linked list.
///     This is essentially a linked list of atomic `VecDeque<T>`
///     This would lower the amount of times we need to atomically update the head/tail of the linked list
struct Inner<T> {
    queue: VecDeque<T>,
    senders: usize,
}

/// A async multi-producer, single consmer channel.
/// It is unbounded, so the internal queue can grow until we run out of memory.
pub fn channel<T>() -> (Sender<T>, Receiver<T>) {
    let inner = Inner {
        queue: VecDeque::default(),
        senders: 1,
    };
    let shared = SharedState {
        inner: Mutex::new(inner),
        available: Condvar::new(),
    };
    let shared = Arc::new(shared);
    (
        Sender {
            shared: shared.clone(),
        },
        Receiver {
            shared: shared.clone(),
            buffer: VecDeque::default(),
        },
    )
}

#[cfg(test)]
mod tests {
    use crate::channel;

    #[test]
    fn ping_pong() {
        let (tx, mut rx) = channel();
        tx.send(5);
        assert_eq!(Some(5), rx.recv());
    }

    #[test]
    fn no_senders() {
        let (tx, mut rx) = channel::<()>();
        drop(tx);
        assert_eq!(None, rx.recv());
    }
}
