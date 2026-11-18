//! Task driver for persistence workflow tests.

use crate::Message;

pub(crate) fn drain_task(task: iced::Task<Message>) -> Vec<Message> {
    use std::task::{Context, Poll};

    let Some(stream) = iced_test::runtime::task::into_stream(task) else {
        return Vec::new();
    };
    let mut stream = Box::pin(stream);
    let waker = std::task::Waker::noop();
    let mut context = Context::from_waker(waker);
    let mut messages = Vec::new();
    let mut spins = 0u32;
    // Saves settle on the save worker, so the drain waits by deadline
    // rather than assuming the work ran inside the poll.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        match iced_test::futures::futures::Stream::poll_next(stream.as_mut(), &mut context) {
            Poll::Ready(Some(iced_test::runtime::Action::Output(message))) => {
                messages.push(message);
            }
            Poll::Ready(Some(_)) => {}
            Poll::Ready(None) => break,
            Poll::Pending => {
                spins += 1;
                if std::time::Instant::now() >= deadline {
                    break;
                }
                if spins > 1_000 {
                    std::thread::sleep(std::time::Duration::from_millis(1));
                } else {
                    std::thread::yield_now();
                }
            }
        }
    }
    messages
}
