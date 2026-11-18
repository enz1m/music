use enzui_core::blocking_task::{BlockingShutdown, BlockingTask, BlockingTaskOwner};
use enzui_services::media::mpris::zbus_transport::{
    MPRIS_CALL_QUEUE_CAPACITY, MprisCallWakeReceiver, MprisConnectCancel, MprisShutdown,
    MprisShutdownOutcome, MprisTransportState, ZbusMprisConnection,
};
use enzui_services::media::{
    MprisAdapter, MprisCall, MprisConnection, MprisProperties, MprisRefusal,
};
use iced::Subscription;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Waker};

const APP_IDENTITY: &str = "Music";
const CONNECT_GENERATION: u64 = 1;

#[derive(Debug)]
enum ConnectResult {
    Connected {
        connection: Box<ZbusMprisConnection>,
        receiver: MprisCallWakeReceiver,
    },
    Unavailable(String),
}

#[derive(Clone)]
pub struct ConnectDelivery {
    slot: Arc<Mutex<Option<ConnectResult>>>,
    started: Arc<AtomicBool>,
}

impl std::fmt::Debug for ConnectDelivery {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ConnectDelivery")
            .finish_non_exhaustive()
    }
}

impl ConnectDelivery {
    fn new() -> Self {
        Self {
            slot: Arc::new(Mutex::new(None)),
            started: Arc::new(AtomicBool::new(false)),
        }
    }

    fn store(&self, result: ConnectResult) {
        *self
            .slot
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(result);
    }

    fn take(&self) -> Option<ConnectResult> {
        self.slot
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CloseReadiness {
    Pending,
    Ready,
    Vetoed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Availability {
    Inert,
    Connecting,
    Live,
    Unavailable,
    Releasing,
    Released,
    Vetoed,
}

enum Transport {
    Zbus(Box<ZbusMprisConnection>),
}

impl Transport {
    fn recv_call(&self) -> Option<MprisCall> {
        match self {
            Self::Zbus(connection) => connection.recv_call(),
        }
    }

    fn publish(&mut self, properties: &MprisProperties) {
        match self {
            Self::Zbus(connection) => connection.publish(properties),
        }
    }

    fn refuse(&mut self, call: &MprisCall, refusal: MprisRefusal) {
        match self {
            Self::Zbus(connection) => connection.refuse(call, refusal),
        }
    }
}

pub(crate) struct Runtime {
    adapter: MprisAdapter,
    availability: Availability,
    notice: Option<String>,
    connect_owner: Option<BlockingTaskOwner>,
    connect_cancel: Option<MprisConnectCancel>,
    connect_delivery: Option<ConnectDelivery>,
    transport: Option<Transport>,
    receiver: Option<MprisCallWakeReceiver>,
    shutdown: Option<MprisShutdown>,
    shutdown_outcome: Option<MprisShutdownOutcome>,
    last_published: Option<MprisProperties>,
    closing: bool,
    owner_failed: bool,
    #[cfg(test)]
    test_wake_generation: Option<u64>,
}

impl Runtime {
    pub(crate) fn inert() -> Self {
        Self {
            adapter: MprisAdapter::new(APP_IDENTITY),
            availability: Availability::Inert,
            notice: None,
            connect_owner: None,
            connect_cancel: None,
            connect_delivery: None,
            transport: None,
            receiver: None,
            shutdown: None,
            shutdown_outcome: None,
            last_published: None,
            closing: false,
            owner_failed: false,
            #[cfg(test)]
            test_wake_generation: None,
        }
    }

    pub(crate) fn start(&mut self) -> Option<(BlockingTask<()>, ConnectDelivery)> {
        if self.availability != Availability::Inert {
            return None;
        }
        let worker_count = std::num::NonZeroUsize::new(1).expect("nonzero MPRIS worker count");
        let queue_capacity = std::num::NonZeroUsize::new(1).expect("nonzero MPRIS queue capacity");
        let owner =
            match BlockingTaskOwner::new("music-mpris-connect", worker_count, queue_capacity) {
                Ok(owner) => owner,
                Err(error) => {
                    self.set_unavailable("Desktop media controls are unavailable.");
                    eprintln!("music MPRIS unavailable: connect owner: {error}");
                    return None;
                }
            };
        Some(self.start_on(owner))
    }

    fn start_on(&mut self, owner: BlockingTaskOwner) -> (BlockingTask<()>, ConnectDelivery) {
        let cancel = MprisConnectCancel::new();
        let worker_cancel = cancel.clone();
        let delivery = ConnectDelivery::new();
        let worker_delivery = delivery.clone();
        let task = owner.submit(CONNECT_GENERATION, move |context| {
            worker_delivery.started.store(true, Ordering::Release);
            if context.is_cancelled() {
                worker_cancel.cancel();
            }
            let result =
                match ZbusMprisConnection::connect_cancellable(APP_IDENTITY, &worker_cancel) {
                    Ok(mut connection) => match connection.take_call_wake_receiver() {
                        Some(receiver) => ConnectResult::Connected {
                            connection: Box::new(connection),
                            receiver,
                        },
                        None => ConnectResult::Unavailable(
                            "the MPRIS call receiver was already taken".to_string(),
                        ),
                    },
                    Err(error) => ConnectResult::Unavailable(error.to_string()),
                };
            worker_delivery.store(result);
        });
        self.availability = Availability::Connecting;
        self.connect_owner = Some(owner);
        self.connect_cancel = Some(cancel);
        self.connect_delivery = Some(delivery.clone());
        (task, delivery)
    }

    pub(crate) fn finish_connect(&mut self, delivery: &ConnectDelivery) {
        let Some(current) = self.connect_delivery.as_ref() else {
            return;
        };
        if !Arc::ptr_eq(&current.slot, &delivery.slot) {
            return;
        }
        let Some(result) = delivery.take() else {
            if !self.closing {
                self.owner_failed = true;
                self.set_unavailable("Desktop media controls are unavailable.");
                eprintln!("music MPRIS unavailable: connect result lost");
            }
            return;
        };
        self.connect_delivery = None;
        self.connect_cancel = None;
        if let Some(owner) = self.connect_owner.as_mut() {
            owner.shutdown();
        }
        match result {
            ConnectResult::Connected {
                connection,
                receiver,
            } => {
                self.transport = Some(Transport::Zbus(connection));
                self.receiver = Some(receiver);
                self.availability = Availability::Live;
                self.notice = None;
                if self.closing {
                    self.begin_release();
                }
            }
            ConnectResult::Unavailable(diagnostic) => {
                self.set_unavailable("Desktop media controls are unavailable.");
                eprintln!("music MPRIS unavailable: {}", escaped_bounded(&diagnostic));
            }
        }
        self.try_join_connect_owner();
    }

    pub(crate) fn translate(
        &self,
        call: &MprisCall,
        snapshot: &enzui_services::media::Snapshot,
    ) -> Result<enzui_services::media::MediaCommand, MprisRefusal> {
        self.adapter.translate(call, snapshot)
    }

    pub(crate) fn drain_calls(&self) -> Vec<MprisCall> {
        if self.availability != Availability::Live || self.closing {
            return Vec::new();
        }
        let Some(transport) = self.transport.as_ref() else {
            return Vec::new();
        };
        std::iter::from_fn(|| transport.recv_call())
            .take(MPRIS_CALL_QUEUE_CAPACITY)
            .collect()
    }

    pub(crate) fn refuse(&mut self, call: &MprisCall, refusal: MprisRefusal) {
        if let Some(transport) = self.transport.as_mut() {
            transport.refuse(call, refusal);
        }
    }

    pub(crate) fn publish(&mut self, properties: MprisProperties) {
        if self.availability != Availability::Live
            || self.closing
            || self.last_published.as_ref() == Some(&properties)
        {
            return;
        }
        let Some(transport) = self.transport.as_mut() else {
            return;
        };
        transport.publish(&properties);
        self.last_published = Some(properties);
    }

    pub(crate) fn call_subscription(
        &self,
    ) -> Option<Subscription<enzui_services::media::mpris::zbus_transport::MprisCallWake>> {
        if self.availability != Availability::Live {
            return None;
        }
        if let Some(receiver) = self.receiver.as_ref() {
            return Some(receiver.subscription());
        }
        #[cfg(test)]
        if let Some(generation) = self.test_wake_generation {
            return Some(Subscription::run_with(
                ("music-mpris-test-wake", generation),
                |&(_, generation)| {
                    iced::futures::stream::once(async move {
                        enzui_services::media::mpris::zbus_transport::MprisCallWake {
                            generation,
                            state: MprisTransportState::Live,
                        }
                    })
                },
            ));
        }
        None
    }

    pub(crate) fn notice(&self) -> Option<&str> {
        self.notice.as_deref()
    }

    pub(crate) fn transport_ended(&mut self, state: MprisTransportState) {
        if state == MprisTransportState::Lost {
            self.notice = Some("Desktop media controls disconnected.".to_string());
            eprintln!("music MPRIS unavailable: session bus connection was lost");
        }
        self.begin_release();
    }

    pub(crate) fn begin_close(&mut self) {
        self.closing = true;
        if let Some(cancel) = self.connect_cancel.as_ref() {
            cancel.cancel();
        }
        if let Some(owner) = self.connect_owner.as_mut() {
            owner.shutdown();
        }
        self.consume_connect_result();
        self.begin_release();
    }

    pub(crate) fn poll_lifecycle(&mut self) {
        self.consume_connect_result();
        self.try_join_connect_owner();
        self.poll_release();
    }

    pub(crate) fn poll_close(&mut self) -> CloseReadiness {
        self.poll_lifecycle();

        if self.owner_failed || self.availability == Availability::Vetoed {
            return CloseReadiness::Vetoed;
        }
        if self.connect_owner.is_some()
            || self.connect_delivery.is_some()
            || self.shutdown.is_some()
        {
            return CloseReadiness::Pending;
        }
        match self.availability {
            Availability::Inert | Availability::Unavailable | Availability::Released => {
                CloseReadiness::Ready
            }
            Availability::Connecting | Availability::Live | Availability::Releasing => {
                CloseReadiness::Pending
            }
            Availability::Vetoed => CloseReadiness::Vetoed,
        }
    }

    pub(crate) fn needs_lifecycle_wake(&self) -> bool {
        self.closing && matches!(self.poll_readiness(), CloseReadiness::Pending)
            || self.shutdown.is_some()
            || self.connect_owner.is_some() && self.availability != Availability::Connecting
    }

    fn poll_readiness(&self) -> CloseReadiness {
        if self.owner_failed || self.availability == Availability::Vetoed {
            CloseReadiness::Vetoed
        } else if self.connect_owner.is_some()
            || self.connect_delivery.is_some()
            || self.shutdown.is_some()
            || matches!(
                self.availability,
                Availability::Connecting | Availability::Live | Availability::Releasing
            )
        {
            CloseReadiness::Pending
        } else {
            CloseReadiness::Ready
        }
    }

    fn consume_connect_result(&mut self) {
        let delivery = self.connect_delivery.clone();
        if let Some(delivery) = delivery
            && delivery
                .slot
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .is_some()
        {
            self.finish_connect(&delivery);
        }
    }

    fn try_join_connect_owner(&mut self) {
        if self.availability == Availability::Connecting && !self.closing {
            return;
        }
        let Some(owner) = self.connect_owner.as_mut() else {
            return;
        };
        let Some(outcome) = owner.try_join_shutdown() else {
            return;
        };
        match outcome {
            BlockingShutdown::Joined { panicked: 0, .. } => {}
            BlockingShutdown::Joined { panicked, .. } => {
                self.owner_failed = true;
                self.set_unavailable("Desktop media controls are unavailable.");
                eprintln!("music MPRIS unavailable: {panicked} connect worker panic(s)");
            }
            BlockingShutdown::TimedOut { .. } => {
                self.owner_failed = true;
                self.availability = Availability::Vetoed;
                self.notice =
                    Some("Desktop media controls could not shut down safely.".to_string());
            }
            _ => {
                self.owner_failed = true;
                self.availability = Availability::Vetoed;
                self.notice = Some("Desktop media controls lost connection custody.".to_string());
            }
        }
        self.connect_owner = None;
        self.consume_connect_result();
        if self.availability == Availability::Connecting && self.connect_delivery.is_some() {
            // After joining, a job that never started cannot leave a connection to release.
            if self.closing
                && self
                    .connect_delivery
                    .as_ref()
                    .is_some_and(|delivery| !delivery.started.load(Ordering::Acquire))
            {
                self.connect_delivery = None;
                self.connect_cancel = None;
                self.availability = Availability::Released;
            } else {
                self.owner_failed = true;
                self.availability = Availability::Vetoed;
                self.notice = Some("Desktop media controls lost connection custody.".to_string());
            }
        }
    }

    fn begin_release(&mut self) {
        if self.shutdown.is_some() || self.shutdown_outcome.is_some() {
            return;
        }
        let Some(Transport::Zbus(connection)) = self.transport.as_mut() else {
            return;
        };
        self.receiver = None;
        self.availability = Availability::Releasing;
        match connection.begin_shutdown() {
            Some(shutdown) => self.shutdown = Some(shutdown),
            None => match connection.shutdown_outcome() {
                Some(outcome) => self.finish_release(outcome),
                None => self.veto_release("MPRIS shutdown custody is unknown"),
            },
        }
    }

    fn poll_release(&mut self) {
        let Some(shutdown) = self.shutdown.as_mut() else {
            return;
        };
        let waker = Waker::noop();
        let mut context = Context::from_waker(waker);
        if let Poll::Ready(outcome) = Future::poll(Pin::new(shutdown), &mut context) {
            self.shutdown = None;
            self.finish_release(outcome);
        }
    }

    fn finish_release(&mut self, outcome: MprisShutdownOutcome) {
        self.shutdown_outcome = Some(outcome);
        if outcome.is_complete() {
            self.availability = Availability::Released;
        } else {
            let reason = outcome
                .incomplete_reason()
                .map_or("shutdown custody is unknown".to_string(), |reason| {
                    reason.to_string()
                });
            self.veto_release(&reason);
        }
    }

    fn veto_release(&mut self, reason: &str) {
        self.availability = Availability::Vetoed;
        self.notice = Some("Desktop media controls could not shut down safely.".to_string());
        eprintln!("music MPRIS close vetoed: {}", escaped_bounded(reason));
    }

    fn set_unavailable(&mut self, notice: &str) {
        self.availability = Availability::Unavailable;
        self.notice = Some(notice.to_string());
    }
}

impl Drop for Runtime {
    fn drop(&mut self) {
        self.begin_close();
    }
}

fn escaped_bounded(value: &str) -> String {
    value
        .chars()
        .flat_map(char::escape_default)
        .take(512)
        .collect()
}
